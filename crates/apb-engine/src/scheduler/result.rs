//! Result capture at settlement (see [`apb_core::result_store`]).
//!
//! An execution of a node that declares `results:` gets one snapshot when
//! it settles with `succeeded`, `failed` or `timed_out`: its output text,
//! its declared fields when the output is a JSON object, and (for a
//! `succeeded` status only) every declared file as it is now, changed or
//! not. A list item's settlement after its step ran gets an item snapshot
//! of the item's full output and fields; declared files are captured once,
//! on the list node's own settlement. Write-offs (`cancelled`, the
//! unattended and deferred closings) never capture.
//!
//! Every source is read through [`Beneath`] from its scope root (the run's
//! working tree or the run directory), never following a link at any
//! component, never opening a FIFO or a device, refusing a file with more
//! than one hard link unread, and never opening a secret-shaped name. Each
//! text is scrubbed of the run tree's secret values and token shapes
//! ([`RunRedactor::secret_text`]); when the run's secret values cannot all
//! be read, nothing is read or stored and every part is omitted
//! `scrub_unavailable`. The limits of [`apb_core::result_store`] stop work,
//! not only storage, and a stopped capture is `incomplete`.
//!
//! Capture never fails or delays the node beyond its limits: an I/O error
//! drops the transaction, writes a `result_capture_failed` diagnostic and
//! settles the node without a stamp.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use apb_core::fsutil::beneath::{Beneath, CappedRead, EntryKind, ListBudget, ReadRefusal};
use apb_core::result_store::{
    self as store, EntryPart, IncompleteReason, OmissionReason, OmittedItem, OmittedMore,
    OutputOmitted, ResultFormat, ResultScope, ResultStamp, ResultTxn, SnapshotEntry,
    SnapshotManifest, SnapshotOmission, SnapshotProvenance, SnapshotSubject, Staged,
};
use apb_core::schema::{Node, ResultDecl};
use serde_json::{Map, Value};

use crate::error::EngineError;
use crate::event::{Event, EventPayload};
use crate::run_redact::RunRedactor;
use crate::state::NodeStatus;

/// One settlement to capture.
pub(crate) struct Request<'a> {
    pub run_dir: &'a Path,
    /// The run's working tree: the root of `scope: workspace`.
    pub workdir: &'a Path,
    pub node: &'a Node,
    pub status: NodeStatus,
    /// The settlement's full output text.
    pub output: &'a str,
    /// The node was a cache hit: the files are the restored ones.
    pub cache_hit: bool,
    /// For an item snapshot: the item and its declared output fields.
    pub item: Option<ItemSubject<'a>>,
    /// The node is snapshotted ([`snapshotted`]).
    pub snapshotted: bool,
}

/// Whether node `node_id` takes snapshots: it declares `results:`, or a
/// gate's `approves` or a node's `requires_approval.of` names it, so its
/// output and fields are there to bind
/// ([`apb_core::validate::snapshotted_nodes`]).
pub(crate) fn snapshotted(playbook: &apb_core::schema::Playbook, node_id: &str) -> bool {
    apb_core::validate::snapshotted_nodes(playbook).contains(node_id)
}

/// The list item an item snapshot is of.
pub(crate) struct ItemSubject<'a> {
    pub id: &'a str,
    /// The list's declared item outputs; empty keeps the whole object.
    pub fields: &'a [apb_core::schema::FieldDecl],
}

/// A published snapshot whose stamp is not journaled yet.
pub(crate) struct Taken {
    txn: ResultTxn,
    stamp: ResultStamp,
    run_dir: std::path::PathBuf,
    subject: SnapshotSubject,
}

impl std::fmt::Debug for Taken {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Taken")
            .field("stamp", &self.stamp)
            .finish_non_exhaustive()
    }
}

/// Whether a settlement with `status` of a node that is `snapshotted`
/// takes a snapshot.
pub(crate) fn wanted(snapshotted: bool, status: NodeStatus) -> bool {
    snapshotted
        && matches!(
            status,
            NodeStatus::Succeeded | NodeStatus::Failed | NodeStatus::TimedOut
        )
}

