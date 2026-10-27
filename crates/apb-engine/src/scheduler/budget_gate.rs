//! The drive side of the run budget (see `crate::budget`): before a gated
//! node starts a new visit, and before each batch of a list node, the gate
//! weighs what is left against the node's median spend from the run's
//! estimate snapshot, journals any decision other than a plain admission
//! (`budget_gate`), and hands back the engine pause a refusal asks for. The
//! caller posts that pause on the run's control channel (or, for a list,
//! returns it), so it is applied exactly like a quota pause.
//!
//! A concurrent batch reserves each admitted member's median until the
//! member reports back, so parallel members do not each pass against the
//! same remaining amount.

use std::collections::BTreeMap;
use std::path::Path;

use apb_core::budget::BudgetRequest;
use apb_core::schema::{Node, Playbook};

use super::journal::{Journal, node_finished_count, node_started_count};
use crate::budget::gate::{GateInput, Need, Verdict, decide};
use crate::budget::kind::{BudgetAmount, GateBasis, GateDecision};
use crate::budget::{BudgetFold, Spent};
use crate::error::EngineError;
use crate::event::{Event, EventLog, EventPayload, read_all};
use crate::pause::PauseRequest;
use crate::run_estimate::{EstimateTarget, RunEstimate, snapshot};

/// What the gate decided for the caller.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Admission {
    Go,
    /// Pause the run with this engine pause (already journaled as a
    /// `budget_gate paused`).
    Pause(PauseRequest),
}

/// The medians of the admitted batch members still running.
#[derive(Debug, Default)]
pub(crate) struct Reservations(BTreeMap<String, BudgetAmount>);

impl Reservations {
    fn reserve(&mut self, node: &str, need: BudgetAmount) {
        self.0.insert(node.to_string(), need);
    }

    /// A member reported back: its spend is in the journal now.
    pub(crate) fn release(&mut self, node: &str) {
        self.0.remove(node);
    }

    fn total(&self) -> BudgetAmount {
        self.0
            .values()
            .fold(BudgetAmount::default(), |acc, a| acc.plus(a))
    }
}

/// The gate of one drive: the run's estimate snapshot, and whether the run
/// has a budget at all (else every admission is a plain one).
pub(crate) struct DriveGate {
    estimate: Option<RunEstimate>,
    active: bool,
}

/// Where a gate decision stands.
struct Site<'a> {
    node: &'a str,
    batch: Option<u32>,
    items: Option<u32>,
}

impl DriveGate {
    /// The gate for the run at `run_dir`, as its journal stands.
    pub(crate) fn load(run_dir: &Path) -> Result<Self, EngineError> {
        let active = BudgetFold::of(&read_all(run_dir)?).is_some();
        Ok(DriveGate {
            estimate: active.then(|| snapshot::read(run_dir)).flatten(),
            active,
        })
    }

    /// Before `node` starts a new visit (on the sequential arm, or as a
    /// member of a concurrent batch). A node that is not gated, a list node
    /// (gated per batch), and a visit already open (an interactive node
    /// re-entering the loop) are admitted as they are. On admission the
    /// node's median goes into `reserved` until the caller releases it.
    pub(crate) fn admit_node(
        &self,
        run_dir: &Path,
        journal: &Journal<'_>,
        node: &Node,
        reserved: &mut Reservations,
    ) -> Result<Admission, EngineError> {
        if !self.gates(node) || node.list().is_some() {
            return Ok(Admission::Go);
        }
        let events = read_all(run_dir)?;
        if node_started_count(&events, &node.id) > node_finished_count(&events, &node.id) {
            return Ok(Admission::Go);
        }
        let need = self.need(&node.id, 1.0);
        let site = Site {
            node: &node.id,
            batch: None,
            items: None,
        };
        let admission = self.decide_at(run_dir, journal, &events, &site, need, reserved.total())?;
        if admission == Admission::Go
            && let Need::Known(n) = need
        {
            reserved.reserve(&node.id, n);
        }
        Ok(admission)
    }

    /// Before batch `batch` (`items` items) of list node `node`: the need
    /// is the median per item times the batch's items. A list node that is
    /// not gated (a `script` list) is admitted as it is.
    pub(crate) fn admit_batch(
        &self,
        run_dir: &Path,
        journal: &Journal<'_>,
        node: &Node,
        batch: u32,
        items: usize,
    ) -> Result<Admission, EngineError> {
        if !self.gates(node) {
            return Ok(Admission::Go);
        }
        let events = read_all(run_dir)?;
        let need = self.need(&node.id, items as f64);
        let site = Site {
            node: &node.id,
            batch: Some(batch),
            items: Some(items as u32),
        };
        self.decide_at(
            run_dir,
            journal,
            &events,
            &site,
            need,
            BudgetAmount::default(),
        )
    }

    /// Whether the gate weighs `node` at all: the run has a budget and the
    /// node spends agent tokens ([`apb_core::schema::NodeKind::budget_gated`]),
    /// one rule for a single visit and a list's batches alike, so a `script`
    /// list node, which runs no agent, is never gated.
    fn gates(&self, node: &Node) -> bool {
        self.active && node.kind.budget_gated()
    }

