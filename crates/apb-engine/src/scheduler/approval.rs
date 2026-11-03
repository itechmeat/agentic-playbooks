//! The drive side of approvals ([`crate::approval`]): binding a gate's
//! request to the latest result snapshots of the nodes it `approves`,
//! re-verifying them when a decision is consumed, and the check before a
//! node with `requires_approval` starts ([`check_before`]).

use std::path::Path;

use apb_core::result_store::{EntryPart, SnapshotManifest};
use apb_core::schema::{Approves, Node, NodeKind, Playbook};

use crate::approval::{
    ApprovalFailure, ApprovalRefusal, BindingMissing, BindingMissingReason, BoundSnapshot,
    ReviewBinding, binding_nonce,
};
use crate::event::{Event, EventPayload};

/// The binding of the request gate `gate` is about to declare: for each
/// node `approves` names, the snapshot of its latest execution (for a list
/// node also the item snapshots of that execution), or why there is none.
/// `visit` is the gate's request count plus one, known before the request
/// is appended.
pub(crate) fn bind_request(
    run_dir: &Path,
    gate: &str,
    approves: &Approves,
    events: &[Event],
) -> ReviewBinding {
    let index = crate::identity::index(events);
    let mut snapshots = Vec::new();
    let mut missing = Vec::new();
    for node in &approves.results {
        let Some(last) = index.executions(node).iter().rev().find(|x| !x.is_open()) else {
            missing.push(BindingMissing {
                node: node.clone(),
                reason: BindingMissingReason::NotRun,
            });
            continue;
        };
        let bound = bound_in(run_dir, events, node, last);
        if !bound.iter().any(|b| b.item.is_none()) {
            missing.push(BindingMissing {
                node: node.clone(),
                reason: BindingMissingReason::NoSnapshot,
            });
            continue;
        }
        snapshots.extend(bound);
    }
    let visit = crate::event::review_requested_count(events, gate) as u32 + 1;
    let run_id = run_dir.file_name().and_then(|n| n.to_str()).unwrap_or("");
    ReviewBinding {
        nonce: binding_nonce(run_id, gate, visit, &snapshots, &missing),
        snapshots,
        missing,
    }
}

/// Every stamped settlement of one execution of `node`: its item
/// snapshots and its own, in journal order.
fn bound_in(
    run_dir: &Path,
    events: &[Event],
    node: &str,
    x: &crate::identity::Execution,
) -> Vec<BoundSnapshot> {
    let end = x.end_seq.unwrap_or(u64::MAX);
    events
        .iter()
        .filter(|e| e.seq >= x.begin_seq && e.seq <= end)
        .filter_map(|e| {
            let (item, stamp) = match &e.payload {
                EventPayload::NodeFinished {
                    node: n,
                    result: Some(stamp),
                    ..
                } if n == node => (None, stamp),
                EventPayload::ListItemSettled {
                    node: n,
                    item,
                    result: Some(stamp),
                    ..
                } if n == node => (Some(item.clone()), stamp),
                _ => return None,
            };
            let redacted = crate::result_view::verified_manifest(run_dir, &stamp.snapshot_id)
                .is_ok_and(|m| m.entries.iter().any(|en| en.redacted));
            Some(BoundSnapshot {
                node: node.to_string(),
                execution: x.ordinal,
                item,
                settlement_seq: e.seq,
                snapshot_id: stamp.snapshot_id.clone(),
                omitted: stamp.omitted,
                incomplete: stamp.incomplete,
                redacted,
            })
        })
        .collect()
}

/// One line for the request's instruction naming what an approval binds,
/// so a reader of the instruction alone knows which bytes it answers and
/// which nonce an approving decision carries.
pub(crate) fn binding_line(b: &ReviewBinding) -> String {
    let mut parts: Vec<String> = b
        .snapshots
        .iter()
        .map(|s| {
            let what = match &s.item {
                Some(item) => format!("{} item {item}", s.node),
                None => s.node.clone(),
            };
            format!(
                "{what} (execution {}, snapshot {})",
                s.execution,
                &s.snapshot_id[..s.snapshot_id.len().min(12)]
            )
        })
        .collect();
    parts.extend(
        b.missing
            .iter()
            .map(|m| format!("{} (nothing bound: {})", m.node, m.reason.as_str())),
    );
    format!(
        " Approving binds the result snapshots of {}; an approving decision carries nonce `{}`.",
        parts.join(", "),
        b.nonce
    )
}