/// Captures the settlement of node `node_id` itself (not a list item):
/// the call every settlement writer of node work makes.
pub(crate) fn of_node(
    playbook: &apb_core::schema::Playbook,
    run_dir: &Path,
    workdir: &Path,
    node_id: &str,
    status: NodeStatus,
    output: &str,
    cache_hit: bool,
) -> Option<Taken> {
    capture(Request {
        run_dir,
        workdir,
        node: playbook.node(node_id)?,
        status,
        output,
        cache_hit,
        item: None,
        snapshotted: snapshotted(playbook, node_id),
    })
}

/// Captures one settlement; `None` when none is taken or the capture
/// failed (the failure is a diagnostic, never an error of the node).
pub(crate) fn capture(req: Request<'_>) -> Option<Taken> {
    if !wanted(req.snapshotted, req.status) {
        return None;
    }
    let events = crate::event::read_all_lossy_tail(req.run_dir).unwrap_or_default();
    let subject = subject_of(&events, &req.node.id, req.item.as_ref().map(|i| i.id));
    let after_seq = events.last().map_or(0, |e| e.seq);
    match take(&req, &events, subject.clone()) {
        Ok(taken) => Some(taken),
        Err(e) => {
            crate::diagnostics::capture::record(
                req.run_dir,
                after_seq,
                &subject,
                &format!("snapshot not stored: {e}"),
            );
            None
        }
    }
}

/// Journals a settlement with the stamp of `taken`, then ends its
/// transaction. When the append reports an error the journal decides: a
/// stamp it holds after all (the line was written, a later step of the
/// append failed) ends the transaction, a journal without it aborts it, so
/// no snapshot stays that the journal does not name, and one it names is
/// never removed. A journal that cannot be read whole (a torn last line
/// may hold the stamp) leaves the transaction for cleanup, which resolves
/// it by the journal.
pub(crate) fn journal<T>(
    taken: Option<Taken>,
    append: impl FnOnce(Option<ResultStamp>) -> Result<T, EngineError>,
) -> Result<T, EngineError> {
    let Some(taken) = taken else {
        return append(None);
    };
    let Taken {
        txn,
        stamp,
        run_dir,
        subject,
    } = taken;
    let snapshot_id = stamp.snapshot_id.clone();
    match append(Some(stamp)) {
        Ok(v) => {
            // The stamp is journaled: a staging left behind by a failed end
            // is completed by cleanup.
            if let Err(e) = txn.end() {
                let after = crate::event::read_all_lossy_tail(&run_dir)
                    .ok()
                    .and_then(|ev| ev.last().map(|e| e.seq))
                    .unwrap_or(0);
                crate::diagnostics::capture::record(
                    &run_dir,
                    after,
                    &subject,
                    &format!("snapshot stored, its transaction not closed: {e}"),
                );
            }
            Ok(v)
        }
        Err(e) => {
            // Read whole: a torn last line may hold the stamp, so a journal
            // that does not read whole leaves the transaction too.
            match crate::event::read_all(&run_dir) {
                Ok(events) if events.iter().any(|ev| stamps(ev, &snapshot_id)) => {
                    let _ = txn.end();
                }
                Ok(_) => {
                    let _ = txn.abort();
                }
                // Dropped open: its staging and lock stay for cleanup,
                // which reads the journal textually.
                Err(_) => drop(txn),
            }
            Err(e)
        }
    }
}

/// Whether `e` is a settlement carrying the stamp of `snapshot_id`.
fn stamps(e: &Event, snapshot_id: &str) -> bool {
    match &e.payload {
        EventPayload::NodeFinished {
            result: Some(s), ..
        }
        | EventPayload::ListItemSettled {
            result: Some(s), ..
        } => s.snapshot_id == snapshot_id,
        _ => false,
    }
}

/// The subject of a snapshot taken now: the open execution of `node` (the
/// one the settlement about to be journaled ends), the current list visit
/// and the item, and the latest attempt of the node or the item.
fn subject_of(events: &[Event], node: &str, item: Option<&str>) -> SnapshotSubject {
    let index = crate::identity::index(events);
    let execs = index.executions(node);
    let execution = match execs.last() {
        Some(x) if x.is_open() => x.ordinal,
        _ => execs.len() as u32 + 1,
    };
    let begin = execs
        .last()
        .filter(|x| x.is_open())
        .map_or(0, |x| x.begin_seq);
    let visit_seq = item.and_then(|_| {
        crate::identity::list_visits(events)
            .into_iter()
            .rev()
            .find(|v| v.node == node)
            .map(|v| v.started_seq)
    });
    let attempt = events.iter().rev().find_map(|e| match &e.payload {
        EventPayload::AttemptStarted {
            node: n, item: i, ..
        } if n == node && i.as_deref() == item && e.seq >= begin => Some(e.seq),
        _ => None,
    });
    SnapshotSubject {
        node: node.to_string(),
        execution,
        visit_seq,
        item: item.map(str::to_string),
        attempt,
    }
}

