//! Host execution mode (0.23.0): the engine side of the test matrix. The test
//! is the host: it reads `pending_tasks` and submits scripted replies through
//! the same `host_task` functions the MCP and CLI facades call.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use apb_core::execution::{ExecutionMode, ExecutionRequest};
use apb_core::registry::init_project;
use apb_engine::event::{Event, EventPayload, read_all};
use apb_engine::host_task::{
    self, FallbackOf, HintSource, PendingHostTask, SubmitRequest, SubmitStatus, SubmittedUsage,
};
use apb_engine::scheduler::{RunOptions, run_background};
use apb_engine::state::{RunState, RunStatus};

use crate::common;

struct Env(Vec<(&'static str, Option<std::ffi::OsString>)>);

impl Env {
    fn set(vars: &[(&'static str, &str)]) -> Self {
        let mut saved = Vec::new();
        for (k, v) in vars {
            saved.push((*k, std::env::var_os(k)));
            unsafe { std::env::set_var(k, v) };
        }
        Env(saved)
    }
}

impl Drop for Env {
    fn drop(&mut self) {
        for (k, v) in self.0.drain(..).rev() {
            match v {
                Some(v) => unsafe { std::env::set_var(k, v) },
                None => unsafe { std::env::remove_var(k) },
            }
        }
    }
}

struct Host {
    root: tempfile::TempDir,
    cfg: tempfile::TempDir,
}

const HOST: ExecutionRequest = ExecutionRequest {
    mode: Some(ExecutionMode::Host),
    host_session: true,
    client: None,
    inherited: false,
};

/// A `cli` run started by an MCP host session (the host fallback applies).
const CLI_SESSION: ExecutionRequest = ExecutionRequest {
    mode: None,
    host_session: true,
    client: None,
    inherited: false,
};

fn playbook(defaults: &str, nodes: &str, edges: &str) -> String {
    format!(
        "schema: 1\nid: h\nname: H\nversion: 1.0.0\ndefaults:\n  profile: main\n{defaults}nodes:\n  - {{ id: start, type: start }}\n{nodes}  - {{ id: done, type: finish, outcome: success }}\nedges:\n{edges}"
    )
}

/// start -> w -> done, with `extra` on the agent node.
fn one_node(extra: &str) -> String {
    playbook(
        "",
        &format!("  - {{ id: w, type: agent_task, prompt: \"Do the work\"{extra} }}\n"),
        "  - { from: start, to: w }\n  - { from: w, to: done }\n",
    )
}

impl Host {
    /// A project with playbook `h`, profile `main` (claude-code + haiku, the
    /// given fallbacks, a role prompt) and an agent CLI that must never run in
    /// host mode: it leaves `cli-ran` behind when it does.
    fn new(yaml: &str, fallbacks: &[(&str, &str)]) -> Self {
        let root = tempfile::tempdir().unwrap();
        let cfg = tempfile::tempdir().unwrap();
        init_project(root.path()).unwrap();
        let dir = root.path().join(".apb/playbooks/h/1.0.0");
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("playbook.yaml"), yaml).unwrap();
        fs::write(root.path().join(".apb/playbooks/h/current"), "1.0.0").unwrap();
        common::seed_profile(root.path(), "main", "claude-code", "haiku", fallbacks);
        fs::write(
            root.path().join(".apb/profiles/main/SOUL.md"),
            "You are a careful engineer.",
        )
        .unwrap();
        let h = Host { root, cfg };
        h.agent(&format!("touch {}\necho ok", h.path("cli-ran").display()));
        h
    }

    fn path(&self, name: &str) -> PathBuf {
        self.root.path().join(name)
    }

    fn agent(&self, body: &str) {
        let path = self.path("agent.sh");
        common::write_sync(&path, &format!("#!/bin/sh\n{body}\n"));
        let mut perm = fs::metadata(&path).unwrap().permissions();
        perm.set_mode(0o755);
        fs::set_permissions(&path, perm).unwrap();
    }

    fn env(&self) -> Env {
        let cfg = self.cfg.path().to_string_lossy().to_string();
        let agent = self.path("agent.sh").to_string_lossy().to_string();
        Env::set(&[("APB_CONFIG_DIR", &cfg), ("APB_AGENT_CMD", &agent)])
    }

    fn start(&self, execution: ExecutionRequest) -> String {
        run_background(
            self.root.path(),
            "h",
            None,
            RunOptions {
                execution: ExecutionRequest {
                    client: Some("test-host".into()),
                    ..execution
                },
                // Runs of one test follow each other; the finished drive
                // thread may still be releasing the lock.
                allow_shared_workdir: true,
                ..Default::default()
            },
        )
        .unwrap()
    }

    fn run_dir(&self, run_id: &str) -> PathBuf {
        self.root.path().join(".apb/runs").join(run_id)
    }

    fn pending(&self, run_id: &str) -> Vec<PendingHostTask> {
        host_task::pending_for_run(self.root.path(), run_id).unwrap()
    }

    /// Waits for a pending task of `node`.
    fn task(&self, run_id: &str, node: &str) -> PendingHostTask {
        poll(&format!("a host task of `{node}`"), || {
            self.pending(run_id).into_iter().find(|t| t.node == node)
        })
    }

    fn submit(&self, run_id: &str, task_id: &str, status: SubmitStatus, output: &str) {
        self.submit_with(run_id, task_id, status, output, None);
    }

    fn submit_with(
        &self,
        run_id: &str,
        task_id: &str,
        status: SubmitStatus,
        output: &str,
        usage: Option<SubmittedUsage>,
    ) {
        host_task::submit_to_run(
            self.root.path(),
            run_id,
            SubmitRequest {
                task_id: task_id.into(),
                status,
                output: output.into(),
                usage,
                note: None,
                submitted_by: "host".into(),
                client: Some("test-host".into()),
                model: None,
            },
        )
        .unwrap();
    }

    /// A `succeeded` reply that names the model the host ran it on.
    fn submit_on_model(&self, run_id: &str, task_id: &str, output: &str, model: &str) {
        host_task::submit_to_run(
            self.root.path(),
            run_id,
            SubmitRequest {
                task_id: task_id.into(),
                status: SubmitStatus::Succeeded,
                output: output.into(),
                usage: None,
                note: None,
                submitted_by: "host".into(),
                client: Some("test-host".into()),
                model: Some(model.into()),
            },
        )
        .unwrap();
    }

    fn finish(&self, run_id: &str) -> (RunStatus, Vec<Event>) {
        let dir = self.run_dir(run_id);
        poll("the run to end", || {
            let events = read_all(&dir).ok()?;
            let st = RunState::fold(&events).run_status;
            st.is_terminal().then_some((st, events))
        })
    }

    fn cli_ran(&self) -> bool {
        self.path("cli-ran").exists()
    }
}

fn poll<T>(what: &str, mut f: impl FnMut() -> Option<T>) -> T {
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        if let Some(v) = f() {
            return v;
        }
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn count(events: &[Event], pred: impl Fn(&EventPayload) -> bool) -> usize {
    events.iter().filter(|e| pred(&e.payload)).count()
}

fn node_output(events: &[Event], node: &str) -> String {
    events
        .iter()
        .rev()
        .find_map(|e| match &e.payload {
            EventPayload::NodeFinished {
                node: n, output, ..
            } if n == node => Some(output.clone()),
            _ => None,
        })
        .unwrap_or_default()
}

fn attempt_statuses(events: &[Event], node: &str) -> Vec<String> {
    events
        .iter()
        .filter_map(|e| match &e.payload {
            EventPayload::AttemptFinished {
                node: n, status, ..
            } if n == node => Some(status.clone()),
            _ => None,
        })
        .collect()
}

const TWO_NODES: &str = "  - { id: plan, type: agent_task, prompt: \"Plan the work\" }\n  - { id: build, type: agent_task, prompt: \"Build from: {{nodes.plan.output}}\" }\n";
const TWO_EDGES: &str =
    "  - { from: start, to: plan }\n  - { from: plan, to: build }\n  - { from: build, to: done }\n";

#[test]
fn a_two_node_run_succeeds_on_host_replies_without_spawning_a_cli() {
    let h = Host::new(&playbook("", TWO_NODES, TWO_EDGES), &[]);
    let _lock = common::env_lock();
    let _env = h.env();
    let run_id = h.start(HOST);
    let plan = h.task(&run_id, "plan");
    assert!(plan.prompt.starts_with("Plan the work"), "{}", plan.prompt);
    assert!(
        plan.prompt.contains("status: success | failure"),
        "the report contract rides the prompt"
    );
    assert_eq!(
        plan.role_prompt.as_deref(),
        Some("You are a careful engineer.")
    );
    assert_eq!(plan.model_hint, None, "the profile's own model is no hint");
    assert_eq!(
        plan.hint_source,
        Some(HintSource::Primary {
            profile: "main".into()
        }),
        "the primary step is labelled as the profile's own executor"
    );
    assert_eq!(plan.fallback_of, None);
    assert_labelled(&plan);
    assert_eq!(plan.run_id, run_id);
    h.submit_with(
        &run_id,
        &plan.task_id,
        SubmitStatus::Succeeded,
        "PLAN-42\n\n```yaml\nstatus: success\nsummary: planned\n```",
        Some(SubmittedUsage {
            input_tokens: 10,
            output_tokens: 5,
            ..Default::default()
        }),
    );
    let build = h.task(&run_id, "build");
    assert!(
        build.prompt.contains("Build from: PLAN-42"),
        "{}",
        build.prompt
    );
    // Lenient default: no report block reads as success, the whole reply is
    // the output.
    h.submit(&run_id, &build.task_id, SubmitStatus::Succeeded, "built it");
    let (status, events) = h.finish(&run_id);
    assert_eq!(status, RunStatus::Succeeded);
    assert!(!h.cli_ran(), "host mode must never spawn the profile CLI");
    assert_eq!(node_output(&events, "plan"), "PLAN-42");
    assert_eq!(node_output(&events, "build"), "built it");
    assert_eq!(
        count(&events, |p| matches!(
            p,
            EventPayload::HostTaskRequested { .. }
        )),
        2
    );
    let submitted: Vec<(String, Option<String>)> = events
        .iter()
        .filter_map(|e| match &e.payload {
            EventPayload::HostTaskSubmitted {
                submitted_by,
                client,
                ..
            } => Some((submitted_by.clone(), client.clone())),
            _ => None,
        })
        .collect();
    assert_eq!(
        submitted,
        vec![
            ("host".to_string(), Some("test-host".to_string())),
            ("host".to_string(), Some("test-host".to_string()))
        ]
    );
    let agents: Vec<String> = events
        .iter()
        .filter_map(|e| match &e.payload {
            EventPayload::AttemptStarted { agent, .. } => Some(agent.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(agents, vec!["host", "host"]);
    let usage = events.iter().find_map(|e| match &e.payload {
        EventPayload::AttemptFinished {
            node,
            usage: Some(u),
            ..
        } if node == "plan" => Some(u.clone()),
        _ => None,
    });
    let usage = usage.expect("the reported usage is on attempt_finished");
    assert_eq!((usage.input_tokens, usage.output_tokens), (10, 5));
    assert_eq!(usage.source, apb_core::agent_output::UsageSource::Reported);
    // The prompt and the reply live in the run directory, not in the event.
    let dir = h.run_dir(&run_id);
    assert!(
        dir.join("tasks")
            .join(&plan.task_id)
            .join("prompt.md")
            .is_file()
    );
    assert!(
        dir.join("tasks")
            .join(&plan.task_id)
            .join("output.md")
            .is_file()
    );
    assert!(h.pending(&run_id).is_empty());
    let manifest = apb_engine::manifest::read(&dir).unwrap().unwrap();
    assert!(manifest.is_host_mode().unwrap());
}

#[test]
fn a_failure_report_block_fails_the_attempt_and_a_retry_is_a_new_task() {
    let h = Host::new(&one_node(", max_retries: 1"), &[]);
    let _lock = common::env_lock();
    let _env = h.env();
    let run_id = h.start(HOST);
    let first = h.task(&run_id, "w");
    h.submit(
        &run_id,
        &first.task_id,
        SubmitStatus::Succeeded,
        "could not\n```yaml\nstatus: failure\nsummary: no\n```",
    );
    let second = poll("the retry task", || {
        h.pending(&run_id)
            .into_iter()
            .find(|t| t.task_id != first.task_id)
    });
    assert_eq!(second.attempt, 2);
    h.submit(&run_id, &second.task_id, SubmitStatus::Succeeded, "done");
    let (status, events) = h.finish(&run_id);
    assert_eq!(status, RunStatus::Succeeded);
    assert_eq!(attempt_statuses(&events, "w"), vec!["failed", "succeeded"]);
}

/// The `host_task_requested` event of `task_id`: its hint, the hint's label,
/// what closed the previous chain step and the rendered note.
fn requested_hint(
    events: &[Event],
    task_id: &str,
) -> (
    Option<String>,
    Option<HintSource>,
    Option<FallbackOf>,
    Option<String>,
) {
    events
        .iter()
        .find_map(|e| match &e.payload {
            EventPayload::HostTaskRequested {
                task_id: t,
                model_hint,
                hint_source,
                fallback_of,
                hint_note,
                ..
            } if t == task_id => Some((
                model_hint.clone(),
                hint_source.clone(),
                fallback_of.clone(),
                hint_note.clone(),
            )),
            _ => None,
        })
        .expect("a host_task_requested event for the task")
}

/// The task's note is the engine's rendering of its own fields, and the
/// journal's request event carries the same fields and note while the task
/// is still pending (before `fallback_triggered` lands at the node's end).
fn assert_labelled(t: &PendingHostTask) {
    let note = host_task::hint::describe(
        t.model_hint.as_deref(),
        t.hint_source.as_ref(),
        t.fallback_of.as_ref(),
    );
    assert!(note.is_some());
    assert_eq!(t.hint_note, note);
    let dir = t.env.get("APB_RUN_DIR").expect("APB_RUN_DIR");
    let events = read_all(Path::new(dir)).unwrap();
    assert_eq!(
        requested_hint(&events, &t.task_id),
        (
            t.model_hint.clone(),
            t.hint_source.clone(),
            t.fallback_of.clone(),
            note
        )
    );
}

fn fallback(index: u32, of: u32) -> Option<HintSource> {
    Some(HintSource::Fallback {
        index,
        of,
        profile: "main".into(),
    })
}

fn closed(attempt: u32, reason: &str) -> Option<FallbackOf> {
    Some(FallbackOf {
        attempt,
        reason: reason.into(),
    })
}

/// The next pending task of `node` other than `after`.
fn next_task(h: &Host, run_id: &str, after: &str) -> PendingHostTask {
    poll("the next task", || {
        h.pending(run_id).into_iter().find(|t| t.task_id != after)
    })
}

#[test]
fn a_failed_submission_retries_and_then_walks_the_fallback_with_a_model_hint() {
    let h = Host::new(
        &one_node(", max_retries: 1"),
        &[("claude", "sonnet"), ("claude", "opus")],
    );
    let _lock = common::env_lock();
    let _env = h.env();
    let run_id = h.start(HOST);
    let first = h.task(&run_id, "w");
    h.submit(&run_id, &first.task_id, SubmitStatus::Failed, "no luck");
    let retry = next_task(&h, &run_id, &first.task_id);
    assert_eq!(retry.model_hint, None, "a retry keeps the same executor");
    assert!(matches!(
        retry.hint_source,
        Some(HintSource::Primary { .. })
    ));
    assert_eq!(retry.fallback_of, None, "a retry is no chain advance");
    h.submit(&run_id, &retry.task_id, SubmitStatus::Failed, "still no");
    // Fallback 1 after the primary's last attempt.
    let fb1 = next_task(&h, &run_id, &retry.task_id);
    assert_eq!(fb1.attempt, 3);
    assert_eq!(fb1.model_hint.as_deref(), Some("sonnet"));
    assert_eq!(fb1.hint_source, fallback(1, 2));
    assert_eq!(fb1.fallback_of, closed(2, "failed"));
    assert_labelled(&fb1);
    let dir = h.run_dir(&run_id);
    assert_eq!(
        count(&read_all(&dir).unwrap(), |p| matches!(
            p,
            EventPayload::FallbackTriggered { .. }
        )),
        0,
        "the request event, not fallback_triggered, says why the task exists"
    );
    h.submit(&run_id, &fb1.task_id, SubmitStatus::Failed, "no");
    // A retry inside the fallback step names no previous step.
    let fb1_retry = next_task(&h, &run_id, &fb1.task_id);
    assert_eq!(fb1_retry.hint_source, fallback(1, 2));
    assert_eq!(fb1_retry.fallback_of, None);
    h.submit(&run_id, &fb1_retry.task_id, SubmitStatus::Failed, "no");
    // Fallback 2 after the last attempt of fallback 1.
    let fb2 = next_task(&h, &run_id, &fb1_retry.task_id);
    assert_eq!(fb2.model_hint.as_deref(), Some("opus"));
    assert_eq!(fb2.hint_source, fallback(2, 2));
    assert_eq!(fb2.fallback_of, closed(fb1_retry.attempt, "failed"));
    assert_labelled(&fb2);
    h.submit_on_model(&run_id, &fb2.task_id, "done", "host-model-x");
    let (status, events) = h.finish(&run_id);
    assert_eq!(status, RunStatus::Succeeded);
    assert_eq!(
        count(&events, |p| matches!(
            p,
            EventPayload::FallbackTriggered { .. }
        )),
        2
    );
    // The model the host reports it used is journaled as reported.
    let reported = events.iter().find_map(|e| match &e.payload {
        EventPayload::HostTaskSubmitted { task_id, model, .. } if *task_id == fb2.task_id => {
            Some(model.clone())
        }
        _ => None,
    });
    assert_eq!(reported, Some(Some("host-model-x".to_string())));
    assert!(!h.cli_ran());
}

#[test]
fn an_expired_deadline_walks_the_fallback_and_says_so_on_the_task() {
    let h = Host::new(&one_node(", timeout_seconds: 1"), &[("claude", "sonnet")]);
    let _lock = common::env_lock();
    let _env = h.env();
    let run_id = h.start(HOST);
    let first = h.task(&run_id, "w");
    let fb = next_task(&h, &run_id, &first.task_id);
    assert_eq!(fb.hint_source, fallback(1, 1));
    assert_eq!(fb.fallback_of, closed(1, "expired"));
    assert_labelled(&fb);
    h.submit(&run_id, &fb.task_id, SubmitStatus::Succeeded, "done");
    let (status, _) = h.finish(&run_id);
    assert_eq!(status, RunStatus::Succeeded);
}

#[test]
fn an_unanswered_question_walks_the_fallback_as_question_timeout() {
    let h = Host::new(
        &one_node(", interactive: true, question_timeout_seconds: 1"),
        &[("claude", "sonnet")],
    );
    let _lock = common::env_lock();
    let _env = h.env();
    let run_id = h.start(HOST);
    let first = h.task(&run_id, "w");
    h.submit(
        &run_id,
        &first.task_id,
        SubmitStatus::Blocked,
        "Which colour?",
    );
    let fb = next_task(&h, &run_id, &first.task_id);
    assert_eq!(fb.hint_source, fallback(1, 1));
    assert_eq!(fb.fallback_of, closed(1, "question_timeout"));
    assert_labelled(&fb);
    assert!(
        fb.hint_note
            .as_deref()
            .is_some_and(|n| n.contains("after attempt 1 got no answer to its question in time")),
        "{:?}",
        fb.hint_note
    );
    h.submit(&run_id, &fb.task_id, SubmitStatus::Succeeded, "done");
    let (status, _) = h.finish(&run_id);
    assert_eq!(status, RunStatus::Succeeded);
}

#[test]
fn the_closing_answer_labels_its_fallback_task() {
    let yaml = "schema: 1\nid: h\nname: H\nversion: 1.0.0\ndefaults:\n  profile: main\nnodes:\n  - { id: start, type: start }\n  - { id: done, type: finish, outcome: success, prompt: \"compose the answer\" }\nedges:\n  - { from: start, to: done }\n";
    let h = Host::new(yaml, &[("claude", "sonnet")]);
    let _lock = common::env_lock();
    let _env = h.env();
    let run_id = h.start(HOST);
    let first = h.task(&run_id, "done");
    assert!(matches!(
        first.hint_source,
        Some(HintSource::Primary { .. })
    ));
    h.submit(&run_id, &first.task_id, SubmitStatus::Failed, "no");
    let fb = next_task(&h, &run_id, &first.task_id);
    assert_eq!(fb.node, "done");
    assert_eq!(fb.model_hint.as_deref(), Some("sonnet"));
    assert_eq!(fb.hint_source, fallback(1, 1));
    assert_eq!(fb.fallback_of, closed(first.attempt, "failed"));
    assert_labelled(&fb);
    h.submit(&run_id, &fb.task_id, SubmitStatus::Succeeded, "the answer");
    let (status, _) = h.finish(&run_id);
    assert_eq!(status, RunStatus::Succeeded);
}

#[test]
fn success_check_judges_the_host_output() {
    let h = Host::new(
        &one_node(", max_retries: 1, success_check: { marker: \"WAVE-COMPLETE\" }"),
        &[],
    );
    let _lock = common::env_lock();
    let _env = h.env();
    let run_id = h.start(HOST);
    let first = h.task(&run_id, "w");
    h.submit(&run_id, &first.task_id, SubmitStatus::Succeeded, "interim");
    let second = poll("the retry", || {
        h.pending(&run_id)
            .into_iter()
            .find(|t| t.task_id != first.task_id)
    });
    h.submit(
        &run_id,
        &second.task_id,
        SubmitStatus::Succeeded,
        "all done WAVE-COMPLETE",
    );
    let (status, events) = h.finish(&run_id);
    assert_eq!(status, RunStatus::Succeeded);
    assert_eq!(attempt_statuses(&events, "w"), vec!["failed", "succeeded"]);
    let rejected = events.iter().find_map(|e| match &e.payload {
        EventPayload::AttemptFinished {
            rejected_output: Some(r),
            ..
        } => Some(r.clone()),
        _ => None,
    });
    assert_eq!(rejected.as_deref(), Some("interim"));
}

#[test]
fn require_verdict_takes_the_submission_as_the_verdict_and_a_written_status_file_wins() {
    let h = Host::new(
        &playbook(
            "",
            "  - { id: w, type: agent_task, prompt: \"Do\", require_verdict: true }\n  - { id: v, type: agent_task, prompt: \"Publish\", require_verdict: true }\n",
            "  - { from: start, to: w }\n  - { from: w, to: v }\n  - { from: v, to: done }\n",
        ),
        &[],
    );
    let _lock = common::env_lock();
    let _env = h.env();
    let run_id = h.start(HOST);
    let w = h.task(&run_id, "w");
    let status_file = w
        .env
        .get("APB_STATUS_FILE")
        .expect("the task names its status file")
        .clone();
    assert!(w.prompt.contains(&status_file), "the prompt names the path");
    h.submit(&run_id, &w.task_id, SubmitStatus::Succeeded, "did it");
    let v = h.task(&run_id, "v");
    // The subagent writes its own verdict with named outputs.
    fs::write(
        v.env.get("APB_STATUS_FILE").unwrap(),
        r#"{"status":"success","outputs":{"url":"https://x"}}"#,
    )
    .unwrap();
    h.submit(&run_id, &v.task_id, SubmitStatus::Succeeded, "published");
    let (status, events) = h.finish(&run_id);
    assert_eq!(status, RunStatus::Succeeded);
    assert_eq!(attempt_statuses(&events, "w"), vec!["succeeded"]);
    assert_eq!(node_output(&events, "v"), r#"{"url":"https://x"}"#);
}

#[test]
fn a_blocked_task_asks_the_person_and_the_answer_comes_back_as_a_new_task() {
    let h = Host::new(&one_node(""), &[]);
    let _lock = common::env_lock();
    let _env = h.env();
    let run_id = h.start(HOST);
    let first = h.task(&run_id, "w");
    h.submit(
        &run_id,
        &first.task_id,
        SubmitStatus::Blocked,
        "Which colour should it be?",
    );
    let dir = h.run_dir(&run_id);
    let question = poll("the pending question", || {
        let events = read_all(&dir).ok()?;
        apb_engine::progress::from_run_dir(&dir, &events)?.pending_question
    });
    assert_eq!(question.question, "Which colour should it be?");
    assert_eq!(question.answer_by, "human");
    apb_engine::post_answer(&dir, None, "blue", "human").unwrap();
    let follow = poll("the follow-up task", || {
        h.pending(&run_id)
            .into_iter()
            .find(|t| t.task_id != first.task_id)
    });
    assert!(follow.prompt.contains("Which colour should it be?"));
    assert!(follow.prompt.contains("Their answer: blue"));
    h.submit(
        &run_id,
        &follow.task_id,
        SubmitStatus::Succeeded,
        "painted blue",
    );
    let (status, events) = h.finish(&run_id);
    assert_eq!(status, RunStatus::Succeeded);
    assert_eq!(
        count(&events, |p| matches!(p, EventPayload::QuestionAsked { .. })),
        1
    );
    assert_eq!(
        count(&events, |p| matches!(
            p,
            EventPayload::QuestionAnswered { .. }
        )),
        1
    );
    assert_eq!(node_output(&events, "w"), "painted blue");
}

#[test]
fn an_unsubmitted_task_times_out_at_the_node_deadline() {
    let h = Host::new(&one_node(", timeout_seconds: 1"), &[]);
    let _lock = common::env_lock();
    let _env = h.env();
    let run_id = h.start(HOST);
    let task = h.task(&run_id, "w");
    assert!(task.deadline.is_some());
    let (status, events) = h.finish(&run_id);
    assert_eq!(status, RunStatus::Failed);
    assert_eq!(attempt_statuses(&events, "w"), vec!["timed_out"]);
    let closed = events.iter().find_map(|e| match &e.payload {
        EventPayload::HostTaskSubmitted {
            status,
            submitted_by,
            note,
            ..
        } => Some((status.clone(), submitted_by.clone(), note.clone())),
        _ => None,
    });
    let (status, by, note) = closed.expect("the engine closes the expired task");
    assert_eq!((status.as_str(), by.as_str()), ("expired", "engine"));
    assert!(note.unwrap().contains("host_task_timeout"));
    assert!(h.pending(&run_id).is_empty());
}

#[test]
fn parallel_branches_expose_two_tasks_at_once() {
    let h = Host::new(
        &playbook(
            "",
            "  - { id: a, type: agent_task, prompt: \"A\" }\n  - { id: b, type: agent_task, prompt: \"B\" }\n",
            "  - { from: start, to: a }\n  - { from: start, to: b }\n  - { from: a, to: done }\n  - { from: b, to: done }\n",
        ),
        &[],
    );
    let _lock = common::env_lock();
    let _env = h.env();
    let run_id = h.start(HOST);
    let both = poll("two pending tasks", || {
        let p = h.pending(&run_id);
        (p.len() == 2).then_some(p)
    });
    let mut nodes: Vec<&str> = both.iter().map(|t| t.node.as_str()).collect();
    nodes.sort_unstable();
    assert_eq!(nodes, vec!["a", "b"]);
    for t in &both {
        h.submit(&run_id, &t.task_id, SubmitStatus::Succeeded, "ok");
    }
    let (status, _) = h.finish(&run_id);
    assert_eq!(status, RunStatus::Succeeded);
}

#[test]
fn continue_session_starts_cold_with_reason_host_mode() {
    let h = Host::new(
        &playbook(
            "",
            "  - { id: a, type: agent_task, prompt: \"A\" }\n  - { id: b, type: agent_task, prompt: \"B\", continue_session: a }\n",
            "  - { from: start, to: a }\n  - { from: a, to: b }\n  - { from: b, to: done }\n",
        ),
        &[],
    );
    let _lock = common::env_lock();
    let _env = h.env();
    let run_id = h.start(HOST);
    let a = h.task(&run_id, "a");
    h.submit(&run_id, &a.task_id, SubmitStatus::Succeeded, "a done");
    let b = h.task(&run_id, "b");
    h.submit(&run_id, &b.task_id, SubmitStatus::Succeeded, "b done");
    let (status, events) = h.finish(&run_id);
    assert_eq!(status, RunStatus::Succeeded);
    let handoff = events.iter().find_map(|e| match &e.payload {
        EventPayload::SessionHandoff { warm, reason, .. } => Some((*warm, reason.clone())),
        _ => None,
    });
    assert_eq!(handoff, Some((false, Some("host_mode".to_string()))));
}

#[test]
fn the_completion_check_runs_on_the_host_output() {
    let h = Host::new(&one_node(", completion_check: auto"), &[]);
    fs::write(
        h.cfg.path().join("decisions.yaml"),
        "version: 1\nmode: shadow\nproviders:\n  - { id: stub, kind: fake, answers: { final_result: { type: noul, noul: 0.9 } } }\nuses:\n  completion_check: { mode: shadow }\n",
    )
    .unwrap();
    let _lock = common::env_lock();
    let _env = h.env();
    let _fake = Env::set(&[("APB_DECISIONS_ALLOW_FAKE", "1")]);
    let run_id = h.start(HOST);
    let w = h.task(&run_id, "w");
    h.submit(&run_id, &w.task_id, SubmitStatus::Succeeded, "the result");
    let (status, events) = h.finish(&run_id);
    assert_eq!(status, RunStatus::Succeeded);
    let asked = events.iter().any(|e| {
        matches!(&e.payload, EventPayload::DecisionMade { use_site, node, .. }
            if use_site == "completion_check" && node.as_deref() == Some("w"))
    });
    assert!(asked, "the decision layer judged the host output");
}

#[test]
fn a_project_cannot_turn_host_mode_on() {
    let h = Host::new(&one_node(""), &[]);
    fs::write(
        h.path(".apb/config.yaml"),
        "execution:\n  mode: host\n  fallback_to_host: true\n",
    )
    .unwrap();
    let _lock = common::env_lock();
    let _env = h.env();
    let run_id = h.start(ExecutionRequest::default());
    let (status, events) = h.finish(&run_id);
    assert_eq!(status, RunStatus::Succeeded);
    assert!(h.cli_ran(), "the run executed its CLI");
    assert_eq!(
        count(&events, |p| matches!(
            p,
            EventPayload::HostTaskRequested { .. }
        )),
        0
    );
    let manifest = apb_engine::manifest::read(&h.run_dir(&run_id))
        .unwrap()
        .unwrap();
    assert!(manifest.execution.is_none());
}

#[test]
fn apb_execution_cli_beats_a_host_request() {
    let h = Host::new(&one_node(""), &[]);
    let _lock = common::env_lock();
    let _env = h.env();
    let _kill = Env::set(&[("APB_EXECUTION", "cli")]);
    let run_id = h.start(HOST);
    let (status, events) = h.finish(&run_id);
    assert_eq!(status, RunStatus::Succeeded);
    assert!(h.cli_ran());
    assert_eq!(
        count(&events, |p| matches!(
            p,
            EventPayload::HostTaskRequested { .. }
        )),
        0
    );
}

#[test]
fn a_sub_playbook_inherits_host_mode_and_its_tasks_show_on_the_parent() {
    let h = Host::new(
        &playbook(
            "",
            "  - { id: sub, type: playbook, playbook: kid }\n",
            "  - { from: start, to: sub }\n  - { from: sub, to: done }\n",
        ),
        &[],
    );
    let kid = h.path(".apb/playbooks/kid/1.0.0");
    fs::create_dir_all(&kid).unwrap();
    fs::write(
        kid.join("playbook.yaml"),
        "schema: 1\nid: kid\nname: Kid\nversion: 1.0.0\ndefaults:\n  profile: main\nnodes:\n  - { id: start, type: start }\n  - { id: k, type: agent_task, prompt: \"Kid work\" }\n  - { id: done, type: finish, outcome: success }\nedges:\n  - { from: start, to: k }\n  - { from: k, to: done }\n",
    )
    .unwrap();
    fs::write(h.path(".apb/playbooks/kid/current"), "1.0.0").unwrap();
    let _lock = common::env_lock();
    let _env = h.env();
    let run_id = h.start(HOST);
    let k = h.task(&run_id, "k");
    assert_ne!(k.run_id, run_id, "the task belongs to the child run");
    // Submitted through the parent's run id, as a host that only knows the
    // run it started does.
    h.submit(&run_id, &k.task_id, SubmitStatus::Succeeded, "kid done");
    let (status, _) = h.finish(&run_id);
    assert_eq!(status, RunStatus::Succeeded);
    assert!(!h.cli_ran());
}

// --- The host fallback of a `cli` run an MCP host session started ---------

fn fallback_events(events: &[Event]) -> Vec<String> {
    events
        .iter()
        .filter_map(|e| match &e.payload {
            EventPayload::ExecutionFallback { reason, .. } => Some(reason.clone()),
            _ => None,
        })
        .collect()
}

#[test]
fn a_missing_agent_binary_falls_back_to_a_host_task() {
    let h = Host::new(&one_node(""), &[]);
    let _lock = common::env_lock();
    let _env = h.env();
    let _missing = Env::set(&[("APB_AGENT_CMD", "/nonexistent/apb-test-agent")]);
    let run_id = h.start(CLI_SESSION);
    let task = h.task(&run_id, "w");
    assert!(task.prompt.starts_with("Do the work"));
    // The host step stands in for the whole CLI chain: its own label, no
    // hint, and the CLI attempt that could not start.
    assert_eq!(
        task.hint_source,
        Some(HintSource::HostFallback {
            profile: "main".into()
        })
    );
    assert_eq!(task.model_hint, None);
    assert_eq!(task.fallback_of, closed(1, "unstartable"));
    assert_labelled(&task);
    h.submit(
        &run_id,
        &task.task_id,
        SubmitStatus::Succeeded,
        "done by host",
    );
    let (status, events) = h.finish(&run_id);
    assert_eq!(status, RunStatus::Succeeded);
    let reasons = fallback_events(&events);
    assert_eq!(reasons.len(), 1);
    assert!(reasons[0].contains("spawn"), "{reasons:?}");
    assert_eq!(node_output(&events, "w"), "done by host");
}

#[test]
fn an_interactive_fallback_task_asks_the_host_way_and_parks_on_blocked() {
    let h = Host::new(&one_node(", interactive: true"), &[]);
    let _lock = common::env_lock();
    let _env = h.env();
    let _missing = Env::set(&[("APB_AGENT_CMD", "/nonexistent/apb-test-agent")]);
    let run_id = h.start(CLI_SESSION);
    let task = h.task(&run_id, "w");
    assert!(
        task.prompt
            .contains("reply with only the question for the user"),
        "the host question paragraph: {}",
        task.prompt
    );
    assert!(
        !task.prompt.contains(apb_engine::adapter::QUESTION_MARKER),
        "no stdout marker contract for a host step: {}",
        task.prompt
    );
    h.submit(
        &run_id,
        &task.task_id,
        SubmitStatus::Blocked,
        "Which colour?",
    );
    let dir = h.run_dir(&run_id);
    let question = poll("the pending question", || {
        let events = read_all(&dir).ok()?;
        apb_engine::progress::from_run_dir(&dir, &events)?.pending_question
    });
    assert_eq!(question.question, "Which colour?");
    apb_engine::scheduler::run_cancel(h.root.path(), &run_id).unwrap();
    let (status, _) = h.finish(&run_id);
    assert_ne!(status, RunStatus::Succeeded);
}

#[test]
fn a_logged_out_agent_falls_back_to_a_host_task() {
    let h = Host::new(&one_node(""), &[]);
    h.agent("echo 'Error: Not logged in. Please run /login' >&2\nexit 1");
    let _lock = common::env_lock();
    let _env = h.env();
    let run_id = h.start(CLI_SESSION);
    let task = h.task(&run_id, "w");
    h.submit(&run_id, &task.task_id, SubmitStatus::Succeeded, "ok");
    let (status, events) = h.finish(&run_id);
    assert_eq!(status, RunStatus::Succeeded);
    assert_eq!(fallback_events(&events).len(), 1);
}

#[test]
fn a_model_turn_failure_never_falls_back() {
    let h = Host::new(&one_node(", max_retries: 1"), &[]);
    h.agent("echo 'the model could not finish the refactor' >&2\nexit 1");
    let _lock = common::env_lock();
    let _env = h.env();
    let run_id = h.start(CLI_SESSION);
    let (status, events) = h.finish(&run_id);
    assert_eq!(status, RunStatus::Failed);
    assert_eq!(attempt_statuses(&events, "w"), vec!["failed", "failed"]);
    assert!(fallback_events(&events).is_empty());
    assert_eq!(
        count(&events, |p| matches!(
            p,
            EventPayload::HostTaskRequested { .. }
        )),
        0
    );
}

#[test]
fn an_auth_error_after_a_model_turn_never_falls_back() {
    let h = Host::new(&one_node(", max_retries: 1"), &[]);
    // A real turn (a reply on stdout), then a push that the remote refused.
    h.agent("echo 'pushed the branch, opening the PR'\necho 'error: Bad credentials' >&2\nexit 1");
    let _lock = common::env_lock();
    let _env = h.env();
    let run_id = h.start(CLI_SESSION);
    let (status, events) = h.finish(&run_id);
    assert_eq!(status, RunStatus::Failed);
    // As in a plain CLI run: an auth failure is non-transient, no retry.
    assert_eq!(attempt_statuses(&events, "w"), vec!["failed"]);
    assert!(fallback_events(&events).is_empty());
    assert_eq!(
        count(&events, |p| matches!(
            p,
            EventPayload::HostTaskRequested { .. }
        )),
        0
    );
}

#[test]
fn a_cli_started_run_keeps_refusing_as_before() {
    let h = Host::new(&one_node(""), &[]);
    let _lock = common::env_lock();
    let _env = h.env();
    let _missing = Env::set(&[("APB_AGENT_CMD", "/nonexistent/apb-test-agent")]);
    let run_id = h.start(ExecutionRequest::default());
    let (status, events) = h.finish(&run_id);
    assert_eq!(status, RunStatus::Failed);
    assert!(fallback_events(&events).is_empty());
    let manifest = apb_engine::manifest::read(&h.run_dir(&run_id))
        .unwrap()
        .unwrap();
    assert!(
        manifest.execution.is_none(),
        "a plain CLI run's manifest is unchanged"
    );
}

#[test]
fn the_fallback_is_off_when_the_global_or_the_project_config_says_so() {
    for how in ["config", "project"] {
        let h = Host::new(&one_node(""), &[]);
        match how {
            "config" => fs::write(
                h.cfg.path().join("config.yaml"),
                "execution:\n  fallback_to_host: false\n",
            )
            .unwrap(),
            "project" => fs::write(
                h.path(".apb/config.yaml"),
                "execution:\n  fallback_to_host: false\n",
            )
            .unwrap(),
            _ => {}
        }
        let _lock = common::env_lock();
        let _env = h.env();
        let _missing = Env::set(&[("APB_AGENT_CMD", "/nonexistent/apb-test-agent")]);
        let run_id = h.start(CLI_SESSION);
        let (status, events) = h.finish(&run_id);
        assert_eq!(status, RunStatus::Failed, "{how}");
        assert!(fallback_events(&events).is_empty(), "{how}");
    }
}

fn git(root: &Path, args: &[&str]) {
    let mut full: Vec<&str> = vec!["-C", root.to_str().unwrap(), "-c", "commit.gpgsign=false"];
    full.extend_from_slice(args);
    let ok = std::process::Command::new("git")
        .args(&full)
        .output()
        .unwrap()
        .status
        .success();
    assert!(ok, "git {args:?} failed");
}

#[test]
fn host_and_cli_runs_never_share_a_cache_entry() {
    let h = Host::new(&one_node(", cache: auto"), &[]);
    let root = h.root.path();
    fs::create_dir_all(root.join("src")).unwrap();
    fs::write(root.join("src/work.txt"), "hello\n").unwrap();
    fs::write(root.join(".gitignore"), ".apb/\nagent.sh\ncli-ran\n").unwrap();
    git(root, &["init", "-q"]);
    git(root, &["config", "user.email", "t@t"]);
    git(root, &["config", "user.name", "t"]);
    git(root, &["add", "."]);
    git(root, &["commit", "-qm", "c1"]);
    let _lock = common::env_lock();
    let _env = h.env();
    // Two CLI runs: the second is served from the cache.
    let first = h.start(ExecutionRequest::default());
    assert_eq!(h.finish(&first).0, RunStatus::Succeeded);
    fs::remove_file(h.path("cli-ran")).unwrap();
    let second = h.start(ExecutionRequest::default());
    assert_eq!(h.finish(&second).0, RunStatus::Succeeded);
    assert!(!h.cli_ran(), "the second CLI run is a cache hit");
    // A host run of the same node misses the CLI entry and asks the host.
    let host = h.start(HOST);
    let task = h.task(&host, "w");
    h.submit(&host, &task.task_id, SubmitStatus::Succeeded, "host reply");
    let (status, events) = h.finish(&host);
    assert_eq!(status, RunStatus::Succeeded);
    assert_eq!(node_output(&events, "w"), "host reply");
}

#[test]
fn a_judge_node_never_emulates_through_the_profile_cli_in_host_mode() {
    let h = Host::new(
        "schema: 2\nid: h\nname: H\nversion: 1.0.0\ndefaults:\n  profile: main\nnodes:\n  - { id: start, type: start }\n  - id: triage\n    type: judge\n    state: { note: \"fixed text\" }\n    questions:\n      risky: { type: noul, instructions: \"Is `note` risky?\" }\n    on_unavailable: emulate\n  - { id: done, type: finish, outcome: success }\n  - { id: failed, type: finish, outcome: failure }\nedges:\n  - { from: start, to: triage }\n  - { from: triage, to: failed, condition: { type: node_status, node: triage, equals: failure } }\n  - { from: triage, to: done, fallback: true }\n",
        &[],
    );
    let _lock = common::env_lock();
    let _env = h.env();
    let run_id = h.start(HOST);
    let (status, events) = h.finish(&run_id);
    assert!(!h.cli_ran(), "host mode must never spawn the profile CLI");
    assert_eq!(status, RunStatus::Failed);
    let out = node_output(&events, "triage");
    assert!(
        out.contains("host execution mode spawns no agent CLI"),
        "{out}"
    );
    assert_eq!(
        count(&events, |p| matches!(
            p,
            EventPayload::HostTaskRequested { .. }
        )),
        0
    );
}

#[test]
fn a_host_fallback_output_is_never_cached_for_the_cli_agent() {
    let h = Host::new(&one_node(", cache: auto"), &[]);
    let root = h.root.path();
    fs::create_dir_all(root.join("src")).unwrap();
    fs::write(root.join("src/work.txt"), "hello\n").unwrap();
    fs::write(root.join(".gitignore"), ".apb/\nagent.sh\ncli-ran\n").unwrap();
    git(root, &["init", "-q"]);
    git(root, &["config", "user.email", "t@t"]);
    git(root, &["config", "user.name", "t"]);
    git(root, &["add", "."]);
    git(root, &["commit", "-qm", "c1"]);
    let _lock = common::env_lock();
    let _env = h.env();
    // A cli run whose CLI cannot start: the host does the step.
    let fell_back = {
        let _missing = Env::set(&[("APB_AGENT_CMD", "/nonexistent/apb-test-agent")]);
        let run_id = h.start(CLI_SESSION);
        let task = h.task(&run_id, "w");
        h.submit(
            &run_id,
            &task.task_id,
            SubmitStatus::Succeeded,
            "host reply",
        );
        h.finish(&run_id).1
    };
    assert_eq!(fallback_events(&fell_back).len(), 1);
    assert_eq!(
        count(&fell_back, |p| matches!(
            p,
            EventPayload::NodeCacheStored { .. }
        )),
        0,
        "the host's output is not stored under the CLI agent's key"
    );
    // A later pure CLI run runs its CLI instead of reusing the host reply.
    let run_id = h.start(ExecutionRequest::default());
    let (status, events) = h.finish(&run_id);
    assert_eq!(status, RunStatus::Succeeded);
    assert!(h.cli_ran(), "the CLI run is not served the host's output");
    assert_eq!(node_output(&events, "w"), "ok");
}
