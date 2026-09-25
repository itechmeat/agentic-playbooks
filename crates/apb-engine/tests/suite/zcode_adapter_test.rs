//! zcode (Z.ai's ZCode headless CLI) through the generic headless adapter:
//! argv with an explicit permission mode, `--json` unwrapping, session
//! capture, and error/quota classification.
//!
//! Fixture provenance (`tests/fixtures/zcode/`):
//! - `error_no_model.stderr` is a REAL capture from zcode-agent 0.16.9 on a
//!   machine whose standalone CLI was not logged in.
//! - `json_result.json` and `stream_json_result.jsonl` follow the output
//!   writer of zcode-agent 0.16.9 (`runPrompt`: `--json` prints
//!   `JSON.stringify(summary, null, 2)` with `sessionId`, `traceId`, `turnId`,
//!   `response`, `usage`, `eventCount`, `projection`; `--output-format
//!   stream-json` ends with a one-line `{"type":"result",...}`), with
//!   placeholder ids: a real headless turn needs `zcode-agent login`.
//! - `error_quota.stderr` is Z.ai's documented 1308 message in the CLI's
//!   `Error: <message> (traceId: ...)` framing.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;

use apb_engine::adapter::{
    AgentAdapter, AgentTask, ClaudeAdapter, ConnectorEnvPolicy, ErrorClass, capture_session,
};
use apb_engine::event::{EventPayload, read_all};
use apb_engine::failure_class::{FailureKind, classify};
use apb_engine::invocation::builtin;
use apb_engine::scheduler::{RunOptions, run};
use apb_engine::state::{NodeStatus, RunStatus};

use crate::common;

const FIXTURES: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/zcode");

fn fixture(name: &str) -> String {
    fs::read_to_string(Path::new(FIXTURES).join(name)).unwrap()
}

/// A stub zcode: records its argv (one element per line, NUL-free) into
/// `argv.txt` next to itself, then runs `body`.
fn stub_zcode(dir: &Path, body: &str) -> String {
    let path = dir.join("zcode");
    let argv_file = dir.join("argv.txt");
    common::write_sync(
        &path,
        &format!(
            "#!/bin/sh\nfor a in \"$@\"; do printf '%s\\001' \"$a\" >> '{}'; done\n{body}\n",
            argv_file.display()
        ),
    );
    let mut perm = fs::metadata(&path).unwrap().permissions();
    perm.set_mode(0o755);
    fs::set_permissions(&path, perm).unwrap();
    path.to_string_lossy().into_owned()
}

fn recorded_argv(dir: &Path) -> Vec<String> {
    fs::read_to_string(dir.join("argv.txt"))
        .unwrap()
        .split('\u{1}')
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect()
}

fn task<'a>(dir: &'a Path, policy: &'a ConnectorEnvPolicy, grant_autonomy: bool) -> AgentTask<'a> {
    AgentTask {
        prompt: "reply PONG",
        // Empty: no plan selection, so the test does not depend on a ZCode
        // login in the environment (model passing is covered in apb-core).
        model: "",
        workdir: dir,
        timeout: None,
        stream_log: None,
        soul: Some("You are terse."),
        grant_autonomy,
        connector_policy: policy,
        interactive: false,
        report_contract: true,
        node: "n",
        agent: "zcode",
        extract: None,
        status_file: None,
        hermetic_settings: None,
    }
}

