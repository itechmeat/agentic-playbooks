//! The drive's arm for a `publish` node (see [`crate::publish`]). It is
//! re-entered on every drive iteration and never pushes twice:
//!
//! 1. the ledger first: a confirmed push finishes `published`; an unknown
//!    one is reconciled by reading the remote ref back, and fails
//!    `publish_unknown` (naming `apb intent resolve`) when that does not
//!    confirm it; an earlier push that was not sent spends its consent;
//! 2. an open request waits for a decision carrying the plan's nonce;
//!    `skip` finishes `skipped`, `publish` re-plans at once;
//! 3. the read-only plan refuses with a code, finishes when there is
//!    nothing to push, or is journaled and asked about;
//! 4. a plan identical to the one a person just consented to (also after a
//!    crash between the decision and the push) is recorded in the ledger,
//!    pushed, and settled from what git reported.
//!
//! No recommendation is asked for and no decision is ever posted
//! automatically: the consent is a person's.

use super::review_gate::{Consume, GateDrive, GateRequest, GateStep, await_decision};
use super::*;

use apb_core::publish::porcelain::Outcome;
use apb_core::publish::{self as core_publish, OPTION_PUBLISH, OPTION_SKIP, output};

use crate::connector::intent::{Admission, IntentState, ledger};
use crate::publish::intent as push_intent;
use crate::publish::plan::{Plan, PlanInput, Planned};
use crate::publish::push::{Remote, RemoteHead};

/// What one look at a publish node found.
pub(crate) enum PublishStep {
    /// The node finished: the shared tail journals `node_finished`.
    Finished(NodeStatus, String),
    /// Waiting for a person or an in-flight push; the drive sleeps.
    Waiting,
    /// Something was journaled that the next look acts on at once.
    Again,
    /// A supervisor directive moved the run off the node.
    Moved,
}

/// The run facts the arm reads.
pub(crate) struct PublishRun<'a> {
    pub(crate) root: &'a Path,
    pub(crate) run_id: &'a str,
    pub(crate) workdir: &'a Path,
    pub(crate) state: &'a RunState,
    pub(crate) cfg: &'a RunConfig,
    pub(crate) scrub: &'a [String],
    /// The abort-only flag: a pause never interrupts a push.
    pub(crate) abort: &'a AtomicBool,
}

/// The node's settings.
struct Spec<'a> {
    from: &'a str,
    remote: &'a str,
    branch: Option<&'a str>,
    timeout: u64,
    prompt: Option<&'a str>,
    title: Option<&'a str>,
}

fn spec<'a>(playbook: &'a Playbook, node: &str) -> Result<Spec<'a>, EngineError> {
    let n = playbook
        .node(node)
        .ok_or_else(|| EngineError::NotFound(node.to_string()))?;
    match &n.kind {
        NodeKind::Publish {
            from,
            remote,
            branch,
            timeout_seconds,
            prompt,
        } => Ok(Spec {
            from,
            remote,
            branch: branch.as_deref(),
            timeout: timeout_seconds.unwrap_or(core_publish::DEFAULT_TIMEOUT_SECONDS),
            prompt: prompt.as_deref(),
            title: n.title.as_deref(),
        }),
        _ => Err(EngineError::Invalid(format!(
            "node `{node}` is not a publish node"
        ))),
    }
}

/// Whether the node's current visit already journaled `node_started`.
fn visit_open(events: &[Event], node: &str) -> bool {
    let count = |want: fn(&EventPayload, &str) -> bool| {
        events.iter().filter(|e| want(&e.payload, node)).count()
    };
    count(|p, n| matches!(p, EventPayload::NodeStarted { node, .. } if node == n))
        > count(|p, n| matches!(p, EventPayload::NodeFinished { node, .. } if node == n))
}

/// Journals `node_started` once per visit.
fn start_visit(run_dir: &Path, log: &mut EventLog, node: &str) -> Result<(), EngineError> {
    if !visit_open(&read_all(run_dir)?, node) {
        log.append(EventPayload::NodeStarted {
            node: node.to_string(),
            attempt: 1,
        })?;
    }
    Ok(())
}

/// How many decisions answering the plan with `digest` were applied.
fn decided_for(events: &[Event], node: &str, digest: &str) -> usize {
    events
        .iter()
        .filter(|e| {
            matches!(&e.payload, EventPayload::ReviewDecided { node: n, plan_digest: Some(d), .. }
                if n == node && d == digest)
        })
        .count()
}

