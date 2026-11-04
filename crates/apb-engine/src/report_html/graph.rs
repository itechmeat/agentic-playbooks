//! The report's playbook graph: the definition the run executed, laid out
//! in layers and drawn as one inline SVG. The layout is a small
//! Sugiyama-style pass: rank by the longest path from the start (the back
//! edges of loops are drawn but never used for the ranking), then a few
//! barycenter passes over the order inside a rank to reduce crossings.
//! Each node is a box with its id or title and its kind, filled by the
//! status the run reports for it; each edge is a line with an arrowhead,
//! solid when the run traversed it and dashed when it did not, and a judge
//! or review edge carries its option label. Above [GRAPH_MAX_NODES] nodes
//! the section says so instead of drawing. Nothing here runs: the drawing
//! is one SVG element tree, every string from the playbook redacted and
//! escaped like the rest of the page.

use std::collections::{BTreeMap, HashSet};

use apb_core::schema::{Edge, EdgeCondition, Node, Playbook};

use super::markup::{Markup, SvgAttr};
use super::model::Report;
use super::sections::{Writer, section};
use crate::run_outcome::EdgeState;

/// Above this many nodes the report draws no graph.
pub const GRAPH_MAX_NODES: usize = 80;

/// The box geometry, in SVG units.
const NODE_W: usize = 176;
const NODE_H: usize = 48;
/// The gaps between boxes, in SVG units.
const GAP_X: usize = 28;
const GAP_Y: usize = 64;
/// The drawing's padding, in SVG units.
const PAD: usize = 16;
/// The longest node label, in chars, cut after redaction.
const LABEL_MAX_CHARS: usize = 24;
/// The longest tooltip, in chars, cut after redaction.
const TITLE_MAX_CHARS: usize = 80;
/// The horizontal room one back edge gets, drawn around the right side.
const BACK_LANE: usize = 34;
/// How far from a box's right end a lane-routed edge attaches.
const LANE_INSET: usize = 18;
/// How far a lane-routed edge's horizontal run sits from the box border it
/// leaves or enters (below `GAP_Y / 2`, where straight edges' labels sit).
const LANE_CHANNEL: usize = 10;
/// The step by which lanes attaching to one box border are kept apart.
const LANE_ATTACH_STEP: usize = 6;
/// The step by which lanes running through one channel are kept apart
/// (four steps stay above the straight edges' labels).
const LANE_CHANNEL_STEP: usize = 4;
/// How many barycenter passes over the order inside a rank.
const BARYCENTER_PASSES: usize = 4;

/// The Graph section: the definition the run actually executed (the shown
/// segment's stored definition), laid out and drawn. A definition the run
/// did not establish, or one too big to draw, says so in one line.
pub(super) fn run_graph(w: &Writer, report: &Report) -> Markup {
    let Some(pb) = report.definition.as_ref() else {
        return section(
            "Graph",
            w.el(
                "p",
                Some("empty"),
                "The run's definition is not established, so its graph is not shown.",
            ),
        );
    };
    let statuses: BTreeMap<String, String> = report
        .graph
        .nodes
        .iter()
        .filter_map(|n| n.status.clone().map(|s| (n.node.clone(), s)))
        .collect();
    let taken = taken_edges(report, pb);
    section("Graph", draw(w, pb, &statuses, &taken))
}

/// Which of `pb`'s edges the run traversed, keyed by position in
/// `pb.edges`: an edge of the shown segment the journal marked taken is
/// solid, everything else is dashed. A shown segment always addresses the
/// same edge list, so the lookup by rendered address is exact.
fn taken_edges(report: &Report, pb: &Playbook) -> BTreeMap<usize, bool> {
    let addressed = apb_core::edge_address::edge_addresses(pb);
    let by_rendered: BTreeMap<&str, usize> = addressed
        .iter()
        .map(|a| (a.rendered.as_str(), a.index))
        .collect();
    let mut out = BTreeMap::new();
    if let Some(seg) = report.graph.shown_segment {
        for e in report.graph.edges_of(seg) {
            if let Some(&index) = by_rendered.get(e.rendered.as_str()) {
                out.insert(index, matches!(e.state, EdgeState::Taken { .. }));
            }
        }
    }
    out
}

/// The drawn graph for one definition: `statuses` maps a node id to the
/// status the run reports for it (absent reads as never ran), `taken`
/// says whether the run traversed the edge at that position of
/// `pb.edges` (absent reads as not traversed).
pub(super) fn draw(
    w: &Writer,
    pb: &Playbook,
    statuses: &BTreeMap<String, String>,
    taken: &BTreeMap<usize, bool>,
) -> Markup {
    if pb.nodes.is_empty() {
        return note(w, "The playbook has no nodes to draw.");
    }
    if pb.nodes.len() > GRAPH_MAX_NODES {
        return note(
            w,
            &format!(
                "The playbook has {} nodes; the report draws a graph for at most {GRAPH_MAX_NODES}.",
                pb.nodes.len()
            ),
        );
    }
    let l = layout(pb);
    // The drawing keeps its intrinsic scale and the panel scrolls: a wide
    // graph shrunk to the page width would be unreadable.
    Markup::concat([
        Markup::el(
            "div",
            Some("graph-scroll"),
            svg_of(w, pb, &l, statuses, taken),
        ),
        legend(w),
    ])
}

/// One line where a graph would be.
fn note(w: &Writer, text: &str) -> Markup {
    w.el("p", Some("empty"), text)
}

/// Lays `pb` out: ranks, then the order inside each rank, then coordinates.
/// Edges that would cut across the grid (they skip ranks, run upward, or
/// run along one rank) each get a lane around the right side, and the
/// drawing widens by one lane each.
pub(super) fn layout(pb: &Playbook) -> Layout {
    let back = back_edges_of(pb);
    let ranks = ranks_of(pb, &back);
    let rows = order_of(pb, &ranks);
    let mut l = coords_of(&rows);
    l.width += lanes_of(pb, &ranks).len() * BACK_LANE;
    l
}

/// The edges that must leave the straight grid because a straight line
/// would cut across boxes they do not touch: every edge that skips ranks,
/// runs upward, or runs along one rank. Each gets its own lane around the
/// right side, keyed by its position in `pb.edges` to its lane number.
fn lanes_of(pb: &Playbook, ranks: &BTreeMap<String, usize>) -> BTreeMap<usize, usize> {
    let mut need = Vec::new();
    for (i, e) in pb.edges.iter().enumerate() {
        let (Some(sr), Some(tr)) = (ranks.get(&e.from), ranks.get(&e.to)) else {
            continue;
        };
        if *tr <= *sr || *tr - *sr > 1 {
            need.push(i);
        }
    }
    need.into_iter().enumerate().map(|(k, i)| (i, k)).collect()
}

