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

use crate::event::{DecisionAnswer, DecisionBaseline, Event, EventPayload, supervisor_action};

/// One journaled `decision_made`, with the run it belongs to.
#[derive(Debug, Clone, PartialEq)]
pub struct DecisionRecord {
    pub run_id: String,
    pub playbook: String,
    pub seq: u64,
    pub ts: u128,
    pub use_site: String,
    pub node: Option<String>,
    pub attempt: Option<u32>,
    pub provider: Option<String>,
    pub model: Option<String>,
    /// The provider's kind from the run manifest (`systemone`, `fake`, an
    /// emulation), when the manifest names it.
    pub provider_kind: Option<String>,
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
            attempt: *attempt,
            provider: provider.clone(),
            model: model.clone(),
            provider_kind: None,
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

/// Labels a completion check from what happened after the attempt:
///
/// - `Act` (the reply was not finished) when, before the node starts again,
///   a supervisor retried it (`node_retry`), the run was moved back to it or
///   to a node that ran before it (`run_continue_from`, a patch or migration
///   `continue_from`), or the next node to start after it finished `failed`;
/// - `Keep` when the next node finished `succeeded` with none of those, or
///   the node was the last one and the run finished `succeeded`;
/// - unlabelled while the run has not shown either, when another attempt of
///   the same visit followed (its outcome is not this attempt's), and above
///   shadow when the check's own anomaly wake came first (the outcome may be
///   the decision's doing).
pub struct CompletionLabeller;

/// The node a supervisor or patch moved the run back to, if this event does.
fn moved_back_to(e: &Event) -> Option<&str> {
    match &e.payload {
        EventPayload::SupervisorAction {
            action,
            node: Some(n),
            ..
        } if action == supervisor_action::NODE_RETRY
            || action == supervisor_action::RUN_CONTINUE_FROM =>
        {
            Some(n)
        }
        EventPayload::PatchApplied { continue_from, .. }
        | EventPayload::RunMigrated { continue_from, .. } => Some(continue_from),
        _ => None,
    }
}

impl Labeller for CompletionLabeller {
    fn use_site(&self) -> &'static str {
        "completion_check"
    }

