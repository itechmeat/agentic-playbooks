//! A host's grading session: the host agent grades the playbook's own
//! criteria by reading the run through the bounded reader, then submits
//! one level per criterion with the records it cites.
//!
//! One session per run at a time (`runs/<id>/grading/`, under a lock),
//! open for [`SESSION_TTL_MS`]. The session file keeps the reading state
//! (operations, references read in full, references only routed to) and
//! the first round's levels, never any text. A refused first round may be
//! corrected once; the second submission is always accepted (see
//! [`super::evidence`]) and closes the session.
//!
//! A run takes one accepted host grade: once it has one, a new session is
//! refused until a person reopens the run's grading ([`reopen`], from the
//! CLI or the dashboard, never from inside a run). A host's
//! `not_applicable` does not count in a promotion: it can hold a candidate
//! back, never let one through.
//!
//! The run's sources are read and redacted once and kept for a few
//! minutes per process ([`open_run`]), so each reading step does not
//! re-read the whole run.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime};

use apb_core::quality::criteria::{CriterionDef, CriterionSource, ReadKind};
use apb_core::quality::grade::{Grade, GraderId, Level};
use serde::{Deserialize, Serialize};

use super::evidence::{Proposal, Rejection, Round, Validated, validate};
use super::reader::{DEFAULT_BUDGET_CHARS, ReadRequest, ReadResponse, ReadState, Reader};
use super::sources::{RunEnd, RunSources};
use crate::error::EngineError;
use crate::redact::Redactor;

pub const GRADING_DIR: &str = "grading";
const LOCK_FILE: &str = ".lock";
/// A person's leave for one more host grade of the run.
const REOPENED_FILE: &str = "reopened.json";
/// How long a session stays open.
pub const SESSION_TTL_MS: u64 = 60 * 60 * 1000;

#[derive(Debug, Clone, Serialize, Deserialize)]
struct SessionFile {
    session: String,
    run_id: String,
    created_at: u64,
    expires_at: u64,
    criteria: Vec<String>,
    state: ReadState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    round: Option<Round>,
    /// The grade the session ended with.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    closed: Option<String>,
}

impl SessionFile {
    fn open_at(&self, now: u64) -> bool {
        self.closed.is_none() && now < self.expires_at
    }
}

/// A criterion as the host is told about it.
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct CriterionBrief {
    pub id: String,
    pub subject: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub applies_when: Option<String>,
    /// `presence`, `absence` or `both`.
    pub evidence: String,
    /// The views it may rest on.
    pub reads: Vec<String>,
    pub good: String,
    pub partial: String,
    pub poor: String,
}

impl CriterionBrief {
    fn of(d: &CriterionDef) -> Self {
        let word = |v: serde_json::Value| v.as_str().unwrap_or_default().to_string();
        CriterionBrief {
            id: d.id().to_string(),
            subject: d.criterion.subject.key(),
            applies_when: d.criterion.applies_when.clone(),
            evidence: word(serde_json::json!(d.criterion.evidence)),
            reads: d
                .reads()
                .into_iter()
                .map(|k: ReadKind| word(serde_json::json!(k)))
                .collect(),
            good: d.criterion.levels.good.clone(),
            partial: d.criterion.levels.partial.clone(),
            poor: d.criterion.levels.poor.clone(),
        }
    }
}

/// What a started session gives the host.
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct SessionStart {
    pub session: String,
    /// Unix milliseconds.
    pub expires_at: u64,
    pub criteria: Vec<CriterionBrief>,
    /// The views of the run (routing only).
    pub outline: ReadResponse,
}

/// The answer to a submission.
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum SubmitOutcome {
    /// Stored; the session is closed.
    Graded { grade: Box<Grade> },
    /// Refused: correct and submit once more.
    Rejected { problems: Vec<Rejection> },
}

fn grading_dir(run_dir: &Path) -> PathBuf {
    run_dir.join(GRADING_DIR)
}

fn session_path(run_dir: &Path, session: &str) -> Result<PathBuf, EngineError> {
    if !apb_core::registry::is_safe_segment(session) {
        return Err(EngineError::Invalid(format!("session `{session}`")));
    }
    Ok(grading_dir(run_dir).join(format!("{session}.json")))
}

fn save(run_dir: &Path, file: &SessionFile) -> Result<(), EngineError> {
    let json = serde_json::to_vec_pretty(file).map_err(|e| EngineError::Invalid(e.to_string()))?;
    apb_core::fsutil::atomic_write_private(&session_path(run_dir, &file.session)?, &json)?;
    Ok(())
}

fn load(run_dir: &Path, session: &str) -> Result<SessionFile, EngineError> {
    let raw = std::fs::read(session_path(run_dir, session)?)
        .map_err(|_| EngineError::NotFound(format!("grading session `{session}`")))?;
    let file: SessionFile =
        serde_json::from_slice(&raw).map_err(|e| EngineError::Invalid(e.to_string()))?;
    if !file.open_at(apb_core::clock::now_ms_u64()) {
        return Err(EngineError::Conflict(format!(
            "grading session `{session}` is closed or expired: start a new one"
        )));
    }
    Ok(file)
}

