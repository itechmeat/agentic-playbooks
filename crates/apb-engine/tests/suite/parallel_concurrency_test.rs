use std::fs;
use std::path::Path;
use std::time::Instant;

use apb_core::registry::init_project;
use apb_engine::event::{Event, EventPayload, read_all};
use apb_engine::scheduler::{RunMode, RunOptions, run};
use apb_engine::state::RunStatus;

// Two script branches of ~0.4s each, converging in join:all. If the branches run
// concurrently, the wall-clock time is noticeably less than the sum (0.8s).
const PLAYBOOK: &str = r#"
schema: 1
id: conc
name: Conc
version: 1.0.0
nodes:
  - { id: start, type: start }
  - { id: a, type: script, script: "scripts/slow.sh", runner: sh }
  - { id: b, type: script, script: "scripts/slow.sh", runner: sh }
  - { id: j, type: prompt, prompt: "joined" }
  - { id: done, type: finish, outcome: success }
edges:
  - { from: start, to: a }
  - { from: start, to: b }
  - { from: a, to: j, join: all }
  - { from: b, to: j, join: all }
  - { from: j, to: done }
"#;

fn seed(root: &Path) {
    init_project(root).unwrap();
    let dir = root.join(".apb/playbooks/conc/1.0.0");
    let scripts = dir.join("scripts");
    fs::create_dir_all(&scripts).unwrap();
    fs::write(dir.join("playbook.yaml"), PLAYBOOK).unwrap();
    fs::write(root.join(".apb/playbooks/conc/current"), "1.0.0").unwrap();
    fs::write(scripts.join("slow.sh"), "sleep 1.5\n").unwrap();
}

// The same diamond, but each branch PROVES the overlap instead of being timed:
// every branch marks itself started and then waits (bounded) for the sibling's
// marker. Concurrent execution makes both waits return immediately; a serialized
// run makes the first branch give up and leave a `no_overlap_*` file behind,
// which is what the assertions read. Written for supervised mode, where the
// concurrent batch used to be gated off entirely.
const RENDEZVOUS: &str = r#"
schema: 1
id: supconc
name: Supervised conc
version: 1.0.0
nodes:
  - { id: start, type: start }
  - { id: a, type: script, script: "scripts/rv_a.sh", runner: sh }
  - { id: b, type: script, script: "scripts/rv_b.sh", runner: sh }
  - { id: j, type: prompt, prompt: "joined" }
  - { id: done, type: finish, outcome: success }
edges:
  - { from: start, to: a }
  - { from: start, to: b }
  - { from: a, to: j, join: all }
  - { from: b, to: j, join: all }
  - { from: j, to: done }
"#;

/// A branch script that publishes its own start marker and then waits for the
/// sibling's. The wait is bounded (100 x 50ms) and, on expiry, records WHY it
/// gave up in `no_overlap_<mine>` and exits 0: a failing script node in a
/// supervised run would park the drive for a supervisor that no test can be
/// asked to impersonate, so the evidence travels in a file instead of an exit
/// code.
fn rendezvous_script(root: &Path, mine: &str, theirs: &str) -> String {
    format!(
        "touch '{root}/started_{mine}'\n\
         i=0\n\
         while [ ! -f '{root}/started_{theirs}' ]; do\n\
         \x20 i=$((i + 1))\n\
         \x20 if [ \"$i\" -gt 100 ]; then\n\
         \x20   echo 'branch {theirs} never started while {mine} was running' > '{root}/no_overlap_{mine}'\n\
         \x20   exit 0\n\
         \x20 fi\n\
         \x20 sleep 0.05\n\
         done\n",
        root = root.display(),
    )
}

fn seed_rendezvous(root: &Path) {
    init_project(root).unwrap();
    let dir = root.join(".apb/playbooks/supconc/1.0.0");
    let scripts = dir.join("scripts");
    fs::create_dir_all(&scripts).unwrap();
    fs::write(dir.join("playbook.yaml"), RENDEZVOUS).unwrap();
    fs::write(root.join(".apb/playbooks/supconc/current"), "1.0.0").unwrap();
    fs::write(scripts.join("rv_a.sh"), rendezvous_script(root, "a", "b")).unwrap();
    fs::write(scripts.join("rv_b.sh"), rendezvous_script(root, "b", "a")).unwrap();
}

