//! Copying a playbook definition into another scope's store, whole: every
//! version, the `current` pointer, layouts, provenance and lifecycle. The
//! copy is assembled in a staging directory beside `playbooks/` (so a
//! listing never sees half a playbook) and renamed into place in one step.
//! Content digests do not depend on where a definition lives, so the copy
//! keeps the trust of the source.
//!
//! The legacy instruction draft (`meta/instruction-draft.md`) is left out:
//! run input belongs to each project's memory ([`crate::run_memory`]), not
//! to a definition shared by every project. So are the transient files of
//! the source: its lock files and a move's intent.
//!
//! The source is read under its pointer lock, after an interrupted move is
//! recovered, so `current`, the candidate pointer, the trial records and
//! the history come from one moment. Locks are always taken source first,
//! then the destination's namespace lock. The copy's history gets one
//! `scope_move` line before it is published.
//!
//! The copy carries its launch floor: the destination's key (passed by the
//! caller, never derived from a path) is raised to the stricter of the
//! source key's floor and every copied version's declaration before the
//! final rename, under the floor lock (see [`crate::launch::floor`]).

use std::io;
use std::path::{Path, PathBuf};

use crate::fsutil::{copy_tree, ensure_no_symlink_below, lock_dir};
use crate::launch::floor::{self, FloorKey, SaveAuthority};
use crate::registry::is_safe_segment;
use crate::versioning::current::{self, MoveCurrent, MovedBy};

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
    Launch(#[from] floor::FloorError),
    #[error(transparent)]
    Io(#[from] io::Error),
}

impl From<current::MoveError> for ScopeMoveError {
    fn from(e: current::MoveError) -> Self {
        ScopeMoveError::Io(e.into_io())
    }
}

/// The staging area beside `<dst_parent>/playbooks`.
const STAGING_DIR: &str = "playbooks-staging";
/// What the copy leaves out, relative to the playbook directory.
const LEFT_OUT: &[&str] = &["meta/instruction-draft.md"];

/// Copies the playbook directory `src_playbook_dir` (of the scope named
/// `from_scope`) to `<dst_parent>/playbooks/<id>` and returns the new
/// directory. Holds the source's pointer lock, then the lock
/// `<dst_parent>/playbooks/<id>.lock`, while it works; refuses
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
    from_scope: &str,
    floors: &CopyFloors,
) -> Result<PathBuf, ScopeMoveError> {
    if !is_safe_segment(id) {
        return Err(ScopeMoveError::InvalidId(id.to_string()));
    }
    if !src_playbook_dir.is_dir() {
        return Err(ScopeMoveError::NotFound(id.to_string()));
    }
    let src_lock = current::lock(src_playbook_dir)?;
    current::recover(src_playbook_dir, &src_lock)?;
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
    let staged = stage(src_playbook_dir, &staging, from_scope)
        .map_err(ScopeMoveError::from)
        .and_then(|()| {
            let carried = floor::floor_of(Some(&floors.from), Some(src_playbook_dir))?
                .max(floor::scan(&staging));
            floor::publish(&floors.to, &dst, carried, || {
                #[cfg(windows)]
                std::fs::remove_dir(&dst)?;
                std::fs::rename(&staging, &dst)
            })?
            .map_err(ScopeMoveError::from)
        });
    if let Err(e) = staged {
        let _ = std::fs::remove_dir_all(&staging);
        let _ = std::fs::remove_dir(&dst);
        return Err(e);
    }
    Ok(dst)
}

/// The launch floor keys of a copy: the source's, read, and the
/// destination's with the channel the copy comes through.
#[derive(Debug, Clone)]
pub struct CopyFloors {
    pub from: FloorKey,
    pub to: SaveAuthority,
}

impl CopyFloors {
    /// A person's copy of project playbook `id` of workspace `ws` to the
    /// global store.
    pub fn project_to_global(workspace_id: &str, id: &str, channel: floor::SaveChannel) -> Self {
        CopyFloors {
            from: FloorKey::Project {
                workspace_id: workspace_id.to_string(),
                id: id.to_string(),
            },
            to: SaveAuthority {
                key: FloorKey::global(id),
                channel,
            },
        }
    }
}

