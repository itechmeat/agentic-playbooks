//! The start key on MCP `playbook_run`: a keyed repeat answers the run
//! already started, another request under the key is a conflict, a bad key
//! is refused, and neither a candidate version the gate would pick nor a
//! global start's resolved version changes the request.

use super::*;

const POLICY: &str = "supervisor:\n  policy:\n    promote_supervisor_patches: on_success\n    trial_candidates: next_run\n";

fn quick(id: &str, head: &str) -> String {
    format!(
        "schema: 2\nid: {id}\nname: Q\nversion: 1.0.0\n{head}params:\n  - {{ name: who, type: text, default: x }}\nnodes:\n  - {{ id: start, type: start }}\n  - {{ id: s, type: script, script: \"scripts/s.sh\", runner: sh }}\n  - {{ id: done, type: finish, outcome: success }}\nedges:\n  - {{ from: start, to: s }}\n  - {{ from: s, to: done }}\n"
    )
}

/// Seeds playbook `id` under `parent` (a project's `.apb` or the config
/// dir).
fn seed(parent: &Path, id: &str, yaml: &str) {
    let vdir = parent.join("playbooks").join(id).join("1.0.0");
    fs::create_dir_all(vdir.join("scripts")).unwrap();
    fs::write(vdir.join("playbook.yaml"), yaml).unwrap();
    fs::write(vdir.join("scripts/s.sh"), "echo ok\n").unwrap();
    fs::write(parent.join("playbooks").join(id).join("current"), "1.0.0").unwrap();
}

fn args(id: &str, key: &str, who: &str, scope: Option<&str>) -> PlaybookRunArgs {
    PlaybookRunArgs {
        deadline: None,
        start_key: Some(key.into()),
        start_key_reuse: None,
        id: id.into(),
        version: None,
        params: BTreeMap::from([("who".to_string(), who.to_string())]),
        instruction: None,
        supervise: None,
        background: None,
        acknowledge_untrusted: Some(true),
        confirm_irreversible: None,
        scope: scope.map(str::to_string),
        continued_from: None,
        worktree: None,
        execution: None,
        budget: None,
        remember: None,
        notify: None,
        launch_version: None,
        launch_choice: None,
        unattended: None,
        confirm_launch: None,
        external_data: None,
    }
}

async fn start(server: &WfMcp, a: PlaybookRunArgs) -> serde_json::Value {
    serde_json::from_str(&result_text(&server.playbook_run(Parameters(a)).await)).unwrap()
}

#[tokio::test]
async fn a_keyed_repeat_answers_the_run_and_another_request_conflicts() {
    let _l = CROSS_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let _cfg = ConfigDirGuard::new();
    let proj = tempfile::tempdir().unwrap();
    let root = proj.path();
    apb_core::registry::init_project(root).unwrap();
    seed(&root.join(".apb"), "q", &quick("q", ""));
    let server = WfMcp::new(root.to_path_buf());
    let first = start(&server, args("q", "ev-1", "a", None)).await;
    let run_id = first["run_id"]
        .as_str()
        .unwrap_or_else(|| panic!("{first}"))
        .to_string();
    let again = start(&server, args("q", "ev-1", "a", None)).await;
    assert_eq!(again["already_started"], true, "{again}");
    assert_eq!(again["run_id"], run_id.as_str());
    assert_eq!(again["status"], "succeeded");
    let other = start(&server, args("q", "ev-1", "b", None)).await;
    let other = &other["policy_refusal"];
    assert_eq!(other["policy"], "start_key_conflict", "{other}");
    assert_eq!(other["run_id"], run_id.as_str());
    let token = format!(
        "ghp_{}",
        "a1B2c3D4e5".repeat(4).chars().take(36).collect::<String>()
    );
    let bad = start(&server, args("q", &token, "a", None)).await;
    assert_eq!(
        bad["policy_refusal"]["policy"], "start_key_invalid",
        "{bad}"
    );
    assert!(!bad.to_string().contains(&token));
    let status: serde_json::Value = serde_json::from_str(&result_text(
        &server
            .run_status(Parameters(RunRefArgs {
                run_id: run_id.clone(),
                workspace: None,
            }))
            .await,
    ))
    .unwrap();
    assert_eq!(
        status["start_key"].as_str().map(str::len),
        Some(12),
        "{status}"
    );
}

