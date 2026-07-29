# Suggestion Decisions Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace the rigid v1 dismiss store with a two-scope suggestion-decision store (schema 2) that records soft and hard declines with a synopsis, escalates soft declines on a configurable backoff, exposes the active records to the agent for semantic matching, and gives the user CLI and dashboard control over them, exactly as specified in `docs/superpowers/specs/2026-07-29-suggestion-decisions-design.md`.

**Architecture:** `apb-core::dismiss` becomes the single store: two files (`<root>/.apb/suggestions.json` for project scope, `<config-dir>/suggestions.json` for global scope) merged per read with a stricter-wins conflict rule, pruned on read, written atomically under a dir lock, and migrated once from the v1 `<config-dir>/dismissed.json`. Timing defaults are named constants in that module, overridable per key by an optional `suggestions:` section in the global `<config-dir>/config.yaml` and in the project `<root>/.apb/config.yaml` (project over global). `apb-mcp` grows three backward-compatible `suggestion_dismiss` args and a `suppressed_suggestions` field on `playbook_catalog` that also folds into the catalog revision. `apb-cli` gets an `apb suggestions list|allow|reset` group and `apb-server` a `GET`/`DELETE /api/suggestions` pair, both thin wrappers over the same core functions; the svelte dashboard renders a compact section on the playbooks page. Two instruction texts (MCP tier 0 and the standing block asset) teach the agent to match by synopsis meaning and to record soft declines.

**Tech Stack:** Rust 2024 workspace (`apb-core`, `apb-engine`, `apb-mcp`, `apb-cli`, `apb-server`), serde plus serde_json and serde_yaml_ng, rmcp for the MCP surface, clap for the CLI, axum for the HTTP API, svelte 5 with shadcn-svelte and Tailwind v4 plus vitest in `web/`, cargo test with the per-crate consolidated integration binaries (`tests/main.rs`).

## Global Constraints

- No em-dashes (U+2014) and no exclamation marks in docs or user-facing strings. No CJK anywhere in code or prose.
- New serde fields only with `#[serde(default)]`, so an older state file or an older tool call still deserializes.
- Atomic state IO via `apb_core::fsutil` (`atomic_write_private` for state files, `lock_dir` for the read-modify-write critical section). Never write a state file with a bare `std::fs::write`.
- Wall clock only via `apb_core::clock` (`now_ms_u64` for the millisecond stamps this store persists). No direct `SystemTime::now` in new code.
- TIER0 byte cap 1950: `crates/apb-mcp/src/instructions.rs` must stay at or under 1950 bytes, verified by a unit test, because Claude Code truncates server instructions at 2KB.
- Commits require owner approval and use `git commit --signoff` plus the `Co-Authored-By` trailer for the acting model. Do not commit or push on your own initiative; the commit step of each task runs only after the owner approves that task.
- `cargo fmt --all -- --check`, `cargo clippy --workspace --all-targets -- -D warnings` and code-ranker must be clean before a task is considered done. Warm the cargo cache first with `cargo metadata --format-version 1 >/dev/null`, then run `code-ranker check .` and read `code-ranker docs base <ID>` before fixing any violation it reports.
- Markdown files are written without hard line wraps (one paragraph per line).
- Secret hygiene: a synopsis is user-visible prose, never a secret value; the existing capture secret scan stays the model for any new text-carrying field.

---

## Task 1: Core store schema 2 (records, two scopes, merge, prune, migration)

**Files:**
- Modify: `/Users/techmeat/www/projects/omniteamhq/agentic-playbooks/crates/apb-core/src/dismiss.rs` (the whole v1 store is replaced in place; the module path stays so `apb_core::dismiss` callers keep working)
- Test: `/Users/techmeat/www/projects/omniteamhq/agentic-playbooks/crates/apb-core/src/dismiss.rs` (`#[cfg(test)] mod tests` at the bottom of the same file, using `crate::env_test_lock()` exactly as the v1 tests do)

**Interfaces:**
- Consumes: `apb_core::fsutil::{atomic_write_private, lock_dir}`, `apb_core::clock::now_ms_u64`, `apb_core::config::config_dir`.
- Produces:
  - `pub const SOFT_BACKOFF_DAYS: [u64; 4] = [1, 7, 30, 90]`
  - `pub const HARD_TTL_DAYS: u64 = 90`
  - `pub const SOFT_RETAIN_DAYS: u64 = 365`
  - `pub enum DecisionKind { Soft, Hard }` with `pub fn as_str(&self) -> &'static str` and `pub fn parse(s: &str) -> Option<DecisionKind>`
  - `pub enum DecisionScope { Project, Global }` with `pub fn as_str(&self) -> &'static str` and `pub fn parse(s: &str) -> Option<DecisionScope>`
  - `pub struct SuggestionRecord { pub pattern: String, pub synopsis: String, pub kind: DecisionKind, pub declines: u32, pub snoozed_until_ms: u64, pub updated_at_ms: u64 }`
  - `pub struct ScopedRecord { pub scope: DecisionScope, pub record: SuggestionRecord }`
  - `pub struct SuggestionView { pub records: Vec<ScopedRecord>, pub diagnostics: Vec<String> }`
  - `pub fn active(root: &Path) -> SuggestionView`
  - `pub fn active_patterns(root: &Path) -> Vec<String>`
  - `pub fn iso_utc(ms: u64) -> String`
  - `pub fn write_record(root: &Path, scope: DecisionScope, record: SuggestionRecord) -> std::io::Result<()>`
  - `pub fn remove_record(root: &Path, pattern: &str, scope: DecisionScope) -> std::io::Result<bool>`
  - `pub fn stored_record(root: &Path, pattern: &str, scope: DecisionScope) -> Option<SuggestionRecord>`

### Steps

- [ ] Write the failing roundtrip test. Replace the whole `#[cfg(test)] mod tests` block at the bottom of `crates/apb-core/src/dismiss.rs` with:

```rust
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
        assert_eq!(active_patterns(proj.path()), vec!["code-review-run"]);
        assert!(view.diagnostics.is_empty());
        assert!(proj.path().join(".apb/suggestions.json").is_file());
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
}
```

- [ ] Run it and watch it fail to compile: `cargo test -p apb-core --lib dismiss::tests` fails with `cannot find type SuggestionRecord in this scope` and friends (the v1 module has no such items).

- [ ] Implement the store head of the module. Replace everything above the `#[cfg(test)]` block in `crates/apb-core/src/dismiss.rs` with:

```rust
//! Store of the user's decisions about "make this a playbook" suggestions
//! (spec 2026-07-29-suggestion-decisions-design, evolving spec 8.2).
//!
//! One record per suggestion pattern, in two scopes: the project store
//! `<root>/.apb/suggestions.json` and the global store
//! `<config-dir>/suggestions.json`. A record carries the agent's one-sentence
//! `synopsis`, so future matching is done by MEANING on the agent side; the
//! server never does language processing. A soft decline escalates the snooze
//! along `SOFT_BACKOFF_DAYS`, a hard decline silences the suggestion for
//! `HARD_TTL_DAYS`. Reads never fail: a corrupt or unreadable file yields an
//! empty list plus a diagnostic string.

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
fn store_dir(root: &Path, scope: DecisionScope) -> Option<PathBuf> {
    match scope {
        DecisionScope::Project => Some(root.join(".apb")),
        DecisionScope::Global => crate::config::config_dir(),
    }
}

/// Reads one store file. `Ok(None)` when the file is absent (a normal empty
/// store); `Err` carries a human-readable diagnostic for a corrupt or
/// unreadable file, which callers surface without failing the read.
fn read_store(path: &Path) -> Result<Option<StoreFile>, String> {
    let raw = match std::fs::read_to_string(path) {
        Ok(r) => r,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => {
            return Err(format!(
                "unreadable suggestion store `{}`: {e}",
                path.display()
            ));
        }
    };
    serde_json::from_str::<StoreFile>(&raw)
        .map(Some)
        .map_err(|e| format!("malformed suggestion store `{}`: {e}", path.display()))
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
/// retained and rewriting the file only when the prune changed something.
fn load_scope(dir: &Path, now: u64, diagnostics: &mut Vec<String>) -> Vec<SuggestionRecord> {
    let _lock = lock_dir(dir, LOCK_NAME).ok();
    let mut file = match read_store(&dir.join(STORE_FILE)) {
        Ok(Some(f)) => f,
        Ok(None) => StoreFile::default(),
        Err(diag) => {
            diagnostics.push(diag);
            return Vec::new();
        }
    };
    let before = file.records.len();
    file.records.retain(|r| is_retained(r, now));
    if file.records.len() != before
        && let Err(e) = write_store(dir, &file)
    {
        diagnostics.push(format!("could not prune `{}`: {e}", dir.display()));
    }
    file.records
}

/// The merged, currently-suppressing records for this project: project scope
/// plus global scope, stricter-wins on a pattern present in both. Never fails.
pub fn active(root: &Path) -> SuggestionView {
    let now = crate::clock::now_ms_u64();
    let mut diagnostics = Vec::new();
    let mut scoped: Vec<ScopedRecord> = Vec::new();
    for scope in [DecisionScope::Project, DecisionScope::Global] {
        let Some(dir) = store_dir(root, scope) else {
            continue;
        };
        if scope == DecisionScope::Global {
            migrate_legacy(&dir, now, &mut diagnostics);
        }
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

/// The active patterns as a plain slug list (the v1 catalog field).
pub fn active_patterns(root: &Path) -> Vec<String> {
    active(root)
        .records
        .into_iter()
        .map(|s| s.record.pattern)
        .collect()
}

/// Writes (inserts or replaces) a record in one scope, under that scope's dir
/// lock.
pub fn write_record(
    root: &Path,
    scope: DecisionScope,
    record: SuggestionRecord,
) -> std::io::Result<()> {
    let Some(dir) = store_dir(root, scope) else {
        return Ok(());
    };
    let _lock = lock_dir(&dir, LOCK_NAME).ok();
    let mut file = read_store(&dir.join(STORE_FILE))
        .unwrap_or(None)
        .unwrap_or_default();
    file.schema = SCHEMA_VERSION;
    file.records.retain(|r| r.pattern != record.pattern);
    file.records.push(record);
    file.records.sort_by(|a, b| a.pattern.cmp(&b.pattern));
    write_store(&dir, &file)
}

/// Removes a record from one scope. `Ok(false)` when there was nothing to
/// remove.
pub fn remove_record(
    root: &Path,
    pattern: &str,
    scope: DecisionScope,
) -> std::io::Result<bool> {
    let Some(dir) = store_dir(root, scope) else {
        return Ok(false);
    };
    let _lock = lock_dir(&dir, LOCK_NAME).ok();
    let mut file = read_store(&dir.join(STORE_FILE))
        .unwrap_or(None)
        .unwrap_or_default();
    let before = file.records.len();
    file.records.retain(|r| r.pattern != pattern);
    if file.records.len() == before {
        return Ok(false);
    }
    file.schema = SCHEMA_VERSION;
    write_store(&dir, &file)?;
    Ok(true)
}

/// The stored record for a pattern in one scope, whether or not it is still
/// active. Used by the surfaces that edit a record (reset) rather than read
/// the suppression set.
pub fn stored_record(
    root: &Path,
    pattern: &str,
    scope: DecisionScope,
) -> Option<SuggestionRecord> {
    let dir = store_dir(root, scope)?;
    read_store(&dir.join(STORE_FILE))
        .ok()
        .flatten()
        .and_then(|f| f.records.into_iter().find(|r| r.pattern == pattern))
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
```

- [ ] Add the merge and migration helpers below `civil_from_days` in the same file:

```rust
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
fn migrate_legacy(dir: &Path, now: u64, diagnostics: &mut Vec<String>) {
    let new_path = dir.join(STORE_FILE);
    let legacy_path = dir.join(LEGACY_FILE);
    if new_path.exists() || !legacy_path.is_file() {
        return;
    }
    let _lock = lock_dir(dir, LOCK_NAME).ok();
    if new_path.exists() {
        return;
    }
    let raw = match std::fs::read_to_string(&legacy_path) {
        Ok(r) => r,
        Err(e) => {
            diagnostics.push(format!(
                "unreadable legacy dismiss store `{}`: {e}",
                legacy_path.display()
            ));
            return;
        }
    };
    let legacy: LegacyFile = match serde_json::from_str(&raw) {
        Ok(l) => l,
        Err(e) => {
            diagnostics.push(format!(
                "malformed legacy dismiss store `{}`: {e}",
                legacy_path.display()
            ));
            return;
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
        return;
    }
    if let Err(e) = std::fs::remove_file(&legacy_path) {
        diagnostics.push(format!(
            "migrated but could not remove `{}`: {e}",
            legacy_path.display()
        ));
    }
}
```

- [ ] Run the tests and watch them pass: `cargo test -p apb-core --lib dismiss::tests` prints `test result: ok. 3 passed`. Note that `apb-mcp` will not compile yet (its two call sites still use the v1 signatures); that is Task 3 and Task 4.

- [ ] Write the failing merge and migration tests. Append inside the same `mod tests`:

```rust
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

        // Same kind: the later snooze wins, and it is the project one here.
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
        assert!(view.diagnostics.iter().any(|d| d.contains("malformed suggestion store")));
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
        assert!(remove_record(proj.path(), "gone-soon", DecisionScope::Project).unwrap());
        assert!(!remove_record(proj.path(), "gone-soon", DecisionScope::Project).unwrap());
        assert!(active(proj.path()).records.is_empty());
    }
```

- [ ] Run them: `cargo test -p apb-core --lib dismiss::tests` prints `test result: ok. 7 passed`. If `stricter_scope_wins_for_the_same_pattern` fails on the second half, check `is_stricter` compares `snoozed_until_ms` for equal kinds.