/// The boxes of one laid-out definition.
pub(super) struct Layout {
    /// The boxes in rank and row order, not in `pb.nodes` order: look a
    /// node up by its id.
    pub(super) nodes: Vec<NodeBox>,
    /// The width of the drawn content (no padding, no back lanes).
    pub(super) content_width: usize,
    /// The drawing's full size, in SVG units.
    pub(super) width: usize,
    /// The drawing's height, in SVG units.
    pub(super) height: usize,
}

/// One node's box: its id, the layer it sits in and where.
pub(super) struct NodeBox {
    pub(super) id: String,
    pub(super) rank: usize,
    pub(super) x: usize,
    pub(super) y: usize,
}

/// The positions of `pb`'s edges that a depth-first walk finds going back
/// up (or sideways): they are drawn, but never used for the ranking.
pub(super) fn back_edges_of(pb: &Playbook) -> HashSet<usize> {
    let mut adj: BTreeMap<&str, Vec<usize>> = BTreeMap::new();
    let known: HashSet<&str> = pb.nodes.iter().map(|n| n.id.as_str()).collect();
    for (i, e) in pb.edges.iter().enumerate() {
        if !known.contains(e.from.as_str()) || !known.contains(e.to.as_str()) {
            continue;
        }
        adj.entry(e.from.as_str()).or_default().push(i);
    }
    // 0 white, 1 grey (on the walk's stack), 2 black (done).
    let mut color: BTreeMap<&str, u8> = BTreeMap::new();
    let mut back = HashSet::new();
    for n in &pb.nodes {
        if color.get(n.id.as_str()).copied().unwrap_or(0) != 0 {
            continue;
        }
        let id = n.id.as_str();
        color.insert(id, 1);
        let mut stack: Vec<(&str, usize)> = vec![(id, 0)];
        while let Some((v, ci)) = stack.pop() {
            let children = adj.get(v).map(Vec::as_slice).unwrap_or(&[]);
            if ci < children.len() {
                stack.push((v, ci + 1));
                let i = children[ci];
                let next = pb.edges[i].to.as_str();
                match color.get(next).copied().unwrap_or(0) {
                    0 => {
                        color.insert(next, 1);
                        stack.push((next, 0));
                    }
                    1 => {
                        back.insert(i);
                    }
                    _ => {}
                }
            } else {
                color.insert(v, 2);
            }
        }
    }
    back
}

/// The layer of each node: the longest path of non-back edges that reaches
/// it, with the sources (no incoming forward edge) at rank 0.
fn ranks_of(pb: &Playbook, back: &HashSet<usize>) -> BTreeMap<String, usize> {
    let mut rank: BTreeMap<String, usize> = pb.nodes.iter().map(|n| (n.id.clone(), 0)).collect();
    let mut indegree: BTreeMap<&str, usize> = pb.nodes.iter().map(|n| (n.id.as_str(), 0)).collect();
    let mut fwd: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
    let known: HashSet<&str> = pb.nodes.iter().map(|n| n.id.as_str()).collect();
    for (i, e) in pb.edges.iter().enumerate() {
        if back.contains(&i) || !known.contains(e.from.as_str()) || !known.contains(e.to.as_str()) {
            // An edge naming an id the definition does not define is
            // skipped: the report never draws, and never panics on it.
            continue;
        }
        fwd.entry(e.from.as_str()).or_default().push(e.to.as_str());
        *indegree.entry(e.to.as_str()).or_insert(0) += 1;
    }
    // The walk of the back-edge DFS leaves an acyclic rest graph, so
    // Kahn's order visits every node and every node keeps a rank.
    let mut queue: Vec<&str> = pb
        .nodes
        .iter()
        .filter(|n| indegree[n.id.as_str()] == 0)
        .map(|n| n.id.as_str())
        .collect();
    let mut qi = 0;
    while qi < queue.len() {
        let v = queue[qi];
        qi += 1;
        let r = rank[v];
        for next in fwd.get(v).into_iter().flatten() {
            let r = r + 1;
            let slot = rank.get_mut(*next).unwrap();
            if *slot < r {
                *slot = r;
            }
            let d = indegree.get_mut(*next).unwrap();
            *d -= 1;
            if *d == 0 {
                queue.push(next);
            }
        }
    }
    rank
}

/// The order inside each rank: definition order first, then a few
/// barycenter passes (each node moves towards the mean position of its
/// neighbours in the adjacent rank; the sweeps alternate down and up).
fn order_of(pb: &Playbook, ranks: &BTreeMap<String, usize>) -> Vec<Vec<String>> {
    let max_rank = ranks.values().copied().max().unwrap_or(0);
    let mut rows: Vec<Vec<String>> = vec![Vec::new(); max_rank + 1];
    for n in &pb.nodes {
        rows[ranks[&n.id]].push(n.id.clone());
    }
    let mut preds: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
    let mut succs: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
    for e in &pb.edges {
        succs
            .entry(e.from.as_str())
            .or_default()
            .push(e.to.as_str());
        preds
            .entry(e.to.as_str())
            .or_default()
            .push(e.from.as_str());
    }
    for pass in 0..BARYCENTER_PASSES {
        let down = pass % 2 == 0;
        for r in rank_seq(max_rank, down) {
            let Some(adj) = adjacent_rank(r, max_rank, down) else {
                continue;
            };
            order_rank(&mut rows, &preds, &succs, r, adj, down);
        }
    }
    rows
}

/// The ranks one sweep visits, top down or bottom up.
fn rank_seq(max_rank: usize, down: bool) -> Vec<usize> {
    let mut seq: Vec<usize> = (0..=max_rank).collect();
    if !down {
        seq.reverse();
    }
    seq
}

/// The rank whose positions this sweep reads, if there is one.
fn adjacent_rank(r: usize, max_rank: usize, down: bool) -> Option<usize> {
    match down {
        true if r > 0 => Some(r - 1),
        false if r < max_rank => Some(r + 1),
        _ => None,
    }
}

