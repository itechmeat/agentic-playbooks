//! The scenario rules, V146 to V151.
//!
//! - **V146** (error): the file on its own: it does not parse or cannot be
//!   read, an unknown field, `schema` is not 1, `id` is not the file name or
//!   outside the name grammar, `versions` is malformed, a visit carries no
//!   answer or more than one kind (besides the two pairs a node can take:
//!   `items` with `judge`, `judge` with `attempts`), a wait answer names not
//!   exactly one of `elapsed`, `signal` and `timed_out`, an attempt's `then`
//!   has no `question`, a question is both answered and timed out (or
//!   neither), a succeeded attempt names a failure kind, a goal key is not
//!   an index.
//! - **V147** (error): an answer key: an unknown node; an answer kind the
//!   node does not take (`items` on a node without `list:` among them;
//!   `judge` on a list node needs a judge item check, `attempts` on a judge
//!   needs `on_unavailable: emulate`);
//!   a visit that is not positive or is numbered twice; more than one
//!   `visit: default`; a goal index that is not a script criterion; a
//!   baseline answer for a check the playbook does not have.
//! - **V148** (error): an answer value: a review or publish decision that is
//!   not an option; a judge question the node does not ask, a choice or
//!   level that is not one of its options, a probability or confidence
//!   outside 0 to 1; a judge edge address that does not resolve or is not a
//!   judge edge of that node; a nested scenario name outside the grammar; a
//!   scripted child output that is not a JSON object where the node hands
//!   its parent the child's outputs.
//! - **V149** (error): a param the playbook declares `secret` is set.
//! - **V150** (error): an expectation names an unknown node, edge address
//!   or run output, a finish that is not a finish node, a join input that
//!   does not feed that join, a run outcome, node status or output state
//!   apb does not write, or a regex that does not compile.
//! - **V151** (warning): a visit answers more attempts than the node's
//!   `max_retries` allows (infrastructure and quota failures, which retry
//!   out of their own budget, are not counted).

use std::collections::BTreeSet;
use std::path::Path;

use super::format::{
    AttemptAnswer, ChildAnswer, Expect, JudgeAnswer, QuestionAnswer, Scenario, ScriptedFailure,
    ScriptedStatus, Visit, VisitAnswer, WaitAnswer,
};
use crate::edge_address::EdgeAddress;
use crate::judge::JudgeQuestion;
use crate::schema::{EdgeCondition, GoalCheck, Node, NodeKind, Playbook};
use crate::validate::{Issue, Severity};

/// The answer kinds a visit can carry; `judge_edges` rides along with any.
const ATTEMPTS: &str = "attempts";
const SCRIPT: &str = "script";
const REVIEW: &str = "review";
const JUDGE: &str = "judge";
const ITEMS: &str = "items";
const PLAYBOOK: &str = "playbook";
const WAIT: &str = "wait";
const PUBLISH: &str = "publish";

/// The output states a run output can end in.
const OUTPUT_STATES: [&str; 3] = ["value", "null", "missing"];

/// The statuses a `node_finished` records, what `expect.status` may name.
const NODE_STATUSES: [&str; 6] = [
    "succeeded",
    "failed",
    "timed_out",
    "cancelled",
    "skipped",
    "unknown",
];

/// The pairs of kinds one visit may carry, because one node crosses both
/// boundaries: a list whose item check is a judge (`items` and `judge`),
/// and a judge that may go on to its profile (`judge` and `attempts`).
const PAIRS: [[&str; 2]; 2] = [[ITEMS, JUDGE], [JUDGE, ATTEMPTS]];

/// Whether a visit carrying `kinds` asks for more than any node takes.
fn too_many_kinds(kinds: &[&str]) -> bool {
    match kinds {
        [] | [_] => false,
        [a, b] => !PAIRS
            .iter()
            .any(|p| (p[0] == *a && p[1] == *b) || (p[0] == *b && p[1] == *a)),
        _ => true,
    }
}

