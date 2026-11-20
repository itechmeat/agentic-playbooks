//! Channel for human_review decisions (`reviews.jsonl`). Mirrors control.rs:
//! decision makers (`apb review`, MCP review_decide, HTTP) append their
//! decision here, while drive only reads it and, based on it, writes a
//! ReviewDecided event. This does not violate the single-writer rule for
//! events: events.jsonl is still written only by drive.

use std::io::Write;
use std::path::Path;

use serde::{Deserialize, Serialize};

pub mod channel;
pub mod form;
pub mod passkey_proof;
pub mod scope;
pub mod views;

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
    /// The credential id prefix of the passkey whose assertion this
    /// decision carried, stamped only by the dashboard route after it
    /// verified the assertion. While this machine holds at least one
    /// passkey, a person-only or bound gate consumes nothing else.
    /// `None` in old channels and whenever no passkey was used.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub passkey: Option<String>,
    /// The full WebAuthn assertion the decision was minted from, so the
    /// drive re-verifies the stamp at consumption instead of trusting the
    /// string above (a plain JSONL line is writable by any same-user
    /// process). `None` in old channels and whenever no passkey was used.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub passkey_proof: Option<passkey_proof::PasskeyProof>,
    /// The field values the decision carried, checked when it was posted
    /// and scrubbed before they are journaled (people-in-the-loop D3).
    /// Empty for a gate without fields and in old channels.
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub fields: std::collections::BTreeMap<String, serde_json::Value>,
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
    fields: &std::collections::BTreeMap<String, serde_json::Value>,
    named: Option<u64>,
    poster: Poster,
) -> Result<Stamps, EngineError> {
    use apb_core::publish::OPTION_PUBLISH;
    use apb_core::schema::NodeKind;

    let node = cmd.node.as_str();
    let consent = cmd.decision == OPTION_PUBLISH;
    // Whether the node is a publish node, and whether a snapshot said so.
    // `formed`: the gate uses a form feature
    // (`NodeKind::needs_review_detail`), whose checks fail closed on
    // a journal that does not read.
    // One read of the run's definition: the classification below and the
    // form's checks at the end judge by the same snapshot.
    let run_playbook = crate::progress::load_run_playbook(run_dir);
    let (publish, snapshot, formed) = match &run_playbook {
        Some(playbook) => {
            let gate = playbook.node(node);
            let kind = gate.map(|n| &n.kind);
            let publish = matches!(kind, Some(NodeKind::Publish { .. }));
            let formed = kind.is_some_and(apb_core::schema::NodeKind::needs_review_detail);
            let pilot = gate.is_some_and(|g| g.list().is_some_and(|l| l.pilot.is_some()));
            if !publish && !matches!(kind, Some(NodeKind::HumanReview { .. })) && !pilot {
                return Err(EngineError::NotFound(format!(
                    "node `{node}` is not a human_review, publish or list pilot node of this run's playbook `{}`",
                    playbook.id
                )));
            }
            if pilot {
                // A pilot gate takes only its three options, and a nonce
                // answers a publish node only.
                if !apb_core::list::PILOT_OPTIONS.contains(&cmd.decision.as_str()) {
                    return Err(EngineError::Invalid(format!(
                        "node `{node}` is a list pilot gate; decide continue, revise or stop"
                    )));
                }
                if nonce.is_some() {
                    return Err(EngineError::Invalid(format!(
                        "node `{node}` is a list pilot gate; a nonce answers a publish node only"
                    )));
                }
            }
            if let Some(gate) = gate.filter(|g| consumes_stamped(g)) {
                if gate.person_only {
                    crate::review::channel::refuse_for_person_only(
                        run_dir,
                        node,
                        poster.channel,
                        poster.proof.is_some(),
                    )?;
                }
                if !publish {
                    let stamps = check_bound_decision(run_dir, gate, cmd, nonce, named, poster)?;
                    // The form's checks hold for a stamped gate too.
                    check_form(run_dir, gate, cmd, fields)?;
                    return Ok(stamps);
                }
            }
            if !publish && nonce.is_some() {
                return Err(EngineError::Invalid(format!(
                    "node `{node}` is a human_review node; a nonce answers a publish node only"
                )));
            }
            (publish, true, formed)
        }
        None => {
            if consent || nonce.is_some() {
                refuse_step_consent()?;
            }
            // Field values are checked against the gate's definition: a
            // post that carries them waits for a definition that reads,
            // never lands unchecked for the drive to refuse later.
            if !fields.is_empty() {
                return Err(EngineError::Conflict(format!(
                    "the playbook of node `{node}` cannot be read right now; decide again"
                )));
            }
            match crate::event::read_all(run_dir) {
                // A request whose journal holds its form detail is a form
                // gate's: its checks need the definition too.
                Ok(events) if crate::review::form::detail_of(&events, node).is_some() => {
                    return Err(EngineError::Conflict(format!(
                        "the playbook of node `{node}` cannot be read right now; decide again"
                    )));
                }
                Ok(events) => {
                    // A run whose run guard names a review form feature or
                    // the list pilot never lets its unreadable snapshot
                    // make a gate plain: the post is refused until the
                    // write-once snapshot reads again. A publish node is
                    // answered by its nonce, not its definition.
                    if crate::publish::latest_plan(&events, node).is_none()
                        && crate::event::latest_run_guard(&events).is_some_and(|guard| {
                            guard
                                .iter()
                                .any(|f| form_guard_features().contains(&f.as_str()))
                        })
                    {
                        return Err(EngineError::Conflict(scrubbed_refusal(
                            run_dir,
                            decide_again_unreadable_definition(node),
                        )));
                    }
                    (
                        crate::publish::latest_plan(&events, node).is_some(),
                        false,
                        false,
                    )
                }
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
        if publish || formed {
            Err(EngineError::Conflict(format!(
                "the {what} of node `{node}` cannot be read right now; decide again"
            )))
        } else {
            Ok(Stamps::nonce(nonce.clone()))
        }
    };
    let Ok(events) = crate::event::read_all(run_dir) else {
        return unreadable("journal");
    };
    let open = crate::event::review_open_count(&events, node);
    let consumed = crate::event::review_consumed_count(&events, node);
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
    // request. The drive takes the record at the node's consumed count
    // (`scheduler::review_gate`, `Consume::Nth`: one per decision and one per
    // entry a scope re-ask set aside, `review_consumed_count`), which is
    // exactly the count this reads back: everything past that is queued and
    // unconsumed. Once the queue already answers every outstanding request,
    // another decision is not an answer, it is a stale extra a cyclic gate
    // would consume on its NEXT visit without anyone confirming it.
    let unconsumed = queued
        .iter()
        .filter(|e| e.cmd.node == node)
        .count()
        .saturating_sub(consumed);
    if unconsumed >= open {
        return Err(EngineError::Conflict(format!(
            "a decision is already queued for node `{node}`"
        )));
    }
    if let Some(gate) = run_playbook.as_ref().and_then(|p| p.node(node)) {
        check_form(run_dir, gate, cmd, fields)?;
    }
    // The channel is stamped on every gate's entry, so the decision is
    // journaled with `via` and a scenario's scripted one stays apart from
    // a person's on an ordinary gate too.
    Ok(Stamps {
        channel: poster.channel,
        ..Stamps::default()
    })
}

/// The post-time checks of a gate's form (people-in-the-loop D2/D3), run
/// for every `human_review` gate, stamped ones included, once the request
/// is known to be open:
///
/// - an option `max_scope` withholds above the scope is refused, not
///   hidden: every channel refuses it, the engine's own decisions
///   included, so an enforced recommendation of a withheld option waits
///   for a person;
/// - the values a decision carries are checked against the gate's
///   `fields` and the request's rendered defaults.
///
/// Fail closed while the open request has no `review_detail` (an older
/// journal; the drive journals the detail before it waits): no option
/// `max_scope` may withhold is accepted, and the values are checked
/// against the definition with no defaults. A gate without `fields`
/// takes no values.
///
/// Every refusal it returns passes the run's redactor first
/// ([`crate::review::form::refusal_text`]): a gate or field name, an
/// option word and a withheld reason are definition text, and a
/// hand-written definition may carry anything.
fn check_form(
    run_dir: &Path,
    gate: &apb_core::schema::Node,
    cmd: &ReviewCommand,
    fields: &std::collections::BTreeMap<String, serde_json::Value>,
) -> Result<(), EngineError> {
    check_form_unscrubbed(run_dir, gate, cmd, fields).map_err(|e| {
        let redactor = crate::run_redact::RunRedactor::for_run_dir(run_dir);
        let scrub = |s: String| crate::review::form::refusal_text(&redactor, &s);
        match e {
            EngineError::Invalid(s) => EngineError::Invalid(scrub(s)),
            EngineError::Conflict(s) => EngineError::Conflict(scrub(s)),
            other => other,
        }
    })
}

fn check_form_unscrubbed(
    run_dir: &Path,
    gate: &apb_core::schema::Node,
    cmd: &ReviewCommand,
    fields: &std::collections::BTreeMap<String, serde_json::Value>,
) -> Result<(), EngineError> {
    let node = gate.id.as_str();
    let apb_core::schema::NodeKind::HumanReview {
        fields: spec,
        max_scope,
        options,
        ..
    } = &gate.kind
    else {
        // A list pilot gate takes no fields either: its decision is one
        // option word, the same text a `human_review` without fields
        // gives.
        if gate.list().is_some_and(|l| l.pilot.is_some()) && !fields.is_empty() {
            return Err(EngineError::Invalid(format!(
                "gate `{node}` takes no fields"
            )));
        }
        return Ok(());
    };
    if spec.is_empty() && !fields.is_empty() {
        return Err(EngineError::Invalid(format!(
            "gate `{node}` takes no fields"
        )));
    }
    // A gate without a form feature journals no detail
    // (`NodeKind::needs_review_detail`): nothing more to check.
    if !gate.kind.needs_review_detail() {
        return Ok(());
    }
    let events = crate::event::read_all(run_dir).map_err(|_| {
        EngineError::Conflict(format!(
            "the journal of gate `{node}` cannot be read right now; decide again"
        ))
    })?;
    // A gate that uses a form feature and whose request has no detail yet
    // (a write torn between the two lines, repaired by the drive's next
    // look) fails closed: it is never read as a gate without the feature.
    let Some(detail) = crate::review::form::detail_of(&events, node) else {
        let may_withhold = max_scope.as_ref().is_some_and(|max| {
            max.withhold_for(&apb_core::schema::effective_review_options(options))
                .contains(&cmd.decision)
        });
        let what = if may_withhold {
            format!(
                "the scope of gate `{node}` is not recorded yet, so option `{}` may be withheld",
                cmd.decision
            )
        } else {
            format!("the form of gate `{node}` is not recorded yet")
        };
        return Err(EngineError::Conflict(format!("{what}; decide again")));
    };
    if detail.withheld.contains(&cmd.decision) {
        let reason = detail.withheld_reason.clone().unwrap_or_default();
        return Err(EngineError::Invalid(format!(
            "option `{}` of gate `{node}` is withheld: {reason}",
            cmd.decision
        )));
    }
    if spec.is_empty() {
        return Ok(());
    }
    apb_core::review_form::check_values(
        spec,
        &cmd.decision,
        fields,
        &crate::review::form::defaults_of(Some(&detail)),
    )
    .map(|_| ())
    .map_err(|e| EngineError::Invalid(format!("gate `{node}`: {e}")))
}

/// Who posts a decision: the channel it came through, and whether it is the
/// engine's own decision (an unattended run's declared one, an enforced
/// recommendation), which takes the open binding without naming it.
#[derive(Debug, Clone)]
struct Poster {
    channel: Option<DecisionChannel>,
    engine: bool,
    /// Set only by the dashboard route after it verified a WebAuthn
    /// assertion for this very decision: the credential id prefix the
    /// entry stamps in `passkey`, and the full assertion it re-verifies at
    /// consumption.
    passkey: Option<String>,
    proof: Option<passkey_proof::PasskeyProof>,
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

/// Whether a decision on `gate` must name the request it answers: a gate
/// that uses a form feature (`fields`, `show` or `max_scope`) and, once
/// list pilots exist, a list gate with a `pilot`. Its entry is stamped with
/// the request and the drive consumes nothing else, so a decision made on
/// a request the gate asked again since never answers the newer one.
pub fn requires_request(gate: &apb_core::schema::Node) -> bool {
    gate.kind.needs_review_detail() || gate.list().is_some_and(|l| l.pilot.is_some())
}

/// Whether `gate` consumes its decisions only from entries stamped with
/// its open request: the stamped gates (`is_stamped_gate`) and every gate
/// whose decision must name its request (`requires_request`).
pub fn consumes_stamped(gate: &apb_core::schema::Node) -> bool {
    is_stamped_gate(gate) || requires_request(gate)
}

/// The refusal code a decision on a request-naming gate carries when it
/// names no request, or one the gate replaced or withdrew: the caller
/// reads the gate's request again and decides on it.
pub const DECIDE_AGAIN: &str = "decide_again";

/// The guarded features whose gate a decision must never read as a plain
/// one while the run's definition snapshot does not read.
fn form_guard_features() -> [&'static str; 3] {
    [
        apb_core::validate::GuardedFeature::ReviewFields.as_str(),
        apb_core::validate::GuardedFeature::ReviewMaxScope.as_str(),
        apb_core::validate::GuardedFeature::ListPilot.as_str(),
    ]
}

/// The refusal texts of [`DECIDE_AGAIN`], built from definition text and
/// passed through the run's redactor as the form's refusals are.
fn decide_again_none_named(node: &str, open: u64) -> String {
    format!(
        "{DECIDE_AGAIN}: gate `{node}` takes a decision only for the request it answers, and this decision names none; read its open request (seq {open}) and decide on it"
    )
}

fn decide_again_replaced(node: &str, seq: u64, latest: u64) -> String {
    format!(
        "{DECIDE_AGAIN}: the request of gate `{node}` at seq {seq} was replaced by the request at seq {latest}; read that request and decide on it"
    )
}

fn decide_again_withdrawn(node: &str, seq: u64) -> String {
    format!(
        "{DECIDE_AGAIN}: the request of gate `{node}` at seq {seq} was withdrawn; read the gate's request again when it asks"
    )
}

fn decide_again_unreadable_definition(node: &str) -> String {
    format!(
        "{DECIDE_AGAIN}: the playbook of node `{node}` cannot be read right now and this run uses review forms or a list pilot; read its request again and decide on it"
    )
}

/// A refusal reason through the run's redactor, as the form's refusals go.
fn scrubbed_refusal(run_dir: &Path, text: String) -> String {
    let redactor = crate::run_redact::RunRedactor::for_run_dir(run_dir);
    crate::review::form::refusal_text(&redactor, &text)
}

/// The refusal for a post that names request seq `s` of `node` while the
/// open request is another one, classified on the same journal read the
/// open request came from: no request at `s` is not found, one the node
/// decided is already answered, one the node withdrew or replaced asks the
/// caller to read the gate's request again.
fn classify_named_request(
    run_dir: &Path,
    events: &[crate::event::Event],
    node: &str,
    s: u64,
    open: u64,
) -> EngineError {
    use crate::event::EventPayload;
    let asked = events.iter().any(|e| {
        e.seq == s
            && matches!(&e.payload, EventPayload::ReviewRequested { node: n, .. } if n == node)
    });
    if !asked {
        return EngineError::NotFound(format!(
            "node `{node}` requested no review at journal seq {s}"
        ));
    }
    // The first later event that says what became of `s`: a decision, a
    // withdrawal, or a new request that replaced it.
    let mut first_close: Option<bool> = None;
    let mut reasked: Option<u64> = None;
    for e in events.iter().filter(|e| e.seq > s) {
        match &e.payload {
            EventPayload::ReviewRequested { node: n, .. } if n == node => {
                reasked = Some(e.seq);
                break;
            }
            EventPayload::ReviewDecided { node: n, .. } if n == node => {
                first_close = Some(true);
                break;
            }
            EventPayload::ReviewWithdrawn { node: n, .. } if n == node => {
                first_close = Some(false);
                break;
            }
            _ => {}
        }
    }
    let conflict = |text: String| EngineError::Conflict(scrubbed_refusal(run_dir, text));
    match (first_close, reasked) {
        (Some(true), _) => EngineError::AlreadyAnswered(format!(
            "the review of node `{node}` at seq {s} is already decided"
        )),
        (Some(false), _) => conflict(decide_again_withdrawn(node, s)),
        (_, Some(latest)) => conflict(decide_again_replaced(node, s, latest)),
        // Nothing later names the request and it is not the open one: fall
        // back to the open request this same read found.
        (None, None) => conflict(decide_again_replaced(node, s, open)),
    }
}

#[cfg(test)]
thread_local! {
    /// Test-only hook: `check_bound_decision` calls it right after its
    /// journal read and nowhere else, so a test can change the journal
    /// between the read and the stamp. A one-shot closure; absent from
    /// every build without `cfg(test)`. Kept because the race it shows (a
    /// request re-asked between the one read and the stamp) cannot be
    /// timed from the integration boundary: the window is a few
    /// instructions wide, and the integration tests only see its outcome.
    static AFTER_BOUND_READ: std::cell::RefCell<Option<Box<dyn FnOnce() + 'static>>> =
        const { std::cell::RefCell::new(None) };
}

/// Runs the test-only hook [`AFTER_BOUND_READ`] once, when set. No-op
/// outside tests.
#[cfg(test)]
pub(crate) fn run_after_bound_read() {
    AFTER_BOUND_READ.with(|h| {
        if let Some(f) = h.borrow_mut().take() {
            f();
        }
    });
}

/// A publish decision carries its nonce and the open request seq: a
/// person-only one also the channel. The request is stamped on every
/// publish entry, person-only or not, because the passkey assertion is
/// minted over a challenge derived from that very request: without the
/// stamp the drive re-derives a different challenge at consumption and no
/// dashboard consent would ever verify.
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
    Stamps {
        nonce,
        request: latest_request(events, node),
        channel: person_only.then_some(poster.channel).flatten(),
    }
}

/// The seq of the gate's latest `review_requested`.
fn latest_request(events: &[crate::event::Event], node: &str) -> Option<u64> {
    events.iter().rev().find_map(|e| match &e.payload {
        crate::event::EventPayload::ReviewRequested { node: n, .. } if n == node => Some(e.seq),
        _ => None,
    })
}

/// The decision on a gate that binds snapshots, that only a person
/// decides, or whose decision must name the request it answers
/// ([`consumes_stamped`]), under the reviews lock: it answers the open
/// request, which cannot be judged on a journal or channel that does not
/// read (a conflict to retry, never "accept"). The request the decision
/// names is compared with the open request on this one journal read, and
/// that seq is what the entry is stamped with: a request asked again
/// between a check and a later stamp never receives a decision made for
/// the one before. One that names no request is refused
/// [`DECIDE_AGAIN`] on a gate where [`requires_request`] holds (the
/// engine's own posts always name one); on the other stamped gates the
/// open request is stamped without being named, as before. On a bound
/// request an approving option needs the binding's nonce, a
/// non-approving one without a nonce is stamped with it, a stale nonce is
/// a conflict naming the current binding, and a second decision for the
/// request is refused. Entries no poster stamped (a hand-written line, an
/// older apb) neither answer nor block the request.
fn check_bound_decision(
    run_dir: &Path,
    gate: &apb_core::schema::Node,
    cmd: &ReviewCommand,
    nonce: Option<String>,
    named: Option<u64>,
    poster: Poster,
) -> Result<Stamps, EngineError> {
    let node = gate.id.as_str();
    let events = crate::event::read_all(run_dir).map_err(|_| {
        EngineError::Conflict(format!(
            "the journal of gate `{node}` cannot be read right now; decide again"
        ))
    })?;
    #[cfg(test)]
    run_after_bound_read();
    let Some((request, binding)) = open_request(&events, node) else {
        // A post naming a request while none is open (withdrawn, not asked
        // again yet) is told what became of it.
        if let Some(s) = named {
            return Err(classify_named_request(run_dir, &events, node, s, s));
        }
        return Err(EngineError::Conflict(format!(
            "node `{node}` has no review decision pending"
        )));
    };
    // The request the decision names, decided on this one read: the stamp
    // is always the seq the check compared, never a second read's.
    match named {
        Some(s) if s == request => {}
        Some(s) => return Err(classify_named_request(run_dir, &events, node, s, request)),
        None if requires_request(gate) && !poster.engine => {
            return Err(EngineError::Conflict(scrubbed_refusal(
                run_dir,
                decide_again_none_named(node, request),
            )));
        }
        // A person-only or bound gate without a form feature keeps main's
        // behaviour: the open request is stamped without being named.
        None => {}
    }
    let queued = read_reviews_after(run_dir, None).map_err(|_| {
        EngineError::Conflict(format!(
            "the review channel of gate `{node}` cannot be read right now; decide again"
        ))
    })?;
    if queued.iter().any(|e| {
        e.cmd.node == node
            && answers_stamped(
                run_dir,
                e,
                request,
                binding,
                gate.person_only,
                is_stamped_gate(gate),
            )
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
/// person-only gate when the gate is one. `passkey` is the consumer's own
/// answer to whether the gate takes a passkey stamp (`is_stamped_gate`): a
/// form gate that is neither person-only nor bound must not demand one.
/// The drive consumes nothing else for such a gate, and nothing else
/// stands in the way of a decision.
pub(crate) fn answers_stamped(
    run_dir: &Path,
    e: &ReviewEntry,
    request: u64,
    binding: Option<&crate::approval::ReviewBinding>,
    person_only: bool,
    passkey: bool,
) -> bool {
    e.request == Some(request)
        && binding.is_none_or(|b| e.nonce.as_deref() == Some(b.nonce.as_str()))
        && (!person_only || decides_person_only_entry(run_dir, e))
        && (!passkey || passkey_stamp_ok(run_dir, e, person_only))
}

/// Whether a channel entry carries the passkey stamp this machine demands:
/// any entry at all when no passkey is enforced, else only an entry whose
/// assertion proof verifies against the store (the engine's own decisions
/// and a scenario run's scripted one are exempt: they are not a person's
/// consent). `person_only` is the consumer's own flag, never read from the
/// run directory (see [`engine_own_entry`]). The drive consumes nothing
/// else, and `apb doctor --run` names the reason.
pub fn passkey_stamp_ok(run_dir: &Path, e: &ReviewEntry, person_only: bool) -> bool {
    let engine_own = engine_own_entry(run_dir, e, person_only);
    let scripted = e.channel == Some(crate::decision_channel::DecisionChannel::Scenario)
        && crate::scenario::session::is_scenario(run_dir);
    // What the challenge covers: the bare decision word without fields
    // (every existing challenge unchanged), the decision bound to its
    // field values with them (people-in-the-loop D3).
    let material = apb_core::review_form::decision_material(&e.cmd.decision, &e.fields);
    passkey_proof::stamp_ok(
        run_dir,
        passkey_proof::Stamp {
            node: &e.cmd.node,
            decision: &material,
            request: e.request,
            nonce: e.nonce.as_deref(),
            proof: e.passkey_proof.as_ref(),
            engine_own,
            scripted,
        },
    )
}

/// Whether the run's own state proves the engine made this decision, so the
/// entry needs no person's assertion. The `by` field of a channel line is
/// forgeable like every other field of the line (it is deserialized with
/// `lenient_decided_by` from a file any same-user process can append to),
/// so the line's own word decides nothing: a `by: unattended` entry counts
/// only when the run's manifest records an unattended run and the gate
/// itself declares the entry's decision as its `unattended.decide` outcome,
/// and a `by: model` entry only when the request the entry stamps carries
/// the enforced recommendation (`auto_decide`, `applied`) that posts
/// exactly this decision: the entry is pinned to the request by
/// `answers_stamped`, so this is the request the engine itself would have
/// auto-decided, not an earlier visit's.
///
/// `person_only` is the consumer's own flag (the drive's definition, the
/// doctor's), never read back from the run directory: a person-only gate
/// takes no engine decision at all (V156 refuses one in the definition; the
/// consumer enforces it here), whatever the run directory's copy of the
/// playbook says. A playbook or journal that does not read is never proof
/// of anything.
pub(crate) fn engine_own_entry(run_dir: &Path, e: &ReviewEntry, person_only: bool) -> bool {
    use crate::event::{DecidedBy, EventPayload};
    if person_only {
        return false;
    }
    let node = e.cmd.node.as_str();
    let playbook = crate::progress::load_run_playbook(run_dir);
    let Some(playbook) = playbook else {
        return false;
    };
    match e.by {
        Some(DecidedBy::Unattended) => {
            let declared = playbook
                .node(node)
                .and_then(|n| n.unattended.as_ref().and_then(|u| u.decide()))
                .is_some_and(|d| d.decide == e.cmd.decision);
            let unattended = matches!(crate::manifest::read(run_dir), Ok(Some(m)) if m.unattended);
            declared && unattended
        }
        Some(DecidedBy::Model) => {
            let Some(request) = e.request else {
                return false;
            };
            let events = match crate::event::read_all(run_dir) {
                Ok(events) => events,
                Err(_) => return false,
            };
            events.iter().rev().any(|ev| {
                ev.seq == request
                    && match &ev.payload {
                        EventPayload::ReviewRequested {
                            node: n,
                            recommendation: Some(r),
                            ..
                        } => n == node && r.applied && r.option == e.cmd.decision,
                        _ => false,
                    }
            })
        }
        _ => false,
    }
}

/// Whether the entry decides a person-only gate: a person's channel, or a
/// dashboard request that carries a verified assertion proof (which is the
/// person), or a scenario run's scripted decision in a scenario run.
pub fn decides_person_only_entry(run_dir: &Path, e: &ReviewEntry) -> bool {
    if channel::decides_person_only(run_dir, e.channel) {
        return true;
    }
    let dashboard = matches!(
        e.channel,
        Some(crate::decision_channel::DecisionChannel::DashboardSession)
            | Some(crate::decision_channel::DecisionChannel::DashboardKeyless)
    );
    dashboard && e.passkey_proof.is_some()
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
            passkey: None,
            proof: None,
        },
        std::collections::BTreeMap::new(),
    )
}

/// A decision with field values, through `channel`, optionally with a
/// verified passkey assertion: the one entry the CLI, MCP and the HTTP
/// route use when a gate takes fields. The other `post_decision_*`
/// functions call it with no fields.
pub struct DecisionPost {
    pub cmd: ReviewCommand,
    pub nonce: Option<String>,
    pub request: Option<u64>,
    pub channel: DecisionChannel,
    /// The values the person gave, by field name; checked against the
    /// gate's `fields` when the decision is posted (people-in-the-loop D3).
    pub fields: std::collections::BTreeMap<String, serde_json::Value>,
    /// The credential id prefix and the assertion, from the dashboard
    /// route only.
    pub passkey: Option<(String, passkey_proof::PasskeyProof)>,
}

/// Posts [`DecisionPost`]: exactly what [`post_decision_via`] and
/// [`post_decision_passkey`] do, by `p.passkey`, with the fields handed to
/// [`append_decision`].
pub fn post(run_dir: &Path, p: DecisionPost) -> Result<u64, EngineError> {
    match &p.passkey {
        Some(_) => {
            if p.channel == DecisionChannel::HttpBearer {
                return Err(EngineError::ChannelRefused {
                    refusal: crate::decision_channel::CallerRefusal::PasskeyRequired,
                    message: "a decision through an API key is never a person's, passkey assertion or not; a key-made client cannot decide a gate that needs a passkey. Sign in to the dashboard in a browser and decide there".to_string(),
                });
            }
            crate::review::channel::refuse_publish_consent(
                run_dir,
                &p.cmd.node,
                &p.cmd.decision,
                p.channel,
            )?;
            crate::review::channel::refuse_own_run(
                run_dir,
                &apb_core::step_env::StepCaller::from_env(),
            )?;
        }
        None => {
            crate::review::channel::refuse_publish_consent(
                run_dir,
                &p.cmd.node,
                &p.cmd.decision,
                p.channel,
            )?;
            if p.channel != DecisionChannel::Scenario {
                crate::review::channel::refuse_own_run(
                    run_dir,
                    &apb_core::step_env::StepCaller::from_env(),
                )?;
            }
        }
    }
    let (passkey, proof) = match p.passkey {
        Some((c, pr)) => (Some(c), Some(pr)),
        None => (None, None),
    };
    append_decision(
        run_dir,
        p.cmd,
        p.nonce,
        p.request,
        None,
        Poster {
            channel: Some(p.channel),
            engine: false,
            passkey,
            proof,
        },
        p.fields,
    )
}

/// [`post_decision_to`] stating the channel the decision came through: the
/// entry point every decision surface uses. A person-only gate refuses a
/// channel that is not a person's ([`EngineError::ChannelRefused`]), a
/// dashboard without keys gives no publish consent
/// ([`channel::refuse_publish_consent`]); a
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
    post(
        run_dir,
        DecisionPost {
            cmd,
            nonce,
            request,
            channel,
            fields: std::collections::BTreeMap::new(),
            passkey: None,
        },
    )
}

/// [`post_decision_via`] carrying a verified passkey assertion: the
/// dashboard route's entry, after the route verified the assertion against
/// the store and bound it to this run, gate, request and option. The
/// credential id prefix is stamped on the entry; the engine consumes such
/// a gate's decision only from an entry that carries it while this
/// machine holds at least one passkey. `passkey_credential` is a short
/// prefix of the credential id, never a secret.
pub fn post_decision_passkey(
    run_dir: &Path,
    cmd: ReviewCommand,
    nonce: Option<String>,
    request: Option<u64>,
    channel: DecisionChannel,
    passkey_credential: String,
    proof: passkey_proof::PasskeyProof,
) -> Result<u64, EngineError> {
    post(
        run_dir,
        DecisionPost {
            cmd,
            nonce,
            request,
            channel,
            fields: std::collections::BTreeMap::new(),
            passkey: Some((passkey_credential, proof)),
        },
    )
}

/// Whether the decision `cmd` makes on the run at `run_dir` needs a fresh
/// passkey assertion on this machine: at least one passkey is enrolled
/// and the gate is `person_only`, is a `publish` consent, or answers a
/// bound request (an approval, `approves` or `requires_approval`). A node
/// the playbook does not have is not a passkey matter (the caller refuses
/// it as unknown); a playbook or journal that does not read fails closed,
/// since the store is what decided the requirement.
/// Whether at least one passkey is enforced on this machine: a passkey is
/// enrolled, or the store exists but does not read (fail closed: the store
/// is what decides whether the gate stands).
pub fn passkey_enforced() -> bool {
    match apb_core::passkey::count_strict() {
        Ok(n) => n > 0,
        Err(_) => true,
    }
}

pub fn passkey_required(run_dir: &Path, cmd: &ReviewCommand) -> bool {
    if !passkey_enforced() {
        return false;
    }
    let Some(playbook) = crate::progress::load_run_playbook(run_dir) else {
        // The playbook does not read: fail closed, the store is why.
        return true;
    };
    let Some(gate) = playbook.node(cmd.node.as_str()) else {
        // The playbook reads and names no such node: the ordinary
        // unknown-node refusal answers, not the passkey gate.
        return false;
    };
    if gate.person_only || matches!(gate.kind, apb_core::schema::NodeKind::Publish { .. }) {
        return true;
    }
    match crate::event::read_all(run_dir) {
        Ok(events) => {
            open_request(&events, cmd.node.as_str()).is_some_and(|(_, binding)| binding.is_some())
        }
        // The journal does not read: fail closed.
        Err(_) => true,
    }
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
            passkey: None,
            proof: None,
        },
        std::collections::BTreeMap::new(),
    )
}

