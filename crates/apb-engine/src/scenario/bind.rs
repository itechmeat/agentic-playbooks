//! The scenario drive as the store plays it: [`drive`] runs one scenario on
//! the engine's own drive ([`crate::scheduler::run_scenario`]) and hands
//! back what its answer book and session saw, in the report's terms
//! ([`DriveOutcome`]).
//!
//! The book names a scripted answer by its place in the scenario file
//! (node, visit entry, part); the report names it by boundary and key.
//! Answers used by number and answers nobody used translate from the book;
//! answers a `visit: default` entry gave are read from the session's
//! `scenario_answer` records, which carry the visit they answered.

use std::collections::BTreeMap;
use std::path::PathBuf;

use apb_core::scenario::{LoadedScenario, ScenarioLevel, Visit};
use apb_core::schema::{NodeKind, Playbook};
use serde_json::Value;

use super::book::{self, MissingAnswer};
use super::report::{AnswerRef, AnswerUse, BoundaryUse, DriveOutcome};
use super::session::{self, ScenarioPrep};
use super::{BoundaryKey, BoundaryKind};
use crate::event::EventPayload;

/// What the scenario drive is asked to play.
#[derive(Debug, Clone)]
pub struct DriveRequest {
    /// The project root (absolute): the run is written under its
    /// `.apb/scenario-runs/`.
    pub root: PathBuf,
    /// Where the playbook is read: the project's registry or the global
    /// store.
    pub scope: apb_core::scope::Scope,
    pub playbook_id: String,
    pub version: String,
    /// The version's trust digest.
    pub playbook_digest: String,
    /// The version's definition.
    pub playbook: Playbook,
    pub scenario: LoadedScenario,
    pub level: ScenarioLevel,
    /// The smoke cap on decision requests; `None` at quick.
    pub max_decision_requests: Option<u32>,
    /// The scenario run processes external data ([`crate::external_data`]).
    pub external_data: bool,
}

/// The scenario drive: prepares a run under `.apb/scenario-runs/` with
/// `origin: scenario`, drives it with every boundary answered from the
/// scenario, and says what its answer book and session saw. `Err` when it
/// could not drive (nothing to report but the reason).
pub type Drive = fn(&DriveRequest) -> Result<DriveOutcome, String>;

/// Plays `req` on the engine's drive. `Err` only when the run could not be
/// prepared or driven at all.
pub fn drive(req: &DriveRequest) -> Result<DriveOutcome, String> {
    let prep = ScenarioPrep {
        scenario: req.scenario.scenario.clone(),
        level: req.level,
        scenario_digest: req.scenario.digest.clone(),
        max_requests: req
            .max_decision_requests
            .unwrap_or(session::SMOKE_DEFAULT_REQUESTS),
        sent: Default::default(),
        external_data: req.external_data,
    };
    let before = apb_core::process_mode::scenario_refusals().len();
    let run = crate::scheduler::run_scenario_of(
        &req.root,
        req.scope,
        &req.playbook_id,
        Some(&req.version),
        &prep,
    )
    .map_err(|e| e.to_string())?;
    let refused: Vec<String> = apb_core::process_mode::scenario_refusals()
        .into_iter()
        .skip(before)
        .collect();
    let events = crate::event::read_all_lossy_tail(&run.run_dir).unwrap_or_default();
    let (decision_requests, cost_usd) = spent(&events);
    let answers = answer_use(&req.playbook, &run.answers, &run.run_dir);
    Ok(DriveOutcome {
        boundaries: boundary_use(req.level, &run.boundaries, &answers.missing),
        invalid: invalid_of(run.halted.as_ref(), &refused),
        run_id: run.run_id,
        run_dir: run.run_dir,
        answers,
        decision_requests,
        cost_usd,
    })
}

/// Why a drive was not played as written: the missing answer that stopped
/// it, else what a guard of the scenario process refused while it ran (a
/// site acted instead of taking its answer, even where its caller went on).
fn invalid_of(halted: Option<&MissingAnswer>, refused: &[String]) -> Option<String> {
    match (halted, refused) {
        (Some(m), _) => Some(m.describe()),
        (None, []) => None,
        (None, ops) => Some(format!(
            "{}: a site acted instead of taking its answer and was refused ({})",
            apb_core::process_mode::SCENARIO_REFUSAL,
            ops.join(", ")
        )),
    }
}

/// The decision requests the run sent and what they cost: every
/// `decision_made` a provider of this machine answered or failed (not the
/// scenario's own scripted provider, not the run cache, not one the cap
/// stopped before it left).
fn spent(events: &[crate::event::Event]) -> (u32, Option<f64>) {
    let mut requests = 0u32;
    let mut cost: Option<f64> = None;
    for e in events {
        if let EventPayload::DecisionMade {
            provider,
            cached,
            error,
            cost_usd,
            ..
        } = &e.payload
        {
            let scripted = provider.as_deref() == Some(session::SCENARIO_AGENT);
            let held = matches!(error.as_deref(), Some("budget") | Some("cancelled"));
            if scripted || *cached || held || provider.is_none() {
                continue;
            }
            requests += 1;
            if let Some(c) = cost_usd {
                *cost.get_or_insert(0.0) += c;
            }
        }
    }
    (requests, cost)
}