/// A keyed start without a version; a candidate version appears; the
/// repeat without a version answers the first run, not a conflict.
#[tokio::test]
async fn a_candidate_does_not_change_the_request() {
    let _l = CROSS_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let _cfg = ConfigDirGuard::new();
    let proj = tempfile::tempdir().unwrap();
    let root = proj.path();
    apb_core::registry::init_project(root).unwrap();
    seed(&root.join(".apb"), "c", &quick("c", POLICY));
    let server = WfMcp::new(root.to_path_buf());
    let first = start(&server, args("c", "ev-1", "a", None)).await;
    let run_id = first["run_id"]
        .as_str()
        .unwrap_or_else(|| panic!("{first}"))
        .to_string();
    let patched = quick("c", POLICY).replace("name: Q", "name: Q2");
    apb_engine::forward_patch::create(
        root,
        &run_id,
        &apb_engine::forward_patch::ForwardPatchRequest {
            yaml: patched,
            classification: "improvement".into(),
            rationale: Some("a clearer name".into()),
            evidence: vec!["node s".into()],
        },
    )
    .unwrap();
    let again = start(&server, args("c", "ev-1", "a", None)).await;
    assert_eq!(again["already_started"], true, "{again}");
    assert_eq!(again["run_id"], run_id.as_str());
}

/// A keyed start of a global playbook without a version reaches the
/// engine with a concrete version; the repeat answers the first run.
#[tokio::test]
async fn a_global_start_repeats() {
    let _l = CROSS_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let _cfg = ConfigDirGuard::new();
    let proj = tempfile::tempdir().unwrap();
    let root = proj.path();
    apb_core::registry::init_project(root).unwrap();
    let cfg = apb_core::config::config_dir().unwrap();
    seed(&cfg, "g", &quick("g", ""));
    let server = WfMcp::new(root.to_path_buf());
    let first = start(&server, args("g", "ev-1", "a", Some("global"))).await;
    let run_id = first["run_id"]
        .as_str()
        .unwrap_or_else(|| panic!("{first}"))
        .to_string();
    let again = start(&server, args("g", "ev-1", "a", Some("global"))).await;
    assert_eq!(again["already_started"], true, "{again}");
    assert_eq!(again["run_id"], run_id.as_str());
}

/// The lookup comes before the checks that read the environment: a keyed
/// unattended start, repeated from a run step (whose unattended start the
/// gate would refuse `unattended_caller`), answers the run it started.
#[tokio::test]
async fn a_repeat_is_answered_before_the_environment_checks() {
    let _l = CROSS_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let _cfg = ConfigDirGuard::new();
    let proj = tempfile::tempdir().unwrap();
    let root = proj.path();
    apb_core::registry::init_project(root).unwrap();
    seed(&root.join(".apb"), "u", &quick("u", ""));
    let server = WfMcp::new(root.to_path_buf());
    let keyed = || PlaybookRunArgs {
        unattended: Some(true),
        ..args("u", "ev-u", "a", None)
    };
    let first = start(&server, keyed()).await;
    let run_id = first["run_id"]
        .as_str()
        .unwrap_or_else(|| panic!("{first}"))
        .to_string();
    // A step of a run whose mode cannot be read counts as attended.
    // SAFETY: CROSS_ENV_LOCK serializes the env across these tests.
    unsafe { std::env::set_var("APB_RUN_ID", "attended-step-1") };
    let again = start(&server, keyed()).await;
    unsafe { std::env::remove_var("APB_RUN_ID") };
    assert_eq!(again["already_started"], true, "{again}");
    assert_eq!(again["run_id"], run_id.as_str());
}
