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
use crate::event::{Event, EventPayload};
use crate::liveness;
use crate::state::{RunState, RunStatus};

mod check;
mod drift;
mod env;
mod exposure;
mod isolation;
mod review;

pub use check::{FAIL, OK, RunCheck, WARN};

/// Diagnoses run `run_id` under project `root`.
///
/// Returns the checks in a fixed order - run, nodes, journal drift (the
/// binaries, unknown and undecodable records, deferred retries), attempts,
/// autonomy, models, process env, tool exposure, result store, review
/// channel, driver, workdir lock, control backlog, supervisor actions, file
/// modes - so two reports of the same run are comparable line by line.
pub fn diagnose_run(root: &Path, run_id: &str) -> Result<Vec<RunCheck>, EngineError> {
    if !apb_core::registry::is_safe_segment(run_id) {
        return Err(EngineError::NotFound(format!("run `{run_id}`")));
    }
    let run_dir = root.join(".apb/runs").join(run_id);
    if !run_dir.is_dir() {
        return Err(EngineError::NotFound(format!("run `{run_id}`")));
    }
    // The drift rows, or one FAIL row when the journal is there but cannot
    // be read at all: no row then calls its format fine.
    let survey = drift::read(&run_dir);
    let drift_rows = || match &survey {
        Ok(journal) => drift::checks(journal),
        Err(e) => vec![RunCheck::new(
            FAIL,
            "journal",
            format!(
                "the journal cannot be read: {}",
                drift::shown(&e.to_string())
            ),
        )],
    };
    let view = match crate::run_view::RunView::load(&run_dir, run_id) {
        Ok(view) => view,
        // A journal no engine reader accepts is still described: the drift
        // rows name the line, and the checks that need no events still run.
        // The error quotes journal text, so it is shown escaped and cut.
        Err(e) => {
            let mut checks = vec![RunCheck::new(
                FAIL,
                "run",
                format!(
                    "the journal cannot be read: {}",
                    drift::shown(&e.to_string())
                ),
            )];
            checks.extend(drift_rows());
            checks.push(driver_check(&run_dir, run_id));
            // The run's own tree, from the lines the survey could decode.
            let tree = survey
                .as_ref()
                .ok()
                .and_then(|j| RunState::fold(&j.events).worktree);
            checks.push(workdir_lock_check(root, tree.as_deref().map(Path::new)));
            checks.push(file_modes_check(&run_dir));
            return Ok(checks);
        }
    };
    let events = view.events.as_slice();

    let mut checks = vec![run_check(&view), nodes_check(&view)];
    checks.extend(drift_rows());
    checks.extend(failure_reason_check(events));
    checks.extend(supervisor_wait_check(events));
    checks.extend(attempt_checks(events));
    checks.extend(autonomy_check(&run_dir));
    checks.extend(model_check(&run_dir, events));
    checks.extend(step_entry_check(&run_dir, events));
    checks.extend(env::env_checks(&run_dir, events));
    checks.extend(exposure::exposure_checks(events));
    checks.extend(isolation::isolation_checks(events));
    checks.extend(results::results_check(&run_dir, run_id));
    checks.extend(review::review_check(&run_dir, events));
    checks.push(driver_check(&run_dir, run_id));
    checks.push(workdir_lock_check(
        root,
        view.state.worktree.as_deref().map(Path::new),
    ));
    checks.push(control_check(&run_dir)?);
    checks.push(supervisor_action_check(events));
    checks.push(file_modes_check(&run_dir));
    Ok(checks)
}

/// A run directory whose entries other users can read (one an older apb
/// wrote): it may hold an echoed secret. Reported with the line that fixes
/// it by hand; the next drive or resume narrows it (`crate::run_perms`).
/// Reads only.
fn file_modes_check(run_dir: &Path) -> RunCheck {
    match crate::run_perms::wide(run_dir) {
        None => RunCheck::new(OK, "files", "only the owner can read the run directory"),
        Some(entry) => RunCheck::new(
            WARN,
            "files",
            format!(
                "`{}` is readable beyond its owner; the next resume narrows it, or run: {}",
                entry.display(),
                crate::run_perms::fix_line(run_dir)
            ),
        ),
    }
}

