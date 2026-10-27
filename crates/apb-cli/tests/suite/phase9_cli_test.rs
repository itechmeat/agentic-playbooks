use assert_cmd::Command;
use predicates::prelude::*;
use std::fs;

const VALID: &str = include_str!("../../../apb-core/tests/fixtures/valid.yaml");

fn playbook() -> Command {
    crate::common::apb()
}

fn seeded_dir() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    playbook()
        .arg("init")
        .current_dir(dir.path())
        .assert()
        .success();
    let vdir = dir.path().join(".apb/playbooks/implement-task/1.0.0");
    fs::create_dir_all(&vdir).unwrap();
    fs::write(vdir.join("playbook.yaml"), VALID).unwrap();
    fs::write(
        dir.path().join(".apb/playbooks/implement-task/current"),
        "1.0.0",
    )
    .unwrap();
    // valid.yaml references the architect profile - it must exist in the project.
    seed_architect(dir.path());
    // ... and so must the `lint` node's script (V92).
    fs::create_dir_all(vdir.join("scripts")).unwrap();
    fs::write(vdir.join("scripts/node-lint.sh"), "true\n").unwrap();
    dir
}

/// Seeds a complete architect profile (profile.yaml + SOUL.md) so it
/// actually RESOLVES, rather than merely existing as a directory.
fn seed_architect(root: &std::path::Path) {
    let pdir = root.join(".apb/profiles/architect");
    fs::create_dir_all(&pdir).unwrap();
    fs::write(
        pdir.join("profile.yaml"),
        "name: architect\ndescription: test\nexecutor:\n  agent: claude\n  model: haiku\n",
    )
    .unwrap();
    fs::write(pdir.join("SOUL.md"), "").unwrap();
}

/// The bundle digest `apb import --preview` prints for `bundle` (what
/// `--accept` must name).
fn previewed_digest(
    bundle: &std::path::Path,
    dst: &std::path::Path,
    cfg: &std::path::Path,
) -> String {
    let out = playbook()
        .args(["import", "--preview"])
        .arg(bundle)
        .env("APB_CONFIG_DIR", cfg)
        .current_dir(dst)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .find_map(|l| l.strip_prefix("bundle digest: "))
        .expect("the preview prints the bundle digest")
        .trim()
        .to_string()
}

/// A project with the architect profile, ready to import the fixture.
fn import_target() -> (tempfile::TempDir, tempfile::TempDir) {
    let dst = tempfile::tempdir().unwrap();
    let cfg = tempfile::tempdir().unwrap();
    playbook()
        .arg("init")
        .env("APB_CONFIG_DIR", cfg.path())
        .current_dir(dst.path())
        .assert()
        .success();
    seed_architect(dst.path());
    (dst, cfg)
}

fn exported(src: &tempfile::TempDir) -> std::path::PathBuf {
    let bundle_path = src.path().join("bundle.json");
    playbook()
        .args(["export", "implement-task", "--out"])
        .arg(&bundle_path)
        .current_dir(src.path())
        .assert()
        .success();
    bundle_path
}

/// Without a terminal and without --accept, an import prints the preview
/// and exits 3 without writing; an --accept that names another digest is
/// refused (2) without writing; the previewed digest imports.
#[test]
fn import_without_a_terminal_needs_the_previewed_digest() {
    let src = seeded_dir();
    let bundle = exported(&src);
    let (dst, cfg) = import_target();
    let pdir = dst.path().join(".apb/playbooks/implement-task");

    playbook()
        .arg("import")
        .arg(&bundle)
        .env("APB_CONFIG_DIR", cfg.path())
        .current_dir(dst.path())
        .assert()
        .code(3)
        .stdout(predicate::str::contains("new playbook"))
        .stdout(predicate::str::contains("scripts added: node-lint.sh"))
        .stderr(predicate::str::contains("--accept sha256:"));
    assert!(!pdir.exists(), "nothing is written without an acceptance");

    playbook()
        .args(["import", "--accept", "sha256:0000"])
        .arg(&bundle)
        .env("APB_CONFIG_DIR", cfg.path())
        .current_dir(dst.path())
        .assert()
        .code(2);
    assert!(!pdir.exists(), "a wrong digest writes nothing");

    let digest = previewed_digest(&bundle, dst.path(), cfg.path());
    playbook()
        .args(["import", "--accept", &digest])
        .arg(&bundle)
        .env("APB_CONFIG_DIR", cfg.path())
        .current_dir(dst.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("imported implement-task"));
    assert!(pdir.join("current").is_file());
}

