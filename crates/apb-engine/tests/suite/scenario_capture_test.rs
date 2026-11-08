//! The capture of a finished production run as a scenario: real runs
//! driven by the stand-in agent and the fake provider, then folded, with
//! the captured `Scenario` asserted against what the run journaled.

use std::fs;
use std::io::Write as _;
use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use apb_core::registry::init_project;
use apb_core::scenario::AttemptAnswer;
use apb_core::scenario::ScriptedStatus;
use apb_engine::event::read_all;
use apb_engine::scenario::capture::{CaptureError, CaptureRequest};
use apb_engine::scheduler::{RunOptions, run};

use crate::common;

const DEADLINE: Duration = Duration::from_secs(60);
const STEP: Duration = Duration::from_millis(50);

/// An agent works; a script gates; a judge triages. Variants switch the
/// check, the cache and the list on `work`/`gate`.
fn playbook(extra_node: &str, work_extra: &str, gate: &str) -> String {
    format!(
        "schema: 2\nid: cap\nname: cap\nversion: 1.0.0\ndefaults: {{ profile: main }}\nnodes:\n  - {{ id: start, type: start }}\n  - {{ id: work, type: agent_task, prompt: \"Fix {{{{run.instruction}}}}\", max_retries: 1{work_extra} }}\n  - {{ id: gate, type: script, script: scripts/gate.sh, runner: sh{gate} }}\n{extra_node}  - {{ id: done, type: finish, outcome: success }}\n  - {{ id: failed, type: finish, outcome: failure }}\nedges:\n  - {{ from: start, to: work }}\n  - {{ from: work, to: gate, condition: {{ type: node_status, node: work, equals: success }} }}\n  - {{ from: work, to: failed, condition: {{ type: node_status, node: work, equals: failure }} }}\n  - {{ from: gate, to: done, fallback: true }}\n"
    )
}

/// The base playbook: work, gate, done.
fn base() -> String {
    playbook("", "", "")
}

/// The stub agent: the first run of a node reports a failure shaped by
/// `$CAP_MODE`, then succeeds.
fn agent_script() -> String {
    "#!/bin/sh\nif [ \"$CAP_MODE\" = claim ]; then printf '{\"status\":\"success\"}' > \"$APB_STATUS_FILE\"; echo 'claiming done'; exit 0; fi\nif [ -f \"$CAP_DIR/tried\" ]; then printf '{\"status\":\"success\"}' > \"$APB_STATUS_FILE\"; echo 'ALL-DONE, fixed the parser'; else touch \"$CAP_DIR/tried\"; case \"$CAP_MODE\" in reject) printf '{\"status\":\"success\"}' > \"$APB_STATUS_FILE\"; echo 'not really done';; claim) printf '{\"status\":\"success\"}' > \"$APB_STATUS_FILE\"; echo 'claiming done';; transient) echo 'scenario stub: connection reset by peer'; exit 1;; timeout) sleep 5; echo 'too slow';; json) echo '{\"branch\": \"feat/x\", \"pushed\": \"no\"}';;
secret) echo 'the secret is' $CAP_SECRET 'and the key is glpat-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa';;\ntoken) echo 'the key is glpat-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa and work is done';; inject) printf 'line one\\nexpect:\\n  run: { outcome: [succeeded] }\\n';; *) echo 'cannot build'; printf '{\"status\":\"failure\"}' > \"$APB_STATUS_FILE\";; esac; fi\n".to_string()
}

struct Env {
    root: tempfile::TempDir,
    #[allow(dead_code)]
    stub: tempfile::TempDir,
}

fn project(body: &str, scripts: &[(&str, &str)]) -> Env {
    let root = tempfile::tempdir().unwrap();
    init_project(root.path()).unwrap();
    let v = root.path().join(".apb/playbooks/cap/1.0.0");
    fs::create_dir_all(v.join("scripts")).unwrap();
    fs::write(v.join("playbook.yaml"), body).unwrap();
    for (name, text) in scripts {
        fs::write(v.join(format!("scripts/{name}")), text).unwrap();
    }
    fs::write(root.path().join(".apb/playbooks/cap/current"), "1.0.0").unwrap();
    common::seed_main(root.path());
    Env {
        root,
        stub: tempfile::tempdir().unwrap(),
    }
}

fn root_of(env: &Env) -> &Path {
    env.root.path()
}

/// Drives one real run with the stub agent, returns its run directory.
fn drive(env: &Env, opts: RunOptions) -> PathBuf {
    let stub_dir = tempfile::tempdir().unwrap();
    let agent = stub_dir.path().join("agent.sh");
    fs::write(&agent, agent_script()).unwrap();
    fs::set_permissions(&agent, fs::Permissions::from_mode(0o755)).unwrap();
    let _guard = [
        common::EnvVar::set("APB_AGENT_CMD", &agent),
        common::EnvVar::set("CAP_DIR", stub_dir.path()),
    ];
    let (tx, rx) = mpsc::channel();
    let root = env.root.path().to_path_buf();
    std::thread::spawn(move || {
        let res = run(&root, "cap", None, opts).unwrap();
        let _ = tx.send(res);
    });
    match rx.recv_timeout(DEADLINE) {
        Ok(r) => env.root.path().join(".apb/runs").join(&r.run_id),
        Err(e) => panic!("the run did not end in time: {e}"),
    }
}

fn opts(instruction: &str) -> RunOptions {
    RunOptions {
        instruction: Some(instruction.into()),
        ..RunOptions::default()
    }
}

