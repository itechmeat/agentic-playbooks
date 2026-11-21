//! Runs a list visit's remaining items in batches. Each batch admits up to
//! `batch.size` ids (requeued items go to the tail), runs them on at most
//! `workers` threads (each item through the ordinary `execute_node` path
//! with an item-stamped journal, then its check), and settles every item on
//! the runner thread before the next batch starts. After each batch a
//! `run_progress` carries the counts.
//!
//! A drain pause admits no new item and lets the running ones finish; a
//! `--now` pause kills them and journals them interrupted; a quota pause of
//! one item stops admission and drains the rest; an abort ends the visit
//! cancelled with its items open. None of these spends an item's round.

use std::collections::VecDeque;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};

use apb_core::list::verdict::{ItemEvent, NeedsLookReason, Transition, transition};

use super::ItemExec;
use super::check::{self, Checked};
use super::run::{ListRun, Settlement, settled_output};
use super::sends::ItemSends;
use crate::control::Control;
use crate::error::EngineError;
use crate::event::{EventPayload, read_all};
use crate::exec_key::ExecKey;
use crate::list::ledger::{ListFold, RECONCILE, progress_label, settled};
use crate::list::prompt::item_section;
use crate::pause::PauseRequest;
use crate::scheduler::budget_gate::Admission;
use crate::scheduler::outcome::{AttemptOutcome, ExecOptions, NodeCtx};
use crate::state::NodeStatus;

/// How the batches ended.
pub(super) enum End {
    /// Every item has a terminal state.
    Done,
    /// A pause stopped admission; the visit stays open.
    Halted,
    /// The engine pauses the run: an item hit a provider quota, or the
    /// budget gate refused the next batch.
    Pause(PauseRequest),
    /// The run was stopped.
    Cancelled,
    /// An unattended run's budget refused the next batch: no item is
    /// admitted, and the drive ends the run `budget_exhausted`.
    Exhausted,
    /// The pilot slice settled and a person decides; the visit stays open.
    PilotGate,
    /// The visit ended early: a pilot `stop` or `stop_when`.
    Stopped,
}

/// The visit's early-stop rule and the ids excluded from its count (the
/// latest pilot slice).
pub(super) struct StopRule<'a> {
    pub(super) when: &'a apb_core::list::StopWhen,
    pub(super) outside: Vec<String>,
}

/// What one item's execution came to, sent from a worker.
enum Ran {
    Finished {
        status: NodeStatus,
        output: String,
        events: Vec<EventPayload>,
        checked: Option<Checked>,
    },
    Paused {
        pause: PauseRequest,
        events: Vec<EventPayload>,
    },
}

/// Runs the batches of `run` until every item is terminal or a stop. A
/// visit with a pilot runs its slice first (to [`End::PilotGate`]), and a
/// visit whose pilot gate was answered `stop` ends at once; the early-stop
/// rule of `stop_when` is read outside the slice, before a batch is
/// admitted and after one whose items all settled.
pub(super) fn run_batches(run: &ListRun<'_, '_, '_>) -> Result<End, EngineError> {
    let events = read_all(run.ctx.run_dir)?;
    let fold = ListFold::of(&events, &run.node.id).unwrap_or_default();
    // A drive that crashed between `list_stopped` and `node_finished`.
    if fold.stopped.is_some() {
        return Ok(End::Stopped);
    }
    let mut batch_no = fold.batch;
    if let Some(spec) = run.spec.pilot {
        let sends = ItemSends::of(&events, &run.node.id);
        match super::pilot::step(run, &spec, &fold, &sends)? {
            super::pilot::Step::Slice(ids) => {
                // The queue holds the slice's pending ids only, and
                // `stop_when` is never evaluated inside the slice.
                // A pause, cancel or exhaustion inside the slice ends the
                // visit as it is: it is never read as a slice waiting for
                // its gate.
                if let Some(end) = run_queue(run, VecDeque::from(ids), &mut batch_no, None)? {
                    return Ok(end);
                }
                let events = read_all(run.ctx.run_dir)?;
                let fold = ListFold::of(&events, &run.node.id).unwrap_or_default();
                if crate::list::ledger::pilot_phase(&spec, &fold)
                    != crate::list::ledger::PilotPhase::Done
                {
                    return Ok(End::PilotGate);
                }
            }
            super::pilot::Step::Wait => return Ok(End::PilotGate),
            super::pilot::Step::Stop => return Ok(End::Stopped),
            super::pilot::Step::Go => {}
        }
    }
    let events = read_all(run.ctx.run_dir)?;
    let fold = ListFold::of(&events, &run.node.id).unwrap_or_default();
    // The early stop judges the items worked: never the pilot slice a
    // person read, nor an item a skip settled.
    let mut outside = fold
        .pilot
        .as_ref()
        .map(|p| p.items.clone())
        .unwrap_or_default();
    outside.extend(fold.skipped_ids());
    let stop = run
        .spec
        .stop_when
        .as_ref()
        .map(|when| StopRule { when, outside });
    let queue: VecDeque<String> = fold
        .remaining(run.items.iter().map(|i| i.id.as_str()))
        .into_iter()
        .map(str::to_string)
        .collect();
    match run_queue(run, queue, &mut batch_no, stop.as_ref())? {
        Some(end) => Ok(end),
        None => Ok(End::Done),
    }
}

