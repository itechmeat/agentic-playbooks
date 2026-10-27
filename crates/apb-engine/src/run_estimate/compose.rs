//! Turns samples of comparable runs into the run's figures: a spread per
//! node and dimension, list nodes scaled by their item count, loops through
//! each node's per-run totals, and the run sums.

use std::collections::BTreeMap;

use apb_core::schema::Node;

use super::sample::{AttemptSpend, RunSample};
use super::types::{
    AttemptMedian, EstimateBasis, EstimateTarget, ItemsBasis, ListEstimate, NodeEstimate,
    NodeSpread, SpendEstimate, Spread, SpreadKind, TimeEstimate,
};
use crate::decision::report::stats::quantile;

impl Spread<f64> {
    /// The spread of `values`: p25 to p75 from 4 values on, min to max for
    /// 2 or 3, a single value for 1; `None` for none.
    pub fn of(mut values: Vec<f64>) -> Option<Self> {
        values.retain(|v| v.is_finite());
        values.sort_by(f64::total_cmp);
        let n = values.len();
        let p50 = quantile(&values, 0.5)?;
        let (kind, low, high) = match n {
            1 => (SpreadKind::Single, p50, p50),
            2 | 3 => (SpreadKind::MinMax, values[0], values[n - 1]),
            _ => (
                SpreadKind::Iqr,
                quantile(&values, 0.25)?,
                quantile(&values, 0.75)?,
            ),
        };
        Some(Spread {
            n: n as u32,
            p50,
            low,
            high,
            kind,
        })
    }

    /// Every figure times `k`.
    pub fn scaled(&self, k: f64) -> Self {
        Spread {
            p50: self.p50 * k,
            low: self.low * k,
            high: self.high * k,
            ..*self
        }
    }

    /// Whole numbers, for tokens and milliseconds.
    pub fn rounded(&self) -> Spread<u64> {
        let r = |x: f64| x.max(0.0).round() as u64;
        Spread {
            n: self.n,
            p50: r(self.p50),
            low: r(self.low),
            high: r(self.high),
            kind: self.kind,
        }
    }
}

impl Spread<u64> {
    pub fn as_f64(&self) -> Spread<f64> {
        Spread {
            n: self.n,
            p50: self.p50 as f64,
            low: self.low as f64,
            high: self.high as f64,
            kind: self.kind,
        }
    }
}

/// The sum of node spreads: the p50s add up (an approximation of the p50
/// of the sum), and so do the lows and the highs, which assumes the nodes
/// move together and keeps the range deliberately wide. `n` is the
/// smallest sample; the sum is a single figure only when its range is
/// empty, else a min-max range when any part is one (the widest kind), else
/// an interquartile one.
fn sum(parts: &[Spread<f64>]) -> Option<Spread<f64>> {
    let n = parts.iter().map(|p| p.n).min()?;
    let (low, high): (f64, f64) = (
        parts.iter().map(|p| p.low).sum(),
        parts.iter().map(|p| p.high).sum(),
    );
    let kind = if low == high {
        SpreadKind::Single
    } else if parts.iter().any(|p| p.kind == SpreadKind::Iqr)
        && !parts.iter().any(|p| p.kind == SpreadKind::MinMax)
    {
        SpreadKind::Iqr
    } else {
        SpreadKind::MinMax
    };
    Some(Spread {
        n,
        p50: parts.iter().map(|p| p.p50).sum(),
        low,
        high,
        kind,
    })
}

fn median_f(values: Vec<f64>) -> Option<f64> {
    Spread::of(values).map(|s| s.p50)
}

fn tokens_of(values: impl IntoIterator<Item = Option<u64>>) -> Option<Spread<u64>> {
    Spread::of(values.into_iter().flatten().map(|v| v as f64).collect()).map(|s| s.rounded())
}

fn cost_of(values: impl IntoIterator<Item = Option<f64>>) -> Option<Spread<f64>> {
    Spread::of(values.into_iter().flatten().collect())
}