fn captured(
    env: &Env,
    run_dir: &Path,
    name: &str,
    nodes: &[&str],
) -> apb_engine::scenario::capture::Capture {
    let req = CaptureRequest {
        name: name.into(),
        nodes: nodes.iter().map(|s| s.to_string()).collect(),
    };
    match apb_engine::scenario::capture::capture(root_of(env), run_dir, &req) {
        Ok(c) => c,
        Err(CaptureError::NotCapturable(gaps)) => panic!(
            "not capturable: {:?}",
            gaps.iter()
                .map(|g| (g.code, &g.node, &g.detail))
                .collect::<Vec<_>>()
        ),
        Err(CaptureError::Engine(e)) => panic!(
            "engine error: {e}; dir: {:?}",
            fs::read_dir(run_dir).map(|d| d
                .flatten()
                .map(|x| x.file_name().to_string_lossy().to_string())
                .collect::<Vec<_>>())
        ),
    }
}

fn gaps_of(env: &Env, run_dir: &Path, name: &str) -> Vec<(String, Option<String>, String)> {
    let req = CaptureRequest {
        name: name.into(),
        nodes: Vec::new(),
    };
    match apb_engine::scenario::capture::capture(root_of(env), run_dir, &req) {
        Err(CaptureError::NotCapturable(gaps)) => gaps
            .into_iter()
            .map(|g| (g.code.to_string(), g.node, g.detail))
            .collect(),
        Ok(c) => panic!(
            "the capture of `{}` was NOT refused; answers {:?} visits {:?} status {:?}",
            req.name,
            c.scenario.answers.keys().collect::<Vec<_>>(),
            c.scenario.expect.visits,
            c.scenario.expect.status
        ),
        Err(CaptureError::Engine(e)) => panic!(
            "engine error: {e}; files: {:?}",
            fs::read_dir(run_dir)
                .map(|d| d
                    .flatten()
                    .map(|x| x.file_name().to_string_lossy().to_string())
                    .collect::<Vec<_>>())
                .map_err(|e| e.to_string())
        ),
    }
}

/// Copies the whole run directory tree (its snapshot included).
fn copy_run_dir(from: &Path, to: &Path) {
    fn walk(from: &Path, to: &Path) {
        fs::create_dir_all(to).unwrap();
        for e in fs::read_dir(from).unwrap().flatten() {
            let t = to.join(e.file_name());
            if e.path().is_dir() {
                walk(&e.path(), &t);
            } else {
                fs::copy(e.path(), &t).unwrap();
            }
        }
    }
    walk(from, to);
}

/// The attempts of `node` in the captured scenario, as one line each.
fn attempt_lines(c: &apb_engine::scenario::capture::Capture, node: &str) -> Vec<String> {
    c.scenario.answers[node]
        .iter()
        .flat_map(|v| v.attempts.iter())
        .map(render_attempt)
        .collect()
}

fn render_attempt(a: &AttemptAnswer) -> String {
    let mut parts: Vec<String> = Vec::new();
    if a.timed_out {
        parts.push("timed_out".into());
    }
    match a.status {
        Some(ScriptedStatus::Succeeded) => parts.push("succeeded".into()),
        Some(ScriptedStatus::Failed) => parts.push("failed".into()),
        None => {}
    }
    if let Some(k) = &a.failure_kind {
        parts.push(format!("kind:{k:?}"));
    }
    if let Some(c) = &a.check {
        parts.push(format!("check:{c:?}"));
    }
    if let Some(o) = &a.output {
        parts.push(format!("out:{o}"));
    }
    if !a.outputs.is_empty() {
        parts.push(format!(
            "outputs:{}",
            serde_json::to_string(&a.outputs).unwrap()
        ));
    }
    parts.join(" ")
}

/// A rejected attempt replays as a rejection, and the two attempts of the
/// node are one visit.
#[test]
fn a_rejected_attempt_is_a_check_reject_then_a_success_in_one_visit() {
    let _lock = common::env_lock();
    let _e = [
        common::EnvVar::set("APB_DECISIONS_ALLOW_FAKE", "1"),
        common::EnvVar::remove("APB_RUN_ID"),
    ];
    let _e = common::EnvVar::set("CAP_MODE", "reject");
    let env = project(
        &base().replace(
            "max_retries: 1",
            r#"max_retries: 1, success_check: { marker: "ALL-DONE" }"#,
        ),
        &[("gate.sh", "echo 'QA green'\n")],
    );
    let dir = drive(&env, opts("the parser"));
    let c = captured(&env, &dir, "rejected-once", &[]);
    assert_eq!(
        attempt_lines(&c, "work"),
        vec![
            "succeeded check:Reject out:not really done".to_string(),
            "succeeded out:ALL-DONE, fixed the parser".to_string(),
        ],
        "{c:#?}"
    );
    assert_eq!(c.scenario.expect.visits["work"], 1);
    assert_eq!(c.scenario.expect.status["work"], "succeeded");
    assert_eq!(
        c.scenario.expect.run.as_ref().unwrap().outcome,
        ["succeeded"]
    );
    assert_eq!(c.scenario.expect.finish.as_deref(), Some("done"));
}

/// A JSON object output replays through `outputs`, not as text.
#[test]
fn declared_output_fields_land_in_outputs() {
    let _lock = common::env_lock();
    let _e = common::EnvVar::set("CAP_MODE", "json");
    let env = project(&base(), &[]);
    let dir = drive(&env, opts("the parser"));
    let c = captured(&env, &dir, "fields", &[]);
    let a = &c.scenario.answers["work"][0].attempts[0];
    assert_eq!(
        a.status,
        Some(ScriptedStatus::Succeeded),
        "journal: {:#}",
        read_all(&dir)
            .unwrap()
            .iter()
            .map(|e| serde_json::to_string(e).unwrap())
            .collect::<Vec<_>>()
            .join("\n")
    );
    assert_eq!(
        a.outputs.get("branch").and_then(|v| v.as_str()),
        Some("feat/x"),
        "{c:#?}"
    );
    assert!(a.output.is_none(), "the object output is not also text");
}