/// Runs `queue` until it empties or a stop: the loop body the visits share.
/// A batch that ended `Halted`, `Pause`, `Cancelled` or `Exhausted` is
/// honoured first, and the early-stop rule is not evaluated after it (its
/// unsettled items go back to pending, and the resumed visit evaluates the
/// rule before its next batch). `stop` is `None` inside a pilot slice.
fn run_queue(
    run: &ListRun<'_, '_, '_>,
    mut queue: VecDeque<String>,
    batch_no: &mut u32,
    stop: Option<&StopRule<'_>>,
) -> Result<Option<End>, EngineError> {
    let size = run.spec.batch.size.max(1) as usize;
    while !queue.is_empty() {
        if let Some(end) = stopped(run) {
            return Ok(Some(end));
        }
        // The early-stop rule, before a batch is admitted: a resumed visit
        // that already meets it ends before one more batch runs.
        if let Some(detail) = stop_when_holds(run, stop)? {
            run.ctx.journal.append(EventPayload::ListStopped {
                node: run.node.id.clone(),
                reason: "stop_when".into(),
                detail,
            })?;
            return Ok(Some(End::Stopped));
        }
        let take = queue.len().min(size);
        // A batch the budget refuses never starts: its items stay pending.
        if let Some(gate) = run.budget {
            match gate.admit_batch(
                run.ctx.run_dir,
                run.ctx.journal,
                run.node,
                *batch_no + 1,
                take,
            )? {
                Admission::Go => {}
                Admission::Pause(p) => return Ok(Some(End::Pause(p))),
                Admission::Exhausted => return Ok(Some(End::Exhausted)),
            }
        }
        let ids: Vec<String> = queue.drain(..take).collect();
        *batch_no += 1;
        run.ctx.journal.append(EventPayload::ListBatchStarted {
            node: run.node.id.clone(),
            batch: *batch_no,
            items: ids.clone(),
        })?;
        let ended = run_batch(run, ids, &mut queue)?;
        progress(run)?;
        if let Some(end) = ended {
            return Ok(Some(end));
        }
        // The rule again, after a batch whose items all settled: a stop
        // never leaves an item running.
        if let Some(detail) = stop_when_holds(run, stop)? {
            run.ctx.journal.append(EventPayload::ListStopped {
                node: run.node.id.clone(),
                reason: "stop_when".into(),
                detail,
            })?;
            return Ok(Some(End::Stopped));
        }
    }
    Ok(None)
}

/// The early-stop rule over the items settled outside the pilot slice.
fn stop_when_holds(
    run: &ListRun<'_, '_, '_>,
    stop: Option<&StopRule<'_>>,
) -> Result<Option<String>, EngineError> {
    let Some(rule) = stop else {
        return Ok(None);
    };
    let events = read_all(run.ctx.run_dir)?;
    let mut fold = ListFold::of(&events, &run.node.id).unwrap_or_default();
    // The excluded ids are taken out of the fold's item map by key, so a
    // long skip list costs one lookup per id, never a scan per row.
    for id in &rule.outside {
        fold.items.remove(id);
    }
    let c = fold.counts();
    Ok(rule.when.holds(c.processed, c.needs_look))
}

