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
pub(crate) mod judge;
mod providers;
mod redact;
pub mod report;
pub mod standalone;
// Engine uses (issue #165 Parts 9-12 and 14), one module each.
pub(crate) mod retry_advice;
pub(crate) mod review_triage;
pub(crate) mod routing;
pub(crate) mod supervisor_triage;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

use apb_core::decisions::{DecisionMode, EffectiveDecisions, SendClass};
use apb_decide::{Answer, DecisionCache, DecisionRequest, Question, UseSite};
use serde_json::{Value, json};

use crate::error::EngineError;
use crate::event::{DecisionAnswer, DecisionBaseline, Event, EventPayload};
use providers::Chains;

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

/// The enforce path of a use (issue #165 Part 14). The runner decides
/// whether the use may act: the use in `enforce` for the run, the playbook's
/// opt-in, a calibrated answer (or `allow_uncalibrated`), a stored threshold
/// for the answering provider and model, and the per-run action cap. Only
/// then does it ask `acts` with that threshold; `applied: true` is journaled
/// before the caller acts. Anything short of that journals `enforce_refused`
/// and the caller treats the answer as advise.
pub(crate) struct Enforce<'a> {
    /// The playbook (or node) opted in to the enforce path.
    pub(crate) opted_in: bool,
    /// A refusal the use knows before asking (e.g. `effects` at a gate).
    pub(crate) refused: Option<&'static str>,
    /// Whether the answers call for the action at the stored threshold.
    pub(crate) acts: &'a dyn Fn(&BTreeMap<String, DecisionAnswer>, f64) -> bool,
}

/// Join keys computed from the answers.
pub(crate) type JoinFrom<'a> =
    dyn Fn(&BTreeMap<String, DecisionAnswer>) -> BTreeMap<String, Value> + 'a;

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
    /// Join keys journaled with the decision for the report's labellers.
    pub(crate) join: BTreeMap<String, Value>,
    /// Join keys that depend on the answers (added when it arrives).
    pub(crate) join_from: Option<&'a JoinFrom<'a>>,
    /// The use's enforce path, if it has one.
    pub(crate) enforce: Option<Enforce<'a>>,
}

/// What a caller needs besides the answers: who answered and whether the
/// enforce path acted (both read back from the journal on a replay).
#[derive(Debug, Clone, PartialEq, Default)]
pub(crate) struct AnswerMeta {
    pub(crate) provider: Option<String>,
    pub(crate) model: Option<String>,
    pub(crate) calibrated: bool,
    /// The enforce path acted: the caller must apply the action.
    pub(crate) applied: bool,
    pub(crate) enforce_refused: Option<String>,
    /// The `decision_made` seq (`None` on a replay).
    pub(crate) seq: Option<u64>,
}

/// The result of one call. The caller applies its fallback on anything but
/// `Answered`.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum DecisionOutcome {
    Answered {
        answers: BTreeMap<String, DecisionAnswer>,
        mode: DecisionMode,
        replayed: bool,
        meta: AnswerMeta,
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
    meta: AnswerMeta,
}

