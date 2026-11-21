//! The list runner: a node with `list:` runs its step once per item, in
//! batches, checks each item and settles it (see `apb_core::list::verdict`).
//!
//! A visit loads its items ([`source`]), decides whether it resumes an open
//! visit, reopens a failed one (a supervisor retry) or starts a new one,
//! renders the node's prompt once for every item, runs the batches
//! ([`batch`]) and composes the node output from the journal's record of
//! the visit (`crate::list::ListFold`). A list node always runs on the
//! drive's sequential arm; its items run on its own worker pool, at most
//! `min(batch.parallel, max_parallel)` at a time.

mod batch;
mod check;
pub(super) mod pilot;
mod run;
mod sends;
mod source;

use std::collections::BTreeSet;

use apb_core::list::items::{item_bytes, items_digest};
use apb_core::list::verdict::{ItemState, NeedsLookReason};
use apb_core::list::{ITEM_MAX_BYTES, RerunPolicy};
use apb_core::schema::NodeKind;
use serde_json::{Value, json};

pub(crate) use super::outcome::ItemExec;
use super::outcome::{AttemptOutcome, ExecOptions, NodeCtx};
use crate::error::EngineError;
use crate::event::{Event, EventPayload, read_all, supervisor_action};
use crate::list::ledger::{ItemRecord, ListFold, settled};
use crate::state::NodeStatus;
use run::{ListRun, Settlement};

/// Runs list node `node_id` (its whole visit).
pub(crate) fn execute(
    ctx: &NodeCtx<'_, '_>,
    node_id: &str,
    opts: &ExecOptions<'_>,
) -> Result<AttemptOutcome, EngineError> {
    let node = ctx
        .playbook
        .node(node_id)
        .ok_or_else(|| EngineError::NotFound(node_id.into()))?;
    let spec = node
        .list()
        .ok_or_else(|| EngineError::Invalid(format!("node `{node_id}` is not a list node")))?;
    let loaded = match source::load(ctx.playbook, ctx.state, spec, ctx.run_dir, node_id) {
        Ok(l) => l,
        Err(why) => {
            return Ok(AttemptOutcome::Finished {
                status: NodeStatus::Failed,
                output: format!("list `{node_id}`: {why}"),
                events: Vec::new(),
            });
        }
    };
    let template = match &node.kind {
        NodeKind::AgentTask { prompt, .. } => {
            super::node::journal_missing_inputs(
                ctx.journal,
                ctx.run_dir,
                node_id,
                prompt,
                ctx.state,
            )?;
            super::node::render_node_prompt(
                ctx.playbook,
                ctx.run_dir,
                ctx.run_id,
                ctx.state,
                ctx.cfg,
                prompt,
                &ctx.playbook.context_budget(node_id),
            )?
        }
        _ => String::new(),
    };
    let mut run = ListRun {
        ctx,
        node,
        spec,
        items: loaded.items,
        template,
        stop: opts.stop,
        control_after: opts.control_after,
        budget: opts.budget,
        workers: spec.workers(opts.max_parallel),
        root: super::judge::cfg_root(ctx.run_dir),
        spawn_env: ctx.cfg.spawn_env(),
        earlier: None,
        skip_allowed: std::sync::atomic::AtomicBool::new(false),
        work: std::sync::OnceLock::new(),
        pin: std::sync::OnceLock::new(),
        stamp_block: std::sync::OnceLock::new(),
        scripts_diverged: std::sync::atomic::AtomicBool::new(false),
        latch_journaled: std::sync::atomic::AtomicBool::new(false),
        execution: std::sync::OnceLock::new(),
        pins_diverged: std::sync::atomic::AtomicBool::new(false),
        runtimes_diverged: std::sync::atomic::AtomicBool::new(false),
    };
    let events = open_visit(&run, loaded.filtered_out)?;
    // Under `rerun: not_passed` an item run again in a later visit is told
    // what it reported and what its check said the visit before.
    if spec.rerun == RerunPolicy::NotPassed {
        run.earlier = ListFold::previous(&events, node_id);
    }
    let outcome = batch::run_batches(&run)?;
    match outcome {
        batch::End::Done => compose(&run),
        batch::End::Cancelled => Ok(AttemptOutcome::Finished {
            status: NodeStatus::Cancelled,
            output: "cancelled".to_string(),
            events: Vec::new(),
        }),
        // The visit stays open like a halted one; the drive reads the
        // gate's refusal and ends the run.
        batch::End::Halted | batch::End::Exhausted => {
            Ok(AttemptOutcome::Halted { events: Vec::new() })
        }
        batch::End::Pause(pause) => Ok(AttemptOutcome::EnginePaused {
            pause,
            events: Vec::new(),
        }),
        // The pilot slice settled and a person decides: no `node_finished`,
        // the drive goes back to the loop top, where the pilot gate branch
        // takes it.
        batch::End::PilotGate => Ok(AttemptOutcome::Halted { events: Vec::new() }),
        // The visit ended early: the node fails with `stopped` in its
        // output, whatever `on_needs_look` says.
        batch::End::Stopped => compose(&run),
    }
}

