//! Which transitions a subscription is told about: only news, only the
//! kinds it covers, only what is still open, and each one at most once,
//! with a bounded retry of a send that failed.

use apb_engine::event::{Event, EventPayload};
use apb_engine::notify::NotifyState;
use apb_engine::notify::record::NotifyTargetRecord;
use apb_engine::notify::transitions::{KeyWindow, MAX_TRIES, RETRY_GAP_MS, due};
use apb_engine::pause::kind::PauseBy;

const NOW: u64 = 1_000_000;

fn ev(seq: u64, payload: EventPayload) -> Event {
    Event {
        seq,
        ts: u128::from(NOW - 100_000 + seq),
        payload,
    }
}

fn subscribed(seq: u64, on: &[&str]) -> Event {
    ev(
        seq,
        EventPayload::NotifySet {
            target: Some(NotifyTargetRecord {
                connector: "chat".into(),
                account: "me".into(),
                function: "send".into(),
                to: "42".into(),
                connector_digest: "sha256:c".into(),
                account_digest: "sha256:a".into(),
            }),
            on: on.iter().map(|s| s.to_string()).collect(),
            source: "run".into(),
            by: None,
            problem: None,
        },
    )
}

fn started() -> Event {
    ev(
        0,
        EventPayload::RunStarted {
            playbook: "p".into(),
            version: "1.0.0".into(),
        },
    )
}

fn review(seq: u64, node: &str) -> Event {
    ev(
        seq,
        EventPayload::ReviewRequested {
            node: node.into(),
            options: vec![],
            title: None,
            instruction: String::new(),
            prompt: None,
            recommendation: None,
        },
    )
}

fn paused(seq: u64, by: PauseBy) -> Event {
    ev(
        seq,
        EventPayload::RunPaused {
            reason: "pause".into(),
            by: Some(by),
            mode: None,
            until: None,
            keep_lock: false,
        },
    )
}

fn aborted(seq: u64, by: PauseBy) -> Event {
    ev(
        seq,
        EventPayload::RunAborted {
            reason: "stop".into(),
            by: Some(by),
        },
    )
}

/// A `key`-mode window of ten minutes for a target that answers in ten
/// seconds.
const WINDOW: KeyWindow = KeyWindow {
    window_ms: 600_000,
    timeout_ms: 10_000,
};

fn ids(events: &[Event]) -> Vec<String> {
    ids_within(events, WINDOW)
}

fn ids_within(events: &[Event], window: KeyWindow) -> Vec<String> {
    due(events, &NotifyState::fold(events), NOW, window)
        .iter()
        .map(|t| t.id())
        .collect()
}

/// A notification try of `label`: recorded at `seq` with `mode`, by a
/// process that is gone, settled as `outcome`/`source` when given.
fn tried(seq: u64, id: &str, label: &str, mode: &str, settled: Option<(&str, &str)>) -> Vec<Event> {
    let mut out = vec![ev(
        seq,
        EventPayload::SendIntentRecorded {
            item: None,
            intent: id.into(),
            nonce: "n".into(),
            node: "@notify".into(),
            attempt: None,
            attempt_seq: None,
            connector: "chat".into(),
            function: "send".into(),
            account: "me".into(),
            mode: mode.into(),
            label: label.into(),
            pid: u32::MAX,
            deadline_ms: 0,
            replay_of: None,
            resend_of: None,
        },
    )];
    if let Some((outcome, source)) = settled {
        out.push(ev(
            seq + 1,
            EventPayload::SendIntentSettled {
                intent: id.into(),
                outcome: outcome.into(),
                source: source.into(),
                detail: None,
                effect_ref: None,
                by: None,
            },
        ));
    }
    out
}

#[test]
fn an_open_review_is_due_and_a_decided_one_is_not() {
    let mut events = vec![started(), subscribed(1, &["review"]), review(2, "gate")];
    assert_eq!(ids(&events), ["review.2"]);
    events.push(ev(
        3,
        EventPayload::ReviewDecided {
            node: "gate".into(),
            decision: "approve".into(),
            note: String::new(),
        },
    ));
    assert!(ids(&events).is_empty());
}

