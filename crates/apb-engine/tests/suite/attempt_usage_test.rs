//! Token usage per agent attempt (issue #167): the built-in claude form asks
//! for the CLI's JSON result, the attempt journals the usage it reports, the
//! node output is the unwrapped reply, and the run view totals it. An agent
//! printing plain text journals no usage at all.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;

use apb_core::agent_output::{AgentUsage, UsageSource};
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
