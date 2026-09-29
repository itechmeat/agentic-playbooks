//! Protected paths on an `agent_task` (C6).
//!
//! A post-hoc check that works for every agent: before each attempt the
//! engine copies the files matching the node's `protect` globs (relative to
//! the node's working directory; paths git ignores are not covered) into the
//! run directory and records their digests. After the attempt's process has
//! ended it lists the matches again. Any file modified, deleted or added is
//! restored from the copy (an added one is removed), one
//! `protected_paths_modified` event names the changes, and a reported
//! success is rejected with `protected path modified: <path>`, which
//! consumes a normal retry and keeps the report as `rejected_output`. The
//! next attempt therefore starts from the protected files as they were, with
//! no worktree required.
//!
//! The check cannot stop the write while the attempt runs, only undo it and
//! reject the attempt afterwards.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use super::journal::Journal;
use crate::error::EngineError;
use crate::event::{EventPayload, ProtectedChange};

/// The protected files as they were before one attempt.
pub(crate) struct Snapshot {
    node: String,
    attempt: u32,
    root: PathBuf,
    globs: Vec<String>,
    /// Where the copies live: `<run>/protect/<node>/<attempt>/`.
    store: PathBuf,
    digests: BTreeMap<String, String>,
}

fn listing_error(e: apb_core::fingerprint::FingerprintError) -> EngineError {
    EngineError::Invalid(format!("protect: {e}"))
}

impl Snapshot {
    /// Copies the files `globs` match under `root`; `None` without globs.
    pub(crate) fn take(
        run_dir: &Path,
        node: &str,
        attempt: u32,
        root: &Path,
        globs: &[String],
    ) -> Result<Option<Self>, EngineError> {
        if globs.is_empty() {
            return Ok(None);
        }
        let files = apb_core::fingerprint::matching_files(root, globs).map_err(listing_error)?;
        let digests = apb_core::fingerprint::file_digests(root, &files).map_err(listing_error)?;
        let store = run_dir.join("protect").join(node).join(attempt.to_string());
        match std::fs::remove_dir_all(&store) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e.into()),
        }
        std::fs::create_dir_all(&store)?;
        for rel in &files {
            let dst = store.join(rel);
            if let Some(parent) = dst.parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::copy(root.join(rel), &dst)?;
        }
        Ok(Some(Self {
            node: node.to_string(),
            attempt,
            root: root.to_path_buf(),
            globs: globs.to_vec(),
            store,
            digests,
        }))
    }

    /// The changes since [`Snapshot::take`], in path order.
    fn changes(&self) -> Result<Vec<ProtectedChange>, EngineError> {
        let now = apb_core::fingerprint::matching_files(&self.root, &self.globs)
            .map_err(listing_error)?;
        let after = apb_core::fingerprint::file_digests(&self.root, &now).map_err(listing_error)?;
        let mut out = Vec::new();
        for (path, digest) in &self.digests {
            match after.get(path) {
                None => out.push(ProtectedChange {
                    path: path.clone(),
                    change: "deleted".into(),
                }),
                Some(d) if d != digest => out.push(ProtectedChange {
                    path: path.clone(),
                    change: "modified".into(),
                }),
                Some(_) => {}
            }
        }
        for path in after.keys().filter(|p| !self.digests.contains_key(*p)) {
            out.push(ProtectedChange {
                path: path.clone(),
                change: "added".into(),
            });
        }
        out.sort_by(|a, b| a.path.cmp(&b.path));
        Ok(out)
    }

    /// Puts one changed file back; `false` when that failed.
    fn restore(&self, c: &ProtectedChange) -> bool {
        let target = self.root.join(&c.path);
        if c.change == "added" {
            return std::fs::remove_file(&target).is_ok();
        }
        if let Some(parent) = target.parent()
            && std::fs::create_dir_all(parent).is_err()
        {
            return false;
        }
        // A symlink or directory the attempt put in the file's place goes
        // first, so the copy lands as a plain file.
        if std::fs::symlink_metadata(&target).is_ok_and(|m| !m.is_file()) {
            let _ = std::fs::remove_dir_all(&target).or_else(|_| std::fs::remove_file(&target));
        }
        std::fs::copy(self.store.join(&c.path), &target).is_ok()
    }

    /// Compares, restores what changed, journals the changes, and returns
    /// the rejection reason when anything changed. The copies are removed
    /// either way.
    pub(crate) fn check_and_restore(
        self,
        journal: &Journal,
    ) -> Result<Option<String>, EngineError> {
        let changes = self.changes();
        let result = match changes {
            Ok(changes) if changes.is_empty() => Ok(None),
            Ok(changes) => {
                let restore_failed: Vec<String> = changes
                    .iter()
                    .filter(|c| !self.restore(c))
                    .map(|c| c.path.clone())
                    .collect();
                let first = &changes[0].path;
                let reason = match changes.len() {
                    1 => format!("protected path modified: {first}"),
                    n => format!("protected path modified: {first} (and {} more)", n - 1),
                };
                journal.append(EventPayload::ProtectedPathsModified {
                    node: self.node.clone(),
                    attempt: self.attempt,
                    changes,
                    restore_failed,
                })?;
                Ok(Some(reason))
            }
            Err(e) => Err(e),
        };
        let _ = std::fs::remove_dir_all(&self.store);
        result
    }
}
