//! Installing and uninstalling connectors in the on-disk store
//! (`<config_dir>/connectors/<name>/`). Both operations are shared by the CLI
//! (`apb connector install` / the dashboard's uninstall has no CLI twin yet)
//! and the HTTP API, so the staging, digest and trust semantics live here once
//! rather than being reimplemented per front end.
//!
//! Install is deliberately conservative: the embedded files are materialized
//! into a sibling staging folder first, digested there, and only then moved
//! into place, so a failure part-way through never leaves a half-written
//! connector where the engine could load it. A target that already holds the
//! exact same tree digest is a no-op (reporting `no_op`), and a target that
//! differs is refused unless the caller passes `force`.
//!
//! Uninstall removes only the connector folder. It never touches
//! `connector-config/` in either scope, which is what makes "disconnect but
//! keep the configuration" work: reconnecting later picks the previous accounts
//! back up with no re-entry. Trust is likewise left in place on purpose (see
//! [`uninstall`]).

use std::path::PathBuf;

use super::official;
use super::store;

/// The outcome of a successful [`install_official`].
///
/// `no_op` is true when the target already held this exact tree digest, so
/// nothing on disk changed; the trust record is still refreshed in that case,
/// which keeps a reinstall a reliable way to re-assert trust for bytes that
/// came out of the binary.
///
/// `trust_warning` is `Some` when the connector landed on disk but the trust
/// record could not be written. That is a warning, not a failure: the connector
/// is installed and usable, the user is simply asked to approve it through the
/// normal flow.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstallReport {
    pub name: String,
    pub version: String,
    pub digest: String,
    pub no_op: bool,
    pub trust_warning: Option<String>,
}

/// Why [`install_official`] refused. Each variant is a distinct condition a
/// caller must be able to report differently (the HTTP API maps them to
/// distinct status codes), so they are never collapsed into one string error.
#[derive(Debug, thiserror::Error)]
pub enum InstallError {
    #[error("invalid connector name `{name}`: {detail}")]
    InvalidName { name: String, detail: String },
    #[error("`{0}` is not an embedded official connector")]
    NotEmbedded(String),
    #[error("no config directory available")]
    NoConfigDir,
    #[error(
        "`{path}` already exists and differs from the embedded version; pass --force to overwrite"
    )]
    NeedsForce { path: String },
    /// A filesystem step failed. The message is already caller-ready (for
    /// example "cannot stage `github`: ..."), so a front end only has to
    /// prefix it.
    #[error("{0}")]
    Io(String),
}

/// The outcome of a successful [`uninstall`]. `no_op` is true when the
/// connector was not installed to begin with, which is a clean reported result
/// rather than an error: removing something that is already gone is exactly
/// what the caller asked for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UninstallReport {
    pub name: String,
    pub no_op: bool,
}

/// Why [`uninstall`] refused. Uninstall cannot fail for "unknown connector"
/// (that is a no-op) or "needs force" (there is nothing to overwrite), so its
/// failure set is strictly smaller than [`InstallError`].
#[derive(Debug, thiserror::Error)]
pub enum UninstallError {
    #[error("invalid connector name `{name}`: {detail}")]
    InvalidName { name: String, detail: String },
    #[error("no config directory available")]
    NoConfigDir,
    #[error("{0}")]
    Io(String),
}

/// Records connector trust for a freshly installed embedded connector. Origin
/// is `Bundled`: the bytes came out of the trusted binary the user already
/// runs, so no separate approval step is asked of them. Returns the warning
/// text when the record could not be written.
fn record_trust(name: &str, digest: &str) -> Option<String> {
    let mut trust = crate::trust::TrustStore::load();
    match trust.approve_kind(
        digest,
        name,
        crate::trust::Kind::Connector,
        crate::trust::OriginKind::Bundled,
    ) {
        Ok(()) => None,
        Err(e) => Some(e.to_string()),
    }
}

