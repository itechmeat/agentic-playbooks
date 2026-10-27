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
    RecheckRecord, RecheckVerdict, ReportedText, TEXTS_KEPT, leading,
};
use crate::fsutil::{DirLock, atomic_write_private, lock_dir};
use crate::scope::Scope;

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

impl ProblemFile {
    fn new(scope: Scope, playbook: &str) -> Self {
        ProblemFile {
            schema: SCHEMA,
            scope: scope.as_str().to_string(),
            playbook: playbook.to_string(),
            ..ProblemFile::default()
        }
    }

    fn signed_bytes(&self) -> io::Result<Vec<u8>> {
        let value = serde_json::to_value(ProblemFile {
            mac: None,
            ..self.clone()
        })
        .map_err(io::Error::other)?;
        bytes_of(&value)
    }

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
        match texts.iter_mut().find(|t| t.text == text) {
            Some(t) => {
                t.runs = t.runs.saturating_add(1);
                t.last_seen_ms = t.last_seen_ms.max(at_ms);
            }
            None => texts.push(ReportedText {
                text: text.to_string(),
                runs: 1,
                last_seen_ms: at_ms,
            }),
        }
        // Full: the wording seen longest ago goes, never the leading one nor
        // the one just reported (a new wording must be able to build up runs).
        if texts.len() > TEXTS_KEPT {
            let lead = leading(texts).map(|t| t.text.clone());
            if let Some(i) = texts
                .iter()
                .enumerate()
                .filter(|(_, t)| Some(&t.text) != lead.as_ref() && t.text != text)
                .min_by_key(|(_, t)| (t.last_seen_ms, t.runs))
                .map(|(i, _)| i)
            {
                texts.remove(i);
            }
        }
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

fn checked_id(playbook: &str) -> Result<(), ProblemsError> {
    if crate::registry::is_safe_segment(playbook) {
        Ok(())
    } else {
        Err(ProblemsError::InvalidId(playbook.to_string()))
    }
}

/// The directory of `scope`'s files under the execution root `root`.
pub fn dir(root: &Path, scope: Scope) -> PathBuf {
    root.join(".apb").join("problems").join(scope.as_str())
}

fn file(root: &Path, scope: Scope, playbook: &str) -> PathBuf {
    dir(root, scope).join(format!("{playbook}.json"))
}

/// Reads `playbook`'s file without changing anything: an empty signed file
/// when there is none, an unsigned empty one when this installation did not
/// write it (or it does not parse). Only a signed file's problems are ever
/// read: an unsigned file's texts and ids reach no surface.
pub fn read(root: &Path, scope: Scope, playbook: &str) -> Result<Loaded, ProblemsError> {
    checked_id(playbook)?;
    let raw = match std::fs::read(file(root, scope, playbook)) {
        Ok(raw) => raw,
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            return Ok(Loaded {
                file: ProblemFile::new(scope, playbook),
                signed: true,
                unsigned_problems: 0,
            });
        }
        Err(e) => return Err(e.into()),
    };
    let unsigned = |count: usize| {
        Ok(Loaded {
            file: ProblemFile::new(scope, playbook),
            signed: false,
            unsigned_problems: count,
        })
    };
    let Ok(mut value) = serde_json::from_slice::<serde_json::Value>(&raw) else {
        return unsigned(0);
    };
    let count = value
        .get("problems")
        .and_then(|p| p.as_array())
        .map_or(0, Vec::len);
    let mac = value
        .as_object_mut()
        .and_then(|o| o.remove("mac"))
        .and_then(|m| m.as_str().map(str::to_string));
    let signed_here = match (mac, bytes_of(&value)) {
        (Some(mac), Ok(bytes)) => crate::run_origin::verify_tag(MAC_DOMAIN, &bytes, &mac),
        _ => false,
    };
    if !signed_here {
        return unsigned(count);
    }
    let schema = value.get("schema").and_then(|v| v.as_u64()).unwrap_or(0);
    if schema > u64::from(SCHEMA) {
        return Err(ProblemsError::NewerSchema(
            u32::try_from(schema).unwrap_or(u32::MAX),
        ));
    }
    let Ok(f) = serde_json::from_value::<ProblemFile>(value) else {
        return unsigned(count);
    };
    // The MAC covers the scope and id inside the file: a signed file put
    // under another playbook's name does not count for it.
    if f.scope != scope.as_str() || f.playbook != playbook {
        return unsigned(count);
    }
    Ok(Loaded {
        file: f,
        signed: true,
        unsigned_problems: 0,
    })
}

/// The bytes a MAC covers: the JSON value without `mac`, every object's
/// keys sorted explicitly (so the bytes never depend on whether a crate in
/// the build turns on serde_json's `preserve_order`).
fn bytes_of(value: &serde_json::Value) -> io::Result<Vec<u8>> {
    serde_json::to_vec(&canonical(value)).map_err(io::Error::other)
}

/// `value` with every object's keys in sorted order.
fn canonical(value: &serde_json::Value) -> serde_json::Value {
    use serde_json::Value;
    match value {
        Value::Object(map) => {
            let mut keys: Vec<&String> = map.keys().collect();
            keys.sort();
            Value::Object(
                keys.into_iter()
                    .map(|k| (k.clone(), canonical(&map[k])))
                    .collect(),
            )
        }
        Value::Array(items) => Value::Array(items.iter().map(canonical).collect()),
        other => other.clone(),
    }
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
    checked_id(playbook)?;
    let _held: DirLock = lock_dir(&dir(root, scope), &format!("{playbook}.json.lock"))?;
    // An unsigned file reads as a fresh one: it is replaced.
    let mut f = read(root, scope, playbook)?.file;
    let (out, changed) = change(&mut f)?;
    if changed {
        write(root, scope, playbook, f)?;
    }
    Ok(out)
}

fn write(root: &Path, scope: Scope, playbook: &str, f: ProblemFile) -> Result<(), ProblemsError> {
    let mut f = ProblemFile {
        mac: None,
        schema: SCHEMA,
        ..f
    };
    f.mac = Some(crate::run_origin::tag(MAC_DOMAIN, &f.signed_bytes()?)?);
    let bytes = serde_json::to_vec_pretty(&f).map_err(io::Error::other)?;
    Ok(atomic_write_private(&file(root, scope, playbook), &bytes)?)
}
