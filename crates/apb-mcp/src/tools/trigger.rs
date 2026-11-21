//! The fire every rule start goes through (start rules): the start
//! sequence of design D1 "The fire", built from the same `tools::` checks
//! the MCP, HTTP and CLI starts share, in the order a start surface runs
//! them. `fire` is what the sweep calls; `fire_traced` is the same steps
//! collecting one [`CheckLine`] each, so `apb trigger test` and MCP
//! `trigger_test` report exactly what a real start meets and there is no
//! second implementation. [`approval_need`] and [`approve`] are a person's
//! approval of a rule (D1 "Approval"); [`test_rule`] evaluates one stored
//! event and runs the dry fire, writing nothing (D1 "`apb trigger test`");
//! [`list`] and [`sweep_root`] are the read view and the host entry of the
//! sweep.

use std::collections::BTreeMap;
use std::path::Path;

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use apb_core::connector::inbox::{Inbox, InboxEvent};
use apb_core::scope::{Origin, PlaybookRef, Scope};
use apb_core::start_key::{REQUEST_V, StartKey, StartRequestFields, label};
use apb_core::trigger::{
    Approval, RenderError, Rule, RuleEntry, RuleScope, RuleSource, parse as parse_template,
    render as render_template, rule_digest,
};
use apb_engine::RunMode;
use apb_engine::error::EngineError;
use apb_engine::gate;
use apb_engine::gate::billing::{
    BILLING_ASK, BILLING_DENIED, BILLING_ENGINE, BILLING_STEP, BILLING_TRIGGER,
};
use apb_engine::gate::launch::{
    self, ASK_POLICY, BINDING_CHANGED, DENY_POLICY, ENGINE_POLICY, Initiator, LaunchContext,
    LaunchKind, STEP_POLICY, TRIGGER_POLICY,
};
use apb_engine::run_config::CacheRunMode;
use apb_engine::trigger::{FireOutcome, FireRequest, RuleSweep};

use super::ToolError;
use super::run::{KeyAnswer, StartArgs};

/// The refusal code of a dry test from inside a run step (D0 "A person's
/// command" item 3): its checks would read the environment as the step's
/// process, which refuses or flags what the listener would not.
pub const TRIGGER_TEST_REFUSED: &str = "trigger_test_refused_in_run_step";

/// The refusal when the rule's target needs a person's launch and so can
/// never be a rule's target.
pub const TRIGGER_LAUNCH_DENIED: &str = "trigger_launch_denied";

/// The refusal when what a person is about to approve differs from the
/// rule as it stands now.
pub const TRIGGER_APPROVAL_CHANGED: &str = "trigger_approval_changed";

/// The refusal when the rule's start would have no budget at all.
pub const TRIGGER_BUDGET_REQUIRED: &str = "trigger_budget_required";

/// The refusal when the installation key is missing, so the start-key
/// store cannot even be read without writing.
pub const TRIGGER_KEY_UNAVAILABLE: &str = "trigger_key_unavailable";

// --- the report of a dry test (D1 "`apb trigger test`") ---

/// Which check of the fire a [`CheckLine`] is about, in the order the fire
/// decides them.
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CheckId {
    Approved,
    RenderParams,
    StartKey,
    Unattended,
    Execution,
    RunGate,
    Binding,
    Launch,
    Consent,
    ParamPatterns,
    Budget,
    Notify,
}

impl CheckId {
    /// The order: whether the rule is approved for its current rule digest
    /// (`approved`, the test's own line), then the fire's steps: render (3),
    /// start key (4), unattended (5), execution (6), then `check_run` (8)
    /// split in three by where its refusal comes from (`run_gate` before the
    /// binding comparison, `binding` for the comparison in `Gate::admit`,
    /// `launch` for `launch::decide`); then consent (9), param patterns
    /// (10), budget (11), notify (12).
    pub const ORDER: [CheckId; 12] = [
        CheckId::Approved,
        CheckId::RenderParams,
        CheckId::StartKey,
        CheckId::Unattended,
        CheckId::Execution,
        CheckId::RunGate,
        CheckId::Binding,
        CheckId::Launch,
        CheckId::Consent,
        CheckId::ParamPatterns,
        CheckId::Budget,
        CheckId::Notify,
    ];
}

/// How one check came out.
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum CheckState {
    Passed,
    /// The code the real start would record (`binding_changed` for a changed
    /// binding, `param_missing` for a param that did not render).
    Refused {
        code: String,
    },
    /// An earlier check refused, so this one was not run, as in a real
    /// start, which stops at its first refusal.
    NotRun,
}

/// One check of the fire, as the report holds it; the state's tag is the
/// wire's `state` field, its code beside it.
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Clone, Serialize)]
pub struct CheckLine {
    pub id: CheckId,
    #[serde(flatten)]
    pub state: CheckState,
}

/// The answer of `apb trigger test`, MCP `trigger_test` and the HTTP test
/// route. Every field is typed; values appear only where `show_values` is
/// true (the CLI at a terminal outside a run step).
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Clone, Serialize)]
pub struct TestReport {
    pub rule: String,
    pub seq: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub event_type: Option<String>,
    pub filter: FilterVerdict,
    pub conditions: Vec<apb_core::connector::event_filter::ConditionResult>,
    /// One per param of the rule, in name order.
    pub params: Vec<ParamCheck>,
    pub key: KeyReport,
    /// From the no-write peek; `None` when the key did not render.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub key_use: Option<KeyUse>,
    /// Exactly one line per [`CheckId`], in [`CheckId::ORDER`].
    pub checks: Vec<CheckLine>,
    /// Always `["probation", "readiness", "concurrency", "working_tree"]`,
    /// shown as "checked at start"; never run by a test.
    #[cfg_attr(feature = "ts", ts(type = "string[]"))]
    pub checked_at_start: [&'static str; 4],
}

/// How the rule's filter and checks judged the event.
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "verdict", rename_all = "snake_case")]
pub enum FilterVerdict {
    Matched,
    /// A code of the journal's closed set (D1 "Evaluating one event") and
    /// the rule's own path or param name it is about.
    Skipped {
        code: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        #[cfg_attr(feature = "ts", ts(optional))]
        name: Option<String>,
    },
}

/// One param of the rule, rendered against the stored event.
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Clone, Serialize)]
pub struct ParamCheck {
    pub name: String,
    /// The rendered value, `show_values` only.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub value: Option<String>,
    /// Characters of the rendered value; `None` when it did not render.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub chars: Option<usize>,
    /// The param's `pattern` against the rendered value; `None` without a
    /// pattern or a rendered value.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub pattern_ok: Option<bool>,
    /// Why it did not render; `None` when it rendered.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub failure: Option<RenderFailure>,
}