/// Whether the node's last gate or push event is a `publish` decision for
/// exactly `digest`: a consent not spent by a push, a finish, or a newer
/// request.
fn unspent_consent(events: &[Event], node: &str, digest: &str) -> bool {
    events
        .iter()
        .rev()
        .find_map(|e| match &e.payload {
            EventPayload::ReviewDecided {
                node: n,
                decision,
                plan_digest,
                ..
            } if n == node => {
                Some(decision == OPTION_PUBLISH && plan_digest.as_deref() == Some(digest))
            }
            EventPayload::ReviewRequested { node: n, .. }
            | EventPayload::ReviewWithdrawn { node: n, .. }
            | EventPayload::NodeFinished { node: n, .. }
            | EventPayload::SendIntentRecorded { node: n, .. }
                if n == node =>
            {
                Some(false)
            }
            _ => None,
        })
        .unwrap_or(false)
}

/// One look at the publish node `current`.
pub(crate) fn step(r: &PublishRun<'_>, d: &mut GateDrive<'_>) -> Result<PublishStep, EngineError> {
    let node = d.current.clone();
    let s = spec(d.playbook, &node)?;
    let events = read_all(d.run_dir)?;

    // 1. The ledger first.
    if let Some((rec, state)) = push_intent::latest(&events, &node) {
        match state {
            IntentState::Confirmed { .. } => {
                return finished(d, &node, NodeStatus::Succeeded, output::PUBLISHED.into());
            }
            IntentState::Pending { .. } => return Ok(PublishStep::Waiting),
            IntentState::Unknown { .. } => return reconcile(r, d, &node, &events, &rec),
            // Not sent: any earlier consent is spent; plan again.
            IntentState::NotSent { .. } => {}
        }
    }

    // 2. An open request waits for the decision carrying its nonce.
    if review_open_count(&events, &node) > 0
        && let Some(EventPayload::PublishPlanned {
            plan_digest, nonce, ..
        }) = crate::publish::latest_plan(&events, &node).map(|e| &e.payload)
    {
        let consume = Consume::Nonce {
            nonce,
            consumed: decided_for(&events, &node, plan_digest),
        };
        // The request is open, so it is never declared again here.
        let step = await_decision(d, consume, |_, _| {
            Err(EngineError::Invalid(format!(
                "publish node `{node}` has an open request"
            )))
        })?;
        return decided(d, &node, plan_digest, step);
    }

    // 3. Plan, read-only.
    let tree = source_tree(r, d, &s)?;
    let branch = match s.branch {
        Some(t) => Some(render_node_prompt(
            d.run_dir,
            r.run_id,
            r.state,
            r.cfg,
            t,
            &d.playbook.context_budget(&node),
        )?),
        None => None,
    };
    let home = std::env::var_os("HOME").map(PathBuf::from);
    let writable = writable_paths(d.playbook, home.as_deref());
    let input = PlanInput {
        node: &node,
        from: s.from,
        remote: s.remote,
        events: &events,
        tree: &tree,
        branch,
        run_dir: d.run_dir,
        writable: &writable,
        scrub: r.scrub,
        abort: Some(r.abort),
    };
    let plan = match crate::publish::plan::plan(&input) {
        Planned::Refused { why, detail } => {
            let out = crate::publish::refused_output(why, detail.as_deref());
            return finished(d, &node, NodeStatus::Failed, out);
        }
        Planned::Nothing => {
            let out = output::NOTHING_TO_PUBLISH.to_string();
            return finished(d, &node, NodeStatus::Succeeded, out);
        }
        Planned::AlreadyOnRemote { after } => {
            let out = format!(
                "{}: the remote branch already points at {after}; nothing was pushed",
                output::ALREADY_ON_REMOTE
            );
            return finished(d, &node, NodeStatus::Succeeded, out);
        }
        Planned::Ready(p) => p,
    };

    // 4. A consent to exactly this plan pushes; otherwise ask.
    if unspent_consent(&events, &node, &plan.digest) {
        return push(r, d, &node, &s, &plan, &input);
    }
    let consume = Consume::Nonce {
        nonce: &plan.nonce,
        consumed: decided_for(&events, &node, &plan.digest),
    };
    let step = await_decision(d, consume, |log, _| {
        log.append(plan.event(&node, s.from))?;
        Ok(request(&node, &s, &plan))
    })?;
    decided(d, &node, &plan.digest, step)
}

