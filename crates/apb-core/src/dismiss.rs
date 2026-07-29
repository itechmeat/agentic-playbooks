//! Store of the user's decisions about "make this a playbook" suggestions
//! (spec 2026-07-29-suggestion-decisions-design, evolving spec 8.2).
//!
//! One record per suggestion pattern, in two scopes: the project store
//! `<root>/.apb/suggestions.json` and the global store
//! `<config-dir>/suggestions.json`. A record carries the agent's one-sentence
//! `synopsis`, so future matching is done by MEANING on the agent side; the
//! server never does language processing. A soft decline escalates the snooze
//! along `SOFT_BACKOFF_DAYS`, a hard decline silences the suggestion for
//! `HARD_TTL_DAYS` (a long silence with an expiry, not a permanent ban).
//!
//! Reads never fail: a corrupt, unreadable, or newer-schema file yields an
//! empty list plus a diagnostic string, and a read that cannot take the dir
//! lock reports the store unpruned rather than rewriting it unsynchronized.
//! Writes are the opposite: a mutation that cannot take the lock, that meets a
//! schema this build does not understand, or that has nowhere to store a
//! global record fails loudly instead of confirming a decision that was never
//! persisted.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::fsutil::{atomic_write_private, lock_dir};

/// Escalating snooze for repeated soft declines, in days. The Nth soft decline
/// (1-based) uses `SOFT_BACKOFF_DAYS[min(N - 1, len - 1)]`, so the last entry
/// is the schedule tail that every further decline reuses.
pub const SOFT_BACKOFF_DAYS: [u64; 4] = [1, 7, 30, 90];
/// Silence window for an explicit "do not suggest this again", in days.
pub const HARD_TTL_DAYS: u64 = 90;
/// How long an inactive SOFT record is kept after its snooze ends, in days.
/// The record stops suppressing the suggestion the moment the snooze expires,
/// but its `declines` counter has to survive that moment, otherwise the next
/// soft decline would restart the backoff at one day and "each soft decline
/// pushes the next offer further out" would not hold. A hard record has
/// nothing to escalate, so it is pruned at its expiry.
pub const SOFT_RETAIN_DAYS: u64 = 365;

const SCHEMA_VERSION: u32 = 2;
const MS_PER_DAY: u64 = 24 * 60 * 60 * 1000;
const STORE_FILE: &str = "suggestions.json";
const LEGACY_FILE: &str = "dismissed.json";
const LOCK_NAME: &str = "suggestions.json.lock";
/// Where an unreadable or unparseable store is moved before a write replaces
/// it, so the bytes the user actually had on disk survive the repair.
const CORRUPT_FILE: &str = "suggestions.json.corrupt";
/// What a global mutation reports when there is no config dir at all. The
/// record has nowhere to be written, and answering `Ok` confirmed a decision
/// that was never stored; this is the same failure a global `playbook_capture`
/// already reports.
const NO_GLOBAL_CONFIG_DIR: &str =
    "no global config dir; set APB_CONFIG_DIR, XDG_CONFIG_HOME or HOME to record a global decision";

/// Kind of decision. `Hard` is the default on read, so a record written by a
/// future version without the field is treated as the stricter of the two.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DecisionKind {
    Soft,
    Hard,
}

impl DecisionKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            DecisionKind::Soft => "soft",
            DecisionKind::Hard => "hard",
        }
    }

    pub fn parse(s: &str) -> Option<DecisionKind> {
        match s {
            "soft" => Some(DecisionKind::Soft),
            "hard" => Some(DecisionKind::Hard),
            _ => None,
        }
    }
}

fn default_kind() -> DecisionKind {
    DecisionKind::Hard
}

/// Which of the two stores a record lives in. A global record suppresses the
/// suggestion in every project, a project record only in its own.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DecisionScope {
    Project,
    Global,
}

impl DecisionScope {
    pub fn as_str(&self) -> &'static str {
        match self {
            DecisionScope::Project => "project",
            DecisionScope::Global => "global",
        }
    }

    pub fn parse(s: &str) -> Option<DecisionScope> {
        match s {
            "project" => Some(DecisionScope::Project),
            "global" => Some(DecisionScope::Global),
            _ => None,
        }
    }
}

/// One stored decision. `snoozed_until_ms` and `updated_at_ms` are epoch
/// milliseconds (the apb convention for every persisted timestamp); the
/// user-facing surfaces render them through [`iso_utc`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SuggestionRecord {
    pub pattern: String,
    #[serde(default)]
    pub synopsis: String,
    #[serde(default = "default_kind")]
    pub kind: DecisionKind,
    #[serde(default)]
    pub declines: u32,
    #[serde(default)]
    pub snoozed_until_ms: u64,
    #[serde(default)]
    pub updated_at_ms: u64,
}

/// A record together with the scope it was read from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScopedRecord {
    pub scope: DecisionScope,
    pub record: SuggestionRecord,
}

/// The merged read model: the records that currently suppress a suggestion in
/// this project, plus any diagnostics collected while reading the two files.
#[derive(Debug, Clone, Default)]
pub struct SuggestionView {
    pub records: Vec<ScopedRecord>,
    pub diagnostics: Vec<String>,
}

#[derive(Debug, Serialize, Deserialize)]
struct StoreFile {
    #[serde(default = "default_schema")]
    schema: u32,
    #[serde(default)]
    records: Vec<SuggestionRecord>,
}

fn default_schema() -> u32 {
    SCHEMA_VERSION
}

impl Default for StoreFile {
    fn default() -> Self {
        Self {
            schema: SCHEMA_VERSION,
            records: Vec::new(),
        }
    }
}

/// Directory holding the store for a scope: `<root>/.apb` for project scope,
/// the global config dir for global scope. `None` for global when there is no
/// config dir at all (the config-less path stays functional).
///
/// The one-time v1 to v2 migration runs here, in the global branch, and
/// nowhere else. That is the single choke point every global-scope entry
/// point goes through, read and write alike, which is exactly what the
/// migration needs: `migrate_legacy` gives up the moment `suggestions.json`
/// exists, so a global-scope WRITE that ran before any read would otherwise
/// create the new file first and strand the user's v1 `dismissed.json` on
/// disk forever. Hanging the migration off the read path alone made that
/// ordering silently lossy.
/// `Ok(None)` for global scope when there is no config dir at all. `Err` when
/// the migration could not run under the dir lock (see [`global_store_dir`]);
/// the never-fail read path turns that into a diagnostic, a mutation into a
/// refusal.
fn store_dir(
    root: &Path,
    scope: DecisionScope,
    diagnostics: &mut Vec<String>,
) -> std::io::Result<Option<PathBuf>> {
    match scope {
        DecisionScope::Project => Ok(Some(root.join(".apb"))),
        DecisionScope::Global => global_store_dir(diagnostics),
    }
}

/// The global store dir, running the one-time v1 to v2 migration on the way
/// through (see [`store_dir`] for why the migration hangs here). The migration
/// rewrites the store, so it takes the same dir lock every mutation takes: a
/// failed acquisition is an error rather than a rewrite with no mutual
/// exclusion. It is attempted at all only while a migration is actually
/// pending, so the lock is not touched on the normal path.
fn global_store_dir(diagnostics: &mut Vec<String>) -> std::io::Result<Option<PathBuf>> {
    let Some(dir) = crate::config::config_dir() else {
        return Ok(None);
    };
    migrate_legacy(&dir, crate::clock::now_ms_u64(), diagnostics)?;
    Ok(Some(dir))
}

/// `store_dir` for the callers whose return type cannot carry a diagnostic.
/// The migration still runs (that is the point); only its diagnostics are
/// dropped, exactly as they were before diagnostics existed on this path. A
/// migration FAILURE is still propagated: it is the caller's own operation
/// that cannot proceed safely.
fn store_dir_quiet(root: &Path, scope: DecisionScope) -> std::io::Result<Option<PathBuf>> {
    store_dir(root, scope, &mut Vec::new())
}

/// The store dir for a MUTATION: a global write with no config dir at all has
/// nowhere to persist the record, so it fails instead of reporting a success
/// that left no state behind.
fn mutation_dir(
    root: &Path,
    scope: DecisionScope,
    diagnostics: &mut Vec<String>,
) -> std::io::Result<PathBuf> {
    store_dir(root, scope, diagnostics)?
        .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::NotFound, NO_GLOBAL_CONFIG_DIR))
}

/// Why a store on disk cannot be used as it is. The distinction decides what a
/// write may do with the file: a corrupt one is moved aside and replaced, while
/// one that merely carries a NEWER schema holds intact data this build cannot
/// represent and must never be rewritten, downgraded, or quarantined.
enum StoreError {
    Corrupt(String),
    UnsupportedSchema(String),
}

impl StoreError {
    fn message(self) -> String {
        match self {
            StoreError::Corrupt(m) | StoreError::UnsupportedSchema(m) => m,
        }
    }
}

/// Reads one store file. `Ok(None)` when the file is absent (a normal empty
/// store); `Err` carries a human-readable diagnostic, which the read path
/// surfaces without failing.
///
/// Only [`SCHEMA_VERSION`] is accepted. A file written by a newer apb parses
/// here (neither `StoreFile` nor `SuggestionRecord` denies unknown fields), but
/// every write re-serializes only the fields THIS build knows and stamps the
/// current schema on the way out, so accepting a newer file would silently
/// downgrade it and drop whatever the newer version stored. Anything that is
/// not schema 2 is therefore read-only here.
fn read_store(path: &Path) -> Result<Option<StoreFile>, StoreError> {
    let raw = match std::fs::read_to_string(path) {
        Ok(r) => r,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => {
            return Err(StoreError::Corrupt(format!(
                "unreadable suggestion store `{}`: {e}",
                path.display()
            )));
        }
    };
    let file: StoreFile = serde_json::from_str(&raw).map_err(|e| {
        StoreError::Corrupt(format!(
            "malformed suggestion store `{}`: {e}",
            path.display()
        ))
    })?;
    if file.schema != SCHEMA_VERSION {
        return Err(StoreError::UnsupportedSchema(format!(
            "suggestion store `{}` has schema {}, this apb understands schema {SCHEMA_VERSION}; the file is left untouched",
            path.display(),
            file.schema
        )));
    }
    Ok(Some(file))
}

