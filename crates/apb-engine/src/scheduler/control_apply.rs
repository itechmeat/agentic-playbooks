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
    /// `definition` says what the patch did to the definition the drive
    /// executes.
    Migrated { definition: Definition },
    /// The run ends here with this outcome.
    Terminal(RunStatus),
}

/// The discard of a park (design D6 rule 4): the cursor moves past it, in
/// memory and persisted, and nothing is journaled for it. The drive goes on
/// with the next entry.
fn discard_park(
    run_dir: &Path,
    control_cursor: &mut Option<u64>,
    entry: &crate::control::ControlEntry,
) -> Result<(), EngineError> {
    *control_cursor = Some(entry.seq);
    write_control_cursor(run_dir, entry.seq)
}

/// The `(park seconds, timeout seconds)` of the node's open wait, for the
/// due time the scan computes again (design D6 rule 3.1, inbox-waits D4
/// "Parking" item 2): a timer or an inbox wait parks, a webhook wait never
/// does.
fn park_wait_of(playbook: &Playbook, node: &str) -> Option<(u64, u64)> {
    let node = playbook.node(node)?;
    match &node.kind {
        apb_core::schema::NodeKind::Wait {
            wait_for: apb_core::schema::WaitFor::Webhook { .. },
            ..
        } => None,
        apb_core::schema::NodeKind::Wait {
            wait_for,
            timeout_seconds,
            ..
        } => Some((
            crate::scheduler::wait_park::park_seconds(wait_for, *timeout_seconds),
            *timeout_seconds,
        )),
        _ => None,
    }
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
    // What the patch did to the definition the drive executes.
    let mut definition = Definition::Current;
    // Set to the node named by a Retry/ContinueFrom the scan stopped at
    // without consuming it: everything queued BEHIND that entry - including
    // a pending Abort - is unreachable for this scan. See the cancel check
    // below the loop.
    let mut blocked_by: Option<String> = None;
    let pending_control = read_control_after(run_dir, *control_cursor)?;
    // Whether an entry of this pass was consumed already: a park is a
    // candidate only as the last entry of a pass that applied nothing
    // before it (design D6 rule 2.2).
    let mut consumed = false;
    // The cursor value an exit that consumed entry `idx` writes: the seq of
    // a park that is the entry right after it, discarded in the same write
    // (design D6 rule 5), else the entry's own. Never below the consumed
    // entry: a stop posted without the numbering lock can stand above a
    // park appended after it, and the cursor must not leave it pending.
    let cursor_after = |idx: usize| -> u64 {
        let own = pending_control[idx].seq;
        match pending_control.get(idx + 1) {
            Some(next) => match &next.cmd {
                Control::Pause(req) if req.is_park() => next.seq.max(own),
                _ => own,
            },
            None => own,
        }
    };
    for (idx, entry) in pending_control.iter().enumerate() {
        match entry.cmd.clone() {
            Control::Abort { reason, by, .. } => {
                // Effect first, cursor persisted last: if `log.append` errs
                // (ordinary I/O failure), the entry must NOT be marked
                // applied - it has to resurface on the next drive rather
                // than being silently dropped. Persisted before the return
                // (once the effect has actually happened) so a resumed
                // drive never sees this same terminal entry again (Task 4
                // completion-plan defect 1 - a stale stop command re-firing
                // on resume).
                log.append(EventPayload::RunAborted { reason, by })?;
                let target = cursor_after(idx);
                write_control_cursor(run_dir, target)?;
                *control_cursor = Some(target);
                return Ok(ControlScan::Terminal(RunStatus::Aborted));
            }
            Control::Pause(req) => {
                // A park is decided here only as the last word: the last
                // entry of the pass, nothing consumed before it, at the
                // node the drive is at (design D6 rule 2), then re-decided
                // under the run's event lock (rule 3). Any other park is
                // discarded (rule 4).
                if req.is_park() {
                    let candidate = idx + 1 == pending_control.len()
                        && !consumed
                        && crate::pause::cause::parse_wait_reason(
                            req.reason.as_deref().unwrap_or(""),
                        ) == Some(current.as_str());
                    if !candidate {
                        discard_park(run_dir, control_cursor, entry)?;
                        continue;
                    }
                    // A lock that cannot be taken within its bounded wait
                    // discards the park (rule 4); it never ends the drive.
                    let Ok(lock) = apb_core::fsutil::lock_dir(run_dir, crate::stop::EVENT_LOCK)
                    else {
                        discard_park(run_dir, control_cursor, entry)?;
                        continue;
                    };
                    // Nothing may sit behind the park when it is applied.
                    if !read_control_after(run_dir, Some(entry.seq))?.is_empty() {
                        drop(lock);
                        discard_park(run_dir, control_cursor, entry)?;
                        continue;
                    }
                    // The wait's due time, computed again on the current
                    // clock (rule 3.1): a pause journaled directly since the
                    // park was posted moved it; a past or stale park never
                    // equals it.
                    let due_again =
                        park_wait_of(playbook, current).and_then(|(seconds, timeout)| {
                            let events = read_all(run_dir).ok()?;
                            let now = apb_core::clock::now_ms();
                            crate::scheduler::wait_park::due_at_ms(
                                &events, current, seconds, timeout, now,
                            )
                        });
                    if due_again != req.until {
                        drop(lock);
                        discard_park(run_dir, control_cursor, entry)?;
                        continue;
                    }
                    // Same ordering and reasoning as Abort above.
                    log.append(req.to_event())?;
                    *control_cursor = Some(entry.seq);
                    write_control_cursor(run_dir, entry.seq)?;
                    drop(lock);
                    return Ok(ControlScan::Terminal(RunStatus::Paused));
                }
                // Same ordering and reasoning as Abort above.
                log.append(req.to_event())?;
                let target = cursor_after(idx);
                write_control_cursor(run_dir, target)?;
                *control_cursor = Some(target);
                return Ok(ControlScan::Terminal(RunStatus::Paused));
            }
            Control::ContextAppend { note } => {
                log.append(EventPayload::SupervisorAction {
                    action: crate::event::supervisor_action::CONTEXT_APPEND.into(),
                    node: None,
                    detail: note,
                })?;
                rebuild_context_md(run_dir)?;
                *control_cursor = Some(entry.seq);
                write_control_cursor(run_dir, entry.seq)?;
                consumed = true;
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
                let target = cursor_after(idx);
                *control_cursor = Some(target);
                write_control_cursor(run_dir, target)?;
                consumed = true;
                match result {
                    PatchResult::Applied(applied) => {
                        definition = applied.definition;
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
                consumed = true;
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
                consumed = true;
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
                consumed = true;
            }
            Control::Retry { ref node, .. } | Control::ContinueFrom { ref node } => {
                // Valid only inside await_control, in response to a wake -
                // we do not advance the cursor, the command remains unconsumed.
                blocked_by = Some(node.clone());
                break;
            }
        }
    }

    // A terminal command (Abort or Pause) that the scan above cannot reach.
    // The watcher (`StopWatcher`, stop.rs) reads the raw control file and so
    // DOES see a terminal entry queued behind an unconsumable
    // Retry/ContinueFrom; the scan stops short of it and never applies it.
    // For Abort specifically, `run_cancel` then stayed latched for the rest
    // of the drive: every later node returned `Cancelled` instantly,
    // `Cancelled` is neither Unknown nor Interrupted, so it fell through to
    // edge selection, matched nothing, and the drive failed with "has no
    // outgoing edge" - which `drive_prepared` stamped as run_finished(failed).
    // An operator who asked for a stop got a FAILED run. A queued Pause had
    // it worse: nothing gates on it at all (`run_cancel` is Abort-only, see
    // `stop.rs:28-38`), so the scan fell straight through to `Proceed` and
    // the run kept executing despite the pause request.
    //
    // `blocked_by` being set is proof a Retry/ContinueFrom is stuck; find the
    // first terminal entry (Abort or Pause, whichever comes first in seq
    // order) anywhere in the pending tail and salvage it instead - consuming
    // it properly, cursor and all. Skipping the cursor forward past the
    // Retry is what the scalar cursor forces (see `write_control_cursor`),
    // and it is the right trade here: the run is stopping, so a queued Retry
    // has nothing left to retry. What it must not be is silent, hence the
    // `retry_superseded_by_stop` record ahead of the terminal event. NOT
    // advancing would be far worse than losing the Retry: this arm would not
    // consume anything, so every later resume would re-enter it and append
    // another terminal event, forever.
    //
    // `run_cancel` is kept as belt-and-braces for the Abort case (it is set
    // synchronously by `stop_run` before this scan can even run again) but
    // must not gate Pause, which has no equivalent flag.
    //
    // If neither terminal kind is in `pending_control` (the watcher saw an
    // append that landed after our read), fall through rather than invent a
    // seq: the next iteration re-reads control and this arm fires with the
    // real entry in hand.
    if let Some(blocked_node) = blocked_by.as_ref() {
        let salvage = pending_control.iter().enumerate().find_map(|(idx, e)| {
            match &e.cmd {
                Control::Abort { reason, by, .. } => Some((
                    idx,
                    e.seq,
                    RunStatus::Aborted,
                    reason.clone(),
                    EventPayload::RunAborted {
                        reason: reason.clone(),
                        by: *by,
                    },
                )),
                // A park is never salvaged: it is discarded by the cursor
                // move, the way the Retry behind it is.
                Control::Pause(req) if !req.is_park() => Some((
                    idx,
                    e.seq,
                    RunStatus::Paused,
                    req.journal_reason(),
                    req.to_event(),
                )),
                _ => None,
            }
        });
        if let Some((idx, _terminal_seq, status, reason, terminal)) = salvage
            && (status != RunStatus::Aborted || run_cancel.load(Ordering::SeqCst))
        {
            log.append(EventPayload::SupervisorAction {
                action: "retry_superseded_by_stop".into(),
                node: Some(blocked_node.clone()),
                detail: format!(
                    "a pending stop was applied before this command could be consumed, so it was discarded: {reason}"
                ),
            })?;
            log.append(terminal)?;
            let target = cursor_after(idx);
            write_control_cursor(run_dir, target)?;
            *control_cursor = Some(target);
            return Ok(ControlScan::Terminal(status));
        }
    }

    if patch_applied {
        // A migration - linear continuation from continue_from; the caller
        // drops the previous parallel branches and restarts the iteration.
        return Ok(ControlScan::Migrated { definition });
    }
    Ok(ControlScan::Proceed)
}

#[cfg(test)]
mod tests {
    use super::*;

    const PLAYBOOK: &str = r#"
schema: 1
id: w
name: W
version: 1.0.0
nodes:
  - { id: start, type: start }
  - { id: pause, type: wait, wait_for: { type: timer, seconds: 30 }, timeout_seconds: 3600 }
  - { id: done, type: finish, outcome: success }
edges:
  - { from: start, to: pause }
  - { from: pause, to: done }
"#;

    /// A run event lock that cannot be taken within its bounded wait
    /// discards the park (design D6 rule 4): the scan moves the cursor
    /// past it, journals nothing and goes on; it never ends the drive.
    #[test]
    fn a_park_whose_event_lock_is_busy_is_discarded() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let run_dir = root.join("run");
        std::fs::create_dir_all(&run_dir).unwrap();
        let mut log = EventLog::create(&run_dir).unwrap();
        log.append(EventPayload::WaitStarted {
            node: "pause".into(),
            kind: "timer".into(),
        })
        .unwrap();
        let events = read_all(&run_dir).unwrap();
        let due = crate::scheduler::wait_park::due_at_ms(
            &events,
            "pause",
            30,
            3600,
            apb_core::clock::now_ms(),
        )
        .unwrap();
        let n = crate::control::post_control(
            &run_dir,
            Control::Pause(crate::pause::PauseRequest {
                reason: Some(crate::pause::cause::wait_reason("pause")),
                by: Some(crate::pause::PauseBy::Engine),
                mode: crate::pause::PauseMode::Drain,
                until: Some(due),
                keep_lock: false,
            }),
        )
        .unwrap();
        // A live holder of the run's event lock.
        let _held = apb_core::fsutil::lock_dir(&run_dir, crate::stop::EVENT_LOCK).unwrap();
        let mut playbook = Playbook::from_yaml(PLAYBOOK).unwrap();
        let mut current = "pause".to_string();
        let mut cursor = None;
        let mut last = None;
        let scan = scan_control(
            root,
            &run_dir,
            &mut log,
            &RunConfig::default(),
            &mut playbook,
            &mut current,
            &mut cursor,
            &mut last,
            &AtomicBool::new(false),
        )
        .expect("a busy lock never ends the drive");
        assert!(matches!(scan, ControlScan::Proceed));
        assert_eq!(cursor, Some(n), "the cursor moved past the park");
        assert_eq!(read_control_cursor(&run_dir).unwrap(), Some(n));
        assert_eq!(
            read_all(&run_dir).unwrap().len(),
            events.len(),
            "nothing journaled"
        );
    }

    /// A stop numbered above a park appended after it (the stop posted
    /// without the numbering lock): consuming the stop discards the park in
    /// the same write, and the cursor stands at the stop, never below it,
    /// so a later drive never applies the stop again.
    #[test]
    fn a_stop_numbered_above_a_later_park_is_not_left_pending() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let run_dir = root.join("run");
        std::fs::create_dir_all(&run_dir).unwrap();
        let mut log = EventLog::create(&run_dir).unwrap();
        let entries = [
            crate::control::ControlEntry {
                seq: 2,
                cmd: Control::Abort {
                    reason: "stop".into(),
                    by: None,
                    graceful: false,
                },
            },
            crate::control::ControlEntry {
                seq: 1,
                cmd: Control::Pause(crate::pause::PauseRequest {
                    reason: Some(crate::pause::cause::wait_reason("pause")),
                    by: Some(crate::pause::PauseBy::Engine),
                    mode: crate::pause::PauseMode::Drain,
                    until: Some(u64::MAX),
                    keep_lock: false,
                }),
            },
        ];
        let lines: String = entries
            .iter()
            .map(|e| serde_json::to_string(e).unwrap() + "\n")
            .collect();
        std::fs::write(run_dir.join("control.jsonl"), lines).unwrap();
        let mut playbook = Playbook::from_yaml(PLAYBOOK).unwrap();
        let mut current = "pause".to_string();
        let mut cursor = None;
        let mut last = None;
        let scan = scan_control(
            root,
            &run_dir,
            &mut log,
            &RunConfig::default(),
            &mut playbook,
            &mut current,
            &mut cursor,
            &mut last,
            &AtomicBool::new(false),
        )
        .unwrap();
        assert!(matches!(scan, ControlScan::Terminal(RunStatus::Aborted)));
        assert_eq!(cursor, Some(2), "the cursor stands at the stop");
        assert_eq!(read_control_cursor(&run_dir).unwrap(), Some(2));
        assert!(
            crate::control::read_control_after(&run_dir, cursor)
                .unwrap()
                .is_empty(),
            "nothing left pending"
        );
    }

    const INBOX_PLAYBOOK: &str = r#"
schema: 1
id: w
name: W
version: 1.0.0
nodes:
  - { id: start, type: start }
  - id: pause
    type: wait
    wait_for:
      type: inbox
      connector: echo-hooks
      account: main
    timeout_seconds: 120
  - { id: done, type: finish, outcome: success }
edges:
  - { from: start, to: pause }
  - { from: pause, to: done }
"#;

    const WEBHOOK_PLAYBOOK: &str = r#"
schema: 1
id: w
name: W
version: 1.0.0
nodes:
  - { id: start, type: start }
  - { id: pause, type: wait, wait_for: { type: webhook, key: ci }, timeout_seconds: 60 }
  - { id: done, type: finish, outcome: success }
edges:
  - { from: start, to: pause }
  - { from: pause, to: done }
"#;

    /// The scan re-decides an inbox park with the wait's timeout (D4
    /// "Parking" item 2): `park_wait_of` gives `Some((timeout, timeout))`
    /// for an inbox wait and `None` for a webhook wait, and a park the
    /// drive posted over the inbox wait's timeout is applied (its until
    /// recomputes to the same value), while one over a webhook wait is
    /// discarded.
    #[test]
    fn an_inbox_park_is_re_decided_with_its_timeout() {
        // The shapes of `park_wait_of`.
        let inbox = Playbook::from_yaml(INBOX_PLAYBOOK).unwrap();
        assert_eq!(
            park_wait_of(&inbox, "pause"),
            Some((120, 120)),
            "an inbox wait parks for its timeout"
        );
        let webhook = Playbook::from_yaml(WEBHOOK_PLAYBOOK).unwrap();
        assert_eq!(
            park_wait_of(&webhook, "pause"),
            None,
            "a webhook never parks"
        );

        // The scan's re-decision path over a journaled inbox wait_started:
        // a park carrying the timeout's due time is applied with it.
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let run_dir = root.join("run");
        std::fs::create_dir_all(&run_dir).unwrap();
        let mut log = EventLog::create(&run_dir).unwrap();
        log.append(EventPayload::WaitStarted {
            node: "pause".into(),
            kind: "inbox".into(),
        })
        .unwrap();
        let events = read_all(&run_dir).unwrap();
        let due = crate::scheduler::wait_park::due_at_ms(
            &events,
            "pause",
            120,
            120,
            apb_core::clock::now_ms(),
        )
        .unwrap();
        crate::control::post_control(
            &run_dir,
            Control::Pause(crate::pause::PauseRequest {
                reason: Some(crate::pause::cause::wait_reason("pause")),
                by: Some(crate::pause::PauseBy::Engine),
                mode: crate::pause::PauseMode::Drain,
                until: Some(due),
                keep_lock: false,
            }),
        )
        .unwrap();
        let mut playbook = Playbook::from_yaml(INBOX_PLAYBOOK).unwrap();
        let mut current = "pause".to_string();
        let mut cursor = None;
        let mut last = None;
        let scan = scan_control(
            root,
            &run_dir,
            &mut log,
            &RunConfig::default(),
            &mut playbook,
            &mut current,
            &mut cursor,
            &mut last,
            &AtomicBool::new(false),
        )
        .unwrap();
        assert!(matches!(scan, ControlScan::Terminal(RunStatus::Paused)));
        let applied = read_all(&run_dir).unwrap();
        assert!(
            applied.iter().any(|e| matches!(
                &e.payload,
                EventPayload::RunPaused {
                    until: Some(u),
                    ..
                } if *u == due
            )),
            "the park is applied with the recomputed timeout due time"
        );

        // The same park over a webhook wait: `park_wait_of` is `None`, so
        // the recomputed due time never equals the posted one and the scan
        // discards the park, journaling nothing.
        let run_dir = root.join("run-wh");
        std::fs::create_dir_all(&run_dir).unwrap();
        let mut log = EventLog::create(&run_dir).unwrap();
        log.append(EventPayload::WaitStarted {
            node: "pause".into(),
            kind: "webhook".into(),
        })
        .unwrap();
        let before = read_all(&run_dir).unwrap().len();
        // Two parks, so the cursor's expected place is not the first
        // command's number, which every run's first command shares.
        let park = || {
            crate::control::post_control(
                &run_dir,
                Control::Pause(crate::pause::PauseRequest {
                    reason: Some(crate::pause::cause::wait_reason("pause")),
                    by: Some(crate::pause::PauseBy::Engine),
                    mode: crate::pause::PauseMode::Drain,
                    until: Some(due),
                    keep_lock: false,
                }),
            )
            .unwrap()
        };
        let first = park();
        let n = park();
        assert_ne!(first, n, "the second park has its own number");
        let mut playbook = Playbook::from_yaml(WEBHOOK_PLAYBOOK).unwrap();
        let mut current = "pause".to_string();
        let mut cursor = None;
        let mut last = None;
        let scan = scan_control(
            root,
            &run_dir,
            &mut log,
            &RunConfig::default(),
            &mut playbook,
            &mut current,
            &mut cursor,
            &mut last,
            &AtomicBool::new(false),
        )
        .unwrap();
        assert!(
            matches!(scan, ControlScan::Proceed),
            "the park is discarded"
        );
        assert_eq!(cursor, Some(n), "the cursor moved past the park");
        assert_eq!(
            read_all(&run_dir).unwrap().len(),
            before,
            "nothing journaled"
        );
    }
}
