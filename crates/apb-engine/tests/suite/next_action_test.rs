//! Next actions outside one observation: the start actions of a refused
//! start and of a fit report, and the request an answer or a decision
//! names, checked inside the lock that guards its append.

use apb_engine::gate::launch::{LaunchContext, LaunchKind};
use std::fs;
use std::path::Path;

use apb_core::registry::init_project;
use apb_core::scope::{Origin, PlaybookRef};
use apb_engine::EngineError;
use apb_engine::consent::Confirmation;
use apb_engine::event::{EventLog, EventPayload};
use apb_engine::next_action::start::{Remedy, StartActionCode, check_start, from_fit};
use apb_engine::next_action::{ArgValue, Principal};

use crate::common::{EnvVar, env_lock};

fn seed(root: &Path, id: &str, yaml: &str) {
    let _ = init_project(root);
    let vdir = root.join(".apb/playbooks").join(id).join("1.0.0");
    fs::create_dir_all(&vdir).unwrap();
    fs::write(vdir.join("playbook.yaml"), yaml).unwrap();
    fs::write(
        root.join(".apb/playbooks").join(id).join("current"),
        "1.0.0",
    )
    .unwrap();
}

fn linear(id: &str, head: &str, a_extra: &str) -> String {
    format!(
        "schema: 2\nid: {id}\nname: {id}\nversion: 1.0.0\n{head}nodes:\n  - {{ id: start, type: start }}\n  - {{ id: a, type: prompt, prompt: \"x\"{a_extra} }}\n  - {{ id: done, type: finish, outcome: success }}\nedges:\n  - {{ from: start, to: a }}\n  - {{ from: a, to: done }}\n"
    )
}

fn wref(id: &str) -> PlaybookRef {
    PlaybookRef {
        origin: Origin::Project { workspace_id: None },
        id: id.into(),
        version: None,
    }
}

fn given<'a>(args: &'a [apb_engine::next_action::ActionArg], name: &str) -> &'a str {
    match &args.iter().find(|a| a.name == name).expect(name).value {
        ArgValue::Given { value } => value,
        other => panic!("{name} is not given: {other:?}"),
    }
}

/// An irreversible start nobody approved gets both answers in one refusal
/// (the acknowledge and the consent with its nonce), and one retry that
/// carries both goes through. Nothing is written for the refused start.
#[test]
fn an_irreversible_untrusted_start_gets_two_actions_and_one_retry_passes() {
    let _env = env_lock();
    let cfg = tempfile::tempdir().unwrap();
    let _cfg = EnvVar::set("APB_CONFIG_DIR", cfg.path());
    let dir = tempfile::tempdir().unwrap();
    seed(
        dir.path(),
        "rel",
        &linear("rel", "", ", effects: [irreversible]"),
    );

    let refused = check_start(
        dir.path(),
        &wref("rel"),
        false,
        false,
        None,
        &LaunchContext::dashboard(LaunchKind::Start),
    )
    .expect_err("refused");
    let codes: Vec<_> = refused.actions.iter().map(|a| a.code).collect();
    assert!(
        codes.contains(&StartActionCode::AcknowledgeUntrusted),
        "{codes:?}"
    );
    assert!(
        codes.contains(&StartActionCode::ConfirmIrreversibleStart),
        "{codes:?}"
    );
    let confirm = refused
        .actions
        .iter()
        .find(|a| a.code == StartActionCode::ConfirmIrreversibleStart)
        .unwrap();
    assert_eq!(confirm.principals, vec![Principal::Person]);
    let approve = refused
        .actions
        .iter()
        .find(|a| matches!(a.remedy, Some(Remedy::ApproveDigest { .. })))
        .expect("the lasting fix: an approval by digest");
    assert_eq!(approve.code, StartActionCode::FixRequirement);
    assert!(
        !dir.path()
            .join(".apb/runs")
            .read_dir()
            .is_ok_and(|mut d| d.next().is_some())
    );

    let ack = given(
        &refused
            .actions
            .iter()
            .find(|a| a.code == StartActionCode::AcknowledgeUntrusted)
            .unwrap()
            .args,
        "acknowledge_untrusted",
    ) == "true";
    let nonce = Confirmation::Nonce(given(&confirm.args, "confirm_irreversible").to_string());
    let (_permit, note) = check_start(
        dir.path(),
        &wref("rel"),
        ack,
        false,
        Some(&nonce),
        &LaunchContext::dashboard(LaunchKind::Start),
    )
    .expect("the retry passes");
    assert_eq!(note, None);
}

