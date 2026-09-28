//! Executor tier routing (issue #165 Part 12, Part 14.5).
//!
//! Before the first attempt of an `agent_task` with `route: auto` whose
//! profile declares `tiers`: one `tier` choice over the declared tiers (their
//! `for` texts as criteria, plus `unclear`) and one `difficulty` score. The
//! answer is smoothed by hysteresis toward the tier the previous node of the
//! same profile ran on: a different tier needs
//! `uses.routing.thresholds.hysteresis` confidence.
//!
//! Shadow and advise journal the would-be tier (`join.tier`) and run the
//! profile's own executor. Enforce (a stored threshold, actions left): the
//! first attempt runs on the recommended tier, through the same rebind
//! overlay a supervisor's `rebind_profile` writes (`profile_rebound` with a
//! `routing:` reason); an agent failure on a tier below the profile's own
//! executor goes up a tier at once (`fallback_triggered.reason: routing`)
//! before the normal chain.
//!
//! Never routed (no request, the reason journaled): `continue_session`
//! nodes and handoff sources (a warm handoff needs the same executor), a
//! node a supervisor rebound, an answer round. Retries and fallback steps of
//! an execution are never routed: routing happens once, before attempt 1.

use std::collections::BTreeMap;

use apb_core::decisions::DecisionMode;
use apb_decide::{Question, UseSite};
use serde_json::{Value, json};

use super::{
    DecisionCall, DecisionJournal, DecisionOutcome, DecisionRunner, Enforce, FieldClass,
    Judgement, StateField, StateParts,
};
use crate::event::{DecisionAnswer, Event, EventPayload};
use crate::manifest::ManifestProfile;

const PROMPT_HEAD: usize = 3 * 1024;

/// The five difficulty levels, in words (a model never sees numbers).
const LEVELS: [&str; 5] = [
    "a mechanical edit or lookup",
    "a small, local change",
    "an ordinary implementation or review task",
    "a change across several parts that needs care",
    "a cross-cutting design, concurrency or security change",
];

/// The name of the tier that is the profile's own executor, or `executor`
/// when no tier says `use: executor` (the executor then counts as the
/// heaviest).
pub(crate) fn executor_tier(entry: &ManifestProfile) -> (String, usize) {
    match entry.tiers.iter().position(|t| t.invocation.is_none()) {
        Some(i) => (entry.tiers[i].name.clone(), i),
        None => ("executor".to_string(), entry.tiers.len()),
    }
}

pub(crate) fn questions(entry: &ManifestProfile) -> BTreeMap<String, Question> {
    let mut criteria: Vec<(String, Option<Value>)> = entry
        .tiers
        .iter()
        .map(|t| (t.name.clone(), Some(json!(t.for_work))))
        .collect();
    criteria.push((
        "unclear".to_string(),
        Some(json!("`step` does not say enough to tell.")),
    ));
    BTreeMap::from([
        (
            "tier".to_string(),
            Question::Choice {
                instructions: json!(
                    "Which kind of work does `step` ask for, judging by its title, prompt and declared outputs?"
                ),
                criteria: criteria.into_iter().collect(),
            },
        ),
        (
            "difficulty".to_string(),
            Question::Score {
                instructions: json!("How hard is the work `step` asks for?"),
                levels: LEVELS.iter().map(|l| json!(l)).collect(),
            },
        ),
    ])
}

/// The tier the last node of the same profile ran on in this run: its
/// routing decision's `join.tier` when it was applied, else the profile's
/// executor tier (`join.executor_tier`).
pub(crate) fn previous_tier(events: &[Event], profile: &str, node: &str) -> Option<String> {
    events.iter().rev().find_map(|e| match &e.payload {
        EventPayload::DecisionMade {
            use_site,
            node: Some(n),
            join,
            applied,
            error: None,
            ..
        } if use_site == "routing"
            && n != node
            && join.get("profile").and_then(Value::as_str) == Some(profile)
            && join.contains_key("tier") =>
        {
            let key = if *applied { "tier" } else { "executor_tier" };
            join.get(key).and_then(Value::as_str).map(str::to_string)
        }
        _ => None,
    })
}

/// What the node's first attempt runs on.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Plan {
    /// The profile's own chain.
    Executor,
    /// Enforced: this overlay entry (the routed chain) for the execution,
    /// with the reason for `profile_rebound`.
    Routed {
        entry: Box<ManifestProfile>,
        reason: String,
    },
}

/// The node, as the execution start knows it.
pub(crate) struct Step<'a> {
    pub(crate) node: &'a str,
    pub(crate) title: Option<&'a str>,
    pub(crate) prompt: &'a str,
    pub(crate) outputs: &'a [String],
    /// The node's binding from the manifest (not an overlay).
    pub(crate) entry: &'a ManifestProfile,
    pub(crate) previous_tier: Option<String>,
}

