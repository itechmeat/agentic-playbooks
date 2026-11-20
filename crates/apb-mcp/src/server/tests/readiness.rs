//! Readiness on the MCP start tool: an unattended `playbook_run` of a
//! definition that is not ready to run alone answers its readiness block,
//! assessed on the version the start loaded, and the tool takes no
//! probation waiver (a waiver is a person's typed confirmation at a
//! terminal only).

use super::*;

const BARE: &str = "schema: 2\nid: bare\nname: Bare\nversion: 1.0.0\nnodes:\n  - { id: start, type: start }\n  - { id: done, type: finish, outcome: success }\nedges:\n  - { from: start, to: done }\n";

fn args(unattended: Option<bool>) -> PlaybookRunArgs {
    PlaybookRunArgs {
        deadline: None,
        start_key: None,
        start_key_reuse: None,
        id: "bare".into(),
        version: None,
        params: BTreeMap::new(),
        instruction: None,
        supervise: None,
        background: None,
        acknowledge_untrusted: Some(true),
        confirm_irreversible: None,
        scope: None,
        continued_from: None,
        worktree: None,
        execution: None,
        budget: None,
        remember: None,
        notify: None,
        launch_version: None,
        launch_choice: None,
        unattended,
        confirm_launch: None,
        external_data: None,
    }
}

#[tokio::test]
async fn an_unready_unattended_start_answers_its_readiness() {
    let _l = CROSS_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let _cfg = ConfigDirGuard::new();
    let proj = tempfile::tempdir().unwrap();
    let root = proj.path();
    apb_core::registry::init_project(root).unwrap();
    let vdir = root.join(".apb/playbooks/bare/1.0.0");
    fs::create_dir_all(&vdir).unwrap();
    fs::write(vdir.join("playbook.yaml"), BARE).unwrap();
    fs::write(root.join(".apb/playbooks/bare/current"), "1.0.0").unwrap();
    let server = WfMcp::new(root.to_path_buf());

    let out: serde_json::Value = serde_json::from_str(&result_text(
        &server.playbook_run(Parameters(args(Some(true)))).await,
    ))
    .unwrap();
    assert!(out["run_id"].is_string(), "{out}");
    assert_eq!(out["readiness"]["ready"], false, "{out}");
    let items = out["readiness"]["items"].as_array().unwrap();
    assert!(
        items
            .iter()
            .any(|i| i["code"] == "budget" && i["ok"] == false),
        "{out}"
    );
    // An attended start answers no readiness block.
    let out: serde_json::Value = serde_json::from_str(&result_text(
        &server.playbook_run(Parameters(args(None))).await,
    ))
    .unwrap();
    assert!(out["run_id"].is_string(), "{out}");
    assert!(out.get("readiness").is_none(), "{out}");
}

/// The tool's input schema has no waiver argument.
#[test]
fn the_start_tool_takes_no_probation_waiver() {
    let schema = serde_json::to_string(&schemars::schema_for!(PlaybookRunArgs)).unwrap();
    assert!(schema.contains("unattended"), "{schema}");
    assert!(!schema.contains("waive"), "{schema}");
}
