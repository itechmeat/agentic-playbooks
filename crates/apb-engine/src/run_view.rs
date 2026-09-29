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
    /// Every attempt the run finished, with or without usage, so a line
    /// can say "5 attempts, 3 with usage" instead of passing the reporting
    /// ones off as all of them.
    pub finished_attempts: u32,
}

impl RunUsage {
    /// The totals over `events`, `None` when no attempt reported usage.
    pub fn from_events(events: &[Event]) -> Option<Self> {
        let mut total = RunUsage::default();
        for e in events {
            let EventPayload::AttemptFinished { usage, .. } = &e.payload else {
                continue;
            };
            total.finished_attempts = total.finished_attempts.saturating_add(1);
            let Some(u) = usage else {
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

// --- decision totals (issue #165 Part 4) -----------------------------------

/// What the run's decision-model uses cost and how fast they answered,
/// totalled from its `decision_made` events (see
/// [`crate::decision::decision_totals`]). Absent from every surface when the
/// run journaled no decision, so an unconfigured run reads as before.
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Clone, Default, PartialEq, serde::Serialize)]
pub struct RunDecisions {
    /// Decisions journaled: answered, failed, or skipped for the budget.
    pub decisions: u32,
    /// Requests actually sent to a provider.
    pub requests: u32,
    /// Decisions answered without a request (the run's cache).
    pub replayed: u32,
    pub errors: u32,
    /// Provider-reported cost, or the list-price estimate where the
    /// provider reported none (then `cost_estimated`).
    pub cost_usd: f64,
    pub cost_estimated: bool,
    /// Over the requests actually sent; absent when none was.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub p50_latency_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub p95_latency_ms: Option<u64>,
    pub by_use: BTreeMap<String, RunDecisionUse>,
}

/// One use's share of [`RunDecisions`].
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Clone, Default, PartialEq, serde::Serialize)]
pub struct RunDecisionUse {
    pub requests: u32,
    pub errors: u32,
    /// Decisions that changed engine behaviour (never in shadow).
    pub applied: u32,
    /// Shadow decisions whose answer a higher mode would have acted on.
    pub shadow_would_change: u32,
}

impl RunDecisions {
    /// The totals over `events`, `None` when no decision was journaled.
    pub fn from_events(events: &[Event]) -> Option<Self> {
        let t = crate::decision::decision_totals(events);
        (t.decisions > 0).then(|| RunDecisions {
            decisions: t.decisions,
            requests: t.requests,
            replayed: t.cached,
            errors: t.errors,
            cost_usd: t.cost_usd,
            cost_estimated: t.cost_estimated,
            p50_latency_ms: t.p50_latency_ms,
            p95_latency_ms: t.p95_latency_ms,
            by_use: t
                .by_use
                .into_iter()
                .map(|(name, u)| {
                    (
                        name,
                        RunDecisionUse {
                            requests: u.requests,
                            errors: u.errors,
                            applied: u.applied,
                            shadow_would_change: u.shadow_would_change,
                        },
                    )
                })
                .collect(),
        })
    }

    /// Shadow decisions a higher mode would have acted on, over every use.
    pub fn shadow_would_change(&self) -> u32 {
        self.by_use.values().map(|u| u.shadow_would_change).sum()
    }

    /// The one line `apb runs <id>` and `apb wait` print:
    /// `14 (2 replayed, 1 error), $0.0004, p50 190 ms; shadow would change: 3`.
    pub fn line(&self) -> String {
        let mut extra = Vec::new();
        if self.replayed > 0 {
            extra.push(format!("{} replayed", self.replayed));
        }
        if self.errors > 0 {
            let noun = if self.errors == 1 { "error" } else { "errors" };
            extra.push(format!("{} {noun}", self.errors));
        }
        let mut line = self.decisions.to_string();
        if !extra.is_empty() {
            line.push_str(&format!(" ({})", extra.join(", ")));
        }
        line.push_str(&format!(", ${:.4}", self.cost_usd));
        if self.cost_estimated {
            line.push_str(" estimated");
        }
        if let Some(p50) = self.p50_latency_ms {
            line.push_str(&format!(", p50 {p50} ms"));
        }
        let would = self.shadow_would_change();
        if would > 0 {
            line.push_str(&format!("; shadow would change: {would}"));
        }
        line
    }
}

impl RunView {
    /// The run's decision totals, `None` when it journaled no decision.
    pub fn decisions(&self) -> Option<RunDecisions> {
        RunDecisions::from_events(&self.events)
    }
}

