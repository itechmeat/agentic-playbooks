//! Next actions as MCP tool calls: each [`NextAction`] of the engine mapped
//! to the tool that executes it, the arguments the action carries, the ones
//! the caller fills in, and whether this caller may take it.
//!
//! The engine says whom the run accepts an action from (`principals`); the
//! caller's own limits are this layer's: a supervisor session acts only on
//! what the run accepts from its supervisor, and a host relays to its
//! person the actions only a person takes. The tool that executes an action
//! checks everything again, so an action shown as available can still be
//! refused.

use apb_engine::decision_channel::DecisionChannel;
use apb_engine::next_action::start::{StartAction, StartOf};
use apb_engine::next_action::{
    ActionArg, ActionCode, ArgValue, CallerRefusal, NextAction, Principal, RunActions,
};
use serde_json::{Map, Value, json};

/// Who reads the actions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActionCaller {
    /// A host agent session: it takes what the run accepts from a host and
    /// relays to its person what only a person (or an operator) takes.
    Host,
    /// A supervisor session (its token): only the supervisor's actions.
    Supervisor,
    /// A person at a terminal (`apb runs <id> --json`).
    Person,
    /// A host session on a read-only server (`apb mcp --read-only`): it
    /// takes nothing that changes a run, whoever the run accepts it from.
    ReadOnly,
}

/// Why a read-only server's caller may not take an action that changes a
/// run.
pub const READ_ONLY_REFUSAL: &str = "read_only_server";

/// Whether taking `code` changes the run (everything but waiting again and
/// reading the report).
fn mutates(code: ActionCode) -> bool {
    !matches!(code, ActionCode::WaitAgain | ActionCode::ReadReport)
}

impl ActionCaller {
    /// Whether this caller may take `code`, which the run accepts from
    /// `principals`, and if not, why.
    fn refusal(self, code: ActionCode, principals: &[Principal]) -> Option<CallerRefusal> {
        if self == ActionCaller::ReadOnly {
            return mutates(code).then_some(CallerRefusal::ReadOnlyServer);
        }
        let has = |p: Principal| principals.contains(&p);
        let ok = match self {
            ActionCaller::Host => {
                has(Principal::Host) || has(Principal::Person) || has(Principal::Operator)
            }
            ActionCaller::Supervisor => has(Principal::Supervisor),
            ActionCaller::Person => has(Principal::Person) || has(Principal::Operator),
            ActionCaller::ReadOnly => true,
        };
        if ok {
            return None;
        }
        Some(match self {
            ActionCaller::Supervisor => CallerRefusal::RelayToPerson,
            ActionCaller::Host if has(Principal::Supervisor) => CallerRefusal::SupervisorOnly,
            _ => CallerRefusal::NotThisCaller,
        })
    }

    /// The channel this caller's decisions come through: this MCP server's
    /// ([`mcp_channel`]) for a session, the CLI's for a person
    /// at `apb runs <id> --json` (a run step's inside one, the terminal at
    /// one, else the CLI without a terminal).
    pub fn channel(self) -> DecisionChannel {
        use std::io::IsTerminal as _;
        match self {
            ActionCaller::Person => {
                if apb_core::step_env::StepCaller::from_env()
                    .describe()
                    .is_some()
                {
                    DecisionChannel::RunStep
                } else if std::io::stdin().is_terminal() && std::io::stderr().is_terminal() {
                    DecisionChannel::Terminal
                } else {
                    DecisionChannel::CliUnattended
                }
            }
            _ => mcp_channel(),
        }
    }

    /// Whether this caller passes the action on to its person rather than
    /// taking it on its own judgment.
    fn relays(self, principals: &[Principal]) -> bool {
        self == ActionCaller::Host && !principals.contains(&Principal::Host)
    }
}

/// The tool that executes `code`.
pub(crate) fn tool_of(code: ActionCode) -> &'static str {
    match code {
        ActionCode::AnswerQuestion => "run_answer",
        ActionCode::DecideReview | ActionCode::DecidePublish => "review_decide",
        ActionCode::SubmitHostTask => "run_task_submit",
        ActionCode::SupervisorRetryNode => "supervisor_node_retry",
        ActionCode::SupervisorContinueFrom => "supervisor_run_continue_from",
        ActionCode::SupervisorAbort => "supervisor_run_abort",
        ActionCode::ResumeRun | ActionCode::ResumeWithBudget | ActionCode::RetryNow => "run_resume",
        ActionCode::ResolveIntent => "run_intent_resolve",
        ActionCode::WaitAgain => "run_wait",
        ActionCode::ReadReport => "run_report",
    }
}

