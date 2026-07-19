//! CI validate test for the demo playbooks (spec
//! 2026-07-19-official-connectors-design section 6): installs the repo
//! connectors `--from-dir` into a temp project, approves each connector and
//! its fake `default` account exactly as a user would via `apb connector
//! approve`, writes fake non-secret account configs, registers the
//! `examples/playbooks/*.yaml` playbooks as installed playbook versions, and
//! runs `apb validate` on each. This is what fails the build when a manifest
//! change (a renamed function, a tightened args_schema) breaks a demo
//! playbook's connector grants.

use std::fs;
use std::path::Path;
use std::process::Command;

/// Runs `apb <args>` with a fresh per-test config dir and no inherited run
/// context, mirroring `connector_cli.rs`'s `playbook`/`playbook_env` helpers.
fn playbook(dir: &Path, args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_apb"))
        .args(args)
        .current_dir(dir)
        .env("APB_CONFIG_DIR", dir.join("cfg"))
        .env("HOME", dir.join("home"))
        .env("APB_NO_REGISTRY", "1")
        .env_remove("APB_RUN_DIR")
        .env_remove("APB_NODE_ID")
        .output()
        .unwrap()
}

fn apb_ok(dir: &Path, args: &[&str]) -> std::process::Output {
    let out = playbook(dir, args);
    assert!(
        out.status.success(),
        "`apb {}` failed: {}",
        args.join(" "),
        String::from_utf8_lossy(&out.stderr)
    );
    out
}

/// The repository's `connectors/<name>/` folder, resolved from the crate
/// manifest dir the same way the CI manifest gate does.
fn repo_connector_dir(name: &str) -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../connectors")
        .join(name)
        .canonicalize()
        .unwrap_or_else(|e| panic!("connectors/{name} must exist: {e}"))
}

/// The repository's `examples/playbooks/<file>` fixture.
fn repo_playbook_yaml(file: &str) -> String {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../examples/playbooks")
        .join(file);
    fs::read_to_string(&path).unwrap_or_else(|e| panic!("cannot read {}: {e}", path.display()))
}

/// Installs a repo connector `--from-dir`, then approves the connector's
/// tree digest and a fake `default` account, exactly as a user would via
/// `apb connector approve`.
fn install_and_approve(dir: &Path, name: &str, account_yaml: &str) {
    let src = repo_connector_dir(name);
    apb_ok(
        dir,
        &["connector", "install", "--from-dir", src.to_str().unwrap()],
    );

    let cfg_path = dir
        .join(".apb/connector-config")
        .join(format!("{name}.yaml"));
    fs::create_dir_all(cfg_path.parent().unwrap()).unwrap();
    fs::write(&cfg_path, account_yaml).unwrap();

    apb_ok(dir, &["connector", "approve", name]);
    apb_ok(dir, &["connector", "approve", name, "--account", "default"]);
}

/// Seeds a minimal project-scope profile `main`, referenced by both demo
/// playbooks' `defaults.profile`.
fn seed_profile_main(dir: &Path) {
    let pdir = dir.join(".apb/profiles/main");
    fs::create_dir_all(&pdir).unwrap();
    fs::write(
        pdir.join("profile.yaml"),
        "name: main\ndescription: test\nexecutor:\n  agent: claude\n  model: haiku\n",
    )
    .unwrap();
    fs::write(pdir.join("SOUL.md"), "test soul").unwrap();
}

/// Copies a playbook YAML file into `.apb/playbooks/<id>/<version>/playbook.yaml`
/// plus a `current` marker, mirroring `connector_cli.rs`'s `write_run_playbook`.
fn register_playbook(dir: &Path, id: &str, version: &str, yaml: &str) {
    let vdir = dir.join(".apb/playbooks").join(id).join(version);
    fs::create_dir_all(&vdir).unwrap();
    fs::write(vdir.join("playbook.yaml"), yaml).unwrap();
    fs::write(dir.join(".apb/playbooks").join(id).join("current"), version).unwrap();
}

fn setup(dir: &Path) -> std::path::PathBuf {
    let root = dir.to_path_buf();
    fs::create_dir_all(root.join("cfg")).unwrap();
    fs::create_dir_all(root.join("home")).unwrap();
    apb_ok(&root, &["init"]);

    install_and_approve(
        &root,
        "github",
        "accounts:\n  - name: default\n    api_base: https://api.github.com\n    token: \"{{env.DEMO_GITHUB_TOKEN}}\"\n",
    );
    install_and_approve(
        &root,
        "telegram",
        "accounts:\n  - name: default\n    api_base: https://api.telegram.org\n    token: \"{{env.DEMO_TELEGRAM_TOKEN}}\"\n",
    );
    install_and_approve(
        &root,
        "smtp",
        "accounts:\n  - name: default\n    host: smtp.example.com\n    port: \"587\"\n    from_email: releases@example.com\n    from_name: Release Bot\n    use_tls: \"true\"\n    username: releases@example.com\n    password: \"{{env.DEMO_SMTP_PASSWORD}}\"\n",
    );
    install_and_approve(
        &root,
        "sentry",
        "accounts:\n  - name: default\n    base_url: https://sentry.io\n    org: acme\n    token: \"{{env.DEMO_SENTRY_TOKEN}}\"\n",
    );
    install_and_approve(
        &root,
        "asana",
        "accounts:\n  - name: default\n    api_base: https://app.asana.com/api/1.0\n    token: \"{{env.DEMO_ASANA_TOKEN}}\"\n",
    );
    install_and_approve(
        &root,
        "imap",
        "accounts:\n  - name: default\n    host: imap.example.com\n    port: \"993\"\n    use_tls: \"true\"\n    auth_method: password\n    username: mailbox@example.com\n    password: \"{{env.DEMO_IMAP_PASSWORD}}\"\n",
    );

    seed_profile_main(&root);
    root
}

#[test]
fn sentry_triage_demo_playbook_validates() {
    let dir = tempfile::tempdir().unwrap();
    let root = setup(dir.path());
    register_playbook(
        &root,
        "sentry-triage",
        "1.0.0",
        &repo_playbook_yaml("sentry-triage.yaml"),
    );

    let out = apb_ok(&root, &["validate", "sentry-triage"]);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("sentry-triage: OK"),
        "sentry-triage should validate cleanly: {stdout}"
    );
}

#[test]
fn release_announce_demo_playbook_validates() {
    let dir = tempfile::tempdir().unwrap();
    let root = setup(dir.path());
    register_playbook(
        &root,
        "release-announce",
        "1.0.0",
        &repo_playbook_yaml("release-announce.yaml"),
    );

    let out = apb_ok(&root, &["validate", "release-announce"]);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("release-announce: OK"),
        "release-announce should validate cleanly: {stdout}"
    );
}

#[test]
fn inbox_triage_demo_playbook_validates() {
    let dir = tempfile::tempdir().unwrap();
    let root = setup(dir.path());
    register_playbook(
        &root,
        "inbox-triage",
        "1.0.0",
        &repo_playbook_yaml("inbox-triage.yaml"),
    );

    let out = apb_ok(&root, &["validate", "inbox-triage"]);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("inbox-triage: OK"),
        "inbox-triage should validate cleanly: {stdout}"
    );
}