/// Re-verifies every snapshot of a binding (manifest and every object
/// re-hashed): the check the drive makes before it journals a decision on
/// a bound request, so nothing is decided on content apb can no longer
/// show.
pub(crate) fn verify_binding(run_dir: &Path, b: &ReviewBinding) -> Result<(), ApprovalFailure> {
    for s in &b.snapshots {
        if crate::result_view::verified_manifest(run_dir, &s.snapshot_id).is_err() {
            return Err(ApprovalFailure {
                refusal: ApprovalRefusal::ApprovalSnapshotUnreadable,
                detail: format!("snapshot {} of node `{}`", s.snapshot_id, s.node),
            });
        }
    }
    Ok(())
}

/// The approval a node with `requires_approval` needs, checked when the
/// drive admits it, before `node_started` and before any attempt: the
/// gate's latest decision approves a binding that covers every required
/// node, nothing of it ran again since, the bound snapshots verify and are
/// complete, and (`live: check`) every bound file still hashes to its
/// approved digest. `Ok` for a node without the declaration. An
/// unattended run's declared decision and an enforced recommendation count
/// like a person's: the journal labels them with `decided_by`.
pub fn check_before(
    run_dir: &Path,
    playbook: &Playbook,
    node: &Node,
    events: &[Event],
) -> Result<(), ApprovalFailure> {
    let Some(required) = &node.requires_approval else {
        return Ok(());
    };
    let gate_id = required.gate.as_str();
    let missing = |detail: String| ApprovalFailure {
        refusal: ApprovalRefusal::ApprovalMissing,
        detail,
    };
    let gate = playbook.node(gate_id);
    let (Some(approves), Some(NodeKind::HumanReview { options, .. })) = (
        gate.and_then(|g| g.approves.as_ref()),
        gate.map(|g| &g.kind),
    ) else {
        return Err(missing(format!("gate `{gate_id}` approves no result")));
    };
    let decided = events.iter().rev().find_map(|e| match &e.payload {
        EventPayload::ReviewDecided {
            node: n,
            decision,
            binding,
            ..
        } if n == gate_id => Some((decision, binding)),
        _ => None,
    });
    let Some((decision, binding)) = decided else {
        return Err(missing(format!("gate `{gate_id}` has no decision")));
    };
    if !approves.approving_options(options).contains(decision) {
        return Err(missing(format!(
            "gate `{gate_id}` decided `{decision}`, which does not approve"
        )));
    }
    let Some(binding) = binding
        .as_deref()
        .and_then(|nonce| bound_request(events, gate_id, nonce))
    else {
        return Err(missing(format!(
            "the decision of gate `{gate_id}` answers no bound request"
        )));
    };
    let of = required.of.as_ref().unwrap_or(&approves.results);
    for n in of {
        if let Some(m) = binding.missing.iter().find(|m| &m.node == n) {
            return Err(missing(format!(
                "`{n}` was not bound by gate `{gate_id}` ({})",
                m.reason.as_str()
            )));
        }
        let Some(latest) = binding
            .snapshots
            .iter()
            .filter(|s| &s.node == n)
            .map(|s| s.settlement_seq)
            .max()
        else {
            return Err(missing(format!("`{n}` was not bound by gate `{gate_id}`")));
        };
        if ran_after(events, n, latest) {
            return Err(ApprovalFailure {
                refusal: ApprovalRefusal::ApprovalStale,
                detail: format!("{n} ran again after approval"),
            });
        }
    }
    let bound: Vec<&BoundSnapshot> = binding
        .snapshots
        .iter()
        .filter(|s| of.contains(&s.node))
        .collect();
    let mut manifests = Vec::new();
    for s in &bound {
        match crate::result_view::verified_manifest(run_dir, &s.snapshot_id) {
            Ok(m) => manifests.push((*s, m)),
            Err(_) => {
                return Err(ApprovalFailure {
                    refusal: ApprovalRefusal::ApprovalSnapshotUnreadable,
                    detail: format!("snapshot {} of node `{}`", s.snapshot_id, s.node),
                });
            }
        }
    }
    if let Some((s, _)) = manifests
        .iter()
        .find(|(s, m)| s.incomplete || m.incomplete.is_some())
    {
        return Err(ApprovalFailure {
            refusal: ApprovalRefusal::ApprovalIncomplete,
            detail: format!(
                "snapshot {} of node `{}` stopped short of what was declared",
                s.snapshot_id, s.node
            ),
        });
    }
    if required.live == apb_core::schema::ApprovalLive::Ignore {
        return Ok(());
    }
    for (s, m) in &manifests {
        live_check(run_dir, events, s, m)?;
    }
    Ok(())
}

