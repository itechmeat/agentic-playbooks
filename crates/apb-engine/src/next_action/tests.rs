//! One table test per wait kind: the actions, the primary, the verdict,
//! the default wait's exit (unchanged) and the exit of a wait that follows
//! automatic progress; then the compositions, the paused clock and the
//! children.

use std::path::{Path, PathBuf};
use std::time::Duration;

use serde_json::json;

use super::verdict::WaitExit;
use super::*;
use crate::event::{EventLog, EventPayload};
use crate::run_wait::{FollowAutoWaiter, wait_run_with};

const PLAYBOOK: &str = r#"
schema: 2
id: p
name: p
version: 1.0.0
defaults: { profile: x }
nodes:
  - { id: s, type: start }
  - { id: ask, type: agent_task, prompt: hi, interactive: true, question_timeout_seconds: 600, default_answer: go }
  - { id: ask_nodef, type: agent_task, prompt: hi, interactive: true, question_timeout_seconds: 600 }
  - { id: ask_free, type: agent_task, prompt: hi, interactive: true }
  - { id: gate, type: human_review, options: [approved, rejected] }
  - { id: timer, type: wait, wait_for: { type: timer, seconds: 600 }, timeout_seconds: 1200 }
  - { id: hook, type: wait, wait_for: { type: webhook, key: k }, timeout_seconds: 600 }
  - { id: work, type: agent_task, prompt: hi }
  - { id: kid, type: playbook, playbook: { id: c } }
  - { id: f, type: finish, outcome: success }
edges:
  - { from: s, to: ask }
  - { from: ask, to: ask_nodef }
  - { from: ask_nodef, to: ask_free }
  - { from: ask_free, to: gate }
  - { from: gate, to: timer }
  - { from: timer, to: hook }
  - { from: hook, to: work }
  - { from: work, to: kid }
  - { from: kid, to: f }
"#;

/// A project with runs written by hand.
struct Fx {
    tmp: tempfile::TempDir,
}

/// One run of a [`Fx`].
struct Run {
    dir: PathBuf,
    log: EventLog,
}

impl Fx {
    fn new() -> Self {
        Self {
            tmp: tempfile::tempdir().unwrap(),
        }
    }

    fn root(&self) -> &Path {
        self.tmp.path()
    }

    /// A started run with the test playbook and a live driver.
    fn run(&self, id: &str) -> Run {
        let dir = self.root().join(".apb/runs").join(id);
        let mut log = EventLog::create(&dir).unwrap();
        std::fs::write(dir.join("playbook.yaml"), PLAYBOOK).unwrap();
        log.append(EventPayload::RunStarted {
            playbook: "p".into(),
            version: "1.0.0".into(),
            definition: None,
        })
        .unwrap();
        let run = Run { dir, log };
        run.driver(std::process::id());
        run
    }

    fn observe(&self, id: &str) -> RunActions {
        let dir = self.root().join(".apb/runs").join(id);
        next_actions(&Observation::load(&dir, id).unwrap())
    }

    /// The default wait's exit; 5 when it keeps waiting.
    fn default_exit(&self, id: &str) -> u8 {
        let r = wait_run_with(
            self.root(),
            id,
            Duration::from_millis(300),
            Duration::from_millis(20),
            Duration::from_millis(10),
        )
        .unwrap();
        r.reason.exit(r.status).code()
    }

    /// The following wait's exit; 5 when it keeps waiting.
    fn follow_exit(&self, id: &str) -> u8 {
        FollowAutoWaiter::new(self.root(), id)
            .unwrap()
            .with_timing(Duration::from_millis(20), Duration::from_millis(10))
            .wait(Duration::from_millis(300))
            .unwrap()
            .exit
            .code()
    }
}

impl Run {
    fn driver(&self, pid: u32) {
        std::fs::write(crate::driver::driver_pid_path(&self.dir), pid.to_string()).unwrap();
    }

    fn dead_driver(&self) {
        let mut child = std::process::Command::new("sh")
            .arg("-c")
            .arg("exit 0")
            .spawn()
            .unwrap();
        let pid = child.id();
        child.wait().unwrap();
        self.driver(pid);
    }

    fn ev(&mut self, v: serde_json::Value) -> u64 {
        self.log
            .append(serde_json::from_value(v).unwrap())
            .unwrap()
            .seq
    }

    fn started(&mut self, node: &str) {
        self.ev(json!({"type": "node_started", "node": node, "attempt": 1}));
    }

