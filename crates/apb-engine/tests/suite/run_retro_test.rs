//! The run retrospective (issue #192 part 3): `run_retro::build` over run
//! directories laid out as apb writes them, and `{{run.retro}}`.

use std::fs;
use std::path::{Path, PathBuf};

use apb_engine::context::{OutputClip, render};
use apb_engine::run_retro::{self, RetroRun};
use apb_engine::run_stats::StatsRun;
use serde_json::{Value, json};

const SNAPSHOT: &str = r#"
schema: 2
id: p
name: P
version: 1.0.0
goal:
  statement: the site is built
  criteria:
    - { description: the work says it is done, check: { type: marker, marker: DONE } }
nodes:
  - { id: start, type: start }
  - { id: work, type: agent_task, prompt: work, profile: main, expected_duration: 30 }
  - { id: cli, type: agent_task, prompt: check, profile: main, expected_duration: 60 }
  - { id: done, type: finish, outcome: success }
edges:
  - { from: start, to: work }
  - { from: work, to: cli }
  - { from: cli, to: done }
"#;

fn run_dir(root: &Path, id: &str) -> PathBuf {
    root.join(".apb/runs").join(id)
}

/// Writes `events` (`(ts, payload)`) as the run's journal plus its snapshot.
fn write_run(root: &Path, id: &str, events: &[(u128, Value)]) -> PathBuf {
    let dir = run_dir(root, id);
    fs::create_dir_all(&dir).unwrap();
    let mut journal = String::new();
    for (seq, (ts, payload)) in events.iter().enumerate() {
        let mut line = payload.clone();
        line["seq"] = json!(seq as u64 + 1);
        line["ts"] = json!(*ts as u64);
        journal.push_str(&line.to_string());
        journal.push('\n');
    }
    fs::write(dir.join("events.jsonl"), journal).unwrap();
    fs::write(dir.join("playbook.yaml"), SNAPSHOT).unwrap();
    dir
}

fn usage(input: u64, output: u64, cost: f64) -> Value {
    json!({"input_tokens": input, "output_tokens": output, "cost_usd": cost, "source": "reported"})
}

