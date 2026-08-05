//! Structural rules over the node graph: identity, entry and exit points,
//! edge targets, reachability, cycles and loop bounds, and the first-match
//! routing rules a set of outgoing edges has to satisfy.

use super::*;
use crate::graphutil::{adjacency, reachable_from, sccs};

pub(crate) fn check_unique_ids(playbook: &Playbook, r: &mut ValidationReport) {
    let mut seen = HashSet::new();
    for n in &playbook.nodes {
        if !seen.insert(n.id.as_str()) {
            r.error("V01", Some(&n.id), format!("duplicate node id `{}`", n.id));
        }
    }
    let mut pseen = HashSet::new();
    for p in &playbook.params {
        if !pseen.insert(p.name.as_str()) {
            r.error("V02", None, format!("duplicate param name `{}`", p.name));
        }
    }
}

pub(crate) fn check_start_finish(playbook: &Playbook, r: &mut ValidationReport) {
    let starts: Vec<_> = playbook
        .nodes
        .iter()
        .filter(|n| matches!(n.kind, NodeKind::Start))
        .collect();
    if starts.len() != 1 {
        r.error(
            "V03",
            None,
            format!("expected exactly one start node, found {}", starts.len()),
        );
    }
    for e in &playbook.edges {
        if let Some(to) = playbook.node(&e.to)
            && matches!(to.kind, NodeKind::Start)
        {
            r.error(
                "V04",
                Some(&e.to),
                "start node must not have incoming edges".into(),
            );
        }
        if let Some(from) = playbook.node(&e.from)
            && matches!(from.kind, NodeKind::Finish { .. })
        {
            r.error(
                "V05",
                Some(&e.from),
                "finish node must not have outgoing edges".into(),
            );
        }
    }
}

pub(crate) fn check_edges_exist(playbook: &Playbook, r: &mut ValidationReport) {
    for e in &playbook.edges {
        for id in [&e.from, &e.to] {
            if playbook.node(id).is_none() {
                r.error(
                    "V06",
                    Some(id),
                    format!("edge references unknown node `{id}`"),
                );
            }
        }
    }
}

/// V35: `defaults.on_failure`, when it names a node rather than `route` or
/// `stop`, must name one that exists and can actually receive a failure. This
/// is also what catches a misspelled reserved word, since anything that is not
/// `route` or `stop` parses as a node id.
pub(crate) fn check_failure_policy(playbook: &Playbook, r: &mut ValidationReport) {
    let FailurePolicy::Node(target) = &playbook.defaults.on_failure else {
        return;
    };
    match playbook.node(target) {
        None => r.error(
            "V35",
            None,
            format!(
                "defaults.on_failure names unknown node `{target}` (expected a node id, `route` or `stop`)"
            ),
        ),
        Some(node) if matches!(node.kind, NodeKind::Start) => r.error(
            "V35",
            Some(target),
            "defaults.on_failure must not target the start node".into(),
        ),
        Some(_) => {}
    }
}

pub(crate) fn check_reachability(playbook: &Playbook, r: &mut ValidationReport) {
    let Some(start) = playbook
        .nodes
        .iter()
        .find(|n| matches!(n.kind, NodeKind::Start))
    else {
        return;
    };
    let adj = adjacency(playbook);
    let mut seen = HashSet::new();
    let mut q = VecDeque::from([start.id.as_str()]);
    // The failure policy is a route like any other, it just has no edge drawn
    // for it: without this the handler a playbook points every unhandled
    // failure at would read as unreachable the moment its last incoming edge
    // is deleted, which is exactly what the policy exists to allow.
    if let FailurePolicy::Node(target) = &playbook.defaults.on_failure
        && playbook.node(target).is_some()
    {
        q.push_back(target.as_str());
    }
    while let Some(id) = q.pop_front() {
        if seen.insert(id) {
            for next in adj.get(id).into_iter().flatten() {
                q.push_back(next);
            }
        }
    }
    for n in &playbook.nodes {
        if !seen.contains(n.id.as_str()) {
            r.error(
                "V07",
                Some(&n.id),
                format!("node `{}` is unreachable from start", n.id),
            );
        }
    }
    // V08: from every reachable node some finish node must be reachable (otherwise warning)
    let finishes: HashSet<&str> = playbook
        .nodes
        .iter()
        .filter(|n| matches!(n.kind, NodeKind::Finish { .. }))
        .map(|n| n.id.as_str())
        .collect();
    for n in &playbook.nodes {
        if !seen.contains(n.id.as_str()) {
            continue;
        }
        let mut vis = HashSet::new();
        let mut q = VecDeque::from([n.id.as_str()]);
        let mut ok = false;
        while let Some(id) = q.pop_front() {
            if finishes.contains(id) {
                ok = true;
                break;
            }
            if vis.insert(id) {
                for next in adj.get(id).into_iter().flatten() {
                    q.push_back(next);
                }
            }
        }
        if !ok {
            r.warn(
                "V08",
                Some(&n.id),
                format!("no path from `{}` to any finish node", n.id),
            );
        }
    }
}