    /// A question of `node` in the channel and the journal; its seq.
    fn question(&mut self, node: &str) -> u64 {
        self.started(node);
        crate::question::post_question(&self.dir, node, 1, "go on?", vec![]).unwrap();
        self.ev(json!({"type": "question_asked", "node": node, "attempt": 1, "question": "go on?", "options": []}))
    }

    fn review(&mut self) -> u64 {
        self.started("gate");
        self.ev(json!({"type": "review_requested", "node": "gate", "options": ["approved", "rejected"]}))
    }

    fn wait_node(&mut self, node: &str, kind: &str) -> u64 {
        self.started(node);
        self.ev(json!({"type": "wait_started", "node": node, "kind": kind}))
    }

    fn pause(&mut self, until: Option<u64>) -> u64 {
        let p = crate::pause::PauseRequest {
            reason: Some("later".into()),
            by: Some(crate::pause::PauseBy::Operator),
            mode: crate::pause::PauseMode::Drain,
            until,
            keep_lock: false,
        };
        self.log.append(p.to_event()).unwrap().seq
    }

    fn finish(&mut self, outcome: &str) {
        self.ev(json!({"type": "run_finished", "outcome": outcome}));
    }

    /// A child run started by node `kid`, driven by this run.
    fn child(&mut self, fx: &Fx, id: &str) -> Run {
        self.started("kid");
        self.ev(json!({"type": "child_run_started", "node_id": "kid", "run_id": id}));
        fx.run(id)
    }
}

fn now() -> u64 {
    apb_core::clock::now_ms_u64()
}

fn codes(a: &RunActions) -> Vec<ActionCode> {
    a.actions.iter().map(|x| x.code).collect()
}

fn primary(a: &RunActions) -> &NextAction {
    a.primary().expect("a primary action")
}

// --- one test per row --------------------------------------------------------

#[test]
fn a_question_with_a_default_under_a_live_driver_progresses_by_itself() {
    let fx = Fx::new();
    let mut r = fx.run("r");
    let seq = r.question("ask");
    let a = fx.observe("r");
    let p = primary(&a);
    assert_eq!(p.code, ActionCode::AnswerQuestion);
    assert_eq!(
        p.target.request.as_ref().map(|q| (q.kind, q.seq)),
        Some((RequestKind::Question, seq))
    );
    assert!(matches!(
        p.auto,
        Some(AutoProgress::At {
            outcome: AutoOutcome::AnsweredDefault,
            assured: true,
            ..
        })
    ));
    assert_eq!(a.stop, StopVerdict::ProgressesByItself);
    assert_eq!(fx.default_exit("r"), 3);
    assert_eq!(fx.follow_exit("r"), 5, "keeps waiting");
}

#[test]
fn a_question_with_a_timeout_and_no_default_needs_input() {
    let fx = Fx::new();
    let mut r = fx.run("r");
    r.question("ask_nodef");
    let a = fx.observe("r");
    assert!(matches!(
        primary(&a).auto,
        Some(AutoProgress::At {
            outcome: AutoOutcome::AttemptFails,
            ..
        })
    ));
    assert_eq!(a.stop, StopVerdict::NeedsInput);
    assert_eq!((fx.default_exit("r"), fx.follow_exit("r")), (3, 3));
}

#[test]
fn a_question_without_a_timeout_needs_input() {
    let fx = Fx::new();
    let mut r = fx.run("r");
    r.question("ask_free");
    let a = fx.observe("r");
    let p = primary(&a);
    assert_eq!((p.code, p.auto), (ActionCode::AnswerQuestion, None));
    assert_eq!(p.principals, vec![Principal::Person, Principal::Operator]);
    assert_eq!(a.stop, StopVerdict::NeedsInput);
    assert_eq!((fx.default_exit("r"), fx.follow_exit("r")), (3, 3));
}

#[test]
fn a_human_review_needs_input_with_its_options_and_request() {
    let fx = Fx::new();
    let mut r = fx.run("r");
    let seq = r.review();
    let a = fx.observe("r");
    let p = primary(&a);
    assert_eq!(p.code, ActionCode::DecideReview);
    assert_eq!(p.target.request.as_ref().map(|q| q.seq), Some(seq));
    assert!(
        matches!(&p.args[0].value, ArgValue::Choice { options, .. } if options == &["approved", "rejected"])
    );
    assert_eq!((fx.default_exit("r"), fx.follow_exit("r")), (3, 3));
}

