//! One answer to "is this still alive?", for every caller that needs it.
//!
//! Three separate questions live here because they share one hazard and must
//! share one bias:
//!
//!   * is OS process `pid` running (`pid_alive`, `pid_is_live`);
//!   * is a process really driving run `<id>` right now (`driver_is_live`);
//!   * what does the run journal say about attempts that never closed
//!     (`open_attempts`, `node_times`, `lost_nodes`).
//!
//! The hazard is that a wrong "dead" is far worse than a wrong "alive". A
//! false "dead" lets a caller write a terminal event over work that is still
//! going on, or report a healthy node as `lost`; a false "alive" only leaves a
//! run that a later stop (or `apb doctor --run`) can still finalize. So every
//! rule here is biased toward "alive", and only a probe that positively
//! answers "there is no such process" may conclude otherwise.
//!
//! This module exists so that bias is written once. It previously lived
//! half in `stop.rs` (the `ps`-based probe) and half in `workdir.rs` (the raw
//! `kill -0`), and a second copy would have been the natural way to add
//! liveness to `run_status`.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::process::Command;

use serde::Serialize;

use crate::event::{Event, EventPayload, now_millis};
use crate::state::{NodeStatus, RunState};

// ---------------------------------------------------------------------------
// Process liveness
// ---------------------------------------------------------------------------

/// A pid that could actually name a process, as the `i32` `kill(2)` wants.
///
/// `None` for values that cannot name one, and the distinction matters far
/// more than it looks: to `kill(2)`, the out-of-range values are not invalid,
/// they are WILDCARDS. `pid 0` means "every process in my group" and `-1`
/// means "every process I may signal". A `u32` pid above `i32::MAX` becomes
/// negative when narrowed - `u32::MAX` becomes exactly `-1` - so probing a
/// garbage pid asked "may I signal EVERYTHING?", got yes, and reported the
/// garbage pid as a running process.
///
/// A pid file holding an out-of-range or corrupt value therefore read as
/// permanently held, wedging the workdir with no way to reclaim it, which is
/// the bug this rejects. It is also why the narrowing must never be done
/// silently anywhere near a signal: with a real signal instead of the 0 used
/// here, the same truncation would have killed every process the user owns.
fn probeable_pid(pid: u32) -> Option<i32> {
    match i32::try_from(pid) {
        Ok(p) if p > 0 => Some(p),
        _ => None,
    }
}

/// The raw primitive: does process `pid` exist? Cheaper than `process_probe`
/// and enough where a pid cannot have been reused (the workdir lock, held for
/// the lifetime of one process). Anything that must survive pid reuse wants
/// `driver_is_live` instead.
///
/// This is the `kill(pid, 0)` syscall, not a `kill -0` subprocess. The
/// subprocess form was both a portability hazard (BSD and procps-ng `kill`
/// disagree about argument parsing, as `proc.rs` and `detect.rs` record) and
/// unable to tell the module's three cases apart, because an exit code cannot
/// distinguish "no such process" from "could not ask".
///
/// The bias is the module's: only `ESRCH`, a positive "there is no such
/// process", counts as dead. `EPERM` means the process is there but not ours
/// to signal, which is alive; anything else is unknown, which is also alive.
/// Note this direction is a CHANGE - the old code's `.unwrap_or(false)` on a
/// failed spawn meant a host without a usable `kill` binary reported every pid
/// as dead, which is precisely the wrong-"dead" this module exists to prevent.
///
/// One caveat, so the bias is not oversold as strictly safer. On a NON-UNIX
/// host there is no probe at all, so every probeable pid resolves to "alive",
/// and for the workdir-lock caller that trades one permanent failure mode for
/// another: `workdir::acquire` would then refuse the workdir for any lock file
/// holding any positive number, with no self-healing path as the process it
/// names comes and goes. The escape is `--allow-shared-workdir`, which skips
/// the lock entirely. This is unreachable today - the release matrix is
/// aarch64/x86_64-darwin plus linux-gnu/linux-musl, with no Windows target -
/// and it is recorded here so that adding one is a deliberate decision rather
/// than a surprise.
pub fn pid_alive(pid: u32) -> bool {
    // Ahead of the platform split on purpose: a pid that cannot name a process
    // is dead everywhere, and the non-unix arm's "unknown means alive" must
    // not swallow that and hand back a permanently-held lock.
    let Some(pid) = probeable_pid(pid) else {
        return false;
    };
    #[cfg(unix)]
    {
        // SAFETY: `kill` takes no pointers and is async-signal-safe; signal 0
        // performs the existence and permission checks without delivering
        // anything. `pid` is known positive, so no wildcard can be reached.
        if unsafe { libc::kill(pid, 0) } == 0 {
            return true;
        }
        std::io::Error::last_os_error().raw_os_error() != Some(libc::ESRCH)
    }
    #[cfg(not(unix))]
    {
        // No probe available: unknown, and unknown means alive.
        let _ = pid;
        true
    }
}