/// A `requires` refusal names what to provide; a trusted start without
/// consent gets the consent action alone.
#[test]
fn requires_and_consent_refusals_give_their_actions() {
    let _env = env_lock();
    let cfg = tempfile::tempdir().unwrap();
    let _cfg = EnvVar::set("APB_CONFIG_DIR", cfg.path());
    let dir = tempfile::tempdir().unwrap();
    seed(
        dir.path(),
        "req",
        &linear("req", "requires:\n  files: [CHANGELOG.md]\n", ""),
    );
    let refused = check_start(
        dir.path(),
        &wref("req"),
        true,
        false,
        None,
        &LaunchContext::dashboard(LaunchKind::Start),
    )
    .expect_err("refused");
    assert_eq!(refused.refusal["policy"], "requires_unmet");
    assert!(matches!(
        &refused.actions[..],
        [a] if a.code == StartActionCode::FixRequirement
            && matches!(&a.remedy, Some(Remedy::Manual { detail }) if detail.contains("CHANGELOG.md"))
    ));

    seed(
        dir.path(),
        "rel",
        &linear("rel", "effects: [irreversible]\n", ""),
    );
    let refused = check_start(
        dir.path(),
        &wref("rel"),
        true,
        false,
        None,
        &LaunchContext::dashboard(LaunchKind::Start),
    )
    .expect_err("refused");
    let codes: Vec<_> = refused.actions.iter().map(|a| a.code).collect();
    assert_eq!(codes, vec![StartActionCode::ConfirmIrreversibleStart]);
}

/// A param without a default and nothing remembered is a note in the fit
/// report and still an action: provide it.
#[test]
fn a_real_fit_report_with_a_missing_param_gives_provide_param() {
    let _env = env_lock();
    let cfg = tempfile::tempdir().unwrap();
    let _cfg = EnvVar::set("APB_CONFIG_DIR", cfg.path());
    let dir = tempfile::tempdir().unwrap();
    seed(
        dir.path(),
        "p",
        &linear("p", "params:\n  - { name: topic, type: text }\n", ""),
    );
    let fit = apb_engine::fit::assess(dir.path(), &wref("p")).unwrap();
    let item = fit
        .setup
        .iter()
        .find(|i| i.code == "param_needs_value")
        .expect("the note");
    assert_eq!(item.severity, apb_engine::fit::FitSeverity::Note);
    let actions = from_fit(&fit);
    let provide = actions
        .iter()
        .find(|a| matches!(&a.remedy, Some(Remedy::ProvideParam { name }) if name == "topic"))
        .expect("provide the param");
    assert_eq!(provide.code, StartActionCode::FixRequirement);
    assert!(matches!(provide.args[0].value, ArgValue::Supplied { .. }));
    // Untrusted: the acknowledge and the approval by digest.
    assert!(
        actions
            .iter()
            .any(|a| a.code == StartActionCode::AcknowledgeUntrusted)
    );
}

// --- request identity ---------------------------------------------------

const GATED: &str = "schema: 2\nid: g\nname: g\nversion: 1.0.0\ndefaults: { profile: x }\nnodes:\n  - { id: s, type: start }\n  - { id: ask, type: agent_task, prompt: hi, interactive: true }\n  - { id: gate, type: human_review, options: [approved, rejected] }\n  - { id: f, type: finish, outcome: success }\nedges:\n  - { from: s, to: ask }\n  - { from: ask, to: gate }\n  - { from: gate, to: f }\n";

fn gated_run(root: &Path) -> (std::path::PathBuf, EventLog) {
    let dir = root.join(".apb/runs/r");
    let mut log = EventLog::create(&dir).unwrap();
    fs::write(dir.join("playbook.yaml"), GATED).unwrap();
    log.append(EventPayload::RunStarted {
        playbook: "g".into(),
        version: "1.0.0".into(),
    })
    .unwrap();
    (dir, log)
}

fn append(log: &mut EventLog, v: serde_json::Value) -> u64 {
    log.append(serde_json::from_value(v).unwrap()).unwrap().seq
}

fn ask(dir: &Path, log: &mut EventLog) -> u64 {
    apb_engine::question::post_question(dir, "ask", 1, "go?", vec![]).unwrap();
    append(
        log,
        serde_json::json!({"type": "question_asked", "node": "ask", "attempt": 1, "question": "go?", "options": []}),
    )
}