#[test]
fn a_supervisor_park_offers_the_supervisor_its_three_actions() {
    let fx = Fx::new();
    let mut r = fx.run("r");
    r.started("work");
    r.ev(
        json!({"type": "wake_raised", "trigger": "node_failed", "node": "work", "detail": "boom"}),
    );
    let a = fx.observe("r");
    assert_eq!(
        codes(&a),
        vec![
            ActionCode::SupervisorRetryNode,
            ActionCode::SupervisorContinueFrom,
            ActionCode::SupervisorAbort
        ]
    );
    assert!(
        a.actions
            .iter()
            .all(|x| x.principals == vec![Principal::Supervisor])
    );
    assert_eq!((fx.default_exit("r"), fx.follow_exit("r")), (3, 3));
}

#[test]
fn a_host_task_is_the_host_s_with_its_deadline() {
    let fx = Fx::new();
    let mut r = fx.run("r");
    r.started("work");
    let deadline = now() + 600_000;
    let record = crate::host_task::TaskRecord {
        task_id: "work-1".into(),
        node: "work".into(),
        attempt: 1,
        workdir: ".".into(),
        skills: vec![],
        outputs: None,
        deadline_ms: Some(deadline),
        model_hint: None,
        hint_source: None,
        fallback_of: None,
        env: Default::default(),
        requested_ts: now(),
    };
    crate::host_task::write_task(&r.dir, &record, "do it", None).unwrap();
    r.ev(json!({"type": "host_task_requested", "task_id": "work-1", "node": "work", "attempt": 1}));
    let a = fx.observe("r");
    let p = primary(&a);
    assert_eq!(
        (p.code, p.target.task_id.as_deref()),
        (ActionCode::SubmitHostTask, Some("work-1"))
    );
    assert_eq!(p.principals, vec![Principal::Host]);
    assert!(matches!(
        p.auto,
        Some(AutoProgress::At {
            outcome: AutoOutcome::AttemptFails,
            ..
        })
    ));
    assert_eq!((fx.default_exit("r"), fx.follow_exit("r")), (3, 3));
}

#[test]
fn a_timer_wait_progresses_by_itself_and_a_webhook_wait_needs_input() {
    let fx = Fx::new();
    let mut r = fx.run("t");
    r.wait_node("timer", "timer");
    let a = fx.observe("t");
    assert_eq!(primary(&a).code, ActionCode::WaitAgain);
    assert!(matches!(
        primary(&a).auto,
        Some(AutoProgress::At {
            outcome: AutoOutcome::TimerElapses,
            assured: true,
            ..
        })
    ));
    assert_eq!(a.stop, StopVerdict::ProgressesByItself);
    assert_eq!((fx.default_exit("t"), fx.follow_exit("t")), (5, 5));

    let mut r = fx.run("h");
    r.wait_node("hook", "webhook");
    let a = fx.observe("h");
    assert!(matches!(
        primary(&a).auto,
        Some(AutoProgress::At {
            outcome: AutoOutcome::WaitTimesOut,
            ..
        })
    ));
    assert_eq!(a.stop, StopVerdict::NeedsInput);
    assert_eq!((fx.default_exit("h"), fx.follow_exit("h")), (5, 3));
}

#[test]
fn a_pause_with_until_alone_is_stopped_and_followed_until_its_grace() {
    let fx = Fx::new();
    let mut r = fx.run("r");
    r.pause(Some(now() + 3_600_000));
    let a = fx.observe("r");
    let p = primary(&a);
    assert_eq!(p.code, ActionCode::ResumeRun);
    assert!(matches!(
        p.auto,
        Some(AutoProgress::At {
            outcome: AutoOutcome::ResumeAttempt,
            assured: false,
            ..
        })
    ));
    assert_eq!(a.stop, StopVerdict::Stopped);
    assert_eq!((fx.default_exit("r"), fx.follow_exit("r")), (4, 5));

    // Past `until` plus the grace: stopped, 4.
    let mut r = fx.run("late");
    r.pause(Some(now() - verdict::UNTIL_GRACE_MS - 1_000));
    assert_eq!(fx.follow_exit("late"), 4);

    // A new pause seq restarts the grace: the old pause ran out, the new
    // one has not.
    let mut r = fx.run("again");
    r.pause(Some(now() - verdict::UNTIL_GRACE_MS - 1_000));
    r.ev(json!({"type": "run_resumed", "from_node": "work"}));
    let seq = r.pause(Some(now() - 1_000));
    let obs = Observation::load(&r.dir, "again").unwrap();
    let held = verdict::sole_timed_pause(&obs, &next_actions(&obs)).unwrap();
    assert_eq!(held.seq, seq);
    assert_eq!(fx.follow_exit("again"), 5, "inside the new pause's grace");

    // Paused inside a timer `wait`: the timer finishes by itself once the
    // waker resumes the run, so the pause is still the only stop.
    let mut r = fx.run("timer");
    r.wait_node("timer", "timer");
    r.pause(Some(now() + 3_600_000));
    let a = fx.observe("timer");
    assert!(codes(&a).contains(&ActionCode::WaitAgain), "{a:?}");
    assert_eq!(a.stop, StopVerdict::Stopped);
    assert_eq!(fx.follow_exit("timer"), 5, "followed through the pause");
}

