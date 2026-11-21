//! The inbound event store (spec 2026-08-16-webhook-ingest-design, "Inbox
//! store"): one append-only log per connector and account, under
//! `<config_dir>/connector-inbox/<connector>/<account>/`.
//!
//! Machine-scoped on purpose. Deliveries arrive whether or not a run is
//! executing, so binding them to a run id (the way the run-hook endpoint
//! does) would drop everything that arrives between runs.
//!
//! Three files per account, all 0600:
//!   * `events.jsonl` - one `InboxEvent` per line, ordered by `seq`.
//!   * `dedupe.idx`   - recently seen provider ids, time-indexed, one
//!     `<received_at_ms> <digest_hex>` per line. The id is stored as a
//!     fixed-size SHA-256 digest, never raw, so a provider-controlled scalar
//!     can carry neither a newline (which would corrupt this newline-delimited
//!     file) nor an unbounded length (which would make every append rewrite
//!     megabytes). Entries are evicted by age against [`DEDUPE_RETENTION_MS`],
//!     with [`DEDUPE_MAX_ENTRIES`] as a size safety cap.
//!   * `cursors.yaml` - `consumer -> last acked seq`.
//!
//! Every mutation happens under `fsutil::lock_dir` on the account directory
//! and `seq` is derived inside that lock, so two concurrent deliveries can
//! never be handed the same number. The run-signal channel's older
//! read-count-then-append shape is fixed to match in Task 2.
//!
//! Bodies stored here are authored by whoever can reach the ingest endpoint.
//! Nothing in this module logs one, and no caller may put one in a run's
//! event log; an inbox wait journals the values of the fields its playbook
//! declares, scrubbed, never a body.

use std::collections::BTreeMap;
use std::fs::OpenOptions;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::fsutil::{atomic_write_private, lock_dir};

/// Directory under the global config dir holding every account inbox.
pub const INBOX_ROOT: &str = "connector-inbox";
/// The append-only event log inside one account directory.
pub const EVENTS_FILE: &str = "events.jsonl";
/// The rolling provider-id index consulted before an append.
pub const DEDUPE_FILE: &str = "dedupe.idx";
/// The named consumer cursors.
pub const CURSORS_FILE: &str = "cursors.yaml";
/// The persisted dropped-delivery counter: a plain decimal integer, no
/// newline required. Deliberately not part of `events.jsonl` or any other
/// structured file - it is a single number the ingest handler bumps once per
/// drop, and a doctor or dashboard reads back across process boundaries, so
/// the in-process `IngestState` counter (which does not survive a restart or
/// cross a second `apb dashboard`/`apb ingest` process) is never the
/// operator-visible source of truth.
pub const DROPPED_FILE: &str = "dropped.count";
/// Lock file serializing every read-modify-write on one account directory.
const INBOX_LOCK: &str = "inbox.lock";
/// How long a provider id stays in the dedupe index. Retention is time-based,
/// not count-based: the old count window forgot an id after a fixed number of
/// later deliveries with no time bound, so a captured signed delivery could be
/// replayed once enough traffic had rolled it out. Sized to comfortably cover
/// a provider's retry-and-signature-validity horizon (Meta retries a webhook
/// for hours), so a replay inside that horizon is still recognized as a
/// duplicate and refused.
pub const DEDUPE_RETENTION_MS: u64 = 24 * 60 * 60 * 1000;
/// Size safety cap on dedupe-index entries, independent of age: it bounds the
/// file and the per-append rewrite even under a delivery flood that stays
/// inside [`DEDUPE_RETENTION_MS`]. Age is the primary eviction; this only
/// caps pathological volume, keeping the file a cheap linear scan and each
/// append's rewrite bounded. An account whose real traffic exceeds this in a
/// day sheds its oldest digests early, shortening their replay window rather
/// than growing the file without bound.
pub const DEDUPE_MAX_ENTRIES: usize = 10_000;

/// One stored delivery. `body` is the payload exactly as parsed from the
/// request; `provider_id` is the dedupe identity the connector's webhook
/// block selected; `received_at` is milliseconds since the epoch from the
/// single wall-clock source.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct InboxEvent {
    pub seq: u64,
    pub received_at: u64,
    pub provider_id: String,
    /// The event's type as the connector's webhook block names it, cleaned
    /// (`webhook::event_type`); absent for an untyped delivery and for every
    /// event stored before this field existed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub event_type: Option<String>,
    pub body: Value,
}

/// The per-account retention envelope. Enforced opportunistically on append,
/// under the same lock as the append itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Retention {
    pub max_bytes: u64,
    pub max_age_ms: u64,
}

impl Default for Retention {
    fn default() -> Self {
        Retention {
            max_bytes: 50 * 1024 * 1024,
            max_age_ms: 30 * 24 * 60 * 60 * 1000,
        }
    }
}

/// What an append did. A duplicate is answered 200 by the ingest handler and
/// stored nowhere: providers retry aggressively, so idempotency is a
/// functional requirement and not only replay protection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Appended {
    Stored(u64),
    Duplicate,
}

/// The inclusive range of sequence numbers a read found lost: events that
/// were stored once and dropped by retention before this consumer ever read
/// or acknowledged them. `from` is the first seq after the consumer's cursor,
/// `through` the seq just before the oldest surviving event.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InboxGap {
    pub from: u64,
    pub through: u64,
}

/// What a filtered read found.
#[derive(Debug, Clone, PartialEq)]
pub struct FilteredRead {
    pub events: Vec<InboxEvent>,
    /// The consumer's cursor after the read (moved past a leading run of
    /// skipped events when `ack_skipped`).
    pub cursor: u64,
    /// The gap, computed from the cursor before anything moved.
    pub gap: Option<InboxGap>,
    /// Pending events the read did not return because they did not match.
    pub skipped: u64,
}

/// Counts for the doctor and the dashboard panel. Carries no body.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Depth {
    pub pending: u64,
    pub total: u64,
    pub cursor: u64,
    pub last_received_at: Option<u64>,
    /// Deliveries this account's accept cap has dropped, from the persisted
    /// counter (`note_dropped`/`dropped_count`), not the in-process one: the
    /// doctor and the dashboard route both read `depth` for their per-account
    /// row, and this must be visible from whichever process asks, not only
    /// the one that happened to reject the delivery.
    pub dropped: u64,
}

#[derive(Debug, thiserror::Error)]
pub enum InboxError {
    #[error("no config directory: set HOME or APB_CONFIG_DIR")]
    NoConfigDir,
    #[error("invalid inbox name `{0}`: {1}")]
    Name(String, String),
    #[error("inbox `{0}`: {1}")]
    Io(String, String),
    #[error("inbox `{0}` is corrupt: {1}")]
    Corrupt(String, String),
}

/// `<config_dir>/connector-inbox`. `None` in a config-less environment,
/// mirroring `crate::config::config_dir`.
pub fn inbox_root() -> Option<PathBuf> {
    crate::config::config_dir().map(|dir| dir.join(INBOX_ROOT))
}

