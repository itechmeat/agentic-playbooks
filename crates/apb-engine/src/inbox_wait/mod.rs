//! The inbox wait (inbox-waits D4): a `wait_for: { type: inbox }` node that
//! reads a connector account's stored events from the position its run's
//! journal holds and ends at its first matching event. The position lives
//! only in the run journal ([`position`]); a visit authorizes against the
//! manifest ([`authorize`]) before it reads; the end is one write of an
//! `inbox_wait_read` line and the existing `wait_signalled` or
//! `wait_timeout`; a long idle wait parks through the wait park, and the
//! park's waker asks [`pending::delivery_pending`]. Nothing outside the run
//! directory is written and nothing is acknowledged.

use crate::error::EngineError;
use crate::event::{Event, EventLog, EventPayload};
use crate::state::RunState;
use apb_core::connector::inbox::{Inbox, InboxEvent, InboxGap};
use std::path::Path;
#[cfg(any(test, feature = "testing"))]
use std::path::PathBuf;
use std::sync::atomic::AtomicBool;

pub(crate) mod pending;

/// What the last read of an account's log left behind: not read (before
/// the first read or after a failed one), read and absent (the log does
/// not exist) or read with the stamp the log carried.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum ReadState {
    /// Not read yet, or the last read failed: the next read is due.
    #[default]
    NotRead,
    /// The log was read and does not exist.
    ReadAbsent,
    /// The log was read and carries this stamp.
    Read {
        len: u64,
        modified: std::time::SystemTime,
    },
}

impl ReadState {
    /// The state a successful read leaves: the log's stamp, or absent when
    /// the log does not exist.
    pub(crate) fn of(stamp: Option<(u64, std::time::SystemTime)>) -> Self {
        match stamp {
            Some((len, modified)) => ReadState::Read { len, modified },
            None => ReadState::ReadAbsent,
        }
    }

    /// Whether a read is due: the state holds no read, or the log's stamp
    /// now differs from the one held (absent against present included). A
    /// successful read of an absent log is cached like any other.
    pub(crate) fn read_due(&self, current: Option<(u64, std::time::SystemTime)>) -> bool {
        match self {
            ReadState::NotRead => true,
            ReadState::ReadAbsent => current.is_some(),
            ReadState::Read { len, modified } => current != Some((*len, *modified)),
        }
    }
}

/// How many reads of an account's log a run did, and how many of them
/// failed (test builds only).
#[cfg(any(test, feature = "testing"))]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ReadCounts {
    /// The reads that were attempted.
    pub reads: usize,
    /// The reads that failed.
    pub failures: usize,
}

/// The reads of the account's logs, by run directory and account (test
/// builds only).
#[cfg(any(test, feature = "testing"))]
static READ_COUNTS: std::sync::Mutex<std::collections::BTreeMap<(PathBuf, String), ReadCounts>> =
    std::sync::Mutex::new(std::collections::BTreeMap::new());

/// The final reads of the account's logs, by run directory and account:
/// the read before a timeout end, which ignores the read interval (test
/// builds only).
#[cfg(any(test, feature = "testing"))]
static FINAL_READ_COUNTS: std::sync::Mutex<
    std::collections::BTreeMap<(PathBuf, String), ReadCounts>,
> = std::sync::Mutex::new(std::collections::BTreeMap::new());

/// The head reads of the visits' starts, by run directory (test builds
/// only): `start` reads the log's head before any visit is declared, apart
/// from the visit's own reads.
#[cfg(any(test, feature = "testing"))]
static HEAD_READ_COUNTS: std::sync::Mutex<std::collections::BTreeMap<PathBuf, ReadCounts>> =
    std::sync::Mutex::new(std::collections::BTreeMap::new());

/// The head reads the run at `run_dir` did before its visits started, and
/// how many failed (test builds only).
#[cfg(any(test, feature = "testing"))]
pub fn head_read_counts(run_dir: &Path) -> ReadCounts {
    HEAD_READ_COUNTS
        .lock()
        .ok()
        .and_then(|counts| counts.get(run_dir).copied())
        .unwrap_or_default()
}

/// Counts one read of an account's log, and whether it failed (test
/// builds only). The final read is counted apart: it is a read of its own,
/// and the tests count it.
#[cfg(any(test, feature = "testing"))]
pub(crate) fn count_read(run_dir: &Path, account: &str, final_read: bool, failed: bool) {
    let counts = if final_read {
        &FINAL_READ_COUNTS
    } else {
        &READ_COUNTS
    };
    if let Ok(mut counts) = counts.lock() {
        let entry = counts
            .entry((run_dir.to_path_buf(), account.to_string()))
            .or_default();
        entry.reads += 1;
        if failed {
            entry.failures += 1;
        }
    }
}

/// The reads the run at `run_dir` did, over every account (test builds
/// only).
#[cfg(any(test, feature = "testing"))]
pub fn read_counts(run_dir: &Path) -> ReadCounts {
    let mut total = ReadCounts::default();
    if let Ok(counts) = READ_COUNTS.lock() {
        for ((dir, _), counts) in counts.iter() {
            if dir == run_dir {
                total.reads += counts.reads;
                total.failures += counts.failures;
            }
        }
    }
    total
}

/// The final reads the run at `run_dir` did, over every account: the read
/// before a timeout end and its retry (test builds only).
#[cfg(any(test, feature = "testing"))]
pub fn final_read_counts(run_dir: &Path) -> ReadCounts {
    let mut total = ReadCounts::default();
    if let Ok(counts) = FINAL_READ_COUNTS.lock() {
        for ((dir, _), counts) in counts.iter() {
            if dir == run_dir {
                total.reads += counts.reads;
                total.failures += counts.failures;
            }
        }
    }
    total
}

/// The reads the run at `run_dir` did of one account's log (test builds
/// only).
#[cfg(any(test, feature = "testing"))]
pub fn account_read_counts(run_dir: &Path, account: &str) -> ReadCounts {
    READ_COUNTS
        .lock()
        .ok()
        .and_then(|counts| {
            counts
                .get(&(run_dir.to_path_buf(), account.to_string()))
                .copied()
        })
        .unwrap_or_default()
}

/// Forgets the reads of the run at `run_dir` (test builds only).
#[cfg(any(test, feature = "testing"))]
pub fn reset_read_counts(run_dir: &Path) {
    if let Ok(mut counts) = READ_COUNTS.lock() {
        counts.retain(|(dir, _), _| dir != run_dir);
    }
    if let Ok(mut counts) = FINAL_READ_COUNTS.lock() {
        counts.retain(|(dir, _), _| dir != run_dir);
    }
    if let Ok(mut counts) = HEAD_READ_COUNTS.lock() {
        counts.remove(run_dir);
    }
}

/// The armed read failures of the runs, by run directory (test builds
/// only).
#[cfg(any(test, feature = "testing"))]
#[derive(Default, Clone, Copy)]
struct ArmedReads {
    /// The failures left for the ordinary reads, the reads inside the read
    /// interval.
    ordinary: usize,
    /// The failures left for the final reads, the read before a timeout end.
    final_reads: usize,
    /// Whether the ordinary reads fail until the final read is admitted.
    hold_ordinary: bool,
}

/// The armed read failures of the runs, by run directory (test builds
/// only).
#[cfg(any(test, feature = "testing"))]
static READ_FAILURES: std::sync::Mutex<std::collections::BTreeMap<PathBuf, ArmedReads>> =
    std::sync::Mutex::new(std::collections::BTreeMap::new());

/// Makes the next `n` ordinary reads of the run at `run_dir` fail, as an
/// unreadable log would (test builds only). A count of zero disarms them.
#[cfg(any(test, feature = "testing"))]
pub fn fail_next_reads(run_dir: &Path, n: usize) {
    if let Ok(mut failures) = READ_FAILURES.lock() {
        failures.entry(run_dir.to_path_buf()).or_default().ordinary = n;
    }
}