/// The kinds `v` carries, besides `judge_edges` (`items` and `source` are
/// one kind).
fn kinds_of(v: &VisitAnswer) -> Vec<&'static str> {
    let mut out = Vec::new();
    if !v.attempts.is_empty() {
        out.push(ATTEMPTS);
    }
    for (present, kind) in [
        (v.script.is_some(), SCRIPT),
        (v.review.is_some(), REVIEW),
        (v.judge.is_some(), JUDGE),
        (!v.items.is_empty() || v.source.is_some(), ITEMS),
        (v.playbook.is_some(), PLAYBOOK),
        (v.wait.is_some(), WAIT),
        (v.publish.is_some(), PUBLISH),
    ] {
        if present {
            out.push(kind);
        }
    }
    out
}

fn visit_word(v: Visit) -> String {
    match v {
        Visit::Nth(n) => format!("visit {n}"),
        Visit::Default => "visit default".to_string(),
    }
}

fn wait_outcomes(w: &WaitAnswer) -> usize {
    [w.elapsed, w.signal.is_some(), w.timed_out]
        .into_iter()
        .filter(|b| *b)
        .count()
}

fn attempt_shape(a: &AttemptAnswer, at: &str, out: &mut Vec<String>) {
    if a.status == Some(ScriptedStatus::Succeeded) && a.failure_kind.is_some() {
        out.push(format!("{at}: a succeeded attempt has no `failure_kind`"));
    }
    match (&a.question, &a.then) {
        (None, Some(_)) => out.push(format!("{at}: `then` needs a `question`")),
        (Some(q), _) if q.answer.is_some() == q.timeout => out.push(format!(
            "{at}: a question is either answered (`answer`) or left to time out (`timeout: true`)"
        )),
        _ => {}
    }
    if let Some(then) = &a.then {
        attempt_shape(then, at, out);
    }
}

/// The V146 problems of a parsed scenario on its own (`name` is its file
/// stem), as sentences. Empty when the file is well formed.
pub fn file_problems(s: &Scenario, name: &str) -> Vec<String> {
    let mut p = Vec::new();
    if s.schema != super::format::SCENARIO_SCHEMA {
        p.push(format!(
            "schema {} is not supported (expected {})",
            s.schema,
            super::format::SCENARIO_SCHEMA
        ));
    }
    if s.id != name {
        p.push(format!(
            "id `{}` does not match the file name `{name}.yaml`",
            s.id
        ));
    }
    if let Some(r) = &s.versions
        && crate::eval::VersionRange::parse(r).is_none()
    {
        p.push(format!(
            "versions `{r}` is not a range like `>=1.2.0, <2.0.0`"
        ));
    }
    for (node, visits) in &s.answers {
        for v in visits {
            let at = format!("answers.{node} {}", visit_word(v.visit));
            let kinds = kinds_of(v);
            if kinds.is_empty() && v.judge_edges.is_empty() {
                p.push(format!("{at} carries no answer"));
            }
            if too_many_kinds(&kinds) {
                p.push(format!(
                    "{at} carries {} answers; a visit takes one kind, or `items` with `judge`, or `judge` with `attempts`",
                    kinds.join(" and ")
                ));
            }
            if let Some(w) = &v.wait
                && wait_outcomes(w) != 1
            {
                p.push(format!(
                    "{at}: a wait answer names exactly one of `elapsed`, `signal` and `timed_out`"
                ));
            }
            for (k, a) in v.attempts.iter().enumerate() {
                attempt_shape(a, &format!("{at} attempt {}", k + 1), &mut p);
            }
            for (item, answers) in &v.items {
                for (k, a) in item_attempts(answers).iter().enumerate() {
                    attempt_shape(a, &format!("{at} item `{item}` attempt {}", k + 1), &mut p);
                }
            }
        }
    }
    for key in s.goal.keys() {
        if key.parse::<usize>().is_err() {
            p.push(format!(
                "goal key `{key}` is not a criterion index (\"0\", \"1\", ...)"
            ));
        }
    }
    p
}

fn item_attempts(a: &super::format::ItemAnswers) -> &[AttemptAnswer] {
    match a {
        super::format::ItemAnswers::Attempts(v) => v,
        super::format::ItemAnswers::One(a) => std::slice::from_ref(a),
    }
}

