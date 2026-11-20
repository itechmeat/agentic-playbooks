//! Labellers: what a run's later events say about a journaled decision
//! (issue #165 Part 13). One labeller per use; each reads only facts the
//! journal already records, never a model.
//!
//! A decision is framed as "act or not": the completion check acts by
//! flagging a reply, retry advice by stopping or switching, triage by
//! retrying on its own, the review triage by deciding, routing by moving an
//! attempt to another tier. A label says whether acting would have been
//! right ([`Label::Act`]) or wrong ([`Label::Keep`]); a decision whose run
//! has not shown it yet stays [`Label::Unlabelled`] and is left out of every
//! accuracy figure (it still counts in the coverage line).
//!
//! The completion check, the review triage and retry advice are labelled.
//! The other uses are stubs ([`PendingLabeller`]) until their events journal
//! the join keys named in [`JoinKey`]; each stub says which later event will
//! label it.

use std::collections::BTreeMap;
use std::path::Path;

use crate::event::{DecisionAnswer, DecisionBaseline, Event, EventPayload};

/// One journaled `decision_made`, with the run it belongs to.
#[derive(Debug, Clone, PartialEq)]
pub struct DecisionRecord {
    pub run_id: String,
    pub playbook: String,
    pub seq: u64,
    pub ts: u128,
    pub use_site: String,
    pub node: Option<String>,
    /// The list item the decision judged (`decision_made.item`), for an
    /// item of a list node.
    pub item: Option<String>,
    pub attempt: Option<u32>,
    pub provider: Option<String>,
    pub model: Option<String>,
    /// The provider's kind from the run manifest (`systemone`, `fake`, an
    /// emulation), when the manifest names it.
    pub provider_kind: Option<String>,
    /// The privacy class of a cloud catalog model that answered
    /// (`decision_made.privacy_class`).
    pub privacy_class: Option<String>,
    pub calibrated: bool,
    pub mode: String,
    pub answers: BTreeMap<String, DecisionAnswer>,
    pub applied: bool,
    pub would_change: Option<bool>,
    pub baseline: Option<DecisionBaseline>,
    pub latency_ms: u64,
    pub cost_usd: Option<f64>,
    pub cached: bool,
    pub error: Option<String>,
    pub output_chars: Option<u64>,
}

impl DecisionRecord {
    /// The record of a `decision_made` event, `None` for any other event.
    pub fn from_event(run_id: &str, playbook: &str, e: &Event) -> Option<Self> {
        let EventPayload::DecisionMade {
            use_site,
            node,
            item,
            attempt,
            provider,
            model,
            calibrated,
            mode,
            output_chars,
            answers,
            applied,
            would_change,
            baseline,
            latency_ms,
            cost_usd,
            cached,
            error,
            privacy_class,
            ..
        } = &e.payload
        else {
            return None;
        };
        Some(DecisionRecord {
            run_id: run_id.to_string(),
            playbook: playbook.to_string(),
            seq: e.seq,
            ts: e.ts,
            use_site: use_site.clone(),
            node: node.clone(),
            item: item.clone(),
            attempt: *attempt,
            provider: provider.clone(),
            model: model.clone(),
            provider_kind: None,
            privacy_class: privacy_class.clone(),
            calibrated: *calibrated,
            mode: mode.clone(),
            answers: answers.clone(),
            applied: *applied,
            would_change: *would_change,
            baseline: baseline.clone(),
            latency_ms: *latency_ms,
            cost_usd: *cost_usd,
            cached: *cached,
            error: error.clone(),
            output_chars: *output_chars,
        })
    }

    /// Answered: a provider replied (a cache hit included).
    pub fn answered(&self) -> bool {
        self.error.is_none() && self.provider.is_some()
    }

    /// The `p` of one answer.
    pub fn p(&self, question: &str) -> Option<f64> {
        self.answers.get(question).and_then(|a| a.p)
    }
}

/// What ties a decision to the later events that label it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum JoinKey {
    /// One attempt of one node: the completion check, retry advice, routing.
    Attempt { node: String, attempt: u32 },
    /// The n-th visit (1-based) of a `human_review` gate: the review triage.
    /// Needs a gate-visit field on the event (not journaled yet).
    GateVisit { node: String, visit: u32 },
    /// The `wake_raised` a supervisor triage answered, by its seq. Needs a
    /// wake-seq field on the event (not journaled yet).
    Wake { seq: u64 },
}

impl JoinKey {
    /// The key the record carries today: node and attempt, when present.
    pub fn of(r: &DecisionRecord) -> Option<Self> {
        Some(JoinKey::Attempt {
            node: r.node.clone()?,
            attempt: r.attempt?,
        })
    }
}

/// What a run's later events say about one decision.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Label {
    /// Acting would have been right (the completion check: the reply was
    /// not a finished result).
    Act,
    /// Acting would have been wrong: today's behaviour was right.
    Keep,
    /// Not known (yet), with the reason.
    Unlabelled(&'static str),
}