/// The book's answer use in the report's terms.
fn answer_use(playbook: &Playbook, used: &book::AnswerUse, run_dir: &std::path::Path) -> AnswerUse {
    AnswerUse {
        used: used
            .used
            .iter()
            .filter_map(|r| translate(playbook, r))
            .collect(),
        defaults: defaults_used(run_dir),
        missing: used.missing.iter().filter_map(missing_ref).collect(),
        extra: used
            .extra
            .iter()
            .filter_map(|r| translate(playbook, r))
            .collect(),
    }
}

/// One scripted answer of the file as a boundary and key. A `default`
/// visit entry keeps visit 0: it answers whichever visit asks.
fn translate(playbook: &Playbook, r: &book::AnswerRef) -> Option<AnswerRef> {
    let visit = match r.visit {
        Some(Visit::Nth(n)) => n,
        _ => 0,
    };
    let node = r.node.clone();
    let node_kind = node
        .as_deref()
        .and_then(|n| playbook.node(n))
        .map(|n| &n.kind);
    let attempt_kind = match node_kind {
        Some(NodeKind::Script { .. }) => BoundaryKind::Script,
        _ => BoundaryKind::AgentAttempt,
    };
    let judge_kind = match node_kind {
        Some(NodeKind::Judge { .. }) => BoundaryKind::JudgeNative,
        _ => BoundaryKind::ListJudgeCheck,
    };
    let mut key = BoundaryKey {
        node,
        visit,
        ..BoundaryKey::default()
    };
    let words: Vec<&str> = r.part.splitn(4, ' ').collect();
    let kind = match words.as_slice() {
        ["attempt", k] => {
            key.attempt = Some(k.parse().ok()?);
            attempt_kind
        }
        ["item", id, "attempt", k] => {
            key.item = Some((*id).to_string());
            key.attempt = Some(k.parse().ok()?);
            attempt_kind
        }
        ["edge", address] => {
            key.part = Some((*address).to_string());
            BoundaryKind::JudgeEdges
        }
        ["goal", i] => {
            key.part = Some((*i).to_string());
            BoundaryKind::GoalScript
        }
        ["script"] => BoundaryKind::Script,
        ["review"] => BoundaryKind::Review,
        ["judge"] => judge_kind,
        ["source"] => BoundaryKind::ListSource,
        ["playbook"] => BoundaryKind::ChildPlaybook,
        ["wait"] => BoundaryKind::Wait,
        ["publish"] => BoundaryKind::Publish,
        ["baseline"] => BoundaryKind::Baseline,
        ["question"] => BoundaryKind::Question,
        ["check"] => BoundaryKind::SuccessCheck,
        _ => return None,
    };
    Some(AnswerRef { kind, key })
}

fn missing_ref(m: &MissingAnswer) -> Option<AnswerRef> {
    Some(AnswerRef {
        kind: kind_named(&m.boundary)?,
        key: BoundaryKey {
            node: m.node.clone(),
            visit: m.visit,
            attempt: m.attempt,
            item: m.item.clone(),
            part: m.part.clone(),
        },
    })
}

fn kind_named(name: &str) -> Option<BoundaryKind> {
    serde_json::from_value(Value::String(name.to_string())).ok()
}

/// The uses of `visit: default` entries, each with the key of the
/// crossing it answered, from the session's `scenario_answer` records.
fn defaults_used(run_dir: &std::path::Path) -> Vec<AnswerRef> {
    crate::diagnostics::index::read_raw(run_dir)
        .into_iter()
        .filter(|v| {
            v.get("kind").and_then(Value::as_str) == Some(session::ANSWER_RECORD)
                && v.get("use").and_then(Value::as_str) == Some("used")
                && v.pointer("/from/visit").and_then(Value::as_str) == Some("default")
        })
        .filter_map(|v| {
            Some(AnswerRef {
                kind: kind_named(v.get("boundary")?.as_str()?)?,
                key: serde_json::from_value(v.get("key")?.clone()).ok()?,
            })
        })
        .collect()
}