/// The seq of the open execution's `artifacts_committed`, if it wrote one.
fn commits_seq(events: &[Event], node: &str) -> Option<u64> {
    let index = crate::identity::index(events);
    let begin = index
        .executions(node)
        .last()
        .filter(|x| x.is_open())?
        .begin_seq;
    events.iter().rev().find_map(|e| match &e.payload {
        EventPayload::ArtifactsCommitted { node: n, .. } if n == node && e.seq >= begin => {
            Some(e.seq)
        }
        _ => None,
    })
}

/// What one capture builds up before the manifest is written.
struct Builder<'r> {
    redactor: &'r RunRedactor,
    txn: ResultTxn,
    entries: Vec<SnapshotEntry>,
    omitted: Vec<OmittedItem>,
    /// Which limits stopped work.
    stopped: BTreeSet<IncompleteReason>,
    /// The last text refused for storage was over the snapshot's own limit,
    /// not the run's.
    snapshot_full: bool,
    output_omitted: Option<OutputOmitted>,
    /// Paths the scrub changed, numbered in order of appearance.
    redacted_paths: BTreeMap<String, usize>,
}

impl Builder<'_> {
    /// `path` as the manifest may carry it: scrubbed, or `<redacted path n>`
    /// when the scrub changed it.
    fn path(&mut self, path: &str) -> String {
        match self.redactor.secret_text(path) {
            Ok(clean) if clean == path => clean,
            _ => {
                let next = self.redacted_paths.len() + 1;
                let n = *self.redacted_paths.entry(path.to_string()).or_insert(next);
                format!("{}{n}>", store::REDACTED_PATH_PREFIX)
            }
        }
    }

    fn omit(&mut self, part: EntryPart, file: Option<(ResultScope, &str)>, reason: OmissionReason) {
        let (scope, path) = match file {
            Some((scope, path)) => (Some(scope), Some(self.path(path))),
            None => (None, None),
        };
        self.omitted.push(OmittedItem {
            part: Some(part),
            scope,
            path,
            reason,
        });
    }

    /// Scrubs `text` and stages it: the digest, whether the scrub changed
    /// it, its size and the stored (scrubbed) text; `Err(reason)` when it
    /// was not stored.
    fn stage(
        &mut self,
        text: &str,
    ) -> std::io::Result<Result<(String, bool, u64, String), OmissionReason>> {
        if text.contains('\0') {
            return Ok(Err(OmissionReason::BinaryNotSupported));
        }
        let Ok(clean) = self.redactor.secret_text(text) else {
            return Ok(Err(OmissionReason::ScrubUnavailable));
        };
        let redacted = clean != text;
        match self.txn.stage_object(clean.as_bytes())? {
            Staged::Stored(digest) => {
                let size = clean.len() as u64;
                Ok(Ok((digest, redacted, size, clean)))
            }
            full @ (Staged::SnapshotFull | Staged::RunFull) => {
                self.snapshot_full = matches!(full, Staged::SnapshotFull);
                self.stopped.insert(IncompleteReason::StorageLimit);
                Ok(Err(OmissionReason::StorageLimit))
            }
        }
    }
}

/// The budgets one settlement's file capture shares across declarations.
struct Budgets {
    list: ListBudget,
    matches: usize,
    read: u64,
}

