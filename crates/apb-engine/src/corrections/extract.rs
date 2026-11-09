//! The sightings of one run's journal: what a person corrected there, one
//! [`Sighting`] each. A pure fold, one pass, no playbook needed beyond the
//! node kinds the delivering-node rule reads.

use std::collections::{BTreeMap, BTreeSet, VecDeque};

use apb_core::corrections::{CorrectionKind, Sighting};
use apb_core::schema::Playbook;
use apb_core::scrub::Scrubber;

use crate::event::{Event, EventPayload};

/// The longest text a sighting keeps (`apb_core::corrections::TEXT_MAX`).
use apb_core::corrections::TEXT_MAX;

/// One pass over `events`: every correction the journal holds, in journal
/// order. `playbook` is the run's snapshot (the node kinds the
/// delivering-node rule reads); `None` names no delivering node, ever.
pub fn sightings(
    run_id: &str,
    version: &str,
    events: &[Event],
    scrubber: &Scrubber,
    playbook: Option<&Playbook>,
) -> Vec<Sighting> {
    use CorrectionKind as K;
    let mut out: Vec<Sighting> = Vec::new();
    // The delivering node of each gate visit: the `from`s of the hops into
    // the gate since the gate's previous review decision (or the run
    // start), and the deliverers each gate's open request captured: the
    // `from`s the window held when it was asked.
    let mut deliveries: Vec<(String, BTreeSet<String>)> = Vec::new();
    let mut pending: Vec<(String, BTreeSet<String>)> = Vec::new();
    // The questions asked per node, in order: the N-th answer of a node
    // matches its N-th question (count-based, as the event docs say).
    let mut questions: BTreeMap<String, VecDeque<String>> = BTreeMap::new();
    for e in events {
        match &e.payload {
            EventPayload::EdgeTraversed { from, to, .. } => {
                match deliveries.iter_mut().find(|(gate, _)| gate == to) {
                    Some((_, set)) => {
                        set.insert(from.clone());
                    }
                    None => deliveries.push((to.clone(), BTreeSet::from([from.clone()]))),
                }
            }
            EventPayload::ReviewRequested { node, .. } => {
                // This visit's deliverers: the hops the window held, and
                // the window clears for the next visit.
                let set = deliveries
                    .iter()
                    .find(|(gate, _)| gate == node)
                    .map(|(_, set)| set.clone())
                    .unwrap_or_default();
                pending.push((node.clone(), set));
                deliveries.retain(|(gate, _)| gate != node);
            }
            EventPayload::ReviewDecided {
                node,
                decision,
                note,
                plan_digest,
                decided_by,
                ..
            } => {
                // Every decision ends its gate visit, so its deliverers are
                // consumed before any filter: a later visit of the gate never
                // inherits this visit's deliverers.
                let deliverers = pending
                    .iter()
                    .find(|(gate, _)| gate == node)
                    .map(|(_, set)| set.clone())
                    .unwrap_or_default();
                pending.retain(|(gate, _)| gate != node);
                // A publish consent is not a correction; a model's word is
                // not a person's; an approve with or without a note is the
                // person saying it is fine.
                if plan_digest.is_some()
                    || decided_by.is_some()
                    || decision == "approve"
                    || note.is_empty()
                {
                    continue;
                }
                let node_of = delivering_node(playbook, &deliverers);
                out.push(sighting(
                    K::ReviewNote,
                    node_of,
                    Some(node.clone()),
                    note,
                    At {
                        run_id,
                        seq: e.seq,
                        at_ms: e.ts,
                        version,
                    },
                    scrubber,
                ));
            }
            EventPayload::SupervisorAction {
                action,
                node: Some(n),
                detail,
                ..
            } if action == crate::event::supervisor_action::CONTEXT_APPEND => {
                // The note's text is stored scrubbed with a normal key; the
                // draft is refused because the supervisor channel does not
                // tell an agent from a person, not because the text is
                // withheld.
                let mut s = sighting(
                    K::NodeNote,
                    Some(n.clone()),
                    None,
                    detail,
                    At {
                        run_id,
                        seq: e.seq,
                        at_ms: e.ts,
                        version,
                    },
                    scrubber,
                );
                s.via_supervisor = true;
                out.push(s);
            }
            EventPayload::QuestionAsked { node, question, .. } => {
                questions
                    .entry(node.clone())
                    .or_default()
                    .push_back(question.clone());
            }
            EventPayload::QuestionAnswered {
                node,
                answer,
                answered_by,
                ..
            } => {
                let Some(question) = questions.get_mut(node).and_then(VecDeque::pop_front) else {
                    continue;
                };
                let node = node.clone();
                if answered_by != "human" {
                    continue;
                }
                out.push(sighting(
                    K::HumanAnswer,
                    (!node.is_empty()).then_some(node),
                    None,
                    &format!("{question} -> {answer}"),
                    At {
                        run_id,
                        seq: e.seq,
                        at_ms: e.ts,
                        version,
                    },
                    scrubber,
                ));
            }
            EventPayload::AttemptFinished {
                node,
                status,
                rejected_for,
                rejection_fingerprint,
                ..
            } if status == "failed" => {
                let node = Some(node.clone());
                let fp = rejection_fingerprint
                    .as_deref()
                    .and_then(apb_core::corrections::normalize::fingerprint_key);
                // A rejection with neither a reason nor a valid fingerprint
                // says nothing to group; a fingerprint alone is an entry
                // whose text is withheld.
                if rejected_for.is_none() && fp.is_none() {
                    continue;
                }
                let rejected = rejected_for.as_deref().unwrap_or_default();
                out.push(match fp {
                    Some(key) => {
                        // The fingerprint is the key; nothing derived from
                        // the raw reason is kept when the scrubber flags
                        // it, and the stored text is the reason only when
                        // it scrubbed whole.
                        let text = rejected_for.as_deref().and_then(|r| kept_text(scrubber, r));
                        Sighting {
                            kind: K::Rejection,
                            node,
                            gate: None,
                            key,
                            text,
                            run_id: run_id.to_string(),
                            seq: e.seq,
                            at_ms: u64::try_from(e.ts).unwrap_or(0),
                            version: version.to_string(),
                            via_supervisor: false,
                        }
                    }
                    None => sighting(
                        K::Rejection,
                        node,
                        None,
                        rejected,
                        At {
                            run_id,
                            seq: e.seq,
                            at_ms: e.ts,
                            version,
                        },
                        scrubber,
                    ),
                });
            }
            _ => {}
        }
    }
    out.retain(|s| !s.key.is_empty());
    out
}