/// The rule's start key, rendered against the stored event.
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Clone, Serialize)]
pub struct KeyReport {
    /// `start_key::key_tag_if_keyed` of the prefixed key; never the key.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub tag: Option<String>,
    /// The rendered key, `show_values` only.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub value: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub failure: Option<RenderFailure>,
}

/// `render::RenderError` as a report shows it: the rule's path, never a
/// body value.
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum RenderFailure {
    Missing {
        path: String,
    },
    NotScalar {
        path: String,
    },
    TooLong,
    /// The template itself does not parse; never rendered.
    Invalid,
}

/// What the no-write peek of the start key answered.
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum KeyUse {
    Free,
    Used { run_id: String, conflict: bool },
    Busy,
}

// --- what a person approves (D1 "Approval") ---

/// What a person approves for a rule: shown by every surface, posted back
/// unchanged by the dashboard, compared field by field at `approve`.
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ApprovalNeed {
    pub rule: String,
    /// `rule_digest` of the entry (D1 "Where rules live" item 1): every
    /// field that changes whether or how the rule fires, and its
    /// incarnation.
    pub rule_digest: String,
    pub playbook: String,
    pub version: String,
    pub playbook_digest: String,
    /// `permit.permit_digest()` of the gate's `RunPermit` for the target.
    pub permit_digest: String,
    pub launch_policy: String,
    pub launch_sources: Vec<String>,
    /// The full billing decision for the tree's executors, as
    /// `billing.binding_lines()` (D0 "Who starts the run" item 5).
    pub billing: Vec<String>,
    /// `approval_binding` of the three above: what `approve` stores and
    /// admission compares.
    pub binding: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub irreversible_sources: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub consent_nonce: Option<String>,
}

// --- helpers ---

fn origin_of(scope: RuleScope) -> Origin {
    match scope {
        RuleScope::Project => Origin::Project { workspace_id: None },
        RuleScope::Global => Origin::Global,
    }
}

fn wref_of(rule: &Rule) -> PlaybookRef {
    PlaybookRef {
        origin: origin_of(rule.playbook.scope),
        id: rule.playbook.id.clone(),
        version: Some(rule.playbook.version.clone()),
    }
}

fn scope_label_of(scope: RuleScope) -> &'static str {
    match scope {
        RuleScope::Project => "project",
        RuleScope::Global => "global",
    }
}

fn inbox_of(rule: &Rule) -> Option<&apb_core::trigger::InboxSource> {
    match &rule.source {
        RuleSource::Inbox(inbox) => Some(inbox),
    }
}

/// The `TriggeredBy` of a rule entry: the rule's name and incarnation, the
/// inbox source when there is one.
fn triggered_by_of(entry: &RuleEntry) -> apb_core::launch::TriggeredBy {
    let (connector, account) = match inbox_of(&entry.rule) {
        Some(inbox) => (Some(inbox.connector.clone()), Some(inbox.account.clone())),
        None => (None, None),
    };
    apb_core::launch::TriggeredBy {
        rule: entry.rule.name.clone(),
        incarnation: entry.incarnation.clone(),
        source: "inbox".to_string(),
        connector,
        account,
        event_seq: None,
    }
}

/// The `TriggeredBy` of an entry for one event.
fn triggered_by_with_event(entry: &RuleEntry, seq: u64) -> apb_core::launch::TriggeredBy {
    let mut by = triggered_by_of(entry);
    by.event_seq = Some(seq);
    by
}

fn passed(id: CheckId) -> CheckLine {
    CheckLine {
        id,
        state: CheckState::Passed,
    }
}

fn refused_line(id: CheckId, code: &str) -> CheckLine {
    CheckLine {
        id,
        state: CheckState::Refused {
            code: code.to_string(),
        },
    }
}

/// The `policy` of a structured refusal, as the fire's code records it.
fn policy_of(refusal: &Value) -> String {
    refusal["policy"].as_str().unwrap_or("refused").to_string()
}

/// The first param a `param_pattern_mismatch` refusal names (a name from
/// the rule, never a value), `None` for any other refusal.
fn pattern_param_of(refusal: &Value) -> Option<String> {
    if policy_of(refusal) != "param_pattern_mismatch" {
        return None;
    }
    refusal["invalid"]
        .as_array()
        .and_then(|v| v.first())
        .and_then(|v| v.as_str())
        .map(str::to_string)
}

/// The skip code and the param's name a render error maps to.
fn render_code_and_name(error: &RenderError, name: &str) -> (&'static str, Option<String>) {
    match error {
        RenderError::Missing(_) => ("param_missing", Some(name.to_string())),
        RenderError::NotScalar(_) => ("param_not_scalar", Some(name.to_string())),
        RenderError::TooLong => ("param_too_long", Some(name.to_string())),
    }
}

/// `render::RenderError` as the report shows it: the rule's path, never a
/// body value.
fn render_failure(error: RenderError) -> RenderFailure {
    match error {
        RenderError::Missing(path) => RenderFailure::Missing { path },
        RenderError::NotScalar(path) => RenderFailure::NotScalar { path },
        RenderError::TooLong => RenderFailure::TooLong,
    }
}

/// Whether a refusal of `check_run` names a launch decision (`deny`, the
/// ask confirmation, the rule approval, a billing decision) rather than a
/// run-gate check or the binding comparison.
fn is_launch_decision(code: &str) -> bool {
    matches!(
        code,
        ASK_POLICY
            | DENY_POLICY
            | TRIGGER_POLICY
            | STEP_POLICY
            | ENGINE_POLICY
            | BILLING_ASK
            | BILLING_DENIED
            | BILLING_STEP
            | BILLING_ENGINE
            | BILLING_TRIGGER
    )
}

/// `EngineError::Invalid` or `Conflict` text as its `<code>` prefix, when
/// it carries one.
fn code_prefix(text: &str) -> Option<&str> {
    text.split_once(": ").map(|(code, _)| code)
}

/// A refusal value as the fire's `Refused` outcome, its `policy` as the
/// code and `name` the first param of a pattern refusal. The detail
/// sentence is dropped, never stored (D1 "The state and the trigger
/// journal").
fn refused_outcome(refusal: &Value, name: Option<String>) -> FireOutcome {
    FireOutcome::Refused {
        code: policy_of(refusal),
        name,
    }
}

