//! `apb resume --all` (issue #200, design 7): the runs a reboot or a lost
//! driver left behind.
//!
//! One pass over the workspace's runs, skipping sub-playbook children (they
//! follow their parent):
//! 1. a run that reads `running` with no live driver is resumed; when the
//!    run gate refuses, it is journaled `run_paused { reason: "driverless",
//!    by: engine }`, so `apb runs` shows it instead of a false `running`;
//!    while another run holds the busy lock of its tree, it is left as it is
//!    for a later pass;
//! 2. a paused run whose `until` has passed is resumed (the waker that would
//!    have done it died with the machine);
//! 3. a released run (`drive_released`, only deferred retries left) whose
//!    wake time has passed is resumed;
//! 4. a released run whose wake time is ahead, and a paused run whose
//!    `until` is ahead, get a new waker, since the one they had may have
//!    died with the machine;
//! 5. any other paused run is left alone.
//!
//! A finished run with a notification due a retry gets one sweep
//! (`crate::notify::retry`); it is not listed.
//!
//! Before either resume (and before a due quota pause's probe) each run
//! goes through the launch check of a resume (`crate::gate::launch`): an
//! authorized run passes, any other one needs its policy to admit the
//! caller. A run the launch policy refuses is listed as such and left as it
//! is: it is not marked driverless.

use std::path::Path;

use crate::error::EngineError;
use crate::event::{EventLog, read_all};
use crate::state::{RunState, RunStatus};

use super::{PauseBy, PauseRequest};
use crate::waker::Lease;

/// The reason journaled on a driverless run the gate would not resume.
pub const DRIVERLESS_REASON: &str = "driverless";

/// Why [`resume_all`] resumed a run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResumeCause {
    /// It read `running` but no live process drove it.
    Driverless,
    /// It was paused with an `until` that has passed.
    Due,
    /// It was released and the deferred retry it waits for is due.
    RetryDue,
}