fn write_store(dir: &Path, file: &StoreFile) -> std::io::Result<()> {
    let bytes = serde_json::to_vec_pretty(file).map_err(std::io::Error::other)?;
    atomic_write_private(&dir.join(STORE_FILE), &bytes)
}

/// Whether the record still suppresses the suggestion.
fn is_active(rec: &SuggestionRecord, now: u64) -> bool {
    now < rec.snoozed_until_ms
}

/// Whether the record is still worth keeping on disk. A soft record outlives
/// its snooze by `SOFT_RETAIN_DAYS` so its decline counter can escalate the
/// next decline; a hard record is dropped at its expiry. Retention counts from
/// the later of the snooze end and the last write, so a record whose snooze was
/// cleared by `reset_records` keeps its synopsis instead of being pruned on the
/// very next read.
fn is_retained(rec: &SuggestionRecord, now: u64) -> bool {
    match rec.kind {
        DecisionKind::Hard => is_active(rec, now),
        DecisionKind::Soft => {
            let from = rec.snoozed_until_ms.max(rec.updated_at_ms);
            now < from.saturating_add(SOFT_RETAIN_DAYS.saturating_mul(MS_PER_DAY))
        }
    }
}

/// Loads one scope under its dir lock, pruning records that are no longer
/// retained and rewriting the file only when the prune changed something AND
/// the lock was actually acquired.
///
/// The read itself never fails, so a lock that could not be taken is not an
/// error here; the prune-WRITE, however, is a read-modify-write like any other
/// and must not run without mutual exclusion, or it would clobber a concurrent
/// writer's newer record. Without the lock the snapshot is reported as it
/// stands, with a diagnostic, and the prune waits for a later read.
///
/// Checks for the store file before taking the dir lock: `lock_dir` creates
/// the scope directory (`create_dir_all`) as a side effect, and `.apb`
/// existence is an adoption marker elsewhere in apb (registry
/// auto-registration, "already initialized" checks), so a pure read must
/// never materialize it. The resulting TOCTOU (a store written between this
/// check and a concurrent writer's lock) is benign: the next read picks it
/// up.
fn load_scope(dir: &Path, now: u64, diagnostics: &mut Vec<String>) -> Vec<SuggestionRecord> {
    let store_path = dir.join(STORE_FILE);
    if !store_path.is_file() {
        return Vec::new();
    }
    let lock = match lock_dir(dir, LOCK_NAME) {
        Ok(l) => Some(l),
        Err(e) => {
            diagnostics.push(format!(
                "could not lock `{}` to prune expired records: {e}; the store is reported unpruned",
                dir.display()
            ));
            None
        }
    };
    let mut file = match read_store(&store_path) {
        Ok(Some(f)) => f,
        Ok(None) => StoreFile::default(),
        Err(e) => {
            diagnostics.push(e.message());
            return Vec::new();
        }
    };
    let before = file.records.len();
    file.records.retain(|r| is_retained(r, now));
    if file.records.len() != before
        && lock.is_some()
        && let Err(e) = write_store(dir, &file)
    {
        diagnostics.push(format!("could not prune `{}`: {e}", dir.display()));
    }
    file.records
}

/// The merged, currently-suppressing records for this project: project scope
/// plus global scope, stricter-wins on a pattern present in both. Never fails.
pub fn active(root: &Path) -> SuggestionView {
    view_for(Some(root))
}

/// The currently-suppressing records of the GLOBAL scope alone, for a surface
/// that has no project to read against: the dashboard on a machine with zero
/// registered projects still has to show (and offer to remove) the
/// machine-wide decisions, and there is no workspace id to resolve a root
/// from. Never fails, exactly like [`active`].
pub fn active_global() -> SuggestionView {
    view_for(None)
}

/// Shared body of [`active`] and [`active_global`]. `None` is the global-only
/// view: the project scope is not read at all, which is why the placeholder
/// root below is never used for anything (`store_dir` ignores its root
/// argument for the global scope).
fn view_for(root: Option<&Path>) -> SuggestionView {
    let now = crate::clock::now_ms_u64();
    let mut diagnostics = Vec::new();
    let mut scoped: Vec<ScopedRecord> = Vec::new();
    let scopes: &[DecisionScope] = match root {
        Some(_) => &[DecisionScope::Project, DecisionScope::Global],
        None => &[DecisionScope::Global],
    };
    for &scope in scopes {
        let dir = match store_dir(root.unwrap_or(Path::new("")), scope, &mut diagnostics) {
            Ok(Some(d)) => d,
            Ok(None) => continue,
            Err(e) => {
                diagnostics.push(format!(
                    "{} suggestion store is unavailable: {e}",
                    scope.as_str()
                ));
                continue;
            }
        };
        for record in load_scope(&dir, now, &mut diagnostics) {
            if is_active(&record, now) {
                scoped.push(ScopedRecord { scope, record });
            }
        }
    }
    SuggestionView {
        records: merge_scopes(scoped),
        diagnostics,
    }
}

/// Reads, mutates, and conditionally writes back the store for one directory
/// under a single `lock_dir` acquisition. Every caller that changes a store
/// (`write_record`, `remove_record`, `reset_records`, `record_decision`) goes
/// through this one function, so "read the current state, fold in the
/// change, write" is always one atomic critical section under the real
/// cross-process lock file, never a read followed by a separately-locked
/// write (that gap is exactly how two concurrent soft declines on the same
/// pattern could both read `declines = N` and both write `N + 1`, silently
/// losing one escalation step). `mutate` returns `(value, changed)`; the
/// store is written only when `changed` is true, so a no-op call (e.g.
/// removing a pattern that isn't there) costs no write.
///
/// A store that is present but unreadable or unparseable is treated as empty
/// for the fold (a write must not be blocked by a broken file), but the
/// original bytes are NOT discarded: right before the replacement is written,
/// the broken file is renamed to `suggestions.json.corrupt`, so the user's
/// decisions can still be recovered by hand. Without that rename the very
/// next decline would silently overwrite the only copy. The rename is
/// best-effort: if it fails, the failure is reported as a diagnostic and the
/// write proceeds, because refusing to record the decision would be the worse
/// outcome.
///
/// Two things this function refuses outright rather than working around:
///
/// - A lock that could not be acquired. Proceeding would make the whole
///   critical section above a fiction: two declines could read the same
///   `declines = N` and both write `N + 1`, and a remove could drop a record a
///   concurrent writer had just added. `lock_dir` already waits, and steals
///   only a demonstrably stale lock, so a failure here means a live owner (or a
///   real IO fault) and the honest answer is an error the caller surfaces.
/// - A store whose schema this build does not understand. Unlike a corrupt
///   file, that one holds intact data written by a newer apb; rewriting it
///   would silently downgrade it, and quarantining it would treat good data as
///   broken.
fn with_locked_store<T>(
    dir: &Path,
    diagnostics: &mut Vec<String>,
    mutate: impl FnOnce(&mut StoreFile) -> (T, bool),
) -> std::io::Result<T> {
    let _lock = lock_dir(dir, LOCK_NAME)?;
    let path = dir.join(STORE_FILE);
    let (mut file, corrupt) = match read_store(&path) {
        Ok(Some(f)) => (f, false),
        Ok(None) => (StoreFile::default(), false),
        Err(e @ StoreError::UnsupportedSchema(_)) => {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                e.message(),
            ));
        }
        Err(e) => {
            diagnostics.push(e.message());
            (StoreFile::default(), true)
        }
    };
    let (value, changed) = mutate(&mut file);
    if changed {
        if corrupt {
            quarantine_corrupt(&path, diagnostics);
        }
        file.schema = SCHEMA_VERSION;
        write_store(dir, &file)?;
    }
    Ok(value)
}

/// Moves an unreadable store aside to `suggestions.json.corrupt` so the write
/// that follows cannot destroy it. Same-directory rename, so it is atomic on
/// every platform apb targets.
fn quarantine_corrupt(path: &Path, diagnostics: &mut Vec<String>) {
    let Some(dir) = path.parent() else {
        return;
    };
    let target = dir.join(CORRUPT_FILE);
    match std::fs::rename(path, &target) {
        Ok(()) => diagnostics.push(format!(
            "kept the unreadable store as `{}` before rewriting it",
            target.display()
        )),
        Err(e) => diagnostics.push(format!(
            "could not keep the unreadable store `{}` aside: {e}",
            path.display()
        )),
    }
}

/// A suggestion pattern is a stable record id that later has to survive a
/// path join (`apb suggestions allow <pattern>`) and a URL segment (the
/// dashboard's `DELETE /api/suggestions/{pattern}`, which rejects anything
/// `is_safe_id` would). Enforcing the apb slug rule at WRITE time is what
/// keeps those surfaces honest: a record written with a slash, a dot, or an
/// empty name could be stored but never removed again.
pub fn validate_pattern(pattern: &str) -> Result<(), String> {
    crate::profile::validate_slug("suggestion pattern", pattern)
}

/// `validate_pattern` in the `io::Result` shape every store mutator returns.
fn check_pattern(pattern: &str) -> std::io::Result<()> {
    validate_pattern(pattern).map_err(|m| std::io::Error::new(std::io::ErrorKind::InvalidInput, m))
}

