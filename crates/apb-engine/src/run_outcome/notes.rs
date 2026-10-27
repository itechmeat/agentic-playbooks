//! The findings and warnings a run's attempts reported (`attempt_notes`),
//! for the run surfaces. A retried node often reports the same problem
//! again, so each entry is kept once per node, with the latest attempt
//! that reported it.

use apb_core::run_notes::{Finding, NoteWarning};
use serde::Serialize;

use crate::event::{Event, EventPayload};

/// One finding with the node and attempt that reported it.
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RunFinding {
    pub node: String,
    pub attempt: u32,
    #[serde(flatten)]
    #[cfg_attr(feature = "ts", ts(flatten))]
    pub finding: Finding,
}

/// One warning with the node and attempt that reported it.
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RunWarning {
    pub node: String,
    pub attempt: u32,
    #[serde(flatten)]
    #[cfg_attr(feature = "ts", ts(flatten))]
    pub warning: NoteWarning,
}

/// Every finding and warning of a run, deduplicated, in the order they were
/// first reported.
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct RunNotes {
    pub findings: Vec<RunFinding>,
    pub warnings: Vec<RunWarning>,
    /// Malformed entries the attempts sent, dropped when they were
    /// journaled.
    pub dropped: u32,
}

impl RunNotes {
    /// The notes `events` journaled.
    pub fn from_events(events: &[Event]) -> Self {
        let mut notes = RunNotes::default();
        for e in events {
            let EventPayload::AttemptNotes {
                node,
                attempt,
                findings,
                warnings,
                dropped,
                ..
            } = &e.payload
            else {
                continue;
            };
            notes.dropped = notes.dropped.saturating_add(*dropped);
            for f in findings {
                upsert(
                    &mut notes.findings,
                    RunFinding {
                        node: node.clone(),
                        attempt: *attempt,
                        finding: f.clone(),
                    },
                    |a, b| a.node == b.node && same_finding(&a.finding, &b.finding),
                    // The latest attempt's own words: its number and why it
                    // left the problem, never one attempt's number next to
                    // another's reason.
                    |e, n| *e = n.clone(),
                );
            }
            for w in warnings {
                upsert(
                    &mut notes.warnings,
                    RunWarning {
                        node: node.clone(),
                        attempt: *attempt,
                        warning: w.clone(),
                    },
                    |a, b| a.node == b.node && a.warning == b.warning,
                    |e, n| *e = n.clone(),
                );
            }
        }
        notes
    }

    /// Nothing was reported and nothing was dropped.
    pub fn is_empty(&self) -> bool {
        self.findings.is_empty() && self.warnings.is_empty() && self.dropped == 0
    }
}

/// The same problem: the same kind at the same place with the same
/// evidence. Why it was not fixed may be worded differently each time.
fn same_finding(a: &Finding, b: &Finding) -> bool {
    a.kind == b.kind && a.location == b.location && a.evidence == b.evidence
}

/// Updates the entry `same` finds with `update`, or appends `new`.
fn upsert<T>(
    list: &mut Vec<T>,
    new: T,
    same: impl Fn(&T, &T) -> bool,
    update: impl Fn(&mut T, &T),
) {
    match list.iter_mut().find(|e| same(e, &new)) {
        Some(existing) => update(existing, &new),
        None => list.push(new),
    }
}

/// `security at src/auth.rs:12: <evidence> (not fixed: <why>; reported by
/// <node> attempt <n>)`, the one text line of a finding.
pub fn finding_line(f: &RunFinding) -> String {
    format!(
        "{} at {}: {} (not fixed: {}; reported by {} attempt {})",
        f.finding.kind,
        f.finding.location,
        f.finding.evidence,
        f.finding.why_not_fixed,
        f.node,
        f.attempt
    )
}

/// `<subject>: <reason> (reported by <node> attempt <n>)`, the one text
/// line of a warning.
pub fn warning_line(w: &RunWarning) -> String {
    format!(
        "{}: {} (reported by {} attempt {})",
        w.warning.subject, w.warning.reason, w.node, w.attempt
    )
}
