//! The standing block: written once, taken back only where its bytes are
//! unchanged.

use super::standing;

#[test]
fn standing_block_removal_takes_back_only_what_was_written() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("CLAUDE.md"), "# Mine\n").unwrap();
    let written = standing::write(dir.path()).unwrap();
    assert_eq!(
        standing::block_state(dir.path()),
        standing::BlockState::Present
    );
    assert!(standing::remove(&written).is_empty());
    assert_eq!(
        std::fs::read_to_string(dir.path().join("CLAUDE.md")).unwrap(),
        "# Mine\n"
    );
    assert!(
        !dir.path().join("AGENTS.md").exists(),
        "a created file goes"
    );

    // An edited block stays.
    let written = standing::write(dir.path()).unwrap();
    let path = dir.path().join("AGENTS.md");
    let text = std::fs::read_to_string(&path).unwrap();
    std::fs::write(&path, text.replace("Two duties", "Three duties")).unwrap();
    let kept = standing::remove(&written);
    assert_eq!(kept.len(), 1, "{kept:?}");
    assert!(
        std::fs::read_to_string(&path)
            .unwrap()
            .contains("Three duties")
    );
}

#[test]
fn setup_howto_is_served_read_only_and_setup_plan_is_not() {
    assert!(crate::server::read_only::serves("setup_howto"));
    assert!(!crate::server::read_only::serves("setup_plan"));
}

#[test]
fn defaults_are_reconciled_only_once_setup_was_applied() {
    use super::record::{self, Applied, Record};
    use super::upgrade::reconcile_if_set_up_in;
    use apb_core::bundled::ReconcileNote;

    let note = || {
        vec![ReconcileNote {
            failed: false,
            text: "ran".into(),
        }]
    };
    let dir = tempfile::tempdir().unwrap();
    let setup = dir.path().join("setup");

    // No record: nothing runs.
    assert!(reconcile_if_set_up_in(&setup, || panic!("ran without setup")).is_empty());

    // A record with no applied plan (setup only removed): nothing runs.
    std::fs::create_dir_all(&setup).unwrap();
    record::save(&setup, &Record::default()).unwrap();
    assert!(reconcile_if_set_up_in(&setup, || panic!("ran without a plan")).is_empty());

    // An applied plan: the reconcile runs and its notes come back.
    let applied = Record {
        applied: vec![Applied {
            digest: "d".into(),
            at_ms: 1,
        }],
        entries: Vec::new(),
    };
    record::save(&setup, &applied).unwrap();
    assert_eq!(reconcile_if_set_up_in(&setup, note), note());

    // An unreadable record is a note, never a failure of the caller.
    std::fs::write(record::record_path(&setup), "not json").unwrap();
    let out = reconcile_if_set_up_in(&setup, || panic!("ran on a broken record"));
    assert_eq!(out.len(), 1);
    assert!(out[0].failed, "{out:?}");
}

#[cfg(unix)]
#[test]
fn a_linked_instruction_file_gets_one_block_and_keeps_its_link() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(dir.path()).unwrap();
    let agents = root.join("AGENTS.md");
    std::fs::write(&agents, "# Rules\n").unwrap();
    std::fs::set_permissions(&agents, std::fs::Permissions::from_mode(0o640)).unwrap();
    std::os::unix::fs::symlink("AGENTS.md", root.join("CLAUDE.md")).unwrap();

    let written = standing::write(&root).unwrap();
    let text = std::fs::read_to_string(&agents).unwrap();
    assert_eq!(text.matches(standing::PLAYBOOK_MARKER).count(), 1, "{text}");
    let link = std::fs::symlink_metadata(root.join("CLAUDE.md")).unwrap();
    assert!(link.file_type().is_symlink(), "the link stays a link");
    assert_eq!(
        std::fs::metadata(&agents).unwrap().permissions().mode() & 0o777,
        0o640
    );

    assert!(standing::remove(&written).is_empty());
    assert_eq!(std::fs::read_to_string(&agents).unwrap(), "# Rules\n");
    let link = std::fs::symlink_metadata(root.join("CLAUDE.md")).unwrap();
    assert!(
        link.file_type().is_symlink(),
        "removal never replaces the link"
    );
    assert_eq!(
        std::fs::metadata(&agents).unwrap().permissions().mode() & 0o777,
        0o640
    );
}

