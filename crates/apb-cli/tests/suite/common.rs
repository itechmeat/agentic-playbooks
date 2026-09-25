//! Shared test-only utilities for the apb-cli integration binaries (the
//! consolidated `../main.rs` suite and `../init_interactive_test.rs`).
//!
//! Every spawn of the `apb` binary goes through [`apb`] or [`apb_std`], which
//! point `APB_CONFIG_DIR` at a sandbox. Without it the child resolves the
//! global config dir from `XDG_CONFIG_HOME`/`HOME` - the developer's REAL
//! `~/.config/apb` - and every command run inside a seeded tempdir
//! auto-registers that tempdir in the real `projects.json` (plus writes
//! `state/agents-detect.json` there). CI never noticed: auto-registration is
//! off whenever `CI` is set. A test that needs its own config dir still sets
//! `APB_CONFIG_DIR` itself; the later `.env()` wins.
//!
//! The sandbox is one directory per test process, not per command: several
//! tests run a sequence of commands that share global state, exactly as they
//! used to share the real config dir. `real_config_guard_test.rs` fails the suite
//! if a test spawns the binary any other way.

#![allow(dead_code)] // each test binary uses a different subset

use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::{Duration, SystemTime};

/// Path of the `apb` binary under test. Only for handing to code that spawns
/// it itself (e.g. a script run by an apb child, which inherits that child's
/// sandboxed env); tests spawn it through [`apb`] / [`apb_std`].
pub fn apb_bin() -> &'static Path {
    Path::new(env!("CARGO_BIN_EXE_apb"))
}

/// Prefix of the per-process sandbox directories under `CARGO_TARGET_TMPDIR`.
const SANDBOX_PREFIX: &str = "apb-cli-test-config-";

/// The per-process global config dir every spawned `apb` uses. Created on
/// first use under cargo's `target/tmp`, never under the real home. A static
/// is never dropped, so sandboxes left by earlier runs are pruned here once
/// they are an hour old (a live suite's own dir is younger than that).
pub fn sandbox_config_dir() -> &'static Path {
    static DIR: OnceLock<PathBuf> = OnceLock::new();
    DIR.get_or_init(|| {
        let parent = Path::new(env!("CARGO_TARGET_TMPDIR"));
        std::fs::create_dir_all(parent).expect("create CARGO_TARGET_TMPDIR");
        prune_stale_sandboxes(parent);
        tempfile::Builder::new()
            .prefix(SANDBOX_PREFIX)
            .tempdir_in(parent)
            .expect("create sandbox config dir")
            .keep()
    })
}

fn prune_stale_sandboxes(parent: &Path) {
    let Ok(entries) = std::fs::read_dir(parent) else {
        return;
    };
    let cutoff = SystemTime::now() - Duration::from_secs(3600);
    for entry in entries.flatten() {
        let stale = entry
            .file_name()
            .to_string_lossy()
            .starts_with(SANDBOX_PREFIX)
            && entry
                .metadata()
                .and_then(|m| m.modified())
                .is_ok_and(|t| t < cutoff);
        if stale {
            let _ = std::fs::remove_dir_all(entry.path());
        }
    }
}

/// `assert_cmd` handle on the `apb` binary with a sandboxed global config dir.
pub fn apb() -> assert_cmd::Command {
    let mut cmd = assert_cmd::Command::new(apb_bin());
    cmd.env("APB_CONFIG_DIR", sandbox_config_dir());
    cmd
}

/// `std::process` handle on the `apb` binary with a sandboxed global config dir.
pub fn apb_std() -> std::process::Command {
    let mut cmd = std::process::Command::new(apb_bin());
    cmd.env("APB_CONFIG_DIR", sandbox_config_dir());
    cmd
}