/// One barycenter sweep over rank `r`: every node moves towards the mean
/// position of its neighbours in rank `adj`.
fn order_rank(
    rows: &mut [Vec<String>],
    preds: &BTreeMap<&str, Vec<&str>>,
    succs: &BTreeMap<&str, Vec<&str>>,
    r: usize,
    adj: usize,
    down: bool,
) {
    let positions: BTreeMap<&str, f64> = rows[adj]
        .iter()
        .enumerate()
        .map(|(i, id)| (id.as_str(), i as f64))
        .collect();
    let neighbours_of = |v: &str| match down {
        true => preds.get(v),
        false => succs.get(v),
    };
    let mut keyed: Vec<(f64, &String)> = rows[r]
        .iter()
        .map(|v| {
            let mut sum = 0.0;
            let mut count = 0usize;
            for n in neighbours_of(v).into_iter().flatten() {
                if let Some(p) = positions.get(*n) {
                    sum += p;
                    count += 1;
                }
            }
            let key = if count == 0 {
                rows[r].iter().position(|x| x == v).unwrap_or(0) as f64
            } else {
                sum / count as f64
            };
            (key, v)
        })
        .collect();
    keyed.sort_by(|a, b| a.0.total_cmp(&b.0));
    rows[r] = keyed.into_iter().map(|(_, v)| v.clone()).collect();
}

/// Coordinates from the ordered ranks: boxes centered per rank, one layer
/// per rank.
fn coords_of(rows: &[Vec<String>]) -> Layout {
    let rank_w = |c: usize| {
        if c == 0 {
            0
        } else {
            c * NODE_W + (c - 1) * GAP_X
        }
    };
    let content_width = rows.iter().map(|r| rank_w(r.len())).max().unwrap_or(0);
    let mut nodes = Vec::new();
    for (r, row) in rows.iter().enumerate() {
        let offset = (content_width.saturating_sub(rank_w(row.len()))) / 2;
        for (i, id) in row.iter().enumerate() {
            nodes.push(NodeBox {
                id: id.clone(),
                rank: r,
                x: offset + i * (NODE_W + GAP_X),
                y: PAD + r * (NODE_H + GAP_Y),
            });
        }
    }
    Layout {
        content_width,
        width: content_width + 2 * PAD,
        height: 2 * PAD + rows.len() * NODE_H + rows.len().saturating_sub(1) * GAP_Y,
        nodes,
    }
}

/// The graph as one SVG element tree. Edges are drawn first (under the
/// boxes), then the nodes; every string through the redactor and escaped.
fn svg_of(
    w: &Writer,
    pb: &Playbook,
    l: &Layout,
    statuses: &BTreeMap<String, String>,
    taken: &BTreeMap<usize, bool>,
) -> Markup {
    let pos: BTreeMap<&str, &NodeBox> = l.nodes.iter().map(|n| (n.id.as_str(), n)).collect();
    let ranks: BTreeMap<String, usize> = l.nodes.iter().map(|n| (n.id.clone(), n.rank)).collect();
    let lane_of = lanes_of(pb, &ranks);
    let mut parts = vec![arrow_defs()];
    for (i, e) in pb.edges.iter().enumerate() {
        let (Some(s), Some(t)) = (
            pos.get(e.from.as_str()).copied(),
            pos.get(e.to.as_str()).copied(),
        ) else {
            // An edge naming an id the definition does not define is not
            // drawn.
            continue;
        };
        let solid = taken.get(&i).copied().unwrap_or(false);
        // An edge that would cut across the grid (it skips ranks, runs
        // upward, or runs along one rank) goes around the right side in
        // its own lane.
        let detour = lane_of.get(&i).map(|k| (detour_x(l, *k), *k));
        parts.push(edge_svg(w, e, solid, detour, s, t));
    }
    let by_id: BTreeMap<&str, &Node> = pb.nodes.iter().map(|n| (n.id.as_str(), n)).collect();
    for b in &l.nodes {
        let Some(node) = by_id.get(b.id.as_str()).copied() else {
            continue;
        };
        let status = statuses.get(&b.id).map(String::as_str);
        parts.push(node_svg(w, node, b, status));
    }
    Markup::svg(
        "svg",
        Some("graph-svg"),
        &[
            SvgAttr("viewBox", format!("0 0 {} {}", l.width, l.height)),
            SvgAttr("width", l.width.to_string()),
            SvgAttr("height", l.height.to_string()),
            SvgAttr("role", "img".into()),
        ],
        Markup::concat(parts),
    )
}

/// Where the lane `k` detour around the right side runs.
fn detour_x(l: &Layout, k: usize) -> usize {
    l.content_width + PAD + k * BACK_LANE + BACK_LANE / 2
}

/// One edge as a path with its arrowhead marker, plus its option label
/// when the condition routes on one (a judge or a review edge).
fn edge_svg(
    w: &Writer,
    e: &Edge,
    solid: bool,
    detour: Option<(usize, usize)>,
    s: &NodeBox,
    t: &NodeBox,
) -> Markup {
    let (d, label_at) = match detour {
        Some((x, k)) => detour_path(s, t, x, k),
        None => straight_path(s, t),
    };
    let (cls, marker) = match solid {
        true => ("graph-edge-taken", "url(#apb-arrow-taken)"),
        false => ("graph-edge-skip", "url(#apb-arrow-skip)"),
    };
    let mut parts = vec![Markup::svg(
        "path",
        Some(&format!("graph-edge {cls}")),
        &[SvgAttr("d", d), SvgAttr("marker-end", marker.into())],
        Markup::default(),
    )];
    if let Some(label) = e.condition.as_ref().and_then(edge_label) {
        parts.push(label_svg(w, &label, label_at));
    }
    Markup::concat(parts)
}

/// The label of a condition edge: what it routes on, in the forms the
/// dashboard spells them. A judge question edge shows its threshold rather
/// than its question (the question is too long for a label; the threshold
/// is what the answer is compared against).
fn edge_label(condition: &EdgeCondition) -> Option<String> {
    match condition {
        EdgeCondition::ReviewStatus { equals } => Some(format!("review: {equals}")),
        EdgeCondition::OutputField { field, equals, .. } => Some(format!("{field}: {equals}")),
        EdgeCondition::Judge { min_p, .. } => Some(format!("judge p>={}", min_p.0)),
        _ => None,
    }
}

/// The option label as one muted text element at `at`.
fn label_svg(w: &Writer, label: &str, at: (usize, usize)) -> Markup {
    Markup::svg(
        "text",
        Some("graph-elabel"),
        &[
            SvgAttr("x", at.0.to_string()),
            SvgAttr("y", at.1.to_string()),
            SvgAttr("text-anchor", "middle".into()),
        ],
        Markup::text(&w.short(label, LABEL_MAX_CHARS)),
    )
}

