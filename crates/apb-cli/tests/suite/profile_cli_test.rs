use std::fs;
use std::path::Path;
use std::process::Command;

const NAMED: &str = "schema: 1\nid: a\nname: W\nversion: 1.0.0\nexecutors:\n  main:\n    agent: claude\n    model: haiku\ndefaults:\n  executor: main\nnodes:\n  - { id: start, type: start }\n  - { id: t, type: agent_task, prompt: \"do\" }\n  - { id: done, type: finish, outcome: success }\nedges:\n  - { from: start, to: t }\n  - { from: t, to: done }\n";

fn playbook(dir: &Path, args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_apb"))
        .args(args)
        .current_dir(dir)
        .env("APB_CONFIG_DIR", dir.join("cfg"))
        .env("HOME", dir.join("home"))
        .env("APB_NO_REGISTRY", "1")
        .output()
        .unwrap()
}

/// Like `playbook`, but requires a zero exit code; stderr is shown on
/// failure - otherwise a failing command would be masked as empty stdout.
fn apb_ok(dir: &Path, args: &[&str]) -> std::process::Output {
    let out = playbook(dir, args);
    assert!(
        out.status.success(),
        "`playbook {}` failed: {}",
        args.join(" "),
        String::from_utf8_lossy(&out.stderr)
    );
    out
}

fn seed(dir: &Path) {
    apb_ok(dir, &["init"]);
    let vdir = dir.join(".apb/playbooks/a/1.0.0");
    fs::create_dir_all(&vdir).unwrap();
    fs::write(vdir.join("playbook.yaml"), NAMED).unwrap();
    fs::write(dir.join(".apb/playbooks/a/current"), "1.0.0").unwrap();
}

/// Like `playbook`, but with $EDITOR set (for profile edit).
fn apb_with_editor(dir: &Path, editor: &Path, args: &[&str]) -> std::process::Output {
    apb_with_editor_str(dir, &editor.to_string_lossy(), args)
}

/// Like `apb_with_editor`, but $EDITOR is an arbitrary string (may carry
/// arguments, e.g. "ed.sh --wait").
fn apb_with_editor_str(dir: &Path, editor: &str, args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_apb"))
        .args(args)
        .current_dir(dir)
        .env("APB_CONFIG_DIR", dir.join("cfg"))
        .env("HOME", dir.join("home"))
        .env("APB_NO_REGISTRY", "1")
        .env("EDITOR", editor)
        .output()
        .unwrap()
}

fn write_script(path: &Path, body: &str) {
    use std::os::unix::fs::PermissionsExt;
    fs::write(path, format!("#!/bin/sh\n{body}\n")).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
}

#[test]
fn profile_write_creates_then_stale_digest_conflicts() {
    let dir = tempfile::tempdir().unwrap();
    fs::create_dir_all(dir.path().join("cfg")).unwrap();
    fs::create_dir_all(dir.path().join("home")).unwrap();
    apb_ok(dir.path(), &["init"]);

    // write creates the profile.
    apb_ok(
        dir.path(),
        &[
            "profile",
            "write",
            "p1",
            "--agent",
            "claude",
            "--model",
            "haiku",
            "--description",
            "d",
        ],
    );
    assert!(dir.path().join(".apb/profiles/p1/profile.yaml").is_file());

    // profile show returns it.
    let out = apb_ok(dir.path(), &["profile", "show", "p1"]);
    let s = String::from_utf8_lossy(&out.stdout);
    assert!(s.contains("claude"), "show missing agent: {s}");

    // A repeat write with a stale expected-digest -> conflict (exit 2).
    let out = playbook(
        dir.path(),
        &[
            "profile",
            "write",
            "p1",
            "--agent",
            "claude",
            "--model",
            "sonnet",
            "--expected-digest",
            "sha256:deadbeef",
        ],
    );
    assert!(!out.status.success(), "stale expected-digest must conflict");
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("conflict"), "expected conflict, got: {err}");
}

