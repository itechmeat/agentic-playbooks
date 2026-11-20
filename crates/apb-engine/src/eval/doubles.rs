//! The run side of connector doubles (an eval case's `connectors:`): the
//! answer of each call of a doubled run, the capture of the calls the
//! doubles answered, and the reader the checks use.
//!
//! A call is answered in two durable steps. [`reserve`] picks its answer
//! and appends a `reserved` line, synced before the call is dispatched, and
//! holds an owner lock for it; [`deliver`] appends the `delivered` line,
//! synced before the answer is handed to the caller, and then releases the
//! owner lock. A caller that died between the two left a reservation whose
//! owner lock is free: the same call (same key) retried gets that
//! reservation back, with no `times` used again. The capture never holds
//! raw arguments: they go through the run's storage redactor, and the
//! call's identity is a MAC under this installation's key.

use std::io::Write;
use std::path::Path;

use apb_core::connector::def::MockSpec;
use apb_core::eval_doubles::Doubles;
use apb_core::fsutil::HeldLock;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::connector::result::{CallError, CallErrorCode};
use crate::event::{Event, EventPayload};

/// The capture file of a doubled run, under the run directory.
pub const CALLS_FILE: &str = "eval-calls.jsonl";
const CALLS_LOCK: &str = "eval-calls.lock";
/// The directory of the reservations' owner locks, under the run directory.
const OWNERS_DIR: &str = "eval-calls.owners";
/// Every entry of the capture, under the run directory.
pub const CAPTURE_ENTRIES: [&str; 3] = [CALLS_FILE, CALLS_LOCK, OWNERS_DIR];
/// The most bytes of one call's scrubbed args kept in a capture.
pub const CAPTURED_ARGS_MAX_BYTES: usize = 16 * 1024;
/// The most bytes of a capture file the reader reads; a larger file does
/// not read (an agent that is not isolated can write the run directory).
pub const CAPTURE_MAX_BYTES: u64 = 64 * 1024 * 1024;
/// The MAC domain of a call's identity (`apb_core::run_origin::tag`).
const CALL_KEY_DOMAIN: &str = "eval-call/1";

/// One line of [`CALLS_FILE`]: a reservation, written and synced before
/// the call is dispatched, or the delivery of an earlier reservation,
/// written and synced before the call returns its answer to its caller.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum CaptureLine {
    Reserved(CapturedCall),
    Delivered {
        reservation: u64,
    },
    /// A retry of the same call took over the reservation its dead caller
    /// left (written and synced before the retry is dispatched): one more
    /// dispatch of that reservation, journaled as its own `connector_call`.
    Replayed {
        reservation: u64,
    },
}

/// One reserved call.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CapturedCall {
    /// The line number of the reservation in [`CALLS_FILE`].
    pub seq: u64,
    /// The binding site: the node id, `<node>#check`, `@baseline`,
    /// `@follow_up`, or a site a later change adds (`<node>#inbox`).
    pub site: String,
    pub connector: String,
    pub function: String,
    pub account: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub item: Option<String>,
    /// The call's identity: a MAC under this installation's key of the
    /// canonical JSON of site, connector, function, account, item and the
    /// raw args, so two calls compare equal without the raw args being
    /// stored. Never empty: a call whose key cannot be computed is
    /// refused before a reservation is written.
    pub call_key: String,
    /// The call's input arguments (as the call was made, before the
    /// connector's text modes and send markers), through the run's
    /// storage redactor; `{"clipped": true}` when they are longer than
    /// [`CAPTURED_ARGS_MAX_BYTES`], `{"withheld": "secrets_unresolved"}`
    /// when the redactor refuses.
    pub args: Value,
    /// Whether `args` holds the whole scrubbed args. An assertion that
    /// reads args never decides over a capture that does not.
    #[serde(default)]
    pub args_kept: ArgsKept,
    /// The index of the answer that served it; `None`: unmatched.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub answer: Option<usize>,
    /// Set by [`read_calls`]: a `delivered` line names this reservation.
    #[serde(skip)]
    pub delivered: bool,
    /// Set by [`read_calls`]: how many `replayed` lines name it (each a
    /// further dispatch of the same call).
    #[serde(skip)]
    pub replays: u32,
}