// A supervised run must batch its ready non-interactive branches exactly like an
// autonomous one (spec 1.3): the two branches have to be in flight at the same
// time. Nothing here can park, so the drive never blocks on a supervisor.
#[test]
fn supervised_script_branches_run_concurrently() {
    let dir = tempfile::tempdir().unwrap();
    seed_rendezvous(dir.path());
    let opts = RunOptions {
        mode: RunMode::Supervised,
        ..Default::default()
    };
    let started = Instant::now();
    let res = run(dir.path(), "supconc", None, opts).unwrap();
    let elapsed = started.elapsed();
    assert_eq!(res.outcome, RunStatus::Succeeded);

    for branch in ["a", "b"] {
        let path = dir.path().join(format!("no_overlap_{branch}"));
        assert!(
            !path.is_file(),
            "supervised branches did not overlap: {}",
            fs::read_to_string(&path).unwrap_or_default().trim()
        );
    }
    // Secondary, and only an upper bound: a serialized run pays the full 5s
    // rendezvous timeout of the first branch.
    assert!(
        elapsed.as_millis() < 3000,
        "supervised diamond took {elapsed:?}; expected the branches to overlap"
    );
}

fn position<F: Fn(&EventPayload) -> bool>(events: &[Event], what: &str, pred: F) -> usize {
    events
        .iter()
        .position(|e| pred(&e.payload))
        .unwrap_or_else(|| panic!("expected a {what} event in the log"))
}

fn started_at(events: &[Event], node: &'static str) -> usize {
    position(
        events,
        &format!("node_started for {node}"),
        |p| matches!(p, EventPayload::NodeStarted { node: n, .. } if n == node),
    )
}

fn finished_at(events: &[Event], node: &'static str) -> usize {
    position(
        events,
        &format!("node_finished for {node}"),
        |p| matches!(p, EventPayload::NodeFinished { node: n, .. } if n == node),
    )
}

// Two 0.3s sleeps in the same diamond, capped to one at a time.
const SERIAL: &str = r#"
schema: 1
id: serialpb
name: Serial
version: 1.0.0
defaults:
  max_parallel: 1
nodes:
  - { id: start, type: start }
  - { id: a, type: script, script: "scripts/nap.sh", runner: sh }
  - { id: b, type: script, script: "scripts/nap.sh", runner: sh }
  - { id: j, type: prompt, prompt: "joined" }
  - { id: done, type: finish, outcome: success }
edges:
  - { from: start, to: a }
  - { from: start, to: b }
  - { from: a, to: j, join: all }
  - { from: b, to: j, join: all }
  - { from: j, to: done }
"#;

fn seed_nap(root: &Path, id: &str, yaml: &str) {
    init_project(root).unwrap();
    let dir = root.join(".apb/playbooks").join(id).join("1.0.0");
    let scripts = dir.join("scripts");
    fs::create_dir_all(&scripts).unwrap();
    fs::write(dir.join("playbook.yaml"), yaml).unwrap();
    fs::write(
        root.join(".apb/playbooks").join(id).join("current"),
        "1.0.0",
    )
    .unwrap();
    fs::write(scripts.join("nap.sh"), "sleep 0.3\n").unwrap();
}

