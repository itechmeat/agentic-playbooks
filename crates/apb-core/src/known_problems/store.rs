//! The known-problem file of one playbook:
//! `<root>/.apb/problems/<scope>/<id>.json`, git-ignored, written
//! atomically (0600) under a lock of its directory.
//!
//! Every write carries a MAC under this installation's key
//! ([`crate::run_origin::tag`]): the file sits in the checkout, and what it
//! holds reaches agent prompts, so a file this installation did not write
//! (one a repository brought) is never shown and never merged into; the
//! next write replaces it. Reading never changes anything.
//!
//! The MAC covers the file's JSON as written, every field but `mac` itself
//! (fields this build does not know included), so the check never depends
//! on how this build would serialise the file again. A file whose MAC does
//! not verify is not this installation's whatever schema it claims, so a
//! planted "newer" file never blocks a write.

use std::collections::HashSet;
use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::{
    Decided, EVIDENCE_KEPT, KnownProblem, OBSERVED_RUNS_KEPT, Observation, ProblemStatus,
    RecheckRecord, RecheckVerdict, leading,
};
use crate::scope::Scope;
use crate::signed_store::{self, SignedDoc, StoreError};

/// The file format this build reads and writes.
pub const SCHEMA: u32 = 1;
/// The purpose the file's MAC is bound to.
const MAC_DOMAIN: &str = "apb-known-problems-v1";

/// Why the file could not be read or written.
#[derive(Debug, thiserror::Error)]
pub enum ProblemsError {
    #[error("playbook id `{0}` is not a safe name")]
    InvalidId(String),
    #[error("no known problem `{0}` for this playbook")]
    NotFound(String),
    #[error("the known-problem file has schema {0}, newer than this apb understands")]
    NewerSchema(u32),
    #[error("{0}")]
    Refused(String),
    #[error(transparent)]
    Io(#[from] io::Error),
}

impl From<StoreError> for ProblemsError {
    fn from(e: StoreError) -> Self {
        match e {
            StoreError::InvalidId(id) => ProblemsError::InvalidId(id),
            StoreError::NewerSchema(s) => ProblemsError::NewerSchema(s),
            StoreError::Io(e) => ProblemsError::Io(e),
            // Only a `Refuse` transaction returns it; this store replaces.
            e @ StoreError::Unverified(_) => ProblemsError::Io(io::Error::other(e.to_string())),
        }
    }
}

/// One playbook's known problems.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProblemFile {
    pub schema: u32,
    pub scope: String,
    pub playbook: String,
    /// The last [`OBSERVED_RUNS_KEPT`] runs whose sightings are counted.
    #[serde(default)]
    pub observed_runs: Vec<String>,
    #[serde(default)]
    pub problems: Vec<KnownProblem>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mac: Option<String>,
}

/// The file as one read found it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Loaded {
    pub file: ProblemFile,
    /// Whether this installation wrote it as it is. An unsigned file is
    /// read as an empty one: its problems are never shown.
    pub signed: bool,
    /// How many problems an unsigned file holds (its texts and ids are not
    /// read).
    pub unsigned_problems: usize,
}

impl SignedDoc for ProblemFile {
    const MAC_DOMAIN: &'static str = MAC_DOMAIN;
    const SCHEMA: u32 = SCHEMA;
    const DIR: &'static str = "problems";
    const ENTRIES_KEY: &'static str = "problems";

    fn fresh(scope: Scope, playbook: &str) -> Self {
        ProblemFile {
            schema: SCHEMA,
            scope: scope.as_str().to_string(),
            playbook: playbook.to_string(),
            ..ProblemFile::default()
        }
    }

