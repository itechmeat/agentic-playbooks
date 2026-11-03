//! The reading view of a run's result snapshots: every snapshot a journal
//! stamp names, as rows by node, execution and item, the stored text of one
//! entry re-verified on every read, and the comparison of a stored file with
//! its live copy. Every surface (MCP, HTTP, CLI, the dashboard) answers
//! through this module, so they agree on bytes, digests and verdicts.
//!
//! Nothing here reads a snapshot no stamp names. A manifest whose sha256 is
//! not its id, or an object whose sha256 is not its entry's digest, reads as
//! [`Verified::Corrupt`], never as content. Every text passes the run
//! redactor on the way out, so a secret known only after capture stays
//! hidden ([`EntryContent::redacted_on_read`]). Such an entry is shown by
//! what is served, never by what is stored: a path the redactor changes
//! reads `<redacted path n>` and its id is built from that (no spelling
//! of the value, the percent-encoded one included, leaves), and its digest
//! and size are those of the served text, so neither the stored digest
//! nor a live comparison can confirm a guess of the value.

use std::io;
use std::path::{Path, PathBuf};

use serde::Serialize;

use apb_core::fsutil::beneath::{Beneath, CappedRead, ReadRefusal};
use apb_core::result_store::{
    self as store, EntryId, EntryPart, IncompleteReason, MAX_FILE_BYTES, MAX_MANIFEST_BYTES,
    MAX_SNAPSHOT_BYTES, OutputOmitted, REDACTED_PATH_PREFIX, ResultFormat, ResultScope,
    ResultStamp, SnapshotEntry, SnapshotManifest, SnapshotOmission, SnapshotProvenance,
};

use crate::event::{Event, EventPayload};
use crate::identity::{ExecutionRef, ListVisitRef};
use crate::result_ref::ResultItem;
use crate::run_redact::RunRedactor;

/// An entry as a reader sees it: its id and path after the run redactor,
/// see the module docs.
struct ShownId {
    id: Option<EntryId>,
    path: Option<String>,
    path_redacted: bool,
}

/// Each entry of `m` as a reader sees it, in order. A path the redactor
/// changes now is numbered after the `<redacted path n>` names capture
/// already gave (entries and omissions alike), so two never meet.
fn shown_ids(m: &SnapshotManifest, redactor: &RunRedactor) -> Vec<ShownId> {
    let number = |p: Option<&str>| {
        p?.strip_prefix(REDACTED_PATH_PREFIX)?
            .strip_suffix('>')?
            .parse::<usize>()
            .ok()
    };
    let omitted = m.omitted.iter().filter_map(|o| match o {
        SnapshotOmission::Item(i) => number(i.path.as_deref()),
        SnapshotOmission::More(_) => None,
    });
    let mut next = m
        .entries
        .iter()
        .filter_map(|e| number(e.path.as_deref()))
        .chain(omitted)
        .max()
        .unwrap_or(0);
    m.entries
        .iter()
        .map(|e| match (e.part, e.scope, e.path.as_deref()) {
            (EntryPart::File, Some(scope), Some(p)) if redactor.redact_str(p) != p => {
                next += 1;
                let path = format!("{REDACTED_PATH_PREFIX}{next}>");
                ShownId {
                    id: Some(EntryId::File {
                        scope,
                        path: path.clone(),
                    }),
                    path: Some(path),
                    path_redacted: true,
                }
            }
            _ => ShownId {
                id: e.id(),
                path: e.path.clone(),
                path_redacted: false,
            },
        })
        .collect()
}

/// The display cap of one inline read over MCP and HTTP JSON; the raw HTTP
/// route serves the whole entry.
pub const DISPLAY_CAP: u64 = 64 * 1024;

/// Every snapshot of one run, and the artifact refs recorded without one.
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct ResultsView {
    pub rows: Vec<ResultRow>,
    pub legacy: Vec<LegacyRefRow>,
    /// Settlements of declared results whose snapshot was not stored (the
    /// `result_capture_failed` diagnostics), so a missing row says why.
    pub not_stored: Vec<NotStoredRow>,
}

