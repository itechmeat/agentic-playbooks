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

/// The nodes a run's journal records as having fallen back to the host
/// (`execution_fallback`), each once, in the order they first fell back.
/// The one source for `run_status`'s `execution.fell_back` and the
/// `apb runs <id>` text line.
pub fn fell_back_nodes(events: &[Event]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for e in events {
        if let crate::event::EventPayload::ExecutionFallback { node, .. } = &e.payload
            && !out.contains(node)
        {
            out.push(node.clone());
        }
    }
    out
}

/// One isolated attempt and what its isolation held and did not: the row
/// `apb runs <id>`, `apb doctor --run`, MCP `run_status` and the run page
/// show.
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct AttemptIsolationRow {
    pub node: String,
    /// The list item the attempt ran, for an item of a list node.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub item: Option<String>,
    pub attempt: u32,
    pub isolation: crate::event::AttemptIsolation,
}

impl AttemptIsolationRow {
    /// The row of an `attempt_started` that carries an isolation record.
    pub fn of_event(e: &Event) -> Option<Self> {
        match &e.payload {
            EventPayload::AttemptStarted {
                node,
                attempt,
                item,
                isolation: Some(iso),
                ..
            } => Some(AttemptIsolationRow {
                node: node.clone(),
                item: item.clone(),
                attempt: *attempt,
                isolation: iso.clone(),
            }),
            _ => None,
        }
    }

    /// Whether the attempt ran in a sandbox (`backend: bwrap`). Any other
    /// value, an empty or unknown one included, is not one.
    pub fn sandboxed(&self) -> bool {
        self.isolation.backend == "bwrap"
    }

    /// One line: `node `w` attempt 1: full on bwrap, network none; limits:
    /// ...`, with the reasons a `best_effort` attempt ran without a sandbox.
    pub fn line(&self) -> String {
        let iso = &self.isolation;
        let item = self
            .item
            .as_deref()
            .map(|i| format!(" item `{i}`"))
            .unwrap_or_default();
        let limits = if iso.limits.is_empty() {
            "none".to_string()
        } else {
            iso.limits.join(", ")
        };
        let mut line = format!(
            "node `{}`{item} attempt {}: {} on {}, network {}; limits: {limits}",
            self.node, self.attempt, iso.level, iso.backend, iso.network,
        );
        if !iso.fallbacks.is_empty() {
            line.push_str(&format!("; no sandbox: {}", iso.fallbacks.join("; ")));
        }
        line
    }
}

/// Every isolated attempt of `events`, in journal order.
pub fn attempt_isolation(events: &[Event]) -> Vec<AttemptIsolationRow> {
    events
        .iter()
        .filter_map(AttemptIsolationRow::of_event)
        .collect()
}

/// The `read_only` check of one attempt of a read-only node: its tier and
/// what the before-state covered, and the changes that failed it. The row
/// `apb runs <id>`, MCP `run_status`, the HTTP run detail and the run page
/// show, from the attempt's (or a script's) `read_only_checked`.
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct ReadOnlyRow {
    pub node: String,
    /// The list item the attempt ran, for an item of a list node.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub item: Option<String>,
    pub attempt: u32,
    #[serde(flatten)]
    #[cfg_attr(feature = "ts", ts(flatten))]
    pub check: crate::event::ReadOnlyCheck,
    /// The changes that failed the node (`read_only_dirty`); absent while
    /// none was seen.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub violated: Option<ReadOnlyViolation>,
}

/// What a `read_only_violated` recorded: the changed paths (names, never
/// content), how many were left out, the evidence file, and, for a member
/// of a concurrent batch, the members whose attempts overlapped its own.
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct ReadOnlyViolation {
    pub changes: Vec<crate::event::ReadOnlyChange>,
    /// Changed paths left out of `changes`.
    #[serde(skip_serializing_if = "is_zero")]
    #[cfg_attr(feature = "ts", ts(type = "number", optional))]
    pub truncated: u64,
    /// The run-relative evidence file.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub evidence: Option<String>,
    /// The batch members whose attempts overlapped this one; the change
    /// cannot be attributed to one of them. Absent when empty.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    #[cfg_attr(feature = "ts", ts(as = "Option<Vec<String>>", optional))]
    pub concurrent_with: Vec<String>,
}

fn is_zero(n: &u64) -> bool {
    *n == 0
}

/// The changed paths one line names before it counts the rest.
const READ_ONLY_LINE_CHANGES: usize = 5;