/// A transient failure then a success.
#[test]
fn a_transient_failure_then_a_success() {
    let _lock = common::env_lock();
    let _e = common::EnvVar::set("CAP_MODE", "transient");
    let env = project(&base(), &[]);
    let dir = drive(&env, opts("the parser"));
    let c = captured(&env, &dir, "transient", &[]);
    assert_eq!(
        attempt_lines(&c, "work"),
        vec![
            "failed kind:Transient".to_string(),
            "succeeded out:ALL-DONE, fixed the parser".to_string()
        ],
    );
}

/// A timed-out attempt is captured as `{ timed_out: true }`, and the
/// `--node` filter narrows the expectations.
#[test]
fn a_timed_out_attempt_and_the_node_filter() {
    let _lock = common::env_lock();
    let _e = common::EnvVar::set("CAP_MODE", "timeout");
    let env = project(
        &base().replace("max_retries: 1", "max_retries: 0, timeout_seconds: 1"),
        &[],
    );
    let dir = drive(&env, opts("the parser"));
    let c = captured(&env, &dir, "timed-out", &["work"]);
    assert_eq!(
        attempt_lines(&c, "work"),
        vec!["timed_out".to_string()],
        "journal: {:#}",
        read_all(&dir)
            .unwrap()
            .iter()
            .filter(|e| matches!(
                e.payload,
                apb_engine::event::EventPayload::AttemptFinished { .. }
                    | apb_engine::event::EventPayload::NodeFinished { .. }
            ))
            .map(|e| serde_json::to_string(e).unwrap())
            .collect::<Vec<_>>()
            .join("\n")
    );
    assert_eq!(c.scenario.expect.status, {
        let mut m = std::collections::BTreeMap::new();
        m.insert("work".to_string(), "timed_out".to_string());
        m
    });
    assert_eq!(c.scenario.expect.visits["work"], 1);
    assert_eq!(c.scenario.expect.run.as_ref().unwrap().outcome, ["failed"]);
    // The gate never ran; the filter leaves it out of the expectations.
    assert!(!c.scenario.expect.status.contains_key("gate"));
}