#[test]
fn zcode_json_output_is_unwrapped_and_its_session_captured() {
    let dir = tempfile::tempdir().unwrap();
    let json = Path::new(FIXTURES).join("json_result.json");
    let ad = ClaudeAdapter {
        program: stub_zcode(dir.path(), &format!("cat '{}'", json.display())),
        spec: builtin("zcode").unwrap(),
    };
    let report = ad
        .run(&task(dir.path(), &ConnectorEnvPolicy::default(), false))
        .unwrap();
    assert_eq!(report.status, NodeStatus::Succeeded);
    // The node output is the reply inside `response`, report block stripped,
    // not the JSON envelope.
    assert_eq!(report.output, "PONG from the plan node.");
    assert_eq!(report.summary, "replied PONG");
    assert_eq!(
        report.session.as_deref(),
        Some("sess_3f1c2a9e-7b4d-4e21-9a55-0c8d6f2b1e47")
    );
    // raw keeps the full stdout for debugging.
    assert!(report.raw.contains("\"eventCount\""));

    let argv = recorded_argv(dir.path());
    assert_eq!(argv[0], "-p");
    assert!(
        argv[1].starts_with("You are terse.\n\n---\n\nreply PONG"),
        "SOUL travels as a prompt prefix: {:?}",
        argv[1]
    );
    assert_eq!(&argv[2..], &["--json", "--mode", "build"]);
    assert!(!argv.iter().any(|a| a == "yolo"), "never yolo implicitly");
}

#[test]
fn zcode_autonomy_switches_the_mode_to_yolo_last() {
    let dir = tempfile::tempdir().unwrap();
    let json = Path::new(FIXTURES).join("json_result.json");
    let ad = ClaudeAdapter {
        program: stub_zcode(dir.path(), &format!("cat '{}'", json.display())),
        spec: builtin("zcode").unwrap(),
    };
    ad.run(&task(dir.path(), &ConnectorEnvPolicy::default(), true))
        .unwrap();
    let argv = recorded_argv(dir.path());
    // Both modes appear; zcode's parser keeps the LAST, so yolo must follow.
    let modes: Vec<&str> = argv
        .windows(2)
        .filter(|w| w[0] == "--mode")
        .map(|w| w[1].as_str())
        .collect();
    assert_eq!(modes, vec!["build", "yolo"]);
}

#[test]
fn zcode_error_exit_is_process_exit_with_the_cli_message() {
    let dir = tempfile::tempdir().unwrap();
    let err_file = Path::new(FIXTURES).join("error_no_model.stderr");
    let ad = ClaudeAdapter {
        program: stub_zcode(
            dir.path(),
            &format!("cat '{}' >&2\nexit 1", err_file.display()),
        ),
        spec: builtin("zcode").unwrap(),
    };
    let (class, msg) = ad
        .run(&task(dir.path(), &ConnectorEnvPolicy::default(), false))
        .unwrap_err();
    assert_eq!(class, ErrorClass::ProcessExit);
    assert!(msg.contains("Model creation failed"), "{msg}");
}

/// A Z.ai quota stop must read as a budget failure, so the engine skips the
/// rest of that plan (and only that plan) in the fallback chain.
#[test]
fn zcode_quota_stop_classifies_as_budget() {
    let dir = tempfile::tempdir().unwrap();
    let err_file = Path::new(FIXTURES).join("error_quota.stderr");
    let ad = ClaudeAdapter {
        program: stub_zcode(
            dir.path(),
            &format!("cat '{}' >&2\nexit 1", err_file.display()),
        ),
        spec: builtin("zcode").unwrap(),
    };
    let (_, msg) = ad
        .run(&task(dir.path(), &ConnectorEnvPolicy::default(), false))
        .unwrap_err();
    assert_eq!(classify(&msg), FailureKind::Budget, "{msg}");
}

#[test]
fn capture_session_reads_zcode_json_and_stream_json() {
    let pretty = fixture("json_result.json");
    assert_eq!(
        capture_session("zcode", &pretty).as_deref(),
        Some("sess_3f1c2a9e-7b4d-4e21-9a55-0c8d6f2b1e47")
    );
    let stream = fixture("stream_json_result.jsonl");
    assert_eq!(
        capture_session("zcode", &stream).as_deref(),
        Some("sess_3f1c2a9e-7b4d-4e21-9a55-0c8d6f2b1e47")
    );
    assert_eq!(
        apb_core::zcode::response_text(&stream).as_deref(),
        Some("PONG from the plan node.")
    );
    assert_eq!(capture_session("zcode", "plain text reply\n"), None);
}