impl ReadOnlyRow {
    /// The tier's wire name.
    pub fn tier_name(&self) -> &'static str {
        tier_name(self.check.tier)
    }

    /// One line: `node `r` attempt 1: enforced (the sandbox binds the tree
    /// read-only)`, the counts of a detected tier, the limit of a
    /// partial one, and the changes that failed it.
    pub fn line(&self) -> String {
        use crate::event::{ReadOnlyLimit, ReadOnlyTier};
        let item = self
            .item
            .as_deref()
            .map(|i| format!(" item `{i}`"))
            .unwrap_or_default();
        let c = &self.check;
        // A script's row comes from its violation alone, which carries no
        // counts.
        let over = if c.entries == 0 && c.bytes == 0 {
            String::new()
        } else {
            format!(
                " over {} {}, {} {}",
                c.entries,
                if c.entries == 1 { "entry" } else { "entries" },
                c.bytes,
                if c.bytes == 1 { "byte" } else { "bytes" }
            )
        };
        let how = match c.tier {
            ReadOnlyTier::Enforced => "the sandbox binds the tree read-only".to_string(),
            ReadOnlyTier::Detected => format!("checked after the attempt{over}"),
            ReadOnlyTier::DetectedPartial => format!(
                "checked after the attempt{over}; the {} limit stopped it short, so a change past it goes unseen",
                match c.partial {
                    Some(ReadOnlyLimit::Bytes) => "bytes",
                    Some(ReadOnlyLimit::Entries) | None => "entries",
                }
            ),
        };
        let mut line = format!(
            "node `{}`{item} attempt {}: {} ({how})",
            self.node,
            self.attempt,
            self.tier_name()
        );
        if let Some(v) = &self.violated {
            let total = v.changes.len() as u64 + v.truncated;
            let named: Vec<String> = v
                .changes
                .iter()
                .take(READ_ONLY_LINE_CHANGES)
                .map(|ch| format!("{} {}", ch.path, ch.change))
                .collect();
            let more = total.saturating_sub(named.len() as u64);
            // A batch member names the peers it ran beside; without any,
            // today's wording.
            let dirty = if v.concurrent_with.is_empty() {
                "changed the tree".to_string()
            } else {
                format!(
                    "the tree changed while it ran beside {}",
                    v.concurrent_with.join(", ")
                )
            };
            line.push_str(&format!(
                "; {dirty} (read_only_dirty), {total} {}: {}",
                if total == 1 { "path" } else { "paths" },
                named.join(", ")
            ));
            if more > 0 {
                line.push_str(&format!(", {more} more"));
            }
            if let Some(e) = &v.evidence {
                line.push_str(&format!("; evidence {e}"));
            }
        }
        line
    }
}

/// The wire name of a read-only tier.
pub fn tier_name(tier: crate::event::ReadOnlyTier) -> &'static str {
    use crate::event::ReadOnlyTier;
    match tier {
        ReadOnlyTier::Enforced => "enforced",
        ReadOnlyTier::Detected => "detected",
        ReadOnlyTier::DetectedPartial => "detected_partial",
    }
}

/// Every read-only check of `events`, in journal order: one row per
/// `read_only_checked`, with the `read_only_violated` of the same node and
/// attempt folded into it; a violation with no such check (a journal cut
/// between the two) is a row of its own.
pub fn read_only_rows(events: &[Event]) -> Vec<ReadOnlyRow> {
    let mut rows: Vec<ReadOnlyRow> = Vec::new();
    for e in events {
        match &e.payload {
            EventPayload::ReadOnlyChecked {
                node,
                attempt,
                check,
            } => rows.push(ReadOnlyRow {
                node: node.clone(),
                item: None,
                attempt: *attempt,
                check: check.clone(),
                violated: None,
            }),
            EventPayload::ReadOnlyViolated {
                node,
                attempt,
                tier,
                changes,
                truncated,
                evidence,
                concurrent_with,
            } => {
                let violation = ReadOnlyViolation {
                    changes: changes.clone(),
                    truncated: *truncated,
                    evidence: evidence.clone(),
                    concurrent_with: concurrent_with.clone(),
                };
                match rows
                    .iter_mut()
                    .rev()
                    .find(|r| r.node == *node && r.attempt == *attempt && r.violated.is_none())
                {
                    Some(row) => row.violated = Some(violation),
                    None => rows.push(ReadOnlyRow {
                        node: node.clone(),
                        item: None,
                        attempt: *attempt,
                        check: crate::event::ReadOnlyCheck {
                            tier: *tier,
                            entries: 0,
                            bytes: 0,
                            partial: None,
                        },
                        violated: Some(violation),
                    }),
                }
            }
            _ => {}
        }
    }
    rows
}

/// Whether the run at `run_dir` is unattended, from its write-once
/// manifest (the authoritative record; `run.yaml` is only the drive's
/// copy). A run without a manifest, or one an older apb wrote, is attended.
pub fn run_unattended(run_dir: &Path) -> bool {
    crate::manifest::read(run_dir)
        .ok()
        .flatten()
        .is_some_and(|m| m.unattended)
}

/// The param names (and `instruction`) the start of the run in `run_dir`
/// took from the project's remembered input, as its journal records them.
/// Every start surface reports them, so input the caller never passed is
/// never silent. Empty when nothing was recalled or the journal does not
/// read.
pub fn recalled_input(run_dir: &Path) -> Vec<String> {
    read_events(run_dir)
        .unwrap_or_default()
        .into_iter()
        .find_map(|e| match e.payload {
            crate::event::EventPayload::RunProvenance { remembered, .. } => Some(remembered),
            _ => None,
        })
        .unwrap_or_default()
}

/// The limits the run recorded at start (`run_provenance.limits`), such as
/// a reviewer on the main role's agent. Every run detail surface shows them
/// as one line each. Recorded once: a later profile rebind does not update
/// them. Empty for a run without limits or an older journal.
pub fn run_limits(events: &[Event]) -> Vec<crate::event::RunLimit> {
    events
        .iter()
        .find_map(|e| match &e.payload {
            crate::event::EventPayload::RunProvenance { limits, .. } => Some(limits.clone()),
            _ => None,
        })
        .unwrap_or_default()
}

