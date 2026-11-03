//! CI validate test for the demo playbooks (spec
//! 2026-07-19-official-connectors-design section 6): installs the repo
//! connectors `--from-dir` into a temp project, approves each connector and
//! its fake `default` account exactly as a user would via `apb connector
//! approve`, writes fake non-secret account configs, registers the
//! `examples/playbooks/*.yaml` playbooks as installed playbook versions, and
//! runs `apb validate` on each. This is what fails the build when a manifest
//! change (a renamed function, a tightened args_schema) breaks a demo
//! playbook's connector grants.
//!
//! It also plays the shipped keyless demo (`crates/apb-core/assets/demo`)
//! through the binary, to its review and on to `report.md`, and the "check
//! the task first" example's scenarios at quick.

use std::fs;
use std::path::Path;

/// Runs `apb <args>` with a fresh per-test config dir and no inherited run
/// context, mirroring `connector_cli.rs`'s `playbook`/`playbook_env` helpers.
fn playbook(dir: &Path, args: &[&str]) -> std::process::Output {
    crate::common::apb_std()
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

/// Seeds a minimal project-scope profile `main`, referenced by the demo
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
    install_and_approve(
        &root,
        "gitlab",
        "accounts:\n  - name: default\n    api_base: https://gitlab.com/api/v4\n    token: \"{{env.DEMO_GITLAB_TOKEN}}\"\n",
    );
    install_and_approve(
        &root,
        "youtrack",
        "accounts:\n  - name: default\n    api_base: https://example.youtrack.cloud/api\n    token: \"{{env.DEMO_YOUTRACK_TOKEN}}\"\n",
    );
    install_and_approve(
        &root,
        "slack",
        "accounts:\n  - name: default\n    api_base: https://slack.com/api\n    token: \"{{env.DEMO_SLACK_TOKEN}}\"\n",
    );
    install_and_approve(
        &root,
        "whatsapp",
        "accounts:\n  - name: default\n    base_url: https://graph.facebook.com\n    graph_version: v23.0\n    phone_number_id: \"100000000000000\"\n    waba_id: \"200000000000000\"\n    access_token: \"{{env.DEMO_WHATSAPP_ACCESS_TOKEN}}\"\n    app_secret: \"{{env.DEMO_WHATSAPP_APP_SECRET}}\"\n    verify_token: \"{{env.DEMO_WHATSAPP_VERIFY_TOKEN}}\"\n",
    );
    install_and_approve(
        &root,
        "twenty",
        "accounts:\n  - name: default\n    base_url: https://api.twenty.com\n    api_key: \"{{env.DEMO_TWENTY_API_KEY}}\"\n",
    );

    seed_profile_main(&root);
    root
}

/// Every runnable example playbook validates cleanly, the ones that bind the
/// repository connectors against them, in one project with one connector
/// setup. A new example is covered once its id is added here.
#[test]
fn the_demo_playbooks_validate() {
    let dir = tempfile::tempdir().unwrap();
    let root = setup(dir.path());
    let ids = [
        "sentry-triage",
        "release-announce",
        "release-heartbeat",
        "inbox-triage",
        "whatsapp-inbox",
        "ci-failure-triage",
        "dependency-security-fix",
        "bulk-reminders",
        "crm-cleanup",
        "review-triage",
        "review-panel",
        "review-panel-list",
        "task-check-first",
    ];
    for id in ids {
        register_playbook(
            &root,
            id,
            "1.0.0",
            &repo_playbook_yaml(&format!("{id}.yaml")),
        );
    }
    for id in ids {
        let out = apb_ok(&root, &["validate", id]);
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(
            stdout.contains(&format!("{id}: OK")),
            "{id} should validate cleanly: {stdout}"
        );
    }
}

/// A bare project in `dir` with its own config dir and the profile `main`.
fn bare(dir: &Path) -> std::path::PathBuf {
    let root = dir.to_path_buf();
    fs::create_dir_all(root.join("cfg")).unwrap();
    fs::create_dir_all(root.join("home")).unwrap();
    apb_core::registry::init_project(&root).unwrap();
    seed_profile_main(&root);
    root
}

