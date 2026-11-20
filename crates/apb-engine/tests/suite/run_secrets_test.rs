//! The secret values of a run (design: "secret values of a run"): what a
//! decision provider, a grader and the known-problems store receive from a
//! run whose `message` param is declared secret, and that the run's secret
//! set only grows. Routing reads the executor's raw output, never a
//! redacted copy.
//!
//! The value is an ordinary phrase on purpose: nothing here may depend on a
//! credential shape, only on the declaration.
//!
//! A secret param is never rendered (V340): a definition that references
//! one is refused at start, at a patch, at resume and as a candidate, a
//! param once secret in a run stays secret for it, and the value lives in
//! the run's `run.yaml` only, where a step reads it.

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
use apb_engine::pause::{PauseBy, PauseRequest};
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
    seed_definition_with(root, id, yaml, &[]);
}

/// [`seed_definition`] with script files written under the version's
/// directory (a script node names them `scripts/<file>`).
fn seed_definition_with(root: &Path, id: &str, yaml: &str, scripts: &[(&str, &str)]) {
    store_version(root, id, "1.0.0", yaml, scripts);
    fs::write(
        root.join(".apb/playbooks").join(id).join("current"),
        "1.0.0",
    )
    .unwrap();
}

/// Writes a version straight into the store, as a version stored before
/// this apb or edited on disk would be: nothing validates it.
fn store_version(root: &Path, id: &str, version: &str, yaml: &str, scripts: &[(&str, &str)]) {
    let dir = root.join(".apb/playbooks").join(id).join(version);
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("playbook.yaml"), yaml).unwrap();
    for (name, body) in scripts {
        let path = dir.join(name);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, body).unwrap();
    }
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

/// Every entry under `dir` whose name or bytes hold `needle`, recursively
/// and without following a link: only regular files are read, bytes are
/// compared (a file that is not UTF-8 is searched too), and an entry that
/// cannot be inspected fails the test rather than hiding a copy. A `dir`
/// that does not exist holds nothing.
fn files_holding(dir: &Path, needle: &str) -> Vec<PathBuf> {
    let bytes = needle.as_bytes();
    let mut out = Vec::new();
    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return out,
        Err(e) => panic!("cannot list {}: {e}", dir.display()),
    };
    for e in entries {
        let path = e
            .unwrap_or_else(|e| panic!("cannot list {}: {e}", dir.display()))
            .path();
        let meta = fs::symlink_metadata(&path)
            .unwrap_or_else(|e| panic!("cannot inspect {}: {e}", path.display()));
        let named = path.to_string_lossy().contains(needle);
        if named {
            out.push(path.clone());
        }
        if meta.is_dir() {
            out.extend(files_holding(&path, needle));
        } else if meta.is_file() && !named {
            let content =
                fs::read(&path).unwrap_or_else(|e| panic!("cannot read {}: {e}", path.display()));
            if content.windows(bytes.len()).any(|w| w == bytes) {
                out.push(path);
            }
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
    let leaked = files_holding(&dir.join("grades"), SECRET);
    assert!(
        leaked.is_empty(),
        "the value leaked into a stored grade: {leaked:?}"
    );
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
  - { id: p2, type: prompt, prompt: "second" }
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

/// A supervised run of `id` prepared with `opts`, `controls` posted before
/// its drive, driven to its end.
fn supervised(
    root: &Path,
    id: &str,
    opts: RunOptions,
    controls: Vec<Control>,
) -> (String, RunResult) {
    let prepared = prepare_supervised_background(
        root,
        id,
        None,
        RunOptions {
            mode: RunMode::Supervised,
            ..opts
        },
    )
    .unwrap();
    let run_id = prepared.run_id().to_string();
    for c in controls {
        post_supervisor_command(root, &run_id, c).unwrap();
    }
    let result = drive_in_background(root.to_path_buf(), prepared)
        .recv_timeout(Duration::from_secs(120))
        .expect("the drive did not finish")
        .unwrap();
    (run_id, result)
}

fn patch(version: &str) -> Control {
    Control::Patch {
        version: version.into(),
        classification: "improvement".into(),
        continue_from: "p1".into(),
    }
}

fn pause() -> Control {
    Control::Pause(PauseRequest {
        by: Some(PauseBy::Operator),
        ..PauseRequest::default()
    })
}

/// The reason of the run's one `patch_rejected`.
fn rejection(run_dir: &Path) -> String {
    let reasons: Vec<String> = read_all(run_dir)
        .unwrap()
        .into_iter()
        .filter_map(|e| match e.payload {
            EventPayload::PatchRejected { reason } => Some(reason),
            _ => None,
        })
        .collect();
    assert_eq!(reasons.len(), 1, "{reasons:?}");
    reasons[0].clone()
}

fn applied(run_dir: &Path) -> bool {
    read_all(run_dir)
        .unwrap()
        .iter()
        .any(|e| matches!(e.payload, EventPayload::PatchApplied { .. }))
}

fn manifest_secrets(run_dir: &Path) -> Vec<String> {
    apb_engine::manifest::read(run_dir)
        .unwrap()
        .map(|m| m.secret_params)
        .unwrap_or_default()
}

/// The journal as text, for a failure message.
fn journal_text(run_dir: &Path) -> String {
    fs::read_to_string(run_dir.join("events.jsonl")).unwrap_or_default()
}

/// Every entry under `dir` with its bytes, for "nothing was written": a
/// link (dangling or not) is recorded by its target, never followed.
fn tree(dir: &Path) -> std::collections::BTreeMap<PathBuf, Vec<u8>> {
    let mut out = std::collections::BTreeMap::new();
    let Ok(entries) = fs::read_dir(dir) else {
        return out;
    };
    for e in entries.flatten() {
        let path = e.path();
        let Ok(meta) = fs::symlink_metadata(&path) else {
            continue;
        };
        if meta.file_type().is_symlink() {
            let target = fs::read_link(&path).unwrap();
            let mut entry = b"link:".to_vec();
            entry.extend_from_slice(target.as_os_str().as_encoded_bytes());
            out.insert(path, entry);
        } else if meta.is_dir() {
            out.extend(tree(&path));
        } else if let Ok(bytes) = fs::read(&path) {
            out.insert(path, bytes);
        }
    }
    out
}

/// Drops `name` from the manifest's `secret_params`, as a crash between a
/// patch's new snapshot and its second manifest write leaves it.
fn forget_in_manifest(run_dir: &Path, name: &str) {
    let path = run_dir.join("manifest.yaml");
    let mut m: yaml_serde::Value =
        yaml_serde::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
    if let Some(list) = m
        .get_mut("secret_params")
        .and_then(yaml_serde::Value::as_sequence_mut)
    {
        list.retain(|v| v.as_str() != Some(name));
    }
    fs::write(&path, yaml_serde::to_string(&m).unwrap()).unwrap();
}

/// D7 fixtures item 1 (a), D10 test 11 (a): a current-run patch that
/// removes `secret: true` is refused; the snapshot and the manifest keep
/// the declaration, and the drive still ends.
#[test]
fn a_patch_that_drops_a_secret_declaration_is_refused() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    init_project(root).unwrap();
    seed_definition(root, "mono", MONO);
    let undeclared = MONO.replace(
        "{ name: message, type: text, secret: true }",
        "{ name: message, type: text }",
    );
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
    assert_eq!(manifest_secrets(&run_dir), vec!["message".to_string()]);
    let before = fs::read(run_dir.join("playbook.yaml")).unwrap();
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
    assert!(
        rejection(&run_dir).starts_with("secret_declaration_dropped: "),
        "{}",
        rejection(&run_dir)
    );
    assert!(!applied(&run_dir));
    assert_eq!(fs::read(run_dir.join("playbook.yaml")).unwrap(), before);
    assert_eq!(manifest_secrets(&run_dir), vec!["message".to_string()]);
}

/// D7 fixtures item 1 (b): a run whose `playbook.yaml` lost the declaration
/// on disk (what a patch of an older apb left) still redacts the value its
/// manifest names.
#[test]
fn a_snapshot_without_the_declaration_never_drops_the_redaction() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    init_project(root).unwrap();
    // `p2` prints the value it reads from `run.yaml`.
    let printing = MONO.replace(
        r#"{ id: p2, type: prompt, prompt: "second" }"#,
        "{ id: p2, type: script, script: scripts/print.sh, runner: sh }",
    );
    seed_definition_with(
        root,
        "mono",
        &printing,
        &[("scripts/print.sh", &print_script())],
    );
    let res = run(root, "mono", None, options()).unwrap();
    assert_eq!(res.outcome, RunStatus::Succeeded);
    let run_dir = root.join(".apb/runs").join(&res.run_id);
    fs::write(
        run_dir.join("playbook.yaml"),
        printing.replace(
            "{ name: message, type: text, secret: true }",
            "{ name: message, type: text }",
        ),
    )
    .unwrap();
    assert_eq!(manifest_secrets(&run_dir), vec!["message".to_string()]);
    // The output folded from the journal holds the value as the executor
    // produced it; a fresh redactor removes it there.
    let state = apb_engine::state::RunState::fold(&read_all(&run_dir).unwrap());
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
        "a folded output was not redacted: {outputs}"
    );
    assert!(!r.redact_str(&p2).contains(SECRET));
    let (shown, names) = r.params(&params());
    assert_eq!(shown["message"], apb_core::run_params::REDACTED);
    assert_eq!(names, vec!["message".to_string()]);
}

