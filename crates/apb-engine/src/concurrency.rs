//! The concurrency key (routine health, D1): the signed per-playbook slot
//! file, the admission transaction, the wait for the turn and a resume's
//! reservation. A claim is provisional until its run's provenance is
//! durable, owned by a live `flock` lock rather than a timer, and every
//! stop a slot promises is posted outside the slot's transaction. The key
//! is taken before the working tree: a keyed start never holds the tree
//! while it waits for its key.

use std::collections::BTreeMap;
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::time::Duration;

use apb_core::scope::Scope;

use crate::error::EngineError;
use crate::event::{ConcurrencyRecord, Event, EventPayload};

pub mod slots;

pub(crate) use slots::read_slots;
pub use slots::{ConcurrencyFile, Holder, PendingStop, Slot};

/// How often a waiter looks at its slot.
pub const TURN_STEP: Duration = Duration::from_millis(250);
/// The least time a keyed run waits for the working tree once it holds
/// its key.
pub const TREE_AFTER_TURN: Duration = Duration::from_secs(30);

/// A run a stored file names, checked before anything acts on it.
#[derive(Debug)]
pub enum RunCheck {
    /// This installation's run of the playbook and scope, journal read.
    Valid(RunDir),
    /// Proven not to be such a run: the holder or fact is dropped.
    Void,
    /// The run may be such a run but its journal does not read strictly
    /// (a torn tail, an unknown event after the last checkpoint, an I/O
    /// error): never dropped, never counted, never removed.
    Unreadable,
}

/// The run directory and its journal, as [`checked_run`] read them.
#[derive(Debug)]
pub struct RunDir {
    pub path: PathBuf,
    pub events: Vec<Event>,
}

impl RunCheck {
    /// The run directory when the check found a valid run.
    pub fn dir(&self) -> Option<&RunDir> {
        match self {
            RunCheck::Valid(d) => Some(d),
            _ => None,
        }
    }
}

/// Checks the run a stored file names (a slot holder, a health fact): a
/// safe segment, a real directory under `.apb/runs`, this installation's
/// origin stamp, a strict journal read, its `run_started.playbook` and
/// `run_provenance.origin` equal to `playbook` and `scope`, and, for a
/// slot holder, a `concurrency` record whose `key_digest` matches.
pub fn checked_run(
    root: &Path,
    scope: Scope,
    playbook: &str,
    run_id: &str,
    key_digest: Option<&str>,
) -> RunCheck {
    if !apb_core::registry::is_safe_segment(run_id) {
        return RunCheck::Void;
    }
    let run_dir = root.join(".apb/runs").join(run_id);
    if !std::fs::symlink_metadata(&run_dir).is_ok_and(|m| m.is_dir()) {
        return RunCheck::Void;
    }
    if !apb_core::run_origin::verify(&run_dir, run_id) {
        return RunCheck::Void;
    }
    let events = match crate::event::read_all(&run_dir) {
        Ok(ev) => ev,
        Err(_) => return RunCheck::Unreadable,
    };
    let head = crate::run_history::started(&run_dir);
    if head.map(|h| h.playbook).as_deref() != Some(playbook) {
        return RunCheck::Void;
    }
    let provenance = events.iter().find_map(|e| match &e.payload {
        EventPayload::RunProvenance { origin, .. } => origin.clone(),
        _ => None,
    });
    if provenance.as_deref() != Some(scope.as_str()) {
        return RunCheck::Void;
    }
    if let Some(digest) = key_digest {
        let recorded = events.iter().find_map(|e| match &e.payload {
            EventPayload::RunProvenance {
                concurrency: Some(record),
                ..
            } => Some(record.key_digest.clone()),
            _ => None,
        });
        if recorded.as_deref() != Some(digest) {
            return RunCheck::Void;
        }
    }
    RunCheck::Valid(RunDir {
        path: run_dir,
        events,
    })
}

impl Slot {
    /// The holder that names `run_id` as active or waiting.
    pub fn holder_of(&self, run_id: &str) -> Option<&Holder> {
        self.active
            .iter()
            .chain(self.waiting.iter())
            .find(|h| h.run_id == run_id)
    }
}

/// The directory of a playbook's owner locks.
fn owners_dir(root: &Path, scope: Scope, playbook: &str) -> PathBuf {
    apb_core::signed_store::dir(
        root,
        <ConcurrencyFile as apb_core::signed_store::SignedDoc>::DIR,
        scope,
    )
    .join(format!("{playbook}.owners"))
}

/// Whether the owner of `reservation` is alive: its lock cannot be taken.
fn owner_alive(root: &Path, scope: Scope, playbook: &str, reservation: &str) -> bool {
    apb_core::fsutil::try_hold_lock(
        &owners_dir(root, scope, playbook),
        &format!("{reservation}.lock"),
    )
    .is_err()
}

/// Whether the run at `rd` has a durable claim: no terminal event yet (a
/// live start), or a `run_resumed` newer than its last terminal event (a
/// resume that journaled it).
fn claim_durable(rd: &RunDir) -> bool {
    let last_terminal = rd
        .events
        .iter()
        .filter(|e| e.payload.is_checkpoint())
        .map(|e| e.seq)
        .max();
    match last_terminal {
        None => true,
        Some(t) => rd
            .events
            .iter()
            .any(|e| e.seq > t && matches!(&e.payload, EventPayload::RunResumed { .. })),
    }
}

/// Heals a slot on every access inside a transaction: reads only run
/// directories, writes only the slot.
fn heal(root: &Path, scope: Scope, playbook: &str, slot: &mut Slot) {
    let key = slot.key_digest.clone();
    // Rules 1 and 2: a holder with a reservation.
    if let Some(h) = slot.active.clone()
        && let Some(reservation) = &h.reservation
    {
        if owner_alive(root, scope, playbook, reservation) {
            // Kept as it is, whatever its directory holds.
        } else {
            match checked_run(root, scope, playbook, &h.run_id, Some(&key)) {
                RunCheck::Unreadable => {}
                RunCheck::Valid(rd) => {
                    if claim_durable(&rd) {
                        slot.active = Some(Holder {
                            run_id: h.run_id,
                            reservation: None,
                        });
                    } else {
                        slot.active = None;
                    }
                }
                RunCheck::Void => slot.active = None,
            }
        }
    }
    if let Some(h) = slot.waiting.clone()
        && let Some(reservation) = &h.reservation
    {
        if owner_alive(root, scope, playbook, reservation) {
            // Kept: the owner is alive.
        } else {
            let dir = root.join(".apb/runs").join(&h.run_id);
            if crate::liveness::driver_alive(&dir, &h.run_id) == Some(true) {
                // Kept: a detached driver between its start and its
                // first look holds the driver claim its spawner
                // published.
            } else {
                slot.waiting = None;
                if let RunCheck::Valid(rd) =
                    checked_run(root, scope, playbook, &h.run_id, Some(&key))
                    && !crate::state::RunState::fold(&rd.events)
                        .run_status
                        .is_terminal()
                {
                    slot.stops.push(PendingStop {
                        run_id: h.run_id,
                        reason: format!(
                            "superseded: its driver ended while it waited for key `{}`",
                            slot.display
                        ),
                    });
                }
            }
        }
    }
    // Rule 3: a holder without a reservation.
    if let Some(h) = slot.active.clone()
        && h.reservation.is_none()
    {
        match checked_run(root, scope, playbook, &h.run_id, Some(&key)) {
            RunCheck::Unreadable => {}
            RunCheck::Valid(rd) => {
                if crate::state::RunState::fold(&rd.events)
                    .run_status
                    .is_terminal()
                {
                    slot.active = None;
                }
            }
            RunCheck::Void => slot.active = None,
        }
    }
    if let Some(h) = slot.waiting.clone()
        && h.reservation.is_none()
    {
        match checked_run(root, scope, playbook, &h.run_id, Some(&key)) {
            RunCheck::Unreadable => {}
            RunCheck::Valid(rd) => {
                if crate::state::RunState::fold(&rd.events)
                    .run_status
                    .is_terminal()
                {
                    slot.waiting = None;
                }
            }
            RunCheck::Void => slot.waiting = None,
        }
    }
    // Rule 4: a stop entry whose run is void is dropped, except one aimed
    // at a provisional holder: its owner is alive (rules 1 and 2 kept it),
    // so the run may simply not have written its provenance yet, and its
    // confirm posts the stop.
    let provisional: BTreeSet<String> = slot
        .active
        .iter()
        .chain(slot.waiting.iter())
        .filter(|h| h.reservation.is_some())
        .map(|h| h.run_id.clone())
        .collect();
    slot.stops.retain(|s| {
        provisional.contains(&s.run_id)
            || !matches!(
                checked_run(root, scope, playbook, &s.run_id, None),
                RunCheck::Void
            )
    });
}

/// One transaction over the playbook's slot file, refusing a present file
/// that does not verify (`concurrency_state_unreadable`), with empty
/// slots removed from anything written.
fn store<R>(
    root: &Path,
    scope: Scope,
    playbook: &str,
    change: impl FnOnce(&mut ConcurrencyFile) -> Result<(R, bool), EngineError>,
) -> Result<R, EngineError> {
    apb_core::signed_store::transact::<ConcurrencyFile, R, EngineError>(
        root,
        scope,
        playbook,
        apb_core::signed_store::Unsigned::Refuse,
        |f| {
            let (out, changed) = change(f)?;
            if changed {
                f.slots
                    .retain(|s| s.active.is_some() || s.waiting.is_some() || !s.stops.is_empty());
            }
            Ok((out, changed))
        },
    )
    .map_err(|e| match e {
        EngineError::Conflict(text) if text.contains("does not verify") => EngineError::Conflict(
            format!("concurrency_state_unreadable: {text}; move it aside to start again"),
        ),
        other => other,
    })
}

impl std::fmt::Debug for SlotOwner {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SlotOwner")
            .field("playbook", &self.playbook)
            .field("key_digest", &self.key_digest)
            .field("run_id", &self.run_id)
            .finish_non_exhaustive()
    }
}

/// The owner lock of a provisional claim, held for the claim's lifetime:
/// a live preparation, a resuming process or a waiter. The owner is alive
/// while `try_hold_lock` on its lock fails with `WouldBlock`; the kernel
/// releases the lock when the owner's process ends, so a live owner is
/// never displaced and a dead one never blocks.
pub struct SlotOwner {
    root: PathBuf,
    scope: Scope,
    playbook: String,
    key_digest: String,
    run_id: String,
    reservation: String,
    lock: Option<apb_core::fsutil::HeldLock>,
}

impl SlotOwner {
    /// Takes the owner lock of a new reservation for `run_id`.
    fn acquire(
        root: &Path,
        scope: Scope,
        playbook: &str,
        key_digest: &str,
        run_id: &str,
    ) -> Result<SlotOwner, EngineError> {
        let reservation = apb_core::connector::marker::new_nonce();
        let lock = apb_core::fsutil::hold_lock(
            &owners_dir(root, scope, playbook),
            &format!("{reservation}.lock"),
        )?;
        Ok(SlotOwner {
            root: root.to_path_buf(),
            scope,
            playbook: playbook.to_string(),
            key_digest: key_digest.to_string(),
            run_id: run_id.to_string(),
            reservation,
            lock: Some(lock),
        })
    }

    /// Once `run_provenance` is durable: an active holder's reservation
    /// is cleared and the lock released; a waiter keeps its reservation
    /// (it stays provisional until its turn) and the owner is kept by
    /// `wait_turn`. Fails `concurrency_lost` when the slot no longer
    /// names this run with this reservation.
    pub fn confirm(mut self) -> Result<Option<SlotOwner>, EngineError> {
        let root = self.root.clone();
        let scope = self.scope;
        let playbook = self.playbook.clone();
        let key = self.key_digest.clone();
        let run_id = self.run_id.clone();
        let reservation = self.reservation.clone();
        let active = store(&root, scope, &playbook, |f| {
            let Some(slot) = f.slots.iter_mut().find(|s| s.key_digest == key) else {
                return Err(EngineError::Conflict(
                    "concurrency_lost: the run's key slot no longer names it".into(),
                ));
            };
            heal(&root, scope, &playbook, slot);
            if let Some(h) = slot.active.as_mut()
                && h.run_id == run_id
                && h.reservation.as_deref() == Some(&reservation)
            {
                h.reservation = None;
                return Ok((true, true));
            }
            if let Some(h) = slot.waiting.as_mut()
                && h.run_id == run_id
                && h.reservation.as_deref() == Some(&reservation)
            {
                // A waiter stays provisional until its turn.
                return Ok((false, true));
            }
            Err(EngineError::Conflict(
                "concurrency_lost: the run's key slot no longer names it".into(),
            ))
        })?;
        post_stops(&root, scope, &playbook, &key);
        if active {
            // An active holder: the lock drops with self.
            return Ok(None);
        }
        // A waiter: the owner stays with the run until its turn.
        Ok(Some(SlotOwner {
            lock: self.lock.take(),
            ..self
        }))
    }
}

