//! The answer book of a scenario run: which scripted answer answers which
//! crossing of a boundary, and which answers were used, taken by default,
//! missing or never used.
//!
//! Keys (design rules):
//!
//! - a **visit** is the node's execution ordinal (1-based), handed in by the
//!   boundary site from the journal; `visit: default` answers every visit
//!   with no numbered entry and is never extra;
//! - an **attempt** is the k-th attempt the book handed out for one visit
//!   (and item), counted here, never the journal's display ordinal. An
//!   interactive question is part of its attempt: the attempt that asks
//!   hands out `question`, the next call for the same visit hands out its
//!   `then` without counting a new attempt;
//! - a **list item** is keyed by its id, `"*"` answering every item not
//!   listed;
//! - **judge edges** are keyed by edge address and handed out by the rank
//!   the engine asks under (`edge_<rank>`).
//!
//! An answer nobody used by the end of the run is extra; the book never
//! reads the journal.

use std::collections::{BTreeMap, BTreeSet};

use apb_core::scenario::{
    AttemptAnswer, BaselineAnswer, CheckAnswer, ChildAnswer, GoalAnswer, ItemAnswers, JudgeAnswer,
    PublishAnswer, QuestionAnswer, QuestionScript, ReviewAnswer, Scenario, ScriptAnswer, Visit,
    VisitAnswer, WaitAnswer,
};
use apb_core::schema::Playbook;
use serde::Serialize;

/// The default answer of every list item not named.
pub const ANY_ITEM: &str = "*";

/// One scripted answer of the scenario file: the node, the visit entry
/// (`default` or a number) and which part of it.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
pub struct AnswerRef {
    /// Absent for a run-level answer (baseline, goal).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub node: Option<String>,
    /// The visit entry: a number, or `default`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub visit: Option<Visit>,
    /// `attempt 2`, `item a.md attempt 1`, `script`, `review`, `judge`,
    /// `edge id:x`, `source`, `playbook`, `wait`, `publish`, `baseline`,
    /// `goal 0`.
    pub part: String,
}

/// A boundary the scenario had no answer for.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct MissingAnswer {
    /// The boundary kind's name.
    pub boundary: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub node: Option<String>,
    pub visit: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub attempt: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub item: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub part: Option<String>,
}

impl MissingAnswer {
    /// The words a failure or a refusal names it with.
    pub fn describe(&self) -> String {
        let mut s = format!("scenario has no answer for {}", self.boundary);
        if let Some(n) = &self.node {
            s.push_str(&format!(" of `{n}` visit {}", self.visit));
        }
        if let Some(i) = &self.item {
            s.push_str(&format!(" item `{i}`"));
        }
        if let Some(a) = self.attempt {
            s.push_str(&format!(" attempt {a}"));
        }
        if let Some(p) = &self.part {
            s.push_str(&format!(" ({p})"));
        }
        s
    }
}

/// How the scenario's answers were used.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct AnswerUse {
    /// Numbered answers that were used.
    pub used: Vec<AnswerRef>,
    /// `visit: default` answers that were used.
    pub defaults: Vec<AnswerRef>,
    /// Boundaries the scenario had no answer for, in order.
    pub missing: Vec<MissingAnswer>,
    /// Numbered answers nobody used.
    pub extra: Vec<AnswerRef>,
}

/// One handed-out answer and the scripted entry it came from.
#[derive(Debug, Clone, PartialEq)]
pub struct Taken<T> {
    pub answer: T,
    pub from: AnswerRef,
}

/// The attempt key: node, visit, item.
type AttemptKey = (String, u32, Option<String>);

/// The answer book of one scenario run.
#[derive(Debug)]
pub struct AnswerBook {
    scenario: Scenario,
    /// `(node, edge address) -> rank among the node's outgoing edges`.
    edge_ranks: BTreeMap<(String, String), usize>,
    /// Attempts handed out per visit and item.
    attempts: BTreeMap<AttemptKey, u32>,
    /// The last attempt handed out per visit and item, its `then` still
    /// owed when it asked a question.
    current: BTreeMap<AttemptKey, Current>,
    used: BTreeSet<AnswerRef>,
    missing: Vec<MissingAnswer>,
}

#[derive(Debug, Clone)]
struct Current {
    answer: AttemptAnswer,
    attempt: u32,
    /// Set when the attempt asks: its `then` is the next hand-out.
    asked: bool,
}