/// A settlement that carries no stamp because its capture failed. The
/// node settled all the same.
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct NotStoredRow {
    pub node: String,
    /// The execution ordinal, 1-based.
    pub execution: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub item: Option<String>,
    /// `snapshot not stored: <why>`, scrubbed and clipped at capture.
    pub reason: String,
}

/// One stamped snapshot.
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ResultRow {
    pub execution: ExecutionRef,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub item: Option<ResultItem>,
    /// The seq of the `node_finished` or `list_item_settled` carrying the
    /// stamp.
    pub settlement_seq: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub produced_by_attempt: Option<u64>,
    /// That attempt's number, as its `attempt_started` counts it (the
    /// number people read; the id above is a journal seq).
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub attempt_number: Option<u32>,
    /// The settlement's status (`succeeded`, `failed`, `timed_out`) or an
    /// item's state.
    pub status: String,
    pub snapshot_id: String,
    pub entries: Vec<EntryView>,
    pub omitted: Vec<SnapshotOmission>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub incomplete: Option<IncompleteReason>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub output_omitted: Option<OutputOmitted>,
    pub provenance: SnapshotProvenance,
    pub verified: Verified,
}

/// One entry of a row, with its id inside the snapshot, as a reader is
/// served it.
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct EntryView {
    /// `output`, `fields` or `file:<scope>:<percent-encoded path>`, built
    /// from the path as shown.
    pub entry: String,
    pub part: EntryPart,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub scope: Option<ResultScope>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub path: Option<String>,
    /// The size and digest of the text as served: the stored ones unless
    /// the run redactor changes the text now. Absent when the object is
    /// missing or no longer matches: nothing verifies the stored ones, and
    /// next to a secret known only now they would confirm a guess.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub size: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub digest: Option<String>,
    pub redacted: bool,
    /// The run redactor changes this entry's text or path now: a secret
    /// became known after capture.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    #[cfg_attr(feature = "ts", ts(as = "Option<bool>", optional))]
    pub redacted_on_read: bool,
    pub format: ResultFormat,
}

/// Whether a row's manifest is there and hashes to its id.
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Verified {
    Ok,
    Missing,
    Corrupt,
}

/// A `node_finished` artifact ref without a snapshot: a recorded digest,
/// no stored bytes.
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct LegacyRefRow {
    pub execution: ExecutionRef,
    pub settlement_seq: u64,
    pub scope: String,
    pub path: String,
    pub digest: String,
}

/// The stored text of one entry, or the requested byte range of it.
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct EntryContent {
    pub text: String,
    /// The digest of the whole text as served, `sha256:<hex>`: the stored
    /// one unless the text was redacted on read.
    pub digest: String,
    /// The byte length of the whole text as served; a reader pages
    /// against it.
    pub size: u64,
    /// The byte range `[a, b)` of the served text this answer holds.
    pub range: (u64, u64),
    pub redacted_at_capture: bool,
    pub redacted_on_read: bool,
}

/// How a stored file compares with the live file now. Only the word is
/// answered, never live content.
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum LiveComparison {
    Same,
    Differs,
    Missing,
    Unreadable,
    /// A redacted entry, or one that is not a workspace file.
    NotComparable,
}

/// Why an entry could not be read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResultReadError {
    /// No stamp of the run names the snapshot, or it has no such entry.
    NotFound,
    /// The manifest or the object does not hash to its name.
    Corrupt,
    /// The range is out of bounds or not on character boundaries.
    BadRange,
    /// The run redactor could not resolve the run tree; nothing is served.
    Withheld,
    Io(String),
}

impl std::fmt::Display for ResultReadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ResultReadError::NotFound => {
                f.write_str("no stamp of the run names this snapshot and entry")
            }
            ResultReadError::Corrupt => f.write_str(
                "corrupt: the stored snapshot or its object is missing or does not hash to its name",
            ),
            ResultReadError::BadRange => f.write_str(
                "the range is out of bounds or not on character boundaries of the text",
            ),
            ResultReadError::Withheld => f.write_str(
                "secrets_unresolved: the run's secret values could not all be read, so no text is served",
            ),
            ResultReadError::Io(e) => write!(f, "read failed: {e}"),
        }
    }
}

