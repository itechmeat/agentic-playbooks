//! What a run looks like to anyone reading it: the one model behind every
//! read-only run surface (`apb runs`, `apb wait`, MCP `run_status`,
//! `run_report`, `run_wait` and `runs_list`, the dashboard list and detail).
//!
//! Each of those used to read the journal and derive the status on its own,
//! and they drifted: one used the strict reader and failed on a line the
//! driver was still writing, one folded a child's journal with no liveness at
//! all, one ignored a dead driver. Every one of them now goes through
//! [`RunView::load`], so a run reads the same everywhere.

use std::collections::BTreeMap;
use std::path::Path;

use crate::error::EngineError;
use crate::event::{Event, EventPayload, UnknownEvent};
use crate::progress::ProgressSummary;
use crate::state::{RunState, RunStatus};

/// The journal as a read-only surface sees it: tolerant of a torn last line,
/// which is a normal transient state while the driver appends (see
/// [`crate::event::read_all_lossy_tail`]). The drive itself, and anything
/// that writes to the journal after reading it, stays on the strict
/// [`crate::event::read_all`].
pub fn read_events(run_dir: &Path) -> Result<Vec<Event>, EngineError> {
    crate::event::read_all_lossy_tail(run_dir)
}

/// One observation of a run, with the process-table overlay applied once.
#[derive(Debug)]
pub struct RunView {
    pub events: Vec<Event>,
    /// Events of a type this binary does not know (a newer apb wrote them),
    /// skipped from `events` and everything folded from it. Read-only
    /// surfaces show their count; they never fail on them.
    pub unknown: Vec<UnknownEvent>,
    /// The pure fold of `events`: outputs, failure reason, reviews.
    pub state: RunState,
    pub progress: Option<ProgressSummary>,
    /// `None` when nothing claims to drive the run, otherwise whether that
    /// claim holds (see [`crate::liveness::driver_alive`]).
    pub driver_alive: Option<bool>,
    /// The status to report: the fold, corrected by liveness (see
    /// [`crate::liveness::reported_run_status`]).
    pub run_status: RunStatus,
}

/// Token usage summed over a run's own agent attempts that reported it (see
/// `AttemptFinished.usage`); a sub-playbook run reports its own.
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Clone, Default, PartialEq, serde::Serialize)]
pub struct RunUsage {
    /// Attempts whose output reported usage. Attempts that reported none
    /// (plain-text agents, attempts that died) are not in the totals.
    pub attempts: u32,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_read_tokens: u64,
    pub cache_write_tokens: u64,
    /// The sum of the costs the agent CLIs reported, absent when none did.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub cost_usd: Option<f64>,
    /// How many of `attempts` reported a cost, so a partial sum reads as one.
    pub cost_attempts: u32,
    /// At least one attempt's numbers are apb's own estimate rather than a
    /// count the agent CLI printed (`source: estimated`).
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    #[cfg_attr(feature = "ts", ts(as = "Option<bool>", optional))]
    pub estimated: bool,
}

impl RunUsage {
    /// The totals over `events`, `None` when no attempt reported usage.
    pub fn from_events(events: &[Event]) -> Option<Self> {
        let mut total = RunUsage::default();
        for e in events {
            let EventPayload::AttemptFinished { usage: Some(u), .. } = &e.payload else {
                continue;
            };
            // Saturating: the numbers come from agent output, and a bogus
            // one must not panic a read-only surface.
            total.attempts = total.attempts.saturating_add(1);
            total.input_tokens = total.input_tokens.saturating_add(u.input_tokens);
            total.output_tokens = total.output_tokens.saturating_add(u.output_tokens);
            total.cache_read_tokens = total.cache_read_tokens.saturating_add(u.cache_read_tokens);
            total.cache_write_tokens = total
                .cache_write_tokens
                .saturating_add(u.cache_write_tokens);
            if let Some(c) = u.cost_usd {
                total.cost_usd = Some(total.cost_usd.unwrap_or(0.0) + c);
                total.cost_attempts = total.cost_attempts.saturating_add(1);
            }
            total.estimated |= u.source == apb_core::agent_output::UsageSource::Estimated;
        }
        (total.attempts > 0).then_some(total)
    }
}

/// A sub-playbook run started by a run, as its parent reports it.
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Clone, serde::Serialize)]
pub struct ChildRun {
    pub node_id: String,
    pub run_id: String,
    /// The child's reported status, or `unknown` when its run directory is
    /// gone or unreadable.
    pub status: String,
}