- [ ] Gate the task: `cargo fmt --all -- --check`, then `cargo clippy -p apb-core --all-targets -- -D warnings`, then `cargo metadata --format-version 1 >/dev/null && code-ranker check .`, fixing anything reported.

- [ ] Commit (after owner approval):

```
git add crates/apb-core/src/dismiss.rs
git commit --signoff -m "feat(core): suggestion decision store schema 2 with two scopes and v1 migration

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
```

---

## Task 2: Backoff engine and the optional `suggestions:` config section

**Files:**
- Modify: `/Users/techmeat/www/projects/omniteamhq/agentic-playbooks/crates/apb-core/src/config.rs` (add `SuggestionSettings`, hang it off `GlobalConfig`, add the project-config reader)
- Modify: `/Users/techmeat/www/projects/omniteamhq/agentic-playbooks/crates/apb-core/src/dismiss.rs` (add `SuggestionTiming`, `timing`, `next_snooze_ms`, `record_decision`, `reset_records`)
- Test: `/Users/techmeat/www/projects/omniteamhq/agentic-playbooks/crates/apb-core/src/dismiss.rs` (`mod tests`)
- Test: `/Users/techmeat/www/projects/omniteamhq/agentic-playbooks/crates/apb-core/tests/suite/config_test.rs` (the `suggestions:` section on the global config)

**Interfaces:**
- Consumes: `apb_core::dismiss::{SOFT_BACKOFF_DAYS, HARD_TTL_DAYS, DecisionKind, DecisionScope, SuggestionRecord, write_record, stored_record}`, `apb_core::config::{GlobalConfig, config_dir}`, `apb_core::clock::now_ms_u64`.
- Produces (in `config.rs`):
  - `pub struct SuggestionSettings { pub soft_backoff_days: Option<Vec<u64>>, pub hard_ttl_days: Option<u64> }` with `pub fn validate(&self) -> Result<(), String>`
  - `GlobalConfig.suggestions: SuggestionSettings`
  - `pub fn project_suggestion_settings(root: &Path) -> Result<SuggestionSettings, String>`
- Produces (in `dismiss.rs`):
  - `pub struct SuggestionTiming { pub soft_backoff_days: Vec<u64>, pub hard_ttl_days: u64 }` with `Default`
  - `pub fn timing(root: &Path) -> (SuggestionTiming, Vec<String>)`
  - `pub fn next_snooze_ms(now_ms: u64, kind: DecisionKind, declines: u32, timing: &SuggestionTiming) -> u64`
  - `pub struct DecisionInput { pub pattern: String, pub synopsis: String, pub kind: DecisionKind, pub scope: DecisionScope, pub hard_ttl_days_override: Option<u64> }`
  - `pub fn record_decision(root: &Path, input: DecisionInput) -> std::io::Result<SuggestionRecord>`
  - `pub struct ResetOutcome { pub reset: Vec<String>, pub skipped_hard: Vec<String> }`
  - `pub fn reset_records(root: &Path, pattern: Option<&str>) -> std::io::Result<ResetOutcome>`

### Steps

- [ ] Write the failing config test. Append to `crates/apb-core/tests/suite/config_test.rs`:

```rust
#[test]
fn suggestions_section_loads_and_validates() {
    let _lock = crate::common::env_lock();
    let dir = tempfile::tempdir().unwrap();
    unsafe {
        std::env::set_var("APB_CONFIG_DIR", dir.path());
    }
    let yaml = "suggestions:\n  soft_backoff_days: [2, 14]\n  hard_ttl_days: 30\n";
    std::fs::write(dir.path().join("config.yaml"), yaml).unwrap();
    let cfg = apb_core::config::GlobalConfig::load().unwrap();
    assert_eq!(cfg.suggestions.soft_backoff_days, Some(vec![2, 14]));
    assert_eq!(cfg.suggestions.hard_ttl_days, Some(30));
    assert!(cfg.suggestions.validate().is_ok());

    // A config with no section at all keeps the defaults (both None).
    std::fs::write(dir.path().join("config.yaml"), "port: 7321\n").unwrap();
    let cfg = apb_core::config::GlobalConfig::load().unwrap();
    assert_eq!(cfg.suggestions.soft_backoff_days, None);
    assert_eq!(cfg.suggestions.hard_ttl_days, None);

    unsafe {
        std::env::remove_var("APB_CONFIG_DIR");
    }
}

#[test]
fn suggestion_settings_reject_empty_arrays_and_zero_days() {
    use apb_core::config::SuggestionSettings;
    let empty = SuggestionSettings {
        soft_backoff_days: Some(Vec::new()),
        hard_ttl_days: None,
    };
    assert!(
        empty.validate().unwrap_err().contains("soft_backoff_days"),
        "an empty schedule must be a validation error"
    );
    let zero = SuggestionSettings {
        soft_backoff_days: Some(vec![1, 0, 7]),
        hard_ttl_days: None,
    };
    assert!(zero.validate().is_err(), "a zero-day step is not positive");
    let zero_ttl = SuggestionSettings {
        soft_backoff_days: None,
        hard_ttl_days: Some(0),
    };
    assert!(zero_ttl.validate().unwrap_err().contains("hard_ttl_days"));
    let ok = SuggestionSettings {
        soft_backoff_days: Some(vec![1, 7, 30, 90]),
        hard_ttl_days: Some(90),
    };
    assert!(ok.validate().is_ok());
}

#[test]
fn project_suggestion_settings_read_the_project_config() {
    let root = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(root.path().join(".apb")).unwrap();
    // The project config carries unrelated keys too, which must not break the
    // partial read.
    std::fs::write(
        root.path().join(".apb/config.yaml"),
        "skills_dir: .agents/skills\nsuggestions:\n  hard_ttl_days: 7\n",
    )
    .unwrap();
    let s = apb_core::config::project_suggestion_settings(root.path()).unwrap();
    assert_eq!(s.hard_ttl_days, Some(7));
    assert_eq!(s.soft_backoff_days, None);

    // No project config at all is an empty settings block, not an error.
    let bare = tempfile::tempdir().unwrap();
    let s = apb_core::config::project_suggestion_settings(bare.path()).unwrap();
    assert_eq!(s.hard_ttl_days, None);
}
```

- [ ] Run it and watch it fail: `cargo test -p apb-core --test main suggestions_section_loads_and_validates` fails with `no field suggestions on type GlobalConfig`.

- [ ] Implement the config side. In `crates/apb-core/src/config.rs`, add the field to `GlobalConfig` right after `registry_purge_days`:

```rust
    /// Timing knobs for the suggestion-decision store (spec
    /// 2026-07-29-suggestion-decisions-design). Absent keys fall back to the
    /// named constants in `crate::dismiss`; a project `.apb/config.yaml` may
    /// override either key for its own project.
    pub suggestions: SuggestionSettings,
```

and add the type plus the project reader at the end of the file:

```rust
/// Optional `suggestions:` section, in the global config and in the project
/// `.apb/config.yaml`. Every key is optional so a user who never touches the
/// section keeps the defaults; a present key overrides only itself.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct SuggestionSettings {
    /// Escalating snooze for repeated soft declines, in days.
    pub soft_backoff_days: Option<Vec<u64>>,
    /// Silence window for a hard dismissal, in days.
    pub hard_ttl_days: Option<u64>,
}

impl SuggestionSettings {
    /// Semantic checks the serde layer cannot express: values are days, so
    /// positive integers, and an empty schedule is a configuration error
    /// rather than "never snooze".
    pub fn validate(&self) -> Result<(), String> {
        if let Some(days) = &self.soft_backoff_days {
            if days.is_empty() {
                return Err("suggestions.soft_backoff_days must not be empty".into());
            }
            if days.iter().any(|d| *d == 0) {
                return Err("suggestions.soft_backoff_days values are days and must be positive"
                    .into());
            }
        }
        if self.hard_ttl_days == Some(0) {
            return Err("suggestions.hard_ttl_days is a number of days and must be positive".into());
        }
        Ok(())
    }
}

/// Partial view of the project `.apb/config.yaml`: only the `suggestions:`
/// section. Deliberately tolerant of the other keys that file carries
/// (`skills_dir`, `port`), the same way `crate::skills` reads it.
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct ProjectSuggestionsFile {
    suggestions: SuggestionSettings,
}

/// The project-level `suggestions:` section. A missing file is an empty
/// section; a malformed one is an error so a typo is not silently ignored.
pub fn project_suggestion_settings(root: &Path) -> Result<SuggestionSettings, String> {
    let path = root.join(".apb/config.yaml");
    if !path.is_file() {
        return Ok(SuggestionSettings::default());
    }
    let raw = std::fs::read_to_string(&path).map_err(|e| e.to_string())?;
    let parsed: ProjectSuggestionsFile = serde_yaml_ng::from_str(&raw)
        .map_err(|e| format!("invalid project config `{}`: {e}", path.display()))?;
    Ok(parsed.suggestions)
}
```

Add the `Path` import at the top of `config.rs`, changing `use std::path::PathBuf;` to `use std::path::{Path, PathBuf};`.

- [ ] Run the config tests: `cargo test -p apb-core --test main suggestion` prints `test result: ok. 3 passed`.

- [ ] Write the failing backoff tests. Append inside `mod tests` in `crates/apb-core/src/dismiss.rs`:

```rust
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

    #[test]
    fn repeated_soft_declines_escalate_the_stored_record() {
        let _lock = crate::env_test_lock();
        let cfg = tempfile::tempdir().unwrap();
        let proj = tempfile::tempdir().unwrap();
        unsafe {
            std::env::set_var("APB_CONFIG_DIR", cfg.path());
        }
        let _g = EnvGuard;

        let input = || DecisionInput {
            pattern: "code-review-run".to_string(),
            synopsis: "Review a source file and write findings to a report".to_string(),
            kind: DecisionKind::Soft,
            scope: DecisionScope::Project,
            hard_ttl_days_override: None,
        };
        let first = record_decision(proj.path(), input()).unwrap();
        assert_eq!(first.declines, 1);
        let second = record_decision(proj.path(), input()).unwrap();
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
        .unwrap();
        assert_eq!(hard.kind, DecisionKind::Hard);
        assert!(hard.snoozed_until_ms >= crate::clock::now_ms_u64() + 89 * MS_PER_DAY);
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
```

- [ ] Run them and watch them fail: `cargo test -p apb-core --lib dismiss::tests` fails with `cannot find type SuggestionTiming in this scope`.

- [ ] Implement the backoff side in `crates/apb-core/src/dismiss.rs`, appended after `remove_record`:

```rust
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

/// Records a decision and returns the stored record, including the server-computed
/// snooze. A soft decline increments the decline counter of an existing record
/// in the same scope (that is the escalation); a hard dismissal replaces the
/// kind and uses the hard TTL. A non-empty synopsis always replaces the stored
/// one; an empty one keeps whatever was there.
pub fn record_decision(root: &Path, input: DecisionInput) -> std::io::Result<SuggestionRecord> {
    let now = crate::clock::now_ms_u64();
    let (mut resolved, _diagnostics) = timing(root);
    if let Some(ttl) = input.hard_ttl_days_override {
        resolved.hard_ttl_days = ttl;
    }
    let previous = stored_record(root, &input.pattern, input.scope);
    let declines = match input.kind {
        DecisionKind::Soft => previous.as_ref().map(|p| p.declines).unwrap_or(0) + 1,
        DecisionKind::Hard => previous.as_ref().map(|p| p.declines).unwrap_or(0),
    };
    let synopsis = if input.synopsis.trim().is_empty() {
        previous.map(|p| p.synopsis).unwrap_or_default()
    } else {
        input.synopsis.trim().to_string()
    };
    let record = SuggestionRecord {
        pattern: input.pattern,
        synopsis,
        kind: input.kind,
        declines,
        snoozed_until_ms: next_snooze_ms(now, input.kind, declines, &resolved),
        updated_at_ms: now,
    };
    write_record(root, input.scope, record.clone())?;
    Ok(record)
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
pub fn reset_records(root: &Path, pattern: Option<&str>) -> std::io::Result<ResetOutcome> {
    let Some(dir) = store_dir(root, DecisionScope::Project) else {
        return Ok(ResetOutcome::default());
    };
    let _lock = lock_dir(&dir, LOCK_NAME).ok();
    let mut file = read_store(&dir.join(STORE_FILE))
        .unwrap_or(None)
        .unwrap_or_default();
    let now = crate::clock::now_ms_u64();
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
    if !outcome.reset.is_empty() {
        file.schema = SCHEMA_VERSION;
        write_store(&dir, &file)?;
    }
    Ok(outcome)
}
```

- [ ] Run the tests: `cargo test -p apb-core --lib dismiss::tests` prints `test result: ok. 12 passed`. If `reset_zeroes_soft_records_and_leaves_hard_ones` fails on the last assertion, confirm `is_active` uses a strict `<` so a `snoozed_until_ms` of 0 is never active.

- [ ] Gate the task: `cargo fmt --all -- --check`, `cargo clippy -p apb-core --all-targets -- -D warnings`, `cargo metadata --format-version 1 >/dev/null && code-ranker check .`.

- [ ] Commit (after owner approval):

```
git add crates/apb-core/src/config.rs crates/apb-core/src/dismiss.rs crates/apb-core/tests/suite/config_test.rs
git commit --signoff -m "feat(core): escalating soft-decline backoff with optional suggestions config

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
```

---

## Task 3: MCP `suggestion_dismiss` with kind, synopsis and scope