/// The binding the latest decision of the gate `node` requires answered,
/// for a node [`check_before`] already passed: what a publish node's plan
/// names and compares its commit with. `None` without `requires_approval`
/// or without a bound decision.
pub(crate) fn approved_binding<'e>(node: &Node, events: &'e [Event]) -> Option<&'e ReviewBinding> {
    let gate = node.requires_approval.as_ref()?.gate.as_str();
    let nonce = events.iter().rev().find_map(|e| match &e.payload {
        EventPayload::ReviewDecided {
            node: n, binding, ..
        } if n == gate => Some(binding.as_deref()),
        _ => None,
    })??;
    bound_request(events, gate, nonce)
}

/// The commit the approved result of `source` was taken at: `None` when
/// `source` is not among the nodes the approval covers for `node`, else
/// the `provenance.head` of its bound node snapshot (the latest, not an
/// item's), `Some(None)` when there is none to read: the snapshot recorded
/// no commit, or there is no node snapshot.
pub(crate) fn approved_head(
    run_dir: &Path,
    playbook: &Playbook,
    node: &Node,
    binding: &ReviewBinding,
    source: &str,
) -> Option<Option<String>> {
    let required = node.requires_approval.as_ref()?;
    let of = match &required.of {
        Some(of) => of.clone(),
        None => playbook
            .node(&required.gate)
            .and_then(|g| g.approves.as_ref())
            .map(|a| a.results.clone())
            .unwrap_or_default(),
    };
    if !of.iter().any(|n| n == source) {
        return None;
    }
    let bound = binding
        .snapshots
        .iter()
        .filter(|s| s.node == source && s.item.is_none())
        .max_by_key(|s| s.settlement_seq);
    Some(bound.and_then(|b| {
        crate::result_view::verified_manifest(run_dir, &b.snapshot_id)
            .ok()?
            .provenance
            .head
    }))
}

/// The binding of the request a decision with `nonce` answered.
fn bound_request<'e>(events: &'e [Event], gate: &str, nonce: &str) -> Option<&'e ReviewBinding> {
    events.iter().rev().find_map(|e| match &e.payload {
        EventPayload::ReviewRequested {
            node,
            binding: Some(b),
            ..
        } if node == gate && b.nonce == nonce => Some(b),
        _ => None,
    })
}

/// Whether `node` started or settled again after the settlement at `seq`.
fn ran_after(events: &[Event], node: &str, seq: u64) -> bool {
    events.iter().filter(|e| e.seq > seq).any(|e| {
        matches!(&e.payload,
            EventPayload::NodeStarted { node: n, .. } | EventPayload::NodeFinished { node: n, .. }
                if n == node)
    })
}

/// `live: check` for one bound snapshot: every file entry re-hashed live.
fn live_check(
    run_dir: &Path,
    events: &[Event],
    s: &BoundSnapshot,
    m: &SnapshotManifest,
) -> Result<(), ApprovalFailure> {
    use crate::result_view::LiveComparison;
    for e in m.entries.iter().filter(|e| e.part == EntryPart::File) {
        let path = e.path.as_deref().unwrap_or("");
        if e.redacted || path.starts_with(apb_core::result_store::REDACTED_PATH_PREFIX) {
            return Err(ApprovalFailure {
                refusal: ApprovalRefusal::ApprovalUnverifiable,
                detail: format!("{path} was stored redacted"),
            });
        }
        match crate::result_view::live_state(run_dir, events, e) {
            LiveComparison::Same => {}
            _ => {
                return Err(ApprovalFailure {
                    refusal: ApprovalRefusal::ApprovalStale,
                    detail: format!("{path} changed (node `{}`)", s.node),
                });
            }
        }
    }
    Ok(())
}