/// The source node's tree, resolved exactly as provenance resolves it.
fn source_tree(
    r: &PublishRun<'_>,
    d: &GateDrive<'_>,
    s: &Spec<'_>,
) -> Result<PathBuf, EngineError> {
    Ok(super::provenance::node_tree(
        d.playbook,
        s.from,
        r.workdir,
        |t| {
            render_node_prompt(
                d.run_dir,
                r.run_id,
                r.state,
                r.cfg,
                t,
                &d.playbook.context_budget(s.from),
            )
        },
    ))
}

/// The tree `node` runs in when its `workdir` resolves now (the run's tree
/// when it declares none), `None` when it does not: what the pin reads.
pub(crate) fn resolved_tree(
    playbook: &Playbook,
    node: &str,
    workdir: &Path,
    render: impl FnOnce(&str) -> Result<String, EngineError>,
) -> Option<PathBuf> {
    match super::node_workdir::resolve(playbook, node, workdir, render) {
        Ok(Ok(dir)) => Some(dir),
        _ => None,
    }
}

/// Every `writable` path a node of the playbook declares, resolved on this
/// host (`home` standing for `~`): a `file` remote inside one is refused.
/// Nothing is dropped: a path that does not resolve yet is kept as written
/// (an agent may create it later), and a `~` path without a home becomes
/// `/`, so every `file` remote is refused rather than none.
fn writable_paths(playbook: &Playbook, home: Option<&Path>) -> Vec<PathBuf> {
    use apb_core::isolation::paths;
    playbook
        .nodes
        .iter()
        .filter_map(|n| n.kind.isolation())
        .flat_map(|spec| spec.writable.iter())
        .map(|p| {
            let raw = match home {
                Some(h) => paths::expand(p, h),
                None if p.trim().starts_with('~') => PathBuf::from("/"),
                None => PathBuf::from(p.trim()),
            };
            raw.canonicalize()
                .ok()
                .or_else(|| paths::resolve_loose(&raw))
                .unwrap_or(raw)
        })
        .collect()
}

/// The `review_requested` of a plan.
fn request(node: &str, s: &Spec<'_>, plan: &Plan) -> GateRequest {
    let instruction = crate::people::instruction::publish_instruction(
        node,
        s.title,
        plan.url.as_str(),
        &plan.branch,
        &plan.after,
        plan.remote_before.as_deref(),
        plan.commits.len() + plan.omitted,
        plan.outside_push_suspected,
        s.prompt,
        &plan.nonce,
    );
    GateRequest {
        event: EventPayload::ReviewRequested {
            node: node.to_string(),
            options: core_publish::OPTIONS.map(str::to_string).to_vec(),
            title: s.title.map(str::to_string),
            instruction,
            prompt: s.prompt.map(str::to_string),
            recommendation: None,
            plan_digest: Some(plan.digest.clone()),
        },
        auto: None,
    }
}

/// What a gate look means for the node.
fn decided(
    d: &mut GateDrive<'_>,
    node: &str,
    digest: &str,
    step: GateStep,
) -> Result<PublishStep, EngineError> {
    match step {
        GateStep::Waiting => Ok(PublishStep::Waiting),
        GateStep::Moved => Ok(PublishStep::Moved),
        // The visit's `node_started` waits for what finishes it (a skip, a
        // refusal, the push), so a request withdrawn by a directive never
        // leaves a visit open.
        GateStep::Decided(entry) => {
            if entry.cmd.decision == OPTION_SKIP {
                start_visit(d.run_dir, d.log, node)?;
            }
            d.log.append(EventPayload::ReviewDecided {
                node: node.to_string(),
                decision: entry.cmd.decision.clone(),
                note: entry.cmd.note.clone(),
                plan_digest: Some(digest.to_string()),
            })?;
            if entry.cmd.decision == OPTION_SKIP {
                Ok(PublishStep::Finished(
                    NodeStatus::Succeeded,
                    output::SKIPPED.into(),
                ))
            } else {
                // `publish`: re-plan at once; an identical plan pushes.
                Ok(PublishStep::Again)
            }
        }
    }
}

/// Finishes the visit with `status` and `output`.
fn finished(
    d: &mut GateDrive<'_>,
    node: &str,
    status: NodeStatus,
    output: String,
) -> Result<PublishStep, EngineError> {
    start_visit(d.run_dir, d.log, node)?;
    Ok(PublishStep::Finished(status, output))
}

