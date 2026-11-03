//! Result snapshots: the layout of `runs/<id>/results/`, the limits every
//! capture keeps, the canonical manifest and its id, the stamp a settlement
//! event carries, and the citation grammar that names one stored entry.
//!
//! A snapshot is taken for an execution of a node that declares `results:`
//! (see [`crate::schema::ResultDecl`]). It holds the execution's output text,
//! its declared fields and its declared files as text, scrubbed of known
//! secrets, addressed by digest. The manifest names its subject by plain
//! values ([`SnapshotSubject`]); the engine's view maps a subject plus the
//! seq of the event that carries the [`ResultStamp`] to a result reference.
//!
//! This module holds the shared types and the pure rules over them. The
//! transaction that writes a snapshot, the capture and the reading view live
//! with their owners (the engine's `scheduler::result`, `result_view` and
//! `result_clean`).

use std::collections::BTreeMap;
use std::fmt;
use std::io;
use std::path::Path;

use percent_encoding::{AsciiSet, NON_ALPHANUMERIC, percent_decode_str, utf8_percent_encode};
use serde::{Deserialize, Serialize};

use crate::fsutil::HeldLock;
use crate::fsutil::beneath::{Beneath, CappedRead, EntryKind, ReadRefusal};

// --- layout ---------------------------------------------------------------

/// The directory under a run directory that holds every snapshot.
pub const RESULTS_DIR: &str = "results";
/// The run-level results lock, taken by every step that changes `results/`.
pub const LOCK_NAME: &str = ".lock";
/// The run's byte reservation ([`Reservation`]).
pub const RESERVATION_FILE: &str = "reservation.json";
/// Open transactions, one directory per token.
pub const STAGING_DIR: &str = "staging";
/// The record of one open transaction ([`TxnRecord`]), inside its staging
/// directory.
pub const TXN_FILE: &str = "txn.json";
/// Content-addressed objects: `objects/<aa>/<hex>`, write-once.
pub const OBJECTS_DIR: &str = "objects";
/// Manifests: `snapshots/<snapshot_id>.json`.
pub const SNAPSHOTS_DIR: &str = "snapshots";

/// Names under a run directory that are apb's own storage: never captured
/// as a result and skipped by the run-directory walk of `outputs.files`.
pub const INTERNAL_RUN_NAMES: &[&str] = &[
    RESULTS_DIR,
    "definitions",
    "bindings",
    "diagnostics.jsonl",
    "evidence",
];

/// Workspace directories a declaration may not reach into and a capture
/// never reads: the repository's own data and apb's project storage.
pub const WORKSPACE_RESERVED: &[&str] = &[".git", ".apb"];

/// Whether `first`, the first segment of a path in `scope`, names what a
/// result never holds ([`WORKSPACE_RESERVED`], [`INTERNAL_RUN_NAMES`]),
/// compared ASCII case-insensitively: on a file system that folds case,
/// `.Git` is `.git`.
pub fn is_reserved_first(scope: ResultScope, first: &str) -> bool {
    let names = match scope {
        ResultScope::Workspace => WORKSPACE_RESERVED,
        ResultScope::Run => INTERNAL_RUN_NAMES,
    };
    names.iter().any(|n| n.eq_ignore_ascii_case(first))
}

/// File name patterns omitted as `secret_shaped` without being opened. A
/// trailing `*` matches any rest, a leading `*` any start.
pub const SECRET_SHAPED_NAMES: &[&str] = &[".env*", "*.pem", "*.key", "*.p12", "id_*"];

/// What replaces a path the scrub changed, followed by its number.
pub const REDACTED_PATH_PREFIX: &str = "<redacted path ";

// --- limits ---------------------------------------------------------------

/// `results` entries one node may declare.
pub const MAX_DECLARATIONS: usize = 32;
/// Directory entries examined per settlement across all globs.
pub const MAX_ENTRIES_EXAMINED: usize = 4096;
/// Matched files considered per settlement.
pub const MAX_MATCHES: usize = 64;
/// Bytes read per settlement.
pub const MAX_READ_BYTES: u64 = 32 * 1024 * 1024;
/// Bytes of one stored file; a larger file is omitted `too_large`.
pub const MAX_FILE_BYTES: u64 = 1024 * 1024;
/// Bytes of objects in one snapshot.
pub const MAX_SNAPSHOT_BYTES: u64 = 16 * 1024 * 1024;
/// Bytes of objects, manifests and staging in one run.
pub const MAX_RUN_BYTES: u64 = 256 * 1024 * 1024;
/// Omission records listed before the one aggregate record.
pub const MAX_OMISSION_RECORDS: usize = 64;
/// Bytes of one manifest.
pub const MAX_MANIFEST_BYTES: usize = 256 * 1024;

