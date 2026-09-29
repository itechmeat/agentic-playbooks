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
/// changes. The `.apb/` directory is always excluded (from both the diff and
/// the untracked set), mirroring the `files_fingerprint` walk: the engine
/// writes run and cache state under `.apb/` every run, so counting it would
/// change the fingerprint on every run even when the workspace is otherwise
/// clean, defeating the cache in projects that do not gitignore `.apb/`.
///
/// Returns `None` on any git failure: not a git work tree, git binary
/// unavailable, or no HEAD commit yet. This is a deliberate "unknown state"
/// signal for callers (treat as uncacheable), never a swallowed error
/// pretending to be a valid fingerprint.
pub fn git_fingerprint(root: &Path, exclude: &[String]) -> Option<String> {
    let ex = build_globset(exclude).ok()?;
    let head = git(root, &["rev-parse", "HEAD"])?;

    let pathspecs: Vec<String> = exclude.iter().map(|g| format!(":(exclude){g}")).collect();
    let mut diff_args = vec!["diff", "HEAD", "--binary", "--", ".", ":(exclude).apb"];
    diff_args.extend(pathspecs.iter().map(String::as_str));
    let diff = git(root, &diff_args)?;

    let untracked = git(root, &["ls-files", "--others", "--exclude-standard", "-z"])?;

    let mut acc = Vec::new();
    acc.extend_from_slice(head.as_bytes());
    acc.extend_from_slice(sha256_hex(diff.as_bytes()).as_bytes());

    let mut files: Vec<&str> = untracked
        .split('\0')
        .filter(|p| !p.is_empty() && *p != ".apb" && !p.starts_with(".apb/") && !ex.is_match(p))
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

// --- 0.23.0 protected paths (C6) ---------------------------------------------

/// `git` for the protected-path listings (0.23.0): the same as [`git`] with
/// the repository's config-driven helpers switched off, so a listing never
/// runs a program named in the tree's own `.git/config` (a
/// `core.fsmonitor` hook).
fn git_listing(root: &Path, args: &[&str]) -> Option<String> {
    let mut full = vec!["-c", "core.fsmonitor=false"];
    full.extend_from_slice(args);
    git(root, &full)
}

/// Whether a directory between `root` and `rel` (both excluded) is a
/// symlink, so `root.join(rel)` may resolve outside `root`. A missing
/// component ends the walk: nothing below it exists to resolve.
pub fn has_symlinked_parent(root: &Path, rel: &str) -> bool {
    let mut cur = root.to_path_buf();
    let mut parts: Vec<&str> = rel.split('/').filter(|s| !s.is_empty()).collect();
    parts.pop();
    for part in parts {
        cur.push(part);
        match std::fs::symlink_metadata(&cur) {
            Ok(m) if m.file_type().is_symlink() => return true,
            Ok(_) => {}
            Err(_) => return false,
        }
    }
    false
}

/// The files under `root` that match `include`, as sorted paths relative to
/// `root` with `/` separators. On a git work tree the candidates are the
/// files git tracks or would track (`ls-files --cached --others
/// --exclude-standard`), so ignored paths are never matched; elsewhere every
/// file. `.git` and `.apb` are always left out, and so are symlinks, paths
/// below a symlinked directory and tracked files missing from disk.
pub fn matching_files(root: &Path, include: &[String]) -> Result<Vec<String>, FingerprintError> {
    let inc = build_globset(include).map_err(FingerprintError::Glob)?;
    let candidates: Vec<String> = match git_listing(
        root,
        &[
            "ls-files",
            "-z",
            "--cached",
            "--others",
            "--exclude-standard",
        ],
    ) {
        Some(listed) => listed
            .split('\0')
            .filter(|p| !p.is_empty())
            .map(str::to_string)
            .collect(),
        None => {
            let mut paths = Vec::new();
            walk(root, root, &mut paths)?;
            paths
        }
    };
    let mut out: Vec<String> = candidates
        .into_iter()
        .filter(|p| {
            !p.split('/').any(|seg| seg == ".git" || seg == ".apb")
                && inc.is_match(p)
                && !has_symlinked_parent(root, p)
                && std::fs::symlink_metadata(root.join(p)).is_ok_and(|m| m.is_file())
        })
        .collect();
    out.sort_unstable();
    out.dedup();
    Ok(out)
}

/// The paths under `root` git ignores right now, relative to `root`: files
/// by name and wholly ignored directories with a trailing `/`
/// (`ls-files --others --ignored --exclude-standard --directory`). Empty
/// outside a git work tree, where nothing is ignored.
pub fn ignored_paths(root: &Path) -> Vec<String> {
    git_listing(
        root,
        &[
            "ls-files",
            "-z",
            "--others",
            "--ignored",
            "--exclude-standard",
            "--directory",
        ],
    )
    .map(|listed| {
        listed
            .split('\0')
            .filter(|p| !p.is_empty())
            .map(str::to_string)
            .collect()
    })
    .unwrap_or_default()
}

/// The content digest of each of `files` (relative to `root`).
pub fn file_digests(
    root: &Path,
    files: &[String],
) -> Result<std::collections::BTreeMap<String, String>, FingerprintError> {
    let mut out = std::collections::BTreeMap::new();
    for rel in files {
        let bytes = std::fs::read(root.join(rel))?;
        out.insert(rel.clone(), sha256_hex(&bytes));
    }
    Ok(out)
}

// --- end of protected paths ------------------------------------------------------

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
    fn git_fingerprint_excludes_apb_dir() {
        // A project that does NOT gitignore `.apb/`: the engine writes run and
        // cache state under `.apb/` every run, and that state must never move
        // the fingerprint (else the zero-declaration git path never hits).
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        git(root, &["init", "-q"]);
        git(root, &["config", "user.email", "t@t"]);
        git(root, &["config", "user.name", "t"]);
        std::fs::write(root.join("a.txt"), "one").unwrap();
        git(root, &["add", "."]);
        git(root, &["commit", "-qm", "c1"]);
        let base = git_fingerprint(root, &[]).unwrap();
        // Simulate a run writing cache objects and run state under `.apb/`.
        std::fs::create_dir_all(root.join(".apb/cache/objects/ab")).unwrap();
        std::fs::write(root.join(".apb/cache/objects/ab/whatever"), "obj").unwrap();
        std::fs::create_dir_all(root.join(".apb/runs/r1")).unwrap();
        std::fs::write(root.join(".apb/runs/r1/events.jsonl"), "{}").unwrap();
        assert_eq!(
            base,
            git_fingerprint(root, &[]).unwrap(),
            "`.apb/` run and cache state must not change the fingerprint"
        );
        // A normal untracked file still changes it.
        std::fs::write(root.join("undeclared.txt"), "x").unwrap();
        assert_ne!(
            base,
            git_fingerprint(root, &[]).unwrap(),
            "a normal untracked file must still change the fingerprint"
        );
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
