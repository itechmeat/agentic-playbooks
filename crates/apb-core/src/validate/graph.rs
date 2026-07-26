//! Structural rules over the node graph: identity, entry and exit points,
//! edge targets, reachability, cycles and loop bounds, and the first-match
//! routing rules a set of outgoing edges has to satisfy.

use super::*;

pub(crate) fn adjacency(playbook: &Playbook) -> HashMap<&str, Vec<&str>> {
    let mut adj: HashMap<&str, Vec<&str>> = HashMap::new();
    for n in &playbook.nodes {
        adj.entry(n.id.as_str()).or_default();
    }
    for e in &playbook.edges {
        adj.entry(e.from.as_str()).or_default().push(e.to.as_str());
    }
    adj
}

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

pub(crate) fn reachable_from<'a>(
    adj: &HashMap<&'a str, Vec<&'a str>>,
    from: &'a str,
) -> HashSet<&'a str> {
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
    // It's enough to check the SCCs: a component with a cycle must contain such a node.
    let ids: Vec<&str> = playbook.nodes.iter().map(|n| n.id.as_str()).collect();
    let adj = adjacency(playbook);
    // iterative Tarjan
    let index_of: HashMap<&str, usize> = ids.iter().enumerate().map(|(i, s)| (*s, i)).collect();
    let n = ids.len();
    let mut index = vec![usize::MAX; n];
    let mut low = vec![0usize; n];
    let mut on_stack = vec![false; n];
    let mut stack: Vec<usize> = Vec::new();
    let mut counter = 0usize;
    let mut sccs: Vec<Vec<usize>> = Vec::new();

    for root in 0..n {
        if index[root] != usize::MAX {
            continue;
        }
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
                        comp.push(w);
                        if w == v {
                            break;
                        }
                    }
                    sccs.push(comp);
                }
                call.pop();
                if let Some(&(parent, _)) = call.last() {
                    low[parent] = low[parent].min(low[v]);
                }
            }
        }
    }

    let self_loop: HashSet<&str> = playbook
        .edges
        .iter()
        .filter(|e| e.from == e.to)
        .map(|e| e.from.as_str())
        .collect();
    for comp in sccs {
        let cyclic = comp.len() > 1 || self_loop.contains(ids[comp[0]]);
        if !cyclic {
            continue;
        }
        let members: HashSet<&str> = comp.iter().map(|&i| ids[i]).collect();
        // A cycle is bounded when it passes through a condition node with
        // max_loops OR contains at least one edge (both endpoints inside the
        // component) carrying max_traversals. Either guard makes the loop
        // terminate, so V11 only fires when neither is present.
        let has_max_loops = comp.iter().any(|&i| {
            matches!(
                playbook.node(ids[i]).map(|n| &n.kind),
                Some(NodeKind::Condition { max_loops: Some(_) })
            )
        });
        let has_bounded_edge = playbook.edges.iter().any(|e| {
            e.max_traversals.is_some()
                && members.contains(e.from.as_str())
                && members.contains(e.to.as_str())
        });
        if !has_max_loops && !has_bounded_edge {
            let member_list: Vec<&str> = comp.iter().map(|&i| ids[i]).collect();
            r.error(
                "V11",
                Some(member_list[0]),
                format!(
                    "cycle [{}] must contain an edge with max_traversals or pass through a condition node with max_loops",
                    member_list.join(", ")
                ),
            );
        }
    }
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