/// The manifest format written by this binary.
pub const MANIFEST_VERSION: u32 = 1;

// --- declaration values ---------------------------------------------------

/// Where a declared result lives: the node's working tree or the run
/// directory.
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ResultScope {
    #[default]
    Workspace,
    Run,
}

impl ResultScope {
    pub fn as_str(self) -> &'static str {
        match self {
            ResultScope::Workspace => "workspace",
            ResultScope::Run => "run",
        }
    }

    fn parse(s: &str) -> Option<Self> {
        match s {
            "workspace" => Some(ResultScope::Workspace),
            "run" => Some(ResultScope::Run),
            _ => None,
        }
    }
}

/// How a stored text entry is meant: plain text, or a records package
/// (stored as metadata only until the records reader exists).
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ResultFormat {
    #[default]
    Text,
    Records,
}

// --- manifest -------------------------------------------------------------

/// One snapshot, as stored in `snapshots/<snapshot_id>.json`. Field order is
/// part of the canonical bytes ([`canonical_bytes`]); never reorder.
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SnapshotManifest {
    pub v: u32,
    pub run_id: String,
    pub subject: SnapshotSubject,
    pub provenance: SnapshotProvenance,
    pub entries: Vec<SnapshotEntry>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    #[cfg_attr(feature = "ts", ts(as = "Option<Vec<SnapshotOmission>>", optional))]
    pub omitted: Vec<SnapshotOmission>,
    /// Why capture stopped early, when a limit stopped it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub incomplete: Option<IncompleteReason>,
    /// Why not even the output was stored.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub output_omitted: Option<OutputOmitted>,
}

/// What a snapshot is of, in plain values: the node, its execution ordinal
/// (1-based, in journal order), the list visit and item for an item
/// snapshot, and the attempt id that produced it. The settlement seq is not
/// known when the manifest is written; the stamp on the settlement event
/// closes the link.
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SnapshotSubject {
    pub node: String,
    pub execution: u32,
    /// The seq of the list visit's `list_started`, for an item snapshot.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub visit_seq: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub item: Option<String>,
    /// The attempt id (the seq of its `attempt_started`); absent for work
    /// that spawned no attempt.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub attempt: Option<u64>,
}

/// Where the stored bytes came from.
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SnapshotProvenance {
    /// The node was a cache hit: the bytes are the restored ones.
    pub cache_hit: bool,
    /// `HEAD` of the tree at capture; absent off git.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub head: Option<String>,
    /// The seq of the execution's `artifacts_committed`, when it committed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub commits_seq: Option<u64>,
}

/// Which part of a settled result an entry holds. The order of the variants
/// is the canonical order of entries.
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EntryPart {
    Output,
    Fields,
    File,
}

/// One stored entry: its text is the object `objects/<aa>/<hex>` named by
/// `digest`.
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SnapshotEntry {
    pub part: EntryPart,
    /// For a file entry.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub scope: Option<ResultScope>,
    /// For a file entry: relative to its scope root, `/`-separated, scrubbed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub path: Option<String>,
    /// Bytes of the stored (scrubbed) text.
    pub size: u64,
    /// `sha256:<hex>` of the stored bytes.
    pub digest: String,
    /// The secret scrub changed the text; nothing of the original is kept.
    pub redacted: bool,
    pub format: ResultFormat,
}

impl SnapshotEntry {
    /// The entry's id inside its snapshot.
    pub fn id(&self) -> Option<EntryId> {
        match self.part {
            EntryPart::Output => Some(EntryId::Output),
            EntryPart::Fields => Some(EntryId::Fields),
            EntryPart::File => Some(EntryId::File {
                scope: self.scope?,
                path: self.path.clone()?,
            }),
        }
    }
}

/// Something a capture did not store: one record, or the aggregate record
/// that counts every omission past [`MAX_OMISSION_RECORDS`].
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum SnapshotOmission {
    Item(OmittedItem),
    More(OmittedMore),
}

/// One omitted part or file.
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OmittedItem {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub part: Option<EntryPart>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub scope: Option<ResultScope>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub path: Option<String>,
    pub reason: OmissionReason,
}

/// The aggregate of the omissions not listed one by one.
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OmittedMore {
    pub more: u64,
    pub reasons: BTreeMap<OmissionReason, u64>,
}

