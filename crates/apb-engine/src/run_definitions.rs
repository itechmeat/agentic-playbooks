//! The definitions a run executed, kept in the run directory so any journal
//! seq can be read against the definition in force at that seq.
//!
//! `runs/<id>/playbook.yaml` is what a run executes (a start writes it, a
//! patch replaces it, a resume reads it), and nothing about that changes
//! here. Beside it, every start, resume and patch stores the exact bytes it
//! is about to run under `runs/<id>/definitions/<sha256>.yaml` (write-once,
//! private, best effort), and journals their digest on its own event:
//! `run_started.definition`, `run_resumed.definition` and
//! `patch_applied.definition`. Those digests are read by surfaces only; no
//! execution path reads them, and an older apb drops them.
//!
//! A stored definition is a run-local snapshot: it is not a playbook
//! version, publishes nothing and never touches a launch floor or `current`.
//!
//! Certainty needs positive evidence in the journal itself
//! ([`DefinitionsInForce::load`]): every drive journals the digest of what
//! it executes, so a segment of the journal is `Known` only when the digest
//! its own boundary journaled matches stored bytes at read time. Everything
//! else is `Unknown`, with the reason. The diagnostics sidecar is never
//! evidence: a `definition_mismatch` record is extra detail for surfaces,
//! and its absence (a sidecar that was lost, full, or never written by an
//! older apb) proves nothing.

use std::path::Path;

use apb_core::schema::Playbook;

use crate::diagnostics::Diagnostic;
use crate::event::{Event, EventPayload};

/// The directory of stored definitions inside a run directory.
pub const DIR: &str = "definitions";

/// The store of the run's definitions.
const STORE: crate::run_store::RunStore = crate::run_store::RunStore {
    dir: DIR,
    ext: "yaml",
};

/// The digest a definition is stored and journaled under: sha256 of its
/// exact bytes, lower-case hex.
pub fn digest(bytes: &[u8]) -> String {
    crate::run_store::digest(bytes)
}

#[cfg(test)]
fn stored_path(run_dir: &Path, digest: &str) -> std::path::PathBuf {
    STORE.path(run_dir, digest)
}

/// Stores `yaml` as one of the run's definitions and returns its digest.
/// Write-once: a definition already stored under the digest is kept. Refuses
/// a `definitions` entry that is not a real directory (a link included) and
/// a stored entry that is not a regular file.
pub fn store(run_dir: &Path, yaml: &[u8]) -> std::io::Result<String> {
    STORE.store(run_dir, yaml)
}

/// [`store`] for an execution path: returns the digest to journal whether
/// or not the bytes could be stored (a definition that is not stored reads
/// as `Unknown`), and never stops the start, the resume or the patch.
pub(crate) fn store_best_effort(run_dir: &Path, yaml: &[u8]) -> String {
    let _ = store(run_dir, yaml);
    digest(yaml)
}

/// The bytes stored under `digest`, when they are there, regular, and hash
/// to `digest`.
pub fn read_stored(run_dir: &Path, digest: &str) -> Option<Vec<u8>> {
    STORE.read(run_dir, digest)
}

/// Where the definition in force changes or is journaled again: the start,
/// every resume and every patch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Boundary {
    /// The seq of the `run_started`, `run_resumed` or `patch_applied` event.
    pub seq: u64,
    /// The digest it journaled, absent when an older apb wrote it.
    pub definition: Option<String>,
    pub kind: BoundaryKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BoundaryKind {
    Start,
    Resume,
    Patch,
}

/// The definition boundaries of a journal, in order.
pub fn boundaries(events: &[Event]) -> Vec<Boundary> {
    events
        .iter()
        .filter_map(|e| {
            let (definition, kind) = match &e.payload {
                EventPayload::RunStarted { definition, .. } => (definition, BoundaryKind::Start),
                EventPayload::RunResumed { definition, .. } => (definition, BoundaryKind::Resume),
                EventPayload::PatchApplied { definition, .. } => (definition, BoundaryKind::Patch),
                _ => return None,
            };
            Some(Boundary {
                seq: e.seq,
                definition: definition.clone(),
                kind,
            })
        })
        .collect()
}

