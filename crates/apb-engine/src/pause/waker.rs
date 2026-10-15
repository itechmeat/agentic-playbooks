//! The waker of a paused run with an `until` (issue #200, design 4.2).
//!
//! A paused run holds no driver, so nothing would ever resume it at `until`.
//! The driver that journals such a pause (or `request_pause`, for a run
//! nothing drives) spawns a small detached `apb __pause-waker` process. It
//! holds no lock and runs no node: it sleeps until `until`, and then resumes
//! the run only when the run is still paused by the very `run_paused` it was
//! spawned for (journal sequence match). An earlier `apb resume`, a newer
//! pause or a stop makes it exit without doing anything. A reboot kills it;
//! `apb resume --all` covers that case ([`super::resume_all`]).

use std::path::Path;
use std::process::{Command, Stdio};
use std::time::Duration;

use crate::error::EngineError;
use crate::event::read_all;
use crate::state::{RunState, RunStatus};

/// Milliseconds between the waker's journal reads while it sleeps: how fast
/// it notices that the run was resumed, paused again or stopped. Tests
/// shorten it.
pub const POLL_ENV: &str = "APB_WAKER_POLL_MS";

const DEFAULT_POLL: Duration = Duration::from_secs(5);

/// How a waker ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WakeOutcome {
    /// `until` passed with the same pause in place: the run was resumed in a
    /// detached driver with this pid.
    Resumed { pid: u32 },
    /// The pause it was spawned for no longer holds the run (resumed early,
    /// paused again, stopped): nothing to do.
    Superseded,
    /// `until` passed but the resume was refused (the run gate, a missing
    /// consent, environment drift): the run stays paused.
    Refused(String),
}

fn poll() -> Duration {
    std::env::var(POLL_ENV)
        .ok()
        .and_then(|v| v.trim().parse::<u64>().ok())
        .map_or(DEFAULT_POLL, Duration::from_millis)
}

/// The `until` of the pause that holds the run now, when that pause is the
/// `run_paused` at journal sequence `seq`. `None` when the run is not paused,
/// or paused by another event, or that pause has no `until`.
fn same_pause_until(events: &[crate::event::Event], seq: u64) -> Option<u64> {
    if RunState::fold(events).run_status != RunStatus::Paused {
        return None;
    }
    match latest_pause(events)? {
        (s, _, until) if s == seq => until,
        _ => None,
    }
}

/// The run's latest `run_paused`: its journal sequence, `keep_lock` and
/// `until`.
pub(crate) fn latest_pause(events: &[crate::event::Event]) -> Option<(u64, bool, Option<u64>)> {
    events.iter().rev().find_map(|e| match &e.payload {
        crate::event::EventPayload::RunPaused {
            keep_lock, until, ..
        } => Some((e.seq, *keep_lock, *until)),
        _ => None,
    })
}

/// The engine-side resume of the waker and `apb resume --all`: the run gate
/// a person's `apb resume` stands for (a run directory this machine created,
/// the consent a resume needs), then a detached driver whose `run_resumed`
/// names `engine`. With `pause_seq`, that driver journals `run_resumed` only
/// while the pause at that journal sequence still holds the run. A gate
/// refusal is `Conflict`; a busy lock is `WorkdirBusy`, so a caller can wait
/// for it instead of giving up.
pub(crate) fn try_engine_resume(
    root: &Path,
    run_id: &str,
    pause_seq: Option<u64>,
) -> Result<u32, EngineError> {
    // Acknowledged: like `apb resume`, the operator who set up the pause or
    // the boot unit is the confirmation; what still refuses is a run
    // directory this machine did not create.
    crate::gate::check_resume(root, run_id, true)
        .map_err(|v| EngineError::Conflict(v.to_string()))?;
    match pause_seq {
        Some(seq) => crate::scheduler::resume_detached_pause(root, run_id, seq),
        None => {
            crate::scheduler::resume_detached_by(root, run_id, None, false, super::PauseBy::Engine)
        }
    }
}

/// A refusal of [`try_engine_resume`] as a line for the operator.
pub(crate) fn refusal_text(e: EngineError) -> String {
    match e {
        EngineError::Conflict(why) => why,
        other => other.to_string(),
    }
}