/// The labeller of one use.
pub trait Labeller: Sync {
    fn use_site(&self) -> &'static str;
    /// Where the labels come from, printed with the report.
    fn label_source(&self) -> &'static str;
    /// Today's behaviour without the use, as a label: the completion check
    /// never flags (`Keep`, "always complete").
    fn today(&self) -> (Label, &'static str);
    /// The config default of the use's threshold (the scale of
    /// `apb decisions thresholds set --threshold`).
    fn default_threshold(&self) -> f64;
    /// The probability that acting is right, for Brier and ECE.
    fn act_probability(&self, r: &DecisionRecord) -> Option<f64>;
    /// Whether the use acts on this answer at `threshold`.
    fn acts_at(&self, r: &DecisionRecord, threshold: f64) -> Option<bool>;
    /// For a `choice` use, the option that labels the decision (the
    /// eligibility rule wants 20 per option). `None` for a yes/no use.
    fn label_option(&self, _r: &DecisionRecord, _label: &Label) -> Option<String> {
        None
    }
    /// The label from the events of the decision's run.
    fn label(&self, r: &DecisionRecord, events: &[Event]) -> Label;
    /// The label and the labelling rule it was computed under, for a use
    /// whose labels have rule versions
    /// ([`apb_core::decision_thresholds::versioned_use`]); `None` for every
    /// other use. The rule is chosen per decision from its journal alone,
    /// so a report computed twice agrees.
    fn label_with_rule(
        &self,
        r: &DecisionRecord,
        events: &[Event],
    ) -> (Label, Option<&'static str>) {
        (self.label(r, events), None)
    }
    /// [`Labeller::label_with_rule`] over a run read from disk
    /// (`RunJournal::run_dir`): a labeller that needs more than the events (a
    /// stored definition) reads it from `run_dir`. The report and the replay
    /// label through it.
    fn label_in(
        &self,
        r: &DecisionRecord,
        events: &[Event],
        _run_dir: Option<&Path>,
    ) -> (Label, Option<&'static str>) {
        self.label_with_rule(r, events)
    }
    /// A stub that labels nothing yet.
    fn pending(&self) -> bool {
        false
    }
    /// Whether the decision's answer was shown to the person whose later
    /// action labels it (an advisory review recommendation): such a label
    /// may follow the recommendation rather than judge it, so the report
    /// counts it apart from the unbiased (shadow) ones.
    fn shown_to_labeller(&self, _r: &DecisionRecord) -> bool {
        false
    }
}

/// The labeller of a use name. Every known use has one.
pub fn labeller_for(use_site: &str) -> Box<dyn Labeller> {
    match use_site {
        "completion_check" => Box::new(CompletionLabeller),
        // --- 0.23.0 labellers (C9) ---
        "review_triage" => Box::new(ReviewTriageLabeller),
        "retry_advice" => Box::new(RetryAdviceLabeller),
        // --- end of the 0.23.0 labellers ---
        other => Box::new(PendingLabeller::for_use(other)),
    }
}

// --- the completion check ---------------------------------------------------

/// Labels a completion check under rule `reply/1`
/// ([`apb_core::decision_thresholds::COMPLETION_RULE`]), from what the
/// journal proves about the reply itself:
///
/// - `Keep` (flagging the reply would have been wrong) when the node's next
///   `node_finished` succeeded, the first node to start or ask after it is a
///   review gate whose request is bound to this execution's snapshot, the
///   gate's stored definition `approves` the node, and a person decided
///   that request (or a scope re-ask of it that keeps the binding) with one
///   of the gate's approving options;
/// - nothing is labelled `Act`: a reply that asks a question back stops
///   before the check runs, and a later failure, a supervisor retry or a
///   move back say something went wrong later, not that the reply was
///   unfinished;
/// - unlabelled otherwise, when another attempt of the same visit followed,
///   and above shadow when the check's own anomaly wake came first.
///
/// The approving options come from the definition the run executed, which
/// only a run directory holds: [`Labeller::label`] (events only) never
/// labels `Keep`.
pub struct CompletionLabeller;

impl CompletionLabeller {
    fn label_at(&self, r: &DecisionRecord, events: &[Event], run_dir: Option<&Path>) -> Label {
        let Some(node) = r.node.as_deref() else {
            return Label::Unlabelled("no node on the decision");
        };
        // An item's reply is settled by the item, not by the list's
        // aggregate `node_finished`, and the list's gate approves the list:
        // neither says a person approved this item's reply.
        if r.item.is_some() {
            return Label::Unlabelled("the decision judged a list item's reply");
        }
        let Some(at) = events.iter().position(|e| e.seq == r.seq) else {
            return Label::Unlabelled("decision not in its journal");
        };
        // The events up to the node's next start: a later start is a new
        // decision's business.
        let window: Vec<&Event> = events[at + 1..]
            .iter()
            .take_while(
                |e| !matches!(&e.payload, EventPayload::NodeStarted { node: n, .. } if n == node),
            )
            .collect();
        // Another attempt of the same visit (the checked one was rejected
        // or failed after it): what follows is that attempt's outcome, not
        // this one's, and an enforce rejection would label itself.
        if window.iter().any(|e| {
            matches!(&e.payload, EventPayload::AttemptStarted { node: n, attempt, .. }
                if n == node && r.attempt.is_some_and(|a| *attempt > a))
        }) {
            return Label::Unlabelled("a later attempt of the node followed");
        }
        // Above shadow the check raised its own anomaly wake: what answers
        // it would confirm the decision by its own doing.
        if r.mode != "shadow"
            && window.iter().any(|e| {
                matches!(&e.payload, EventPayload::WakeRaised { trigger: crate::event::WakeTrigger::Anomaly, node: n, .. } if n == node)
            })
        {
            return Label::Unlabelled("the decision's own anomaly wake preceded the outcome");
        }
        const NOT_APPROVED: &str = "no person approved the reply at the gate that follows it";
        const NOT_THE_REPLY: &str = "the gate does not approve the reply";
        let Some(fin) = window.iter().position(
            |e| matches!(&e.payload, EventPayload::NodeFinished { node: n, .. } if n == node),
        ) else {
            return Label::Unlabelled(if window.len() < events.len() - at - 1 {
                "the node ran again before any outcome"
            } else {
                "no downstream outcome yet"
            });
        };
        let settlement = window[fin].seq;
        if !matches!(&window[fin].payload, EventPayload::NodeFinished { status, .. } if status == "succeeded")
        {
            return Label::Unlabelled(NOT_APPROVED);
        }
        // The first node to start or ask after the reply settled.
        let next = window[fin + 1..].iter().find(|e| match &e.payload {
            EventPayload::NodeStarted { node: n, .. }
            | EventPayload::ReviewRequested { node: n, .. } => n != node,
            _ => false,
        });
        let Some(request) = next else {
            return Label::Unlabelled(NOT_APPROVED);
        };
        let EventPayload::ReviewRequested {
            node: gate,
            plan_digest: None,
            binding,
            ..
        } = &request.payload
        else {
            return Label::Unlabelled(NOT_APPROVED);
        };
        if !binds(binding.as_ref(), node, settlement) {
            return Label::Unlabelled(NOT_THE_REPLY);
        }
        let (decision, _, final_request) =
            match chain_decision(events, gate, request.seq, node, settlement) {
                Ok(found) => found,
                Err(why) => return Label::Unlabelled(why),
            };
        let Some(definition) = run_dir.and_then(|d| definition_at(d, events, final_request)) else {
            return Label::Unlabelled("the gate's definition is not stored");
        };
        let Some(approving) = approving_options_of(&definition, gate, node) else {
            return Label::Unlabelled(NOT_THE_REPLY);
        };
        if approving.iter().any(|o| o == decision) {
            Label::Keep
        } else {
            Label::Unlabelled(NOT_APPROVED)
        }
    }
}

/// Whether `binding` holds a snapshot of `node` settled at `settlement`.
fn binds(binding: Option<&crate::approval::ReviewBinding>, node: &str, settlement: u64) -> bool {
    binding.is_some_and(|b| {
        b.snapshots
            .iter()
            .any(|s| s.node == node && s.settlement_seq == settlement)
    })
}

/// The definition in force at `seq`: the last boundary at or before it,
/// when it journaled a digest whose bytes are stored and parse.
fn definition_at(run_dir: &Path, events: &[Event], seq: u64) -> Option<apb_core::schema::Playbook> {
    let boundary = crate::run_definitions::boundaries(events)
        .into_iter()
        .rev()
        .find(|b| b.seq <= seq)?;
    let bytes = crate::run_definitions::read_stored(run_dir, boundary.definition.as_deref()?)?;
    apb_core::schema::Playbook::from_yaml(std::str::from_utf8(&bytes).ok()?).ok()
}

/// The approving options of `gate` in `definition` when it is a
/// `human_review` whose `approves.results` names `node`.
fn approving_options_of(
    definition: &apb_core::schema::Playbook,
    gate: &str,
    node: &str,
) -> Option<Vec<String>> {
    let gate = definition.nodes.iter().find(|n| n.id == gate)?;
    let apb_core::schema::NodeKind::HumanReview { options, .. } = &gate.kind else {
        return None;
    };
    let approves = gate.approves.as_ref()?;
    approves
        .results
        .iter()
        .any(|r| r == node)
        .then(|| approves.approving_options(options))
}

/// The person's decision on the request at `first` of gate `gate` and on
/// every re-ask that keeps its binding of `node`'s snapshot settled at
/// `settlement` with no `patch_applied` since `first`: `Ok((decision,
/// note, final_request_seq))` of the last request of the chain, or why
/// there is none. A person's decision is one with no `decided_by` whose
/// recorded channel (`via`) is a person's (`terminal` or
/// `dashboard_session`); a decision without a recorded channel, or through
/// MCP, a run step or any other surface, is no person's label.
fn chain_decision<'a>(
    events: &'a [Event],
    gate: &str,
    first: u64,
    node: &str,
    settlement: u64,
) -> Result<(&'a str, &'a str, u64), &'static str> {
    const UNDECIDED: &str = "the gate visit is not decided yet";
    let Some(start) = events.iter().position(|e| e.seq == first) else {
        return Err(UNDECIDED);
    };
    let mut open = first;
    let mut i = start + 1;
    while i < events.len() {
        match &events[i].payload {
            EventPayload::ReviewRequested { node: n, .. } if n == gate => {
                // The next visit began before this one was decided.
                return Err("the gate visit was never decided");
            }
            // No decision across a patch is read as an approval, re-asked
            // or not.
            EventPayload::PatchApplied { .. } => {
                return Err("the gate's definition changed while it waited");
            }
            EventPayload::ReviewWithdrawn { node: n, reason } if n == gate => {
                let reask = crate::event::is_scope_changed(reason)
                    .then(|| reask_after(events, i, gate))
                    .flatten()
                    .filter(|&j| {
                        matches!(&events[j].payload, EventPayload::ReviewRequested { binding, .. }
                            if binds(binding.as_ref(), node, settlement))
                    });
                let Some(j) = reask else {
                    return Err("the gate visit was withdrawn");
                };
                if events[start..j]
                    .iter()
                    .any(|e| matches!(e.payload, EventPayload::PatchApplied { .. }))
                {
                    return Err("the gate's definition changed while it waited");
                }
                open = events[j].seq;
                i = j;
            }
            EventPayload::ReviewDecided {
                node: n,
                decided_by: Some(by),
                ..
            } if n == gate => return Err(not_a_person(by)),
            EventPayload::ReviewDecided {
                node: n,
                decision,
                note,
                via,
                ..
            } if n == gate => {
                return match via {
                    Some(c) if c.is_person_channel() => {
                        Ok((decision.as_str(), note.as_str(), open))
                    }
                    Some(_) => Err("the gate visit was decided through no person's channel"),
                    None => Err("the gate visit's decision records no channel"),
                };
            }
            _ => {}
        }
        i += 1;
    }
    Err(UNDECIDED)
}

/// The index of the re-ask that follows the withdrawal at `at`: the next
/// event of `gate` when it is a `review_requested` and no other node
/// started before it.
fn reask_after(events: &[Event], at: usize, gate: &str) -> Option<usize> {
    for (j, e) in events.iter().enumerate().skip(at + 1) {
        match &e.payload {
            EventPayload::ReviewRequested { node: n, .. } if n == gate => return Some(j),
            EventPayload::NodeStarted { .. } => return None,
            EventPayload::ReviewDecided { node: n, .. }
            | EventPayload::ReviewWithdrawn { node: n, .. }
            | EventPayload::NodeFinished { node: n, .. }
                if n == gate =>
            {
                return None;
            }
            _ => {}
        }
    }
    None
}

/// Why a decision with `decided_by` set is no person's label.
fn not_a_person(by: &crate::event::DecidedBy) -> &'static str {
    match by {
        crate::event::DecidedBy::Unattended => "the gate visit was decided unattended",
        crate::event::DecidedBy::Model => "the model decided the gate itself",
        crate::event::DecidedBy::Unknown => "the gate visit was decided by no person",
    }
}

