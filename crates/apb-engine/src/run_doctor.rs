//! `apb doctor --run <id>`: what is wrong with ONE run.
//!
//! The environment doctor (`apb_core::doctor`) answers "can this machine run
//! playbooks". This one answers the question an operator actually has when a
//! run has been reading `running` for twenty minutes, and which previously
//! took `ps` plus transcript forensics to answer at all.
//!
//! Strictly read-only. Every check names a fact and repairs nothing: the
//! repair verbs already exist (`apb stop`, `run_resume`), and a doctor that
//! silently mutated a run would be the last tool an operator could trust while
//! diagnosing one. Nothing here writes to the run directory.
//!
//! Every liveness verdict comes from `crate::liveness`, which is biased so
//! that any uncertainty resolves to "alive". A doctor that cried "dead" on a
//! host with a reduced `ps` would be worse than no doctor.

use std::collections::BTreeMap;
use std::path::Path;

use crate::control::{read_control_after, read_control_cursor};
use crate::error::EngineError;
use crate::event::{Event, EventPayload, read_all};
use crate::liveness;
use crate::state::{RunState, RunStatus};

/// One diagnosis line. `status` is one of `"ok"`, `"warn"`, `"fail"` - the
/// same three levels `apb_core::doctor::CheckStatus` uses, as plain strings so
/// this stays a leaf type with no cross-crate enum to keep in sync.
#[derive(Debug, Clone)]
pub struct RunCheck {
    pub status: &'static str,
    pub subject: String,
    pub detail: String,
}

pub const OK: &str = "ok";
pub const WARN: &str = "warn";
pub const FAIL: &str = "fail";

impl RunCheck {
    fn new(status: &'static str, subject: impl Into<String>, detail: impl Into<String>) -> Self {
        RunCheck {
            status,
            subject: subject.into(),
            detail: detail.into(),
        }
    }
}

/// Diagnoses run `run_id` under project `root`.
///
/// Returns the checks in a fixed order - run, nodes, attempts, autonomy,
/// driver, workdir lock, control backlog, supervisor actions - so two reports
/// of the same run are comparable line by line.
pub fn diagnose_run(root: &Path, run_id: &str) -> Result<Vec<RunCheck>, EngineError> {
    if !apb_core::registry::is_safe_segment(run_id) {
        return Err(EngineError::NotFound(format!("run `{run_id}`")));
    }
    let run_dir = root.join(".apb/runs").join(run_id);
    if !run_dir.is_dir() {
        return Err(EngineError::NotFound(format!("run `{run_id}`")));
    }
    let events = read_all(&run_dir)?;

    let mut checks = vec![run_check(&events), nodes_check(&events)];
    checks.extend(failure_reason_check(&events));
    checks.extend(supervisor_wait_check(&events));
    checks.extend(attempt_checks(&events));
    checks.extend(autonomy_check(&run_dir));
    checks.push(driver_check(&run_dir, run_id));
    checks.push(workdir_lock_check(root));
    checks.push(control_check(&run_dir)?);
    checks.push(supervisor_action_check(&events));
    Ok(checks)
}

/// Whether any check is blocking, for the caller's exit code.
pub fn has_failure(checks: &[RunCheck]) -> bool {
    checks.iter().any(|c| c.status == FAIL)
}

/// The live-reported run status (pure fold plus process-table overlay so a
/// live open attempt never reads as interrupted - issue #45 finding 9). Only
/// the two bad terminal outcomes warn: a run that is still going, paused, or
/// succeeded is not a problem to be reported. `interrupted` warns too - it is
/// exactly the state a crashed driver leaves.
fn run_check(events: &[Event]) -> RunCheck {
    let status = liveness::reported_run_status(events);
    let level = match status {
        RunStatus::Failed | RunStatus::Aborted | RunStatus::Interrupted => WARN,
        _ => OK,
    };
    RunCheck::new(level, "run", format!("folded status: {}", status.as_str()))
}

/// When a supervised run is parked on a failure/timeout wake, name the wake
/// and the available options so doctor does not read as silent "running"
/// (issue #45 finding 4).
fn supervisor_wait_check(events: &[Event]) -> Option<RunCheck> {
    let pending = crate::progress::pending_supervisor_decision(events)?;
    Some(RunCheck::new(
        WARN,
        "supervisor wait",
        format!(
            "waiting for supervisor decision after {} on node `{}`; options: {}",
            pending.trigger,
            pending.node,
            pending.options.join(", ")
        ),
    ))
}