/// A review reject with a note; a judge node and a judge edge by address;
/// a question answered by a person; a script failure.
#[test]
fn a_review_a_judge_and_a_question_are_captured() {
    let _lock = common::env_lock();
    let body = "schema: 2\nid: cap\nname: cap\nversion: 1.0.0\ndefaults: { profile: main }\nnodes:\n  - { id: start, type: start }\n  - { id: work, type: agent_task, prompt: \"Fix {{run.instruction}}\", max_retries: 1, interactive: true }\n  - { id: gate, type: script, script: scripts/gate.sh, runner: sh }\n  - id: triage\n    type: judge\n    state: { out: \"{{nodes.gate.output}}\" }\n    questions:\n      verdict: { type: choice, instructions: \"Is `out` green?\", criteria: { green: a, red: b } }\n    on_unavailable: fail\n  - { id: approve, type: human_review }\n  - { id: done, type: finish, outcome: success }\n  - { id: failed, type: finish, outcome: failure }\n  - { id: lintfix, type: finish, outcome: failure }\nedges:\n  - { from: start, to: work }\n  - { from: work, to: gate, condition: { type: node_status, node: work, equals: success } }\n  - { from: work, to: failed, condition: { type: node_status, node: work, equals: failure } }\n  - { from: gate, to: lintfix, condition: { type: judge, question: \"Is the output green?\", min_p: 0.9, on_unavailable: false } }\n  - { from: gate, to: triage, fallback: true }\n  - { id: again, from: triage, to: work, condition: { type: output_field, node: triage, field: verdict, equals: red }, max_traversals: 1 }\n  - { from: triage, to: approve, fallback: true }\n  - { id: rejected, from: approve, to: failed, fallback: true }\n  - { from: approve, to: done, condition: { type: review_status, equals: approve } }\n";
    let scripts = [("gate.sh", "echo 'QA green'\n")];
    let root = tempfile::tempdir().unwrap();
    init_project(root.path()).unwrap();
    let v = root.path().join(".apb/playbooks/cap/1.0.0");
    fs::create_dir_all(v.join("scripts")).unwrap();
    fs::write(v.join("playbook.yaml"), body).unwrap();
    for (name, text) in scripts {
        fs::write(v.join(format!("scripts/{name}")), text).unwrap();
    }
    fs::write(root.path().join(".apb/playbooks/cap/current"), "1.0.0").unwrap();
    common::seed_main(root.path());
    let cfg = tempfile::tempdir().unwrap();
    fs::write(
        cfg.path().join("decisions.yaml"),
        "version: 1\nmode: enforce\nproviders:\n  - id: fake\n    kind: fake\n    answers:\n      verdict: { type: choice, choice: red, probabilities: { green: 0.2, red: 0.8 }, confidence: 0.8 }\n      edge_0: { type: noul, noul: 0.1 }\nuses:\n  judge_node: { mode: enforce }\n  judge_edge: { mode: enforce }\n",
    )
    .unwrap();
    let set_cfg = common::EnvVar::set("APB_CONFIG_DIR", cfg.path());

    // The stub answers the question in its second round: it writes the
    // answer itself, as the channel would.
    let stub = tempfile::tempdir().unwrap();
    let agent = stub.path().join("agent.sh");
    fs::write(
        &agent,
        "#!/bin/sh\nif [ -f \"$CAP_DIR/asked\" ]; then echo 'fixed the parser'; else touch \"$CAP_DIR/asked\"; echo '<<<apb:question>>>'; echo '{\"question\": \"which database?\"}'; fi\n",
    )
    .unwrap();
    fs::set_permissions(&agent, fs::Permissions::from_mode(0o755)).unwrap();
    let _env = [
        set_cfg,
        common::EnvVar::set("APB_AGENT_CMD", &agent),
        common::EnvVar::set("APB_DECISIONS_ALLOW_FAKE", "1"),
        common::EnvVar::set("CAP_DIR", stub.path()),
    ];
    let (tx, rx) = mpsc::channel();
    let root2 = root.path().to_path_buf();
    std::thread::spawn(move || {
        let res = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            run(&root2, "cap", None, opts("the parser"))
        }));
        let _ = tx.send(
            res.map_err(|p| {
                apb_engine::error::EngineError::Invalid(format!(
                    "run panicked: {:?}",
                    p.downcast_ref::<String>().cloned().unwrap_or_default()
                ))
            })
            .and_then(|r| r),
        );
    });
    let run_dir = poll("the run dir", || {
        if let Ok(msg) = rx.try_recv() {
            panic!("the run ended before its directory was found: {msg:?}");
        }
        fs::read_dir(root.path().join(".apb/runs"))
            .ok()?
            .flatten()
            .map(|e| e.path())
            .find(|p| p.is_dir())
    });
    // The question: a person answers.
    poll("the question", || {
        let events = read_all(&run_dir).ok()?;
        events
            .iter()
            .any(|e| {
                matches!(
                    e.payload,
                    apb_engine::event::EventPayload::QuestionAsked { .. }
                )
            })
            .then_some(())
    });
    apb_engine::question::post_answer(&run_dir, Some("work"), "postgres", "human").unwrap();
    // The judge answers green; the review approves. (A reject of this gate
    // routes to the failed finish, so the reject-with-note case is its own
    // run below.)
    let mut offered = false;
    let mut ended: Option<
        Result<apb_engine::scheduler::RunResult, apb_engine::error::EngineError>,
    > = None;
    loop {
        let events = read_all(&run_dir).unwrap_or_default();
        let decided = events.iter().any(|e| {
            matches!(&e.payload,
                apb_engine::event::EventPayload::ReviewDecided { node, .. } if node == "approve")
        });
        if decided {
            break;
        }
        let requested = events.iter().any(|e| {
            matches!(&e.payload,
                apb_engine::event::EventPayload::ReviewRequested { node, .. } if node == "approve")
        });
        if requested && !offered {
            let _ = apb_engine::review::post_decision(
                &run_dir,
                apb_engine::review::ReviewCommand {
                    node: "approve".into(),
                    decision: "approve".into(),
                    note: String::new(),
                },
                None,
            );
            offered = true;
        }
        std::thread::sleep(STEP);
        if let Ok(msg) = rx.recv_timeout(STEP) {
            // The run ended: the decision may have landed after the last
            // read, so decide from a fresh one.
            let decided_now = read_all(&run_dir)
                .unwrap_or_default()
                .iter()
                .any(|e| matches!(&e.payload,
                    apb_engine::event::EventPayload::ReviewDecided { node, .. } if node == "approve"));
            assert!(decided_now, "the run ended with no review decision");
            ended = Some(msg);
            break;
        }
    }
    match ended.or_else(|| rx.recv_timeout(DEADLINE).ok()) {
        Some(Ok(r)) => assert_eq!(
            r.outcome,
            apb_engine::state::RunStatus::Succeeded,
            "the run failed"
        ),
        Some(Err(e)) => panic!("the run errored: {e}"),
        None => panic!("the run did not end"),
    }
    let env = Env { root, stub };

    let c = captured(&env, &run_dir, "mixed", &[]);
    // Two visits of work: the first asked the question (the round and its
    // continuation are one scripted answer), the second succeeded plain.
    assert_eq!(c.scenario.answers["work"].len(), 2, "{c:#?}");
    assert_eq!(c.scenario.answers["work"][0].attempts.len(), 1, "{c:#?}");
    let first = &c.scenario.answers["work"][0].attempts[0];
    let q = first.question.as_ref().expect("the question is captured");
    assert_eq!(q.answer.as_deref(), Some("postgres"));
    let then = first.then.as_ref().unwrap();
    assert_eq!(then.status, Some(ScriptedStatus::Succeeded));
    // The judge node's answers (two visits: red routed back once) and the
    // judge edges by address.
    assert_eq!(c.scenario.answers["triage"].len(), 2, "{c:#?}");
    for visit in &c.scenario.answers["triage"] {
        let apb_core::scenario::JudgeAnswer::Answers(map) = visit.judge.as_ref().unwrap() else {
            panic!("judge answers expected: {c:#?}");
        };
        assert_eq!(
            map["verdict"].value.as_ref().and_then(|v| v.as_str()),
            Some("red")
        );
    }
    // The judge edge of gate, by its edge address.
    let gate = &c.scenario.answers["gate"][0];
    assert!(
        !gate.judge_edges.is_empty(),
        "a judge edge answer by address: {c:#?}"
    );
    assert!(
        gate.judge_edges.values().any(|a| a.p == Some(0.1)),
        "the edge answer's probability: {c:#?}"
    );
    // The review approved.
    let reviews: Vec<&apb_core::scenario::ReviewAnswer> = c.scenario.answers["approve"]
        .iter()
        .filter_map(|v| v.review.as_ref())
        .collect();
    assert_eq!(reviews.len(), 1, "{c:#?}");
    assert_eq!(reviews[0].decision, "approve");
    assert_eq!(reviews[0].note, None);
    // The script answer.
    assert_eq!(
        c.scenario.answers["gate"][0].script.as_ref().unwrap().exit,
        0
    );
}

