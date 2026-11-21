//! The signed trigger state of a project (`.apb/trigger-state/project/
//! state.json`) and the per-incarnation trigger journal
//! (`.apb/trigger-state/log/<incarnation>.jsonl`). The state holds each
//! rule's position (`seen_through`); the journal holds, as codes and
//! condition marks only, why every event came out as it did. The journal
//! decides nothing: the position reads the state, and the line is appended
//! and synced before the state transaction that moves past the event it
//! records.

use std::collections::HashSet;
use std::io::Write as _;
use std::path::Path;
use std::{fs, io};

use serde::{Deserialize, Serialize};

use super::model::{RuleEntry, RuleSource, TriggerError};
use crate::scope::Scope;
use crate::signed_store::{StoreError, Unsigned};

/// The signed state of every rule of one project, bound to the project's
/// registered workspace id.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TriggerStateFile {
    pub schema: u32,
    pub scope: String,
    pub id: String,
    /// As `RulesFile.project`.
    #[serde(default)]
    pub project: String,
    #[serde(default)]
    pub rules: Vec<RuleState>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mac: Option<String>,
}

impl crate::signed_store::SignedDoc for TriggerStateFile {
    const MAC_DOMAIN: &'static str = "apb-trigger-state-v1";
    const SCHEMA: u32 = 1;
    const DIR: &'static str = "trigger-state";
    const ENTRIES_KEY: &'static str = "rules";

    fn fresh(scope: Scope, playbook: &str) -> Self {
        TriggerStateFile {
            schema: Self::SCHEMA,
            scope: scope.as_str().to_string(),
            id: playbook.to_string(),
            project: String::new(),
            rules: Vec::new(),
            mac: None,
        }
    }

    const NO_LINKS: bool = true;

    fn identity(&self) -> (&str, &str) {
        (&self.scope, &self.id)
    }

    fn belongs_to(&self, root: &Path) -> bool {
        crate::projects::registered_id(root).is_some_and(|id| id == self.project)
    }

    fn mac(&self) -> Option<&str> {
        self.mac.as_deref()
    }

    fn set_mac(&mut self, mac: Option<String>) {
        self.mac = mac;
    }

    fn set_schema(&mut self, schema: u32) {
        self.schema = schema;
    }
}

/// Which account a position belongs to (one per incarnation).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceId {
    pub connector: String,
    pub account: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RuleState {
    pub rule: String,
    /// The incarnation this state belongs to.
    pub incarnation: String,
    /// The source the position belongs to (one per incarnation).
    pub source: SourceId,
    /// The last inbox seq whose verdict line is in the journal.
    pub seen_through: u64,
}

/// The most events one pass of one rule evaluates, and the most starts it
/// makes (a fairness bound between rules; the next pass is 5 s away).
pub const MAX_EVENTS_PER_PASS: usize = 500;
pub const MAX_STARTS_PER_PASS: usize = 10;

/// The journal keeps the newest `LOG_KEEP` lines, rewritten through an
/// atomic write once it holds more than `LOG_REWRITE_AT`.
pub const LOG_KEEP: usize = 1000;
pub const LOG_REWRITE_AT: usize = 1200;

/// One line of a trigger journal: codes and safe structured metadata only,
/// from a closed set of fields; no field holds an engine sentence, a body
/// value or a param value.
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LogEntry {
    /// The stable line id: `<incarnation>:<seq>:<verdict>` for an event's
    /// line (`held` lines add `:<code>`, a `binding_changed` line
    /// `:<binding>` after its verdict), `<incarnation>:gap:<a>-<b>` and
    /// `<incarnation>:positioned:<head>` otherwise. A line with an id the
    /// file holds already is never appended again.
    pub id: String,
    /// When the line was written, in milliseconds since the Unix epoch
    /// (the unit of `run_started.ts` and of every sweep clock).
    pub at: u64,
    pub verdict: Verdict,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub seq: Option<u64>,
    /// A gap's range, or a `positioned` line's head twice.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub range: Option<(u64, u64)>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_id: Option<String>,
    /// The reason as a code, never a sentence.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub code: Option<String>,
    /// The name the code is about, taken from the rule itself, never from
    /// the event.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// A `binding_changed` line's digests: the one resolved and the one the
    /// approval held (digests, no value).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub binding: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub approved: Option<String>,
    /// Validator codes (V260 to V266) of a `check_rule` refusal.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub codes: Vec<String>,
    /// Per condition of the filter, in the rule's order: its index and
    /// whether it passed. Never the condition's literal nor the event's
    /// value.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub conditions: Vec<ConditionMark>,
}

#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConditionMark {
    pub index: u32,
    pub passed: bool,
}

// `TriggerVerdict` in the generated TypeScript: quality's comparison
// `Verdict` already holds the plain name in the generated module (the
// `EvidenceVerdict` and `ScenarioVerdict` renames are the same case).
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(rename = "TriggerVerdict"))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Verdict {
    Skipped,
    Held,
    Started,
    AlreadyStarted,
    Refused,
    Failed,
    Gap,
    Positioned,
}

/// The journal directory of the project at `root`, for the tests that
/// plant files in it; production reaches it through the descriptor walk.
#[cfg(test)]
fn log_dir(root: &Path) -> std::path::PathBuf {
    root.join(".apb").join("trigger-state").join("log")
}

/// The incarnation names one file under `log/`; a name that is not one
/// path segment would climb out of it, so it is refused.
fn checked_incarnation(incarnation: &str) -> io::Result<()> {
    if crate::registry::is_safe_segment(incarnation) {
        return Ok(());
    }
    Err(io::Error::new(
        io::ErrorKind::InvalidInput,
        format!("`{incarnation}` is not an incarnation"),
    ))
}

/// The most bytes a journal read takes: `LOG_REWRITE_AT` lines of a few
/// hundred bytes each fit many times over; a larger file is refused.
const JOURNAL_READ_MAX: u64 = 16 << 20;

// How many journal reads this thread made: a listing reads every journal
// once, and a test counts the reads. Compiled out of a release build.
#[cfg(any(test, feature = "testing"))]
thread_local! {
    static JOURNAL_READS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// How many journal reads this thread made since the last reset.
#[cfg(any(test, feature = "testing"))]
pub fn journal_reads_count() -> usize {
    JOURNAL_READS.with(|c| c.get())
}

/// Forgets the journal reads this thread made.
#[cfg(any(test, feature = "testing"))]
pub fn reset_journal_reads() {
    JOURNAL_READS.with(|c| c.set(0));
}

/// The journal file of one incarnation as bytes, read beneath `.apb` with
/// no link followed at any component (`fsutil::beneath`): `None` when it is
/// not there, a refusal when it cannot be read (a link, not a regular file, a
/// hard link, too large) or when a component of its path is refused.
fn read_journal(root: &Path, incarnation: &str) -> Result<Option<Vec<u8>>, JournalRefusal> {
    #[cfg(any(test, feature = "testing"))]
    JOURNAL_READS.with(|c| c.set(c.get() + 1));
    use crate::fsutil::beneath::{Beneath, CappedRead, ReadRefusal};
    let apb = root.join(".apb");
    let beneath = match Beneath::open(&apb) {
        Ok(beneath) => beneath,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(e) => {
            // A link at the anchor: the path reaches nothing below it.
            if std::fs::symlink_metadata(&apb).is_ok_and(|m| m.file_type().is_symlink()) {
                return Err(JournalRefusal::Component(".apb".to_string()));
            }
            return Err(JournalRefusal::Io(e.to_string()));
        }
    };
    let rel = format!("trigger-state/log/{incarnation}.jsonl");
    match beneath.read_regular_capped(&rel, JOURNAL_READ_MAX) {
        Ok(CappedRead::Read { bytes, .. }) => Ok(Some(bytes)),
        Ok(CappedRead::Refused(ReadRefusal::Missing)) => Ok(None),
        Ok(CappedRead::Refused(ReadRefusal::LinkAt { component })) => {
            Err(JournalRefusal::Component(format!(".apb/{component}")))
        }
        Ok(CappedRead::Refused(refusal)) => Err(JournalRefusal::Journal(refusal)),
        Err(e) => Err(JournalRefusal::Io(e.to_string())),
    }
}

/// Why a journal was not read: the journal itself was refused, or a
/// component of the path that reaches it was. The words of a refusal are
/// produced once, from this typed reason.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum JournalRefusal {
    /// The journal itself: too large, a link, a hard link, or not a
    /// regular file.
    Journal(crate::fsutil::beneath::ReadRefusal),
    /// A component of the path: a link at `.apb`, at `trigger-state` or
    /// at `log`, named.
    Component(String),
    /// The read failed another way.
    Io(String),
}

impl JournalRefusal {
    /// The words of the reason, without the remedy.
    pub fn reason_words(&self) -> String {
        match self {
            JournalRefusal::Journal(refusal) => refusal_words(refusal),
            JournalRefusal::Component(component) => format!("{component} is a link"),
            JournalRefusal::Io(text) => text.clone(),
        }
    }