/// The recorded reason for a run that ended `failed` (issue #42 finding 3):
/// scheduler/prepare paths now append a `RunError` event before the terminal
/// `run_finished(failed)`, so `run_check` no longer has to be the only signal
/// that something is wrong - this names WHY, blocking (`FAIL`) so a failed run
/// can no longer read as all-ok. `None` for anything other than a failed run;
/// for a failed run whose journal predates this fix (no `RunError` ever
/// appended), still surfaces as a `WARN` naming the gap rather than staying
/// silent.
fn failure_reason_check(events: &[Event]) -> Option<RunCheck> {
    let state = RunState::fold(events);
    if state.run_status != RunStatus::Failed {
        return None;
    }
    Some(match &state.failure_reason {
        Some(f) => RunCheck::new(FAIL, "failure reason", f.display()),
        None => RunCheck::new(
            WARN,
            "failure reason",
            "run failed but no explanatory event was recorded (a journal from before issue #42 finding 3 was fixed)",
        ),
    })
}

/// The live-reported node statuses, as a count per status. Warns when any node
/// ended badly, so the summary line alone tells an operator whether to read
/// further. Uses the same overlay as `run_status` (live open attempts report
/// as running, dead ones as lost) so doctor and status cannot disagree on an
/// healthy in-flight attempt (issue #45 finding 9).
fn nodes_check(events: &[Event]) -> RunCheck {
    let nodes = liveness::reported_node_statuses(events);
    if nodes.is_empty() {
        return RunCheck::new(OK, "nodes", "no nodes have started");
    }
    let mut counts: BTreeMap<&str, usize> = BTreeMap::new();
    let mut bad = false;
    for status in nodes.values() {
        *counts.entry(status.as_str()).or_default() += 1;
        bad |= matches!(
            status.as_str(),
            "failed" | "timed_out" | "interrupted" | "lost"
        );
    }
    let detail = counts
        .iter()
        .map(|(name, n)| format!("{name} {n}"))
        .collect::<Vec<_>>()
        .join(", ");
    RunCheck::new(if bad { WARN } else { OK }, "nodes", detail)
}

/// One check per attempt the journal left open, which is where a crashed run
/// shows itself: a dead pid with no `attempt_finished` is work nothing is
/// doing any more.
fn attempt_checks(events: &[Event]) -> Vec<RunCheck> {
    let open = liveness::open_attempts(events);
    if open.is_empty() {
        return vec![RunCheck::new(OK, "attempts", "no open attempts")];
    }
    open.iter()
        .map(|a| {
            let subject = format!("attempt {}#{}", a.node, a.attempt);
            match a.pid {
                // Unknown is not dead: an old journal simply cannot answer.
                None => RunCheck::new(
                    WARN,
                    subject,
                    "open with no journaled pid, so liveness cannot be checked",
                ),
                Some(pid) if liveness::pid_is_live(pid) => {
                    RunCheck::new(OK, subject, format!("open under pid {pid}, which is running"))
                }
                Some(pid) => RunCheck::new(
                    FAIL,
                    subject,
                    format!(
                        "open under pid {pid}, which is not running: the attempt died without finishing"
                    ),
                ),
            }
        })
        .collect()
}

/// An autonomous `agent_task` node bound to an agent with no known
/// non-interactive permission flag can hang forever on an approval prompt
/// nobody will answer (#85 finding 1). Reads the run's own manifest and
/// playbook SNAPSHOTS (`runs/<id>/manifest.yaml`, `runs/<id>/playbook.yaml`),
/// not live profiles or the working-copy playbook, so the verdict matches
/// what this run actually bound rather than whatever was edited since.
/// Skipped for a node with no snapshot at all (the manifest-less executor
/// path, or a run started before either snapshot existed) - nothing to read.
/// An `interactive` agent_task already has an escape valve (the question can
/// be relayed through `ask_user`), and any other node kind (e.g.
/// `human_review`) never binds a profile, so both are excluded by
/// construction rather than by an explicit check.
///
/// The WHOLE chain is inspected, not just the primary (external review,
/// finding 5): a fallback invocation runs the agent exactly the way the primary
/// does, so a fallback with no permission flag can hang on the same approval
/// prompt. Each bare link gets its own row, so the operator sees which one.
fn autonomy_check(run_dir: &Path) -> Vec<RunCheck> {
    let Some(playbook) = crate::progress::load_run_playbook(run_dir) else {
        return Vec::new();
    };
    let Ok(Some(manifest)) = crate::manifest::read(run_dir) else {
        return Vec::new();
    };
    playbook
        .nodes
        .iter()
        .filter(|node| {
            matches!(
                &node.kind,
                apb_core::schema::NodeKind::AgentTask {
                    interactive: false,
                    ..
                }
            )
        })
        .flat_map(|node| match manifest.for_node(&node.id) {
            Some(profile) => bare_links(&node.id, &profile.chain),
            None => Vec::new(),
        })
        .collect()
}