// `defaults.max_parallel: 1` is the authoring end of the precedence chain: the
// same fan-out must run one branch at a time.
#[test]
fn defaults_max_parallel_one_serializes_the_fan_out() {
    let dir = tempfile::tempdir().unwrap();
    seed_nap(dir.path(), "serialpb", SERIAL);
    let started = Instant::now();
    let res = run(dir.path(), "serialpb", None, RunOptions::default()).unwrap();
    let elapsed = started.elapsed();
    assert_eq!(res.outcome, RunStatus::Succeeded);

    // Structural proof, independent of machine speed: branch b cannot have
    // started before branch a finished.
    let events = read_all(&dir.path().join(".apb/runs").join(&res.run_id)).unwrap();
    assert!(
        finished_at(&events, "a") < started_at(&events, "b"),
        "with max_parallel 1 branch b must start only after branch a finished"
    );
    // And the wall clock is at least the sum of the two sleeps.
    assert!(
        elapsed.as_millis() >= 600,
        "two serialized 0.3s branches ran in {elapsed:?}; expected >= 600ms"
    );
}

// The invocation end of the chain, and the fact that it is PERSISTED: a detached
// driver re-reads run.yaml and must keep the cap it was started with.
const SERIAL_NO_DEFAULTS: &str = r#"
schema: 1
id: optserial
name: Opt serial
version: 1.0.0
nodes:
  - { id: start, type: start }
  - { id: a, type: script, script: "scripts/nap.sh", runner: sh }
  - { id: b, type: script, script: "scripts/nap.sh", runner: sh }
  - { id: j, type: prompt, prompt: "joined" }
  - { id: done, type: finish, outcome: success }
edges:
  - { from: start, to: a }
  - { from: start, to: b }
  - { from: a, to: j, join: all }
  - { from: b, to: j, join: all }
  - { from: j, to: done }
"#;

#[test]
fn run_options_max_parallel_is_persisted_and_serializes_the_fan_out() {
    let dir = tempfile::tempdir().unwrap();
    seed_nap(dir.path(), "optserial", SERIAL_NO_DEFAULTS);
    let opts = RunOptions {
        max_parallel: Some(1),
        ..Default::default()
    };
    let res = run(dir.path(), "optserial", None, opts).unwrap();
    assert_eq!(res.outcome, RunStatus::Succeeded);

    let run_dir = dir.path().join(".apb/runs").join(&res.run_id);
    let events = read_all(&run_dir).unwrap();
    assert!(
        finished_at(&events, "a") < started_at(&events, "b"),
        "RunOptions.max_parallel 1 must serialize the fan-out"
    );
    let persisted = fs::read_to_string(run_dir.join("run.yaml")).unwrap();
    assert!(
        persisted.contains("max_parallel: 1"),
        "the cap must survive into run.yaml for a detached re-drive, got:\n{persisted}"
    );
}

// A fan-out whose FIRST branch (in edge order, so the one that becomes
// `current`) cannot batch, while the two behind it can. The batch must not run
// the pair and leave `current` behind: it is not re-queued, so it would be
// skipped silently while the merge below writes it off as dead and the run still
// reports success.
const NON_BATCHABLE_HEAD: &str = r#"
schema: 1
id: mixedpb
name: Mixed head
version: 1.0.0
nodes:
  - { id: start, type: start }
  - { id: note, type: prompt, prompt: "not batchable" }
  - { id: a, type: script, script: "scripts/nap.sh", runner: sh }
  - { id: b, type: script, script: "scripts/nap.sh", runner: sh }
  - { id: done, type: finish, outcome: success }
edges:
  - { from: start, to: note }
  - { from: start, to: a }
  - { from: start, to: b }
  - { from: note, to: done }
  - { from: a, to: done }
  - { from: b, to: done }
"#;

#[test]
fn a_non_batchable_head_is_not_dropped_by_a_batch_of_its_siblings() {
    let dir = tempfile::tempdir().unwrap();
    seed_nap(dir.path(), "mixedpb", NON_BATCHABLE_HEAD);
    let res = run(dir.path(), "mixedpb", None, RunOptions::default()).unwrap();
    assert_eq!(res.outcome, RunStatus::Succeeded);

    let events = read_all(&dir.path().join(".apb/runs").join(&res.run_id)).unwrap();
    for n in ["note", "a", "b"] {
        assert!(
            events.iter().any(
                |e| matches!(&e.payload, EventPayload::NodeFinished { node, .. } if node == n)
            ),
            "branch {n} must execute, not be skipped by the batch of its siblings"
        );
    }
}

