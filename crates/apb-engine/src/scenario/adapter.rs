//! The scripted agent of a scenario run: an [`AgentAdapter`] that starts no
//! program and answers each attempt from the answer book.
//!
//! It sits where the drive chooses an attempt's adapter (next to the CLI
//! and host adapters), so everything around the attempt (retries,
//! fallbacks, failure classification, the quota pause, the deferred retry,
//! the success and completion checks, the journal) is the real drive's. It
//! reports what an agent would have left behind:
//!
//! - `succeeded`: the output, and a `success` verdict with the declared
//!   output fields in the status file, as an agent writes them;
//! - `failed` without a kind (or `agent`): a failed report and a `failure`
//!   verdict in the status file, as an agent that says it failed;
//! - `failed` with `transient`, `quota`, `auth` or `budget`: a process
//!   failure whose detail the failure classifier reads as that kind;
//! - `question`: the question an interactive agent asks, the attempt's
//!   `then` coming on the answer round.

use std::path::Path;
use std::sync::atomic::AtomicBool;

use apb_core::scenario::{AttemptAnswer, ScriptedFailure, ScriptedStatus};

use super::BoundaryKey;
use super::session::{node_key, of};
use crate::adapter::{
    AgentAdapter, AgentFailure, AgentReport, AgentTask, AskedQuestion, ControlHooks, ErrorClass,
    LiveHooks, StallHooks,
};
use crate::state::NodeStatus;

/// The scripted agent of one node execution (and list item).
pub(crate) struct ScenarioAdapter<'a> {
    pub(crate) run_dir: &'a Path,
    pub(crate) node: &'a str,
    pub(crate) item: Option<&'a str>,
}

/// A detail the failure classifier reads as the scripted kind.
pub(crate) fn failure_detail(kind: ScriptedFailure) -> &'static str {
    match kind {
        ScriptedFailure::Transient => "scenario: connection reset by the agent's provider",
        ScriptedFailure::Quota => "scenario: rate limit reached (429 too many requests)",
        ScriptedFailure::Auth => "scenario: authentication failed, the agent is not logged in",
        ScriptedFailure::Budget => "scenario: spend limit reached for this account",
        ScriptedFailure::Agent => "scenario: the agent failed",
    }
}

impl ScenarioAdapter<'_> {
    fn answer(&self, task: &AgentTask) -> Result<AgentReport, AgentFailure> {
        let Some(session) = of(self.run_dir) else {
            return Err(AgentFailure::new(
                ErrorClass::Transport,
                "the scripted agent runs only in a scenario run",
            ));
        };
        let key = BoundaryKey {
            item: self.item.map(str::to_string),
            ..node_key(self.node)
        };
        let (key, taken) = session.attempt(&key);
        let Some(taken) = taken else {
            // Under `on_missing: error` the session holds the stop, which
            // the drive raises once this attempt is journaled.
            let _ = session.missing(super::BoundaryKind::AgentAttempt, &key);
            let what = super::book::MissingAnswer {
                boundary: super::BoundaryKind::AgentAttempt.as_str().into(),
                node: key.node.clone(),
                visit: key.visit,
                attempt: key.attempt,
                item: key.item.clone(),
                part: None,
            };
            return Ok(report(NodeStatus::Failed, what.describe()));
        };
        let a: AttemptAnswer = taken.answer;
        if let Some(q) = &a.question {
            let mut r = report(NodeStatus::Succeeded, String::new());
            r.question = Some(AskedQuestion {
                question: q.text.clone(),
                options: Vec::new(),
            });
            return Ok(r);
        }
        // The scripted timeout: the failure a deadline kill reports, at
        // once. Everything after (the `timed_out` status, the node's final
        // status, the retry the timeout spends on the scripted agent, the
        // timeout continuation, the supervisor wake `NodeTimeout`) is the
        // real drive's, exactly as for a real timeout.
        if a.timed_out {
            const TIMEOUT_DETAIL: &str = "scenario: the attempt reached its deadline";
            let message = match a.output.as_deref() {
                Some(o) if !o.is_empty() => format!("{TIMEOUT_DETAIL}: {o}"),
                _ => TIMEOUT_DETAIL.to_string(),
            };
            return Err(AgentFailure::new(ErrorClass::Timeout, message));
        }
        let output = a.output.clone().unwrap_or_default();
        match (
            a.status.unwrap_or(ScriptedStatus::Succeeded),
            a.failure_kind,
        ) {
            (ScriptedStatus::Succeeded, _) => {
                write_verdict(task, "success", &a.outputs)?;
                Ok(report(NodeStatus::Succeeded, output))
            }
            (ScriptedStatus::Failed, None | Some(ScriptedFailure::Agent)) => {
                write_verdict(task, "failure", &a.outputs)?;
                Ok(report(NodeStatus::Failed, output))
            }
            (ScriptedStatus::Failed, Some(kind)) => {
                let detail = failure_detail(kind);
                let message = match output.is_empty() {
                    true => detail.to_string(),
                    false => format!("{detail}: {output}"),
                };
                // The detail decides the class only when the output does not
                // read as another kind.
                let message = match crate::failure_class::classify(&message)
                    == crate::failure_class::classify(detail)
                {
                    true => message,
                    false => detail.to_string(),
                };
                Err(AgentFailure::new(ErrorClass::ProcessExit, message))
            }
        }
    }
}

