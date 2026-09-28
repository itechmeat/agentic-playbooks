//! The judge node and the judge edge in a run (issue #165 Parts 5-7):
//! routing on the answer at enforce, every `on_unavailable` form without a
//! provider, emulation through a profile and through an OpenAI-compatible
//! endpoint, journal first, replay on resume, the node cache, and judge
//! edges with both `on_unavailable` values and loop executions.

use std::fs;
use std::os::unix::fs::PermissionsExt;

use apb_core::registry::init_project;
use apb_decide::testing::{StubResponse, StubServer};
use apb_engine::event::{Event, EventPayload, read_all};
use apb_engine::scheduler::{RunOptions, resume, run};
use apb_engine::state::RunStatus;
use serde_json::{Value, json};

use crate::common;

/// Sets environment variables for one test and restores them on drop.
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

const REVIEW: &str = "Found one defect: parser.rs line 40 drops the last token.";

struct Project {
    root: tempfile::TempDir,
    cfg: tempfile::TempDir,
}

impl Project {
    fn new(playbook_yaml: &str) -> Self {
        let root = tempfile::tempdir().unwrap();
        let cfg = tempfile::tempdir().unwrap();
        init_project(root.path()).unwrap();
        let id = serde_yaml_ng::from_str::<Value>(playbook_yaml).unwrap()["id"]
            .as_str()
            .unwrap()
            .to_string();
        let dir = root.path().join(format!(".apb/playbooks/{id}/1.0.0"));
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("playbook.yaml"), playbook_yaml).unwrap();
        fs::write(
            root.path().join(format!(".apb/playbooks/{id}/current")),
            "1.0.0",
        )
        .unwrap();
        common::seed_main(root.path());
        let p = Project { root, cfg };
        p.set_review(REVIEW);
        p.set_emulation("{}");
        p
    }

    fn set_review(&self, text: &str) {
        fs::write(self.root.path().join("stdout.txt"), text).unwrap();
    }

    /// What the stub agent answers when it is asked to emulate a decision.
    fn set_emulation(&self, text: &str) {
        fs::write(self.root.path().join("emulation.txt"), text).unwrap();
    }

    fn agent(&self) -> String {
        let path = self.root.path().join("agent.sh");
        let r = self.root.path().display();
        common::write_sync(
            &path,
            &format!(
                "#!/bin/sh\ncase \"$*\" in\n  *'stand in for a decision model'*) echo x >> '{r}/emulations.log'; cat '{r}/emulation.txt' ;;\n  *) cat '{r}/stdout.txt' ;;\nesac\n"
            ),
        );
        let mut perm = fs::metadata(&path).unwrap().permissions();
        perm.set_mode(0o755);
        fs::set_permissions(&path, perm).unwrap();
        path.to_string_lossy().to_string()
    }

    fn emulations(&self) -> usize {
        fs::read_to_string(self.root.path().join("emulations.log"))
            .map(|s| s.lines().count())
            .unwrap_or(0)
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
            ("APB_DECISIONS_ALLOW_FAKE", "1"),
        ])
    }

    fn run(&self, id: &str) -> (RunStatus, String, Vec<Event>) {
        let res = run(self.root.path(), id, None, RunOptions::default()).unwrap();
        let events = read_all(&self.run_dir(&res.run_id)).unwrap();
        (res.outcome, res.run_id, events)
    }

    fn run_dir(&self, run_id: &str) -> std::path::PathBuf {
        self.root.path().join(".apb/runs").join(run_id)
    }
}

fn example() -> String {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../examples/playbooks/review-triage.yaml");
    fs::read_to_string(path).unwrap()
}

/// The example with its human review gate replaced by a finish node, so an
/// autonomous test run ends where the gate would wait.
fn without_gate(yaml: &str) -> String {
    let mut doc: Value = serde_yaml_ng::from_str(yaml).unwrap();
    for n in doc["nodes"].as_array_mut().unwrap() {
        if n["id"] == "human_review" {
            *n = json!({"id": "human_review", "type": "finish", "outcome": "success"});
        }
    }
    doc["edges"]
        .as_array_mut()
        .unwrap()
        .retain(|e| e["from"] != "human_review");
    serde_yaml_ng::to_string(&doc).unwrap()
}