/// The refusal code of a supervisor patch, in-run or forward, that weakens
/// a person-only gate or an approval: these are a person's constraints, as
/// a launch policy is, and only a person's own version change may loosen
/// them.
pub(crate) const APPROVAL_WEAKENED: &str = "approval_weakened";

/// Why `patched` weakens a person-only gate or an approval that `running`
/// declares, `None` when it does not. Weakening is: a node that declared
/// `person_only`, `approves` or `requires_approval` gone or without it; an
/// `approves` that binds fewer results or counts more options as
/// approval; a `requires_approval` that names another gate, requires more
/// than a subset of what it required (`of` widened), or stops checking
/// the live files; and a node that a person-only or approving gate stood
/// before on every path of `running` reachable without it in `patched`.
pub(crate) fn patch_weakening(running: &Playbook, patched: &Playbook) -> Option<String> {
    for before in &running.nodes {
        let id = before.id.as_str();
        let after = patched.node(id);
        let why = node_weakening(running, before, patched, after);
        if let Some(why) = why {
            return Some(format!("node `{id}` {why}"));
        }
    }
    for gate in running.nodes.iter().filter(|n| guards_downstream(n)) {
        if patched.node(&gate.id).is_none() {
            continue;
        }
        let before = dominated_by(running, &gate.id);
        let after = dominated_by(patched, &gate.id);
        if let Some(n) = before
            .iter()
            .find(|n| patched.node(n).is_some() && !after.contains(*n))
        {
            return Some(format!(
                "node `{n}` becomes reachable without passing gate `{}`",
                gate.id
            ));
        }
    }
    None
}

/// The full refusal reason of an in-run patch: one [`patch_weakening`]
/// refuses, one that changes where an option of a person-only or approving
/// gate routes ([`route_change`]), or a `continue_from` that skips a gate
/// no passing decision answered ([`skips_gate`]).
pub(crate) fn patch_refusal(
    running: &Playbook,
    patched: &Playbook,
    events: &[Event],
    continue_from: &str,
) -> Option<String> {
    let why = patch_weakening(running, patched)
        .or_else(|| route_change(running, patched))
        .or_else(|| skips_gate(patched, events, continue_from))?;
    Some(refusal_line(&why))
}

/// The refusal line of a weakening `why`, in the words every surface
/// shows.
pub(crate) fn refusal_line(why: &str) -> String {
    format!(
        "{APPROVAL_WEAKENED}: {why}; only a person may weaken a person-only gate or an approval"
    )
}

/// Why moving the run to `target` (a patch's `continue_from`, a
/// supervisor's `run_continue_from` or `node_retry`, a resume's
/// `from_node`) skips a person-only or approving gate of `playbook`;
/// `None` when it does not. A gate is skipped when it stands before
/// `target` on every path and its latest decision does not pass it
/// toward `target` in the current visit ([`gate_passed`]). The one check
/// every one of those paths shares; only a person's resume (a terminal, a
/// dashboard session) may go past it.
pub fn skips_gate(playbook: &Playbook, events: &[Event], target: &str) -> Option<String> {
    playbook
        .nodes
        .iter()
        .filter(|g| guards_downstream(g) && g.id != target)
        .filter(|g| dominated_by(playbook, &g.id).contains(target))
        .find(|g| !gate_passed(playbook, g, events, target))
        .map(|g| {
            format!(
                "`{target}` skips gate `{}`, which no decision passed toward it in this visit",
                g.id
            )
        })
}

/// [`skips_gate`] for a resume of the run at `run_dir` from `from_node`,
/// over the run's own definition and journal, as the refusal line a
/// surface shows. `Ok(None)` when nothing is skipped.
pub fn resume_skips_gate(
    run_dir: &Path,
    from_node: &str,
) -> Result<Option<String>, crate::error::EngineError> {
    let playbook = crate::legacy_snapshot::load_run_playbook(run_dir).ok_or_else(|| {
        crate::error::EngineError::Invalid(
            "the run's definition does not read, so a resume from a named node is refused".into(),
        )
    })?;
    let events = crate::event::read_all(run_dir)?;
    Ok(skips_gate(&playbook, &events, from_node).map(|why| refusal_line(&why)))
}