/// Decides the visit and journals its start: resume an open visit of the
/// same items, reopen a failed one a supervisor retried, or start a new one
/// (carrying items that passed last time under `rerun: not_passed`). An
/// item whose earlier send's outcome is still unknown never runs again
/// until a person settles that send. Returns the journal as it read it once
/// the visit's start is in it (the visits before it are as they stand).
fn open_visit(run: &ListRun<'_, '_, '_>, filtered_out: u32) -> Result<Vec<Event>, EngineError> {
    let node = &run.node.id;
    let events = read_all(run.ctx.run_dir)?;
    let digest = items_digest(&run.items);
    let fold = ListFold::of(&events, node);
    let same = fold.as_ref().filter(|f| f.items_digest == digest);
    if same.is_some_and(ListFold::is_open) {
        // A drive that died between the visit's `list_started` and its
        // first batch or pilot slice may have left the opening steps
        // undone: they are completed before anything runs, against the
        // journal as the first pass read it.
        run.skip_allowed.store(
            run.spec.skip.is_some() && same.is_some_and(|f| f.skip_refused.is_none()),
            std::sync::atomic::Ordering::SeqCst,
        );
        complete_opening(run, &events)?;
        recheck_skipped(run)?;
        let events = read_all(run.ctx.run_dir)?;
        settle_too_large(run, ListFold::of(&events, node).as_ref())?;
        return read_all(run.ctx.run_dir);
    }
    let reopen = same.is_some_and(|f| retried_after_failure(&events, node, f.started_seq));
    let sends = sends::ItemSends::of(&events, node);
    // A new visit of a list with `skip` decides once, on the state at its
    // start, whether it skips; its opening steps and their recovery read
    // the decision from its `list_started`. A reopen keeps its visit's.
    let skip_refused = if run.spec.skip.is_some() && !reopen {
        skip_decision(run, &events)?
    } else {
        None
    };
    run.skip_allowed.store(
        run.spec.skip.is_some()
            && if reopen {
                same.is_some_and(|f| f.skip_refused.is_none())
            } else {
                skip_refused.is_none()
            },
        std::sync::atomic::Ordering::SeqCst,
    );
    run.ctx.journal.append(EventPayload::ListStarted {
        node: node.clone(),
        total: run.items.len() as u32,
        filtered_out,
        items_digest: digest,
        batch_size: run.spec.batch.size.max(1),
        parallel: run.workers as u32,
        reopened: reopen,
        skip_refused: skip_refused.clone(),
    })?;
    let set = crate::list::skip::recorded(&events, node);
    opening_steps(
        run,
        reopen,
        fold.as_ref(),
        &sends,
        &BTreeSet::new(),
        Skip {
            set: set.as_ref(),
            refused: skip_refused.as_deref(),
        },
    )?;
    if reopen {
        recheck_skipped(run)?;
    }
    let events = read_all(run.ctx.run_dir)?;
    settle_too_large(run, ListFold::of(&events, node).as_ref())?;
    Ok(events)
}