impl Run {
    /// `work` failed once and its retry waits `delay_ms`.
    fn deferred(&mut self, delay_ms: u64) -> u64 {
        self.started("work");
        self.ev(
            json!({"type": "retry_deferred", "node": "work", "after_attempt": 1,
            "retry": 1, "step": 0, "binding": "b", "delay_ms": delay_ms}),
        )
    }
}

/// The action that starts `work`'s waiting retry now.
fn retry_now(a: &RunActions) -> &NextAction {
    a.actions
        .iter()
        .find(|x| x.code == ActionCode::RetryNow)
        .expect("a retry_now action")
}

#[test]
fn a_deferred_retry_under_a_live_driver_progresses_by_itself() {
    let fx = Fx::new();
    let mut r = fx.run("r");
    r.deferred(600_000);
    let a = fx.observe("r");
    assert_eq!(codes(&a), vec![ActionCode::RetryNow]);
    let p = primary(&a);
    assert_eq!(p.target.node.as_deref(), Some("work"));
    assert_eq!(
        p.principals,
        vec![
            Principal::Person,
            Principal::Operator,
            Principal::Supervisor
        ]
    );
    assert!(matches!(
        p.auto,
        Some(AutoProgress::At {
            outcome: AutoOutcome::RetryAttempt,
            assured: true,
            at_ms,
        }) if at_ms > now() + 500_000
    ));
    assert_eq!(
        p.command.as_deref(),
        None,
        "set only by for_surface; rendered below"
    );
    assert_eq!(
        command::render(p, &Default::default()).unwrap().line(),
        "apb resume --retry-now --node=work r"
    );
    assert_eq!(a.stop, StopVerdict::ProgressesByItself);
    assert_eq!((fx.default_exit("r"), fx.follow_exit("r")), (5, 5));
}

/// A retry that waits under a driver that is gone starts by nothing: no
/// automatic progress is promised, and its line says the driver is gone.
#[test]
fn a_deferred_retry_under_a_dead_driver_promises_no_start() {
    let fx = Fx::new();
    let mut r = fx.run("r");
    r.deferred(600_000);
    r.dead_driver();
    let a = fx.observe("r");
    let wait = retry_now(&a);
    assert_eq!(wait.auto, None, "{a:?}");
    assert!(wait.reason.contains("driver is gone"), "{}", wait.reason);
    assert_eq!(a.stop, StopVerdict::Stopped);
}

#[test]
fn a_deferred_retry_of_a_paused_run_is_frozen() {
    let fx = Fx::new();
    let mut r = fx.run("r");
    r.deferred(600_000);
    r.pause(None);
    let a = fx.observe("r");
    assert_eq!(primary(&a).code, ActionCode::ResumeRun);
    assert!(matches!(
        retry_now(&a).auto,
        Some(AutoProgress::Frozen {
            outcome: AutoOutcome::RetryAttempt,
            remaining_ms,
        }) if remaining_ms > 500_000 && remaining_ms <= 600_000
    ));
    assert_eq!(a.stop, StopVerdict::Stopped);
    assert_eq!((fx.default_exit("r"), fx.follow_exit("r")), (4, 4));
}

