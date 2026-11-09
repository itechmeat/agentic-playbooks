//! External data scenario runs on the surfaces that hand the run to an
//! `apb __scenario-run` child: MCP `playbook_scenario_run` through `apb
//! mcp`, and the dashboard's `POST /api/playbooks/{id}/scenario-runs`.
//! Each flagged request leaves a scenario run whose manifest carries the
//! flag; an unflagged one leaves none.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::ChildStdin;

use crate::mcp_cli_test::{request, start_mcp, tool_json};

const PLAYBOOK: &str = "schema: 2\nid: ext\nname: ext\nversion: 1.0.0\ndefaults: { profile: x }\nnodes:\n  - { id: start, type: start }\n  - { id: work, type: agent_task, prompt: work }\n  - { id: done, type: finish, outcome: success }\nedges:\n  - { from: start, to: work }\n  - { from: work, to: done }\n";

const GREEN: &str = "schema: 1\nid: green\nanswers:\n  work:\n    - { visit: 1, attempts: [ { status: succeeded, output: ok } ] }\nexpect: { finish: done }\n";

/// A project holding playbook `ext` with its scenario `green`.
fn project(base: &Path) -> PathBuf {
    let project = base.join("project");
    apb_core::registry::init_project(&project).unwrap();
    let pb = project.join(".apb/playbooks/ext");
    fs::create_dir_all(pb.join("1.0.0")).unwrap();
    fs::write(pb.join("1.0.0/playbook.yaml"), PLAYBOOK).unwrap();
    fs::write(pb.join("current"), "1.0.0").unwrap();
    fs::create_dir_all(pb.join("evals/scenarios")).unwrap();
    fs::write(pb.join("evals/scenarios/green.yaml"), GREEN).unwrap();
    project
}

/// Whether the scenario run `report` names carries the flag.
fn flagged(project: &Path, report: &serde_json::Value) -> bool {
    let run = report["run_id"]
        .as_str()
        .unwrap_or_else(|| panic!("no run id: {report}"));
    let dir = project.join(".apb/scenario-runs").join(run);
    let m = apb_engine::manifest::read(&dir).unwrap().unwrap();
    assert!(matches!(
        m.origin,
        Some(apb_engine::manifest::RunOrigin::Scenario { .. })
    ));
    m.external_data
}

#[test]
fn mcp_playbook_scenario_run_carries_the_flag() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = dir.path().join("cfg");
    let project = project(dir.path());
    let env = [
        ("APB_CONFIG_DIR", cfg.to_str().unwrap()),
        ("APB_NO_REGISTRY", "1"),
    ];
    let (mut child, mut stdin, rx) = start_mcp(&project, &env);
    let play = |stdin: &mut ChildStdin, id: u32, flag: Option<bool>| {
        let mut args = serde_json::json!({ "id": "ext", "scenario": "green", "level": "quick" });
        if let Some(flag) = flag {
            args["external_data"] = flag.into();
        }
        let body = serde_json::json!({
            "method": "tools/call",
            "params": { "name": "playbook_scenario_run", "arguments": args },
        })
        .to_string();
        let played = tool_json(&request(stdin, &rx, id, &body[1..body.len() - 1]));
        assert_eq!(played["report"]["verdict"], "passed", "{played}");
        flagged(&project, &played["report"])
    };
    assert!(play(&mut stdin, 2, Some(true)));
    assert!(!play(&mut stdin, 3, None));
    let _ = child.kill();
    let _ = child.wait();
}

#[test]
fn the_dashboards_scenario_run_carries_the_flag() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = tempfile::tempdir_in(env!("CARGO_TARGET_TMPDIR")).unwrap();
    let project = project(dir.path());
    // Register the project in this test's own registry, so the global
    // dashboard can address it.
    let listed = crate::common::apb_std()
        .arg("list")
        .current_dir(&project)
        .env("APB_CONFIG_DIR", cfg.path())
        .env_remove("CI")
        .env_remove("APB_NO_REGISTRY")
        .output()
        .unwrap();
    assert!(listed.status.success(), "apb list failed: {listed:?}");
    let workspace = fs::read_to_string(project.join(".apb/workspace.local"))
        .expect("the project was registered")
        .trim()
        .to_string();
    let dashboard = crate::detached_driver_test::Dashboard::start_with(cfg.path(), &[]);
    let play = |body: &str| {
        let played = dashboard.post(
            &format!("/api/playbooks/ext/scenario-runs?workspace={workspace}"),
            body,
        );
        assert_eq!(played["report"]["verdict"], "passed", "{played}");
        flagged(&project, &played["report"])
    };
    assert!(play(
        r#"{"scenario":"green","level":"quick","external_data":true}"#
    ));
    assert!(!play(r#"{"scenario":"green","level":"quick"}"#));
}
