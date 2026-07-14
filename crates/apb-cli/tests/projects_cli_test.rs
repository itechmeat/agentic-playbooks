use std::path::Path;
use std::process::Command;

fn apb_bin() -> &'static str {
    env!("CARGO_BIN_EXE_apb")
}

fn init(dir: &Path) {
    apb_core::registry::init_project(dir).unwrap();
}

/// Runs `playbook projects ...` in directory `cwd` with a shared APB_CONFIG_DIR.
fn run_projects(cfg: &Path, cwd: &Path, args: &[&str]) -> (String, String) {
    let out = Command::new(apb_bin())
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
    let list_bin = apb_bin();
    for dir in [a.path(), b.path()] {
        Command::new(list_bin)
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