/// Makes the next `n` final reads of the run at `run_dir` fail, the read
/// before a timeout end, as an unreadable log would (test builds only). A
/// count of zero disarms them.
#[cfg(any(test, feature = "testing"))]
pub fn fail_next_final_reads(run_dir: &Path, n: usize) {
    if let Ok(mut failures) = READ_FAILURES.lock() {
        failures
            .entry(run_dir.to_path_buf())
            .or_default()
            .final_reads = n;
    }
}

/// Holds the ordinary reads of the run at `run_dir` until its final read is
/// admitted: every ordinary read fails until the final read is tried, so
/// the final read is the only read that can find the event (test builds
/// only).
#[cfg(any(test, feature = "testing"))]
pub fn hold_ordinary_reads_until_final(run_dir: &Path) {
    if let Ok(mut failures) = READ_FAILURES.lock() {
        failures
            .entry(run_dir.to_path_buf())
            .or_default()
            .hold_ordinary = true;
    }
}

/// Whether the next read of the run at `run_dir` must fail: an ordinary
/// read, or the final read before a timeout end (test builds only).
#[cfg(any(test, feature = "testing"))]
fn read_fails(run_dir: &Path, final_read: bool) -> bool {
    let Ok(mut failures) = READ_FAILURES.lock() else {
        return false;
    };
    let Some(armed) = failures.get_mut(run_dir) else {
        return false;
    };
    if final_read {
        // The final read is admitted: the ordinary reads are free again.
        armed.hold_ordinary = false;
        if armed.final_reads > 0 {
            armed.final_reads -= 1;
            return true;
        }
        return false;
    }
    if armed.hold_ordinary {
        return true;
    }
    if armed.ordinary > 0 {
        armed.ordinary -= 1;
        return true;
    }
    false
}

/// The gap appends that answer `false`, by run directory (test builds
/// only).
#[cfg(any(test, feature = "testing"))]
static GAP_APPEND_FAILURES: std::sync::Mutex<std::collections::BTreeMap<PathBuf, usize>> =
    std::sync::Mutex::new(std::collections::BTreeMap::new());

/// Makes the next `n` gap appends of the run at `run_dir` answer `false`,
/// as a journal that cannot be written would, and disarms them when the
/// returned guard is dropped (test builds only).
#[cfg(any(test, feature = "testing"))]
pub fn fail_next_gap_appends(run_dir: &Path, n: usize) -> GapFailureGuard {
    if let Ok(mut failures) = GAP_APPEND_FAILURES.lock() {
        if n == 0 {
            failures.remove(run_dir);
        } else {
            failures.insert(run_dir.to_path_buf(), n);
        }
    }
    GapFailureGuard {
        run_dir: run_dir.to_path_buf(),
    }
}

/// The guard that disarms the armed gap appends of its run when it is
/// dropped, so a failed assertion leaves nothing armed for the next test
/// (test builds only).
#[cfg(any(test, feature = "testing"))]
pub struct GapFailureGuard {
    run_dir: PathBuf,
}

#[cfg(any(test, feature = "testing"))]
impl Drop for GapFailureGuard {
    fn drop(&mut self) {
        if let Ok(mut failures) = GAP_APPEND_FAILURES.lock() {
            failures.remove(&self.run_dir);
        }
    }
}

/// Whether the next gap append of the run at `run_dir` must answer `false`
/// (test builds only).
#[cfg(any(test, feature = "testing"))]
fn gap_append_fails(run_dir: &Path) -> bool {
    let Ok(mut failures) = GAP_APPEND_FAILURES.lock() else {
        return false;
    };
    match failures.get_mut(run_dir) {
        Some(n) if *n > 0 => {
            *n -= 1;
            true
        }
        _ => false,
    }
}

/// One visit's in-memory state, owned by the drive loop in a
/// `BTreeMap<String, Scan>` keyed by node beside `park_retry`, created
/// at the first poll of each drive and dropped when its node finishes.
#[derive(Default)]
pub(crate) struct Scan {
    pub(crate) rendered: Vec<String>,
    /// The last seq examined; starts at the position.
    pub(crate) after: u64,
    pub(crate) examined: u32,
    /// What the last read of the account's log left behind: not read
    /// before the first one and after a failed one.
    pub(crate) read_state: ReadState,
    /// When the last read was, in milliseconds.
    pub(crate) last_read_ms: u64,
    /// Whether the last read succeeded, its gap (if any) is journaled,
    /// and it was examined to its end.
    pub(crate) caught_up: bool,
    pub(crate) redactor: Option<crate::run_redact::RunRedactor>,
    /// Whether this drive's first poll ran (its authorize and render).
    pub(crate) first_poll_done: bool,
    /// Whether the visit already logged one failed read on stderr.
    pub(crate) read_error_logged: bool,
}

/// The outcome of one poll of an open visit.
pub(crate) enum Poll {
    /// Not over; the drive sleeps a tick, or parks when it may.
    Open,
    /// Over; the end pair is journaled; the node succeeds with `output`.
    Ended { output: String },
    /// Over with no match at the timeout; the end pair is journaled; the
    /// node times out.
    TimedOut,
    /// Refused (authorization, unreadable head, unscrubbable output);
    /// nothing was journaled for the wait; the node fails with `reason`.
    Failed { reason: String },
}

/// What the drive holds while a visit is open: the pieces [`poll`] and
/// [`start`] need besides the wait itself and the visit's [`Scan`]. The
/// playbook and the run's id are what the render of the conditions (the
/// one render every template of a run goes through) reads; the run config
/// and the drive's cancel flag are what that render and the bounded
/// head-read retries need.
pub(crate) struct WaitCx<'a> {
    pub(crate) root: &'a Path,
    pub(crate) run_dir: &'a Path,
    pub(crate) node: &'a str,
    pub(crate) log: &'a mut EventLog,
    pub(crate) state: &'a RunState,
    pub(crate) cfg: &'a crate::run_config::RunConfig,
    pub(crate) cancel: &'a AtomicBool,
    /// The definition the drive executes.
    pub(crate) playbook: &'a apb_core::schema::Playbook,
    /// The run's id, as a template reads it.
    pub(crate) run_id: &'a str,
}

/// Whether this process may read `node`'s inbox account now (D4
/// "Authorization"): the manifest grant names exactly the wait's account,
/// the live connector and account digests equal the manifest's, and both
/// are approved, in the order `LiveTarget::require_trust` uses. A refusal
/// names which check failed and the connector and account, never a field
/// value. Guards reads of the inbox, and only reads: a journaled end
/// needs no authorization, because nothing more is read.
pub(crate) fn authorize(
    root: &Path,
    run_dir: &Path,
    node: &str,
    w: &apb_core::inbox_wait::InboxWait,
) -> Result<(), String> {
    let site = apb_core::inbox_wait::inbox_site(node);
    let names = format!("connector `{}` account `{}`", w.connector, w.account);
    // 1. The manifest grant and the snapshotted account.
    let m = crate::manifest::read(run_dir)
        .map_err(|e| format!("{names}: the run manifest does not read ({e})"))?
        .ok_or_else(|| format!("{names}: the run has no manifest"))?;
    let grant_ok = m
        .grants_for(&site)
        .iter()
        .any(|g| g.connector == w.connector && g.accounts.as_slice() == [w.account.as_str()]);
    let manifest_account = m
        .connector(&w.connector)
        .and_then(|c| c.accounts.iter().find(|a| a.name == w.account));
    let (Some(manifest_account), true) = (manifest_account, grant_ok) else {
        return Err(format!(
            "{names}: the manifest's grant for site `{site}` does not name exactly this account"
        ));
    };
    let manifest_connector = m.connector(&w.connector).unwrap_or_else(|| {
        unreachable!("checked above: the manifest connector holds the account");
    });
    // 2. The live digests equal the manifest's.
    let loaded = apb_core::connector::store::load(&w.connector)
        .map_err(|e| format!("{names}: the installed connector does not load ({e})"))?;
    if loaded.digest != manifest_connector.digest {
        return Err(format!(
            "{names}: the installed connector changed since the run started"
        ));
    }
    let accounts = apb_core::connector::config::load_merged(root, &w.connector)
        .map_err(|e| format!("{names}: the account config does not read ({e})"))?;
    let live = accounts
        .iter()
        .find(|a| a.name == w.account)
        .ok_or_else(|| format!("{names}: the account is not configured any more"))?;
    let live_digest = apb_core::connector::config::account_digest(live);
    if live_digest != manifest_account.digest {
        return Err(format!(
            "{names}: the account changed since the run started"
        ));
    }
    // 3. Both digests approved.
    let trust = apb_core::trust::TrustStore::load();
    if !trust.is_approved(&loaded.digest) {
        return Err(format!("{names}: the connector is not approved"));
    }
    if !trust.is_approved(&live_digest) {
        return Err(format!("{names}: the account is not approved"));
    }
    Ok(())
}

