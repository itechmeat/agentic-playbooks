//! `apb doctor`: the environment doctor, `--agents` (every installed
//! built-in agent), `--run <id>` (one run's health) and `--probe-agent <id>`
//! (one paid call that checks an agent's output format).
//!
//! The reports print the same way (one `[level] subject: detail` line per
//! check, exit 1 on a blocking one) because they answer the same kind of
//! question at different scopes, and an operator should not have to learn
//! several output formats while debugging.

use std::path::Path;
use std::process::ExitCode;
use std::time::Duration;

use apb_core::doctor::{CheckStatus, DoctorReport};
use apb_engine::agent_probe::{COST_NOTICE, ProbeRequest, probe, refusal};
use apb_engine::run_doctor::{FAIL, OK, WARN};

use crate::util::{QUESTION_TEXT_MAX, StepCaller, sanitize_for_terminal};

/// How long the paid probe waits for the agent.
const PROBE_TIMEOUT: Duration = Duration::from_secs(180);

/// At most this many characters of agent-derived text (a failure message,
/// a model name) reach the terminal.
const PROBE_TEXT_MAX: usize = 400;

/// The `sandbox` line: bwrap's path, version and its own error (at most
/// 300 characters), then the Landlock facts, which must survive the cut.
const SANDBOX_LINE_MAX: usize = 800;

/// What `apb doctor` was asked for.
#[derive(Debug, Default)]
pub(crate) struct DoctorArgs {
    pub run: Option<String>,
    pub agents: bool,
    pub probe_agent: Option<String>,
    pub model: Option<String>,
}

pub(crate) fn doctor_cmd(root: &Path, args: DoctorArgs) -> ExitCode {
    if let Some(agent) = &args.probe_agent {
        return doctor_probe(root, agent, args.model.as_deref());
    }
    if let Some(run_id) = &args.run {
        return doctor_run(root, run_id);
    }
    if args.agents {
        return finish(print_report(&apb_core::doctor::diagnose_agents()));
    }
    doctor_env(root)
}

/// The marker of a level, padded so the subjects line up.
fn marker(status: &str) -> &str {
    match status {
        OK => "[ok]  ",
        WARN => "[warn]",
        FAIL => "[fail]",
        other => other,
    }
}

fn line(status: &str, subject: &str, detail: &str) {
    println!("{} {subject}: {detail}", marker(status));
}

/// Prints an environment report; whether it holds a blocking problem.
fn print_report(report: &DoctorReport) -> bool {
    for c in &report.checks {
        let status = match c.status {
            CheckStatus::Ok => OK,
            CheckStatus::Warn => WARN,
            CheckStatus::Fail => FAIL,
        };
        line(status, &c.name, &c.detail);
    }
    report.has_failure()
}

fn finish(blocking: bool) -> ExitCode {
    if blocking {
        eprintln!("doctor: found blocking problems");
        ExitCode::from(1)
    } else {
        ExitCode::SUCCESS
    }
}

fn doctor_env(root: &Path) -> ExitCode {
    let blocking = print_report(&apb_core::doctor::diagnose(root));
    // The isolation sandbox: not blocking, only `isolation: full` needs it.
    let host = apb_engine::sandbox::probe::host_facts();
    let (ok, detail) = apb_engine::sandbox::probe::doctor_line(&host);
    // The detail carries bwrap's own output and paths: never raw control
    // sequences on the operator's terminal.
    line(
        if ok { OK } else { WARN },
        "sandbox",
        &sanitize_for_terminal(&detail, SANDBOX_LINE_MAX),
    );
    match crate::dashboard_check::dashboard_version_line() {
        Some(Ok(detail)) => line(OK, "dashboard", &detail),
        Some(Err(detail)) => line(WARN, "dashboard", &detail),
        None => {}
    }
    finish(blocking)
}

/// The per-run doctor. Read-only: it names problems and repairs nothing, so
/// the repair verbs (`apb stop`, resume) stay explicit operator decisions.
fn doctor_run(root: &Path, run_id: &str) -> ExitCode {
    use apb_engine::run_doctor::{diagnose_run, has_failure};
    let checks = match diagnose_run(root, run_id) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("doctor: {e}");
            return ExitCode::from(2);
        }
    };
    for c in &checks {
        line(c.status, &c.subject, &c.detail);
    }
    print_pending_question_check(root, run_id);
    finish(has_failure(&checks))
}

