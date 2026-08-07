//! Drive-entry reaping of dead open attempts (issue #71 item 4, spec
//! 2026-08-05 section 2.4).
//!
//! The shape under test is a run whose DRIVER died mid-attempt: the journal
//! ends at an `attempt_started` whose agent process is gone, and nothing ever
//! wrote the matching `attempt_finished`. Before drive-entry reaping the run
//! read as forever in-flight (`lost` at the status surface) and the stale
//! attempt was never closed; the node only ever re-ran because `plan_resume`
//! restarts interrupted work, leaving an attempt open in the journal for good.
//!
//! Every test builds that shape out of a REAL run - run once with a stub agent,
//! then rewrite the journal - so the run dir keeps its snapshot, config and
//! manifest and stays genuinely drivable. The single-branch tests cut the
//! journal back to the `attempt_started`; the fork test replaces it wholesale,
//! because two concurrent branches make the cut point a race. The dead pid is a
//! spawned-and-reaped child's (plausible but absent, per
//! `docs/TESTING-GUIDELINES.md`), never `u32::MAX`, which takes the
//! impossible-pid path in both `kill(2)` and `ps` instead.
//!
//! Against the pre-reaping engine, two of the three fail on the missing
//! `interrupted` closure; `resume_does_not_reap_an_attempt_without_a_pid` passes
//! both ways by design - it pins a boundary that must not move.

use apb_engine::event::{Event, EventPayload, read_all};
use apb_engine::scheduler::{RunOptions, resume, run};
use apb_engine::state::RunStatus;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;

use crate::common;

const PLAYBOOK: &str = r#"
schema: 1
id: reapflow
name: Reap
version: 1.0.0
defaults:
  profile: main
nodes:
  - { id: start, type: start }
  - { id: work, type: agent_task, prompt: "do" }
  - { id: done, type: finish, outcome: success }
edges:
  - { from: start, to: work }
  - { from: work, to: done }
"#;

/// A fan-out whose two agent branches meet at an implicit join: the shape where
/// a driver death leaves TWO open attempts, and the resume plan falls back to
/// the last finished node instead of restarting one interrupted node.
const FORK_PLAYBOOK: &str = r#"
schema: 1
id: reapfork
name: ReapFork
version: 1.0.0
defaults:
  profile: main
nodes:
  - { id: start, type: start }
  - { id: b, type: agent_task, prompt: "left" }
  - { id: c, type: agent_task, prompt: "right" }
  - { id: j, type: prompt, prompt: "merge" }
  - { id: done, type: finish, outcome: success }
edges:
  - { from: start, to: b }
  - { from: start, to: c }
  - { from: b, to: j }
  - { from: c, to: j }
  - { from: j, to: done }
"#;

/// The reaped shape on a node that REQUIRES a verdict: the one where the fresh
/// attempt is supposed to be told that work may already exist (spec 2026-08-05
/// section 2.2 composed with 2.4).
const VERDICT_PLAYBOOK: &str = r#"
schema: 1
id: reapverdict
name: ReapVerdict
version: 1.0.0
defaults:
  profile: main
  max_retries: 1
nodes:
  - { id: start, type: start }
  - { id: work, type: agent_task, prompt: "do", require_verdict: true }
  - { id: done, type: finish, outcome: success }
edges:
  - { from: start, to: work }
  - { from: work, to: done }
"#;

/// A real pid that is reliably absent: a child spawned, waited for and reaped,
/// so the number was genuinely valid and is now free. Deliberately not
/// `u32::MAX` - an impossible pid exercises the invalid-pid rejection rather
/// than the stale-holder property this fixture is about.
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

/// Seeds a project with `PLAYBOOK`, profile `main`, and a stub agent that
/// records each invocation by appending one byte to a tally file.
fn seed(root: &Path) -> (String, std::path::PathBuf) {
    seed_named(root, "reapflow", PLAYBOOK)
}

/// The project half of every seed below: registry, the playbook version, and
/// profile `main`. The stub agent is the caller's, so each test picks the one
/// whose observable side effect it needs.
fn seed_project(root: &Path, id: &str, yaml: &str) {
    apb_core::registry::init_project(root).unwrap();
    let vdir = root.join(".apb/playbooks").join(id).join("1.0.0");
    fs::create_dir_all(&vdir).unwrap();
    fs::write(vdir.join("playbook.yaml"), yaml).unwrap();
    fs::write(
        root.join(".apb/playbooks").join(id).join("current"),
        "1.0.0",
    )
    .unwrap();
    common::seed_main(root);
}

