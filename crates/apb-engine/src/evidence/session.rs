//! One evidence session per attempt of a node with `evidence:`.
//!
//! The engine prepares the session before the attempt's prompt is built
//! ([`prepare`]: a 128-bit random id, the bound snapshots and their views)
//! and writes `runs/<id>/evidence/<attempt_id>.json` right after the
//! attempt's `attempt_started` is journaled ([`Pending::open`]). The agent
//! gets the session's handle `<run_id>:<session>` in its prompt and in
//! `APB_EVIDENCE_SESSION`, and reads through [`read`] (MCP
//! `run_evidence_read`, `apb evidence read`). When the attempt's process
//! is gone the engine closes the session ([`close`]); every later read is
//! refused. The check of the attempt's claims writes its verdicts and the
//! read trace into the file ([`record`]).
//!
//! The file keeps references, digests and counts only, never a text: the
//! views are rebuilt from the stored snapshots on every read, and a record
//! whose text as served changed since the session opened (a secret known
//! only now) is no longer citable.

use std::path::{Path, PathBuf};

use apb_core::quality::grade::ReadTrace;
use apb_core::schema::Node;
use serde::{Deserialize, Serialize};

use super::check::Outcome;
use super::sources::{self, BoundSource, ViewMeta, Views};
use crate::citation::ReadState;
use crate::citation::reader::{DEFAULT_BUDGET_CHARS, ReadRequest, ReadResponse, Reader};
use crate::error::EngineError;
use crate::event::Event;
use crate::redact::Redactor;

/// The directory of a run's evidence sessions.
pub const EVIDENCE_DIR: &str = "evidence";
/// The variable that hands an attempt's session handle to its agent.
pub const SESSION_ENV: &str = "APB_EVIDENCE_SESSION";
/// The session file format this binary writes.
pub const FILE_VERSION: u32 = 1;
const LOCK_FILE: &str = ".lock";

/// A session as its file keeps it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionFile {
    pub v: u32,
    /// 32 lowercase hex characters (128 random bits).
    pub session: String,
    pub run_id: String,
    pub node: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub item: Option<String>,
    /// The attempt number.
    pub attempt: u32,
    /// The attempt id: the seq of its `attempt_started`.
    pub attempt_id: u64,
    /// The attempt's status file stem (`agent-status/<stem>.json`).
    pub stem: String,
    pub opened_at: u64,
    pub sources: Vec<BoundSource>,
    /// The kinds the declaration allows; absent: every kind.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kinds: Option<Vec<String>>,
    pub views: Vec<ViewMeta>,
    pub state: ReadState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub closed: Option<Closed>,
}

/// How a session ended.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Closed {
    pub at: u64,
    /// The operations, the budget and how far each view was read.
    pub trace: ReadTrace,
    /// The claims were checked (a succeeded attempt).
    #[serde(default)]
    pub checked: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub outcome: Option<Outcome>,
}

impl SessionFile {
    /// The handle an agent names the session by.
    pub fn handle(&self) -> String {
        format!("{}:{}", self.run_id, self.session)
    }

    /// What the check of this session's claims reads.
    pub fn reading(&self) -> super::check::Reading<'_> {
        super::check::Reading {
            node: &self.node,
            kinds: self.kinds.as_deref(),
            sources: &self.sources,
            state: &self.state,
        }
    }

    fn split(&self) -> bool {
        self.kinds.is_some()
    }
}

fn dir(run_dir: &Path) -> PathBuf {
    run_dir.join(EVIDENCE_DIR)
}

fn path(run_dir: &Path, attempt_id: u64) -> PathBuf {
    dir(run_dir).join(format!("{attempt_id}.json"))
}

fn lock(run_dir: &Path) -> Result<apb_core::fsutil::DirLock, EngineError> {
    Ok(apb_core::fsutil::lock_dir(&dir(run_dir), LOCK_FILE)?)
}

fn save(run_dir: &Path, file: &SessionFile) -> Result<(), EngineError> {
    let json = serde_json::to_vec_pretty(file).map_err(|e| EngineError::Invalid(e.to_string()))?;
    apb_core::fsutil::atomic_write_private(&path(run_dir, file.attempt_id), &json)?;
    Ok(())
}

fn load(run_dir: &Path, attempt_id: u64) -> Result<Option<SessionFile>, EngineError> {
    match std::fs::read(path(run_dir, attempt_id)) {
        Ok(raw) => serde_json::from_slice(&raw)
            .map(Some)
            .map_err(|e| EngineError::Invalid(format!("evidence session file: {e}"))),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e.into()),
    }
}