#[cfg(test)]
mod decision_tests {
    use super::*;
    use crate::event::DecisionAnswer;

    fn made(
        seq: u64,
        mode: &str,
        would_change: Option<bool>,
        cached: bool,
        error: Option<&str>,
    ) -> Event {
        let answered = error.is_none();
        Event {
            seq,
            ts: seq as u128,
            payload: EventPayload::DecisionMade {
                enforce_refused: None,
                join: BTreeMap::new(),
                use_site: "completion_check".into(),
                node: Some("w".into()),
                attempt: Some(1),
                provider: Some("main".into()),
                model: answered.then(|| "jev-1.13.0".to_string()),
                calibrated: answered,
                mode: mode.into(),
                questions_digest: String::new(),
                state_digest: String::new(),
                state_bytes: 10,
                output_chars: None,
                answers: std::collections::BTreeMap::from([(
                    "final_result".to_string(),
                    DecisionAnswer {
                        p: Some(0.5),
                        ..Default::default()
                    },
                )]),
                applied: false,
                would_change,
                baseline: None,
                latency_ms: if cached { 0 } else { 100 + seq * 10 },
                input_tokens: Some(1000),
                cost_usd: (!cached && answered).then_some(0.000042),
                cost_estimated: !cached && answered,
                cached,
                error: error.map(str::to_string),
            },
        }
    }

    fn json(events: &[Event]) -> Option<String> {
        RunDecisions::from_events(events).map(|d| serde_json::to_string(&d).unwrap())
    }

    #[test]
    fn a_run_without_decisions_has_no_block() {
        let events = [Event {
            seq: 0,
            ts: 0,
            payload: EventPayload::RunStarted {
                playbook: "p".into(),
                version: "1.0.0".into(),
            },
        }];
        assert_eq!(json(&events), None);
    }

    #[test]
    fn a_shadow_run_totals_its_decisions() {
        let events = [
            made(1, "shadow", Some(true), false, None),
            made(2, "shadow", Some(false), false, None),
        ];
        assert_eq!(
            json(&events).unwrap(),
            r#"{"decisions":2,"requests":2,"replayed":0,"errors":0,"cost_usd":0.000084,"cost_estimated":true,"p50_latency_ms":110,"p95_latency_ms":120,"by_use":{"completion_check":{"requests":2,"errors":0,"applied":0,"shadow_would_change":1}}}"#
        );
        let d = RunDecisions::from_events(&events).unwrap();
        assert_eq!(
            d.line(),
            "2, $0.0001 estimated, p50 110 ms; shadow would change: 1"
        );
    }

    #[test]
    fn errors_and_replayed_decisions_are_counted_apart() {
        let events = [
            made(1, "shadow", Some(false), false, None),
            made(2, "shadow", Some(false), true, None),
            made(3, "shadow", None, false, Some("timeout")),
        ];
        let d = RunDecisions::from_events(&events).unwrap();
        assert_eq!(
            serde_json::to_string(&d).unwrap(),
            r#"{"decisions":3,"requests":2,"replayed":1,"errors":1,"cost_usd":0.000042,"cost_estimated":true,"p50_latency_ms":110,"p95_latency_ms":130,"by_use":{"completion_check":{"requests":2,"errors":1,"applied":0,"shadow_would_change":0}}}"#
        );
        assert_eq!(
            d.line(),
            "3 (1 replayed, 1 error), $0.0000 estimated, p50 110 ms"
        );
    }
}

// --- end of decision totals -------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    /// The totals cover the attempts that reported usage; the count of all
    /// finished attempts rides along so no surface passes them off as all.
    #[test]
    fn usage_counts_every_finished_attempt_next_to_the_reporting_ones() {
        let line = |usage: &str| {
            serde_json::from_str::<Event>(&format!(
                r#"{{"seq":0,"ts":1,"type":"attempt_finished","node":"a","attempt":1,"status":"failed"{usage}}}"#
            ))
            .unwrap()
        };
        let reported = r#","usage":{"input_tokens":10,"output_tokens":2,"cache_read_tokens":0,"cache_write_tokens":0,"source":"reported"}"#;
        let events = vec![line(""), line(reported), line("")];
        let u = RunUsage::from_events(&events).unwrap();
        assert_eq!((u.attempts, u.finished_attempts), (1, 3));
        assert_eq!(RunUsage::from_events(&[line(""), line("")]), None);
    }

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