/// A closed list of wire names that reads leniently: a name a newer apb
/// wrote reads as `Unknown`, so a newer manifest still loads. `ALL` lists
/// every name this binary writes, in canonical order.
macro_rules! lenient_names {
    ($(#[$m:meta])* pub enum $name:ident { $($(#[$vm:meta])* $var:ident = $wire:literal,)* }) => {
        $(#[$m])*
        #[cfg_attr(feature = "ts", derive(ts_rs::TS))]
        #[cfg_attr(feature = "ts", ts(rename_all = "snake_case"))]
        #[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
        pub enum $name {
            $($(#[$vm])* $var,)*
            /// A name a newer apb wrote; this binary never writes it.
            Unknown,
        }

        impl $name {
            pub const ALL: &'static [$name] = &[$($name::$var),*];

            pub fn as_str(self) -> &'static str {
                match self {
                    $($name::$var => $wire,)*
                    $name::Unknown => "unknown",
                }
            }
        }

        impl Serialize for $name {
            fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
                s.serialize_str(self.as_str())
            }
        }

        impl<'de> Deserialize<'de> for $name {
            fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
                let wire = String::deserialize(d)?;
                Ok($name::ALL
                    .iter()
                    .copied()
                    .find(|v| v.as_str() == wire)
                    .unwrap_or($name::Unknown))
            }
        }
    };
}

lenient_names! {
    /// Why something was not stored. Declared in name order, so the
    /// reasons of an aggregate record serialize sorted by name.
    pub enum OmissionReason {
        /// Not valid UTF-8, or a NUL byte.
        BinaryNotSupported = "binary_not_supported",
        /// A regular file with more than one hard link.
        HardLink = "hard_link",
        /// A link at some component of the path.
        Link = "link",
        /// Past [`MAX_MATCHES`].
        MatchLimit = "match_limit",
        /// A path that is not valid UTF-8.
        NonUtf8Path = "non_utf8_path",
        /// A declared exact path did not exist.
        NotFound = "not_found",
        /// A FIFO, device, socket or directory where a file was declared.
        NotRegularFile = "not_regular_file",
        /// Past [`MAX_READ_BYTES`].
        ReadLimit = "read_limit",
        /// A scenario run: the tree is not the node's result.
        ScenarioRun = "scenario_run",
        /// The run redactor could not resolve the run tree; fail closed.
        ScrubUnavailable = "scrub_unavailable",
        /// A name matching [`SECRET_SHAPED_NAMES`]; never opened.
        SecretShaped = "secret_shaped",
        /// Past [`MAX_SNAPSHOT_BYTES`] or [`MAX_RUN_BYTES`].
        StorageLimit = "storage_limit",
        /// Over [`MAX_FILE_BYTES`].
        TooLarge = "too_large",
        /// Reading failed.
        Unreadable = "unreadable",
    }
}

lenient_names! {
    /// Which limit stopped a capture.
    pub enum IncompleteReason {
        EnumerationLimit = "enumeration_limit",
        MatchLimit = "match_limit",
        ReadLimit = "read_limit",
        StorageLimit = "storage_limit",
    }
}

lenient_names! {
    /// Why the output itself was not stored.
    pub enum OutputOmitted {
        /// The run reservation had no room left.
        RunLimit = "run_limit",
        /// Larger than one snapshot may hold ([`MAX_SNAPSHOT_BYTES`]).
        SnapshotLimit = "snapshot_limit",
        /// The run redactor could not resolve the run tree.
        ScrubUnavailable = "scrub_unavailable",
    }
}

/// The canonical bytes of a manifest: compact JSON of the fixed-order
/// structs, entries sorted by part, then scope, then path bytes, the listed
/// omissions likewise with the aggregate record last.
pub fn canonical_bytes(manifest: &SnapshotManifest) -> Vec<u8> {
    let mut m = manifest.clone();
    m.entries.sort_by(|a, b| entry_key(a).cmp(&entry_key(b)));
    m.omitted
        .sort_by(|a, b| omission_key(a).cmp(&omission_key(b)));
    serde_json::to_vec(&m).expect("a manifest always serializes")
}

/// The id of a manifest: the lowercase hex sha256 of its canonical bytes.
pub fn snapshot_id(canonical: &[u8]) -> String {
    use sha2::Digest as _;
    crate::content::hex_lower(&sha2::Sha256::digest(canonical))
}

/// True for a well-formed snapshot id: 64 lowercase hex characters.
pub fn is_snapshot_id(s: &str) -> bool {
    s.len() == 64 && s.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

type SortKey<'a> = (u8, Option<EntryPart>, &'a [u8], &'a [u8]);

fn sort_key(
    part: Option<EntryPart>,
    scope: Option<ResultScope>,
    path: Option<&str>,
) -> SortKey<'_> {
    (
        0,
        part,
        scope.map(ResultScope::as_str).unwrap_or("").as_bytes(),
        path.unwrap_or("").as_bytes(),
    )
}

fn entry_key(e: &SnapshotEntry) -> SortKey<'_> {
    sort_key(Some(e.part), e.scope, e.path.as_deref())
}

