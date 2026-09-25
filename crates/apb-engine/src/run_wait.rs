//! Server-side blocking waits for a run, so a caller hands the work off and
//! spends nothing until there is something to act on.
//!
//! Every status check an orchestrating agent makes is a model turn: it pays for
//! the whole conversation again plus the tool result. A caller that follows a
//! run by calling `run_status` in a loop spends tokens on every check while the
//! run simply keeps running. These waits block in the APB process instead (a
//! cheap file poll), and return only when the caller has a decision to make:
//! the run finished, the run needs input, or the caller's own timeout ran out.
//!
//! Two waits live here:
//! - [`wait_run`] for a plain caller (MCP `run_wait`, `apb wait`): returns on a
//!   terminal outcome, a gate that needs an answer, or a stopped run;
//! - [`wait_supervisor_event`] for a supervisor: returns on a wake, a new
//!   human-review gate it has to relay, or the end of the run, and keeps the
//!   supervisor's heartbeat fresh while it blocks, so a long wait is never
//!   mistaken for a lost supervisor.

use std::path::Path;
use std::thread::sleep;
use std::time::{Duration, Instant};

use serde::Serialize;

use crate::error::EngineError;
use crate::event::EventPayload;
use crate::inspect::WakeEvent;
use crate::run_view::read_events;
use crate::state::RunStatus;

/// How often the waits re-read the run. A file read, not a model call.
pub const WAIT_POLL: Duration = Duration::from_millis(200);

/// How long a non-terminal stop condition (a pending gate, a paused or
/// driverless run) must hold before [`wait_run`] reports it. An answer or a
/// review decision posted just before the wait is consumed by the driver
/// within its own 50 ms poll; without this grace the caller's next wait would
/// return the gate it has already answered, and pay a turn for nothing.
pub const STABLE_GRACE: Duration = Duration::from_millis(1500);

/// Why [`wait_run`] returned.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum WaitReason {
    /// The run reached `succeeded`, `failed` or `aborted`.
    Finished,
    /// The run is blocked on a question, a human review, or a supervisor
    /// decision; the answer channel is named in [`RunWaitResult::needs`].
    NeedsInput,
    /// The run is not progressing and nothing can answer it from here: it is
    /// paused, or interrupted with no live driver, including a driver that
    /// died while the journal still read `running` (resume it).
    Stopped,
    /// The caller's timeout ran out while the run was still working.
    Timeout,
}

/// The gate a run is blocked on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum NeedsInput {
    Question,
    Review,
    Supervisor,
}

impl NeedsInput {
    pub fn as_str(self) -> &'static str {
        match self {
            NeedsInput::Question => "question",
            NeedsInput::Review => "review",
            NeedsInput::Supervisor => "supervisor",
        }
    }
}

/// One observation of a run: its reported status and the gate, if any.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RunSnapshot {
    pub status: RunStatus,
    pub needs: Option<NeedsInput>,
    pub driver_alive: Option<bool>,
}

impl RunSnapshot {
    /// What this observation would make a waiting caller do, if anything.
    fn stop_reason(&self) -> Option<WaitReason> {
        if self.status.is_terminal() {
            return Some(WaitReason::Finished);
        }
        if self.needs.is_some() {
            return Some(WaitReason::NeedsInput);
        }
        match self.status {
            RunStatus::Paused => Some(WaitReason::Stopped),
            RunStatus::Interrupted if self.driver_alive != Some(true) => Some(WaitReason::Stopped),
            _ => None,
        }
    }
}

/// The result of [`wait_run`].
#[derive(Debug, Clone)]
pub struct RunWaitResult {
    pub reason: WaitReason,
    pub status: RunStatus,
    pub needs: Option<NeedsInput>,
    /// The drive claim at the last observation: `Some(false)` means the
    /// process driving the run is gone and only a resume continues it.
    pub driver_alive: Option<bool>,
    pub waited: Duration,
    /// The observation the wait decided on. A caller that reports more than
    /// the fields above (pending gates, node counts, the answer) reads them
    /// from here, never from a second read that may disagree with `reason`.
    pub view: std::sync::Arc<crate::run_view::RunView>,
}

/// Reads the run once, through the same [`crate::run_view::RunView`] every
/// status surface reports from.
pub fn snapshot(run_dir: &Path, run_id: &str) -> Result<RunSnapshot, EngineError> {
    Ok(snapshot_of(&crate::run_view::RunView::load(
        run_dir, run_id,
    )?))
}

