//! `apb self-update`: updates an install made by the apb installer, using the
//! install receipt that installer wrote under the XDG config dir. Builds
//! that came from Homebrew or from source have no receipt, so self-update
//! deliberately refuses to guess and instead points the user at the right
//! upgrade path for their install method.
//!
//! After an update the new binary brings the default playbooks and skills
//! setup installed in line with its own embedded content: this process
//! still runs the old build, so it re-executes the updated file as `apb
//! __reconcile-defaults` instead of reconciling with its own old copy.
//! When that cannot start, the next `apb mcp` or dashboard start does it.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use axoupdater::{AxoUpdater, AxoupdateError};

const NO_RECEIPT_GUIDANCE: &str = "self-update only works for installs made by the apb installer. \
If you installed with Homebrew, run: brew upgrade apb. \
If you built from source, rebuild and reinstall from the repo.";

/// Outcome of a self-update attempt, decoupled from process exit so it can be
/// asserted on directly in tests.
pub(crate) struct Outcome {
    pub(crate) code: u8,
    pub(crate) message: String,
    /// A new build was installed.
    pub(crate) updated: bool,
}

/// Runs `apb self-update`. `check` selects the dry-run path (report only,
/// never installs). Reads the install receipt from the real XDG config dir.
pub(crate) fn run_self_update(check: bool) -> ExitCode {
    let out = self_update_with_config_root(check, None);
    if out.code == 0 {
        println!("{}", out.message);
    } else {
        eprintln!("{}", out.message);
    }
    if out.updated {
        reconcile_in_new_binary();
    }
    ExitCode::from(out.code)
}

/// Runs `apb __reconcile-defaults` in the updated binary. A failure to
/// start it never fails the update.
fn reconcile_in_new_binary() {
    let started = std::env::current_exe().map(updated_binary).and_then(|exe| {
        std::process::Command::new(exe)
            .arg(RECONCILE_COMMAND)
            .status()
    });
    if let Err(e) = started {
        eprintln!(
            "could not check the default playbooks and skills with the new build ({e}); the next apb mcp or dashboard start does it"
        );
    }
}

/// The hidden command the updated binary runs.
const RECONCILE_COMMAND: &str = "__reconcile-defaults";

/// The path of the updated binary, from this process's own path. On Linux
/// the path of a running file that was replaced reads `<path> (deleted)`;
/// the new file sits at `<path>`.
fn updated_binary(exe: PathBuf) -> PathBuf {
    match exe.to_str().and_then(|s| s.strip_suffix(" (deleted)")) {
        Some(path) => PathBuf::from(path),
        None => exe,
    }
}

/// `apb __reconcile-defaults`: the reconcile of the defaults setup
/// installed, with its notes on stdout and failures on stderr. Exit 1 when
/// an item could not be updated.
pub(crate) fn reconcile_defaults_cmd() -> ExitCode {
    let notes = apb_mcp::setup::reconcile_if_set_up();
    apb_core::bundled::print_notes("apb: ", &notes, false);
    if notes.iter().any(|n| n.failed) {
        ExitCode::from(1)
    } else {
        ExitCode::SUCCESS
    }
}

