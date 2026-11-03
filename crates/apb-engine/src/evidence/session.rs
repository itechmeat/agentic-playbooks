//! One evidence session per agent attempt: opened by the engine before the
//! attempt starts, bound to its `attempt_started`, read through the shared
//! bounded reader, closed when the attempt ends.
//!
//! The session file `runs/<id>/evidence/<session>.json` (private, written
//! atomically under the directory's lock) holds references and counts
//! only: the bound snapshots, the views with each record's reference and
//! the digest of its stored text, the budget, the read state and, once the
//! attempt ended, the trace and the claim verdicts. No source text and no
//! claim text is ever stored.
//!
//! Every read rebuilds the views from the bound snapshots (re-verified and
//! served through the run redactor) and refuses when a record's stored
//! text no longer has the digest the session opened with. A read after the
//! attempt ended, or from a step of another run, is refused.

use std::path::{Path, PathBuf};

use apb_core::quality::grade::ReadTrace;
use apb_core::schema::Evidence;
use apb_core::step_env::StepCaller;
use serde::{Deserialize, Serialize};

use super::sources::{self, BoundSnapshot, Built, ViewRecord};
use crate::citation::reader::{DEFAULT_BUDGET_CHARS, ReadRequest, ReadResponse, Reader};
use crate::citation::views::{ReadState, SourceRecord, SourceView, ViewSet};
use crate::error::EngineError;
use crate::run_redact::RunRedactor;

/// The directory of a run's sessions.
pub const DIR: &str = "evidence";
const LOCK_FILE: &str = ".lock";
/// The session file format this binary writes.
pub const FORMAT: u32 = 1;

/// One session as its file keeps it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionFile {
    pub v: u32,
    pub session: String,
    pub run_id: String,
    /// The consumer node.
    pub node: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub item: Option<String>,
    pub attempt_number: u32,
    /// The attempt id (the seq of its `attempt_started`), once journaled.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attempt: Option<u64>,
    /// The attempt's file stem, which names its status file.
    pub stem: String,
    pub opened_at: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub closed_at: Option<u64>,
    pub snapshots: Vec<BoundSnapshot>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kinds: Option<Vec<String>>,
    pub views: Vec<ViewRecord>,
    pub state: ReadState,
    /// The read trace, written when the session closes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trace: Option<ReadTrace>,
    /// The claim verdicts, written by the check at the end of a succeeded
    /// attempt.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub claims: Option<ClaimsOutcome>,
}

impl SessionFile {
    /// The views as references only (empty texts), for what needs no text:
    /// the trace and whether a view was read to its end.
    pub fn skeleton(&self) -> ViewSet<String> {
        ViewSet::new(
            self.views
                .iter()
                .map(|v| SourceView {
                    id: v.id.clone(),
                    kind: v.kind.clone(),
                    complete: v.complete,
                    records: v
                        .records
                        .iter()
                        .map(|r| SourceRecord {
                            r#ref: r.r#ref.clone(),
                            text: String::new(),
                        })
                        .collect(),
                })
                .collect(),
        )
    }
}