/// Account directory names that exist for `connector` under `base`, sorted.
/// A non-directory entry or an entry whose name is not a valid slug is
/// skipped, matching how the connector store lists installed connectors.
pub fn list_accounts(base: &Path, connector: &str) -> Vec<String> {
    if crate::profile::validate_profile_name(connector).is_err() {
        return Vec::new();
    }
    let Ok(entries) = std::fs::read_dir(base.join(connector)) else {
        return Vec::new();
    };
    let mut out: Vec<String> = entries
        .filter_map(Result::ok)
        .filter(|e| e.file_type().map(|t| t.is_dir()).unwrap_or(false))
        .map(|e| e.file_name().to_string_lossy().to_string())
        .filter(|n| crate::profile::validate_profile_name(n).is_ok())
        .collect();
    out.sort();
    out
}

/// The consumer cursor map, as stored.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct Cursors {
    consumers: BTreeMap<String, u64>,
}

/// One account's inbox. Cheap to construct: it holds a path and opens
/// nothing until a method is called.
#[derive(Debug)]
pub struct Inbox {
    dir: PathBuf,
}

impl Inbox {
    /// An inbox under an explicit base directory. Both segments must be
    /// valid connector/account slugs, so no delivery path can name anything
    /// but a directory one level down.
    pub fn at(base: &Path, connector: &str, account: &str) -> Result<Self, InboxError> {
        for segment in [connector, account] {
            crate::profile::validate_profile_name(segment)
                .map_err(|e| InboxError::Name(segment.to_string(), e))?;
        }
        Ok(Inbox {
            dir: base.join(connector).join(account),
        })
    }

    /// An inbox under the standard `<config_dir>/connector-inbox` root.
    pub fn open(connector: &str, account: &str) -> Result<Self, InboxError> {
        let base = inbox_root().ok_or(InboxError::NoConfigDir)?;
        Self::at(&base, connector, account)
    }

    /// The account directory. Public so tests and the dashboard route can
    /// name the files without duplicating the layout.
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// Whether anything was ever appended here.
    pub fn exists(&self) -> bool {
        self.dir.join(EVENTS_FILE).is_file()
    }

    /// Appends one delivery with the default retention envelope.
    pub fn append(&self, provider_id: &str, body: &Value) -> Result<Appended, InboxError> {
        self.append_with(provider_id, body, &Retention::default())
    }

    /// Whether `provider_id` is already in the dedupe index, read without
    /// taking the directory lock (like `cursor` and `depth`).
    ///
    /// Advisory by construction: `append_with` re-checks under the lock and
    /// remains the authority, so a racing concurrent append can still turn a
    /// `false` here into an `Appended::Duplicate` there. It exists so a caller
    /// can decide something *before* the append without changing what gets
    /// stored - the ingest listener uses it to keep a provider's retry storm
    /// on one message from spending the per-account accept budget that new
    /// deliveries need. The comparison is against the stored digest, so the
    /// caller passes the raw provider id and this hashes it the same way the
    /// authoritative append does.
    pub fn is_duplicate(&self, provider_id: &str) -> Result<bool, InboxError> {
        self.is_duplicate_at(provider_id, crate::clock::now_ms_u64(), DEDUPE_RETENTION_MS)
    }

    /// [`is_duplicate`](Self::is_duplicate) with the clock and age window made
    /// explicit, the way `append_bounded` already exposes them. The shared
    /// [`dedupe_hit`] helper it uses is the same one `append_bounded` uses, so
    /// the advisory check and the authority never disagree on which entries are
    /// still live: an entry past [`DEDUPE_RETENTION_MS`] is not a duplicate in
    /// either, so a genuinely fresh redelivery long after a quiet period is
    /// stored rather than silently answered as an already-seen 200.
    pub fn is_duplicate_at(
        &self,
        provider_id: &str,
        now: u64,
        dedupe_retention_ms: u64,
    ) -> Result<bool, InboxError> {
        let key = dedupe_key(provider_id);
        Ok(dedupe_hit(
            &self.read_dedupe()?,
            &key,
            now,
            dedupe_retention_ms,
        ))
    }

    /// Appends one delivery, then enforces `retention`. Everything happens
    /// under one directory lock: the dedupe check, the `seq` derivation, the
    /// append itself, and the retention rewrite. A duplicate provider id
    /// returns without writing anything.
    pub fn append_with(
        &self,
        provider_id: &str,
        body: &Value,
        retention: &Retention,
    ) -> Result<Appended, InboxError> {
        self.append_at(
            provider_id,
            body,
            None,
            retention,
            DEDUPE_RETENTION_MS,
            DEDUPE_MAX_ENTRIES,
            // The function, not its value: the clock is read under the
            // lock, so the receipt is the moment the seq is assigned.
            crate::clock::now_ms_u64,
        )
    }

    /// [`append_with`](Self::append_with) carrying the delivery's event type
    /// (the webhook block's cleaned event type) with the default retention
    /// envelope, dedupe window and clock. The dedupe identity does not
    /// change: a redelivery is a duplicate whatever its type.
    pub fn append_typed(
        &self,
        provider_id: &str,
        event_type: Option<&str>,
        body: &Value,
    ) -> Result<Appended, InboxError> {
        self.append_at(
            provider_id,
            body,
            event_type,
            &Retention::default(),
            DEDUPE_RETENTION_MS,
            DEDUPE_MAX_ENTRIES,
            crate::clock::now_ms_u64,
        )
    }

    /// `append_with` with the dedupe age window, size cap and wall-clock `now`
    /// all made explicit, the way `retention` already is. Production passes
    /// [`DEDUPE_RETENTION_MS`], [`DEDUPE_MAX_ENTRIES`] and the real clock; the
    /// seam exists so a test can drive the time-based eviction deterministically
    /// and prove the size cap rolls without writing ten thousand events.
    ///
    /// `now` stamps the dedupe entry, so a duplicate is judged against the
    /// same clock the entry was recorded with; the stored event's
    /// `received_at` is never below the previous event's stamp (see
    /// [`Self::append_at`]).
    #[allow(clippy::too_many_arguments)]
    pub fn append_bounded(
        &self,
        provider_id: &str,
        body: &Value,
        event_type: Option<&str>,
        retention: &Retention,
        dedupe_retention_ms: u64,
        dedupe_max_entries: usize,
        now: u64,
    ) -> Result<Appended, InboxError> {
        self.append_at(
            provider_id,
            body,
            event_type,
            retention,
            dedupe_retention_ms,
            dedupe_max_entries,
            move || now,
        )
    }