/// The tool `caller` takes `code` through: a supervisor session starts a
/// waiting retry with `supervisor_node_retry` (its sessions have no
/// `run_resume`), everyone else through [`tool_of`].
fn tool_for(code: ActionCode, caller: ActionCaller) -> &'static str {
    if code == ActionCode::RetryNow && caller == ActionCaller::Supervisor {
        "supervisor_node_retry"
    } else {
        tool_of(code)
    }
}

/// Where in the tool's arguments an action argument goes: its path of
/// keys, one key for a top-level argument (most take the action's own
/// name).
fn tool_path(code: ActionCode, name: &str) -> Vec<&str> {
    match (code, name) {
        (ActionCode::ResumeWithBudget, "raise_budget_tokens") => vec!["budget", "raise_tokens"],
        (ActionCode::ResolveIntent, "decision") => vec!["sent"],
        _ => vec![name],
    }
}

/// `value` put at `path` of `out`, objects made on the way.
fn insert_at(out: &mut Map<String, Value>, path: &[&str], value: Value) {
    match path {
        [] => {}
        [last] => {
            out.insert((*last).into(), value);
        }
        [first, rest @ ..] => {
            let inner = out
                .entry(*first)
                .or_insert_with(|| Value::Object(Map::new()));
            if let Value::Object(map) = inner {
                insert_at(map, rest, value);
            }
        }
    }
}

/// An action argument's value as the tool takes it: `run_intent_resolve`
/// takes the decision on a send as the bool `sent`, `run_resume` a token
/// raise as a number.
fn tool_value(code: ActionCode, name: &str, value: &str) -> Value {
    match (code, name, value) {
        (ActionCode::ResolveIntent, "decision", "sent") => json!(true),
        (ActionCode::ResolveIntent, "decision", "not_sent") => json!(false),
        (ActionCode::ResumeWithBudget, "raise_budget_tokens", v) => {
            v.parse::<u64>().map_or_else(|_| json!(v), |n| json!(n))
        }
        _ => json!(value),
    }
}

/// The arguments the action carries, as the tool takes them.
fn known_arguments(a: &NextAction, caller: ActionCaller) -> Map<String, Value> {
    let mut out = Map::new();
    let supervised = matches!(
        a.code,
        ActionCode::SupervisorRetryNode
            | ActionCode::SupervisorContinueFrom
            | ActionCode::SupervisorAbort
    ) || (matches!(a.code, ActionCode::AnswerQuestion | ActionCode::RetryNow)
        && caller == ActionCaller::Supervisor);
    // A supervisor tool names its run by the session token, which an
    // action never carries; every other tool by the run id.
    if !supervised {
        out.insert("run_id".into(), json!(a.run_id));
    }
    let node_tools = matches!(
        a.code,
        ActionCode::AnswerQuestion | ActionCode::DecideReview | ActionCode::DecidePublish
    );
    if node_tools && let Some(node) = &a.target.node {
        out.insert("node".into(), json!(node));
    }
    // `run_resume` starts a waiting retry only when asked to.
    if a.code == ActionCode::RetryNow && !supervised {
        out.insert("retry_now".into(), json!(true));
    }
    if let Some(task) = &a.target.task_id {
        out.insert("task_id".into(), json!(task));
    }
    if let Some(r) = &a.target.request {
        out.insert("request".into(), json!(r.seq));
    }
    for arg in &a.args {
        if let ArgValue::Given { value } = &arg.value {
            insert_at(
                &mut out,
                &tool_path(a.code, &arg.name),
                tool_value(a.code, &arg.name, value),
            );
        }
    }
    out
}

