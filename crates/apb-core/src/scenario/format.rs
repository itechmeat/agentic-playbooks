//! The scenario file format: `.apb/playbooks/<id>/evals/scenarios/<name>.yaml`.
//!
//! A scenario scripts every answer that comes from outside the engine (an
//! agent attempt, a script, a review, a judge, a wait, a child playbook) so
//! a playbook can be driven by the real engine without starting a program,
//! sending a request or touching the project tree. These are the types
//! only: loading, digests and the V-scenario rules live next to them.
//!
//! Every struct refuses unknown fields, like the eval case format
//! ([`crate::eval`]), so a typo is an error and never a silently ignored
//! answer. Which answer kind fits which node kind is a validation rule, not
//! a parse rule: one [`VisitAnswer`] carries every kind as an optional field.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::decisions::DecisionMode;
use crate::eval::{OutputCheck, RunCheck};

/// The only `schema` a scenario file may name today.
pub const SCENARIO_SCHEMA: u32 = 1;

/// How far a scenario run reaches outside apb. `deep` is `apb eval` itself
/// and has no scenario run.
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ScenarioLevel {
    /// Everything scripted: no program, no request, no cost.
    Quick,
    /// [`ScenarioLevel::Quick`] plus this machine's decision providers,
    /// capped.
    Smoke,
}

impl ScenarioLevel {
    pub fn as_str(self) -> &'static str {
        match self {
            ScenarioLevel::Quick => "quick",
            ScenarioLevel::Smoke => "smoke",
        }
    }
}

/// One scenario file.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Scenario {
    pub schema: u32,
    pub id: String,
    #[serde(default)]
    pub title: Option<String>,
    /// Optional semver range of the versions the scenario applies to, in
    /// the eval case syntax (`">=1.4.0, <2.0.0"`).
    #[serde(default)]
    pub versions: Option<String>,
    /// Optional pin: a version with another digest makes the scenario
    /// stale.
    #[serde(default)]
    pub playbook_digest: Option<String>,
    #[serde(default)]
    pub instruction: Option<String>,
    /// Run params. A param the playbook declares `secret` may not be set.
    #[serde(default)]
    pub params: BTreeMap<String, String>,
    /// Run the scenario as an unattended run.
    #[serde(default)]
    pub unattended: bool,
    #[serde(default)]
    pub on_missing: OnMissing,
    #[serde(default)]
    pub on_extra: OnExtra,
    /// The judge modes of a quick run (default `enforce` for both).
    #[serde(default)]
    pub decisions: ScenarioDecisions,
    /// Answers per node id, one entry per visit.
    #[serde(default)]
    pub answers: BTreeMap<String, Vec<VisitAnswer>>,
    #[serde(default)]
    pub baseline: Option<BaselineAnswer>,
    /// Script goal criteria by their index in the playbook's `goal:` list
    /// (`"0"`, `"1"`); marker and list criteria are evaluated for real.
    #[serde(default)]
    pub goal: BTreeMap<String, GoalAnswer>,
    #[serde(default)]
    pub expect: Expect,
}

/// What a boundary with no answer does.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OnMissing {
    /// Stop the scenario at once; the verdict is `invalid`.
    #[default]
    Error,
    /// Fail the boundary the way the real one fails, and go on.
    Fail,
}

/// What answers nobody used do to the verdict.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OnExtra {
    /// The verdict is `failed` with `unused_answers`.
    #[default]
    Error,
    /// Listed in the report only.
    Warn,
}

/// The modes of the judge uses in a quick run. Absent means `enforce`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ScenarioDecisions {
    #[serde(default)]
    pub judge_node: Option<DecisionMode>,
    #[serde(default)]
    pub judge_edge: Option<DecisionMode>,
}

/// Which execution of a node an answer is for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Visit {
    /// The n-th execution, 1-based.
    Nth(u32),
    /// Any execution with no numbered entry; never reported as extra.
    Default,
}

impl Serialize for Visit {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        match self {
            Visit::Nth(n) => s.serialize_u32(*n),
            Visit::Default => s.serialize_str("default"),
        }
    }
}

impl<'de> Deserialize<'de> for Visit {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Raw {
            Nth(u32),
            Word(String),
        }
        match Raw::deserialize(d)? {
            Raw::Nth(n) => Ok(Visit::Nth(n)),
            Raw::Word(w) if w == "default" => Ok(Visit::Default),
            Raw::Word(w) => Err(serde::de::Error::custom(format!(
                "visit `{w}` is neither a number nor `default`"
            ))),
        }
    }
}