/// The last stored seq of the wait's account, or 0. One read.
fn head_of(w: &apb_core::inbox_wait::InboxWait) -> Result<u64, String> {
    let inbox = apb_core::connector::inbox::Inbox::open(&w.connector, &w.account)
        .map_err(|e| e.to_string())?;
    let events = inbox.read_events().map_err(|e| e.to_string())?;
    Ok(events.last().map(|e| e.seq).unwrap_or(0))
}

/// Step 2 for a node with no position: authorize, read the head, append
/// the start pair. The wait arm calls it instead of appending
/// `wait_started` alone (D4 "The visit" step 2). The head read is retried
/// within the engine's bounded infrastructure retries; only when the last
/// read of the schedule fails does it return `Err("inbox_wait_unreadable:
/// ..")`, with nothing journaled for the wait. No visit is declared before
/// the head is read, so no timeout bounds these reads; the schedule does.
pub(crate) fn start(
    cx: &mut WaitCx<'_>,
    w: &apb_core::inbox_wait::InboxWait,
) -> Result<Result<(), String>, EngineError> {
    if let Err(reason) = authorize(cx.root, cx.run_dir, cx.node, w) {
        return Ok(Err(format!("inbox_wait_unauthorized: {reason}")));
    }
    let names = format!("connector `{}` account `{}`", w.connector, w.account);
    let mut head = None;
    let mut last_err = String::new();
    // The first read at once, then one per backoff wait, as the attempt
    // loop walks its infrastructure budget (scheduler/node.rs).
    let schedule = crate::failure_class::backoff_schedule();
    for wait in std::iter::once(std::time::Duration::ZERO).chain(schedule) {
        if !wait.is_zero() && !crate::failure_class::wait_backoff(wait, cx.cancel) {
            // An abort was posted during the wait; the node fails and the
            // run's own stop path takes over.
            break;
        }
        let read = head_of(w);
        #[cfg(any(test, feature = "testing"))]
        if let Ok(mut counts) = HEAD_READ_COUNTS.lock() {
            let entry = counts.entry(cx.run_dir.to_path_buf()).or_default();
            entry.reads += 1;
            if read.is_err() {
                entry.failures += 1;
            }
        }
        match read {
            Ok(h) => {
                head = Some(h);
                break;
            }
            Err(e) => last_err = e,
        }
    }
    let Some(head) = head else {
        return Ok(Err(format!(
            "inbox_wait_unreadable: {names}: the inbox log does not read ({last_err})"
        )));
    };
    // The start line and `wait_started` in ONE write: a write cut after
    // the start line leaves no declared visit (crash table row 2).
    let node = cx.node.to_string();
    cx.log.append_pair(
        EventPayload::InboxWaitRead {
            node,
            through_seq: head,
            examined: 0,
            end: None,
            output: None,
        },
        |_, _| {},
        |_seq| EventPayload::WaitStarted {
            node: cx.node.to_string(),
            kind: "inbox".into(),
        },
    )?;
    Ok(Ok(()))
}

/// The wait's account inbox. `Inbox::open` fails only on a bad name or a
/// missing config dir; a caller that authorized already passed those.
fn inbox_of(w: &apb_core::inbox_wait::InboxWait) -> Result<Inbox, String> {
    apb_core::connector::inbox::Inbox::open(&w.connector, &w.account).map_err(|e| e.to_string())
}

/// The log's stamp and every stored event, one read.
type LogRead = (
    Option<(u64, std::time::SystemTime)>,
    Vec<apb_core::connector::inbox::InboxEvent>,
);

fn read_all_events(
    w: &apb_core::inbox_wait::InboxWait,
    final_read: bool,
) -> Result<LogRead, String> {
    let inbox = inbox_of(w)?;
    let stamp = inbox.log_stamp();
    // The final read before a timeout end waits for an append in flight:
    // a delivery stamped before the deadline whose line is still being
    // written is then examined, not left for the next visit. It only looks
    // at the store's lock, so the wait still writes nothing to the store.
    let events = if final_read {
        inbox.read_events_settled(FINAL_READ_SETTLE)
    } else {
        inbox.read_events()
    }
    .map_err(|e| e.to_string())?;
    Ok((stamp, events))
}

/// How long the final read waits for an append in flight to finish.
const FINAL_READ_SETTLE: std::time::Duration = std::time::Duration::from_secs(5);

/// One read of the account's log, counted (test builds only). The final
/// read before a timeout end is counted apart: it ignores the read
/// interval, and the tests count it.
#[cfg_attr(not(any(test, feature = "testing")), allow(unused_variables))]
fn read_log(
    cx: &WaitCx<'_>,
    w: &apb_core::inbox_wait::InboxWait,
    final_read: bool,
) -> Result<LogRead, String> {
    #[cfg(any(test, feature = "testing"))]
    if read_fails(cx.run_dir, final_read) {
        count_read(cx.run_dir, &w.account, final_read, true);
        return Err(format!(
            "the account log of `{}` does not read (injected failure)",
            w.account
        ));
    }
    let read = read_all_events(w, final_read);
    #[cfg(any(test, feature = "testing"))]
    count_read(cx.run_dir, &w.account, final_read, read.is_err());
    read
}

/// Every `equals` template of the wait, rendered over the run state as a
/// prompt renders: the one render every template of a run goes through
/// ([`crate::judge_eval::inputs::JudgeInputs`]).
fn render_conditions(
    cx: &WaitCx<'_>,
    w: &apb_core::inbox_wait::InboxWait,
) -> Result<Vec<String>, EngineError> {
    let inputs = crate::judge_eval::inputs::JudgeInputs::from_state(
        cx.playbook,
        cx.run_dir,
        cx.run_id,
        cx.state,
        cx.cfg,
        &cx.playbook.context_budget(cx.node),
    )
    .map_err(|e| EngineError::Invalid(format!("inbox wait conditions: {e}")))?;
    Ok(w.conditions
        .iter()
        .map(|c| inputs.render(&c.equals, cx.run_dir, 0))
        .collect())
}

/// The gap line of the wait's node, and whether it is in the journal.
fn journal_gap(cx: &mut WaitCx<'_>, w: &apb_core::inbox_wait::InboxWait, gap: &InboxGap) -> bool {
    #[cfg(any(test, feature = "testing"))]
    if gap_append_fails(cx.run_dir) {
        return false;
    }
    let node = cx.node;
    crate::connector::inbox::journal_gap_in(
        cx.log,
        &w.connector,
        &w.account,
        node,
        gap,
        &crate::connector::inbox::GapScope::Node,
    )
}

/// What one read of a visit found.
enum Examined {
    /// A matching event: its seq and the stored output.
    Matched(u64, String),
    /// Nothing matched.
    Nothing,
    /// The node fails with this reason; nothing was journaled for it.
    Failed(String),
}

