//! `apb corrections`: the list in text and JSON, the draft a proposed
//! entry offers (show), and a person's fixed, dismiss and reopen, with the
//! exit codes a refusal answers with. The ledger is hand-built with the
//! shared signed store, signed under the sandbox's key so the spawned
//! `apb` reads it.

use predicates::prelude::*;
use std::fs;
use std::path::Path;

use crate::common::{apb, sandbox_config_dir};

const YAML: &str = "schema: 2\nid: fix-build\nname: Fix build\nversion: 1.0.0\nnodes:\n  \
- { id: start, type: start }\n  \
- { id: fix, type: agent_task, prompt: \"Fix the build\", profile: main }\n  \
- { id: done, type: finish, outcome: success }\n\
edges:\n  - { from: start, to: fix }\n  - { from: fix, to: done }\n";

const KEY: &str = "t:0123456789abcdef";

fn sighting(
    key: &str,
    text: Option<&str>,
    run_id: &str,
    seq: u64,
    at_ms: u64,
) -> apb_core::corrections::Sighting {
    apb_core::corrections::Sighting {
        kind: apb_core::corrections::CorrectionKind::ReviewNote,
        node: Some("fix".into()),
        gate: Some("review".into()),
        key: key.to_string(),
        text: text.map(str::to_string),
        run_id: run_id.into(),
        seq,
        at_ms,
        version: "1.0.0".into(),
        via_supervisor: false,
    }
}

/// Seeds one proposed entry (two runs of one wording) and one open entry
/// whose text was withheld (a fingerprint key, no text); returns their ids.
fn seed_ledger(root: &Path) -> (String, String) {
    use apb_core::corrections::{CorrectionFile, ObservedRun, entry_id};
    use apb_core::signed_store::{self, StoreError};
    let worded = KEY;
    let withheld = "f:0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef0123";
    signed_store::update(
        root,
        apb_core::scope::Scope::Project,
        "fix-build",
        |f: &mut CorrectionFile| {
            f.observe(
                ObservedRun {
                    run_id: "fix-build-1".into(),
                    version: "1.0.0".into(),
                    started_ms: 1000,
                },
                vec![sighting(
                    worded,
                    Some("quote the version"),
                    "fix-build-1",
                    5,
                    1100,
                )],
                1100,
            );
            f.observe(
                ObservedRun {
                    run_id: "fix-build-2".into(),
                    version: "1.0.0".into(),
                    started_ms: 2000,
                },
                vec![
                    sighting(worded, Some("quote the version"), "fix-build-2", 6, 2100),
                    sighting(withheld, None, "fix-build-2", 7, 2200),
                ],
                2200,
            );
            Ok::<_, StoreError>(((), true))
        },
    )
    .unwrap();
    (entry_id(worded), entry_id(withheld))
}

/// A project with `fix-build` and a hand-built corrections ledger. The
/// ledger's MAC must verify under the config dir the spawned `apb` reads
/// (`sandbox_config_dir`), so the test process signs with the same key.
fn project() -> (tempfile::TempDir, String, String) {
    let sandbox = sandbox_config_dir();
    fs::write(sandbox.join("run-origin.key"), "k".repeat(64)).unwrap();
    let own = apb_core::config::config_dir().expect("the test process has a config dir");
    fs::create_dir_all(&own).unwrap();
    fs::write(own.join("run-origin.key"), "k".repeat(64)).unwrap();

    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    apb_core::registry::init_project(root).unwrap();
    let v = root.join(".apb/playbooks/fix-build/1.0.0");
    fs::create_dir_all(&v).unwrap();
    fs::write(v.join("playbook.yaml"), YAML).unwrap();
    fs::write(root.join(".apb/playbooks/fix-build/current"), "1.0.0").unwrap();
    let (worded, withheld) = seed_ledger(root);
    (dir, worded, withheld)
}