#[derive(Debug, Default)]
struct Ledger {
    requests: u32,
    cost_usd: f64,
    replay: Vec<Replay>,
    /// Automatic actions taken per use this run (the enforce cap).
    actions: BTreeMap<String, u32>,
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
    chain: OnceLock<Chains>,
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
        Some(Self::with_settings(
            settings,
            root,
            run_dir,
            events,
            scrub_names,
        ))
    }

    /// A runner over `settings`, its budget and replay list seeded from the
    /// journal.
    fn with_settings(
        settings: EffectiveDecisions,
        root: &Path,
        run_dir: &Path,
        events: &[Event],
        scrub_names: &[String],
    ) -> Self {
        let mut ledger = Ledger::default();
        for e in events {
            if let EventPayload::DecisionMade {
                use_site,
                node,
                attempt,
                provider,
                model,
                calibrated,
                questions_digest,
                state_digest,
                answers,
                cost_usd,
                cached,
                error,
                applied,
                enforce_refused,
                ..
            } = &e.payload
            {
                if provider.is_some() && !cached {
                    ledger.requests += 1;
                }
                ledger.cost_usd += cost_usd.unwrap_or(0.0);
                if *applied {
                    *ledger.actions.entry(use_site.clone()).or_default() += 1;
                }
                if error.is_none() {
                    ledger.replay.push(Replay {
                        use_site: use_site.clone(),
                        node: node.clone(),
                        attempt: *attempt,
                        state_digest: state_digest.clone(),
                        questions_digest: questions_digest.clone(),
                        answers: answers.clone(),
                        meta: AnswerMeta {
                            provider: provider.clone(),
                            model: model.clone(),
                            calibrated: *calibrated,
                            applied: *applied,
                            enforce_refused: enforce_refused.clone(),
                            seq: None,
                        },
                    });
                }
            }
        }
        DecisionRunner {
            settings,
            root: root.to_path_buf(),
            run_dir: run_dir.to_path_buf(),
            scrub_names: scrub_names.to_vec(),
            chain: OnceLock::new(),
            redactor: OnceLock::new(),
            cache: DecisionCache::new(),
            ledger: Mutex::new(ledger),
        }
    }

    pub(crate) fn settings(&self) -> &EffectiveDecisions {
        &self.settings
    }

    /// The use's mode now: the snapshot, capped by what the machine and the
    /// project say at this moment (the kill switch, a lowered ceiling or use
    /// mode, a removed file), so every path stops mid-run.
    pub(crate) fn mode_for(&self, site: UseSite) -> DecisionMode {
        if apb_core::decisions::killed_by_switch() {
            return DecisionMode::Off;
        }
        let snapshot = self.settings.mode_for(site.as_str());
        if snapshot == DecisionMode::Off {
            return snapshot;
        }
        snapshot.min(apb_core::decisions::live_mode(&self.root, site.as_str()))
    }

    /// A use's threshold from the run's settings, or `default`.
    pub(crate) fn threshold_or(&self, site: UseSite, name: &str, default: f64) -> f64 {
        self.settings
            .threshold(site.as_str(), name)
            .unwrap_or(default)
    }

    /// Builds the provider chains on first use: keys resolve now, and a
    /// provider whose key does not resolve is left out (said once on
    /// stderr, never with a value). Also returns the resolved keys, which
    /// the redactor treats as secrets.
    fn chains(&self) -> &Chains {
        self.chain
            .get_or_init(|| providers::build_chains(&self.settings))
    }

    fn redactor(&self) -> &redact::Redactor {
        self.redactor.get_or_init(|| {
            let mut secrets: Vec<String> = self
                .scrub_names
                .iter()
                .filter_map(|n| apb_core::connector::secrets::resolve_var(&self.root, n))
                .collect();
            secrets.extend(self.chains().keys.iter().cloned());
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
        answered_by: Option<&[String]>,
    ) -> Option<(BTreeMap<String, DecisionAnswer>, AnswerMeta)> {
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
                    && answered_by
                        .is_none_or(|ids| r.meta.provider.as_ref().is_some_and(|p| ids.contains(p)))
            })
            .map(|r| (r.answers.clone(), r.meta.clone()))
    }

    /// The enforce gate (Part 14 common rules), after an answer arrived.
    /// Returns `(applied, enforce_refused)`.
    #[allow(clippy::too_many_arguments)]
    fn enforce_gate(
        &self,
        site: UseSite,
        mode: DecisionMode,
        enforce: Option<&Enforce>,
        answers: &BTreeMap<String, DecisionAnswer>,
        provider: &str,
        model: &str,
        calibrated: bool,
    ) -> (bool, Option<&'static str>) {
        let Some(en) = enforce else {
            return (false, None);
        };
        if mode != DecisionMode::Enforce {
            return (false, None);
        }
        if !en.opted_in {
            return (false, Some("not_opted_in"));
        }
        if let Some(r) = en.refused {
            return (false, Some(r));
        }
        if !calibrated && !self.settings.allows_uncalibrated(site.as_str()) {
            return (false, Some("uncalibrated"));
        }
        let Some(threshold) =
            apb_core::decision_thresholds::stored_threshold(site.as_str(), provider, model)
        else {
            return (false, Some("no_threshold"));
        };
        if !(en.acts)(answers, threshold) {
            return (false, None);
        }
        let taken = self
            .ledger
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .actions
            .get(site.as_str())
            .copied()
            .unwrap_or(0);
        if taken >= self.settings.max_actions(site.as_str()) {
            return (false, Some("cap"));
        }
        (true, None)
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
        self.decide_routed(journal, call, Route::All).0
    }

    /// [`Self::decide`] through a chosen route, also naming who answered.
    /// A custom route (a backend outside `decisions.yaml`, the judge's
    /// profile emulation) is asked whatever the use's mode, and what it
    /// concludes is applied whatever the mode: the node declared it.
    pub(crate) fn decide_routed(
        &self,
        journal: &dyn DecisionJournal,
        call: DecisionCall,
        route: Route,
    ) -> (DecisionOutcome, Option<AnswerSource>) {
        let mode = self.mode_for(call.site);
        let custom = matches!(route, Route::Custom { .. });
        // The emulation routes are a judge node's declared fallback: what
        // they conclude is applied whatever the mode.
        let declared = custom || matches!(route, Route::Emulation);
        if mode == DecisionMode::Off && !custom {
            return (DecisionOutcome::Skipped { reason: "off" }, None);
        }
        let state = self.build_state(&call.state);
        let state_bytes = serde_json::to_string(&state).map_or(0, |s| s.len()) as u64;
        let state_digest = apb_decide::digest::digest(&state);
        let questions_digest = apb_decide::digest::questions_digest(&call.questions);
        let output_chars = output_chars(&call.state);
        let route_ids: Option<Vec<String>> = match &route {
            Route::All => None,
            Route::Native => Some(
                self.chains()
                    .native
                    .ids()
                    .iter()
                    .map(|s| s.to_string())
                    .collect(),
            ),
            Route::Emulation => Some(
                self.chains()
                    .emulation
                    .ids()
                    .iter()
                    .map(|s| s.to_string())
                    .collect(),
            ),
            Route::Custom { id, .. } => Some(vec![id.to_string()]),
        };
        if let Some((answers, meta)) = self.replayed(
            call.site,
            call.node,
            call.attempt,
            &state_digest,
            &questions_digest,
            route_ids.as_deref(),
        ) {
            let source = meta.source();
            return (
                DecisionOutcome::Answered {
                    answers,
                    mode,
                    replayed: true,
                    meta,
                },
                source,
            );
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
            output_chars,
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
            enforce_refused: None,
            join: call.join.clone(),
        };
        if self.budget_spent() {
            let mut event = base;
            if let EventPayload::DecisionMade { error, .. } = &mut event {
                *error = Some("budget".into());
            }
            let _ = journal.append_decision(event);
            return (DecisionOutcome::Skipped { reason: "budget" }, None);
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
        let result = match &route {
            Route::All => self.chains().all.decide(&request, Some(&self.cache)),
            Route::Native => self.chains().native.decide(&request, Some(&self.cache)),
            Route::Emulation => self.chains().emulation.decide(&request, Some(&self.cache)),
            Route::Custom { id, model, ask } => match self.cache.get(id, model, &request) {
                Some(hit) => Ok(hit),
                None => ask(&request).inspect(|r| self.cache.put(id, model, &request, r)),
            },
        };
        let elapsed = started.elapsed().as_millis() as u64;
        let mut event = base;
        let mut full: Option<BTreeMap<String, Answer>> = None;
        let outcome_answers;
        let mut meta = AnswerMeta::default();
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
            enforce_refused,
            join,
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
                    let (enforced, refused) = self.enforce_gate(
                        call.site,
                        mode,
                        call.enforce.as_ref(),
                        &compact,
                        &resp.provider,
                        &resp.model,
                        resp.calibrated,
                    );
                    *provider = Some(resp.provider.clone());
                    *model = Some(resp.model.clone());
                    *calibrated = resp.calibrated;
                    *answers = compact.clone();
                    if let Some(extra) = call.join_from {
                        join.extend(extra(&compact));
                    }
                    // Never applied in shadow, whatever the use concluded
                    // (a custom or emulation route is the node's own
                    // declared fallback), unless the enforce gate acted.
                    *applied =
                        (verdict.applied && (declared || mode > DecisionMode::Shadow)) || enforced;
                    *enforce_refused = refused.map(str::to_string);
                    meta = AnswerMeta {
                        provider: provider.clone(),
                        model: model.clone(),
                        calibrated: resp.calibrated,
                        applied: *applied,
                        enforce_refused: enforce_refused.clone(),
                        seq: None,
                    };
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
                    *provider = match &route {
                        Route::All => self.chains().all.ids().last().map(|s| s.to_string()),
                        Route::Native => self.chains().native.ids().last().map(|s| s.to_string()),
                        Route::Emulation => {
                            self.chains().emulation.ids().last().map(|s| s.to_string())
                        }
                        Route::Custom { id, .. } => Some(id.to_string()),
                    };
                    *latency_ms = elapsed;
                    *error = Some(e.kind().to_string());
                    outcome_answers = None;
                }
            }
        } else {
            unreachable!("the event is a DecisionMade")
        }
        let applied_now = meta.applied;
        let (cost, counted) = match &event {
            EventPayload::DecisionMade {
                cost_usd,
                cached,
                provider,
                ..
            } => (cost_usd.unwrap_or(0.0), provider.is_some() && !cached),
            _ => (0.0, false),
        };
        let source = meta.source();
        let seq = match journal.append_decision(event) {
            Ok(seq) => seq,
            Err(_) => {
                return (
                    DecisionOutcome::Failed {
                        error_kind: "journal".into(),
                    },
                    None,
                );
            }
        };
        {
            let mut ledger = self.ledger.lock().unwrap_or_else(|e| e.into_inner());
            if counted {
                ledger.requests += 1;
            }
            ledger.cost_usd += cost;
            if applied_now {
                *ledger
                    .actions
                    .entry(call.site.as_str().to_string())
                    .or_default() += 1;
            }
            if let Some(answers) = &outcome_answers {
                ledger.replay.push(Replay {
                    use_site: call.site.as_str().to_string(),
                    node: call.node.map(str::to_string),
                    attempt: call.attempt,
                    state_digest,
                    questions_digest,
                    answers: answers.clone(),
                    meta: meta.clone(),
                });
            }
        }
        if self.settings.privacy.debug_state {
            self.write_debug_state(
                seq,
                &state,
                &request.state_order,
                &call.questions,
                full.as_ref(),
            );
        }
        let outcome = match (outcome_answers, result) {
            (Some(answers), _) => DecisionOutcome::Answered {
                answers,
                mode,
                replayed: false,
                meta: AnswerMeta {
                    seq: Some(seq),
                    ..meta
                },
            },
            (None, Err(e)) => DecisionOutcome::Failed {
                error_kind: e.kind().to_string(),
            },
            (None, Ok(_)) => DecisionOutcome::Failed {
                error_kind: "unavailable".into(),
            },
        };
        (outcome, source)
    }

    /// Journals that a use was not asked on purpose (a routing exclusion
    /// such as a handoff node): a `decision_made` with no provider, no
    /// answers and `join.excluded` naming why. Nothing when the use is off.
    /// No request is made and none is counted.
    pub(crate) fn record_excluded(
        &self,
        journal: &dyn DecisionJournal,
        site: UseSite,
        node: Option<&str>,
        attempt: Option<u32>,
        reason: &str,
    ) {
        let mode = self.mode_for(site);
        if mode == DecisionMode::Off {
            return;
        }
        let _ = journal.append_decision(EventPayload::DecisionMade {
            output_chars: None,
            use_site: site.as_str().to_string(),
            node: node.map(str::to_string),
            attempt,
            provider: None,
            model: None,
            calibrated: false,
            mode: mode.as_str().to_string(),
            questions_digest: String::new(),
            state_digest: String::new(),
            state_bytes: 0,
            answers: BTreeMap::new(),
            applied: false,
            would_change: None,
            baseline: None,
            latency_ms: 0,
            input_tokens: None,
            cost_usd: None,
            cost_estimated: false,
            cached: false,
            error: None,
            enforce_refused: None,
            join: BTreeMap::from([("excluded".to_string(), Value::from(reason))]),
        });
    }

    /// Step 9: the redacted state, the questions and the full answers, for
    /// debugging a use. Best effort: a failed write never affects the run.
    fn write_debug_state(
        &self,
        seq: u64,
        state: &Value,
        state_order: &[String],
        questions: &BTreeMap<String, Question>,
        answers: Option<&BTreeMap<String, Answer>>,
    ) {
        let dir = self.run_dir.join("decisions");
        // `state_order` lets `apb decisions replay` send the state in the
        // order it was sent (key order is part of the prompt).
        let body = json!({"seq": seq, "state": state, "state_order": state_order, "questions": questions, "answers": answers});
        if std::fs::create_dir_all(&dir).is_ok()
            && let Ok(text) = serde_json::to_string_pretty(&body)
        {
            let _ =
                apb_core::fsutil::atomic_write(&dir.join(format!("{seq}.json")), text.as_bytes());
        }
    }
}

