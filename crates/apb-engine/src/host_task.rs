//! Host tasks: the channel between a host-mode run and the host session that
//! executes its agent steps (host execution mode, 0.23.0).
//!
//! In host mode the drive spawns no agent CLI. Each agent attempt becomes a
//! host task under `runs/<id>/tasks/<task_id>/`:
//!
//! - `prompt.md`, `role.md`: the rendered prompt and the profile's role
//!   prompt, written by the drive;
//! - `task.json`: the rest of the task (node, attempt, workdir, skills, the
//!   outputs contract, the deadline, the model hint, the environment the step
//!   expects);
//! - `submission.json`: the host's reply, written once by a facade
//!   ([`submit`]: MCP `run_task_submit`, `apb tasks submit`);
//! - `output.md`: the reply text, written by the drive when it consumes the
//!   submission.
//!
//! Mirrors `question.rs`: only the drive writes `events.jsonl`
//! (`host_task_requested`, `host_task_submitted`); a facade only writes the
//! submission file, and the drive observes it from the attempt's poll loop.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::EngineError;
use crate::event::{Event, EventPayload};

/// The directory under a run that holds its host tasks.
pub const TASKS_DIR: &str = "tasks";
const PROMPT_FILE: &str = "prompt.md";
const ROLE_FILE: &str = "role.md";
const TASK_FILE: &str = "task.json";
const SUBMISSION_FILE: &str = "submission.json";
const OUTPUT_FILE: &str = "output.md";

/// What the host reports for a task.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SubmitStatus {
    /// The subagent finished the task; its reply is the attempt's output.
    Succeeded,
    /// The subagent could not do the task; the attempt fails and the normal
    /// retry and fallback policy applies.
    Failed,
    /// The subagent needs input from the person: `output` is the question.
    /// The run parks on it like on an interactive question.
    Blocked,
}

impl SubmitStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            SubmitStatus::Succeeded => "succeeded",
            SubmitStatus::Failed => "failed",
            SubmitStatus::Blocked => "blocked",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s.trim() {
            "succeeded" | "success" => Some(SubmitStatus::Succeeded),
            "failed" | "failure" => Some(SubmitStatus::Failed),
            "blocked" => Some(SubmitStatus::Blocked),
            _ => None,
        }
    }
}

/// Token usage as a host reports it. Recorded on `attempt_finished` with
/// `source: reported`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct SubmittedUsage {
    #[serde(default)]
    pub input_tokens: u64,
    #[serde(default)]
    pub output_tokens: u64,
    #[serde(default)]
    pub cache_read_tokens: u64,
    #[serde(default)]
    pub cache_write_tokens: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cost_usd: Option<f64>,
}

impl SubmittedUsage {
    pub fn to_agent_usage(&self) -> apb_core::agent_output::AgentUsage {
        apb_core::agent_output::AgentUsage {
            input_tokens: self.input_tokens,
            output_tokens: self.output_tokens,
            cache_read_tokens: self.cache_read_tokens,
            cache_write_tokens: self.cache_write_tokens,
            cost_usd: self.cost_usd.filter(|c| *c > 0.0),
            source: apb_core::agent_output::UsageSource::Reported,
        }
    }
}

/// `tasks/<id>/task.json`: everything about a task except its prompt texts.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TaskRecord {
    pub task_id: String,
    pub node: String,
    pub attempt: u32,
    pub workdir: String,
    #[serde(default)]
    pub skills: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub outputs: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deadline_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_hint: Option<String>,
    /// Environment the step expects, as the engine would have set it for a
    /// CLI attempt (`APB_RUN_DIR`, `APB_NODE_ID`, `APB_STATUS_FILE`): the
    /// host sets them for its subagent so `apb connector call` and the status
    /// file work the same way.
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    #[serde(default)]
    pub requested_ts: u64,
}

/// `tasks/<id>/submission.json`: the host's reply, written exactly once.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Submission {
    pub task_id: String,
    pub status: SubmitStatus,
    #[serde(default)]
    pub output: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage: Option<SubmittedUsage>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    /// `host` for an MCP session, `cli` for `apb tasks submit`.
    pub submitted_by: String,
    /// The MCP client name of the submitting session, when known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client: Option<String>,
    #[serde(default)]
    pub submitted_ts: u64,
}