/// The [`RunSnapshot`] of an already loaded view.
fn snapshot_of(view: &crate::run_view::RunView) -> RunSnapshot {
    let needs = view.progress.as_ref().and_then(|p| {
        if p.pending_question.is_some() {
            Some(NeedsInput::Question)
        } else if p.pending_review.is_some() {
            Some(NeedsInput::Review)
        } else if p.pending_supervisor.is_some() {
            Some(NeedsInput::Supervisor)
        } else {
            None
        }
    });
    RunSnapshot {
        status: view.run_status,
        needs,
        driver_alive: view.driver_alive,
    }
}

/// Blocks until run `run_id` finishes, needs input, stops, or `timeout`
/// elapses, re-reading it every [`WAIT_POLL`]. A terminal outcome returns at
/// once; a gate or a stop must hold for [`STABLE_GRACE`] first.
pub fn wait_run(
    root: &Path,
    run_id: &str,
    timeout: Duration,
) -> Result<RunWaitResult, EngineError> {
    RunWaiter::new(root, run_id)?.wait(timeout)
}

/// [`wait_run`] with explicit grace and poll step (tests).
pub fn wait_run_with(
    root: &Path,
    run_id: &str,
    timeout: Duration,
    grace: Duration,
    poll: Duration,
) -> Result<RunWaitResult, EngineError> {
    let mut w = RunWaiter::new(root, run_id)?;
    w.grace = grace;
    w.poll = poll;
    w.wait(timeout)
}

/// A wait on one run that can be resumed across several calls of
/// [`RunWaiter::wait`] (the MCP server waits in short slices between progress
/// notifications) without losing how long a gate has already been pending.
#[derive(Debug)]
pub struct RunWaiter {
    run_dir: std::path::PathBuf,
    run_id: String,
    grace: Duration,
    poll: Duration,
    started: Instant,
    /// When the current (non-terminal) stop condition was first seen, and
    /// what it was: a condition that changes restarts the grace.
    pending_since: Option<(Instant, WaitReason, Option<NeedsInput>)>,
}

impl RunWaiter {
    pub fn new(root: &Path, run_id: &str) -> Result<Self, EngineError> {
        Ok(Self {
            run_dir: resolve(root, run_id)?,
            run_id: run_id.to_string(),
            grace: STABLE_GRACE,
            poll: WAIT_POLL,
            started: Instant::now(),
            pending_since: None,
        })
    }

    /// Waits up to `timeout` more; `waited` in the result counts from the
    /// waiter's creation.
    pub fn wait(&mut self, timeout: Duration) -> Result<RunWaitResult, EngineError> {
        let deadline = Instant::now() + timeout;
        loop {
            let view =
                std::sync::Arc::new(crate::run_view::RunView::load(&self.run_dir, &self.run_id)?);
            let snap = snapshot_of(&view);
            let now = Instant::now();
            match snap.stop_reason() {
                Some(WaitReason::Finished) => {
                    return Ok(result(WaitReason::Finished, snap, view, self.started));
                }
                Some(reason) => {
                    let since = match self.pending_since {
                        Some((t, r, n)) if r == reason && n == snap.needs => t,
                        _ => {
                            self.pending_since = Some((now, reason, snap.needs));
                            now
                        }
                    };
                    if now.duration_since(since) >= self.grace {
                        return Ok(result(reason, snap, view, self.started));
                    }
                }
                None => self.pending_since = None,
            }
            if now >= deadline {
                return Ok(result(WaitReason::Timeout, snap, view, self.started));
            }
            sleep(
                self.poll
                    .min(deadline.saturating_duration_since(now))
                    .max(Duration::from_millis(1)),
            );
        }
    }
}

fn result(
    reason: WaitReason,
    snap: RunSnapshot,
    view: std::sync::Arc<crate::run_view::RunView>,
    started: Instant,
) -> RunWaitResult {
    RunWaitResult {
        reason,
        status: snap.status,
        needs: snap.needs,
        driver_alive: snap.driver_alive,
        waited: started.elapsed(),
        view,
    }
}

/// What a supervisor wait returned.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SupervisorWait {
    /// The first `WakeRaised` after the cursor.
    Wake(WakeEvent),
    /// A human-review gate opened after the cursor: the supervisor relays it.
    /// `seq` is the `ReviewRequested` event, the cursor for the next wait.
    Review { seq: u64, node: String },
    /// The run has ended; nothing is left to wake for.
    Ended,
    /// The timeout ran out with nothing new.
    TimedOut,
}