pub(crate) fn check_conditions(playbook: &Playbook, r: &mut ValidationReport) {
    let adj = adjacency(playbook);
    for n in &playbook.nodes {
        if !matches!(n.kind, NodeKind::Condition { .. }) {
            continue;
        }
        let out: Vec<_> = playbook.edges.iter().filter(|e| e.from == n.id).collect();
        let has_fallback = out.iter().any(|e| e.fallback);
        // V09: node_status branches must cover success and failure (or declare a fallback)
        let mut covered = HashSet::new();
        for e in &out {
            if let Some(EdgeCondition::NodeStatus { equals, .. }) = &e.condition {
                covered.insert(*equals);
            }
        }
        let uses_node_status = out
            .iter()
            .any(|e| matches!(e.condition, Some(EdgeCondition::NodeStatus { .. })));
        if uses_node_status && covered.len() < 2 && !has_fallback {
            r.error(
                "V09",
                Some(&n.id),
                "condition edges must cover both success and failure or declare a fallback edge"
                    .into(),
            );
        }
        // V10: a condition may only reference nodes from which this condition node is reachable
        for e in &out {
            let referenced = match &e.condition {
                Some(EdgeCondition::NodeStatus { node, .. }) => Some(node),
                Some(EdgeCondition::OutputMatch { node, .. }) => Some(node),
                // Reads one field of the source node's output, so it needs the
                // same source node to exist and to be able to run first.
                Some(EdgeCondition::OutputField { node, .. }) => Some(node),
                _ => None,
            };
            if let Some(dep) = referenced {
                let ok = playbook.node(dep).is_some()
                    && reachable_from(&adj, dep.as_str()).contains(n.id.as_str());
                if !ok {
                    r.error(
                        "V10",
                        Some(&n.id),
                        format!(
                            "condition references node `{dep}` that cannot execute before `{}`",
                            n.id
                        ),
                    );
                }
            }
        }
    }
}

pub(crate) fn check_cycles(playbook: &Playbook, r: &mut ValidationReport) {
    // Every cycle must pass through a condition node with max_loops.
    // It's enough to check the SCCs: a component with a cycle must contain such
    // a node. The components come from the shared `graphutil` pass, the same one
    // the engine consults to tell an acyclic fan-in from a cycle merge point.
    let self_loop: HashSet<&str> = playbook
        .edges
        .iter()
        .filter(|e| e.from == e.to)
        .map(|e| e.from.as_str())
        .collect();
    for comp in sccs(playbook) {
        let cyclic = comp.len() > 1 || self_loop.contains(comp[0].as_str());
        if !cyclic {
            continue;
        }
        let members: HashSet<&str> = comp.iter().map(String::as_str).collect();
        // A cycle is bounded when it passes through a condition node with
        // max_loops OR contains at least one edge (both endpoints inside the
        // component) carrying max_traversals. Either guard makes the loop
        // terminate, so V11 only fires when neither is present.
        let has_max_loops = comp.iter().any(|id| {
            matches!(
                playbook.node(id).map(|n| &n.kind),
                Some(NodeKind::Condition { max_loops: Some(_) })
            )
        });
        let has_bounded_edge = playbook.edges.iter().any(|e| {
            e.max_traversals.is_some()
                && members.contains(e.from.as_str())
                && members.contains(e.to.as_str())
        });
        if !has_max_loops && !has_bounded_edge {
            r.error(
                "V11",
                Some(&comp[0]),
                format!(
                    "cycle [{}] must contain an edge with max_traversals or pass through a condition node with max_loops",
                    comp.join(", ")
                ),
            );
        }
    }
}