#[test]
fn a_released_run_is_stopped_and_followed_until_its_wake_time() {
    let fx = Fx::new();
    let mut r = fx.run("r");
    r.deferred(3_600_000);
    let wake = now() + 3_600_000;
    let seq = r.ev(json!({"type": "drive_released", "wake_at_ms": wake, "nodes": ["work"]}));
    // The driver exited on purpose: its pid file is gone.
    std::fs::remove_file(crate::driver::driver_pid_path(&r.dir)).unwrap();
    let obs = Observation::load(&r.dir, "r").unwrap();
    assert_eq!(obs.status(), crate::state::RunStatus::Running);
    assert!(!obs.view.driver_dead(), "released, not driver dead");
    let a = next_actions(&obs);
    let p = primary(&a);
    assert_eq!(p.code, ActionCode::ResumeRun);
    assert!(
        p.reason.contains("waiting to retry `work` at"),
        "{}",
        p.reason
    );
    assert!(matches!(
        p.auto,
        Some(AutoProgress::At { outcome: AutoOutcome::ResumeAttempt, assured: false, at_ms })
            if at_ms == wake
    ));
    assert!(matches!(
        retry_now(&a).auto,
        Some(AutoProgress::At {
            outcome: AutoOutcome::RetryAttempt,
            assured: false,
            ..
        })
    ));
    assert_eq!(a.stop, StopVerdict::Stopped);
    let held = verdict::sole_timed_pause(&obs, &a).unwrap();
    assert_eq!((held.seq, held.until_ms), (seq, wake));
    assert_eq!((fx.default_exit("r"), fx.follow_exit("r")), (4, 5));

    // Past the wake time plus the grace, for the same release: stopped.
    let mut r = fx.run("late");
    r.deferred(1_000);
    r.ev(json!({"type": "drive_released",
        "wake_at_ms": now() - verdict::UNTIL_GRACE_MS - 1_000, "nodes": ["work"]}));
    std::fs::remove_file(crate::driver::driver_pid_path(&r.dir)).unwrap();
    assert_eq!(fx.follow_exit("late"), 4);
}

#[test]
fn a_paused_run_with_an_open_review_needs_input_when_followed() {
    let fx = Fx::new();
    let mut r = fx.run("r");
    r.review();
    r.pause(Some(now() + 3_600_000));
    let a = fx.observe("r");
    assert_eq!(
        primary(&a).code,
        ActionCode::DecideReview,
        "the gate is primary"
    );
    assert!(codes(&a).contains(&ActionCode::ResumeRun));
    assert_eq!(a.stop, StopVerdict::NeedsInput);
    assert_eq!((fx.default_exit("r"), fx.follow_exit("r")), (4, 3));
}

#[test]
fn a_pause_without_until_a_budget_pause_and_a_dead_driver_are_stopped() {
    let fx = Fx::new();
    let mut r = fx.run("plain");
    r.pause(None);
    let a = fx.observe("plain");
    assert_eq!(
        (primary(&a).code, primary(&a).auto),
        (ActionCode::ResumeRun, None)
    );
    assert_eq!((fx.default_exit("plain"), fx.follow_exit("plain")), (4, 4));

    let mut r = fx.run("budget");
    let reason = format!(
        "{}nothing left before `work`)",
        crate::pause::cause::BUDGET_PREFIX
    );
    r.log
        .append(crate::pause::budget::budget_pause(reason).to_event())
        .unwrap();
    let a = fx.observe("budget");
    assert_eq!(primary(&a).code, ActionCode::ResumeWithBudget);
    assert_eq!(
        primary(&a).principals,
        vec![Principal::Person, Principal::Operator]
    );
    assert_eq!(
        (fx.default_exit("budget"), fx.follow_exit("budget")),
        (4, 4)
    );

    let mut r = fx.run("dead");
    r.started("work");
    r.ev(json!({"type": "node_finished", "node": "work", "status": "succeeded", "attempt": 1, "output": ""}));
    r.dead_driver();
    let a = fx.observe("dead");
    assert_eq!(primary(&a).code, ActionCode::ResumeRun);
    assert_eq!(a.stop, StopVerdict::Stopped);
    assert_eq!((fx.default_exit("dead"), fx.follow_exit("dead")), (4, 4));
}

#[test]
fn a_child_waiting_for_review_is_the_parent_s_input_with_the_child_s_run_id() {
    let fx = Fx::new();
    let mut parent = fx.run("parent");
    let mut child = parent.child(&fx, "child");
    let seq = child.review();
    let a = fx.observe("parent");
    let p = primary(&a);
    assert_eq!(
        (p.code, p.run_id.as_str()),
        (ActionCode::DecideReview, "child")
    );
    assert_eq!(p.target.request.as_ref().map(|q| q.seq), Some(seq));
    assert!(p.reason.starts_with("sub-playbook node `kid`"));
    assert_eq!(a.stop, StopVerdict::NeedsInput);
    // The default wait reads the parent's own gates only: it keeps waiting.
    assert_eq!(
        (fx.default_exit("parent"), fx.follow_exit("parent")),
        (5, 3)
    );
}

