//! What changed between two playbook definitions: nodes and edges
//! compared structurally, and the YAML line by line. Shared by the diff of
//! two stored versions and by the preview of an import.

use std::collections::{HashMap, HashSet};

use serde::Serialize;

use super::{VersioningError, schema_err};
use crate::edge_address::{CheckOrder, EdgeAddress, edge_addresses};
use crate::schema::{Edge, Playbook};

/// Structural and textual diff of two versions of the same playbook.
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct VersionDiff {
    pub nodes_added: Vec<String>,
    pub nodes_removed: Vec<String>,
    pub nodes_changed: Vec<String>,
    /// `from->to` pairs with an edge in the new definition only.
    pub edges_added: Vec<String>,
    /// `from->to` pairs with an edge in the old definition only.
    pub edges_removed: Vec<String>,
    /// One row per edge of either definition ([`edge_correspondence`]):
    /// first every edge of the new definition in declaration order, with the
    /// old edge it corresponds to if any, then every old edge nothing
    /// corresponds to, in the old declaration order.
    pub edges: Vec<EdgeCorrespondence>,
    /// The one-of sources whose corresponding edges are checked in another
    /// order, sorted by node id.
    pub edge_order_changed: Vec<EdgeOrderChange>,
    pub yaml_diff: String,
}

/// One edge of either definition and its counterpart in the other.
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct EdgeCorrespondence {
    /// Its address in the old definition; `None` for an added edge.
    pub before: Option<EdgeAddress>,
    /// Its address in the new definition; `None` for a removed edge.
    pub after: Option<EdgeAddress>,
    /// What differs between the two when both exist: `id`, `from`, `to`,
    /// `condition`, `fallback`, `join`, `max_traversals`, `require`, and
    /// `address` when the rendered addresses differ. Empty otherwise.
    pub changed: Vec<String>,
}

/// A one-of source (one with a conditional or fallback outgoing edge) whose
/// corresponding conditional edges are checked in another order. Only those
/// have a check order: a fallback is taken last wherever it is declared, and
/// unconditional edges are all taken together.
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct EdgeOrderChange {
    pub node: String,
    /// The corresponding conditional edges in the old check order, by old
    /// address.
    pub before: Vec<EdgeAddress>,
    /// The same edges in the new check order, still by old address, so the
    /// two lists read as one set reordered.
    pub after: Vec<EdgeAddress>,
}

/// The attributes an edge correspondence compares, beyond its id.
fn same_attributes(a: &Edge, b: &Edge) -> bool {
    a.condition == b.condition
        && a.fallback == b.fallback
        && a.join == b.join
        && a.max_traversals == b.max_traversals
        && a.require == b.require
}

/// One pass of [`edge_correspondence`]: whether an old and a new edge match.
type EdgeMatch<'a> = dyn Fn(&Edge, &Edge) -> bool + 'a;

/// The pairs `(old index, new index)` of corresponding edges: an authored
/// id matches the same id first; then, among the edges left with the same
/// ends, edges with identical attributes and the same id (or none) match in
/// declaration order, and the rest pair up in declaration order. Whatever is left was added or
/// removed.
pub fn edge_correspondence(before: &[Edge], after: &[Edge]) -> Vec<(usize, usize)> {
    let mut taken_b = vec![false; before.len()];
    let mut taken_a = vec![false; after.len()];
    let mut pairs = Vec::new();
    let same_id = |b: &Edge, a: &Edge| a.id.is_some() && b.id == a.id;
    let same_ends = |b: &Edge, a: &Edge| b.from == a.from && b.to == a.to;
    let same_all = |b: &Edge, a: &Edge| same_ends(b, a) && b.id == a.id && same_attributes(b, a);
    let passes: [&EdgeMatch; 3] = [&same_id, &same_all, &same_ends];
    for matches in passes {
        for (ai, a) in after.iter().enumerate() {
            if taken_a[ai] {
                continue;
            }
            let found = before
                .iter()
                .enumerate()
                .find(|(bi, b)| !taken_b[*bi] && matches(b, a))
                .map(|(bi, _)| bi);
            if let Some(bi) = found {
                taken_b[bi] = true;
                taken_a[ai] = true;
                pairs.push((bi, ai));
            }
        }
    }
    pairs.sort_unstable_by_key(|&(_, ai)| ai);
    pairs
}