impl Labeller for CompletionLabeller {
    fn use_site(&self) -> &'static str {
        "completion_check"
    }

    fn label_source(&self) -> &'static str {
        "the reply itself: a person approved it at the review gate that follows it and approves its result (keep); nothing in the journal says a reply was unfinished, so none is labelled act"
    }

    fn today(&self) -> (Label, &'static str) {
        (Label::Keep, "always complete")
    }

    fn default_threshold(&self) -> f64 {
        apb_core::decisions::COMPLETION_FINAL_RESULT_CUT
    }

    fn act_probability(&self, r: &DecisionRecord) -> Option<f64> {
        r.p("final_result").map(|p| 1.0 - p)
    }

    fn acts_at(&self, r: &DecisionRecord, threshold: f64) -> Option<bool> {
        r.p("final_result").map(|p| p < threshold)
    }

    fn label(&self, r: &DecisionRecord, events: &[Event]) -> Label {
        self.label_with_rule(r, events).0
    }

    /// The rule is chosen by the code: the old downstream rule is wrong for
    /// every journal.
    fn label_with_rule(
        &self,
        r: &DecisionRecord,
        events: &[Event],
    ) -> (Label, Option<&'static str>) {
        (
            self.label_at(r, events, None),
            Some(apb_core::decision_thresholds::COMPLETION_RULE),
        )
    }

    fn label_in(
        &self,
        r: &DecisionRecord,
        events: &[Event],
        run_dir: Option<&Path>,
    ) -> (Label, Option<&'static str>) {
        (
            self.label_at(r, events, run_dir),
            Some(apb_core::decision_thresholds::COMPLETION_RULE),
        )
    }
}

// --- the review triage (0.23.0, C9) ------------------------------------------

/// Labels a review recommendation by the person's decision at the same gate
/// visit (the decision's `attempt` is the visit, the `gate_visit` join key):
/// `Act` (deciding on the model's answer would have been right) when the
/// person chose the recommended option, `Keep` when they chose another.
/// Unlabelled while the visit is undecided or was withdrawn, and when the
/// model decided the gate itself (`auto:` note). A recommendation shown to
/// the reviewer (advise or enforce) is labelled too, but counted apart from
/// shadow ones: the person saw it.
pub struct ReviewTriageLabeller;

fn answer_value(r: &DecisionRecord, question: &str) -> Option<String> {
    r.answers
        .get(question)?
        .value
        .as_ref()?
        .as_str()
        .map(str::to_string)
}

fn answer_confidence(r: &DecisionRecord, question: &str) -> Option<f64> {
    let a = r.answers.get(question)?;
    a.confidence.or(a.p)
}

/// The person's decision at visit `visit` of gate `node`: `Ok(decision,
/// note)`, or why there is none.
fn gate_visit_decision<'a>(
    events: &'a [Event],
    node: &str,
    visit: u32,
) -> Result<(&'a str, &'a str), &'static str> {
    let mut seen = 0u32;
    let mut open = false;
    for e in events {
        match &e.payload {
            EventPayload::ReviewRequested { node: n, .. } if n == node => {
                if open {
                    // The next visit began before this one was decided.
                    return Err("the gate visit was never decided");
                }
                seen += 1;
                open = seen == visit;
            }
            EventPayload::ReviewWithdrawn { node: n, .. } if n == node && open => {
                return Err("the gate visit was withdrawn");
            }
            // A decision no person made is no person's label.
            EventPayload::ReviewDecided {
                node: n,
                decided_by: Some(by),
                ..
            } if n == node && open => return Err(not_a_person(by)),
            EventPayload::ReviewDecided {
                node: n,
                decision,
                note,
                ..
            } if n == node && open => return Ok((decision.as_str(), note.as_str())),
            _ => {}
        }
    }
    Err("the gate visit is not decided yet")
}

impl Labeller for ReviewTriageLabeller {
    fn use_site(&self) -> &'static str {
        "review_triage"
    }

    fn label_source(&self) -> &'static str {
        "the review_decided of the same gate visit (advise-shown decisions counted apart)"
    }

    fn today(&self) -> (Label, &'static str) {
        (Label::Keep, "a person decides every gate")
    }

    fn default_threshold(&self) -> f64 {
        0.9
    }

    fn act_probability(&self, r: &DecisionRecord) -> Option<f64> {
        r.p("decision")
    }

    /// The enforce path acts on a confident answer other than `approve`,
    /// which is never decided automatically.
    fn acts_at(&self, r: &DecisionRecord, threshold: f64) -> Option<bool> {
        let value = answer_value(r, "decision")?;
        let c = answer_confidence(r, "decision")?;
        Some(value != "approve" && c >= threshold)
    }

    fn label(&self, r: &DecisionRecord, events: &[Event]) -> Label {
        let Some(node) = r.node.as_deref() else {
            return Label::Unlabelled("no node on the decision");
        };
        let Some(visit) = r.attempt else {
            return Label::Unlabelled("no gate visit on the decision");
        };
        let Some(recommended) = answer_value(r, "decision") else {
            return Label::Unlabelled("no recommended option in the answer");
        };
        match gate_visit_decision(events, node, visit) {
            Err(why) => Label::Unlabelled(why),
            Ok((_, note)) if note.starts_with("auto:") => {
                Label::Unlabelled("the model decided the gate itself")
            }
            Ok((decision, _)) if decision == recommended => Label::Act,
            Ok(_) => Label::Keep,
        }
    }

    fn shown_to_labeller(&self, r: &DecisionRecord) -> bool {
        r.mode != "shadow"
    }
}

// --- retry advice (0.23.0, C9) ------------------------------------------------

/// Labels retry advice by the outcome of the next attempt of the same node
/// on the same executor (agent and model): `Keep` (retrying was right, as
/// today) when it succeeded, `Act` (switching or stopping would have been
/// right) when it failed too. Unlabelled when no further attempt followed,
/// when the next attempt ran on another executor (a fallback), and when an
/// enforced advice changed what ran next.
pub struct RetryAdviceLabeller;

impl Labeller for RetryAdviceLabeller {
    fn use_site(&self) -> &'static str {
        "retry_advice"
    }

    fn label_source(&self) -> &'static str {
        "the next same-executor attempt of the node: failed is act, succeeded is keep"
    }

    fn today(&self) -> (Label, &'static str) {
        (Label::Keep, "always retry the same executor")
    }

    fn default_threshold(&self) -> f64 {
        0.6
    }

    fn act_probability(&self, r: &DecisionRecord) -> Option<f64> {
        let p = r.p("next")?;
        Some(
            if answer_value(r, "next").as_deref() == Some(crate::decision::retry_advice::RETRY_SAME)
            {
                1.0 - p
            } else {
                p
            },
        )
    }

    fn acts_at(&self, r: &DecisionRecord, threshold: f64) -> Option<bool> {
        let value = answer_value(r, "next")?;
        let c = answer_confidence(r, "next")?;
        Some(
            (value == crate::decision::retry_advice::SWITCH
                || value == crate::decision::retry_advice::STOP)
                && c >= threshold,
        )
    }

    fn label(&self, r: &DecisionRecord, events: &[Event]) -> Label {
        self.label_with_rule(r, events).0
    }

    fn label_with_rule(
        &self,
        r: &DecisionRecord,
        events: &[Event],
    ) -> (Label, Option<&'static str>) {
        use apb_core::decision_thresholds::{RETRY_ADVICE_LEGACY_RULE, RETRY_ADVICE_RULE};
        let (Some(node), Some(failed)) = (r.node.as_deref(), r.attempt) else {
            return (
                Label::Unlabelled("no node or attempt on the decision"),
                Some(RETRY_ADVICE_LEGACY_RULE),
            );
        };
        let Some(at) = events.iter().position(|e| e.seq == r.seq) else {
            return (
                Label::Unlabelled("decision not in its journal"),
                Some(RETRY_ADVICE_LEGACY_RULE),
            );
        };
        let is_visit_start = |e: &Event| matches!(&e.payload, EventPayload::NodeStarted { node: n, .. } if n == node);
        let visit_from = events[..at].iter().rposition(is_visit_start).unwrap_or(0);
        // Attempt numbers restart at 1 on every visit of the node, so the
        // failed attempt is the one of the current visit: the latest start of
        // that number after the node's last `node_started`.
        let failed_start = events[..at]
            .iter()
            .rev()
            .take_while(|e| !is_visit_start(e))
            .find(|e| attempt_start(e, node, failed).is_some());
        // Up to the node's next execution: a later visit is not this retry.
        let window: Vec<&Event> = events[at + 1..]
            .iter()
            .take_while(|e| !is_visit_start(e))
            .collect();
        let next_start = window
            .iter()
            .copied()
            .find(|e| attempt_start(e, node, failed + 1).is_some());
        let has_exec = |e: &Event| attempt_start(e, node, 0).is_some_and(|s| s.exec.is_some());
        let exec_rule = failed_start.is_some_and(has_exec) && next_start.is_none_or(has_exec);
        let rule = if exec_rule {
            RETRY_ADVICE_RULE
        } else {
            RETRY_ADVICE_LEGACY_RULE
        };
        // The identities of this visit only, folded once per decision: the
        // attempts compared and their host tasks all sit inside it.
        let visit = &events[visit_from..at + 1 + window.len()];
        let ids = std::cell::OnceCell::new();
        let same = |next: &Event| match (failed_start, exec_rule) {
            (None, _) => SameExecutor::No,
            (Some(f), true) => same_by_identity(
                f,
                next,
                ids.get_or_init(|| crate::execution_identity::fold::fold(visit)),
            ),
            (Some(f), false) => {
                let key = |e: &Event| {
                    attempt_start(e, node, 0).map(|s| (s.agent.clone(), s.model.clone()))
                };
                if key(f) == key(next) {
                    SameExecutor::Yes
                } else {
                    SameExecutor::No
                }
            }
        };
        (outcome_after(node, failed, &window, same), Some(rule))
    }
}

