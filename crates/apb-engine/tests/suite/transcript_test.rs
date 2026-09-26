//! Per-attempt transcripts (issue #67 item 10): every agent attempt leaves
//! what it printed in `<run>/attempts/<node>-<attempt>/`, named on its
//! `attempt_started` event, including an attempt killed at its deadline, and
//! a claude attempt also leaves a copy of the CLI's own session transcript.

use std::fs;
use std::path::{Path, PathBuf};

use apb_core::registry::init_project;
use apb_engine::event::{EventPayload, read_all};
use apb_engine::scheduler::{RunOptions, run};
use apb_engine::state::RunStatus;

use crate::common;
use crate::token_economy_test::{recording_stub, seed_playbook};

const OK: &str = "printf '\\n```yaml\\nstatus: success\\nsummary: ok\\n```\\n'";

fn playbook(extra: &str) -> String {
    format!(
        "schema: 2\nid: one\nname: One\nversion: 1.0.0\ndefaults: {{ profile: main }}\nnodes:\n  - {{ id: start, type: start }}\n  - {{ id: work, type: agent_task, prompt: work{extra} }}\n  - {{ id: done, type: finish, outcome: success }}\n  - {{ id: failed, type: finish, outcome: failure }}\nedges:\n  - {{ from: start, to: work }}\n  - {{ from: work, to: done, condition: {{ type: node_status, node: work, equals: success }} }}\n  - {{ from: work, to: failed, condition: {{ type: node_status, node: work, equals: failure }} }}\n"
    )
}

/// Runs `one` under `stub` with `CLAUDE_CONFIG_DIR` pointed at `claude_dir`,
/// returning the run's outcome and directory.
fn run_one(root: &Path, stub: &str, claude_dir: &Path) -> (RunStatus, PathBuf) {
    let _env = common::env_lock();
    let prev = std::env::var_os("CLAUDE_CONFIG_DIR");
    unsafe {
        std::env::set_var("APB_AGENT_CMD", stub);
        std::env::set_var("CLAUDE_CONFIG_DIR", claude_dir);
    }
    let res = run(root, "one", None, RunOptions::default());
    unsafe {
        std::env::remove_var("APB_AGENT_CMD");
        match prev {
            Some(v) => std::env::set_var("CLAUDE_CONFIG_DIR", v),
            None => std::env::remove_var("CLAUDE_CONFIG_DIR"),
        }
    }
    let res = res.unwrap();
    (res.outcome, root.join(".apb/runs").join(&res.run_id))
}

/// The transcript directory `attempt_started` names for `work`'s attempts.
fn transcripts(run_dir: &Path) -> Vec<PathBuf> {
    read_all(run_dir)
        .unwrap()
        .into_iter()
        .filter_map(|e| match e.payload {
            EventPayload::AttemptStarted {
                node, transcript, ..
            } if node == "work" => transcript.map(|t| run_dir.join(t)),
            _ => None,
        })
        .collect()
}

#[test]
fn an_attempt_keeps_its_output_and_the_claude_session_transcript() {
    let dir = tempfile::tempdir().unwrap();
    init_project(dir.path()).unwrap();
    seed_playbook(dir.path(), "one", &playbook(""));
    common::seed_profile(dir.path(), "main", "claude", "haiku", &[]);
    let claude = dir.path().join("claude-home");
    // The stub stands in for claude: it records its session where claude
    // keeps it, keyed by the id apb assigned.
    let stub = recording_stub(
        dir.path(),
        &format!(
            "prev=''; for a in \"$@\"; do if [ \"$prev\" = --session-id ]; then mkdir -p \"$CLAUDE_CONFIG_DIR/projects/-some-project\"; echo '{{\"type\":\"tool_use\",\"name\":\"Bash\"}}' > \"$CLAUDE_CONFIG_DIR/projects/-some-project/$a.jsonl\"; fi; prev=\"$a\"; done; echo 'progress on stdout'; echo 'noise on stderr' 1>&2; {OK}"
        ),
    );
    let (outcome, run_dir) = run_one(dir.path(), &stub, &claude);
    assert_eq!(outcome, RunStatus::Succeeded);
    let t = transcripts(&run_dir);
    assert_eq!(t, vec![run_dir.join("attempts/work-1")]);
    let out = fs::read_to_string(t[0].join("stdout.log")).unwrap();
    assert!(
        out.contains("progress on stdout") && out.contains("status: success"),
        "{out}"
    );
    let err = fs::read_to_string(t[0].join("stderr.log")).unwrap();
    assert!(err.contains("noise on stderr"), "{err}");
    let session = fs::read_to_string(t[0].join("session.jsonl")).unwrap();
    assert!(session.contains("tool_use"), "{session}");
}

/// An attempt killed at its deadline still leaves what it printed.
#[test]
fn a_timed_out_attempt_keeps_what_it_printed() {
    let dir = tempfile::tempdir().unwrap();
    init_project(dir.path()).unwrap();
    seed_playbook(dir.path(), "one", &playbook(", timeout_seconds: 1"));
    common::seed_profile(dir.path(), "main", "claude", "haiku", &[]);
    let stub = recording_stub(dir.path(), "echo 'ran the gate: 3 red'; sleep 5");
    let (outcome, run_dir) = run_one(dir.path(), &stub, &dir.path().join("none"));
    assert_eq!(outcome, RunStatus::Failed);
    let t = transcripts(&run_dir);
    assert!(!t.is_empty());
    let out = fs::read_to_string(t[0].join("stdout.log")).unwrap();
    assert!(out.contains("ran the gate: 3 red"), "{out}");
}
