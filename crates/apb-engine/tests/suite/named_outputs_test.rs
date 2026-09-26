//! Declared node outputs and a templated node `workdir` (issue #67 item 4).
//!
//! A node declares the named values it publishes (`outputs.fields`); an
//! agent_task is then told exactly which keys to write into its status file,
//! and a later node can take its working directory from one of them
//! (`workdir: "{{nodes.probe.output.working_tree}}"`). A recording stub stands
//! in for the agent, so the tests read back the prompt it was given and the
//! directory it ran in.

use std::fs;
use std::path::Path;

use apb_core::registry::init_project;
use apb_engine::event::{EventPayload, read_all};
use apb_engine::state::RunStatus;

use crate::common;
use crate::token_economy_test::{invocations, recording_stub, run_with_stub, seed_playbook};

const OK: &str = "printf '\\n```yaml\\nstatus: success\\nsummary: ok\\n```\\n'";

/// The single run directory under `root`.
fn run_dir(root: &Path) -> std::path::PathBuf {
    fs::read_dir(root.join(".apb/runs"))
        .unwrap()
        .filter_map(|e| e.ok().map(|e| e.path()))
        .find(|p| p.is_dir())
        .expect("one run")
}

/// A stub part: the `probe` node publishes `working_tree` (whatever `$1`
/// expands to at run time) through its status file; every node records the
/// directory it ran in.
fn probe_part(tree: &str) -> String {
    format!(
        "pwd > \"$(dirname \"$0\")/pwd-$NODE\"; \
         if [ \"$NODE\" = probe ]; then printf '{{\"status\":\"success\",\"outputs\":{{\"working_tree\":\"{tree}\"}}}}' > \"$APB_STATUS_FILE\"; fi; \
         {OK}"
    )
}

fn playbook(work_extra: &str) -> String {
    format!(
        r#"schema: 2
id: tree
name: Tree
version: 1.0.0
defaults: {{ profile: main }}
nodes:
  - {{ id: start, type: start }}
  - {{ id: probe, type: agent_task, prompt: "Find the tree.", outputs: {{ fields: [working_tree] }} }}
  - {{ id: work, type: agent_task, prompt: "Work in the tree."{work_extra} }}
  - {{ id: gate, type: script, script: scripts/pwd.sh, runner: sh, workdir: "{{{{nodes.probe.output.working_tree}}}}" }}
  - {{ id: done, type: finish, outcome: success }}
  - {{ id: failed, type: finish, outcome: failure }}
edges:
  - {{ from: start, to: probe }}
  - {{ from: probe, to: work }}
  - {{ from: work, to: gate, condition: {{ type: node_status, node: work, equals: success }} }}
  - {{ from: work, to: failed, condition: {{ type: node_status, node: work, equals: failure }} }}
  - {{ from: gate, to: done }}
"#
    )
}

fn seed(root: &Path, work_extra: &str) {
    init_project(root).unwrap();
    seed_playbook(root, "tree", &playbook(work_extra));
    let scripts = root.join(".apb/playbooks/tree/1.0.0/scripts");
    fs::create_dir_all(&scripts).unwrap();
    fs::write(scripts.join("pwd.sh"), "pwd\n").unwrap();
    common::seed_main(root);
}

/// A declared field makes the status-file contract part of the prompt even
/// without a `success_check`, naming the exact key to write.
#[test]
fn declared_fields_are_asked_for_in_the_prompt() {
    let dir = tempfile::tempdir().unwrap();
    let tree = dir.path().join("wt");
    fs::create_dir_all(&tree).unwrap();
    seed(dir.path(), "");
    let stub = recording_stub(dir.path(), &probe_part(&tree.display().to_string()));
    assert_eq!(
        run_with_stub(dir.path(), "tree", &stub, None),
        RunStatus::Succeeded
    );
    let inv = invocations(dir.path());
    let probe = inv.iter().find(|i| i.node == "probe").unwrap().prompt();
    assert!(probe.contains("APB_STATUS_FILE"), "{probe}");
    assert!(probe.contains("`working_tree`"), "{probe}");
    let work = inv.iter().find(|i| i.node == "work").unwrap().prompt();
    assert!(
        !work.contains("APB_STATUS_FILE"),
        "a node declaring nothing keeps the report-only contract: {work}"
    );
}