    /// The refusal as an io error, for the callers that carry one.
    pub fn into_io(self) -> io::Error {
        match self {
            JournalRefusal::Journal(refusal) => io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "the trigger journal is refused: {}",
                    refusal_words(&refusal)
                ),
            ),
            JournalRefusal::Component(component) => io::Error::new(
                io::ErrorKind::InvalidData,
                format!("the trigger journal is refused: {component} is a link"),
            ),
            JournalRefusal::Io(text) => io::Error::other(text),
        }
    }
}

/// Why the journal was not read, in words: what a person's surface and a
/// sweep's refusal name.
fn refusal_words(refusal: &crate::fsutil::beneath::ReadRefusal) -> String {
    use crate::fsutil::beneath::ReadRefusal;
    match refusal {
        ReadRefusal::Missing => "not there".to_string(),
        ReadRefusal::Link => "a link".to_string(),
        ReadRefusal::LinkAt { component } => format!("{component} is a link"),
        ReadRefusal::NotRegular => "not a regular file".to_string(),
        ReadRefusal::HardLink => "more than one link".to_string(),
        ReadRefusal::TooLarge { size } => format!("{size} bytes, over the read cap"),
    }
}

/// Whether the journal of `incarnation` reads: the same bounded read
/// `journal_ids` makes, without its rewrite. A surface reports the refusal
/// and changes nothing.
pub fn journal_reads(root: &Path, incarnation: &str) -> Result<(), JournalRefusal> {
    checked_incarnation(incarnation).map_err(|e| JournalRefusal::Io(e.to_string()))?;
    read_journal(root, incarnation).map(|_| ())
}

/// The one sentence a refused journal is named with, wherever it is shown:
/// the problem once, then the remedy. A refusal of the journal itself is
/// remedied by removing the rule and saving it again, or by deleting the
/// file; a refusal at a component of its path is remedied by replacing that
/// link with a directory, never by a path through the link.
pub fn journal_refused_text(rule: &str, incarnation: &str, refusal: &JournalRefusal) -> String {
    match refusal {
        JournalRefusal::Component(component) => format!(
            "journal_refused: the trigger journal of `{rule}` cannot be read ({component} is a link); \
             replace the link at {component} with a directory"
        ),
        other => format!(
            "journal_refused: the trigger journal of `{rule}` cannot be read ({}); \
             remove the rule and save it again, or delete .apb/trigger-state/log/{incarnation}.jsonl",
            other.reason_words()
        ),
    }
}

/// The parseable entries of journal bytes, oldest first. A line that does
/// not parse (torn, or a field or verdict a later apb wrote) is skipped.
fn parse_entries(bytes: &[u8]) -> Vec<LogEntry> {
    bytes
        .split(|b| *b == b'\n')
        .filter(|line| !line.is_empty())
        .filter_map(|line| serde_json::from_str::<LogEntry>(std::str::from_utf8(line).ok()?).ok())
        .collect()
}

/// An io error in the `TriggerError` shape (the store's `Io`).
fn io_error(error: io::Error) -> TriggerError {
    TriggerError::Store(StoreError::Io(error))
}

/// A sync of one durability point (`file`, `log`, `trigger-state`, `apb`,
/// `state`), recorded by test builds, which can also fail one once (the
/// `versioning/current/fault.rs` pattern, at the sync itself).
fn sync_point(point: &'static str, sync: impl FnOnce() -> io::Result<()>) -> io::Result<()> {
    #[cfg(test)]
    {
        SYNCS.with(|s| s.borrow_mut().push(point));
        if FAIL_AT.with(|f| f.get()) == Some(point) {
            FAIL_AT.with(|f| f.set(None));
            return Err(io::Error::other(format!(
                "injected fault at the {point} sync"
            )));
        }
    }
    let _ = point;
    sync()
}

/// The ids the journal file holds, read once by a pass under the sweep
/// lock (the file only grows under it). This is where the rewrite to the
/// newest `LOG_KEEP` lines happens, when the file holds more than
/// `LOG_REWRITE_AT`: an id the rewrite dropped belongs to an event at or
/// below `seen_through`, which no pass evaluates again.
pub fn journal_ids(root: &Path, incarnation: &str) -> Result<HashSet<String>, JournalRefusal> {
    checked_incarnation(incarnation).map_err(|e| JournalRefusal::Io(e.to_string()))?;
    let entries = parse_entries(&read_journal(root, incarnation)?.unwrap_or_default());
    if entries.len() > LOG_REWRITE_AT {
        let keep = &entries[entries.len() - LOG_KEEP..];
        let mut bytes = Vec::new();
        for entry in keep {
            bytes.extend_from_slice(
                &serde_json::to_vec(entry).map_err(|e| JournalRefusal::Io(e.to_string()))?,
            );
            bytes.push(b'\n');
        }
        dir_swap_step();
        // The replacement is one rename beneath `.apb`, no link followed at
        // any component, and the directory is synced after the rename (I2):
        // the ids a pass relies on are durable with the file that holds
        // them, whatever a later append writes. The sync goes through the
        // walk's own descriptor, never a path reopened afterwards.
        let beneath = crate::fsutil::beneath::Beneath::open(&root.join(".apb"))
            .map_err(|e| JournalRefusal::Io(e.to_string()))?;
        beneath
            .write_new("trigger-state/log", &format!("{incarnation}.jsonl"), &bytes)
            .map_err(|e| JournalRefusal::Io(e.to_string()))?;
        pre_sync_step();
        sync_point("log", || beneath.sync_dir("trigger-state/log"))
            .map_err(|e| JournalRefusal::Io(e.to_string()))?;
        return Ok(keep.iter().map(|entry| entry.id.clone()).collect());
    }
    Ok(entries.into_iter().map(|entry| entry.id).collect())
}

/// Under the rule's sweep lock: opens the file beneath `.apb` with no link
/// followed (one descriptor, read and append), first repairs its tail,
/// then appends the lines whose id is not in `ids` (adding them), then
/// syncs the file and the whole directory chain it lives in. Whether the
/// file already existed, whether a tail needed repair and whether every
/// line id was new decide nothing about the directory syncs: presence and
/// matching ids prove that a line is there, never that it is durable (I2).
/// An error is returned before anything else is done, so the caller does
/// not move the position.
pub fn append(
    root: &Path,
    incarnation: &str,
    ids: &mut HashSet<String>,
    lines: &[LogEntry],
) -> io::Result<()> {
    checked_incarnation(incarnation)?;
    if lines.is_empty() {
        return Ok(());
    }
    let apb = root.join(".apb");
    // No component below the project is a link: the journal is reached
    // through one walk of the anchor, every directory opened with no link
    // followed, and the file opened from the descriptor it reached.
    if !apb.is_dir() {
        fs::create_dir(&apb)?;
    }
    let beneath = crate::fsutil::beneath::Beneath::open(&apb)?;
    beneath.mkdir_all("trigger-state/log")?;
    dir_swap_step();
    let mut opened = beneath.open_read_append(&format!("trigger-state/log/{incarnation}.jsonl"))?;
    // Once per id, within this call as across calls: a line with an id the
    // file holds already is never appended again.
    let mut batch: HashSet<String> = HashSet::new();
    let fresh: Vec<&LogEntry> = lines
        .iter()
        .filter(|line| !ids.contains(&line.id) && batch.insert(line.id.clone()))
        .collect();
    {
        let file = &mut opened.file;
        // The repair a crashed appender's tail needs comes first, so a
        // replayed line is never glued to a fragment (I2).
        crate::fsutil::repair_line_tail_file(file, JOURNAL_READ_MAX, |line| {
            serde_json::from_str::<LogEntry>(line).ok()
        })?;
        for line in &fresh {
            let mut bytes = serde_json::to_vec(line).map_err(io::Error::other)?;
            bytes.push(b'\n');
            file.write_all(&bytes)?;
        }
        file.flush()?;
        sync_point("file", || file.sync_all())?;
    }
    pre_sync_step();
    // The directory that holds the file is synced through the descriptor
    // the walk reached, never a path reopened after it: a directory
    // swapped in at `log` meanwhile is never synced in place of it. The
    // two directories above it are synced through their own walks, where
    // a link swapped in meanwhile is refused.
    sync_point("log", || opened.sync_dir())?;
    sync_point("trigger-state", || beneath.sync_dir("trigger-state"))?;
    sync_point("apb", || beneath.sync_dir(""))?;
    for line in fresh {
        ids.insert(line.id.clone());
    }
    Ok(())
}

/// The newest `limit` entries, newest last. This is the reader that
/// dedupes: a line whose id appeared earlier is dropped, and an
/// `already_started` line with the seq and run id of an earlier `started`
/// line is folded into it. A line that does not parse is skipped.
pub fn read(root: &Path, incarnation: &str, limit: usize) -> Vec<LogEntry> {
    read_with_refusal(root, incarnation, limit).0
}

/// The newest `limit` lines of the journal and the refusal that stopped
/// it, from one read: a listing reads every journal once, not twice.
pub fn read_with_refusal(
    root: &Path,
    incarnation: &str,
    limit: usize,
) -> (Vec<LogEntry>, Option<JournalRefusal>) {
    if checked_incarnation(incarnation).is_err() {
        return (Vec::new(), None);
    }
    // A journal that is refused (a link, not a regular file) reads empty,
    // and the refusal is carried beside it.
    let bytes = match read_journal(root, incarnation) {
        Ok(bytes) => bytes.unwrap_or_default(),
        Err(refusal) => return (Vec::new(), Some(refusal)),
    };
    (folded(&bytes, limit), None)
}

