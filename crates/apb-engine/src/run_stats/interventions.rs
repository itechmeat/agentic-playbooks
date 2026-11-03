//! Interventions of one run (U24): what came into the run from outside the
//! playbook's own flow, counted from the journal alone.
//!
//! A pure fold, [`interventions`], read by `apb stats` (per version, see
//! [`InterventionStats`]) and by the run retro. Nothing is journaled for it.
//!
//! Categories:
//!
//! - **Planned review**: every `review_requested` is one gate visit, closed
//!   by the next `review_decided` or `review_withdrawn` of its node. A
//!   decision is `automatic` when an applied `review_triage` decision of
//!   that visit or a `decided_by` says so, `channel` otherwise (through the
//!   review channel: a person or whoever holds the channel).
//! - **Question**: answers per question asked, by `answered_by`; `human`
//!   and `supervisor` answers are interventions, `unattended` an automatic
//!   answer, `timeout` neither.
//! - **Automatic decision**: applied `decision_made` of the completion
//!   check, retry advice, supervisor triage, review triage and tier routing,
//!   once per `(use, node, item, execution, attempt)`, so a cached replay
//!   after a resume counts once. Judge answers are the playbook's own
//!   routing and are not counted.
//! - **Operator action**: pause, resume, abort and expedite `by: operator`.
//!   The abort a parent's stop propagates into a child, and an expedite by
//!   a caller not shown to be a person (`by: host`), are the engine's, kept
//!   apart and never in a total.
//! - **Supervisor correction** (through the supervisor channel, which does
//!   not tell an agent from a person: `apb note` posts the same
//!   `context_append`): retries, continues, notes, interrupts, rebinds,
//!   patches and pause, resume, abort by the supervisor.
//! - **Engine action**: the engine's own markers and pauses, reported
//!   apart and never in a total.
//!
//! Absence is not zero: a fact that depends on a field newer than the
//! binaries that wrote the run reads unknown ([`ATTRIBUTION_SINCE`]), and a
//! pause, resume or abort without `by` is `actor unknown`.

use std::cmp::Ordering;
use std::collections::{BTreeMap, BTreeSet};

use serde::Serialize;

use crate::event::{DecidedBy, Event, EventPayload, supervisor_action};
use crate::identity::ExecutionIndex;
use crate::pause::kind::PauseBy;
use crate::resume_kind::ExpediteBy;

/// The first apb whose journals attribute every gate decision
/// (`decided_by`, `via`), every expedite and every unattended answer. A run
/// any older binary wrote reads those facts as unknown.
pub const ATTRIBUTION_SINCE: &str = "1.0.0-beta.3";

/// The decision uses whose applied answers are automatic decisions.
pub const AUTOMATIC_USES: [&str; 5] = [
    "completion_check",
    "retry_advice",
    "supervisor_triage",
    "review_triage",
    "routing",
];

/// Engine markers: reported apart, never an intervention.
const ENGINE_ACTIONS: [&str; 8] = [
    crate::failure_class::INFRA_RETRY_ACTION,
    crate::failure_class::TIMEOUT_CONTINUATION_ACTION,
    crate::pause::quota::QUOTA_PROBE_ACTION,
    crate::stall::STALL_ACTION,
    "interaction_downgraded",
    "retry_superseded_by_stop",
    "supervisor_billing_hold",
    supervisor_action::DIRECTIVE_REFUSED,
];

/// Markers that are the effect or the acknowledgement of something counted
/// elsewhere: never counted again.
const COUNTED_ELSEWHERE: [&str; 4] = [
    "control_received",
    crate::decision::supervisor_triage::TRIAGE_RETRY_ACTION,
    crate::decision::retry_advice::RETRY_ADVICE_ACTION,
    QUESTION_ABANDONED,
];

const QUESTION_ABANDONED: &str = "question_abandoned";
const ATTEMPT_INTERRUPTED: &str = "attempt_interrupted";

