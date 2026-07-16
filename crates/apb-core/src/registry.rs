use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use serde::Serialize;

use crate::fsutil::atomic_write;
use crate::schema::{Playbook, SchemaError};

const DEFAULT_CONFIG: &str = "# playbook project config\n# server:\n#   port: 7321\n";

pub fn init_project(root: &Path) -> io::Result<()> {
    let playbook = root.join(".apb");
    for sub in ["playbooks", "profiles", "runs"] {
        std::fs::create_dir_all(playbook.join(sub))?;
    }
    let config = playbook.join("config.yaml");
    if !config.exists() {
        atomic_write(&config, DEFAULT_CONFIG.as_bytes())?;
    }
    Ok(())
}

#[derive(Debug, thiserror::Error)]
pub enum RegistryError {
    #[error("playbook `{0}` not found")]
    NotFound(String),
    #[error("playbook `{0}` has no current pointer")]
    NoCurrent(String),
    #[error("version in playbook.yaml (`{file}`) does not match directory (`{dir}`)")]
    VersionMismatch { file: String, dir: String },
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Schema(#[from] SchemaError),
    #[error("layout parse error: {0}")]
    Layout(String),
}

/// Checks that a path segment is safe to join: non-empty and does not
/// contain path separators or `..`, to rule out directory traversal (path traversal).
pub fn is_safe_segment(s: &str) -> bool {
    !s.is_empty() && !s.contains('/') && !s.contains('\\') && !s.contains("..")
}

/// Marker filename (inside the playbook directory, alongside `current`) that
/// flags a playbook as frozen. A frozen playbook rejects every definition
/// change (new version, patch, promote) so agents can no longer alter it;
/// runs and run-scoped supervisor intervention keep working. Freeze is a
/// playbook-wide lifecycle flag, not part of any immutable version's content,
/// which is why it lives next to `current` rather than inside a version dir.
pub const FROZEN_MARKER: &str = "frozen";

/// Whether the given playbook directory carries the frozen marker.
pub fn is_frozen_dir(playbook_dir: &Path) -> bool {
    playbook_dir.join(FROZEN_MARKER).is_file()
}

#[derive(Debug, Serialize)]
pub struct PlaybookSummary {
    pub id: String,
    pub name: String,
    pub description: String,
    pub current: String,
    pub versions: Vec<String>,
    /// True when the playbook is frozen (definition changes are refused).
    pub frozen: bool,
}

#[derive(Debug)]
pub struct LoadedPlaybook {
    pub playbook: Playbook,
    pub yaml: String,
    pub layout: Option<serde_json::Value>,
    pub version: String,
}

pub struct Registry {
    /// Directory that directly contains `playbooks/` and `profiles/`. For a
    /// project this is `<root>/.apb`; for the global store it's
    /// `<config_dir>` (spec 3, 5.1). Previously this held the project root and
    /// `.apb` was appended in every method - that tied the definition's storage
    /// location to the project root. Now the base is self-contained, which is
    /// what separates origin from execution.
    base: PathBuf,
}

impl Registry {
    /// Opens the registry from a project root: requires `<root>/.apb`.
    pub fn open(root: &Path) -> Result<Self, RegistryError> {
        let playbook = root.join(".apb");
        if !playbook.is_dir() {
            return Err(RegistryError::NotFound(".apb".into()));
        }
        Self::open_dir(&playbook)
    }

    /// Opens the registry from a directory that itself contains `playbooks/`
    /// (and optionally `profiles/`), without requiring a `.apb` wrapper. The
    /// entry point for the global store (spec 3).
    pub fn open_dir(base: &Path) -> Result<Self, RegistryError> {
        if !base.is_dir() {
            return Err(RegistryError::NotFound(base.to_string_lossy().into_owned()));
        }
        Ok(Self {
            base: base.to_path_buf(),
        })
    }

    fn playbooks_dir(&self) -> PathBuf {
        self.base.join("playbooks")
    }

