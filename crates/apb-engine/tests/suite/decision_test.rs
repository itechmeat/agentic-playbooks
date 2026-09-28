//! Decision models in a run (issue #165 Parts 2, 3 and 8): the completion
//! check in shadow mode against a local stub provider. Shadow means journal
//! only, so every test also checks that the run's status, the node output and
//! the wake list are what they would be without the check.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::time::Duration;

use apb_core::registry::init_project;
use apb_decide::testing::{StubResponse, StubServer};
use apb_engine::decision::decision_totals;
use apb_engine::event::{DecisionAnswer, Event, EventPayload, read_all};
use apb_engine::scheduler::{RunOptions, resume, run};
use apb_engine::state::RunStatus;

use crate::common;

/// Sets environment variables for one test and restores the originals on
/// drop, panics included. Declared after the shared env lock guard.
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

const SENTINEL_KEY: &str = "sk-sentinel-decision-key-7d3e91";
const SENTINEL_CONNECTOR_SECRET: &str = "connector-secret-5b1c77aa";
const SENTINEL_EMAIL: &str = "someone.private@example.org";

fn playbook(nodes: &str, edges: &str) -> String {
    format!(
        "schema: 2\nid: d\nname: Decisions\nversion: 1.0.0\ndefaults:\n  profile: main\nnodes:\n  - {{ id: start, type: start }}\n{nodes}  - {{ id: ok, type: finish, outcome: success }}\nedges:\n  - {{ from: start, to: w }}\n{edges}"
    )
}

fn one_node(extra: &str) -> String {
    playbook(
        &format!("  - {{ id: w, type: agent_task, prompt: \"Implement the parser\"{extra} }}\n"),
        "  - { from: w, to: ok }\n",
    )
}

fn two_nodes() -> String {
    playbook(
        "  - { id: w, type: agent_task, prompt: \"First step\" }\n  - { id: w2, type: agent_task, prompt: \"Second step\" }\n",
        "  - { from: w, to: w2 }\n  - { from: w2, to: ok }\n",
    )
}

struct Project {
    root: tempfile::TempDir,
    cfg: tempfile::TempDir,
}

impl Project {
    fn new(playbook_yaml: &str, agent_stdout: &str) -> Self {
        let root = tempfile::tempdir().unwrap();
        let cfg = tempfile::tempdir().unwrap();
        init_project(root.path()).unwrap();
        let dir = root.path().join(".apb/playbooks/d/1.0.0");
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("playbook.yaml"), playbook_yaml).unwrap();
        fs::write(root.path().join(".apb/playbooks/d/current"), "1.0.0").unwrap();
        fs::create_dir_all(dir.join("scripts")).unwrap();
        fs::write(dir.join("scripts/check.sh"), "exit 0\n").unwrap();
        common::seed_main(root.path());
        let p = Project { root, cfg };
        p.set_output(agent_stdout);
        p
    }

    fn set_output(&self, stdout: &str) {
        fs::write(self.root.path().join("stdout.txt"), stdout).unwrap();
    }

    /// A stub agent printing `stdout.txt` verbatim.
    fn agent(&self) -> String {
        let path = self.root.path().join("agent.sh");
        common::write_sync(
            &path,
            &format!(
                "#!/bin/sh\ncat '{}'\n",
                self.root.path().join("stdout.txt").display()
            ),
        );
        let mut perm = fs::metadata(&path).unwrap().permissions();
        perm.set_mode(0o755);
        fs::set_permissions(&path, perm).unwrap();
        path.to_string_lossy().to_string()
    }

    fn decisions(&self, body: &str) {
        fs::write(self.cfg.path().join("decisions.yaml"), body).unwrap();
    }

    fn env(&self) -> Env {
        let cfg = self.cfg.path().to_string_lossy().to_string();
        let agent = self.agent();
        Env::set(&[
            ("APB_CONFIG_DIR", &cfg),
            ("APB_AGENT_CMD", &agent),
            ("DECISION_TEST_KEY", SENTINEL_KEY),
        ])
    }

    fn run(&self) -> (RunStatus, String, Vec<Event>) {
        let res = run(self.root.path(), "d", None, RunOptions::default()).unwrap();
        let events = read_all(&self.run_dir(&res.run_id)).unwrap();
        (res.outcome, res.run_id, events)
    }

    fn run_dir(&self, run_id: &str) -> std::path::PathBuf {
        self.root.path().join(".apb/runs").join(run_id)
    }
}