/// `whole`, `clipped` or `withheld`; reads as `withheld` when absent, so
/// a capture written without it never counts as whole.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ArgsKept {
    Whole,
    Clipped,
    #[default]
    Withheld,
}

/// A capture file that does not read: its line and why.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CaptureError {
    pub line: usize,
    pub reason: String,
}

impl std::fmt::Display for CaptureError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "line {}: {}", self.line, self.reason)
    }
}

/// One call of a doubled run, as [`reserve`] reads it.
pub(crate) struct DoubledCall<'a> {
    pub site: &'a str,
    pub connector: &'a str,
    pub function: &'a str,
    pub account: &'a str,
    pub item: Option<&'a str>,
    pub args: &'a Value,
}

/// The reservation a call holds until its answer is delivered: the line's
/// seq, the answer, and the owner lock of the reservation, released on
/// drop.
pub(crate) struct Reservation {
    pub seq: u64,
    pub answer: Result<MockSpec, CallError>,
    /// Held for its `Drop`: the reservation's owner is alive while it is.
    _owner: HeldLock,
}

/// The doubles of the run in `run_dir` (its `run.yaml` eval settings),
/// `Ok(None)` for a run that has none. Fails closed: a `run.yaml` that
/// does not read, or a journal whose `run_provenance` says
/// `eval_doubles: true` while the settings name none, is an error
/// (`CallErrorCode::Config`), never "no doubles".
pub(crate) fn of_run(run_dir: &Path, events: &[Event]) -> Result<Option<Doubles>, CallError> {
    let cfg = crate::run_config::read_run_config(run_dir).map_err(|_| {
        CallError::new(
            CallErrorCode::Config,
            "the run's settings (run.yaml) do not read, so whether its connector calls are doubled cannot be told; nothing was sent",
        )
    })?;
    if let Some(d) = cfg.doubles() {
        return Ok(Some(d.clone()));
    }
    let provenance_says = events.iter().any(|e| {
        matches!(
            e.payload,
            EventPayload::RunProvenance {
                eval_doubles: true,
                ..
            }
        )
    });
    if provenance_says {
        return Err(CallError::new(
            CallErrorCode::Config,
            "the run's journal says it is a doubled eval run but its settings name no doubles; nothing was sent",
        ));
    }
    Ok(None)
}

fn config(message: impl Into<String>) -> CallError {
    CallError::new(CallErrorCode::Config, message)
}

/// The answer of reservation `answer` of `call`'s function: its recorded
/// answer, or `double_unmatched`.
fn answer_of(
    doubles: &Doubles,
    call: &DoubledCall<'_>,
    answer: Option<usize>,
) -> Result<MockSpec, CallError> {
    answer
        .and_then(|i| {
            doubles
                .get(call.connector)
                .and_then(|f| f.get(call.function))
                .and_then(|a| a.get(i))
        })
        .map(|a| a.answer.clone())
        .ok_or_else(|| {
            CallError::new(
                CallErrorCode::DoubleUnmatched,
                format!(
                    "no double of the eval case answers this call of `{}.{}` at `{}`; nothing was sent",
                    call.connector, call.function, call.site
                ),
            )
        })
}

/// The call's identity under this installation's key.
fn call_key(call: &DoubledCall<'_>) -> Result<String, CallError> {
    let canonical = json!({
        "site": call.site,
        "connector": call.connector,
        "function": call.function,
        "account": call.account,
        "item": call.item,
        "args": call.args,
    });
    apb_core::run_origin::tag(CALL_KEY_DOMAIN, canonical.to_string().as_bytes())
        .ok()
        .filter(|k| !k.is_empty())
        .ok_or_else(|| {
            config(
                "eval_call_key_unavailable: this installation has no usable key, so a doubled call cannot be identified; nothing was answered",
            )
        })
}

/// The item id as the capture keeps it: through the same storage redactor
/// as the args, withheld when the redactor refuses.
fn captured_item(redactor: &crate::run_redact::RunRedactor, item: Option<&str>) -> Option<String> {
    item.map(|i| {
        redactor
            .secret_text(i)
            .unwrap_or_else(|_| "[secrets_unresolved]".to_string())
    })
}