/// The `run_aborted` reason a parent's stop propagates into a sub-playbook
/// child (`crate::stop::abort_children` journals it as
/// `crate::stop::PARENT_ABORTED_REASON`). The person acted on the parent,
/// so the child counts the abort apart, never as its own operator action.
/// Kept as a literal here: an import from `crate::stop` would close an
/// import cycle across the run stats modules.
pub const PROPAGATED_ABORT: &str = "parent aborted";

/// The gate visits of one run.
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct ReviewCounts {
    /// Review requests: one per gate visit.
    pub visits: u64,
    /// Decisions through the review channel.
    pub channel: u64,
    /// Decisions the engine took: an enforced recommendation or the gate's
    /// unattended outcome.
    pub automatic: u64,
    /// Requests a directive closed without a decision.
    pub withdrawn: u64,
    /// Requests still open.
    pub open: u64,
    /// Decisions an older binary wrote without saying who decided:
    /// neither `channel` nor `automatic` is known for the run then.
    pub unattributed: u64,
    /// Decisions by option.
    pub options: BTreeMap<String, u64>,
    /// Channel decisions by the surface they came through; `not_recorded`
    /// when the decision names none.
    pub via: BTreeMap<String, u64>,
}

/// The questions of one run.
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct QuestionCounts {
    pub asked: u64,
    /// Answers by `answered_by`.
    pub answered: BTreeMap<String, u64>,
    pub abandoned: u64,
}

/// One decision use over a run.
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize)]
pub struct UseCounts {
    pub asked: u64,
    pub applied: u64,
}

/// What came into one run from outside its playbook's flow.
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct RunInterventions {
    /// The oldest apb that wrote the run, when every writer recorded one.
    #[cfg_attr(feature = "ts", ts(optional))]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub writer: Option<String>,
    /// The writer records who decided every gate, every expedite and every
    /// unattended answer ([`ATTRIBUTION_SINCE`]).
    pub attribution_known: bool,
    pub reviews: ReviewCounts,
    pub questions: QuestionCounts,
    /// Automatic decisions by use.
    pub automatic: BTreeMap<String, UseCounts>,
    /// Operator actions by kind.
    pub operator: BTreeMap<String, u64>,
    /// Supervisor corrections aimed at one node, by kind.
    pub supervisor_node: BTreeMap<String, u64>,
    /// Supervisor corrections of the whole run, by kind.
    pub supervisor_run: BTreeMap<String, u64>,
    /// Node executions begun (race losers left out): the denominator of
    /// the node-targeted corrections.
    pub node_executions: u64,
    /// Pauses, resumes, aborts and expedites that name no actor.
    pub actor_unknown: BTreeMap<String, u64>,
    /// The engine's own actions, never an intervention.
    pub engine: BTreeMap<String, u64>,
    /// Supervisor action names this binary does not know (a newer apb's),
    /// never in a total.
    pub other: BTreeMap<String, u64>,
    /// Distinct list items the counted decisions belong to.
    pub items: u64,
    /// Interventions: channel decisions, human and supervisor answers,
    /// operator actions and supervisor corrections. Unknown when a gate
    /// decision is unattributed or an actor is unknown.
    #[cfg_attr(feature = "ts", ts(optional))]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub total: Option<u64>,
    /// Automatic decisions, automatic gate decisions and unattended answers;
    /// unknown when a gate decision is unattributed.
    #[cfg_attr(feature = "ts", ts(optional))]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub automatic_total: Option<u64>,
}

impl RunInterventions {
    /// The review channel count, when known for the run.
    pub fn reviews_channel(&self) -> Option<u64> {
        (self.reviews.unattributed == 0).then_some(self.reviews.channel)
    }

    /// The operator actions, when no actor is unknown.
    pub fn operator_total(&self) -> Option<u64> {
        self.actor_unknown
            .is_empty()
            .then(|| self.operator.values().sum())
    }

    /// The run-level supervisor corrections, when no actor is unknown.
    pub fn supervisor_run_total(&self) -> Option<u64> {
        self.actor_unknown
            .is_empty()
            .then(|| self.supervisor_run.values().sum())
    }