/// The fields of an `attempt_started` of `node` the retry-advice labels
/// read; `attempt` 0 matches any attempt number.
struct AttemptStart<'a> {
    agent: &'a String,
    model: &'a Option<String>,
    exec: &'a Option<crate::execution_identity::ExecStep>,
}

fn attempt_start<'a>(e: &'a Event, node: &str, attempt: u32) -> Option<AttemptStart<'a>> {
    match &e.payload {
        EventPayload::AttemptStarted {
            node: n,
            attempt: a,
            agent,
            model,
            exec,
            ..
        } if n == node && (attempt == 0 || *a == attempt) => {
            Some(AttemptStart { agent, model, exec })
        }
        _ => None,
    }
}

/// Whether the next attempt ran on the failed one's executor.
enum SameExecutor {
    Yes,
    No,
    /// A host attempt whose tasks did not all report one model.
    Unknown,
}

/// Rule `exec/1`: a CLI attempt is the same executor when its binding and
/// step are; a host attempt and a CLI attempt never are; two host attempts
/// when both attempts' tasks reported one model and it is the same
/// ([`crate::execution_identity::fold::same_model`]), unknown otherwise.
/// The hint is never read as the model that ran.
fn same_by_identity(
    failed: &Event,
    next: &Event,
    ids: &[crate::execution_identity::fold::ExecutionIdentity],
) -> SameExecutor {
    use crate::execution_identity::fold::{ReportedModel, same_model};
    let id = |e: &Event| ids.iter().find(|i| i.attempt == e.seq);
    let (Some(f), Some(n)) = (id(failed), id(next)) else {
        return SameExecutor::No;
    };
    if f.is_host() != n.is_host() {
        return SameExecutor::No;
    }
    if f.is_host() {
        return match (f.reported_model(), n.reported_model()) {
            (ReportedModel::One { model: a }, ReportedModel::One { model: b }) => {
                if same_model(&a, &b) {
                    SameExecutor::Yes
                } else {
                    SameExecutor::No
                }
            }
            _ => SameExecutor::Unknown,
        };
    }
    let at = |i: &crate::execution_identity::fold::ExecutionIdentity| {
        i.step.as_ref().map(|s| (s.binding.clone(), s.step))
    };
    if at(f) == at(n) {
        SameExecutor::Yes
    } else {
        SameExecutor::No
    }
}

/// The retry-advice label from the events after the decision in the
/// node's current execution: the outcome of the next attempt when `same`
/// says it ran on the failed attempt's executor.
fn outcome_after(
    node: &str,
    failed: u32,
    window: &[&Event],
    same: impl Fn(&Event) -> SameExecutor,
) -> Label {
    let mut next_started = false;
    for (i, e) in window.iter().enumerate() {
        match &e.payload {
            EventPayload::SupervisorAction {
                action,
                node: Some(n),
                ..
            } if n == node && action == crate::decision::retry_advice::RETRY_ADVICE_ACTION => {
                return Label::Unlabelled("the enforced advice changed what ran next");
            }
            EventPayload::FallbackTriggered { node: n, .. } if n == node && !next_started => {
                return Label::Unlabelled("the next attempt ran on another executor");
            }
            EventPayload::AttemptStarted {
                node: n, attempt, ..
            } if n == node && *attempt == failed + 1 => {
                match same(e) {
                    SameExecutor::Yes => {}
                    SameExecutor::No => {
                        return Label::Unlabelled("the next attempt ran on another executor");
                    }
                    SameExecutor::Unknown => return Label::Unlabelled("executor unknown"),
                }
                next_started = true;
            }
            EventPayload::AttemptFinished {
                node: n,
                attempt,
                status,
                ..
            } if n == node && *attempt == failed + 1 => {
                // An attempt a pause interrupted (issue #200) says nothing
                // about the executor: it neither failed nor helped, and it
                // runs again on resume. The pause follows it in the journal.
                if status == "interrupted"
                    && window[i + 1..]
                        .iter()
                        .any(|f| matches!(f.payload, EventPayload::RunPaused { .. }))
                {
                    return Label::Unlabelled("the next attempt was interrupted by a pause");
                }
                return match status.as_str() {
                    "succeeded" => Label::Keep,
                    "failed" | "timed_out" | "interrupted" => Label::Act,
                    _ => Label::Unlabelled("the next attempt neither failed nor succeeded"),
                };
            }
            _ => {}
        }
    }
    if next_started {
        Label::Unlabelled("the next attempt has not finished yet")
    } else {
        Label::Unlabelled("no further attempt of the node followed")
    }
}

// --- end of the 0.23.0 labellers ------------------------------------------------

// --- the uses whose events do not carry join keys yet -----------------------

/// A use without a labeller yet: every decision stays unlabelled, with the
/// event that will label it once the use journals its join key.
pub struct PendingLabeller {
    use_site: &'static str,
    source: &'static str,
}

impl PendingLabeller {
    pub fn for_use(use_site: &str) -> Self {
        let (use_site, source) = match use_site {
            "supervisor_triage" => (
                "supervisor_triage",
                "pending: the supervisor directive that answered the wake (Wake key)",
            ),
            "routing" => (
                "routing",
                "pending: the attempt outcome on the chosen tier and any fallback (Attempt key)",
            ),
            "judge_node" => (
                "judge_node",
                "pending: later human corrections (weakest labels)",
            ),
            "judge_edge" => (
                "judge_edge",
                "pending: later human corrections (weakest labels)",
            ),
            "catalog_rank" => ("catalog_rank", "pending: the playbook the agent ran"),
            "host_task" => (
                "host_task",
                "none: the host task that asked acts on the answer itself (counts, latency and cost only)",
            ),
            "grade" => (
                "grade",
                "pending: a host or a person grading the same run on the same criterion",
            ),
            "problem_recheck" => (
                "problem_recheck",
                "pending: the problem reported again after the recheck, or a person's decision on it",
            ),
            "save_nudge" => (
                "save_nudge",
                "pending: the host agent's own offer, a save or a decline in the same session",
            ),
            _ => ("unknown", "no labeller for this use"),
        };
        PendingLabeller { use_site, source }
    }
}

