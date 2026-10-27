//! `runs/<id>/estimate.json`: the estimate a run started with, written once
//! at start, so the budget gate and every run surface use the figures the
//! person saw, and a resume uses them too.

use std::path::Path;

use super::types::RunEstimate;
use crate::error::EngineError;

const FILE: &str = "estimate.json";

/// Writes the snapshot unless one exists; `Ok(false)` when it did. Write
/// once even under a race: the bytes go to a private temp file first, which
/// is then linked into place, and a link never replaces an existing file.
/// On a filesystem without hard links the snapshot is written atomically in
/// place instead, still only when none exists.
pub fn write(run_dir: &Path, estimate: &RunEstimate) -> Result<bool, EngineError> {
    let path = run_dir.join(FILE);
    if path.exists() {
        return Ok(false);
    }
    let bytes = serde_json::to_vec_pretty(estimate)
        .map_err(|e| EngineError::Invalid(format!("cannot encode the run estimate: {e}")))?;
    let tmp = run_dir.join(format!(
        ".{FILE}.{}.{}",
        std::process::id(),
        apb_core::clock::now_nanos()
    ));
    apb_core::fsutil::atomic_write_private(&tmp, &bytes)?;
    let linked = std::fs::hard_link(&tmp, &path);
    let _ = std::fs::remove_file(&tmp);
    place(linked, &path, &bytes)
}

/// What the link of the snapshot into `path` came to: placed, already
/// there, or (the link failed for another reason) an atomic private write
/// of `bytes` when `path` still does not exist.
fn place(linked: std::io::Result<()>, path: &Path, bytes: &[u8]) -> Result<bool, EngineError> {
    match linked {
        Ok(()) => Ok(true),
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => Ok(false),
        Err(_) if path.exists() => Ok(false),
        Err(_) => {
            apb_core::fsutil::atomic_write_private(path, bytes)?;
            Ok(true)
        }
    }
}

/// The run's snapshot, `None` when there is none (a sub-playbook child, a
/// run started without a budget or a shown estimate, or before estimates
/// existed) or it does not read.
pub fn read(run_dir: &Path) -> Option<RunEstimate> {
    let bytes = std::fs::read(run_dir.join(FILE)).ok()?;
    serde_json::from_slice(&bytes).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn unsupported() -> std::io::Result<()> {
        Err(std::io::Error::from(std::io::ErrorKind::Unsupported))
    }

    #[test]
    fn a_failed_link_falls_back_to_a_private_write() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(FILE);
        assert!(place(unsupported(), &path, b"{}").unwrap());
        assert_eq!(std::fs::read(&path).unwrap(), b"{}");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&path).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600);
        }
    }

    #[test]
    fn the_fallback_never_replaces_an_existing_snapshot() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(FILE);
        std::fs::write(&path, b"first").unwrap();
        assert!(!place(unsupported(), &path, b"second").unwrap());
        let exists = Err(std::io::Error::from(std::io::ErrorKind::AlreadyExists));
        assert!(!place(exists, &path, b"second").unwrap());
        assert_eq!(std::fs::read(&path).unwrap(), b"first");
    }
}
