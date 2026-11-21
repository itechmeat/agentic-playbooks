//! The run's `scripts/` copy (script nodes, goal scripts and baseline
//! scripts run from it) and the digest pin that keeps an earlier node from
//! rewriting a script that later decides the run's goal or baseline.

use std::path::Path;

use crate::error::EngineError;
use crate::event::EventPayload;

/// The content digest of `run_dir/scripts` (the copy script nodes and goal
/// scripts run from), `none` when there is no such directory or it is
/// empty. Pinned in `run_provenance` at start.
pub(crate) fn digest(run_dir: &Path) -> Result<String, EngineError> {
    digest_or_why(run_dir)
        .map_err(|e| EngineError::Invalid(format!("run scripts cannot be digested: {e}")))
}

/// The read-only scripts copy of one drive of a run that may skip: taken
/// at the drive's entry and given back when the value drops, whatever ends
/// the drive (a finish, an abort, a pause, a park, an engine pause, an
/// error return, a panic that unwinds). Taking it clears the write bits of
/// every file and directory under the copy, the directory itself included
/// (links are left as they are and never followed), and records the mode
/// of every entry it walked; dropping it sets exactly those modes again, so
/// a `0555` script is `0555` and a `0664` file `0664` after the drive. A
/// script that writes next to itself fails on its own write and an agent
/// edit of a check script is refused instead of passing between the two
/// ends an item check sees. The modes do not stop root or a process that
/// changes them first; the digest checks at both ends of every item stay.
/// Only a process that is killed leaves the copy read-only, for the run's
/// end ([`unprotect`]) and the removers' [`writable_tree`]. A no-op off
/// unix and for a run without the directory.
pub(crate) struct Protected(Vec<(std::path::PathBuf, u32)>);

impl Protected {
    /// Makes the copy read-only for as long as the value lives.
    pub(crate) fn take(run_dir: &Path) -> std::io::Result<Self> {
        let mut changed = Vec::new();
        let taken = clear_write(&run_dir.join("scripts"), &mut changed);
        let protected = Self(changed);
        // A walk that failed half way gives back what it changed (the drop).
        taken.map(|()| protected)
    }
}

impl Drop for Protected {
    fn drop(&mut self) {
        // Directories were recorded before their entries: given back in
        // reverse, every entry first, its directory after.
        for (path, mode) in self.0.iter().rev() {
            if let Err(e) = set_mode(path, *mode) {
                eprintln!(
                    "apb: could not give {} of the run's scripts copy its mode back: {e}",
                    path.display()
                );
            }
        }
    }
}

/// Gives a scripts copy left read-only (by a drive whose process was
/// killed) its owner write bits back, at the run's end. A copy whose
/// directory has its owner write bit was never left protected and keeps
/// its modes. The removers use the blunter [`writable_tree`].
pub(crate) fn unprotect(run_dir: &Path) -> std::io::Result<()> {
    let scripts = run_dir.join("scripts");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        match std::fs::symlink_metadata(&scripts) {
            Ok(m) if m.is_dir() && m.permissions().mode() & 0o200 == 0 => {}
            _ => return Ok(()),
        }
    }
    set_writable(&scripts, true)
}

/// Clears the write bits of `path` and everything under it, recording the
/// mode of every entry it walks in `changed` (a directory before its
/// entries). A directory is read before it is made read-only.
fn clear_write(path: &Path, changed: &mut Vec<(std::path::PathBuf, u32)>) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let meta = match std::fs::symlink_metadata(path) {
            Ok(m) => m,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(e) => return Err(e),
        };
        if meta.file_type().is_symlink() {
            return Ok(());
        }
        let mode = meta.permissions().mode() & 0o7777;
        let next = mode & !0o222;
        let entries: Vec<std::path::PathBuf> = if meta.is_dir() {
            std::fs::read_dir(path)?
                .flatten()
                .map(|e| e.path())
                .collect()
        } else {
            Vec::new()
        };
        // Every entry is recorded, changed or not: the run's end may give a
        // read-only copy owner write while the drive still holds it.
        changed.push((path.to_path_buf(), mode));
        for e in &entries {
            clear_write(e, changed)?;
        }
        if next != mode {
            set_mode(path, next)?;
        }
    }
    #[cfg(not(unix))]
    let _ = (path, changed);
    Ok(())
}