#[test]
fn a_paused_child_and_a_child_with_a_dead_driver_stop_the_followed_parent() {
    let fx = Fx::new();
    let mut parent = fx.run("p1");
    let mut child = parent.child(&fx, "c1");
    child.pause(None);
    let a = fx.observe("p1");
    assert_eq!(a.stop, StopVerdict::Stopped);
    assert_eq!(primary(&a).run_id, "c1");
    assert_eq!((fx.default_exit("p1"), fx.follow_exit("p1")), (5, 4));

    let mut parent = fx.run("p2");
    let mut child = parent.child(&fx, "c2");
    child.started("work");
    child.dead_driver();
    assert_eq!(fx.observe("p2").stop, StopVerdict::Stopped);
    assert_eq!(fx.follow_exit("p2"), 4);
}

#[test]
fn a_finished_run_offers_its_report_and_each_unknown_send() {
    let fx = Fx::new();
    let mut r = fx.run("ok");
    r.finish("succeeded");
    let a = fx.observe("ok");
    assert_eq!(codes(&a), vec![ActionCode::ReadReport]);
    assert_eq!(a.stop, StopVerdict::Finished);
    assert_eq!((fx.default_exit("ok"), fx.follow_exit("ok")), (0, 0));

    let mut r = fx.run("sent");
    r.ev(
        json!({"type": "send_intent_recorded", "intent": "si-1", "nonce": "n", "node": "work",
        "connector": "chat", "function": "send", "account": "main", "mode": "key", "label": "hello",
        "deadline_ms": 1}),
    );
    r.ev(json!({"type": "send_intent_settled", "intent": "si-1", "outcome": "unknown", "source": "timeout"}));
    r.finish("failed");
    let a = fx.observe("sent");
    assert_eq!(
        codes(&a),
        vec![ActionCode::ResolveIntent, ActionCode::ReadReport]
    );
    assert_eq!(primary(&a).principals, vec![Principal::Person]);
    // The action prints the one `apb intent resolve` command the run
    // outcome's gap prints too (`command::resolve_intent`).
    let line = super::command::render(primary(&a), &Default::default())
        .unwrap()
        .line();
    assert_eq!(
        line,
        super::command::resolve_intent(&primary(&a).run_id, Some("si-1"), None).line()
    );
    assert_eq!((fx.default_exit("sent"), fx.follow_exit("sent")), (1, 1));
}

/// A first send left unknown after a resend under `on_unknown: resend` went
/// out: the run still needs a person for the first one, and the action and
/// the run outcome (the text every surface shows) both say that a resend
/// went out and that whether the first also went out is unknown.
#[test]
fn an_unknown_send_after_a_resend_says_the_resend_went_out() {
    let fx = Fx::new();
    let mut r = fx.run("resent");
    r.ev(
        json!({"type": "send_intent_recorded", "intent": "si-1", "nonce": "n1", "node": "work",
        "connector": "chat", "function": "send", "account": "main", "mode": "key", "label": "hello",
        "deadline_ms": 1}),
    );
    r.ev(json!({"type": "send_intent_settled", "intent": "si-1", "outcome": "unknown", "source": "response", "detail": "timeout"}));
    r.ev(
        json!({"type": "send_intent_recorded", "intent": "si-2", "nonce": "n2", "node": "work",
        "connector": "chat", "function": "send", "account": "main", "mode": "key", "label": "hello",
        "deadline_ms": 1, "resend_of": "si-1"}),
    );
    r.ev(json!({"type": "send_intent_settled", "intent": "si-2", "outcome": "confirmed", "source": "response"}));
    r.finish("succeeded");
    let a = fx.observe("resent");
    assert_eq!(
        codes(&a),
        vec![ActionCode::ResolveIntent, ActionCode::ReadReport]
    );
    assert_eq!(
        primary(&a).reason,
        "a resend of intent `si-1` (chat.send) went out as `si-2`; whether the first send also went out is unknown; check the service and record what you found"
    );
    let dir = fx.root().join(".apb/runs/resent");
    let view = crate::run_view::RunView::load(&dir, "resent").unwrap();
    let outcome = crate::run_outcome::OutcomeSummary::build(&view, &dir);
    let resolve = super::command::resolve_intent("resent", Some("si-1"), None).line();
    assert!(
        outcome.lines().contains(&format!(
            "  [call outcome unknown] chat.send: intent \"hello\" (si-1) by work: a resend went out (si-2); whether this first send also went out is not known (timeout); resolve with {resolve}"
        )),
        "{:#?}",
        outcome.lines()
    );
}

// --- compositions ------------------------------------------------------------