/// One WARN row per invocation in `chain` that carries no non-interactive
/// permission flag, naming its position so a fallback is not mistaken for the
/// primary binding.
fn bare_links(node_id: &str, chain: &[crate::invocation::ResolvedInvocation]) -> Vec<RunCheck> {
    chain
        .iter()
        .enumerate()
        .filter(|(_, invocation)| invocation.spec.autonomous_args.is_empty())
        .map(|(at, invocation)| {
            let bound = match at {
                0 => format!("binds agent `{}`", invocation.agent_id),
                n => format!("binds fallback agent `{}` (fallback {n})", invocation.agent_id),
            };
            RunCheck::new(
                WARN,
                "autonomy",
                format!(
                    "node `{node_id}` {bound}, which has no known non-interactive permission flag; an autonomous run can hang waiting for an approval prompt nobody will answer. Consider a stdout-only reviewer profile (prompt the agent to report its findings to stdout with no write access) or a human_review gate before this node."
                ),
            )
        })
        .collect()
}

/// Drive claim against the process table. A stale `driver.pid` is the single
/// reason `stop_run` refuses to finalize a run whose driver is gone, so it is
/// worth a blocking verdict rather than a warning. Parent-driven children
/// (issue #45 finding 10) follow the parent's drive via `driven_by` /
/// `parent_run` and must not FAIL while the parent is healthy.
fn driver_check(run_dir: &Path, run_id: &str) -> RunCheck {
    const NO_DRIVE: &str = "no driver.pid, so no drive is in progress";
    let parent_id = || {
        crate::driver::read_driven_by(run_dir).or_else(|| {
            crate::run_config::read_run_config(run_dir)
                .ok()
                .and_then(|c| c.parent_run)
        })
    };
    let own_pid_live = || {
        crate::driver::read_driver_pid(run_dir)
            .is_some_and(|pid| liveness::driver_pid_is_live(pid, run_id))
    };

    match liveness::driver_alive(run_dir, run_id) {
        Some(true) => {
            // Nested in-process drive (no dedicated live claim of our own).
            if let Some(parent) = parent_id()
                && !own_pid_live()
            {
                return RunCheck::new(
                    OK,
                    "driver",
                    format!("driven in-process by parent run `{parent}`, which is live"),
                );
            }
            if let Some(pid) = crate::driver::read_driver_pid(run_dir) {
                return RunCheck::new(
                    OK,
                    "driver",
                    format!("driver.pid names pid {pid}, which is driving this run"),
                );
            }
            RunCheck::new(OK, "driver", "drive claim is live")
        }
        Some(false) => {
            if let Some(parent) = parent_id() {
                return RunCheck::new(
                    FAIL,
                    "driver",
                    format!("parent-driven child of `{parent}`, but the parent drive is not live"),
                );
            }
            let Some(pid) = crate::driver::read_driver_pid(run_dir) else {
                return RunCheck::new(OK, "driver", NO_DRIVE);
            };
            // Confirm the file still holds the same pid before FAIL (same race
            // guard as before: a clean finish mid-check must not read as wedged).
            match crate::driver::read_driver_pid(run_dir) {
                None => RunCheck::new(
                    OK,
                    "driver",
                    format!("{NO_DRIVE} (pid {pid} released it while this check ran)"),
                ),
                Some(now) if now != pid => RunCheck::new(
                    OK,
                    "driver",
                    format!("driver.pid moved from pid {pid} to pid {now} while this check ran"),
                ),
                Some(_) => RunCheck::new(
                    FAIL,
                    "driver",
                    format!(
                        "driver.pid names pid {pid}, which is not driving this run: stale pid file"
                    ),
                ),
            }
        }
        None => RunCheck::new(OK, "driver", NO_DRIVE),
    }
}

/// The project-wide workdir lock. A lock held by a dead pid blocks every
/// future write-run in this project, and nothing clears it until the next
/// `acquire` notices - so it is worth naming even though the run being
/// diagnosed may not be the one that leaked it.
fn workdir_lock_check(root: &Path) -> RunCheck {
    let path = crate::workdir::lock_path(root);
    let Some(pid) = crate::workdir::lock_holder(&path) else {
        return RunCheck::new(OK, "workdir lock", "not held");
    };
    // `pid_alive` (kill -0), NOT the ps-based `pid_is_live`, because
    // `workdir::acquire` decides with `pid_alive` and this check must report
    // what the code that ACTS on the lock will conclude. The two disagree for
    // a pid owned by another user: `kill -0` fails with EPERM and reads as
    // dead, so `acquire` steals the lock while `ps` can still see the holder.
    // A doctor that said "held by a running process" where the next write-run
    // will say "stale, taking it" would be describing a different system.
    if liveness::pid_alive(pid) {
        RunCheck::new(
            OK,
            "workdir lock",
            format!("held by pid {pid}, which is running"),
        )
    } else {
        RunCheck::new(
            WARN,
            "workdir lock",
            format!("held by pid {pid}, which is not running: stale lock"),
        )
    }
}

