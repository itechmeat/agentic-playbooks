//! The start key (routine operations, D1), engine side: the read-only
//! [`lookup`] every launch surface asks before its gate, and the [`Claim`]
//! preparation takes before the tree lock and holds until its run is
//! verifiable.
//!
//! An entry has an owner while it is not verifiable (`pending`): the
//! preparation that holds the `flock` lock of its reservation file. A
//! reader decides by the owner, never by age: owner alive with a run id is
//! that run (preparing); owner alive without one is busy for a moment;
//! owner gone, the entry counts as bound when its run directory passes the
//! run check, else it is void. Every read and write goes through the
//! signed store's transaction under its lock, and a file that does not
//! verify refuses every keyed start of the playbook
//! (`start_key_state_unreadable`) instead of reading as empty.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use apb_core::fsutil::HeldLock;
use apb_core::scope::Scope;
use apb_core::signed_store::{self, StoreError, Unsigned};
use apb_core::start_key::{EntryState, StartKeyEntry, StartKeyRequest, StartKeysFile};
use apb_core::step_env::StepCaller;

use crate::error::EngineError;
use crate::event::EventPayload;
use crate::state::RunStatus;

/// The refusal code of another request under a key.
pub const CONFLICT: &str = "start_key_conflict";
/// The refusal code of a reuse the run's state does not allow.
pub const REUSE_REFUSED: &str = "start_key_reuse_refused";
/// The answer while another start with the key is being prepared.
pub const BUSY: &str = "start_key_busy";
/// The refusal code of a key file that does not verify.
pub const UNREADABLE: &str = "start_key_state_unreadable";

/// How long a start waits for another start with its key to allocate
/// its run before it answers [`BUSY`].
const BUSY_WAIT: Duration = Duration::from_secs(2);
const BUSY_STEP: Duration = Duration::from_millis(50);

/// What a key says about a start.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Lookup {
    /// No live entry, or a reuse the run allows: the start goes ahead.
    Free,
    /// The same request started this run; nothing starts.
    Started { run_id: String, status: RunStatus },
    /// The key names another request.
    Conflict { run_id: String },
}

/// One entry as a reader judges it.
enum Judged {
    /// Void, expired or gone: as if absent.
    Void,
    /// Its owner is alive and has no run yet.
    Busy,
    /// Its run, verified (or preparing under a live owner).
    Run { run_id: String, status: RunStatus },
}

fn scope_of(label: &str) -> Scope {
    Scope::parse(label).unwrap_or(Scope::Project)
}

/// The days a key is remembered in `root` (`runs.start_key_days`); the
/// default when the settings do not read.
fn days(root: &Path) -> u32 {
    apb_core::policy_file::Policies::load(root)
        .map(|p| p.start_key_days())
        .unwrap_or(apb_core::start_key::DEFAULT_DAYS)
}

fn now_ms() -> u64 {
    apb_core::clock::now_ms_u64()
}

/// The directory of the reservation lock files of `id`.
fn pending_dir(root: &Path, scope: Scope, id: &str) -> PathBuf {
    signed_store::dir(root, <StartKeysFile as signed_store::SignedDoc>::DIR, scope)
        .join(format!("{id}.pending"))
}

fn lock_name(reservation: &str) -> String {
    format!("{reservation}.lock")
}

fn store_error(root: &Path, scope: Scope, id: &str, e: StoreError) -> EngineError {
    match e {
        StoreError::Unverified(path) => EngineError::Conflict(format!(
            "{UNREADABLE}: {} does not verify (it was changed outside apb or written by another installation); move it aside to start again with keys",
            path.display()
        )),
        other => EngineError::Invalid(format!(
            "{UNREADABLE}: {}: {other}",
            signed_store::file::<StartKeysFile>(root, scope, id).display()
        )),
    }
}

