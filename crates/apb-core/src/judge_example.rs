//! Where a judge node's labelled examples live and how their files are
//! named, listed and read: `.apb/playbooks/<id>/evals/judge/<node>/<example>.yaml`,
//! outside versions and runs (like scenarios), committed with the project.
//!
//! The example file's format is the engine's
//! (`apb_engine::judge_eval::example::JudgeExample`, it holds the engine's
//! inputs and answers); this module owns the location, the names and the
//! filesystem rules every reader and writer shares. The project tree is
//! untrusted: a directory below the playbook that is a link, or a file that
//! is not a plain one, is never followed, read or written through.

use std::io::{self, Read};
use std::path::{Path, PathBuf};

use crate::registry::is_safe_segment;

/// Below a playbook's directory: `<node>/<example>.yaml` live here.
pub const EXAMPLES_DIR: &str = "evals/judge";

/// The extension of an example file.
pub const EXTENSION: &str = "yaml";

/// The largest example file read; a bigger one is refused, never read in
/// part.
pub const MAX_FILE_BYTES: u64 = 512 * 1024;

/// The prefix of the ids [`next_id`] hands out.
const ID_PREFIX: &str = "ex-";

/// Whether `id` names an example file stem: `[a-z0-9][a-z0-9-]*`, at most
/// 64 characters.
pub fn is_example_id(id: &str) -> bool {
    id.len() <= 64
        && id
            .chars()
            .next()
            .is_some_and(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
        && id
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

/// The directory of node `node`'s examples below the playbook directory
/// `playbook_dir`, path arithmetic only; `None` for a node id that is not a
/// safe path segment.
pub fn node_dir(playbook_dir: &Path, node: &str) -> Option<PathBuf> {
    is_safe_segment(node).then(|| playbook_dir.join(EXAMPLES_DIR).join(node))
}

/// Refuses a link (or anything but a directory) at any existing step from
/// `playbook_dir` down to `dir`: a repository could commit `evals` as a link
/// that sends a write, or a read, out of the project.
pub fn check_no_link(playbook_dir: &Path, dir: &Path) -> io::Result<()> {
    let rel = dir
        .strip_prefix(playbook_dir)
        .map_err(|_| io::Error::other("the examples directory is outside the playbook"))?;
    let mut cur = playbook_dir.to_path_buf();
    for part in rel.components() {
        cur.push(part);
        match std::fs::symlink_metadata(&cur) {
            Ok(m) if m.file_type().is_dir() => {}
            Ok(_) => {
                return Err(io::Error::other(format!(
                    "`{}` is not a plain directory; examples are never read or written through it",
                    cur.display()
                )));
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(()),
            Err(e) => return Err(e),
        }
    }
    Ok(())
}

/// The example ids in `dir`, sorted: the stems of its plain `.yaml` files
/// whose stem is an example id. A missing directory has none; a link at the
/// directory or above it (from `playbook_dir`) is refused.
pub fn example_ids(playbook_dir: &Path, dir: &Path) -> io::Result<Vec<String>> {
    check_no_link(playbook_dir, dir)?;
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e),
    };
    let mut ids: Vec<String> = entries
        .flatten()
        .filter(|e| e.file_type().is_ok_and(|t| t.is_file()))
        .filter_map(|e| {
            let name = e.file_name().into_string().ok()?;
            let stem = name.strip_suffix(&format!(".{EXTENSION}"))?;
            is_example_id(stem).then(|| stem.to_string())
        })
        .collect();
    ids.sort();
    Ok(ids)
}

/// The nodes below `playbook_dir` that have an examples directory, sorted.
pub fn example_nodes(playbook_dir: &Path) -> io::Result<Vec<String>> {
    let root = playbook_dir.join(EXAMPLES_DIR);
    check_no_link(playbook_dir, &root)?;
    let entries = match std::fs::read_dir(&root) {
        Ok(e) => e,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e),
    };
    let mut nodes: Vec<String> = entries
        .flatten()
        .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
        .filter_map(|e| e.file_name().into_string().ok())
        .filter(|n| is_safe_segment(n))
        .collect();
    nodes.sort();
    Ok(nodes)
}

