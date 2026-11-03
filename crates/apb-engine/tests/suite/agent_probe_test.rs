//! The paid probe behind `apb doctor --probe-agent`, on stub binaries only:
//! the recorded machine output conforms and keeps every promise, plain text
//! from the built-in form is a drift, plain text from a custom form is
//! allowed, the config is read under the agent name as given, the spawn
//! goes the way a run step's does (no autonomy flags, connector variables
//! withheld, scoped files and claude's minimal settings in the probe's own
//! directory, which is removed afterwards), the cancel flag stops the agent,
//! a run step is refused, and billing `deny` holds, a redirected config
//! dir's included.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use apb_core::agents::Conformance;
use apb_core::config::GlobalConfig;
use apb_core::step_env::StepCaller;
use apb_engine::agent_probe::{ProbeError, ProbeReport, ProbeRequest, probe, refusal};
use apb_engine::gate::launch::Initiator;

use crate::common::{self, EnvVar};

const AGENT: &str = "claude";

const RECORDED: &str =
    include_str!("../../../apb-core/tests/fixtures/agent_output/claude_result.json");

/// A variable a connector config references: the probed agent must never
/// get it.
const CONNECTOR_VAR: &str = "APB_PROBE_TEST_CONNECTOR_VAR";

/// A stub that records its argv, working directory and environment names
/// into `dir`, then prints `stdout`.
fn stub(dir: &Path, stdout: &str) -> String {
    stub_with(dir, stdout, "")
}

/// [`stub`] that runs `before` (shell) before it prints.
fn stub_with(dir: &Path, stdout: &str, before: &str) -> String {
    let out = dir.join("out.txt");
    fs::write(&out, stdout).unwrap();
    let path = dir.join("agent.sh");
    common::write_sync(
        &path,
        &format!(
            "#!/bin/sh\nprintf '%s\\n' \"$@\" > '{args}'\npwd > '{cwd}'\nenv | cut -d= -f1 > '{names}'\nprintf '%s' \"$CODEX_HOME\" > '{codex}'\n{before}\ncat '{out}'\n",
            args = dir.join("argv.txt").display(),
            cwd = dir.join("cwd.txt").display(),
            names = dir.join("env.txt").display(),
            codex = dir.join("codex_home.txt").display(),
            out = out.display(),
        ),
    );
    let mut p = fs::metadata(&path).unwrap().permissions();
    p.set_mode(0o755);
    fs::set_permissions(&path, p).unwrap();
    path.to_string_lossy().into_owned()
}

fn lines(path: PathBuf) -> Vec<String> {
    fs::read_to_string(path)
        .unwrap_or_default()
        .lines()
        .map(str::to_string)
        .collect()
}

/// A connector in config dir `cfg` whose account in project `root` reads
/// [`CONNECTOR_VAR`].
fn seed_connector(cfg: &Path, root: &Path) {
    let dir = cfg.join("connectors/probe-conn");
    fs::create_dir_all(&dir).unwrap();
    fs::write(
        dir.join("connector.yaml"),
        "name: probe-conn\nversion: 0.1.0\naccount_fields:\n  - name: token\n    required: true\n    secret: true\nfunctions:\n  - name: ping\n    description: ping\n    mock: { status: 200, body: {} }\n",
    )
    .unwrap();
    let accounts = root.join(".apb/connector-config");
    fs::create_dir_all(&accounts).unwrap();
    fs::write(
        accounts.join("probe-conn.yaml"),
        format!("accounts:\n  - name: a\n    token: \"{{{{env.{CONNECTOR_VAR}}}}}\"\n"),
    )
    .unwrap();
}

/// Runs the probe of `agent` under `config` with a hermetic home, config
/// dir and project, and the connector variable set.
fn probe_agent(agent: &str, config: &str, cancel: &AtomicBool) -> Result<ProbeReport, ProbeError> {
    probe_under(agent, config, cancel, &Machine::default())
}

