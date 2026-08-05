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

/// The strongly connected component `node` belongs to, `node` included. An
/// unknown node yields an empty set. A node whose component is just itself is
/// on no cycle unless it carries a self-loop edge.
pub fn component_of(playbook: &Playbook, node: &str) -> BTreeSet<String> {
    sccs(playbook)
        .into_iter()
        .find(|comp| comp.iter().any(|id| id == node))
        .map(|comp| comp.into_iter().collect())
        .unwrap_or_default()
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

    fn playbook() -> Playbook {
        Playbook::from_yaml(DIAMOND_WITH_LOOP).unwrap()
    }

    fn set(ids: &[&str]) -> BTreeSet<String> {
        ids.iter().map(|s| (*s).to_string()).collect()
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
        assert_eq!(component_of(&playbook(), "j"), set(&["j", "check"]));
        assert_eq!(component_of(&playbook(), "a"), set(&["a"]));
        assert_eq!(component_of(&playbook(), "missing"), set(&[]));
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
