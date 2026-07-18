//! Phase 2 CLI: detect / adopt / subscriptions. Tests are non-interactive
//! (stdin null -> is_terminal == false): commands do not hang and do not
//! change onboarding state without explicit flags. PATH is empty - detect
//! spawns nothing.

use std::path::Path;
use std::process::{Command, Stdio};

fn playbook(dir: &Path, empty_bin: &Path, args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_apb"))
        .args(args)
        .current_dir(dir)
        .env("APB_CONFIG_DIR", dir.join("cfg"))
        .env("HOME", dir.join("home"))
        .env("PATH", empty_bin)
        .env("APB_NO_REGISTRY", "1")
        .stdin(Stdio::null())
        .output()
        .unwrap()
}

fn onboarding_raw(dir: &Path) -> Option<String> {
    std::fs::read_to_string(dir.join("cfg/state/onboarding.json")).ok()
}

struct Ctx {
    dir: tempfile::TempDir,
    _bin: tempfile::TempDir,
    bin: std::path::PathBuf,
}

fn setup() -> Ctx {
    let dir = tempfile::tempdir().unwrap();
    let bin = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("cfg")).unwrap();
    std::fs::create_dir_all(dir.path().join("home")).unwrap();
    Ctx {
        bin: bin.path().to_path_buf(),
        dir,
        _bin: bin,
    }
}

#[test]
fn detect_runs_and_leaves_state_uninitialized() {
    let c = setup();
    let out = playbook(c.dir.path(), &c.bin, &["detect"]);
    assert!(
        out.status.success(),
        "detect failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let s = String::from_utf8_lossy(&out.stdout);
    assert!(s.contains("\"agents\""), "detect must print agents: {s}");
    // Non-TTY: no onboarding state change at all.
    assert!(
        onboarding_raw(c.dir.path()).is_none(),
        "detect must not write onboarding state"
    );
}

#[test]
fn bare_subscriptions_non_tty_does_not_hang_or_change_state() {
    let c = setup();
    let out = playbook(c.dir.path(), &c.bin, &["subscriptions"]);
    assert!(out.status.success());
    let s = String::from_utf8_lossy(&out.stdout);
    assert!(s.contains("no subscriptions declared"));
    assert!(
        onboarding_raw(c.dir.path()).is_none(),
        "bare subscriptions must not change state"
    );
}

#[test]
fn subscriptions_set_flag_declares_and_lists() {
    let c = setup();
    let out = playbook(
        c.dir.path(),
        &c.bin,
        &[
            "subscriptions",
            "--set",
            "claude:max:full",
            "--set",
            "opencode",
        ],
    );
    assert!(
        out.status.success(),
        "set failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let raw = onboarding_raw(c.dir.path()).expect("state written");
    assert!(
        raw.contains("configured"),
        "state must be configured: {raw}"
    );

    let list = playbook(c.dir.path(), &c.bin, &["subscriptions"]);
    let s = String::from_utf8_lossy(&list.stdout);
    assert!(s.contains("claude"), "list must show claude: {s}");
    assert!(s.contains("opencode"), "list must show opencode: {s}");
}

#[test]
fn subscriptions_set_rejects_malformed_and_does_not_persist() {
    let c = setup();
    // Invalid coverage -> rejection, exit 2, state is not written.
    let out = playbook(
        c.dir.path(),
        &c.bin,
        &["subscriptions", "--set", "claude:max:bogus"],
    );
    assert!(!out.status.success(), "malformed coverage must fail");
    assert_eq!(out.status.code(), Some(2));
    assert!(
        onboarding_raw(c.dir.path()).is_none(),
        "malformed input must not persist state"
    );
}

#[test]
fn subscriptions_decline_marks_state() {
    let c = setup();
    let out = playbook(c.dir.path(), &c.bin, &["subscriptions", "--decline"]);
    assert!(out.status.success());
    let raw = onboarding_raw(c.dir.path()).expect("state written");
    assert!(raw.contains("declined"), "state must be declined: {raw}");
}

#[test]
fn profile_list_non_tty_does_not_prompt_or_change_state() {
    let c = setup();
    let dir = c.dir.path();
    assert!(playbook(dir, &c.bin, &["init"]).status.success());
    // `apb profile list` in non-TTY: prints JSON, does not offer the survey,
    // does not write onboarding state (the onboarding trigger is gated on
    // interactive stdin).
    let out = playbook(dir, &c.bin, &["profile", "list"]);
    assert!(
        out.status.success(),
        "profile list failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        onboarding_raw(dir).is_none(),
        "profile list must not change onboarding state in non-TTY"
    );
    // The subscriptions hint is not printed in non-interactive mode.
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(
        !err.contains("apb subscriptions"),
        "must not offer onboarding in non-TTY: {err}"
    );
}

#[test]
fn declined_state_is_not_reoffered() {
    let c = setup();
    let dir = c.dir.path();
    // Decline the survey, then detect/profile must not re-offer it and must
    // not touch the state (it stays declined).
    assert!(
        playbook(dir, &c.bin, &["subscriptions", "--decline"])
            .status
            .success()
    );
    let before = onboarding_raw(dir).expect("declined written");
    assert!(before.contains("declined"));
    let d = playbook(dir, &c.bin, &["detect"]);
    assert!(d.status.success());
    let after = onboarding_raw(dir).expect("state still present");
    assert!(
        after.contains("declined"),
        "declined must stay declined after detect: {after}"
    );
}

#[test]
fn adopt_reports_missing_profile() {
    let c = setup();
    let dir = c.dir.path();
    assert!(playbook(dir, &c.bin, &["init"]).status.success());
    // The playbook references a nonexistent profile named ghost.
    let vdir = dir.join(".apb/playbooks/wf1/1.0.0");
    std::fs::create_dir_all(&vdir).unwrap();
    let yaml = "schema: 1\nid: wf1\nname: W\nversion: 1.0.0\nnodes:\n  - { id: start, type: start }\n  - { id: a, type: agent_task, prompt: \"do\", profile: ghost }\n  - { id: done, type: finish, outcome: success }\nedges:\n  - { from: start, to: a }\n  - { from: a, to: done }\n";
    std::fs::write(vdir.join("playbook.yaml"), yaml).unwrap();
    std::fs::write(dir.join(".apb/playbooks/wf1/current"), "1.0.0").unwrap();

    let out = playbook(dir, &c.bin, &["adopt", "wf1"]);
    assert!(
        out.status.success(),
        "adopt failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let s = String::from_utf8_lossy(&out.stdout);
    assert!(
        s.contains("profile_missing"),
        "adopt must flag missing profile: {s}"
    );
}
