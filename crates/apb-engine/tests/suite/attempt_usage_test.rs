//! Token usage per agent attempt (issue #167): the built-in claude form asks
//! for the CLI's JSON result, the attempt journals the usage it reports, the
//! node output is the unwrapped reply, and the run view totals it. An agent
//! printing plain text journals no usage at all.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;

use apb_core::agent_output::{AgentUsage, CostBasis, UsageSource};
use apb_core::registry::init_project;
use apb_engine::event::{EventPayload, read_all};
use apb_engine::run_view::{RunUsage, RunView};
use apb_engine::scheduler::{RunOptions, run};
use apb_engine::state::RunStatus;

use crate::common;

const PLAYBOOK: &str = r#"
schema: 1
id: u
name: Usage
version: 1.0.0
defaults:
  profile: main
nodes:
  - { id: start, type: start }
  - { id: w, type: agent_task, prompt: "do" }
  - { id: ok, type: finish, outcome: success }
  - { id: no, type: finish, outcome: failure }
edges:
  - { from: start, to: w }
  - { from: w, to: ok, condition: { type: node_status, node: w, equals: success } }
  - { from: w, to: no, fallback: true }
"#;

/// Restores `APB_AGENT_CMD` on drop, panics included.
struct AgentCmd(Option<std::ffi::OsString>);

impl AgentCmd {
    fn set(prog: &str) -> Self {
        let old = std::env::var_os("APB_AGENT_CMD");
        unsafe { std::env::set_var("APB_AGENT_CMD", prog) };
        AgentCmd(old)
    }
}

impl Drop for AgentCmd {
    fn drop(&mut self) {
        match self.0.take() {
            Some(v) => unsafe { std::env::set_var("APB_AGENT_CMD", v) },
            None => unsafe { std::env::remove_var("APB_AGENT_CMD") },
        }
    }
}

fn seed(root: &Path) {
    init_project(root).unwrap();
    let dir = root.join(".apb/playbooks/u/1.0.0");
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("playbook.yaml"), PLAYBOOK).unwrap();
    fs::write(root.join(".apb/playbooks/u/current"), "1.0.0").unwrap();
    common::seed_main(root);
}

/// A stub agent that records its argv and prints `stdout` verbatim.
fn stub(root: &Path, stdout: &str) -> String {
    fs::write(root.join("stdout.txt"), stdout).unwrap();
    let path = root.join("agent.sh");
    common::write_sync(
        &path,
        &format!(
            "#!/bin/sh\nprintf '%s\\n' \"$@\" > '{argv}'\ncat '{out}'\n",
            argv = root.join("argv.txt").display(),
            out = root.join("stdout.txt").display(),
        ),
    );
    let mut p = fs::metadata(&path).unwrap().permissions();
    p.set_mode(0o755);
    fs::set_permissions(&path, p).unwrap();
    path.to_string_lossy().to_string()
}

/// [`stub`], exiting 1 after printing, as claude does on an errored result.
fn failing_stub(root: &Path, stdout: &str) -> String {
    let prog = stub(root, stdout);
    let failing = root.join("failing.sh");
    common::write_sync(&failing, &format!("#!/bin/sh\n'{prog}' \"$@\"\nexit 1\n"));
    let mut p = fs::metadata(&failing).unwrap().permissions();
    p.set_mode(0o755);
    fs::set_permissions(&failing, p).unwrap();
    failing.to_string_lossy().to_string()
}

fn claude_result(is_error: bool) -> String {
    serde_json::json!({
        "type": "result",
        "subtype": "success",
        "is_error": is_error,
        "result": "the work\n\n```yaml\nstatus: success\nsummary: done\n```",
        "session_id": "0f5b6a52-4f7e-4a8e-9c1d-2b3c4d5e6f70",
        "total_cost_usd": 0.0125,
        "usage": {"input_tokens": 1, "output_tokens": 1},
        "modelUsage": {
            "claude-sonnet": {"inputTokens": 120, "outputTokens": 45, "cacheReadInputTokens": 3000, "cacheCreationInputTokens": 200},
            "claude-haiku": {"inputTokens": 30, "outputTokens": 5, "cacheReadInputTokens": 0, "cacheCreationInputTokens": 0}
        }
    })
    .to_string()
}

