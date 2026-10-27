//! Everything a grader may read about one run, as views of numbered
//! records with a citation each, on top of the retrospective's read of the
//! run ([`RetroRun`]):
//!
//! - `journal`: every event (`j:<seq>`);
//! - `outputs`: each node's last output (`o:<node>`) and each attempt's
//!   status file (`s:<node>-<attempt>`);
//! - `connector_calls`: the `connector_call` events (`j:<seq>`);
//! - `findings`: the findings and warnings attempts reported (`j:<seq>`);
//! - one view per attempt stream (`t:<stem>/<stream>#<record>`, see
//!   [`crate::transcript`]).
//!
//! A run-level view is complete once the run has ended; a transcript view
//! carries the completeness of its stream.

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};

use apb_core::quality::criteria::ReadKind;

use crate::error::EngineError;
use crate::event::EventPayload;
use crate::run_retro::RetroRun;
use crate::transcript::{self, View};

pub const JOURNAL: &str = "journal";
pub const OUTPUTS: &str = "outputs";
pub const CONNECTOR_CALLS: &str = "connector_calls";
pub const FINDINGS: &str = "findings";

/// One record of a view: its citation and its text before redaction.
#[derive(Debug, Clone, PartialEq)]
pub struct SourceRecord {
    pub r#ref: String,
    pub text: String,
}

/// One readable view.
#[derive(Debug, Clone, PartialEq)]
pub struct SourceView {
    pub id: String,
    pub kind: ReadKind,
    pub complete: bool,
    pub records: Vec<SourceRecord>,
}

impl SourceView {
    pub fn chars(&self) -> u64 {
        self.records
            .iter()
            .map(|r| r.text.chars().count() as u64)
            .sum()
    }
}

/// How a run ended, for a grade made while it is ending (the inline grade
/// runs before `run_finished` is written).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunEnd {
    /// Read the end from the journal.
    Journaled,
    /// The run is ending now with this outcome.
    Ending { succeeded: bool },
}

/// The sources of one run.
#[derive(Debug, Clone)]
pub struct RunSources {
    pub run_dir: PathBuf,
    pub run: RetroRun,
    /// `succeeded`, `failed` or `aborted`; `None` while the run is live.
    pub outcome: Option<String>,
    /// The parsed transcripts, in journal order (the facts rules read).
    pub transcripts: Vec<View>,
    pub views: Vec<SourceView>,
    /// Where each reference is: `(view, record)` indexes, every view that
    /// holds it (a `j:` reference is in the journal and in a filtered
    /// view).
    index: HashMap<String, Vec<(usize, usize)>>,
    /// The record texts went through a redactor already
    /// ([`RunSources::redact`]).
    redacted: bool,
}

