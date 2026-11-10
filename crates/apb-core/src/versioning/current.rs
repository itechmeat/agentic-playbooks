//! The one operation that moves the `current` pointer of a playbook.
//!
//! Every writer of `current` goes through [`move_current`] (or
//! [`move_current_locked`] under a lock it already holds): a person on the
//! CLI or the dashboard, a candidate promotion, an applied patch's
//! promotion, a new version made current, a schema migration and a copy
//! into another scope. The candidate lock (`.candidate.lock`) guards
//! `current`, the candidate pointer, the trial records and the history
//! together.
//!
//! Under the lock the operation writes in a fixed order, so a crash leaves
//! a state [`recover`] can finish or undo:
//!
//! 1. refuse in a read-only process, then recover an earlier intent;
//! 2. check: frozen (a migration and a scope copy are exempt), the target
//!    holds a `playbook.yaml`, the caller's expectation of `current`, a
//!    person never moves to the candidate, and every trial record the move
//!    updates reads;
//! 3. write the intent `current.intent.json`, then, for
//!    [`move_current_writing_locked`] only, the target version itself
//!    (shipped content apb installs), so the version and the move are one
//!    operation;
//! 4. end the trial of a candidate the move passes by (`superseded`);
//! 5. write `current`;
//! 6. when the target is the candidate: clear the pointer and record its
//!    trial as `promoted`;
//! 7. append the history line to `current-history.jsonl`;
//! 8. delete the intent.
//!
//! A failure before step 5 is an error, and the next recovery records the
//! intent as aborted. A failure after step 5 is a move that happened: the
//! answer says `audit_pending`, and the next recovery rolls the rest
//! forward. Recovery tells the two apart by the value of `current` alone.
//!
//! The history audits itself by value: every line names the sha256 of the
//! line before it, a move starts where the previous one ended, and
//! `current` holds the last recorded target. File times and inodes are
//! never read, so a clone or a copy of the project is not a gap. A series
//! of untracked writes that ends on the recorded value is not seen.

use std::io;
use std::path::{Path, PathBuf};

use serde::Serialize;

use super::error::VersioningError;
use super::sidecar::{
    check_trial_record, read_current_pointer, read_provenance_in, supersede_candidate_locked,
    update_trial_in, written_digest,
};
use crate::candidate_pointer::{
    CANDIDATE_LOCK_FILE, OUTCOME_PROMOTED, clear_candidate_if, read_candidate,
};
use crate::fsutil::{DirLock, atomic_write, lock_dir};
use crate::registry::{is_frozen_dir, is_safe_segment};

pub(crate) mod fault;
mod history;
mod intent;
mod model;
#[cfg(test)]
mod tests;

pub use history::HISTORY_FILE;
pub use intent::INTENT_FILE;
pub use model::{EntryKind, History, HistoryEntry, MovedBy, Surface, TrialFinal};

use fault::Step;
use intent::{FinishCandidate, Intent};

/// The pointer file this module moves.
pub const CURRENT_FILE: &str = "current";
/// The longest reason a move keeps, in characters (after the scrub).
pub const REASON_MAX: usize = 256;

/// One move of `current`.
#[derive(Debug, Clone)]
pub struct MoveCurrent<'a> {
    /// The version `current` names afterwards.
    pub to: &'a str,
    /// What the caller saw in `current`, `Some(None)` when it was unset; a
    /// different value refuses the move ([`MoveError::Stale`]). `None`
    /// moves whatever it names.
    pub expected_from: Option<Option<&'a str>>,
    pub by: MovedBy,
    /// A person's free text: scrubbed and cut to [`REASON_MAX`] before it
    /// is written anywhere.
    pub reason: Option<&'a str>,
    /// The trial record to finish when `to` is the candidate. A person's
    /// move without one never targets the candidate
    /// ([`MoveError::IsCandidate`]).
    pub trial: Option<TrialFinal>,
}

impl<'a> MoveCurrent<'a> {
    /// A move to `to` by `by`, with no expectation, reason or trial.
    pub fn new(to: &'a str, by: MovedBy) -> Self {
        MoveCurrent {
            to,
            expected_from: None,
            by,
            reason: None,
            trial: None,
        }
    }

    /// A new playbook or a version saved as current.
    pub fn created(to: &'a str) -> Self {
        Self::new(to, MovedBy::Created)
    }
}