/// D7 fixtures item 1 (c): a child run holding the value in a plain param
/// redacts its parent's value.
#[test]
fn a_child_redacts_its_parents_value_through_a_plain_param() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    init_project(root).unwrap();
    seed_definition(root, "mono", MONO);
    let res = run(root, "mono", None, options()).unwrap();
    let child = root.join(".apb/runs/child-1");
    fs::create_dir_all(&child).unwrap();
    apb_engine::run_config::write_run_config(
        &child,
        &apb_engine::run_config::RunConfig {
            params: [("note".to_string(), SECRET.to_string())].into(),
            parent_run: Some(res.run_id.clone()),
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

/// Sets `v` to the run's `params.message`, read from its `run.yaml` under
/// the top-level `params:` key. A line cut, as the authoring guide shows
/// it: it reads a value YAML leaves plain, which the fixture's value is.
const READ_MESSAGE: &str = "v=$(awk '/^params:/ { p = 1; next } /^[^ ]/ { p = 0 } p && sub(/^  message: /, \"\") { print; exit }' \"$APB_RUN_DIR/run.yaml\")\n";

/// Prints `said <value>`, the value read from the run's `run.yaml`.
fn print_script() -> String {
    format!("{READ_MESSAGE}echo \"said $v\"\n")
}

/// An edge that matches the value in an output routes exactly as it would
/// without redaction: the engine reads the executor's output.
#[test]
fn routing_on_an_output_that_holds_the_value_is_unchanged() {
    let tmp = tempfile::tempdir().unwrap();
    init_project(tmp.path()).unwrap();
    seed_definition_with(
        tmp.path(),
        "route",
        &format!(
            "schema: 1\nid: route\nname: Route\nversion: 1.0.0\n{SECRET_PARAM}nodes:\n  - {{ id: start, type: start }}\n  - {{ id: p, type: script, script: scripts/print.sh, runner: sh }}\n  - {{ id: ok, type: finish, outcome: success }}\n  - {{ id: no, type: finish, outcome: failure }}\nedges:\n  - {{ from: start, to: p }}\n  - {{ from: p, to: ok, condition: {{ type: output_match, node: p, pattern: \"{SECRET}\" }} }}\n  - {{ from: p, to: no, fallback: true }}\n"
        ),
        &[("scripts/print.sh", &print_script())],
    );
    let res = run(tmp.path(), "route", None, options()).unwrap();
    assert_eq!(
        res.outcome,
        RunStatus::Succeeded,
        "the match edge was not taken: {}",
        journal_text(&tmp.path().join(".apb/runs").join(&res.run_id))
    );
}

// --- a secret param is never rendered ----------------------------------------------

/// A playbook `sec` with the secret `message`, the plain `token`, `head`
/// above `params`, `extra` params, and `p2`'s prompt.
fn sec(version: &str, head: &str, extra: &str, p2: &str) -> String {
    format!(
        "schema: 2\nid: sec\nname: Sec\nversion: {version}\n{head}params:\n  - {{ name: message, type: text, secret: true }}\n  - {{ name: token, type: text }}\n{extra}supervisor:\n  policy:\n    promote_supervisor_patches: on_success\nnodes:\n  - {{ id: start, type: start }}\n  - {{ id: p1, type: prompt, prompt: \"first\" }}\n  - {{ id: p2, type: prompt, prompt: \"{p2}\" }}\n  - {{ id: done, type: finish, outcome: success }}\nedges:\n  - {{ from: start, to: p1 }}\n  - {{ from: p1, to: p2 }}\n  - {{ from: p2, to: done }}\n"
    )
}

const TOKEN: &str = "amber falcon";

fn sec_options() -> RunOptions {
    RunOptions {
        params: [
            ("message".to_string(), SECRET.to_string()),
            ("token".to_string(), TOKEN.to_string()),
        ]
        .into(),
        ..Default::default()
    }
}

fn sec_project(root: &Path, yaml: &str) {
    init_project(root).unwrap();
    seed_definition(root, "sec", yaml);
}

/// A run of `sec` paused before it ends.
fn paused_run(root: &Path, opts: RunOptions) -> String {
    let (run_id, res) = supervised(root, "sec", opts, vec![pause()]);
    assert_eq!(res.outcome, RunStatus::Paused);
    run_id
}

fn conflict(e: apb_engine::EngineError) -> String {
    match e {
        apb_engine::EngineError::Conflict(msg) => msg,
        other => panic!("expected a conflict, got {other:?}"),
    }
}

/// D10 test 5, the child half: a child playbook that declares a secret
/// param starts with it empty; nothing of the parent's value reaches its
/// run directory.
#[test]
fn a_child_gets_no_params_of_its_parent() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    init_project(root).unwrap();
    seed_definition(
        root,
        "kidpb",
        &format!(
            "schema: 2\nid: kidpb\nname: Kid\nversion: 1.0.0\n{SECRET_PARAM}nodes:\n  - {{ id: start, type: start }}\n  - {{ id: k, type: prompt, prompt: \"child\" }}\n  - {{ id: done, type: finish, outcome: success }}\nedges:\n  - {{ from: start, to: k }}\n  - {{ from: k, to: done }}\n"
        ),
    );
    seed_definition(
        root,
        "par",
        &format!(
            "schema: 2\nid: par\nname: Parent\nversion: 1.0.0\n{SECRET_PARAM}nodes:\n  - {{ id: start, type: start }}\n  - {{ id: kid, type: playbook, playbook: kidpb }}\n  - {{ id: done, type: finish, outcome: success }}\nedges:\n  - {{ from: start, to: kid }}\n  - {{ from: kid, to: done }}\n"
        ),
    );
    let res = run(root, "par", None, options()).unwrap();
    assert_eq!(res.outcome, RunStatus::Succeeded);
    let child = apb_engine::scheduler::list_runs(root)
        .unwrap()
        .into_iter()
        .find(|r| r.parent_run.as_deref() == Some(res.run_id.as_str()))
        .expect("the child run")
        .run_id;
    let child_dir = root.join(".apb/runs").join(&child);
    let cfg = apb_engine::run_config::read_run_config(&child_dir).unwrap();
    assert!(cfg.params.is_empty(), "{:?}", cfg.params.keys());
    let holders = files_holding(&child_dir, SECRET);
    assert!(holders.is_empty(), "{holders:?} hold the value");
}

/// D10 test 6: a version stored before the rule is refused at start,
/// naming V340, before any run directory exists.
#[test]
fn a_stored_version_that_renders_a_secret_is_refused_at_start() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    sec_project(root, &sec("1.0.0", "", "", "said {{params.message}}"));
    let e = run(root, "sec", None, sec_options())
        .unwrap_err()
        .to_string();
    assert!(e.contains("playbook `sec` is invalid"), "{e}");
    assert!(e.contains("V340"), "{e}");
    let runs = fs::read_dir(root.join(".apb/runs"))
        .map(|d| d.count())
        .unwrap_or(0);
    assert_eq!(runs, 0, "a run directory was left");
}