fn choice(value: &str, p: f64) -> Value {
    let mut probs = serde_json::Map::new();
    for o in ["clean", "needs_fix", "unclear"] {
        probs.insert(
            o.into(),
            json!(if o == value { p } else { (1.0 - p) / 2.0 }),
        );
    }
    json!({"type": "choice", "choice": value, "probabilities": probs, "confidence": 0.8})
}

fn fake(mode: &str, verdict: &str) -> String {
    format!(
        "version: 1\nmode: enforce\nproviders:\n  - id: fake\n    kind: fake\n    answers:\n      verdict: {}\n      risky: {{ type: noul, noul: 0.1 }}\nuses:\n  judge_node: {{ mode: {mode} }}\n",
        choice(verdict, 0.8)
    )
}

fn output(events: &[Event], node: &str) -> Option<(String, Value)> {
    events.iter().rev().find_map(|e| match &e.payload {
        EventPayload::NodeFinished {
            node: n,
            status,
            output,
            ..
        } if n == node => Some((
            status.clone(),
            serde_json::from_str(output).unwrap_or(Value::String(output.clone())),
        )),
        _ => None,
    })
}

fn ran(events: &[Event], node: &str) -> bool {
    events
        .iter()
        .any(|e| matches!(&e.payload, EventPayload::NodeFinished { node: n, .. } if n == node))
}

/// `(use_site, applied, calibrated, provider, error)` per decision.
type Decided = (String, bool, bool, Option<String>, Option<String>);

fn decisions(events: &[Event]) -> Vec<Decided> {
    events
        .iter()
        .filter_map(|e| match &e.payload {
            EventPayload::DecisionMade {
                use_site,
                applied,
                calibrated,
                provider,
                error,
                ..
            } => Some((
                use_site.clone(),
                *applied,
                *calibrated,
                provider.clone(),
                error.clone(),
            )),
            _ => None,
        })
        .collect()
}

#[test]
fn the_example_routes_on_the_verdict_with_the_fake_provider() {
    for (verdict, next, skipped) in [
        ("needs_fix", "fix", "human_review"),
        ("clean", "done", "fix"),
    ] {
        let p = Project::new(&example());
        p.decisions(&fake("enforce", verdict));
        let _lock = common::env_lock();
        let _env = p.env();
        let (status, _, events) = p.run("review-triage");
        assert_eq!(status, RunStatus::Succeeded, "{verdict}");
        let (st, out) = output(&events, "triage").unwrap();
        assert_eq!(st, "succeeded");
        assert_eq!(out["verdict"], json!(verdict));
        assert_eq!(out["verdict_p"], json!(0.8));
        assert_eq!(out["risky"], json!(false));
        assert_eq!(out["decided_by"], json!("fake/fake-1"));
        assert!(ran(&events, next) && !ran(&events, skipped), "{verdict}");
        let d = decisions(&events);
        assert_eq!(d.len(), 1);
        assert_eq!((d[0].0.as_str(), d[0].1), ("judge_node", true));
        // Journal first: the decision precedes the node's output.
        let pos = |pred: &dyn Fn(&EventPayload) -> bool| {
            events.iter().position(|e| pred(&e.payload)).unwrap()
        };
        assert!(
            pos(&|e| matches!(e, EventPayload::DecisionMade { .. }))
                < pos(
                    &|e| matches!(e, EventPayload::NodeFinished { node, .. } if node == "triage")
                )
        );
    }
}

#[test]
fn below_enforce_or_without_a_provider_the_route_is_taken() {
    // No decisions.yaml: no request, no decision event, the declared route.
    let p = Project::new(&without_gate(&example()));
    let _lock = common::env_lock();
    let _env = p.env();
    let (status, _, events) = p.run("review-triage");
    assert_eq!(status, RunStatus::Succeeded);
    assert!(decisions(&events).is_empty());
    let (st, out) = output(&events, "triage").unwrap();
    assert_eq!(st, "succeeded");
    assert_eq!(
        out,
        json!({"decided_by": "unavailable", "reason": "not_configured"})
    );
    assert!(ran(&events, "human_review") && !ran(&events, "fix"));

    // Shadow: asked and journaled, never applied, the route still taken.
    let p = Project::new(&without_gate(&example()));
    p.decisions(&fake("shadow", "needs_fix"));
    let _env = p.env();
    let (_, _, events) = p.run("review-triage");
    let d = decisions(&events);
    assert_eq!(d.len(), 1);
    assert!(!d[0].1, "shadow never applies");
    assert_eq!(
        output(&events, "triage").unwrap().1["reason"],
        json!("mode")
    );
    assert!(!ran(&events, "fix"));
}