#[cfg(unix)]
#[test]
fn a_link_out_of_the_project_is_refused_and_nothing_is_written() {
    let dir = tempfile::tempdir().unwrap();
    let project = dir.path().join("project");
    std::fs::create_dir_all(&project).unwrap();
    let outside = dir.path().join("dotfile");
    std::fs::write(&outside, "export TOKEN=x\n").unwrap();
    std::os::unix::fs::symlink(&outside, project.join("CLAUDE.md")).unwrap();

    assert!(standing::link_problem(&project).is_some());
    let err = standing::write(&project).unwrap_err();
    assert!(err.to_string().contains("outside the project"), "{err}");
    assert_eq!(
        std::fs::read_to_string(&outside).unwrap(),
        "export TOKEN=x\n"
    );
    assert!(
        !project.join("AGENTS.md").exists(),
        "refused before any write"
    );

    // A record that names the link (written before the link was changed)
    // never pulls the outside file into the project.
    let record = serde_json::json!({
        "dir": project,
        "files": [{ "path": project.join("CLAUDE.md"), "created_file": false, "inserted": "x" }],
    });
    let kept = standing::remove(&record);
    assert_eq!(kept.len(), 1, "{kept:?}");
    let link = std::fs::symlink_metadata(project.join("CLAUDE.md")).unwrap();
    assert!(link.file_type().is_symlink());
    assert_eq!(
        std::fs::read_to_string(&outside).unwrap(),
        "export TOKEN=x\n"
    );
}

#[test]
fn the_digest_covers_the_host_cli_apply_starts() {
    use super::hosts::HostState;
    use super::plan::{Action, ActionKind, digest};
    let host = |cli: &str| HostState {
        host: "claude".into(),
        detected: true,
        cli: Some(cli.into()),
        config: None,
        method: "cli",
        registration: apb_core::agents::McpRegistration::Absent,
        note: None,
        manual: None,
        config_sha256: None,
    };
    let action = |h: &HostState| Action {
        kind: ActionKind::RegisterMcp,
        target: h.host.clone(),
        detail: "register".into(),
        reversible: true,
        undo: "apb setup remove".into(),
        params: h.precondition(),
    };
    let a = digest(&[action(&host("/usr/bin/claude"))], None);
    let b = digest(&[action(&host("/tmp/elsewhere/claude"))], None);
    assert_ne!(a, b, "another binary is another plan");
    assert_eq!(a, digest(&[action(&host("/usr/bin/claude"))], None));
}

/// A target that does not read stops the write before any file changes,
/// so no block is left behind that the setup record does not hold.
#[test]
fn a_standing_block_target_that_does_not_read_changes_nothing() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("CLAUDE.md"), "# Mine\n").unwrap();
    std::fs::write(dir.path().join("AGENTS.md"), [0xff, 0xfe, 0x00]).unwrap();
    assert!(standing::write(dir.path()).is_err());
    assert_eq!(
        std::fs::read_to_string(dir.path().join("CLAUDE.md")).unwrap(),
        "# Mine\n"
    );
    assert_eq!(
        std::fs::read(dir.path().join("AGENTS.md")).unwrap(),
        [0xff, 0xfe, 0x00]
    );
}

/// A host config another program changed after setup read it is never
/// overwritten with the older copy.
#[test]
fn a_host_config_changed_meanwhile_is_not_overwritten() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.toml");
    std::fs::write(&path, "a = 1\n").unwrap();
    let read_sha = apb_core::content::sha256_hex(b"a = 1\n");
    std::fs::write(&path, "a = 1\nb = 2\n").unwrap();
    let err = super::hosts::publish(&path, Some(&read_sha), "a = 1\nc = 3\n").unwrap_err();
    assert!(err.contains("changed while setup"), "{err}");
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "a = 1\nb = 2\n");
    // A file that appeared where none was read is not overwritten either.
    assert!(super::hosts::publish(&path, None, "x").is_err());
    let now = apb_core::content::sha256_hex(b"a = 1\nb = 2\n");
    super::hosts::publish(&path, Some(&now), "a = 1\nb = 2\nc = 3\n").unwrap();
    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        "a = 1\nb = 2\nc = 3\n"
    );
}