/// One argument the caller fills in: `name` the tool's argument (dotted
/// when it is nested, `path` then its keys), the options as the tool takes
/// them, or what to supply.
fn fill_entry(code: ActionCode, arg: &ActionArg) -> Option<Value> {
    let path = tool_path(code, &arg.name);
    let mut entry = json!({ "name": path.join("."), "required": arg.required });
    if path.len() > 1 {
        entry["path"] = json!(path);
    }
    match &arg.value {
        ArgValue::Given { .. } => return None,
        ArgValue::Choice { options, default } => {
            let as_tool = |o: &String| tool_value(code, &arg.name, o);
            entry["options"] = Value::Array(options.iter().map(as_tool).collect());
            if let Some(d) = default {
                entry["default"] = as_tool(d);
            }
            if code == ActionCode::ResolveIntent {
                entry["hint"] = json!("true: the send went out; false: it did not");
            }
        }
        ArgValue::Supplied { hint } => entry["hint"] = json!(mcp_hint(code, &arg.name, hint)),
    }
    Some(entry)
}

/// What to supply, said for the MCP tool: a host task's `output` is the
/// subagent's reply itself, where the CLI takes a file holding it.
fn mcp_hint<'a>(code: ActionCode, name: &str, hint: &'a str) -> &'a str {
    match (code, name) {
        (ActionCode::SubmitHostTask, "output") => "the subagent's final reply, verbatim",
        _ => hint,
    }
}

/// `a` as the MCP tool call that takes it, for `caller`.
pub fn render(a: &NextAction, caller: ActionCaller) -> Value {
    let mut fill: Vec<Value> = a
        .args
        .iter()
        .filter_map(|g| fill_entry(a.code, g))
        .collect();
    let arguments = known_arguments(a, caller);
    let supervised = !arguments.contains_key("run_id");
    if supervised {
        fill.insert(
            0,
            json!({ "name": "token", "required": true, "hint": "your supervisor session token" }),
        );
    }
    // Whom the run accepts it from first, then this caller's channel (a
    // person-only gate, a step's own run) as the surface marked it.
    let refusal = caller.refusal(a.code, &a.principals).or(a.caller_refusal);
    let mut out = json!({
        "code": a.code,
        "run_id": a.run_id,
        "tool": tool_for(a.code, caller),
        "arguments": arguments,
        "fill": fill,
        "principals": a.principals,
        "ready": a.ready,
        "available_to_caller": refusal.is_none(),
        "reason": a.reason,
    });
    if let Some(why) = refusal {
        out["caller_refusal"] = json!(why.as_str());
    }
    if refusal.is_none() && caller.relays(&a.principals) {
        out["relay_to_person"] = json!(true);
    }
    if let Some(n) = &a.not_ready {
        out["not_ready"] = json!(n);
    }
    if let Some(auto) = &a.auto {
        out["auto"] = json!(auto);
    }
    // What an unattended run does at this wait, on every run.
    if let Some(u) = &a.unattended {
        out["unattended"] = json!(u);
    }
    out
}

/// The channel of a decision through this MCP server: a run step's when
/// the server runs inside one (`APB_RUN_ID` or `APB_HOST_TASK` in its
/// environment), else the agent channel. Neither decides a person-only
/// gate.
pub fn mcp_channel() -> DecisionChannel {
    if apb_core::step_env::StepCaller::from_env()
        .describe()
        .is_some()
    {
        DecisionChannel::RunStep
    } else {
        DecisionChannel::Mcp
    }
}

/// Marks the actions of the runs under `runs_dir` with what `caller`'s
/// channel may not take ([`apb_engine::next_action::mark_for_caller`]).
pub fn mark(actions: &mut RunActions, runs_dir: &std::path::Path, caller: ActionCaller) {
    apb_engine::next_action::mark_for_caller(
        actions,
        runs_dir,
        caller.channel(),
        &apb_core::step_env::StepCaller::from_env(),
    );
}

/// Adds `next_actions` (each action as a tool call), `primary` (its index)
/// and `stop` (the verdict over all of them) to a run answer.
pub fn add_to(out: &mut Value, actions: &RunActions, caller: ActionCaller) {
    let rendered: Vec<Value> = actions.actions.iter().map(|a| render(a, caller)).collect();
    out["next_actions"] = Value::Array(rendered);
    out["stop"] = json!(actions.stop);
    if let Some(i) = actions.primary {
        out["primary"] = json!(i);
    }
    if actions.children_truncated {
        out["children_truncated"] = json!(true);
    }
    if let Some(d) = &actions.run_deadline {
        out["run_deadline"] = json!(d);
    }
}