/// One node's figures over the runs whose binding of it is comparable.
pub(super) fn node_estimate(node: &Node, runs: &[&RunSample]) -> NodeEstimate {
    let id = node.id.as_str();
    let visits: Vec<_> = runs
        .iter()
        .flat_map(|r| r.visits.get(id).into_iter().flatten())
        .collect();
    let attempts: Vec<&AttemptSpend> = runs
        .iter()
        .flat_map(|r| r.attempts.get(id).into_iter().flatten())
        .collect();
    let per_visit = NodeSpread {
        tokens: tokens_of(visits.iter().map(|v| v.tokens)),
        cost_usd: cost_of(visits.iter().map(|v| v.cost)),
        ms: tokens_of(visits.iter().map(|v| Some(v.active_ms))),
    };
    let per_run = NodeSpread {
        tokens: tokens_of(runs.iter().map(|r| r.per_run(id, |v| v.tokens))),
        cost_usd: cost_of(runs.iter().map(|r| r.per_run(id, |v| v.cost))),
        ms: tokens_of(runs.iter().map(|r| r.per_run(id, |v| Some(v.active_ms)))),
    };
    let per_attempt_p50 = AttemptMedian {
        tokens: median_f(
            attempts
                .iter()
                .filter_map(|a| a.tokens)
                .map(|t| t as f64)
                .collect(),
        )
        .map(|t| t.round() as u64),
        cost_usd: median_f(attempts.iter().filter_map(|a| a.cost).collect()),
    };
    let visits_p50 = median_f(
        runs.iter()
            .map(|r| r.visits.get(id).map_or(0, Vec::len) as f64)
            .collect(),
    )
    .unwrap_or(0.0);
    NodeEstimate {
        node: id.to_string(),
        visits_p50,
        per_visit,
        per_run,
        per_attempt_p50,
        list: node.list().map(|spec| list_estimate(spec, id, runs)),
    }
}

fn list_estimate(spec: &apb_core::list::ListSpec, id: &str, runs: &[&RunSample]) -> ListEstimate {
    let lists: Vec<_> = runs
        .iter()
        .flat_map(|r| r.lists.get(id).into_iter().flatten())
        .collect();
    let items: Vec<&AttemptSpend> = lists.iter().flat_map(|l| &l.items).collect();
    let (count, basis) = match inline_count(spec) {
        Some(n) => (Some(n), ItemsBasis::Inline),
        None => match median_f(lists.iter().map(|l| f64::from(l.total)).collect()) {
            Some(m) => (Some(m.round() as u32), ItemsBasis::History),
            None => (None, ItemsBasis::Unknown),
        },
    };
    ListEstimate {
        per_item: NodeSpread {
            tokens: tokens_of(items.iter().map(|i| i.tokens)),
            cost_usd: cost_of(items.iter().map(|i| i.cost)),
            ms: None,
        },
        items: count,
        items_basis: basis,
        batch_ms_p50: median_f(
            lists
                .iter()
                .flat_map(|l| &l.batch_ms)
                .map(|m| *m as f64)
                .collect(),
        )
        .map(|m| m.round() as u64),
        batch_size: spec.batch.size.max(1),
    }
}

/// The item count of an inline list, after its filter.
fn inline_count(spec: &apb_core::list::ListSpec) -> Option<u32> {
    let src = &spec.items;
    if src.from.is_some() {
        return None;
    }
    let items =
        apb_core::list::items::items_from_values(&src.inline, &src.key, src.max_items).ok()?;
    let sourced = items
        .into_iter()
        .map(|item| apb_core::list::items::SourcedItem { item, state: None })
        .collect();
    let (kept, _) = apb_core::list::items::apply_filter(sourced, src.filter.as_ref());
    Some(kept.len() as u32)
}

/// A node's contribution to the run total in one dimension: a list node's
/// per-item spread times its item count, else its per-run spread. `None`
/// when the node cannot be estimated in that dimension.
fn run_part(
    n: &NodeEstimate,
    pick: impl Fn(&NodeSpread) -> Option<Spread<f64>>,
) -> Option<Spread<f64>> {
    match &n.list {
        Some(l) => Some(pick(&l.per_item)?.scaled(f64::from(l.items?))),
        None => pick(&n.per_run),
    }
}

/// Whether the gate has nothing to weigh for this node: no token and no
/// cost sample (for a list node: per item, or no item count).
pub(super) fn unestimated(n: &NodeEstimate) -> bool {
    let empty = |s: &NodeSpread| s.tokens.is_none() && s.cost_usd.is_none();
    match &n.list {
        Some(l) => empty(&l.per_item) || l.items.is_none(),
        None => empty(&n.per_visit),
    }
}