/// Restores the env a full-run zcode test mutates, under the shared env lock.
struct ZcodeRunEnv {
    _lock: std::sync::MutexGuard<'static, ()>,
    saved: Vec<(&'static str, Option<std::ffi::OsString>)>,
}

impl ZcodeRunEnv {
    fn set(vars: &[(&'static str, &Path)]) -> Self {
        let lock = common::env_lock();
        let mut saved = Vec::new();
        for (k, v) in vars {
            saved.push((*k, std::env::var_os(k)));
            unsafe { std::env::set_var(k, v) };
        }
        for k in [
            "APB_AGENT_CMD",
            apb_core::zcode::BUILTIN_CONFIG_ENV,
            apb_core::zcode::PERSONAL_CONFIG_ENV,
        ] {
            saved.push((k, std::env::var_os(k)));
            unsafe { std::env::remove_var(k) };
        }
        unsafe { std::env::set_var("APB_INFRA_BACKOFF_MS", "20,20") };
        saved.push(("APB_INFRA_BACKOFF_MS", None));
        Self { _lock: lock, saved }
    }
}

impl Drop for ZcodeRunEnv {
    fn drop(&mut self) {
        for (k, v) in self.saved.drain(..).rev() {
            match v {
                Some(v) => unsafe { std::env::set_var(k, v) },
                None => unsafe { std::env::remove_var(k) },
            }
        }
    }
}

/// A fake ZCode home whose headless CLI is logged in to BOTH the paid
/// individual plan and the free start plan.
fn fake_zcode_home(home: &Path) {
    let bundled = home.join(apb_core::zcode::HOME_REL_BUILTIN_CONFIG);
    fs::create_dir_all(bundled.parent().unwrap()).unwrap();
    fs::write(
        &bundled,
        r#"{"schemaVersion":1,"revision":30,"config":{"modelConfigRules":{"builtinProviderModelRules":[
            {"modelId":"GLM-5.3","config":{"enabled":true},"providerId":"account:zai-individual-coding-plan"},
            {"modelId":"GLM-5.3-Flash","config":{"enabled":true},"providerId":"account:zai-individual-coding-plan"},
            {"modelId":"GLM-5.3","config":{"enabled":true},"providerId":"account:zai-start-plan"}]}}}"#,
    )
    .unwrap();
    let v2 = home.join(".zcode/v2");
    fs::write(v2.join("setting.json"), r#"{"providerFamilyDomain":"zai"}"#).unwrap();
    fs::write(
        v2.join("credentials.json"),
        r#"{"account-provider:account:zai-individual-coding-plan:identity":"a","account-provider:account:zai-start-plan:identity":"b"}"#,
    )
    .unwrap();
}

const PLAN_FALLBACK_WF: &str = r#"
schema: 1
id: zplan
name: ZCode plan fallback
version: 1.0.0
defaults:
  profile: main
nodes:
  - { id: start, type: start }
  - { id: work, type: agent_task, prompt: "do" }
  - { id: done, type: finish, outcome: success }
  - { id: failed, type: finish, outcome: failure }
edges:
  - { from: start, to: work }
  - { from: work, to: done, condition: { type: node_status, node: work, equals: success } }
  - { from: work, to: failed, condition: { type: node_status, node: work, equals: failure } }
"#;