impl std::error::Error for ResultReadError {}

impl From<io::Error> for ResultReadError {
    fn from(e: io::Error) -> Self {
        ResultReadError::Io(e.to_string())
    }
}

/// One settlement event that carries a stamp.
struct Stamped<'a> {
    seq: u64,
    node: &'a str,
    status: &'a str,
    /// The list item, for a `list_item_settled`.
    item: Option<&'a str>,
    stamp: &'a ResultStamp,
}

fn stamped(e: &Event) -> Option<Stamped<'_>> {
    match &e.payload {
        EventPayload::NodeFinished {
            node,
            status,
            result: Some(stamp),
            ..
        } => Some(Stamped {
            seq: e.seq,
            node,
            status,
            item: None,
            stamp,
        }),
        EventPayload::ListItemSettled {
            node,
            item,
            state,
            result: Some(stamp),
            ..
        } => Some(Stamped {
            seq: e.seq,
            node,
            status: state,
            item: Some(item),
            stamp,
        }),
        _ => None,
    }
}

fn journal(run_dir: &Path) -> io::Result<Vec<Event>> {
    crate::event::read_journal(run_dir)
        .map(|j| j.events)
        .map_err(|e| io::Error::other(e.to_string()))
}

/// A manifest as read from the store: verified against its id, or why not.
enum ManifestRead {
    Ok(Box<SnapshotManifest>),
    Missing,
    Corrupt,
}

/// Reads `snapshots/<id>.json` and checks that its bytes hash to `id` and
/// that it is the canonical form of what it decodes to.
fn read_manifest(b: &Beneath, id: &str) -> io::Result<ManifestRead> {
    if !store::is_snapshot_id(id) {
        return Ok(ManifestRead::Corrupt);
    }
    let bytes = match b.read_regular_capped(&store::snapshot_rel(id), MAX_MANIFEST_BYTES as u64)? {
        CappedRead::Read { bytes, .. } => bytes,
        CappedRead::Refused(ReadRefusal::Missing) => return Ok(ManifestRead::Missing),
        CappedRead::Refused(_) => return Ok(ManifestRead::Corrupt),
    };
    if store::snapshot_id(&bytes) != id {
        return Ok(ManifestRead::Corrupt);
    }
    match serde_json::from_slice::<SnapshotManifest>(&bytes) {
        Ok(m) => Ok(ManifestRead::Ok(Box::new(m))),
        Err(_) => Ok(ManifestRead::Corrupt),
    }
}

/// The stored text of one entry, re-hashed: `None` when the object is
/// missing, does not hash to the entry's digest, has another size, or is
/// not UTF-8.
fn read_object(b: &Beneath, entry: &SnapshotEntry) -> io::Result<Option<String>> {
    let Some(hex) = store::digest_hex(&entry.digest) else {
        return Ok(None);
    };
    if hex.len() != 64
        || !hex
            .bytes()
            .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase())
    {
        return Ok(None);
    }
    let bytes = match b.read_regular_capped(&store::object_rel(hex), MAX_SNAPSHOT_BYTES)? {
        CappedRead::Read { bytes, digest } if digest == entry.digest => bytes,
        _ => return Ok(None),
    };
    if bytes.len() as u64 != entry.size {
        return Ok(None);
    }
    Ok(String::from_utf8(bytes).ok())
}

/// Whether a manifest is the snapshot of the settlement that stamps it.
fn names_subject(m: &SnapshotManifest, run_id: Option<&str>, s: &Stamped<'_>) -> bool {
    run_id.is_none_or(|r| r == m.run_id)
        && m.subject.node == s.node
        && m.subject.item.as_deref() == s.item
}

fn run_id_of(run_dir: &Path) -> Option<&str> {
    run_dir.file_name().and_then(|n| n.to_str())
}

