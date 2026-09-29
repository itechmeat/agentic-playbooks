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
    // A run without goal script criteria pins no scripts digest: its
    // `run_provenance` reads as in 0.22.
    let journal = std::fs::read_to_string(
        root.join(".apb/runs")
            .join(&res.run_id)
            .join("events.jsonl"),
    )
    .unwrap();
    assert!(!journal.contains("scripts_digest"), "{journal}");
}

/// Seeds playbook `p2` from `yaml` with the given scripts.
fn seed_other(root: &Path, yaml: &str, scripts: &[(&str, &str)]) {
    init_project(root).unwrap();
    let dir = root.join(".apb/playbooks/p2/1.0.0");
    fs::create_dir_all(dir.join("scripts")).unwrap();
    fs::write(dir.join("playbook.yaml"), yaml).unwrap();
    for (name, body) in scripts {
        fs::write(dir.join("scripts").join(name), body).unwrap();
    }
    fs::write(root.join(".apb/playbooks/p2/current"), "1.0.0").unwrap();
}

fn git_repo(root: &Path) {
    fs::write(root.join(".gitignore"), ".apb/\n").unwrap();
    git(root, &["init", "-q", "-b", "main"]);
    git(root, &["add", ".gitignore"]);
    git(root, &["commit", "-q", "-m", "base"]);
}

fn listed(events: &[apb_engine::event::Event]) -> Vec<(String, Vec<String>)> {
    events
        .iter()
        .filter_map(|e| match &e.payload {
            EventPayload::ArtifactsCommitted { node, commits, .. } => Some((
                node.clone(),
                commits.iter().map(|c| c.subject.clone()).collect(),
            )),
            _ => None,
        })
        .collect()
}

/// A node that only switches branches committed nothing: the other
/// branch's history is never listed as its commits.
#[test]
fn a_branch_switch_lists_no_commits() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    seed_other(
        root,
        r#"
schema: 2
id: p2
name: Switch
version: 1.0.0
nodes:
  - { id: start, type: start }
  - { id: switch, type: script, script: "scripts/switch.sh", runner: sh }
  - { id: done, type: finish, outcome: success }
edges:
  - { from: start, to: switch }
  - { from: switch, to: done }
"#,
        &[("switch.sh", "git checkout -q main\n")],
    );
    git_repo(root);
    git(root, &["checkout", "-q", "-b", "feat"]);
    git(root, &["checkout", "-q", "main"]);
    git(
        root,
        &["commit", "-q", "--allow-empty", "-m", "only on main"],
    );
    git(root, &["checkout", "-q", "feat"]);
    let res = run(root, "p2", None, RunOptions::default()).unwrap();
    assert_eq!(res.outcome, RunStatus::Succeeded);
    let events = read_all(&root.join(".apb/runs").join(&res.run_id)).unwrap();
    assert!(listed(&events).is_empty(), "{:?}", listed(&events));
}

/// Nodes that run at the same time on one tree move the same `HEAD`: no
/// node is credited with a sibling's commit.
#[test]
fn concurrent_nodes_never_list_each_others_commits() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    let commit = |name: &str, delay: &str| {
        format!(
            "sleep {delay}\necho {name} > {name}.txt\ngit add {name}.txt\n\
             git -c user.name=t -c user.email=t@example.invalid -c commit.gpgsign=false \
             commit -q -m 'by {name}'\n"
        )
    };
    seed_other(
        root,
        r#"
schema: 2
id: p2
name: Fork
version: 1.0.0
nodes:
  - { id: start, type: start }
  - { id: a, type: script, script: "scripts/a.sh", runner: sh }
  - { id: b, type: script, script: "scripts/b.sh", runner: sh }
  - { id: done, type: finish, outcome: success }
edges:
  - { from: start, to: a }
  - { from: start, to: b }
  - { from: a, to: done }
  - { from: b, to: done }
"#,
        &[("a.sh", &commit("a", "0.2")), ("b.sh", &commit("b", "1"))],
    );
    git_repo(root);
    let res = run(root, "p2", None, RunOptions::default()).unwrap();
    assert_eq!(res.outcome, RunStatus::Succeeded);
    let events = read_all(&root.join(".apb/runs").join(&res.run_id)).unwrap();
    for (node, subjects) in listed(&events) {
        assert!(
            subjects.iter().all(|s| *s == format!("by {node}")),
            "{node}: {subjects:?}"
        );
    }
}