/// Cross-plan fallback: the paid plan's quota is exhausted, so the other
/// model on the SAME paid plan is skipped (same account, same quota) and the
/// same model on the free plan runs. The stub decides by the plan in the
/// run-scoped provider config apb hands it, which is also the proof that the
/// plan-qualified model reached zcode.
#[test]
fn a_paid_plan_quota_stop_falls_back_to_the_same_model_on_another_plan() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("proj");
    let home = dir.path().join("home");
    let cfg = dir.path().join("cfg");
    fs::create_dir_all(&root).unwrap();
    fs::create_dir_all(&cfg).unwrap();
    fake_zcode_home(&home);
    init_project_with(&root);
    common::seed_profile(
        &root,
        "main",
        "zcode",
        "zai-individual/GLM-5.3",
        &[
            ("zcode", "zai-individual/GLM-5.3-Flash"),
            ("zcode", "zai-start/GLM-5.3@max"),
        ],
    );
    let json = Path::new(FIXTURES).join("json_result.json");
    let quota = Path::new(FIXTURES).join("error_quota.stderr");
    let stub = dir.path().join("zcode-stub");
    common::write_sync(
        &stub,
        &format!(
            "#!/bin/sh\nif grep -q 'zai-individual-coding-plan' \"$ZCODE_PERSONAL_PROVIDER_CONFIG_FILE\"; then cat '{}' >&2; exit 1; fi\ngrep -q '\"reasoningLevel\": \"max\"' \"$ZCODE_PERSONAL_PROVIDER_CONFIG_FILE\" || exit 7\ncat '{}'\n",
            quota.display(),
            json.display()
        ),
    );
    let mut perm = fs::metadata(&stub).unwrap().permissions();
    perm.set_mode(0o755);
    fs::set_permissions(&stub, perm).unwrap();
    fs::write(
        cfg.join("config.yaml"),
        format!("agents:\n  zcode:\n    program: {}\n", stub.display()),
    )
    .unwrap();

    let _env = ZcodeRunEnv::set(&[("HOME", &home), ("APB_CONFIG_DIR", &cfg)]);
    let res = run(&root, "zplan", None, RunOptions::default()).unwrap();
    assert_eq!(res.outcome, RunStatus::Succeeded);

    let run_dir = root.join(".apb/runs").join(&res.run_id);
    let events = read_all(&run_dir).unwrap();
    let attempts: Vec<(String, Option<String>)> = events
        .iter()
        .filter_map(|e| match &e.payload {
            EventPayload::AttemptFinished {
                node,
                status,
                failure_kind,
                ..
            } if node == "work" => Some((status.clone(), failure_kind.clone())),
            _ => None,
        })
        .collect();
    assert_eq!(
        attempts,
        vec![
            ("failed".to_string(), Some("budget".to_string())),
            ("succeeded".to_string(), None),
        ],
        "the same-plan Flash step must be skipped, the free plan must run"
    );
    let fallbacks: Vec<(Option<String>, Option<String>)> = events
        .iter()
        .filter_map(|e| match &e.payload {
            EventPayload::FallbackTriggered {
                from_model,
                to_model,
                ..
            } => Some((from_model.clone(), to_model.clone())),
            _ => None,
        })
        .collect();
    assert_eq!(
        fallbacks,
        vec![(
            Some("zai-individual/GLM-5.3".to_string()),
            Some("zai-start/GLM-5.3@max".to_string())
        )]
    );
    let node_out = events.iter().find_map(|e| match &e.payload {
        EventPayload::NodeFinished { node, output, .. } if node == "work" => Some(output.clone()),
        _ => None,
    });
    assert_eq!(node_out.as_deref(), Some("PONG from the plan node."));
    // The user's own ZCode config was never written.
    assert!(
        !home
            .join(apb_core::zcode::HOME_REL_PERSONAL_CONFIG)
            .exists()
    );
}

fn init_project_with(root: &Path) {
    apb_core::registry::init_project(root).unwrap();
    let vdir = root.join(".apb/playbooks/zplan/1.0.0");
    fs::create_dir_all(&vdir).unwrap();
    fs::write(vdir.join("playbook.yaml"), PLAN_FALLBACK_WF).unwrap();
    fs::write(root.join(".apb/playbooks/zplan/current"), "1.0.0").unwrap();
}