/// Sets the permission bits of `path` to `mode`; a no-op off unix.
fn set_mode(path: &Path, mode: u32) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode))?;
    }
    #[cfg(not(unix))]
    let _ = (path, mode);
    Ok(())
}

/// Gives every directory under `path` (itself included) its owner write
/// bit, so a removal of the whole tree can unlink what a protected scripts
/// copy holds. Links are not followed.
pub(crate) fn writable_tree(path: &Path) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let Ok(meta) = std::fs::symlink_metadata(path) else {
            return Ok(());
        };
        if !meta.is_dir() {
            return Ok(());
        }
        let mode = meta.permissions().mode();
        if mode & 0o200 == 0 {
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode | 0o200))?;
        }
        for e in std::fs::read_dir(path)?.flatten() {
            writable_tree(&e.path())?;
        }
    }
    #[cfg(not(unix))]
    let _ = path;
    Ok(())
}

/// Sets or clears the write bits of `path` and everything under it: a
/// directory is opened before it is made read-only and after it is made
/// writable, so the walk never needs a write it took away.
fn set_writable(path: &Path, writable: bool) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let meta = match std::fs::symlink_metadata(path) {
            Ok(m) => m,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(e) => return Err(e),
        };
        if meta.file_type().is_symlink() {
            return Ok(());
        }
        let mode = meta.permissions().mode();
        let next = if writable {
            mode | 0o200
        } else {
            mode & !0o222
        };
        let set = || std::fs::set_permissions(path, std::fs::Permissions::from_mode(next));
        if meta.is_dir() {
            if writable {
                set()?;
            }
            for e in std::fs::read_dir(path)?.flatten() {
                set_writable(&e.path(), writable)?;
            }
            if !writable {
                set()?;
            }
        } else if next != mode {
            set()?;
        }
    }
    #[cfg(not(unix))]
    let _ = (path, writable);
    Ok(())
}

/// [`digest`], with the bare reason it failed.
fn digest_or_why(run_dir: &Path) -> Result<String, String> {
    let scripts = run_dir.join("scripts");
    let empty = match std::fs::read_dir(&scripts) {
        Ok(mut d) => d.next().is_none(),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => true,
        Err(e) => return Err(e.to_string()),
    };
    if empty {
        return Ok("none".into());
    }
    apb_core::content::tree_digest(&scripts, &apb_core::content::TreeLimits::default())
        .map_err(|e| e.to_string())
}

/// Why the run's `scripts/` copy cannot be trusted for `what` (goal or
/// baseline scripts), or
/// `None` when it still matches what the run pinned at start. Any earlier
/// node could rewrite the copy (its path is in `APB_RUN_DIR`), which would
/// let it decide an enforced goal or a baseline. The pin is taken when the
/// playbook has goal or baseline scripts; a run journaled before the pin
/// existed is checked against its whole definition digest instead.
pub(crate) fn tampered(
    run_dir: &Path,
    events: &[crate::event::Event],
    what: &str,
) -> Option<String> {
    let (definition, pinned) = events.iter().find_map(|e| match &e.payload {
        EventPayload::RunProvenance {
            digest,
            scripts_digest,
            ..
        } => Some((digest.clone(), scripts_digest.clone())),
        _ => None,
    })?;
    let changed =
        format!("the run's scripts/ copy changed after the run started, so {what} do not run");
    let undigestable = |e: String| {
        format!("the run's scripts/ copy cannot be digested, so {what} do not run: {e}")
    };
    match (pinned, definition) {
        (Some(pinned), _) => match digest_or_why(run_dir) {
            Ok(now) if now == pinned => None,
            Ok(_) => Some(changed),
            Err(e) => Some(undigestable(e)),
        },
        (None, Some(definition)) => {
            let yaml = match std::fs::read_to_string(run_dir.join("playbook.yaml")) {
                Ok(y) => y,
                Err(e) => {
                    return Some(format!(
                        "the run's playbook snapshot cannot be read, so {what} do not run: {e}"
                    ));
                }
            };
            match apb_core::scope::definition_digest(&yaml, run_dir) {
                Ok(now) if now == definition => None,
                Ok(_) => Some(changed),
                Err(e) => Some(undigestable(e.to_string())),
            }
        }
        (None, None) => None,
    }
}
