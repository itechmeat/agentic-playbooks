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
mod run;
mod source;

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
    let loaded = match source::load(ctx.playbook, ctx.state, spec) {
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
        batch::End::Halted => Ok(AttemptOutcome::Halted { events: Vec::new() }),
        batch::End::Pause(pause) => Ok(AttemptOutcome::EnginePaused {
            pause,
            events: Vec::new(),
        }),
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
        settle_too_large(run, same)?;
        return Ok(events);
    }
    let reopen = same.is_some_and(|f| retried_after_failure(&events, node, f.started_seq));
    let sends = batch::ItemSends::of(&events, node);
    run.ctx.journal.append(EventPayload::ListStarted {
        node: node.clone(),
        total: run.items.len() as u32,
        filtered_out,
        items_digest: digest,
        batch_size: run.spec.batch.size.max(1),
        parallel: run.workers as u32,
        reopened: reopen,
    })?;
    if reopen {
        let previous = same.expect("a reopen has a visit");
        requeue_for_retry(run, previous, &sends)?;
    } else {
        if run.spec.rerun == RerunPolicy::NotPassed
            && let Some(previous) = &fold
        {
            carry_passed(run, previous)?;
        }
        park_unknown_sends(run, &sends)?;
    }
    let events = read_all(run.ctx.run_dir)?;
    settle_too_large(run, ListFold::of(&events, node).as_ref())?;
    Ok(events)
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
    sends: &batch::ItemSends,
) -> Result<(), EngineError> {
    for (id, rec) in &previous.items {
        if rec.state != Some(ItemState::NeedsLook)
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
    sends: &batch::ItemSends,
) -> Result<(), EngineError> {
    for item in &run.items {
        if sends.unknown(run.key(&item.id)).is_some() {
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

/// `rerun: not_passed`: items that passed in the previous visit with the
/// same content are carried over as passed. One whose recorded content
/// digest is missing or different runs again.
fn carry_passed(run: &ListRun<'_, '_, '_>, previous: &ListFold) -> Result<(), EngineError> {
    for item in &run.items {
        let Some(rec) = previous.item(&item.id) else {
            continue;
        };
        if rec.state != Some(ItemState::Passed)
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
        })?;
    }
    Ok(())
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
            json!({
                "id": item.id,
                "state": rec.state.map_or("pending", ItemState::as_str),
                "note": rec.note,
                "item": item.value,
                "output": output,
            })
        })
        .collect();
    let output = json!({
        "total": c.total,
        "processed": c.processed,
        "passed": c.passed,
        "passed_with_note": c.passed_with_note,
        "needs_look": c.needs_look,
        "filtered_out": fold.filtered_out,
        "items": items,
    })
    .to_string();
    let failed = c.needs_look > 0 && run.spec.on_needs_look == apb_core::list::OnNeedsLook::Fail;
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
