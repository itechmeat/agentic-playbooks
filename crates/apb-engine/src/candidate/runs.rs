//! The runs a project candidate is judged from. A candidate lives in a
//! project (`apb_core::candidate::playbook_dir`), so a run of a global
//! definition never counts for one; the one predicate the quality
//! comparison and the candidate summary share.

use std::path::{Path, PathBuf};

/// The production runs of the project playbook `id` (of `version` when
/// given) created on this machine, newest first: `run_history::all_runs_of`
/// without the runs of a global definition of the same id, and without
/// the runs whose journal does not read (their scope is unknown, so they
/// are evidence for neither side).
pub(super) fn project_runs_of(root: &Path, id: &str, version: Option<&str>) -> Vec<PathBuf> {
    crate::run_history::all_runs_of(std::slice::from_ref(&root.to_path_buf()), id, version)
        .into_iter()
        .filter(|d| is_project_run(d))
        .collect()
}

/// Whether the run at `run_dir` executes a project definition, read from a
/// journal that reads whole: the only runs a project candidate counts.
pub(super) fn is_project_run(run_dir: &Path) -> bool {
    crate::run_origin::run_scope(run_dir) == crate::run_origin::RunScope::Project
}

/// Seeded run directories for the candidate layer's unit tests.
#[cfg(test)]
pub(crate) mod testing {
    use std::path::{Path, PathBuf};

    /// A run directory of playbook `dup` version 1.0.0, created on this
    /// machine, whose `run_provenance` carries `origin` and `digest`. With
    /// `torn`, the journal ends in a half-written line after the provenance.
    pub(crate) fn seeded_run(
        root: &Path,
        name: &str,
        origin: Option<&str>,
        digest: &str,
        torn: bool,
    ) -> PathBuf {
        let dir = root.join(".apb/runs").join(name);
        std::fs::create_dir_all(&dir).unwrap();
        let started = r#"{"seq":1,"ts":1,"type":"run_started","playbook":"dup","version":"1.0.0"}"#;
        let prov = match origin {
            Some(o) => format!(
                r#"{{"seq":2,"ts":2,"type":"run_provenance","origin":"{o}","digest":"{digest}"}}"#
            ),
            None => format!(r#"{{"seq":2,"ts":2,"type":"run_provenance","digest":"{digest}"}}"#),
        };
        let tail = if torn {
            r#"{"seq":3,"ts":3,"type":"no"#
        } else {
            ""
        };
        std::fs::write(
            dir.join("events.jsonl"),
            format!("{started}\n{prov}\n{tail}"),
        )
        .unwrap();
        apb_core::run_origin::stamp(&dir, name).unwrap();
        dir
    }

    /// Runs `f` over a fresh project root in a child process whose config
    /// dir is its own (the run-origin key lives there), so other lib tests
    /// keep their env.
    pub(crate) fn child(name: &str, f: impl FnOnce(&Path)) {
        apb_core::process_mode::testing::in_plain_child(name, || {
            let cfg = tempfile::tempdir().unwrap();
            // SAFETY: a child process runs this one test on one thread.
            unsafe { std::env::set_var("APB_CONFIG_DIR", cfg.path()) };
            apb_core::run_origin::ensure_key().unwrap();
            let project = tempfile::tempdir().unwrap();
            std::fs::create_dir_all(project.path().join(".apb/runs")).unwrap();
            f(project.path());
        });
    }
}

#[cfg(test)]
mod tests {
    use super::testing::{child, seeded_run};
    use super::*;

    #[test]
    fn a_global_run_is_not_a_project_run_for_the_comparison() {
        child(
            "candidate::runs::tests::a_global_run_is_not_a_project_run_for_the_comparison",
            |root| {
                let project = seeded_run(root, "dup-p", None, "sha256:aa", false);
                let global = seeded_run(root, "dup-g", Some("global"), "sha256:bb", false);
                let all =
                    crate::run_history::all_runs_of(&[root.to_path_buf()], "dup", Some("1.0.0"));
                assert_eq!(
                    all.len(),
                    2,
                    "all_runs_of counts both: it is the candidate layer's filter that means project"
                );
                let listed = project_runs_of(root, "dup", Some("1.0.0"));
                assert_eq!(listed, vec![project]);
                assert!(!listed.contains(&global));
            },
        );
    }

    #[test]
    fn a_global_run_with_a_torn_journal_is_no_project_run() {
        child(
            "candidate::runs::tests::a_global_run_with_a_torn_journal_is_no_project_run",
            |root| {
                let project = seeded_run(root, "dup-p", None, "sha256:aa", false);
                seeded_run(root, "dup-g", Some("global"), "sha256:bb", true);
                assert_eq!(project_runs_of(root, "dup", Some("1.0.0")), vec![project]);
            },
        );
    }

    /// A run directory whose journal is gone is no project run: the
    /// predicate `project_runs_of` filters by answers no.
    #[test]
    fn a_run_without_a_journal_is_no_project_run() {
        let root = tempfile::tempdir().unwrap();
        let dir = root.path().join(".apb/runs/dup-gone");
        std::fs::create_dir_all(&dir).unwrap();
        assert!(!is_project_run(&dir));
    }
}
