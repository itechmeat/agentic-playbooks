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

/// Records connector trust for a freshly installed embedded connector (see
/// [`crate::trust::record_bundled`]).
fn record_trust(name: &str, digest: &str) -> Option<String> {
    crate::trust::record_bundled(digest, name, crate::trust::Kind::Connector)
}

/// Installs the embedded official connector `name` into the global store and
/// records its trust in the same action.
///
/// Stages the embedded files into a unique `<connectors_dir>/.<name>.install-tmp-*`,
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
    let installed = crate::tree_install::install_tree(&base, name, &official.files, |_| force)
        .map_err(|e| match e {
            crate::tree_install::TreeInstallError::NeedsForce { path } => {
                InstallError::NeedsForce { path }
            }
            crate::tree_install::TreeInstallError::Io(m) => InstallError::Io(m),
        })?;
    let trust_warning = record_trust(name, &installed.digest);
    Ok(InstallReport {
        name: name.to_string(),
        version: official.version,
        digest: installed.digest,
        no_op: installed.no_op,
        trust_warning,
    })
}

/// What [`reconcile_official`] did with one installed official connector whose
/// files differ from the copy embedded in the running binary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Reconciled {
    /// The installed tree was a pristine copy an earlier apb installed (its
    /// digest is trusted with origin `Bundled`), so it was replaced by the
    /// embedded version.
    Updated {
        name: String,
        from_version: String,
        to_version: String,
    },
    /// The installed tree carries local changes (or an approval the user gave
    /// by hand), so it was left alone; the embedded version is offered as an
    /// update instead (`apb connector install <name> --force`).
    UpdateAvailable {
        name: String,
        installed_version: String,
        embedded_version: String,
    },
    /// The installed copy is newer shipped content than this binary's (an
    /// older binary runs beside a newer one): left alone, never moved back.
    NewerInstalled {
        name: String,
        installed_version: String,
        embedded_version: String,
    },
    /// The installed copy was pristine but replacing it failed.
    Failed { name: String, error: String },
}

/// Whether the files under `dir` are exactly the embedded `official` files.
fn matches_embedded(dir: &std::path::Path, official: &official::OfficialConnector) -> bool {
    crate::tree_install::dir_matches(dir, &official.files)
}

/// The embedded version of `name` when it is an official connector whose
/// installed copy differs from the one in this binary (`None` when it matches,
/// is not installed, is not official, or is newer than the embedded one).
/// Read-only: the one "update available" check every connector listing uses.
pub fn embedded_update(name: &str) -> Option<String> {
    let official = official::get(name)?;
    let dir = store::connectors_dir()?.join(name);
    if !dir.is_dir() || matches_embedded(&dir, &official) {
        return None;
    }
    // A newer installed copy is no update: reinstalling the embedded one
    // would move it back.
    let installed = store::list().into_iter().find(|c| c.name == name);
    if installed.is_some_and(|c| crate::bundled::is_older(&official.version, &c.version)) {
        return None;
    }
    Some(official.version)
}

