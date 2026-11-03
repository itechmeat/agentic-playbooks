use std::path::Path;

fn init(dir: &Path) {
    apb_core::registry::init_project(dir).unwrap();
}

/// Runs `playbook projects ...` in directory `cwd` with a shared APB_CONFIG_DIR.
fn run_projects(cfg: &Path, cwd: &Path, args: &[&str]) -> (String, String) {
    let out = crate::common::apb_std()
        .args(["projects"])
        .args(args)
        .current_dir(cwd)
        .env("APB_CONFIG_DIR", cfg)
        .env_remove("CI")
        .env_remove("APB_NO_REGISTRY")
        .output()
        .expect("run playbook projects");
    (
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

#[test]
fn projects_list_and_remove() {
    let cfg = tempfile::tempdir().unwrap();
    let a = tempfile::tempdir().unwrap();
    let b = tempfile::tempdir().unwrap();
    init(a.path());
    init(b.path());

    // Auto-registration happens when playbook runs inside a project: two
    // commands in two projects register both (list on its own also does not
    // touch it - the touch is tied to running inside the project directory,
    // so we register via `apb list` in each).
    for dir in [a.path(), b.path()] {
        crate::common::apb_std()
            .arg("list")
            .current_dir(dir)
            .env("APB_CONFIG_DIR", cfg.path())
            .env_remove("CI")
            .env_remove("APB_NO_REGISTRY")
            .output()
            .unwrap();
    }

    // list from any directory shows both workspaces.
    let (stdout, _) = run_projects(cfg.path(), a.path(), &["list"]);
    let a_name = a.path().file_name().unwrap().to_string_lossy();
    let b_name = b.path().file_name().unwrap().to_string_lossy();
    assert!(
        stdout.contains(a_name.as_ref()),
        "A missing in list: {stdout}"
    );
    assert!(
        stdout.contains(b_name.as_ref()),
        "B missing in list: {stdout}"
    );

    // Get project B's workspace_id and remove it.
    let b_id = apb_core::workspace::ensure_id(b.path()).unwrap();
    let (rm_out, _) = run_projects(cfg.path(), a.path(), &["remove", &b_id]);
    assert!(rm_out.contains("removed"), "remove output: {rm_out}");

    let (after, _) = run_projects(cfg.path(), a.path(), &["list"]);
    assert!(
        !after.contains(b_name.as_ref()),
        "B should be gone: {after}"
    );
}

/// `apb projects prune --dry-run` prints the gone workspace and changes
/// nothing; `apb projects prune` removes it and keeps the live one.
#[test]
fn projects_prune_removes_gone_workspaces() {
    let cfg = tempfile::tempdir().unwrap();
    let live = tempfile::tempdir().unwrap();
    let gone = tempfile::tempdir().unwrap();
    init(live.path());
    init(gone.path());
    for dir in [live.path(), gone.path()] {
        run_projects(cfg.path(), dir, &["list"]);
    }
    let live_id = apb_core::workspace::ensure_id(live.path()).unwrap();
    let gone_id = apb_core::workspace::ensure_id(gone.path()).unwrap();
    drop(gone);

    let (dry, _) = run_projects(cfg.path(), live.path(), &["prune", "--dry-run"]);
    assert!(dry.contains(&format!("would remove {gone_id}")), "{dry}");
    assert!(!dry.contains(&live_id), "{dry}");
    let (still, _) = run_projects(cfg.path(), live.path(), &["list"]);
    assert!(still.contains(&gone_id), "a dry run removed it: {still}");

    let (out, _) = run_projects(cfg.path(), live.path(), &["prune"]);
    assert!(out.contains(&format!("removed {gone_id}")), "{out}");
    let (after, _) = run_projects(cfg.path(), live.path(), &["list"]);
    assert!(
        after.contains(&live_id) && !after.contains(&gone_id),
        "{after}"
    );

    let (again, _) = run_projects(cfg.path(), live.path(), &["prune"]);
    assert!(again.contains("nothing to prune"), "{again}");
}