    pub fn supervisor_node_total(&self) -> u64 {
        self.supervisor_node.values().sum()
    }

    /// Human and supervisor answers.
    pub fn question_total(&self) -> u64 {
        ["human", "supervisor"]
            .iter()
            .filter_map(|k| self.questions.answered.get(*k))
            .sum()
    }

    /// Applied automatic decisions over every use.
    pub fn applied_total(&self) -> u64 {
        self.automatic.values().map(|u| u.applied).sum()
    }
}

/// The oldest apb that wrote the journal: the start's `run_provenance.apb`
/// and every resume's `apb`. `None` when any of them recorded none (a run
/// an older binary started or resumed).
pub fn writer_of(events: &[Event]) -> Option<String> {
    let mut writers: Vec<Option<&str>> = Vec::new();
    for e in events {
        match &e.payload {
            EventPayload::RunProvenance { apb, .. } | EventPayload::RunResumed { apb, .. } => {
                writers.push(apb.as_deref());
            }
            _ => {}
        }
    }
    if writers.is_empty() {
        return None;
    }
    let mut oldest: Option<&str> = None;
    for w in writers {
        let w = w?;
        if oldest.is_none_or(|o| crate::run_binaries::compare(w, o) == Ordering::Less) {
            oldest = Some(w);
        }
    }
    oldest.map(str::to_string)
}

/// [`interventions`] of a whole journal, its writer read from it.
pub fn of_run(events: &[Event]) -> RunInterventions {
    let writer = writer_of(events);
    interventions(events, &crate::identity::index(events), writer.as_deref())
}

/// Whether a run `writer` wrote records every attribution field.
pub fn attribution_known(writer: Option<&str>) -> bool {
    writer.is_some_and(|w| crate::run_binaries::compare(w, ATTRIBUTION_SINCE) != Ordering::Less)
}

fn bump(map: &mut BTreeMap<String, u64>, key: &str) {
    *map.entry(key.to_string()).or_default() += 1;
}

/// One open gate visit.
struct OpenVisit {
    /// Applied `review_triage` decisions seen while it was open.
    triaged: bool,
}

/// One control entry's interrupt: the attempts it stopped, and whether the
/// entry named one node. A broadcast (no target in the control entry) is a
/// run correction whatever observed it.
#[derive(Default)]
struct InterruptHit {
    nodes: BTreeSet<String>,
    targeted: bool,
}

