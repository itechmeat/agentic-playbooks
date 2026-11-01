//! The secret values of a run (design: "secret values of a run"): what a
//! decision provider, a grader and the known-problems store receive from a
//! run whose `message` param is declared secret, and that the run's secret
//! set only grows. Routing reads the executor's raw output, never a
//! redacted copy.
//!
//! The value is an ordinary phrase on purpose: nothing here may depend on a
//! credential shape, only on the declaration.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::Duration;

use apb_core::registry::init_project;
use apb_core::versioning::create_patch_version;
use apb_decide::testing::{StubResponse, StubServer};
use apb_engine::control::Control;
use apb_engine::event::{EventPayload, read_all};
use apb_engine::run_redact::RunRedactor;
use apb_engine::scheduler::{
    RunMode, RunOptions, RunResult, drive_prepared, post_supervisor_command,
    prepare_supervised_background, run,
};
use apb_engine::state::RunStatus;
use serde_json::{Value, json};

use crate::common;

/// The declared secret value every test echoes.
const SECRET: &str = "violet hedgehog";

fn params() -> std::collections::BTreeMap<String, String> {
    [("message".to_string(), SECRET.to_string())].into()
}

fn options() -> RunOptions {
    RunOptions {
        params: params(),
        ..Default::default()
    }
}

/// The `params:` block declaring `message` secret.
const SECRET_PARAM: &str = "params:\n  - { name: message, type: text, secret: true }\n";

fn seed_definition(root: &Path, id: &str, yaml: &str) {
    let dir = root.join(".apb/playbooks").join(id).join("1.0.0");
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("playbook.yaml"), yaml).unwrap();
    fs::write(
        root.join(".apb/playbooks").join(id).join("current"),
        "1.0.0",
    )
    .unwrap();
}

fn write_agent(root: &Path, body: &str) -> PathBuf {
    let path = root.join("agent.sh");
    common::write_sync(&path, &format!("#!/bin/sh\n{body}\n"));
    let mut perm = fs::metadata(&path).unwrap().permissions();
    perm.set_mode(0o755);
    fs::set_permissions(&path, perm).unwrap();
    path
}

/// Every text file under `dir`, recursively.
fn every_file_text(dir: &Path) -> Vec<(PathBuf, String)> {
    let mut out = Vec::new();
    let Ok(entries) = fs::read_dir(dir) else {
        return out;
    };
    for e in entries.flatten() {
        let path = e.path();
        if path.is_dir() {
            out.extend(every_file_text(&path));
        } else if let Ok(text) = fs::read_to_string(&path) {
            out.push((path, text));
        }
    }
    out
}

// --- decision requests ------------------------------------------------------

fn completion_reply() -> StubResponse {
    let probs: serde_json::Map<String, Value> = [
        "complete",
        "partial",
        "not_started",
        "blocked_on_input",
        "unclear",
    ]
    .iter()
    .map(|o| {
        (
            o.to_string(),
            json!(if *o == "complete" { 0.8 } else { 0.05 }),
        )
    })
    .collect();
    StubResponse::json(
        200,
        json!({
            "model": "jev-1.13.0",
            "answers": {
                "final_result": {"type": "noul", "noul": 0.95},
                "completion": {"type": "choice", "choice": "complete", "probabilities": probs, "confidence": 0.75},
            },
            "usage": {"input_tokens": 500, "output_tokens": 2}
        })
        .to_string(),
    )
}