/// The answer kind a node takes, `None` for a node that takes none (a
/// start, a condition, a finish without a prompt).
fn expected_kind(node: &Node) -> Option<&'static str> {
    if node.list.is_some() {
        return Some(ITEMS);
    }
    match &node.kind {
        NodeKind::AgentTask { .. } | NodeKind::Prompt { .. } => Some(ATTEMPTS),
        NodeKind::Finish {
            prompt: Some(_), ..
        } => Some(ATTEMPTS),
        NodeKind::Script { .. } => Some(SCRIPT),
        NodeKind::HumanReview { .. } => Some(REVIEW),
        NodeKind::Judge { .. } => Some(JUDGE),
        NodeKind::Playbook { .. } => Some(PLAYBOOK),
        NodeKind::Wait { .. } => Some(WAIT),
        NodeKind::Publish { .. } => Some(PUBLISH),
        NodeKind::Start | NodeKind::Condition { .. } | NodeKind::Finish { prompt: None, .. } => {
            None
        }
    }
}

/// Every answer kind a node takes: its own ([`expected_kind`]), plus
/// `judge` for a list whose item check is a judge, and `attempts` for a
/// judge that may emulate with its profile.
fn taken_kinds(node: &Node) -> Vec<&'static str> {
    let mut kinds: Vec<&'static str> = expected_kind(node).into_iter().collect();
    if list_judge(node).is_some() {
        kinds.push(JUDGE);
    }
    if matches!(node.kind, NodeKind::Judge { .. }) && node.kind.runs_agent() {
        kinds.push(ATTEMPTS);
    }
    kinds
}

/// The judge item check of a list node.
fn list_judge(node: &Node) -> Option<&crate::list::JudgeCheck> {
    node.list.as_ref()?.check.as_ref()?.judge.as_ref()
}

struct Ctx<'a> {
    name: &'a str,
    playbook: &'a Playbook,
    out: Vec<Issue>,
}

impl Ctx<'_> {
    fn push(&mut self, code: &'static str, node: Option<&str>, msg: String) {
        self.push_edges(code, node, msg, Vec::new());
    }

    fn push_edges(&mut self, code: &'static str, node: Option<&str>, msg: String, e: Vec<String>) {
        let severity = if code == "V151" {
            Severity::Warning
        } else {
            Severity::Error
        };
        self.out.push(
            Issue::new(
                code,
                severity,
                node.map(str::to_string),
                format!("scenario `{}`: {msg}", self.name),
            )
            .with_edges(e),
        );
    }
}

fn probability_problem(q: &QuestionAnswer) -> Option<String> {
    for (field, v) in [("p", q.p), ("confidence", q.confidence)] {
        if let Some(x) = v
            && !(0.0..=1.0).contains(&x)
        {
            return Some(format!("`{field}` {x} is outside 0 to 1"));
        }
    }
    None
}

fn judge_value_problem(question: &JudgeQuestion, q: &QuestionAnswer) -> Option<String> {
    let value = q.value.as_ref()?;
    match question {
        JudgeQuestion::Choice { criteria, .. } => {
            let options: Vec<&str> = criteria.keys().collect();
            match value.as_str() {
                Some(v) if options.contains(&v) => None,
                _ => Some(format!(
                    "value {value} is not one of its options ({})",
                    options.join(", ")
                )),
            }
        }
        JudgeQuestion::Score { levels, .. } => match value.as_str() {
            Some(v) if levels.iter().any(|l| l == v) => None,
            _ => Some(format!(
                "value {value} is not one of its levels ({})",
                levels.join(", ")
            )),
        },
        JudgeQuestion::Noul { .. } => {
            (!value.is_boolean()).then(|| format!("value {value} is not `true` or `false`"))
        }
    }
}

fn check_judge(cx: &mut Ctx, node: &Node, at: &str, answer: &JudgeAnswer) {
    let JudgeAnswer::Answers(map) = answer else {
        return;
    };
    let questions = match (&node.kind, list_judge(node)) {
        (_, Some(check)) => check.questions(),
        (NodeKind::Judge { questions, .. }, None) => questions.clone(),
        _ => return,
    };
    for (qid, q) in map {
        let Some(question) = questions.get(qid) else {
            cx.push(
                "V148",
                Some(&node.id),
                format!("{at}: judge `{}` asks no question `{qid}`", node.id),
            );
            continue;
        };
        if let Some(why) = judge_value_problem(question, q).or_else(|| probability_problem(q)) {
            cx.push(
                "V148",
                Some(&node.id),
                format!("{at}: question `{qid}`: {why}"),
            );
        }
    }
}