/// A skipped row is valid only under the work it was skipped with. When a
/// visit is continued (a resume, a drive after a crash) or reopened (a
/// supervisor retry after a patch or a rebind), every row the visit holds
/// as skipped is checked again: when the visit may not hold skips at all
/// ([`visit_refusal`], the one rule a new visit is decided on too), every
/// skipped row goes back to pending with that refusal's reason
/// (`skip_supervisor_note` for a note in force, else
/// `skip_config_changed`); otherwise a row whose skip digest no longer
/// matches the work as this execution resolves it goes back to pending
/// with `skip_config_changed`. Rows the visit actually worked are left as
/// they are, as on main.
fn recheck_skipped(run: &ListRun<'_, '_, '_>) -> Result<(), EngineError> {
    use crate::list::skip as s;
    if run.spec.skip.is_none() {
        return Ok(());
    }
    let events = read_all(run.ctx.run_dir)?;
    let Some(fold) = ListFold::of(&events, &run.node.id) else {
        return Ok(());
    };
    let refused = visit_refusal(run, &events).map(|(reason, _)| reason);
    let work = run.work();
    for (id, rec) in &fold.items {
        if rec.skipped_from.is_none() || rec.state != Some(ItemState::Passed) {
            continue;
        }
        let holds = refused.is_none()
            && match (&work, run.item(id)) {
                (Ok(w), Some(item)) => {
                    rec.skip_digest.as_deref()
                        == Some(s::skip_digest(id, &run.item_part(item), w).as_str())
                }
                _ => false,
            };
        if !holds {
            run.settle(Settlement {
                id,
                state: settled::INTERRUPTED,
                reason: Some(refused.unwrap_or(s::SKIP_CONFIG_CHANGED)),
                round: rec.round,
                ..Settlement::default()
            })?;
        }
    }
    Ok(())
}

/// Why a new visit of a list with `skip` skips nothing, decided on the
/// state at its start: the set's refusal, then [`visit_refusal`]. `None`:
/// the visit may skip. A node with no recorded set refuses the visit.
fn skip_decision(
    run: &ListRun<'_, '_, '_>,
    events: &[Event],
) -> Result<Option<String>, EngineError> {
    use crate::list::skip;
    let Some(set) = skip::recorded(events, &run.node.id) else {
        return Err(no_set(run));
    };
    if let Some(refused) = set.refused {
        return Ok(Some(refused));
    }
    Ok(visit_refusal(run, events).map(|(_, why)| why))
}

/// The one rule whether a visit may hold skips, on the journal as it
/// stands, for a new visit ([`skip_decision`]) and a continued or reopened
/// one ([`recheck_skipped`]) alike: a field `unless_changed` names that no
/// item has, the work that does not read, scripts that differ from the
/// pin, a supervisor note in force. `Some((reason, why))`: the visit may
/// not, with the settlement reason a retained skipped row goes back to
/// pending with and the sentence a new visit records.
fn visit_refusal(run: &ListRun<'_, '_, '_>, events: &[Event]) -> Option<(&'static str, String)> {
    use crate::list::skip;
    let changed = |why: String| Some((skip::SKIP_CONFIG_CHANGED, why));
    // A field `unless_changed` names that no item of the visit has (a
    // misspelled name) would make every item's part constant: the visit
    // skips nothing.
    if let Some(fields) = run
        .spec
        .skip
        .as_ref()
        .and_then(|s| s.unless_changed.as_ref())
        && let Some(f) = fields
            .iter()
            .find(|f| !run.items.iter().any(|i| i.value.get(f.as_str()).is_some()))
    {
        return changed(format!(
            "{}: no item of the visit has the field `{f}` unless_changed names, so every item works",
            skip::UNLESS_CHANGED_ABSENT
        ));
    }
    if let Err(why) = run.work() {
        return changed(why);
    }
    if !run.scripts_pinned() {
        return changed(format!(
            "{}: the run's scripts copy differs from the one pinned at start, so every item works",
            skip::SCRIPTS_CHANGED
        ));
    }
    if !crate::context::applied_supervisor_notes(events).is_empty() {
        return Some((
            skip::SUPERVISOR_NOTE,
            format!(
                "{}: a supervisor note is in force, so every item works",
                skip::SUPERVISOR_NOTE
            ),
        ));
    }
    None
}

/// The refusal of a visit of a node that declares `skip` and has no
/// recorded set.
fn no_set(run: &ListRun<'_, '_, '_>) -> EngineError {
    EngineError::Invalid(format!(
        "{}: node {} declares skip and the run recorded no skip set for it; start a new run",
        crate::list::skip::INCOMPLETE,
        run.node.id
    ))
}