/// Every session file of the run, by attempt id.
pub fn all(run_dir: &Path) -> Vec<SessionFile> {
    let mut out: Vec<SessionFile> = std::fs::read_dir(dir(run_dir))
        .into_iter()
        .flatten()
        .filter_map(Result::ok)
        .filter(|e| e.file_name().to_string_lossy().ends_with(".json"))
        .filter_map(|e| std::fs::read(e.path()).ok())
        .filter_map(|raw| serde_json::from_slice::<SessionFile>(&raw).ok())
        .collect();
    out.sort_by_key(|f| f.attempt_id);
    out
}

/// A session prepared for an attempt, written once the attempt has an id.
#[derive(Debug, Clone)]
pub struct Pending {
    file: SessionFile,
}

impl Pending {
    /// The handle the attempt's prompt and environment carry.
    pub fn handle(&self) -> String {
        self.file.handle()
    }

    /// Writes the session file for the attempt `attempt_id` (the seq of its
    /// `attempt_started`): from now on the agent can read.
    pub fn open(&self, run_dir: &Path, attempt_id: u64) -> Result<(), EngineError> {
        let _held = lock(run_dir)?;
        let file = SessionFile {
            attempt_id,
            opened_at: apb_core::clock::now_ms_u64(),
            ..self.file.clone()
        };
        save(run_dir, &file)
    }
}

/// The session of one attempt of `node`, when it declares `evidence`:
/// the latest stamped snapshot of each source in `events` and their views
/// (texts read, redacted and dropped again: only references and digests
/// stay). Refused (`secrets_unresolved`) while the run redactor cannot
/// read every secret of the run's tree.
pub fn prepare(
    run_dir: &Path,
    events: &[Event],
    node: &Node,
    key: crate::exec_key::ExecKey<'_>,
    attempt: u32,
) -> Result<Option<Pending>, EngineError> {
    let Some(decl) = &node.evidence else {
        return Ok(None);
    };
    let run_id = run_dir
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let bound = sources::sources_of(run_dir, sources::bind(events, &decl.from))?;
    let Views { meta, .. } = sources::load(run_dir, &bound, decl.kinds.is_some())?;
    Ok(Some(Pending {
        file: SessionFile {
            v: FILE_VERSION,
            session: uuid::Uuid::new_v4().simple().to_string(),
            run_id,
            node: node.id.clone(),
            item: key.item.map(str::to_string),
            attempt,
            attempt_id: 0,
            stem: key.stem(attempt),
            opened_at: 0,
            sources: bound,
            kinds: decl.kinds.clone(),
            views: meta,
            state: ReadState::new(decl.budget_chars.unwrap_or(DEFAULT_BUDGET_CHARS)),
            closed: None,
        },
    }))
}

/// The views of `file` as they read now: rebuilt from its snapshots, the
/// references checked against the ones it opened with, and a record whose
/// served text changed since (a secret known only now) no longer citable.
pub(crate) fn views_of(run_dir: &Path, file: &SessionFile) -> Result<Views, EngineError> {
    let views = sources::load(run_dir, &file.sources, file.split())?;
    let refs = |m: &[ViewMeta]| -> Vec<(String, Vec<String>)> {
        m.iter()
            .map(|v| {
                (
                    v.id.clone(),
                    v.records.iter().map(|r| r.r#ref.clone()).collect(),
                )
            })
            .collect()
    };
    if refs(&views.meta) != refs(&file.views) {
        return Err(EngineError::Conflict(
            "the sources of this evidence session changed since it opened".into(),
        ));
    }
    let changed: Vec<String> = file
        .views
        .iter()
        .zip(&views.meta)
        .flat_map(|(was, now)| was.records.iter().zip(&now.records))
        .filter(|(was, now)| !was.citable || was.digest != now.digest)
        .map(|(was, _)| was.r#ref.clone())
        .collect();
    Ok(Views {
        set: views.set.with_uncitable(changed),
        meta: views.meta,
    })
}

/// A session handle: `<run_id>:<32 hex>`.
pub fn parse_handle(handle: &str) -> Result<(&str, &str), EngineError> {
    let bad = || EngineError::Invalid(format!("`{handle}` is not an evidence session handle"));
    let (run_id, session) = handle.rsplit_once(':').ok_or_else(bad)?;
    let hex = session.len() == 32
        && session
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b));
    if !hex || !apb_core::registry::is_safe_segment(run_id) || run_id.contains(':') {
        return Err(bad());
    }
    Ok((run_id, session))
}