/// The agent node and the script node both run in the directory the probe
/// published, resolved relative to the execution root when relative.
#[test]
fn a_templated_workdir_runs_the_node_where_a_named_output_points() {
    for relative in [false, true] {
        let dir = tempfile::tempdir().unwrap();
        let tree = dir.path().join("wt");
        fs::create_dir_all(&tree).unwrap();
        seed(
            dir.path(),
            ", workdir: \"{{nodes.probe.output.working_tree}}\"",
        );
        let published = if relative {
            "wt".to_string()
        } else {
            tree.display().to_string()
        };
        let stub = recording_stub(dir.path(), &probe_part(&published));
        assert_eq!(
            run_with_stub(dir.path(), "tree", &stub, None),
            RunStatus::Succeeded
        );
        let canon = tree.canonicalize().unwrap();
        let work_pwd = fs::read_to_string(dir.path().join("pwd-work")).unwrap();
        assert_eq!(Path::new(work_pwd.trim()).canonicalize().unwrap(), canon);
        let probe_pwd = fs::read_to_string(dir.path().join("pwd-probe")).unwrap();
        assert_eq!(
            Path::new(probe_pwd.trim()).canonicalize().unwrap(),
            dir.path().canonicalize().unwrap(),
            "a node without workdir stays in the execution root"
        );

        let events = read_all(&run_dir(dir.path())).unwrap();
        let gate_out = events
            .iter()
            .find_map(|e| match &e.payload {
                EventPayload::NodeFinished { node, output, .. } if node == "gate" => {
                    Some(output.clone())
                }
                _ => None,
            })
            .unwrap();
        assert_eq!(
            Path::new(gate_out.trim()).canonicalize().unwrap(),
            canon,
            "the script ran in the published tree"
        );
        let recorded = events.iter().find_map(|e| match &e.payload {
            EventPayload::AttemptStarted { node, workdir, .. } if node == "work" => workdir.clone(),
            _ => None,
        });
        assert_eq!(
            recorded.map(|w| Path::new(&w).canonicalize().unwrap()),
            Some(canon),
            "attempt_started records where the attempt ran"
        );
    }
}

/// A workdir that renders empty (the probe never published it) or names a
/// directory that does not exist fails the node without spawning the agent,
/// never falls back to the execution root.
#[test]
fn an_unresolvable_workdir_fails_the_node_instead_of_running_elsewhere() {
    for published in ["", "does/not/exist"] {
        let dir = tempfile::tempdir().unwrap();
        seed(
            dir.path(),
            ", workdir: \"{{nodes.probe.output.working_tree}}\"",
        );
        let stub = recording_stub(dir.path(), &probe_part(published));
        assert_eq!(
            run_with_stub(dir.path(), "tree", &stub, None),
            RunStatus::Failed,
            "{published:?}"
        );
        assert!(
            !invocations(dir.path()).iter().any(|i| i.node == "work"),
            "{published:?}: the agent must not be spawned"
        );
        let events = read_all(&run_dir(dir.path())).unwrap();
        let out = events
            .iter()
            .find_map(|e| match &e.payload {
                EventPayload::NodeFinished {
                    node,
                    output,
                    status,
                    ..
                } if node == "work" => Some((status.clone(), output.clone())),
                _ => None,
            })
            .unwrap();
        assert_eq!(out.0, "failed");
        assert!(out.1.contains("workdir"), "{published:?}: {}", out.1);
    }
}

/// A successful node whose output lacks a declared field journals a warning;
/// the node itself still succeeds.
#[test]
fn a_missing_declared_field_is_journaled_and_does_not_fail_the_node() {
    let dir = tempfile::tempdir().unwrap();
    init_project(dir.path()).unwrap();
    seed_playbook(
        dir.path(),
        "one",
        "schema: 2\nid: one\nname: One\nversion: 1.0.0\ndefaults: { profile: main }\nnodes:\n  - { id: start, type: start }\n  - { id: probe, type: agent_task, prompt: probe, outputs: { fields: [working_tree, verdict] } }\n  - { id: done, type: finish, outcome: success }\nedges:\n  - { from: start, to: probe }\n  - { from: probe, to: done }\n",
    );
    common::seed_main(dir.path());
    let stub = recording_stub(
        dir.path(),
        &format!(
            "printf '{{\"status\":\"success\",\"outputs\":{{\"verdict\":\"ok\"}}}}' > \"$APB_STATUS_FILE\"; {OK}"
        ),
    );
    assert_eq!(
        run_with_stub(dir.path(), "one", &stub, None),
        RunStatus::Succeeded
    );
    let events = read_all(&run_dir(dir.path())).unwrap();
    let missing: Vec<Vec<String>> = events
        .iter()
        .filter_map(|e| match &e.payload {
            EventPayload::OutputFieldsMissing { node, fields } if node == "probe" => {
                Some(fields.clone())
            }
            _ => None,
        })
        .collect();
    assert_eq!(missing, vec![vec!["working_tree".to_string()]]);
}
