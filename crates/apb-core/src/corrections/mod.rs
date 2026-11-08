//! The corrections ledger: what people and runs corrected in earlier runs
//! of a playbook, kept per playbook in a signed file under `.apb/corrections/`
//! so a later run can be told what its predecessors got wrong.
//!
//! - [`normalize`]: the grouping key of a correction (a normalized token
//!   key or an engine fingerprint);
//! - [`store`]: the signed per-playbook file
//!   (`.apb/corrections/<scope>/<id>.json`);
//! - this module: the stored types and the fold of one run into the file.
//!
//! The ledger holds people's words and engine fingerprints and is shown
//! only to people: nothing here is read into a prompt, and every entry
//! point that reaches it is refused from a run step (the surfaces own
//! that check).
//!
//! The fold ([`CorrectionFile::observe`]) is idempotent per run: a run
//! already counted changes nothing, and a run older than the observed
//! window's floor ([`CorrectionFile::observed_floor_ms`]) can never count
//! again although the window is capped. A person decides an entry
//! ([`CorrectionFile::dismiss`], [`CorrectionFile::reopen`],
//! [`CorrectionFile::mark_fixed`]); apb never applies a draft itself, it
//! only offers one ([`draft`]) for a person to edit and apply through the
//! usual ways of changing a playbook.

pub mod normalize;
pub mod store;

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::known_problems::Decided;
use crate::schema::Playbook;

/// A correction's id: `c-` and the first 12 hex of its key's digest.
pub fn entry_id(key: &str) -> String {
    let digest = Sha256::digest(key.as_bytes());
    format!("c-{}", &crate::content::hex_lower(&digest)[..12])
}

/// A proposed entry: `open` becomes `proposed` at this many runs (fixed,
/// not configurable).
pub const MIN_RUNS: u32 = 2;
/// Runs on the fix version before it is judged (see [`CorrectionStatus::Fixed`]).
pub const REOPEN_AFTER_RUNS: u32 = 3;
/// The longest stored correction text.
pub const TEXT_MAX: usize = 300;
/// The longest draft prompt line.
pub const LINE_MAX: usize = 400;
/// Evidence references kept per correction (the latest).
pub const EVIDENCE_KEPT: usize = 10;
/// Runs remembered as observed, so observing a run twice counts once. The
/// floor ([`CorrectionFile::observed_floor_ms`]) makes the dedup exact
/// although the list is capped.
pub const OBSERVED_KEPT: usize = 1000;

/// What kind of correction a sighting carries.
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CorrectionKind {
    /// A person's non-approve decision with a note at a review gate.
    ReviewNote,
    /// A note a supervisor channel appended on a node (an agent may have
    /// written it: never offered as a draft).
    NodeNote,
    /// A person's answer to a node's question.
    HumanAnswer,
    /// A success check rejected an attempt's result.
    Rejection,
}

impl CorrectionKind {
    /// The serde name of the kind, as the key and every surface spell it.
    pub fn as_word(self) -> &'static str {
        match self {
            CorrectionKind::ReviewNote => "review_note",
            CorrectionKind::NodeNote => "node_note",
            CorrectionKind::HumanAnswer => "human_answer",
            CorrectionKind::Rejection => "rejection",
        }
    }
}

/// Where an entry stands in its life. `open` counts runs until it is
/// `proposed`; a person marks the version that carries the fix (`fixed`)
/// and the fold reopens the entry when the correction keeps meeting its
/// old rate on that version.
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CorrectionStatus {
    /// Seen once so far.
    #[default]
    Open,
    /// Seen in [`MIN_RUNS`] runs: shown to people, offering a draft.
    Proposed,
    /// A person marked a version as carrying the fix.
    Fixed,
    /// Back from `fixed`: the correction kept meeting its old rate on the
    /// fix version (see [`REOPEN_AFTER_RUNS`]).
    Reopened,
    /// A person dismissed it.
    Dismissed,
}

/// One correction of one run, built by the engine's extractor.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Sighting {
    pub kind: CorrectionKind,
    /// The node a draft fix would change, when one is known.
    pub node: Option<String>,
    /// The review gate, for a review note.
    pub gate: Option<String>,
    /// `f:<fingerprint>` or `t:<16 hex>` (see [`normalize`]).
    pub key: String,
    /// The stored text; `None` only for an `f:` key (the text held a
    /// secret or a credential shape, so nothing derived from it is kept).
    pub text: Option<String>,
    pub run_id: String,
    pub seq: u64,
    pub at_ms: u64,
    pub version: String,
    /// The supervisor channel does not tell an agent from a person.
    pub via_supervisor: bool,
}

/// Where a correction was met: the run and its journal record. The TS
/// export is renamed: the run outcome's `EvidenceRef` (a different shape)
/// already holds that name in the generated contract.
#[cfg_attr(
    feature = "ts",
    derive(ts_rs::TS),
    ts(rename = "CorrectionEvidenceRef")
)]
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct EvidenceRef {
    pub run_id: String,
    pub seq: u64,
}