/// The args as the capture keeps them: scrubbed by the run's storage
/// redactor, clipped or withheld when they cannot be kept whole.
fn captured_args(redactor: &crate::run_redact::RunRedactor, args: &Value) -> (Value, ArgsKept) {
    let mut scrubbed = args.clone();
    if redactor.secret_json(&mut scrubbed).is_err() {
        return (
            json!({ "withheld": "secrets_unresolved" }),
            ArgsKept::Withheld,
        );
    }
    if scrubbed.to_string().len() > CAPTURED_ARGS_MAX_BYTES {
        return (json!({ "clipped": true }), ArgsKept::Clipped);
    }
    (scrubbed, ArgsKept::Whole)
}

/// Appends one line to the capture file and syncs it.
fn append_line(run_dir: &Path, line: &CaptureLine) -> std::io::Result<()> {
    let mut text = serde_json::to_string(line).map_err(std::io::Error::other)?;
    text.push('\n');
    let mut file = apb_core::fsutil::open_append_private_in(run_dir, CALLS_FILE)?;
    append_restoring(
        &mut file,
        text.as_bytes(),
        std::fs::File::sync_data,
        |f, len| f.set_len(len).and_then(|()| f.sync_data()),
    )
}

/// What [`append_restoring`] appends when it cannot cut a failed line
/// back: a line that never parses, so the capture no longer reads and
/// every later read and reservation of the run fails closed.
const POISON_LINE: &[u8] = b"\ncapture_unsure: an append failed and could not be undone\n";

/// Appends `bytes` to `file` and syncs it with `sync`. When the write or
/// the sync fails, the file is cut back to its length before the append
/// with `cut`, so a line its caller reports as not recorded is not read
/// back as one (a `delivered` line whose answer was not handed out, above
/// all). When even the cut fails, a line that never parses is appended, so
/// the capture fails closed instead of reading a line that may stand. The
/// caller holds [`CALLS_LOCK`], so nothing else appended meanwhile.
fn append_restoring(
    file: &mut std::fs::File,
    bytes: &[u8],
    sync: impl FnOnce(&std::fs::File) -> std::io::Result<()>,
    cut: impl FnOnce(&std::fs::File, u64) -> std::io::Result<()>,
) -> std::io::Result<()> {
    let before = file.metadata()?.len();
    let written = file.write_all(bytes).and_then(|()| sync(file));
    if written.is_err() && cut(file, before).is_err() {
        let _ = file.write_all(POISON_LINE).and_then(|()| file.sync_data());
    }
    written
}

/// The run directories whose entries this process synced after a
/// reservation: the capture file's entry is durable there.
static SYNCED_DIRS: std::sync::Mutex<std::collections::BTreeSet<std::path::PathBuf>> =
    std::sync::Mutex::new(std::collections::BTreeSet::new());

/// Syncs `run_dir` once per process before a reservation's call is
/// dispatched, a new reservation or a replayed one alike, so a power loss
/// never takes the capture file's entry with a dispatched call in it. A
/// failed sync is tried again by the next call; a call process makes one
/// call, so this is one directory sync per dispatch at most.
fn sync_capture_dir(run_dir: &Path) -> std::io::Result<()> {
    if SYNCED_DIRS.lock().is_ok_and(|s| s.contains(run_dir)) {
        return Ok(());
    }
    #[cfg(any(test, feature = "testing"))]
    crate::deadline_hooks::at(
        crate::deadline_hooks::Hook::DoubleBeforeCaptureDirSync,
        run_dir,
    );
    apb_core::fsutil::sync_dir(run_dir)?;
    if let Ok(mut s) = SYNCED_DIRS.lock() {
        s.insert(run_dir.to_path_buf());
    }
    Ok(())
}

/// The reason of a capture file that is not there. A doubled run's capture
/// is created at its start ([`create_capture`]), so a missing one was
/// removed: never read as no call.
pub const CAPTURE_MISSING: &str =
    "capture_missing: the call capture file is not in the run directory";

fn missing() -> CaptureError {
    CaptureError {
        line: 0,
        reason: CAPTURE_MISSING.to_string(),
    }
}