/// The machine a probe runs on: the `policy.yaml` of the config dir it
/// reads (redirected away from the default one, as every probe here is),
/// the default config dir's, who asks (a person at a terminal unless said
/// otherwise), and whether an api key in the agent's environment makes its
/// call read as billed.
#[derive(Default)]
struct Machine<'a> {
    policy: Option<&'a str>,
    default_policy: Option<&'a str>,
    unattended: bool,
    api_key: bool,
}

/// [`probe_agent`] on `machine`.
fn probe_under(
    agent: &str,
    config: &str,
    cancel: &AtomicBool,
    machine: &Machine<'_>,
) -> Result<ProbeReport, ProbeError> {
    let _env = common::env_lock();
    let sandbox = tempfile::tempdir().unwrap();
    let home = sandbox.path().join("home");
    let cfg = sandbox.path().join("config");
    let default_cfg = sandbox.path().join("default-config");
    let project = sandbox.path().join("project");
    for d in [&home, &cfg, &default_cfg, &project] {
        fs::create_dir_all(d).unwrap();
    }
    seed_connector(&cfg, &project);
    for (dir, policy) in [
        (&cfg, machine.policy),
        (&default_cfg, machine.default_policy),
    ] {
        if let Some(policy) = policy {
            fs::write(dir.join("policy.yaml"), policy).unwrap();
        }
    }
    let _guards = [
        EnvVar::remove("APB_AGENT_CMD"),
        EnvVar::remove("CODEX_HOME"),
        EnvVar::set("HOME", &home),
        EnvVar::set("APB_CONFIG_DIR", &cfg),
        EnvVar::set("APB_TEST_DEFAULT_CONFIG_DIR", &default_cfg),
        EnvVar::set(CONNECTOR_VAR, "1"),
        if machine.api_key {
            EnvVar::set("ANTHROPIC_API_KEY", "k")
        } else {
            EnvVar::remove("ANTHROPIC_API_KEY")
        },
    ];
    let global: GlobalConfig = yaml_serde::from_str(config).unwrap();
    let initiator = Initiator::CliUnattested {
        terminal: !machine.unattended,
    };
    let req = ProbeRequest {
        agent,
        model: Some("haiku"),
        timeout: Duration::from_secs(10),
        project: &project,
        initiator: &initiator,
    };
    probe(&req, &global, cancel)
}

fn probe_with(config: &str) -> ProbeReport {
    probe_agent(AGENT, config, &AtomicBool::new(false)).expect("the probe runs")
}

#[test]
fn recorded_machine_output_conforms_and_keeps_every_promise() {
    let dir = tempfile::tempdir().unwrap();
    let program = stub(dir.path(), RECORDED);
    let report = probe_with(&format!("agents:\n  {AGENT}: {{ program: {program} }}\n"));
    assert!(report.form_is_builtin);
    assert_eq!(report.conformance, Conformance::Machine);
    assert!(!report.promises.is_empty());
    assert!(
        report.promises.iter().all(|p| p.kept),
        "{:?}",
        report.promises
    );
    assert!(!report.drifted());
    assert_eq!(report.usage.as_ref().map(|u| u.output_tokens), Some(443));
}

/// The spawn is a run step's without autonomy: the autonomy flags are left
/// out (and reported), claude gets its minimal-environment settings, the
/// connector variable is withheld, and the probe's directory is gone after.
#[test]
fn the_probe_spawns_like_a_run_step_without_autonomy() {
    let dir = tempfile::tempdir().unwrap();
    let program = stub(dir.path(), RECORDED);
    let report = probe_with(&format!("agents:\n  {AGENT}: {{ program: {program} }}\n"));
    let form = apb_engine::invocation::builtin(AGENT).unwrap();
    assert!(!form.autonomous_args.is_empty());
    assert_eq!(report.omitted_flags, form.autonomous_args);
    let argv = lines(dir.path().join("argv.txt"));
    assert!(
        !form.autonomous_args.iter().any(|f| argv.contains(f)),
        "{argv:?}"
    );
    assert!(argv.iter().any(|a| a == "--settings"), "{argv:?}");
    let env = lines(dir.path().join("env.txt"));
    assert!(env.iter().any(|n| n == "PATH"));
    assert!(!env.iter().any(|n| n == CONNECTOR_VAR));
    let cwd = lines(dir.path().join("cwd.txt"));
    let cwd = Path::new(cwd.first().expect("the stub ran"));
    assert!(!cwd.exists(), "the probe's directory is removed");
}