fn attempt_usages(root: &Path, run_id: &str) -> Vec<Option<AgentUsage>> {
    read_all(&root.join(".apb/runs").join(run_id))
        .unwrap()
        .into_iter()
        .filter_map(|e| match e.payload {
            EventPayload::AttemptFinished { usage, .. } => Some(usage),
            _ => None,
        })
        .collect()
}

#[test]
fn a_claude_json_result_journals_its_usage_and_the_unwrapped_reply() {
    let _env = common::env_lock();
    let dir = tempfile::tempdir().unwrap();
    seed(dir.path());
    let _cmd = AgentCmd::set(&stub(dir.path(), &claude_result(false)));

    let res = run(dir.path(), "u", None, RunOptions::default()).unwrap();
    assert_eq!(res.outcome, RunStatus::Succeeded);

    let argv = fs::read_to_string(dir.path().join("argv.txt")).unwrap();
    assert!(
        argv.contains("--output-format\njson\n"),
        "claude must be asked for its JSON result: {argv}"
    );
    let expected = AgentUsage {
        input_tokens: 150,
        output_tokens: 50,
        cache_read_tokens: 3000,
        cache_write_tokens: 200,
        cost_usd: Some(0.0125),
        source: UsageSource::Reported,
        cost_basis: CostBasis::Unknown,
    };
    assert_eq!(
        attempt_usages(dir.path(), &res.run_id),
        vec![Some(expected)]
    );

    let run_dir = dir.path().join(".apb/runs").join(&res.run_id);
    let view = RunView::load(&run_dir, &res.run_id).unwrap();
    // The node output is the reply, not the JSON envelope around it.
    assert_eq!(view.state.outputs["w"], "the work");
    assert_eq!(
        view.usage(),
        Some(RunUsage {
            attempts: 1,
            input_tokens: 150,
            output_tokens: 50,
            cache_read_tokens: 3000,
            cache_write_tokens: 200,
            cost_usd: Some(0.0125),
            cost_attempts: 1,
            cost_by_basis: apb_engine::run_view::CostByBasis {
                unknown_basis_usd: Some(0.0125),
                ..Default::default()
            },
            estimated: false,
            finished_attempts: 1,
        })
    );
}

#[test]
fn a_claude_result_marked_as_an_error_fails_the_attempt() {
    let _env = common::env_lock();
    let dir = tempfile::tempdir().unwrap();
    seed(dir.path());
    let _cmd = AgentCmd::set(&stub(dir.path(), &claude_result(true)));

    let res = run(dir.path(), "u", None, RunOptions::default()).unwrap();
    // The report block says success, but the CLI itself flagged the result.
    assert_eq!(res.outcome, RunStatus::Failed);
    assert!(attempt_usages(dir.path(), &res.run_id)[0].is_some());
}

#[test]
fn plain_text_output_journals_no_usage() {
    let _env = common::env_lock();
    let dir = tempfile::tempdir().unwrap();
    seed(dir.path());
    let _cmd = AgentCmd::set(&stub(
        dir.path(),
        "plain reply\n\n```yaml\nstatus: success\nsummary: done\n```",
    ));

    let res = run(dir.path(), "u", None, RunOptions::default()).unwrap();
    assert_eq!(res.outcome, RunStatus::Succeeded);
    assert_eq!(attempt_usages(dir.path(), &res.run_id), vec![None]);
    let run_dir = dir.path().join(".apb/runs").join(&res.run_id);
    let view = RunView::load(&run_dir, &res.run_id).unwrap();
    assert_eq!(view.state.outputs["w"], "plain reply");
    assert_eq!(view.usage(), None);
    // No usage key on the wire for such an attempt.
    let journal = fs::read_to_string(run_dir.join("events.jsonl")).unwrap();
    assert!(!journal.contains("\"usage\""), "{journal}");
}