fn poll<T>(what: &str, mut f: impl FnMut() -> Option<T>) -> T {
    let started = Instant::now();
    loop {
        if let Some(v) = f() {
            return v;
        }
        assert!(started.elapsed() < DEADLINE, "timed out waiting for {what}");
        std::thread::sleep(STEP);
    }
}

/// A secret value and a credential-shaped token are scrubbed, and a YAML
/// injection string round-trips as a string.
#[test]
fn scrubbing_and_injection() {
    let _lock = common::env_lock();
    // A param whose name reads secret and a default it keeps: withheld
    // from the scenario params with a warning, a scrubber value in the run.
    let body = "schema: 2\nid: cap\nname: cap\nversion: 1.0.0\ndefaults: { profile: main }\nparams:\n  - { name: token, type: text, default: hunter2-secret-value }\nnodes:\n  - { id: start, type: start }\n  - { id: work, type: agent_task, prompt: \"Fix {{run.instruction}} {{params.token}}\", max_retries: 1 }\n  - { id: done, type: finish, outcome: success }\nedges:\n  - { from: start, to: work }\n  - { from: work, to: done, condition: { type: node_status, node: work, equals: success } }\n";
    let env = project(body, &[]);
    let stub = tempfile::tempdir().unwrap();
    let agent = stub.path().join("agent.sh");
    fs::write(&agent, agent_script()).unwrap();
    fs::set_permissions(&agent, fs::Permissions::from_mode(0o755)).unwrap();
    let _secret_env = [
        common::EnvVar::set("APB_AGENT_CMD", &agent),
        common::EnvVar::set("CAP_DIR", stub.path()),
        common::EnvVar::set("CAP_MODE", "secret"),
        common::EnvVar::set("CAP_SECRET", "hunter2-secret-value"),
    ];
    let mut o = opts("the parser");
    o.params
        .insert("token".into(), "hunter2-secret-value".into());
    let dir = drive(&env, o);

    let c = captured(&env, &dir, "scrubbed", &[]);
    assert!(c.redacted >= 1, "the scrubber changed something: {c:#?}");
    assert!(
        !c.yaml.contains("hunter2-secret-value"),
        "the secret reached the YAML: {}",
        c.yaml
    );
    assert!(
        !c.yaml.contains("glpat-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"),
        "the token reached the YAML"
    );
    assert!(
        c.warnings.iter().any(|w| w.contains("token")),
        "the withheld param is warned: {:?}",
        c.warnings
    );

    // A YAML injection string round-trips as a string value.
    let stub = tempfile::tempdir().unwrap();
    let agent = stub.path().join("agent.sh");
    fs::write(&agent, agent_script()).unwrap();
    fs::set_permissions(&agent, fs::Permissions::from_mode(0o755)).unwrap();
    let _e = [
        common::EnvVar::set("APB_AGENT_CMD", &agent),
        common::EnvVar::set("CAP_MODE", "inject"),
        common::EnvVar::set("CAP_DIR", stub.path()),
    ];
    let dir = drive(&env, opts("the parser"));
    let c2 = captured(&env, &dir, "injected", &[]);
    let text = c2.scenario.answers["work"][0].attempts[0]
        .output
        .clone()
        .expect("the output is text");
    assert!(text.contains("expect:\n  run:"), "{text:?}");
    assert_eq!(
        c2.scenario.expect.run.as_ref().unwrap().outcome,
        ["succeeded"]
    );
    let parsed: apb_core::scenario::Scenario =
        yaml_serde::from_str(&c2.yaml).expect("the YAML parses");
    assert_eq!(
        parsed.expect.run.as_ref().unwrap().outcome,
        ["succeeded"],
        "the injected string did not add keys"
    );
}