/// The node a draft fix would change: exactly one distinct deliverer, and
/// it is an `agent_task` or a `finish` with a prompt. None, several (a
/// gate fed by an agent and a script is ambiguous too), or another kind
/// gives `None`.
fn delivering_node(playbook: Option<&Playbook>, deliverers: &BTreeSet<String>) -> Option<String> {
    let playbook = playbook?;
    let mut only = deliverers.iter();
    let (Some(node), None) = (only.next(), only.next()) else {
        return None;
    };
    matches!(
        playbook.node(node).map(|n| &n.kind),
        Some(apb_core::schema::NodeKind::AgentTask { .. })
            | Some(apb_core::schema::NodeKind::Prompt { .. })
            | Some(apb_core::schema::NodeKind::Finish {
                prompt: Some(_),
                ..
            })
    )
    .then(|| node.clone())
}

/// The stored text a sighting keeps: the scrubber's stored-text path, and
/// a text whose every word a redaction marker replaced (the note was one
/// credential-shaped token) reads as withheld.
fn kept_text(scrubber: &Scrubber, text: &str) -> Option<String> {
    scrubber.stored_text(text, TEXT_MAX).filter(|t| {
        t.replace("[redacted]", "")
            .replace(apb_core::token_shape::TOKEN_MARK, "")
            .replace("[email]", "")
            .chars()
            .any(char::is_alphanumeric)
    })
}

/// Builds one sighting: the text through the scrubber's stored-text path,
/// the key from a text that stored whole, the sighting dropped (no key)
/// when the scrubber flags its text and no fingerprint stands in.
/// Where one sighting happened: who saw it, where, when.
struct At<'a> {
    run_id: &'a str,
    seq: u64,
    at_ms: u128,
    version: &'a str,
}

