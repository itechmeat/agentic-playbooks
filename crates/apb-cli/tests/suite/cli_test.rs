use assert_cmd::Command;
use predicates::prelude::*;
use std::fs;
use std::path::Path;

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
    fs::create_dir_all(dir.path().join(".apb/profiles/architect")).unwrap();
    dir
}

#[test]
fn init_creates_structure() {
    let dir = tempfile::tempdir().unwrap();
    playbook()
        .arg("init")
        .current_dir(dir.path())
        .assert()
        .success()
        .stdout(predicate::str::contains(".apb"));
    assert!(dir.path().join(".apb/playbooks").is_dir());
}

#[test]
fn list_shows_playbook() {
    let dir = seeded_dir();
    playbook()
        .arg("list")
        .current_dir(dir.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("implement-task"))
        .stdout(predicate::str::contains("1.0.0"));
}

#[test]
fn validate_ok_playbook() {
    let dir = seeded_dir();
    playbook()
        .args(["validate", "implement-task"])
        .current_dir(dir.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("OK"));
}

#[test]
fn validate_broken_playbook_fails_with_code() {
    let dir = seeded_dir();
    let vdir = dir.path().join(".apb/playbooks/implement-task/1.0.0");
    let bad = VALID.replace("{{params.task}}", "{{params.ghost}}");
    fs::write(vdir.join("playbook.yaml"), bad).unwrap();
    playbook()
        .args(["validate", "implement-task"])
        .current_dir(dir.path())
        .assert()
        .code(1)
        .stdout(predicate::str::contains("V13"));
}

/// Whole-project validation must not skip a playbook that fails to load (the
/// listing drops it, so it used to vanish and the run exited 0): a playbook
/// with unparseable YAML is named with its load error and the exit code is 1,
/// while the healthy playbook next to it is still reported OK.
#[test]
fn validate_all_reports_a_playbook_that_fails_to_load() {
    let dir = seeded_dir();
    let broken = dir.path().join(".apb/playbooks/broken-one");
    fs::create_dir_all(broken.join("1.0.0")).unwrap();
    fs::write(broken.join("1.0.0/playbook.yaml"), "nodes: [unclosed").unwrap();
    fs::write(broken.join("current"), "1.0.0").unwrap();
    playbook()
        .arg("validate")
        .current_dir(dir.path())
        .assert()
        .code(1)
        .stdout(predicate::str::contains("broken-one: error"))
        .stdout(predicate::str::contains("implement-task: OK"));
}

/// `apb validate` checks the connector rules run start checks: a playbook
/// binding an installed connector whose manifest no longer loads is a V42
/// error here, not only a code-less refusal when the run starts.
#[test]
fn validate_reports_v42_for_an_installed_broken_connector() {
    let dir = seeded_dir();
    let cfg = tempfile::tempdir().unwrap();
    let conn = cfg.path().join("connectors/brokenhook");
    fs::create_dir_all(&conn).unwrap();
    fs::write(
        conn.join("connector.yaml"),
        "name: brokenhook\nversion: 0.1.0\nfunctions:\n  - name: inbox_read\n    description: read pending\n    read_only: true\n    response_pick: [events]\n    inbox:\n      op: read\n",
    )
    .unwrap();
    let pdir = dir.path().join(".apb/playbooks/hooked");
    fs::create_dir_all(pdir.join("1.0.0")).unwrap();
    fs::write(
        pdir.join("1.0.0/playbook.yaml"),
        "schema: 2\nid: hooked\nname: hooked\nversion: 1.0.0\nnodes:\n  - { id: s, type: start }\n  - id: a\n    type: agent_task\n    prompt: hi\n    profile: architect\n    connectors: [{ name: brokenhook, functions: [inbox_read] }]\n  - { id: f, type: finish, outcome: success }\nedges:\n  - { from: s, to: a }\n  - { from: a, to: f }\n",
    )
    .unwrap();
    fs::write(pdir.join("current"), "1.0.0").unwrap();
    playbook()
        .arg("validate")
        .env("APB_CONFIG_DIR", cfg.path())
        .current_dir(dir.path())
        .assert()
        .code(1)
        .stdout(predicate::str::contains("hooked: error V42"));
}