/// Steps 4 and 5 over one read: the gap line comes before anything else of
/// this read, then the events above the position in order, up to the first
/// match or the first late event.
///
/// A gap that cannot be journaled ends the read at once: nothing is
/// examined, `after` and `examined` keep their values, the visit does not
/// park, and the next poll reads again whether or not the log changed.
fn examine(
    cx: &mut WaitCx<'_>,
    w: &apb_core::inbox_wait::InboxWait,
    scan: &mut Scan,
    events: &[Event],
    list: &[InboxEvent],
    timeout_seconds: u64,
) -> Result<Examined, EngineError> {
    // Step 4: the gap line comes before anything else of this read.
    let first_seq = list.first().map(|e| e.seq);
    if let Some(gap) = apb_core::connector::inbox::lost_prefix(scan.after, first_seq)
        && !journal_gap(cx, w, &gap)
    {
        // The read ends at once. The step 5 loop never runs, so it never
        // overwrites the `caught_up` this sets, and no line moves the
        // position past the loss.
        scan.read_state = ReadState::NotRead;
        scan.caught_up = false;
        return Ok(Examined::Nothing);
    }
    // Step 5: examine the events above the position in order; stop at
    // the first match or the first late event.
    let mut caught_up = true;
    let after = scan.after;
    for e in list.iter().filter(|e| e.seq > after) {
        if !in_time(events, cx.node, timeout_seconds, e.received_at) {
            // Late: not examined, `after` stays below it; the step goes
            // to 6, where the wait is necessarily over (the event was
            // received after the deadline and before now).
            caught_up = false;
            break;
        }
        if apb_core::inbox_wait::matches(w, &e.body, &scan.rendered, e.event_type.as_deref()) {
            let mut value = serde_json::Value::Object(apb_core::inbox_wait::output(
                w,
                e.seq,
                e.received_at,
                e.event_type.as_deref(),
                &e.body,
            ));
            if scan.redactor.is_none() {
                scan.redactor = Some(crate::run_redact::RunRedactor::for_storing(
                    cx.root, cx.run_dir,
                ));
            }
            if scan
                .redactor
                .as_ref()
                .unwrap()
                .secret_json(&mut value)
                .is_err()
            {
                return Ok(Examined::Failed(
                    "inbox_wait_secrets_unresolved: the run's secrets could not be resolved, so the event's values could not be stored scrubbed".into(),
                ));
            }
            let output = serde_json::to_string(&value)
                .map_err(|e| EngineError::Invalid(format!("inbox wait output: {e}")))?;
            scan.caught_up = false;
            return Ok(Examined::Matched(e.seq, output));
        }
        scan.examined += 1;
        scan.after = e.seq;
    }
    scan.caught_up = caught_up;
    Ok(Examined::Nothing)
}

/// One poll of an open visit (D4 "The visit").
pub(crate) fn poll(
    cx: &mut WaitCx<'_>,
    w: &apb_core::inbox_wait::InboxWait,
    scan: &mut Scan,
    elapsed_ms: u128,
    timeout_seconds: u64,
) -> Result<Poll, EngineError> {
    let events = crate::event::read_all(cx.run_dir)?;

    // Step 1: a decided end whose end event is missing (a write of the end
    // pair cut after its first line, crash table row 8): the end is
    // appended alone from the journal, no `authorize`, no read.
    if let Some((_line, kind, output)) = torn_end(&events, cx.node) {
        let node = cx.node.to_string();
        let event = if kind == "matched" {
            EventPayload::WaitSignalled { node }
        } else {
            EventPayload::WaitTimeout { node }
        };
        cx.log.append(event)?;
        return Ok(match output {
            Some(o) => Poll::Ended {
                output: o.to_string(),
            },
            None => Poll::TimedOut,
        });
    }

    // Step 2: the first poll of this drive authorizes before it reads, and
    // renders every condition over the run state.
    if !scan.first_poll_done {
        scan.first_poll_done = true;
        if let Err(reason) = authorize(cx.root, cx.run_dir, cx.node, w) {
            return Ok(Poll::Failed {
                reason: format!("inbox_wait_unauthorized: {reason}"),
            });
        }
        scan.rendered = render_conditions(cx, w)?;
        scan.after = position(&events, cx.node).unwrap_or(0);
    }

    // Step 3: read when due - the first read of this drive, or the log
    // changed and the least interval passed since the last one.
    let now = apb_core::clock::now_ms_u64();
    let mut fresh: Option<Vec<InboxEvent>> = None;
    if read_due(w, scan, now) {
        match read_log(cx, w, false) {
            Ok((stamp, list)) => {
                scan.read_state = ReadState::of(stamp);
                scan.last_read_ms = now;
                fresh = Some(list);
            }
            Err(e) => {
                // A failed read is never cached: the state is dropped, so
                // the next poll past the interval reads again whether or
                // not the file changed. The visit does not park meanwhile.
                // The attempt itself is timed, so a log that stays
                // unreadable is tried once per read interval, not on every
                // poll.
                scan.read_state = ReadState::NotRead;
                scan.last_read_ms = now;
                scan.caught_up = false;
                log_read_error(scan, cx.node, &e);
            }
        }
    }

    // Steps 4 and 5 over the fresh read.
    let mut matched = match &fresh {
        Some(list) => examine(cx, w, scan, &events, list, timeout_seconds)?,
        None => Examined::Nothing,
    };
    if let Examined::Failed(reason) = matched {
        return Ok(Poll::Failed { reason });
    }

    // Step 6: the timeout, with no match. Before the visit ends, one final
    // read that ignores the read interval and the stamp comparison, so an
    // event stored in time is never lost to the interval between two
    // reads.
    if matches!(matched, Examined::Nothing)
        && elapsed_ms >= u128::from(timeout_seconds) * 1000
        && fresh.is_none()
    {
        matched = final_read(cx, w, scan, &events, timeout_seconds)?;
        if let Examined::Failed(reason) = matched {
            return Ok(Poll::Failed { reason });
        }
    }

    if matches!(matched, Examined::Nothing) && elapsed_ms < u128::from(timeout_seconds) * 1000 {
        return Ok(Poll::Open);
    }

    // Step 7: the end pair, one write.
    let node = cx.node.to_string();
    let (through_seq, end, output, end_event) = match &matched {
        Examined::Matched(seq, output) => (
            *seq,
            "matched",
            Some(output.clone()),
            EventPayload::WaitSignalled { node },
        ),
        Examined::Nothing => (
            scan.after,
            "timeout",
            None,
            EventPayload::WaitTimeout { node },
        ),
        Examined::Failed(_) => unreachable!("a failed read ends the poll above"),
    };
    cx.log.append_pair(
        EventPayload::InboxWaitRead {
            node: cx.node.to_string(),
            through_seq,
            examined: scan.examined,
            end: Some(end.into()),
            output,
        },
        |_, _| {},
        |_seq| end_event,
    )?;
    Ok(match matched {
        Examined::Matched(_, output) => Poll::Ended { output },
        Examined::Nothing => Poll::TimedOut,
        Examined::Failed(_) => unreachable!("a failed read ends the poll above"),
    })
}

/// Whether the account's log is due a read: the first read of this drive
/// always reads; after that the least interval must have passed, and the
/// state holds no read (the last one failed) or the log's stamp now
/// differs from the one held (absent against present included).
fn read_due(w: &apb_core::inbox_wait::InboxWait, scan: &Scan, now: u64) -> bool {
    if scan.last_read_ms == 0 {
        return true;
    }
    if now.saturating_sub(scan.last_read_ms) < apb_core::inbox_wait::MIN_READ_INTERVAL_MS {
        return false;
    }
    let current = inbox_of(w).ok().and_then(|i| i.log_stamp());
    scan.read_state.read_due(current)
}