impl RunSources {
    /// Reads every source of the run at `run_dir`.
    pub fn load(run_dir: &Path, end: RunEnd) -> Result<Self, EngineError> {
        let run = RetroRun::load(run_dir)?;
        let outcome = match end {
            RunEnd::Ending { succeeded: true } => Some("succeeded".to_string()),
            RunEnd::Ending { succeeded: false } => Some("failed".to_string()),
            RunEnd::Journaled => run.events.iter().rev().find_map(|e| match &e.payload {
                EventPayload::RunFinished { outcome } => Some(outcome.clone()),
                EventPayload::RunAborted { .. } => Some("aborted".to_string()),
                _ => None,
            }),
        };
        let ended = outcome.is_some();
        let transcripts: Vec<View> = transcript::sources(run_dir, &run.events)
            .iter()
            .map(transcript::read)
            .collect();
        let mut views = run_views(run_dir, &run, ended);
        views.extend(transcripts.iter().map(|t| {
            SourceView {
                id: t.id(),
                kind: ReadKind::Transcripts,
                complete: t.complete,
                records: t
                    .records
                    .iter()
                    .map(|r| SourceRecord {
                        r#ref: t.record_ref(r.index),
                        text: r.text.clone(),
                    })
                    .collect(),
            }
        }));
        let mut index: HashMap<String, Vec<(usize, usize)>> = HashMap::new();
        for (vi, v) in views.iter().enumerate() {
            for (ri, r) in v.records.iter().enumerate() {
                index.entry(r.r#ref.clone()).or_default().push((vi, ri));
            }
        }
        Ok(RunSources {
            run_dir: run_dir.to_path_buf(),
            run,
            outcome,
            transcripts,
            views,
            index,
            redacted: false,
        })
    }

    /// Redacts every record's text once, so a reader returns it as it is
    /// and a span is counted on the text the reader shows.
    pub(crate) fn redact(&mut self, redactor: &crate::redact::Redactor) {
        if self.redacted {
            return;
        }
        for v in &mut self.views {
            for r in &mut v.records {
                r.text = redactor.redact(&r.text);
            }
        }
        self.redacted = true;
    }

    /// Whether the record texts are redacted already.
    pub fn is_redacted(&self) -> bool {
        self.redacted
    }

    pub fn view(&self, id: &str) -> Option<&SourceView> {
        self.views.iter().find(|v| v.id == id)
    }

    /// Every view that holds the record `r`.
    pub fn views_holding(&self, r: &str) -> impl Iterator<Item = &SourceView> {
        self.index
            .get(r)
            .into_iter()
            .flatten()
            .map(|(v, _)| &self.views[*v])
    }

    /// The views of the kinds a criterion reads.
    pub fn views_of(&self, kinds: &[ReadKind]) -> impl Iterator<Item = &SourceView> {
        self.views.iter().filter(move |v| kinds.contains(&v.kind))
    }

    /// The record a citation names, wherever it is.
    pub fn record(&self, r: &str) -> Option<&SourceRecord> {
        let (v, i) = *self.index.get(r)?.first()?;
        Some(&self.views[v].records[i])
    }

    pub fn run_id(&self) -> &str {
        &self.run.run_id
    }

    /// The playbook id and version the run started.
    pub fn started(&self) -> Option<(String, String)> {
        self.run.events.iter().find_map(|e| match &e.payload {
            EventPayload::RunStarted { playbook, version } => {
                Some((playbook.clone(), version.clone()))
            }
            _ => None,
        })
    }
}

fn journal_ref(seq: u64) -> String {
    format!("j:{seq}")
}

/// The run-level views: the journal, the outputs, the connector calls and
/// the findings.
fn run_views(run_dir: &Path, run: &RetroRun, ended: bool) -> Vec<SourceView> {
    let event_record = |e: &crate::event::Event| SourceRecord {
        r#ref: journal_ref(e.seq),
        text: serde_json::to_string(&e.payload).unwrap_or_default(),
    };
    let journal: Vec<SourceRecord> = run.events.iter().map(event_record).collect();
    let calls: Vec<SourceRecord> = run
        .events
        .iter()
        .filter(|e| matches!(e.payload, EventPayload::ConnectorCall { .. }))
        .map(event_record)
        .collect();
    let findings: Vec<SourceRecord> = run
        .events
        .iter()
        .filter(|e| matches!(e.payload, EventPayload::AttemptNotes { .. }))
        .map(event_record)
        .collect();
    let mut outputs: BTreeMap<String, String> = BTreeMap::new();
    for e in &run.events {
        if let EventPayload::NodeFinished { node, output, .. } = &e.payload {
            outputs.insert(node.clone(), output.clone());
        }
    }
    let mut output_records: Vec<SourceRecord> = outputs
        .into_iter()
        .map(|(node, text)| SourceRecord {
            r#ref: format!("o:{node}"),
            text,
        })
        .collect();
    // A status file that no longer reads is left out, and the view says it
    // is not whole: nothing rests on a record that is not there.
    let mut outputs_whole = true;
    for (node, attempt) in run.verdicts.keys() {
        let stem = format!("{node}-{attempt}");
        match std::fs::read_to_string(run_dir.join("agent-status").join(format!("{stem}.json"))) {
            Ok(text) => output_records.push(SourceRecord {
                r#ref: format!("s:{stem}"),
                text,
            }),
            Err(_) => outputs_whole = false,
        }
    }
    [
        (JOURNAL, ReadKind::Journal, journal, true),
        (OUTPUTS, ReadKind::Outputs, output_records, outputs_whole),
        (CONNECTOR_CALLS, ReadKind::ConnectorCalls, calls, true),
        (FINDINGS, ReadKind::Findings, findings, true),
    ]
    .into_iter()
    .map(|(id, kind, records, whole)| SourceView {
        id: id.to_string(),
        kind,
        complete: ended && whole,
        records,
    })
    .collect()
}
