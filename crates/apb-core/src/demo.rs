//! The keyless demo (`apb demo`, the dashboard's "Try the demo"): a
//! playbook shipped in the binary that runs on any machine with nothing to
//! set up. A script node writes `report.md` with `printf`, a person
//! approves or rejects it at a review gate; no agent, no connector, no
//! network, no decision model, no key.
//!
//! It lives in `<config dir>/demo/`, a project of its own, with the
//! playbook `demo-report` 1.0.0 current and one quick scenario (`approve`).
//! Installing is idempotent; an installed demo whose files differ from the
//! shipped ones is reinstalled only on request ([`install`] with `reset`),
//! the old definition going to the project's trash. [`remove`] deletes
//! the directory and unregisters the project.
//!
//! The demo is an ordinary playbook: it is written like any version a
//! person saves with scripts ([`crate::versioning::create_with_scripts`]),
//! neither imported nor approved, so its start asks for the same
//! acknowledgement as any playbook nobody approved. Nothing in the run gate,
//! the trust store or the launch policy knows its id.

use std::path::{Path, PathBuf};

use serde::Serialize;

use crate::content::TreeFile;
use crate::schema::Playbook;

/// The demo playbook's id.
pub const DEMO_ID: &str = "demo-report";
/// The version the demo installs.
pub const DEMO_VERSION: &str = "1.0.0";
/// The demo project's directory under the config dir.
pub const DEMO_DIR: &str = "demo";
/// The demo's scenario.
pub const DEMO_SCENARIO: &str = "approve";

const PLAYBOOK: &str = include_str!("../assets/demo/playbook.yaml");
const SCRIPT_NAME: &str = "write-report.sh";
const SCRIPT: &str = include_str!("../assets/demo/scripts/write-report.sh");
const SCENARIO: &str = include_str!("../assets/demo/evals/scenarios/approve.yaml");

/// The installed demo, for the surfaces.
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DemoProject {
    /// The demo project's directory.
    pub dir: String,
    pub playbook: String,
    pub version: String,
    /// The project's id in the projects registry; absent when registration
    /// is off on this machine.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub workspace_id: Option<String>,
}

/// What is in the demo directory now.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DemoState {
    /// No demo playbook there.
    Absent,
    /// The shipped files, `current` on the shipped version.
    Installed,
    /// A demo playbook that differs from the shipped one.
    Modified,
}

/// Why the demo could not be installed or removed.
#[derive(Debug, thiserror::Error)]
pub enum DemoError {
    #[error(
        "the demo in {0} differs from the one this apb ships: reinstall it with --reset (its definition goes to the project's trash)"
    )]
    Modified(String),
    #[error("{0} does not hold the apb demo: nothing was removed")]
    NotDemo(String),
    #[error(
        "the demo in {0} was deleted, but its projects registry entry could not be removed: {1}"
    )]
    Unregister(String, String),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Versioning(#[from] crate::versioning::VersioningError),
}

/// The demo directory under `config_dir`.
pub fn demo_dir(config_dir: &Path) -> PathBuf {
    config_dir.join(DEMO_DIR)
}

fn playbook_dir(dir: &Path) -> PathBuf {
    crate::versioning::playbooks_dir(dir).join(DEMO_ID)
}

fn scenario_path(dir: &Path) -> PathBuf {
    crate::scenario::scenarios_dir(&playbook_dir(dir)).join(format!("{DEMO_SCENARIO}.yaml"))
}

/// A definition compared by meaning, the `version:` field set aside: the
/// store keeps a normalized copy.
fn same_definition(a: &str, b: &str) -> bool {
    let norm = |y: &str| {
        Playbook::from_yaml(y).ok().map(|mut p| {
            p.version = String::new();
            serde_json::to_value(p).ok()
        })
    };
    matches!((norm(a), norm(b)), (Some(Some(x)), Some(Some(y))) if x == y)
}