fn take(req: &Request<'_>, events: &[Event], subject: SnapshotSubject) -> std::io::Result<Taken> {
    let redactor = RunRedactor::for_run_dir(req.run_dir);
    let scrub_ok = redactor.secret_text("").is_ok();
    let txn = ResultTxn::begin(req.run_dir)?;
    let mut b = Builder {
        redactor: &redactor,
        txn,
        entries: Vec::new(),
        omitted: Vec::new(),
        stopped: BTreeSet::new(),
        snapshot_full: false,
        output_omitted: None,
        redacted_paths: BTreeMap::new(),
    };
    let result = fill(req, &mut b, scrub_ok);
    let manifest = match result {
        Ok(()) => manifest(req, events, subject.clone(), &mut b),
        Err(e) => {
            let _ = b.txn.abort();
            return Err(e);
        }
    };
    let Builder { mut txn, .. } = b;
    match txn.publish(&manifest) {
        Ok(snapshot_id) => {
            let stamp = ResultStamp {
                snapshot_id,
                entries: manifest.entries.len() as u32,
                bytes: manifest.entries.iter().map(|e| e.size).sum(),
                omitted: omitted_count(&manifest.omitted),
                incomplete: manifest.incomplete.is_some(),
            };
            Ok(Taken {
                txn,
                stamp,
                run_dir: req.run_dir.to_path_buf(),
                subject,
            })
        }
        Err(e) => {
            let _ = txn.abort();
            Err(e)
        }
    }
}

/// Every omission a manifest counts, the aggregate's included.
fn omitted_count(omitted: &[SnapshotOmission]) -> u32 {
    omitted
        .iter()
        .map(|o| match o {
            SnapshotOmission::Item(_) => 1,
            SnapshotOmission::More(m) => m.more,
        })
        .sum::<u64>()
        .min(u64::from(u32::MAX)) as u32
}

/// Stages the output, the fields and the files.
fn fill(req: &Request<'_>, b: &mut Builder<'_>, scrub_ok: bool) -> std::io::Result<()> {
    let decls: &[ResultDecl] = req.node.results.as_deref().unwrap_or(&[]);
    if !scrub_ok {
        // Fail closed: nothing is read, nothing stored, every part named.
        b.output_omitted = Some(OutputOmitted::ScrubUnavailable);
        if fields_text(req).is_some() {
            b.omit(EntryPart::Fields, None, OmissionReason::ScrubUnavailable);
        }
        if files_wanted(req) {
            for _ in decls {
                b.omit(EntryPart::File, None, OmissionReason::ScrubUnavailable);
            }
        }
        return Ok(());
    }
    // The output is reserved first.
    match b.stage(req.output)? {
        Ok((digest, redacted, size, _)) => b.entries.push(SnapshotEntry {
            part: EntryPart::Output,
            scope: None,
            path: None,
            size,
            digest,
            redacted,
            format: ResultFormat::Text,
            records_invalid: None,
        }),
        Err(OmissionReason::StorageLimit) => {
            b.output_omitted = Some(if b.snapshot_full {
                OutputOmitted::SnapshotLimit
            } else {
                OutputOmitted::RunLimit
            });
        }
        Err(reason) => b.omit(EntryPart::Output, None, reason),
    }
    if let Some(fields) = fields_text(req) {
        match scrub_fields(b.redactor, fields) {
            Some((text, redacted)) => match b.stage_scrubbed(&text)? {
                Ok((digest, size)) => b.entries.push(SnapshotEntry {
                    part: EntryPart::Fields,
                    scope: None,
                    path: None,
                    size,
                    digest,
                    redacted,
                    format: ResultFormat::Text,
                    records_invalid: None,
                }),
                Err(reason) => b.omit(EntryPart::Fields, None, reason),
            },
            None => b.omit(EntryPart::Fields, None, OmissionReason::ScrubUnavailable),
        }
    }
    if !files_wanted(req) {
        return Ok(());
    }
    if crate::scenario::session::is_scenario(req.run_dir) {
        // A scenario run executed nothing: the tree is not the node's result.
        for decl in decls {
            b.omit(
                EntryPart::File,
                Some((decl.scope, &decl.path)),
                OmissionReason::ScenarioRun,
            );
        }
        return Ok(());
    }
    files(req, b, decls)
}

impl Builder<'_> {
    /// Stages text the caller already scrubbed.
    fn stage_scrubbed(
        &mut self,
        text: &str,
    ) -> std::io::Result<Result<(String, u64), OmissionReason>> {
        match self.txn.stage_object(text.as_bytes())? {
            Staged::Stored(digest) => Ok(Ok((digest, text.len() as u64))),
            Staged::SnapshotFull | Staged::RunFull => {
                self.stopped.insert(IncompleteReason::StorageLimit);
                Ok(Err(OmissionReason::StorageLimit))
            }
        }
    }
}