/// A host CLI that stalls is stopped, so setup never holds its lock past
/// what the lock's lifetime allows.
#[test]
fn a_stalled_host_cli_is_stopped_within_its_limit() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    let cli = dir.path().join("host");
    std::fs::write(&cli, "#!/bin/sh\nsleep 30\n").unwrap();
    std::fs::set_permissions(&cli, std::fs::Permissions::from_mode(0o755)).unwrap();
    let started = std::time::Instant::now();
    let err =
        super::hosts::run_bounded(&cli, &[], std::time::Duration::from_millis(300)).unwrap_err();
    assert!(err.contains("was stopped"), "{err}");
    assert!(started.elapsed() < std::time::Duration::from_secs(10));
}

/// Writes `body` as an executable shell script at `path`.
#[cfg(unix)]
fn script(path: &std::path::Path, body: &str) {
    use std::os::unix::fs::PermissionsExt;
    std::fs::write(path, format!("#!/bin/sh\nset -e\n{body}")).unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
}

/// A host CLI that writes more error output than is kept, pauses and writes
/// again still succeeds: the reader drains the pipe to the end. A reader
/// that closed the pipe once it had kept enough would kill the second write
/// with SIGPIPE, and `set -e` makes that a failed command.
#[cfg(unix)]
#[test]
fn a_chatty_host_cli_is_not_cut_off_by_a_closed_error_pipe() {
    let dir = tempfile::tempdir().unwrap();
    let cli = dir.path().join("host");
    let done = dir.path().join("done");
    script(
        &cli,
        "head -c 70000 /dev/zero | tr '\\0' x >&2\nsleep 0.3\nhead -c 70000 /dev/zero | tr '\\0' y >&2\ntouch \"$1\"\n",
    );
    let result = super::hosts::run_bounded(
        &cli,
        &[done.to_string_lossy().to_string()],
        std::time::Duration::from_secs(10),
    );
    assert_eq!(result, Ok(()));
    assert!(done.exists(), "the CLI was cut off before it finished");
}

/// A failing host CLI that writes far more error output than is kept is
/// reported with the start of what it wrote, not its end.
#[cfg(unix)]
#[test]
fn a_failing_host_cli_is_reported_with_the_start_of_its_error_output() {
    let dir = tempfile::tempdir().unwrap();
    let cli = dir.path().join("host");
    script(
        &cli,
        "echo first-line-of-the-error >&2\nhead -c 200000 /dev/zero | tr '\\0' z >&2\necho last-line >&2\nexit 1\n",
    );
    let err = super::hosts::run_bounded(&cli, &[], std::time::Duration::from_secs(10)).unwrap_err();
    assert!(err.contains(": first-line-of-the-error"), "{err}");
    assert!(!err.contains("last-line"), "{err}");
    assert!(
        err.len() < 1000,
        "the message is bounded: {} bytes",
        err.len()
    );
}

/// A launcher that leaves a process in the background holding its error
/// pipe and then fails returns within the limit, with its error output,
/// and what it left behind is killed with its process group.
#[cfg(target_os = "linux")]
#[test]
fn a_failing_host_cli_with_a_background_child_returns_within_its_limit() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    let cli = dir.path().join("host");
    let pidfile = dir.path().join("sleeper.pid");
    std::fs::write(
        &cli,
        "#!/bin/sh\nsleep 1900 &\necho $! > \"$1\"\necho boom >&2\nexit 1\n",
    )
    .unwrap();
    std::fs::set_permissions(&cli, std::fs::Permissions::from_mode(0o755)).unwrap();
    let started = std::time::Instant::now();
    let err = super::hosts::run_bounded(
        &cli,
        &[pidfile.to_string_lossy().to_string()],
        std::time::Duration::from_secs(5),
    )
    .unwrap_err();
    assert!(
        started.elapsed() < std::time::Duration::from_secs(5),
        "{err}"
    );
    assert!(err.contains("exited with") && err.contains("boom"), "{err}");
    let pid = std::fs::read_to_string(&pidfile)
        .unwrap()
        .trim()
        .to_string();
    let gone = || match std::fs::read_to_string(format!("/proc/{pid}/stat")) {
        Err(_) => true,
        Ok(stat) => stat
            .rsplit(')')
            .next()
            .is_some_and(|r| r.trim_start().starts_with('Z')),
    };
    let until = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while !gone() && std::time::Instant::now() < until {
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    assert!(gone(), "the background sleeper {pid} still runs");
}