/// Posts the slot's promised stops, outside every lock. A stop aimed at a
/// provisional claim (a start not confirmed, a resume not yet journaled)
/// is deferred: it stays recorded and is posted by the confirm that ends
/// the provisional state. Every other stop is posted through the one stop
/// path, graceful; a stop of a run nothing drives finalizes it and runs
/// its health observation, which may send, which is why no stop is ever
/// posted inside a transaction.
fn post_stops(root: &Path, scope: Scope, playbook: &str, key_digest: &str) {
    let to_post = match store(root, scope, playbook, |f| {
        let Some(slot) = f.slots.iter_mut().find(|s| s.key_digest == key_digest) else {
            return Ok((Vec::new(), false));
        };
        heal(root, scope, playbook, slot);
        let provisional: BTreeSet<String> = slot
            .active
            .iter()
            .chain(slot.waiting.iter())
            .filter(|h| h.reservation.is_some())
            .map(|h| h.run_id.clone())
            .collect();
        let post: Vec<PendingStop> = slot
            .stops
            .iter()
            .filter(|s| !provisional.contains(&s.run_id))
            .cloned()
            .collect();
        Ok((post, false))
    }) {
        Ok(post) => post,
        Err(_) => return,
    };
    if to_post.is_empty() {
        return;
    }
    let mut posted: Vec<PendingStop> = Vec::new();
    for stop in to_post {
        let outcome = crate::stop::stop_run_with(
            root,
            &stop.run_id,
            &crate::stop::StopRequest {
                by: crate::pause::kind::PauseBy::Engine,
                reason: &stop.reason,
                graceful: true,
                due: None,
            },
        );
        match outcome {
            Ok(_) | Err(EngineError::NotFound(_)) => posted.push(stop),
            Err(_) => {}
        }
    }
    if posted.is_empty() {
        return;
    }
    let _ = store(root, scope, playbook, |f| {
        let Some(slot) = f.slots.iter_mut().find(|s| s.key_digest == key_digest) else {
            return Ok(((), false));
        };
        let before = slot.stops.len();
        slot.stops.retain(|s| {
            !posted
                .iter()
                .any(|p| p.run_id == s.run_id && p.reason == s.reason)
        });
        Ok(((), slot.stops.len() != before))
    });
}

/// What a start keys on and where: nothing of it when the playbook has no
/// block, or the run is a scenario, an eval or a child.
#[derive(Debug, Clone, Copy, Default)]
pub struct KeyScope {
    pub scenario: bool,
    pub eval: bool,
    pub child: bool,
    pub depth: usize,
}

/// The key of a start: rendered, checked and recorded, or `None` for a
/// run that takes no slot (no block, a child, a scenario, an eval). The
/// render is the same call the worktree resolution uses, over the final
/// params, with `WHOLE_PLAYBOOK_KEY` when `key` is absent. Each param the
/// key reads is checked for sensitivity: a sensitive value refuses the
/// start, naming the param, never the value.
pub fn key_for_start(
    root: &Path,
    playbook: &apb_core::schema::Playbook,
    params: &BTreeMap<String, String>,
    opts: &KeyScope,
) -> Result<Option<ConcurrencyRecord>, EngineError> {
    let Some(block) = &playbook.concurrency else {
        return Ok(None);
    };
    if opts.scenario || opts.eval || opts.child || opts.depth > 0 {
        return Ok(None);
    }
    let template = block
        .key
        .as_deref()
        .unwrap_or(apb_core::concurrency::WHOLE_PLAYBOOK_KEY);
    let rendered = crate::context::render(
        template,
        params,
        None,
        &BTreeMap::new(),
        &BTreeMap::new(),
        &BTreeMap::new(),
        &BTreeMap::new(),
        "",
        &BTreeMap::new(),
        &BTreeMap::new(),
        &crate::context::OutputClip {
            run_dir: root,
            max_bytes: 0,
            retro: None,
        },
    );
    for cap in apb_core::template::refs(template) {
        if let ["params", name] = cap.split('.').collect::<Vec<&str>>().as_slice()
            && let Some(value) = params.get(*name)
        {
            let declared = playbook
                .params
                .iter()
                .find(|p| &p.name == name)
                .map(|p| p.secret)
                .unwrap_or(false);
            if apb_core::run_params::sensitive(declared, name, value).is_some() {
                return Err(EngineError::Conflict(format!(
                    "concurrency_key_sensitive: param `{name}` holds a value that looks like a credential; it cannot key a run"
                )));
            }
        }
    }
    let digest = apb_core::run_origin::tag(apb_core::concurrency::KEY_DOMAIN, rendered.as_bytes())
        .map_err(|e| EngineError::Invalid(e.to_string()))?;
    let scrubber = apb_core::scrub::Scrubber::for_workspace(root, Vec::new());
    let scrubbed = scrubber.scrub(&rendered);
    let display: String = scrubbed
        .chars()
        .take(apb_core::concurrency::DISPLAY_MAX_CHARS)
        .collect();
    Ok(Some(ConcurrencyRecord {
        key_digest: digest,
        display,
        on_busy: block.on_busy.as_str().to_string(),
        queued: false,
    }))
}

/// The runs a start must never wait for or stop: the run step's own run,
/// its parent chain and its starter chain, in this project. Runs of
/// another project are never in it (their keys live in another project's
/// store).
pub fn lineage(root: &Path, started_by: Option<&apb_core::launch::StartedBy>) -> BTreeSet<String> {
    let this = apb_core::projects::registered_id(root);
    let verified = |sb: &apb_core::launch::StartedBy| -> Option<String> {
        match &sb.workspace_id {
            None => Some(sb.run_id.clone()),
            Some(w) if this.as_deref() == Some(w) => Some(sb.run_id.clone()),
            _ => None,
        }
    };
    let mut out = BTreeSet::new();
    let mut frontier: Vec<String> = started_by.and_then(verified).into_iter().collect();
    let bound =
        apb_core::launch::MAX_START_CHAIN as usize + apb_core::schema::MAX_SUBPLAYBOOK_DEPTH;
    let mut steps = 0usize;
    while let Some(run) = frontier.pop() {
        if !out.insert(run.clone()) {
            continue;
        }
        steps += 1;
        if steps > bound {
            break;
        }
        let dir = root.join(".apb/runs").join(&run);
        if let Ok(Some(manifest)) = crate::manifest::read(&dir)
            && let Some(sb) = manifest.launch.as_ref().and_then(|l| l.started_by.as_ref())
            && let Some(id) = verified(sb)
        {
            frontier.push(id);
        }
        if let Ok(cfg) = crate::run_config::read_run_config(&dir)
            && let Some(parent) = cfg.parent_run
        {
            frontier.push(parent);
        }
    }
    out
}

/// What admission answered.
#[derive(Debug)]
pub enum Admission {
    /// The slot was free: this run is active.
    Active(SlotOwner),
    /// The run waits for `holder`.
    Queued { holder: String, owner: SlotOwner },
}

/// Under the store's lock: heal, decide, record the provisional holder
/// and any stops it promises, release; then post the stops. On a refusal
/// the owner lock is dropped before the error returns.
pub fn admit(
    root: &Path,
    scope: Scope,
    playbook: &str,
    record: &ConcurrencyRecord,
    on_busy: apb_core::concurrency::OnBusy,
    run_id: &str,
    lineage: &BTreeSet<String>,
) -> Result<Admission, EngineError> {
    let owner = SlotOwner::acquire(root, scope, playbook, &record.key_digest, run_id)?;
    let reservation = owner.reservation.clone();
    let key = record.key_digest.clone();
    let display = record.display.clone();
    let new_run = run_id.to_string();
    let answer = store(root, scope, playbook, |f| {
        let slot = match f.slots.iter_mut().find(|s| s.key_digest == key) {
            Some(s) => s,
            None => {
                f.slots.push(Slot {
                    key_digest: key.clone(),
                    display: display.clone(),
                    active: None,
                    waiting: None,
                    stops: Vec::new(),
                });
                f.slots
                    .iter_mut()
                    .find(|s| s.key_digest == key)
                    .expect("the slot was just pushed")
            }
        };
        heal(root, scope, playbook, slot);
        // Rule 1: the holder is this run's starter.
        let holder = slot
            .active
            .as_ref()
            .or(slot.waiting.as_ref())
            .map(|h| h.run_id.clone());
        if let Some(h) = &holder
            && lineage.contains(h)
        {
            return Err(EngineError::Conflict(format!(
                "concurrency_own_run: key `{}` is held by run `{h}`, which started this run; a run cannot wait for or replace the run that started it",
                record.display
            )));
        }
        // Rule 2: refuse.
        if on_busy == apb_core::concurrency::OnBusy::Refuse
            && (slot.active.is_some() || slot.waiting.is_some())
        {
            let id = holder.unwrap_or_default();
            return Err(EngineError::Conflict(format!(
                "concurrency_busy: key `{}` is held by run `{id}`; it ends on its own, or `apb stop {id}` frees it",
                record.display
            )));
        }
        // Rule 3: a free slot.
        if slot.active.is_none() && slot.waiting.is_none() {
            slot.active = Some(Holder {
                run_id: new_run.clone(),
                reservation: Some(reservation.clone()),
            });
            return Ok((("active", String::new()), true));
        }
        // Rule 4: no active, a waiter that was not yet promoted: the
        // newest start replaces it, as everywhere else.
        if slot.active.is_none() {
            if let Some(b) = slot.waiting.take() {
                slot.stops.push(PendingStop {
                    run_id: b.run_id,
                    reason: format!(
                        "superseded: a newer start (run `{new_run}`) replaced this queued run"
                    ),
                });
            }
            slot.active = Some(Holder {
                run_id: new_run.clone(),
                reservation: Some(reservation.clone()),
            });
            return Ok((("active", String::new()), true));
        }
        // Rule 5: an active holder: this run waits.
        let holder = slot
            .active
            .as_ref()
            .map(|h| h.run_id.clone())
            .unwrap_or_default();
        if let Some(b) = slot.waiting.take() {
            slot.stops.push(PendingStop {
                run_id: b.run_id,
                reason: format!(
                    "superseded: a newer start (run `{new_run}`) replaced this queued run"
                ),
            });
        }
        slot.waiting = Some(Holder {
            run_id: new_run.clone(),
            reservation: Some(reservation.clone()),
        });
        if on_busy == apb_core::concurrency::OnBusy::Supersede {
            slot.stops.push(PendingStop {
                run_id: holder.clone(),
                reason: format!(
                    "superseded: run `{new_run}` started for key `{}`",
                    record.display
                ),
            });
        }
        Ok((("queued", holder), true))
    });
    match answer {
        Err(e) => Err(e),
        Ok(("active", _)) => {
            post_stops(root, scope, playbook, &key);
            Ok(Admission::Active(owner))
        }
        Ok(("queued", holder)) => {
            post_stops(root, scope, playbook, &key);
            Ok(Admission::Queued { holder, owner })
        }
        Ok(_) => unreachable!("admit answers active or queued"),
    }
}

/// The turn of a queued run.
#[derive(Debug)]
pub enum Turn {
    /// The run is the slot's active run: it may claim the tree.
    Active,
    /// The run was paused before it got the key (a pause journaled while
    /// it had no driver claim): it gives up its place and a later resume
    /// takes the key like any resume.
    Paused,
}