/// The start-key request fields of a rule's start (the fire's step 4),
/// shared with the dry test's peek, so both bind the same request.
fn start_fields(
    rule: &Rule,
    params: &BTreeMap<String, String>,
    external_data: bool,
) -> Result<StartRequestFields, ()> {
    // The canonical JSON of the rule's stored budget, the conversion `apb
    // run` and MCP `playbook_run` make of their typed argument: absent
    // fields skipped, keys sorted, so the same rule hashes the same bytes
    // every time.
    let budget = rule
        .budget
        .as_ref()
        .map(serde_json::to_value)
        .transpose()
        .map_err(|_| ())?;
    Ok(StartRequestFields {
        v: REQUEST_V,
        version: Some(rule.playbook.version.clone()),
        params: params.clone(),
        execution: Some("cli".to_string()),
        supervision: label(&RunMode::Autonomous),
        cache: label(&CacheRunMode::Auto),
        budget,
        unattended: true,
        external_data,
        ..Default::default()
    })
}

// --- the fire (D1 "The fire", steps 1 to 13) ---

/// The barriers the claim-race test holds a fire at, keyed by the root so
/// a parallel test's fire cannot take another test's barrier. Test builds
/// only.
#[cfg(any(test, feature = "testing"))]
pub mod test_hooks {
    use std::path::{Path, PathBuf};
    use std::sync::{Arc, Barrier, Mutex};

    static AFTER_LOOKUP: Mutex<Vec<(PathBuf, Arc<Barrier>)>> = Mutex::new(Vec::new());

    /// Holds every fire under `root` right after its start key answered
    /// free, before its start attempt claims the key: the arrival pairs
    /// with the returned barrier once, the release with a second wait.
    pub fn hold_after_lookup(root: &Path) -> Arc<Barrier> {
        let b = Arc::new(Barrier::new(2));
        AFTER_LOOKUP
            .lock()
            .unwrap()
            .push((root.to_path_buf(), b.clone()));
        b
    }

    /// Removes the hook of `root`.
    pub fn clear(root: &Path) {
        AFTER_LOOKUP.lock().unwrap().retain(|(r, _)| r != root);
    }

    pub(crate) fn after_lookup(root: &Path) {
        // One fire: the hook goes with the fire it held, so a second fire
        // of the same pass runs free.
        let hit = {
            let mut hooks = AFTER_LOOKUP.lock().unwrap();
            hooks
                .iter()
                .position(|(r, _)| r == root)
                .map(|at| hooks.remove(at).1)
        };
        if let Some(b) = hit {
            b.wait();
            b.wait();
        }
    }
}

/// The fire of the sweep: steps 1 to 13 of design D1 "The fire", no trace.
pub fn fire(req: &FireRequest<'_>) -> FireOutcome {
    fire_traced(req, &mut FireTrace::default())
}

/// What a traced fire records as it goes: one [`CheckLine`] per step that
/// decided, and what the start key answered (the dry test's `key_use`,
/// from the fire's own lookup, never a second one).
#[derive(Debug, Default)]
pub struct FireTrace {
    pub checks: Vec<CheckLine>,
    pub key_use: Option<KeyUse>,
}

