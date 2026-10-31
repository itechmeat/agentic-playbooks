//! The send ledger's decision table ([`admit_with`]): every state of an
//! earlier intent against every idempotency mode, the key window, the
//! `on_unknown` policy, the attempt binding and the replay chain. Pure: the
//! journal is built in memory, the clock and the process check are passed
//! in.

use apb_core::connector::idempotency::IdempotencyMode::{self, Key, Marker, None as NoKey};
use apb_core::schema::OnUnknown;

use super::*;
use crate::connector::intent::IntentKey;
use crate::event::{Event, EventPayload};

const NOW: u64 = 1_000_000_000;

fn key(label: &str) -> IntentKey {
    IntentKey {
        item: None,
        node: "n".into(),
        connector: "c".into(),
        function: "send".into(),
        account: "main".into(),
        label: label.into(),
    }
}

/// A journal under construction, one event per call, seqs in order.
#[derive(Default)]
struct Journal(Vec<Event>);

impl Journal {
    fn push(&mut self, ts: u64, payload: EventPayload) -> &mut Self {
        let seq = self.0.len() as u64;
        self.0.push(Event {
            seq,
            ts: u128::from(ts),
            payload,
        });
        self
    }

    fn intent(&mut self, id: &str, label: &str, ts: u64, deadline_ms: u64) -> &mut Self {
        self.intent_of(id, label, ts, deadline_ms, None)
    }

    fn intent_of(
        &mut self,
        id: &str,
        label: &str,
        ts: u64,
        deadline_ms: u64,
        replay_of: Option<&str>,
    ) -> &mut Self {
        self.linked(id, label, ts, deadline_ms, replay_of, None)
    }

    /// A resend of `of`: a new send with its own nonce.
    fn resend(&mut self, id: &str, of: &str, ts: u64) -> &mut Self {
        self.linked(id, "a", ts, NOW, None, Some(of))
    }

    fn linked(
        &mut self,
        id: &str,
        label: &str,
        ts: u64,
        deadline_ms: u64,
        replay_of: Option<&str>,
        resend_of: Option<&str>,
    ) -> &mut Self {
        self.push(
            ts,
            EventPayload::SendIntentRecorded {
                item: None,
                intent: id.into(),
                // A replay carries the nonce of the send it repeats.
                nonce: format!("nonce-{}", replay_of.unwrap_or(id)),
                node: "n".into(),
                attempt: Some(1),
                attempt_seq: Some(0),
                connector: "c".into(),
                function: "send".into(),
                account: "main".into(),
                mode: "key".into(),
                label: label.into(),
                pid: 42,
                deadline_ms,
                replay_of: replay_of.map(str::to_string),
                resend_of: resend_of.map(str::to_string),
            },
        )
    }

    fn settle(&mut self, id: &str, outcome: &str) -> &mut Self {
        self.settle_by(id, outcome, "response")
    }

    fn settle_by(&mut self, id: &str, outcome: &str, source: &str) -> &mut Self {
        self.push(
            NOW,
            EventPayload::SendIntentSettled {
                intent: id.into(),
                outcome: outcome.into(),
                source: source.into(),
                detail: None,
                effect_ref: (outcome == "confirmed").then(|| format!("ref-{id}")),
                by: None,
            },
        )
    }

    fn node_finished(&mut self, status: &str) -> &mut Self {
        self.push(
            NOW,
            EventPayload::NodeFinished {
                node: "n".into(),
                status: status.into(),
                attempt: 1,
                output: String::new(),
                artifacts: Vec::new(),
            },
        )
    }
}

fn query(k: &IntentKey, mode: IdempotencyMode, on_unknown: OnUnknown) -> AdmitQuery<'_> {
    AdmitQuery {
        key: k,
        mode,
        window_sec: 120,
        timeout_sec: 0,
        on_unknown,
        attempt_open: true,
        now_ms: NOW,
    }
}

fn alive(_: u32) -> bool {
    true
}

fn dead(_: u32) -> bool {
    false
}

fn decide(j: &Journal, mode: IdempotencyMode, on_unknown: OnUnknown) -> Admission {
    let k = key("a");
    admit_with(&j.0, &query(&k, mode, on_unknown), &alive)
}

#[test]
fn a_first_send_is_new_and_a_closed_attempt_is_refused_whatever_the_ledger_holds() {
    let empty = Journal::default();
    for mode in [Key, Marker, NoKey] {
        assert_eq!(
            decide(&empty, mode, OnUnknown::Refuse),
            Admission::New { resend_of: None }
        );
    }
    let mut j = Journal::default();
    j.intent("i1", "a", NOW, NOW + 10).settle("i1", "confirmed");
    let k = key("a");
    let mut q = query(&k, Key, OnUnknown::Refuse);
    q.attempt_open = false;
    assert_eq!(admit_with(&j.0, &q, &alive), Admission::AttemptClosed);
}

