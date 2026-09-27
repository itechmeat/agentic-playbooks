//! Decisions outside a run (issue #165 Part 16): the MCP catalog ranking
//! asks through the same configuration, provider chain and redaction as a
//! run, but has no run journal. Each decision is logged as one JSON line in
//! `<root>/.apb/decisions.jsonl` (git-ignored, `workspace::LOCAL_ENTRIES`)
//! with the `decision_made` fields minus node and attempt, and a use's
//! `max_requests_per_day` caps the requests per project and UTC day.
//!
//! Every step fails open: the caller gets an outcome and keeps today's
//! behaviour on anything but `Answered`. Nothing here is ever applied by the
//! engine; the one caller (catalog ranking) is advisory by definition.

use std::collections::BTreeMap;
use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use apb_core::decisions::{
    CATALOG_RANK_MAX_REQUESTS_PER_DAY, DecisionMode, EffectiveDecisions, SendClass,
};
use apb_decide::{Answer, DecisionRequest, ProviderChain, Question, UseSite};
use serde_json::{Value, json};

use super::{compact_answer, list_price_per_million, providers, redact, redact_value};

/// The log file under `<root>/.apb/`.
pub const DECISIONS_LOG: &str = "decisions.jsonl";

const DAY_MS: u64 = 86_400_000;

/// An answered standalone decision.
#[derive(Debug, Clone, PartialEq)]
pub struct StandaloneAnswer {
    pub provider: String,
    pub model: String,
    pub calibrated: bool,
    /// The effective mode the decision was asked in.
    pub mode: DecisionMode,
    pub answers: BTreeMap<String, Answer>,
    pub latency_ms: u64,
}

/// What a standalone decision came to. The caller keeps today's behaviour
/// on anything but `Answered`.
#[derive(Debug, Clone, PartialEq)]
pub enum StandaloneOutcome {
    Answered(StandaloneAnswer),
    /// Nothing asked: the use is off, prompts may not be sent, the state is
    /// over the byte budget, or the daily cap is reached.
    Skipped {
        reason: &'static str,
    },
    /// Asked and failed; logged with this error kind.
    Failed {
        error_kind: String,
    },
}

/// Asks decisions for one project outside any run.
#[derive(Debug)]
pub struct StandaloneDecider {
    settings: EffectiveDecisions,
    root: PathBuf,
    chain: OnceLock<(ProviderChain, Vec<String>)>,
    redactor: OnceLock<redact::Redactor>,
}

impl StandaloneDecider {
    /// The decider for the project at `root`, or `None` when the layer is
    /// off for it (no file, an invalid file, the kill switch, the project
    /// opted out, every use off).
    pub fn for_project(root: &Path) -> Option<Self> {
        apb_core::decisions::resolve(root)
            .active()
            .map(|s| Self::with_settings(s, root))
    }

    /// A decider over explicit settings (already resolved and capped).
    pub fn with_settings(settings: EffectiveDecisions, root: &Path) -> Self {
        StandaloneDecider {
            settings,
            root: root.to_path_buf(),
            chain: OnceLock::new(),
            redactor: OnceLock::new(),
        }
    }

    /// The use's mode now: the settings, unless the kill switch is set.
    pub fn mode_for(&self, site: UseSite) -> DecisionMode {
        if apb_core::decisions::killed_by_switch() {
            DecisionMode::Off
        } else {
            self.settings.mode_for(site.as_str())
        }
    }

    /// A use's threshold by name.
    pub fn threshold(&self, site: UseSite, name: &str) -> Option<f64> {
        self.settings.threshold(site.as_str(), name)
    }

    /// Whether any configured provider resolved (its key and account id):
    /// without one nothing can be asked, and a caller keeps today's
    /// behaviour without saying so.
    pub fn available(&self) -> bool {
        !self.chain().0.is_empty()
    }

    /// The byte budget of one request's state.
    pub fn max_state_bytes(&self) -> usize {
        self.settings.privacy.max_state_bytes
    }

    fn chain(&self) -> &(ProviderChain, Vec<String>) {
        self.chain
            .get_or_init(|| providers::build_chain(&self.settings))
    }

    fn redactor(&self) -> &redact::Redactor {
        self.redactor
            .get_or_init(|| redact::Redactor::new(self.chain().1.clone(), &self.root))
    }

    fn log_path(&self) -> PathBuf {
        self.root.join(".apb").join(DECISIONS_LOG)
    }

    fn daily_cap(&self, site: UseSite) -> u32 {
        self.settings
            .uses
            .get(site.as_str())
            .and_then(|u| u.max_requests_per_day)
            .unwrap_or(CATALOG_RANK_MAX_REQUESTS_PER_DAY)
    }