#[test]
fn corrections_are_listed_shown_and_decided() {
    let (dir, id, withheld) = project();
    let root = dir.path();

    // The list, as text: the proposed entry with its gate, runs and text,
    // and the withheld one with the marker instead of a text.
    apb()
        .args(["corrections", "fix-build"])
        .current_dir(root)
        .assert()
        .success()
        .stdout(predicate::str::contains(format!(
            "{id} proposed review_note at node fix (gate review): 2 runs, last fix-build-2: \"quote the version\""
        )))
        .stdout(predicate::str::contains(format!(
            "{withheld} open review_note at node fix (gate review): 1 run, last fix-build-2: (text withheld)"
        )));

    // --json: the same view value MCP serves.
    let out = apb()
        .args(["corrections", "fix-build", "--json"])
        .current_dir(root)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["playbook"], "fix-build");
    assert_eq!(v["scope"], "project");
    assert!(v["signed"].as_bool().unwrap());
    assert_eq!(v["entries"].as_array().unwrap().len(), 2);
    let e = v["entries"]
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["correction"]["id"] == id.as_str())
        .unwrap();
    assert_eq!(e["correction"]["status"], "proposed");
    assert_eq!(e["draft"]["node"], "fix");
    assert!(e.get("rate").is_none(), "no fix is marked, so no rate");

    // show: the draft as data, with its node.
    apb()
        .args(["corrections", "fix-build", "show", &id])
        .current_dir(root)
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "draft: A person corrected this step in earlier runs: quote the version",
        ))
        .stdout(predicate::str::contains("node: fix"));
    // The withheld text offers no draft, and says so.
    apb()
        .args(["corrections", "fix-build", "show", &withheld])
        .current_dir(root)
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "no draft (the node is unknown, ambiguous, or the text was withheld)",
        ));

    // fixed marks a version of the store as carrying the fix.
    apb()
        .args([
            "corrections",
            "fix-build",
            "fixed",
            &id,
            "--version",
            "1.0.0",
            "--reason",
            "rewrote the prompt",
        ])
        .current_dir(root)
        .assert()
        .success()
        .stdout(predicate::str::contains(format!("{id}: fixed in 1.0.0")));
    let out = apb()
        .args(["corrections", "fix-build", "--json"])
        .current_dir(root)
        .output()
        .unwrap();
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    let e = v["entries"]
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["correction"]["id"] == id.as_str())
        .unwrap();
    assert_eq!(e["correction"]["status"], "fixed");
    assert_eq!(e["correction"]["fix"]["version"], "1.0.0");
    assert_eq!(e["correction"]["fix"]["marked"]["by"], "cli");
    assert_eq!(
        e["correction"]["fix"]["marked"]["reason"],
        "rewrote the prompt"
    );
    assert_eq!(e["rate"]["base_runs"], 2, "the base counts at marking time");
    assert_eq!(e["rate"]["base_hits"], 2);
    assert!(e.get("draft").is_none(), "a fixed entry offers no draft");

    // dismiss and reopen keep their decision.
    apb()
        .args([
            "corrections",
            "fix-build",
            "dismiss",
            &id,
            "--reason",
            "not a problem",
        ])
        .current_dir(root)
        .assert()
        .success()
        .stdout(predicate::str::contains(format!("{id}: dismissed")));
    let out = apb()
        .args(["corrections", "fix-build", "--json"])
        .current_dir(root)
        .output()
        .unwrap();
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    let e = v["entries"]
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["correction"]["id"] == id.as_str())
        .unwrap();
    assert_eq!(e["correction"]["decided"]["by"], "cli");
    assert_eq!(e["correction"]["decided"]["reason"], "not a problem");

    apb()
        .args(["corrections", "fix-build", "reopen", &id])
        .current_dir(root)
        .assert()
        .success()
        .stdout(predicate::str::contains(format!("{id}: proposed")));
}

#[test]
fn a_refused_decision_exits_2_and_a_run_step_may_not_read() {
    let (dir, id, _withheld) = project();
    let root = dir.path();
    // A fix names a version the store has.
    apb()
        .args([
            "corrections",
            "fix-build",
            "fixed",
            &id,
            "--version",
            "9.9.9",
        ])
        .current_dir(root)
        .assert()
        .code(2)
        .stderr(predicate::str::contains("unknown_version"));
    // An unknown entry.
    apb()
        .args(["corrections", "fix-build", "dismiss", "c-000000000000"])
        .current_dir(root)
        .assert()
        .code(2)
        .stderr(predicate::str::contains("no correction"));
    // A run step reads and decides nothing.
    apb()
        .args(["corrections", "fix-build"])
        .env("APB_RUN_ID", "fix-build-3")
        .current_dir(root)
        .assert()
        .code(2)
        .stderr(predicate::str::contains("run step"));
    apb()
        .args(["corrections", "fix-build", "dismiss", &id])
        .env("APB_RUN_ID", "fix-build-3")
        .current_dir(root)
        .assert()
        .code(2)
        .stderr(predicate::str::contains("run step"));
}