/// D10 test 7 (a), (b): a pre-change run is refused at both resume entries,
/// with nothing written; a stop still ends it.
#[test]
fn a_run_that_renders_a_secret_is_refused_at_resume_with_nothing_written() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    sec_project(root, &sec("1.0.0", "", "", "second"));
    let run_id = paused_run(root, sec_options());
    let run_dir = root.join(".apb/runs").join(&run_id);
    fs::write(
        run_dir.join("playbook.yaml"),
        sec("1.0.0", "", "", "said {{params.message}}"),
    )
    .unwrap();
    let before = tree(&root.join(".apb"));
    let detached = apb_engine::scheduler::resume_detached(root, &run_id, None).unwrap_err();
    let msg = conflict(detached);
    assert!(
        msg.starts_with(&format!(
            "secret_param_rendered: run `{run_id}` renders secret param `message`"
        )),
        "{msg}"
    );
    assert!(msg.contains("Stop the run"), "{msg}");
    let in_process = apb_engine::scheduler::resume(root, &run_id, None).unwrap_err();
    assert!(conflict(in_process).starts_with("secret_param_rendered: "));
    let preflight = apb_engine::scheduler::resume_secret_preflight(root, &run_id).unwrap_err();
    assert!(conflict(preflight).starts_with("secret_param_rendered: "));
    assert_eq!(tree(&root.join(".apb")), before, "a refused resume wrote");
    apb_engine::stop_run(root, &run_id).unwrap();
    let state = apb_engine::state::RunState::fold(&read_all(&run_dir).unwrap());
    assert_eq!(state.run_status, RunStatus::Aborted);
}

/// The detached driver of a prepared run whose start journaled no
/// definition digest (an older apb's, or a run directory made by hand)
/// checks the snapshot like a resume: a rendering one is never driven.
#[test]
fn a_digestless_prepared_run_that_renders_a_secret_is_not_driven() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    sec_project(root, &sec("1.0.0", "", "", "second"));
    let prepared = prepare_supervised_background(root, "sec", None, sec_options()).unwrap();
    let run_id = prepared.run_id().to_string();
    drop(prepared);
    let run_dir = root.join(".apb/runs").join(&run_id);
    let journal = fs::read_to_string(run_dir.join("events.jsonl")).unwrap();
    let stripped: String = journal
        .lines()
        .map(|line| {
            let mut v: Value = serde_json::from_str(line).unwrap();
            if v["type"] == "run_started" {
                v.as_object_mut().unwrap().remove("definition");
            }
            format!("{v}\n")
        })
        .collect();
    assert_ne!(stripped, journal, "the start journaled no digest to remove");
    fs::write(run_dir.join("events.jsonl"), &stripped).unwrap();
    fs::write(
        run_dir.join("playbook.yaml"),
        sec("1.0.0", "", "", "said {{params.message}}"),
    )
    .unwrap();
    let before = tree(&root.join(".apb"));
    let msg = conflict(apb_engine::scheduler::drive_run_from_dir(root, &run_id).unwrap_err());
    assert!(
        msg.starts_with(&format!(
            "secret_param_rendered: run `{run_id}` renders secret param `message`"
        )),
        "{msg}"
    );
    assert_eq!(tree(&root.join(".apb")), before, "the refused drive wrote");
}

/// The pause waker and `apb resume --all` reach a due pause through the
/// resume entries: a rendering run is refused, its journal unchanged.
#[test]
fn the_waker_and_resume_all_refuse_a_rendering_run() {
    // An installation key, so the run is stamped as created here: the
    // waker resumes only such a run.
    let _lock = common::env_lock();
    let cfg = tempfile::tempdir().unwrap();
    let _cfg = common::EnvVar::set("APB_CONFIG_DIR", cfg.path());
    apb_core::run_origin::ensure_key().unwrap();
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    sec_project(root, &sec("1.0.0", "", "", "second"));
    let run_id = paused_run(root, sec_options());
    let run_dir = root.join(".apb/runs").join(&run_id);
    // The pause in force is due: its `until` has passed.
    let now = u64::try_from(apb_core::clock::now_ms()).unwrap();
    let mut log = apb_engine::event::EventLog::open(&run_dir).unwrap();
    log.append(
        PauseRequest {
            reason: Some("later".into()),
            by: Some(PauseBy::Operator),
            until: Some(now - 1_000),
            ..PauseRequest::default()
        }
        .to_event(),
    )
    .unwrap();
    drop(log);
    let seq = read_all(&run_dir).unwrap().last().unwrap().seq;
    fs::write(
        run_dir.join("playbook.yaml"),
        sec("1.0.0", "", "", "said {{params.message}}"),
    )
    .unwrap();
    let journal = fs::read(run_dir.join("events.jsonl")).unwrap();

    let woke = format!("{:?}", apb_engine::pause::run_waker(root, &run_id, seq));
    assert!(woke.contains("secret_param_rendered"), "{woke}");
    assert_eq!(fs::read(run_dir.join("events.jsonl")).unwrap(), journal);

    let entries = apb_engine::pause::resume_all(root).unwrap();
    let action = entries
        .iter()
        .find(|e| e.run_id == run_id)
        .map(|e| format!("{:?}", e.action))
        .unwrap_or_default();
    assert!(action.contains("secret_param_rendered"), "{entries:?}");
    assert_eq!(fs::read(run_dir.join("events.jsonl")).unwrap(), journal);
    assert_eq!(
        apb_engine::state::RunState::fold(&read_all(&run_dir).unwrap()).run_status,
        RunStatus::Paused
    );
}

/// A parent resumed at a sub-playbook node reattaches its running child
/// through the resume entry: a child whose snapshot renders a secret param
/// is refused, and its journal is unchanged.
#[test]
fn a_reattached_child_that_renders_a_secret_is_refused() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    init_project(root).unwrap();
    let parent = "schema: 2\nid: parent\nname: parent\nversion: 1.0.0\nnodes:\n  - { id: s, type: start }\n  - { id: c, type: playbook, playbook: child, instruction: \"child input\" }\n  - { id: f, type: finish, outcome: success }\nedges:\n  - { from: s, to: c }\n  - { from: c, to: f }\n";
    let child = format!(
        "schema: 2\nid: child\nname: child\nversion: 1.0.0\n{SECRET_PARAM}nodes:\n  - {{ id: s, type: start }}\n  - {{ id: n, type: prompt, prompt: \"said {{{{params.message}}}}\" }}\n  - {{ id: f, type: finish, outcome: success }}\nedges:\n  - {{ from: s, to: n }}\n  - {{ from: n, to: f }}\n"
    );
    seed_definition(root, "parent", parent);
    seed_definition(
        root,
        "child",
        &child.replace("said {{params.message}}", "said"),
    );
    let seed_run = |id: &str, yaml: &str, events: &str| {
        let dir = root.join(".apb/runs").join(id);
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("events.jsonl"), events).unwrap();
        fs::write(dir.join("run.yaml"), "unattended: false\n").unwrap();
        fs::write(dir.join("playbook.yaml"), yaml).unwrap();
    };
    // The parent waits at node c on a child that is still running.
    seed_run(
        "parent-1",
        parent,
        concat!(
            "{\"seq\":0,\"ts\":0,\"type\":\"run_started\",\"playbook\":\"parent\",\"version\":\"1.0.0\"}\n",
            "{\"seq\":1,\"ts\":0,\"type\":\"node_started\",\"node\":\"c\",\"attempt\":1}\n",
            "{\"seq\":2,\"ts\":0,\"type\":\"child_run_started\",\"node_id\":\"c\",\"run_id\":\"child-1\"}\n",
        ),
    );
    seed_run(
        "child-1",
        &child,
        concat!(
            "{\"seq\":0,\"ts\":0,\"type\":\"run_started\",\"playbook\":\"child\",\"version\":\"1.0.0\"}\n",
            "{\"seq\":1,\"ts\":0,\"type\":\"node_finished\",\"node\":\"s\",\"status\":\"succeeded\",\"attempt\":1,\"output\":\"\"}\n",
        ),
    );
    let child_dir = root.join(".apb/runs/child-1");
    let journal = fs::read(child_dir.join("events.jsonl")).unwrap();
    let outcome = format!(
        "{:?}",
        apb_engine::scheduler::resume(root, "parent-1", Some("c"))
    );
    assert!(outcome.contains("outcome: Failed"), "{outcome}");
    let parent_journal = journal_text(&root.join(".apb/runs/parent-1"));
    assert!(
        parent_journal
            .contains("secret_param_rendered: run `child-1` renders secret param `message`"),
        "{parent_journal}"
    );
    assert_eq!(fs::read(child_dir.join("events.jsonl")).unwrap(), journal);
}