/// The fire with one [`CheckLine`] pushed as each step decides, in
/// [`CheckId::ORDER`] order. `fire(req)` is `fire_traced(req, &mut
/// FireTrace::default())`, so the sweep and a dry test run the same steps
/// in the same order; there is no second implementation. A refusal stops
/// the trace; the caller fills what is missing (`NotRun`).
pub fn fire_traced(req: &FireRequest<'_>, out: &mut FireTrace) -> FireOutcome {
    let trace = &mut out.checks;
    let key_use = &mut out.key_use;
    let entry = req.entry;
    let rule = &entry.rule;
    let pb = &rule.playbook;
    let root = req.root;
    let dry = req.dry_run;

    // 1. The target of the rule, at the version it pins.
    let wref = wref_of(rule);

    // 2. External data, effective and fixed for the run.
    let external_data =
        match apb_engine::external_data::for_surface_start(root, Some(rule.external_data), None) {
            Ok(flag) => flag,
            Err(refusal) => return refused_outcome(&refusal.body(), None),
        };

    // 3. Every param rendered from the event the pass just evaluated. A
    // render error is the skip code the journal records, the param's name
    // with it.
    let mut params = BTreeMap::new();
    for (name, template) in &rule.params {
        let rendered = match parse_template(template) {
            Ok(parts) => render_template(&parts, req.event),
            Err(_) => {
                // A saved rule's templates parse (V260 refuses the rest);
                // the evaluation maps a malformed one to `param_missing`.
                trace.push(refused_line(CheckId::RenderParams, "param_missing"));
                return FireOutcome::Refused {
                    code: "param_missing".to_string(),
                    name: Some(name.clone()),
                };
            }
        };
        match rendered {
            Ok(text) => {
                params.insert(name.clone(), text);
            }
            Err(e) => {
                let (code, name) = render_code_and_name(&e, name);
                trace.push(refused_line(CheckId::RenderParams, code));
                return FireOutcome::Refused {
                    code: code.to_string(),
                    name,
                };
            }
        }
    }
    trace.push(passed(CheckId::RenderParams));

    // 4. The start key: the request captured from the rule, then the
    // lookup, or the no-write peek for a dry run. A key bound to a started
    // run is `AlreadyStarted` (a `start_key_conflict` too: one run per
    // rendered key is the rule's contract), before any limit; a key whose
    // run another start is still preparing, and `start_key_busy`, hold the
    // event (`Busy`, I2): that run may yet never start.
    let fields = match start_fields(rule, &params, external_data) {
        Ok(fields) => fields,
        Err(()) => {
            return FireOutcome::Failed {
                code: "budget_unencodable".to_string(),
            };
        }
    };
    let scope_label = scope_label_of(pb.scope);
    let keyed = if dry {
        super::run::keyed_peek(root, scope_label, &pb.id, &req.start_key, &fields)
    } else {
        super::run::keyed_lookup_holding(root, scope_label, &pb.id, &req.start_key, &fields)
    };
    let skr = match keyed {
        Ok(KeyAnswer::Go(skr)) => {
            trace.push(passed(CheckId::StartKey));
            *key_use = Some(KeyUse::Free);
            // The claim-race test holds the fire here: the lookup said the
            // key is free, the start has not claimed it yet.
            #[cfg(any(test, feature = "testing"))]
            test_hooks::after_lookup(root);
            skr
        }
        Ok(KeyAnswer::Named { bound: false, .. }) => {
            trace.push(passed(CheckId::StartKey));
            *key_use = Some(KeyUse::Busy);
            return FireOutcome::Busy;
        }
        Ok(KeyAnswer::Named {
            run_id, conflict, ..
        }) => {
            trace.push(passed(CheckId::StartKey));
            *key_use = Some(KeyUse::Used {
                run_id: run_id.clone(),
                conflict,
            });
            return FireOutcome::AlreadyStarted { run_id, conflict };
        }
        Err(answer) => {
            let policy = answer["policy"].as_str().unwrap_or("refused").to_string();
            if policy == "start_key_busy" {
                trace.push(passed(CheckId::StartKey));
                *key_use = Some(KeyUse::Busy);
                return FireOutcome::Busy;
            }
            trace.push(refused_line(CheckId::StartKey, &policy));
            return FireOutcome::Refused {
                code: policy,
                name: None,
            };
        }
    };

    // The limits on new starts (the hourly cap, the pass's start bound),
    // asked only now that the key said this is a new start.
    if let Err(code) = (req.admit)() {
        return FireOutcome::Limited { code };
    }

    // 5. A rule's start is always unattended, checked by the run gate's
    // own check.
    let execution = apb_core::execution::ExecutionRequest {
        mode: Some(apb_core::execution::ExecutionMode::Cli),
        host_session: false,
        client: Some(format!("trigger:{}", rule.name)),
        inherited: false,
    };
    let unattended_start = apb_engine::gate::UnattendedStart {
        unattended: true,
        mode: RunMode::Autonomous,
        execution: &execution,
    };
    if let Err(refusal) = gate::check_unattended(root, &unattended_start) {
        let code = policy_of(&refusal);
        trace.push(refused_line(CheckId::Unattended, &code));
        return refused_outcome(&refusal, None);
    }
    trace.push(passed(CheckId::Unattended));

    // 6. The execution mode the run takes (a rule asks for `cli`).
    if apb_core::execution::resolve_for(root, &execution).is_err() {
        trace.push(refused_line(CheckId::Execution, "execution_unresolved"));
        return FireOutcome::Refused {
            code: "execution_unresolved".to_string(),
            name: None,
        };
    }
    trace.push(passed(CheckId::Execution));

    // 7. The launch context of a rule start: the approval travels only
    // while its rule digest is the entry's current one.
    let digest = rule_digest(entry);
    let approval = entry.approval.as_ref().filter(|a| a.rule_digest == digest);
    let by = Box::new(req.triggered_by.clone());
    let mut launch = LaunchContext::new(Initiator::Trigger { by }, LaunchKind::Start)
        .with_external_data(external_data);
    if let Some(binding) = approval.map(|a| a.binding.clone()) {
        launch = launch.with_approved_binding(binding);
    } else if dry {
        // An unapproved rule's dry fire runs with the binding the gate
        // resolves now (as `approval_need` does), so the other lines say
        // what a start would meet once it is approved. The first pass
        // resolves the binding; it decides nothing. A refusal before the
        // binding was even resolved (a draft target, a hold, trust) is the
        // real answer.
        let bare = LaunchContext {
            approved_binding: None,
            ..launch.clone()
        };
        match gate::check_run(root, &wref, false, false, &bare) {
            Err(v) if v["policy"].as_str() == Some(TRIGGER_POLICY) => match v["binding"].as_str() {
                Some(binding) => launch = launch.with_approved_binding(binding.to_string()),
                None => {
                    trace.push(refused_line(CheckId::Launch, TRIGGER_POLICY));
                    return FireOutcome::Refused {
                        code: TRIGGER_POLICY.to_string(),
                        name: None,
                    };
                }
            },
            Err(v) => {
                let code = policy_of(&v);
                if is_launch_decision(&code) {
                    trace.push(passed(CheckId::RunGate));
                    trace.push(passed(CheckId::Binding));
                    trace.push(refused_line(CheckId::Launch, &code));
                } else {
                    trace.push(refused_line(CheckId::RunGate, &code));
                }
                return refused_outcome(&v, None);
            }
            // Impossible for a `Trigger` initiator without a binding.
            Ok(_) => {
                return FireOutcome::Refused {
                    code: TRIGGER_POLICY.to_string(),
                    name: None,
                };
            }
        }
    }

    // 8. The run gate (never `acknowledge_untrusted`: a rule is no trust
    // approval). Its refusal splits by where it comes from: before the
    // binding comparison (`run_gate`), the comparison itself (`binding`),
    // the launch decision (`launch`). The permit is handed to the engine
    // verbatim as the expected pins of step 13 (the anti-TOCTOU hand-over),
    // so a profile that changes after admission fails preparation's own
    // bundle check and starts nothing.
    let permit = match gate::check_run(root, &wref, false, false, &launch) {
        Ok(p) => {
            trace.push(passed(CheckId::RunGate));
            trace.push(passed(CheckId::Binding));
            trace.push(passed(CheckId::Launch));
            p
        }
        Err(v) => {
            if let Some(binding) = launch::binding_changed_of(&v) {
                trace.push(passed(CheckId::RunGate));
                trace.push(refused_line(CheckId::Binding, BINDING_CHANGED));
                return FireOutcome::BindingChanged { binding };
            }
            let code = policy_of(&v);
            if is_launch_decision(&code) {
                trace.push(passed(CheckId::RunGate));
                trace.push(passed(CheckId::Binding));
                trace.push(refused_line(CheckId::Launch, &code));
            } else {
                trace.push(refused_line(CheckId::RunGate, &code));
            }
            return refused_outcome(&v, None);
        }
    };
    let version = permit.run_version(Some(&pb.version));
    let pinned = PlaybookRef {
        version,
        ..wref.clone()
    };

    // 9. The consent: the approval's nonce is the confirmation of the
    // exact irreversible tree, and the consent names what gave it.
    let consent = if permit.needs_consent() {
        let confirmation = approval
            .and_then(|a| a.consent_nonce.as_deref())
            .map(|n| apb_engine::consent::Confirmation::Nonce(n.to_string()));
        if let Err(refusal) = permit.check_confirmation(confirmation.as_ref()) {
            let code = policy_of(&refusal);
            trace.push(refused_line(CheckId::Consent, &code));
            return refused_outcome(&refusal, None);
        }
        Some(apb_engine::consent::RunConsent::irreversible(format!(
            "rule_approval:{}",
            rule.name
        )))
    } else {
        None
    };
    trace.push(passed(CheckId::Consent));

    // 10. The declared param patterns against the rendered values.
    if let Err(refusal) = apb_engine::gate::check_params_with(root, &pinned, &params, external_data)
    {
        let code = policy_of(&refusal);
        let name = pattern_param_of(&refusal);
        trace.push(refused_line(CheckId::ParamPatterns, &code));
        return refused_outcome(&refusal, name);
    }
    trace.push(passed(CheckId::ParamPatterns));

    // 11. The budget: a rule starts unattended, so an effective budget (a
    // limit and what to do at a node with no estimate) must exist, and the
    // pre-start check must not need a person's decision.
    let resolved_def = match apb_core::store::resolve(root, &pinned) {
        Ok(r) => r,
        Err(_) => {
            return FireOutcome::Failed {
                code: "start_failed".to_string(),
            };
        }
    };
    if !has_effective_budget(&resolved_def, rule.budget.as_ref()) {
        trace.push(refused_line(CheckId::Budget, TRIGGER_BUDGET_REQUIRED));
        return FireOutcome::Refused {
            code: TRIGGER_BUDGET_REQUIRED.to_string(),
            name: None,
        };
    }
    let checked = match super::budget::preflight(&resolved_def, "cli", rule.budget.as_ref()) {
        Ok(p) => p,
        Err(refusal) => {
            let code = policy_of(&refusal);
            trace.push(refused_line(CheckId::Budget, &code));
            return refused_outcome(&refusal, None);
        }
    };
    trace.push(passed(CheckId::Budget));

    // 12. The notify request: the rule's flag through the shared check.
    let notify = match build_notify(root, &pinned, rule.notify, trace) {
        Ok(notify) => notify,
        Err(outcome) => return outcome,
    };

    // 13. A dry run stops here: preparation's own checks (probation,
    // readiness, the concurrency key, the working tree) did not run.
    if dry {
        trace.push(passed(CheckId::Notify));
        return FireOutcome::PreStartPassed;
    }

    // 13. The start, with the permit handed over verbatim as the expected
    // pins. A busy tree waits for the dashboard's queue, not the event's
    // verdict (`workdir_queue_wait`); a rule's start needs readiness as its
    // rule says and earns no probation waiver.
    let start = StartArgs {
        launch: permit.launch.clone(),
        params,
        instruction: None,
        expected_digest: Some(permit.playbook_digest.clone()),
        expected_profile_bundles: Some(permit.profile_bundles.clone()),
        expected_children: Some(permit.children.clone()),
        expected_connectors: permit.connectors.clone(),
        expected_connector_accounts: permit.connector_accounts.clone(),
        continued_from: None,
        worktree: None,
        execution: execution.clone(),
        consent,
        candidate_skipped: None,
        budget: rule.budget.clone(),
        estimate: checked.estimate,
        notify,
        unattended: true,
        external_data,
        deadline_ms: None,
        start_key: Some(skr),
    };
    let mut opts = super::run::start_options(start, RunMode::Autonomous);
    opts.require_ready = rule.require_ready;
    opts.waive_probation = false;
    opts.workdir_queue_wait = apb_core::config::GlobalConfig::load()
        .unwrap_or_default()
        .server
        .workdir_queue_wait();
    let started = if matches!(pb.scope, RuleScope::Global) {
        apb_engine::start_detached_resolved(&resolved_def, opts)
    } else {
        apb_engine::start_detached(root, &pb.id, Some(&pb.version), opts)
    };
    match started {
        Ok(run_id) => FireOutcome::Started { run_id },
        // The claim answers a run another start is still preparing as
        // started (a person's answer): judged again, bound or held.
        Err(EngineError::AlreadyStarted(_)) => {
            claim_answer(root, scope_label, &pb.id, req, &fields)
        }
        Err(EngineError::WorkdirBusy(_)) => FireOutcome::Refused {
            code: "workdir_busy".to_string(),
            name: None,
        },
        Err(EngineError::Invalid(text)) | Err(EngineError::Conflict(text)) => {
            match code_prefix(&text) {
                Some("start_key_busy") => FireOutcome::Busy,
                Some("start_key_conflict") => claim_answer(root, scope_label, &pb.id, req, &fields),
                Some(code) => FireOutcome::Refused {
                    code: code.to_string(),
                    name: None,
                },
                None => FireOutcome::Failed {
                    code: "start_failed".to_string(),
                },
            }
        }
        Err(_) => FireOutcome::Failed {
            code: "start_failed".to_string(),
        },
    }
}