/// Characters of the output-class fields as the use handed them (before
/// redaction and clipping), `None` when the state has none.
fn output_chars(parts: &StateParts) -> Option<u64> {
    let mut outputs = parts
        .fields
        .iter()
        .filter(|f| f.class == FieldClass::Output)
        .peekable();
    outputs.peek()?;
    Some(outputs.map(|f| f.text.chars().count() as u64).sum())
}

// --- replay (issue #165 Part 13) ---------------------------------------------

impl DecisionRunner {
    /// A runner over `settings` for `apb decisions replay`: bound to no run
    /// and no journal. Only [`DecisionRunner::ask_unjournaled`] is meant
    /// for it.
    pub(crate) fn for_replay(settings: EffectiveDecisions, root: &Path) -> Self {
        DecisionRunner {
            settings,
            root: root.to_path_buf(),
            run_dir: root.to_path_buf(),
            scrub_names: Vec::new(),
            chain: OnceLock::new(),
            redactor: OnceLock::new(),
            cache: DecisionCache::new(),
            ledger: Mutex::new(Ledger::default()),
        }
    }

    /// Sends a request that was already redacted and clipped (a debug
    /// state) to the chain: no journal, no budget, no cache. Evaluation
    /// only; a run never calls this.
    pub(crate) fn ask_unjournaled(
        &self,
        request: &DecisionRequest,
    ) -> Result<apb_decide::DecisionResponse, apb_decide::DecideError> {
        self.chains().all.decide(request, None)
    }
}