/// Copies every file under `src` into `dst`, keeping relative paths.
fn copy_tree(src: &Path, dst: &Path) {
    for entry in fs::read_dir(src).unwrap().flatten() {
        let to = dst.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            fs::create_dir_all(&to).unwrap();
            copy_tree(&entry.path(), &to);
        } else {
            fs::create_dir_all(dst).unwrap();
            fs::copy(entry.path(), &to).unwrap();
        }
    }
}

fn events(root: &Path, run_id: &str) -> String {
    fs::read_to_string(root.join(".apb/runs").join(run_id).join("events.jsonl")).unwrap_or_default()
}

fn poll(what: &str, mut f: impl FnMut() -> bool) {
    let start = std::time::Instant::now();
    while !f() {
        assert!(
            start.elapsed() < std::time::Duration::from_secs(60),
            "timed out waiting for {what}"
        );
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
}

/// The shipped demo, laid out as `apb demo` installs it: its playbook and
/// script as version 1.0.0, made current, its scenario next to it.
fn lay_out_demo(root: &Path) {
    let assets = Path::new(env!("CARGO_MANIFEST_DIR")).join("../apb-core/assets/demo");
    let pb = root.join(".apb/playbooks/demo-report");
    fs::create_dir_all(pb.join("1.0.0/scripts")).unwrap();
    fs::copy(assets.join("playbook.yaml"), pb.join("1.0.0/playbook.yaml")).unwrap();
    fs::copy(
        assets.join("scripts/write-report.sh"),
        pb.join("1.0.0/scripts/write-report.sh"),
    )
    .unwrap();
    copy_tree(&assets.join("evals"), &pb.join("evals"));
    fs::write(pb.join("current"), "1.0.0").unwrap();
}

/// The demo validates, its scenario passes at quick, and a real run stops
/// at the review, then finishes with `report.md` once a person approves,
/// its `report` output set.
#[test]
fn the_shipped_demo_runs_to_its_review_and_finishes_with_the_report() {
    let dir = tempfile::tempdir().unwrap();
    let root = bare(dir.path());
    lay_out_demo(&root);
    let out = apb_ok(&root, &["validate", "demo-report"]);
    assert!(String::from_utf8_lossy(&out.stdout).contains("demo-report: OK"));
    apb_ok(
        &root,
        &[
            "eval",
            "demo-report",
            "--level",
            "quick",
            "--scenario",
            "approve",
        ],
    );

    let out = apb_ok(&root, &["run", "demo-report", "--detach"]);
    let stdout = String::from_utf8_lossy(&out.stdout);
    let run_id = stdout
        .lines()
        .find_map(|l| l.strip_prefix("run started: "))
        .unwrap_or_else(|| panic!("{stdout}"))
        .trim()
        .to_string();
    poll("the review", || {
        events(&root, &run_id).contains("\"review_requested\"")
    });
    assert!(!events(&root, &run_id).contains("\"run_finished\""));
    apb_ok(
        &root,
        &["review", &run_id, "review", "--decision", "approve"],
    );
    poll("the run to finish", || {
        events(&root, &run_id).contains("\"run_finished\"")
    });
    crate::common::wait_released(&root, &run_id);

    let report = fs::read_to_string(root.join("report.md")).unwrap();
    assert!(report.contains(&run_id), "{report}");
    let out = apb_ok(&root, &["runs", &run_id, "--json"]);
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["run_status"], "succeeded", "{v}");
    let output = &v["run_outputs"]["outputs"][0];
    assert_eq!(
        (output["name"].as_str(), output["state"].as_str()),
        (Some("report"), Some("value"))
    );
    assert!(
        output["value"].as_str().unwrap().contains("REPORT_WRITTEN"),
        "{output}"
    );
}