/// Refuses field values the run's secret scrub would change, before
/// anything of the post is written (people-in-the-loop D3): the channel
/// line is the person's post as typed, so a value that holds a secret of
/// the run, or is shaped like a credential, never reaches it; the drive
/// scrubs again when it consumes. While the run's secret values cannot be
/// read, a non-empty text value is refused the same way the run redactor
/// withholds text. The refusal names the field (only a field the gate
/// declares, by [`apb_core::review_form::nameable`]), never the value.
fn refuse_secret_fields(
    run_dir: &Path,
    node: &str,
    fields: &std::collections::BTreeMap<String, serde_json::Value>,
) -> Result<(), EngineError> {
    if fields.is_empty() {
        return Ok(());
    }
    let redactor = crate::run_redact::RunRedactor::for_run_dir(run_dir);
    let declared: Vec<String> = crate::progress::load_run_playbook(run_dir)
        .and_then(|p| match p.node(node).map(|n| &n.kind) {
            Some(apb_core::schema::NodeKind::HumanReview { fields, .. }) => {
                Some(fields.iter().map(|f| f.name.clone()).collect())
            }
            _ => None,
        })
        .unwrap_or_default();
    for (key, value) in fields {
        let label = if declared.contains(key) && apb_core::review_form::nameable(key) {
            format!("field `{key}`")
        } else {
            "a posted value".to_string()
        };
        let mut scrubbed = value.clone();
        match redactor.secret_json(&mut scrubbed) {
            Ok(()) if scrubbed == *value => {}
            Ok(()) => {
                return Err(EngineError::Invalid(crate::review::form::refusal_text(
                    &redactor,
                    &format!(
                        "gate `{node}`: {label} holds a value this run keeps secret; it is not taken"
                    ),
                )));
            }
            // Nothing scrubs while the values do not read, so the refusal
            // carries no text of the post or the run: the label only.
            Err(_) if value.as_str().is_some_and(|s| !s.is_empty()) => {
                return Err(EngineError::Invalid(format!(
                    "{label}: this run's secret values cannot be read right now, so a text value is not taken (it may hold a value this run keeps secret); decide again later"
                )));
            }
            Err(_) => {}
        }
    }
    Ok(())
}