    fn identity(&self) -> (&str, &str) {
        (&self.scope, &self.playbook)
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

impl ProblemFile {
    /// The problem `id`.
    pub fn get(&self, id: &str) -> Option<&KnownProblem> {
        self.problems.iter().find(|p| p.id == id)
    }

    fn get_mut(&mut self, id: &str) -> Result<&mut KnownProblem, ProblemsError> {
        self.problems
            .iter_mut()
            .find(|p| p.id == id)
            .ok_or_else(|| ProblemsError::NotFound(id.to_string()))
    }

    /// Adds the sightings of run `run_id`. A run already observed adds
    /// nothing (observing is idempotent), and one run counts once per
    /// problem, and once per wording, however often it saw it. A closed
    /// problem seen again is open again; a dismissed one stays dismissed (a
    /// person decided). Returns whether anything changed.
    pub fn observe(&mut self, run_id: &str, observations: &[Observation]) -> bool {
        if self.observed_runs.iter().any(|r| r == run_id) {
            return false;
        }
        self.observed_runs.push(run_id.to_string());
        let over = self.observed_runs.len().saturating_sub(OBSERVED_RUNS_KEPT);
        self.observed_runs.drain(..over);
        let mut counted: HashSet<String> = HashSet::new();
        let mut worded: HashSet<(String, bool, String)> = HashSet::new();
        for o in observations {
            let id = o.id();
            let first_in_run = counted.insert(id.clone());
            let mut first_wording =
                |workaround: bool, text: &str| worded.insert((id.clone(), workaround, text.into()));
            let new_symptom = first_wording(false, &o.symptom);
            let new_workaround = o
                .workaround
                .as_deref()
                .is_some_and(|w| first_wording(true, w));
            let p = match self.problems.iter_mut().find(|p| p.id == id) {
                Some(p) => {
                    if first_in_run {
                        p.runs = p.runs.saturating_add(1);
                    }
                    p.last_seen_ms = p.last_seen_ms.max(o.at_ms);
                    p.seen_on(o.source.as_ref());
                    if p.status == ProblemStatus::Closed {
                        p.status = ProblemStatus::Open;
                    }
                    p.evidence.push(o.evidence.clone());
                    let over = p.evidence.len().saturating_sub(EVIDENCE_KEPT);
                    p.evidence.drain(..over);
                    p
                }
                None => {
                    self.problems.push(KnownProblem {
                        id,
                        scope: o.scope.clone(),
                        symptom: o.symptom.clone(),
                        workaround: o.workaround.clone(),
                        symptoms: Vec::new(),
                        workarounds: Vec::new(),
                        origin: o.origin.clone(),
                        evidence: vec![o.evidence.clone()],
                        runs: 1,
                        first_seen_ms: o.at_ms,
                        last_seen_ms: o.at_ms,
                        source: o.source.clone(),
                        status: ProblemStatus::Open,
                        confirmed: None,
                        decided: None,
                        check: None,
                        checks: Vec::new(),
                    });
                    self.problems.last_mut().expect("just pushed")
                }
            };
            if new_symptom {
                p.add_wording(false, &o.symptom, o.at_ms);
            }
            if new_workaround && let Some(w) = &o.workaround {
                p.add_wording(true, w, o.at_ms);
            }
        }
        self.order();
        true
    }

    /// Orders the problems by runs (most first), then by last sighting.
    fn order(&mut self) {
        self.problems
            .sort_by(|a, b| (b.runs, b.last_seen_ms, &a.id).cmp(&(a.runs, a.last_seen_ms, &b.id)));
    }

    /// Records a recheck of problem `id`.
    pub fn record_check(&mut self, id: &str, record: RecheckRecord) -> Result<(), ProblemsError> {
        self.get_mut(id)?.record_check(record);
        Ok(())
    }

    /// Applies a person's decision to problem `id`. A close goes through
    /// [`ProblemFile::record_check`] with a person's recheck instead.
    pub fn decide(
        &mut self,
        id: &str,
        decision: PersonDecision,
        who: Decided,
    ) -> Result<(), ProblemsError> {
        let p = self.get_mut(id)?;
        match decision {
            PersonDecision::Confirm => {
                if p.status != ProblemStatus::Open {
                    return Err(ProblemsError::Refused(format!(
                        "problem `{id}` is not open: reopen it first"
                    )));
                }
                p.confirmed = Some(who);
            }
            PersonDecision::Dismiss => {
                p.status = ProblemStatus::Dismissed;
                p.decided = Some(who);
            }
            PersonDecision::Reopen => {
                p.status = ProblemStatus::Open;
                p.decided = Some(who);
                // A proposed close or an undetermined recheck no longer
                // hides it: the person reopened it.
                p.drop_unsettled_check();
            }
        }
        Ok(())
    }
}

impl KnownProblem {
    /// A sighting on `source`: when that text is not the one the latest
    /// recheck judged, the recheck is stale and the sighting's source is the
    /// one a later change is compared with. A recheck that proposed a close
    /// or could not settle is answered by the sighting too: the problem was
    /// met again.
    fn seen_on(&mut self, source: Option<&super::ProblemSource>) {
        if let Some(s) = source {
            if self.check.as_ref().is_some_and(|c| c.digest != s.digest) {
                self.check = None;
            }
            if self.source.as_ref() != Some(s) {
                self.source = Some(s.clone());
            }
        }
        self.drop_unsettled_check();
    }

    /// Forgets a latest recheck that proposed a close or could not settle
    /// (the history in `checks` stays).
    fn drop_unsettled_check(&mut self) {
        if self.check.as_ref().is_some_and(|c| {
            matches!(
                c.verdict,
                RecheckVerdict::ClosedProposed | RecheckVerdict::Undetermined
            )
        }) {
            self.check = None;
        }
    }

    /// Counts one run's `text` as a symptom (or a workaround) wording. The
    /// leading wording is the one shown; when it changes, a person's
    /// confirmation no longer covers what is shown and is dropped.
    fn add_wording(&mut self, workaround: bool, text: &str, at_ms: u64) {
        let (texts, shown) = if workaround {
            (&mut self.workarounds, self.workaround.clone())
        } else {
            (&mut self.symptoms, Some(self.symptom.clone()))
        };
        super::keep_wording(texts, text, at_ms);
        let lead = leading(texts).map(|t| t.text.clone());
        if lead != shown {
            if workaround {
                self.workaround = lead;
            } else if let Some(l) = lead {
                self.symptom = l;
            }
            self.confirmed = None;
        }
    }
}

/// A person's decision other than a close.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PersonDecision {
    Confirm,
    Dismiss,
    Reopen,
}

/// The directory of `scope`'s files under the execution root `root`.
pub fn dir(root: &Path, scope: Scope) -> PathBuf {
    signed_store::dir(root, "problems", scope)
}

/// Reads `playbook`'s file without changing anything: an empty signed file
/// when there is none, an unsigned empty one when this installation did not
/// write it (or it does not parse). Only a signed file's problems are ever
/// read: an unsigned file's texts and ids reach no surface.
pub fn read(root: &Path, scope: Scope, playbook: &str) -> Result<Loaded, ProblemsError> {
    Ok(
        signed_store::read::<ProblemFile>(root, scope, playbook).map(|l| Loaded {
            file: l.file,
            signed: l.signed,
            unsigned_problems: l.unsigned_entries,
        })?,
    )
}

/// Changes `playbook`'s file under its lock: `change` gets the signed file
/// (a fresh one in place of an unsigned file) and the result is written
/// signed. Nothing is written when `change` reports no change.
pub fn update<R>(
    root: &Path,
    scope: Scope,
    playbook: &str,
    change: impl FnOnce(&mut ProblemFile) -> Result<(R, bool), ProblemsError>,
) -> Result<R, ProblemsError> {
    signed_store::update(root, scope, playbook, change)
}

#[cfg(test)]
mod fixture_tests {
    use super::*;