/// The compact journal form of answers, shared with replay.
pub(crate) fn compact_answers(
    answers: &BTreeMap<String, Answer>,
) -> BTreeMap<String, DecisionAnswer> {
    answers
        .iter()
        .map(|(k, a)| (k.clone(), compact_answer(a)))
        .collect()
}

// --- end of replay -----------------------------------------------------------

// --- issue #165 Parts 5-7: routes for the judge uses ------------------------

/// A backend asked outside `decisions.yaml`.
pub(crate) type AskFn<'a> =
    &'a dyn Fn(&DecisionRequest) -> Result<apb_decide::DecisionResponse, apb_decide::DecideError>;

/// Which providers a call goes to.
pub(crate) enum Route<'a> {
    /// Every configured provider, in order (what [`DecisionRunner::decide`] uses).
    All,
    /// The decision models proper, without the `llm_emulation` providers: a
    /// judge's own question.
    Native,
    /// Only the configured `llm_emulation` providers.
    Emulation,
    /// A backend outside the chain (the judge node's profile emulation),
    /// journaled under `id` and `model`.
    Custom {
        id: &'a str,
        model: &'a str,
        ask: AskFn<'a>,
    },
}

/// Who answered a decision.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct AnswerSource {
    pub(crate) provider: String,
    pub(crate) model: String,
    pub(crate) calibrated: bool,
}

