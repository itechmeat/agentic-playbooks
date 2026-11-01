//! The open waits of a run and its live children, the one stop verdict over
//! all of them, the primary order, and what a caller that follows a run's
//! automatic progress does with the verdict.
//!
//! The verdict:
//! 1. the observed run ended: `finished`;
//! 2. any open wait needs a person, a host or the supervisor (anything but
//!    an assured successful auto outcome under a live driver, and every
//!    wait whose auto outcome is a failure): `needs_input`;
//! 3. otherwise the run or a live child is paused or has a dead driver:
//!    `stopped`;
//! 4. otherwise: `progresses_by_itself`.
//!
//! Several open waits never hide each other: the primary is chosen by kind
//! (publish, review, question without a default, supervisor, host task,
//! webhook wait, question with a default, timer wait, resume), then by the
//! request seq, then by run (the parent before its children, children in
//! start order).

use serde::Serialize;

use super::deadlines::{self, Clock};
use super::observation::Observation;
use super::types::{
    ActionArg, ActionCode, ActionTarget, ArgValue, NextAction, Principal, RequestKind, RequestRef,
    RunActions, StopVerdict,
};
use crate::event::{Event, EventPayload};
use crate::state::RunStatus;

/// The order of the primary action, earliest first.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Kind {
    Publish,
    Review,
    Question,
    Supervisor,
    HostTask,
    Webhook,
    QuestionWithDefault,
    Timer,
    Resume,
    Intent,
    Report,
}

/// One action with what the verdict and the primary order need to know.
#[derive(Debug, Clone)]
pub(super) struct Wait {
    kind: Kind,
    seq: u64,
    run_order: usize,
    /// It needs a person, a host or the supervisor.
    needs_someone: bool,
    pub(super) action: NextAction,
}

/// Every action of `obs` and its live children, in primary order.
pub(super) fn collect(obs: &Observation) -> Vec<Wait> {
    let mut out = Vec::new();
    if obs.status().is_terminal() {
        finished(obs, &mut out);
    } else {
        let runs = obs.runs();
        let mut tasks_seen = std::collections::BTreeSet::new();
        for (order, run) in runs.iter().enumerate() {
            let cx = RunCx {
                run,
                order,
                child_of: child_node(obs, &run.run_id),
                clock: Clock {
                    now_ms: obs.now_ms,
                    paused: run.status() == RunStatus::Paused,
                    driver_alive: run.driver_alive(),
                },
            };
            cx.gates(&mut out);
            cx.host_tasks(&runs, &mut tasks_seen, &mut out);
            cx.resume(&mut out);
        }
    }
    // Seqs are per journal: they order waits within one run only.
    out.sort_by_key(|w| (w.kind, w.run_order, w.seq));
    out
}

/// The verdict over `waits`, the actions of `obs`.
pub(super) fn stop_verdict(obs: &Observation, waits: &[Wait]) -> StopVerdict {
    if obs.status().is_terminal() {
        StopVerdict::Finished
    } else if waits.iter().any(|w| w.needs_someone) {
        StopVerdict::NeedsInput
    } else if obs.runs().iter().any(|r| r.stopped()) {
        StopVerdict::Stopped
    } else {
        StopVerdict::ProgressesByItself
    }
}

/// A run stopped only by one pause with `until`: no other run is stopped
/// and nothing but its resume, and timers that finish by themselves once it
/// resumes, is open.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TimedPause {
    pub run_id: String,
    /// The journal seq of the `run_paused`; a new pause is a new seq.
    pub seq: u64,
    pub until_ms: u64,
}

/// How long past a pause's `until` a follower keeps waiting for the waker.
pub const UNTIL_GRACE_MS: u64 = 60_000;

/// The pause with `until` that alone stops `obs`, if that is the case.
pub fn sole_timed_pause(obs: &Observation, actions: &RunActions) -> Option<TimedPause> {
    let by_itself = |a: &NextAction| {
        a.code == ActionCode::ResumeRun
            || (a.code == ActionCode::WaitAgain && a.auto.is_some_and(|p| p.outcome().is_success()))
    };
    if actions.stop != StopVerdict::Stopped || !actions.actions.iter().all(by_itself) {
        return None;
    }
    let runs = obs.runs();
    let [run] = runs.iter().filter(|r| r.stopped()).collect::<Vec<_>>()[..] else {
        return None;
    };
    Some(TimedPause {
        run_id: run.run_id.clone(),
        seq: run.pause_seq?,
        until_ms: run.view.pause()?.until?,
    })
}

