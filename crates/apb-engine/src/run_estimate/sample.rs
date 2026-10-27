//! Per-run, per-node samples of what a run spent and how long its parts
//! took, read from its journal. The estimator composes them into run
//! figures, and `apb stats` groups them by executor digest.
//!
//! Every duration is active time: the time the run spent paused is left
//! out ([`crate::pause::paused_intervals`]).

use std::collections::BTreeMap;
use std::path::Path;

use crate::event::{Event, EventPayload};

/// How deep a sub-playbook's own sub-playbooks are followed for spend.
const CHILD_DEPTH: usize = apb_core::schema::MAX_SUBPLAYBOOK_DEPTH;

/// One attempt's spend: billable tokens and reported cost, each `None` when
/// the attempt did not report it.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct AttemptSpend {
    pub tokens: Option<u64>,
    pub cost: Option<f64>,
}

impl AttemptSpend {
    /// The spend an `attempt_finished` (or any usage block) reports.
    pub fn of(usage: Option<&apb_core::agent_output::AgentUsage>) -> Self {
        AttemptSpend {
            tokens: usage.map(crate::run_view::billable_tokens),
            cost: usage.and_then(|u| u.cost_usd),
        }
    }
}

/// The node a finished attempt counts toward and its spend: the attempt's
/// own node, or the list node of an item check site. `None` for every other
/// event.
pub fn attempt_spend(p: &EventPayload) -> Option<(&str, AttemptSpend)> {
    let EventPayload::AttemptFinished { node, usage, .. } = p else {
        return None;
    };
    let owner = apb_core::list::check_site_node(node).unwrap_or(node);
    Some((owner, AttemptSpend::of(usage.as_ref())))
}

/// Every finished attempt of sub-playbook run `run_id` under `runs_dir`, and
/// of its own sub-playbook runs, in journal order. A child whose journal
/// does not read contributes nothing.
pub fn child_attempts(runs_dir: &Path, run_id: &str) -> Vec<AttemptSpend> {
    let mut out = Vec::new();
    collect_child(runs_dir, run_id, 0, &mut out);
    out
}

fn collect_child(runs_dir: &Path, run_id: &str, depth: usize, out: &mut Vec<AttemptSpend>) {
    if depth > CHILD_DEPTH || !apb_core::registry::is_safe_segment(run_id) {
        return;
    }
    let Ok(events) = crate::event::read_all_lossy_tail(&runs_dir.join(run_id)) else {
        return;
    };
    for e in &events {
        match &e.payload {
            EventPayload::AttemptFinished { usage, .. } => {
                out.push(AttemptSpend::of(usage.as_ref()));
            }
            EventPayload::ChildRunStarted { run_id, .. } => {
                collect_child(runs_dir, run_id, depth + 1, out);
            }
            _ => {}
        }
    }
}

/// One visit of a node: its `node_started` up to its `node_finished`.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct VisitSample {
    /// Billable tokens, only when every finished attempt in the visit
    /// reported usage.
    pub tokens: Option<u64>,
    /// Reported cost, only when every finished attempt reported one.
    pub cost: Option<f64>,
    pub active_ms: u64,
    pub attempts: u32,
    pub attempts_without_usage: u32,
    pub attempts_without_cost: u32,
    /// A profile or execution fallback happened inside the visit.
    pub fell_back: bool,
    /// `succeeded`, `failed` and so on, as journaled.
    pub status: String,
}

/// What one list visit's items did.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ListVisit {
    /// `list_started.total`.
    pub total: u32,
    pub batch_size: u32,
    /// Active milliseconds of each batch: one `list_batch_started` to the
    /// next, or to the visit's end.
    pub batch_ms: Vec<u64>,
    /// Each item's spend over every attempt of it in the visit, rounds and
    /// item checks included.
    pub items: Vec<AttemptSpend>,
}

/// One run, as the estimator reads it.
#[derive(Debug, Clone, Default)]
pub struct RunSample {
    pub run_id: String,
    /// `succeeded`, `failed` or `aborted`; `None` while the run has not
    /// ended.
    pub outcome: Option<&'static str>,
    /// Active milliseconds from start to the terminal event.
    pub active_ms: Option<u64>,
    /// Gate and question waits, summed.
    pub human_wait_ms: u64,
    /// Every finished visit, per node.
    pub visits: BTreeMap<String, Vec<VisitSample>>,
    /// Every finished attempt's spend, per node (a sub-playbook node's
    /// child attempts under it).
    pub attempts: BTreeMap<String, Vec<AttemptSpend>>,
    /// Every finished list visit, per node.
    pub lists: BTreeMap<String, Vec<ListVisit>>,
}