/// The final read before a timeout end: one read that ignores the read
/// interval and the stamp comparison, tried once more at once when it
/// fails. When it fails again the visit ends `timeout` with the position
/// it held before the read: nothing is lost and the timeout stays a bound.
fn final_read(
    cx: &mut WaitCx<'_>,
    w: &apb_core::inbox_wait::InboxWait,
    scan: &mut Scan,
    events: &[Event],
    timeout_seconds: u64,
) -> Result<Examined, EngineError> {
    for attempt in 0..2 {
        match read_log(cx, w, true) {
            Ok((stamp, list)) => {
                scan.read_state = ReadState::of(stamp);
                scan.last_read_ms = apb_core::clock::now_ms_u64();
                return examine(cx, w, scan, events, &list, timeout_seconds);
            }
            Err(e) => {
                scan.read_state = ReadState::NotRead;
                scan.caught_up = false;
                log_read_error(scan, cx.node, &e);
                if attempt == 1 {
                    break;
                }
            }
        }
    }
    Ok(Examined::Nothing)
}

/// The visit's one line about a failed read on stderr.
fn log_read_error(scan: &mut Scan, node: &str, cause: &str) {
    if !scan.read_error_logged {
        scan.read_error_logged = true;
        eprintln!("apb inbox_wait_read_error node={node}: {cause}");
    }
}

/// The output a journaled end carries: the latest `inbox_wait_read.output`
/// of the node after its latest `wait_started` (the end pair wrote it in
/// one write), `None` when no line carries one (D4 "An ended visit").
pub(crate) fn journaled_output(events: &[Event], node: &str) -> Option<String> {
    let started = events.iter().rposition(
        |e| matches!(&e.payload, EventPayload::WaitStarted { node: n, .. } if n == node),
    )?;
    events[started..]
        .iter()
        .rev()
        .find_map(|e| match &e.payload {
            EventPayload::InboxWaitRead {
                node: n,
                output: Some(o),
                ..
            } if n == node => Some(o.clone()),
            _ => None,
        })
}

/// The output of a visit that ended signalled, finished from the journal:
/// the end line's output, or `{}` when no read line carries one. A real
/// inbox wait writes its output in the same write as the end pair, so only
/// a scripted wait (a scenario run, which reads nothing and journals no
/// read line) has none, and `{}` is what its uninterrupted run outputs.
pub(crate) fn signalled_output(events: &[Event], node: &str) -> String {
    journaled_output(events, node).unwrap_or_else(|| "{}".to_string())
}

/// The end line of the open visit whose end event is missing: a write of
/// the end pair cut after its first line (crash table row 8). The latest
/// `inbox_wait_read` of the node with `end` set after its latest
/// `wait_started`, with no end event of the node after it, with the end's
/// kind (`matched` or `timeout`) and the line's output.
pub(crate) fn torn_end<'a>(
    events: &'a [Event],
    node: &str,
) -> Option<(&'a Event, &'a str, Option<&'a str>)> {
    let mut declared = false;
    let mut candidate: Option<(&Event, &str, Option<&str>)> = None;
    for e in events {
        match &e.payload {
            EventPayload::WaitStarted { node: n, .. } if n == node => {
                declared = true;
                candidate = None;
            }
            EventPayload::NodeFinished { node: n, .. } if n == node => {
                declared = false;
                candidate = None;
            }
            EventPayload::WaitSignalled { node: n } | EventPayload::WaitTimeout { node: n }
                if n == node =>
            {
                // The end event followed: the pair is whole, nothing torn.
                candidate = None;
            }
            EventPayload::InboxWaitRead {
                node: n,
                end: Some(kind),
                output,
                ..
            } if n == node && declared => {
                candidate = Some((e, kind.as_str(), output.as_deref()));
            }
            _ => {}
        }
    }
    candidate
}

/// The position of `node`: the `through_seq` of the latest counted
/// `inbox_wait_read` of the node in the run's journal, any visit, start,
/// progress or end line; `None` before the node's first complete start
/// line (D4 "The position"). The only position state there is.
///
/// Which lines count, read from the event that follows: `start` writes the
/// start line and `wait_started` in one `append_pair`, so a complete start
/// line is directly followed by the node's `wait_started`; a progress or an
/// end line is written only inside a declared visit. Any other line of the
/// node is a torn start line (the pair's write was cut after its first
/// line), superseded and ignored, so `position` reads `None` when the torn
/// line was the node's first.
pub(crate) fn position(events: &[Event], node: &str) -> Option<u64> {
    let mut declared = false;
    let mut position = None;
    for (i, e) in events.iter().enumerate() {
        match &e.payload {
            EventPayload::InboxWaitRead {
                node: n,
                through_seq,
                ..
            } if n == node => {
                // Counted when the next event is the node's `wait_started`
                // (a complete start line) or when a visit of the node is
                // declared at this line (a progress or end line of that
                // visit). Anything else is a torn start line: ignored.
                let complete_start = events.get(i + 1).is_some_and(|next| {
                    matches!(
                        &next.payload,
                        EventPayload::WaitStarted { node: m, .. } if m == node
                    )
                });
                if complete_start || declared {
                    position = Some(*through_seq);
                }
            }
            EventPayload::WaitStarted { node: n, .. } if n == node => {
                declared = true;
            }
            EventPayload::NodeFinished { node: n, .. } if n == node => {
                declared = false;
            }
            _ => {}
        }
    }
    position
}

