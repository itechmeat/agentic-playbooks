//! Copying a playbook definition into another scope's store, whole: every
//! version, the `current` pointer, layouts, provenance and lifecycle. The
//! copy is assembled in a staging directory beside `playbooks/` (so a
//! listing never sees half a playbook) and renamed into place in one step.
//! Content digests do not depend on where a definition lives, so the copy
//! keeps the trust of the source.
//!
//! The legacy instruction draft (`meta/instruction-draft.md`) is left out:
//! run input belongs to each project's memory ([`crate::run_memory`]), not
//! to a definition shared by every project.

use std::io;
use std::path::{Path, PathBuf};

use crate::fsutil::{copy_tree, ensure_no_symlink_below, lock_dir};
use crate::registry::is_safe_segment;

/// Why a playbook could not be copied.
#[derive(Debug, thiserror::Error)]
pub enum ScopeMoveError {
    #[error("playbook id `{0}` is not a safe name")]
    InvalidId(String),
    #[error("playbook `{0}` not found")]
    NotFound(String),
    #[error("a playbook `{0}` already exists at the destination")]
    IdTaken(String),
    #[error(transparent)]
    Io(#[from] io::Error),
}

/// The staging area beside `<dst_parent>/playbooks`.
const STAGING_DIR: &str = "playbooks-staging";
/// What the copy leaves out, relative to the playbook directory.
const LEFT_OUT: &[&str] = &["meta/instruction-draft.md"];

/// Copies the playbook directory `src_playbook_dir` to
/// `<dst_parent>/playbooks/<id>` and returns the new directory. Holds the
/// lock `<dst_parent>/playbooks/<id>.lock` while it works; refuses
/// [`ScopeMoveError::IdTaken`] when the destination exists. On any failure
/// nothing is left at the destination or in staging.
///
/// The destination is claimed first with an exclusive `mkdir`, the same
/// claim a new draft makes, so a writer that does not take the lock cannot
/// create the id between the check and the move; the staged copy then
/// replaces the empty claimed directory in one rename (on Windows, where a
/// rename cannot replace a directory, the empty claim is removed just
/// before it). A process killed between the claim and the rename leaves an
/// empty `playbooks/<id>`, which refuses the id until it is removed by
/// hand: nothing can tell it from a draft being created. Symbolic links are
/// copied as links, never followed; the digest check of the copy refuses
/// one that leaves the tree.
pub fn copy_playbook_to(
    src_playbook_dir: &Path,
    dst_parent: &Path,
    id: &str,
) -> Result<PathBuf, ScopeMoveError> {
    if !is_safe_segment(id) {
        return Err(ScopeMoveError::InvalidId(id.to_string()));
    }
    if !src_playbook_dir.is_dir() {
        return Err(ScopeMoveError::NotFound(id.to_string()));
    }
    let playbooks = dst_parent.join("playbooks");
    let _lock = lock_dir(&playbooks, &format!("{id}.lock"))?;
    let dst = playbooks.join(id);
    match std::fs::create_dir(&dst) {
        Ok(()) => {}
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {
            return Err(ScopeMoveError::IdTaken(id.to_string()));
        }
        Err(e) => return Err(e.into()),
    }
    // The staging area stays: removing it could race another copy that is
    // about to create its own directory in it. Listings never see it, as it
    // sits beside `playbooks/`.
    let staging = dst_parent
        .join(STAGING_DIR)
        .join(format!("{id}-{}", crate::clock::now_nanos()));
    let staged = stage(src_playbook_dir, &staging).and_then(|()| {
        #[cfg(windows)]
        std::fs::remove_dir(&dst)?;
        std::fs::rename(&staging, &dst)
    });
    if let Err(e) = staged {
        let _ = std::fs::remove_dir_all(&staging);
        let _ = std::fs::remove_dir(&dst);
        return Err(e.into());
    }
    Ok(dst)
}

fn stage(src: &Path, staging: &Path) -> io::Result<()> {
    copy_tree(src, staging)?;
    for rel in LEFT_OUT {
        // A removal through a linked directory (`meta -> elsewhere`) would
        // land outside the copy.
        let path = staging.join(rel);
        ensure_no_symlink_below(staging, &path)?;
        match std::fs::remove_file(&path) {
            Err(e) if e.kind() != io::ErrorKind::NotFound => return Err(e),
            _ => {}
        }
    }
    Ok(())
}