/// Control entries posted past the persisted cursor: commands an operator or
/// supervisor issued that no drive loop has consumed. On a live run these
/// clear within a poll interval; on a dead one they are the pending work that
/// will never happen, and they explain why a stop or retry "did nothing".
fn control_check(run_dir: &Path) -> Result<RunCheck, EngineError> {
    let cursor = read_control_cursor(run_dir)?;
    let pending = read_control_after(run_dir, cursor)?;
    let total = read_control_after(run_dir, None)?.len();
    let at = match cursor {
        Some(seq) => format!("cursor {seq}"),
        None => "no cursor (nothing applied yet)".to_string(),
    };
    if pending.is_empty() {
        return Ok(RunCheck::new(
            OK,
            "control",
            format!("{total} entries posted, all applied ({at})"),
        ));
    }
    let names = pending
        .iter()
        .map(|e| format!("{}#{}", control_name(&e.cmd), e.seq))
        .collect::<Vec<_>>()
        .join(", ");
    Ok(RunCheck::new(
        WARN,
        "control",
        format!(
            "{} of {total} entries posted but not applied, past {at}: {names}",
            pending.len()
        ),
    ))
}

/// Machine-facing name of a control command, for the doctor line. Delegates to
/// `Control::kind` so the doctor's labels never drift from the drive loop's.
fn control_name(cmd: &crate::control::Control) -> &'static str {
    cmd.kind()
}