/// Whether a stored event received at `received_at` arrived before the
/// deadline of the current visit of `node` (D4 "The deadline"): the wall
/// instant at which the run's paused-interval clock, measured from the
/// visit's `wait_started`, reaches `timeout_seconds`. Pure over the
/// journal; evaluated at `received_at`, so a poll that comes late neither
/// loses an event that arrived in time nor accepts one that arrived after.
/// The deadline instant lies after the receipt exactly when the wait's
/// active time at the receipt is below the timeout, so the comparison is
/// made there, in `u128`: a timeout whose deadline does not fit a `u64`
/// instant still takes an event received in time. An event received at
/// the deadline itself is late.
pub(crate) fn in_time(
    events: &[Event],
    node: &str,
    timeout_seconds: u64,
    received_at: u64,
) -> bool {
    crate::scheduler::wait_active_ms(events, node, u128::from(received_at))
        .is_some_and(|active| active < u128::from(timeout_seconds) * 1000)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pause::kind::{PauseBy, PauseMode};

    fn at(seq: u128, payload: EventPayload) -> Event {
        Event {
            seq: seq as u64,
            ts: seq,
            payload,
        }
    }

    /// A start line: no `end`, no `output`.
    fn start_line(seq: u128, node: &str, through: u64) -> Event {
        at(
            seq,
            EventPayload::InboxWaitRead {
                node: node.into(),
                through_seq: through,
                examined: 0,
                end: None,
                output: None,
            },
        )
    }

    /// A progress line: no `end`, no `output`.
    fn progress(seq: u128, node: &str, through: u64, examined: u32) -> Event {
        at(
            seq,
            EventPayload::InboxWaitRead {
                node: node.into(),
                through_seq: through,
                examined,
                end: None,
                output: None,
            },
        )
    }

    /// An end line that matched.
    fn end_line(seq: u128, node: &str, through: u64, examined: u32) -> Event {
        at(
            seq,
            EventPayload::InboxWaitRead {
                node: node.into(),
                through_seq: through,
                examined,
                end: Some("matched".into()),
                output: Some("{}".into()),
            },
        )
    }

    fn wait_started(seq: u128, node: &str) -> Event {
        at(
            seq,
            EventPayload::WaitStarted {
                node: node.into(),
                kind: "inbox".into(),
            },
        )
    }

    fn node_finished(seq: u128, node: &str) -> Event {
        at(
            seq,
            EventPayload::node_finished(node, "succeeded", 1, String::new()),
        )
    }

    fn paused(seq: u128, reason: &str, by: PauseBy) -> Event {
        at(
            seq,
            EventPayload::RunPaused {
                reason: reason.into(),
                by: Some(by),
                mode: Some(PauseMode::Drain),
                until: None,
                keep_lock: false,
            },
        )
    }

    fn resumed(seq: u128, by: PauseBy) -> Event {
        at(
            seq,
            EventPayload::RunResumed {
                from_node: "wait".into(),
                by: Some(by),
                explicit: false,
                launch: None,
                definition: None,
                apb: None,
                deadline: None,
            },
        )
    }

    /// The position is the `through_seq` of the latest counted line: the
    /// start line's own value while only the start pair exists, and the
    /// end line's after a progress and an end line of the visit.
    #[test]
    fn position_is_the_latest_read_line() {
        let start_pair = vec![start_line(1, "wait", 3), wait_started(2, "wait")];
        assert_eq!(position(&start_pair, "wait"), Some(3));
        let visit = vec![
            start_line(1, "wait", 3),
            wait_started(2, "wait"),
            progress(3, "wait", 5, 3),
            end_line(4, "wait", 9, 5),
        ];
        assert_eq!(position(&visit, "wait"), Some(9));
        // The visit's end event and the node's `node_finished` change
        // nothing about the lines that counted; a later torn line would.
        let closed = vec![
            start_line(1, "wait", 3),
            wait_started(2, "wait"),
            progress(3, "wait", 5, 3),
            end_line(4, "wait", 9, 5),
            node_finished(5, "wait"),
        ];
        assert_eq!(position(&closed, "wait"), Some(9));
        // Another node's lines never count.
        let other = vec![
            start_line(1, "other", 30),
            wait_started(2, "other"),
            progress(3, "other", 35, 1),
            end_line(4, "other", 39, 2),
        ];
        assert_eq!(position(&other, "wait"), None);
    }

    /// A start line the `wait_started` does not follow is a torn start
    /// line: superseded, never counted.
    #[test]
    fn position_ignores_a_torn_start_line() {
        // With only the torn line: no position at all.
        let torn = vec![start_line(1, "wait", 7)];
        assert_eq!(position(&torn, "wait"), None);
        // A complete start pair after the torn line: the torn line never
        // counted, the position is the later start line's `through_seq`.
        let after_torn = vec![
            start_line(1, "wait", 7),
            start_line(2, "wait", 8),
            wait_started(3, "wait"),
        ];
        assert_eq!(position(&after_torn, "wait"), Some(8));
        // A torn line under an open visit of another node stays ignored.
        let torn_under_other = vec![
            start_line(1, "other", 1),
            wait_started(2, "other"),
            start_line(3, "wait", 7),
            node_finished(4, "other"),
        ];
        assert_eq!(position(&torn_under_other, "wait"), None);
        // A torn line before a later visit's declared lines stays ignored.
        let torn_then_visit = vec![
            start_line(1, "wait", 7),
            node_finished(2, "other"),
            start_line(3, "wait", 10),
            wait_started(4, "wait"),
            progress(5, "wait", 12, 2),
        ];
        assert_eq!(position(&torn_then_visit, "wait"), Some(12));
        // A progress and an end line inside a declared visit count.
        let in_visit = vec![
            start_line(1, "wait", 0),
            wait_started(2, "wait"),
            progress(3, "wait", 5, 3),
            end_line(4, "wait", 9, 5),
        ];
        assert_eq!(position(&in_visit, "wait"), Some(9));
    }

    const TIMEOUT_SECONDS: u64 = 10;

    /// Receipt decides, on the paused-interval clock: the deadline is the
    /// `wait_started` plus the timeout, moved by a person's pause, unmoved
    /// by a park (a park is active wait time). An event received 1 ms
    /// before the deadline is in time, one at it and one after are not.
    #[test]
    fn in_time_reads_the_receipt_on_the_paused_interval_clock() {
        // No pause: the deadline is the wait_started (1_000) plus 10 s.
        let plain = vec![wait_started(1_000, "wait")];
        assert!(in_time(&plain, "wait", TIMEOUT_SECONDS, 10_999));
        assert!(!in_time(&plain, "wait", TIMEOUT_SECONDS, 11_000));
        assert!(!in_time(&plain, "wait", TIMEOUT_SECONDS, 11_001));

        // A person's pause of 10 s between the wait_started and the
        // receipt moves the deadline by the pause's length: 21_000.
        let person = vec![
            wait_started(1_000, "wait"),
            paused(2_000, "looking", PauseBy::Operator),
            resumed(12_000, PauseBy::Operator),
        ];
        assert!(in_time(&person, "wait", TIMEOUT_SECONDS, 20_999));
        assert!(!in_time(&person, "wait", TIMEOUT_SECONDS, 21_000));

        // A park of 5 s does not move the deadline: the paused-interval
        // clock counts park time as active, so the deadline stays 11_000.
        let park = vec![
            wait_started(1_000, "wait"),
            paused(2_000, "wait (wait)", PauseBy::Engine),
            resumed(7_000, PauseBy::Engine),
        ];
        assert!(in_time(&park, "wait", TIMEOUT_SECONDS, 10_999));
        assert!(!in_time(&park, "wait", TIMEOUT_SECONDS, 11_000));

        // No wait_started of the node: nothing is in time (no visit to be
        // in time of).
        assert!(!in_time(&[], "wait", TIMEOUT_SECONDS, 0));
    }

    /// A signalled end finished from the journal outputs the end line's
    /// output, and `{}` for a scripted wait, which journals no read line,
    /// as its uninterrupted run does.
    #[test]
    fn a_signalled_end_outputs_the_empty_object_without_a_read_line() {
        let scripted = vec![wait_started(1_000, "wait")];
        assert_eq!(signalled_output(&scripted, "wait"), "{}");
        let mut read = scripted.clone();
        read.push(at(
            1_500,
            EventPayload::InboxWaitRead {
                node: "wait".into(),
                through_seq: 3,
                examined: 0,
                end: Some("matched".into()),
                output: Some("{\"seq\":3}".into()),
            },
        ));
        assert_eq!(signalled_output(&read, "wait"), "{\"seq\":3}");
    }

    /// A timeout whose deadline instant does not fit a `u64` still takes an
    /// event received in time: the comparison is of the active time at the
    /// receipt with the timeout, never of a computed deadline instant.
    #[test]
    fn in_time_takes_an_event_under_a_timeout_past_the_u64_instant() {
        let events = vec![wait_started(1_000, "wait")];
        assert!(in_time(&events, "wait", u64::MAX / 1000, 2_000));
        assert!(in_time(&events, "wait", u64::MAX, 2_000));
    }

    /// A condition renders over the run state as a prompt renders: a
    /// review decision, the run's context and an earlier success's
    /// outputs all read. The one render every template of a run goes
    /// through.
    #[test]
    fn a_condition_renders_over_the_run_state() {
        let dir = tempfile::tempdir().unwrap();
        let run_dir = dir.path().join("run");
        std::fs::create_dir_all(&run_dir).unwrap();
        {
            let mut log = EventLog::create(&run_dir).unwrap();
            log.append(EventPayload::node_finished(
                "a",
                "succeeded",
                1,
                "the earlier output",
            ))
            .unwrap();
        }
        let playbook = apb_core::schema::Playbook::from_yaml(
            "schema: 2\nid: p\nname: p\nversion: 1.0.0\nnodes:\n  - { id: a, type: start }\nedges: []\n",
        )
        .unwrap();
        let mut state = RunState::default();
        state.reviews.insert(
            "gate".to_string(),
            crate::state::ReviewDecision {
                decision: "approved".to_string(),
                note: String::new(),
            },
        );
        state.previous_success = Some(crate::event::PreviousSuccess {
            run_id: Some("r0".to_string()),
            outputs: [(
                "nodes.a.output".to_string(),
                "the earlier success".to_string(),
            )]
            .into_iter()
            .collect(),
            ..Default::default()
        });
        let cfg = crate::run_config::RunConfig::default();
        let mut log = EventLog::open(&run_dir).unwrap();
        let cx = WaitCx {
            root: dir.path(),
            run_dir: &run_dir,
            node: "w",
            log: &mut log,
            state: &state,
            cfg: &cfg,
            cancel: &AtomicBool::new(false),
            playbook: &playbook,
            run_id: "r1",
        };
        let condition = |path: &str, equals: &str| apb_core::inbox_wait::Condition {
            path: path.to_string(),
            equals: equals.to_string(),
        };
        let w = apb_core::inbox_wait::InboxWait {
            connector: "echo".to_string(),
            account: "main".to_string(),
            conditions: vec![
                condition("a", "{{nodes.gate.review_decision}}"),
                condition("b", "{{run.previous_success.nodes.a.output}}"),
                condition("c", "{{run.context}}"),
            ],
            types: Vec::new(),
            take: Vec::new(),
        };
        let rendered = render_conditions(&cx, &w).unwrap();
        assert_eq!(rendered[0], "approved", "the review decision");
        assert_eq!(rendered[1], "the earlier success", "the earlier success");
        assert!(
            rendered[2].contains("the earlier output"),
            "the run's context: {}",
            rendered[2]
        );
    }
}