/// Issue #193: one warning per attempt that ran on another model than its
/// node's profile names (a host picks its own model; a fallback step runs
/// another), so a run whose figures look off says why. Nothing when every
/// attempt matches or no model is known.
fn model_check(run_dir: &Path, events: &[Event]) -> Vec<RunCheck> {
    crate::execution_identity::fold::views(run_dir, events)
        .iter()
        .filter(|v| v.mismatch)
        .map(|v| RunCheck::new(WARN, "model", mismatch_line(v)))
        .collect()
}

/// One row per attempt that entered a later chain step, with the reason
/// the step before closed, as the journal recorded it: `node attempt 1
/// entered step 1 fallback 1 of 2 from step 0: check_rejected`. An old
/// journal records none, so it shows no row rather than a guess.
fn step_entry_check(run_dir: &Path, events: &[Event]) -> Vec<RunCheck> {
    crate::execution_identity::fold::views(run_dir, events)
        .iter()
        .filter_map(|v| {
            let s = v.step.as_ref()?;
            let e = s.entered.as_ref()?;
            Some(RunCheck::new(
                OK,
                "step",
                format!(
                    "{} attempt {} entered step {} {} from step {}: {}",
                    v.node,
                    v.attempt,
                    s.step,
                    s.source.words(),
                    e.from_step,
                    e.reason
                ),
            ))
        })
        .collect()
}

/// `node attempt 1: requested by apb M (cli), the profile names P`, or for
/// a host attempt `node attempt 1: the host reported M, ...`: a CLI reports
/// no model of its own, so its line never claims what ran.
fn mismatch_line(v: &crate::execution_identity::fold::ExecutionIdentityView) -> String {
    use crate::execution_identity::fold::ReportedModel;
    let what = match &v.reported {
        ReportedModel::One { model } => format!("the host reported {model}"),
        _ => format!(
            "requested by apb {} ({})",
            v.requested_model.as_deref().unwrap_or("?"),
            v.executed_by
        ),
    };
    format!(
        "{} attempt {}: {what}, the profile names {}",
        v.node,
        v.attempt,
        v.expected.as_deref().unwrap_or("?")
    )
}

/// Whether any check is blocking, for the caller's exit code.
pub fn has_failure(checks: &[RunCheck]) -> bool {
    checks.iter().any(|c| c.status == FAIL)
}