/// The interventions of one run's journal. `writer` is the oldest apb that
/// wrote it ([`writer_of`]).
pub fn interventions(
    events: &[Event],
    index: &ExecutionIndex,
    writer: Option<&str>,
) -> RunInterventions {
    let known = attribution_known(writer);
    let mut out = RunInterventions {
        writer: writer.map(str::to_string),
        attribution_known: known,
        ..Default::default()
    };
    let mut visits: BTreeMap<&str, Vec<OpenVisit>> = BTreeMap::new();
    // Applied `review_triage` decisions journaled before their visit's
    // `review_requested` (the real journal order): held per node until the
    // visit opens.
    let mut pending_triage: BTreeSet<String> = BTreeSet::new();
    let mut decisions: BTreeMap<(&str, &str, &str, u32, u32), bool> = BTreeMap::new();
    let mut items: BTreeSet<(&str, &str)> = BTreeSet::new();
    let mut interrupts: BTreeMap<String, InterruptHit> = BTreeMap::new();
    for e in events {
        match &e.payload {
            EventPayload::ReviewRequested { node, .. } => {
                out.reviews.visits += 1;
                visits.entry(node).or_default().push(OpenVisit {
                    triaged: pending_triage.remove(node),
                });
            }
            EventPayload::DecisionMade {
                use_site,
                node,
                attempt,
                applied,
                item,
                ..
            } if AUTOMATIC_USES.contains(&use_site.as_str()) => {
                let n = node.as_deref().unwrap_or("");
                let ordinal = node
                    .as_deref()
                    .and_then(|n| {
                        index
                            .containing(n, e.seq)
                            .or_else(|| index.routed_by(n, e.seq))
                    })
                    .map_or(0, |x| x.ordinal);
                let it = item.as_deref().unwrap_or("");
                let key = (use_site.as_str(), n, it, ordinal, attempt.unwrap_or(0));
                let seen = decisions.entry(key).or_default();
                *seen |= *applied;
                if !it.is_empty() {
                    items.insert((n, it));
                }
                if *applied
                    && use_site == "review_triage"
                    && let Some(v) = visits.get_mut(n).and_then(|q| q.first_mut())
                {
                    v.triaged = true;
                } else if *applied && use_site == "review_triage" {
                    // The triage decision came before the visit it advises.
                    pending_triage.insert(n.to_string());
                }
            }
            EventPayload::ReviewDecided {
                node,
                decision,
                decided_by,
                via,
                ..
            } => {
                let Some(v) = visits.get_mut(node.as_str()).filter(|q| !q.is_empty()) else {
                    continue;
                };
                let visit = v.remove(0);
                bump(&mut out.reviews.options, decision);
                let automatic = visit.triaged
                    || matches!(decided_by, Some(DecidedBy::Unattended | DecidedBy::Model))
                    || via.is_some_and(|c| !c.is_surface());
                if automatic {
                    out.reviews.automatic += 1;
                } else if decided_by.is_some() {
                    // A decider this binary does not know: not a person's
                    // channel, not an engine it can name.
                    out.reviews.unattributed += 1;
                } else if via.is_none() && !known {
                    out.reviews.unattributed += 1;
                } else {
                    out.reviews.channel += 1;
                    let surface = via.map_or("not_recorded", |c| c.as_str());
                    bump(&mut out.reviews.via, surface);
                }
            }
            EventPayload::ReviewWithdrawn { node, .. } => {
                if let Some(v) = visits.get_mut(node.as_str()).filter(|q| !q.is_empty()) {
                    v.remove(0);
                    out.reviews.withdrawn += 1;
                }
            }
            EventPayload::QuestionAsked { .. } => out.questions.asked += 1,
            EventPayload::QuestionAnswered { answered_by, .. } => {
                bump(&mut out.questions.answered, answered_by);
            }
            EventPayload::RunPaused { by, reason, .. } => {
                // The legacy resume marker of old journals is not a pause.
                if !reason.starts_with("resume from ") {
                    actor(&mut out, *by, "pause");
                }
            }
            EventPayload::RunResumed { by, .. } => actor(&mut out, *by, "resume"),
            EventPayload::RunAborted { by, reason, .. } => {
                if reason == PROPAGATED_ABORT {
                    // The engine relaying a parent's stop: kept apart with
                    // the other engine actions, never in a total.
                    bump(&mut out.engine, "abort_propagated");
                } else {
                    actor(&mut out, *by, "abort");
                }
            }
            EventPayload::RetryExpedited { by, .. } => match by {
                Some(ExpediteBy::Operator) => bump(&mut out.operator, "expedite"),
                Some(ExpediteBy::Supervisor) => bump(&mut out.supervisor_run, "expedite"),
                Some(ExpediteBy::Engine) => bump(&mut out.engine, "expedite"),
                // A caller not shown to be a person: not the operator, so
                // it is counted apart with the engine actions.
                Some(ExpediteBy::Host) => bump(&mut out.engine, "expedite_by_host"),
                None => bump(&mut out.actor_unknown, "expedite"),
            },
            EventPayload::ProfileRebound { .. } => bump(&mut out.supervisor_node, "rebind"),
            EventPayload::PatchApplied { .. } => bump(&mut out.supervisor_run, "patch"),
            EventPayload::SupervisorAction {
                action,
                node,
                detail,
            } => {
                let a = action.as_str();
                if a == ATTEMPT_INTERRUPTED {
                    // One interrupt reaches every attempt it stops; it is
                    // counted once, by the control entry it came from.
                    let key = control_seq(detail).unwrap_or_else(|| format!("@{}", e.seq));
                    let hit = interrupts.entry(key).or_default();
                    hit.nodes.insert(node.as_deref().unwrap_or("").to_string());
                } else if a == "control_received" && detail.starts_with("interrupt") {
                    // The acknowledgment carries the entry's target: a
                    // targeted interrupt names its node, a broadcast one
                    // does not (see `scheduler::node::observe_control`).
                    let key = control_seq(detail).unwrap_or_else(|| format!("@{}", e.seq));
                    let hit = interrupts.entry(key).or_default();
                    hit.targeted |= detail.contains("node `");
                } else if a == QUESTION_ABANDONED {
                    out.questions.abandoned += 1;
                } else if a == supervisor_action::NODE_RETRY
                    || a == supervisor_action::RUN_CONTINUE_FROM
                    || a == supervisor_action::CONTEXT_APPEND
                {
                    if node.is_some() {
                        bump(&mut out.supervisor_node, a);
                    } else {
                        bump(&mut out.supervisor_run, a);
                    }
                } else if ENGINE_ACTIONS.contains(&a) {
                    bump(&mut out.engine, a);
                } else if !COUNTED_ELSEWHERE.contains(&a) {
                    bump(&mut out.other, a);
                }
            }
            _ => {}
        }
    }
    for hit in interrupts.values() {
        // A targeted interrupt of one node is a node correction; a
        // broadcast interrupt is the run's, whatever observed it.
        if hit.targeted && hit.nodes.len() == 1 && !hit.nodes.contains("") {
            bump(&mut out.supervisor_node, "interrupt");
        } else {
            bump(&mut out.supervisor_run, "interrupt");
        }
    }
    out.reviews.open = visits.values().map(|q| q.len() as u64).sum();
    for ((use_site, ..), applied) in decisions {
        let u = out.automatic.entry(use_site.to_string()).or_default();
        u.asked += 1;
        u.applied += u64::from(applied);
    }
    out.items = items.len() as u64;
    out.node_executions = index
        .nodes()
        .map(|(_, x)| x.iter().filter(|x| !x.cancelled).count() as u64)
        .sum();
    let unattended_answers = out.questions.answered.get("unattended").copied();
    let unattended_known = known || unattended_answers.is_some();
    out.total = (|| {
        Some(
            out.reviews_channel()?
                + out.question_total()
                + out.operator_total()?
                + out.supervisor_run_total()?
                + out.supervisor_node_total(),
        )
    })();
    out.automatic_total = (out.reviews.unattributed == 0 && unattended_known).then(|| {
        // Review triage is counted through the gate decision it applied.
        let decided: u64 = out
            .automatic
            .iter()
            .filter(|(k, _)| k.as_str() != "review_triage")
            .map(|(_, u)| u.applied)
            .sum();
        decided + out.reviews.automatic + unattended_answers.unwrap_or(0)
    });
    out
}