/// One correction of one playbook, folded from every sighting of its key.
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Correction {
    /// `c-` and the first 12 hex of the key's digest (see
    /// [`entry_id`]).
    pub id: String,
    pub kind: CorrectionKind,
    /// The node a draft fix would change, of the sighting that created the
    /// entry.
    pub node: Option<String>,
    /// The review gate, for a review note.
    pub gate: Option<String>,
    pub key: String,
    /// The leading wording of [`Correction::texts`], when any sighting
    /// carried text.
    pub text: Option<String>,
    /// The distinct wordings runs reported, at most
    /// [`crate::known_problems::TEXTS_KEPT`].
    pub texts: Vec<crate::known_problems::ReportedText>,
    /// Distinct runs that reported it.
    pub runs: u32,
    /// Runs of this entry per version: the numerator of the reopen rule.
    pub runs_by_version: BTreeMap<String, u32>,
    pub first_seen_ms: u64,
    pub last_seen_ms: u64,
    /// The latest [`EVIDENCE_KEPT`] sightings.
    pub evidence: Vec<EvidenceRef>,
    /// Any sighting came through the supervisor channel.
    pub via_supervisor: bool,
    pub status: CorrectionStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub fix: Option<FixRecord>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub decided: Option<Decided>,
    /// The version of the entry's latest sighting: the base a fix is
    /// judged against (see [`FixRecord`]).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub last_version: String,
}

/// A person's statement that a version carries the fix, with the base
/// counts taken at that moment, so the reopen rule never needs history the
/// file no longer holds.
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FixRecord {
    /// The version the person names as the fix.
    pub version: String,
    /// The version of the entry's latest sighting then.
    pub base_version: String,
    /// `runs_by_version[base_version]` of the file then.
    pub base_runs: u32,
    /// The entry's `runs_by_version[base_version]` then.
    pub base_hits: u32,
    pub marked: Decided,
}

/// A run the fold counted.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct ObservedRun {
    pub run_id: String,
    pub version: String,
    pub started_ms: u64,
}

/// One playbook's corrections file (see [`store`]).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CorrectionFile {
    pub schema: u32,
    pub scope: String,
    pub playbook: String,
    /// The latest [`OBSERVED_KEPT`] runs whose sightings are counted, by
    /// `started_ms`.
    #[serde(default)]
    pub observed: Vec<ObservedRun>,
    /// The `started_ms` of the newest run dropped from `observed`: a run
    /// that started at or before it is never counted again.
    #[serde(default)]
    pub observed_floor_ms: u64,
    /// Every counted run per version: the denominator of the reopen rule.
    #[serde(default)]
    pub runs_by_version: BTreeMap<String, u32>,
    #[serde(default)]
    pub entries: Vec<Correction>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mac: Option<String>,
}

impl CorrectionFile {
    /// The entry `entry_id`, when the file holds it.
    pub fn get(&self, entry_id: &str) -> Option<&Correction> {
        self.entries.iter().find(|e| e.id == entry_id)
    }

    fn get_mut(&mut self, entry_id: &str) -> Result<&mut Correction, String> {
        self.entries
            .iter_mut()
            .find(|e| e.id == entry_id)
            .ok_or_else(|| format!("no correction `{entry_id}` for this playbook"))
    }

    /// Folds one run and its sightings into the file: idempotent per run
    /// (a run already counted, or one that started at or before
    /// `observed_floor_ms`, changes nothing and returns `false`). The run
    /// counts toward `runs_by_version` even with no sighting: that is the
    /// denominator of the reopen rule. Sightings are grouped by key; each
    /// key counts once per run. `at_ms` is the run's end, as
    /// `first_seen_ms` of a new entry.
    pub fn observe(&mut self, run: ObservedRun, sightings: Vec<Sighting>, at_ms: u64) -> bool {
        if self.observed.iter().any(|r| r.run_id == run.run_id)
            || run.started_ms <= self.observed_floor_ms
        {
            return false;
        }
        let pos = self
            .observed
            .partition_point(|r| r.started_ms <= run.started_ms);
        self.observed.insert(pos, run.clone());
        if self.observed.len() > OBSERVED_KEPT {
            let dropped = self.observed.remove(0);
            self.observed_floor_ms = dropped.started_ms;
        }
        *self.runs_by_version.entry(run.version.clone()).or_default() += 1;

        // The run's sightings grouped by key, in first-seen order: a key
        // counts once per run, however many records of the run saw it.
        let mut keys: Vec<&str> = Vec::new();
        let mut groups: BTreeMap<&str, Vec<&Sighting>> = BTreeMap::new();
        for s in &sightings {
            if !groups.contains_key(s.key.as_str()) {
                keys.push(s.key.as_str());
            }
            groups.entry(s.key.as_str()).or_default().push(s);
        }
        for key in keys {
            let group = &groups[key];
            let first = group[0];
            let seq = group.iter().map(|s| s.seq).max().unwrap_or(0);
            let seen_ms = group.iter().map(|s| s.at_ms).max().unwrap_or(at_ms);
            let entry = match self.entries.iter_mut().find(|e| e.key == key) {
                Some(e) => e,
                None => {
                    self.entries.push(Correction {
                        id: entry_id(key),
                        kind: first.kind,
                        node: first.node.clone(),
                        gate: first.gate.clone(),
                        key: key.to_string(),
                        text: None,
                        texts: Vec::new(),
                        runs: 0,
                        runs_by_version: BTreeMap::new(),
                        first_seen_ms: at_ms,
                        last_seen_ms: seen_ms,
                        evidence: Vec::new(),
                        via_supervisor: false,
                        status: CorrectionStatus::Open,
                        fix: None,
                        decided: None,
                        last_version: String::new(),
                    });
                    self.entries.last_mut().expect("just pushed")
                }
            };
            entry.runs = entry.runs.saturating_add(1);
            *entry
                .runs_by_version
                .entry(first.version.clone())
                .or_default() += 1;
            entry.last_seen_ms = entry.last_seen_ms.max(seen_ms);
            entry.last_version = first.version.clone();
            entry.evidence.push(EvidenceRef {
                run_id: run.run_id.clone(),
                seq,
            });
            let over = entry.evidence.len().saturating_sub(EVIDENCE_KEPT);
            entry.evidence.drain(..over);
            entry.via_supervisor |= group.iter().any(|s| s.via_supervisor);
            // A sighting without text never sets or changes the leading
            // wording.
            let mut worded: Vec<(&str, u64)> = Vec::new();
            for s in group {
                if let Some(t) = s.text.as_deref()
                    && !worded.iter().any(|(w, _)| *w == t)
                {
                    worded.push((t, s.at_ms));
                }
            }
            for (text, at) in worded {
                crate::known_problems::keep_wording(&mut entry.texts, text, at);
            }
            if let Some(lead) = crate::known_problems::leading(&entry.texts) {
                entry.text = Some(lead.text.clone());
            }
        }
        self.settle_status();
        self.order();
        true
    }