/// A visit's skip as its opening steps read it: the run's recorded set and
/// the visit's recorded refusal.
#[derive(Clone, Copy)]
struct Skip<'s> {
    set: Option<&'s crate::list::skip::SkipSet>,
    refused: Option<&'s str>,
}

/// The opening steps of a visit, right after its `list_started`: a
/// reopen requeues what needs a look; a new visit carries the passed items
/// (`rerun: not_passed`) and parks the items with an unknown send.
/// `previous` is the latest visit as the journal stood before the
/// `list_started`, `sends` the sends as they stood then; an item in `done`
/// already has a settlement in this visit and is left as it is.
fn opening_steps(
    run: &ListRun<'_, '_, '_>,
    reopen: bool,
    previous: Option<&ListFold>,
    sends: &sends::ItemSends,
    done: &BTreeSet<String>,
    skip: Skip<'_>,
) -> Result<(), EngineError> {
    if reopen {
        if let Some(previous) = previous {
            requeue_for_retry(run, previous, sends, done)?;
        }
    } else {
        let mut carried = BTreeSet::new();
        if run.spec.rerun == RerunPolicy::NotPassed
            && let Some(previous) = previous
        {
            carried = carry_passed(run, previous, done)?;
        }
        if run.spec.skip.is_some() {
            skip_passed(run, skip, sends, done, &carried)?;
        }
        park_unknown_sends(run, sends, done)?;
    }
    Ok(())
}

/// A new visit of a list with `skip`: every item of the run's recorded set
/// whose skip digest still matches is settled passed without running, with
/// the earlier run it passed in. Nothing when the visit's start recorded a
/// refusal. The scripts pin is checked right before the settlements: a
/// difference fails the visit and no item is settled as skipped.
fn skip_passed(
    run: &ListRun<'_, '_, '_>,
    skip: Skip<'_>,
    sends: &sends::ItemSends,
    done: &BTreeSet<String>,
    carried: &BTreeSet<String>,
) -> Result<(), EngineError> {
    use crate::list::skip as s;
    let Some(set) = skip.set else {
        return Err(no_set(run));
    };
    if skip.refused.is_some() {
        return Ok(());
    }
    let Ok(work) = run.work() else {
        return Ok(());
    };
    // A skipped item carries the output its source settlement recorded
    // (the set holds it, read on the one read the set came from), or none
    // when the source item produced none.
    let skipped: Vec<(&str, &String, Option<&String>)> = run
        .items
        .iter()
        .filter(|item| {
            !done.contains(&item.id)
                && !carried.contains(&item.id)
                && sends.unknown(run.key(&item.id)).is_none()
        })
        .filter_map(|item| {
            let entry = set.items.get(&item.id)?;
            let output = set.outputs.get(&item.id);
            (s::skip_digest(&item.id, &run.item_part(item), &work) == *entry).then_some((
                item.id.as_str(),
                entry,
                output,
            ))
        })
        .collect();
    if skipped.is_empty() {
        return Ok(());
    }
    if !run.scripts_pinned() {
        return Err(EngineError::Invalid(format!(
            "{}: the run's scripts copy changed before the skip settlements were written",
            s::SCRIPTS_CHANGED_FAIL
        )));
    }
    for (id, entry, output) in skipped {
        run.settle(Settlement {
            id,
            state: settled::PASSED,
            carried: true,
            output: output.cloned(),
            skipped_from: set.source.clone(),
            skip_digest: Some(entry.clone()),
            ..Settlement::default()
        })?;
    }
    Ok(())
}

