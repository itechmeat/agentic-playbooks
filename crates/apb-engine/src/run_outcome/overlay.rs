//! A run laid over a playbook version: the playbook page's "last run" layer.
//!
//! **Exact** means the run executed exactly the shown version's stored
//! bytes: same version, every definition boundary of its journal known,
//! and every segment's digest equal to the digest of the shown YAML. Every
//! boundary is checked, so a run that went A, then B by a patch, then A
//! again is not exact. A boundary without a journaled digest (an older apb)
//! stays unknown: no snapshot comparison stands in for it.
//!
//! The default run is the newest exact run, searched over every run of the
//! playbook in the project newest first, independent of the candidate page.
//! Candidates come in pages of [`PAGE`], newest first, any version; `run=`
//! resolves any run of the playbook. Each known segment is mapped to the
//! shown definition by its own correspondence; an unknown segment has none.

use std::path::{Path, PathBuf};

use apb_core::versioning::{EdgeCorrespondence, diff_definitions};
use serde::Serialize;

use super::graph::RunGraph;
use crate::run_definitions::{DefinitionsInForce, Resolved, digest, read_stored};

/// Candidates per page.
pub const PAGE: usize = 20;

/// How a run's executed definitions compare with the shown version.
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Alignment {
    /// Every segment executed the shown stored bytes.
    Exact,
    /// The shown version, but another definition (run overrides).
    SameVersionOtherDefinition,
    /// The shown version, patched while it ran.
    Patched,
    /// Another version.
    OtherVersion,
    /// A segment's definition is not established.
    Unknown,
}

/// One run of the playbook the picker offers.
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct OverlayCandidate {
    pub run_id: String,
    pub version: String,
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub started_ts: u128,
    pub alignment: Alignment,
    /// Why it is not exact, in words.
    pub reason: Option<String>,
}

/// One known segment of the chosen run mapped onto the shown definition.
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct SegmentMapping {
    pub segment: usize,
    /// Each edge of either definition with its counterpart (`before` is the
    /// run's segment, `after` the shown version); `None` for an unknown
    /// segment, which is never projected.
    pub correspondence: Option<Vec<EdgeCorrespondence>>,
    /// Nodes the segment has and the shown version lacks.
    pub missing_in_shown: Vec<String>,
    /// Nodes the shown version has and the segment lacks.
    pub missing_in_run: Vec<String>,
}

/// The overlay of one run on one version.
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct PlaybookOverlay {
    pub candidates: Vec<OverlayCandidate>,
    /// The `before` of the next page, when there is one.
    #[cfg_attr(feature = "ts", ts(type = "number | null"))]
    pub next_before: Option<u128>,
    /// The newest exact run of the shown version.
    pub default_run: Option<String>,
    /// The run laid over the graph.
    pub chosen: Option<String>,
    pub alignment: Option<Alignment>,
    pub reason: Option<String>,
    /// The chosen run's graph, through its run's redactor on the way out
    /// (a surface replaces it with [`RunGraph::present`]); a string when
    /// the run's text is withheld.
    #[cfg_attr(feature = "ts", ts(type = "RunGraph | string | null"))]
    pub graph: Option<RunGraph>,
    pub mapping: Vec<SegmentMapping>,
}

/// What [`overlay`] reads.
pub struct OverlayRequest<'a> {
    /// The project root whose `.apb/runs` holds the runs.
    pub root: &'a Path,
    pub playbook: &'a str,
    pub version: &'a str,
    /// The shown version's stored YAML bytes.
    pub yaml: &'a str,
    pub run: Option<&'a str>,
    pub before: Option<u128>,
}

struct Head {
    run_id: String,
    dir: PathBuf,
    version: String,
    ts: u128,
}

fn runs_of(root: &Path, playbook: &str) -> Vec<Head> {
    let mut out: Vec<Head> = crate::decision::report::candidate_run_dirs(&[root.to_path_buf()])
        .into_iter()
        .filter_map(|dir| {
            let head = crate::run_history::started(&dir)?;
            let run_id = dir.file_name()?.to_string_lossy().into_owned();
            (head.playbook == playbook).then_some(Head {
                run_id,
                version: head.version,
                ts: head.ts,
                dir,
            })
        })
        .collect();
    out.sort_by(|a, b| b.ts.cmp(&a.ts).then_with(|| b.run_id.cmp(&a.run_id)));
    out
}

