//! A8: a scenario run follows the same rules as a real run. The same
//! playbook runs twice: once for real, with a stub agent CLI, a real
//! script and the `fake` decision provider; once as a quick scenario run
//! whose answers equal what those return. The journals, normalized for what
//! only the executor and the clock decide, are equal event for event, and
//! so are the final run states.

use std::fs;
use std::os::unix::fs::PermissionsExt as _;
use std::path::Path;

use apb_core::registry::init_project;
use apb_core::scenario::{Scenario, ScenarioLevel};
use apb_engine::event::{Event, read_all};
use apb_engine::scenario::session::ScenarioPrep;
use apb_engine::scheduler::{RunOptions, run, run_scenario};
use apb_engine::state::RunState;
use serde_json::{Value, json};

use crate::common;

/// An agent works (its first attempt fails, the retry succeeds), a script
/// gates, a judge routes; a red verdict would loop back once.
const PLAYBOOK: &str = r#"schema: 2
id: parity
name: parity
version: 1.0.0
defaults: { profile: main }
nodes:
  - { id: start, type: start }
  - { id: work, type: agent_task, prompt: "Fix {{run.instruction}}", max_retries: 1 }
  - { id: gate, type: script, script: scripts/gate.sh, runner: sh }
  - id: triage
    type: judge
    state: { out: "{{nodes.gate.output}}" }
    questions:
      verdict: { type: choice, instructions: "Is `out` green?", criteria: { green: all pass, red: a failure, unclear: cannot tell } }
    on_unavailable: fail
  - { id: done, type: finish, outcome: success }
  - { id: failed, type: finish, outcome: failure }
edges:
  - { from: start, to: work }
  - { from: work, to: gate, condition: { type: node_status, node: work, equals: success } }
  - { from: work, to: failed, condition: { type: node_status, node: work, equals: failure } }
  - { from: gate, to: triage }
  - { id: again, from: triage, to: work, condition: { type: output_field, node: triage, field: verdict, equals: red }, max_traversals: 1 }
  - { from: triage, to: done, fallback: true }
"#;

/// The stub agent: the first time it reports that it failed (in its status
/// file, as an agent does), then it prints the fix.
const AGENT: &str = "#!/bin/sh\nif [ -f \"$PARITY_DIR/tried\" ]; then echo 'fixed the parser'; else touch \"$PARITY_DIR/tried\"; echo 'cannot build'; printf '{\"status\":\"failure\"}' > \"$APB_STATUS_FILE\"; fi\n";

/// The scenario with the answers the real run's stub, script and provider
/// give.
const SCENARIO: &str = r#"schema: 1
id: parity
instruction: the parser
answers:
  work:
    - visit: 1
      attempts:
        - { status: failed, output: "cannot build" }
        - { status: succeeded, output: "fixed the parser" }
  gate:
    - { visit: 1, script: { exit: 0, stdout: "QA green" } }
  triage:
    - { visit: 1, judge: { verdict: { value: green, p: 0.8, confidence: 0.8 } } }
"#;

const DECISIONS: &str = "version: 1\nmode: enforce\nproviders:\n  - id: fake\n    kind: fake\n    answers:\n      verdict: { type: choice, choice: green, probabilities: { green: 0.8, red: 0.1, unclear: 0.1 }, confidence: 0.8 }\nuses:\n  judge_node: { mode: enforce }\n";

fn project(root: &Path) {
    init_project(root).unwrap();
    let v = root.join(".apb/playbooks/parity/1.0.0");
    fs::create_dir_all(v.join("scripts")).unwrap();
    fs::write(v.join("playbook.yaml"), PLAYBOOK).unwrap();
    fs::write(v.join("scripts/gate.sh"), "echo 'QA green'\n").unwrap();
    fs::write(root.join(".apb/playbooks/parity/current"), "1.0.0").unwrap();
    common::seed_main(root);
}

/// Fields only the executor, the clock, the project path or the run's
/// identity decide. A scenario run works in `work/` inside its run
/// directory, never in the project tree, so `workdir` and
/// `execution_root` differ by design.
const VOLATILE: [&str; 19] = [
    "execution_root",
    "session",
    "workdir",
    "soul_delivery",
    "profiles",
    "seq",
    "ts",
    "run_id",
    "pid",
    "spawn_ms",
    "agent",
    "model",
    "exec",
    "duration_ms",
    "latency_ms",
    "token",
    "attempt_token",
    "definition",
    "started_at",
];

