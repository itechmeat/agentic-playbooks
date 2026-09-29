//! Hardened git for everything the eval runner and its checks run in a
//! scratch tree. The tree's `.git` is the agent's to edit during the run
//! (a filesystem monitor command, a hooks path, a credential helper), and
//! the operator's shell may carry `GIT_DIR` and friends (inside a git hook,
//! `git rebase -x`, `git bisect run`). None of that may reach apb's own git
//! calls or the case scripts:
//!
//! - the repository-location variables are removed;
//! - `core.fsmonitor` is off, hooks come from an empty directory, and only
//!   the local `file` transport is allowed (the scratch `origin`);
//! - the system config is not read.

use std::path::Path;
use std::process::Command;

/// Environment variables that point git at another repository.
pub const LOCATION_VARS: [&str; 6] = [
    "GIT_DIR",
    "GIT_WORK_TREE",
    "GIT_COMMON_DIR",
    "GIT_INDEX_FILE",
    "GIT_OBJECT_DIRECTORY",
    "GIT_ALTERNATE_OBJECT_DIRECTORIES",
];

/// The `-c` settings every hardened git call carries, `hooks_dir` being an
/// empty directory.
pub fn settings(hooks_dir: &Path) -> Vec<String> {
    vec![
        "core.fsmonitor=false".into(),
        format!("core.hooksPath={}", hooks_dir.display()),
        "protocol.allow=never".into(),
        "protocol.file.allow=always".into(),
    ]
}

/// `git` with the hardening applied; the caller adds `-C`, its own `-c` and
/// the subcommand.
pub fn command(hooks_dir: &Path) -> Command {
    let mut cmd = Command::new("git");
    for v in LOCATION_VARS {
        cmd.env_remove(v);
    }
    cmd.env("GIT_CONFIG_NOSYSTEM", "1");
    for s in settings(hooks_dir) {
        cmd.arg("-c").arg(s);
    }
    cmd
}

/// The same settings as `GIT_CONFIG_PARAMETERS`, for a case script's own
/// git calls: every value single-quoted the way git reads it back.
pub fn config_parameters(hooks_dir: &Path) -> String {
    settings(hooks_dir)
        .iter()
        .map(|s| format!("'{}'", s.replace('\'', "'\\''")))
        .collect::<Vec<_>>()
        .join(" ")
}

/// Makes `dir` an empty directory (creating it, emptying it when something
/// was put there), so it can serve as `core.hooksPath`.
pub fn empty_hooks_dir(dir: &Path) -> std::io::Result<()> {
    if let Ok(m) = std::fs::symlink_metadata(dir)
        && !m.is_dir()
    {
        std::fs::remove_file(dir)?;
    }
    std::fs::create_dir_all(dir)?;
    for e in std::fs::read_dir(dir)? {
        let p = e?.path();
        match std::fs::symlink_metadata(&p) {
            Ok(m) if m.is_dir() => std::fs::remove_dir_all(&p)?,
            _ => std::fs::remove_file(&p)?,
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A hook the tree's own config names never runs under the hardened
    /// command, and the parameters git reads back from the environment keep
    /// a quote in a path intact.
    #[cfg(unix)]
    #[test]
    fn the_hardened_command_ignores_the_trees_hooks_and_fsmonitor() {
        let t = tempfile::tempdir().unwrap();
        let repo = t.path().join("r");
        let hooks = t.path().join("no'hooks");
        empty_hooks_dir(&hooks).unwrap();
        let run = |args: &[&str]| {
            let out = command(&hooks)
                .arg("-C")
                .arg(&repo)
                .args(["-c", "user.name=t", "-c", "user.email=t@t.invalid"])
                .args(args)
                .output()
                .unwrap();
            assert!(
                out.status.success(),
                "{args:?}: {}",
                String::from_utf8_lossy(&out.stderr)
            );
        };
        std::fs::create_dir_all(&repo).unwrap();
        run(&["init", "-q"]);
        let marker = t.path().join("ran");
        let evil = t.path().join("evil.sh");
        std::fs::write(&evil, format!("#!/bin/sh\ntouch '{}'\n", marker.display())).unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&evil, std::fs::Permissions::from_mode(0o755)).unwrap();
        run(&["config", "core.fsmonitor", &evil.to_string_lossy()]);
        run(&["config", "core.hooksPath", &t.path().to_string_lossy()]);
        std::fs::copy(&evil, t.path().join("pre-commit")).unwrap();
        std::fs::write(repo.join("a"), "a").unwrap();
        run(&["add", "-A"]);
        run(&["status", "--porcelain"]);
        run(&["commit", "-q", "-m", "x"]);
        assert!(!marker.exists(), "a hook or the fsmonitor ran");
        let out = Command::new("git")
            .args(["config", "--get", "core.hooksPath"])
            .env("GIT_CONFIG_PARAMETERS", config_parameters(&hooks))
            .current_dir(&repo)
            .output()
            .unwrap();
        assert_eq!(
            String::from_utf8_lossy(&out.stdout).trim(),
            hooks.to_string_lossy()
        );
    }
}
