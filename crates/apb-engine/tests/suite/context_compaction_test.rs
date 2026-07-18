use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;

use apb_core::registry::init_project;
use apb_engine::context::build_context_for_render;
use apb_engine::event::{EventPayload, read_all};
use apb_engine::scheduler::{RunOptions, run};
use apb_engine::state::RunStatus;

use crate::common;

// Three agent_task nodes in a row, each printing a large output. With a small
// context_max_bytes, the engine must compact old sections into context_compact.md
// with a cheap model BEFORE rendering the next prompt, while the primary
// context.md (the materialized full view) stays untouched - this is the
// replay-determinism invariant (spec 8.5).
const PLAYBOOK: &str = r#"
schema: 1
id: cc
name: Compaction
version: 1.0.0
defaults:
  profile: main
nodes:
  - { id: start, type: start }
  - { id: a, type: agent_task, prompt: "do a" }
  - { id: b, type: agent_task, prompt: "do b" }
  - { id: c, type: agent_task, prompt: "do c" }
  - { id: done, type: finish, outcome: success }
edges:
  - { from: start, to: a }
  - { from: a, to: b }
  - { from: b, to: c }
  - { from: c, to: done }
"#;

// The mock agent prints 300 bytes of fixed output regardless of the
// prompt/model: both worker nodes and the compaction call get the same response.
fn write_blob_agent(root: &Path) -> String {
    let path = root.join("blob-agent.sh");
    fs::write(&path, "#!/bin/sh\nprintf 'X%.0s' $(seq 1 300)\necho\n").unwrap();
    let mut p = fs::metadata(&path).unwrap().permissions();
    p.set_mode(0o755);
    fs::set_permissions(&path, p).unwrap();
    path.to_string_lossy().to_string()
}

fn seed(root: &Path) {
    init_project(root).unwrap();
    let dir = root.join(".apb/playbooks/cc/1.0.0");
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("playbook.yaml"), PLAYBOOK).unwrap();
    fs::write(root.join(".apb/playbooks/cc/current"), "1.0.0").unwrap();
    common::seed_main(root);
}

#[test]
fn context_compaction_writes_artifact_and_keeps_primary_intact() {
    let _env = common::env_lock();
    let dir = tempfile::tempdir().unwrap();
    seed(dir.path());
    let prog = write_blob_agent(dir.path());
    unsafe {
        std::env::set_var("APB_AGENT_CMD", &prog);
    }

    let opts = RunOptions {
        // The threshold is deliberately smaller than the sum of two sections (~300 bytes each): by the
        // time node c is rendered, old sections must have been compacted.
        context_max_bytes: Some(500),
        ..RunOptions::default()
    };
    let res = run(dir.path(), "cc", None, opts).unwrap();

    unsafe {
        std::env::remove_var("APB_AGENT_CMD");
    }

    assert_eq!(res.outcome, RunStatus::Succeeded);

    let run_dir = dir.path().join(".apb/runs").join(&res.run_id);
    let events = read_all(&run_dir).unwrap();

    // 1. The compaction event is recorded and references a file rather than storing the summary.
    let compacted: Vec<_> = events
        .iter()
        .filter_map(|e| match &e.payload {
            EventPayload::ContextCompacted {
                compact_file,
                up_to_seq,
                ..
            } => Some((compact_file.clone(), *up_to_seq)),
            _ => None,
        })
        .collect();
    assert!(
        !compacted.is_empty(),
        "expected at least one ContextCompacted event"
    );
    assert_eq!(compacted[0].0, "context_compact.md");

    // 2. The compaction artifact exists on disk.
    assert!(
        run_dir.join("context_compact.md").is_file(),
        "context_compact.md must exist"
    );

    // 3. The primary log does not contain the summary text: the raw compaction
    //    event line carries only file/model/seq (replay determinism).
    let raw = fs::read_to_string(run_dir.join("events.jsonl")).unwrap();
    for line in raw.lines().filter(|l| l.contains("context_compacted")) {
        assert!(
            line.contains("context_compact.md"),
            "compaction event must name the file"
        );
        assert!(
            !line.contains("summary (compacted)"),
            "summary text must not leak into the log"
        );
    }

    // 4. The full materialized context (build_context) still contains
    //    the raw sections of all nodes - the primary source for replay is untouched.
    let full = apb_engine::context::build_context(&events);
    assert!(
        full.contains("## a ("),
        "full context must retain node a section"
    );
    assert!(
        full.contains("## b ("),
        "full context must retain node b section"
    );

    // 5. The render context = summary + uncompacted tail: shorter than the full one and
    //    starts with the summary heading.
    let rendered = build_context_for_render(&run_dir, &events).unwrap();
    assert!(
        rendered.starts_with("## summary (compacted)"),
        "rendered context must lead with summary"
    );
    assert!(
        rendered.len() < full.len(),
        "compacted render must be shorter than full context"
    );
}
