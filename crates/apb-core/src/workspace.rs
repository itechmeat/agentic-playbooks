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

/// Everything machine-local APB writes under `<root>/.apb/`, as lines of
/// `<root>/.apb/.gitignore`: the workspace id, run directories (event logs,
/// node outputs and per-attempt agent transcripts, which can hold whatever
/// the agent saw), the node result cache, soft-deleted playbooks, schema
/// migration backups and the workdir lock. The one list `ensure_local_ignored`
/// writes; a new runtime path under `.apb/` belongs here.
pub const LOCAL_ENTRIES: &[&str] = &[
    WORKSPACE_FILE,
    "runs/",
    "cache/",
    "trash/",
    "backup-*/",
    "workdir.lock",
    "locks/",
];

/// The workspace id recorded in `<root>/.apb/workspace.local`, read only when
/// that file is a regular file (not followed through a symlink): a checkout
/// whose identity file is a link to another project's would otherwise claim
/// that project's id. `None` when the file is missing, empty or not a regular
/// file. The one read of a workspace's identity (registration, reachability).
pub fn read_id(root: &Path) -> Option<String> {
    let path = root.join(".apb").join(WORKSPACE_FILE);
    let meta = std::fs::symlink_metadata(&path).ok()?;
    if !meta.file_type().is_file() {
        return None;
    }
    let id = std::fs::read_to_string(&path).ok()?.trim().to_string();
    (!id.is_empty()).then_some(id)
}

/// Reads or creates the local `workspace_id` in `<root>/.apb/workspace.local`.
/// Also ensures that `<root>/.apb/.gitignore` ignores this file (otherwise a
/// clone would drag the id along and collide with the original) and the rest
/// of `LOCAL_ENTRIES`. Best-effort for the gitignore: a failed append does not
/// prevent returning the id.
///
/// A `workspace.local` that exists but is not a regular file (a symlink, a
/// directory) is refused with `InvalidData` rather than followed or replaced:
/// it is not this workspace's identity.
pub fn ensure_id(root: &Path) -> std::io::Result<String> {
    let playbook = root.join(".apb");
    std::fs::create_dir_all(&playbook)?;
    let path = playbook.join(WORKSPACE_FILE);
    if let Ok(meta) = std::fs::symlink_metadata(&path)
        && !meta.file_type().is_file()
    {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("{} is not a regular file", path.display()),
        ));
    }
    if let Some(existing) = read_id(root) {
        ensure_local_ignored(root);
        return Ok(existing);
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
            ensure_local_ignored(root);
            Ok(id)
        }
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
            ensure_local_ignored(root);
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

/// Ensures `<root>/.apb/.gitignore` lists every `LOCAL_ENTRIES` line: keeps
/// the existing lines as they are and appends only the missing entries, so a
/// repeat call changes nothing. A line counts as present in its anchored
/// (`/runs/`) or slash-less (`runs`) spelling too. Best-effort, never fails
/// its caller, and never creates `.apb/` itself (a driver of a deleted
/// workspace must not bring it back).
pub fn ensure_local_ignored(root: &Path) {
    let playbook = root.join(".apb");
    if !playbook.is_dir() {
        return;
    }
    let gi = playbook.join(".gitignore");
    let current = std::fs::read_to_string(&gi).unwrap_or_default();
    let norm = |l: &str| {
        l.trim()
            .trim_start_matches('/')
            .trim_end_matches('/')
            .to_string()
    };
    let present: Vec<String> = current.lines().map(norm).collect();
    let missing: Vec<&str> = LOCAL_ENTRIES
        .iter()
        .copied()
        .filter(|e| !present.contains(&norm(e)))
        .collect();
    if missing.is_empty() {
        return;
    }
    let mut next = current;
    if !next.is_empty() && !next.ends_with('\n') {
        next.push('\n');
    }
    for entry in missing {
        next.push_str(entry);
        next.push('\n');
    }
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

    fn gitignore(root: &Path) -> String {
        std::fs::read_to_string(root.join(".apb/.gitignore")).unwrap()
    }

    #[test]
    fn fresh_project_gets_every_local_entry() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir(tmp.path().join(".apb")).unwrap();
        ensure_local_ignored(tmp.path());
        let expected: String = LOCAL_ENTRIES.iter().map(|e| format!("{e}\n")).collect();
        assert_eq!(gitignore(tmp.path()), expected);
        for entry in [
            "workspace.local",
            "runs/",
            "cache/",
            "trash/",
            "backup-*/",
            "workdir.lock",
        ] {
            assert!(LOCAL_ENTRIES.contains(&entry), "{entry} missing");
        }
    }

    #[test]
    fn existing_lines_are_kept_in_order_and_only_missing_entries_added() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir(tmp.path().join(".apb")).unwrap();
        // User lines, one entry already present, one in its anchored and one
        // in its slash-less spelling, no trailing newline.
        std::fs::write(
            tmp.path().join(".apb/.gitignore"),
            "# mine\nnotes.txt\nworkspace.local\n/runs/\ncache",
        )
        .unwrap();
        ensure_local_ignored(tmp.path());
        let mut expected = String::from("# mine\nnotes.txt\nworkspace.local\n/runs/\ncache\n");
        for e in LOCAL_ENTRIES {
            if !["workspace.local", "runs/", "cache/"].contains(e) {
                expected.push_str(&format!("{e}\n"));
            }
        }
        assert_eq!(gitignore(tmp.path()), expected);
    }

    #[test]
    fn repeated_ensures_never_duplicate_a_line() {
        let tmp = tempfile::tempdir().unwrap();
        ensure_id(tmp.path()).unwrap();
        let once = gitignore(tmp.path());
        ensure_local_ignored(tmp.path());
        ensure_id(tmp.path()).unwrap();
        assert_eq!(gitignore(tmp.path()), once);
        for e in LOCAL_ENTRIES {
            assert_eq!(once.lines().filter(|l| l == e).count(), 1, "{e}");
        }
    }

    #[test]
    fn a_missing_apb_dir_is_not_created() {
        let tmp = tempfile::tempdir().unwrap();
        ensure_local_ignored(tmp.path());
        assert!(!tmp.path().join(".apb").exists());
    }

    #[test]
    fn fingerprint_none_without_git() {
        let tmp = tempfile::tempdir().unwrap();
        assert!(fingerprint(tmp.path()).is_none());
    }
}