/// A completion check whose state holds the echoed value and a path: the
/// provider never receives the value, whatever `privacy.redact` says, and
/// with redaction off the path still arrives as written (the run values
/// are removed, nothing else).
#[test]
fn a_completion_check_never_sends_the_declared_value_whatever_privacy_redact_says() {
    for redact in [false, true] {
        let root = tempfile::tempdir().unwrap();
        let cfg = tempfile::tempdir().unwrap();
        init_project(root.path()).unwrap();
        common::seed_main(root.path());
        seed_definition(
            root.path(),
            "d",
            &format!(
                "schema: 2\nid: d\nname: Decisions\nversion: 1.0.0\n{SECRET_PARAM}defaults:\n  profile: main\nnodes:\n  - {{ id: start, type: start }}\n  - {{ id: w, type: agent_task, prompt: \"Write the note\" }}\n  - {{ id: ok, type: finish, outcome: success }}\nedges:\n  - {{ from: start, to: w }}\n  - {{ from: w, to: ok }}\n"
            ),
        );
        let marker = root.path().join("notes-dir/notes.txt");
        let agent = write_agent(
            root.path(),
            &format!("echo 'Wrote {SECRET} into {}'", marker.display()),
        );
        let server = StubServer::start_with_fallback(vec![], completion_reply());
        fs::write(
            cfg.path().join("decisions.yaml"),
            format!(
                "version: 1\nmode: advise\nprivacy: {{ send: [prompts, outputs], redact: {redact} }}\nproviders:\n  - {{ id: stub, kind: systemone, base_url: \"{}\", model: jev-1.13.0 }}\nuses:\n  completion_check: {{ mode: advise }}\n",
                server.base_url
            ),
        )
        .unwrap();
        let _lock = common::env_lock();
        let _cfg = common::EnvVar::set("APB_CONFIG_DIR", cfg.path());
        let _agent = common::EnvVar::set("APB_AGENT_CMD", &agent);
        let res = run(root.path(), "d", None, options()).unwrap();
        assert_eq!(res.outcome, RunStatus::Succeeded);
        let requests = server.requests();
        assert!(!requests.is_empty(), "the completion check asked nothing");
        assert!(
            requests.iter().all(|r| !r.contains(SECRET)),
            "the declared value reached the provider (redact: {redact})"
        );
        // The value was in the state: its place is marked.
        assert!(
            requests.iter().any(|r| r.contains("[redacted]")),
            "the echo did not reach the state (redact: {redact})"
        );
        if !redact {
            assert!(
                requests
                    .iter()
                    .any(|r| r.contains(&marker.display().to_string())),
                "with redaction off a path arrives as written"
            );
        }
    }
}

// --- grading ------------------------------------------------------------------

