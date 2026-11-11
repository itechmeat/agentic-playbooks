//! The run archive (retention D3): the signed summary of a removed
//! family, its recoverable commit protocol, and the combined view every
//! reader counts from. A summary is about 400 bytes and never holds a
//! param value, an output or a reason's text.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::EngineError;

/// One run of a removed family; never a param value, an output or a
/// reason's text.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ArchivedRun {
    pub run_id: String,
    pub playbook: String,
    pub scope: String,
    pub version: String,
    /// `production`, `external_data` or `eval` (the manifest's origin).
    pub origin: String,
    /// The parent run of a child; absent for a top-level run.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent: Option<String>,
    pub started_ms: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ended_ms: Option<u64>,
    /// `succeeded`, `failed` or `aborted`.
    pub outcome: String,
    /// The health class of the run when it was removed: what
    /// [`apb_core::health::class_of`] answered over its live journal
    /// (`succeeded`, `failed` or `ignored`), and `ignored` for a run the
    /// live health facts never counted (no origin stamp of this
    /// installation, no `run_provenance`). Health reads this field and
    /// never re-derives a class from `abort_code`.
    pub class: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub aborted_by: Option<String>,
    /// `deadline_exceeded`, `superseded`, `operator_stop`,
    /// `parent_aborted` or `other`; for display only, never the reason's
    /// text.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub abort_code: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tokens: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cost_usd: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub execution: Option<String>,
}

/// One file per removed family: `.apb/runs-archive/<root-id>.json`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ArchivedFamily {
    /// 1.
    pub format: u32,
    /// The top-level run.
    pub root: String,
    /// The root first, then its descendants.
    pub runs: Vec<ArchivedRun>,
    /// `run_origin::tag(ARCHIVE_DOMAIN, <canonical JSON without mac>)`.
    pub mac: String,
}

/// The MAC domain of an archived family.
pub const ARCHIVE_DOMAIN: &str = "apb-run-archive-v1";
/// The lock every reader of the combined view and every archive writer
/// takes, a `DirLock` in `.apb/runs-archive`.
pub const ARCHIVE_LOCK: &str = "archive.lock";
/// The one-prune-at-a-time lock, a `HeldLock` in `.apb/runs-archive`.
pub const PRUNE_LOCK: &str = "prune.lock";

/// `<root>/.apb/runs-archive`.
pub fn archive_dir(root: &Path) -> PathBuf {
    root.join(".apb").join("runs-archive")
}

/// `<root>/.apb/runs`.
pub(crate) fn runs_dir(root: &Path) -> PathBuf {
    root.join(".apb").join("runs")
}

/// The archive directory when it exists as a real directory, `None` when
/// it is absent, and the `runs_archive_unsafe` refusal when it is a link
/// or not a directory.
pub(crate) fn archive_state(root: &Path) -> Result<Option<PathBuf>, EngineError> {
    let dir = archive_dir(root);
    match std::fs::symlink_metadata(&dir) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(EngineError::Io(e)),
        Ok(m) if m.is_dir() => {
            apb_core::fsutil::ensure_no_symlink_below(root, &dir).map_err(|_| unsafe_archive())?;
            Ok(Some(dir))
        }
        Ok(_) => Err(unsafe_archive()),
    }
}

/// The refusal of an archive that is a link or not a directory.
pub(crate) fn unsafe_archive() -> EngineError {
    EngineError::Invalid(
        "runs_archive_unsafe: .apb/runs-archive is a link; nothing was pruned".into(),
    )
}

/// The refusal of a recovery whose `.apb/runs` is a link or not a
/// directory.
fn unsafe_runs() -> EngineError {
    EngineError::Invalid(
        "runs_dir_unsafe: .apb/runs is a link or not a directory; nothing was recovered".into(),
    )
}

/// The canonical bytes a MAC covers: the value without `mac`, keys
/// sorted ([`apb_core::signed_store::bytes_of`]).
fn covered(family: &ArchivedFamily) -> Result<Vec<u8>, EngineError> {
    let mut value = serde_json::to_value(family)
        .map_err(|e| EngineError::Invalid(format!("archive summary: {e}")))?;
    if let Some(o) = value.as_object_mut() {
        o.remove("mac");
    }
    Ok(apb_core::signed_store::bytes_of(&value)?)
}