/// The mismatch a resume over `events` would record for the
/// `playbook.yaml` bytes it is about to execute, or `None`: the digest of
/// those bytes differs from the last journaled digest, or the last patch
/// was journaled without one (an older apb). A start or a resume without a
/// digest (an older apb) is skipped: the comparison goes to the boundary
/// before it, and a journal with nothing to compare with records nothing.
pub fn detect_mismatch(
    events: &[Event],
    yaml: &[u8],
) -> Option<crate::diagnostics::mismatch::DefinitionMismatch> {
    let Boundary {
        seq,
        definition,
        kind,
    } = boundaries(events)
        .into_iter()
        .rev()
        .find(|b| b.definition.is_some() || b.kind == BoundaryKind::Patch)?;
    let found = digest(yaml);
    let reason = match (&definition, kind) {
        (Some(d), _) if *d == found => return None,
        (Some(_), _) => "digest_differs",
        (None, _) => "patch_without_definition",
    };
    Some(crate::diagnostics::mismatch::DefinitionMismatch {
        after_seq: events.last().map(|e| e.seq).unwrap_or(0),
        boundary_seq: seq,
        journaled: definition,
        found,
        reason: reason.into(),
    })
}

/// What a resume does with the `playbook.yaml` bytes it is about to execute
/// (`yaml`), before it journals `run_resumed`: records a mismatch against
/// the journal's last digest for the surfaces, stores the bytes, and returns
/// their digest for `run_resumed.definition`. Never restores, never refuses,
/// never fails the resume.
pub(crate) fn on_resume(run_dir: &Path, events: &[Event], yaml: &[u8]) -> String {
    if let Some(found) = detect_mismatch(events, yaml) {
        crate::diagnostics::mismatch::record(run_dir, found);
    }
    store_best_effort(run_dir, yaml)
}

/// Why a segment's definition is not established.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnknownReason {
    /// The boundary journaled no digest (an older apb wrote it), or the
    /// journal has no boundary at all.
    NotJournaled,
    /// The journaled digest has no stored definition, or the stored bytes
    /// do not hash to it.
    NotStored,
    /// The stored definition does not parse.
    Unreadable,
}

impl UnknownReason {
    pub fn as_str(self) -> &'static str {
        match self {
            UnknownReason::NotJournaled => "definition_not_journaled",
            UnknownReason::NotStored => "definition_not_stored",
            UnknownReason::Unreadable => "definition_unreadable",
        }
    }
}

/// The definition of a segment.
#[derive(Debug, Clone)]
pub enum Resolved {
    Known {
        playbook: Box<Playbook>,
        digest: String,
    },
    Unknown(UnknownReason),
}

impl Resolved {
    pub fn is_known(&self) -> bool {
        matches!(self, Resolved::Known { .. })
    }
}

/// A stretch of the journal executed under one definition.
#[derive(Debug, Clone)]
pub struct Segment {
    /// The first seq the segment covers.
    pub from_seq: u64,
    /// The last seq it covers; `None` for the segment in force now.
    pub to_seq: Option<u64>,
    /// The boundary that opened it (`None` for a journal without one).
    pub boundary: Option<Boundary>,
    pub resolved: Resolved,
    /// A resume recorded a `definition_mismatch` against this segment's
    /// boundary: detail for the surfaces, never evidence either way.
    pub mismatch_recorded: bool,
}

/// Every segment of a run's journal with its definition.
#[derive(Debug, Clone, Default)]
pub struct DefinitionsInForce {
    pub segments: Vec<Segment>,
    /// `playbook.yaml` no longer hashes to the digest the segment in force
    /// journaled: something changed it that no event records yet. The
    /// segment still executed what it journaled; the next resume journals
    /// what it reads. Detail for the surfaces.
    pub playbook_changed: bool,
}

