//! What an evidence session reads: the latest stamped snapshot of each
//! `evidence.from` node (for a list node, its node snapshot and the item
//! snapshots of its latest visit), as views of records.
//!
//! Per snapshot:
//!
//! - `output`: one record, the output entry;
//! - `fields`: one record, the fields entry;
//! - `file`: one view per plain text file entry, one record (a `format:
//!   records` entry that is no package is one too);
//! - one view per package: one record per package record, its kind
//!   `records`, or, when the declaration names `kinds`, one view per record
//!   kind, of that kind.
//!
//! A record is `r:<snapshot_id>/<entry>`, or
//! `r:<snapshot_id>/<entry>@<record id>` for a package record
//! ([`apb_core::result_store::Citation`]). Every text is served through the
//! run redactor; a record the redactor changes is served redacted and is
//! not citable, so every citable span is a range of stored bytes. A view of
//! a snapshot capture marked incomplete, or that omitted anything, is
//! incomplete; a package view is incomplete also when its collector says
//! so.

use std::collections::BTreeMap;
use std::path::Path;

use apb_core::result_store::{Citation, EntryId, ResultFormat, SnapshotManifest};
use serde::{Deserialize, Serialize};

use crate::citation::ViewSet;
use crate::citation::views::{SourceRecord, SourceView};
use crate::error::EngineError;
use crate::event::{Event, EventPayload};
use crate::run_redact::RunRedactor;

/// The kind of a view that is no package: the output.
pub const OUTPUT: &str = "output";
/// The kind of the fields view.
pub const FIELDS: &str = "fields";
/// The kind of a plain text file view.
pub const FILE: &str = "file";
/// The kind of a package view when the declaration names no kinds.
pub const RECORDS: &str = "records";

/// One snapshot a session reads.
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BoundSource {
    pub node: String,
    pub execution: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub item: Option<String>,
    pub snapshot_id: String,
}

/// One record of a view as a session file keeps it: its reference, the
/// digest of its text as served, and whether a citation may rest on it.
/// No text.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecordMeta {
    #[serde(rename = "ref")]
    pub r#ref: String,
    pub digest: String,
    pub citable: bool,
}

/// One view as a session file keeps it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ViewMeta {
    pub id: String,
    pub kind: String,
    pub complete: bool,
    /// For a package view: what its collector says about its source.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub collector_complete: Option<bool>,
    pub records: Vec<RecordMeta>,
}

/// The views of a session: what a reader reads (texts as served) and what
/// the session file keeps.
pub struct Views {
    pub set: ViewSet<String>,
    pub meta: Vec<ViewMeta>,
}

/// The latest stamped snapshot of each node of `from`, in `from` order: a
/// node's latest `node_finished` stamp, and for a list node the latest
/// stamp of each item settled in its latest visit.
pub fn bind(events: &[Event], from: &[String]) -> Vec<(String, Option<String>, String)> {
    let mut out = Vec::new();
    for node in from {
        let mut latest: Option<String> = None;
        let mut visit = 0u64;
        let mut items: BTreeMap<String, String> = BTreeMap::new();
        for e in events {
            match &e.payload {
                EventPayload::NodeFinished {
                    node: n,
                    result: Some(stamp),
                    ..
                } if n == node => latest = Some(stamp.snapshot_id.clone()),
                EventPayload::ListStarted { node: n, .. } if n == node => {
                    visit = e.seq;
                    items.clear();
                }
                EventPayload::ListItemSettled {
                    node: n,
                    item,
                    result: Some(stamp),
                    ..
                } if n == node && visit > 0 => {
                    items.insert(item.clone(), stamp.snapshot_id.clone());
                }
                _ => {}
            }
        }
        if let Some(id) = latest {
            out.push((node.clone(), None, id));
        }
        out.extend(
            items
                .into_iter()
                .map(|(item, id)| (node.clone(), Some(item), id)),
        );
    }
    out
}

fn reference(snapshot: &str, entry: &EntryId, record: Option<&str>) -> String {
    Citation {
        snapshot_id: snapshot.to_string(),
        entry: entry.clone(),
        record: record.map(str::to_string),
        range: None,
    }
    .to_string()
}

/// One view under construction.
struct Building<'r> {
    redactor: &'r RunRedactor,
    views: Vec<SourceView<String>>,
    meta: Vec<ViewMeta>,
}