/// Completes the opening steps of the open visit a resumed drive finds,
/// when no batch and no pilot slice was journaled since its latest
/// `list_started` (the opening steps always come before either). Each
/// step runs on the journal as it stood before that `list_started`, so
/// it decides as the first pass did, and skips every item already
/// settled since, so a step the first pass finished is not repeated.
fn complete_opening(run: &ListRun<'_, '_, '_>, events: &[Event]) -> Result<(), EngineError> {
    let node = run.node.id.as_str();
    let Some(at) = events.iter().rposition(
        |e| matches!(&e.payload, EventPayload::ListStarted { node: n, .. } if n == node),
    ) else {
        return Ok(());
    };
    let mut done = BTreeSet::new();
    for e in &events[at + 1..] {
        match &e.payload {
            EventPayload::ListBatchStarted { node: n, .. }
            | EventPayload::ListPilot { node: n, .. }
            | EventPayload::ListStopped { node: n, .. }
                if n == node =>
            {
                return Ok(());
            }
            EventPayload::ListItemSettled { node: n, item, .. } if n == node => {
                done.insert(item.clone());
            }
            _ => {}
        }
    }
    let (reopen, refused) = match &events[at].payload {
        EventPayload::ListStarted {
            reopened,
            skip_refused,
            ..
        } => (*reopened, skip_refused.clone()),
        _ => (false, None),
    };
    let before = &events[..at];
    let previous = ListFold::of(before, node);
    let sends = sends::ItemSends::of(before, node);
    // The visit's skip decision as its start recorded it, never recomputed.
    let set = crate::list::skip::recorded(events, node);
    opening_steps(
        run,
        reopen,
        previous.as_ref(),
        &sends,
        &done,
        Skip {
            set: set.as_ref(),
            refused: refused.as_deref(),
        },
    )
}

/// Whether a supervisor retried `node` after the visit starting at `seq`
/// ended: the node's latest `node_finished` since then failed, and a
/// `node_retry` follows it. A visit a retry reopened that then succeeded is
/// over; the next visit of the node starts afresh.
fn retried_after_failure(events: &[Event], node: &str, seq: u64) -> bool {
    let last = events.iter().rposition(|e| {
        e.seq > seq && matches!(&e.payload, EventPayload::NodeFinished { node: n, .. } if n == node)
    });
    last.is_some_and(|at| {
        matches!(&events[at].payload, EventPayload::NodeFinished { status, .. }
            if status != NodeStatus::Succeeded.as_str())
            && events[at..].iter().any(|e| {
                matches!(&e.payload, EventPayload::SupervisorAction { action, node: Some(n), .. }
                    if n == node && action == supervisor_action::NODE_RETRY)
            })
    })
}

/// A supervisor retry: every item that needs a look runs again, except one
/// with a send whose outcome is still unknown. Once a person settled that
/// send either way, the item runs again: the ledger answers a repeat of a
/// confirmed send with `already_sent`, and the item's check decides. A
/// repeat is the same send even when the rerun writes the payload
/// differently: an item's send made without `--intent` is labelled by the
/// item, not by its arguments (`connector::intent::ITEM_LABEL`), so one
/// item sends a function once unless the agent names distinct sends with
/// distinct `--intent` labels.
fn requeue_for_retry(
    run: &ListRun<'_, '_, '_>,
    previous: &ListFold,
    sends: &sends::ItemSends,
    done: &BTreeSet<String>,
) -> Result<(), EngineError> {
    for (id, rec) in &previous.items {
        if done.contains(id)
            || rec.state != Some(ItemState::NeedsLook)
            || rec.reason.as_deref() == Some(NeedsLookReason::ItemTooLarge.as_str())
            || sends.unknown(run.key(id)).is_some()
        {
            continue;
        }
        run.settle(Settlement {
            id,
            round: rec.round,
            state: settled::INTERRUPTED,
            reason: Some("retry"),
            ..Settlement::default()
        })?;
    }
    Ok(())
}

/// A new visit: an item with an earlier send whose outcome is still
/// unknown needs a look at once and is never resent.
fn park_unknown_sends(
    run: &ListRun<'_, '_, '_>,
    sends: &sends::ItemSends,
    done: &BTreeSet<String>,
) -> Result<(), EngineError> {
    for item in &run.items {
        if !done.contains(&item.id) && sends.unknown(run.key(&item.id)).is_some() {
            run.settle(Settlement {
                id: &item.id,
                state: settled::NEEDS_LOOK,
                reason: Some(NeedsLookReason::UnknownSend.as_str()),
                note: batch::failure_note(NeedsLookReason::UnknownSend, ""),
                ..Settlement::default()
            })?;
        }
    }
    Ok(())
}