    /// The one append body, with the wall clock read under the lock
    /// (inbox-waits design D4 "The deadline"): a delivery stamped before it
    /// took the lock could be stored after it, and two concurrent
    /// deliveries could take seqs in the opposite order of their stamps.
    /// `clock` is called exactly once, right after [`lock_dir`] returned
    /// and after the read of the last stored event, and the stored event's
    /// `received_at` is `max(clock(), last.received_at)`, so `seq` and
    /// `received_at` are monotonic together: a later seq never has an
    /// earlier stamp. The dedupe check and the dedupe entry keep the
    /// clock's own value.
    #[allow(clippy::too_many_arguments)]
    fn append_at(
        &self,
        provider_id: &str,
        body: &Value,
        event_type: Option<&str>,
        retention: &Retention,
        dedupe_retention_ms: u64,
        dedupe_max_entries: usize,
        clock: impl FnOnce() -> u64,
    ) -> Result<Appended, InboxError> {
        std::fs::create_dir_all(&self.dir).map_err(|e| self.io(&e))?;
        let _lock = lock_dir(&self.dir, INBOX_LOCK).map_err(|e| self.io(&e))?;

        let last = self.last_event()?;
        let now = clock();

        let key = dedupe_key(provider_id);
        let mut seen = self.read_dedupe()?;
        // A still-live prior sighting deduplicates; an entry past the age
        // window does not, so a genuinely fresh delivery long after the window
        // is accepted while a replay inside it is refused. `is_duplicate` reads
        // the same helper, so the advisory pre-check agrees with this authority.
        if dedupe_hit(&seen, &key, now, dedupe_retention_ms) {
            return Ok(Appended::Duplicate);
        }

        let seq = last.as_ref().map(|e| e.seq).unwrap_or(0) + 1;
        let received_at = last.map(|e| e.received_at).unwrap_or(0).max(now);
        let event = InboxEvent {
            seq,
            received_at,
            provider_id: provider_id.to_string(),
            event_type: event_type.map(str::to_string),
            body: body.clone(),
        };
        let line = serde_json::to_string(&event)
            .map_err(|e| InboxError::Corrupt(self.path_str(EVENTS_FILE), e.to_string()))?;
        self.append_line(EVENTS_FILE, &line)?;

        seen.push(DedupeEntry { at: now, id: key });
        // Evict by age first, then apply the size safety cap oldest-first.
        seen.retain(|e| now.saturating_sub(e.at) < dedupe_retention_ms);
        if seen.len() > dedupe_max_entries {
            let excess = seen.len() - dedupe_max_entries;
            seen.drain(..excess);
        }
        self.write_dedupe(&seen)?;

        self.enforce_retention(retention)?;
        Ok(Appended::Stored(seq))
    }

    /// The log's stamp: the length and the modification time of
    /// `events.jsonl`, from one metadata call, `None` when the file does
    /// not exist. Any append changes it, so a reader that compares stamps
    /// never re-reads an unchanged log (inbox-waits design D4 "The visit"
    /// step 3).
    pub fn log_stamp(&self) -> Option<(u64, SystemTime)> {
        let meta = std::fs::metadata(self.dir.join(EVENTS_FILE)).ok()?;
        Some((meta.len(), meta.modified().ok()?))
    }

    /// The events `consumer` has not acknowledged, oldest first, at most
    /// `limit` of them, plus the cursor they were read from and the gap
    /// retention left behind, when it left one. Does not move the cursor:
    /// at-least-once with an explicit ack is the only honest contract when
    /// the reader is an agent that may stop mid-thought.
    ///
    /// The gap: retention may have dropped a prefix of the log this consumer
    /// never acknowledged. Sequence numbers are dense (they are handed out
    /// one at a time under the directory lock) and retention only ever drops
    /// a prefix, so a first stored seq past `cursor + 1` means exactly the
    /// closed range `cursor + 1 ..= first - 1` was stored and is now gone
    /// unread, and that is what [`InboxGap`] reports. Events this consumer
    /// acked before they were dropped are gone legitimately: the cursor sits
    /// at or above them, the first stored seq is `cursor + 1` or lower, and
    /// no gap is reported.
    pub fn read(
        &self,
        consumer: &str,
        limit: usize,
    ) -> Result<(Vec<InboxEvent>, u64, Option<InboxGap>), InboxError> {
        check_consumer(consumer)?;
        // The log first, the cursor second, with no lock: a concurrent ack
        // only moves the cursor forward, so a cursor read after the log can
        // only shrink the gap, never invent one that retention made
        // legitimately in between.
        let all = self.read_events()?;
        let cursor = self.cursor(consumer)?;
        let gap = lost_prefix(cursor, all.first().map(|e| e.seq));
        let mut out: Vec<InboxEvent> = all.into_iter().filter(|e| e.seq > cursor).collect();
        out.truncate(limit);
        Ok((out, cursor, gap))
    }

    /// Moves `consumer`'s cursor to `up_to_seq`, forward only, and returns
    /// where it ended up. An ack for an older seq is a no-op rather than an
    /// error: a retried ack must be harmless.
    ///
    /// The target is clamped to the current head under the lock. An ack past
    /// the last stored seq must never persist: it would silently mark every
    /// future event through that seq as already consumed, so a caller acking
    /// `100` against a three-event log would swallow events 4..=100 the moment
    /// they arrive, and retention would treat them as acknowledged. Clamping to
    /// `last_seq` keeps an optimistic or stale ack bounded by what actually
    /// exists while staying idempotent.
    pub fn ack(&self, consumer: &str, up_to_seq: u64) -> Result<u64, InboxError> {
        check_consumer(consumer)?;
        std::fs::create_dir_all(&self.dir).map_err(|e| self.io(&e))?;
        let _lock = lock_dir(&self.dir, INBOX_LOCK).map_err(|e| self.io(&e))?;
        let target = up_to_seq.min(self.last_seq()?);
        let mut cursors = self.read_cursors()?;
        let entry = cursors.consumers.entry(consumer.to_string()).or_insert(0);
        if target > *entry {
            *entry = target;
        }
        let moved = *entry;
        self.write_cursors(&cursors)?;
        Ok(moved)
    }