fn omission_key(o: &SnapshotOmission) -> SortKey<'_> {
    match o {
        SnapshotOmission::Item(i) => sort_key(i.part, i.scope, i.path.as_deref()),
        SnapshotOmission::More(_) => (1, None, &[], &[]),
    }
}

// --- stamp ----------------------------------------------------------------

/// What a settlement event (`node_finished`, `list_item_settled`) carries
/// about the snapshot taken for it. Display and storage only: no execution
/// decision reads it.
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResultStamp {
    pub snapshot_id: String,
    #[serde(default)]
    pub entries: u32,
    #[serde(default)]
    pub bytes: u64,
    #[serde(default, skip_serializing_if = "is_zero")]
    #[cfg_attr(feature = "ts", ts(as = "Option<u32>", optional))]
    pub omitted: u32,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    #[cfg_attr(feature = "ts", ts(as = "Option<bool>", optional))]
    pub incomplete: bool,
}

fn is_zero(n: &u32) -> bool {
    *n == 0
}

// --- bookkeeping files ----------------------------------------------------

/// `results/reservation.json`: bytes reserved by open transactions and
/// committed by finished ones, kept under the results lock.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Reservation {
    #[serde(default)]
    pub reserved_bytes: u64,
    #[serde(default)]
    pub committed_bytes: u64,
    /// Bytes reserved per open transaction token.
    #[serde(default)]
    pub open: BTreeMap<String, u64>,
}

/// `staging/<token>/txn.json`: what one transaction has written so far.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TxnRecord {
    pub token: String,
    #[serde(default)]
    pub pid: u32,
    /// Digests of the objects this transaction staged or published.
    #[serde(default)]
    pub objects: Vec<String>,
    /// The snapshot id, once the manifest is written.
    #[serde(default)]
    pub manifest: Option<String>,
    /// Digests of the objects this transaction itself moved into
    /// `objects/`, named before each move: its close counts exactly these,
    /// so a cleanup while it is open leaves them out of the committed
    /// total and counts every other object it keeps.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub added: Vec<String>,
}

// --- citations ------------------------------------------------------------

/// One entry of a snapshot: `output`, `fields` or
/// `file:<scope>:<percent-encoded path>`.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum EntryId {
    Output,
    Fields,
    File { scope: ResultScope, path: String },
}

/// Every byte except the RFC 3986 unreserved characters; a path is encoded
/// segment by segment, so its `/` stays literal.
const SEGMENT: &AsciiSet = &NON_ALPHANUMERIC
    .remove(b'-')
    .remove(b'.')
    .remove(b'_')
    .remove(b'~');

impl fmt::Display for EntryId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            EntryId::Output => f.write_str("output"),
            EntryId::Fields => f.write_str("fields"),
            EntryId::File { scope, path } => {
                write!(f, "file:{}:", scope.as_str())?;
                for (i, seg) in path.split('/').enumerate() {
                    if i > 0 {
                        f.write_str("/")?;
                    }
                    write!(f, "{}", utf8_percent_encode(seg, SEGMENT))?;
                }
                Ok(())
            }
        }
    }
}

impl std::str::FromStr for EntryId {
    type Err = CitationError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "output" => return Ok(EntryId::Output),
            "fields" => return Ok(EntryId::Fields),
            _ => {}
        }
        let rest = s.strip_prefix("file:").ok_or(CitationError::Entry)?;
        let (scope, encoded) = rest.split_once(':').ok_or(CitationError::Entry)?;
        let scope = ResultScope::parse(scope).ok_or(CitationError::Entry)?;
        if encoded.is_empty() {
            return Err(CitationError::Entry);
        }
        let path = percent_decode_str(encoded)
            .decode_utf8()
            .map_err(|_| CitationError::Entry)?
            .into_owned();
        Ok(EntryId::File { scope, path })
    }
}

/// `r:<snapshot_id>/<entry>` with an optional byte range `#[a,b)` into the
/// stored UTF-8 text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Citation {
    pub snapshot_id: String,
    pub entry: EntryId,
    pub range: Option<(u64, u64)>,
}

/// Why a citation did not parse.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CitationError {
    /// It does not start with `r:`.
    Prefix,
    /// The snapshot id is not 64 lowercase hex characters.
    SnapshotId,
    /// The entry is none of the three forms.
    Entry,
    /// The range is not `#[a,b)` with `a <= b`.
    Range,
}

