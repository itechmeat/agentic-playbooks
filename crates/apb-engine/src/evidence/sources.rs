//! What an evidence session reads: the latest stamped snapshot of each
//! `from` node (for a list node, its node snapshot and the item snapshots
//! of its latest visit), as views of records.
//!
//! Per snapshot, a view of kind `output` (one record, the output entry), a
//! view of kind `fields`, one view of kind `file` per plain text file
//! entry (one record each) and, per records package, one view of kind
//! `records` (one record per package record), or, when the declaration
//! names `kinds`, one view per named kind (empty when the package has no
//! record of it). With `kinds`, only views of those kinds are built.
//!
//! A record is named `r:<snapshot_id>/<entry>` or, for a package record,
//! `r:<snapshot_id>/<entry>@<record id, percent-encoded>`. Its text is the
//! stored text through the run redactor; a record the redactor changes is
//! served redacted and never citable, so every citable span is a range of
//! stored bytes. A view is complete only when its snapshot is (no limit
//! stopped capture, nothing omitted) and, for a package, when the collector
//! says so.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use apb_core::records;
use apb_core::result_store::{Citation, EntryPart, ResultFormat};
use serde::{Deserialize, Serialize};

use crate::citation::views::{SourceRecord, SourceView, ViewSet};
use crate::result_view::{ResultReadError, ResultsView, Verified, stored_entries};
use crate::run_redact::RunRedactor;

/// The kind of an output view.
pub const OUTPUT: &str = "output";
/// The kind of a fields view.
pub const FIELDS: &str = "fields";
/// The kind of a plain text file view.
pub const FILE: &str = "file";
/// The kind of a package view when the declaration names no kinds.
pub const RECORDS: &str = "records";

/// One snapshot a session is bound to.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BoundSnapshot {
    pub snapshot_id: String,
    pub node: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub item: Option<String>,
}

/// The latest stamped, verified snapshots of the `from` nodes, in the
/// order the declaration names them.
pub fn bind(run_dir: &Path, from: &[String]) -> std::io::Result<Vec<BoundSnapshot>> {
    let view = ResultsView::load(run_dir)?;
    let rows: Vec<_> = view
        .rows
        .iter()
        .filter(|r| r.verified == Verified::Ok)
        .collect();
    let mut bound = Vec::new();
    for node in from {
        let mine = || rows.iter().filter(|r| &r.execution.node == node);
        if let Some(r) = mine()
            .filter(|r| r.item.is_none())
            .max_by_key(|r| r.settlement_seq)
        {
            bound.push(BoundSnapshot {
                snapshot_id: r.snapshot_id.clone(),
                node: node.clone(),
                item: None,
            });
        }
        let visit = mine()
            .filter_map(|r| r.item.as_ref().map(|i| i.visit.started_seq))
            .max();
        let mut items: BTreeMap<&str, (u64, &str)> = BTreeMap::new();
        for r in mine() {
            let Some(i) = &r.item else { continue };
            if Some(i.visit.started_seq) != visit {
                continue;
            }
            let latest = items.entry(&i.id).or_insert((0, ""));
            if r.settlement_seq >= latest.0 {
                *latest = (r.settlement_seq, &r.snapshot_id);
            }
        }
        bound.extend(items.into_iter().map(|(id, (_, sid))| BoundSnapshot {
            snapshot_id: sid.to_string(),
            node: node.clone(),
            item: Some(id.to_string()),
        }));
    }
    Ok(bound)
}

/// One record of a built view: what a session file keeps of it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecordRef {
    #[serde(rename = "ref")]
    pub r#ref: String,
    /// `sha256:<hex>` of the record's stored text.
    pub digest: String,
    pub citable: bool,
}

/// One view of a session as its file keeps it: no text.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ViewRecord {
    pub id: String,
    pub kind: String,
    pub complete: bool,
    /// For a package view: the collector's own statement.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub collector_complete: Option<bool>,
    /// The node of the snapshot the view comes from.
    pub node: String,
    pub records: Vec<RecordRef>,
}