/// The same bundle again writes nothing and exits 0; a bundle whose version
/// exists with other content stops after the preview with exit 2, before
/// anyone is asked to accept it.
#[test]
fn import_of_the_same_or_a_conflicting_bundle_asks_nothing() {
    let src = seeded_dir();
    let bundle = exported(&src);
    let (dst, cfg) = import_target();
    let digest = previewed_digest(&bundle, dst.path(), cfg.path());
    let import = |file: &std::path::Path| {
        let mut cmd = playbook();
        cmd.arg("import")
            .arg(file)
            .env("APB_CONFIG_DIR", cfg.path())
            .current_dir(dst.path());
        cmd
    };
    import(&bundle)
        .arg("--accept")
        .arg(&digest)
        .assert()
        .success();
    import(&bundle)
        .assert()
        .success()
        .stdout(predicate::str::contains("nothing to import"));

    let mut changed: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(&bundle).unwrap()).unwrap();
    let yaml = changed["playbook"].as_str().unwrap().to_string();
    changed["playbook"] = serde_json::json!(yaml.replacen("name: ", "name: changed ", 1));
    let other = src.path().join("other.json");
    fs::write(&other, changed.to_string()).unwrap();
    import(&other)
        .assert()
        .code(2)
        .stderr(predicate::str::contains("import refused: see the preview"));
}

/// A version imported with scripts is approved only by naming its digest:
/// `apb trust approve` without --digest prints the digest and the scripts
/// and exits 2; with the digest it approves and activates the version.
#[test]
fn trust_approve_of_imported_scripts_needs_the_digest() {
    let src = seeded_dir();
    let bundle = exported(&src);
    let (dst, cfg) = import_target();
    let digest = previewed_digest(&bundle, dst.path(), cfg.path());
    playbook()
        .args(["import", "--accept", &digest])
        .arg(&bundle)
        .env("APB_CONFIG_DIR", cfg.path())
        .current_dir(dst.path())
        .assert()
        .success();

    let out = playbook()
        .args(["trust", "approve", "implement-task"])
        .env("APB_CONFIG_DIR", cfg.path())
        .current_dir(dst.path())
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(2));
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout_has(&out.stdout, "node-lint.sh"),
        "the scripts are listed"
    );
    let trust_digest = stdout
        .lines()
        .find_map(|l| l.split("--digest ").nth(1))
        .expect("the refusal names the digest")
        .trim()
        .to_string();
    assert!(
        !fs::read_to_string(cfg.path().join("trust.json"))
            .unwrap_or_default()
            .contains(&trust_digest)
    );

    playbook()
        .args([
            "trust",
            "approve",
            "implement-task",
            "--digest",
            "sha256:0000",
        ])
        .env("APB_CONFIG_DIR", cfg.path())
        .current_dir(dst.path())
        .assert()
        .code(2)
        .stderr(predicate::str::contains(format!(
            "digests to {trust_digest}, not sha256:0000"
        )));
    playbook()
        .args([
            "trust",
            "approve",
            "implement-task",
            "--digest",
            &trust_digest,
        ])
        .env("APB_CONFIG_DIR", cfg.path())
        .current_dir(dst.path())
        .assert()
        .success();
    assert!(
        fs::read_to_string(cfg.path().join("trust.json"))
            .unwrap()
            .contains(&trust_digest)
    );
}

fn stdout_has(stdout: &[u8], needle: &str) -> bool {
    String::from_utf8_lossy(stdout).contains(needle)
}

#[test]
fn doctor_runs_and_reports_playbook() {
    let dir = seeded_dir();
    playbook()
        .arg("doctor")
        .current_dir(dir.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("playbook implement-task"));
}

