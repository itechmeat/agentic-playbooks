//! Channel for human_review decisions (`reviews.jsonl`). Mirrors control.rs:
//! decision makers (`apb review`, MCP review_decide, HTTP) append their
//! decision here, while drive only reads it and, based on it, writes a
//! ReviewDecided event. This does not violate the single-writer rule for
//! events: events.jsonl is still written only by drive.

use std::fs::File;
use std::io::{BufRead, BufReader, Write};
use std::path::Path;

use serde::{Deserialize, Serialize};

pub mod channel;

pub use channel::open_request;

use crate::decision_channel::DecisionChannel;
use crate::error::EngineError;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReviewCommand {
    pub node: String,
    pub decision: String,
    #[serde(default)]
    pub note: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReviewEntry {
    pub seq: u64,
    #[serde(flatten)]
    pub cmd: ReviewCommand,
    /// The nonce of the plan a `publish` decision answers (see
    /// `apb_core::publish::nonce`), stamped by [`post_decision`]. `None` for
    /// every `human_review` decision and in old channels.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub nonce: Option<String>,
    /// Who decided when it was not a person: `unattended` for the gate's
    /// declared outcome in an unattended run, `model` for an enforced
    /// review recommendation, stamped by the engine alone
    /// ([`post_engine_decision`]); no decision surface can set it.
    /// `None` for a person's decision and in old channels; a value this
    /// binary does not know reads as `unknown`, never as a person.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::event::lenient_decided_by"
    )]
    pub by: Option<crate::event::DecidedBy>,
    /// The journal seq of the request this decision answers, stamped by
    /// [`post_decision`] for a bound or person-only gate. The drive consumes
    /// such a gate's decision only from an entry stamped with the open
    /// request. `None` in old channels.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub request: Option<u64>,
    /// The channel the decision came through, stamped by [`post_decision`]
    /// for a bound or person-only gate. `None` in old channels; a name this
    /// binary does not know reads as `unknown`, never as a person's channel.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::decision_channel::lenient_channel"
    )]
    pub channel: Option<crate::decision_channel::DecisionChannel>,
}

