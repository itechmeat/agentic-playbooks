//! Graph algorithms over a playbook's node graph, shared by the validator and
//! the engine: successor adjacency, forward reachability, and the strongly
//! connected components an iterative Tarjan pass produces.
//!
//! The validator uses the components to bound cycles (V11) and reachability for
//! its happens-before rules; the engine uses the same components to tell an
//! acyclic fan-in (which becomes an implicit join barrier) from a cycle merge
//! point, and reachability to decide whether an input branch can still arrive.
//! One implementation, so both answers always agree.
//!
//! Edges are followed unconditionally: this is the structural graph, not a
//! run-state-dependent routing decision.

use std::collections::{BTreeSet, HashMap, HashSet, VecDeque};

use crate::schema::Playbook;

/// Successor adjacency of the node graph. Every declared node gets an entry,
/// including one with no outgoing edge; an edge whose endpoint is not a
/// declared node still shows up as a target (structural validation of edge
/// endpoints is V06's job, not this function's).
pub fn adjacency(playbook: &Playbook) -> HashMap<&str, Vec<&str>> {
    let mut adj: HashMap<&str, Vec<&str>> = HashMap::new();
    for n in &playbook.nodes {
        adj.entry(n.id.as_str()).or_default();
    }
    for e in &playbook.edges {
        adj.entry(e.from.as_str()).or_default().push(e.to.as_str());
    }
    adj
}

/// Nodes forward-reachable from `from` over `adj`, `from` itself included.
/// Takes a prebuilt adjacency so a caller asking the question once per node
/// pays for the adjacency only once.
pub fn reachable_from<'a>(adj: &HashMap<&'a str, Vec<&'a str>>, from: &'a str) -> HashSet<&'a str> {
    let mut seen = HashSet::new();
    let mut q = VecDeque::from([from]);
    while let Some(id) = q.pop_front() {
        if seen.insert(id) {
            for next in adj.get(id).into_iter().flatten() {
                q.push_back(next);
            }
        }
    }
    seen
}

/// Whether a fan-in at `node` is an ACYCLIC fan-in: at least two branches, and
/// no source lies in `node`'s own forward-reachable set (a source downstream of
/// `node` is what a cycle merge point or a self-loop looks like, and
/// `reachable_from` seeds with `node` itself, so a self-loop source is excluded
/// here with no special case).
///
/// This backs the IMPLICIT half of both `apb_engine::parallel::join_kind` and
/// `apb_core::validate::graph::waits_for_all_inputs`, so the two can never drift.
/// It answers only the acyclic-fan-in question: a caller's explicit-`join`
/// priority (an author's `join` on any incoming edge always wins, including
/// inside a cycle) is layered on top by the caller, before this is ever reached.
/// This function is not told about `join` at all.
///
/// Takes an adjacency map and bare source ids rather than a `Playbook`, because
/// the validator's call site holds only a prebuilt adjacency map and a slice of
/// incoming edges, and neither caller needs anything about an edge except its
/// `from`.
pub fn is_acyclic_fan_in<'a>(
    adj: &HashMap<&'a str, Vec<&'a str>>,
    node: &'a str,
    sources: &[&'a str],
) -> bool {
    if sources.len() < 2 {
        return false;
    }
    let downstream = reachable_from(adj, node);
    sources.iter().all(|s| !downstream.contains(s))
}

/// Nodes forward-reachable from any of `from`, the seeds themselves included
/// (a seed that is not a declared node contributes only itself).
pub fn reachable(playbook: &Playbook, from: &[&str]) -> BTreeSet<String> {
    let adj = adjacency(playbook);
    let mut seen: BTreeSet<String> = from.iter().map(|s| (*s).to_string()).collect();
    let seeds: Vec<&str> = adj.keys().copied().filter(|k| from.contains(k)).collect();
    for seed in seeds {
        seen.extend(reachable_from(&adj, seed).into_iter().map(str::to_string));
    }
    seen
}

