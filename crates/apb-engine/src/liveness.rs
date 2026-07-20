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

/// The raw primitive: `kill -0 <pid>` exits successfully if the process
/// exists. Cheaper than `process_probe` and enough where a pid cannot have
/// been reused (the workdir lock, held for the lifetime of one process).
/// Anything that must survive pid reuse wants `driver_is_live` instead.
pub fn pid_alive(pid: u32) -> bool {
    Command::new("kill")
        .arg("-0")
        .arg(pid.to_string())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
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

/// One `ps` invocation, interpreted. Never call this directly: `process_probe`
/// wraps it with the self-probe guard that decides whether `ps` can be
/// believed at all.
fn raw_probe(pid: u32) -> Probe {
    let out = match Command::new("ps")
        .args(["-o", "stat=", "-o", "args=", "-p", &pid.to_string()])
        .output()
    {
        Ok(out) => out,
        // `ps` is missing or could not be spawned at all.
        Err(_) => return Probe::Unknown,
    };
    let line = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if !out.status.success() {
        // The ordinary "no such process" answer: a non-zero exit with nothing
        // on stdout (the diagnostic goes to stderr). A non-zero exit that DID
        // print something is some other failure, and we do not guess.
        return if line.is_empty() {
            Probe::NotFound
        } else {
            Probe::Unknown
        };
    }
    // A successful probe that printed nothing also means no such process.
    if line.is_empty() {
        return Probe::NotFound;
    }
    let Some((stat, argv)) = line.split_once(char::is_whitespace) else {
        // Output we cannot parse: unknown, not dead.
        return Probe::Unknown;
    };
    if stat.starts_with('Z') {
        return Probe::NotFound;
    }
    Probe::Running(argv.trim().to_string())
}

/// Is the `ps` on this host one we can believe? Answered by probing a pid
/// whose liveness we are certain of: our own. A `ps` that cannot see the
/// process asking the question cannot see anything.
///
/// This exists because of a specific failure mode. A busybox (or otherwise
/// reduced) `ps` rejects the `-o stat= -o args= -p <pid>` invocation outright:
/// it prints its usage to stderr and exits non-zero with an EMPTY stdout,
/// which is bit-for-bit the shape of the ordinary "no such process" answer.
/// Without this guard such a host classifies EVERY pid as dead, and a false
/// dead is the error that writes a terminal event over live work.
fn ps_tool_is_usable() -> bool {
    matches!(raw_probe(std::process::id()), Probe::Running(_))
}

/// `raw_probe` with the self-probe guard applied.
///
/// The guard is only consulted on the `NotFound` branch, which is an
/// optimization and not a change in meaning: `Running` and `Unknown` are
/// already safe verdicts (neither concludes "dead"), so a broken `ps` cannot
/// turn either of them into a harmful answer. `NotFound` is the only verdict
/// worth paying a second `ps` to double-check, and on a healthy host that
/// second call happens only for pids that really are gone.
pub(crate) fn process_probe(pid: u32) -> Probe {
    match raw_probe(pid) {
        Probe::NotFound if !ps_tool_is_usable() => Probe::Unknown,
        other => other,
    }
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
    let Some(pid) = crate::driver::read_driver_pid(run_dir) else {
        return false;
    };
    // A drive running on a thread of THIS process (the CLI's synchronous run,
    // the in-process background drive) needs no probing and cannot be a
    // reused pid.
    if pid == std::process::id() {
        return true;
    }
    let argv = match process_probe(pid) {
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
    crate::driver::read_driver_pid(run_dir)?;
    Some(driver_is_live(run_dir, run_id))
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
    // The newest open attempt per node wins: a retried node has one open
    // attempt at a time, and if a journal ever showed two, the later one is
    // the work actually in flight.
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
        assert!(ps_tool_is_usable());
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