/// Rejects a decision that names something other than a currently pending gate
/// of this run (issue #103.1).
///
/// The check lives here rather than in any one caller because `post_review` is
/// the single entry point of every decision surface (`apb review`, MCP
/// `review_decide`, `POST /api/runs/{id}/review`): before this, all three
/// happily wrote a decision for a node that does not exist, is not a gate, or
/// has no open request, returned a `posted_seq` that looks like success, and
/// left a record no drive would ever consume.
///
/// "Pending" is the same predicate every reporting surface already uses
/// (`progress::compute_with`): more `ReviewRequested` than `ReviewDecided`
/// events for the node. So a caller that was told a gate is pending can always
/// decide it, and nothing else is accepted. A pending gate whose decision is
/// already queued in the channel is no longer waiting for one, so the check
/// counts the channel too and refuses the duplicate.
///
/// Both reads are "cannot judge means accept". A run with no playbook snapshot
/// (pre-snapshot runs, and the bare run dirs the channel's own tests build) has
/// nothing to validate against, and a journal that will not read - most
/// plausibly a torn trailing line the drive is in the middle of appending -
/// cannot answer whether the gate is pending. `post_review` never read
/// `events.jsonl` at all before this check existed, so failing the write on
/// either would be a new way for a perfectly valid decision to be refused,
/// in exactly the live-run race this change set removes elsewhere. Rejecting a
/// decision is reserved for a journal that positively says the gate is not
/// waiting. A publish gate is the exception: its consent is consumed only by
/// the nonce this check stamps, so a decision for a publish node whose
/// journal or channel cannot be read now is a conflict to retry, never an
/// unstamped entry no drive would consume. Without the snapshot the journal
/// tells a publish node by the plan it journaled. Who decides does not
/// depend on the snapshot: a `publish` consent from inside a run step is
/// refused for a publish node, and for any node when the snapshot cannot
/// say what the node is.
fn check_review_target(
    run_dir: &Path,
    cmd: &ReviewCommand,
    nonce: Option<String>,
    poster: Poster,
) -> Result<Stamps, EngineError> {
    use apb_core::publish::OPTION_PUBLISH;
    use apb_core::schema::NodeKind;

    let node = cmd.node.as_str();
    let consent = cmd.decision == OPTION_PUBLISH;
    // Whether the node is a publish node, and whether a snapshot said so.
    let (publish, snapshot) = match crate::progress::load_run_playbook(run_dir) {
        Some(playbook) => {
            let gate = playbook.node(node);
            let kind = gate.map(|n| &n.kind);
            let publish = matches!(kind, Some(NodeKind::Publish { .. }));
            if !publish && !matches!(kind, Some(NodeKind::HumanReview { .. })) {
                return Err(EngineError::NotFound(format!(
                    "node `{node}` is not a human_review or publish node of this run's playbook `{}`",
                    playbook.id
                )));
            }
            if let Some(gate) = gate.filter(|g| is_stamped_gate(g)) {
                if gate.person_only {
                    crate::review::channel::refuse_for_person_only(run_dir, node, poster.channel)?;
                }
                if !publish {
                    return check_bound_decision(run_dir, gate, cmd, nonce, poster);
                }
            }
            if !publish && nonce.is_some() {
                return Err(EngineError::Invalid(format!(
                    "node `{node}` is a human_review node; a nonce answers a publish node only"
                )));
            }
            (publish, true)
        }
        None => {
            if consent || nonce.is_some() {
                refuse_step_consent()?;
            }
            match crate::event::read_all(run_dir) {
                Ok(events) => (crate::publish::latest_plan(&events, node).is_some(), false),
                Err(_) if consent || nonce.is_some() => {
                    return Err(EngineError::Conflict(format!(
                        "the journal of node `{node}` cannot be read right now; decide again"
                    )));
                }
                // Nothing to judge by: accept, as before snapshots existed.
                Err(_) => return Ok(Stamps::nonce(nonce)),
            }
        }
    };
    if publish {
        check_publish_decision(cmd)?;
        if consent {
            refuse_step_consent()?;
        }
    } else if !snapshot {
        return Ok(Stamps::nonce(nonce));
    }

    let unreadable = |what: &str| -> Result<Stamps, EngineError> {
        if publish {
            Err(EngineError::Conflict(format!(
                "the {what} of publish node `{node}` cannot be read right now; decide again"
            )))
        } else {
            Ok(Stamps::nonce(nonce.clone()))
        }
    };
    let Ok(events) = crate::event::read_all(run_dir) else {
        return unreadable("journal");
    };
    let open = crate::event::review_open_count(&events, node);
    let decided = crate::event::review_decided_count(&events, node);
    if open == 0 {
        return Err(EngineError::Conflict(format!(
            "node `{node}` has no review decision pending"
        )));
    }
    let Ok(queued) = read_reviews_after(run_dir, None) else {
        return unreadable("review channel");
    };
    if publish {
        let nonce = check_publish_nonce(&events, &queued, node, cmd, nonce)?;
        return Ok(publish_stamps(&events, node, nonce, poster, run_dir));
    }

    // The journal alone cannot see a decision that is already sitting in the
    // channel and has not been folded into a `ReviewDecided` event yet, so
    // requested-vs-decided would accept a second decision for the same open
    // request. The drive consumes the `decided`-th record for the node
    // (`scheduler::review_gate`, `Consume::Nth`), which is exactly the count
    // this reads back: everything past that is queued and unconsumed. Once
    // the queue already answers every outstanding request, another decision
    // is not an answer, it is a stale extra a cyclic gate would consume on
    // its NEXT visit without anyone confirming it.
    let unconsumed = queued
        .iter()
        .filter(|e| e.cmd.node == node)
        .count()
        .saturating_sub(decided);
    if unconsumed >= open {
        return Err(EngineError::Conflict(format!(
            "a decision is already queued for node `{node}`"
        )));
    }
    Ok(Stamps::default())
}