fn small(on_unavailable: &str, extra_defaults: &str) -> String {
    format!(
        "schema: 2\nid: small\nname: small\nversion: 1.0.0\ndefaults:\n  profile: main\n{extra_defaults}nodes:\n  - {{ id: start, type: start }}\n  - {{ id: review, type: agent_task, prompt: review }}\n  - id: triage\n    type: judge\n    state: {{ review_output: \"{{{{nodes.review.output}}}}\" }}\n    questions:\n      verdict: {{ type: choice, instructions: \"Which outcome does `review_output` report?\", criteria: {{ clean: a, needs_fix: b, unclear: c }} }}\n      risky: {{ type: noul, instructions: \"Is `review_output` risky?\" }}\n{on_unavailable}  - {{ id: fix, type: finish, outcome: success }}\n  - {{ id: done, type: finish, outcome: success }}\n  - {{ id: failed, type: finish, outcome: failure }}\nedges:\n  - {{ from: start, to: review }}\n  - {{ from: review, to: triage }}\n  - {{ from: triage, to: failed, condition: {{ type: node_status, node: triage, equals: failure }} }}\n  - {{ from: triage, to: fix, condition: {{ type: output_field, node: triage, field: verdict, equals: needs_fix }} }}\n  - {{ from: triage, to: done, fallback: true }}\n"
    )
}

#[test]
fn default_and_fail_apply_without_a_provider() {
    let p = Project::new(&small(
        "    on_unavailable: { default: { verdict: needs_fix, risky: true } }\n",
        "",
    ));
    let _lock = common::env_lock();
    let _env = p.env();
    let (status, _, events) = p.run("small");
    assert_eq!(status, RunStatus::Succeeded);
    assert_eq!(
        output(&events, "triage").unwrap().1,
        json!({"verdict": "needs_fix", "risky": true, "decided_by": "default", "reason": "not_configured"})
    );
    assert!(ran(&events, "fix"));

    // `fail`, and an absent on_unavailable, fail the node (not validation).
    for fallback in ["    on_unavailable: fail\n", ""] {
        let p = Project::new(&small(fallback, ""));
        let _env = p.env();
        let (status, _, events) = p.run("small");
        assert_eq!(output(&events, "triage").unwrap().0, "failed");
        assert!(ran(&events, "failed"));
        assert_eq!(status, RunStatus::Failed);
    }
}

#[test]
fn emulate_asks_the_profile_once_and_journals_it_uncalibrated() {
    let p = Project::new(&small("    on_unavailable: emulate\n", ""));
    // The BTreeMap order of the questions is risky, verdict -> q1, q2.
    p.set_emulation("```json\n{\"q1\": 0.9, \"q2\": {\"clean\": 0.1, \"needs_fix\": 0.8, \"unclear\": 0.1}}\n```");
    let _lock = common::env_lock();
    let _env = p.env();
    let (status, run_id, events) = p.run("small");
    assert_eq!(status, RunStatus::Succeeded);
    let out = output(&events, "triage").unwrap().1;
    assert_eq!(out["verdict"], json!("needs_fix"));
    assert_eq!(out["risky"], json!(true));
    assert_eq!(out["decided_by"], json!("emulated:profile:main"));
    assert_eq!(p.emulations(), 1);
    let d = decisions(&events);
    assert_eq!(d.len(), 1);
    assert_eq!(
        (d[0].1, d[0].2, d[0].3.as_deref()),
        (true, false, Some("profile:main"))
    );
    let attempts = events
        .iter()
        .filter(|e| matches!(&e.payload, EventPayload::AttemptFinished { node, .. } if node == "triage"))
        .count();
    assert_eq!(
        attempts, 1,
        "one agent attempt, journaled as a normal attempt"
    );

    // Resume from the judge: the emulated answer is replayed, no agent runs.
    resume(p.root.path(), &run_id, Some("triage")).unwrap();
    let events = read_all(&p.run_dir(&run_id)).unwrap();
    assert_eq!(p.emulations(), 1);
    assert_eq!(decisions(&events).len(), 1);
    assert_eq!(
        output(&events, "triage").unwrap().1["verdict"],
        json!("needs_fix")
    );
}

