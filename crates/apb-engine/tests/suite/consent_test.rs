//! The `irreversible` consent gate (0.24.0): a run whose tree declares
//! `irreversible` starts only with a consent, which the manifest records, a
//! sub-playbook inherits and a resume keeps.

use apb_core::registry::init_project;
use apb_engine::consent::RunConsent;
use apb_engine::event::{EventPayload, read_all};
use apb_engine::scheduler::{RunOptions, resume, run};
use apb_engine::state::RunStatus;
use std::fs;
use std::path::Path;

fn playbook(id: &str, head: &str, nodes: &str, edges: &str) -> String {
    format!(
        "schema: 1\nid: {id}\nname: {id}\nversion: 1.0.0\n{head}nodes:\n  - {{ id: start, type: start }}\n{nodes}  - {{ id: done, type: finish, outcome: success }}\nedges:\n{edges}"
    )
}

/// start -> a (a prompt node, no agent) -> done, with `head` above `nodes:`
/// and `a_extra` inside node `a`.
fn linear(id: &str, head: &str, a_extra: &str) -> String {
    playbook(
        id,
        head,
        &format!("  - {{ id: a, type: prompt, prompt: \"x\"{a_extra} }}\n"),
        "  - { from: start, to: a }\n  - { from: a, to: done }\n",
    )
}

fn seed(root: &Path, id: &str, yaml: &str) {
    let _ = init_project(root);
    let vdir = root.join(".apb/playbooks").join(id).join("1.0.0");
    fs::create_dir_all(&vdir).unwrap();
    fs::write(vdir.join("playbook.yaml"), yaml).unwrap();
    fs::write(
        root.join(".apb/playbooks").join(id).join("current"),
        "1.0.0",
    )
    .unwrap();
}

fn with_consent(by: &str) -> RunOptions {
    RunOptions {
        consent: Some(RunConsent::irreversible(by)),
        ..Default::default()
    }
}

fn manifest_consent(root: &Path, run_id: &str) -> Option<RunConsent> {
    apb_engine::manifest::read(&root.join(".apb/runs").join(run_id))
        .unwrap()
        .and_then(|m| m.consent)
}

fn runs_dir_is_empty(root: &Path) -> bool {
    fs::read_dir(root.join(".apb/runs"))
        .map(|mut d| d.next().is_none())
        .unwrap_or(true)
}

#[test]
fn an_irreversible_playbook_is_refused_without_consent_and_leaves_no_run() {
    let dir = tempfile::tempdir().unwrap();
    seed(
        dir.path(),
        "rel",
        &linear("rel", "effects: [irreversible]\n", ""),
    );
    let err = run(dir.path(), "rel", None, RunOptions::default())
        .expect_err("no consent, no run")
        .to_string();
    assert!(err.contains("irreversible_requires_confirmation"), "{err}");
    assert!(
        err.contains("playbook `rel` has irreversible effects (playbook)"),
        "{err}"
    );
    assert!(runs_dir_is_empty(dir.path()), "a refusal writes no run");
}

#[test]
fn a_node_level_irreversible_effect_needs_consent_too() {
    let dir = tempfile::tempdir().unwrap();
    seed(
        dir.path(),
        "rel",
        &linear("rel", "", ", effects: [irreversible]"),
    );
    let err = run(dir.path(), "rel", None, RunOptions::default())
        .expect_err("no consent, no run")
        .to_string();
    assert!(err.contains("(node a)"), "{err}");
}

#[test]
fn a_consented_run_records_the_consent_in_the_manifest() {
    let dir = tempfile::tempdir().unwrap();
    seed(
        dir.path(),
        "rel",
        &linear("rel", "effects: [irreversible]\n", ""),
    );
    let res = run(dir.path(), "rel", None, with_consent("cli")).unwrap();
    assert_eq!(res.outcome, RunStatus::Succeeded);
    assert_eq!(
        manifest_consent(dir.path(), &res.run_id),
        Some(RunConsent::irreversible("cli"))
    );
}