/// Writes (inserts or replaces) a record in one scope, under that scope's dir
/// lock. The pattern is validated first, so no surface can persist a record it
/// would later be unable to address. A global write with no config dir at all
/// fails rather than reporting a success that stored nothing.
pub fn write_record(
    root: &Path,
    scope: DecisionScope,
    record: SuggestionRecord,
) -> std::io::Result<()> {
    check_pattern(&record.pattern)?;
    let dir = mutation_dir(root, scope, &mut Vec::new())?;
    with_locked_store(&dir, &mut Vec::new(), |file| {
        file.records.retain(|r| r.pattern != record.pattern);
        file.records.push(record);
        file.records.sort_by(|a, b| a.pattern.cmp(&b.pattern));
        ((), true)
    })
}

/// What a removal actually achieved: whether a record was removed, and whether
/// the pattern is STILL silenced afterwards by the other scope's record.
///
/// The same pattern can exist in both the project and the global store, and a
/// removal only ever touches the scope it was asked for. Reporting "removed"
/// alone let every surface promise that the suggestion can be offered again
/// while the other scope kept suppressing it, which the dashboard made obvious:
/// the card reappeared on the very next load, next to a success toast.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct RemoveOutcome {
    pub removed: bool,
    /// The scope whose record still suppresses this pattern, if any. Always
    /// `None` when nothing was removed.
    pub still_suppressed_by: Option<DecisionScope>,
}

/// Removes a record from one scope, reporting whether the pattern is still
/// suppressed by the other scope afterwards. A default [`RemoveOutcome`]
/// (`removed: false`) when there was nothing to remove.
///
/// Checks for the store file before taking the dir lock, exactly like
/// `load_scope`: `lock_dir` (inside `with_locked_store`) would otherwise
/// `create_dir_all` the scope directory as a side effect, and for the project
/// scope that directory is `.apb`, whose existence is an adoption marker
/// elsewhere in apb. A pure "nothing to remove" outcome must never
/// materialize it. The resulting TOCTOU (a store written between this check
/// and a concurrent writer's lock) is benign for the same reason it is in
/// `load_scope`: at worst this call reports "nothing to remove" once and a
/// retry sees the new file.
pub fn remove_record(
    root: &Path,
    pattern: &str,
    scope: DecisionScope,
) -> std::io::Result<RemoveOutcome> {
    let Some(dir) = store_dir_quiet(root, scope)? else {
        return Ok(RemoveOutcome::default());
    };
    if !dir.join(STORE_FILE).is_file() {
        return Ok(RemoveOutcome::default());
    }
    let removed = with_locked_store(&dir, &mut Vec::new(), |file| {
        let before = file.records.len();
        file.records.retain(|r| r.pattern != pattern);
        let removed = file.records.len() != before;
        (removed, removed)
    })?;
    if !removed {
        return Ok(RemoveOutcome::default());
    }
    // Recompute the merged view rather than guess: the other scope may still
    // carry a record for this pattern, and the caller has to say so instead of
    // promising an offer that stays silenced.
    let still_suppressed_by = active(root)
        .records
        .into_iter()
        .find(|s| s.record.pattern == pattern)
        .map(|s| s.scope);
    Ok(RemoveOutcome {
        removed: true,
        still_suppressed_by,
    })
}

/// The stored record for a pattern in one scope, whether or not it is still
/// active. A test-only read helper: every shipped surface reads through
/// `active` (the suppression set) or mutates through the locked-store path,
/// so exposing this as public API would leave dead weight behind.
#[cfg(test)]
fn stored_record(root: &Path, pattern: &str, scope: DecisionScope) -> Option<SuggestionRecord> {
    let dir = store_dir_quiet(root, scope).ok().flatten()?;
    read_store(&dir.join(STORE_FILE))
        .ok()
        .flatten()
        .and_then(|f| f.records.into_iter().find(|r| r.pattern == pattern))
}

/// Resolved timing knobs for one project: the named-constant defaults with the
/// global and then the project `suggestions:` section applied per key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SuggestionTiming {
    pub soft_backoff_days: Vec<u64>,
    pub hard_ttl_days: u64,
}

impl Default for SuggestionTiming {
    fn default() -> Self {
        Self {
            soft_backoff_days: SOFT_BACKOFF_DAYS.to_vec(),
            hard_ttl_days: HARD_TTL_DAYS,
        }
    }
}

/// Resolves the timing for `root`: defaults, then the global section, then the
/// project section, each key independently. An unreadable or invalid section
/// is reported as a diagnostic and its keys keep the previous value, so a
/// broken config never blocks a decline from being recorded.
pub fn timing(root: &Path) -> (SuggestionTiming, Vec<String>) {
    let mut out = SuggestionTiming::default();
    let mut diagnostics = Vec::new();
    let global = match crate::config::GlobalConfig::load() {
        Ok(cfg) => cfg.suggestions,
        Err(e) => {
            diagnostics.push(format!("suggestions config ignored: {e}"));
            crate::config::SuggestionSettings::default()
        }
    };
    let project = match crate::config::project_suggestion_settings(root) {
        Ok(s) => s,
        Err(e) => {
            diagnostics.push(format!("project suggestions config ignored: {e}"));
            crate::config::SuggestionSettings::default()
        }
    };
    for settings in [global, project] {
        match settings.validate() {
            Ok(()) => {
                if let Some(days) = settings.soft_backoff_days {
                    out.soft_backoff_days = days;
                }
                if let Some(ttl) = settings.hard_ttl_days {
                    out.hard_ttl_days = ttl;
                }
            }
            Err(e) => diagnostics.push(e),
        }
    }
    (out, diagnostics)
}

/// The snooze end for a decision. A soft decline takes the schedule entry for
/// its 1-based decline count, holding at the last entry (the schedule tail);
/// a hard dismissal takes the hard TTL.
pub fn next_snooze_ms(
    now_ms: u64,
    kind: DecisionKind,
    declines: u32,
    timing: &SuggestionTiming,
) -> u64 {
    let days = match kind {
        DecisionKind::Hard => timing.hard_ttl_days,
        DecisionKind::Soft => {
            let schedule = if timing.soft_backoff_days.is_empty() {
                SOFT_BACKOFF_DAYS.as_slice()
            } else {
                timing.soft_backoff_days.as_slice()
            };
            let idx = (declines.max(1) as usize - 1).min(schedule.len() - 1);
            schedule[idx]
        }
    };
    now_ms.saturating_add(days.saturating_mul(MS_PER_DAY))
}

/// What the caller decided about one suggestion.
#[derive(Debug, Clone)]
pub struct DecisionInput {
    pub pattern: String,
    pub synopsis: String,
    pub kind: DecisionKind,
    pub scope: DecisionScope,
    /// Hard-TTL override for one call, used only by the legacy
    /// `suggestion_dismiss` `ttl_days` argument so an old-style call keeps its
    /// old meaning. Ignored for a soft decline.
    pub hard_ttl_days_override: Option<u64>,
}

/// What one `record_decision` call produced: the stored record, plus every
/// diagnostic collected on the way (an invalid `suggestions:` config section,
/// a store that had to be moved aside as corrupt, a v1 migration that could
/// not finish). Diagnostics are part of the RESULT rather than a swallowed
/// side effect: `timing` is the only validator of the `suggestions:` config
/// section, and a caller that dropped its findings made an invalid section
/// silently ignored on every surface at once.
#[derive(Debug, Clone)]
pub struct DecisionOutcome {
    pub record: SuggestionRecord,
    pub diagnostics: Vec<String>,
}

/// Records a decision and returns the stored record, including the server-computed
/// snooze. A soft decline increments the decline counter of an existing record
/// in the same scope (that is the escalation); a hard dismissal replaces the
/// kind and uses the hard TTL. A non-empty synopsis always replaces the stored
/// one; an empty one keeps whatever was there.
///
/// The read of the previous record and the write of the new one happen inside
/// one `with_locked_store` critical section (one `lock_dir` acquisition), not
/// as a separate unlocked read followed by a separately-locked write:
/// otherwise two concurrent soft declines on the same pattern could both read
/// `declines = N` before either writes, and both would then write `N + 1`,
/// silently losing one escalation step.
///
/// Unlike `load_scope`, `remove_record` and `reset_records`, this function
/// does NOT guard against materializing a project-scope `.apb` directory that
/// did not exist before the call. That asymmetry is intentional, not an
/// oversight: a project-scope decision only reaches this function through the
/// MCP server, and the server only runs with its root set to the directory
/// the user actually connected apb to. Persisting the decision there is
/// exactly the adoption the user's connection already implied; the next `apb`
/// command in that directory auto-registers the workspace and finds `.apb`
/// already in place, same as if `apb init` had run first. The read path
/// (`active`) and the write-side "nothing to do" guards (`remove_record`,
/// `reset_records`) protect a DIFFERENT invariant: a call that has nothing to
/// report must not silently adopt a root it was never told to adopt. A call
/// that DOES have something to persist, on a root the server is actively
/// driving, is a different case and is allowed to create `.apb` on first
/// write.
pub fn record_decision(root: &Path, input: DecisionInput) -> std::io::Result<DecisionOutcome> {
    check_pattern(&input.pattern)?;
    let now = crate::clock::now_ms_u64();
    let (mut resolved, mut diagnostics) = timing(root);
    if let Some(ttl) = input.hard_ttl_days_override {
        resolved.hard_ttl_days = ttl;
    }
    let build = |previous: Option<&SuggestionRecord>| -> SuggestionRecord {
        // Hard always zeroes the counter, even when escalating from a
        // previously soft record: `declines` drives the soft backoff
        // position (spec: "absent (0) for hard records"), and a hard record
        // has nothing left to escalate. Carrying a stale count forward would
        // resurface as the wrong backoff step if the record were ever
        // re-declined soft after the hard TTL expires.
        let declines = match input.kind {
            DecisionKind::Soft => previous.map(|p| p.declines).unwrap_or(0) + 1,
            DecisionKind::Hard => 0,
        };
        let synopsis = if input.synopsis.trim().is_empty() {
            previous.map(|p| p.synopsis.clone()).unwrap_or_default()
        } else {
            input.synopsis.trim().to_string()
        };
        SuggestionRecord {
            pattern: input.pattern.clone(),
            synopsis,
            kind: input.kind,
            declines,
            snoozed_until_ms: next_snooze_ms(now, input.kind, declines, &resolved),
            updated_at_ms: now,
        }
    };

    // Global scope with no config dir at all has nowhere to persist the
    // record. This used to return the computed record with `Ok`, so the MCP
    // tool answered `dismissed`, `scope: global` and a `snoozed_until` for a
    // decision that left no state behind at all.
    let dir = mutation_dir(root, input.scope, &mut diagnostics)?;
    let record = with_locked_store(&dir, &mut diagnostics, |file| {
        let previous = file.records.iter().find(|r| r.pattern == input.pattern);
        let record = build(previous);
        file.records.retain(|r| r.pattern != record.pattern);
        file.records.push(record.clone());
        file.records.sort_by(|a, b| a.pattern.cmp(&b.pattern));
        (record, true)
    })?;
    Ok(DecisionOutcome {
        record,
        diagnostics,
    })
}