/// codex's `exec --json` stream of a turn that ended without an agent
/// message (it only ran a tool). The plain form printed nothing for such a
/// turn, so the node output stays empty rather than becoming the JSON event
/// stream itself.
#[test]
fn a_codex_stream_without_an_agent_message_is_an_empty_reply_not_the_stream() {
    let _env = common::env_lock();
    let dir = tempfile::tempdir().unwrap();
    seed(dir.path());
    common::seed_profile(dir.path(), "main", "codex", "gpt-5", &[]);
    let events = [
        r#"{"type":"thread.started","thread_id":"01a0e454-ef08-7a33-99f4-48719b962fe3"}"#,
        r#"{"type":"turn.started"}"#,
        r#"{"type":"item.completed","item":{"id":"item_0","type":"command_execution","command":"cat secret.txt","aggregated_output":"file contents\n","exit_code":0,"status":"completed"}}"#,
        r#"{"type":"turn.completed","usage":{"input_tokens":100,"cached_input_tokens":40,"output_tokens":7}}"#,
    ]
    .join("\n");
    let _cmd = AgentCmd::set(&stub(dir.path(), &events));

    let res = run(dir.path(), "u", None, RunOptions::default()).unwrap();
    let run_dir = dir.path().join(".apb/runs").join(&res.run_id);
    let view = RunView::load(&run_dir, &res.run_id).unwrap();
    assert_eq!(
        view.state.outputs["w"], "",
        "the node output must not be the event stream"
    );
    assert_eq!(
        attempt_usages(dir.path(), &res.run_id)[0]
            .as_ref()
            .map(|u| u.input_tokens),
        Some(60)
    );
}

/// claude exits non-zero on an errored result, with the JSON result on
/// stdout. The attempt's failure message quotes the reply inside it, as the
/// plain form printed it, not the whole JSON envelope.
#[test]
fn a_failed_claude_exit_quotes_the_reply_not_the_json_envelope() {
    let _env = common::env_lock();
    let dir = tempfile::tempdir().unwrap();
    seed(dir.path());
    let result = serde_json::json!({
        "type": "result",
        "subtype": "success",
        "is_error": true,
        "result": "There is an issue with the selected model.",
        "session_id": "49af5891-8ceb-450b-b315-40cbf9f62a7a",
        "total_cost_usd": 0,
        "usage": {"input_tokens": 0, "output_tokens": 0},
        "modelUsage": {}
    })
    .to_string();
    let _cmd = AgentCmd::set(&failing_stub(dir.path(), &result));

    let res = run(dir.path(), "u", None, RunOptions::default()).unwrap();
    assert_eq!(res.outcome, RunStatus::Failed);
    let journal = fs::read_to_string(
        dir.path()
            .join(".apb/runs")
            .join(&res.run_id)
            .join("events.jsonl"),
    )
    .unwrap();
    assert!(
        journal.contains("There is an issue with the selected model."),
        "{journal}"
    );
    assert!(!journal.contains("modelUsage"), "{journal}");
}

