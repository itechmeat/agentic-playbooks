//! A run's secret values on `run_status`: a child's value that reaches its
//! parent inside JSON text apb wrote (the child's run outputs as the
//! parent node's output) is hidden in its escaped spelling too, and a run
//! whose own `run.yaml` is gone withholds its text.

use super::*;

/// A child whose run output is its sensitive param (secret by its name),
/// a value with a quote in it.
const QUOTED_CHILD: &str = r#"
schema: 2
id: qchild
name: qchild
version: 1.0.0
params:
  - { name: password, type: text, default: 'pa"ss-word-1' }
outputs:
  creds: { from: { node: emit } }
nodes:
  - { id: start, type: start }
  - { id: emit, type: script, script: "scripts/emit.sh", runner: sh }
  - { id: done, type: finish, outcome: success }
edges:
  - { from: start, to: emit }
  - { from: emit, to: done }
"#;

/// A parent that takes the child's run outputs as its node's output.
const QUOTED_PARENT: &str = r#"
schema: 2
id: qparent
name: qparent
version: 1.0.0
nodes:
  - { id: start, type: start }
  - { id: sub, type: playbook, playbook: qchild, output: outputs }
  - { id: done, type: finish, outcome: success }
edges:
  - { from: start, to: sub }
  - { from: sub, to: done }
"#;

fn install(root: &Path, id: &str, yaml: &str, scripts: &[(&str, &str)]) {
    let vdir = root.join(format!(".apb/playbooks/{id}/1.0.0"));
    fs::create_dir_all(vdir.join("scripts")).unwrap();
    fs::write(vdir.join("playbook.yaml"), yaml).unwrap();
    for (name, body) in scripts {
        fs::write(vdir.join("scripts").join(name), body).unwrap();
    }
    fs::write(root.join(format!(".apb/playbooks/{id}/current")), "1.0.0").unwrap();
}

fn project() -> tempfile::TempDir {
    let proj = tempfile::tempdir().unwrap();
    apb_core::registry::init_project(proj.path()).unwrap();
    install(
        proj.path(),
        "qchild",
        QUOTED_CHILD,
        &[("emit.sh", "printf '%s\\n' 'pa\"ss-word-1'\n")],
    );
    install(proj.path(), "qparent", QUOTED_PARENT, &[]);
    proj
}

async fn status(server: &WfMcp, run_id: &str) -> String {
    result_text(
        &server
            .run_status(Parameters(RunRefArgs {
                run_id: run_id.to_string(),
                workspace: None,
            }))
            .await,
    )
}

#[tokio::test]
async fn a_childs_secret_with_a_quote_is_hidden_in_the_json_text_of_its_parents_output() {
    let _l = CROSS_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let _cfg = ConfigDirGuard::new();
    let proj = project();
    let run_id = apb_engine::run(
        proj.path(),
        "qparent",
        None,
        apb_engine::RunOptions::default(),
    )
    .unwrap()
    .run_id;
    let server = WfMcp::new(proj.path().to_path_buf());
    let shown = status(&server, &run_id).await;
    let v: serde_json::Value = serde_json::from_str(&shown).unwrap();
    assert!(v["outputs"]["sub"].is_string(), "{shown}");
    assert!(!shown.contains("ss-word-1"), "{shown}");
}

#[tokio::test]
async fn a_run_whose_own_config_is_gone_withholds_its_text() {
    let _l = CROSS_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let _cfg = ConfigDirGuard::new();
    let proj = project();
    let run_id = apb_engine::run(
        proj.path(),
        "qchild",
        None,
        apb_engine::RunOptions::default(),
    )
    .unwrap()
    .run_id;
    let dir = proj.path().join(".apb/runs").join(&run_id);
    fs::remove_file(dir.join("run.yaml")).unwrap();
    let server = WfMcp::new(proj.path().to_path_buf());
    let shown = status(&server, &run_id).await;
    assert!(!shown.contains("ss-word-1"), "{shown}");
    let v: serde_json::Value = serde_json::from_str(&shown).unwrap();
    assert_eq!(v["secrets_unresolved"], true, "{shown}");
}