#[test]
fn a_confirmed_send_is_answered_in_every_mode_and_another_label_is_new() {
    let mut j = Journal::default();
    j.intent("i1", "a", NOW, NOW + 10).settle("i1", "confirmed");
    for mode in [Key, Marker, NoKey] {
        assert_eq!(
            decide(&j, mode, OnUnknown::Resend),
            Admission::AlreadySent {
                intent: "i1".into(),
                label: "a".into(),
                attempt: Some(1),
                effect_ref: Some("ref-i1".into()),
            }
        );
    }
    let other = key("b");
    assert_eq!(
        admit_with(&j.0, &query(&other, Key, OnUnknown::Refuse), &alive),
        Admission::New { resend_of: None }
    );
}

#[test]
fn a_send_that_did_not_go_out_may_be_sent_again() {
    let mut j = Journal::default();
    j.intent("i1", "a", NOW, NOW + 10).settle("i1", "not_sent");
    for mode in [Key, Marker, NoKey] {
        assert_eq!(
            decide(&j, mode, OnUnknown::Refuse),
            Admission::New {
                resend_of: Some("i1".into())
            }
        );
    }
}

#[test]
fn a_pending_send_is_in_flight_until_its_deadline_or_its_sender_ends() {
    let mut live = Journal::default();
    live.intent("i1", "a", NOW, NOW + 10);
    assert_eq!(
        decide(&live, NoKey, OnUnknown::Refuse),
        Admission::InFlight {
            intent: "i1".into()
        }
    );

    let mut late = Journal::default();
    late.intent("i1", "a", NOW - 50, NOW - 1);
    assert!(matches!(
        decide(&late, NoKey, OnUnknown::Refuse),
        Admission::RefuseUnknown { .. }
    ));

    let k = key("a");
    assert!(matches!(
        admit_with(&live.0, &query(&k, NoKey, OnUnknown::Refuse), &dead),
        Admission::RefuseUnknown { .. }
    ));
    let ledger = Ledger::fold(&live.0);
    assert!(matches!(
        effective_state_with(&ledger.records()[0], NOW, &dead),
        IntentState::Unknown { source, .. } if source == "orphan"
    ));
}

#[test]
fn a_push_stays_pending_until_its_deadline_even_when_its_recorder_ended() {
    let ref_intent = |deadline_ms: u64| {
        let mut j = Journal::default();
        j.intent("i1", "ref:main", NOW - 10, deadline_ms);
        if let EventPayload::SendIntentRecorded { mode, .. } = &mut j.0[0].payload {
            *mode = "ref".into();
        }
        Ledger::fold(&j.0)
    };
    // The push runs in its own session and may still be updating the
    // remote: a reconcile now could read a ref it is about to move.
    let live = ref_intent(NOW + 10);
    assert!(matches!(
        effective_state_with(&live.records()[0], NOW, &dead),
        IntentState::Pending { .. }
    ));
    let late = ref_intent(NOW - 1);
    assert!(matches!(
        effective_state_with(&late.records()[0], NOW, &dead),
        IntentState::Unknown { source, .. } if source == "orphan"
    ));
}

#[test]
fn an_unknown_key_send_is_replayed_inside_its_window_and_refused_or_resent_after() {
    let mut inside = Journal::default();
    inside
        .intent("i1", "a", NOW - 60_000, NOW)
        .settle("i1", "unknown");
    assert_eq!(
        decide(&inside, Key, OnUnknown::Refuse),
        Admission::Replay {
            of: "i1".into(),
            nonce: "nonce-i1".into()
        }
    );

    let mut past = Journal::default();
    past.intent("i1", "a", NOW - 121_000, NOW)
        .settle("i1", "unknown");
    assert!(matches!(
        decide(&past, Key, OnUnknown::Refuse),
        Admission::RefuseUnknown { .. }
    ));
    assert_eq!(
        decide(&past, Key, OnUnknown::Resend),
        Admission::New {
            resend_of: Some("i1".into())
        }
    );
}

/// A replay goes out with the nonce of the intent that started the chain, so
/// the window counts from that first send, not from the replay.
#[test]
fn a_replay_chain_counts_its_window_from_the_first_send() {
    let mut j = Journal::default();
    j.intent("i1", "a", NOW - 121_000, NOW)
        .settle("i1", "unknown")
        .intent_of("i2", "a", NOW - 1_000, NOW, Some("i1"))
        .settle("i2", "unknown");
    assert!(matches!(
        decide(&j, Key, OnUnknown::Refuse),
        Admission::RefuseUnknown { intent, .. } if intent == "i2"
    ));
    let ledger = Ledger::fold(&j.0);
    assert_eq!(
        ledger.get("i1").unwrap().superseded_by.as_deref(),
        Some("i2")
    );
}