/// The answer to "what is process `pid`?". Deliberately three-valued: "the
/// probe says there is no such process" and "the probe could not tell us
/// anything" are very different facts, and collapsing them into `None` is what
/// would make a missing `ps` read as a dead driver.
pub(crate) enum Probe {
    Running(String),
    NotFound,
    Unknown,
}

/// What one `ps` invocation returned, reduced to what the shape judgment
/// needs. `None` from a `PsRun` means `ps` could not be spawned at all.
struct PsOutput {
    success: bool,
    stdout: String,
}

/// How a probe actually asks the OS. A parameter rather than a hard-coded
/// call so `classify`'s shape judgment and the self-probe guard can be tested
/// against a deliberately broken `ps` without mutating this process's `PATH`
/// (these tests run as threads of one binary, so an env change would leak
/// into every other test that spawns a process).
type PsRun<'a> = &'a dyn Fn(u32) -> Option<PsOutput>;

/// Runs the real `ps` named by `program`. Split from `run_ps` so a test can
/// point it at a stub binary that reproduces a reduced `ps`.
fn run_ps_program(program: &str, pid: u32) -> Option<PsOutput> {
    let out = Command::new(program)
        .args(["-o", "stat=", "-o", "args=", "-p", &pid.to_string()])
        .output()
        .ok()?;
    Some(PsOutput {
        success: out.status.success(),
        stdout: String::from_utf8_lossy(&out.stdout).trim().to_string(),
    })
}

fn run_ps(pid: u32) -> Option<PsOutput> {
    run_ps_program("ps", pid)
}

/// The shape judgment: what one `ps` answer means. Pure, and the fragile part
/// of this module - the "no such process" and "this ps rejected my arguments"
/// answers are genuinely indistinguishable here, which is what the self-probe
/// guard in `guarded_probe` exists to compensate for.
fn classify(out: Option<PsOutput>) -> Probe {
    // `ps` is missing or could not be spawned at all.
    let Some(out) = out else {
        return Probe::Unknown;
    };
    if !out.success {
        // The ordinary "no such process" answer: a non-zero exit with nothing
        // on stdout (the diagnostic goes to stderr). A non-zero exit that DID
        // print something is some other failure, and we do not guess.
        return if out.stdout.is_empty() {
            Probe::NotFound
        } else {
            Probe::Unknown
        };
    }
    // A successful probe that printed nothing also means no such process.
    if out.stdout.is_empty() {
        return Probe::NotFound;
    }
    let Some((stat, argv)) = out.stdout.split_once(char::is_whitespace) else {
        // Output we cannot parse: unknown, not dead.
        return Probe::Unknown;
    };
    if stat.starts_with('Z') {
        return Probe::NotFound;
    }
    Probe::Running(argv.trim().to_string())
}

/// The shape judgment plus the self-probe guard.
///
/// The guard answers "is the `ps` on this host one we can believe?" by probing
/// a pid whose liveness we are certain of: our own. A `ps` that cannot see the
/// process asking the question cannot see anything.
///
/// It exists because of a specific failure mode. A busybox (or otherwise
/// reduced) `ps` rejects the `-o stat= -o args= -p <pid>` invocation outright:
/// it prints its usage to stderr and exits non-zero with an EMPTY stdout,
/// which is bit-for-bit the shape of the ordinary "no such process" answer.
/// Without this guard such a host classifies EVERY pid as dead, and a false
/// dead is the error that writes a terminal event over live work.
///
/// The guard is only consulted on the `NotFound` branch, which is an
/// optimization and not a change in meaning: `Running` and `Unknown` are
/// already safe verdicts (neither concludes "dead"), so a broken `ps` cannot
/// turn either of them into a harmful answer. `NotFound` is the only verdict
/// worth paying a second `ps` to double-check, and on a healthy host that
/// second call happens only for pids that really are gone.
fn guarded_probe(pid: u32, run: PsRun<'_>) -> Probe {
    // An impossible pid is `NotFound` without asking anyone. This is the same
    // rejection `pid_alive` makes, kept here so both probes classify a corrupt
    // pid file identically rather than inheriting whatever a given `ps` does
    // with an out-of-range argument. It cannot mask a live process: no such
    // value can name one.
    if probeable_pid(pid).is_none() {
        return Probe::NotFound;
    }
    match classify(run(pid)) {
        Probe::NotFound if !matches!(classify(run(std::process::id())), Probe::Running(_)) => {
            Probe::Unknown
        }
        other => other,
    }
}