fn config(base_url: &str, extra: &str) -> String {
    format!(
        "version: 1\nmode: shadow\nproviders:\n  - {{ id: stub, kind: systemone, base_url: \"{base_url}\", model: jev-1.13.0, api_key: \"{{{{env.DECISION_TEST_KEY}}}}\" }}\nuses:\n  completion_check: {{ mode: shadow }}\n{extra}"
    )
}

fn reply(final_result: f64, completion: &str) -> String {
    let mut probs = serde_json::Map::new();
    for o in [
        "complete",
        "partial",
        "not_started",
        "blocked_on_input",
        "unclear",
    ] {
        probs.insert(
            o.into(),
            serde_json::json!(if o == completion { 0.8 } else { 0.05 }),
        );
    }
    serde_json::json!({
        "model": "jev-1.13.0",
        "answers": {
            "final_result": {"type": "noul", "noul": final_result},
            "completion": {"type": "choice", "choice": completion, "probabilities": probs, "confidence": 0.75}
        },
        "usage": {"input_tokens": 2000, "output_tokens": 2}
    })
    .to_string()
}

struct Decision {
    node: Option<String>,
    attempt: Option<u32>,
    mode: String,
    answers: std::collections::BTreeMap<String, DecisionAnswer>,
    applied: bool,
    would_change: Option<bool>,
    regex_flag: Option<bool>,
    pattern: Option<String>,
    cost_usd: Option<f64>,
    cost_estimated: bool,
    error: Option<String>,
}

fn decisions(events: &[Event]) -> Vec<Decision> {
    events
        .iter()
        .filter_map(|e| match &e.payload {
            EventPayload::DecisionMade {
                node,
                attempt,
                mode,
                answers,
                applied,
                would_change,
                baseline,
                cost_usd,
                cost_estimated,
                error,
                ..
            } => Some(Decision {
                node: node.clone(),
                attempt: *attempt,
                mode: mode.clone(),
                answers: answers.clone(),
                applied: *applied,
                would_change: *would_change,
                regex_flag: baseline.as_ref().map(|b| b.regex_flag),
                pattern: baseline.as_ref().and_then(|b| b.pattern.clone()),
                cost_usd: *cost_usd,
                cost_estimated: *cost_estimated,
                error: error.clone(),
            }),
            _ => None,
        })
        .collect()
}

fn node_output(events: &[Event], node: &str) -> Option<(String, String)> {
    events.iter().rev().find_map(|e| match &e.payload {
        EventPayload::NodeFinished {
            node: n,
            status,
            output,
            ..
        } if n == node => Some((status.clone(), output.clone())),
        _ => None,
    })
}

fn anomalies(events: &[Event]) -> usize {
    events
        .iter()
        .filter(|e| matches!(e.payload, EventPayload::WakeRaised { .. }))
        .count()
}

/// The event kinds of a journal, in order: what a golden comparison of two
/// runs can hold equal (seqs, timestamps, pids and durations differ).
fn kinds(events: &[Event]) -> Vec<String> {
    events
        .iter()
        .map(|e| {
            serde_json::to_value(&e.payload).unwrap()["type"]
                .as_str()
                .unwrap()
                .to_string()
        })
        .collect()
}

const DONE: &str = "Implemented the parser in src/parser.rs, added unit tests for every rule, and the whole suite passes.";
const RUNNING: &str = "Implemented the parser and pushed the branch. The CI suite is still running, I will report back when it finishes.";

