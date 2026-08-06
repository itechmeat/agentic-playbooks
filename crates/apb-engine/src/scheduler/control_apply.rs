//! The top-of-loop control scan: applying whatever the supervisor (or `apb
//! stop`) queued in `control.jsonl` before the next node runs.
//!
//! The scan is the only place a queued command is consumed proactively, and it
//! is deliberately conservative: an entry's effect is journaled BEFORE its
//! cursor is persisted, so an I/O fault leaves the command to resurface rather
//! than vanish, and a `Retry`/`ContinueFrom` (valid only in answer to a wake)
//! stops the scan instead of being swallowed.
//! Shares the parent module's imports via `use super::*`.

use super::*;

/// What the scan decided for the current iteration of the drive loop.
pub(crate) enum ControlScan {
    /// Nothing terminal: execute the current node.
    Proceed,
    /// A patch migrated the run; restart the iteration on the new playbook.
    Migrated,
    /// The run ends here with this outcome.
    Terminal(RunStatus),
}

/// Applies every control entry queued after `control_cursor` that can be
/// applied at a node boundary. `playbook`, `current` and `control_cursor` are
/// updated in place, mirroring what the loop itself would have done.
#[allow(clippy::too_many_arguments)]
pub(crate) fn scan_control(
    root: &Path,
    run_dir: &Path,
    log: &mut EventLog,
    cfg: &RunConfig,
    playbook: &mut Playbook,
    current: &mut String,
    control_cursor: &mut Option<u64>,
    last_applied_patch: &mut Option<AppliedPatch>,
    run_cancel: &AtomicBool,
) -> Result<ControlScan, EngineError> {
    // Top-of-loop scan of control.jsonl - works in BOTH modes (Autonomous
    // and Supervised), at the boundary of every iteration, before executing a node.
    //
    // The `control_cursor` is shared with `await_control` below - it is one and
    // the same monotonic sequence of consuming control.jsonl, which is what
    // prevents double application: once a command has advanced the cursor here,
    // `await_control` will never see it again (it reads after the same cursor), and
    // vice versa.
    //
    // Proactively (without a wake) only "stop" commands are handled here -
    // Pause (-> RunPaused, exit Paused), Abort (-> RunAborted, exit
    // Aborted) - and ContextAppend (not terminal: logs SupervisorAction +
    // rebuilds context.md, the cursor advances, the scan continues).
    //
    // Retry/ContinueFrom outside a wake is a caller error (the supervisor
    // should only send them in response to WakeRaised), but we do not lose them:
    // the scan STOPS at such an entry, without advancing the cursor past it.
    // The command stays in control.jsonl with a seq greater than the current cursor and
    // will be consumed by `await_control` on the nearest wake of that same node. This
    // was exactly the Phase 4a bug: the cursor advanced past ANY entry, including
    // Retry/ContinueFrom, silently losing them.
    let mut patch_applied = false;
    // Set to the node named by a Retry/ContinueFrom the scan stopped at
    // without consuming it: everything queued BEHIND that entry - including
    // a pending Abort - is unreachable for this scan. See the cancel check
    // below the loop.
    let mut blocked_by: Option<String> = None;
    let pending_control = read_control_after(run_dir, *control_cursor)?;
    for entry in pending_control.iter().cloned() {
        match entry.cmd {
            Control::Abort { reason } => {
                // Effect first, cursor persisted last: if `log.append` errs
                // (ordinary I/O failure), the entry must NOT be marked
                // applied - it has to resurface on the next drive rather
                // than being silently dropped. Persisted before the return
                // (once the effect has actually happened) so a resumed
                // drive never sees this same terminal entry again (Task 4
                // completion-plan defect 1 - a stale stop command re-firing
                // on resume).
                log.append(EventPayload::RunAborted { reason })?;
                write_control_cursor(run_dir, entry.seq)?;
                return Ok(ControlScan::Terminal(RunStatus::Aborted));
            }
            Control::Pause => {
                // Same ordering and reasoning as Abort above.
                log.append(EventPayload::RunPaused {
                    reason: "supervisor pause".into(),
                })?;
                write_control_cursor(run_dir, entry.seq)?;
                return Ok(ControlScan::Terminal(RunStatus::Paused));
            }
            Control::ContextAppend { note } => {
                log.append(EventPayload::SupervisorAction {
                    action: "context_append".into(),
                    node: None,
                    detail: note,
                })?;
                rebuild_context_md(run_dir)?;
                *control_cursor = Some(entry.seq);
                write_control_cursor(run_dir, entry.seq)?;
            }
            Control::Patch {
                version,
                classification,
                continue_from,
            } => {
                // Effect first (`apply_patch` can itself err on ordinary
                // I/O - unreadable events.jsonl, a bad snapshot read), then
                // persist the cursor only once it has actually returned
                // Ok: an error here must leave the entry unconsumed so it
                // resurfaces on the next drive instead of being silently
                // dropped.
                let result = apply_patch(
                    root,
                    run_dir,
                    log,
                    cfg,
                    playbook,
                    current,
                    PatchCommand {
                        version,
                        classification,
                        continue_from,
                    },
                )?;
                *control_cursor = Some(entry.seq);
                write_control_cursor(run_dir, entry.seq)?;
                match result {
                    PatchResult::Applied(applied) => {
                        *last_applied_patch = Some(*applied);
                        patch_applied = true;
                        break;
                    }
                    PatchResult::Rejected => {}
                    PatchResult::Paused => {
                        return Ok(ControlScan::Terminal(RunStatus::Paused));
                    }
                }
            }
            Control::Progress {
                done,
                total,
                label,
                node,
            } => {
                log.append(EventPayload::RunProgress {
                    node_id: node.unwrap_or_else(|| current.clone()),
                    done,
                    total,
                    label,
                })?;
                *control_cursor = Some(entry.seq);
                write_control_cursor(run_dir, entry.seq)?;
            }
            Control::Interrupt { .. } => {
                // A mid-attempt interrupt (finding 7 of issue #42, third
                // item of issue #40) reaching a node boundary is spent: the
                // running attempt's own poll loop already observed it live
                // (journaling control_received / attempt_interrupted) and
                // terminated the agent. At a boundary there is no attempt to
                // interrupt, so just consume it and move on - an interrupt
                // posted with no attempt running is a harmless no-op.
                *control_cursor = Some(entry.seq);
                write_control_cursor(run_dir, entry.seq)?;
            }
            Control::Rebind {
                node,
                profile,
                scope,
                bundle,
                reason,
            } => {
                // Applied in place like ContextAppend: the effect (verify +
                // journal + overlay) runs first, the cursor is persisted only
                // once it returns Ok, so an I/O fault leaves the entry to
                // resurface on the next drive instead of being dropped. Never
                // terminal - the scan continues.
                apply_rebind(
                    root,
                    run_dir,
                    log,
                    RebindCommand {
                        node,
                        profile,
                        scope,
                        bundle,
                        reason,
                    },
                )?;
                *control_cursor = Some(entry.seq);
                write_control_cursor(run_dir, entry.seq)?;
            }
            Control::Retry { ref node, .. } | Control::ContinueFrom { ref node } => {
                // Valid only inside await_control, in response to a wake -
                // we do not advance the cursor, the command remains unconsumed.
                blocked_by = Some(node.clone());
                break;
            }
        }
    }

    // A stop that the scan above cannot reach. The watcher reads the raw
    // control file and so DOES see an Abort queued behind an unconsumable
    // Retry/ContinueFrom; the scan stops short of it and never applies it.
    // The flag then stayed latched for the rest of the drive: every later
    // node returned `Cancelled` instantly, `Cancelled` is neither Unknown
    // nor Interrupted, so it fell through to edge selection, matched
    // nothing, and the drive failed with "has no outgoing edge" - which
    // `drive_prepared` stamped as run_finished(failed). An operator who
    // asked for a stop got a FAILED run.
    //
    // The flag being set is proof that an Abort is pending, so finalize as
    // aborted here instead - and consume the abort properly, cursor and
    // all. Skipping the cursor forward past the Retry is what the scalar
    // cursor forces (see `write_control_cursor`), and it is the right
    // trade here: the run is stopping, so a queued Retry has nothing left
    // to retry. What it must not be is silent, hence the
    // `retry_superseded_by_stop` record ahead of the terminal event. NOT
    // advancing would be far worse than losing the Retry: this arm does
    // not consume anything, so every later resume would re-enter it and
    // append another RunAborted, forever.
    //
    // If the Abort is not in `pending_control` (the watcher saw an append
    // that landed after our read), fall through rather than invent a seq:
    // the next iteration re-reads control and this arm fires with the real
    // entry in hand.
    if let Some(blocked_node) = blocked_by.as_ref()
        && run_cancel.load(Ordering::SeqCst)
        && let Some((abort_seq, reason)) = pending_control.iter().find_map(|e| match &e.cmd {
            Control::Abort { reason } => Some((e.seq, reason.clone())),
            _ => None,
        })
    {
        log.append(EventPayload::SupervisorAction {
            action: "retry_superseded_by_stop".into(),
            node: Some(blocked_node.clone()),
            detail: format!(
                "a pending stop was applied before this command could be consumed, so it was discarded: {reason}"
            ),
        })?;
        log.append(EventPayload::RunAborted { reason })?;
        write_control_cursor(run_dir, abort_seq)?;
        return Ok(ControlScan::Terminal(RunStatus::Aborted));
    }

    if patch_applied {
        // A migration - linear continuation from continue_from; the caller
        // drops the previous parallel branches and restarts the iteration.
        return Ok(ControlScan::Migrated);
    }
    Ok(ControlScan::Proceed)
}