/// The next actions of the run at `dir`, redacted through `redaction` (the
/// one the surface redacts the rest of its answer with), from the view the
/// surface already read, so the actions never disagree with the rest.
pub fn of_view(
    redaction: &apb_engine::run_redact::WorkspaceRedaction,
    dir: &std::path::Path,
    run_id: &str,
    view: std::sync::Arc<apb_engine::run_view::RunView>,
) -> RunActions {
    let obs = apb_engine::next_action::Observation::from_view(dir, run_id, view);
    apb_engine::next_action::for_surface(redaction, &obs)
}

/// A start refusal with the start actions that answer it: the refusal
/// object itself, with `start_actions` added when there is any.
pub fn refusal_with_actions(of: StartOf<'_>, refusal: Value) -> Value {
    let actions = start_actions(of, &refusal);
    with_start_actions(refusal, &actions)
}

/// The answer of an MCP start the run gate (or the consent check) refused:
/// `policy_refusal` as before, and beside it the `start_actions` that
/// answer it.
pub fn policy_refusal(of: StartOf<'_>, refusal: Value) -> Value {
    let actions = start_actions(of, &refusal);
    with_start_actions(json!({ "policy_refusal": refusal }), &actions)
}

/// The start actions of a refusal, as the engine names them.
pub fn start_actions(of: StartOf<'_>, refusal: &Value) -> Vec<StartAction> {
    apb_engine::next_action::start::from_refusal(of, refusal)
}

/// `value` (an object) with `start_actions` set to `actions`, left as it
/// is when there is none.
pub fn with_start_actions(mut value: Value, actions: &[StartAction]) -> Value {
    if !actions.is_empty()
        && let Some(obj) = value.as_object_mut()
    {
        obj.insert("start_actions".into(), json!(actions));
    }
    value
}

#[cfg(test)]
mod tests {
    use super::*;
    use apb_engine::next_action::{ActionTarget, RequestKind, RequestRef};

    fn action(code: ActionCode, principals: Vec<Principal>, args: Vec<ActionArg>) -> NextAction {
        NextAction {
            code,
            run_id: "r-1".into(),
            target: ActionTarget {
                node: Some("ask".into()),
                request: Some(RequestRef {
                    kind: RequestKind::Question,
                    seq: 7,
                    nonce: None,
                }),
                ..ActionTarget::default()
            },
            args,
            principals,
            ready: true,
            not_ready: None,
            auto: None,
            command: None,
            unattended: None,
            person_only: false,
            caller_refusal: None,
            reason: "node `ask` asks a question".into(),
        }
    }

    /// The fill of a decision on an unknown send is the tool's bool `sent`;
    /// a budget raise is nested under `budget`; a host task's output is the
    /// reply itself.
    #[test]
    fn fills_take_the_shape_the_tool_takes() {
        let choice = |name: &str, options: &[&str]| ActionArg {
            name: name.into(),
            value: ArgValue::Choice {
                options: options.iter().map(|o| o.to_string()).collect(),
                default: None,
            },
            required: true,
        };
        let a = action(
            ActionCode::ResolveIntent,
            vec![Principal::Person],
            vec![choice("decision", &["sent", "not_sent"])],
        );
        let v = render(&a, ActionCaller::Host);
        assert_eq!(v["fill"][0]["name"], "sent");
        assert_eq!(v["fill"][0]["options"], json!([true, false]));
        let mut given = a.clone();
        given.args[0].value = ArgValue::Given {
            value: "not_sent".into(),
        };
        assert_eq!(
            render(&given, ActionCaller::Host)["arguments"]["sent"],
            false
        );

        let mut raise = action(
            ActionCode::ResumeWithBudget,
            vec![Principal::Person],
            vec![],
        );
        raise.args = vec![supplied("raise_budget_tokens")];
        let v = render(&raise, ActionCaller::Host);
        assert_eq!(v["fill"][0]["path"], json!(["budget", "raise_tokens"]));
        raise.args[0].value = ArgValue::Given {
            value: "5000".into(),
        };
        assert_eq!(
            render(&raise, ActionCaller::Host)["arguments"]["budget"],
            json!({ "raise_tokens": 5000 })
        );

        let task = action(
            ActionCode::SubmitHostTask,
            vec![Principal::Host],
            vec![supplied("output")],
        );
        let v = render(&task, ActionCaller::Host);
        assert_eq!(v["fill"][0]["hint"], "the subagent's final reply, verbatim");
    }

    fn supplied(name: &str) -> ActionArg {
        ActionArg {
            name: name.into(),
            value: ArgValue::Supplied {
                hint: "your answer".into(),
            },
            required: true,
        }
    }