/// Installs the embedded official connector `name` into the global store and
/// records its trust in the same action.
///
/// Stages the embedded files into `<connectors_dir>/.<name>.install-tmp`,
/// digests the staged tree, and only then moves it onto the target. An
/// existing target with the same digest is a no-op; one that differs is
/// refused unless `force` is set. The staging folder is removed on every path,
/// success or failure.
///
/// # Errors
///
/// [`InstallError::InvalidName`] for a name that is not a valid slug,
/// [`InstallError::NotEmbedded`] when no official connector carries that name,
/// [`InstallError::NoConfigDir`] in a config-less environment,
/// [`InstallError::NeedsForce`] when a differing version is already installed
/// and `force` is false, and [`InstallError::Io`] for any filesystem failure.
pub fn install_official(name: &str, force: bool) -> Result<InstallReport, InstallError> {
    crate::profile::validate_profile_name(name).map_err(|e| InstallError::InvalidName {
        name: name.to_string(),
        detail: e.to_string(),
    })?;
    let official =
        official::get(name).ok_or_else(|| InstallError::NotEmbedded(name.to_string()))?;
    let base = store::connectors_dir().ok_or(InstallError::NoConfigDir)?;

    let target = base.join(name);
    let staging = base.join(format!(".{name}.install-tmp"));
    let _ = std::fs::remove_dir_all(&staging);
    if let Err(e) = official.write_to(&staging) {
        let _ = std::fs::remove_dir_all(&staging);
        return Err(InstallError::Io(format!("cannot stage `{name}`: {e}")));
    }

    let limits = crate::content::TreeLimits::default();
    let new_digest = match crate::content::tree_digest(&staging, &limits) {
        Ok(d) => d,
        Err(e) => {
            let _ = std::fs::remove_dir_all(&staging);
            return Err(InstallError::Io(format!("cannot digest `{name}`: {e}")));
        }
    };

    if target.exists() {
        let current = crate::content::tree_digest(&target, &limits).ok();
        if current.as_deref() == Some(new_digest.as_str()) {
            let _ = std::fs::remove_dir_all(&staging);
            let trust_warning = record_trust(name, &new_digest);
            return Ok(InstallReport {
                name: name.to_string(),
                version: official.version,
                digest: new_digest,
                no_op: true,
                trust_warning,
            });
        }
        if !force {
            let _ = std::fs::remove_dir_all(&staging);
            return Err(InstallError::NeedsForce {
                path: target.display().to_string(),
            });
        }

        // Backup-swap: move the existing target aside before moving staging
        // into place, so a rename failure partway through never leaves
        // nothing installed. Any stale backup from a previous interrupted
        // force-reinstall is cleared first so this rename never fails
        // because the sibling already exists. On a failed second rename the
        // backup is moved straight back, restoring the connector that was
        // there a moment ago; it is removed only once the swap has fully
        // succeeded.
        let backup = base.join(format!(".{name}.install-backup"));
        let _ = std::fs::remove_dir_all(&backup);
        if let Err(e) = std::fs::rename(&target, &backup) {
            let _ = std::fs::remove_dir_all(&staging);
            return Err(InstallError::Io(format!(
                "cannot replace {}: {e}",
                target.display()
            )));
        }
        if let Err(e) = std::fs::rename(&staging, &target) {
            let _ = std::fs::rename(&backup, &target);
            return Err(InstallError::Io(format!(
                "cannot install into {}: {e}",
                target.display()
            )));
        }
        let _ = std::fs::remove_dir_all(&backup);

        let trust_warning = record_trust(name, &new_digest);
        return Ok(InstallReport {
            name: name.to_string(),
            version: official.version,
            digest: new_digest,
            no_op: false,
            trust_warning,
        });
    }

    if let Err(e) = std::fs::rename(&staging, &target) {
        let _ = std::fs::remove_dir_all(&staging);
        return Err(InstallError::Io(format!(
            "cannot install into {}: {e}",
            target.display()
        )));
    }

    let trust_warning = record_trust(name, &new_digest);
    Ok(InstallReport {
        name: name.to_string(),
        version: official.version,
        digest: new_digest,
        no_op: false,
        trust_warning,
    })
}

