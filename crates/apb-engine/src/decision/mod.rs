//! The decision runner (issue #165 Part 3): the one entry point through
//! which a run asks a decision model anything.
//!
//! A run gets a runner only when its manifest carries a `decisions` block
//! (`decisions.yaml` present and a use above off at start). Every call goes
//! through the same steps, and none of them can fail a node or the run: the
//! caller gets an outcome and applies its own fallback.
//!
//! 1. The use's mode (the snapshot, capped by the ceiling; the kill switch
//!    re-checked now). Off: nothing happens, nothing is journaled.
//! 2. Fields of a material class `privacy.send` does not allow are emptied.
//! 3. Redaction ([`redact`]), then each field's own clip, then the share of
//!    `privacy.max_state_bytes` it may take, head and tail kept.
//! 4. sha256 of the state and of the questions.
//! 5. A decision journaled earlier in this run for the same use, node,
//!    attempt, state and questions is replayed: no request.
//! 6. A spent budget journals `error: budget` and skips.
//! 7. The provider chain, through the run's cache, within `timeout_ms`.
//! 8. `DecisionMade` is appended to the journal; the answer is returned only
//!    after the append succeeded.
//! 9. With `privacy.debug_state`, the redacted state and the full answers go
//!    to `runs/<id>/decisions/<seq>.json`.

pub(crate) mod completion;
mod redact;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

use apb_core::decisions::{
    DecisionMode, EffectiveDecisions, KeyRef, ProviderKind, ProviderSpec, SendClass,
};
use apb_decide::{
    Answer, ApiKey, DecisionCache, DecisionProvider, DecisionRequest, FakeProvider, ProviderChain,
    Question, SystemOne, UseSite,
};
use serde_json::{Value, json};

use crate::error::EngineError;
use crate::event::{DecisionAnswer, DecisionBaseline, Event, EventPayload};

/// The settings a new run snapshots into its manifest, or `None` when the
/// layer is off for it (no file, the kill switch, the project opted out,
/// every use off). A file that does not load is reported once on stderr and
/// leaves the layer off.
pub(crate) fn snapshot(root: &Path) -> Option<EffectiveDecisions> {
    use apb_core::decisions::Resolution;
    match apb_core::decisions::resolve(root) {
        Resolution::Active(eff) => Some(eff),
        Resolution::Invalid(e) => {
            eprintln!(
                "apb: {} ignored, decision models stay off: {e}",
                apb_core::decisions::DECISIONS_FILE
            );
            None
        }
        _ => None,
    }
}

/// Where a journaled decision goes: the attempt journal, written in place
/// while the node runs (not the post-node batch).
pub(crate) trait DecisionJournal {
    /// Appends and returns the event's seq.
    fn append_decision(&self, payload: EventPayload) -> Result<u64, EngineError>;
}

/// Which material class a state field belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FieldClass {
    Prompt,
    Output,
}

impl FieldClass {
    fn send_class(self) -> SendClass {
        match self {
            FieldClass::Prompt => SendClass::Prompts,
            FieldClass::Output => SendClass::Outputs,
        }
    }
}

/// One text field of a state, with the use's own clip: its first `head` and
/// last `tail` bytes are kept.
#[derive(Debug, Clone)]
pub(crate) struct StateField {
    pub(crate) name: &'static str,
    pub(crate) class: FieldClass,
    pub(crate) text: String,
    pub(crate) head: usize,
    pub(crate) tail: usize,
}

/// A state: text fields plus code-computed `meta` values (always sent).
#[derive(Debug, Clone, Default)]
pub(crate) struct StateParts {
    pub(crate) fields: Vec<StateField>,
    pub(crate) meta: serde_json::Map<String, Value>,
}

/// What a use concludes from the answers, before they are journaled.
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct Judgement {
    pub(crate) applied: bool,
    pub(crate) would_change: Option<bool>,
}