impl ResumeCause {
    pub fn as_str(self) -> &'static str {
        match self {
            ResumeCause::Driverless => "driverless",
            ResumeCause::Due => "due",
            ResumeCause::RetryDue => "retry_due",
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
    /// A due paused run the gate refused: it stays paused. Also a driverless
    /// run that could not be journaled paused, and a due run whose waker's
    /// lease could not be taken.
    Refused { detail: String },
    /// Another run holds the busy lock of the run's tree, or the run's event
    /// lock stayed busy while a wake-up probe's outcome waited to be
    /// journaled: left as it is, a later pass (or `apb resume`) continues it.
    Busy { detail: String },
    /// A paused run with no `until`, or one whose wake-up probe paused it
    /// again or found it superseded.
    LeftPaused,
    /// A released run whose wake time, or a paused run whose `until`, is
    /// still ahead: a new waker was spawned for it (none starts when this
    /// process is not `apb`).
    Rearmed { wake_at_ms: u64 },
    /// The launch policy refused the caller for a run never authorized: left
    /// as it is, not marked driverless.
    LaunchRefused { detail: String },
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
    resume_all_as(root, &|run_id, _| {
        Some(crate::gate::launch::LaunchContext::engine(
            crate::gate::launch::EngineResume::ResumeAll,
            run_id,
        ))
    })
}

/// Who resumes a run of [`resume_all_as`] and how they confirm: called
/// first with no refusal for the run's context; when the launch check
/// refuses, called again with the refusal (a terminal asks the person
/// then) for a context to retry with, `None` to leave the run.
pub type LaunchAsk<'a> =
    dyn Fn(&str, Option<&serde_json::Value>) -> Option<crate::gate::launch::LaunchContext> + 'a;

/// [`resume_all`] by the caller `ask` describes (`apb resume --all` passes
/// the CLI's initiator and its confirmation).
pub fn resume_all_as(root: &Path, ask: &LaunchAsk<'_>) -> Result<Vec<ResumeAllEntry>, EngineError> {
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
        let state = RunState::fold(&events);
        let status = state.run_status;
        // A finished run whose notify waker died with the machine: one
        // sweep sends what is due (nothing is listed for it).
        if status.is_terminal() {
            crate::notify::retry::sweep_finished(root, &run_dir, &run_id, &events);
            continue;
        }
        // A released run's own driver exited on purpose: its waker resumes
        // it, or this pass does once its time came (at once when one of its
        // retries was expedited). A live waker is left to it.
        let release = state
            .released
            .as_ref()
            .filter(|_| status == RunStatus::Running)
            .and_then(|r| super::release_wake(&events, r.seq).map(|at| (r.seq, at)));
        if let Some((seq, wake_at_ms)) = release
            && crate::liveness::driver_alive(&run_dir, &run_id) != Some(true)
            && u128::from(wake_at_ms) > now
        {
            super::waker::spawn_release_waker(root, &run_id, seq);
            out.push(ResumeAllEntry {
                run_id,
                action: ResumeAllAction::Rearmed { wake_at_ms },
            });
            continue;
        }
        let due = match status {
            RunStatus::Paused => super::waker::latest_pause(&events)
                .and_then(|(seq, _, until)| until.map(|u| (seq, u)))
                .filter(|(_, until)| u128::from(*until) <= now)
                .map(|(seq, _)| seq),
            _ => None,
        };
        let candidate = match status {
            RunStatus::Paused => due.is_some(),
            RunStatus::Running | RunStatus::Interrupted => {
                crate::liveness::driver_alive(&run_dir, &run_id) != Some(true)
            }
            _ => false,
        };
        if !candidate {
            if status == RunStatus::Paused {
                // A pause with an `until` still ahead gets its waker back.
                let action = match super::waker::latest_pause(&events) {
                    Some((seq, _, Some(until))) => {
                        super::waker::spawn_waker(root, &run_id, seq);
                        ResumeAllAction::Rearmed { wake_at_ms: until }
                    }
                    _ => ResumeAllAction::LeftPaused,
                };
                out.push(ResumeAllEntry { run_id, action });
            }
            continue;
        }
        // A waker that holds the wait's lease is resuming it right now: it
        // is left to it, and this pass keeps the lease while it probes and
        // resumes, so no waker starts a second driver or a second probe.
        let hold = match (due, release) {
            (Some(seq), _) => Some(super::WakeHold::Pause(seq)),
            (None, Some((seq, _))) => Some(super::WakeHold::Release(seq)),
            _ => None,
        };
        let _lease = match hold.map(|h| Lease::try_take(&run_dir, h.into())) {
            Some(Ok(None)) => {
                out.push(ResumeAllEntry {
                    run_id,
                    action: ResumeAllAction::Busy {
                        detail: "its waker is resuming it".into(),
                    },
                });
                continue;
            }
            Some(Ok(Some(lease))) => Some(lease),
            // Without the lease a live waker could probe or resume beside
            // this pass: left as it is, `apb resume <run>` continues it.
            Some(Err(e)) => {
                out.push(ResumeAllEntry {
                    run_id,
                    action: ResumeAllAction::Refused {
                        detail: format!("its waker's lease could not be taken: {e}"),
                    },
                });
                continue;
            }
            None => None,
        };
        // The launch check, before any probe or resume.
        let grant = match launch_for(root, &run_id, ask) {
            Ok(g) => g,
            Err(detail) => {
                out.push(ResumeAllEntry {
                    run_id,
                    action: ResumeAllAction::LaunchRefused { detail },
                });
                continue;
            }
        };
        let action = match status {
            RunStatus::Paused => match due {
                Some(seq) => {
                    // A due quota pause probes its executor first, as its
                    // waker would have (design 6.6).
                    // A probe whose outcome is not known never leads to a
                    // resume (see the waker).
                    match super::probe::probe_before_wake_within(
                        root,
                        &run_id,
                        seq,
                        super::probe::RESUME_ALL_JOURNAL_TRIES,
                        // Only a grant stands for a launch check: a run whose
                        // check was set aside (a tree that does not resolve)
                        // is checked again as the engine's own resume before
                        // its executor is probed.
                        grant.is_some(),
                    ) {
                        Ok(super::WakeProbe::Repaused { seq, .. }) => {
                            super::waker::spawn_waker(root, &run_id, seq);
                            ResumeAllAction::LeftPaused
                        }
                        Ok(super::WakeProbe::Superseded) => ResumeAllAction::LeftPaused,
                        // The run's event lock stayed busy: a later pass
                        // probes again.
                        Err(EngineError::WorkdirBusy(detail)) => ResumeAllAction::Busy { detail },
                        Err(e) => ResumeAllAction::Refused {
                            detail: super::waker::refusal_text(e),
                        },
                        Ok(_) => match super::waker::try_engine_resume(
                            root,
                            &run_id,
                            Some(super::WakeHold::Pause(seq)),
                            grant.as_ref(),
                        ) {
                            Ok(pid) => ResumeAllAction::Resumed {
                                cause: ResumeCause::Due,
                                pid,
                            },
                            Err(EngineError::WorkdirBusy(detail)) => {
                                ResumeAllAction::Busy { detail }
                            }
                            Err(e) => ResumeAllAction::Refused {
                                detail: super::waker::refusal_text(e),
                            },
                        },
                    }
                }
                None => ResumeAllAction::LeftPaused,
            },
            RunStatus::Running | RunStatus::Interrupted => {
                match super::waker::try_engine_resume(root, &run_id, hold, grant.as_ref()) {
                    Ok(pid) => ResumeAllAction::Resumed {
                        // What was resumed, not what the run once read.
                        cause: match hold {
                            Some(super::WakeHold::Release(_)) => ResumeCause::RetryDue,
                            _ => ResumeCause::Driverless,
                        },
                        pid,
                    },
                    // Busy is not a refusal: the run is resumable once the
                    // lock frees, so it is not marked.
                    Err(EngineError::WorkdirBusy(detail)) => ResumeAllAction::Busy { detail },
                    Err(e) => {
                        let detail = super::waker::refusal_text(e);
                        match mark_driverless(&run_dir) {
                            Ok(()) => ResumeAllAction::MarkedDriverless { detail },
                            Err(mark) => ResumeAllAction::Refused {
                                detail: format!(
                                    "{detail}; it could not be marked paused (driverless) either: {mark}"
                                ),
                            },
                        }
                    }
                }
            }
            _ => continue,
        };
        // The lease goes once the driver took the run over.
        if let (ResumeAllAction::Resumed { pid, .. }, Some(hold)) = (&action, hold) {
            super::waker::await_takeover(&run_dir, *pid, hold);
        }
        out.push(ResumeAllEntry { run_id, action });
    }
    Ok(out)
}