impl ResultsView {
    /// Every stamped snapshot of the run at `run_dir`, verified: each
    /// manifest and each object is re-hashed. A run without stamps reads
    /// empty and opens nothing under `results/`.
    pub fn load(run_dir: &Path) -> io::Result<ResultsView> {
        let events = journal(run_dir)?;
        let index = crate::identity::index(&events);
        let visits = crate::identity::list_visits(&events);
        let run_id = run_id_of(run_dir);
        let redactor = RunRedactor::for_run_dir(run_dir);
        let mut beneath: Option<Beneath> = None;
        let mut view = ResultsView::default();
        for e in &events {
            if let EventPayload::NodeFinished {
                node,
                artifacts,
                result: None,
                ..
            } = &e.payload
            {
                let execution = execution_of(&index, node, e.seq);
                view.legacy.extend(artifacts.iter().map(|a| LegacyRefRow {
                    execution: execution.clone(),
                    settlement_seq: e.seq,
                    scope: match a.scope {
                        apb_core::cache::ArtifactScope::Run => "run".into(),
                        apb_core::cache::ArtifactScope::Workspace => "workspace".into(),
                    },
                    path: a.path.clone(),
                    digest: a.digest.clone(),
                }));
                continue;
            }
            let Some(s) = stamped(e) else { continue };
            if beneath.is_none() {
                beneath = Some(Beneath::open(run_dir)?);
            }
            let b = beneath.as_ref().expect("opened above");
            view.rows
                .push(row(b, &redactor, &events, run_id, &index, &visits, &s)?);
        }
        // A capture that failed after its stamp (its transaction left open)
        // has a row; only a settlement without one is listed.
        let stamped = |f: &crate::diagnostics::capture::CaptureFailed| {
            view.rows.iter().any(|r| {
                r.execution.node == f.node
                    && r.execution.ordinal == f.execution
                    && r.item.as_ref().map(|i| i.id.as_str()) == f.item.as_deref()
            })
        };
        let not_stored: Vec<NotStoredRow> = crate::diagnostics::capture_failures(run_dir)
            .into_iter()
            .filter(|f| !stamped(f))
            .map(|f| NotStoredRow {
                node: f.node,
                execution: f.execution,
                item: f.item,
                reason: f.reason,
            })
            .collect();
        view.not_stored = not_stored;
        Ok(view)
    }
}

fn execution_of(index: &crate::identity::ExecutionIndex, node: &str, seq: u64) -> ExecutionRef {
    index
        .containing(node, seq)
        .map(|x| x.reference())
        .unwrap_or_else(|| ExecutionRef {
            node: node.to_string(),
            ordinal: 0,
            ended_seq: None,
        })
}

fn row(
    b: &Beneath,
    redactor: &RunRedactor,
    events: &[Event],
    run_id: Option<&str>,
    index: &crate::identity::ExecutionIndex,
    visits: &[ListVisitRef],
    s: &Stamped<'_>,
) -> io::Result<ResultRow> {
    let item = s.item.map(|id| ResultItem {
        visit: visits
            .iter()
            .filter(|v| v.node == s.node && v.started_seq < s.seq)
            .max_by_key(|v| v.started_seq)
            .cloned()
            .unwrap_or_else(|| ListVisitRef {
                node: s.node.to_string(),
                started_seq: 0,
            }),
        id: id.to_string(),
    });
    let mut out = ResultRow {
        execution: execution_of(index, s.node, s.seq),
        item,
        settlement_seq: s.seq,
        produced_by_attempt: None,
        attempt_number: None,
        status: s.status.to_string(),
        snapshot_id: s.stamp.snapshot_id.clone(),
        entries: Vec::new(),
        omitted: Vec::new(),
        incomplete: None,
        output_omitted: None,
        provenance: SnapshotProvenance::default(),
        verified: Verified::Ok,
    };
    let m = match read_manifest(b, &s.stamp.snapshot_id)? {
        ManifestRead::Ok(m) if names_subject(&m, run_id, s) => m,
        ManifestRead::Ok(_) | ManifestRead::Corrupt => {
            out.verified = Verified::Corrupt;
            return Ok(out);
        }
        ManifestRead::Missing => {
            out.verified = Verified::Missing;
            return Ok(out);
        }
    };
    let mut stored = Vec::with_capacity(m.entries.len());
    for entry in &m.entries {
        let text = read_object(b, entry)?;
        if text.is_none() {
            out.verified = Verified::Corrupt;
        }
        stored.push(text);
    }
    out.produced_by_attempt = m.subject.attempt;
    out.attempt_number = m.subject.attempt.and_then(|seq| {
        events
            .iter()
            .find(|e| e.seq == seq)
            .and_then(|e| match &e.payload {
                EventPayload::AttemptStarted { attempt, .. } => Some(*attempt),
                _ => None,
            })
    });
    let shown = shown_ids(&m, redactor);
    out.entries = m
        .entries
        .iter()
        .zip(shown)
        .zip(&stored)
        .filter_map(|((e, shown), text)| entry_view(e, shown, text.as_deref(), redactor))
        .collect();
    let m = *m;
    out.omitted = m.omitted;
    out.incomplete = m.incomplete;
    out.output_omitted = m.output_omitted;
    out.provenance = m.provenance;
    Ok(out)
}