    pub fn list(&self) -> Result<Vec<PlaybookSummary>, RegistryError> {
        let mut out = Vec::new();
        let dir = self.playbooks_dir();
        if !dir.is_dir() {
            return Ok(out);
        }
        for entry in fs::read_dir(&dir)? {
            let entry = entry?;
            if !entry.file_type()?.is_dir() {
                continue;
            }
            let id = entry.file_name().to_string_lossy().to_string();
            // A single broken definition (missing `current`, unparseable YAML,
            // version mismatch) must not take down the whole listing: `list`
            // powers the web dashboard, the CLI list and the MCP catalog. Skip
            // the broken entry - `apb doctor` is where broken definitions
            // surface for repair.
            let Ok(current) = self.read_current(&id) else {
                continue;
            };
            let Ok(loaded) = self.load(&id, Some(&current)) else {
                continue;
            };
            // Enumerating the version dirs must not be fatal either: a single
            // unreadable playbook directory should be skipped like the other
            // broken cases above, not abort the whole listing.
            let Ok(version_entries) = fs::read_dir(entry.path()) else {
                continue;
            };
            let mut versions: Vec<String> = version_entries
                .filter_map(Result::ok)
                .filter(|e| e.file_type().map(|t| t.is_dir()).unwrap_or(false))
                .map(|e| e.file_name().to_string_lossy().to_string())
                .filter(|name| name != "layouts" && name != "meta")
                .collect();
            versions.sort();
            out.push(PlaybookSummary {
                id,
                name: loaded.playbook.name.clone(),
                description: loaded.playbook.description.clone(),
                current,
                versions,
                frozen: is_frozen_dir(&entry.path()),
            });
        }
        out.sort_by(|a, b| a.id.cmp(&b.id));
        Ok(out)
    }

    /// Names (ids) of all playbooks without loading them - just the
    /// subdirectories under playbooks/. Unlike `list`, it reports broken
    /// playbooks too (it never loads them), so it is suitable for diagnostics
    /// (`apb doctor`), where each one is loaded independently. `list` silently
    /// skips definitions that fail to load; `playbook_ids` keeps them.
    pub fn playbook_ids(&self) -> Vec<String> {
        let dir = self.playbooks_dir();
        let mut out: Vec<String> = fs::read_dir(&dir)
            .into_iter()
            .flatten()
            .filter_map(Result::ok)
            .filter(|e| e.file_type().map(|t| t.is_dir()).unwrap_or(false))
            .map(|e| e.file_name().to_string_lossy().to_string())
            .collect();
        out.sort();
        out
    }

    fn read_current(&self, id: &str) -> Result<String, RegistryError> {
        let p = self.playbooks_dir().join(id).join("current");
        if !p.is_file() {
            return Err(RegistryError::NoCurrent(id.into()));
        }
        Ok(fs::read_to_string(p)?.trim().to_string())
    }

    pub fn load(&self, id: &str, version: Option<&str>) -> Result<LoadedPlaybook, RegistryError> {
        let base = self.playbooks_dir().join(id);
        if !base.is_dir() {
            return Err(RegistryError::NotFound(id.into()));
        }
        let version = match version {
            Some(v) => v.to_string(),
            None => self.read_current(id)?,
        };
        // Reject unsafe id/version before building paths, to rule out path
        // traversal (e.g. `../../etc` or `/etc`).
        if !is_safe_segment(id) || !is_safe_segment(&version) {
            return Err(RegistryError::NotFound(format!("{id}@{version}")));
        }
        let yaml_path = base.join(&version).join("playbook.yaml");
        if !yaml_path.is_file() {
            return Err(RegistryError::NotFound(format!("{id}@{version}")));
        }
        let yaml = fs::read_to_string(&yaml_path)?;
        let playbook = Playbook::from_yaml(&yaml)?;
        if playbook.version != version {
            return Err(RegistryError::VersionMismatch {
                file: playbook.version.clone(),
                dir: version,
            });
        }
        let layout_path = base.join("layouts").join(format!("{version}.yaml"));
        let layout = if layout_path.is_file() {
            let raw = fs::read_to_string(&layout_path)?;
            let val: serde_yaml_ng::Value =
                serde_yaml_ng::from_str(&raw).map_err(|e| RegistryError::Layout(e.to_string()))?;
            Some(serde_json::to_value(val).map_err(|e| RegistryError::Layout(e.to_string()))?)
        } else {
            None
        };
        Ok(LoadedPlaybook {
            playbook,
            yaml,
            layout,
            version,
        })
    }

    /// Whether the playbook `id` is frozen (definition changes are refused).
    pub fn is_frozen(&self, id: &str) -> bool {
        is_frozen_dir(&self.playbooks_dir().join(id))
    }