#[test]
fn a_question_answered_before_the_sweep_is_not_due() {
    let asked = |seq| {
        ev(
            seq,
            EventPayload::QuestionAsked {
                node: "ask".into(),
                question: "which one?".into(),
                options: vec![],
            },
        )
    };
    let mut events = vec![started(), subscribed(1, &["question"]), asked(2), asked(3)];
    events.push(ev(
        4,
        EventPayload::QuestionAnswered {
            node: "ask".into(),
            answer: "a".into(),
            answered_by: "human".into(),
        },
    ));
    assert_eq!(ids(&events), ["question.3"]);
}

#[test]
fn a_pause_or_a_stop_a_person_made_is_not_news_to_them() {
    let on = ["paused", "finished"];
    let operator = vec![started(), subscribed(1, &on), paused(2, PauseBy::Operator)];
    assert!(ids(&operator).is_empty());
    let engine = vec![started(), subscribed(1, &on), paused(2, PauseBy::Engine)];
    assert_eq!(ids(&engine), ["paused.2"]);
    let stopped = vec![started(), subscribed(1, &on), aborted(2, PauseBy::Operator)];
    assert!(ids(&stopped).is_empty());
    let supervisor = vec![
        started(),
        subscribed(1, &on),
        aborted(2, PauseBy::Supervisor),
    ];
    assert_eq!(ids(&supervisor), ["finished.2"]);
}

#[test]
fn the_end_of_the_run_closes_every_wait() {
    let events = vec![
        started(),
        subscribed(1, &["review", "finished"]),
        review(2, "gate"),
        ev(
            3,
            EventPayload::RunFinished {
                outcome: "failed".into(),
            },
        ),
    ];
    assert_eq!(ids(&events), ["finished.3"]);
}

#[test]
fn only_news_and_only_the_kinds_subscribed_to() {
    // A review journaled before the subscription took effect is not news.
    let late = vec![
        started(),
        review(1, "gate"),
        subscribed(2, &["review", "paused"]),
    ];
    assert!(ids(&late).is_empty());
    let other_kind = vec![started(), subscribed(1, &["paused"]), review(2, "gate")];
    assert!(ids(&other_kind).is_empty());
    // A change to a subscription in force keeps the seq it started at.
    let changed = vec![
        started(),
        subscribed(1, &["paused"]),
        review(2, "gate"),
        subscribed(3, &["review"]),
    ];
    assert_eq!(ids(&changed), ["review.2"]);
    // Off is off.
    let mut off = changed.clone();
    off.push(ev(
        4,
        EventPayload::NotifySet {
            target: None,
            on: vec![],
            source: "person".into(),
            by: Some(PauseBy::Operator),
            problem: None,
        },
    ));
    assert!(ids(&off).is_empty());
}

#[test]
fn a_sent_or_unknown_notification_is_never_sent_again() {
    let base = [started(), subscribed(1, &["review"]), review(2, "gate")];
    let sent = [
        &base[..],
        &tried(
            3,
            "si-1",
            "notify:review.2",
            "none",
            Some(("confirmed", "response")),
        ),
    ]
    .concat();
    assert!(ids(&sent).is_empty());
    // Pending with its sender gone reads as unknown, and `none` mode
    // cannot tell whether it went out.
    let orphan = [
        &base[..],
        &tried(3, "si-1", "notify:review.2", "none", None),
    ]
    .concat();
    assert!(ids(&orphan).is_empty());
    // In `key` mode the service drops a repeat, so the ledger may replay it,
    // but only while the replay lands inside the service's window.
    let keyed = [&base[..], &tried(3, "si-1", "notify:review.2", "key", None)].concat();
    assert_eq!(ids(&keyed), ["review.2"]);
    let closed = KeyWindow {
        window_ms: 60_000,
        timeout_ms: 10_000,
    };
    assert!(ids_within(&keyed, closed).is_empty());
    assert!(ids_within(&keyed, KeyWindow::default()).is_empty());
}

#[test]
fn a_try_a_passing_fault_kept_from_going_out_is_retried() {
    let base = [started(), subscribed(1, &["review"]), review(2, "gate")];
    for source in ["unsent", "journal", "refused", "response"] {
        let failed = [
            &base[..],
            &tried(
                3,
                "si-1",
                "notify:review.2",
                "none",
                Some(("not_sent", source)),
            ),
        ]
        .concat();
        assert_eq!(ids(&failed), ["review.2"], "source {source}");
    }
}