fn entry_view(
    e: &SnapshotEntry,
    shown: ShownId,
    stored: Option<&str>,
    redactor: &RunRedactor,
) -> Option<EntryView> {
    let served = stored
        .map(|t| redactor.redact_str(t))
        .filter(|served| Some(served.as_str()) != stored);
    let (size, digest) = match (&served, stored) {
        (Some(t), _) => (
            Some(t.len() as u64),
            Some(apb_core::content::sha256_hex(t.as_bytes())),
        ),
        (None, Some(_)) => (Some(e.size), Some(e.digest.clone())),
        (None, None) => (None, None),
    };
    Some(EntryView {
        entry: shown.id?.to_string(),
        part: e.part,
        scope: e.scope,
        path: shown.path,
        size,
        digest,
        redacted: e.redacted,
        redacted_on_read: served.is_some() || shown.path_redacted,
        format: e.format,
    })
}

/// The manifest of a snapshot a stamp of the run names, verified, with the
/// event that stamps it; `NotFound` when no stamp names it, `Corrupt` when
/// the store does not hold it intact.
fn stamped_manifest(
    run_dir: &Path,
    snapshot_id: &str,
) -> Result<(Beneath, Vec<Event>, SnapshotManifest), ResultReadError> {
    if !store::is_snapshot_id(snapshot_id) {
        return Err(ResultReadError::NotFound);
    }
    let events = journal(run_dir)?;
    let b = Beneath::open(run_dir)?;
    let manifest = {
        let Some(s) = events
            .iter()
            .filter_map(stamped)
            .find(|s| s.stamp.snapshot_id == snapshot_id)
        else {
            return Err(ResultReadError::NotFound);
        };
        match read_manifest(&b, snapshot_id)? {
            ManifestRead::Ok(m) if names_subject(&m, run_id_of(run_dir), &s) => *m,
            _ => return Err(ResultReadError::Corrupt),
        }
    };
    Ok((b, events, manifest))
}

/// The manifest of a snapshot a stamp of the run names, with the manifest
/// and every object it names re-hashed: an approval binds and re-checks
/// only snapshots that still verify whole.
pub(crate) fn verified_manifest(
    run_dir: &Path,
    snapshot_id: &str,
) -> Result<SnapshotManifest, ResultReadError> {
    let (b, _, m) = stamped_manifest(run_dir, snapshot_id)?;
    for e in &m.entries {
        if read_object(&b, e)?.is_none() {
            return Err(ResultReadError::Corrupt);
        }
    }
    Ok(m)
}

/// A stamped snapshot's manifest with the stored text of each entry, in
/// entry order, the manifest and every object re-hashed. The texts are as
/// stored, before the run redactor: the one caller, the evidence reader
/// (`crate::evidence::sources`), redacts every text it serves and marks a
/// record the redactor changes as one no citation may rest on.
pub(crate) fn stored_texts(
    run_dir: &Path,
    snapshot_id: &str,
) -> Result<(SnapshotManifest, Vec<String>), ResultReadError> {
    let (b, _, m) = stamped_manifest(run_dir, snapshot_id)?;
    let mut texts = Vec::with_capacity(m.entries.len());
    for e in &m.entries {
        texts.push(read_object(&b, e)?.ok_or(ResultReadError::Corrupt)?);
    }
    Ok((m, texts))
}