fn review(log: &mut EventLog) -> u64 {
    append(
        log,
        serde_json::json!({"type": "review_requested", "node": "gate", "options": ["approved", "rejected"]}),
    )
}

fn decision(d: &str) -> apb_engine::review::ReviewCommand {
    apb_engine::review::ReviewCommand {
        node: "gate".into(),
        decision: d.into(),
        note: String::new(),
    }
}

/// A decision for request A after the gate looped and opened request B is
/// a conflict; without a request it is accepted as before.
#[test]
fn a_decision_for_a_replaced_review_request_is_a_conflict() {
    use apb_engine::review::{post_decision, post_decision_to};
    let tmp = tempfile::tempdir().unwrap();
    let (dir, mut log) = gated_run(tmp.path());
    let a = review(&mut log);
    // Request A is decided through the channel and the drive journals it.
    post_decision_to(&dir, decision("rejected"), None, Some(a)).expect("request A");
    append(
        &mut log,
        serde_json::json!({"type": "review_decided", "node": "gate", "decision": "rejected", "note": ""}),
    );
    let b = review(&mut log);
    let err = post_decision_to(&dir, decision("approved"), None, Some(a)).unwrap_err();
    assert!(matches!(err, EngineError::Conflict(_)), "{err}");
    post_decision_to(&dir, decision("approved"), None, Some(b)).expect("the open request");
    let again = post_decision_to(&dir, decision("rejected"), None, Some(b)).unwrap_err();
    assert!(matches!(again, EngineError::AlreadyAnswered(_)), "{again}");

    let tmp = tempfile::tempdir().unwrap();
    let (dir, mut log) = gated_run(tmp.path());
    review(&mut log);
    post_decision(&dir, decision("approved"), None).expect("no request: as today");
}

/// The timeout path queued the default answer for question A and the drive
/// has not journaled it: an answer naming A is already answered, with or
/// without a next question in the channel. A later round is a conflict.
#[test]
fn an_answer_for_a_question_with_a_queued_default_is_already_answered() {
    use apb_engine::question::{
        post_answer, post_answer_if_unanswered, post_answer_to, post_question,
    };
    let tmp = tempfile::tempdir().unwrap();
    let (dir, mut log) = gated_run(tmp.path());
    let a = ask(&dir, &mut log);
    post_answer_if_unanswered(&dir, "ask", "go", "timeout", 0)
        .unwrap()
        .expect("queued");
    let err = post_answer_to(&dir, Some("ask"), "no", "human", Some(a)).unwrap_err();
    assert!(matches!(err, EngineError::AlreadyAnswered(_)), "{err}");
    // A next question in the channel, not journaled yet: still A's answer.
    post_question(&dir, "ask", 2, "again?", vec![]).unwrap();
    let err = post_answer_to(&dir, Some("ask"), "no", "human", Some(a)).unwrap_err();
    assert!(matches!(err, EngineError::AlreadyAnswered(_)), "{err}");
    // The drive journals the answer and the next round.
    append(
        &mut log,
        serde_json::json!({"type": "question_answered", "node": "ask", "answer": "go", "answered_by": "timeout"}),
    );
    let b = append(
        &mut log,
        serde_json::json!({"type": "question_asked", "node": "ask", "attempt": 2, "question": "again?", "options": []}),
    );
    let err = post_answer_to(&dir, Some("ask"), "no", "human", Some(a)).unwrap_err();
    assert!(matches!(err, EngineError::Conflict(_)), "{err}");
    post_answer_to(&dir, Some("ask"), "yes", "human", Some(b)).expect("the open round");
    // Without a request: appended as today.
    post_answer(&dir, Some("ask"), "extra", "human").expect("as today");
}

fn plan(log: &mut EventLog, nonce: &str) -> u64 {
    append(
        log,
        serde_json::json!({"type": "publish_planned", "node": "gate", "from": "ask", "from_seq": 1,
            "branch": "apb/x", "local_branch": "refs/heads/main", "after": "bb", "remote_before": "aa",
            "url": "file:///r.git", "commits": [], "outside_push_suspected": false,
            "plan_digest": format!("sha256:{nonce}"), "nonce": nonce}),
    );
    append(
        log,
        serde_json::json!({"type": "review_requested", "node": "gate", "options": ["publish", "skip"]}),
    )
}