/// One question to ask.
pub(crate) struct DecisionCall<'a> {
    pub(crate) site: UseSite,
    pub(crate) node: Option<&'a str>,
    pub(crate) attempt: Option<u32>,
    pub(crate) state: StateParts,
    pub(crate) questions: BTreeMap<String, Question>,
    /// A code-only verdict recorded alongside.
    pub(crate) baseline: Option<DecisionBaseline>,
    /// Decides `applied` and `would_change` from the answers.
    pub(crate) judge: &'a dyn Fn(&BTreeMap<String, DecisionAnswer>) -> Judgement,
}

/// The result of one call. The caller applies its fallback on anything but
/// `Answered`.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum DecisionOutcome {
    Answered {
        answers: BTreeMap<String, DecisionAnswer>,
        mode: DecisionMode,
        replayed: bool,
    },
    /// Nothing asked: the use is off, or the budget is spent.
    Skipped { reason: &'static str },
    /// Asked and failed; journaled with this error kind.
    Failed { error_kind: String },
}

/// A journaled answer that a later identical ask replays.
#[derive(Debug, Clone)]
struct Replay {
    use_site: String,
    node: Option<String>,
    attempt: Option<u32>,
    state_digest: String,
    questions_digest: String,
    answers: BTreeMap<String, DecisionAnswer>,
}

#[derive(Debug, Default)]
struct Ledger {
    requests: u32,
    cost_usd: f64,
    replay: Vec<Replay>,
}

/// List price in USD per million input tokens for the models whose provider
/// reports no cost (output tokens are free). Used only when the reply
/// carried no cost of its own; the event then says `cost_estimated`.
fn list_price_per_million(model: &str) -> Option<f64> {
    match model {
        "jev-1.13.0" | "jev-1.13" | "typesafe/jev-1.13" => Some(0.042),
        _ => None,
    }
}

/// The run's decision runner. `Sync`: parallel branches share it.
#[derive(Debug)]
pub(crate) struct DecisionRunner {
    settings: EffectiveDecisions,
    root: PathBuf,
    run_dir: PathBuf,
    /// Variables whose values never leave the machine (connector secrets).
    scrub_names: Vec<String>,
    chain: OnceLock<(ProviderChain, Vec<String>)>,
    redactor: OnceLock<redact::Redactor>,
    cache: DecisionCache,
    ledger: Mutex<Ledger>,
}

impl DecisionRunner {
    /// The runner for a run, or `None` when its manifest has no decisions
    /// block. Seeds the budget and the replay list from the journal.
    pub(crate) fn for_run(
        root: &Path,
        run_dir: &Path,
        events: &[Event],
        scrub_names: &[String],
    ) -> Option<Self> {
        let settings = crate::manifest::read(run_dir).ok()??.decisions?;
        let mut ledger = Ledger::default();
        for e in events {
            if let EventPayload::DecisionMade {
                use_site,
                node,
                attempt,
                provider,
                questions_digest,
                state_digest,
                answers,
                cost_usd,
                cached,
                error,
                ..
            } = &e.payload
            {
                if provider.is_some() && !cached {
                    ledger.requests += 1;
                }
                ledger.cost_usd += cost_usd.unwrap_or(0.0);
                if error.is_none() {
                    ledger.replay.push(Replay {
                        use_site: use_site.clone(),
                        node: node.clone(),
                        attempt: *attempt,
                        state_digest: state_digest.clone(),
                        questions_digest: questions_digest.clone(),
                        answers: answers.clone(),
                    });
                }
            }
        }
        Some(DecisionRunner {
            settings,
            root: root.to_path_buf(),
            run_dir: run_dir.to_path_buf(),
            scrub_names: scrub_names.to_vec(),
            chain: OnceLock::new(),
            redactor: OnceLock::new(),
            cache: DecisionCache::new(),
            ledger: Mutex::new(ledger),
        })
    }

    pub(crate) fn settings(&self) -> &EffectiveDecisions {
        &self.settings
    }