// Three branches, two slots: admission is chunked in batch order, so the third
// branch waits for the first chunk to drain.
const CAPPED_FANOUT: &str = r#"
schema: 1
id: cappb
name: Capped
version: 1.0.0
defaults:
  max_parallel: 2
nodes:
  - { id: start, type: start }
  - { id: a, type: script, script: "scripts/nap.sh", runner: sh }
  - { id: b, type: script, script: "scripts/nap.sh", runner: sh }
  - { id: c, type: script, script: "scripts/nap.sh", runner: sh }
  - { id: m, type: prompt, prompt: "merged" }
  - { id: done, type: finish, outcome: success }
edges:
  - { from: start, to: a }
  - { from: start, to: b }
  - { from: start, to: c }
  - { from: a, to: m, join: all }
  - { from: b, to: m, join: all }
  - { from: c, to: m, join: all }
  - { from: m, to: done }
"#;

#[test]
fn max_parallel_two_admits_the_third_branch_after_the_first_chunk() {
    let dir = tempfile::tempdir().unwrap();
    seed_nap(dir.path(), "cappb", CAPPED_FANOUT);
    let res = run(dir.path(), "cappb", None, RunOptions::default()).unwrap();
    assert_eq!(res.outcome, RunStatus::Succeeded);

    let events = read_all(&dir.path().join(".apb/runs").join(&res.run_id)).unwrap();
    // First chunk: a and b overlap.
    assert!(
        started_at(&events, "b") < finished_at(&events, "a"),
        "the first chunk must run its two branches concurrently"
    );
    // Second chunk: c is admitted only once a slot is free.
    for earlier in ["a", "b"] {
        assert!(
            finished_at(&events, earlier) < started_at(&events, "c"),
            "branch c must wait for branch {earlier} to release its slot"
        );
    }
    // The barrier still collected all three inputs.
    finished_at(&events, "m");
}

// The queued half of join:any cancellation: the race is decided by the first
// chunk, so the branch that never got a slot must never be SPAWNED (its
// process tree never runs) - but it still gets the paired write-off
// NodeStarted/NodeFinished journal shape, so it is not left looking pending
// either.
const CAPPED_ANY: &str = r#"
schema: 1
id: capany
name: Capped any
version: 1.0.0
defaults:
  max_parallel: 2
nodes:
  - { id: start, type: start }
  - { id: a, type: script, script: "scripts/nap.sh", runner: sh }
  - { id: b, type: script, script: "scripts/nap.sh", runner: sh }
  - { id: c, type: script, script: "scripts/nap.sh", runner: sh }
  - { id: m, type: prompt, prompt: "merged" }
  - { id: done, type: finish, outcome: success }
edges:
  - { from: start, to: a }
  - { from: start, to: b }
  - { from: start, to: c }
  - { from: a, to: m, join: any }
  - { from: b, to: m, join: any }
  - { from: c, to: m, join: any }
  - { from: m, to: done }
"#;

#[test]
fn a_queued_branch_is_cancelled_when_an_any_join_is_already_won() {
    let dir = tempfile::tempdir().unwrap();
    seed_nap(dir.path(), "capany", CAPPED_ANY);
    let res = run(dir.path(), "capany", None, RunOptions::default()).unwrap();
    assert_eq!(res.outcome, RunStatus::Succeeded);

    let events = read_all(&dir.path().join(".apb/runs").join(&res.run_id)).unwrap();
    // A queued branch now gets a paired NodeStarted (the unified cancelled
    // journal shape), so `!NodeStarted` is no longer the honest property.
    // What this pins instead is the pairing itself: the queued branch's
    // NodeStarted is immediately followed - nothing interleaved - by its own
    // cancelled NodeFinished.
    let started_at = events
        .iter()
        .position(|e| matches!(&e.payload, EventPayload::NodeStarted { node, .. } if node == "c"))
        .unwrap_or_else(|| panic!("queued branch c has no NodeStarted"));
    assert!(
        matches!(
            events.get(started_at + 1).map(|e| &e.payload),
            Some(EventPayload::NodeFinished { node, status, .. }) if node == "c" && status == "cancelled"
        ),
        "queued branch c's NodeStarted must be immediately followed by its own cancelled NodeFinished, got {:?}",
        events.get(started_at + 1).map(|e| &e.payload)
    );
    finished_at(&events, "m");
}