/// A finished one-node run whose output and transcript echo the value, with
/// the playbook's own criterion `reply-tone` over outputs and transcripts,
/// graded by `grader`.
fn graded_run(root: &Path, grader: &str) -> PathBuf {
    let journal =
        crate::quality_common::agent_journal("claude", &root.join("work"), "", "succeeded")
            .replace(
                r#""output":"done""#,
                &format!(r#""output":"The reply to {SECRET} was polite""#),
            );
    let snapshot = format!(
        "schema: 2\nid: fix-test\nname: Fix test\nversion: 1.0.0\n{SECRET_PARAM}quality:\n  criteria_grader: {grader}\n  criteria:\n    - id: reply-tone\n      evidence: both\n      reads: [outputs, transcripts]\n      levels: {{ good: \"every reply is polite\", partial: \"one reply is curt\", poor: \"a reply is rude\" }}\nnodes:\n  - {{ id: start, type: start }}\n  - {{ id: fix, type: agent_task, prompt: fix it, profile: coder }}\n  - {{ id: done, type: finish, outcome: success }}\nedges:\n  - {{ from: start, to: fix }}\n  - {{ from: fix, to: done }}\n"
    );
    let dir = crate::quality_common::run_dir(root, "r1", &journal, &snapshot);
    apb_engine::run_config::write_run_config(
        &dir,
        &apb_engine::run_config::RunConfig {
            params: params(),
            ..Default::default()
        },
    )
    .unwrap();
    crate::quality_common::attempt_file(&dir, "fix-1", "stdout.log", &format!("{SECRET}\n"));
    crate::quality_common::attempt_file(&dir, "fix-1", "stderr.log", &format!("{SECRET}\n"));
    dir
}

/// A host grading session: the start and every view, read in full, carry
/// no value of the run.
#[test]
fn a_host_grading_session_never_shows_the_declared_value() {
    use apb_engine::grade::reader::ReadRequest;
    use apb_engine::grade::{read, start_session};
    let tmp = tempfile::tempdir().unwrap();
    let dir = graded_run(tmp.path(), "host");
    let started = start_session(&dir, None).unwrap();
    assert!(
        !format!("{started:?}").contains(SECRET),
        "the value leaked into the session start"
    );
    let s = started.session.clone();
    let outline = read(&dir, &s, &ReadRequest::Outline).unwrap();
    let mut texts: Vec<String> = outline.items.iter().map(|i| i.text.clone()).collect();
    // An outline entry names a view as `n:<view>`.
    let views: Vec<String> = outline
        .items
        .iter()
        .map(|i| i.r#ref.strip_prefix("n:").unwrap_or(&i.r#ref).to_string())
        .collect();
    assert!(!views.is_empty());
    for view in views {
        for req in [
            ReadRequest::Page {
                view: view.clone(),
                from: 0,
                count: None,
            },
            ReadRequest::Children {
                view: view.clone(),
                from: 0,
                count: None,
            },
        ] {
            if let Ok(r) = read(&dir, &s, &req) {
                for item in &r.items {
                    texts.push(item.text.clone());
                    if let Ok(whole) = read(
                        &dir,
                        &s,
                        &ReadRequest::Read {
                            r#ref: item.r#ref.clone(),
                            span: None,
                        },
                    ) {
                        texts.extend(whole.items.into_iter().map(|i| i.text));
                    }
                }
            }
        }
    }
    if let Ok(hits) = read(
        &dir,
        &s,
        &ReadRequest::Search {
            query: "hedgehog".into(),
            view: None,
        },
    ) {
        texts.extend(hits.items.into_iter().map(|i| i.text));
    }
    assert!(
        texts.iter().all(|t| !t.contains(SECRET)),
        "the value leaked into a grading view"
    );
    assert!(
        texts.iter().any(|t| t.contains("[redacted]")),
        "no view read the echoed output"
    );
}

/// The model grader's request to the provider carries no value of the run,
/// with `privacy.redact` off.
#[test]
fn the_model_grader_never_sends_the_declared_value() {
    let _l = common::env_lock();
    let tmp = tempfile::tempdir().unwrap();
    let cfg = tempfile::tempdir().unwrap();
    fs::create_dir_all(tmp.path().join(".apb")).unwrap();
    let _env = common::EnvVar::set("APB_CONFIG_DIR", cfg.path());
    let choice = |v: &str| json!({"type": "choice", "choice": v, "probabilities": { v: 1.0 }, "confidence": 0.9});
    let server = StubServer::start_with_fallback(
        vec![],
        StubResponse::json(
            200,
            json!({
                "model": "jev-1.13.0",
                "answers": {
                    "level": choice("good"),
                    "support_good": choice("none"),
                    "support_partial": choice("none"),
                    "support_poor": choice("none"),
                },
                "usage": {"input_tokens": 400, "output_tokens": 4}
            })
            .to_string(),
        ),
    );
    fs::write(
        cfg.path().join("decisions.yaml"),
        format!(
            "version: 1\nmode: enforce\nprivacy: {{ send: [prompts, outputs], redact: false }}\nproviders:\n  - {{ id: stub, kind: systemone, base_url: \"{}\", model: jev-1.13.0 }}\nuses:\n  grade: {{ mode: enforce }}\n",
            server.base_url
        ),
    )
    .unwrap();
    let dir = graded_run(tmp.path(), "model");
    let out = apb_engine::grade::grade_model(&dir).unwrap();
    assert!(
        matches!(out, apb_engine::grade::ModelGraded::Graded(_)),
        "the model grader did not grade: {out:?}"
    );
    let requests = server.requests();
    assert!(!requests.is_empty());
    assert!(
        requests.iter().all(|r| !r.contains(SECRET)),
        "the value reached the model grader's provider"
    );
    assert!(
        requests.iter().any(|r| r.contains("[redacted]")),
        "the echoed output was not sent"
    );
    for (_, body) in every_file_text(&dir.join("grades")) {
        assert!(
            !body.contains(SECRET),
            "the value leaked into a stored grade"
        );
    }
}

// --- known problems -----------------------------------------------------------

/// A run that saw tool problems echoing the value: the store and a later
/// read of the playbook's problems carry the redacted text, never the value.
#[test]
fn known_problems_store_and_show_no_declared_value() {
    let root = tempfile::tempdir().unwrap();
    init_project(root.path()).unwrap();
    let dir = root.path().join(".apb/runs/kp-1");
    fs::create_dir_all(&dir).unwrap();
    let journal = include_str!("../fixtures/known_problems/events.jsonl")
        .replace("PLANTED_VALUE", "violet-token")
        .replace(
            "an off-by-one in the parser",
            &format!("the parser drops {SECRET}"),
        )
        .replace(
            "build.sh passes -Werror=deprecated",
            &format!("build.sh prints {SECRET} and passes -Werror=deprecated"),
        );
    fs::write(dir.join("events.jsonl"), journal).unwrap();
    fs::write(
        dir.join("playbook.yaml"),
        format!(
            "schema: 2\nid: fix-build\nname: Fix build\nversion: 1.0.0\n{SECRET_PARAM}nodes:\n  - {{ id: start, type: start }}\n  - {{ id: fix, type: agent_task, prompt: fix }}\n  - {{ id: done, type: finish, outcome: success }}\nedges:\n  - {{ from: start, to: fix }}\n  - {{ from: fix, to: done }}\n"
        ),
    )
    .unwrap();
    apb_engine::run_config::write_run_config(
        &dir,
        &apb_engine::run_config::RunConfig {
            params: params(),
            ..Default::default()
        },
    )
    .unwrap();
    assert!(apb_engine::known_problems::observe(root.path(), &dir).unwrap() > 0);
    let stored = every_file_text(&root.path().join(".apb/problems"));
    assert!(!stored.is_empty(), "nothing was stored");
    for (_, body) in &stored {
        assert!(!body.contains(SECRET), "the value leaked into the store");
    }
    assert!(
        stored.iter().any(|(_, b)| b.contains("[redacted]")),
        "the echoing sighting was not kept in its redacted form"
    );
    let view = apb_engine::known_problems::view_current(
        root.path(),
        apb_core::scope::Scope::Project,
        "fix-build",
    )
    .unwrap();
    let shown = serde_json::to_string(&view).unwrap();
    assert!(
        !shown.contains(SECRET),
        "the value leaked into a later read"
    );
}

// --- the run's secret set only grows ------------------------------------------

const MONO: &str = r#"
schema: 1
id: mono
name: Mono
version: 1.0.0
params:
  - { name: message, type: text, secret: true }
nodes:
  - { id: start, type: start }
  - { id: p1, type: prompt, prompt: "first" }
  - { id: p2, type: prompt, prompt: "said {{params.message}}" }
  - { id: done, type: finish, outcome: success }
edges:
  - { from: start, to: p1 }
  - { from: p1, to: p2 }
  - { from: p2, to: done }
"#;

fn drive_in_background(
    root: PathBuf,
    prepared: apb_engine::scheduler::PreparedRun,
) -> mpsc::Receiver<Result<RunResult, apb_engine::EngineError>> {
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(drive_prepared(&root, prepared));
    });
    rx
}