/// Repeated supervisor actions on the same node. One retry is a supervisor
/// doing its job; the same action recorded again and again is a supervisor
/// looping against a node that never changes state, which is what burns a run
/// down while every individual event looks reasonable.
fn supervisor_action_check(events: &[Event]) -> RunCheck {
    let mut counts: BTreeMap<(String, String), usize> = BTreeMap::new();
    for e in events {
        if let EventPayload::SupervisorAction { action, node, .. } = &e.payload {
            let target = node.clone().unwrap_or_else(|| "run".to_string());
            *counts.entry((action.clone(), target)).or_default() += 1;
        }
    }
    let dupes: Vec<String> = counts
        .iter()
        .filter(|(_, n)| **n > 1)
        .map(|((action, target), n)| format!("{action} on {target} x{n}"))
        .collect();
    if dupes.is_empty() {
        return RunCheck::new(
            OK,
            "supervisor actions",
            format!("{} recorded, none repeated", counts.values().sum::<usize>()),
        );
    }
    RunCheck::new(
        WARN,
        "supervisor actions",
        format!("repeated actions: {}", dupes.join(", ")),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::control::{Control, post_control, write_control_cursor};

    /// A real pid that is reliably absent: a child we spawned, waited for and
    /// reaped, so the number was genuinely valid and is now free.
    ///
    /// Deliberately NOT `u32::MAX`, which these fixtures used to use. An
    /// impossible pid does not exercise the stale-holder property these tests
    /// are about; it exercises the invalid-pid rejection, which is now tested
    /// directly in `liveness`. Worse, it hid a real bug for as long as it was
    /// used here: `u32::MAX` narrows to `-1`, which `kill(2)` reads as the
    /// "every process I may signal" wildcard, so the probe answered "alive"
    /// and this fixture passed on macOS while failing on Linux.
    ///
    /// The pid could in principle be recycled between the reap and the
    /// assertion. On an idle host within a few milliseconds that is
    /// vanishingly unlikely, and unlike an impossible pid it exercises the
    /// probe path an operator's stale lock file actually takes.
    fn dead_pid() -> u32 {
        let mut child = std::process::Command::new("sh")
            .arg("-c")
            .arg("exit 0")
            .spawn()
            .expect("spawn a throwaway child to borrow a pid from");
        let pid = child.id();
        // Bounded by construction: `exit 0` cannot fail to exit.
        child.wait().expect("reap the throwaway child");
        pid
    }

    /// A run directory under a project root, with a hand-built journal. The
    /// interesting states (a dead pid, a stale lock) cannot be produced by a
    /// real run on demand, so every fixture here is written by hand.
    fn run_root(journal: &str) -> (tempfile::TempDir, std::path::PathBuf) {
        let tmp = tempfile::tempdir().unwrap();
        let run_dir = tmp.path().join(".apb/runs/r1");
        std::fs::create_dir_all(&run_dir).unwrap();
        std::fs::write(run_dir.join("events.jsonl"), journal).unwrap();
        (tmp, run_dir)
    }

    fn find<'a>(checks: &'a [RunCheck], subject: &str) -> &'a RunCheck {
        checks
            .iter()
            .find(|c| c.subject == subject)
            .unwrap_or_else(|| panic!("no `{subject}` check in {checks:?}"))
    }

    const HEALTHY: &str = concat!(
        r#"{"seq":0,"ts":1000,"type":"run_started","playbook":"p","version":"1.0.0"}"#,
        "\n",
        r#"{"seq":1,"ts":2000,"type":"node_started","node":"a","attempt":1}"#,
        "\n",
        r#"{"seq":2,"ts":3000,"type":"node_finished","node":"a","status":"succeeded","attempt":1,"output":""}"#,
        "\n",
        r#"{"seq":3,"ts":4000,"type":"run_finished","outcome":"succeeded"}"#,
        "\n",
    );

    #[test]
    fn a_healthy_completed_run_is_all_ok() {
        let (tmp, _) = run_root(HEALTHY);
        let checks = diagnose_run(tmp.path(), "r1").unwrap();
        assert!(
            checks.iter().all(|c| c.status == OK),
            "expected every check ok, got {checks:?}"
        );
        assert!(!has_failure(&checks));
    }

    /// A join write-off is engine bookkeeping, not a supervisor acting: two of
    /// them for one join (a resume re-advance, or a loop re-entering an either-or
    /// fork) must not read as a looping supervisor. This pins the reason it is its
    /// own event variant rather than a `SupervisorAction`.
    #[test]
    fn repeated_join_write_offs_are_not_a_repeated_supervisor_action() {
        let journal = concat!(
            r#"{"seq":0,"ts":1000,"type":"run_started","playbook":"p","version":"1.0.0"}"#,
            "\n",
            r#"{"seq":1,"ts":2000,"type":"join_input_dead","node":"m","sources":["b"]}"#,
            "\n",
            r#"{"seq":2,"ts":3000,"type":"join_input_dead","node":"m","sources":["b"]}"#,
            "\n",
            r#"{"seq":3,"ts":4000,"type":"run_finished","outcome":"succeeded"}"#,
            "\n",
        );
        let (tmp, _) = run_root(journal);
        let checks = diagnose_run(tmp.path(), "r1").unwrap();
        assert_eq!(
            find(&checks, "supervisor actions").status,
            OK,
            "an engine write-off must not count as a supervisor action: {checks:?}"
        );
    }

    #[test]
    fn an_unknown_run_is_an_error() {
        let tmp = tempfile::tempdir().unwrap();
        assert!(matches!(
            diagnose_run(tmp.path(), "ghost"),
            Err(EngineError::NotFound(_))
        ));
        // A traversal attempt is rejected before it can touch the filesystem.
        assert!(matches!(
            diagnose_run(tmp.path(), "../etc"),
            Err(EngineError::NotFound(_))
        ));
    }

    #[test]
    fn a_dead_attempt_pid_fails_the_attempt_check() {
        let dead = dead_pid();
        let journal = format!(
            concat!(
                r#"{{"seq":0,"ts":1000,"type":"run_started","playbook":"p","version":"1.0.0"}}"#,
                "\n",
                r#"{{"seq":1,"ts":2000,"type":"node_started","node":"a","attempt":1}}"#,
                "\n",
                r#"{{"seq":2,"ts":3000,"type":"attempt_started","node":"a","attempt":1,"agent":"stub","pid":{pid}}}"#,
                "\n",
            ),
            pid = dead
        );
        let (tmp, _) = run_root(&journal);
        let checks = diagnose_run(tmp.path(), "r1").unwrap();
        let c = find(&checks, "attempt a#1");
        assert_eq!(c.status, FAIL, "{c:?}");
        assert!(c.detail.contains(&dead.to_string()));
        assert!(has_failure(&checks));
    }

    #[test]
    fn a_stale_driver_pid_fails_and_a_live_one_does_not() {
        let (tmp, run_dir) = run_root(HEALTHY);
        std::fs::write(run_dir.join("driver.pid"), dead_pid().to_string()).unwrap();
        let checks = diagnose_run(tmp.path(), "r1").unwrap();
        assert_eq!(find(&checks, "driver").status, FAIL);

        // Our own pid is by definition a live driver.
        std::fs::write(run_dir.join("driver.pid"), std::process::id().to_string()).unwrap();
        let checks = diagnose_run(tmp.path(), "r1").unwrap();
        assert_eq!(find(&checks, "driver").status, OK);
    }

    #[test]
    fn a_stale_workdir_lock_warns() {
        let (tmp, _) = run_root(HEALTHY);
        std::fs::create_dir_all(tmp.path().join(".apb")).unwrap();
        std::fs::write(tmp.path().join(".apb/workdir.lock"), dead_pid().to_string()).unwrap();
        let checks = diagnose_run(tmp.path(), "r1").unwrap();
        let c = find(&checks, "workdir lock");
        assert_eq!(c.status, WARN, "{c:?}");
    }

    #[test]
    fn control_entries_past_the_cursor_warn_and_applied_ones_do_not() {
        let (tmp, run_dir) = run_root(HEALTHY);
        let seq = post_control(
            &run_dir,
            Control::Abort {
                reason: "stop requested".into(),
            },
        )
        .unwrap();

        // Posted, no cursor: the command is pending and nothing will apply it.
        let checks = diagnose_run(tmp.path(), "r1").unwrap();
        let c = find(&checks, "control");
        assert_eq!(c.status, WARN, "{c:?}");
        assert!(c.detail.contains("abort"), "{c:?}");

        // Once the cursor catches up, the backlog is gone.
        write_control_cursor(&run_dir, seq).unwrap();
        let checks = diagnose_run(tmp.path(), "r1").unwrap();
        assert_eq!(find(&checks, "control").status, OK);
    }

    #[test]
    fn repeated_supervisor_actions_warn() {
        let journal = concat!(
            r#"{"seq":0,"ts":1000,"type":"run_started","playbook":"p","version":"1.0.0"}"#,
            "\n",
            r#"{"seq":1,"ts":2000,"type":"supervisor_action","action":"retry","node":"a","detail":""}"#,
            "\n",
            r#"{"seq":2,"ts":3000,"type":"supervisor_action","action":"retry","node":"a","detail":""}"#,
            "\n",
            r#"{"seq":3,"ts":4000,"type":"supervisor_action","action":"pause","node":null,"detail":""}"#,
            "\n",
        );
        let (tmp, _) = run_root(journal);
        let checks = diagnose_run(tmp.path(), "r1").unwrap();
        let c = find(&checks, "supervisor actions");
        assert_eq!(c.status, WARN, "{c:?}");
        assert!(c.detail.contains("retry on a x2"), "{c:?}");
        // A single action of another kind is not a repeat.
        assert!(!c.detail.contains("pause"), "{c:?}");
    }

    /// Issue #42 finding 3: a failed run must stop reading as all-ok. Before
    /// this fix `run_check` alone gave a failed run a `WARN` (not `FAIL`)
    /// verdict with no reason attached, so `has_failure` (the CLI's exit
    /// code) stayed clean for a run an operator very much needed to see.
    #[test]
    fn a_failed_run_with_a_recorded_reason_fails_doctor_and_names_it() {
        let journal = concat!(
            r#"{"seq":0,"ts":1000,"type":"run_started","playbook":"p","version":"1.0.0"}"#,
            "\n",
            r#"{"seq":1,"ts":2000,"type":"node_started","node":"a","attempt":1}"#,
            "\n",
            r#"{"seq":2,"ts":3000,"type":"node_finished","node":"a","status":"failed","attempt":1,"output":"boom"}"#,
            "\n",
            r#"{"seq":3,"ts":3500,"type":"run_error","node":"a","reason":"node `a` has no outgoing edge and is not finish"}"#,
            "\n",
            r#"{"seq":4,"ts":4000,"type":"run_finished","outcome":"failed"}"#,
            "\n",
        );
        let (tmp, _) = run_root(journal);
        let checks = diagnose_run(tmp.path(), "r1").unwrap();
        assert!(
            has_failure(&checks),
            "a failed run with a known reason must block, not read all-ok: {checks:?}"
        );
        let c = find(&checks, "failure reason");
        assert_eq!(c.status, FAIL, "{c:?}");
        assert!(c.detail.contains("no outgoing edge"), "{c:?}");
        assert!(c.detail.contains("node `a`"), "{c:?}");
    }

    /// A failed run whose journal predates this fix (no `run_error` was ever
    /// appended) still gets a `failure reason` row - a visible gap, not
    /// silence - rather than just vanishing back into "all ok" for lack of a
    /// FAIL-level check.
    #[test]
    fn a_failed_run_with_no_recorded_reason_still_gets_a_warn_row() {
        let journal = concat!(
            r#"{"seq":0,"ts":1000,"type":"run_started","playbook":"p","version":"1.0.0"}"#,
            "\n",
            r#"{"seq":1,"ts":2000,"type":"run_finished","outcome":"failed"}"#,
            "\n",
        );
        let (tmp, _) = run_root(journal);
        let checks = diagnose_run(tmp.path(), "r1").unwrap();
        let c = find(&checks, "failure reason");
        assert_eq!(c.status, WARN, "{c:?}");
    }

    /// The check is entirely absent for a run that is not `failed` - it must
    /// not clutter a healthy or a still-running report.
    #[test]
    fn a_healthy_run_has_no_failure_reason_row() {
        let (tmp, _) = run_root(HEALTHY);
        let checks = diagnose_run(tmp.path(), "r1").unwrap();
        assert!(
            !checks.iter().any(|c| c.subject == "failure reason"),
            "{checks:?}"
        );
    }

    /// Issue #45 finding 4: a supervised failure wake must surface as a
    /// dedicated doctor line, not a silent "folded status: running".
    #[test]
    fn supervisor_wait_after_node_failed_is_named() {
        let journal = concat!(
            r#"{"seq":0,"ts":1000,"type":"run_started","playbook":"p","version":"1.0.0"}"#,
            "\n",
            r#"{"seq":1,"ts":2000,"type":"node_started","node":"work","attempt":1}"#,
            "\n",
            r#"{"seq":2,"ts":3000,"type":"node_finished","node":"work","status":"failed","attempt":1,"output":"boom"}"#,
            "\n",
            r#"{"seq":3,"ts":4000,"type":"wake_raised","trigger":"node_failed","node":"work","detail":"boom"}"#,
            "\n",
        );
        let (tmp, _) = run_root(journal);
        let checks = diagnose_run(tmp.path(), "r1").unwrap();
        let c = find(&checks, "supervisor wait");
        assert_eq!(c.status, WARN, "{c:?}");
        assert!(c.detail.contains("work"), "{c:?}");
        assert!(c.detail.contains("retry"), "{c:?}");
        assert!(c.detail.contains("continue_from"), "{c:?}");
        assert!(c.detail.contains("abort"), "{c:?}");
        // Run status itself stays running (the drive is parked, not crashed).
        assert_eq!(find(&checks, "run").detail, "folded status: running");
    }

    /// Issue #45 finding 9: a live open attempt must not make doctor report
    /// folded status interrupted while the attempt line says running.
    #[test]
    fn live_open_attempt_does_not_report_interrupted_run() {
        let live = std::process::id();
        let journal = format!(
            concat!(
                r#"{{"seq":0,"ts":1000,"type":"run_started","playbook":"p","version":"1.0.0"}}"#,
                "\n",
                r#"{{"seq":1,"ts":2000,"type":"node_started","node":"a","attempt":1}}"#,
                "\n",
                r#"{{"seq":2,"ts":3000,"type":"attempt_started","node":"a","attempt":1,"agent":"stub","pid":{pid}}}"#,
                "\n",
            ),
            pid = live
        );
        let (tmp, _) = run_root(&journal);
        let checks = diagnose_run(tmp.path(), "r1").unwrap();
        assert_eq!(
            find(&checks, "run").detail,
            "folded status: running",
            "{checks:?}"
        );
        assert!(
            find(&checks, "nodes").detail.contains("running"),
            "{checks:?}"
        );
        let attempt = find(&checks, "attempt a#1");
        assert_eq!(attempt.status, OK, "{attempt:?}");
    }

    /// A run snapshot (`playbook.yaml` + `manifest.yaml`) binding a single
    /// `agent_task` node `a` to `agent_id`, via a fresh builtin
    /// `ResolvedInvocation` for that agent - so the fixture always reflects
    /// `invocation::builtin`'s current `autonomous_args`, not a hand-copied
    /// value that could drift from it.
    fn write_autonomy_fixture(run_dir: &Path, agent_id: &str) {
        write_autonomy_chain_fixture(run_dir, &[agent_id]);
    }

    /// The same fixture with the node's binding carrying a whole fallback
    /// chain, in order: `chain[0]` is the primary.
    fn write_autonomy_chain_fixture(run_dir: &Path, chain: &[&str]) {
        std::fs::write(
            run_dir.join("playbook.yaml"),
            "schema: 2\nid: p\nname: p\nversion: 1.0.0\ndefaults: { profile: x }\nnodes:\n  - { id: s, type: start }\n  - { id: a, type: agent_task, prompt: hi }\n  - { id: f, type: finish, outcome: success }\nedges:\n  - { from: s, to: a }\n  - { from: a, to: f }\n",
        )
        .unwrap();
        let chain: Vec<crate::invocation::ResolvedInvocation> = chain
            .iter()
            .map(|agent_id| crate::invocation::ResolvedInvocation {
                agent_id: (*agent_id).to_string(),
                model: "m".into(),
                spec: crate::invocation::builtin(agent_id).expect("builtin agent"),
                soul_delivery: apb_core::config::SoulDelivery::Prefix,
                canonical_executable: std::path::PathBuf::from("/bin/true"),
                executable_fingerprint: "0:0".into(),
            })
            .collect();
        let manifest = crate::manifest::RunExecutionManifest {
            profiles: vec![crate::manifest::ManifestProfile {
                scope: "project".into(),
                name: "x".into(),
                profile_digest: "sha256:a".into(),
                bundle_digest: "sha256:b".into(),
                soul: "role".into(),
                soul_requirement: apb_core::profile::SoulRequirement::Any,
                skills: Vec::new(),
                chain,
                ephemeral: false,
                hermetic: false,
            }],
            node_bindings: BTreeMap::from([("a".to_string(), "project/x".to_string())]),
            connectors: Vec::new(),
            connector_grants: BTreeMap::new(),
        };
        crate::manifest::write(run_dir, &manifest).unwrap();
    }

    /// #85 finding 1: hermes carries no verified non-interactive flag, so a
    /// node bound to it must warn rather than read as silently fine.
    #[test]
    fn a_node_bound_to_an_agent_with_no_autonomous_flag_warns_autonomy() {
        let (tmp, run_dir) = run_root(HEALTHY);
        write_autonomy_fixture(&run_dir, "hermes");
        let checks = diagnose_run(tmp.path(), "r1").unwrap();
        let c = find(&checks, "autonomy");
        assert_eq!(c.status, WARN, "{c:?}");
        assert!(c.detail.contains("node `a`"), "{c:?}");
        assert!(c.detail.contains("agent `hermes`"), "{c:?}");
        assert!(c.detail.contains("stdout-only reviewer profile"), "{c:?}");
        assert!(c.detail.contains("human_review"), "{c:?}");
    }

    /// External review, finding 5: the check used to read `chain.first()` only,
    /// so a node whose PRIMARY carries a permission flag but whose FALLBACK does
    /// not passed silently - and the fallback hangs on an approval prompt the
    /// same way. The row must name the fallback, not the primary.
    #[test]
    fn a_fallback_with_no_autonomous_flag_warns_autonomy() {
        let (tmp, run_dir) = run_root(HEALTHY);
        write_autonomy_chain_fixture(&run_dir, &["claude", "hermes"]);
        let checks = diagnose_run(tmp.path(), "r1").unwrap();
        let warnings: Vec<&RunCheck> = checks
            .iter()
            .filter(|c| c.subject == "autonomy" && c.status == WARN)
            .collect();
        assert_eq!(
            warnings.len(),
            1,
            "only the bare fallback warns, not the flagged primary: {checks:?}"
        );
        let c = warnings[0];
        assert!(c.detail.contains("node `a`"), "{c:?}");
        assert!(
            c.detail.contains("fallback agent `hermes` (fallback 1)"),
            "the row must name the fallback and its position: {c:?}"
        );
        assert!(
            !c.detail.contains("`claude`"),
            "the flagged primary must not be blamed: {c:?}"
        );
    }

    /// claude carries a verified `bypassPermissions` flag: no warning row.
    #[test]
    fn a_node_bound_to_claude_does_not_warn_autonomy() {
        let (tmp, run_dir) = run_root(HEALTHY);
        write_autonomy_fixture(&run_dir, "claude");
        let checks = diagnose_run(tmp.path(), "r1").unwrap();
        assert!(
            !checks
                .iter()
                .any(|c| c.subject == "autonomy" && c.status == WARN),
            "{checks:?}"
        );
    }

    /// Issue #45 finding 10: a parent-driven child with a live parent must
    /// not FAIL on driver (no stale pid file).
    #[test]
    fn parent_driven_child_with_live_parent_is_ok() {
        let tmp = tempfile::tempdir().unwrap();
        let runs = tmp.path().join(".apb/runs");
        let parent_dir = runs.join("parent-1");
        let child_dir = runs.join("child-1");
        std::fs::create_dir_all(&parent_dir).unwrap();
        std::fs::create_dir_all(&child_dir).unwrap();
        std::fs::write(parent_dir.join("events.jsonl"), HEALTHY).unwrap();
        std::fs::write(
            child_dir.join("events.jsonl"),
            concat!(
                r#"{"seq":0,"ts":1000,"type":"run_started","playbook":"c","version":"1.0.0"}"#,
                "\n",
            ),
        )
        .unwrap();
        std::fs::write(
            parent_dir.join("driver.pid"),
            std::process::id().to_string(),
        )
        .unwrap();
        crate::run_config::write_run_config(
            &child_dir,
            &crate::run_config::RunConfig {
                parent_run: Some("parent-1".into()),
                ..Default::default()
            },
        )
        .unwrap();
        apb_core::fsutil::atomic_write(&crate::driver::driven_by_path(&child_dir), b"parent-1")
            .unwrap();

        let checks = diagnose_run(tmp.path(), "child-1").unwrap();
        let c = find(&checks, "driver");
        assert_eq!(c.status, OK, "{c:?}");
        assert!(
            c.detail.contains("parent-1") || c.detail.contains("parent"),
            "{c:?}"
        );
        assert!(!has_failure(&checks), "{checks:?}");
    }
}