/// A task waiting for the host, with its prompt texts inline: what
/// `run_wait`, `run_status`, `apb tasks` and the dashboard show.
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct PendingHostTask {
    /// The run the task belongs to: the run itself, or a sub-playbook child
    /// run it started (a child inherits its parent's host). Submit with this
    /// id or the parent's.
    pub run_id: String,
    pub task_id: String,
    pub node: String,
    pub attempt: u32,
    /// The task for the subagent: the rendered node prompt, the report
    /// contract included.
    pub prompt: String,
    /// The subagent's system context (the profile's role prompt), when the
    /// profile has one.
    pub role_prompt: Option<String>,
    /// Paths of the skills the subagent should load.
    pub skills: Vec<String>,
    /// The directory the subagent works in.
    pub workdir: String,
    /// The node's declared `outputs` contract, if any.
    #[cfg_attr(feature = "ts", ts(type = "unknown"))]
    pub outputs: Option<serde_json::Value>,
    /// Wall-clock milliseconds by which the task must be submitted.
    pub deadline: Option<u64>,
    /// The model a fallback entry or tier routing asks for (a hint).
    pub model_hint: Option<String>,
    /// Environment variables to set for the subagent.
    pub env: BTreeMap<String, String>,
    /// Milliseconds since epoch when the task was requested.
    pub requested_at: u64,
}

fn tasks_root(run_dir: &Path) -> PathBuf {
    run_dir.join(TASKS_DIR)
}

/// The directory of one task.
pub fn task_dir(run_dir: &Path, task_id: &str) -> PathBuf {
    tasks_root(run_dir).join(task_id)
}

/// A task id is a single safe path segment of `[A-Za-z0-9_.-]`.
pub fn is_valid_task_id(task_id: &str) -> bool {
    !task_id.is_empty()
        && task_id.len() <= 200
        && apb_core::registry::is_safe_segment(task_id)
        && task_id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
}

/// The path of a task file relative to the run directory (the `*_ref`
/// fields of the journal).
pub fn relative_ref(task_id: &str, file: &str) -> String {
    format!("{TASKS_DIR}/{task_id}/{file}")
}

pub fn prompt_ref(task_id: &str) -> String {
    relative_ref(task_id, PROMPT_FILE)
}

pub fn role_ref(task_id: &str) -> String {
    relative_ref(task_id, ROLE_FILE)
}

pub fn output_ref(task_id: &str) -> String {
    relative_ref(task_id, OUTPUT_FILE)
}

/// Allocates a fresh task id `<node>-<n>` for `node`, claiming its directory
/// atomically (`create_dir` fails on an existing one), so concurrent branches
/// never share an id.
pub(crate) fn allocate(run_dir: &Path, node: &str) -> Result<String, EngineError> {
    let root = tasks_root(run_dir);
    apb_core::fsutil::create_dir_under(run_dir, &root)?;
    let stem: String = node
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect();
    let stem = if stem.is_empty() { "task".into() } else { stem };
    for n in 1..100_000u32 {
        let id = format!("{stem}-{n}");
        match std::fs::create_dir(root.join(&id)) {
            Ok(()) => return Ok(id),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e.into()),
        }
    }
    Err(EngineError::Invalid(format!(
        "no free host task id for node `{node}`"
    )))
}

/// Writes a task's files. The directory already exists (see [`allocate`]).
pub(crate) fn write_task(
    run_dir: &Path,
    record: &TaskRecord,
    prompt: &str,
    role: Option<&str>,
) -> Result<(), EngineError> {
    let dir = task_dir(run_dir, &record.task_id);
    apb_core::fsutil::create_dir_under(run_dir, &dir)?;
    apb_core::fsutil::atomic_write_private(&dir.join(PROMPT_FILE), prompt.as_bytes())?;
    if let Some(role) = role.filter(|r| !r.trim().is_empty()) {
        apb_core::fsutil::atomic_write_private(&dir.join(ROLE_FILE), role.as_bytes())?;
    }
    let json = serde_json::to_vec_pretty(record).map_err(|e| EngineError::Yaml(e.to_string()))?;
    apb_core::fsutil::atomic_write_private(&dir.join(TASK_FILE), &json)?;
    Ok(())
}

/// Writes the reply text the drive consumed (`output_ref`).
pub(crate) fn write_output(run_dir: &Path, task_id: &str, text: &str) -> Result<(), EngineError> {
    apb_core::fsutil::atomic_write_private(
        &task_dir(run_dir, task_id).join(OUTPUT_FILE),
        text.as_bytes(),
    )?;
    Ok(())
}

pub fn read_record(run_dir: &Path, task_id: &str) -> Option<TaskRecord> {
    let raw = std::fs::read(task_dir(run_dir, task_id).join(TASK_FILE)).ok()?;
    serde_json::from_slice(&raw).ok()
}

