//! The decision-model grader over the non-code fixture (an inbox triage run
//! against a CRM connector) and a local stub provider: what it cites, what
//! it counts as read, when a level is `not_enough_data`, when it counts for
//! a promotion, and the daily cap.

use std::fs;
use std::path::{Path, PathBuf};

use apb_core::quality::grade::{GraderId, Level};
use apb_decide::testing::{StubResponse, StubServer};
use apb_engine::grade::{ModelGraded, grade_model, grades_of};
use serde_json::{Value, json};

use crate::common;
use crate::quality_common::{CRM_JOURNAL, run_dir};

/// The inbox playbook's snapshot with one criterion of its own over `reads`.
fn snapshot(reads: &str) -> String {
    format!(
        "schema: 2\nid: inbox-triage\nname: Inbox triage\nversion: 1.0.0\nquality:\n  criteria_grader: model\n  criteria:\n    - id: right-action\n      evidence: both\n      reads: [{reads}]\n      levels: {{ good: \"every message got the action it asked for\", partial: \"one message got the wrong action\", poor: \"a contact was deleted that should not have been\" }}\nnodes:\n  - {{ id: start, type: start }}\n  - {{ id: triage, type: agent_task, prompt: triage, profile: x }}\n  - {{ id: act, type: agent_task, prompt: act, profile: x }}\n  - {{ id: done, type: finish, outcome: success }}\nedges:\n  - {{ from: start, to: triage }}\n  - {{ from: triage, to: act }}\n  - {{ from: act, to: done }}\n"
    )
}

fn config(cfg: &Path, base_url: &str, mode: &str, extra: &str) {
    fs::write(
        cfg.join("decisions.yaml"),
        format!(
            "version: 1\nmode: enforce\nproviders:\n  - {{ id: stub, kind: systemone, base_url: \"{base_url}\", model: jev-1.13.0 }}\n{extra}uses:\n  grade: {{ mode: {mode}, max_requests_per_day: 1 }}\n"
        ),
    )
    .unwrap();
}

fn choice(value: &str, confidence: f64) -> Value {
    json!({"type": "choice", "choice": value, "probabilities": { value: 1.0 }, "confidence": confidence})
}

fn reply(level: &str, confidence: f64, support: &str) -> StubResponse {
    StubResponse::json(
        200,
        json!({
            "model": "jev-1.13.0",
            "answers": {
                "level": choice(level, confidence),
                "support_good": choice(support, 0.9),
                "support_partial": choice("none", 0.9),
                "support_poor": choice("none", 0.9),
            },
            "usage": {"input_tokens": 400, "output_tokens": 4}
        })
        .to_string(),
    )
}

struct Fixture {
    root: tempfile::TempDir,
    cfg: tempfile::TempDir,
}

impl Fixture {
    fn new() -> Self {
        let f = Fixture {
            root: tempfile::tempdir().unwrap(),
            cfg: tempfile::tempdir().unwrap(),
        };
        fs::create_dir_all(f.root.path().join(".apb")).unwrap();
        f
    }

    /// Points the config dir at this fixture; the caller holds
    /// `common::env_lock` for as long as the guard lives.
    fn env(&self) -> common::EnvVar {
        common::EnvVar::set("APB_CONFIG_DIR", self.cfg.path())
    }

    fn run(&self, id: &str, journal: &str, reads: &str) -> PathBuf {
        run_dir(self.root.path(), id, journal, &snapshot(reads))
    }
}

fn graded(out: ModelGraded) -> apb_core::quality::grade::Grade {
    match out {
        ModelGraded::Graded(g) => *g,
        ModelGraded::Skipped { reason } => panic!("skipped: {reason}"),
    }
}

