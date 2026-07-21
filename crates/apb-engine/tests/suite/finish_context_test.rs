//! Issue #42 finding 5: a terminal finish-with-prompt node must compose its
//! closing answer from the run's COMPLETE accumulated context, and a failure to
//! compose that closing answer must not flip an otherwise successful run to
//! failed.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::time::{Duration, Instant};

use apb_core::registry::init_project;
use apb_core::versioning::create_patch_version;
use apb_engine::control::Control;
use apb_engine::event::{EventPayload, read_all};
use apb_engine::scheduler::{
    RunMode, RunOptions, post_supervisor_command, resume, run, run_background,
};
use apb_engine::state::{RunState, RunStatus};

use crate::common;

const POLL_DEADLINE: Duration = Duration::from_secs(10);
const POLL_STEP: Duration = Duration::from_millis(10);

fn poll_until<T>(what: &str, mut f: impl FnMut() -> Option<T>) -> T {
    let started = Instant::now();
    loop {
        if let Some(value) = f() {
            return value;
        }
        if started.elapsed() > POLL_DEADLINE {
            panic!("timed out after {POLL_DEADLINE:?} waiting for {what}");
        }
        std::thread::sleep(POLL_STEP);
    }
}

fn set_exec(path: &Path) {
    let mut p = fs::metadata(path).unwrap().permissions();
    p.set_mode(0o755);
    fs::set_permissions(path, p).unwrap();
}

fn write_stub(dir: &Path, name: &str, body: &str) -> String {
    let path = dir.join(name);
    fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
    set_exec(&path);
    path.to_string_lossy().to_string()
}

/// The compaction summarizer (its prompt starts with the fixed "Summarize ..."
/// preamble) returns a fixed, detail-free summary - modelling a real cheap
/// model that, across repeated re-summarization, compresses earlier nodes'
/// substantive output away. Every other agent (the finish node) captures the
/// full prompt it received into `finish-prompt.txt` so the test can inspect
/// exactly what context the terminal node saw.
fn lossy_summarizer_agent(dir: &Path) -> String {
    let out = dir.join("finish-prompt.txt");
    write_stub(
        dir,
        "lossy-agent.sh",
        &format!(
            "case \"$*\" in\n  *\"Summarize the following playbook run context\"*)\n    printf 'LOSSY_SUMMARY\\n' ;;\n  *)\n    printf '%s\\n' \"$*\" > '{out}'\n    printf '%s\\n' \"$*\" ;;\nesac",
            out = out.display()
        ),
    )
}

fn seed(root: &Path, yaml: &str) {
    init_project(root).unwrap();
    let vdir = root.join(".apb/playbooks/fctx/1.0.0");
    fs::create_dir_all(&vdir).unwrap();
    fs::write(vdir.join("playbook.yaml"), yaml).unwrap();
    fs::write(root.join(".apb/playbooks/fctx/current"), "1.0.0").unwrap();
    common::seed_main(root);
}

fn wait_for_workdir_unlocked(root: &Path) {
    poll_until("the workdir lock to clear", || {
        (!root.join(".apb/workdir.lock").is_file()).then_some(())
    });
}

fn node_finished(run_dir: &Path, node: &str) -> bool {
    read_all(run_dir)
        .unwrap_or_default()
        .iter()
        .any(|e| matches!(&e.payload, EventPayload::NodeFinished { node: n, .. } if n == node))
}

// Four prompt nodes, each with a short but distinct output, feeding a terminal
// finish-with-prompt. With a small context_max_bytes, compaction summarizes the
// oldest sections (p1 among them) into the lossy summary before the finish node
// runs - so p1's output only survives to the terminal node if that node reads
// the FULL log rather than the compacted render view.
fn wf(p3: &str) -> String {
    format!(
        r#"
schema: 1
id: fctx
name: FinishContext
version: 1.0.0
defaults:
  profile: main
nodes:
  - {{ id: start, type: start }}
  - {{ id: p1, type: prompt, prompt: "MARKER_P1 shipped-the-feature-branch" }}
  - {{ id: p2, type: prompt, prompt: "MARKER_P2 opened-the-pull-request" }}
  - {{ id: p3, type: prompt, prompt: "MARKER_P3 {p3}" }}
  - {{ id: done, type: finish, outcome: success, prompt: "compose closing answer from: {{{{run.context}}}}" }}
edges:
  - {{ from: start, to: p1 }}
  - {{ from: p1, to: p2 }}
  - {{ from: p2, to: p3 }}
  - {{ from: p3, to: done }}
"#
    )
}