/// The parked-wait fixture of the `pending` and `pause::waker` tests (test
/// builds only): one project with the `echo-hooks` fixture connector
/// installed and approved, the account configured and approved, and a run
/// whose inbox wait parked with a journaled start pair and `run_paused`.
/// Every test runs through `in_child`: the fixture sets `APB_CONFIG_DIR`,
/// which is process-wide.
#[cfg(test)]
pub(crate) mod park_fixture {
    use crate::event::{Event, EventLog, EventPayload, read_all};
    use apb_core::connector::inbox::{Appended, Inbox, Retention};
    use std::path::{Path, PathBuf};

    pub(crate) const CONNECTOR: &str = "echo-hooks";
    pub(crate) const ACCOUNT: &str = "main";
    pub(crate) const NODE: &str = "wait";
    pub(crate) const RUN: &str = "iw-park";
    /// The wait node's `timeout_seconds`, read back by `in_time`.
    pub(crate) const TIMEOUT_SECONDS: u64 = 10;

    /// The prefix of the libtest path of a test in `inbox_wait::pending`.
    pub(crate) const PENDING_PREFIX: &str = "inbox_wait::pending::tests::";
    /// The prefix of the libtest path of a test in `pause::waker`.
    pub(crate) const WAKER_PREFIX: &str = "pause::waker::tests::";

    /// Runs `body` alone in a child process of this test binary: the fixture
    /// mutates `APB_CONFIG_DIR`, which parallel tests read.
    pub(crate) fn in_child(prefix: &str, name: &str, body: impl FnOnce()) {
        apb_core::process_mode::testing::in_plain_child(&format!("{prefix}{name}"), body);
    }

    pub(crate) struct Fx {
        _dir: tempfile::TempDir,
        cfg: PathBuf,
        pub(crate) root: PathBuf,
        pub(crate) run_dir: PathBuf,
        /// The journal stamp of the wait's `wait_started`.
        pub(crate) ws_ms: u128,
    }

    impl Fx {
        /// The project, the approvals and the parked run. The start line
        /// sits at `through`; `types` is the wait's `types` list ("": no
        /// list, every event).
        pub(crate) fn parked(types: &str, through: u64) -> Fx {
            let dir = tempfile::tempdir().unwrap();
            let cfg = dir.path().join("cfg");
            let root = dir.path().join("project");
            std::fs::create_dir_all(cfg.join("connectors").join(CONNECTOR)).unwrap();
            std::fs::create_dir_all(&root).unwrap();
            // SAFETY: the fixture runs alone in a plain child process.
            unsafe { std::env::set_var("APB_CONFIG_DIR", &cfg) };
            apb_core::registry::init_project(&root).unwrap();
            let src = Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("tests/fixtures/connectors")
                .join(CONNECTOR)
                .join("connector.yaml");
            std::fs::write(
                cfg.join("connectors")
                    .join(CONNECTOR)
                    .join("connector.yaml"),
                std::fs::read_to_string(src).unwrap(),
            )
            .unwrap();
            let loaded = apb_core::connector::store::load(CONNECTOR).unwrap();
            {
                use apb_core::trust::{OriginKind, TrustStore};
                let mut trust = TrustStore::load();
                trust
                    .approve(&loaded.digest, CONNECTOR, OriginKind::LocallyApproved)
                    .unwrap();
            }
            // The account, at the project level (its fields are plain
            // values: `authorize` digests them, and nothing here resolves
            // or validates them).
            let account = format!(
                "accounts:\n  - name: {ACCOUNT}\n    default: true\n    verify_token: \"token\"\n    app_secret: \"app-secret\"\n"
            );
            let path = root
                .join(".apb/connector-config")
                .join(format!("{CONNECTOR}.yaml"));
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(&path, account).unwrap();
            let accounts = apb_core::connector::config::load_merged(&root, CONNECTOR).unwrap();
            let live = accounts.iter().find(|a| a.name == ACCOUNT).unwrap();
            let digest = apb_core::connector::config::account_digest(live);
            {
                use apb_core::trust::{OriginKind, TrustStore};
                let mut trust = TrustStore::load();
                trust
                    .approve(
                        &digest,
                        &format!("{CONNECTOR}/{ACCOUNT}"),
                        OriginKind::LocallyApproved,
                    )
                    .unwrap();
            }
            // The run: snapshot, manifest with the wait's grant, journal.
            let run_dir = root.join(".apb/runs").join(RUN);
            std::fs::create_dir_all(&run_dir).unwrap();
            std::fs::write(run_dir.join("playbook.yaml"), playbook_yaml(types)).unwrap();
            std::fs::write(run_dir.join("run.yaml"), "unattended: false\n").unwrap();
            let mut m = crate::manifest::RunExecutionManifest::default();
            m.connectors.push(crate::manifest::ManifestConnector {
                name: CONNECTOR.into(),
                digest: loaded.digest.clone(),
                accounts: vec![crate::manifest::ManifestAccount {
                    name: ACCOUNT.into(),
                    default: true,
                    fields: Default::default(),
                    env: Default::default(),
                    cmd: Default::default(),
                    digest: digest.clone(),
                }],
                doubled_accounts: Vec::new(),
            });
            // `authorize` looks the site up: `<node>#inbox`.
            m.connector_grants.insert(
                apb_core::inbox_wait::inbox_site(NODE),
                vec![crate::manifest::ManifestConnectorGrant {
                    connector: CONNECTOR.into(),
                    accounts: vec![ACCOUNT.into()],
                    functions: Vec::new(),
                    max_calls: None,
                    on_unknown: None,
                }],
            );
            crate::manifest::write(&run_dir, &m).unwrap();
            let mut log = EventLog::create(&run_dir).unwrap();
            for payload in [
                EventPayload::InboxWaitRead {
                    node: NODE.into(),
                    through_seq: through,
                    examined: 0,
                    end: None,
                    output: None,
                },
                EventPayload::WaitStarted {
                    node: NODE.into(),
                    kind: "inbox".into(),
                },
                // The park carries an `until` well past the deadline, so a
                // waker that checks the delivery first is the one that wakes
                // the run.
                EventPayload::RunPaused {
                    reason: crate::pause::cause::wait_reason(NODE),
                    by: Some(crate::pause::kind::PauseBy::Engine),
                    mode: Some(crate::pause::kind::PauseMode::Drain),
                    until: Some(u64::try_from(apb_core::clock::now_ms()).unwrap_or(0) + 60_000),
                    keep_lock: false,
                },
            ] {
                log.append(payload).unwrap();
            }
            drop(log);
            let ws_ms = read_all(&run_dir)
                .unwrap()
                .iter()
                .find(|e| matches!(&e.payload, EventPayload::WaitStarted { .. }))
                .unwrap()
                .ts;
            // The origin stamp, for the waker's resume gate.
            apb_core::run_origin::ensure_key().unwrap();
            apb_core::run_origin::stamp(&run_dir, RUN).unwrap();
            Fx {
                _dir: dir,
                cfg,
                root,
                run_dir,
                ws_ms,
            }
        }