fn executable_stub(root: &Path, name: &str, body: &str) -> String {
    let prog = root.join(name);
    common::write_sync(&prog, body);
    let mut p = fs::metadata(&prog).unwrap().permissions();
    p.set_mode(0o755);
    fs::set_permissions(&prog, p).unwrap();
    prog.to_string_lossy().into_owned()
}

fn seed_named(root: &Path, id: &str, yaml: &str) -> (String, std::path::PathBuf) {
    seed_project(root, id, yaml);
    let tally = root.join("invocations");
    let prog = executable_stub(
        root,
        "stub.sh",
        &format!(
            "#!/bin/sh\nprintf x >> '{t}'\necho done\n",
            t = tally.display()
        ),
    );
    (prog, tally)
}

/// Seeds a project whose stub agent APPENDS every invocation's argv to `dump` and
/// records a success verdict in `$APB_STATUS_FILE`. The prompt is delivered via
/// argv (`-p <prompt> --model <model>`), so dumping `"$@"` is how a test sees what
/// an attempt was actually told; the verdict is what lets a `require_verdict` node
/// complete at all.
fn seed_dumping(root: &Path, id: &str, yaml: &str, dump: &Path) -> String {
    seed_project(root, id, yaml);
    executable_stub(
        root,
        "dumping-stub.sh",
        &format!(
            "#!/bin/sh\nprintf '%s\\n' \"$@\" >> '{d}'\n\
             if [ -n \"$APB_STATUS_FILE\" ]; then \
             printf '%s' '{{\"status\":\"success\"}}' > \"$APB_STATUS_FILE\"; fi\n\
             echo done\n",
            d = dump.display()
        ),
    )
}

fn invocations(tally: &Path) -> usize {
    fs::read(tally).map(|b| b.len()).unwrap_or(0)
}

/// Cuts the journal back to `node`'s `attempt_started` (inclusive) and rewrites
/// that event's pid: the exact on-disk shape a driver death leaves behind.
fn cut_at_open_attempt(run_dir: &Path, node: &str, pid: Option<u32>) {
    let events = read_all(run_dir).unwrap();
    let cut = events
        .iter()
        .position(
            |e| matches!(&e.payload, EventPayload::AttemptStarted { node: n, .. } if n == node),
        )
        .expect("the run journaled an attempt_started for the node");
    let mut kept: Vec<Event> = events[..=cut].to_vec();
    if let EventPayload::AttemptStarted { pid: p, .. } = &mut kept[cut].payload {
        *p = pid;
    }
    let mut buf = String::new();
    for e in &kept {
        buf.push_str(&serde_json::to_string(e).unwrap());
        buf.push('\n');
    }
    fs::write(run_dir.join("events.jsonl"), buf).unwrap();
}

/// Replaces a run's journal with `payloads`, numbering seq from 0. The rest of
/// the run dir (playbook snapshot, config, manifest) is left intact, so the run
/// is still drivable.
fn write_journal(run_dir: &Path, payloads: &[EventPayload]) {
    let mut buf = String::new();
    for (seq, p) in payloads.iter().enumerate() {
        let e = Event {
            seq: seq as u64,
            ts: 1_000 + seq as u128,
            payload: p.clone(),
        };
        buf.push_str(&serde_json::to_string(&e).unwrap());
        buf.push('\n');
    }
    fs::write(run_dir.join("events.jsonl"), buf).unwrap();
}

fn node_started(node: &str) -> EventPayload {
    EventPayload::NodeStarted {
        node: node.into(),
        attempt: 1,
    }
}

fn attempt_started(node: &str, pid: Option<u32>) -> EventPayload {
    EventPayload::AttemptStarted {
        node: node.into(),
        attempt: 1,
        agent: "claude-code".into(),
        soul_delivery: None,
        skills_mode: None,
        pid,
        spawn_ms: None,
    }
}

fn attempt_statuses(run_dir: &Path, node: &str) -> Vec<String> {
    read_all(run_dir)
        .unwrap()
        .into_iter()
        .filter_map(|e| match e.payload {
            EventPayload::AttemptFinished {
                node: n, status, ..
            } if n == node => Some(status),
            _ => None,
        })
        .collect()
}