/// Completes or cuts the unterminated tail a crashed poster left, before
/// any check or sequence number: a tail that parses as a whole entry
/// keeps it by getting its newline (the drive may already have consumed
/// it), a partial one is cut (its poster never returned success, so no
/// caller was told its decision was recorded). Only posters write, under
/// the lock this runs under.
fn repair_tail(path: &Path) -> Result<(), EngineError> {
    let Ok(bytes) = std::fs::read(path) else {
        return Ok(());
    };
    if bytes.is_empty() || bytes.ends_with(b"\n") {
        return Ok(());
    }
    let (_, tail) = split_reviews(&bytes);
    let mut file = apb_core::fsutil::open_append_private(path)?;
    if tail.is_some_and(|t| serde_json::from_slice::<ReviewEntry>(t).is_ok()) {
        file.write_all(b"\n")?;
    } else {
        let cut = bytes.iter().rposition(|b| *b == b'\n').map_or(0, |i| i + 1);
        file.set_len(cut as u64)?;
    }
    file.flush()?;
    Ok(())
}

fn append_decision(
    run_dir: &Path,
    cmd: ReviewCommand,
    nonce: Option<String>,
    request: Option<u64>,
    by: Option<crate::event::DecidedBy>,
    poster: Poster,
    fields: std::collections::BTreeMap<String, serde_json::Value>,
) -> Result<u64, EngineError> {
    crate::driver::ensure_run_dir(run_dir)?;
    refuse_secret_fields(run_dir, &cmd.node, &fields)?;
    // The check and the append under one lock: two posters (a person and
    // the engine's automatic decision, issue #165 Part 14.4) that both pass
    // the check would queue two decisions for one visit, and the second
    // would decide the gate's next visit unseen.
    let _lock = apb_core::fsutil::lock_dir(run_dir, "reviews.jsonl.lock")?;
    repair_tail(&run_dir.join("reviews.jsonl"))?;
    let passkey_prefix = poster.passkey.clone();
    let poster_channel = poster.channel;
    let poster_engine = poster.engine;
    let poster_proof = poster.proof.clone();
    // A gate whose decision must name the request it answers has its
    // whole check on one journal read in `check_bound_decision`, the
    // named seq included; the named-request check here stays for every
    // other gate (a plain gate, a publish node, an unreadable
    // definition).
    let stamped_consume = crate::progress::load_run_playbook(run_dir).is_some_and(|p| {
        p.node(&cmd.node)
            .map(|g| {
                // A publish node goes down the publish path, never into
                // `check_bound_decision`, so its named request is checked
                // here even when it is person-only.
                crate::review::consumes_stamped(g)
                    && !matches!(g.kind, apb_core::schema::NodeKind::Publish { .. })
            })
            .unwrap_or(false)
    });
    if let Some(seq) = request.filter(|_| !stamped_consume) {
        check_review_request(run_dir, &cmd.node, seq)?;
    }
    let stamps = check_review_target(run_dir, &cmd, nonce, &fields, request, poster)?;

    // The passkey gate: while this machine holds at least one passkey, a
    // person-only gate and a bound approval take a decision only with a
    // fresh WebAuthn assertion, which only the dashboard route can verify.
    // The engine's own decisions (an unattended run's declared outcome, an
    // enforced recommendation) and a scenario run's scripted one are
    // exempt: they are not a person's consent anyway, and V156 already
    // refuses both on a person-only gate.
    let exempt = poster_engine || poster_channel == Some(DecisionChannel::Scenario);
    let stamped_dashboard = matches!(
        poster_channel,
        Some(DecisionChannel::DashboardSession) | Some(DecisionChannel::DashboardKeyless)
    );
    if passkey_prefix.is_some() && !stamped_dashboard {
        return Err(EngineError::Invalid(
            "a passkey assertion is stamped only on a decision through the dashboard".to_string(),
        ));
    }
    if passkey_prefix.is_some() && poster_proof.is_none() {
        return Err(EngineError::Invalid(
            "a passkey decision carries the full assertion, not only the credential prefix"
                .to_string(),
        ));
    }
    let entry_passkey = if let Some(prefix) = passkey_prefix {
        Some(prefix)
    } else if !exempt && passkey_required(run_dir, &cmd) {
        let run = run_dir
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("")
            .to_string();
        let link = run_dir
            .parent()
            .and_then(|p| p.parent())
            .and_then(|root| crate::people::link::run_link(root, &run))
            .unwrap_or_else(|| "the dashboard's run page".to_string());
        return Err(EngineError::ChannelRefused {
            refusal: crate::decision_channel::CallerRefusal::PasskeyRequired,
            message: format!(
                "gate `{}` of run `{run}` needs a passkey: at least one passkey is enrolled on this machine, so this decision takes a fresh passkey assertion. Decide on the dashboard: {link}",
                cmd.node
            ),
        });
    } else {
        None
    };

    let seq = read_reviews_after(run_dir, None)?.len() as u64;

    let entry = ReviewEntry {
        seq,
        cmd,
        nonce: stamps.nonce,
        by,
        request: stamps.request,
        channel: stamps.channel,
        passkey: entry_passkey,
        passkey_proof: poster_proof,
        fields,
    };
    let line = serde_json::to_string(&entry).map_err(|e| EngineError::Yaml(e.to_string()))?;

    let path = run_dir.join("reviews.jsonl");
    let mut file = apb_core::fsutil::open_append_private(&path)?;
    file.write_all(format!("{line}\n").as_bytes())?;
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
    let consumed = crate::event::review_consumed_count(&events, node);
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
            e.cmd.node == node
                && answers_stamped(
                    run_dir,
                    e,
                    seq,
                    binding,
                    gate.person_only,
                    is_stamped_gate(&gate),
                )
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
        .saturating_sub(consumed);
    if queued >= open {
        return Err(EngineError::AlreadyAnswered(format!(
            "the review of node `{node}` at seq {seq} already has a decision queued"
        )));
    }
    Ok(())
}