impl AnswerMeta {
    /// Who answered, when a provider did (`None` for a failed or skipped
    /// call).
    pub(crate) fn source(&self) -> Option<AnswerSource> {
        self.provider.as_ref().map(|p| AnswerSource {
            provider: p.clone(),
            model: self.model.clone().unwrap_or_default(),
            calibrated: self.calibrated,
        })
    }
}

impl DecisionRunner {
    /// A runner for a run whose manifest has no decisions block, used only
    /// to journal and replay what a judge node's profile emulation answers:
    /// every use is off, no provider is configured, the default privacy and
    /// budget apply.
    pub(crate) fn for_emulation(
        root: &Path,
        run_dir: &Path,
        events: &[Event],
        scrub_names: &[String],
    ) -> Self {
        let settings = EffectiveDecisions {
            mode: DecisionMode::Off,
            timeout_ms: 3000,
            providers: Vec::new(),
            budget: Default::default(),
            privacy: Default::default(),
            uses: BTreeMap::new(),
        };
        Self::with_settings(settings, root, run_dir, events, scrub_names)
    }

    /// Whether any `llm_emulation` provider is configured for the run.
    pub(crate) fn has_emulation(&self) -> bool {
        !self.chains().emulation.is_empty()
    }
}

/// A state field name with a `'static` lifetime, for the judge node's
/// author-named fields. Each distinct name is kept once for the life of the
/// process (bounded by the names the playbooks run here declare).
pub(crate) fn intern(name: &str) -> &'static str {
    static NAMES: OnceLock<Mutex<std::collections::BTreeSet<&'static str>>> = OnceLock::new();
    let mut names = NAMES
        .get_or_init(Default::default)
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    if let Some(n) = names.get(name) {
        return n;
    }
    let leaked: &'static str = Box::leak(name.to_string().into_boxed_str());
    names.insert(leaked);
    leaked
}

// --- end judge routes ---------------------------------------------------------

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
    /// Requests actually sent for this use.
    pub requests: u32,
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
            u.requests += 1;
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
