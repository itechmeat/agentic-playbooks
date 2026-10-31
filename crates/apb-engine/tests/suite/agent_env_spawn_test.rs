//! The process environment every built-in agent is spawned with under a
//! profile's `process_env`, in its start form and its resume form, and the
//! background supervisor's. A stub binary records the names of its
//! environment; the assertions name variables, never their values.

use std::collections::BTreeSet;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

use apb_core::process_env::ProcessEnvPolicy;
use apb_engine::adapter::{AgentAdapter, AgentTask, ClaudeAdapter, SpawnEnv};
use apb_engine::invocation::{builtin, resume_spec};

use crate::common::{self, EnvVar};

/// The operator variable no agent may see under an allowlist.
const OPERATOR_CANARY: &str = "APB_TEST_OPERATOR_SECRET";
/// The name the profile's `allow` adds.
const ALLOWED_CANARY: &str = "APB_TEST_PROFILE_ALLOWED";
/// A name the profile allows and a connector references: the scrub wins.
const SCRUBBED_CANARY: &str = "APB_TEST_CONNECTOR_TOKEN";

/// Each built-in agent, a model it accepts and a variable of its own set.
const AGENTS: &[(&str, &str, &str)] = &[
    ("claude", "m", "ANTHROPIC_APB_TEST_CANARY"),
    ("agy", "m", "AGY_APB_TEST_CANARY"),
    ("codex", "m", "CODEX_APB_TEST_CANARY"),
    ("opencode", "openrouter/m", "OPENCODE_APB_TEST_CANARY"),
    ("hermes", "m", "HERMES_APB_TEST_CANARY"),
    ("grok", "m", "GROK_APB_TEST_CANARY"),
    ("cursor", "m", "CURSOR_APB_TEST_CANARY"),
    ("qoder", "m", "QODER_APB_TEST_CANARY"),
    // Empty: no zcode plan selection, so no zcode login is needed.
    ("zcode", "", "ZCODE_APB_TEST_CANARY"),
];

/// A stub agent writing the NUL-separated `env -0` of its process to
/// `dump` (atomically, so a reader never sees half a file).
fn stub(dir: &Path, dump: &Path) -> String {
    let path = dir.join("agent");
    let tmp = dump.with_extension("tmp");
    common::write_sync(
        &path,
        &format!(
            "#!/bin/sh\nenv -0 > '{}'\nmv '{}' '{}'\necho done\n",
            tmp.display(),
            tmp.display(),
            dump.display()
        ),
    );
    let mut perm = fs::metadata(&path).unwrap().permissions();
    perm.set_mode(0o755);
    fs::set_permissions(&path, perm).unwrap();
    path.to_string_lossy().into_owned()
}

/// The variable names in a dump.
fn names(dump: &Path) -> BTreeSet<String> {
    fs::read(dump)
        .unwrap()
        .split(|b| *b == 0)
        .filter_map(|entry| {
            let text = String::from_utf8_lossy(entry);
            text.split_once('=').map(|(k, _)| k.to_string())
        })
        .collect()
}

/// The value of `name` in a dump, for a variable that is not a secret.
fn value(dump: &Path, name: &str) -> Option<String> {
    fs::read(dump)
        .unwrap()
        .split(|b| *b == 0)
        .find_map(|entry| {
            let text = String::from_utf8_lossy(entry);
            let (k, v) = text.split_once('=')?;
            (k == name).then(|| v.to_string())
        })
}

fn allowlist() -> ProcessEnvPolicy {
    ProcessEnvPolicy::from_surface(
        Some("allowlist"),
        &[ALLOWED_CANARY.to_string(), SCRUBBED_CANARY.to_string()],
    )
    .unwrap()
    .unwrap()
}

/// Every canary set in the process env for the life of the guards, with
/// HOME and CODEX_HOME pointing at the temp `home` so no operator config is
/// read.
fn canaries(home: &Path) -> Vec<EnvVar> {
    let mut guards = vec![
        EnvVar::set("HOME", home),
        EnvVar::set("CODEX_HOME", home.join(".codex")),
        EnvVar::set(OPERATOR_CANARY, "1"),
        EnvVar::set(ALLOWED_CANARY, "1"),
        EnvVar::set(SCRUBBED_CANARY, "1"),
    ];
    guards.extend(AGENTS.iter().map(|(_, _, canary)| EnvVar::set(canary, "1")));
    guards
}

struct Spawned {
    names: BTreeSet<String>,
}

impl Spawned {
    fn has(&self, name: &str) -> bool {
        self.names.contains(name)
    }
}

