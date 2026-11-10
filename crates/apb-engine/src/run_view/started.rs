//! Runs started from a run step (routine operations, D3), as the run
//! surfaces show them: a run's starter, from its own launch record, and
//! the reverse links, the runs a step of a run started in this project,
//! read from their records. Nothing is written into the starting run.

use std::path::Path;

pub use apb_core::launch::StartedBy;

/// How many run directories the reverse scan reads at most.
pub const STARTED_RUNS_SCAN: usize = 500;
/// How many started runs a surface lists at most.
pub const STARTED_RUNS_MAX: usize = 50;

/// One run a step of a run started.
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct StartedRunView {
    pub run_id: String,
    pub playbook: String,
    /// `project` or `global`: where its definition came from.
    pub scope: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub node: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub attempt: Option<u32>,
}

/// The runs a step of one run started in this project.
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize)]
pub struct StartedRuns {
    /// Newest first, at most [`STARTED_RUNS_MAX`].
    pub runs: Vec<StartedRunView>,
    /// The project holds more run directories than the scan's bound
    /// ([`STARTED_RUNS_SCAN`]): entries among those it did not read may be
    /// missing. Journal-write order says nothing about start times, so no
    /// run inside the bound proves the rest complete.
    pub truncated: bool,
}

/// The starter the run at `run_dir` records (its manifest's launch
/// record); `None` for a run no run step started.
pub fn started_by(run_dir: &Path) -> Option<StartedBy> {
    crate::manifest::read(run_dir)
        .ok()
        .flatten()
        .and_then(|m| m.launch)
        .and_then(|l| l.started_by)
}

/// The runs a step of `run_id` started in the project `root`: runs this
/// installation created whose launch record has a verified `started_by`
/// naming `run_id` (and this project's workspace id, when it names one).
/// The scan reads at most [`STARTED_RUNS_SCAN`] run directories, newest
/// written first, and skips those that started before the starter; it is
/// truncated whenever the project holds more directories than that.
pub fn started_runs(root: &Path, run_id: &str) -> StartedRuns {
    let starter_ts = crate::run_history::started(&root.join(".apb/runs").join(run_id))
        .map(|h| h.ts)
        .unwrap_or(0);
    let here = apb_core::projects::registered_id(root);
    // One directory past the bound says whether any was left unread.
    let mut dirs =
        crate::decision::report::newest_run_dirs(&[root.to_path_buf()], STARTED_RUNS_SCAN + 1);
    let mut out = StartedRuns {
        truncated: dirs.len() > STARTED_RUNS_SCAN,
        ..StartedRuns::default()
    };
    dirs.truncate(STARTED_RUNS_SCAN);
    for dir in dirs {
        let Some(head) = crate::run_history::started(&dir) else {
            continue;
        };
        // A run that started before the starter cannot be one it started.
        // It is skipped, not a stop: directories come by their journal's
        // last write, so an older run still writing sits among newer ones.
        if head.ts < starter_ts {
            continue;
        }
        let Some(by) = started_by(&dir) else {
            continue;
        };
        let ours = by.verified
            && by.run_id == run_id
            && by.workspace_id.is_none_or(|ws| Some(&ws) == here.as_ref());
        if !ours || out.runs.len() >= STARTED_RUNS_MAX {
            continue;
        }
        let scope = match crate::run_origin::parent_run_origin(&dir) {
            apb_core::scope::Origin::Global => "global",
            _ => "project",
        };
        out.runs.push(StartedRunView {
            run_id: dir
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default(),
            playbook: head.playbook,
            scope: scope.to_string(),
            node: by.node,
            attempt: by.attempt,
        });
    }
    out
}

/// The line of a run's starter: `run <id> (node <n>, attempt <k>)`, with
/// `, not verified` and `, in workspace <ws> (<root>)` for a starter in
/// another project (`root` resolved through the projects registry).
pub fn started_by_line(by: &StartedBy, here: Option<&str>) -> String {
    let mut parts = Vec::new();
    if let Some(n) = &by.node {
        parts.push(format!("node {n}"));
    }
    if let Some(a) = by.attempt {
        parts.push(format!("attempt {a}"));
    }
    let mut line = format!("run {}", by.run_id);
    if !parts.is_empty() {
        line.push_str(&format!(" ({})", parts.join(", ")));
    }
    if !by.verified {
        line.push_str(", not verified");
    }
    if let Some(ws) = by.workspace_id.as_deref()
        && Some(ws) != here
    {
        let root = apb_core::projects::resolve_root(ws)
            .map(|r| r.display().to_string())
            .unwrap_or_else(|_| "not registered here".into());
        line.push_str(&format!(", in workspace {ws} ({root})"));
    }
    line
}

/// The line of a run's started runs: `<id> (<playbook>), ...`, with the
/// truncation note when the scan stopped at its bound.
pub fn started_runs_line(s: &StartedRuns) -> String {
    let mut line = s
        .runs
        .iter()
        .map(|r| format!("{} ({})", r.run_id, r.playbook))
        .collect::<Vec<_>>()
        .join(", ");
    if s.truncated {
        line.push_str(&format!(
            ", more may exist beyond the newest {STARTED_RUNS_SCAN} runs"
        ));
    }
    line
}

#[cfg(test)]
mod tests;