impl RunSample {
    /// The visit total of `node` in this run: the sum over its visits, 0
    /// when it did not run, `None` when a visit's spend is not known.
    pub fn per_run<T: Copy + std::iter::Sum<T> + Default>(
        &self,
        node: &str,
        pick: impl Fn(&VisitSample) -> Option<T>,
    ) -> Option<T> {
        match self.visits.get(node) {
            None => Some(T::default()),
            Some(v) => v.iter().map(pick).sum(),
        }
    }
}

/// Accumulates one open visit.
#[derive(Default)]
struct OpenVisit {
    start: u128,
    attempts: Vec<AttemptSpend>,
    fell_back: bool,
    list: Option<OpenList>,
}

#[derive(Default)]
struct OpenList {
    total: u32,
    batch_size: u32,
    batch_start: Option<u128>,
    batch_ms: Vec<u64>,
    items: BTreeMap<String, Vec<AttemptSpend>>,
}

impl OpenVisit {
    fn close(
        self,
        status: &str,
        end: u128,
        paused: &[(u128, u128)],
    ) -> (VisitSample, Vec<AttemptSpend>, Option<ListVisit>) {
        let sample = visit_of(
            &self.attempts,
            status,
            self.fell_back,
            active(paused, self.start, end),
        );
        let list = self.list.map(|mut l| {
            if let Some(b) = l.batch_start.take() {
                l.batch_ms.push(active(paused, b, end));
            }
            ListVisit {
                total: l.total,
                batch_size: l.batch_size,
                batch_ms: l.batch_ms,
                items: l.items.into_values().map(|a| sum_spend(&a)).collect(),
            }
        });
        (sample, self.attempts, list)
    }
}

fn active(paused: &[(u128, u128)], from: u128, to: u128) -> u64 {
    crate::run_stats::spans::active_ms(paused, from, to)
}

/// The sum of attempts' spend; a dimension any attempt left out is `None`,
/// and so is every dimension of no attempt at all (a cache hit, a missing
/// child journal, a visit cut short before its first attempt): an unknown,
/// not a zero sample.
pub fn sum_spend(attempts: &[AttemptSpend]) -> AttemptSpend {
    if attempts.is_empty() {
        return AttemptSpend::default();
    }
    AttemptSpend {
        tokens: attempts.iter().map(|a| a.tokens).sum(),
        cost: attempts.iter().map(|a| a.cost).sum(),
    }
}

fn visit_of(
    attempts: &[AttemptSpend],
    status: &str,
    fell_back: bool,
    active_ms: u64,
) -> VisitSample {
    let total = sum_spend(attempts);
    let count = |f: fn(&AttemptSpend) -> bool| attempts.iter().filter(|a| f(a)).count() as u32;
    VisitSample {
        tokens: total.tokens,
        cost: total.cost,
        active_ms,
        attempts: attempts.len() as u32,
        attempts_without_usage: count(|a| a.tokens.is_none()),
        attempts_without_cost: count(|a| a.cost.is_none()),
        fell_back,
        status: status.to_string(),
    }
}