/// Refuses a resume of the run at `run_dir` from `from_node` through
/// `channel` when it skips a gate ([`resume_skips_gate`]) and `channel` is
/// not a person's: a terminal or a dashboard session may go past a gate,
/// an agent (MCP), a run step, a script and a dashboard without keys never.
/// The message names how a person does it.
pub fn refuse_resume_skip(
    run_dir: &Path,
    from_node: &str,
    channel: crate::decision_channel::DecisionChannel,
) -> Result<(), crate::error::EngineError> {
    if channel.is_person_channel() {
        return Ok(());
    }
    let Some(line) = resume_skips_gate(run_dir, from_node)? else {
        return Ok(());
    };
    let refusal = channel
        .person_only_refusal()
        .unwrap_or(crate::decision_channel::CallerRefusal::NotThisCaller);
    let run = run_dir
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    Err(crate::error::EngineError::ChannelRefused {
        refusal,
        message: format!(
            "{line}. {} cannot resume past it ({}): decide the gate first, or resume at an interactive terminal outside a run step with `apb resume {run} --from-node {from_node}`, or on the run page of a dashboard where you are signed in.",
            channel.as_str(),
            refusal.as_str()
        ),
    })
}

/// Whether a gate's decision stands before the nodes after it: a
/// person-only gate or one that approves results.
fn guards_downstream(n: &Node) -> bool {
    n.person_only || n.approves.is_some()
}

fn node_weakening(
    running: &Playbook,
    before: &Node,
    patched: &Playbook,
    after: Option<&Node>,
) -> Option<&'static str> {
    let declares =
        before.person_only || before.approves.is_some() || before.requires_approval.is_some();
    let Some(after) = after else {
        return declares.then_some("is removed");
    };
    if before.person_only && !after.person_only {
        return Some("drops `person_only`");
    }
    if let Some(a) = &before.approves {
        let Some(b) = &after.approves else {
            return Some("drops `approves`");
        };
        if a.results.iter().any(|r| !b.results.contains(r)) {
            return Some("binds fewer results in `approves`");
        }
        let options = |n: &Node, x: &Approves| match &n.kind {
            NodeKind::HumanReview { options, .. } => x.approving_options(options),
            _ => Vec::new(),
        };
        let (was, now) = (options(before, a), options(after, b));
        if now.iter().any(|o| !was.contains(o)) {
            return Some("counts more options as approval");
        }
    }
    if let Some(r) = &before.requires_approval {
        let Some(s) = &after.requires_approval else {
            return Some("drops `requires_approval`");
        };
        if s.gate != r.gate {
            return Some("requires the approval of another gate");
        }
        let all = |p: &Playbook| {
            p.node(&r.gate)
                .and_then(|g| g.approves.as_ref())
                .map(|a| a.results.clone())
                .unwrap_or_default()
        };
        let (was, now) = (
            r.of.clone().unwrap_or_else(|| all(running)),
            s.of.clone().unwrap_or_else(|| all(patched)),
        );
        if was.iter().any(|n| !now.contains(n)) {
            return Some("requires the approval of fewer results");
        }
        if r.live == apb_core::schema::ApprovalLive::Check
            && s.live == apb_core::schema::ApprovalLive::Ignore
        {
            return Some("stops checking the live files (`live: ignore`)");
        }
    }
    None
}

/// The nodes `gate` stands before on every path: reachable from the start
/// and every failure route only through `gate`.
fn dominated_by(playbook: &Playbook, gate: &str) -> std::collections::BTreeSet<String> {
    use apb_core::graphutil::{adjacency, policy_handlers};
    let Some(start) = playbook
        .nodes
        .iter()
        .find(|n| matches!(n.kind, NodeKind::Start))
    else {
        return Default::default();
    };
    let mut seeds = vec![start.id.as_str()];
    seeds.extend(policy_handlers(playbook));
    let adj = adjacency(playbook);
    let mut without = adj.clone();
    without.insert(gate, Vec::new());
    let all = reach_all(&adj, &seeds);
    let open = reach_all(&without, &seeds);
    all.difference(&open)
        .filter(|n| **n != gate)
        .map(|n| n.to_string())
        .collect()
}

fn reach_all<'a>(
    adj: &std::collections::HashMap<&'a str, Vec<&'a str>>,
    seeds: &[&'a str],
) -> std::collections::BTreeSet<&'a str> {
    seeds
        .iter()
        .flat_map(|s| apb_core::graphutil::reachable_from(adj, s))
        .collect()
}

