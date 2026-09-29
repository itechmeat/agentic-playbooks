//! Run provenance (C7): `APB_RUN_ID` and `{{run.id}}` name the run, and a
//! node that commits on a git tree journals `artifacts_committed`.

use std::fs;
use std::path::Path;
use std::process::Command;

use apb_core::registry::init_project;
use apb_engine::event::{EventPayload, read_all};
use apb_engine::scheduler::{RunOptions, run};
use apb_engine::state::RunStatus;

const PLAYBOOK: &str = r#"
schema: 2
id: prov
name: Provenance
version: 1.0.0
nodes:
  - { id: start, type: start }
  - { id: commit, type: script, script: "scripts/commit.sh", runner: sh }
  - { id: note, type: prompt, prompt: "run {{run.id}}" }
  - { id: done, type: finish, outcome: success }
edges:
  - { from: start, to: commit }
  - { from: commit, to: note }
  - { from: note, to: done }
"#;

/// Writes the run id it got to `id.txt` and commits it with the trailer
/// when the tree is a git repository.
const SCRIPT: &str = r#"printf '%s' "$APB_RUN_ID" > id.txt
printf '%s|%s' "$APB_NODE_ID" "$(basename "$APB_RUN_DIR")" > ctx.txt
if git rev-parse --verify -q HEAD >/dev/null 2>&1; then
  git add id.txt ctx.txt
  git -c user.name=t -c user.email=t@example.invalid -c commit.gpgsign=false \
    commit -q -m "record the run id" -m "Apb-Run: $APB_RUN_ID"
fi
"#;

fn git(dir: &Path, args: &[&str]) {
    let ok = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args([
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@example.invalid",
            "-c",
            "commit.gpgsign=false",
        ])
        .args(args)
        .status()
        .unwrap()
        .success();
    assert!(ok, "git {args:?}");
}

fn seed(root: &Path) {
    init_project(root).unwrap();
    let dir = root.join(".apb/playbooks/prov/1.0.0");
    fs::create_dir_all(dir.join("scripts")).unwrap();
    fs::write(dir.join("playbook.yaml"), PLAYBOOK).unwrap();
    fs::write(dir.join("scripts/commit.sh"), SCRIPT).unwrap();
    fs::write(root.join(".apb/playbooks/prov/current"), "1.0.0").unwrap();
}

fn committed(events: &[apb_engine::event::Event]) -> Vec<&EventPayload> {
    events
        .iter()
        .map(|e| &e.payload)
        .filter(|p| matches!(p, EventPayload::ArtifactsCommitted { .. }))
        .collect()
}

#[test]
fn a_committing_node_on_a_git_tree_journals_its_commits_and_the_run_id() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    seed(root);
    fs::write(root.join(".gitignore"), ".apb/\n").unwrap();
    git(root, &["init", "-q"]);
    git(root, &["add", ".gitignore"]);
    git(root, &["commit", "-q", "-m", "base"]);

    let res = run(root, "prov", None, RunOptions::default()).unwrap();
    assert_eq!(res.outcome, RunStatus::Succeeded);
    let run_id = res.run_id.clone();
    assert_eq!(fs::read_to_string(root.join("id.txt")).unwrap(), run_id);
    assert_eq!(
        fs::read_to_string(root.join("ctx.txt")).unwrap(),
        format!("commit|{run_id}")
    );

    let events = read_all(&root.join(".apb/runs").join(&run_id)).unwrap();
    let found = committed(&events);
    assert_eq!(found.len(), 1, "{found:?}");
    let EventPayload::ArtifactsCommitted {
        node,
        before,
        after,
        commits,
        omitted,
    } = found[0]
    else {
        unreachable!()
    };
    assert_eq!(node, "commit");
    assert_ne!(before, after);
    assert_eq!(*omitted, 0);
    assert_eq!(commits.len(), 1);
    assert_eq!(commits[0].sha, *after);
    assert_eq!(commits[0].subject, "record the run id");
    // The trailer carries the id from APB_RUN_ID.
    let msg = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["log", "-1", "--format=%B"])
        .output()
        .unwrap();
    assert!(String::from_utf8_lossy(&msg.stdout).contains(&format!("Apb-Run: {run_id}")));
    // It is journaled before the node's own node_finished (its checkpoint).
    let at = |pred: &dyn Fn(&EventPayload) -> bool| {
        events.iter().position(|e| pred(&e.payload)).unwrap()
    };
    let rec = at(&|p| matches!(p, EventPayload::ArtifactsCommitted { .. }));
    let fin = at(&|p| matches!(p, EventPayload::NodeFinished { node, .. } if node == "commit"));
    assert!(rec < fin);

    // `{{run.id}}` renders the same id.
    let note = events
        .iter()
        .find_map(|e| match &e.payload {
            EventPayload::NodeFinished { node, output, .. } if node == "note" => {
                Some(output.clone())
            }
            _ => None,
        })
        .unwrap();
    assert_eq!(note, format!("run {run_id}"));

    // The run view lists the commits for the run surfaces.
    let view = apb_engine::run_view::RunView::load(&root.join(".apb/runs").join(&run_id), &run_id)
        .unwrap();
    assert_eq!(view.commits().len(), 1);
}

/// Outside git the run journals no provenance record at all: the same event
/// types as before the feature.
#[test]
fn a_tree_without_git_journals_no_provenance() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    seed(root);
    let res = run(root, "prov", None, RunOptions::default()).unwrap();
    assert_eq!(res.outcome, RunStatus::Succeeded);
    let events = read_all(&root.join(".apb/runs").join(&res.run_id)).unwrap();
    assert!(committed(&events).is_empty());
    let kinds: Vec<String> = events
        .iter()
        .map(|e| {
            serde_json::to_value(&e.payload).unwrap()["type"]
                .as_str()
                .unwrap()
                .to_string()
        })
        .collect();
    assert_eq!(
        kinds,
        [
            "run_started",
            "run_provenance",
            "node_started",
            "node_finished",
            "edge_traversed",
            "node_started",
            "node_finished",
            "edge_traversed",
            "node_started",
            "node_finished",
            "edge_traversed",
            "node_finished",
            "run_finished",
        ]
    );
}