impl fmt::Display for CitationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            CitationError::Prefix => "a result reference starts with r:",
            CitationError::SnapshotId => "the snapshot id is not 64 lowercase hex characters",
            CitationError::Entry => "the entry is not output, fields or file:<scope>:<path>",
            CitationError::Range => "the range is not #[a,b) with a <= b",
        })
    }
}

impl std::error::Error for CitationError {}

impl fmt::Display for Citation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "r:{}/{}", self.snapshot_id, self.entry)?;
        if let Some((a, b)) = self.range {
            write!(f, "#[{a},{b})")?;
        }
        Ok(())
    }
}

impl std::str::FromStr for Citation {
    type Err = CitationError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let rest = s.strip_prefix("r:").ok_or(CitationError::Prefix)?;
        let (snapshot_id, rest) = rest.split_once('/').ok_or(CitationError::SnapshotId)?;
        if !is_snapshot_id(snapshot_id) {
            return Err(CitationError::SnapshotId);
        }
        let (entry, range) = match rest.split_once("#[") {
            Some((entry, range)) => (entry, Some(parse_range(range)?)),
            None => (rest, None),
        };
        Ok(Citation {
            snapshot_id: snapshot_id.to_string(),
            entry: entry.parse()?,
            range,
        })
    }
}

fn parse_range(s: &str) -> Result<(u64, u64), CitationError> {
    let inner = s.strip_suffix(')').ok_or(CitationError::Range)?;
    let (a, b) = inner.split_once(',').ok_or(CitationError::Range)?;
    let digits = |t: &str| {
        if t.is_empty() || !t.bytes().all(|c| c.is_ascii_digit()) {
            return Err(CitationError::Range);
        }
        t.parse::<u64>().map_err(|_| CitationError::Range)
    };
    let (a, b) = (digits(a)?, digits(b)?);
    if a > b {
        return Err(CitationError::Range);
    }
    Ok((a, b))
}

// --- secret-shaped names --------------------------------------------------

/// Whether a file name matches [`SECRET_SHAPED_NAMES`], in any letter case:
/// such a file is omitted `secret_shaped` without being opened.
pub fn is_secret_shaped(name: &str) -> bool {
    let name = name.to_ascii_lowercase();
    SECRET_SHAPED_NAMES.iter().any(|pattern| {
        if let Some(suffix) = pattern.strip_prefix('*') {
            name.ends_with(suffix)
        } else if let Some(prefix) = pattern.strip_suffix('*') {
            name.starts_with(prefix)
        } else {
            name == *pattern
        }
    })
}

// --- store paths ----------------------------------------------------------

/// How long a step waits for the results lock before its capture fails.
pub const LOCK_WAIT: std::time::Duration = std::time::Duration::from_secs(10);

/// The largest bookkeeping file read (`reservation.json`, `txn.json`).
const BOOKKEEPING_MAX_BYTES: u64 = 1024 * 1024;

/// `results/objects/<aa>/<hex>` of an object, relative to the run directory.
pub fn object_rel(hex: &str) -> String {
    format!("{RESULTS_DIR}/{OBJECTS_DIR}/{}/{hex}", &hex[..2])
}

/// `results/snapshots/<id>.json`, relative to the run directory.
pub fn snapshot_rel(id: &str) -> String {
    format!("{RESULTS_DIR}/{SNAPSHOTS_DIR}/{id}.json")
}

/// `results/staging`, relative to the run directory.
pub fn staging_rel() -> String {
    format!("{RESULTS_DIR}/{STAGING_DIR}")
}

/// The lock file name of a transaction, inside `results/staging`.
pub fn txn_lock_name(token: &str) -> String {
    format!("{token}.lock")
}

/// True for a transaction token: 32 lowercase hex characters.
pub fn is_token(s: &str) -> bool {
    s.len() == 32 && s.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

/// The hex of a `sha256:<hex>` digest.
pub fn digest_hex(digest: &str) -> Option<&str> {
    digest.strip_prefix("sha256:").filter(|h| is_snapshot_id(h))
}

/// Takes the results lock of the run whose directory `b` is anchored at.
pub fn lock_results(b: &Beneath) -> io::Result<HeldLock> {
    b.mkdir_all(RESULTS_DIR)?;
    b.hold_lock(RESULTS_DIR, LOCK_NAME, LOCK_WAIT)
}

/// A bookkeeping JSON file beneath `b`, `None` when absent.
pub fn read_json<T: serde::de::DeserializeOwned>(b: &Beneath, rel: &str) -> io::Result<Option<T>> {
    match b.read_regular_capped(rel, BOOKKEEPING_MAX_BYTES)? {
        CappedRead::Read { bytes, .. } => serde_json::from_slice(&bytes)
            .map(Some)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, format!("{rel}: {e}"))),
        CappedRead::Refused(ReadRefusal::Missing) => Ok(None),
        CappedRead::Refused(why) => Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("{rel} is not readable: {why:?}"),
        )),
    }
}