/// The state of the demo in `dir`.
pub fn state(dir: &Path) -> DemoState {
    let pdir = playbook_dir(dir);
    if !pdir.is_dir() {
        return DemoState::Absent;
    }
    let read = |p: PathBuf| std::fs::read_to_string(p).ok();
    let vdir = pdir.join(DEMO_VERSION);
    let current = crate::versioning::read_current_pointer(&pdir);
    let shipped = current.as_deref() == Some(DEMO_VERSION)
        && read(vdir.join("playbook.yaml")).is_some_and(|y| same_definition(&y, PLAYBOOK))
        && read(vdir.join("scripts").join(SCRIPT_NAME)).as_deref() == Some(SCRIPT)
        && read(scenario_path(dir)).as_deref() == Some(SCENARIO);
    if shipped {
        DemoState::Installed
    } else {
        DemoState::Modified
    }
}

/// The demo in `dir` as the surfaces show it.
pub fn project(dir: &Path) -> DemoProject {
    DemoProject {
        dir: dir.to_string_lossy().into_owned(),
        playbook: DEMO_ID.to_string(),
        version: DEMO_VERSION.to_string(),
        workspace_id: crate::projects::registered_id(dir),
    }
}

fn refuse_link(dir: &Path) -> Result<(), DemoError> {
    match std::fs::symlink_metadata(dir) {
        Ok(m) if m.file_type().is_symlink() => Err(DemoError::NotDemo(dir.display().to_string())),
        _ => Ok(()),
    }
}

/// Writes the shipped demo into `dir` as a fresh project.
fn write_fresh(dir: &Path) -> Result<(), DemoError> {
    crate::registry::init_project(dir)?;
    let script = TreeFile {
        rel: SCRIPT_NAME.to_string(),
        bytes: SCRIPT.as_bytes().to_vec(),
        executable: true,
    };
    crate::versioning::create_with_scripts(dir, DEMO_ID, PLAYBOOK, DEMO_VERSION, &[script])?;
    let scenario = scenario_path(dir);
    if let Some(parent) = scenario.parent() {
        std::fs::create_dir_all(parent)?;
    }
    crate::fsutil::atomic_write(&scenario, SCENARIO.as_bytes())?;
    Ok(())
}

/// Installs the demo in `dir` (made a project when it is not one) and
/// registers it for the dashboard. Nothing is written when the shipped demo
/// is there already. A demo that differs is refused unless `reset`, which
/// moves its definition to the project's trash and installs the shipped one.
pub fn install(dir: &Path, reset: bool) -> Result<DemoProject, DemoError> {
    refuse_link(dir)?;
    match state(dir) {
        DemoState::Installed => {}
        DemoState::Absent => write_fresh(dir)?,
        DemoState::Modified if !reset => {
            return Err(DemoError::Modified(dir.display().to_string()));
        }
        DemoState::Modified => {
            crate::versioning::delete_playbook(dir, DEMO_ID, crate::clock::now_ms())?;
            write_fresh(dir)?;
        }
    }
    crate::projects::touch(dir);
    Ok(project(dir))
}