mod started;
pub use started::{
    STARTED_RUNS_MAX, STARTED_RUNS_SCAN, StartedRunView, StartedRuns, started_by, started_by_line,
    started_runs, started_runs_line,
};

/// The record of `run.previous_success.*` a surface shows: everything but
/// the output values, which reach the prompts and stay there.
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize)]
pub struct PreviousSuccessView {
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub run_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub started_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub finished_at: Option<String>,
    /// The snapshotted output keys, without their values.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    #[cfg_attr(feature = "ts", ts(as = "Option<Vec<String>>", optional))]
    pub keys: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    #[cfg_attr(feature = "ts", ts(as = "Option<Vec<String>>", optional))]
    pub withheld: Vec<String>,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub fallback: bool,
}

/// The nodes that render untrusted text, as the start recorded them on
/// `run_provenance.untrusted`.
pub fn untrusted_nodes(events: &[Event]) -> Vec<crate::event::UntrustedNode> {
    events
        .iter()
        .find_map(|e| match &e.payload {
            crate::event::EventPayload::RunProvenance { untrusted, .. } => Some(untrusted.clone()),
            _ => None,
        })
        .unwrap_or_default()
}

/// What the run's start recorded for `run.previous_success.*`
/// (`run_provenance.previous_success`) as a surface shows it; `None` when
/// its definition does not read the namespace.
pub fn previous_success(events: &[Event]) -> Option<PreviousSuccessView> {
    crate::previous_success::recorded(events).map(|ps| PreviousSuccessView {
        run_id: ps.run_id.clone(),
        started_at: ps.started_at.clone(),
        finished_at: ps.finished_at.clone(),
        keys: ps.outputs.keys().cloned().collect(),
        withheld: ps.withheld.clone(),
        fallback: ps.fallback,
    })
}

/// The first characters of the start key's MAC the run was started with
/// (`run_provenance.start_key`); `None` for a run started without a key.
pub fn start_key(events: &[Event]) -> Option<String> {
    events.iter().find_map(|e| match &e.payload {
        crate::event::EventPayload::RunProvenance { start_key, .. } => start_key.clone(),
        _ => None,
    })
}

/// A run's deadline as every surface shows it (`apb runs <id>`, MCP
/// `run_status`, the HTTP run detail, the report).
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct RunDeadlineView {
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub total_ms: u64,
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub elapsed_ms: u64,
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub remaining_ms: u64,
    pub source: crate::event::DeadlineSource,
    /// A person paused the run, or it ended: the clock does not run.
    pub frozen: bool,
    /// Wall-clock end while it runs (RFC 3339 UTC); absent while frozen.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub ends_at: Option<String>,
}

/// The run's deadline view at `now_ms`; `None` without a deadline.
pub fn deadline(events: &[Event], now_ms: u128) -> Option<RunDeadlineView> {
    let d = crate::pause::clock::in_force(events)?;
    let elapsed =
        u64::try_from(crate::pause::clock::deadline_elapsed_ms(events, now_ms)).unwrap_or(u64::MAX);
    let remaining = d.total_ms.saturating_sub(elapsed);
    let frozen = crate::state::RunState::fold(events)
        .run_status
        .is_terminal()
        || crate::pause::clock::paused_by(events) == Some(crate::pause::kind::PauseBy::Operator);
    let ends_at = (!frozen).then(|| {
        apb_core::clock::iso_utc(
            u64::try_from(now_ms)
                .unwrap_or(u64::MAX)
                .saturating_add(remaining),
        )
    });
    Some(RunDeadlineView {
        total_ms: d.total_ms,
        elapsed_ms: elapsed,
        remaining_ms: remaining,
        source: d.source,
        frozen,
        ends_at,
    })
}

/// The one text line of a deadline view: `45m, 12m left (ends <UTC>)`,
/// `45m, 12m left, frozen (paused by a person)`, or `45m, exceeded`.
pub fn deadline_line(v: &RunDeadlineView) -> String {
    let total = apb_core::duration::format_duration_ms(v.total_ms);
    if v.remaining_ms == 0 {
        return format!("{total}, exceeded");
    }
    let left = apb_core::duration::format_duration_ms(v.remaining_ms);
    match &v.ends_at {
        Some(at) => format!("{total}, {left} left (ends {at})"),
        None => format!("{total}, {left} left, frozen (paused by a person)"),
    }
}

/// A run's concurrency key as every surface shows it (`apb runs <id>`,
/// MCP `run_status`, the HTTP run detail, the report).
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct ConcurrencyView {
    pub display: String,
    pub on_busy: String,
    /// The run this one waits for, while it waits.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub waiting_for: Option<String>,
}

/// The run's concurrency view: the record it journaled, and, while the
/// run is not terminal, a display read of its slot file: when the slot
/// still names this run as `waiting` and holds an active run, that run is
/// what it waits for.
pub fn concurrency(
    root: &Path,
    run_id: &str,
    scope: apb_core::scope::Scope,
    playbook: &str,
    events: &[Event],
) -> Option<ConcurrencyView> {
    let record = events.iter().find_map(|e| match &e.payload {
        crate::event::EventPayload::RunProvenance {
            concurrency: Some(record),
            ..
        } => Some(record.clone()),
        _ => None,
    })?;
    let terminal = crate::state::RunState::fold(events)
        .run_status
        .is_terminal();
    let waiting_for = if terminal {
        None
    } else {
        crate::concurrency::slots::read_slots(root, scope, playbook)
            .iter()
            .find(|s| s.key_digest == record.key_digest)
            .filter(|s| s.waiting.as_ref().is_some_and(|h| h.run_id == run_id))
            .and_then(|s| s.active.as_ref().map(|h| h.run_id.clone()))
    };
    Some(ConcurrencyView {
        display: record.display,
        on_busy: record.on_busy,
        waiting_for,
    })
}

