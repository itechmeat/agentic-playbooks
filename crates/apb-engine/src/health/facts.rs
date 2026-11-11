//! The facts the health rules read (routine health D2): one per top-level
//! production or external data run of the playbook, from its directory
//! or, once pruned, from its archived summary, so a removal in progress
//! never hides a run.

use std::path::Path;

use apb_core::health::{RunClass, RunFact};
use apb_core::scope::Scope;

use crate::error::EngineError;
use crate::event::{Event, EventPayload};

/// How a run's journal ended: the fold's status word, who wrote the
/// terminal abort, its full reason and its time.
pub(crate) struct Ended<'a> {
    /// `succeeded`, `failed`, `aborted`, or empty while not ended.
    pub outcome: &'static str,
    pub by: Option<crate::pause::kind::PauseBy>,
    pub reason: Option<&'a str>,
    pub ts: Option<u64>,
}

impl Ended<'_> {
    /// `operator`, `supervisor` or `engine`.
    pub fn by_word(&self) -> Option<&'static str> {
        use crate::pause::kind::PauseBy;
        self.by.map(|b| match b {
            PauseBy::Operator => "operator",
            PauseBy::Supervisor => "supervisor",
            PauseBy::Engine => "engine",
        })
    }

    /// The one classification ([`apb_core::health::class_of`]).
    pub fn class(&self) -> RunClass {
        apb_core::health::class_of(self.outcome, self.by_word(), self.reason)
    }
}

/// How `events` ended.
pub(crate) fn ended(events: &[Event]) -> Ended<'_> {
    use crate::state::RunStatus;
    let status = crate::state::RunState::fold(events).run_status;
    let outcome = match status {
        RunStatus::Succeeded => "succeeded",
        RunStatus::Failed => "failed",
        s if s.is_terminal() => "aborted",
        _ => "",
    };
    let terminal = events.iter().rev().find(|e| {
        matches!(
            e.payload,
            EventPayload::RunFinished { .. } | EventPayload::RunAborted { .. }
        )
    });
    let (by, reason) = match terminal.map(|e| &e.payload) {
        Some(EventPayload::RunAborted { reason, by }) => (*by, Some(reason.as_str())),
        _ => (None, None),
    };
    Ended {
        outcome,
        by,
        reason,
        ts: terminal
            .filter(|_| !outcome.is_empty())
            .map(|e| u64::try_from(e.ts).unwrap_or(u64::MAX)),
    }
}

/// Whether a run directory's manifest origin is one health counts.
fn operational(run_dir: &Path) -> bool {
    matches!(
        crate::manifest::RunOrigin::of_read(&crate::manifest::read(run_dir)),
        crate::manifest::RunOrigin::Production | crate::manifest::RunOrigin::ExternalData
    )
}

/// The fact of an archived run of `id` in `scope`; `None` for a child,
/// another origin or playbook, or a class word this binary does not know.
fn archived_fact(
    a: &crate::retention::archive::ArchivedRun,
    scope: Scope,
    id: &str,
) -> Option<RunFact> {
    let counted = matches!(a.origin.as_str(), "production" | "external_data")
        && a.parent.is_none()
        && a.playbook == id
        && a.scope == scope.as_str();
    counted.then_some(())?;
    Some(RunFact {
        run_id: a.run_id.clone(),
        started_ms: a.started_ms,
        ended_ms: a.ended_ms,
        class: RunClass::parse(&a.class)?,
    })
}

/// The facts of playbook `id` in `scope` under `root`: the listed run
/// directories that check valid, are production or external data and
/// top-level, then the archived runs of the same view. A view that does
/// not read is an error, never an empty history.
pub fn facts(root: &Path, scope: Scope, id: &str) -> Result<Vec<RunFact>, EngineError> {
    let view = crate::retention::archive::runs_view(root)?;
    let ours: std::collections::BTreeSet<String> =
        crate::run_history::operational_runs_of(&[root.to_path_buf()], id)
            .iter()
            .filter_map(|d| d.file_name().map(|n| n.to_string_lossy().into_owned()))
            .collect();
    let runs = root.join(".apb").join("runs");
    let mut out = Vec::new();
    for name in &view.dirs {
        let dir = runs.join(name);
        if !ours.contains(name) && !gone(&dir) {
            continue;
        }
        out.extend(listed_fact(root, scope, id, name, &dir)?);
    }
    out.extend(
        view.archived
            .iter()
            .filter_map(|a| archived_fact(a, scope, id)),
    );
    Ok(out)
}

fn gone(dir: &Path) -> bool {
    std::fs::symlink_metadata(dir).is_err()
}