/// A pause, resume or abort by `by`.
fn actor(out: &mut RunInterventions, by: Option<PauseBy>, kind: &str) {
    match by {
        Some(PauseBy::Operator) => bump(&mut out.operator, kind),
        Some(PauseBy::Supervisor) => bump(&mut out.supervisor_run, kind),
        Some(PauseBy::Engine) => bump(&mut out.engine, kind),
        None => bump(&mut out.actor_unknown, kind),
    }
}

/// The control entry an `attempt_interrupted` names: `(control seq N)`.
fn control_seq(detail: &str) -> Option<String> {
    let rest = detail.split("(control seq ").nth(1)?;
    let n: String = rest.chars().take_while(char::is_ascii_digit).collect();
    (!n.is_empty()).then_some(n)
}

/// A count over runs where it is known: `value` over the `known_runs` of
/// `runs` that could tell.
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize)]
pub struct KnownCount {
    pub value: u64,
    pub known_runs: usize,
    pub runs: usize,
}

impl KnownCount {
    fn add(&mut self, v: Option<u64>) {
        self.runs += 1;
        if let Some(v) = v {
            self.value += v;
            self.known_runs += 1;
        }
    }

    /// `3`, or `3 (known in 2 of 3 runs)`.
    pub fn text(&self) -> String {
        if self.known_runs == self.runs {
            self.value.to_string()
        } else {
            format!(
                "{} (known in {} of {} runs)",
                self.value, self.known_runs, self.runs
            )
        }
    }
}