/// The waker's body: sleeps until the pause at journal sequence `seq` of run
/// `run_id` reaches its `until`, then resumes the run (`run_resumed.by` is
/// `engine`) if that pause still holds it. Blocks for as long as the pause
/// lasts.
pub fn run_waker(root: &Path, run_id: &str, seq: u64) -> Result<WakeOutcome, EngineError> {
    if !apb_core::registry::is_safe_segment(run_id) {
        return Err(EngineError::NotFound(format!("run `{run_id}`")));
    }
    let run_dir = root.join(".apb/runs").join(run_id);
    let mut seq = seq;
    loop {
        if !run_dir.is_dir() {
            return Ok(WakeOutcome::Superseded);
        }
        let Some(until) = same_pause_until(&read_all(&run_dir)?, seq) else {
            return Ok(WakeOutcome::Superseded);
        };
        let now = apb_core::clock::now_ms();
        if now >= u128::from(until) {
            // A quota pause probes its executor first (design 6.6); a probe
            // that hits the quota again re-pauses the run, and this waker
            // goes on waiting for the new pause.
            match super::probe::probe_before_wake(root, run_id, seq)
                .unwrap_or_else(|e| super::WakeProbe::Skipped(e.to_string()))
            {
                super::WakeProbe::Repaused { seq: next, .. } => {
                    seq = next;
                    continue;
                }
                super::WakeProbe::Superseded => return Ok(WakeOutcome::Superseded),
                super::WakeProbe::NotQuota
                | super::WakeProbe::Skipped(_)
                | super::WakeProbe::Passed => {}
            }
            // Another run holding the busy lock of this run's tree is not a
            // refusal: the run stays paused and the waker retries until the
            // lock frees or another pause, resume or stop supersedes it.
            loop {
                match try_engine_resume(root, run_id, Some(seq)) {
                    Ok(pid) => return Ok(WakeOutcome::Resumed { pid }),
                    Err(EngineError::WorkdirBusy(_))
                        if run_dir.is_dir()
                            && same_pause_until(&read_all(&run_dir)?, seq).is_some() =>
                    {
                        std::thread::sleep(poll());
                    }
                    Err(EngineError::WorkdirBusy(_)) => return Ok(WakeOutcome::Superseded),
                    Err(EngineError::Conflict(why)) => return Ok(WakeOutcome::Refused(why)),
                    Err(other) => return Ok(WakeOutcome::Refused(other.to_string())),
                }
            }
        }
        let left =
            Duration::from_millis(u64::try_from(u128::from(until) - now).unwrap_or(u64::MAX));
        std::thread::sleep(left.min(poll()));
    }
}

/// Spawns the detached waker for the pause at journal sequence `seq` of run
/// `run_id`. Best effort: a waker that cannot start leaves an ordinary
/// paused run, which `apb resume` or `apb resume --all` continues. Only the
/// `apb` binary can serve as the waker, so a host process that is not `apb`
/// (a test binary) spawns nothing.
pub fn spawn_waker(root: &Path, run_id: &str, seq: u64) -> Option<u32> {
    let exe = apb_core::fsutil::reexec_exe().ok()?;
    if exe.file_stem().and_then(|s| s.to_str()) != Some("apb") {
        return None;
    }
    let root = std::fs::canonicalize(root).ok()?;
    let mut cmd = Command::new(exe);
    cmd.arg("__pause-waker")
        .arg("--root")
        .arg(&root)
        .arg("--run-id")
        .arg(run_id)
        .arg("--seq")
        .arg(seq.to_string())
        .current_dir(&root)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    // Its own process group, like the detached driver: tearing down the
    // launcher's group must not take the waker with it.
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt as _;
        cmd.process_group(0);
    }
    match apb_core::fsutil::spawn_when_not_busy(&mut cmd) {
        Ok(mut child) => {
            let pid = child.id();
            std::thread::spawn(move || {
                let _ = child.wait();
            });
            Some(pid)
        }
        Err(e) => {
            eprintln!(
                "apb: warning: run `{run_id}` is paused until a set time, but its waker could not start ({e}); `apb resume {run_id}` or `apb resume --all` continues it"
            );
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::{Event, EventPayload};
    use crate::pause::PauseBy;

    fn ev(seq: u64, payload: EventPayload) -> Event {
        Event {
            seq,
            ts: u128::from(seq),
            payload,
        }
    }

    fn paused(until: Option<u64>) -> EventPayload {
        super::super::PauseRequest {
            until,
            by: Some(PauseBy::Operator),
            ..Default::default()
        }
        .to_event()
    }

    #[test]
    fn the_waker_acts_only_on_its_own_pause() {
        let started = ev(
            0,
            EventPayload::RunStarted {
                playbook: "p".into(),
                version: "1.0.0".into(),
            },
        );
        let events = vec![started.clone(), ev(1, paused(Some(500)))];
        assert_eq!(same_pause_until(&events, 1), Some(500));
        // Another pause replaced it.
        let newer = vec![
            started.clone(),
            ev(1, paused(Some(500))),
            ev(2, paused(Some(900))),
        ];
        assert_eq!(same_pause_until(&newer, 1), None);
        assert_eq!(same_pause_until(&newer, 2), Some(900));
        // Resumed early.
        let resumed = vec![
            started,
            ev(1, paused(Some(500))),
            ev(
                2,
                EventPayload::RunResumed {
                    from_node: "a".into(),
                    by: Some(PauseBy::Operator),
                },
            ),
        ];
        assert_eq!(same_pause_until(&resumed, 1), None);
    }
}