/// How the run at `dir` (of `version`) compares with the shown version,
/// whose stored bytes hash to `shown`.
pub fn alignment_of(
    dir: &Path,
    version: &str,
    shown_version: &str,
    shown: &str,
) -> (Alignment, Option<String>) {
    if version != shown_version {
        return (
            Alignment::OtherVersion,
            Some(format!("the run executed version {version}")),
        );
    }
    let Ok(events) = crate::run_view::read_events(dir) else {
        return (
            Alignment::Unknown,
            Some("the run's journal does not read".into()),
        );
    };
    let defs = DefinitionsInForce::load(dir, &events);
    if let Some(s) = defs.segments.iter().find(|s| !s.resolved.is_known()) {
        let why = match &s.resolved {
            Resolved::Unknown(r) => r.as_str(),
            Resolved::Known { .. } => "",
        };
        return (
            Alignment::Unknown,
            Some(format!(
                "a definition of the run is not established ({why})"
            )),
        );
    }
    let all_shown = defs.segments.iter().all(|s| match &s.resolved {
        Resolved::Known { digest, .. } => digest == shown,
        Resolved::Unknown(_) => false,
    });
    if all_shown {
        return (Alignment::Exact, None);
    }
    let patched = defs.segments.iter().any(|s| {
        s.boundary
            .as_ref()
            .is_some_and(|b| b.kind == crate::run_definitions::BoundaryKind::Patch)
    });
    match patched {
        true => (
            Alignment::Patched,
            Some("the run was patched while it ran".into()),
        ),
        false => (
            Alignment::SameVersionOtherDefinition,
            Some("run overrides: the run executed another definition of this version".into()),
        ),
    }
}

/// The overlay for `req`.
pub fn overlay(req: &OverlayRequest<'_>) -> PlaybookOverlay {
    let shown = digest(req.yaml.as_bytes());
    let runs = runs_of(req.root, req.playbook);
    let align = |h: &Head| alignment_of(&h.dir, &h.version, req.version, &shown);
    let page: Vec<&Head> = runs
        .iter()
        .filter(|h| req.before.is_none_or(|b| h.ts < b))
        .take(PAGE + 1)
        .collect();
    let next_before = (page.len() > PAGE).then(|| page[PAGE - 1].ts);
    let candidates = page
        .iter()
        .take(PAGE)
        .map(|h| {
            let (alignment, reason) = align(h);
            OverlayCandidate {
                run_id: h.run_id.clone(),
                version: h.version.clone(),
                started_ts: h.ts,
                alignment,
                reason,
            }
        })
        .collect();
    let default_run = runs
        .iter()
        .filter(|h| h.version == req.version)
        .find(|h| align(h).0 == Alignment::Exact)
        .map(|h| h.run_id.clone());
    let chosen = req
        .run
        .and_then(|r| runs.iter().find(|h| h.run_id == r))
        .or_else(|| {
            default_run
                .as_ref()
                .and_then(|d| runs.iter().find(|h| &h.run_id == d))
        });
    let mut out = PlaybookOverlay {
        candidates,
        next_before,
        default_run,
        chosen: None,
        alignment: None,
        reason: None,
        graph: None,
        mapping: Vec::new(),
    };
    let Some(h) = chosen else {
        return out;
    };
    let (alignment, reason) = align(h);
    out.chosen = Some(h.run_id.clone());
    out.alignment = Some(alignment);
    out.reason = reason;
    if let Ok(view) = crate::run_view::RunView::load(&h.dir, &h.run_id) {
        let defs = DefinitionsInForce::load(&h.dir, &view.events);
        out.mapping = mappings(&h.dir, &defs, req.yaml);
        let diagnostics = crate::diagnostics::read(&h.dir);
        out.graph = Some(RunGraph::build(&super::graph::GraphInput {
            events: &view.events,
            definitions: &defs,
            diagnostics: &diagnostics,
            statuses: &view.nodes(),
        }));
    }
    out
}

/// Each segment of the run mapped onto the shown YAML through its own
/// correspondence.
pub fn mappings(run_dir: &Path, defs: &DefinitionsInForce, shown: &str) -> Vec<SegmentMapping> {
    defs.segments
        .iter()
        .enumerate()
        .map(|(segment, s)| {
            let diff = match &s.resolved {
                Resolved::Known { digest, .. } => read_stored(run_dir, digest)
                    .and_then(|b| String::from_utf8(b).ok())
                    .and_then(|y| diff_definitions(&y, shown).ok()),
                Resolved::Unknown(_) => None,
            };
            match diff {
                Some(d) => SegmentMapping {
                    segment,
                    correspondence: Some(d.edges),
                    missing_in_shown: d.nodes_removed,
                    missing_in_run: d.nodes_added,
                },
                None => SegmentMapping {
                    segment,
                    correspondence: None,
                    missing_in_shown: Vec::new(),
                    missing_in_run: Vec::new(),
                },
            }
        })
        .collect()
}
