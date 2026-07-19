use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use crate::event::{Event, EventPayload};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NodeStatus {
    Pending,
    Ready,
    Running,
    Succeeded,
    Failed,
    Unknown,
    TimedOut,
    Interrupted,
    Skipped,
    Cancelled,
}

impl NodeStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            NodeStatus::Pending => "pending",
            NodeStatus::Ready => "ready",
            NodeStatus::Running => "running",
            NodeStatus::Succeeded => "succeeded",
            NodeStatus::Failed => "failed",
            NodeStatus::Unknown => "unknown",
            NodeStatus::TimedOut => "timed_out",
            NodeStatus::Interrupted => "interrupted",
            NodeStatus::Skipped => "skipped",
            NodeStatus::Cancelled => "cancelled",
        }
    }
    pub fn from_label(s: &str) -> NodeStatus {
        match s {
            "pending" => NodeStatus::Pending,
            "ready" => NodeStatus::Ready,
            "running" => NodeStatus::Running,
            "succeeded" => NodeStatus::Succeeded,
            "failed" => NodeStatus::Failed,
            "timed_out" => NodeStatus::TimedOut,
            "interrupted" => NodeStatus::Interrupted,
            "skipped" => NodeStatus::Skipped,
            "cancelled" => NodeStatus::Cancelled,
            _ => NodeStatus::Unknown,
        }
    }
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunStatus {
    #[default]
    Created,
    Running,
    Paused,
    Succeeded,
    Failed,
    Aborted,
    Interrupted,
}

impl RunStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            RunStatus::Created => "created",
            RunStatus::Running => "running",
            RunStatus::Paused => "paused",
            RunStatus::Succeeded => "succeeded",
            RunStatus::Failed => "failed",
            RunStatus::Aborted => "aborted",
            RunStatus::Interrupted => "interrupted",
        }
    }
}

/// A human_review node decision: the chosen option and the reviewer's note.
#[derive(Debug, Clone)]
pub struct ReviewDecision {
    pub decision: String,
    pub note: String,
}

#[derive(Debug, Default, Clone)]
pub struct RunState {
    pub run_status: RunStatus,
    pub nodes: BTreeMap<String, NodeStatus>,
    pub attempts: BTreeMap<String, u32>,
    pub outputs: BTreeMap<String, String>,
    pub reviews: BTreeMap<String, ReviewDecision>,
    pub last_node: Option<String>,
}

impl RunState {
    pub fn fold(events: &[Event]) -> RunState {
        let mut s = RunState::default();
        // Open attempts: node -> attempt. Closed by attempt_finished/node_finished.
        let mut open: BTreeSet<String> = BTreeSet::new();
        for e in events {
            match &e.payload {
                EventPayload::RunStarted { .. } => s.run_status = RunStatus::Running,
                EventPayload::RunProvenance { .. } => {}
                EventPayload::NodeStarted { node, .. } => {
                    s.nodes.insert(node.clone(), NodeStatus::Running);
                }
                EventPayload::AttemptStarted { node, attempt, .. } => {
                    s.nodes.insert(node.clone(), NodeStatus::Running);
                    s.attempts.insert(node.clone(), *attempt);
                    open.insert(node.clone());
                }
                EventPayload::AttemptFinished { node, .. } => {
                    open.remove(node);
                }
                EventPayload::NodeFinished {
                    node,
                    status,
                    output,
                    ..
                } => {
                    open.remove(node);
                    s.nodes.insert(node.clone(), NodeStatus::from_label(status));
                    s.outputs.insert(node.clone(), output.clone());
                    s.last_node = Some(node.clone());
                }
                EventPayload::RunPaused { .. } => s.run_status = RunStatus::Paused,
                EventPayload::RunFinished { outcome } => {
                    s.run_status = match outcome.as_str() {
                        "succeeded" => RunStatus::Succeeded,
                        _ => RunStatus::Failed,
                    };
                }
                EventPayload::RetryStarted { .. } | EventPayload::FallbackTriggered { .. } => {}
                EventPayload::RunAborted { .. } => s.run_status = RunStatus::Aborted,
                EventPayload::WakeRaised { .. } => {}
                EventPayload::SupervisorAction { .. } => {}
                EventPayload::SupervisorLost { .. } => {}
                EventPayload::PatchApplied { .. }
                | EventPayload::PatchRejected { .. }
                | EventPayload::RunMigrated { .. }
                | EventPayload::VersionPromoted { .. } => {}
                EventPayload::WaitStarted { .. }
                | EventPayload::WaitSignalled { .. }
                | EventPayload::WaitTimeout { .. } => {}
                // Context compaction is a materialized rendering artifact,
                // it does not affect run state.
                EventPayload::ContextCompacted { .. } => {}
                // An accepted environment drift is an audit record, it does not change state.
                EventPayload::EnvironmentDriftAccepted { .. } => {}
                // Progress is an audit-only cycle report, it does not change state.
                EventPayload::RunProgress { .. } => {}
                // A child-run marker is an audit record; the node's own status
                // events carry the run-state effect.
                EventPayload::ChildRunStarted { .. } => {}
                // A connector call is an audit record (spec 6.2); it does not
                // change run state.
                EventPayload::ConnectorCall { .. } => {}
                EventPayload::ReviewRequested { .. } => {}
                EventPayload::ReviewDecided {
                    node,
                    decision,
                    note,
                } => {
                    s.reviews.insert(
                        node.clone(),
                        ReviewDecision {
                            decision: decision.clone(),
                            note: note.clone(),
                        },
                    );
                }
            }
        }
        // Attempts still open after the last event: interrupted.
        if !open.is_empty() {
            for node in &open {
                s.nodes.insert(node.clone(), NodeStatus::Interrupted);
            }
            if !matches!(s.run_status, RunStatus::Succeeded | RunStatus::Failed) {
                s.run_status = RunStatus::Interrupted;
            }
        }
        s
    }
}