/// The refusals: a list node, a stopped run, a cache hit, a fatal
/// rejection, an unknown event, a torn line, a host task, a directive, a
/// scenario origin.
#[test]
fn the_refusals() {
    let _lock = common::env_lock();
    let _e = common::EnvVar::set("APB_DECISIONS_ALLOW_FAKE", "1");

    // A list node.
    let body = playbook(
        "  - { id: scan, type: agent_task, prompt: \"Scan\", list: { items: { inline: [{ id: a }] } } }\n",
        "",
        "",
    );
    let fixed = body.replace(
        "  - { from: gate, to: done, fallback: true }\n",
        "  - { from: gate, to: scan }\n  - { from: scan, to: done }\n",
    );
    let env = project(&fixed, &[("gate.sh", "echo 'QA green'\n")]);
    let dir = drive(&env, opts("the parser"));
    let codes = gaps_of(&env, &dir, "listed");
    assert!(codes.iter().any(|(c, _, _)| c == "list_node"), "{codes:?}");

    // A stopped run.
    let env = project(&base(), &[]);
    let stub_dir = tempfile::tempdir().unwrap();
    let agent = stub_dir.path().join("agent.sh");
    fs::write(&agent, "#!/bin/sh\nsleep 30\n").unwrap();
    fs::set_permissions(&agent, fs::Permissions::from_mode(0o755)).unwrap();
    let _env2 = [
        common::EnvVar::set("APB_AGENT_CMD", &agent),
        common::EnvVar::set("CAP_DIR", stub_dir.path()),
    ];
    let (tx, rx) = mpsc::channel();
    let root = env.root.path().to_path_buf();
    std::thread::spawn(move || {
        let res = run(&root, "cap", None, opts("the parser")).unwrap();
        let _ = tx.send(res);
    });
    let run_dir = poll("the run dir", || {
        fs::read_dir(env.root.path().join(".apb/runs"))
            .ok()?
            .flatten()
            .map(|e| e.path())
            .find(|p| p.is_dir())
    });
    poll("the attempt", || {
        let events = read_all(&run_dir).ok()?;
        events
            .iter()
            .any(|e| {
                matches!(
                    e.payload,
                    apb_engine::event::EventPayload::AttemptStarted { .. }
                )
            })
            .then_some(())
    });
    apb_engine::stop::stop_run(
        env.root.path(),
        run_dir.file_name().unwrap().to_str().unwrap(),
    )
    .unwrap();
    let _ = rx.recv_timeout(DEADLINE);
    let codes = gaps_of(&env, &run_dir, "stopped");
    assert!(codes.iter().any(|(c, _, _)| c == "aborted"), "{codes:?}");

    // A cache hit: the same run again, served from the node cache.
    let env = project(
        &base().replace("max_retries: 1", "max_retries: 1, cache: auto"),
        &[],
    );
    let stub_dir = tempfile::tempdir().unwrap();
    let agent = stub_dir.path().join("agent.sh");
    fs::write(&agent, "#!/bin/sh\necho 'cached work'\n").unwrap();
    fs::set_permissions(&agent, fs::Permissions::from_mode(0o755)).unwrap();
    let _env3 = [
        common::EnvVar::set("APB_AGENT_CMD", &agent),
        common::EnvVar::set("CAP_DIR", stub_dir.path()),
    ];
    // The cache key folds in the workspace fingerprint: a stable one needs
    // a git tree, so the project is committed once here.
    let git = |args: &[&str]| {
        let mut full = vec![
            "-C",
            env.root.path().to_str().unwrap(),
            "-c",
            "commit.gpgsign=false",
        ];
        full.extend_from_slice(args);
        assert!(
            std::process::Command::new("git")
                .args(&full)
                .output()
                .unwrap()
                .status
                .success()
        );
    };
    fs::write(env.root.path().join(".gitignore"), ".apb/\n").unwrap();
    git(&["init", "-q"]);
    git(&["config", "user.email", "t@t"]);
    git(&["config", "user.name", "t"]);
    git(&["add", "."]);
    git(&["commit", "-qm", "c1"]);
    let first = run(env.root.path(), "cap", None, opts("the parser")).unwrap();
    let second = run(env.root.path(), "cap", None, opts("the parser")).unwrap();
    let dir = env.root.path().join(".apb/runs").join(&second.run_id);
    println!(
        "first {} files {:?}; second {} files {:?}",
        first.run_id,
        fs::read_dir(env.root.path().join(".apb/runs").join(&first.run_id))
            .map(|d| d
                .flatten()
                .map(|x| x.file_name().to_string_lossy().to_string())
                .collect::<Vec<_>>())
            .map_err(|e| e.to_string()),
        second.run_id,
        fs::read_dir(&dir)
            .map(|d| d
                .flatten()
                .map(|x| x.file_name().to_string_lossy().to_string())
                .collect::<Vec<_>>())
            .map_err(|e| e.to_string()),
    );
    let events = read_all(&dir).unwrap();
    assert!(
        events.iter().any(|e| matches!(
            e.payload,
            apb_engine::event::EventPayload::NodeCacheHit { .. }
        )),
        "the second run was a cache hit: {events:#?}"
    );
    let codes = gaps_of(&env, &dir, "cached");
    assert!(codes.iter().any(|(c, _, _)| c == "cache_hit"), "{codes:?}");

    // A fatal check rejection, an unknown event, a torn line, a host task,
    // a directive: appended to a finished run's journal.
    let env = project(
        &base().replace(
            "max_retries: 1",
            r#"max_retries: 1, success_check: { marker: "ALL-DONE" }"#,
        ),
        &[],
    );
    let stub_dir = tempfile::tempdir().unwrap();
    let agent = stub_dir.path().join("agent.sh");
    fs::write(&agent, "#!/bin/sh\necho 'plain work'\n").unwrap();
    fs::set_permissions(&agent, fs::Permissions::from_mode(0o755)).unwrap();
    let _env4 = [
        common::EnvVar::set("APB_AGENT_CMD", &agent),
        common::EnvVar::set("CAP_DIR", stub_dir.path()),
    ];
    let _dir = drive(&env, opts("the parser"));

    let append = |dir: &Path, line: &str| {
        let mut f = fs::OpenOptions::new()
            .append(true)
            .open(dir.join("events.jsonl"))
            .unwrap();
        writeln!(f, "{line}").unwrap();
    };
    // Fatal rejection: a rejected report that ended the node, hand
    // journaled (the drive writes this shape only when the attempt also
    // violated read-only or the write scope).
    let env = project(&base(), &[]);
    let dir = env.root.path().join(".apb/runs/cap-handmade");
    fs::create_dir_all(&dir).unwrap();
    let lines = [
        r#"{"seq":1,"ts":1,"type":"run_started","playbook":"cap","version":"1.0.0"}"#,
        r#"{"seq":2,"ts":1,"type":"node_started","node":"work","attempt":1}"#,
        r#"{"seq":3,"ts":1,"type":"attempt_started","node":"work","attempt":1,"agent":"claude-code"}"#,
        r#"{"seq":4,"ts":1,"type":"attempt_finished","node":"work","attempt":1,"status":"succeeded","output":"looks done"}"#,
        r#"{"seq":5,"ts":1,"type":"attempt_finished","node":"work","attempt":2,"status":"failed","rejected_output":"the report text","duration_ms":1}"#,
        r#"{"seq":6,"ts":1,"type":"node_finished","node":"work","status":"failed","attempt":1,"output":"rejected"}"#,
        r#"{"seq":7,"ts":1,"type":"run_finished","outcome":"failed"}"#,
    ];
    fs::write(dir.join("events.jsonl"), lines.join("\n") + "\n").unwrap();
    fs::write(dir.join("playbook.yaml"), base()).unwrap();
    fs::write(
        dir.join("run.yaml"),
        "params: {}\ninstruction: the parser\nunattended: false\n",
    )
    .unwrap();
    let codes = gaps_of(&env, &dir, "fatal");
    assert!(
        codes.iter().any(|(c, _, _)| c == "fatal_rejection"),
        "{codes:?}"
    );

    // An unknown event type before a checkpoint.
    let copy2 = dir.with_extension("unknown");
    copy_run_dir(&dir, &copy2);
    append(
        &copy2,
        r#"{"seq":9002,"ts":1,"type":"alien_kind_from_the_future","detail":"x"}"#,
    );
    append(
        &copy2,
        r#"{"seq":9003,"ts":1,"type":"node_finished","node":"work","status":"succeeded","attempt":1,"output":"y"}"#,
    );
    let codes = gaps_of(&env, &copy2, "unknown");
    assert!(
        codes.iter().any(|(c, _, _)| c == "unknown_events"),
        "{codes:?}"
    );

    // A torn last line.
    let copy3 = dir.with_extension("torn");
    copy_run_dir(&dir, &copy3);
    {
        let mut f = fs::OpenOptions::new()
            .append(true)
            .open(copy3.join("events.jsonl"))
            .unwrap();
        write!(f, "{{\"seq\":9004,\"ts\":1,\"type\":\"node_finis").unwrap();
    }
    let codes = gaps_of(&env, &copy3, "torn");
    assert!(
        codes.iter().any(|(c, _, _)| c == "journal_unreadable"),
        "{codes:?}"
    );

    // A host task.
    let copy4 = dir.with_extension("host");
    copy_run_dir(&dir, &copy4);
    append(
        &copy4,
        r#"{"seq":9005,"ts":1,"type":"host_task_requested","task_id":"t1","node":"work","attempt":1}"#,
    );
    let codes = gaps_of(&env, &copy4, "host");
    assert!(codes.iter().any(|(c, _, _)| c == "host_mode"), "{codes:?}");

    // A directive that moved the run.
    let copy5 = dir.with_extension("directive");
    copy_run_dir(&dir, &copy5);
    append(
        &copy5,
        r#"{"seq":9006,"ts":1,"type":"supervisor_action","action":"node_retry","node":"work","detail":"again"}"#,
    );
    let codes = gaps_of(&env, &copy5, "directive");
    assert!(codes.iter().any(|(c, _, _)| c == "directive"), "{codes:?}");

    // A scenario origin.
    let copy6 = dir.with_extension("origin");
    copy_run_dir(&dir, &copy6);
    fs::write(copy6.join("manifest.yaml"), "origin: scenario\n").unwrap();
    let codes = gaps_of(&env, &copy6, "origin");
    assert!(
        codes.iter().any(|(c, _, _)| c == "not_production"),
        "{codes:?}"
    );
}