impl RunSample {
    /// Reads run `run_id` from its journal. `runs_dir` holds its
    /// sub-playbook runs; without it a `playbook` node's visits carry no
    /// child spend.
    pub fn of(run_id: &str, events: &[Event], runs_dir: Option<&Path>) -> Self {
        let last = events.last().map(|e| e.ts).unwrap_or_default();
        let paused = crate::pause::paused_intervals(events, last);
        let start = events.iter().find_map(|e| match e.payload {
            EventPayload::RunStarted { .. } => Some(e.ts),
            _ => None,
        });
        let mut out = RunSample {
            run_id: run_id.to_string(),
            ..RunSample::default()
        };
        let mut open: BTreeMap<String, OpenVisit> = BTreeMap::new();
        for e in events {
            match &e.payload {
                EventPayload::NodeStarted { node, .. } => {
                    open.entry(node.clone()).or_insert_with(|| OpenVisit {
                        start: e.ts,
                        ..OpenVisit::default()
                    });
                }
                EventPayload::AttemptFinished { item, .. } => {
                    let Some((node, spend)) = attempt_spend(&e.payload) else {
                        continue;
                    };
                    if let Some(v) = open.get_mut(node) {
                        v.attempts.push(spend);
                        if let (Some(l), Some(id)) = (v.list.as_mut(), item) {
                            l.items.entry(id.clone()).or_default().push(spend);
                        }
                    }
                }
                EventPayload::ChildRunStarted { node_id, run_id } => {
                    if let (Some(v), Some(dir)) = (open.get_mut(node_id.as_str()), runs_dir) {
                        v.attempts.extend(child_attempts(dir, run_id));
                    }
                }
                EventPayload::FallbackTriggered { node, .. }
                | EventPayload::ExecutionFallback { node, .. } => {
                    if let Some(v) = open.get_mut(node.as_str()) {
                        v.fell_back = true;
                    }
                }
                EventPayload::ListStarted {
                    node,
                    total,
                    batch_size,
                    ..
                } => {
                    if let Some(v) = open.get_mut(node.as_str()) {
                        v.list = Some(OpenList {
                            total: *total,
                            batch_size: *batch_size,
                            ..OpenList::default()
                        });
                    }
                }
                EventPayload::ListBatchStarted { node, .. } => {
                    if let Some(l) = open.get_mut(node.as_str()).and_then(|v| v.list.as_mut())
                        && let Some(b) = l.batch_start.replace(e.ts)
                    {
                        l.batch_ms.push(active(&paused, b, e.ts));
                    }
                }
                EventPayload::NodeFinished { node, status, .. } => {
                    if let Some(v) = open.remove(node.as_str()) {
                        let (visit, attempts, list) = v.close(status, e.ts, &paused);
                        out.visits.entry(node.clone()).or_default().push(visit);
                        out.attempts
                            .entry(node.clone())
                            .or_default()
                            .extend(attempts);
                        if let Some(l) = list {
                            out.lists.entry(node.clone()).or_default().push(l);
                        }
                    }
                }
                EventPayload::RunFinished { outcome } => {
                    out.outcome = Some(match outcome.as_str() {
                        "succeeded" => "succeeded",
                        "aborted" => "aborted",
                        _ => "failed",
                    });
                    out.active_ms = start.map(|s| active(&paused, s, e.ts));
                }
                EventPayload::RunAborted { .. } => {
                    out.outcome = Some("aborted");
                    out.active_ms = start.map(|s| active(&paused, s, e.ts));
                }
                _ => {}
            }
        }
        let (gate, question) = crate::run_stats::spans::human_waits(events);
        out.human_wait_ms = gate.iter().chain(&question).sum();
        out
    }

