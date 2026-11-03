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
//!
//! A scenario process (`apb __scenario-run`) is the third mode: it drives a
//! run whose every outside answer is scripted, so it starts no program,
//! calls no connector, asks a decision provider only at
//! [`ScenarioLevel::Smoke`], and writes only below its write root. The
//! guards of that mode are [`guard_program_start`],
//! [`guard_connector_call`], [`guard_decision_request`] and
//! [`guard_write`]; each refuses in a read-only process too.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, Ordering};

pub use crate::scenario::ScenarioLevel;

static READ_ONLY: AtomicBool = AtomicBool::new(false);

static SCENARIO: OnceLock<ScenarioMode> = OnceLock::new();

/// The prefix of the error text of every operation a scenario process
/// refuses; the operation's name follows it.
pub const SCENARIO_REFUSAL: &str = "scenario_guard";

/// The mode of a scenario process.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScenarioMode {
    pub level: ScenarioLevel,
    /// The one directory the process may write below: the scenario run
    /// directories and the results index. Absolute.
    pub write_root: PathBuf,
}

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

/// Puts this process in scenario mode, for the rest of its life. `apb
/// __scenario-run` enters through the engine's `scenario::guard::enter`,
/// which calls this first. Refused in a read-only
/// process, for a relative write root, and when the process is already in
/// scenario mode.
pub fn enter_scenario(level: ScenarioLevel, write_root: &Path) -> std::io::Result<()> {
    guard()?;
    if !write_root.is_absolute() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!(
                "the scenario write root {} is not absolute",
                write_root.display()
            ),
        ));
    }
    SCENARIO
        .set(ScenarioMode {
            level,
            write_root: write_root.to_path_buf(),
        })
        .map_err(|_| {
            std::io::Error::new(
                std::io::ErrorKind::AlreadyExists,
                "this process is already a scenario process",
            )
        })
}

/// The scenario mode of this process, if it is a scenario process.
pub fn scenario() -> Option<&'static ScenarioMode> {
    SCENARIO.get()
}

/// The operations this scenario process refused, each once, in the order
/// first refused, at most [`MAX_RECORDED_REFUSALS`]. A caller may ignore a
/// refused write (a memo, a stamp); the record keeps it visible, so a run
/// can tell that some site reached a guard instead of its boundary. A site
/// refused over and over (every git command a caller builds) takes one
/// entry, never the room another operation needs.
static REFUSALS: std::sync::Mutex<Vec<String>> = std::sync::Mutex::new(Vec::new());

/// The most refusals [`scenario_refusals`] keeps.
pub const MAX_RECORDED_REFUSALS: usize = 64;

/// The operations this scenario process refused so far, oldest first.
pub fn scenario_refusals() -> Vec<String> {
    REFUSALS.lock().map(|r| r.clone()).unwrap_or_default()
}

/// The error a scenario process returns for `operation`, recorded in
/// [`scenario_refusals`].
pub fn scenario_refusal(operation: &str) -> std::io::Error {
    if let Ok(mut r) = REFUSALS.lock()
        && r.len() < MAX_RECORDED_REFUSALS
        && !r.iter().any(|o| o == operation)
    {
        r.push(operation.to_string());
    }
    std::io::Error::new(
        std::io::ErrorKind::PermissionDenied,
        format!("{SCENARIO_REFUSAL}: {operation}"),
    )
}

/// The guard before any program starts (an agent, a script, git, a
/// helper, a waker): refused in a read-only and in a scenario process.
pub fn guard_program_start() -> std::io::Result<()> {
    guard()?;
    match scenario() {
        Some(_) => Err(scenario_refusal("program start")),
        None => Ok(()),
    }
}

/// The guard before a connector call: refused in a read-only and in a
/// scenario process.
pub fn guard_connector_call() -> std::io::Result<()> {
    guard()?;
    match scenario() {
        Some(_) => Err(scenario_refusal("connector call")),
        None => Ok(()),
    }
}

/// The guard before a request to a decision provider: refused in a
/// read-only process and in a quick scenario process; a smoke process asks
/// its configured providers. A run's decision runner asks it before any
/// provider.
pub fn guard_decision_request() -> std::io::Result<()> {
    guard()?;
    match scenario() {
        Some(m) if m.level == ScenarioLevel::Quick => Err(scenario_refusal("decision request")),
        _ => Ok(()),
    }
}