/// How many times a listed run is read again when a fresh view lists it
/// once more after its directory was gone.
const LISTED_READS: usize = 3;

/// The fact of a run the view listed under `.apb/runs`. Every read goes
/// to a directory that still exists only if it exists after them: a run
/// moved away meanwhile may have read as a top-level run started at zero,
/// or not at all. Such a run is looked up in a fresh view: counted from
/// its summary when archived, read again from its directory when listed
/// again (a removal undone), skipped when in neither.
fn listed_fact(
    root: &Path,
    scope: Scope,
    id: &str,
    name: &str,
    dir: &Path,
) -> Result<Option<RunFact>, EngineError> {
    use crate::retention::archive::{AfterListing, after_listing};
    for _ in 0..LISTED_READS {
        let read = match crate::concurrency::checked_run(root, scope, id, name, None) {
            crate::concurrency::RunCheck::Valid(rd) => dir_fact(name, &rd),
            crate::concurrency::RunCheck::Void | crate::concurrency::RunCheck::Unreadable => None,
        };
        if !gone(dir) {
            return Ok(read);
        }
        #[cfg(any(test, feature = "testing"))]
        crate::deadline_hooks::at(crate::deadline_hooks::Hook::HealthFactGone, dir);
        match after_listing(root, name)? {
            AfterListing::Listed => {}
            AfterListing::Archived(a) => return Ok(archived_fact(&a, scope, id)),
            AfterListing::Absent => return Ok(None),
        }
    }
    Err(EngineError::Conflict(format!(
        "run `{name}` moved while its fact was read; try again"
    )))
}

