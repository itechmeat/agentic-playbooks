//! `apb stats` by executor digest: when a node of one version ran under
//! more than one profile or bundle digest (a profile, its model, its SOUL or
//! its skills changed between runs), its figures per digest pair, and in
//! `--compare` the nodes whose latest executor differs between the two
//! versions.

use std::collections::{BTreeMap, BTreeSet};

use serde::Serialize;

use super::rate::Rate;
use super::stats_run::StatsRun;
use super::waits::Waits;
use crate::decision::report::stats::median;
use crate::manifest::NodeDigests;
use crate::run_estimate::sample::RunSample;

/// How many hex characters of a digest the report shows.
const SHORT: usize = 12;

/// The digest's first [`SHORT`] hex characters, its algorithm prefix
/// (`sha256:`) left out.
fn short(d: &str) -> String {
    let hex = d.split_once(':').map_or(d, |(_, h)| h);
    hex.chars().take(SHORT).collect()
}

/// A node's figures over the runs that bound it to one executor.
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct DigestStats {
    /// The profile digest, shortened.
    pub profile: String,
    /// The bundle digest (profile plus skills), shortened.
    pub bundle: String,
    /// Runs the node ran in under this executor.
    pub runs: usize,
    /// Runs where the node passed on its one visit and first attempt,
    /// without a fallback.
    pub first_pass: Rate,
    pub duration: Waits,
    /// The median tokens of a visit that reported usage on every attempt.
    #[cfg_attr(feature = "ts", ts(optional))]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tokens_per_visit: Option<u64>,
}

/// A node whose latest executor differs between two versions.
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct ExecutorChange {
    pub node: String,
    pub base_profile: String,
    pub against_profile: String,
    pub base_bundle: String,
    pub against_bundle: String,
}

/// The digests of `node` per run that bound it.
fn bound<'a>(node: &str, runs: &[&'a StatsRun]) -> Vec<(&'a StatsRun, &'a NodeDigests)> {
    runs.iter()
        .filter_map(|r| Some((*r, r.node_profiles.get(node)?)))
        .collect()
}

/// Whether any node of `runs` was bound to more than one executor.
pub(super) fn any_change(runs: &[&StatsRun]) -> bool {
    let mut seen: BTreeMap<&str, BTreeSet<&NodeDigests>> = BTreeMap::new();
    for r in runs {
        for (node, d) in &r.node_profiles {
            seen.entry(node).or_default().insert(d);
        }
    }
    seen.values().any(|s| s.len() > 1)
}

/// The groups of `node` per executor, oldest first; empty when every run
/// bound it to the same one. `samples` are the runs' samples, by run id.
pub(super) fn node_digests(
    node: &str,
    runs: &[&StatsRun],
    samples: &BTreeMap<String, RunSample>,
) -> Vec<DigestStats> {
    let mut bound = bound(node, runs);
    bound.sort_by_cached_key(|(r, _)| r.started_ms());
    let mut groups: Vec<(&NodeDigests, Vec<&RunSample>)> = Vec::new();
    for (run, d) in bound {
        let Some(s) = samples
            .get(&run.run_id)
            .filter(|s| s.visits.contains_key(node))
        else {
            continue;
        };
        match groups.iter_mut().find(|(g, _)| *g == d) {
            Some((_, v)) => v.push(s),
            None => groups.push((d, vec![s])),
        }
    }
    if groups.len() < 2 {
        return Vec::new();
    }
    groups
        .into_iter()
        .map(|(d, runs)| {
            let visits: Vec<_> = runs.iter().flat_map(|r| &r.visits[node]).collect();
            let first_pass = runs
                .iter()
                .filter(|r| {
                    let v = &r.visits[node];
                    v.len() == 1
                        && v[0].attempts == 1
                        && !v[0].fell_back
                        && v[0].status == "succeeded"
                })
                .count();
            let mut tokens: Vec<u64> = visits.iter().filter_map(|v| v.tokens).collect();
            DigestStats {
                profile: short(&d.profile),
                bundle: short(&d.bundle),
                runs: runs.len(),
                first_pass: Rate::new(first_pass, runs.len()),
                duration: Waits::of(visits.iter().map(|v| v.active_ms).collect()),
                tokens_per_visit: median(&mut tokens),
            }
        })
        .collect()
}

/// Each node's executor in the newest run of `runs` that bound it.
fn latest<'a>(runs: &[&'a StatsRun]) -> BTreeMap<&'a str, &'a NodeDigests> {
    let mut by_time: Vec<&StatsRun> = runs.to_vec();
    by_time.sort_by_cached_key(|r| r.started_ms());
    let mut out = BTreeMap::new();
    for r in by_time {
        for (node, d) in &r.node_profiles {
            out.insert(node.as_str(), d);
        }
    }
    out
}

/// The nodes whose latest executor differs between the `base` and the
/// `against` runs.
pub(super) fn executor_changes(base: &[&StatsRun], against: &[&StatsRun]) -> Vec<ExecutorChange> {
    let (a, b) = (latest(base), latest(against));
    a.iter()
        .filter_map(|(node, x)| {
            let y = b.get(node)?;
            (x != y).then(|| ExecutorChange {
                node: (*node).to_string(),
                base_profile: short(&x.profile),
                against_profile: short(&y.profile),
                base_bundle: short(&x.bundle),
                against_bundle: short(&y.bundle),
            })
        })
        .collect()
}

#[cfg(test)]
mod short_tests {
    #[test]
    fn a_short_digest_keeps_hex_not_the_algorithm_prefix() {
        assert_eq!(super::short("sha256:3f2a1b9c0e4d5f6a7b8c"), "3f2a1b9c0e4d");
        assert_eq!(super::short("abc"), "abc");
    }
}