/// The gated nodes the gate has no token figure for, and those it has no
/// cost figure for (see [`NodeEstimate::gate_figure`]).
pub(super) fn unestimated_by_dimension(nodes: &[NodeEstimate]) -> (Vec<String>, Vec<String>) {
    let pick = |f: fn(&NodeSpread) -> bool| {
        nodes
            .iter()
            .filter(|n| unestimated(n) || !f(n.gate_figure()))
            .map(|n| n.node.clone())
            .collect()
    };
    (pick(|s| s.tokens.is_some()), pick(|s| s.cost_usd.is_some()))
}

/// The run's tokens and cost: the sum over the estimated gated nodes.
pub(super) fn run_spend(
    nodes: &[NodeEstimate],
) -> (Option<SpendEstimate<u64>>, Option<SpendEstimate<f64>>) {
    let est = |n: &&NodeEstimate| !unestimated(n);
    let tokens: Vec<_> = nodes
        .iter()
        .filter(est)
        .filter_map(|n| run_part(n, |s| s.tokens.map(|t| t.as_f64())))
        .collect();
    let cost: Vec<_> = nodes
        .iter()
        .filter(est)
        .filter_map(|n| run_part(n, |s| s.cost_usd))
        .collect();
    (
        sum(&tokens).map(|s| SpendEstimate {
            basis: EstimateBasis::History,
            spread: s.rounded(),
        }),
        sum(&cost).map(|s| SpendEstimate {
            basis: EstimateBasis::History,
            spread: s,
        }),
    )
}

/// The run's time: active time over comparable succeeded runs, else the
/// declared expected durations, else nothing.
pub(super) fn time(target: &EstimateTarget, succeeded: &[&RunSample]) -> TimeEstimate {
    let history = tokens_of(succeeded.iter().map(|r| r.active_ms));
    if let Some(spread) = history {
        return TimeEstimate {
            basis: EstimateBasis::History,
            spread: Some(spread),
            human_wait_p50_ms: median_f(succeeded.iter().map(|r| r.human_wait_ms as f64).collect())
                .map(|m| m.round() as u64),
        };
    }
    let declared: u64 = target
        .playbook
        .nodes
        .iter()
        .filter(|n| n.kind.needs_duration_estimate())
        .map(|n| n.expected_seconds().saturating_mul(1000))
        .fold(0u64, u64::saturating_add);
    if declared == 0 {
        return TimeEstimate {
            basis: EstimateBasis::None,
            spread: None,
            human_wait_p50_ms: None,
        };
    }
    TimeEstimate {
        basis: EstimateBasis::Declared,
        spread: Some(Spread {
            n: 0,
            p50: declared,
            low: declared,
            high: declared,
            kind: SpreadKind::Single,
        }),
        human_wait_p50_ms: None,
    }
}

/// The runs whose binding of `node` is the target's: the same profile
/// digest (both absent for a node without a profile). Returns them and how
/// many comparable runs bound it otherwise.
pub(super) fn matching<'a>(
    target: &EstimateTarget,
    node: &str,
    runs: &[(
        &'a RunSample,
        &'a BTreeMap<String, crate::manifest::NodeDigests>,
    )],
) -> (Vec<&'a RunSample>, u32) {
    let want = target.node_profiles.get(node);
    let mut out = Vec::new();
    let mut mismatch = 0;
    for (sample, digests) in runs {
        if digests.get(node).map(|d| &d.profile) == want {
            out.push(*sample);
        } else {
            mismatch += 1;
        }
    }
    (out, mismatch)
}

#[cfg(test)]
mod tests {
    use super::super::sample::{ListVisit, VisitSample};
    use super::*;
    use apb_core::schema::Playbook;

    fn playbook(nodes: &str) -> Playbook {
        Playbook::from_yaml(&format!(
            "schema: 2\nid: p\nname: p\nversion: 1.0.0\nnodes:\n  - {{ id: start, type: start }}\n{nodes}  - {{ id: done, type: finish, outcome: success }}\nedges:\n  - {{ from: start, to: done }}\n"
        ))
        .unwrap()
    }

    fn visit(tokens: u64) -> VisitSample {
        VisitSample {
            tokens: Some(tokens),
            active_ms: 1000,
            attempts: 1,
            status: "succeeded".into(),
            ..VisitSample::default()
        }
    }

    fn target(p: Playbook) -> EstimateTarget {
        EstimateTarget {
            version: p.version.clone(),
            playbook: p,
            execution: "cli".into(),
            node_profiles: BTreeMap::new(),
            exclude_run: None,
        }
    }