    /// Freezes or unfreezes the playbook `id` by adding/removing its marker
    /// file. The operation is idempotent. `NotFound` if the playbook does not
    /// exist. This is an operator action (surfaced only through the dashboard);
    /// agents have no path to call it.
    pub fn set_frozen(&self, id: &str, frozen: bool) -> Result<(), RegistryError> {
        if !is_safe_segment(id) {
            return Err(RegistryError::NotFound(id.into()));
        }
        let dir = self.playbooks_dir().join(id);
        if !dir.is_dir() {
            return Err(RegistryError::NotFound(id.into()));
        }
        let marker = dir.join(FROZEN_MARKER);
        if frozen {
            atomic_write(&marker, b"1")?;
        } else if marker.exists() {
            fs::remove_file(&marker)?;
        }
        Ok(())
    }

    pub fn profiles(&self) -> Vec<String> {
        let dir = self.base.join("profiles");
        let mut out: Vec<String> = fs::read_dir(dir)
            .ok()
            .into_iter()
            .flatten()
            .filter_map(Result::ok)
            .filter(|e| e.file_type().map(|t| t.is_dir()).unwrap_or(false))
            .map(|e| e.file_name().to_string_lossy().to_string())
            .collect();
        out.sort();
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn open_dir_works_without_dot_apb() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(tmp.path().join("playbooks")).unwrap();
        assert!(Registry::open_dir(tmp.path()).is_ok());
        assert!(Registry::open(tmp.path()).is_err());
    }

    #[test]
    fn open_delegates_to_dot_apb() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(tmp.path().join(".apb/playbooks")).unwrap();
        assert!(Registry::open(tmp.path()).is_ok());
    }

    const MINI: &str = "schema: 1\nid: mini\nname: Mini\nversion: {v}\nnodes:\n  - id: s\n    type: start\nedges: []\n";

    fn seed_version(base: &Path, id: &str, version: &str) -> PathBuf {
        let vdir = base.join("playbooks").join(id).join(version);
        std::fs::create_dir_all(&vdir).unwrap();
        let yaml = MINI
            .replace("id: mini", &format!("id: {id}"))
            .replace("{v}", version);
        std::fs::write(vdir.join("playbook.yaml"), yaml).unwrap();
        vdir
    }

    #[test]
    fn set_frozen_roundtrip_is_idempotent() {
        let tmp = tempfile::tempdir().unwrap();
        let base = tmp.path();
        seed_version(base, "alpha", "1.0.0");
        std::fs::write(
            base.join("playbooks").join("alpha").join("current"),
            "1.0.0",
        )
        .unwrap();

        let reg = Registry::open_dir(base).unwrap();
        assert!(!reg.is_frozen("alpha"));

        reg.set_frozen("alpha", true).unwrap();
        reg.set_frozen("alpha", true).unwrap(); // idempotent
        assert!(reg.is_frozen("alpha"));
        assert!(
            base.join("playbooks")
                .join("alpha")
                .join(FROZEN_MARKER)
                .is_file()
        );

        reg.set_frozen("alpha", false).unwrap();
        reg.set_frozen("alpha", false).unwrap(); // idempotent
        assert!(!reg.is_frozen("alpha"));

        // Unknown playbook: NotFound.
        assert!(reg.set_frozen("nope", true).is_err());
    }

    // A single playbook missing its `current` pointer must not take down the
    // whole listing: `list` powers the web dashboard, the CLI list and the MCP
    // catalog, so one broken definition would otherwise 500 the dashboard and
    // hide every healthy playbook.
    #[test]
    fn list_skips_broken_playbook_and_returns_healthy_ones() {
        let tmp = tempfile::tempdir().unwrap();
        let base = tmp.path();

        seed_version(base, "alpha", "1.0.0");
        std::fs::write(
            base.join("playbooks").join("alpha").join("current"),
            "1.0.0",
        )
        .unwrap();

        // Broken: a version dir exists but there is no `current` pointer.
        seed_version(base, "broken", "1.0.1");

        let reg = Registry::open_dir(base).unwrap();
        let list = reg
            .list()
            .expect("list must not fail on one broken playbook");
        let ids: Vec<&str> = list.iter().map(|p| p.id.as_str()).collect();
        assert_eq!(ids, vec!["alpha"]);
    }
}