/// The answers for one visit of one node. Exactly one kind fits the node
/// (a validation rule): `attempts` for an agent task or a finish with a
/// prompt, `script`, `review`, `judge`, `judge_edges` (any node with judge
/// edges, besides its own kind), `items` and `source` for a list node,
/// `playbook`, `wait`, `publish`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VisitAnswer {
    pub visit: Visit,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub attempts: Vec<AttemptAnswer>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub script: Option<ScriptAnswer>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub review: Option<ReviewAnswer>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub judge: Option<JudgeAnswer>,
    /// Judge edge answers keyed by edge address (`id:<id>` or
    /// `e:<from>><to>#<n>`).
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub judge_edges: BTreeMap<String, QuestionAnswer>,
    /// List item answers keyed by item id; `"*"` answers every item not
    /// listed.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub items: BTreeMap<String, ItemAnswers>,
    /// The items of a list that reads an external source.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<Vec<serde_json::Value>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub playbook: Option<ChildAnswer>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wait: Option<WaitAnswer>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub publish: Option<PublishAnswer>,
}

/// The status of a scripted attempt or step.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ScriptedStatus {
    Succeeded,
    Failed,
}

/// The failure class of a scripted failed attempt, in the journal's words
/// (`attempt_finished.failure_kind`); `infrastructure` reads as
/// `transient`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ScriptedFailure {
    #[serde(alias = "infrastructure")]
    Transient,
    Quota,
    Auth,
    Budget,
    Agent,
}

/// One agent attempt: what the agent leaves behind, and the success or
/// completion check's verdict on it.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AttemptAnswer {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<ScriptedStatus>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failure_kind: Option<ScriptedFailure>,
    /// The output text.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output: Option<String>,
    /// The declared output fields, as a status file would carry them.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub outputs: BTreeMap<String, serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub check: Option<CheckAnswer>,
    /// An interactive question the attempt asks; `then` is the attempt
    /// after the answer, in the same attempt.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub question: Option<QuestionScript>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub then: Option<Box<AttemptAnswer>>,
    /// What the read-only check of a `read_only` node finds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub read_only: Option<ReadOnlyAnswer>,
}

/// The verdict of a node's `success_check` or completion check.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CheckAnswer {
    Pass,
    Reject,
}

/// An interactive question inside an attempt: answered with `answer`, or
/// left to time out.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct QuestionScript {
    pub text: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub answer: Option<String>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub timeout: bool,
}

/// The paths a read-only check finds changed (empty: clean).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReadOnlyAnswer {
    #[serde(default)]
    pub dirty: Vec<String>,
}

/// A script node's run.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ScriptAnswer {
    pub exit: i32,
    #[serde(default)]
    pub stdout: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub stderr: String,
}

/// A person's review decision.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReviewAnswer {
    pub decision: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

/// Why a scripted judge question has no answer, in the decision client's
/// error kinds.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UnavailableKind {
    Unavailable,
    RateLimited,
    Invalid,
    Auth,
    Budget,
    Timeout,
}

/// A judge node's answers: one per question, or the whole ask unavailable.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum JudgeAnswer {
    Unavailable { unavailable: UnavailableKind },
    Answers(BTreeMap<String, QuestionAnswer>),
}

/// One question's answer, as a provider would give it: the chosen option
/// (choice) or level (score) in `value`, `p` its probability (the yes
/// probability of a yes or no question).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct QuestionAnswer {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub value: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub p: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub confidence: Option<f64>,
}

/// A list item's attempts: a list of attempts, or one attempt.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ItemAnswers {
    Attempts(Vec<AttemptAnswer>),
    One(AttemptAnswer),
}

/// A child playbook node: a nested scenario run of the child at the same
/// level, or the child's result as written.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ChildAnswer {
    Scenario {
        scenario: String,
    },
    Result {
        status: ScriptedStatus,
        #[serde(default)]
        output: Option<serde_json::Value>,
    },
}

/// A wait node: the timer elapses, a signal arrives, or the wait times out
/// (exactly one, a validation rule). Due at once: nothing sleeps.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WaitAnswer {
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub elapsed: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signal: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub timed_out: bool,
}

/// A publish node: the person's consent and the push's result. Nothing is
/// pushed.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PublishAnswer {
    pub decision: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<ScriptedStatus>,
}

/// The baseline check's observations, or `skip` (every source unknown).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum BaselineAnswer {
    Skip(SkipWord),
    Observed {
        #[serde(default)]
        before: BTreeMap<String, serde_json::Value>,
        #[serde(default)]
        after: BTreeMap<String, serde_json::Value>,
    },
}

/// The one word `skip`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SkipWord {
    Skip,
}