    /// The use's mode now: the snapshot, unless the kill switch is set.
    pub(crate) fn mode_for(&self, site: UseSite) -> DecisionMode {
        if apb_core::decisions::killed_by_switch() {
            DecisionMode::Off
        } else {
            self.settings.mode_for(site.as_str())
        }
    }

    /// Builds the provider chain on first use: keys resolve now, and a
    /// provider whose key does not resolve is left out (said once on
    /// stderr, never with a value). Also returns the resolved keys, which
    /// the redactor treats as secrets.
    fn chain(&self) -> &(ProviderChain, Vec<String>) {
        self.chain.get_or_init(|| {
            let timeout = Duration::from_millis(self.settings.timeout_ms);
            let mut providers: Vec<Box<dyn DecisionProvider>> = Vec::new();
            let mut keys = Vec::new();
            for spec in &self.settings.providers {
                let key = match resolve_key(spec) {
                    Ok(k) => k,
                    Err(why) => {
                        eprintln!("apb: decision provider `{}` left out: {why}", spec.id);
                        continue;
                    }
                };
                if let Some(k) = &key {
                    keys.push(k.clone());
                }
                match spec.kind {
                    ProviderKind::Systemone => providers.push(Box::new(SystemOne::new(
                        spec.id.clone(),
                        spec.base_url.clone().unwrap_or_default(),
                        spec.model.clone().unwrap_or_default(),
                        key.map(ApiKey::new),
                        timeout,
                    ))),
                    ProviderKind::Fake => {
                        let mut fake = FakeProvider::new(spec.id.clone());
                        for (qid, item) in &spec.answers {
                            fake = fake.answer(qid.clone(), item.clone());
                        }
                        providers.push(Box::new(fake));
                    }
                }
            }
            (ProviderChain::new(providers), keys)
        })
    }

    fn redactor(&self) -> &redact::Redactor {
        self.redactor.get_or_init(|| {
            let mut secrets: Vec<String> = self
                .scrub_names
                .iter()
                .filter_map(|n| apb_core::connector::secrets::resolve_var(&self.root, n))
                .collect();
            secrets.extend(self.chain().1.iter().cloned());
            redact::Redactor::new(secrets, &self.root)
        })
    }

    /// Steps 2 and 3: the state object as sent.
    fn build_state(&self, parts: &StateParts) -> Value {
        let privacy = &self.settings.privacy;
        let meta = Value::Object(parts.meta.clone());
        let meta_len = serde_json::to_string(&meta).map_or(0, |s| s.len());
        let share = privacy
            .max_state_bytes
            .saturating_sub(meta_len + 64)
            .checked_div(parts.fields.len().max(1))
            .unwrap_or(0);
        let mut obj = serde_json::Map::new();
        for f in &parts.fields {
            let text = if !self.settings.sends(f.class.send_class()) {
                String::new()
            } else {
                let text = if privacy.redact {
                    self.redactor().redact(&f.text)
                } else {
                    f.text.clone()
                };
                let own = redact::clip(&text, f.head, f.tail);
                if own.len() <= share {
                    own
                } else {
                    let keep = f.head + f.tail;
                    let head = share * f.head / keep.max(1);
                    redact::clip(&text, head, share - head)
                }
            };
            obj.insert(f.name.to_string(), Value::String(text));
        }
        let meta = if privacy.redact {
            redact_value(self.redactor(), meta)
        } else {
            meta
        };
        obj.insert("meta".to_string(), meta);
        Value::Object(obj)
    }

    fn replayed(
        &self,
        site: UseSite,
        node: Option<&str>,
        attempt: Option<u32>,
        state_digest: &str,
        questions_digest: &str,
    ) -> Option<BTreeMap<String, DecisionAnswer>> {
        let ledger = self.ledger.lock().unwrap_or_else(|e| e.into_inner());
        ledger
            .replay
            .iter()
            .find(|r| {
                r.use_site == site.as_str()
                    && r.node.as_deref() == node
                    && r.attempt == attempt
                    && r.state_digest == state_digest
                    && r.questions_digest == questions_digest
            })
            .map(|r| r.answers.clone())
    }