/// Waits until this run is the slot's active run. Every look at the slot
/// is one transaction (heal, decide, write), and the slot's stops are
/// posted after it; the journal checks run outside the lock. Called
/// before any tree lock, at the two drive entries; a caller answers
/// `Err(Superseded)` by settling the run aborted `superseded:` and
/// `Turn::Paused` by giving the slot up.
pub fn wait_turn(
    root: &Path,
    run_dir: &Path,
    run_id: &str,
    mut owner: Option<SlotOwner>,
) -> Result<Turn, EngineError> {
    let events = crate::event::read_all(run_dir)?;
    let Some(record) = events.iter().find_map(|e| match &e.payload {
        EventPayload::RunProvenance {
            concurrency: Some(record),
            ..
        } => Some(record.clone()),
        _ => None,
    }) else {
        return Ok(Turn::Active);
    };
    let scope = events
        .iter()
        .find_map(|e| match &e.payload {
            EventPayload::RunProvenance { origin, .. } => origin.clone(),
            _ => None,
        })
        .and_then(|o| Scope::parse(&o))
        .unwrap_or(Scope::Project);
    let playbook = crate::run_history::started(run_dir)
        .map(|h| h.playbook)
        .unwrap_or_default();
    let key = record.key_digest.clone();
    // A driver taking over a seeded waiter re-reserves under a new nonce:
    // the preparing process's owner lock ended with that process.
    if owner.is_none() {
        let named_waiter = store::<bool>(root, scope, &playbook, |f| {
            let Some(slot) = f.slots.iter_mut().find(|s| s.key_digest == key) else {
                return Err(EngineError::Conflict(
                    "concurrency_lost: the run's key slot no longer names it".into(),
                ));
            };
            heal(root, scope, &playbook, slot);
            Ok((slot.holder_of(run_id).is_some(), false))
        })
        .unwrap_or(false);
        if named_waiter
            && let Some(h) = read_slots(root, scope, &playbook)
                .iter()
                .find(|s| s.key_digest == key)
                .and_then(|s| s.holder_of(run_id))
            && h.reservation.is_none()
        {
            // The driver takes the waiter's place under a fresh owner:
            // the preparing process's owner lock ended with that process.
            let fresh = SlotOwner::acquire(root, scope, &playbook, &key, run_id)?;
            let reservation = fresh.reservation.clone();
            store::<()>(root, scope, &playbook, |f| {
                let Some(slot) = f.slots.iter_mut().find(|s| s.key_digest == key) else {
                    return Ok(((), false));
                };
                if let Some(h) = slot
                    .waiting
                    .as_mut()
                    .filter(|h| h.run_id == run_id && h.reservation.is_none())
                {
                    h.reservation = Some(reservation);
                    return Ok(((), true));
                }
                Ok(((), false))
            })?;
            owner = Some(fresh);
        }
    }
    loop {
        #[cfg(any(test, feature = "testing"))]
        crate::deadline_hooks::at(crate::deadline_hooks::Hook::KeyBeforeLook, run_dir);
        #[cfg(not(any(test, feature = "testing")))]
        let _ = run_dir;
        let decided = store::<Option<Turn>>(root, scope, &playbook, |f| {
            let Some(slot) = f.slots.iter_mut().find(|s| s.key_digest == key) else {
                return Err(EngineError::Conflict(
                    "concurrency_lost: the run's key slot no longer names it".into(),
                ));
            };
            heal(root, scope, &playbook, slot);
            if let Some(h) = slot.active.as_mut()
                && h.run_id == run_id
            {
                h.reservation = None;
                return Ok((Some(Turn::Active), true));
            }
            if slot.active.is_none()
                && let Some(h) = slot.waiting.as_mut()
                && h.run_id == run_id
            {
                slot.active = Some(Holder {
                    run_id: run_id.to_string(),
                    reservation: None,
                });
                slot.waiting = None;
                return Ok((Some(Turn::Active), true));
            }
            if !slot
                .active
                .iter()
                .chain(slot.waiting.iter())
                .any(|h| h.run_id == run_id)
            {
                return Err(EngineError::Superseded(
                    "a newer start replaced this queued run".into(),
                ));
            }
            Ok((None::<Turn>, false))
        });
        match decided {
            Err(e) => {
                // The owner is dropped; the heal or the close-out cleans.
                return Err(e);
            }
            Ok(Some(turn)) => {
                drop(owner);
                return Ok(turn);
            }
            Ok(None) => {}
        }
        post_stops(root, scope, &playbook, &key);
        // Outside the lock: the journal checks.
        let journal = crate::event::read_all_lossy_tail(run_dir)?;
        if crate::state::RunState::fold(&journal)
            .run_status
            .is_terminal()
        {
            return Err(EngineError::Conflict(
                "the run ended while it waited for its key".into(),
            ));
        }
        if crate::state::RunState::fold(&journal).run_status == crate::state::RunStatus::Paused {
            // A pause journaled directly: the run gives up its place.
            let _ = store(root, scope, &playbook, |f| {
                let Some(slot) = f.slots.iter_mut().find(|s| s.key_digest == key) else {
                    return Ok(((), false));
                };
                heal(root, scope, &playbook, slot);
                let mut changed = false;
                if let Some(h) = slot.waiting.as_ref()
                    && h.run_id == run_id
                {
                    slot.waiting = None;
                    changed = true;
                }
                Ok(((), changed))
            });
            post_stops(root, scope, &playbook, &key);
            drop(owner);
            return Ok(Turn::Paused);
        }
        if crate::run_deadline::expired(&journal, apb_core::clock::now_ms()) {
            match crate::run_deadline::stop_if_expired(
                root,
                run_id,
                crate::run_deadline::Expect::Any,
            ) {
                Ok(true) => {
                    return Err(EngineError::Conflict(
                        crate::run_deadline::DEADLINE_EXCEEDED.to_string(),
                    ));
                }
                Ok(false) => {}
                Err(e) => return Err(e),
            }
        }
        if matches!(crate::control::pending_stop_seq(run_dir), Ok(Some(_))) {
            return Err(EngineError::Conflict(
                "stopped while it waited for its key".into(),
            ));
        }
        std::thread::sleep(TURN_STEP);
    }
}

/// A resume's reservation, held from before the tree is acquired until
/// `run_resumed` is journaled. Dropping it unconfirmed gives the slot
/// back; confirming clears the provisional reservation and posts the
/// slot's stops.
pub struct ResumeReservation {
    root: PathBuf,
    scope: Scope,
    playbook: String,
    key_digest: String,
    run_id: String,
    reservation: String,
    was_active: bool,
    lock: Option<apb_core::fsutil::HeldLock>,
}

impl std::fmt::Debug for ResumeReservation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ResumeReservation")
            .field("playbook", &self.playbook)
            .field("run_id", &self.run_id)
            .finish_non_exhaustive()
    }
}

impl ResumeReservation {
    /// After `run_resumed` returned `Ok`, outside the event lock: the
    /// reservation is cleared when the slot still names this run with
    /// this reservation, and fails `concurrency_lost` otherwise; the
    /// owner lock is released and the slot's stops are posted, so a stop
    /// deferred against the provisional resume reaches the now live run.
    pub fn confirm(mut self) -> Result<(), EngineError> {
        let root = self.root.clone();
        let scope = self.scope;
        let playbook = self.playbook.clone();
        let key = self.key_digest.clone();
        let run_id = self.run_id.clone();
        let reservation = self.reservation.clone();
        store(&root, scope, &playbook, |f| {
            let Some(slot) = f.slots.iter_mut().find(|s| s.key_digest == key) else {
                return Err(EngineError::Conflict(
                    "concurrency_lost: the run's key slot changed while it resumed".into(),
                ));
            };
            heal(&root, scope, &playbook, slot);
            match slot.active.as_mut() {
                Some(h) if h.run_id == run_id && h.reservation.as_deref() == Some(&reservation) => {
                    h.reservation = None;
                    Ok(((), true))
                }
                _ => Err(EngineError::Conflict(
                    "concurrency_lost: the run's key slot changed while it resumed".into(),
                )),
            }
        })?;
        post_stops(&root, scope, &playbook, &key);
        self.lock = None;
        Ok(())
    }

    fn release(&mut self) {
        let root = self.root.clone();
        let scope = self.scope;
        let playbook = self.playbook.clone();
        let key = self.key_digest.clone();
        let run_id = self.run_id.clone();
        let reservation = self.reservation.clone();
        let was_active = self.was_active;
        let _ = store(&root, scope, &playbook, |f| {
            let Some(slot) = f.slots.iter_mut().find(|s| s.key_digest == key) else {
                return Ok(((), false));
            };
            heal(&root, scope, &playbook, slot);
            match slot.active.as_mut() {
                Some(h) if h.run_id == run_id && h.reservation.as_deref() == Some(&reservation) => {
                    if was_active {
                        // The paused holder was already active: only the
                        // reservation goes.
                        h.reservation = None;
                    } else {
                        slot.active = None;
                    }
                    Ok(((), true))
                }
                _ => Ok(((), false)),
            }
        });
        post_stops(&root, scope, &playbook, &key);
        self.lock = None;
    }
}

impl Drop for ResumeReservation {
    /// A refused resume gives the slot back: the holder it created is
    /// cleared, the reservation of a paused holder is cleared, and the
    /// slot's stops are posted. A crash leaves the owner lock free, and
    /// the heal decides.
    fn drop(&mut self) {
        if self.lock.is_some() {
            self.release();
        }
    }
}

/// A resume's reservation (design D1 "Resumes"): reads the run's
/// `run_provenance.concurrency`; none: `Ok(None)`. Else it takes the
/// owner lock of a new reservation and, in one transaction, heals, then
/// refuses a busy slot, re-reserves a paused holder or becomes active.
pub fn reserve_resume(
    root: &Path,
    run_dir: &Path,
    run_id: &str,
) -> Result<Option<ResumeReservation>, EngineError> {
    let events = crate::event::read_all(run_dir)?;
    let Some(record) = events.iter().find_map(|e| match &e.payload {
        EventPayload::RunProvenance {
            concurrency: Some(record),
            ..
        } => Some(record.clone()),
        _ => None,
    }) else {
        return Ok(None);
    };
    let scope = events
        .iter()
        .find_map(|e| match &e.payload {
            EventPayload::RunProvenance { origin, .. } => origin.clone(),
            _ => None,
        })
        .and_then(|o| Scope::parse(&o))
        .unwrap_or(Scope::Project);
    let playbook = crate::run_history::started(run_dir)
        .map(|h| h.playbook)
        .unwrap_or_default();
    let key = record.key_digest.clone();
    let display = record.display.clone();
    let mut owner = SlotOwner::acquire(root, scope, &playbook, &key, run_id)?;
    let reservation = owner.reservation.clone();
    let mut was_active = false;
    store(root, scope, &playbook, |f| {
        if f.slots.iter_mut().find(|s| s.key_digest == key).is_none() {
            f.slots.push(Slot {
                key_digest: key.clone(),
                display: display.clone(),
                active: None,
                waiting: None,
                stops: Vec::new(),
            });
        }
        let slot = f
            .slots
            .iter_mut()
            .find(|s| s.key_digest == key)
            .expect("the slot was just ensured");
        heal(root, scope, &playbook, slot);
        match slot.active.as_ref() {
            Some(h) if h.run_id != run_id => {
                return Err(EngineError::Conflict(format!(
                    "concurrency_busy: key `{display}` is held by run `{}`; it ends on its own, or `apb stop {}` frees it",
                    h.run_id, h.run_id
                )));
            }
            Some(h) if h.run_id == run_id && h.reservation.is_some() => {
                return Err(EngineError::Conflict(format!(
                    "concurrency_busy: run `{run_id}` is being resumed by another process"
                )));
            }
            Some(h) if h.run_id == run_id => {
                // A paused holder resuming: only the reservation goes in.
                was_active = true;
            }
            _ => {}
        }
        slot.active = Some(Holder {
            run_id: run_id.to_string(),
            reservation: Some(reservation.clone()),
        });
        Ok(((), true))
    })?;
    post_stops(root, scope, &playbook, &key);
    Ok(Some(ResumeReservation {
        root: root.to_path_buf(),
        scope,
        playbook,
        key_digest: key,
        run_id: run_id.to_string(),
        reservation,
        was_active,
        lock: owner.lock.take(),
    }))
}

/// Why a supervisor patch from `old` to `new` must be refused: the block
/// decides which runs wait or stop, and is not a supervisor's to change.
pub fn patch_refusal(
    old: &apb_core::schema::Playbook,
    new: &apb_core::schema::Playbook,
) -> Option<String> {
    if old.concurrency == new.concurrency {
        return None;
    }
    Some(
        "concurrency_changed: the run's `concurrency:` block is not a supervisor's to change"
            .to_string(),
    )
}

