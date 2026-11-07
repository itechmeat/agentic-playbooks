//! Numeric thresholds on an `output_field` edge (spec node-contracts D2):
//! a script's JSON output routes by number, with a numeric string reading
//! as its number, and an unreadable value falling through to the fallback.

use std::fs;
use std::path::Path;

use apb_core::registry::init_project;
use apb_engine::diagnostics::{self, Diagnostic};
use apb_engine::event::{EventPayload, read_all};
use apb_engine::state::RunStatus;

use crate::common;
use crate::token_economy_test::seed_playbook;

/// The single run directory under `root`.
fn run_dir(root: &Path) -> std::path::PathBuf {
    fs::read_dir(root.join(".apb/runs"))
        .unwrap()
        .filter_map(|e| e.ok().map(|e| e.path()))
        .find(|p| p.is_dir())
        .expect("one run")
}

fn seed(root: &Path, stdout: &str) {
    init_project(root).unwrap();
    seed_playbook(
        root,
        "of",
        r#"schema: 2
id: of
name: OF
version: 1.0.0
defaults: { profile: main }
nodes:
  - { id: start, type: start }
  - { id: score, type: script, script: scripts/score.sh, runner: sh, outputs: { fields: [coverage] } }
  - { id: low, type: script, script: scripts/low.sh, runner: sh }
  - { id: high, type: script, script: scripts/high.sh, runner: sh }
  - { id: other, type: script, script: scripts/other.sh, runner: sh }
  - { id: done, type: finish, outcome: success }
edges:
  - { from: start, to: score }
  - { from: score, to: low, condition: { type: output_field, node: score, field: coverage, lt: 80 } }
  - { from: score, to: high, condition: { type: output_field, node: score, field: coverage, gte: 80 } }
  - { from: score, to: other, fallback: true }
  - { from: low, to: done }
  - { from: high, to: done }
  - { from: other, to: done }
"#,
    );
    let vdir = root.join(".apb/playbooks/of/1.0.0/scripts");
    fs::create_dir_all(&vdir).unwrap();
    fs::write(vdir.join("score.sh"), format!("echo '{stdout}'\n")).unwrap();
    for name in ["low.sh", "high.sh", "other.sh"] {
        fs::write(vdir.join(name), "echo route\n").unwrap();
    }
    common::seed_main(root);
}

fn routed(root: &Path) -> Vec<String> {
    read_all(&run_dir(root))
        .unwrap()
        .iter()
        .filter_map(|e| match &e.payload {
            EventPayload::NodeFinished {
                node,
                output,
                status,
                ..
            } if *status == "succeeded" && output.trim() == "route" => Some(node.clone()),
            _ => None,
        })
        .collect()
}

#[test]
fn a_number_a_numeric_string_and_a_word_route_by_threshold() {
    let _env = common::env_lock();

    // 72.5 is below 80.
    let dir = tempfile::tempdir().unwrap();
    seed(dir.path(), r#"{"coverage": 72.5}"#);
    let res = apb_engine::scheduler::run(dir.path(), "of", None, Default::default()).unwrap();
    assert_eq!(res.outcome, RunStatus::Succeeded);
    assert!(routed(dir.path()).contains(&"low".to_string()));

    // "91" reads as the number 91: at least 80.
    let dir = tempfile::tempdir().unwrap();
    seed(dir.path(), r#"{"coverage": "91"}"#);
    assert_eq!(
        apb_engine::scheduler::run(dir.path(), "of", None, Default::default())
            .unwrap()
            .outcome,
        RunStatus::Succeeded
    );
    assert!(routed(dir.path()).contains(&"high".to_string()));

    // "n/a" is not a number, and neither is " 80" (a numeric string must
    // be exactly a JSON number): neither threshold matches, the fallback
    // takes it. The threshold edge's diagnostic names the text it read and
    // the number routing made of it, read from the raw value, not the text.
    for word in ["n/a", " 80"] {
        let dir = tempfile::tempdir().unwrap();
        seed(dir.path(), &format!(r#"{{"coverage": "{word}"}}"#));
        assert_eq!(
            apb_engine::scheduler::run(dir.path(), "of", None, Default::default())
                .unwrap()
                .outcome,
            RunStatus::Succeeded
        );
        let taken = routed(dir.path());
        assert!(taken.contains(&"other".to_string()), "{word:?}: {taken:?}");
        assert!(!taken.contains(&"low".to_string()), "{word:?}: {taken:?}");
        assert!(!taken.contains(&"high".to_string()), "{word:?}: {taken:?}");
        let routes: Vec<_> = diagnostics::read(&run_dir(dir.path()))
            .into_iter()
            .filter_map(|d| match d {
                Diagnostic::Route(r) => Some(r),
                _ => None,
            })
            .collect();
        let threshold = routes
            .iter()
            .flat_map(|r| r.checks.iter())
            .find(|c| c.observed.is_some())
            .expect("a route record with an observed value");
        assert_eq!(
            threshold.observed,
            Some(serde_json::json!({"value": word, "number": null})),
            "{word:?}"
        );
    }
}