impl AnswerBook {
    /// The book of `scenario` over the playbook it runs.
    pub fn new(scenario: Scenario, playbook: &Playbook) -> Self {
        let edge_ranks = apb_core::edge_address::edge_addresses(playbook)
            .into_iter()
            .map(|a| {
                (
                    (playbook.edges[a.index].from.clone(), a.rendered.clone()),
                    a.rank,
                )
            })
            .collect();
        AnswerBook {
            scenario,
            edge_ranks,
            attempts: BTreeMap::new(),
            current: BTreeMap::new(),
            used: BTreeSet::new(),
            missing: Vec::new(),
        }
    }

    pub fn scenario(&self) -> &Scenario {
        &self.scenario
    }

    /// The visit entry answering visit `visit` of `node`: the numbered one,
    /// else the default.
    fn entry(&self, node: &str, visit: u32) -> Option<&VisitAnswer> {
        let entries = self.scenario.answers.get(node)?;
        entries
            .iter()
            .find(|v| v.visit == Visit::Nth(visit))
            .or_else(|| entries.iter().find(|v| v.visit == Visit::Default))
    }

    fn take<T>(
        &mut self,
        node: &str,
        visit: u32,
        part: String,
        pick: impl Fn(&VisitAnswer) -> Option<T>,
    ) -> Option<Taken<T>> {
        let entry = self.entry(node, visit)?;
        let answer = pick(entry)?;
        let from = AnswerRef {
            node: Some(node.to_string()),
            visit: Some(entry.visit),
            part,
        };
        self.used.insert(from.clone());
        Some(Taken { answer, from })
    }

    /// Records a boundary with no answer.
    pub fn record_missing(&mut self, missing: MissingAnswer) {
        self.missing.push(missing);
    }

    /// The next attempt of visit `visit` of `node` (of item `item`): the
    /// `then` an asking attempt owes, else the next scripted attempt. Also
    /// returns the attempt's number within the visit.
    pub fn attempt(
        &mut self,
        node: &str,
        visit: u32,
        item: Option<&str>,
    ) -> (u32, Option<Taken<AttemptAnswer>>) {
        let key: AttemptKey = (node.to_string(), visit, item.map(str::to_string));
        if let Some(cur) = self.current.get_mut(&key)
            && cur.asked
        {
            let attempt = cur.attempt;
            let then = cur.answer.then.as_deref().cloned().unwrap_or_default();
            cur.answer = then.clone();
            cur.asked = then.question.is_some();
            let from = AnswerRef {
                node: Some(node.to_string()),
                visit: None,
                part: attempt_part(item, attempt),
            };
            return (attempt, Some(Taken { answer: then, from }));
        }
        let n = self.attempts.entry(key.clone()).or_default();
        *n += 1;
        let k = *n;
        // The entry that answers: the item's own, else `"*"`; a single
        // answer answers every retry as its attempt 1.
        let taken = match item {
            None => self.take(node, visit, attempt_part(None, k), |v| {
                v.attempts.get(k as usize - 1).cloned()
            }),
            Some(id) => {
                let entry = self.entry(node, visit);
                let named = entry
                    .and_then(|v| v.items.get(id).map(|a| (id, a)))
                    .or_else(|| entry.and_then(|v| v.items.get(ANY_ITEM).map(|a| (ANY_ITEM, a))));
                match named {
                    None => None,
                    Some((name, answers)) => {
                        let (n, answer) = match answers {
                            ItemAnswers::Attempts(list) => (k, list.get(k as usize - 1).cloned()),
                            ItemAnswers::One(one) => (1, Some(one.clone())),
                        };
                        let name = name.to_string();
                        answer.and_then(|a| {
                            self.take(node, visit, attempt_part(Some(&name), n), |_| {
                                Some(a.clone())
                            })
                        })
                    }
                }
            }
        };
        if let Some(t) = &taken {
            self.current.insert(
                key,
                Current {
                    answer: t.answer.clone(),
                    attempt: k,
                    // The scripted agent asks as it is handed out: its
                    // `then` is owed whoever answers (the scenario's person,
                    // a default, an unattended run).
                    asked: t.answer.question.is_some(),
                },
            );
        }
        (k, taken)
    }

    /// The question the current attempt of the visit asks (its `then` is
    /// owed from the hand-out on).
    pub fn question(&self, node: &str, visit: u32, item: Option<&str>) -> Option<QuestionScript> {
        let key: AttemptKey = (node.to_string(), visit, item.map(str::to_string));
        self.current.get(&key)?.answer.question.clone()
    }

    /// The check verdict of the current attempt of the visit: its
    /// `check`, `pass` when it names none.
    pub fn check(&self, node: &str, visit: u32, item: Option<&str>) -> CheckAnswer {
        let key: AttemptKey = (node.to_string(), visit, item.map(str::to_string));
        self.current
            .get(&key)
            .and_then(|c| c.answer.check)
            .unwrap_or(CheckAnswer::Pass)
    }