impl Building<'_> {
    /// Adds a view of `records` (reference, stored text).
    fn push(
        &mut self,
        id: String,
        kind: &str,
        complete: bool,
        collector_complete: Option<bool>,
        records: Vec<(String, &str)>,
    ) {
        let mut served = Vec::with_capacity(records.len());
        let mut metas = Vec::with_capacity(records.len());
        for (r, stored) in records {
            let text = self.redactor.redact_str(stored);
            metas.push(RecordMeta {
                r#ref: r.clone(),
                digest: apb_core::content::sha256_hex(text.as_bytes()),
                citable: text == stored,
            });
            served.push(SourceRecord { r#ref: r, text });
        }
        self.views.push(SourceView {
            id: id.clone(),
            kind: kind.to_string(),
            complete,
            records: served,
        });
        self.meta.push(ViewMeta {
            id,
            kind: kind.to_string(),
            complete,
            collector_complete,
            records: metas,
        });
    }

    /// The views of one snapshot.
    fn snapshot(
        &mut self,
        src: &BoundSource,
        m: &SnapshotManifest,
        texts: &[String],
        split_kinds: bool,
    ) {
        let whole = m.incomplete.is_none() && m.omitted.is_empty() && m.output_omitted.is_none();
        let item = src
            .item
            .as_deref()
            .map(|i| format!("[{i}]"))
            .unwrap_or_default();
        let base = format!("{}.{}{item}", src.node, src.execution);
        let snap = src.snapshot_id.as_str();
        let mut have_output = false;
        let mut have_fields = false;
        for (e, text) in m.entries.iter().zip(texts) {
            let Some(entry) = e.id() else { continue };
            // A path the redactor changes would spell a secret in a
            // reference: such an entry is left out, and the snapshot's
            // views say they are not whole.
            if let EntryId::File { path, .. } = &entry
                && self.redactor.redact_str(path) != *path
            {
                continue;
            }
            let id = format!("{base}/{entry}");
            let plain = vec![(reference(snap, &entry, None), text.as_str())];
            match &entry {
                EntryId::Output => {
                    have_output = true;
                    self.push(id, OUTPUT, whole, None, plain);
                }
                EntryId::Fields => {
                    have_fields = true;
                    self.push(id, FIELDS, whole, None, plain);
                }
                EntryId::File { .. } => {
                    let package = (e.format == ResultFormat::Records
                        && e.records_invalid.is_none())
                    .then(|| apb_core::records::parse(text).ok())
                    .flatten();
                    match package {
                        None => self.push(id, FILE, whole, None, plain),
                        Some(p) => {
                            let said = p.header.complete;
                            let records = |kind: Option<&str>| -> Vec<(String, &str)> {
                                p.records
                                    .iter()
                                    .filter(|r| kind.is_none_or(|k| r.kind == k))
                                    .map(|r| {
                                        (reference(snap, &entry, Some(&r.id)), r.text.as_str())
                                    })
                                    .collect()
                            };
                            if split_kinds {
                                let mut kinds: Vec<&str> =
                                    p.records.iter().map(|r| r.kind.as_str()).collect();
                                kinds.sort_unstable();
                                kinds.dedup();
                                for k in kinds {
                                    let recs = records(Some(k));
                                    self.push(
                                        format!("{id}~{k}"),
                                        k,
                                        whole && said,
                                        Some(said),
                                        recs,
                                    );
                                }
                            } else {
                                let recs = records(None);
                                self.push(id, RECORDS, whole && said, Some(said), recs);
                            }
                        }
                    }
                }
            }
        }
        // An output or fields part that was not stored is an empty view
        // that is not whole: an absence never rests on it.
        if !have_output {
            self.push(format!("{base}/output"), OUTPUT, false, None, Vec::new());
        }
        if !have_fields && m.output_omitted.is_some() {
            self.push(format!("{base}/fields"), FIELDS, false, None, Vec::new());
        }
    }
}

/// The views of `sources` at `run_dir`, every text through the run
/// redactor. `split_kinds`: a package is one view per record kind (the
/// declaration names `kinds`), else one `records` view. Refused while the
/// run redactor withholds (`secrets_unresolved`) and when a snapshot no
/// longer reads whole.
pub fn load(
    run_dir: &Path,
    sources: &[BoundSource],
    split_kinds: bool,
) -> Result<Views, EngineError> {
    let redactor = RunRedactor::for_run_dir(run_dir);
    if redactor.withholds() {
        return Err(crate::run_redact::SecretsUnresolved.into());
    }
    let mut b = Building {
        redactor: &redactor,
        views: Vec::new(),
        meta: Vec::new(),
    };
    for src in sources {
        let (m, texts) = crate::result_view::stored_texts(run_dir, &src.snapshot_id)
            .map_err(|e| EngineError::Conflict(format!("snapshot `{}`: {e}", src.snapshot_id)))?;
        b.snapshot(src, &m, &texts, split_kinds);
    }
    let uncitable: Vec<String> = b
        .meta
        .iter()
        .flat_map(|v| v.records.iter())
        .filter(|r| !r.citable)
        .map(|r| r.r#ref.clone())
        .collect();
    Ok(Views {
        set: ViewSet::new(b.views).with_uncitable(uncitable),
        meta: b.meta,
    })
}

/// The sources of `bound` with their execution ordinals, read from their
/// manifests.
pub fn sources_of(
    run_dir: &Path,
    bound: Vec<(String, Option<String>, String)>,
) -> Result<Vec<BoundSource>, EngineError> {
    bound
        .into_iter()
        .map(|(node, item, snapshot_id)| {
            let (m, _) = crate::result_view::stored_texts(run_dir, &snapshot_id)
                .map_err(|e| EngineError::Conflict(format!("snapshot `{snapshot_id}`: {e}")))?;
            Ok(BoundSource {
                node,
                execution: m.subject.execution,
                item,
                snapshot_id,
            })
        })
        .collect()
}