/// The entry a reader names by its shown id ([`shown_ids`]), with whether
/// its path is redacted on read.
fn find_entry<'m>(
    m: &'m SnapshotManifest,
    entry: &EntryId,
    redactor: &RunRedactor,
) -> Option<(&'m SnapshotEntry, bool)> {
    m.entries
        .iter()
        .zip(shown_ids(m, redactor))
        .find(|(_, shown)| shown.id.as_ref() == Some(entry))
        .map(|(e, shown)| (e, shown.path_redacted))
}

/// The served text of an entry: stored, re-verified, through the run
/// redactor.
struct Served {
    text: String,
    entry: SnapshotEntry,
    redacted_on_read: bool,
}

fn served(run_dir: &Path, snapshot_id: &str, entry: &EntryId) -> Result<Served, ResultReadError> {
    let (b, _, m) = stamped_manifest(run_dir, snapshot_id)?;
    let redactor = RunRedactor::for_run_dir(run_dir);
    if redactor.withholds() {
        return Err(ResultReadError::Withheld);
    }
    let (e, _) = find_entry(&m, entry, &redactor).ok_or(ResultReadError::NotFound)?;
    let stored = read_object(&b, e)?.ok_or(ResultReadError::Corrupt)?;
    let text = redactor.redact_str(&stored);
    Ok(Served {
        redacted_on_read: text != stored,
        text,
        entry: e.clone(),
    })
}

fn content(s: Served, a: usize, b: usize) -> EntryContent {
    let (digest, size) = if s.redacted_on_read {
        (
            apb_core::content::sha256_hex(s.text.as_bytes()),
            s.text.len() as u64,
        )
    } else {
        (s.entry.digest, s.entry.size)
    };
    EntryContent {
        text: s.text[a..b].to_string(),
        digest,
        size,
        range: (a as u64, b as u64),
        redacted_at_capture: s.entry.redacted,
        redacted_on_read: s.redacted_on_read,
    }
}

/// Reads one entry of a stamped snapshot, re-verifying manifest and object
/// on every read. `range` is a byte range `[a, b)` on character boundaries;
/// `None` is the whole entry. The text passes the run redactor: when that
/// changed it (`redacted_on_read`), the range counts in the text as served,
/// and the digest and size are the served text's.
pub fn read_entry(
    run_dir: &Path,
    snapshot_id: &str,
    entry: &EntryId,
    range: Option<(u64, u64)>,
) -> Result<EntryContent, ResultReadError> {
    let s = served(run_dir, snapshot_id, entry)?;
    let (a, b) = match range {
        None => (0, s.text.len()),
        Some((a, b)) => {
            let (a, b) = (to_index(a)?, to_index(b)?);
            if a > b
                || b > s.text.len()
                || !s.text.is_char_boundary(a)
                || !s.text.is_char_boundary(b)
            {
                return Err(ResultReadError::BadRange);
            }
            (a, b)
        }
    };
    Ok(content(s, a, b))
}

/// [`read_entry`] from `offset` for at most `limit` bytes (at most
/// [`DISPLAY_CAP`]), the end moved back to a character boundary. `offset`
/// must sit on one and inside the text (or at its end).
pub fn read_window(
    run_dir: &Path,
    snapshot_id: &str,
    entry: &EntryId,
    offset: u64,
    limit: Option<u64>,
) -> Result<EntryContent, ResultReadError> {
    let s = served(run_dir, snapshot_id, entry)?;
    let a = to_index(offset)?;
    if a > s.text.len() || !s.text.is_char_boundary(a) {
        return Err(ResultReadError::BadRange);
    }
    let limit = to_index(limit.unwrap_or(DISPLAY_CAP).min(DISPLAY_CAP))?;
    let mut b = a.saturating_add(limit).min(s.text.len());
    while !s.text.is_char_boundary(b) {
        b -= 1;
    }
    Ok(content(s, a, b))
}

fn to_index(n: u64) -> Result<usize, ResultReadError> {
    usize::try_from(n).map_err(|_| ResultReadError::BadRange)
}