/// Why admission stops: an abort the watcher latched, the run's deadline
/// passed (its stop is posted here), or a pause (or an abort) posted and
/// not applied yet. The control channel is read as well as the flags, so an
/// item is never admitted after a pause request returned.
fn stopped(run: &ListRun<'_, '_, '_>) -> Option<End> {
    if crate::run_deadline::refuses_work(run.ctx.run_dir) {
        return Some(End::Halted);
    }
    if let Some(stop) = run.stop {
        if stop.cancel.load(Ordering::SeqCst) {
            return Some(End::Cancelled);
        }
        if stop.halt.load(Ordering::SeqCst) {
            return Some(End::Halted);
        }
    }
    let pending = crate::control::read_control_after(run.ctx.run_dir, run.control_after)
        .unwrap_or_default()
        .iter()
        .any(|e| matches!(e.cmd, Control::Pause(_) | Control::Abort { .. }));
    pending.then_some(End::Halted)
}

/// One `run_progress` with the visit's counts.
fn progress(run: &ListRun<'_, '_, '_>) -> Result<(), EngineError> {
    let events = read_all(run.ctx.run_dir)?;
    let c = ListFold::of(&events, &run.node.id)
        .unwrap_or_default()
        .counts();
    run.ctx.journal.append(EventPayload::RunProgress {
        node_id: run.node.id.clone(),
        done: u64::from(c.processed),
        total: u64::from(c.total),
        label: Some(progress_label(&c)),
    })
}

/// Runs one batch on the worker pool and settles every item of it as it
/// comes back; returns how the visit ends when the batch stopped it.
fn run_batch(
    run: &ListRun<'_, '_, '_>,
    ids: Vec<String>,
    queue: &mut VecDeque<String>,
) -> Result<Option<End>, EngineError> {
    let fold = ListFold::of(&read_all(run.ctx.run_dir)?, &run.node.id).unwrap_or_default();
    let work: Mutex<VecDeque<String>> = Mutex::new(ids.into_iter().collect());
    // Set by the runner when an item hit a quota: no item starts after it.
    let local_halt = AtomicBool::new(false);
    let mut quota: Option<PauseRequest> = None;
    let mut first_err: Option<EngineError> = None;
    std::thread::scope(|scope| {
        let (tx, rx) = std::sync::mpsc::channel::<(String, Result<Ran, EngineError>)>();
        for _ in 0..run.workers {
            let tx = tx.clone();
            let (work, local_halt, fold) = (&work, &local_halt, &fold);
            scope.spawn(move || {
                loop {
                    if local_halt.load(Ordering::SeqCst) || stopped(run).is_some() {
                        break;
                    }
                    let Some(id) = work.lock().unwrap_or_else(|e| e.into_inner()).pop_front()
                    else {
                        break;
                    };
                    let ran = run_item(run, fold, &id);
                    if tx.send((id, ran)).is_err() {
                        break;
                    }
                }
            });
        }
        drop(tx);
        for (id, ran) in rx {
            let res = match ran {
                Ok(Ran::Paused { pause, events }) => {
                    local_halt.store(true, Ordering::SeqCst);
                    quota.get_or_insert(pause);
                    append_all(run, &id, events).and_then(|()| interrupted(run, &fold, &id, None))
                }
                Ok(Ran::Finished {
                    status,
                    output,
                    events,
                    checked,
                }) => append_all(run, &id, events).and_then(|()| {
                    let step = Step {
                        status,
                        output: &output,
                        checked,
                    };
                    finished(run, &fold, &id, step, queue)
                }),
                Err(e) => Err(e),
            };
            if let Err(e) = res {
                local_halt.store(true, Ordering::SeqCst);
                first_err.get_or_insert(e);
            }
        }
    });
    if let Some(e) = first_err {
        return Err(e);
    }
    let aborted = run.stop.is_some_and(|s| s.cancel.load(Ordering::SeqCst));
    // Items of the batch that never started go back to pending.
    let left: Vec<String> = work.into_inner().unwrap_or_else(|e| e.into_inner()).into();
    if !aborted {
        for id in &left {
            interrupted(run, &fold, id, None)?;
        }
    }
    Ok(if aborted {
        Some(End::Cancelled)
    } else if let Some(pause) = quota {
        Some(End::Pause(pause))
    } else if !left.is_empty() || stopped(run).is_some() {
        // A drain pause (no item admitted after it) or a `--now` pause.
        Some(End::Halted)
    } else {
        None
    })
}