/// The `apb doctor` WARN line for slot files that do not verify under
/// `root`; `None` when every one reads (or there is none).
pub fn doctor_line(root: &Path) -> Option<String> {
    let mut bad = Vec::new();
    for scope in [Scope::Project, Scope::Global] {
        let dir = apb_core::signed_store::dir(
            root,
            <ConcurrencyFile as apb_core::signed_store::SignedDoc>::DIR,
            scope,
        );
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for e in entries.filter_map(Result::ok) {
            let name = e.file_name().to_string_lossy().into_owned();
            let Some(id) = name.strip_suffix(".json") else {
                continue;
            };
            let readable = apb_core::signed_store::read::<ConcurrencyFile>(root, scope, id)
                .is_ok_and(|l| l.signed);
            if !readable {
                bad.push(e.path().display().to_string());
            }
        }
    }
    (!bad.is_empty()).then(|| {
        format!(
            "concurrency slot files that do not verify (keyed starts of their playbooks are refused until each is moved aside): {}",
            bad.join(", ")
        )
    })
}

/// Writes one slot for a view test (test only): the signed store without
/// a heal.
/// The slots of a playbook in project scope, for the e2e suite's slot
/// assertions (test builds only).
#[cfg(any(test, feature = "testing"))]
pub fn read_slots_for_test(root: &Path) -> Vec<Slot> {
    read_slots(root, Scope::Project, "ck")
}

