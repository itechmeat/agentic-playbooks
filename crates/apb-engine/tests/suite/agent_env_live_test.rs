//! Live check that each installed agent CLI still answers, and resumes its
//! session, under `process_env: allowlist`. Ignored by default; run with
//! `APB_LIVE_TEST_AGENT_ENV=claude,codex,... cargo test -p apb-engine --test
//! main agent_env_live_test -- --ignored`. Each listed agent needs its CLI on
//! PATH and a working login. Models come from
//! `APB_LIVE_TEST_AGENT_ENV_MODEL_<AGENT>` (upper case), else a default.

use std::sync::Arc;
use std::time::Duration;

use apb_core::process_env::ProcessEnvPolicy;
use apb_engine::adapter::{AgentAdapter, AgentTask, ClaudeAdapter, SpawnEnv};
use apb_engine::invocation::{builtin, resume_spec};

fn default_model(agent: &str) -> &'static str {
    match agent {
        "claude" => "claude-haiku-4-5",
        "codex" => "gpt-6-sol",
        "zcode" => "GLM-5.3-Flash",
        _ => "",
    }
}

fn ask(
    agent: &str,
    model: &str,
    spec: apb_core::config::InvocationDef,
    env: &SpawnEnv,
    workdir: &std::path::Path,
) -> apb_engine::adapter::AgentReport {
    let adapter = ClaudeAdapter {
        program: apb_core::detect::default_program(agent),
        spec,
    };
    let task = AgentTask {
        prompt: "Reply with the single word PONG and nothing else.",
        model,
        workdir,
        timeout: Some(Duration::from_secs(300)),
        stream_log: None,
        soul: None,
        grant_autonomy: false,
        spawn_env: env,
        interactive: false,
        report_contract: false,
        node: "live",
        agent,
        extract: None,
        status_file: None,
        hermetic_settings: None,
        transcript_dir: None,
        output_limits: Default::default(),
    };
    match adapter.run(&task) {
        Ok(report) => report,
        Err(f) => panic!("{agent}: the attempt failed under allowlist: {}", f.message),
    }
}

#[test]
#[ignore = "needs installed, logged-in agent CLIs; set APB_LIVE_TEST_AGENT_ENV"]
fn installed_agents_answer_and_resume_under_an_allowlist() {
    // Run with --ignored but without the list: fail loudly, a silent pass
    // would read as a passed live check.
    let list = std::env::var("APB_LIVE_TEST_AGENT_ENV").expect(
        "set APB_LIVE_TEST_AGENT_ENV to a comma-separated agent list (claude,codex,...) to run this live test",
    );
    let policy = ProcessEnvPolicy::from_surface(Some("allowlist"), &[])
        .unwrap()
        .unwrap();
    for agent in list.split(',').map(str::trim).filter(|a| !a.is_empty()) {
        let model = std::env::var(format!(
            "APB_LIVE_TEST_AGENT_ENV_MODEL_{}",
            agent.to_ascii_uppercase()
        ))
        .unwrap_or_else(|_| default_model(agent).to_string());
        // Outside the system temp dir: codex refuses helper binaries there.
        let run_dir = tempfile::tempdir_in(env!("CARGO_TARGET_TMPDIR")).unwrap();
        // A git work tree, which codex requires of its working directory.
        let git = std::process::Command::new("git")
            .args(["init", "-q"])
            .current_dir(run_dir.path())
            .status()
            .expect("git");
        assert!(git.success());
        let plan = apb_engine::agent_env::plan_for(agent, &model, &policy, &[]);
        let env = SpawnEnv::for_node(Vec::new(), run_dir.path(), "live", Vec::new())
            .with_attempt(1)
            .with_process(Some(Arc::new(plan)));
        let spec = builtin(agent).unwrap_or_else(|| panic!("{agent}: no built-in form"));
        let first = ask(agent, &model, spec.clone(), &env, run_dir.path());
        assert!(
            first.output.contains("PONG"),
            "{agent}: unexpected reply under allowlist"
        );
        let Some(session) = first.session else {
            continue;
        };
        if let Some(resume) = resume_spec(&spec, agent, &session) {
            let again = ask(agent, &model, resume, &env.with_attempt(2), run_dir.path());
            assert!(
                again.output.contains("PONG"),
                "{agent}: unexpected reply on resume under allowlist"
            );
        }
    }
}