/// What an item's step came to, as [`finished`] settles it.
struct Step<'a> {
    status: NodeStatus,
    output: &'a str,
    checked: Option<Checked>,
}

/// Appends an item execution's returned events through its journal.
fn append_all(
    run: &ListRun<'_, '_, '_>,
    id: &str,
    events: Vec<EventPayload>,
) -> Result<(), EngineError> {
    let journal = run.ctx.journal.for_item(Some(id));
    for ev in events {
        journal.append(ev)?;
    }
    Ok(())
}

/// Runs one item: its step through `execute_node`, then its check.
fn run_item(run: &ListRun<'_, '_, '_>, fold: &ListFold, id: &str) -> Result<Ran, EngineError> {
    let item = run
        .item(id)
        .ok_or_else(|| EngineError::Invalid(format!("list item `{id}` is not in the visit")))?;
    let ordinal = run.items.iter().position(|i| i.id == id).unwrap_or(0) + 1;
    // The scripts as the item's work starts: a copy another item rewrote
    // latches the visit, so this item's settlement stamps nothing.
    run.scripts_held();
    let files = run.files(item, None)?;
    // The reviewer's note of a revised pilot: every later item run of the
    // visit is told it, the slice it revises included.
    let guidance = fold
        .pilot
        .as_ref()
        .and_then(|p| {
            p.guidance.as_deref().map(|note| {
                crate::list::prompt::guidance_section(
                    note,
                    p.guidance_via.as_deref().unwrap_or("unknown"),
                )
            })
        })
        .unwrap_or_default();
    let exec = ItemExec {
        id: id.to_string(),
        template: run.template.clone(),
        section: format!(
            "{}{guidance}",
            item_section(item, ordinal, run.items.len(), run.previous(fold, id))
        ),
        outputs: run.spec.item_outputs.clone(),
        checked: run.spec.check.is_some(),
        file: files.item,
        // An item of a skip visit runs with the visit's resolved execution,
        // the value its stamp's work is computed from.
        exec: run.spec.skip.as_ref().and_then(|_| run.execution().ok()),
    };
    let journal = run.ctx.journal.for_item(Some(id));
    let ctx = NodeCtx {
        journal: &journal,
        ..*run.ctx
    };
    let outcome = super::super::node::execute_node(
        &ctx,
        &run.node.id,
        ExecOptions {
            item: Some(&exec),
            track_provenance: run.workers == 1,
            ..ExecOptions::default()
        },
    )?;
    Ok(match outcome {
        AttemptOutcome::Finished {
            status,
            output,
            events,
        } => {
            let checked =
                (status == NodeStatus::Succeeded).then(|| check::run(run, item, &output, &journal));
            Ran::Finished {
                status,
                output,
                events,
                checked,
            }
        }
        AttemptOutcome::EnginePaused { pause, events } => Ran::Paused { pause, events },
        AttemptOutcome::Suspended { .. } | AttemptOutcome::Halted { .. } => {
            return Err(EngineError::Invalid(format!(
                "list item `{id}` of node `{}` cannot suspend",
                run.node.id
            )));
        }
        // A list node's item retries are never delayed (`retry_delay` is
        // refused on a list node).
        AttemptOutcome::Deferred { .. } => {
            return Err(EngineError::Invalid(format!(
                "list item `{id}` of node `{}` cannot defer its retry (`retry_delay` is refused on a list node)",
                run.node.id
            )));
        }
    })
}