/// The one text line of a concurrency view:
/// `key "<display>" (queue_latest)` and, queued,
/// `key "<display>" (queue_latest), waiting for run <id>`.
pub fn concurrency_line(v: &ConcurrencyView) -> String {
    match &v.waiting_for {
        Some(id) => format!(
            "key \"{}\" ({}), waiting for run {}",
            v.display, v.on_busy, id
        ),
        None => format!("key \"{}\" ({})", v.display, v.on_busy),
    }
}

/// What a run surface reports for a liveness it did not probe.
pub const LIVENESS_UNKNOWN: &str = "unknown";

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
    /// The pause to report (issue #200), read through [`RunView::pause`].
    pause: Option<crate::pause::PauseInfo>,
    /// Whether the liveness overlay was applied. A read-only process starts
    /// no process probe, so its view keeps the journal's statuses, has no
    /// `driver_alive`, and reports liveness as unknown.
    pub liveness_checked: bool,
    /// The run's release is long overdue and no waker holds it
    /// ([`crate::pause::release_lost`]).
    release_lost: bool,
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
    /// It mixes bases; `cost_by_basis` splits it, and a surface shows the
    /// split rather than this sum alone.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub cost_usd: Option<f64>,
    /// How many of `attempts` reported a cost, so a partial sum reads as one.
    pub cost_attempts: u32,
    /// `cost_usd` split by what each figure is.
    #[serde(skip_serializing_if = "CostByBasis::is_empty")]
    #[cfg_attr(
        feature = "ts",
        ts(as = "Option<crate::run_view::CostByBasis>", optional)
    )]
    pub cost_by_basis: CostByBasis,
    /// At least one attempt's numbers are apb's own estimate rather than a
    /// count the agent CLI printed (`source: estimated`).
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    #[cfg_attr(feature = "ts", ts(as = "Option<bool>", optional))]
    pub estimated: bool,
    /// Every attempt the run finished, with or without usage, so a line
    /// can say "5 attempts, 3 with usage" instead of passing the reporting
    /// ones off as all of them.
    pub finished_attempts: u32,
    /// Where apb priced costs the agents did not print, one line per catalog
    /// and fetch date (`priced from NEAR AI Cloud catalog 2026-11-09`);
    /// empty when every cost was printed.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    #[cfg_attr(feature = "ts", ts(as = "Option<Vec<String>>", optional))]
    pub priced_from: Vec<String>,
}

/// Reported costs split by their basis (`apb_core::agent_output::CostBasis`):
/// each amount is absent when no attempt reported a figure on that basis.
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Clone, Copy, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct CostByBasis {
    /// Money billed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub billed_usd: Option<f64>,
    /// The list price of work a subscription covers; nobody is billed it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub subscription_equivalent_usd: Option<f64>,
    /// Figures whose basis is not known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub unknown_basis_usd: Option<f64>,
}

impl CostByBasis {
    /// Whether no figure was added.
    pub fn is_empty(&self) -> bool {
        self.parts().is_empty()
    }

    /// Adds `usd` on `basis`.
    pub fn add(&mut self, basis: apb_core::agent_output::CostBasis, usd: f64) {
        use apb_core::agent_output::CostBasis as B;
        let slot = match basis {
            B::Billed => &mut self.billed_usd,
            B::SubscriptionEquivalent => &mut self.subscription_equivalent_usd,
            B::Unknown => &mut self.unknown_basis_usd,
        };
        *slot = Some(slot.unwrap_or(0.0) + usd);
    }

    /// Adds every amount of `other`.
    pub fn merge(&mut self, other: &CostByBasis) {
        for (basis, usd) in other.parts() {
            self.add(basis, usd);
        }
    }

    /// The amounts present, billed first, with their basis.
    pub fn parts(&self) -> Vec<(apb_core::agent_output::CostBasis, f64)> {
        use apb_core::agent_output::CostBasis as B;
        B::ALL
            .into_iter()
            .filter_map(|b| {
                let v = match b {
                    B::Billed => self.billed_usd,
                    B::SubscriptionEquivalent => self.subscription_equivalent_usd,
                    B::Unknown => self.unknown_basis_usd,
                };
                v.map(|v| (b, v))
            })
            .collect()
    }

    /// The amounts in plain words, each formatted by `money`:
    /// `$0.0100 billed; $0.0125 subscription equivalent, not billed`.
    pub fn describe(&self, money: impl Fn(f64) -> String) -> String {
        self.parts()
            .into_iter()
            .map(|(b, v)| format!("{} {}", money(v), b.label()))
            .collect::<Vec<_>>()
            .join("; ")
    }
}

/// The tokens one attempt's usage counts toward a budget and the run
/// statistics: input plus output, cache reads and writes left out.
pub fn billable_tokens(u: &apb_core::agent_output::AgentUsage) -> u64 {
    u.input_tokens.saturating_add(u.output_tokens)
}