#[test]
fn emulate_goes_to_a_configured_endpoint_when_the_native_provider_is_down() {
    let native = StubServer::start(vec![]);
    let completion = json!({
        "model": "small-model",
        "choices": [{"message": {"content": "{\"q1\": 0.2, \"q2\": {\"clean\": 0.9, \"needs_fix\": 0.05, \"unclear\": 0.05}}"}}],
        "usage": {"prompt_tokens": 100, "completion_tokens": 10}
    })
    .to_string();
    let endpoint = StubServer::start(vec![StubResponse::json(200, completion)]);
    let p = Project::new(&small("    on_unavailable: emulate\n", ""));
    p.decisions(&format!(
        "version: 1\nmode: enforce\ntimeout_ms: 2000\nproviders:\n  - {{ id: native, kind: systemone, base_url: \"{}\", model: jev-1.13.0 }}\n  - {{ id: emu, kind: llm_emulation, via: openai_compatible, base_url: \"{}/v1\", model: small-model }}\nuses:\n  judge_node: {{ mode: enforce }}\n",
        native.base_url, endpoint.base_url
    ));
    let _lock = common::env_lock();
    let _env = p.env();
    let (status, _, events) = p.run("small");
    assert_eq!(status, RunStatus::Succeeded);
    let out = output(&events, "triage").unwrap().1;
    assert_eq!(out["verdict"], json!("clean"));
    assert_eq!(out["decided_by"], json!("emulated:emu"));
    assert_eq!(endpoint.count(), 1);
    assert_eq!(p.emulations(), 0, "the profile was not needed");
    let d = decisions(&events);
    assert_eq!(d.len(), 2, "the native failure, then the emulated answer");
    assert_eq!(d[0].4.as_deref(), Some("unavailable"));
    assert_eq!(
        (d[1].1, d[1].2, d[1].3.as_deref()),
        (true, false, Some("emu"))
    );
    assert!(endpoint.requests()[0].contains("<document>"));
}

#[test]
fn a_resumed_judge_replays_its_answer_without_a_request() {
    let server = StubServer::start_with_fallback(
        vec![],
        StubResponse::json(
            200,
            json!({"model": "jev-1.13.0", "answers": {"verdict": choice("needs_fix", 0.9), "risky": {"type": "noul", "noul": 0.2}}}).to_string(),
        ),
    );
    let p = Project::new(&small("    on_unavailable: fail\n", ""));
    p.decisions(&format!(
        "version: 1\nmode: enforce\nproviders:\n  - {{ id: stub, kind: systemone, base_url: \"{}\", model: jev-1.13.0 }}\nuses:\n  judge_node: {{ mode: enforce }}\n",
        server.base_url
    ));
    let _lock = common::env_lock();
    let _env = p.env();
    let (_, run_id, _) = p.run("small");
    assert_eq!(server.count(), 1);
    resume(p.root.path(), &run_id, Some("triage")).unwrap();
    let events = read_all(&p.run_dir(&run_id)).unwrap();
    assert_eq!(server.count(), 1, "the resumed judge made no request");
    assert_eq!(decisions(&events).len(), 1);
    assert_eq!(
        output(&events, "triage").unwrap().1["decided_by"],
        json!("stub/jev-1.13.0")
    );
}

#[test]
fn a_cached_judge_answer_is_reused_across_runs_without_a_request() {
    let server = StubServer::start_with_fallback(
        vec![],
        StubResponse::json(
            200,
            json!({"model": "jev-1.13.0", "answers": {"verdict": choice("clean", 0.9), "risky": {"type": "noul", "noul": 0.2}}}).to_string(),
        ),
    );
    let yaml = small("    on_unavailable: fail\n    cache: auto\n", "");
    let p = Project::new(&yaml);
    p.decisions(&format!(
        "version: 1\nmode: enforce\nproviders:\n  - {{ id: stub, kind: systemone, base_url: \"{}\", model: jev-1.13.0 }}\nuses:\n  judge_node: {{ mode: enforce }}\n",
        server.base_url
    ));
    let _lock = common::env_lock();
    let _env = p.env();
    let (_, _, first) = p.run("small");
    assert!(
        first
            .iter()
            .any(|e| matches!(e.payload, EventPayload::NodeCacheStored { .. })),
        "{:?}",
        first.iter().map(|e| &e.payload).collect::<Vec<_>>()
    );
    let (status, _, second) = p.run("small");
    assert_eq!(status, RunStatus::Succeeded);
    assert_eq!(server.count(), 1, "the second run answered from the cache");
    assert!(
        second
            .iter()
            .any(|e| matches!(e.payload, EventPayload::NodeCacheHit { .. }))
    );
    assert_eq!(
        output(&second, "triage").unwrap().1["verdict"],
        json!("clean")
    );
}