#[test]
fn without_a_decisions_file_nothing_is_asked_and_nothing_changes() {
    let p = Project::new(&one_node(""), DONE);
    let _lock = common::env_lock();
    let _env = p.env();
    let (status, run_a, events_a) = p.run();
    assert_eq!(status, RunStatus::Succeeded);
    assert!(decisions(&events_a).is_empty());
    let manifest_a = fs::read_to_string(p.run_dir(&run_a).join("manifest.yaml")).unwrap();
    assert!(!manifest_a.contains("decisions"), "{manifest_a}");

    // A file whose every use is off is the same as no file at all.
    let server = StubServer::start(vec![]);
    p.decisions(&config(&server.base_url, "").replace(
        "completion_check: { mode: shadow }",
        "completion_check: { mode: off }",
    ));
    let (_, run_b, events_b) = p.run();
    let manifest_b = fs::read_to_string(p.run_dir(&run_b).join("manifest.yaml")).unwrap();
    assert_eq!(manifest_a, manifest_b, "the manifest is byte-identical");
    assert_eq!(kinds(&events_a), kinds(&events_b));
    assert_eq!(node_output(&events_a, "w"), node_output(&events_b, "w"));
    assert_eq!(server.count(), 0);
    assert!(!p.run_dir(&run_b).join("decisions").exists());
}