/// The verdict an agent leaves in its status file, when the attempt was
/// handed one: `status`, plus the scripted output fields when there are
/// any. Without it a `require_verdict` node would read the attempt as
/// interrupted, not as the success or failure the answer scripts.
fn write_verdict(
    task: &AgentTask,
    status: &str,
    outputs: &std::collections::BTreeMap<String, serde_json::Value>,
) -> Result<(), AgentFailure> {
    let Some(path) = &task.status_file else {
        return Ok(());
    };
    let doc = match outputs.is_empty() {
        true => serde_json::json!({ "status": status }),
        false => serde_json::json!({ "status": status, "outputs": outputs }),
    };
    apb_core::fsutil::atomic_write_private(path, doc.to_string().as_bytes()).map_err(|e| {
        AgentFailure::new(
            ErrorClass::Transport,
            format!("scenario: the status file was not written: {e}"),
        )
    })
}

fn report(status: NodeStatus, output: String) -> AgentReport {
    AgentReport {
        status,
        // The summary a real reply of this text would carry.
        summary: crate::adapter::interpret_report(&output).summary,
        raw: output.clone(),
        output,
        question: None,
        session: None,
        usage: None,
        capture: Default::default(),
        exposure: None,
        host_notes_dropped: Default::default(),
    }
}

impl AgentAdapter for ScenarioAdapter<'_> {
    fn run(&self, task: &AgentTask) -> Result<AgentReport, AgentFailure> {
        self.answer(task)
    }

    fn run_cancellable(
        &self,
        task: &AgentTask,
        _cancel: &AtomicBool,
        on_spawn: Option<&dyn Fn(u32, u64)>,
        _live: Option<&LiveHooks>,
        _stall: Option<&StallHooks>,
        _control: Option<&ControlHooks>,
    ) -> Result<AgentReport, AgentFailure> {
        // Nothing is spawned: the attempt starts in this process.
        if let Some(f) = on_spawn {
            f(std::process::id(), 0);
        }
        self.answer(task)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::failure_class::{FailureKind, classify};
    use crate::scenario::session::{ScenarioPrep, ScenarioSession};

    /// Each scripted failure kind reads as itself to the failure
    /// classifier, which decides the infrastructure retry, the quota pause
    /// and the blocked agent exactly as for a real agent.
    #[test]
    fn every_scripted_failure_detail_classifies_as_its_kind() {
        for (scripted, kind) in [
            (ScriptedFailure::Transient, FailureKind::Transient),
            (ScriptedFailure::Quota, FailureKind::Quota),
            (ScriptedFailure::Auth, FailureKind::Auth),
            (ScriptedFailure::Budget, FailureKind::Budget),
            (ScriptedFailure::Agent, FailureKind::Agent),
        ] {
            assert_eq!(classify(failure_detail(scripted)), kind, "{scripted:?}");
        }
    }

    /// A timed-out answer makes the scripted agent report the failure a
    /// deadline kill reports: an `AgentFailure` of the timeout class, at
    /// once, with the output as its detail.
    #[test]
    fn a_timed_out_answer_is_a_timeout_failure_at_once() {
        let dir = tempfile::tempdir().unwrap();
        let playbook = apb_core::schema::Playbook::from_yaml(
            "schema: 2\nid: p\nname: p\nversion: 1.0.0\ndefaults: { profile: x }\nnodes:\n  - { id: start, type: start }\n  - { id: work, type: agent_task, prompt: w }\n  - { id: done, type: finish, outcome: success }\nedges:\n  - { from: start, to: work }\n  - { from: work, to: done }\n",
        )
        .unwrap();
        let scenario: apb_core::scenario::Scenario = yaml_serde::from_str(
            "schema: 1\nid: s\nanswers:\n  work:\n    - { visit: 1, attempts: [ { timed_out: true, output: partial } ] }\n",
        )
        .unwrap();
        let prep = ScenarioPrep {
            scenario: scenario.clone(),
            level: apb_core::scenario::ScenarioLevel::Quick,
            scenario_digest: apb_core::scenario::scenario_digest(&scenario),
            max_requests: 0,
            sent: Default::default(),
            external_data: false,
        };
        let session = std::sync::Arc::new(ScenarioSession::new(&prep, &playbook, dir.path()));
        super::super::session::register(dir.path(), session);
        let adapter = ScenarioAdapter {
            run_dir: dir.path(),
            node: "work",
            item: None,
        };
        let task = AgentTask {
            prompt: "w",
            untrusted_section: None,
            model: "m",
            workdir: dir.path(),
            timeout: Some(std::time::Duration::from_secs(600)),
            stream_log: None,
            soul: None,
            grant_autonomy: false,
            spawn_env: SPAWN_ENV.get_or_init(Default::default),
            interactive: false,
            report_contract: false,
            node: "work",
            agent: "scenario",
            extract: None,
            status_file: None,
            hermetic_settings: None,
            transcript_dir: None,
            output_limits: Default::default(),
        };
        let err = adapter
            .run(&task)
            .expect_err("the timed-out answer is a failure");
        assert_eq!(err.class, ErrorClass::Timeout, "{err:?}");
        assert!(err.message.contains("deadline"), "{err:?}");
        assert!(err.message.contains("partial"), "{err:?}");
        super::super::session::unregister(dir.path());
    }

    static SPAWN_ENV: std::sync::OnceLock<crate::adapter::SpawnEnv> = std::sync::OnceLock::new();
}