/// #79: two implicit multi-input consumers of the same producer pair must run
/// CONCURRENTLY. `w1` and `w2` each have two incoming edges and no `join:`
/// field, so they are implicit fan-ins; before the narrowing they were both
/// excluded from the batch and ran one scheduling pass each. The overlap proof
/// is the rendezvous marker, not elapsed time.
const IMPLICIT_CONSUMERS: &str = r#"
schema: 1
id: implconc
name: Implicit consumers
version: 1.0.0
defaults:
  max_parallel: 4
nodes:
  - { id: start, type: start }
  - { id: a, type: prompt, prompt: "a" }
  - { id: b, type: prompt, prompt: "b" }
  - { id: w1, type: script, script: "scripts/rv_w1.sh", runner: sh }
  - { id: w2, type: script, script: "scripts/rv_w2.sh", runner: sh }
  - { id: done, type: finish, outcome: success }
edges:
  - { from: start, to: a }
  - { from: start, to: b }
  - { from: a, to: w1 }
  - { from: b, to: w1 }
  - { from: a, to: w2 }
  - { from: b, to: w2 }
  - { from: w1, to: done }
  - { from: w2, to: done }
"#;

fn seed_implicit_consumers(root: &Path) {
    init_project(root).unwrap();
    let dir = root.join(".apb/playbooks/implconc/1.0.0");
    let scripts = dir.join("scripts");
    fs::create_dir_all(&scripts).unwrap();
    fs::write(dir.join("playbook.yaml"), IMPLICIT_CONSUMERS).unwrap();
    fs::write(root.join(".apb/playbooks/implconc/current"), "1.0.0").unwrap();
    fs::write(
        scripts.join("rv_w1.sh"),
        rendezvous_script(root, "w1", "w2"),
    )
    .unwrap();
    fs::write(
        scripts.join("rv_w2.sh"),
        rendezvous_script(root, "w2", "w1"),
    )
    .unwrap();
}

#[test]
fn implicit_fan_in_consumers_batch_with_each_other() {
    let dir = tempfile::tempdir().unwrap();
    seed_implicit_consumers(dir.path());
    let res = run(dir.path(), "implconc", None, RunOptions::default()).unwrap();
    assert_eq!(res.outcome, RunStatus::Succeeded);
    for branch in ["w1", "w2"] {
        let path = dir.path().join(format!("no_overlap_{branch}"));
        assert!(
            !path.is_file(),
            "implicit fan-in consumers did not overlap: {}",
            fs::read_to_string(&path).unwrap_or_default().trim()
        );
    }
}

#[test]
fn parallel_script_branches_run_concurrently() {
    let dir = tempfile::tempdir().unwrap();
    seed(dir.path());
    let started = Instant::now();
    let res = run(dir.path(), "conc", None, RunOptions::default()).unwrap();
    let elapsed = started.elapsed();
    assert_eq!(res.outcome, RunStatus::Succeeded);

    // Both branches ran and converged.
    let run_dir = dir.path().join(".apb/runs").join(&res.run_id);
    let events = read_all(&run_dir).unwrap();
    for n in ["a", "b", "j"] {
        assert!(
            events.iter().any(
                |e| matches!(&e.payload, EventPayload::NodeFinished { node, .. } if node == n)
            ),
            "node {n} must finish"
        );
    }
    // Concurrent: 3s of work total, but wall-clock should be ~1.5s + spawn
    // overhead. Threshold 2.5s leaves margin for slow CI runners while still
    // catching sequential execution (~3s).
    assert!(
        elapsed.as_millis() < 2500,
        "two 1.5s branches ran in {elapsed:?}; expected concurrent (< 2500ms)"
    );
}