/// codex gets its isolated home inside the probe's own directory, never the
/// user's codex home.
#[test]
fn codex_gets_an_isolated_home_in_the_probe_directory() {
    let dir = tempfile::tempdir().unwrap();
    let program = stub(dir.path(), "OK\n");
    probe_agent(
        "codex",
        &format!("agents:\n  codex: {{ program: {program} }}\n"),
        &AtomicBool::new(false),
    )
    .expect("the probe runs");
    let cwd = lines(dir.path().join("cwd.txt"));
    let probe_dir = Path::new(cwd.first().expect("the stub ran"))
        .parent()
        .unwrap();
    let codex_home = fs::read_to_string(dir.path().join("codex_home.txt")).unwrap();
    assert!(!codex_home.is_empty());
    // Compared by the probe directory's unique name: on macOS the temp dir
    // is reached through the `/var` -> `/private/var` link, so the stub's
    // working directory and the home apb passed differ in their prefix.
    let probe_name = probe_dir.file_name().unwrap();
    assert!(
        Path::new(&codex_home)
            .components()
            .any(|c| c.as_os_str() == probe_name),
        "{codex_home}"
    );
    assert!(!Path::new(&codex_home).starts_with(cwd.first().unwrap()));
    assert!(!probe_dir.exists(), "the probe's directory is removed");
}

/// A config keyed by an alias is read under that alias: its program and
/// form run, and the custom form is reported as such.
#[test]
fn an_alias_config_is_read_under_the_alias() {
    let dir = tempfile::tempdir().unwrap();
    let program = stub(dir.path(), RECORDED);
    let report = probe_agent(
        "claude-code",
        &format!("agents:\n  claude-code: {{ program: {program} }}\n"),
        &AtomicBool::new(false),
    )
    .expect("the alias's program runs");
    assert!(report.form_is_builtin);
    assert_eq!(report.conformance, Conformance::Machine);

    let custom = tempfile::tempdir().unwrap();
    let program = stub(custom.path(), "OK\n");
    let report = probe_agent(
        "claude-code",
        &format!(
            "agents:\n  claude-code:\n    program: {program}\n    invocation:\n      argv: [\"--model\", \"{{model}}\", \"{{prompt}}\"]\n"
        ),
        &AtomicBool::new(false),
    )
    .expect("the alias's custom form runs");
    assert!(!report.form_is_builtin);
    assert_eq!(report.conformance, Conformance::PlainByDesign);
    assert_eq!(
        lines(custom.path().join("argv.txt"))
            .first()
            .map(String::as_str),
        Some("--model")
    );
}

/// The cancel flag (set by SIGINT or SIGTERM in the CLI) stops the agent
/// well before the timeout, and the probe's directory is removed.
#[test]
fn the_cancel_flag_stops_the_agent() {
    let dir = tempfile::tempdir().unwrap();
    let program = stub_with(dir.path(), RECORDED, "sleep 30");
    let cancel = AtomicBool::new(false);
    let started = Instant::now();
    let result = std::thread::scope(|s| {
        s.spawn(|| {
            let deadline = Instant::now() + Duration::from_secs(5);
            while !dir.path().join("cwd.txt").exists() && Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(20));
            }
            cancel.store(true, Ordering::SeqCst);
        });
        probe_agent(
            AGENT,
            &format!("agents:\n  {AGENT}: {{ program: {program} }}\n"),
            &cancel,
        )
    });
    let err = result.expect_err("a cancelled probe has no report");
    assert_eq!(err.0, apb_engine::agent_probe::INTERRUPTED);
    assert!(started.elapsed() < Duration::from_secs(9));
    let cwd = lines(dir.path().join("cwd.txt"));
    assert!(!Path::new(cwd.first().expect("the stub ran")).exists());
}