/// A retry-now is a resume surface: `expedite_live` refuses a run whose
/// definition renders a secret param before it journals anything, live
/// driver or not.
#[test]
fn a_retry_now_of_a_rendering_run_is_refused() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    init_project(root).unwrap();
    // A running run with a waiting retry, its driver pid file naming this
    // (live) process.
    let run_id = "r-live".to_string();
    let run_dir = root.join(".apb/runs").join(&run_id);
    fs::create_dir_all(&run_dir).unwrap();
    let mut log = apb_engine::event::EventLog::create(&run_dir).unwrap();
    for p in [
        EventPayload::RunStarted {
            playbook: "sec".into(),
            version: "1.0.0".into(),
            definition: None,
            external_data: false,
        },
        EventPayload::RetryDeferred {
            node: "p2".into(),
            after_attempt: 0,
            retry: 1,
            step: 0,
            binding: "b".repeat(64),
            delay_ms: 3_600_000,
            failure_kind: None,
        },
    ] {
        log.append(p).unwrap();
    }
    drop(log);
    fs::write(
        apb_engine::driver::driver_pid_path(&run_dir),
        std::process::id().to_string(),
    )
    .unwrap();
    fs::write(
        run_dir.join("playbook.yaml"),
        sec("1.0.0", "", "", "said {{params.message}}"),
    )
    .unwrap();
    let journal = fs::read(run_dir.join("events.jsonl")).unwrap();
    let msg = conflict(
        apb_engine::scheduler::deferred::expedite_live(
            &run_dir,
            &run_id,
            None,
            apb_engine::scheduler::deferred::ExpediteBy::Operator,
        )
        .unwrap_err(),
    );
    assert!(msg.starts_with("secret_param_rendered: "), "{msg}");
    assert_eq!(fs::read(run_dir.join("events.jsonl")).unwrap(), journal);
}

/// D10 test 7 (c): the run's set comes from the manifest too, not the
/// snapshot alone.
#[test]
fn a_snapshot_that_dropped_the_declaration_is_refused_at_resume() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    sec_project(root, &sec("1.0.0", "", "", "second"));
    let run_id = paused_run(root, sec_options());
    let run_dir = root.join(".apb/runs").join(&run_id);
    fs::write(
        run_dir.join("playbook.yaml"),
        sec("1.0.0", "", "", "said {{params.message}}").replace(
            "{ name: message, type: text, secret: true }",
            "{ name: message, type: text }",
        ),
    )
    .unwrap();
    assert_eq!(manifest_secrets(&run_dir), vec!["message".to_string()]);
    let before = tree(&root.join(".apb"));
    let msg = conflict(apb_engine::scheduler::resume(root, &run_id, None).unwrap_err());
    assert!(
        msg.starts_with(&format!(
            "secret_declaration_dropped: run `{run_id}` keeps param `message` plain"
        )),
        "{msg}"
    );
    let msg = conflict(apb_engine::scheduler::resume_detached(root, &run_id, None).unwrap_err());
    assert!(msg.starts_with("secret_declaration_dropped: "), "{msg}");
    assert_eq!(tree(&root.join(".apb")), before);
}

/// D10 test 7 (d): a schema-1 run with no manifest is refused before the
/// legacy shim writes its ephemeral manifest.
#[test]
fn a_legacy_run_is_refused_before_its_manifest_is_written() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    sec_project(root, &sec("1.0.0", "", "", "second"));
    let run_id = paused_run(root, sec_options());
    let run_dir = root.join(".apb/runs").join(&run_id);
    fs::remove_file(run_dir.join("manifest.yaml")).unwrap();
    fs::write(
        run_dir.join("playbook.yaml"),
        format!(
            "schema: 1\nid: sec\nname: Sec\nversion: 1.0.0\n{SECRET_PARAM}executors:\n  main: {{ agent: claude, model: haiku }}\ndefaults:\n  executor: main\nnodes:\n  - {{ id: start, type: start }}\n  - {{ id: p1, type: prompt, prompt: \"first\" }}\n  - {{ id: p2, type: prompt, prompt: \"said {{{{params.message}}}}\" }}\n  - {{ id: done, type: finish, outcome: success }}\nedges:\n  - {{ from: start, to: p1 }}\n  - {{ from: p1, to: p2 }}\n  - {{ from: p2, to: done }}\n"
        ),
    )
    .unwrap();
    let msg = conflict(apb_engine::scheduler::resume(root, &run_id, None).unwrap_err());
    assert!(msg.starts_with("secret_param_rendered: "), "{msg}");
    assert!(!run_dir.join("manifest.yaml").exists());
    let msg = conflict(apb_engine::scheduler::resume_secret_preflight(root, &run_id).unwrap_err());
    assert!(msg.starts_with("secret_param_rendered: "), "{msg}");
    assert!(!run_dir.join("manifest.yaml").exists());
}

/// D10 test 7 (f): a consent recorded before the snapshot changed does not
/// let the resume through; the test pins the refusal, not the consent.
#[test]
fn a_recorded_consent_does_not_let_a_rendering_run_resume() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    let head = "effects: [irreversible]\n";
    sec_project(root, &sec("1.0.0", head, "", "second"));
    let run_id = paused_run(
        root,
        RunOptions {
            consent: Some(apb_engine::consent::RunConsent::irreversible("terminal")),
            ..sec_options()
        },
    );
    let run_dir = root.join(".apb/runs").join(&run_id);
    let consent = apb_engine::manifest::read(&run_dir)
        .unwrap()
        .unwrap()
        .consent;
    assert!(consent.is_some(), "the start recorded no consent");
    fs::write(
        run_dir.join("playbook.yaml"),
        sec("1.0.0", head, "", "said {{params.message}}"),
    )
    .unwrap();
    let journal = fs::read(run_dir.join("events.jsonl")).unwrap();
    for e in [
        apb_engine::scheduler::resume(root, &run_id, None).unwrap_err(),
        apb_engine::scheduler::resume_detached(root, &run_id, None).unwrap_err(),
    ] {
        assert!(conflict(e).starts_with("secret_param_rendered: "));
    }
    assert_eq!(fs::read(run_dir.join("events.jsonl")).unwrap(), journal);
}

/// D10 test 7 (g): the preflight checks the run id before it joins it to
/// `.apb/runs`.
#[test]
fn the_preflight_refuses_an_unsafe_or_missing_run() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    init_project(root).unwrap();
    // A run directory outside `.apb/runs` the traversal would reach.
    fs::create_dir_all(root.join(".apb/evil")).unwrap();
    fs::write(root.join(".apb/evil/playbook.yaml"), "not: [yaml").unwrap();
    let abs = root.join(".apb/evil").display().to_string();
    for id in ["..", "a/b", "../evil", abs.as_str(), "missing-run"] {
        match apb_engine::scheduler::resume_secret_preflight(root, id) {
            Err(apb_engine::EngineError::NotFound(_)) => {}
            other => panic!("{id}: expected not found, got {other:?}"),
        }
    }
}