/// Whole-project validation checks zcode profile models against apb's
/// allowlist: the bare id and the legacy `zai-individual/` spelling pass, any
/// other model is an error naming the allowlist.
#[test]
fn validate_refuses_a_zcode_profile_model_off_the_allowlist() {
    let dir = seeded_dir();
    let profile =
        |model: &str| format!("name: architect\nexecutor:\n  agent: zcode\n  model: {model}\n");
    let path = dir.path().join(".apb/profiles/architect/profile.yaml");
    for ok in ["GLM-5.3-Flash@high", "zai-individual/GLM-5.3"] {
        fs::write(&path, profile(ok)).unwrap();
        playbook()
            .arg("validate")
            .current_dir(dir.path())
            .assert()
            .success();
    }
    fs::write(&path, profile("GLM-5-Turbo")).unwrap();
    playbook()
        .arg("validate")
        .current_dir(dir.path())
        .assert()
        .code(1)
        .stdout(predicate::str::contains(
            "profile architect: error zcode_model_not_allowed",
        ))
        .stdout(predicate::str::contains("GLM-5.3-Flash"));
}

/// A profile whose agent (or a fallback) has no invocation form fails every
/// run at start, in cli and in host mode; `apb validate` refuses it up front
/// instead of passing it silently.
#[test]
fn validate_refuses_a_profile_agent_without_an_invocation_form() {
    let dir = seeded_dir();
    let path = dir.path().join(".apb/profiles/architect/profile.yaml");
    fs::write(
        &path,
        "name: architect\nexecutor:\n  agent: claude\n  model: sonnet\n  fallbacks:\n    - agent: pi\n      model: any\n",
    )
    .unwrap();
    playbook()
        .arg("validate")
        .current_dir(dir.path())
        .assert()
        .code(1)
        .stdout(predicate::str::contains(
            "profile architect: error agent_no_invocation agent `pi`",
        ))
        .stdout(predicate::str::contains("agent `claude`").not());
}

/// `apb validate` and `apb doctor` are the local preflight: besides the schema
/// they check what a run would trip over on this machine, as warnings. A model
/// id outside the agent's known list (a typo or a made-up id), a `requires`
/// the machine does not meet, and a bound connector with no configured account
/// all used to pass both commands silently (issue #137).
#[test]
fn validate_and_doctor_report_an_unknown_model_unmet_requires_and_an_unconfigured_connector() {
    let dir = seeded_dir();
    let cfg = tempfile::tempdir().unwrap();
    playbook()
        .args(["connector", "init", "widget"])
        .env("APB_CONFIG_DIR", cfg.path())
        .current_dir(dir.path())
        .assert()
        .success();
    fs::write(
        dir.path().join(".apb/profiles/architect/profile.yaml"),
        "name: architect\nexecutor:\n  agent: claude\n  model: claude-made-up-9\n",
    )
    .unwrap();
    let pdir = dir.path().join(".apb/playbooks/needy");
    fs::create_dir_all(pdir.join("1.0.0")).unwrap();
    fs::write(
        pdir.join("1.0.0/playbook.yaml"),
        "schema: 2\nid: needy\nname: needy\nversion: 1.0.0\nrequires:\n  commands: [apb-no-such-command-137]\nnodes:\n  - { id: s, type: start }\n  - id: a\n    type: agent_task\n    prompt: hi\n    profile: { name: architect, scope: project }\n    expected_duration: 1m\n    connectors: [{ name: widget, functions: [ping] }]\n  - { id: f, type: finish, outcome: success }\nedges:\n  - { from: s, to: a }\n  - { from: a, to: f }\n",
    )
    .unwrap();
    fs::write(pdir.join("current"), "1.0.0").unwrap();

    playbook()
        .arg("validate")
        .env("APB_CONFIG_DIR", cfg.path())
        .current_dir(dir.path())
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "profile architect: warning model_unknown",
        ))
        .stdout(predicate::str::contains("claude-made-up-9"))
        .stdout(predicate::str::contains("needy: warning requires_unmet"))
        .stdout(predicate::str::contains("command:apb-no-such-command-137"))
        .stdout(predicate::str::contains(
            "needy: warning connector_unconfigured",
        ));
    let doctor = playbook()
        .arg("doctor")
        .env("APB_CONFIG_DIR", cfg.path())
        .current_dir(dir.path())
        .assert()
        .stdout(predicate::str::contains("command:apb-no-such-command-137"))
        .stdout(predicate::str::contains("connector `widget`"));
    // One finding per profile, however many ways the playbooks name it
    // (`architect` and `{ name: architect, scope: project }`).
    let out = String::from_utf8_lossy(&doctor.get_output().stdout).to_string();
    assert_eq!(out.matches("claude-made-up-9").count(), 1, "{out}");
}

