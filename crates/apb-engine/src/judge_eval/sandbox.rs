//! The judge sandbox: one judge node of one version evaluated over typed
//! inputs or a past run's values, outside any run.
//!
//! These are the request and answer shapes every surface shares (CLI `apb
//! judge eval`, MCP `judge_evaluate`, `POST .../judge/{node}/evaluate`). The
//! request names no provider, URL, key or model: the sandbox asks only the
//! providers of this machine's `decisions.yaml`, so a caller cannot choose
//! where the state goes.
//!
//! [`evaluate`] is the one entry: it renders the state through the shared
//! evaluation (`super::evaluate`), asks through a [`DecisionRunner`] made
//! outside any run (privacy classes, redaction, clip and mode rule as in a
//! run), and appends one `.apb/decisions.jsonl` line per request with
//! `use: judge_sandbox` and no state. At most [`SANDBOX_DAILY_CAP`]
//! requests go out per project and UTC day, counted under the log's lock.
//! It never starts a program (the profile emulation is never run), never
//! writes a run directory, journal, cache, trust or quality record, and
//! never applies an answer anywhere.

use std::collections::BTreeMap;
use std::path::Path;

use apb_core::decisions::DecisionMode;
use apb_core::schema::{NodeKind, Playbook};
use apb_decide::UseSite;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::{Answers, JudgeEvaluation, JudgeInputs, JudgeOutcome, JudgeRequest};
use crate::decision::judge::output_from_answers;
use crate::decision::standalone::{DecisionLog, lock_lifetime};
use crate::decision::{
    DecisionJournal, DecisionOutcome, DecisionRunner, Judgement, RequestCap, Route,
};
use crate::error::EngineError;
use crate::event::{DecisionAnswer, EventPayload};

/// The `use` a sandbox line of `.apb/decisions.jsonl` carries: the daily cap
/// counts these lines only, and no other cap counts them.
pub const USE_JUDGE_SANDBOX: &str = crate::decision::standalone::JUDGE_SANDBOX_USE;

/// Sandbox requests per project and UTC day. Fixed: a `decisions.yaml` key
/// for it would make the file invalid for older apb.
pub const SANDBOX_DAILY_CAP: u32 = 100;

/// What the route preview does not model, said with every preview.
pub const ROUTE_PREVIEW_NOTE: &str = "the route preview follows the node's edges over its output alone: edge budgets, joins and judge edges are not modelled";

/// Who gave answers a person typed (or an example's observed answers), as
/// the output's `decided_by` names them.
pub const GIVEN_BY: &str = "given";

/// Added to an `emulate` outcome: the sandbox never starts the profile.
pub const PROFILE_NOTE: &str = "the node would start its profile here; the sandbox never does";

/// Where a run directory would stand: a clip note points into it. Never
/// created; the sandbox refuses to run while anything sits there, so a
/// planted directory cannot hold what a run directory would.
const SCRATCH: &str = "judge-sandbox";

/// What a param declared secret is sent and stored as (a labelled
/// example keeps it so too).
pub const SECRET_PLACEHOLDER: &str = "<secret>";

/// What a decision model answered for an example's inputs, and who.
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ObservedAnswers {
    pub provider: String,
    pub model: String,
    pub answers: BTreeMap<String, DecisionAnswer>,
}

/// One sandbox evaluation, as a surface receives it.
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SandboxRequest {
    /// The playbook version; `current` when absent.
    #[serde(default)]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub version: Option<String>,
    #[serde(default)]
    pub inputs: JudgeInputs,
    /// Answers given per question (typed by a person, or an example's
    /// observed answers): evaluated without a request.
    #[serde(default)]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub answers: Option<BTreeMap<String, DecisionAnswer>>,
    /// When the node would hand over to its emulation, ask the configured
    /// `llm_emulation` providers as a run would (priced, inside the daily
    /// cap). The node's profile emulation is never started.
    #[serde(default)]
    pub emulate: bool,
}

/// What a sandbox evaluation came to: the three answers side by side (the
/// raw answers, what the node outputs on this machine now, what it would
/// output at enforce) with the route of each, and what the day has spent.
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SandboxEvaluation {
    pub playbook: String,
    pub version: String,
    pub node: String,
    pub evaluation: JudgeEvaluation,
    /// Edge addresses the enforce output would take; empty when the
    /// answers cannot apply.
    pub route_at_enforce: Vec<String>,
    /// Edge addresses the output on this machine would take.
    pub route_as_configured: Vec<String>,
    /// [`ROUTE_PREVIEW_NOTE`].
    pub route_note: String,
    /// Sandbox requests of this project today, this one included.
    pub requests_today: u32,
    pub daily_cap: u32,
}

/// One value a past run recorded, offered to place into any input: a node
/// output, a review note, a param, a baseline observation (as JSON text).
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecordedValue {
    /// Where it came from: `outputs.<node>`, `reviews.<node>.note`,
    /// `params.<name>`, `baseline.<check>.before`, `baseline.<check>.after`.
    pub label: String,
    /// Redacted with the run tree's redactor.
    pub text: String,
}