/// Creates the empty capture file of a doubled run and syncs it and the
/// run directory, before the run's first node.
pub fn create_capture(run_dir: &Path) -> std::io::Result<()> {
    let file = apb_core::fsutil::open_append_private_in(run_dir, CALLS_FILE)?;
    file.sync_all()?;
    apb_core::fsutil::sync_dir(run_dir)
}

/// The lines of the capture file, strictly: a line that does not parse is
/// an error naming it, and a missing file is [`CAPTURE_MISSING`]. Read beneath the run
/// directory: no link is followed, only a regular file with one link is
/// opened (never a FIFO), at most [`CAPTURE_MAX_BYTES`].
fn read_lines(run_dir: &Path) -> Result<Vec<CaptureLine>, CaptureError> {
    use apb_core::fsutil::beneath::{Beneath, CappedRead, ReadRefusal};
    let unreadable = |reason: String| CaptureError { line: 0, reason };
    let read = match Beneath::open(run_dir)
        .and_then(|b| b.read_regular_capped(CALLS_FILE, CAPTURE_MAX_BYTES))
    {
        Ok(read) => read,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Err(missing()),
        Err(e) => return Err(unreadable(format!("the capture cannot be read: {e}"))),
    };
    let raw = match read {
        CappedRead::Read { bytes, .. } => bytes,
        CappedRead::Refused(ReadRefusal::Missing) => return Err(missing()),
        CappedRead::Refused(ReadRefusal::TooLarge { size }) => {
            return Err(unreadable(format!(
                "the capture is {size} bytes, more than the {CAPTURE_MAX_BYTES} it may hold"
            )));
        }
        CappedRead::Refused(r) => {
            return Err(unreadable(format!(
                "the capture is not a plain file ({r:?}), so it is not read"
            )));
        }
    };
    if !raw.is_empty() && raw.last() != Some(&b'\n') {
        let line = raw.iter().filter(|b| **b == b'\n').count() + 1;
        return Err(CaptureError {
            line,
            reason: "the line is torn (no line end)".to_string(),
        });
    }
    let mut out = Vec::new();
    let whole = &raw[..raw.len().saturating_sub(1)];
    for (i, bytes) in whole.split(|b| *b == b'\n').enumerate() {
        if raw.is_empty() {
            break;
        }
        let line: CaptureLine = serde_json::from_slice(bytes).map_err(|e| CaptureError {
            line: i + 1,
            reason: format!("the line does not parse: {e}"),
        })?;
        if let CaptureLine::Reserved(c) = &line
            && c.seq != i as u64
        {
            return Err(CaptureError {
                line: i + 1,
                reason: format!("the reservation names seq {} at line {}", c.seq, i + 1),
            });
        }
        out.push(line);
    }
    Ok(out)
}

/// Every reservation of the run, in order, each with `delivered` set from
/// the `delivered` lines; [`CAPTURE_MISSING`] without the file. A line that does
/// not parse (torn or corrupt) is an error naming its line number, never
/// skipped.
pub fn read_calls(run_dir: &Path) -> Result<Vec<CapturedCall>, CaptureError> {
    Ok(calls_of(read_lines(run_dir)?))
}

/// The reservations of `lines`, each marked delivered when a line says so.
fn calls_of(lines: Vec<CaptureLine>) -> Vec<CapturedCall> {
    let mut delivered = std::collections::BTreeSet::new();
    let mut replays: std::collections::BTreeMap<u64, u32> = std::collections::BTreeMap::new();
    for l in &lines {
        match l {
            CaptureLine::Delivered { reservation } => {
                delivered.insert(*reservation);
            }
            CaptureLine::Replayed { reservation } => *replays.entry(*reservation).or_insert(0) += 1,
            CaptureLine::Reserved(_) => {}
        }
    }
    lines
        .into_iter()
        .filter_map(|l| match l {
            CaptureLine::Reserved(mut c) => {
                c.delivered = delivered.contains(&c.seq);
                c.replays = replays.get(&c.seq).copied().unwrap_or(0);
                Some(c)
            }
            CaptureLine::Delivered { .. } | CaptureLine::Replayed { .. } => None,
        })
        .collect()
}

