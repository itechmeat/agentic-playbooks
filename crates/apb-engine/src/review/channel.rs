//! Who may post a decision: a person-only gate takes one only from a
//! person's channel ([`crate::decision_channel::DecisionChannel`]), and a
//! step of a run's own tree never decides or answers for it.

use std::path::Path;

use crate::decision_channel::{CallerRefusal, DecisionChannel};
use crate::error::EngineError;

/// Whether a decision through `channel` decides a person-only gate of the
/// run at `run_dir`: a person's channel, or a scenario run's scripted
/// decision in a scenario run (its person is the script). An entry without
/// a channel never does.
pub fn decides_person_only(run_dir: &Path, channel: Option<DecisionChannel>) -> bool {
    match channel {
        Some(c) if c.is_person_channel() => true,
        Some(DecisionChannel::Scenario) => crate::scenario::session::is_scenario(run_dir),
        _ => false,
    }
}

/// The refusal of a decision through `channel` on the person-only gate
/// `node` of the run at `run_dir`; `Ok` when the channel decides it. The
/// message names both ways a person decides.
pub fn refuse_for_person_only(
    run_dir: &Path,
    node: &str,
    channel: Option<DecisionChannel>,
    has_proof: bool,
) -> Result<(), EngineError> {
    // A dashboard request stays open: its route stamps the entry with the
    // full assertion only after it verified it, and the append-time check
    // enforces that stamp. The terminal has no assertion to give, so with
    // a passkey enrolled it is refused like any other channel. A dashboard
    // request that carries a proof decides a person-only gate even where
    // the channel alone would not: the assertion is the person.
    let dashboard_proof = has_proof
        && matches!(
            channel,
            Some(DecisionChannel::DashboardSession) | Some(DecisionChannel::DashboardKeyless)
        );
    if dashboard_proof {
        return Ok(());
    }
    if decides_person_only(run_dir, channel) && !passkey_required_for_person_only(run_dir, node) {
        return Ok(());
    }
    let refusal = channel
        .and_then(DecisionChannel::person_only_refusal)
        .unwrap_or(if decides_person_only(run_dir, channel) {
            CallerRefusal::PasskeyRequired
        } else {
            CallerRefusal::NotThisCaller
        });
    let run = run_id_of(run_dir);
    let through = channel.map_or("an unstated channel", DecisionChannel::as_str);
    // With a passkey enrolled on this machine the terminal stops being a
    // way to decide a person-only gate at all: the dashboard, where a
    // fresh WebAuthn assertion is asked for, is the only way left.
    let passkey = passkey_required_for_person_only(run_dir, node);
    let how = if passkey {
        let link = run_dir
            .parent()
            .and_then(|p| p.parent())
            .and_then(|root| crate::people::link::run_link(root, &run))
            .unwrap_or_else(|| "the dashboard's run page".to_string());
        format!(
            "A passkey is enrolled on this machine, so this gate needs a fresh passkey assertion; decide on the dashboard: {link}"
        )
    } else {
        format!(
            "Decide at an interactive terminal outside a run step with `apb review {run} {node} --decision <option>{}`, or on the run page of a dashboard where you are signed in.",
            bound_args(run_dir, node)
        )
    };
    Err(EngineError::ChannelRefused {
        refusal: if passkey {
            CallerRefusal::PasskeyRequired
        } else {
            refusal
        },
        message: format!(
            "gate `{node}` of run `{run}` is person_only, so only a person decides it; {through} is not a person's channel ({}). {how}",
            refusal.as_str()
        ),
    })
}

/// Whether `channel` never gives `decision` as a publish consent on a
/// `publish` gate (`is_publish_gate`): a dashboard without keys takes no
/// publish consent (decision record 0010). Anything on this machine
/// reaches it, a sandboxed agent under `network: open` included, and the
/// open plan's nonce is in the run payload, so it could consent to its own
/// push. The rule protects consent to a publish node's push plan only: an
/// option named `publish` on a `human_review` gate is an ordinary review
/// decision, so the gate decides it. A `skip` pushes nothing and goes
/// through. The one rule the decision path ([`refuse_publish_consent`])
/// and the next actions (`next_action::caller_refusal`) share.
pub fn refuses_publish_consent(
    channel: DecisionChannel,
    decision: &str,
    is_publish_gate: bool,
) -> bool {
    is_publish_gate
        && channel == DecisionChannel::DashboardKeyless
        && decision == apb_core::publish::OPTION_PUBLISH
}