/// The resolve hint of an unknown push.
fn unknown_output(run_id: &str, intent: &str, why: &str) -> String {
    format!(
        "{}: {why}; apb never pushes again on its own. Check the remote, then run `apb intent resolve {run_id} {intent} --sent` or `--not-sent`",
        output::UNKNOWN
    )
}

/// Reads the remote ref an unknown push targeted: the plan's commit there
/// confirms the push; anything else leaves it to a person. Never pushes.
fn reconcile(
    r: &PublishRun<'_>,
    d: &mut GateDrive<'_>,
    node: &str,
    events: &[Event],
    rec: &crate::connector::intent::IntentRecord,
) -> Result<PublishStep, EngineError> {
    let Some(pushed) = push_intent::pushed_plan(events, rec) else {
        let out = unknown_output(r.run_id, &rec.id, "the plan of the push is missing");
        return finished(d, node, NodeStatus::Failed, out);
    };
    let found = match (
        apb_core::publish::url::PushUrl::parse(&pushed.url),
        crate::publish::pin_of(events, node),
    ) {
        (Ok(url), Some(pin)) => {
            match Remote::open(
                d.run_dir,
                &url,
                Path::new(&pin.common_dir),
                r.scrub,
                Some(r.abort),
            ) {
                Ok(remote) => remote.head_of(&pushed.branch),
                Err(_) => RemoteHead::Unreachable,
            }
        }
        (Err(_), _) => {
            let out = unknown_output(
                r.run_id,
                &rec.id,
                "the push's result is unknown and its recorded URL can no longer be read back",
            );
            return finished(d, node, NodeStatus::Failed, out);
        }
        (Ok(_), None) => {
            let out = unknown_output(
                r.run_id,
                &rec.id,
                "the push's result is unknown and the node's pin is missing",
            );
            return finished(d, node, NodeStatus::Failed, out);
        }
    };
    let branch = &pushed.branch;
    let why = match found {
        RemoteHead::At(sha) if sha == pushed.after => {
            push_intent::settle(
                d.log,
                &rec.id,
                push_intent::settlement(
                    ledger::CONFIRMED,
                    push_intent::RECONCILE,
                    None,
                    Some(core_publish::effect_ref(
                        &pushed.url,
                        branch,
                        pushed.remote_before.as_deref(),
                        &pushed.after,
                    )),
                ),
            )?;
            return finished(d, node, NodeStatus::Succeeded, output::PUBLISHED.into());
        }
        RemoteHead::At(sha) => {
            format!("the push's result is unknown and the remote branch `{branch}` is at {sha}")
        }
        RemoteHead::Absent => {
            format!("the push's result is unknown and the remote has no branch `{branch}`")
        }
        RemoteHead::Unreachable => {
            "the push's result is unknown and the remote could not be listed".to_string()
        }
    };
    let out = unknown_output(r.run_id, &rec.id, &why);
    finished(d, node, NodeStatus::Failed, out)
}

/// What a ledger answer other than a new send means for the node: the
/// ledger already holds a push for this key, so nothing is sent again. A
/// confirmed push counts as this plan's only when the plan it carried sent
/// this plan's commit (the key names the node, URL and branch, not the
/// commit); when that cannot be told, a person looks.
fn not_admitted(
    r: &PublishRun<'_>,
    d: &mut GateDrive<'_>,
    node: &str,
    plan: &Plan,
    admission: Admission,
) -> Result<PublishStep, EngineError> {
    match admission {
        Admission::AlreadySent { intent, .. } => {
            // The commit that push sent, from its own plan: the effect
            // reference is a display string (capped, and absent on a
            // person's settlement), never evidence.
            let events = read_all(d.run_dir)?;
            let ledger = crate::connector::intent::Ledger::fold(&events);
            let sent = ledger
                .records()
                .iter()
                .find(|rec| rec.id == intent)
                .and_then(|rec| push_intent::pushed_plan(&events, rec))
                .map(|p| p.after);
            match sent {
                Some(after) if after == plan.after => {
                    finished(d, node, NodeStatus::Succeeded, output::PUBLISHED.into())
                }
                Some(after) => {
                    let out = format!(
                        "{}: a confirmed push of {after} to branch `{}` already holds this node's ledger entry; {} was not pushed",
                        output::NOT_SENT,
                        plan.branch,
                        plan.after
                    );
                    finished(d, node, NodeStatus::Failed, out)
                }
                None => {
                    let out = unknown_output(
                        r.run_id,
                        &intent,
                        &format!(
                            "a confirmed push to branch `{}` holds this node's ledger entry, and which commit it sent cannot be told; check whether the remote holds {}",
                            plan.branch, plan.after
                        ),
                    );
                    finished(d, node, NodeStatus::Failed, out)
                }
            }
        }
        Admission::InFlight { .. } => Ok(PublishStep::Waiting),
        Admission::Reconcile { intent } => {
            let events = read_all(d.run_dir)?;
            let ledger = crate::connector::intent::Ledger::fold(&events);
            match ledger.records().iter().find(|rec| rec.id == intent) {
                Some(rec) => reconcile(r, d, node, &events, rec),
                None => {
                    let out = unknown_output(r.run_id, &intent, "the push's record is missing");
                    finished(d, node, NodeStatus::Failed, out)
                }
            }
        }
        Admission::RefuseUnknown { intent, why } => {
            let out = unknown_output(r.run_id, &intent, &why);
            finished(d, node, NodeStatus::Failed, out)
        }
        other => {
            let out = format!(
                "{}: the send ledger did not admit the push ({other:?}); nothing was pushed",
                output::NOT_SENT
            );
            finished(d, node, NodeStatus::Failed, out)
        }
    }
}