pub fn read_prompt(run_dir: &Path, task_id: &str) -> Option<String> {
    std::fs::read_to_string(task_dir(run_dir, task_id).join(PROMPT_FILE)).ok()
}

pub fn read_role(run_dir: &Path, task_id: &str) -> Option<String> {
    std::fs::read_to_string(task_dir(run_dir, task_id).join(ROLE_FILE)).ok()
}

/// The host's submission for a task, if one was written.
pub fn read_submission(run_dir: &Path, task_id: &str) -> Option<Submission> {
    let raw = std::fs::read(task_dir(run_dir, task_id).join(SUBMISSION_FILE)).ok()?;
    serde_json::from_slice(&raw).ok()
}

/// Task ids the journal requested and has not closed, latest request per id,
/// in request order. A task is closed by its `host_task_submitted`.
fn open_requests(events: &[Event]) -> Vec<(String, String)> {
    let mut order: Vec<(String, String)> = Vec::new();
    for e in events {
        match &e.payload {
            EventPayload::HostTaskRequested { task_id, node, .. } => {
                order.retain(|(t, _)| t != task_id);
                order.push((task_id.clone(), node.clone()));
            }
            EventPayload::HostTaskSubmitted { task_id, .. } => {
                order.retain(|(t, _)| t != task_id);
            }
            // A run that ended leaves nothing for the host to do.
            EventPayload::RunFinished { .. } | EventPayload::RunAborted { .. } => order.clear(),
            _ => {}
        }
    }
    order
}

/// Every task the host still has to execute: requested, not closed in the
/// journal, and not yet submitted (a submission the drive has not consumed
/// yet is no longer the host's work), in request order, followed by those of
/// the sub-playbook child runs the run started (depth first).
pub fn pending_tasks(run_dir: &Path, events: &[Event]) -> Vec<PendingHostTask> {
    let mut out = own_pending(run_dir, events);
    for child in child_runs(events) {
        let Some(runs) = run_dir.parent() else {
            continue;
        };
        if !apb_core::registry::is_safe_segment(&child) {
            continue;
        }
        let child_dir = runs.join(&child);
        if let Ok(child_events) = crate::event::read_all_lossy_tail(&child_dir) {
            out.extend(pending_tasks(&child_dir, &child_events));
        }
    }
    out
}

fn own_pending(run_dir: &Path, events: &[Event]) -> Vec<PendingHostTask> {
    open_requests(events)
        .into_iter()
        .filter(|(task_id, _)| is_valid_task_id(task_id))
        .filter(|(task_id, _)| !task_dir(run_dir, task_id).join(SUBMISSION_FILE).exists())
        .filter_map(|(task_id, _)| pending_task(run_dir, &task_id))
        .collect()
}

/// The sub-playbook runs `events` started, oldest first, each once.
fn child_runs(events: &[Event]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for e in events {
        if let EventPayload::ChildRunStarted { run_id, .. } = &e.payload
            && !out.contains(run_id)
        {
            out.push(run_id.clone());
        }
    }
    out
}

fn pending_task(run_dir: &Path, task_id: &str) -> Option<PendingHostTask> {
    let record = read_record(run_dir, task_id)?;
    let prompt = read_prompt(run_dir, task_id)?;
    Some(PendingHostTask {
        run_id: run_dir
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default(),
        task_id: record.task_id,
        node: record.node,
        attempt: record.attempt,
        prompt,
        role_prompt: read_role(run_dir, task_id),
        skills: record.skills,
        workdir: record.workdir,
        outputs: record.outputs,
        deadline: record.deadline_ms,
        model_hint: record.model_hint,
        env: record.env,
        requested_at: record.requested_ts,
    })
}

/// What a successful [`submit`] returns.
#[derive(Debug, Clone, Serialize)]
pub struct SubmitReceipt {
    pub task_id: String,
    pub node: String,
    pub status: SubmitStatus,
}

/// A host's reply to a task, as a facade receives it.
#[derive(Debug, Clone)]
pub struct SubmitRequest {
    pub task_id: String,
    pub status: SubmitStatus,
    pub output: String,
    pub usage: Option<SubmittedUsage>,
    pub note: Option<String>,
    pub submitted_by: String,
    pub client: Option<String>,
}