/// "Fill from a run": a judge execution's inputs as the run had them when
/// it started, the run's other recorded values, and what the model
/// answered then.
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RunValues {
    pub run_id: String,
    pub node: String,
    /// The judge node's execution, 1-based.
    pub execution: u32,
    /// Redacted with the run tree's redactor; a secret param reads
    /// [`SECRET_PLACEHOLDER`].
    pub inputs: JudgeInputs,
    #[serde(default)]
    pub recorded: Vec<RecordedValue>,
    /// The answers of that execution's `decision_made`, when it asked.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub observed: Option<ObservedAnswers>,
}

/// Where a run directory would stand for an evaluation outside a run
/// ([`SCRATCH`] under the project's `.apb`), refused while anything sits
/// there.
pub(crate) fn scratch(root: &Path) -> Result<std::path::PathBuf, EngineError> {
    let scratch = root.join(".apb").join(SCRATCH);
    if std::fs::symlink_metadata(&scratch).is_ok() {
        return Err(EngineError::Conflict(format!(
            "`{}` exists; the judge sandbox keeps nothing there, remove it",
            scratch.display()
        )));
    }
    Ok(scratch)
}

/// The HTTP requests a `decisions.jsonl` line counts against the sandbox's
/// day: its `requests` (the worst case its ask kept), else one for an
/// answered, uncached line; nothing for a line of another use.
fn sandbox_requests(v: &Value) -> u32 {
    if v["use"] != USE_JUDGE_SANDBOX {
        return 0;
    }
    match v["requests"].as_u64() {
        Some(n) => u32::try_from(n).unwrap_or(u32::MAX),
        None => u32::from(!v["provider"].is_null() && v["cached"] != true),
    }
}

/// Sandbox requests of the project at `root` today (UTC).
pub fn requests_today(root: &Path) -> u32 {
    DecisionLog::of(root).weigh_today(apb_core::clock::now_ms_u64(), sandbox_requests)
}

/// Evaluates the judge node `node` of project playbook `id` (`req.version`,
/// else `current`) over `req.inputs`: with `req.answers` given, without a
/// request, applied at this machine's mode; else asked through this
/// machine's `decisions.yaml` inside the daily cap. A param the playbook
/// declares secret, or one that looks like a credential, is sent as
/// [`SECRET_PLACEHOLDER`].
///
/// Who may ask (a person, not a run step; a dashboard with keys for a live
/// ask) is the surface's check; this function only spends within the cap.
pub fn evaluate(
    root: &Path,
    id: &str,
    node: &str,
    req: &SandboxRequest,
) -> Result<SandboxEvaluation, EngineError> {
    use apb_core::registry::{Registry, is_safe_segment};
    if !is_safe_segment(id) {
        return Err(EngineError::NotFound(format!("playbook `{id}`")));
    }
    if !is_safe_segment(node) {
        return Err(EngineError::NotFound(format!("node `{node}`")));
    }
    let loaded = Registry::open(root)?.load(id, req.version.as_deref())?;
    let playbook = &loaded.playbook;
    match playbook.node(node).map(|n| &n.kind) {
        Some(NodeKind::Judge { .. }) => {}
        Some(_) => {
            return Err(EngineError::Invalid(format!(
                "node `{node}` of `{id}` is not a judge"
            )));
        }
        None => return Err(EngineError::NotFound(format!("node `{node}` of `{id}`"))),
    }
    let scratch = scratch(root)?;
    let inputs = without_secrets(playbook, &req.inputs);
    let request = JudgeRequest {
        playbook,
        node_id: node,
        inputs: &inputs,
        clip_dir: &scratch,
    };
    let settings = apb_core::decisions::resolve(root).active();
    let log = DecisionLog::of(root);
    let mut evaluation = match &req.answers {
        Some(answers) => {
            let mode = settings.map(|s| {
                DecisionRunner::outside_run(s, root, &scratch).mode_for(UseSite::JudgeNode)
            });
            super::evaluate_judge(
                &request,
                Answers::GivenAt {
                    answers,
                    by: GIVEN_BY,
                    mode,
                },
            )?
        }
        None => {
            if crate::read_only::active() {
                return Err(EngineError::Conflict(
                    "a read-only process asks no decision model; give the answers instead".into(),
                ));
            }
            ask(
                &request,
                settings,
                root,
                &scratch,
                &log,
                id,
                &loaded.version,
                req.emulate,
            )?
        }
    };
    evaluation.as_configured = with_profile_note(evaluation.as_configured);
    let route = |o: &JudgeOutcome| match o {
        JudgeOutcome::Output { output } | JudgeOutcome::Fallback { output, .. } => {
            super::route_of(playbook, node, output)
        }
        JudgeOutcome::Fail { .. } | JudgeOutcome::Emulate { .. } => Vec::new(),
    };
    Ok(SandboxEvaluation {
        playbook: id.to_string(),
        version: loaded.version.clone(),
        node: node.to_string(),
        route_at_enforce: route(&evaluation.at_enforce),
        route_as_configured: route(&evaluation.as_configured),
        evaluation,
        route_note: ROUTE_PREVIEW_NOTE.to_string(),
        requests_today: requests_today(root),
        daily_cap: SANDBOX_DAILY_CAP,
    })
}