/// The exit code of a wait, shared by the default wait and the one that
/// follows automatic progress.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum WaitExit {
    Succeeded,
    Failed,
    Error,
    NeedsInput,
    Stopped,
    Timeout,
}

impl WaitExit {
    pub fn code(self) -> u8 {
        match self {
            WaitExit::Succeeded => 0,
            WaitExit::Failed => 1,
            WaitExit::Error => 2,
            WaitExit::NeedsInput => 3,
            WaitExit::Stopped => 4,
            WaitExit::Timeout => 5,
        }
    }

    /// The exit of a run that ended with `status`.
    pub fn of_finished(status: RunStatus) -> Self {
        if status == RunStatus::Succeeded {
            WaitExit::Succeeded
        } else {
            WaitExit::Failed
        }
    }
}

/// What a caller that follows a run's automatic progress does now.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FollowAuto {
    KeepWaiting,
    /// Returns, with this exit, once the condition held for the grace.
    Return(WaitExit),
}

/// The follow rule: keep waiting while the run progresses by itself, or
/// while one pause with `until` alone stops it, up to `until` plus
/// [`UNTIL_GRACE_MS`] for that pause; otherwise return with the exit of the
/// aggregated verdict: 0 or 1 when finished, 3 when it needs input (a
/// paused run with an open gate included), 4 when stopped.
pub fn follow_auto(obs: &Observation, actions: &RunActions) -> FollowAuto {
    match actions.stop {
        StopVerdict::Finished => FollowAuto::Return(WaitExit::of_finished(actions.status)),
        StopVerdict::NeedsInput => FollowAuto::Return(WaitExit::NeedsInput),
        StopVerdict::ProgressesByItself => FollowAuto::KeepWaiting,
        StopVerdict::Stopped => match sole_timed_pause(obs, actions) {
            Some(p) if obs.now_ms < p.until_ms.saturating_add(UNTIL_GRACE_MS) => {
                FollowAuto::KeepWaiting
            }
            _ => FollowAuto::Return(WaitExit::Stopped),
        },
    }
}

/// The node of the parent that started `run_id`, for a child.
fn child_node(obs: &Observation, run_id: &str) -> Option<String> {
    fn find(o: &Observation, run_id: &str) -> Option<String> {
        o.children.iter().find_map(|c| {
            if c.observation.run_id == run_id {
                Some(c.node_id.clone())
            } else {
                find(&c.observation, run_id)
            }
        })
    }
    find(obs, run_id)
}

/// The actions of a run that ended: resolving each send nobody can tell
/// went out, and reading the report.
fn finished(obs: &Observation, out: &mut Vec<Wait>) {
    for intent in &obs.open_intents {
        out.push(Wait {
            kind: Kind::Intent,
            seq: 0,
            run_order: 0,
            needs_someone: false,
            action: NextAction {
                code: ActionCode::ResolveIntent,
                run_id: obs.run_id.clone(),
                target: ActionTarget {
                    node: Some(intent.node.clone()),
                    ..ActionTarget::default()
                },
                args: vec![
                    given("intent", &intent.id),
                    choice("decision", &["sent", "not_sent"], None),
                ],
                principals: vec![Principal::Person],
                ready: true,
                not_ready: None,
                auto: None,
                command: None,
                reason: format!(
                    "nobody can tell whether the send of intent `{}` ({}) went out; check the service and record what you found",
                    intent.id,
                    intent.target()
                ),
            },
        });
    }
    out.push(Wait {
        kind: Kind::Report,
        seq: 0,
        run_order: 0,
        needs_someone: false,
        action: NextAction {
            code: ActionCode::ReadReport,
            run_id: obs.run_id.clone(),
            target: ActionTarget::default(),
            args: Vec::new(),
            principals: everyone(),
            ready: true,
            not_ready: None,
            auto: None,
            command: None,
            reason: format!(
                "the run finished ({}); read its report",
                obs.status().as_str()
            ),
        },
    });
}

