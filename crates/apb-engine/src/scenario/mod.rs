//! Scenario runs: the ordinary drive with every answer that comes from
//! outside the engine scripted by a scenario file
//! ([`apb_core::scenario`]).
//!
//! The drive is not forked. Each point where a real run would act outside
//! apb (start an agent, run a script, wait for a person, ask a decision
//! provider, sleep) is one [`BoundaryKind`], and its site asks [`at`]
//! before it acts. Without a scenario session the answer is
//! [`Resolution::Real`] and the site acts exactly as before; in a scenario
//! run the session answers from the scenario's answer book.
//!
//! Scenario runs live under `.apb/scenario-runs/` ([`RunsDir::Scenario`]),
//! never under `.apb/runs/`, so no run surface and no history reader of
//! any released apb finds them.

pub mod bind;
pub mod capture;
pub mod guard;
pub mod report;
pub mod store;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

pub(crate) mod adapter;
pub mod book;
pub mod decide;
pub mod session;

use apb_core::scenario::{
    AttemptAnswer, BaselineAnswer, CheckAnswer, ChildAnswer, GoalAnswer, JudgeAnswer,
    PublishAnswer, QuestionAnswer, QuestionScript, ReviewAnswer, ScriptAnswer, WaitAnswer,
};
use serde::{Deserialize, Serialize};

/// Defines [`BoundaryKind`], its [`BoundaryKind::ALL`] and its names from
/// one list, so no kind can be left out of `ALL` or go without a name.
macro_rules! boundary_kinds {
    ($($(#[$doc:meta])* $kind:ident => $name:literal,)*) => {
        /// Every point where a run reaches outside the engine. Closed: a site
        /// added to the drive gets a variant here, and every variant has a site.
        #[cfg_attr(feature = "ts", derive(ts_rs::TS))]
        #[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
        #[serde(rename_all = "snake_case")]
        pub enum BoundaryKind {
            $($(#[$doc])* $kind,)*
        }

        impl BoundaryKind {
            /// Every kind, in the order of the drive.
            pub const ALL: [BoundaryKind; [$($name),*].len()] = [$(BoundaryKind::$kind),*];

            /// The snake_case name, as the report and the diagnostics write it.
            pub fn as_str(self) -> &'static str {
                match self {
                    $(BoundaryKind::$kind => $name,)*
                }
            }
        }
    };
}

boundary_kinds! {
    /// An agent attempt: an agent task, a finish with a prompt, a judge's
    /// profile emulation.
    AgentAttempt => "agent_attempt",
    /// An interactive question inside an attempt, its timeout and default.
    Question => "question",
    /// A node's `success_check`.
    SuccessCheck => "success_check",
    /// The completion check of an attempt.
    CompletionCheck => "completion_check",
    /// A script node.
    Script => "script",
    /// A judge node's own question.
    JudgeNative => "judge_native",
    /// A judge's `llm_emulation` providers.
    JudgeEmulation => "judge_emulation",
    /// The judge edges of a node.
    JudgeEdges => "judge_edges",
    /// A list node's judge item check.
    ListJudgeCheck => "list_judge_check",
    /// The engine's own decision uses (completion check model, retry
    /// advice, supervisor triage, review recommendation, tier routing).
    EngineAid => "engine_aid",
    /// A list's items read from a connector or a script.
    ListSource => "list_source",
    /// A `human_review` gate.
    Review => "review",
    /// A `publish` node: the consent and the push.
    Publish => "publish",
    /// A wait node (timer, webhook, inbox).
    Wait => "wait",
    /// A child playbook node.
    ChildPlaybook => "child_playbook",
    /// The baseline check, before and after.
    Baseline => "baseline",
    /// A script goal criterion.
    GoalScript => "goal_script",
    /// The protected-path snapshot and restore.
    Protect => "protect",
    /// The run's working tree and the busy lock.
    Worktree => "worktree",
    /// The commits a node made (`HEAD` reads).
    Provenance => "provenance",
    /// "Notify me": the subscription read and every send.
    Notify => "notify",
    /// The node cache and the judge cache.
    NodeCache => "node_cache",
    /// A deferred retry's delay (`retry_delay`).
    RetryDelay => "retry_delay",
    /// The infrastructure backoff between attempts.
    InfraBackoff => "infra_backoff",
    /// The tree states a `read_only` node is checked between.
    ReadOnlyCheck => "read_only_check",
    /// What runs after a run ends: the grade, the candidate settlement,
    /// known problems.
    FinishHooks => "finish_hooks",
}

/// Which crossing of a boundary is meant: the answer book's key.
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(optional_fields))]
#[derive(Debug, Clone, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct BoundaryKey {
    /// The node, absent for a run-level boundary (baseline, goal,
    /// worktree, notify, finish hooks).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub node: Option<String>,
    /// The node execution ordinal, 1-based (`identity::index`); 0 for a
    /// run-level boundary.
    #[serde(default)]
    pub visit: u32,
    /// The k-th attempt inside the execution (or the item), 1-based,
    /// counted by the answer book, never the journal's display ordinal.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attempt: Option<u32>,
    /// A list item id.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub item: Option<String>,
    /// The part of a boundary that has several: `before` or `after` of the
    /// baseline, a goal criterion's index.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub part: Option<String>,
}