/// Reserves the answer of one call of a doubled run and returns it with
/// the reservation it holds; the answer is `double_unmatched` when no
/// double answers. An error (nothing answered, nothing written) when the
/// capture does not read, the call has no identity, or the reservation
/// line cannot be written and synced.
pub(crate) fn reserve(
    run_dir: &Path,
    root: &Path,
    doubles: &Doubles,
    call: &DoubledCall<'_>,
) -> Result<Reservation, CallError> {
    // Scrubbed outside the capture lock: the redactor reads the tree.
    let redactor = crate::run_redact::RunRedactor::for_storing(root, run_dir);
    let (args, args_kept) = captured_args(&redactor, call.args);
    let item = captured_item(&redactor, call.item);
    let _lock = apb_core::fsutil::lock_dir(run_dir, CALLS_LOCK)
        .map_err(|e| config(format!("the call capture cannot be locked: {e}")))?;
    let lines = read_lines(run_dir).map_err(|e| {
        config(format!(
            "the call capture does not read ({e}); nothing was answered"
        ))
    })?;
    // The seq of a new reservation: the line count before its append.
    let seq = lines.len() as u64;
    let calls = calls_of(lines);
    let key = call_key(call)?;
    let owners = run_dir.join(OWNERS_DIR);
    // A reservation of the same call whose owner is gone: its caller never
    // received the answer, this retry gets it.
    for c in calls.iter().filter(|c| !c.delivered && c.call_key == key) {
        if let Ok(owner) = apb_core::fsutil::try_hold_lock(&owners, &format!("{}.lock", c.seq)) {
            append_line(run_dir, &CaptureLine::Replayed { reservation: c.seq })
                .and_then(|()| sync_capture_dir(run_dir))
                .map_err(|e| {
                config(format!(
                    "the call capture could not record this reservation: {e}; nothing was answered"
                ))
            })?;
            return Ok(Reservation {
                seq: c.seq,
                answer: answer_of(doubles, call, c.answer),
                _owner: owner,
            });
        }
    }
    let answers: &[apb_core::eval_doubles::DoubleAnswer] = doubles
        .get(call.connector)
        .and_then(|f| f.get(call.function))
        .map(Vec::as_slice)
        .unwrap_or_default();
    let used: Vec<u32> = (0..answers.len())
        .map(|i| {
            calls
                .iter()
                .filter(|c| {
                    c.connector == call.connector
                        && c.function == call.function
                        && c.answer == Some(i)
                })
                .count() as u32
        })
        .collect();
    let answer = apb_core::eval_doubles::pick(answers, call.site, call.args, &used);
    let owner = apb_core::fsutil::try_hold_lock(&owners, &format!("{seq}.lock")).map_err(|e| {
        config(format!(
            "the call capture could not record this reservation: {e}"
        ))
    })?;
    let line = CaptureLine::Reserved(CapturedCall {
        seq,
        site: call.site.to_string(),
        connector: call.connector.to_string(),
        function: call.function.to_string(),
        account: call.account.to_string(),
        item,
        call_key: key,
        args,
        args_kept,
        answer,
        delivered: false,
        replays: 0,
    });
    append_line(run_dir, &line)
        .and_then(|()| sync_capture_dir(run_dir))
        .map_err(|e| {
            config(format!(
                "the call capture could not record this reservation: {e}; nothing was answered"
            ))
        })?;
    Ok(Reservation {
        seq,
        answer: answer_of(doubles, call, answer),
        _owner: owner,
    })
}