#[test]
fn plain_text_from_the_built_in_form_is_a_drift() {
    let dir = tempfile::tempdir().unwrap();
    let program = stub(dir.path(), "OK\n");
    let report = probe_with(&format!("agents:\n  {AGENT}: {{ program: {program} }}\n"));
    assert_eq!(report.conformance, Conformance::Drifted);
    assert!(report.promises.iter().any(|p| !p.kept));
    assert!(report.drifted());
}

#[test]
fn plain_text_from_a_custom_form_is_allowed() {
    let dir = tempfile::tempdir().unwrap();
    let program = stub(dir.path(), "OK\n");
    let report = probe_with(&format!(
        "agents:\n  {AGENT}:\n    program: {program}\n    invocation:\n      argv: [\"--model\", \"{{model}}\", \"{{prompt}}\"]\n"
    ));
    assert!(!report.form_is_builtin);
    assert_eq!(report.conformance, Conformance::PlainByDesign);
    assert!(report.promises.is_empty());
    assert!(!report.drifted());
}

#[test]
fn a_run_step_is_refused_and_a_person_is_not() {
    assert_eq!(refusal(&StepCaller::default()), None);
    let step = StepCaller {
        run_id: Some("r1".into()),
        host_task: None,
    };
    let why = refusal(&step).expect("refused");
    assert!(why.contains("inside run `r1`"), "{why}");
}

/// `billed: deny` holds for a person too: a probe whose call reads as
/// billed (an api key in the agent's environment) is refused before the
/// agent is spawned.
#[test]
fn a_billed_probe_is_refused_under_deny() {
    let dir = tempfile::tempdir().unwrap();
    let program = stub(dir.path(), RECORDED);
    let err = probe_under(
        AGENT,
        &format!("agents:\n  {AGENT}: {{ program: {program} }}\n"),
        &AtomicBool::new(false),
        &Machine {
            policy: Some("billing:\n  billed: deny\n"),
            api_key: true,
            ..Machine::default()
        },
    )
    .expect_err("refused");
    assert!(err.0.starts_with("billing_denied"), "{err}");
    assert!(!dir.path().join("argv.txt").exists(), "nothing was spawned");
}

/// A redirected config dir does not hide the default one's `billed: deny`
/// from a CLI without a terminal, exactly as at a launch: the billed probe
/// is refused before the agent is spawned. A person at a terminal is not
/// held to the default dir, as at a launch, and the probe runs.
#[test]
fn a_redirected_config_dir_does_not_hide_the_default_dirs_deny() {
    for (unattended, refused) in [(true, true), (false, false)] {
        let dir = tempfile::tempdir().unwrap();
        let program = stub(dir.path(), RECORDED);
        let out = probe_under(
            AGENT,
            &format!("agents:\n  {AGENT}: {{ program: {program} }}\n"),
            &AtomicBool::new(false),
            &Machine {
                default_policy: Some("billing:\n  billed: deny\n"),
                unattended,
                api_key: true,
                ..Machine::default()
            },
        );
        let spawned = dir.path().join("argv.txt").exists();
        if refused {
            let err = out.expect_err("refused");
            assert!(err.0.starts_with("billing_denied"), "{err}");
            assert!(err.0.contains("default config dir"), "{err}");
            assert!(!spawned, "nothing was spawned");
        } else {
            out.expect("a person at a terminal runs the probe");
            assert!(spawned, "the agent was spawned");
        }
    }
}