fn lock(run_dir: &Path) -> Result<apb_core::fsutil::DirLock, EngineError> {
    if !run_dir.join("events.jsonl").is_file() {
        return Err(EngineError::NotFound(format!(
            "run `{}`",
            run_dir.display()
        )));
    }
    Ok(apb_core::fsutil::lock_dir(
        &grading_dir(run_dir),
        LOCK_FILE,
    )?)
}

/// An ended run as a session reads it: its sources, redacted, and its
/// host-graded criteria.
struct OpenRun {
    sources: RunSources,
    defs: Vec<CriterionDef>,
}

/// What an opened run is cached under: the run and its journal as it is.
type OpenKey = (PathBuf, u64, Option<SystemTime>);

/// How long an opened run is kept, and how many are.
const OPEN_TTL: Duration = Duration::from_secs(10 * 60);
const OPEN_CACHED: usize = 4;

static OPENED: Mutex<Vec<(OpenKey, Instant, Arc<OpenRun>)>> = Mutex::new(Vec::new());

fn open_key(run_dir: &Path) -> OpenKey {
    let meta = std::fs::metadata(run_dir.join("events.jsonl")).ok();
    (
        run_dir.to_path_buf(),
        meta.as_ref().map_or(0, std::fs::Metadata::len),
        meta.and_then(|m| m.modified().ok()),
    )
}

/// The ended run at `run_dir`, its sources redacted once: from the cache
/// while its journal is unchanged, else read anew. An ended run's
/// transcripts do not change.
fn open_run(run_dir: &Path) -> Result<Arc<OpenRun>, EngineError> {
    let key = open_key(run_dir);
    let mut cache = OPENED.lock().unwrap_or_else(|e| e.into_inner());
    cache.retain(|(_, at, _)| at.elapsed() < OPEN_TTL);
    if let Some((_, _, run)) = cache.iter().find(|(k, _, _)| *k == key) {
        return Ok(Arc::clone(run));
    }
    drop(cache);
    let mut sources = RunSources::load(run_dir, RunEnd::Journaled)?;
    if sources.outcome.is_none() {
        return Err(EngineError::Conflict(
            "the run has not ended: a run is graded once it ends".into(),
        ));
    }
    sources.redact(&super::grader::workspace_redactor(run_dir));
    let defs: Vec<CriterionDef> = super::grader::criteria_for(&sources)
        .into_iter()
        .filter(|d| d.source == CriterionSource::Playbook)
        .collect();
    let run = Arc::new(OpenRun { sources, defs });
    let mut cache = OPENED.lock().unwrap_or_else(|e| e.into_inner());
    cache.retain(|(k, _, _)| k.0 != key.0);
    if cache.len() >= OPEN_CACHED {
        cache.remove(0);
    }
    cache.push((key, Instant::now(), Arc::clone(&run)));
    Ok(run)
}

/// A person's leave to grade the run again: the host grades the run had
/// when it was given (any new one spends it), when and from which surface.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct Reopened {
    over: Vec<String>,
    at: u64,
    by: String,
}

/// The host grades the run had when a person last reopened it.
fn reopened_over(run_dir: &Path) -> Option<Vec<String>> {
    let raw = std::fs::read(grading_dir(run_dir).join(REOPENED_FILE)).ok()?;
    serde_json::from_slice::<Reopened>(&raw)
        .ok()
        .map(|r| r.over)
}

/// The ids of the run's host grades, oldest first.
fn host_grade_ids(run_dir: &Path) -> Vec<String> {
    super::store::grades_of(run_dir)
        .into_iter()
        .filter(|g| g.grader == GraderId::HOST)
        .map(|g| g.grade_id)
        .collect()
}

/// Whether the run's grading is reopened now: a person's leave was given
/// over exactly the host grades the run has (a new grade spends it; a
/// grade file that went missing or unreadable does not reopen anything).
fn reopened(run_dir: &Path, ids: &[String]) -> bool {
    !ids.is_empty() && reopened_over(run_dir).as_deref() == Some(ids)
}

/// The latest host grade of the run when a new one is not allowed: the
/// run has one and no person reopened its grading since.
fn host_graded(run_dir: &Path) -> Option<String> {
    let ids = host_grade_ids(run_dir);
    if reopened(run_dir, &ids) {
        return None;
    }
    ids.last().cloned()
}

/// A person reopens the grading of the run at `run_dir` (`by` names the
/// surface): one more host grade may be stored. Refused for a run without
/// a host grade, whose grading is open; a run reopened already stays so.
pub fn reopen(run_dir: &Path, by: &str) -> Result<(), EngineError> {
    let _held = lock(run_dir)?;
    let ids = host_grade_ids(run_dir);
    if ids.is_empty() {
        return Err(EngineError::Conflict(
            "the run has no host grade to grade again: its grading is open".into(),
        ));
    }
    if reopened(run_dir, &ids) {
        return Ok(());
    }
    let marker = Reopened {
        over: ids,
        at: apb_core::clock::now_ms_u64(),
        by: by.to_string(),
    };
    let json =
        serde_json::to_vec_pretty(&marker).map_err(|e| EngineError::Invalid(e.to_string()))?;
    apb_core::fsutil::atomic_write_private(&grading_dir(run_dir).join(REOPENED_FILE), &json)?;
    Ok(())
}