/// The attribute names that differ between two corresponding edges.
fn changed_attributes(
    b: &Edge,
    a: &Edge,
    b_addr: &EdgeAddress,
    a_addr: &EdgeAddress,
) -> Vec<String> {
    let mut out = Vec::new();
    let mut note = |differs: bool, name: &str| {
        if differs {
            out.push(name.to_string());
        }
    };
    note(b.id != a.id, "id");
    note(b.from != a.from, "from");
    note(b.to != a.to, "to");
    note(b.condition != a.condition, "condition");
    note(b.fallback != a.fallback, "fallback");
    note(b.join != a.join, "join");
    note(b.max_traversals != a.max_traversals, "max_traversals");
    note(b.require != a.require, "require");
    note(b_addr.render() != a_addr.render(), "address");
    out
}

/// The edge rows and the order changes of two definitions.
fn edge_rows(
    before: &Playbook,
    after: &Playbook,
) -> (Vec<EdgeCorrespondence>, Vec<EdgeOrderChange>) {
    let b_addr = edge_addresses(before);
    let a_addr = edge_addresses(after);
    let pairs = edge_correspondence(&before.edges, &after.edges);
    let of_after: HashMap<usize, usize> = pairs.iter().map(|&(bi, ai)| (ai, bi)).collect();
    let matched_before: HashSet<usize> = pairs.iter().map(|&(bi, _)| bi).collect();
    let mut rows = Vec::new();
    for (ai, a) in after.edges.iter().enumerate() {
        let row = match of_after.get(&ai) {
            Some(&bi) => EdgeCorrespondence {
                before: Some(b_addr[bi].address.clone()),
                after: Some(a_addr[ai].address.clone()),
                changed: changed_attributes(
                    &before.edges[bi],
                    a,
                    &b_addr[bi].address,
                    &a_addr[ai].address,
                ),
            },
            None => EdgeCorrespondence {
                before: None,
                after: Some(a_addr[ai].address.clone()),
                changed: Vec::new(),
            },
        };
        rows.push(row);
    }
    for (bi, addr) in b_addr.iter().enumerate() {
        if !matched_before.contains(&bi) {
            rows.push(EdgeCorrespondence {
                before: Some(addr.address.clone()),
                after: None,
                changed: Vec::new(),
            });
        }
    }

    let one_of = |pb: &Playbook, node: &str| {
        pb.edges
            .iter()
            .any(|e| e.from == node && (e.condition.is_some() || e.fallback))
    };
    let mut sources: Vec<&str> = before
        .edges
        .iter()
        .chain(&after.edges)
        .map(|e| e.from.as_str())
        .filter(|n| one_of(before, n) || one_of(after, n))
        .collect();
    sources.sort_unstable();
    sources.dedup();
    let mut order = Vec::new();
    for node in sources {
        let ordered = |c: &CheckOrder| matches!(c, CheckOrder::Ordered(_));
        let mut mine: Vec<(usize, usize)> = pairs
            .iter()
            .copied()
            .filter(|&(bi, ai)| {
                before.edges[bi].from == node
                    && after.edges[ai].from == node
                    && ordered(&b_addr[bi].check)
                    && ordered(&a_addr[ai].check)
            })
            .collect();
        mine.sort_unstable_by_key(|&(bi, _)| bi);
        let by_before = mine.clone();
        mine.sort_unstable_by_key(|&(_, ai)| ai);
        if by_before == mine {
            continue;
        }
        order.push(EdgeOrderChange {
            node: node.to_string(),
            before: by_before
                .iter()
                .map(|&(bi, _)| b_addr[bi].address.clone())
                .collect(),
            after: mine
                .iter()
                .map(|&(bi, _)| b_addr[bi].address.clone())
                .collect(),
        });
    }
    (rows, order)
}