/// D10 test 8: the value stays in `run.yaml`, at the documented place.
#[test]
fn the_value_lives_only_in_run_yaml() {
    // A value of this test alone: no other test writes it, so whatever holds
    // it under the shared directories below is this run's.
    const VALUE: &str = "copper lantern eight";
    let only = || -> std::collections::BTreeMap<String, String> {
        [("message".to_string(), VALUE.to_string())].into()
    };
    let _lock = common::env_lock();
    let cfg = tempfile::tempdir().unwrap();
    let _cfg = common::EnvVar::set("APB_CONFIG_DIR", cfg.path());
    // The run's temporary files land in a directory of this test's own.
    let scratch = tempfile::tempdir().unwrap();
    let tmp = tempfile::tempdir().unwrap();
    let _tmpdir = common::EnvVar::set("TMPDIR", scratch.path());
    let root = tmp.path();
    init_project(root).unwrap();
    common::seed_main(root);
    let agent = write_agent(root, "echo ok");
    let _agent = common::EnvVar::set("APB_AGENT_CMD", &agent);
    // Reads the value and uses it without printing it.
    let check = format!("{READ_MESSAGE}[ -n \"$v\" ] && echo has-value\n");
    seed_definition_with(
        root,
        "keep",
        "schema: 2\nid: keep\nname: Keep\nversion: 1.0.0\nparams:\n  - { name: message, type: text, secret: true }\n  - { name: region, type: text }\n  - { name: tier, type: text, default: gold }\n  - { name: note, type: text }\ndefaults:\n  profile: main\nnodes:\n  - { id: start, type: start }\n  - { id: w, type: agent_task, prompt: \"Write the note\" }\n  - { id: p, type: prompt, prompt: \"region {{params.region}}\" }\n  - { id: s, type: script, script: scripts/check.sh, runner: sh }\n  - { id: done, type: finish, outcome: success }\nedges:\n  - { from: start, to: w }\n  - { from: w, to: p }\n  - { from: p, to: s }\n  - { from: s, to: done }\n",
        &[("scripts/check.sh", &check)],
    );
    // The project remembers `region` (and is handed the secret, which it
    // never stores).
    let loaded = apb_core::registry::Registry::open(root)
        .unwrap()
        .load("keep", None)
        .unwrap();
    let key =
        apb_core::run_memory::MemoryKey::new(apb_core::scope::Scope::Project, "keep").unwrap();
    apb_core::run_memory::remember(
        root,
        &key,
        &loaded.playbook.params,
        &loaded.version,
        apb_core::run_memory::RememberInput {
            params: Some(
                [
                    ("message".to_string(), VALUE.to_string()),
                    ("region".to_string(), "eu-north".to_string()),
                ]
                .into(),
            ),
            instruction: None,
        },
    )
    .unwrap();
    // A keyed start: the start key file exists and holds no value.
    let key = apb_core::start_key::StartKeyRequest {
        key: apb_core::start_key::StartKey::parse("keep-once").unwrap(),
        reuse: false,
        request: apb_core::start_key::StartRequest::capture(
            &apb_core::start_key::StartRequestFields {
                v: apb_core::start_key::REQUEST_V,
                params: only(),
                supervision: "autonomous".into(),
                cache: "auto".into(),
                ..Default::default()
            },
        )
        .unwrap(),
    };
    let res = run(
        root,
        "keep",
        None,
        RunOptions {
            start_key: Some(key),
            params: only(),
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(
        res.outcome,
        RunStatus::Succeeded,
        "{}",
        journal_text(&root.join(".apb/runs").join(&res.run_id))
    );
    assert!(
        !tree(&root.join(".apb/start-keys")).is_empty(),
        "no start key file was written"
    );
    let run_dir = root.join(".apb/runs").join(&res.run_id);
    let s_out = apb_engine::state::RunState::fold(&read_all(&run_dir).unwrap())
        .outputs
        .get("s")
        .cloned()
        .unwrap_or_default();
    assert!(
        s_out.contains("has-value"),
        "the script did not read the value: {s_out}"
    );

    // The project, the config dir and the run's temporary directory: only
    // `run.yaml` holds the value.
    let mut holders = files_holding(root, VALUE);
    holders.extend(files_holding(cfg.path(), VALUE));
    holders.extend(files_holding(scratch.path(), VALUE));
    assert_eq!(holders, vec![run_dir.join("run.yaml")], "{holders:?}");
    let mode = fs::metadata(run_dir.join("run.yaml"))
        .unwrap()
        .permissions()
        .mode();
    assert_eq!(mode & 0o777, 0o600);
    let parsed: yaml_serde::Value =
        yaml_serde::from_str(&fs::read_to_string(run_dir.join("run.yaml")).unwrap()).unwrap();
    let params = parsed.get("params").unwrap();
    assert_eq!(params.get("message").and_then(|v| v.as_str()), Some(VALUE));
    assert_eq!(
        params.get("region").and_then(|v| v.as_str()),
        Some("eu-north")
    );
    assert_eq!(params.get("tier").and_then(|v| v.as_str()), Some("gold"));
    assert!(params.get("note").is_none(), "{params:?}");
    let page = apb_engine::report_html::render(root, &run_dir).unwrap();
    assert!(!page.contains(VALUE));

    // A value YAML must quote reads back unchanged through `params`.
    let quoted = tmp.path().join("quoted");
    fs::create_dir_all(&quoted).unwrap();
    apb_engine::run_config::write_run_config(
        &quoted,
        &apb_engine::run_config::RunConfig {
            params: [("message".to_string(), "007: #a".to_string())].into(),
            ..Default::default()
        },
    )
    .unwrap();
    let parsed: yaml_serde::Value =
        yaml_serde::from_str(&fs::read_to_string(quoted.join("run.yaml")).unwrap()).unwrap();
    assert_eq!(
        parsed
            .get("params")
            .and_then(|p| p.get("message"))
            .and_then(|v| v.as_str()),
        Some("007: #a")
    );
}

/// A `success_check` is not handed `APB_RUN_DIR`: it cannot find the
/// run's `run.yaml` through it.
#[test]
fn a_success_check_gets_no_run_dir() {
    let _lock = common::env_lock();
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    init_project(root).unwrap();
    common::seed_main(root);
    let agent = write_agent(root, "echo ok");
    let _agent = common::EnvVar::set("APB_AGENT_CMD", &agent);
    let seen = root.join("check-env.txt");
    let check = format!(
        "echo \"dir=${{APB_RUN_DIR-unset}}\" > '{}'\n",
        seen.display()
    );
    seed_definition_with(
        root,
        "chk",
        &format!(
            "schema: 2\nid: chk\nname: Chk\nversion: 1.0.0\n{SECRET_PARAM}defaults:\n  profile: main\nnodes:\n  - {{ id: start, type: start }}\n  - {{ id: w, type: agent_task, prompt: \"Write the note\", success_check: scripts/check.sh }}\n  - {{ id: done, type: finish, outcome: success }}\nedges:\n  - {{ from: start, to: w }}\n  - {{ from: w, to: done }}\n"
        ),
        &[("scripts/check.sh", &check)],
    );
    let res = run(root, "chk", None, options()).unwrap();
    assert_eq!(
        res.outcome,
        RunStatus::Succeeded,
        "{}",
        journal_text(&root.join(".apb/runs").join(&res.run_id))
    );
    assert_eq!(fs::read_to_string(&seen).unwrap().trim(), "dir=unset");
}

/// D10 test 11 (b), (c): a patch that removes a referenced secret param,
/// and a stored version that renders one, are refused; the snapshot stays.
#[test]
fn a_patch_that_removes_or_renders_a_secret_is_refused() {
    let removed = sec("1.0.1", "", "", "said {{params.message}}")
        .replace("  - { name: message, type: text, secret: true }\n", "");
    let rendered = sec("1.0.1", "", "", "said {{params.message}}");
    for (yaml, code) in [
        (removed, "secret_declaration_dropped: "),
        (rendered, "secret_param_rendered: "),
    ] {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        sec_project(root, &sec("1.0.0", "", "", "second"));
        store_version(root, "sec", "1.0.1", &yaml, &[]);
        let before = fs::read(root.join(".apb/playbooks/sec/1.0.0/playbook.yaml")).unwrap();
        let (run_id, res) = supervised(root, "sec", sec_options(), vec![patch("1.0.1")]);
        assert_eq!(res.outcome, RunStatus::Succeeded);
        let run_dir = root.join(".apb/runs").join(&run_id);
        assert!(
            rejection(&run_dir).starts_with(code),
            "{}",
            rejection(&run_dir)
        );
        assert!(!applied(&run_dir));
        assert_eq!(fs::read(run_dir.join("playbook.yaml")).unwrap(), before);
        assert_eq!(manifest_secrets(&run_dir), vec!["message".to_string()]);
    }
}

/// D10 test 11 (d): a patch refused after the secret check (it loosens the
/// launch policy) never leaves the manifest naming the param it declared.
#[test]
fn a_patch_refused_later_records_no_new_secret_name() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    sec_project(root, &sec("1.0.0", "agent_launch: deny\n", "", "second"));
    store_version(
        root,
        "sec",
        "1.0.1",
        &sec("1.0.1", "agent_launch: allow\n", "", "second").replace(
            "{ name: token, type: text }",
            "{ name: token, type: text, secret: true }",
        ),
        &[],
    );
    let before = fs::read(root.join(".apb/playbooks/sec/1.0.0/playbook.yaml")).unwrap();
    // A `deny` playbook starts only through a launch check: a person's.
    let _lock = common::env_lock();
    let permit = apb_engine::gate::check_run(
        root,
        &sec_ref(),
        true,
        true,
        &apb_engine::gate::launch::LaunchContext::dashboard(
            apb_engine::gate::launch::LaunchKind::Start,
        ),
    )
    .unwrap();
    let mut opts = sec_options();
    permit.apply(&mut opts);
    let (run_id, res) = supervised(root, "sec", opts, vec![patch("1.0.1")]);
    assert_eq!(res.outcome, RunStatus::Succeeded);
    let run_dir = root.join(".apb/runs").join(&run_id);
    assert!(
        rejection(&run_dir).contains("launch_policy_loosened"),
        "{}",
        rejection(&run_dir)
    );
    assert_eq!(manifest_secrets(&run_dir), vec!["message".to_string()]);
    assert_eq!(fs::read(run_dir.join("playbook.yaml")).unwrap(), before);
    apb_engine::scheduler::resume_secret_preflight(root, &run_id).unwrap();
}

/// D10 test 11 (e), (f): an applied patch's new name is in the manifest;
/// with it removed again (a crash between the new snapshot and the second
/// manifest write) the run still counts it secret through its snapshot.
#[test]
fn an_applied_patch_records_its_new_secret_name_after_the_snapshot() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    sec_project(root, &sec("1.0.0", "", "", "second"));
    let old_snapshot = fs::read(root.join(".apb/playbooks/sec/1.0.0/playbook.yaml")).unwrap();
    store_version(
        root,
        "sec",
        "1.0.1",
        &sec("1.0.1", "", "", "second").replace(
            "{ name: token, type: text }",
            "{ name: token, type: text, secret: true }",
        ),
        &[],
    );
    let (run_id, res) = supervised(root, "sec", sec_options(), vec![patch("1.0.1")]);
    assert_eq!(res.outcome, RunStatus::Succeeded);
    let run_dir = root.join(".apb/runs").join(&run_id);
    assert!(applied(&run_dir));
    assert_eq!(
        manifest_secrets(&run_dir),
        vec!["message".to_string(), "token".to_string()]
    );

    // The crash state: the new snapshot, the manifest without `token`.
    forget_in_manifest(&run_dir, "token");
    assert_eq!(manifest_secrets(&run_dir), vec!["message".to_string()]);
    apb_engine::scheduler::resume_secret_preflight(root, &run_id).unwrap();
    let set = apb_engine::run_redact::secret_names_strict(&run_dir).unwrap();
    assert!(set.contains("token"), "{set:?}");
    assert!(
        !fresh(root, &run_dir)
            .redact_str(&format!("t {TOKEN}"))
            .contains(TOKEN)
    );

    // The state before the snapshot write: the unpatched run.
    fs::write(run_dir.join("playbook.yaml"), &old_snapshot).unwrap();
    apb_engine::scheduler::resume_secret_preflight(root, &run_id).unwrap();
    let set = apb_engine::run_redact::secret_names_strict(&run_dir).unwrap();
    assert!(!set.contains("token"), "{set:?}");
}