/// Records a host's reply to a pending task of the run in `run_dir`. Every
/// facade (MCP `run_task_submit`, `apb tasks submit`, the web API) goes
/// through here. Refuses a task that is not pending (unknown, already
/// submitted, closed by the engine, or of a finished run) and a `blocked`
/// reply without a question. The file is published with a no-clobber link,
/// so two racing submissions cannot both land.
pub fn submit(run_dir: &Path, req: SubmitRequest) -> Result<SubmitReceipt, EngineError> {
    if !is_valid_task_id(&req.task_id) {
        return Err(EngineError::NotFound(format!(
            "host task `{}`",
            req.task_id
        )));
    }
    let events = crate::event::read_all(run_dir)?;
    let open = open_requests(&events);
    let Some((_, node)) = open.iter().find(|(t, _)| *t == req.task_id) else {
        return Err(EngineError::NotFound(format!(
            "host task `{}` is not pending on this run (unknown, already submitted, or closed)",
            req.task_id
        )));
    };
    if req.status == SubmitStatus::Blocked && req.output.trim().is_empty() {
        return Err(EngineError::Invalid(
            "a blocked submission needs the question for the person in `output`".into(),
        ));
    }
    let submission = Submission {
        task_id: req.task_id.clone(),
        status: req.status,
        output: req.output,
        usage: req.usage,
        note: req.note.filter(|n| !n.trim().is_empty()),
        submitted_by: req.submitted_by,
        client: req.client.filter(|c| !c.trim().is_empty()),
        submitted_ts: apb_core::clock::now_ms() as u64,
    };
    let dir = task_dir(run_dir, &req.task_id);
    let bytes =
        serde_json::to_vec_pretty(&submission).map_err(|e| EngineError::Yaml(e.to_string()))?;
    let tmp = dir.join(format!(".submission.tmp-{}", std::process::id()));
    let _ = std::fs::remove_file(&tmp);
    apb_core::fsutil::atomic_write_private(&tmp, &bytes)?;
    let published = std::fs::hard_link(&tmp, dir.join(SUBMISSION_FILE));
    let _ = std::fs::remove_file(&tmp);
    match published {
        Ok(()) => Ok(SubmitReceipt {
            task_id: req.task_id,
            node: node.clone(),
            status: req.status,
        }),
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => Err(EngineError::Conflict(
            format!("host task `{}` was already submitted", req.task_id),
        )),
        Err(e) => Err(e.into()),
    }
}

/// [`submit`] for a run addressed by id under a project root. The task may
/// belong to the run or to one of its sub-playbook child runs, which is what
/// `pending_tasks` of the parent lists.
pub fn submit_to_run(
    root: &Path,
    run_id: &str,
    req: SubmitRequest,
) -> Result<SubmitReceipt, EngineError> {
    let run_dir = run_dir_of(root, run_id)?;
    let events = crate::event::read_all_lossy_tail(&run_dir)?;
    // Task ids are unique only within one run: a parent and a child run may
    // both have a pending `build-1`. Through the parent's id that is
    // ambiguous, so the caller has to name the run.
    let owners: Vec<String> = pending_tasks(&run_dir, &events)
        .into_iter()
        .filter(|t| t.task_id == req.task_id)
        .map(|t| t.run_id)
        .collect();
    if owners.len() > 1 {
        return Err(EngineError::Conflict(format!(
            "host task `{}` is pending on more than one run ({}); submit with the run id of the one you mean",
            req.task_id,
            owners.join(", ")
        )));
    }
    match owners.into_iter().next() {
        Some(owner) if owner != run_id => submit(&run_dir_of(root, &owner)?, req),
        _ => submit(&run_dir, req),
    }
}

/// The pending tasks of a run addressed by id under a project root.
pub fn pending_for_run(root: &Path, run_id: &str) -> Result<Vec<PendingHostTask>, EngineError> {
    let run_dir = run_dir_of(root, run_id)?;
    let events = crate::event::read_all_lossy_tail(&run_dir)?;
    Ok(pending_tasks(&run_dir, &events))
}

fn run_dir_of(root: &Path, run_id: &str) -> Result<PathBuf, EngineError> {
    if !apb_core::registry::is_safe_segment(run_id) {
        return Err(EngineError::NotFound(format!("run `{run_id}`")));
    }
    let run_dir = root.join(".apb/runs").join(run_id);
    if !run_dir.is_dir() {
        return Err(EngineError::NotFound(format!("run `{run_id}`")));
    }
    Ok(run_dir)
}

/// What a resume finds open for a node: the task it adopts and the others it
/// closes as `superseded`.
#[derive(Debug, Default)]
pub(crate) struct Adoption {
    pub adopted: Option<TaskRecord>,
    pub superseded: Vec<String>,
}