#[test]
fn an_unknown_marker_send_is_looked_up_and_never_resent_blind() {
    let mut j = Journal::default();
    j.intent("i1", "a", NOW, NOW).settle("i1", "unknown");
    for policy in [OnUnknown::Refuse, OnUnknown::Resend] {
        assert_eq!(
            decide(&j, Marker, policy),
            Admission::Reconcile {
                intent: "i1".into()
            }
        );
    }
}

#[test]
fn an_unknown_send_without_a_key_is_refused_unless_resend_is_allowed() {
    let mut j = Journal::default();
    j.intent("i1", "a", NOW, NOW).settle("i1", "unknown");
    assert!(matches!(
        decide(&j, NoKey, OnUnknown::Refuse),
        Admission::RefuseUnknown { .. }
    ));
    assert_eq!(
        decide(&j, NoKey, OnUnknown::Resend),
        Admission::New {
            resend_of: Some("i1".into())
        }
    );
}

/// A later settlement wins, except that a confirmed send stays confirmed
/// whatever a process says later; only a person changes it.
#[test]
fn confirmed_is_final_except_for_a_person() {
    for later in ["unknown", "not_sent"] {
        let mut j = Journal::default();
        j.intent("i1", "a", NOW, NOW)
            .settle("i1", "confirmed")
            .settle("i1", later);
        assert!(matches!(
            Ledger::fold(&j.0).records()[0].state,
            IntentState::Confirmed { .. }
        ));
    }
    let mut person = Journal::default();
    person
        .intent("i1", "a", NOW, NOW)
        .settle("i1", "confirmed")
        .settle_by("i1", "not_sent", "person");
    assert!(matches!(
        Ledger::fold(&person.0).records()[0].state,
        IntentState::NotSent { .. }
    ));
    let mut k = Journal::default();
    k.intent("i1", "a", NOW, NOW)
        .settle("i1", "unknown")
        .settle("i1", "confirmed");
    assert!(matches!(
        Ledger::fold(&k.0).records()[0].state,
        IntentState::Confirmed { .. }
    ));
}

/// Intents count only after the node's last success: a loop visit starts a
/// fresh scope, a failed visit does not.
#[test]
fn the_effect_scope_starts_after_the_last_success() {
    let mut failed = Journal::default();
    failed
        .intent("i1", "a", NOW, NOW)
        .settle("i1", "confirmed")
        .node_finished("failed");
    assert!(matches!(
        decide(&failed, Key, OnUnknown::Refuse),
        Admission::AlreadySent { .. }
    ));
    let mut looped = Journal::default();
    looped
        .intent("i1", "a", NOW, NOW)
        .settle("i1", "confirmed")
        .node_finished("succeeded");
    assert_eq!(
        decide(&looped, Key, OnUnknown::Refuse),
        Admission::New { resend_of: None }
    );
}

/// A replay that did not go out says only that the replay did not act: the
/// unknown send it repeated still decides the chain.
#[test]
fn a_replay_that_did_not_go_out_leaves_the_chain_unknown() {
    let mut j = Journal::default();
    j.intent("i1", "a", NOW - 10_000, NOW)
        .settle("i1", "unknown")
        .intent_of("i2", "a", NOW - 5_000, NOW, Some("i1"))
        .settle("i2", "not_sent");
    assert_eq!(
        decide(&j, Key, OnUnknown::Refuse),
        Admission::Replay {
            of: "i2".into(),
            nonce: "nonce-i1".into()
        }
    );
    assert_eq!(
        decide(&j, Marker, OnUnknown::Refuse),
        Admission::Reconcile {
            intent: "i1".into()
        }
    );
    assert!(matches!(
        decide(&j, NoKey, OnUnknown::Refuse),
        Admission::RefuseUnknown { intent, .. } if intent == "i1"
    ));
    assert_eq!(
        decide(&j, NoKey, OnUnknown::Resend),
        Admission::New {
            resend_of: Some("i2".into())
        }
    );
    let ledger = Ledger::fold(&j.0);
    let verdict = chain_verdict(&ledger, ledger.get("i2").unwrap(), NOW);
    assert_eq!(verdict.member.id, "i1");
    assert!(matches!(verdict.state, IntentState::Unknown { .. }));
}

/// Any member that went out settles the chain: the repeat is answered with
/// that member's result.
#[test]
fn a_chain_with_a_confirmed_member_is_answered_whatever_came_after() {
    let mut j = Journal::default();
    j.intent("i1", "a", NOW - 10_000, NOW)
        .settle("i1", "unknown")
        .intent_of("i2", "a", NOW - 5_000, NOW, Some("i1"))
        .settle("i2", "confirmed")
        .intent_of("i3", "a", NOW - 1_000, NOW, Some("i2"))
        .settle("i3", "not_sent");
    assert_eq!(
        decide(&j, NoKey, OnUnknown::Refuse),
        Admission::AlreadySent {
            intent: "i2".into(),
            label: "a".into(),
            attempt: Some(1),
            effect_ref: Some("ref-i2".into()),
        }
    );
}