/// The run check: the directory is a real one this installation created,
/// its `run_started` names playbook `id`, and its provenance names the
/// scope, or it has none and its journal ended (a preparation that failed
/// before provenance). The run's reported status when it passes.
fn checked_run(root: &Path, scope: Scope, id: &str, run_id: &str) -> Option<RunStatus> {
    if !apb_core::registry::is_safe_segment(run_id) {
        return None;
    }
    let dir = root.join(".apb/runs").join(run_id);
    let meta = std::fs::symlink_metadata(&dir).ok()?;
    if meta.file_type().is_symlink() || !meta.is_dir() {
        return None;
    }
    if !apb_core::run_origin::verify(&dir, run_id) {
        return None;
    }
    let events = crate::event::read_all_lossy_tail(&dir).ok()?;
    let started = events
        .iter()
        .any(|e| matches!(&e.payload, EventPayload::RunStarted { playbook, .. } if playbook == id));
    if !started {
        return None;
    }
    let origin = events.iter().find_map(|e| match &e.payload {
        EventPayload::RunProvenance { origin, .. } => Some(origin.clone()),
        _ => None,
    });
    let ok = match origin {
        Some(o) => o.as_deref().is_none_or(|o| o == scope.as_str()),
        None => matches!(
            events.last().map(|e| &e.payload),
            Some(EventPayload::RunFinished { .. })
        ),
    };
    ok.then(|| {
        crate::liveness::reported_run_status(
            &events,
            false,
            crate::liveness::driver_alive(&dir, run_id),
        )
    })
}

/// How a reader judges `e`, under the store lock.
fn judge(root: &Path, scope: Scope, id: &str, e: &StartKeyEntry) -> Judged {
    if e.state == EntryState::Pending
        && let Some(res) = e.reservation.as_deref()
    {
        let dir = pending_dir(root, scope, id);
        let probe = apb_core::fsutil::try_hold_lock(&dir, &lock_name(res));
        if let Ok(gone) = probe {
            // Nobody holds it: the probe's own file goes with it.
            let _ = std::fs::remove_file(dir.join(lock_name(res)));
            drop(gone);
        } else {
            // The owner is alive: its run, preparing, or a moment of busy.
            return match &e.run_id {
                Some(run_id) => Judged::Run {
                    run_id: run_id.clone(),
                    status: RunStatus::Running,
                },
                None => Judged::Busy,
            };
        }
    }
    match e.run_id.as_deref() {
        Some(run_id) => match checked_run(root, scope, id, run_id) {
            Some(status) => Judged::Run {
                run_id: run_id.to_string(),
                status,
            },
            None => Judged::Void,
        },
        None => Judged::Void,
    }
}

/// The answer for `req` given the judged live entry of its key, or
/// `None` while it is busy.
fn answer(
    e: &StartKeyEntry,
    judged: Judged,
    req: &StartKeyRequest,
    step: &StepCaller,
) -> Result<Option<Lookup>, EngineError> {
    let (run_id, status) = match judged {
        Judged::Void => return Ok(Some(Lookup::Free)),
        Judged::Busy => return Ok(None),
        Judged::Run { run_id, status } => (run_id, status),
    };
    if req.reuse {
        if let Some(why) = step.refusal("reusing a start key") {
            return Err(EngineError::Conflict(format!("{REUSE_REFUSED}: {why}")));
        }
        return match status {
            RunStatus::Failed | RunStatus::Aborted => Ok(Some(Lookup::Free)),
            other => Err(EngineError::Conflict(format!(
                "{REUSE_REFUSED}: run `{run_id}` is {}; a key is reused only after a failed or aborted run",
                other.as_str()
            ))),
        };
    }
    if e.request == req.request.tag() {
        Ok(Some(Lookup::Started { run_id, status }))
    } else {
        Ok(Some(Lookup::Conflict { run_id }))
    }
}

/// Retries `f` while it reports busy, up to [`BUSY_WAIT`], then answers
/// [`BUSY`].
fn while_busy<T>(mut f: impl FnMut() -> Result<Option<T>, EngineError>) -> Result<T, EngineError> {
    let until = Instant::now() + BUSY_WAIT;
    loop {
        if let Some(v) = f()? {
            return Ok(v);
        }
        if Instant::now() >= until {
            return Err(EngineError::Conflict(format!(
                "{BUSY}: another start with this key is being prepared; try again"
            )));
        }
        std::thread::sleep(BUSY_STEP);
    }
}

/// The read-only answer for a launch surface, before any gate or prompt.
/// A run step's reuse is refused here as at the claim.
pub fn lookup(
    root: &Path,
    scope_label: &str,
    id: &str,
    req: &StartKeyRequest,
) -> Result<Lookup, EngineError> {
    let scope = scope_of(scope_label);
    let key = apb_core::start_key::key_tag(&req.key).map_err(|e| key_error(&e))?;
    let days = days(root);
    let step = StepCaller::from_env();
    while_busy(|| {
        signed_store::transact(
            root,
            scope,
            id,
            Unsigned::Refuse,
            |f: &mut StartKeysFile| -> Result<(Option<Lookup>, bool), KeyError> {
                let Some(e) = f.find(&key, now_ms(), days) else {
                    return Ok((Some(Lookup::Free), false));
                };
                let judged = judge(root, scope, id, e);
                Ok((answer(e, judged, req, &step)?, false))
            },
        )
        .map_err(|e| unwrap_store(root, scope, id, e))
    })
}