fn edges_playbook(loops: bool) -> String {
    // With `loops`, the fallback goes round through a condition node that
    // allows two more passes, then leaves for `done`.
    let (extra_node, tail) = if loops {
        (
            "  - { id: again, type: condition, max_loops: 2 }\n",
            "  - { from: review, to: again, fallback: true }\n  - { from: again, to: review }\n  - { from: again, to: done, fallback: true }\n",
        )
    } else {
        ("", "  - { from: review, to: done, fallback: true }\n")
    };
    format!(
        "schema: 2\nid: edges\nname: edges\nversion: 1.0.0\ndefaults:\n  profile: main\nnodes:\n  - {{ id: start, type: start }}\n  - {{ id: review, type: agent_task, prompt: review }}\n{extra_node}  - {{ id: fix, type: finish, outcome: success }}\n  - {{ id: ship, type: finish, outcome: success }}\n  - {{ id: done, type: finish, outcome: success }}\nedges:\n  - {{ from: start, to: review }}\n  - {{ from: review, to: fix, condition: {{ type: judge, question: \"Does `output` report a defect?\", min_p: 0.7, on_unavailable: false }} }}\n  - {{ from: review, to: ship, condition: {{ type: judge, question: \"Is `output` ready to ship?\", min_p: 0.7, on_unavailable: true }} }}\n{tail}"
    )
}

fn edge_config(defect: f64, ship: f64, mode: &str) -> String {
    format!(
        "version: 1\nmode: enforce\nproviders:\n  - id: fake\n    kind: fake\n    answers:\n      edge_0: {{ type: noul, noul: {defect} }}\n      edge_1: {{ type: noul, noul: {ship} }}\nuses:\n  judge_edge: {{ mode: {mode} }}\n"
    )
}

#[test]
fn judge_edges_share_one_request_and_route_on_min_p() {
    for (defect, ship, next) in [(0.9, 0.9, "fix"), (0.2, 0.8, "ship"), (0.2, 0.3, "done")] {
        let p = Project::new(&edges_playbook(false));
        p.decisions(&edge_config(defect, ship, "enforce"));
        let _lock = common::env_lock();
        let _env = p.env();
        let (status, _, events) = p.run("edges");
        assert_eq!(status, RunStatus::Succeeded);
        let d = decisions(&events);
        assert_eq!(d.len(), 1, "one request for both edges");
        assert_eq!((d[0].0.as_str(), d[0].1), ("judge_edge", true));
        assert!(ran(&events, next), "{defect}/{ship} -> {next}");
    }
}

#[test]
fn without_a_provider_on_unavailable_decides_each_edge() {
    // edge 0 (false) is skipped, edge 1 (true) is taken.
    let p = Project::new(&edges_playbook(false));
    let _lock = common::env_lock();
    let _env = p.env();
    let (_, _, events) = p.run("edges");
    assert!(decisions(&events).is_empty());
    assert!(ran(&events, "ship") && !ran(&events, "fix"));

    // Shadow: asked, journaled with would_change, not applied.
    let p = Project::new(&edges_playbook(false));
    p.decisions(&edge_config(0.9, 0.1, "shadow"));
    let _env = p.env();
    let (_, _, events) = p.run("edges");
    assert_eq!(decisions(&events).len(), 1);
    assert!(ran(&events, "ship"), "shadow keeps on_unavailable routing");
}

#[test]
fn every_loop_execution_gets_its_own_answer() {
    let p = Project::new(&edges_playbook(true));
    // Neither judge edge matches, so the loop edge is taken twice.
    p.decisions(&edge_config(0.1, 0.1, "enforce"));
    let _lock = common::env_lock();
    let _env = p.env();
    let (status, _, events) = p.run("edges");
    assert_eq!(
        status,
        RunStatus::Succeeded,
        "{:#?}",
        events.iter().map(|e| &e.payload).collect::<Vec<_>>()
    );
    let attempts: Vec<Option<u32>> = events
        .iter()
        .filter_map(|e| match &e.payload {
            EventPayload::DecisionMade {
                use_site, attempt, ..
            } if use_site == "judge_edge" => Some(*attempt),
            _ => None,
        })
        .collect();
    assert_eq!(attempts, vec![Some(1), Some(2), Some(3)]);
    assert!(ran(&events, "done"));
}