    /// Status after the fold, per entry: `dismissed` stays (a person
    /// decided); `open` becomes `proposed` at [`MIN_RUNS`]; a `fixed`
    /// entry is `reopened` once [`REOPEN_AFTER_RUNS`] runs lie on the fix
    /// version and the correction still meets its base rate there (integer
    /// cross-multiplication, no float).
    fn settle_status(&mut self) {
        let file_runs = self.runs_by_version.clone();
        for e in &mut self.entries {
            match e.status {
                CorrectionStatus::Dismissed => {}
                CorrectionStatus::Open if e.runs >= MIN_RUNS => {
                    e.status = CorrectionStatus::Proposed;
                }
                CorrectionStatus::Fixed => {
                    let Some(fix) = &e.fix else { continue };
                    let n_fix = file_runs.get(&fix.version).copied().unwrap_or(0);
                    let h_fix = e.runs_by_version.get(&fix.version).copied().unwrap_or(0);
                    // The fix holds when the per-run rate fell: hits per
                    // fix run below hits per base run.
                    if n_fix >= REOPEN_AFTER_RUNS
                        && u64::from(h_fix) * u64::from(fix.base_runs)
                            >= u64::from(fix.base_hits) * u64::from(n_fix)
                    {
                        e.status = CorrectionStatus::Reopened;
                    }
                }
                _ => {}
            }
        }
    }

    /// Orders the entries by runs (most first), then last sighting, then id.
    fn order(&mut self) {
        self.entries
            .sort_by(|a, b| (b.runs, b.last_seen_ms, &a.id).cmp(&(a.runs, a.last_seen_ms, &b.id)));
    }

    /// A person dismisses entry `entry_id` (any status but `dismissed`):
    /// hidden from every surface, never reopened by the fold. The reason is
    /// cleaned like every person's reason ([`clean_reason`]).
    pub fn dismiss(
        &mut self,
        entry_id: &str,
        by: &str,
        at_ms: u64,
        reason: Option<&str>,
    ) -> Result<(), String> {
        let e = self.get_mut(entry_id)?;
        if e.status == CorrectionStatus::Dismissed {
            return Err(format!("correction `{entry_id}` is already dismissed"));
        }
        e.status = CorrectionStatus::Dismissed;
        e.decided = Some(Decided {
            by: by.to_string(),
            at_ms,
            reason: clean_reason(reason),
        });
        Ok(())
    }

    /// A person reopens a `dismissed` or `fixed` entry: back to `proposed`
    /// at [`MIN_RUNS`] runs, else `open`, the fix record dropped, the new
    /// decision recorded.
    pub fn reopen(
        &mut self,
        entry_id: &str,
        by: &str,
        at_ms: u64,
        reason: Option<&str>,
    ) -> Result<(), String> {
        let e = self.get_mut(entry_id)?;
        if !matches!(
            e.status,
            CorrectionStatus::Dismissed | CorrectionStatus::Fixed
        ) {
            return Err(format!("correction `{entry_id}` is not dismissed or fixed"));
        }
        e.status = if e.runs >= MIN_RUNS {
            CorrectionStatus::Proposed
        } else {
            CorrectionStatus::Open
        };
        e.fix = None;
        e.decided = Some(Decided {
            by: by.to_string(),
            at_ms,
            reason: clean_reason(reason),
        });
        Ok(())
    }