**Files:**
- Modify: `/Users/techmeat/www/projects/omniteamhq/agentic-playbooks/crates/apb-mcp/src/server/args.rs` (`SuggestionDismissArgs`)
- Modify: `/Users/techmeat/www/projects/omniteamhq/agentic-playbooks/crates/apb-mcp/src/tools/capture.rs` (`suggestion_dismiss`)
- Modify: `/Users/techmeat/www/projects/omniteamhq/agentic-playbooks/crates/apb-mcp/src/server/playbook.rs` (handler and tool description)
- Test: `/Users/techmeat/www/projects/omniteamhq/agentic-playbooks/crates/apb-mcp/tests/suite/capture_tools_test.rs`

**Interfaces:**
- Consumes: `apb_core::dismiss::{DecisionInput, DecisionKind, DecisionScope, record_decision, iso_utc}` (Task 1 and Task 2).
- Produces:
  - `pub struct DismissRequest<'a> { pub pattern: &'a str, pub synopsis: &'a str, pub kind: Option<&'a str>, pub scope: Option<&'a str>, pub ttl_days: Option<u64> }` in `crates/apb-mcp/src/tools/capture.rs`
  - `pub fn suggestion_dismiss(root: &Path, req: DismissRequest<'_>) -> Result<Value, ToolError>` (same module, replacing the two-argument v1 signature)
  - `SuggestionDismissArgs { pattern: String, ttl_days: Option<u64>, kind: Option<String>, synopsis: String, scope: Option<String> }`

### Steps

- [ ] Write the failing tests. In `crates/apb-mcp/tests/suite/capture_tools_test.rs`, replace the `dismiss_roundtrip_visible_in_catalog` test with:

```rust
#[test]
fn old_style_dismiss_call_is_a_hard_project_dismissal() {
    let _l = lock();
    let cfg = tempfile::tempdir().unwrap();
    let proj = tempfile::tempdir().unwrap();
    setup(cfg.path());
    let _g = EnvGuard;
    init_project(proj.path()).unwrap();

    // An old-style call: pattern only, no new fields.
    let res = suggestion_dismiss(
        proj.path(),
        DismissRequest {
            pattern: "save-cleanup-playbook",
            synopsis: "",
            kind: None,
            scope: None,
            ttl_days: None,
        },
    )
    .unwrap();
    assert_eq!(res["dismissed"], "save-cleanup-playbook");
    assert_eq!(res["kind"], "hard");
    assert_eq!(res["scope"], "project");
    assert_eq!(res["synopsis"], "");
    assert!(
        res["snoozed_until"].as_str().unwrap().ends_with('Z'),
        "the response reports the computed snooze: {res}"
    );

    let cat = playbook_catalog(proj.path(), None, None, None).unwrap();
    let dismissed = cat["dismissed_patterns"].as_array().unwrap();
    assert!(dismissed.iter().any(|p| p == "save-cleanup-playbook"));
}

#[test]
fn soft_dismiss_stores_synopsis_and_reports_the_escalating_snooze() {
    let _l = lock();
    let cfg = tempfile::tempdir().unwrap();
    let proj = tempfile::tempdir().unwrap();
    setup(cfg.path());
    let _g = EnvGuard;
    init_project(proj.path()).unwrap();

    let req = || DismissRequest {
        pattern: "code-review-run",
        synopsis: "Review a source file for bugs and write findings to a report",
        kind: Some("soft"),
        scope: Some("project"),
        ttl_days: None,
    };
    let first = suggestion_dismiss(proj.path(), req()).unwrap();
    assert_eq!(first["kind"], "soft");
    assert_eq!(first["declines"], 1);
    assert_eq!(
        first["synopsis"],
        "Review a source file for bugs and write findings to a report"
    );
    let second = suggestion_dismiss(proj.path(), req()).unwrap();
    assert_eq!(second["declines"], 2);
    assert!(
        second["snoozed_until_ms"].as_u64().unwrap()
            > first["snoozed_until_ms"].as_u64().unwrap(),
        "the second soft decline snoozes further out: {second}"
    );
}

#[test]
fn global_scope_dismissal_is_recorded_globally() {
    let _l = lock();
    let cfg = tempfile::tempdir().unwrap();
    let proj = tempfile::tempdir().unwrap();
    setup(cfg.path());
    let _g = EnvGuard;
    init_project(proj.path()).unwrap();

    let res = suggestion_dismiss(
        proj.path(),
        DismissRequest {
            pattern: "never-anywhere",
            synopsis: "Something the user never wants offered again anywhere",
            kind: Some("hard"),
            scope: Some("global"),
            ttl_days: None,
        },
    )
    .unwrap();
    assert_eq!(res["scope"], "global");
    assert!(cfg.path().join("suggestions.json").is_file());
    assert!(!proj.path().join(".apb/suggestions.json").exists());
}

#[test]
fn unknown_kind_or_scope_is_rejected() {
    let _l = lock();
    let cfg = tempfile::tempdir().unwrap();
    let proj = tempfile::tempdir().unwrap();
    setup(cfg.path());
    let _g = EnvGuard;
    init_project(proj.path()).unwrap();

    let err = suggestion_dismiss(
        proj.path(),
        DismissRequest {
            pattern: "p",
            synopsis: "",
            kind: Some("maybe"),
            scope: None,
            ttl_days: None,
        },
    );
    assert!(err.is_err(), "an unknown kind must not be silently coerced");
    let err = suggestion_dismiss(
        proj.path(),
        DismissRequest {
            pattern: "p",
            synopsis: "",
            kind: None,
            scope: Some("everywhere"),
            ttl_days: None,
        },
    );
    assert!(err.is_err());
}
```

and extend the import at the top of the same file to `use apb_mcp::tools::{DismissRequest, playbook_capture, playbook_catalog, suggestion_dismiss};`.

- [ ] Run them and watch them fail: `cargo test -p apb-mcp --test main capture_tools_test` fails with `cannot find struct DismissRequest` and `this function takes 2 arguments but 2 arguments were supplied` mismatches.

- [ ] Implement the tool. In `crates/apb-mcp/src/tools/capture.rs`, replace the v1 `suggestion_dismiss` at the bottom of the file with:

```rust
/// One `suggestion_dismiss` call. `kind` and `scope` are the raw strings the
/// tool received (`None` means the argument was absent, which is the
/// backward-compatible default: hard, project scope).
#[derive(Debug, Clone)]
pub struct DismissRequest<'a> {
    pub pattern: &'a str,
    pub synopsis: &'a str,
    pub kind: Option<&'a str>,
    pub scope: Option<&'a str>,
    /// Legacy hard-TTL override in days (v1 argument). Applies to a hard
    /// dismissal only.
    pub ttl_days: Option<u64>,
}

/// Records the user's decline of a save-as-playbook suggestion (spec
/// 2026-07-29). A soft decline escalates the snooze, a hard one silences the
/// suggestion for the hard TTL; the response reports the stored record,
/// including the server-computed `snoozed_until`, so the agent can tell the
/// user how long the silence lasts. An absent `kind`/`scope` reproduces v1
/// behavior exactly (hard, project scope).
pub fn suggestion_dismiss(root: &Path, req: DismissRequest<'_>) -> Result<Value, ToolError> {
    let kind = match req.kind {
        None => apb_core::dismiss::DecisionKind::Hard,
        Some(raw) => apb_core::dismiss::DecisionKind::parse(raw)
            .ok_or_else(|| ToolError::Engine(format!("unknown kind `{raw}` (soft or hard)")))?,
    };
    let scope = match req.scope {
        None => apb_core::dismiss::DecisionScope::Project,
        Some(raw) => apb_core::dismiss::DecisionScope::parse(raw).ok_or_else(|| {
            ToolError::Engine(format!("unknown scope `{raw}` (project or global)"))
        })?,
    };
    // A synopsis is prose the user will see again in `apb suggestions list`;
    // the same secret-shape net that guards a capture synopsis applies here.
    if let Some(m) = secret_like(req.synopsis) {
        return Ok(json!({ "rejected": "secret_like_value", "match": m }));
    }
    let stored = apb_core::dismiss::record_decision(
        root,
        apb_core::dismiss::DecisionInput {
            pattern: req.pattern.to_string(),
            synopsis: req.synopsis.to_string(),
            kind,
            scope,
            hard_ttl_days_override: req.ttl_days,
        },
    )
    .map_err(|e| ToolError::Engine(e.to_string()))?;
    Ok(json!({
        // Kept from v1 so an existing client reading `dismissed` still works.
        "dismissed": stored.pattern,
        "pattern": stored.pattern,
        "synopsis": stored.synopsis,
        "kind": stored.kind.as_str(),
        "scope": scope.as_str(),
        "declines": stored.declines,
        "snoozed_until": apb_core::dismiss::iso_utc(stored.snoozed_until_ms),
        "snoozed_until_ms": stored.snoozed_until_ms,
    }))
}
```

- [ ] Wire the args and the handler. In `crates/apb-mcp/src/server/args.rs`, replace `SuggestionDismissArgs` with:

```rust
#[derive(Debug, Deserialize, JsonSchema)]
pub struct SuggestionDismissArgs {
    /// English kebab-slug identifying the suggestion. A stable record id, not
    /// the matching key: matching is done by the synopsis below.
    pub pattern: String,
    /// Legacy hard-TTL override in days; defaults to 90. Applies to a hard
    /// dismissal only.
    #[serde(default)]
    pub ttl_days: Option<u64>,
    /// "soft" for "not now" (the snooze escalates with every repeat) or
    /// "hard" for an explicit never-again. Absent means hard, so an old-style
    /// call keeps its old meaning.
    #[serde(default)]
    pub kind: Option<String>,
    /// One English sentence describing the action that was offered. Strongly
    /// recommended: this is what a future session compares a candidate action
    /// against, by meaning. Never put secret values here.
    #[serde(default)]
    pub synopsis: String,
    /// "project" (default) or "global". Use global only when the user's own
    /// wording says everywhere.
    #[serde(default)]
    pub scope: Option<String>,
}
```

In `crates/apb-mcp/src/server/playbook.rs`, replace the `suggestion_dismiss` handler with:

```rust
    #[tool(
        description = "Record the user's decline of a save-as-playbook suggestion. kind soft is a not-now decline whose silence escalates with every repeat, kind hard (the default) is an explicit never-again. Always pass a one-sentence synopsis of the action: later sessions match against it by meaning. scope defaults to project; use global only when the user says everywhere. Returns the stored record with the computed snoozed_until.",
        annotations(destructive_hint = true)
    )]
    pub(crate) async fn suggestion_dismiss(
        &self,
        Parameters(SuggestionDismissArgs {
            pattern,
            ttl_days,
            kind,
            synopsis,
            scope,
        }): Parameters<SuggestionDismissArgs>,
    ) -> CallToolResult {
        to_call_tool_result(tools::suggestion_dismiss(
            &self.root,
            tools::DismissRequest {
                pattern: &pattern,
                synopsis: &synopsis,
                kind: kind.as_deref(),
                scope: scope.as_deref(),
                ttl_days,
            },
        ))
    }
```

- [ ] Run the tests: `cargo test -p apb-mcp --test main capture_tools_test` prints `test result: ok` for the four dismiss tests. The `playbook_catalog` assertion in the first test passes only after Task 4, so if it fails with `dismissed_patterns` missing, complete Task 4 and re-run.

- [ ] Gate the task: `cargo fmt --all -- --check`, `cargo clippy -p apb-mcp --all-targets -- -D warnings`, `cargo metadata --format-version 1 >/dev/null && code-ranker check .`.

- [ ] Commit (after owner approval):

```
git add crates/apb-mcp/src/server/args.rs crates/apb-mcp/src/server/playbook.rs crates/apb-mcp/src/tools/capture.rs crates/apb-mcp/tests/suite/capture_tools_test.rs
git commit --signoff -m "feat(mcp): suggestion_dismiss takes kind, synopsis and scope

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
```

---

## Task 4: Catalog `suppressed_suggestions` and the revision fold

**Files:**
- Modify: `/Users/techmeat/www/projects/omniteamhq/agentic-playbooks/crates/apb-mcp/src/catalog.rs` (`build`, `compute_revision`, the new payload field)
- Modify: `/Users/techmeat/www/projects/omniteamhq/agentic-playbooks/crates/apb-mcp/src/tools/playbook.rs` (`playbook_catalog` passes the merged records)
- Modify: `/Users/techmeat/www/projects/omniteamhq/agentic-playbooks/crates/apb-mcp/src/tools/capture.rs` (the dedup call to `catalog::build`)
- Test: `/Users/techmeat/www/projects/omniteamhq/agentic-playbooks/crates/apb-mcp/tests/suite/catalog_tools_test.rs`

**Interfaces:**
- Consumes: `apb_core::dismiss::{ScopedRecord, active, iso_utc}` (Task 1), `apb_mcp::tools::{DismissRequest, suggestion_dismiss}` (Task 3).
- Produces:
  - `pub fn build(root: &Path, workspace_id: Option<&str>, revision: Option<&str>, limit: Option<usize>, suppressed: Vec<ScopedRecord>) -> Value` (the fifth parameter replaces `dismissed_patterns: Vec<String>`)
  - response fields `dismissed_patterns` (unchanged shape) and `suppressed_suggestions`

### Steps