    /// Every rendered action carries what an unattended run does at the
    /// wait, in the engine's shape, and nothing where it does not apply.
    #[test]
    fn a_rendered_action_carries_its_unattended_outcome() {
        let mut a = action(ActionCode::DecideReview, vec![Principal::Person], vec![]);
        assert!(render(&a, ActionCaller::Host).get("unattended").is_none());
        a.unattended = Some(apb_engine::next_action::UnattendedOutcome::Decides {
            option: "approve".into(),
        });
        assert_eq!(
            render(&a, ActionCaller::Host)["unattended"],
            json!({ "kind": "decides", "option": "approve" })
        );
        a.unattended = Some(apb_engine::next_action::UnattendedOutcome::FailsRun {
            code: apb_engine::run_config::HITL_REQUIRED.into(),
        });
        assert_eq!(
            render(&a, ActionCaller::Supervisor)["unattended"],
            json!({ "kind": "fails_run", "code": "hitl_required" })
        );
    }

    #[test]
    fn a_question_maps_to_run_answer_with_its_request() {
        let a = action(
            ActionCode::AnswerQuestion,
            vec![Principal::Person, Principal::Operator],
            vec![supplied("answer")],
        );
        let v = render(&a, ActionCaller::Host);
        assert_eq!(v["tool"], "run_answer");
        assert_eq!(
            v["arguments"],
            json!({ "run_id": "r-1", "node": "ask", "request": 7 })
        );
        assert_eq!(v["fill"][0]["name"], "answer");
        assert_eq!(v["available_to_caller"], true);
        assert_eq!(v["relay_to_person"], true);
    }

    /// A supervisor session sees a human question as not its own, and the
    /// host sees the supervisor's actions as not its own.
    #[test]
    fn caller_capability_follows_the_principals() {
        let human = action(
            ActionCode::AnswerQuestion,
            vec![Principal::Person, Principal::Operator],
            vec![supplied("answer")],
        );
        let v = render(&human, ActionCaller::Supervisor);
        assert_eq!(v["available_to_caller"], false);
        assert_eq!(v["caller_refusal"], "relay_to_person");
        assert_eq!(v["fill"][0]["name"], "token");
        assert!(v["arguments"].get("run_id").is_none());

        let retry = action(
            ActionCode::SupervisorRetryNode,
            vec![Principal::Supervisor],
            vec![],
        );
        let v = render(&retry, ActionCaller::Host);
        assert_eq!(v["available_to_caller"], false);
        assert_eq!(v["caller_refusal"], "supervisor_only");
        assert_eq!(v["tool"], "supervisor_node_retry");
        assert_eq!(
            render(&retry, ActionCaller::Supervisor)["available_to_caller"],
            true
        );
        assert_eq!(
            render(&retry, ActionCaller::Person)["available_to_caller"],
            false
        );
        assert_eq!(
            render(&retry, ActionCaller::Person)["caller_refusal"],
            "not_this_caller"
        );
    }

    /// A read-only server's caller takes nothing that changes a run, even
    /// what the run accepts from a host; waiting and reading stay open.
    #[test]
    fn a_read_only_caller_takes_no_mutating_action() {
        let every = vec![
            Principal::Person,
            Principal::Operator,
            Principal::Host,
            Principal::Supervisor,
        ];
        for code in [
            ActionCode::AnswerQuestion,
            ActionCode::DecideReview,
            ActionCode::DecidePublish,
            ActionCode::SubmitHostTask,
            ActionCode::SupervisorRetryNode,
            ActionCode::SupervisorContinueFrom,
            ActionCode::SupervisorAbort,
            ActionCode::ResumeRun,
            ActionCode::ResumeWithBudget,
            ActionCode::ResolveIntent,
        ] {
            let v = render(&action(code, every.clone(), vec![]), ActionCaller::ReadOnly);
            assert_eq!(v["available_to_caller"], false, "{code:?}");
            assert_eq!(v["caller_refusal"], READ_ONLY_REFUSAL, "{code:?}");
            assert!(v.get("relay_to_person").is_none(), "{code:?}");
        }
        for code in [ActionCode::WaitAgain, ActionCode::ReadReport] {
            let v = render(&action(code, every.clone(), vec![]), ActionCaller::ReadOnly);
            assert_eq!(v["available_to_caller"], true, "{code:?}");
        }
    }
}