/// One category over a version's finished runs.
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct CategoryStats {
    /// `review`, `question`, `automatic`, `operator`, `supervisor_node`
    /// or `supervisor_run`.
    pub category: String,
    /// What `denominator` counts: `gate visits`, `questions asked`,
    /// `decisions asked`, `runs` or `node executions`.
    pub unit: String,
    pub denominator: u64,
    /// The category's interventions (for `automatic`, the applied
    /// decisions).
    pub count: KnownCount,
    /// The count split by kind, option, surface or use.
    pub split: BTreeMap<String, KnownCount>,
}

/// Interventions over a version's finished runs.
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct InterventionStats {
    /// Finished runs.
    pub runs: usize,
    pub total: KnownCount,
    /// Interventions per run over the runs where it is known.
    #[cfg_attr(feature = "ts", ts(optional))]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub per_run_median: Option<f64>,
    #[cfg_attr(feature = "ts", ts(optional))]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub per_run_mean: Option<f64>,
    /// Runs without any intervention, over the runs where it is known.
    pub without_any: KnownCount,
    pub automatic_total: KnownCount,
    pub categories: Vec<CategoryStats>,
    /// The engine's own actions by kind, outside every total.
    pub engine: BTreeMap<String, u64>,
    /// Unknown supervisor action names, outside every total.
    pub other: BTreeMap<String, u64>,
    /// Actions that name no actor, outside every total.
    pub actor_unknown: BTreeMap<String, u64>,
}

#[derive(Default)]
struct CategoryAcc {
    denominator: u64,
    count: KnownCount,
    split: BTreeMap<String, KnownCount>,
}

impl CategoryAcc {
    fn split(&mut self, key: String, v: Option<u64>) {
        self.split.entry(key).or_default().add(v);
    }

    fn finish(self, category: &str, unit: &str) -> CategoryStats {
        CategoryStats {
            category: category.to_string(),
            unit: unit.to_string(),
            denominator: self.denominator,
            count: self.count,
            split: self.split,
        }
    }
}

/// Every key of `maps` once.
fn keys<'a>(maps: impl Iterator<Item = &'a BTreeMap<String, u64>>) -> BTreeSet<String> {
    maps.flat_map(|m| m.keys().cloned()).collect()
}

fn merge(into: &mut BTreeMap<String, u64>, from: &BTreeMap<String, u64>) {
    for (k, v) in from {
        *into.entry(k.clone()).or_default() += v;
    }
}