// Sub-bug 1 (context loss): after a resume plus patch migration - the exact
// combination that grows the run context past the compaction budget on a real
// run - the terminal finish node must still see the earliest completed node's
// output (MARKER_P1), even though compaction has replaced it with a lossy
// summary in the mid-run render view.
#[test]
fn finish_context_survives_resume_and_patch_migration() {
    let dir = tempfile::tempdir().unwrap();
    seed(dir.path(), &wf("merged-and-closed"));
    let _env = common::env_lock();
    let prog = lossy_summarizer_agent(dir.path());
    unsafe {
        std::env::set_var("APB_AGENT_CMD", &prog);
    }

    let run_id = run_background(
        dir.path(),
        "fctx",
        None,
        RunOptions {
            mode: RunMode::Supervised,
            context_max_bytes: Some(120),
            ..Default::default()
        },
    )
    .unwrap();
    let run_dir = dir.path().join(".apb/runs").join(&run_id);

    // A patch that touches only the not-yet-executed p3 (so migration is valid),
    // continuing from p3.
    let version = create_patch_version(
        dir.path(),
        "fctx",
        "1.0.0",
        &wf("re-ran-after-patch"),
        &run_id,
        "improvement",
    )
    .unwrap();

    // Pause after p2 finished: p1 and p2 are done, p3 is still pending.
    poll_until("p2 finished", || {
        node_finished(&run_dir, "p2").then_some(())
    });
    post_supervisor_command(dir.path(), &run_id, Control::Pause).unwrap();
    poll_until("run paused", || {
        (RunState::fold(&read_all(&run_dir).unwrap()).run_status == RunStatus::Paused).then_some(())
    });
    wait_for_workdir_unlocked(dir.path());

    // Queue the migration, then resume: the resumed drive applies the patch,
    // continues from p3, compacts the older sections, and reaches the finish node.
    post_supervisor_command(
        dir.path(),
        &run_id,
        Control::Patch {
            version,
            classification: "improvement".into(),
            continue_from: "p3".into(),
        },
    )
    .unwrap();
    let res = resume(dir.path(), &run_id, None).unwrap();

    unsafe {
        std::env::remove_var("APB_AGENT_CMD");
    }
    drop(_env);

    assert_eq!(res.outcome, RunStatus::Succeeded, "run must succeed");

    // Compaction really happened (otherwise this test proves nothing): the
    // lossy summary artifact was produced and it dropped MARKER_P1, so the only
    // way the terminal node can still see MARKER_P1 is by reading the full log
    // rather than this compacted summary.
    let compact = fs::read_to_string(run_dir.join("context_compact.md")).unwrap_or_default();
    assert!(
        compact.contains("LOSSY_SUMMARY") && !compact.contains("MARKER_P1"),
        "expected compaction to have summarized MARKER_P1 away into the lossy summary, got: {compact:?}"
    );

    // The finish node's ACTUAL prompt must still contain MARKER_P1: it composes
    // the closing answer from the full log, not the lossy compacted view.
    let finish_prompt =
        fs::read_to_string(dir.path().join("finish-prompt.txt")).unwrap_or_default();
    assert!(
        finish_prompt.contains("MARKER_P1"),
        "the terminal finish node lost the earliest node's output; it saw only:\n{finish_prompt}"
    );
}

// Sub-bug 2 (outcome override): a finish-with-prompt whose answer-composition
// agent fails must NOT flip an otherwise-successful run to failed. The declared
// `outcome: success` stands; the failure is journaled as a failed attempt plus
// a RunError anomaly, and the node output falls back to a minimal generated
// closing message.
#[test]
fn finish_answer_failure_keeps_declared_success_outcome() {
    let dir = tempfile::tempdir().unwrap();
    let yaml = r#"
schema: 1
id: fctx
name: FinishContext
version: 1.0.0
defaults:
  profile: main
nodes:
  - { id: start, type: start }
  - { id: work, type: prompt, prompt: "did the substantive work" }
  - { id: done, type: finish, outcome: success, prompt: "close: {{run.context}}" }
edges:
  - { from: start, to: work }
  - { from: work, to: done }
"#;
    seed(dir.path(), yaml);
    let _env = common::env_lock();
    // The finish agent always fails; no compaction agent runs (context is small).
    let prog = write_stub(dir.path(), "fail-agent.sh", "echo 'boom' 1>&2\nexit 1");
    unsafe {
        std::env::set_var("APB_AGENT_CMD", &prog);
    }

    let res = run(dir.path(), "fctx", None, RunOptions::default()).unwrap();

    unsafe {
        std::env::remove_var("APB_AGENT_CMD");
    }
    drop(_env);

    // The run's real outcome (the finish node's declared `success`) stands even
    // though the closing-answer composition failed.
    assert_eq!(
        res.outcome,
        RunStatus::Succeeded,
        "a failed closing-answer composition must not flip the run to failed"
    );

    let run_dir = dir.path().join(".apb/runs").join(&res.run_id);
    let events = read_all(&run_dir).unwrap();

    // The composition failure is visible: a failed attempt on the finish node...
    assert!(
        events.iter().any(|e| matches!(
            &e.payload,
            EventPayload::AttemptFinished { node, status, .. }
                if node == "done" && status == "failed"
        )),
        "expected a failed attempt on the finish node"
    );
    // ...plus a RunError anomaly naming the node.
    assert!(
        events.iter().any(|e| matches!(
            &e.payload,
            EventPayload::RunError { node: Some(n), reason }
                if n == "done" && reason.contains("finish answer composition failed")
        )),
        "expected a RunError anomaly for the finish node's composition failure"
    );

    // The node completes with a generated fallback closing message and the run
    // finishes succeeded.
    let state = RunState::fold(&events);
    assert_eq!(
        state.nodes.get("done"),
        Some(&apb_engine::state::NodeStatus::Succeeded)
    );
    assert!(
        state
            .outputs
            .get("done")
            .is_some_and(|o| o.contains("Run complete")),
        "the finish node must fall back to a generated closing message, got: {:?}",
        state.outputs.get("done")
    );
    assert!(
        events.iter().any(|e| matches!(
            &e.payload,
            EventPayload::RunFinished { outcome } if outcome == "succeeded"
        )),
        "expected a succeeded run_finished"
    );
}