pub(crate) fn process_probe(pid: u32) -> Probe {
    guarded_probe(pid, &run_ps)
}

/// Single-pid liveness with the module's bias: only a probe that positively
/// reports "no such process" counts as dead.
///
/// Unlike `driver_is_live` this does NOT defend against pid reuse - it cannot,
/// because a bare pid carries no identity. Callers that hold a pid recorded
/// long ago and would take a destructive action on "dead" want the argv-aware
/// check; callers that only report a fact (`run_status`, `doctor --run`) want
/// this one.
pub fn pid_is_live(pid: u32) -> bool {
    // Our own pid needs no probing and cannot be a reused number.
    if pid == std::process::id() {
        return true;
    }
    !matches!(process_probe(pid), Probe::NotFound)
}

// ---------------------------------------------------------------------------
// Driver liveness
// ---------------------------------------------------------------------------

/// Is a process really driving this run right now?
///
/// `driver.pid` alone cannot answer this. Drivers lead their own process group
/// and are reaped promptly, so their pids are released and REUSED: a bare
/// `kill -0` would happily succeed for a completely unrelated process that
/// inherited the number, and we would leave a dead run unfinalized forever.
///
/// The disambiguator is free: a detached driver's argv carries
/// `--run-id <id>`. Around that definitive signal the rule keeps the module's
/// bias toward "live".
pub fn driver_is_live(run_dir: &Path, run_id: &str) -> bool {
    match crate::driver::read_driver_pid(run_dir) {
        Some(pid) => driver_pid_is_live(pid, run_id),
        None => false,
    }
}

/// The same rule against a pid the caller has ALREADY read from `driver.pid`.
///
/// Callers that need both the pid and the verdict must go through this rather
/// than reading the file and then calling `driver_is_live`, which would read it
/// a second time. A drive that finishes cleanly between the two reads removes
/// the file, so the second read finds nothing and the pair reports "there is a
/// driver, and it is dead" for a run that in fact just completed normally.
pub fn driver_pid_is_live(pid: u32, run_id: &str) -> bool {
    // A drive running on a thread of THIS process (the CLI's synchronous run,
    // the in-process background drive) needs no probing and cannot be a
    // reused pid.
    if pid == std::process::id() {
        return true;
    }
    driver_verdict(run_id, process_probe(pid))
}

/// The driver rule applied to an already-obtained probe result. Separated from
/// the probing so the argv reasoning - and in particular the bias that an
/// unusable probe means "live" - is testable without a real process.
fn driver_verdict(run_id: &str, probe: Probe) -> bool {
    let argv = match probe {
        // The probe worked and the process is gone (or is a zombie, which
        // holds a pid but drives nothing). The only branch that may conclude
        // "dead" without knowing what the process is.
        Probe::NotFound => return false,
        // The probe itself did not work: no `ps` on this host, an unreadable
        // answer, a `ps` that failed its own self-probe. We know nothing, so
        // we assume the driver is alive.
        Probe::Unknown => return true,
        Probe::Running(argv) => argv,
    };
    if driver_argv_names_run(&argv, run_id) {
        // Definitive: this process is the detached driver of this very run.
        return true;
    }
    if argv.split_whitespace().any(|t| t == "__drive-run") {
        // A driver, but of some other run: our pid was reused.
        return false;
    }
    // Not a detached driver. Any other `apb` process may still be driving this
    // run on a thread (`apb run`, `apb mcp`), so we do not finalize behind its
    // back. Anything that is not `apb` at all is a reused pid.
    argv_program_is_apb(&argv)
}