/// A redactor built now, as every surface builds one per answer.
fn fresh(root: &Path, run_dir: &Path) -> RunRedactor {
    RunRedactor::for_run(root, run_dir)
}

/// A current-run patch removes `secret: true`: the manifest keeps the
/// classification, a fresh redactor still redacts the value, the output
/// the run produced after the patch is redacted, and a child run started
/// after it (the value in a plain child param) redacts its parent's value.
#[test]
fn a_patch_that_drops_a_secret_declaration_never_drops_the_redaction() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    init_project(root).unwrap();
    seed_definition(root, "mono", MONO);
    let prepared = prepare_supervised_background(
        root,
        "mono",
        None,
        RunOptions {
            mode: RunMode::Supervised,
            ..options()
        },
    )
    .unwrap();
    let run_id = prepared.run_id().to_string();
    let run_dir = root.join(".apb/runs").join(&run_id);
    let manifest = apb_engine::manifest::read(&run_dir).unwrap().unwrap();
    assert_eq!(manifest.secret_params, vec!["message".to_string()]);

    let undeclared = MONO.replace(
        "{ name: message, type: text, secret: true }",
        "{ name: message, type: text }",
    );
    let version =
        create_patch_version(root, "mono", "1.0.0", &undeclared, &run_id, "workaround").unwrap();
    post_supervisor_command(
        root,
        &run_id,
        Control::Patch {
            version,
            classification: "workaround".into(),
            continue_from: "p1".into(),
        },
    )
    .unwrap();
    let result = drive_in_background(root.to_path_buf(), prepared)
        .recv_timeout(Duration::from_secs(120))
        .expect("the drive did not finish")
        .unwrap();
    assert_eq!(result.outcome, RunStatus::Succeeded);
    let events = read_all(&run_dir).unwrap();
    assert!(
        events
            .iter()
            .any(|e| matches!(e.payload, EventPayload::PatchApplied { .. })),
        "the patch was not applied"
    );
    // The snapshot no longer declares it; the manifest still does.
    let snapshot = fs::read_to_string(run_dir.join("playbook.yaml")).unwrap();
    assert!(!snapshot.contains("secret: true"));
    let manifest = apb_engine::manifest::read(&run_dir).unwrap().unwrap();
    assert_eq!(manifest.secret_params, vec!["message".to_string()]);

    // The output after the patch holds the value as the executor produced
    // it; a fresh redactor removes it.
    let state = apb_engine::state::RunState::fold(&events);
    let p2 = state.outputs.get("p2").cloned().unwrap_or_default();
    assert!(
        p2.contains(SECRET),
        "the journal keeps the executor's output"
    );
    let mut outputs = serde_json::to_value(&state.outputs).unwrap();
    let r = fresh(root, &run_dir);
    r.redact_json(&mut outputs);
    assert!(
        !outputs.to_string().contains(SECRET),
        "a later output was not redacted"
    );
    let (shown, names) = r.params(&params());
    assert_eq!(shown["message"], apb_core::run_params::REDACTED);
    assert_eq!(names, vec!["message".to_string()]);

    // A child started after the patch, the value in a plain param.
    let child = root.join(".apb/runs/child-1");
    fs::create_dir_all(&child).unwrap();
    apb_engine::run_config::write_run_config(
        &child,
        &apb_engine::run_config::RunConfig {
            params: [("note".to_string(), SECRET.to_string())].into(),
            parent_run: Some(run_id.clone()),
            ..Default::default()
        },
    )
    .unwrap();
    fs::write(
        child.join("playbook.yaml"),
        "schema: 2\nid: kid\nname: Kid\nversion: 1.0.0\nparams:\n  - { name: note, type: text }\nnodes: []\nedges: []\n",
    )
    .unwrap();
    let c = fresh(root, &child);
    assert!(
        !c.redact_str(&format!("child says {SECRET}"))
            .contains(SECRET),
        "the child does not redact its parent's value"
    );
    let (shown, _) = c.params(&[("note".to_string(), "garden".to_string())].into());
    assert_eq!(shown["note"], "garden", "a child param is not made secret");
}