    #[test]
    fn inline_list_scales_per_item_by_item_count() {
        let p = playbook(
            "  - id: work\n    type: agent_task\n    prompt: x\n    profile: m\n    list:\n      items: { inline: [{ id: a, keep: yes }, { id: b, keep: yes }, { id: c, keep: no }], where: { field: keep, equals: yes } }\n",
        );
        let runs: Vec<RunSample> = [10u64, 20, 30]
            .iter()
            .map(|t| RunSample {
                lists: BTreeMap::from([(
                    "work".to_string(),
                    vec![ListVisit {
                        total: 7,
                        batch_size: 1,
                        batch_ms: vec![5],
                        items: vec![AttemptSpend {
                            tokens: Some(*t),
                            cost: None,
                        }],
                    }],
                )]),
                ..RunSample::default()
            })
            .collect();
        let refs: Vec<&RunSample> = runs.iter().collect();
        let n = node_estimate(p.node("work").unwrap(), &refs);
        let l = n.list.unwrap();
        assert_eq!(l.items_basis, ItemsBasis::Inline);
        let (tokens, _) = run_spend(std::slice::from_ref(&n));
        let per_item = l.per_item.tokens.unwrap();
        assert_eq!(l.items, Some(2));
        assert_eq!(per_item.p50, 20);
        assert_eq!(tokens.unwrap().spread.p50, 40);
        assert!(!unestimated(&n));
    }

    #[test]
    fn loop_node_run_total_covers_traversals() {
        let p = playbook("  - { id: fix, type: agent_task, prompt: x, profile: m }\n");
        let runs: Vec<RunSample> = [
            vec![visit(100), visit(100), visit(100)],
            vec![visit(100)],
            vec![visit(100), visit(100)],
        ]
        .into_iter()
        .map(|v| RunSample {
            visits: BTreeMap::from([("fix".to_string(), v)]),
            ..RunSample::default()
        })
        .collect();
        let refs: Vec<&RunSample> = runs.iter().collect();
        let n = node_estimate(p.node("fix").unwrap(), &refs);
        // The gate weighs one traversal; the run total covers all of them.
        assert_eq!(n.per_visit.tokens.unwrap().p50, 100);
        assert_eq!(n.per_run.tokens.unwrap().p50, 200);
        assert_eq!(n.visits_p50, 2.0);
        assert_eq!(run_spend(&[n]).0.unwrap().spread.p50, 200);
    }

    #[test]
    fn no_history_falls_back_to_declared_expected_duration() {
        let p = playbook(
            "  - { id: a, type: agent_task, prompt: x, profile: m, expected_duration: 2m }\n  - { id: b, type: agent_task, prompt: x, profile: m, expected_duration: 30s }\n",
        );
        let t = time(&target(p), &[]);
        assert_eq!(t.basis, EstimateBasis::Declared);
        assert_eq!(t.spread.unwrap().p50, 150_000);
        let ok = RunSample {
            active_ms: Some(9_000),
            human_wait_ms: 1_000,
            ..RunSample::default()
        };
        let t = time(&target(playbook("")), &[&ok]);
        assert_eq!(t.basis, EstimateBasis::History);
        assert_eq!(
            (t.spread.unwrap().p50, t.human_wait_p50_ms),
            (9_000, Some(1_000))
        );
        assert_eq!(time(&target(playbook("")), &[]).basis, EstimateBasis::None);
    }

    #[test]
    fn quantile_interpolates_and_spread_switches_iqr_minmax_single() {
        assert_eq!(quantile(&[1.0, 2.0, 3.0, 4.0], 0.5), Some(2.5));
        assert_eq!(quantile(&[10.0, 20.0], 0.25), Some(12.5));
        assert_eq!(quantile(&[], 0.5), None);
        let iqr = Spread::of(vec![40.0, 10.0, 30.0, 20.0]).unwrap();
        assert_eq!(iqr.kind, SpreadKind::Iqr);
        assert_eq!((iqr.low, iqr.p50, iqr.high), (17.5, 25.0, 32.5));
        let mm = Spread::of(vec![5.0, 1.0, 3.0]).unwrap();
        assert_eq!(
            (mm.kind, mm.low, mm.p50, mm.high),
            (SpreadKind::MinMax, 1.0, 3.0, 5.0)
        );
        let one = Spread::of(vec![7.0]).unwrap();
        assert_eq!(
            (one.kind, one.n, one.low, one.high),
            (SpreadKind::Single, 1, 7.0, 7.0)
        );
        assert!(Spread::of(Vec::new()).is_none());
    }
}
