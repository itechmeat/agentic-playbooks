//! What changed between two playbook definitions: nodes and edges
//! compared structurally, and the YAML line by line. Shared by the diff of
//! two stored versions and by the preview of an import.

use std::collections::{HashMap, HashSet};

use serde::Serialize;

use super::{VersioningError, schema_err};
use crate::schema::Playbook;

/// Structural and textual diff of two versions of the same playbook.
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct VersionDiff {
    pub nodes_added: Vec<String>,
    pub nodes_removed: Vec<String>,
    pub nodes_changed: Vec<String>,
    pub edges_added: Vec<String>,
    pub edges_removed: Vec<String>,
    pub yaml_diff: String,
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

    Ok(VersionDiff {
        nodes_added,
        nodes_removed,
        nodes_changed,
        edges_added,
        edges_removed,
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