/// Outcome of a reset: which project-scope soft records were zeroed, and which
/// matched patterns were hard records and therefore left alone (a hard record
/// is removed with `remove_record`, never reset).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ResetOutcome {
    pub reset: Vec<String>,
    pub skipped_hard: Vec<String>,
}

/// Zeroes the decline counter and clears the snooze of project-scope soft
/// records, keeping the records themselves so their synopsis stays available.
/// `Some(pattern)` targets one record, `None` every soft record in the project
/// scope.
///
/// Checks for the store file before taking the dir lock, exactly like
/// `load_scope` and `remove_record`: an absent store has nothing to reset, and
/// `lock_dir` (inside `with_locked_store`) would otherwise `create_dir_all`
/// the project's `.apb` directory as a side effect, corrupting the adoption
/// marker a fresh root relies on. The TOCTOU this leaves is the same benign
/// one documented on `load_scope`.
pub fn reset_records(root: &Path, pattern: Option<&str>) -> std::io::Result<ResetOutcome> {
    let Some(dir) = store_dir_quiet(root, DecisionScope::Project)? else {
        return Ok(ResetOutcome::default());
    };
    if !dir.join(STORE_FILE).is_file() {
        return Ok(ResetOutcome::default());
    }
    let now = crate::clock::now_ms_u64();
    with_locked_store(&dir, &mut Vec::new(), |file| {
        let mut outcome = ResetOutcome::default();
        for rec in file.records.iter_mut() {
            if pattern.is_some_and(|p| p != rec.pattern) {
                continue;
            }
            match rec.kind {
                DecisionKind::Hard => outcome.skipped_hard.push(rec.pattern.clone()),
                DecisionKind::Soft => {
                    rec.declines = 0;
                    rec.snoozed_until_ms = 0;
                    rec.updated_at_ms = now;
                    outcome.reset.push(rec.pattern.clone());
                }
            }
        }
        let changed = !outcome.reset.is_empty();
        (outcome, changed)
    })
}

/// Renders epoch milliseconds as a UTC RFC-3339 timestamp, the shape the spec
/// shows for `snoozed_until`. apb has no date dependency, so the civil-date
/// conversion is done here and nowhere else.
pub fn iso_utc(ms: u64) -> String {
    let secs = (ms / 1000) as i64;
    let days = secs.div_euclid(86_400);
    let rem = secs.rem_euclid(86_400);
    let (y, m, d) = civil_from_days(days);
    let (hh, mm, ss) = (rem / 3600, (rem % 3600) / 60, rem % 60);
    format!("{y:04}-{m:02}-{d:02}T{hh:02}:{mm:02}:{ss:02}Z")
}

/// Days-since-epoch to (year, month, day), the standard proleptic-Gregorian
/// `civil_from_days` algorithm (Howard Hinnant, public domain).
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

/// Stricter-wins merge of the two scopes for one project: hard beats soft, and
/// within the same kind the later `snoozed_until_ms` wins. The surviving
/// record keeps the scope label it was read from, so a surface can tell the
/// user where the decision lives. Output is sorted by pattern.
fn merge_scopes(scoped: Vec<ScopedRecord>) -> Vec<ScopedRecord> {
    let mut merged: Vec<ScopedRecord> = Vec::new();
    for candidate in scoped {
        match merged
            .iter_mut()
            .find(|s| s.record.pattern == candidate.record.pattern)
        {
            Some(existing) => {
                if is_stricter(&candidate.record, &existing.record) {
                    *existing = candidate;
                }
            }
            None => merged.push(candidate),
        }
    }
    merged.sort_by(|a, b| a.record.pattern.cmp(&b.record.pattern));
    merged
}

/// Whether `a` is the stricter of two records for the same pattern.
fn is_stricter(a: &SuggestionRecord, b: &SuggestionRecord) -> bool {
    match (a.kind, b.kind) {
        (DecisionKind::Hard, DecisionKind::Soft) => true,
        (DecisionKind::Soft, DecisionKind::Hard) => false,
        _ => a.snoozed_until_ms > b.snoozed_until_ms,
    }
}

#[derive(Debug, Deserialize)]
struct LegacyRecord {
    #[serde(default)]
    created_ms: u64,
    #[serde(default)]
    ttl_days: u64,
}

#[derive(Debug, Default, Deserialize)]
struct LegacyFile {
    #[serde(default)]
    patterns: std::collections::BTreeMap<String, LegacyRecord>,
}