impl DefinitionsInForce {
    /// Resolves the segments of `events` (the run's journal, read once by
    /// the caller) against the definitions stored in `run_dir`, and reads
    /// its `playbook.yaml` and its diagnostics for detail. Read-only:
    /// nothing is created, repaired or backfilled.
    pub fn load(run_dir: &Path, events: &[Event]) -> Self {
        let diagnostics = crate::diagnostics::read(run_dir);
        let current = std::fs::read(run_dir.join("playbook.yaml")).ok();
        Self::resolve(run_dir, events, current.as_deref(), &diagnostics)
    }

    fn resolve(
        run_dir: &Path,
        events: &[Event],
        current: Option<&[u8]>,
        diagnostics: &[Diagnostic],
    ) -> Self {
        let bounds = boundaries(events);
        if bounds.is_empty() {
            return DefinitionsInForce {
                segments: vec![Segment {
                    from_seq: 0,
                    to_seq: None,
                    boundary: None,
                    resolved: Resolved::Unknown(UnknownReason::NotJournaled),
                    mismatch_recorded: false,
                }],
                playbook_changed: false,
            };
        }
        let mismatched: Vec<u64> = diagnostics
            .iter()
            .filter_map(|d| match d {
                Diagnostic::Mismatch(m) => Some(m.boundary_seq),
                _ => None,
            })
            .collect();
        let segments: Vec<Segment> = bounds
            .iter()
            .enumerate()
            .map(|(i, b)| Segment {
                from_seq: if i == 0 { 0 } else { b.seq },
                to_seq: bounds.get(i + 1).map(|n| n.seq.saturating_sub(1)),
                boundary: Some(b.clone()),
                resolved: resolve_one(run_dir, b.definition.as_deref()),
                mismatch_recorded: mismatched.contains(&b.seq),
            })
            .collect();
        let in_force = bounds.last().and_then(|b| b.definition.as_deref());
        let playbook_changed = in_force.is_some_and(|d| current.map(digest).as_deref() != Some(d));
        DefinitionsInForce {
            segments,
            playbook_changed,
        }
    }

    /// The segment in force at `seq`.
    pub fn at(&self, seq: u64) -> Option<&Segment> {
        self.segments
            .iter()
            .rev()
            .find(|s| s.from_seq <= seq && s.to_seq.is_none_or(|t| seq <= t))
    }

    /// Whether every segment's definition is established.
    pub fn all_known(&self) -> bool {
        self.segments.iter().all(|s| s.resolved.is_known())
    }
}

