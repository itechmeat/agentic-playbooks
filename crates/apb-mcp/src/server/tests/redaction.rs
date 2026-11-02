//! A run's secret values on `run_status`: a child's value that reaches its
//! parent inside JSON text apb wrote (the child's run outputs as the
//! parent node's output, nested) is hidden in that text, which stays valid
//! JSON, and a run whose own `run.yaml` is gone withholds its text.

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

/// A playbook that runs `child` as its node `sub` and passes that node's
/// output (the child's run outputs as JSON text) on as its own output.
fn passing(id: &str, child: &str) -> String {
    format!(
        "schema: 2\nid: {id}\nname: {id}\nversion: 1.0.0\noutputs:\n  inner: {{ from: {{ node: sub }} }}\nnodes:\n  - {{ id: start, type: start }}\n  - {{ id: sub, type: playbook, playbook: {child}, output: outputs }}\n  - {{ id: done, type: finish, outcome: success }}\nedges:\n  - {{ from: start, to: sub }}\n  - {{ from: sub, to: done }}\n"
    )
}

/// The child's value under three nested `output: outputs` hand-offs: the
/// root's node output holds it three times escaped, and still hides it.
#[tokio::test]
async fn a_childs_secret_with_a_quote_is_hidden_three_hand_offs_up() {
    let _l = CROSS_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let _cfg = ConfigDirGuard::new();
    let proj = project();
    install(proj.path(), "qmid1", &passing("qmid1", "qchild"), &[]);
    install(proj.path(), "qmid2", &passing("qmid2", "qmid1"), &[]);
    install(proj.path(), "qroot", &passing("qroot", "qmid2"), &[]);
    let run_id = apb_engine::run(
        proj.path(),
        "qroot",
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

/// A child whose sensitive value starts with a line break, printed right
/// after a backslash.
const BACKSLASH_CHILD: &str = r#"
schema: 2
id: bchild
name: bchild
version: 1.0.0
params:
  - { name: password, type: text, default: "\nsecret-7" }
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

/// The value hidden in the parent's node output leaves that output valid
/// JSON: the backslash before the value stays a backslash.
#[tokio::test]
async fn a_hidden_value_after_a_backslash_leaves_the_embedded_json_valid() {
    let _l = CROSS_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let _cfg = ConfigDirGuard::new();
    let proj = tempfile::tempdir().unwrap();
    apb_core::registry::init_project(proj.path()).unwrap();
    install(
        proj.path(),
        "bchild",
        BACKSLASH_CHILD,
        &[("emit.sh", "printf '\\\\\\nsecret-7\\n'\n")],
    );
    install(proj.path(), "bparent", &passing("bparent", "bchild"), &[]);
    let run_id = apb_engine::run(
        proj.path(),
        "bparent",
        None,
        apb_engine::RunOptions::default(),
    )
    .unwrap()
    .run_id;
    let server = WfMcp::new(proj.path().to_path_buf());
    let shown = status(&server, &run_id).await;
    assert!(!shown.contains("secret-7"), "{shown}");
    let v: serde_json::Value = serde_json::from_str(&shown).unwrap();
    let sub = v["outputs"]["sub"]
        .as_str()
        .unwrap_or_else(|| panic!("{shown}"));
    let embedded: serde_json::Value =
        serde_json::from_str(sub).unwrap_or_else(|e| panic!("{e}: {sub}"));
    assert_eq!(embedded["creds"], "\\[redacted]", "{sub}");
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