/// One-time v1 to v2 migration in the global scope: when `suggestions.json` is
/// absent and the v1 `dismissed.json` exists, every v1 entry becomes a HARD
/// record whose `snoozed_until_ms` is the v1 expiry (`created_ms + ttl`), with
/// an empty synopsis (the agent then falls back to slug comparison for it,
/// which is exactly v1 behavior). The old file is removed only after the new
/// one is atomically in place; a corrupt v1 file yields an empty store plus a
/// diagnostic and is left on disk untouched.
///
/// The migration is a store WRITE, so it runs under the same dir lock as every
/// other write and returns `Err` when that lock cannot be taken, rather than
/// racing a concurrent writer for the same file. Everything it can still do
/// something sensible about (an unreadable or malformed v1 file) stays a
/// diagnostic.
fn migrate_legacy(dir: &Path, now: u64, diagnostics: &mut Vec<String>) -> std::io::Result<()> {
    let new_path = dir.join(STORE_FILE);
    let legacy_path = dir.join(LEGACY_FILE);
    if new_path.exists() || !legacy_path.is_file() {
        return Ok(());
    }
    let _lock = lock_dir(dir, LOCK_NAME)?;
    if new_path.exists() {
        return Ok(());
    }
    let raw = match std::fs::read_to_string(&legacy_path) {
        Ok(r) => r,
        Err(e) => {
            diagnostics.push(format!(
                "unreadable legacy dismiss store `{}`: {e}",
                legacy_path.display()
            ));
            return Ok(());
        }
    };
    let legacy: LegacyFile = match serde_json::from_str(&raw) {
        Ok(l) => l,
        Err(e) => {
            diagnostics.push(format!(
                "malformed legacy dismiss store `{}`: {e}",
                legacy_path.display()
            ));
            return Ok(());
        }
    };
    let records: Vec<SuggestionRecord> = legacy
        .patterns
        .into_iter()
        .map(|(pattern, old)| SuggestionRecord {
            pattern,
            synopsis: String::new(),
            kind: DecisionKind::Hard,
            declines: 0,
            snoozed_until_ms: old
                .created_ms
                .saturating_add(old.ttl_days.saturating_mul(MS_PER_DAY)),
            updated_at_ms: now,
        })
        .collect();
    let file = StoreFile {
        schema: SCHEMA_VERSION,
        records,
    };
    if let Err(e) = write_store(dir, &file) {
        diagnostics.push(format!(
            "could not migrate `{}`: {e}",
            legacy_path.display()
        ));
        return Ok(());
    }
    if let Err(e) = std::fs::remove_file(&legacy_path) {
        diagnostics.push(format!(
            "migrated but could not remove `{}`: {e}",
            legacy_path.display()
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    struct EnvGuard;
    impl Drop for EnvGuard {
        fn drop(&mut self) {
            unsafe {
                std::env::remove_var("APB_CONFIG_DIR");
            }
        }
    }

    /// Unsets the variables `config_dir` consults and puts them back on drop
    /// (including on a panic), so a test of the config-less path cannot leave
    /// the process without a `HOME` for every test that runs after it.
    struct NoConfigDir(Vec<(&'static str, Option<String>)>);

    impl NoConfigDir {
        fn take() -> Self {
            let keys = ["APB_CONFIG_DIR", "XDG_CONFIG_HOME", "HOME"];
            let saved = keys
                .iter()
                .map(|k| (*k, std::env::var(k).ok()))
                .collect::<Vec<_>>();
            unsafe {
                for k in keys {
                    std::env::remove_var(k);
                }
            }
            Self(saved)
        }
    }

    impl Drop for NoConfigDir {
        fn drop(&mut self) {
            unsafe {
                for (k, v) in &self.0 {
                    match v {
                        Some(val) => std::env::set_var(k, val),
                        None => std::env::remove_var(k),
                    }
                }
            }
        }
    }

    fn rec(pattern: &str, kind: DecisionKind, snoozed_until_ms: u64) -> SuggestionRecord {
        SuggestionRecord {
            pattern: pattern.to_string(),
            synopsis: format!("synopsis for {pattern}"),
            kind,
            declines: 0,
            snoozed_until_ms,
            updated_at_ms: 1_753_785_000_000,
        }
    }

    #[test]
    fn project_record_roundtrips_through_active() {
        let _lock = crate::env_test_lock();
        let cfg = tempfile::tempdir().unwrap();
        let proj = tempfile::tempdir().unwrap();
        unsafe {
            std::env::set_var("APB_CONFIG_DIR", cfg.path());
        }
        let _g = EnvGuard;

        assert!(active(proj.path()).records.is_empty());
        let far_future = crate::clock::now_ms_u64() + 10 * MS_PER_DAY;
        write_record(
            proj.path(),
            DecisionScope::Project,
            rec("code-review-run", DecisionKind::Soft, far_future),
        )
        .unwrap();

        let view = active(proj.path());
        assert_eq!(view.records.len(), 1, "records: {:?}", view.records);
        assert_eq!(view.records[0].scope, DecisionScope::Project);
        assert_eq!(view.records[0].record.pattern, "code-review-run");
        assert_eq!(view.records[0].record.kind, DecisionKind::Soft);
        assert_eq!(
            view.records[0].record.synopsis,
            "synopsis for code-review-run"
        );
        assert!(view.diagnostics.is_empty());
        assert!(proj.path().join(".apb/suggestions.json").is_file());
    }

    #[test]
    fn reading_a_fresh_root_never_creates_the_apb_dir() {
        // `.apb` existence is an adoption marker elsewhere in apb (registry
        // auto-registration, "already initialized" checks), so a pure read
        // must never materialize it, even though `lock_dir` itself would
        // `create_dir_all` the scope directory if it were ever reached.
        let _lock = crate::env_test_lock();
        let cfg = tempfile::tempdir().unwrap();
        let proj = tempfile::tempdir().unwrap();
        unsafe {
            std::env::set_var("APB_CONFIG_DIR", cfg.path());
        }
        let _g = EnvGuard;

        assert!(!proj.path().join(".apb").exists());
        let view = active(proj.path());
        assert!(view.records.is_empty());
        assert!(view.diagnostics.is_empty());
        assert!(
            !proj.path().join(".apb").exists(),
            "a pure catalog read must not materialize .apb"
        );
    }

    #[test]
    fn expired_hard_record_is_pruned_on_read() {
        let _lock = crate::env_test_lock();
        let cfg = tempfile::tempdir().unwrap();
        let proj = tempfile::tempdir().unwrap();
        unsafe {
            std::env::set_var("APB_CONFIG_DIR", cfg.path());
        }
        let _g = EnvGuard;

        write_record(
            proj.path(),
            DecisionScope::Project,
            rec("old-thing", DecisionKind::Hard, 1),
        )
        .unwrap();
        assert!(active(proj.path()).records.is_empty());
        let raw = std::fs::read_to_string(proj.path().join(".apb/suggestions.json")).unwrap();
        assert!(
            !raw.contains("old-thing"),
            "an expired hard record must be pruned from the file: {raw}"
        );
    }

    #[test]
    fn iso_utc_renders_a_utc_timestamp() {
        assert_eq!(iso_utc(0), "1970-01-01T00:00:00Z");
        assert_eq!(iso_utc(1_753_785_000_000), "2025-07-29T10:30:00Z");
        assert_eq!(iso_utc(1_785_000_000_000), "2026-07-25T17:20:00Z");
    }

    #[test]
    fn stricter_scope_wins_for_the_same_pattern() {
        let _lock = crate::env_test_lock();
        let cfg = tempfile::tempdir().unwrap();
        let proj = tempfile::tempdir().unwrap();
        unsafe {
            std::env::set_var("APB_CONFIG_DIR", cfg.path());
        }
        let _g = EnvGuard;

        let now = crate::clock::now_ms_u64();
        // Project: a soft record with a near snooze. Global: hard, further out.
        write_record(
            proj.path(),
            DecisionScope::Project,
            rec("same-slug", DecisionKind::Soft, now + 2 * MS_PER_DAY),
        )
        .unwrap();
        write_record(
            proj.path(),
            DecisionScope::Global,
            rec("same-slug", DecisionKind::Hard, now + 90 * MS_PER_DAY),
        )
        .unwrap();

        let view = active(proj.path());
        assert_eq!(view.records.len(), 1, "records: {:?}", view.records);
        assert_eq!(view.records[0].scope, DecisionScope::Global);
        assert_eq!(view.records[0].record.kind, DecisionKind::Hard);

        // Same kind, project record has the later snooze: project wins.
        // `active()` iterates Project before Global, so this direction alone
        // would also pass under a "first written wins" bug; the next case
        // pins the opposite direction to rule that out.
        write_record(
            proj.path(),
            DecisionScope::Global,
            rec("both-soft", DecisionKind::Soft, now + 3 * MS_PER_DAY),
        )
        .unwrap();
        write_record(
            proj.path(),
            DecisionScope::Project,
            rec("both-soft", DecisionKind::Soft, now + 30 * MS_PER_DAY),
        )
        .unwrap();
        let view = active(proj.path());
        let both = view
            .records
            .iter()
            .find(|s| s.record.pattern == "both-soft")
            .expect("both-soft present");
        assert_eq!(both.scope, DecisionScope::Project);

        // Same kind, later-iterated Global record has the later snooze:
        // Global must win here too, which rules out a merge that always
        // keeps the first-seen (Project) record for equal kinds.
        write_record(
            proj.path(),
            DecisionScope::Project,
            rec("both-soft-2", DecisionKind::Soft, now + 3 * MS_PER_DAY),
        )
        .unwrap();
        write_record(
            proj.path(),
            DecisionScope::Global,
            rec("both-soft-2", DecisionKind::Soft, now + 30 * MS_PER_DAY),
        )
        .unwrap();
        let view = active(proj.path());
        let both_2 = view
            .records
            .iter()
            .find(|s| s.record.pattern == "both-soft-2")
            .expect("both-soft-2 present");
        assert_eq!(both_2.scope, DecisionScope::Global);
    }

    #[test]
    fn legacy_v1_store_migrates_to_hard_records_then_is_removed() {
        let _lock = crate::env_test_lock();
        let cfg = tempfile::tempdir().unwrap();
        let proj = tempfile::tempdir().unwrap();
        unsafe {
            std::env::set_var("APB_CONFIG_DIR", cfg.path());
        }
        let _g = EnvGuard;

        let now = crate::clock::now_ms_u64();
        let legacy = format!(
            "{{\"schema_version\":1,\"patterns\":{{\"daily-note-creation\":{{\"created_ms\":{now},\"ttl_days\":90}}}}}}"
        );
        std::fs::write(cfg.path().join("dismissed.json"), legacy).unwrap();

        let view = active(proj.path());
        assert_eq!(view.records.len(), 1, "records: {:?}", view.records);
        let migrated = &view.records[0];
        assert_eq!(migrated.scope, DecisionScope::Global);
        assert_eq!(migrated.record.pattern, "daily-note-creation");
        assert_eq!(migrated.record.kind, DecisionKind::Hard);
        assert_eq!(migrated.record.synopsis, "");
        assert_eq!(
            migrated.record.snoozed_until_ms,
            now + 90 * MS_PER_DAY,
            "the v1 expiry is preserved as the snooze"
        );
        assert!(cfg.path().join("suggestions.json").is_file());
        assert!(
            !cfg.path().join("dismissed.json").exists(),
            "the v1 file is removed once the new one is in place"
        );
    }

    #[test]
    fn corrupt_stores_yield_an_empty_list_with_diagnostics() {
        let _lock = crate::env_test_lock();
        let cfg = tempfile::tempdir().unwrap();
        let proj = tempfile::tempdir().unwrap();
        unsafe {
            std::env::set_var("APB_CONFIG_DIR", cfg.path());
        }
        let _g = EnvGuard;

        std::fs::create_dir_all(proj.path().join(".apb")).unwrap();
        std::fs::write(proj.path().join(".apb/suggestions.json"), "{ not json").unwrap();
        std::fs::write(cfg.path().join("dismissed.json"), "{ not json either").unwrap();

        let view = active(proj.path());
        assert!(view.records.is_empty());
        assert_eq!(view.diagnostics.len(), 2, "{:?}", view.diagnostics);
        assert!(
            view.diagnostics
                .iter()
                .any(|d| d.contains("malformed suggestion store"))
        );
        assert!(
            view.diagnostics
                .iter()
                .any(|d| d.contains("malformed legacy dismiss store")),
        );
        assert!(
            cfg.path().join("dismissed.json").exists(),
            "a corrupt v1 file is left alone, never deleted"
        );
    }

    /// The v1 to v2 migration must not depend on a read happening first.
    /// `migrate_legacy` gives up as soon as `suggestions.json` exists, so when
    /// the first global-scope operation in a fresh config dir is a WRITE, a
    /// migration hung off the read path alone would create the new file and
    /// strand `dismissed.json` on disk forever, silently losing every v1
    /// decision the user had made.
    #[test]
    fn a_global_write_before_any_read_still_migrates_the_v1_store() {
        let _lock = crate::env_test_lock();
        let cfg = tempfile::tempdir().unwrap();
        let proj = tempfile::tempdir().unwrap();
        unsafe {
            std::env::set_var("APB_CONFIG_DIR", cfg.path());
        }
        let _g = EnvGuard;

        let now = crate::clock::now_ms_u64();
        let legacy = format!(
            "{{\"schema_version\":1,\"patterns\":{{\"legacy-pattern\":{{\"created_ms\":{now},\"ttl_days\":90}}}}}}"
        );
        std::fs::write(cfg.path().join("dismissed.json"), legacy).unwrap();

        // The very first global-scope call is a write, with no read before it.
        let outcome = record_decision(
            proj.path(),
            DecisionInput {
                pattern: "brand-new-pattern".to_string(),
                synopsis: "A decision recorded before anything read the store".to_string(),
                kind: DecisionKind::Hard,
                scope: DecisionScope::Global,
                hard_ttl_days_override: None,
            },
        )
        .unwrap();
        assert_eq!(outcome.record.pattern, "brand-new-pattern");

        assert!(
            !cfg.path().join("dismissed.json").exists(),
            "the v1 file must be gone once its records are migrated"
        );
        let view = active(proj.path());
        let patterns: Vec<&str> = view
            .records
            .iter()
            .map(|s| s.record.pattern.as_str())
            .collect();
        assert!(
            patterns.contains(&"legacy-pattern"),
            "the v1 record must survive the write that ran before any read: {patterns:?}"
        );
        assert!(patterns.contains(&"brand-new-pattern"), "{patterns:?}");
    }

    /// A store that is present but unparseable used to collapse to an empty
    /// store, and the next write then overwrote the only copy of the user's
    /// decisions. The bytes must be moved aside first.
    #[test]
    fn a_corrupt_store_is_kept_aside_instead_of_being_overwritten() {
        let _lock = crate::env_test_lock();
        let cfg = tempfile::tempdir().unwrap();
        let proj = tempfile::tempdir().unwrap();
        unsafe {
            std::env::set_var("APB_CONFIG_DIR", cfg.path());
        }
        let _g = EnvGuard;

        const ORIGINAL: &str = "{ \"schema\": 2, \"records\": [ truncated mid-write";
        std::fs::create_dir_all(proj.path().join(".apb")).unwrap();
        std::fs::write(proj.path().join(".apb/suggestions.json"), ORIGINAL).unwrap();

        let now = crate::clock::now_ms_u64();
        write_record(
            proj.path(),
            DecisionScope::Project,
            rec("after-corruption", DecisionKind::Hard, now + MS_PER_DAY),
        )
        .unwrap();

        let quarantined = proj.path().join(".apb/suggestions.json.corrupt");
        assert!(
            quarantined.is_file(),
            "the unreadable store must be kept aside, not destroyed"
        );
        assert_eq!(
            std::fs::read_to_string(&quarantined).unwrap(),
            ORIGINAL,
            "the quarantined copy must be the original bytes, byte for byte"
        );
        let raw = std::fs::read_to_string(proj.path().join(".apb/suggestions.json")).unwrap();
        let parsed: StoreFile = serde_json::from_str(&raw).expect("the new store parses");
        assert_eq!(parsed.records.len(), 1, "{raw}");
        assert_eq!(parsed.records[0].pattern, "after-corruption");
    }

    /// A pattern is a record id that later has to survive a CLI argument and a
    /// URL segment (`DELETE /api/suggestions/{pattern}`, which rejects
    /// anything `is_safe_id` would). Writing one that those surfaces cannot
    /// address produced a record that could never be removed again, so the
    /// rule is enforced at write time, on every writer.
    #[test]
    fn an_unaddressable_pattern_is_rejected_by_every_writer() {
        let _lock = crate::env_test_lock();
        let cfg = tempfile::tempdir().unwrap();
        let proj = tempfile::tempdir().unwrap();
        unsafe {
            std::env::set_var("APB_CONFIG_DIR", cfg.path());
        }
        let _g = EnvGuard;

        for good in ["code-review-run", "a", "x9", "a-b-c-1"] {
            assert!(validate_pattern(good).is_ok(), "`{good}` must be accepted");
        }
        for bad in [
            "",
            "..",
            ".",
            "a/b",
            "a\\b",
            "a.b",
            "Code-Review",
            "trailing space",
            "-leading-hyphen",
            "_under",
        ] {
            assert!(
                validate_pattern(bad).is_err(),
                "`{bad}` must be rejected as a pattern"
            );
        }

        let bad_decision = record_decision(
            proj.path(),
            DecisionInput {
                pattern: "../escape".to_string(),
                synopsis: "Should never be stored".to_string(),
                kind: DecisionKind::Soft,
                scope: DecisionScope::Project,
                hard_ttl_days_override: None,
            },
        );
        assert!(
            bad_decision.is_err(),
            "record_decision must reject an unaddressable pattern"
        );
        let bad_write = write_record(
            proj.path(),
            DecisionScope::Project,
            rec("a/b", DecisionKind::Hard, 1),
        );
        assert!(
            bad_write.is_err(),
            "write_record must reject an unaddressable pattern too"
        );
        assert!(
            !proj.path().join(".apb/suggestions.json").exists(),
            "a rejected pattern must not create a store"
        );
    }

    /// `timing` is the only validator of the `suggestions:` config section,
    /// so a `record_decision` that dropped its diagnostics made an invalid
    /// section silently ignored on every surface at once.
    #[test]
    fn record_decision_hands_back_the_config_diagnostics() {
        let _lock = crate::env_test_lock();
        let cfg = tempfile::tempdir().unwrap();
        let proj = tempfile::tempdir().unwrap();
        unsafe {
            std::env::set_var("APB_CONFIG_DIR", cfg.path());
        }
        let _g = EnvGuard;

        std::fs::write(
            cfg.path().join("config.yaml"),
            "suggestions:\n  hard_ttl_days: 0\n",
        )
        .unwrap();

        let outcome = record_decision(
            proj.path(),
            DecisionInput {
                pattern: "still-recorded".to_string(),
                synopsis: "The decision is recorded even so".to_string(),
                kind: DecisionKind::Hard,
                scope: DecisionScope::Project,
                hard_ttl_days_override: None,
            },
        )
        .unwrap();
        assert_eq!(outcome.record.pattern, "still-recorded");
        assert!(
            outcome
                .diagnostics
                .iter()
                .any(|d| d.contains("hard_ttl_days")),
            "the invalid section must reach the caller: {:?}",
            outcome.diagnostics
        );
    }

    #[test]
    fn removing_a_record_re_enables_the_suggestion() {
        let _lock = crate::env_test_lock();
        let cfg = tempfile::tempdir().unwrap();
        let proj = tempfile::tempdir().unwrap();
        unsafe {
            std::env::set_var("APB_CONFIG_DIR", cfg.path());
        }
        let _g = EnvGuard;

        let now = crate::clock::now_ms_u64();
        write_record(
            proj.path(),
            DecisionScope::Project,
            rec("gone-soon", DecisionKind::Hard, now + MS_PER_DAY),
        )
        .unwrap();
        assert!(stored_record(proj.path(), "gone-soon", DecisionScope::Project).is_some());
        let outcome = remove_record(proj.path(), "gone-soon", DecisionScope::Project).unwrap();
        assert_eq!(
            outcome,
            RemoveOutcome {
                removed: true,
                still_suppressed_by: None
            },
            "the only record for the pattern is gone, so nothing suppresses it now"
        );
        assert_eq!(
            remove_record(proj.path(), "gone-soon", DecisionScope::Project).unwrap(),
            RemoveOutcome::default()
        );
        assert!(active(proj.path()).records.is_empty());
    }

    #[test]
    fn soft_backoff_walks_the_schedule_and_holds_at_the_tail() {
        let t = SuggestionTiming::default();
        assert_eq!(t.soft_backoff_days, vec![1, 7, 30, 90]);
        assert_eq!(t.hard_ttl_days, 90);
        let now = 1_753_785_000_000u64;
        for (declines, days) in [(1u32, 1u64), (2, 7), (3, 30), (4, 90), (5, 90), (99, 90)] {
            assert_eq!(
                next_snooze_ms(now, DecisionKind::Soft, declines, &t),
                now + days * MS_PER_DAY,
                "decline {declines} should snooze {days} days"
            );
        }
        assert_eq!(
            next_snooze_ms(now, DecisionKind::Hard, 0, &t),
            now + 90 * MS_PER_DAY
        );
    }

    /// Pins that `record_decision` with project scope is intentionally
    /// allowed to create `<root>/.apb` on a bare tempdir that never went
    /// through `init_project`, unlike the read path (`active`) and the
    /// write-side "nothing to do" guards
    /// (`remove_record`, `reset_records`), which must NOT do that (see
    /// `reading_a_fresh_root_never_creates_the_apb_dir` and
    /// `write_path_guards_never_materialize_apb_on_a_fresh_root`). A
    /// project-scope decision only reaches `record_decision` through the MCP
    /// server, whose root is the directory the user actually connected apb
    /// to, so persisting the decision there is exactly the adoption that
    /// connection already implied: the next `apb` command in that directory
    /// auto-registers the workspace and finds `.apb` already in place. This
    /// is not a regression to guard against, it is the documented contract.
    #[test]
    fn record_decision_intentionally_adopts_the_root_on_first_project_decision() {
        let _lock = crate::env_test_lock();
        let cfg = tempfile::tempdir().unwrap();
        let proj = tempfile::tempdir().unwrap();
        unsafe {
            std::env::set_var("APB_CONFIG_DIR", cfg.path());
        }
        let _g = EnvGuard;

        assert!(
            !proj.path().join(".apb").exists(),
            "the tempdir starts bare, never initialized"
        );
        let input = || DecisionInput {
            pattern: "code-review-run".to_string(),
            synopsis: "Review a source file and write findings to a report".to_string(),
            kind: DecisionKind::Soft,
            scope: DecisionScope::Project,
            hard_ttl_days_override: None,
        };
        let first = record_decision(proj.path(), input()).unwrap().record;
        assert_eq!(first.declines, 1);
        assert!(
            proj.path().join(".apb/suggestions.json").is_file(),
            "a project-scope decision intentionally adopts the root by creating .apb"
        );
        let second = record_decision(proj.path(), input()).unwrap().record;
        assert_eq!(second.declines, 2);
        assert!(
            second.snoozed_until_ms > first.snoozed_until_ms,
            "the second decline must push the snooze further out"
        );
        assert_eq!(second.kind, DecisionKind::Soft);
        assert_eq!(
            second.synopsis,
            "Review a source file and write findings to a report"
        );

        // A hard decision on the same pattern overwrites the kind and uses the
        // hard TTL, keeping the synopsis.
        let hard = record_decision(
            proj.path(),
            DecisionInput {
                kind: DecisionKind::Hard,
                ..input()
            },
        )
        .unwrap()
        .record;
        assert_eq!(hard.kind, DecisionKind::Hard);
        assert!(hard.snoozed_until_ms >= crate::clock::now_ms_u64() + 89 * MS_PER_DAY);
    }

    /// Pins the fold-under-lock discipline `record_decision` must follow: the
    /// read of the previous record and the write of the escalated one happen
    /// inside a single `with_locked_store` critical section, not as an
    /// unlocked read (e.g. via `stored_record`) followed by a separately
    /// locked write. Run sequentially, two soft declines on the same pattern
    /// must land on declines=2 and the schedule's second entry (7 days), the
    /// same result a single caller making both calls back-to-back would see
    /// under true mutual exclusion; a regression to a read-then-write race
    /// would still pass this in a single thread, but the fold must go through
    /// `with_locked_store` so a concurrent second caller cannot observe the
    /// same "previous" state the first caller already read.
    #[test]
    fn record_decision_folds_previous_state_under_one_lock() {
        let _lock = crate::env_test_lock();
        let cfg = tempfile::tempdir().unwrap();
        let proj = tempfile::tempdir().unwrap();
        unsafe {
            std::env::set_var("APB_CONFIG_DIR", cfg.path());
        }
        let _g = EnvGuard;

        let input = || DecisionInput {
            pattern: "locked-fold".to_string(),
            synopsis: "Fold under one lock".to_string(),
            kind: DecisionKind::Soft,
            scope: DecisionScope::Project,
            hard_ttl_days_override: None,
        };
        let first = record_decision(proj.path(), input()).unwrap().record;
        assert_eq!(first.declines, 1);
        assert_eq!(
            first.snoozed_until_ms - first.updated_at_ms,
            MS_PER_DAY,
            "the first soft decline snoozes 1 day"
        );

        let second = record_decision(proj.path(), input()).unwrap().record;
        assert_eq!(
            second.declines, 2,
            "the second call must see the first call's write, not a stale read"
        );
        assert_eq!(
            second.snoozed_until_ms - second.updated_at_ms,
            7 * MS_PER_DAY,
            "the second soft decline snoozes 7 days"
        );

        // The store itself agrees: only one record, at declines=2.
        let stored = stored_record(proj.path(), "locked-fold", DecisionScope::Project)
            .expect("the record was persisted");
        assert_eq!(stored.declines, 2);
    }

    /// `declines` drives the soft backoff position and is meaningless once a
    /// record turns hard. A hard decline on a pattern that already carries a
    /// soft record with a nonzero `declines` must zero it, not carry the
    /// stale count forward.
    #[test]
    fn a_hard_decline_zeroes_a_carried_over_soft_declines_count() {
        let _lock = crate::env_test_lock();
        let cfg = tempfile::tempdir().unwrap();
        let proj = tempfile::tempdir().unwrap();
        unsafe {
            std::env::set_var("APB_CONFIG_DIR", cfg.path());
        }
        let _g = EnvGuard;

        let soft_input = || DecisionInput {
            pattern: "escalated".to_string(),
            synopsis: "Escalating from soft to hard".to_string(),
            kind: DecisionKind::Soft,
            scope: DecisionScope::Project,
            hard_ttl_days_override: None,
        };
        record_decision(proj.path(), soft_input()).unwrap();
        let after_second_soft = record_decision(proj.path(), soft_input()).unwrap().record;
        assert_eq!(after_second_soft.declines, 2);

        let hard = record_decision(
            proj.path(),
            DecisionInput {
                pattern: "escalated".to_string(),
                synopsis: "Now never again".to_string(),
                kind: DecisionKind::Hard,
                scope: DecisionScope::Project,
                hard_ttl_days_override: None,
            },
        )
        .unwrap()
        .record;
        assert_eq!(
            hard.declines, 0,
            "a hard record must not carry the soft counter forward"
        );
    }

    #[test]
    fn project_config_overrides_global_per_key() {
        let _lock = crate::env_test_lock();
        let cfg = tempfile::tempdir().unwrap();
        let proj = tempfile::tempdir().unwrap();
        unsafe {
            std::env::set_var("APB_CONFIG_DIR", cfg.path());
        }
        let _g = EnvGuard;

        std::fs::write(
            cfg.path().join("config.yaml"),
            "suggestions:\n  soft_backoff_days: [2, 4]\n  hard_ttl_days: 30\n",
        )
        .unwrap();
        std::fs::create_dir_all(proj.path().join(".apb")).unwrap();
        std::fs::write(
            proj.path().join(".apb/config.yaml"),
            "suggestions:\n  hard_ttl_days: 3\n",
        )
        .unwrap();

        let (t, diags) = timing(proj.path());
        assert!(diags.is_empty(), "{diags:?}");
        assert_eq!(t.soft_backoff_days, vec![2, 4], "global key survives");
        assert_eq!(t.hard_ttl_days, 3, "project key wins");
    }

    #[test]
    fn invalid_config_falls_back_to_defaults_with_a_diagnostic() {
        let _lock = crate::env_test_lock();
        let cfg = tempfile::tempdir().unwrap();
        let proj = tempfile::tempdir().unwrap();
        unsafe {
            std::env::set_var("APB_CONFIG_DIR", cfg.path());
        }
        let _g = EnvGuard;

        std::fs::write(
            cfg.path().join("config.yaml"),
            "suggestions:\n  soft_backoff_days: []\n",
        )
        .unwrap();
        let (t, diags) = timing(proj.path());
        assert_eq!(t.soft_backoff_days, vec![1, 7, 30, 90]);
        assert_eq!(diags.len(), 1, "{diags:?}");
        assert!(diags[0].contains("soft_backoff_days"));
    }

    #[test]
    fn reset_zeroes_soft_records_and_leaves_hard_ones() {
        let _lock = crate::env_test_lock();
        let cfg = tempfile::tempdir().unwrap();
        let proj = tempfile::tempdir().unwrap();
        unsafe {
            std::env::set_var("APB_CONFIG_DIR", cfg.path());
        }
        let _g = EnvGuard;

        record_decision(
            proj.path(),
            DecisionInput {
                pattern: "soft-one".to_string(),
                synopsis: "A soft one".to_string(),
                kind: DecisionKind::Soft,
                scope: DecisionScope::Project,
                hard_ttl_days_override: None,
            },
        )
        .unwrap();
        record_decision(
            proj.path(),
            DecisionInput {
                pattern: "hard-one".to_string(),
                synopsis: "A hard one".to_string(),
                kind: DecisionKind::Hard,
                scope: DecisionScope::Project,
                hard_ttl_days_override: None,
            },
        )
        .unwrap();

        let outcome = reset_records(proj.path(), Some("hard-one")).unwrap();
        assert!(outcome.reset.is_empty());
        assert_eq!(outcome.skipped_hard, vec!["hard-one".to_string()]);

        let outcome = reset_records(proj.path(), None).unwrap();
        assert_eq!(outcome.reset, vec!["soft-one".to_string()]);
        assert_eq!(outcome.skipped_hard, vec!["hard-one".to_string()]);

        let kept = stored_record(proj.path(), "soft-one", DecisionScope::Project).unwrap();
        assert_eq!(kept.declines, 0);
        assert_eq!(kept.snoozed_until_ms, 0, "the snooze is cleared");
        assert_eq!(kept.synopsis, "A soft one", "the synopsis stays available");
        assert!(
            active(proj.path())
                .records
                .iter()
                .all(|s| s.record.pattern != "soft-one"),
            "a reset record no longer suppresses the suggestion"
        );
        // The read above prunes; the reset record must survive it, otherwise
        // its synopsis would be gone and the next decline would start over.
        let after_read = stored_record(proj.path(), "soft-one", DecisionScope::Project)
            .expect("a reset soft record survives prune-on-read");
        assert_eq!(after_read.synopsis, "A soft one");
    }

    /// `remove_record` and `reset_records` must never materialize `.apb` on a
    /// fresh root: `.apb` existence is an adoption marker elsewhere in apb
    /// (registry auto-registration, "already initialized" checks), and both
    /// functions route their write through `with_locked_store` ->
    /// `lock_dir`, which `create_dir_all`s the scope directory as a side
    /// effect once reached. A "nothing to remove"/"nothing to reset" outcome
    /// on an absent store must guard before that lock, exactly like
    /// `load_scope` does for reads.
    #[test]
    fn write_path_guards_never_materialize_apb_on_a_fresh_root() {
        let _lock = crate::env_test_lock();
        let cfg = tempfile::tempdir().unwrap();
        let proj = tempfile::tempdir().unwrap();
        unsafe {
            std::env::set_var("APB_CONFIG_DIR", cfg.path());
        }
        let _g = EnvGuard;

        assert!(!proj.path().join(".apb").exists());

        assert_eq!(
            remove_record(proj.path(), "nope", DecisionScope::Project).unwrap(),
            RemoveOutcome::default()
        );
        assert!(
            !proj.path().join(".apb").exists(),
            "remove_record must not materialize .apb on a fresh root"
        );

        let outcome = reset_records(proj.path(), Some("nope")).unwrap();
        assert_eq!(outcome, ResetOutcome::default());
        assert!(
            !proj.path().join(".apb").exists(),
            "reset_records must not materialize .apb on a fresh root"
        );

        let outcome = reset_records(proj.path(), None).unwrap();
        assert_eq!(outcome, ResetOutcome::default());
        assert!(
            !proj.path().join(".apb").exists(),
            "reset_records with no pattern (--all) must not materialize .apb either"
        );
    }

    /// `lock_dir(...).ok()` turned a failed acquisition into a write with no
    /// mutual exclusion at all: two concurrent soft declines could both read
    /// `declines = N` and both write `N + 1`, and a prune could clobber a
    /// newer record. With a fresh lock held by someone else (`lock_dir` never
    /// steals a non-stale lock), a mutation must FAIL and write nothing, while
    /// the never-fail read must still hand back the records without
    /// prune-writing behind the lock's back.
    #[test]
    fn a_busy_lock_refuses_the_mutation_instead_of_writing_without_it() {
        let _lock = crate::env_test_lock();
        let cfg = tempfile::tempdir().unwrap();
        let proj = tempfile::tempdir().unwrap();
        unsafe {
            std::env::set_var("APB_CONFIG_DIR", cfg.path());
        }
        let _g = EnvGuard;

        let input = || DecisionInput {
            pattern: "busy-lock".to_string(),
            synopsis: "A decline attempted while the store is locked".to_string(),
            kind: DecisionKind::Soft,
            scope: DecisionScope::Project,
            hard_ttl_days_override: None,
        };
        let first = record_decision(proj.path(), input()).unwrap().record;
        assert_eq!(first.declines, 1);
        // An already-expired hard record, so a read has something to prune and
        // the "no prune-write without the lock" half of this test has teeth.
        write_record(
            proj.path(),
            DecisionScope::Project,
            rec("expired-hard", DecisionKind::Hard, 1),
        )
        .unwrap();

        let dir = proj.path().join(".apb");
        let store = dir.join(STORE_FILE);
        let before = std::fs::read_to_string(&store).unwrap();

        // A fresh lock held by another owner. `lock_dir` waits, refuses to
        // steal a non-stale lock, and returns WouldBlock.
        let held = crate::fsutil::lock_dir(&dir, LOCK_NAME).expect("the test takes the lock first");

        let busy = record_decision(proj.path(), input());
        assert!(
            busy.is_err(),
            "a mutation that could not take the lock must fail, not write anyway"
        );
        assert_eq!(
            std::fs::read_to_string(&store).unwrap(),
            before,
            "a refused mutation must not touch the store"
        );

        // The read path never fails, but it must not prune-WRITE either.
        let view = active(proj.path());
        assert!(
            view.records.iter().any(|s| s.record.pattern == "busy-lock"),
            "the never-fail read still reports the records: {:?}",
            view.records
        );
        assert!(
            !view.diagnostics.is_empty(),
            "a read that could not take the lock says so"
        );
        assert_eq!(
            std::fs::read_to_string(&store).unwrap(),
            before,
            "the read path must not prune-write without the lock"
        );

        // Once the lock is free the refused update can be retried, and the
        // escalation the busy call rejected was not lost along the way.
        drop(held);
        let second = record_decision(proj.path(), input()).unwrap().record;
        assert_eq!(
            second.declines, 2,
            "the refused decline must be retryable from the state that survived"
        );
    }

    /// A store written by a FUTURE apb parses fine here (no
    /// `deny_unknown_fields`), and every mutation used to stamp
    /// `schema = SCHEMA_VERSION` on the way out, silently dropping the fields
    /// this build does not know. An unsupported schema must therefore be
    /// read-only: a diagnostic on the read path, a refusal on the write path,
    /// and never a quarantine (the file is valid, just newer).
    #[test]
    fn an_unknown_schema_is_left_untouched_by_reads_and_refused_by_mutations() {
        let _lock = crate::env_test_lock();
        let cfg = tempfile::tempdir().unwrap();
        let proj = tempfile::tempdir().unwrap();
        unsafe {
            std::env::set_var("APB_CONFIG_DIR", cfg.path());
        }
        let _g = EnvGuard;

        const FUTURE: &str = r#"{
  "schema": 3,
  "policy": "a field this build has never heard of",
  "records": [
    {
      "pattern": "from-the-future",
      "synopsis": "Written by a newer apb",
      "kind": "hard",
      "declines": 0,
      "snoozed_until_ms": 99999999999999,
      "updated_at_ms": 1753785000000,
      "muted_channels": ["dashboard"]
    }
  ]
}"#;
        std::fs::create_dir_all(proj.path().join(".apb")).unwrap();
        let store = proj.path().join(".apb/suggestions.json");
        std::fs::write(&store, FUTURE).unwrap();

        let view = active(proj.path());
        assert!(
            view.records.is_empty(),
            "an unsupported schema yields no records: {:?}",
            view.records
        );
        assert!(
            view.diagnostics.iter().any(|d| d.contains("schema")),
            "the unsupported schema must be reported: {:?}",
            view.diagnostics
        );
        assert_eq!(
            std::fs::read_to_string(&store).unwrap(),
            FUTURE,
            "a read must leave a newer store byte-for-byte alone"
        );

        for attempt in [
            record_decision(
                proj.path(),
                DecisionInput {
                    pattern: "brand-new".to_string(),
                    synopsis: "Recorded against a newer store".to_string(),
                    kind: DecisionKind::Soft,
                    scope: DecisionScope::Project,
                    hard_ttl_days_override: None,
                },
            )
            .map(|_| ()),
            remove_record(proj.path(), "from-the-future", DecisionScope::Project).map(|_| ()),
            reset_records(proj.path(), None).map(|_| ()),
            write_record(
                proj.path(),
                DecisionScope::Project,
                rec("another", DecisionKind::Soft, 1),
            ),
        ] {
            let e = attempt.expect_err("a mutation against an unsupported schema must refuse");
            assert!(
                e.to_string().contains("schema"),
                "the refusal must name the schema: {e}"
            );
        }
        assert_eq!(
            std::fs::read_to_string(&store).unwrap(),
            FUTURE,
            "a refused mutation must leave the bytes untouched"
        );
        assert!(
            !proj.path().join(".apb/suggestions.json.corrupt").exists(),
            "a valid-but-newer store is not corrupt and must never be quarantined"
        );
    }

    /// With no `APB_CONFIG_DIR`, no `XDG_CONFIG_HOME` and no `HOME`, the global
    /// store has nowhere to live. Returning `Ok` there made the MCP tool answer
    /// `dismissed`, `scope: global` and a `snoozed_until` for a decision that
    /// was never persisted. A global mutation must fail the way a global
    /// `playbook_capture` already does.
    #[test]
    fn a_global_decision_without_a_config_dir_is_an_error_not_a_false_success() {
        let _lock = crate::env_test_lock();
        let proj = tempfile::tempdir().unwrap();
        let _no_cfg = NoConfigDir::take();
        assert!(
            crate::config::config_dir().is_none(),
            "no config dir at all"
        );

        let global = || DecisionInput {
            pattern: "nowhere-to-store".to_string(),
            synopsis: "A global decision with no global store".to_string(),
            kind: DecisionKind::Hard,
            scope: DecisionScope::Global,
            hard_ttl_days_override: None,
        };
        let err = record_decision(proj.path(), global())
            .expect_err("a global decision with nowhere to go must not report success");
        assert!(
            err.to_string().contains("no global config dir"),
            "the error must say what is missing: {err}"
        );
        let err = write_record(
            proj.path(),
            DecisionScope::Global,
            rec("nowhere-to-store", DecisionKind::Hard, u64::MAX),
        )
        .expect_err("write_record must refuse a global write with nowhere to go");
        assert!(err.to_string().contains("no global config dir"), "{err}");

        // The config-less path itself stays functional: a project decision is
        // recorded, and reads still work.
        record_decision(
            proj.path(),
            DecisionInput {
                scope: DecisionScope::Project,
                ..global()
            },
        )
        .expect("a project decision needs no global config dir");
        assert_eq!(active(proj.path()).records.len(), 1);
    }

    /// Removing the project record does not re-enable an offer that the GLOBAL
    /// record still silences, and vice versa. Every surface used to promise
    /// "the suggestion can be offered again" from the removal alone, which the
    /// dashboard exposed most plainly: the card came straight back on the next
    /// load, right next to the success toast.
    #[test]
    fn removing_one_scope_reports_that_the_other_scope_still_suppresses() {
        let _lock = crate::env_test_lock();
        let cfg = tempfile::tempdir().unwrap();
        let proj = tempfile::tempdir().unwrap();
        unsafe {
            std::env::set_var("APB_CONFIG_DIR", cfg.path());
        }
        let _g = EnvGuard;

        let now = crate::clock::now_ms_u64();
        for scope in [DecisionScope::Project, DecisionScope::Global] {
            write_record(
                proj.path(),
                scope,
                rec("both-scopes", DecisionKind::Hard, now + 30 * MS_PER_DAY),
            )
            .unwrap();
        }

        let first = remove_record(proj.path(), "both-scopes", DecisionScope::Project).unwrap();
        assert!(first.removed);
        assert_eq!(
            first.still_suppressed_by,
            Some(DecisionScope::Global),
            "the global record still silences the offer"
        );
        assert_eq!(
            active(proj.path()).records.len(),
            1,
            "and the merged view agrees"
        );

        let second = remove_record(proj.path(), "both-scopes", DecisionScope::Global).unwrap();
        assert!(second.removed);
        assert_eq!(
            second.still_suppressed_by, None,
            "with both records gone the offer really is back"
        );
        assert!(active(proj.path()).records.is_empty());
    }

    /// The dashboard on a machine with zero registered projects has no
    /// workspace id to resolve a root from, so the machine-wide records were
    /// unreachable: they could be neither seen nor removed. `active_global`
    /// reads them with no project at all.
    #[test]
    fn active_global_reads_the_machine_wide_records_without_a_project() {
        let _lock = crate::env_test_lock();
        let cfg = tempfile::tempdir().unwrap();
        let proj = tempfile::tempdir().unwrap();
        unsafe {
            std::env::set_var("APB_CONFIG_DIR", cfg.path());
        }
        let _g = EnvGuard;

        let now = crate::clock::now_ms_u64();
        write_record(
            proj.path(),
            DecisionScope::Global,
            rec("machine-wide", DecisionKind::Hard, now + 30 * MS_PER_DAY),
        )
        .unwrap();
        write_record(
            proj.path(),
            DecisionScope::Project,
            rec("project-only", DecisionKind::Soft, now + 30 * MS_PER_DAY),
        )
        .unwrap();

        let view = active_global();
        assert_eq!(view.records.len(), 1, "{:?}", view.records);
        assert_eq!(view.records[0].scope, DecisionScope::Global);
        assert_eq!(view.records[0].record.pattern, "machine-wide");
        assert!(view.diagnostics.is_empty(), "{:?}", view.diagnostics);
    }
}