/// Files are captured on a `succeeded` settlement of the node itself.
fn files_wanted(req: &Request<'_>) -> bool {
    req.item.is_none() && req.status == NodeStatus::Succeeded
}

/// The declared fields of the output as a JSON object, when it is one: a
/// node's `outputs.fields`, a list item's declared item outputs (all of
/// the object when the list declares none).
fn fields_text(req: &Request<'_>) -> Option<Map<String, Value>> {
    let keep: Vec<String> = match &req.item {
        Some(item) => apb_core::schema::field_names(item.fields),
        None => {
            let declared = req.node.outputs.as_ref().map(|o| o.fields.as_slice())?;
            if declared.is_empty() {
                return None;
            }
            apb_core::schema::field_names(declared)
        }
    };
    let Value::Object(obj) = serde_json::from_str::<Value>(req.output).ok()? else {
        return None;
    };
    Some(if keep.is_empty() {
        obj
    } else {
        obj.into_iter()
            .filter(|(k, _)| keep.iter().any(|f| f == k))
            .collect()
    })
}

/// The fields as compact JSON, scrubbed token by token (keys and strings,
/// and a number that spells a secret value becomes a string), so the
/// stored text stays one JSON document; with whether the scrub changed
/// anything. `None` when the scrub is unavailable.
fn scrub_fields(redactor: &RunRedactor, fields: Map<String, Value>) -> Option<(String, bool)> {
    let before = Value::Object(fields);
    let mut clean = before.clone();
    redactor.secret_json(&mut clean).ok()?;
    let changed = clean != before;
    Some((serde_json::to_string(&clean).ok()?, changed))
}

/// Whether a name matches a last-segment glob of `*` and `?`.
fn glob_match(pattern: &[u8], name: &[u8]) -> bool {
    let (mut p, mut n) = (0, 0);
    let (mut star, mut mark) = (None, 0);
    while n < name.len() {
        if p < pattern.len() && (pattern[p] == b'?' || pattern[p] == name[n]) {
            p += 1;
            n += 1;
        } else if p < pattern.len() && pattern[p] == b'*' {
            star = Some(p);
            mark = n;
            p += 1;
        } else if let Some(s) = star {
            p = s + 1;
            mark += 1;
            n = mark;
        } else {
            return false;
        }
    }
    while p < pattern.len() && pattern[p] == b'*' {
        p += 1;
    }
    p == pattern.len()
}

fn is_glob(segment: &str) -> bool {
    segment.contains(['*', '?'])
}

/// Captures every declared file, in declaration order then path bytes.
fn files(req: &Request<'_>, b: &mut Builder<'_>, decls: &[ResultDecl]) -> std::io::Result<()> {
    let mut budgets = Budgets {
        list: ListBudget {
            remaining: store::MAX_ENTRIES_EXAMINED,
        },
        matches: 0,
        read: 0,
    };
    let mut seen: std::collections::HashSet<(ResultScope, String)> = Default::default();
    let mut anchors: std::collections::HashMap<ResultScope, Option<Beneath>> = Default::default();
    for decl in decls {
        let anchor = anchors.entry(decl.scope).or_insert_with(|| {
            let root = match decl.scope {
                ResultScope::Workspace => req.workdir,
                ResultScope::Run => req.run_dir,
            };
            Beneath::open(root).ok()
        });
        let Some(anchor) = anchor.as_ref() else {
            b.omit(
                EntryPart::File,
                Some((decl.scope, &decl.path)),
                OmissionReason::Unreadable,
            );
            continue;
        };
        let (dir, leaf) = match decl.path.rsplit_once('/') {
            Some((d, l)) => (d, l),
            None => ("", decl.path.as_str()),
        };
        let candidates: Vec<String> = if is_glob(leaf) {
            if b.stopped.contains(&IncompleteReason::EnumerationLimit) {
                continue;
            }
            let listing = match anchor.list_dir(dir, &mut budgets.list) {
                Ok(l) => l,
                // A missing or unreadable directory matches nothing.
                Err(_) => continue,
            };
            if listing.exhausted {
                b.stopped.insert(IncompleteReason::EnumerationLimit);
            }
            let mut names = Vec::new();
            for entry in listing.entries {
                let Some(name) = entry.name.to_str() else {
                    if glob_match(leaf.as_bytes(), entry.name.as_encoded_bytes()) {
                        b.omit(EntryPart::File, None, OmissionReason::NonUtf8Path);
                    }
                    continue;
                };
                if !glob_match(leaf.as_bytes(), name.as_bytes()) {
                    continue;
                }
                if dir.is_empty() && store::is_reserved_first(decl.scope, name) {
                    continue;
                }
                match entry.kind {
                    EntryKind::Dir => continue,
                    _ => names.push(join(dir, name)),
                }
            }
            names
        } else {
            vec![decl.path.clone()]
        };
        for rel in candidates {
            // V153 refuses these names outright; a capture checks again,
            // in any case, before it reads anything.
            if rel
                .split('/')
                .next()
                .is_some_and(|first| store::is_reserved_first(decl.scope, first))
            {
                continue;
            }
            if !seen.insert((decl.scope, rel.clone())) {
                continue;
            }
            file(anchor, decl, &rel, b, &mut budgets)?;
        }
    }
    Ok(())
}