/// The version figures over the finished runs' interventions; `None` when
/// there is none.
pub fn version_stats(runs: &[RunInterventions]) -> Option<InterventionStats> {
    if runs.is_empty() {
        return None;
    }
    let mut s = InterventionStats {
        runs: runs.len(),
        ..Default::default()
    };
    let mut per_run: Vec<u64> = Vec::new();
    let (mut review, mut question, mut auto) = Default::default();
    let (mut operator, mut sup_node, mut sup_run): (CategoryAcc, CategoryAcc, CategoryAcc) =
        Default::default();
    let review_keys = keys(runs.iter().map(|r| &r.reviews.options));
    let via_keys = keys(runs.iter().map(|r| &r.reviews.via));
    let answer_keys = keys(runs.iter().map(|r| &r.questions.answered));
    let op_keys = keys(runs.iter().map(|r| &r.operator));
    let node_keys = keys(runs.iter().map(|r| &r.supervisor_node));
    let run_keys = keys(runs.iter().map(|r| &r.supervisor_run));
    let use_keys: BTreeSet<String> = runs
        .iter()
        .flat_map(|r| r.automatic.keys().cloned())
        .collect();
    for r in runs {
        s.total.add(r.total);
        s.without_any.add(r.total.map(|t| u64::from(t == 0)));
        s.automatic_total.add(r.automatic_total);
        per_run.extend(r.total);
        merge(&mut s.engine, &r.engine);
        merge(&mut s.other, &r.other);
        merge(&mut s.actor_unknown, &r.actor_unknown);
        let attributed = r.reviews.unattributed == 0;
        let review_acc: &mut CategoryAcc = &mut review;
        review_acc.denominator += r.reviews.visits;
        review_acc.count.add(r.reviews_channel());
        review_acc.split("channel".into(), r.reviews_channel());
        review_acc.split(
            "automatic".into(),
            attributed.then_some(r.reviews.automatic),
        );
        review_acc.split("withdrawn".into(), Some(r.reviews.withdrawn));
        for k in &review_keys {
            review_acc.split(
                format!("option:{k}"),
                Some(r.reviews.options.get(k).copied().unwrap_or(0)),
            );
        }
        for k in &via_keys {
            review_acc.split(
                format!("via:{k}"),
                attributed.then(|| r.reviews.via.get(k).copied().unwrap_or(0)),
            );
        }
        let q: &mut CategoryAcc = &mut question;
        q.denominator += r.questions.asked;
        q.count.add(Some(r.question_total()));
        for k in &answer_keys {
            let v = r.questions.answered.get(k).copied().unwrap_or(0);
            // An older writer could not record an unattended answer.
            let known = k != "unattended" || r.attribution_known || v > 0;
            q.split(k.clone(), known.then_some(v));
        }
        q.split("abandoned".into(), Some(r.questions.abandoned));
        let a: &mut CategoryAcc = &mut auto;
        a.denominator += r.automatic.values().map(|u| u.asked).sum::<u64>();
        a.count.add(Some(r.applied_total()));
        for k in &use_keys {
            let u = r.automatic.get(k).copied().unwrap_or_default();
            a.split(format!("asked:{k}"), Some(u.asked));
            a.split(format!("applied:{k}"), Some(u.applied));
        }
        operator.denominator += 1;
        operator.count.add(r.operator_total());
        for k in &op_keys {
            let v = r.operator.get(k).copied().unwrap_or(0);
            operator.split(k.clone(), r.actor_unknown.is_empty().then_some(v));
        }
        sup_node.denominator += r.node_executions;
        sup_node.count.add(Some(r.supervisor_node_total()));
        for k in &node_keys {
            sup_node.split(
                k.clone(),
                Some(r.supervisor_node.get(k).copied().unwrap_or(0)),
            );
        }
        sup_run.denominator += 1;
        sup_run.count.add(r.supervisor_run_total());
        for k in &run_keys {
            let v = r.supervisor_run.get(k).copied().unwrap_or(0);
            sup_run.split(k.clone(), r.actor_unknown.is_empty().then_some(v));
        }
    }
    if !per_run.is_empty() {
        let mean = per_run.iter().sum::<u64>() as f64 / per_run.len() as f64;
        s.per_run_mean = Some((mean * 100.0).round() / 100.0);
        s.per_run_median = median_f(&mut per_run);
    }
    s.categories = vec![
        review.finish("review", "gate visits"),
        question.finish("question", "questions asked"),
        auto.finish("automatic", "decisions asked"),
        operator.finish("operator", "runs"),
        sup_node.finish("supervisor_node", "node executions"),
        sup_run.finish("supervisor_run", "runs"),
    ];
    Some(s)
}

/// The median, the mean of the two middle values for an even count.
fn median_f(values: &mut [u64]) -> Option<f64> {
    if values.is_empty() {
        return None;
    }
    values.sort_unstable();
    let n = values.len();
    Some(if n % 2 == 1 {
        values[n / 2] as f64
    } else {
        (values[n / 2 - 1] + values[n / 2]) as f64 / 2.0
    })
}

/// `a 1, b 2`.
fn kinds(map: &BTreeMap<String, u64>) -> String {
    map.iter()
        .map(|(k, v)| format!("{k} {v}"))
        .collect::<Vec<_>>()
        .join(", ")
}

