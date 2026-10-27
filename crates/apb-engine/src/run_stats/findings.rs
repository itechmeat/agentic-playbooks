//! The findings, warnings and baseline comparisons of a version's runs, for
//! `apb stats`: what keeps coming up across runs, and how each baseline
//! observation fares.
//!
//! Findings are counted once per run per (node, kind, location, evidence),
//! so a retry reporting the same finding again does not inflate them, and
//! grouped across runs by kind and [`location_key`]. A baseline observation
//! counts its current comparison in each run, as
//! [`BaselineState`](crate::baseline::BaselineState) reads it.

use std::collections::{BTreeMap, BTreeSet};

use apb_core::run_notes::location_key;
use serde::Serialize;

use apb_core::schema::Playbook;

use super::rate::Rate;
use crate::baseline::BaselineState;
use crate::event::{Event, EventPayload};

/// How many finding groups a version lists.
pub const TOP_GROUPS: usize = 20;

/// One finding kind over the runs.
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct KindCount {
    pub kind: String,
    pub occurrences: usize,
    pub runs: usize,
}

/// One kind and location reported across runs.
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct FindingGroup {
    pub kind: String,
    pub location: String,
    pub runs: usize,
    pub occurrences: usize,
    /// The latest run that reported it.
    pub last_run: String,
}

/// The findings and warnings of a version's runs.
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct FindingStats {
    pub by_kind: Vec<KindCount>,
    /// The most widespread groups, by runs, at most [`TOP_GROUPS`].
    pub top: Vec<FindingGroup>,
    pub runs_with_warnings: Rate,
}

/// One baseline observation over the runs that compared it.
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct BaselineStats {
    pub check: String,
    pub observation: String,
    /// Runs that compared it.
    pub compared: usize,
    pub met: Rate,
    pub regressed: usize,
    pub unknown: usize,
}

#[derive(Default)]
struct GroupAcc {
    runs: usize,
    occurrences: usize,
    last_run: String,
    /// When `last_run` started: runs arrive by directory, not by time.
    last_started: u128,
}

/// Accumulates the runs of one version.
#[derive(Default)]
pub(super) struct Acc {
    runs: usize,
    runs_with_warnings: usize,
    kinds: BTreeMap<String, (usize, usize)>,
    groups: BTreeMap<(String, String), GroupAcc>,
    baseline: BTreeMap<(String, String), BaselineStats>,
}

impl Acc {
    /// Adds run `run_id`, which started at `started` (epoch ms), read
    /// against its playbook snapshot when it reads.
    pub(super) fn add(
        &mut self,
        run_id: &str,
        started: u128,
        playbook: Option<&Playbook>,
        events: &[Event],
    ) {
        self.runs += 1;
        let mut seen: BTreeSet<(&str, &str, &str, &str)> = BTreeSet::new();
        let mut warned = false;
        for e in events {
            if let EventPayload::AttemptNotes {
                node,
                findings,
                warnings,
                ..
            } = &e.payload
            {
                warned |= !warnings.is_empty();
                for f in findings {
                    seen.insert((node, &f.kind, &f.location, &f.evidence));
                }
            }
        }
        self.runs_with_warnings += usize::from(warned);
        let mut kinds_in_run: BTreeSet<&str> = BTreeSet::new();
        let mut groups_in_run: BTreeSet<(String, String)> = BTreeSet::new();
        for (_, kind, location, _) in &seen {
            self.kinds.entry(kind.to_string()).or_default().0 += 1;
            kinds_in_run.insert(kind);
            let key = (kind.to_string(), location_key(location));
            self.groups.entry(key.clone()).or_default().occurrences += 1;
            groups_in_run.insert(key);
        }
        for kind in kinds_in_run {
            self.kinds.entry(kind.to_string()).or_default().1 += 1;
        }
        for key in groups_in_run {
            let g = self.groups.entry(key).or_default();
            g.runs += 1;
            if g.last_run.is_empty() || started >= g.last_started {
                g.last_run = run_id.to_string();
                g.last_started = started;
            }
        }
        let state = BaselineState::fold_with(playbook, events);
        for (check, c) in state.current() {
            let (observation, verdict, regressed) =
                (c.observation.as_str(), c.verdict.as_str(), c.regressed);
            let b = self
                .baseline
                .entry((check.to_string(), observation.to_string()))
                .or_insert_with(|| BaselineStats {
                    check: check.to_string(),
                    observation: observation.to_string(),
                    ..BaselineStats::default()
                });
            b.compared += 1;
            if verdict == apb_core::baseline::Verdict::Met.as_str() {
                b.met.count += 1;
            } else if verdict == apb_core::baseline::Verdict::Unknown.as_str() {
                b.unknown += 1;
            }
            b.regressed += usize::from(regressed == Some(true));
        }
    }

    /// The finding statistics and the baseline rows.
    pub(super) fn finish(self) -> (FindingStats, Vec<BaselineStats>) {
        let by_kind = self
            .kinds
            .into_iter()
            .map(|(kind, (occurrences, runs))| KindCount {
                kind,
                occurrences,
                runs,
            })
            .collect();
        let mut top: Vec<FindingGroup> = self
            .groups
            .into_iter()
            .map(|((kind, location), g)| FindingGroup {
                kind,
                location,
                runs: g.runs,
                occurrences: g.occurrences,
                last_run: g.last_run,
            })
            .collect();
        top.sort_by(|a, b| {
            b.runs
                .cmp(&a.runs)
                .then(b.occurrences.cmp(&a.occurrences))
                .then_with(|| (&a.kind, &a.location).cmp(&(&b.kind, &b.location)))
        });
        top.truncate(TOP_GROUPS);
        let baseline = self
            .baseline
            .into_values()
            .map(|mut b| {
                b.met = Rate::new(b.met.count, b.compared);
                b
            })
            .collect();
        (
            FindingStats {
                by_kind,
                top,
                runs_with_warnings: Rate::new(self.runs_with_warnings, self.runs),
            },
            baseline,
        )
    }
}

/// The text lines `apb stats` prints for a version's findings and baseline.
/// The kinds and locations an agent reported, and the check and
/// observation names a journal carries, are shown with any hidden
/// character replaced, so a journal written by hand cannot drive the
/// terminal.
pub(super) fn lines(findings: &FindingStats, baseline: &[BaselineStats]) -> Vec<String> {
    use apb_core::text::visible;
    let mut out = Vec::new();
    if !findings.by_kind.is_empty() {
        let kinds: Vec<String> = findings
            .by_kind
            .iter()
            .map(|k| {
                format!(
                    "{}: {} {} in {} runs",
                    visible(&k.kind),
                    k.occurrences,
                    if k.occurrences == 1 {
                        "finding"
                    } else {
                        "findings"
                    },
                    k.runs
                )
            })
            .collect();
        out.push(format!("findings: {}", kinds.join(", ")));
        for g in &findings.top {
            out.push(format!(
                "  {} at {}: {} runs, last {}",
                visible(&g.kind),
                visible(&g.location),
                g.runs,
                visible(&g.last_run)
            ));
        }
    }
    if findings.runs_with_warnings.count > 0 {
        out.push(format!(
            "runs with warnings: {}",
            findings.runs_with_warnings.text()
        ));
    }
    for b in baseline {
        let mut line = format!(
            "baseline {}.{}: met {}",
            visible(&b.check),
            visible(&b.observation),
            b.met.text()
        );
        if b.regressed > 0 {
            line.push_str(&format!(", regressed in {}", b.regressed));
        }
        if b.unknown > 0 {
            line.push_str(&format!(", unknown in {}", b.unknown));
        }
        out.push(line);
    }
    out
}