#[test]
fn several_open_waits_never_hide_each_other() {
    let fx = Fx::new();
    // A question with a default plus a review: needs input, review first.
    let mut r = fx.run("a");
    r.question("ask");
    r.review();
    let a = fx.observe("a");
    assert_eq!(a.stop, StopVerdict::NeedsInput);
    assert_eq!(
        codes(&a),
        vec![ActionCode::DecideReview, ActionCode::AnswerQuestion]
    );

    // A timer plus a host task: needs input.
    let mut r = fx.run("b");
    r.wait_node("timer", "timer");
    r.started("work");
    let record = crate::host_task::TaskRecord {
        task_id: "work-1".into(),
        node: "work".into(),
        attempt: 1,
        workdir: ".".into(),
        skills: vec![],
        outputs: None,
        deadline_ms: None,
        model_hint: None,
        hint_source: None,
        fallback_of: None,
        env: Default::default(),
        requested_ts: now(),
    };
    crate::host_task::write_task(&r.dir, &record, "do it", None).unwrap();
    r.ev(json!({"type": "host_task_requested", "task_id": "work-1", "node": "work", "attempt": 1}));
    let a = fx.observe("b");
    assert_eq!(a.stop, StopVerdict::NeedsInput);
    assert_eq!(
        codes(&a),
        vec![ActionCode::SubmitHostTask, ActionCode::WaitAgain]
    );
}

/// A paused run's timers are frozen with what is left on the paused-time
/// clock; a question with a default is then not assured and needs input.
#[test]
fn a_paused_run_reports_frozen_timers() {
    let fx = Fx::new();
    let mut r = fx.run("r");
    r.question("ask");
    r.wait_node("timer", "timer");
    r.pause(None);
    let a = fx.observe("r");
    for x in &a.actions {
        if matches!(x.code, ActionCode::AnswerQuestion | ActionCode::WaitAgain) {
            assert!(
                matches!(x.auto, Some(AutoProgress::Frozen { remaining_ms, .. }) if remaining_ms > 0),
                "{:?}",
                x.auto
            );
        }
    }
    assert_eq!(a.stop, StopVerdict::NeedsInput);
    let q = r.dir.clone();
    let view = crate::run_view::RunView::load(&q, "r").unwrap();
    let pq = &view.progress.unwrap().pending_questions[0];
    assert_eq!(pq.default_answer.as_deref(), Some("go"));
    assert!(pq.timeout_ms.is_some_and(|t| t <= 600_000 && t > 590_000));
}

/// A run nobody started a child for reads the same after the change: the
/// default classifier's exits on the rows above are today's.
#[test]
fn the_exit_codes_are_the_documented_ones() {
    use crate::run_wait::WaitReason;
    use crate::state::RunStatus;
    assert_eq!(WaitReason::Finished.exit(RunStatus::Succeeded).code(), 0);
    assert_eq!(WaitReason::Finished.exit(RunStatus::Aborted).code(), 1);
    assert_eq!(WaitReason::NeedsInput.exit(RunStatus::Running).code(), 3);
    assert_eq!(WaitReason::Stopped.exit(RunStatus::Paused).code(), 4);
    assert_eq!(WaitReason::Timeout.exit(RunStatus::Running).code(), 5);
    assert_eq!(WaitExit::Error.code(), 2);
}

// --- what an unattended run does at each wait ---------------------------------

/// The `unattended` outcome of the first action with `code` on run `id`.
fn unattended_of(fx: &Fx, id: &str, code: ActionCode) -> Option<UnattendedOutcome> {
    fx.observe(id)
        .actions
        .into_iter()
        .find(|a| a.code == code)
        .unwrap_or_else(|| panic!("run `{id}` offers no {code:?}"))
        .unattended
}

fn fails(code: &str) -> Option<UnattendedOutcome> {
    Some(UnattendedOutcome::FailsRun { code: code.into() })
}

fn proceeds(auto: AutoOutcome) -> Option<UnattendedOutcome> {
    Some(UnattendedOutcome::Proceeds { auto })
}