    /// The paths the read-only check of the current attempt finds changed.
    pub fn read_only_dirty(&self, node: &str, visit: u32, item: Option<&str>) -> Vec<String> {
        let key: AttemptKey = (node.to_string(), visit, item.map(str::to_string));
        self.current
            .get(&key)
            .and_then(|c| c.answer.read_only.as_ref())
            .map(|r| r.dirty.clone())
            .unwrap_or_default()
    }

    pub fn script(&mut self, node: &str, visit: u32) -> Option<Taken<ScriptAnswer>> {
        self.take(node, visit, "script".into(), |v| v.script.clone())
    }

    pub fn review(&mut self, node: &str, visit: u32) -> Option<Taken<ReviewAnswer>> {
        self.take(node, visit, "review".into(), |v| v.review.clone())
    }

    pub fn judge(&mut self, node: &str, visit: u32) -> Option<Taken<JudgeAnswer>> {
        self.take(node, visit, "judge".into(), |v| v.judge.clone())
    }

    /// The judge edge answers of execution `visit` of `node`, keyed by the
    /// rank the engine asks under (`edge_<rank>`). An address that resolves
    /// to no outgoing edge of the node is left out (a V-scenario error).
    pub fn judge_edges(
        &mut self,
        node: &str,
        visit: u32,
    ) -> Option<Taken<BTreeMap<String, QuestionAnswer>>> {
        let entry = self.entry(node, visit)?;
        if entry.judge_edges.is_empty() {
            return None;
        }
        let visit_label = entry.visit;
        let answers = entry.judge_edges.clone();
        let mut by_rank = BTreeMap::new();
        for (address, answer) in &answers {
            if let Some(rank) = self.edge_ranks.get(&(node.to_string(), address.clone())) {
                by_rank.insert(format!("edge_{rank}"), answer.clone());
                self.used.insert(AnswerRef {
                    node: Some(node.to_string()),
                    visit: Some(visit_label),
                    part: format!("edge {address}"),
                });
            }
        }
        Some(Taken {
            answer: by_rank,
            from: AnswerRef {
                node: Some(node.to_string()),
                visit: Some(visit_label),
                part: "judge_edges".into(),
            },
        })
    }

    pub fn source(&mut self, node: &str, visit: u32) -> Option<Taken<Vec<serde_json::Value>>> {
        self.take(node, visit, "source".into(), |v| v.source.clone())
    }

    pub fn child(&mut self, node: &str, visit: u32) -> Option<Taken<ChildAnswer>> {
        self.take(node, visit, "playbook".into(), |v| v.playbook.clone())
    }

    pub fn wait(&mut self, node: &str, visit: u32) -> Option<Taken<WaitAnswer>> {
        self.take(node, visit, "wait".into(), |v| v.wait.clone())
    }

    pub fn publish(&mut self, node: &str, visit: u32) -> Option<Taken<PublishAnswer>> {
        self.take(node, visit, "publish".into(), |v| v.publish.clone())
    }

    /// The baseline observations.
    pub fn baseline(&mut self) -> Option<Taken<BaselineAnswer>> {
        let answer = self.scenario.baseline.clone()?;
        let from = AnswerRef {
            node: None,
            visit: None,
            part: "baseline".into(),
        };
        self.used.insert(from.clone());
        Some(Taken { answer, from })
    }

    /// The result of the script goal criterion at `index`.
    pub fn goal(&mut self, index: usize) -> Option<Taken<GoalAnswer>> {
        let answer = *self.scenario.goal.get(&index.to_string())?;
        let from = AnswerRef {
            node: None,
            visit: None,
            part: format!("goal {index}"),
        };
        self.used.insert(from.clone());
        Some(Taken { answer, from })
    }

    /// How the answers were used: every scripted answer of a numbered visit
    /// that was not used is extra, and so are an unused baseline and goal
    /// answers.
    pub fn usage(&self) -> AnswerUse {
        let mut all: Vec<AnswerRef> = Vec::new();
        for (node, entries) in &self.scenario.answers {
            for v in entries {
                for part in parts_of(v) {
                    all.push(AnswerRef {
                        node: Some(node.clone()),
                        visit: Some(v.visit),
                        part,
                    });
                }
            }
        }
        if self.scenario.baseline.is_some() {
            all.push(AnswerRef {
                node: None,
                visit: None,
                part: "baseline".into(),
            });
        }
        for index in self.scenario.goal.keys() {
            all.push(AnswerRef {
                node: None,
                visit: None,
                part: format!("goal {index}"),
            });
        }
        let mut out = AnswerUse {
            missing: self.missing.clone(),
            ..Default::default()
        };
        for r in all {
            // `visit: default` and the `"*"` item answer are defaults: used
            // or not, never extra.
            let default =
                r.visit == Some(Visit::Default) || r.part.starts_with(&format!("item {ANY_ITEM} "));
            match (self.used.contains(&r), default) {
                (true, true) => out.defaults.push(r),
                (true, false) => out.used.push(r),
                (false, true) => {}
                (false, false) => out.extra.push(r),
            }
        }
        out
    }
}

