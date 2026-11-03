//! Cleanup of a run's result store (`runs/<id>/results/`): what no journal
//! stamp and no open transaction names is removed, and a transaction whose
//! writer died is resolved first.
//!
//! Everything happens under the run's results lock, and the journal is read
//! after the lock is taken: a transaction ends (under that lock) only after
//! its stamp is journaled, so a manifest is always named either by an open
//! transaction or by the journal this reads. A transaction is open while its
//! lock file is held; one whose lock is free belongs to a process that ended.
//! It is completed when the journal names its manifest and rolled back
//! otherwise; either way its staging goes, and what it published is then
//! kept or collected by the same rule as everything else.
//!
//! "Named by the journal" is read textually: every run of 64 lowercase hex
//! characters in `events.jsonl` protects the manifest of that id. A line
//! this binary cannot decode (a newer event, a torn write) still protects
//! what it names. Objects are collected only when every manifest kept was
//! read and verified; one that does not verify keeps every object.
//!
//! [`survey`] answers the same question without the lock and writes
//! nothing: `apb doctor --run` and a dry run use it. Snapshots live inside
//! the run directory and leave with it; cache prune and clear never reach
//! them, and a later command that deletes a run calls [`clean`] first.

use std::collections::BTreeSet;
use std::io;
use std::path::Path;

use apb_core::fsutil::beneath::{Beneath, EntryKind, ListBudget, LockState};
use apb_core::result_store::{
    self as store, MAX_MANIFEST_BYTES, OBJECTS_DIR, RESULTS_DIR, Reservation, SNAPSHOTS_DIR,
    SnapshotManifest, TXN_FILE, TxnRecord,
};

/// How a transaction whose writer ended was resolved.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeadOutcome {
    /// The journal names its manifest: it is kept.
    Completed,
    /// Nothing journaled it: what it published is collected.
    RolledBack,
}

/// One transaction whose writer ended without closing it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeadTxn {
    pub token: String,
    pub outcome: DeadOutcome,
}

/// What a cleanup removed, or a survey would remove.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CleanReport {
    /// Nothing was written (a survey or a dry run).
    pub dry_run: bool,
    /// Transactions whose writer ended, resolved or to be resolved.
    pub dead: Vec<DeadTxn>,
    /// Transactions open now; nothing they name is touched.
    pub open: usize,
    /// Manifests kept: named by a stamp or an open transaction.
    pub kept_snapshots: usize,
    /// Manifests no stamp and no open transaction names.
    pub removed_snapshots: Vec<String>,
    /// Objects referenced by nothing kept.
    pub removed_objects: usize,
    /// Bytes of the removed manifests and objects.
    pub removed_bytes: u64,
    /// A kept manifest did not verify, so no object was collected.
    pub objects_unverified: bool,
}

impl CleanReport {
    /// Whether the store holds anything to clean.
    pub fn has_leftovers(&self) -> bool {
        !self.dead.is_empty() || !self.removed_snapshots.is_empty() || self.removed_objects > 0
    }
}

/// Cleans the result store of the run at `run_dir`; with `dry_run`, only
/// reports what it would do ([`survey`]). A run without `results/` is
/// left alone.
pub fn clean(run_dir: &Path, dry_run: bool) -> io::Result<CleanReport> {
    if dry_run {
        return survey(run_dir);
    }
    let b = Beneath::open(run_dir)?;
    if b.kind(RESULTS_DIR)?.is_none() {
        return Ok(CleanReport::default());
    }
    let _lock = store::lock_results(&b)?;
    let plan = Plan::read(&b, run_dir)?;
    plan.apply(&b)?;
    Ok(plan.report(false))
}

/// What [`clean`] would do, read without the lock: writes nothing.
pub fn survey(run_dir: &Path) -> io::Result<CleanReport> {
    let b = Beneath::open(run_dir)?;
    if b.kind(RESULTS_DIR)?.is_none() {
        return Ok(CleanReport {
            dry_run: true,
            ..CleanReport::default()
        });
    }
    Ok(Plan::read(&b, run_dir)?.report(true))
}

/// Everything decided before anything is removed.
struct Plan {
    dead: Vec<(DeadTxn, bool)>,
    open: Vec<TxnRecord>,
    stray_staging: Vec<String>,
    kept_snapshots: BTreeSet<String>,
    removed_snapshots: Vec<String>,
    removed_objects: Vec<String>,
    removed_bytes: u64,
    objects_unverified: bool,
    /// The reservation as it stands once the plan is applied.
    reservation: Reservation,
}