/// The launch check of one run of the pass: the grant to resume with, or
/// the refusal's text when the caller gave up.
fn launch_for(
    root: &Path,
    run_id: &str,
    ask: &LaunchAsk<'_>,
) -> Result<Option<crate::gate::launch::LaunchGrant>, String> {
    let text = crate::gate::launch::refusal_line;
    let ctx = ask(run_id, None).ok_or_else(|| "left by the caller".to_string())?;
    match crate::resume_launch::check_resume_launch(root, run_id, &ctx) {
        Ok(g) => Ok(g),
        // A run whose snapshot does not read is the resume's own problem:
        // it refuses (and a driverless run is marked) as before, while the
        // engine still refuses an unchecked run that is not `allow`.
        Err(refusal)
            if !refusal["policy"]
                .as_str()
                .is_some_and(crate::gate::launch::is_launch_refusal) =>
        {
            Ok(None)
        }
        Err(refusal) => {
            let Some(retry) = ask(run_id, Some(&refusal)) else {
                return Err(text(&refusal));
            };
            crate::resume_launch::check_resume_launch(root, run_id, &retry).map_err(|v| text(&v))
        }
    }
}

/// Journals `run_paused { reason: "driverless", by: engine }` on a run no
/// driver will ever finish (design 7.2).
fn mark_driverless(run_dir: &Path) -> Result<(), EngineError> {
    // Unlike `stop_run`, which must never be kept from stopping a run, a
    // pause journaled without the lock could race a stop or another pause;
    // a lock that cannot be taken refuses (the caller reports it).
    let _lock = apb_core::fsutil::lock_dir(run_dir, crate::stop::EVENT_LOCK)?;
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