    /// The bytes the pre-extraction code wrote for one problem, kept so the
    /// generic store's MAC coverage is provably unchanged: this file must
    /// still verify after the move.
    const FIXTURE_BYTES: &str = r#"{
  "schema": 1,
  "scope": "project",
  "playbook": "p",
  "observed_runs": [
    "p-1"
  ],
  "problems": [
    {
      "id": "kp-4a4045f986",
      "scope": {
        "kind": "node",
        "name": "work"
      },
      "symptom": "the parser rejects an empty header line",
      "symptoms": [
        {
          "text": "the parser rejects an empty header line",
          "runs": 1,
          "last_seen_ms": 1700000000000
        }
      ],
      "origin": {
        "type": "finding",
        "kind": "tool_problem"
      },
      "evidence": [
        {
          "run_id": "p-1",
          "seq": 9,
          "node": "work"
        }
      ],
      "runs": 1,
      "first_seen_ms": 1700000000000,
      "last_seen_ms": 1700000000000,
      "status": "open"
    }
  ],
  "mac": "a238430ff0e0166bd7bd78eb4653e56e933571180f8e9edc9ea790e3a505a4c3"
}"#;

    /// The installation key the fixture was written under, seeded into the
    /// test's config dir so the MAC verifies whatever machine runs this.
    const FIXTURE_KEY: &str =
        "probe-key-please-replace-0123456789abcdef0123456789abcdef0123456789abcdef";

    #[test]
    fn a_file_the_previous_code_wrote_still_verifies() {
        let _lock = crate::env_test_lock();
        let tmp = tempfile::tempdir().unwrap();
        let cfg = tempfile::tempdir().unwrap();
        std::fs::write(cfg.path().join("run-origin.key"), FIXTURE_KEY).unwrap();
        let prev = std::env::var_os("APB_CONFIG_DIR");
        // SAFETY: under env_test_lock, the one env change at a time.
        unsafe { std::env::set_var("APB_CONFIG_DIR", cfg.path()) };
        let root = tmp.path();
        let scope = Scope::Project;
        let path = dir(root, scope).join("p.json");
        std::fs::create_dir_all(dir(root, scope)).unwrap();
        std::fs::write(&path, FIXTURE_BYTES).unwrap();
        let loaded = read(root, scope, "p");
        // SAFETY: under env_test_lock.
        unsafe {
            match &prev {
                Some(v) => std::env::set_var("APB_CONFIG_DIR", v),
                None => std::env::remove_var("APB_CONFIG_DIR"),
            }
        }
        let loaded = loaded.unwrap();
        assert!(loaded.signed, "the fixture bytes did not verify");
        assert_eq!(loaded.file.playbook, "p");
        assert_eq!(loaded.file.problems.len(), 1);
        assert_eq!(loaded.unsigned_problems, 0);
    }
}