fn normalized(events: &[Event]) -> Vec<Value> {
    events
        .iter()
        .map(|e| {
            let mut v = serde_json::to_value(e).unwrap();
            if let Value::Object(m) = &mut v {
                for k in VOLATILE {
                    m.remove(k);
                }
            }
            v
        })
        .collect()
}

/// The decision a judge made, without who answered and how fast: the
/// `decision_made` provider fields and the `decided_by` of the judge's
/// output.
fn decision_of(v: &mut Value) {
    if v["type"] == "decision_made"
        && let Value::Object(m) = v
    {
        for k in [
            "provider",
            "calibrated",
            "state_digest",
            "state_bytes",
            "cost_usd",
        ] {
            m.remove(k);
        }
    }
    if v["type"] == "node_finished"
        && let Some(Ok(Value::Object(mut out))) =
            v["output"].as_str().map(serde_json::from_str::<Value>)
        && out.remove("decided_by").is_some()
    {
        v["output"] = Value::String(Value::Object(out).to_string());
    }
}

#[test]
fn a_quick_scenario_run_journals_what_a_real_run_journals() {
    let _lock = common::env_lock();
    let cfg = tempfile::tempdir().unwrap();
    fs::write(cfg.path().join("decisions.yaml"), DECISIONS).unwrap();
    let stub_dir = tempfile::tempdir().unwrap();
    let agent = stub_dir.path().join("agent.sh");
    fs::write(&agent, AGENT).unwrap();
    fs::set_permissions(&agent, fs::Permissions::from_mode(0o755)).unwrap();
    let _env = [
        common::EnvVar::set("APB_CONFIG_DIR", cfg.path()),
        common::EnvVar::set("APB_AGENT_CMD", &agent),
        common::EnvVar::set("APB_DECISIONS_ALLOW_FAKE", "1"),
        common::EnvVar::set("PARITY_DIR", stub_dir.path()),
    ];

    let real_root = tempfile::tempdir().unwrap();
    project(real_root.path());
    let opts = RunOptions {
        instruction: Some("the parser".into()),
        ..RunOptions::default()
    };
    let real = run(real_root.path(), "parity", None, opts).unwrap();
    let real_dir = real_root.path().join(".apb/runs").join(&real.run_id);
    let real_events = read_all(&real_dir).unwrap();

    let quick_root = tempfile::tempdir().unwrap();
    project(quick_root.path());
    let prep = ScenarioPrep {
        scenario: yaml_serde::from_str::<Scenario>(SCENARIO).unwrap(),
        level: ScenarioLevel::Quick,
        scenario_digest: "sha256:parity".into(),
        max_requests: 20,
        sent: Default::default(),
        external_data: false,
    };
    let quick = run_scenario(quick_root.path(), "parity", None, &prep).unwrap();
    let quick_events = read_all(&quick.run_dir).unwrap();

    assert_eq!(real.outcome, quick.outcome);
    let mut a = normalized(&real_events);
    let mut b = normalized(&quick_events);
    a.iter_mut().for_each(decision_of);
    b.iter_mut().for_each(decision_of);
    let types = |v: &[Value]| v.iter().map(|e| e["type"].clone()).collect::<Vec<_>>();
    assert_eq!(types(&a), types(&b), "the event sequence differs");
    for (x, y) in a.iter().zip(&b) {
        assert_eq!(x, y, "{}", json!({ "real": x, "quick": y }));
    }
    let state = |events: &[Event]| {
        let s = RunState::fold(events);
        let outputs: std::collections::BTreeMap<String, String> = s
            .outputs
            .iter()
            .map(|(node, out)| {
                let mut v = json!({ "type": "node_finished", "output": out });
                decision_of(&mut v);
                (
                    node.clone(),
                    v["output"].as_str().unwrap_or_default().to_string(),
                )
            })
            .collect();
        format!(
            "{:?} {:?} {:?} {outputs:?}",
            s.run_status, s.nodes, s.attempts
        )
    };
    assert_eq!(state(&real_events), state(&quick_events));
}
