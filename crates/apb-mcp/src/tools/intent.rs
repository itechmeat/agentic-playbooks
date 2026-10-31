//! The send intents of a run (`run_intent_resolve`, `apb intent`): what the
//! send ledger knows about each send, and a person's decision on one whose
//! outcome nobody can tell.

use std::path::Path;

use apb_engine::connector::intent::ResolveError;
use apb_engine::run_outcome::{RunIntent, intents_from};
use apb_engine::run_view::read_events;
use serde_json::{Value, json};

use super::{ToolError, resolve_run_dir};

/// Every send intent of `run_id`, in journal order.
pub fn run_intents(root: &Path, run_id: &str) -> Result<Vec<RunIntent>, ToolError> {
    let dir = resolve_run_dir(root, run_id)?;
    let events = read_events(&dir)?;
    Ok(intents_from(&events, apb_core::clock::now_ms_u64()))
}

/// Who asks for a decision on a send intent: the surface a person used
/// (`by`, kept with the decision) and the run context the caller itself
/// runs in. Only a surface that can see its caller fills the context: the
/// CLI reads its own process environment (`APB_RUN_ID`, `APB_HOST_TASK`);
/// an MCP server cannot tell a person's session from a host subagent that
/// shares it, so it passes none and the rule is an instruction there.
#[derive(Debug, Clone, Copy)]
pub struct IntentCaller<'a> {
    /// The surface the person used, as the settled intent names it.
    pub by: &'a str,
    /// The run the caller runs inside (`APB_RUN_ID`), if any.
    pub run_id: Option<&'a str>,
    /// The host task the caller runs for (`APB_HOST_TASK`), if any.
    pub host_task: Option<&'a str>,
}

impl<'a> IntentCaller<'a> {
    /// A caller with no run context of its own.
    pub fn person(by: &'a str) -> Self {
        Self {
            by,
            run_id: None,
            host_task: None,
        }
    }
}

/// Records a person's decision on send intent `intent` of `run_id`: it went
/// out (`sent`) or it did not. Refused (a conflict) for an intent whose
/// outcome is known or that is still in flight, and for a caller inside a
/// run (see [`refusal_inside_run`]); an unknown run or intent is not found.
/// A later repeat of the send then answers `already_sent`, or sends it
/// again.
pub fn run_intent_resolve(
    root: &Path,
    run_id: &str,
    intent: &str,
    sent: bool,
    note: Option<String>,
    caller: IntentCaller<'_>,
) -> Result<Value, ToolError> {
    if let Some(refusal) = refusal_inside_run(caller.run_id, caller.host_task) {
        return Err(ToolError::Conflict(refusal));
    }
    let dir = resolve_run_dir(root, run_id)?;
    let note = note.map(|n| n.trim().to_string()).filter(|n| !n.is_empty());
    apb_engine::connector::intent::resolve_by_person(&dir, intent, sent, note, caller.by).map_err(
        |e| match e {
            ResolveError::NotFound(m) => ToolError::NotFound(m),
            ResolveError::Refused(m) => ToolError::Conflict(m),
            ResolveError::Journal(e) => ToolError::from(e),
        },
    )?;
    Ok(json!({
        "run_id": run_id,
        "intent": intent,
        "settled": if sent { "confirmed" } else { "not_sent" },
        "next": if sent {
            "a repeat of this send answers already_sent without sending"
        } else {
            "a repeat of this send sends it again"
        },
    }))
}