/// The effective tier: the answer, smoothed toward `previous`.
fn effective_tier(
    answers: &BTreeMap<String, DecisionAnswer>,
    entry: &ManifestProfile,
    previous: Option<&str>,
    hysteresis: f64,
) -> Option<(String, String, f64)> {
    let a = answers.get("tier")?;
    let rec = a.value.as_ref()?.as_str()?.to_string();
    let conf = a.confidence.unwrap_or(0.0);
    let (exec, _) = executor_tier(entry);
    let known = entry.tiers.iter().any(|t| t.name == rec);
    let effective = match previous {
        _ if !known => previous.map(str::to_string).unwrap_or(exec),
        Some(prev) if prev != rec && conf < hysteresis => prev.to_string(),
        _ => rec.clone(),
    };
    Some((rec, effective, conf))
}

/// The routed chain for `tier`: the tier's executor, then (for a tier below
/// the profile's own executor) every tier above it up to the executor, then
/// the profile's own chain.
pub(crate) fn routed_entry(entry: &ManifestProfile, tier: &str) -> Option<ManifestProfile> {
    let idx = entry.tiers.iter().position(|t| t.name == tier)?;
    let first = entry.tiers[idx].invocation.clone()?;
    let (_, exec_idx) = executor_tier(entry);
    let mut chain = vec![first];
    let mut cascade = 0u32;
    if idx < exec_idx {
        cascade = 1;
        for t in &entry.tiers[idx + 1..exec_idx.min(entry.tiers.len())] {
            if let Some(inv) = &t.invocation {
                chain.push(inv.clone());
                cascade += 1;
            }
        }
    }
    chain.extend(entry.chain.iter().cloned());
    let mut routed = entry.clone();
    routed.chain = chain;
    routed.routed_tier = Some(tier.to_string());
    routed.cascade = cascade;
    Some(routed)
}

/// Asks once for this execution. Fail-open: anything but an enforced answer
/// runs the profile's own executor.
pub(crate) fn route(runner: &DecisionRunner, journal: &dyn DecisionJournal, s: Step) -> Plan {
    if runner.mode_for(UseSite::Routing) == DecisionMode::Off || s.entry.tiers.is_empty() {
        return Plan::Executor;
    }
    let hysteresis = runner.threshold_or(UseSite::Routing, "hysteresis", 0.75);
    let (exec, _) = executor_tier(s.entry);
    let step = match s.title {
        Some(t) if !t.trim().is_empty() => format!("{t}\n\n{}", s.prompt),
        _ => s.prompt.to_string(),
    };
    let mut meta = serde_json::Map::new();
    meta.insert("declared_outputs".into(), json!(s.outputs));
    let previous = s.previous_tier.clone();
    let entry = s.entry;
    let judge = |answers: &BTreeMap<String, DecisionAnswer>| Judgement {
        applied: false,
        would_change: effective_tier(answers, entry, previous.as_deref(), hysteresis)
            .map(|(_, eff, _)| eff != exec),
    };
    let acts = |answers: &BTreeMap<String, DecisionAnswer>, threshold: f64| {
        matches!(effective_tier(answers, entry, previous.as_deref(), hysteresis),
            Some((_, eff, conf)) if eff != exec && conf >= threshold && routed_entry(entry, &eff).is_some())
    };
    let join_from = |answers: &BTreeMap<String, DecisionAnswer>| {
        let mut extra = BTreeMap::new();
        if let Some((rec, eff, _)) = effective_tier(answers, entry, previous.as_deref(), hysteresis)
        {
            extra.insert("recommended".to_string(), Value::from(rec));
            extra.insert("tier".to_string(), Value::from(eff));
        }
        extra
    };
    let join = BTreeMap::from([
        ("profile".to_string(), Value::from(entry.key())),
        ("executor_tier".to_string(), Value::from(exec.clone())),
    ]);
    let outcome = runner.decide(
        journal,
        DecisionCall {
            site: UseSite::Routing,
            node: Some(s.node),
            attempt: Some(1),
            state: StateParts {
                fields: vec![StateField {
                    name: "step",
                    class: FieldClass::Prompt,
                    text: step,
                    head: PROMPT_HEAD,
                    tail: 0,
                }],
                meta,
            },
            questions: questions(entry),
            baseline: None,
            judge: &judge,
            join,
            join_from: Some(&join_from),
            enforce: Some(Enforce {
                opted_in: true,
                refused: None,
                acts: &acts,
            }),
        },
    );
    let DecisionOutcome::Answered { answers, meta, .. } = outcome else {
        return Plan::Executor;
    };
    if !meta.applied {
        return Plan::Executor;
    }
    let Some((_, eff, conf)) = effective_tier(&answers, entry, previous.as_deref(), hysteresis)
    else {
        return Plan::Executor;
    };
    match routed_entry(entry, &eff) {
        Some(routed) => Plan::Routed {
            entry: Box::new(routed),
            reason: format!(
                "routing: tier `{eff}` (confidence {conf:.2}, {}/{})",
                meta.provider.as_deref().unwrap_or("-"),
                meta.model.as_deref().unwrap_or("-")
            ),
        },
        None => Plan::Executor,
    }
}