/// Whether `s` has the shape of a session id: 32 lowercase hex characters.
pub fn is_session_id(s: &str) -> bool {
    s.len() == 32
        && s.bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

fn dir(run_dir: &Path) -> PathBuf {
    run_dir.join(DIR)
}

fn path(run_dir: &Path, session: &str) -> Result<PathBuf, EngineError> {
    if !is_session_id(session) {
        return Err(EngineError::Invalid(format!(
            "`{session}` is not an evidence session id"
        )));
    }
    Ok(dir(run_dir).join(format!("{session}.json")))
}

fn lock(run_dir: &Path) -> Result<apb_core::fsutil::HeldLock, EngineError> {
    match apb_core::fsutil::create_private_dir(&dir(run_dir)) {
        Err(e) if e.kind() != std::io::ErrorKind::AlreadyExists => return Err(e.into()),
        _ => {}
    }
    Ok(apb_core::fsutil::hold_lock(&dir(run_dir), LOCK_FILE)?)
}

fn save(run_dir: &Path, file: &SessionFile) -> Result<(), EngineError> {
    let json = serde_json::to_vec_pretty(file).map_err(|e| EngineError::Invalid(e.to_string()))?;
    apb_core::fsutil::atomic_write_private(&path(run_dir, &file.session)?, &json)?;
    Ok(())
}

/// The session file `session` of the run at `run_dir`.
pub fn load(run_dir: &Path, session: &str) -> Result<SessionFile, EngineError> {
    let raw = std::fs::read(path(run_dir, session)?)
        .map_err(|_| EngineError::NotFound(format!("evidence session `{session}`")))?;
    serde_json::from_slice(&raw).map_err(|e| EngineError::Invalid(e.to_string()))
}

/// Every session of the run at `run_dir`, oldest first.
pub fn all(run_dir: &Path) -> Vec<SessionFile> {
    let Ok(entries) = std::fs::read_dir(dir(run_dir)) else {
        return Vec::new();
    };
    let mut out: Vec<SessionFile> = entries
        .filter_map(Result::ok)
        .filter_map(|e| {
            let name = e.file_name().into_string().ok()?;
            let id = name.strip_suffix(".json")?;
            load(run_dir, id).ok()
        })
        .collect();
    out.sort_by_key(|f| (f.opened_at, f.session.clone()));
    out
}

/// What [`open`] needs to know about the attempt.
pub struct Opening<'a> {
    pub run_id: &'a str,
    pub node: &'a str,
    pub item: Option<&'a str>,
    pub attempt_number: u32,
    pub stem: String,
    pub spec: &'a Evidence,
}

/// Opens a session for an attempt about to start: binds the latest
/// snapshots of the `from` nodes and records their views. The session id.
pub fn open(run_dir: &Path, o: &Opening<'_>) -> Result<String, EngineError> {
    let _held = lock(run_dir)?;
    let snapshots = sources::bind(run_dir, &o.spec.from)?;
    let redactor = RunRedactor::for_run_dir(run_dir);
    let built = sources::build(run_dir, &snapshots, o.spec.kinds.as_deref(), &redactor)
        .map_err(|e| EngineError::Conflict(format!("evidence session not opened: {e}")))?;
    let file = SessionFile {
        v: FORMAT,
        session: uuid::Uuid::new_v4().simple().to_string(),
        run_id: o.run_id.to_string(),
        node: o.node.to_string(),
        item: o.item.map(str::to_string),
        attempt_number: o.attempt_number,
        attempt: None,
        stem: o.stem.clone(),
        opened_at: apb_core::clock::now_ms_u64(),
        closed_at: None,
        snapshots,
        kinds: o.spec.kinds.clone(),
        views: built.records,
        state: ReadState::new(o.spec.budget_chars.unwrap_or(DEFAULT_BUDGET_CHARS)),
        trace: None,
        claims: None,
    };
    save(run_dir, &file)?;
    Ok(file.session)
}

/// Binds session `session` to its attempt once `attempt_started` is
/// journaled at `seq`.
pub fn bind_attempt(run_dir: &Path, session: &str, seq: u64) -> Result<(), EngineError> {
    let _held = lock(run_dir)?;
    let mut file = load(run_dir, session)?;
    file.attempt = Some(seq);
    save(run_dir, &file)
}

/// Closes session `session`: no read is served after this, and the trace is
/// kept. Closing twice is a no-op.
pub fn close(run_dir: &Path, session: &str) -> Result<(), EngineError> {
    let _held = lock(run_dir)?;
    let mut file = load(run_dir, session)?;
    if file.closed_at.is_some() {
        return Ok(());
    }
    file.closed_at = Some(apb_core::clock::now_ms_u64());
    file.trace = Some(file.state.trace(&file.skeleton()));
    save(run_dir, &file)
}

/// Closes its session on drop, unless defused: the guard an attempt keeps
/// from the moment its session is open, so an early error on any path
/// between the open and the explicit close leaves the session closed, never
/// readable.
pub struct CloseGuard<'a> {
    run_dir: &'a Path,
    session: Option<&'a str>,
}

impl<'a> CloseGuard<'a> {
    pub fn new(run_dir: &'a Path, session: &'a str) -> Self {
        CloseGuard {
            run_dir,
            session: Some(session),
        }
    }

    /// The explicit close takes over; the guard does nothing on drop.
    pub fn defuse(&mut self) {
        self.session = None;
    }
}

impl Drop for CloseGuard<'_> {
    fn drop(&mut self) {
        if let Some(s) = self.session {
            let _ = close(self.run_dir, s);
        }
    }
}

