//! Decision-model engine uses (issue #165 Parts 8 advise, 9, 10, 11, 12 and
//! the Part 14 enforce paths) against a local stub provider. Every enforce
//! path is checked with a stored threshold (it acts), without one (advise
//! with `enforce_refused: no_threshold`) and where it applies, capped,
//! uncalibrated, switched off mid-run and with the provider down.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use apb_core::registry::init_project;
use apb_decide::testing::{StubResponse, StubServer};
use apb_engine::event::{Event, EventPayload, read_all};
use apb_engine::scheduler::{RunMode, RunOptions, RunResult, resume, run};
use apb_engine::state::RunStatus;
use serde_json::{Value, json};

use crate::common;

struct Env(Vec<(&'static str, Option<std::ffi::OsString>)>);

impl Env {
    fn set(vars: &[(&'static str, &str)]) -> Self {
        let mut saved = Vec::new();
        for (k, v) in vars {
            saved.push((*k, std::env::var_os(k)));
            unsafe { std::env::set_var(k, v) };
        }
        Env(saved)
    }
}

impl Drop for Env {
    fn drop(&mut self) {
        for (k, v) in self.0.drain(..).rev() {
            match v {
                Some(v) => unsafe { std::env::set_var(k, v) },
                None => unsafe { std::env::remove_var(k) },
            }
        }
    }
}

struct Project {
    root: tempfile::TempDir,
    cfg: tempfile::TempDir,
}

impl Project {
    /// A project with playbook `d` and a stub agent running `agent_body`.
    fn new(playbook_yaml: &str, agent_body: &str) -> Self {
        let root = tempfile::tempdir().unwrap();
        let cfg = tempfile::tempdir().unwrap();
        init_project(root.path()).unwrap();
        let dir = root.path().join(".apb/playbooks/d/1.0.0");
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("playbook.yaml"), playbook_yaml).unwrap();
        fs::write(root.path().join(".apb/playbooks/d/current"), "1.0.0").unwrap();
        common::seed_main(root.path());
        let p = Project { root, cfg };
        p.agent_body(agent_body);
        p
    }

    fn path(&self, name: &str) -> PathBuf {
        self.root.path().join(name)
    }

    fn agent_body(&self, body: &str) {
        let path = self.path("agent.sh");
        common::write_sync(&path, &format!("#!/bin/sh\n{body}\n"));
        let mut perm = fs::metadata(&path).unwrap().permissions();
        perm.set_mode(0o755);
        fs::set_permissions(&path, perm).unwrap();
    }

    fn decisions(&self, body: &str) {
        fs::write(self.cfg.path().join("decisions.yaml"), body).unwrap();
    }

    /// Stores a threshold for `(use, stub, jev-1.13.0)`.
    fn threshold(&self, uses: &[(&str, f64)]) {
        let mut body = String::from("version: 1\nthresholds:\n");
        for (u, t) in uses {
            body.push_str(&format!(
                "  - {{ use: {u}, provider: stub, model: jev-1.13.0, threshold: {t} }}\n"
            ));
        }
        fs::write(self.cfg.path().join("decisions-thresholds.yaml"), body).unwrap();
    }

    fn env(&self) -> Env {
        let cfg = self.cfg.path().to_string_lossy().to_string();
        let agent = self.path("agent.sh").to_string_lossy().to_string();
        Env::set(&[("APB_CONFIG_DIR", &cfg), ("APB_AGENT_CMD", &agent)])
    }

    fn run(&self) -> (RunStatus, String, Vec<Event>) {
        let res = run(self.root.path(), "d", None, RunOptions::default()).unwrap();
        let events = read_all(&self.run_dir(&res.run_id)).unwrap();
        (res.outcome, res.run_id, events)
    }

    fn run_dir(&self, run_id: &str) -> PathBuf {
        self.root.path().join(".apb/runs").join(run_id)
    }
}

fn playbook(extra_top: &str, nodes: &str, edges: &str) -> String {
    format!(
        "schema: 2\nid: d\nname: Decisions\nversion: 1.0.0\n{extra_top}defaults:\n  profile: main\nnodes:\n  - {{ id: start, type: start }}\n{nodes}  - {{ id: ok, type: finish, outcome: success }}\nedges:\n  - {{ from: start, to: w }}\n{edges}"
    )
}

fn one_node(extra: &str) -> String {
    playbook(
        "",
        &format!("  - {{ id: w, type: agent_task, prompt: \"Implement the parser\"{extra} }}\n"),
        "  - { from: w, to: ok }\n",
    )
}

/// `uses:` lines under a ceiling.
fn config(base_url: &str, ceiling: &str, uses: &str) -> String {
    format!(
        "version: 1\nmode: {ceiling}\nproviders:\n  - {{ id: stub, kind: systemone, base_url: \"{base_url}\", model: jev-1.13.0 }}\nuses:\n{uses}"
    )
}

fn choice(value: &str, options: &[&str], p: f64, confidence: f64) -> Value {
    let rest = (1.0 - p) / (options.len() as f64 - 1.0).max(1.0);
    let probs: serde_json::Map<String, Value> = options
        .iter()
        .map(|o| (o.to_string(), json!(if *o == value { p } else { rest })))
        .collect();
    json!({"type": "choice", "choice": value, "probabilities": probs, "confidence": confidence})
}

fn noul(p: f64) -> Value {
    json!({"type": "noul", "noul": p})
}

fn reply(answers: Value) -> StubResponse {
    StubResponse::json(
        200,
        json!({"model": "jev-1.13.0", "answers": answers, "usage": {"input_tokens": 500, "output_tokens": 2}})
            .to_string(),
    )
}

const COMPLETION: [&str; 5] = [
    "complete",
    "partial",
    "not_started",
    "blocked_on_input",
    "unclear",
];

fn completion(final_result: f64, value: &str) -> StubResponse {
    reply(json!({
        "final_result": noul(final_result),
        "completion": choice(value, &COMPLETION, 0.8, 0.75),
    }))
}

#[derive(Debug)]
struct Decision {
    use_site: String,
    node: Option<String>,
    mode: String,
    applied: bool,
    would_change: Option<bool>,
    enforce_refused: Option<String>,
    error: Option<String>,
    join: std::collections::BTreeMap<String, Value>,
    answers: std::collections::BTreeMap<String, apb_engine::event::DecisionAnswer>,
}

fn decisions(events: &[Event], use_site: &str) -> Vec<Decision> {
    events
        .iter()
        .filter_map(|e| match &e.payload {
            EventPayload::DecisionMade {
                use_site: u,
                node,
                mode,
                applied,
                would_change,
                enforce_refused,
                error,
                join,
                answers,
                ..
            } if u == use_site => Some(Decision {
                use_site: u.clone(),
                node: node.clone(),
                mode: mode.clone(),
                applied: *applied,
                would_change: *would_change,
                enforce_refused: enforce_refused.clone(),
                error: error.clone(),
                join: join.clone(),
                answers: answers.clone(),
            }),
            _ => None,
        })
        .collect()
}

fn anomalies(events: &[Event]) -> Vec<String> {
    events
        .iter()
        .filter_map(|e| match &e.payload {
            EventPayload::WakeRaised { detail, .. } => Some(detail.clone()),
            _ => None,
        })
        .collect()
}

fn attempts(events: &[Event], node: &str) -> Vec<(String, Option<String>, Option<String>)> {
    events
        .iter()
        .filter_map(|e| match &e.payload {
            EventPayload::AttemptFinished {
                node: n,
                status,
                rejected_output,
                ..
            } if n == node => Some((status.clone(), rejected_output.clone(), None)),
            _ => None,
        })
        .collect()
}

fn position(events: &[Event], pred: impl Fn(&EventPayload) -> bool) -> usize {
    events.iter().position(|e| pred(&e.payload)).unwrap()
}

const RUNNING: &str = "Implemented the parser and pushed the branch. The CI suite is still running, I will report back when it finishes.";

// --- Part 8 advise --------------------------------------------------------

#[test]
fn completion_advise_raises_one_anomaly_per_flagged_attempt_and_never_changes_status() {
    for (reply_of, flagged) in [
        (completion(0.06, "partial"), true),
        (completion(0.93, "complete"), false),
        (StubResponse::json(503, "{}"), false),
    ] {
        let p = Project::new(&one_node(""), &format!("echo '{RUNNING}'"));
        let server = StubServer::start_with_fallback(vec![], reply_of);
        p.decisions(&config(
            &server.base_url,
            "advise",
            "  completion_check: { mode: advise }\n",
        ));
        let _lock = common::env_lock();
        let _env = p.env();
        let (status, _, events) = p.run();
        assert_eq!(status, RunStatus::Succeeded);
        let wakes = anomalies(&events);
        if flagged {
            assert_eq!(wakes.len(), 1, "{wakes:?}");
            assert_eq!(
                wakes[0],
                "agent_task node `w` attempt 1 reported success, but the completion check rates it partial (p=0.80) and final_result p=0.06"
            );
            let d = &decisions(&events, "completion_check")[0];
            assert_eq!((d.mode.as_str(), d.applied), ("advise", false));
            // Journal first: the decision precedes its anomaly.
            assert!(
                position(&events, |e| matches!(e, EventPayload::DecisionMade { .. }))
                    < position(&events, |e| matches!(e, EventPayload::WakeRaised { .. }))
            );
        } else {
            assert!(wakes.is_empty(), "{wakes:?}");
        }
        assert_eq!(attempts(&events, "w").len(), 1);
    }
}

// --- Part 14.1 completion enforce -----------------------------------------

#[test]
fn completion_enforce_with_a_stored_threshold_fails_the_attempt_and_consumes_one_retry() {
    let p = Project::new(
        &one_node(", completion_check: enforce, max_retries: 1"),
        &format!(
            "if [ -f \"$0.marker\" ]; then echo 'Implemented the parser; the suite passed.'; else touch \"$0.marker\"; echo '{RUNNING}'; fi"
        ),
    );
    let server = StubServer::start(vec![
        completion(0.02, "partial"),
        completion(0.95, "complete"),
    ]);
    p.decisions(&config(
        &server.base_url,
        "enforce",
        "  completion_check: { mode: enforce }\n",
    ));
    p.threshold(&[("completion_check", 0.05)]);
    let _lock = common::env_lock();
    let _env = p.env();
    let (status, _, events) = p.run();
    assert_eq!(status, RunStatus::Succeeded);
    let a = attempts(&events, "w");
    assert_eq!(a.len(), 2, "one retry consumed: {a:?}");
    assert_eq!(a[0].0, "failed");
    assert_eq!(a[0].1.as_deref(), Some(RUNNING), "rejected_output kept");
    assert_eq!(a[1].0, "succeeded");
    let d = decisions(&events, "completion_check");
    assert!(d[0].applied && d[0].enforce_refused.is_none());
    assert!(!d[1].applied);
    // `applied: true` is journaled before the attempt is failed.
    assert!(
        position(&events, |e| matches!(
            e,
            EventPayload::DecisionMade { applied: true, .. }
        )) < position(&events, |e| matches!(
            e,
            EventPayload::AttemptFinished { .. }
        ))
    );
    assert!(anomalies(&events).is_empty());
}

/// The enforce gate reads the store `apb decisions thresholds set` writes
/// (`apb_core::decision_thresholds`, with its `version`): a threshold set
/// through that module lets the path act, and a store of an unknown version
/// reads as no threshold.
#[test]
fn the_enforce_gate_reads_the_threshold_store_the_cli_writes() {
    for (stored, acts) in [(true, true), (false, false)] {
        let p = Project::new(
            &one_node(", completion_check: enforce, max_retries: 1"),
            &format!(
                "if [ -f \"$0.marker\" ]; then echo 'Implemented the parser; the suite passed.'; else touch \"$0.marker\"; echo '{RUNNING}'; fi"
            ),
        );
        let server = StubServer::start(vec![
            completion(0.02, "partial"),
            completion(0.95, "complete"),
        ]);
        p.decisions(&config(
            &server.base_url,
            "enforce",
            "  completion_check: { mode: enforce }\n",
        ));
        if stored {
            apb_core::decision_thresholds::set_threshold_in(
                p.cfg.path(),
                "completion_check",
                "stub",
                "jev-1.13.0",
                0.05,
            )
            .unwrap();
        } else {
            fs::write(
                p.cfg.path().join("decisions-thresholds.yaml"),
                "version: 2\nthresholds:\n  - { use: completion_check, provider: stub, model: jev-1.13.0, threshold: 0.05 }\n",
            )
            .unwrap();
        }
        let _lock = common::env_lock();
        let _env = p.env();
        let (_, _, events) = p.run();
        let d = decisions(&events, "completion_check");
        assert_eq!(d[0].applied, acts, "stored={stored}");
        let refused = d[0].enforce_refused.as_deref();
        assert_eq!(refused, if acts { None } else { Some("no_threshold") });
    }
}

#[test]
fn completion_enforce_refusals_act_as_advise() {
    // No stored threshold, a blocked_on_input answer, and the action cap.
    for (case, threshold, reply_of, max_actions) in [
        ("no_threshold", false, completion(0.02, "partial"), None),
        ("blocked", true, completion(0.02, "blocked_on_input"), None),
        ("cap", true, completion(0.02, "partial"), Some(1)),
    ] {
        let p = Project::new(
            &one_node(", completion_check: enforce, max_retries: 3"),
            &format!("echo '{RUNNING}'"),
        );
        let server = StubServer::start_with_fallback(vec![], reply_of);
        let uses = match max_actions {
            Some(n) => format!("  completion_check: {{ mode: enforce, max_actions: {n} }}\n"),
            None => "  completion_check: { mode: enforce }\n".to_string(),
        };
        p.decisions(&config(&server.base_url, "enforce", &uses));
        if threshold {
            p.threshold(&[("completion_check", 0.05)]);
        }
        let _lock = common::env_lock();
        let _env = p.env();
        let (status, _, events) = p.run();
        assert_eq!(status, RunStatus::Succeeded, "{case}");
        let d = decisions(&events, "completion_check");
        match case {
            "no_threshold" => {
                assert_eq!(d.len(), 1);
                assert_eq!(d[0].enforce_refused.as_deref(), Some("no_threshold"));
                assert!(!d[0].applied);
                assert_eq!(anomalies(&events).len(), 1, "advise instead");
            }
            "blocked" => {
                assert_eq!(d.len(), 1);
                assert!(!d[0].applied && d[0].enforce_refused.is_none());
                assert_eq!(anomalies(&events).len(), 1, "blocked_on_input only raises");
            }
            _ => {
                assert_eq!(d.len(), 2, "{d:?}");
                assert!(d[0].applied);
                assert_eq!(d[1].enforce_refused.as_deref(), Some("cap"));
                assert_eq!(attempts(&events, "w").len(), 2);
                assert_eq!(anomalies(&events).len(), 1);
            }
        }
    }
}

#[test]
fn an_uncalibrated_provider_is_refused_unless_allowed() {
    for allow in [false, true] {
        let p = Project::new(
            &one_node(", completion_check: enforce, max_retries: 1"),
            &format!("echo '{RUNNING}'"),
        );
        p.decisions(&format!(
            "version: 1\nmode: enforce\nproviders:\n  - {{ id: stub, kind: fake, answers: {{ final_result: {{ type: noul, noul: 0.01 }} }} }}\nuses:\n  completion_check: {{ mode: enforce, allow_uncalibrated: {allow} }}\n"
        ));
        fs::write(
            p.cfg.path().join("decisions-thresholds.yaml"),
            "thresholds:\n  - { use: completion_check, provider: stub, model: fake-1, threshold: 0.05 }\n",
        )
        .unwrap();
        let _lock = common::env_lock();
        let _env = p.env();
        let _fake = Env::set(&[("APB_DECISIONS_ALLOW_FAKE", "1")]);
        let (_, _, events) = p.run();
        let d = decisions(&events, "completion_check");
        if allow {
            assert!(d[0].applied, "{d:?}");
        } else {
            assert_eq!(d[0].enforce_refused.as_deref(), Some("uncalibrated"));
            assert_eq!(attempts(&events, "w").len(), 1);
        }
    }
}

#[test]
fn a_lowered_ceiling_mid_run_stops_the_enforce_path() {
    // The first node's agent lowers the machine's ceiling to shadow while
    // the run is going: the second node's decision is shadow and acts not.
    let yaml = playbook(
        "",
        "  - { id: w, type: agent_task, prompt: \"First\", completion_check: enforce, max_retries: 1 }\n  - { id: w2, type: agent_task, prompt: \"Second\", completion_check: enforce, max_retries: 1 }\n",
        "  - { from: w, to: w2 }\n  - { from: w2, to: ok }\n",
    );
    let p = Project::new(&yaml, "");
    let cfg_file = p.cfg.path().join("decisions.yaml");
    let marker = p.path("lowered");
    p.agent_body(&format!(
        "if [ ! -f '{m}' ]; then touch '{m}'; sed -i 's/^mode: enforce/mode: shadow/' '{c}'; echo 'Done: first step finished and verified end to end.'; else echo '{RUNNING}'; fi",
        m = marker.display(),
        c = cfg_file.display()
    ));
    let server = StubServer::start_with_fallback(vec![], completion(0.02, "partial"));
    p.decisions(&config(
        &server.base_url,
        "enforce",
        "  completion_check: { mode: enforce }\n",
    ));
    p.threshold(&[("completion_check", 0.05)]);
    let _lock = common::env_lock();
    let _env = p.env();
    let (status, _, events) = p.run();
    assert_eq!(status, RunStatus::Succeeded);
    let d = decisions(&events, "completion_check");
    let w2: Vec<_> = d
        .iter()
        .filter(|d| d.node.as_deref() == Some("w2"))
        .collect();
    assert_eq!(w2.len(), 1);
    assert_eq!(w2[0].mode, "shadow");
    assert!(!w2[0].applied);
    assert_eq!(attempts(&events, "w2").len(), 1);
    // And the kill switch: nothing is asked at all.
    let _off = Env::set(&[("APB_DECISIONS", "off")]);
    let before = server.count();
    let (_, _, events) = p.run();
    assert!(decisions(&events, "completion_check").is_empty());
    assert_eq!(server.count(), before);
}

// --- Part 9 retry advice ---------------------------------------------------

fn next_reply(value: &str, confidence: f64, repeat: f64) -> StubResponse {
    reply(json!({
        "next": choice(
            value,
            &["retry_same_likely_helps", "switch_executor", "stop_and_route_failure", "unclear"],
            0.85,
            confidence,
        ),
        "repeat": noul(repeat),
    }))
}

const FAILING: &str = "echo 'the linker rejected the object file' 1>&2; exit 1";

#[test]
fn retry_advice_shadow_records_would_change_and_still_retries() {
    let p = Project::new(&one_node(", max_retries: 2"), FAILING);
    let server = StubServer::start_with_fallback(vec![], next_reply("switch_executor", 0.8, 0.9));
    p.decisions(&config(
        &server.base_url,
        "shadow",
        "  retry_advice: { mode: shadow }\n",
    ));
    let _lock = common::env_lock();
    let _env = p.env();
    let (status, _, events) = p.run();
    assert_eq!(status, RunStatus::Failed);
    assert_eq!(attempts(&events, "w").len(), 3, "every retry still runs");
    let d = decisions(&events, "retry_advice");
    assert_eq!(d.len(), 2, "asked only while a retry is pending");
    assert!(d.iter().all(|d| d.would_change == Some(true) && !d.applied));
    assert_eq!(d[0].join["retries_left"], json!(2));
    assert_eq!(d[1].join["retries_left"], json!(1));
    // `previous_failure` (and the repeat question) only from the second.
    assert!(!d[0].answers.contains_key("repeat"));
    assert!(d[1].answers.contains_key("repeat"));
    let second = &server.requests()[1];
    assert!(second.contains("\"previous_failure\":"), "{second}");
    assert!(!server.requests()[0].contains("\"previous_failure\":"));
}

#[test]
fn retry_advice_is_not_asked_for_other_failures_or_without_retries() {
    for (body, extra) in [
        // Auth failures are never advised.
        (
            "echo 'Invalid API key. Please run /login' 1>&2; exit 1",
            ", max_retries: 2",
        ),
        // No retry pending.
        (FAILING, ""),
    ] {
        let p = Project::new(&one_node(extra), body);
        let server =
            StubServer::start_with_fallback(vec![], next_reply("switch_executor", 0.8, 0.1));
        p.decisions(&config(
            &server.base_url,
            "shadow",
            "  retry_advice: { mode: shadow }\n",
        ));
        let _lock = common::env_lock();
        let _env = p.env();
        let (_, _, events) = p.run();
        assert!(decisions(&events, "retry_advice").is_empty(), "{body}");
        assert_eq!(server.count(), 0);
    }
}

fn with_fallback(root: &Path) {
    common::seed_profile(
        root,
        "main",
        "claude-code",
        "haiku",
        &[("claude-code", "sonnet")],
    );
}

#[test]
fn retry_advice_enforce_skips_or_stops_and_journals_the_marker() {
    for (value, threshold) in [
        ("switch_executor", true),
        ("stop_and_route_failure", true),
        ("switch_executor", false),
    ] {
        let yaml = playbook(
            "",
            "  - { id: w, type: agent_task, prompt: \"Implement the parser\", max_retries: 2 }\n",
            "  - { from: w, to: ok }\n",
        )
        .replace(
            "  profile: main\n",
            "  profile: main\n  retry_advice: enforce\n",
        );
        let p = Project::new(&yaml, FAILING);
        with_fallback(p.root.path());
        let server = StubServer::start_with_fallback(vec![], next_reply(value, 0.9, 0.1));
        p.decisions(&config(
            &server.base_url,
            "enforce",
            "  retry_advice: { mode: enforce }\n",
        ));
        if threshold {
            p.threshold(&[("retry_advice", 0.7)]);
        }
        let _lock = common::env_lock();
        let _env = p.env();
        let (status, _, events) = p.run();
        assert_eq!(status, RunStatus::Failed);
        let markers: Vec<_> = events
            .iter()
            .filter(|e| matches!(&e.payload, EventPayload::SupervisorAction { action, .. } if action == "retry_advice"))
            .collect();
        let fallbacks = events
            .iter()
            .filter(|e| matches!(e.payload, EventPayload::FallbackTriggered { .. }))
            .count();
        let n = attempts(&events, "w").len();
        match (value, threshold) {
            ("switch_executor", true) => {
                // The first executor's retries are skipped; the last one has
                // no fallback left, so it retries as today.
                assert_eq!(n, 4, "{value}");
                assert_eq!(markers.len(), 1);
                assert_eq!(fallbacks, 1);
            }
            ("stop_and_route_failure", true) => {
                assert_eq!(n, 1);
                assert_eq!(markers.len(), 1);
                assert_eq!(fallbacks, 0);
            }
            _ => {
                assert_eq!(n, 6, "no threshold: today's retries");
                assert!(markers.is_empty());
                let d = decisions(&events, "retry_advice");
                assert!(
                    d.iter()
                        .all(|d| d.enforce_refused.as_deref() == Some("no_threshold"))
                );
            }
        }
        // Journal first: the applied decision precedes its marker.
        if let Some(m) = markers.first() {
            let applied = events
                .iter()
                .position(|e| matches!(e.payload, EventPayload::DecisionMade { applied: true, .. }))
                .unwrap();
            assert!(applied < events.iter().position(|e| e.seq == m.seq).unwrap());
        }
    }
}

// --- Part 11 review recommendation and Part 14.4 auto-decide ---------------

fn gate_playbook(effects: &str, gate_extra: &str, after: &str) -> String {
    format!(
        "schema: 2\nid: d\nname: Decisions\nversion: 1.0.0\n{effects}defaults:\n  profile: main\nnodes:\n  - {{ id: start, type: start }}\n  - {{ id: w, type: agent_task, prompt: \"Review the change\" }}\n  - {{ id: g, type: human_review, title: Check the review, prompt: \"Approve when the review is clean.\", options: [approve, needs_changes]{gate_extra} }}\n  - {{ id: {after}, type: agent_task, prompt: \"Apply fixes\" }}\n  - {{ id: ok, type: finish, outcome: success }}\nedges:\n  - {{ from: start, to: w }}\n  - {{ from: w, to: g }}\n  - {{ from: g, to: {after}, when: {{ type: review_status, equals: needs_changes }} }}\n  - {{ from: g, to: ok, when: {{ type: review_status, equals: approve }} }}\n  - {{ from: {after}, to: ok }}\n"
    )
}

fn decision_reply(value: &str, confidence: f64) -> StubResponse {
    reply(json!({"decision": choice(value, &["approve", "needs_changes"], 0.9, confidence)}))
}

fn run_in_background(root: PathBuf, opts: RunOptions) -> mpsc::Receiver<RunResult> {
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        if let Ok(res) = run(&root, "d", None, opts) {
            let _ = tx.send(res);
        }
    });
    rx
}

fn poll<T>(what: &str, mut f: impl FnMut() -> Option<T>) -> T {
    let start = Instant::now();
    loop {
        if let Some(v) = f() {
            return v;
        }
        assert!(
            start.elapsed() < Duration::from_secs(20),
            "timed out: {what}"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn find_run_dir(root: &Path) -> PathBuf {
    poll("run dir", || {
        fs::read_dir(root.join(".apb/runs"))
            .ok()?
            .filter_map(|e| e.ok())
            .find(|e| e.file_name().to_string_lossy().starts_with("d-"))
            .map(|e| e.path())
    })
}

fn review_requested(
    run_dir: &Path,
) -> Option<(String, Option<apb_engine::event::ReviewRecommendation>)> {
    read_all(run_dir)
        .ok()?
        .into_iter()
        .find_map(|e| match e.payload {
            EventPayload::ReviewRequested {
                instruction,
                recommendation,
                ..
            } => Some((instruction, recommendation)),
            _ => None,
        })
}

#[test]
fn review_advise_recommends_without_deciding() {
    let p = Project::new(
        &gate_playbook(
            "",
            ", option_descriptions: { approve: \"The review found nothing to fix.\", needs_changes: \"The review lists fixes.\" }",
            "fix",
        ),
        "echo 'Review: one blocking defect in the parser.'",
    );
    let server = StubServer::start_with_fallback(vec![], decision_reply("needs_changes", 0.8));
    p.decisions(&config(
        &server.base_url,
        "advise",
        "  review_triage: { mode: advise }\n",
    ));
    let _lock = common::env_lock();
    let _env = p.env();
    let rx = run_in_background(p.root.path().to_path_buf(), RunOptions::default());
    let run_dir = find_run_dir(p.root.path());
    let (instruction, rec) = poll("review_requested", || review_requested(&run_dir));
    let rec = rec.expect("a recommendation");
    assert_eq!((rec.option.as_str(), rec.applied), ("needs_changes", false));
    assert!(instruction.ends_with(" Advisory recommendation: needs_changes (p=0.90)."));
    // The state carries the gate and the predecessor's output, the criteria
    // the declared descriptions.
    let raw = &server.requests()[0];
    assert!(raw.contains("one blocking defect") && raw.contains("The review lists fixes."));
    // Nothing is decided.
    std::thread::sleep(Duration::from_millis(200));
    assert!(
        !read_all(&run_dir)
            .unwrap()
            .iter()
            .any(|e| matches!(e.payload, EventPayload::ReviewDecided { .. }))
    );
    let d = decisions(&read_all(&run_dir).unwrap(), "review_triage");
    assert_eq!(d[0].join["gate_visit"], json!(1));
    apb_engine::review::post_review(
        &run_dir,
        apb_engine::review::ReviewCommand {
            node: "g".into(),
            decision: "approve".into(),
            note: String::new(),
        },
    )
    .unwrap();
    let res = rx.recv_timeout(Duration::from_secs(20)).unwrap();
    assert_eq!(res.outcome, RunStatus::Succeeded);
}

#[test]
fn review_auto_decide_posts_needs_changes_with_an_auto_note() {
    let p = Project::new(
        &gate_playbook(
            "",
            ", auto_decide: { allow: [needs_changes], min_confidence: 0.8 }",
            "fix",
        ),
        "echo 'Review: one blocking defect in the parser.'",
    );
    let server = StubServer::start_with_fallback(vec![], decision_reply("needs_changes", 0.9));
    p.decisions(&config(
        &server.base_url,
        "enforce",
        "  review_triage: { mode: enforce }\n",
    ));
    p.threshold(&[("review_triage", 0.5)]);
    let _lock = common::env_lock();
    let _env = p.env();
    let (status, run_id, events) = p.run();
    assert_eq!(status, RunStatus::Succeeded);
    let note = events
        .iter()
        .find_map(|e| match &e.payload {
            EventPayload::ReviewDecided { decision, note, .. } => {
                Some((decision.clone(), note.clone()))
            }
            _ => None,
        })
        .unwrap();
    assert_eq!(
        note,
        (
            "needs_changes".to_string(),
            "auto: stub/jev-1.13.0 p=0.90".to_string()
        )
    );
    assert!(decisions(&events, "review_triage")[0].applied);
    let _ = run_id;
}

#[test]
fn review_auto_decide_is_refused_for_inherited_effects_and_waits() {
    // A sub-playbook declaring `irreversible` runs after the gate.
    let yaml = gate_playbook("", ", auto_decide: { allow: [needs_changes] }", "fix").replace(
        "  - { id: fix, type: agent_task, prompt: \"Apply fixes\" }\n",
        "  - { id: fix, type: playbook, playbook: child }\n",
    );
    let p = Project::new(&yaml, "echo 'Review: one blocking defect.'");
    let child = p.root.path().join(".apb/playbooks/child/1.0.0");
    fs::create_dir_all(&child).unwrap();
    fs::write(
        child.join("playbook.yaml"),
        "schema: 2\nid: child\nname: Child\nversion: 1.0.0\neffects: [irreversible]\nnodes:\n  - { id: start, type: start }\n  - { id: done, type: finish, outcome: success }\nedges:\n  - { from: start, to: done }\n",
    )
    .unwrap();
    fs::write(p.root.path().join(".apb/playbooks/child/current"), "1.0.0").unwrap();
    let server = StubServer::start_with_fallback(vec![], decision_reply("needs_changes", 0.99));
    p.decisions(&config(
        &server.base_url,
        "enforce",
        "  review_triage: { mode: enforce }\n",
    ));
    p.threshold(&[("review_triage", 0.5)]);
    let _lock = common::env_lock();
    let _env = p.env();
    let rx = run_in_background(p.root.path().to_path_buf(), RunOptions::default());
    let run_dir = find_run_dir(p.root.path());
    let (_, rec) = poll("review_requested", || review_requested(&run_dir));
    assert!(!rec.unwrap().applied, "fail-closed: the gate waits");
    let d = decisions(&read_all(&run_dir).unwrap(), "review_triage");
    assert_eq!(d[0].enforce_refused.as_deref(), Some("effects"));
    std::thread::sleep(Duration::from_millis(200));
    assert!(
        !read_all(&run_dir)
            .unwrap()
            .iter()
            .any(|e| matches!(e.payload, EventPayload::ReviewDecided { .. }))
    );
    apb_engine::stop::stop_run(
        p.root.path(),
        run_dir.file_name().unwrap().to_str().unwrap(),
    )
    .ok();
    let _ = rx.recv_timeout(Duration::from_secs(20));
}

#[test]
fn an_earlier_human_decision_wins_over_auto_decide() {
    let p = Project::new(
        &gate_playbook("", ", auto_decide: { allow: [needs_changes] }", "fix"),
        "echo 'Review: one blocking defect.'",
    );
    let server = StubServer::start_with_fallback(vec![], decision_reply("needs_changes", 0.99));
    p.decisions(&config(
        &server.base_url,
        "enforce",
        "  review_triage: { mode: enforce }\n",
    ));
    p.threshold(&[("review_triage", 0.5)]);
    let _lock = common::env_lock();
    let _env = p.env();
    // A provider slow enough that the person's decision lands first.
    let rx = run_in_background(p.root.path().to_path_buf(), RunOptions::default());
    let run_dir = find_run_dir(p.root.path());
    poll("review_requested", || review_requested(&run_dir));
    // The queued auto decision (if any) blocks a second one: the channel
    // accepts exactly one decision per open request.
    let second = apb_engine::review::post_review(
        &run_dir,
        apb_engine::review::ReviewCommand {
            node: "g".into(),
            decision: "approve".into(),
            note: "person".into(),
        },
    );
    let res = rx.recv_timeout(Duration::from_secs(20)).unwrap();
    assert_eq!(res.outcome, RunStatus::Succeeded);
    let decided: Vec<(String, String)> = read_all(&run_dir)
        .unwrap()
        .iter()
        .filter_map(|e| match &e.payload {
            EventPayload::ReviewDecided { decision, note, .. } => {
                Some((decision.clone(), note.clone()))
            }
            _ => None,
        })
        .collect();
    assert_eq!(decided.len(), 1, "one decision per visit: {decided:?}");
    match second {
        Ok(_) => assert_eq!(decided[0].1, "person"),
        Err(_) => assert!(decided[0].1.starts_with("auto: ")),
    }
}

// --- Part 10 supervisor pre-triage and Part 14.3 auto-retry -----------------

fn action_reply(action: &str, p: f64, looping: f64) -> StubResponse {
    reply(json!({
        "action": choice(
            action,
            &["retry_same", "retry_with_note", "continue_from_next", "pause_for_human", "needs_supervisor"],
            p,
            0.8,
        ),
        "looping": noul(looping),
    }))
}

const FLAKY: &str = "if [ -f \"$0.marker\" ]; then echo 'Implemented and verified the parser end to end.'; else touch \"$0.marker\"; echo 'error: flaky network fetch' 1>&2; exit 1; fi";

#[test]
fn supervisor_triage_advise_puts_the_recommendation_on_the_wake() {
    let p = Project::new(&one_node(""), FLAKY);
    let server = StubServer::start_with_fallback(vec![], action_reply("retry_same", 0.9, 0.1));
    p.decisions(&config(
        &server.base_url,
        "advise",
        "  supervisor_triage: { mode: advise }\n",
    ));
    let _lock = common::env_lock();
    let _env = p.env();
    let rx = run_in_background(
        p.root.path().to_path_buf(),
        RunOptions {
            mode: RunMode::Supervised,
            ..Default::default()
        },
    );
    let run_dir = find_run_dir(p.root.path());
    let (detail, triage, seq) = poll("wake", || {
        read_all(&run_dir)
            .ok()?
            .into_iter()
            .find_map(|e| match e.payload {
                EventPayload::WakeRaised { detail, triage, .. } => Some((detail, triage, e.seq)),
                _ => None,
            })
    });
    let t = triage.expect("advise adds triage");
    assert_eq!((t.action.as_str(), t.applied), ("retry_same", false));
    assert!(
        detail.ends_with(" Triage (advisory): retry_same p=0.90."),
        "{detail}"
    );
    let d = decisions(&read_all(&run_dir).unwrap(), "supervisor_triage");
    assert_eq!(d[0].join["wake_seq"], json!(seq));
    // `switch_executor` is not offered: the profile has no alternative.
    assert!(!server.requests()[0].contains("\"switch_executor\""));
    // The wait surface passes it on.
    let w = apb_engine::inspect::wait_wake(
        p.root.path(),
        run_dir.file_name().unwrap().to_str().unwrap(),
        None,
        Duration::from_secs(1),
    )
    .unwrap()
    .unwrap();
    assert!(w.triage.is_some());
    apb_engine::control::post_control(
        &run_dir,
        apb_engine::control::Control::Retry {
            node: "w".into(),
            prompt_override: None,
        },
    )
    .unwrap();
    let res = rx.recv_timeout(Duration::from_secs(20)).unwrap();
    assert_eq!(res.outcome, RunStatus::Succeeded);
}

#[test]
fn supervisor_auto_retry_is_a_capped_node_retry_attributed_to_triage() {
    let yaml = one_node("").replace(
        "defaults:\n",
        "supervisor:\n  pre_triage: enforce\ndefaults:\n",
    );
    let p = Project::new(&yaml, FLAKY);
    let server = StubServer::start_with_fallback(vec![], action_reply("retry_with_note", 0.9, 0.1));
    p.decisions(&config(
        &server.base_url,
        "enforce",
        "  supervisor_triage: { mode: enforce }\n",
    ));
    p.threshold(&[("supervisor_triage", 0.5)]);
    let _lock = common::env_lock();
    let _env = p.env();
    let rx = run_in_background(
        p.root.path().to_path_buf(),
        RunOptions {
            mode: RunMode::Supervised,
            ..Default::default()
        },
    );
    let res = rx.recv_timeout(Duration::from_secs(30)).unwrap();
    assert_eq!(res.outcome, RunStatus::Succeeded, "no supervisor needed");
    let events = read_all(&p.run_dir(&res.run_id)).unwrap();
    let wake = events
        .iter()
        .find_map(|e| match &e.payload {
            EventPayload::WakeRaised { triage, .. } => triage.clone(),
            _ => None,
        })
        .unwrap();
    assert!(wake.applied);
    let actions: Vec<String> = events
        .iter()
        .filter_map(|e| match &e.payload {
            EventPayload::SupervisorAction { action, .. } => Some(action.clone()),
            _ => None,
        })
        .collect();
    assert!(
        actions
            .windows(3)
            .any(|w| w == ["triage_retry", "context_append", "node_retry"]),
        "{actions:?}"
    );
    // The code-template note reached the context.
    let note = events.iter().find_map(|e| match &e.payload {
        EventPayload::SupervisorAction { action, detail, .. } if action == "context_append" => {
            Some(detail.clone())
        }
        _ => None,
    });
    assert_eq!(
        note.as_deref(),
        Some(
            "Previous attempt failed with: agent; agent exited with Some(1): error: flaky network fetch"
        )
    );
}

#[test]
fn supervisor_auto_retry_stops_at_its_cap_and_wakes_the_supervisor() {
    let yaml = one_node("").replace(
        "defaults:\n",
        "supervisor:\n  pre_triage: enforce\ndefaults:\n",
    );
    let p = Project::new(&yaml, "echo 'error: always broken' 1>&2; exit 1");
    let server = StubServer::start_with_fallback(vec![], action_reply("retry_same", 0.9, 0.1));
    p.decisions(&config(
        &server.base_url,
        "enforce",
        "  supervisor_triage: { mode: enforce, max_actions: 1 }\n",
    ));
    p.threshold(&[("supervisor_triage", 0.5)]);
    let _lock = common::env_lock();
    let _env = p.env();
    let rx = run_in_background(
        p.root.path().to_path_buf(),
        RunOptions {
            mode: RunMode::Supervised,
            ..Default::default()
        },
    );
    let run_dir = find_run_dir(p.root.path());
    let second = poll("the second wake", || {
        let wakes: Vec<_> = read_all(&run_dir)
            .ok()?
            .into_iter()
            .filter_map(|e| match e.payload {
                EventPayload::WakeRaised { triage, .. } => Some(triage),
                _ => None,
            })
            .collect();
        (wakes.len() >= 2).then(|| wakes[1].clone())
    });
    let t = second.expect("the recommendation is still shown");
    assert!(!t.applied, "the cap turns enforce into advise");
    let d = decisions(&read_all(&run_dir).unwrap(), "supervisor_triage");
    assert!(d[0].applied);
    assert_eq!(d[1].enforce_refused.as_deref(), Some("cap"));
    apb_engine::control::post_control(
        &run_dir,
        apb_engine::control::Control::Abort {
            reason: "test".into(),
        },
    )
    .unwrap();
    let res = rx.recv_timeout(Duration::from_secs(20)).unwrap();
    assert_eq!(res.outcome, RunStatus::Aborted);
}

// --- Part 12 routing -------------------------------------------------------

fn seed_tiers(root: &Path) {
    let dir = root.join(".apb/profiles/main");
    fs::write(
        dir.join("profile.yaml"),
        "name: main\ndescription: test\nexecutor:\n  agent: claude-code\n  model: sonnet\ntiers:\n  light: { agent: claude-code, model: haiku, for: \"Mechanical edits and small lookups.\" }\n  standard: { use: executor, for: \"Ordinary implementation and review tasks.\" }\n  heavy: { agent: claude-code, model: opus, for: \"Cross-cutting design changes.\" }\n",
    )
    .unwrap();
}

fn tier_reply(tier: &str, confidence: f64) -> StubResponse {
    reply(
        json!({"tier": choice(tier, &["light", "standard", "heavy", "unclear"], 0.85, confidence)}),
    )
}

fn attempt_models(events: &[Event]) -> Vec<String> {
    events
        .iter()
        .filter_map(|e| match &e.payload {
            EventPayload::AttemptStarted { model, .. } => model.clone(),
            _ => None,
        })
        .collect()
}

#[test]
fn routing_shadow_journals_the_tier_and_keeps_the_executor() {
    let p = Project::new(&one_node(", route: auto"), "echo 'Renamed the helper.'");
    seed_tiers(p.root.path());
    let server = StubServer::start_with_fallback(vec![], tier_reply("light", 0.9));
    p.decisions(&config(
        &server.base_url,
        "shadow",
        "  routing: { mode: shadow }\n",
    ));
    let _lock = common::env_lock();
    let _env = p.env();
    let (status, run_id, events) = p.run();
    assert_eq!(status, RunStatus::Succeeded);
    assert_eq!(attempt_models(&events), ["sonnet"]);
    let d = decisions(&events, "routing");
    assert_eq!(d[0].join["tier"], json!("light"));
    assert_eq!(d[0].join["executor_tier"], json!("standard"));
    assert_eq!((d[0].would_change, d[0].applied), (Some(true), false));
    // The manifest snapshots the tiers.
    let manifest = fs::read_to_string(p.run_dir(&run_id).join("manifest.yaml")).unwrap();
    assert!(manifest.contains("tiers:"));
}

#[test]
fn routing_enforce_runs_the_tier_and_cascades_up_on_an_agent_failure() {
    let p = Project::new(&one_node(", route: auto, max_retries: 2"), "");
    seed_tiers(p.root.path());
    // Fails on the light tier (haiku), succeeds above it.
    p.agent_body("case \"$*\" in *haiku*) echo 'error: cannot parse the grammar' 1>&2; exit 1;; *) echo 'Done and verified.';; esac");
    let server = StubServer::start_with_fallback(vec![], tier_reply("light", 0.9));
    p.decisions(&config(
        &server.base_url,
        "enforce",
        "  routing: { mode: enforce }\n",
    ));
    p.threshold(&[("routing", 0.6)]);
    let _lock = common::env_lock();
    let _env = p.env();
    let (status, _, events) = p.run();
    assert_eq!(status, RunStatus::Succeeded);
    assert_eq!(
        attempt_models(&events),
        ["haiku", "sonnet"],
        "no same-tier retry on the light tier"
    );
    assert!(events.iter().any(|e| matches!(&e.payload, EventPayload::ProfileRebound { reason, .. } if reason.starts_with("routing: tier `light`"))));
    assert!(events.iter().any(|e| matches!(&e.payload, EventPayload::FallbackTriggered { reason: Some(r), .. } if r == "routing")));
    assert!(decisions(&events, "routing")[0].applied);
}

#[test]
fn routing_never_reroutes_a_handoff_node_and_refuses_without_a_threshold() {
    let yaml = playbook(
        "",
        "  - { id: w, type: agent_task, prompt: \"First\", route: auto }\n  - { id: w2, type: agent_task, prompt: \"Second\", route: auto, continue_session: w }\n",
        "  - { from: w, to: w2 }\n  - { from: w2, to: ok }\n",
    );
    let p = Project::new(&yaml, "echo 'Done.'");
    seed_tiers(p.root.path());
    let server = StubServer::start_with_fallback(vec![], tier_reply("heavy", 0.95));
    p.decisions(&config(
        &server.base_url,
        "enforce",
        "  routing: { mode: enforce }\n",
    ));
    let _lock = common::env_lock();
    let _env = p.env();
    let (_, _, events) = p.run();
    let d = decisions(&events, "routing");
    // `w` is a handoff source and `w2` continues it: neither is asked.
    assert_eq!(d.len(), 2);
    assert!(d.iter().all(|d| d.join.contains_key("excluded")));
    assert_eq!(server.count(), 0);
    assert_eq!(attempt_models(&events), ["sonnet", "sonnet"]);
}

// --- replays ---------------------------------------------------------------

#[test]
fn resumed_runs_replay_every_use_without_a_request() {
    // Completion (enforce, applied), routing (enforce, applied) and the
    // review recommendation: resuming the node asks nothing again.
    let p = Project::new(
        &one_node(", route: auto, completion_check: enforce, max_retries: 1"),
        "",
    );
    seed_tiers(p.root.path());
    p.agent_body("echo 'Done and verified.'");
    let server = StubServer::start_with_fallback(
        vec![],
        reply(json!({
            "tier": choice("heavy", &["light", "standard", "heavy", "unclear"], 0.85, 0.9),
            "final_result": noul(0.95),
            "completion": choice("complete", &COMPLETION, 0.8, 0.75),
        })),
    );
    p.decisions(&config(
        &server.base_url,
        "enforce",
        "  routing: { mode: enforce }\n  completion_check: { mode: enforce }\n",
    ));
    p.threshold(&[("routing", 0.6), ("completion_check", 0.05)]);
    let _lock = common::env_lock();
    let _env = p.env();
    let (_, run_id, events) = p.run();
    let asked = server.count();
    assert_eq!(asked, 2);
    let rebounds = |ev: &[Event]| {
        ev.iter()
            .filter(|e| matches!(e.payload, EventPayload::ProfileRebound { .. }))
            .count()
    };
    assert_eq!(rebounds(&events), 1);
    resume(p.root.path(), &run_id, Some("w")).unwrap();
    let events = read_all(&p.run_dir(&run_id)).unwrap();
    assert_eq!(server.count(), asked, "the resume made no request");
    assert_eq!(
        rebounds(&events),
        1,
        "the replayed route writes nothing new"
    );
    assert_eq!(
        attempt_models(&events),
        ["opus", "opus"],
        "the applied path repeats"
    );
    let d: Vec<_> = decisions(&events, "routing");
    assert_eq!(d.len(), 1);
    assert_eq!(d[0].use_site, "routing");
    assert!(d[0].error.is_none());
}