/// The refusal of a decision made from inside a run: an agent or script
/// step (`APB_RUN_ID`) or a host task's subagent (`APB_HOST_TASK`) cannot
/// speak for the person about what a send did, so it reports the send as a
/// finding instead. `None` outside a run.
pub(crate) fn refusal_inside_run(run_id: Option<&str>, host_task: Option<&str>) -> Option<String> {
    let run_id = run_id.filter(|v| !v.trim().is_empty());
    let host_task = host_task.filter(|v| !v.trim().is_empty());
    let from = match (run_id, host_task) {
        (_, Some(task)) => format!("host task `{task}` (APB_HOST_TASK is set)"),
        (Some(run), None) => format!("run `{run}` (APB_RUN_ID is set)"),
        (None, None) => return None,
    };
    Some(format!(
        "a send's outcome is decided by a person, and this request comes from inside {from}; report the send as a finding instead, and a person resolves it"
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A run whose one send went out with no answer recorded.
    fn seed(root: &Path) {
        let dir = root.join(".apb/runs/r1");
        std::fs::create_dir_all(&dir).unwrap();
        let lines = [
            r#"{"seq":0,"ts":1,"type":"send_intent_recorded","intent":"si-a","nonce":"n","node":"post","connector":"telegram","function":"send_message","account":"main","mode":"none","label":"hello","pid":0,"deadline_ms":0}"#,
            r#"{"seq":1,"ts":2,"type":"send_intent_settled","intent":"si-a","outcome":"unknown","source":"response","detail":"timeout"}"#,
        ];
        std::fs::write(dir.join("events.jsonl"), lines.join("\n") + "\n").unwrap();
    }

    #[test]
    fn a_person_resolves_an_unknown_send_once() {
        let tmp = tempfile::tempdir().unwrap();
        seed(tmp.path());
        let before = run_intents(tmp.path(), "r1").unwrap();
        assert_eq!(before[0].state, "unknown");

        let ack = run_intent_resolve(
            tmp.path(),
            "r1",
            "si-a",
            true,
            Some(" checked ".into()),
            IntentCaller::person("t"),
        )
        .unwrap();
        assert_eq!(ack["settled"], "confirmed");
        let after = run_intents(tmp.path(), "r1").unwrap();
        assert_eq!(after[0].state, "confirmed");
        assert_eq!(after[0].settled_by.as_deref(), Some("t"));

        // A known outcome is not decided twice, and a missing run or intent
        // is named.
        assert!(matches!(
            run_intent_resolve(
                tmp.path(),
                "r1",
                "si-a",
                false,
                None,
                IntentCaller::person("t")
            ),
            Err(ToolError::Conflict(_))
        ));
        assert!(matches!(
            run_intent_resolve(
                tmp.path(),
                "r1",
                "si-x",
                true,
                None,
                IntentCaller::person("t")
            ),
            Err(ToolError::NotFound(_))
        ));
        assert!(matches!(
            run_intents(tmp.path(), "r2"),
            Err(ToolError::NotFound(_))
        ));
    }

    #[test]
    fn a_caller_inside_a_run_is_refused_and_the_send_stays_unknown() {
        let tmp = tempfile::tempdir().unwrap();
        seed(tmp.path());
        let inside = IntentCaller {
            by: "t",
            run_id: Some("r9"),
            host_task: Some("t-1"),
        };
        match run_intent_resolve(tmp.path(), "r1", "si-a", true, None, inside) {
            Err(ToolError::Conflict(m)) => assert!(m.contains("host task `t-1`")),
            other => panic!("expected a conflict, got {}", other.is_ok()),
        }
        assert_eq!(run_intents(tmp.path(), "r1").unwrap()[0].state, "unknown");
    }

    #[test]
    fn a_decision_from_inside_a_run_or_a_host_task_is_refused() {
        assert_eq!(refusal_inside_run(None, None), None);
        assert_eq!(refusal_inside_run(Some(" "), Some("")), None);
        let from_run = refusal_inside_run(Some("r1"), None).unwrap();
        assert!(from_run.contains("run `r1` (APB_RUN_ID is set)"));
        let from_task = refusal_inside_run(Some("r1"), Some("t-1")).unwrap();
        assert!(from_task.contains("host task `t-1`"));
        assert!(
            from_task.ends_with("report the send as a finding instead, and a person resolves it")
        );
    }
}