impl RunUsage {
    /// Input plus output tokens, cache reads and writes left out: what a
    /// budget and the run statistics count (see [`billable_tokens`]).
    pub fn billable_tokens(&self) -> u64 {
        self.input_tokens.saturating_add(self.output_tokens)
    }

    /// The totals over `events`, `None` when no attempt reported usage.
    pub fn from_events(events: &[Event]) -> Option<Self> {
        let total = Self::totals(events);
        (total.attempts > 0).then_some(total)
    }

    /// The totals over `events`, zero when no attempt reported usage
    /// (`finished_attempts` still counts every finished attempt).
    pub fn totals(events: &[Event]) -> Self {
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
                total.cost_by_basis.add(u.cost_basis, c);
            }
            if let Some(p) = &u.priced_from {
                let display = apb_core::cloud_models::lookup(&p.catalog)
                    .map_or(p.catalog.as_str(), |c| c.display);
                let line = format!(
                    "priced from {display} catalog {}",
                    apb_core::cloud_models::date(&p.fetched_at)
                );
                if !total.priced_from.contains(&line) {
                    total.priced_from.push(line);
                }
            }
            total.estimated |= u.source == apb_core::agent_output::UsageSource::Estimated;
        }
        total
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
        let liveness_checked = !crate::read_only::active();
        let state = RunState::fold(&events);
        let (driver_alive, run_status, release_lost) = if liveness_checked {
            let driver_alive = crate::liveness::driver_alive(run_dir, run_id);
            let waiting = progress.as_ref().is_some_and(|p| p.waiting_on.is_some());
            let status = crate::liveness::reported_run_status(&events, waiting, driver_alive);
            // A release nothing will wake: the run reads interrupted, and
            // its driver dead, like any run whose driver is gone.
            let lost = status == RunStatus::Running
                && driver_alive != Some(true)
                && state.released.as_ref().is_some_and(|r| {
                    crate::pause::release_lost(run_dir, r, apb_core::clock::now_ms_u64())
                });
            let status = if lost { RunStatus::Interrupted } else { status };
            (driver_alive, status, lost)
        } else {
            (None, state.run_status, false)
        };
        // A control file a surface cannot read only hides the `pausing`
        // overlay; it never fails the view.
        let pending_pause = if run_status == RunStatus::Running {
            crate::control::pending_pause(run_dir).ok().flatten()
        } else {
            None
        };
        let pause =
            crate::pause::pause_info(&events, run_status, pending_pause.as_ref(), driver_alive);
        Ok(Self {
            pause,
            state,
            release_lost,
            events,
            unknown,
            progress,
            driver_alive,
            run_status,
            liveness_checked,
        })
    }

    /// The run has a drive claim whose process is provably gone and the run
    /// has not ended: it is waiting for a resume nobody will do on their
    /// own. A finished run has nothing left to drive, so a stale claim there
    /// (a child driven in-process by its parent's drive, a driver that
    /// exited after writing the outcome) is never reported. The one "driver
    /// dead" predicate of every surface.
    pub fn driver_dead(&self) -> bool {
        self.driver_alive != Some(true)
            && !self.run_status.is_terminal()
            && (self.release_lost
                || (self.driver_alive == Some(false) && self.released().is_none()))
    }

    /// The release that holds a run nobody drives on purpose: its driver
    /// let go while only deferred retries were left, and a waker resumes it
    /// at `wake_at_ms`. `None` while a driver is live, and for a run that is
    /// paused or ended. Surfaces say "waiting to retry" instead of "driver
    /// dead" for it.
    pub fn released(&self) -> Option<&crate::state::Released> {
        self.state.released.as_ref().filter(|_| {
            !self.release_lost
                && self.driver_alive != Some(true)
                && !self.run_status.is_terminal()
                && self.run_status != RunStatus::Paused
        })
    }

    /// The run's pause: the latest `run_paused` of a paused run, or the
    /// queued pause a live driver drains toward (`pausing`). `None` for a run
    /// that is neither. The one pause accessor every surface reports from.
    pub fn pause(&self) -> Option<&crate::pause::PauseInfo> {
        self.pause.as_ref()
    }

    /// "driven by apb X, resumed by Y" from the journal's recorded binary
    /// versions (`crate::run_binaries`); `None` when no drive recorded one.
    pub fn driven_by(&self) -> Option<String> {
        crate::run_binaries::RunBinaries::from_events(&self.events).line()
    }

    /// Per-node status for live reporting (`lost` for a dead attempt,
    /// `running` for a live one the fold calls interrupted, `interrupted` for
    /// in-flight work under a provably dead driver).
    pub fn nodes(&self) -> BTreeMap<String, String> {
        if !self.liveness_checked {
            return self
                .state
                .nodes
                .iter()
                .map(|(k, v)| (k.clone(), v.as_str().to_string()))
                .collect();
        }
        crate::liveness::reported_node_statuses(&self.events, self.driver_alive)
    }

    /// `driver_alive` as a run surface reports it: `true`, `false`, `null`
    /// (nothing claims to drive the run) or `"unknown"` (a read-only
    /// process did not probe).
    pub fn driver_alive_json(&self) -> serde_json::Value {
        if self.liveness_checked {
            serde_json::json!(self.driver_alive)
        } else {
            serde_json::json!(LIVENESS_UNKNOWN)
        }
    }

    /// Token usage over the run's attempts, `None` when none reported any.
    pub fn usage(&self) -> Option<RunUsage> {
        RunUsage::from_events(&self.events)
    }

    /// Every isolated attempt with what its isolation held and did not.
    pub fn isolation(&self) -> Vec<AttemptIsolationRow> {
        attempt_isolation(&self.events)
    }

    /// Every read-only check with the changes that failed it.
    pub fn read_only(&self) -> Vec<ReadOnlyRow> {
        read_only_rows(&self.events)
    }

    /// The attempts that lost part of their output (bounded agent output).
    pub fn output_losses(&self) -> Vec<crate::capture::AttemptOutputLoss> {
        crate::capture::attempt_losses(&self.events)
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

/// The one line of a `run.previous_success` record, as `apb runs <id>`
/// and the HTML report show it (the dashboard mirrors it in
/// `previoussuccess.ts`).
pub fn previous_success_line(ps: &PreviousSuccessView) -> String {
    let mut line = match &ps.run_id {
        Some(id) => match (&ps.started_at, &ps.finished_at) {
            (Some(started), Some(finished)) => {
                format!("{id}, started {started}, finished {finished}")
            }
            (Some(started), None) => format!("{id}, started {started}"),
            _ => id.clone(),
        },
        None if ps.fallback => {
            let mut times: Vec<String> = ps.started_at.iter().cloned().collect();
            if let Some(f) = &ps.finished_at {
                times.push(format!("finished {f}"));
            }
            format!("none (window from when_none: {})", times.join(", "))
        }
        None => "none".to_string(),
    };
    if !ps.withheld.is_empty() {
        line.push_str(&format!(", withheld: {}", ps.withheld.join(", ")));
    }
    line
}

#[cfg(test)]
mod decision_tests {
    fn ev(seq: u64, payload: crate::event::EventPayload) -> Event {
        Event {
            seq,
            ts: 0,
            payload,
        }
    }

    fn keyed_record(queued: bool) -> crate::event::ConcurrencyRecord {
        crate::event::ConcurrencyRecord {
            key_digest: "d1".into(),
            display: "acme".into(),
            on_busy: "queue_latest".into(),
            queued,
        }
    }

    /// An active run's view: the record's display and mode, no holder.
    #[test]
    fn concurrency_view_of_an_active_run() {
        let events = vec![ev(
            1,
            crate::event::EventPayload::RunProvenance {
                origin: Some("project".into()),
                digest: None,
                execution_root: None,
                scripts_digest: None,
                profiles: Vec::new(),
                remembered: Vec::new(),
                apb: None,
                limits: Vec::new(),
                previous_success: None,
                untrusted: Vec::new(),
                deadline: None,
                start_key: None,
                concurrency: Some(keyed_record(false)),
            },
        )];
        let tmp = tempfile::tempdir().unwrap();
        let v = concurrency(
            tmp.path(),
            "p-1",
            apb_core::scope::Scope::Project,
            "p",
            &events,
        )
        .unwrap();
        assert_eq!(v.display, "acme");
        assert_eq!(v.on_busy, "queue_latest");
        assert_eq!(v.waiting_for, None);
        assert_eq!(concurrency_line(&v), "key \"acme\" (queue_latest)");
    }

    /// A run the slot names as waiting shows the holder it waits for.
    #[test]
    fn concurrency_view_of_a_waiter() {
        let events = vec![ev(
            1,
            crate::event::EventPayload::RunProvenance {
                origin: Some("project".into()),
                digest: None,
                execution_root: None,
                scripts_digest: None,
                profiles: Vec::new(),
                remembered: Vec::new(),
                apb: None,
                limits: Vec::new(),
                previous_success: None,
                untrusted: Vec::new(),
                deadline: None,
                start_key: None,
                concurrency: Some(keyed_record(true)),
            },
        )];
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let slot = crate::concurrency::Slot {
            key_digest: "d1".into(),
            display: "acme".into(),
            active: Some(crate::concurrency::Holder {
                run_id: "p-0".into(),
                reservation: None,
            }),
            waiting: Some(crate::concurrency::Holder {
                run_id: "p-1".into(),
                reservation: None,
            }),
            stops: Vec::new(),
        };
        let key = slot.key_digest.clone();
        let display = slot.display.clone();
        crate::concurrency::seed_for_view(root, slot);
        let v = concurrency(root, "p-1", apb_core::scope::Scope::Project, "p", &events).unwrap();
        assert_eq!(v.waiting_for.as_deref(), Some("p-0"));
        assert_eq!(
            concurrency_line(&v),
            "key \"acme\" (queue_latest), waiting for run p-0"
        );
        let _ = (key, display);
    }

    /// No record, no view.
    #[test]
    fn no_record_no_view() {
        let events = Vec::new();
        let tmp = tempfile::tempdir().unwrap();
        assert!(
            concurrency(
                tmp.path(),
                "p-1",
                apb_core::scope::Scope::Project,
                "p",
                &events
            )
            .is_none()
        );
    }

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
                item: None,
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
                edge_addresses: Default::default(),
                privacy_class: None,
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
                definition: None,
                external_data: false,
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

    /// Only attempts with an isolation record get a row; a list item names
    /// itself and a sandbox-less attempt says why.
    #[test]
    fn isolation_rows_cover_the_isolated_attempts_only() {
        let events: Vec<Event> = [
            r#"{"seq":0,"ts":1,"type":"attempt_started","node":"plain","attempt":1,"agent":"claude"}"#,
            r#"{"seq":1,"ts":2,"type":"attempt_started","node":"w","attempt":2,"agent":"claude","item":"a.md","isolation":{"level":"full","backend":"bwrap","network":"none","limits":["user.host_uid","kernel.escapes"]}}"#,
            r#"{"seq":2,"ts":3,"type":"attempt_started","node":"b","attempt":1,"agent":"claude","isolation":{"level":"best_effort","backend":"none","network":"open","limits":["no_sandbox"],"fallbacks":["bwrap_missing (install bubblewrap (bwrap))"]}}"#,
        ]
        .iter()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
        let rows = attempt_isolation(&events);
        assert_eq!(rows.len(), 2);
        assert!(rows[0].sandboxed());
        assert_eq!(
            rows[0].line(),
            "node `w` item `a.md` attempt 2: full on bwrap, network none; limits: user.host_uid, kernel.escapes"
        );
        assert!(!rows[1].sandboxed());
        assert!(rows[1].line().ends_with(
            "limits: no_sandbox; no sandbox: bwrap_missing (install bubblewrap (bwrap))"
        ));
    }

    /// An attempt's row comes from its `read_only_checked`, its violation
    /// folds into it; a violation without a check (a journal cut between
    /// the two) is a row of its own; an attempt without a check gets none.
    #[test]
    fn read_only_rows_join_the_start_and_the_violation() {
        let events: Vec<Event> = [
            r#"{"seq":0,"ts":1,"type":"attempt_started","node":"plain","attempt":1,"agent":"claude"}"#,
            r#"{"seq":1,"ts":2,"type":"read_only_checked","node":"look","attempt":1,"check":{"tier":"enforced"}}"#,
            r#"{"seq":2,"ts":3,"type":"read_only_checked","node":"scan","attempt":2,"check":{"tier":"detected_partial","entries":20000,"bytes":512,"partial":"entries"}}"#,
            r#"{"seq":3,"ts":4,"type":"read_only_violated","node":"scan","attempt":2,"tier":"detected_partial","changes":[{"path":"a.txt","change":"modified"}],"evidence":"read_only/scan/2.json"}"#,
            r#"{"seq":4,"ts":5,"type":"read_only_violated","node":"sh","attempt":1,"changes":[{"path":".env","change":"added"},{"path":"b","change":"deleted"}],"truncated":3}"#,
        ]
        .iter()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
        let rows = read_only_rows(&events);
        assert_eq!(rows.len(), 3);
        assert_eq!(
            rows[0].line(),
            "node `look` attempt 1: enforced (the sandbox binds the tree read-only)"
        );
        assert_eq!(
            rows[1].line(),
            "node `scan` attempt 2: detected_partial (checked after the attempt over 20000 entries, 512 bytes; the entries limit stopped it short, so a change past it goes unseen); changed the tree (read_only_dirty), 1 path: a.txt modified; evidence read_only/scan/2.json"
        );
        assert_eq!(rows[2].tier_name(), "detected");
        assert!(
            rows[2].line().ends_with(
                "changed the tree (read_only_dirty), 5 paths: .env added, b deleted, 3 more"
            ),
            "{}",
            rows[2].line()
        );
        let one: Event = serde_json::from_str(
            r#"{"seq":5,"ts":6,"type":"read_only_checked","node":"one","attempt":1,"check":{"tier":"detected","entries":1,"bytes":1}}"#,
        )
        .unwrap();
        assert_eq!(
            read_only_rows(&[one])[0].line(),
            "node `one` attempt 1: detected (checked after the attempt over 1 entry, 1 byte)"
        );
        let json = serde_json::to_value(&rows[1]).unwrap();
        assert_eq!(json["tier"], "detected_partial");
        assert_eq!(json["partial"], "entries");
        assert_eq!(json["violated"]["changes"][0]["path"], "a.txt");
    }

    /// A violation of a batch member names the peers it ran beside in the
    /// line; without any, the line is exactly the sequential one.
    #[test]
    fn a_read_only_line_names_the_peers_it_ran_beside() {
        let mut row = ReadOnlyRow {
            node: "scan".into(),
            item: None,
            attempt: 2,
            check: crate::event::ReadOnlyCheck {
                tier: crate::event::ReadOnlyTier::Detected,
                entries: 3,
                bytes: 40,
                partial: None,
            },
            violated: Some(ReadOnlyViolation {
                changes: vec![crate::event::ReadOnlyChange {
                    path: "a.txt".into(),
                    change: "modified".into(),
                }],
                truncated: 0,
                evidence: None,
                concurrent_with: Vec::new(),
            }),
        };
        assert_eq!(
            row.line(),
            "node `scan` attempt 2: detected (checked after the attempt over 3 entries, 40 bytes); changed the tree (read_only_dirty), 1 path: a.txt modified"
        );
        row.violated.as_mut().unwrap().concurrent_with =
            vec!["write".to_string(), "other".to_string()];
        assert_eq!(
            row.line(),
            "node `scan` attempt 2: detected (checked after the attempt over 3 entries, 40 bytes); the tree changed while it ran beside write, other (read_only_dirty), 1 path: a.txt modified"
        );
    }

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

    /// A released run whose wake time passed long ago with no waker holding
    /// its lease and no driver is not "waiting to retry": nothing will
    /// resume it, so it reads interrupted with its driver dead. One whose
    /// wake is ahead still waits, and so does a late one whose waker holds
    /// its lease (retrying behind a busy tree, say).
    #[test]
    fn a_release_long_overdue_with_no_waker_reads_driver_dead() {
        let tmp = tempfile::tempdir().unwrap();
        let now = apb_core::clock::now_ms_u64();
        for (id, wake_at_ms, waker, lost) in [
            ("late", now - 600_000, false, true),
            ("ahead", now + 600_000, false, false),
            ("late-held", now - 600_000, true, false),
        ] {
            let run_dir = tmp.path().join(".apb/runs").join(id);
            write_run(
                &run_dir,
                &[
                    EventPayload::RunStarted {
                        playbook: "p".into(),
                        version: "1.0.0".into(),
                        definition: None,
                        external_data: false,
                    },
                    EventPayload::RetryDeferred {
                        node: "work".into(),
                        after_attempt: 1,
                        retry: 1,
                        step: 0,
                        binding: "b".repeat(64),
                        delay_ms: 1_000,
                        failure_kind: None,
                    },
                    EventPayload::DriveReleased {
                        wake_at_ms,
                        nodes: vec!["work".into()],
                    },
                ],
                reaped_pid(),
            );
            let _lease = waker.then(|| {
                let seq = RunState::fold(&crate::event::read_all(&run_dir).unwrap())
                    .released
                    .unwrap()
                    .seq;
                crate::waker::Lease::try_take(&run_dir, crate::waker::WakerFor::Release(seq))
                    .unwrap()
                    .unwrap()
            });
            let view = RunView::load(&run_dir, id).unwrap();
            assert_eq!(view.released().is_none(), lost, "{id}");
            assert_eq!(view.driver_dead(), lost, "{id}");
            let status = if lost {
                RunStatus::Interrupted
            } else {
                RunStatus::Running
            };
            assert_eq!(view.run_status, status, "{id}");
        }
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
                    definition: None,
                    external_data: false,
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
                    item: None,
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
                    token: None,
                    isolation: None,
                    exec: None,
                    cloud: None,
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
        assert!(view.driver_dead());
    }

    /// A finished run whose drive claim names a process that is gone (a
    /// sub-playbook child its parent drove in-process) is not "driver dead":
    /// nothing is left to drive.
    #[test]
    fn a_finished_run_is_never_driver_dead() {
        let dir = tempfile::tempdir().unwrap();
        let run_dir = dir.path().join("r1");
        write_run(
            &run_dir,
            &[
                EventPayload::RunStarted {
                    playbook: "p".into(),
                    version: "1.0.0".into(),
                    definition: None,
                    external_data: false,
                },
                EventPayload::RunFinished {
                    outcome: "succeeded".into(),
                },
            ],
            reaped_pid(),
        );
        let view = RunView::load(&run_dir, "r1").unwrap();
        assert_eq!(view.driver_alive, Some(false));
        assert_eq!(view.run_status, RunStatus::Succeeded);
        assert!(!view.driver_dead());
    }

    /// The three line forms of a deadline view.
    #[test]
    fn the_deadline_line_has_three_forms() {
        let v = |remaining_ms: u64, ends_at: Option<&str>| RunDeadlineView {
            total_ms: 45 * 60_000,
            elapsed_ms: 45 * 60_000 - remaining_ms,
            remaining_ms,
            source: crate::event::DeadlineSource::Playbook,
            frozen: ends_at.is_none(),
            ends_at: ends_at.map(str::to_string),
        };
        assert_eq!(
            deadline_line(&v(12 * 60_000, Some("2026-11-10T10:00:00Z"))),
            "45m, 12m left (ends 2026-11-10T10:00:00Z)"
        );
        assert_eq!(
            deadline_line(&v(12 * 60_000, None)),
            "45m, 12m left, frozen (paused by a person)"
        );
        assert_eq!(deadline_line(&v(0, None)), "45m, exceeded");
    }

    /// A run without a deadline has no view; one paused by a person is
    /// frozen with no end time.
    #[test]
    fn a_persons_pause_freezes_the_view() {
        let ev = |seq: u64, ts: u128, p: crate::event::EventPayload| Event {
            seq,
            ts,
            payload: p,
        };
        let started = ev(
            0,
            0,
            crate::event::EventPayload::RunStarted {
                playbook: "p".into(),
                version: "1".into(),
                definition: None,
                external_data: false,
            },
        );
        assert!(deadline(std::slice::from_ref(&started), 10).is_none());
        let prov: crate::event::EventPayload = serde_json::from_value(serde_json::json!({
            "type": "run_provenance", "deadline": { "total_ms": 60000, "source": "run" }
        }))
        .unwrap();
        let paused = crate::event::EventPayload::RunPaused {
            reason: "x".into(),
            by: Some(crate::pause::kind::PauseBy::Operator),
            mode: None,
            until: None,
            keep_lock: false,
        };
        let events = vec![started, ev(1, 0, prov), ev(2, 10_000, paused)];
        let v = deadline(&events, 30_000).unwrap();
        assert!(v.frozen && v.ends_at.is_none());
        assert_eq!(v.remaining_ms, 50_000);
    }
}