/// Settles an item whose step finished.
fn finished(
    run: &ListRun<'_, '_, '_>,
    fold: &ListFold,
    id: &str,
    step: Step<'_>,
    queue: &mut VecDeque<String>,
) -> Result<(), EngineError> {
    let Step {
        status,
        output,
        checked,
    } = step;
    let rec = fold.item(id).cloned().unwrap_or_default();
    let key = ExecKey::new(&run.node.id, Some(id));
    if status == NodeStatus::Cancelled {
        let by_pause = run
            .stop
            .is_some_and(|s| super::super::journal::interrupted_by_pause(&s.pause_now, &s.cancel));
        if by_pause {
            super::super::journal::journal_interrupted(run.ctx.journal, run.ctx.run_dir, key)?;
        }
        if run.stop.is_some_and(|s| s.cancel.load(Ordering::SeqCst)) {
            // An abort: the visit ends cancelled with its items open.
            return Ok(());
        }
        return interrupted(run, fold, id, None);
    }
    // A send whose outcome is unknown outweighs whatever the step and its
    // check said: the item passes only once that send is settled. The item
    // settles as soon as it is back (its siblings may still run and their
    // progress shows), so its sends are read then: one journal read and one
    // fold of the ledger for the item.
    let unknown = ItemSends::of(&read_all(run.ctx.run_dir)?, &run.node.id).unknown(key);
    let event = match (&checked, status, unknown) {
        (_, _, Some(mode)) => ItemEvent::Failed {
            unknown_send: Some(mode),
        },
        (Some(c), NodeStatus::Succeeded, None) => match c.verdict {
            Some(v) => ItemEvent::Checked(v),
            None => ItemEvent::CheckUnknown,
        },
        _ => ItemEvent::Failed { unknown_send: None },
    };
    let kept = settled_output(run.spec, output);
    let note = match unknown {
        Some(_) => None,
        None => checked.as_ref().and_then(|c| c.note.clone()),
    };
    let kind = checked.as_ref().and_then(|c| c.kind);
    let verdict = match event {
        ItemEvent::Checked(v) => Some(v.as_str()),
        _ => None,
    };
    let base = Settlement {
        id,
        verdict,
        output: kept,
        round: rec.round,
        check: kind,
        ran: Some((status, output)),
        check_decided: checked
            .as_ref()
            .is_some_and(|c| c.verdict.is_some() && !c.fallback),
        ..Settlement::default()
    };
    let digest = run.item(id).map(|i| i.digest.as_str());
    match transition(event, rec.round, run.spec.batch.retries, rec.reconciled) {
        Transition::Pass { .. } => run.settle(Settlement {
            state: settled::PASSED,
            note,
            digest,
            ..base
        }),
        Transition::Requeue {
            round,
            reconcile: false,
        } => {
            queue.push_back(id.to_string());
            run.settle(Settlement {
                state: settled::REJECTED,
                note,
                round,
                ..base
            })
        }
        Transition::Requeue {
            reconcile: true, ..
        } => {
            queue.push_back(id.to_string());
            interrupted(run, fold, id, Some(RECONCILE))
        }
        Transition::NeedsLook(reason) => run.settle(Settlement {
            state: settled::NEEDS_LOOK,
            reason: Some(reason.as_str()),
            note: note.or_else(|| failure_note(reason, output)),
            ..base
        }),
        Transition::Pending => interrupted(run, fold, id, None),
    }
}

/// Settles `id` back to pending in its current round.
fn interrupted(
    run: &ListRun<'_, '_, '_>,
    fold: &ListFold,
    id: &str,
    reason: Option<&str>,
) -> Result<(), EngineError> {
    let round = fold.item(id).map_or(0, |r| r.round);
    run.settle(Settlement {
        id,
        state: settled::INTERRUPTED,
        reason,
        round,
        ..Settlement::default()
    })
}

/// The note of an item whose step failed: the first line of its failure.
pub(super) fn failure_note(reason: NeedsLookReason, output: &str) -> Option<String> {
    match reason {
        NeedsLookReason::Failed => output
            .lines()
            .map(str::trim)
            .find(|l| !l.is_empty())
            .map(|l| crate::list::prompt::clip(l, 300).to_string()),
        NeedsLookReason::UnknownSend => {
            Some("a send's outcome is unknown; a person resolves it before a retry".into())
        }
        _ => None,
    }
}