/// Whether the latest decision of `gate` passes it toward `target` in the
/// current visit. It answers the gate's latest request (a request since,
/// open or withdrawn, is a new visit); it is an approving option on that
/// request's binding for a gate that `approves` results, and for a
/// person-only gate any option but `reject`; an edge that option takes
/// leads to `target` without passing the gate again; and no node started
/// since it that leads back into the gate and not on to `target` (the run
/// left this visit's route, as a loop back toward the gate does).
fn gate_passed(playbook: &Playbook, gate: &Node, events: &[Event], target: &str) -> bool {
    let id = gate.id.as_str();
    let Some((decided, decision, nonce)) = events.iter().rev().find_map(|e| match &e.payload {
        EventPayload::ReviewDecided {
            node,
            decision,
            binding,
            ..
        } if node == id => Some((e.seq, decision.as_str(), binding.as_deref())),
        _ => None,
    }) else {
        return false;
    };
    let Some(requested) = events.iter().rev().find_map(|e| match &e.payload {
        EventPayload::ReviewRequested { node, .. } if node == id => Some(e.seq),
        _ => None,
    }) else {
        return false;
    };
    if requested > decided {
        return false;
    }
    let options = match &gate.kind {
        NodeKind::HumanReview { options, .. } => options.as_slice(),
        _ => &[],
    };
    let passes = match &gate.approves {
        Some(a) => {
            a.approving_options(options).iter().any(|o| o == decision)
                && nonce.is_some_and(|n| {
                    events.iter().any(|e| {
                        e.seq == requested
                            && matches!(&e.payload, EventPayload::ReviewRequested {
                                binding: Some(b), ..
                            } if b.nonce == n)
                    })
                })
        }
        None => decision != apb_core::schema::DEFAULT_REVIEW_OPTIONS[1],
    };
    if !passes {
        return false;
    }
    use apb_core::graphutil::{adjacency, reachable_from};
    let adj = adjacency(playbook);
    let mut without = adj.clone();
    without.insert(id, Vec::new());
    let leads_to = |from: &str| from == target || reachable_from(&without, from).contains(target);
    let routed = option_routes(playbook, id)
        .remove(decision)
        .unwrap_or_default();
    if !routed.iter().any(|(to, _)| leads_to(to)) {
        return false;
    }
    !events
        .iter()
        .filter(|e| e.seq > decided)
        .any(|e| match &e.payload {
            EventPayload::NodeStarted { node, .. } => {
                node != id && reachable_from(&adj, node).contains(id) && !leads_to(node)
            }
            _ => false,
        })
}

/// Where each option of gate `gate` routes: the targets of the edges out
/// of it that the option can take (an edge without a condition, a
/// `review_status` edge naming it, an edge with another condition that
/// may match), each marked whether it is a fallback; a fallback is taken
/// only when nothing else matches. Empty for a node that is no
/// `human_review`.
fn option_routes(
    playbook: &Playbook,
    gate: &str,
) -> std::collections::BTreeMap<String, std::collections::BTreeSet<(String, bool)>> {
    let Some(NodeKind::HumanReview { options, .. }) = playbook.node(gate).map(|n| &n.kind) else {
        return Default::default();
    };
    apb_core::schema::effective_review_options(options)
        .into_iter()
        .map(|option| {
            let routes = playbook
                .edges
                .iter()
                .filter(|e| e.from == gate)
                .filter(|e| {
                    e.fallback
                        || match &e.condition {
                            Some(apb_core::schema::EdgeCondition::ReviewStatus { equals }) => {
                                *equals == option
                            }
                            _ => true,
                        }
                })
                .map(|e| (e.to.clone(), e.fallback))
                .collect();
            (option, routes)
        })
        .collect()
}

/// Why an in-run patch changes where an option of a person-only or
/// approving gate routes; `None` when every such gate's options route as
/// they did. A person decides an option for what it leads to, so a
/// supervisor never re-points one while the run is under way.
fn route_change(running: &Playbook, patched: &Playbook) -> Option<String> {
    running
        .nodes
        .iter()
        .filter(|g| guards_downstream(g) && patched.node(&g.id).is_some())
        .find(|g| option_routes(running, &g.id) != option_routes(patched, &g.id))
        .map(|g| format!("node `{}` changes where its options route", g.id))
}
