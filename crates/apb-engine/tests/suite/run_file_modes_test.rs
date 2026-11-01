//! The file modes of a run directory (`apb_engine::run_perms`): a fresh run
//! is private to its owner, and a run directory an older apb left readable
//! beyond its owner is narrowed when the run is resumed, owner execute and
//! link targets kept.
#![cfg(unix)]

use std::fs;
use std::os::unix::fs::PermissionsExt as _;
use std::path::Path;

use apb_core::registry::init_project;
use apb_engine::scheduler::{RunOptions, resume, run};
use apb_engine::state::RunStatus;

const PLAYBOOK: &str = r#"
schema: 2
id: modes
name: Modes
version: 1.0.0
nodes:
  - { id: start, type: start }
  - { id: a, type: script, script: "scripts/step.sh", runner: sh }
  - { id: done, type: finish, outcome: success }
edges:
  - { from: start, to: a }
  - { from: a, to: done }
"#;

/// The first run leaves an executable helper under the run's work
/// directory and fails; once `go` exists, the step runs that helper.
fn seed(root: &Path) {
    init_project(root).unwrap();
    let vdir = root.join(".apb/playbooks/modes/1.0.0");
    fs::create_dir_all(vdir.join("scripts")).unwrap();
    fs::write(vdir.join("playbook.yaml"), PLAYBOOK).unwrap();
    fs::write(root.join(".apb/playbooks/modes/current"), "1.0.0").unwrap();
    let go = root.join("go");
    let ran = root.join("helper-ran");
    fs::write(
        vdir.join("scripts/step.sh"),
        format!(
            "helper=\"$APB_RUN_DIR/work/a/1/helper.sh\"\n\
             if [ -f '{go}' ]; then \"$helper\" > '{ran}'; exit $?; fi\n\
             mkdir -p \"$(dirname \"$helper\")\"\n\
             printf '#!/bin/sh\\necho helper ran\\n' > \"$helper\"\n\
             chmod 755 \"$helper\"\n\
             echo first pass\n\
             exit 1\n",
            go = go.display(),
            ran = ran.display()
        ),
    )
    .unwrap();
}

fn mode(p: &Path) -> u32 {
    fs::symlink_metadata(p).unwrap().permissions().mode() & 0o777
}

/// Every entry under `dir` (itself included), links not followed.
fn entries(dir: &Path) -> Vec<std::path::PathBuf> {
    let mut out = vec![dir.to_path_buf()];
    let meta = fs::symlink_metadata(dir).unwrap();
    if meta.is_dir() && !meta.file_type().is_symlink() {
        for e in fs::read_dir(dir).unwrap().flatten() {
            out.extend(entries(&e.path()));
        }
    }
    out
}

fn set(p: &Path, m: u32) {
    fs::set_permissions(p, fs::Permissions::from_mode(m)).unwrap();
}

#[test]
fn a_fresh_run_is_private_and_a_widened_one_is_narrowed_on_resume() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    seed(root);
    let first = run(root, "modes", None, RunOptions::default()).unwrap();
    assert_eq!(first.outcome, RunStatus::Failed);
    let run_dir = root.join(".apb/runs").join(&first.run_id);

    assert_eq!(mode(&run_dir), 0o700);
    for name in ["run.yaml", "events.jsonl"] {
        assert_eq!(mode(&run_dir.join(name)), 0o600, "{name}");
    }
    let helper = run_dir.join("work/a/1/helper.sh");
    assert_eq!(mode(&helper), 0o755, "the step's own chmod stands");
    // Every file apb wrote (the step's own work directory aside) is
    // private; a directory inside the 0700 run directory needs no more.
    let widened: Vec<_> = entries(&run_dir)
        .into_iter()
        .filter(|p| !p.starts_with(run_dir.join("work")) && p.is_file() && mode(p) & 0o077 != 0)
        .collect();
    assert!(widened.is_empty(), "readable beyond the owner: {widened:?}");
    // The step's 0755 helper inside the private run directory is reachable
    // by nobody else: the doctor does not flag a fresh run.
    assert_eq!(apb_engine::run_perms::wide(&run_dir), None);

    // The run directory as an older apb left it, plus a link to a file
    // outside it.
    let sentinel = root.join("sentinel.txt");
    fs::write(&sentinel, "plain").unwrap();
    set(&sentinel, 0o644);
    std::os::unix::fs::symlink(&sentinel, run_dir.join("outside-link")).unwrap();
    for p in entries(&run_dir) {
        if p == helper || fs::symlink_metadata(&p).unwrap().file_type().is_symlink() {
            continue;
        }
        set(&p, if p.is_dir() { 0o775 } else { 0o664 });
    }
    assert!(apb_engine::run_perms::wide(&run_dir).is_some());

    fs::write(root.join("go"), "").unwrap();
    let second = resume(root, &first.run_id, Some("a")).unwrap();
    assert_eq!(second.outcome, RunStatus::Succeeded);
    assert_eq!(
        fs::read_to_string(root.join("helper-ran")).unwrap().trim(),
        "helper ran"
    );
    assert_eq!(mode(&helper), 0o700, "only group and other bits go");
    assert_eq!(mode(&run_dir), 0o700);
    assert_eq!(mode(&run_dir.join("events.jsonl")), 0o600);
    assert_eq!(mode(&run_dir.join("run.yaml")), 0o600);
    assert_eq!(mode(&sentinel), 0o644, "a link target keeps its mode");
}