fn key_error(e: &std::io::Error) -> EngineError {
    EngineError::Invalid(format!(
        "{}: the key cannot be checked: {e}",
        apb_core::start_key::INVALID
    ))
}

/// An engine error a transaction returned, with a store error mapped.
fn unwrap_store(root: &Path, scope: Scope, id: &str, e: KeyError) -> EngineError {
    match e {
        KeyError::Store(s) => store_error(root, scope, id, s),
        KeyError::Engine(e) => e,
    }
}

/// The error of a transaction over the key file.
#[derive(Debug)]
enum KeyError {
    Store(StoreError),
    Engine(EngineError),
}

impl From<StoreError> for KeyError {
    fn from(e: StoreError) -> Self {
        KeyError::Store(e)
    }
}

impl From<EngineError> for KeyError {
    fn from(e: EngineError) -> Self {
        KeyError::Engine(e)
    }
}

/// The pending reservation preparation owns from before the tree lock
/// until its run is verifiable.
pub struct Claim {
    owner: Option<HeldLock>,
    root: PathBuf,
    scope: Scope,
    id: String,
    reservation: String,
    bound: bool,
}

/// Claims `req`'s key for a new run of `id`: `AlreadyStarted` when the
/// same request started a run, a conflict for another request, the busy
/// answer while another start with the key has no run yet; otherwise a
/// pending entry this process owns until [`Claim::bind`].
pub fn claim(
    root: &Path,
    scope_label: &str,
    id: &str,
    req: &StartKeyRequest,
    step: &StepCaller,
) -> Result<Claim, EngineError> {
    let scope = scope_of(scope_label);
    let key = apb_core::start_key::key_tag(&req.key).map_err(|e| key_error(&e))?;
    let days = days(root);
    let pending = pending_dir(root, scope, id);
    while_busy(|| {
        signed_store::transact(
            root,
            scope,
            id,
            Unsigned::Refuse,
            |f: &mut StartKeysFile| -> Result<(Option<Claim>, bool), KeyError> {
                let now = now_ms();
                f.expire(now, days);
                if let Some(e) = f.find(&key, now, days) {
                    let judged = judge(root, scope, id, e);
                    match answer(e, judged, req, step)? {
                        None => return Ok((None, false)),
                        Some(Lookup::Started { run_id, .. }) => {
                            return Err(EngineError::AlreadyStarted(run_id).into());
                        }
                        Some(Lookup::Conflict { run_id }) => {
                            return Err(EngineError::Conflict(format!(
                                "{CONFLICT}: the key names run `{run_id}`, started with other params or instruction; use another key"
                            ))
                            .into());
                        }
                        Some(Lookup::Free) => {}
                    }
                }
                let reused = f
                    .entries
                    .iter()
                    .find(|e| e.key == key)
                    .map(|e| e.reused + u32::from(req.reuse))
                    .unwrap_or(0);
                f.entries.retain(|e| e.key != key);
                let reservation = apb_core::connector::marker::new_nonce();
                let owner = apb_core::fsutil::hold_lock(&pending, &lock_name(&reservation))
                    .map_err(StoreError::Io)?;
                f.entries.push(StartKeyEntry {
                    key: key.clone(),
                    request: req.request.tag().to_string(),
                    state: EntryState::Pending,
                    reservation: Some(reservation.clone()),
                    run_id: None,
                    at_ms: now,
                    reused,
                });
                Ok((
                    Some(Claim {
                        owner: Some(owner),
                        root: root.to_path_buf(),
                        scope,
                        id: id.to_string(),
                        reservation,
                        bound: false,
                    }),
                    true,
                ))
            },
        )
        .map_err(|e| unwrap_store(root, scope, id, e))
    })
}

impl Claim {
    /// Changes this claim's entry under the store lock.
    fn change(&self, f: impl FnOnce(&mut StartKeysFile, usize)) -> Result<(), EngineError> {
        signed_store::transact(
            &self.root,
            self.scope,
            &self.id,
            Unsigned::Refuse,
            |file: &mut StartKeysFile| -> Result<((), bool), KeyError> {
                let at = file
                    .entries
                    .iter()
                    .position(|e| e.reservation.as_deref() == Some(self.reservation.as_str()));
                match at {
                    Some(i) => {
                        f(file, i);
                        Ok(((), true))
                    }
                    None => Ok(((), false)),
                }
            },
        )
        .map_err(|e| unwrap_store(&self.root, self.scope, &self.id, e))
    }