/// Who posts a decision: the channel it came through, and whether it is the
/// engine's own decision (an unattended run's declared one, an enforced
/// recommendation), which takes the open binding without naming it.
#[derive(Debug, Clone, Copy)]
struct Poster {
    channel: Option<DecisionChannel>,
    engine: bool,
}

/// What [`post_decision`] stamps on a channel entry.
#[derive(Debug, Default)]
struct Stamps {
    nonce: Option<String>,
    request: Option<u64>,
    channel: Option<DecisionChannel>,
}

impl Stamps {
    fn nonce(nonce: Option<String>) -> Stamps {
        Stamps {
            nonce,
            ..Stamps::default()
        }
    }
}

/// Whether the drive consumes a gate's decisions only from entries stamped
/// with the open request and the channel: a gate that binds snapshots
/// (`approves`) or that only a person decides (`person_only`).
pub fn is_stamped_gate(gate: &apb_core::schema::Node) -> bool {
    gate.person_only || gate.approves.is_some()
}

/// A publish decision on a person-only node carries the open request and
/// the channel; any other keeps only its nonce.
fn publish_stamps(
    events: &[crate::event::Event],
    node: &str,
    nonce: Option<String>,
    poster: Poster,
    run_dir: &Path,
) -> Stamps {
    let person_only = crate::progress::load_run_playbook(run_dir)
        .and_then(|p| p.node(node).map(|n| n.person_only))
        .unwrap_or(false);
    if !person_only {
        return Stamps::nonce(nonce);
    }
    Stamps {
        nonce,
        request: latest_request(events, node),
        channel: poster.channel,
    }
}

/// The seq of the gate's latest `review_requested`.
fn latest_request(events: &[crate::event::Event], node: &str) -> Option<u64> {
    events.iter().rev().find_map(|e| match &e.payload {
        crate::event::EventPayload::ReviewRequested { node: n, .. } if n == node => Some(e.seq),
        _ => None,
    })
}

/// The decision on a gate that binds snapshots or that only a person
/// decides, under the reviews lock: it answers the open request, which
/// cannot be judged on a journal or channel that does not read (a
/// conflict to retry, never "accept"). On a bound request an approving
/// option needs the binding's nonce, a non-approving one without a nonce
/// is stamped with it, a stale nonce is a conflict naming the current
/// binding, and a second decision for the request is refused. Entries no
/// poster stamped (a hand-written line, an older apb) neither answer nor
/// block the request.
fn check_bound_decision(
    run_dir: &Path,
    gate: &apb_core::schema::Node,
    cmd: &ReviewCommand,
    nonce: Option<String>,
    poster: Poster,
) -> Result<Stamps, EngineError> {
    let node = gate.id.as_str();
    let events = crate::event::read_all(run_dir).map_err(|_| {
        EngineError::Conflict(format!(
            "the journal of gate `{node}` cannot be read right now; decide again"
        ))
    })?;
    let Some((request, binding)) = open_request(&events, node) else {
        return Err(EngineError::Conflict(format!(
            "node `{node}` has no review decision pending"
        )));
    };
    let queued = read_reviews_after(run_dir, None).map_err(|_| {
        EngineError::Conflict(format!(
            "the review channel of gate `{node}` cannot be read right now; decide again"
        ))
    })?;
    if queued.iter().any(|e| {
        e.cmd.node == node && answers_stamped(run_dir, e, request, binding, gate.person_only)
    }) {
        return Err(EngineError::Conflict(format!(
            "a decision is already queued for the request of node `{node}` at seq {request}"
        )));
    }
    let nonce = match binding {
        Some(b) => {
            let approving = match (&gate.approves, &gate.kind) {
                (Some(a), apb_core::schema::NodeKind::HumanReview { options, .. }) => {
                    a.approving_options(options).contains(&cmd.decision)
                }
                _ => false,
            };
            let given = match nonce {
                Some(n) => n,
                None if !approving || poster.engine => b.nonce.clone(),
                None => {
                    return Err(EngineError::Invalid(format!(
                        "approving gate `{node}` needs the nonce of the snapshots the person saw: the open request binds {} with nonce `{}`",
                        bound_list(b),
                        b.nonce
                    )));
                }
            };
            if given != b.nonce {
                return Err(EngineError::Conflict(format!(
                    "nonce `{given}` does not answer the open request of gate `{node}`: it binds {} with nonce `{}`; show the person those snapshots and decide again",
                    bound_list(b),
                    b.nonce
                )));
            }
            Some(given)
        }
        None if nonce.is_some() => {
            return Err(EngineError::Invalid(format!(
                "the open request of gate `{node}` binds no snapshot; a nonce answers a bound gate or a publish node only"
            )));
        }
        None => None,
    };
    Ok(Stamps {
        nonce,
        request: Some(request),
        channel: poster.channel,
    })
}

