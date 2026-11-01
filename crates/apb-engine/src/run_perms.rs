//! The file modes of a run directory.
//!
//! A run directory holds what the executor produced, an echoed secret
//! included, so only its owner may read it: apb creates it 0700 and every
//! file that can carry run content 0600. A run directory an older apb wrote
//! (0775 with 0664 files) is narrowed by [`narrow`] when apb opens the run
//! for driving or resuming. Narrowing clears the group and other bits only
//! (`mode & !0o077`): owner bits stay as they are, owner execute included,
//! so an executable helper an attempt left under `work/<node>/<attempt>/`
//! still runs after a resume. The walk skips every symbolic link it sees:
//! its target keeps its mode, wherever it points. The change itself goes by
//! path, so an entry someone swaps for a link between the look and the
//! change is changed through the link; narrowing only ever removes group
//! and other bits, and only a directory an older apb left writable to
//! others allows such a swap.
//!
//! A read-only surface never narrows; `apb doctor --run` reports a wide run
//! directory through [`wide`] and changes nothing.

use std::path::{Path, PathBuf};

/// The bits a run directory and its entries must not carry.
#[cfg(unix)]
const GROUP_OTHER: u32 = 0o077;

/// Clears the group and other bits of `run_dir` and of every entry under
/// it, symbolic links skipped and never followed. Best effort: an entry
/// that cannot be read or changed is left as it is. Does nothing off unix.
pub fn narrow(run_dir: &Path) {
    if crate::read_only::active() {
        return;
    }
    #[cfg(unix)]
    walk(run_dir, &mut |path, mode| {
        use std::os::unix::fs::PermissionsExt as _;
        if mode & GROUP_OTHER != 0 {
            let _ = std::fs::set_permissions(
                path,
                std::fs::Permissions::from_mode(mode & !GROUP_OTHER),
            );
        }
        true
    });
    #[cfg(not(unix))]
    let _ = run_dir;
}

/// The first entry of `run_dir` (the directory itself included) whose mode
/// carries group or other bits and that someone else can reach: below a
/// private directory nobody else can, so the walk does not go there (a
/// step's own files under `work/` keep the modes it gave them). Symbolic
/// links are skipped; `None` when nothing is reachable (and always off
/// unix). Reads only.
pub fn wide(run_dir: &Path) -> Option<PathBuf> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        let meta = std::fs::symlink_metadata(run_dir).ok()?;
        if meta.file_type().is_symlink() || meta.permissions().mode() & GROUP_OTHER == 0 {
            return None;
        }
        if !meta.is_dir() {
            return Some(run_dir.to_path_buf());
        }
        // The directory is open to others: it is the finding, unless only
        // its search bits are (then its entries are what others can read).
        if meta.permissions().mode() & 0o066 != 0 {
            return Some(run_dir.to_path_buf());
        }
        std::fs::read_dir(run_dir)
            .ok()?
            .flatten()
            .find_map(|e| wide(&e.path()))
    }
    #[cfg(not(unix))]
    {
        let _ = run_dir;
        None
    }
}

/// The shell line that narrows `run_dir` by hand, the path quoted as one
/// shell word.
pub fn fix_line(run_dir: &Path) -> String {
    format!(
        "chmod -R go-rwx {}",
        apb_core::publish::shell_word(&run_dir.to_string_lossy())
    )
}

/// Visits `dir` and every entry under it with its mode (from `lstat`),
/// depth first; a symbolic link is neither visited nor descended. `visit`
/// returns `false` to stop the walk. Returns whether the walk went on.
#[cfg(unix)]
fn walk(dir: &Path, visit: &mut dyn FnMut(&Path, u32) -> bool) -> bool {
    use std::os::unix::fs::PermissionsExt as _;
    let Ok(meta) = std::fs::symlink_metadata(dir) else {
        return true;
    };
    if meta.file_type().is_symlink() {
        return true;
    }
    if !visit(dir, meta.permissions().mode()) {
        return false;
    }
    if !meta.is_dir() {
        return true;
    }
    let Ok(entries) = std::fs::read_dir(dir) else {
        return true;
    };
    for entry in entries.flatten() {
        if !walk(&entry.path(), visit) {
            return false;
        }
    }
    true
}

// Narrowing a run end to end (owner execute kept, a linked file's target
// left alone) is `tests/suite/run_file_modes_test.rs`; these cover what a
// run cannot produce.
#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt as _;

    fn mode(p: &Path) -> u32 {
        std::fs::symlink_metadata(p).unwrap().permissions().mode() & 0o777
    }

    fn set(p: &Path, m: u32) {
        std::fs::set_permissions(p, std::fs::Permissions::from_mode(m)).unwrap();
    }

    #[test]
    fn a_linked_directory_keeps_its_mode() {
        let dir = tempfile::tempdir().unwrap();
        let outside_dir = dir.path().join("elsewhere");
        std::fs::create_dir(&outside_dir).unwrap();
        set(&outside_dir, 0o755);
        let run = dir.path().join("run-1");
        std::fs::create_dir(&run).unwrap();
        set(&run, 0o775);
        std::os::unix::fs::symlink(&outside_dir, run.join("dir-link")).unwrap();

        narrow(&run);

        assert_eq!(mode(&run), 0o700);
        assert_eq!(mode(&outside_dir), 0o755);
        assert_eq!(wide(&run), None);
    }

    #[test]
    fn wide_reads_only_and_stops_below_a_private_directory() {
        let dir = tempfile::tempdir().unwrap();
        let run = dir.path().join("run-1");
        std::fs::create_dir(&run).unwrap();
        let f = run.join("run.yaml");
        std::fs::write(&f, "params: {}\n").unwrap();
        set(&run, 0o700);
        set(&f, 0o664);
        assert_eq!(wide(&run), None, "nobody else reaches it");
        set(&run, 0o711);
        assert_eq!(wide(&run).as_deref(), Some(f.as_path()));
        assert_eq!(mode(&f), 0o664);
        set(&run, 0o775);
        assert_eq!(wide(&run).as_deref(), Some(run.as_path()));
    }

    #[test]
    fn the_fix_line_quotes_the_path() {
        let line = fix_line(Path::new("/tmp/My Projects/$x/run-1"));
        assert_eq!(line, "chmod -R go-rwx '/tmp/My Projects/$x/run-1'");
    }
}