/// What a move did.
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Moved {
    pub change_id: String,
    pub from: Option<String>,
    pub to: String,
    /// The candidate whose trial the move ended.
    pub superseded_candidate: Option<String>,
    /// The new current version was imported with scripts nobody approved:
    /// starts stay refused until a person approves it. Moving grants
    /// nothing.
    pub awaiting_approval: bool,
    /// `current` moved, but its follow-up (the candidate's trial record,
    /// the history line) did not finish; the next recovery finishes it.
    pub audit_pending: bool,
}

/// Why `current` did not move.
#[derive(Debug, thiserror::Error)]
pub enum MoveError {
    #[error("the playbook is frozen: unfreeze it to change the current version")]
    Frozen,
    #[error("{0} not found")]
    NotFound(String),
    #[error("the current version changed meanwhile: it is now {}", .actual.as_deref().unwrap_or("unset"))]
    Stale { actual: Option<String> },
    #[error(
        "{version} is the candidate on trial: decide the candidate instead (apb candidate --promote, or the candidate card of the dashboard)"
    )]
    IsCandidate { version: String },
    /// The version a current-run patch of a run that processed external
    /// data created: it stays with that run.
    #[error(
        "external_data_run: {version} was made by a patch of run {run}, which processed external data; it never becomes current"
    )]
    ExternalDataRun { version: String, run: String },
    #[error("{}", crate::process_mode::READ_ONLY_REFUSAL)]
    ReadOnly,
    /// A trial record the move must update does not read: fix or remove
    /// it by hand. Nothing moved.
    #[error("{0}")]
    Record(String),
    #[error(transparent)]
    Io(#[from] io::Error),
}

impl MoveError {
    /// The machine-readable code every surface answers with.
    pub fn code(&self) -> &'static str {
        match self {
            MoveError::Frozen => "frozen",
            MoveError::NotFound(_) => "not_found",
            MoveError::Stale { .. } => "stale",
            MoveError::IsCandidate { .. } => "is_candidate",
            MoveError::ExternalDataRun { .. } => "external_data_run",
            MoveError::ReadOnly => "read_only",
            MoveError::Record(_) => "record_unreadable",
            MoveError::Io(_) => "io",
        }
    }

    /// The error as an `io::Error`, for a caller whose errors are I/O.
    pub fn into_io(self) -> io::Error {
        match self {
            MoveError::Io(e) => e,
            MoveError::ReadOnly => crate::process_mode::refusal(),
            other => io::Error::other(other.to_string()),
        }
    }

    /// The error of a versioning call that moved `current` for playbook
    /// `id`.
    pub fn into_versioning(self, id: &str) -> VersioningError {
        match self {
            MoveError::Frozen => VersioningError::Frozen(id.to_string()),
            MoveError::NotFound(what) => VersioningError::NotFound(what),
            MoveError::ReadOnly => VersioningError::Io(crate::process_mode::refusal()),
            MoveError::Io(e) => VersioningError::Io(e),
            MoveError::Record(why) => VersioningError::Schema(why),
            e @ (MoveError::Stale { .. }
            | MoveError::IsCandidate { .. }
            | MoveError::ExternalDataRun { .. }) => VersioningError::Conflict(e.to_string()),
        }
    }
}

impl From<VersioningError> for MoveError {
    fn from(e: VersioningError) -> Self {
        match e {
            VersioningError::NotFound(what) => MoveError::NotFound(what),
            VersioningError::Frozen(_) => MoveError::Frozen,
            VersioningError::Io(e) => MoveError::Io(e),
            other => MoveError::Io(io::Error::other(other.to_string())),
        }
    }
}

/// The pointer lock of the playbook in `playbook_dir`.
pub fn lock(playbook_dir: &Path) -> io::Result<DirLock> {
    lock_dir(playbook_dir, CANDIDATE_LOCK_FILE)
}

/// [`move_current_locked`] under the playbook's pointer lock.
pub fn move_current(playbook_dir: &Path, req: &MoveCurrent<'_>) -> Result<Moved, MoveError> {
    refuse_read_only()?;
    if !playbook_dir.is_dir() {
        return Err(MoveError::NotFound(id_of(playbook_dir)));
    }
    let held = lock(playbook_dir)?;
    move_current_locked(playbook_dir, req, &held)
}

/// Moves `current` of the playbook in `playbook_dir` as `req` says, in the
/// order the module documents. The caller holds the pointer lock (`held`).
pub fn move_current_locked(
    playbook_dir: &Path,
    req: &MoveCurrent<'_>,
    held: &DirLock,
) -> Result<Moved, MoveError> {
    run(playbook_dir, req, held, None)
}