/// Step 1: a resume over a run whose driver died mid-attempt closes the open
/// attempt as `interrupted` and re-executes the node to success, instead of
/// leaving the attempt open forever and reporting the node only as `lost`.
#[test]
fn resume_reaps_a_dead_open_attempt_and_reruns_the_node() {
    let dir = tempfile::tempdir().unwrap();
    let (prog, tally) = seed(dir.path());

    let _env = common::env_lock();
    unsafe {
        std::env::set_var("APB_AGENT_CMD", &prog);
    }
    let res = run(dir.path(), "reapflow", None, RunOptions::default()).unwrap();
    assert_eq!(res.outcome, RunStatus::Succeeded);
    assert_eq!(invocations(&tally), 1, "the first pass ran the agent once");

    let run_dir = dir.path().join(".apb/runs").join(&res.run_id);
    cut_at_open_attempt(&run_dir, "work", Some(dead_pid()));

    let again = resume(dir.path(), &res.run_id, None);
    unsafe {
        std::env::remove_var("APB_AGENT_CMD");
    }
    drop(_env);

    let again = again.unwrap();
    assert_eq!(again.outcome, RunStatus::Succeeded);
    assert_eq!(
        invocations(&tally),
        2,
        "the reaped node must re-execute through the agent"
    );
    assert_eq!(
        attempt_statuses(&run_dir, "work"),
        vec!["interrupted".to_string(), "succeeded".to_string()],
        "the dead attempt must be journaled closed as interrupted BEFORE the fresh attempt runs"
    );
    // The closure carries no partial output: the mid-work text died with the
    // process, and the journal never held it.
    let reaped_partial = read_all(&run_dir)
        .unwrap()
        .into_iter()
        .find_map(|e| match e.payload {
            EventPayload::AttemptFinished {
                status,
                partial_output,
                ..
            } if status == "interrupted" => Some(partial_output),
            _ => None,
        })
        .expect("an interrupted attempt_finished");
    assert_eq!(reaped_partial, None);
}

/// Several dead attempts at once, on a run the resume plan cannot pin to a
/// single interrupted node: BOTH branches are reaped in one entry pass and both
/// re-execute, which is what "the node re-enters scheduling" has to mean for a
/// fork. Also the composition check: the reap changes no status the frontier
/// reconstruction keys on (`interrupted` and `running` are both non-terminal),
/// so nothing has to reconstruct anything a second time.
#[test]
fn a_fork_with_two_dead_attempts_reaps_both_and_finishes() {
    let dir = tempfile::tempdir().unwrap();
    let (prog, tally) = seed_named(dir.path(), "reapfork", FORK_PLAYBOOK);

    let _env = common::env_lock();
    unsafe {
        std::env::set_var("APB_AGENT_CMD", &prog);
    }
    let res = run(dir.path(), "reapfork", None, RunOptions::default()).unwrap();
    assert_eq!(res.outcome, RunStatus::Succeeded);
    assert_eq!(invocations(&tally), 2, "both branches ran once");

    // Rewrite the journal into the two-open-attempts crash shape. Hand-built
    // rather than truncated: the two branches run concurrently, so where a real
    // journal happens to be cut is a race, and this test is about the shape, not
    // about which branch won.
    let run_dir = dir.path().join(".apb/runs").join(&res.run_id);
    write_journal(
        &run_dir,
        &[
            EventPayload::RunStarted {
                playbook: "reapfork".into(),
                version: "1.0.0".into(),
            },
            EventPayload::NodeFinished {
                node: "start".into(),
                status: "succeeded".into(),
                attempt: 1,
                output: String::new(),
                artifacts: Vec::new(),
            },
            node_started("b"),
            attempt_started("b", Some(dead_pid())),
            node_started("c"),
            attempt_started("c", Some(dead_pid())),
        ],
    );

    let again = resume(dir.path(), &res.run_id, None);
    unsafe {
        std::env::remove_var("APB_AGENT_CMD");
    }
    drop(_env);

    assert_eq!(again.unwrap().outcome, RunStatus::Succeeded);
    assert_eq!(
        invocations(&tally),
        4,
        "both reaped branches must re-execute through the agent"
    );
    assert_eq!(attempt_statuses(&run_dir, "b")[0], "interrupted");
    assert_eq!(attempt_statuses(&run_dir, "c")[0], "interrupted");
}