fn join(dir: &str, name: &str) -> String {
    if dir.is_empty() {
        name.to_string()
    } else {
        format!("{dir}/{name}")
    }
}

/// Captures one candidate file.
fn file(
    anchor: &Beneath,
    decl: &ResultDecl,
    rel: &str,
    b: &mut Builder<'_>,
    budgets: &mut Budgets,
) -> std::io::Result<()> {
    let at = Some((decl.scope, rel));
    if budgets.matches >= store::MAX_MATCHES {
        b.stopped.insert(IncompleteReason::MatchLimit);
        b.omit(EntryPart::File, at, OmissionReason::MatchLimit);
        return Ok(());
    }
    budgets.matches += 1;
    let name = rel.rsplit('/').next().unwrap_or(rel);
    if store::is_secret_shaped(name) {
        b.omit(EntryPart::File, at, OmissionReason::SecretShaped);
        return Ok(());
    }
    if budgets.read >= store::MAX_READ_BYTES {
        b.stopped.insert(IncompleteReason::ReadLimit);
        b.omit(EntryPart::File, at, OmissionReason::ReadLimit);
        return Ok(());
    }
    let cap = store::MAX_FILE_BYTES.min(store::MAX_READ_BYTES - budgets.read);
    let bytes = match anchor.read_regular_capped(rel, cap) {
        Ok(CappedRead::Read { bytes, .. }) => bytes,
        Ok(CappedRead::Refused(why)) => {
            let reason = match why {
                ReadRefusal::Missing => OmissionReason::NotFound,
                ReadRefusal::Link => OmissionReason::Link,
                ReadRefusal::NotRegular => OmissionReason::NotRegularFile,
                ReadRefusal::HardLink => OmissionReason::HardLink,
                ReadRefusal::TooLarge { size } if size > store::MAX_FILE_BYTES => {
                    OmissionReason::TooLarge
                }
                ReadRefusal::TooLarge { .. } => {
                    b.stopped.insert(IncompleteReason::ReadLimit);
                    budgets.read = store::MAX_READ_BYTES;
                    OmissionReason::ReadLimit
                }
            };
            b.omit(EntryPart::File, at, reason);
            return Ok(());
        }
        Err(_) => {
            b.omit(EntryPart::File, at, OmissionReason::Unreadable);
            return Ok(());
        }
    };
    budgets.read += bytes.len() as u64;
    let Ok(text) = std::str::from_utf8(&bytes) else {
        b.omit(EntryPart::File, at, OmissionReason::BinaryNotSupported);
        return Ok(());
    };
    match b.stage(text)? {
        Ok((digest, redacted, size, stored)) => {
            // A declared package that does not parse as stored (after the
            // scrub) is kept as plain text with its first problem
            // ([`apb_core::records`]).
            let records_invalid = match decl.format {
                ResultFormat::Records => apb_core::records::parse(&stored).err(),
                ResultFormat::Text => None,
            };
            let path = b.path(rel);
            b.entries.push(SnapshotEntry {
                part: EntryPart::File,
                scope: Some(decl.scope),
                path: Some(path),
                size,
                digest,
                redacted,
                format: if records_invalid.is_some() {
                    ResultFormat::Text
                } else {
                    decl.format
                },
                records_invalid,
            });
        }
        Err(reason) => b.omit(EntryPart::File, at, reason),
    }
    Ok(())
}