/// Writes `<root-id>.json` under the archive anchor, signed with the
/// installation key.
pub fn write_summary(root: &Path, family: &ArchivedFamily) -> Result<(), EngineError> {
    if !apb_core::registry::is_safe_segment(&family.root) {
        return Err(EngineError::Invalid(format!(
            "archive summary: `{}` is not a run id",
            family.root
        )));
    }
    let mut family = family.clone();
    family.mac = apb_core::run_origin::tag(ARCHIVE_DOMAIN, &covered(&family)?)?;
    let bytes = serde_json::to_vec_pretty(&family)
        .map_err(|e| EngineError::Invalid(format!("archive summary: {e}")))?;
    let dir = archive_dir(root);
    apb_core::fsutil::create_dir_under(root, &dir)?;
    archive_state(root)?;
    apb_core::fsutil::atomic_write_private_under(
        &dir,
        &dir.join(format!("{}.json", family.root)),
        &bytes,
    )?;
    Ok(())
}

/// The family at `path` when it parses and its MAC verifies.
pub(crate) fn verified(path: &Path) -> Option<ArchivedFamily> {
    let meta = std::fs::symlink_metadata(path).ok()?;
    if !meta.is_file() {
        return None;
    }
    let bytes = std::fs::read(path).ok()?;
    let family: ArchivedFamily = serde_json::from_slice(&bytes).ok()?;
    let ok = apb_core::run_origin::verify_tag(ARCHIVE_DOMAIN, &covered(&family).ok()?, &family.mac);
    let named = path
        .file_name()
        .and_then(|n| n.to_str())
        .is_some_and(|n| n == format!("{}.json", family.root));
    (ok && named).then_some(family)
}

/// The id a summary file name carries: `<id>.json` with a safe id, `None`
/// for staging, lock and every other name.
pub(crate) fn summary_id(name: &str) -> Option<&str> {
    name.strip_suffix(".json")
        .filter(|id| apb_core::registry::is_safe_segment(id) && !id.starts_with('.'))
}

/// The verified families, read under the archive lock by the caller.
/// Staging and lock names are skipped, and so is every file whose MAC
/// does not verify.
pub fn read_verified_locked(root: &Path) -> Vec<ArchivedFamily> {
    let Ok(Some(dir)) = archive_state(root) else {
        return Vec::new();
    };
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return Vec::new();
    };
    let mut out: Vec<ArchivedFamily> = entries
        .filter_map(Result::ok)
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().into_owned();
            summary_id(&name)?;
            verified(&e.path())
        })
        .collect();
    out.sort_by(|a, b| a.root.cmp(&b.root));
    out
}

/// The recovery of an interrupted removal, at the start of every `apply`
/// (the caller holds the prune lock), under the archive lock:
/// 1. `.removed-<id>`: a committed removal, deleted for good.
/// 2. `.removing-<id>`: a staged removal is undone: every member goes back
///    under `.apb/runs` (one whose name is taken there stays in place and
///    is reported), the empty staging directory is removed, then the
///    summary is deleted, so no reader misses a run.
/// 3. `<id>.json` some of whose runs still have a directory and no staging
///    directory (a crash between the summary and the move): deleted; the
///    directories count.
pub fn recover(root: &Path) -> Result<(), EngineError> {
    let Some(dir) = archive_state(root)? else {
        return Ok(());
    };
    let runs = runs_dir(root);
    let _lock = apb_core::fsutil::lock_dir(&dir, ARCHIVE_LOCK)?;
    // A member goes back only under a real `.apb/runs` inside the project:
    // a link there would move it wherever the link points.
    let runs_ready = |runs: &Path| -> Result<(), EngineError> {
        match std::fs::symlink_metadata(runs) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                apb_core::fsutil::create_dir_under(root, runs)?;
            }
            Err(e) => return Err(EngineError::Io(e)),
            Ok(m) if m.is_dir() => {}
            Ok(_) => return Err(unsafe_runs()),
        }
        apb_core::fsutil::ensure_no_symlink_below(root, runs).map_err(|_| unsafe_runs())?;
        if !std::fs::symlink_metadata(runs).is_ok_and(|m| m.is_dir()) {
            return Err(unsafe_runs());
        }
        Ok(())
    };
    let mut names: Vec<String> = std::fs::read_dir(&dir)?
        .filter_map(Result::ok)
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    let mut conflicts: Vec<String> = Vec::new();
    for name in &names {
        let path = dir.join(name);
        if !std::fs::symlink_metadata(&path).is_ok_and(|m| m.is_dir()) {
            continue;
        }
        if name.starts_with(".removed-") {
            std::fs::remove_dir_all(&path)?;
        } else if let Some(id) = name.strip_prefix(".removing-") {
            runs_ready(&runs)?;
            let mut left = false;
            for e in std::fs::read_dir(&path)?.filter_map(Result::ok) {
                let member = e.file_name().to_string_lossy().into_owned();
                let target = runs.join(&member);
                // Only a real run directory goes back: a file or a link
                // that appeared in staging was never a staged run.
                let a_run_dir = std::fs::symlink_metadata(e.path()).is_ok_and(|m| m.is_dir());
                if !apb_core::registry::is_safe_segment(&member)
                    || !a_run_dir
                    || std::fs::symlink_metadata(&target).is_ok()
                {
                    conflicts.push(format!("{name}/{member}"));
                    left = true;
                    continue;
                }
                std::fs::rename(e.path(), &target)?;
            }
            if !left {
                std::fs::remove_dir(&path)?;
                if apb_core::registry::is_safe_segment(id) {
                    match std::fs::remove_file(dir.join(format!("{id}.json"))) {
                        Err(e) if e.kind() != std::io::ErrorKind::NotFound => return Err(e.into()),
                        _ => {}
                    }
                }
            }
        }
    }
    for name in &names {
        let Some(id) = summary_id(name) else {
            continue;
        };
        if dir.join(format!(".removing-{id}")).exists() {
            continue;
        }
        let path = dir.join(name);
        let Some(family) = verified(&path) else {
            continue;
        };
        let present = family.runs.iter().any(|r| {
            apb_core::registry::is_safe_segment(&r.run_id)
                && std::fs::symlink_metadata(runs.join(&r.run_id)).is_ok_and(|m| m.is_dir())
        });
        if present {
            std::fs::remove_file(&path)?;
        }
    }
    if conflicts.is_empty() {
        Ok(())
    } else {
        Err(EngineError::Invalid(format!(
            "runs_archive_conflict: a staged run could not go back because its name is taken in .apb/runs; left in place: {}",
            conflicts.join(", ")
        )))
    }
}