/// Runs `agent` once through `program` with `spec` and the env `env`.
fn spawn(
    program: &str,
    spec: apb_core::config::InvocationDef,
    agent: &str,
    model: &str,
    env: &SpawnEnv,
    workdir: &Path,
    dump: &Path,
) -> Spawned {
    let _ = fs::remove_file(dump);
    let adapter = ClaudeAdapter {
        program: program.to_string(),
        spec,
    };
    let task = AgentTask {
        prompt: "go",
        model,
        workdir,
        timeout: None,
        stream_log: None,
        soul: None,
        grant_autonomy: false,
        spawn_env: env,
        interactive: false,
        report_contract: false,
        node: "n",
        agent,
        extract: None,
        status_file: Some(workdir.join("status.json")),
        hermetic_settings: None,
        transcript_dir: None,
        output_limits: Default::default(),
    };
    // The reply does not matter, only what the process was given.
    let _ = adapter.run(&task);
    Spawned { names: names(dump) }
}

fn run_env(run_dir: &Path, process: Option<Arc<apb_core::process_env::EnvPlan>>) -> SpawnEnv {
    SpawnEnv::for_node(
        vec![SCRUBBED_CANARY.to_string()],
        run_dir,
        "n",
        vec![("APB_TEST_EVAL_OVERLAY".to_string(), "1".to_string())],
    )
    .with_attempt(1)
    .with_process(process)
}

#[test]
fn every_agent_gets_only_its_allowlist_in_start_and_resume_forms() {
    let _l = common::env_lock();
    let home = tempfile::tempdir().unwrap();
    let _env = canaries(home.path());
    let bin = tempfile::tempdir().unwrap();
    let run_dir = tempfile::tempdir().unwrap();
    let dump = bin.path().join("env.dump");
    let program = stub(bin.path(), &dump);
    let policy = allowlist();
    let scrub = vec![SCRUBBED_CANARY.to_string()];
    for (agent, model, own) in AGENTS {
        let plan = apb_engine::agent_env::plan_for(agent, model, &policy, &scrub);
        let env = run_env(run_dir.path(), Some(Arc::new(plan)));
        let base = builtin(agent).unwrap_or_else(|| panic!("{agent}: no built-in form"));
        let resume = resume_spec(&base, agent, "session-1");
        if *agent == "agy" {
            assert!(resume.is_none(), "agy has no resume form");
        }
        let forms = std::iter::once(("start", base.clone())).chain(resume.map(|r| ("resume", r)));
        for (form, spec) in forms {
            let got = spawn(&program, spec, agent, model, &env, run_dir.path(), &dump);
            let ctx = format!("{agent} ({form})");
            for name in ["PATH", "HOME", own, ALLOWED_CANARY] {
                assert!(got.has(name), "{ctx}: `{name}` missing");
            }
            for name in [
                "APB_RUN_DIR",
                "APB_NODE_ID",
                "APB_ATTEMPT_TOKEN",
                "APB_STATUS_FILE",
                "APB_TEST_EVAL_OVERLAY",
            ] {
                assert!(got.has(name), "{ctx}: `{name}` missing");
            }
            assert!(
                !got.has(OPERATOR_CANARY),
                "{ctx}: the operator canary leaked"
            );
            assert!(!got.has(SCRUBBED_CANARY), "{ctx}: a connector name leaked");
            if *agent == "codex" {
                assert!(got.has("CODEX_HOME"), "{ctx}: CODEX_HOME missing");
            }
            if *agent == "zcode" {
                assert!(
                    got.has(apb_core::zcode::PERSONAL_CONFIG_ENV),
                    "{ctx}: the zcode provider config is missing"
                );
            }
            // Another agent's set never leaks into this one's.
            for (other, _, other_own) in AGENTS {
                if other != agent {
                    assert!(
                        !got.has(other_own),
                        "{ctx}: `{other_own}` of {other} leaked"
                    );
                }
            }
        }
    }
}

#[test]
fn inherit_passes_the_operator_environment_on() {
    let _l = common::env_lock();
    let home = tempfile::tempdir().unwrap();
    let _env = canaries(home.path());
    let bin = tempfile::tempdir().unwrap();
    let run_dir = tempfile::tempdir().unwrap();
    let dump = bin.path().join("env.dump");
    let program = stub(bin.path(), &dump);
    let env = run_env(run_dir.path(), None);
    let got = spawn(
        &program,
        builtin("claude").unwrap(),
        "claude",
        "m",
        &env,
        run_dir.path(),
        &dump,
    );
    assert!(
        got.has(OPERATOR_CANARY),
        "inherit must keep the operator env"
    );
    assert!(got.has("CODEX_APB_TEST_CANARY"));
    assert!(
        !got.has(SCRUBBED_CANARY),
        "the connector scrub applies under inherit too"
    );
}