#[test]
fn export_then_import_round_trip_via_cli() {
    let src = seeded_dir();
    let bundle_path = src.path().join("bundle.json");
    playbook()
        .args(["export", "implement-task", "--out"])
        .arg(&bundle_path)
        .current_dir(src.path())
        .assert()
        .success();
    assert!(bundle_path.is_file(), "bundle file must be written");

    // Import into a clean project (the architect profile must be set up -
    // it's a project-level dependency of the playbook, and import validates
    // references).
    let (dst, cfg) = import_target();
    let digest = previewed_digest(&bundle_path, dst.path(), cfg.path());
    playbook()
        .args(["import", "--accept", &digest])
        .arg(&bundle_path)
        .env("APB_CONFIG_DIR", cfg.path())
        .current_dir(dst.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("imported implement-task"));

    // The imported playbook shows up in the list.
    playbook()
        .arg("list")
        .env("APB_CONFIG_DIR", cfg.path())
        .current_dir(dst.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("implement-task"));
}

#[test]
fn export_to_stdout_emits_bundle_json() {
    let dir = seeded_dir();
    playbook()
        .args(["export", "implement-task"])
        .current_dir(dir.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("\"apb_bundle\""))
        .stdout(predicate::str::contains("\"id\": \"implement-task\""));
}

#[test]
fn import_missing_file_fails() {
    let dir = seeded_dir();
    playbook()
        .args(["import", "nope.json"])
        .current_dir(dir.path())
        .assert()
        .failure();
}

#[test]
fn dev_without_frontend_fails_clearly() {
    // Empty directory without web/: dev must fail clearly, without hanging.
    let dir = tempfile::tempdir().unwrap();
    playbook()
        .arg("dev")
        .arg("--no-open")
        .current_dir(dir.path())
        .assert()
        .failure()
        .stderr(predicate::str::contains("frontend not found"));
}

/// `apb import` never approves what it imported, even a bundle without
/// scripts: the bundle came from elsewhere, so its digest stays out of the
/// trust store and the import says how to approve it.
#[test]
fn import_of_a_bundle_without_scripts_leaves_it_untrusted() {
    let src = seeded_dir();
    fs::remove_dir_all(
        src.path()
            .join(".apb/playbooks/implement-task/1.0.0/scripts"),
    )
    .unwrap();
    let bundle_path = src.path().join("bundle.json");
    playbook()
        .args(["export", "implement-task", "--out"])
        .arg(&bundle_path)
        .current_dir(src.path())
        .assert()
        .success();
    let (dst, cfg) = import_target();
    let digest = previewed_digest(&bundle_path, dst.path(), cfg.path());
    playbook()
        .args(["import", "--accept", &digest])
        .arg(&bundle_path)
        .env("APB_CONFIG_DIR", cfg.path())
        .current_dir(dst.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("is untrusted until approved"));
    let pdir = dst.path().join(".apb/playbooks/implement-task");
    let version = fs::read_to_string(pdir.join("current")).unwrap();
    let saved = fs::read_to_string(pdir.join(version.trim()).join("playbook.yaml")).unwrap();
    let trust = fs::read_to_string(cfg.path().join("trust.json")).unwrap_or_default();
    assert!(
        !trust.contains(&apb_core::scope::digest_str(&saved)),
        "an imported digest must not be approved, trust store: {trust}"
    );
}

/// A bundle that carries scripts imports as an untrusted version, and the
/// import says so with the way to approve it; the scripts arrive with it.
#[test]
fn import_of_a_bundle_with_scripts_reports_it_untrusted() {
    // The seeded version carries the `lint` node's script.
    let src = seeded_dir();
    let bundle_path = src.path().join("bundle.json");
    playbook()
        .args(["export", "implement-task", "--out"])
        .arg(&bundle_path)
        .current_dir(src.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("(1 script file(s))"));

    let (dst, cfg) = import_target();
    let digest = previewed_digest(&bundle_path, dst.path(), cfg.path());
    playbook()
        .args(["import", "--accept", &digest])
        .arg(&bundle_path)
        .env("APB_CONFIG_DIR", cfg.path())
        .current_dir(dst.path())
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "does not run until approved by digest",
        ))
        .stdout(predicate::str::contains("apb trust approve implement-task"));
    let pdir = dst.path().join(".apb/playbooks/implement-task");
    let version = fs::read_to_string(pdir.join("current")).unwrap();
    assert_eq!(
        fs::read_to_string(pdir.join(version.trim()).join("scripts/node-lint.sh")).unwrap(),
        "true\n"
    );
}
