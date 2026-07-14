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

#[derive(Debug, Serialize)]
pub struct PlaybookSummary {
    pub id: String,
    pub name: String,
    pub description: String,
    pub current: String,
    pub versions: Vec<String>,
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
            let current = self.read_current(&id)?;
            let mut versions: Vec<String> = fs::read_dir(entry.path())?
                .filter_map(Result::ok)
                .filter(|e| e.file_type().map(|t| t.is_dir()).unwrap_or(false))
                .map(|e| e.file_name().to_string_lossy().to_string())
                .filter(|name| name != "layouts" && name != "meta")
                .collect();
            versions.sort();
            let loaded = self.load(&id, Some(&current))?;
            out.push(PlaybookSummary {
                id,
                name: loaded.playbook.name.clone(),
                description: loaded.playbook.description.clone(),
                current,
                versions,
            });
        }
        out.sort_by(|a, b| a.id.cmp(&b.id));
        Ok(out)
    }

    /// Names (ids) of all playbooks without loading them - just the
    /// subdirectories under playbooks/. Unlike `list`, does not fail because
    /// of one broken playbook, so it is suitable for diagnostics (`apb
    /// doctor`), where each one is loaded independently.
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
}