#[test]
fn profile_edit_roundtrip_and_concurrent_change_conflicts() {
    let dir = tempfile::tempdir().unwrap();
    fs::create_dir_all(dir.path().join("cfg")).unwrap();
    fs::create_dir_all(dir.path().join("home")).unwrap();
    apb_ok(dir.path(), &["init"]);
    apb_ok(
        dir.path(),
        &[
            "profile", "write", "p1", "--agent", "claude", "--model", "haiku",
        ],
    );

    // $EDITOR that rewrites SOUL.md (second arg) - a plain round-trip.
    let ok_editor = dir.path().join("editor_ok.sh");
    write_script(&ok_editor, "printf 'EDITED-SOUL' > \"$2\"");
    let out = apb_with_editor(dir.path(), &ok_editor, &["profile", "edit", "p1"]);
    assert!(
        out.status.success(),
        "edit round-trip failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let soul = fs::read_to_string(dir.path().join(".apb/profiles/p1/SOUL.md")).unwrap();
    assert_eq!(soul, "EDITED-SOUL", "edit must persist the new SOUL");

    // $EDITOR that CONCURRENTLY changes the real profile on disk (simulating
    // a concurrent edit), then edits the temp file: saving must conflict via
    // CAS (the digest moved since digest_before was captured).
    let real_soul = dir.path().join(".apb/profiles/p1/SOUL.md");
    let concurrent = dir.path().join("editor_concurrent.sh");
    write_script(
        &concurrent,
        &format!(
            "printf 'CONCURRENT' > '{}'\nprintf 'MY-EDIT' > \"$2\"",
            real_soul.display()
        ),
    );
    let out = apb_with_editor(dir.path(), &concurrent, &["profile", "edit", "p1"]);
    assert!(
        !out.status.success(),
        "concurrent change must conflict, not clobber"
    );
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("conflict"), "expected conflict, got: {err}");
}

#[test]
fn profile_edit_handles_editor_with_arguments() {
    let dir = tempfile::tempdir().unwrap();
    fs::create_dir_all(dir.path().join("cfg")).unwrap();
    fs::create_dir_all(dir.path().join("home")).unwrap();
    apb_ok(dir.path(), &["init"]);
    apb_ok(
        dir.path(),
        &[
            "profile", "write", "p1", "--agent", "claude", "--model", "haiku",
        ],
    );

    // $EDITOR with an argument: "ed.sh --wait". Without command-line
    // parsing, the playbook would try to run the whole string as the
    // executable name and fail.
    // The script edits ITS own last argument (SOUL.md).
    let ed = dir.path().join("ed.sh");
    write_script(
        &ed,
        "for a in \"$@\"; do last=\"$a\"; done\nprintf 'ARGS-EDITED' > \"$last\"",
    );
    let editor = format!("{} --wait", ed.display());
    let out = apb_with_editor_str(dir.path(), &editor, &["profile", "edit", "p1"]);
    assert!(
        out.status.success(),
        "edit with an argument-bearing $EDITOR must work: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let soul = fs::read_to_string(dir.path().join(".apb/profiles/p1/SOUL.md")).unwrap();
    assert_eq!(soul, "ARGS-EDITED");
}

#[test]
fn migrate_dry_run_then_apply_then_profile_list_show() {
    let dir = tempfile::tempdir().unwrap();
    fs::create_dir_all(dir.path().join("cfg")).unwrap();
    fs::create_dir_all(dir.path().join("home")).unwrap();
    seed(dir.path());

    // Dry-run: prints the plan, writes nothing.
    let out = apb_ok(dir.path(), &["migrate"]);
    let s = String::from_utf8_lossy(&out.stdout);
    assert!(
        s.contains("profile `main`"),
        "dry-run plan missing profile: {s}"
    );
    assert!(
        !dir.path().join(".apb/profiles/main").exists(),
        "dry-run must not write"
    );

    // Apply: creates the profile and a new version.
    apb_ok(dir.path(), &["migrate", "--apply"]);
    assert!(dir.path().join(".apb/profiles/main/profile.yaml").is_file());

    // profile list shows the migrated profile.
    let out = apb_ok(dir.path(), &["profile", "list"]);
    let s = String::from_utf8_lossy(&out.stdout);
    assert!(s.contains("\"main\""), "profile list missing main: {s}");

    // profile show returns the content.
    let out = apb_ok(dir.path(), &["profile", "show", "main"]);
    let s = String::from_utf8_lossy(&out.stdout);
    assert!(
        s.contains("profile_yaml"),
        "profile show missing content: {s}"
    );
    assert!(s.contains("claude"), "profile show missing agent: {s}");
}