        /// The parked run of a timer wait: the node waits for its timer,
        /// the journal holds the `wait_started` and the pause, nothing of
        /// the inbox.
        pub(crate) fn timer_parked() -> Fx {
            let dir = tempfile::tempdir().unwrap();
            let cfg = dir.path().join("cfg");
            let root = dir.path().join("project");
            std::fs::create_dir_all(cfg.join("connectors").join(CONNECTOR)).unwrap();
            std::fs::create_dir_all(&root).unwrap();
            // SAFETY: the fixture runs alone in a plain child process.
            unsafe { std::env::set_var("APB_CONFIG_DIR", &cfg) };
            apb_core::registry::init_project(&root).unwrap();
            let src = Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("tests/fixtures/connectors")
                .join(CONNECTOR)
                .join("connector.yaml");
            std::fs::write(
                cfg.join("connectors")
                    .join(CONNECTOR)
                    .join("connector.yaml"),
                std::fs::read_to_string(src).unwrap(),
            )
            .unwrap();
            let run_dir = root.join(".apb/runs").join(RUN);
            std::fs::create_dir_all(&run_dir).unwrap();
            std::fs::write(run_dir.join("playbook.yaml"), timer_yaml()).unwrap();
            std::fs::write(run_dir.join("run.yaml"), "unattended: false\n").unwrap();
            let mut log = EventLog::create(&run_dir).unwrap();
            for payload in [
                EventPayload::WaitStarted {
                    node: NODE.into(),
                    kind: "timer".into(),
                },
                EventPayload::RunPaused {
                    reason: crate::pause::cause::wait_reason(NODE),
                    by: Some(crate::pause::kind::PauseBy::Engine),
                    mode: Some(crate::pause::kind::PauseMode::Drain),
                    until: None,
                    keep_lock: false,
                },
            ] {
                log.append(payload).unwrap();
            }
            drop(log);
            let ws_ms = read_all(&run_dir)
                .unwrap()
                .iter()
                .find(|e| matches!(&e.payload, EventPayload::WaitStarted { .. }))
                .unwrap()
                .ts;
            Fx {
                _dir: dir,
                cfg,
                root,
                run_dir,
                ws_ms,
            }
        }

        /// The account's inbox, under the fixture's config dir.
        pub(crate) fn inbox(&self) -> Inbox {
            Inbox::at(&self.cfg.join("connector-inbox"), CONNECTOR, ACCOUNT).unwrap()
        }

        /// An untyped event appended with the wall clock.
        pub(crate) fn store(&self, body: serde_json::Value) -> u64 {
            match self
                .inbox()
                .append_typed(
                    &format!("m-{}", apb_core::connector::marker::new_nonce()),
                    None,
                    &body,
                )
                .unwrap()
            {
                Appended::Stored(seq) => seq,
                other => panic!("the event was not stored: {other:?}"),
            }
        }

        /// A typed event appended with the wall clock.
        pub(crate) fn store_typed(&self, event_type: &str, body: serde_json::Value) -> u64 {
            match self
                .inbox()
                .append_typed(
                    &format!("m-{}", apb_core::connector::marker::new_nonce()),
                    Some(event_type),
                    &body,
                )
                .unwrap()
            {
                Appended::Stored(seq) => seq,
                other => panic!("the event was not stored: {other:?}"),
            }
        }

        /// An event appended with an explicit receipt stamp: a delivery
        /// that arrived then.
        pub(crate) fn store_at(&self, now: u64, body: serde_json::Value) -> u64 {
            match self
                .inbox()
                .append_bounded(
                    &format!("m-{}", apb_core::connector::marker::new_nonce()),
                    &body,
                    None,
                    &Retention::default(),
                    u64::MAX,
                    usize::MAX,
                    now,
                )
                .unwrap()
            {
                Appended::Stored(seq) => seq,
                other => panic!("the event was not stored: {other:?}"),
            }
        }

        /// Raw lines appended to the account's log, seqs `from` to
        /// `through`: what retention leaves behind.
        pub(crate) fn store_raw(&self, from: u64, through: u64, body: serde_json::Value) {
            use std::io::Write as _;
            let dir = self
                .cfg
                .join("connector-inbox")
                .join(CONNECTOR)
                .join(ACCOUNT);
            std::fs::create_dir_all(&dir).unwrap();
            let path = dir.join("events.jsonl");
            let mut f = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&path)
                .unwrap();
            for seq in from..=through {
                let line = serde_json::json!({
                    "seq": seq,
                    "received_at": apb_core::clock::now_ms_u64(),
                    "provider_id": format!("raw-{seq}"),
                    "body": body,
                });
                writeln!(f, "{line}").unwrap();
            }
        }

        /// The account log's raw bytes.
        pub(crate) fn log_bytes(&self) -> Vec<u8> {
            std::fs::read(
                self.cfg
                    .join("connector-inbox")
                    .join(CONNECTOR)
                    .join(ACCOUNT)
                    .join("events.jsonl"),
            )
            .unwrap()
        }

        /// Overwrites the account log with `bytes`, then restores its
        /// modification time to `stamp`: a change of content with the
        /// stamp of the old content.
        pub(crate) fn rewrite_log_with_stamp(
            &self,
            bytes: &[u8],
            stamp: (u64, std::time::SystemTime),
        ) {
            let path = self
                .cfg
                .join("connector-inbox")
                .join(CONNECTOR)
                .join(ACCOUNT)
                .join("events.jsonl");
            let mut f = std::fs::OpenOptions::new()
                .write(true)
                .truncate(true)
                .open(&path)
                .unwrap();
            use std::io::Write as _;
            f.write_all(bytes).unwrap();
            f.set_times(std::fs::FileTimes::new().set_modified(stamp.1))
                .unwrap();
        }
    }

    /// The parked inbox wait's playbook, with `types`.
    fn playbook_yaml(types: &str) -> String {
        let types = if types.is_empty() {
            String::new()
        } else {
            format!("\n      types: [{types}]")
        };
        format!(
            r#"
schema: 1
id: w
name: W
version: 1.0.0
nodes:
  - {{ id: start, type: start }}
  - id: wait
    type: wait
    wait_for:
      type: inbox
      connector: {CONNECTOR}
      account: {ACCOUNT}{types}
    timeout_seconds: {TIMEOUT_SECONDS}
  - {{ id: ok, type: finish, outcome: success }}
edges:
  - {{ from: start, to: wait }}
  - {{ from: wait, to: ok }}
"#
        )
    }

    /// The parked timer wait's playbook.
    fn timer_yaml() -> String {
        r#"
schema: 1
id: w
name: W
version: 1.0.0
nodes:
  - { id: start, type: start }
  - { id: wait, type: wait, wait_for: { type: timer, seconds: 3 }, timeout_seconds: 3600 }
  - { id: done, type: finish, outcome: success }
edges:
  - { from: start, to: wait }
  - { from: wait, to: done }
"#
        .to_string()
    }

    /// The events of the fixture's run journal.
    pub(crate) fn events(fx: &Fx) -> Vec<Event> {
        read_all(&fx.run_dir).unwrap()
    }

    /// The journal seq of the fixture's `run_paused`.
    pub(crate) fn pause_seq(fx: &Fx) -> u64 {
        events(fx)
            .iter()
            .rev()
            .find(|e| matches!(&e.payload, EventPayload::RunPaused { .. }))
            .unwrap()
            .seq
    }
}