/// V36 (error): an `Edge.join` value other than `all` or `any`. The engine
/// parses the field leniently (anything that is not `any` means `all`, see
/// `apb_engine::parallel::JoinMode::parse`), so without this rule a typo like
/// `join: al` silently becomes a wait-for-all barrier. Parsing stays lenient on
/// purpose: a stored run snapshot with a legacy value must keep loading, so the
/// value space is guarded at validation time rather than at deserialization.
///
/// V37 (warning): the incoming edges of one node disagree on the join mode. The
/// engine takes the first `join` in file order and ignores the rest
/// (`apb_engine::parallel::join_mode`), so a mixed fan-in means the author's
/// intent for the later edges is silently dropped.
pub(crate) fn check_joins(playbook: &Playbook, r: &mut ValidationReport) {
    for e in &playbook.edges {
        if let Some(join) = &e.join
            && !matches!(join.as_str(), "all" | "any")
        {
            r.error(
                "V36",
                Some(&e.to),
                format!(
                    "edge `{}` -> `{}` has join `{join}`, expected `all` or `any`",
                    e.from, e.to
                ),
            );
        }
    }
    // Declared modes per target node, in file order. Only well-formed values
    // take part: a value V36 already rejected must not also read as a mix.
    let mut by_target: HashMap<&str, Vec<&str>> = HashMap::new();
    for e in &playbook.edges {
        if let Some(join) = e.join.as_deref()
            && matches!(join, "all" | "any")
        {
            by_target.entry(e.to.as_str()).or_default().push(join);
        }
    }
    // Sorted so the report order is stable regardless of hash iteration order.
    let mut targets: Vec<(&str, Vec<&str>)> = by_target.into_iter().collect();
    targets.sort_unstable_by_key(|(id, _)| *id);
    for (target, modes) in targets {
        let mut distinct = modes.clone();
        distinct.sort_unstable();
        distinct.dedup();
        if distinct.len() < 2 {
            continue;
        }
        let winner = modes.first().copied().unwrap_or("all");
        r.warn(
            "V37",
            Some(target),
            format!(
                "incoming edges of `{target}` mix join modes [{}]; the engine takes the first one in file order (`{winner}`) and ignores the rest",
                distinct.join(", ")
            ),
        );
    }
}

/// Whether `node` waits for EVERY incoming branch before it executes, mirroring
/// `apb_engine::parallel::join_kind` plus `join_mode`: an explicit `join` that is
/// not `any` is a wait-for-all barrier, and a fan-in with no `join` at all is the
/// implicit barrier an acyclic fan-in forms. A merge point inside its own cycle
/// (a back edge among its inputs) keeps first-arrival semantics, so it does not
/// wait; neither does a `join: any`.
fn waits_for_all_inputs<'a>(
    node: &'a str,
    adj: &HashMap<&'a str, Vec<&'a str>>,
    incoming: &[&crate::schema::Edge],
) -> bool {
    if incoming.len() < 2 {
        return false;
    }
    if let Some(mode) = incoming.iter().find_map(|e| e.join.as_deref()) {
        // Lenient exactly like the engine: only `any` is first-arrival.
        return mode != "any";
    }
    let downstream = reachable_from(adj, node);
    incoming
        .iter()
        .all(|e| !downstream.contains(e.from.as_str()))
}