/// Whether the gate `node` of the run at `run_dir` is a `publish` node, so
/// [`refuses_publish_consent`] applies to it. A playbook that does not
/// read, or a node it does not have, is one: the rule fails closed, the
/// same way `check_review_target` judges an unreadable snapshot.
pub fn is_publish_gate(run_dir: &Path, node: &str) -> bool {
    crate::progress::load_run_playbook(run_dir)
        .and_then(|p| {
            p.node(node)
                .map(|n| matches!(n.kind, apb_core::schema::NodeKind::Publish { .. }))
        })
        .unwrap_or(true)
}

/// The refusal of `decision` on the publish gate `node` of the run at
/// `run_dir` through `channel` when [`refuses_publish_consent`] holds
/// (`keyless_dashboard`); `Ok` otherwise. The message names where the
/// person consents instead: the CLI, or, while a passkey is enforced and
/// the CLI refuses a publish consent too, a signed-in dashboard session.
pub fn refuse_publish_consent(
    run_dir: &Path,
    node: &str,
    decision: &str,
    channel: DecisionChannel,
) -> Result<(), EngineError> {
    if !refuses_publish_consent(channel, decision, is_publish_gate(run_dir, node)) {
        return Ok(());
    }
    let message = if passkey_enforced_here() {
        "a dashboard without keys does not take a publish consent, since anything on this machine can reach it, and while a passkey is enrolled the CLI refuses it too: issue a key with `apb server key issue`, sign in, and decide with a passkey on the run page".to_string()
    } else {
        format!(
            "a dashboard without keys does not take a publish consent, since anything on this machine can reach it; approve with `apb publish approve {} --node {node} --nonce <nonce>`, or issue a key with `apb server key issue` and sign in",
            run_id_of(run_dir)
        )
    };
    Err(EngineError::ChannelRefused {
        refusal: CallerRefusal::KeylessDashboard,
        message,
    })
}

/// At least one passkey enrolled, or a store that exists but does not
/// read (fail closed); kept here, off the parent module's
/// `passkey_enforced`, so this leaf never depends back on it.
fn passkey_enforced_here() -> bool {
    match apb_core::passkey::count_strict() {
        Ok(n) => n > 0,
        Err(_) => true,
    }
}

/// Whether a passkey assertion is required on top of a person's channel
/// for the gate `node` of the run at `run_dir`: at least one passkey is
/// enrolled on this machine and the gate is one only a person decides
/// ([`super::passkey_required`] covers bound approvals too). A playbook
/// that does not read fails closed: the requirement stands.
fn passkey_required_for_person_only(run_dir: &Path, node: &str) -> bool {
    passkey_enforced_here()
        && crate::progress::load_run_playbook(run_dir)
            .and_then(|p| {
                p.node(node).map(|n| {
                    n.person_only || matches!(n.kind, apb_core::schema::NodeKind::Publish { .. })
                })
            })
            .unwrap_or(true)
}

/// The open request of gate `gate`: the seq of its latest
/// `review_requested` and the binding it carries, while that request has
/// neither a decision nor a withdrawal. `None` when nothing is open.
pub fn open_request<'e>(
    events: &'e [crate::event::Event],
    gate: &str,
) -> Option<(u64, Option<&'e crate::approval::ReviewBinding>)> {
    if crate::event::review_open_count(events, gate) == 0 {
        return None;
    }
    events.iter().rev().find_map(|e| match &e.payload {
        crate::event::EventPayload::ReviewRequested { node, binding, .. } if node == gate => {
            Some((e.seq, binding.as_ref()))
        }
        _ => None,
    })
}

/// The ` --nonce <binding> --request review:<seq>` an approving decision
/// on the open request of gate `node` carries when that request binds
/// snapshots, so the command in a refusal is one that is accepted; empty
/// otherwise, or when the journal does not read.
fn bound_args(run_dir: &Path, node: &str) -> String {
    let events = crate::event::read_all_lossy_tail(run_dir).unwrap_or_default();
    match open_request(&events, node) {
        Some((seq, Some(b))) => format!(" --nonce {} --request review:{seq}", b.nonce),
        _ => String::new(),
    }
}