    /// Records the run directory preparation just allocated.
    pub fn attach(&mut self, run_id: &str) -> Result<(), EngineError> {
        self.change(|f, i| f.entries[i].run_id = Some(run_id.to_string()))
    }

    /// The run is verifiable: the entry is bound, and the owner's lock is
    /// dropped and its file removed (every probe runs under the store
    /// lock, and the entry stops naming the reservation in the same
    /// transaction, so no later probe opens that path).
    pub fn bind(mut self) -> Result<(), EngineError> {
        self.change(|f, i| {
            f.entries[i].state = EntryState::Bound;
            f.entries[i].reservation = None;
        })?;
        self.bound = true;
        self.release();
        Ok(())
    }

    fn release(&mut self) {
        drop(self.owner.take());
        let _ = std::fs::remove_file(
            pending_dir(&self.root, self.scope, &self.id).join(lock_name(&self.reservation)),
        );
    }
}

impl Drop for Claim {
    /// A preparation that returned before `bind`: a run that has
    /// `run_started` (a failed start) holds the key; any other pending
    /// entry of this reservation is removed, so a start refused before it
    /// allocated anything leaves the key free. Best effort.
    fn drop(&mut self) {
        if self.bound {
            return;
        }
        let root = self.root.clone();
        let _ = self.change(|f, i| {
            let started = f.entries[i].run_id.as_deref().is_some_and(|run_id| {
                crate::run_history::started(&root.join(".apb/runs").join(run_id)).is_some()
            });
            if started {
                f.entries[i].state = EntryState::Bound;
                f.entries[i].reservation = None;
            } else {
                f.entries.remove(i);
            }
        });
        self.release();
    }
}

/// The `apb doctor` line of start-key files that do not verify under
/// `root`: `None` when every one reads (or there is none).
pub fn doctor_line(root: &Path) -> Option<String> {
    let mut bad = Vec::new();
    for scope in [Scope::Project, Scope::Global] {
        let dir = signed_store::dir(root, <StartKeysFile as signed_store::SignedDoc>::DIR, scope);
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for e in entries.filter_map(Result::ok) {
            let name = e.file_name().to_string_lossy().into_owned();
            let Some(id) = name.strip_suffix(".json") else {
                continue;
            };
            let readable =
                signed_store::read::<StartKeysFile>(root, scope, id).is_ok_and(|l| l.signed);
            if !readable {
                bad.push(e.path().display().to_string());
            }
        }
    }
    (!bad.is_empty()).then(|| {
        format!(
            "start-key files that do not verify (keyed starts of their playbooks are refused until each is moved aside): {}",
            bad.join(", ")
        )
    })
}

/// The first characters of the key's MAC a run's provenance shows.
pub fn shown_tag(req: &StartKeyRequest) -> Option<String> {
    apb_core::start_key::key_tag(&req.key).ok().map(|t| {
        t.chars()
            .take(apb_core::start_key::SHOWN_TAG_CHARS)
            .collect()
    })
}

/// Barriers the start-key race tests hold preparation at. Test builds
/// only.
#[cfg(any(test, feature = "testing"))]
pub mod test_hooks {
    use std::path::{Path, PathBuf};
    use std::sync::{Arc, Barrier, Mutex};

    static AFTER_ATTACH: Mutex<Vec<(PathBuf, Arc<Barrier>)>> = Mutex::new(Vec::new());

    /// Holds every preparation under `root` right after its claim's
    /// `attach` at the returned barrier (two waits: the hook arrives, then
    /// it leaves when the test waits again).
    pub fn hold_after_attach(root: &Path) -> Arc<Barrier> {
        let b = Arc::new(Barrier::new(2));
        AFTER_ATTACH
            .lock()
            .unwrap()
            .push((root.to_path_buf(), b.clone()));
        b
    }

    /// Removes the hook of `root`.
    pub fn clear(root: &Path) {
        AFTER_ATTACH.lock().unwrap().retain(|(r, _)| r != root);
    }

    pub(crate) fn after_attach(root: &Path) {
        let hit = AFTER_ATTACH
            .lock()
            .unwrap()
            .iter()
            .find(|(r, _)| r == root)
            .map(|(_, b)| b.clone());
        if let Some(b) = hit {
            b.wait();
            b.wait();
        }
    }
}