/// The open tasks of `node` a previous drive requested and never closed (the
/// driver died while it waited). A resume adopts one instead of requesting a
/// new one, whatever its prompt (a resumed attempt's prompt may differ: an
/// interruption note, a new status-file path), so the host keeps seeing the
/// same task. It adopts the latest one the host already submitted, so that
/// reply is consumed rather than lost, else the latest one; every other open
/// task of the node is superseded.
pub(crate) fn adoptable(run_dir: &Path, events: &[Event], node: &str) -> Adoption {
    let open: Vec<String> = open_requests(events)
        .into_iter()
        .filter(|(t, n)| n == node && is_valid_task_id(t))
        .map(|(t, _)| t)
        .collect();
    let submitted = open
        .iter()
        .rev()
        .find(|t| task_dir(run_dir, t).join(SUBMISSION_FILE).exists());
    let chosen = submitted
        .or_else(|| {
            open.iter()
                .rev()
                .find(|t| read_record(run_dir, t).is_some())
        })
        .cloned();
    let adopted = chosen.as_deref().and_then(|t| read_record(run_dir, t));
    let superseded = open
        .into_iter()
        .filter(|t| Some(t) != chosen.as_ref())
        .collect();
    Adoption {
        adopted,
        superseded,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record(id: &str, node: &str) -> TaskRecord {
        TaskRecord {
            task_id: id.into(),
            node: node.into(),
            attempt: 1,
            workdir: "/w".into(),
            skills: vec![],
            outputs: None,
            deadline_ms: None,
            model_hint: None,
            env: BTreeMap::new(),
            requested_ts: 1,
        }
    }

    fn requested(seq: u64, id: &str, node: &str) -> Event {
        Event {
            seq,
            ts: 1,
            payload: EventPayload::HostTaskRequested {
                task_id: id.into(),
                node: node.into(),
                attempt: 1,
                prompt_ref: prompt_ref(id),
                role_prompt_ref: None,
                skills: vec![],
                workdir: "/w".into(),
                outputs: None,
                deadline_ms: None,
                model_hint: None,
            },
        }
    }

    #[test]
    fn allocate_hands_out_distinct_ids_per_node() {
        let dir = tempfile::tempdir().unwrap();
        let a = allocate(dir.path(), "build").unwrap();
        let b = allocate(dir.path(), "build").unwrap();
        assert_eq!((a.as_str(), b.as_str()), ("build-1", "build-2"));
        assert!(is_valid_task_id(&a));
        assert!(!is_valid_task_id("../x"));
    }

    #[test]
    fn a_pending_task_disappears_once_submitted_and_a_second_submit_conflicts() {
        let dir = tempfile::tempdir().unwrap();
        let run = dir.path();
        let id = allocate(run, "plan").unwrap();
        write_task(
            run,
            &record(&id, "plan"),
            "do it",
            Some("you are a planner"),
        )
        .unwrap();
        let events = vec![requested(1, &id, "plan")];
        let pending = pending_tasks(run, &events);
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].prompt, "do it");
        assert_eq!(pending[0].role_prompt.as_deref(), Some("you are a planner"));
        // `submit` reads the journal from disk.
        let mut log = crate::event::EventLog::open(run).unwrap();
        log.append(events[0].payload.clone()).unwrap();
        let req = SubmitRequest {
            task_id: id.clone(),
            status: SubmitStatus::Succeeded,
            output: "done".into(),
            usage: None,
            note: None,
            submitted_by: "host".into(),
            client: Some("test-host".into()),
        };
        submit(run, req.clone()).unwrap();
        assert!(pending_tasks(run, &events).is_empty());
        let again = submit(run, req).unwrap_err();
        assert!(matches!(again, EngineError::Conflict(_)), "{again}");
        assert_eq!(read_submission(run, &id).unwrap().output, "done");
    }

    #[test]
    fn submit_refuses_unknown_tasks_and_blocked_without_a_question() {
        let dir = tempfile::tempdir().unwrap();
        let run = dir.path();
        let id = allocate(run, "plan").unwrap();
        write_task(run, &record(&id, "plan"), "p", None).unwrap();
        let mut log = crate::event::EventLog::open(run).unwrap();
        log.append(requested(1, &id, "plan").payload).unwrap();
        let mut req = SubmitRequest {
            task_id: "plan-9".into(),
            status: SubmitStatus::Succeeded,
            output: "x".into(),
            usage: None,
            note: None,
            submitted_by: "host".into(),
            client: None,
        };
        assert!(matches!(
            submit(run, req.clone()),
            Err(EngineError::NotFound(_))
        ));
        req.task_id = id;
        req.status = SubmitStatus::Blocked;
        req.output = "  ".into();
        assert!(matches!(submit(run, req), Err(EngineError::Invalid(_))));
    }

    #[test]
    fn a_closed_or_finished_task_is_not_pending() {
        let dir = tempfile::tempdir().unwrap();
        let run = dir.path();
        let id = allocate(run, "plan").unwrap();
        write_task(run, &record(&id, "plan"), "p", None).unwrap();
        let closed = vec![
            requested(1, &id, "plan"),
            Event {
                seq: 2,
                ts: 2,
                payload: EventPayload::HostTaskSubmitted {
                    task_id: id.clone(),
                    status: "expired".into(),
                    output_ref: None,
                    usage: None,
                    submitted_by: "engine".into(),
                    client: None,
                    note: None,
                },
            },
        ];
        assert!(pending_tasks(run, &closed).is_empty());
        let finished = vec![
            requested(1, &id, "plan"),
            Event {
                seq: 2,
                ts: 2,
                payload: EventPayload::RunFinished {
                    outcome: "failed".into(),
                },
            },
        ];
        assert!(pending_tasks(run, &finished).is_empty());
    }

    #[test]
    fn adoptable_takes_the_node_s_latest_open_task_whatever_its_prompt() {
        let dir = tempfile::tempdir().unwrap();
        let run = dir.path();
        let a = allocate(run, "plan").unwrap();
        write_task(run, &record(&a, "plan"), "first prompt", None).unwrap();
        let b = allocate(run, "plan").unwrap();
        write_task(run, &record(&b, "plan"), "second prompt", None).unwrap();
        let events = vec![requested(1, &a, "plan"), requested(2, &b, "plan")];
        let found = adoptable(run, &events, "plan");
        assert_eq!(found.adopted.map(|r| r.task_id), Some(b.clone()));
        assert_eq!(found.superseded, vec![a.clone()]);
        assert!(adoptable(run, &events, "build").adopted.is_none());
        // A task the host already submitted wins, so its reply is used.
        std::fs::write(task_dir(run, &a).join(SUBMISSION_FILE), "{}").unwrap();
        let found = adoptable(run, &events, "plan");
        assert_eq!(found.adopted.map(|r| r.task_id), Some(a));
        assert_eq!(found.superseded, vec![b]);
    }

    /// A run directory `.apb/runs/<id>` under `root` with one requested task
    /// per `(task_id, node)` and, optionally, a started child run.
    fn seed_run(root: &Path, id: &str, tasks: &[(&str, &str)], child: Option<&str>) -> PathBuf {
        let run = root.join(".apb/runs").join(id);
        std::fs::create_dir_all(&run).unwrap();
        let mut log = crate::event::EventLog::open(&run).unwrap();
        if let Some(child) = child {
            log.append(EventPayload::ChildRunStarted {
                node_id: "sub".into(),
                run_id: child.into(),
            })
            .unwrap();
        }
        for (task_id, node) in tasks {
            std::fs::create_dir_all(task_dir(&run, task_id)).unwrap();
            write_task(&run, &record(task_id, node), "p", None).unwrap();
            log.append(requested(1, task_id, node).payload).unwrap();
        }
        run
    }

    #[test]
    fn a_task_id_pending_on_the_parent_and_a_child_is_refused_through_the_parent() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        seed_run(root, "parent", &[("build-1", "build")], Some("child"));
        let child = seed_run(root, "child", &[("build-1", "build")], None);
        let req = SubmitRequest {
            task_id: "build-1".into(),
            status: SubmitStatus::Succeeded,
            output: "done".into(),
            usage: None,
            note: None,
            submitted_by: "host".into(),
            client: None,
        };
        let err = submit_to_run(root, "parent", req.clone()).unwrap_err();
        match err {
            EngineError::Conflict(msg) => {
                assert!(msg.contains("parent") && msg.contains("child"), "{msg}")
            }
            other => panic!("expected a conflict, got {other}"),
        }
        assert!(read_submission(&child, "build-1").is_none());
        // Naming the child run is unambiguous.
        submit_to_run(root, "child", req.clone()).unwrap();
        assert_eq!(read_submission(&child, "build-1").unwrap().output, "done");
        // With the child's task submitted, the parent id is unambiguous again.
        submit_to_run(root, "parent", req).unwrap();
    }
}