    /// Finished attempts over the run, and those without usage and without
    /// a cost.
    pub fn attempt_counts(&self) -> (u32, u32, u32) {
        let all = self.attempts.values().flatten();
        let (mut n, mut no_usage, mut no_cost) = (0, 0, 0);
        for a in all {
            n += 1;
            no_usage += u32::from(a.tokens.is_none());
            no_cost += u32::from(a.cost.is_none());
        }
        (n, no_usage, no_cost)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn run(body: &[(u128, serde_json::Value)]) -> Vec<Event> {
        let mut events = vec![Event {
            seq: 0,
            ts: 0,
            payload: serde_json::from_value(
                json!({"type": "run_started", "playbook": "p", "version": "1.0.0"}),
            )
            .unwrap(),
        }];
        for (i, (ts, v)) in body.iter().enumerate() {
            events.push(Event {
                seq: i as u64 + 1,
                ts: *ts,
                payload: serde_json::from_value(v.clone()).unwrap(),
            });
        }
        events
    }

    fn started(node: &str) -> serde_json::Value {
        json!({"type": "node_started", "node": node, "attempt": 1})
    }
    fn finished(node: &str) -> serde_json::Value {
        json!({"type": "node_finished", "node": node, "status": "succeeded", "attempt": 1, "output": ""})
    }
    fn attempt(node: &str, tokens: Option<(u64, u64)>, item: Option<&str>) -> serde_json::Value {
        let mut v =
            json!({"type": "attempt_finished", "node": node, "attempt": 1, "status": "succeeded"});
        if let Some((i, o)) = tokens {
            v["usage"] = json!({"input_tokens": i, "output_tokens": o, "cache_read_tokens": 5000, "source": "reported"});
        }
        if let Some(id) = item {
            v["item"] = json!(id);
        }
        v
    }

    #[test]
    fn a_visit_with_one_attempt_without_usage_is_not_a_token_sample_but_is_counted() {
        let events = run(&[
            (10, started("a")),
            (20, attempt("a", Some((100, 50)), None)),
            (30, attempt("a", None, None)),
            (40, finished("a")),
            (50, started("b")),
            (60, attempt("b", Some((7, 3)), None)),
            (70, finished("b")),
        ]);
        let s = RunSample::of("r", &events, None);
        let a = &s.visits["a"][0];
        assert_eq!(a.tokens, None);
        assert_eq!((a.attempts, a.attempts_without_usage), (2, 1));
        // Cache tokens are not billable.
        assert_eq!(s.visits["b"][0].tokens, Some(10));
        assert_eq!(s.attempt_counts(), (3, 1, 3));
        assert_eq!(s.per_run("b", |v| v.tokens), Some(10));
        assert_eq!(s.per_run("never", |v| v.tokens), Some(0));
        assert_eq!(s.per_run("a", |v| v.tokens), None);
    }

    #[test]
    fn list_item_sums_include_rounds_and_check_attempts() {
        let check = apb_core::list::check_site("fix");
        let events = run(&[
            (10, started("fix")),
            (
                11,
                json!({"type": "list_started", "node": "fix", "total": 2, "batch_size": 1}),
            ),
            (
                12,
                json!({"type": "list_batch_started", "node": "fix", "batch": 1, "items": ["x"]}),
            ),
            (20, attempt("fix", Some((10, 0)), Some("x"))),
            (21, attempt(&check, Some((5, 0)), Some("x"))),
            (22, attempt("fix", Some((10, 0)), Some("x"))),
            (
                30,
                json!({"type": "list_batch_started", "node": "fix", "batch": 2, "items": ["y"]}),
            ),
            (40, attempt("fix", Some((1, 1)), Some("y"))),
            (50, finished("fix")),
        ]);
        let s = RunSample::of("r", &events, None);
        let l = &s.lists["fix"][0];
        assert_eq!(l.total, 2);
        assert_eq!(l.batch_ms, vec![18, 20]);
        let mut items: Vec<_> = l.items.iter().map(|i| i.tokens).collect();
        items.sort();
        assert_eq!(items, vec![Some(2), Some(25)]);
        assert_eq!(s.visits["fix"][0].tokens, Some(27));
    }

    #[test]
    fn paused_time_is_left_out_of_durations() {
        let events = run(&[
            (100, started("a")),
            (
                200,
                json!({"type": "run_paused", "reason": "x", "by": "operator"}),
            ),
            (1_200, json!({"type": "run_resumed", "from_node": "a"})),
            (1_300, finished("a")),
            (
                1_400,
                json!({"type": "run_finished", "outcome": "succeeded"}),
            ),
        ]);
        let s = RunSample::of("r", &events, None);
        assert_eq!(s.visits["a"][0].active_ms, 200);
        assert_eq!(s.active_ms, Some(400));
        assert_eq!(s.outcome, Some("succeeded"));
    }

    #[test]
    fn playbook_node_spend_is_the_child_total() {
        let dir = tempfile::tempdir().unwrap();
        let child = dir.path().join("child-1");
        std::fs::create_dir_all(&child).unwrap();
        let line = |seq: u64, v: serde_json::Value| {
            let mut v = v;
            v["seq"] = json!(seq);
            v["ts"] = json!(seq);
            v.to_string()
        };
        let journal = [
            line(
                0,
                json!({"type": "run_started", "playbook": "c", "version": "1.0.0"}),
            ),
            line(1, attempt("inner", Some((40, 2)), None)),
            line(2, attempt("inner", Some((8, 0)), None)),
        ]
        .join("\n")
            + "\n";
        std::fs::write(child.join("events.jsonl"), journal).unwrap();
        let events = run(&[
            (10, started("sub")),
            (
                11,
                json!({"type": "child_run_started", "node_id": "sub", "run_id": "child-1"}),
            ),
            (20, finished("sub")),
        ]);
        let s = RunSample::of("r", &events, Some(dir.path()));
        assert_eq!(s.visits["sub"][0].tokens, Some(50));
        assert_eq!(s.visits["sub"][0].attempts, 2);
    }
}