/// A script goal criterion's result.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GoalAnswer {
    Pass,
    Fail,
}

/// What a scenario run must show. `run` and `outputs` are the eval case's
/// checks; `visits` is an exact count per node, `route.path` the
/// exact sequence of node executions in settlement order.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Expect {
    #[serde(default)]
    pub run: Option<RunCheck>,
    /// The finish node the run ends at.
    #[serde(default)]
    pub finish: Option<String>,
    #[serde(default)]
    pub route: Option<ScenarioRoute>,
    #[serde(default)]
    pub visits: BTreeMap<String, u32>,
    #[serde(default)]
    pub edges: EdgeExpect,
    #[serde(default)]
    pub joins: JoinExpect,
    #[serde(default)]
    pub outputs: Vec<OutputCheck>,
    /// Run outputs by name.
    #[serde(default)]
    pub run_outputs: BTreeMap<String, RunOutputExpect>,
    #[serde(default)]
    pub templates_empty: TemplatesEmpty,
}

/// The route a scenario run must take: the eval case's route words
/// (`visits` in order or not, `not_visits`) plus `path`, the exact
/// sequence of node executions in settlement order.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ScenarioRoute {
    #[serde(default)]
    pub path: Vec<String>,
    #[serde(default)]
    pub visits: Vec<String>,
    #[serde(default)]
    pub in_order: bool,
    #[serde(default)]
    pub not_visits: Vec<String>,
}

/// Edges by address: how often each was taken, and which never were.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EdgeExpect {
    #[serde(default)]
    pub taken: BTreeMap<String, u32>,
    #[serde(default)]
    pub not_taken: Vec<String>,
}

/// Join inputs expected dead, per join node.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct JoinExpect {
    #[serde(default)]
    pub dead: BTreeMap<String, Vec<String>>,
}

/// One run output's expected state.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RunOutputExpect {
    pub state: String,
}

/// What a template reference that rendered empty does to the verdict.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TemplatesEmpty {
    #[default]
    Error,
    Report,
    Ignore,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The worked example of the format: every answer kind, both visit
    /// forms and every expectation, in one file.
    const EXAMPLE: &str = r#"
schema: 1
id: green-after-one-qa-fix
title: QA fails once, the fix loop repairs it
versions: ">=1.4.0"
playbook_digest: sha256:abc
instruction: "Prepare the PR for branch feat/x"
params: { kind: feature }
unattended: false
on_missing: fail
on_extra: warn
decisions: { judge_node: enforce, judge_edge: shadow }
answers:
  intake:
    - visit: 1
      attempts:
        - { status: succeeded, output: "branch feat/x", check: pass }
  qa_gates:
    - { visit: 1, script: { exit: 1, stdout: "QA-GATE: red" } }
  qa_fix:
    - visit: 1
      attempts:
        - { status: failed, failure_kind: infrastructure, output: "spawn failed" }
        - { status: succeeded, read_only: { dirty: ["src/a.rs"] } }
  bot_review:
    - { visit: default, attempts: [ { status: succeeded, outputs: { pushed: "no", findings: 0 } } ] }
  push_approval:
    - { visit: 1, review: { decision: approve, note: "ok to push" } }
  check_task:
    - { visit: 1, judge: { sufficient: { value: clear, p: 0.9, confidence: 0.8 }, risky: { p: 0.1 } } }
    - { visit: 2, judge: { unavailable: timeout } }
  review:
    - { visit: 1, judge_edges: { "id:needs-fix": { p: 0.8 } } }
  files:
    - visit: 1
      items:
        "a.md": [ { status: succeeded, output: "ok" } ]
        "*": { status: succeeded, output: "ok" }
      source: [ { id: a.md }, { id: b.md } ]
  ask_db:
    - visit: 1
      attempts:
        - { question: { text: "Which DB?", answer: "postgres" }, then: { status: succeeded } }
        - { question: { text: "Which DB?", timeout: true }, then: { status: succeeded } }
  child:
    - { visit: 1, playbook: { scenario: child-green } }
    - { visit: 2, playbook: { status: succeeded, output: { pr_url: "x" } } }
  deploy_wait:
    - { visit: 1, wait: { elapsed: true } }
  ship:
    - { visit: 1, publish: { decision: approve, status: succeeded } }
baseline: { before: { unowned: { count: 37 } }, after: { unowned: { count: 0 } } }
goal: { "0": pass, "1": fail }
expect:
  run: { outcome: [succeeded] }
  finish: ready
  route: { path: [start, intake], in_order: true, not_visits: [blocked] }
  visits: { qa_gates: 2 }
  edges: { taken: { "e:ci_check>ready#1": 1 }, not_taken: ["id:push-reject"] }
  joins: { dead: { ready: [ci_wait_final] } }
  outputs: [ { node: bot_review, field: pushed, equals: "no" } ]
  run_outputs: { pr_url: { state: value } }
  templates_empty: report
