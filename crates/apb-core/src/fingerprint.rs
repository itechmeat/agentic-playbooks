//! Workspace fingerprints for the node cache (spec 2026-07-19).
use crate::content::sha256_hex;
use crate::validate::build_globset;
use std::path::Path;
use std::process::Command;

/// Errors from [`files_fingerprint`]. `git_fingerprint` never returns an
/// error: any git failure (not a repo, no HEAD yet, git missing) collapses
/// to `None`, per the node-cache spec.
#[derive(Debug, thiserror::Error)]
pub enum FingerprintError {
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("invalid glob `{0}`")]
    Glob(String),
}

/// Git-aware fingerprint: HEAD + staged/unstaged diff + untracked contents.
///
/// `exclude` globs (a node's declared outputs) are filtered out of the dirty
/// state via a git pathspec exclusion on the diff and a globset filter on
/// untracked paths, so a node's own products never count as workspace
/// changes.
///
/// Returns `None` on any git failure: not a git work tree, git binary
/// unavailable, or no HEAD commit yet. This is a deliberate "unknown state"
/// signal for callers (treat as uncacheable), never a swallowed error
/// pretending to be a valid fingerprint.
pub fn git_fingerprint(root: &Path, exclude: &[String]) -> Option<String> {
    let ex = build_globset(exclude).ok()?;
    let head = git(root, &["rev-parse", "HEAD"])?;

    let pathspecs: Vec<String> = exclude.iter().map(|g| format!(":(exclude){g}")).collect();
    let mut diff_args = vec!["diff", "HEAD", "--binary", "--", "."];
    diff_args.extend(pathspecs.iter().map(String::as_str));
    let diff = git(root, &diff_args)?;

    let untracked = git(root, &["ls-files", "--others", "--exclude-standard", "-z"])?;

    let mut acc = Vec::new();
    acc.extend_from_slice(head.as_bytes());
    acc.extend_from_slice(sha256_hex(diff.as_bytes()).as_bytes());

    let mut files: Vec<&str> = untracked
        .split('\0')
        .filter(|p| !p.is_empty() && !ex.is_match(p))
        .collect();
    files.sort_unstable();
    for path in files {
        let bytes = std::fs::read(root.join(path)).ok()?;
        acc.extend_from_slice(path.as_bytes());
        acc.extend_from_slice(sha256_hex(&bytes).as_bytes());
    }

    Some(sha256_hex(&acc))
}

/// Run a git subcommand in `root` and return stdout as text, or `None` if
/// the process could not run or exited non-zero.
fn git(root: &Path, args: &[&str]) -> Option<String> {
    let out = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(args)
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    Some(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// Hash of exactly the files matching `include` minus `exclude`, as sorted
/// (relative path, content digest) pairs. Skips `.git` and `.apb`
/// directories entirely (never descends into them).
pub fn files_fingerprint(
    root: &Path,
    include: &[String],
    exclude: &[String],
) -> Result<String, FingerprintError> {
    let inc = build_globset(include).map_err(FingerprintError::Glob)?;
    let ex = build_globset(exclude).map_err(FingerprintError::Glob)?;

    let mut paths = Vec::new();
    walk(root, root, &mut paths)?;
    paths.sort_unstable();

    let mut acc = Vec::new();
    for rel in paths {
        if inc.is_match(&rel) && !ex.is_match(&rel) {
            let bytes = std::fs::read(root.join(&rel))?;
            acc.extend_from_slice(rel.as_bytes());
            acc.extend_from_slice(sha256_hex(&bytes).as_bytes());
        }
    }
    Ok(sha256_hex(&acc))
}

/// Recursively collect `dir`'s files as paths relative to `root`, skipping
/// `.git` and `.apb` directories.
///
/// Uses `DirEntry::file_type` (no-follow, `lstat`-based) rather than
/// `Path::is_dir` (which follows symlinks) to decide whether to recurse, so
/// a symlink cycle in the workspace cannot cause unbounded recursion.
/// Symlinks themselves are neither recursed into nor hashed as files.
fn walk(root: &Path, dir: &Path, out: &mut Vec<String>) -> std::io::Result<()> {
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        let name = entry.file_name();
        let file_type = entry.file_type()?;
        if file_type.is_dir() {
            if name == ".git" || name == ".apb" {
                continue;
            }
            walk(root, &path, out)?;
        } else if file_type.is_file()
            && let Ok(rel) = path.strip_prefix(root)
        {
            out.push(rel.to_string_lossy().replace('\\', "/"));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Fixture-only wrapper around the production `git` helper: disables
    /// commit signing so a developer's global `commit.gpgsign = true` can
    /// never make a fixture `commit` hang or fail (which the ignored return
    /// value of a bare `git(...)` call would otherwise hide until a later
    /// `git_fingerprint(...).unwrap()` panics). Production code is
    /// unchanged; only test fixtures route through this.
    fn git(root: &Path, args: &[&str]) -> Option<String> {
        let mut full_args = vec!["-c", "commit.gpgsign=false"];
        full_args.extend_from_slice(args);
        super::git(root, &full_args)
    }

    #[test]
    fn git_fingerprint_tracks_dirty_state() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        git(root, &["init", "-q"]);
        git(root, &["config", "user.email", "t@t"]);
        git(root, &["config", "user.name", "t"]);
        std::fs::write(root.join("a.txt"), "one").unwrap();
        git(root, &["add", "."]);
        git(root, &["commit", "-qm", "c1"]);
        let clean = git_fingerprint(root, &[]).unwrap();
        assert_eq!(clean, git_fingerprint(root, &[]).unwrap()); // stable
        std::fs::write(root.join("a.txt"), "two").unwrap(); // unstaged edit
        let dirty = git_fingerprint(root, &[]).unwrap();
        assert_ne!(clean, dirty);
        std::fs::write(root.join("new.txt"), "x").unwrap(); // untracked
        assert_ne!(dirty, git_fingerprint(root, &[]).unwrap());
    }

    #[test]
    fn git_fingerprint_none_outside_git() {
        let dir = tempfile::tempdir().unwrap();
        assert!(git_fingerprint(dir.path(), &[]).is_none());
    }

    #[test]
    fn git_fingerprint_exclude_ignores_declared_outputs() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        git(root, &["init", "-q"]);
        git(root, &["config", "user.email", "t@t"]);
        git(root, &["config", "user.name", "t"]);
        std::fs::write(root.join("a.txt"), "one").unwrap();
        git(root, &["add", "."]);
        git(root, &["commit", "-qm", "c1"]);
        let exclude = vec!["out.json".to_string()];
        let clean = git_fingerprint(root, &exclude).unwrap();
        std::fs::write(root.join("out.json"), "artifact").unwrap(); // declared output
        assert_eq!(clean, git_fingerprint(root, &exclude).unwrap());
        std::fs::write(root.join("undeclared.txt"), "x").unwrap();
        assert_ne!(clean, git_fingerprint(root, &exclude).unwrap());
    }

    #[test]
    fn files_fingerprint_matches_only_globs() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::write(root.join("src/a.rs"), "a").unwrap();
        std::fs::write(root.join("other.md"), "m").unwrap();
        let fp = files_fingerprint(root, &["src/**".into()], &[]).unwrap();
        std::fs::write(root.join("other.md"), "changed").unwrap();
        assert_eq!(
            fp,
            files_fingerprint(root, &["src/**".into()], &[]).unwrap()
        );
        std::fs::write(root.join("src/a.rs"), "b").unwrap();
        assert_ne!(
            fp,
            files_fingerprint(root, &["src/**".into()], &[]).unwrap()
        );
    }
}
