//! The process-wide mode of an apb process: normal, or read-only (`apb mcp
//! --read-only`).
//!
//! A read-only process changes nothing and starts nothing. The mode is
//! entered once, at the very start of the process, and never left. The
//! named guard set checks it at the operation itself, not at a constructor:
//! the file writers of [`crate::fsutil`], the workspace identity, the
//! installation key, the project registry, `.gitignore` upkeep, every git
//! command ([`crate::git`]), agent detection probes, command-sourced
//! secrets, and in the engine the journal, decision requests, connector
//! calls, the process probe and the sandbox probe. Each refuses with
//! [`READ_ONLY_REFUSAL`] before it acts.
//!
//! The mode is a guard against a read-only server doing more than it
//! should, layered under the read-only tool allowlist; it is no sandbox
//! against code that bypasses these entry points.

use std::sync::atomic::{AtomicBool, Ordering};

static READ_ONLY: AtomicBool = AtomicBool::new(false);

/// The error text of every refused operation.
pub const READ_ONLY_REFUSAL: &str = "read-only server";

/// Puts this process in read-only mode, for the rest of its life. Called
/// by `apb mcp --read-only` before anything else runs.
pub fn enter_read_only() {
    READ_ONLY.store(true, Ordering::SeqCst);
}

/// Whether this process is read-only.
pub fn is_read_only() -> bool {
    READ_ONLY.load(Ordering::SeqCst)
}

/// `Err` ([`READ_ONLY_REFUSAL`], `PermissionDenied`) in a read-only
/// process: the guard an operation that writes or starts a program calls
/// before it acts.
pub fn guard() -> std::io::Result<()> {
    if is_read_only() {
        return Err(refusal());
    }
    Ok(())
}

/// The error a guarded operation returns in a read-only process.
pub fn refusal() -> std::io::Error {
    std::io::Error::new(std::io::ErrorKind::PermissionDenied, READ_ONLY_REFUSAL)
}

/// Running a test body in a read-only child process. The read-only mode is
/// process-wide and cannot be left, so a test never enters it in its own
/// process (the other tests of the binary share it): the test re-runs its
/// own binary for exactly itself, and only that child enters the mode.
#[cfg(any(test, feature = "testing"))]
pub mod testing {
    /// The variable naming the one test the child runs read-only.
    const CHILD_ENV: &str = "APB_TEST_READ_ONLY_CHILD";

    /// In the test process: re-runs the current test binary for the test
    /// `path` (its libtest name, `module::tests::name`) and asserts that
    /// the child passed. In that child: enters read-only mode and runs
    /// `body`.
    pub fn in_read_only_child(path: &str, body: impl FnOnce()) {
        in_read_only_child_after(path, || (), |()| body());
    }

    /// [`in_read_only_child`] whose child runs `setup` before it enters
    /// read-only mode and hands its value to `body`: for a handle opened
    /// while writing was still allowed.
    pub fn in_read_only_child_after<T>(
        path: &str,
        setup: impl FnOnce() -> T,
        body: impl FnOnce(T),
    ) {
        if std::env::var(CHILD_ENV).as_deref() == Ok(path) {
            let value = setup();
            super::enter_read_only();
            body(value);
            return;
        }
        let exe = std::env::current_exe().expect("the test binary");
        let out = std::process::Command::new(exe)
            // `--include-ignored`: an `#[ignore]` test the parent runs under
            // `--ignored` runs in the child too.
            .args([
                path,
                "--exact",
                "--include-ignored",
                "--nocapture",
                "--test-threads=1",
            ])
            .env(CHILD_ENV, path)
            .output()
            .expect("the read-only child runs");
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(
            out.status.success(),
            "the read-only child of `{path}` failed:\n{stdout}\n{}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(
            stdout.contains("1 passed"),
            "the read-only child ran no test named `{path}`:\n{stdout}"
        );
    }