/// One node as a group: a box with the status fill, its id or title, its
/// kind, and a hover title with id and status.
fn node_svg(w: &Writer, node: &Node, b: &NodeBox, status: Option<&str>) -> Markup {
    let cls = status_class(status);
    let label = node.title.as_deref().unwrap_or(&b.id);
    Markup::svg(
        "g",
        Some(&format!("graph-node {cls}")),
        &[SvgAttr("transform", format!("translate({}, {})", b.x, b.y))],
        Markup::concat([
            Markup::svg(
                "title",
                None,
                &[],
                Markup::text(&w.short(
                    &format!("{}: {}", b.id, status.unwrap_or("not run")),
                    TITLE_MAX_CHARS,
                )),
            ),
            Markup::svg(
                "rect",
                Some("graph-box"),
                &[
                    SvgAttr("width", NODE_W.to_string()),
                    SvgAttr("height", NODE_H.to_string()),
                    SvgAttr("rx", "8".into()),
                ],
                Markup::default(),
            ),
            Markup::svg(
                "text",
                Some("graph-nlabel"),
                &[
                    SvgAttr("x", (NODE_W / 2).to_string()),
                    SvgAttr("y", "19".into()),
                    SvgAttr("text-anchor", "middle".into()),
                ],
                Markup::text(&w.short(label, LABEL_MAX_CHARS)),
            ),
            Markup::svg(
                "text",
                Some("graph-nkind"),
                &[
                    SvgAttr("x", (NODE_W / 2).to_string()),
                    SvgAttr("y", "37".into()),
                    SvgAttr("text-anchor", "middle".into()),
                ],
                Markup::text(&w.short(node.kind.type_str(), LABEL_MAX_CHARS)),
            ),
        ]),
    )
}

/// The graph class of one node status: the buckets the report's status
/// colors carry.
fn status_class(status: Option<&str>) -> &'static str {
    match status {
        Some("succeeded") => "graph-st-succeeded",
        Some("failed" | "timed_out") => "graph-st-failed",
        // An interrupted node belongs to a run a person paused and that
        // will continue, so it reads as running, not as failed.
        Some("running" | "retry_waiting" | "interrupted" | "unknown") => "graph-st-running",
        Some("skipped" | "cancelled") => "graph-st-skipped",
        _ => "graph-st-norun",
    }
}

/// The line from box to box: where it leaves the source and enters the
/// target, between the two centers.
fn straight_path(s: &NodeBox, t: &NodeBox) -> (String, (usize, usize)) {
    let sc = (s.x + NODE_W / 2, s.y + NODE_H / 2);
    let tc = (t.x + NODE_W / 2, t.y + NODE_H / 2);
    let from = border_point(sc, tc, NODE_W / 2, NODE_H / 2);
    let to = border_point(tc, sc, NODE_W / 2, NODE_H / 2);
    (
        format!("M {} {} L {} {}", from.0, from.1, to.0, to.1),
        ((from.0 + to.0) / 2, (from.1 + to.1) / 2),
    )
}

/// The polyline for an edge that does not step one rank down (it skips
/// ranks, runs upward, or runs along one rank): out of the source's
/// bottom border (a forward edge) or top border (a back edge), across the
/// free channel between two ranks, down or up the lane `k` at `x`, across
/// the channel next to the target, and into the target's top or bottom
/// border. Every horizontal run sits in an inter-rank channel or in the
/// lane right of the content, where no box is, so the edge never runs
/// hidden under a same-rank sibling. It attaches near the right end of a
/// border, away from the centered straight edges, and both the attach
/// point and the channel height are staggered per lane so two lanes that
/// share a box or a channel stay apart (up to four; lanes four apart share
/// a height) and clear of the straight edges' labels at mid-channel.
fn detour_path(s: &NodeBox, t: &NodeBox, x: usize, k: usize) -> (String, (usize, usize)) {
    let inset = LANE_INSET + LANE_ATTACH_STEP * (k % 4);
    let sx = s.x + NODE_W - inset;
    let tx = t.x + NODE_W - inset;
    let ch = LANE_CHANNEL + LANE_CHANNEL_STEP * (k % 4);
    let (from_y, y1, y2, to_y) = if t.y >= s.y {
        (s.y + NODE_H, s.y + NODE_H + ch, t.y.saturating_sub(ch), t.y)
    } else {
        (s.y, s.y.saturating_sub(ch), t.y + NODE_H + ch, t.y + NODE_H)
    };
    (
        format!("M {sx} {from_y} L {sx} {y1} L {x} {y1} L {x} {y2} L {tx} {y2} L {tx} {to_y}"),
        (x, (y1 + y2) / 2),
    )
}

/// The point where the segment from `from` towards `to` leaves the box
/// centered at `from` with the half sizes `hw` by `hh`.
fn border_point(from: (usize, usize), to: (usize, usize), hw: usize, hh: usize) -> (usize, usize) {
    let (fx, fy) = (from.0 as f64, from.1 as f64);
    let (tx, ty) = (to.0 as f64, to.1 as f64);
    let (dx, dy) = (tx - fx, ty - fy);
    let mut scale = f64::INFINITY;
    if dx != 0.0 {
        scale = scale.min(hw as f64 / dx.abs());
    }
    if dy != 0.0 {
        scale = scale.min(hh as f64 / dy.abs());
    }
    if scale.is_infinite() {
        return from;
    }
    (
        (fx + dx * scale).round() as usize,
        (fy + dy * scale).round() as usize,
    )
}

/// The two arrowhead markers, one per edge state.
fn arrow_defs() -> Markup {
    let marker = |id: &'static str, cls: &'static str| {
        Markup::svg(
            "marker",
            None,
            &[
                SvgAttr("id", id.into()),
                SvgAttr("viewBox", "0 0 10 10".into()),
                SvgAttr("refX", "9".into()),
                SvgAttr("refY", "5".into()),
                SvgAttr("markerWidth", "7".into()),
                SvgAttr("markerHeight", "7".into()),
                SvgAttr("orient", "auto".into()),
            ],
            Markup::svg(
                "path",
                Some(cls),
                &[SvgAttr("d", "M 0 0 L 10 5 L 0 10 Z".into())],
                Markup::default(),
            ),
        )
    };
    Markup::svg(
        "defs",
        None,
        &[],
        Markup::concat([
            marker("apb-arrow-taken", "graph-arrow-taken"),
            marker("apb-arrow-skip", "graph-arrow-skip"),
        ]),
    )
}