/// A publish decision queued for plan A whose request was withdrawn never
/// answers plan B: a decision naming B's request, with B's nonce, is
/// accepted, and a second one for B is refused.
#[test]
fn a_decision_queued_for_a_withdrawn_plan_does_not_block_the_next_plan() {
    use apb_engine::review::{ReviewCommand, post_decision_to};
    let tmp = tempfile::tempdir().unwrap();
    let (dir, mut log) = gated_run(tmp.path());
    fs::write(
        dir.join("playbook.yaml"),
        "schema: 2\nid: g\nname: g\nversion: 1.0.0\nnodes:\n  - { id: s, type: start }\n  - { id: ask, type: script, script: \"true\", runner: sh }\n  - { id: gate, type: publish, from: ask, remote: origin, branch: \"apb/x\" }\n  - { id: f, type: finish, outcome: success }\nedges:\n  - { from: s, to: ask }\n  - { from: ask, to: gate }\n  - { from: gate, to: f }\n",
    )
    .unwrap();
    let publish = |d: &str| ReviewCommand {
        node: "gate".into(),
        decision: d.into(),
        note: String::new(),
    };
    let a = plan(&mut log, "nonce-a");
    post_decision_to(&dir, publish("publish"), Some("nonce-a".into()), Some(a)).expect("plan A");
    append(
        &mut log,
        serde_json::json!({"type": "review_withdrawn", "node": "gate"}),
    );
    let b = plan(&mut log, "nonce-b");
    post_decision_to(&dir, publish("publish"), Some("nonce-b".into()), Some(b))
        .expect("plan B is open");
    let again =
        post_decision_to(&dir, publish("skip"), Some("nonce-b".into()), Some(b)).unwrap_err();
    assert!(matches!(again, EngineError::AlreadyAnswered(_)), "{again}");
}

/// Rounds of the concurrency tests: each starts two posters at one barrier.
const ROUNDS: usize = 50;

/// Runs `post(i)` on two threads released together; the results in order.
fn race<T: Send + 'static>(post: impl Fn(usize) -> T + Send + Sync + 'static) -> Vec<T> {
    let post = std::sync::Arc::new(post);
    let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
    (0..2)
        .map(|i| {
            let (post, barrier) = (post.clone(), barrier.clone());
            std::thread::spawn(move || {
                barrier.wait();
                post(i)
            })
        })
        .collect::<Vec<_>>()
        .into_iter()
        .map(|h| h.join().unwrap())
        .collect()
}

/// Two answers to the same request released at once, round after round:
/// exactly one is appended each time, the other is already answered.
#[test]
fn two_concurrent_answers_to_one_request_append_one() {
    for round in 0..ROUNDS {
        let tmp = tempfile::tempdir().unwrap();
        let (dir, mut log) = gated_run(tmp.path());
        let a = ask(&dir, &mut log);
        let target = dir.clone();
        let results = race(move |i| {
            apb_engine::question::post_answer_to(
                &target,
                Some("ask"),
                &format!("a{i}"),
                "human",
                Some(a),
            )
        });
        assert_eq!(
            results.iter().filter(|r| r.is_ok()).count(),
            1,
            "round {round}: {results:?}"
        );
        assert!(
            results
                .iter()
                .any(|r| matches!(r, Err(EngineError::AlreadyAnswered(_)))),
            "round {round}: {results:?}"
        );
        let answers = apb_engine::question::read_answers_after(&dir, None).unwrap();
        assert_eq!(answers.len(), 1, "round {round}");
    }
}

/// Two decisions for the same review request released at once, round
/// after round: exactly one is queued each time.
#[test]
fn two_concurrent_decisions_for_one_request_append_one() {
    for round in 0..ROUNDS {
        let tmp = tempfile::tempdir().unwrap();
        let (dir, mut log) = gated_run(tmp.path());
        let b = review(&mut log);
        let target = dir.clone();
        let results = race(move |i| {
            let d = if i == 0 { "approved" } else { "rejected" };
            apb_engine::review::post_decision_to(&target, decision(d), None, Some(b))
        });
        assert_eq!(
            results.iter().filter(|r| r.is_ok()).count(),
            1,
            "round {round}: {results:?}"
        );
        assert!(
            results
                .iter()
                .any(|r| matches!(r, Err(EngineError::AlreadyAnswered(_)))),
            "round {round}: {results:?}"
        );
        let queued = apb_engine::review::read_reviews_after(&dir, None).unwrap();
        assert_eq!(queued.len(), 1, "round {round}");
    }
}
