use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::error::EngineError;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "cmd", rename_all = "snake_case")]
pub enum Control {
    Retry {
        node: String,
        prompt_override: Option<String>,
    },
    ContinueFrom {
        node: String,
    },
    /// Pause the run (issue #200). The payload's fields are all optional,
    /// so the historical `{"cmd":"pause"}` line still parses.
    Pause(crate::pause::PauseRequest),
    Abort {
        reason: String,
        /// Who asked for the stop; journaled on the `run_aborted` it
        /// becomes. Absent in older control files. Additive.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        by: Option<crate::pause::kind::PauseBy>,
        /// Kill the attempts in flight with SIGTERM and the grace period of
        /// a `--now` pause instead of at once. Absent (false) in older
        /// control files and for `apb stop`. Additive.
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        graceful: bool,
    },
    ContextAppend {
        note: String,
    },
    Progress {
        done: u64,
        total: u64,
        #[serde(default)]
        label: Option<String>,
        /// The node that produced this report (issue #78), mirroring
        /// `Interrupt.node`. With concurrent branches in flight the drive's
        /// current attribution is wrong by construction: the report belongs to
        /// a batch member, not to whatever the drive loop is holding. `None` is
        /// the historical shape, and what an older `control.jsonl` line
        /// deserializes to: the drive stamps whatever node it is currently
        /// attributing to, as it always did.
        #[serde(default)]
        node: Option<String>,
    },
    Patch {
        version: String,
        classification: String,
        continue_from: String,
    },
    /// Terminate the RUNNING attempt's agent process (finding 7 of issue #42,
    /// third item of issue #40). Posted by a supervisor that has decided -
    /// typically after a stall anomaly woke it - to break a wedged attempt
    /// rather than wait out a hang that may never end. The attempt's own poll
    /// loop observes this live, SIGKILLs the agent's process group, and
    /// journals it; because a signal-terminated attempt is journaled failed
    /// (the exit-by-signal path), ordinary retry/fallback/patch then proceeds
    /// at the next attempt boundary. It NEVER aborts the run (that stays
    /// `Abort`'s job) and it never applies a patch itself - it only forces the
    /// attempt boundary to happen. An interrupt with no attempt running is a
    /// no-op, consumed at the next node boundary.
    ///
    /// `node` targets ONE node's running attempt (spec 2026-08-05 section 1.6):
    /// with concurrent branches in flight, only the named node's attempt observes
    /// it and dies, and its siblings run on untouched. `None` - the historical
    /// shape, and what an older `control.jsonl` line deserializes to through
    /// `#[serde(default)]` - keeps the documented BROADCAST semantics: every
    /// attempt currently running in the run observes the same entry and dies.
    Interrupt {
        reason: String,
        #[serde(default)]
        node: Option<String>,
    },
    /// Rebind a node's executor profile to a new one mid-run (issue #45 finding
    /// 5). Posted by a supervisor holding the `rebind` capability once the policy
    /// gate has trust-verified the new profile bundle. `bundle` is that verified
    /// digest, pinned so the drive loop re-snapshots the profile and refuses any
    /// drift between gate and apply (anti-TOCTOU, exactly as run start). Applied
    /// in place - the node's future attempts use the new binding - and never
    /// terminal; a rebind that fails re-verification is journaled `RebindRejected`
    /// and the node keeps its old binding.
    Rebind {
        node: String,
        profile: String,
        scope: apb_core::profile::ProfileScope,
        bundle: String,
        #[serde(default)]
        reason: Option<String>,
    },
}