/// The version figures as `apb stats` prints them, one line each.
pub fn lines(s: &InterventionStats) -> Vec<String> {
    let mut out = Vec::new();
    let mut head = format!(
        "interventions: {} over {} finished runs",
        s.total.text(),
        s.runs
    );
    if let (Some(m), Some(a)) = (s.per_run_median, s.per_run_mean) {
        head.push_str(&format!(", median {m} and mean {a} per run"));
    }
    head.push_str(&format!(
        ", runs without any {} of {}",
        s.without_any.value, s.without_any.known_runs
    ));
    if s.without_any.known_runs != s.without_any.runs {
        head.push_str(&format!(
            " (known in {} of {} runs)",
            s.without_any.known_runs, s.without_any.runs
        ));
    }
    out.push(head);
    for c in &s.categories {
        let label = match c.category.as_str() {
            "review" => "  decided through the review channel",
            "question" => "  questions answered by a person or the supervisor",
            "automatic" => "  automatic decisions applied (not interventions)",
            "operator" => "  operator actions",
            "supervisor_node" => "  node corrections through the supervisor channel",
            "supervisor_run" => "  run corrections through the supervisor channel",
            other => other,
        };
        let mut line = format!(
            "{label}: {} over {} {}",
            c.count.text(),
            c.denominator,
            c.unit
        );
        let split: Vec<String> = c
            .split
            .iter()
            .filter(|(_, k)| k.value > 0 || k.known_runs != k.runs)
            .map(|(name, k)| format!("{name} {}", k.text()))
            .collect();
        if !split.is_empty() {
            line.push_str(&format!(" ({})", split.join(", ")));
        }
        out.push(line);
    }
    out.push(format!("  automatic total: {}", s.automatic_total.text()));
    if !s.engine.is_empty() {
        out.push(format!(
            "  engine actions (not interventions): {}",
            kinds(&s.engine)
        ));
    }
    if !s.actor_unknown.is_empty() {
        out.push(format!(
            "  actor unknown (outside the totals): {}",
            kinds(&s.actor_unknown)
        ));
    }
    if !s.other.is_empty() {
        out.push(format!(
            "  actions this apb does not know (outside the totals): {}",
            kinds(&s.other)
        ));
    }
    out
}

/// One run's interventions as the retro prints them.
pub fn run_lines(r: &RunInterventions) -> Vec<String> {
    let unknown = "unknown".to_string();
    let total = r.total.map_or(unknown.clone(), |t| t.to_string());
    let mut out = vec![format!("interventions: {total}")];
    let channel = r
        .reviews_channel()
        .map_or(unknown.clone(), |c| c.to_string());
    out.push(format!(
        "  gate visits {}: through the review channel {channel}, automatic {}, withdrawn {}",
        r.reviews.visits, r.reviews.automatic, r.reviews.withdrawn
    ));
    if r.questions.asked > 0 {
        out.push(format!(
            "  questions asked {}: {}",
            r.questions.asked,
            kinds(&r.questions.answered)
        ));
    }
    if !r.operator.is_empty() {
        out.push(format!("  operator actions: {}", kinds(&r.operator)));
    }
    let mut sup = r.supervisor_node.clone();
    merge(&mut sup, &r.supervisor_run);
    if !sup.is_empty() {
        out.push(format!(
            "  corrections through the supervisor channel: {}",
            kinds(&sup)
        ));
    }
    let auto = r.automatic_total.map_or(unknown, |t| t.to_string());
    out.push(format!("  automatic decisions and answers: {auto}"));
    if !r.engine.is_empty() {
        out.push(format!(
            "  engine actions (not interventions): {}",
            kinds(&r.engine)
        ));
    }
    if !r.actor_unknown.is_empty() {
        out.push(format!("  actor unknown: {}", kinds(&r.actor_unknown)));
    }
    if !r.attribution_known {
        out.push(format!(
            "  written by an apb older than {ATTRIBUTION_SINCE}: who decided is not recorded"
        ));
    }
    out
}
