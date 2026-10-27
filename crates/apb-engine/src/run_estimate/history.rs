//! The comparable runs of a target: the newest [`ESTIMATE_WINDOW`] runs of
//! the same playbook id, version and execution mode, among the newest
//! [`ESTIMATE_SCAN`] run directories of the project (a long history is not
//! read in full), each read in full only after its first journal lines
//! matched.

use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};

use super::types::{ESTIMATE_SCAN, ESTIMATE_WINDOW, EstimateTarget};
use crate::event::{Event, EventPayload};
use crate::run_stats::StatsRun;

/// How many leading journal lines the prefilter reads for `run_started`.
const HEAD_LINES: usize = 8;

/// The playbook, version and start time a run's journal opens with, read
/// from its first lines only.
fn started(run_dir: &Path) -> Option<(String, String, u128)> {
    let file = std::fs::File::open(run_dir.join("events.jsonl")).ok()?;
    BufReader::new(file)
        .lines()
        .take(HEAD_LINES)
        .map_while(Result::ok)
        .filter_map(|l| serde_json::from_str::<Event>(&l).ok())
        .find_map(|e| match e.payload {
            EventPayload::RunStarted { playbook, version } => Some((playbook, version, e.ts)),
            _ => None,
        })
}

/// The comparable runs, newest first: the newest [`ESTIMATE_WINDOW`] runs
/// of the target's playbook id, version and execution mode (the target's
/// own run left out; a run whose mode this apb does not know is left out
/// too).
pub(super) fn comparable(roots: &[PathBuf], target: &EstimateTarget) -> Vec<(PathBuf, StatsRun)> {
    let mut matched: Vec<(u128, PathBuf)> =
        crate::decision::report::newest_run_dirs(roots, ESTIMATE_SCAN)
            .into_iter()
            .filter(|d| {
                target
                    .exclude_run
                    .as_deref()
                    .is_none_or(|own| d.file_name().and_then(|n| n.to_str()) != Some(own))
            })
            .filter_map(|d| {
                let (id, version, ts) = started(&d)?;
                (id == target.playbook.id && version == target.version).then_some((ts, d))
            })
            .collect();
    matched.sort_by_key(|a| std::cmp::Reverse(a.0));
    matched
        .into_iter()
        .filter_map(|(_, d)| {
            let run = StatsRun::load(&d)?;
            (run.execution.as_deref() == Some(target.execution.as_str())).then_some((d, run))
        })
        .take(ESTIMATE_WINDOW)
        .collect()
}