/// The answer of a claim that found the key taken after the lookup said it
/// was free (another start claimed it in between): the key is looked up
/// again and only a run bound to it answers `AlreadyStarted`; one still
/// being prepared, or an answer that does not read, holds the event
/// (`Busy`), and the next pass decides again.
fn claim_answer(
    root: &Path,
    scope_label: &str,
    id: &str,
    req: &FireRequest<'_>,
    fields: &StartRequestFields,
) -> FireOutcome {
    match super::run::keyed_lookup_holding(root, scope_label, id, &req.start_key, fields) {
        Ok(KeyAnswer::Named {
            run_id,
            conflict,
            bound: true,
        }) => FireOutcome::AlreadyStarted { run_id, conflict },
        _ => FireOutcome::Busy,
    }
}

/// Whether the run would start with an effective budget: the rule's
/// argument over the target's `budget:` block, with a limit. The playbook
/// is read again here (the gate verified it moments ago); a target that
/// cannot be read has no budget, the safe direction for an unattended
/// start.
fn has_effective_budget(
    resolved: &apb_core::store::ResolvedPlaybook,
    req: Option<&apb_core::budget::BudgetRequest>,
) -> bool {
    let block = apb_core::registry::Registry::open_dir(&resolved.definition_parent)
        .ok()
        .and_then(|reg| reg.load(&resolved.id, Some(&resolved.version)).ok())
        .and_then(|loaded| loaded.playbook.budget);
    apb_core::budget::effective(block.as_ref(), req).is_some()
}