/// Puts `kind` where the run's manifest was: a file that does not parse,
/// a directory, or a dangling link.
fn break_manifest(run_dir: &Path, kind: &str) {
    let path = run_dir.join("manifest.yaml");
    let _ = fs::remove_file(&path);
    match kind {
        "garbage" => fs::write(&path, "secret_params: [unclosed\n").unwrap(),
        "directory" => fs::create_dir_all(path.join("inside")).unwrap(),
        "dangling" => std::os::unix::fs::symlink(run_dir.join("gone.yaml"), &path).unwrap(),
        other => panic!("{other}"),
    }
}

/// A directory or a dangling link at `manifest.yaml` is not "no manifest":
/// after a patch dropped an unused secret declaration the manifest is the
/// only record of it, so a resume and the preflight refuse
/// `secret_set_unreadable` and write nothing.
#[test]
fn a_manifest_that_is_not_a_regular_file_refuses_a_resume() {
    for kind in ["directory", "dangling"] {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        sec_project(root, &sec("1.0.0", "", "", "second"));
        let run_id = paused_run(root, sec_options());
        let run_dir = root.join(".apb/runs").join(&run_id);
        // The snapshot no longer declares `message` (an unused declaration
        // a patch dropped): only the manifest names it.
        fs::write(
            run_dir.join("playbook.yaml"),
            sec("1.0.0", "", "", "second")
                .replace("  - { name: message, type: text, secret: true }\n", ""),
        )
        .unwrap();
        break_manifest(&run_dir, kind);
        let before = tree(&root.join(".apb"));
        let msg =
            conflict(apb_engine::scheduler::resume_secret_preflight(root, &run_id).unwrap_err());
        assert!(
            msg.starts_with(&format!(
                "secret_set_unreadable: run `{run_id}`: manifest.yaml does not read"
            )),
            "{kind}: {msg}"
        );
        // Both resume entries refuse too: through the secret check, or an
        // earlier read of the same manifest that refuses it first.
        for e in [
            apb_engine::scheduler::resume(root, &run_id, None).unwrap_err(),
            apb_engine::scheduler::resume_detached(root, &run_id, None).unwrap_err(),
        ] {
            let msg = e.to_string();
            assert!(
                msg.contains("secret_set_unreadable") || msg.contains("manifest.yaml"),
                "{kind}: {msg}"
            );
        }
        assert_eq!(
            tree(&root.join(".apb")),
            before,
            "{kind}: a refused resume wrote"
        );
    }
}

fn forward(root: &Path, run_id: &str, yaml: &str) -> Result<String, apb_engine::EngineError> {
    apb_engine::forward_patch::create(
        root,
        run_id,
        &apb_engine::forward_patch::ForwardPatchRequest {
            yaml: yaml.to_string(),
            classification: "improvement".into(),
            rationale: Some("a clearer prompt".into()),
            evidence: vec!["node p2".into()],
        },
    )
    .map(|r| r.version)
}

fn candidate_pointer(root: &Path) -> Option<String> {
    fs::read_to_string(root.join(".apb/playbooks/sec/candidate")).ok()
}

/// D10 test 12: a forward patch that drops a declaration is refused, and
/// the run's set is read right before the candidate write.
#[test]
fn a_forward_patch_that_drops_a_declaration_is_refused() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    sec_project(root, &sec("1.0.0", "", "", "second"));
    let res = run(root, "sec", None, sec_options()).unwrap();
    assert_eq!(res.outcome, RunStatus::Succeeded);
    let dropped = sec("1.0.0", "", "", "second again").replace(
        "{ name: message, type: text, secret: true }",
        "{ name: message, type: text }",
    );
    let e = forward(root, &res.run_id, &dropped)
        .unwrap_err()
        .to_string();
    assert!(e.contains("(secret_declaration_dropped)"), "{e}");
    assert_eq!(candidate_pointer(root), None);

    // The recheck: the YAML is made from the run as it started (`token`
    // plain, rendered), then an in-run patch of a second run declares
    // `token` secret.
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    sec_project(root, &sec("1.0.0", "", "", "second"));
    let yaml = sec("1.0.0", "", "", "token {{params.token}}");
    store_version(
        root,
        "sec",
        "1.0.1",
        &sec("1.0.1", "", "", "second").replace(
            "{ name: token, type: text }",
            "{ name: token, type: text, secret: true }",
        ),
        &[],
    );
    let (run_id, res) = supervised(root, "sec", sec_options(), vec![patch("1.0.1")]);
    assert_eq!(res.outcome, RunStatus::Succeeded);
    assert!(applied(&root.join(".apb/runs").join(&run_id)));
    let e = forward(root, &run_id, &yaml).unwrap_err().to_string();
    assert!(e.contains("(secret_declaration_dropped)"), "{e}");
    assert_eq!(candidate_pointer(root), None);

    // Only the run's set can refuse: the manifest names `token`, which the
    // base version (and so the YAML made from it) declares plain.
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    sec_project(root, &sec("1.0.0", "", "", "second"));
    let res = run(root, "sec", None, sec_options()).unwrap();
    assert_eq!(res.outcome, RunStatus::Succeeded);
    let run_dir = root.join(".apb/runs").join(&res.run_id);
    apb_engine::manifest::add_secret_params(&run_dir, &res.run_id, ["token".to_string()]).unwrap();
    let e = forward(
        root,
        &res.run_id,
        &sec("1.0.0", "", "", "token {{params.token}}"),
    )
    .unwrap_err()
    .to_string();
    assert!(e.contains("(secret_declaration_dropped)"), "{e}");
    assert!(e.contains("param `token`"), "{e}");
    assert_eq!(candidate_pointer(root), None);
}