/// The strongly connected components of the node graph (iterative Tarjan).
/// Every declared node appears in exactly one component; a component of one
/// node is cyclic only when that node carries a self-loop edge.
pub fn sccs(playbook: &Playbook) -> Vec<Vec<String>> {
    let ids: Vec<&str> = playbook.nodes.iter().map(|n| n.id.as_str()).collect();
    let adj = adjacency(playbook);
    let index_of: HashMap<&str, usize> = ids.iter().enumerate().map(|(i, s)| (*s, i)).collect();
    let n = ids.len();
    let mut index = vec![usize::MAX; n];
    let mut low = vec![0usize; n];
    let mut on_stack = vec![false; n];
    let mut stack: Vec<usize> = Vec::new();
    let mut counter = 0usize;
    let mut out: Vec<Vec<String>> = Vec::new();

    for root in 0..n {
        if index[root] != usize::MAX {
            continue;
        }
        // An explicit call stack of (node, next neighbor index) frames, so a
        // deep graph cannot blow the real stack.
        let mut call: Vec<(usize, usize)> = vec![(root, 0)];
        while let Some(&(v, ei)) = call.last() {
            if ei == 0 {
                index[v] = counter;
                low[v] = counter;
                counter += 1;
                stack.push(v);
                on_stack[v] = true;
            }
            let neigh: Vec<usize> = adj
                .get(ids[v])
                .into_iter()
                .flatten()
                .filter_map(|t| index_of.get(t).copied())
                .collect();
            if ei < neigh.len() {
                call.last_mut().expect("frame exists").1 += 1;
                let w = neigh[ei];
                if index[w] == usize::MAX {
                    call.push((w, 0));
                } else if on_stack[w] {
                    low[v] = low[v].min(index[w]);
                }
            } else {
                if low[v] == index[v] {
                    let mut comp = Vec::new();
                    while let Some(w) = stack.pop() {
                        on_stack[w] = false;
                        comp.push(ids[w].to_string());
                        if w == v {
                            break;
                        }
                    }
                    out.push(comp);
                }
                call.pop();
                if let Some(&(parent, _)) = call.last() {
                    low[parent] = low[parent].min(low[v]);
                }
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const DIAMOND_WITH_LOOP: &str = r#"
schema: 1
id: g
name: G
version: 1.0.0
nodes:
  - { id: start, type: start }
  - { id: a, type: prompt, prompt: "a" }
  - { id: b, type: prompt, prompt: "b" }
  - { id: j, type: prompt, prompt: "j" }
  - { id: check, type: condition, max_loops: 2 }
  - { id: done, type: finish, outcome: success }
edges:
  - { from: start, to: a }
  - { from: start, to: b }
  - { from: a, to: j }
  - { from: b, to: j }
  - { from: j, to: check }
  - { from: check, to: j, condition: { type: node_status, node: check, equals: failure } }
  - { from: check, to: done, condition: { type: node_status, node: check, equals: success } }
"#;

    /// A fan-in whose merge point also carries a self edge: `tick` has three
    /// inputs, one of which is itself.
    const SELF_LOOP: &str = r#"
schema: 1
id: sl
name: SL
version: 1.0.0
nodes:
  - { id: start, type: start }
  - { id: a, type: prompt, prompt: "a" }
  - { id: tick, type: condition, max_loops: 2 }
  - { id: done, type: finish, outcome: success }
edges:
  - { from: start, to: tick }
  - { from: start, to: a }
  - { from: a, to: tick }
  - { from: tick, to: tick, condition: { type: node_status, node: tick, equals: failure } }
  - { from: tick, to: done, condition: { type: node_status, node: tick, equals: success } }
"#;

    fn playbook() -> Playbook {
        Playbook::from_yaml(DIAMOND_WITH_LOOP).unwrap()
    }

    fn set(ids: &[&str]) -> BTreeSet<String> {
        ids.iter().map(|s| (*s).to_string()).collect()
    }

    #[test]
    fn is_acyclic_fan_in_true_for_a_plain_diamond() {
        let pb = playbook();
        let adj = adjacency(&pb);
        assert!(
            is_acyclic_fan_in(&adj, "j", &["a", "b"]),
            "a two-branch diamond merge point is an acyclic fan-in"
        );
    }

    #[test]
    fn is_acyclic_fan_in_false_for_a_cycle_merge_point() {
        let pb = playbook();
        let adj = adjacency(&pb);
        // `check` is downstream of `j` (j -> check), so the fan-in at `j` that
        // includes the back edge keeps first-arrival semantics.
        assert!(
            !is_acyclic_fan_in(&adj, "j", &["a", "b", "check"]),
            "a source reachable from the node itself makes the fan-in cyclic"
        );
    }

    #[test]
    fn is_acyclic_fan_in_false_for_a_self_loop() {
        let pb = Playbook::from_yaml(SELF_LOOP).unwrap();
        let adj = adjacency(&pb);
        // No special case in the predicate: `reachable_from` seeds with `tick`
        // itself, so a self-loop source is downstream by construction.
        assert!(
            !is_acyclic_fan_in(&adj, "tick", &["start", "a", "tick"]),
            "a self-loop source must not read as an acyclic fan-in"
        );
    }

    #[test]
    fn is_acyclic_fan_in_false_below_minimum_fan_in() {
        let pb = playbook();
        let adj = adjacency(&pb);
        assert!(
            !is_acyclic_fan_in(&adj, "j", &["a"]),
            "one source is not a fan-in"
        );
        assert!(
            !is_acyclic_fan_in(&adj, "j", &[]),
            "no source is not a fan-in"
        );
    }

    #[test]
    fn every_node_lands_in_exactly_one_component() {
        let comps = sccs(&playbook());
        let mut all: Vec<String> = comps.into_iter().flatten().collect();
        all.sort();
        assert_eq!(all, vec!["a", "b", "check", "done", "j", "start"]);
    }

    #[test]
    fn the_cycle_forms_one_component_and_the_rest_are_trivial() {
        let comps = sccs(&playbook());
        let of = |node: &str| -> BTreeSet<String> {
            comps
                .iter()
                .find(|c| c.iter().any(|id| id == node))
                .cloned()
                .map(|c| c.into_iter().collect())
                .unwrap_or_default()
        };
        assert_eq!(of("j"), set(&["j", "check"]));
        assert_eq!(of("a"), set(&["a"]));
        assert_eq!(of("missing"), set(&[]));
    }

    #[test]
    fn reachable_walks_forward_from_every_seed() {
        assert_eq!(
            reachable(&playbook(), &["a"]),
            set(&["a", "j", "check", "done"])
        );
        assert_eq!(
            reachable(&playbook(), &["a", "b"]),
            set(&["a", "b", "j", "check", "done"])
        );
        assert_eq!(reachable(&playbook(), &["done"]), set(&["done"]));
        // An unknown seed contributes itself and nothing else.
        assert_eq!(reachable(&playbook(), &["nope"]), set(&["nope"]));
    }
}