/// One observed run while the actions are collected.
struct RunCx<'a> {
    run: &'a Observation,
    order: usize,
    child_of: Option<String>,
    clock: Clock,
}

impl RunCx<'_> {
    fn events(&self) -> &[Event] {
        &self.run.view.events
    }

    fn reason(&self, line: String) -> String {
        match &self.child_of {
            Some(node) => format!("sub-playbook node `{node}`: {line}"),
            None => line,
        }
    }

    fn push(&self, out: &mut Vec<Wait>, kind: Kind, seq: u64, needs_someone: bool, a: NextAction) {
        out.push(Wait {
            kind,
            seq,
            run_order: self.order,
            needs_someone,
            action: a,
        });
    }

    fn action(&self, code: ActionCode, target: ActionTarget, line: String) -> NextAction {
        NextAction {
            code,
            run_id: self.run.run_id.clone(),
            target,
            args: Vec::new(),
            principals: Vec::new(),
            ready: true,
            not_ready: None,
            auto: None,
            command: None,
            reason: self.reason(line),
        }
    }

    /// Reviews, publish gates, questions, the supervisor park and `wait`
    /// nodes.
    fn gates(&self, out: &mut Vec<Wait>) {
        let Some(p) = self.run.view.progress.as_ref() else {
            return;
        };
        for r in &p.pending_reviews {
            self.review(r, out);
        }
        for q in &p.pending_questions {
            self.question(q, out);
        }
        if let Some(s) = &p.pending_supervisor {
            self.supervisor(s, out);
        }
        for node in &p.pending_waits {
            self.wait_node(node, out);
        }
    }

    fn review(&self, r: &crate::progress::PendingReview, out: &mut Vec<Wait>) {
        let seq = last_seq(
            self.events(),
            |p| matches!(p, EventPayload::ReviewRequested { node, .. } if *node == r.node),
        )
        .unwrap_or(0);
        let node = Some(r.node.clone());
        let (kind, a) = match &r.publish {
            Some(plan) => {
                let mut a = self.action(
                    ActionCode::DecidePublish,
                    ActionTarget {
                        node,
                        request: Some(RequestRef {
                            kind: RequestKind::Publish,
                            seq,
                            nonce: Some(plan.nonce.clone()),
                        }),
                        ..ActionTarget::default()
                    },
                    format!(
                        "node `{}` waits for a person's consent to its exact publish plan",
                        r.node
                    ),
                );
                a.args = vec![
                    choice("decision", &apb_core::publish::OPTIONS, None),
                    given("nonce", &plan.nonce),
                ];
                a.principals = vec![Principal::Person];
                (Kind::Publish, a)
            }
            None => {
                let mut a = self.action(
                    ActionCode::DecideReview,
                    ActionTarget {
                        node,
                        request: Some(RequestRef {
                            kind: RequestKind::Review,
                            seq,
                            nonce: None,
                        }),
                        ..ActionTarget::default()
                    },
                    format!("node `{}` waits for a human review decision", r.node),
                );
                let options: Vec<&str> = r.options.iter().map(String::as_str).collect();
                a.args = vec![
                    choice("decision", &options, None),
                    optional("note", "a note"),
                ];
                a.principals = vec![Principal::Person, Principal::Operator];
                (Kind::Review, a)
            }
        };
        self.push(out, kind, seq, true, a);
    }

    fn question(&self, q: &crate::progress::PendingQuestion, out: &mut Vec<Wait>) {
        // A question the drive has not journaled yet has no request to
        // answer: it gets its action on the next observation.
        let Some(seq) = open_question_seq(self.events(), &q.node) else {
            return;
        };
        let timeout = self.run.playbook.as_ref().and_then(|pb| {
            pb.node(&q.node).and_then(|n| match &n.kind {
                apb_core::schema::NodeKind::AgentTask {
                    question_timeout_seconds,
                    ..
                } => *question_timeout_seconds,
                _ => None,
            })
        });
        let auto = deadlines::question(
            self.clock,
            self.events(),
            &q.node,
            timeout,
            q.default_answer.is_some(),
        );
        let mut a = self.action(
            ActionCode::AnswerQuestion,
            ActionTarget {
                node: Some(q.node.clone()),
                request: Some(RequestRef {
                    kind: RequestKind::Question,
                    seq,
                    nonce: None,
                }),
                ..ActionTarget::default()
            },
            match &q.default_answer {
                Some(_) => format!(
                    "node `{}` asks a question; without an answer its default is posted at the timeout",
                    q.node
                ),
                None => format!("node `{}` asks a question", q.node),
            },
        );
        a.args = vec![if q.options.is_empty() {
            ActionArg {
                name: "answer".into(),
                value: ArgValue::Supplied {
                    hint: "your answer".into(),
                },
                required: true,
            }
        } else {
            let options: Vec<&str> = q.options.iter().map(String::as_str).collect();
            choice("answer", &options, q.default_answer.as_deref())
        }];
        a.principals = if q.answer_by == "supervisor" {
            vec![
                Principal::Supervisor,
                Principal::Person,
                Principal::Operator,
            ]
        } else {
            vec![Principal::Person, Principal::Operator]
        };
        a.auto = auto;
        let with_default = auto.is_some_and(|p| p.outcome().is_success());
        let kind = if with_default {
            Kind::QuestionWithDefault
        } else {
            Kind::Question
        };
        let needs = !auto.is_some_and(|p| p.assured_success());
        self.push(out, kind, seq, needs, a);
    }

    fn supervisor(&self, s: &crate::progress::PendingSupervisor, out: &mut Vec<Wait>) {
        let seq = last_seq(self.events(), |p| {
            matches!(p, EventPayload::WakeRaised { .. })
        })
        .unwrap_or(0);
        let target = || ActionTarget {
            node: Some(s.node.clone()),
            ..ActionTarget::default()
        };
        let actions = [
            (
                ActionCode::SupervisorRetryNode,
                vec![given("node", &s.node)],
                format!(
                    "node `{}` stopped ({}); the supervisor may retry it",
                    s.node, s.trigger
                ),
            ),
            (
                ActionCode::SupervisorContinueFrom,
                vec![supplied("node", "the node to continue from")],
                format!(
                    "node `{}` stopped ({}); the supervisor may continue from another node",
                    s.node, s.trigger
                ),
            ),
            (
                ActionCode::SupervisorAbort,
                Vec::new(),
                format!(
                    "node `{}` stopped ({}); the supervisor may abort the run",
                    s.node, s.trigger
                ),
            ),
        ];
        for (code, args, line) in actions {
            let mut a = self.action(code, target(), line);
            a.args = args;
            a.principals = vec![Principal::Supervisor];
            self.push(out, Kind::Supervisor, seq, true, a);
        }
    }

    fn wait_node(&self, node: &str, out: &mut Vec<Wait>) {
        let Some((wait_for, timeout)) = self.run.playbook.as_ref().and_then(|pb| {
            pb.node(node).and_then(|n| match &n.kind {
                apb_core::schema::NodeKind::Wait {
                    wait_for,
                    timeout_seconds,
                    ..
                } => Some((wait_for.clone(), *timeout_seconds)),
                _ => None,
            })
        }) else {
            return;
        };
        let seq = last_seq(
            self.events(),
            |p| matches!(p, EventPayload::WaitStarted { node: n, .. } if n == node),
        )
        .unwrap_or(0);
        let auto = deadlines::wait_node(self.clock, self.events(), node, &wait_for, timeout);
        let webhook = matches!(wait_for, apb_core::schema::WaitFor::Webhook { .. });
        let mut a = self.action(
            ActionCode::WaitAgain,
            ActionTarget {
                node: Some(node.to_string()),
                ..ActionTarget::default()
            },
            if webhook {
                format!("node `{node}` waits for its webhook signal")
            } else {
                format!("node `{node}` waits for its timer")
            },
        );
        a.principals = everyone();
        a.auto = Some(auto);
        let (kind, needs) = if webhook {
            (Kind::Webhook, true)
        } else {
            (Kind::Timer, false)
        };
        self.push(out, kind, seq, needs, a);
    }

    /// The host tasks of this run, and those of runs the observation did
    /// not reach (a child past the depth cap), each once.
    fn host_tasks(
        &self,
        runs: &[&Observation],
        seen: &mut std::collections::BTreeSet<(String, String)>,
        out: &mut Vec<Wait>,
    ) {
        let Some(p) = self.run.view.progress.as_ref() else {
            return;
        };
        for t in &p.pending_tasks {
            let observed_elsewhere =
                t.run_id != self.run.run_id && runs.iter().any(|r| r.run_id == t.run_id);
            if observed_elsewhere || !seen.insert((t.run_id.clone(), t.task_id.clone())) {
                continue;
            }
            let mut a = self.action(
                ActionCode::SubmitHostTask,
                ActionTarget {
                    node: Some(t.node.clone()),
                    task_id: Some(t.task_id.clone()),
                    ..ActionTarget::default()
                },
                format!(
                    "host task `{}` of node `{}` waits for the host to execute it",
                    t.task_id, t.node
                ),
            );
            a.run_id = t.run_id.clone();
            a.args = vec![
                choice("status", &["succeeded", "failed", "blocked"], None),
                supplied("output", "the file holding the reply"),
            ];
            a.principals = vec![Principal::Host];
            a.auto = deadlines::host_task(self.clock, t.deadline);
            self.push(out, Kind::HostTask, 0, true, a);
        }
    }

    /// The resume of a paused run or of one whose driver is dead.
    fn resume(&self, out: &mut Vec<Wait>) {
        if !self.run.stopped() {
            return;
        }
        let pause = self.run.view.pause();
        let budget = pause.and_then(|p| p.cause) == Some(crate::pause::PauseCause::Budget);
        let a = if budget {
            let mut a = self.action(
                ActionCode::ResumeWithBudget,
                ActionTarget::default(),
                "the run is paused by its budget; resume it with a changed budget or let the paused node run once".into(),
            );
            a.args = vec![supplied("raise_budget_tokens", "tokens to add")];
            a.principals = vec![Principal::Person, Principal::Operator];
            a
        } else {
            let line = if self.run.status() == RunStatus::Paused {
                "the run is paused; resume it".to_string()
            } else {
                "the run's driver is gone; resume it".to_string()
            };
            let mut a = self.action(ActionCode::ResumeRun, ActionTarget::default(), line);
            a.principals = vec![Principal::Person, Principal::Operator, Principal::Host];
            a.auto = deadlines::pause_until(pause.and_then(|p| p.until));
            a
        };
        self.push(out, Kind::Resume, self.run.pause_seq.unwrap_or(0), false, a);
    }
}