/// The lines the journal holds, newest last, with the duplicates folded
/// away.
fn folded(bytes: &[u8], limit: usize) -> Vec<LogEntry> {
    let mut kept: Vec<LogEntry> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    for entry in parse_entries(bytes) {
        // First occurrence wins: a line the file holds twice is shown once.
        if !seen.insert(entry.id.clone()) {
            continue;
        }
        // The start key answers a repeated start `already_started`: the
        // answer folds into the start it names, shown once.
        if entry.verdict == Verdict::AlreadyStarted
            && entry.seq.is_some()
            && entry.run_id.is_some()
            && kept.iter().any(|start| {
                start.verdict == Verdict::Started
                    && start.seq == entry.seq
                    && start.run_id == entry.run_id
            })
        {
            continue;
        }
        kept.push(entry);
    }
    let skip = kept.len().saturating_sub(limit);
    kept.into_iter().skip(skip).collect()
}

/// Positions the rule's state at `head`, by the pass only: the
/// `positioned` line is appended and synced, then one state transaction
/// sets `{ incarnation, source, seen_through: head }` only while the entry
/// is still missing or of another incarnation (compare and set inside the
/// transaction), replacing such an entry. `true` when it positioned,
/// `false` when an entry of this incarnation was already there. It removes
/// no file.
pub fn position_at_head(
    root: &Path,
    entry: &RuleEntry,
    head: u64,
    now: u64,
) -> Result<bool, TriggerError> {
    let incarnation = entry.incarnation.clone();
    let source = match &entry.rule.source {
        RuleSource::Inbox(inbox) => SourceId {
            connector: inbox.connector.clone(),
            account: inbox.account.clone(),
        },
    };
    // Line first (I2): an attempt record the next pass finds, whatever the
    // transaction below does.
    let line = LogEntry::for_position(&incarnation, now, head);
    let mut ids = journal_ids(root, &incarnation).map_err(|e| io_error(e.into_io()))?;
    append(root, &incarnation, &mut ids, &[line]).map_err(io_error)?;
    // The line is durable; the state transaction commits what it records.
    sync_point("state", || Ok(())).map_err(io_error)?;
    let project = crate::projects::registered_id(root).ok_or(TriggerError::Unregistered)?;
    crate::signed_store::transact(
        root,
        Scope::Project,
        "state",
        Unsigned::Refuse,
        |file: &mut TriggerStateFile| -> Result<(bool, bool), TriggerError> {
            // Every write stamps the project the file belongs to.
            file.project = project.clone();
            let state = RuleState {
                rule: entry.rule.name.clone(),
                incarnation,
                source,
                seen_through: head,
            };
            // Compare and set: only a missing entry or one of another
            // incarnation is positioned; an entry of this incarnation was
            // committed by the holder before, and it stands.
            match file.rules.iter_mut().find(|s| s.rule == state.rule) {
                Some(existing) if existing.incarnation == state.incarnation => Ok((false, false)),
                Some(existing) => {
                    *existing = state;
                    Ok((true, true))
                }
                None => {
                    file.rules.push(state);
                    Ok((true, true))
                }
            }
        },
    )
}

/// Whether the state needs `position_at_head` before a pass: no entry, or
/// one of another incarnation. Read only.
pub fn needs_position(file: &TriggerStateFile, entry: &RuleEntry) -> bool {
    match file.rules.iter().find(|s| s.rule == entry.rule.name) {
        Some(state) => state.incarnation != entry.incarnation,
        None => true,
    }
}

/// Inside a state transaction: removes the rule's entry (what `approve`,
/// `enable` and `remove` do); absent is a no-op.
pub fn delete_entry(file: &mut TriggerStateFile, rule: &str) {
    file.rules.retain(|s| s.rule != rule);
}

// The line constructors the engine pass and this module build lines with.

/// Whether `code` passes the closed pattern `[a-z0-9_]{1,64}`: the one
/// check a stored code and a rendered code share.
pub fn is_code_text(code: &str) -> bool {
    (1..=64).contains(&code.len())
        && code
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
}

/// Whether `name` passes what the code pattern allows, plus `.`.
fn is_name_text(name: &str) -> bool {
    (1..=64).contains(&name.len())
        && name
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' || c == '.')
}

/// The stored code: the code itself when it passes the closed pattern,
/// `refused_other` otherwise.
fn normalize_code(code: &str) -> String {
    if is_code_text(code) {
        code.to_string()
    } else {
        "refused_other".to_string()
    }
}

/// The verdict's word in an id, the same snake_case its JSON uses.
fn verdict_word(verdict: Verdict) -> &'static str {
    match verdict {
        Verdict::Skipped => "skipped",
        Verdict::Held => "held",
        Verdict::Started => "started",
        Verdict::AlreadyStarted => "already_started",
        Verdict::Refused => "refused",
        Verdict::Failed => "failed",
        Verdict::Gap => "gap",
        Verdict::Positioned => "positioned",
    }
}

/// A line with only its id, time and verdict set.
fn line(id: String, at: u64, verdict: Verdict) -> LogEntry {
    LogEntry {
        id,
        at,
        verdict,
        seq: None,
        range: None,
        run_id: None,
        code: None,
        name: None,
        binding: None,
        approved: None,
        codes: Vec::new(),
        conditions: Vec::new(),
    }
}

impl LogEntry {
    /// An event's line, by the pass: the id `<incarnation>:<seq>:<verdict>`,
    /// a `held` line adding `:<code>`. The code is checked against
    /// `[a-z0-9_]{1,64}`, and anything else is stored as `refused_other`;
    /// the id carries the stored code, so a repeat of the same event keeps
    /// the same id. The name is checked like the code plus `.`; one that
    /// does not pass is dropped.
    pub fn for_event(
        incarnation: &str,
        seq: u64,
        at: u64,
        verdict: Verdict,
        code: Option<&str>,
        name: Option<&str>,
    ) -> LogEntry {
        let code = code.map(normalize_code);
        let name = name.filter(|name| is_name_text(name)).map(str::to_string);
        let id = match (&code, verdict) {
            (Some(code), Verdict::Held) => format!("{incarnation}:{seq}:held:{code}"),
            _ => format!("{incarnation}:{seq}:{}", verdict_word(verdict)),
        };
        LogEntry {
            seq: Some(seq),
            code,
            name,
            ..line(id, at, verdict)
        }
    }

    /// A `gap` line for the events `[from, to]` retention removed.
    pub fn for_gap(incarnation: &str, at: u64, from: u64, to: u64) -> LogEntry {
        let mut entry = line(format!("{incarnation}:gap:{from}-{to}"), at, Verdict::Gap);
        entry.range = Some((from, to));
        entry
    }

    /// The `positioned` line of a position at `head`.
    pub fn for_position(incarnation: &str, at: u64, head: u64) -> LogEntry {
        let mut entry = line(
            format!("{incarnation}:positioned:{head}"),
            at,
            Verdict::Positioned,
        );
        entry.range = Some((head, head));
        entry
    }

    /// The `binding_changed` line of a start attempted under a changed
    /// approval binding: `held` with the code, `binding` the one resolved
    /// and `approved` the approval's, id
    /// `<incarnation>:<seq>:binding_changed:<binding>` (the seq and the
    /// binding are both stable across a crash repeat).
    pub fn for_binding_changed(
        incarnation: &str,
        seq: u64,
        at: u64,
        binding: &str,
        approved: &str,
    ) -> LogEntry {
        let mut entry = line(
            format!("{incarnation}:{seq}:binding_changed:{binding}"),
            at,
            Verdict::Held,
        );
        entry.seq = Some(seq);
        entry.code = Some("binding_changed".to_string());
        entry.binding = Some(binding.to_string());
        entry.approved = Some(approved.to_string());
        entry
    }
}

// The sync points the test build records, and the one the next call fails
// at (the `versioning/current/fault.rs` pattern, at the sync itself).
#[cfg(test)]
thread_local! {
    static SYNCS: std::cell::RefCell<Vec<&'static str>> =
        const { std::cell::RefCell::new(Vec::new()) };
    static FAIL_AT: std::cell::Cell<Option<&'static str>> =
        const { std::cell::Cell::new(None) };
}

/// Makes the next sync of `point` fail once (`None` clears it), as a power
/// cut at that sync would.
#[cfg(test)]
pub(crate) fn fail_sync_at(point: Option<&'static str>) {
    FAIL_AT.with(|f| f.set(point));
}

/// The sync points recorded so far on this thread, cleared.
#[cfg(test)]
pub(crate) fn take_syncs() -> Vec<&'static str> {
    SYNCS.take()
}

#[cfg(test)]
thread_local! {
    /// What runs between a journal operation's checks and its first use of
    /// the filesystem, on this thread: a race test's swap, compiled out of
    /// a release build.
    static DIR_SWAP: std::cell::RefCell<Option<Box<dyn FnOnce()>>> =
        const { std::cell::RefCell::new(None) };
}