/// The live-reported run status (pure fold plus process-table overlay so a
/// live open attempt never reads as interrupted - issue #45 finding 9 - and a
/// run parked on a wait/signal park with a live driver never reads as
/// interrupted either - issue #102.4 cause B). Only the two bad terminal
/// outcomes warn: a run that is still going, paused, parked and waiting, or
/// succeeded is not a problem to be reported. `interrupted` warns too - it is
/// exactly the state a crashed driver leaves.
fn run_check(view: &crate::run_view::RunView) -> RunCheck {
    let status = view.run_status;
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
fn nodes_check(view: &crate::run_view::RunView) -> RunCheck {
    let nodes = view.nodes();
    if nodes.is_empty() {
        return RunCheck::new(OK, "nodes", "no nodes have started");
    }
    let mut counts: BTreeMap<&str, usize> = BTreeMap::new();
    let mut bad = false;
    for status in nodes.values() {
        *counts.entry(status.as_str()).or_default() += 1;
        bad |=
            status == liveness::LOST || crate::state::NodeStatus::from_label(status).ended_badly();
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
                // The drive saw the exit itself (issue #107): it is finishing
                // the attempt, unless it died before it could.
                Some(pid) if a.exited => RunCheck::new(
                    WARN,
                    subject,
                    format!(
                        "pid {pid} exited and its drive is finishing the attempt, or died before it could"
                    ),
                ),
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

/// The busy lock of the tree the run works in: the project-wide workdir
/// lock, or the lock of the run's own working tree once it has one (issue #67
/// item 8). A lock held by a dead pid blocks every future write-run over that
/// tree, and nothing clears it until the next `acquire` notices - so it is
/// worth naming even though the run being diagnosed may not be the one that
/// leaked it.
fn workdir_lock_check(root: &Path, tree: Option<&Path>) -> RunCheck {
    let path = crate::workdir::tree_lock_path(root, tree);
    // A run paused with `--keep-lock` (issue #200) holds the lock with no
    // process behind it, by design.
    if let Some(crate::workdir::LockOwner::PausedRun(id)) = crate::workdir::lock_owner(&path) {
        return RunCheck::new(
            OK,
            "workdir lock",
            format!(
                "held by the paused run `{id}` (--keep-lock); its resume takes it back, its stop releases it"
            ),
        );
    }
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

mod results {
    //! The `results` row of `apb doctor --run`: leftovers of the run's result
    //! store (manifests and objects nothing names, transactions whose writer
    //! ended), from the same survey a dry-run cleanup prints. Writes nothing:
    //! resolving a transaction is the cleanup's work, never the doctor's.

    use std::path::Path;

    use super::check::{OK, RunCheck, WARN};
    use crate::result_clean::{CleanReport, survey};

    const SUBJECT: &str = "results";

    /// One row when the run has a result store; none otherwise.
    pub(super) fn results_check(run_dir: &Path, run_id: &str) -> Option<RunCheck> {
        if !run_dir.join(apb_core::result_store::RESULTS_DIR).exists() {
            return None;
        }
        Some(match survey(run_dir) {
            Ok(report) if !report.has_leftovers() => RunCheck::new(
                OK,
                SUBJECT,
                format!(
                    "{} snapshot(s) stored, nothing left over",
                    report.kept_snapshots
                ),
            ),
            Ok(report) => RunCheck::new(WARN, SUBJECT, leftovers_line(&report, run_id)),
            Err(e) => RunCheck::new(
                WARN,
                SUBJECT,
                format!("the result store cannot be surveyed: {e}"),
            ),
        })
    }

    fn leftovers_line(r: &CleanReport, run_id: &str) -> String {
        format!(
            "{} unreferenced snapshot(s), {} unreferenced object(s), {} bytes, {} transaction(s) whose writer ended; remove them with: apb results clean {run_id}",
            r.removed_snapshots.len(),
            r.removed_objects,
            r.removed_bytes,
            r.dead.len()
        )
    }
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
        crate::run_perms::narrow(&run_dir);
        (tmp, run_dir)
    }

    #[cfg(unix)]
    #[test]
    fn a_wide_run_directory_warns_with_the_fix_and_is_left_as_is() {
        use std::os::unix::fs::PermissionsExt as _;
        let (tmp, run_dir) = run_root(HEALTHY);
        // As an older apb left it: the directory and its files open to the
        // group (below a private directory nothing is reachable).
        let journal = run_dir.join("events.jsonl");
        std::fs::set_permissions(&journal, std::fs::Permissions::from_mode(0o664)).unwrap();
        std::fs::set_permissions(&run_dir, std::fs::Permissions::from_mode(0o775)).unwrap();
        let checks = diagnose_run(tmp.path(), "r1").unwrap();
        let files = find(&checks, "files");
        assert_eq!(files.status, WARN);
        assert!(files.detail.contains("chmod -R go-rwx"), "{files:?}");
        let mode = |p: &std::path::Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
        assert_eq!((mode(&run_dir), mode(&journal)), (0o775, 0o664));
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
                by: None,
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
                cloud: None,
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
                zcode_mode: None,
                process_env: None,
                tiers: Vec::new(),
                routed_tier: None,
                cascade: 0,
            }],
            node_bindings: BTreeMap::from([("a".to_string(), "project/x".to_string())]),
            connectors: Vec::new(),
            connector_grants: BTreeMap::new(),
            decisions: None,
            execution: None,
            consent: None,
            candidate_trial: false,
            candidate_skipped: None,
            task_type: None,
            secret_params: Vec::new(),
            launch: None,
            unattended: false,
            origin: None,
            external_data: false,
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

    /// A journal that cannot be read never earns an OK format row, and a
    /// decode error quoting hostile journal text reaches the report escaped.
    #[test]
    fn an_unreadable_or_hostile_journal_never_reads_fine_or_raw() {
        let tmp = tempfile::tempdir().unwrap();
        let run_dir = tmp.path().join(".apb/runs/r1");
        std::fs::create_dir_all(run_dir.join("events.jsonl")).unwrap();
        let checks = diagnose_run(tmp.path(), "r1").unwrap();
        assert!(
            !checks.iter().any(|c| c.subject == "journal format"),
            "{checks:?}"
        );
        assert_eq!(find(&checks, "journal").status, FAIL, "{checks:?}");

        std::fs::remove_dir(run_dir.join("events.jsonl")).unwrap();
        let hostile = format!(
            "{}\n{}\n",
            serde_json::json!({"seq": 0, "ts": 1, "type": "\u{1b}]52;c;Zm9v\u{7}"}),
            serde_json::json!({"seq": 1, "ts": 1, "type": "run_started", "playbook": "p", "version": "1.0.0"})
        );
        std::fs::write(run_dir.join("events.jsonl"), hostile).unwrap();
        let checks = diagnose_run(tmp.path(), "r1").unwrap();
        let run = find(&checks, "run");
        assert_eq!(run.status, FAIL, "{checks:?}");
        assert!(!run.detail.contains('\u{1b}'), "{run:?}");
        assert!(run.detail.len() < 400, "{run:?}");
    }

    /// The tree under `root` as (path, bytes) pairs, to prove a read wrote
    /// nothing.
    fn snapshot(root: &std::path::Path) -> Vec<(std::path::PathBuf, Vec<u8>)> {
        let mut out = Vec::new();
        let mut stack = vec![root.to_path_buf()];
        while let Some(dir) = stack.pop() {
            for entry in std::fs::read_dir(&dir).unwrap().flatten() {
                let path = entry.path();
                if path.is_dir() {
                    out.push((path.clone(), Vec::new()));
                    stack.push(path);
                } else {
                    out.push((path.clone(), std::fs::read(&path).unwrap()));
                }
            }
        }
        out.sort();
        out
    }

    /// The drift rows: the binaries and a downgrade, unknown records with
    /// their seqs, an undecodable known line with its line number (the
    /// report still comes out), a torn tail, and a deferral whose last
    /// checkpoint has no guard. Nothing under the project changes.
    #[test]
    fn the_drift_report_names_every_line_it_cannot_use_and_writes_nothing() {
        let v = env!("CARGO_PKG_VERSION");
        let journal = [
            r#"{"seq":0,"ts":1000,"type":"run_started","playbook":"p","version":"1.0.0"}"#
                .to_string(),
            format!(r#"{{"seq":1,"ts":1000,"type":"run_provenance","apb":"{v}"}}"#),
            r#"{"seq":2,"ts":1000,"type":"corpus_future_record"}"#.to_string(),
            r#"{"seq":3,"ts":1000,"type":"retry_deferred","node":"a","after_attempt":1,"retry":1,"step":0,"binding":"b","delay_ms":60000}"#.to_string(),
            r#"{"seq":4,"ts":1000,"type":"node_finished","node":"b","status":"success","attempt":1,"output":""}"#.to_string(),
            r#"{"seq":5,"ts":1000,"type":"run_resumed","from_node":"a"}"#.to_string(),
            r#"{"seq":6,"ts":1000,"type":"corpus_future_record"}"#.to_string(),
        ]
        .join("\n")
            + "\n";
        let (tmp, run_dir) = run_root(&journal);
        let before = snapshot(tmp.path());
        let checks = diagnose_run(tmp.path(), "r1").unwrap();
        assert_eq!(snapshot(tmp.path()), before, "the doctor wrote nothing");
        assert!(
            find(&checks, "binaries")
                .detail
                .contains(&format!("driven by apb {v}, resumed by an older apb")),
            "{checks:?}"
        );
        assert!(find(&checks, "downgrade").detail.contains("seq 5"));
        let unknown = find(&checks, "unknown records");
        assert!(
            unknown
                .detail
                .contains("`corpus_future_record` x2 (seq 2, 6)"),
            "{unknown:?}"
        );
        let deferral = find(&checks, "deferred retry");
        assert_eq!(deferral.status, WARN, "{deferral:?}");
        assert!(deferral.detail.contains("seq 4"), "{deferral:?}");

        // A known line that does not decode, then a torn tail: the engine's
        // reader refuses the journal, the report still names both.
        let mut broken = journal.clone();
        broken.push_str(r#"{"seq":7,"ts":1000,"type":"node_finished","node":7}"#);
        broken.push('\n');
        broken.push_str(r#"{"seq":8,"ts":1000,"type":"deferred_ret"#);
        std::fs::write(run_dir.join("events.jsonl"), &broken).unwrap();
        let before = snapshot(tmp.path());
        let checks = diagnose_run(tmp.path(), "r1").unwrap();
        assert_eq!(snapshot(tmp.path()), before, "the doctor wrote nothing");
        assert_eq!(find(&checks, "run").status, FAIL, "{checks:?}");
        let line = find(&checks, "journal line");
        assert!(
            line.detail.contains("line 8 (seq 7, `node_finished`)"),
            "{line:?}"
        );
        assert!(find(&checks, "torn tail").detail.contains("line 9"));
        assert!(find(&checks, "binaries").detail.contains(v));
    }
}
