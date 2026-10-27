//! `playbook_memory`: remembered run input per project, through the tool
//! layer the MCP server and the dashboard share.

use std::collections::BTreeMap;
use std::path::Path;

use apb_core::registry::init_project;
use apb_core::run_memory::{RememberInput, What};
use apb_mcp::tools::memory::{MemoryAction, MemoryTarget, playbook_memory};
use serde_json::{Value, json};

const PLAYBOOK: &str = "schema: 2\nid: __ID__\nname: n\nversion: 1.0.0\nparams:\n  - { name: channel, type: enum, options: [beta, stable] }\n  - { name: token, type: text, secret: true }\nnodes:\n  - { id: s, type: start }\nedges: []\n";

fn seed(parent: &Path, id: &str) {
    let vdir = parent.join("playbooks").join(id).join("1.0.0");
    std::fs::create_dir_all(&vdir).unwrap();
    std::fs::write(vdir.join("playbook.yaml"), PLAYBOOK.replace("__ID__", id)).unwrap();
    std::fs::write(parent.join("playbooks").join(id).join("current"), "1.0.0").unwrap();
}

fn target<'a>(id: &'a str, scope: &'a str) -> MemoryTarget<'a> {
    MemoryTarget {
        id,
        scope,
        version: None,
    }
}

fn set(params: &[(&str, &str)], instruction: Option<&str>) -> MemoryAction {
    MemoryAction::Set(RememberInput {
        params: Some(
            params
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect::<BTreeMap<_, _>>(),
        ),
        instruction: instruction.map(str::to_string),
    })
}

fn get(root: &Path, id: &str, scope: &str) -> Value {
    playbook_memory(root, &target(id, scope), MemoryAction::Get).unwrap()
}

#[test]
fn set_get_and_clear_with_secrets_refused() {
    let _cfg = crate::common::config_sandbox();
    let root = tempfile::tempdir().unwrap();
    init_project(root.path()).unwrap();
    seed(&root.path().join(".apb"), "p");

    let stored = playbook_memory(
        root.path(),
        &target("p", "project"),
        set(
            &[("channel", "beta"), ("token", "x")],
            Some("since the tag"),
        ),
    )
    .unwrap();
    assert_eq!(stored["stored"], json!(["channel", "instruction"]));
    assert_eq!(
        stored["refused"],
        json!([{ "name": "token", "reason": "secret_param" }])
    );

    let got = get(root.path(), "p", "project");
    assert_eq!(got["instruction"], "since the tag");
    assert_eq!(got["values"][0]["name"], "channel");
    assert_eq!(
        got["missing"],
        json!(["token"]),
        "a secret is always asked for"
    );

    playbook_memory(
        root.path(),
        &target("p", "project"),
        MemoryAction::Clear(What::All),
    )
    .unwrap();
    let got = get(root.path(), "p", "project");
    assert!(got["instruction"].is_null());
    assert_eq!(got["values"], json!([]));
}

/// A global playbook remembers per project: what project A keeps is not
/// what project B starts with.
#[test]
fn a_global_playbook_remembers_per_project() {
    let _cfg = crate::common::config_sandbox();
    let global = apb_core::store::global_playbooks_parent().unwrap();
    seed(&global, "shared");
    let a = tempfile::tempdir().unwrap();
    let b = tempfile::tempdir().unwrap();
    init_project(a.path()).unwrap();
    init_project(b.path()).unwrap();

    playbook_memory(
        a.path(),
        &target("shared", "global"),
        set(&[("channel", "stable")], None),
    )
    .unwrap();
    assert_eq!(
        get(a.path(), "shared", "global")["values"][0]["value"],
        "stable"
    );
    assert_eq!(get(b.path(), "shared", "global")["values"], json!([]));
    assert!(
        playbook_memory(a.path(), &target("shared", "project"), MemoryAction::Get).is_err(),
        "the project scope has no playbook of that id"
    );
}

/// Memory of a playbook that no longer loads (deleted, broken YAML) can
/// still be forgotten: clearing needs no definition.
#[test]
fn memory_of_a_playbook_that_is_gone_can_still_be_cleared() {
    let _cfg = crate::common::config_sandbox();
    let root = tempfile::tempdir().unwrap();
    init_project(root.path()).unwrap();
    seed(&root.path().join(".apb"), "p");
    playbook_memory(
        root.path(),
        &target("p", "project"),
        set(&[("channel", "beta")], Some("keep this")),
    )
    .unwrap();
    std::fs::remove_dir_all(root.path().join(".apb/playbooks/p")).unwrap();

    let out = playbook_memory(
        root.path(),
        &target("p", "project"),
        MemoryAction::Clear(What::All),
    )
    .unwrap();
    assert_eq!(out["cleared"], "all");
    assert!(!root.path().join(".apb/memory/project/p.json").exists());
}

/// A set is checked against the current version, whatever version the
/// caller names: a later read of the current version sees the same params.
#[test]
fn a_set_is_checked_against_the_current_version() {
    let _cfg = crate::common::config_sandbox();
    let root = tempfile::tempdir().unwrap();
    init_project(root.path()).unwrap();
    let parent = root.path().join(".apb");
    seed(&parent, "p");
    let v2 = parent.join("playbooks/p/2.0.0");
    std::fs::create_dir_all(&v2).unwrap();
    std::fs::write(
        v2.join("playbook.yaml"),
        PLAYBOOK
            .replace("__ID__", "p")
            .replace("1.0.0", "2.0.0")
            .replace("options: [beta, stable]", "options: [beta, stable, edge]"),
    )
    .unwrap();
    std::fs::write(parent.join("playbooks/p/current"), "2.0.0").unwrap();

    let old = MemoryTarget {
        id: "p",
        scope: "project",
        version: Some("1.0.0"),
    };
    let stored = playbook_memory(root.path(), &old, set(&[("channel", "edge")], None)).unwrap();
    assert_eq!(stored["stored"], json!(["channel"]));
    assert_eq!(get(root.path(), "p", "project")["schema_changed"], false);
}