/// The "check the task first" example's scenarios pass at quick, and the
/// unattended one fails where a person would be asked, `hitl_required`.
#[test]
fn the_check_first_example_scenarios_pass_at_quick() {
    let dir = tempfile::tempdir().unwrap();
    let root = bare(dir.path());
    let id = "task-check-first";
    register_playbook(
        &root,
        id,
        "1.0.0",
        &repo_playbook_yaml(&format!("{id}.yaml")),
    );
    copy_tree(
        &Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../examples/playbooks")
            .join(id),
        &root.join(".apb/playbooks").join(id),
    );
    let mut played = Vec::new();
    for name in ["clear-task", "unclear-then-clarified", "unclear-unattended"] {
        let out = playbook(
            &root,
            &["eval", id, "--level", "quick", "--scenario", name, "--json"],
        );
        let v: serde_json::Value = serde_json::from_slice(&out.stdout)
            .unwrap_or_else(|e| panic!("{name}: {e}: {}", String::from_utf8_lossy(&out.stderr)));
        let report = &v["reports"][0];
        assert_eq!(report["verdict"], "passed", "{name}: {report:#}");
        assert_eq!(out.status.code(), Some(0), "{name}");
        played.push(report["run_id"].as_str().unwrap().to_string());
    }
    let unattended = fs::read_to_string(
        root.join(".apb/scenario-runs")
            .join(&played[2])
            .join("events.jsonl"),
    )
    .unwrap();
    assert!(unattended.contains("hitl_required"), "{unattended}");
}

/// `apb demo` in an empty config dir installs the demo once, `--run`
/// starts it to the review (`--remove` is refused while it waits there),
/// approving finishes it with `report.md`, and `--remove` unregisters the
/// project and deletes it.
#[test]
fn apb_demo_installs_runs_to_the_review_and_removes() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().to_path_buf();
    fs::create_dir_all(root.join("home")).unwrap();
    let demo = |args: &[&str]| {
        crate::common::apb_std()
            .args(args)
            .current_dir(&root)
            .env("APB_CONFIG_DIR", root.join("cfg"))
            .env("HOME", root.join("home"))
            .env_remove("APB_RUN_DIR")
            .env_remove("APB_NODE_ID")
            .output()
            .unwrap()
    };
    let ok = |args: &[&str]| {
        let out = demo(args);
        assert!(out.status.success(), "`apb {}`: {out:?}", args.join(" "));
        out
    };
    let project = root.join("cfg/demo");
    let first: serde_json::Value = serde_json::from_slice(&ok(&["demo", "--json"]).stdout).unwrap();
    assert_eq!(first["playbook"], "demo-report");
    let ws = first["workspace_id"]
        .as_str()
        .expect("registered")
        .to_string();
    let again: serde_json::Value = serde_json::from_slice(&ok(&["demo", "--json"]).stdout).unwrap();
    assert_eq!(again, first);
    let listed = String::from_utf8_lossy(&ok(&["projects", "list"]).stdout).into_owned();
    assert!(listed.contains(&ws), "{listed}");

    let out = ok(&["demo", "--run"]);
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    let run_id = stdout
        .lines()
        .find_map(|l| l.strip_prefix("run started: "))
        .unwrap_or_else(|| panic!("{stdout}"))
        .trim()
        .to_string();
    assert!(stdout.contains("apb review"), "{stdout}");
    poll("the review", || {
        events(&project, &run_id).contains("\"review_requested\"")
    });
    // A run waiting at its review still drives the demo: removing it is
    // refused and leaves the project whole.
    let busy = demo(&["demo", "--remove"]);
    assert!(!busy.status.success(), "{busy:?}");
    assert!(
        String::from_utf8_lossy(&busy.stderr).contains("run"),
        "{busy:?}"
    );
    assert!(project.join(".apb/runs").join(&run_id).is_dir());
    let decided = crate::common::apb_std()
        .args(["review", &run_id, "review", "--decision", "approve"])
        .current_dir(&project)
        .env("APB_CONFIG_DIR", root.join("cfg"))
        .env("HOME", root.join("home"))
        .output()
        .unwrap();
    assert!(decided.status.success(), "{decided:?}");
    poll("the run to finish", || {
        events(&project, &run_id).contains("\"run_finished\"")
    });
    crate::common::wait_released(&project, &run_id);
    assert!(
        fs::read_to_string(project.join("report.md"))
            .unwrap()
            .contains(&run_id)
    );

    let removed: serde_json::Value =
        serde_json::from_slice(&ok(&["demo", "--remove", "--json"]).stdout).unwrap();
    assert_eq!(removed["removed"], true);
    assert!(!project.exists());
    let listed = String::from_utf8_lossy(&ok(&["projects", "list"]).stdout).into_owned();
    assert!(!listed.contains(&ws), "{listed}");
}