/// The path of example `id` in `dir`.
pub fn example_path(dir: &Path, id: &str) -> PathBuf {
    dir.join(format!("{id}.{EXTENSION}"))
}

/// Reads example `id` of `dir` as text: never through a link, only a plain
/// file, at most [`MAX_FILE_BYTES`].
pub fn read_example(playbook_dir: &Path, dir: &Path, id: &str) -> io::Result<String> {
    check_no_link(playbook_dir, dir)?;
    let path = example_path(dir, id);
    let meta = std::fs::symlink_metadata(&path)?;
    if !meta.file_type().is_file() {
        return Err(io::Error::other(format!(
            "`{}` is not a plain file",
            path.display()
        )));
    }
    if meta.len() > MAX_FILE_BYTES {
        return Err(io::Error::other(format!(
            "`{}` is over {MAX_FILE_BYTES} bytes",
            path.display()
        )));
    }
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    let mut text = String::new();
    options
        .open(&path)?
        .take(MAX_FILE_BYTES + 1)
        .read_to_string(&mut text)?;
    Ok(text)
}

/// The next free id after `existing`: `ex-<n>` with `n` one past the
/// highest number taken, four digits at least.
pub fn next_id(existing: &[String]) -> String {
    let highest = existing
        .iter()
        .filter_map(|id| id.strip_prefix(ID_PREFIX)?.parse::<u32>().ok())
        .max()
        .unwrap_or(0);
    format!("{ID_PREFIX}{:04}", highest.saturating_add(1))
}

/// `YYYY-MM-DD` (UTC) of epoch milliseconds: the day an example was
/// labelled.
pub fn labelled_on(ms: u64) -> String {
    crate::clock::iso_utc(ms)[..10].to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Ids are file stems an author can type; a new one is one past the
    /// highest number, whatever other names sit next to it.
    #[test]
    fn ids_are_safe_stems_and_the_next_one_follows_the_highest() {
        for ok in ["ex-0001", "a", "0", "missing-detail-1"] {
            assert!(is_example_id(ok), "{ok}");
        }
        for bad in ["", "-x", "Ex-1", "ex_1", "../x", "ex.1", &"a".repeat(65)] {
            assert!(!is_example_id(bad), "{bad}");
        }
        assert_eq!(next_id(&[]), "ex-0001");
        let taken = ["ex-0002", "ex-0010", "by-hand", "ex-x"].map(String::from);
        assert_eq!(next_id(&taken), "ex-0011");
        assert_eq!(labelled_on(1_793_200_094_570), "2026-10-28");
    }

    /// A repository that commits the examples directory, or an example
    /// file, as a link gets nothing read or listed through it.
    #[cfg(unix)]
    #[test]
    fn a_planted_link_is_never_followed() {
        let tree = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let pb = tree.path().join("pb");
        std::fs::create_dir_all(pb.join("evals")).unwrap();
        std::fs::create_dir_all(outside.path().join("triage")).unwrap();
        std::fs::write(outside.path().join("triage/ex-0001.yaml"), "x").unwrap();
        std::os::unix::fs::symlink(outside.path(), pb.join("evals/judge")).unwrap();
        let dir = node_dir(&pb, "triage").unwrap();
        assert!(example_ids(&pb, &dir).is_err());
        assert!(example_nodes(&pb).is_err());
        assert!(read_example(&pb, &dir, "ex-0001").is_err());

        // A link as the file itself, inside a real directory.
        std::fs::remove_file(pb.join("evals/judge")).unwrap();
        std::fs::create_dir_all(&dir).unwrap();
        std::os::unix::fs::symlink(
            outside.path().join("triage/ex-0001.yaml"),
            dir.join("ex-0002.yaml"),
        )
        .unwrap();
        std::fs::write(dir.join("ex-0003.yaml"), "kept").unwrap();
        assert_eq!(example_ids(&pb, &dir).unwrap(), ["ex-0003"]);
        assert!(read_example(&pb, &dir, "ex-0002").is_err());
        assert_eq!(read_example(&pb, &dir, "ex-0003").unwrap(), "kept");
        assert!(node_dir(&pb, "../x").is_none());
    }
}