/// A `model_policy` in the global config (e.g. an org allowlist) is
/// enforced by `apb validate`: a profile model the policy does not allow is an
/// error, a model it allows passes.
#[test]
fn validate_refuses_a_profile_model_the_config_policy_forbids() {
    let dir = seeded_dir();
    let cfg = tempfile::tempdir().unwrap();
    fs::write(
        cfg.path().join("config.yaml"),
        "model_policy:\n  - agent: zcode\n    allow: [GLM-5.3]\n    reason: org allowlist\n",
    )
    .unwrap();
    let path = dir.path().join(".apb/profiles/architect/profile.yaml");
    let profile =
        |model: &str| format!("name: architect\nexecutor:\n  agent: zcode\n  model: {model}\n");
    fs::write(&path, profile("GLM-5.3-Flash@high")).unwrap();
    playbook()
        .arg("validate")
        .env("APB_CONFIG_DIR", cfg.path())
        .current_dir(dir.path())
        .assert()
        .code(1)
        .stdout(predicate::str::contains(
            "profile architect: error model_policy_violation",
        ))
        .stdout(predicate::str::contains("org allowlist"));
    fs::write(&path, profile("zai-individual/GLM-5.3@low")).unwrap();
    playbook()
        .arg("validate")
        .env("APB_CONFIG_DIR", cfg.path())
        .current_dir(dir.path())
        .assert()
        .success();
}

/// A current Claude id validates without a finding; an unlisted id of a
/// known Claude family (a model newer than this binary) passes with an info
/// note in `apb validate` and `apb doctor`; an unknown family still warns.
#[test]
fn validate_accepts_current_and_newer_claude_family_ids() {
    let dir = seeded_dir();
    let cfg = tempfile::tempdir().unwrap();
    // A stub claude on a PATH of our own and an empty HOME: whether the
    // machine running the suite has claude installed must not matter (a
    // missing agent reads `agent_not_installed` before any model note).
    let bin = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    let claude = bin.path().join("claude");
    fs::write(&claude, "#!/bin/sh\necho 2.1.0\n").unwrap();
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&claude, fs::Permissions::from_mode(0o755)).unwrap();
    }
    let path_var =
        std::env::join_paths([bin.path(), Path::new("/usr/bin"), Path::new("/bin")]).unwrap();
    let path = dir.path().join(".apb/profiles/architect/profile.yaml");
    let profile =
        |model: &str| format!("name: architect\nexecutor:\n  agent: claude\n  model: {model}\n");
    let apb = || {
        let mut cmd = playbook();
        cmd.env("APB_CONFIG_DIR", cfg.path())
            .env("HOME", home.path())
            .env("PATH", &path_var)
            .current_dir(dir.path());
        cmd
    };
    let validate = || apb().arg("validate").assert().success();
    fs::write(&path, profile("claude-haiku-5-5")).unwrap();
    validate().stdout(predicate::str::contains("profile architect").not());

    fs::write(&path, profile("claude-sonnet-6-0")).unwrap();
    validate()
        .stdout(predicate::str::contains(
            "profile architect: info model_new_in_family",
        ))
        .stdout(predicate::str::contains("warning model_unknown").not());
    apb()
        .arg("doctor")
        .assert()
        .stdout(predicate::str::contains("model_new_in_family"))
        .stdout(predicate::str::contains("model_unknown").not());

    fs::write(&path, profile("claude-foo-5-5")).unwrap();
    validate().stdout(predicate::str::contains(
        "profile architect: warning model_unknown",
    ));
}