/// Driver liveness as a three-valued answer for reporting: `None` when there
/// is no `driver.pid` at all (nothing claims to be driving, which is the
/// normal state of a finished run), otherwise whether that claim holds.
///
/// `run_status` needs the distinction: "no drive in progress" and "the drive
/// that claimed this run is gone" look identical under a plain bool, and only
/// the second one is a problem.
pub fn driver_alive(run_dir: &Path, run_id: &str) -> Option<bool> {
    // One read, then the verdict against that pid: see `driver_pid_is_live`
    // for why reading `driver.pid` twice reports a cleanly finished drive as a
    // dead one.
    let pid = crate::driver::read_driver_pid(run_dir)?;
    Some(driver_pid_is_live(pid, run_id))
}

/// Does this command line belong to the detached driver of `run_id`?
///
/// Compared token by token rather than as a substring: `--run-id stopflow-12`
/// is a prefix of `--run-id stopflow-123456`, so a substring test would let a
/// stop of the short run mistake the long run's driver for its own and report
/// a live driver that is in fact driving something else. Both the
/// `--run-id X` and `--run-id=X` spellings are accepted; the engine only ever
/// spawns the former, but reading argv is a loose contract.
fn driver_argv_names_run(argv: &str, run_id: &str) -> bool {
    let mut tokens = argv.split_whitespace();
    while let Some(tok) = tokens.next() {
        if tok == "--run-id" {
            return tokens.next() == Some(run_id);
        }
        if let Some(value) = tok.strip_prefix("--run-id=") {
            return value == run_id;
        }
    }
    false
}

fn argv_program_is_apb(argv: &str) -> bool {
    argv.split_whitespace()
        .next()
        .and_then(|p| p.rsplit('/').next())
        .is_some_and(|name| name == "apb" || name.starts_with("apb-") || name.starts_with("apb."))
}

// ---------------------------------------------------------------------------
// Attempt liveness, read off the journal
// ---------------------------------------------------------------------------

/// An attempt journaled as started that never wrote a matching
/// `attempt_finished`. On a healthy run the newest node has exactly one of
/// these while its agent works; on a crashed run it is the fingerprint of the
/// work that was in flight when the process died.
#[derive(Debug, Clone)]
pub struct OpenAttempt {
    pub node: String,
    pub attempt: u32,
    /// The agent pid captured at spawn. `None` for old journals and for paths
    /// that do not journal the spawn, in which case liveness is unknowable.
    pub pid: Option<u32>,
    /// Epoch milliseconds of the `attempt_started` event.
    pub started_ms: u128,
}

/// Every attempt still open at the end of the journal, ordered by node then
/// attempt number. Mirrors the `open` set inside `RunState::fold`; kept
/// separate because the fold deliberately throws away the pid and the
/// timestamp, which are exactly what liveness needs.
pub fn open_attempts(events: &[Event]) -> Vec<OpenAttempt> {
    let mut open: BTreeMap<(String, u32), (u128, Option<u32>)> = BTreeMap::new();
    for e in events {
        match &e.payload {
            EventPayload::AttemptStarted {
                node, attempt, pid, ..
            } => {
                open.insert((node.clone(), *attempt), (e.ts, *pid));
            }
            EventPayload::AttemptFinished { node, attempt, .. } => {
                open.remove(&(node.clone(), *attempt));
            }
            // A node that reported a verdict closes every attempt it had: the
            // fold does the same, and an `attempt_finished` can be missing on
            // paths that only journal the node-level outcome.
            EventPayload::NodeFinished { node, .. } => {
                open.retain(|(n, _), _| n != node);
            }
            _ => {}
        }
    }
    open.into_iter()
        .map(|((node, attempt), (started_ms, pid))| OpenAttempt {
            node,
            attempt,
            pid,
            started_ms,
        })
        .collect()
}

/// Per-node timings surfaced by `run_status`. Without these "is it stuck or
/// working?" is unanswerable from the API: the journal carries the timestamps,
/// but nothing exposed them.
#[derive(Debug, Clone, Serialize)]
pub struct NodeTimes {
    /// Epoch milliseconds of the node's most recent `node_started`.
    pub started_ms: u64,
    /// How long the node's currently open attempt has been running. `None`
    /// when the node has no open attempt (it finished, or never spawned one).
    pub attempt_age_ms: Option<u64>,
    /// The pid of that open attempt. `None` when there is no open attempt, or
    /// when the journal recorded none.
    pub attempt_pid: Option<u32>,
}

