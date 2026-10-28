//! The profile skills copy a minimal-environment agent is pointed at
//! (`--add-dir`), shared by every node of one profile bundle in a run (issue
//! #67 item 7).
//!
//! The directory an agent CLI is pointed at is part of its system prompt, so
//! a per-node path made every node of a profile start with a different prefix
//! and miss the provider's prompt cache (measured on claude: about 3k tokens
//! written to the cache again on every node). The path is now keyed by the
//! profile bundle digest, and the copy is verified against the run snapshot's
//! skill digests before every step, so an attempt that edits its copy cannot
//! hand the edit to the next one.

use super::*;

/// The shared skills copy of `entry` under `run_dir`, laid down from the run
/// snapshot when it is missing or no longer matches the snapshot.
pub(crate) fn shared_skills_dir(
    run_dir: &Path,
    entry: &ManifestProfile,
) -> Result<PathBuf, EngineError> {
    let parent = run_dir.join("agent-skills");
    apb_core::fsutil::create_dir_under(run_dir, &parent)?;
    let key: String = entry
        .bundle_digest
        .trim_start_matches("sha256:")
        .chars()
        .filter(char::is_ascii_hexdigit)
        .take(16)
        .collect();
    let dir = parent.join(format!("bundle-{key}"));
    // Parallel nodes of one profile check and lay down the same directory.
    let _lock = apb_core::fsutil::lock_dir(&parent, &format!(".lock-{key}"))?;
    if !intact(&dir, entry) {
        match std::fs::remove_dir_all(&dir) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e.into()),
        }
        materialize_isolated_skills(run_dir, entry, &dir)?;
    }
    Ok(dir)
}

/// Whether `dir` holds exactly the profile's skills with the snapshot's
/// digests.
fn intact(dir: &Path, entry: &ManifestProfile) -> bool {
    let skills = dir.join(".agents/skills");
    let Ok(listed) = std::fs::read_dir(&skills) else {
        return false;
    };
    let mut names: Vec<String> = listed
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    let mut expected: Vec<String> = entry.skills.iter().map(|s| s.name.clone()).collect();
    expected.sort();
    if names != expected {
        return false;
    }
    let limits = apb_core::content::TreeLimits::default();
    entry.skills.iter().all(|s| {
        apb_core::content::tree_digest(&skills.join(&s.name), &limits).is_ok_and(|d| d == s.digest)
    })
}

/// Materializes profile skills as REAL copies from the run snapshot into an
/// isolated attempt's own directory (completion-plan Task 3): its sandbox box
/// or its scratch directory. The source is the snapshot
/// (`run_dir/profiles/<scope>/<name>/skills/<sscope>/<sname>`), NOT the live
/// `.agents/skills`: editing a skill after the run has started has no effect on
/// the run. The `.claude/skills` bridge is aimed at the real copies via symlinks.
/// The directory is created even without skills.
pub(crate) fn materialize_isolated_skills(
    run_dir: &Path,
    entry: &ManifestProfile,
    workdir: &Path,
) -> Result<(), EngineError> {
    let skills_parent = workdir.join(".agents/skills");
    std::fs::create_dir_all(&skills_parent)?;
    for sk in &entry.skills {
        let src = run_dir
            .join("profiles")
            .join(&entry.scope)
            .join(&entry.name)
            .join("skills")
            .join(&sk.scope)
            .join(&sk.name);
        apb_core::fsutil::copy_tree(&src, &skills_parent.join(&sk.name))?;
    }
    if !entry.skills.is_empty() {
        let claude_parent = workdir.join(".claude/skills");
        // Fail-closed: the isolated node's workdir is fresh, so the
        // `.claude/skills` bridge must be laid down cleanly. Any note here is a
        // real failure (a symlink could not be created, etc.), not a benign case of
        // "already exists/foreign bridge"; silently continuing would mean running the
        // agent without skills visible via `.claude` and passing off an incorrect run as a success.
        let notes = apb_core::skills::ensure_claude_bridge(&skills_parent, &claude_parent);
        if !notes.is_empty() {
            return Err(EngineError::Invalid(format!(
                "isolated skill bridge failed: {}",
                notes.join("; ")
            )));
        }
    }
    Ok(())
}