/// [`move_current_locked`] to a version that does not exist yet: `write`
/// creates `req.to` after the intent is written and before `current`
/// moves. A crash before `current` moves leaves an intent the next
/// recovery records as aborted (the written version stays, named by
/// nothing); a crash after it is rolled forward. The caller holds the
/// pointer lock (`held`).
pub fn move_current_writing_locked(
    playbook_dir: &Path,
    req: &MoveCurrent<'_>,
    held: &DirLock,
    write: &mut dyn FnMut() -> Result<(), MoveError>,
) -> Result<Moved, MoveError> {
    run(playbook_dir, req, held, Some(write))
}

fn run(
    playbook_dir: &Path,
    req: &MoveCurrent<'_>,
    held: &DirLock,
    write: Option<&mut dyn FnMut() -> Result<(), MoveError>>,
) -> Result<Moved, MoveError> {
    refuse_read_only()?;
    recover_locked(playbook_dir, held)?;
    let intent = plan(playbook_dir, req, write.is_some())?;
    intent::write(playbook_dir, &intent)?;
    fault::after(Step::Intent)?;
    if let Some(write) = write {
        write()?;
        fault::after(Step::Target)?;
        if !playbook_dir
            .join(&intent.to)
            .join("playbook.yaml")
            .is_file()
        {
            return Err(MoveError::NotFound(format!(
                "{}@{}",
                id_of(playbook_dir),
                intent.to
            )));
        }
    }
    if intent.supersede.is_some() {
        supersede_candidate_locked(
            playbook_dir,
            Some(&intent.to),
            &format!("`current` moved to {}", intent.to),
            held,
        )?;
        fault::after(Step::Supersede)?;
    }
    atomic_write(&playbook_dir.join(CURRENT_FILE), intent.to.as_bytes())?;
    // From here on the move happened: a failure leaves its record pending.
    let finished = fault::after(Step::Current)
        .map_err(MoveError::from)
        .and_then(|()| roll_forward(playbook_dir, &intent));
    let audit_pending = match finished {
        Ok(()) => false,
        Err(e) => {
            eprintln!(
                "apb: `current` moved to {}, but its record did not finish ({e}); the next change finishes it",
                intent.to
            );
            true
        }
    };
    Ok(Moved {
        awaiting_approval: awaits_approval(playbook_dir, &intent.to),
        change_id: intent.change_id,
        from: intent.from,
        to: intent.to,
        superseded_candidate: intent.supersede,
        audit_pending,
    })
}

/// Finishes or undoes a move a crash interrupted: with `current` at the
/// intent's target, the rest is rolled forward; otherwise the intent is
/// recorded as aborted. The caller holds the pointer lock (`held`).
pub fn recover(playbook_dir: &Path, held: &DirLock) -> Result<(), MoveError> {
    refuse_read_only()?;
    recover_locked(playbook_dir, held)
}

/// Whether an interrupted move waits for [`recover`].
pub fn intent_pending(playbook_dir: &Path) -> bool {
    playbook_dir.join(INTENT_FILE).exists()
}

/// The newest `limit` history lines of the playbook in `playbook_dir`,
/// newest first, with the audit. Outside a read-only process an
/// interrupted move is recovered first; a read-only process only reports
/// it (`pending`).
pub fn history(playbook_dir: &Path, limit: usize) -> Result<History, MoveError> {
    if !playbook_dir.is_dir() {
        return Err(MoveError::NotFound(id_of(playbook_dir)));
    }
    // Under the lock, a move in flight cannot show between the reads as a
    // gap. A read-only process cannot take it and reads as it stands, and
    // so does a reader that cannot create the lock (a directory it may not
    // write) while no interrupted move needs recovering.
    let _held = if crate::process_mode::is_read_only() {
        None
    } else {
        match lock(playbook_dir) {
            Ok(held) => {
                recover_locked(playbook_dir, &held)?;
                Some(held)
            }
            Err(_) if !intent_pending(playbook_dir) => None,
            Err(e) => return Err(e.into()),
        }
    };
    let pending = intent_pending(playbook_dir);
    let pointers = history::Pointers {
        current: read_current_pointer(playbook_dir),
        pending,
        pending_to: pending
            .then(|| intent::read(playbook_dir).ok().flatten().map(|i| i.to))
            .flatten(),
    };
    Ok(history::read(playbook_dir, limit, pointers)?)
}