impl RunView {
    /// Reads run `run_id` at `run_dir` once.
    pub fn load(run_dir: &Path, run_id: &str) -> Result<Self, EngineError> {
        let crate::event::JournalRead { events, unknown } = crate::event::read_journal(run_dir)?;
        let progress = crate::progress::from_run_dir(run_dir, &events);
        let driver_alive = crate::liveness::driver_alive(run_dir, run_id);
        let waiting = progress.as_ref().is_some_and(|p| p.waiting_on.is_some());
        let run_status = crate::liveness::reported_run_status(&events, waiting, driver_alive);
        Ok(Self {
            state: RunState::fold(&events),
            events,
            unknown,
            progress,
            driver_alive,
            run_status,
        })
    }

    /// Per-node status for live reporting (`lost` for a dead attempt,
    /// `running` for a live one the fold calls interrupted, `interrupted` for
    /// in-flight work under a provably dead driver).
    pub fn nodes(&self) -> BTreeMap<String, String> {
        crate::liveness::reported_node_statuses(&self.events, self.driver_alive)
    }

    /// Token usage over the run's attempts, `None` when none reported any.
    pub fn usage(&self) -> Option<RunUsage> {
        RunUsage::from_events(&self.events)
    }

    /// The failure reason, only for a run that actually ended `failed`.
    pub fn failure_reason(&self) -> Option<String> {
        (self.run_status == RunStatus::Failed)
            .then(|| {
                self.state
                    .failure_reason
                    .as_ref()
                    .map(crate::state::FailureReason::display)
            })
            .flatten()
    }

    /// The sub-playbook runs this run started, each reported through its own
    /// [`RunView`]: a child driven by a live parent reads `running`, not the
    /// bare fold's `interrupted`.
    pub fn children(&self, run_dir: &Path) -> Vec<ChildRun> {
        self.events
            .iter()
            .filter_map(|e| match &e.payload {
                EventPayload::ChildRunStarted { node_id, run_id } => {
                    let status = run_dir
                        .parent()
                        .map(|runs| runs.join(run_id))
                        .filter(|d| d.is_dir())
                        .and_then(|d| RunView::load(&d, run_id).ok())
                        .map(|child| child.run_status.as_str().to_string())
                        .unwrap_or_else(|| "unknown".to_string());
                    Some(ChildRun {
                        node_id: node_id.clone(),
                        run_id: run_id.clone(),
                        status,
                    })
                }
                _ => None,
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A pid that was valid and is now free: spawn, reap, reuse the number.
    fn reaped_pid() -> u32 {
        let mut child = std::process::Command::new("sh")
            .arg("-c")
            .arg("exit 0")
            .spawn()
            .expect("spawn a throwaway child");
        let pid = child.id();
        child.wait().expect("reap the throwaway child");
        pid
    }

    fn write_run(run_dir: &Path, events: &[EventPayload], driver_pid: u32) {
        std::fs::create_dir_all(run_dir).unwrap();
        let journal: String = events
            .iter()
            .enumerate()
            .map(|(i, p)| {
                let e = Event {
                    seq: i as u64,
                    ts: 1_000 + i as u128,
                    payload: p.clone(),
                };
                serde_json::to_string(&e).unwrap() + "\n"
            })
            .collect();
        std::fs::write(run_dir.join("events.jsonl"), journal).unwrap();
        std::fs::write(
            crate::driver::driver_pid_path(run_dir),
            driver_pid.to_string(),
        )
        .unwrap();
    }

    /// A node whose work no process journaled a pid for (a script node, or an
    /// attempt whose spawn recorded none) reads like the run once the driver
    /// is provably dead: `interrupted`, never `running` under an
    /// `interrupted` run.
    #[test]
    fn a_node_without_a_pid_under_a_dead_driver_reads_interrupted() {
        let tmp = tempfile::tempdir().unwrap();
        let run_dir = tmp.path().join(".apb/runs/r1");
        write_run(
            &run_dir,
            &[
                EventPayload::RunStarted {
                    playbook: "p".into(),
                    version: "1.0.0".into(),
                },
                EventPayload::NodeStarted {
                    node: "script".into(),
                    attempt: 1,
                },
                EventPayload::NodeStarted {
                    node: "agent".into(),
                    attempt: 1,
                },
                EventPayload::AttemptStarted {
                    node: "agent".into(),
                    attempt: 1,
                    agent: "stub".into(),
                    soul_delivery: None,
                    skills_mode: None,
                    pid: None,
                    spawn_ms: None,
                    model: None,
                    workdir: None,
                    transcript: None,
                },
            ],
            reaped_pid(),
        );

        let view = RunView::load(&run_dir, "r1").unwrap();
        assert_eq!(view.driver_alive, Some(false));
        assert_eq!(view.run_status, RunStatus::Interrupted);
        let nodes = view.nodes();
        assert_eq!(nodes["script"], "interrupted");
        assert_eq!(nodes["agent"], "interrupted");
    }
}