    /// A person marks `version` as carrying the fix of entry `entry_id`.
    /// The entry must be `proposed` or `reopened` (`not_proposed`); whether
    /// `version` exists in the playbook's store is the surface's check
    /// (`unknown_version` there). The base counts of the entry's latest
    /// sighting's version are snapshotted, so the reopen rule never needs
    /// history the file no longer holds.
    pub fn mark_fixed(
        &mut self,
        entry_id: &str,
        version: &str,
        by: &str,
        at_ms: u64,
        reason: Option<&str>,
    ) -> Result<(), String> {
        let base_version = self
            .get(entry_id)
            .ok_or_else(|| format!("no correction `{entry_id}` for this playbook"))?
            .last_version
            .clone();
        let base_runs = self
            .runs_by_version
            .get(&base_version)
            .copied()
            .unwrap_or(0);
        let e = self.get_mut(entry_id)?;
        if !matches!(
            e.status,
            CorrectionStatus::Proposed | CorrectionStatus::Reopened
        ) {
            return Err("not_proposed".to_string());
        }
        let base_hits = e.runs_by_version.get(&base_version).copied().unwrap_or(0);
        e.fix = Some(FixRecord {
            version: version.to_string(),
            base_version,
            base_runs,
            base_hits,
            marked: Decided {
                by: by.to_string(),
                at_ms,
                reason: clean_reason(reason),
            },
        });
        e.status = CorrectionStatus::Fixed;
        Ok(())
    }
}

/// The longest person's reason kept, as `apb_mcp::tools::problems` caps one.
const REASON_MAX: usize = 300;

/// A person's reason as the store keeps it: one line without hidden
/// characters, at most [`REASON_MAX`] characters. The same cleaning
/// `apb_mcp::tools::problems::clean_reason` applies to a known-problem
/// reason.
fn clean_reason(reason: Option<&str>) -> String {
    let line = crate::text::replace_hidden(reason.unwrap_or_default(), ' ');
    line.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .chars()
        .take(REASON_MAX)
        .collect()
}

/// A draft a person edits and applies themselves; apb never applies one.
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Draft {
    /// The node whose prompt the line belongs in.
    pub node: String,
    /// The one-line instruction for that prompt.
    pub line: String,
}

