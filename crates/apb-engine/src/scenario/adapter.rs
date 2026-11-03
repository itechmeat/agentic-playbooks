//! The scripted agent of a scenario run: an [`AgentAdapter`] that starts no
//! program and answers each attempt from the answer book.
//!
//! It sits where the drive chooses an attempt's adapter (next to the CLI
//! and host adapters), so everything around the attempt (retries,
//! fallbacks, failure classification, the quota pause, the deferred retry,
//! the success and completion checks, the journal) is the real drive's. It
//! reports what an agent would have left behind:
//!
//! - `succeeded`: the output, and the declared output fields in the status
//!   file, as an agent writes them;
//! - `failed` without a kind (or `agent`): a failed report, as an agent that
//!   says it failed;
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
        let output = a.output.clone().unwrap_or_default();
        match (
            a.status.unwrap_or(ScriptedStatus::Succeeded),
            a.failure_kind,
        ) {
            (ScriptedStatus::Succeeded, _) => {
                if !a.outputs.is_empty()
                    && let Some(path) = &task.status_file
                {
                    let doc = serde_json::json!({ "status": "success", "outputs": a.outputs });
                    apb_core::fsutil::atomic_write_private(path, doc.to_string().as_bytes())
                        .map_err(|e| {
                            AgentFailure::new(
                                ErrorClass::Transport,
                                format!("scenario: the status file was not written: {e}"),
                            )
                        })?;
                }
                Ok(report(NodeStatus::Succeeded, output))
            }
            (ScriptedStatus::Failed, None | Some(ScriptedFailure::Agent)) => {
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
}