/// A scripted answer, in the scenario file's own types.
#[derive(Debug, Clone, PartialEq)]
pub enum Answer {
    Attempt(AttemptAnswer),
    Question(QuestionScript),
    Check(CheckAnswer),
    Script(ScriptAnswer),
    Judge(JudgeAnswer),
    JudgeEdges(BTreeMap<String, QuestionAnswer>),
    Source(Vec<serde_json::Value>),
    Review(ReviewAnswer),
    Publish(PublishAnswer),
    Wait(WaitAnswer),
    Child(ChildAnswer),
    Baseline(BaselineAnswer),
    Goal(GoalAnswer),
}

/// What a boundary site does.
#[derive(Debug, Clone, PartialEq)]
pub enum Resolution {
    /// Not a scenario run: act as a real run does.
    Real,
    /// A scenario run does not cross this boundary: skip the act (no
    /// snapshot, no lock, no send, no sleep: a delay is due at once).
    Skip,
    /// The scenario's answer for the key.
    Answer(Answer),
    /// The scenario has no answer for the key: the site applies the
    /// scenario's `on_missing`.
    Missing,
}

/// The scenario session of a run: answers boundaries from the answer book
/// and records each use. Held by the run's configuration; a real run has
/// none.
pub trait Session: Send + Sync {
    fn resolve(&self, kind: BoundaryKind, key: &BoundaryKey) -> Resolution;
}

/// The one question every boundary site asks before it acts.
pub fn at(session: Option<&dyn Session>, kind: BoundaryKind, key: &BoundaryKey) -> Resolution {
    match session {
        None => Resolution::Real,
        Some(s) => s.resolve(kind, key),
    }
}

/// Where a run directory is allocated.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunsDir {
    /// `.apb/runs/`: every run that counts.
    Production,
    /// `.apb/scenario-runs/`: scenario runs, which no run surface reads.
    Scenario,
}

impl RunsDir {
    /// The directory's name under `.apb/`.
    pub fn name(self) -> &'static str {
        match self {
            RunsDir::Production => "runs",
            RunsDir::Scenario => "scenario-runs",
        }
    }

    /// The directory under the project root `root`.
    pub fn under(self, root: &Path) -> PathBuf {
        root.join(".apb").join(self.name())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The report, the diagnostics and the generated constant list name a
    /// boundary by its serde name: `as_str` must agree with it for every
    /// kind, and no two kinds may share one.
    #[test]
    fn every_boundary_kind_has_one_name_everywhere() {
        let mut seen = std::collections::BTreeSet::new();
        for k in BoundaryKind::ALL {
            assert_eq!(
                serde_json::to_value(k).unwrap(),
                serde_json::Value::from(k.as_str())
            );
            assert!(seen.insert(k.as_str()), "{} twice", k.as_str());
        }
    }
}