/// For every node, the nodes the graph GUARANTEES have finished before it runs.
///
/// A must-analysis over the node graph:
///
/// * a node with a single incoming edge inherits its source's set plus the
///   source itself (a linear chain accumulates);
/// * a node that waits for every input ([`waits_for_all_inputs`]) takes the
///   UNION over its inputs, since all of them have to land before it starts;
/// * any other multi-input node (a `join: any`, or a cycle merge point) takes
///   the INTERSECTION, since first arrival is enough to start it.
///
/// Entry nodes (no incoming edge) start from the empty set, every other node
/// from the full node set, and the iteration shrinks to a fixed point: the
/// standard maximal-fixed-point form of a must-analysis over a graph with
/// cycles. Values only ever shrink, so stopping the iteration early can only
/// leave an over-approximation, which keeps the rules built on this answer
/// conservative (a missed warning, never a false one).
///
/// Note that the union at a wait-for-all barrier is deliberately optimistic
/// about conditional routing: after an either-or fork only the taken branch
/// really ran (the join fires because the other one is dead, see
/// `apb_engine::parallel::arrival`), yet both count as finished here. That is
/// what keeps the common either-or merge reading both branches out of V38.
pub(crate) fn must_have_finished(playbook: &Playbook) -> HashMap<&str, HashSet<&str>> {
    let ids: Vec<&str> = playbook.nodes.iter().map(|n| n.id.as_str()).collect();
    let known: HashSet<&str> = ids.iter().copied().collect();
    let adj = adjacency(playbook);
    let mut incoming: HashMap<&str, Vec<&crate::schema::Edge>> = HashMap::new();
    for e in &playbook.edges {
        incoming.entry(e.to.as_str()).or_default().push(e);
    }
    let waits: HashSet<&str> = ids
        .iter()
        .copied()
        .filter(|id| {
            waits_for_all_inputs(id, &adj, incoming.get(id).map(Vec::as_slice).unwrap_or(&[]))
        })
        .collect();
    let mut done: HashMap<&str, HashSet<&str>> = ids
        .iter()
        .copied()
        .map(|id| {
            let seed = match incoming.get(id).is_some_and(|v| !v.is_empty()) {
                true => known.clone(),
                false => HashSet::new(),
            };
            (id, seed)
        })
        .collect();
    // Values shrink monotonically, so the fixed point is reached in at most one
    // pass per node; the cap is a guard, not the termination argument.
    for _ in 0..ids.len() + 2 {
        let mut changed = false;
        for id in &ids {
            let Some(inc) = incoming.get(id).filter(|v| !v.is_empty()) else {
                continue;
            };
            let union = waits.contains(id);
            let mut next: Option<HashSet<&str>> = None;
            for e in inc {
                let source = e.from.as_str();
                let mut arrived: HashSet<&str> = done.get(source).cloned().unwrap_or_default();
                if known.contains(source) {
                    arrived.insert(source);
                }
                next = Some(match next {
                    None => arrived,
                    Some(acc) => match union {
                        true => acc.union(&arrived).copied().collect(),
                        false => acc.intersection(&arrived).copied().collect(),
                    },
                });
            }
            let Some(next) = next else { continue };
            if done.get(id) != Some(&next) {
                done.insert(id, next);
                changed = true;
            }
        }
        if !changed {
            break;
        }
    }
    done
}

/// V30 (error): a `max_traversals` of 0 on an edge. A bounded edge that can
/// never be traversed is an authoring mistake; the minimum useful cap is 1.
///
/// V34 (error): two edges from the same node whose first-match routing keys are
/// structurally identical but whose targets differ. Under first-match routing
/// (see `apb_engine::parallel::selected_edges`) only one of those targets is
/// ever chosen, so the graph is contradictory. The routing key is:
///
///   * a non-fallback edge with a condition: the full condition (type +
///     parameters), compared structurally;
///   * a fallback edge (`fallback: true`): all fallbacks from a node share one
///     key, since selection takes the first fallback when nothing else matches.
///
/// Unconditional non-fallback edges are deliberately excluded: several of them
/// from one node are parallel fan-out (join:any / join:all), not first-match.
/// Two edges with identical keys and the same target are redundant but not
/// contradictory, so they are allowed.
///
/// Also V34: an unconditional non-fallback edge from a node makes every
/// conditional non-fallback edge from that same node unreachable (selection
/// returns the unconditional set as soon as it is non-empty). Flagged because
/// first-match routing makes the outcome precise and predictable.
pub(crate) fn check_edges(playbook: &Playbook, r: &mut ValidationReport) {
    for e in &playbook.edges {
        if e.max_traversals == Some(0) {
            r.error("V30", None, "max_traversals must be at least 1".to_string());
        }
    }
    check_duplicate_route_edges(playbook, r);
}