- [ ] Write the failing tests. Append to `crates/apb-mcp/tests/suite/catalog_tools_test.rs` (reusing that file's existing env-lock and setup helpers; if it has none, copy the `EnvGuard`/`setup` pair from `capture_tools_test.rs` verbatim into it):

```rust
#[test]
fn catalog_returns_suppressed_suggestions_and_moves_its_revision() {
    let _l = lock();
    let cfg = tempfile::tempdir().unwrap();
    let proj = tempfile::tempdir().unwrap();
    setup(cfg.path());
    let _g = EnvGuard;
    init_project(proj.path()).unwrap();

    let before = playbook_catalog(proj.path(), None, None, None).unwrap();
    let rev0 = before["catalog_revision"].as_str().unwrap().to_string();
    assert_eq!(before["suppressed_suggestions"].as_array().unwrap().len(), 0);

    let req = || apb_mcp::tools::DismissRequest {
        pattern: "code-review-run",
        synopsis: "Review a source file for bugs and write findings to a report",
        kind: Some("soft"),
        scope: Some("project"),
        ttl_days: None,
    };
    apb_mcp::tools::suggestion_dismiss(proj.path(), req()).unwrap();

    let after = playbook_catalog(proj.path(), None, None, None).unwrap();
    let rev1 = after["catalog_revision"].as_str().unwrap().to_string();
    assert_ne!(rev0, rev1, "a dismiss write must move the revision");
    let suppressed = after["suppressed_suggestions"].as_array().unwrap();
    assert_eq!(suppressed.len(), 1, "{after}");
    assert_eq!(suppressed[0]["pattern"], "code-review-run");
    assert_eq!(
        suppressed[0]["synopsis"],
        "Review a source file for bugs and write findings to a report"
    );
    assert_eq!(suppressed[0]["kind"], "soft");
    assert_eq!(suppressed[0]["scope"], "project");
    assert!(suppressed[0]["snoozed_until"].as_str().unwrap().ends_with('Z'));
    // Backward compatibility: the slug list is still there.
    let dismissed = after["dismissed_patterns"].as_array().unwrap();
    assert!(dismissed.iter().any(|p| p == "code-review-run"));

    // A second soft decline changes only the snooze, and the revision must
    // still move: otherwise a client holding rev1 would get unchanged: true
    // and never see the longer silence.
    apb_mcp::tools::suggestion_dismiss(proj.path(), req()).unwrap();
    let third = playbook_catalog(proj.path(), None, Some(&rev1), None).unwrap();
    assert!(
        third["unchanged"].is_null(),
        "the revision must not match after a second decline: {third}"
    );

    // A matching revision still short-circuits.
    let rev2 = third["catalog_revision"].as_str().unwrap().to_string();
    let same = playbook_catalog(proj.path(), None, Some(&rev2), None).unwrap();
    assert_eq!(same["unchanged"], true, "{same}");
}
```

- [ ] Run it and watch it fail: `cargo test -p apb-mcp --test main catalog_returns_suppressed_suggestions_and_moves_its_revision` fails on `suppressed_suggestions` being null.

- [ ] Implement the catalog change. In `crates/apb-mcp/src/catalog.rs`, add the import `use apb_core::dismiss::ScopedRecord;`, then replace `compute_revision` and the `build` signature and body sections as follows:

```rust
/// The JSON shape of one suppressing record: what the agent needs to decide
/// "is this the same action" (the synopsis) and how long the silence lasts.
fn suppressed_json(records: &[ScopedRecord]) -> Vec<Value> {
    records
        .iter()
        .map(|s| {
            json!({
                "pattern": s.record.pattern,
                "synopsis": s.record.synopsis,
                "kind": s.record.kind.as_str(),
                "scope": s.scope.as_str(),
                "declines": s.record.declines,
                "snoozed_until": apb_core::dismiss::iso_utc(s.record.snoozed_until_ms),
            })
        })
        .collect()
}

/// A stable catalog revision: the digest of a canonical concatenation of all
/// entries PLUS the suppressing suggestion records and diagnostics. Folding the
/// records in (not just their slugs) matters: a second soft decline changes
/// only the snooze, and a client holding the previous revision must not get
/// `unchanged: true` and miss the longer silence.
fn compute_revision(
    entries: &[CatalogEntry],
    suppressed: &[ScopedRecord],
    diagnostics: &[Value],
) -> String {
    let mut lines: Vec<String> = entries
        .iter()
        .map(|e| {
            let scope = match e.playbook_ref.origin {
                Origin::Global => "global",
                Origin::Project { .. } => "project",
            };
            format!(
                "e|{scope}|{}|{}|{}|{}|{}|{}|{}",
                e.playbook_ref.id,
                e.playbook_ref.version.as_deref().unwrap_or(""),
                e.digest,
                e.lifecycle,
                e.trusted,
                e.shadowed,
                e.ambiguous
            )
        })
        .collect();
    for s in suppressed {
        // The `d|` line keeps the v1 slug contribution; the `s|` line adds the
        // rest of the record.
        lines.push(format!("d|{}", s.record.pattern));
        lines.push(format!(
            "s|{}|{}|{}|{}|{}|{}",
            s.scope.as_str(),
            s.record.pattern,
            s.record.kind.as_str(),
            s.record.declines,
            s.record.snoozed_until_ms,
            s.record.synopsis
        ));
    }
    for diag in diagnostics {
        lines.push(format!("x|{diag}"));
    }
    lines.sort();
    digest_str(&lines.join("\n"))
}
```

Then change `build`'s signature and the two places that used `dismissed_patterns`:

```rust
/// Builds the catalog for the project root `root` plus the global store.
/// `revision` - if it matches, returns `{ unchanged: true }`. `limit` -
/// an optional cap on the number of entries (after sorting and shadowing).
/// `suppressed` - the merged active suggestion-decision records for this
/// project (`apb_core::dismiss::active`), returned both as the v1
/// `dismissed_patterns` slug list and as the full `suppressed_suggestions`.
pub fn build(
    root: &Path,
    workspace_id: Option<&str>,
    revision: Option<&str>,
    limit: Option<usize>,
    suppressed: Vec<ScopedRecord>,
) -> Value {
```

with the revision call becoming `let catalog_revision = compute_revision(&entries, &suppressed, &diagnostics);` and the final payload becoming:

```rust
    let dismissed_patterns: Vec<String> = suppressed
        .iter()
        .map(|s| s.record.pattern.clone())
        .collect();
    json!({
        "catalog_revision": catalog_revision,
        "entries": entries,
        "diagnostics": diagnostics,
        "dismissed_patterns": dismissed_patterns,
        "suppressed_suggestions": suppressed_json(&suppressed),
        "profiles_hint": { "count": profiles_count },
    })
```

- [ ] Update the two callers. In `crates/apb-mcp/src/tools/playbook.rs`, replace the body of `playbook_catalog` with:

```rust
pub fn playbook_catalog(
    root: &Path,
    workspace_id: Option<&str>,
    revision: Option<&str>,
    limit: Option<usize>,
) -> Result<Value, ToolError> {
    let suppressed = apb_core::dismiss::active(root).records;
    Ok(crate::catalog::build(
        root,
        workspace_id,
        revision,
        limit,
        suppressed,
    ))
}
```

In `crates/apb-mcp/src/tools/capture.rs`, the dedup call keeps its shape and only its empty-vec type changes, so no edit is needed there beyond confirming it still reads `let catalog = crate::catalog::build(root, None, None, None, Vec::new());` and compiles.

- [ ] Run the tests: `cargo test -p apb-mcp --test main catalog` and `cargo test -p apb-mcp --test main capture_tools_test` both pass, including `old_style_dismiss_call_is_a_hard_project_dismissal` from Task 3.

- [ ] Gate the task: `cargo fmt --all -- --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo metadata --format-version 1 >/dev/null && code-ranker check .`.

- [ ] Commit (after owner approval):

```
git add crates/apb-mcp/src/catalog.rs crates/apb-mcp/src/tools/playbook.rs crates/apb-mcp/tests/suite/catalog_tools_test.rs
git commit --signoff -m "feat(mcp): catalog carries suppressed_suggestions and folds them into the revision

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
```

---

## Task 5: Instruction texts (tier 0 and the standing block)

**Files:**
- Modify: `/Users/techmeat/www/projects/omniteamhq/agentic-playbooks/crates/apb-mcp/src/instructions.rs` (new TIER0 text plus a byte-cap unit test)
- Modify: `/Users/techmeat/www/projects/omniteamhq/agentic-playbooks/crates/apb-cli/assets/playbook-instructions.md` (new standing block)
- Test: `/Users/techmeat/www/projects/omniteamhq/agentic-playbooks/crates/apb-mcp/src/instructions.rs` (`#[cfg(test)] mod tests`, new)
- Test: `/Users/techmeat/www/projects/omniteamhq/agentic-playbooks/crates/apb-cli/src/onboarding.rs` (extend `playbook_block_carries_the_discovery_and_capture_duties`)

**Interfaces:**
- Consumes: nothing new. Nothing pins TIER0 today (its only reader is `crates/apb-mcp/src/server/mod.rs` via `.with_instructions(crate::instructions::TIER0)`), so this task adds the pin.
- Produces: `pub const TIER0: &str` (new body, 1935 bytes) and `pub const TIER0_MAX_BYTES: usize = 1950` in `crates/apb-mcp/src/instructions.rs`.

### Steps

- [ ] Write the failing pin test. Append to `crates/apb-mcp/src/instructions.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    /// Claude Code truncates a server's `instructions` at 2KB (measured, July
    /// 2026, see docs/HOST-INTEGRATION.md), so the text has a hard budget. A
    /// silent overrun would drop the tail of the text rather than fail, which
    /// is why this is pinned by a test rather than by a comment.
    #[test]
    fn tier0_fits_the_host_budget() {
        assert!(
            TIER0.len() <= TIER0_MAX_BYTES,
            "TIER0 is {} bytes, over the {TIER0_MAX_BYTES} byte cap",
            TIER0.len()
        );
    }

    #[test]
    fn tier0_keeps_the_load_bearing_rules() {
        for phrase in [
            "playbook_catalog",
            "you MUST offer once to save it with playbook_capture",
            "suppressed_suggestions",
            "by synopsis meaning, not slug",
            "suggestion_dismiss",
            "kind soft",
            "kind hard",
            "review_decide",
            "profile_list",
            "projects_list",
        ] {
            assert!(TIER0.contains(phrase), "TIER0 lost the phrase `{phrase}`");
        }
    }

    #[test]
    fn tier0_follows_the_prose_conventions() {
        assert!(!TIER0.contains('\u{2014}'), "no em-dashes in user-facing text");
        assert!(!TIER0.contains('!'), "no exclamation marks in user-facing text");
    }
}
```

- [ ] Run it and watch it fail: `cargo test -p apb-mcp --lib instructions::tests` fails with `cannot find value TIER0_MAX_BYTES in this scope`, and `tier0_keeps_the_load_bearing_rules` fails on `suppressed_suggestions`.

- [ ] Replace the whole body of `crates/apb-mcp/src/instructions.rs` above the test module with the new text (1935 bytes, verified by the test above):

```rust
//! Tier 0 (spec 4): static behavior rules baked into
//! `ServerInfo.instructions`. Only our trusted text - no project data
//! (injection hygiene). The catalog and details are pulled by tools.

/// Byte budget for the instructions field: Claude Code truncates a server's
/// instructions at 2KB, so the shipped text stays under this cap (pinned by a
/// unit test, see docs/HOST-INTEGRATION.md).
pub const TIER0_MAX_BYTES: usize = 1950;

pub const TIER0: &str = "\
Discovery: call playbook_catalog once per task that names a doable action, before acting. It returns trigger, effects, trust, scope and suppressed_suggestions. Skip chit-chat.

Offering to save: if you just completed a multi-step repeatable action, or the user asks for one recurring by nature, and no playbook matched, you MUST offer once to save it with playbook_capture: one short question offering project or global scope, recommended first (project if project-specific). First compare the action with suppressed_suggestions by synopsis meaning, not slug; a covering record means no offer. One offer per session.

Declines: when the user declines without saying never, call suggestion_dismiss with kind soft, project scope and a one-sentence synopsis; the server escalates the silence. Reserve kind hard for an explicit never-again, global scope for everywhere-wording. Never ask about scope.

Using a match: on a confident match to an active, trusted playbook here or global, name it in one line and run it. One short question if ambiguous; confirm first for another project.

Running policy: the server refuses drafts and untrusted playbooks until trial or acknowledgement. Effects beyond the request (network, secrets, deploys, irreversible) need user confirmation.

Human gates: run_status, supervisor_wait_event and supervisor_run_inspect return pending_review at a human_review gate. The moment you see it you MUST relay its instruction in the user's language with the options, then record the answer with review_decide. Frozen until then; repeat while pending.

Profiles: a node binds its executor only through a profile (agent, model, fallbacks, role prompt, skills). Call profile_list to reuse one, profile_howto for format.

Lifecycle: you may update, clone, version and delete playbooks; pull playbook_howto when authoring. Call projects_list for another workspace. Machine fields are English; speak the user's language.";
```

- [ ] Run the pin tests: `cargo test -p apb-mcp --lib instructions::tests` prints `test result: ok. 3 passed`. Print the exact byte count for the record with `cargo test -p apb-mcp --lib instructions::tests::tier0_fits_the_host_budget -- --nocapture`; the text above is 1935 bytes, 15 under the cap.

- [ ] Write the failing standing-block test. In `crates/apb-cli/src/onboarding.rs`, replace `playbook_block_carries_the_discovery_and_capture_duties` with:

```rust
    #[test]
    fn playbook_block_carries_the_discovery_capture_and_decline_duties() {
        assert!(PLAYBOOK_BLOCK.starts_with(PLAYBOOKS.marker));
        for tool in ["playbook_catalog", "playbook_capture", "suggestion_dismiss"] {
            assert!(
                PLAYBOOK_BLOCK.contains(tool),
                "playbook block lost the reference to {tool}"
            );
        }
        for phrase in [
            "suppressed_suggestions",
            "not by slug equality",
            "kind soft",
            "kind hard",
            "global scope only when",
        ] {
            assert!(
                PLAYBOOK_BLOCK.contains(phrase),
                "playbook block lost the phrase `{phrase}`"
            );
        }
        assert!(!PLAYBOOK_BLOCK.contains('\u{2014}'));
        assert!(!PLAYBOOK_BLOCK.contains('!'));
    }
```

- [ ] Run it and watch it fail: `cargo test -p apb-cli --lib onboarding::tests::playbook_block_carries_the_discovery_capture_and_decline_duties` fails on `suppressed_suggestions`.

- [ ] Replace the whole content of `crates/apb-cli/assets/playbook-instructions.md` with (one paragraph per line, no hard wraps):

```markdown
## apb playbooks (standing instruction)

This project uses apb playbooks: saved, repeatable processes managed through the agentic-playbooks MCP server. Two duties apply to every task, in addition to whatever skills or tools you use to perform the work itself.

Before acting on a task that describes a doable action, call playbook_catalog once to check whether a saved playbook already fits. On a confident match to an active, trusted playbook, name it in one line and use it.

After finishing the work: if either (a) the action you just completed was multi-step and likely to be repeated, or (b) the user asked for an action that is recurring by nature, and no playbook matched, you MUST offer once to save it as a playbook with playbook_capture. Ask exactly one short question offering project or global scope with the recommended option first. Before offering, compare the candidate action against the catalog's suppressed_suggestions by the meaning of each record's synopsis, not by slug equality: a record that already covers the action means no offer. At most one offer per session.

When the user declines an offer without saying never, record it with suggestion_dismiss using kind soft, project scope, and a one-sentence synopsis of the action; the server computes an escalating silence, so a repeated decline is honored longer. Reserve kind hard for an explicit never-again, and use global scope only when the user's own wording says everywhere. Do not ask an extra question about scope, and never put secret values into a synopsis.
```

- [ ] Run the CLI unit tests: `cargo test -p apb-cli --lib onboarding` passes, including the idempotency tests that only depend on the marker heading (unchanged).

- [ ] Gate the task: `cargo fmt --all -- --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo metadata --format-version 1 >/dev/null && code-ranker check .`.

- [ ] Commit (after owner approval):

```
git add crates/apb-mcp/src/instructions.rs crates/apb-cli/assets/playbook-instructions.md crates/apb-cli/src/onboarding.rs
git commit --signoff -m "feat(instructions): semantic suggestion matching and soft-decline duty in tier 0 and the standing block

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
```

---

## Task 6: CLI `apb suggestions list|allow|reset`

**Files:**
- Create: `/Users/techmeat/www/projects/omniteamhq/agentic-playbooks/crates/apb-cli/src/suggestions.rs`
- Modify: `/Users/techmeat/www/projects/omniteamhq/agentic-playbooks/crates/apb-cli/src/main.rs` (module, `Command::Suggestions`, dispatch arm)
- Modify: `/Users/techmeat/www/projects/omniteamhq/agentic-playbooks/crates/apb-cli/src/util.rs` (shared `print_table`)
- Modify: `/Users/techmeat/www/projects/omniteamhq/agentic-playbooks/crates/apb-cli/src/connector.rs` (drop the private `print_table`, use the shared one)
- Create: `/Users/techmeat/www/projects/omniteamhq/agentic-playbooks/crates/apb-cli/tests/suite/suggestions_cli_test.rs`
- Modify: `/Users/techmeat/www/projects/omniteamhq/agentic-playbooks/crates/apb-cli/tests/main.rs` (register the new suite module)

**Interfaces:**
- Consumes: `apb_core::dismiss::{DecisionKind, DecisionScope, ResetOutcome, active, iso_utc, remove_record, reset_records}` (Tasks 1 and 2), `crate::util::print_json`.
- Produces:
  - `pub(crate) enum SuggestionsAction { List { json: bool }, Allow { pattern: String, global: bool }, Reset { pattern: Option<String>, all: bool } }`
  - `pub(crate) fn suggestions_cmd(root: &Path, action: SuggestionsAction) -> ExitCode`
  - `pub(crate) fn print_table(rows: &[Vec<String>])` in `crates/apb-cli/src/util.rs`

### Steps

- [ ] Write the failing integration test. Create `crates/apb-cli/tests/suite/suggestions_cli_test.rs`:

```rust
//! `apb suggestions list|allow|reset` against a temp global config dir and a
//! temp project, driving the real binary the way the other CLI suites do.

use std::path::Path;
use std::process::Command;

fn apb_bin() -> &'static str {
    env!("CARGO_BIN_EXE_apb")
}

fn run(cfg: &Path, cwd: &Path, args: &[&str]) -> (String, String, bool) {
    let out = Command::new(apb_bin())
        .args(["suggestions"])
        .args(args)
        .current_dir(cwd)
        .env("APB_CONFIG_DIR", cfg)
        .env_remove("CI")
        .env_remove("APB_NO_REGISTRY")
        .output()
        .expect("run apb suggestions");
    (
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
        out.status.success(),
    )
}

/// Seeds records through the core store, which is what the MCP tool writes too.
fn seed(root: &Path, pattern: &str, synopsis: &str, kind: &str, scope: &str) {
    apb_core::dismiss::record_decision(
        root,
        apb_core::dismiss::DecisionInput {
            pattern: pattern.to_string(),
            synopsis: synopsis.to_string(),
            kind: apb_core::dismiss::DecisionKind::parse(kind).unwrap(),
            scope: apb_core::dismiss::DecisionScope::parse(scope).unwrap(),
            hard_ttl_days_override: None,
        },
    )
    .unwrap();
}

#[test]
fn list_shows_both_scopes_as_a_table_and_as_json() {
    let cfg = tempfile::tempdir().unwrap();
    let proj = tempfile::tempdir().unwrap();
    apb_core::registry::init_project(proj.path()).unwrap();
    unsafe {
        std::env::set_var("APB_CONFIG_DIR", cfg.path());
    }
    seed(proj.path(), "code-review-run", "Review a file", "soft", "project");
    seed(proj.path(), "never-anywhere", "Never offer this", "hard", "global");
    unsafe {
        std::env::remove_var("APB_CONFIG_DIR");
    }

    let (stdout, stderr, ok) = run(cfg.path(), proj.path(), &["list"]);
    assert!(ok, "list failed: {stderr}");
    assert!(stdout.contains("PATTERN"), "no table header: {stdout}");
    assert!(stdout.contains("code-review-run"), "{stdout}");
    assert!(stdout.contains("never-anywhere"), "{stdout}");
    assert!(stdout.contains("project"), "{stdout}");
    assert!(stdout.contains("global"), "{stdout}");
    assert!(stdout.contains("Review a file"), "{stdout}");

    let (stdout, stderr, ok) = run(cfg.path(), proj.path(), &["list", "--json"]);
    assert!(ok, "list --json failed: {stderr}");
    let parsed: serde_json::Value = serde_json::from_str(&stdout).expect("json output");
    let rows = parsed["suggestions"].as_array().unwrap();
    assert_eq!(rows.len(), 2, "{parsed}");
    let soft = rows
        .iter()
        .find(|r| r["pattern"] == "code-review-run")
        .unwrap();
    assert_eq!(soft["kind"], "soft");
    assert_eq!(soft["scope"], "project");
    assert_eq!(soft["declines"], 1);
    assert!(soft["snoozed_until"].as_str().unwrap().ends_with('Z'));
}

#[test]
fn allow_removes_a_record_in_the_requested_scope() {
    let cfg = tempfile::tempdir().unwrap();
    let proj = tempfile::tempdir().unwrap();
    apb_core::registry::init_project(proj.path()).unwrap();
    unsafe {
        std::env::set_var("APB_CONFIG_DIR", cfg.path());
    }
    seed(proj.path(), "in-project", "Project one", "hard", "project");
    seed(proj.path(), "in-global", "Global one", "hard", "global");
    unsafe {
        std::env::remove_var("APB_CONFIG_DIR");
    }

    let (stdout, stderr, ok) = run(cfg.path(), proj.path(), &["allow", "in-project"]);
    assert!(ok, "allow failed: {stderr}");
    assert!(stdout.contains("in-project"), "{stdout}");

    // Without --global the global record is untouched, and the command says so.
    let (_out, _err, ok) = run(cfg.path(), proj.path(), &["allow", "in-global"]);
    assert!(!ok, "removing a global record without --global must not report success");

    let (stdout, stderr, ok) = run(cfg.path(), proj.path(), &["allow", "in-global", "--global"]);
    assert!(ok, "allow --global failed: {stderr}");
    assert!(stdout.contains("in-global"), "{stdout}");

    let (stdout, _err, ok) = run(cfg.path(), proj.path(), &["list", "--json"]);
    assert!(ok);
    let parsed: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    assert_eq!(parsed["suggestions"].as_array().unwrap().len(), 0, "{parsed}");
}

#[test]
fn reset_clears_soft_records_and_refuses_hard_ones() {
    let cfg = tempfile::tempdir().unwrap();
    let proj = tempfile::tempdir().unwrap();
    apb_core::registry::init_project(proj.path()).unwrap();
    unsafe {
        std::env::set_var("APB_CONFIG_DIR", cfg.path());
    }
    seed(proj.path(), "soft-one", "Soft one", "soft", "project");
    seed(proj.path(), "soft-two", "Soft two", "soft", "project");
    seed(proj.path(), "hard-one", "Hard one", "hard", "project");
    unsafe {
        std::env::remove_var("APB_CONFIG_DIR");
    }

    let (stdout, stderr, ok) = run(cfg.path(), proj.path(), &["reset", "hard-one"]);
    assert!(!ok, "resetting a hard record is refused: {stdout} {stderr}");
    assert!(
        stdout.contains("apb suggestions allow") || stderr.contains("apb suggestions allow"),
        "the refusal must point at allow: {stdout} {stderr}"
    );

    let (stdout, stderr, ok) = run(cfg.path(), proj.path(), &["reset", "soft-one"]);
    assert!(ok, "reset failed: {stderr}");
    assert!(stdout.contains("soft-one"), "{stdout}");

    let (stdout, stderr, ok) = run(cfg.path(), proj.path(), &["reset", "--all"]);
    assert!(ok, "reset --all failed: {stderr}");
    assert!(stdout.contains("soft-two"), "{stdout}");

    // Both soft records are inactive now; the hard one still suppresses.
    let (stdout, _err, ok) = run(cfg.path(), proj.path(), &["list", "--json"]);
    assert!(ok);
    let parsed: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    let rows = parsed["suggestions"].as_array().unwrap();
    assert_eq!(rows.len(), 1, "{parsed}");
    assert_eq!(rows[0]["pattern"], "hard-one");
}
```

- [ ] Register the module. In `crates/apb-cli/tests/main.rs`, add after the `run_doctor_cli_test` entry:

```rust
#[path = "suite/suggestions_cli_test.rs"]
mod suggestions_cli_test;
```

- [ ] Run it and watch it fail: `cargo test -p apb-cli --test main suggestions_cli_test` fails because `apb suggestions` is not a known subcommand (clap exits with a usage error, so `ok` is false and stdout has no table header).

- [ ] Share the table printer. In `crates/apb-cli/src/util.rs`, add:

```rust
/// Prints left-aligned, space-padded columns with a two-space gutter. Rows are
/// ragged-tolerant: a short row simply has fewer columns. The first row is the
/// header by convention of the callers.
pub(crate) fn print_table(rows: &[Vec<String>]) {
    let cols = rows.iter().map(Vec::len).max().unwrap_or(0);
    let mut widths = vec![0usize; cols];
    for row in rows {
        for (i, cell) in row.iter().enumerate() {
            widths[i] = widths[i].max(cell.len());
        }
    }
    for row in rows {
        let mut line = String::new();
        for (i, cell) in row.iter().enumerate() {
            if i > 0 {
                line.push_str("  ");
            }
            line.push_str(&format!("{:<width$}", cell, width = widths[i]));
        }
        println!("{}", line.trim_end());
    }
}
```

In `crates/apb-cli/src/connector.rs`, delete the private `fn print_table(rows: &[[String; 4]])` definition (lines 212 to 229 of that file), and change `use crate::util::print_json;` to `use crate::util::{print_json, print_table};`. Then change the header row in `list_cmd` from the fixed-size array form to the shared vector form:

```rust
        let mut rows: Vec<Vec<String>> = vec![vec![
            "NAME".to_string(),
            "VERSION".to_string(),
            "TRUST".to_string(),
            "ACCOUNTS".to_string(),
        ]];
```

and the single per-connector push in the same function to:

```rust
            rows.push(vec![
                s.name.clone(),
                s.version.clone(),
                trust_state.to_string(),
                accounts_count.to_string(),
            ]);
```

- [ ] Implement the command. Create `crates/apb-cli/src/suggestions.rs`:

```rust
//! `apb suggestions` subcommands (spec 2026-07-29-suggestion-decisions-design
//! section "CLI"): a thin dispatch over `apb_core::dismiss` so the user can see
//! and undo what the agent recorded. `list` reads both scopes, `allow` removes
//! a record outright (offers resume immediately), `reset` zeroes a soft
//! record's escalation while keeping its synopsis.

use std::path::Path;
use std::process::ExitCode;

use apb_core::dismiss::{self, DecisionScope};
use clap::Subcommand;
use serde_json::json;

use crate::util::{print_json, print_table};

#[derive(Subcommand)]
pub(crate) enum SuggestionsAction {
    /// Show the suggestion decisions that currently silence an offer, from the
    /// project and the global store
    List {
        /// Machine-readable output for scripts
        #[arg(long)]
        json: bool,
    },
    /// Remove a record so the suggestion can be offered again right away
    Allow {
        pattern: String,
        /// Remove the global record instead of the project one
        #[arg(long)]
        global: bool,
    },
    /// Zero a soft record's decline counter and clear its snooze, keeping the
    /// record so its synopsis stays available. Project scope only; a hard
    /// record is removed with `apb suggestions allow`.
    Reset {
        /// Pattern to reset; omit only with --all
        pattern: Option<String>,
        /// Reset every soft record in the project scope
        #[arg(long)]
        all: bool,
    },
}

pub(crate) fn suggestions_cmd(root: &Path, action: SuggestionsAction) -> ExitCode {
    match action {
        SuggestionsAction::List { json } => list_cmd(root, json),
        SuggestionsAction::Allow { pattern, global } => allow_cmd(root, &pattern, global),
        SuggestionsAction::Reset { pattern, all } => reset_cmd(root, pattern.as_deref(), all),
    }
}

fn list_cmd(root: &Path, as_json: bool) -> ExitCode {
    let view = dismiss::active(root);
    if as_json {
        let rows: Vec<serde_json::Value> = view
            .records
            .iter()
            .map(|s| {
                json!({
                    "pattern": s.record.pattern,
                    "synopsis": s.record.synopsis,
                    "kind": s.record.kind.as_str(),
                    "scope": s.scope.as_str(),
                    "declines": s.record.declines,
                    "snoozed_until": dismiss::iso_utc(s.record.snoozed_until_ms),
                })
            })
            .collect();
        print_json(&json!({ "suggestions": rows, "diagnostics": view.diagnostics }));
        return ExitCode::SUCCESS;
    }
    for diag in &view.diagnostics {
        eprintln!("apb: {diag}");
    }
    if view.records.is_empty() {
        println!("no suggestion decisions recorded (offers are not silenced here)");
        return ExitCode::SUCCESS;
    }
    let mut rows: Vec<Vec<String>> = vec![vec![
        "PATTERN".to_string(),
        "SCOPE".to_string(),
        "KIND".to_string(),
        "DECLINES".to_string(),
        "UNTIL".to_string(),
        "SYNOPSIS".to_string(),
    ]];
    for s in &view.records {
        rows.push(vec![
            s.record.pattern.clone(),
            s.scope.as_str().to_string(),
            s.record.kind.as_str().to_string(),
            s.record.declines.to_string(),
            dismiss::iso_utc(s.record.snoozed_until_ms),
            s.record.synopsis.clone(),
        ]);
    }
    print_table(&rows);
    ExitCode::SUCCESS
}

fn allow_cmd(root: &Path, pattern: &str, global: bool) -> ExitCode {
    let scope = if global {
        DecisionScope::Global
    } else {
        DecisionScope::Project
    };
    match dismiss::remove_record(root, pattern, scope) {
        Ok(true) => {
            println!(
                "removed `{pattern}` from the {} store; the suggestion can be offered again",
                scope.as_str()
            );
            ExitCode::SUCCESS
        }
        Ok(false) => {
            eprintln!(
                "no `{pattern}` record in the {} store (try `apb suggestions list`)",
                scope.as_str()
            );
            ExitCode::from(1)
        }
        Err(e) => {
            eprintln!("could not remove `{pattern}`: {e}");
            ExitCode::from(2)
        }
    }
}

fn reset_cmd(root: &Path, pattern: Option<&str>, all: bool) -> ExitCode {
    if pattern.is_none() && !all {
        eprintln!("name a pattern or pass --all");
        return ExitCode::from(2);
    }
    let outcome = match dismiss::reset_records(root, if all { None } else { pattern }) {
        Ok(o) => o,
        Err(e) => {
            eprintln!("could not reset: {e}");
            return ExitCode::from(2);
        }
    };
    for hard in &outcome.skipped_hard {
        println!("`{hard}` is a hard record; remove it with `apb suggestions allow {hard}`");
    }
    if outcome.reset.is_empty() {
        if outcome.skipped_hard.is_empty() {
            eprintln!("nothing to reset (try `apb suggestions list`)");
        }
        return ExitCode::from(1);
    }
    for pattern in &outcome.reset {
        println!("reset `{pattern}`: decline counter zeroed, snooze cleared");
    }
    ExitCode::SUCCESS
}
```

- [ ] Wire it into the CLI. In `crates/apb-cli/src/main.rs`: add `mod suggestions;` to the module list, `use crate::suggestions::{SuggestionsAction, suggestions_cmd};` to the imports, this variant to `enum Command` right after `Subscriptions`:

```rust
    /// Inspect and undo the suggestion decisions the agent recorded
    /// (spec 2026-07-29)
    Suggestions {
        #[command(subcommand)]
        action: SuggestionsAction,
    },
```

and this dispatch arm right after the `Subscriptions` arm:

```rust
        Some(Command::Suggestions { action }) => suggestions_cmd(&root, action),
```

- [ ] Run the tests: `cargo test -p apb-cli --test main suggestions_cli_test` prints `test result: ok. 3 passed`. Also re-run `cargo test -p apb-cli --test main connector_cli` to prove the shared `print_table` did not change the connector table output.

- [ ] Gate the task: `cargo fmt --all -- --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo metadata --format-version 1 >/dev/null && code-ranker check .`.

- [ ] Commit (after owner approval):

```
git add crates/apb-cli/src/suggestions.rs crates/apb-cli/src/main.rs crates/apb-cli/src/util.rs crates/apb-cli/src/connector.rs crates/apb-cli/tests/main.rs crates/apb-cli/tests/suite/suggestions_cli_test.rs
git commit --signoff -m "feat(cli): apb suggestions list, allow and reset

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
```

---

## Task 7: Server routes `GET` and `DELETE /api/suggestions`

**Files:**
- Create: `/Users/techmeat/www/projects/omniteamhq/agentic-playbooks/crates/apb-server/src/routes/suggestions.rs`
- Modify: `/Users/techmeat/www/projects/omniteamhq/agentic-playbooks/crates/apb-server/src/routes/mod.rs` (module list)
- Modify: `/Users/techmeat/www/projects/omniteamhq/agentic-playbooks/crates/apb-server/src/lib.rs` (two routes)
- Create: `/Users/techmeat/www/projects/omniteamhq/agentic-playbooks/crates/apb-server/tests/suite/suggestions_api_test.rs`
- Modify: `/Users/techmeat/www/projects/omniteamhq/agentic-playbooks/crates/apb-server/tests/main.rs` (register the new suite module)

**Interfaces:**
- Consumes: `apb_core::dismiss::{active, iso_utc, remove_record, DecisionScope}` (Task 1), `crate::state::{AppState, WorkspaceQuery, is_safe_id, resolve_root}`.
- Produces:
  - `pub(crate) async fn list_suggestions_handler(State(state): State<AppState>, Query(q): Query<WorkspaceQuery>) -> Response`
  - `pub(crate) async fn delete_suggestion_handler(State(state): State<AppState>, Path(pattern): Path<String>, Query(q): Query<SuggestionScopeQuery>) -> Response`
  - `pub(crate) struct SuggestionScopeQuery { pub(crate) workspace: Option<String>, pub(crate) scope: Option<String> }`

### Steps

- [ ] Write the failing route test. Create `crates/apb-server/tests/suite/suggestions_api_test.rs`:

```rust
//! GET and DELETE /api/suggestions. Mutates process env (APB_CONFIG_DIR), so
//! it takes `common::env_lock()` like the other env-mutating suites.

use apb_server::{AppState, build_router};
use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use tower::ServiceExt;

async fn send(app: axum::Router, req: Request<Body>) -> (StatusCode, serde_json::Value) {
    let res = app.oneshot(req).await.unwrap();
    let status = res.status();
    let bytes = res.into_body().collect().await.unwrap().to_bytes();
    let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null);
    (status, json)
}

fn seed(root: &std::path::Path, pattern: &str, synopsis: &str, kind: &str, scope: &str) {
    apb_core::dismiss::record_decision(
        root,
        apb_core::dismiss::DecisionInput {
            pattern: pattern.to_string(),
            synopsis: synopsis.to_string(),
            kind: apb_core::dismiss::DecisionKind::parse(kind).unwrap(),
            scope: apb_core::dismiss::DecisionScope::parse(scope).unwrap(),
            hard_ttl_days_override: None,
        },
    )
    .unwrap();
}

#[tokio::test]
async fn list_and_delete_suggestions() {
    let _guard = crate::common::env_lock().await;
    let cfg = tempfile::tempdir().unwrap();
    let proj = tempfile::tempdir().unwrap();
    unsafe {
        std::env::set_var("APB_CONFIG_DIR", cfg.path());
    }
    apb_core::registry::init_project(proj.path()).unwrap();
    seed(proj.path(), "code-review-run", "Review a file", "soft", "project");
    seed(proj.path(), "never-anywhere", "Never offer this", "hard", "global");
    let root = proj.path().to_path_buf();

    let app = build_router(AppState::new(root.clone()));
    let (status, json) = send(app, Request::get("/api/suggestions").body(Body::empty()).unwrap()).await;
    assert_eq!(status, StatusCode::OK);
    let rows = json["suggestions"].as_array().expect("suggestions array");
    assert_eq!(rows.len(), 2, "{json}");
    let soft = rows.iter().find(|r| r["pattern"] == "code-review-run").unwrap();
    assert_eq!(soft["kind"], "soft");
    assert_eq!(soft["scope"], "project");
    assert_eq!(soft["synopsis"], "Review a file");
    assert!(soft["snoozed_until"].as_str().unwrap().ends_with('Z'));

    // DELETE with the default (project) scope.
    let app = build_router(AppState::new(root.clone()));
    let (status, json) = send(
        app,
        Request::delete("/api/suggestions/code-review-run")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{json}");
    assert_eq!(json["removed"], true);

    // A second delete of the same record is a 404.
    let app = build_router(AppState::new(root.clone()));
    let (status, _json) = send(
        app,
        Request::delete("/api/suggestions/code-review-run")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    // The global record needs scope=global.
    let app = build_router(AppState::new(root.clone()));
    let (status, _json) = send(
        app,
        Request::delete("/api/suggestions/never-anywhere?scope=global")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    let app = build_router(AppState::new(root.clone()));
    let (status, json) = send(app, Request::get("/api/suggestions").body(Body::empty()).unwrap()).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json["suggestions"].as_array().unwrap().len(), 0, "{json}");

    unsafe {
        std::env::remove_var("APB_CONFIG_DIR");
    }
}

#[tokio::test]
async fn suggestions_reject_a_bad_workspace_or_pattern_or_scope() {
    let _guard = crate::common::env_lock().await;
    let cfg = tempfile::tempdir().unwrap();
    let proj = tempfile::tempdir().unwrap();
    unsafe {
        std::env::set_var("APB_CONFIG_DIR", cfg.path());
    }
    apb_core::registry::init_project(proj.path()).unwrap();
    let root = proj.path().to_path_buf();

    let app = build_router(AppState::new(root.clone()));
    let (status, _json) = send(
        app,
        Request::get("/api/suggestions?workspace=../escape")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "workspace id is validated");

    let app = build_router(AppState::new(root.clone()));
    let (status, _json) = send(
        app,
        Request::delete("/api/suggestions/..?scope=project")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "pattern is validated");

    let app = build_router(AppState::new(root.clone()));
    let (status, _json) = send(
        app,
        Request::delete("/api/suggestions/whatever?scope=everywhere")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "scope is validated");

    unsafe {
        std::env::remove_var("APB_CONFIG_DIR");
    }
}
```

- [ ] Register the module. In `crates/apb-server/tests/main.rs`, add after the `runs_api_test` entry:

```rust
#[path = "suite/suggestions_api_test.rs"]
mod suggestions_api_test;
```

- [ ] Run it and watch it fail: `cargo test -p apb-server --test main suggestions_api_test` fails with `404 Not Found` for `/api/suggestions` (the fallback static handler answers instead).

- [ ] Implement the routes. Create `crates/apb-server/src/routes/suggestions.rs`:

```rust
//! The suggestion-decision surface of the dashboard (spec
//! 2026-07-29-suggestion-decisions-design section "Dashboard"): read the
//! records that currently silence an offer in a workspace, and remove one.
//! Both handlers are thin wrappers over `apb_core::dismiss`, the same
//! functions `apb suggestions` uses.

use apb_core::dismiss::{self, DecisionScope};
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Json, Response};
use serde::Deserialize;

use crate::state::{AppState, WorkspaceQuery, is_safe_id, resolve_root};

/// `GET /api/suggestions?workspace=<id>`: the merged active records for the
/// workspace, project and global scope, each labelled with its scope.
pub(crate) async fn list_suggestions_handler(
    State(state): State<AppState>,
    Query(q): Query<WorkspaceQuery>,
) -> Response {
    let root = match resolve_root(&state, q.workspace.as_deref()) {
        Ok(r) => r,
        Err(res) => return res,
    };
    let view = dismiss::active(&root);
    let rows: Vec<serde_json::Value> = view
        .records
        .iter()
        .map(|s| {
            serde_json::json!({
                "pattern": s.record.pattern,
                "synopsis": s.record.synopsis,
                "kind": s.record.kind.as_str(),
                "scope": s.scope.as_str(),
                "declines": s.record.declines,
                "snoozed_until": dismiss::iso_utc(s.record.snoozed_until_ms),
            })
        })
        .collect();
    Json(serde_json::json!({
        "suggestions": rows,
        "diagnostics": view.diagnostics,
    }))
    .into_response()
}

/// Query for the delete route: the target workspace plus which store the
/// record lives in. An absent `scope` means project, matching the tool default.
#[derive(Deserialize, Default)]
pub(crate) struct SuggestionScopeQuery {
    pub(crate) workspace: Option<String>,
    pub(crate) scope: Option<String>,
}

/// `DELETE /api/suggestions/{pattern}?workspace=<id>&scope=project|global`:
/// same effect as `apb suggestions allow`, so offers resume immediately.
pub(crate) async fn delete_suggestion_handler(
    State(state): State<AppState>,
    Path(pattern): Path<String>,
    Query(q): Query<SuggestionScopeQuery>,
) -> Response {
    if !is_safe_id(&pattern) {
        return (StatusCode::BAD_REQUEST, "invalid pattern").into_response();
    }
    let scope = match q.scope.as_deref() {
        None | Some("") => DecisionScope::Project,
        Some(raw) => match DecisionScope::parse(raw) {
            Some(s) => s,
            None => return (StatusCode::BAD_REQUEST, "invalid scope").into_response(),
        },
    };
    let root = match resolve_root(&state, q.workspace.as_deref()) {
        Ok(r) => r,
        Err(res) => return res,
    };
    match dismiss::remove_record(&root, &pattern, scope) {
        Ok(true) => Json(serde_json::json!({ "removed": true, "pattern": pattern })).into_response(),
        Ok(false) => (StatusCode::NOT_FOUND, "no such suggestion record").into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}
```

Add `pub mod suggestions;` to `crates/apb-server/src/routes/mod.rs` (alphabetically, after `runs`), and register the routes in `crates/apb-server/src/lib.rs` right after the `/api/skills` route:

```rust
        .route(
            "/api/suggestions",
            get(routes::suggestions::list_suggestions_handler),
        )
        .route(
            "/api/suggestions/{pattern}",
            axum::routing::delete(routes::suggestions::delete_suggestion_handler),
        )
```

- [ ] Run the tests: `cargo test -p apb-server --test main suggestions_api_test` prints `test result: ok. 2 passed`.

- [ ] Gate the task: `cargo fmt --all -- --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo metadata --format-version 1 >/dev/null && code-ranker check .`.

- [ ] Commit (after owner approval):

```
git add crates/apb-server/src/routes/suggestions.rs crates/apb-server/src/routes/mod.rs crates/apb-server/src/lib.rs crates/apb-server/tests/main.rs crates/apb-server/tests/suite/suggestions_api_test.rs
git commit --signoff -m "feat(server): GET and DELETE /api/suggestions

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
```

---

## Task 8: Dashboard section on the playbooks page

**Files:**
- Create: `/Users/techmeat/www/projects/omniteamhq/agentic-playbooks/web/src/lib/suggestions.ts`
- Create: `/Users/techmeat/www/projects/omniteamhq/agentic-playbooks/web/src/lib/suggestions.test.ts`
- Modify: `/Users/techmeat/www/projects/omniteamhq/agentic-playbooks/web/src/lib/api/core.ts` (two endpoint functions)
- Create: `/Users/techmeat/www/projects/omniteamhq/agentic-playbooks/web/src/lib/components/SuggestionsSection.svelte`
- Modify: `/Users/techmeat/www/projects/omniteamhq/agentic-playbooks/web/src/pages/PlaybookList.svelte` (render the section)

**Interfaces:**
- Consumes: `GET /api/suggestions?workspace=<id>` and `DELETE /api/suggestions/{pattern}?workspace=<id>&scope=<scope>` (Task 7), `fetchProjects` from `web/src/lib/api/core.ts`, `getJson`/`requestJson`/`qs` from `web/src/lib/api/http.ts`.
- Produces:
  - `export interface SuggestionRecord { pattern: string; synopsis: string; kind: 'soft' | 'hard'; scope: 'project' | 'global'; declines: number; snoozed_until: string }`
  - `export interface SuggestionRow extends SuggestionRecord { workspace_id: string; project: string }`
  - `export function kindLabel(kind: string): string`
  - `export function untilLabel(snoozedUntilIso: string, nowMs: number): string`
  - `export function sortSuggestions(rows: SuggestionRow[]): SuggestionRow[]`
  - `export const fetchSuggestions: (workspace?: string) => Promise<SuggestionRecord[]>` and `export const deleteSuggestion: (pattern: string, workspace: string, scope: string) => Promise<{ removed: boolean }>` in `web/src/lib/api/core.ts`

### Steps

- [ ] Write the failing vitest. Create `web/src/lib/suggestions.test.ts`:

```ts
import { describe, expect, it } from 'vitest'
import { kindLabel, sortSuggestions, untilLabel, type SuggestionRow } from './suggestions'

const row = (over: Partial<SuggestionRow>): SuggestionRow => ({
  pattern: 'p',
  synopsis: 's',
  kind: 'soft',
  scope: 'project',
  declines: 1,
  snoozed_until: '2026-08-05T10:00:00Z',
  workspace_id: 'w',
  project: 'proj',
  ...over,
})

describe('kindLabel', () => {
  it('names a hard record as never again', () => {
    expect(kindLabel('hard')).toBe('never again')
  })
  it('names a soft record as snoozed', () => {
    expect(kindLabel('soft')).toBe('snoozed')
  })
  it('passes an unknown kind through rather than inventing one', () => {
    expect(kindLabel('weird')).toBe('weird')
  })
})

describe('untilLabel', () => {
  const now = Date.parse('2026-07-29T10:00:00Z')
  it('renders a multi-day distance', () => {
    expect(untilLabel('2026-08-05T10:00:00Z', now)).toBe('in 7 days')
  })
  it('renders a single day in the singular', () => {
    expect(untilLabel('2026-07-30T10:00:00Z', now)).toBe('in 1 day')
  })
  it('renders less than a day as today', () => {
    expect(untilLabel('2026-07-29T18:00:00Z', now)).toBe('today')
  })
  it('renders a past date as expired', () => {
    expect(untilLabel('2026-07-01T10:00:00Z', now)).toBe('expired')
  })
  it('renders an unparseable date as a dash', () => {
    expect(untilLabel('not a date', now)).toBe('-')
  })
})

describe('sortSuggestions', () => {
  it('puts hard records first, then sorts by pattern', () => {
    const rows = [
      row({ pattern: 'b-soft', kind: 'soft' }),
      row({ pattern: 'z-hard', kind: 'hard' }),
      row({ pattern: 'a-soft', kind: 'soft' }),
      row({ pattern: 'a-hard', kind: 'hard' }),
    ]
    expect(sortSuggestions(rows).map((r) => r.pattern)).toEqual([
      'a-hard',
      'z-hard',
      'a-soft',
      'b-soft',
    ])
  })
  it('does not mutate its input', () => {
    const rows = [row({ pattern: 'b', kind: 'soft' }), row({ pattern: 'a', kind: 'hard' })]
    sortSuggestions(rows)
    expect(rows[0].pattern).toBe('b')
  })
})
```

- [ ] Run it and watch it fail: `cd web && bun run test suggestions` reports `Failed to resolve import "./suggestions"`.

- [ ] Implement the module. Create `web/src/lib/suggestions.ts`:

```ts
// The suggestion-decision records the dashboard shows on the playbooks page:
// what the agent recorded when the user declined a save-as-playbook offer, and
// for how long that silences the offer. Pure formatting and ordering here; the
// fetching lives in `api/core.ts` and the rendering in
// `components/SuggestionsSection.svelte`.

export interface SuggestionRecord {
  pattern: string
  synopsis: string
  kind: 'soft' | 'hard'
  scope: 'project' | 'global'
  declines: number
  // RFC-3339 UTC, as the server renders it.
  snoozed_until: string
}

// A record plus the project it was read for: the playbooks page aggregates
// every registered workspace, so a row has to say which one it belongs to.
export interface SuggestionRow extends SuggestionRecord {
  workspace_id: string
  project: string
}

const DAY_MS = 24 * 60 * 60 * 1000

// Human label for the kind. An unknown value is passed through verbatim rather
// than mapped to one of the two known labels, so a future kind is visible
// instead of silently mislabelled.
export function kindLabel(kind: string): string {
  if (kind === 'hard') return 'never again'
  if (kind === 'soft') return 'snoozed'
  return kind
}

// How much silence is left. Whole days, because the backoff schedule is in
// days; anything under a day reads as "today" and a past date as "expired"
// (a record can be listed and then expire while the page is open).
export function untilLabel(snoozedUntilIso: string, nowMs: number): string {
  const until = Date.parse(snoozedUntilIso)
  if (Number.isNaN(until)) return '-'
  const delta = until - nowMs
  if (delta <= 0) return 'expired'
  const days = Math.floor(delta / DAY_MS)
  if (days === 0) return 'today'
  return days === 1 ? 'in 1 day' : `in ${days} days`
}

// Hard records first (the permanent decisions the user is most likely looking
// for), then by pattern. Returns a new array.
export function sortSuggestions(rows: SuggestionRow[]): SuggestionRow[] {
  return [...rows].sort((a, b) => {
    if (a.kind !== b.kind) return a.kind === 'hard' ? -1 : 1
    return a.pattern.localeCompare(b.pattern)
  })
}
```

- [ ] Run the test: `cd web && bun run test suggestions` prints all 11 assertions passing.

- [ ] Add the two endpoints. Append to `web/src/lib/api/core.ts`:

```ts
import type { SuggestionRecord } from '../suggestions'

export const fetchSuggestions = (workspace = '') =>
  getJson<{ suggestions: SuggestionRecord[] }>(`/api/suggestions${qs({ workspace })}`).then(
    (r) => r.suggestions,
  )

export const deleteSuggestion = (pattern: string, workspace = '', scope = 'project') =>
  requestJson<{ removed: boolean }>(
    `/api/suggestions/${encodeURIComponent(pattern)}${qs({ workspace, scope })}`,
    { method: 'DELETE' },
  )
```

(Place the `import type` line with the other imports at the top of the file rather than in the middle.)

- [ ] Create the section component `web/src/lib/components/SuggestionsSection.svelte`:

```svelte
<script lang="ts">
  import { deleteSuggestion, fetchProjects, fetchSuggestions } from '../api'
  import { sortSuggestions, kindLabel, untilLabel, type SuggestionRow } from '../suggestions'
  import { Badge } from '$lib/components/ui/badge'
  import { Button } from '$lib/components/ui/button'
  import * as Card from '$lib/components/ui/card'
  import { toast } from 'svelte-sonner'
  import BellOff from '@lucide/svelte/icons/bell-off'
  import Trash2 from '@lucide/svelte/icons/trash-2'

  let rows = $state<SuggestionRow[]>([])
  let loaded = $state(false)
  let removing = $state<string | null>(null)
  const now = Date.now()

  const key = (r: SuggestionRow) => `${r.workspace_id}/${r.scope}/${r.pattern}`

  async function load() {
    try {
      const projects = await fetchProjects()
      const perProject = await Promise.all(
        projects.map(async (p) => {
          const records = await fetchSuggestions(p.workspace_id).catch(() => [])
          return records.map((r) => ({ ...r, workspace_id: p.workspace_id, project: p.name }))
        }),
      )
      rows = sortSuggestions(perProject.flat())
    } catch (e) {
      toast.error('Failed to load suggestion decisions', { description: String(e) })
    } finally {
      loaded = true
    }
  }

  $effect(() => {
    load()
  })

  async function remove(r: SuggestionRow) {
    removing = key(r)
    try {
      await deleteSuggestion(r.pattern, r.workspace_id, r.scope)
      await load()
      toast.success(`"${r.pattern}" can be suggested again`)
    } catch (e) {
      toast.error('Remove failed', { description: String(e) })
    } finally {
      removing = null
    }
  }
</script>

{#if loaded && rows.length > 0}
  <section class="mb-8">
    <h2 class="mb-3 flex items-center gap-2 text-xs font-semibold uppercase tracking-wider text-muted-foreground">
      <BellOff class="size-3" />
      Silenced suggestions
    </h2>
    <div class="flex flex-col gap-2">
      {#each rows as r (key(r))}
        <Card.Root>
          <Card.Header>
            <div class="flex flex-wrap items-center gap-2">
              <Card.Title class="font-mono text-sm">{r.pattern}</Card.Title>
              <Badge variant="outline">{kindLabel(r.kind)}</Badge>
              <Badge variant="outline">{r.scope}</Badge>
              <span class="text-xs text-muted-foreground">{untilLabel(r.snoozed_until, now)}</span>
            </div>
            <Card.Description>{r.synopsis || 'no synopsis recorded'}</Card.Description>
            <Card.Action>
              <Button
                variant="ghost"
                size="sm"
                class="max-sm:px-2 text-muted-foreground hover:text-destructive"
                onclick={() => remove(r)}
                disabled={removing === key(r)}
                title="Allow this suggestion again"
              >
                <Trash2 data-icon="inline-start" />
                <span class="max-sm:sr-only">Remove</span>
              </Button>
            </Card.Action>
          </Card.Header>
        </Card.Root>
      {/each}
    </div>
  </section>
{/if}
```

- [ ] Render it on the playbooks page. In `web/src/pages/PlaybookList.svelte`, add `import SuggestionsSection from '$lib/components/SuggestionsSection.svelte'` to the imports, and place `<SuggestionsSection />` inside `<PageScroll>` immediately after the opening `<div class="mx-auto w-full max-w-4xl px-4 py-6">` so the section shows above the playbook groups and stays visible on the empty-state path.

- [ ] Run the web gates: `cd web && bun run test` (whole suite green) and `cd web && bun run check` (svelte-check and tsc clean).

- [ ] Commit (after owner approval):

```
git add web/src/lib/suggestions.ts web/src/lib/suggestions.test.ts web/src/lib/api/core.ts web/src/lib/components/SuggestionsSection.svelte web/src/pages/PlaybookList.svelte
git commit --signoff -m "feat(web): silenced-suggestions section on the playbooks page

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
```

---

## Task 9: Documentation

**Files:**
- Modify: `/Users/techmeat/www/projects/omniteamhq/agentic-playbooks/docs/MCP.md` (`suggestion_dismiss` row, catalog payload sentence)
- Modify: `/Users/techmeat/www/projects/omniteamhq/agentic-playbooks/docs/HOST-INTEGRATION.md` (the standing-block scope sentence)
- Modify: `/Users/techmeat/www/projects/omniteamhq/agentic-playbooks/docs/superpowers/specs/2026-07-11-agent-transparent-workflows-design.md` (section 8.2 pointer)
- Modify: `/Users/techmeat/www/projects/omniteamhq/agentic-playbooks/docs/superpowers/specs/2026-07-29-suggestion-decisions-design.md` (status line points at this plan)

**Interfaces:**
- Consumes: the shipped behavior from Tasks 1 through 8. No code.
- Produces: documentation only. Every file is markdown with no hard line wraps (one paragraph per line).

### Steps

- [ ] Update `docs/MCP.md`. Replace the `suggestion_dismiss` row of the mutations table with:

```markdown
| `suggestion_dismiss` | Record the user's decline of a save-as-playbook suggestion: `kind` `soft` (a not-now decline whose silence escalates along the backoff schedule) or `hard` (an explicit never-again, the default so an old-style call is unchanged), a one-sentence `synopsis` of the action, and `scope` `project` (default) or `global`. Returns the stored record with the server-computed `snoozed_until` |
```

and add this paragraph right after the read-tools table (the paragraph that documents the optional `workspace` argument):

```markdown
`playbook_catalog` returns both `dismissed_patterns` (the slug list, unchanged) and `suppressed_suggestions`: the active suggestion-decision records for the current project, merged from the project store `.apb/suggestions.json` and the global `<config-dir>/suggestions.json`, each with `pattern`, `synopsis`, `kind`, `scope`, `declines` and `snoozed_until`. Matching a candidate action against those records is done by the meaning of the synopsis, on the agent side; the server does no language processing. Both fields fold into `catalog_revision`, so an `unchanged: true` response stays correct after any dismiss write. Timing defaults are `soft_backoff_days: [1, 7, 30, 90]` and `hard_ttl_days: 90`, overridable per key by a `suggestions:` section in the global `config.yaml` and in the project `.apb/config.yaml` (project wins). `apb suggestions list|allow|reset` and the dashboard's silenced-suggestions section manage the same records.
```

- [ ] Update `docs/HOST-INTEGRATION.md`. Replace the last paragraph of the "Standing instruction in CLAUDE.md / AGENTS.md" section with:

```markdown
The block intentionally duplicates the proactive duties from tier 0 and nothing else: the catalog check before acting, the offer-to-save after, the semantic check against `suppressed_suggestions` before offering, and how a decline is recorded with `suggestion_dismiss` (kind soft by default, kind hard only for an explicit never-again). Run policy, gates and authoring rules stay in the server instructions, so the memory-file section does not go stale when those evolve.
```

- [ ] Add the supersession pointer. In `docs/superpowers/specs/2026-07-11-agent-transparent-workflows-design.md`, append one line at the end of section 8.2 (right before the `### 8.3` heading), leaving the historical text untouched:

```markdown
Superseded by `docs/superpowers/specs/2026-07-29-suggestion-decisions-design.md`: the dismiss store described here became a two-scope suggestion-decision store with soft and hard declines, an escalating backoff, a stored synopsis for semantic matching, and user-facing management surfaces.
```

- [ ] Point the new spec at this plan. In `docs/superpowers/specs/2026-07-29-suggestion-decisions-design.md`, extend the status line to:

```markdown
Status: approved design, 2026-07-29. Supersedes the dismiss portion of spec 8.2 in `2026-07-11-agent-transparent-workflows-design.md`. Implementation plan: `docs/superpowers/plans/2026-07-29-suggestion-decisions.md`.
```

- [ ] Verify the prose conventions with a codepoint grep (the em-dash is named by escape so this plan and the check itself stay free of the character): `grep -nP '\x{2014}' docs/MCP.md docs/HOST-INTEGRATION.md docs/superpowers/specs/2026-07-11-agent-transparent-workflows-design.md docs/superpowers/specs/2026-07-29-suggestion-decisions-design.md` returns nothing, and `grep -n '!' ` over the same four files shows no added sentence using an exclamation mark.

- [ ] Run the full workspace gate one last time: `cargo test --workspace`, `cargo fmt --all -- --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo metadata --format-version 1 >/dev/null && code-ranker check .`, `cd web && bun run test && bun run check`.

- [ ] Commit (after owner approval):

```
git add docs/MCP.md docs/HOST-INTEGRATION.md docs/superpowers/specs/2026-07-11-agent-transparent-workflows-design.md docs/superpowers/specs/2026-07-29-suggestion-decisions-design.md
git commit --signoff -m "docs: suggestion decisions in the MCP, host-integration and spec pointers

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
```

---

## Manual end-to-end scenario (after Task 9)

Not automatable in CI, kept here because the spec's testing section asks for it. Run it in a scratch project with `APB_CONFIG_DIR` pointed at a throwaway directory, using the same harness as the July 2026 offer experiment.

- [ ] Perform a multi-step action in a session so the offer fires, then decline with a plain "not now". Confirm the agent called `suggestion_dismiss` with kind soft and a synopsis, and that it told the user how long the silence lasts.
- [ ] Start a new session, ask for the same action in different words, and confirm no offer is made (the record covers it by synopsis meaning, not by slug).
- [ ] Run `apb suggestions list` and confirm the record shows scope `project`, kind `soft`, `declines: 1` and a snooze one day out.
- [ ] Run `apb suggestions reset <pattern>` and confirm a new session offers again. Then decline twice and confirm `declines: 2` with a seven-day snooze, which is the escalation.
- [ ] Open the dashboard's playbooks page, confirm the silenced-suggestions section lists the record, remove it there, and confirm `apb suggestions list` no longer shows it.

---

## Spec coverage

Every section of `docs/superpowers/specs/2026-07-29-suggestion-decisions-design.md` and where this plan covers it.

| Spec section | Requirement | Task |
| --- | --- | --- |
| Problem, point 1 (byte-exact slug matching) | `synopsis` stored per record; matching moved to the agent by meaning | Task 1 (field), Task 4 (`suppressed_suggestions`), Task 5 (instruction wording) |
| Problem, point 2 (soft decline unrecorded) | `kind: soft` with a `declines` counter and an escalating snooze | Task 1, Task 2, Task 3 |
| Problem, point 3 (no visibility or control) | `apb suggestions list/allow/reset`, dashboard section, API | Task 6, Task 7, Task 8 |
| Problem, point 4 (one global TTL, global-only store) | named constants plus config override, two scopes | Task 1 (scopes), Task 2 (constants and config) |
| Decisions, semantic matching is the model's job | server stores the synopsis and does no language processing | Task 1, Task 4, Task 5 |
| Decisions, soft declines escalate | `next_snooze_ms` walks `SOFT_BACKOFF_DAYS` | Task 2 |
| Decisions, timing knobs with config override | `SuggestionSettings`, `SuggestionTiming`, `timing` | Task 2 |
| Decisions, scoped records | `DecisionScope`, two stores, project default | Task 1, Task 3 |
| Decisions, management surfaces over the same core functions | CLI and server both call `apb_core::dismiss` | Task 6, Task 7 |
| Store, record shape (`pattern`, `synopsis`, `kind`, `declines`, `snoozed_until`, `updated_at`) | `SuggestionRecord`; timestamps persist as epoch ms and render via `iso_utc` | Task 1 |
| Store, schema version 2 | `SCHEMA_VERSION = 2`, written on every write | Task 1 |
| Store, two locations merged like connector config | `store_dir` plus `merge_scopes` | Task 1 |
| Store, stricter-wins conflict rule (hard beats soft, later snooze beats earlier) | `is_stricter` | Task 1 |
| Store, prune on read | `is_retained` in `load_scope`, with the soft-retention rule | Task 1 |
| Store, reads never fail plus a diagnostic | `read_store` returns a diagnostic string, `SuggestionView.diagnostics` | Task 1 |
| Store, writes through `apb_core::fsutil` (atomic, 0600, dir lock) | `write_store` plus `lock_dir` in every mutating function | Task 1 |
| Store, v1 to v2 migration (hard, expiry preserved, empty synopsis, old file removed last) | `migrate_legacy` | Task 1 |
| Backoff, named constants `SOFT_BACKOFF_DAYS` and `HARD_TTL_DAYS` | both public in `dismiss` | Task 1 (declaration), Task 2 (use) |
| Backoff, soft arithmetic with the schedule tail | `next_snooze_ms` with `min(declines - 1, len - 1)` | Task 2 |
| Backoff, hard arithmetic | `next_snooze_ms` for `DecisionKind::Hard` | Task 2 |
| Config, `suggestions:` section in global and project config, project over global | `GlobalConfig.suggestions`, `project_suggestion_settings`, `timing` | Task 2 |
| Config, empty arrays are a validation error, values are positive day integers | `SuggestionSettings::validate` | Task 2 |
| Config, untouched section keeps the defaults | both keys `Option`, defaults from the constants | Task 2 |
| MCP, three new args with `#[serde(default)]`, defaults hard and project | `SuggestionDismissArgs`, `DismissRequest` | Task 3 |
| MCP, synopsis strongly recommended in the tool description, capture secret hygiene | tool description plus the `secret_like` scan on the synopsis | Task 3 |
| MCP, response reports the stored record including `snoozed_until` | `suggestion_dismiss` payload | Task 3 |
| MCP, catalog keeps `dismissed_patterns` and adds `suppressed_suggestions` | `catalog::build` | Task 4 |
| MCP, revision folds the new field | `compute_revision` with `d|` and `s|` lines | Task 4 |
| Instructions, matching sentence in TIER0 and the standing block | new texts | Task 5 |
| Instructions, soft-decline sentence in TIER0 and the standing block | new texts | Task 5 |
| Instructions, TIER0 at or under 1950 bytes, re-verified | new TIER0 is 1935 bytes, pinned by `tier0_fits_the_host_budget` | Task 5 |
| Instructions, no extra scope question | "Never ask about scope" in TIER0, "Do not ask an extra question about scope" in the block | Task 5 |
| CLI, `apb suggestions list` with scope, kind, declines, until, synopsis, and `--json` | `list_cmd` | Task 6 |
| CLI, `apb suggestions allow <pattern>` with `--global` | `allow_cmd` | Task 6 |
| CLI, `apb suggestions reset <pattern>` with `--all`, hard records untouched | `reset_cmd` plus `reset_records` | Task 2 (core), Task 6 (CLI) |
| Dashboard, `GET /api/suggestions?workspace=<id>` | `list_suggestions_handler` | Task 7 |
| Dashboard, `DELETE /api/suggestions/{pattern}?workspace=&scope=` | `delete_suggestion_handler` | Task 7 |
| Dashboard, one module per resource in `routes/`, `is_safe_id` validation | `routes/suggestions.rs`, `is_safe_id` on the pattern, `resolve_root` on the workspace | Task 7 |
| Dashboard, compact web section with pattern, synopsis, kind, until-date, remove action | `SuggestionsSection.svelte` plus `suggestions.ts` | Task 8 |
| Agent behavior summary (offer, accept, soft decline, hard decline, pre-offer check) | encoded in the two instruction texts and enforced by the store defaults | Task 3, Task 5 |
| Out of scope (session ledger, embeddings, analytics) | nothing in this plan adds any of them; the store has no session identity and no text analysis | all tasks, by omission |
| Testing, core unit tests (migration, corrupt v1, scope merge, backoff with tail and overrides, prune) | `dismiss::tests` and `config_test.rs` | Task 1, Task 2 |
| Testing, MCP tests (old-style call, new-field call, catalog fields, revision moves) | `capture_tools_test.rs`, `catalog_tools_test.rs` | Task 3, Task 4 |
| Testing, CLI integration tests including the global flag | `suggestions_cli_test.rs` | Task 6 |
| Testing, server route tests with workspace validation | `suggestions_api_test.rs` | Task 7 |
| Testing, manual end-to-end sandbox scenario | the manual scenario checklist above | after Task 9 |

## Resolved ambiguities

Three points where the spec left room for interpretation, and the choice this plan makes.

1. **Timestamp representation.** The spec's JSON example shows `snoozed_until` and `updated_at` as RFC-3339 strings, but apb has no date dependency and every other persisted timestamp in the codebase is epoch milliseconds (`trust.json`, `projects.json`, run events, all from `apb_core::clock`). Resolution: persist `snoozed_until_ms` and `updated_at_ms` as epoch milliseconds and render the spec's RFC-3339 shape at every edge (`iso_utc`), so the MCP response, the CLI, the HTTP API and the dashboard all show `snoozed_until: "2026-08-05T10:00:00Z"` while the file needs no date parser and no new crate.
2. **What "pruned on read" means for a soft record.** Pruning a soft record the moment its snooze ends would discard `declines` and reset the backoff to one day, which contradicts "each soft decline on the same suggestion pushes the next offer further out". Resolution: a record stops suppressing at `snoozed_until` (that is what `active` returns), but a soft record stays on disk for `SOFT_RETAIN_DAYS = 365` counted from the later of its snooze end and its last write, so its counter can escalate the next decline and a record whose snooze was cleared by `reset` keeps its synopsis; a hard record is pruned at its expiry, exactly as in v1.
3. **The legacy `ttl_days` argument of `suggestion_dismiss`.** The spec lists three new args and does not mention the existing `ttl_days`. Removing it would break an old-style call that passes it, which the spec explicitly wants to keep working. Resolution: keep `ttl_days` as an optional hard-TTL override (`hard_ttl_days_override`), honored only for a hard dismissal, which is precisely its v1 meaning, and document it as legacy in the arg doc comment.
