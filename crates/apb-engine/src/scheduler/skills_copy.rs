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

/// Materializes profile skills as REAL copies from the run snapshot into the
/// isolated per-node workdir (completion-plan Task 3). The source is the snapshot
/// (`run_dir/profiles/<scope>/<name>/skills/<sscope>/<sname>`), NOT the live
/// `.agents/skills`: editing a skill after the run has started has no effect on
/// the run. The `.claude/skills` bridge is aimed at the real copies via symlinks.
/// The workdir is created even without skills (an isolated node execution directory).
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