/// D10 test 15, the integration part: the crash state between a patch's
/// snapshot replacement and its manifest update, with the snapshot then
/// unreadable or unparsable, refuses every check that reads the run's set.
#[test]
fn an_unreadable_secret_set_refuses_the_forward_patch_and_the_resume() {
    for broken in ["directory", "garbage"] {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        sec_project(root, &sec("1.0.0", "", "", "second"));
        store_version(
            root,
            "sec",
            "1.0.1",
            &sec("1.0.1", "", "", "second").replace(
                "{ name: token, type: text }",
                "{ name: token, type: text, secret: true }",
            ),
            &[],
        );
        let (run_id, _) = supervised(root, "sec", sec_options(), vec![patch("1.0.1")]);
        let run_dir = root.join(".apb/runs").join(&run_id);
        forget_in_manifest(&run_dir, "token");
        fs::remove_file(run_dir.join("playbook.yaml")).unwrap();
        if broken == "directory" {
            fs::create_dir_all(run_dir.join("playbook.yaml")).unwrap();
        } else {
            fs::write(run_dir.join("playbook.yaml"), "params: {not: [a list\n").unwrap();
        }
        let yaml = sec("1.0.0", "", "", "token {{params.token}}");
        let e = forward(root, &run_id, &yaml).unwrap_err().to_string();
        assert!(e.contains("(secret_set_unreadable)"), "{broken}: {e}");
        assert!(e.contains("playbook.yaml does not read"), "{broken}: {e}");
        assert_eq!(candidate_pointer(root), None);
        let journal = fs::read(run_dir.join("events.jsonl")).unwrap();
        let manifest = fs::read(run_dir.join("manifest.yaml")).unwrap();
        for e in [
            apb_engine::scheduler::resume_secret_preflight(root, &run_id).unwrap_err(),
            apb_engine::scheduler::resume(root, &run_id, None).unwrap_err(),
        ] {
            let msg = conflict(e);
            assert!(
                msg.starts_with(&format!(
                    "secret_set_unreadable: run `{run_id}`: playbook.yaml does not read"
                )),
                "{broken}: {msg}"
            );
        }
        assert_eq!(fs::read(run_dir.join("events.jsonl")).unwrap(), journal);
        assert_eq!(fs::read(run_dir.join("manifest.yaml")).unwrap(), manifest);
    }
}

/// A consent surface meets a manifest that does not read: the preflight
/// refuses `secret_set_unreadable`, naming the file, and writes nothing.
#[test]
fn the_preflight_refuses_a_run_whose_manifest_does_not_read() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    sec_project(root, &sec("1.0.0", "", "", "second"));
    let run_id = paused_run(root, sec_options());
    let run_dir = root.join(".apb/runs").join(&run_id);
    fs::write(
        run_dir.join("manifest.yaml"),
        "secret_params: [unclosed
",
    )
    .unwrap();
    let before = tree(&root.join(".apb"));
    let msg = conflict(apb_engine::scheduler::resume_secret_preflight(root, &run_id).unwrap_err());
    assert!(
        msg.starts_with(&format!(
            "secret_set_unreadable: run `{run_id}`: manifest.yaml does not read"
        )),
        "{msg}"
    );
    assert_eq!(tree(&root.join(".apb")), before, "the preflight wrote");
}

/// D10 test 15: an in-run patch on a run whose manifest does not read is
/// rejected `secret_set_unreadable`.
#[test]
fn an_in_run_patch_on_an_unreadable_manifest_is_rejected() {
    // A run with a secret param, and one without: the set is read strictly
    // for every run.
    let plain = |v: &str| {
        sec(v, "", "", "second").replace(
            "{ name: message, type: text, secret: true }",
            "{ name: message, type: text }",
        )
    };
    for (declared, broken) in [
        (true, "garbage"),
        (true, "directory"),
        (true, "dangling"),
        (false, "garbage"),
    ] {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let (base, next) = if declared {
            (
                sec("1.0.0", "", "", "second"),
                sec("1.0.1", "", "", "second, patched"),
            )
        } else {
            (
                plain("1.0.0"),
                plain("1.0.1").replace("second", "second, patched"),
            )
        };
        sec_project(root, &base);
        store_version(root, "sec", "1.0.1", &next, &[]);
        let prepared = prepare_supervised_background(
            root,
            "sec",
            None,
            RunOptions {
                mode: RunMode::Supervised,
                ..sec_options()
            },
        )
        .unwrap();
        let run_id = prepared.run_id().to_string();
        let run_dir = root.join(".apb/runs").join(&run_id);
        let before = fs::read(run_dir.join("playbook.yaml")).unwrap();
        break_manifest(&run_dir, broken);
        post_supervisor_command(root, &run_id, patch("1.0.1")).unwrap();
        let _ = drive_in_background(root.to_path_buf(), prepared)
            .recv_timeout(Duration::from_secs(120))
            .expect("the drive did not finish");
        assert!(
            rejection(&run_dir).starts_with("secret_set_unreadable: "),
            "declared {declared}, {broken}: {}",
            rejection(&run_dir)
        );
        assert_eq!(fs::read(run_dir.join("playbook.yaml")).unwrap(), before);
    }
}

/// Starts `sec` through the gate as every surface does; returns the
/// permit's `candidate_skipped` and the version the run ran.
fn sec_ref() -> apb_core::scope::PlaybookRef {
    apb_core::scope::PlaybookRef {
        origin: apb_core::scope::Origin::Project { workspace_id: None },
        id: "sec".into(),
        version: None,
    }
}

fn gated_start(root: &Path) -> (Option<String>, String) {
    let permit = apb_engine::gate::check_run(
        root,
        &sec_ref(),
        true,
        false,
        &apb_engine::gate::launch::LaunchContext::dashboard(
            apb_engine::gate::launch::LaunchKind::Start,
        ),
    )
    .unwrap();
    let skipped = permit.candidate_skipped.clone();
    let version = permit.run_version(None);
    let mut opts = sec_options();
    permit.apply(&mut opts);
    let res = run(root, "sec", version.as_deref(), opts).unwrap();
    assert_eq!(res.outcome, RunStatus::Succeeded);
    let ran = read_all(&root.join(".apb/runs").join(&res.run_id))
        .unwrap()
        .into_iter()
        .find_map(|e| match e.payload {
            EventPayload::RunStarted { version, .. } => Some(version),
            _ => None,
        })
        .unwrap();
    (skipped, ran)
}

/// A trusted `current` and a waiting candidate made by a forward patch,
/// whose stored definition is then rewritten on disk by `edit`.
fn waiting_candidate(root: &Path, edit: impl Fn(&str) -> String) {
    sec_project(root, &sec("1.0.0", "", "", "second"));
    let loaded = apb_core::registry::Registry::open(root)
        .unwrap()
        .load("sec", Some("1.0.0"))
        .unwrap();
    apb_core::trust::TrustStore::load()
        .approve(
            &loaded.trust_digest().unwrap(),
            "sec",
            apb_core::trust::OriginKind::LocallyApproved,
        )
        .unwrap();
    let first = run(root, "sec", None, sec_options()).unwrap();
    let version = forward(
        root,
        &first.run_id,
        &sec("1.0.0", "", "", "second, clearer"),
    )
    .unwrap();
    let path = root
        .join(".apb/playbooks/sec")
        .join(&version)
        .join("playbook.yaml");
    let stored = fs::read_to_string(&path).unwrap();
    fs::write(&path, edit(&stored)).unwrap();
}