/// Brings installed official connectors in line with the copies embedded in
/// the running binary after an upgrade. A copy that is exactly what an earlier
/// apb installed (its tree digest is trusted with origin `Bundled`) is replaced
/// by the embedded version, trust included; a copy with local changes is left
/// alone and reported as [`Reconciled::UpdateAvailable`], and a copy whose
/// version is newer than the embedded one (an older binary running beside a
/// newer one) is never moved back and is reported as
/// [`Reconciled::NewerInstalled`]. Connectors that
/// already match, and non-official connectors, produce no entry.
pub fn reconcile_official() -> Vec<Reconciled> {
    let trust = crate::trust::TrustStore::load();
    let limits = crate::content::TreeLimits::default();
    let Some(base) = store::connectors_dir() else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for installed in store::list() {
        let Some(official) = official::get(&installed.name) else {
            continue;
        };
        let dir = base.join(&installed.name);
        if matches_embedded(&dir, &official) {
            continue;
        }
        if crate::bundled::is_older(&official.version, &installed.version) {
            out.push(Reconciled::NewerInstalled {
                name: installed.name,
                installed_version: installed.version,
                embedded_version: official.version,
            });
            continue;
        }
        let pristine = crate::content::tree_digest(&dir, &limits)
            .ok()
            .and_then(|d| trust.origin(&d))
            == Some(crate::trust::OriginKind::Bundled);
        if !pristine {
            out.push(Reconciled::UpdateAvailable {
                name: installed.name,
                installed_version: installed.version,
                embedded_version: official.version,
            });
            continue;
        }
        out.push(match install_official(&installed.name, true) {
            Ok(report) => Reconciled::Updated {
                name: installed.name,
                from_version: installed.version,
                to_version: report.version,
            },
            Err(e) => Reconciled::Failed {
                name: installed.name,
                error: e.to_string(),
            },
        });
    }
    out
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
        assert!(install_leftovers(&cfg.path().join("connectors")).is_empty());
        drop(cfg);
    }

    /// The staging and backup folders of `EMBEDDED` left in `dir`.
    fn install_leftovers(dir: &std::path::Path) -> Vec<String> {
        let prefix = format!(".{EMBEDDED}.install-");
        std::fs::read_dir(dir)
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().to_string())
            .filter(|n| n.starts_with(&prefix))
            .collect()
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
        let forced = install_official(EMBEDDED, true).unwrap();

        assert!(!forced.no_op);
        assert!(dir.is_dir(), "the reinstalled target must be in place");
        assert!(!dir.join("EXTRA.md").exists());
        assert!(
            install_leftovers(&cfg.path().join("connectors")).is_empty(),
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

    /// Installs the embedded `name` as a binary shipping `version` would
    /// have: its manifest names that version, and the tree is trusted with
    /// origin `Bundled`. The installed directory.
    fn shipped_as(name: &str, version: &str) -> std::path::PathBuf {
        install_official(name, false).unwrap();
        let dir = store::connectors_dir().unwrap().join(name);
        let manifest = dir.join("connector.yaml");
        let yaml = std::fs::read_to_string(&manifest).unwrap();
        let embedded = official::get(name).unwrap().version;
        let changed = yaml.replacen(
            &format!("version: {embedded}"),
            &format!("version: {version}"),
            1,
        );
        assert_ne!(changed, yaml, "the manifest names its version");
        std::fs::write(&manifest, changed).unwrap();
        let digest =
            crate::content::tree_digest(&dir, &crate::content::TreeLimits::default()).unwrap();
        crate::trust::TrustStore::load()
            .approve_kind(
                &digest,
                name,
                crate::trust::Kind::Connector,
                crate::trust::OriginKind::Bundled,
            )
            .unwrap();
        dir
    }

    /// An older apb started beside a newer one never moves a newer,
    /// untouched connector back to its own copy: it reports it and leaves
    /// the files as they are.
    #[test]
    fn reconcile_leaves_a_newer_installed_connector_alone() {
        let _lock = crate::env_test_lock();
        let (_cfg, _guard) = set_config_dir();
        let dir = shipped_as("github", "99.0.0");
        let before = std::fs::read(dir.join("connector.yaml")).unwrap();

        let outcome = reconcile_official();

        assert_eq!(
            outcome,
            [Reconciled::NewerInstalled {
                name: "github".into(),
                installed_version: "99.0.0".into(),
                embedded_version: official::get("github").unwrap().version,
            }]
        );
        assert_eq!(std::fs::read(dir.join("connector.yaml")).unwrap(), before);
        // No listing offers the older embedded copy as an update either:
        // reinstalling it would be the same downgrade.
        assert_eq!(embedded_update("github"), None);
    }

    /// F20: after an upgrade, a pristine installed copy (the bytes an earlier
    /// apb installed, trusted as `Bundled`) is replaced by the embedded
    /// version, while a copy with local edits is kept and only reported.
    #[test]
    fn reconcile_updates_pristine_copies_and_reports_edited_ones() {
        let _lock = crate::env_test_lock();
        let (cfg, _guard) = set_config_dir();
        let limits = crate::content::TreeLimits::default();
        let base = cfg.path().join("connectors");

        // `github` as an older apb shipped it: a lower version, installed and
        // trusted with origin Bundled by that older binary.
        let old = shipped_as("github", "0.0.1");
        // `slack` with a local edit the user approved by hand.
        install_official("slack", false).unwrap();
        let edited = base.join("slack");
        std::fs::write(edited.join("NOTES.md"), "mine").unwrap();
        let edited_digest = crate::content::tree_digest(&edited, &limits).unwrap();
        crate::trust::TrustStore::load()
            .approve_kind(
                &edited_digest,
                "slack",
                crate::trust::Kind::Connector,
                crate::trust::OriginKind::LocallyApproved,
            )
            .unwrap();
        assert!(embedded_update("github").is_some());

        let outcome = reconcile_official();

        let github = official::get("github").unwrap();
        assert!(matches_embedded(&old, &github), "pristine copy updated");
        assert!(embedded_update("github").is_none());
        assert!(edited.join("NOTES.md").is_file(), "local edit kept");
        assert!(
            outcome.contains(&Reconciled::Updated {
                name: "github".into(),
                from_version: "0.0.1".into(),
                to_version: github.version.clone(),
            }),
            "{outcome:?}"
        );
        assert!(
            outcome
                .iter()
                .any(|r| matches!(r, Reconciled::UpdateAvailable { name, .. } if name == "slack")),
            "{outcome:?}"
        );
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