/// Reading the node's commits never runs the repository's `gpg.program`
/// through `log.showSignature`.
#[test]
fn listing_commits_never_runs_the_repository_gpg_program() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    let bin = tempfile::tempdir().unwrap();
    let marker = bin.path().join("verified");
    let gpg = bin.path().join("gpg.sh");
    fs::write(
        &gpg,
        format!(
            "#!/bin/sh\ncase \"$*\" in *--verify*) touch '{}'; exit 1;; esac\n\
             cat >/dev/null\necho '[GNUPG:] SIG_CREATED D 1 8 00 0 FP' >&2\n\
             printf -- '-----BEGIN PGP SIGNATURE-----\\n\\nx\\n-----END PGP SIGNATURE-----\\n'\n",
            marker.display()
        ),
    )
    .unwrap();
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(&gpg, fs::Permissions::from_mode(0o755)).unwrap();
    seed_other(
        root,
        r#"
schema: 2
id: p2
name: Signed
version: 1.0.0
nodes:
  - { id: start, type: start }
  - { id: sign, type: script, script: "scripts/sign.sh", runner: sh }
  - { id: done, type: finish, outcome: success }
edges:
  - { from: start, to: sign }
  - { from: sign, to: done }
"#,
        &[(
            "sign.sh",
            "git -c user.name=t -c user.email=t@example.invalid commit -q -S -m signed --allow-empty\n",
        )],
    );
    git_repo(root);
    git(root, &["config", "gpg.program", &gpg.to_string_lossy()]);
    git(root, &["config", "log.showSignature", "true"]);
    let res = run(root, "p2", None, RunOptions::default()).unwrap();
    assert_eq!(res.outcome, RunStatus::Succeeded);
    let events = read_all(&root.join(".apb/runs").join(&res.run_id)).unwrap();
    assert_eq!(listed(&events), [("sign".into(), vec!["signed".into()])]);
    assert!(!marker.exists());
}

/// An interactive node that commits, then asks a question, then finishes
/// after the answer: its one record lists the commit made before the
/// question.
#[test]
fn an_interactive_node_records_the_commits_made_before_its_question() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().to_path_buf();
    seed_other(
        &root,
        r#"
schema: 2
id: p2
name: Ask
version: 1.0.0
defaults:
  profile: main
nodes:
  - { id: start, type: start }
  - { id: ask, type: agent_task, prompt: "work, then ask", interactive: true }
  - { id: done, type: finish, outcome: success }
edges:
  - { from: start, to: ask }
  - { from: ask, to: done }
"#,
        &[],
    );
    crate::common::seed_main(&root);
    git_repo(&root);
    let bin = tempfile::tempdir().unwrap();
    let agent = bin.path().join("agent.sh");
    crate::common::write_sync(
        &agent,
        &format!(
            "#!/bin/sh\nc='{}'\nn=$(cat \"$c\" 2>/dev/null || echo 0); n=$((n+1)); echo $n > \"$c\"\n\
             if [ \"$n\" = 1 ]; then\n\
               echo one > one.txt; git add one.txt\n\
               git -c user.name=t -c user.email=t@example.invalid -c commit.gpgsign=false commit -q -m 'before the question'\n\
               printf '%s\\n' '<<<apb:question>>>'\n\
               printf '%s\\n' '{{\"question\":\"Go on?\",\"options\":[\"yes\",\"no\"]}}'\n\
               exit 0\n\
             fi\necho done\n",
            bin.path().join("count").display()
        ),
    );
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(&agent, fs::Permissions::from_mode(0o755)).unwrap();

    let _env = crate::common::env_lock();
    unsafe {
        std::env::set_var("APB_AGENT_CMD", &agent);
    }
    let r = root.clone();
    let handle = std::thread::spawn(move || run(&r, "p2", None, RunOptions::default()));
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    let run_dir = loop {
        let asked = fs::read_dir(root.join(".apb/runs"))
            .ok()
            .and_then(|mut d| d.find_map(|e| e.ok().map(|e| e.path())))
            .filter(|dir| {
                read_all(dir).is_ok_and(|evs| {
                    evs.iter()
                        .any(|e| matches!(e.payload, EventPayload::QuestionAsked { .. }))
                })
            });
        if let Some(dir) = asked {
            break dir;
        }
        assert!(std::time::Instant::now() < deadline, "no question asked");
        std::thread::sleep(std::time::Duration::from_millis(20));
    };
    apb_engine::question::post_answer(&run_dir, Some("ask"), "yes", "human").unwrap();
    let res = handle.join().unwrap().unwrap();
    unsafe {
        std::env::remove_var("APB_AGENT_CMD");
    }
    assert_eq!(res.outcome, RunStatus::Succeeded);
    let events = read_all(&run_dir).unwrap();
    assert_eq!(
        listed(&events),
        [("ask".into(), vec!["before the question".into()])]
    );
}