/// Whether channel entry `e` answers the stamped request at seq `request`
/// of its gate: stamped with that request, with the request's binding
/// nonce on a bound request, and through a channel that decides a
/// person-only gate when the gate is one. The drive consumes nothing else
/// for such a gate, and nothing else stands in the way of a decision.
pub(crate) fn answers_stamped(
    run_dir: &Path,
    e: &ReviewEntry,
    request: u64,
    binding: Option<&crate::approval::ReviewBinding>,
    person_only: bool,
) -> bool {
    e.request == Some(request)
        && binding.is_none_or(|b| e.nonce.as_deref() == Some(b.nonce.as_str()))
        && (!person_only || crate::review::channel::decides_person_only(run_dir, e.channel))
}

/// The bound snapshots of a binding, for a message.
fn bound_list(b: &crate::approval::ReviewBinding) -> String {
    let names: Vec<String> = b
        .snapshots
        .iter()
        .map(|s| format!("`{}` snapshot {}", s.node, s.snapshot_id))
        .collect();
    if names.is_empty() {
        "no snapshot".to_string()
    } else {
        names.join(", ")
    }
}

/// A publish decision is `publish` or `skip` (a run step's `publish` is
/// already refused by [`refuse_step_consent`]).
fn check_publish_decision(cmd: &ReviewCommand) -> Result<(), EngineError> {
    use apb_core::publish::{OPTIONS, is_option};
    if !is_option(&cmd.decision) {
        return Err(EngineError::Invalid(format!(
            "`{}` is not a decision of publish node `{}`; choose one of {}",
            cmd.decision,
            cmd.node,
            OPTIONS.join(", ")
        )));
    }
    Ok(())
}

/// A publish consent is a person's: one posted from inside a run step
/// (`APB_RUN_ID` or `APB_HOST_TASK` set in the posting process) is refused,
/// whatever the run's snapshot says.
fn refuse_step_consent() -> Result<(), EngineError> {
    match apb_core::step_env::StepCaller::from_env().refusal("publishing") {
        Some(why) => Err(EngineError::Invalid(why)),
        None => Ok(()),
    }
}