/// Appends the `delivered` line of `r` under [`CALLS_LOCK`], syncs the
/// file and then releases its owner lock. An error when the line cannot
/// be written or synced: the caller then returns that error instead of
/// the answer, and the reservation stays undelivered.
pub(crate) fn deliver(run_dir: &Path, r: Reservation) -> Result<(), CallError> {
    let unrecorded = || {
        config(
            "eval_call_delivery_unrecorded: the call capture could not record this answer's delivery; the answer was not handed out",
        )
    };
    let lock = apb_core::fsutil::lock_dir(run_dir, CALLS_LOCK).map_err(|_| unrecorded())?;
    append_line(run_dir, &CaptureLine::Delivered { reservation: r.seq })
        .map_err(|_| unrecorded())?;
    drop(lock);
    drop(r);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// When the failed line cannot be cut off, the capture stops reading:
    /// the line that may stand is never read as a delivery.
    #[test]
    fn a_failed_cut_makes_the_capture_unreadable() {
        let t = tempfile::tempdir().unwrap();
        let reserved = serde_json::to_string(&reserved(0, "k", Some(0))).unwrap() + "\n";
        std::fs::write(t.path().join(CALLS_FILE), &reserved).unwrap();
        assert_eq!(read_calls(t.path()).unwrap().len(), 1);
        let delivered =
            serde_json::to_string(&CaptureLine::Delivered { reservation: 0 }).unwrap() + "\n";
        let mut f = std::fs::OpenOptions::new()
            .append(true)
            .open(t.path().join(CALLS_FILE))
            .unwrap();
        let err = append_restoring(
            &mut f,
            delivered.as_bytes(),
            |_| Err(std::io::Error::other("sync failed")),
            |_, _| Err(std::io::Error::other("cut failed")),
        );
        assert!(err.is_err());
        assert!(read_calls(t.path()).is_err(), "the capture no longer reads");
    }

    /// A line whose sync failed is cut off again: a failed `delivered`
    /// append never reads as a delivery.
    #[test]
    fn a_failed_sync_leaves_no_line() {
        let t = tempfile::tempdir().unwrap();
        let path = t.path().join("calls.jsonl");
        std::fs::write(&path, b"first\n").unwrap();
        let mut f = std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap();
        let cut = |f: &std::fs::File, len: u64| f.set_len(len);
        let err = append_restoring(
            &mut f,
            b"second\n",
            |_| Err(std::io::Error::other("sync failed")),
            cut,
        );
        assert!(err.is_err());
        assert_eq!(std::fs::read(&path).unwrap(), b"first\n");
        append_restoring(&mut f, b"third\n", std::fs::File::sync_data, cut).unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"first\nthird\n");
    }

    fn reserved(seq: u64, key: &str, answer: Option<usize>) -> CaptureLine {
        CaptureLine::Reserved(CapturedCall {
            seq,
            site: "a".into(),
            connector: "c".into(),
            function: "f".into(),
            account: "main".into(),
            item: None,
            call_key: key.into(),
            args: json!({}),
            args_kept: ArgsKept::Whole,
            answer,
            delivered: false,
            replays: 0,
        })
    }

    fn write(dir: &Path, lines: &[CaptureLine], tail: &str) {
        let mut text: String = lines
            .iter()
            .map(|l| format!("{}\n", serde_json::to_string(l).unwrap()))
            .collect();
        text.push_str(tail);
        std::fs::write(dir.join(CALLS_FILE), text).unwrap();
    }

    #[test]
    fn read_calls_reports_a_torn_line() {
        let dir = tempfile::tempdir().unwrap();
        create_capture(dir.path()).unwrap();
        assert_eq!(read_calls(dir.path()).unwrap(), Vec::new());
        write(
            dir.path(),
            &[
                reserved(0, "k", Some(0)),
                CaptureLine::Delivered { reservation: 0 },
                reserved(2, "k", None),
            ],
            "",
        );
        let calls = read_calls(dir.path()).unwrap();
        assert_eq!(calls.len(), 2);
        assert!(calls[0].delivered);
        assert!(!calls[1].delivered);
        // A torn last line is an error naming it, never skipped.
        write(
            dir.path(),
            &[reserved(0, "k", Some(0))],
            "{\"kind\":\"delivered\",\"reserva",
        );
        let e = read_calls(dir.path()).unwrap_err();
        assert_eq!(e.line, 2, "{e}");
        // So is a corrupt line in the middle.
        write(
            dir.path(),
            &[reserved(0, "k", Some(0))],
            "not json\n{\"kind\":\"delivered\",\"reservation\":0}\n",
        );
        assert_eq!(read_calls(dir.path()).unwrap_err().line, 2);
        // An older capture without `args_kept` never reads as whole.
        std::fs::write(
            dir.path().join(CALLS_FILE),
            "{\"kind\":\"reserved\",\"seq\":0,\"site\":\"a\",\"connector\":\"c\",\"function\":\"f\",\"account\":\"m\",\"call_key\":\"k\",\"args\":{}}\n",
        )
        .unwrap();
        assert_eq!(
            read_calls(dir.path()).unwrap()[0].args_kept,
            ArgsKept::Withheld
        );
    }

    #[test]
    fn the_reader_opens_only_a_plain_capped_file() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("elsewhere");
        write(dir.path(), &[reserved(0, "k", Some(0))], "");
        std::fs::rename(dir.path().join(CALLS_FILE), &target).unwrap();
        // A link is never followed.
        std::os::unix::fs::symlink(&target, dir.path().join(CALLS_FILE)).unwrap();
        let e = read_calls(dir.path()).unwrap_err();
        assert!(e.reason.contains("not a plain file"), "{e}");
        std::fs::remove_file(dir.path().join(CALLS_FILE)).unwrap();
        // A FIFO is refused without blocking on it.
        let made = std::process::Command::new("mkfifo")
            .arg(dir.path().join(CALLS_FILE))
            .status()
            .unwrap();
        assert!(made.success());
        let e = read_calls(dir.path()).unwrap_err();
        assert!(e.reason.contains("not a plain file"), "{e}");
        std::fs::remove_file(dir.path().join(CALLS_FILE)).unwrap();
        // A file larger than the cap does not read.
        let big = std::fs::File::create(dir.path().join(CALLS_FILE)).unwrap();
        big.set_len(CAPTURE_MAX_BYTES + 1).unwrap();
        let e = read_calls(dir.path()).unwrap_err();
        assert!(e.reason.contains("more than"), "{e}");
        // No file, or no run directory, is a missing capture, never no calls.
        std::fs::remove_file(dir.path().join(CALLS_FILE)).unwrap();
        assert_eq!(read_calls(dir.path()).unwrap_err().reason, CAPTURE_MISSING);
        assert_eq!(
            read_calls(&dir.path().join("gone")).unwrap_err().reason,
            CAPTURE_MISSING
        );
        create_capture(dir.path()).unwrap();
        assert_eq!(read_calls(dir.path()).unwrap(), Vec::new());
    }

    #[test]
    fn a_replay_needs_a_free_owner_lock() {
        apb_core::process_mode::testing::in_plain_child(
            "eval::doubles::tests::a_replay_needs_a_free_owner_lock",
            || {
                let cfg = tempfile::tempdir().unwrap();
                // SAFETY: a child process runs this one test on one thread.
                unsafe { std::env::set_var("APB_CONFIG_DIR", cfg.path()) };
                let root = tempfile::tempdir().unwrap();
                let run = root.path().join(".apb/runs/r1");
                std::fs::create_dir_all(&run).unwrap();
                create_capture(&run).unwrap();
                let doubles: Doubles = serde_json::from_value(json!({
                    "c": { "f": [
                        { "answer": { "status": 200, "body": { "n": 1 } }, "times": 1 },
                        { "answer": { "status": 200, "body": { "n": 2 } } }
                    ] }
                }))
                .unwrap();
                let args = json!({ "q": "x" });
                let call = DoubledCall {
                    site: "a",
                    connector: "c",
                    function: "f",
                    account: "main",
                    item: None,
                    args: &args,
                };
                let first = reserve(&run, root.path(), &doubles, &call).unwrap();
                assert_eq!(first.seq, 0);
                // Its owner lives: the same call reserves its own.
                let second = reserve(&run, root.path(), &doubles, &call).unwrap();
                assert_eq!(second.seq, 1);
                assert_eq!(second.answer.as_ref().unwrap().body, json!({ "n": 2 }));
                // Its owner is gone: the same call gets it back, no line.
                drop(first);
                let again = reserve(&run, root.path(), &doubles, &call).unwrap();
                assert_eq!(again.seq, 0);
                assert_eq!(again.answer.as_ref().unwrap().body, json!({ "n": 1 }));
                assert_eq!(read_calls(&run).unwrap().len(), 2);
                deliver(&run, again).unwrap();
                deliver(&run, second).unwrap();
                let calls = read_calls(&run).unwrap();
                assert!(calls.iter().all(|c| c.delivered), "{calls:?}");
                // A delivered reservation is never replayed.
                let third = reserve(&run, root.path(), &doubles, &call).unwrap();
                // Two reservations, one replay and two deliveries before it.
                assert_eq!(third.seq, 5);
                assert_eq!(read_calls(&run).unwrap()[0].replays, 1);
            },
        );
    }
}