/// The combined view: the run ids under `.apb/runs` and the verified
/// archived runs, both listed under one hold of the archive lock.
#[derive(Debug, Clone, Default)]
pub struct RunsView {
    pub dirs: Vec<String>,
    pub archived: Vec<ArchivedRun>,
}

/// The names under `.apb/runs`, nothing opened.
fn list_dirs(root: &Path) -> Vec<String> {
    let mut dirs: Vec<String> = std::fs::read_dir(runs_dir(root))
        .map(|rd| {
            rd.filter_map(Result::ok)
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .collect()
        })
        .unwrap_or_default();
    dirs.sort();
    dirs
}

/// How long a reader waits for a removal that holds the archive lock.
const READER_WAIT: std::time::Duration = std::time::Duration::from_secs(10);

/// The archive lock for a reader: a removal holds it only for its summary
/// and its renames, so a reader waits for it rather than fail.
fn lock_archive(dir: &Path) -> Result<apb_core::fsutil::DirLock, EngineError> {
    let started = std::time::Instant::now();
    loop {
        match apb_core::fsutil::lock_dir(dir, ARCHIVE_LOCK) {
            Ok(lock) => return Ok(lock),
            Err(e)
                if e.kind() == std::io::ErrorKind::WouldBlock
                    && started.elapsed() < READER_WAIT => {}
            Err(e) => return Err(e.into()),
        }
    }
}

/// Where a fresh view finds a run an earlier view listed under
/// `.apb/runs` whose directory was gone when it was read.
#[derive(Debug, Clone, PartialEq)]
pub enum AfterListing {
    /// Listed under `.apb/runs` again (a removal undone): read it again.
    Listed,
    /// Archived: counted from its summary.
    Archived(Box<ArchivedRun>),
    /// In neither: no longer a run.
    Absent,
}

/// A run listed by an earlier view whose directory is gone when it is
/// read, looked up once more in a fresh view. A view that does not read
/// is an error, never an absent run.
pub fn after_listing(root: &Path, run_id: &str) -> Result<AfterListing, EngineError> {
    let view = runs_view(root)?;
    if view.dirs.iter().any(|d| d == run_id) {
        return Ok(AfterListing::Listed);
    }
    Ok(view
        .archived
        .into_iter()
        .find(|a| a.run_id == run_id)
        .map_or(AfterListing::Absent, |a| {
            AfterListing::Archived(Box::new(a))
        }))
}