/// The fact of a run directory that checked valid; `None` for a child or
/// a run of another origin.
fn dir_fact(name: &str, rd: &crate::concurrency::RunDir) -> Option<RunFact> {
    #[cfg(any(test, feature = "testing"))]
    crate::deadline_hooks::at(crate::deadline_hooks::Hook::HealthFactRead, &rd.path);
    let top = crate::run_config::read_run_config(&rd.path)
        .ok()
        .and_then(|c| c.parent_run)
        .is_none();
    if !top || !operational(&rd.path) {
        return None;
    }
    let started_ms = crate::run_history::started(&rd.path)
        .map(|h| u64::try_from(h.ts).unwrap_or(u64::MAX))
        .unwrap_or(0);
    let end = ended(&rd.events);
    Some(RunFact {
        run_id: name.to_string(),
        started_ms,
        ended_ms: end.ts,
        class: end.class(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use apb_core::health::RunClass;
    use std::path::PathBuf;

    fn seed(
        root: &Path,
        id: &str,
        playbook: &str,
        origin: &str,
        ts: u64,
        end: serde_json::Value,
    ) -> PathBuf {
        let dir = root.join(".apb/runs").join(id);
        std::fs::create_dir_all(&dir).unwrap();
        apb_core::run_origin::ensure_key().unwrap();
        apb_core::run_origin::stamp(&dir, id).unwrap();
        let mut lines = vec![
            serde_json::json!({"seq": 1, "ts": ts, "type": "run_started", "playbook": playbook, "version": "1.0.0"}),
            serde_json::json!({"seq": 2, "ts": ts, "type": "run_provenance", "origin": origin}),
        ];
        if !end.is_null() {
            let mut end = end;
            end["seq"] = serde_json::json!(3);
            end["ts"] = serde_json::json!(ts + 10);
            lines.push(end);
        }
        let text: String = lines.iter().map(|l| format!("{l}\n")).collect();
        std::fs::write(dir.join("events.jsonl"), text).unwrap();
        dir
    }

    fn failed() -> serde_json::Value {
        serde_json::json!({"type": "run_finished", "outcome": "failed"})
    }

    fn ids(f: &[RunFact]) -> Vec<&str> {
        let mut v: Vec<&str> = f.iter().map(|f| f.run_id.as_str()).collect();
        v.sort();
        v
    }

    fn archive(root: &Path, id: &str, class: &str, parent: Option<&str>, origin: &str) {
        crate::retention::archive::write_summary(
            root,
            &crate::retention::archive::ArchivedFamily {
                format: 1,
                root: id.into(),
                runs: vec![crate::retention::archive::ArchivedRun {
                    run_id: id.into(),
                    playbook: "p".into(),
                    scope: "project".into(),
                    version: "1.0.0".into(),
                    origin: origin.into(),
                    parent: parent.map(str::to_string),
                    started_ms: 5,
                    ended_ms: Some(6),
                    outcome: "aborted".into(),
                    class: class.into(),
                    aborted_by: Some("engine".into()),
                    abort_code: Some("superseded".into()),
                    active_ms: None,
                    tokens: None,
                    cost_usd: None,
                    execution: None,
                }],
                mac: String::new(),
            },
        )
        .unwrap();
    }

    #[test]
    fn facts_read_directories_and_summaries_once() {
        let t = tempfile::tempdir().unwrap();
        seed(t.path(), "p-1", "p", "project", 1, failed());
        archive(t.path(), "p-1", "succeeded", None, "production");
        archive(t.path(), "p-2", "failed", None, "production");
        let f = facts(t.path(), Scope::Project, "p").unwrap();
        assert_eq!(ids(&f), ["p-1", "p-2"]);
        let p1 = f.iter().find(|f| f.run_id == "p-1").unwrap();
        assert_eq!(
            p1.class,
            RunClass::Failed,
            "the directory counts, not the summary"
        );
    }

    #[test]
    fn facts_skip_children_and_other_scopes() {
        let t = tempfile::tempdir().unwrap();
        seed(t.path(), "p-1", "p", "project", 1, failed());
        let child = seed(t.path(), "p-2", "p", "project", 2, failed());
        crate::run_config::write_run_config(
            &child,
            &crate::run_config::RunConfig {
                parent_run: Some("q-1".into()),
                ..Default::default()
            },
        )
        .unwrap();
        seed(t.path(), "p-3", "p", "global", 3, failed());
        seed(t.path(), "o-1", "other", "project", 4, failed());
        archive(t.path(), "p-4", "failed", Some("q-1"), "production");
        archive(t.path(), "p-5", "failed", None, "eval");
        assert_eq!(ids(&facts(t.path(), Scope::Project, "p").unwrap()), ["p-1"]);
        assert_eq!(ids(&facts(t.path(), Scope::Global, "p").unwrap()), ["p-3"]);
    }

    #[test]
    fn facts_classify_a_deadline_abort_as_failed() {
        let t = tempfile::tempdir().unwrap();
        seed(
            t.path(),
            "p-1",
            "p",
            "project",
            1,
            serde_json::json!({"type": "run_aborted", "reason": "deadline_exceeded: 2h passed", "by": "engine"}),
        );
        seed(
            t.path(),
            "p-2",
            "p",
            "project",
            2,
            serde_json::json!({"type": "run_aborted", "reason": "stop requested", "by": "operator"}),
        );
        let f = facts(t.path(), Scope::Project, "p").unwrap();
        let class = |id: &str| f.iter().find(|f| f.run_id == id).unwrap().class;
        assert_eq!(class("p-1"), RunClass::Failed);
        assert_eq!(class("p-2"), RunClass::Ignored);
        assert_eq!(
            f.iter().find(|f| f.run_id == "p-1").unwrap().ended_ms,
            Some(11)
        );
    }

    #[test]
    fn facts_skip_an_unreadable_journal() {
        let t = tempfile::tempdir().unwrap();
        let d = seed(t.path(), "p-1", "p", "project", 1, failed());
        let mut f = std::fs::OpenOptions::new()
            .append(true)
            .open(d.join("events.jsonl"))
            .unwrap();
        std::io::Write::write_all(&mut f, b"{\"seq\": 9, \"ty").unwrap();
        assert!(facts(t.path(), Scope::Project, "p").unwrap().is_empty());
    }

    #[test]
    fn facts_skip_a_run_without_provenance() {
        let t = tempfile::tempdir().unwrap();
        let dir = t.path().join(".apb/runs/p-1");
        std::fs::create_dir_all(&dir).unwrap();
        apb_core::run_origin::ensure_key().unwrap();
        apb_core::run_origin::stamp(&dir, "p-1").unwrap();
        std::fs::write(
            dir.join("events.jsonl"),
            format!(
                "{}\n{}\n",
                serde_json::json!({"seq": 1, "ts": 1, "type": "run_started", "playbook": "p", "version": "1.0.0"}),
                serde_json::json!({"seq": 2, "ts": 2, "type": "run_finished", "outcome": "failed"})
            ),
        )
        .unwrap();
        assert!(facts(t.path(), Scope::Project, "p").unwrap().is_empty());
    }

    /// A run moved away by a prune after its journal was read counts from
    /// its summary, never as a top-level run started at zero.
    #[test]
    fn a_run_pruned_while_its_fact_is_read_counts_from_its_summary() {
        let t = tempfile::tempdir().unwrap();
        seed(t.path(), "p-1", "p", "project", 1_000, failed());
        let root = t.path().to_path_buf();
        let moved = std::sync::atomic::AtomicBool::new(false);
        crate::deadline_hooks::set(
            crate::deadline_hooks::Hook::HealthFactRead,
            t.path(),
            move |d| {
                if moved.swap(true, std::sync::atomic::Ordering::SeqCst) {
                    return;
                }
                // The prune: the summary first, then the directory leaves.
                archive(&root, "p-1", "failed", None, "production");
                std::fs::rename(d, root.join("moved-p-1")).unwrap();
            },
        );
        let f = facts(t.path(), Scope::Project, "p").unwrap();
        crate::deadline_hooks::clear(t.path());
        assert_eq!(ids(&f), ["p-1"]);
        assert_eq!(f[0].started_ms, 5, "the start comes from the summary");
    }

    /// A run moved away and moved back (the recovery of an interrupted
    /// removal) while its fact was looked up is read again from its
    /// directory: no summary is left for it, and it is never skipped.
    #[test]
    fn a_run_moved_back_while_its_fact_is_looked_up_is_read_again() {
        let t = tempfile::tempdir().unwrap();
        seed(t.path(), "p-1", "p", "project", 1_000, failed());
        let root = t.path().to_path_buf();
        let staged = root.join("staged-p-1");
        let moved = std::sync::atomic::AtomicBool::new(false);
        crate::deadline_hooks::set(crate::deadline_hooks::Hook::HealthFactRead, t.path(), {
            let (root, staged) = (root.clone(), staged.clone());
            move |d| {
                if moved.swap(true, std::sync::atomic::Ordering::SeqCst) {
                    return;
                }
                archive(&root, "p-1", "failed", None, "production");
                std::fs::rename(d, &staged).unwrap();
            }
        });
        let back = std::sync::atomic::AtomicBool::new(false);
        crate::deadline_hooks::set(crate::deadline_hooks::Hook::HealthFactGone, t.path(), {
            let root = root.clone();
            move |d| {
                if back.swap(true, std::sync::atomic::Ordering::SeqCst) {
                    return;
                }
                // The removal is undone: the directory back, its summary gone.
                std::fs::rename(&staged, d).unwrap();
                std::fs::remove_file(root.join(".apb/runs-archive/p-1.json")).unwrap();
            }
        });
        let f = facts(t.path(), Scope::Project, "p");
        crate::deadline_hooks::clear(t.path());
        let f = f.unwrap();
        assert_eq!(ids(&f), ["p-1"]);
        assert_eq!(f[0].started_ms, 1_000, "read from its directory");
        assert_eq!(f[0].class, RunClass::Failed);
    }

    /// A run gone while its fact is read whose fresh view does not read is
    /// an error, never a run left out.
    #[test]
    fn a_fresh_view_that_does_not_read_is_an_error() {
        let t = tempfile::tempdir().unwrap();
        seed(t.path(), "p-1", "p", "project", 1_000, failed());
        let root = t.path().to_path_buf();
        let elsewhere = tempfile::tempdir().unwrap();
        let target = elsewhere.path().to_path_buf();
        let moved = std::sync::atomic::AtomicBool::new(false);
        crate::deadline_hooks::set(
            crate::deadline_hooks::Hook::HealthFactRead,
            t.path(),
            move |d| {
                if moved.swap(true, std::sync::atomic::Ordering::SeqCst) {
                    return;
                }
                std::fs::rename(d, root.join("moved-p-1")).unwrap();
                std::os::unix::fs::symlink(&target, root.join(".apb/runs-archive")).unwrap();
            },
        );
        let f = facts(t.path(), Scope::Project, "p");
        crate::deadline_hooks::clear(t.path());
        assert!(f.is_err(), "{f:?}");
    }

    /// A combined view that does not read is an error, not no runs.
    #[test]
    fn a_view_that_does_not_read_is_an_error() {
        let t = tempfile::tempdir().unwrap();
        seed(t.path(), "p-1", "p", "project", 1, failed());
        let elsewhere = tempfile::tempdir().unwrap();
        std::os::unix::fs::symlink(elsewhere.path(), t.path().join(".apb/runs-archive")).unwrap();
        assert!(facts(t.path(), Scope::Project, "p").is_err());
    }

    #[test]
    fn an_archived_class_is_read_not_derived() {
        let t = tempfile::tempdir().unwrap();
        // An engine abort whose code says superseded but whose class says
        // failed: the class is what counts.
        archive(t.path(), "p-1", "failed", None, "production");
        archive(t.path(), "p-2", "ignored", None, "external_data");
        archive(t.path(), "p-3", "weird", None, "production");
        let f = facts(t.path(), Scope::Project, "p").unwrap();
        assert_eq!(ids(&f), ["p-1", "p-2"]);
        assert_eq!(
            f.iter().find(|f| f.run_id == "p-1").unwrap().class,
            RunClass::Failed
        );
    }
}