/// `current` and its newest move as [`recover`] leaves them, read without
/// the lock and without writing anything.
#[derive(Debug, Clone, PartialEq)]
pub struct Settled {
    /// The version `current` names.
    pub current: Option<String>,
    /// Who made the newest move (aborted lines skipped).
    pub last_move_by: Option<MovedBy>,
    /// An interrupted move waits for [`recover`]; the other fields already
    /// say what recovering it settles.
    pub pending: bool,
}

/// `current` and its newest move of the playbook in `playbook_dir`, with an
/// interrupted move projected the way [`recover`] settles it: rolled
/// forward when `current` is at its target, else aborted. Nothing is
/// written, so a read-only check (`apb setup plan`, `apb doctor`) never
/// changes the store; under the pointer lock after [`recover`] it is
/// exact. An intent that does not read is an error, as for [`recover`].
pub fn settled(playbook_dir: &Path) -> io::Result<Settled> {
    let current = read_current_pointer(playbook_dir);
    let pending = intent::read(playbook_dir)?;
    let rolled_forward = pending
        .as_ref()
        .filter(|i| current.as_deref() == Some(i.to.as_str()));
    let last_move_by = match rolled_forward {
        Some(i) => Some(i.by.clone()),
        None => history::last_move(playbook_dir)?.and_then(|e| e.by),
    };
    Ok(Settled {
        current,
        last_move_by,
        pending: pending.is_some(),
    })
}

fn refuse_read_only() -> Result<(), MoveError> {
    if crate::process_mode::is_read_only() {
        return Err(MoveError::ReadOnly);
    }
    Ok(())
}

fn recover_locked(playbook_dir: &Path, _held: &DirLock) -> Result<(), MoveError> {
    let Some(intent) = intent::read(playbook_dir)? else {
        return Ok(());
    };
    if read_current_pointer(playbook_dir).as_deref() == Some(intent.to.as_str()) {
        return roll_forward(playbook_dir, &intent);
    }
    // `current` never moved: the candidate the intent meant to pass by
    // counts as superseded only when its pointer is gone.
    let superseded = intent
        .supersede
        .clone()
        .filter(|c| read_candidate(playbook_dir).as_deref() != Some(c.as_str()));
    history::append_once(
        playbook_dir,
        &HistoryEntry::aborted(&intent.change_id, superseded),
    )?;
    intent::remove(playbook_dir)?;
    Ok(())
}

/// Steps 6 to 8: everything after `current`. Each step is idempotent.
fn roll_forward(playbook_dir: &Path, intent: &Intent) -> Result<(), MoveError> {
    if let Some(finish) = &intent.finish_candidate {
        // A move only ever promotes its own target: an intent that names
        // another was edited, and finishing it would end a trial nobody
        // decided.
        if finish.version != intent.to {
            return Err(MoveError::Io(io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "{INTENT_FILE} promotes {} but moves `current` to {}: check `current` and remove it by hand",
                    finish.version, intent.to
                ),
            )));
        }
        finish_candidate(playbook_dir, finish)?;
    }
    history::append_once(playbook_dir, &intent.history_line)?;
    fault::after(Step::History)?;
    intent::remove(playbook_dir)?;
    fault::after(Step::IntentRemoval)?;
    Ok(())
}

fn finish_candidate(playbook_dir: &Path, finish: &FinishCandidate) -> Result<(), MoveError> {
    clear_candidate_if(playbook_dir, &finish.version)?;
    fault::after(Step::PointerClear)?;
    update_trial_in(playbook_dir, &finish.version, |t| {
        t.outcome = Some(OUTCOME_PROMOTED.to_string());
        t.run_id = finish.trial.run_id.clone().or(t.run_id.take());
        if let Some(q) = &finish.trial.quality {
            t.quality = Some(q.clone());
        }
    })?;
    fault::after(Step::TrialRecord)?;
    Ok(())
}