/// Every row of the unattended table, on attended runs: what the mode would
/// do at each wait.
#[test]
fn every_wait_says_what_an_unattended_run_does_there() {
    use crate::run_config::{BUDGET_EXHAUSTED, HITL_REQUIRED, UNATTENDED_UNSUPPORTED};
    let fx = Fx::new();
    fx.run("q").question("ask");
    assert_eq!(
        unattended_of(&fx, "q", ActionCode::AnswerQuestion),
        Some(UnattendedOutcome::AnswersDefault)
    );
    fx.run("qn").question("ask_nodef");
    assert_eq!(
        unattended_of(&fx, "qn", ActionCode::AnswerQuestion),
        fails(HITL_REQUIRED)
    );

    fx.run("g").review();
    assert_eq!(
        unattended_of(&fx, "g", ActionCode::DecideReview),
        fails(HITL_REQUIRED)
    );
    // A declared decision: the run decides it when nothing after the gate
    // refuses it, and fails as the run would when the unpinned
    // sub-playbook `kid` lies in its reach.
    let declared = PLAYBOOK.replace(
        "options: [approved, rejected] }",
        "options: [approved, rejected], unattended: { decide: approved } }",
    );
    let mut r = fx.run("gd");
    std::fs::write(
        r.dir.join("playbook.yaml"),
        declared.replace("{ from: work, to: kid }", "{ from: work, to: f }"),
    )
    .unwrap();
    r.review();
    assert_eq!(
        unattended_of(&fx, "gd", ActionCode::DecideReview),
        Some(UnattendedOutcome::Decides {
            option: "approved".into()
        })
    );
    let mut r = fx.run("gk");
    std::fs::write(r.dir.join("playbook.yaml"), &declared).unwrap();
    r.review();
    assert_eq!(
        unattended_of(&fx, "gk", ActionCode::DecideReview),
        fails(HITL_REQUIRED)
    );

    fx.run("t").wait_node("timer", "timer");
    assert_eq!(
        unattended_of(&fx, "t", ActionCode::WaitAgain),
        proceeds(AutoOutcome::TimerElapses)
    );
    fx.run("h").wait_node("hook", "webhook");
    assert_eq!(
        unattended_of(&fx, "h", ActionCode::WaitAgain),
        proceeds(AutoOutcome::WaitTimesOut)
    );

    fx.run("d").deferred(600_000);
    assert_eq!(
        unattended_of(&fx, "d", ActionCode::RetryNow),
        proceeds(AutoOutcome::RetryAttempt)
    );

    let mut r = fx.run("s");
    r.started("work");
    r.ev(
        json!({"type": "wake_raised", "trigger": "node_failed", "node": "work", "detail": "boom"}),
    );
    assert_eq!(
        unattended_of(&fx, "s", ActionCode::SupervisorRetryNode),
        Some(UnattendedOutcome::RefusedAtStart {
            code: UNATTENDED_UNSUPPORTED.into()
        })
    );

    let mut r = fx.run("b");
    let reason = format!(
        "{}nothing left before `work`)",
        crate::pause::cause::BUDGET_PREFIX
    );
    r.log
        .append(crate::pause::budget::budget_pause(reason).to_event())
        .unwrap();
    assert_eq!(
        unattended_of(&fx, "b", ActionCode::ResumeWithBudget),
        fails(BUDGET_EXHAUSTED)
    );

    let mut r = fx.run("quota");
    let quota = crate::pause::PauseRequest {
        reason: Some("quota (claude/opus)".into()),
        by: Some(crate::pause::PauseBy::Engine),
        mode: crate::pause::PauseMode::Drain,
        until: Some(now() + 600_000),
        keep_lock: false,
    };
    r.log.append(quota.to_event()).unwrap();
    assert_eq!(
        unattended_of(&fx, "quota", ActionCode::ResumeRun),
        proceeds(AutoOutcome::ResumeAttempt)
    );
    let mut r = fx.run("ht");
    r.started("work");
    let record = crate::host_task::TaskRecord {
        task_id: "work-1".into(),
        node: "work".into(),
        attempt: 1,
        workdir: ".".into(),
        skills: vec![],
        outputs: None,
        deadline_ms: None,
        model_hint: None,
        hint_source: None,
        fallback_of: None,
        env: Default::default(),
        requested_ts: now(),
    };
    crate::host_task::write_task(&r.dir, &record, "do it", None).unwrap();
    r.ev(json!({"type": "host_task_requested", "task_id": "work-1", "node": "work", "attempt": 1}));
    assert_eq!(
        unattended_of(&fx, "ht", ActionCode::SubmitHostTask),
        Some(UnattendedOutcome::RefusedAtStart {
            code: UNATTENDED_UNSUPPORTED.into()
        })
    );
    // An operator's pause is no wait of the run.
    fx.run("op").pause(Some(now() + 600_000));
    assert_eq!(unattended_of(&fx, "op", ActionCode::ResumeRun), None);
}
