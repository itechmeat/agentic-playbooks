//! The pilot steps of a list visit: which items run before the pilot gate
//! ([`step`], called by the batch runner), and whether the visit's pilot
//! gate is due in the drive ([`gate_due`], the drive's own check). The
//! gate itself is an ordinary review request on the list node, decided
//! through the review channel like any gate's.

use apb_core::list::verdict::ItemState;

use crate::error::EngineError;
use crate::event::{Event, EventPayload, read_all};
use crate::list::ledger::{
    ItemRecord, ListFold, PILOT_REVISE_REASON, PilotPhase, pilot_phase, settled,
};

use super::run::{ListRun, Settlement};
use super::sends::ItemSends;

/// What the runner does with the visit's pilot.
pub(super) enum Step {
    /// Run these ids of the latest slice (the pending ones); the visit
    /// stays inside the slice until it settles.
    Slice(Vec<String>),
    /// The gate is due or open: the runner returns and the drive asks.
    Wait,
    /// The visit ends early: the pilot gate was answered stop.
    Stop,
    /// No pilot, or it passed: the rest of the visit runs.
    Go,
}

/// The runner's pilot step for a visit at `fold`, under `spec`.
pub(super) fn step(
    run: &ListRun<'_, '_, '_>,
    spec: &apb_core::list::PilotSpec,
    fold: &ListFold,
    sends: &ItemSends,
) -> Result<Step, EngineError> {
    match pilot_phase(spec, fold) {
        PilotPhase::Start => {
            // The first `spec.items` ids that still need to run, in source
            // order.
            let ids: Vec<String> = fold
                .remaining(run.items.iter().map(|i| i.id.as_str()))
                .into_iter()
                .take(spec.items as usize)
                .map(str::to_string)
                .collect();
            if ids.is_empty() {
                return Ok(Step::Go);
            }
            run.ctx.journal.append(EventPayload::ListPilot {
                node: run.node.id.clone(),
                round: 1,
                items: ids.clone(),
            })?;
            Ok(Step::Slice(ids))
        }
        PilotPhase::Running => {
            let pilot = fold.pilot.as_ref().expect("a running pilot has a slice");
            let ids: Vec<String> = fold
                .remaining(pilot.items.iter().map(String::as_str))
                .into_iter()
                .map(str::to_string)
                .collect();
            Ok(Step::Slice(ids))
        }
        PilotPhase::Gate => Ok(Step::Wait),
        PilotPhase::Revise => {
            let pilot = fold.pilot.as_ref().expect("a revise has a slice");
            for id in &pilot.items {
                // An item whose send has an unknown outcome is never
                // interrupted: it keeps its `needs_look` record and stays
                // in the slice as such, its send never repeated until a
                // person resolves it.
                if sends.unknown(run.key(id)).is_some() {
                    continue;
                }
                // An interrupt a crashed drive already wrote needs no
                // second one: the transition resumes once.
                if fold
                    .items
                    .get(id)
                    .is_some_and(|r| r.state == Some(ItemState::Pending))
                {
                    continue;
                }
                run.settle(Settlement {
                    id,
                    state: settled::INTERRUPTED,
                    reason: Some(PILOT_REVISE_REASON),
                    round: 0,
                    ..Settlement::default()
                })?;
            }
            run.ctx.journal.append(EventPayload::ListPilot {
                node: run.node.id.clone(),
                round: pilot.round + 1,
                items: pilot.items.clone(),
            })?;
            // The queue of the new round holds the pending ids only: the
            // unknown-send item keeps its needs-look record, the passed
            // ones keep theirs.
            let events = read_all(run.ctx.run_dir)?;
            let fresh = ListFold::of(&events, &run.node.id).unwrap_or_default();
            let ids: Vec<String> = fresh
                .remaining(pilot.items.iter().map(String::as_str))
                .into_iter()
                .map(str::to_string)
                .collect();
            Ok(Step::Slice(ids))
        }
        PilotPhase::Stop => {
            if fold.stopped.is_none() {
                let pilot = fold.pilot.as_ref().expect("a stop has a slice");
                let total = run.items.len() as u32;
                let ran: u32 = pilot
                    .items
                    .iter()
                    .filter(|id| fold.items.get(*id).is_some_and(ItemRecord::is_terminal))
                    .count() as u32;
                let rest = total.saturating_sub(ran);
                run.ctx.journal.append(EventPayload::ListStopped {
                    node: run.node.id.clone(),
                    reason: "pilot".into(),
                    detail: format!(
                        "the pilot gate was answered stop; {rest} of {total} items did not run"
                    ),
                })?;
            }
            Ok(Step::Stop)
        }
        PilotPhase::Done => Ok(Step::Go),
    }
}

/// The request of a pilot gate: the three options, the line that says
/// where the slice stands, and, in an unattended run, the `continue` the
/// engine decides. Never a form detail: the pilot gate is a plain gate.
pub(crate) fn request(
    playbook: &apb_core::schema::Playbook,
    node: &str,
    gate: &PilotGate,
    auto: Option<super::super::review_gate::AutoDecision>,
) -> super::super::review_gate::GateRequest {
    use super::super::review_gate::GateRequest;
    let options: Vec<String> = apb_core::list::PILOT_OPTIONS
        .iter()
        .map(|s| (*s).to_string())
        .collect();
    let pilot = gate.fold.pilot.as_ref().expect("a gate due has a slice");
    let k = pilot.items.len();
    let total = gate.fold.total as usize;
    let passed = pilot
        .items
        .iter()
        .filter(|id| {
            gate.fold
                .items
                .get(*id)
                .is_some_and(|r| r.state == Some(ItemState::Passed))
        })
        .count();
    let needs_look = gate.fold.pilot_needs_look();
    let look = match needs_look {
        1 => "1 needs a look".to_string(),
        n => format!("{n} need a look"),
    };
    let rest = total.saturating_sub(k);
    let line = format!(
        "Pilot of list `{node}`: {k} of {total} items ran, {passed} passed, {look}. continue runs the other {rest}; revise runs these {k} again with your note in their prompt; stop ends the list with {rest} not run."
    );
    let title = playbook.node(node).and_then(|n| n.title.clone());
    let instruction = crate::people::instruction::review_instruction_for(
        node,
        title.as_deref(),
        &options,
        Some(&line),
        false,
    );
    GateRequest {
        event: EventPayload::ReviewRequested {
            node: node.to_string(),
            options,
            title,
            instruction,
            prompt: None,
            recommendation: None,
            plan_digest: None,
            binding: None,
            person_only: false,
        },
        auto,
        detail: crate::review::form::GateDetail::default(),
    }
}

/// The pilot gate of a visit: its spec and the fold the decision is read
/// from.
pub(crate) struct PilotGate {
    pub(crate) spec: apb_core::list::PilotSpec,
    pub(crate) fold: ListFold,
}

/// Whether the drive must ask at `node`'s pilot gate: the node has a
/// `list.pilot`, its latest visit is open, and the visit's pilot stands at
/// its gate.
pub(crate) fn gate_due(
    playbook: &apb_core::schema::Playbook,
    events: &[Event],
    node: &str,
) -> Option<PilotGate> {
    let spec = playbook.node(node)?.list()?.pilot.as_ref().copied()?;
    let fold = ListFold::of(events, node)?;
    if fold.finished {
        return None;
    }
    (pilot_phase(&spec, &fold) == PilotPhase::Gate).then_some(PilotGate { spec, fold })
}