/// D10 test 13: a waiting candidate that renders a secret, or drops a
/// declaration `current` holds, is skipped and the start runs `current`.
#[test]
fn a_waiting_candidate_that_renders_or_declassifies_is_skipped() {
    let _lock = common::env_lock();
    for (edit, code) in [
        (
            Box::new(|s: &str| s.replace("second, clearer", "said {{params.message}}"))
                as Box<dyn Fn(&str) -> String>,
            "refused: secret_param_rendered",
        ),
        (
            Box::new(|s: &str| {
                // The stored candidate is apb's own serialization: edit
                // the declaration through the YAML value.
                let mut v: yaml_serde::Value = yaml_serde::from_str(s).unwrap();
                for p in v["params"].as_sequence_mut().unwrap() {
                    if p["name"].as_str() == Some("message") {
                        p.as_mapping_mut().unwrap().remove("secret");
                    }
                }
                let edited = yaml_serde::to_string(&v).unwrap();
                assert_ne!(edited, s, "the declaration was not dropped");
                edited
            }),
            "refused: secret_declaration_dropped",
        ),
    ] {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        waiting_candidate(root, edit);
        let (skipped, ran) = gated_start(root);
        assert_eq!(skipped.as_deref(), Some(code));
        assert_eq!(ran, "1.0.0");
    }
}

/// A retry that fixed the candidate as its choice (`launch_version` and
/// `launch_choice: candidate`) goes through the same secret check as a
/// waiting candidate, and has no fallback to `current`: the start is
/// refused with the check's code.
#[test]
fn a_fixed_candidate_that_renders_or_declassifies_is_refused() {
    let _lock = common::env_lock();
    for (edit, code) in [
        (
            Box::new(|s: &str| s.replace("second, clearer", "said {{params.message}}"))
                as Box<dyn Fn(&str) -> String>,
            "secret_param_rendered",
        ),
        (
            Box::new(|s: &str| {
                let mut v: yaml_serde::Value = yaml_serde::from_str(s).unwrap();
                for p in v["params"].as_sequence_mut().unwrap() {
                    if p["name"].as_str() == Some("message") {
                        p.as_mapping_mut().unwrap().remove("secret");
                    }
                }
                yaml_serde::to_string(&v)
                    .unwrap()
                    .replace("second, clearer", "said {{params.message}}")
            }),
            "secret_declaration_dropped",
        ),
    ] {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        waiting_candidate(root, edit);
        let version = candidate_pointer(root).expect("a waiting candidate");
        let refusal = apb_engine::gate::check_run(
            root,
            &sec_ref(),
            true,
            false,
            &apb_engine::gate::launch::LaunchContext::dashboard(
                apb_engine::gate::launch::LaunchKind::Start,
            )
            .fixed(Some(version.trim().to_string()), Some("candidate")),
        )
        .expect_err("the fixed candidate was admitted");
        assert_eq!(refusal["policy"], code, "{refusal}");
        assert!(
            !refusal.to_string().contains(SECRET),
            "the refusal names a value: {refusal}"
        );
    }
}

/// The fields V104 and V210 own at validation, and `previous_success.when_none`
/// beside them, each reading the secret: `(head above params, the same
/// field reading it)`.
fn owned_fields() -> Vec<(&'static str, String)> {
    vec![
        (
            "quality.task_type",
            "quality:\n  task_type: \"{{params.message}}\"\n".to_string(),
        ),
        (
            "previous_success.when_none.started_at",
            "previous_success:\n  when_none:\n    started_at: \"{{params.message}}\"\n".to_string(),
        ),
        (
            "concurrency.key",
            "concurrency:\n  key: \"{{params.message}}\"\n".to_string(),
        ),
    ]
}

/// D10 test 4, outside validation: a candidate, an in-run patch and a
/// resume each see the three owned fields.
#[test]
fn the_owned_fields_are_checked_outside_validation() {
    let _lock = common::env_lock();
    for (field, head) in owned_fields() {
        // (a) A waiting candidate.
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        waiting_candidate(root, |s| format!("{head}{s}"));
        let (skipped, ran) = gated_start(root);
        assert_eq!(
            skipped.as_deref(),
            Some("refused: secret_param_rendered"),
            "{field}"
        );
        assert_eq!(ran, "1.0.0", "{field}");

        // (b) An in-run patch naming a stored version.
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        sec_project(root, &sec("1.0.0", "", "", "second"));
        store_version(
            root,
            "sec",
            "1.0.1",
            &sec("1.0.1", &head, "", "second"),
            &[],
        );
        let before = fs::read(root.join(".apb/playbooks/sec/1.0.0/playbook.yaml")).unwrap();
        let (run_id, _) = supervised(root, "sec", sec_options(), vec![patch("1.0.1")]);
        let run_dir = root.join(".apb/runs").join(&run_id);
        let reason = rejection(&run_dir);
        assert!(
            reason.starts_with("secret_param_rendered: "),
            "{field}: {reason}"
        );
        assert!(reason.contains(field), "{field}: {reason}");
        assert_eq!(fs::read(run_dir.join("playbook.yaml")).unwrap(), before);

        // (c) A resume of a run whose snapshot reads it there.
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        sec_project(root, &sec("1.0.0", "", "", "second"));
        let run_id = paused_run(root, sec_options());
        let run_dir = root.join(".apb/runs").join(&run_id);
        fs::write(
            run_dir.join("playbook.yaml"),
            sec("1.0.0", &head, "", "second"),
        )
        .unwrap();
        let journal = fs::read(run_dir.join("events.jsonl")).unwrap();
        let msg = conflict(apb_engine::scheduler::resume(root, &run_id, None).unwrap_err());
        assert!(msg.starts_with("secret_param_rendered: "), "{field}: {msg}");
        assert_eq!(fs::read(run_dir.join("events.jsonl")).unwrap(), journal);
    }
}

/// D10 test 16: a run with a secret param carries no mark of it: no
/// `run_guard`, the same event types as without the declaration, and no
/// manifest field main's lacks.
#[test]
fn a_secret_param_leaves_no_mark_on_the_run() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    init_project(root).unwrap();
    let plain = MONO.replace(
        "{ name: message, type: text, secret: true }",
        "{ name: message, type: text }",
    );
    seed_definition(root, "mono", MONO);
    seed_definition(root, "plain", &plain.replace("id: mono", "id: plain"));
    let secret_run = run(root, "mono", None, options()).unwrap();
    let plain_run = run(root, "plain", None, options()).unwrap();
    let kinds = |id: &str| -> Vec<String> {
        read_all(&root.join(".apb/runs").join(id))
            .unwrap()
            .iter()
            .map(|e| {
                serde_json::to_value(&e.payload).unwrap()["type"]
                    .as_str()
                    .unwrap()
                    .to_string()
            })
            .collect()
    };
    let secret_kinds = kinds(&secret_run.run_id);
    assert!(
        !secret_kinds.iter().any(|k| k == "run_guard"),
        "{secret_kinds:?}"
    );
    assert_eq!(secret_kinds, kinds(&plain_run.run_id));
    let keys = |id: &str| -> std::collections::BTreeSet<String> {
        let m: serde_json::Value = yaml_serde::from_str(
            &fs::read_to_string(root.join(".apb/runs").join(id).join("manifest.yaml")).unwrap(),
        )
        .unwrap();
        m.as_object().unwrap().keys().cloned().collect()
    };
    // A plain prompt-only run writes no manifest at all; the secret run's
    // holds exactly the keys main writes for it, `secret_params` among them.
    assert!(
        !root
            .join(".apb/runs")
            .join(&plain_run.run_id)
            .join("manifest.yaml")
            .exists()
    );
    let expected: std::collections::BTreeSet<String> = [
        "connector_grants",
        "connectors",
        "node_bindings",
        "profiles",
        "secret_params",
    ]
    .into_iter()
    .map(String::from)
    .collect();
    assert_eq!(keys(&secret_run.run_id), expected);
}