    fn budget_spent(&self) -> bool {
        let ledger = self.ledger.lock().unwrap_or_else(|e| e.into_inner());
        ledger.requests >= self.settings.budget.max_requests_per_run
            || ledger.cost_usd >= self.settings.budget.max_usd_per_run
    }

    /// Asks one decision. See the module docs for the steps.
    pub(crate) fn decide(
        &self,
        journal: &dyn DecisionJournal,
        call: DecisionCall,
    ) -> DecisionOutcome {
        let mode = self.mode_for(call.site);
        if mode == DecisionMode::Off {
            return DecisionOutcome::Skipped { reason: "off" };
        }
        let state = self.build_state(&call.state);
        let state_bytes = serde_json::to_string(&state).map_or(0, |s| s.len()) as u64;
        let state_digest = apb_decide::digest::digest(&state);
        let questions_digest = apb_decide::digest::questions_digest(&call.questions);
        if let Some(answers) = self.replayed(
            call.site,
            call.node,
            call.attempt,
            &state_digest,
            &questions_digest,
        ) {
            return DecisionOutcome::Answered {
                answers,
                mode,
                replayed: true,
            };
        }
        let base = EventPayload::DecisionMade {
            use_site: call.site.as_str().to_string(),
            node: call.node.map(str::to_string),
            attempt: call.attempt,
            provider: None,
            model: None,
            calibrated: false,
            mode: mode.as_str().to_string(),
            questions_digest: questions_digest.clone(),
            state_digest: state_digest.clone(),
            state_bytes,
            answers: BTreeMap::new(),
            applied: false,
            would_change: None,
            baseline: call.baseline.clone(),
            latency_ms: 0,
            input_tokens: None,
            cost_usd: None,
            cost_estimated: false,
            cached: false,
            error: None,
        };
        if self.budget_spent() {
            let mut event = base;
            if let EventPayload::DecisionMade { error, .. } = &mut event {
                *error = Some("budget".into());
            }
            let _ = journal.append_decision(event);
            return DecisionOutcome::Skipped { reason: "budget" };
        }
        let request = DecisionRequest {
            use_site: call.site,
            state: state.clone(),
            state_order: call
                .state
                .fields
                .iter()
                .map(|f| f.name.to_string())
                .chain(["meta".to_string()])
                .collect(),
            questions: call.questions.clone(),
        };
        let started = std::time::Instant::now();
        let result = self.chain().0.decide(&request, Some(&self.cache));
        let elapsed = started.elapsed().as_millis() as u64;
        let mut event = base;
        let mut full: Option<BTreeMap<String, Answer>> = None;
        let outcome_answers;
        if let EventPayload::DecisionMade {
            provider,
            model,
            calibrated,
            answers,
            applied,
            would_change,
            latency_ms,
            input_tokens,
            cost_usd,
            cost_estimated,
            cached,
            error,
            ..
        } = &mut event
        {
            match &result {
                Ok(resp) => {
                    let compact: BTreeMap<String, DecisionAnswer> = resp
                        .answers
                        .iter()
                        .map(|(k, a)| (k.clone(), compact_answer(a)))
                        .collect();
                    let verdict = (call.judge)(&compact);
                    *provider = Some(resp.provider.clone());
                    *model = Some(resp.model.clone());
                    *calibrated = resp.calibrated;
                    *answers = compact.clone();
                    // Never applied in shadow, whatever the use concluded.
                    *applied = verdict.applied && mode > DecisionMode::Shadow;
                    *would_change = verdict.would_change;
                    *latency_ms = if resp.cached {
                        0
                    } else {
                        resp.latency_ms.max(1)
                    };
                    *input_tokens = resp.usage.input_tokens;
                    *cached = resp.cached;
                    if resp.cached {
                        *cost_usd = None;
                    } else if let Some(c) = resp.usage.cost_usd {
                        *cost_usd = Some(c);
                    } else if let (Some(price), Some(tokens)) =
                        (list_price_per_million(&resp.model), resp.usage.input_tokens)
                    {
                        *cost_usd = Some(price * tokens as f64 / 1_000_000.0);
                        *cost_estimated = true;
                    }
                    full = Some(resp.answers.clone());
                    outcome_answers = Some(compact);
                }
                Err(e) => {
                    *provider = self.chain().0.ids().last().map(|s| s.to_string());
                    *latency_ms = elapsed;
                    *error = Some(e.kind().to_string());
                    outcome_answers = None;
                }
            }
        } else {
            unreachable!("the event is a DecisionMade")
        }
        let (cost, counted) = match &event {
            EventPayload::DecisionMade {
                cost_usd,
                cached,
                provider,
                ..
            } => (cost_usd.unwrap_or(0.0), provider.is_some() && !cached),
            _ => (0.0, false),
        };
        let seq = match journal.append_decision(event) {
            Ok(seq) => seq,
            Err(_) => {
                return DecisionOutcome::Failed {
                    error_kind: "journal".into(),
                };
            }
        };
        {
            let mut ledger = self.ledger.lock().unwrap_or_else(|e| e.into_inner());
            if counted {
                ledger.requests += 1;
            }
            ledger.cost_usd += cost;
            if let Some(answers) = &outcome_answers {
                ledger.replay.push(Replay {
                    use_site: call.site.as_str().to_string(),
                    node: call.node.map(str::to_string),
                    attempt: call.attempt,
                    state_digest,
                    questions_digest,
                    answers: answers.clone(),
                });
            }
        }
        if self.settings.privacy.debug_state {
            self.write_debug_state(seq, &state, &call.questions, full.as_ref());
        }
        match (outcome_answers, result) {
            (Some(answers), _) => DecisionOutcome::Answered {
                answers,
                mode,
                replayed: false,
            },
            (None, Err(e)) => DecisionOutcome::Failed {
                error_kind: e.kind().to_string(),
            },
            (None, Ok(_)) => DecisionOutcome::Failed {
                error_kind: "unavailable".into(),
            },
        }
    }