/// Testable core of `self-update`. `config_root`, when set, overrides where
/// axoupdater looks for the install receipt (its own config-path override,
/// not a general apb setting) so the no-receipt path can be exercised
/// deterministically against an empty directory instead of the real XDG
/// config dir. axoupdater only exposes this override via an environment
/// variable (`AXOUPDATER_CONFIG_PATH`; see its `receipt::get_config_paths`),
/// so this function sets it for the duration of the call and restores the
/// previous value afterward.
///
/// Safety note: mutating process env is normally unsound under parallel
/// tests, but cargo-nextest (the runner this workspace uses, see
/// `crates/apb-cli/tests/main.rs`) runs each `#[test]` in its own process,
/// so this mutation never races another test. It is still scoped and
/// restored so a plain `cargo test` (single-process, multi-threaded) run
/// stays safe as long as no other test reads `AXOUPDATER_CONFIG_PATH`
/// concurrently; this crate has exactly one test that touches it.
pub(crate) fn self_update_with_config_root(check: bool, config_root: Option<&Path>) -> Outcome {
    let _env_guard = config_root.map(ConfigRootGuard::set);

    let mut updater = AxoUpdater::new_for("apb");
    if let Err(e) = updater.load_receipt() {
        return match e {
            AxoupdateError::NoReceipt { .. } => Outcome {
                code: 2,
                message: NO_RECEIPT_GUIDANCE.to_string(),
                updated: false,
            },
            other => Outcome {
                code: 2,
                message: format!("self-update failed: {other}"),
                updated: false,
            },
        };
    }

    if check {
        match updater.is_update_needed_sync() {
            Ok(true) => Outcome {
                code: 10,
                message: "update available".to_string(),
                updated: false,
            },
            Ok(false) => Outcome {
                code: 0,
                message: "apb is up to date".to_string(),
                updated: false,
            },
            Err(e) => Outcome {
                code: 2,
                message: format!("self-update check failed: {e}"),
                updated: false,
            },
        }
    } else {
        match updater.run_sync() {
            Ok(Some(result)) => Outcome {
                code: 0,
                message: format!("updated to {}", result.new_version),
                updated: true,
            },
            Ok(None) => Outcome {
                code: 0,
                message: "apb is up to date".to_string(),
                updated: false,
            },
            Err(e) => Outcome {
                code: 2,
                message: format!("self-update failed: {e}"),
                updated: false,
            },
        }
    }
}

/// Scopes `AXOUPDATER_CONFIG_PATH` to a single call, restoring whatever was
/// there before (including "unset") on drop. Exists only to make the
/// no-receipt path testable; production calls (`config_root: None`) never
/// touch the environment.
struct ConfigRootGuard {
    previous: Option<std::ffi::OsString>,
}

impl ConfigRootGuard {
    fn set(path: &Path) -> Self {
        let previous = std::env::var_os("AXOUPDATER_CONFIG_PATH");
        // SAFETY: scoped to this process's self-update invocation; see the
        // doc comment on `self_update_with_config_root` for why this is safe
        // under this workspace's test runner (nextest, one process per test).
        unsafe {
            std::env::set_var("AXOUPDATER_CONFIG_PATH", path);
        }
        Self { previous }
    }
}

impl Drop for ConfigRootGuard {
    fn drop(&mut self) {
        // SAFETY: see `ConfigRootGuard::set`.
        unsafe {
            match &self.previous {
                Some(v) => std::env::set_var("AXOUPDATER_CONFIG_PATH", v),
                None => std::env::remove_var("AXOUPDATER_CONFIG_PATH"),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_receipt_maps_to_guidance_and_code_2() {
        let dir = tempfile::tempdir().unwrap();
        // axoupdater resolves the receipt under XDG config; point it at an
        // empty dir via its own env override so `load_receipt` fails
        // deterministically without touching the real XDG config dir.
        let out = self_update_with_config_root(false, Some(dir.path()));
        assert_eq!(out.code, 2);
        assert!(out.message.contains("brew upgrade") || out.message.contains("source"));
        assert!(!out.updated, "a failed update reconciles nothing");
    }

    #[test]
    fn the_updated_binary_is_the_replaced_path() {
        assert_eq!(
            updated_binary(PathBuf::from("/opt/bin/apb (deleted)")),
            PathBuf::from("/opt/bin/apb")
        );
        assert_eq!(
            updated_binary(PathBuf::from("/opt/bin/apb")),
            PathBuf::from("/opt/bin/apb")
        );
    }

    #[test]
    fn the_reconcile_command_is_hidden() {
        use clap::CommandFactory;
        let cmd = crate::Cli::command();
        let sub = cmd.find_subcommand(RECONCILE_COMMAND).unwrap();
        assert!(sub.is_hide_set());
    }

    #[test]
    fn check_flag_is_parsed() {
        // covered by clap derive; assert the CLI accepts it and produces the
        // expected variant (Command has no Debug derive, so match instead of
        // asserting equality)
        use clap::Parser;
        let cli = crate::Cli::try_parse_from(["apb", "self-update", "--check"]).unwrap();
        let matched = matches!(
            cli.command,
            Some(crate::Command::SelfUpdate { check: true })
        );
        assert!(matched, "expected SelfUpdate {{ check: true }}");
    }
}
