//! The `candidate` pointer file (issue #192): which version of a playbook is
//! on trial, next to `current`. Kept apart from [`crate::candidate`] so the
//! version store ([`crate::versioning`]) can read and drop it without
//! depending on the trial logic built on top of it.

use std::fs;
use std::io;
use std::path::Path;

use crate::registry::is_safe_segment;

/// The pointer file next to `current`.
pub const CANDIDATE_FILE: &str = "candidate";

/// The candidate version of the playbook in `playbook_dir`, if one is set.
pub fn read_candidate(playbook_dir: &Path) -> Option<String> {
    fs::read_to_string(playbook_dir.join(CANDIDATE_FILE))
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty() && is_safe_segment(s))
}

/// Removes the pointer when it still names `version`; `Ok(true)` when it
/// did. A pointer that moved on to a newer candidate is left alone.
pub fn clear_candidate_if(playbook_dir: &Path, version: &str) -> io::Result<bool> {
    if read_candidate(playbook_dir).as_deref() != Some(version) {
        return Ok(false);
    }
    match fs::remove_file(playbook_dir.join(CANDIDATE_FILE)) {
        Ok(()) => Ok(true),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(e),
    }
}