/// Deletes the demo project in `dir`, then unregisters it; `false` when
/// there is none. A directory that holds no demo playbook is refused and
/// left alone; a registry entry that could not be removed is an error after
/// the directory is gone. Whether a run still drives the demo is the
/// caller's check (the engine's busy lock), held across this call.
pub fn remove(dir: &Path) -> Result<bool, DemoError> {
    refuse_link(dir)?;
    if !dir.exists() {
        return Ok(false);
    }
    if state(dir) == DemoState::Absent {
        return Err(DemoError::NotDemo(dir.display().to_string()));
    }
    let id = crate::projects::registered_id(dir);
    std::fs::remove_dir_all(dir)?;
    if let Some(id) = id {
        crate::projects::remove(&id)
            .map_err(|e| DemoError::Unregister(dir.display().to_string(), e.to_string()))?;
    }
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The shipped definition is valid as it is: no validator error, and the
    /// review routes cover both options.
    #[test]
    fn the_shipped_demo_is_a_valid_playbook_with_no_agent() {
        let p = Playbook::from_yaml(PLAYBOOK).unwrap();
        assert_eq!((p.id.as_str(), p.version.as_str()), (DEMO_ID, DEMO_VERSION));
        let report = crate::validate::validate(&p, &crate::validate::ValidationContext::default());
        let errors: Vec<_> = report
            .issues
            .iter()
            .filter(|i| i.severity == crate::validate::Severity::Error)
            .collect();
        assert!(errors.is_empty(), "{errors:?}");
        assert!(!p.nodes.iter().any(|n| n.kind.runs_agent()));
        let scenarios =
            crate::scenario::parse(SCENARIO, DEMO_SCENARIO).expect("the shipped scenario parses");
        assert!(crate::scenario::applies_to(&scenarios, DEMO_VERSION));
    }

    /// The trust digest of the installed demo, and whether anything
    /// approved it.
    fn digest_and_approved(dir: &Path) -> (String, bool) {
        let d = crate::registry::Registry::open(dir)
            .unwrap()
            .load(DEMO_ID, None)
            .unwrap()
            .trust_digest()
            .unwrap();
        let approved = crate::trust::TrustStore::load().is_approved(&d);
        (d, approved)
    }

    /// Installing writes the shipped demo once, registered and approved by
    /// nobody; again it changes nothing; a changed demo is refused until a
    /// reset, whose old definition goes to the trash; removing unregisters
    /// it and deletes the directory, and leaves a directory that holds no
    /// demo alone.
    #[test]
    fn the_demo_installs_once_unapproved_and_is_removed_whole() {
        let _cfg = crate::test_config();
        let config = crate::config::config_dir().unwrap();
        let dir = demo_dir(&config);

        let p = install(&dir, false).unwrap();
        assert_eq!(state(&dir), DemoState::Installed);
        let ws = p
            .workspace_id
            .clone()
            .expect("registered for the dashboard");
        assert!(
            crate::projects::list_active()
                .iter()
                .any(|e| e.workspace_id == ws)
        );
        let (digest, approved) = digest_and_approved(&dir);
        assert!(!approved, "nothing vouches for the demo's script");
        let prov = crate::versioning::read_provenance(&dir, DEMO_ID, DEMO_VERSION)
            .unwrap()
            .unwrap();
        assert!(
            prov.imported.is_none(),
            "not an import: no digest approval is required"
        );
        let script = dir
            .join(".apb/playbooks/demo-report/1.0.0/scripts")
            .join(SCRIPT_NAME);
        assert_eq!(std::fs::read_to_string(&script).unwrap(), SCRIPT);

        // Again: nothing is written.
        let mtime = std::fs::metadata(&script).unwrap().modified().unwrap();
        assert_eq!(install(&dir, false).unwrap(), p);
        assert_eq!(
            std::fs::metadata(&script).unwrap().modified().unwrap(),
            mtime
        );
        assert_eq!(digest_and_approved(&dir).0, digest);

        // A changed scenario: refused, then reinstalled on request.
        std::fs::write(scenario_path(&dir), "schema: 1\nid: approve\n").unwrap();
        assert_eq!(state(&dir), DemoState::Modified);
        assert!(matches!(install(&dir, false), Err(DemoError::Modified(_))));
        install(&dir, true).unwrap();
        assert_eq!(state(&dir), DemoState::Installed);
        assert_eq!(crate::versioning::list_trash(&dir).unwrap().len(), 1);

        assert!(remove(&dir).unwrap());
        assert!(!dir.exists());
        assert_eq!(crate::projects::registered_id(&dir), None);
        assert!(
            !crate::projects::list_active()
                .iter()
                .any(|e| e.workspace_id == ws)
        );
        assert!(!remove(&dir).unwrap());

        let other = config.join("not-a-demo");
        std::fs::create_dir_all(&other).unwrap();
        assert!(matches!(remove(&other), Err(DemoError::NotDemo(_))));
        assert!(other.exists());
    }
}