"#;

    fn parse(s: &str) -> Result<Scenario, yaml_serde::Error> {
        yaml_serde::from_str(s)
    }

    #[test]
    fn the_worked_example_reads_every_answer_kind() {
        let s = parse(EXAMPLE).unwrap();
        assert_eq!((s.on_missing, s.on_extra), (OnMissing::Fail, OnExtra::Warn));
        assert_eq!(s.decisions.judge_edge, Some(DecisionMode::Shadow));
        let fix = &s.answers["qa_fix"][0];
        assert_eq!(
            fix.attempts[0].failure_kind,
            Some(ScriptedFailure::Transient)
        );
        assert_eq!(
            fix.attempts[1].read_only.as_ref().unwrap().dirty,
            ["src/a.rs"]
        );
        assert_eq!(s.answers["bot_review"][0].visit, Visit::Default);
        let judge = &s.answers["check_task"];
        assert!(
            matches!(&judge[0].judge, Some(JudgeAnswer::Answers(a)) if a["sufficient"].p == Some(0.9))
        );
        assert_eq!(
            judge[1].judge,
            Some(JudgeAnswer::Unavailable {
                unavailable: UnavailableKind::Timeout
            })
        );
        let items = &s.answers["files"][0].items;
        assert!(matches!(items["a.md"], ItemAnswers::Attempts(_)));
        assert!(matches!(items["*"], ItemAnswers::One(_)));
        let ask = &s.answers["ask_db"][0].attempts;
        assert!(ask[1].question.as_ref().unwrap().timeout);
        assert_eq!(
            ask[0].then.as_ref().unwrap().status,
            Some(ScriptedStatus::Succeeded)
        );
        let child = &s.answers["child"];
        assert!(
            matches!(&child[0].playbook, Some(ChildAnswer::Scenario { scenario }) if scenario == "child-green")
        );
        assert!(matches!(
            &child[1].playbook,
            Some(ChildAnswer::Result {
                status: ScriptedStatus::Succeeded,
                output: Some(_)
            })
        ));
        assert!(s.answers["deploy_wait"][0].wait.as_ref().unwrap().elapsed);
        assert!(matches!(s.baseline, Some(BaselineAnswer::Observed { .. })));
        assert_eq!(s.goal["1"], GoalAnswer::Fail);
        assert_eq!(s.expect.route.as_ref().unwrap().path, ["start", "intake"]);
        assert_eq!(s.expect.templates_empty, TemplatesEmpty::Report);
        // Defaults when absent.
        let bare = parse("schema: 1\nid: x\n").unwrap();
        assert_eq!(
            (bare.on_missing, bare.on_extra),
            (OnMissing::Error, OnExtra::Error)
        );
        assert_eq!(bare.expect.templates_empty, TemplatesEmpty::Error);
        assert!(matches!(
            parse("schema: 1\nid: x\nbaseline: skip\n")
                .unwrap()
                .baseline,
            Some(BaselineAnswer::Skip(SkipWord::Skip))
        ));
    }

    /// A misspelt key is an error wherever it sits, never an answer or an
    /// expectation silently dropped.
    #[test]
    fn a_misspelt_key_is_refused_at_every_level() {
        for bad in [
            "schema: 1\nid: x\nanswer: {}\n",
            "schema: 1\nid: x\nanswers: { a: [ { visit: 1, atempts: [] } ] }\n",
            "schema: 1\nid: x\nanswers: { a: [ { visit: 1, attempts: [ { status: succeeded, ouput: y } ] } ] }\n",
            "schema: 1\nid: x\nanswers: { a: [ { visit: 1, review: { decision: approve, notes: y } } ] }\n",
            "schema: 1\nid: x\nanswers: { a: [ { visit: 1, judge_edges: { \"id:e\": { p: 0.1, pp: 1 } } } ] }\n",
            "schema: 1\nid: x\nexpect: { route: { path: [a], visit: [a] } }\n",
            "schema: 1\nid: x\nexpect: { outputs: [ { node: a, equal: b } ] }\n",
            "schema: 1\nid: x\ndecisions: { completion_check: enforce }\n",
            "schema: 1\nid: x\nanswers: { a: [ { visit: first } ] }\n",
            "schema: 1\nid: x\nanswers: { a: [ { visit: 1, attempts: [ { status: failed, failure_kind: flaky } ] } ] }\n",
        ] {
            assert!(parse(bad).is_err(), "accepted: {bad}");
        }
    }
}