/// Flags a pending interactive question on the run, if any
/// (spec 2026-07-20-interactive-nodes, Task 9): read directly from the
/// `questions.jsonl`/`answers.jsonl` channel files via
/// `apb_engine::progress::from_run_dir` (Task 3), the same source
/// `run_status` and `apb runs` use, so it is visible even before drive has
/// journaled a `QuestionAsked` event for it. Not part of `diagnose_run`'s own
/// fixed check order (that stays an engine-crate concern); a pending
/// question is a normal wait state, not a blocking problem, so it never
/// affects this command's exit code. A best-effort read: an unreadable
/// events.jsonl simply omits the line rather than failing the whole report.
fn print_pending_question_check(root: &Path, run_id: &str) {
    let run_dir = root.join(".apb/runs").join(run_id);
    let Ok(events) = apb_engine::run_view::read_events(&run_dir) else {
        return;
    };
    if let Some(pq) =
        apb_engine::progress::from_run_dir(&run_dir, &events).and_then(|p| p.pending_question)
    {
        line(
            WARN,
            "pending question",
            &format!(
                "node `{}`: {}",
                pq.node,
                sanitize_for_terminal(&pq.question, QUESTION_TEXT_MAX)
            ),
        );
    }
}

/// The paid probe: the cost notice, one call, the verdict. Exit 1 when the
/// output format drifted, 2 when the probe could not run or was interrupted.
/// SIGINT and SIGTERM stop the agent (it runs in its own process group, out
/// of a terminal's Ctrl-C) and remove the probe's temporary directory.
fn doctor_probe(root: &Path, agent: &str, model: Option<&str>) -> ExitCode {
    if let Some(why) = refusal(&StepCaller::from_env()) {
        eprintln!("doctor: {why}");
        return ExitCode::from(2);
    }
    let global = match apb_core::config::GlobalConfig::load() {
        Ok(g) => g,
        Err(e) => {
            eprintln!("doctor: global config: {e}");
            return ExitCode::from(2);
        }
    };
    eprintln!("doctor: {COST_NOTICE}");
    let req = ProbeRequest {
        agent,
        model,
        timeout: PROBE_TIMEOUT,
        project: root,
    };
    crate::interrupt::install();
    let report = match probe(&req, &global, &crate::interrupt::INTERRUPTED) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("doctor: {}", terminal_text(&e.to_string()));
            return ExitCode::from(2);
        }
    };
    print_probe(&report);
    if report.drifted() {
        eprintln!("doctor: the agent's output format drifted from its built-in form");
        ExitCode::from(1)
    } else {
        ExitCode::SUCCESS
    }
}

/// Agent-derived text, safe for the terminal.
fn terminal_text(s: &str) -> String {
    sanitize_for_terminal(s, PROBE_TEXT_MAX)
}

/// The probe's report, one line per check.
fn print_probe(report: &apb_engine::agent_probe::ProbeReport) {
    let subject = format!("agent {} probe", report.agent);
    line(
        OK,
        &subject,
        &format!(
            "model {}, {} form",
            terminal_text(&report.model),
            if report.form_is_builtin {
                "built-in"
            } else {
                "custom"
            }
        ),
    );
    if !report.omitted_flags.is_empty() {
        line(
            OK,
            &subject,
            &format!(
                "not passed, the probe grants no autonomy: {}",
                report.omitted_flags.join(" ")
            ),
        );
    }
    for p in &report.promises {
        if p.kept {
            line(OK, &subject, &format!("{} as promised", p.what));
        } else {
            line(
                FAIL,
                &subject,
                &format!("no {} where the built-in form promises one", p.what),
            );
        }
    }
    let (status, verdict) = match report.conformance {
        apb_core::agents::Conformance::Machine => (OK, "machine output, read as such"),
        apb_core::agents::Conformance::PlainByDesign => (
            OK,
            "plain text, allowed for this agent or form (read as the reply)",
        ),
        apb_core::agents::Conformance::Drifted => (
            FAIL,
            "the built-in form printed no machine output: the CLI's output format changed",
        ),
    };
    line(status, &subject, verdict);
    if let Some(u) = &report.usage {
        let cost = u
            .cost_usd
            .map(|c| format!(", cost ${c:.4}"))
            .unwrap_or_default();
        line(
            OK,
            &subject,
            &format!(
                "reported {} input and {} output tokens{cost}",
                u.input_tokens, u.output_tokens
            ),
        );
    }
    if let Some(e) = &report.exposure {
        line(
            OK,
            &subject,
            &format!(
                "sees {} tools and {} MCP servers",
                e.tools.len(),
                e.servers.len()
            ),
        );
    }
}