fn write_json<T: Serialize>(b: &Beneath, dir: &str, name: &str, value: &T) -> io::Result<()> {
    let bytes = serde_json::to_vec(value).map_err(io::Error::other)?;
    b.write_new(dir, name, &bytes)
}

/// The run's reservation (an absent file is an empty one).
pub fn read_reservation(b: &Beneath) -> io::Result<Reservation> {
    Ok(read_json(b, &format!("{RESULTS_DIR}/{RESERVATION_FILE}"))?.unwrap_or_default())
}

/// Writes the run's reservation; the caller holds the results lock.
pub fn write_reservation(b: &Beneath, r: &Reservation) -> io::Result<()> {
    write_json(b, RESULTS_DIR, RESERVATION_FILE, r)
}

// --- the transaction ------------------------------------------------------

/// What staging one object came to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Staged {
    /// Staged, `sha256:<hex>` of the bytes.
    Stored(String),
    /// Past [`MAX_SNAPSHOT_BYTES`]; nothing written.
    SnapshotFull,
    /// Past [`MAX_RUN_BYTES`] for the run; nothing written.
    RunFull,
}

/// One snapshot being written into `runs/<id>/results/`, from `begin` to
/// `end`. It holds its transaction lock the whole time; a process that dies
/// leaves the staging directory and a free lock, which cleanup resolves by
/// the journal. Dropping it without `end` or `abort` is exactly that: the
/// staging stays for cleanup.
///
/// Steps: [`ResultTxn::begin`] reserves room for the manifest and records
/// the transaction; [`ResultTxn::stage_object`] reserves and writes each
/// text into staging, named in `txn.json` before it can move;
/// [`ResultTxn::move_objects`] and [`ResultTxn::publish`] move them into
/// `objects/` (an object already there is kept) and rename the manifest to
/// `snapshots/<id>.json`; the caller journals the stamp; [`ResultTxn::end`]
/// removes the staging and settles the reservation.
pub struct ResultTxn {
    b: Beneath,
    record: TxnRecord,
    /// Hex of each staged object and its size.
    staged: BTreeMap<String, u64>,
    staged_bytes: u64,
    reserved: u64,
    moved: bool,
    /// Bytes this transaction added to `objects/`.
    added: u64,
    /// Bytes of the manifest it renamed into `snapshots/`.
    manifest_bytes: u64,
    _lock: HeldLock,
}

impl fmt::Debug for ResultTxn {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ResultTxn")
            .field("token", &self.record.token)
            .field("staged", &self.staged.len())
            .finish_non_exhaustive()
    }
}

impl ResultTxn {
    /// Opens a transaction in the run directory `run_dir`, reserving room
    /// for its manifest. Fails when the run has no room for one.
    pub fn begin(run_dir: &Path) -> io::Result<ResultTxn> {
        let b = Beneath::open(run_dir)?;
        let token = uuid::Uuid::new_v4().simple().to_string();
        let staging = staging_rel();
        let staging_dir = format!("{staging}/{token}");
        let guard = lock_results(&b)?;
        b.mkdir_all(&staging)?;
        let lock_rel = format!("{staging}/{}", txn_lock_name(&token));
        // The token is new, so only a passive probe (a doctor's survey) can
        // hold its lock file for an instant: wait it out instead of failing
        // the capture.
        let lock = b.hold_lock(&staging, &txn_lock_name(&token), LOCK_WAIT)?;
        let manifest_room = MAX_MANIFEST_BYTES as u64;
        let reserved = match reserve_locked(&b, &token, manifest_room) {
            Ok(r) => r,
            Err(e) => {
                let _ = b.remove(&lock_rel);
                return Err(e);
            }
        };
        if !reserved {
            let _ = b.remove(&lock_rel);
            return Err(io::Error::new(
                io::ErrorKind::StorageFull,
                "the run's result storage is full",
            ));
        }
        let record = TxnRecord {
            token: token.clone(),
            pid: std::process::id(),
            objects: Vec::new(),
            manifest: None,
            added: Vec::new(),
        };
        let setup = b
            .mkdir_all(&staging_dir)
            .and_then(|()| write_json(&b, &staging_dir, TXN_FILE, &record));
        if let Err(e) = setup {
            // Nothing names the reservation yet: give it back, still under
            // the results lock, so no failed begin keeps room for good.
            release_locked(&b, &token);
            let _ = b.remove_tree(&staging_dir);
            let _ = b.remove(&lock_rel);
            return Err(e);
        }
        drop(guard);
        Ok(ResultTxn {
            b,
            record,
            staged: BTreeMap::new(),
            staged_bytes: 0,
            reserved: manifest_room,
            moved: false,
            added: 0,
            manifest_bytes: 0,
            _lock: lock,
        })
    }