/// A replay is admitted only when the call can be answered before the
/// service forgets the key: the call's timeout counts against the window.
#[test]
fn a_replay_must_fit_its_timeout_inside_the_key_window() {
    let mut j = Journal::default();
    // The window (120 s) ends 10 s from now.
    j.intent("i1", "a", NOW - 110_000, NOW)
        .settle("i1", "unknown");
    let k = key("a");
    let mut q = query(&k, Key, OnUnknown::Refuse);
    q.timeout_sec = 10;
    assert!(matches!(
        admit_with(&j.0, &q, &alive),
        Admission::Replay { .. }
    ));
    q.timeout_sec = 11;
    assert!(matches!(
        admit_with(&j.0, &q, &alive),
        Admission::RefuseUnknown { .. }
    ));
}

/// A link that names the intent itself, a later intent, or two intents that
/// name each other (a duplicated id) never makes the chain walk spin.
#[test]
fn a_link_loop_in_a_corrupt_journal_ends_the_chain_walk() {
    let mut j = Journal::default();
    j.intent_of("self", "a", NOW - 9_000, NOW, Some("self"))
        .intent_of("x", "b", NOW - 8_000, NOW, Some("y"))
        .intent_of("y", "b", NOW - 7_000, NOW, Some("x"))
        // A duplicated id: `p` again, now replaying `q`, which replays the
        // first `p`.
        .intent("p", "c", NOW - 6_000, NOW)
        .intent_of("q", "c", NOW - 5_000, NOW, Some("p"))
        .intent_of("p", "c", NOW - 4_000, NOW, Some("q"));
    let ledger = Ledger::fold(&j.0);
    let me = ledger.get("self").unwrap();
    assert_eq!(me.replay_of, None, "a self link is ignored");
    assert_eq!(ledger.chain_of(me).len(), 1);
    // `x` named a later intent, so only `y` links back to `x`.
    assert_eq!(ledger.get("x").unwrap().replay_of, None);
    let ids = |rec| -> Vec<&str> { ledger.chain_of(rec).iter().map(|r| r.id.as_str()).collect() };
    assert_eq!(ids(ledger.get("y").unwrap()), ["x", "y"]);
    assert!(ids(ledger.get("p").unwrap()).len() <= ledger.records().len());
    assert!(ids(ledger.get("q").unwrap()).len() <= ledger.records().len());
    let k = key("c");
    let q = query(&k, Key, OnUnknown::Refuse);
    assert!(matches!(
        admit_with(&j.0, &q, &alive),
        Admission::InFlight { .. }
    ));
}

/// A resend is a separate send: each unknown member stays open on its own,
/// and a later resend that went out answers a repeat without closing the
/// earlier unknown send, which may have gone out too.
#[test]
fn a_resend_that_went_out_leaves_the_earlier_unknown_send_open() {
    let mut j = Journal::default();
    j.intent("i1", "a", NOW - 10_000, NOW)
        .settle("i1", "unknown")
        .resend("i2", "i1", NOW - 5_000)
        .settle("i2", "unknown");
    let ledger = Ledger::fold(&j.0);
    for id in ["i1", "i2"] {
        let verdict = send_verdict(&ledger, ledger.get(id).unwrap(), NOW);
        assert_eq!(verdict.member.id, id);
        assert!(matches!(verdict.state, IntentState::Unknown { .. }));
    }

    j.settle_by("i2", "confirmed", PERSON);
    let ledger = Ledger::fold(&j.0);
    let first = send_verdict(&ledger, ledger.get("i1").unwrap(), NOW);
    assert_eq!(first.member.id, "i1");
    assert!(matches!(first.state, IntentState::Unknown { .. }));
    // A repeat is still answered by the send that went out.
    assert!(matches!(
        decide(&j, NoKey, OnUnknown::Refuse),
        Admission::AlreadySent { intent, .. } if intent == "i2"
    ));
}

/// Replays share one nonce and so one effect: a replay that went out
/// settles the send it repeated.
#[test]
fn a_replay_that_went_out_settles_the_send_it_repeated() {
    let mut j = Journal::default();
    j.intent("i1", "a", NOW - 10_000, NOW)
        .settle("i1", "unknown")
        .intent_of("i2", "a", NOW - 5_000, NOW, Some("i1"))
        .settle("i2", "confirmed");
    let ledger = Ledger::fold(&j.0);
    let verdict = send_verdict(&ledger, ledger.get("i1").unwrap(), NOW);
    assert_eq!(verdict.member.id, "i2");
    assert!(matches!(verdict.state, IntentState::Confirmed { .. }));
}
