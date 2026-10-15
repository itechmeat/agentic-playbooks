//! `apb resume --all` (issue #200, design 7): the runs a reboot or a lost
//! driver left behind.
//!
//! One pass over the workspace's runs, skipping sub-playbook children (they
//! follow their parent):
//! 1. a run that reads `running` with no live driver is resumed; when the
//!    run gate refuses, it is journaled `run_paused { reason: "driverless",
//!    by: engine }`, so `apb runs` shows it instead of a false `running`;
//! 2. a paused run whose `until` has passed is resumed (the waker that would
//!    have done it died with the machine);
//! 3. any other paused run is left alone.

use std::path::Path;

use crate::error::EngineError;
use crate::event::{EventLog, read_all};
use crate::state::{RunState, RunStatus};

use super::{PauseBy, PauseRequest};

/// The reason journaled on a driverless run the gate would not resume.
pub const DRIVERLESS_REASON: &str = "driverless";

/// Why [`resume_all`] resumed a run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResumeCause {
    /// It read `running` but no live process drove it.
    Driverless,
    /// It was paused with an `until` that has passed.
    Due,
}

impl ResumeCause {
    pub fn as_str(self) -> &'static str {
        match self {
            ResumeCause::Driverless => "driverless",
            ResumeCause::Due => "due",
        }
    }
}

/// What [`resume_all`] did with one run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResumeAllAction {
    /// Resumed in a detached driver with this pid.
    Resumed { cause: ResumeCause, pid: u32 },
    /// A driverless run the gate refused: journaled paused (`driverless`).
    MarkedDriverless { detail: String },
    /// A due paused run the gate refused: it stays paused.
    Refused { detail: String },
    /// A paused run whose `until` has not passed, or that has none.
    LeftPaused,
}

/// One run [`resume_all`] considered.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResumeAllEntry {
    pub run_id: String,
    pub action: ResumeAllAction,
}

/// Resumes what a lost driver or a passed `until` left waiting in the
/// workspace at `root` (see the module docs). Returns one entry per run it
/// considered, in run-id order; finished runs and live ones are not listed.
pub fn resume_all(root: &Path) -> Result<Vec<ResumeAllEntry>, EngineError> {
    let runs_dir = root.join(".apb/runs");
    let mut ids: Vec<String> = match std::fs::read_dir(&runs_dir) {
        Ok(rd) => rd
            .flatten()
            .filter(|e| e.path().is_dir())
            .filter_map(|e| e.file_name().into_string().ok())
            .filter(|id| apb_core::registry::is_safe_segment(id))
            .collect(),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Vec::new(),
        Err(e) => return Err(e.into()),
    };
    ids.sort();
    let now = apb_core::clock::now_ms();
    let mut out = Vec::new();
    for run_id in ids {
        let run_dir = runs_dir.join(&run_id);
        // A sub-playbook child follows its parent's drive.
        if crate::run_config::read_run_config(&run_dir)
            .ok()
            .is_some_and(|c| c.parent_run.is_some())
        {
            continue;
        }
        // A journal this binary cannot read is somebody else's problem
        // (`apb doctor --run` names it), not a reason to stop the pass.
        let Ok(events) = read_all(&run_dir) else {
            continue;
        };
        let action = match RunState::fold(&events).run_status {
            RunStatus::Paused => match super::waker::latest_pause(&events) {
                Some((seq, _, Some(until))) if u128::from(until) <= now => {
                    // A due quota pause probes its executor first, as its
                    // waker would have (design 6.6).
                    match super::probe::probe_before_wake(root, &run_id, seq)
                        .unwrap_or_else(|e| super::WakeProbe::Skipped(e.to_string()))
                    {
                        super::WakeProbe::Repaused { seq, .. } => {
                            super::waker::spawn_waker(root, &run_id, seq);
                            ResumeAllAction::LeftPaused
                        }
                        super::WakeProbe::Superseded => ResumeAllAction::LeftPaused,
                        _ => match super::waker::engine_resume(root, &run_id) {
                            Ok(pid) => ResumeAllAction::Resumed {
                                cause: ResumeCause::Due,
                                pid,
                            },
                            Err(detail) => ResumeAllAction::Refused { detail },
                        },
                    }
                }
                _ => ResumeAllAction::LeftPaused,
            },
            RunStatus::Running | RunStatus::Interrupted => {
                if crate::liveness::driver_alive(&run_dir, &run_id) == Some(true) {
                    continue;
                }
                match super::waker::engine_resume(root, &run_id) {
                    Ok(pid) => ResumeAllAction::Resumed {
                        cause: ResumeCause::Driverless,
                        pid,
                    },
                    Err(detail) => {
                        mark_driverless(&run_dir)?;
                        ResumeAllAction::MarkedDriverless { detail }
                    }
                }
            }
            _ => continue,
        };
        out.push(ResumeAllEntry { run_id, action });
    }
    Ok(out)
}

/// Journals `run_paused { reason: "driverless", by: engine }` on a run no
/// driver will ever finish (design 7.2).
fn mark_driverless(run_dir: &Path) -> Result<(), EngineError> {
    let _lock = apb_core::fsutil::lock_dir(run_dir, crate::stop::EVENT_LOCK).ok();
    let mut log = EventLog::open(run_dir)?;
    log.append(
        PauseRequest {
            reason: Some(DRIVERLESS_REASON.into()),
            by: Some(PauseBy::Engine),
            ..PauseRequest::default()
        }
        .to_event(),
    )?;
    Ok(())
}