    /// Step 9: the redacted state, the questions and the full answers, for
    /// debugging a use. Best effort: a failed write never affects the run.
    fn write_debug_state(
        &self,
        seq: u64,
        state: &Value,
        questions: &BTreeMap<String, Question>,
        answers: Option<&BTreeMap<String, Answer>>,
    ) {
        let dir = self.run_dir.join("decisions");
        let body = json!({"seq": seq, "state": state, "questions": questions, "answers": answers});
        if std::fs::create_dir_all(&dir).is_ok()
            && let Ok(text) = serde_json::to_string_pretty(&body)
        {
            let _ =
                apb_core::fsutil::atomic_write(&dir.join(format!("{seq}.json")), text.as_bytes());
        }
    }
}

fn resolve_key(spec: &ProviderSpec) -> Result<Option<String>, String> {
    match &spec.key {
        None => Ok(None),
        Some(KeyRef::Env(var)) => apb_core::decisions::resolve_key_var(var)
            .map(Some)
            .ok_or_else(|| format!("variable `{var}` is not set")),
        Some(KeyRef::Cmd(cmd)) => apb_core::connector::secrets::resolve_cmd(
            cmd,
            apb_core::connector::secrets::CMD_SECRET_TIMEOUT,
        )
        .map(|k| Some(k.trim().to_string()))
        .map_err(|_| "its key command failed".to_string()),
    }
}