#[test]
fn list_without_apb_dir_fails() {
    let dir = tempfile::tempdir().unwrap();
    playbook()
        .arg("list")
        .current_dir(dir.path())
        .assert()
        .code(2);
}

#[test]
fn dashboard_is_visible_and_serve_alias_parses() {
    // Primary name is visible; the hidden `serve` alias is not listed.
    playbook()
        .arg("--help")
        .assert()
        .success()
        .stdout(predicate::str::contains("dashboard"))
        .stdout(predicate::str::contains("Start the web dashboard"))
        // Trailing space distinguishes the alias from the unrelated `server`
        // command, which also starts with the substring "serve".
        .stdout(predicate::str::contains("  serve ").not());

    // Hidden alias still resolves to the same command (help works).
    playbook()
        .args(["serve", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Start the web dashboard"));

    playbook()
        .args(["dashboard", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Start the web dashboard"));
}

/// `apb trash`: the listing shows a deletion, a restore by id brings the
/// playbook back, and a restore whose id is taken again is refused with exit
/// code 1 and a message saying why (scripts tell it apart from not found, 2).
#[test]
fn trash_list_restore_and_conflict() {
    let dir = seeded_dir();
    apb_core::versioning::delete_playbook(dir.path(), "implement-task", 1_700_000_000_000).unwrap();

    let out = playbook()
        .args(["trash", "list", "--json"])
        .current_dir(dir.path())
        .output()
        .unwrap();
    assert!(out.status.success());
    let listed: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(listed[0]["name"], "implement-task-1700000000000");
    playbook()
        .args(["trash", "list"])
        .current_dir(dir.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("2023-11-14T22:13:20Z"));

    playbook()
        .args(["trash", "restore", "implement-task"])
        .current_dir(dir.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("restored implement-task"));
    playbook()
        .args(["validate", "implement-task"])
        .current_dir(dir.path())
        .assert()
        .success();

    apb_core::versioning::delete_playbook(dir.path(), "implement-task", 1).unwrap();
    let vdir = dir.path().join(".apb/playbooks/implement-task/1.0.0");
    fs::create_dir_all(&vdir).unwrap();
    fs::write(vdir.join("playbook.yaml"), VALID).unwrap();
    playbook()
        .args(["trash", "restore", "implement-task"])
        .current_dir(dir.path())
        .assert()
        .code(1)
        .stderr(predicate::str::contains("exists again"));
    playbook()
        .args(["trash", "restore", "no-such"])
        .current_dir(dir.path())
        .assert()
        .code(2);
}

/// `apb trust`: the listing shows every approval, a revoke by id removes all
/// of that id's approvals of the given kind and prints them, and a target
/// that matches nothing exits 2.
#[test]
fn trust_list_and_revoke_by_id() {
    let cfg = tempfile::tempdir().unwrap();
    fs::write(
        cfg.path().join("trust.json"),
        r#"{"schema_version":1,"approved":{
            "sha256:aa":{"id":"demo","origin_kind":"locally_approved","approved_at_ms":1,"kind":"playbook"},
            "sha256:bb":{"id":"demo","origin_kind":"locally_approved","approved_at_ms":2,"kind":"playbook"},
            "sha256:cc":{"id":"keep","origin_kind":"agent_generated","approved_at_ms":3,"kind":"profile_bundle"}}}"#,
    )
    .unwrap();
    let apb = |args: &[&str]| {
        let mut cmd = playbook();
        cmd.args(args).env("APB_CONFIG_DIR", cfg.path());
        cmd
    };

    let out = apb(&["trust", "list", "--json"]).output().unwrap();
    assert!(out.status.success());
    let listed: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(listed.as_array().unwrap().len(), 3, "{listed}");

    apb(&["trust", "revoke", "demo", "--kind", "playbook"])
        .assert()
        .success()
        .stdout(predicate::str::contains("revoked 2 approval(s)"))
        .stdout(predicate::str::contains("sha256:bb"));
    let out = apb(&["trust", "list", "--json"]).output().unwrap();
    let listed: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(listed[0]["id"], "keep", "{listed}");
    assert_eq!(listed.as_array().unwrap().len(), 1, "{listed}");

    apb(&["trust", "revoke", "demo"]).assert().code(2);
}
