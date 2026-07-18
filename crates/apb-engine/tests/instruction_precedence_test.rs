use apb_core::registry::{Registry, init_project};
use apb_engine::run_config::read_run_config;
use apb_engine::scheduler::{RunOptions, run};
use std::fs;
use std::path::Path;

fn seed(root: &Path) {
    init_project(root).unwrap();
    let vdir = root.join(".apb/playbooks/p/1.0.0");
    fs::create_dir_all(&vdir).unwrap();
    fs::write(
        vdir.join("playbook.yaml"),
        "schema: 2\nid: p\nname: p\nversion: 1.0.0\nnodes:\n  - { id: s, type: start }\n  - { id: n, type: prompt, prompt: \"{{run.instruction}}\" }\n  - { id: f, type: finish, outcome: success }\nedges:\n  - { from: s, to: n }\n  - { from: n, to: f }\n",
    )
    .unwrap();
    fs::write(root.join(".apb/playbooks/p/current"), "1.0.0").unwrap();
}

fn instruction_of(root: &Path, run_id: &str) -> Option<String> {
    read_run_config(&root.join(".apb/runs").join(run_id))
        .unwrap()
        .instruction
}

#[test]
fn draft_used_when_no_explicit_instruction() {
    let dir = tempfile::tempdir().unwrap();
    seed(dir.path());
    Registry::open(dir.path())
        .unwrap()
        .write_instruction_draft("p", "from draft")
        .unwrap();

    let res = run(dir.path(), "p", None, RunOptions::default()).unwrap();
    assert_eq!(
        instruction_of(dir.path(), &res.run_id).as_deref(),
        Some("from draft")
    );
}

#[test]
fn explicit_instruction_beats_draft() {
    let dir = tempfile::tempdir().unwrap();
    seed(dir.path());
    Registry::open(dir.path())
        .unwrap()
        .write_instruction_draft("p", "from draft")
        .unwrap();

    let opts = RunOptions {
        instruction: Some("explicit".into()),
        ..Default::default()
    };
    let res = run(dir.path(), "p", None, opts).unwrap();
    assert_eq!(
        instruction_of(dir.path(), &res.run_id).as_deref(),
        Some("explicit")
    );
}

#[test]
fn none_when_no_draft_and_no_explicit() {
    let dir = tempfile::tempdir().unwrap();
    seed(dir.path());
    let res = run(dir.path(), "p", None, RunOptions::default()).unwrap();
    assert_eq!(instruction_of(dir.path(), &res.run_id), None);
}

#[test]
fn param_default_is_filled_into_persisted_run_config() {
    // Review I6/R1-I2: a declared param the caller omits falls back to its
    // schema `default` in the SINGLE normalization point (prepare), for every
    // run - here a plain top-level run. The persisted RunConfig carries it.
    let dir = tempfile::tempdir().unwrap();
    init_project(dir.path()).unwrap();
    let vdir = dir.path().join(".apb/playbooks/pd/1.0.0");
    fs::create_dir_all(&vdir).unwrap();
    fs::write(
        vdir.join("playbook.yaml"),
        "schema: 2\nid: pd\nname: pd\nversion: 1.0.0\nparams:\n  - { name: mode, type: text, default: \"fast\" }\n  - { name: given, type: text, default: \"unused\" }\nnodes:\n  - { id: s, type: start }\n  - { id: n, type: prompt, prompt: \"{{params.mode}}\" }\n  - { id: f, type: finish, outcome: success }\nedges:\n  - { from: s, to: n }\n  - { from: n, to: f }\n",
    )
    .unwrap();
    fs::write(dir.path().join(".apb/playbooks/pd/current"), "1.0.0").unwrap();

    // Caller supplies `given` explicitly; `mode` is omitted and must default.
    let mut params = std::collections::BTreeMap::new();
    params.insert("given".to_string(), "explicit".to_string());
    let opts = RunOptions {
        params,
        ..Default::default()
    };
    let res = run(dir.path(), "pd", None, opts).unwrap();

    let cfg = read_run_config(&dir.path().join(".apb/runs").join(&res.run_id)).unwrap();
    assert_eq!(
        cfg.params.get("mode").map(|s| s.as_str()),
        Some("fast"),
        "the omitted param takes its schema default"
    );
    assert_eq!(
        cfg.params.get("given").map(|s| s.as_str()),
        Some("explicit"),
        "an explicitly passed param is not overwritten by its default"
    );
}