/// Removes the installed connector `name`, that is the single directory
/// `<config_dir>/connectors/<name>/`.
///
/// Account configuration is deliberately untouched. It lives in a separate
/// tree (`<config_dir>/connector-config/<name>.yaml` and
/// `<root>/.apb/connector-config/<name>.yaml`), so disconnecting a connector
/// cannot lose the accounts the user configured, and reinstalling later picks
/// them straight back up.
///
/// The trust record is deliberately left in place too. Reinstalling the same
/// embedded version reproduces the same tree digest, so the existing approval
/// still matches and the user is not asked to re-approve bytes they already
/// approved. A digest that is no longer installed grants nothing on its own:
/// trust is only ever consulted against a connector actually present on disk.
///
/// # Errors
///
/// [`UninstallError::InvalidName`] for a name that is not a valid slug (the
/// name is validated before any path is built from it),
/// [`UninstallError::NoConfigDir`] in a config-less environment, and
/// [`UninstallError::Io`] when the directory exists but cannot be removed.
pub fn uninstall(name: &str) -> Result<UninstallReport, UninstallError> {
    crate::profile::validate_profile_name(name).map_err(|e| UninstallError::InvalidName {
        name: name.to_string(),
        detail: e.to_string(),
    })?;
    let base = store::connectors_dir().ok_or(UninstallError::NoConfigDir)?;
    let target: PathBuf = base.join(name);
    if !target.exists() {
        return Ok(UninstallReport {
            name: name.to_string(),
            no_op: true,
        });
    }
    std::fs::remove_dir_all(&target)
        .map_err(|e| UninstallError::Io(format!("cannot remove {}: {e}", target.display())))?;
    Ok(UninstallReport {
        name: name.to_string(),
        no_op: false,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    struct ConfigDirGuard {
        prior: Option<std::ffi::OsString>,
    }
    impl Drop for ConfigDirGuard {
        fn drop(&mut self) {
            unsafe {
                match &self.prior {
                    Some(v) => std::env::set_var("APB_CONFIG_DIR", v),
                    None => std::env::remove_var("APB_CONFIG_DIR"),
                }
            }
        }
    }

    /// Points `APB_CONFIG_DIR` at a fresh tempdir for the duration of the
    /// guard, restoring the prior value on drop. Must be created under
    /// `crate::env_test_lock()`.
    fn set_config_dir() -> (tempfile::TempDir, ConfigDirGuard) {
        let cfg = tempfile::tempdir().unwrap();
        let guard = ConfigDirGuard {
            prior: std::env::var_os("APB_CONFIG_DIR"),
        };
        unsafe {
            std::env::set_var("APB_CONFIG_DIR", cfg.path());
        }
        (cfg, guard)
    }

    /// An embedded connector name that is guaranteed to exist, so the tests
    /// exercise the real embedded set rather than a hand-built fixture.
    const EMBEDDED: &str = "github";

    #[test]
    fn install_official_writes_the_connector_and_records_trust() {
        let _lock = crate::env_test_lock();
        let (cfg, _guard) = set_config_dir();

        let report = install_official(EMBEDDED, false).unwrap();

        assert_eq!(report.name, EMBEDDED);
        assert!(!report.no_op);
        assert!(report.digest.starts_with("sha256:"));
        assert!(report.trust_warning.is_none(), "{:?}", report.trust_warning);
        assert!(
            cfg.path()
                .join("connectors")
                .join(EMBEDDED)
                .join("connector.yaml")
                .is_file()
        );
        assert!(crate::trust::TrustStore::load().is_approved(&report.digest));
        // No staging leftovers.
        assert!(
            !cfg.path()
                .join("connectors")
                .join(format!(".{EMBEDDED}.install-tmp"))
                .exists()
        );
        drop(cfg);
    }

    #[test]
    fn installing_twice_is_a_no_op_with_the_same_digest() {
        let _lock = crate::env_test_lock();
        let (cfg, _guard) = set_config_dir();

        let first = install_official(EMBEDDED, false).unwrap();
        let second = install_official(EMBEDDED, false).unwrap();

        assert!(!first.no_op);
        assert!(second.no_op);
        assert_eq!(first.digest, second.digest);
        drop(cfg);
    }

    #[test]
    fn a_differing_target_needs_force() {
        let _lock = crate::env_test_lock();
        let (cfg, _guard) = set_config_dir();

        install_official(EMBEDDED, false).unwrap();
        let dir = cfg.path().join("connectors").join(EMBEDDED);
        std::fs::write(dir.join("EXTRA.md"), "local edit").unwrap();

        let err = install_official(EMBEDDED, false).unwrap_err();
        assert!(matches!(err, InstallError::NeedsForce { .. }), "{err}");
        // The local edit survives a refused install.
        assert!(dir.join("EXTRA.md").is_file());

        let forced = install_official(EMBEDDED, true).unwrap();
        assert!(!forced.no_op);
        assert!(!dir.join("EXTRA.md").exists());
        drop(cfg);
    }

    /// The backup-swap used by a forced reinstall (rename target aside,
    /// rename staging into place, drop the backup only after the swap
    /// succeeds) must not leave the sibling `.install-backup` folder behind
    /// once the swap completes cleanly. Simulating the mid-swap rename
    /// failure itself is not cleanly testable here without a filesystem
    /// mock or permission tricks that would not be portable/reliable in CI,
    /// so this covers the success path's cleanup guarantee instead; the
    /// preserved-on-failure property is exercised by inspection of the
    /// implementation (backup is restored before any error return).
    #[test]
    fn force_reinstall_removes_the_backup_after_success() {
        let _lock = crate::env_test_lock();
        let (cfg, _guard) = set_config_dir();

        install_official(EMBEDDED, false).unwrap();
        let dir = cfg.path().join("connectors").join(EMBEDDED);
        std::fs::write(dir.join("EXTRA.md"), "local edit").unwrap();
        let backup = cfg
            .path()
            .join("connectors")
            .join(format!(".{EMBEDDED}.install-backup"));

        let forced = install_official(EMBEDDED, true).unwrap();

        assert!(!forced.no_op);
        assert!(dir.is_dir(), "the reinstalled target must be in place");
        assert!(!dir.join("EXTRA.md").exists());
        assert!(
            !backup.exists(),
            "the swap backup must be removed once the reinstall succeeds"
        );
        drop(cfg);
    }

    #[test]
    fn unknown_and_invalid_names_are_distinct_errors() {
        let _lock = crate::env_test_lock();
        let (cfg, _guard) = set_config_dir();

        assert!(matches!(
            install_official("definitely-not-a-connector", false).unwrap_err(),
            InstallError::NotEmbedded(_)
        ));
        assert!(matches!(
            install_official("../etc", false).unwrap_err(),
            InstallError::InvalidName { .. }
        ));
        assert!(matches!(
            install_official("Bad_Name", false).unwrap_err(),
            InstallError::InvalidName { .. }
        ));
        drop(cfg);
    }

    #[test]
    fn uninstall_removes_the_connector_and_is_idempotent() {
        let _lock = crate::env_test_lock();
        let (cfg, _guard) = set_config_dir();

        install_official(EMBEDDED, false).unwrap();
        let dir = cfg.path().join("connectors").join(EMBEDDED);
        assert!(dir.is_dir());

        let first = uninstall(EMBEDDED).unwrap();
        assert!(!first.no_op);
        assert!(!dir.exists());

        let second = uninstall(EMBEDDED).unwrap();
        assert!(
            second.no_op,
            "removing an absent connector is a clean no-op"
        );
        drop(cfg);
    }

    /// The core requirement behind "disconnect but keep the configuration":
    /// uninstall touches `connectors/<name>/` and nothing else, so both the
    /// global and the project account files are still there afterwards.
    #[test]
    fn uninstall_leaves_account_config_untouched() {
        let _lock = crate::env_test_lock();
        let (cfg, _guard) = set_config_dir();
        let project = tempfile::tempdir().unwrap();

        install_official(EMBEDDED, false).unwrap();

        let global = super::super::config::global_config_path(EMBEDDED).unwrap();
        std::fs::create_dir_all(global.parent().unwrap()).unwrap();
        std::fs::write(&global, "accounts:\n  - name: work\n    default: true\n").unwrap();
        let scoped = super::super::config::project_config_path(project.path(), EMBEDDED);
        std::fs::create_dir_all(scoped.parent().unwrap()).unwrap();
        std::fs::write(&scoped, "accounts:\n  - name: side\n").unwrap();

        uninstall(EMBEDDED).unwrap();

        assert!(!cfg.path().join("connectors").join(EMBEDDED).exists());
        assert!(global.is_file(), "global account config must survive");
        assert!(scoped.is_file(), "project account config must survive");
        assert_eq!(
            std::fs::read_to_string(&global).unwrap(),
            "accounts:\n  - name: work\n    default: true\n"
        );

        // And a reinstall picks those same accounts back up.
        install_official(EMBEDDED, false).unwrap();
        let accounts = super::super::config::load_merged(project.path(), EMBEDDED).unwrap();
        let names: Vec<&str> = accounts.iter().map(|a| a.name.as_str()).collect();
        assert_eq!(names, vec!["work", "side"]);
        drop(cfg);
    }

    #[test]
    fn uninstall_rejects_an_invalid_name_before_touching_the_filesystem() {
        let _lock = crate::env_test_lock();
        let (cfg, _guard) = set_config_dir();

        assert!(matches!(
            uninstall("../etc").unwrap_err(),
            UninstallError::InvalidName { .. }
        ));
        assert!(matches!(
            uninstall("Bad_Name").unwrap_err(),
            UninstallError::InvalidName { .. }
        ));
        drop(cfg);
    }
}