/// The tree a workspace file is compared in: the one capture read it from
/// (`scheduler/result.rs`), the run's working tree once resolved, else the
/// project root. Never an attempt's own `workdir`: capture does not read
/// there either, so a live answer always compares like with like.
fn live_root(run_dir: &Path, events: &[Event]) -> Option<PathBuf> {
    events
        .iter()
        .find_map(|e| match &e.payload {
            EventPayload::WorktreeResolved { path, .. } => Some(PathBuf::from(path)),
            _ => None,
        })
        .or_else(|| crate::run_config::project_root_of(run_dir))
}

/// How a stored file entry compares with its live copy now, for an
/// approval's `live: check`: a workspace file against the tree it was read
/// from, a run-scope file against the run directory. Output, fields and
/// redacted entries are `not_comparable`.
pub(crate) fn live_state(run_dir: &Path, events: &[Event], e: &SnapshotEntry) -> LiveComparison {
    let root = match (e.part, e.scope) {
        (EntryPart::File, Some(ResultScope::Workspace)) => live_root(run_dir, events),
        (EntryPart::File, Some(ResultScope::Run)) => Some(run_dir.to_path_buf()),
        _ => return LiveComparison::NotComparable,
    };
    match (e.path.as_deref(), root) {
        (Some(p), Some(root)) if !e.redacted && !p.starts_with(REDACTED_PATH_PREFIX) => {
            compare_at(&root, p, &e.digest)
        }
        (_, None) => LiveComparison::Missing,
        _ => LiveComparison::NotComparable,
    }
}

/// The live file at `path` beneath `root` against `digest`, through a
/// descriptor walk that follows no link.
fn compare_at(root: &Path, path: &str, digest: &str) -> LiveComparison {
    let b = match Beneath::open(root) {
        Ok(b) => b,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return LiveComparison::Missing,
        Err(_) => return LiveComparison::Unreadable,
    };
    match b.read_regular_capped(path, MAX_FILE_BYTES) {
        Ok(CappedRead::Read { digest: d, .. }) if d == digest => LiveComparison::Same,
        Ok(CappedRead::Read { .. }) | Ok(CappedRead::Refused(ReadRefusal::TooLarge { .. })) => {
            LiveComparison::Differs
        }
        Ok(CappedRead::Refused(ReadRefusal::Missing)) => LiveComparison::Missing,
        Ok(CappedRead::Refused(_)) => LiveComparison::Unreadable,
        Err(err) if err.kind() == io::ErrorKind::NotFound => LiveComparison::Missing,
        Err(_) => LiveComparison::Unreadable,
    }
}

/// Compares a stored workspace file with the live file, by digest, through
/// a descriptor walk that follows no link. Only the word is answered. The
/// stored object is re-hashed first, as on every read (`Corrupt`
/// otherwise). A redacted entry (nothing of the original is kept), a
/// redacted path, an entry the run redactor changes now (the answer would
/// confirm a guess of the value), a run-scope file and the output and
/// fields are `not_comparable`.
pub fn compare_live(
    run_dir: &Path,
    snapshot_id: &str,
    entry: &EntryId,
) -> Result<LiveComparison, ResultReadError> {
    let (run_b, events, m) = stamped_manifest(run_dir, snapshot_id)?;
    let redactor = RunRedactor::for_run_dir(run_dir);
    if redactor.withholds() {
        return Err(ResultReadError::Withheld);
    }
    let (e, path_redacted) = find_entry(&m, entry, &redactor).ok_or(ResultReadError::NotFound)?;
    let stored = read_object(&run_b, e)?.ok_or(ResultReadError::Corrupt)?;
    let path = match (e.part, e.scope, e.path.as_deref()) {
        (EntryPart::File, Some(ResultScope::Workspace), Some(p))
            if !e.redacted
                && !path_redacted
                && !p.starts_with(REDACTED_PATH_PREFIX)
                && redactor.redact_str(&stored) == stored =>
        {
            p
        }
        _ => return Ok(LiveComparison::NotComparable),
    };
    let Some(root) = live_root(run_dir, &events) else {
        return Ok(LiveComparison::Missing);
    };
    Ok(compare_at(&root, path, &e.digest))
}