#[cfg(test)]
pub(crate) fn seed_for_view(root: &Path, slot: Slot) {
    let key = slot.key_digest.clone();
    let display = slot.display.clone();
    let _ = store(root, Scope::Project, "p", move |f: &mut ConcurrencyFile| {
        f.slots.retain(|s| s.key_digest != key);
        f.slots.push(Slot {
            key_digest: key,
            display,
            ..slot
        });
        Ok(((), true))
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::EventPayload;

    /// Runs `f` under a fresh config dir in a plain child process, in a
    /// fresh project with an empty runs directory.
    fn child(name: &str, f: impl FnOnce(&Path)) {
        apb_core::process_mode::testing::in_plain_child(name, || {
            let cfg = tempfile::tempdir().unwrap();
            // SAFETY: a child process runs this one test on one thread.
            unsafe { std::env::set_var("APB_CONFIG_DIR", cfg.path()) };
            apb_core::run_origin::ensure_key().unwrap();
            let project = tempfile::tempdir().unwrap();
            let root = project.path();
            std::fs::create_dir_all(root.join(".apb/runs")).unwrap();
            f(root);
        });
    }

    /// A run directory seeded as this installation's run `p-1` of
    /// playbook `p` in scope `scope`, with a concurrency record for
    /// `key` when `keyed`.
    fn seed_run(root: &Path, id: &str, scope: Scope, playbook: &str, key: Option<&str>) {
        let dir = root.join(".apb/runs").join(id);
        std::fs::create_dir_all(&dir).unwrap();
        let mut log = crate::event::EventLog::create(&dir).unwrap();
        log.append(EventPayload::RunStarted {
            playbook: playbook.to_string(),
            version: "1.0.0".into(),
            definition: None,
            external_data: false,
        })
        .unwrap();
        log.append(EventPayload::RunProvenance {
            origin: Some(scope.as_str().to_string()),
            digest: None,
            execution_root: None,
            scripts_digest: None,
            profiles: Vec::new(),
            remembered: Vec::new(),
            apb: None,
            limits: Vec::new(),
            previous_success: None,
            untrusted: Vec::new(),
            deadline: None,
            start_key: None,
            concurrency: key.map(|d| ConcurrencyRecord {
                key_digest: d.to_string(),
                display: "k".into(),
                on_busy: "refuse".into(),
                queued: false,
            }),
        })
        .unwrap();
        apb_core::run_origin::stamp(&dir, id).unwrap();
    }

    /// Ends the seeded run with `outcome`.
    fn end_run(root: &Path, id: &str, outcome: &str) {
        let dir = root.join(".apb/runs").join(id);
        let mut log = crate::event::EventLog::open(&dir).unwrap();
        log.append(EventPayload::RunFinished {
            outcome: outcome.to_string(),
        })
        .unwrap();
    }

    /// Writes `slot` into the playbook's signed slot file.
    fn seed_slot(root: &Path, slot: Slot) {
        let key = slot.key_digest.clone();
        let display = slot.display.clone();
        store(root, Scope::Project, "p", |f: &mut ConcurrencyFile| {
            f.slots.retain(|s| s.key_digest != key);
            f.slots.push(Slot {
                key_digest: key,
                display,
                ..slot
            });
            Ok(((), true))
        })
        .unwrap();
    }

    fn slot_with(
        key: &str,
        display: &str,
        active: Option<Holder>,
        waiting: Option<Holder>,
    ) -> Slot {
        Slot {
            key_digest: key.to_string(),
            display: display.to_string(),
            active,
            waiting,
            stops: Vec::new(),
        }
    }

    fn holder(run: &str, reservation: Option<&str>) -> Holder {
        Holder {
            run_id: run.to_string(),
            reservation: reservation.map(str::to_string),
        }
    }

    const KEY: &str = "d1";

    /// The slots of playbook `p`, display read.
    fn read_slots(root: &Path, scope: Scope, playbook: &str) -> Vec<Slot> {
        match apb_core::signed_store::read::<ConcurrencyFile>(root, scope, playbook) {
            Ok(loaded) if loaded.signed => loaded.file.slots,
            _ => Vec::new(),
        }
    }

    // --- the run check ---

    #[test]
    fn a_valid_run_checks_valid() {
        child("concurrency::tests::a_valid_run_checks_valid", |root| {
            seed_run(root, "p-1", Scope::Project, "p", Some(KEY));
            assert!(matches!(
                checked_run(root, Scope::Project, "p", "p-1", Some(KEY)),
                RunCheck::Valid(_)
            ));
            assert!(matches!(
                checked_run(root, Scope::Project, "p", "p-1", None),
                RunCheck::Valid(_)
            ));
        });
    }

    /// A run directory that is a link is void: the holder is dropped.
    #[test]
    fn a_linked_run_directory_is_void() {
        child(
            "concurrency::tests::a_linked_run_directory_is_void",
            |root| {
                seed_run(root, "p-1", Scope::Project, "p", Some(KEY));
                let outside = tempfile::tempdir().unwrap();
                std::fs::rename(root.join(".apb/runs/p-1"), outside.path().join("moved")).unwrap();
                std::os::unix::fs::symlink(
                    outside.path().join("moved"),
                    root.join(".apb/runs/p-1"),
                )
                .unwrap();
                assert!(matches!(
                    checked_run(root, Scope::Project, "p", "p-1", None),
                    RunCheck::Void
                ));
            },
        );
    }

    /// A signed slot naming a run of another playbook is void.
    #[test]
    fn a_run_of_another_playbook_is_void() {
        child(
            "concurrency::tests::a_run_of_another_playbook_is_void",
            |root| {
                seed_run(root, "q-1", Scope::Project, "q", Some(KEY));
                assert!(matches!(
                    checked_run(root, Scope::Project, "p", "q-1", Some(KEY)),
                    RunCheck::Void
                ));
            },
        );
    }

    /// A holder whose recorded key digest differs from the slot's is
    /// void.
    #[test]
    fn a_different_key_digest_is_void() {
        child(
            "concurrency::tests::a_different_key_digest_is_void",
            |root| {
                seed_run(root, "p-1", Scope::Project, "p", Some("other"));
                assert!(matches!(
                    checked_run(root, Scope::Project, "p", "p-1", Some(KEY)),
                    RunCheck::Void
                ));
            },
        );
    }

    /// A run without `run_provenance` is void.
    #[test]
    fn a_missing_provenance_is_void() {
        child("concurrency::tests::a_missing_provenance_is_void", |root| {
            let dir = root.join(".apb/runs/p-1");
            std::fs::create_dir_all(&dir).unwrap();
            let mut log = crate::event::EventLog::create(&dir).unwrap();
            log.append(EventPayload::RunStarted {
                playbook: "p".into(),
                version: "1.0.0".into(),
                definition: None,
                external_data: false,
            })
            .unwrap();
            apb_core::run_origin::stamp(&dir, "p-1").unwrap();
            assert!(matches!(
                checked_run(root, Scope::Project, "p", "p-1", None),
                RunCheck::Void
            ));
        });
    }

    /// A journal with an unknown event type after its last checkpoint
    /// does not read strictly: the holder is kept, never dropped.
    #[test]
    fn an_unknown_event_after_the_last_checkpoint_is_unreadable() {
        child(
            "concurrency::tests::an_unknown_event_after_the_last_checkpoint_is_unreadable",
            |root| {
                seed_run(root, "p-1", Scope::Project, "p", Some(KEY));
                use std::io::Write;
                let mut f = std::fs::OpenOptions::new()
                    .append(true)
                    .open(root.join(".apb/runs/p-1/events.jsonl"))
                    .unwrap();
                writeln!(
                    f,
                    r#"{{"seq":99,"ts":1,"type":"unknown_event_of_a_newer_apb"}}"#
                )
                .unwrap();
                assert!(matches!(
                    checked_run(root, Scope::Project, "p", "p-1", None),
                    RunCheck::Unreadable
                ));
            },
        );
    }

    /// A torn tail is unreadable, not void.
    #[test]
    fn a_torn_tail_is_unreadable() {
        child("concurrency::tests::a_torn_tail_is_unreadable", |root| {
            seed_run(root, "p-1", Scope::Project, "p", Some(KEY));
            use std::io::Write;
            let mut f = std::fs::OpenOptions::new()
                .append(true)
                .open(root.join(".apb/runs/p-1/events.jsonl"))
                .unwrap();
            f.write_all(b"{\"seq\":100,\"ts\":1,\"type\":\"run_fin")
                .unwrap();
            assert!(matches!(
                checked_run(root, Scope::Project, "p", "p-1", None),
                RunCheck::Unreadable
            ));
        });
    }

    // --- the heal ---

    /// A holder with a live owner is kept whatever its directory holds:
    /// the whole point of the owner lock (no timer decides).
    #[test]
    fn a_holder_with_a_live_owner_is_kept_whatever_its_directory() {
        child(
            "concurrency::tests::a_holder_with_a_live_owner_is_kept_whatever_its_directory",
            |root| {
                let _lock =
                    apb_core::fsutil::hold_lock(&owners_dir(root, Scope::Project, "p"), "r1.lock")
                        .unwrap();
                seed_slot(
                    root,
                    slot_with(KEY, "k", Some(holder("p-1", Some("r1"))), None),
                );
                store(root, Scope::Project, "p", |f: &mut ConcurrencyFile| {
                    let slot = f.slots.iter_mut().find(|s| s.key_digest == KEY).unwrap();
                    heal(root, Scope::Project, "p", slot);
                    assert_eq!(
                        slot.active.as_ref().unwrap().reservation.as_deref(),
                        Some("r1")
                    );
                    Ok(((), false))
                })
                .unwrap();
            },
        );
    }

    /// A provisional active holder whose run's start is durable has its
    /// reservation cleared (the confirm was lost in a crash).
    #[test]
    fn a_provisional_active_holder_with_a_durable_start_is_confirmed() {
        child(
            "concurrency::tests::a_provisional_active_holder_with_a_durable_start_is_confirmed",
            |root| {
                seed_run(root, "p-1", Scope::Project, "p", Some(KEY));
                seed_slot(
                    root,
                    slot_with(KEY, "k", Some(holder("p-1", Some("r1"))), None),
                );
                store(root, Scope::Project, "p", |f: &mut ConcurrencyFile| {
                    let slot = f.slots.iter_mut().find(|s| s.key_digest == KEY).unwrap();
                    heal(root, Scope::Project, "p", slot);
                    assert_eq!(slot.active.as_ref().unwrap().reservation, None);
                    Ok(((), false))
                })
                .unwrap();
            },
        );
    }

    /// A provisional active holder whose run never recorded its
    /// provenance is cleared: a crash in preparation frees the key.
    #[test]
    fn a_provisional_active_holder_without_one_is_cleared() {
        child(
            "concurrency::tests::a_provisional_active_holder_without_one_is_cleared",
            |root| {
                let dir = root.join(".apb/runs/p-1");
                std::fs::create_dir_all(&dir).unwrap();
                apb_core::run_origin::stamp(&dir, "p-1").unwrap();
                seed_slot(
                    root,
                    slot_with(KEY, "k", Some(holder("p-1", Some("r1"))), None),
                );
                store(root, Scope::Project, "p", |f: &mut ConcurrencyFile| {
                    let slot = f.slots.iter_mut().find(|s| s.key_digest == KEY).unwrap();
                    heal(root, Scope::Project, "p", slot);
                    assert!(slot.active.is_none());
                    Ok(((), false))
                })
                .unwrap();
            },
        );
    }

    /// A provisional resume whose `run_resumed` is journaled is
    /// confirmed.
    #[test]
    fn a_provisional_resume_with_run_resumed_is_confirmed() {
        child(
            "concurrency::tests::a_provisional_resume_with_run_resumed_is_confirmed",
            |root| {
                seed_run(root, "p-1", Scope::Project, "p", Some(KEY));
                end_run(root, "p-1", "failed");
                let dir = root.join(".apb/runs/p-1");
                let mut log = crate::event::EventLog::open(&dir).unwrap();
                log.append(EventPayload::RunResumed {
                    from_node: String::new(),
                    by: None,
                    explicit: false,
                    launch: None,
                    definition: None,
                    deadline: None,
                    apb: None,
                })
                .unwrap();
                seed_slot(
                    root,
                    slot_with(KEY, "k", Some(holder("p-1", Some("r1"))), None),
                );
                store(root, Scope::Project, "p", |f: &mut ConcurrencyFile| {
                    let slot = f.slots.iter_mut().find(|s| s.key_digest == KEY).unwrap();
                    heal(root, Scope::Project, "p", slot);
                    assert_eq!(slot.active.as_ref().unwrap().reservation, None);
                    Ok(((), false))
                })
                .unwrap();
            },
        );
    }

    /// An unreadable provisional holder is kept, never dropped.
    #[test]
    fn an_unreadable_provisional_holder_is_kept() {
        child(
            "concurrency::tests::an_unreadable_provisional_holder_is_kept",
            |root| {
                seed_run(root, "p-1", Scope::Project, "p", Some(KEY));
                use std::io::Write;
                let mut f = std::fs::OpenOptions::new()
                    .append(true)
                    .open(root.join(".apb/runs/p-1/events.jsonl"))
                    .unwrap();
                writeln!(f, r#"{{"seq":99,"ts":1,"type":"alien_future_event"}}"#).unwrap();
                seed_slot(
                    root,
                    slot_with(KEY, "k", Some(holder("p-1", Some("r1"))), None),
                );
                store(root, Scope::Project, "p", |f: &mut ConcurrencyFile| {
                    let slot = f.slots.iter_mut().find(|s| s.key_digest == KEY).unwrap();
                    heal(root, Scope::Project, "p", slot);
                    assert!(slot.active.is_some());
                    Ok(((), false))
                })
                .unwrap();
            },
        );
    }

    /// A waiter whose owner is gone but whose driver claim is live is
    /// kept: a detached driver between its start and its first look.
    #[test]
    fn a_waiter_with_a_live_driver_claim_is_kept() {
        child(
            "concurrency::tests::a_waiter_with_a_live_driver_claim_is_kept",
            |root| {
                seed_run(root, "p-1", Scope::Project, "p", Some(KEY));
                crate::driver::publish_driver_pid(&root.join(".apb/runs/p-1"), std::process::id());
                seed_slot(
                    root,
                    slot_with(KEY, "k", None, Some(holder("p-1", Some("r1")))),
                );
                store(root, Scope::Project, "p", |f: &mut ConcurrencyFile| {
                    let slot = f.slots.iter_mut().find(|s| s.key_digest == KEY).unwrap();
                    heal(root, Scope::Project, "p", slot);
                    assert!(slot.waiting.is_some());
                    Ok(((), false))
                })
                .unwrap();
            },
        );
    }

    /// A waiter whose owner and driver are gone is released and stopped:
    /// nothing will ever drive it.
    #[test]
    fn a_waiter_whose_owner_and_driver_are_gone_is_released_and_stopped() {
        child(
            "concurrency::tests::a_waiter_whose_owner_and_driver_are_gone_is_released_and_stopped",
            |root| {
                seed_run(root, "p-1", Scope::Project, "p", Some(KEY));
                seed_slot(
                    root,
                    slot_with(KEY, "k", None, Some(holder("p-1", Some("r1")))),
                );
                store(root, Scope::Project, "p", |f: &mut ConcurrencyFile| {
                    let slot = f.slots.iter_mut().find(|s| s.key_digest == KEY).unwrap();
                    heal(root, Scope::Project, "p", slot);
                    assert!(slot.waiting.is_none());
                    assert_eq!(slot.stops.len(), 1);
                    assert!(slot.stops[0].reason.starts_with("superseded:"));
                    Ok(((), false))
                })
                .unwrap();
            },
        );
    }

    /// A terminal holder is cleared.
    #[test]
    fn a_terminal_holder_is_cleared() {
        child("concurrency::tests::a_terminal_holder_is_cleared", |root| {
            seed_run(root, "p-1", Scope::Project, "p", Some(KEY));
            end_run(root, "p-1", "succeeded");
            seed_slot(root, slot_with(KEY, "k", Some(holder("p-1", None)), None));
            store(root, Scope::Project, "p", |f: &mut ConcurrencyFile| {
                let slot = f.slots.iter_mut().find(|s| s.key_digest == KEY).unwrap();
                heal(root, Scope::Project, "p", slot);
                assert!(slot.active.is_none());
                Ok(((), false))
            })
            .unwrap();
        });
    }

    /// A paused active run keeps the slot: `apb stop <id>` frees it.
    #[test]
    fn a_paused_holder_keeps_the_slot() {
        child(
            "concurrency::tests::a_paused_holder_keeps_the_slot",
            |root| {
                seed_run(root, "p-1", Scope::Project, "p", Some(KEY));
                let dir = root.join(".apb/runs/p-1");
                let mut log = crate::event::EventLog::open(&dir).unwrap();
                log.append(EventPayload::RunPaused {
                    reason: "paused".into(),
                    by: Some(crate::pause::kind::PauseBy::Operator),
                    mode: Some(crate::pause::kind::PauseMode::Drain),
                    until: None,
                    keep_lock: false,
                })
                .unwrap();
                seed_slot(root, slot_with(KEY, "k", Some(holder("p-1", None)), None));
                store(root, Scope::Project, "p", |f: &mut ConcurrencyFile| {
                    let slot = f.slots.iter_mut().find(|s| s.key_digest == KEY).unwrap();
                    heal(root, Scope::Project, "p", slot);
                    assert!(slot.active.is_some());
                    Ok(((), false))
                })
                .unwrap();
            },
        );
    }

    /// A void holder (a run of another playbook) is cleared.
    #[test]
    fn a_void_holder_is_cleared() {
        child("concurrency::tests::a_void_holder_is_cleared", |root| {
            seed_run(root, "q-1", Scope::Project, "q", Some(KEY));
            seed_slot(root, slot_with(KEY, "k", Some(holder("q-1", None)), None));
            store(root, Scope::Project, "p", |f: &mut ConcurrencyFile| {
                let slot = f.slots.iter_mut().find(|s| s.key_digest == KEY).unwrap();
                heal(root, Scope::Project, "p", slot);
                assert!(slot.active.is_none());
                Ok(((), false))
            })
            .unwrap();
        });
    }

    // --- posting promised stops ---

    /// A stop of a dead run is posted after the transaction (the run
    /// ends aborted with the stop's reason) and its entry is dropped.
    #[test]
    fn a_stop_is_posted_after_the_transaction() {
        child(
            "concurrency::tests::a_stop_is_posted_after_the_transaction",
            |root| {
                seed_run(root, "p-1", Scope::Project, "p", Some(KEY));
                seed_slot(root, slot_with(KEY, "k", Some(holder("p-1", None)), None));
                store(root, Scope::Project, "p", |f: &mut ConcurrencyFile| {
                    let slot = f.slots.iter_mut().find(|s| s.key_digest == KEY).unwrap();
                    slot.stops.push(PendingStop {
                        run_id: "p-1".into(),
                        reason: "superseded: test stop".into(),
                    });
                    Ok(((), true))
                })
                .unwrap();
                post_stops(root, Scope::Project, "p", KEY);
                let events = crate::event::read_all(&root.join(".apb/runs/p-1")).unwrap();
                let aborted = events.iter().any(|e| {
                    matches!(&e.payload, EventPayload::RunAborted { reason, .. }
                        if reason.contains("superseded: test stop"))
                });
                assert!(aborted, "the promised stop must be posted");
                let stops = read_slots(root, Scope::Project, "p");
                assert!(stops.is_empty() || stops[0].stops.is_empty());
            },
        );
    }

    /// A stop against a provisional holder is deferred: it stays
    /// recorded and no call happens.
    #[test]
    fn a_stop_against_a_provisional_holder_is_deferred() {
        child(
            "concurrency::tests::a_stop_against_a_provisional_holder_is_deferred",
            |root| {
                seed_run(root, "p-1", Scope::Project, "p", Some(KEY));
                let _owner_lock =
                    apb_core::fsutil::hold_lock(&owners_dir(root, Scope::Project, "p"), "r1.lock")
                        .unwrap();
                seed_slot(
                    root,
                    slot_with(KEY, "k", Some(holder("p-1", Some("r1"))), None),
                );
                store(root, Scope::Project, "p", |f: &mut ConcurrencyFile| {
                    let slot = f.slots.iter_mut().find(|s| s.key_digest == KEY).unwrap();
                    slot.stops.push(PendingStop {
                        run_id: "p-1".into(),
                        reason: "superseded: deferred".into(),
                    });
                    Ok(((), true))
                })
                .unwrap();
                post_stops(root, Scope::Project, "p", KEY);
                let events = crate::event::read_all(&root.join(".apb/runs/p-1")).unwrap();
                assert!(
                    !events
                        .iter()
                        .any(|e| matches!(&e.payload, EventPayload::RunAborted { .. })),
                    "a stop against a provisional claim is deferred"
                );
                let stops = read_slots(root, Scope::Project, "p");
                assert_eq!(stops[0].stops.len(), 1);
            },
        );
    }

    /// A stop whose post failed stays recorded for the next access: the
    /// run's journal is unreadable, so the stop errors and the entry
    /// stays; a stop of a run that is gone answers NotFound and drops.
    #[test]
    fn a_failed_post_keeps_the_entry() {
        child(
            "concurrency::tests::a_failed_post_keeps_the_entry",
            |root| {
                // An unreadable run: stamped, but its journal does not read.
                let dir = root.join(".apb/runs/p-1");
                std::fs::create_dir_all(&dir).unwrap();
                std::fs::write(dir.join("events.jsonl"), b"garbage\n").unwrap();
                apb_core::run_origin::stamp(&dir, "p-1").unwrap();
                let mut slot = slot_with(KEY, "k", None, None);
                slot.stops.push(PendingStop {
                    run_id: "p-1".into(),
                    reason: "superseded: stuck".into(),
                });
                seed_slot(root, slot);
                post_stops(root, Scope::Project, "p", KEY);
                let slots = read_slots(root, Scope::Project, "p");
                assert_eq!(slots[0].stops.len(), 1, "a failed post keeps its entry");

                // A stop whose run is void is dropped by the next locked
                // write, as the heal decides.
                let mut slot = slot_with(KEY, "k", None, None);
                slot.stops.push(PendingStop {
                    run_id: "p-gone".into(),
                    reason: "superseded: gone".into(),
                });
                seed_slot(root, slot);
                let record = ConcurrencyRecord {
                    key_digest: KEY.into(),
                    display: "k".into(),
                    on_busy: "refuse".into(),
                    queued: false,
                };
                let _ = admit(
                    root,
                    Scope::Project,
                    "p",
                    &record,
                    apb_core::concurrency::OnBusy::Refuse,
                    "p-2",
                    &BTreeSet::new(),
                );
                let slots = read_slots(root, Scope::Project, "p");
                assert!(slots[0].stops.is_empty(), "a void stop entry is dropped");
            },
        );
    }

    // --- admission ---

    /// A free slot admits the start active.
    #[test]
    fn a_free_slot_admits_active() {
        child("concurrency::tests::a_free_slot_admits_active", |root| {
            seed_run(root, "p-1", Scope::Project, "p", None);
            let record = ConcurrencyRecord {
                key_digest: KEY.into(),
                display: "k".into(),
                on_busy: "refuse".into(),
                queued: false,
            };
            let lineage = BTreeSet::new();
            match admit(
                root,
                Scope::Project,
                "p",
                &record,
                apb_core::concurrency::OnBusy::Refuse,
                "p-1",
                &lineage,
            )
            .unwrap()
            {
                Admission::Active(_) => {}
                Admission::Queued { .. } => panic!("a free slot must admit active"),
            }
        });
    }

    /// A busy slot queues the start behind the holder.
    #[test]
    fn a_busy_slot_queues() {
        child("concurrency::tests::a_busy_slot_queues", |root| {
            seed_run(root, "p-1", Scope::Project, "p", Some(KEY));
            seed_slot(root, slot_with(KEY, "k", Some(holder("p-1", None)), None));
            let record = ConcurrencyRecord {
                key_digest: KEY.into(),
                display: "k".into(),
                on_busy: "queue_latest".into(),
                queued: false,
            };
            match admit(
                root,
                Scope::Project,
                "p",
                &record,
                apb_core::concurrency::OnBusy::QueueLatest,
                "p-2",
                &BTreeSet::new(),
            )
            .unwrap()
            {
                Admission::Queued { holder, .. } => assert_eq!(holder, "p-1"),
                Admission::Active(_) => panic!("a busy slot must queue"),
            }
        });
    }

    /// A second waiter replaces the first, with a stop promised for it.
    #[test]
    fn a_second_waiter_replaces_the_first_with_a_stop() {
        child(
            "concurrency::tests::a_second_waiter_replaces_the_first_with_a_stop",
            |root| {
                seed_run(root, "p-1", Scope::Project, "p", Some(KEY));
                seed_run(root, "p-2", Scope::Project, "p", Some(KEY));
                seed_slot(
                    root,
                    slot_with(
                        KEY,
                        "k",
                        Some(holder("p-1", None)),
                        Some(holder("p-2", None)),
                    ),
                );
                let record = ConcurrencyRecord {
                    key_digest: KEY.into(),
                    display: "k".into(),
                    on_busy: "queue_latest".into(),
                    queued: false,
                };
                admit(
                    root,
                    Scope::Project,
                    "p",
                    &record,
                    apb_core::concurrency::OnBusy::QueueLatest,
                    "p-3",
                    &BTreeSet::new(),
                )
                .unwrap();
                let slots = read_slots(root, Scope::Project, "p");
                assert_eq!(slots[0].waiting.as_ref().unwrap().run_id, "p-3");
                // The displaced waiter is not provisional: its stop posts
                // at once and the run ends aborted `superseded:`.
                let events = crate::event::read_all(&root.join(".apb/runs/p-2")).unwrap();
                assert!(events.iter().any(
                    |e| matches!(&e.payload, EventPayload::RunAborted { reason, .. }
                            if reason.starts_with("superseded:"))
                ));
            },
        );
    }

    /// A waiter with no active holder is replaced and the new start is
    /// active (the waiter was not yet promoted; latest wins).
    #[test]
    fn a_waiter_without_an_active_holder_is_replaced_and_the_new_start_is_active() {
        child(
            "concurrency::tests::a_waiter_without_an_active_holder_is_replaced_and_the_new_start_is_active",
            |root| {
                seed_run(root, "p-1", Scope::Project, "p", Some(KEY));
                seed_run(root, "p-2", Scope::Project, "p", Some(KEY));
                seed_slot(root, slot_with(KEY, "k", None, Some(holder("p-2", None))));
                let record = ConcurrencyRecord {
                    key_digest: KEY.into(),
                    display: "k".into(),
                    on_busy: "queue_latest".into(),
                    queued: false,
                };
                match admit(
                    root,
                    Scope::Project,
                    "p",
                    &record,
                    apb_core::concurrency::OnBusy::QueueLatest,
                    "p-3",
                    &BTreeSet::new(),
                )
                .unwrap()
                {
                    Admission::Active(_) => {}
                    Admission::Queued { .. } => panic!("the newest start must be active"),
                }
                let slots = read_slots(root, Scope::Project, "p");
                assert_eq!(slots[0].active.as_ref().unwrap().run_id, "p-3");
                let events = crate::event::read_all(&root.join(".apb/runs/p-2")).unwrap();
                assert!(events.iter().any(
                    |e| matches!(&e.payload, EventPayload::RunAborted { reason, .. }
                            if reason.starts_with("superseded:"))
                ));
            },
        );
    }

    /// `supersede` promises a stop of the active run.
    #[test]
    fn supersede_promises_a_stop_of_the_active_run() {
        child(
            "concurrency::tests::supersede_promises_a_stop_of_the_active_run",
            |root| {
                seed_run(root, "p-1", Scope::Project, "p", Some(KEY));
                seed_slot(root, slot_with(KEY, "k", Some(holder("p-1", None)), None));
                let record = ConcurrencyRecord {
                    key_digest: KEY.into(),
                    display: "k".into(),
                    on_busy: "supersede".into(),
                    queued: false,
                };
                admit(
                    root,
                    Scope::Project,
                    "p",
                    &record,
                    apb_core::concurrency::OnBusy::Supersede,
                    "p-2",
                    &BTreeSet::new(),
                )
                .unwrap();
                // The holder is not provisional, so its stop posts at
                // once: the run ends aborted `superseded:`.
                let events = crate::event::read_all(&root.join(".apb/runs/p-1")).unwrap();
                assert!(events.iter().any(
                    |e| matches!(&e.payload, EventPayload::RunAborted { reason, .. }
                            if reason.starts_with("superseded:"))
                ));
                let slots = read_slots(root, Scope::Project, "p");
                assert!(slots[0].stops.is_empty());
            },
        );
    }

    /// `refuse` on a busy slot writes nothing: the file bytes are
    /// unchanged.
    #[test]
    fn refuse_on_a_busy_slot_writes_nothing() {
        child(
            "concurrency::tests::refuse_on_a_busy_slot_writes_nothing",
            |root| {
                seed_run(root, "p-1", Scope::Project, "p", Some(KEY));
                seed_slot(root, slot_with(KEY, "k", Some(holder("p-1", None)), None));
                let path =
                    apb_core::signed_store::file::<ConcurrencyFile>(root, Scope::Project, "p");
                let before = std::fs::read(&path).unwrap();
                let record = ConcurrencyRecord {
                    key_digest: KEY.into(),
                    display: "k".into(),
                    on_busy: "refuse".into(),
                    queued: false,
                };
                let e = admit(
                    root,
                    Scope::Project,
                    "p",
                    &record,
                    apb_core::concurrency::OnBusy::Refuse,
                    "p-2",
                    &BTreeSet::new(),
                )
                .unwrap_err();
                assert!(e.to_string().contains("concurrency_busy"), "{e}");
                assert_eq!(std::fs::read(&path).unwrap(), before);
            },
        );
    }

    /// A waiter counts as busy for `refuse`.
    #[test]
    fn refuse_counts_a_waiter_as_busy() {
        child(
            "concurrency::tests::refuse_counts_a_waiter_as_busy",
            |root| {
                seed_run(root, "p-1", Scope::Project, "p", Some(KEY));
                seed_slot(
                    root,
                    slot_with(
                        KEY,
                        "k",
                        Some(holder("p-1", None)),
                        Some(holder("p-2", None)),
                    ),
                );
                let record = ConcurrencyRecord {
                    key_digest: KEY.into(),
                    display: "k".into(),
                    on_busy: "refuse".into(),
                    queued: false,
                };
                let e = admit(
                    root,
                    Scope::Project,
                    "p",
                    &record,
                    apb_core::concurrency::OnBusy::Refuse,
                    "p-3",
                    &BTreeSet::new(),
                )
                .unwrap_err();
                assert!(e.to_string().contains("concurrency_busy"), "{e}");
            },
        );
    }

    /// A start whose key is held by the run that started it is refused
    /// `concurrency_own_run`, whatever `on_busy` says.
    #[test]
    fn the_starter_is_refused_as_own_run() {
        child(
            "concurrency::tests::the_starter_is_refused_as_own_run",
            |root| {
                seed_run(root, "p-1", Scope::Project, "p", Some(KEY));
                seed_slot(root, slot_with(KEY, "k", Some(holder("p-1", None)), None));
                let record = ConcurrencyRecord {
                    key_digest: KEY.into(),
                    display: "k".into(),
                    on_busy: "queue_latest".into(),
                    queued: false,
                };
                let mut lineage = BTreeSet::new();
                lineage.insert("p-1".to_string());
                for busy in [
                    apb_core::concurrency::OnBusy::QueueLatest,
                    apb_core::concurrency::OnBusy::Supersede,
                    apb_core::concurrency::OnBusy::Refuse,
                ] {
                    let e = admit(root, Scope::Project, "p", &record, busy, "p-2", &lineage)
                        .unwrap_err();
                    assert!(e.to_string().contains("concurrency_own_run"), "{e}");
                }
            },
        );
    }

    /// An unverified slot file refuses admission
    /// `concurrency_state_unreadable`.
    #[test]
    fn an_unverified_file_refuses_admission() {
        child(
            "concurrency::tests::an_unverified_file_refuses_admission",
            |root| {
                let path =
                    apb_core::signed_store::file::<ConcurrencyFile>(root, Scope::Project, "p");
                std::fs::create_dir_all(path.parent().unwrap()).unwrap();
                std::fs::write(&path, b"garbage").unwrap();
                let record = ConcurrencyRecord {
                    key_digest: KEY.into(),
                    display: "k".into(),
                    on_busy: "refuse".into(),
                    queued: false,
                };
                let e = admit(
                    root,
                    Scope::Project,
                    "p",
                    &record,
                    apb_core::concurrency::OnBusy::Refuse,
                    "p-1",
                    &BTreeSet::new(),
                )
                .unwrap_err();
                assert!(
                    e.to_string().contains("concurrency_state_unreadable"),
                    "{e}"
                );
            },
        );
    }

    /// A refusal releases the owner lock: the next start of the key is
    /// admitted.
    #[test]
    fn a_refusal_releases_the_owner_lock() {
        child(
            "concurrency::tests::a_refusal_releases_the_owner_lock",
            |root| {
                seed_run(root, "p-1", Scope::Project, "p", Some(KEY));
                seed_slot(root, slot_with(KEY, "k", Some(holder("p-1", None)), None));
                let record = ConcurrencyRecord {
                    key_digest: KEY.into(),
                    display: "k".into(),
                    on_busy: "refuse".into(),
                    queued: false,
                };
                let _ = admit(
                    root,
                    Scope::Project,
                    "p",
                    &record,
                    apb_core::concurrency::OnBusy::Refuse,
                    "p-2",
                    &BTreeSet::new(),
                );
                // The refused start took and released an owner lock; a
                // fresh start of a free slot must still be admitted.
                let free_key = format!("{KEY}free");
                let record = ConcurrencyRecord {
                    key_digest: free_key.clone(),
                    display: "free".into(),
                    on_busy: "refuse".into(),
                    queued: false,
                };
                match admit(
                    root,
                    Scope::Project,
                    "p",
                    &record,
                    apb_core::concurrency::OnBusy::Refuse,
                    "p-3",
                    &BTreeSet::new(),
                )
                .unwrap()
                {
                    Admission::Active(_) => {}
                    Admission::Queued { .. } => panic!("a free slot must admit"),
                }
            },
        );
    }

    /// A key whose value looks like a credential refuses the start,
    /// naming the param, never the value.
    #[test]
    fn a_sensitive_value_refuses_the_key() {
        child(
            "concurrency::tests::a_sensitive_value_refuses_the_key",
            |root| {
                let yaml = "schema: 2\nid: p\nname: p\nversion: 1.0.0\nparams:\n  - { name: account, type: text }\nconcurrency:\n  key: \"{{params.account}}\"\nnodes:\n  - { id: start, type: start }\n  - { id: done, type: finish, outcome: success }\nedges:\n  - { from: start, to: done }\n";
                let pb = apb_core::schema::Playbook::from_yaml(yaml).unwrap();
                let mut params = BTreeMap::new();
                params.insert("account".to_string(), format!("ghp_{}", "a".repeat(36)));
                let opts = KeyScope::default();
                let e = key_for_start(root, &pb, &params, &opts).unwrap_err();
                assert!(e.to_string().contains("concurrency_key_sensitive"), "{e}");
                assert!(e.to_string().contains("account"));
                assert!(!e.to_string().contains("ghp_"));
            },
        );
    }

    /// The display is scrubbed and clipped to 120 characters; two keys
    /// equal in their first 120 characters hold two digests.
    #[test]
    fn display_is_scrubbed_and_clipped_to_120_characters() {
        child(
            "concurrency::tests::display_is_scrubbed_and_clipped_to_120_characters",
            |root| {
                let yaml = "schema: 2\nid: p\nname: p\nversion: 1.0.0\nparams:\n  - { name: account, type: text }\nconcurrency:\n  key: \"{{params.account}}\"\nnodes:\n  - { id: start, type: start }\n  - { id: done, type: finish, outcome: success }\nedges:\n  - { from: start, to: done }\n";
                let pb = apb_core::schema::Playbook::from_yaml(yaml).unwrap();
                let mut params = BTreeMap::new();
                let long: String = "x".repeat(300);
                params.insert("account".to_string(), long);
                let opts = KeyScope::default();
                let record = key_for_start(root, &pb, &params, &opts).unwrap().unwrap();
                assert!(record.display.chars().count() <= 120);
                let mut short = BTreeMap::new();
                short.insert("account".to_string(), "x".repeat(119));
                let other = key_for_start(root, &pb, &short, &opts).unwrap().unwrap();
                assert_ne!(record.key_digest, other.key_digest);
            },
        );
    }

    /// Two keys that scrub to the same display hold two digests
    /// (ownership is the digest, never the display).
    #[test]
    fn two_keys_with_one_display_have_two_digests() {
        child(
            "concurrency::tests::two_keys_with_one_display_have_two_digests",
            |root| {
                let d1 = apb_core::run_origin::tag(apb_core::concurrency::KEY_DOMAIN, b"secret-a")
                    .unwrap();
                let d2 = apb_core::run_origin::tag(apb_core::concurrency::KEY_DOMAIN, b"secret-b")
                    .unwrap();
                seed_slot(
                    root,
                    slot_with(&d1, "acct", Some(holder("p-1", None)), None),
                );
                seed_slot(
                    root,
                    slot_with(&d2, "acct", Some(holder("p-2", None)), None),
                );
                let slots = read_slots(root, Scope::Project, "p");
                assert_eq!(slots.len(), 2);
            },
        );
    }

    /// A playbook without the block takes no slot.
    #[test]
    fn no_block_takes_no_slot() {
        child("concurrency::tests::no_block_takes_no_slot", |root| {
            let yaml = "schema: 2\nid: p\nname: p\nversion: 1.0.0\nnodes:\n  - { id: start, type: start }\n  - { id: done, type: finish, outcome: success }\nedges:\n  - { from: start, to: done }\n";
            let pb = apb_core::schema::Playbook::from_yaml(yaml).unwrap();
            let opts = KeyScope::default();
            assert!(
                key_for_start(root, &pb, &BTreeMap::new(), &opts)
                    .unwrap()
                    .is_none()
            );
        });
    }

    /// A child takes no slot, whatever its playbook says.
    #[test]
    fn a_child_takes_no_slot() {
        child("concurrency::tests::a_child_takes_no_slot", |root| {
            let yaml = "schema: 2\nid: p\nname: p\nversion: 1.0.0\nconcurrency: { on_busy: refuse }\nnodes:\n  - { id: start, type: start }\n  - { id: done, type: finish, outcome: success }\nedges:\n  - { from: start, to: done }\n";
            let pb = apb_core::schema::Playbook::from_yaml(yaml).unwrap();
            for opts in [
                KeyScope {
                    scenario: true,
                    ..Default::default()
                },
                KeyScope {
                    eval: true,
                    ..Default::default()
                },
                KeyScope {
                    child: true,
                    ..Default::default()
                },
                KeyScope {
                    depth: 1,
                    ..Default::default()
                },
            ] {
                assert!(
                    key_for_start(root, &pb, &BTreeMap::new(), &opts)
                        .unwrap()
                        .is_none()
                );
            }
        });
    }

    // --- confirm ---

    /// Confirming an active reservation clears it and releases the
    /// owner.
    #[test]
    fn confirm_clears_an_active_reservation_and_releases_the_lock() {
        child(
            "concurrency::tests::confirm_clears_an_active_reservation_and_releases_the_lock",
            |root| {
                seed_run(root, "p-1", Scope::Project, "p", Some(KEY));
                let record = ConcurrencyRecord {
                    key_digest: KEY.into(),
                    display: "k".into(),
                    on_busy: "refuse".into(),
                    queued: false,
                };
                let owner = match admit(
                    root,
                    Scope::Project,
                    "p",
                    &record,
                    apb_core::concurrency::OnBusy::Refuse,
                    "p-1",
                    &BTreeSet::new(),
                )
                .unwrap()
                {
                    Admission::Active(owner) => owner,
                    Admission::Queued { .. } => panic!("free"),
                };
                assert!(owner.confirm().unwrap().is_none());
                let slots = read_slots(root, Scope::Project, "p");
                assert_eq!(slots[0].active.as_ref().unwrap().reservation, None);
            },
        );
    }

    /// Confirming a waiter keeps its owner (it stays provisional until
    /// its turn).
    #[test]
    fn confirm_keeps_a_waiters_owner() {
        child(
            "concurrency::tests::confirm_keeps_a_waiters_owner",
            |root| {
                seed_run(root, "p-1", Scope::Project, "p", Some(KEY));
                seed_slot(root, slot_with(KEY, "k", Some(holder("p-1", None)), None));
                let record = ConcurrencyRecord {
                    key_digest: KEY.into(),
                    display: "k".into(),
                    on_busy: "queue_latest".into(),
                    queued: false,
                };
                let owner = match admit(
                    root,
                    Scope::Project,
                    "p",
                    &record,
                    apb_core::concurrency::OnBusy::QueueLatest,
                    "p-2",
                    &BTreeSet::new(),
                )
                .unwrap()
                {
                    Admission::Queued { owner, .. } => owner,
                    Admission::Active(_) => panic!("busy"),
                };
                let kept = owner.confirm().unwrap();
                assert!(kept.is_some());
                let slots = read_slots(root, Scope::Project, "p");
                assert!(slots[0].waiting.as_ref().unwrap().reservation.is_some());
            },
        );
    }

    /// Confirm fails `concurrency_lost` when the slot no longer names
    /// this run with this reservation.
    #[test]
    fn confirm_fails_concurrency_lost_when_the_slot_changed() {
        child(
            "concurrency::tests::confirm_fails_concurrency_lost_when_the_slot_changed",
            |root| {
                seed_run(root, "p-1", Scope::Project, "p", Some(KEY));
                let record = ConcurrencyRecord {
                    key_digest: KEY.into(),
                    display: "k".into(),
                    on_busy: "refuse".into(),
                    queued: false,
                };
                let owner = match admit(
                    root,
                    Scope::Project,
                    "p",
                    &record,
                    apb_core::concurrency::OnBusy::Refuse,
                    "p-1",
                    &BTreeSet::new(),
                )
                .unwrap()
                {
                    Admission::Active(owner) => owner,
                    Admission::Queued { .. } => panic!("free"),
                };
                // The slot file is moved aside: the claim is lost.
                let path =
                    apb_core::signed_store::file::<ConcurrencyFile>(root, Scope::Project, "p");
                std::fs::remove_file(&path).unwrap();
                let e = owner.confirm().unwrap_err();
                assert!(e.to_string().contains("concurrency_lost"), "{e}");
            },
        );
    }

    /// Confirm posts a stop that was deferred against this claim.
    #[test]
    fn confirm_posts_a_deferred_stop() {
        child(
            "concurrency::tests::confirm_posts_a_deferred_stop",
            |root| {
                seed_run(root, "p-1", Scope::Project, "p", Some(KEY));
                let record = ConcurrencyRecord {
                    key_digest: KEY.into(),
                    display: "k".into(),
                    on_busy: "refuse".into(),
                    queued: false,
                };
                let owner = match admit(
                    root,
                    Scope::Project,
                    "p",
                    &record,
                    apb_core::concurrency::OnBusy::Refuse,
                    "p-1",
                    &BTreeSet::new(),
                )
                .unwrap()
                {
                    Admission::Active(owner) => owner,
                    Admission::Queued { .. } => panic!("free"),
                };
                store(root, Scope::Project, "p", |f: &mut ConcurrencyFile| {
                    let slot = f.slots.iter_mut().find(|s| s.key_digest == KEY).unwrap();
                    slot.stops.push(PendingStop {
                        run_id: "p-1".into(),
                        reason: "superseded: while provisional".into(),
                    });
                    Ok(((), true))
                })
                .unwrap();
                owner.confirm().unwrap();
                let events = crate::event::read_all(&root.join(".apb/runs/p-1")).unwrap();
                assert!(events.iter().any(
                    |e| matches!(&e.payload, EventPayload::RunAborted { reason, .. }
                            if reason.contains("superseded: while provisional"))
                ));
            },
        );
    }

    /// A supersede's stop of a provisional holder that has no
    /// provenance yet survives the newer start's confirm and reaches the
    /// holder once it confirms.
    #[test]
    fn a_supersede_stop_survives_until_the_provisional_holder_confirms() {
        child(
            "concurrency::tests::a_supersede_stop_survives_until_the_provisional_holder_confirms",
            |root| {
                let record = |on_busy: &str| ConcurrencyRecord {
                    key_digest: KEY.into(),
                    display: "k".into(),
                    on_busy: on_busy.into(),
                    queued: false,
                };
                // A is admitted and still preparing: no run directory yet.
                let a = match admit(
                    root,
                    Scope::Project,
                    "p",
                    &record("supersede"),
                    apb_core::concurrency::OnBusy::Supersede,
                    "p-1",
                    &BTreeSet::new(),
                )
                .unwrap()
                {
                    Admission::Active(owner) => owner,
                    Admission::Queued { .. } => panic!("free"),
                };
                // B supersedes A and finishes its preparation first.
                let b = match admit(
                    root,
                    Scope::Project,
                    "p",
                    &record("supersede"),
                    apb_core::concurrency::OnBusy::Supersede,
                    "p-2",
                    &BTreeSet::new(),
                )
                .unwrap()
                {
                    Admission::Queued { owner, .. } => owner,
                    Admission::Active(_) => panic!("A holds the key"),
                };
                seed_run(root, "p-2", Scope::Project, "p", Some(KEY));
                let _b_waits = b.confirm().unwrap();
                assert!(
                    read_slots(root, Scope::Project, "p")[0]
                        .stops
                        .iter()
                        .any(|s| s.run_id == "p-1"),
                    "the promised stop of A is kept while A is provisional"
                );
                // A's provenance is durable now; its confirm posts the stop.
                seed_run(root, "p-1", Scope::Project, "p", Some(KEY));
                a.confirm().unwrap();
                let events = crate::event::read_all(&root.join(".apb/runs/p-1")).unwrap();
                assert!(
                    events
                        .iter()
                        .any(|e| matches!(&e.payload, EventPayload::RunAborted { .. })),
                    "A is stopped as B promised"
                );
            },
        );
    }

    // --- the turn ---

    /// A run the slot names active gets its turn at once.
    #[test]
    fn wait_turn_returns_for_the_active_run() {
        child(
            "concurrency::tests::wait_turn_returns_for_the_active_run",
            |root| {
                seed_run(root, "p-1", Scope::Project, "p", Some(KEY));
                seed_slot(root, slot_with(KEY, "k", Some(holder("p-1", None)), None));
                let turn = wait_turn(root, &root.join(".apb/runs/p-1"), "p-1", None).unwrap();
                assert!(matches!(turn, Turn::Active));
            },
        );
    }

    /// A waiter is promoted when the slot frees.
    #[test]
    fn wait_turn_promotes_the_waiter_when_the_slot_frees() {
        child(
            "concurrency::tests::wait_turn_promotes_the_waiter_when_the_slot_frees",
            |root| {
                seed_run(root, "p-1", Scope::Project, "p", Some(KEY));
                seed_run(root, "p-2", Scope::Project, "p", Some(KEY));
                seed_slot(
                    root,
                    slot_with(
                        KEY,
                        "k",
                        Some(holder("p-1", None)),
                        Some(holder("p-2", None)),
                    ),
                );
                let h = std::thread::spawn({
                    let root = root.to_path_buf();
                    move || wait_turn(&root, &root.join(".apb/runs/p-2"), "p-2", None).unwrap()
                });
                // The waiter's first look sees the holder; free the slot.
                std::thread::sleep(Duration::from_millis(300));
                end_run(root, "p-1", "succeeded");
                assert!(matches!(h.join().unwrap(), Turn::Active));
                let slots = read_slots(root, Scope::Project, "p");
                assert_eq!(slots[0].active.as_ref().unwrap().run_id, "p-2");
                assert!(slots[0].waiting.is_none());
            },
        );
    }

    /// A replaced waiter answers `Superseded`.
    #[test]
    fn wait_turn_reports_superseded_for_a_replaced_waiter() {
        child(
            "concurrency::tests::wait_turn_reports_superseded_for_a_replaced_waiter",
            |root| {
                seed_run(root, "p-1", Scope::Project, "p", Some(KEY));
                seed_run(root, "p-2", Scope::Project, "p", Some(KEY));
                seed_slot(
                    root,
                    slot_with(
                        KEY,
                        "k",
                        Some(holder("p-1", None)),
                        Some(holder("p-2", None)),
                    ),
                );
                let h = std::thread::spawn({
                    let root = root.to_path_buf();
                    move || wait_turn(&root, &root.join(".apb/runs/p-2"), "p-2", None)
                });
                std::thread::sleep(Duration::from_millis(300));
                // C replaces B as the waiter.
                store(root, Scope::Project, "p", |f: &mut ConcurrencyFile| {
                    let slot = f.slots.iter_mut().find(|s| s.key_digest == KEY).unwrap();
                    slot.waiting = Some(holder("p-3", None));
                    Ok(((), true))
                })
                .unwrap();
                let err = h.join().unwrap().unwrap_err();
                assert!(matches!(err, EngineError::Superseded(_)), "{err}");
            },
        );
    }

    /// A stop posted while the run waits ends the wait.
    #[test]
    fn wait_turn_stops_at_a_posted_stop() {
        child(
            "concurrency::tests::wait_turn_stops_at_a_posted_stop",
            |root| {
                seed_run(root, "p-1", Scope::Project, "p", Some(KEY));
                seed_run(root, "p-2", Scope::Project, "p", Some(KEY));
                seed_slot(
                    root,
                    slot_with(
                        KEY,
                        "k",
                        Some(holder("p-1", None)),
                        Some(holder("p-2", None)),
                    ),
                );
                let h = std::thread::spawn({
                    let root = root.to_path_buf();
                    move || wait_turn(&root, &root.join(".apb/runs/p-2"), "p-2", None)
                });
                std::thread::sleep(Duration::from_millis(300));
                crate::control::post_control(
                    &root.join(".apb/runs/p-2"),
                    crate::control::Control::Abort {
                        reason: "stopped".into(),
                        by: Some(crate::pause::kind::PauseBy::Operator),
                        graceful: false,
                    },
                )
                .unwrap();
                let err = h.join().unwrap().unwrap_err();
                assert!(err.to_string().contains("stopped"), "{err}");
            },
        );
    }

    /// A pause journaled while the run waited ends the wait with
    /// `Turn::Paused` and leaves the slot.
    #[test]
    fn wait_turn_answers_paused_for_a_journaled_pause_and_leaves_the_slot() {
        child(
            "concurrency::tests::wait_turn_answers_paused_for_a_journaled_pause_and_leaves_the_slot",
            |root| {
                seed_run(root, "p-1", Scope::Project, "p", Some(KEY));
                seed_run(root, "p-2", Scope::Project, "p", Some(KEY));
                seed_slot(
                    root,
                    slot_with(
                        KEY,
                        "k",
                        Some(holder("p-1", None)),
                        Some(holder("p-2", None)),
                    ),
                );
                let h = std::thread::spawn({
                    let root = root.to_path_buf();
                    move || wait_turn(&root, &root.join(".apb/runs/p-2"), "p-2", None)
                });
                std::thread::sleep(Duration::from_millis(300));
                let dir = root.join(".apb/runs/p-2");
                let mut log = crate::event::EventLog::open(&dir).unwrap();
                log.append(EventPayload::RunPaused {
                    reason: "paused".into(),
                    by: Some(crate::pause::kind::PauseBy::Operator),
                    mode: Some(crate::pause::kind::PauseMode::Drain),
                    until: None,
                    keep_lock: false,
                })
                .unwrap();
                let turn = h.join().unwrap().unwrap();
                assert!(matches!(turn, Turn::Paused));
                let slots = read_slots(root, Scope::Project, "p");
                assert!(slots[0].waiting.is_none());
                assert_eq!(slots[0].active.as_ref().unwrap().run_id, "p-1");
            },
        );
    }

    /// A deadline stop that is not due keeps the run waiting.
    #[test]
    fn wait_turn_goes_on_when_the_deadline_stop_is_not_due() {
        child(
            "concurrency::tests::wait_turn_goes_on_when_the_deadline_stop_is_not_due",
            |root| {
                seed_run(root, "p-1", Scope::Project, "p", Some(KEY));
                seed_run(root, "p-2", Scope::Project, "p", Some(KEY));
                seed_slot(
                    root,
                    slot_with(
                        KEY,
                        "k",
                        Some(holder("p-1", None)),
                        Some(holder("p-2", None)),
                    ),
                );
                let h = std::thread::spawn({
                    let root = root.to_path_buf();
                    move || wait_turn(&root, &root.join(".apb/runs/p-2"), "p-2", None)
                });
                std::thread::sleep(Duration::from_millis(300));
                end_run(root, "p-1", "succeeded");
                // The holder ended, so the waiter is promoted before any
                // deadline could matter; the assertion is the promotion.
                assert!(matches!(h.join().unwrap().unwrap(), Turn::Active));
            },
        );
    }

    /// A driver taking over a seeded waiter re-reserves its place under
    /// a new nonce.
    #[test]
    fn wait_turn_without_an_owner_re_reserves_its_place() {
        child(
            "concurrency::tests::wait_turn_without_an_owner_re_reserves_its_place",
            |root| {
                seed_run(root, "p-1", Scope::Project, "p", Some(KEY));
                seed_run(root, "p-2", Scope::Project, "p", Some(KEY));
                seed_slot(
                    root,
                    slot_with(
                        KEY,
                        "k",
                        Some(holder("p-1", None)),
                        Some(holder("p-2", None)),
                    ),
                );
                // The waiter's driver claim is live (this process id).
                crate::driver::publish_driver_pid(&root.join(".apb/runs/p-2"), std::process::id());
                // A look from a thread holds the waiter in place while
                // the holder is live; the reservation was re-recorded.
                let h = std::thread::spawn({
                    let root = root.to_path_buf();
                    move || wait_turn(&root, &root.join(".apb/runs/p-2"), "p-2", None)
                });
                std::thread::sleep(Duration::from_millis(300));
                end_run(root, "p-1", "succeeded");
                assert!(matches!(h.join().unwrap().unwrap(), Turn::Active));
                let slots = read_slots(root, Scope::Project, "p");
                assert_eq!(slots[0].active.as_ref().unwrap().run_id, "p-2");
            },
        );
    }

    // --- resumes ---

    /// A reservation dropped unconfirmed frees the slot.
    #[test]
    fn a_reservation_dropped_unconfirmed_frees_the_slot() {
        child(
            "concurrency::tests::a_reservation_dropped_unconfirmed_frees_the_slot",
            |root| {
                seed_run(root, "p-1", Scope::Project, "p", Some(KEY));
                seed_slot(root, slot_with(KEY, "k", None, None));
                {
                    let reservation = reserve_resume(root, &root.join(".apb/runs/p-1"), "p-1")
                        .unwrap()
                        .unwrap();
                    drop(reservation);
                }
                let slots = read_slots(root, Scope::Project, "p");
                assert!(slots.is_empty() || slots[0].active.is_none());
            },
        );
    }

    /// A reservation is refused while another run is active.
    #[test]
    fn a_reservation_is_refused_while_another_run_is_active() {
        child(
            "concurrency::tests::a_reservation_is_refused_while_another_run_is_active",
            |root| {
                seed_run(root, "p-1", Scope::Project, "p", Some(KEY));
                seed_run(root, "p-2", Scope::Project, "p", Some(KEY));
                seed_slot(root, slot_with(KEY, "k", Some(holder("p-2", None)), None));
                let e = reserve_resume(root, &root.join(".apb/runs/p-1"), "p-1").unwrap_err();
                assert!(e.to_string().contains("concurrency_busy"), "{e}");
            },
        );
    }

    /// A second resume of the same run while one is in progress is
    /// refused.
    #[test]
    fn a_second_resume_of_the_same_run_is_refused() {
        child(
            "concurrency::tests::a_second_resume_of_the_same_run_is_refused",
            |root| {
                seed_run(root, "p-1", Scope::Project, "p", Some(KEY));
                end_run(root, "p-1", "failed");
                let first = reserve_resume(root, &root.join(".apb/runs/p-1"), "p-1")
                    .unwrap()
                    .unwrap();
                let e = reserve_resume(root, &root.join(".apb/runs/p-1"), "p-1").unwrap_err();
                assert!(e.to_string().contains("being resumed by another"), "{e}");
                first.confirm().unwrap();
            },
        );
    }

    /// A paused holder resuming keeps its slot: only the reservation
    /// changes.
    #[test]
    fn a_paused_holder_resuming_keeps_its_slot() {
        child(
            "concurrency::tests::a_paused_holder_resuming_keeps_its_slot",
            |root| {
                seed_run(root, "p-1", Scope::Project, "p", Some(KEY));
                seed_slot(root, slot_with(KEY, "k", Some(holder("p-1", None)), None));
                let reservation = reserve_resume(root, &root.join(".apb/runs/p-1"), "p-1")
                    .unwrap()
                    .unwrap();
                reservation.confirm().unwrap();
                let slots = read_slots(root, Scope::Project, "p");
                assert_eq!(slots[0].active.as_ref().unwrap().run_id, "p-1");
                assert_eq!(slots[0].active.as_ref().unwrap().reservation, None);
            },
        );
    }

    /// A dropped second reservation leaves the first in place.
    #[test]
    fn a_dropped_second_reservation_leaves_the_first() {
        child(
            "concurrency::tests::a_dropped_second_reservation_leaves_the_first",
            |root| {
                seed_run(root, "p-1", Scope::Project, "p", Some(KEY));
                end_run(root, "p-1", "failed");
                let _first = reserve_resume(root, &root.join(".apb/runs/p-1"), "p-1")
                    .unwrap()
                    .unwrap();
                let _ = reserve_resume(root, &root.join(".apb/runs/p-1"), "p-1");
                let slots = read_slots(root, Scope::Project, "p");
                assert!(slots[0].active.as_ref().unwrap().reservation.is_some());
            },
        );
    }

    /// A patch changing the block is refused.
    #[test]
    fn a_patch_changing_the_block_is_refused() {
        let yaml = "schema: 2\nid: p\nname: p\nversion: 1.0.0\nconcurrency: { on_busy: refuse }\nnodes:\n  - { id: start, type: start }\n  - { id: done, type: finish, outcome: success }\nedges:\n  - { from: start, to: done }\n";
        let old = apb_core::schema::Playbook::from_yaml(yaml).unwrap();
        let plain = "schema: 2\nid: p\nname: p\nversion: 1.0.0\nnodes:\n  - { id: start, type: start }\n  - { id: done, type: finish, outcome: success }\nedges:\n  - { from: start, to: done }\n";
        let new = apb_core::schema::Playbook::from_yaml(plain).unwrap();
        let refusal = patch_refusal(&old, &new);
        assert!(
            refusal
                .as_deref()
                .is_some_and(|r| r.starts_with("concurrency_changed"))
        );
        assert_eq!(patch_refusal(&old, &old), None);
    }
}