#[test]
fn a_consent_is_not_recorded_for_a_run_that_needs_none() {
    let dir = tempfile::tempdir().unwrap();
    seed(dir.path(), "plain", &linear("plain", "", ""));
    let res = run(dir.path(), "plain", None, with_consent("cli")).unwrap();
    assert_eq!(res.outcome, RunStatus::Succeeded);
    assert_eq!(manifest_consent(dir.path(), &res.run_id), None);
}

#[test]
fn a_resume_keeps_the_consent_and_asks_for_none() {
    let dir = tempfile::tempdir().unwrap();
    seed(
        dir.path(),
        "rel",
        &linear("rel", "effects: [irreversible]\n", ""),
    );
    let res = run(dir.path(), "rel", None, with_consent("mcp:host")).unwrap();
    let again = resume(dir.path(), &res.run_id, Some("a")).unwrap();
    assert_eq!(again.outcome, RunStatus::Succeeded);
    assert_eq!(
        manifest_consent(dir.path(), &res.run_id),
        Some(RunConsent::irreversible("mcp:host"))
    );
}

fn parent_of(child: &str) -> String {
    playbook(
        "parent",
        "",
        &format!("  - {{ id: sub, type: playbook, playbook: {child} }}\n"),
        "  - { from: start, to: sub }\n  - { from: sub, to: done }\n",
    )
}

#[test]
fn an_irreversible_sub_playbook_makes_the_parent_need_consent() {
    let dir = tempfile::tempdir().unwrap();
    seed(
        dir.path(),
        "child",
        &linear("child", "effects: [irreversible]\n", ""),
    );
    seed(dir.path(), "parent", &parent_of("child"));
    let err = run(dir.path(), "parent", None, RunOptions::default())
        .expect_err("the tree is irreversible")
        .to_string();
    assert!(err.contains("(sub-playbook node sub)"), "{err}");
    assert!(runs_dir_is_empty(dir.path()));
}

#[test]
fn a_sub_playbook_inherits_the_parents_consent() {
    let dir = tempfile::tempdir().unwrap();
    seed(
        dir.path(),
        "child",
        &linear("child", "effects: [irreversible]\n", ""),
    );
    seed(dir.path(), "parent", &parent_of("child"));
    let res = run(dir.path(), "parent", None, with_consent("dashboard")).unwrap();
    assert_eq!(res.outcome, RunStatus::Succeeded);
    let events = read_all(&dir.path().join(".apb/runs").join(&res.run_id)).unwrap();
    let child_run = events
        .iter()
        .find_map(|e| match &e.payload {
            EventPayload::ChildRunStarted { run_id, .. } => Some(run_id.clone()),
            _ => None,
        })
        .expect("the child ran");
    assert_eq!(
        manifest_consent(dir.path(), &child_run),
        Some(RunConsent {
            irreversible: true,
            by: "dashboard".into(),
            inherited_from: Some(res.run_id.clone()),
        })
    );
}

#[test]
fn the_gate_refuses_with_the_sources_and_accepts_a_consent() {
    let dir = tempfile::tempdir().unwrap();
    seed(
        dir.path(),
        "child",
        &linear("child", "effects: [irreversible]\n", ""),
    );
    seed(dir.path(), "parent", &parent_of("child"));
    let wref = apb_core::scope::PlaybookRef {
        origin: apb_core::scope::Origin::Project { workspace_id: None },
        id: "parent".into(),
        version: None,
    };
    let permit = apb_engine::gate::check_run(dir.path(), &wref, true, false).unwrap();
    let refusal = permit.consent_refusal(None).expect_err("needs consent");
    assert_eq!(refusal["policy"], "irreversible_requires_confirmation");
    assert_eq!(
        refusal["sources"],
        serde_json::json!(["sub-playbook node sub"])
    );
    assert!(
        permit
            .consent_refusal(Some(&RunConsent::irreversible("cli")))
            .is_ok()
    );
    // A gated run (pins from the permit) is checked along the pins too.
    let mut opts = RunOptions::default();
    permit.apply(&mut opts);
    let err = run(dir.path(), "parent", None, opts).expect_err("pinned tree");
    assert!(err.to_string().contains("(sub-playbook node sub)"), "{err}");
}