/// One boundary's definition: `Known` only on its own journaled digest and
/// stored bytes that hash to it and parse.
fn resolve_one(run_dir: &Path, definition: Option<&str>) -> Resolved {
    let Some(d) = definition else {
        return Resolved::Unknown(UnknownReason::NotJournaled);
    };
    let Some(bytes) = read_stored(run_dir, d) else {
        return Resolved::Unknown(UnknownReason::NotStored);
    };
    match String::from_utf8(bytes)
        .ok()
        .and_then(|y| crate::legacy_snapshot::parse_snapshot_playbook(&y).ok())
    {
        Some(playbook) => Resolved::Known {
            playbook: Box::new(playbook),
            digest: d.to_string(),
        },
        None => Resolved::Unknown(UnknownReason::Unreadable),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const V1: &str = "schema: 2\nid: p\nname: p\nversion: 1.0.0\nnodes:\n  - { id: start, type: start }\n  - { id: done, type: finish, outcome: success }\nedges:\n  - { from: start, to: done }\n";
    const V2: &str = "schema: 2\nid: p\nname: p\nversion: 1.0.1\nnodes:\n  - { id: start, type: start }\n  - { id: other, type: finish, outcome: success }\nedges:\n  - { from: start, to: other }\n";
    const V3: &str = "schema: 2\nid: p\nname: p\nversion: 1.0.2\nnodes:\n  - { id: start, type: start }\n  - { id: third, type: finish, outcome: success }\nedges:\n  - { from: start, to: third }\n";

    fn ev(seq: u64, body: serde_json::Value) -> Event {
        let mut v = body;
        v["seq"] = seq.into();
        v["ts"] = 0.into();
        serde_json::from_value(v).unwrap()
    }

    fn boundary(seq: u64, kind: &str, definition: Option<&str>) -> Event {
        let mut v = match kind {
            "start" => {
                serde_json::json!({"type": "run_started", "playbook": "p", "version": "1.0.0"})
            }
            "resume" => serde_json::json!({"type": "run_resumed", "from_node": "start"}),
            _ => {
                serde_json::json!({"type": "patch_applied", "version": "1.0.1", "classification": "improvement", "continue_from": "start"})
            }
        };
        if let Some(d) = definition {
            v["definition"] = d.into();
        }
        ev(seq, v)
    }

    fn started(seq: u64, definition: Option<&str>) -> Event {
        boundary(seq, "start", definition)
    }

    fn resumed(seq: u64, definition: Option<&str>) -> Event {
        boundary(seq, "resume", definition)
    }

    fn patch(seq: u64, definition: Option<&str>) -> Event {
        boundary(seq, "patch", definition)
    }

    fn run_dir() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("events.jsonl"), "").unwrap();
        dir
    }

    fn reasons(d: &DefinitionsInForce) -> Vec<Option<&'static str>> {
        d.segments
            .iter()
            .map(|s| match &s.resolved {
                Resolved::Known { .. } => None,
                Resolved::Unknown(r) => Some(r.as_str()),
            })
            .collect()
    }

    #[test]
    fn storing_is_write_once_private_and_refuses_a_linked_directory() {
        let dir = run_dir();
        let d = store(dir.path(), V1.as_bytes()).unwrap();
        assert_eq!(d, digest(V1.as_bytes()));
        assert_eq!(store(dir.path(), V1.as_bytes()).unwrap(), d);
        assert_eq!(read_stored(dir.path(), &d).unwrap(), V1.as_bytes());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(stored_path(dir.path(), &d))
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(mode & 0o077, 0);
            let other = run_dir();
            let target = tempfile::tempdir().unwrap();
            std::os::unix::fs::symlink(target.path(), other.path().join(DIR)).unwrap();
            assert!(store(other.path(), V1.as_bytes()).is_err());
            assert!(std::fs::read_dir(target.path()).unwrap().next().is_none());
        }
    }

    fn known(defs: &DefinitionsInForce) -> Vec<Option<String>> {
        defs.segments
            .iter()
            .map(|s| match &s.resolved {
                Resolved::Known { digest, .. } => Some(digest.clone()),
                Resolved::Unknown(_) => None,
            })
            .collect()
    }

    #[test]
    fn segments_before_and_after_a_patch_are_known_by_their_digests() {
        let dir = run_dir();
        let d1 = store(dir.path(), V1.as_bytes()).unwrap();
        let d2 = store(dir.path(), V2.as_bytes()).unwrap();
        std::fs::write(dir.path().join("playbook.yaml"), V2).unwrap();
        let events = vec![started(1, Some(&d1)), patch(9, Some(&d2))];
        let defs = DefinitionsInForce::load(dir.path(), &events);
        assert_eq!(reasons(&defs), vec![None, None]);
        let at = |seq| match &defs.at(seq).unwrap().resolved {
            Resolved::Known { digest, .. } => digest.clone(),
            Resolved::Unknown(_) => panic!("unknown"),
        };
        assert_eq!((at(5), at(9), at(40)), (d1.clone(), d2.clone(), d2));
        assert_eq!(defs.segments[0].to_seq, Some(8));
        assert!(!defs.playbook_changed);
    }

    /// Start on A; a patch to B stores B and replaces the snapshot, then
    /// crashes before `patch_applied`; a resume executes B; a later patch
    /// to C succeeds; the sidecar is gone. The resumed segment reads B,
    /// never A: the evidence is the digest each drive journaled.
    #[test]
    fn a_crashed_patch_resumed_then_patched_again_reads_what_each_drive_journaled() {
        let dir = run_dir();
        let a = store(dir.path(), V1.as_bytes()).unwrap();
        let mut events = vec![started(1, Some(&a))];
        // The crashed patch: B stored and in place, nothing journaled.
        store(dir.path(), V2.as_bytes()).unwrap();
        std::fs::write(dir.path().join("playbook.yaml"), V2).unwrap();
        let defs = DefinitionsInForce::load(dir.path(), &events);
        // The segment executed A up to the crash; the snapshot changed since.
        assert_eq!(known(&defs), vec![Some(a.clone())]);
        assert!(defs.playbook_changed);
        // The resume journals what it executes.
        let b = on_resume(dir.path(), &events, V2.as_bytes());
        assert_eq!(b, digest(V2.as_bytes()));
        events.push(resumed(10, Some(&b)));
        // A later patch to C.
        let c = store(dir.path(), V3.as_bytes()).unwrap();
        std::fs::write(dir.path().join("playbook.yaml"), V3).unwrap();
        events.push(patch(20, Some(&c)));
        std::fs::remove_file(dir.path().join(crate::diagnostics::FILE)).unwrap();
        let defs = DefinitionsInForce::load(dir.path(), &events);
        assert_eq!(known(&defs), vec![Some(a), Some(b.clone()), Some(c)]);
        assert_eq!(
            (defs.segments[1].from_seq, defs.segments[1].to_seq),
            (10, Some(19))
        );
        assert!(!defs.segments[1].mismatch_recorded);
        assert!(!defs.playbook_changed);
        // The same journal with the resume written by an older apb (no
        // digest): the resumed segment is unknown, never A.
        let old: Vec<Event> = events
            .iter()
            .map(|e| match e.seq {
                10 => resumed(10, None),
                _ => e.clone(),
            })
            .collect();
        let defs = DefinitionsInForce::load(dir.path(), &old);
        assert_eq!(
            reasons(&defs),
            vec![None, Some("definition_not_journaled"), None]
        );
    }

    #[test]
    fn a_mismatch_record_is_detail_and_never_decides_certainty() {
        let dir = run_dir();
        let a = store(dir.path(), V1.as_bytes()).unwrap();
        std::fs::write(dir.path().join("playbook.yaml"), V2).unwrap();
        let mut events = vec![started(1, Some(&a))];
        let b = on_resume(dir.path(), &events, V2.as_bytes());
        events.push(resumed(5, Some(&b)));
        let defs = DefinitionsInForce::load(dir.path(), &events);
        assert_eq!(known(&defs), vec![Some(a), Some(b)]);
        // Recorded against the start, whose segment executed A.
        assert!(defs.segments[0].mismatch_recorded);
        assert!(!defs.segments[1].mismatch_recorded);
        // Nothing was written by the read.
        let before = std::fs::read(dir.path().join(crate::diagnostics::FILE)).unwrap();
        DefinitionsInForce::load(dir.path(), &events);
        assert_eq!(
            std::fs::read(dir.path().join(crate::diagnostics::FILE)).unwrap(),
            before
        );
    }

    #[test]
    fn without_a_sidecar_certainty_still_needs_the_digests() {
        let dir = run_dir();
        let d1 = store(dir.path(), V1.as_bytes()).unwrap();
        std::fs::write(dir.path().join("playbook.yaml"), V1).unwrap();
        let events = vec![started(1, Some(&d1))];
        assert_eq!(
            reasons(&DefinitionsInForce::load(dir.path(), &events)),
            vec![None]
        );
        // A run started before definitions were journaled: no evidence, no
        // certainty, though no diagnostic says anything.
        let old = vec![started(1, None)];
        assert_eq!(
            reasons(&DefinitionsInForce::load(dir.path(), &old)),
            vec![Some("definition_not_journaled")]
        );
        // No boundary at all.
        assert_eq!(
            reasons(&DefinitionsInForce::load(dir.path(), &[])),
            vec![Some("definition_not_journaled")]
        );
        // A journaled digest whose stored file is gone.
        std::fs::remove_file(stored_path(dir.path(), &d1)).unwrap();
        assert_eq!(
            reasons(&DefinitionsInForce::load(dir.path(), &events)),
            vec![Some("definition_not_stored")]
        );
    }

    #[test]
    fn a_stored_definition_that_does_not_parse_is_unreadable() {
        let dir = run_dir();
        let bad = store(dir.path(), b"nodes: [ this is not a playbook").unwrap();
        let events = vec![started(1, Some(&bad))];
        assert_eq!(
            reasons(&DefinitionsInForce::load(dir.path(), &events)),
            vec![Some("definition_unreadable")]
        );
    }

    #[test]
    fn an_older_patch_without_a_digest_leaves_the_start_known_and_the_rest_unknown() {
        let dir = run_dir();
        let d1 = store(dir.path(), V1.as_bytes()).unwrap();
        std::fs::write(dir.path().join("playbook.yaml"), V2).unwrap();
        let events = vec![started(1, Some(&d1)), patch(9, None)];
        let found = detect_mismatch(&events, V2.as_bytes()).unwrap();
        assert_eq!(
            (found.boundary_seq, found.reason.as_str()),
            (9, "patch_without_definition")
        );
        let d2 = on_resume(dir.path(), &events, V2.as_bytes());
        let mut events = events;
        events.push(resumed(12, Some(&d2)));
        let defs = DefinitionsInForce::load(dir.path(), &events);
        assert_eq!(
            reasons(&defs),
            vec![None, Some("definition_not_journaled"), None]
        );
        assert!(defs.segments[1].mismatch_recorded);
    }

    #[test]
    fn a_resume_without_a_digest_is_skipped_by_the_mismatch_check() {
        let d1 = digest(V1.as_bytes());
        let events = vec![started(1, Some(&d1)), resumed(4, None)];
        let found = detect_mismatch(&events, V2.as_bytes()).unwrap();
        assert_eq!(
            (found.boundary_seq, found.reason.as_str()),
            (1, "digest_differs")
        );
        assert!(detect_mismatch(&events, V1.as_bytes()).is_none());
        // Nothing to compare with: a journal of digest-less boundaries.
        assert!(detect_mismatch(&[started(1, None), resumed(3, None)], V2.as_bytes()).is_none());
    }

    #[test]
    fn mismatches_are_one_record_per_boundary_and_two_across_a_patch() {
        let dir = run_dir();
        let d1 = store(dir.path(), V1.as_bytes()).unwrap();
        let mut events = vec![started(1, Some(&d1))];
        // Two resumes over the same edited snapshot, both written by an
        // older apb (no digest): one record against the start.
        on_resume(dir.path(), &events, V2.as_bytes());
        events.push(resumed(5, None));
        on_resume(dir.path(), &events, V2.as_bytes());
        // A successful patch, then another edit and resume: a second record.
        let d3 = store(dir.path(), V3.as_bytes()).unwrap();
        events.push(patch(20, Some(&d3)));
        on_resume(dir.path(), &events, V2.as_bytes());
        let records: Vec<u64> = crate::diagnostics::read(dir.path())
            .into_iter()
            .filter_map(|d| match d {
                crate::diagnostics::Diagnostic::Mismatch(m) => Some(m.boundary_seq),
                _ => None,
            })
            .collect();
        assert_eq!(records, vec![1, 20]);
        // A resume over the matching snapshot records nothing.
        assert!(detect_mismatch(&events, V3.as_bytes()).is_none());
    }
}