/// What each boundary crossing did. A crossing with no answer is
/// `missing`; the kinds the session lets pass without acting are
/// `skipped`; a judge at smoke asks this machine's providers (`real`);
/// every other crossing was answered from the scenario.
fn boundary_use(
    level: ScenarioLevel,
    counts: &[session::BoundaryUse],
    missing: &[AnswerRef],
) -> Vec<BoundaryUse> {
    let mut missed: BTreeMap<BoundaryKind, u32> = BTreeMap::new();
    for m in missing {
        *missed.entry(m.kind).or_default() += 1;
    }
    counts
        .iter()
        .map(|c| {
            let missing = missed.get(&c.kind).copied().unwrap_or(0).min(c.count);
            let rest = c.count - missing;
            let mut u = BoundaryUse {
                kind: c.kind,
                answered: 0,
                skipped: 0,
                missing,
                real: 0,
            };
            match crossing(level, c.kind) {
                Crossing::Skipped => u.skipped = rest,
                Crossing::Real => u.real = rest,
                Crossing::Answered => u.answered = rest,
            }
            u
        })
        .collect()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Crossing {
    Answered,
    Skipped,
    Real,
}

/// How the session resolves a crossing of `kind` at `level`, short of a
/// missing answer (the rule of `ScenarioSession::resolve`, held to it by
/// a test).
fn crossing(level: ScenarioLevel, kind: BoundaryKind) -> Crossing {
    use BoundaryKind as K;
    let smoke = level == ScenarioLevel::Smoke;
    match kind {
        K::Protect
        | K::Worktree
        | K::Provenance
        | K::Notify
        | K::NodeCache
        | K::FinishHooks
        | K::RetryDelay
        | K::InfraBackoff
        | K::EngineAid
        | K::ReadOnlyCheck => Crossing::Skipped,
        K::JudgeEmulation if !smoke => Crossing::Skipped,
        K::JudgeEmulation | K::JudgeNative | K::JudgeEdges | K::ListJudgeCheck if smoke => {
            Crossing::Real
        }
        _ => Crossing::Answered,
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::scenario::{Resolution, Session};

    const PLAYBOOK: &str = "schema: 2\nid: p\nname: p\nversion: 1.0.0\nnodes:\n  - { id: start, type: start }\n  - { id: done, type: finish, outcome: success }\nedges:\n  - { from: start, to: done }\n";

    /// The crossing rule here and the session's own resolution agree for
    /// every kind at both levels: a session with an empty scenario answers
    /// `Skip` exactly where this says skipped, `Real` where it says real,
    /// and an answer or a missing one everywhere else.
    #[test]
    fn the_crossing_rule_is_the_sessions() {
        let dir = tempfile::tempdir().unwrap();
        let playbook = Playbook::from_yaml(PLAYBOOK).unwrap();
        for level in [ScenarioLevel::Quick, ScenarioLevel::Smoke] {
            let prep = ScenarioPrep {
                scenario: yaml_serde::from_str("schema: 1\nid: s\n").unwrap(),
                level,
                scenario_digest: "sha256:x".into(),
                max_requests: 1,
                sent: Default::default(),
                external_data: false,
            };
            let s = Arc::new(session::ScenarioSession::new(&prep, &playbook, dir.path()));
            for kind in BoundaryKind::ALL {
                let got = match s.resolve(kind, &BoundaryKey::default()) {
                    Resolution::Skip => Crossing::Skipped,
                    Resolution::Real => Crossing::Real,
                    _ => Crossing::Answered,
                };
                assert_eq!(got, crossing(level, kind), "{kind:?} at {level:?}");
            }
        }
    }

    /// Every part the book names translates to the boundary that hands it
    /// out, with its attempt, item, edge address or goal index in the key.
    #[test]
    fn book_parts_translate_to_boundaries_and_keys() {
        let playbook = Playbook::from_yaml(
            "schema: 2\nid: p\nname: p\nversion: 1.0.0\nnodes:\n  - { id: start, type: start }\n  - { id: s, type: script, script: x, runner: sh }\n  - { id: done, type: finish, outcome: success }\nedges:\n  - { from: start, to: s }\n  - { from: s, to: done }\n",
        )
        .unwrap();
        let r = |node: &str, visit: Option<Visit>, part: &str| {
            translate(
                &playbook,
                &book::AnswerRef {
                    node: Some(node.into()),
                    visit,
                    part: part.into(),
                },
            )
            .map(|a| a.describe())
        };
        let two = Some(Visit::Nth(2));
        assert_eq!(
            r("w", two, "attempt 3").as_deref(),
            Some("w visit 2 attempt 3 (agent_attempt)")
        );
        assert_eq!(
            r("w", two, "item a.md attempt 1").as_deref(),
            Some("w visit 2 attempt 1 item a.md (agent_attempt)")
        );
        assert_eq!(
            r("s", two, "item a attempt 1").as_deref(),
            Some("s visit 2 attempt 1 item a (script)")
        );
        assert_eq!(
            r("j", Some(Visit::Default), "edge id:x").as_deref(),
            Some("j visit 0 id:x (judge_edges)")
        );
        assert_eq!(r("g", two, "review").as_deref(), Some("g visit 2 (review)"));
        assert_eq!(r("g", two, "nonsense"), None);
    }
}