/// `inputs` with every sensitive param (declared secret, named like one, or
/// shaped like a credential) as the placeholder: its value never leaves.
pub(crate) fn without_secrets(playbook: &Playbook, inputs: &JudgeInputs) -> JudgeInputs {
    let mut out = inputs.clone();
    for (name, value) in out.params.iter_mut() {
        let declared = playbook.params.iter().any(|p| p.name == *name && p.secret);
        if apb_core::run_params::sensitive(declared, name, value).is_some() {
            *value = SECRET_PLACEHOLDER.to_string();
        }
    }
    out
}

/// The live ask: under the decisions log's lock, the day's count first,
/// then the runner over the machine's settings with the requests left today
/// as its request cap, so an ask whose worst case (every provider with its
/// retries) does not fit is journaled `budget` and never sent, and each
/// line counts what its ask kept.
#[allow(clippy::too_many_arguments)]
fn ask(
    request: &JudgeRequest,
    settings: Option<apb_core::decisions::EffectiveDecisions>,
    root: &Path,
    scratch: &Path,
    log: &DecisionLog,
    id: &str,
    version: &str,
    emulate: bool,
) -> Result<JudgeEvaluation, EngineError> {
    // Until the day is counted under the lock, nothing may be sent.
    let idle = RequestCap::new(0);
    let journal = SandboxLog {
        log,
        playbook: id,
        version,
        cap: &idle,
    };
    let Some(mut settings) = settings else {
        return super::evaluate_judge(
            request,
            Answers::Runner {
                runner: None,
                journal: &journal,
            },
        );
    };
    let off = DecisionRunner::outside_run(settings.clone(), root, scratch);
    if off.mode_for(UseSite::JudgeNode) == DecisionMode::Off {
        // Nothing asked, nothing journaled.
        return super::evaluate_judge(
            request,
            Answers::Runner {
                runner: Some(&off),
                journal: &journal,
            },
        );
    }
    // The native question and, at most, one emulation question.
    let lifetime = lock_lifetime(&settings, 2);
    let now = apb_core::clock::now_ms_u64();
    let (_lock, broke_stale) = log.lock(lifetime).ok_or_else(|| {
        EngineError::Conflict(
            "the project's decisions log is held by another request; try again shortly".into(),
        )
    })?;
    if broke_stale {
        log.note_stale_lock(now, lifetime);
    }
    // A log that is not a plain file, or cannot be opened to append, counts
    // as a spent day: a request it could not count is never sent.
    let used = match log.open(true) {
        Ok(Some(_)) => log.weigh_today(now, sandbox_requests),
        _ => SANDBOX_DAILY_CAP,
    };
    let left = SANDBOX_DAILY_CAP.saturating_sub(used);
    settings.budget.max_requests_per_run = settings.budget.max_requests_per_run.min(left);
    let cap = std::sync::Arc::new(RequestCap::new(left));
    let journal = SandboxLog {
        cap: &cap,
        ..journal
    };
    let runner = DecisionRunner::outside_run(settings, root, scratch).with_request_cap(cap.clone());
    let evaluated = super::evaluate(
        request,
        Answers::Runner {
            runner: Some(&runner),
            journal: &journal,
        },
    )?;
    let mut evaluation = evaluated.evaluation;
    let JudgeOutcome::Emulate { reason } = &evaluation.as_configured else {
        return Ok(evaluation);
    };
    let reason = reason.clone();
    let Some(NodeKind::Judge {
        questions,
        thresholds,
        ..
    }) = request.playbook.node(request.node_id).map(|n| &n.kind)
    else {
        return Ok(evaluation);
    };
    let mut emulated = None;
    if emulate && runner.has_emulation() {
        let before = runner.spent();
        let judge = |answers: &BTreeMap<String, DecisionAnswer>| Judgement {
            applied: output_from_answers(questions, thresholds, answers, "").is_ok(),
            would_change: None,
        };
        let (outcome, source) = runner.decide_routed(
            &journal,
            super::call(request.node_id, &evaluated.parts, &evaluated.wire, &judge),
            Route::Emulation,
        );
        let after = runner.spent();
        evaluation.requests += after.0.saturating_sub(before.0);
        let cost = after.1 - before.1;
        if cost > 0.0 {
            evaluation.cost_usd = Some(evaluation.cost_usd.unwrap_or(0.0) + cost);
        }
        if let DecisionOutcome::Answered { answers, .. } = outcome {
            let by = format!(
                "emulated:{}",
                source.map_or_else(|| "unknown".to_string(), |s| s.provider)
            );
            emulated = output_from_answers(questions, thresholds, &answers, &by).ok();
        }
    }
    evaluation.as_configured = match emulated {
        Some(output) => JudgeOutcome::Output { output },
        None => JudgeOutcome::Emulate { reason },
    };
    Ok(evaluation)
}