#[test]
fn a_gate_refusal_holds_until_the_subscription_is_set_again() {
    let mut events = [
        &[started(), subscribed(1, &["review"]), review(2, "gate")][..],
        &tried(
            3,
            "si-1",
            "notify:review.2",
            "none",
            Some(("not_sent", "gate")),
        ),
    ]
    .concat();
    assert!(ids(&events).is_empty());
    // The person sets it up again: the refusal before it no longer holds.
    events.push(subscribed(5, &["review"]));
    assert_eq!(ids(&events), ["review.2"]);
    // A refusal after the new set does.
    events.extend(tried(
        6,
        "si-2",
        "notify:review.2",
        "none",
        Some(("not_sent", "gate")),
    ));
    assert!(ids(&events).is_empty());
}

#[test]
fn a_failed_try_is_retried_a_bounded_number_of_times_and_a_gate_refusal_never() {
    let base = vec![started(), subscribed(1, &["review"]), review(2, "gate")];
    let failed = [
        &base[..],
        &tried(
            3,
            "si-1",
            "notify:review.2",
            "none",
            Some(("not_sent", "response")),
        ),
    ]
    .concat();
    assert_eq!(ids(&failed), ["review.2"]);
    // Not again inside the retry gap.
    let mut recent = failed.clone();
    for e in recent.iter_mut().skip(3) {
        e.ts = u128::from(NOW - RETRY_GAP_MS / 2);
    }
    assert!(ids(&recent).is_empty());
    let mut many = base.clone();
    for i in 0..MAX_TRIES as u64 {
        many.extend(tried(
            3 + i * 2,
            &format!("si-{i}"),
            "notify:review.2",
            "none",
            Some(("not_sent", "response")),
        ));
    }
    assert!(ids(&many).is_empty());
    let refused = [
        &base[..],
        &tried(
            3,
            "si-1",
            "notify:review.2",
            "none",
            Some(("not_sent", "gate")),
        ),
    ]
    .concat();
    assert!(ids(&refused).is_empty());
}

#[test]
fn a_notification_that_failed_as_the_run_ended_is_reported_as_given_up() {
    use apb_engine::notify::RunNotify;

    let ended = ev(
        2,
        EventPayload::RunFinished {
            outcome: "success".into(),
        },
    );
    let mut events = [
        &[started(), subscribed(1, &["finished"]), ended][..],
        &tried(
            3,
            "si-1",
            "notify:finished.2",
            "none",
            Some(("not_sent", "response")),
        ),
    ]
    .concat();
    let view = RunNotify::from_events(&events, NOW).unwrap();
    assert_eq!(view.sent[0].state, "not_sent");
    assert!(
        view.problems.iter().any(
            |p| p.contains("`finished.2` was not sent") && p.contains("nothing tries it again")
        ),
        "{:?}",
        view.problems
    );
    // A resume tries it again, so a live run reports no such problem.
    events.push(ev(
        5,
        EventPayload::RunResumed {
            from_node: "s".into(),
            by: None,
            explicit: false,
        },
    ));
    let view = RunNotify::from_events(&events, NOW).unwrap();
    assert!(view.problems.is_empty(), "{:?}", view.problems);
}

#[test]
fn a_keyed_try_nobody_knows_about_is_not_reported_as_not_sent_after_a_failed_replay() {
    use apb_engine::notify::RunNotify;

    let ended = ev(
        2,
        EventPayload::RunFinished {
            outcome: "success".into(),
        },
    );
    let events = [
        &[started(), subscribed(1, &["finished"]), ended][..],
        &tried(
            3,
            "si-1",
            "notify:finished.2",
            "key",
            Some(("unknown", "response")),
        ),
        &tried(
            5,
            "si-2",
            "notify:finished.2",
            "key",
            Some(("not_sent", "response")),
        ),
    ]
    .concat();
    let view = RunNotify::from_events(&events, NOW).unwrap();
    assert_eq!(view.sent[0].state, "unknown");
    assert!(view.problems.is_empty(), "{:?}", view.problems);
}