/// Structural identity of an edge's first-match routing key. See [`check_edges`].
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) enum RouteKey<'a> {
    /// Non-fallback edge with a condition; first matching wins.
    Conditional(&'a EdgeCondition),
    /// Fallback edge; first fallback wins when nothing else matches.
    Fallback,
}

pub(crate) fn route_key(e: &crate::schema::Edge) -> Option<RouteKey<'_>> {
    if e.fallback {
        return Some(RouteKey::Fallback);
    }
    e.condition.as_ref().map(RouteKey::Conditional)
}

pub(crate) fn describe_route_key(key: &RouteKey<'_>) -> String {
    match key {
        RouteKey::Fallback => "fallback".to_string(),
        RouteKey::Conditional(EdgeCondition::NodeStatus { node, equals }) => {
            format!("node_status node=`{node}` equals={equals:?}")
        }
        RouteKey::Conditional(EdgeCondition::ReviewStatus { equals }) => {
            format!("review_status equals=`{equals}`")
        }
        RouteKey::Conditional(EdgeCondition::OutputMatch { node, pattern }) => {
            format!("output_match node=`{node}` pattern=`{pattern}`")
        }
        // The field is part of the routing key: two edges reading DIFFERENT
        // fields of the same node are distinct routes, so only an identical
        // node/field/value triple is a duplicate.
        RouteKey::Conditional(EdgeCondition::OutputField {
            node,
            field,
            equals,
        }) => {
            format!("output_field node=`{node}` field=`{field}` equals=`{equals}`")
        }
    }
}

pub(crate) fn check_duplicate_route_edges(playbook: &Playbook, r: &mut ValidationReport) {
    // Group edges by source for a single pass per node.
    let mut by_from: HashMap<&str, Vec<&crate::schema::Edge>> = HashMap::new();
    for e in &playbook.edges {
        by_from.entry(e.from.as_str()).or_default().push(e);
    }
    for (from, outs) in by_from {
        // Unconditional non-fallback edges shadow every conditional edge under
        // first-match routing (selected_edges returns the unconditional set
        // wholesale and never consults conditions).
        let has_unconditional = outs.iter().any(|e| e.condition.is_none() && !e.fallback);
        let shadowed: Vec<&&crate::schema::Edge> = outs
            .iter()
            .filter(|e| e.condition.is_some() && !e.fallback)
            .collect();
        if has_unconditional && !shadowed.is_empty() {
            let targets: Vec<&str> = shadowed.iter().map(|e| e.to.as_str()).collect();
            r.error(
                "V34",
                Some(from),
                format!(
                    "unconditional edge from `{from}` makes conditional edge(s) to [{}] unreachable under first-match routing",
                    targets.join(", ")
                ),
            );
        }

        // Identical first-match keys with different targets.
        let mut groups: HashMap<RouteKey<'_>, Vec<&str>> = HashMap::new();
        for e in &outs {
            let Some(key) = route_key(e) else {
                // Unconditional non-fallback: parallel fan-out, not first-match.
                continue;
            };
            groups.entry(key).or_default().push(e.to.as_str());
        }
        for (key, targets) in groups {
            let mut unique: Vec<&str> = targets.clone();
            unique.sort_unstable();
            unique.dedup();
            if unique.len() <= 1 {
                // Same target (or a single edge): redundant at worst, not contradictory.
                continue;
            }
            r.error(
                "V34",
                Some(from),
                format!(
                    "contradictory edges from `{from}` with identical condition ({}) to different targets: [{}]",
                    describe_route_key(&key),
                    unique.join(", ")
                ),
            );
        }
    }
}