/// The tokens of an errored claude result were spent all the same: when
/// claude exits non-zero with its JSON result on stdout, the failed attempt
/// journals the usage that result reports, and its failure message still
/// quotes the reply rather than the envelope.
#[test]
fn a_failed_claude_exit_still_journals_the_usage_it_reported() {
    let _env = common::env_lock();
    let dir = tempfile::tempdir().unwrap();
    seed(dir.path());
    let _cmd = AgentCmd::set(&failing_stub(dir.path(), &claude_result(true)));

    let res = run(dir.path(), "u", None, RunOptions::default()).unwrap();
    assert_eq!(res.outcome, RunStatus::Failed);
    let expected = AgentUsage {
        input_tokens: 150,
        output_tokens: 50,
        cache_read_tokens: 3000,
        cache_write_tokens: 200,
        cost_usd: Some(0.0125),
        source: UsageSource::Reported,
        cost_basis: CostBasis::Unknown,
    };
    let usages = attempt_usages(dir.path(), &res.run_id);
    assert!(!usages.is_empty());
    assert!(
        usages.iter().all(|u| u.as_ref() == Some(&expected)),
        "{usages:?}"
    );
    let journal = fs::read_to_string(
        dir.path()
            .join(".apb/runs")
            .join(&res.run_id)
            .join("events.jsonl"),
    )
    .unwrap();
    assert!(journal.contains("agent exited with"), "{journal}");
    assert!(journal.contains("the work"), "{journal}");
    assert!(!journal.contains("modelUsage"), "{journal}");
}

/// What every `attempt_finished` of run `run_id` journals as its usage's
/// `cost_basis`, read from the journal's own JSON.
fn journaled_bases(root: &Path, run_id: &str) -> Vec<Option<String>> {
    fs::read_to_string(root.join(".apb/runs").join(run_id).join("events.jsonl"))
        .unwrap()
        .lines()
        .filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
        .filter(|v| v["type"] == "attempt_finished")
        .map(|v| v["usage"]["cost_basis"].as_str().map(str::to_string))
        .collect()
}

/// Where a case puts a file: claude's config dir, the project the agent runs
/// in, the repository around it, or the machine's managed settings
/// directory.
#[derive(Clone, Copy)]
enum At {
    Config,
    Project,
    /// The repository root of a [`Layout::Subdir`], the main checkout of a
    /// [`Layout::Worktree`].
    Repo,
    Managed,
}

/// The managed settings directory under the system root, as claude reads it
/// on this OS.
#[cfg(target_os = "macos")]
const MANAGED_DIR: &str = "Library/Application Support/ClaudeCode";
#[cfg(not(target_os = "macos"))]
const MANAGED_DIR: &str = "etc/claude-code";

/// A subscription login as claude records it on Linux.
const CREDENTIALS: (At, &str, &str) = (
    At::Config,
    ".credentials.json",
    r#"{"claudeAiOauth":{"subscriptionType":"max","scopes":["user:inference"]}}"#,
);

/// Where the project the agent runs in sits.
#[derive(Clone, Copy)]
enum Layout {
    /// Its own directory, outside any repository.
    Plain,
    /// A subdirectory of a git repository.
    Subdir,
    /// A linked worktree of a git repository whose main checkout is
    /// elsewhere.
    Worktree,
}

/// One case of the basis table.
struct Case {
    name: &'static str,
    env: &'static [(&'static str, &'static str)],
    files: &'static [(At, &'static str, &'static str)],
    overlay: Option<&'static str>,
    /// The profile runs with `environment: full`, so claude loads the
    /// user's settings too.
    full: bool,
    /// Neither `HOME` nor `CLAUDE_CONFIG_DIR` reaches apb.
    no_home: bool,
    layout: Layout,
    /// Shell the stub agent runs in its working directory before it prints
    /// its result: what the agent does while it works.
    during: &'static str,
    /// The stub exits 1 after printing, as claude does on an errored result.
    fail: bool,
    expected: &'static str,
}

const NONE: Case = Case {
    name: "",
    env: &[],
    files: &[],
    overlay: None,
    full: false,
    no_home: false,
    layout: Layout::Plain,
    during: "",
    fail: false,
    expected: "unknown",
};