/// Timings for every node the journal has ever started, keyed by node id.
///
/// `attempt_age_ms` is measured against wall-clock now, so it grows between
/// two reads of a live run - which is the whole point: a caller polling
/// `run_status` can see progress without any heartbeat mechanism.
pub fn node_times(events: &[Event]) -> BTreeMap<String, NodeTimes> {
    let now = now_millis();
    let mut started: BTreeMap<String, u128> = BTreeMap::new();
    for e in events {
        if let EventPayload::NodeStarted { node, .. } = &e.payload {
            started.insert(node.clone(), e.ts);
        }
    }
    // The highest-numbered open attempt per node wins. `open_attempts` yields
    // `(node, attempt)` in ascending order, so the last insert for a node is
    // its highest attempt number. A node has one open attempt at a time in
    // practice (a retry closes the previous one), and where a journal ever
    // showed two, the higher attempt number is the later work.
    let mut latest: BTreeMap<String, &OpenAttempt> = BTreeMap::new();
    let opens = open_attempts(events);
    for a in &opens {
        latest.insert(a.node.clone(), a);
    }
    started
        .into_iter()
        .map(|(node, ts)| {
            let open = latest.get(&node);
            let times = NodeTimes {
                started_ms: clamp_ms(ts),
                attempt_age_ms: open.map(|a| clamp_ms(now.saturating_sub(a.started_ms))),
                attempt_pid: open.and_then(|a| a.pid),
            };
            (node, times)
        })
        .collect()
}

/// Epoch/duration milliseconds narrowed to the `u64` the API reports. The
/// journal stores `u128`; saturating rather than wrapping keeps a corrupt or
/// absurd timestamp from presenting as a small, plausible number.
fn clamp_ms(ms: u128) -> u64 {
    u64::try_from(ms).unwrap_or(u64::MAX)
}

/// The status string reported for a node whose attempt pid is provably gone.
/// Deliberately not a `NodeStatus` variant: the fold is pure and replayable
/// from the journal alone, while this verdict depends on the machine's process
/// table at read time and would not survive a replay.
pub const LOST: &str = "lost";