/// The guard before a write at `path`: refused in a read-only process, and
/// in a scenario process unless `path` is a plain path below the write
/// root with no symbolic link under it. A relative `path` is taken from
/// the current directory.
pub fn guard_write(path: &Path) -> std::io::Result<()> {
    guard()?;
    let Some(m) = scenario() else {
        return Ok(());
    };
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()?.join(path)
    };
    let refused = || scenario_refusal(&format!("write to {}", path.display()));
    let below = absolute
        .strip_prefix(&m.write_root)
        .map_err(|_| refused())?;
    // Every component below the root is a plain name and no existing one
    // is a link (a write through it would land outside the root). This
    // module sits under `fsutil`, so the walk is its own.
    let mut cur = m.write_root.clone();
    for c in below.components() {
        let std::path::Component::Normal(name) = c else {
            return Err(refused());
        };
        cur.push(name);
        match std::fs::symlink_metadata(&cur) {
            Ok(meta) if meta.file_type().is_symlink() => return Err(refused()),
            Ok(_) => {}
            // Nothing below a missing component exists yet; the remaining
            // names are still checked to be plain.
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(_) => return Err(refused()),
        }
    }
    Ok(())
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
        run_child(path, CHILD_ENV, "read-only");
    }

    /// The variable naming the one test the child runs as a scenario
    /// process.
    const SCENARIO_CHILD_ENV: &str = "APB_TEST_SCENARIO_CHILD";

    /// [`in_read_only_child`] for scenario mode: the child enters scenario
    /// mode at `level` with a fresh temporary directory as its write root
    /// and hands that root to `body`.
    pub fn in_scenario_child(
        path: &str,
        level: super::ScenarioLevel,
        body: impl FnOnce(&std::path::Path),
    ) {
        if std::env::var(SCENARIO_CHILD_ENV).as_deref() == Ok(path) {
            // A fresh directory of this child process (the feature has no
            // `tempfile`), removed after the body, also when it panics.
            struct Removed(std::path::PathBuf);
            impl Drop for Removed {
                fn drop(&mut self) {
                    let _ = std::fs::remove_dir_all(&self.0);
                }
            }
            let root = std::env::temp_dir().join(format!(
                "apb-scenario-child-{}-{}",
                std::process::id(),
                crate::clock::now_ms()
            ));
            std::fs::create_dir_all(&root).expect("a write root");
            let root = Removed(root.canonicalize().expect("the canonical root"));
            super::enter_scenario(level, &root.0).expect("enters scenario mode");
            body(&root.0);
            return;
        }
        run_child(path, SCENARIO_CHILD_ENV, "scenario");
    }

    /// The variable naming the one test the child runs in no mode.
    const PLAIN_CHILD_ENV: &str = "APB_TEST_PLAIN_CHILD";

    /// [`in_read_only_child`] whose child enters no mode itself: for a
    /// test of an entry point that enters a mode (the engine's scenario
    /// entry), which a process can do only once.
    pub fn in_plain_child(path: &str, body: impl FnOnce()) {
        if std::env::var(PLAIN_CHILD_ENV).as_deref() == Ok(path) {
            body();
            return;
        }
        run_child(path, PLAIN_CHILD_ENV, "plain");
    }

    /// Re-runs the current test binary for exactly the test `path` with
    /// `env` naming it, and asserts that the child passed.
    fn run_child(path: &str, env: &str, what: &str) {
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
            .env(env, path)
            .output()
            .expect("the child test process runs");
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(
            out.status.success(),
            "the {what} child of `{path}` failed:\n{stdout}\n{}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(
            stdout.contains("1 passed"),
            "the {what} child ran no test named `{path}`:\n{stdout}"
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

    /// A `#[test]` whose body runs in a scenario child process at `level`
    /// (see [`in_scenario_child`]), with `root` bound to its write root.
    #[macro_export]
    macro_rules! scenario_test {
        ($(#[$attr:meta])* fn $name:ident() at $level:ident, $root:ident => $body:block) => {
            $(#[$attr])*
            #[test]
            fn $name() {
                let full = concat!(module_path!(), "::", stringify!($name));
                let path = full.split_once("::").map_or(full, |(_, rest)| rest);
                $crate::process_mode::testing::in_scenario_child(
                    path,
                    $crate::process_mode::ScenarioLevel::$level,
                    |$root: &::std::path::Path| $body,
                );
            }
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
            // The scenario guards are a superset: a read-only process
            // refuses them as read-only, and cannot become a scenario one.
            for e in [
                guard_program_start(),
                guard_connector_call(),
                guard_decision_request(),
                guard_write(Path::new("/tmp/x")),
                enter_scenario(ScenarioLevel::Quick, Path::new("/tmp")),
            ] {
                assert_eq!(e.unwrap_err().to_string(), READ_ONLY_REFUSAL);
            }
        }
    }

    #[test]
    fn a_normal_process_passes_every_scenario_guard() {
        assert!(scenario().is_none());
        assert!(guard_program_start().is_ok());
        assert!(guard_connector_call().is_ok());
        assert!(guard_decision_request().is_ok());
        assert!(guard_write(Path::new("/anywhere/at/all")).is_ok());
    }

    fn scenario_refused(r: std::io::Result<()>, operation: &str) {
        let e = r.unwrap_err();
        assert_eq!(e.kind(), std::io::ErrorKind::PermissionDenied);
        assert_eq!(e.to_string(), format!("{SCENARIO_REFUSAL}: {operation}"));
    }

    crate::scenario_test! {
        fn a_quick_process_starts_nothing_asks_nothing_and_writes_only_below_its_root() at Quick, root => {
            assert!(!is_read_only() && guard().is_ok());
            scenario_refused(guard_program_start(), "program start");
            scenario_refused(guard_connector_call(), "connector call");
            scenario_refused(guard_decision_request(), "decision request");
            assert!(guard_write(root).is_ok());
            assert!(guard_write(&root.join("run-1/events.jsonl")).is_ok());
            assert!(guard_write(&root.join("not-yet/created/deep")).is_ok());
            let outside = root.parent().unwrap().join("events.jsonl");
            assert!(guard_write(&outside).is_err());
            assert!(guard_write(&root.join("run-1/../../escape")).is_err());
            // A link below the root to a place outside it is no way out.
            #[cfg(unix)]
            {
                std::os::unix::fs::symlink(root.parent().unwrap(), root.join("link")).unwrap();
                assert!(guard_write(&root.join("link/x")).is_err());
            }
            assert!(enter_scenario(ScenarioLevel::Smoke, root).is_err(), "entered twice");
            assert_eq!(scenario().unwrap().level, ScenarioLevel::Quick);
        }
    }

    // Every refusal is kept in order, also one whose caller ignores the
    // error; an allowed write below the root is not a refusal.
    crate::scenario_test! {
        fn a_scenario_process_keeps_what_it_refused() at Quick, root => {
            assert!(scenario_refusals().is_empty());
            assert!(guard_write(&root.join("run/events.jsonl")).is_ok());
            let _ = guard_program_start();
            let _ = guard_write(Path::new("/elsewhere/memo.json"));
            assert_eq!(
                scenario_refusals(),
                ["program start", "write to /elsewhere/memo.json"]
            );
        }
    }

    // A refusal repeated without end (every git command a caller builds)
    // takes one entry, so it never crowds out another operation.
    crate::scenario_test! {
        fn a_repeated_refusal_never_crowds_out_another() at Quick, _root => {
            for _ in 0..(MAX_RECORDED_REFUSALS * 2) {
                let _ = guard_program_start();
            }
            let _ = guard_write(Path::new("/elsewhere/memo.json"));
            assert_eq!(
                scenario_refusals(),
                ["program start", "write to /elsewhere/memo.json"]
            );
        }
    }

    crate::scenario_test! {
        fn a_smoke_process_asks_its_providers_and_still_starts_nothing() at Smoke, _root => {
            assert!(guard_decision_request().is_ok());
            scenario_refused(guard_program_start(), "program start");
            scenario_refused(guard_connector_call(), "connector call");
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