/// A reported cost says what it is, and only facts that positively show a
/// subscription login make it a subscription equivalent. claude's module
/// reads how the spawned process logs in: key variables reaching it (by
/// name), the `env` blocks and `apiKeyHelper` of every settings file claude
/// loads (the user's only outside the minimal environment, the project's,
/// the managed ones), and the login it recorded. A custom endpoint, an
/// unknown login, a declaration alone and an odd or unreadable file leave
/// the basis unknown; a key, a helper, a cloud switch or a Console login is
/// billed.
#[test]
fn a_reported_cost_journals_the_basis_it_was_spent_on() {
    const SUB: &str = "subscriptions:\n  - { agent: claude, plan: personal }\n";
    let cases = [
        Case {
            name: "an api key reaches the process",
            env: &[("ANTHROPIC_API_KEY", "k")],
            expected: "billed",
            ..NONE
        },
        Case {
            name: "an api key wins over a subscription login and a declaration",
            env: &[("ANTHROPIC_API_KEY", "k")],
            files: &[CREDENTIALS],
            overlay: Some(SUB),
            expected: "billed",
            ..NONE
        },
        Case {
            name: "a subscription token reaches the process",
            env: &[("CLAUDE_CODE_OAUTH_TOKEN", "t")],
            expected: "subscription_equivalent",
            ..NONE
        },
        Case {
            name: "a subscription login in the config dir",
            files: &[CREDENTIALS],
            expected: "subscription_equivalent",
            ..NONE
        },
        Case {
            name: "a recorded login that names no known plan",
            files: &[(At::Config, ".credentials.json", "{}")],
            ..NONE
        },
        Case {
            name: "a subscription account recorded in the state file",
            files: &[(
                At::Config,
                ".claude.json",
                r#"{"oauthAccount":{"organizationType":"claude_team"}}"#,
            )],
            expected: "subscription_equivalent",
            ..NONE
        },
        Case {
            name: "an account of an unknown kind in the state file",
            files: &[(
                At::Config,
                ".claude.json",
                r#"{"oauthAccount":{"organizationType":"api_console"}}"#,
            )],
            ..NONE
        },
        Case {
            name: "a key the console login created",
            files: &[
                CREDENTIALS,
                (At::Config, ".claude.json", r#"{"primaryApiKey":"x"}"#),
            ],
            expected: "billed",
            ..NONE
        },
        Case {
            name: "a project that forces the console login",
            files: &[
                CREDENTIALS,
                (
                    At::Project,
                    ".claude/settings.json",
                    r#"{"forceLoginMethod":"console"}"#,
                ),
            ],
            expected: "billed",
            ..NONE
        },
        Case {
            name: "only the declared subscription",
            overlay: Some(SUB),
            ..NONE
        },
        Case {
            name: "a custom endpoint on a subscription token",
            env: &[
                ("ANTHROPIC_BASE_URL", "http://gw"),
                ("CLAUDE_CODE_OAUTH_TOKEN", "t"),
            ],
            files: &[CREDENTIALS],
            ..NONE
        },
        Case {
            name: "a custom endpoint with a key and a declared subscription",
            env: &[
                ("ANTHROPIC_BASE_URL", "http://gw"),
                ("ANTHROPIC_API_KEY", "k"),
            ],
            files: &[CREDENTIALS],
            overlay: Some(SUB),
            ..NONE
        },
        Case {
            name: "a cloud switch next to a leftover custom endpoint",
            env: &[
                ("CLAUDE_CODE_USE_BEDROCK", "1"),
                ("ANTHROPIC_BASE_URL", "http://gw"),
            ],
            files: &[CREDENTIALS],
            overlay: Some(SUB),
            expected: "billed",
            ..NONE
        },
        Case {
            name: "a cloud switch turned off",
            env: &[("CLAUDE_CODE_USE_BEDROCK", "0")],
            files: &[CREDENTIALS],
            expected: "subscription_equivalent",
            ..NONE
        },
        Case {
            name: "a key in the project's local settings env",
            files: &[
                CREDENTIALS,
                (
                    At::Project,
                    ".claude/settings.local.json",
                    r#"{"env":{"ANTHROPIC_API_KEY":"k"}}"#,
                ),
            ],
            expected: "billed",
            ..NONE
        },
        Case {
            name: "a gateway in the project's settings env",
            files: &[
                CREDENTIALS,
                (
                    At::Project,
                    ".claude/settings.json",
                    r#"{"env":{"ANTHROPIC_BASE_URL":"http://gw","ANTHROPIC_AUTH_TOKEN":"t"}}"#,
                ),
            ],
            ..NONE
        },
        Case {
            name: "a key helper in the project's settings",
            files: &[
                CREDENTIALS,
                (
                    At::Project,
                    ".claude/settings.json",
                    r#"{"apiKeyHelper":"echo"}"#,
                ),
            ],
            expected: "billed",
            ..NONE
        },
        Case {
            name: "a key helper in the user's settings, full environment",
            files: &[
                CREDENTIALS,
                (At::Config, "settings.json", r#"{"apiKeyHelper":"echo"}"#),
            ],
            full: true,
            expected: "billed",
            ..NONE
        },
        Case {
            name: "a key helper in the user's settings, which the minimal environment does not load",
            files: &[
                CREDENTIALS,
                (At::Config, "settings.json", r#"{"apiKeyHelper":"echo"}"#),
            ],
            expected: "subscription_equivalent",
            ..NONE
        },
        Case {
            name: "a key helper in the managed settings",
            files: &[
                CREDENTIALS,
                (
                    At::Managed,
                    "managed-settings.json",
                    r#"{"apiKeyHelper":"echo"}"#,
                ),
            ],
            expected: "billed",
            ..NONE
        },
        Case {
            name: "a key in a managed settings drop-in",
            files: &[
                CREDENTIALS,
                (
                    At::Managed,
                    "managed-settings.d/10-key.json",
                    r#"{"env":{"ANTHROPIC_API_KEY":"k"}}"#,
                ),
            ],
            expected: "billed",
            ..NONE
        },
        Case {
            name: "project settings that are not JSON",
            files: &[CREDENTIALS, (At::Project, ".claude/settings.json", "{")],
            ..NONE
        },
        Case {
            name: "a key helper in the project with no home and no config dir",
            files: &[(
                At::Project,
                ".claude/settings.json",
                r#"{"apiKeyHelper":"echo"}"#,
            )],
            overlay: Some(SUB),
            no_home: true,
            expected: "billed",
            ..NONE
        },
        Case {
            name: "a key in the repository root's local settings, run in a subdirectory",
            files: &[
                CREDENTIALS,
                (
                    At::Repo,
                    ".claude/settings.local.json",
                    r#"{"env":{"ANTHROPIC_API_KEY":"k"}}"#,
                ),
            ],
            layout: Layout::Subdir,
            expected: "billed",
            ..NONE
        },
        Case {
            name: "a key helper in the main checkout's local settings, run in a worktree",
            files: &[
                CREDENTIALS,
                (
                    At::Repo,
                    ".claude/settings.local.json",
                    r#"{"apiKeyHelper":"echo"}"#,
                ),
            ],
            layout: Layout::Worktree,
            expected: "billed",
            ..NONE
        },
        Case {
            name: "a subscription login, run in a worktree",
            files: &[CREDENTIALS],
            layout: Layout::Worktree,
            expected: "subscription_equivalent",
            ..NONE
        },
        Case {
            name: "a git variable that moves the repository claude finds",
            env: &[("GIT_DIR", "/elsewhere/.git")],
            files: &[CREDENTIALS],
            ..NONE
        },
        Case {
            name: "a .git file that names no git directory",
            files: &[CREDENTIALS, (At::Project, ".git", "not a link\n")],
            ..NONE
        },
        Case {
            name: "the agent removes the key helper it ran with before it exits",
            files: &[
                CREDENTIALS,
                (
                    At::Project,
                    ".claude/settings.local.json",
                    r#"{"apiKeyHelper":"echo"}"#,
                ),
            ],
            during: "rm .claude/settings.local.json",
            ..NONE
        },
        Case {
            name: "the agent adds a key while it works and removes it again",
            files: &[CREDENTIALS, (At::Project, ".claude/settings.json", "{}")],
            during: "printf '{\"env\":{\"ANTHROPIC_API_KEY\":\"k\"}}' > .claude/settings.local.json && rm .claude/settings.local.json",
            ..NONE
        },
        Case {
            name: "the agent rewrites the project settings it ran with",
            files: &[
                CREDENTIALS,
                (
                    At::Project,
                    ".claude/settings.json",
                    r#"{"apiKeyHelper":"echo"}"#,
                ),
            ],
            during: "printf '{}' > .claude/settings.json",
            ..NONE
        },
        Case {
            name: "a failed attempt on a subscription login",
            files: &[CREDENTIALS],
            fail: true,
            expected: "subscription_equivalent",
            ..NONE
        },
        Case {
            name: "a failed attempt with an api key",
            env: &[("ANTHROPIC_API_KEY", "k")],
            files: &[CREDENTIALS],
            fail: true,
            expected: "billed",
            ..NONE
        },
        Case {
            name: "nothing known",
            ..NONE
        },
    ];
    let _lock = common::env_lock();
    let wrong: Vec<String> = cases
        .iter()
        .filter_map(|case| {
            let (outcome, bases) = run_case(case);
            let want = if case.fail {
                RunStatus::Failed
            } else {
                RunStatus::Succeeded
            };
            let expected = vec![Some(case.expected.to_string()); bases.len().max(1)];
            (outcome != want || bases != expected).then(|| {
                format!(
                    "{}: {outcome:?} {bases:?}, want {}",
                    case.name, case.expected
                )
            })
        })
        .collect();
    assert!(wrong.is_empty(), "{wrong:#?}");
}

/// One run of the stub claude for `case`: its outcome and the bases its
/// attempts journaled.
fn run_case(case: &Case) -> (RunStatus, Vec<Option<String>>) {
    let tmp = tempfile::tempdir().unwrap();
    let (project, repo) = lay_out(tmp.path(), case.layout);
    let project = project.as_path();
    fs::create_dir_all(project).unwrap();
    seed(project);
    if case.full {
        let profile = project.join(".apb/profiles/main/profile.yaml");
        let yaml = fs::read_to_string(&profile).unwrap();
        fs::write(&profile, format!("{yaml}environment: full\n")).unwrap();
    }
    let claude_dir = tmp.path().join("claude-config");
    let system = tmp.path().join("system");
    fs::create_dir_all(&claude_dir).unwrap();
    for (at, rel, body) in case.files {
        let base = match at {
            At::Config => claude_dir.clone(),
            At::Project => project.to_path_buf(),
            At::Repo => repo.clone().expect("a case with a repository"),
            At::Managed => system.join(MANAGED_DIR),
        };
        let path = base.join(rel);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, body).unwrap();
    }
    let config = tmp.path().join("apb-config");
    fs::create_dir_all(&config).unwrap();
    if let Some(overlay) = case.overlay {
        fs::write(config.join("models.yaml"), overlay).unwrap();
    }
    let mut env = vec![
        common::EnvVar::set("APB_CONFIG_DIR", &config),
        common::EnvVar::set("APB_TEST_SYSTEM_ROOT", &system),
    ];
    if case.no_home {
        env.push(common::EnvVar::remove("CLAUDE_CONFIG_DIR"));
        env.push(common::EnvVar::remove("HOME"));
    } else {
        env.push(common::EnvVar::set("CLAUDE_CONFIG_DIR", &claude_dir));
    }
    env.extend(case.env.iter().map(|(k, v)| common::EnvVar::set(k, v)));
    let prog = stub_doing(project, &claude_result(case.fail), case.during, case.fail);
    let _cmd = AgentCmd::set(&prog);
    let res = run(project, "u", None, RunOptions::default()).unwrap();
    let bases = journaled_bases(project, &res.run_id);
    drop(env);
    (res.outcome, bases)
}

/// The project directory for `layout` under `tmp`, and the repository
/// directory a case's [`At::Repo`] files go in.
fn lay_out(tmp: &Path, layout: Layout) -> (std::path::PathBuf, Option<std::path::PathBuf>) {
    match layout {
        Layout::Plain => (tmp.join("project"), None),
        Layout::Subdir => {
            let repo = tmp.join("repo");
            fs::create_dir_all(&repo).unwrap();
            git(&repo, &["init", "-q"]);
            (repo.join("frontend"), Some(repo))
        }
        Layout::Worktree => {
            let main = tmp.join("main");
            fs::create_dir_all(&main).unwrap();
            fs::write(main.join("README"), "main\n").unwrap();
            git(&main, &["init", "-q"]);
            git(&main, &["add", "README"]);
            git(&main, &["commit", "-q", "-m", "base"]);
            let wt = tmp.join("wt");
            git(
                &main,
                &["worktree", "add", "-q", "--detach", wt.to_str().unwrap()],
            );
            (wt, Some(main))
        }
    }
}

/// A plain `git -C dir <args>` for building a fixture, no signing.
fn git(dir: &Path, args: &[&str]) {
    let status = std::process::Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["-c", "user.name=t", "-c", "user.email=t@example.invalid"])
        .args(["-c", "commit.gpgsign=false"])
        .args(args)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .status()
        .unwrap();
    assert!(status.success(), "git {args:?} in {}", dir.display());
}

/// [`stub`] that first runs `during` in its working directory, and exits 1
/// after printing when `fail`.
fn stub_doing(root: &Path, stdout: &str, during: &str, fail: bool) -> String {
    fs::write(root.join("stdout.txt"), stdout).unwrap();
    let path = root.join("agent.sh");
    common::write_sync(
        &path,
        &format!(
            "#!/bin/sh\n{during}\ncat '{out}'\n{exit}\n",
            out = root.join("stdout.txt").display(),
            exit = if fail { "exit 1" } else { "exit 0" },
        ),
    );
    let mut p = fs::metadata(&path).unwrap().permissions();
    p.set_mode(0o755);
    fs::set_permissions(&path, p).unwrap();
    path.to_string_lossy().to_string()
}

/// A project-controlled settings file claude would read is a fifo: reading
/// it must never block the drive, and the basis it hides is unknown.
#[test]
fn a_fifo_in_place_of_project_settings_never_blocks_the_drive() {
    let _lock = common::env_lock();
    let fifo = |root: &Path| {
        let dir = root.join(".claude");
        fs::create_dir_all(&dir).unwrap();
        let path = std::ffi::CString::new(
            dir.join("settings.local.json")
                .to_string_lossy()
                .into_owned(),
        )
        .unwrap();
        // SAFETY: a valid NUL-terminated path.
        assert_eq!(unsafe { libc::mkfifo(path.as_ptr(), 0o644) }, 0);
    };
    let dir = tempfile::tempdir().unwrap();
    seed(dir.path());
    fifo(dir.path());
    let claude_dir = dir.path().join("claude-config");
    fs::create_dir_all(&claude_dir).unwrap();
    fs::write(claude_dir.join(CREDENTIALS.1), CREDENTIALS.2).unwrap();
    let _env = [
        common::EnvVar::set("CLAUDE_CONFIG_DIR", &claude_dir),
        common::EnvVar::set("APB_TEST_SYSTEM_ROOT", dir.path().join("system")),
    ];
    let _cmd = AgentCmd::set(&stub(dir.path(), &claude_result(false)));
    let (tx, rx) = std::sync::mpsc::channel();
    let root = dir.path().to_path_buf();
    std::thread::spawn(move || {
        let res = run(&root, "u", None, RunOptions::default()).unwrap();
        let _ = tx.send((res.outcome, journaled_bases(&root, &res.run_id)));
    });
    let (outcome, bases) = rx
        .recv_timeout(std::time::Duration::from_secs(60))
        .expect("the drive blocked on the fifo");
    assert_eq!(outcome, RunStatus::Succeeded);
    assert_eq!(bases, vec![Some("unknown".to_string())]);
}