impl Labeller for PendingLabeller {
    fn use_site(&self) -> &'static str {
        self.use_site
    }
    fn label_source(&self) -> &'static str {
        self.source
    }
    fn today(&self) -> (Label, &'static str) {
        (Label::Keep, "today's behaviour")
    }
    fn default_threshold(&self) -> f64 {
        0.5
    }
    fn act_probability(&self, _r: &DecisionRecord) -> Option<f64> {
        None
    }
    fn acts_at(&self, r: &DecisionRecord, _threshold: f64) -> Option<bool> {
        r.would_change
    }
    fn label(&self, _r: &DecisionRecord, _events: &[Event]) -> Label {
        Label::Unlabelled("no labeller for this use yet")
    }
    fn pending(&self) -> bool {
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::supervisor_action;
    use apb_core::decision_thresholds::{RETRY_ADVICE_LEGACY_RULE, RETRY_ADVICE_RULE};

    fn ev(seq: u64, payload: EventPayload) -> Event {
        Event {
            seq,
            ts: seq as u128,
            payload,
        }
    }

    fn started(seq: u64, node: &str) -> Event {
        ev(
            seq,
            EventPayload::NodeStarted {
                node: node.into(),
                attempt: 1,
            },
        )
    }

    fn finished(seq: u64, node: &str, status: &str) -> Event {
        ev(
            seq,
            EventPayload::node_finished(node, status, 1, String::new()),
        )
    }

    fn decision(seq: u64, node: &str) -> DecisionRecord {
        DecisionRecord {
            run_id: "r".into(),
            playbook: "p".into(),
            seq,
            ts: 0,
            use_site: "completion_check".into(),
            node: Some(node.into()),
            item: None,
            attempt: Some(1),
            provider: Some("main".into()),
            model: Some("m".into()),
            provider_kind: None,
            privacy_class: None,
            calibrated: true,
            mode: "shadow".into(),
            answers: BTreeMap::new(),
            applied: false,
            would_change: None,
            baseline: None,
            latency_ms: 1,
            cost_usd: None,
            cached: false,
            error: None,
            output_chars: None,
        }
    }

    #[test]
    fn a_later_attempt_or_the_decisions_own_wake_leaves_it_unlabelled() {
        let attempt = |seq: u64, n: u32| {
            ev(
                seq,
                serde_json::from_value(serde_json::json!({
                    "type": "attempt_started", "node": "a", "attempt": n, "agent": "x"
                }))
                .unwrap(),
            )
        };
        let events = vec![
            started(1, "a"),
            decision_event(2),
            attempt(3, 2),
            finished(4, "a", "succeeded"),
            started(5, "b"),
            finished(6, "b", "succeeded"),
        ];
        assert!(matches!(
            CompletionLabeller.label(&decision(2, "a"), &events),
            Label::Unlabelled(_)
        ));
        let wake = ev(
            3,
            EventPayload::WakeRaised {
                trigger: crate::event::WakeTrigger::Anomaly,
                node: "a".into(),
                detail: String::new(),
                triage: None,
            },
        );
        let retry = ev(
            4,
            EventPayload::SupervisorAction {
                action: supervisor_action::NODE_RETRY.into(),
                node: Some("a".into()),
                detail: String::new(),
            },
        );
        let events = vec![started(1, "a"), decision_event(2), wake, retry];
        let own_wake = Label::Unlabelled("the decision's own anomaly wake preceded the outcome");
        let mut advised = decision(2, "a");
        assert_ne!(CompletionLabeller.label(&advised, &events), own_wake);
        advised.mode = "advise".into();
        assert_eq!(CompletionLabeller.label(&advised, &events), own_wake);
    }

    fn decision_event(seq: u64) -> Event {
        ev(seq, EventPayload::resumed("placeholder", None, false))
    }

    // --- reply/1 ---

    const APPROVES_A: &str = "{ id: g, type: human_review, options: [ship, hold, reject], approves: { results: [a], options: [ship] } }";

    fn gate_yaml(gate: &str) -> String {
        format!(
            "schema: 2\nid: p\nname: p\nversion: 1.0.0\nnodes:\n  - {{ id: s, type: start }}\n  - {{ id: a, type: agent_task, prompt: hi, profile: x, results: [] }}\n  - {{ id: b, type: agent_task, prompt: hi, profile: x, results: [] }}\n  - {gate}\nedges: []\n"
        )
    }

    /// A run directory holding the stored definitions of `gates`, and their
    /// digests in order.
    fn stored(gates: &[&str]) -> (tempfile::TempDir, Vec<String>) {
        let dir = tempfile::tempdir().unwrap();
        let digests = gates
            .iter()
            .map(|g| {
                let yaml = gate_yaml(g);
                apb_core::schema::Playbook::from_yaml(&yaml).unwrap();
                crate::run_definitions::store(dir.path(), yaml.as_bytes()).unwrap()
            })
            .collect();
        (dir, digests)
    }

    fn json_ev(seq: u64, v: serde_json::Value) -> Event {
        ev(seq, serde_json::from_value(v).unwrap())
    }

    fn boundary(seq: u64, digest: &str) -> Event {
        let mut p = EventPayload::resumed("s", None, false);
        if let EventPayload::RunResumed { definition, .. } = &mut p {
            *definition = Some(digest.to_string());
        }
        ev(seq, p)
    }

    fn patched(seq: u64, digest: &str) -> Event {
        json_ev(
            seq,
            serde_json::json!({ "type": "patch_applied", "version": "1.0.1",
                "classification": "prompt", "continue_from": "g", "definition": digest }),
        )
    }

    /// A request of gate `g` bound to `a`'s snapshot settled at `settlement`.
    fn bound_request(seq: u64, settlement: u64) -> Event {
        json_ev(
            seq,
            serde_json::json!({ "type": "review_requested", "node": "g",
                "options": ["ship", "hold", "reject"],
                "binding": { "nonce": "n", "snapshots": [{ "node": "a", "execution": 1,
                    "settlement_seq": settlement, "snapshot_id": "s1" }] } }),
        )
    }

    fn person_decided(seq: u64, decision: &str) -> Event {
        json_ev(
            seq,
            serde_json::json!({ "type": "review_decided", "node": "g",
                "decision": decision, "note": "", "via": "terminal" }),
        )
    }

    fn gate_withdrawn(seq: u64, reason: &str) -> Event {
        json_ev(
            seq,
            serde_json::json!({ "type": "review_withdrawn", "node": "g", "reason": reason }),
        )
    }

    /// The definition boundary, `a` started, the decision at 2 and `a`
    /// settled at 3.
    fn reply(digest: &str) -> Vec<Event> {
        vec![
            boundary(0, digest),
            started(1, "a"),
            ev(2, EventPayload::hop("s", "a", false, false, Vec::new())),
            finished(3, "a", "succeeded"),
        ]
    }

    /// [`reply`], then gate `g` asks (bound to seq 3) and a person decides.
    fn approved(digest: &str, decision: &str) -> Vec<Event> {
        let mut events = reply(digest);
        events.extend([
            bound_request(4, 3),
            started(5, "g"),
            person_decided(6, decision),
        ]);
        events
    }

    fn label_in(dir: &std::path::Path, events: Vec<Event>) -> Label {
        let (label, rule) = CompletionLabeller.label_in(&decision(2, "a"), &events, Some(dir));
        assert_eq!(rule, Some(apb_core::decision_thresholds::COMPLETION_RULE));
        label
    }

    const NOT_APPROVED: Label =
        Label::Unlabelled("no person approved the reply at the gate that follows it");
    const NOT_THE_REPLY: Label = Label::Unlabelled("the gate does not approve the reply");

    #[test]
    fn an_approval_at_the_next_gate_labels_keep() {
        let (dir, d) = stored(&[APPROVES_A]);
        assert_eq!(label_in(dir.path(), approved(&d[0], "ship")), Label::Keep);
    }

    /// A list item's completion decision: with `on_needs_look: continue`
    /// an item needs a look, the list's node succeeds and a person approves
    /// the list's gate. Neither the aggregate `node_finished` nor that
    /// approval is the item's: the decision stays unlabelled, while the same
    /// journal labels a decision of the whole node keep.
    #[test]
    fn an_item_decision_is_never_credited_with_the_lists_approval() {
        let yaml = "schema: 2\nid: p\nname: p\nversion: 1.0.0\nnodes:\n  - { id: s, type: start }\n  - { id: a, type: agent_task, prompt: hi, profile: x, results: [], list: { items: { inline: [ { id: i1 }, { id: i2 } ] }, on_needs_look: continue } }\n  - { id: g, type: human_review, options: [ship, hold, reject], approves: { results: [a], options: [ship] } }\nedges: []\n";
        apb_core::schema::Playbook::from_yaml(yaml).unwrap();
        let dir = tempfile::tempdir().unwrap();
        let d = crate::run_definitions::store(dir.path(), yaml.as_bytes()).unwrap();
        let decided = json_ev(
            2,
            serde_json::json!({ "type": "decision_made", "use_site": "completion_check",
                "node": "a", "item": "i1", "attempt": 1, "provider": "main", "model": "m",
                "calibrated": true, "mode": "shadow", "questions_digest": "q",
                "state_digest": "s", "state_bytes": 1, "applied": false,
                "latency_ms": 1, "cached": false }),
        );
        let settled = |seq: u64, item: &str, state: &str| {
            json_ev(
                seq,
                serde_json::json!({ "type": "list_item_settled", "node": "a",
                    "item": item, "state": state }),
            )
        };
        let events = vec![
            boundary(0, &d),
            started(1, "a"),
            decided.clone(),
            settled(3, "i1", "needs_look"),
            settled(4, "i2", "passed"),
            finished(5, "a", "succeeded"),
            bound_request(6, 5),
            started(7, "g"),
            person_decided(8, "ship"),
        ];
        let record = DecisionRecord::from_event("r", "p", &decided).unwrap();
        assert_eq!(record.item.as_deref(), Some("i1"), "the item is kept");
        let (label, _) = CompletionLabeller.label_in(&record, &events, Some(dir.path()));
        assert_eq!(
            label,
            Label::Unlabelled("the decision judged a list item's reply")
        );
        // The same journal, read for a decision of the whole node.
        let whole = DecisionRecord {
            item: None,
            ..record
        };
        let (label, _) = CompletionLabeller.label_in(&whole, &events, Some(dir.path()));
        assert_eq!(label, Label::Keep);
    }

    #[test]
    fn label_alone_never_labels_keep() {
        let (_dir, d) = stored(&[APPROVES_A]);
        let events = approved(&d[0], "ship");
        assert_eq!(
            CompletionLabeller.label(&decision(2, "a"), &events),
            Label::Unlabelled("the gate's definition is not stored")
        );
        assert_eq!(
            CompletionLabeller
                .label_with_rule(&decision(2, "a"), &events)
                .1,
            Some(apb_core::decision_thresholds::COMPLETION_RULE)
        );
    }

    #[test]
    fn an_unstored_definition_leaves_it_unlabelled() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(
            label_in(dir.path(), approved("sha256:00", "ship")),
            Label::Unlabelled("the gate's definition is not stored")
        );
    }

    #[test]
    fn a_declared_approving_option_labels_keep() {
        let (dir, d) = stored(&[APPROVES_A]);
        // `approve` is not one of the gate's approving options, `ship` is.
        assert_eq!(label_in(dir.path(), approved(&d[0], "ship")), Label::Keep);
        assert_eq!(label_in(dir.path(), approved(&d[0], "hold")), NOT_APPROVED);
        assert_eq!(
            label_in(dir.path(), approved(&d[0], "approve")),
            NOT_APPROVED
        );
        // Without declared options the default `approve` approves.
        let (dir, d) = stored(&["{ id: g, type: human_review, approves: { results: [a] } }"]);
        assert_eq!(
            label_in(dir.path(), approved(&d[0], "approve")),
            Label::Keep
        );
    }

    #[test]
    fn a_gate_without_approves_leaves_it_unlabelled() {
        let (dir, d) = stored(&["{ id: g, type: human_review, options: [ship, hold, reject] }"]);
        assert_eq!(label_in(dir.path(), approved(&d[0], "ship")), NOT_THE_REPLY);
    }

    #[test]
    fn a_gate_approving_another_node_leaves_it_unlabelled() {
        let (dir, d) = stored(&[
            "{ id: g, type: human_review, options: [ship, hold, reject], approves: { results: [b], options: [ship] } }",
        ]);
        assert_eq!(label_in(dir.path(), approved(&d[0], "ship")), NOT_THE_REPLY);
    }

    #[test]
    fn a_binding_of_an_earlier_execution_leaves_it_unlabelled() {
        let (dir, d) = stored(&[APPROVES_A]);
        let mut events = reply(&d[0]);
        events.extend([
            bound_request(4, 1),
            started(5, "g"),
            person_decided(6, "ship"),
        ]);
        assert_eq!(label_in(dir.path(), events), NOT_THE_REPLY);
    }

    #[test]
    fn a_rejected_reply_stays_unlabelled() {
        let (dir, d) = stored(&[APPROVES_A]);
        assert_eq!(
            label_in(dir.path(), approved(&d[0], "reject")),
            NOT_APPROVED
        );
    }

    #[test]
    fn an_unattended_approval_leaves_it_unlabelled() {
        let (dir, d) = stored(&[APPROVES_A]);
        for (by, why) in [
            ("unattended", "the gate visit was decided unattended"),
            ("model", "the model decided the gate itself"),
        ] {
            let mut events = approved(&d[0], "ship");
            events.pop();
            events.push(json_ev(
                6,
                serde_json::json!({ "type": "review_decided", "node": "g",
                    "decision": "ship", "note": "", "decided_by": by }),
            ));
            assert_eq!(label_in(dir.path(), events), Label::Unlabelled(why), "{by}");
        }
    }

    #[test]
    fn a_publish_consent_is_not_a_review_of_the_reply() {
        let (dir, d) = stored(&[APPROVES_A]);
        let mut events = reply(&d[0]);
        events.extend([
            json_ev(
                4,
                serde_json::json!({ "type": "review_requested", "node": "g",
                    "options": ["ship"], "plan_digest": "sha256:p",
                    "binding": { "nonce": "n", "snapshots": [{ "node": "a", "execution": 1,
                        "settlement_seq": 3, "snapshot_id": "s1" }] } }),
            ),
            person_decided(5, "ship"),
        ]);
        assert_eq!(label_in(dir.path(), events), NOT_APPROVED);
    }

    #[test]
    fn a_finished_run_without_a_gate_stays_unlabelled() {
        let (dir, d) = stored(&[APPROVES_A]);
        let mut events = reply(&d[0]);
        events.extend([
            started(4, "b"),
            finished(5, "b", "succeeded"),
            ev(
                6,
                EventPayload::RunFinished {
                    outcome: "succeeded".into(),
                },
            ),
        ]);
        assert_eq!(label_in(dir.path(), events), NOT_APPROVED);
    }

    #[test]
    fn a_finished_report_followed_by_a_failing_node_is_not_act() {
        let (dir, d) = stored(&[APPROVES_A]);
        let mut events = reply(&d[0]);
        events.extend([started(4, "b"), finished(5, "b", "failed")]);
        assert_eq!(label_in(dir.path(), events), NOT_APPROVED);
    }

    #[test]
    fn a_supervisor_retry_leaves_the_reply_unlabelled() {
        let (dir, d) = stored(&[APPROVES_A]);
        let mut events = reply(&d[0]);
        events.extend([
            started(4, "b"),
            finished(5, "b", "succeeded"),
            ev(
                6,
                EventPayload::SupervisorAction {
                    action: supervisor_action::NODE_RETRY.into(),
                    node: Some("a".into()),
                    detail: String::new(),
                },
            ),
        ]);
        assert_eq!(label_in(dir.path(), events), NOT_APPROVED);
    }

    /// No journal labels `act`: a failing next node, a retry, a move back,
    /// a rejection, an approval.
    #[test]
    fn nothing_is_labelled_act_under_reply_1() {
        let (dir, d) = stored(&[APPROVES_A]);
        let mut failing = reply(&d[0]);
        failing.extend([started(4, "b"), finished(5, "b", "failed")]);
        let mut back = reply(&d[0]);
        back.push(ev(
            4,
            EventPayload::SupervisorAction {
                action: supervisor_action::RUN_CONTINUE_FROM.into(),
                node: Some("s".into()),
                detail: String::new(),
            },
        ));
        let mut retried = reply(&d[0]);
        retried.push(ev(
            4,
            EventPayload::SupervisorAction {
                action: supervisor_action::NODE_RETRY.into(),
                node: Some("a".into()),
                detail: String::new(),
            },
        ));
        for events in [
            failing,
            back,
            retried,
            approved(&d[0], "reject"),
            approved(&d[0], "ship"),
        ] {
            assert_ne!(label_in(dir.path(), events), Label::Act);
        }
    }

    #[test]
    fn an_approval_on_a_scope_reask_labels_keep() {
        let (dir, d) = stored(&[APPROVES_A]);
        let mut events = reply(&d[0]);
        events.extend([
            bound_request(4, 3),
            started(5, "g"),
            gate_withdrawn(6, crate::event::SCOPE_CHANGED_REASON),
            bound_request(7, 3),
            person_decided(8, "ship"),
        ]);
        assert_eq!(label_in(dir.path(), events), Label::Keep);
    }

    #[test]
    fn a_reask_binding_another_snapshot_leaves_it_unlabelled() {
        let (dir, d) = stored(&[APPROVES_A]);
        let mut events = reply(&d[0]);
        events.extend([
            bound_request(4, 3),
            started(5, "g"),
            gate_withdrawn(6, crate::event::SCOPE_CHANGED_REASON),
            bound_request(7, 1),
            person_decided(8, "ship"),
        ]);
        assert_eq!(
            label_in(dir.path(), events),
            Label::Unlabelled("the gate visit was withdrawn")
        );
    }

    #[test]
    fn a_withdrawal_without_a_reask_leaves_it_unlabelled() {
        let (dir, d) = stored(&[APPROVES_A]);
        let withdrawn_for = |reason: &str| {
            let mut events = reply(&d[0]);
            events.extend([
                bound_request(4, 3),
                started(5, "g"),
                gate_withdrawn(6, reason),
            ]);
            events
        };
        // A scope re-ask that never came: another node started first.
        let mut moved = withdrawn_for(crate::event::SCOPE_CHANGED_REASON);
        moved.extend([
            started(7, "b"),
            bound_request(8, 3),
            person_decided(9, "ship"),
        ]);
        assert_eq!(
            label_in(dir.path(), moved),
            Label::Unlabelled("the gate visit was withdrawn")
        );
        // A withdrawal for another reason, even when the gate asks again.
        let mut other = withdrawn_for("node_retry");
        other.extend([bound_request(7, 3), person_decided(8, "ship")]);
        assert_eq!(
            label_in(dir.path(), other),
            Label::Unlabelled("the gate visit was withdrawn")
        );
    }

    #[test]
    fn a_reask_after_a_patch_leaves_it_unlabelled() {
        // The patched gate's `ship` no longer approves.
        let (dir, d) = stored(&[
            APPROVES_A,
            "{ id: g, type: human_review, options: [ship, hold, reject], approves: { results: [a], options: [hold] } }",
        ]);
        let with_patch = |patch: bool| {
            let mut events = reply(&d[0]);
            events.extend([bound_request(4, 3), started(5, "g")]);
            if patch {
                events.push(patched(6, &d[1]));
            }
            events.extend([
                gate_withdrawn(7, crate::event::SCOPE_CHANGED_REASON),
                bound_request(8, 3),
                person_decided(9, "ship"),
            ]);
            events
        };
        assert_eq!(
            label_in(dir.path(), with_patch(true)),
            Label::Unlabelled("the gate's definition changed while it waited")
        );
        assert_eq!(label_in(dir.path(), with_patch(false)), Label::Keep);
    }

    #[test]
    fn only_a_persons_channel_labels_keep() {
        let (dir, d) = stored(&[APPROVES_A]);
        let through = |via: Option<&str>| {
            let mut events = approved(&d[0], "ship");
            events.pop();
            let mut decided = serde_json::json!({ "type": "review_decided", "node": "g",
                "decision": "ship", "note": "" });
            if let Some(v) = via {
                decided["via"] = serde_json::json!(v);
            }
            events.push(json_ev(6, decided));
            label_in(dir.path(), events)
        };
        assert_eq!(through(Some("terminal")), Label::Keep);
        assert_eq!(through(Some("dashboard_session")), Label::Keep);
        for via in [
            "mcp",
            "run_step",
            "cli_unattended",
            "dashboard_keyless",
            "http_bearer",
            "scenario",
            "a_newer_channel",
        ] {
            assert_eq!(
                through(Some(via)),
                Label::Unlabelled("the gate visit was decided through no person's channel"),
                "{via}"
            );
        }
        assert_eq!(
            through(None),
            Label::Unlabelled("the gate visit's decision records no channel")
        );
    }

    #[test]
    fn a_patch_while_the_gate_waits_leaves_it_unlabelled() {
        // No re-ask: the request at 4 is decided after a patch at 6.
        let (dir, d) = stored(&[APPROVES_A, APPROVES_A]);
        let mut events = reply(&d[0]);
        events.extend([
            bound_request(4, 3),
            started(5, "g"),
            patched(6, &d[1]),
            person_decided(7, "ship"),
        ]);
        assert_eq!(
            label_in(dir.path(), events),
            Label::Unlabelled("the gate's definition changed while it waited")
        );
    }

    #[test]
    fn the_final_requests_definition_is_read() {
        // Resumed with other bytes while the gate waited: there `hold`
        // approves and `ship` does not.
        let (dir, d) = stored(&[
            APPROVES_A,
            "{ id: g, type: human_review, options: [ship, hold, reject], approves: { results: [a], options: [hold] } }",
        ]);
        let chain = |decision: &str| {
            let mut events = reply(&d[0]);
            events.extend([
                bound_request(4, 3),
                started(5, "g"),
                boundary(6, &d[1]),
                gate_withdrawn(7, crate::event::SCOPE_CHANGED_REASON),
                bound_request(8, 3),
                person_decided(9, decision),
            ]);
            events
        };
        assert_eq!(label_in(dir.path(), chain("hold")), Label::Keep);
        assert_eq!(label_in(dir.path(), chain("ship")), NOT_APPROVED);
    }

    // --- 0.23.0 labellers (C9) ---

    fn answered(
        use_site: &str,
        node: &str,
        attempt: u32,
        q: &str,
        value: &str,
        mode: &str,
    ) -> DecisionRecord {
        let mut r = decision(2, node);
        r.use_site = use_site.into();
        r.attempt = Some(attempt);
        r.mode = mode.into();
        r.answers = BTreeMap::from([(
            q.to_string(),
            DecisionAnswer {
                value: Some(serde_json::json!(value)),
                p: Some(0.8),
                confidence: Some(0.95),
                invalid: None,
            },
        )]);
        r
    }

    fn requested(seq: u64, node: &str) -> Event {
        ev(
            seq,
            serde_json::from_value(serde_json::json!({
                "type": "review_requested", "node": node, "options": ["approve", "needs_changes"]
            }))
            .unwrap(),
        )
    }

    fn decided(seq: u64, node: &str, decision: &str, note: &str) -> Event {
        ev(
            seq,
            EventPayload::review_decided(node, decision, note, None),
        )
    }

    #[test]
    fn a_review_recommendation_is_labelled_by_the_same_visits_decision() {
        let l = ReviewTriageLabeller;
        // Visit 1 decided approve, visit 2 decided needs_changes.
        let events = vec![
            requested(1, "g"),
            decided(3, "g", "approve", ""),
            requested(4, "g"),
            decided(6, "g", "needs_changes", "fix the date"),
        ];
        let first = answered(
            "review_triage",
            "g",
            1,
            "decision",
            "needs_changes",
            "shadow",
        );
        assert_eq!(l.label(&first, &events), Label::Keep);
        let second = answered(
            "review_triage",
            "g",
            2,
            "decision",
            "needs_changes",
            "shadow",
        );
        assert_eq!(l.label(&second, &events), Label::Act);
        // Undecided, withdrawn, and the model's own decision stay unlabelled.
        let open = vec![requested(1, "g")];
        assert!(matches!(l.label(&first, &open), Label::Unlabelled(_)));
        let withdrawn = vec![
            requested(1, "g"),
            ev(
                2,
                EventPayload::ReviewWithdrawn {
                    node: "g".into(),
                    reason: String::new(),
                },
            ),
        ];
        assert!(matches!(l.label(&first, &withdrawn), Label::Unlabelled(_)));
        let auto = vec![
            requested(1, "g"),
            decided(2, "g", "needs_changes", "auto: main/m p=0.97"),
        ];
        assert!(matches!(l.label(&first, &auto), Label::Unlabelled(_)));
        // An unattended run's declared decision is no person's label.
        let mut unattended = decided(2, "g", "approve", "");
        if let EventPayload::ReviewDecided { decided_by, .. } = &mut unattended.payload {
            *decided_by = Some(crate::event::DecidedBy::Unattended);
        }
        let unattended = vec![requested(1, "g"), unattended];
        assert!(matches!(l.label(&first, &unattended), Label::Unlabelled(_)));
        // Nor is a model's enforced decision, by its decider alone (no
        // `auto:` note to fall back on).
        let mut by_model = decided(2, "g", "needs_changes", "");
        if let EventPayload::ReviewDecided { decided_by, .. } = &mut by_model.payload {
            *decided_by = Some(crate::event::DecidedBy::Model);
        }
        let by_model = vec![requested(1, "g"), by_model];
        assert_eq!(
            l.label(&first, &by_model),
            Label::Unlabelled("the model decided the gate itself")
        );
        // Shown to the reviewer outside shadow; acting never picks approve.
        assert!(!l.shown_to_labeller(&first));
        assert!(l.shown_to_labeller(&answered(
            "review_triage",
            "g",
            1,
            "decision",
            "approve",
            "advise"
        )));
        assert_eq!(l.acts_at(&first, 0.9), Some(true));
        assert_eq!(
            l.acts_at(
                &answered("review_triage", "g", 1, "decision", "approve", "shadow"),
                0.5
            ),
            Some(false)
        );
        assert!(!labeller_for("review_triage").pending());
    }

    fn attempt_started(seq: u64, node: &str, attempt: u32, model: &str) -> Event {
        ev(
            seq,
            serde_json::from_value(serde_json::json!({
                "type": "attempt_started", "node": node, "attempt": attempt, "agent": "claude", "model": model
            }))
            .unwrap(),
        )
    }

    fn attempt_finished(seq: u64, node: &str, attempt: u32, status: &str) -> Event {
        ev(
            seq,
            serde_json::from_value(serde_json::json!({
                "type": "attempt_finished", "node": node, "attempt": attempt, "status": status,
                "duration_ms": 1, "session": null, "summary": null
            }))
            .unwrap(),
        )
    }

    #[test]
    fn retry_advice_is_labelled_by_the_next_same_executor_attempt() {
        let l = RetryAdviceLabeller;
        let r = answered("retry_advice", "a", 1, "next", "switch_executor", "shadow");
        let base = || {
            vec![
                started(0, "a"),
                attempt_started(1, "a", 1, "m"),
                attempt_finished(2, "a", 1, "failed"),
                decision_event(3),
            ]
        };
        let mut failed_again = base();
        failed_again.extend([
            attempt_started(4, "a", 2, "m"),
            attempt_finished(5, "a", 2, "failed"),
        ]);
        assert_eq!(l.label(&r, &failed_again), Label::Act);
        let mut helped = base();
        helped.extend([
            attempt_started(4, "a", 2, "m"),
            attempt_finished(5, "a", 2, "succeeded"),
        ]);
        assert_eq!(l.label(&r, &helped), Label::Keep);
        // Another executor next (a fallback, or another model), no further
        // attempt, or an enforced skip: unlabelled.
        let mut other = base();
        other.extend([
            attempt_started(4, "a", 2, "other"),
            attempt_finished(5, "a", 2, "succeeded"),
        ]);
        assert!(matches!(l.label(&r, &other), Label::Unlabelled(_)));
        let mut fallback = base();
        fallback.push(ev(
            4,
            serde_json::from_value(serde_json::json!({
                "type": "fallback_triggered", "node": "a", "from": "claude", "to": "codex"
            }))
            .unwrap(),
        ));
        assert!(matches!(l.label(&r, &fallback), Label::Unlabelled(_)));
        assert!(matches!(l.label(&r, &base()), Label::Unlabelled(_)));
        let mut enforced = base();
        enforced.push(ev(
            4,
            EventPayload::SupervisorAction {
                action: crate::decision::retry_advice::RETRY_ADVICE_ACTION.into(),
                node: Some("a".into()),
                detail: String::new(),
            },
        ));
        assert!(matches!(l.label(&r, &enforced), Label::Unlabelled(_)));
        // An attempt a pause interrupted is not a failure of the executor:
        // unlabelled, while a supervisor-interrupted one still counts as Act.
        let mut paused = base();
        paused.extend([
            attempt_started(4, "a", 2, "m"),
            attempt_finished(5, "a", 2, "interrupted"),
            ev(6, crate::pause::engine_paused("operator pause")),
        ]);
        assert!(matches!(l.label(&r, &paused), Label::Unlabelled(_)));
        let mut interrupted = base();
        interrupted.extend([
            attempt_started(4, "a", 2, "m"),
            attempt_finished(5, "a", 2, "interrupted"),
        ]);
        assert_eq!(l.label(&r, &interrupted), Label::Act);
        // Acting means switching or stopping at the threshold.
        assert_eq!(l.acts_at(&r, 0.6), Some(true));
        assert_eq!(
            l.acts_at(
                &answered(
                    "retry_advice",
                    "a",
                    1,
                    "next",
                    "retry_same_likely_helps",
                    "shadow"
                ),
                0.6
            ),
            Some(false)
        );
        assert!(!labeller_for("retry_advice").pending());
    }

    #[test]
    fn retry_advice_compares_the_executors_of_the_current_visit() {
        let l = RetryAdviceLabeller;
        let mut r = answered("retry_advice", "a", 1, "next", "switch_executor", "shadow");
        r.seq = 9;
        let events = vec![
            // Visit 1: attempt 1 failed, a fallback ran attempt 2 elsewhere.
            started(0, "a"),
            attempt_started(1, "a", 1, "m"),
            attempt_finished(2, "a", 1, "failed"),
            ev(
                3,
                serde_json::from_value(serde_json::json!({
                    "type": "fallback_triggered", "node": "a", "from": "claude", "to": "codex"
                }))
                .unwrap(),
            ),
            attempt_started(4, "a", 2, "other"),
            attempt_finished(5, "a", 2, "succeeded"),
            // Visit 2 (a loop back): attempt numbers restart at 1.
            started(6, "a"),
            attempt_started(7, "a", 1, "m"),
            attempt_finished(8, "a", 1, "failed"),
            decision_event(9),
            attempt_started(10, "a", 2, "m"),
            attempt_finished(11, "a", 2, "failed"),
        ];
        assert_eq!(l.label(&r, &events), Label::Act);
    }

    fn exec_started(seq: u64, attempt: u32, binding: &str, step: u32, by: &str) -> Event {
        ev(
            seq,
            serde_json::from_value(serde_json::json!({
                "type": "attempt_started", "node": "a", "attempt": attempt,
                "agent": if by == "host" { "host" } else { "claude" }, "model": "m",
                "exec": {"binding": binding, "step": step, "source": {"kind": "primary"}, "executed_by": by}
            }))
            .unwrap(),
        )
    }

    fn host_task(seq: u64, task: &str, attempt: u32, hint: &str) -> Event {
        ev(
            seq,
            serde_json::from_value(serde_json::json!({
                "type": "host_task_requested", "task_id": task, "node": "a", "attempt": attempt, "model_hint": hint
            }))
            .unwrap(),
        )
    }

    fn host_submitted(seq: u64, task: &str, model: Option<&str>) -> Event {
        ev(
            seq,
            serde_json::from_value(serde_json::json!({
                "type": "host_task_submitted", "task_id": task, "status": "failed", "submitted_by": "host", "model": model
            }))
            .unwrap(),
        )
    }

    /// Design 2.10 item 1: under `exec/1` (both attempts journal `exec`) a
    /// CLI attempt is the same executor when its binding and step are; a
    /// journal without `exec` on either attempt is labelled by `exec/0`,
    /// today's agent and model comparison, unchanged.
    #[test]
    fn retry_advice_rules_follow_the_journal_of_both_attempts() {
        let l = RetryAdviceLabeller;
        let r = answered("retry_advice", "a", 1, "next", "switch_executor", "shadow");
        let failed = || {
            vec![
                started(0, "a"),
                exec_started(1, 1, "sha256:b1", 0, "cli"),
                attempt_finished(2, "a", 1, "failed"),
                decision_event(3),
            ]
        };
        let with_next = |binding: &str, step: u32, status: &str| {
            let mut e = failed();
            e.extend([
                exec_started(4, 2, binding, step, "cli"),
                attempt_finished(5, "a", 2, status),
            ]);
            e
        };
        assert_eq!(
            l.label_with_rule(&r, &with_next("sha256:b1", 0, "failed")),
            (Label::Act, Some(RETRY_ADVICE_RULE))
        );
        assert_eq!(
            l.label_with_rule(&r, &with_next("sha256:b1", 0, "succeeded")),
            (Label::Keep, Some(RETRY_ADVICE_RULE))
        );
        for (binding, step) in [("sha256:b1", 1), ("sha256:b2", 0)] {
            let (label, rule) = l.label_with_rule(&r, &with_next(binding, step, "succeeded"));
            assert_eq!(rule, Some(RETRY_ADVICE_RULE));
            assert_eq!(
                label,
                Label::Unlabelled("the next attempt ran on another executor")
            );
        }
        // No further attempt yet: the failed attempt's rule.
        assert_eq!(l.label_with_rule(&r, &failed()).1, Some(RETRY_ADVICE_RULE));
        // The next attempt without `exec` (an older binary resumed the run).
        let mut mixed = failed();
        mixed.extend([
            attempt_started(4, "a", 2, "m"),
            attempt_finished(5, "a", 2, "failed"),
        ]);
        assert_eq!(
            l.label_with_rule(&r, &mixed),
            (Label::Act, Some(RETRY_ADVICE_LEGACY_RULE))
        );
        let old = vec![
            started(0, "a"),
            attempt_started(1, "a", 1, "m"),
            attempt_finished(2, "a", 1, "failed"),
            decision_event(3),
            attempt_started(4, "a", 2, "m"),
            attempt_finished(5, "a", 2, "succeeded"),
        ];
        assert_eq!(
            l.label_with_rule(&r, &old),
            (Label::Keep, Some(RETRY_ADVICE_LEGACY_RULE))
        );
        // A use without rule versions has no rule; the completion check's
        // is chosen by the code.
        assert_eq!(
            ReviewTriageLabeller
                .label_with_rule(&decision(2, "a"), &old)
                .1,
            None
        );
        assert_eq!(
            CompletionLabeller
                .label_with_rule(&decision(2, "a"), &old)
                .1,
            Some(apb_core::decision_thresholds::COMPLETION_RULE)
        );
    }

    /// Design 2.10 item 1 and 2.13 item 3: a host attempt is another
    /// executor only when both reported one model and they differ; a changed
    /// hint with the same reported model is the same executor, and an
    /// attempt whose tasks did not all report one model is unlabelled.
    #[test]
    fn retry_advice_compares_what_the_host_reported_under_exec_1() {
        let l = RetryAdviceLabeller;
        let r = answered("retry_advice", "a", 1, "next", "switch_executor", "shadow");
        let journal = |first: Option<&str>, next: Option<&str>| {
            vec![
                started(0, "a"),
                host_task(1, "t1", 1, "sonnet"),
                exec_started(2, 1, "sha256:b1", 0, "host"),
                host_submitted(3, "t1", first),
                attempt_finished(4, "a", 1, "failed"),
                decision_event(5),
                host_task(6, "t2", 2, "opus"),
                exec_started(7, 2, "sha256:b1", 0, "host"),
                host_submitted(8, "t2", next),
                attempt_finished(9, "a", 2, "failed"),
            ]
        };
        let mut r = r;
        r.seq = 5;
        assert_eq!(
            l.label_with_rule(&r, &journal(Some("claude-sonnet-4-5"), Some("sonnet"))),
            (Label::Act, Some(RETRY_ADVICE_RULE)),
            "a changed hint with the same reported model"
        );
        assert_eq!(
            l.label_with_rule(&r, &journal(Some("sonnet"), Some("glm-5.3-flash")))
                .0,
            Label::Unlabelled("the next attempt ran on another executor")
        );
        assert_eq!(
            l.label_with_rule(&r, &journal(None, Some("sonnet"))).0,
            Label::Unlabelled("executor unknown")
        );
    }

    /// A host attempt and a CLI attempt are always two executors, whatever
    /// the host reported: never `executor unknown`.
    #[test]
    fn retry_advice_host_and_cli_attempts_are_another_executor() {
        let l = RetryAdviceLabeller;
        let mut r = answered("retry_advice", "a", 1, "next", "switch_executor", "shadow");
        r.seq = 5;
        for first in [None, Some("sonnet")] {
            let events = vec![
                started(0, "a"),
                host_task(1, "t1", 1, "sonnet"),
                exec_started(2, 1, "sha256:b1", 0, "host"),
                host_submitted(3, "t1", first),
                attempt_finished(4, "a", 1, "failed"),
                decision_event(5),
                exec_started(6, 2, "sha256:b1", 1, "cli"),
                attempt_finished(7, "a", 2, "succeeded"),
            ];
            assert_eq!(
                l.label_with_rule(&r, &events),
                (
                    Label::Unlabelled("the next attempt ran on another executor"),
                    Some(RETRY_ADVICE_RULE)
                ),
                "{first:?}"
            );
        }
    }
}