fn attempt_part(item: Option<&str>, attempt: u32) -> String {
    match item {
        Some(id) => format!("item {id} attempt {attempt}"),
        None => format!("attempt {attempt}"),
    }
}

/// Every answer one visit entry scripts.
fn parts_of(v: &VisitAnswer) -> Vec<String> {
    let mut parts: Vec<String> = (1..=v.attempts.len())
        .map(|k| attempt_part(None, k as u32))
        .collect();
    let mut add = |present: bool, name: &str| {
        if present {
            parts.push(name.to_string());
        }
    };
    add(v.script.is_some(), "script");
    add(v.review.is_some(), "review");
    add(v.judge.is_some(), "judge");
    add(v.source.is_some(), "source");
    add(v.playbook.is_some(), "playbook");
    add(v.wait.is_some(), "wait");
    add(v.publish.is_some(), "publish");
    for address in v.judge_edges.keys() {
        parts.push(format!("edge {address}"));
    }
    for (id, answers) in &v.items {
        let n = match answers {
            ItemAnswers::Attempts(list) => list.len(),
            ItemAnswers::One(_) => 1,
        };
        for k in 1..=n {
            parts.push(attempt_part(Some(id), k as u32));
        }
    }
    parts
}

#[cfg(test)]
mod tests {
    use super::*;

    const PLAYBOOK: &str = "schema: 2\nid: p\nname: p\nversion: 1.0.0\nnodes:\n  - { id: start, type: start }\n  - { id: work, type: script, script: s, runner: sh }\n  - { id: fix, type: finish, outcome: success }\n  - { id: ship, type: finish, outcome: success }\nedges:\n  - { from: start, to: work }\n  - { id: needs-fix, from: work, to: fix, condition: { type: judge, question: q, min_p: 0.7, on_unavailable: false } }\n  - { from: work, to: ship, fallback: true }\n";

    fn book(answers: &str) -> AnswerBook {
        let scenario: Scenario =
            yaml_serde::from_str(&format!("schema: 1\nid: s\n{answers}")).unwrap();
        AnswerBook::new(scenario, &Playbook::from_yaml(PLAYBOOK).unwrap())
    }

    fn part(r: &AnswerRef) -> String {
        format!(
            "{}:{}:{}",
            r.node.as_deref().unwrap_or("-"),
            match r.visit {
                Some(Visit::Nth(n)) => n.to_string(),
                Some(Visit::Default) => "default".into(),
                None => "-".into(),
            },
            r.part
        )
    }

    /// Attempts are counted per visit by the book: attempt 2 of visit 1 is
    /// the second scripted attempt, a later visit starts at 1 again, and a
    /// default answers a visit with no numbered entry without ever being
    /// extra. An attempt answered beyond the script is no answer.
    #[test]
    fn attempts_are_counted_per_visit_and_a_default_is_never_extra() {
        let mut b = book(
            "answers:\n  work:\n    - { visit: 1, attempts: [ { status: failed, output: a }, { status: succeeded, output: b } ] }\n    - { visit: default, attempts: [ { status: succeeded, output: d } ] }\n",
        );
        let (k, t) = b.attempt("work", 1, None);
        assert_eq!((k, t.unwrap().answer.output.as_deref()), (1, Some("a")));
        let (k, t) = b.attempt("work", 1, None);
        assert_eq!((k, t.unwrap().answer.output.as_deref()), (2, Some("b")));
        assert!(b.attempt("work", 1, None).1.is_none(), "no third attempt");
        let (k, t) = b.attempt("work", 2, None);
        assert_eq!((k, t.unwrap().answer.output.as_deref()), (1, Some("d")));
        let use_ = b.usage();
        assert!(use_.extra.is_empty(), "{:?}", use_.extra);
        assert_eq!(
            use_.defaults.iter().map(part).collect::<Vec<_>>(),
            ["work:default:attempt 1"]
        );
    }