/// The non-reap boundary at the e2e level: an open attempt with NO journaled
/// pid cannot be proven dead, so it is left open. The run still recovers (the
/// resume plan restarts interrupted work either way), which is what makes the
/// conservative rule affordable.
#[test]
fn resume_does_not_reap_an_attempt_without_a_pid() {
    let dir = tempfile::tempdir().unwrap();
    let (prog, tally) = seed(dir.path());

    let _env = common::env_lock();
    unsafe {
        std::env::set_var("APB_AGENT_CMD", &prog);
    }
    let res = run(dir.path(), "reapflow", None, RunOptions::default()).unwrap();
    let run_dir = dir.path().join(".apb/runs").join(&res.run_id);
    cut_at_open_attempt(&run_dir, "work", None);

    let again = resume(dir.path(), &res.run_id, None);
    unsafe {
        std::env::remove_var("APB_AGENT_CMD");
    }
    drop(_env);

    assert_eq!(again.unwrap().outcome, RunStatus::Succeeded);
    assert_eq!(invocations(&tally), 2);
    assert_eq!(
        attempt_statuses(&run_dir, "work"),
        vec!["succeeded".to_string()],
        "an attempt with no pid must not be journaled interrupted: unknown is not dead"
    );
}

/// The Task 7 review's required follow-up: Task 5's interruption note is delivered
/// through `was_interrupted`, an in-memory flag local to ONE `execute_node` call,
/// so the FRESH attempt of a REAPED node - a different execution entirely - used to
/// carry no note at all. That is the worst place to lose it: the driver died, so
/// the journal preserved no partial output either (`partial_output: None` by honest
/// design), and the new agent has nothing but the prompt to tell it work may
/// already exist. The note must therefore be seeded from the journal's own
/// `interrupted` closure, which is exactly what the reap writes.
#[test]
fn a_reaped_require_verdict_node_carries_the_interruption_note_into_its_fresh_attempt() {
    let dir = tempfile::tempdir().unwrap();
    let dump = dir.path().join("argv-dump.txt");
    let prog = seed_dumping(dir.path(), "reapverdict", VERDICT_PLAYBOOK, &dump);

    let _env = common::env_lock();
    unsafe {
        std::env::set_var("APB_AGENT_CMD", &prog);
    }
    let res = run(dir.path(), "reapverdict", None, RunOptions::default()).unwrap();
    assert_eq!(res.outcome, RunStatus::Succeeded);

    let run_dir = dir.path().join(".apb/runs").join(&res.run_id);
    cut_at_open_attempt(&run_dir, "work", Some(dead_pid()));
    // Only the FRESH attempt's prompt is the subject; the first pass's would
    // otherwise be indistinguishable from it in the same file.
    fs::remove_file(&dump).unwrap();

    let again = resume(dir.path(), &res.run_id, None);
    unsafe {
        std::env::remove_var("APB_AGENT_CMD");
    }
    drop(_env);

    assert_eq!(again.unwrap().outcome, RunStatus::Succeeded);
    assert_eq!(
        attempt_statuses(&run_dir, "work")[0],
        "interrupted",
        "the premise: the reap closed the dead attempt as interrupted"
    );
    let prompt = fs::read_to_string(&dump).unwrap();
    assert!(
        prompt.contains("cut off mid-work"),
        "the fresh attempt of a reaped require_verdict node must carry the interruption note, got:\n{prompt}"
    );
    assert!(
        prompt.contains("APB_STATUS_FILE"),
        "and the status-file contract the note's closing clause refers to, got:\n{prompt}"
    );
}

