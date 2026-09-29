//! Protected paths on an `agent_task` (C6).
//!
//! A post-hoc check that works for every agent: before each attempt the
//! engine copies the files matching the node's `protect` globs (relative to
//! the node's working directory; paths git ignores when the attempt starts
//! and paths below a symlinked directory are not covered) into the run
//! directory and records their digests. After the attempt's process has
//! ended it lists the matches again. Any file modified, deleted or added is
//! restored from the copy (an added one is removed), one
//! `protected_paths_modified` event names the changes, and a reported
//! success is rejected with `protected path modified: <path>`, which
//! consumes a normal retry and keeps the report as `rejected_output`. The
//! next attempt therefore starts from the protected files as they were, with
//! no worktree required.
//!
//! The restore writes nothing through a symlinked directory, replaces a
//! file by renaming a fresh copy over it (never writing into an inode the
//! attempt may have hardlinked elsewhere), and writes a copy back only when
//! it still matches its recorded digest: a tampered copy fails the node at
//! once. The copies are kept when a path could not be restored.
//!
//! The check cannot stop the write while the attempt runs, only undo it and
//! reject the attempt afterwards.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use apb_core::fingerprint::has_symlinked_parent;

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
    /// What git ignored when the snapshot was taken (files, and wholly
    /// ignored directories ending in `/`). Such paths were never protected,
    /// so a later listing that shows them (the attempt edited `.gitignore`
    /// or removed `.git`) never reads them as added.
    ignored: Vec<String>,
}

/// Why a protected attempt is rejected.
pub(crate) struct Violation {
    /// The attempt's failure message.
    pub reason: String,
    /// A stored copy no longer matched its recorded digest: the node fails
    /// at once instead of retrying from a tree that cannot be trusted.
    pub fatal: bool,
}

/// What restoring one change did.
#[derive(PartialEq)]
enum Restored {
    Yes,
    Failed,
    /// The stored copy was changed after the snapshot; nothing was written.
    Tampered,
}

fn listing_error(e: apb_core::fingerprint::FingerprintError) -> EngineError {
    EngineError::Invalid(format!("protect: {e}"))
}