    fn label_source(&self) -> &'static str {
        "downstream outcome: the next node failing, a supervisor retry, or a continue_from back"
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
        let Some(node) = r.node.as_deref() else {
            return Label::Unlabelled("no node on the decision");
        };
        let Some(at) = events.iter().position(|e| e.seq == r.seq) else {
            return Label::Unlabelled("decision not in its journal");
        };
        // Nodes that ran before the decision: moving back to one of them
        // redoes the checked node's work.
        let before: Vec<&str> = events[..at]
            .iter()
            .filter_map(|e| match &e.payload {
                EventPayload::NodeStarted { node, .. } => Some(node.as_str()),
                _ => None,
            })
            .collect();
        // The events up to the node's next start: a retry of it or a move
        // back ends up there, and a later start is a new decision's business.
        let window: Vec<&Event> = events[at + 1..]
            .iter()
            .take_while(
                |e| !matches!(&e.payload, EventPayload::NodeStarted { node: n, .. } if n == node),
            )
            .collect();
        let restarted = window.len() < events.len() - at - 1;
        // Another attempt of the same visit (the checked one was rejected
        // or failed after it): what follows is that attempt's outcome, not
        // this one's, and an enforce rejection would label itself.
        if window.iter().any(|e| {
            matches!(&e.payload, EventPayload::AttemptStarted { node: n, attempt, .. }
                if n == node && r.attempt.is_some_and(|a| *attempt > a))
        }) {
            return Label::Unlabelled("a later attempt of the node followed");
        }
        // Above shadow the check raised its own anomaly wake: a supervisor
        // retry that answers it would confirm the decision by its own doing.
        if r.mode != "shadow"
            && window.iter().any(|e| {
                matches!(&e.payload, EventPayload::WakeRaised { trigger: crate::event::WakeTrigger::Anomaly, node: n, .. } if n == node)
            })
        {
            return Label::Unlabelled("the decision's own anomaly wake preceded the outcome");
        }
        if window.iter().any(|e| {
            moved_back_to(e).is_some_and(|target| target == node || before.contains(&target))
        }) {
            return Label::Act;
        }
        let mut own_finished = false;
        let mut next: Option<&str> = None;
        for e in &window {
            match &e.payload {
                EventPayload::NodeFinished { node: n, .. } if n == node => own_finished = true,
                EventPayload::NodeStarted { node: n, .. } if own_finished && next.is_none() => {
                    next = Some(n.as_str());
                }
                EventPayload::NodeFinished {
                    node: n, status, ..
                } if Some(n.as_str()) == next => {
                    return match status.as_str() {
                        "failed" => Label::Act,
                        "succeeded" => Label::Keep,
                        _ => Label::Unlabelled("the next node neither failed nor succeeded"),
                    };
                }
                EventPayload::RunFinished { outcome } if next.is_none() && own_finished => {
                    return if outcome == "succeeded" {
                        Label::Keep
                    } else {
                        Label::Unlabelled("the run ended without a next node")
                    };
                }
                _ => {}
            }
        }
        if restarted {
            return Label::Unlabelled("the node ran again before any outcome");
        }
        Label::Unlabelled("no downstream outcome yet")
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
            } if n == node && open => {
                return Err(match by {
                    crate::event::DecidedBy::Unattended => "the gate visit was decided unattended",
                    crate::event::DecidedBy::Model => "the model decided the gate itself",
                    crate::event::DecidedBy::Unknown => "the gate visit was decided by no person",
                });
            }
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
            attempt: Some(1),
            provider: Some("main".into()),
            model: Some("m".into()),
            provider_kind: None,
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
        let mut advised = decision(2, "a");
        assert_eq!(CompletionLabeller.label(&advised, &events), Label::Act);
        advised.mode = "advise".into();
        assert!(matches!(
            CompletionLabeller.label(&advised, &events),
            Label::Unlabelled(_)
        ));
    }

    fn decision_event(seq: u64) -> Event {
        ev(seq, EventPayload::resumed("placeholder", None, false))
    }

    fn label(events: &[Event]) -> Label {
        CompletionLabeller.label(&decision(2, "a"), events)
    }

    #[test]
    fn the_next_node_decides_the_completion_label() {
        let base = || vec![started(0, "start"), started(1, "a"), decision_event(2)];
        let mut ok = base();
        ok.extend([finished(3, "a", "succeeded"), started(4, "b")]);
        let mut pending = ok.clone();
        ok.push(finished(5, "b", "succeeded"));
        assert_eq!(label(&ok), Label::Keep);
        let mut failed = pending.clone();
        failed.push(finished(5, "b", "failed"));
        assert_eq!(label(&failed), Label::Act);
        pending.push(ev(5, crate::pause::engine_paused("x")));
        assert!(matches!(label(&pending), Label::Unlabelled(_)));
        // The last node: the run's outcome labels it.
        let mut last = base();
        last.extend([
            finished(3, "a", "succeeded"),
            ev(
                4,
                EventPayload::RunFinished {
                    outcome: "succeeded".into(),
                },
            ),
        ]);
        assert_eq!(label(&last), Label::Keep);
    }

    #[test]
    fn a_retry_or_a_move_back_labels_act() {
        let mut retry = vec![
            started(0, "start"),
            started(1, "a"),
            decision_event(2),
            finished(3, "a", "succeeded"),
            started(4, "b"),
            finished(5, "b", "succeeded"),
        ];
        let mut back = retry.clone();
        retry.push(ev(
            6,
            EventPayload::SupervisorAction {
                action: supervisor_action::NODE_RETRY.into(),
                node: Some("a".into()),
                detail: String::new(),
            },
        ));
        // A retry of the node wins even after the next node succeeded (a
        // gate that let a false success through).
        assert_eq!(label(&retry), Label::Act);
        back.truncate(5);
        back.push(ev(
            5,
            EventPayload::SupervisorAction {
                action: supervisor_action::RUN_CONTINUE_FROM.into(),
                node: Some("start".into()),
                detail: String::new(),
            },
        ));
        assert_eq!(label(&back), Label::Act);
        // A move forward to a node that never ran before is not a move back.
        let mut forward = back.clone();
        forward.pop();
        forward.push(ev(
            5,
            EventPayload::SupervisorAction {
                action: supervisor_action::RUN_CONTINUE_FROM.into(),
                node: Some("c".into()),
                detail: String::new(),
            },
        ));
        assert!(matches!(label(&forward), Label::Unlabelled(_)));
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
        // Every other use has no rule.
        assert_eq!(
            CompletionLabeller
                .label_with_rule(&decision(2, "a"), &old)
                .1,
            None
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