/// A publish decision answers exactly the open plan: `publish` must carry
/// its nonce, a `skip` without one is stamped with it, a stale nonce is a
/// conflict naming the current plan, and a second decision for the same
/// nonce is refused. The drive consumes by nonce, so a decision queued for
/// a replaced plan never answers a different one. The nonce names the plan,
/// not the request: a decision queued just before a supervisor withdrew the
/// request answers a later plan only when it is identical (the same push to
/// the same remote state), which is the push the person consented to.
fn check_publish_nonce(
    events: &[crate::event::Event],
    queued: &[ReviewEntry],
    node: &str,
    cmd: &ReviewCommand,
    nonce: Option<String>,
) -> Result<Option<String>, EngineError> {
    use crate::event::EventPayload;
    let Some(EventPayload::PublishPlanned {
        nonce: open,
        plan_digest,
        branch,
        after,
        url,
        ..
    }) = crate::publish::latest_plan(events, node).map(|e| &e.payload)
    else {
        return Err(EngineError::Conflict(format!(
            "publish node `{node}` has no plan pending"
        )));
    };
    let given = match nonce {
        Some(n) => n,
        None if cmd.decision == apb_core::publish::OPTION_SKIP => open.clone(),
        None => {
            return Err(EngineError::Invalid(format!(
                "publishing needs the nonce of the plan the person saw; the current plan of node `{node}` pushes {after} to `{url}` branch `{branch}` with nonce `{open}`"
            )));
        }
    };
    if given != *open {
        return Err(EngineError::Conflict(format!(
            "nonce `{given}` does not answer the current plan of publish node `{node}`: it pushes {after} to `{url}` branch `{branch}` with nonce `{open}`; show the person that plan and decide again"
        )));
    }
    let decided = events
        .iter()
        .filter(|e| {
            matches!(&e.payload, EventPayload::ReviewDecided { node: n, plan_digest: Some(d), .. }
                if n == node && d == plan_digest)
        })
        .count();
    let queued_for_nonce = queued
        .iter()
        .filter(|e| e.cmd.node == node && e.nonce.as_deref() == Some(open.as_str()))
        .count();
    if queued_for_nonce > decided {
        return Err(EngineError::Conflict(format!(
            "a decision is already queued for the plan `{open}` of node `{node}`"
        )));
    }
    Ok(Some(given))
}

/// Queues a decision without a nonce and without a stated channel. Such a
/// decision never decides a person-only gate.
pub fn post_review(run_dir: &Path, cmd: ReviewCommand) -> Result<u64, EngineError> {
    post_decision(run_dir, cmd, None)
}

/// Queues a decision, with the nonce of the plan it answers for a
/// `publish` gate. The single entry point of every decision surface.
pub fn post_decision(
    run_dir: &Path,
    cmd: ReviewCommand,
    nonce: Option<String>,
) -> Result<u64, EngineError> {
    post_decision_to(run_dir, cmd, nonce, None)
}

/// [`post_decision`] naming the request it answers: `request` is the
/// journal seq of the `review_requested` the caller was shown. Checked
/// inside the lock that guards the append, against the journal and the
/// queued decisions: a request a later round replaced (or one withdrawn)
/// is refused with `Conflict`, one that already has a decision, queued or
/// journaled, with `AlreadyAnswered`. A publish decision still carries its
/// nonce. `None` keeps [`post_decision`]'s behaviour.
pub fn post_decision_to(
    run_dir: &Path,
    cmd: ReviewCommand,
    nonce: Option<String>,
    request: Option<u64>,
) -> Result<u64, EngineError> {
    crate::review::channel::refuse_own_run(run_dir, &apb_core::step_env::StepCaller::from_env())?;
    append_decision(
        run_dir,
        cmd,
        nonce,
        request,
        None,
        Poster {
            channel: None,
            engine: false,
        },
    )
}

/// [`post_decision_to`] stating the channel the decision came through: the
/// entry point every decision surface uses. A person-only gate refuses a
/// channel that is not a person's ([`EngineError::ChannelRefused`]); a
/// decision from a step of the run's own tree is refused for every gate
/// (`run_step_own_run`). A bound or person-only gate's entry is stamped
/// with the open request and the channel; the drive consumes nothing else
/// for such a gate.
pub fn post_decision_via(
    run_dir: &Path,
    cmd: ReviewCommand,
    nonce: Option<String>,
    request: Option<u64>,
    channel: DecisionChannel,
) -> Result<u64, EngineError> {
    if channel != DecisionChannel::Scenario {
        crate::review::channel::refuse_own_run(
            run_dir,
            &apb_core::step_env::StepCaller::from_env(),
        )?;
    }
    append_decision(
        run_dir,
        cmd,
        nonce,
        request,
        None,
        Poster {
            channel: Some(channel),
            engine: false,
        },
    )
}