    /// [`Inbox::read`] through `matches`: the pending events of `consumer`
    /// that match, oldest first, at most `limit`.
    ///
    /// The events before the first match that do not match are skipped and,
    /// when `ack_skipped`, acknowledged for this consumer only: its cursor
    /// moves to the last of them. A non-matching event after a match is left
    /// unacknowledged (the reader has not acked the match yet); it is skipped
    /// again by the next read, once the match is acked. The gap is computed
    /// exactly as `read` does, before the cursor moves, so a filter never
    /// hides one.
    ///
    /// A move past a gap first hands the gap to `record_gap`, under the
    /// lock, before the cursor file is written: the caller records it where
    /// it must survive (a run's journal) and answers whether it did. On
    /// `false` the cursor does not move, so the gap is found again by the
    /// next read; a crash between the two leaves the gap recorded and the
    /// cursor where it was. Not called when nothing moves or there is no gap.
    ///
    /// The matcher is a predicate rather than a `connector::event_filter`:
    /// the filter type reads events (`explain`, `matches`), so a typed
    /// parameter here would make the two files import each other. A caller
    /// with an `EventFilter` passes `|e| filter.matches(e)`.
    pub fn read_filtered(
        &self,
        consumer: &str,
        limit: usize,
        matches: &impl Fn(&InboxEvent) -> bool,
        ack_skipped: bool,
        record_gap: impl FnOnce(&InboxGap) -> bool,
    ) -> Result<FilteredRead, InboxError> {
        check_consumer(consumer)?;
        // The whole read-and-move under one directory lock when it acks: a
        // concurrent append or ack never interleaves with the cursor move.
        // Without `ack_skipped` it reads without the lock, as `read` does.
        let _lock = if ack_skipped {
            std::fs::create_dir_all(&self.dir).map_err(|e| self.io(&e))?;
            Some(lock_dir(&self.dir, INBOX_LOCK).map_err(|e| self.io(&e))?)
        } else {
            None
        };
        let all = self.read_events()?;
        let cursor = self.cursor(consumer)?;
        // The gap, exactly as `read` computes it, from the cursor before
        // anything moved: a filter never hides one.
        let gap = lost_prefix(cursor, all.first().map(|e| e.seq));
        let mut out = Vec::new();
        let mut skipped = 0u64;
        // The last event before the first match, acked for this consumer only.
        let mut skip_through = None;
        // Whether a match was seen, returned or not: a skip after one is
        // never acked, even when the limit kept the match out of `out`.
        let mut seen_match = false;
        for event in all.iter().filter(|e| e.seq > cursor) {
            if matches(event) {
                seen_match = true;
                if out.len() < limit {
                    out.push(event.clone());
                }
            } else {
                skipped += 1;
                if !seen_match {
                    skip_through = Some(event.seq);
                }
            }
        }
        let mut cursor = cursor;
        if ack_skipped
            && let Some(target) = skip_through
            && target > cursor
            && gap.as_ref().is_none_or(record_gap)
        {
            // The cursor moves forward only, under the already-held lock (the
            // rule of `ack`).
            let mut cursors = self.read_cursors()?;
            let entry = cursors.consumers.entry(consumer.to_string()).or_insert(0);
            if target > *entry {
                *entry = target;
            }
            cursor = *entry;
            self.write_cursors(&cursors)?;
        }
        Ok(FilteredRead {
            events: out,
            cursor,
            gap,
            skipped,
        })
    }

    /// The highest stored seq, 0 for an empty log: the private `last_seq`,
    /// which stays private and unchanged.
    pub fn head_seq(&self) -> Result<u64, InboxError> {
        self.last_seq()
    }

    /// Counts for one consumer, derived from the first and last stored events
    /// plus the cursor. Reads no lock: an approximate answer under concurrent
    /// delivery is fine for a probe and a dashboard panel.
    ///
    /// Arithmetic, not a scan. Sequence numbers are dense by construction:
    /// they are handed out one at a time under the directory lock, and
    /// retention only ever drops a prefix (acknowledged and expired first,
    /// then oldest by size), so the live log is exactly the closed range
    /// `first.seq ..= last.seq`. That matters because the doctor and the
    /// dashboard panel call this on every refresh against a log that may be
    /// tens of megabytes, and parsing every line to count them would make an
    /// idle dashboard the most expensive thing touching the store.
    pub fn depth(&self, consumer: &str) -> Result<Depth, InboxError> {
        check_consumer(consumer)?;
        let cursor = self.cursor(consumer)?;
        let dropped = self.dropped_count()?;
        let (Some(first), Some(last)) = (self.first_event()?, self.last_event()?) else {
            return Ok(Depth {
                pending: 0,
                total: 0,
                cursor,
                last_received_at: None,
                dropped,
            });
        };
        // A cursor may point below the surviving range after retention took
        // the entries it referred to; nothing before `first` is pending.
        let acked_through = cursor.max(first.seq.saturating_sub(1));
        Ok(Depth {
            pending: last.seq.saturating_sub(acked_through),
            total: last.seq - first.seq + 1,
            cursor,
            last_received_at: Some(last.received_at),
            dropped,
        })
    }

    /// Increments the persisted dropped-delivery counter for this account and
    /// returns the new total. Called from the ingest accept-cap path (spec
    /// 2026-08-16-webhook-ingest-design): a delivery over the accept cap is
    /// answered 200 and dropped without a stored event, so this is the only
    /// record it leaves behind. Under the same directory lock as every other
    /// mutation, so a burst of concurrent drops still counts each one exactly
    /// once.
    pub fn note_dropped(&self) -> Result<u64, InboxError> {
        std::fs::create_dir_all(&self.dir).map_err(|e| self.io(&e))?;
        let _lock = lock_dir(&self.dir, INBOX_LOCK).map_err(|e| self.io(&e))?;
        let count = self.read_dropped()? + 1;
        self.write_dropped(count)?;
        Ok(count)
    }

    /// The persisted dropped-delivery count, or 0 when nothing has ever been
    /// dropped for this account. Reads no lock, matching `cursor` and
    /// `depth`: an approximate answer under a concurrent drop is fine for a
    /// probe and a dashboard panel.
    pub fn dropped_count(&self) -> Result<u64, InboxError> {
        self.read_dropped()
    }