/// The notify request of the rule's flag, through the shared start check.
fn build_notify(
    root: &Path,
    run_ref: &PlaybookRef,
    notify: Option<bool>,
    trace: &mut Vec<CheckLine>,
) -> Result<Option<apb_core::notify::NotifyRequest>, FireOutcome> {
    let arg = notify.map(super::notify::NotifyArg::Flag);
    let request = match super::notify::NotifyArg::request_of(arg.as_ref()) {
        Ok(req) => req,
        Err(e) => {
            let refusal = e.body();
            let code = policy_of(&refusal);
            trace.push(refused_line(CheckId::Notify, &code));
            return Err(refused_outcome(&refusal, None));
        }
    };
    let key = apb_core::policy_file::PlaybookKey::of(run_ref);
    match super::notify::start_check(root, Some(&key), request.as_ref()) {
        Ok(_) => Ok(request),
        Err(e) => {
            let refusal = e.body();
            let code = policy_of(&refusal);
            trace.push(refused_line(CheckId::Notify, &code));
            Err(refused_outcome(&refusal, None))
        }
    }
}

// --- what a person approves (D1 "Approval") ---

/// What a person would approve for the rule as it stands now: the gate's
/// answer for the rule without an approval (the binding, the permit digest,
/// the launch policy and the billing decision), then the gate's answer with
/// that binding (the target as a rule's start). A `deny` target refuses the
/// approval outright; a billing `ask` the rule cannot confirm refuses it
/// too; any other refusal comes back as it is.
pub fn approval_need(root: &Path, entry: &RuleEntry) -> Result<ApprovalNeed, Value> {
    let rule = &entry.rule;
    let pb = &rule.playbook;
    let wref = wref_of(rule);
    let by = Box::new(triggered_by_of(entry));
    let base = LaunchContext::new(Initiator::Trigger { by }, LaunchKind::Start)
        .with_external_data(rule.external_data);

    // Without an approved binding every launch policy refuses in
    // `Gate::admit` with what an approval binds.
    let need = match gate::check_run(root, &wref, false, false, &base) {
        Err(v) if v["policy"].as_str() == Some(TRIGGER_POLICY) => v,
        // Reached only before `admit` (a run-gate refusal): as it is.
        Err(v) => return Err(v),
        Ok(_) => {
            // Impossible for a `Trigger` initiator without a binding; there
            // would be nothing to bind.
            return Err(json!({
                "policy": TRIGGER_POLICY,
                "detail": format!("playbook `{}` gave no binding to approve; check the rule's target", pb.id),
            }));
        }
    };
    let binding = need["binding"]
        .as_str()
        .ok_or_else(
            || json!({ "policy": TRIGGER_POLICY, "detail": "the refusal names no binding" }),
        )?
        .to_string();
    let permit_digest = need["permit_digest"]
        .as_str()
        .unwrap_or_default()
        .to_string();
    let launch_policy = need["launch_policy"]
        .as_str()
        .unwrap_or_default()
        .to_string();
    let billing: Vec<String> = need["billing"]
        .as_array()
        .map(|lines| {
            lines
                .iter()
                .filter_map(|line| line.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default();

    // With that binding the gate admits and decides: what the rule's start
    // would meet once approved.
    let bound = base.with_approved_binding(binding.clone());
    let permit = match gate::check_run(root, &wref, false, false, &bound) {
        Ok(p) => p,
        Err(v) if v["policy"].as_str() == Some(DENY_POLICY) => {
            return Err(json!({
                "policy": TRIGGER_LAUNCH_DENIED,
                "id": pb.id,
                "detail": format!("only a person starts playbook `{}`; a rule cannot start it", pb.id),
            }));
        }
        Err(v) => return Err(v),
    };
    Ok(ApprovalNeed {
        rule: rule.name.clone(),
        rule_digest: rule_digest(entry),
        playbook: pb.id.clone(),
        version: pb.version.clone(),
        playbook_digest: permit.playbook_digest.clone(),
        permit_digest,
        launch_policy,
        launch_sources: permit
            .launch
            .as_ref()
            .map(|g| g.record.sources.clone())
            .unwrap_or_default(),
        billing,
        binding,
        irreversible_sources: permit.irreversible.clone(),
        consent_nonce: permit.needs_consent().then(|| permit.consent_nonce()),
    })
}

/// Stores a person's approval of a rule: under the rule's locks, a fresh
/// need of the stored entry compared with `shown` field by field (the
/// digests and the consent nonce with `ct_eq_str`, every other field with
/// `==`); any difference refuses `trigger_approval_changed`. Then the core
/// stores it, the rule's state entry deleted before the rules write (I5).
pub fn approve(root: &Path, rule: &str, shown: &ApprovalNeed, by: &str) -> Result<Approval, Value> {
    let _lock = apb_core::trigger::hold_rule_lock(root, rule).map_err(rule_error_value)?;
    let entry = stored_entry(root, rule)?;
    let fresh = approval_need(root, &entry)?;
    let same = |a: &str, b: &str| apb_core::secret::ct_eq_str(a, b);
    let nonce_equal = match (&fresh.consent_nonce, &shown.consent_nonce) {
        (Some(a), Some(b)) => same(a, b),
        (None, None) => true,
        _ => false,
    };
    if fresh.rule != shown.rule
        || !same(&fresh.rule_digest, &shown.rule_digest)
        || !same(&fresh.playbook_digest, &shown.playbook_digest)
        || !same(&fresh.permit_digest, &shown.permit_digest)
        || !same(&fresh.binding, &shown.binding)
        || fresh.playbook != shown.playbook
        || fresh.version != shown.version
        || fresh.launch_policy != shown.launch_policy
        || fresh.launch_sources != shown.launch_sources
        || fresh.billing != shown.billing
        || fresh.irreversible_sources != shown.irreversible_sources
        || !nonce_equal
    {
        return Err(json!({
            "policy": TRIGGER_APPROVAL_CHANGED,
            "detail": format!("the rule or its target changed since the need was shown; nothing was approved; ask again (apb trigger approve {rule})"),
        }));
    }
    let now = apb_core::clock::Millis::now();
    let approval = Approval {
        rule_digest: fresh.rule_digest,
        playbook_digest: fresh.playbook_digest,
        binding: fresh.binding,
        permit_digest: fresh.permit_digest,
        launch_policy: fresh.launch_policy,
        consent_nonce: fresh.consent_nonce,
        approved_at: now.0,
        approved_by: by.to_string(),
    };
    apb_core::trigger::store_approval_locked(root, rule, approval.clone(), now, &_lock)
        .map_err(rule_error_value)?;
    Ok(approval)
}

/// The entry of `rule` in the signed rules file, only as this installation
/// signed it for this project.
fn stored_entry(root: &Path, rule: &str) -> Result<RuleEntry, Value> {
    let loaded =
        apb_core::signed_store::read::<apb_core::trigger::RulesFile>(root, Scope::Project, "rules")
            .map_err(rule_store_error_value)?;
    if !loaded.signed {
        return Err(rule_unsigned_value(root));
    }
    loaded
        .file
        .rules
        .into_iter()
        .find(|e| e.rule.name == rule)
        .ok_or_else(|| {
            json!({
                "policy": "trigger_rule_not_found",
                "detail": format!("no rule `{rule}`"),
            })
        })
}

/// A rule store error as a refusal value.
fn rule_error_value(e: apb_core::trigger::TriggerError) -> Value {
    let code = match &e {
        apb_core::trigger::TriggerError::Busy(_) => "trigger_rule_busy",
        apb_core::trigger::TriggerError::Unregistered => "trigger_project_unregistered",
        apb_core::trigger::TriggerError::EnabledNotSettable => "trigger_enabled_not_settable",
        apb_core::trigger::TriggerError::StoreFull(_) => "rules_store_full",
        apb_core::trigger::TriggerError::NotFound(_) => "trigger_rule_not_found",
        apb_core::trigger::TriggerError::Invalid(_) => "trigger_rule_invalid",
        apb_core::trigger::TriggerError::Store(_) | apb_core::trigger::TriggerError::Inbox(_) => {
            "trigger_rules_unreadable"
        }
    };
    json!({ "policy": code, "detail": e.to_string() })
}

fn rule_store_error_value(e: apb_core::signed_store::StoreError) -> Value {
    json!({ "policy": "trigger_rules_unreadable", "detail": e.to_string() })
}

fn rule_unsigned_value(root: &Path) -> Value {
    let path =
        apb_core::signed_store::file::<apb_core::trigger::RulesFile>(root, Scope::Project, "rules");
    json!({
        "policy": apb_core::trigger::RULES_UNVERIFIED,
        "detail": apb_core::trigger::rules_unverified_detail(&path),
    })
}

/// A refusal value as one line (`<policy>: <detail>`), for a `ToolError`.
fn refusal_text(v: &Value) -> String {
    format!(
        "{}: {}",
        v["policy"].as_str().unwrap_or("refused"),
        v["detail"].as_str().unwrap_or_default()
    )
}

// --- the dry test (D1 "`apb trigger test`") ---

/// One stored event through the rule, as `apb trigger test` and MCP
/// `trigger_test` answer it: the evaluation exactly as the sweep runs it,
/// then the dry fire (steps 1 to 12 of the real start, no preparation).
/// Nothing is written: no state, no journal line, no challenge, no run
/// directory, no lock file, no installation key; the start key is peeked,
/// never looked up. Works in a read-only process: nothing on its path
/// calls the write guard. `show_values` is true only for the CLI at a
/// terminal outside a run step.
pub fn test_rule(
    root: &Path,
    rule: &str,
    seq: Option<u64>,
    show_values: bool,
) -> Result<TestReport, ToolError> {
    // A run step's dry start would run `check_unattended` and
    // `for_surface_start` as the step's process, which refuse or flag what
    // the listener would not; refusing is simpler than evaluating as
    // another process would.
    let step = apb_core::step_env::StepCaller::from_env();
    if let Some(why) = step.refusal("testing a trigger rule") {
        return Err(ToolError::Conflict(format!(
            "{TRIGGER_TEST_REFUSED}: {why}"
        )));
    }
    // The signed rules file, read with no lock: the entry as approved (or
    // not), never as another installation signed it.
    let entry = stored_entry(root, rule).map_err(|v| ToolError::Conflict(refusal_text(&v)))?;
    let digest = rule_digest(&entry);
    let approved = entry
        .approval
        .as_ref()
        .is_some_and(|a| a.rule_digest == digest);

    let Some(source) = inbox_of(&entry.rule).cloned() else {
        return Err(ToolError::Engine(
            "the rule has no source a stored event could come from".to_string(),
        ));
    };
    // The stored event: the newest when no seq is named.
    let event = stored_event(&source, seq)?;
    let incarnation = entry.incarnation.clone();

    // The evaluation exactly as the sweep runs it, through the sweep's own
    // own-run predicate.
    let own_run = |run: &str| apb_engine::trigger::own_run(root, run);
    let (verdict, conditions) =
        apb_core::trigger::evaluate_event(&entry.rule, &source, &event, &incarnation, &own_run);
    let (filter, start_key) = match verdict {
        apb_core::trigger::EventVerdict::Matched { start_key } => {
            (FilterVerdict::Matched, Some(start_key))
        }
        apb_core::trigger::EventVerdict::Skipped { code, name } => (
            FilterVerdict::Skipped {
                code: code.to_string(),
                name,
            },
            None,
        ),
    };

    // The params and the key, rendered here for the report; the fire
    // renders them again from the same event. A failure's path names the
    // rule's own path, never a body value.
    let declared = declared_params(root, &entry);
    let mut params = Vec::new();
    let mut params_ok = true;
    for (name, template) in &entry.rule.params {
        let rendered = parse_template(template)
            .map_err(|_| RenderFailure::Invalid)
            .and_then(|parts| render_template(&parts, &event).map_err(render_failure));
        let mut failure = None;
        let mut text = None;
        match rendered {
            Ok(rendered) => text = Some(rendered),
            Err(f) => {
                params_ok = false;
                failure = Some(f);
            }
        }
        // Characters always when it rendered; the value only with
        // `show_values`.
        let chars = text.as_ref().map(|t| t.chars().count());
        let value = if show_values { text.clone() } else { None };
        // The declared param's pattern against the rendered value; `None`
        // without a pattern or a rendered value.
        let pattern_ok = text.as_ref().and_then(|t| {
            declared
                .get(name)
                .map(|p| apb_core::param_pattern::check(p, t).is_ok())
        });
        params.push(ParamCheck {
            name: name.clone(),
            value,
            chars,
            pattern_ok,
            failure,
        });
    }

    let key_rendered = parse_template(&source.key)
        .map_err(|_| RenderFailure::Invalid)
        .and_then(|parts| render_template(&parts, &event).map_err(render_failure));
    let key_report = match &key_rendered {
        Ok(text) => KeyReport {
            tag: StartKey::parse(&format!("trigger:{incarnation}:{text}"))
                .ok()
                .and_then(|k| apb_core::start_key::key_tag_if_keyed(&k)),
            value: show_values.then(|| text.clone()),
            failure: None,
        },
        Err(failure) => KeyReport {
            tag: None,
            value: None,
            failure: Some(failure.clone()),
        },
    };
    let key_ok = key_report.failure.is_none();

    // The checks: the test's own `approved` line, then the dry fire for a
    // match whose params and key rendered (what the sweep would run), the
    // single refusal a skip maps to otherwise.
    let mut checks = vec![CheckLine {
        id: CheckId::Approved,
        state: if approved {
            CheckState::Passed
        } else {
            CheckState::Refused {
                code: "rule_not_approved".to_string(),
            }
        },
    }];
    let mut key_use = None;
    let matched = start_key.is_some();
    if matched && params_ok && key_ok {
        let mut trace = FireTrace::default();
        let req = FireRequest {
            root,
            entry: &entry,
            source: &source,
            event: &event,
            start_key: start_key
                .as_ref()
                .map(|k| k.expose().to_string())
                .unwrap_or_default(),
            triggered_by: triggered_by_with_event(&entry, event.seq),
            dry_run: true,
            // A dry test starts nothing, so no limit on new starts holds it.
            admit: &|| Ok(()),
        };
        let _ = fire_traced(&req, &mut trace);
        checks.extend(trace.checks);
        // What the fire's own no-write peek of the key answered.
        key_use = trace.key_use;
    } else if !params_ok {
        // A param that cannot render: the fire's step 3 refuses with the
        // param's code; every later check is not run.
        let code = params
            .iter()
            .zip(entry.rule.params.iter())
            .find(|(p, _)| p.failure.is_some())
            .and_then(|(p, (_, template))| {
                parse_template(template)
                    .err()
                    .map(|_| "param_missing")
                    .or_else(|| {
                        parse_template(template)
                            .ok()
                            .and_then(|parts| render_template(&parts, &event).err())
                            .map(|e| render_code_and_name(&e, &p.name).0)
                    })
            })
            .unwrap_or("param_missing");
        checks.push(refused_line(CheckId::RenderParams, code));
    } else if !key_ok {
        // A key that cannot render: params may have rendered; the key
        // refuses with the code the evaluation recorded.
        if params_ok {
            checks.push(passed(CheckId::RenderParams));
        }
        let code = match &filter {
            FilterVerdict::Skipped { code, .. } if code.starts_with("key_") => code.clone(),
            _ => "key_invalid".to_string(),
        };
        checks.push(refused_line(CheckId::StartKey, &code));
    }

    // Every CheckId of ORDER exactly once: the trace stopped at its first
    // refusal, the rest were not run.
    let mut full: Vec<CheckLine> = Vec::with_capacity(CheckId::ORDER.len());
    for id in CheckId::ORDER {
        match checks.iter().position(|line| line.id == id) {
            Some(at) => full.push(checks.swap_remove(at)),
            None => full.push(CheckLine {
                id,
                state: CheckState::NotRun,
            }),
        }
    }

    Ok(TestReport {
        rule: rule.to_string(),
        seq: event.seq,
        event_type: event.event_type.clone(),
        filter,
        conditions,
        params,
        key: key_report,
        key_use,
        checks: full,
        checked_at_start: ["probation", "readiness", "concurrency", "working_tree"],
    })
}

/// The playbook's declared params of the rule's target, for the pattern
/// checks of the report; empty when the target does not resolve (the list
/// shows that separately).
fn declared_params(root: &Path, entry: &RuleEntry) -> BTreeMap<String, apb_core::schema::Param> {
    let wref = wref_of(&entry.rule);
    let Ok(resolved) = apb_core::store::resolve(root, &wref) else {
        return BTreeMap::new();
    };
    let Ok(loaded) = apb_core::registry::Registry::open_dir(&resolved.definition_parent)
        .and_then(|reg| reg.load(&resolved.id, Some(&resolved.version)))
    else {
        return BTreeMap::new();
    };
    loaded
        .playbook
        .params
        .into_iter()
        .map(|p| (p.name.clone(), p))
        .collect()
}

/// The stored event of the source: the newest, or the one seq names.
fn stored_event(
    source: &apb_core::trigger::InboxSource,
    seq: Option<u64>,
) -> Result<InboxEvent, ToolError> {
    let inbox = Inbox::open(&source.connector, &source.account)
        .map_err(|e| ToolError::Engine(format!("the rule's source cannot be opened: {e}")))?;
    let events = inbox
        .read_events()
        .map_err(|e| ToolError::Engine(format!("the rule's source cannot be read: {e}")))?;
    let found = match seq {
        None => events.last().cloned(),
        Some(want) => events.iter().find(|e| e.seq == want).cloned(),
    };
    found.ok_or_else(|| {
        ToolError::NotFound(match seq {
            None => "the source holds no event; the report says the event is gone".to_string(),
            Some(seq) => format!("the event {seq} is gone from the source's history"),
        })
    })
}

// --- the list and the sweep host ---

/// The rules of the project as the list shows them, `needs_approval`
/// filled: a rule whose approval is missing, names another digest or whose
/// binding differs needs one, and so does a rule whose need cannot even be
/// computed (the target does not resolve).
pub fn list(root: &Path) -> Result<Vec<apb_engine::trigger::view::RuleView>, ToolError> {
    let mut views = apb_engine::trigger::view::list(root).map_err(ToolError::from)?;
    if views.is_empty() {
        return Ok(views);
    }
    let loaded =
        apb_core::signed_store::read::<apb_core::trigger::RulesFile>(root, Scope::Project, "rules")
            .map_err(ToolError::from)?;
    if !loaded.signed {
        // `view::list` already refused an unsigned file; this cannot hold.
        return Ok(views);
    }
    for view in &mut views {
        let Some(entry) = loaded.file.rules.iter().find(|e| e.rule.name == view.name) else {
            continue;
        };
        view.needs_approval = match approval_need(root, entry) {
            Ok(need) => match entry
                .approval
                .as_ref()
                .filter(|a| a.rule_digest == rule_digest(entry))
            {
                Some(a) => a.binding != need.binding,
                None => true,
            },
            Err(_) => true,
        };
    }
    Ok(views)
}

/// One pass of the sweep over the project at `root`, the host entry the
/// listener and `apb resume --all` share. A run step sweeps nothing: the
/// pass would run its checks and start its runs as the step's process.
pub fn sweep_root(root: &Path) -> Vec<(String, Result<RuleSweep, EngineError>)> {
    let step = apb_core::step_env::StepCaller::from_env();
    if let Some(why) = step.refusal("the rule sweep") {
        return vec![(
            String::from("*"),
            Err(EngineError::Invalid(format!("rule sweep skipped: {why}"))),
        )];
    }
    apb_engine::trigger::sweep(root, &apb_core::clock::Millis::now, &fire)
}