fn sighting(
    kind: CorrectionKind,
    node: Option<String>,
    gate: Option<String>,
    text: &str,
    at: At<'_>,
    scrubber: &Scrubber,
) -> Sighting {
    let at_ms = u64::try_from(at.at_ms).unwrap_or(0);
    let stored = kept_text(scrubber, text);
    let key = match &stored {
        Some(t) => apb_core::corrections::normalize::key_of(kind, node.as_deref(), t),
        None => String::new(),
    };
    Sighting {
        kind,
        node,
        gate,
        key,
        text: stored,
        run_id: at.run_id.to_string(),
        seq: at.seq,
        at_ms,
        version: at.version.to_string(),
        via_supervisor: false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use apb_core::corrections::CorrectionKind as K;
    use apb_core::secret::Secret;

    use crate::event::DecidedBy;

    const PLAYBOOK: &str = "schema: 2\nid: p\nname: p\nversion: 1.0.0\ndefaults: { profile: main }\nnodes:\n  - { id: start, type: start }\n  - { id: work, type: agent_task, prompt: w }\n  - { id: lint, type: agent_task, prompt: l }\n  - { id: review, type: human_review }\n  - { id: done, type: finish, prompt: done, outcome: success }\n  - { id: failed, type: finish, outcome: failure }\nedges:\n  - { from: start, to: work }\n  - { from: work, to: review }\n  - { from: lint, to: review }\n  - { from: review, to: done, condition: { type: review_status, equals: approve } }\n  - { id: rejected, from: review, to: failed, fallback: true }\n";

    fn ev(seq: u64, payload: EventPayload) -> Event {
        Event {
            seq,
            ts: 1_700_000_000_000u128 + u128::from(seq),
            payload,
        }
    }

    fn scrubber() -> Scrubber {
        Scrubber::new(
            vec![Secret::new("hunter2-secret-value")],
            std::path::Path::new("/tmp/proj"),
        )
    }

    fn run_started() -> EventPayload {
        EventPayload::RunStarted {
            playbook: "p".into(),
            version: "1.0.0".into(),
            definition: None,
            external_data: false,
        }
    }

    fn hop(from: &str, to: &str) -> EventPayload {
        EventPayload::hop(from, to, false, true, Vec::new())
    }

    fn review_requested(node: &str) -> EventPayload {
        EventPayload::review_requested(
            node,
            vec!["approve".to_string(), "reject".to_string()],
            String::new(),
        )
    }

    fn review_decided(node: &str, decision: &str, note: &str) -> EventPayload {
        EventPayload::review_decided(node, decision, note, None)
    }

    fn rejected(node: &str, reason: &str, fp: Option<&str>) -> EventPayload {
        EventPayload::AttemptFinished {
            item: None,
            output_truncated: Vec::new(),
            transcript_incomplete: false,
            node: node.into(),
            attempt: 1,
            status: "failed".into(),
            duration_ms: Some(1),
            session: None,
            summary: None,
            rejected_output: Some(reason.into()),
            partial_output: None,
            failure_kind: None,
            usage: None,
            rejected_for: Some(reason.into()),
            rejection_fingerprint: fp.map(str::to_string),
            stalled: false,
        }
    }

    fn take(mut v: Vec<Sighting>) -> Sighting {
        assert_eq!(v.len(), 1, "{v:#?}");
        v.remove(0)
    }

    /// A person's reject with a note, delivered from the one agent task:
    /// kind review_note, node work, gate review.
    #[test]
    fn a_reject_with_a_note_names_its_delivering_node() {
        let pb = Playbook::from_yaml(PLAYBOOK).unwrap();
        let events = vec![
            ev(1, run_started()),
            ev(2, hop("work", "review")),
            ev(3, review_requested("review")),
            ev(
                4,
                review_decided("review", "reject", "add tests for the parser"),
            ),
        ];
        let s = take(sightings("p-1", "1.0.0", &events, &scrubber(), Some(&pb)));
        assert_eq!(s.kind, K::ReviewNote);
        assert_eq!(s.node.as_deref(), Some("work"));
        assert_eq!(s.gate.as_deref(), Some("review"));
        assert_eq!(s.text.as_deref(), Some("add tests for the parser"));
        assert!(!s.via_supervisor);
        assert!(s.key.starts_with("t:"));
    }

    /// A gate fed by two branches gives node None; one branch finishing
    /// later does not make the other branch the deliverer.
    #[test]
    fn a_gate_fed_by_two_branches_has_no_node() {
        let pb = Playbook::from_yaml(PLAYBOOK).unwrap();
        let events = vec![
            ev(1, run_started()),
            ev(2, hop("work", "review")),
            ev(3, hop("lint", "review")),
            ev(4, review_requested("review")),
            ev(5, review_decided("review", "reject", "add tests")),
        ];
        let s = take(sightings("p-1", "1.0.0", &events, &scrubber(), Some(&pb)));
        assert_eq!(s.node, None, "a join of two branches has no draft node");
    }

    /// A gate fed by one agent task names it although another branch
    /// finished later (its hop belongs to the next visit).
    #[test]
    fn a_late_branch_does_not_steal_the_deliverer() {
        let pb = Playbook::from_yaml(PLAYBOOK).unwrap();
        let events = vec![
            ev(1, run_started()),
            ev(2, hop("work", "review")),
            ev(3, review_requested("review")),
            ev(4, review_decided("review", "reject", "add tests")),
            ev(5, hop("lint", "review")),
        ];
        let s = take(sightings("p-1", "1.0.0", &events, &scrubber(), Some(&pb)));
        assert_eq!(s.node.as_deref(), Some("work"));
    }

    /// An approve ends its gate visit too: when the loop comes back to the
    /// gate from another node, a reject there names that node, never the
    /// approved visit's deliverer.
    #[test]
    fn an_approved_visit_does_not_leave_its_deliverer_behind() {
        let pb = Playbook::from_yaml(PLAYBOOK).unwrap();
        let events = vec![
            ev(1, run_started()),
            ev(2, hop("lint", "review")),
            ev(3, review_requested("review")),
            ev(4, review_decided("review", "approve", "")),
            ev(5, hop("work", "review")),
            ev(6, review_requested("review")),
            ev(7, review_decided("review", "reject", "add tests")),
        ];
        let s = take(sightings("p-1", "1.0.0", &events, &scrubber(), Some(&pb)));
        assert_eq!(s.node.as_deref(), Some("work"));
    }

    /// A gate fed by an agent task and a script has two deliverers: no
    /// draft node, although only one of them could take a prompt line.
    #[test]
    fn a_gate_fed_by_an_agent_and_a_script_has_no_node() {
        let pb = Playbook::from_yaml(&PLAYBOOK.replace(
            "  - { id: lint, type: agent_task, prompt: l }",
            "  - { id: lint, type: script, script: scripts/lint.sh, runner: sh }",
        ))
        .unwrap();
        let events = vec![
            ev(1, run_started()),
            ev(2, hop("work", "review")),
            ev(3, hop("lint", "review")),
            ev(4, review_requested("review")),
            ev(5, review_decided("review", "reject", "add tests")),
        ];
        let s = take(sightings("p-1", "1.0.0", &events, &scrubber(), Some(&pb)));
        assert_eq!(s.node, None);
    }

    /// A rejection that carries a fingerprint but no reason is an entry
    /// keyed by the fingerprint, its text withheld; with neither it adds
    /// nothing.
    #[test]
    fn a_fingerprint_without_a_reason_is_an_entry() {
        let pb = Playbook::from_yaml(PLAYBOOK).unwrap();
        let mut only_fp = rejected("work", "x", Some("0123456789abcdef0123456789abcdef"));
        let mut neither = rejected("work", "x", None);
        for p in [&mut only_fp, &mut neither] {
            if let EventPayload::AttemptFinished { rejected_for, .. } = p {
                *rejected_for = None;
            }
        }
        let events = vec![ev(1, run_started()), ev(2, only_fp), ev(3, neither)];
        let s = take(sightings("p-1", "1.0.0", &events, &scrubber(), Some(&pb)));
        assert_eq!(s.kind, K::Rejection);
        assert_eq!(s.key, "f:0123456789abcdef0123456789abcdef");
        assert_eq!(s.text, None);
    }

    /// approve with a note, a publish consent and decided_by model add
    /// nothing.
    #[test]
    fn approve_publish_and_model_add_nothing() {
        let pb = Playbook::from_yaml(PLAYBOOK).unwrap();
        let events = vec![
            ev(1, run_started()),
            ev(2, review_decided("review", "approve", "ok")),
            ev(
                3,
                EventPayload::review_decided(
                    "review",
                    "reject",
                    "ship it",
                    Some("sha256:plan".into()),
                ),
            ),
            ev(
                4,
                EventPayload::ReviewDecided {
                    node: "review".into(),
                    decision: "reject".into(),
                    note: "ship it".into(),
                    plan_digest: None,
                    decided_by: Some(DecidedBy::Model),
                    binding: None,
                    via: None,
                },
            ),
        ];
        assert!(
            sightings("p-1", "1.0.0", &events, &scrubber(), Some(&pb)).is_empty(),
            "an approve, a publish consent and a model's word are not corrections"
        );
    }

    /// A context_append with a node is a via_supervisor node note; one
    /// without a node adds nothing.
    #[test]
    fn a_context_append_is_a_supervisor_node_note() {
        let pb = Playbook::from_yaml(PLAYBOOK).unwrap();
        let events = vec![
            ev(1, run_started()),
            ev(
                2,
                EventPayload::SupervisorAction {
                    action: crate::event::supervisor_action::CONTEXT_APPEND.into(),
                    node: Some("work".into()),
                    detail: "always run the tests first".into(),
                },
            ),
            ev(
                3,
                EventPayload::SupervisorAction {
                    action: crate::event::supervisor_action::CONTEXT_APPEND.into(),
                    node: None,
                    detail: "whole-run note".into(),
                },
            ),
        ];
        let mut v = sightings("p-1", "1.0.0", &events, &scrubber(), Some(&pb));
        assert_eq!(v.len(), 1, "{v:#?}");
        let s = v.remove(0);
        assert_eq!(s.kind, K::NodeNote);
        assert_eq!(s.node.as_deref(), Some("work"));
        assert!(s.via_supervisor);
        assert_eq!(s.text.as_deref(), Some("always run the tests first"));
    }

    /// A human answer with its question; a timeout answer adds nothing.
    #[test]
    fn a_human_answer_carries_its_question() {
        let pb = Playbook::from_yaml(PLAYBOOK).unwrap();
        let mut events = vec![ev(1, run_started())];
        events.push(ev(
            2,
            EventPayload::QuestionAsked {
                node: "work".into(),
                question: "Which database?".into(),
                options: Vec::new(),
            },
        ));
        events.push(ev(
            3,
            EventPayload::QuestionAnswered {
                node: "work".into(),
                answer: "postgres".into(),
                answered_by: "human".into(),
            },
        ));
        events.push(ev(
            4,
            EventPayload::QuestionAsked {
                node: "work".into(),
                question: "Which region?".into(),
                options: Vec::new(),
            },
        ));
        events.push(ev(
            5,
            EventPayload::QuestionAnswered {
                node: "work".into(),
                answer: String::new(),
                answered_by: "timeout".into(),
            },
        ));
        let s = take(sightings("p-1", "1.0.0", &events, &scrubber(), Some(&pb)));
        assert_eq!(s.kind, K::HumanAnswer);
        assert_eq!(s.node.as_deref(), Some("work"));
        assert_eq!(s.text.as_deref(), Some("Which database? -> postgres"));
    }

    /// Two nodes asking at once: each answer pairs with its own node's
    /// question, not with the latest one asked.
    #[test]
    fn an_answer_pairs_with_its_own_nodes_question() {
        let pb = Playbook::from_yaml(PLAYBOOK).unwrap();
        let asked = |node: &str, q: &str| EventPayload::QuestionAsked {
            node: node.into(),
            question: q.into(),
            options: Vec::new(),
        };
        let answered = |node: &str, a: &str| EventPayload::QuestionAnswered {
            node: node.into(),
            answer: a.into(),
            answered_by: "human".into(),
        };
        let events = vec![
            ev(1, run_started()),
            ev(2, asked("work", "Which database?")),
            ev(3, asked("lint", "Which style guide?")),
            ev(4, answered("work", "postgres")),
            ev(5, answered("lint", "the house one")),
        ];
        let got = sightings("p-1", "1.0.0", &events, &scrubber(), Some(&pb));
        let texts: Vec<(Option<&str>, Option<&str>)> = got
            .iter()
            .map(|s| (s.node.as_deref(), s.text.as_deref()))
            .collect();
        assert_eq!(
            texts,
            vec![
                (Some("work"), Some("Which database? -> postgres")),
                (Some("lint"), Some("Which style guide? -> the house one")),
            ]
        );
    }

    /// A rejected attempt is a rejection; with a fingerprint the key is
    /// `f:`. An applied retry advice adds nothing.
    #[test]
    fn a_rejection_keys_on_its_fingerprint() {
        let pb = Playbook::from_yaml(PLAYBOOK).unwrap();
        let events = vec![
            ev(1, run_started()),
            ev(
                2,
                rejected(
                    "work",
                    "the report claims a green build that failed",
                    Some("0123456789abcdef0123456789abcdef"),
                ),
            ),
            ev(
                3,
                EventPayload::DecisionMade {
                    item: None,
                    use_site: "retry_advice".into(),
                    node: Some("work".into()),
                    attempt: Some(1),
                    provider: None,
                    model: None,
                    calibrated: false,
                    mode: "enforce".into(),
                    questions_digest: String::new(),
                    state_digest: String::new(),
                    state_bytes: 0,
                    output_chars: None,
                    answers: Default::default(),
                    applied: true,
                    would_change: None,
                    baseline: None,
                    latency_ms: 0,
                    input_tokens: None,
                    cost_usd: None,
                    cost_estimated: false,
                    cached: false,
                    error: None,
                    enforce_refused: None,
                    join: Default::default(),
                    edge_addresses: Default::default(),
                    privacy_class: None,
                },
            ),
        ];
        let s = take(sightings("p-1", "1.0.0", &events, &scrubber(), Some(&pb)));
        assert_eq!(s.kind, K::Rejection);
        assert_eq!(s.node.as_deref(), Some("work"));
        assert_eq!(s.key, "f:0123456789abcdef0123456789abcdef");
    }

    /// Two notes differing only in a secret value share a key; a note that
    /// is only a credential-shaped token is dropped, and with a
    /// fingerprint kept with text None.
    #[test]
    fn a_secret_never_splits_or_stays() {
        let pb = Playbook::from_yaml(PLAYBOOK).unwrap();
        let scrub = scrubber();
        let events = vec![
            ev(1, run_started()),
            ev(
                2,
                review_decided("review", "reject", "the token is hunter2-secret-value"),
            ),
            ev(
                3,
                review_decided("review", "reject", "the token is [redacted]"),
            ),
        ];
        let v = sightings("p-1", "1.0.0", &events, &scrub, Some(&pb));
        assert_eq!(v.len(), 2, "{v:#?}");
        assert_eq!(v[0].key, v[1].key, "the secret value never splits a key");
        assert!(
            !v[0].text.as_deref().unwrap_or_default().contains("hunter2"),
            "the stored text holds the secret"
        );

        // A note that is only a credential-shaped token: dropped whole.
        let events = vec![
            ev(1, run_started()),
            ev(
                2,
                review_decided(
                    "review",
                    "reject",
                    "glpat-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
                ),
            ),
        ];
        assert!(
            sightings("p-1", "1.0.0", &events, &scrub, Some(&pb)).is_empty(),
            "a token-shaped note is dropped"
        );

        // The same note on a rejection with a fingerprint: kept, no text.
        let events = vec![
            ev(1, run_started()),
            ev(
                2,
                rejected(
                    "work",
                    "glpat-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
                    Some("0123456789abcdef"),
                ),
            ),
        ];
        let s = take(sightings("p-1", "1.0.0", &events, &scrub, Some(&pb)));
        assert_eq!(s.key, "f:0123456789abcdef");
        assert_eq!(s.text, None, "nothing derived from the raw text is kept");
    }

    /// A run without the playbook snapshot names no delivering node.
    #[test]
    fn without_the_snapshot_no_node_is_named() {
        let events = vec![
            ev(1, run_started()),
            ev(2, hop("work", "review")),
            ev(3, review_requested("review")),
            ev(4, review_decided("review", "reject", "add tests")),
        ];
        let s = take(sightings("p-1", "1.0.0", &events, &scrubber(), None));
        assert_eq!(s.node, None);
    }
}
