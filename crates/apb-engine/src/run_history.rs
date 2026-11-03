//! The earlier runs of one playbook: the run directories of a project
//! whose journal opens with that playbook (and version), newest first.
//! Shared by the pre-start estimate and the quality comparison.
//!
//! Only production runs are history ([`counts_as_history`]): a scenario
//! run, an eval run and a run of an origin this apb does not know never
//! count toward statistics, estimates, quality comparisons, candidate
//! trials or a retrospective. Scenario runs also live outside
//! `.apb/runs`, where no released apb's reader looks; the origin is the
//! second layer, for every reader of this apb.
//!
//! A long history is never read in full: only the newest `cap` run
//! directories are considered, and of each only the first journal lines
//! until `run_started` ([`started`]); a caller reads a matched run in full
//! afterwards.

use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};

use crate::event::{Event, EventPayload};
use crate::manifest::RunOrigin;

/// How many leading journal lines the prefilter reads for `run_started`.
const HEAD_LINES: usize = 8;

/// Whether `run` counts as history: only a production run does. The one
/// filter every reader of earlier runs applies.
pub fn counts_as_history(run: &crate::run_stats::StatsRun) -> bool {
    run.origin.is_production()
}

/// [`counts_as_history`] for a run directory not read as a whole: its
/// manifest's origin only.
fn production_dir(run_dir: &Path) -> bool {
    RunOrigin::of_read(&crate::manifest::read(run_dir)).is_production()
}

/// What a run's journal opens with.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunHead {
    pub playbook: String,
    pub version: String,
    /// The `run_started` time, Unix milliseconds.
    pub ts: u128,
}

/// The playbook, version and start time a run's journal opens with, read
/// from its first lines only.
pub fn started(run_dir: &Path) -> Option<RunHead> {
    let file = std::fs::File::open(run_dir.join("events.jsonl")).ok()?;
    BufReader::new(file)
        .lines()
        .take(HEAD_LINES)
        .map_while(Result::ok)
        .filter_map(|l| serde_json::from_str::<Event>(&l).ok())
        .find_map(|e| match e.payload {
            EventPayload::RunStarted {
                playbook, version, ..
            } => Some(RunHead {
                playbook,
                version,
                ts: e.ts,
            }),
            _ => None,
        })
}

/// The production runs of playbook `id` (of `version` when given) among
/// the newest `cap` run directories under `roots`, newest start first.
pub fn runs_of(roots: &[PathBuf], id: &str, version: Option<&str>, cap: usize) -> Vec<PathBuf> {
    let mut matched: Vec<(u128, PathBuf)> = crate::decision::report::newest_run_dirs(roots, cap)
        .into_iter()
        .filter_map(|d| {
            let head = started(&d)?;
            (head.playbook == id && version.is_none_or(|v| v == head.version))
                .then_some((head.ts, d))
        })
        .filter(|(_, d)| production_dir(d))
        .collect();
    matched.sort_by_key(|a| std::cmp::Reverse(a.0));
    matched.into_iter().map(|(_, d)| d).collect()
}

/// Every production run of playbook `id` (of `version` when given) under `roots`
/// that apb created on this machine, newest start first, however many
/// runs the project has. Each directory's journal head is read first and
/// only a match has its origin stamp and its manifest checked, so the cost
/// is a few lines per run directory.
pub fn all_runs_of(roots: &[PathBuf], id: &str, version: Option<&str>) -> Vec<PathBuf> {
    let mut matched: Vec<(u128, PathBuf)> = crate::decision::report::candidate_run_dirs(roots)
        .into_iter()
        .filter_map(|d| {
            let head = started(&d)?;
            (head.playbook == id && version.is_none_or(|v| v == head.version))
                .then_some((head.ts, d))
        })
        .filter(|(_, d)| crate::decision::report::created_here(d) && production_dir(d))
        .collect();
    matched.sort_by_key(|a| std::cmp::Reverse(a.0));
    matched.into_iter().map(|(_, d)| d).collect()
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use crate::manifest::{RunExecutionManifest, RunOrigin};

    /// A run of playbook `p` created on this machine, started at `ts`,
    /// with `origin` in its manifest (`None`: no manifest at all, an older
    /// run's layout).
    fn seed(root: &Path, id: &str, ts: u64, origin: Option<RunOrigin>) {
        let dir = root.join(".apb/runs").join(id);
        std::fs::create_dir_all(&dir).unwrap();
        let line = serde_json::json!({"seq": 1, "ts": ts, "type": "run_started", "playbook": "p", "version": "1.0.0"});
        std::fs::write(dir.join("events.jsonl"), format!("{line}\n")).unwrap();
        if let Some(origin) = origin {
            let m = RunExecutionManifest {
                origin: Some(origin),
                ..Default::default()
            };
            crate::manifest::write(&dir, &m).unwrap();
        }
        apb_core::run_origin::stamp(&dir, id).unwrap();
    }

    /// The history of a playbook, as the estimate, the quality comparison,
    /// the known problems and the grade backfill read it, holds its
    /// production runs only: a scenario run, an eval run, a run of an
    /// origin this apb does not know and a run whose manifest does not
    /// read are left out, however new they are.
    #[test]
    fn only_production_runs_are_the_history_of_a_playbook() {
        apb_core::process_mode::testing::in_plain_child(
            "run_history::tests::only_production_runs_are_the_history_of_a_playbook",
            || {
                let cfg = tempfile::tempdir().unwrap();
                // SAFETY: a child process runs this one test on one thread.
                unsafe { std::env::set_var("APB_CONFIG_DIR", cfg.path()) };
                apb_core::run_origin::ensure_key().unwrap();
                let project = tempfile::tempdir().unwrap();
                let root = project.path();
                seed(root, "old", 1, None);
                seed(root, "prod", 2, Some(RunOrigin::Production));
                seed(
                    root,
                    "scenario",
                    3,
                    Some(RunOrigin::Scenario {
                        level: apb_core::scenario::ScenarioLevel::Smoke,
                        scenario: "green".into(),
                        scenario_digest: "sha256:s".into(),
                    }),
                );
                seed(root, "eval", 4, Some(RunOrigin::Eval));
                seed(root, "newer", 5, Some(RunOrigin::Unknown));
                // A manifest that does not read: its kind cannot be told.
                seed(root, "unreadable", 6, None);
                let unreadable = root.join(".apb/runs/unreadable");
                std::fs::write(
                    unreadable.join("manifest.yaml"),
                    "origin: [not a manifest\n",
                )
                .unwrap();
                assert_eq!(
                    crate::run_stats::StatsRun::load(&unreadable)
                        .unwrap()
                        .origin,
                    RunOrigin::Unknown
                );
                let roots = [root.to_path_buf()];
                let names = |dirs: Vec<std::path::PathBuf>| -> Vec<String> {
                    dirs.iter()
                        .map(|d| d.file_name().unwrap().to_string_lossy().into_owned())
                        .collect()
                };
                assert_eq!(
                    names(super::runs_of(&roots, "p", None, 10)),
                    ["prod", "old"]
                );
                assert_eq!(
                    names(super::all_runs_of(&roots, "p", Some("1.0.0"))),
                    ["prod", "old"]
                );
            },
        );
    }
}