impl Control {
    /// Stable machine-facing name of the command, matching its serialized `cmd`
    /// tag. One definition shared by every site that labels a control entry (the
    /// doctor's pending-control line, the drive loop's `control_received`
    /// acknowledgment), so the names can never drift from the tags or each other.
    pub fn kind(&self) -> &'static str {
        match self {
            Control::Retry { .. } => "retry",
            Control::ContinueFrom { .. } => "continue_from",
            Control::Pause(_) => "pause",
            Control::Abort { .. } => "abort",
            Control::ContextAppend { .. } => crate::event::supervisor_action::CONTEXT_APPEND,
            Control::Progress { .. } => "progress",
            Control::Patch { .. } => "patch",
            Control::Interrupt { .. } => "interrupt",
            Control::Rebind { .. } => "rebind",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ControlEntry {
    pub seq: u64,
    #[serde(flatten)]
    pub cmd: Control,
}

/// The lock every writer of this apb and later takes to number its entry
/// and append it under: the count of the lines and the append are one
/// step, so no two entries share a number and every number is the append
/// order. A leaf below the run's event lock (`crate::stop::EVENT_LOCK`):
/// the callers that hold that lock when they post (`request_pause`,
/// `stop_run_with`, a stop's `abort_children`) take this one after it,
/// never the reverse, and `post_control` takes nothing else while it holds
/// it. Readers take no lock. An apb older than this change posts without
/// it, which is the standing limit that two binaries of different
/// versions never operate on one live run at once.
pub const CONTROL_LOCK: &str = "control.jsonl.lock";

/// How far above the next free number a stop posted without
/// [`CONTROL_LOCK`] is numbered. Locked writers keep numbering from the
/// file's highest entry while the stop is between its count and its
/// append; they could reach its number only by posting this many entries
/// in that moment, so no locked writer publishes the stop's number first.
/// The scan reads entries in file order and moves its cursor to the
/// highest number it consumed, so the gap costs nothing.
pub const UNLOCKED_STOP_GAP: u64 = 1 << 20;

pub fn post_control(run_dir: &Path, cmd: Control) -> Result<u64, EngineError> {
    post_control_within(run_dir, cmd, false).map(|seq| seq.expect("a busy lock is an error here"))
}

/// [`post_control`] for an entry that may be left out: a numbering lock
/// still held by a live owner after its bound is `Ok(None)`, with nothing
/// appended (the engine's park, which the wait decides again at its next
/// poll). Every other failure is the error [`post_control`] gives.
pub fn post_control_unless_busy(run_dir: &Path, cmd: Control) -> Result<Option<u64>, EngineError> {
    post_control_within(run_dir, cmd, true)
}

fn post_control_within(
    run_dir: &Path,
    cmd: Control,
    busy_is_none: bool,
) -> Result<Option<u64>, EngineError> {
    crate::driver::ensure_run_dir(run_dir)?;

    // The number is the append order: number and append under one lock, the
    // line and its newline with one `write_all`.
    let lock = apb_core::fsutil::hold_lock(run_dir, CONTROL_LOCK);
    let seq = match lock {
        Ok(_) => next_seq(run_dir)?,
        Err(e) if busy_is_none && e.kind() == std::io::ErrorKind::WouldBlock => {
            return Ok(None);
        }
        // A stop is never refused for this lock (design D8): an Abort that
        // cannot take it within its bound is appended without it, numbered
        // [`UNLOCKED_STOP_GAP`] above the next free number. A holder that
        // counted already, and every locked writer that counts before the
        // stop is in the file, numbers below it; one that counts after
        // numbers above it. Every other writer keeps the lock's error.
        Err(_) if matches!(cmd, Control::Abort { .. }) => {
            next_seq(run_dir)?.saturating_add(UNLOCKED_STOP_GAP)
        }
        Err(e) => return Err(e.into()),
    };

    let entry = ControlEntry { seq, cmd };
    let mut line = serde_json::to_string(&entry).map_err(|e| EngineError::Yaml(e.to_string()))?;
    line.push('\n');

    let path = run_dir.join("control.jsonl");
    let mut file = OpenOptions::new().create(true).append(true).open(&path)?;
    file.write_all(line.as_bytes())?;
    file.flush()?;
    drop(lock);

    Ok(Some(seq))
}

/// The next free entry number: one above the highest in the file, 0 for
/// an empty one. Equal to the line count while every writer holds
/// [`CONTROL_LOCK`]; one above the stop after an unlocked stop.
fn next_seq(run_dir: &Path) -> Result<u64, EngineError> {
    Ok(read_control_after(run_dir, None)?
        .iter()
        .map(|e| e.seq + 1)
        .max()
        .unwrap_or(0))
}

/// Reads the persisted control cursor for this run (`runs/<id>/control.cursor`):
/// the seq of the last control.jsonl entry any drive loop of this run has
/// already applied. A missing file means no control entry has ever been
/// applied yet - `None`, the exact state a fresh drive starts in. A corrupt
/// file is a hard error rather than silently degrading to `None`: silently
/// replaying from the beginning is exactly the duplicate-application bug this
/// cursor exists to prevent.
pub fn read_control_cursor(run_dir: &Path) -> Result<Option<u64>, EngineError> {
    let path = run_dir.join("control.cursor");
    if !path.is_file() {
        return Ok(None);
    }
    let raw = std::fs::read_to_string(&path)?;
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Ok(None);
    }
    let seq = trimmed
        .parse::<u64>()
        .map_err(|e| EngineError::Yaml(format!("invalid control cursor `{trimmed}`: {e}")))?;
    Ok(Some(seq))
}

/// The cursor is a SINGLE SCALAR, and that shapes every site that writes it.
///
/// Setting it to seq N declares every entry up to and including N applied.
/// There is no way to record "N is applied but N-1 is not", so a site that has
/// to skip ahead to a specific entry - a stop that must take effect while an
/// unconsumable `Retry` sits ahead of it in the queue - necessarily discards
/// what it skips over. The two options are therefore:
///
///   * advance, and DISCARD the entries in between. Correct when the entry we
///     skip to is terminal (the run stops, so a queued Retry has nothing left
///     to retry), but the discard must be made visible in the journal rather
///     than happening silently - see the `retry_superseded_by_stop`
///     `SupervisorAction` written by the drive loop.
///   * do not advance, and REPLAY the entry on the next drive. Correct only
///     when the replay is idempotent and self-limiting. `stop_run`'s dead-run
///     path takes this option: it writes the terminal `RunAborted` without a
///     cursor, and the next drive re-reads the abort, applies it through the
///     drive loop's own Abort arm (which DOES advance the cursor) and returns.
///     That replay clears itself after one pass; a path whose replay does not
///     advance the cursor would loop forever instead.
///
/// Persists the control cursor right after a control.jsonl entry is applied,
/// atomically (temp + rename, per `apb_core::fsutil::atomic_write`) so a crash
/// mid-write never leaves a torn cursor file for a later drive to misread.
/// Every site that advances the in-memory cursor (the drive-loop top-of-loop
/// scan, `await_control`, `drain_progress_after_execute`) calls this so a
/// resumed drive - or a fresh wake within the same drive - never re-applies an
/// entry a prior pass already consumed.
pub fn write_control_cursor(run_dir: &Path, seq: u64) -> Result<(), EngineError> {
    Ok(apb_core::fsutil::atomic_write_under(
        run_dir,
        &run_dir.join("control.cursor"),
        seq.to_string().as_bytes(),
    )?)
}

/// The seq of the first UNAPPLIED `Abort` in this run's control queue, if any.
///
/// A resume of a run with a pending stop terminates immediately: the drive
/// loop's top-of-loop scan applies the abort before it executes anything, so
/// the resume looks like it did nothing at all. Callers that acknowledge a
/// resume to a human (`apb resume`, the `run_resume` MCP tool) check this
/// first so they can say the resume stopped on a pending stop, and that a
/// second resume is what continues past it - see the release notes' stop, note,
/// resume pattern.
pub fn pending_stop_seq(run_dir: &Path) -> Result<Option<u64>, EngineError> {
    Ok(pending_stop(run_dir)?.map(|(seq, _)| seq))
}

/// [`pending_stop_seq`] with who asked for the stop.
pub fn pending_stop(
    run_dir: &Path,
) -> Result<Option<(u64, Option<crate::pause::kind::PauseBy>)>, EngineError> {
    let cursor = read_control_cursor(run_dir)?;
    Ok(read_control_after(run_dir, cursor)?
        .iter()
        .find_map(|e| match &e.cmd {
            Control::Abort { by, .. } => Some((e.seq, *by)),
            _ => None,
        }))
}

/// The first UNAPPLIED `Pause` in this run's control queue, if any (issue
/// #200): what a live driver is draining toward, reported as `pausing`.
pub fn pending_pause(run_dir: &Path) -> Result<Option<crate::pause::PauseRequest>, EngineError> {
    let cursor = read_control_cursor(run_dir)?;
    Ok(read_control_after(run_dir, cursor)?
        .into_iter()
        .find_map(|e| match e.cmd {
            // A park is not a pause in progress: the drive decides it at
            // its wait, and a pending park reads as no `pausing`.
            Control::Pause(req) if !req.is_park() => Some(req),
            _ => None,
        }))
}

pub fn read_control_after(
    run_dir: &Path,
    after_seq: Option<u64>,
) -> Result<Vec<ControlEntry>, EngineError> {
    let path = run_dir.join("control.jsonl");
    if !path.is_file() {
        return Ok(Vec::new());
    }

    let mut out = Vec::new();
    for line in BufReader::new(File::open(&path)?).lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let entry: ControlEntry =
            serde_json::from_str(&line).map_err(|e| EngineError::Yaml(e.to_string()))?;

        if let Some(threshold) = after_seq {
            if entry.seq > threshold {
                out.push(entry);
            }
        } else {
            out.push(entry);
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::{Control, pending_pause, post_control, read_control_after};

    /// An `abort` line an older apb wrote (no `graceful`) decodes as an
    /// immediate abort, and an immediate abort writes no `graceful`.
    #[test]
    fn an_old_abort_line_decodes_as_immediate() {
        let old: Control =
            serde_json::from_str(r#"{"cmd":"abort","reason":"stop requested"}"#).unwrap();
        assert!(matches!(
            old,
            Control::Abort {
                graceful: false,
                ..
            }
        ));
        let line = serde_json::to_string(&old).unwrap();
        assert!(!line.contains("graceful"), "{line}");
        let new: Control =
            serde_json::from_str(r#"{"cmd":"abort","reason":"x","graceful":true}"#).unwrap();
        assert!(matches!(new, Control::Abort { graceful: true, .. }));
    }

    /// Eight threads released by one barrier post 25 entries each into one
    /// run directory: the numbers are exactly 0 to 199, each once, and the
    /// file's lines carry them in increasing order. A race, so on main's
    /// unlocked count-then-append it fails often but not on every run; it
    /// proves distinct numbers only, never an order of two particular
    /// posts.
    #[test]
    fn two_concurrent_posts_get_distinct_increasing_numbers() {
        let dir = tempfile::tempdir().unwrap();
        let run_dir = dir.path().join("r");
        std::fs::create_dir_all(&run_dir).unwrap();
        const THREADS: usize = 8;
        const EACH: u64 = 25;
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(THREADS));
        let mut handles = Vec::new();
        for _ in 0..THREADS {
            let barrier = barrier.clone();
            let run_dir = run_dir.clone();
            handles.push(std::thread::spawn(move || {
                barrier.wait();
                for _ in 0..EACH {
                    post_control(
                        &run_dir,
                        Control::Progress {
                            done: 1,
                            total: 1,
                            label: None,
                            node: None,
                        },
                    )
                    .unwrap();
                }
            }));
        }
        for h in handles {
            h.join().unwrap();
        }
        let entries = read_control_after(&run_dir, None).unwrap();
        let mut seqs: Vec<u64> = entries.iter().map(|e| e.seq).collect();
        seqs.sort_unstable();
        let expected: Vec<u64> = (0..THREADS as u64 * EACH).collect();
        assert_eq!(seqs, expected, "each number exactly once");
        // The file's lines carry them in increasing order.
        let in_order: Vec<u64> = entries.iter().map(|e| e.seq).collect();
        let mut sorted = in_order.clone();
        sorted.sort_unstable();
        assert_eq!(in_order, sorted, "append order is number order");
    }

    /// A held numbering lock never refuses a stop: the Abort is appended
    /// without it, numbered above every entry, while any other writer
    /// still gets the lock's error.
    #[test]
    fn a_stop_is_posted_while_the_numbering_lock_is_held() {
        let dir = tempfile::tempdir().unwrap();
        let run_dir = dir.path().join("r");
        std::fs::create_dir_all(&run_dir).unwrap();
        let progress = || Control::Progress {
            done: 1,
            total: 1,
            label: None,
            node: None,
        };
        assert_eq!(post_control(&run_dir, progress()).unwrap(), 0);
        let held = apb_core::fsutil::hold_lock(&run_dir, super::CONTROL_LOCK).unwrap();
        assert!(post_control(&run_dir, progress()).is_err());
        let seq = post_control(
            &run_dir,
            Control::Abort {
                reason: "stop".into(),
                by: None,
                graceful: false,
            },
        )
        .unwrap();
        let stop = 1 + super::UNLOCKED_STOP_GAP;
        assert_eq!(seq, stop, "far above the next free number");
        // The holder, which counted 1 before the stop, appends after it and
        // still numbers below it; it is a number no one else holds.
        let mut f = std::fs::OpenOptions::new()
            .append(true)
            .open(run_dir.join("control.jsonl"))
            .unwrap();
        use std::io::Write as _;
        writeln!(
            f,
            "{}",
            serde_json::to_string(&super::ControlEntry {
                seq: 1,
                cmd: progress(),
            })
            .unwrap()
        )
        .unwrap();
        drop(held);
        // The next locked writer numbers above the stop.
        assert_eq!(post_control(&run_dir, progress()).unwrap(), stop + 1);
        let seqs: Vec<u64> = read_control_after(&run_dir, None)
            .unwrap()
            .iter()
            .map(|e| e.seq)
            .collect();
        assert_eq!(seqs, vec![0, stop, 1, stop + 1], "every number once");
    }

    /// An entry that may be left out is not appended while the numbering
    /// lock is held past its bound: `Ok(None)` and nothing in the file;
    /// once the lock is free it is posted as usual.
    #[test]
    fn an_optional_entry_is_left_out_while_the_numbering_lock_is_held() {
        let dir = tempfile::tempdir().unwrap();
        let run_dir = dir.path().join("r");
        std::fs::create_dir_all(&run_dir).unwrap();
        let park = || {
            Control::Pause(crate::pause::PauseRequest {
                reason: Some(crate::pause::cause::wait_reason("pause")),
                by: Some(crate::pause::PauseBy::Engine),
                until: Some(1),
                ..crate::pause::PauseRequest::default()
            })
        };
        let held = apb_core::fsutil::hold_lock(&run_dir, super::CONTROL_LOCK).unwrap();
        assert_eq!(
            super::post_control_unless_busy(&run_dir, park()).unwrap(),
            None
        );
        assert!(read_control_after(&run_dir, None).unwrap().is_empty());
        drop(held);
        assert_eq!(
            super::post_control_unless_busy(&run_dir, park()).unwrap(),
            Some(0)
        );
    }

    /// A pending park reads as no `pausing`: `pending_pause` skips it, the
    /// drive decides it at the wait. A park followed by an operator's
    /// pause reports the operator's.
    #[test]
    fn a_pending_park_is_not_a_pending_pause() {
        let dir = tempfile::tempdir().unwrap();
        let run_dir = dir.path().join("r");
        std::fs::create_dir_all(&run_dir).unwrap();
        post_control(
            &run_dir,
            Control::Pause(crate::pause::PauseRequest {
                reason: Some(crate::pause::cause::wait_reason("pause")),
                by: Some(crate::pause::kind::PauseBy::Engine),
                mode: crate::pause::PauseMode::Drain,
                until: Some(1_000),
                keep_lock: false,
            }),
        )
        .unwrap();
        assert_eq!(pending_pause(&run_dir).unwrap(), None);
        let operator = crate::pause::PauseRequest {
            reason: Some("looking".into()),
            by: Some(crate::pause::kind::PauseBy::Operator),
            ..crate::pause::PauseRequest::default()
        };
        post_control(&run_dir, Control::Pause(operator)).unwrap();
        let pending = pending_pause(&run_dir).unwrap().unwrap();
        assert_eq!(pending.by, Some(crate::pause::kind::PauseBy::Operator));
    }
}