/// Nodes whose journaled attempt pid is dead while the node has not reported a
/// verdict: work that no process is doing any more, however the journal reads.
///
/// This is what the production incident needed and did not have. An attempt
/// that crashes without writing `attempt_finished` leaves the node reading as
/// in-flight forever, and the only way to tell that apart from a long-running
/// agent was `ps` and transcript forensics.
///
/// An attempt with no journaled pid is never reported lost: unknown is not
/// dead. Neither is one whose pid the probe cannot resolve.
pub fn lost_nodes(events: &[Event]) -> BTreeSet<String> {
    let state = RunState::fold(events);
    open_attempts(events)
        .into_iter()
        .filter(|a| {
            // Only nodes the journal still shows as in flight. A node that
            // reported a verdict is not lost whatever its old pids are doing.
            !state
                .nodes
                .get(&a.node)
                .copied()
                .unwrap_or(NodeStatus::Pending)
                .is_finished()
        })
        .filter(|a| a.pid.is_some_and(|pid| !pid_is_live(pid)))
        .map(|a| a.node)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ev(seq: u64, ts: u128, payload: EventPayload) -> Event {
        Event { seq, ts, payload }
    }

    fn attempt_started(node: &str, attempt: u32, pid: Option<u32>) -> EventPayload {
        EventPayload::AttemptStarted {
            node: node.into(),
            attempt,
            agent: "stub".into(),
            soul_delivery: None,
            skills_mode: None,
            pid,
        }
    }

    #[test]
    fn only_an_apb_program_counts_as_a_possible_driver() {
        assert!(argv_program_is_apb("/usr/local/bin/apb run demo"));
        assert!(argv_program_is_apb("apb mcp"));
        assert!(!argv_program_is_apb("/bin/zsh -l"));
        assert!(!argv_program_is_apb("apbx run demo"));
        assert!(!argv_program_is_apb(""));
    }

    #[test]
    fn the_current_process_is_always_a_live_driver() {
        let dir = tempfile::tempdir().unwrap();
        apb_core::fsutil::atomic_write(
            &crate::driver::driver_pid_path(dir.path()),
            std::process::id().to_string().as_bytes(),
        )
        .unwrap();
        assert!(driver_is_live(dir.path(), "any-run"));
        assert_eq!(driver_alive(dir.path(), "any-run"), Some(true));
    }

    #[test]
    fn run_id_is_matched_as_a_token_not_a_prefix() {
        assert!(driver_argv_names_run(
            "/usr/bin/apb __drive-run --root /w --run-id stopflow-12",
            "stopflow-12"
        ));
        assert!(driver_argv_names_run(
            "/usr/bin/apb __drive-run --run-id=stopflow-12 --resume",
            "stopflow-12"
        ));
        // The bug this guards: a prefix must not pass for the longer id.
        assert!(!driver_argv_names_run(
            "/usr/bin/apb __drive-run --root /w --run-id stopflow-123456",
            "stopflow-12"
        ));
        assert!(!driver_argv_names_run("/usr/bin/apb mcp", "stopflow-12"));
    }

    #[test]
    fn a_probe_that_cannot_answer_never_concludes_dead() {
        // A live pid this test certainly owns.
        assert!(matches!(
            process_probe(std::process::id()),
            Probe::Running(_)
        ));
        // `ps` answering "no such process" is the one case that means dead.
        assert!(matches!(process_probe(u32::MAX), Probe::NotFound));
    }

    #[test]
    fn the_self_probe_guard_trusts_a_working_ps() {
        // The guard's premise: on any host where these tests run, `ps` can see
        // the process asking. If this ever fails, every `NotFound` on this host
        // degrades to `Unknown` and nothing is ever declared dead - which is
        // the safe direction, and exactly what the guard is for.
        assert!(matches!(
            classify(run_ps(std::process::id())),
            Probe::Running(_)
        ));
    }

    /// Writes an executable stub that reproduces a reduced (busybox-style)
    /// `ps`: it rejects the invocation, prints its usage to stderr, and exits
    /// non-zero with an EMPTY stdout.
    ///
    /// A stub binary rather than a `PATH` shadow on purpose. These tests run
    /// as threads of a single binary, so mutating `PATH` would leak into every
    /// other test that spawns a process and make the suite flaky; passing the
    /// program path explicitly exercises the same real spawn with no global
    /// state.
    #[cfg(unix)]
    fn broken_ps_stub(dir: &Path) -> String {
        use std::os::unix::fs::PermissionsExt;
        let path = dir.join("ps");
        std::fs::write(
            &path,
            "#!/bin/sh\necho \"usage: ps [-aAdefjlmrSTvwXx]\" >&2\nexit 1\n",
        )
        .unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        path.to_string_lossy().into_owned()
    }

    #[cfg(unix)]
    #[test]
    fn a_reduced_ps_is_indistinguishable_from_no_such_process() {
        // The fragile judgment, stated as a test: on its own, one answer from a
        // broken `ps` reads exactly like "this pid is dead". Everything the
        // guard does is justified by this assertion holding.
        let dir = tempfile::tempdir().unwrap();
        let stub = broken_ps_stub(dir.path());
        assert!(matches!(
            classify(run_ps_program(&stub, std::process::id())),
            Probe::NotFound
        ));
    }

    #[cfg(unix)]
    #[test]
    fn the_self_probe_guard_downgrades_a_broken_ps_to_unknown() {
        // The guard's engaged branch. With a `ps` that answers "not found" for
        // every pid - including the live one asking - the guard must refuse to
        // believe the verdict and degrade to Unknown.
        let dir = tempfile::tempdir().unwrap();
        let stub = broken_ps_stub(dir.path());
        let run = |pid: u32| run_ps_program(&stub, pid);

        // Our own pid: certainly alive, and a broken `ps` must not call it dead.
        assert!(matches!(
            guarded_probe(std::process::id(), &run),
            Probe::Unknown
        ));
        // An arbitrary other pid gets the same protection.
        assert!(matches!(guarded_probe(4242, &run), Probe::Unknown));
    }

    #[test]
    fn an_unusable_probe_leaves_the_driver_live() {
        // The consequence that matters: Unknown must resolve to "live", so a
        // host with a reduced `ps` never finalizes a run that is still working.
        assert!(driver_verdict("any-run", Probe::Unknown));
        // A working probe still concludes dead when the process is really gone.
        assert!(!driver_verdict("any-run", Probe::NotFound));
        // And a working probe that finds another run's driver rejects it.
        assert!(!driver_verdict(
            "mine",
            Probe::Running("/usr/bin/apb __drive-run --run-id other".into())
        ));
    }

    #[test]
    fn a_healthy_probe_still_reports_a_dead_pid_dead() {
        // The guard must not blunt the real verdict on a working host: an
        // impossible pid is still NotFound through the full guarded path.
        assert!(matches!(guarded_probe(u32::MAX, &run_ps), Probe::NotFound));
    }

    #[test]
    fn the_pid_taking_rule_never_consults_the_pid_file() {
        // The fix behind `driver_pid_is_live`: a caller that has already read
        // `driver.pid` gets its verdict from the pid it holds, so a drive that
        // finishes and removes the file mid-check cannot flip a live verdict
        // into "stale pid file". Here the file does not exist at all and the
        // verdict is still driven purely by the pid.
        let dir = tempfile::tempdir().unwrap();
        assert!(driver_pid_is_live(std::process::id(), "any-run"));
        // The file-reading entry point, on the same directory, correctly says
        // there is no driver - which is what a second read would have returned.
        assert!(!driver_is_live(dir.path(), "any-run"));
    }

    #[test]
    fn a_missing_pid_file_means_no_driver() {
        let dir = tempfile::tempdir().unwrap();
        assert!(!driver_is_live(dir.path(), "any-run"));
        // Reported as "nothing claims this run", not as "the claim is false".
        assert_eq!(driver_alive(dir.path(), "any-run"), None);
    }

    #[test]
    fn our_own_pid_is_live_and_an_impossible_pid_is_not() {
        assert!(pid_is_live(std::process::id()));
        assert!(!pid_is_live(u32::MAX));
        assert!(pid_alive(std::process::id()));
    }

    /// A pid that cannot name a process must read as DEAD, through both
    /// probes, for every impossible value.
    ///
    /// This is the regression test for a bug that wedged the workdir. A pid
    /// file holding a corrupt or out-of-range value read as "held by a running
    /// process" forever, and there was no way to reclaim it. The cause is that
    /// the out-of-range values are not rejected by `kill(2)` - they are
    /// WILDCARDS. `u32::MAX` narrows to `-1`, "every process I may signal",
    /// which succeeds; `0` means "my own process group", which also succeeds.
    /// So the probe asked "may I signal everything?", got yes, and called the
    /// garbage pid alive.
    ///
    /// It was invisible on macOS, where BSD `kill(1)` rejected the argument
    /// before it ever reached the syscall, and only surfaced on Linux, where
    /// procps-ng `kill(1)` passed it through to be narrowed. Probing by
    /// syscall makes the behaviour identical on both, which is exactly why the
    /// range check has to be explicit rather than delegated to a `kill` binary.
    #[test]
    fn a_pid_that_cannot_name_a_process_is_dead_not_alive() {
        for pid in [0, u32::MAX, u32::MAX - 1, (i32::MAX as u32) + 1] {
            assert!(
                !pid_alive(pid),
                "pid {pid} cannot name a process, so pid_alive must not report it running"
            );
            assert!(
                !pid_is_live(pid),
                "pid {pid} cannot name a process, so pid_is_live must not report it running"
            );
            assert!(
                matches!(process_probe(pid), Probe::NotFound),
                "pid {pid} cannot name a process, so the probe must answer NotFound"
            );
        }
        // The largest pid that IS representable stays probeable: the rejection
        // must be of impossible values, not of large ones.
        assert!(probeable_pid(i32::MAX as u32).is_some());
        assert!(probeable_pid(1).is_some());
    }

    /// The rejection must not have broadened into "unknown means dead", which
    /// is the failure mode this whole module is biased against: a pid that is
    /// merely unreadable, or a probe that cannot answer, still resolves to
    /// alive.
    #[test]
    fn an_unanswerable_probe_still_resolves_to_alive() {
        // `Unknown` is never a "dead" verdict for the driver rule ...
        assert!(driver_verdict("any-run", Probe::Unknown));
        // ... and a real, live, foreign pid is reported alive by the raw
        // primitive even though it is not ours and carries no identity.
        //
        // Reaped through a guard rather than by a `kill` after the assertion:
        // a failing assertion unwinds straight past that line and leaks the
        // `sleep` for its full 30 seconds.
        struct Reaped(std::process::Child);
        impl Drop for Reaped {
            fn drop(&mut self) {
                let _ = self.0.kill();
                // Bounded: the child has just been SIGKILLed.
                let _ = self.0.wait();
            }
        }

        let child = Reaped(
            std::process::Command::new("sh")
                .arg("-c")
                .arg("sleep 30")
                .spawn()
                .expect("spawn a live child"),
        );
        assert!(
            pid_alive(child.0.id()),
            "a live foreign pid must read as alive"
        );
    }

    #[test]
    fn an_attempt_is_open_until_it_or_its_node_finishes() {
        let events = vec![
            ev(0, 100, attempt_started("a", 1, Some(7))),
            ev(1, 200, attempt_started("b", 1, Some(8))),
            ev(
                2,
                300,
                EventPayload::AttemptFinished {
                    node: "a".into(),
                    attempt: 1,
                    status: "succeeded".into(),
                    duration_ms: None,
                },
            ),
        ];
        let open = open_attempts(&events);
        assert_eq!(open.len(), 1);
        assert_eq!(open[0].node, "b");
        assert_eq!(open[0].pid, Some(8));
        assert_eq!(open[0].started_ms, 200);
    }

    #[test]
    fn a_node_verdict_closes_its_attempts_even_without_attempt_finished() {
        // The finish-answer path journals no `attempt_finished`; a node that
        // reported a verdict must not keep an attempt open forever.
        let events = vec![
            ev(0, 100, attempt_started("a", 1, Some(7))),
            ev(
                1,
                200,
                EventPayload::NodeFinished {
                    node: "a".into(),
                    status: "succeeded".into(),
                    attempt: 1,
                    output: String::new(),
                    artifacts: Vec::new(),
                },
            ),
        ];
        assert!(open_attempts(&events).is_empty());
    }

    #[test]
    fn node_times_carry_the_last_start_and_a_growing_age() {
        let now = now_millis();
        let events = vec![
            ev(
                0,
                1_000,
                EventPayload::NodeStarted {
                    node: "a".into(),
                    attempt: 1,
                },
            ),
            // A loop re-entry: the LAST start is the one that matters.
            ev(
                1,
                5_000,
                EventPayload::NodeStarted {
                    node: "a".into(),
                    attempt: 2,
                },
            ),
            ev(2, now.saturating_sub(50), attempt_started("a", 2, Some(9))),
        ];
        let times = node_times(&events);
        let a = times.get("a").expect("a started, so it has times");
        assert_eq!(a.started_ms, 5_000);
        assert_eq!(a.attempt_pid, Some(9));
        assert!(
            a.attempt_age_ms.is_some_and(|age| age >= 50),
            "an attempt opened 50ms ago must report at least that age"
        );
    }

    #[test]
    fn a_node_without_an_open_attempt_has_null_attempt_fields() {
        let events = vec![ev(
            0,
            1_000,
            EventPayload::NodeStarted {
                node: "a".into(),
                attempt: 1,
            },
        )];
        let a = node_times(&events).remove("a").expect("a started");
        assert_eq!(a.attempt_age_ms, None);
        assert_eq!(a.attempt_pid, None);
    }

    #[test]
    fn a_dead_attempt_pid_makes_the_node_lost() {
        let events = vec![
            ev(
                0,
                1_000,
                EventPayload::NodeStarted {
                    node: "a".into(),
                    attempt: 1,
                },
            ),
            ev(1, 2_000, attempt_started("a", 1, Some(u32::MAX))),
        ];
        assert!(lost_nodes(&events).contains("a"));
    }

    #[test]
    fn an_attempt_without_a_pid_is_never_lost() {
        // Unknown is not dead: old journals carry no pid, and reporting those
        // nodes as lost would be a false terminal verdict on every legacy run.
        let events = vec![
            ev(
                0,
                1_000,
                EventPayload::NodeStarted {
                    node: "a".into(),
                    attempt: 1,
                },
            ),
            ev(1, 2_000, attempt_started("a", 1, None)),
        ];
        assert!(lost_nodes(&events).is_empty());
    }

    #[test]
    fn a_live_attempt_pid_is_never_lost() {
        let events = vec![
            ev(
                0,
                1_000,
                EventPayload::NodeStarted {
                    node: "a".into(),
                    attempt: 1,
                },
            ),
            ev(1, 2_000, attempt_started("a", 1, Some(std::process::id()))),
        ];
        assert!(lost_nodes(&events).is_empty());
    }

    #[test]
    fn a_finished_node_is_never_lost() {
        // Its old attempt pid is certainly gone, but the node reported a
        // verdict, so there is nothing lost about it.
        let events = vec![
            ev(0, 1_000, attempt_started("a", 1, Some(u32::MAX))),
            ev(
                1,
                2_000,
                EventPayload::NodeFinished {
                    node: "a".into(),
                    status: "succeeded".into(),
                    attempt: 1,
                    output: String::new(),
                    artifacts: Vec::new(),
                },
            ),
        ];
        assert!(lost_nodes(&events).is_empty());
    }
}
