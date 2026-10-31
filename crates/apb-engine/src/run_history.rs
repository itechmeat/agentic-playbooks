//! The earlier runs of one playbook: the run directories of a project
//! whose journal opens with that playbook (and version), newest first.
//! Shared by the pre-start estimate and the quality comparison.
//!
//! A long history is never read in full: only the newest `cap` run
//! directories are considered, and of each only the first journal lines
//! until `run_started` ([`started`]); a caller reads a matched run in full
//! afterwards.

use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};

use crate::event::{Event, EventPayload};

/// How many leading journal lines the prefilter reads for `run_started`.
const HEAD_LINES: usize = 8;

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
            EventPayload::RunStarted { playbook, version } => Some(RunHead {
                playbook,
                version,
                ts: e.ts,
            }),
            _ => None,
        })
}

/// The runs of playbook `id` (of `version` when given) among the newest
/// `cap` run directories under `roots`, newest start first.
pub fn runs_of(roots: &[PathBuf], id: &str, version: Option<&str>, cap: usize) -> Vec<PathBuf> {
    let mut matched: Vec<(u128, PathBuf)> = crate::decision::report::newest_run_dirs(roots, cap)
        .into_iter()
        .filter_map(|d| {
            let head = started(&d)?;
            (head.playbook == id && version.is_none_or(|v| v == head.version))
                .then_some((head.ts, d))
        })
        .collect();
    matched.sort_by_key(|a| std::cmp::Reverse(a.0));
    matched.into_iter().map(|(_, d)| d).collect()
}

/// Every run of playbook `id` (of `version` when given) under `roots`
/// that apb created on this machine, newest start first, however many
/// runs the project has. Each directory's journal head is read first and
/// only a match has its origin stamp checked, so the cost is a few lines
/// per run directory.
pub fn all_runs_of(roots: &[PathBuf], id: &str, version: Option<&str>) -> Vec<PathBuf> {
    let mut matched: Vec<(u128, PathBuf)> = crate::decision::report::candidate_run_dirs(roots)
        .into_iter()
        .filter_map(|d| {
            let head = started(&d)?;
            (head.playbook == id && version.is_none_or(|v| v == head.version))
                .then_some((head.ts, d))
        })
        .filter(|(_, d)| crate::decision::report::created_here(d))
        .collect();
    matched.sort_by_key(|a| std::cmp::Reverse(a.0));
    matched.into_iter().map(|(_, d)| d).collect()
}
