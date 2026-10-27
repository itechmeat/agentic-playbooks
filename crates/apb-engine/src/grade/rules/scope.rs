//! `scope`: changes outside the task.
//!
//! - a protected path changed (`protected_paths_modified`): `poor`;
//! - a file written outside the attempt's working directory, the run's
//!   working tree, the run directory and the temp directories
//!   ([`TEMP_ROOTS`]): `partial`.
//!
//! A path is resolved before it is compared: `..` is folded lexically and a
//! relative path is joined to the attempt's working directory, so
//! `/work/../etc/passwd` is outside and `src/../lib/x.rs` inside.

use std::path::{Component, Path, PathBuf};

use apb_core::quality::grade::Level;

use super::finding::{Finding, j, nothing_seen, session_records};
use crate::event::EventPayload;
use crate::grade::sources::RunSources;

/// The temp directories a write may go to. A fixed list, not the grading
/// process's own temp directory, so a run grades the same in any process.
pub const TEMP_ROOTS: [&str; 4] = ["/tmp", "/var/tmp", "/private/tmp", "/private/var/folders"];

/// Where a write is inside the task.
fn allowed_roots(sources: &RunSources) -> Vec<PathBuf> {
    let mut roots = vec![sources.run_dir.clone()];
    roots.extend(TEMP_ROOTS.iter().map(PathBuf::from));
    if cfg!(windows) {
        roots.push(std::env::temp_dir());
    }
    roots.extend(sources.run.events.iter().filter_map(|e| match &e.payload {
        EventPayload::WorktreeResolved { path, .. } => Some(PathBuf::from(path)),
        _ => None,
    }));
    roots.into_iter().map(|r| normalize(&r)).collect()
}

/// `path` with `.` dropped and `..` folded into its parent, lexically: a
/// `..` above a root is dropped, and one above a relative start is kept.
fn normalize(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for c in path.components() {
        match c {
            Component::ParentDir => match out.components().next_back() {
                Some(Component::Normal(_)) => {
                    out.pop();
                }
                Some(Component::RootDir | Component::Prefix(_)) => {}
                _ => out.push(".."),
            },
            Component::CurDir => {}
            other => out.push(other),
        }
    }
    out
}

fn inside(path: &str, workdir: Option<&str>, roots: &[PathBuf]) -> bool {
    let p = Path::new(path);
    let full = match (p.is_relative(), workdir) {
        (true, Some(w)) => normalize(&Path::new(w).join(p)),
        (true, None) => return !normalize(p).starts_with(".."),
        (false, _) => normalize(p),
    };
    workdir
        .map(|w| normalize(Path::new(w)))
        .iter()
        .chain(roots)
        .any(|root| full.starts_with(root))
}

pub(super) fn grade(sources: &RunSources) -> Finding {
    let protected: Vec<String> = sources
        .run
        .events
        .iter()
        .filter(|e| matches!(e.payload, EventPayload::ProtectedPathsModified { .. }))
        .map(|e| j(e.seq))
        .collect();
    if !protected.is_empty() {
        return Finding::new(
            Level::Poor,
            "protected_path_changed",
            "a protected path was changed",
        )
        .citing(protected);
    }
    let roots = allowed_roots(sources);
    let outside: Vec<String> = session_records(sources)
        .filter(|(view, rec)| {
            rec.tools.iter().any(|t| {
                t.writes
                    && t.paths
                        .iter()
                        .any(|p| !inside(p, view.source.workdir.as_deref(), &roots))
            })
        })
        .map(|(view, rec)| view.record_ref(rec.index))
        .collect();
    if outside.is_empty() {
        return nothing_seen(sources, "nothing was changed outside the task");
    }
    Finding::new(
        Level::Partial,
        "outside_scope",
        format!("{} write(s) outside the working directory", outside.len()),
    )
    .citing(outside)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_path_is_compared_after_its_dots_are_folded() {
        let roots = vec![PathBuf::from("/tmp")];
        assert!(!inside("/work/../etc/passwd", Some("/work"), &roots));
        assert!(!inside("/tmp/../root/.ssh/keys", Some("/work"), &roots));
        assert!(inside("src/../lib/x.rs", Some("/work"), &roots));
        assert!(!inside("../../etc/x", Some("/work"), &roots));
        assert!(inside("/work/./a/b", Some("/work"), &roots));
        assert!(inside("/tmp/scratch/x", Some("/work"), &roots));
        assert!(!inside("../x", None, &roots));
        assert!(!inside("../../x", None, &roots));
        assert!(!inside("a/../../../x", None, &roots));
        assert!(inside("a/b/../c", None, &roots));
        assert_eq!(normalize(Path::new("/../etc")), PathBuf::from("/etc"));
    }
}