/// The legend: one dot per status bucket and the edge states in words.
fn legend(w: &Writer) -> Markup {
    const DOTS: [(&str, &str); 5] = [
        ("graph-dot graph-dot-succeeded", "succeeded"),
        ("graph-dot graph-dot-failed", "failed"),
        ("graph-dot graph-dot-running", "running"),
        ("graph-dot graph-dot-skipped", "skipped"),
        ("graph-dot graph-dot-norun", "not run"),
    ];
    let mut parts: Vec<Markup> = Vec::new();
    for (dot, label) in DOTS {
        parts.push(Markup::el("span", Some(dot), Markup::default()));
        parts.push(w.el("span", None, label));
    }
    parts.push(Markup::el(
        "span",
        Some("graph-legend-note"),
        Markup::text("edges: solid traversed, dashed not taken"),
    ));
    Markup::el("p", Some("graph-legend"), Markup::concat(parts))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    fn pb(yaml: &str) -> Playbook {
        Playbook::from_yaml(yaml).unwrap()
    }

    fn w() -> Writer {
        Writer::new(crate::redact::Redactor::new(Vec::new(), Path::new("/")))
    }

    fn ranks(l: &Layout) -> BTreeMap<String, usize> {
        l.nodes.iter().map(|n| (n.id.clone(), n.rank)).collect()
    }

    fn node_y(l: &Layout, id: &str) -> usize {
        l.nodes.iter().find(|n| n.id == id).unwrap().y
    }

    fn node_x(l: &Layout, id: &str) -> usize {
        l.nodes.iter().find(|n| n.id == id).unwrap().x
    }

    const CHAIN: &str = "schema: 2\nid: c\nname: C\nversion: 1.0.0\nnodes:\n  - { id: s, type: start }\n  - { id: work, type: script, script: work.sh, runner: sh }\n  - { id: done, type: finish, outcome: success, prompt: sum up }\nedges:\n  - { from: s, to: work }\n  - { from: work, to: done }\n";

    const FAN: &str = "schema: 2\nid: f\nname: F\nversion: 1.0.0\nnodes:\n  - { id: s, type: start }\n  - { id: a, type: script, script: work.sh, runner: sh }\n  - { id: b, type: script, script: work.sh, runner: sh }\n  - { id: done, type: finish, outcome: success, prompt: sum up }\nedges:\n  - { from: s, to: a }\n  - { from: s, to: b }\n  - { from: a, to: done }\n  - { from: b, to: done }\n";

    const LOOP: &str = "schema: 2\nid: l\nname: L\nversion: 1.0.0\nnodes:\n  - { id: s, type: start }\n  - { id: head, type: script, script: work.sh, runner: sh }\n  - { id: tail, type: script, script: work.sh, runner: sh }\n  - { id: done, type: finish, outcome: success, prompt: sum up }\nedges:\n  - { from: s, to: head }\n  - { from: head, to: tail }\n  - { from: tail, to: head }\n  - { from: tail, to: done }\n";

    #[test]
    fn a_chain_lays_out_one_rank_per_step() {
        let l = layout(&pb(CHAIN));
        let r = ranks(&l);
        assert_eq!(r["s"], 0);
        assert_eq!(r["work"], 1);
        assert_eq!(r["done"], 2);
        assert!(node_y(&l, "s") < node_y(&l, "work"));
        assert!(node_y(&l, "work") < node_y(&l, "done"));
    }

    #[test]
    fn fan_out_and_fan_in_rank_by_layer() {
        let l = layout(&pb(FAN));
        let r = ranks(&l);
        assert_eq!(r["s"], 0);
        assert_eq!(r["a"], 1);
        assert_eq!(r["b"], 1);
        assert_eq!(r["done"], 2);
        assert_ne!(
            node_x(&l, "a"),
            node_x(&l, "b"),
            "branches sit side by side"
        );
    }

    #[test]
    fn a_loop_edge_is_drawn_but_not_used_for_ranking() {
        let p = pb(LOOP);
        let back = back_edges_of(&p);
        assert_eq!(back, HashSet::from([2]), "tail to head is the back edge");
        let r = ranks(&layout(&p));
        assert_eq!(r["s"], 0);
        assert_eq!(r["head"], 1);
        assert_eq!(r["tail"], 2);
        assert_eq!(r["done"], 3);
        // The back edge is still drawn: one path per edge of the definition.
        let svg = draw(&w(), &p, &BTreeMap::new(), &BTreeMap::new()).into_string();
        assert_eq!(svg.matches("class=\"graph-edge ").count(), 4, "{svg}");
    }

    #[test]
    fn every_node_and_edge_appears_once_in_the_svg() {
        let p = pb(LOOP);
        let svg = draw(&w(), &p, &BTreeMap::new(), &BTreeMap::new()).into_string();
        assert_eq!(svg.matches("<g class=\"graph-node ").count(), 4, "{svg}");
        assert_eq!(svg.matches("class=\"graph-edge ").count(), 4, "{svg}");
        for id in ["s", "head", "tail", "done"] {
            assert_eq!(svg.matches(&format!(">{id}</text>")).count(), 1, "{id}");
        }
    }

    #[test]
    fn node_text_is_redacted_and_escaped() {
        const MARKED: &str = "schema: 2\nid: m\nname: M\nversion: 1.0.0\nnodes:\n  - { id: s, type: start }\n  - { id: work, type: script, script: work.sh, runner: sh, title: \"<b>Bold</b> \\\"q\\\" y\" }\n  - { id: done, type: finish, outcome: success, prompt: sum up }\nedges:\n  - { from: s, to: work }\n  - { from: work, to: done }\n";
        let svg = draw(&w(), &pb(MARKED), &BTreeMap::new(), &BTreeMap::new()).into_string();
        assert!(
            svg.contains("&lt;b&gt;Bold&lt;/b&gt; &quot;q&quot; y"),
            "{svg}"
        );
        assert!(!svg.contains("<b>Bold</b>"), "{svg}");
    }

    #[test]
    fn statuses_map_to_their_classes() {
        const FIVE: &str = "schema: 2\nid: v\nname: V\nversion: 1.0.0\nnodes:\n  - { id: s, type: start }\n  - { id: ok, type: script, script: work.sh, runner: sh }\n  - { id: bad, type: script, script: work.sh, runner: sh }\n  - { id: live, type: script, script: work.sh, runner: sh }\n  - { id: skip, type: script, script: work.sh, runner: sh }\n  - { id: idle, type: script, script: work.sh, runner: sh }\n  - { id: done, type: finish, outcome: success, prompt: sum up }\nedges:\n  - { from: s, to: ok }\n  - { from: ok, to: bad }\n  - { from: bad, to: live }\n  - { from: live, to: skip }\n  - { from: skip, to: idle }\n";
        let mut statuses = BTreeMap::new();
        statuses.insert("ok".to_string(), "succeeded".to_string());
        statuses.insert("bad".to_string(), "failed".to_string());
        statuses.insert("live".to_string(), "running".to_string());
        statuses.insert("skip".to_string(), "skipped".to_string());
        let svg = draw(&w(), &pb(FIVE), &statuses, &BTreeMap::new()).into_string();
        let case = |svg: &str, id: &str, want: &str| {
            let chunk = node_chunk(svg, id);
            assert!(
                chunk.contains(&format!("graph-st-{want}\"")),
                "{id} should read as {want}: {chunk}"
            );
        };
        case(&svg, "ok", "succeeded");
        case(&svg, "bad", "failed");
        case(&svg, "live", "running");
        case(&svg, "skip", "skipped");
        case(&svg, "idle", "norun");
        case(&svg, "done", "norun");
    }

    #[test]
    fn an_over_limit_definition_shows_a_note() {
        let p = pb(&big(GRAPH_MAX_NODES + 1));
        let out = draw(&w(), &p, &BTreeMap::new(), &BTreeMap::new()).into_string();
        assert!(!out.contains("<svg"), "{out}");
        assert!(out.contains("at most 80"), "{out}");
    }

    #[test]
    fn a_review_edge_carries_its_option_label() {
        const GATED: &str = "schema: 2\nid: g\nname: G\nversion: 1.0.0\nnodes:\n  - { id: s, type: start }\n  - { id: gate, type: human_review, prompt: ok?, options: [approve, reject] }\n  - { id: done, type: finish, outcome: success, prompt: sum up }\nedges:\n  - { from: s, to: gate }\n  - { from: gate, to: done, condition: { type: review_status, equals: approve } }\n";
        let svg = draw(&w(), &pb(GATED), &BTreeMap::new(), &BTreeMap::new()).into_string();
        assert!(svg.contains("review: approve"), "{svg}");
    }

    #[test]
    fn a_traversed_edge_is_solid_and_an_untaken_one_is_dashed() {
        let p = pb(FAN);
        let mut taken = BTreeMap::new();
        taken.insert(0, true);
        taken.insert(1, false);
        let svg = draw(&w(), &p, &BTreeMap::new(), &taken).into_string();
        // Per edge, in definition order: s->a taken, s->b not.
        let chunks: Vec<&str> = svg
            .split("<path class=\"graph-edge ")
            .skip(1)
            .map(|c| &c[..c.find("></path>").unwrap()])
            .collect();
        assert!(chunks[0].contains("graph-edge-taken"), "{}", chunks[0]);
        assert!(!chunks[0].contains("graph-edge-skip"), "{}", chunks[0]);
        assert!(chunks[1].contains("graph-edge-skip"), "{}", chunks[1]);
        assert!(!chunks[1].contains("graph-edge-taken"), "{}", chunks[1]);
    }

    /// The `<g>` chunk of one node box, found by the id its hover title
    /// names (the label may be the node's title instead of the id).
    fn node_chunk<'a>(svg: &'a str, id: &str) -> &'a str {
        let marker = format!("<title>{id}:");
        let start = svg
            .find("<g class=\"graph-node ")
            .expect("the svg draws node boxes");
        let rest = &svg[start..];
        let chunks: Vec<&str> = rest
            .split("<g class=\"graph-node ")
            .skip(1)
            .map(|c| &c[..c.find("</g>").expect("each group closes")])
            .collect();
        chunks
            .into_iter()
            .find(|c| c.contains(&marker))
            .unwrap_or_else(|| panic!("no box draws {id}: {rest}"))
    }

    #[test]
    fn an_out_of_order_definition_labels_each_box_from_its_own_node() {
        const JUMBLED: &str = "schema: 2\nid: j\nname: J\nversion: 1.0.0\nnodes:\n  - { id: done, type: finish, outcome: success, prompt: sum up, title: \"Done things\" }\n  - { id: s, type: start }\n  - { id: work, type: script, script: work.sh, runner: sh }\n  - { id: never, type: script, script: work.sh, runner: sh }\nedges:\n  - { from: s, to: work }\n  - { from: work, to: done }\n  - { from: s, to: never }\n";
        let p = pb(JUMBLED);
        let ranks = ranks_of(&p, &back_edges_of(&p));
        assert_eq!(ranks["done"], 2, "out-of-list order still ranks by reach");
        let svg = draw(&w(), &p, &BTreeMap::new(), &BTreeMap::new()).into_string();
        let done = node_chunk(&svg, "done");
        assert!(done.contains("Done things"), "{done}");
        assert!(done.contains("finish"), "{done}");
        let never = node_chunk(&svg, "never");
        assert!(never.contains("script"), "{never}");
        assert!(!never.contains("finish"), "{never}");
        let work = node_chunk(&svg, "work");
        assert!(work.contains("script"), "{work}");
        assert!(!work.contains("Done things"), "{work}");
    }

    /// A straight segment through the axis-aligned rectangle.
    fn segment_hits_box(from: (f64, f64), to: (f64, f64), box_at: (usize, usize)) -> bool {
        let (mut lo, mut hi) = (from, to);
        if lo.0 > hi.0 {
            std::mem::swap(&mut lo, &mut hi);
        }
        let (bx, by) = (box_at.0 as f64, box_at.1 as f64);
        // Liang-Barsky against the rectangle, padded by a small margin.
        let pad = 0.5;
        let (mut t0, mut t1) = (0.0f64, 1.0f64);
        let (dx, dy) = (hi.0 - lo.0, hi.1 - lo.1);
        for (p, q) in [
            (-dx, lo.0 - (bx - pad)),
            (dx, (bx + NODE_W as f64 + pad) - lo.0),
            (-dy, lo.1 - (by - pad)),
            (dy, (by + NODE_H as f64 + pad) - lo.1),
        ] {
            if p == 0.0 {
                if q < 0.0 {
                    return false;
                }
            } else {
                let r = q / p;
                if p < 0.0 {
                    if r > t1 {
                        return false;
                    }
                    t0 = t0.max(r);
                } else {
                    if r < t0 {
                        return false;
                    }
                    t1 = t1.min(r);
                }
            }
        }
        t0 <= t1
    }

    #[test]
    fn a_long_edge_is_routed_around_the_boxes_between() {
        const BYPASS: &str = "schema: 2\nid: by\nname: B\nversion: 1.0.0\nnodes:\n  - { id: s, type: start }\n  - { id: a, type: script, script: work.sh, runner: sh }\n  - { id: b, type: script, script: work.sh, runner: sh }\n  - { id: done, type: finish, outcome: success, prompt: sum up }\nedges:\n  - { from: s, to: a }\n  - { from: a, to: b }\n  - { from: b, to: done }\n  - { from: s, to: done }\n";
        let p = pb(BYPASS);
        let svg = draw(&w(), &p, &BTreeMap::new(), &BTreeMap::new()).into_string();
        // Every edge stays out of the boxes it does not touch, the lane
        // polyline included.
        assert_edges_clear_of_boxes(&svg);
        // The lane-routed long edge leaves the first column.
        let detour = edge_chunks(&svg)[3];
        let d = path_d(detour);
        assert!(
            widest_x(&path_points(d)) > NODE_W as f64,
            "the long edge is routed out of the column: {d}"
        );
        // The drawing reserves room for the lane.
        let width_at = svg.find("viewBox=\"0 0 ").unwrap() + "viewBox=\"0 0 ".len();
        let width: usize = svg[width_at..]
            .split(|c: char| !c.is_ascii_digit())
            .next()
            .unwrap()
            .parse()
            .unwrap();
        assert!(
            width > NODE_W + 2 * PAD,
            "the drawing reserves the lane: {svg}"
        );
    }

    /// A loop whose back edge leaves a box that has a sibling to its
    /// right, plus a rank-skipping edge from a non-rightmost box: the
    /// shape of the `work`/`never` report that hid the lane curve under
    /// the sibling.
    #[test]
    fn a_lane_routed_edge_stays_out_of_the_sibling_boxes_of_its_rank() {
        const LOOPJ: &str = "schema: 2\nid: lj\nname: LJ\nversion: 1.0.0\nnodes:\n  - { id: s, type: start }\n  - { id: check, type: script, script: work.sh, runner: sh }\n  - { id: work, type: script, script: work.sh, runner: sh }\n  - { id: never, type: script, script: work.sh, runner: sh }\n  - { id: done, type: finish, outcome: success, prompt: sum up }\nedges:\n  - { from: s, to: check }\n  - { from: check, to: work }\n  - { from: check, to: never }\n  - { from: work, to: check }\n  - { from: work, to: done }\n  - { from: never, to: done }\n  - { from: check, to: done }\n  - { from: s, to: done }\n";
        let p = pb(LOOPJ);
        let ranks = ranks_of(&p, &back_edges_of(&p));
        assert_eq!(ranks["work"], 2);
        assert_eq!(ranks["never"], 2, "work and never share a rank");
        assert!(
            node_x(&layout(&p), "work") < node_x(&layout(&p), "never"),
            "work has a sibling to its right"
        );
        let svg = draw(&w(), &p, &BTreeMap::new(), &BTreeMap::new()).into_string();
        // Lane-routed edges are polylines now: no hidden cubic.
        assert!(!svg.contains(" C "), "no curve segments: {svg}");
        // No edge crosses the `work`/`never` boxes it does not touch: on
        // the old rendering the back edge ran horizontally through
        // `never`, which read as a `work`-`never` connection.
        assert_edges_clear_of_boxes(&svg);
        // Two lanes into one target (`check -> done`, `s -> done`) run
        // their horizontal stretches apart, never one over the other.
        assert_no_shared_runs(&svg);
        // Every edge starts on its own source box and ends on its own
        // target box.
        let l = layout(&p);
        let at = |id: &str| {
            let b = l.nodes.iter().find(|n| n.id == id).unwrap();
            (b.x, b.y)
        };
        for (e, chunk) in p.edges.iter().zip(edge_chunks(&svg)) {
            let pts = path_points(path_d(chunk));
            let (first, last) = (pts[0], pts[pts.len() - 1]);
            assert!(
                near(first, at(&e.from)),
                "{} -> {}: {first:?}",
                e.from,
                e.to
            );
            assert!(near(last, at(&e.to)), "{} -> {}: {last:?}", e.from, e.to);
        }
    }

    /// The loop-and-judge shape of a real report: four lanes, two of them
    /// (`work -> done`, `audit -> done`) into one target with the same
    /// attach side, each in its own channel height.
    #[test]
    fn four_lanes_into_one_target_keep_their_runs_apart() {
        const LJ: &str = "schema: 2\nid: lj4\nname: LJ4\nversion: 1.0.0\nnodes:\n  - { id: s, type: start }\n  - { id: work, type: script, script: work.sh, runner: sh }\n  - { id: check, type: script, script: work.sh, runner: sh }\n  - { id: triage, type: script, script: work.sh, runner: sh }\n  - { id: audit, type: script, script: work.sh, runner: sh }\n  - { id: hold, type: script, script: work.sh, runner: sh }\n  - { id: done, type: finish, outcome: success }\nedges:\n  - { from: s, to: work }\n  - { from: work, to: done }\n  - { from: work, to: check }\n  - { from: check, to: work }\n  - { from: check, to: triage }\n  - { from: check, to: audit }\n  - { from: triage, to: done }\n  - { from: triage, to: hold }\n  - { from: audit, to: done }\n  - { from: hold, to: done }\n";
        let p = pb(LJ);
        let svg = draw(&w(), &p, &BTreeMap::new(), &BTreeMap::new()).into_string();
        assert_eq!(lanes_of(&p, &ranks_of(&p, &back_edges_of(&p))).len(), 4);
        assert_edges_clear_of_boxes(&svg);
        assert_no_shared_runs(&svg);
    }

    /// The `d` attribute of one `<path class="graph-edge ...">` chunk.
    fn path_d(chunk: &str) -> &str {
        let d_at = chunk.find(" d=\"").unwrap() + 4;
        let d_end = chunk[d_at..].find('"').unwrap() + d_at;
        &chunk[d_at..d_end]
    }

    /// The edge path chunks of one drawn graph, in definition order.
    fn edge_chunks(svg: &str) -> Vec<&str> {
        svg.split("<path class=\"graph-edge ")
            .skip(1)
            .map(|c| &c[..c.find("></path>").unwrap()])
            .collect()
    }

    /// The polyline points of an edge path (`M x y L x y ...`).
    fn path_points(d: &str) -> Vec<(f64, f64)> {
        let nums: Vec<f64> = d
            .split(|c: char| !(c.is_ascii_digit() || c == '.' || c == '-'))
            .filter(|s| !s.is_empty())
            .map(|s| s.parse().unwrap())
            .collect();
        nums.chunks(2).map(|c| (c[0], c[1])).collect()
    }

    /// No two edges draw a horizontal stretch over each other: at one
    /// height their x ranges meet at most in a point.
    fn assert_no_shared_runs(svg: &str) {
        let mut runs: Vec<(usize, f64, f64, f64)> = Vec::new();
        for (i, chunk) in edge_chunks(svg).into_iter().enumerate() {
            for seg in path_points(path_d(chunk)).windows(2) {
                if seg[0].1 == seg[1].1 && seg[0].0 != seg[1].0 {
                    let (a, b) = (seg[0].0.min(seg[1].0), seg[0].0.max(seg[1].0));
                    runs.push((i, seg[0].1, a, b));
                }
            }
        }
        for (n, r) in runs.iter().enumerate() {
            for q in &runs[n + 1..] {
                if r.0 != q.0 && r.1 == q.1 {
                    assert!(
                        r.3 <= q.2 || q.3 <= r.2,
                        "edges {} and {} share the run at y {}: {svg}",
                        r.0,
                        q.0,
                        r.1
                    );
                }
            }
        }
    }

    /// The largest x of a polyline.
    fn widest_x(pts: &[(f64, f64)]) -> f64 {
        pts.iter().fold(f64::MIN, |m, p| m.max(p.0))
    }

    /// Every straight segment of every drawn edge path stays out of the
    /// boxes it does not belong to: only the path's own source and target
    /// box may be touched.
    fn assert_edges_clear_of_boxes(svg: &str) {
        let mut boxes: Vec<(usize, usize)> = Vec::new();
        for chunk in svg.split("<g class=\"graph-node ").skip(1) {
            let chunk = &chunk[..chunk.find("</g>").unwrap()];
            let tx = chunk.find("translate(").unwrap() + "translate(".len();
            let nums: Vec<usize> = chunk[tx..]
                .split(|c: char| !c.is_ascii_digit())
                .filter(|s| !s.is_empty())
                .take(2)
                .map(|s| s.parse().unwrap())
                .collect();
            boxes.push((nums[0], nums[1]));
        }
        for chunk in edge_chunks(svg) {
            let pts = path_points(path_d(chunk));
            for seg in pts.windows(2) {
                let (from, to) = ((seg[0].0, seg[0].1), (seg[1].0, seg[1].1));
                for at in &boxes {
                    if segment_hits_box(from, to, *at) {
                        assert!(
                            near(from, *at) || near(to, *at),
                            "segment {from:?}-{to:?} of {} crosses the box at {at:?}",
                            path_d(chunk)
                        );
                    }
                }
            }
        }
    }

    /// Whether `p` sits inside the box at `at` (its border region).
    fn near(p: (f64, f64), at: (usize, usize)) -> bool {
        let (bx, by) = (at.0 as f64, at.1 as f64);
        p.0 >= bx - 1.0
            && p.0 <= bx + NODE_W as f64 + 1.0
            && p.1 >= by - 1.0
            && p.1 <= by + NODE_H as f64 + 1.0
    }

    #[test]
    fn an_output_field_route_carries_its_field_and_value() {
        const ROUTED: &str = "schema: 2\nid: r\nname: R\nversion: 1.0.0\nnodes:\n  - { id: s, type: start }\n  - { id: check, type: script, script: work.sh, runner: sh }\n  - { id: again, type: script, script: work.sh, runner: sh }\n  - { id: stop, type: finish, outcome: success, prompt: sum up }\nedges:\n  - { from: s, to: check }\n  - { from: check, to: again, condition: { type: output_field, node: check, field: verdict, equals: again } }\n  - { from: check, to: stop, condition: { type: output_field, node: check, field: verdict, equals: done } }\n";
        let svg = draw(&w(), &pb(ROUTED), &BTreeMap::new(), &BTreeMap::new()).into_string();
        assert!(svg.contains("verdict: again"), "{svg}");
        assert!(svg.contains("verdict: done"), "{svg}");
        assert!(svg.contains("class=\"graph-elabel\""), "{svg}");
    }

    #[test]
    fn an_interrupted_node_reads_as_running_not_failed() {
        let mut statuses = BTreeMap::new();
        statuses.insert("s".to_string(), "interrupted".to_string());
        let svg = draw(&w(), &pb(CHAIN), &statuses, &BTreeMap::new()).into_string();
        let s = node_chunk(&svg, "s");
        assert!(s.contains("graph-st-running"), "{s}");
        assert!(!s.contains("graph-st-failed"), "{s}");
    }

    #[test]
    fn the_legend_dots_carry_the_base_class() {
        let out = draw(&w(), &pb(CHAIN), &BTreeMap::new(), &BTreeMap::new()).into_string();
        assert!(out.contains("graph-dot graph-dot-succeeded"), "{out}");
        assert!(out.contains("graph-dot graph-dot-norun"), "{out}");
        assert!(crate::report_html::STYLESHEET.contains(".graph-dot-norun"));
    }

    #[test]
    fn a_wide_graph_scrolls_instead_of_shrinking() {
        let out = draw(&w(), &pb(CHAIN), &BTreeMap::new(), &BTreeMap::new()).into_string();
        assert!(
            out.contains("<div class=\"graph-scroll\"><svg class=\"graph-svg\""),
            "{out}"
        );
        let css = crate::report_html::STYLESHEET;
        let at = css.find(".graph-scroll").expect("a scroll rule exists");
        assert!(css[at..].contains("overflow-x: auto"), "{css}");
        // Scoped to the `.graph-svg` rule body: an unrelated rule using
        // `max-width: 100%` elsewhere must not fail this test.
        let svg_at = css.find(".graph-svg").expect("a graph svg rule exists");
        let body = &css[svg_at..svg_at + css[svg_at..].find('}').unwrap()];
        assert!(!body.contains("max-width: 100%"), "{body}");
    }

    #[test]
    fn an_edge_with_an_unknown_endpoint_is_skipped_not_a_crash() {
        const GHOST: &str = "schema: 2\nid: gh\nname: H\nversion: 1.0.0\nnodes:\n  - { id: s, type: start }\n  - { id: done, type: finish, outcome: success, prompt: sum up }\nedges:\n  - { from: s, to: done }\n  - { from: s, to: ghost }\n  - { from: ghost, to: done }\n";
        let p = pb(GHOST);
        let svg = draw(&w(), &p, &BTreeMap::new(), &BTreeMap::new()).into_string();
        assert_eq!(svg.matches("class=\"graph-edge ").count(), 1, "{svg}");
        assert_eq!(svg.matches("<g class=\"graph-node ").count(), 2, "{svg}");
    }

    /// A chain of `n` nodes: start, n-2 work nodes, done.
    fn big(n: usize) -> String {
        let mut yaml = String::from(
            "schema: 2\nid: big\nname: Big\nversion: 1.0.0\nnodes:\n  - { id: s, type: start }\n",
        );
        for i in 0..n - 2 {
            yaml.push_str(&format!(
                "  - {{ id: n{i}, type: script, script: work.sh, runner: sh }}\n"
            ));
        }
        yaml.push_str("  - { id: done, type: finish, outcome: success, prompt: sum up }\nedges:\n  - { from: s, to: n0 }\n");
        for i in 1..n - 2 {
            yaml.push_str(&format!("  - {{ from: n{}, to: n{} }}\n", i - 1, i));
        }
        yaml.push_str(&format!("  - {{ from: n{}, to: done }}\n", n - 3));
        yaml
    }
}
