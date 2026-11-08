//! The scenario capture preview on the run tools: the MCP args carry no
//! save argument, a preview writes nothing, and a run step is refused.

use super::*;

const PLAYBOOK: &str = "schema: 2\nid: cap\nname: cap\nversion: 1.0.0\ndefaults: { profile: main }\nnodes:\n  - { id: start, type: start }\n  - { id: work, type: agent_task, prompt: \"Fix {{run.instruction}}\" }\n  - { id: done, type: finish, outcome: success }\nedges:\n  - { from: start, to: work }\n  - { from: work, to: done, condition: { type: node_status, node: work, equals: success } }\n";

fn seed_capture_run(root: &Path) {
    apb_core::registry::init_project(root).unwrap();
    let vdir = root.join(".apb/playbooks/cap/1.0.0");
    fs::create_dir_all(&vdir).unwrap();
    fs::write(vdir.join("playbook.yaml"), PLAYBOOK).unwrap();
    fs::write(root.join(".apb/playbooks/cap/current"), "1.0.0").unwrap();
    let dir = root.join(".apb/runs/cap-1");
    fs::create_dir_all(&dir).unwrap();
    let lines = [
        r#"{"seq":1,"ts":1,"type":"run_started","playbook":"cap","version":"1.0.0"}"#,
        r#"{"seq":2,"ts":1,"type":"node_started","node":"start","attempt":1}"#,
        r#"{"seq":3,"ts":1,"type":"node_finished","node":"start","status":"succeeded","attempt":1,"output":""}"#,
        r#"{"seq":4,"ts":1,"type":"node_started","node":"work","attempt":1}"#,
        r#"{"seq":5,"ts":1,"type":"attempt_started","node":"work","attempt":1,"agent":"the-stub-agent"}"#,
        r#"{"seq":6,"ts":1,"type":"attempt_finished","node":"work","attempt":1,"status":"succeeded","output":"fixed the parser"}"#,
        r#"{"seq":7,"ts":1,"type":"node_finished","node":"work","status":"succeeded","attempt":1,"output":"fixed the parser"}"#,
        r#"{"seq":8,"ts":1,"type":"node_finished","node":"done","status":"succeeded","attempt":1,"output":""}"#,
        r#"{"seq":9,"ts":1,"type":"run_finished","outcome":"succeeded"}"#,
    ];
    fs::write(dir.join("events.jsonl"), lines.join("\n") + "\n").unwrap();
    fs::write(dir.join("playbook.yaml"), PLAYBOOK).unwrap();
    fs::write(
        dir.join("run.yaml"),
        "params: {}\ninstruction: the parser\nunattended: false\n",
    )
    .unwrap();
}

/// The MCP args type has no save argument: a save is a person's act on the
/// CLI or the dashboard, never MCP.
#[test]
fn the_mcp_args_have_no_save() {
    assert!(
        serde_json::from_value::<crate::server::scenario::RunScenarioCaptureArgs>(
            serde_json::json!({
                "run_id": "cap-1",
                "name": "green-after-one-fix",
                "save": true,
            })
        )
        .is_err(),
        "the MCP capture args accepted `save`"
    );
    assert!(
        serde_json::from_value::<crate::server::scenario::RunScenarioCaptureArgs>(
            serde_json::json!({
                "run_id": "cap-1",
                "name": "green-after-one-fix",
                "digest": "sha256:x",
            })
        )
        .is_err(),
        "the MCP capture args accepted `digest`"
    );
}

/// A preview answers the YAML and writes nothing.
#[tokio::test]
async fn a_preview_answers_and_writes_nothing() {
    let _l = CROSS_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let dir = seeded_root();
    seed_capture_run(dir.path());
    let server = WfMcp::new(dir.path().to_path_buf());
    let result = server
        .run_scenario_capture(Parameters(
            crate::server::scenario::RunScenarioCaptureArgs {
                run_id: "cap-1".into(),
                name: "green-after-one-fix".into(),
                nodes: Vec::new(),
            },
        ))
        .await;
    assert_eq!(result.is_error, Some(false), "{}", result_text(&result));
    let text = result_text(&result);
    assert!(text.contains("green-after-one-fix"), "{text}");
    assert!(
        !dir.path()
            .join(".apb/playbooks/cap/evals/scenarios/green-after-one-fix.yaml")
            .exists(),
        "the preview wrote a file"
    );
}

/// Sets `APB_RUN_ID` for one test and puts the earlier value back when
/// dropped, even when the test panics.
struct RunIdGuard(Option<std::ffi::OsString>);

impl RunIdGuard {
    fn set(value: &str) -> Self {
        let old = std::env::var_os("APB_RUN_ID");
        // SAFETY: under CROSS_ENV_LOCK, like every env change in these tests.
        unsafe { std::env::set_var("APB_RUN_ID", value) };
        RunIdGuard(old)
    }
}

impl Drop for RunIdGuard {
    fn drop(&mut self) {
        // SAFETY: under CROSS_ENV_LOCK.
        unsafe {
            match &self.0 {
                Some(v) => std::env::set_var("APB_RUN_ID", v),
                None => std::env::remove_var("APB_RUN_ID"),
            }
        }
    }
}

/// A run step's preview is refused.
#[tokio::test]
async fn a_run_step_is_refused() {
    let _l = CROSS_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let _g = RunIdGuard::set("cap-1");
    let dir = seeded_root();
    seed_capture_run(dir.path());
    let server = WfMcp::new(dir.path().to_path_buf());
    let result = server
        .run_scenario_capture(Parameters(
            crate::server::scenario::RunScenarioCaptureArgs {
                run_id: "cap-1".into(),
                name: "green-after-one-fix".into(),
                nodes: Vec::new(),
            },
        ))
        .await;
    assert_eq!(result.is_error, Some(true), "{}", result_text(&result));
    assert!(
        result_text(&result).contains("capturing a scenario"),
        "{}",
        result_text(&result)
    );
}