/// Clears the skill copies a continued session's directory `workdir` holds,
/// so [`materialize_isolated_skills`] lays them down fresh. A symlink on the
/// way (the agent works in that directory and may have planted one, at
/// `.claude`, `.agents` or their `skills`) is removed as a link and never
/// followed, so nothing outside `workdir` is deleted.
pub(crate) fn clear_isolated_skills(workdir: &Path) -> Result<(), EngineError> {
    for (top, sub) in [(".agents", "skills"), (".claude", "skills")] {
        for path in [workdir.join(top), workdir.join(top).join(sub)] {
            match std::fs::symlink_metadata(&path) {
                Ok(m) if m.file_type().is_symlink() => {
                    remove_link(&path)?;
                    break;
                }
                // A plain file where a directory belongs: remove it, never
                // `remove_dir_all` a file.
                Ok(m) if !m.is_dir() => {
                    std::fs::remove_file(&path)?;
                    break;
                }
                Ok(_) if path.ends_with(sub) => std::fs::remove_dir_all(&path)?,
                Ok(_) => {}
                Err(e) if is_absent(&e) => break,
                Err(e) => return Err(e.into()),
            }
        }
    }
    Ok(())
}

/// A missing path, or one below a regular file (ENOTDIR).
fn is_absent(e: &std::io::Error) -> bool {
    e.kind() == std::io::ErrorKind::NotFound || e.kind() == std::io::ErrorKind::NotADirectory
}

/// Removes the symlink at `path` itself. A directory symlink on Windows is
/// removed with `remove_dir`, which never touches its target either.
fn remove_link(path: &Path) -> std::io::Result<()> {
    std::fs::remove_file(path).or_else(|e| {
        if cfg!(windows) {
            std::fs::remove_dir(path)
        } else {
            Err(e)
        }
    })
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    /// A continued session's directory whose skill paths were replaced by
    /// links out of it: the links go, what they point at stays.
    #[test]
    fn clearing_skills_removes_planted_links_and_never_follows_them() {
        let wd = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let keep = outside.path().join("skills/keep.md");
        std::fs::create_dir_all(keep.parent().unwrap()).unwrap();
        std::fs::write(&keep, "x").unwrap();
        // `.claude` itself links out; `.agents/skills` links out.
        std::os::unix::fs::symlink(outside.path(), wd.path().join(".claude")).unwrap();
        std::fs::create_dir_all(wd.path().join(".agents")).unwrap();
        std::os::unix::fs::symlink(
            outside.path().join("skills"),
            wd.path().join(".agents/skills"),
        )
        .unwrap();
        clear_isolated_skills(wd.path()).unwrap();
        assert!(keep.is_file(), "a file outside the workdir was deleted");
        assert!(std::fs::symlink_metadata(wd.path().join(".claude")).is_err());
        assert!(std::fs::symlink_metadata(wd.path().join(".agents/skills")).is_err());

        // A real skills directory is cleared as before.
        std::fs::create_dir_all(wd.path().join(".claude/skills/old")).unwrap();
        clear_isolated_skills(wd.path()).unwrap();
        assert!(!wd.path().join(".claude/skills").exists());
        assert!(wd.path().join(".claude").is_dir());
    }

    /// A regular file where `.claude`, `.agents` or a `skills` directory
    /// belongs does not fail the clear.
    #[test]
    fn clearing_skills_removes_plain_files_in_place_of_directories() {
        let wd = tempfile::tempdir().unwrap();
        std::fs::write(wd.path().join(".claude"), "f").unwrap();
        std::fs::create_dir_all(wd.path().join(".agents")).unwrap();
        std::fs::write(wd.path().join(".agents/skills"), "f").unwrap();
        clear_isolated_skills(wd.path()).unwrap();
        assert!(!wd.path().join(".claude").exists());
        assert!(!wd.path().join(".agents/skills").exists());
        assert!(wd.path().join(".agents").is_dir());
    }
}