/// The gate the review asked for, pinned: WITHOUT `require_verdict` the note stays
/// away. Its closing clause tells the agent to record its verdict in the status
/// file, and a plain node is never told that contract in the first place, so the
/// note would be advice about a mechanism the prompt never mentioned. Extending it
/// to plain nodes is a separate design decision needing a text split.
#[test]
fn a_reaped_plain_node_gets_no_interruption_note() {
    let dir = tempfile::tempdir().unwrap();
    let dump = dir.path().join("argv-dump.txt");
    let prog = seed_dumping(dir.path(), "reapflow", PLAYBOOK, &dump);

    let _env = common::env_lock();
    unsafe {
        std::env::set_var("APB_AGENT_CMD", &prog);
    }
    let res = run(dir.path(), "reapflow", None, RunOptions::default()).unwrap();
    assert_eq!(res.outcome, RunStatus::Succeeded);

    let run_dir = dir.path().join(".apb/runs").join(&res.run_id);
    cut_at_open_attempt(&run_dir, "work", Some(dead_pid()));
    fs::remove_file(&dump).unwrap();

    let again = resume(dir.path(), &res.run_id, None);
    unsafe {
        std::env::remove_var("APB_AGENT_CMD");
    }
    drop(_env);

    assert_eq!(again.unwrap().outcome, RunStatus::Succeeded);
    // Same premise as the test above - the reap is not gated on require_verdict -
    // so the difference in the prompt is the gate and nothing else.
    assert_eq!(
        attempt_statuses(&run_dir, "work")[0],
        "interrupted",
        "the reap closes a dead attempt whatever the node requires"
    );
    let prompt = fs::read_to_string(&dump).unwrap();
    assert!(
        !prompt.contains("cut off mid-work"),
        "a plain node must not be handed a note that closes on the status-file contract, got:\n{prompt}"
    );
    assert!(
        !prompt.contains("APB_STATUS_FILE"),
        "the premise of the gate: a plain node is never told the status-file contract, got:\n{prompt}"
    );
}

/// Three branches meeting two barriers: `a` and `b` feed the implicit join `j`,
/// while the long-running `c` runs alongside them and reaches the finish node on
/// its own. `max_parallel: 1` keeps the first pass strictly sequential, so the
/// journal cut below lands at a deterministic point: `a` and `b` fully finished,
/// `c` open.
const JOIN_RESUME_PLAYBOOK: &str = r#"
schema: 1
id: reapjoin
name: ReapJoin
version: 1.0.0
defaults:
  profile: main
  max_parallel: 1
nodes:
  - { id: start, type: start }
  - { id: a, type: agent_task, prompt: "left" }
  - { id: b, type: agent_task, prompt: "right" }
  - { id: j, type: prompt, prompt: "merge" }
  - { id: c, type: agent_task, prompt: "long" }
  - { id: done, type: finish, outcome: success }
edges:
  - { from: start, to: a }
  - { from: start, to: b }
  - { from: start, to: c }
  - { from: a, to: j }
  - { from: b, to: j }
  - { from: j, to: done }
  - { from: c, to: done }
"#;

/// A resume must not silently drop a JOIN whose every input landed BEFORE the
/// crash.
///
/// The frontier lives in the dead driver's memory, so a resume rebuilds it from
/// the journal. That reconstruction used to skip every join head, on the rationale
/// that a join re-enters scheduling through `advance_frontier` as soon as an input
/// lands - which is false in precisely the case the reconstruction exists for: here
/// `a` and `b` both delivered into `j` before the driver died, so no further input
/// will ever land and no advance will ever re-offer it. Against the pre-fix engine
/// `j` is dropped, `c` reruns, its branch reaches the finish node, and the run
/// reports SUCCESS with the barrier never executed at all.
#[test]
fn a_resume_still_executes_a_join_whose_inputs_all_landed_before_the_crash() {
    let dir = tempfile::tempdir().unwrap();
    let (prog, tally) = seed_named(dir.path(), "reapjoin", JOIN_RESUME_PLAYBOOK);

    let _env = common::env_lock();
    unsafe {
        std::env::set_var("APB_AGENT_CMD", &prog);
    }
    let res = run(dir.path(), "reapjoin", None, RunOptions::default()).unwrap();
    assert_eq!(res.outcome, RunStatus::Succeeded);
    assert_eq!(
        invocations(&tally),
        3,
        "the first pass ran all three branches"
    );

    // The crash shape: cut at `c`'s open attempt, which the sequential first pass
    // guarantees sits after both `a` and `b` finished and before `j` ever ran.
    let run_dir = dir.path().join(".apb/runs").join(&res.run_id);
    cut_at_open_attempt(&run_dir, "c", Some(dead_pid()));

    let again = resume(dir.path(), &res.run_id, None);
    unsafe {
        std::env::remove_var("APB_AGENT_CMD");
    }
    drop(_env);

    let again = again.unwrap();
    assert_eq!(again.outcome, RunStatus::Succeeded);
    assert!(
        read_all(&run_dir)
            .unwrap()
            .iter()
            .any(|e| matches!(&e.payload, EventPayload::NodeFinished { node, .. } if node == "j")),
        "the join whose inputs all landed before the crash must still execute on the resume"
    );
    assert_eq!(
        invocations(&tally),
        4,
        "only the reaped branch re-runs; the two delivered branches are not re-executed"
    );
}