/// Makes every directory between `root` and `rel` a real directory: a
/// missing one is created, a symlink the attempt put in a directory's place
/// is removed (the link, never its target) and replaced by a directory.
/// `false` when a component is anything else (a plain file), so the caller
/// writes nothing through it.
fn real_parents(root: &Path, rel: &str) -> bool {
    let mut cur = root.to_path_buf();
    let mut parts: Vec<&str> = rel.split('/').filter(|s| !s.is_empty()).collect();
    parts.pop();
    for part in parts {
        cur.push(part);
        match std::fs::symlink_metadata(&cur) {
            Ok(m) if m.is_dir() => continue,
            Ok(m) if m.file_type().is_symlink() => {
                if std::fs::remove_file(&cur).is_err() {
                    return false;
                }
            }
            Ok(_) => return false,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(_) => return false,
        }
        match std::fs::create_dir(&cur) {
            Ok(()) => {}
            Err(_) => return std::fs::symlink_metadata(&cur).is_ok_and(|m| m.is_dir()),
        }
    }
    true
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
        let ignored = apb_core::fingerprint::ignored_paths(root);
        let files = apb_core::fingerprint::matching_files(root, globs).map_err(listing_error)?;
        let digests = apb_core::fingerprint::file_digests(root, &files).map_err(listing_error)?;
        let store = run_dir.join("protect").join(node).join(attempt.to_string());
        match std::fs::remove_dir_all(&store) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e.into()),
        }
        std::fs::create_dir_all(&store)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&store, std::fs::Permissions::from_mode(0o700))?;
        }
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
            ignored,
        }))
    }

    /// Whether `path` was ignored by git when the snapshot was taken.
    fn was_ignored(&self, path: &str) -> bool {
        self.ignored
            .iter()
            .any(|i| i == path || (i.ends_with('/') && path.starts_with(i.as_str())))
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
        for path in after
            .keys()
            .filter(|p| !self.digests.contains_key(*p) && !self.was_ignored(p))
        {
            out.push(ProtectedChange {
                path: path.clone(),
                change: "added".into(),
            });
        }
        out.sort_by(|a, b| a.path.cmp(&b.path));
        Ok(out)
    }

    /// Puts one changed file back. Nothing is written or removed through a
    /// symlinked directory, and a copy is only written back after it is
    /// checked against the digest recorded when it was taken.
    fn restore(&self, c: &ProtectedChange) -> Restored {
        let target = self.root.join(&c.path);
        if c.change == "added" {
            if has_symlinked_parent(&self.root, &c.path) {
                return Restored::Failed;
            }
            return match std::fs::symlink_metadata(&target) {
                Ok(m) if m.is_file() => {
                    if std::fs::remove_file(&target).is_ok() {
                        Restored::Yes
                    } else {
                        Restored::Failed
                    }
                }
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => Restored::Yes,
                _ => Restored::Failed,
            };
        }
        let stored = self.store.join(&c.path);
        let Some(expected) = self.digests.get(&c.path) else {
            return Restored::Failed;
        };
        let (bytes, perms) = match std::fs::symlink_metadata(&stored) {
            Ok(m) if m.is_file() => match std::fs::read(&stored) {
                Ok(b) => (b, m.permissions()),
                Err(_) => return Restored::Failed,
            },
            _ => return Restored::Tampered,
        };
        if apb_core::content::sha256_hex(&bytes) != *expected {
            return Restored::Tampered;
        }
        if !real_parents(&self.root, &c.path) {
            return Restored::Failed;
        }
        // A directory the attempt put in the file's place goes first (the
        // rename below replaces a symlink or any other entry as the entry
        // itself, never its target).
        if std::fs::symlink_metadata(&target).is_ok_and(|m| m.is_dir())
            && std::fs::remove_dir_all(&target).is_err()
        {
            return Restored::Failed;
        }
        // Written beside the target and renamed over it: the target's
        // directory entry is replaced, never the inode it names (a hardlink
        // the attempt made to a file elsewhere keeps that file intact), and a
        // crash leaves either the old file or the whole copy.
        let Some(name) = target.file_name() else {
            return Restored::Failed;
        };
        let tmp = target.with_file_name(format!(
            ".{}.apb-restore-{}",
            name.to_string_lossy(),
            std::process::id()
        ));
        let written = (|| -> std::io::Result<()> {
            use std::io::Write;
            let mut f = std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&tmp)?;
            f.write_all(&bytes)?;
            f.sync_all()?;
            drop(f);
            std::fs::set_permissions(&tmp, perms)?;
            std::fs::rename(&tmp, &target)
        })();
        if written.is_err() {
            let _ = std::fs::remove_file(&tmp);
            return Restored::Failed;
        }
        Restored::Yes
    }

    /// Compares, restores what changed, journals the changes, and returns
    /// the rejection when anything changed. The copies are removed unless a
    /// path could not be restored: then they stay, and the event and the
    /// reason name where.
    pub(crate) fn check_and_restore(
        self,
        journal: &Journal,
    ) -> Result<Option<Violation>, EngineError> {
        let changes = match self.changes() {
            Ok(changes) => changes,
            Err(e) => {
                let _ = std::fs::remove_dir_all(&self.store);
                return Err(e);
            }
        };
        if changes.is_empty() {
            let _ = std::fs::remove_dir_all(&self.store);
            return Ok(None);
        }
        let mut restore_failed = Vec::new();
        let mut tampered = Vec::new();
        for c in &changes {
            match self.restore(c) {
                Restored::Yes => {}
                Restored::Failed => restore_failed.push(c.path.clone()),
                Restored::Tampered => {
                    restore_failed.push(c.path.clone());
                    tampered.push(c.path.clone());
                }
            }
        }
        let kept = (!restore_failed.is_empty()).then(|| self.store.to_string_lossy().into_owned());
        if kept.is_none() {
            let _ = std::fs::remove_dir_all(&self.store);
        }
        let first = &changes[0].path;
        let mut reason = match (tampered.first(), changes.len()) {
            (Some(t), _) => {
                format!("protected path snapshot was tampered with: {t}; the path was not restored")
            }
            (None, 1) => format!("protected path modified: {first}"),
            (None, n) => format!("protected path modified: {first} (and {} more)", n - 1),
        };
        if let Some(k) = &kept {
            reason.push_str(&format!(
                "; not restored: {} (the snapshot copies are kept in {k})",
                restore_failed.join(", ")
            ));
        }
        journal.append(EventPayload::ProtectedPathsModified {
            node: self.node.clone(),
            attempt: self.attempt,
            changes,
            restore_failed,
            kept_copies: kept,
        })?;
        Ok(Some(Violation {
            reason,
            fatal: !tampered.is_empty(),
        }))
    }
}