/// The seq of the `question_asked` of `node` that is still open in the
/// journal: asked more often than answered. `None` before the drive
/// journaled the channel's question.
pub(crate) fn open_question_seq(events: &[Event], node: &str) -> Option<u64> {
    let asked = events
        .iter()
        .filter(|e| matches!(&e.payload, EventPayload::QuestionAsked { node: n, .. } if n == node))
        .count();
    let answered = events
        .iter()
        .filter(
            |e| matches!(&e.payload, EventPayload::QuestionAnswered { node: n, .. } if n == node),
        )
        .count();
    (asked > answered)
        .then(|| {
            last_seq(
                events,
                |p| matches!(p, EventPayload::QuestionAsked { node: n, .. } if n == node),
            )
        })
        .flatten()
}

fn last_seq(events: &[Event], pick: impl Fn(&EventPayload) -> bool) -> Option<u64> {
    events
        .iter()
        .rev()
        .find(|e| pick(&e.payload))
        .map(|e| e.seq)
}

fn everyone() -> Vec<Principal> {
    vec![
        Principal::Person,
        Principal::Operator,
        Principal::Host,
        Principal::Supervisor,
    ]
}

fn given(name: &str, value: &str) -> ActionArg {
    ActionArg {
        name: name.into(),
        value: ArgValue::Given {
            value: value.into(),
        },
        required: true,
    }
}

fn choice(name: &str, options: &[&str], default: Option<&str>) -> ActionArg {
    ActionArg {
        name: name.into(),
        value: ArgValue::Choice {
            options: options.iter().map(|o| o.to_string()).collect(),
            default: default.map(str::to_string),
        },
        required: true,
    }
}

fn supplied(name: &str, hint: &str) -> ActionArg {
    ActionArg {
        name: name.into(),
        value: ArgValue::Supplied { hint: hint.into() },
        required: true,
    }
}

fn optional(name: &str, hint: &str) -> ActionArg {
    ActionArg {
        required: false,
        ..supplied(name, hint)
    }
}