    fn staging_dir(&self) -> String {
        format!("{}/{}", staging_rel(), self.record.token)
    }

    /// Reserves room for `text` and writes it into staging. The same bytes
    /// twice are staged once.
    pub fn stage_object(&mut self, text: &[u8]) -> io::Result<Staged> {
        let digest = crate::content::sha256_hex(text);
        let hex = digest_hex(&digest).expect("a sha256 digest").to_string();
        if self.staged.contains_key(&hex) {
            return Ok(Staged::Stored(digest));
        }
        let size = text.len() as u64;
        if self.staged_bytes + size > MAX_SNAPSHOT_BYTES {
            return Ok(Staged::SnapshotFull);
        }
        {
            let _guard = lock_results(&self.b)?;
            if !reserve_locked(&self.b, &self.record.token, size)? {
                return Ok(Staged::RunFull);
            }
        }
        self.reserved += size;
        // Named in txn.json before the bytes exist, so no object of this
        // transaction is ever outside what cleanup protects.
        self.record.objects.push(hex.clone());
        write_json(&self.b, &self.staging_dir(), TXN_FILE, &self.record)?;
        self.b.write_new(&self.staging_dir(), &hex, text)?;
        self.staged.insert(hex, size);
        self.staged_bytes += size;
        Ok(Staged::Stored(digest))
    }

    /// Moves every staged object into `objects/<aa>/<hex>`, under the
    /// results lock; an object already there is kept and the staged copy
    /// removed.
    pub fn move_objects(&mut self) -> io::Result<()> {
        if self.moved {
            return Ok(());
        }
        let _guard = lock_results(&self.b)?;
        for (hex, size) in &self.staged {
            let staged = format!("{}/{hex}", self.staging_dir());
            let dest = object_rel(hex);
            if self.b.kind(&dest)? == Some(EntryKind::File) {
                self.b.remove(&staged)?;
                continue;
            }
            // Named as added before the move: a move that then fails is
            // counted until the next cleanup recomputes from the disk, an
            // over count that never lets the run exceed its limit.
            self.record.added.push(hex.clone());
            write_json(&self.b, &self.staging_dir(), TXN_FILE, &self.record)?;
            self.added += size;
            self.b
                .mkdir_all(&format!("{RESULTS_DIR}/{OBJECTS_DIR}/{}", &hex[..2]))?;
            self.b.rename_within(&staged, &dest)?;
        }
        self.moved = true;
        Ok(())
    }

    /// Moves the objects if not yet, then writes the manifest and renames
    /// it to `snapshots/<id>.json`, under the results lock. Every entry must
    /// name an object this transaction staged. Returns the snapshot id; the
    /// caller then journals the stamp.
    pub fn publish(&mut self, manifest: &SnapshotManifest) -> io::Result<String> {
        let unstaged = manifest
            .entries
            .iter()
            .any(|e| digest_hex(&e.digest).is_none_or(|hex| !self.staged.contains_key(hex)));
        if unstaged {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "a manifest entry names an object this transaction did not stage",
            ));
        }
        let bytes = canonical_bytes(manifest);
        if bytes.len() > MAX_MANIFEST_BYTES {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("the manifest is {} bytes, over the limit", bytes.len()),
            ));
        }
        self.move_objects()?;
        let id = snapshot_id(&bytes);
        let _guard = lock_results(&self.b)?;
        self.record.manifest = Some(id.clone());
        write_json(&self.b, &self.staging_dir(), TXN_FILE, &self.record)?;
        let name = format!("{id}.json");
        self.b.write_new(&self.staging_dir(), &name, &bytes)?;
        self.b
            .mkdir_all(&format!("{RESULTS_DIR}/{SNAPSHOTS_DIR}"))?;
        self.b.rename_within(
            &format!("{}/{name}", self.staging_dir()),
            &snapshot_rel(&id),
        )?;
        self.manifest_bytes = bytes.len() as u64;
        Ok(id)
    }

    /// Ends a transaction whose stamp is journaled (or that published
    /// nothing): removes its staging, settles its reservation and releases
    /// its lock.
    pub fn end(self) -> io::Result<()> {
        self.close(false)
    }

    /// Ends a transaction whose stamp was not journaled: its manifest is
    /// removed as well. Its objects stay until cleanup, since another
    /// snapshot may have found one of them in place.
    pub fn abort(self) -> io::Result<()> {
        self.close(true)
    }

    fn close(self, abort: bool) -> io::Result<()> {
        let _guard = lock_results(&self.b)?;
        let mut added = self.added + self.manifest_bytes;
        if abort && let Some(id) = &self.record.manifest {
            self.b.remove(&snapshot_rel(id))?;
            added = self.added;
        }
        self.b.remove_tree(&self.staging_dir())?;
        let mut r = read_reservation(&self.b)?;
        let open = r.open.remove(&self.record.token).unwrap_or(self.reserved);
        r.reserved_bytes = r.reserved_bytes.saturating_sub(open);
        r.committed_bytes += added;
        write_reservation(&self.b, &r)?;
        self.b.remove(&format!(
            "{}/{}",
            staging_rel(),
            txn_lock_name(&self.record.token)
        ))?;
        Ok(())
    }
}