/// The draft a `proposed` or `reopened` correction offers for the node it
/// names: none without a node, a node with no prompt (an `agent_task` or a
/// `finish` with a prompt), text, or for a `node_note` (the supervisor
/// channel does not tell an agent from a person).
pub fn draft(entry: &Correction, playbook: &Playbook) -> Option<Draft> {
    let node_id = entry.node.as_deref()?;
    if entry.kind == CorrectionKind::NodeNote {
        return None;
    }
    let text = entry.text.as_deref()?;
    let node = playbook.node(node_id)?;
    let has_prompt = matches!(
        &node.kind,
        crate::schema::NodeKind::AgentTask { .. }
            | crate::schema::NodeKind::Finish {
                prompt: Some(_),
                ..
            }
    );
    if !has_prompt {
        return None;
    }
    let line = match entry.kind {
        CorrectionKind::ReviewNote => {
            format!("A person corrected this step in earlier runs: {text}")
        }
        CorrectionKind::HumanAnswer => {
            format!("When this step asks a question, people answered in earlier runs: {text}")
        }
        CorrectionKind::Rejection => format!(
            "Earlier attempts of this step were rejected by apb: {text}. \
             Make sure the result passes that check before you report success."
        ),
        CorrectionKind::NodeNote => return None,
    };
    // One line: hidden characters out, whitespace collapsed, the line (not
    // the text) capped at [`LINE_MAX`].
    let line = crate::text::replace_hidden(&line, ' ');
    let line = line.split_whitespace().collect::<Vec<_>>().join(" ");
    Some(Draft {
        node: node_id.to_string(),
        line: line.chars().take(LINE_MAX).collect(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const KEY: &str = "t:0123456789abcdef";

    fn sighting(key: &str, text: Option<&str>, run_id: &str, seq: u64, at_ms: u64) -> Sighting {
        Sighting {
            kind: CorrectionKind::ReviewNote,
            node: Some("work".into()),
            gate: Some("review".into()),
            key: key.into(),
            text: text.map(str::to_string),
            run_id: run_id.into(),
            seq,
            at_ms,
            version: "1.0.0".into(),
            via_supervisor: false,
        }
    }

    fn run(id: &str, version: &str, started_ms: u64) -> ObservedRun {
        ObservedRun {
            run_id: id.into(),
            version: version.into(),
            started_ms,
        }
    }

    /// A sighting of run `run_id` on `version`: what the engine builds, a
    /// sighting always carries its own run's version.
    fn sighting_on(
        version: &str,
        key: &str,
        text: Option<&str>,
        run_id: &str,
        seq: u64,
        at_ms: u64,
    ) -> Sighting {
        Sighting {
            version: version.into(),
            ..sighting(key, text, run_id, seq, at_ms)
        }
    }

    fn file() -> CorrectionFile {
        CorrectionFile {
            schema: 1,
            scope: "project".into(),
            playbook: "p".into(),
            observed: Vec::new(),
            observed_floor_ms: 0,
            runs_by_version: BTreeMap::new(),
            entries: Vec::new(),
            mac: None,
        }
    }

    #[test]
    fn observing_a_run_twice_changes_nothing() {
        let mut f = file();
        assert!(f.observe(
            run("r1", "1.0.0", 900),
            vec![sighting(KEY, Some("word one two"), "r1", 5, 1000)],
            1000,
        ));
        let before = f.clone();
        assert!(!f.observe(
            run("r1", "1.0.0", 900),
            vec![sighting(KEY, Some("word one two"), "r1", 5, 1000)],
            1000,
        ));
        assert_eq!(f, before);
    }

    #[test]
    fn a_run_below_the_floor_changes_nothing() {
        let mut f = file();
        let first = sighting(KEY, Some("word one two"), "r1", 1, 100);
        assert!(f.observe(run("r1", "1.0.0", 1000), vec![first.clone()], 100));
        for i in 2..=1002u64 {
            assert!(f.observe(run(&format!("r{i}"), "1.0.0", i * 1000), vec![], 100));
        }
        assert_eq!(f.observed.len(), OBSERVED_KEPT);
        assert_eq!(f.observed_floor_ms, 2000, "the newest dropped run's start");
        let before = f.clone();
        assert!(!f.observe(run("r1", "1.0.0", 1000), vec![first], 100));
        assert_eq!(f, before);
    }

    #[test]
    fn a_run_without_sightings_counts_in_runs_by_version() {
        let mut f = file();
        assert!(f.observe(run("r1", "1.0.0", 1000), vec![], 100));
        assert_eq!(f.runs_by_version.get("1.0.0"), Some(&1));
        assert_eq!(f.observed.len(), 1);
        assert!(f.entries.is_empty());
    }

    #[test]
    fn one_key_twice_in_one_run_counts_once() {
        let mut f = file();
        assert!(f.observe(
            run("r1", "1.0.0", 900),
            vec![
                sighting(KEY, Some("word one two"), "r1", 5, 1000),
                sighting(KEY, Some("word one two"), "r1", 9, 1100),
            ],
            1100,
        ));
        assert_eq!(f.entries.len(), 1);
        let e = &f.entries[0];
        assert_eq!(e.runs, 1);
        assert_eq!(e.runs_by_version.get("1.0.0"), Some(&1));
        assert_eq!(e.texts.len(), 1);
        assert_eq!(e.texts[0].runs, 1);
        assert_eq!(e.evidence.len(), 1, "one ref per run, the latest record");
        assert_eq!(e.evidence[0].seq, 9);
        assert_eq!(e.first_seen_ms, 1100);
        assert_eq!(e.last_seen_ms, 1100);
    }

    #[test]
    fn a_supervisor_sighting_is_marked_and_kept_without_text() {
        let mut f = file();
        let mut s = sighting(KEY, None, "r1", 5, 1000);
        s.via_supervisor = true;
        assert!(f.observe(run("r1", "1.0.0", 900), vec![s], 1000));
        let e = &f.entries[0];
        assert!(e.via_supervisor);
        assert_eq!(e.text, None);
        assert!(e.texts.is_empty(), "a sighting without text stores none");
        // A later sighting with text fills the leading wording in; one
        // without text never changes it.
        assert!(f.observe(
            run("r2", "1.0.0", 1900),
            vec![sighting(KEY, Some("word one two"), "r2", 6, 2000)],
            2000,
        ));
        assert_eq!(f.entries[0].text.as_deref(), Some("word one two"));
        assert!(f.observe(
            run("r3", "1.0.0", 2900),
            vec![sighting(KEY, None, "r3", 7, 3000)],
            3000,
        ));
        assert_eq!(f.entries[0].text.as_deref(), Some("word one two"));
    }

    #[test]
    fn open_becomes_proposed_at_min_runs() {
        let mut f = file();
        f.observe(
            run("r1", "1.0.0", 1000),
            vec![sighting(KEY, Some("alpha beta"), "r1", 1, 100)],
            100,
        );
        assert_eq!(f.entries[0].status, CorrectionStatus::Open);
        f.observe(
            run("r2", "1.0.0", 2000),
            vec![sighting(KEY, Some("alpha beta"), "r2", 2, 200)],
            200,
        );
        assert_eq!(f.entries[0].status, CorrectionStatus::Proposed);
        assert_eq!(f.entries[0].runs, 2);
    }

    #[test]
    fn dismissed_stays_dismissed() {
        let mut f = file();
        f.observe(
            run("r1", "1.0.0", 1000),
            vec![sighting(KEY, Some("alpha beta"), "r1", 1, 100)],
            100,
        );
        let id = f.entries[0].id.clone();
        f.dismiss(&id, "cli", 150, Some("not a problem")).unwrap();
        f.observe(
            run("r2", "1.0.0", 2000),
            vec![sighting(KEY, Some("alpha beta"), "r2", 2, 200)],
            200,
        );
        f.observe(
            run("r3", "1.0.0", 3000),
            vec![sighting(KEY, Some("alpha beta"), "r3", 3, 300)],
            300,
        );
        assert_eq!(f.entries[0].status, CorrectionStatus::Dismissed);
        assert_eq!(f.entries[0].runs, 3, "the sightings are still counted");
    }

    #[test]
    fn wordings_keep_the_leading_one() {
        let mut f = file();
        let key = crate::corrections::normalize::key_of(
            CorrectionKind::ReviewNote,
            None,
            "please add the parser tests",
        );
        let s = |text: &str, run_id: &str, seq: u64, at_ms: u64| {
            sighting(&key, Some(text), run_id, seq, at_ms)
        };
        f.observe(
            run("r1", "1.0.0", 1000),
            vec![s("please add the parser tests", "r1", 1, 100)],
            100,
        );
        f.observe(
            run("r2", "1.0.0", 2000),
            vec![s("add the parser tests, please", "r2", 1, 200)],
            200,
        );
        f.observe(
            run("r3", "1.0.0", 3000),
            vec![s("please add the parser tests", "r3", 1, 300)],
            300,
        );
        assert_eq!(f.entries.len(), 1, "one key, whatever the wording");
        let e = &f.entries[0];
        assert_eq!(e.texts.len(), 2);
        assert_eq!(e.text.as_deref(), Some("please add the parser tests"));
        let lead = e
            .texts
            .iter()
            .find(|t| t.text == "please add the parser tests")
            .unwrap();
        assert_eq!(lead.runs, 2);
    }

    #[test]
    fn evidence_keeps_the_latest_only() {
        let mut f = file();
        for i in 1..=11u64 {
            f.observe(
                run(&format!("r{i}"), "1.0.0", i * 1000),
                vec![sighting(
                    KEY,
                    Some("word one two"),
                    &format!("r{i}"),
                    i,
                    i * 100,
                )],
                i * 100,
            );
        }
        let e = &f.entries[0];
        assert_eq!(e.evidence.len(), EVIDENCE_KEPT);
        assert_eq!(e.evidence[0].run_id, "r2", "the oldest dropped");
        assert_eq!(e.evidence.last().unwrap().run_id, "r11");
        assert_eq!(e.runs, 11);
    }

    #[test]
    fn entries_are_ordered_by_runs_then_last_seen_then_id() {
        let mut f = file();
        let sight = |key: &str, run_id: &str, seq: u64, at_ms: u64| {
            sighting(key, Some("word one two"), run_id, seq, at_ms)
        };
        // Key A: three runs. Key B: two runs, seen last at 200. Key C: two
        // runs, seen last at 300.
        for i in 1..=3u64 {
            f.observe(
                run(&format!("a{i}"), "1.0.0", i * 1000),
                vec![sight("t:aaaaaaaaaaaaaaaa", &format!("a{i}"), i, i * 100)],
                i * 100,
            );
        }
        for i in 1..=2u64 {
            f.observe(
                run(&format!("b{i}"), "1.0.0", 4000 + i),
                vec![sight("t:bbbbbbbbbbbbbbbb", &format!("b{i}"), i, i * 100)],
                i * 100,
            );
        }
        for i in 1..=2u64 {
            f.observe(
                run(&format!("c{i}"), "1.0.0", 5000 + i),
                vec![sight("t:cccccccccccccccc", &format!("c{i}"), i, 300 + i)],
                300 + i,
            );
        }
        let order: Vec<&str> = f.entries.iter().map(|e| e.key.as_str()).collect();
        assert_eq!(
            order,
            [
                "t:aaaaaaaaaaaaaaaa",
                "t:cccccccccccccccc",
                "t:bbbbbbbbbbbbbbbb"
            ]
        );
    }

    #[test]
    fn an_entry_id_names_its_key() {
        let mut f = file();
        f.observe(
            run("r1", "1.0.0", 1000),
            vec![sighting(KEY, Some("word one two"), "r1", 1, 100)],
            100,
        );
        assert_eq!(f.entries[0].id, entry_id(KEY));
        // `c-` and the first 12 hex of the key's digest.
        let id = entry_id("t:0123456789abcdef");
        let hex = id.strip_prefix("c-").expect("a correction id");
        assert_eq!(hex.len(), 12);
        assert!(
            hex.chars()
                .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase())
        );
        assert_eq!(entry_id("t:0123456789abcdef"), id);
        assert_ne!(
            entry_id("t:0123456789abcdef"),
            entry_id("t:fedcba9876543210")
        );
    }

    // ---- a person's decisions and the draft ----

    fn entry(
        kind: CorrectionKind,
        node: Option<&str>,
        key: &str,
        text: Option<&str>,
    ) -> Correction {
        Correction {
            id: entry_id(key),
            kind,
            node: node.map(str::to_string),
            gate: None,
            key: key.to_string(),
            text: text.map(str::to_string),
            texts: Vec::new(),
            runs: 2,
            runs_by_version: BTreeMap::new(),
            first_seen_ms: 0,
            last_seen_ms: 0,
            evidence: Vec::new(),
            via_supervisor: false,
            status: CorrectionStatus::Proposed,
            fix: None,
            decided: None,
            last_version: String::new(),
        }
    }

    fn playbook() -> Playbook {
        Playbook::from_yaml(
            "schema: 2\nid: p\nname: p\nversion: 1.0.0\nnodes:\n  \
             - { id: start, type: start }\n  \
             - { id: work, type: agent_task, prompt: do the work }\n  \
             - { id: fin, type: finish, outcome: success, prompt: compose the answer }\n  \
             - { id: plain, type: finish, outcome: success }\n  \
             - { id: sh, type: script, script: echo hi, runner: sh }\n\
             edges: []\n",
        )
        .unwrap()
    }

    /// Two runs on `1.0.0` with the same sighting: a proposed entry whose
    /// base snapshot is 2 of 2.
    fn proposed_file() -> (CorrectionFile, String) {
        let mut f = file();
        for i in 1..=2u64 {
            f.observe(
                run(&format!("r{i}"), "1.0.0", i * 1000),
                vec![sighting(
                    KEY,
                    Some("word one two"),
                    &format!("r{i}"),
                    i,
                    i * 100,
                )],
                i * 100,
            );
        }
        let id = f.entries[0].id.clone();
        (f, id)
    }

    #[test]
    fn the_three_kinds_have_their_line_forms() {
        let pb = playbook();
        let mut e = entry(
            CorrectionKind::ReviewNote,
            Some("work"),
            KEY,
            Some("check the header line"),
        );
        let d = draft(&e, &pb).unwrap();
        assert_eq!(d.node, "work");
        assert_eq!(
            d.line,
            "A person corrected this step in earlier runs: check the header line"
        );
        e.kind = CorrectionKind::HumanAnswer;
        assert_eq!(
            draft(&e, &pb).unwrap().line,
            "When this step asks a question, people answered in earlier runs: check the header line"
        );
        e.kind = CorrectionKind::Rejection;
        assert_eq!(
            draft(&e, &pb).unwrap().line,
            "Earlier attempts of this step were rejected by apb: check the header line. \
             Make sure the result passes that check before you report success."
        );
    }

    #[test]
    fn no_draft_without_a_node_a_prompt_or_text() {
        let pb = playbook();
        assert!(
            draft(
                &entry(CorrectionKind::ReviewNote, None, KEY, Some("text")),
                &pb
            )
            .is_none()
        );
        assert!(
            draft(
                &entry(CorrectionKind::ReviewNote, Some("nope"), KEY, Some("text")),
                &pb
            )
            .is_none(),
            "a node the playbook does not have"
        );
        assert!(
            draft(
                &entry(CorrectionKind::ReviewNote, Some("sh"), KEY, Some("text")),
                &pb
            )
            .is_none(),
            "a script node has no prompt"
        );
        assert!(
            draft(
                &entry(CorrectionKind::ReviewNote, Some("plain"), KEY, Some("text")),
                &pb
            )
            .is_none(),
            "a finish without a prompt"
        );
        assert!(
            draft(
                &entry(CorrectionKind::ReviewNote, Some("work"), KEY, None),
                &pb
            )
            .is_none()
        );
        assert!(
            draft(
                &entry(CorrectionKind::NodeNote, Some("work"), KEY, Some("text")),
                &pb
            )
            .is_none(),
            "a supervisor note never offers a draft"
        );
        let d = draft(
            &entry(CorrectionKind::ReviewNote, Some("fin"), KEY, Some("text")),
            &pb,
        )
        .expect("a finish with a prompt offers one");
        assert_eq!(d.node, "fin");
    }

    #[test]
    fn the_draft_line_is_one_line_within_the_limit_without_hidden_chars() {
        let pb = playbook();
        let mut e = entry(
            CorrectionKind::ReviewNote,
            Some("work"),
            KEY,
            Some("check  the\u{202E} header\nline"),
        );
        let d = draft(&e, &pb).unwrap();
        assert!(!d.line.contains('\n'));
        assert!(!d.line.contains('\u{202E}'));
        assert_eq!(
            d.line,
            "A person corrected this step in earlier runs: check the header line"
        );
        e.text = Some("x".repeat(1000));
        let d = draft(&e, &pb).unwrap();
        assert_eq!(
            d.line.chars().count(),
            LINE_MAX,
            "the line, not the text, is capped"
        );
    }

    #[test]
    fn mark_fixed_snapshots_the_base_counts() {
        let (mut f, id) = proposed_file();
        f.mark_fixed(&id, "1.1.0", "cli", 500, Some("fixed in the new prompt"))
            .unwrap();
        let e = f.get(&id).unwrap();
        assert_eq!(e.status, CorrectionStatus::Fixed);
        let fix = e.fix.as_ref().unwrap();
        assert_eq!(fix.version, "1.1.0");
        assert_eq!(
            fix.base_version, "1.0.0",
            "the entry's latest sighting then"
        );
        assert_eq!(fix.base_runs, 2);
        assert_eq!(fix.base_hits, 2);
        assert_eq!(fix.marked.by, "cli");
        assert_eq!(fix.marked.at_ms, 500);
        assert_eq!(fix.marked.reason, "fixed in the new prompt");
    }

    #[test]
    fn a_fix_that_holds_stays_fixed_and_one_that_does_not_is_reopened() {
        // The base snapshot is 2 of 2. Three runs on the fix version with
        // the correction in one: the per-run rate fell, the fix holds. In
        // all three: the rate did not fall, the entry is reopened.
        for (hits, expected) in [
            (1, CorrectionStatus::Fixed),
            (3, CorrectionStatus::Reopened),
        ] {
            let (mut f, id) = proposed_file();
            f.mark_fixed(&id, "1.1.0", "cli", 500, None).unwrap();
            for i in 1..=3u64 {
                let sight = (i <= hits).then(|| {
                    sighting_on(
                        "1.1.0",
                        KEY,
                        Some("word one two"),
                        &format!("f{i}"),
                        i,
                        1000 + i,
                    )
                });
                f.observe(
                    run(&format!("f{i}"), "1.1.0", 5000 + i),
                    sight.into_iter().collect(),
                    1000 + i,
                );
            }
            assert_eq!(
                f.get(&id).unwrap().status,
                expected,
                "{hits} of 3 fix runs with the correction"
            );
        }
    }

    #[test]
    fn under_three_fix_runs_there_is_no_judgment() {
        let (mut f, id) = proposed_file();
        f.mark_fixed(&id, "1.1.0", "cli", 500, None).unwrap();
        for i in 1..=2u64 {
            f.observe(
                run(&format!("f{i}"), "1.1.0", 5000 + i),
                vec![sighting_on(
                    "1.1.0",
                    KEY,
                    Some("word one two"),
                    &format!("f{i}"),
                    i,
                    1000 + i,
                )],
                1000 + i,
            );
        }
        assert_eq!(f.get(&id).unwrap().status, CorrectionStatus::Fixed);
    }

    #[test]
    fn mark_fixed_refuses_an_entry_that_is_not_proposed() {
        // An open entry (one run).
        let mut open = file();
        open.observe(
            run("r1", "1.0.0", 1000),
            vec![sighting(KEY, Some("word one two"), "r1", 1, 100)],
            100,
        );
        let open_id = open.entries[0].id.clone();
        assert_eq!(
            open.mark_fixed(&open_id, "1.1.0", "cli", 1, None),
            Err("not_proposed".into())
        );
        // A dismissed one, and an unknown id.
        let (mut f, id) = proposed_file();
        f.dismiss(&id, "cli", 10, None).unwrap();
        assert_eq!(
            f.mark_fixed(&id, "1.1.0", "cli", 20, None),
            Err("not_proposed".into())
        );
        assert!(f.mark_fixed("c-nope", "1.1.0", "cli", 20, None).is_err());
    }

    #[test]
    fn a_person_dismisses_and_reopens() {
        let (mut f, id) = proposed_file();
        f.dismiss(&id, "cli", 10, Some("a\nb\u{202E} c")).unwrap();
        let e = f.get(&id).unwrap();
        assert_eq!(e.status, CorrectionStatus::Dismissed);
        let decided = e.decided.as_ref().unwrap();
        assert_eq!(decided.by, "cli");
        assert_eq!(decided.reason, "a b c", "one line, hidden characters gone");
        assert!(
            f.dismiss(&id, "cli", 20, None).is_err(),
            "already dismissed"
        );

        f.reopen(&id, "dashboard", 30, Some("still happens"))
            .unwrap();
        let e = f.get(&id).unwrap();
        assert_eq!(e.status, CorrectionStatus::Proposed, "two runs");
        assert!(e.fix.is_none());
        assert_eq!(e.decided.as_ref().unwrap().reason, "still happens");

        f.mark_fixed(&id, "1.1.0", "cli", 40, None).unwrap();
        f.reopen(&id, "dashboard", 50, None).unwrap();
        let e = f.get(&id).unwrap();
        assert_eq!(e.status, CorrectionStatus::Proposed);
        assert!(e.fix.is_none(), "the fix record is cleared");
        assert!(
            f.reopen(&id, "dashboard", 60, None).is_err(),
            "not dismissed or fixed"
        );

        // A one-run entry reopens to open.
        let mut one = file();
        one.observe(
            run("r1", "1.0.0", 1000),
            vec![sighting(KEY, Some("word one two"), "r1", 1, 100)],
            100,
        );
        let one_id = one.entries[0].id.clone();
        one.dismiss(&one_id, "cli", 10, None).unwrap();
        one.reopen(&one_id, "cli", 20, None).unwrap();
        assert_eq!(one.get(&one_id).unwrap().status, CorrectionStatus::Open);
    }

    #[test]
    fn a_reason_is_one_line_of_at_most_300_characters() {
        let (mut f, id) = proposed_file();
        let long = "x".repeat(400);
        f.dismiss(&id, "cli", 10, Some(&long)).unwrap();
        assert_eq!(
            f.get(&id)
                .unwrap()
                .decided
                .as_ref()
                .unwrap()
                .reason
                .chars()
                .count(),
            300
        );
    }
}