/// The manifest of what `b` staged.
fn manifest(
    req: &Request<'_>,
    events: &[Event],
    subject: SnapshotSubject,
    b: &mut Builder<'_>,
) -> SnapshotManifest {
    let scenario = crate::scenario::session::is_scenario(req.run_dir);
    let provenance = SnapshotProvenance {
        cache_hit: req.cache_hit,
        head: if scenario || req.item.is_some() {
            None
        } else {
            super::provenance::head(req.workdir)
        },
        commits_seq: if req.item.is_some() {
            None
        } else {
            commits_seq(events, &req.node.id)
        },
    };
    let mut omitted: Vec<SnapshotOmission> = Vec::new();
    let mut more = OmittedMore {
        more: 0,
        reasons: BTreeMap::new(),
    };
    for (i, o) in std::mem::take(&mut b.omitted).into_iter().enumerate() {
        if i < store::MAX_OMISSION_RECORDS {
            omitted.push(SnapshotOmission::Item(o));
        } else {
            more.more += 1;
            *more.reasons.entry(o.reason).or_default() += 1;
        }
    }
    if more.more > 0 {
        omitted.push(SnapshotOmission::More(more));
    }
    // The limit that leaves the most unknown wins: an enumeration that
    // stopped did not even see what it left.
    let incomplete = [
        IncompleteReason::EnumerationLimit,
        IncompleteReason::MatchLimit,
        IncompleteReason::ReadLimit,
        IncompleteReason::StorageLimit,
    ]
    .into_iter()
    .find(|r| b.stopped.contains(r));
    SnapshotManifest {
        v: store::MANIFEST_VERSION,
        run_id: req
            .run_dir
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default(),
        subject,
        provenance,
        entries: std::mem::take(&mut b.entries),
        omitted,
        incomplete,
        output_omitted: b.output_omitted,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use apb_core::result_store::{SnapshotProvenance, snapshot_rel};

    /// The stored fields stay one JSON document: a secret in a number
    /// becomes the string `[redacted]`, and a secret with characters JSON
    /// escapes is found in its decoded spelling.
    #[test]
    fn scrubbed_fields_stay_valid_json() {
        let t = tempfile::tempdir().unwrap();
        let quoted = "pa\"ss\\wd-42";
        let token = format!("ghp_{}", "a1B2c3D4e5".repeat(4));
        let redactor = RunRedactor::with_workspace(
            vec![
                apb_core::secret::Secret::new("48291736"),
                apb_core::secret::Secret::new(quoted),
            ],
            t.path(),
            t.path(),
        );
        let fields: Map<String, Value> = serde_json::from_value(serde_json::json!({
            "pin": 48291736,
            "note": format!("from {quoted}"),
            "key": token,
            "api_key": "9f86d081884c7d659a2feaa0c55ad015",
            "access_token": 771200934455u64,
            "ok": true,
        }))
        .unwrap();
        let (text, redacted) = scrub_fields(&redactor, fields).unwrap();
        assert!(redacted);
        let v: Value = serde_json::from_str(&text).unwrap_or_else(|e| panic!("{e}: {text}"));
        assert_eq!(v["pin"], "[redacted]");
        assert_eq!(v["note"], "from [redacted]");
        assert_eq!(v["ok"], true);
        let key = v["key"].as_str().unwrap();
        assert!(!key.contains(&token), "{key}");
        assert!(!text.contains("ss\\\\wd"), "{text}");
        // A value known only by its key name is still found: the key gives
        // the context, as it does in the text.
        assert!(!text.contains("9f86d081884c7d659a2feaa0c55ad015"), "{text}");
        assert!(!text.contains("771200934455"), "{text}");
        assert!(
            v["api_key"].is_string() && v["access_token"].is_string(),
            "{text}"
        );
    }

    /// A published transaction of one object, ready to be journaled.
    fn taken(run_dir: &Path) -> Taken {
        let mut txn = ResultTxn::begin(run_dir).unwrap();
        let Staged::Stored(digest) = txn.stage_object(b"kept text").unwrap() else {
            panic!("staged");
        };
        let subject = SnapshotSubject {
            node: "work".into(),
            execution: 1,
            visit_seq: None,
            item: None,
            attempt: None,
        };
        let manifest = SnapshotManifest {
            v: store::MANIFEST_VERSION,
            run_id: "run-1".into(),
            subject: subject.clone(),
            provenance: SnapshotProvenance::default(),
            entries: vec![SnapshotEntry {
                part: EntryPart::Output,
                scope: None,
                path: None,
                size: 9,
                digest,
                redacted: false,
                format: ResultFormat::Text,
                records_invalid: None,
            }],
            omitted: Vec::new(),
            incomplete: None,
            output_omitted: None,
        };
        let snapshot_id = txn.publish(&manifest).unwrap();
        Taken {
            txn,
            stamp: ResultStamp {
                snapshot_id,
                entries: 1,
                bytes: 9,
                omitted: 0,
                incomplete: false,
            },
            run_dir: run_dir.to_path_buf(),
            subject,
        }
    }

    fn settled(stamp: Option<ResultStamp>) -> EventPayload {
        EventPayload::node_settled("work", "succeeded", 1, "done", Vec::new(), stamp)
    }

    /// The append wrote the stamped settlement and then failed (a later
    /// step of the append, such as the deferral guard): the journal names
    /// the snapshot, so it stays and its transaction ends.
    #[test]
    fn an_append_that_wrote_the_stamp_then_failed_keeps_the_snapshot() {
        let t = tempfile::tempdir().unwrap();
        let run = t.path().join("run-1");
        std::fs::create_dir_all(&run).unwrap();
        let taken = taken(&run);
        let id = taken.stamp.snapshot_id.clone();
        let out = journal(Some(taken), |stamp| {
            let mut log = crate::event::EventLog::open(&run).unwrap();
            log.append(settled(stamp)).unwrap();
            Err::<(), _>(EngineError::Anomaly("the guard write failed".into()))
        });
        assert!(out.is_err());
        assert!(
            run.join(snapshot_rel(&id)).exists(),
            "a stamped snapshot is kept"
        );
        let staging = std::fs::read_dir(run.join("results/staging"))
            .unwrap()
            .count();
        assert_eq!(staging, 0, "the transaction ended");
    }

    /// An append that left a torn last line may have written the stamp
    /// in it: the transaction is left open for cleanup, which reads the
    /// journal textually, never aborted.
    #[test]
    fn an_append_that_left_a_torn_line_leaves_the_transaction_for_cleanup() {
        let t = tempfile::tempdir().unwrap();
        let run = t.path().join("run-1");
        std::fs::create_dir_all(&run).unwrap();
        let taken = taken(&run);
        let id = taken.stamp.snapshot_id.clone();
        let out = journal(Some(taken), |_| {
            use std::io::Write as _;
            let mut f = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(run.join("events.jsonl"))
                .unwrap();
            write!(f, "{{\"seq\":1,\"result\":{{\"snapshot_id\":\"{id}\"").unwrap();
            Err::<(), _>(EngineError::Anomaly("the write was cut".into()))
        });
        assert!(out.is_err());
        assert!(
            run.join(snapshot_rel(&id)).exists(),
            "the snapshot a torn line may name is kept"
        );
        let staging = std::fs::read_dir(run.join("results/staging"))
            .unwrap()
            .count();
        assert!(staging > 0, "the transaction is left for cleanup");
    }

    /// An append that wrote nothing aborts: no snapshot stays unnamed.
    #[test]
    fn an_append_that_wrote_nothing_aborts() {
        let t = tempfile::tempdir().unwrap();
        let run = t.path().join("run-1");
        std::fs::create_dir_all(&run).unwrap();
        let taken = taken(&run);
        let id = taken.stamp.snapshot_id.clone();
        let out = journal(Some(taken), |_| {
            Err::<(), _>(EngineError::Anomaly("the disk is full".into()))
        });
        assert!(out.is_err());
        assert!(!run.join(snapshot_rel(&id)).exists());
    }

    #[test]
    fn last_segment_globs_match_by_star_and_question_mark() {
        assert!(glob_match(b"*.md", b"report.md"));
        assert!(glob_match(b"*", b"x"));
        assert!(glob_match(b"r?port.*", b"report.txt"));
        assert!(glob_match(b"a*b*c", b"aXXbYYc"));
        assert!(!glob_match(b"*.md", b"report.txt"));
        assert!(!glob_match(b"?", b""));
        assert!(!glob_match(b"a*b", b"aXXc"));
    }
}