/// Default block of one server-side wait (MCP `run_wait`,
/// `supervisor_wait_event`) when the caller names none: under the ~60 s
/// tool-call limit of the strictest hosts (Codex, ChatGPT Apps), so a single
/// wait never times out on the host side.
pub const RUN_WAIT_DEFAULT_MS: u64 = 50_000;
/// Upper bound for one server-side wait (30 minutes). Hosts with a long tool
/// timeout (Claude Code) can wait out a whole run in one call.
pub const RUN_WAIT_MAX_MS: u64 = 30 * 60 * 1000;

/// How long a supervisor agent may stay silent (no heartbeat) before the
/// drive declares it lost and respawns it once. `APB_SUPERVISOR_HEARTBEAT_MS`
/// overrides it (tests).
pub const SUPERVISOR_LOSS_THRESHOLD: Duration = Duration::from_secs(60);

/// How often [`wait_supervisor_event`] refreshes the supervisor heartbeat
/// while it blocks: well inside [`SUPERVISOR_LOSS_THRESHOLD`].
pub const HEARTBEAT_EVERY: Duration = Duration::from_secs(10);
const _: () = assert!(HEARTBEAT_EVERY.as_millis() * 3 <= SUPERVISOR_LOSS_THRESHOLD.as_millis());

/// Blocks until the first wake or human-review gate with seq strictly greater
/// than `after_seq`, the end of the run, or `timeout`. Touches the
/// supervisor's heartbeat on entry and every [`HEARTBEAT_EVERY`] while
/// waiting, so a supervisor that asks for a long wait (and so spends no turns)
/// is never declared lost and respawned.
pub fn wait_supervisor_event(
    root: &Path,
    run_id: &str,
    after_seq: Option<u64>,
    timeout: Duration,
) -> Result<SupervisorWait, EngineError> {
    wait_supervisor_event_with(root, run_id, after_seq, timeout, HEARTBEAT_EVERY)
}

/// [`wait_supervisor_event`] with an explicit heartbeat cadence (tests).
pub fn wait_supervisor_event_with(
    root: &Path,
    run_id: &str,
    after_seq: Option<u64>,
    timeout: Duration,
    heartbeat_every: Duration,
) -> Result<SupervisorWait, EngineError> {
    let run_dir = resolve(root, run_id)?;
    let cursor: i128 = after_seq.map(i128::from).unwrap_or(-1);
    let deadline = Instant::now() + timeout;
    crate::inspect::touch_heartbeat(root, run_id)?;
    let mut last_beat = Instant::now();
    loop {
        for event in read_events(&run_dir)? {
            if i128::from(event.seq) <= cursor {
                continue;
            }
            match event.payload {
                EventPayload::WakeRaised {
                    trigger,
                    node,
                    detail,
                } => {
                    return Ok(SupervisorWait::Wake(WakeEvent {
                        seq: event.seq,
                        trigger,
                        node,
                        detail,
                    }));
                }
                EventPayload::ReviewRequested { node, .. } => {
                    return Ok(SupervisorWait::Review {
                        seq: event.seq,
                        node,
                    });
                }
                EventPayload::RunFinished { .. } | EventPayload::RunAborted { .. } => {
                    return Ok(SupervisorWait::Ended);
                }
                _ => {}
            }
        }
        let now = Instant::now();
        if now >= deadline {
            return Ok(SupervisorWait::TimedOut);
        }
        if now.duration_since(last_beat) >= heartbeat_every {
            crate::inspect::touch_heartbeat(root, run_id)?;
            last_beat = now;
        }
        sleep(WAIT_POLL.min(deadline - now));
    }
}

fn resolve(root: &Path, run_id: &str) -> Result<std::path::PathBuf, EngineError> {
    if !apb_core::registry::is_safe_segment(run_id) {
        return Err(EngineError::NotFound(format!("run `{run_id}`")));
    }
    let run_dir = root.join(".apb/runs").join(run_id);
    if !run_dir.is_dir() {
        return Err(EngineError::NotFound(format!("run `{run_id}`")));
    }
    Ok(run_dir)
}

/// Keeps at most `max` bytes of `text`, preferring the tail (a failed
/// process usually prints its error last) and cutting on a char boundary.
/// Returns the kept text and whether anything was dropped.
pub fn clip_tail(text: &str, max: usize) -> (String, bool) {
    if text.len() <= max {
        return (text.to_string(), false);
    }
    let mut start = text.len() - max;
    while !text.is_char_boundary(start) {
        start += 1;
    }
    (
        format!(
            "[... {} earlier bytes omitted ...]\n{}",
            start,
            &text[start..]
        ),
        true,
    )
}