    /// Requests sent today (UTC) for `site`, per the log.
    fn requests_today(&self, site: UseSite, now_ms: u64) -> u32 {
        let Ok(file) = std::fs::File::open(self.log_path()) else {
            return 0;
        };
        let today = now_ms / DAY_MS;
        std::io::BufReader::new(file)
            .lines()
            .map_while(Result::ok)
            .filter_map(|l| serde_json::from_str::<Value>(&l).ok())
            .filter(|v| {
                v["use_site"] == site.as_str()
                    && v["ts_ms"].as_u64().is_some_and(|t| t / DAY_MS == today)
                    && !v["provider"].is_null()
                    && v["cached"] != true
            })
            .count() as u32
    }

    /// Best effort: a log that cannot be written never changes the answer.
    fn log(&self, line: &Value) {
        let path = self.log_path();
        if !path.parent().is_some_and(Path::is_dir) {
            return;
        }
        let mut options = std::fs::OpenOptions::new();
        options.create(true).append(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        if let (Ok(mut f), Ok(mut text)) = (options.open(&path), serde_json::to_string(line)) {
            text.push('\n');
            let _ = f.write_all(text.as_bytes());
        }
    }

    /// Asks one decision over `state` (material class `prompt`): redacted
    /// when `privacy.redact` is on, refused when over `max_state_bytes`
    /// (the caller clips), logged, capped per day.
    pub fn decide(
        &self,
        site: UseSite,
        state: Value,
        state_order: Vec<String>,
        questions: BTreeMap<String, Question>,
    ) -> StandaloneOutcome {
        let mode = self.mode_for(site);
        if mode == DecisionMode::Off {
            return StandaloneOutcome::Skipped { reason: "off" };
        }
        if !self.settings.sends(SendClass::Prompts) {
            return StandaloneOutcome::Skipped { reason: "send" };
        }
        let state = if self.settings.privacy.redact {
            redact_value(self.redactor(), state)
        } else {
            state
        };
        let state_bytes = serde_json::to_string(&state).map_or(0, |s| s.len());
        if state_bytes > self.settings.privacy.max_state_bytes {
            return StandaloneOutcome::Skipped {
                reason: "state_too_large",
            };
        }
        let now_ms = apb_core::clock::now_ms_u64();
        let questions_digest = apb_decide::digest::questions_digest(&questions);
        let state_digest = apb_decide::digest::digest(&state);
        let mut line = json!({
            "ts_ms": now_ms,
            "use_site": site.as_str(),
            "provider": null,
            "model": null,
            "calibrated": false,
            "mode": mode.as_str(),
            "questions_digest": questions_digest,
            "state_digest": state_digest,
            "state_bytes": state_bytes,
            "answers": {},
            "applied": false,
            "latency_ms": 0,
            "input_tokens": null,
            "cost_usd": null,
            "cost_estimated": false,
            "cached": false,
            "error": null,
        });
        if self.requests_today(site, now_ms) >= self.daily_cap(site) {
            line["error"] = json!("budget");
            self.log(&line);
            return StandaloneOutcome::Skipped { reason: "budget" };
        }
        let request = DecisionRequest {
            use_site: site,
            state,
            state_order,
            questions,
        };
        let started = std::time::Instant::now();
        let result = self.chain().0.decide(&request, None);
        match result {
            Ok(resp) => {
                let compact: BTreeMap<String, _> = resp
                    .answers
                    .iter()
                    .map(|(k, a)| (k.clone(), compact_answer(a)))
                    .collect();
                let (cost, estimated) = match resp.usage.cost_usd {
                    Some(c) => (Some(c), false),
                    None => match (list_price_per_million(&resp.model), resp.usage.input_tokens) {
                        (Some(price), Some(t)) => (Some(price * t as f64 / 1_000_000.0), true),
                        _ => (None, false),
                    },
                };
                line["provider"] = json!(resp.provider);
                line["model"] = json!(resp.model);
                line["calibrated"] = json!(resp.calibrated);
                line["answers"] = json!(compact);
                line["latency_ms"] = json!(resp.latency_ms.max(1));
                line["input_tokens"] = json!(resp.usage.input_tokens);
                line["cost_usd"] = json!(cost);
                line["cost_estimated"] = json!(estimated);
                self.log(&line);
                StandaloneOutcome::Answered(StandaloneAnswer {
                    provider: resp.provider,
                    model: resp.model,
                    calibrated: resp.calibrated,
                    mode,
                    answers: resp.answers,
                    latency_ms: resp.latency_ms,
                })
            }
            Err(e) => {
                line["provider"] = json!(self.chain().0.ids().last());
                line["latency_ms"] = json!(started.elapsed().as_millis() as u64);
                line["error"] = json!(e.kind());
                self.log(&line);
                StandaloneOutcome::Failed {
                    error_kind: e.kind().to_string(),
                }
            }
        }
    }
}
