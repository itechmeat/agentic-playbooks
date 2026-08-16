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
//!   * `dedupe.idx`   - the last `DEDUPE_WINDOW` provider ids, one per line.
//!   * `cursors.yaml` - `consumer -> last acked seq`.
//!
//! Every mutation happens under `fsutil::lock_dir` on the account directory
//! and `seq` is derived inside that lock, so two concurrent deliveries can
//! never be handed the same number. The run-signal channel's older
//! read-count-then-append shape is fixed to match in Task 2.
//!
//! Bodies stored here are authored by whoever can reach the ingest endpoint.
//! Nothing in this module logs one, and no caller may put one in a run's
//! event log.

use std::collections::BTreeMap;
use std::fs::OpenOptions;
use std::io::Write;
use std::path::{Path, PathBuf};

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
/// Lock file serializing every read-modify-write on one account directory.
const INBOX_LOCK: &str = "inbox.lock";
/// How many recently seen provider ids the dedupe index keeps. Large enough
/// to cover a provider's retry window, small enough to stay a cheap linear
/// scan of a file that is a few hundred kilobytes at worst.
pub const DEDUPE_WINDOW: usize = 10_000;

/// One stored delivery. `body` is the payload exactly as parsed from the
/// request; `provider_id` is the dedupe identity the connector's webhook
/// block selected; `received_at` is milliseconds since the epoch from the
/// single wall-clock source.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct InboxEvent {
    pub seq: u64,
    pub received_at: u64,
    pub provider_id: String,
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

/// Counts for the doctor and the dashboard panel. Carries no body.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Depth {
    pub pending: u64,
    pub total: u64,
    pub cursor: u64,
    pub last_received_at: Option<u64>,
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
        std::fs::create_dir_all(&self.dir).map_err(|e| self.io(&e))?;
        let _lock = lock_dir(&self.dir, INBOX_LOCK).map_err(|e| self.io(&e))?;

        let mut seen = self.read_dedupe()?;
        if seen.iter().any(|id| id == provider_id) {
            return Ok(Appended::Duplicate);
        }

        let seq = self.last_seq()? + 1;
        let event = InboxEvent {
            seq,
            received_at: crate::clock::now_ms_u64(),
            provider_id: provider_id.to_string(),
            body: body.clone(),
        };
        let line = serde_json::to_string(&event)
            .map_err(|e| InboxError::Corrupt(self.path_str(EVENTS_FILE), e.to_string()))?;
        self.append_line(EVENTS_FILE, &line)?;

        seen.push(provider_id.to_string());
        if seen.len() > DEDUPE_WINDOW {
            let excess = seen.len() - DEDUPE_WINDOW;
            seen.drain(..excess);
        }
        self.write_dedupe(&seen)?;

        self.enforce_retention(retention)?;
        Ok(Appended::Stored(seq))
    }

    /// The events `consumer` has not acknowledged, oldest first, at most
    /// `limit` of them, plus the cursor they were read from. Does not move
    /// the cursor: at-least-once with an explicit ack is the only honest
    /// contract when the reader is an agent that may stop mid-thought.
    pub fn read(&self, consumer: &str, limit: usize) -> Result<(Vec<InboxEvent>, u64), InboxError> {
        check_consumer(consumer)?;
        let cursor = self.cursor(consumer)?;
        let mut out: Vec<InboxEvent> = self
            .read_events()?
            .into_iter()
            .filter(|e| e.seq > cursor)
            .collect();
        out.truncate(limit);
        Ok((out, cursor))
    }

    /// Moves `consumer`'s cursor to `up_to_seq`, forward only, and returns
    /// where it ended up. An ack for an older seq is a no-op rather than an
    /// error: a retried ack must be harmless.
    pub fn ack(&self, consumer: &str, up_to_seq: u64) -> Result<u64, InboxError> {
        check_consumer(consumer)?;
        std::fs::create_dir_all(&self.dir).map_err(|e| self.io(&e))?;
        let _lock = lock_dir(&self.dir, INBOX_LOCK).map_err(|e| self.io(&e))?;
        let mut cursors = self.read_cursors()?;
        let entry = cursors.consumers.entry(consumer.to_string()).or_insert(0);
        if up_to_seq > *entry {
            *entry = up_to_seq;
        }
        let moved = *entry;
        self.write_cursors(&cursors)?;
        Ok(moved)
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
        let (Some(first), Some(last)) = (self.first_event()?, self.last_event()?) else {
            return Ok(Depth {
                pending: 0,
                total: 0,
                cursor,
                last_received_at: None,
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
        })
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
    /// what makes it a retention candidate before anything unacked.
    fn min_cursor(&self) -> Result<u64, InboxError> {
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

    fn read_dedupe(&self) -> Result<Vec<String>, InboxError> {
        let path = self.dir.join(DEDUPE_FILE);
        match std::fs::read_to_string(&path) {
            Ok(raw) => Ok(raw
                .lines()
                .filter(|l| !l.trim().is_empty())
                .map(str::to_string)
                .collect()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
            Err(e) => Err(self.io(&e)),
        }
    }

    fn write_dedupe(&self, ids: &[String]) -> Result<(), InboxError> {
        let mut body = String::new();
        for id in ids {
            body.push_str(id);
            body.push('\n');
        }
        atomic_write_private(&self.dir.join(DEDUPE_FILE), body.as_bytes()).map_err(|e| self.io(&e))
    }

    fn read_cursors(&self) -> Result<Cursors, InboxError> {
        let path = self.dir.join(CURSORS_FILE);
        match std::fs::read_to_string(&path) {
            Ok(raw) => serde_yaml_ng::from_str(&raw)
                .map_err(|e| InboxError::Corrupt(path.display().to_string(), e.to_string())),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Cursors::default()),
            Err(e) => Err(self.io(&e)),
        }
    }

    fn write_cursors(&self, cursors: &Cursors) -> Result<(), InboxError> {
        let yaml = serde_yaml_ng::to_string(cursors)
            .map_err(|e| InboxError::Corrupt(self.path_str(CURSORS_FILE), e.to_string()))?;
        atomic_write_private(&self.dir.join(CURSORS_FILE), yaml.as_bytes()).map_err(|e| self.io(&e))
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

/// The serialized size of one event's line, including its newline. Used by
/// the size cap so the decision matches what the rewrite will produce rather
/// than what the current file happens to hold.
fn line_bytes(event: &InboxEvent) -> u64 {
    serde_json::to_string(event)
        .map(|s| s.len() as u64 + 1)
        .unwrap_or(0)
}

/// A consumer name is a machine-facing identifier, validated like a function
/// or account-field name. It becomes a key in `cursors.yaml`, so anything
/// looser would let a caller shape that file.
fn check_consumer(consumer: &str) -> Result<(), InboxError> {
    super::common::validate_snake_name(consumer)
        .map_err(|e| InboxError::Name(consumer.to_string(), e))
}