impl Plan {
    fn read(b: &Beneath, run_dir: &Path) -> io::Result<Plan> {
        let named = journal_names(run_dir)?;
        let staging = staging_state(b)?;
        let mut protected: BTreeSet<String> = named.clone();
        let mut protected_objects = BTreeSet::new();
        // What open transactions will count at their close (the manifest
        // and the objects each moved in itself), so the recomputed total
        // leaves it out. An object one of them found in place is counted
        // here: its close does not count it.
        let mut held_manifests = BTreeSet::new();
        let mut held_added = BTreeSet::new();
        let mut dead = Vec::new();
        for (record, held, has_lock) in &staging.txns {
            if *held {
                protected.extend(record.manifest.iter().cloned());
                protected_objects.extend(record.objects.iter().cloned());
                held_manifests.extend(record.manifest.iter().cloned());
                held_added.extend(record.added.iter().cloned());
            } else {
                let outcome = match &record.manifest {
                    Some(id) if named.contains(id) => DeadOutcome::Completed,
                    _ => DeadOutcome::RolledBack,
                };
                dead.push((
                    DeadTxn {
                        token: record.token.clone(),
                        outcome,
                    },
                    *has_lock,
                ));
            }
        }
        let snapshots = names_in(b, &format!("{RESULTS_DIR}/{SNAPSHOTS_DIR}"))?;
        let mut kept_snapshots = BTreeSet::new();
        let mut removed_snapshots = Vec::new();
        let mut removed_bytes = 0;
        let mut objects_unverified = false;
        for name in snapshots {
            let id = name
                .strip_suffix(".json")
                .filter(|id| store::is_snapshot_id(id));
            match id {
                Some(id) if protected.contains(id) => {
                    match verified_objects(b, id)? {
                        Some(objects) => protected_objects.extend(objects),
                        None => objects_unverified = true,
                    }
                    kept_snapshots.insert(id.to_string());
                }
                _ => {
                    let rel = format!("{RESULTS_DIR}/{SNAPSHOTS_DIR}/{name}");
                    removed_bytes += b.file_size(&rel)?.unwrap_or(0);
                    removed_snapshots.push(name);
                }
            }
        }
        let mut removed_objects = Vec::new();
        let mut kept_bytes = 0;
        for rel in object_files(b)? {
            let hex = rel.rsplit('/').next().unwrap_or_default();
            let size = b.file_size(&rel)?.unwrap_or(0);
            if objects_unverified || protected_objects.contains(hex) {
                if !held_added.contains(hex) {
                    kept_bytes += size;
                }
            } else {
                removed_bytes += size;
                removed_objects.push(rel);
            }
        }
        for id in kept_snapshots.difference(&held_manifests) {
            kept_bytes += b.file_size(&store::snapshot_rel(id))?.unwrap_or(0);
        }
        let mut reservation = Reservation {
            committed_bytes: kept_bytes,
            ..Reservation::default()
        };
        let before = store::read_reservation(b).unwrap_or_default();
        for (record, held, _) in &staging.txns {
            if *held {
                let bytes = before.open.get(&record.token).copied().unwrap_or(0);
                reservation.open.insert(record.token.clone(), bytes);
                reservation.reserved_bytes += bytes;
            }
        }
        Ok(Plan {
            dead,
            open: staging
                .txns
                .into_iter()
                .filter(|(_, held, _)| *held)
                .map(|(r, _, _)| r)
                .collect(),
            stray_staging: staging.stray,
            kept_snapshots,
            removed_snapshots,
            removed_objects,
            removed_bytes,
            objects_unverified,
            reservation,
        })
    }

    fn apply(&self, b: &Beneath) -> io::Result<()> {
        let staging = store::staging_rel();
        for (txn, has_lock) in &self.dead {
            b.remove_tree(&format!("{staging}/{}", txn.token))?;
            if *has_lock {
                b.remove(&format!("{staging}/{}", store::txn_lock_name(&txn.token)))?;
            }
        }
        for name in &self.stray_staging {
            b.remove_tree(&format!("{staging}/{name}"))?;
        }
        for name in &self.removed_snapshots {
            b.remove(&format!("{RESULTS_DIR}/{SNAPSHOTS_DIR}/{name}"))?;
        }
        for rel in &self.removed_objects {
            b.remove(rel)?;
        }
        remove_empty_fanout(b)?;
        store::write_reservation(b, &self.reservation)
    }

    fn report(&self, dry_run: bool) -> CleanReport {
        CleanReport {
            dry_run,
            dead: self.dead.iter().map(|(d, _)| d.clone()).collect(),
            open: self.open.len(),
            kept_snapshots: self.kept_snapshots.len(),
            removed_snapshots: self.removed_snapshots.clone(),
            removed_objects: self.removed_objects.len(),
            removed_bytes: self.removed_bytes,
            objects_unverified: self.objects_unverified,
        }
    }
}

/// Every snapshot id the journal of `run_dir` names: each run of 64
/// lowercase hex characters bounded by anything else.
fn journal_names(run_dir: &Path) -> io::Result<BTreeSet<String>> {
    let bytes = match std::fs::read(run_dir.join("events.jsonl")) {
        Ok(b) => b,
        Err(e) if e.kind() == io::ErrorKind::NotFound => Vec::new(),
        Err(e) => return Err(e),
    };
    let mut out = BTreeSet::new();
    let hex = |c: &u8| matches!(c, b'0'..=b'9' | b'a'..=b'f');
    let mut i = 0;
    while i < bytes.len() {
        if !hex(&bytes[i]) {
            i += 1;
            continue;
        }
        let start = i;
        while i < bytes.len() && hex(&bytes[i]) {
            i += 1;
        }
        if i - start == 64 {
            out.insert(String::from_utf8_lossy(&bytes[start..i]).into_owned());
        }
    }
    Ok(out)
}