/// `rerun: not_passed`: items that were worked and passed in the previous
/// visit with the same content are carried over as passed. One whose
/// recorded content digest is missing or different runs again, and a row a
/// skip settled is never carried: each visit decides it again. Returns the
/// ids it carried; a carried row keeps the skip digest it had.
fn carry_passed(
    run: &ListRun<'_, '_, '_>,
    previous: &ListFold,
    done: &BTreeSet<String>,
) -> Result<BTreeSet<String>, EngineError> {
    let mut carried = BTreeSet::new();
    for item in &run.items {
        if done.contains(&item.id) {
            continue;
        }
        let Some(rec) = previous.item(&item.id) else {
            continue;
        };
        if rec.state != Some(ItemState::Passed)
            || rec.skipped_from.is_some()
            || rec.digest.as_deref() != Some(item.digest.as_str())
        {
            continue;
        }
        run.ctx.journal.append(EventPayload::ListItemSettled {
            node: run.node.id.clone(),
            item: item.id.clone(),
            state: settled::PASSED.to_string(),
            verdict: rec.verdict.clone(),
            note: rec.note.clone(),
            reason: None,
            output: rec.output.clone(),
            round: rec.round,
            carried: true,
            check: None,
            digest: Some(item.digest.clone()),
            skipped_from: None,
            skip_digest: rec.skip_digest.clone(),
            skip_latched: None,
            result: None,
        })?;
        carried.insert(item.id.clone());
    }
    Ok(carried)
}

/// Items too large to hand a step need a look before they ever run.
fn settle_too_large(run: &ListRun<'_, '_, '_>, fold: Option<&ListFold>) -> Result<(), EngineError> {
    for item in &run.items {
        let done = fold
            .and_then(|f| f.item(&item.id))
            .is_some_and(ItemRecord::is_terminal);
        if !done && item_bytes(item) > ITEM_MAX_BYTES {
            run.settle(Settlement {
                id: &item.id,
                state: settled::NEEDS_LOOK,
                reason: Some(NeedsLookReason::ItemTooLarge.as_str()),
                note: Some(format!("the item is larger than {ITEM_MAX_BYTES} bytes")),
                ..Settlement::default()
            })?;
        }
    }
    Ok(())
}

/// The node output: the counts and every item with its state, note, item
/// and outputs. Fails the node when items need a look and the list says
/// `on_needs_look: fail`.
fn compose(run: &ListRun<'_, '_, '_>) -> Result<AttemptOutcome, EngineError> {
    let events = read_all(run.ctx.run_dir)?;
    let fold = ListFold::of(&events, &run.node.id).unwrap_or_default();
    let c = fold.counts();
    let items: Vec<Value> = run
        .items
        .iter()
        .map(|item| {
            let rec = fold.item(&item.id).cloned().unwrap_or_default();
            let output = rec
                .output
                .as_deref()
                .and_then(|o| serde_json::from_str::<Value>(o).ok())
                .unwrap_or(Value::Null);
            let mut row = json!({
                "id": item.id,
                "state": rec.state.map_or("pending", ItemState::as_str),
                "note": rec.note,
                "item": item.value,
                "output": output,
            });
            if let Some(from) = &rec.skipped_from {
                row["skipped_from"] = json!(from);
            }
            row
        })
        .collect();
    let mut output = serde_json::Map::new();
    output.insert("total".into(), json!(c.total));
    output.insert("processed".into(), json!(c.processed));
    output.insert("passed".into(), json!(c.passed));
    output.insert("passed_with_note".into(), json!(c.passed_with_note));
    output.insert("needs_look".into(), json!(c.needs_look));
    output.insert("filtered_out".into(), json!(fold.filtered_out));
    // Only a list with `skip` writes the count, so a plain list's output is
    // what it always was.
    if run.spec.skip.is_some() {
        output.insert("skipped".into(), json!(c.skipped));
    }
    output.insert("items".into(), json!(items));
    // A visit that ended early: `stopped` in its output, the node fails
    // whatever `on_needs_look` says.
    if let Some((reason, detail)) = &fold.stopped {
        output.insert(
            "stopped".into(),
            json!({"reason": reason, "detail": detail}),
        );
    }
    let output = Value::Object(output).to_string();
    let stopped = fold.stopped.is_some();
    let failed = stopped
        || (c.needs_look > 0 && run.spec.on_needs_look == apb_core::list::OnNeedsLook::Fail);
    Ok(AttemptOutcome::Finished {
        status: if failed {
            NodeStatus::Failed
        } else {
            NodeStatus::Succeeded
        },
        output,
        events: Vec::new(),
    })
}