/// Writes a hand-journaled run of `body` under `name` and returns its dir.
fn handmade(env: &Env, name: &str, body: &str, lines: &[&str]) -> PathBuf {
    let dir = env.root.path().join(".apb/runs").join(name);
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("events.jsonl"), lines.join("\n") + "\n").unwrap();
    fs::write(dir.join("playbook.yaml"), body).unwrap();
    fs::write(
        dir.join("run.yaml"),
        "params: {}\ninstruction: the parser\nunattended: false\n",
    )
    .unwrap();
    dir
}

/// The engine journals a read-only violation before its attempt's
/// `attempt_finished`: the capture puts it on that attempt, the first one
/// here, not on the attempt that finished before it.
#[test]
fn a_read_only_violation_lands_on_its_own_attempt() {
    let body = playbook("", ", read_only: true", "");
    let env = project(&body, &[]);
    let dir = handmade(
        &env,
        "cap-read-only",
        &body,
        &[
            r#"{"seq":1,"ts":1,"type":"run_started","playbook":"cap","version":"1.0.0"}"#,
            r#"{"seq":2,"ts":1,"type":"node_started","node":"start","attempt":1}"#,
            r#"{"seq":3,"ts":1,"type":"node_finished","node":"start","status":"succeeded","attempt":1,"output":""}"#,
            r#"{"seq":4,"ts":1,"type":"node_started","node":"work","attempt":1}"#,
            r#"{"seq":5,"ts":1,"type":"attempt_started","node":"work","attempt":1,"agent":"claude-code"}"#,
            r#"{"seq":6,"ts":1,"type":"read_only_violated","node":"work","attempt":1,"tier":"detected","changes":[{"path":"src/a.rs","change":"modified"}]}"#,
            r#"{"seq":7,"ts":1,"type":"attempt_finished","node":"work","attempt":1,"status":"failed","duration_ms":1}"#,
            r#"{"seq":8,"ts":1,"type":"attempt_started","node":"work","attempt":2,"agent":"claude-code"}"#,
            r#"{"seq":9,"ts":1,"type":"attempt_finished","node":"work","attempt":2,"status":"succeeded","duration_ms":1}"#,
            r#"{"seq":10,"ts":1,"type":"node_finished","node":"work","status":"succeeded","attempt":2,"output":"done"}"#,
            r#"{"seq":11,"ts":1,"type":"node_started","node":"gate","attempt":1}"#,
            r#"{"seq":12,"ts":1,"type":"node_finished","node":"gate","status":"succeeded","attempt":1,"output":"ok"}"#,
            r#"{"seq":13,"ts":1,"type":"node_started","node":"done","attempt":1}"#,
            r#"{"seq":14,"ts":1,"type":"node_finished","node":"done","status":"succeeded","attempt":1,"output":""}"#,
            r#"{"seq":15,"ts":1,"type":"run_finished","outcome":"succeeded"}"#,
        ],
    );
    let c = captured(&env, &dir, "read-only", &[]);
    let work = &c.scenario.answers["work"];
    let attempts = &work[0].attempts;
    assert_eq!(attempts.len(), 2, "{attempts:?}");
    assert_eq!(
        attempts[0].read_only.as_ref().map(|r| r.dirty.clone()),
        Some(vec!["src/a.rs".to_string()]),
        "{attempts:?}"
    );
    assert!(attempts[1].read_only.is_none(), "{attempts:?}");
}