fn stage(src: &Path, staging: &Path, from_scope: &str) -> io::Result<()> {
    copy_tree(src, staging)?;
    let transient = std::fs::read_dir(staging)?
        .filter_map(Result::ok)
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|name| name.ends_with(".lock") || name == current::INTENT_FILE);
    let left_out: Vec<String> = LEFT_OUT
        .iter()
        .map(|rel| rel.to_string())
        .chain(transient)
        .collect();
    for rel in &left_out {
        // A removal through a linked directory (`meta -> elsewhere`) would
        // land outside the copy.
        let path = staging.join(rel);
        ensure_no_symlink_below(staging, &path)?;
        match std::fs::remove_file(&path) {
            Err(e) if e.kind() != io::ErrorKind::NotFound => return Err(e),
            _ => {}
        }
    }
    record_arrival(staging, from_scope)
}

/// The copy's history line: `current` stays where the source had it, and
/// the move says where it came from. A source without `current`, or whose
/// `current` names no runnable version, gets none: the copy keeps it as it
/// was. A line that does not land refuses the copy, so a published copy
/// never starts with a pending move.
fn record_arrival(staging: &Path, from_scope: &str) -> io::Result<()> {
    let Some(at) = crate::versioning::read_current_pointer(staging)
        .filter(|v| staging.join(v).join("playbook.yaml").is_file())
    else {
        return Ok(());
    };
    let held = current::lock(staging)?;
    let mut req = MoveCurrent::new(
        &at,
        MovedBy::ScopeMove {
            from_scope: from_scope.to_string(),
        },
    );
    req.expected_from = Some(Some(&at));
    let moved =
        current::move_current_locked(staging, &req, &held).map_err(current::MoveError::into_io)?;
    if moved.audit_pending {
        return Err(io::Error::other(
            "the copy's history line did not land; nothing was copied",
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::time::Duration;

    use super::*;
    use crate::versioning::current::{
        HISTORY_FILE, INTENT_FILE, MoveCurrent, MovedBy, Surface, history, move_current,
    };

    const YAML: &str = "schema: 2\nid: p\nname: p\nversion: 1.0.0\nnodes:\n  - { id: s, type: start }\nedges: []\n";

    fn seed(parent: &Path) -> PathBuf {
        let dir = parent.join("playbooks/p");
        for v in ["1.0.0", "1.1.0"] {
            fs::create_dir_all(dir.join(v)).unwrap();
            fs::write(dir.join(v).join("playbook.yaml"), YAML).unwrap();
        }
        fs::write(dir.join("current"), "1.0.0").unwrap();
        dir
    }

    fn person_to(to: &str) -> MoveCurrent<'_> {
        let mut req = MoveCurrent::new(
            to,
            MovedBy::Person {
                surface: Surface::Cli,
            },
        );
        req.expected_from = Some(Some("1.0.0"));
        req
    }

    #[test]
    fn a_move_waits_for_a_copy_and_the_copy_reads_one_moment() {
        let _cfg = crate::test_config();
        let src = tempfile::tempdir().unwrap();
        let dst = tempfile::tempdir().unwrap();
        let dir = seed(src.path());
        let (paused_tx, paused_rx) = std::sync::mpsc::channel();
        let pause = Duration::from_millis(400);
        std::thread::scope(|s| {
            let copier = s.spawn(|| {
                let mut first = Some(paused_tx);
                // Pauses between two source-file reads, once.
                crate::fsutil::copy_seam::set(Some(Box::new(move || {
                    if let Some(tx) = first.take() {
                        tx.send(()).unwrap();
                        std::thread::sleep(pause);
                    }
                })));
                let out = copy_playbook_to(
                    &dir,
                    dst.path(),
                    "p",
                    "project",
                    &CopyFloors::project_to_global("ws", "p", floor::SaveChannel::Person),
                );
                crate::fsutil::copy_seam::set(None);
                out
            });
            paused_rx.recv().unwrap();
            let moved = move_current(&dir, &person_to("1.1.0")).unwrap();
            assert_eq!(moved.to, "1.1.0");
            copier.join().unwrap().unwrap();
        });
        let copied = dst.path().join("playbooks/p");
        let h = history(&copied, 10).unwrap();
        assert!(!h.incomplete && !h.pending, "{h:?}");
        assert_eq!(
            fs::read_to_string(copied.join("current")).unwrap().trim(),
            "1.0.0"
        );
        // The copy holds no trace of the move made meanwhile.
        assert!(h.entries.iter().all(|e| e.to.as_deref() != Some("1.1.0")));
        let last = &h.entries[0];
        assert_eq!(last.to.as_deref(), Some("1.0.0"));
        assert_eq!(
            last.by,
            Some(MovedBy::ScopeMove {
                from_scope: "project".into()
            })
        );
        // The source moved after the copy, with its own record.
        let h = history(&dir, 10).unwrap();
        assert!(!h.incomplete);
        assert_eq!(h.entries[0].to.as_deref(), Some("1.1.0"));
    }

    #[test]
    fn the_copy_recovers_the_source_and_leaves_transient_files_out() {
        let _cfg = crate::test_config();
        let src = tempfile::tempdir().unwrap();
        let dst = tempfile::tempdir().unwrap();
        let dir = seed(src.path());
        move_current(&dir, &person_to("1.1.0")).unwrap();
        // An interrupted move and a stray lock in the source.
        fs::write(dir.join("other.lock"), "x").unwrap();
        let mut back = person_to("1.0.0");
        back.expected_from = Some(Some("1.1.0"));
        use crate::versioning::current::fault::{Step, fail_after};
        fail_after(Some(Step::Intent));
        move_current(&dir, &back).unwrap_err();
        fail_after(None);
        assert!(dir.join(INTENT_FILE).exists());

        let copied = copy_playbook_to(
            &dir,
            dst.path(),
            "p",
            "project",
            &CopyFloors::project_to_global("ws", "p", floor::SaveChannel::Person),
        )
        .unwrap();
        assert!(!dir.join(INTENT_FILE).exists(), "the source was recovered");
        for name in [INTENT_FILE, "other.lock", ".candidate.lock"] {
            assert!(!copied.join(name).exists(), "{name} was copied");
        }
        assert!(copied.join(HISTORY_FILE).is_file());
        let h = history(&copied, 10).unwrap();
        assert!(!h.incomplete, "{h:?}");
        assert_eq!(h.entries.len(), 3, "{h:?}");
    }

    #[test]
    fn a_copy_whose_history_line_does_not_land_is_not_published() {
        let _cfg = crate::test_config();
        let src = tempfile::tempdir().unwrap();
        let dst = tempfile::tempdir().unwrap();
        let dir = seed(src.path());
        use crate::versioning::current::fault::{Step, fail_after};
        fail_after(Some(Step::History));
        let out = copy_playbook_to(
            &dir,
            dst.path(),
            "p",
            "project",
            &CopyFloors::project_to_global("ws", "p", floor::SaveChannel::Person),
        );
        fail_after(None);
        assert!(matches!(out, Err(ScopeMoveError::Io(_))), "{out:?}");
        assert!(!dst.path().join("playbooks/p").exists());
        // A retry copies it whole.
        let copied = copy_playbook_to(
            &dir,
            dst.path(),
            "p",
            "project",
            &CopyFloors::project_to_global("ws", "p", floor::SaveChannel::Person),
        )
        .unwrap();
        assert!(!copied.join(INTENT_FILE).exists());
        assert!(!history(&copied, 10).unwrap().pending);
    }

    #[test]
    fn a_source_whose_current_names_no_runnable_version_still_copies() {
        let _cfg = crate::test_config();
        let src = tempfile::tempdir().unwrap();
        let dst = tempfile::tempdir().unwrap();
        let dir = seed(src.path());
        fs::create_dir_all(dir.join("2.0.0")).unwrap();
        fs::write(dir.join("current"), "2.0.0").unwrap();
        let copied = copy_playbook_to(
            &dir,
            dst.path(),
            "p",
            "project",
            &CopyFloors::project_to_global("ws", "p", floor::SaveChannel::Person),
        )
        .unwrap();
        assert_eq!(fs::read_to_string(copied.join("current")).unwrap(), "2.0.0");
    }

    #[test]
    fn an_intent_that_does_not_read_refuses_the_copy() {
        let _cfg = crate::test_config();
        let src = tempfile::tempdir().unwrap();
        let dst = tempfile::tempdir().unwrap();
        let dir = seed(src.path());
        fs::write(dir.join(INTENT_FILE), "{not json").unwrap();
        let out = copy_playbook_to(
            &dir,
            dst.path(),
            "p",
            "project",
            &CopyFloors::project_to_global("ws", "p", floor::SaveChannel::Person),
        );
        assert!(matches!(out, Err(ScopeMoveError::Io(_))), "{out:?}");
        assert!(!dst.path().join("playbooks/p").exists());
    }
}