/// Drops the open reservation of `token`, best effort. The caller holds
/// the results lock.
fn release_locked(b: &Beneath, token: &str) {
    if let Ok(mut r) = read_reservation(b)
        && let Some(n) = r.open.remove(token)
    {
        r.reserved_bytes = r.reserved_bytes.saturating_sub(n);
        let _ = write_reservation(b, &r);
    }
}

/// Adds `bytes` to the reservation of `token` when the run has room;
/// `false` when it has not. The caller holds the results lock.
fn reserve_locked(b: &Beneath, token: &str, bytes: u64) -> io::Result<bool> {
    let mut r = read_reservation(b)?;
    if r.committed_bytes + r.reserved_bytes + bytes > MAX_RUN_BYTES {
        return Ok(false);
    }
    *r.open.entry(token.to_string()).or_default() += bytes;
    r.reserved_bytes += bytes;
    write_reservation(b, &r)?;
    Ok(true)
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    /// `begin` reserved room and then could not record its transaction:
    /// the room goes back and its lock file is gone, so a failed begin
    /// never keeps room nothing can release.
    /// A passive probe (a doctor's survey) that holds a new transaction's
    /// lock file shared in the instant after it is created does not fail
    /// the capture: `begin` waits it out.
    #[test]
    fn a_begin_waits_out_a_probe_of_its_new_lock_file() {
        let t = tempfile::tempdir().unwrap();
        let (held_tx, held_rx) = std::sync::mpsc::channel();
        let staging = t.path().join(staging_rel());
        crate::fsutil::beneath::tests::plant_after_lock_file(move |path| {
            // The results lock comes first; the probe is of the
            // transaction's own lock file.
            if path.parent() != Some(staging.as_path()) {
                return false;
            }
            let path = path.to_path_buf();
            let (tx, rx) = std::sync::mpsc::channel();
            std::thread::spawn(move || {
                let probe = std::fs::File::open(&path).unwrap();
                probe.lock_shared().unwrap();
                tx.send(()).unwrap();
                std::thread::sleep(std::time::Duration::from_millis(200));
                drop(probe);
            });
            // The probe holds the file before `begin` tries to lock it.
            rx.recv().unwrap();
            held_tx.send(()).unwrap();
            true
        });
        let txn = ResultTxn::begin(t.path());
        assert!(held_rx.try_recv().is_ok(), "the probe ran");
        txn.expect("begin waits out a probe").abort().unwrap();
    }

    #[test]
    fn a_begin_that_cannot_record_its_transaction_gives_its_room_back() {
        let t = tempfile::tempdir().unwrap();
        let staging = t.path().join(staging_rel());
        let swap = staging.clone();
        // The reservation write is the first write of `begin`; before it,
        // the token's directory name is taken by a file.
        crate::fsutil::beneath::tests::plant_between(move || {
            for e in std::fs::read_dir(&swap).unwrap() {
                let name = e.unwrap().file_name().into_string().unwrap();
                if let Some(token) = name.strip_suffix(".lock") {
                    std::fs::write(swap.join(token), b"in the way").unwrap();
                }
            }
        });
        assert!(ResultTxn::begin(t.path()).is_err());
        let b = Beneath::open(t.path()).unwrap();
        let r = read_reservation(&b).unwrap();
        assert_eq!(r.reserved_bytes, 0);
        assert!(r.open.is_empty());
        let locks = std::fs::read_dir(&staging)
            .unwrap()
            .filter(|e| {
                e.as_ref()
                    .unwrap()
                    .file_name()
                    .to_string_lossy()
                    .ends_with(".lock")
            })
            .count();
        assert_eq!(locks, 0);
    }
}