/// Queues a decision the engine itself makes for the request the journal
/// holds at seq `request`, stamped `by`: `unattended` for the declared
/// `unattended: { decide }` outcome of a gate in an unattended run, `model`
/// for an enforced review recommendation. The engine's own path, never a
/// decision surface's: the same lock and checks as a person's decision
/// bound to a request, so a decision already queued for it (a person's, or
/// this one posted before a crash) wins and this one is refused
/// `AlreadyAnswered`.
pub(crate) fn post_engine_decision(
    run_dir: &Path,
    cmd: ReviewCommand,
    request: u64,
    by: crate::event::DecidedBy,
) -> Result<u64, EngineError> {
    let channel = match by {
        crate::event::DecidedBy::Unattended => DecisionChannel::Unattended,
        _ => DecisionChannel::Engine,
    };
    append_decision(
        run_dir,
        cmd,
        None,
        Some(request),
        Some(by),
        Poster {
            channel: Some(channel),
            engine: true,
        },
    )
}

fn append_decision(
    run_dir: &Path,
    cmd: ReviewCommand,
    nonce: Option<String>,
    request: Option<u64>,
    by: Option<crate::event::DecidedBy>,
    poster: Poster,
) -> Result<u64, EngineError> {
    crate::driver::ensure_run_dir(run_dir)?;
    // The check and the append under one lock: two posters (a person and
    // the engine's automatic decision, issue #165 Part 14.4) that both pass
    // the check would queue two decisions for one visit, and the second
    // would decide the gate's next visit unseen.
    let _lock = apb_core::fsutil::lock_dir(run_dir, "reviews.jsonl.lock")?;
    if let Some(seq) = request {
        check_review_request(run_dir, &cmd.node, seq)?;
    }
    let stamps = check_review_target(run_dir, &cmd, nonce, poster)?;

    let seq = read_reviews_after(run_dir, None)?.len() as u64;

    let entry = ReviewEntry {
        seq,
        cmd,
        nonce: stamps.nonce,
        by,
        request: stamps.request,
        channel: stamps.channel,
    };
    let line = serde_json::to_string(&entry).map_err(|e| EngineError::Yaml(e.to_string()))?;

    let path = run_dir.join("reviews.jsonl");
    let mut file = apb_core::fsutil::open_append_private(&path)?;
    writeln!(file, "{line}")?;
    file.flush()?;

    Ok(seq)
}