    /// [`Inbox::read_events`] once no append is in flight, waiting at most
    /// `bound` for one to finish. An append stamps `received_at` after it
    /// took the store's lock and releases the lock only once its line is
    /// written, so a read made while the lock is free holds every event
    /// stamped before that moment. The wait only looks at the lock
    /// ([`crate::fsutil::lock_held`], no lock taken, nothing written), so a
    /// reader that must leave the store untouched can use it; past `bound`
    /// it reads anyway.
    pub fn read_events_settled(
        &self,
        bound: std::time::Duration,
    ) -> Result<Vec<InboxEvent>, InboxError> {
        let until = std::time::Instant::now() + bound;
        while crate::fsutil::lock_held(&self.dir, INBOX_LOCK, SystemTime::now())
            && std::time::Instant::now() < until
        {
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        self.read_events()
    }

    /// Every stored event, oldest first.
    pub fn read_events(&self) -> Result<Vec<InboxEvent>, InboxError> {
        let path = self.dir.join(EVENTS_FILE);
        let raw = match std::fs::read_to_string(&path) {
            Ok(r) => r,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(self.io(&e)),
        };
        let mut out = Vec::new();
        for line in raw.lines() {
            if line.trim().is_empty() {
                continue;
            }
            let event: InboxEvent = serde_json::from_str(line)
                .map_err(|e| InboxError::Corrupt(path.display().to_string(), e.to_string()))?;
            out.push(event);
        }
        Ok(out)
    }

    fn cursor(&self, consumer: &str) -> Result<u64, InboxError> {
        Ok(self
            .read_cursors()?
            .consumers
            .get(consumer)
            .copied()
            .unwrap_or(0))
    }

    /// The lowest cursor across every known consumer, or 0 when none exists.
    /// An event at or below it has been acknowledged by everyone, which is
    /// what makes it a retention candidate before anything unacked. Public so
    /// a caller outside this module (the dashboard's events route) can filter
    /// a raw event list down to the same "still pending for someone" set that
    /// retention itself protects, without re-reading `cursors.yaml` by hand.
    pub fn min_cursor(&self) -> Result<u64, InboxError> {
        let cursors = self.read_cursors()?;
        Ok(cursors.consumers.values().copied().min().unwrap_or(0))
    }

    /// The last stored event, or `None`. Only the last non-empty line is
    /// parsed, so appending and `depth` stay cheap on a large log.
    fn last_event(&self) -> Result<Option<InboxEvent>, InboxError> {
        let path = self.dir.join(EVENTS_FILE);
        let raw = match std::fs::read_to_string(&path) {
            Ok(r) => r,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(self.io(&e)),
        };
        for line in raw.lines().rev() {
            if line.trim().is_empty() {
                continue;
            }
            let event: InboxEvent = serde_json::from_str(line)
                .map_err(|e| InboxError::Corrupt(path.display().to_string(), e.to_string()))?;
            return Ok(Some(event));
        }
        Ok(None)
    }

    /// The `seq` of the last stored event, or 0.
    fn last_seq(&self) -> Result<u64, InboxError> {
        Ok(self.last_event()?.map(|e| e.seq).unwrap_or(0))
    }

    /// The first stored event, for the cheap retention pre-check.
    fn first_event(&self) -> Result<Option<InboxEvent>, InboxError> {
        let path = self.dir.join(EVENTS_FILE);
        let raw = match std::fs::read_to_string(&path) {
            Ok(r) => r,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(self.io(&e)),
        };
        for line in raw.lines() {
            if line.trim().is_empty() {
                continue;
            }
            let event: InboxEvent = serde_json::from_str(line)
                .map_err(|e| InboxError::Corrupt(path.display().to_string(), e.to_string()))?;
            return Ok(Some(event));
        }
        Ok(None)
    }

    /// Drops what the envelope no longer allows: first every acknowledged
    /// event past the age window, then, only if the size cap is still
    /// exceeded, the oldest events regardless of ack state. Rewrites the log
    /// only when something actually goes, so the ordinary append does one
    /// metadata read and one first-line parse.
    ///
    /// Invariant: the newest event is never removed, on either path, even
    /// when it is acked and past the age window. `append` derives the next
    /// `seq` from the last line of `events.jsonl`, so an emptied file would
    /// restart `seq` at 1 while `cursors.yaml` still held the old high
    /// cursor - `depth`'s cheap first/last-plus-cursor arithmetic would then
    /// compute `pending` against a cursor above any live `seq`, and dedupe
    /// and cursor semantics would no longer describe the same log. Keeping
    /// one event alive at all times keeps `seq` continuous across retention.
    fn enforce_retention(&self, retention: &Retention) -> Result<(), InboxError> {
        let path = self.dir.join(EVENTS_FILE);
        let size = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
        let now = crate::clock::now_ms_u64();
        // Strictly younger than the window, so a zero-length window (used
        // by tests and by an operator who wants acked events gone at once)
        // always reaches the rewrite path rather than depending on whether
        // the append landed in the same millisecond.
        let oldest_is_fresh = self
            .first_event()?
            .map(|e| now.saturating_sub(e.received_at) < retention.max_age_ms)
            .unwrap_or(true);
        if size <= retention.max_bytes && oldest_is_fresh {
            return Ok(());
        }

        let mut events = self.read_events()?;
        let before = events.len();
        let acked_through = self.min_cursor()?;
        // The log is append-only and ordered by seq, so the last element
        // holds the newest event; it is exempt from the age-based drop
        // below regardless of ack or age, per the invariant on this method.
        let newest_seq = events.last().map(|e| e.seq);
        events.retain(|e| {
            let acked = e.seq <= acked_through;
            let expired = now.saturating_sub(e.received_at) >= retention.max_age_ms;
            Some(e.seq) == newest_seq || !(acked && expired)
        });
        // The newest event is never dropped by the size cap either: a single
        // delivery larger than the whole envelope must not empty the store,
        // and an inbox that answers "nothing arrived" after something did is
        // worse than one that is briefly over its cap.
        let mut bytes: u64 = events.iter().map(line_bytes).sum();
        while bytes > retention.max_bytes && events.len() > 1 {
            let dropped = events.remove(0);
            bytes = bytes.saturating_sub(line_bytes(&dropped));
        }
        if events.len() == before {
            return Ok(());
        }
        let mut body = String::new();
        for event in &events {
            let line = serde_json::to_string(event)
                .map_err(|e| InboxError::Corrupt(path.display().to_string(), e.to_string()))?;
            body.push_str(&line);
            body.push('\n');
        }
        atomic_write_private(&path, body.as_bytes()).map_err(|e| self.io(&e))
    }

    /// The dedupe index, oldest first. Each line is `<received_at_ms>
    /// <digest_hex>`. A line that does not parse is skipped rather than
    /// treated as corruption: the index is an advisory, self-healing
    /// optimization (a lost entry only risks re-accepting one delivery), and
    /// this also lets an older count-based `dedupe.idx` be superseded in place
    /// without a migration step.
    fn read_dedupe(&self) -> Result<Vec<DedupeEntry>, InboxError> {
        let path = self.dir.join(DEDUPE_FILE);
        match std::fs::read_to_string(&path) {
            Ok(raw) => Ok(raw.lines().filter_map(DedupeEntry::parse).collect()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
            Err(e) => Err(self.io(&e)),
        }
    }

    fn write_dedupe(&self, entries: &[DedupeEntry]) -> Result<(), InboxError> {
        let mut body = String::new();
        for entry in entries {
            body.push_str(&entry.at.to_string());
            body.push(' ');
            body.push_str(&entry.id);
            body.push('\n');
        }
        atomic_write_private(&self.dir.join(DEDUPE_FILE), body.as_bytes()).map_err(|e| self.io(&e))
    }

    fn read_cursors(&self) -> Result<Cursors, InboxError> {
        let path = self.dir.join(CURSORS_FILE);
        match std::fs::read_to_string(&path) {
            Ok(raw) => yaml_serde::from_str(&raw)
                .map_err(|e| InboxError::Corrupt(path.display().to_string(), e.to_string())),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Cursors::default()),
            Err(e) => Err(self.io(&e)),
        }
    }

    fn write_cursors(&self, cursors: &Cursors) -> Result<(), InboxError> {
        let yaml = yaml_serde::to_string(cursors)
            .map_err(|e| InboxError::Corrupt(self.path_str(CURSORS_FILE), e.to_string()))?;
        atomic_write_private(&self.dir.join(CURSORS_FILE), yaml.as_bytes()).map_err(|e| self.io(&e))
    }

    /// The dropped counter's stored value, or 0 when the file is absent. A
    /// present-but-unparsable file is corruption, not an absent counter, so
    /// it is reported rather than silently reset to 0.
    fn read_dropped(&self) -> Result<u64, InboxError> {
        let path = self.dir.join(DROPPED_FILE);
        match std::fs::read_to_string(&path) {
            Ok(raw) => raw
                .trim()
                .parse::<u64>()
                .map_err(|e| InboxError::Corrupt(path.display().to_string(), e.to_string())),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(0),
            Err(e) => Err(self.io(&e)),
        }
    }

    fn write_dropped(&self, count: u64) -> Result<(), InboxError> {
        atomic_write_private(&self.dir.join(DROPPED_FILE), count.to_string().as_bytes())
            .map_err(|e| self.io(&e))
    }

    /// Appends one line to a file in the account directory, creating it 0600
    /// on unix. `O_APPEND` keeps a line whole even if a lock were somehow
    /// bypassed; the lock is what keeps `seq` unique.
    ///
    /// The line and its newline go out in a single `write_all`, not through
    /// `writeln!`: `writeln!` can issue two syscalls, and a crash between
    /// them leaves a tail with no newline, which would glue the next append
    /// onto it and corrupt both records. For the same reason an existing tail
    /// that is missing its newline (written by an older build, or by a crash
    /// before this fix) is repaired by prefixing one rather than appended to
    /// blindly.
    fn append_line(&self, file: &str, line: &str) -> Result<(), InboxError> {
        let path = self.dir.join(file);
        let needs_leading_newline = match std::fs::metadata(&path) {
            Ok(meta) if meta.len() > 0 => !self.ends_with_newline(&path)?,
            _ => false,
        };
        let mut opts = OpenOptions::new();
        opts.create(true).append(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            opts.mode(0o600);
        }
        let mut handle = opts.open(&path).map_err(|e| self.io(&e))?;
        let mut record = String::with_capacity(line.len() + 2);
        if needs_leading_newline {
            record.push('\n');
        }
        record.push_str(line);
        record.push('\n');
        handle
            .write_all(record.as_bytes())
            .map_err(|e| self.io(&e))?;
        handle.flush().map_err(|e| self.io(&e))?;
        Ok(())
    }

    /// Whether the file's last byte is a newline. Reads one byte from the end
    /// rather than the whole file.
    fn ends_with_newline(&self, path: &Path) -> Result<bool, InboxError> {
        use std::io::{Read, Seek, SeekFrom};
        let mut handle = std::fs::File::open(path).map_err(|e| self.io(&e))?;
        handle.seek(SeekFrom::End(-1)).map_err(|e| self.io(&e))?;
        let mut last = [0u8; 1];
        handle.read_exact(&mut last).map_err(|e| self.io(&e))?;
        Ok(last[0] == b'\n')
    }

    fn path_str(&self, file: &str) -> String {
        self.dir.join(file).display().to_string()
    }

    fn io(&self, e: &std::io::Error) -> InboxError {
        InboxError::Io(self.dir.display().to_string(), e.to_string())
    }
}

/// One dedupe-index entry: when the id was recorded (ms since epoch) and the
/// id's fixed-size digest. The digest, never the raw id, is what the index
/// stores and compares, so a provider-controlled scalar carries no structure
/// (a newline, a huge length) into this newline-delimited file.
#[derive(Debug, Clone, PartialEq, Eq)]
struct DedupeEntry {
    at: u64,
    id: String,
}

impl DedupeEntry {
    /// Parses one `<received_at_ms> <digest_hex>` line, or `None` if it does
    /// not have that shape (an empty line, a malformed timestamp, or a legacy
    /// count-based entry without a timestamp column).
    fn parse(line: &str) -> Option<DedupeEntry> {
        let line = line.trim();
        if line.is_empty() {
            return None;
        }
        let (at, id) = line.split_once(' ')?;
        let at = at.parse::<u64>().ok()?;
        if id.is_empty() {
            return None;
        }
        Some(DedupeEntry {
            at,
            id: id.to_string(),
        })
    }
}

/// The stored dedupe identity of a provider id: its SHA-256 as lowercase hex.
/// Hashing here is what makes the dedupe index safe against a
/// provider-controlled id of any length or byte content, and gives every
/// entry the same fixed 64-char width.
fn dedupe_key(provider_id: &str) -> String {
    crate::content::sha256_hex(provider_id.as_bytes())
}

/// Whether `entries` holds a still-live sighting of `key`: a matching digest
/// whose age at `now` is within `dedupe_retention_ms`. Shared by the
/// authoritative `append_bounded` and the advisory `is_duplicate` so both agree
/// that an entry past the window is not a duplicate; without this, a
/// redelivery of an old id long after a quiet period read as a duplicate in the
/// advisory check while the authority would have stored it.
fn dedupe_hit(entries: &[DedupeEntry], key: &str, now: u64, dedupe_retention_ms: u64) -> bool {
    entries
        .iter()
        .any(|e| e.id == key && now.saturating_sub(e.at) < dedupe_retention_ms)
}

/// The serialized size of one event's line, including its newline. Used by
/// the size cap so the decision matches what the rewrite will produce rather
/// than what the current file happens to hold.
fn line_bytes(event: &InboxEvent) -> u64 {
    serde_json::to_string(event)
        .map(|s| s.len() as u64 + 1)
        .unwrap_or(0)
}

/// The unread prefix a retention drop cost the consumer at `cursor`, or
/// `None` when everything after the cursor is still stored. Shared by
/// `read`'s gap detection so the range arithmetic lives in one place:
/// `from` is the first seq the consumer has not seen, `through` the seq just
/// before the oldest survivor. Public so an inbox wait's visit computes the
/// same gap for its position (inbox-waits design D4 "The visit" step 4).
pub fn lost_prefix(cursor: u64, first_stored: Option<u64>) -> Option<InboxGap> {
    let from = cursor.checked_add(1)?;
    let first = first_stored?;
    (first > from).then(|| InboxGap {
        from,
        through: first - 1,
    })
}

/// A consumer name is a machine-facing identifier, validated like a function
/// or account-field name. It becomes a key in `cursors.yaml`, so anything
/// looser would let a caller shape that file.
fn check_consumer(consumer: &str) -> Result<(), InboxError> {
    super::common::validate_snake_name(consumer)
        .map_err(|e| InboxError::Name(consumer.to_string(), e))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// One inbox in a fresh temp directory. The guard keeps the directory
    /// alive for the test's duration.
    struct TempInbox {
        _dir: tempfile::TempDir,
        inbox: Inbox,
    }

    fn temp_inbox() -> TempInbox {
        let dir = tempfile::tempdir().expect("tempdir");
        let inbox = Inbox::at(dir.path(), "github", "main").expect("inbox");
        TempInbox { _dir: dir, inbox }
    }

    /// The matcher the filtered reads use: the event's type is one of
    /// `types`, exactly what an `EventFilter`'s type condition decides.
    fn type_filter(types: &[&str]) -> impl Fn(&InboxEvent) -> bool {
        let types: Vec<String> = types.iter().map(|t| t.to_string()).collect();
        move |e: &InboxEvent| {
            e.event_type
                .as_deref()
                .is_some_and(|t| types.iter().any(|want| want == t))
        }
    }

    /// `head_seq` answers the private `last_seq`: 0 over an empty log, then
    /// the seq the first append was handed.
    #[test]
    fn head_seq_is_zero_for_an_empty_log() {
        let t = temp_inbox();
        assert_eq!(t.inbox.head_seq().expect("head_seq"), 0);
        let stored = t
            .inbox
            .append_typed("p1", Some("issues"), &json!({}))
            .expect("append");
        assert_eq!(stored, Appended::Stored(1), "the first append gets seq 1");
        assert_eq!(t.inbox.head_seq().expect("head_seq"), 1);
    }

    /// A match the read does not return (a limit of 0) still stops the
    /// acknowledgement: a skip after it is never acked past it.
    #[test]
    fn a_match_past_the_limit_is_never_acked_past() {
        let t = temp_inbox();
        t.inbox
            .append_typed("p1", Some("push"), &json!({}))
            .unwrap();
        t.inbox
            .append_typed("p2", Some("issues"), &json!({}))
            .unwrap();
        t.inbox
            .append_typed("p3", Some("push"), &json!({}))
            .unwrap();
        let read = t
            .inbox
            .read_filtered("a", 0, &type_filter(&["issues"]), true, |_| true)
            .expect("read");
        assert!(read.events.is_empty());
        assert_eq!(read.skipped, 2);
        assert_eq!(read.cursor, 1, "only the skip before the match is acked");
    }

    /// A filtered read acks, for its own consumer only, the events before the
    /// first match, and leaves a skip after an unacked match alone.
    #[test]
    fn a_filtered_read_acks_only_the_leading_skips() {
        let t = temp_inbox();
        t.inbox
            .append("p1", &json!({ "kind": "push" }))
            .expect("event 1");
        t.inbox
            .append_typed("p2", Some("issues"), &json!({ "kind": "issue" }))
            .expect("event 2");
        t.inbox
            .append("p3", &json!({ "kind": "push" }))
            .expect("event 3");
        let filter = type_filter(&["issues"]);
        let read = t
            .inbox
            .read_filtered("a", 10, &filter, true, |_| true)
            .expect("read");
        assert_eq!(read.events.len(), 1, "only the match is returned");
        assert_eq!(read.events[0].provider_id, "p2");
        assert_eq!(read.cursor, 1, "the cursor moved past the leading skip");
        assert_eq!(read.skipped, 2, "both non-matching pending events");
        assert_eq!(read.gap, None);

        // The reader acks the match, and the next read moves past the trailing
        // skip, which is now leading.
        t.inbox.ack("a", 2).expect("ack the match");
        let read = t
            .inbox
            .read_filtered("a", 10, &filter, true, |_| true)
            .expect("second read");
        assert!(read.events.is_empty());
        assert_eq!(read.cursor, 3, "the trailing skip is now leading");
        assert_eq!(read.skipped, 1);

        // Without `ack_skipped` the cursor never moves.
        t.inbox
            .append("p4", &json!({ "kind": "push" }))
            .expect("event 4");
        let read = t
            .inbox
            .read_filtered("a", 10, &filter, false, |_| true)
            .expect("third read");
        assert_eq!(read.cursor, 3, "no move without ack_skipped");
        assert_eq!(read.skipped, 1);
    }

    /// A consumer's filtered read and its acks never move another consumer's
    /// cursor.
    #[test]
    fn a_filtered_read_leaves_other_consumers_alone() {
        let t = temp_inbox();
        t.inbox
            .append("p1", &json!({ "kind": "push" }))
            .expect("event 1");
        t.inbox
            .append_typed("p2", Some("issues"), &json!({ "kind": "issue" }))
            .expect("event 2");
        let filter = type_filter(&["issues"]);
        let read = t
            .inbox
            .read_filtered("a", 10, &filter, true, |_| true)
            .expect("read");
        assert_eq!(read.cursor, 1);
        assert_eq!(
            t.inbox.cursor("b").expect("cursor"),
            0,
            "consumer b untouched"
        );
        let (events, cursor_b, _) = t.inbox.read("b", 10).expect("read b");
        assert_eq!(events.len(), 2, "consumer b still reads every event");
        assert_eq!(cursor_b, 0);
    }

    /// Retention dropped seqs 1..=3 before any consumer read them: the log
    /// holds three untyped events, 4 to 6.
    fn seed_after_a_drop(inbox: &Inbox) {
        std::fs::create_dir_all(&inbox.dir).expect("dir");
        for seq in 4..=6u64 {
            let event = InboxEvent {
                seq,
                received_at: 0,
                provider_id: format!("p{seq}"),
                event_type: None,
                body: json!({ "kind": "push" }),
            };
            let line = serde_json::to_string(&event).expect("line");
            use std::io::Write;
            let mut handle = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(inbox.dir.join(EVENTS_FILE))
                .expect("open log");
            writeln!(handle, "{line}").expect("write line");
        }
    }

    /// A move past a gap hands the gap to `record_gap` while the cursor
    /// file still holds the old cursor, and a refusal (the gap was not
    /// recorded) moves nothing: the next read finds the same gap. The hook
    /// is the crash point: what it sees on disk is what a crash there leaves.
    #[test]
    fn a_gap_is_recorded_before_the_cursor_moves_past_it() {
        let t = temp_inbox();
        seed_after_a_drop(&t.inbox);
        let filter = type_filter(&["issues"]);
        let mut seen = Vec::new();
        let read = t
            .inbox
            .read_filtered("a", 10, &filter, true, |gap| {
                seen.push((*gap, t.inbox.cursor("a").expect("cursor")));
                false
            })
            .expect("read");
        assert_eq!(
            seen,
            vec![(
                InboxGap {
                    from: 1,
                    through: 3
                },
                0
            )],
            "the gap is offered while the stored cursor is still 0"
        );
        assert_eq!(read.cursor, 0, "an unrecorded gap moves no cursor");
        assert_eq!(t.inbox.cursor("a").expect("cursor"), 0);

        let read = t
            .inbox
            .read_filtered("a", 10, &filter, true, |_| true)
            .expect("second read");
        assert_eq!(
            read.gap,
            Some(InboxGap {
                from: 1,
                through: 3
            })
        );
        assert_eq!(read.cursor, 6, "a recorded gap lets the cursor move");
    }

    /// The gap is computed from the cursor before anything moved, so a skip
    /// that runs over the gap's range never hides it.
    #[test]
    fn a_filter_never_hides_a_gap() {
        let t = temp_inbox();
        seed_after_a_drop(&t.inbox);
        // Every remaining event is skipped, and the read acks them.
        let filter = type_filter(&["issues"]);
        let read = t
            .inbox
            .read_filtered("a", 10, &filter, true, |_| true)
            .expect("read");
        assert!(read.events.is_empty());
        assert_eq!(read.skipped, 3);
        assert_eq!(read.cursor, 6, "every remaining event was acked");
        assert_eq!(
            read.gap,
            Some(InboxGap {
                from: 1,
                through: 3
            }),
            "a skip over the gap's range never hides it"
        );
    }

    /// A line stored before `event_type` existed reads as an untyped event.
    #[test]
    fn an_old_event_reads_untyped() {
        let base = std::env::temp_dir().join(format!(
            "apb-inbox-test-{}-{}",
            std::process::id(),
            crate::clock::now_ms_u64()
        ));
        let inbox = Inbox::at(&base, "github", "main").expect("inbox");
        std::fs::create_dir_all(&inbox.dir).expect("dir");
        let old = serde_json::json!({
            "seq": 1,
            "received_at": 1700000000000u64,
            "provider_id": "abc",
            "body": { "action": "opened" }
        });
        std::fs::write(
            inbox.dir.join(EVENTS_FILE),
            format!("{}\n", serde_json::to_string(&old).unwrap()),
        )
        .expect("write old line");
        let events = inbox.read_events().expect("read");
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].event_type, None);
        std::fs::remove_dir_all(&base).ok();
    }

    /// An append changes the log stamp: the length grows with every stored
    /// event, so a reader that compares stamps never re-reads an unchanged
    /// log (inbox-waits D4 "The visit" step 3).
    #[test]
    fn log_stamp_changes_on_append() {
        let t = temp_inbox();
        assert!(t.inbox.log_stamp().is_none(), "no log, no stamp");
        t.inbox
            .append_typed("p1", Some("push"), &json!({}))
            .unwrap();
        let first = t.inbox.log_stamp().expect("the append makes a log");
        t.inbox
            .append_typed("p2", Some("push"), &json!({}))
            .unwrap();
        let second = t.inbox.log_stamp().expect("the log exists");
        assert!(
            second.0 > first.0,
            "the length grew: {} to {}",
            first.0,
            second.0
        );
    }

    /// An account whose log was never written has no stamp.
    #[test]
    fn log_stamp_is_none_without_a_log() {
        let t = temp_inbox();
        assert!(t.inbox.log_stamp().is_none());
        // A delivered-but-duplicate append writes nothing: still no log.
        t.inbox
            .append_typed("p1", Some("push"), &json!({}))
            .unwrap();
        t.inbox
            .append_typed("p1", Some("push"), &json!({}))
            .unwrap();
        assert_eq!(
            t.inbox.read_events().unwrap().len(),
            1,
            "the duplicate stored nothing"
        );
    }

    /// Two threads appending at once: every later `seq` has a stamp at or
    /// above the earlier one, because the receipt is stamped under the
    /// lock the `seq` is handed out under (inbox-waits D4 "The deadline").
    #[test]
    fn concurrent_deliveries_are_stamped_in_seq_order() {
        let t = temp_inbox();
        let dir = t.inbox.dir.clone();
        let handles: Vec<_> = (0..2)
            .map(|worker| {
                let dir = dir.clone();
                std::thread::spawn(move || {
                    let inbox = Inbox { dir };
                    for i in 0..25 {
                        let id = format!("w{worker}-{i}");
                        inbox
                            .append_typed(&id, Some("push"), &json!({ "i": i }))
                            .expect("append");
                    }
                })
            })
            .collect();
        for h in handles {
            h.join().unwrap();
        }
        let mut events = t.inbox.read_events().unwrap();
        assert_eq!(events.len(), 50, "every append stored one event");
        events.sort_by_key(|e| e.seq);
        assert!(
            events.windows(2).all(|w| w[0].seq < w[1].seq),
            "seqs are unique"
        );
        assert!(
            events
                .windows(2)
                .all(|w| w[0].received_at <= w[1].received_at),
            "a later seq never has an earlier stamp"
        );
    }

    /// A delivery that waits on the store's lock is stamped when it gets
    /// the lock, not when it asked for it: the stamp is read under the
    /// lock, so the receipt is the moment the `seq` is assigned.
    #[test]
    fn a_delivery_waiting_on_the_lock_is_stamped_when_it_gets_it() {
        let t = temp_inbox();
        // Hold the account's lock while the delivery tries to append.
        let held = crate::fsutil::lock_dir(&t.inbox.dir, INBOX_LOCK).expect("hold the lock");
        let before = crate::clock::now_ms_u64();
        let dir = t.inbox.dir.clone();
        let handle = std::thread::spawn(move || {
            let inbox = Inbox { dir };
            inbox
                .append_typed("p1", Some("push"), &json!({}))
                .expect("append")
        });
        std::thread::sleep(std::time::Duration::from_millis(200));
        let released_at = crate::clock::now_ms_u64();
        drop(held);
        let appended = handle.join().unwrap();
        assert!(
            matches!(appended, Appended::Stored(1)),
            "the waiting delivery stored event 1: {appended:?}"
        );
        let events = t.inbox.read_events().unwrap();
        assert_eq!(events.len(), 1);
        assert!(
            events[0].received_at >= released_at,
            "stamped at or after the release ({} < {released_at})",
            events[0].received_at
        );
        assert!(
            events[0].received_at >= before,
            "stamped at or after the append started"
        );
    }

    /// A settled read waits for the append that holds the store's lock and
    /// then holds its event; it takes no lock and writes nothing itself.
    #[test]
    fn a_settled_read_waits_for_the_append_in_flight() {
        let t = temp_inbox();
        t.inbox
            .append_typed("p0", Some("push"), &json!({}))
            .expect("append");
        // An append in flight: the lock is held and its line not yet written.
        let held = crate::fsutil::lock_dir(&t.inbox.dir, INBOX_LOCK).expect("hold the lock");
        let dir = t.inbox.dir.clone();
        let reader = std::thread::spawn(move || {
            Inbox { dir }
                .read_events_settled(std::time::Duration::from_secs(10))
                .expect("read")
        });
        std::thread::sleep(std::time::Duration::from_millis(200));
        let mut line = serde_json::to_string(&t.inbox.read_events().unwrap()[0]).unwrap();
        line = line.replace("\"seq\":1", "\"seq\":2");
        let mut f = OpenOptions::new()
            .append(true)
            .open(t.inbox.dir.join(EVENTS_FILE))
            .unwrap();
        writeln!(f, "{line}").unwrap();
        drop(f);
        drop(held);
        let events = reader.join().unwrap();
        assert_eq!(
            events.iter().map(|e| e.seq).collect::<Vec<_>>(),
            vec![1, 2],
            "the settled read holds the event the append in flight wrote"
        );
        assert!(
            !t.inbox.dir.join(INBOX_LOCK).exists(),
            "the reader took no lock"
        );
        // A free lock reads at once.
        assert_eq!(
            t.inbox
                .read_events_settled(std::time::Duration::ZERO)
                .unwrap()
                .len(),
            2
        );
    }

    /// A clock that steps back never moves a stamp backwards: the receipt
    /// is at or above the previous event's stamp (`max` rule), while the
    /// dedupe entry keeps the clock's own value (D4 "The deadline").
    #[test]
    fn received_at_never_goes_back() {
        let t = temp_inbox();
        t.inbox
            .append_bounded(
                "p1",
                &json!({}),
                None,
                &Retention::default(),
                DEDUPE_RETENTION_MS,
                DEDUPE_MAX_ENTRIES,
                1_000,
            )
            .unwrap();
        t.inbox
            .append_bounded(
                "p2",
                &json!({}),
                None,
                &Retention::default(),
                DEDUPE_RETENTION_MS,
                DEDUPE_MAX_ENTRIES,
                500,
            )
            .unwrap();
        let events = t.inbox.read_events().unwrap();
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].received_at, 1_000);
        assert_eq!(
            events[1].received_at, 1_000,
            "a stepping-back clock stamps the previous event's time"
        );
    }
}