fn redact_value(r: &redact::Redactor, v: Value) -> Value {
    match v {
        Value::String(s) => Value::String(r.redact(&s)),
        Value::Array(items) => {
            Value::Array(items.into_iter().map(|i| redact_value(r, i)).collect())
        }
        Value::Object(map) => Value::Object(
            map.into_iter()
                .map(|(k, v)| (k, redact_value(r, v)))
                .collect(),
        ),
        other => other,
    }
}

fn compact_answer(a: &Answer) -> DecisionAnswer {
    let round = |x: f64| (x * 1e6).round() / 1e6;
    match a {
        Answer::Noul { p } => DecisionAnswer {
            p: Some(round(*p)),
            ..Default::default()
        },
        Answer::Choice {
            value,
            probabilities,
            confidence,
        } => DecisionAnswer {
            value: Some(Value::String(value.clone())),
            p: probabilities.get(value).copied().map(round),
            confidence: Some(round(*confidence)),
            invalid: None,
        },
        Answer::Score {
            value,
            probabilities,
            confidence,
        } => {
            let top = probabilities
                .iter()
                .max_by(|a, b| a.1.total_cmp(b.1))
                .map(|(_, p)| *p);
            DecisionAnswer {
                value: Some(json!(round(*value))),
                p: top.map(round),
                confidence: Some(round(*confidence)),
                invalid: None,
            }
        }
        Answer::Invalid { reason } => DecisionAnswer {
            invalid: Some(reason.clone()),
            ..Default::default()
        },
    }
}

/// Totals over a run's journal (issue #165 Part 3), for reports and tests.
#[derive(Debug, Clone, Default, PartialEq, serde::Serialize)]
pub struct DecisionTotals {
    /// Decisions journaled (answered, failed or skipped for budget).
    pub decisions: u32,
    /// Requests actually sent (cache hits and budget skips excluded).
    pub requests: u32,
    pub cached: u32,
    pub errors: u32,
    pub cost_usd: f64,
    pub cost_estimated: bool,
    pub p50_latency_ms: Option<u64>,
    pub p95_latency_ms: Option<u64>,
    pub by_use: BTreeMap<String, UseTotals>,
}

#[derive(Debug, Clone, Default, PartialEq, serde::Serialize)]
pub struct UseTotals {
    pub decisions: u32,
    pub errors: u32,
    pub applied: u32,
    pub shadow_would_change: u32,
}

/// Nearest-rank percentile of sorted values.
fn percentile(sorted: &[u64], q: f64) -> Option<u64> {
    if sorted.is_empty() {
        return None;
    }
    let rank = ((q * sorted.len() as f64).ceil() as usize).clamp(1, sorted.len());
    Some(sorted[rank - 1])
}

/// Folds every `DecisionMade` of a journal into totals. Latency percentiles
/// cover the requests actually sent.
pub fn decision_totals(events: &[Event]) -> DecisionTotals {
    let mut t = DecisionTotals::default();
    let mut latencies = Vec::new();
    for e in events {
        let EventPayload::DecisionMade {
            use_site,
            provider,
            mode,
            applied,
            would_change,
            latency_ms,
            cost_usd,
            cost_estimated,
            cached,
            error,
            ..
        } = &e.payload
        else {
            continue;
        };
        t.decisions += 1;
        let u = t.by_use.entry(use_site.clone()).or_default();
        u.decisions += 1;
        if error.is_some() {
            t.errors += 1;
            u.errors += 1;
        }
        if *cached {
            t.cached += 1;
        } else if provider.is_some() {
            t.requests += 1;
            latencies.push(*latency_ms);
        }
        if *applied {
            u.applied += 1;
        }
        if mode == "shadow" && *would_change == Some(true) {
            u.shadow_would_change += 1;
        }
        t.cost_usd += cost_usd.unwrap_or(0.0);
        t.cost_estimated |= *cost_estimated;
    }
    latencies.sort_unstable();
    t.p50_latency_ms = percentile(&latencies, 0.5);
    t.p95_latency_ms = percentile(&latencies, 0.95);
    t
}