/// Opens a grading session over the ended run at `run_dir` for its
/// playbook's own criteria, with a reading budget of `budget_chars`
/// ([`DEFAULT_BUDGET_CHARS`] when `None`). Refused while another session
/// of the run is open.
pub fn start_session(
    run_dir: &Path,
    budget_chars: Option<u64>,
) -> Result<SessionStart, EngineError> {
    let _held = lock(run_dir)?;
    let run = open_run(run_dir)?;
    let (sources, defs) = (&run.sources, &run.defs);
    if defs.is_empty() {
        return Err(EngineError::Invalid(
            "the playbook declares no criteria of its own; the built-ins are graded by rules"
                .into(),
        ));
    }
    if let Some(grade_id) = host_graded(run_dir) {
        return Err(EngineError::Conflict(format!(
            "the run has a host grade already (`{grade_id}`); a person grades it again by reopening its grading: apb grade {} --reopen, or Grade again on the run page",
            sources.run_id()
        )));
    }
    let now = apb_core::clock::now_ms_u64();
    if let Ok(entries) = std::fs::read_dir(grading_dir(run_dir)) {
        for e in entries.filter_map(Result::ok) {
            if e.file_name() == REOPENED_FILE {
                continue;
            }
            let open = std::fs::read(e.path())
                .ok()
                .and_then(|raw| serde_json::from_slice::<SessionFile>(&raw).ok())
                .filter(|f| f.open_at(now));
            if let Some(f) = open {
                return Err(EngineError::Conflict(format!(
                    "grading session `{}` of this run is open until {}",
                    f.session,
                    apb_core::clock::iso_utc(f.expires_at)
                )));
            }
        }
    }
    let redactor = Redactor::default();
    let mut reader = Reader::new(
        sources,
        &redactor,
        ReadState::new(budget_chars.unwrap_or(DEFAULT_BUDGET_CHARS)),
    );
    let outline = reader.outline();
    let file = SessionFile {
        session: uuid::Uuid::new_v4().simple().to_string()[..12].to_string(),
        run_id: sources.run_id().to_string(),
        created_at: now,
        expires_at: now + SESSION_TTL_MS,
        criteria: defs.iter().map(|d| d.id().to_string()).collect(),
        state: reader.state,
        round: None,
        closed: None,
    };
    save(run_dir, &file)?;
    Ok(SessionStart {
        session: file.session,
        expires_at: file.expires_at,
        criteria: defs.iter().map(CriterionBrief::of).collect(),
        outline,
    })
}

/// One reading step of session `session`.
pub fn read(run_dir: &Path, session: &str, req: &ReadRequest) -> Result<ReadResponse, EngineError> {
    let _held = lock(run_dir)?;
    let mut file = load(run_dir, session)?;
    let run = open_run(run_dir)?;
    // The sources are redacted already: the reader shows them as they are.
    let redactor = Redactor::default();
    let mut reader = Reader::new(&run.sources, &redactor, std::mem::take(&mut file.state));
    let answer = reader.handle(req);
    file.state = reader.state;
    save(run_dir, &file)?;
    answer
}

/// Submits the host's levels for session `session`. An accepted round is
/// stored as a `host/1` grade and closes the session.
pub fn submit(
    run_dir: &Path,
    session: &str,
    proposals: &[Proposal],
) -> Result<SubmitOutcome, EngineError> {
    let _held = lock(run_dir)?;
    let mut file = load(run_dir, session)?;
    let run = open_run(run_dir)?;
    let (sources, defs) = (&run.sources, &run.defs);
    let redactor = super::grader::workspace_redactor(run_dir);
    let proposals: Vec<Proposal> = proposals
        .iter()
        .cloned()
        .map(|mut p| {
            p.why = redactor.redact(&p.why);
            p
        })
        .collect();
    match validate(&proposals, defs, sources, &file.state, file.round.as_ref()) {
        Validated::Rejected(problems) => {
            file.round = Some(Round::of(&proposals, &file.state));
            save(run_dir, &file)?;
            Ok(SubmitOutcome::Rejected { problems })
        }
        Validated::Accepted(mut criteria) => {
            // A host's "does not apply" holds a candidate back at most: it
            // never stands in for a comparable grade.
            for c in &mut criteria {
                if c.level == Level::NotApplicable {
                    c.counts_for_promotion = false;
                }
            }
            let grade =
                super::grader::assemble(sources, GraderId::HOST, criteria, &file.state, Vec::new());
            super::store::write(run_dir, &grade)?;
            file.closed = Some(grade.grade_id.clone());
            save(run_dir, &file)?;
            Ok(SubmitOutcome::Graded {
                grade: Box::new(grade),
            })
        }
    }
}