#[test]
fn the_supervisor_spawn_gets_the_allowlist_and_its_own_variables() {
    let _l = common::env_lock();
    let home = tempfile::tempdir().unwrap();
    let _env = canaries(home.path());
    let _timeout = EnvVar::set("MCP_TOOL_TIMEOUT", "7");
    let bin = tempfile::tempdir().unwrap();
    let run_dir = tempfile::tempdir().unwrap();
    let dump = bin.path().join("env.dump");
    let program = stub(bin.path(), &dump);
    let plan =
        apb_engine::agent_env::plan_for("codex", "m", &allowlist(), &[SCRUBBED_CANARY.to_string()]);
    let env = SpawnEnv::for_node(
        vec![SCRUBBED_CANARY.to_string()],
        run_dir.path(),
        "supervisor",
        Vec::new(),
    )
    .with_process(Some(Arc::new(plan)));
    let adapter = ClaudeAdapter {
        program,
        spec: builtin("codex").unwrap(),
    };
    adapter
        .spawn_supervisor("brief", "m", run_dir.path(), None, &env)
        .expect("spawned");
    // The supervisor is detached: wait for its dump to appear.
    let deadline = Instant::now() + Duration::from_secs(10);
    while !dump.is_file() {
        assert!(Instant::now() < deadline, "the supervisor stub never ran");
        std::thread::sleep(Duration::from_millis(20));
    }
    let got = names(&dump);
    assert!(got.contains("CODEX_APB_TEST_CANARY"));
    assert!(got.contains(ALLOWED_CANARY));
    assert!(got.contains(apb_engine::adapter::MCP_ROLE_ENV));
    // The operator set it, but a codex allowlist withholds it, so apb sets
    // its own instead of leaving the supervisor without one.
    assert_eq!(
        value(&dump, "MCP_TOOL_TIMEOUT").as_deref(),
        Some(
            apb_engine::adapter::SUPERVISOR_MCP_TOOL_TIMEOUT_MS
                .to_string()
                .as_str()
        )
    );
    assert!(!got.contains(OPERATOR_CANARY), "the operator canary leaked");
    assert!(!got.contains(SCRUBBED_CANARY), "a connector name leaked");
}

/// On Linux an allowlisted agent cannot read apb's own environment through
/// `/proc/$PPID/environ`, while `stat` and `ps` (what apb's own liveness
/// probes read) keep working.
#[cfg(target_os = "linux")]
#[test]
fn an_allowlisted_agent_cannot_read_the_parents_environ() {
    let _l = common::env_lock();
    let home = tempfile::tempdir().unwrap();
    let _env = canaries(home.path());
    let plan = apb_engine::agent_env::plan_for("claude", "m", &allowlist(), &[]);
    let env = SpawnEnv::default().with_process(Some(Arc::new(plan)));
    let mut cmd = std::process::Command::new("sh");
    cmd.arg("-c").arg(
        "[ \"$(id -u)\" = 0 ] && echo root\n\
         cat /proc/$PPID/environ >/dev/null 2>&1 && echo environ-readable\n\
         cat /proc/$PPID/stat >/dev/null && echo stat-ok\n\
         ps -o stat= -o args= -p $PPID >/dev/null && echo ps-ok\n\
         exit 0",
    );
    env.apply(&mut cmd);
    let out = cmd.output().expect("spawn sh");
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(text.contains("stat-ok"), "stat unreadable: {text}");
    assert!(text.contains("ps-ok"), "ps failed: {text}");
    // Root holds CAP_SYS_PTRACE and reads any environ: nothing to prove.
    if !text.contains("root") {
        assert!(
            !text.contains("environ-readable"),
            "the agent read apb's environment"
        );
    }
}

/// A cloud trigger counts only with a truthy value: `=0` and `=false` keep
/// the provider's credentials withheld, `=1` passes them.
#[test]
fn a_cloud_trigger_needs_a_truthy_value() {
    let _l = common::env_lock();
    let home = tempfile::tempdir().unwrap();
    let _env = canaries(home.path());
    let _aws = EnvVar::set("AWS_APB_TEST_CANARY", "1");
    for (value, kept) in [("0", false), ("false", false), ("1", true), ("True", true)] {
        let _trigger = EnvVar::set("CLAUDE_CODE_USE_BEDROCK", value);
        let plan = apb_engine::agent_env::plan_for("claude", "m", &allowlist(), &[]);
        assert_eq!(
            plan.keeps("AWS_APB_TEST_CANARY"),
            kept,
            "CLAUDE_CODE_USE_BEDROCK={value}"
        );
    }
}