/// An `emulate` outcome says the sandbox does not start the profile.
fn with_profile_note(outcome: JudgeOutcome) -> JudgeOutcome {
    match outcome {
        JudgeOutcome::Emulate { reason } => JudgeOutcome::Emulate {
            reason: format!("{reason}; {PROFILE_NOTE}"),
        },
        other => other,
    }
}

/// The sandbox's journal: one `decisions.jsonl` line per decision, with
/// `use: judge_sandbox`, the playbook and node, and the `decision_made`
/// fields except the state. A line that cannot be written fails the ask.
struct SandboxLog<'a> {
    log: &'a DecisionLog,
    playbook: &'a str,
    version: &'a str,
    /// The day's request cap: a line counts what its ask kept.
    cap: &'a RequestCap,
}

impl DecisionJournal for SandboxLog<'_> {
    fn append_decision(&self, payload: EventPayload) -> Result<Option<u64>, EngineError> {
        let EventPayload::DecisionMade {
            use_site,
            node,
            provider,
            model,
            calibrated,
            mode,
            questions_digest,
            state_digest,
            state_bytes,
            answers,
            would_change,
            latency_ms,
            input_tokens,
            cost_usd,
            cost_estimated,
            cached,
            error,
            ..
        } = payload
        else {
            return Ok(None);
        };
        let line = json!({
            "ts_ms": apb_core::clock::now_ms_u64(),
            "use_site": use_site,
            "use": USE_JUDGE_SANDBOX,
            "playbook": self.playbook,
            "version": self.version,
            "node": node,
            "provider": provider,
            "model": model,
            "calibrated": calibrated,
            "mode": mode,
            "questions_digest": questions_digest,
            "state_digest": state_digest,
            "state_bytes": state_bytes,
            "answers": answers,
            "applied": false,
            "would_change": would_change,
            "latency_ms": latency_ms,
            "input_tokens": input_tokens,
            "cost_usd": cost_usd,
            "cost_estimated": cost_estimated,
            "cached": cached,
            "error": error,
            "requests": self.cap.take_charge(),
        });
        self.log.try_append(&line).map_err(|()| {
            EngineError::Conflict("the project's decisions log could not be written".into())
        })?;
        // No seq: nothing writes a debug state for a sandbox decision.
        Ok(None)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const PLAYBOOK: &str = "schema: 2\nid: j\nname: j\nversion: 1.0.0\nparams:\n  - { name: api_token, type: string, secret: true }\nnodes:\n  - { id: start, type: start }\n  - id: triage\n    type: judge\n    state: { task: \"{{run.instruction}}\", key: \"{{params.api_token}}\" }\n    questions:\n      verdict: { type: choice, instructions: \"Which?\", criteria: { clean: a, needs_fix: b } }\n    thresholds:\n      verdict: { min_confidence: 0.5, below: needs_fix }\nON_UNAVAILABLE  - { id: fix, type: finish, outcome: success }\n  - { id: done, type: finish, outcome: success }\nedges:\n  - { from: start, to: triage }\n  - { id: to-fix, from: triage, to: fix, condition: { type: output_field, node: triage, field: verdict, equals: needs_fix } }\n  - { from: triage, to: done, fallback: true }\n";

    /// A project with playbook `j` 1.0.0 as `current`, its judge's
    /// `on_unavailable` set to `on_unavailable` (a YAML line or empty).
    fn project(on_unavailable: &str) -> tempfile::TempDir {
        let root = tempfile::tempdir().unwrap();
        let dir = root.path().join(".apb/playbooks/j");
        std::fs::create_dir_all(dir.join("1.0.0")).unwrap();
        std::fs::write(
            dir.join("1.0.0/playbook.yaml"),
            PLAYBOOK.replace("ON_UNAVAILABLE", on_unavailable),
        )
        .unwrap();
        std::fs::write(dir.join("current"), "1.0.0\n").unwrap();
        root
    }

    /// Points this (child) process at a config dir whose `decisions.yaml`
    /// is `yaml`, or none.
    fn machine(yaml: Option<&str>) -> tempfile::TempDir {
        let cfg = tempfile::tempdir().unwrap();
        if let Some(yaml) = yaml {
            std::fs::write(cfg.path().join(apb_core::decisions::DECISIONS_FILE), yaml).unwrap();
        }
        // SAFETY: a plain child process runs this one test on one thread.
        unsafe {
            std::env::set_var("APB_CONFIG_DIR", cfg.path());
            std::env::set_var("APB_DECISIONS_ALLOW_FAKE", "1");
        }
        cfg
    }

    /// The fake provider answering `clean` at `confidence`, the
    /// `judge_node` use at `mode`, and `extra` lines of settings.
    fn decisions(mode: &str, confidence: f64, extra: &str) -> String {
        format!(
            "version: 1\nmode: enforce\nproviders:\n  - id: fake\n    kind: fake\n    answers:\n      verdict: {{ type: choice, choice: clean, probabilities: {{ clean: 0.9, needs_fix: 0.1 }}, confidence: {confidence} }}\nuses:\n  judge_node: {{ mode: {mode} }}\n{extra}"
        )
    }

    fn typed() -> SandboxRequest {
        SandboxRequest {
            inputs: JudgeInputs {
                instruction: Some("Fix the parser".into()),
                params: BTreeMap::from([(
                    "api_token".to_string(),
                    "sk-live-abcdefghijklmnopqrstuvwx".to_string(),
                )]),
                ..Default::default()
            },
            ..Default::default()
        }
    }

    fn lines(root: &Path) -> Vec<Value> {
        std::fs::read_to_string(
            root.join(".apb")
                .join(crate::decision::standalone::DECISIONS_LOG),
        )
        .unwrap_or_default()
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect()
    }

    /// Writes `n` answered lines of today to the log, each built by `line`.
    fn seed_lines(root: &Path, n: usize, line: impl Fn() -> Value) {
        let log = DecisionLog::of(root);
        for _ in 0..n {
            log.append(&line());
        }
    }

    /// At `shadow` the sandbox shows the three answers side by side, each
    /// with its route: the raw answer, what this machine does (the fallback,
    /// since shadow never applies) and what enforce would output. One
    /// request, one sandbox line in the project's log, no state in it.
    #[test]
    fn at_shadow_the_raw_the_fallback_and_the_enforce_output_each_route() {
        apb_core::process_mode::testing::in_plain_child(
            "judge_eval::sandbox::tests::at_shadow_the_raw_the_fallback_and_the_enforce_output_each_route",
            || {
                let _cfg = machine(Some(&decisions("shadow", 0.9, "")));
                let root = project("    on_unavailable: { default: { verdict: needs_fix } }\n");
                let out = evaluate(root.path(), "j", "triage", &typed()).unwrap();
                let e = &out.evaluation;
                assert_eq!(e.mode, Some(DecisionMode::Shadow));
                assert_eq!(e.raw["verdict"].value, Some(json!("clean")), "{e:?}");
                assert!(
                    matches!(&e.at_enforce, JudgeOutcome::Output { output } if output["verdict"] == "clean" && output["decided_by"] == "fake/fake-1"),
                    "{:?}",
                    e.at_enforce
                );
                assert!(
                    matches!(&e.as_configured, JudgeOutcome::Fallback { reason, output } if reason == "mode" && output["verdict"] == "needs_fix"),
                    "{:?}",
                    e.as_configured
                );
                assert_eq!(out.route_at_enforce, ["e:triage>done#1"]);
                assert_eq!(out.route_as_configured, ["id:to-fix"]);
                assert_eq!(e.would_change, Some(true));
                // One decision; the day counts its worst case (the
                // provider with its retries).
                assert_eq!(
                    (e.requests, out.requests_today),
                    (1, crate::decision::PROVIDER_ATTEMPTS)
                );
                assert_eq!(out.daily_cap, SANDBOX_DAILY_CAP);
                let log = lines(root.path());
                assert_eq!(log.len(), 1, "{log:?}");
                assert_eq!(log[0]["use"], USE_JUDGE_SANDBOX);
                assert_eq!(log[0]["use_site"], "judge_node");
                assert_eq!(log[0]["provider"], "fake");
                let text = log[0].to_string();
                assert!(
                    !text.contains("Fix the parser") && !text.contains("sk-live"),
                    "{text}"
                );
                // Nothing of a run is left behind.
                assert!(!root.path().join(".apb/runs").exists());
                assert!(!root.path().join(".apb").join(SCRATCH).exists());
            },
        );
    }

    /// At `off`, and without any `decisions.yaml`, nothing is asked and
    /// nothing logged; answers a person gives still evaluate, at this
    /// machine's mode, and at enforce they apply.
    #[test]
    fn at_off_nothing_is_asked_and_given_answers_still_evaluate() {
        apb_core::process_mode::testing::in_plain_child(
            "judge_eval::sandbox::tests::at_off_nothing_is_asked_and_given_answers_still_evaluate",
            || {
                let root = project("    on_unavailable: { default: { verdict: needs_fix } }\n");
                let given: DecisionAnswer = serde_json::from_value(serde_json::json!({
                    "value": "clean", "p": 0.9, "confidence": 0.9
                }))
                .unwrap();
                let with_answers = SandboxRequest {
                    answers: Some(BTreeMap::from([("verdict".to_string(), given)])),
                    ..typed()
                };
                for (yaml, reason) in [
                    (Some(decisions("off", 0.9, "")), "off"),
                    (None, "not_configured"),
                ] {
                    let _cfg = machine(yaml.as_deref());
                    let asked = evaluate(root.path(), "j", "triage", &typed()).unwrap();
                    assert_eq!(asked.evaluation.requests, 0);
                    assert!(asked.evaluation.raw.is_empty());
                    assert!(
                        matches!(&asked.evaluation.as_configured, JudgeOutcome::Fallback { reason: r, .. } if r == reason),
                        "{reason}: {:?}",
                        asked.evaluation.as_configured
                    );
                    let given = evaluate(root.path(), "j", "triage", &with_answers).unwrap();
                    assert!(
                        matches!(&given.evaluation.at_enforce, JudgeOutcome::Output { output } if output["verdict"] == "clean" && output["decided_by"] == GIVEN_BY),
                        "{:?}",
                        given.evaluation.at_enforce
                    );
                    assert!(
                        matches!(&given.evaluation.as_configured, JudgeOutcome::Fallback { reason: r, .. } if r == reason),
                        "{reason}: {:?}",
                        given.evaluation.as_configured
                    );
                    assert!(lines(root.path()).is_empty(), "{reason}");
                }
                let _cfg = machine(Some(&decisions("enforce", 0.9, "")));
                let given = evaluate(root.path(), "j", "triage", &with_answers).unwrap();
                assert_eq!(given.evaluation.as_configured, given.evaluation.at_enforce);
                assert_eq!(given.route_as_configured, ["e:triage>done#1"]);
                assert!(lines(root.path()).is_empty());
            },
        );
    }

    /// The day's cap counts sandbox lines only: other `judge_node` lines of
    /// the log do not shorten it. The ask whose worst case reaches 100 goes
    /// out; the next is logged `budget` without a provider and the node
    /// takes its fallback.
    #[test]
    fn the_hundred_and_first_request_of_a_day_is_refused_budget() {
        apb_core::process_mode::testing::in_plain_child(
            "judge_eval::sandbox::tests::the_hundred_and_first_request_of_a_day_is_refused_budget",
            || {
                let _cfg = machine(Some(&decisions("enforce", 0.9, "")));
                let root = project("    on_unavailable: { default: { verdict: needs_fix } }\n");
                let now = apb_core::clock::now_ms_u64();
                let answered = |extra: Value| {
                    let mut v = json!({ "ts_ms": now, "use_site": "judge_node", "provider": "fake", "cached": false });
                    v.as_object_mut()
                        .unwrap()
                        .extend(extra.as_object().unwrap().clone());
                    v
                };
                let worst = crate::decision::PROVIDER_ATTEMPTS;
                seed_lines(root.path(), (SANDBOX_DAILY_CAP - worst) as usize, || {
                    answered(json!({ "use": USE_JUDGE_SANDBOX }))
                });
                seed_lines(root.path(), 50, || answered(json!({})));
                // Yesterday's lines are another day.
                seed_lines(root.path(), 5, || {
                    answered(json!({ "use": USE_JUDGE_SANDBOX, "ts_ms": now - DAY }))
                });
                let last = evaluate(root.path(), "j", "triage", &typed()).unwrap();
                assert_eq!(last.evaluation.requests, 1);
                assert_eq!(last.requests_today, SANDBOX_DAILY_CAP);
                assert!(matches!(
                    last.evaluation.as_configured,
                    JudgeOutcome::Output { .. }
                ));
                let refused = evaluate(root.path(), "j", "triage", &typed()).unwrap();
                assert_eq!(refused.evaluation.requests, 0);
                assert!(
                    matches!(&refused.evaluation.as_configured, JudgeOutcome::Fallback { reason, .. } if reason == "budget"),
                    "{:?}",
                    refused.evaluation.as_configured
                );
                let log = lines(root.path());
                let tail = log.last().unwrap();
                assert_eq!(
                    (tail["error"].as_str(), tail["provider"].is_null()),
                    (Some("budget"), true)
                );
                assert_eq!(refused.requests_today, SANDBOX_DAILY_CAP);
            },
        );
    }

    /// The cap counts HTTP requests, not decisions: a provider that fails
    /// once and answers on its retry sends two requests for one decision.
    /// With 99 spent the ask is refused before anything leaves, since its
    /// worst case does not fit; with room, the line weighs what the ask
    /// reserved, so the day never passes the cap.
    #[cfg(unix)]
    #[test]
    fn retries_and_fallbacks_never_pass_the_daily_cap() {
        apb_core::process_mode::testing::in_plain_child(
            "judge_eval::sandbox::tests::retries_and_fallbacks_never_pass_the_daily_cap",
            || {
                let reply = json!({
                    "model": "m",
                    "answers": { "verdict": { "type": "choice", "choice": "clean", "probabilities": { "clean": 0.9, "needs_fix": 0.1 }, "confidence": 0.9 } },
                });
                let stub = apb_decide::testing::StubServer::start_with_fallback(
                    vec![apb_decide::testing::StubResponse::json(500, "{}")],
                    apb_decide::testing::StubResponse::json(200, reply.to_string()),
                );
                let _cfg = machine(Some(&format!(
                    "version: 1\nmode: enforce\ntimeout_ms: 2000\nproviders:\n  - {{ id: a, kind: systemone, base_url: \"{}\", model: m }}\nuses:\n  judge_node: {{ mode: enforce }}\n",
                    stub.base_url
                )));
                let root = project("    on_unavailable: { default: { verdict: needs_fix } }\n");
                let now = apb_core::clock::now_ms_u64();
                seed_lines(
                    root.path(),
                    99,
                    || json!({ "ts_ms": now, "use_site": "judge_node", "use": USE_JUDGE_SANDBOX, "provider": "a", "cached": false }),
                );
                let out = evaluate(root.path(), "j", "triage", &typed()).unwrap();
                assert!(
                    99 + stub.count() <= SANDBOX_DAILY_CAP as usize,
                    "{} requests sent over 99 spent",
                    stub.count()
                );
                assert!(out.requests_today <= SANDBOX_DAILY_CAP);
                // A day with room: the retry goes out and the line weighs
                // the ask's worst case.
                let fresh = project("    on_unavailable: { default: { verdict: needs_fix } }\n");
                let before = stub.count();
                let asked = evaluate(fresh.path(), "j", "triage", &typed()).unwrap();
                assert!(matches!(
                    asked.evaluation.as_configured,
                    JudgeOutcome::Output { .. }
                ));
                let sent = (stub.count() - before) as u32;
                assert!(sent >= 1);
                assert!(asked.requests_today >= sent, "{asked:?}");
            },
        );
    }

    const DAY: u64 = 86_400_000;

    /// A log the repository planted as a directory is a spent day: nothing
    /// is sent that could not be counted.
    #[test]
    fn a_log_that_is_not_a_plain_file_sends_nothing() {
        apb_core::process_mode::testing::in_plain_child(
            "judge_eval::sandbox::tests::a_log_that_is_not_a_plain_file_sends_nothing",
            || {
                let _cfg = machine(Some(&decisions("enforce", 0.9, "")));
                let root = project("    on_unavailable: { default: { verdict: needs_fix } }\n");
                std::fs::create_dir(
                    root.path()
                        .join(".apb")
                        .join(crate::decision::standalone::DECISIONS_LOG),
                )
                .unwrap();
                let out = evaluate(root.path(), "j", "triage", &typed()).unwrap();
                assert_eq!(out.evaluation.requests, 0);
                assert!(
                    matches!(&out.evaluation.as_configured, JudgeOutcome::Fallback { reason, .. } if reason == "budget"),
                    "{:?}",
                    out.evaluation.as_configured
                );
            },
        );
    }

    /// A state over `privacy.max_state_bytes` is clipped as a run clips it
    /// and still asked, never skipped; a param declared secret is sent as
    /// the placeholder.
    #[test]
    fn a_large_state_is_clipped_and_asked_and_a_secret_param_never_leaves() {
        apb_core::process_mode::testing::in_plain_child(
            "judge_eval::sandbox::tests::a_large_state_is_clipped_and_asked_and_a_secret_param_never_leaves",
            || {
                let _cfg = machine(Some(&decisions(
                    "enforce",
                    0.9,
                    "privacy: { max_state_bytes: 1024 }\n",
                )));
                let root = project("");
                let mut req = typed();
                req.inputs.instruction = Some("x".repeat(40_000));
                let out = evaluate(root.path(), "j", "triage", &req).unwrap();
                assert_eq!(out.evaluation.requests, 1, "{:?}", out.evaluation);
                assert!(matches!(
                    out.evaluation.as_configured,
                    JudgeOutcome::Output { .. }
                ));
                let line = &lines(root.path())[0];
                assert!(line["state_bytes"].as_u64().unwrap() <= 2048, "{line}");
                let key = out
                    .evaluation
                    .fields
                    .iter()
                    .find(|f| f.name == "key")
                    .unwrap();
                assert_eq!(key.bytes, SECRET_PLACEHOLDER.len());
            },
        );
    }

    /// The sandbox and a run agree: one judge-only run on the fake
    /// provider and the sandbox over the same inputs send the same state
    /// and questions (a state over `privacy.max_state_bytes` clipped alike,
    /// never skipped), get the same answers and output the same.
    #[test]
    fn the_sandbox_and_a_run_send_the_same_state_and_output_the_same() {
        apb_core::process_mode::testing::in_plain_child(
            "judge_eval::sandbox::tests::the_sandbox_and_a_run_send_the_same_state_and_output_the_same",
            || {
                let _cfg = machine(Some(&decisions(
                    "enforce",
                    0.9,
                    "privacy: { max_state_bytes: 1024 }\n",
                )));
                let root = tempfile::tempdir().unwrap();
                apb_core::registry::init_project(root.path()).unwrap();
                let yaml = PLAYBOOK
                    .replace(
                        "params:\n  - { name: api_token, type: string, secret: true }\n",
                        "",
                    )
                    .replace(", key: \"{{params.api_token}}\"", "")
                    .replace("ON_UNAVAILABLE", "");
                let dir = root.path().join(".apb/playbooks/j");
                std::fs::create_dir_all(dir.join("1.0.0")).unwrap();
                std::fs::write(dir.join("1.0.0/playbook.yaml"), &yaml).unwrap();
                std::fs::write(dir.join("current"), "1.0.0\n").unwrap();
                let instruction = format!("Fix the parser {}", "x".repeat(40_000));
                let ran = crate::scheduler::run(
                    root.path(),
                    "j",
                    None,
                    crate::scheduler::RunOptions {
                        instruction: Some(instruction.clone()),
                        ..Default::default()
                    },
                )
                .unwrap();
                let events =
                    crate::event::read_all(&root.path().join(".apb/runs").join(&ran.run_id))
                        .unwrap();
                let (run_state, run_questions, run_answers) = events
                    .iter()
                    .find_map(|e| match &e.payload {
                        EventPayload::DecisionMade {
                            state_digest,
                            questions_digest,
                            answers,
                            ..
                        } => Some((
                            state_digest.clone(),
                            questions_digest.clone(),
                            answers.clone(),
                        )),
                        _ => None,
                    })
                    .expect("the run asked the judge");
                let run_output = events
                    .iter()
                    .rev()
                    .find_map(|e| match &e.payload {
                        EventPayload::NodeFinished { node, output, .. } if node == "triage" => {
                            Some(serde_json::from_str::<Value>(output).unwrap())
                        }
                        _ => None,
                    })
                    .unwrap();
                let req = SandboxRequest {
                    inputs: JudgeInputs {
                        instruction: Some(instruction),
                        ..Default::default()
                    },
                    ..Default::default()
                };
                let out = evaluate(root.path(), "j", "triage", &req).unwrap();
                let e = &out.evaluation;
                assert_eq!(e.requests, 1);
                assert_eq!(e.state_digest, run_state);
                assert_eq!(e.questions_digest, run_questions);
                assert_eq!(e.raw, run_answers);
                let JudgeOutcome::Output { output } = &e.as_configured else {
                    panic!("{:?}", e.as_configured)
                };
                assert_eq!(Value::Object(output.clone()), run_output);
            },
        );
    }

    /// A node that would hand over to its emulation never starts its
    /// profile here: without an `llm_emulation` provider the outcome says
    /// so, and no run directory appears.
    #[test]
    fn emulation_never_starts_the_profile() {
        apb_core::process_mode::testing::in_plain_child(
            "judge_eval::sandbox::tests::emulation_never_starts_the_profile",
            || {
                // The provider answers at a confidence the threshold turns
                // down to `needs_fix`? No: an invalid answer (no option).
                let _cfg = machine(Some(
                    &decisions("enforce", 0.9, "").replace("choice: clean", "choice: nothing"),
                ));
                let root = project("    on_unavailable: emulate\n    profile: main\n");
                let req = SandboxRequest {
                    emulate: true,
                    ..typed()
                };
                let out = evaluate(root.path(), "j", "triage", &req).unwrap();
                assert!(
                    matches!(&out.evaluation.as_configured, JudgeOutcome::Emulate { reason } if reason.starts_with("invalid") && reason.ends_with(PROFILE_NOTE)),
                    "{:?}",
                    out.evaluation.as_configured
                );
                assert!(out.route_as_configured.is_empty());
                assert!(!root.path().join(".apb/runs").exists());
            },
        );
    }

    /// The request has no place for a provider, a URL, a key or a model:
    /// one sent anyway is refused, never ignored into a default, so the
    /// sandbox can only ask the machine's own providers.
    #[test]
    fn a_sandbox_request_cannot_name_where_it_is_sent() {
        let ok: SandboxRequest = serde_json::from_value(serde_json::json!({
            "inputs": { "instruction": "Fix the parser" },
            "answers": { "verdict": { "value": "clean", "p": 0.9, "confidence": 0.8 } },
        }))
        .unwrap();
        assert_eq!(ok.inputs.instruction.as_deref(), Some("Fix the parser"));
        assert!(!ok.emulate);
        for (key, value) in [
            ("provider", "openrouter"),
            ("url", "https://example.invalid/v1"),
            ("api_key", "sk-test"),
            ("model", "any"),
        ] {
            let mut body = serde_json::json!({ "inputs": {} });
            body[key] = value.into();
            let err = serde_json::from_value::<SandboxRequest>(body).unwrap_err();
            assert!(err.to_string().contains(key), "{key}: {err}");
        }
    }
}