/// Writes the claim verdicts of session `session`.
pub(crate) fn store_claims(
    run_dir: &Path,
    session: &str,
    claims: ClaimsOutcome,
) -> Result<(), EngineError> {
    let _held = lock(run_dir)?;
    let mut file = load(run_dir, session)?;
    file.claims = Some(claims);
    save(run_dir, &file)
}

/// The views of `file` as they read now, refused when a record's stored
/// text changed since the session opened. A record not citable when the
/// session opened stays so.
pub(crate) fn rebuild(run_dir: &Path, file: &SessionFile) -> Result<Built, EngineError> {
    let redactor = RunRedactor::for_run_dir(run_dir);
    let mut built = sources::build(run_dir, &file.snapshots, file.kinds.as_deref(), &redactor)
        .map_err(|e| EngineError::Conflict(format!("evidence session `{}`: {e}", file.session)))?;
    let same = built.records.len() == file.views.len()
        && built.records.iter().zip(&file.views).all(|(now, then)| {
            now.id == then.id
                && now.records.len() == then.records.len()
                && now
                    .records
                    .iter()
                    .zip(&then.records)
                    .all(|(a, b)| a.r#ref == b.r#ref && a.digest == b.digest)
        });
    if !same {
        return Err(EngineError::Conflict(format!(
            "evidence session `{}`: its sources changed since it opened",
            file.session
        )));
    }
    let uncitable = built
        .records
        .iter()
        .zip(&file.views)
        .flat_map(|(now, then)| now.records.iter().zip(&then.records))
        .filter(|(a, b)| !a.citable || !b.citable)
        .map(|(a, _)| a.r#ref.clone())
        .collect();
    for (now, then) in built.records.iter_mut().zip(&file.views) {
        for (a, b) in now.records.iter_mut().zip(&then.records) {
            a.citable &= b.citable;
        }
    }
    let views = std::mem::replace(&mut built.views, ViewSet::new(Vec::new()));
    built.views = views.with_uncitable(uncitable).with_scope("this session");
    Ok(built)
}

/// Whether the journal holds the end of the attempt `file` is bound to: a
/// session left open by a crash still refuses reads once its attempt is
/// written off.
fn attempt_ended(run_dir: &Path, file: &SessionFile) -> bool {
    let Some(seq) = file.attempt else {
        return false;
    };
    crate::event::read_journal(run_dir)
        .map(|j| {
            j.events.iter().any(|e| {
                e.seq > seq
                    && matches!(&e.payload, crate::event::EventPayload::AttemptFinished { node, attempt, item, .. }
                        if *node == file.node && *attempt == file.attempt_number && *item == file.item)
            })
        })
        .unwrap_or(false)
}

/// The run directory holding session `session` under the project `root`.
/// A step of a run finds only its own run's sessions: a session of another
/// run is refused (a guard against mistakes, `StepCaller`).
pub fn find(root: &Path, session: &str, caller: &StepCaller) -> Result<PathBuf, EngineError> {
    if !is_session_id(session) {
        return Err(EngineError::Invalid(format!(
            "`{session}` is not an evidence session id"
        )));
    }
    let runs = root.join(".apb/runs");
    let holds = |run_dir: &Path| dir(run_dir).join(format!("{session}.json")).is_file();
    if let Some(own) = &caller.run_id
        && apb_core::registry::is_safe_segment(own)
        && holds(&runs.join(own))
    {
        return Ok(runs.join(own));
    }
    let found = std::fs::read_dir(&runs)
        .ok()
        .into_iter()
        .flatten()
        .filter_map(Result::ok)
        .map(|e| e.path())
        .find(|p| holds(p))
        .ok_or_else(|| EngineError::NotFound(format!("evidence session `{session}`")))?;
    if let Some(own) = &caller.run_id {
        return Err(EngineError::Conflict(format!(
            "evidence session `{session}` belongs to another run than `{own}` (APB_RUN_ID is set): a run step reads only its own run's sessions"
        )));
    }
    Ok(found)
}

/// One read of session `session` in the run at `run_dir`: the shared
/// reader over the session's views, its state saved.
pub fn read(
    run_dir: &Path,
    session: &str,
    req: &ReadRequest,
    caller: &StepCaller,
) -> Result<ReadResponse, EngineError> {
    let _held = lock(run_dir)?;
    let mut file = load(run_dir, session)?;
    if let Some(own) = &caller.run_id
        && *own != file.run_id
    {
        return Err(EngineError::Conflict(format!(
            "evidence session `{session}` belongs to run `{}`, not to `{own}` (APB_RUN_ID is set): a run step reads only its own run's sessions",
            file.run_id
        )));
    }
    if file.closed_at.is_some() || attempt_ended(run_dir, &file) {
        return Err(EngineError::Conflict(format!(
            "evidence session `{session}` is closed: its attempt ended, and a session serves only the attempt it was opened for"
        )));
    }
    // A session that was never bound to an `attempt_started` (its attempt
    // never began) is refused once a newer session exists for the same
    // attempt: a crash left it open and readable. `opened_at` has
    // millisecond resolution, so a session opened in the same millisecond
    // is newer too: `>=`, never `>`.
    if file.attempt.is_none() {
        let superseded = all(run_dir).iter().any(|f| {
            f.session != file.session
                && f.node == file.node
                && f.item == file.item
                && f.attempt_number >= file.attempt_number
                && f.opened_at >= file.opened_at
        });
        if superseded {
            return Err(EngineError::Conflict(format!(
                "evidence session `{session}` was never bound to an attempt and a newer session exists for the same attempt: its attempt never started"
            )));
        }
    }
    let built = rebuild(run_dir, &file)?;
    let mut reader = Reader::new(&built.views, None, std::mem::take(&mut file.state));
    let answer = reader.handle(req);
    file.state = reader.state;
    save(run_dir, &file)?;
    answer
}

// --- what a session keeps of its attempt's claims ---------------------

/// One claim's verdict.
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[cfg_attr(feature = "ts", ts(rename = "EvidenceVerdict"))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Verdict {
    StructurallyValid,
    Unsupported,
    AbsenceOk,
    NotEnoughData,
}

/// A deterministic mark on a structurally valid claim: worth a look, never
/// an error.
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[cfg_attr(feature = "ts", ts(rename = "EvidenceFlag"))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Flag {
    /// A citation without a span.
    WholeRecord,
    /// A span under [`super::check::SHORT_SPAN_CHARS`] characters.
    ShortSpan,
    /// A record of the consumer node's own earlier execution.
    SelfCitation,
    /// One record cited by more than [`super::check::SHARED_RECORD_CLAIMS`] claims.
    SharedRecord,
    /// No word of [`super::check::MIN_WORD_LETTERS`] or more letters shared between the
    /// claim text and the cited fragment, after case folding.
    NoSharedWords,
}

/// One claim as checked.
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClaimVerdict {
    pub id: String,
    pub verdict: Verdict,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub problems: Vec<String>,
    /// Each accepted citation in the byte form `r:...#[a,b)`, the range in
    /// the stored text of the record.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub cites: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub flags: Vec<Flag>,
    /// For a claim of absence: the kinds it is about.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub absence: Option<Vec<String>>,
    /// `sha256:<hex>` of the claim text as checked, so a surface shows the
    /// status file's text only while it still hashes to it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub text_digest: Option<String>,
}

/// The check of one attempt's claims.
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClaimsOutcome {
    /// `claims: required` and no claim was written: the output is
    /// unsupported.
    #[serde(default)]
    pub output_unsupported: bool,
    /// Problems of the claims list itself (missing, not a list, too long).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub problems: Vec<String>,
    pub claims: Vec<ClaimVerdict>,
}

impl ClaimsOutcome {
    /// Whether anything is unsupported: what `on_invalid: fail` rejects.
    /// `not_enough_data` is an honest answer, not an invalid one.
    pub fn invalid(&self) -> bool {
        self.output_unsupported
            || self
                .claims
                .iter()
                .any(|c| c.verdict == Verdict::Unsupported)
    }

    /// The problems an attempt is told, one line each.
    pub fn rejection_note(&self) -> String {
        let mut lines: Vec<String> = self.problems.clone();
        for c in &self.claims {
            if c.verdict == Verdict::Unsupported {
                for p in &c.problems {
                    lines.push(format!("claim `{}`: {p}", c.id));
                }
            }
        }
        format!(
            "evidence check rejected the claims:\n- {}",
            lines.join("\n- ")
        )
    }
}