/// A finished host-mode run: `work` ran as a host task the host submitted
/// after a minute on `opus-4`; `cli` ran a CLI attempt on `gpt-5` that
/// failed, then a retry that succeeded.
fn host_run(root: &Path) -> PathBuf {
    let dir = write_run(
        root,
        "h1",
        &[
            (
                0,
                json!({"type": "run_started", "playbook": "p", "version": "1.0.0"}),
            ),
            (
                1_000,
                json!({"type": "node_started", "node": "work", "attempt": 1}),
            ),
            (
                1_000,
                json!({"type": "attempt_started", "node": "work", "attempt": 1, "agent": "claude", "model": "sonnet"}),
            ),
            (
                1_100,
                json!({"type": "host_task_requested", "task_id": "work-1", "node": "work", "attempt": 1, "prompt_ref": "tasks/work-1/prompt.md", "workdir": "/w"}),
            ),
            (
                61_100,
                json!({"type": "host_task_submitted", "task_id": "work-1", "status": "succeeded", "submitted_by": "host", "model": "opus-4", "usage": usage(100, 50, 0.02)}),
            ),
            (
                61_500,
                json!({"type": "attempt_finished", "node": "work", "attempt": 1, "status": "succeeded", "duration_ms": 60_500, "usage": usage(100, 50, 0.02)}),
            ),
            (
                61_500,
                json!({"type": "node_finished", "node": "work", "status": "succeeded", "attempt": 1, "output": "DONE"}),
            ),
            (
                62_000,
                json!({"type": "node_started", "node": "cli", "attempt": 1}),
            ),
            (
                62_000,
                json!({"type": "attempt_started", "node": "cli", "attempt": 1, "agent": "codex", "model": "gpt-5"}),
            ),
            (
                64_000,
                json!({"type": "attempt_finished", "node": "cli", "attempt": 1, "status": "failed", "duration_ms": 2_000, "failure_kind": "agent"}),
            ),
            (
                64_000,
                json!({"type": "retry_started", "node": "cli", "attempt": 2}),
            ),
            (
                64_100,
                json!({"type": "attempt_started", "node": "cli", "attempt": 2, "agent": "codex", "model": "gpt-5"}),
            ),
            (
                70_000,
                json!({"type": "attempt_finished", "node": "cli", "attempt": 2, "status": "succeeded", "duration_ms": 5_900, "usage": usage(10, 5, 0.001)}),
            ),
            (
                70_000,
                json!({"type": "node_finished", "node": "cli", "status": "succeeded", "attempt": 2, "output": "ok"}),
            ),
            (
                70_500,
                json!({"type": "goal_checked", "index": 0, "description": "the work says it is done", "check": "marker", "status": "passed"}),
            ),
            (
                71_000,
                json!({"type": "run_finished", "outcome": "succeeded"}),
            ),
        ],
    );
    let status = dir.join("agent-status");
    fs::create_dir_all(&status).unwrap();
    fs::write(status.join("work-1.json"), r#"{"status":"success"}"#).unwrap();
    fs::write(status.join("cli-1.json"), r#"{"status":"failure"}"#).unwrap();
    fs::write(status.join("cli-2.json"), "not json").unwrap();
    fs::write(
        dir.join("manifest.yaml"),
        "profiles: []\nnode_bindings:\n  work: project/main\n  cli: project/main\nexecution:\n  mode: host\n",
    )
    .unwrap();
    dir
}

/// An earlier finished run of `version` taking `total_ms`, `work` taking
/// `work_ms` of it, with `tokens` reported.
fn earlier_run(
    root: &Path,
    id: &str,
    version: &str,
    start: u128,
    total_ms: u128,
    work_ms: u128,
    tokens: u64,
) -> StatsRun {
    let dir = write_run(
        root,
        id,
        &[
            (
                start,
                json!({"type": "run_started", "playbook": "p", "version": version}),
            ),
            (
                start,
                json!({"type": "node_started", "node": "work", "attempt": 1}),
            ),
            (
                start + work_ms,
                json!({"type": "attempt_finished", "node": "work", "attempt": 1, "status": "succeeded", "usage": usage(tokens, 0, 0.01)}),
            ),
            (
                start + work_ms,
                json!({"type": "node_finished", "node": "work", "status": "succeeded", "attempt": 1, "output": ""}),
            ),
            (
                start + total_ms,
                json!({"type": "run_finished", "outcome": "succeeded"}),
            ),
        ],
    );
    StatsRun::load(&dir).unwrap()
}

#[test]
fn a_host_mode_run_reports_per_node_numbers_with_the_actual_model() {
    let root = tempfile::tempdir().unwrap();
    let dir = host_run(root.path());
    let report = run_retro::build(&RetroRun::load(&dir).unwrap(), &[], 10);

    assert_eq!(report.outcome.as_deref(), Some("succeeded"));
    assert_eq!(report.execution.as_deref(), Some("host"));
    assert_eq!(report.duration_ms, Some(71_000));
    assert_eq!(report.tokens, Some(165));
    assert_eq!(report.cost_usd, Some(0.021));
    assert!(report.baseline.is_none());
    let goal = report.goal.as_ref().expect("the snapshot declares a goal");
    assert_eq!(goal.passed, 1);

    let ids: Vec<&str> = report.nodes.iter().map(|n| n.node.as_str()).collect();
    assert_eq!(ids, ["work", "cli"]);

    let work = &report.nodes[0];
    assert_eq!(work.profile.as_deref(), Some("project/main"));
    assert_eq!(work.duration_ms, 60_500);
    assert_eq!(work.expected_s, Some(30));
    assert_eq!(work.over_expected, Some(true));
    assert_eq!(work.models, ["opus-4"]);
    assert_eq!(work.host_wait_ms, Some(60_000));
    assert_eq!(work.tokens, Some(150));
    let a = &work.attempts[0];
    assert_eq!(a.model.as_deref(), Some("opus-4"));
    assert_eq!(a.model_source, "host");
    assert_eq!(a.declared_model.as_deref(), Some("sonnet"));
    assert_eq!(a.host_tasks, 1);
    assert_eq!(a.verdict.as_deref(), Some("success"));

    let cli = &report.nodes[1];
    assert_eq!(cli.duration_ms, 8_000);
    assert_eq!(cli.over_expected, Some(false));
    assert_eq!(
        (cli.executions, cli.reentries, cli.retries, cli.fallbacks),
        (1, 0, 1, 0)
    );
    assert_eq!(cli.attempts.len(), 2);
    assert_eq!(cli.models, ["gpt-5"]);
    assert_eq!(cli.host_wait_ms, None);
    let first = &cli.attempts[0];
    assert_eq!(first.model.as_deref(), Some("gpt-5"));
    assert_eq!(first.model_source, "cli");
    assert_eq!(first.status.as_deref(), Some("failed"));
    assert_eq!(first.failure_kind.as_deref(), Some("agent"));
    assert_eq!(first.verdict.as_deref(), Some("failure"));
    assert_eq!(cli.attempts[1].verdict.as_deref(), Some("invalid"));
}

#[test]
fn a_host_task_whose_host_named_no_model_reads_unreported() {
    let root = tempfile::tempdir().unwrap();
    let dir = write_run(
        root.path(),
        "h2",
        &[
            (
                0,
                json!({"type": "run_started", "playbook": "p", "version": "1.0.0"}),
            ),
            (
                0,
                json!({"type": "node_started", "node": "work", "attempt": 1}),
            ),
            (
                0,
                json!({"type": "host_task_requested", "task_id": "work-1", "node": "work", "attempt": 1, "model_hint": "haiku"}),
            ),
            (
                5_000,
                json!({"type": "host_task_submitted", "task_id": "work-1", "status": "succeeded", "submitted_by": "host"}),
            ),
        ],
    );
    let report = run_retro::build(&RetroRun::load(&dir).unwrap(), &[], 0);
    // Live: no outcome, the duration runs to the last journal line.
    assert_eq!(report.outcome, None);
    assert_eq!(report.duration_ms, Some(5_000));
    let a = &report.nodes[0].attempts[0];
    assert_eq!(a.model, None);
    assert_eq!(a.model_source, "unreported");
    assert_eq!(a.declared_model.as_deref(), Some("haiku"));
    assert_eq!(a.host_wait_ms, Some(5_000));
    assert_eq!(report.nodes[0].status, None);
}

#[test]
fn the_baseline_is_the_median_of_the_last_runs_of_the_same_version() {
    let root = tempfile::tempdir().unwrap();
    let dir = host_run(root.path());
    let history = vec![
        // The oldest of four: left out by `compare_last: 3`.
        earlier_run(root.path(), "e0", "1.0.0", 0, 900_000, 800_000, 9_000),
        earlier_run(root.path(), "e1", "1.0.0", 10, 50_000, 20_000, 100),
        earlier_run(root.path(), "e2", "1.0.0", 20, 80_000, 40_000, 300),
        earlier_run(root.path(), "e3", "1.0.0", 30, 60_000, 30_000, 200),
        // Another version never counts.
        earlier_run(root.path(), "x1", "2.0.0", 40, 1_000, 500, 1),
    ];
    let report = run_retro::build(&RetroRun::load(&dir).unwrap(), &history, 3);
    let b = report.baseline.expect("three earlier runs of the version");
    assert_eq!(b.run_ids, ["e3", "e2", "e1"]);
    assert_eq!(b.success.count, 3);
    assert_eq!(b.median_duration_ms, Some(60_000));
    assert_eq!(b.duration_delta_ms, Some(11_000));
    assert_eq!(b.median_tokens, Some(200));
    assert_eq!(b.median_cost_usd, Some(0.01));
    assert_eq!(report.nodes[0].baseline_median_ms, Some(30_000));
    // `cli` never ran in the baseline runs.
    assert_eq!(report.nodes[1].baseline_median_ms, None);
}

#[test]
fn run_retro_renders_into_a_prompt_bounded() {
    let root = tempfile::tempdir().unwrap();
    let dir = host_run(root.path());
    let empty = Default::default();
    let clip = OutputClip {
        run_dir: &dir,
        max_bytes: 0,
        retro: Some(run_retro::text::prompt_text),
    };
    let text = render(
        "Retro:\n{{ run.retro }}",
        &empty,
        None,
        &empty,
        &Default::default(),
        &empty,
        &empty,
        "",
        &clip,
    );
    assert!(
        text.starts_with("Retro:\nRun h1 of p 1.0.0: succeeded"),
        "{text}"
    );
    assert!(
        text.contains("- work: succeeded, 1.0 min (over the expected 30 s)"),
        "{text}"
    );
    assert!(text.contains("model opus-4"), "{text}");
    assert!(text.contains("host wait 1.0 min"), "{text}");
    assert!(
        text.contains("attempt 1: failed, 2 s, model gpt-5 (cli), verdict failure"),
        "{text}"
    );
    assert!(text.contains("[passed] the work says it is done"), "{text}");
    assert!(text.len() <= run_retro::text::MAX_BYTES + "Retro:\n".len());

    let report = run_retro::build(&RetroRun::load(&dir).unwrap(), &[], 0);
    let cut = run_retro::text::render_bounded(&report, 200);
    assert!(cut.len() <= 200, "{}", cut.len());
    assert!(
        cut.ends_with("run_retro_context with this run id)\n"),
        "{cut}"
    );
}
