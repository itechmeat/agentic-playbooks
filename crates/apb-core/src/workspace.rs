//! Workspace identity (spec 6.1).
//!
//! A single git repository lives in multiple checkouts (clones, worktrees), so
//! neither a path nor a file committed to git can serve as identity. Identity
//! is two-level:
//! - `workspace_id` - a local uuid in `.apb/workspace.local` (does NOT go into
//!   git, is gitignored);
//! - `repository_fingerprint` - an optional hash of the git remote, links
//!   workspaces of the same repository together.

use std::path::Path;
use std::process::Command;

use sha2::{Digest, Sha256};

const WORKSPACE_FILE: &str = "workspace.local";

/// Reads or creates the local `workspace_id` in `<root>/.apb/workspace.local`.
/// Also ensures that `<root>/.apb/.gitignore` ignores this file (otherwise a
/// clone would drag the id along and collide with the original). Best-effort
/// for the gitignore: a failed append does not prevent returning the id.
pub fn ensure_id(root: &Path) -> std::io::Result<String> {
    let playbook = root.join(".apb");
    std::fs::create_dir_all(&playbook)?;
    let path = playbook.join(WORKSPACE_FILE);
    if let Ok(existing) = std::fs::read_to_string(&path) {
        let trimmed = existing.trim();
        if !trimmed.is_empty() {
            ensure_gitignored(&playbook);
            return Ok(trimmed.to_string());
        }
    }
    let id = format!("ws-{}", uuid::Uuid::new_v4().simple());
    // Atomic creation: only if the file doesn't exist yet. On a race (two
    // first calls at once), the loser gets AlreadyExists and re-reads the
    // already-written id - so concurrent callers return the SAME id, matching
    // what's on disk.
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        opts.mode(0o600); // like other state files (fsutil convention)
    }
    match opts.open(&path) {
        Ok(mut f) => {
            use std::io::Write;
            f.write_all(id.as_bytes())?;
            ensure_gitignored(&playbook);
            Ok(id)
        }
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
            ensure_gitignored(&playbook);
            // The file exists, but the race winner may not have written the id
            // yet (a window between create_new and write). Wait a bit for its
            // write before treating the file as abandoned.
            for _ in 0..20 {
                let persisted = std::fs::read_to_string(&path)?.trim().to_string();
                if !persisted.is_empty() {
                    return Ok(persisted);
                }
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
            // Still empty: the winner apparently died without writing. Take it over ourselves.
            crate::fsutil::atomic_write(&path, id.as_bytes())?;
            Ok(id)
        }
        Err(e) => Err(e),
    }
}

/// Ensures the `workspace.local` line is present in `<playbook>/.gitignore`. Best-effort.
fn ensure_gitignored(playbook: &Path) {
    let gi = playbook.join(".gitignore");
    let needed = WORKSPACE_FILE;
    let current = std::fs::read_to_string(&gi).unwrap_or_default();
    if current.lines().any(|l| l.trim() == needed) {
        return;
    }
    let mut next = current;
    if !next.is_empty() && !next.ends_with('\n') {
        next.push('\n');
    }
    next.push_str(needed);
    next.push('\n');
    let _ = crate::fsutil::atomic_write(&gi, next.as_bytes());
}

/// Repository fingerprint based on `git remote origin` (spec 6.1). `None` if
/// git is unavailable or no remote is set. Best-effort, for linking clones
/// together - not for security.
pub fn fingerprint(root: &Path) -> Option<String> {
    let out = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["config", "--get", "remote.origin.url"])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let url = String::from_utf8_lossy(&out.stdout);
    let url = url.trim();
    if url.is_empty() {
        return None;
    }
    let mut h = Sha256::new();
    h.update(url.as_bytes());
    Some(format!(
        "sha256:{}",
        crate::content::hex_lower(&h.finalize())
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ensure_id_is_stable_and_gitignored() {
        let tmp = tempfile::tempdir().unwrap();
        let a = ensure_id(tmp.path()).unwrap();
        let b = ensure_id(tmp.path()).unwrap();
        assert_eq!(a, b, "workspace id must be stable across calls");
        assert!(a.starts_with("ws-"));
        let gi = std::fs::read_to_string(tmp.path().join(".apb/.gitignore")).unwrap();
        assert!(gi.lines().any(|l| l.trim() == "workspace.local"));
    }

    #[test]
    fn fingerprint_none_without_git() {
        let tmp = tempfile::tempdir().unwrap();
        assert!(fingerprint(tmp.path()).is_none());
    }
}
