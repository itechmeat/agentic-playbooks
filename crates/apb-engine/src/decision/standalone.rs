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

/// Asks decisions for one project outside any run. `Debug` never prints a
/// key or a secret value.
pub struct StandaloneDecider {
    settings: EffectiveDecisions,
    root: PathBuf,
    chain: OnceLock<(ProviderChain, Vec<String>)>,
    redactor: OnceLock<redact::Redactor>,
}

impl std::fmt::Debug for StandaloneDecider {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StandaloneDecider")
            .field("root", &self.root)
            .field("providers", &self.chain.get().map(|c| &c.0))
            .finish_non_exhaustive()
    }
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

    /// The same secrets a run of this project redacts: the provider keys
    /// and every variable an installed connector references.
    fn redactor(&self) -> &redact::Redactor {
        self.redactor.get_or_init(|| {
            let mut secrets: Vec<String> =
                apb_core::connector::resolve::all_referenced_env_names(&self.root)
                    .iter()
                    .filter_map(|n| apb_core::connector::secrets::resolve_var(&self.root, n))
                    .collect();
            secrets.extend(self.chain().1.iter().cloned());
            redact::Redactor::new(secrets, &self.root)
        })
    }

    /// `text` as a request would carry it: redacted when `privacy.redact` is
    /// on. A caller that clips redacts first, so a cut never splits a
    /// secret into a shape the redactor no longer knows.
    pub fn redact(&self, text: &str) -> String {
        if self.settings.privacy.redact {
            self.redactor().redact(text)
        } else {
            text.to_string()
        }
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
        let (state, questions) = if self.settings.privacy.redact {
            // The questions carry text too (a suggestion's synopsis).
            let questions = serde_json::to_value(&questions)
                .ok()
                .map(|v| redact_value(self.redactor(), v))
                .and_then(|v| serde_json::from_value(v).ok())
                .unwrap_or(questions);
            (redact_value(self.redactor(), state), questions)
        } else {
            (state, questions)
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
        // The count, the request and its log line under the log's lock: MCP
        // servers of several agents share the log, and a count read before
        // another's reply is logged would let each of them past the cap. A
        // lock still held after its wait (a slow request elsewhere) skips
        // this one: fail-open, the caller keeps its plain answer.
        let Some(_lock) = self
            .log_path()
            .parent()
            .and_then(|dir| apb_core::fsutil::lock_dir(dir, "decisions.jsonl.lock").ok())
        else {
            return StandaloneOutcome::Skipped { reason: "busy" };
        };
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

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use apb_decide::{DecideError, DecisionProvider, DecisionResponse, Limits, Usage};

    use super::*;

    /// A provider that keeps every request and takes `delay` to answer.
    #[derive(Debug, Default)]
    struct Slow {
        seen: Mutex<Vec<DecisionRequest>>,
        delay_ms: u64,
    }

    #[derive(Debug)]
    struct Handle(Arc<Slow>);

    impl DecisionProvider for Handle {
        fn id(&self) -> &str {
            "slow"
        }
        fn model(&self) -> &str {
            "m"
        }
        fn limits(&self) -> Limits {
            Limits::default()
        }
        fn decide(&self, req: &DecisionRequest) -> Result<DecisionResponse, DecideError> {
            self.0.seen.lock().unwrap().push(req.clone());
            std::thread::sleep(std::time::Duration::from_millis(self.0.delay_ms));
            Ok(DecisionResponse {
                provider: "slow".into(),
                model: "m".into(),
                calibrated: false,
                answers: BTreeMap::new(),
                usage: Usage::default(),
                latency_ms: 1,
                cached: false,
                ignored_items: 0,
            })
        }
    }

    fn decider(root: &Path, cap: u32, slow: &Arc<Slow>) -> StandaloneDecider {
        let cfg = tempfile::tempdir().unwrap();
        std::fs::write(
            cfg.path().join(apb_core::decisions::DECISIONS_FILE),
            format!("mode: advise\nproviders: [{{ id: slow, kind: systemone, base_url: 'http://127.0.0.1:1', model: m }}]\nuses:\n  catalog_rank: {{ mode: advise, max_requests_per_day: {cap} }}\n"),
        )
        .unwrap();
        let settings = apb_core::decisions::load_file(cfg.path()).unwrap().unwrap();
        let d = StandaloneDecider::with_settings(settings, root);
        let _ = d.chain.set((
            ProviderChain::new(vec![Box::new(Handle(slow.clone()))]),
            vec!["sk-live-provider-key-123456".into()],
        ));
        d
    }

    fn questions(text: &str) -> BTreeMap<String, Question> {
        BTreeMap::from([(
            "covered_0".to_string(),
            Question::Noul {
                instructions: json!({ "question": "same?", "synopsis": text }),
                criteria: None,
            },
        )])
    }

    fn project() -> tempfile::TempDir {
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(root.path().join(".apb")).unwrap();
        root
    }

    #[test]
    fn question_text_is_redacted_and_debug_hides_keys() {
        let root = project();
        let slow = Arc::new(Slow::default());
        let d = decider(root.path(), 10, &slow);
        let _ = d.decide(
            UseSite::CatalogRank,
            json!({"task": "x"}),
            vec!["task".into()],
            questions("deploy /home/alice/app and mail bob@corp.example"),
        );
        let sent = serde_json::to_string(&slow.seen.lock().unwrap()[0].questions).unwrap();
        assert!(
            !sent.contains("alice") && !sent.contains("bob@corp"),
            "{sent}"
        );
        assert!(!format!("{d:?}").contains("sk-live"), "{d:?}");
    }

    #[test]
    fn concurrent_calls_never_pass_the_daily_cap() {
        let root = project();
        let slow = Arc::new(Slow {
            delay_ms: 300,
            ..Default::default()
        });
        let (a, b) = (
            decider(root.path(), 1, &slow),
            decider(root.path(), 1, &slow),
        );
        let ask = |d: &StandaloneDecider| {
            d.decide(
                UseSite::CatalogRank,
                json!({"task": "x"}),
                vec!["task".into()],
                questions("s"),
            )
        };
        let outcomes = std::thread::scope(|s| {
            let x = s.spawn(|| ask(&a));
            let y = s.spawn(|| ask(&b));
            [x.join().unwrap(), y.join().unwrap()]
        });
        assert_eq!(slow.seen.lock().unwrap().len(), 1, "{outcomes:?}");
        // A third call the same day is refused too: the reservation counts.
        assert!(matches!(
            ask(&a),
            StandaloneOutcome::Skipped { reason: "budget" }
        ));
    }
}