/// The staging directory: each transaction with whether its lock is held
/// and whether it has a lock file, and the names that are no transaction.
struct StagingState {
    txns: Vec<(TxnRecord, bool, bool)>,
    stray: Vec<String>,
}

fn staging_state(b: &Beneath) -> io::Result<StagingState> {
    let staging = store::staging_rel();
    let mut tokens = BTreeSet::new();
    let mut locks = BTreeSet::new();
    let mut stray = Vec::new();
    let mut budget = unbounded();
    let listing = match b.kind(&staging)? {
        Some(EntryKind::Dir) => b.list_dir(&staging, &mut budget)?.entries,
        _ => Vec::new(),
    };
    for entry in listing {
        let Some(name) = entry.name.to_str().map(str::to_string) else {
            continue;
        };
        if entry.kind == EntryKind::Dir && store::is_token(&name) {
            tokens.insert(name);
        } else if entry.kind == EntryKind::File
            && let Some(token) = name.strip_suffix(".lock").filter(|t| store::is_token(t))
        {
            locks.insert(token.to_string());
        } else {
            stray.push(name);
        }
    }
    let mut txns = Vec::new();
    for token in tokens.union(&locks) {
        let lock = b.probe_lock(&format!("{staging}/{}", store::txn_lock_name(token)))?;
        let held = lock == LockState::Held;
        let record = if tokens.contains(token) {
            store::read_json::<TxnRecord>(b, &format!("{staging}/{token}/{TXN_FILE}"))
                .ok()
                .flatten()
        } else {
            None
        };
        let record = record.unwrap_or_else(|| TxnRecord {
            token: token.clone(),
            ..TxnRecord::default()
        });
        // A record naming another token is read as naming nothing.
        let record = if record.token == *token {
            record
        } else {
            TxnRecord {
                token: token.clone(),
                ..TxnRecord::default()
            }
        };
        txns.push((record, held, lock != LockState::Absent));
    }
    Ok(StagingState { txns, stray })
}

/// The objects a kept manifest names, `None` when it does not read or
/// does not hash to its id.
fn verified_objects(b: &Beneath, id: &str) -> io::Result<Option<Vec<String>>> {
    use apb_core::fsutil::beneath::CappedRead;
    let bytes = match b.read_regular_capped(&store::snapshot_rel(id), MAX_MANIFEST_BYTES as u64)? {
        CappedRead::Read { bytes, .. } => bytes,
        CappedRead::Refused(_) => return Ok(None),
    };
    if store::snapshot_id(&bytes) != id {
        return Ok(None);
    }
    let Ok(manifest) = serde_json::from_slice::<SnapshotManifest>(&bytes) else {
        return Ok(None);
    };
    Ok(Some(
        manifest
            .entries
            .iter()
            .filter_map(|e| store::digest_hex(&e.digest).map(str::to_string))
            .collect(),
    ))
}

/// Every entry of `objects/<aa>/` as a path relative to the run directory.
fn object_files(b: &Beneath) -> io::Result<Vec<String>> {
    let objects = format!("{RESULTS_DIR}/{OBJECTS_DIR}");
    let mut out = Vec::new();
    for fan in names_in(b, &objects)? {
        let dir = format!("{objects}/{fan}");
        if b.kind(&dir)? != Some(EntryKind::Dir) {
            out.push(dir);
            continue;
        }
        out.extend(names_in(b, &dir)?.into_iter().map(|n| format!("{dir}/{n}")));
    }
    Ok(out)
}

/// Removes every `objects/<aa>/` left empty.
fn remove_empty_fanout(b: &Beneath) -> io::Result<()> {
    let objects = format!("{RESULTS_DIR}/{OBJECTS_DIR}");
    for fan in names_in(b, &objects)? {
        let dir = format!("{objects}/{fan}");
        if b.kind(&dir)? == Some(EntryKind::Dir) && names_in(b, &dir)?.is_empty() {
            b.remove(&dir)?;
        }
    }
    Ok(())
}

/// The UTF-8 names of the directory `rel`; an absent directory has none.
fn names_in(b: &Beneath, rel: &str) -> io::Result<Vec<String>> {
    if b.kind(rel)? != Some(EntryKind::Dir) {
        return Ok(Vec::new());
    }
    Ok(b.list_dir(rel, &mut unbounded())?
        .entries
        .into_iter()
        .filter_map(|e| e.name.into_string().ok())
        .collect())
}

fn unbounded() -> ListBudget {
    ListBudget {
        remaining: usize::MAX,
    }
}