/// Whether the review request opened at journal seq `seq` for `node` is
/// still the one waiting for a decision. Runs under the reviews lock.
fn check_review_request(run_dir: &Path, node: &str, seq: u64) -> Result<(), EngineError> {
    use crate::event::EventPayload;
    let events = crate::event::read_all_lossy_tail(run_dir)?;
    let requested: Vec<u64> = events
        .iter()
        .filter(
            |e| matches!(&e.payload, EventPayload::ReviewRequested { node: n, .. } if n == node),
        )
        .map(|e| e.seq)
        .collect();
    if !requested.contains(&seq) {
        return Err(EngineError::NotFound(format!(
            "node `{node}` requested no review at journal seq {seq}"
        )));
    }
    let latest = requested.last().copied().unwrap_or(seq);
    if latest != seq {
        return Err(EngineError::Conflict(format!(
            "the review of node `{node}` at seq {seq} is over; the node now asks at seq {latest}"
        )));
    }
    let open = crate::event::review_open_count(&events, node);
    let decided = crate::event::review_decided_count(&events, node);
    if open == 0 {
        // Closed: by a decision, or withdrawn without one.
        let decided_last = events.iter().rev().find_map(|e| match &e.payload {
            EventPayload::ReviewDecided { node: n, .. } if n == node => Some(true),
            EventPayload::ReviewWithdrawn { node: n, .. } if n == node => Some(false),
            _ => None,
        });
        return Err(if decided_last == Some(true) {
            EngineError::AlreadyAnswered(format!(
                "the review of node `{node}` at seq {seq} is already decided"
            ))
        } else {
            EngineError::Conflict(format!(
                "the review of node `{node}` at seq {seq} is no longer open"
            ))
        });
    }
    // A publish gate consumes by nonce: a decision queued for an older plan
    // never answers the current one, so only the current plan's backlog
    // counts; a request that already has a decision for it is answered.
    if let Some(EventPayload::PublishPlanned {
        nonce, plan_digest, ..
    }) = crate::publish::latest_plan(&events, node).map(|e| &e.payload)
    {
        let decided_for_plan = events
            .iter()
            .filter(|e| {
                matches!(&e.payload, EventPayload::ReviewDecided { node: n, plan_digest: Some(d), .. }
                    if n == node && d == plan_digest)
            })
            .count();
        let queued_for_plan = read_reviews_after(run_dir, None)?
            .iter()
            .filter(|e| e.cmd.node == node && e.nonce.as_deref() == Some(nonce.as_str()))
            .count();
        if queued_for_plan > decided_for_plan {
            return Err(EngineError::AlreadyAnswered(format!(
                "the review of node `{node}` at seq {seq} already has a decision queued"
            )));
        }
        return Ok(());
    }
    // A bound or person-only gate is answered only by an entry stamped
    // with this request: an unstamped line neither answers nor blocks it.
    let stamped = crate::progress::load_run_playbook(run_dir)
        .and_then(|p| p.node(node).filter(|g| is_stamped_gate(g)).cloned());
    if let Some(gate) = stamped {
        let binding = open_request(&events, node).and_then(|(_, b)| b);
        if read_reviews_after(run_dir, None)?.iter().any(|e| {
            e.cmd.node == node && answers_stamped(run_dir, e, seq, binding, gate.person_only)
        }) {
            return Err(EngineError::AlreadyAnswered(format!(
                "the review of node `{node}` at seq {seq} already has a decision queued"
            )));
        }
        return Ok(());
    }
    let queued = read_reviews_after(run_dir, None)?
        .iter()
        .filter(|e| e.cmd.node == node)
        .count()
        .saturating_sub(decided);
    if queued >= open {
        return Err(EngineError::AlreadyAnswered(format!(
            "the review of node `{node}` at seq {seq} already has a decision queued"
        )));
    }
    Ok(())
}

pub fn read_reviews_after(
    run_dir: &Path,
    after_seq: Option<u64>,
) -> Result<Vec<ReviewEntry>, EngineError> {
    let path = run_dir.join("reviews.jsonl");
    if !path.is_file() {
        return Ok(Vec::new());
    }

    let mut out = Vec::new();
    for line in BufReader::new(File::open(&path)?).lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let entry: ReviewEntry =
            serde_json::from_str(&line).map_err(|e| EngineError::Yaml(e.to_string()))?;

        if let Some(threshold) = after_seq {
            if entry.seq > threshold {
                out.push(entry);
            }
        } else {
            out.push(entry);
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn post_and_read_reviews_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let s0 = post_review(
            dir.path(),
            ReviewCommand {
                node: "gate".into(),
                decision: "approved".into(),
                note: "a".into(),
            },
        )
        .unwrap();
        let s1 = post_review(
            dir.path(),
            ReviewCommand {
                node: "gate2".into(),
                decision: "rejected".into(),
                note: "b".into(),
            },
        )
        .unwrap();
        assert_eq!(s0, 0);
        assert_eq!(s1, 1);

        let all = read_reviews_after(dir.path(), None).unwrap();
        assert_eq!(all.len(), 2);
        assert_eq!(all[0].cmd.node, "gate");

        let after = read_reviews_after(dir.path(), Some(s0)).unwrap();
        assert_eq!(after.len(), 1);
        assert_eq!(after[0].cmd.decision, "rejected");
    }
}