/// The open session file `handle` names at `run_dir`.
fn find(run_dir: &Path, session: &str) -> Result<SessionFile, EngineError> {
    all(run_dir)
        .into_iter()
        .find(|f| f.session == session)
        .ok_or_else(|| EngineError::NotFound(format!("evidence session `{session}`")))
}

/// One reading step of the session `handle` of the run at `run_dir`.
/// `caller_run` is the caller's `APB_RUN_ID`: a run step reads only its
/// own run's sessions (a guard against mistakes, not a boundary). Refused
/// once the session closed.
pub fn read(
    run_dir: &Path,
    handle: &str,
    req: &ReadRequest,
    caller_run: Option<&str>,
) -> Result<ReadResponse, EngineError> {
    let (run_id, session) = parse_handle(handle)?;
    if let Some(caller) = caller_run.filter(|c| *c != run_id) {
        return Err(EngineError::Conflict(format!(
            "run_step_other_run: a step of run `{caller}` reads only its own run's evidence sessions, and `{run_id}` is another run"
        )));
    }
    let _held = lock(run_dir)?;
    let mut file = find(run_dir, session)?;
    if file.run_id != run_id {
        return Err(EngineError::NotFound(format!(
            "evidence session `{session}`"
        )));
    }
    if file.closed.is_some() {
        return Err(EngineError::Conflict(format!(
            "evidence session `{session}` is closed: its attempt ended"
        )));
    }
    let views = views_of(run_dir, &file)?;
    for (meta, now) in file.views.iter_mut().zip(&views.meta) {
        for (r, n) in meta.records.iter_mut().zip(&now.records) {
            r.citable &= r.digest == n.digest;
        }
    }
    // The texts went through the run redactor already.
    let redactor = Redactor::default();
    let mut reader = Reader::over(&views.set, true, &redactor, std::mem::take(&mut file.state));
    let answer = reader.handle(req);
    file.state = reader.state;
    save(run_dir, &file)?;
    answer
}

/// Closes the session of the attempt `attempt_id`, if it has one: every
/// later read is refused. Returns the file as it closed. Closing twice
/// changes nothing.
pub fn close(run_dir: &Path, attempt_id: u64) -> Result<Option<SessionFile>, EngineError> {
    if !path(run_dir, attempt_id).is_file() {
        return Ok(None);
    }
    let _held = lock(run_dir)?;
    let Some(mut file) = load(run_dir, attempt_id)? else {
        return Ok(None);
    };
    if file.closed.is_none() {
        let views = views_of(run_dir, &file).map(|v| v.set).ok();
        let trace = match &views {
            Some(set) => file.state.trace(set),
            None => file
                .state
                .trace(&crate::citation::ViewSet::<String>::new(Vec::new())),
        };
        file.closed = Some(Closed {
            at: apb_core::clock::now_ms_u64(),
            trace,
            checked: false,
            outcome: None,
        });
        save(run_dir, &file)?;
    }
    Ok(Some(file))
}

/// Writes the check's outcome into the closed session of `attempt_id`.
pub fn record(run_dir: &Path, attempt_id: u64, outcome: &Outcome) -> Result<(), EngineError> {
    let _held = lock(run_dir)?;
    let Some(mut file) = load(run_dir, attempt_id)? else {
        return Ok(());
    };
    if let Some(closed) = &mut file.closed {
        closed.checked = true;
        closed.outcome = Some(outcome.clone());
        save(run_dir, &file)?;
    }
    Ok(())
}

/// The run directory a handle names under the project `root`, or
/// `run_dir_hint` (a step's `APB_RUN_DIR`) when its name is the handle's
/// run: a child run need not sit under `root`.
pub fn run_dir_for(
    root: &Path,
    handle: &str,
    run_dir_hint: Option<&Path>,
) -> Result<PathBuf, EngineError> {
    let (run_id, _) = parse_handle(handle)?;
    if let Some(hint) = run_dir_hint
        && hint.file_name().is_some_and(|n| n == run_id)
        && hint.join("events.jsonl").is_file()
    {
        return Ok(hint.to_path_buf());
    }
    let dir = root.join(".apb/runs").join(run_id);
    if !dir.join("events.jsonl").is_file() {
        return Err(EngineError::NotFound(format!("run `{run_id}`")));
    }
    Ok(dir)
}