/// Records the push in the ledger, pushes, and settles it from what git
/// reported.
fn push(
    r: &PublishRun<'_>,
    d: &mut GateDrive<'_>,
    node: &str,
    s: &Spec<'_>,
    plan: &Plan,
    input: &PlanInput<'_>,
) -> Result<PublishStep, EngineError> {
    start_visit(d.run_dir, d.log, node)?;
    let key = push_intent::key(node, plan.url.as_str(), &plan.branch);
    let new = match push_intent::admit(d.log, &key, s.timeout)? {
        Ok(new) => new,
        Err(admission) => return not_admitted(r, d, node, plan, admission),
    };
    let scrubber = apb_core::scrub::Scrubber::for_workspace(r.root, Vec::new());
    // A shadow that cannot be made means git never started: not sent.
    let outcome = match plan.remote(input) {
        Ok(remote) => remote.push(
            &plan.branch,
            &plan.after,
            plan.remote_before.as_deref(),
            Duration::from_secs(s.timeout),
            &scrubber,
        ),
        Err(e) => Outcome::NotSent(format!("the shadow git directory could not be made: {e}")),
    };
    let effect = core_publish::effect_ref(
        plan.url.as_str(),
        &plan.branch,
        plan.remote_before.as_deref(),
        &plan.after,
    );
    let (settlement, status, out) = match outcome {
        Outcome::Confirmed => (
            push_intent::settlement(ledger::CONFIRMED, push_intent::RESPONSE, None, Some(effect)),
            NodeStatus::Succeeded,
            output::PUBLISHED.to_string(),
        ),
        Outcome::NotSent(why) => (
            push_intent::settlement(
                ledger::NOT_SENT,
                push_intent::RESPONSE,
                Some(why.clone()),
                None,
            ),
            NodeStatus::Failed,
            format!("{}: {why}", output::NOT_SENT),
        ),
        Outcome::Unknown(why) => (
            push_intent::settlement(
                ledger::UNKNOWN,
                push_intent::RESPONSE,
                Some(why.clone()),
                None,
            ),
            NodeStatus::Failed,
            unknown_output(r.run_id, &new.id, &why),
        ),
    };
    push_intent::settle(d.log, &new.id, settlement)?;
    Ok(PublishStep::Finished(status, out))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn playbook(writable: &str) -> Playbook {
        Playbook::from_yaml(&format!(
            "schema: 2\nid: p\nname: P\nversion: 1.0.0\nnodes:\n  - {{ id: start, type: start }}\n  - {{ id: w, type: agent_task, prompt: x, profile: p, isolation: {{ level: full, network: none, writable: [{writable}] }} }}\n  - {{ id: done, type: finish, outcome: success }}\nedges:\n  - {{ from: start, to: w }}\n  - {{ from: w, to: done }}\n"
        ))
        .unwrap()
    }

    #[test]
    fn a_writable_path_is_never_dropped() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().canonicalize().unwrap();
        // A path an agent creates later is kept where it will be.
        let later = writable_paths(&playbook("\"~/later/cache\""), Some(&home));
        assert_eq!(later, [home.join("later/cache")]);
        // Without a home a `~` path covers everything: every file remote is
        // refused rather than none.
        assert_eq!(
            writable_paths(&playbook("\"~/.cache/x\""), None),
            [PathBuf::from("/")]
        );
    }
}