    /// A `#[test]` whose body runs in a read-only child process (see
    /// [`in_read_only_child`]). `#[should_panic]` is not supported: the
    /// panic happens in the child, and the parent sees a failed child. With `before x = { .. } then`, the child
    /// binds `x` to the setup's value before it enters read-only mode
    /// ([`in_read_only_child_after`]).
    #[macro_export]
    macro_rules! read_only_test {
        ($(#[$attr:meta])* fn $name:ident() before $v:ident = $setup:block then $body:block) => {
            $(#[$attr])*
            #[test]
            fn $name() {
                let full = concat!(module_path!(), "::", stringify!($name));
                // libtest names a test without its crate.
                let path = full.split_once("::").map_or(full, |(_, rest)| rest);
                $crate::process_mode::testing::in_read_only_child_after(
                    path,
                    || $setup,
                    |$v| $body,
                );
            }
        };
        ($(#[$attr:meta])* fn $name:ident() $body:block) => {
            $crate::read_only_test! { $(#[$attr])* fn $name() before _unit = {} then $body }
        };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_normal_process_is_not_read_only() {
        assert!(!is_read_only());
        assert!(guard().is_ok());
    }

    crate::read_only_test! {
        fn the_guard_refuses_in_a_read_only_process() {
            assert!(is_read_only());
            let e = guard().unwrap_err();
            assert_eq!(e.kind(), std::io::ErrorKind::PermissionDenied);
            assert_eq!(e.to_string(), READ_ONLY_REFUSAL);
        }
    }
}

/// Every guarded operation of this crate, called directly in a read-only
/// child process: each refuses and leaves the disk as it was. A test of the
/// read-only router cannot show this, since a removed tool never reaches
/// the operation.
#[cfg(test)]
mod guard_tests {
    use std::path::Path;

    fn refused(e: std::io::Error) {
        assert_eq!(e.to_string(), super::READ_ONLY_REFUSAL);
    }

    /// Points the config directory at `dir` (this child process only).
    fn config_at(dir: &Path) {
        // SAFETY: a read-only child runs this one test on one thread.
        unsafe { std::env::set_var("APB_CONFIG_DIR", dir) };
    }

    /// A stub program `name` in `dir` that leaves `<dir>/<name>.ran` when
    /// it runs.
    #[cfg(unix)]
    fn stub(dir: &Path, name: &str) -> std::path::PathBuf {
        use std::os::unix::fs::PermissionsExt as _;
        let path = dir.join(name);
        let marker = dir.join(format!("{name}.ran"));
        std::fs::write(
            &path,
            format!("#!/bin/sh\ntouch '{}'\necho value\n", marker.display()),
        )
        .unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        path
    }

    crate::read_only_test! {
        fn every_file_writer_refuses() {
            use crate::fsutil;
            let dir = tempfile::tempdir().unwrap();
            let d = dir.path();
            let f = d.join("sub/f");
            refused(fsutil::atomic_write(&f, b"x").unwrap_err());
            refused(fsutil::atomic_write_private(&f, b"x").unwrap_err());
            refused(fsutil::atomic_write_under(d, &f, b"x").unwrap_err());
            refused(fsutil::atomic_write_private_under(d, &f, b"x").unwrap_err());
            refused(fsutil::create_dir_under(d, &d.join("sub")).unwrap_err());
            refused(fsutil::create_private_dir(&d.join("sub")).unwrap_err());
            refused(fsutil::open_append_private(&d.join("log")).unwrap_err());
            refused(fsutil::lock_dir(d, "x.lock").err().unwrap());
            refused(
                fsutil::lock_dir_with_lifetime(d, "x.lock", std::time::Duration::from_secs(1))
                    .err()
                    .unwrap(),
            );
            refused(fsutil::copy_tree(d, &d.join("copy")).unwrap_err());
            refused(fsutil::symlink(d, &d.join("link")).unwrap_err());
            let mut cmd = std::process::Command::new("true");
            refused(fsutil::spawn_when_not_busy(&mut cmd).err().unwrap());
            assert_eq!(std::fs::read_dir(d).unwrap().count(), 0, "nothing was created");
        }
    }

    crate::read_only_test! {
        fn the_workspace_identity_and_its_gitignore_are_left_alone() {
            let dir = tempfile::tempdir().unwrap();
            let root = dir.path();
            std::fs::create_dir(root.join(".apb")).unwrap();
            refused(crate::workspace::ensure_id(root).unwrap_err());
            crate::workspace::ensure_local_ignored(root);
            assert_eq!(std::fs::read_dir(root.join(".apb")).unwrap().count(), 0);
        }
    }

    crate::read_only_test! {
        fn the_installation_key_and_the_registry_are_not_written() {
            let cfg = tempfile::tempdir().unwrap();
            config_at(cfg.path());
            let project = tempfile::tempdir().unwrap();
            std::fs::create_dir(project.path().join(".apb")).unwrap();
            refused(crate::run_origin::ensure_key().unwrap_err());
            crate::projects::touch(project.path());
            assert_eq!(std::fs::read_dir(cfg.path()).unwrap().count(), 0);
            assert!(!project.path().join(".apb/workspace.local").exists());
            // A removal that would change the registry is refused, and the
            // file stays as it was.
            let registry = cfg.path().join("projects.json");
            let seeded = r#"{"schema_version":2,"entries":{"ws-a":{"workspace_id":"ws-a","path":"/nonexistent/a","name":"a","last_seen_ms":1,"state":{"kind":"active"}}}}"#;
            std::fs::write(&registry, seeded).unwrap();
            assert!(crate::projects::remove("ws-a").is_err());
            assert_eq!(std::fs::read_to_string(&registry).unwrap(), seeded);
        }
    }

    crate::read_only_test! {
        fn no_git_command_runs() {
            let here = std::env::current_dir().unwrap();
            assert_eq!(
                crate::git::Repo::locate(&here).unwrap_err(),
                crate::git::RepoError::ReadOnly
            );
            assert_eq!(crate::git::read(&here, &["status"]), None);
            let spawned = crate::git::base().arg("--version").output();
            assert!(spawned.is_err(), "the read-only git command must not start");
        }
    }

    #[cfg(unix)]
    crate::read_only_test! {
        fn detection_and_a_secret_helper_start_no_program() {
            let bin = tempfile::tempdir().unwrap();
            let cfg = tempfile::tempdir().unwrap();
            config_at(cfg.path());
            let helper = stub(bin.path(), "helper");
            let agent = stub(bin.path(), "claude");
            // SAFETY: a read-only child runs this one test on one thread.
            unsafe { std::env::set_var("PATH", bin.path()) };
            let e = crate::connector::secrets::resolve_cmd(
                &helper.to_string_lossy(),
                std::time::Duration::from_secs(5),
            )
            .unwrap_err();
            assert!(
                matches!(&e, crate::connector::secrets::CmdSecretError::Spawn(m) if m == super::READ_ONLY_REFUSAL),
                "got {e:?}"
            );
            assert!(crate::detect::probe_output(&agent, &["--version".into()]).is_err());
            let agents = crate::detect::probe(true);
            assert!(!agents.is_empty());
            assert!(agents.iter().all(|a| !a.installed
                && a.notes == [crate::detect::NOT_PROBED_READ_ONLY]));
            assert!(!bin.path().join("helper.ran").exists(), "the helper ran");
            assert!(!bin.path().join("claude.ran").exists(), "the agent ran");
            assert_eq!(std::fs::read_dir(cfg.path()).unwrap().count(), 0, "a memo was written");
        }
    }
}