/// The setup lock is held by a live owner however old its file is, and is
/// free at once when no process holds it, whatever its file says.
#[test]
fn the_setup_lock_is_never_taken_from_a_live_owner_and_free_after_a_dead_one() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join(super::record::LOCK);
    let holder = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(&path)
        .unwrap();
    holder.lock().unwrap();
    let old = std::time::SystemTime::now() - std::time::Duration::from_secs(24 * 3600);
    holder.set_modified(old).unwrap();
    let err = super::record::lock(dir.path())
        .err()
        .expect("a live owner keeps it");
    assert!(err.contains("setup is busy"), "{err}");

    // The owner died: its file stays, with any content and a fresh age.
    drop(holder);
    std::fs::write(&path, "12345-somebody").unwrap();
    let started = std::time::Instant::now();
    let held = super::record::lock(dir.path()).expect("a dead owner's lock is free");
    assert!(started.elapsed() < std::time::Duration::from_secs(1));
    assert!(
        super::record::lock(dir.path()).is_err(),
        "held by the guard"
    );
    drop(held);
    super::record::lock(dir.path()).expect("free again after the guard");
}

/// A save between setup's read and its rename is never overwritten: the
/// file is read again right before the rename.
#[test]
fn a_host_config_saved_between_check_and_rename_is_not_overwritten() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.toml");
    std::fs::write(&path, "a = 1\n").unwrap();
    let read_sha = apb_core::content::sha256_hex(b"a = 1\n");
    let p = path.clone();
    super::hosts::publish_hook::set(move || std::fs::write(&p, "a = 1\nb = 2\n").unwrap());
    let err = super::hosts::publish(&path, Some(&read_sha), "a = 1\nc = 3\n").unwrap_err();
    assert!(err.contains("changed while setup"), "{err}");
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "a = 1\nb = 2\n");
    let names: Vec<_> = std::fs::read_dir(dir.path()).unwrap().collect();
    assert_eq!(names.len(), 1, "no temp is left behind");
}

/// A symlinked config moved to another file between setup's read and its
/// rename is refused, and neither file is written.
#[test]
fn a_host_config_link_moved_between_check_and_rename_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let first = dir.path().join("first.toml");
    let second = dir.path().join("second.toml");
    let link = dir.path().join("config.toml");
    // The same bytes in both, so only the link check can catch the move.
    std::fs::write(&first, "a = 1\n").unwrap();
    std::fs::write(&second, "a = 1\n").unwrap();
    std::os::unix::fs::symlink(&first, &link).unwrap();
    let read_sha = apb_core::content::sha256_hex(b"a = 1\n");
    let (l, s) = (link.clone(), second.clone());
    super::hosts::publish_hook::set(move || {
        std::fs::remove_file(&l).unwrap();
        std::os::unix::fs::symlink(&s, &l).unwrap();
    });
    let err = super::hosts::publish(&link, Some(&read_sha), "a = 1\nc = 3\n").unwrap_err();
    assert!(err.contains("changed while setup"), "{err}");
    assert_eq!(std::fs::read_to_string(&first).unwrap(), "a = 1\n");
    assert_eq!(std::fs::read_to_string(&second).unwrap(), "a = 1\n");
    assert_eq!(std::fs::read_link(&link).unwrap(), second);
    // Without a change in between, the link's target is edited in place.
    super::hosts::publish(&link, Some(&read_sha), "a = 1\nc = 3\n").unwrap();
    assert_eq!(std::fs::read_to_string(&second).unwrap(), "a = 1\nc = 3\n");
    assert!(
        std::fs::symlink_metadata(&link)
            .unwrap()
            .file_type()
            .is_symlink()
    );
}
