//! `apb runs --playbook --scope --field` (U45): the same runs as MCP
//! `runs_list`, a footer for the runs without search fields, and no
//! withheld value in any output.

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

use predicates::prelude::*;
use serde_json::{Value, json};

use crate::common::apb;

const SEARCH: &str = "schema: 2\nid: intake\nname: Intake\nversion: 2.0.0\nparams:\n  - { name: customer, type: text, search: true }\nnodes:\n  - { id: start, type: start }\n  - { id: done, type: finish, outcome: success }\nedges:\n  - { from: start, to: done }\n";
const PLAIN: &str = "schema: 2\nid: intake\nname: Intake\nversion: 1.0.0\nparams:\n  - { name: customer, type: text }\nnodes:\n  - { id: start, type: start }\n  - { id: done, type: finish, outcome: success }\nedges:\n  - { from: start, to: done }\n";
const TOKEN: &str = "ghp_abcdefghijklmnopqrstuvwxyz0123456789";

fn run(root: &Path, id: &str, ts: u64, def: &str, customer: &str, origin: &str) {
    let dir = root.join(".apb/runs").join(id);
    fs::create_dir_all(&dir).unwrap();
    let digest = apb_engine::run_definitions::store(&dir, def.as_bytes()).unwrap();
    fs::write(dir.join("playbook.yaml"), def).unwrap();
    let lines = [
        json!({"seq": 1, "ts": ts, "type": "run_started", "playbook": "intake", "version": "1.0.0", "definition": digest}),
        json!({"seq": 2, "ts": ts, "type": "run_provenance", "origin": origin}),
        json!({"seq": 3, "ts": ts + 1, "type": "run_finished", "outcome": "succeeded"}),
    ];
    let body: String = lines.iter().map(|l| format!("{l}\n")).collect();
    fs::write(dir.join("events.jsonl"), body).unwrap();
    let cfg = apb_engine::run_config::RunConfig {
        params: BTreeMap::from([("customer".to_string(), customer.to_string())]),
        ..Default::default()
    };
    apb_engine::run_config::write_run_config(&dir, &cfg).unwrap();
}

fn project() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    apb_core::registry::init_project(root).unwrap();
    run(root, "acme-1", 1, SEARCH, "Acme", "project");
    run(root, "acme-g", 2, SEARCH, "Acme", "global");
    run(root, "other", 3, SEARCH, "Globex", "project");
    run(root, "plain", 4, PLAIN, "Acme", "project");
    run(root, "cred", 5, SEARCH, TOKEN, "project");
    dir
}

fn ids(v: &Value) -> Vec<String> {
    v["runs"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["run_id"].as_str().unwrap().to_string())
        .collect()
}

#[test]
fn a_field_filter_lists_the_same_runs_as_runs_list() {
    let dir = project();
    let out = apb()
        .args([
            "runs",
            "--playbook",
            "intake",
            "--field",
            "customer=acme",
            "--json",
        ])
        .current_dir(dir.path())
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let cli: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(ids(&cli), ["acme-g", "acme-1"]);
    assert_eq!(cli["not_indexed"], 1);
    let filter = apb_mcp::tools::run::run_filter(
        Some("intake".into()),
        None,
        &["customer=acme".to_string()],
        false,
    )
    .unwrap()
    .unwrap();
    let mcp = apb_mcp::tools::run::runs_list_filtered(dir.path(), &filter).unwrap();
    assert_eq!(ids(&mcp), ["acme-g", "acme-1"]);
    assert_eq!(mcp["withheld"], 1, "the credential run is never probed");
    // `--scope global` narrows to the global playbook's run.
    let out = apb()
        .args([
            "runs",
            "--field",
            "customer=Acme",
            "--scope",
            "global",
            "--json",
        ])
        .current_dir(dir.path())
        .output()
        .unwrap();
    let v: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(ids(&v), ["acme-g"]);
    assert_eq!(v["runs"][0]["scope"], "global");
}

#[test]
fn the_text_listing_names_its_fields_and_the_runs_it_could_not_search() {
    let dir = project();
    apb()
        .args(["runs", "--field", "customer=ACME"])
        .current_dir(dir.path())
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "acme-1\tintake\tsucceeded\tproject\tcustomer=Acme",
        ))
        .stdout(predicate::str::contains("1 run has no search fields"))
        .stdout(predicate::str::contains(
            "1 run left out: a searched value is withheld",
        ))
        .stdout(predicate::str::contains("plain").not());
}

#[test]
fn a_withheld_value_is_in_no_output() {
    let dir = project();
    for args in [
        vec!["runs", "--playbook", "intake"],
        vec!["runs", "--playbook", "intake", "--json"],
        vec!["runs"],
    ] {
        let out = apb().args(&args).current_dir(dir.path()).output().unwrap();
        let text = String::from_utf8_lossy(&out.stdout);
        assert!(!text.contains(TOKEN), "{args:?}: {text}");
    }
    apb()
        .args(["runs", "--playbook", "intake"])
        .current_dir(dir.path())
        .assert()
        .stdout(predicate::str::contains(
            "cred\tintake\tsucceeded\tproject\tcustomer=withheld",
        ));
}

#[test]
fn bad_terms_and_scopes_are_refused() {
    let dir = project();
    apb()
        .args(["runs", "--field", "customer"])
        .current_dir(dir.path())
        .assert()
        .code(2)
        .stderr(predicate::str::contains("name=value"));
    apb()
        .args(["runs", "--scope", "elsewhere"])
        .current_dir(dir.path())
        .assert()
        .failure();
    let nine: Vec<String> = (0..9)
        .flat_map(|i| ["--field".to_string(), format!("p{i}=v")])
        .collect();
    apb()
        .arg("runs")
        .args(&nine)
        .current_dir(dir.path())
        .assert()
        .code(2)
        .stderr(predicate::str::contains("at most 8"));
}

#[test]
fn the_plain_listing_is_unchanged() {
    let dir = project();
    let out = apb()
        .args(["runs"])
        .current_dir(dir.path())
        .output()
        .unwrap();
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(text.contains("acme-1\tintake\tsucceeded\n"), "{text}");
    assert!(!text.contains("search fields"), "{text}");
}

/// A search value that carries ESC or a newline reaches the terminal only
/// through the sanitizer: no escape byte, and no forged extra row.
#[test]
fn a_value_with_control_characters_cannot_forge_a_row() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    apb_core::registry::init_project(root).unwrap();
    run(
        root,
        "esc-1",
        1,
        SEARCH,
        "ac\u{1b}me\nINJECTED-ROW",
        "project",
    );
    let out = apb()
        .args(["runs", "--playbook", "intake"])
        .current_dir(root)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(!text.contains('\u{1b}'), "{text:?}");
    assert!(text.contains("ac me INJECTED-ROW"), "{text:?}");
    assert_eq!(
        text.lines().filter(|l| l.contains("intake")).count(),
        1,
        "the newline must not forge a second row: {text:?}"
    );
}