/// Plants the step the next journal operation on this thread runs after
/// its checks and before it touches the filesystem.
#[cfg(test)]
pub(crate) fn plant_dir_swap(step: Box<dyn FnOnce()>) {
    DIR_SWAP.with(|s| *s.borrow_mut() = Some(step));
}

/// Runs the planted step, once.
#[cfg(test)]
fn dir_swap_step() {
    if let Some(step) = DIR_SWAP.with(|s| s.borrow_mut().take()) {
        step();
    }
}

#[cfg(not(test))]
fn dir_swap_step() {}

#[cfg(test)]
thread_local! {
    /// What runs inside a journal operation after the file was opened or
    /// replaced and before the directory syncs, on this thread: a race
    /// test's plant, compiled out of a release build.
    static PRE_SYNC: std::cell::RefCell<Option<Box<dyn FnOnce()>>> =
        const { std::cell::RefCell::new(None) };
}

/// Plants the step the next journal operation on this thread runs after
/// its write and before its syncs.
#[cfg(test)]
pub(crate) fn plant_pre_sync(step: Box<dyn FnOnce()>) {
    PRE_SYNC.with(|s| *s.borrow_mut() = Some(step));
}

/// Runs the planted step, once.
#[cfg(test)]
fn pre_sync_step() {
    if let Some(step) = PRE_SYNC.with(|s| s.borrow_mut().take()) {
        step();
    }
}