/// The complete lines of `bytes` (without their newlines) and the
/// unterminated tail (`None` when the file ends with a newline or is
/// empty). The one split both the reader and the writer repair share.
fn split_reviews(bytes: &[u8]) -> (Vec<&[u8]>, Option<&[u8]>) {
    let mut lines: Vec<&[u8]> = bytes.split(|b| *b == b'\n').collect();
    // The last element of `split` is the unterminated tail: empty when the
    // file ends with a newline, a partial line otherwise.
    let tail = lines.pop().and_then(|t| (!t.is_empty()).then_some(t));
    lines.retain(|l| !l.is_empty());
    (lines, tail)
}

/// Reads the review channel as bytes: every complete line is decoded on
/// its own (a torn line in the middle, and an unparsable line that ends
/// with a newline, are corruption and fail the read as before), and the
/// unterminated tail is a decision still being written: returned when it
/// parses as a whole entry, skipped otherwise, so a decision appended by
/// a poster under its lock while the drive polls is read whole on the
/// next poll instead of failing the run.
pub fn read_reviews_after(
    run_dir: &Path,
    after_seq: Option<u64>,
) -> Result<Vec<ReviewEntry>, EngineError> {
    let path = run_dir.join("reviews.jsonl");
    if !path.is_file() {
        return Ok(Vec::new());
    }
    let bytes = std::fs::read(&path)?;
    let (lines, tail) = split_reviews(&bytes);
    let mut out = Vec::new();
    for line in lines {
        if line.iter().all(|b| b.is_ascii_whitespace()) {
            continue;
        }
        let text = std::str::from_utf8(line).map_err(|e| EngineError::Yaml(e.to_string()))?;
        let entry: ReviewEntry =
            serde_json::from_str(text).map_err(|e| EngineError::Yaml(e.to_string()))?;
        if after_seq.is_none_or(|threshold| entry.seq > threshold) {
            out.push(entry);
        }
    }
    // The unterminated tail: returned when it parses as a whole entry (a
    // writer that wrote the line and not yet the newline), skipped as a
    // line still being written otherwise.
    if let Some(tail) = tail
        && !tail.iter().all(|b| b.is_ascii_whitespace())
        && let Ok(entry) = serde_json::from_slice::<ReviewEntry>(tail)
        && after_seq.is_none_or(|threshold| entry.seq > threshold)
    {
        out.push(entry);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One whole line as `post_review` writes it, for the torn-tail tests.
    fn whole_line() -> String {
        let scratch = tempfile::tempdir().unwrap();
        post_review(
            scratch.path(),
            ReviewCommand {
                node: "gate".into(),
                decision: "approved".into(),
                note: "a".into(),
            },
        )
        .unwrap();
        serde_json::to_string(&read_reviews_after(scratch.path(), None).unwrap()[0]).unwrap()
    }

    /// The line cut in half at a byte (never inside a character).
    fn half_of(line: &str) -> String {
        let b = line.as_bytes();
        String::from_utf8_lossy(&b[..b.len() / 2]).into_owned()
    }

    /// A torn last line (no trailing newline) is a decision not yet
    /// written: the read returns the whole entries and skips it; once the
    /// rest of the line and its newline arrive, it is returned.
    #[test]
    fn a_torn_last_line_is_not_yet_written() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("reviews.jsonl");
        post_review(
            dir.path(),
            ReviewCommand {
                node: "gate".into(),
                decision: "approved".into(),
                note: "a".into(),
            },
        )
        .unwrap();
        post_review(
            dir.path(),
            ReviewCommand {
                node: "gate".into(),
                decision: "approved".into(),
                note: "b".into(),
            },
        )
        .unwrap();
        let line = whole_line();
        use std::io::Write;
        std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap()
            .write_all(half_of(&line).as_bytes())
            .unwrap();
        assert_eq!(
            read_reviews_after(dir.path(), None).unwrap().len(),
            2,
            "a torn last line must not fail the read"
        );
        // The rest of the line and the newline land; now it is read.
        std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap()
            .write_all(format!("{}\n", &line[half_of(&line).len()..]).as_bytes())
            .unwrap();
        assert_eq!(read_reviews_after(dir.path(), None).unwrap().len(), 3);
    }

    /// A torn line in the middle is corruption, not a write in progress.
    #[test]
    fn a_torn_middle_line_still_fails() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("reviews.jsonl");
        post_review(
            dir.path(),
            ReviewCommand {
                node: "gate".into(),
                decision: "approved".into(),
                note: "a".into(),
            },
        )
        .unwrap();
        use std::io::Write;
        let mut f = std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap();
        f.write_all(half_of(&whole_line()).as_bytes()).unwrap();
        f.write_all(b"\n").unwrap();
        drop(f);
        assert!(
            read_reviews_after(dir.path(), None).is_err(),
            "a torn middle line must fail the read"
        );
    }

    /// An unparsable last line that ends with a newline is corruption.
    #[test]
    fn an_unparsable_terminated_last_line_still_fails() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("reviews.jsonl");
        post_review(
            dir.path(),
            ReviewCommand {
                node: "gate".into(),
                decision: "approved".into(),
                note: "a".into(),
            },
        )
        .unwrap();
        use std::io::Write;
        std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap()
            .write_all(b"garbage\n")
            .unwrap();
        assert!(read_reviews_after(dir.path(), None).is_err());
    }

    /// A whole last line whose writer has not written the newline yet is
    /// returned: the drive may consume it.
    #[test]
    fn a_whole_last_line_without_newline_is_read() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("reviews.jsonl");
        post_review(
            dir.path(),
            ReviewCommand {
                node: "gate".into(),
                decision: "approved".into(),
                note: "a".into(),
            },
        )
        .unwrap();
        use std::io::Write;
        std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap()
            .write_all(whole_line().as_bytes())
            .unwrap();
        assert_eq!(read_reviews_after(dir.path(), None).unwrap().len(), 2);
    }

    /// A poster cuts a partial tail a crashed poster left, and its own
    /// decision becomes the only line after the whole one.
    #[test]
    fn a_post_cuts_a_crashed_posters_tail() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("reviews.jsonl");
        post_review(
            dir.path(),
            ReviewCommand {
                node: "gate".into(),
                decision: "approved".into(),
                note: "a".into(),
            },
        )
        .unwrap();
        use std::io::Write;
        std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap()
            .write_all(half_of(&whole_line()).as_bytes())
            .unwrap();
        post_review(
            dir.path(),
            ReviewCommand {
                node: "gate".into(),
                decision: "rejected".into(),
                note: "n".into(),
            },
        )
        .unwrap();
        let raw = std::fs::read(&path).unwrap();
        let lines: Vec<&[u8]> = raw
            .split(|b| *b == b'\n')
            .filter(|l| !l.is_empty())
            .collect();
        assert_eq!(lines.len(), 2, "the partial tail must be cut: {lines:?}");
        for l in &lines {
            serde_json::from_slice::<ReviewEntry>(l)
                .unwrap_or_else(|e| panic!("a line must parse: {e}"));
        }
        let second = serde_json::from_slice::<ReviewEntry>(lines[1]).unwrap();
        assert_eq!(second.seq, 1);
        assert_eq!(second.cmd.note, "n");
    }

    /// A tail cut inside a multi-byte character is a line still being
    /// written, not corruption (a `read_to_string` reader fails here).
    #[test]
    fn a_tail_split_inside_a_character_is_not_yet_written() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("reviews.jsonl");
        post_review(
            dir.path(),
            ReviewCommand {
                node: "gate".into(),
                decision: "approved".into(),
                note: "a".into(),
            },
        )
        .unwrap();
        let line = whole_line().replace(r#""note":"a""#, r#""note":"é""#);
        let bytes = line.as_bytes();
        // The second entry, cut between the two bytes of the e-acute.
        let cut = line.find('é').expect("the fixture holds the e-acute") + 1;
        assert!(
            std::str::from_utf8(&bytes[..cut]).is_err(),
            "the tail must end inside the character"
        );
        use std::io::Write;
        std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap()
            .write_all(&bytes[..cut])
            .unwrap();
        assert_eq!(read_reviews_after(dir.path(), None).unwrap().len(), 1);
    }

    /// A whole entry that lost only its newline survives a later post:
    /// the repair appends the newline instead of cutting the entry.
    #[test]
    fn a_post_keeps_a_whole_entry_missing_its_newline() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("reviews.jsonl");
        post_review(
            dir.path(),
            ReviewCommand {
                node: "gate".into(),
                decision: "approved".into(),
                note: "a".into(),
            },
        )
        .unwrap();
        // Rewrite the file as the whole entry without its newline.
        let raw0 = std::fs::read(&path).unwrap();
        let line = raw0
            .split(|b| *b == b'\n')
            .find(|l| !l.is_empty())
            .unwrap()
            .to_vec();
        std::fs::write(&path, &line).unwrap();
        post_review(
            dir.path(),
            ReviewCommand {
                node: "gate".into(),
                decision: "rejected".into(),
                note: "n".into(),
            },
        )
        .unwrap();
        let raw = std::fs::read(&path).unwrap();
        let lines: Vec<&[u8]> = raw
            .split(|b| *b == b'\n')
            .filter(|l| !l.is_empty())
            .collect();
        assert_eq!(lines.len(), 2);
        let first = serde_json::from_slice::<ReviewEntry>(lines[0]).unwrap();
        assert_eq!(first.seq, 0, "the whole entry missing its newline stays");
        assert_eq!(first.cmd.note, "a");
        let second = serde_json::from_slice::<ReviewEntry>(lines[1]).unwrap();
        assert_eq!(second.seq, 1);
    }

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

    /// The `by: model` exemption is bound to the request the entry stamps
    /// (the regression H1): it holds only when the request the entry
    /// answers is the visit whose enforced recommendation posts exactly
    /// this decision, never an earlier visit of the same gate in a loop.
    /// The consumer's `person_only` flag rules the exemption out outright,
    /// whatever the run directory's copy of the playbook says.
    #[test]
    fn a_model_line_is_engine_own_only_for_the_request_that_was_enforced() {
        use crate::event::{EventLog, EventPayload, ReviewRecommendation};
        let dir = tempfile::tempdir().unwrap();
        let rd = dir.path().join("r1");
        std::fs::create_dir_all(&rd).unwrap();
        // A bound gate with `auto_decide`, not person-only: V156 lets the
        // engine decide it when the recommendation is enforced.
        std::fs::write(
            rd.join("playbook.yaml"),
            "schema: 2\nid: ap\nname: ap\nversion: 1.0.0\nnodes:\n  - { id: start, type: start }\n  - { id: g, type: human_review, options: [needs_changes, reject], auto_decide: { allow: [needs_changes], min_confidence: 0.5 } }\n  - { id: done, type: finish, outcome: success }\nedges:\n  - { from: start, to: g }\n  - { from: g, to: done }\n",
        )
        .unwrap();
        let mut log = EventLog::create(&rd).unwrap();
        // Visit 1: the recommendation `approve` was enforced (applied).
        let visit1 = log
            .append(EventPayload::ReviewRequested {
                node: "g".into(),
                options: vec!["approve".into(), "reject".into()],
                title: None,
                instruction: String::new(),
                prompt: None,
                recommendation: Some(ReviewRecommendation {
                    option: "approve".into(),
                    applied: true,
                    ..Default::default()
                }),
                plan_digest: None,
                binding: None,
                person_only: false,
            })
            .unwrap()
            .seq;
        // Visit 2 (a gate in a loop): the recommendation is not applied
        // this time (say, low confidence), so the person decides.
        let visit2 = log
            .append(EventPayload::ReviewRequested {
                node: "g".into(),
                options: vec!["approve".into(), "reject".into()],
                title: None,
                instruction: String::new(),
                prompt: None,
                recommendation: Some(ReviewRecommendation {
                    option: "approve".into(),
                    applied: false,
                    ..Default::default()
                }),
                plan_digest: None,
                binding: None,
                person_only: false,
            })
            .unwrap()
            .seq;
        let entry = |request: u64| ReviewEntry {
            seq: 1,
            cmd: ReviewCommand {
                node: "g".into(),
                decision: "approve".into(),
                note: String::new(),
            },
            nonce: None,
            by: Some(crate::event::DecidedBy::Model),
            request: Some(request),
            channel: Some(crate::decision_channel::DecisionChannel::Engine),
            passkey: None,
            passkey_proof: None,
            fields: std::collections::BTreeMap::new(),
        };
        assert!(
            engine_own_entry(&rd, &entry(visit1), false),
            "the enforced visit's own request proves the engine decided"
        );
        assert!(
            !engine_own_entry(&rd, &entry(visit2), false),
            "a later, unenforced visit must not inherit visit 1's enforcement"
        );
        assert!(
            !engine_own_entry(&rd, &entry(visit1), true),
            "the consumer's person-only flag rules the exemption out"
        );
        // An entry stamped with no request at all is never engine-own.
        let unstamped = ReviewEntry {
            request: None,
            ..entry(visit1)
        };
        assert!(!engine_own_entry(&rd, &unstamped, false));
    }

    /// A post naming a request the gate withdrew while no request is open
    /// yet is told the request was withdrawn (`decide_again`), not that
    /// nothing is pending.
    #[test]
    fn a_post_naming_a_withdrawn_request_with_none_open_says_withdrawn() {
        use crate::decision_channel::DecisionChannel;
        use crate::event::{EventLog, EventPayload};
        let dir = tempfile::tempdir().unwrap();
        let rd = dir.path().join("r1");
        std::fs::create_dir_all(&rd).unwrap();
        std::fs::write(
            rd.join("playbook.yaml"),
            "schema: 2\nid: ap\nname: ap\nversion: 1.0.0\nnodes:\n  - { id: start, type: start }\n  - { id: g, type: human_review, options: [approve, reject], show: [{ label: L, text: t }] }\n  - { id: done, type: finish, outcome: success }\nedges:\n  - { from: start, to: g }\n  - { from: g, to: done }\n",
        )
        .unwrap();
        let mut log = EventLog::create(&rd).unwrap();
        let seq_a = log
            .append(EventPayload::ReviewRequested {
                node: "g".into(),
                options: vec!["approve".into(), "reject".into()],
                title: None,
                instruction: String::new(),
                prompt: None,
                recommendation: None,
                plan_digest: None,
                binding: None,
                person_only: false,
            })
            .unwrap()
            .seq;
        log.append(EventPayload::ReviewDetail {
            node: "g".into(),
            request: seq_a,
            scope: None,
            show: Vec::new(),
            fields: Vec::new(),
            withheld: Vec::new(),
            withheld_reason: None,
        })
        .unwrap();
        log.append(EventPayload::ReviewWithdrawn {
            node: "g".into(),
            reason: "scope_changed".into(),
        })
        .unwrap();
        drop(log);
        let err = post_decision_via(
            &rd,
            ReviewCommand {
                node: "g".into(),
                decision: "approve".into(),
                note: String::new(),
            },
            None,
            Some(seq_a),
            DecisionChannel::Terminal,
        )
        .unwrap_err();
        let text = err.to_string();
        assert!(text.contains(DECIDE_AGAIN), "{text}");
        assert!(text.contains("was withdrawn"), "{text}");
    }

    /// The request a decision names is the one it is stamped with, decided
    /// on one journal read: a request asked again between the check's read
    /// and the stamp never receives a decision made for the one before
    /// (carried-over decision 2, review round 2 blocker 1). The hook
    /// withdraws the open request A and opens B right after the read; a
    /// post naming A is still stamped A, which never answers B.
    #[test]
    fn a_request_reasked_between_the_check_and_the_stamp_keeps_the_named_stamp() {
        use crate::decision_channel::DecisionChannel;
        use crate::event::{EventLog, EventPayload};
        let dir = tempfile::tempdir().unwrap();
        let rd = dir.path().join("r1");
        std::fs::create_dir_all(&rd).unwrap();
        std::fs::write(
            rd.join("playbook.yaml"),
            "schema: 2\nid: ap\nname: ap\nversion: 1.0.0\nnodes:\n  - { id: start, type: start }\n  - { id: g, type: human_review, options: [approve, reject], show: [{ label: L, text: t }] }\n  - { id: done, type: finish, outcome: success }\nedges:\n  - { from: start, to: g }\n  - { from: g, to: done }\n",
        )
        .unwrap();
        let mut log = EventLog::create(&rd).unwrap();
        let seq_a = log
            .append(EventPayload::ReviewRequested {
                node: "g".into(),
                options: vec!["approve".into(), "reject".into()],
                title: None,
                instruction: String::new(),
                prompt: None,
                recommendation: None,
                plan_digest: None,
                binding: None,
                person_only: false,
            })
            .unwrap()
            .seq;
        // The form detail a `show` gate's request journals beside itself.
        log.append(EventPayload::ReviewDetail {
            node: "g".into(),
            request: seq_a,
            scope: None,
            show: Vec::new(),
            fields: Vec::new(),
            withheld: Vec::new(),
            withheld_reason: None,
        })
        .unwrap();
        drop(log);
        // The hook, one shot: withdraw A and open B the moment the check
        // has read the journal.
        {
            let rd = rd.clone();
            AFTER_BOUND_READ.with(|h| {
                *h.borrow_mut() = Some(Box::new(move || {
                    let mut log = EventLog::open(&rd).unwrap();
                    log.append(EventPayload::ReviewWithdrawn {
                        node: "g".into(),
                        reason: "supervisor".into(),
                    })
                    .unwrap();
                    let seq_b = log
                        .append(EventPayload::ReviewRequested {
                            node: "g".into(),
                            options: vec!["approve".into(), "reject".into()],
                            title: None,
                            instruction: String::new(),
                            prompt: None,
                            recommendation: None,
                            plan_digest: None,
                            binding: None,
                            person_only: false,
                        })
                        .unwrap()
                        .seq;
                    log.append(EventPayload::ReviewDetail {
                        node: "g".into(),
                        request: seq_b,
                        scope: None,
                        show: Vec::new(),
                        fields: Vec::new(),
                        withheld: Vec::new(),
                        withheld_reason: None,
                    })
                    .unwrap();
                }));
            });
        }
        let cmd = ReviewCommand {
            node: "g".into(),
            decision: "approve".into(),
            note: String::new(),
        };
        post_decision_via(
            &rd,
            cmd.clone(),
            None,
            Some(seq_a),
            DecisionChannel::Terminal,
        )
        .expect("the post naming the request it read is accepted");
        let events = crate::event::read_all(&rd).unwrap();
        let seq_b = events
            .iter()
            .rev()
            .find_map(|e| match &e.payload {
                EventPayload::ReviewRequested { node, .. } if node == "g" => Some(e.seq),
                _ => None,
            })
            .unwrap();
        assert_ne!(seq_a, seq_b, "the hook opened a new request");
        let entries = read_reviews_after(&rd, None).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(
            entries[0].request,
            Some(seq_a),
            "the entry keeps the stamp of the request it named, never the newer one"
        );
        // The drive would not consume that entry for B.
        let binding = open_request(&events, "g").and_then(|(_, b)| b.cloned());
        assert!(!answers_stamped(
            &rd,
            &entries[0],
            seq_b,
            binding.as_ref(),
            false,
            false,
        ));
        // A post naming B is accepted and stamped B.
        post_decision_via(&rd, cmd, None, Some(seq_b), DecisionChannel::Terminal)
            .expect("the post naming the open request is accepted");
        let entries = read_reviews_after(&rd, None).unwrap();
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[1].request, Some(seq_b));
    }
}