/// The refusal of a decision or an answer `caller` posts to the run at
/// `run_dir` from inside a step of a run of the same tree: the run, its
/// ancestors, and every descendant of its topmost ancestor, siblings
/// included (followed through `parent_run` up and `child_run_started`
/// down, at most [`MAX_TREE_DEPTH`] levels up and twice that down): a step
/// deciding a gate whose outcome steers its own run defeats the gate. A
/// step of an unrelated run, and a process that is no step, pass. A tree
/// that cannot be read whole (a config or a journal that does not read, a
/// link beyond the depth) refuses a step: the guard fails closed.
pub fn refuse_own_run(
    run_dir: &Path,
    caller: &apb_core::step_env::StepCaller,
) -> Result<(), EngineError> {
    let Some(step_run) = caller.run_id.as_deref() else {
        return Ok(());
    };
    let message = match run_tree(run_dir) {
        Ok(tree) if !tree.iter().any(|r| r == step_run) => return Ok(()),
        Ok(_) => format!(
            "this process runs inside run `{step_run}` (APB_RUN_ID is set), which is in the run tree of run `{}` (the run, an ancestor, a descendant or a sibling); a run step never decides a gate or answers a question of its own run tree. Ask the person to decide outside the run.",
            run_id_of(run_dir)
        ),
        Err(why) => format!(
            "this process runs inside run `{step_run}` (APB_RUN_ID is set), and the run tree of run `{}` cannot be read whole ({why}), so it may be its own; a run step's decision or answer is refused. Ask the person to decide outside the run.",
            run_id_of(run_dir)
        ),
    };
    Err(EngineError::ChannelRefused {
        refusal: CallerRefusal::RunStepOwnRun,
        message,
    })
}

/// How many levels of ancestors and descendants [`refuse_own_run`]
/// follows: the depth run redaction follows.
pub const MAX_TREE_DEPTH: usize = 8;

fn run_id_of(run_dir: &Path) -> String {
    run_dir
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("")
        .to_string()
}

/// The run tree of the run at `run_dir`: the run, its ancestors, and the
/// descendants of every one of them (siblings included). An error names
/// what could not be read, so the caller fails closed.
fn run_tree(run_dir: &Path) -> Result<Vec<String>, String> {
    let Some(runs) = run_dir.parent() else {
        return Ok(vec![run_id_of(run_dir)]);
    };
    let mut tree = vec![run_id_of(run_dir)];
    let mut dir = run_dir.to_path_buf();
    let mut depth = 0;
    loop {
        let parent = crate::run_config::read_run_config(&dir)
            .map_err(|e| format!("the config of run `{}`: {e}", tree[tree.len() - 1]))?
            .parent_run;
        let Some(parent) = parent else { break };
        if !apb_core::registry::is_safe_segment(&parent) || tree.contains(&parent) {
            return Err(format!(
                "run `{}` names parent `{parent}`",
                tree[tree.len() - 1]
            ));
        }
        if depth == MAX_TREE_DEPTH {
            return Err(format!("ancestors beyond {MAX_TREE_DEPTH} levels"));
        }
        depth += 1;
        dir = runs.join(&parent);
        if !dir.is_dir() {
            return Err(format!("parent run `{parent}` is not here"));
        }
        tree.push(parent);
    }
    let mut level = tree.clone();
    for _ in 0..2 * MAX_TREE_DEPTH {
        let mut next = Vec::new();
        for id in &level {
            let events = crate::event::read_all_lossy_tail(&runs.join(id))
                .map_err(|e| format!("the journal of run `{id}`: {e}"))?;
            for e in events {
                if let crate::event::EventPayload::ChildRunStarted { run_id, .. } = e.payload
                    && apb_core::registry::is_safe_segment(&run_id)
                    && !tree.contains(&run_id)
                {
                    tree.push(run_id.clone());
                    next.push(run_id);
                }
            }
        }
        if next.is_empty() {
            return Ok(tree);
        }
        level = next;
    }
    Err(format!("descendants beyond {} levels", 2 * MAX_TREE_DEPTH))
}