/// A run started by an older apb has no `secret_params`: its snapshot's
/// declarations decide.
#[test]
fn a_run_without_secret_params_in_its_manifest_uses_its_snapshot() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join(".apb/runs/old-1");
    fs::create_dir_all(&dir).unwrap();
    apb_engine::run_config::write_run_config(
        &dir,
        &apb_engine::run_config::RunConfig {
            params: params(),
            ..Default::default()
        },
    )
    .unwrap();
    fs::write(
        dir.join("playbook.yaml"),
        format!(
            "schema: 1\nid: old\nname: Old\nversion: 1.0.0\n{SECRET_PARAM}nodes: []\nedges: []\n"
        ),
    )
    .unwrap();
    assert!(apb_engine::manifest::read(&dir).unwrap().is_none());
    let r = fresh(tmp.path(), &dir);
    assert!(!r.redact_text(&format!("x {SECRET}")).contains(SECRET));
}

// --- routing --------------------------------------------------------------------

/// An edge that matches the value in an output routes exactly as it would
/// without redaction: the engine reads the executor's output.
#[test]
fn routing_on_an_output_that_holds_the_value_is_unchanged() {
    let tmp = tempfile::tempdir().unwrap();
    init_project(tmp.path()).unwrap();
    seed_definition(
        tmp.path(),
        "route",
        &format!(
            "schema: 1\nid: route\nname: Route\nversion: 1.0.0\n{SECRET_PARAM}nodes:\n  - {{ id: start, type: start }}\n  - {{ id: p, type: prompt, prompt: \"said {{{{params.message}}}}\" }}\n  - {{ id: ok, type: finish, outcome: success }}\n  - {{ id: no, type: finish, outcome: failure }}\nedges:\n  - {{ from: start, to: p }}\n  - {{ from: p, to: ok, condition: {{ type: output_match, node: p, pattern: \"{SECRET}\" }} }}\n  - {{ from: p, to: no, fallback: true }}\n"
        ),
    );
    let res = run(tmp.path(), "route", None, options()).unwrap();
    assert_eq!(
        res.outcome,
        RunStatus::Succeeded,
        "the match edge was not taken"
    );
}