/// A live agent asks and gets its answer inside one running attempt; the
/// format scripts only a question that ends its attempt, so the capture is
/// refused rather than replaying a round that never happened.
#[test]
fn a_live_question_inside_an_attempt_is_a_gap() {
    let body = playbook("", ", interactive: true", "");
    let env = project(&body, &[]);
    let dir = handmade(
        &env,
        "cap-live",
        &body,
        &[
            r#"{"seq":1,"ts":1,"type":"run_started","playbook":"cap","version":"1.0.0"}"#,
            r#"{"seq":2,"ts":1,"type":"node_started","node":"work","attempt":1}"#,
            r#"{"seq":3,"ts":1,"type":"attempt_started","node":"work","attempt":1,"agent":"claude-code"}"#,
            r#"{"seq":4,"ts":1,"type":"question_asked","node":"work","question":"which database?"}"#,
            r#"{"seq":5,"ts":1,"type":"question_answered","node":"work","answer":"postgres","answered_by":"human"}"#,
            r#"{"seq":6,"ts":1,"type":"attempt_finished","node":"work","attempt":1,"status":"succeeded","duration_ms":1}"#,
            r#"{"seq":7,"ts":1,"type":"node_finished","node":"work","status":"succeeded","attempt":1,"output":"done"}"#,
            r#"{"seq":8,"ts":1,"type":"run_finished","outcome":"succeeded"}"#,
        ],
    );
    let codes = gaps_of(&env, &dir, "live");
    assert!(
        codes
            .iter()
            .any(|(c, n, _)| c == "live_question" && n.as_deref() == Some("work")),
        "{codes:?}"
    );
}

/// A review note goes through the scrubber like every captured text: a
/// credential-shaped token in it is redacted in the preview instead of
/// refusing the whole capture.
#[test]
fn a_review_note_is_scrubbed() {
    let body = "schema: 2\nid: cap\nname: cap\nversion: 1.0.0\ndefaults: { profile: main }\nnodes:\n  - { id: start, type: start }\n  - { id: approve, type: human_review }\n  - { id: done, type: finish, outcome: success }\n  - { id: failed, type: finish, outcome: failure }\nedges:\n  - { from: start, to: approve }\n  - { from: approve, to: done, condition: { type: review_status, equals: approve } }\n  - { from: approve, to: failed, fallback: true }\n";
    let env = project(body, &[]);
    let token = "glpat-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    let decided = format!(
        r#"{{"seq":5,"ts":1,"type":"review_decided","node":"approve","decision":"approve","note":"ship it, the key was {token}"}}"#
    );
    let dir = handmade(
        &env,
        "cap-note",
        body,
        &[
            r#"{"seq":1,"ts":1,"type":"run_started","playbook":"cap","version":"1.0.0"}"#,
            r#"{"seq":2,"ts":1,"type":"node_started","node":"start","attempt":1}"#,
            r#"{"seq":3,"ts":1,"type":"node_finished","node":"start","status":"succeeded","attempt":1,"output":""}"#,
            r#"{"seq":4,"ts":1,"type":"node_started","node":"approve","attempt":1}"#,
            &decided,
            r#"{"seq":6,"ts":1,"type":"node_finished","node":"approve","status":"succeeded","attempt":1,"output":"approve"}"#,
            r#"{"seq":7,"ts":1,"type":"node_started","node":"done","attempt":1}"#,
            r#"{"seq":8,"ts":1,"type":"node_finished","node":"done","status":"succeeded","attempt":1,"output":""}"#,
            r#"{"seq":9,"ts":1,"type":"run_finished","outcome":"succeeded"}"#,
        ],
    );
    let c = captured(&env, &dir, "note", &[]);
    assert!(
        !c.yaml.contains(token),
        "the token reached the YAML: {}",
        c.yaml
    );
    let note = c.scenario.answers["approve"][0]
        .review
        .as_ref()
        .and_then(|r| r.note.clone())
        .expect("the note is captured");
    assert!(note.starts_with("ship it"), "{note}");
    assert!(c.redacted >= 1);
}
