//! The park waker's check for a parked inbox wait (inbox-waits D4,
//! "Parking" item 6): whether a delivery is waiting above the run's
//! journaled position, so the waker resumes the run before its `until`.
//! Depends on the connector inbox, the event types, `legacy_snapshot` and
//! [`super::authorize`]. The park's reason names the node the check reads
//! for ([`crate::pause::cause::parse_wait_reason`]), the one read of the
//! `pause` module.

use crate::event::{Event, EventPayload};
use apb_core::connector::inbox::Inbox;
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

/// The warnings this process has logged once, keyed by run directory, node
/// and kind. The waker polls every few seconds while a park lasts, so a
/// persisting refusal or read error is logged once, the way the waker's
/// control-file check logs once.
static LOGGED: Mutex<BTreeSet<(PathBuf, String, &'static str)>> = Mutex::new(BTreeSet::new());

/// Logs `line` once per (run directory, node, kind) per process.
fn log_once(run_dir: &Path, node: &str, kind: &'static str, line: String) {
    if let Ok(mut logged) = LOGGED.lock()
        && logged.insert((run_dir.to_path_buf(), node.to_string(), kind))
    {
        eprintln!("{line}");
    }
}

/// Whether the parked inbox wait the `run_paused` at `seq` names has a
/// delivery above the run's position: an event of a listed type received in
/// time, or a gap the resumed drive must journal. `inbox_state` holds what
/// the last read of the account log left behind ([`super::ReadState`]): a
/// failed read is never cached, and a successful read of an absent log is
/// cached like any other, so the check reads when the log changed and not
/// on every poll.
pub(crate) fn delivery_pending(
    root: &Path,
    run_dir: &Path,
    events: &[Event],
    seq: u64,
    inbox_state: &mut super::ReadState,
) -> bool {
    // 1. The node from the pause's reason, the wait from the run snapshot.
    // Anything but a wait reason (no event at `seq` included), or a node
    // whose wait is not an inbox one (a timer park included), is no
    // delivery: false.
    let Some(paused) = events.iter().find(|e| e.seq == seq) else {
        return false;
    };
    let EventPayload::RunPaused { reason, .. } = &paused.payload else {
        return false;
    };
    let Some(node) = crate::pause::cause::parse_wait_reason(reason) else {
        return false;
    };
    let Some(playbook) = crate::legacy_snapshot::load_run_playbook(run_dir) else {
        return false;
    };
    let Some(kind) = playbook.node(node).map(|n| &n.kind) else {
        return false;
    };
    let apb_core::schema::NodeKind::Wait {
        wait_for: apb_core::schema::WaitFor::Inbox(w),
        timeout_seconds,
        ..
    } = kind
    else {
        return false;
    };
    // 2. Authorization: a refusal never wakes the run, logged once.
    if let Err(why) = super::authorize(root, run_dir, node, w) {
        let run_id = run_dir
            .file_name()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default();
        log_once(
            run_dir,
            node,
            "auth",
            format!("apb inbox_wait_unauthorized run={run_id} node={node}: {why}"),
        );
        return false;
    }
    // 3. The position: what the visit has already examined.
    let position = super::position(events, node).unwrap_or(0);
    // 4. Read only when due: the first call, after a failed read, or when
    // the log changed since the last successful read. An open failure
    // reads as a failed read.
    let Ok(inbox) = Inbox::open(&w.connector, &w.account) else {
        *inbox_state = super::ReadState::NotRead;
        log_once(
            run_dir,
            node,
            "read",
            format!("apb inbox_wait_read_error node={node}: the account inbox does not open"),
        );
        return false;
    };
    let stamp = inbox.log_stamp();
    if !inbox_state.read_due(stamp) {
        // Nothing stored since the last successful read.
        return false;
    }
    let read = inbox.read_events();
    #[cfg(any(test, feature = "testing"))]
    super::count_read(run_dir, &w.account, false, read.is_err());
    match read {
        Err(e) => {
            *inbox_state = super::ReadState::NotRead;
            log_once(
                run_dir,
                node,
                "read",
                format!("apb inbox_wait_read_error node={node}: {e}"),
            );
            false
        }
        Ok(list) => {
            *inbox_state = super::ReadState::of(stamp);
            // An event above the position that passes the types and was
            // received in time, or a loss the resumed drive must journal.
            let typed = list.iter().any(|e| {
                e.seq > position
                    && apb_core::connector::event_filter::type_passes(
                        &w.types,
                        e.event_type.as_deref(),
                    )
                    && super::in_time(events, node, *timeout_seconds, e.received_at)
            });
            typed
                || apb_core::connector::inbox::lost_prefix(position, list.first().map(|e| e.seq))
                    .is_some()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::inbox_wait::park_fixture::{self, Fx};

    /// Events above the position wake the run, events at or below it do
    /// not.
    #[test]
    fn delivery_pending_compares_with_the_position() {
        park_fixture::in_child(
            park_fixture::PENDING_PREFIX,
            "delivery_pending_compares_with_the_position",
            || {
                let fx = Fx::parked("", 3);
                for n in 0..3 {
                    fx.store(serde_json::json!({ "n": n }));
                }
                let events = park_fixture::events(&fx);
                // First call: reads. Every stored event sits at or below
                // the start line's 3.
                let mut state = crate::inbox_wait::ReadState::NotRead;
                assert!(!delivery_pending(
                    &fx.root,
                    &fx.run_dir,
                    &events,
                    park_fixture::pause_seq(&fx),
                    &mut state
                ));
                let stamp = match state {
                    crate::inbox_wait::ReadState::Read { len, modified } => (len, modified),
                    _ => panic!("the first call read"),
                };
                // Nothing changed: no read, no wake.
                assert!(!delivery_pending(
                    &fx.root,
                    &fx.run_dir,
                    &events,
                    park_fixture::pause_seq(&fx),
                    &mut crate::inbox_wait::ReadState::of(Some(stamp))
                ));
                // A matching event above the position wakes the run.
                fx.store(serde_json::json!({ "n": 3 }));
                let events = park_fixture::events(&fx);
                assert!(delivery_pending(
                    &fx.root,
                    &fx.run_dir,
                    &events,
                    park_fixture::pause_seq(&fx),
                    &mut crate::inbox_wait::ReadState::of(Some(stamp))
                ));
            },
        );
    }

    /// A prior successful read with an unchanged stamp does not read
    /// again: the same bytes with a waking change hidden behind the old
    /// stamp stay undiscovered. A changed stamp reads.
    #[test]
    fn delivery_pending_reads_only_when_the_log_changed() {
        park_fixture::in_child(
            park_fixture::PENDING_PREFIX,
            "delivery_pending_reads_only_when_the_log_changed",
            || {
                let fx = Fx::parked("order", 0);
                // An event of an unlisted type: the read finds nothing.
                fx.store_typed("noise", serde_json::json!({ "n": 0 }));
                let events = park_fixture::events(&fx);
                let mut state = crate::inbox_wait::ReadState::NotRead;
                assert!(!delivery_pending(
                    &fx.root,
                    &fx.run_dir,
                    &events,
                    park_fixture::pause_seq(&fx),
                    &mut state
                ));
                let stamp = match state {
                    crate::inbox_wait::ReadState::Read { len, modified } => (len, modified),
                    _ => panic!("the first call read"),
                };
                // The same length, a waking change (the event is of the
                // listed type now), the old stamp restored: the log reads
                // as unchanged, so no read happens and the change stays
                // hidden.
                let mut changed = fx.log_bytes();
                let from = b"\"noise\"".len();
                let at = changed
                    .windows(from)
                    .position(|w| w == b"\"noise\"")
                    .unwrap();
                changed[at..at + from].copy_from_slice(b"\"order\"");
                fx.rewrite_log_with_stamp(&changed, stamp);
                assert!(!delivery_pending(
                    &fx.root,
                    &fx.run_dir,
                    &events,
                    park_fixture::pause_seq(&fx),
                    &mut crate::inbox_wait::ReadState::of(Some(stamp))
                ));
                // A changed stamp reads: the waking event is found.
                fx.rewrite_log_with_stamp(&changed, (stamp.0, std::time::SystemTime::now()));
                assert!(delivery_pending(
                    &fx.root,
                    &fx.run_dir,
                    &events,
                    park_fixture::pause_seq(&fx),
                    &mut crate::inbox_wait::ReadState::of(Some(stamp))
                ));
            },
        );
    }

    /// An account with no log at all is read once: a successful read of
    /// an absent log is cached like any other, so the check does not read
    /// on every poll.
    #[test]
    fn delivery_pending_reads_an_absent_log_once() {
        park_fixture::in_child(
            park_fixture::PENDING_PREFIX,
            "delivery_pending_reads_an_absent_log_once",
            || {
                let fx = Fx::parked("", 0);
                let events = park_fixture::events(&fx);
                assert!(
                    !fx.inbox().dir().join("events.jsonl").exists(),
                    "the account has no log yet"
                );
                let mut state = crate::inbox_wait::ReadState::NotRead;
                for _ in 0..5 {
                    assert!(!delivery_pending(
                        &fx.root,
                        &fx.run_dir,
                        &events,
                        park_fixture::pause_seq(&fx),
                        &mut state
                    ));
                }
                assert_eq!(
                    crate::inbox_wait::read_counts(&fx.run_dir).reads,
                    1,
                    "the absent log was read once, not on every poll"
                );
                assert_eq!(state, crate::inbox_wait::ReadState::ReadAbsent);
            },
        );
    }

    /// The wait's `types` list applies: an unlisted type does not wake, a
    /// listed one does, and an untyped event passes only an empty list.
    #[test]
    fn delivery_pending_applies_the_types() {
        park_fixture::in_child(
            park_fixture::PENDING_PREFIX,
            "delivery_pending_applies_the_types",
            || {
                let fx = Fx::parked("order", 0);
                let events = park_fixture::events(&fx);
                // The first call reads the log, which holds nothing yet
                // (an empty log keeps no stamp, so every call here reads).
                assert!(
                    !delivery_pending(
                        &fx.root,
                        &fx.run_dir,
                        &events,
                        park_fixture::pause_seq(&fx),
                        &mut crate::inbox_wait::ReadState::NotRead
                    ),
                    "the first call reads an empty log"
                );
                // An unlisted type does not wake.
                fx.store_typed("noise", serde_json::json!({ "n": 1 }));
                let events = park_fixture::events(&fx);
                assert!(!delivery_pending(
                    &fx.root,
                    &fx.run_dir,
                    &events,
                    park_fixture::pause_seq(&fx),
                    &mut crate::inbox_wait::ReadState::NotRead
                ));
                // An untyped event does not wake a typed wait.
                fx.store(serde_json::json!({ "n": 2 }));
                let events = park_fixture::events(&fx);
                assert!(!delivery_pending(
                    &fx.root,
                    &fx.run_dir,
                    &events,
                    park_fixture::pause_seq(&fx),
                    &mut crate::inbox_wait::ReadState::NotRead
                ));
                // The listed type wakes.
                fx.store_typed("order", serde_json::json!({ "n": 3 }));
                let events = park_fixture::events(&fx);
                assert!(delivery_pending(
                    &fx.root,
                    &fx.run_dir,
                    &events,
                    park_fixture::pause_seq(&fx),
                    &mut crate::inbox_wait::ReadState::NotRead
                ));

                // An empty list passes everything, an untyped event
                // included.
                let plain = Fx::parked("", 0);
                plain.store(serde_json::json!({ "n": 1 }));
                let events = park_fixture::events(&plain);
                assert!(delivery_pending(
                    &plain.root,
                    &plain.run_dir,
                    &events,
                    park_fixture::pause_seq(&plain),
                    &mut crate::inbox_wait::ReadState::NotRead
                ));
            },
        );
    }

    /// A gap the resumed drive must journal wakes the run: the oldest
    /// stored seq sits above the position plus one.
    #[test]
    fn delivery_pending_wakes_for_a_gap() {
        park_fixture::in_child(
            park_fixture::PENDING_PREFIX,
            "delivery_pending_wakes_for_a_gap",
            || {
                let fx = Fx::parked("", 0);
                // Retention's outcome, written as it leaves the log: seqs
                // 5 and 6, 1 to 4 gone.
                fx.store_raw(5, 6, serde_json::json!({ "n": 0 }));
                let events = park_fixture::events(&fx);
                assert!(delivery_pending(
                    &fx.root,
                    &fx.run_dir,
                    &events,
                    park_fixture::pause_seq(&fx),
                    &mut crate::inbox_wait::ReadState::NotRead
                ));
                // Everything after the position stored: no gap, no wake.
                let plain = Fx::parked("", 2);
                plain.store_raw(1, 2, serde_json::json!({ "n": 0 }));
                let events = park_fixture::events(&plain);
                assert!(!delivery_pending(
                    &plain.root,
                    &plain.run_dir,
                    &events,
                    park_fixture::pause_seq(&plain),
                    &mut crate::inbox_wait::ReadState::NotRead
                ));
            },
        );
    }

    /// A timer park is untouched: no inbox is read, nothing wakes the run.
    #[test]
    fn delivery_pending_ignores_a_timer_park() {
        park_fixture::in_child(
            park_fixture::PENDING_PREFIX,
            "delivery_pending_ignores_a_timer_park",
            || {
                let fx = Fx::timer_parked();
                // A matching delivery sits above any position; the timer
                // park must not wake for it.
                fx.store(serde_json::json!({ "n": 0 }));
                let events = park_fixture::events(&fx);
                assert!(!delivery_pending(
                    &fx.root,
                    &fx.run_dir,
                    &events,
                    park_fixture::pause_seq(&fx),
                    &mut crate::inbox_wait::ReadState::NotRead
                ));
            },
        );
    }

    /// An event received after the wait's deadline does not wake the run:
    /// receipt decides, the poll's lateness does not. One received before
    /// it does.
    #[test]
    fn delivery_pending_ignores_a_late_event() {
        park_fixture::in_child(
            park_fixture::PENDING_PREFIX,
            "delivery_pending_ignores_a_late_event",
            || {
                // Stored 60 s after the wait started, well past the 10 s
                // deadline.
                let fx = Fx::parked("", 0);
                let late = fx.ws_ms + 60_000;
                fx.store_at(u64::try_from(late).unwrap(), serde_json::json!({ "n": 0 }));
                let events = park_fixture::events(&fx);
                assert!(!delivery_pending(
                    &fx.root,
                    &fx.run_dir,
                    &events,
                    park_fixture::pause_seq(&fx),
                    &mut crate::inbox_wait::ReadState::NotRead
                ));
                // An event received before the deadline wakes (another run:
                // a receipt never goes back).
                let early = Fx::parked("", 0);
                let in_time = early.ws_ms + 1_000;
                early.store_at(
                    u64::try_from(in_time).unwrap(),
                    serde_json::json!({ "n": 1 }),
                );
                let events = park_fixture::events(&early);
                assert!(delivery_pending(
                    &early.root,
                    &early.run_dir,
                    &events,
                    park_fixture::pause_seq(&early),
                    &mut crate::inbox_wait::ReadState::NotRead
                ));
            },
        );
    }

    /// A failed read is never cached: the first poll reads and fails, the
    /// file is not touched again, and the next poll reads again and finds
    /// the event.
    #[test]
    fn delivery_pending_retries_a_failed_read() {
        park_fixture::in_child(
            park_fixture::PENDING_PREFIX,
            "delivery_pending_retries_a_failed_read",
            || {
                use std::io::Write as _;
                let fx = Fx::parked("", 0);
                // A torn line fails the read.
                let path = fx.inbox().dir().join("events.jsonl");
                std::fs::create_dir_all(fx.inbox().dir()).unwrap();
                let torn = br#"{"seq":1,"received_at":1,"provider_id":"torn","body":{"from":"x""#;
                {
                    let mut f = std::fs::OpenOptions::new()
                        .create(true)
                        .append(true)
                        .open(&path)
                        .unwrap();
                    f.write_all(torn).unwrap();
                    f.flush().unwrap();
                }
                let events = park_fixture::events(&fx);
                let mut state = crate::inbox_wait::ReadState::NotRead;
                assert!(!delivery_pending(
                    &fx.root,
                    &fx.run_dir,
                    &events,
                    park_fixture::pause_seq(&fx),
                    &mut state
                ));
                assert_eq!(
                    state,
                    crate::inbox_wait::ReadState::NotRead,
                    "a failed read is not cached"
                );
                assert_eq!(
                    crate::inbox_wait::read_counts(&fx.run_dir).reads,
                    1,
                    "the first call read the log once"
                );
                // The file is not touched again: the next poll reads it as
                // it stands, with the state the failed call left.
                let torn_len = std::fs::metadata(&path).unwrap().len();
                let torn_stamp = fx.inbox().log_stamp();
                assert!(
                    !delivery_pending(
                        &fx.root,
                        &fx.run_dir,
                        &events,
                        park_fixture::pause_seq(&fx),
                        &mut state
                    ),
                    "the torn line still fails"
                );
                assert_eq!(
                    std::fs::metadata(&path).unwrap().len(),
                    torn_len,
                    "the log did not change between the two reads"
                );
                assert_eq!(
                    fx.inbox().log_stamp(),
                    torn_stamp,
                    "the log's stamp did not change between the two reads"
                );
                assert_eq!(
                    crate::inbox_wait::read_counts(&fx.run_dir).reads,
                    2,
                    "the failed read was not cached: the second call read again"
                );
                // The line is completed: the next poll reads and finds the
                // event, which arrived at 1 ms, before the deadline.
                {
                    let mut f = std::fs::OpenOptions::new()
                        .append(true)
                        .open(&path)
                        .unwrap();
                    f.write_all(b"}}\n").unwrap();
                    f.flush().unwrap();
                }
                assert!(delivery_pending(
                    &fx.root,
                    &fx.run_dir,
                    &events,
                    park_fixture::pause_seq(&fx),
                    &mut crate::inbox_wait::ReadState::NotRead
                ));
            },
        );
    }
}