#[test]
fn shadow_journals_a_complete_answer_and_changes_nothing() {
    let p = Project::new(&one_node(""), DONE);
    let server = StubServer::start(vec![StubResponse::json(200, reply(0.93, "complete"))]);
    p.decisions(&config(&server.base_url, ""));
    let _lock = common::env_lock();
    let _env = p.env();
    let (status, run_id, events) = p.run();

    assert_eq!(status, RunStatus::Succeeded);
    assert_eq!(
        node_output(&events, "w"),
        Some(("succeeded".into(), DONE.to_string()))
    );
    assert_eq!(anomalies(&events), 0);
    let d = decisions(&events);
    assert_eq!(d.len(), 1);
    let d = &d[0];
    assert_eq!(
        (d.node.as_deref(), d.attempt, d.mode.as_str()),
        (Some("w"), Some(1), "shadow")
    );
    assert_eq!(d.answers["final_result"].p, Some(0.93));
    assert_eq!(
        d.answers["completion"].value,
        Some(serde_json::json!("complete"))
    );
    assert_eq!(d.would_change, Some(false));
    assert!(!d.applied);
    assert_eq!((d.regex_flag, d.error.as_deref()), (Some(false), None));
    // 2000 input tokens at the list price, estimated.
    assert!(d.cost_estimated);
    assert!((d.cost_usd.unwrap() - 0.000084).abs() < 1e-12);

    // Journal first: the decision precedes the attempt's own finish.
    let pos = |pred: &dyn Fn(&EventPayload) -> bool| {
        events.iter().position(|e| pred(&e.payload)).unwrap()
    };
    assert!(
        pos(&|e| matches!(e, EventPayload::DecisionMade { .. }))
            < pos(&|e| matches!(e, EventPayload::AttemptFinished { .. }))
    );
    // The manifest records the key reference, never the key.
    let manifest = fs::read_to_string(p.run_dir(&run_id).join("manifest.yaml")).unwrap();
    assert!(manifest.contains("DECISION_TEST_KEY"));
    assert!(!manifest.contains(SENTINEL_KEY));

    let totals = decision_totals(&events);
    assert_eq!(
        (totals.decisions, totals.requests, totals.errors),
        (1, 1, 0)
    );
    assert_eq!(totals.by_use["completion_check"].shadow_would_change, 0);

    // The request carried the measured prompt and the named state fields.
    let raw = &server.requests()[0];
    assert!(raw.contains("Is `result` a finished result for `task`"));
    assert!(
        raw.contains(r#""state":{"task":"#),
        "task comes first: {raw}"
    );
    assert!(raw.contains(r#""meta":{"missing_fields":[]}"#));
}

#[test]
fn shadow_records_would_change_and_the_regex_verdict_for_a_progress_note() {
    let p = Project::new(&one_node(""), RUNNING);
    let server = StubServer::start(vec![StubResponse::json(200, reply(0.06, "partial"))]);
    p.decisions(&config(&server.base_url, ""));
    let _lock = common::env_lock();
    let _env = p.env();
    let (status, _, events) = p.run();

    // Flagged in the journal only: status, output and wakes are untouched.
    assert_eq!(status, RunStatus::Succeeded);
    assert_eq!(
        node_output(&events, "w"),
        Some(("succeeded".into(), RUNNING.to_string()))
    );
    assert_eq!(anomalies(&events), 0);
    let d = &decisions(&events)[0];
    assert_eq!(d.would_change, Some(true));
    assert!(!d.applied);
    assert_eq!(d.regex_flag, Some(true));
    assert_eq!(d.pattern.as_deref(), Some("still running"));
    assert_eq!(
        decision_totals(&events).by_use["completion_check"].shadow_would_change,
        1
    );
}

#[test]
fn a_provider_that_is_down_fails_open() {
    for (script, error) in [
        (vec![StubResponse::json(401, "{}")], "auth"),
        (
            vec![
                StubResponse::json(503, "{}"),
                StubResponse::json(503, "{}"),
                StubResponse::json(503, "{}"),
            ],
            "unavailable",
        ),
        (vec![StubResponse::json(200, "not json")], "unavailable"),
    ] {
        let p = Project::new(&one_node(""), DONE);
        let server = StubServer::start(script);
        p.decisions(&config(&server.base_url, ""));
        let _lock = common::env_lock();
        let _env = p.env();
        let (status, _, events) = p.run();
        assert_eq!(status, RunStatus::Succeeded, "{error}");
        assert_eq!(
            node_output(&events, "w"),
            Some(("succeeded".into(), DONE.to_string()))
        );
        assert_eq!(anomalies(&events), 0);
        let d = decisions(&events);
        assert_eq!(d.len(), 1);
        assert_eq!(d[0].error.as_deref(), Some(error));
        assert!(d[0].answers.is_empty() && d[0].would_change.is_none());
    }
}

#[test]
fn a_slow_provider_times_out_and_its_latency_counts_in_the_attempt() {
    let p = Project::new(&one_node(""), DONE);
    let server = StubServer::start(vec![
        StubResponse::json(200, reply(0.9, "complete")).delayed(Duration::from_millis(1500)),
    ]);
    p.decisions(&config(&server.base_url, "timeout_ms: 400\n"));
    let _lock = common::env_lock();
    let _env = p.env();
    let (status, _, events) = p.run();
    assert_eq!(status, RunStatus::Succeeded);
    assert_eq!(decisions(&events)[0].error.as_deref(), Some("timeout"));
    let duration = events
        .iter()
        .find_map(|e| match &e.payload {
            EventPayload::AttemptFinished { duration_ms, .. } => *duration_ms,
            _ => None,
        })
        .unwrap();
    assert!(duration >= 400, "the attempt took {duration} ms");
}

#[test]
fn off_modes_and_skip_cases_ask_nothing() {
    /// A playbook, the agent's output, and an extra variable to set.
    type Case<'a> = (String, &'a str, Option<(&'static str, &'a str)>);
    let cases: Vec<Case> = vec![
        (one_node(", completion_check: off"), DONE, None),
        (one_node(", success_check: scripts/check.sh"), DONE, None),
        (one_node(""), "", None),
        (one_node(""), DONE, Some(("APB_DECISIONS", "off"))),
    ];
    for (yaml, out, extra_env) in cases {
        let p = Project::new(&yaml, out);
        let server = StubServer::start(vec![StubResponse::json(200, reply(0.9, "complete"))]);
        p.decisions(&config(&server.base_url, ""));
        let _lock = common::env_lock();
        let _env = p.env();
        let _extra = extra_env.map(|(k, v)| Env::set(&[(k, v)]));
        let (status, _, events) = p.run();
        assert_eq!(status, RunStatus::Succeeded, "{yaml}");
        assert!(decisions(&events).is_empty(), "{yaml} {extra_env:?}");
        assert_eq!(server.count(), 0);
    }
    // A marker check is not a script: the check still runs after it passes.
    let p = Project::new(&one_node(", success_check: { marker: \"passes\" }"), DONE);
    let server = StubServer::start(vec![StubResponse::json(200, reply(0.9, "complete"))]);
    p.decisions(&config(&server.base_url, ""));
    let _lock = common::env_lock();
    let _env = p.env();
    let (_, _, events) = p.run();
    assert_eq!(decisions(&events).len(), 1);
}

#[test]
fn a_resumed_run_replays_the_decision_without_a_request() {
    let p = Project::new(&one_node(""), DONE);
    let server = StubServer::start_with_fallback(
        vec![StubResponse::json(200, reply(0.93, "complete"))],
        StubResponse::json(200, reply(0.2, "partial")),
    );
    p.decisions(&config(&server.base_url, ""));
    let _lock = common::env_lock();
    let _env = p.env();
    let (_, run_id, _) = p.run();
    assert_eq!(server.count(), 1);

    let again = resume(p.root.path(), &run_id, Some("w")).unwrap();
    assert_eq!(again.outcome, RunStatus::Succeeded);
    let events = read_all(&p.run_dir(&run_id)).unwrap();
    assert_eq!(server.count(), 1, "the resumed attempt made no request");
    let d = decisions(&events);
    assert_eq!(d.len(), 1, "a replay journals nothing new");
    assert_eq!(d[0].answers["final_result"].p, Some(0.93));
    let attempts = |events: &[Event]| {
        events
            .iter()
            .filter(|e| matches!(e.payload, EventPayload::AttemptStarted { .. }))
            .count()
    };
    assert_eq!(attempts(&events), 2, "the node really ran again");

    // A different result on the next resume is a different state: asked anew.
    p.set_output(RUNNING);
    resume(p.root.path(), &run_id, Some("w")).unwrap();
    let events = read_all(&p.run_dir(&run_id)).unwrap();
    assert_eq!(server.count(), 2);
    assert_eq!(decisions(&events).len(), 2);
}

#[test]
fn a_provider_planted_in_the_manifest_is_never_asked_on_resume() {
    // The manifest lives in the project tree: its decisions block can only
    // narrow what the machine's decisions.yaml allows now.
    let p = Project::new(&one_node(""), DONE);
    let owner =
        StubServer::start_with_fallback(vec![], StubResponse::json(200, reply(0.9, "complete")));
    let planted =
        StubServer::start_with_fallback(vec![], StubResponse::json(200, reply(0.9, "complete")));
    p.decisions(&config(&owner.base_url, ""));
    let _lock = common::env_lock();
    let _env = p.env();
    let (_, run_id, _) = p.run();
    assert_eq!(owner.count(), 1);

    let manifest = p.run_dir(&run_id).join("manifest.yaml");
    let text = std::fs::read_to_string(&manifest).unwrap();
    assert!(text.contains(&owner.base_url));
    std::fs::write(&manifest, text.replace(&owner.base_url, &planted.base_url)).unwrap();
    p.set_output(RUNNING);
    resume(p.root.path(), &run_id, Some("w")).unwrap();
    assert_eq!(planted.count(), 0, "a planted provider URL gets no request");
    assert_eq!(
        owner.count(),
        1,
        "the owner's provider is not in the tampered block either"
    );
}

#[test]
fn the_budget_stops_at_its_request_count() {
    let p = Project::new(&two_nodes(), DONE);
    let server =
        StubServer::start_with_fallback(vec![], StubResponse::json(200, reply(0.9, "complete")));
    p.decisions(&config(
        &server.base_url,
        "budget: { max_requests_per_run: 1 }\n",
    ));
    let _lock = common::env_lock();
    let _env = p.env();
    let (status, _, events) = p.run();
    assert_eq!(status, RunStatus::Succeeded);
    let d = decisions(&events);
    assert_eq!(d.len(), 2);
    assert_eq!(
        (d[0].node.as_deref(), d[0].error.as_deref()),
        (Some("w"), None)
    );
    assert_eq!(
        (d[1].node.as_deref(), d[1].error.as_deref()),
        (Some("w2"), Some("budget"))
    );
    assert_eq!(server.count(), 1);
}

#[test]
fn secrets_paths_and_emails_never_reach_the_provider() {
    let p = Project::new(&one_node(""), "");
    let root = std::fs::canonicalize(p.root.path()).unwrap();
    p.set_output(&format!(
        "Done. Used key {SENTINEL_KEY} and token {SENTINEL_CONNECTOR_SECRET}, wrote {}/src/lib.rs, mailed {SENTINEL_EMAIL}, token ghp_abcdefghijklmnopqrstuvwx1234.",
        root.display()
    ));
    // An installed connector whose account references a secret variable,
    // resolved from the global secrets file.
    let conn = p.cfg.path().join("connectors/sentinel-conn");
    fs::create_dir_all(&conn).unwrap();
    fs::write(
        conn.join("connector.yaml"),
        "name: sentinel-conn\nversion: 0.1.0\naccount_fields:\n  - name: api_key\n    required: true\n    secret: true\nfunctions:\n  - name: ping\n    description: d\n    mock: { status: 200, body: {} }\n",
    )
    .unwrap();
    let acct = p
        .root
        .path()
        .join(".apb/connector-config/sentinel-conn.yaml");
    fs::create_dir_all(acct.parent().unwrap()).unwrap();
    fs::write(
        &acct,
        "accounts:\n  - name: a\n    api_key: \"{{env.SENTINEL_CONN_TOKEN}}\"\n",
    )
    .unwrap();
    fs::write(
        p.cfg.path().join("secrets.env"),
        format!("SENTINEL_CONN_TOKEN={SENTINEL_CONNECTOR_SECRET}\n"),
    )
    .unwrap();

    let server = StubServer::start(vec![StubResponse::json(200, reply(0.9, "complete"))]);
    p.decisions(&config(
        &server.base_url,
        "privacy: { debug_state: true }\n",
    ));
    let _lock = common::env_lock();
    let _env = p.env();
    let (_, run_id, _) = p.run();

    let raw = &server.requests()[0];
    let body = raw.split("\r\n\r\n").nth(1).unwrap();
    for leaked in [
        SENTINEL_KEY,
        SENTINEL_CONNECTOR_SECRET,
        SENTINEL_EMAIL,
        "ghp_abcdefghij",
        &root.display().to_string(),
    ] {
        assert!(
            !body.contains(leaked),
            "{leaked} reached the provider: {body}"
        );
    }
    assert!(
        body.contains("wrote src/lib.rs"),
        "paths arrive repo-relative: {body}"
    );
    assert!(body.contains("[email]"));
    // The key travels only in the header.
    assert!(
        raw.to_ascii_lowercase()
            .contains(&format!("bearer {}", SENTINEL_KEY.to_ascii_lowercase()))
    );

    // No decision record, debug state file or manifest holds the key, the
    // connector secret or the e-mail. (The node output itself is the agent's
    // own text and keeps whatever the agent printed.)
    let run_dir = p.run_dir(&run_id);
    let mut texts: Vec<(String, String)> = read_all(&run_dir)
        .unwrap()
        .into_iter()
        .filter(|e| matches!(e.payload, EventPayload::DecisionMade { .. }))
        .map(|e| {
            (
                "decision_made".to_string(),
                serde_json::to_string(&e).unwrap(),
            )
        })
        .collect();
    assert_eq!(texts.len(), 1);
    texts.push((
        "manifest".into(),
        fs::read_to_string(run_dir.join("manifest.yaml")).unwrap(),
    ));
    let debug: Vec<_> = fs::read_dir(run_dir.join("decisions"))
        .unwrap()
        .map(|e| e.unwrap().path())
        .collect();
    assert_eq!(debug.len(), 1, "one debug state file per decision");
    for path in debug {
        texts.push((
            path.display().to_string(),
            fs::read_to_string(&path).unwrap(),
        ));
    }
    for (name, text) in texts {
        for leaked in [SENTINEL_KEY, SENTINEL_CONNECTOR_SECRET, SENTINEL_EMAIL] {
            assert!(!text.contains(leaked), "{leaked} in {name}");
        }
    }
}

#[test]
fn send_nothing_empties_the_prompt_and_output_fields() {
    let p = Project::new(&one_node(""), DONE);
    let server = StubServer::start(vec![StubResponse::json(200, reply(0.9, "complete"))]);
    p.decisions(&config(&server.base_url, "privacy: { send: [] }\n"));
    let _lock = common::env_lock();
    let _env = p.env();
    p.run();
    let raw = &server.requests()[0];
    assert!(raw.contains(r#""task":"","result":"""#), "{raw}");
    assert!(!raw.contains("Implemented the parser"));
}

#[test]
fn a_project_can_switch_the_check_off() {
    let p = Project::new(&one_node(""), DONE);
    fs::write(
        p.root.path().join(".apb/config.yaml"),
        "decisions:\n  uses:\n    completion_check: { mode: off }\n",
    )
    .unwrap();
    let server = StubServer::start(vec![]);
    p.decisions(&config(&server.base_url, ""));
    let _lock = common::env_lock();
    let _env = p.env();
    let (_, run_id, events) = p.run();
    assert!(decisions(&events).is_empty());
    assert_eq!(server.count(), 0);
    let manifest = fs::read_to_string(p.run_dir(&run_id).join("manifest.yaml")).unwrap();
    assert!(!manifest.contains("decisions"));
}

// --- the report and replay over a real run (issue #165 Part 13) ---------------

/// Every file under a directory with its bytes, for an unchanged check.
fn tree(dir: &std::path::Path) -> Vec<(std::path::PathBuf, Vec<u8>)> {
    let mut out = Vec::new();
    for entry in fs::read_dir(dir).unwrap().flatten() {
        let path = entry.path();
        if path.is_dir() {
            out.extend(tree(&path));
        } else {
            out.push((path.clone(), fs::read(&path).unwrap()));
        }
    }
    out.sort();
    out
}

#[test]
fn the_report_labels_a_real_run_and_replay_leaves_its_journal_untouched() {
    use apb_engine::decision::report::{self, ReportFilter, ReportSettings, replay};

    let p = Project::new(&two_nodes(), DONE);
    let server = StubServer::start(vec![
        StubResponse::json(200, reply(0.93, "complete")),
        StubResponse::json(200, reply(0.91, "complete")),
    ]);
    let other = StubServer::start(vec![
        StubResponse::json(200, reply(0.05, "partial")),
        StubResponse::json(200, reply(0.95, "complete")),
    ]);
    // `other` comes second: the run never asks it, only replay does.
    p.decisions(&format!(
        "version: 1\nmode: shadow\nproviders:\n  - {{ id: stub, kind: systemone, base_url: \"{}\", model: jev-1.13.0 }}\n  - {{ id: other, kind: systemone, base_url: \"{}\", model: jev-1.14.0 }}\nuses:\n  completion_check: {{ mode: shadow }}\nprivacy: {{ debug_state: true }}\n",
        server.base_url, other.base_url
    ));
    let _lock = common::env_lock();
    let _env = p.env();
    let (status, run_id, _) = p.run();
    assert_eq!(status, RunStatus::Succeeded);
    assert_eq!((server.count(), other.count()), (2, 0));

    let roots = [p.root.path().to_path_buf()];
    let r = report::report(&roots, &ReportFilter::default(), &ReportSettings::default());
    assert_eq!(r.decisions, 2);
    let g = &r.groups[0];
    assert_eq!(
        (g.provider.as_str(), g.provider_kind.as_deref()),
        ("stub", Some("systemone"))
    );
    // `w` is followed by a successful `w2`, and `w2` ends a successful run.
    assert_eq!(
        (g.labelled, g.keep_labels, g.all.accuracy),
        (2, 2, Some(1.0))
    );

    let run_dir = p.run_dir(&run_id);
    let before = tree(&run_dir);
    assert!(
        run_dir.join("decisions").is_dir(),
        "the run kept its debug state"
    );
    assert_eq!(
        replay::replay(&roots, p.cfg.path(), None, &ReportFilter::default(), 10).unwrap_err(),
        replay::ReplayError::NoProvider
    );
    let s = replay::replay(
        &roots,
        p.cfg.path(),
        Some("other"),
        &ReportFilter::default(),
        10,
    )
    .unwrap();
    assert_eq!((s.matched, s.with_state, s.asked, s.errors), (2, 2, 2, 0));
    // `other` flags `w` (0.05 < 0.15): one of two agree, and labelled
    // accuracy drops from 2/2 to 1/2.
    assert_eq!(s.agreement, Some(0.5));
    assert_eq!(
        (s.original_accuracy, s.replay_accuracy),
        (Some(1.0), Some(0.5))
    );
    assert_eq!(other.count(), 2);
    // The replayed state is the one that was sent, in the same order.
    assert!(other.requests()[0].contains(r#""state":{"task":"#));
    let results = s.results.expect("results written");
    assert!(results.starts_with(p.cfg.path().join("decisions-replay")));
    assert_eq!(tree(&run_dir), before, "replay never touches the run");
}