/// The views of a session, with their texts (served) and what its file
/// keeps of them.
pub struct Built {
    pub views: ViewSet<String>,
    pub records: Vec<ViewRecord>,
}

impl Built {
    /// The node whose snapshot holds the record `r`.
    pub fn node_of(&self, r: &str) -> Option<&str> {
        self.records
            .iter()
            .find(|v| v.records.iter().any(|x| x.r#ref == r))
            .map(|v| v.node.as_str())
    }
}

/// A view under construction.
struct Draft {
    view: ViewRecord,
    texts: Vec<String>,
}

impl Draft {
    fn new(id: String, kind: &str, complete: bool, node: &str) -> Self {
        Draft {
            view: ViewRecord {
                id,
                kind: kind.to_string(),
                complete,
                collector_complete: None,
                node: node.to_string(),
                records: Vec::new(),
            },
            texts: Vec::new(),
        }
    }

    /// Adds the record `r` with its stored text, served through `redactor`.
    fn push(&mut self, r: String, stored: &str, redactor: &RunRedactor, uncitable: bool) {
        let served = redactor.redact_str(stored);
        self.view.records.push(RecordRef {
            r#ref: r,
            digest: apb_core::content::sha256_hex(stored.as_bytes()),
            citable: !uncitable && served == stored,
        });
        self.texts.push(served);
    }
}

/// How many `format: records` results each node of the run's playbook
/// declares. Read from the run's own `playbook.yaml`; a playbook that no
/// longer reads names nothing, which only ever loosens an absence.
fn declared_record_files(run_dir: &Path) -> BTreeMap<String, usize> {
    let Ok(bytes) = std::fs::read(run_dir.join("playbook.yaml")) else {
        return BTreeMap::new();
    };
    apb_core::schema::Playbook::from_yaml(&String::from_utf8_lossy(&bytes))
        .map(|pb| {
            let mut out: BTreeMap<String, usize> = BTreeMap::new();
            for n in &pb.nodes {
                let count = n
                    .results
                    .iter()
                    .flatten()
                    .filter(|r| r.format == ResultFormat::Records)
                    .count();
                if count > 0 {
                    out.insert(n.id.clone(), count);
                }
            }
            out
        })
        .unwrap_or_default()
}

/// Builds the views of `bound`, served through `redactor`, keeping only the
/// kinds `kinds` names when it names any. A snapshot that no longer reads
/// fails the whole build.
pub fn build(
    run_dir: &Path,
    bound: &[BoundSnapshot],
    kinds: Option<&[String]>,
    redactor: &RunRedactor,
) -> Result<Built, ResultReadError> {
    if redactor.withholds() {
        return Err(ResultReadError::Withheld);
    }
    let wanted = |k: &str| kinds.is_none_or(|ks| ks.iter().any(|x| x == k));
    let declared = declared_record_files(run_dir);
    let mut drafts: Vec<Draft> = Vec::new();
    for b in bound {
        let (m, entries) = stored_entries(run_dir, &b.snapshot_id, redactor)?;
        let whole = m.incomplete.is_none() && m.omitted.is_empty() && m.output_omitted.is_none();
        let prefix = match &b.item {
            Some(i) => format!("{}#{i}", b.node),
            None => b.node.clone(),
        };
        let mut by_kind: BTreeSet<String> = BTreeSet::new();
        // Records packages actually captured in this snapshot: the node's
        // playbook may declare more `format: records` files than the
        // snapshot holds (a glob that matched nothing, a declaration whose
        // file was never written), and the kinds only a missing package
        // could carry must not be provable from the captured ones alone.
        let mut packages_seen: usize = 0;
        for e in &entries {
            let entry = e.id.to_string();
            let base = Citation {
                snapshot_id: b.snapshot_id.clone(),
                entry: e.id.clone(),
                record: None,
                range: None,
            };
            let package = (e.entry.format == ResultFormat::Records)
                .then(|| records::parse(&e.text).ok())
                .flatten();
            let kind = match (e.entry.part, &package) {
                (EntryPart::Output, _) => OUTPUT,
                (EntryPart::Fields, _) => FIELDS,
                (EntryPart::File, None) => FILE,
                (EntryPart::File, Some(_)) => RECORDS,
            };
            let Some(p) = package else {
                if wanted(kind) {
                    let mut d = Draft::new(format!("{prefix}/{entry}"), kind, whole, &b.node);
                    d.push(base.to_string(), &e.text, redactor, e.path_redacted);
                    drafts.push(d);
                }
                continue;
            };
            let complete = whole && p.header.complete;
            let record_ref = |id: &str| {
                Citation {
                    record: Some(id.to_string()),
                    ..base.clone()
                }
                .to_string()
            };
            match kinds {
                None => {
                    let mut d = Draft::new(format!("{prefix}/{entry}"), RECORDS, complete, &b.node);
                    d.view.collector_complete = Some(p.header.complete);
                    for r in &p.records {
                        d.push(record_ref(&r.id), &r.text, redactor, e.path_redacted);
                    }
                    drafts.push(d);
                    packages_seen += 1;
                }
                Some(ks) => {
                    for k in ks {
                        if matches!(k.as_str(), OUTPUT | FIELDS | FILE) {
                            continue;
                        }
                        let mut d =
                            Draft::new(format!("{prefix}/{entry}/{k}"), k, complete, &b.node);
                        d.view.collector_complete = Some(p.header.complete);
                        let of_kind = p.records.iter().filter(|r| k == RECORDS || &r.kind == k);
                        for r in of_kind {
                            d.push(record_ref(&r.id), &r.text, redactor, e.path_redacted);
                        }
                        by_kind.insert(k.clone());
                        drafts.push(d);
                    }
                    packages_seen += 1;
                }
            }
        }
        // A snapshot that lost entries may have lost a package: every named
        // record kind it shows no view of gets an empty incomplete one, so
        // an absence over that kind is not taken for proof.
        if let (false, Some(ks)) = (whole, kinds) {
            for k in ks {
                if !matches!(k.as_str(), OUTPUT | FIELDS | FILE) && !by_kind.contains(k) {
                    drafts.push(Draft::new(
                        format!("{prefix}/omitted/{k}"),
                        k,
                        false,
                        &b.node,
                    ));
                }
            }
        }
        // Every declared `format: records` file the snapshot holds no
        // package for (a glob that matched nothing, a file never written, a
        // settlement that failed): an empty incomplete view per kind, so an
        // absence over its kinds is not proven from the captured packages
        // alone.
        let missing = declared
            .get(&b.node)
            .copied()
            .unwrap_or(0)
            .saturating_sub(packages_seen);
        let ks: Vec<String> = match kinds {
            Some(ks) => ks
                .iter()
                .filter(|k| !matches!(k.as_str(), OUTPUT | FIELDS | FILE))
                .cloned()
                .collect(),
            None => vec![RECORDS.to_string()],
        };
        for i in 0..missing {
            for k in &ks {
                drafts.push(Draft::new(
                    format!("{prefix}/notcaptured/{i}/{k}"),
                    k,
                    false,
                    &b.node,
                ));
            }
        }
    }
    let uncitable: BTreeSet<String> = drafts
        .iter()
        .flat_map(|d| d.view.records.iter())
        .filter(|r| !r.citable)
        .map(|r| r.r#ref.clone())
        .collect();
    let views: Vec<SourceView<String>> = drafts
        .iter()
        .map(|d| SourceView {
            id: d.view.id.clone(),
            kind: d.view.kind.clone(),
            complete: d.view.complete,
            records: d
                .view
                .records
                .iter()
                .zip(&d.texts)
                .map(|(r, t)| SourceRecord {
                    r#ref: r.r#ref.clone(),
                    text: t.clone(),
                })
                .collect(),
        })
        .collect();
    Ok(Built {
        views: ViewSet::new(views)
            .with_uncitable(uncitable)
            .with_scope("this session"),
        records: drafts.into_iter().map(|d| d.view).collect(),
    })
}