    /// The node's median spend from the snapshot, per visit (a list node:
    /// per item, times `scale` items).
    fn need(&self, node: &str, scale: f64) -> Need {
        let Some(f) = self
            .estimate
            .as_ref()
            .and_then(|e| e.node(node))
            .map(|n| n.gate_figure())
        else {
            return Need::Unknown;
        };
        if f.tokens.is_none() && f.cost_usd.is_none() {
            return Need::Unknown;
        }
        Need::Known(BudgetAmount {
            tokens: f.tokens.map(|t| t.p50 as f64 * scale),
            usd: f.cost_usd.map(|c| c.p50 * scale),
        })
    }

    fn decide_at(
        &self,
        run_dir: &Path,
        journal: &Journal<'_>,
        events: &[Event],
        site: &Site<'_>,
        need: Need,
        reserved: BudgetAmount,
    ) -> Result<Admission, EngineError> {
        let Some(fold) = BudgetFold::of(events) else {
            return Ok(Admission::Go);
        };
        let spent = Spent::of(run_dir, events, self.estimate.as_ref());
        let verdict = decide(&GateInput {
            fold: &fold,
            spent: &spent,
            need,
            reserved,
            waived: fold.waiver.as_deref() == Some(site.node),
            executor_drift: executor_drift(events, site.node),
        });
        let need_amount = match need {
            Need::Known(n) => Some(n),
            Need::Unknown => None,
        };
        let record = |decision, basis, remaining| EventPayload::BudgetGate {
            node: site.node.to_string(),
            batch: site.batch,
            items: site.items,
            decision,
            basis,
            need: need_amount,
            remaining,
            spent: spent.for_limit(&fold.limit),
            by: None,
        };
        match verdict {
            Verdict::Admit => Ok(Admission::Go),
            Verdict::AdmitWaived => {
                journal.append(record(
                    GateDecision::AdmittedWaived,
                    GateBasis::Operator,
                    None,
                ))?;
                Ok(Admission::Go)
            }
            Verdict::Continue(basis) => {
                journal.append(record(GateDecision::Continued, basis, None))?;
                Ok(Admission::Go)
            }
            Verdict::Pause {
                basis,
                need,
                remaining,
            } => {
                journal.append(record(
                    GateDecision::Paused,
                    basis,
                    (!remaining.is_empty()).then_some(remaining),
                ))?;
                let reason = crate::pause::budget::budget_reason(
                    site.node,
                    basis,
                    need.as_ref(),
                    &remaining,
                );
                Ok(Admission::Pause(crate::pause::budget::budget_pause(reason)))
            }
        }
    }
}

/// Whether `node`'s executor drifted in this run: an execution fallback to
/// the host, or a fallback inside its latest visit. Its history then does
/// not estimate it.
fn executor_drift(events: &[Event], node: &str) -> bool {
    let mut in_latest = false;
    let mut any_host = false;
    for e in events {
        match &e.payload {
            EventPayload::NodeStarted { node: n, .. } if n == node => in_latest = false,
            EventPayload::FallbackTriggered { node: n, .. } if n == node => in_latest = true,
            EventPayload::ExecutionFallback { node: n, .. } if n == node => any_host = true,
            _ => {}
        }
    }
    any_host || in_latest
}

/// What a top-level run's start records about its budget.
pub(crate) struct StartRecord<'a> {
    pub(crate) run_dir: &'a Path,
    pub(crate) root: &'a Path,
    pub(crate) playbook: &'a Playbook,
    pub(crate) manifest: &'a crate::manifest::RunExecutionManifest,
    /// The run's budget argument.
    pub(crate) budget: Option<&'a BudgetRequest>,
    /// The estimate a launch surface showed before the start.
    pub(crate) shown: Option<&'a RunEstimate>,
    /// Run overrides changed the executors: the shown estimate was for the
    /// version's own bindings, so the snapshot is computed from the manifest.
    pub(crate) overridden: bool,
}

/// At the start of a top-level run (a sub-playbook child has neither):
/// journals the budget in force, if any, and writes the run's estimate
/// snapshot when the run has a budget or a surface showed an estimate (that
/// one, as the person saw it, unless run overrides changed the executors;
/// else computed now). The snapshot is best
/// effort: a failed write never fails the start, and without one the gate
/// treats every node as unestimated.
pub(crate) fn record_start(log: &mut EventLog, r: &StartRecord<'_>) -> Result<(), EngineError> {
    let eff = crate::budget::record_start(log, r.playbook.budget.as_ref(), r.budget)?;
    if eff.is_none() && r.shown.is_none() {
        return Ok(());
    }
    let est = match r.shown.filter(|_| !r.overridden) {
        Some(e) => e.clone(),
        None => {
            let run_id = r
                .run_dir
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or_default();
            let target = EstimateTarget::from_manifest(r.playbook, Some(r.manifest), run_id);
            crate::run_estimate::estimate(&[r.root.to_path_buf()], &target)
        }
    };
    if let Err(e) = snapshot::write(r.run_dir, &est) {
        eprintln!("apb: warning: the run's estimate was not saved: {e}");
    }
    Ok(())
}