#[cfg(not(test))]
fn pre_sync_step() {}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::signed_store::{self, SignedDoc, StoreError, Unsigned};
    use crate::trigger::model::{
        InboxSource, Rule, RuleEntry, RuleLimits, RulePlaybook, RuleScope, RuleSource, TriggerError,
    };

    /// An entry of one incarnation (the approval a pass needs is not this
    /// file's business).
    fn entry(incarnation: &str) -> RuleEntry {
        RuleEntry {
            rule: Rule {
                name: "issues".to_string(),
                playbook: RulePlaybook {
                    id: "demo-report".to_string(),
                    version: "1.0.0".to_string(),
                    scope: RuleScope::Project,
                },
                source: RuleSource::Inbox(InboxSource {
                    connector: "github".to_string(),
                    account: "acme".to_string(),
                    types: Vec::new(),
                    conditions: Vec::new(),
                    key: "{{ event.id }}".to_string(),
                }),
                params: Default::default(),
                limits: RuleLimits { max_per_hour: 3 },
                budget: None,
                external_data: true,
                require_ready: true,
                notify: None,
            },
            incarnation: incarnation.to_string(),
            enabled: true,
            approval: None,
            created_at: 1,
            updated_at: 1,
            updated_by: "terminal".to_string(),
        }
    }

    /// Writes `file` over the state path, signed with this installation's
    /// key: the shape of a file another project's store wrote.
    fn write_signed_state(root: &Path, file: &TriggerStateFile) {
        let path = signed_store::file::<TriggerStateFile>(root, Scope::Project, "state");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let mut value = serde_json::to_value(file).unwrap();
        let covered = signed_store::bytes_of(&value).unwrap();
        let mac = crate::run_origin::tag(TriggerStateFile::MAC_DOMAIN, &covered).unwrap();
        value
            .as_object_mut()
            .unwrap()
            .insert("mac".into(), serde_json::Value::String(mac));
        let bytes = serde_json::to_vec_pretty(&value).unwrap();
        std::fs::write(path, bytes).unwrap();
    }

    /// The log file of `incarnation` under the project at `root`.
    fn log_path(root: &Path, incarnation: &str) -> std::path::PathBuf {
        root.join(".apb")
            .join("trigger-state")
            .join("log")
            .join(format!("{incarnation}.jsonl"))
    }

    /// The complete lines the journal file holds.
    fn line_count(path: &Path) -> usize {
        std::fs::read(path)
            .unwrap()
            .into_iter()
            .filter(|b| *b == b'\n')
            .count()
    }

    /// The journal file with `lines` as its only content.
    fn write_log(root: &Path, incarnation: &str, lines: &[&LogEntry]) {
        let path = log_path(root, incarnation);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let mut bytes = Vec::new();
        for line in lines {
            bytes.extend_from_slice(&serde_json::to_vec(line).unwrap());
            bytes.push(b'\n');
        }
        std::fs::write(path, bytes).unwrap();
    }

    /// Writes the journal file with `lines`, the last without its newline
    /// (a whole record a crash left unterminated).
    fn write_log_ends_without_newline(root: &Path, incarnation: &str, lines: &[&LogEntry]) {
        let path = log_path(root, incarnation);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let mut bytes = Vec::new();
        for line in lines {
            bytes.extend_from_slice(&serde_json::to_vec(line).unwrap());
            bytes.push(b'\n');
        }
        bytes.pop();
        std::fs::write(path, bytes).unwrap();
    }

    /// Writes the journal file with `lines` and a cut fragment of `torn`
    /// after them (a partial record a crash left).
    fn write_partial_tail(root: &Path, incarnation: &str, lines: &[&LogEntry], torn: &LogEntry) {
        let path = log_path(root, incarnation);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let mut bytes = Vec::new();
        for line in lines {
            bytes.extend_from_slice(&serde_json::to_vec(line).unwrap());
            bytes.push(b'\n');
        }
        let whole = serde_json::to_vec(torn).unwrap();
        bytes.extend_from_slice(&whole[..whole.len() / 2]);
        std::fs::write(path, bytes).unwrap();
    }

    /// The state transaction ran after the line: the signed state carries
    /// `seen_through`, the source of the entry and the project stamp, and
    /// an entry of another incarnation is replaced.
    #[test]
    fn position_at_head_replaces_another_incarnation() {
        let _cfg = crate::test_config();
        let root = tempfile::tempdir().unwrap();
        crate::projects::bind_id(root.path()).unwrap();
        signed_store::transact(
            root.path(),
            Scope::Project,
            "state",
            Unsigned::Replace,
            |file: &mut TriggerStateFile| -> Result<((), bool), TriggerError> {
                file.project = crate::projects::registered_id(root.path()).unwrap();
                file.rules.push(RuleState {
                    rule: "issues".to_string(),
                    incarnation: "old".to_string(),
                    source: SourceId {
                        connector: "github".into(),
                        account: "gone".into(),
                    },
                    seen_through: 5,
                });
                Ok(((), true))
            },
        )
        .unwrap();
        position_at_head(root.path(), &entry("new"), 7, 100).unwrap();
        let loaded =
            signed_store::read::<TriggerStateFile>(root.path(), Scope::Project, "state").unwrap();
        assert!(
            loaded.signed,
            "the write is signed and belongs to this project"
        );
        assert_eq!(
            loaded.file.rules.len(),
            1,
            "the old entry is replaced, not kept beside"
        );
        let state = &loaded.file.rules[0];
        assert_eq!(state.incarnation, "new");
        assert_eq!(state.seen_through, 7);
        assert_eq!(state.source.connector, "github");
        assert_eq!(state.source.account, "acme");
        assert_eq!(
            loaded.file.project,
            crate::projects::registered_id(root.path()).unwrap(),
            "every write stamps the project"
        );
        assert!(
            log_path(root.path(), "new").exists(),
            "the position removes no file"
        );
    }

    /// Line first (I2): the line is in the journal even when the state
    /// transaction cannot run, and the state it could not move is
    /// untouched.
    #[test]
    fn position_at_head_writes_the_line_first() {
        let _cfg = crate::test_config();
        let root = tempfile::tempdir().unwrap();
        crate::projects::bind_id(root.path()).unwrap();
        // Another project's store wrote its file here: the state cannot be
        // written until a person removes it.
        let other = tempfile::tempdir().unwrap();
        let other_id = crate::projects::bind_id(other.path()).unwrap();
        let mut forged = TriggerStateFile::fresh(Scope::Project, "state");
        forged.project = other_id;
        write_signed_state(root.path(), &forged);
        let err = position_at_head(root.path(), &entry("inc1"), 4, 9).unwrap_err();
        assert!(
            matches!(err, TriggerError::Store(StoreError::Unverified(_))),
            "{err:?}"
        );
        let entries = read(root.path(), "inc1", 10);
        assert_eq!(
            entries.len(),
            1,
            "the line is there although the transaction failed"
        );
        assert_eq!(entries[0].id, "inc1:positioned:4");
        assert_eq!(entries[0].verdict, Verdict::Positioned);
        assert_eq!(entries[0].range, Some((4, 4)));
        assert_eq!(entries[0].at, 9);
        let loaded =
            signed_store::read::<TriggerStateFile>(root.path(), Scope::Project, "state").unwrap();
        assert!(!loaded.signed, "the forged state was not replaced");
    }

    /// The lines are attempt records, the signed state the committed
    /// result: a crashed attempt leaves its line, the next pass writes
    /// another, readers show the last.
    #[test]
    fn read_shows_the_last_position_attempt() {
        let _cfg = crate::test_config();
        let root = tempfile::tempdir().unwrap();
        crate::projects::bind_id(root.path()).unwrap();
        // A crashed attempt: its line is in the journal, its transaction
        // never ran.
        let mut ids = journal_ids(root.path(), "inc1").unwrap();
        append(
            root.path(),
            "inc1",
            &mut ids,
            &[LogEntry::for_position("inc1", 8, 3)],
        )
        .unwrap();
        let before =
            signed_store::read::<TriggerStateFile>(root.path(), Scope::Project, "state").unwrap();
        assert!(before.file.rules.is_empty());
        // The next pass positions at the head it reads then.
        position_at_head(root.path(), &entry("inc1"), 5, 10).unwrap();
        let entries = read(root.path(), "inc1", 10);
        assert_eq!(
            entries.iter().map(|l| l.id.as_str()).collect::<Vec<_>>(),
            vec!["inc1:positioned:3", "inc1:positioned:5"],
            "both attempts are kept, newest last"
        );
        let state =
            signed_store::read::<TriggerStateFile>(root.path(), Scope::Project, "state").unwrap();
        assert_eq!(
            state.file.rules[0].seen_through, 5,
            "readers show the last attempt"
        );
    }

    /// A `where` condition's literal and the event's type never enter the
    /// line: only each condition's index and whether it passed.
    #[test]
    fn a_journal_line_holds_condition_marks_only() {
        let mut line = LogEntry::for_event("inc1", 12, 5, Verdict::Skipped, Some("where"), None);
        line.conditions = vec![
            ConditionMark {
                index: 0,
                passed: true,
            },
            ConditionMark {
                index: 1,
                passed: false,
            },
        ];
        let text = serde_json::to_string(&line).unwrap();
        assert!(
            text.contains(r#""conditions":[{"index":0,"passed":true},{"index":1,"passed":false}]"#),
            "{text}"
        );
        // The literal of the rule's `where` and the event's type, which the
        // dry test's response holds, are nowhere in the stored line.
        assert!(!text.contains("urgent"), "{text}");
        assert!(!text.contains("pull_request"), "{text}");
        let value: serde_json::Value = serde_json::from_str(&text).unwrap();
        let keys: Vec<&str> = value
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        assert!(
            keys.iter()
                .all(|k| matches!(*k, "id" | "at" | "verdict" | "seq" | "code" | "conditions")),
            "only the closed set of fields: {keys:?}"
        );
    }

    /// No entry, or one of another incarnation, needs a position; an entry
    /// of the rule's own incarnation does not.
    #[test]
    fn needs_position_for_a_missing_or_foreign_state() {
        let e = entry("inc2");
        let mut file = TriggerStateFile::fresh(Scope::Project, "state");
        assert!(needs_position(&file, &e), "no entry");
        file.rules.push(RuleState {
            rule: "issues".to_string(),
            incarnation: "old".to_string(),
            source: SourceId::default(),
            seen_through: 1,
        });
        assert!(needs_position(&file, &e), "an entry of another incarnation");
        file.rules[0].incarnation = "inc2".to_string();
        assert!(!needs_position(&file, &e));
        // An entry of another rule does not satisfy this one.
        file.rules[0].rule = "other".to_string();
        assert!(needs_position(&file, &e), "no entry for this rule");
    }

    /// The repair a crashed appender's tail needs comes before the next
    /// append: a whole tail keeps its newline, a partial one is cut, and a
    /// replayed line is never glued to a fragment.
    #[test]
    fn a_torn_tail_is_repaired_before_the_next_append() {
        let root = tempfile::tempdir().unwrap();
        let first = LogEntry::for_event("inc1", 1, 1, Verdict::Skipped, Some("type"), None);
        let gap = LogEntry::for_gap("inc1", 2, 2, 3);
        // A whole final record the crash left without its newline.
        write_log_ends_without_newline(root.path(), "inc1", &[&first, &gap]);
        let mut ids = journal_ids(root.path(), "inc1").unwrap();
        append(
            root.path(),
            "inc1",
            &mut ids,
            &[LogEntry::for_event(
                "inc1",
                4,
                4,
                Verdict::Started,
                None,
                None,
            )],
        )
        .unwrap();
        let got = read(root.path(), "inc1", 10);
        assert_eq!(
            got.iter().map(|l| l.id.as_str()).collect::<Vec<_>>(),
            vec![first.id.as_str(), gap.id.as_str(), "inc1:4:started"],
            "the whole tail kept its newline; the new line is not glued to it"
        );
        // A partial final record is cut to the last complete line.
        write_partial_tail(root.path(), "inc1", &[&first, &gap], &gap);
        let mut ids = journal_ids(root.path(), "inc1").unwrap();
        append(
            root.path(),
            "inc1",
            &mut ids,
            &[LogEntry::for_event(
                "inc1",
                5,
                5,
                Verdict::Failed,
                Some("rate_cap"),
                None,
            )],
        )
        .unwrap();
        let got = read(root.path(), "inc1", 10);
        assert_eq!(
            got.iter().map(|l| l.id.as_str()).collect::<Vec<_>>(),
            vec![first.id.as_str(), gap.id.as_str(), "inc1:5:failed"],
            "the partial tail is cut, the new line is appended whole"
        );
    }

    /// A state file another project's store wrote does not belong here: it
    /// reads unsigned and every write refuses it.
    #[test]
    fn a_state_file_of_another_project_is_refused() {
        let _cfg = crate::test_config();
        let root = tempfile::tempdir().unwrap();
        crate::projects::bind_id(root.path()).unwrap();
        // The state this project's own pass wrote verifies.
        position_at_head(root.path(), &entry("inc1"), 2, 3).unwrap();
        assert!(
            signed_store::read::<TriggerStateFile>(root.path(), Scope::Project, "state")
                .unwrap()
                .signed
        );
        // Another registered project's file: the MAC verifies, the project
        // does not.
        let other = tempfile::tempdir().unwrap();
        let other_id = crate::projects::bind_id(other.path()).unwrap();
        let mut forged = TriggerStateFile::fresh(Scope::Project, "state");
        forged.project = other_id;
        forged.rules.push(RuleState {
            rule: "issues".to_string(),
            incarnation: "x".to_string(),
            source: SourceId::default(),
            seen_through: 1,
        });
        write_signed_state(root.path(), &forged);
        let loaded =
            signed_store::read::<TriggerStateFile>(root.path(), Scope::Project, "state").unwrap();
        assert!(
            !loaded.signed,
            "another project's file does not belong here"
        );
        let err = position_at_head(root.path(), &entry("inc1"), 4, 5).unwrap_err();
        assert!(
            matches!(err, TriggerError::Store(StoreError::Unverified(_))),
            "{err:?}"
        );
    }

    /// The rewrite at the start of a pass keeps the newest `LOG_KEEP`
    /// lines once the file holds more than `LOG_REWRITE_AT`.
    #[test]
    fn the_journal_keeps_the_newest_thousand() {
        let root = tempfile::tempdir().unwrap();
        let mut ids = HashSet::new();
        let lines: Vec<LogEntry> = (0..LOG_REWRITE_AT as u64 + 20)
            .map(|i| LogEntry::for_event("inc1", i, i, Verdict::Skipped, Some("type"), None))
            .collect();
        append(root.path(), "inc1", &mut ids, &lines).unwrap();
        let path = log_path(root.path(), "inc1");
        assert_eq!(line_count(&path), LOG_REWRITE_AT + 20);
        let ids = journal_ids(root.path(), "inc1").unwrap();
        assert_eq!(line_count(&path), LOG_KEEP, "rewritten to LOG_KEEP");
        assert_eq!(ids.len(), LOG_KEEP);
        assert!(!ids.contains("inc1:0:skipped"), "the oldest was dropped");
        assert!(ids.contains(&format!("inc1:{}:skipped", LOG_REWRITE_AT + 19)));
        let got = read(root.path(), "inc1", 10_000);
        assert_eq!(got.len(), LOG_KEEP);
        assert_eq!(got[0].seq, Some(220), "the newest kept, oldest first");
        assert_eq!(got.last().unwrap().seq, Some(LOG_REWRITE_AT as u64 + 19));
    }

    /// A line that does not parse is skipped, by the reader and by the id
    /// read, never shown and never blocking an append.
    #[test]
    fn a_torn_line_is_skipped() {
        let root = tempfile::tempdir().unwrap();
        let first = LogEntry::for_event("inc1", 1, 1, Verdict::Skipped, Some("type"), None);
        let path = log_path(root.path(), "inc1");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let mut bytes = serde_json::to_vec(&first).unwrap();
        bytes.push(b'\n');
        bytes.extend_from_slice(br#"{"id":"torn","at":2,"verd"#);
        std::fs::write(&path, bytes).unwrap();
        let got = read(root.path(), "inc1", 10);
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].id, first.id);
        let ids = journal_ids(root.path(), "inc1").unwrap();
        assert_eq!(ids, HashSet::from([first.id.clone()]));
    }

    /// A verdict a later apb wrote is one this build does not know: the
    /// line is skipped.
    #[test]
    fn an_unknown_verdict_is_skipped() {
        let root = tempfile::tempdir().unwrap();
        let first = LogEntry::for_event("inc1", 1, 1, Verdict::Skipped, Some("type"), None);
        let last = LogEntry::for_event("inc1", 3, 3, Verdict::Started, None, None);
        let path = log_path(root.path(), "inc1");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let mut bytes = serde_json::to_vec(&first).unwrap();
        bytes.push(b'\n');
        bytes.extend_from_slice(br#"{"id":"inc1:2:aligned","at":2,"verdict":"aligned"}"#);
        bytes.push(b'\n');
        bytes.extend_from_slice(&serde_json::to_vec(&last).unwrap());
        bytes.push(b'\n');
        std::fs::write(&path, bytes).unwrap();
        let got = read(root.path(), "inc1", 10);
        assert_eq!(
            got.iter().map(|l| l.id.as_str()).collect::<Vec<_>>(),
            vec![first.id.as_str(), last.id.as_str()]
        );
        assert_eq!(journal_ids(root.path(), "inc1").unwrap().len(), 2);
    }

    /// A line whose id the journal holds is never appended again, within
    /// one call or across calls.
    #[test]
    fn append_skips_ids_already_there() {
        let root = tempfile::tempdir().unwrap();
        let mut ids = journal_ids(root.path(), "inc1").unwrap();
        let line = LogEntry::for_event("inc1", 1, 1, Verdict::Skipped, Some("type"), None);
        append(root.path(), "inc1", &mut ids, &[line.clone(), line.clone()]).unwrap();
        assert_eq!(
            line_count(&log_path(root.path(), "inc1")),
            1,
            "once within one call"
        );
        assert_eq!(ids.len(), 1);
        append(root.path(), "inc1", &mut ids, &[line]).unwrap();
        assert_eq!(
            line_count(&log_path(root.path(), "inc1")),
            1,
            "not again across calls"
        );
        assert_eq!(ids.len(), 1);
    }

    /// The reader drops a line whose id appeared earlier and folds an
    /// `already_started` answer into the start it names; one about another
    /// run is shown.
    #[test]
    fn read_drops_a_repeated_id_and_folds_already_started() {
        let root = tempfile::tempdir().unwrap();
        let mut started = LogEntry::for_event("inc1", 5, 5, Verdict::Started, None, None);
        started.run_id = Some("r1".to_string());
        let mut repeat = started.clone();
        repeat.at = 6;
        let mut answer = LogEntry::for_event("inc1", 5, 7, Verdict::AlreadyStarted, None, None);
        answer.run_id = Some("r1".to_string());
        let mut other = LogEntry::for_event("inc1", 6, 8, Verdict::AlreadyStarted, None, None);
        other.run_id = Some("r2".to_string());
        write_log(root.path(), "inc1", &[&started, &repeat, &answer, &other]);
        let got = read(root.path(), "inc1", 10);
        assert_eq!(
            got.iter().map(|l| l.id.as_str()).collect::<Vec<_>>(),
            vec![started.id.as_str(), other.id.as_str()],
            "the repeated id is dropped, the answer folded, the other run's answer shown"
        );
        // The newest `limit` entries, newest last.
        let got = read(root.path(), "inc1", 1);
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].id, other.id);
    }

    /// A code outside `[a-z0-9_]{1,64}` is stored as `refused_other` (the
    /// id carries the stored code); a name is checked like the code plus
    /// `.`, and one that does not pass is dropped.
    #[test]
    fn a_code_outside_the_closed_set_is_refused_other() {
        let ok = LogEntry::for_event(
            "inc1",
            1,
            1,
            Verdict::Skipped,
            Some("param_too_long"),
            Some("body.path_2"),
        );
        assert_eq!(ok.code.as_deref(), Some("param_too_long"));
        assert_eq!(ok.name.as_deref(), Some("body.path_2"));
        assert_eq!(ok.id, "inc1:1:skipped");
        let bad = LogEntry::for_event(
            "inc1",
            2,
            2,
            Verdict::Refused,
            Some("no params given"),
            None,
        );
        assert_eq!(bad.code.as_deref(), Some("refused_other"));
        assert_eq!(bad.id, "inc1:2:refused");
        let long = LogEntry::for_event("inc1", 3, 3, Verdict::Held, Some(&"a".repeat(65)), None);
        assert_eq!(long.code.as_deref(), Some("refused_other"));
        assert_eq!(
            long.id, "inc1:3:held:refused_other",
            "the id carries the stored code, so a repeat keeps its id"
        );
        let edge = LogEntry::for_event("inc1", 4, 4, Verdict::Held, Some(&"a".repeat(64)), None);
        assert_eq!(edge.code.as_deref(), Some("a".repeat(64).as_str()));
        let bad_name = LogEntry::for_event(
            "inc1",
            5,
            5,
            Verdict::Refused,
            Some("param_too_long"),
            Some("../etc/passwd"),
        );
        assert_eq!(
            bad_name.name, None,
            "a name outside the pattern is never stored"
        );
    }

    /// The first line is durable before the state: the file, `log/`,
    /// `.apb/trigger-state` (and `.apb` when the append created it) sync
    /// in that order, before the state transaction runs.
    #[test]
    fn the_first_line_is_durable_before_the_state() {
        let _cfg = crate::test_config();
        let root = tempfile::tempdir().unwrap();
        crate::projects::bind_id(root.path()).unwrap();
        take_syncs();
        position_at_head(root.path(), &entry("inc1"), 4, 9).unwrap();
        assert_eq!(
            take_syncs(),
            vec!["file", "log", "trigger-state", "apb", "state"],
            "each new directory entry durable, in order, before the state commit"
        );
        let state =
            signed_store::read::<TriggerStateFile>(root.path(), Scope::Project, "state").unwrap();
        assert_eq!(state.file.rules[0].seen_through, 4);
    }

    /// A sync that fails returns the error before anything else is done:
    /// no later sync, and the caller does not move the position. The line
    /// itself may be in the file; the ids make the next pass a no-op.
    #[test]
    fn a_failed_sync_returns_an_error_before_anything_else() {
        let _cfg = crate::test_config();
        let root = tempfile::tempdir().unwrap();
        crate::projects::bind_id(root.path()).unwrap();
        take_syncs();
        fail_sync_at(Some("file"));
        let err = position_at_head(root.path(), &entry("inc1"), 3, 4).unwrap_err();
        fail_sync_at(None);
        assert!(
            matches!(err, TriggerError::Store(StoreError::Io(_))),
            "{err:?}"
        );
        assert_eq!(
            take_syncs(),
            vec!["file"],
            "the error came before anything else"
        );
        let state =
            signed_store::read::<TriggerStateFile>(root.path(), Scope::Project, "state").unwrap();
        assert!(state.file.rules.is_empty(), "the position did not move");
        assert!(
            read(root.path(), "inc1", 10)
                .iter()
                .any(|l| l.id == "inc1:positioned:3"),
            "the written line is there, so the next pass repeats under the same id"
        );
    }

    /// A replayed line (its id already in the file after a sync that
    /// failed) is synced again before the call answers: an existing id
    /// proves the line is there, not that it is durable (I2).
    #[test]
    fn a_replayed_line_is_synced_before_the_state() {
        let root = tempfile::tempdir().unwrap();
        let line = LogEntry::for_event("inc1", 1, 1, Verdict::Skipped, Some("type"), None);
        let mut ids = journal_ids(root.path(), "inc1").unwrap();
        fail_sync_at(Some("file"));
        append(root.path(), "inc1", &mut ids, std::slice::from_ref(&line)).unwrap_err();
        fail_sync_at(None);
        // The next pass reads the id the failed call wrote.
        let mut ids = journal_ids(root.path(), "inc1").unwrap();
        assert!(ids.contains(&line.id), "the line is in the file");
        take_syncs();
        append(root.path(), "inc1", &mut ids, &[line]).unwrap();
        assert_eq!(
            take_syncs(),
            vec!["file", "log", "trigger-state", "apb"],
            "the replay syncs the file and every directory an earlier call may have created"
        );
        assert_eq!(
            line_count(&log_path(root.path(), "inc1")),
            1,
            "not written twice"
        );
    }

    /// Directory durability does not depend on what a retry writes (I2):
    /// a first positioning whose `log/` sync failed, then a retry for
    /// another event (a different line id, the file already there) syncs
    /// the file and the whole directory chain before the state
    /// transaction commits the position.
    #[test]
    fn a_retry_after_a_failed_directory_sync_still_syncs_the_directories() {
        let _cfg = crate::test_config();
        let root = tempfile::tempdir().unwrap();
        crate::projects::bind_id(root.path()).unwrap();
        // The first positioning: its line is in the file, its `log/` sync
        // failed, its transaction never ran.
        fail_sync_at(Some("log"));
        let err = position_at_head(root.path(), &entry("inc1"), 3, 4).unwrap_err();
        fail_sync_at(None);
        assert!(
            matches!(err, TriggerError::Store(StoreError::Io(_))),
            "{err:?}"
        );
        assert!(
            !read(root.path(), "inc1", 10).is_empty(),
            "the line of the failed attempt is in the file"
        );
        // Another event arrived: the retry positions at the new head, a
        // line id the file does not hold yet.
        take_syncs();
        position_at_head(root.path(), &entry("inc1"), 5, 6).unwrap();
        assert_eq!(
            take_syncs(),
            vec!["file", "log", "trigger-state", "apb", "state"],
            "the retry syncs the file and every directory before the commit"
        );
        let state =
            signed_store::read::<TriggerStateFile>(root.path(), Scope::Project, "state").unwrap();
        assert_eq!(state.file.rules[0].seen_through, 5);
    }

    /// A compaction syncs the journal directory after its rename (I2), and
    /// a fresh append after it syncs the file and the directory chain
    /// again: neither the replacement nor what follows it relies on an
    /// unsynced directory entry.
    #[test]
    fn a_compaction_syncs_the_directory_after_the_rename() {
        let root = tempfile::tempdir().unwrap();
        let mut ids = HashSet::new();
        let lines: Vec<LogEntry> = (0..LOG_REWRITE_AT as u64 + 20)
            .map(|i| LogEntry::for_event("inc1", i, i, Verdict::Skipped, Some("type"), None))
            .collect();
        append(root.path(), "inc1", &mut ids, &lines).unwrap();
        take_syncs();
        // The rewrite at the start of a pass.
        let ids = journal_ids(root.path(), "inc1").unwrap();
        assert_eq!(ids.len(), LOG_KEEP, "rewritten to LOG_KEEP");
        assert_eq!(
            take_syncs(),
            vec!["log"],
            "the directory synced after the rename"
        );
        // A fresh append after the compaction: the whole chain again.
        append(
            root.path(),
            "inc1",
            &mut ids.clone(),
            &[LogEntry::for_event(
                "inc1",
                LOG_REWRITE_AT as u64 + 20,
                1,
                Verdict::Skipped,
                Some("type"),
                None,
            )],
        )
        .unwrap();
        assert_eq!(
            take_syncs(),
            vec!["file", "log", "trigger-state", "apb"],
            "the append after the compaction syncs the file and the directories"
        );
    }

    /// A journal over the read cap is refused by the repair through its
    /// own descriptor: the append refuses, and the oversized file is not
    /// read whole to get there.
    #[test]
    fn a_tail_over_the_read_cap_is_refused_not_read() {
        let root = tempfile::tempdir().unwrap();
        let path = log_dir(root.path()).join("inc1.jsonl");
        std::fs::create_dir_all(log_dir(root.path())).unwrap();
        // Over the cap, and torn: a repair that read whole would read it
        // all and complete or cut the tail.
        let mut bytes = vec![b'x'; JOURNAL_READ_MAX as usize + (1 << 20)];
        bytes.push(b'{');
        std::fs::write(&path, &bytes).unwrap();
        let mut ids = HashSet::new();
        let line = LogEntry::for_event("inc1", 1, 1, Verdict::Skipped, Some("type"), None);
        assert!(
            append(root.path(), "inc1", &mut ids, std::slice::from_ref(&line)).is_err(),
            "the append refuses a journal over the read cap"
        );
        assert_eq!(
            std::fs::read(&path).unwrap().len(),
            bytes.len(),
            "the file did not grow"
        );
    }

    /// An append never goes through a directory swapped for a link between
    /// its checks and its use: the operation refuses, and the other
    /// directory's journal stays exactly as it is.
    #[test]
    fn an_append_never_goes_through_a_directory_swapped_for_a_link() {
        let root = tempfile::tempdir().unwrap();
        let elsewhere = tempfile::tempdir().unwrap();
        let other_trigger = elsewhere.path().join("trigger-state");
        let other_log = other_trigger.join("log");
        let trigger = root.path().join(".apb").join("trigger-state");
        std::fs::create_dir_all(root.path().join(".apb")).unwrap();
        std::fs::create_dir_all(&other_log).unwrap();
        std::fs::write(other_log.join("inc1.jsonl"), b"keep me\n").unwrap();
        // The swap a check would miss: the real `trigger-state` moves
        // aside and a link takes its place, reaching the other directory's
        // trigger state, whose `log` holds the journal of this incarnation.
        let (trigger2, elsewhere2) = (trigger.clone(), elsewhere.path().to_path_buf());
        plant_dir_swap(Box::new(move || {
            std::fs::rename(&trigger2, elsewhere2.join("moved")).unwrap();
            std::os::unix::fs::symlink(&other_trigger, &trigger).unwrap();
        }));
        let mut ids = HashSet::new();
        let line = LogEntry::for_event("inc1", 1, 1, Verdict::Skipped, Some("type"), None);
        assert!(
            append(root.path(), "inc1", &mut ids, std::slice::from_ref(&line)).is_err(),
            "the append refuses"
        );
        assert_eq!(
            std::fs::read(other_log.join("inc1.jsonl")).unwrap(),
            b"keep me\n",
            "the other journal is untouched"
        );
    }

    /// The directory syncs go through the walked descriptors, never a path
    /// reopened after the walk: the directory that holds the file is the
    /// one the open reached, so a link at `log` planted once the file was
    /// opened is never synced in place of it, and a FIFO at its target is
    /// never opened (a path-based sync would block on it until a writer
    /// appears).
    #[cfg(unix)]
    #[test]
    fn the_append_syncs_the_walked_directories_not_a_path() {
        use std::os::unix::fs::FileTypeExt;
        let root = tempfile::tempdir().unwrap();
        let elsewhere = tempfile::tempdir().unwrap();
        let fifo = elsewhere.path().join("pipe");
        let c = std::ffi::CString::new(fifo.as_os_str().as_encoded_bytes()).unwrap();
        // SAFETY: a NUL-terminated path.
        assert_eq!(unsafe { libc::mkfifo(c.as_ptr(), 0o600) }, 0);
        std::fs::create_dir_all(root.path().join(".apb")).unwrap();
        let log = root.path().join(".apb/trigger-state/log");
        std::fs::create_dir_all(&log).unwrap();
        let (log2, fifo2, moved2) = (log.clone(), fifo.clone(), elsewhere.path().join("moved"));
        let root2 = root.path().to_path_buf();
        let (tx, rx) = std::sync::mpsc::channel();
        // On its own thread (the plant is thread-local), so a blocking sync
        // shows as a timeout here.
        std::thread::spawn(move || {
            plant_pre_sync(Box::new(move || {
                std::fs::rename(&log2, &moved2).unwrap();
                std::os::unix::fs::symlink(&fifo2, &log2).unwrap();
            }));
            let mut ids = HashSet::new();
            let line = LogEntry::for_event("inc1", 1, 1, Verdict::Skipped, Some("type"), None);
            let _ = tx.send(append(
                &root2,
                "inc1",
                &mut ids,
                std::slice::from_ref(&line),
            ));
        });
        let appended = rx
            .recv_timeout(std::time::Duration::from_secs(10))
            .expect("the sync never reopens the walked directory by path");
        assert!(
            appended.is_ok(),
            "the append syncs the directory it opened: {appended:?}"
        );
        // The FIFO was never opened: it is still a FIFO with no reader.
        let meta = std::fs::metadata(&fifo).unwrap();
        assert!(
            meta.file_type().is_fifo(),
            "the link's target was not touched"
        );
    }

    /// A compaction never goes through a directory swapped for a link
    /// between its read and its replacement: the operation refuses, and
    /// the other directory's journal keeps its bytes.
    #[test]
    fn a_compaction_never_goes_through_a_directory_swapped_for_a_link() {
        let root = tempfile::tempdir().unwrap();
        let elsewhere = tempfile::tempdir().unwrap();
        let other_log = elsewhere.path().join("log");
        let trigger = root.path().join(".apb").join("trigger-state");
        let log = trigger.join("log");
        std::fs::create_dir_all(&log).unwrap();
        // A journal that compacts: more than LOG_REWRITE_AT lines.
        let mut ids = HashSet::new();
        let lines: Vec<LogEntry> = (0..LOG_REWRITE_AT as u64 + 20)
            .map(|i| LogEntry::for_event("inc1", i, i, Verdict::Skipped, Some("type"), None))
            .collect();
        append(root.path(), "inc1", &mut ids, &lines).unwrap();
        // The swap: the real `log` moves into the other directory and a
        // link takes its place, where the other journal waits.
        let (log2, other_log2) = (log.clone(), other_log.clone());
        plant_dir_swap(Box::new(move || {
            std::fs::create_dir_all(&other_log2).unwrap();
            std::fs::rename(&log2, other_log2.join("log")).unwrap();
            std::os::unix::fs::symlink(other_log2.join("log"), &log2).unwrap();
        }));
        let other_journal = other_log.join("log").join("inc1.jsonl");
        // The bytes the journal has now are the bytes the moved file must
        // still have afterwards.
        let before = std::fs::read(log.join("inc1.jsonl")).unwrap();
        assert!(
            journal_ids(root.path(), "inc1").is_err(),
            "the compaction refuses"
        );
        assert_eq!(
            std::fs::read(&other_journal).unwrap(),
            before,
            "the other journal is untouched"
        );
    }

    /// The compaction's directory sync goes through the walked descriptor,
    /// never a path reopened after the rename: a link at `log` planted
    /// once the replacement was written is refused, and a FIFO at its
    /// target is never opened (a path-based sync would block on it).
    #[cfg(unix)]
    #[test]
    fn the_compaction_syncs_the_walked_directory_not_a_path() {
        use std::os::unix::fs::FileTypeExt;
        let root = tempfile::tempdir().unwrap();
        let elsewhere = tempfile::tempdir().unwrap();
        let fifo = elsewhere.path().join("pipe");
        let c = std::ffi::CString::new(fifo.as_os_str().as_encoded_bytes()).unwrap();
        // SAFETY: a NUL-terminated path.
        assert_eq!(unsafe { libc::mkfifo(c.as_ptr(), 0o600) }, 0);
        let log = root.path().join(".apb/trigger-state/log");
        std::fs::create_dir_all(&log).unwrap();
        // A journal that compacts.
        let mut ids = HashSet::new();
        let lines: Vec<LogEntry> = (0..LOG_REWRITE_AT as u64 + 20)
            .map(|i| LogEntry::for_event("inc1", i, i, Verdict::Skipped, Some("type"), None))
            .collect();
        append(root.path(), "inc1", &mut ids, &lines).unwrap();
        let (log2, fifo2, moved2) = (log.clone(), fifo.clone(), elsewhere.path().join("moved"));
        let root2 = root.path().to_path_buf();
        let (tx, rx) = std::sync::mpsc::channel();
        // On its own thread (the plant is thread-local), so a blocking sync
        // shows as a timeout here.
        std::thread::spawn(move || {
            plant_pre_sync(Box::new(move || {
                std::fs::rename(&log2, &moved2).unwrap();
                std::os::unix::fs::symlink(&fifo2, &log2).unwrap();
            }));
            let _ = tx.send(journal_ids(&root2, "inc1"));
        });
        let read = rx
            .recv_timeout(std::time::Duration::from_secs(10))
            .expect("the sync never reopens the walked directory by path");
        assert!(read.is_err(), "the planted link is refused");
        let meta = std::fs::metadata(&fifo).unwrap();
        assert!(
            meta.file_type().is_fifo(),
            "the link's target was not touched"
        );
    }

    /// A whole record a crash left without its newline is completed and
    /// synced before a replay of it answers.
    #[test]
    fn a_repaired_tail_is_synced_before_the_state() {
        let root = tempfile::tempdir().unwrap();
        let first = LogEntry::for_event("inc1", 1, 1, Verdict::Skipped, Some("type"), None);
        let second = LogEntry::for_event("inc1", 2, 2, Verdict::Skipped, Some("type"), None);
        write_log_ends_without_newline(root.path(), "inc1", &[&first, &second]);
        let mut ids = journal_ids(root.path(), "inc1").unwrap();
        take_syncs();
        append(root.path(), "inc1", &mut ids, &[second]).unwrap();
        assert_eq!(take_syncs(), vec!["file", "log", "trigger-state", "apb"]);
        assert_eq!(line_count(&log_path(root.path(), "inc1")), 2);
    }

    /// The append syncs the directory it opened: a `log` swapped for a
    /// link after the open is never synced in place of the directory that
    /// holds the file, so the append still goes through.
    #[cfg(unix)]
    #[test]
    fn the_append_syncs_the_directory_it_opened() {
        let root = tempfile::tempdir().unwrap();
        let elsewhere = tempfile::tempdir().unwrap();
        let log = root.path().join(".apb/trigger-state/log");
        std::fs::create_dir_all(&log).unwrap();
        let (log2, moved2) = (log.clone(), elsewhere.path().join("moved"));
        let moved3 = moved2.clone();
        plant_pre_sync(Box::new(move || {
            std::fs::rename(&log2, &moved3).unwrap();
            std::os::unix::fs::symlink(&moved3, &log2).unwrap();
        }));
        let mut ids = HashSet::new();
        let line = LogEntry::for_event("inc1", 1, 1, Verdict::Skipped, Some("type"), None);
        append(root.path(), "inc1", &mut ids, &[line])
            .expect("the append syncs the directory it opened");
        assert!(
            moved2.join("inc1.jsonl").is_file(),
            "the line is in the directory the file was opened from"
        );
    }

    /// The journal is never read, appended to or repaired through a link:
    /// a link at the file or at `log/` is refused and its target untouched.
    #[test]
    fn the_journal_is_never_used_through_a_link() {
        let root = tempfile::tempdir().unwrap();
        let elsewhere = tempfile::tempdir().unwrap();
        let target = elsewhere.path().join("target.jsonl");
        // A target without a trailing newline: a repair through the link
        // would cut or complete it.
        std::fs::write(&target, b"keep me").unwrap();
        let path = log_path(root.path(), "inc1");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::os::unix::fs::symlink(&target, &path).unwrap();
        let line = LogEntry::for_event("inc1", 1, 1, Verdict::Skipped, Some("type"), None);
        assert!(
            journal_ids(root.path(), "inc1").is_err(),
            "the id read refuses a link"
        );
        assert!(
            read(root.path(), "inc1", 10).is_empty(),
            "the reader shows nothing"
        );
        let mut ids = HashSet::new();
        assert!(append(root.path(), "inc1", &mut ids, std::slice::from_ref(&line)).is_err());
        assert_eq!(
            std::fs::read(&target).unwrap(),
            b"keep me",
            "the target is untouched"
        );
        // A link at `log/` itself.
        std::fs::remove_file(&path).unwrap();
        let log = path.parent().unwrap().to_path_buf();
        std::fs::remove_dir(&log).unwrap();
        std::os::unix::fs::symlink(elsewhere.path(), &log).unwrap();
        std::fs::rename(&target, elsewhere.path().join("inc1.jsonl")).unwrap();
        assert!(journal_ids(root.path(), "inc1").is_err());
        assert!(append(root.path(), "inc1", &mut ids, &[line]).is_err());
        assert_eq!(
            std::fs::read(elsewhere.path().join("inc1.jsonl")).unwrap(),
            b"keep me"
        );
    }

    /// A journal refused at a component of its path names that component
    /// and says to replace the link with a directory: never the leaf's
    /// remedy, which would delete a path through the link.
    #[cfg(unix)]
    #[test]
    fn a_journal_refused_at_a_component_names_the_component() {
        let root = tempfile::tempdir().unwrap();
        let elsewhere = tempfile::tempdir().unwrap();
        // `log` is a link: the journal is never reached.
        let log = root.path().join(".apb/trigger-state/log");
        std::fs::create_dir_all(log.parent().unwrap()).unwrap();
        std::os::unix::fs::symlink(elsewhere.path(), &log).unwrap();
        let refusal = journal_reads(root.path(), "inc1").expect_err("the read is refused");
        let sentence = journal_refused_text("triage", "inc1", &refusal);
        assert!(sentence.contains(".apb/trigger-state/log"), "{sentence}");
        assert!(sentence.contains("is a link"), "{sentence}");
        assert!(
            sentence.contains("replace the link at .apb/trigger-state/log with a directory"),
            "{sentence}"
        );
        assert!(
            !sentence.contains("remove the rule and save it again"),
            "the leaf's remedy is not offered: {sentence}"
        );
        assert!(
            !sentence.contains("delete .apb/trigger-state/log/"),
            "no path through the link is offered: {sentence}"
        );
    }

    /// A journal refused at the file itself keeps the leaf's remedy: the
    /// rule is removed and saved again, or the file is deleted.
    #[cfg(unix)]
    #[test]
    fn a_journal_refused_at_the_file_keeps_the_leaf_remedy() {
        let root = tempfile::tempdir().unwrap();
        let elsewhere = tempfile::tempdir().unwrap();
        let target = elsewhere.path().join("target.jsonl");
        std::fs::write(&target, b"keep me").unwrap();
        let path = log_path(root.path(), "inc1");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::os::unix::fs::symlink(&target, &path).unwrap();
        let refusal = journal_reads(root.path(), "inc1").expect_err("the read is refused");
        let sentence = journal_refused_text("triage", "inc1", &refusal);
        assert!(sentence.contains("a link"), "{sentence}");
        assert!(
            sentence.contains("remove the rule and save it again"),
            "{sentence}"
        );
        assert!(
            sentence.contains("delete .apb/trigger-state/log/inc1.jsonl"),
            "{sentence}"
        );
    }

    /// The position is a compare and set: an entry of the rule's own
    /// incarnation, which another holder committed, is never moved by a
    /// late positioning.
    #[test]
    fn position_at_head_never_moves_an_entry_of_the_same_incarnation() {
        let _cfg = crate::test_config();
        let root = tempfile::tempdir().unwrap();
        crate::projects::bind_id(root.path()).unwrap();
        assert!(position_at_head(root.path(), &entry("inc1"), 5, 1).unwrap());
        // The holder moved on to 9.
        signed_store::transact(
            root.path(),
            Scope::Project,
            "state",
            Unsigned::Refuse,
            |file: &mut TriggerStateFile| -> Result<((), bool), TriggerError> {
                file.rules[0].seen_through = 9;
                Ok(((), true))
            },
        )
        .unwrap();
        assert!(
            !position_at_head(root.path(), &entry("inc1"), 12, 2).unwrap(),
            "nothing positioned"
        );
        let loaded =
            signed_store::read::<TriggerStateFile>(root.path(), Scope::Project, "state").unwrap();
        assert_eq!(
            loaded.file.rules[0].seen_through, 9,
            "the committed position stands"
        );
    }
}