    /// An answer for an attempt that never ran, a visit that never came, or
    /// an item that never ran is extra: this is how a wrong retry count,
    /// loop cap or route shows even when the outcome is right.
    #[test]
    fn answers_never_used_are_extra_per_attempt_visit_and_item() {
        let mut b = book(
            "answers:\n  work:\n    - { visit: 1, attempts: [ { status: succeeded }, { status: succeeded } ] }\n    - { visit: 3, script: { exit: 0 } }\n    - { visit: 2, items: { a.md: [ { status: succeeded } ], b.md: { status: succeeded } } }\n",
        );
        b.attempt("work", 1, None);
        b.attempt("work", 2, Some("a.md"));
        let extra: Vec<String> = b.usage().extra.iter().map(part).collect();
        assert_eq!(
            extra,
            [
                "work:1:attempt 2",
                "work:3:script",
                "work:2:item b.md attempt 1"
            ]
        );
    }

    /// A list item takes its own answers, else `"*"`; one answer for an
    /// item answers every retry of it.
    #[test]
    fn an_item_takes_its_own_answers_then_the_star() {
        let mut b = book(
            "answers:\n  work:\n    - visit: 1\n      items:\n        a.md: [ { status: failed, output: a1 }, { status: succeeded, output: a2 } ]\n        \"*\": { status: succeeded, output: any }\n",
        );
        assert_eq!(
            b.attempt("work", 1, Some("a.md"))
                .1
                .unwrap()
                .answer
                .output
                .as_deref(),
            Some("a1")
        );
        assert_eq!(
            b.attempt("work", 1, Some("a.md"))
                .1
                .unwrap()
                .answer
                .output
                .as_deref(),
            Some("a2")
        );
        assert_eq!(
            b.attempt("work", 1, Some("z.md"))
                .1
                .unwrap()
                .answer
                .output
                .as_deref(),
            Some("any")
        );
        assert_eq!(
            b.attempt("work", 1, Some("z.md"))
                .1
                .unwrap()
                .answer
                .output
                .as_deref(),
            Some("any")
        );
    }

    /// An interactive question is part of its attempt: after the attempt
    /// asks, the next hand-out is its `then`, with the same attempt
    /// number, and the attempt after that is the next scripted one.
    #[test]
    fn a_question_keeps_its_attempt_and_hands_out_then() {
        let mut b = book(
            "answers:\n  work:\n    - visit: 1\n      attempts:\n        - { question: { text: Which, answer: pg }, then: { status: succeeded, output: done } }\n        - { status: succeeded, output: second }\n",
        );
        let (k, first) = b.attempt("work", 1, None);
        assert_eq!(k, 1);
        assert!(first.unwrap().answer.question.is_some());
        assert_eq!(
            b.question("work", 1, None).unwrap().answer.as_deref(),
            Some("pg")
        );
        let (k, then) = b.attempt("work", 1, None);
        assert_eq!(
            (k, then.unwrap().answer.output.as_deref()),
            (1, Some("done"))
        );
        let (k, next) = b.attempt("work", 1, None);
        assert_eq!(
            (k, next.unwrap().answer.output.as_deref()),
            (2, Some("second"))
        );
        assert!(b.usage().extra.is_empty());
    }

    /// Judge edge answers are keyed by address in the file and handed out
    /// by the rank the engine asks under; an address of no outgoing edge
    /// of the node stays extra.
    #[test]
    fn judge_edges_resolve_addresses_to_ranks() {
        let mut b = book(
            "answers:\n  work:\n    - visit: 1\n      judge_edges: { \"id:needs-fix\": { p: 0.8 }, \"id:nowhere\": { p: 0.1 } }\n",
        );
        let t = b.judge_edges("work", 1).unwrap();
        assert_eq!(t.answer.keys().collect::<Vec<_>>(), ["edge_0"]);
        assert_eq!(t.answer["edge_0"].p, Some(0.8));
        let extra: Vec<String> = b.usage().extra.iter().map(part).collect();
        assert_eq!(extra, ["work:1:edge id:nowhere"]);
    }

    /// Missing answers are kept in order and named with their key.
    #[test]
    fn a_missing_answer_is_kept_and_described() {
        let mut b = book("answers: {}\n");
        assert!(b.attempt("work", 1, None).1.is_none());
        b.record_missing(MissingAnswer {
            boundary: "agent_attempt".into(),
            node: Some("work".into()),
            visit: 1,
            attempt: Some(1),
            item: None,
            part: None,
        });
        let m = b.usage().missing;
        assert_eq!(
            m[0].describe(),
            "scenario has no answer for agent_attempt of `work` visit 1 attempt 1"
        );
    }
}