/// The combined view under one hold of the archive lock. A run is counted
/// once: an archived run whose id is also listed under `.apb/runs` is
/// dropped (it is counted from its directory). With no archive directory
/// there is nothing to lock and nothing archived; the listing is taken
/// again when one appeared meanwhile. A read-only process, which may not
/// write the lock file, reads without it.
pub fn runs_view(root: &Path) -> Result<RunsView, EngineError> {
    let Some(dir) = archive_state(root)? else {
        let dirs = list_dirs(root);
        if archive_state(root)?.is_none() {
            return Ok(RunsView {
                dirs,
                archived: Vec::new(),
            });
        }
        return runs_view(root);
    };
    let _lock = if apb_core::process_mode::is_read_only() {
        None
    } else {
        Some(lock_archive(&dir)?)
    };
    let dirs = list_dirs(root);
    let mut archived: Vec<ArchivedRun> = read_verified_locked(root)
        .into_iter()
        .flat_map(|f| f.runs)
        .collect();
    archived.retain(|a| dirs.binary_search(&a.run_id).is_err());
    Ok(RunsView { dirs, archived })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(id: &str) -> ArchivedRun {
        ArchivedRun {
            run_id: id.into(),
            playbook: "p".into(),
            scope: "project".into(),
            version: "1.0.0".into(),
            origin: "production".into(),
            parent: None,
            started_ms: 1,
            ended_ms: Some(2),
            outcome: "succeeded".into(),
            class: "succeeded".into(),
            aborted_by: None,
            abort_code: None,
            active_ms: Some(1),
            tokens: Some(10),
            cost_usd: Some(0.1),
            execution: Some("cli".into()),
        }
    }

    fn family(id: &str) -> ArchivedFamily {
        ArchivedFamily {
            format: 1,
            root: id.into(),
            runs: vec![run(id)],
            mac: String::new(),
        }
    }

    fn project() -> tempfile::TempDir {
        apb_core::run_origin::ensure_key().unwrap();
        tempfile::tempdir().unwrap()
    }

    #[test]
    fn a_family_round_trips_and_verifies() {
        let tmp = project();
        write_summary(tmp.path(), &family("p-1")).unwrap();
        let read = read_verified_locked(tmp.path());
        assert_eq!(read.len(), 1);
        assert_eq!(read[0].root, "p-1");
        assert_eq!(read[0].runs, vec![run("p-1")]);
        assert!(!read[0].mac.is_empty());
    }

    #[test]
    fn a_changed_byte_fails_verification() {
        let tmp = project();
        write_summary(tmp.path(), &family("p-1")).unwrap();
        let path = tmp.path().join(".apb/runs-archive/p-1.json");
        let text = std::fs::read_to_string(&path)
            .unwrap()
            .replace("\"tokens\": 10", "\"tokens\": 90");
        std::fs::write(&path, text).unwrap();
        assert!(read_verified_locked(tmp.path()).is_empty());
    }

    #[test]
    fn read_verified_skips_staging_and_lock_names() {
        let tmp = project();
        write_summary(tmp.path(), &family("p-1")).unwrap();
        let dir = tmp.path().join(".apb/runs-archive");
        let good = std::fs::read(dir.join("p-1.json")).unwrap();
        std::fs::write(dir.join(ARCHIVE_LOCK), b"").unwrap();
        std::fs::write(dir.join(PRUNE_LOCK), b"").unwrap();
        std::fs::write(dir.join(".p-2.json"), &good).unwrap();
        std::fs::write(dir.join("p-1.json.tmp"), &good).unwrap();
        std::fs::create_dir(dir.join(".removing-p-3")).unwrap();
        std::fs::create_dir(dir.join(".removed-p-4")).unwrap();
        let read = read_verified_locked(tmp.path());
        assert_eq!(read.len(), 1, "{read:?}");
        assert_eq!(read[0].root, "p-1");
    }

    #[test]
    fn runs_view_drops_archived_runs_that_have_a_directory() {
        let tmp = project();
        write_summary(tmp.path(), &family("p-1")).unwrap();
        write_summary(tmp.path(), &family("p-2")).unwrap();
        std::fs::create_dir_all(tmp.path().join(".apb/runs/p-1")).unwrap();
        let view = runs_view(tmp.path()).unwrap();
        assert_eq!(view.dirs, vec!["p-1".to_string()]);
        let ids: Vec<&str> = view.archived.iter().map(|a| a.run_id.as_str()).collect();
        assert_eq!(ids, ["p-2"]);
        // A directory gone after the listing is found in a fresh view.
        assert_eq!(
            after_listing(tmp.path(), "p-2").unwrap(),
            AfterListing::Archived(Box::new(run("p-2")))
        );
        assert_eq!(
            after_listing(tmp.path(), "p-1").unwrap(),
            AfterListing::Listed
        );
        assert_eq!(
            after_listing(tmp.path(), "p-9").unwrap(),
            AfterListing::Absent
        );
    }
}