/// Step 2: the checks, and the intent the move writes. With `writes_target`
/// the caller writes the target inside the operation: it must not exist
/// yet.
fn plan(
    playbook_dir: &Path,
    req: &MoveCurrent<'_>,
    writes_target: bool,
) -> Result<Intent, MoveError> {
    let id = id_of(playbook_dir);
    if !playbook_dir.is_dir() {
        return Err(MoveError::NotFound(id));
    }
    if req.by.respects_frozen() && is_frozen_dir(playbook_dir) {
        return Err(MoveError::Frozen);
    }
    if !is_safe_segment(req.to) {
        return Err(MoveError::NotFound(format!("{id}@{}", req.to)));
    }
    let target = playbook_dir.join(req.to);
    if writes_target && target.exists() {
        return Err(MoveError::Io(io::Error::new(
            io::ErrorKind::AlreadyExists,
            format!("{id}@{} exists already", req.to),
        )));
    }
    if !writes_target && !target.join("playbook.yaml").is_file() {
        return Err(MoveError::NotFound(format!("{id}@{}", req.to)));
    }
    if let Some(run) = crate::versioning::sidecar::read_provenance_in(playbook_dir, req.to)
        .and_then(|p| p.external_data_run)
    {
        return Err(MoveError::ExternalDataRun {
            version: req.to.to_string(),
            run,
        });
    }
    let from = read_current_pointer(playbook_dir);
    if let Some(expected) = req.expected_from
        && from.as_deref() != expected
    {
        return Err(MoveError::Stale { actual: from });
    }
    let candidate = read_candidate(playbook_dir);
    let to_candidate = candidate.as_deref() == Some(req.to);
    if to_candidate && req.trial.is_none() && matches!(req.by, MovedBy::Person { .. }) {
        return Err(MoveError::IsCandidate {
            version: req.to.to_string(),
        });
    }
    // A candidate stays on trial while `current` keeps its value: its
    // lineage still reaches it.
    let supersede = candidate.filter(|c| c != req.to && from.as_deref() != Some(req.to));
    // Every trial record the move updates must read before its first write:
    // a refusal then leaves the candidate exactly as it was.
    for version in supersede
        .iter()
        .map(String::as_str)
        .chain(to_candidate.then_some(req.to))
    {
        check_trial_record(playbook_dir, version).map_err(|e| match e {
            VersioningError::Schema(why) => MoveError::Record(why),
            other => other.into(),
        })?;
    }
    let change_id = uuid::Uuid::new_v4().simple().to_string();
    let history_line = HistoryEntry::moved(model::MoveLine {
        change_id: change_id.clone(),
        from: from.clone(),
        to: req.to.to_string(),
        by: req.by.clone(),
        reason: stored_reason(playbook_dir, req.reason),
        superseded_candidate: supersede.clone(),
    });
    Ok(Intent {
        change_id,
        from,
        to: req.to.to_string(),
        by: req.by.clone(),
        supersede,
        finish_candidate: to_candidate.then(|| FinishCandidate {
            version: req.to.to_string(),
            trial: req.trial.clone().unwrap_or_default(),
        }),
        history_line,
    })
}

/// A person's reason as it is kept: through the workspace scrubber
/// (connector secrets, paths, addresses, token shapes), then cut to
/// [`REASON_MAX`]. Dropped when nothing is left or a secret still shows.
fn stored_reason(playbook_dir: &Path, reason: Option<&str>) -> Option<String> {
    let reason = reason.map(str::trim).filter(|r| !r.is_empty())?;
    crate::scrub::Scrubber::for_workspace(&workspace_root(playbook_dir), Vec::new())
        .stored_text(reason, REASON_MAX)
}

/// The workspace a playbook directory belongs to: the project root for
/// `<root>/.apb/playbooks/<id>`, else the store's parent (the global
/// store).
fn workspace_root(playbook_dir: &Path) -> PathBuf {
    let store = playbook_dir
        .parent()
        .and_then(Path::parent)
        .unwrap_or(playbook_dir);
    match store.file_name() {
        Some(name) if name == ".apb" => store.parent().unwrap_or(store).to_path_buf(),
        _ => store.to_path_buf(),
    }
}

/// Whether `version` was imported with scripts and its content now is not
/// approved: the same rule the run gate applies.
fn awaits_approval(playbook_dir: &Path, version: &str) -> bool {
    let imported_scripts = read_provenance_in(playbook_dir, version)
        .and_then(|p| p.imported)
        .is_some_and(|i| !i.scripts.is_empty());
    imported_scripts
        && !written_digest(&playbook_dir.join(version))
            .is_ok_and(|d| crate::trust::TrustStore::load().is_approved(&d))
}

fn id_of(playbook_dir: &Path) -> String {
    playbook_dir
        .file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .into_owned()
}