fn check_judge_edges(cx: &mut Ctx, node: &Node, at: &str, v: &VisitAnswer) {
    for (address, q) in &v.judge_edges {
        let resolved = EdgeAddress::parse(address)
            .ok()
            .and_then(|a| crate::edge_address::resolve(cx.playbook, &a));
        let Some(index) = resolved else {
            cx.push(
                "V148",
                Some(&node.id),
                format!("{at}: judge edge `{address}` names no edge of this playbook"),
            );
            continue;
        };
        let edge = &cx.playbook.edges[index];
        let rendered = crate::edge_address::rendered_at(cx.playbook, index)
            .into_iter()
            .collect();
        if edge.from != node.id || !matches!(edge.condition, Some(EdgeCondition::Judge { .. })) {
            cx.push_edges(
                "V148",
                Some(&node.id),
                format!(
                    "{at}: edge `{address}` is not a judge edge of `{}`",
                    node.id
                ),
                rendered,
            );
        } else if let Some(why) = probability_problem(q) {
            cx.push_edges(
                "V148",
                Some(&node.id),
                format!("{at}: judge edge `{address}`: {why}"),
                rendered,
            );
        }
    }
}

fn check_values(cx: &mut Ctx, node: &Node, at: &str, v: &VisitAnswer) {
    if let (Some(r), NodeKind::HumanReview { options, .. }) = (&v.review, &node.kind) {
        let options = crate::schema::effective_review_options(options);
        if !options.contains(&r.decision) {
            cx.push(
                "V148",
                Some(&node.id),
                format!(
                    "{at}: review decision `{}` is not an option of `{}` ({})",
                    r.decision,
                    node.id,
                    options.join(", ")
                ),
            );
        }
    }
    if let Some(p) = &v.publish
        && !crate::publish::OPTIONS.contains(&p.decision.as_str())
    {
        cx.push(
            "V148",
            Some(&node.id),
            format!(
                "{at}: publish decision `{}` is not one of {}",
                p.decision,
                crate::publish::OPTIONS.join(", ")
            ),
        );
    }
    if let Some(j) = &v.judge {
        check_judge(cx, node, at, j);
    }
    match (&v.playbook, &node.kind) {
        (Some(ChildAnswer::Scenario { scenario }), _) if !super::is_valid_name(scenario) => {
            cx.push(
                "V148",
                Some(&node.id),
                format!("{at}: nested scenario `{scenario}` is not a scenario name"),
            );
        }
        (
            Some(ChildAnswer::Result {
                output: Some(out), ..
            }),
            NodeKind::Playbook { output, .. },
        ) if !output.is_answer() && !out.is_object() => {
            cx.push(
                "V148",
                Some(&node.id),
                format!(
                    "{at}: `{}` hands its parent the child's outputs, so a scripted output must be a JSON object",
                    node.id
                ),
            );
        }
        _ => {}
    }
    check_judge_edges(cx, node, at, v);
}

/// The attempts of a visit that count against `max_retries`.
fn counted_attempts(attempts: &[AttemptAnswer]) -> usize {
    attempts
        .iter()
        .filter(|a| {
            !matches!(
                a.failure_kind,
                Some(ScriptedFailure::Transient | ScriptedFailure::Quota)
            )
        })
        .count()
}

fn max_retries_of(playbook: &Playbook, node: &Node) -> Option<u32> {
    if node.list.is_some() {
        return None;
    }
    match &node.kind {
        NodeKind::AgentTask { .. } => Some(playbook.effective_max_retries(node)),
        NodeKind::Finish {
            prompt: Some(_), ..
        } => Some(playbook.defaults.max_retries.unwrap_or(0)),
        _ => None,
    }
}