#[test]
fn a_calibrated_model_in_enforce_cites_what_it_was_sent_and_counts() {
    let _l = common::env_lock();
    let f = Fixture::new();
    let _env = f.env();
    let server = StubServer::start(vec![reply("good", 0.9, "o:act")]);
    config(f.cfg.path(), &server.base_url, "enforce", "");
    let dir = f.run("r1", CRM_JOURNAL, "outputs, connector_calls");
    let g = graded(grade_model(&dir).unwrap());
    assert_eq!(g.grader, GraderId::MODEL);
    assert_eq!(g.criteria.len(), 1, "only the playbook's own criteria");
    let c = &g.criteria[0];
    assert_eq!(c.level, Level::Good);
    assert_eq!(c.cites[0].r#ref, "o:act");
    assert!(c.counts_for_promotion);
    // Every view it was sent counts as read in full in the trace.
    for view in ["outputs", "connector_calls"] {
        let read = g.reading.views.iter().find(|v| v.view == view).unwrap();
        assert!(read.ended && read.read_share == 1.0, "{view}: {read:?}");
    }
    let journal_view = g
        .reading
        .views
        .iter()
        .find(|v| v.view == "journal")
        .unwrap();
    assert!(!journal_view.ended, "a view not sent is not read");
    assert_eq!(grades_of(&dir).len(), 1);
}

#[test]
fn advise_or_an_uncalibrated_provider_never_counts() {
    let _l = common::env_lock();
    let f = Fixture::new();
    let _env = f.env();
    let server = StubServer::start(vec![reply("good", 0.9, "o:act")]);
    config(f.cfg.path(), &server.base_url, "advise", "");
    let dir = f.run("r1", CRM_JOURNAL, "outputs, connector_calls");
    let g = graded(grade_model(&dir).unwrap());
    assert_eq!(g.criteria[0].level, Level::Good);
    assert!(!g.criteria[0].counts_for_promotion);

    // The fake provider reports itself uncalibrated: enforce is not enough.
    let _fake = common::EnvVar::set("APB_DECISIONS_ALLOW_FAKE", "1");
    fs::write(
        f.cfg.path().join("decisions.yaml"),
        "version: 1\nmode: enforce\nproviders:\n  - id: fake\n    kind: fake\n    answers:\n      level: { type: choice, choice: good, probabilities: { good: 1.0 }, confidence: 0.9 }\n      support_good: { type: choice, choice: \"o:act\", probabilities: { \"o:act\": 1.0 } }\nuses:\n  grade: { mode: enforce }\n",
    )
    .unwrap();
    let dir = f.run("r2", CRM_JOURNAL, "outputs, connector_calls");
    let g = graded(grade_model(&dir).unwrap());
    assert_eq!(g.criteria[0].level, Level::Good);
    assert!(!g.criteria[0].counts_for_promotion);
}

#[test]
fn a_low_confidence_level_is_not_enough_data() {
    let _l = common::env_lock();
    let f = Fixture::new();
    let _env = f.env();
    let server = StubServer::start(vec![reply("good", 0.5, "o:act")]);
    config(f.cfg.path(), &server.base_url, "enforce", "");
    let dir = f.run("r1", CRM_JOURNAL, "outputs, connector_calls");
    let c = &graded(grade_model(&dir).unwrap()).criteria[0];
    assert_eq!(c.level, Level::NotEnoughData);
    assert_eq!(c.reason_code, "low_confidence");
    assert!(c.cites.is_empty());
}

/// The CRM journal with `extra` more failing connector calls before the
/// run ends.
fn noisy_journal(extra: u64) -> String {
    let mut lines: Vec<String> = CRM_JOURNAL.lines().map(str::to_string).collect();
    let end = lines.pop().unwrap();
    for i in 0..extra {
        lines.push(format!(
            r#"{{"seq":{},"ts":2000,"type":"connector_call","node_id":"act","connector":"crm","function":"find_contact","account":"main","url":"","outcome":"rate_limited","http_status":429,"duration_ms":3}}"#,
            100 + i
        ));
    }
    let renumbered = end.replace("\"seq\":10", "\"seq\":999");
    assert_ne!(renumbered, end, "the fixture's run_finished seq changed");
    lines.push(renumbered);
    lines.join("\n") + "\n"
}

#[test]
fn a_view_that_does_not_fit_is_never_an_absence() {
    let _l = common::env_lock();
    let f = Fixture::new();
    let _env = f.env();
    // The outputs fit, the connector calls do not: the model says `good`
    // with no supporting record, which only an absence over every view it
    // reads could carry.
    let server = StubServer::start(vec![reply("good", 0.9, "none")]);
    config(
        f.cfg.path(),
        &server.base_url,
        "enforce",
        "privacy: { max_state_bytes: 1024 }\n",
    );
    let dir = f.run("r1", &noisy_journal(12), "outputs, connector_calls");
    let c = &graded(grade_model(&dir).unwrap()).criteria[0];
    assert_eq!(c.level, Level::NotEnoughData);
    assert_eq!(c.reason_code, "no_support");
    assert!(c.absence_basis.is_empty());
    // The model is offered only the records of the views it was sent.
    let asked = &server.requests()[0];
    assert!(asked.contains("o:act"), "{asked}");
    assert!(!asked.contains("\"j:100\""), "an unsent record was offered");

    // Nothing fits at all: no request is made.
    let dir = f.run("r2", &noisy_journal(12), "connector_calls");
    let c = &graded(grade_model(&dir).unwrap()).criteria[0];
    assert_eq!(c.reason_code, "nothing_fits");
    assert_eq!(server.requests().len(), 1);
}

#[test]
fn the_daily_cap_skips_with_budget_and_stores_nothing() {
    let _l = common::env_lock();
    let f = Fixture::new();
    let _env = f.env();
    let server = StubServer::start(vec![
        reply("good", 0.9, "o:act"),
        reply("good", 0.9, "o:act"),
    ]);
    config(f.cfg.path(), &server.base_url, "enforce", "");
    let first = f.run("r1", CRM_JOURNAL, "outputs, connector_calls");
    graded(grade_model(&first).unwrap());
    let second = f.run("r2", CRM_JOURNAL, "outputs, connector_calls");
    assert_eq!(
        grade_model(&second).unwrap(),
        ModelGraded::Skipped {
            reason: "budget".into()
        }
    );
    assert!(grades_of(&second).is_empty());
    assert_eq!(server.requests().len(), 1);

    // With the use off, nothing is asked at all.
    config(f.cfg.path(), &server.base_url, "off", "");
    let third = f.run("r3", CRM_JOURNAL, "outputs, connector_calls");
    assert_eq!(
        grade_model(&third).unwrap(),
        ModelGraded::Skipped {
            reason: "off".into()
        }
    );
    assert_eq!(server.requests().len(), 1);
}

#[test]
fn a_grade_that_may_not_send_outputs_is_off_and_asks_nothing() {
    let _l = common::env_lock();
    let f = Fixture::new();
    let _env = f.env();
    let server = StubServer::start(vec![reply("good", 0.9, "o:act")]);
    config(
        f.cfg.path(),
        &server.base_url,
        "enforce",
        "privacy: { send: [prompts] }\n",
    );
    let dir = f.run("r1", CRM_JOURNAL, "outputs, connector_calls");
    assert_eq!(
        grade_model(&dir).unwrap(),
        ModelGraded::Skipped {
            reason: "off".into()
        }
    );
    assert!(server.requests().is_empty());
    assert!(grades_of(&dir).is_empty());
}

#[test]
fn a_stop_part_way_keeps_the_criteria_already_answered() {
    let _l = common::env_lock();
    let f = Fixture::new();
    let _env = f.env();
    let server = StubServer::start(vec![reply("good", 0.9, "o:act")]);
    // A daily cap of one request, and two criteria of the playbook's own.
    config(f.cfg.path(), &server.base_url, "enforce", "");
    let two = snapshot("outputs, connector_calls").replace(
        "nodes:\n",
        "    - id: no-deletes\n      evidence: both\n      reads: [outputs]\n      levels: { good: \"no contact was deleted\", partial: \"a contact was archived\", poor: \"a contact was deleted\" }\nnodes:\n",
    );
    assert_ne!(two, snapshot("outputs, connector_calls"));
    let dir = run_dir(f.root.path(), "r1", CRM_JOURNAL, &two);
    let g = graded(grade_model(&dir).unwrap());
    assert_eq!(g.criteria.len(), 2, "{:?}", g.criteria);
    let answered = g.criteria.iter().filter(|c| c.level == Level::Good).count();
    let stopped: Vec<_> = g
        .criteria
        .iter()
        .filter(|c| c.reason_code == "budget")
        .collect();
    assert_eq!((answered, stopped.len()), (1, 1), "{:?}", g.criteria);
    assert_eq!(stopped[0].level, Level::NotEnoughData);
    // Stopped part way: nothing of it counts for a promotion yet.
    assert!(
        g.criteria.iter().all(|c| !c.counts_for_promotion),
        "{:?}",
        g.criteria
    );
    assert_eq!(server.requests().len(), 1);
    assert_eq!(grades_of(&dir).len(), 1);
}