/// Compares two definitions given as YAML text: structurally (nodes and
/// edges) and line by line.
pub fn diff_definitions(from_yaml: &str, to_yaml: &str) -> Result<VersionDiff, VersioningError> {
    let from_playbook = Playbook::from_yaml(from_yaml).map_err(schema_err)?;
    let to_playbook = Playbook::from_yaml(to_yaml).map_err(schema_err)?;

    let from_nodes: HashMap<&str, &crate::schema::Node> = from_playbook
        .nodes
        .iter()
        .map(|n| (n.id.as_str(), n))
        .collect();
    let to_nodes: HashMap<&str, &crate::schema::Node> = to_playbook
        .nodes
        .iter()
        .map(|n| (n.id.as_str(), n))
        .collect();

    let mut nodes_added = Vec::new();
    let mut nodes_removed = Vec::new();
    let mut nodes_changed = Vec::new();

    for node_id in to_nodes.keys() {
        if !from_nodes.contains_key(node_id) {
            nodes_added.push((*node_id).to_string());
        }
    }
    for node_id in from_nodes.keys() {
        if !to_nodes.contains_key(node_id) {
            nodes_removed.push((*node_id).to_string());
        }
    }
    for (node_id, from_node) in &from_nodes {
        if let Some(to_node) = to_nodes.get(node_id) {
            let from_ser = serde_json::to_value(from_node)
                .map_err(|e| VersioningError::Schema(e.to_string()))?;
            let to_ser = serde_json::to_value(to_node)
                .map_err(|e| VersioningError::Schema(e.to_string()))?;
            if from_ser != to_ser {
                nodes_changed.push((*node_id).to_string());
            }
        }
    }
    nodes_added.sort();
    nodes_removed.sort();
    nodes_changed.sort();

    let from_edges: HashSet<String> = from_playbook
        .edges
        .iter()
        .map(|e| format!("{}->{}", e.from, e.to))
        .collect();
    let to_edges: HashSet<String> = to_playbook
        .edges
        .iter()
        .map(|e| format!("{}->{}", e.from, e.to))
        .collect();

    let mut edges_added: Vec<String> = to_edges.difference(&from_edges).cloned().collect();
    let mut edges_removed: Vec<String> = from_edges.difference(&to_edges).cloned().collect();
    edges_added.sort();
    edges_removed.sort();

    let yaml_diff = line_diff(from_yaml, to_yaml);
    let (edges, edge_order_changed) = edge_rows(&from_playbook, &to_playbook);

    Ok(VersionDiff {
        nodes_added,
        nodes_removed,
        nodes_changed,
        edges_added,
        edges_removed,
        edges,
        edge_order_changed,
        yaml_diff,
    })
}

/// The largest LCS table [`line_diff`] builds (lines removed times lines
/// added, after the common prefix and suffix): about 32 MB. A bundle from
/// elsewhere is untrusted input, so the diff of its YAML must stay bounded.
const MAX_DIFF_CELLS: usize = 4_000_000;

/// Line-by-line unified-like diff via LCS (without external crates). The
/// common prefix and suffix are kept as they are; a middle part too large
/// for the table ([`MAX_DIFF_CELLS`]) is shown as all of its old lines
/// removed and all of its new lines added.
pub fn line_diff(from: &str, to: &str) -> String {
    let a: Vec<&str> = from.lines().collect();
    let b: Vec<&str> = to.lines().collect();
    let prefix = a.iter().zip(&b).take_while(|(x, y)| x == y).count();
    let suffix = a[prefix..]
        .iter()
        .rev()
        .zip(b[prefix..].iter().rev())
        .take_while(|(x, y)| x == y)
        .count();
    let (mid_a, mid_b) = (&a[prefix..a.len() - suffix], &b[prefix..b.len() - suffix]);

    let mut out: Vec<String> = a[..prefix].iter().map(|l| format!(" {l}")).collect();
    if mid_a.len().saturating_mul(mid_b.len()) > MAX_DIFF_CELLS {
        out.extend(mid_a.iter().map(|l| format!("-{l}")));
        out.extend(mid_b.iter().map(|l| format!("+{l}")));
    } else {
        lcs_diff(mid_a, mid_b, &mut out);
    }
    out.extend(a[a.len() - suffix..].iter().map(|l| format!(" {l}")));
    out.join("\n")
}