fn check_visits(cx: &mut Ctx, node: &Node, visits: &[VisitAnswer]) {
    let mut seen = BTreeSet::new();
    let mut defaults = 0;
    let expected = expected_kind(node);
    let taken = taken_kinds(node);
    for v in visits {
        let at = format!("answers.{} {}", node.id, visit_word(v.visit));
        match v.visit {
            Visit::Nth(0) => cx.push("V147", Some(&node.id), format!("{at}: visits count from 1")),
            Visit::Nth(n) if !seen.insert(n) => {
                cx.push("V147", Some(&node.id), format!("{at} is answered twice"))
            }
            Visit::Default => defaults += 1,
            Visit::Nth(_) => {}
        }
        for kind in kinds_of(v) {
            if taken.contains(&kind) {
                continue;
            }
            let takes = match expected {
                Some(k) => format!("it takes `{k}`"),
                None => "it takes no answer".to_string(),
            };
            let why = if kind == ITEMS {
                format!("`{}` has no `list:`, so it has no items; {takes}", node.id)
            } else {
                format!("`{}` is a {} node; {takes}", node.id, node.kind.type_str())
            };
            cx.push(
                "V147",
                Some(&node.id),
                format!("{at}: `{kind}` does not fit: {why}"),
            );
        }
        check_values(cx, node, &at, v);
        if let Some(max) = max_retries_of(cx.playbook, node) {
            let n = counted_attempts(&v.attempts);
            if n > max as usize + 1 {
                cx.push(
                    "V151",
                    Some(&node.id),
                    format!(
                        "{at} answers {n} attempts, but `{}` makes at most {} (max_retries {max}); the extra ones are reported unused unless a profile fallback takes them",
                        node.id,
                        max + 1
                    ),
                );
            }
        }
    }
    if defaults > 1 {
        cx.push(
            "V147",
            Some(&node.id),
            format!(
                "answers.{} has {defaults} `visit: default` entries; at most one",
                node.id
            ),
        );
    }
}

fn check_answers(cx: &mut Ctx, s: &Scenario) {
    for (id, visits) in &s.answers {
        match cx.playbook.node(id) {
            Some(node) => check_visits(cx, node, visits),
            None => cx.push(
                "V147",
                None,
                format!(
                    "answers name node `{id}` that version {} does not have",
                    cx.playbook.version
                ),
            ),
        }
    }
    let criteria = cx
        .playbook
        .goal
        .as_ref()
        .map(|g| g.criteria.as_slice())
        .unwrap_or_default();
    for key in s.goal.keys() {
        let Ok(i) = key.parse::<usize>() else {
            continue;
        };
        if !matches!(
            criteria.get(i).map(|c| &c.check),
            Some(GoalCheck::Script { .. })
        ) {
            cx.push(
                "V147",
                None,
                format!("goal `{key}` is not a script criterion of the playbook's goal"),
            );
        }
    }
    if let Some(super::format::BaselineAnswer::Observed { before, after }) = &s.baseline {
        let checks: BTreeSet<&str> = cx
            .playbook
            .baseline_check
            .as_ref()
            .map(|b| b.checks.iter().map(|c| c.id.as_str()).collect())
            .unwrap_or_default();
        let mut unknown: BTreeSet<&str> = BTreeSet::new();
        for id in before.keys().chain(after.keys()) {
            if !checks.contains(id.as_str()) {
                unknown.insert(id);
            }
        }
        for id in unknown {
            cx.push(
                "V147",
                None,
                format!("baseline answers check `{id}`, which the playbook's baseline_check does not have"),
            );
        }
    }
}

fn check_params(cx: &mut Ctx, s: &Scenario) {
    for p in &cx.playbook.params {
        if p.secret && s.params.contains_key(&p.name) {
            cx.push(
                "V149",
                None,
                format!(
                    "param `{}` is secret and may not be set; a scenario run gets a placeholder for it",
                    p.name
                ),
            );
            continue;
        }
        // A scenario value must match the param's pattern (the engine
        // skips a secret param's placeholder at run time).
        if let Some(value) = s.params.get(&p.name)
            && crate::param_pattern::check(p, value).is_err()
        {
            cx.push(
                "V178",
                None,
                format!(
                    "scenario param `{}` does not match its pattern `{}`",
                    p.name,
                    p.pattern.as_deref().unwrap_or("")
                ),
            );
        }
    }
}

fn expected_nodes(e: &Expect) -> Vec<&str> {
    let mut named: Vec<&str> = Vec::new();
    if let Some(r) = &e.route {
        named.extend(r.path.iter().map(String::as_str));
        named.extend(r.visits.iter().map(String::as_str));
        named.extend(r.not_visits.iter().map(String::as_str));
    }
    named.extend(e.visits.keys().map(String::as_str));
    named.extend(e.status.keys().map(String::as_str));
    named.extend(e.outputs.iter().map(|o| o.node.as_str()));
    named
}