fn lcs_diff(a: &[&str], b: &[&str], out: &mut Vec<String>) {
    let n = a.len();
    let m = b.len();

    let mut dp = vec![vec![0usize; m + 1]; n + 1];
    for i in (0..n).rev() {
        for j in (0..m).rev() {
            if a[i] == b[j] {
                dp[i][j] = dp[i + 1][j + 1] + 1;
            } else {
                dp[i][j] = dp[i + 1][j].max(dp[i][j + 1]);
            }
        }
    }

    let mut i = 0;
    let mut j = 0;
    while i < n && j < m {
        if a[i] == b[j] {
            out.push(format!(" {}", a[i]));
            i += 1;
            j += 1;
        } else if dp[i + 1][j] >= dp[i][j + 1] {
            out.push(format!("-{}", a[i]));
            i += 1;
        } else {
            out.push(format!("+{}", b[j]));
            j += 1;
        }
    }
    out.extend(a[i..].iter().map(|l| format!("-{l}")));
    out.extend(b[j..].iter().map(|l| format!("+{l}")));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_diff_keeps_the_common_lines_and_marks_the_changed_ones() {
        assert_eq!(line_diff("a\nb\nc", "a\nx\nc"), " a\n-b\n+x\n c");
        assert_eq!(line_diff("a", "a\nb"), " a\n+b");
    }

    fn yaml(edges: &str) -> String {
        format!(
            "schema: 2\nid: p\nname: p\nversion: 1.0.0\nnodes:\n  - {{ id: s, type: start }}\n  - {{ id: a, type: prompt, prompt: a }}\n  - {{ id: done, type: finish, outcome: success }}\n  - {{ id: bad, type: finish, outcome: failure }}\nedges:\n  - {{ from: s, to: a }}\n{edges}"
        )
    }

    const OK: &str = "condition: { type: node_status, node: a, equals: success }";
    const FAIL: &str = "condition: { type: node_status, node: a, equals: failure }";

    fn d(from: &str, to: &str, ordinal: u32) -> EdgeAddress {
        EdgeAddress::Derived {
            from: from.into(),
            to: to.into(),
            ordinal,
        }
    }

    fn row(
        before: Option<EdgeAddress>,
        after: Option<EdgeAddress>,
        changed: &[&str],
    ) -> EdgeCorrespondence {
        EdgeCorrespondence {
            before,
            after,
            changed: changed.iter().map(|c| c.to_string()).collect(),
        }
    }

    fn diff(before: &str, after: &str) -> VersionDiff {
        diff_definitions(&yaml(before), &yaml(after)).unwrap()
    }

    /// The row of the unchanged `s -> a` every definition starts with.
    fn entry() -> EdgeCorrespondence {
        row(Some(d("s", "a", 1)), Some(d("s", "a", 1)), &[])
    }

    #[test]
    fn swapped_duplicates_move_their_addresses_and_their_order() {
        let x = diff(
            &format!("  - {{ from: a, to: done, {OK} }}\n  - {{ from: a, to: done, {FAIL} }}\n"),
            &format!("  - {{ from: a, to: done, {FAIL} }}\n  - {{ from: a, to: done, {OK} }}\n"),
        );
        assert_eq!(
            x.edges,
            vec![
                entry(),
                row(
                    Some(d("a", "done", 2)),
                    Some(d("a", "done", 1)),
                    &["address"]
                ),
                row(
                    Some(d("a", "done", 1)),
                    Some(d("a", "done", 2)),
                    &["address"]
                ),
            ]
        );
        assert_eq!(
            x.edge_order_changed,
            vec![EdgeOrderChange {
                node: "a".into(),
                before: vec![d("a", "done", 1), d("a", "done", 2)],
                after: vec![d("a", "done", 2), d("a", "done", 1)],
            }]
        );
    }

    #[test]
    fn moving_a_fallback_or_an_unconditional_edge_is_no_order_change() {
        // The fallback moved from first to last, as V123 asks: checked
        // last either way.
        let x = diff(
            &format!(
                "  - {{ from: a, to: bad, fallback: true }}\n  - {{ from: a, to: done, {OK} }}\n"
            ),
            &format!(
                "  - {{ from: a, to: done, {OK} }}\n  - {{ from: a, to: bad, fallback: true }}\n"
            ),
        );
        assert!(
            x.edge_order_changed.is_empty(),
            "{:?}",
            x.edge_order_changed
        );
        assert!(
            x.edges.iter().all(|r| r.changed.is_empty()),
            "{:?}",
            x.edges
        );
        // Two conditional edges keep their order around a moved
        // unconditional one.
        let x = diff(
            &format!(
                "  - {{ from: a, to: done, {OK} }}\n  - {{ from: a, to: bad }}\n  - {{ from: a, to: done, {FAIL} }}\n"
            ),
            &format!(
                "  - {{ from: a, to: bad }}\n  - {{ from: a, to: done, {OK} }}\n  - {{ from: a, to: done, {FAIL} }}\n"
            ),
        );
        assert!(
            x.edge_order_changed.is_empty(),
            "{:?}",
            x.edge_order_changed
        );
    }

    #[test]
    fn an_identical_id_less_edge_is_unchanged_and_the_authored_one_removed() {
        let x = diff(
            "  - { id: k, from: a, to: done }\n  - { from: a, to: done }\n",
            "  - { from: a, to: done }\n",
        );
        let k = EdgeAddress::Authored { id: "k".into() };
        assert_eq!(
            x.edges,
            vec![
                entry(),
                row(Some(d("a", "done", 1)), Some(d("a", "done", 1)), &[]),
                row(Some(k), None, &[]),
            ]
        );
    }

    #[test]
    fn deleting_the_first_duplicate_is_one_removal() {
        let x = diff(
            &format!("  - {{ from: a, to: done, {OK} }}\n  - {{ from: a, to: done, {FAIL} }}\n"),
            &format!("  - {{ from: a, to: done, {FAIL} }}\n"),
        );
        assert_eq!(
            x.edges,
            vec![
                entry(),
                row(
                    Some(d("a", "done", 2)),
                    Some(d("a", "done", 1)),
                    &["address"]
                ),
                row(Some(d("a", "done", 1)), None, &[]),
            ]
        );
        assert!(x.edge_order_changed.is_empty());
    }

    #[test]
    fn a_condition_change_an_added_fallback_and_a_renamed_id() {
        let x = diff(
            &format!("  - {{ id: one, from: a, to: done, {OK} }}\n"),
            &format!(
                "  - {{ id: two, from: a, to: done, {FAIL} }}\n  - {{ from: a, to: bad, fallback: true }}\n"
            ),
        );
        let two = EdgeAddress::Authored { id: "two".into() };
        let one = EdgeAddress::Authored { id: "one".into() };
        assert_eq!(
            x.edges,
            vec![
                entry(),
                row(Some(one), Some(two), &["id", "condition", "address"]),
                row(None, Some(d("a", "bad", 1)), &[]),
            ]
        );
        let x = diff(
            &format!("  - {{ from: a, to: done, {OK} }}\n"),
            &format!("  - {{ from: a, to: done, {FAIL} }}\n"),
        );
        assert_eq!(
            x.edges[1],
            row(
                Some(d("a", "done", 1)),
                Some(d("a", "done", 1)),
                &["condition"]
            )
        );
        let x = diff(
            &format!("  - {{ id: one, from: a, to: done, {OK} }}\n"),
            &format!("  - {{ id: two, from: a, to: done, {OK} }}\n"),
        );
        assert_eq!(x.edges[1].changed, vec!["id", "address"]);
    }

    #[test]
    fn identical_definitions_change_no_edge() {
        let edges = format!(
            "  - {{ from: a, to: done, {OK} }}\n  - {{ from: a, to: done, {FAIL} }}\n  - {{ id: last, from: a, to: bad, fallback: true }}\n"
        );
        let x = diff(&edges, &edges);
        assert_eq!(x.edges.len(), 4);
        assert!(
            x.edges
                .iter()
                .all(|r| r.changed.is_empty() && r.before.is_some() && r.after.is_some())
        );
        assert!(x.edge_order_changed.is_empty());
    }

    #[test]
    fn an_id_match_comes_before_the_ends() {
        let x = diff(
            "  - { id: k, from: a, to: done }\n",
            "  - { id: k, from: a, to: bad }\n",
        );
        let k = EdgeAddress::Authored { id: "k".into() };
        assert_eq!(x.edges[1], row(Some(k.clone()), Some(k), &["to"]));
    }

    #[test]
    fn a_huge_change_is_diffed_without_the_table() {
        let old: String = (0..3000).map(|i| format!("old {i}\n")).collect();
        let new: String = (0..3000).map(|i| format!("new {i}\n")).collect();
        let diff = line_diff(&format!("head\n{old}"), &format!("head\n{new}"));
        let lines: Vec<&str> = diff.lines().collect();
        assert_eq!(lines.len(), 6001);
        assert_eq!(lines[0], " head");
        assert_eq!(lines[1], "-old 0");
        assert_eq!(lines[3001], "+new 0");
    }
}