fn check_expect(cx: &mut Ctx, e: &Expect) {
    let pb = cx.playbook;
    let mut unknown: BTreeSet<&str> = expected_nodes(e)
        .into_iter()
        .filter(|n| pb.node(n).is_none())
        .collect();
    for (join, inputs) in &e.joins.dead {
        if pb.node(join).is_none() {
            unknown.insert(join);
            continue;
        }
        for input in inputs {
            if !pb
                .edges
                .iter()
                .any(|ed| &ed.from == input && &ed.to == join)
            {
                cx.push(
                    "V150",
                    Some(join),
                    format!("expect.joins.dead: `{input}` is not an input of `{join}`"),
                );
            }
        }
    }
    for n in unknown {
        cx.push(
            "V150",
            None,
            format!(
                "expect names node `{n}` that version {} does not have",
                pb.version
            ),
        );
    }
    if let Some(f) = &e.finish
        && !matches!(pb.node(f).map(|n| &n.kind), Some(NodeKind::Finish { .. }))
    {
        cx.push(
            "V150",
            None,
            format!("expect.finish `{f}` is not a finish node"),
        );
    }
    let addresses = e.edges.taken.keys().chain(e.edges.not_taken.iter());
    for a in addresses {
        let resolves = EdgeAddress::parse(a)
            .ok()
            .and_then(|addr| crate::edge_address::resolve(pb, &addr))
            .is_some();
        if !resolves {
            cx.push(
                "V150",
                None,
                format!("expect.edges: `{a}` names no edge of this playbook"),
            );
        }
    }
    for (name, want) in &e.run_outputs {
        if pb.outputs.get(name).is_none() {
            cx.push(
                "V150",
                None,
                format!("expect.run_outputs: the playbook declares no output `{name}`"),
            );
        }
        if !OUTPUT_STATES.contains(&want.state.as_str()) {
            cx.push(
                "V150",
                None,
                format!(
                    "expect.run_outputs.{name}: state `{}` is not one of {}",
                    want.state,
                    OUTPUT_STATES.join(", ")
                ),
            );
        }
    }
    for (node, want) in &e.status {
        if !NODE_STATUSES.contains(&want.as_str()) {
            cx.push(
                "V150",
                Some(node),
                format!(
                    "expect.status.{node}: status `{want}` is not one of {}",
                    NODE_STATUSES.join(", ")
                ),
            );
        }
    }
    if let Some(run) = &e.run {
        for o in &run.outcome {
            if !crate::eval::OUTCOMES.contains(&o.as_str()) {
                cx.push(
                    "V150",
                    None,
                    format!(
                        "expect.run.outcome `{o}` is not one of {}",
                        crate::eval::OUTCOMES.join(", ")
                    ),
                );
            }
        }
    }
    let regexes = e
        .outputs
        .iter()
        .flat_map(|o| [&o.matches, &o.not_matches])
        .flatten();
    for r in regexes {
        if let Err(err) = regex::Regex::new(r) {
            cx.push(
                "V150",
                None,
                format!("expect.outputs: regex `{r}` does not compile: {err}"),
            );
        }
    }
}

/// The V147 to V151 issues of a parsed scenario (`name` is its file stem)
/// against the playbook version it targets.
pub fn validate_scenario(name: &str, scenario: &Scenario, playbook: &Playbook) -> Vec<Issue> {
    let mut cx = Ctx {
        name,
        playbook,
        out: Vec::new(),
    };
    check_answers(&mut cx, scenario);
    check_params(&mut cx, scenario);
    check_expect(&mut cx, &scenario.expect);
    cx.out
}

/// Every scenario issue of the playbook at `playbook_dir` against the loaded
/// version (`apb validate`): V146 for each file that does not load, and
/// V147 to V151 for each that does and whose `versions` admit this version.
/// A scenario kept for other versions is not held to this one. A playbook
/// without scenarios yields nothing.
pub fn validate_all(playbook_dir: &Path, playbook: &Playbook) -> Vec<Issue> {
    let loaded = super::load_all(playbook_dir);
    let mut out = loaded.issues;
    for s in &loaded.scenarios {
        if super::applies_to(&s.scenario, &playbook.version) {
            out.extend(validate_scenario(&s.name, &s.scenario, playbook));
        }
    }
    out
}
