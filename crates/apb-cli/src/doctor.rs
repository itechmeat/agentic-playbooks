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
use apb_core::scope_move::{ClaimLeft, StaleClaim};
use apb_core::text::replace_hidden;
use apb_engine::agent_probe::{COST_NOTICE, ProbeRequest, probe, refusal};
use apb_engine::next_action::command::shell_quote;
use apb_engine::run_doctor::{FAIL, OK, WARN};

use crate::util::{QUESTION_TEXT_MAX, StepCaller, sanitize_for_terminal, sanitize_plain};

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
    /// `{ ok, checks: [{ status, subject, detail }] }` instead of lines.
    pub json: bool,
}

pub(crate) fn doctor_cmd(root: &Path, args: DoctorArgs) -> ExitCode {
    if let Some(agent) = &args.probe_agent {
        return doctor_probe(root, agent, args.model.as_deref());
    }
    let mut out = Out::new(args.json);
    if let Some(run_id) = &args.run {
        return doctor_run(root, run_id, out);
    }
    if args.agents {
        let blocking = print_report(&mut out, &apb_core::doctor::diagnose_agents());
        return out.finish(blocking);
    }
    doctor_env(root, out)
}

/// Where the checks go: one line each, or collected for `--json`.
struct Out {
    json: bool,
    checks: Vec<serde_json::Value>,
}

impl Out {
    fn new(json: bool) -> Self {
        Self {
            json,
            checks: Vec::new(),
        }
    }

    fn line(&mut self, status: &str, subject: &str, detail: &str) {
        if self.json {
            self.checks.push(serde_json::json!({
                "status": status,
                "subject": subject,
                "detail": detail,
            }));
        } else {
            line(status, subject, detail);
        }
    }

    /// Prints the JSON report, if asked for, and the exit code.
    fn finish(self, blocking: bool) -> ExitCode {
        if self.json {
            crate::util::print_json(&serde_json::json!({
                "ok": !blocking,
                "checks": self.checks,
            }));
            return if blocking {
                ExitCode::from(1)
            } else {
                ExitCode::SUCCESS
            };
        }
        finish(blocking)
    }
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
fn print_report(out: &mut Out, report: &DoctorReport) -> bool {
    for c in &report.checks {
        let status = match c.status {
            CheckStatus::Ok => OK,
            CheckStatus::Warn => WARN,
            CheckStatus::Fail => FAIL,
        };
        out.line(status, &c.name, &c.detail);
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

/// One line for a playbook directory a stopped copy or draft creation may
/// have left (`apb_core::scope_move::stale_claims`): a removal command only
/// for an empty directory, the one case where removing it can lose nothing
/// (`rmdir` refuses a directory that is no longer empty); the other two ask
/// a person to inspect it. The path in a command is preserved byte for
/// byte: quoted with `shell_quote`, and the command is printed only when
/// the path is UTF-8 and holds no control or hidden character, so the line
/// is never sanitized. A path that cannot be quoted gets no command and is
/// named through `sanitize_plain` like every other path.
fn stale_claim_line(claim: &StaleClaim) -> String {
    let path = &claim.path;
    match claim.left {
        ClaimLeft::Empty => {
            let quotable = path.to_str().filter(|s| replace_hidden(s, ' ') == *s);
            match quotable {
                Some(s) => format!(
                    "empty directory {}: a copy or a draft creation stopped before it \
                     finished; remove it with `rmdir {}`",
                    sanitize_plain(s, SANDBOX_LINE_MAX),
                    shell_quote(s)
                ),
                None => format!(
                    "empty directory {}: a copy or a draft creation stopped before it \
                     finished; remove it by hand",
                    sanitize_plain(&path.to_string_lossy(), SANDBOX_LINE_MAX)
                ),
            }
        }
        ClaimLeft::DraftDebris => format!(
            "directory {} holds only what a stopped draft creation leaves; inspect it \
             before removing anything (creating the draft again may finish it)",
            sanitize_plain(&path.to_string_lossy(), SANDBOX_LINE_MAX)
        ),
        ClaimLeft::Staging => format!(
            "directory {} has no `current` and a staged copy of it is still in the staging \
             area; a copy may still be running, inspect it before removing anything",
            sanitize_plain(&path.to_string_lossy(), SANDBOX_LINE_MAX)
        ),
        ClaimLeft::Other => format!(
            "directory {} has no `current` and holds entries apb did not leave; inspect it",
            sanitize_plain(&path.to_string_lossy(), SANDBOX_LINE_MAX)
        ),
    }
}

/// Joins each claim's line to the failed load the report already holds
/// for that playbook id; the claims no failed load names come back.
fn fold_claims(report: &mut DoctorReport, claims: Vec<StaleClaim>) -> Vec<StaleClaim> {
    let mut unnamed = Vec::new();
    for claim in claims {
        let check = claim
            .path
            .file_name()
            .and_then(|n| n.to_str())
            .map(|id| format!("playbook {id}"))
            .and_then(|name| {
                report
                    .checks
                    .iter_mut()
                    .find(|c| c.name == name && c.status == CheckStatus::Fail)
            });
        match check {
            Some(c) => c.detail = format!("{}; {}", c.detail, stale_claim_line(&claim)),
            None => unnamed.push(claim),
        }
    }
    unnamed
}

fn doctor_env(root: &Path, mut out: Out) -> ExitCode {
    // A project playbook directory a stopped copy or draft creation left is
    // already a failed load in the report: its guidance joins that line,
    // never a second line naming the same directory.
    let now = std::time::SystemTime::now();
    let mut report = apb_core::doctor::diagnose(root);
    let project_store = root.join(".apb").join("playbooks");
    let mut unnamed = fold_claims(
        &mut report,
        apb_core::scope_move::stale_claims(&project_store, now),
    );
    let blocking = print_report(&mut out, &report);
    // The isolation sandbox: not blocking, only `isolation: full` needs it.
    let host = apb_engine::sandbox::probe::host_facts();
    let (ok, detail) = apb_engine::sandbox::probe::doctor_line(&host);
    // The detail carries bwrap's own output and paths: never raw control
    // sequences on the operator's terminal.
    out.line(
        if ok { OK } else { WARN },
        "sandbox",
        &sanitize_for_terminal(&detail, SANDBOX_LINE_MAX),
    );
    match crate::dashboard_check::dashboard_version_line() {
        Some(Ok(detail)) => out.line(OK, "dashboard", &detail),
        Some(Err(detail)) => out.line(WARN, "dashboard", &detail),
        None => {}
    }
    for (warn, subject, detail) in apb_mcp::setup::doctor_lines(root) {
        out.line(if warn { WARN } else { OK }, subject, &detail);
    }
    // A start-key file that does not verify refuses its playbook's keyed
    // starts until a person moves it aside.
    if let Some(detail) = apb_engine::start_key::doctor_line(root) {
        out.line(
            WARN,
            "start keys",
            &sanitize_for_terminal(&detail, SANDBOX_LINE_MAX),
        );
    }
    // A probation ledger that does not verify refuses the unattended starts
    // of its playbook until a person moves it aside.
    if let Some(detail) = apb_engine::probation::doctor_line(root) {
        out.line(
            WARN,
            "probation",
            &sanitize_for_terminal(&detail, SANDBOX_LINE_MAX),
        );
    }
    // Routine health: the watched playbooks, which tripped, which hold.
    if let Some((warn, detail)) =
        apb_engine::health::doctor_line(root, apb_core::clock::now_ms_u64())
    {
        out.line(
            if warn { WARN } else { OK },
            "health",
            &sanitize_for_terminal(&detail, SANDBOX_LINE_MAX),
        );
    }
    // Start rules (docs/TRIGGERS.md): unreadable files, registration, the
    // sweep, what a crashed remove leaves; then every rule a person still
    // has to approve (the tools' list fills `needs_approval`).
    for (status, detail) in apb_engine::trigger::view::doctor_line(root) {
        out.line(
            match status {
                CheckStatus::Ok => OK,
                CheckStatus::Warn => WARN,
                CheckStatus::Fail => FAIL,
            },
            "triggers",
            &sanitize_for_terminal(&detail, SANDBOX_LINE_MAX),
        );
    }
    if let Ok(views) = apb_mcp::tools::trigger::list(root) {
        for view in views {
            if view.needs_approval {
                let name = view.name.clone();
                out.line(
                    WARN,
                    "triggers",
                    &format!("rule `{name}` needs approval: apb trigger approve {name}"),
                );
            }
        }
    }
    // A concurrency slot file that does not verify refuses its playbook's
    // keyed starts and resumes until a person moves it aside.
    if let Some(detail) = apb_engine::concurrency::doctor_line(root) {
        out.line(
            WARN,
            "concurrency",
            &sanitize_for_terminal(&detail, SANDBOX_LINE_MAX),
        );
    }
    // The derived diagrams beside every playbook.yaml (people-in-the-loop
    // D6): a missing, stale or hand-edited one is named; the doctor stays
    // read-only and `apb diagram --rebuild --all` writes them again.
    let mut current = 0u32;
    let mut missing = 0u32;
    let mut stale = 0u32;
    let mut edited = 0u32;
    let mut stores = vec![root.join(".apb").join("playbooks")];
    if let Some(cfg) = apb_core::store::global_playbooks_parent() {
        stores.push(cfg.join("playbooks"));
    }
    for store in &stores {
        let s = apb_core::diagram::scan(store);
        current += s.current;
        missing += s.missing;
        stale += s.stale;
        edited += s.edited;
        // A playbook directory a stopped copy or draft creation may have
        // left: named read-only, a removal command only for an empty one;
        // the project's own are on their failed load line already.
        let claims = if *store == project_store {
            std::mem::take(&mut unnamed)
        } else {
            apb_core::scope_move::stale_claims(store, now)
        };
        for claim in &claims {
            out.line(WARN, "playbooks", &stale_claim_line(claim));
        }
    }
    let total = current + missing + stale + edited;
    if total > 0 {
        if missing + stale + edited == 0 {
            out.line(
                OK,
                "diagrams",
                &sanitize_for_terminal(&format!("{current} current"), SANDBOX_LINE_MAX),
            );
        } else {
            out.line(
                WARN,
                "diagrams",
                &sanitize_for_terminal(
                    &format!(
                        "{missing} missing, {stale} stale, {edited} edited of {total}; \
                         apb diagram --rebuild --all writes them again"
                    ),
                    SANDBOX_LINE_MAX,
                ),
            );
        }
    }
    out.finish(blocking)
}

/// The per-run doctor. Read-only: it names problems and repairs nothing, so
/// the repair verbs (`apb stop`, resume) stay explicit operator decisions.
fn doctor_run(root: &Path, run_id: &str, mut out: Out) -> ExitCode {
    use apb_engine::run_doctor::{diagnose_run, has_failure};
    let checks = match diagnose_run(root, run_id) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("doctor: {e}");
            return ExitCode::from(2);
        }
    };
    for c in &checks {
        out.line(c.status, &c.subject, &c.detail);
    }
    print_pending_question_check(root, run_id, &mut out);
    out.finish(has_failure(&checks))
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
fn print_pending_question_check(root: &Path, run_id: &str, out: &mut Out) {
    let run_dir = root.join(".apb/runs").join(run_id);
    let Ok(events) = apb_engine::run_view::read_events(&run_dir) else {
        return;
    };
    if let Some(pq) =
        apb_engine::progress::from_run_dir(&run_dir, &events).and_then(|p| p.pending_question)
    {
        out.line(
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
    let initiator = apb_engine::gate::launch::Initiator::cli(crate::launch::at_terminal());
    let req = ProbeRequest {
        agent,
        model,
        timeout: PROBE_TIMEOUT,
        project: root,
        initiator: &initiator,
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
            .map(|c| format!(", cost ${c:.4} {}", u.cost_basis.label()))
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

#[cfg(test)]
mod tests {
    use std::ffi::OsStr;

    use apb_core::doctor::Check;
    use std::path::PathBuf;

    use super::*;

    fn claim(path: &str, left: ClaimLeft) -> StaleClaim {
        StaleClaim {
            path: PathBuf::from(OsStr::new(path)),
            left,
        }
    }

    /// A claim the report already names as a failed load joins that line;
    /// one no line names comes back to be printed on its own.
    #[test]
    fn a_claim_joins_the_failed_load_of_its_id() {
        let mut report = DoctorReport {
            checks: vec![Check {
                name: "playbook gone".into(),
                status: CheckStatus::Fail,
                detail: "load failed: playbook `gone` has no current pointer".into(),
            }],
        };
        let unnamed = fold_claims(
            &mut report,
            vec![
                claim("/p/.apb/playbooks/gone", ClaimLeft::Empty),
                claim("/p/.apb/playbooks/other", ClaimLeft::Empty),
            ],
        );
        assert_eq!(report.checks.len(), 1);
        assert!(
            report.checks[0].detail.starts_with("load failed:")
                && report.checks[0]
                    .detail
                    .contains("rmdir /p/.apb/playbooks/gone"),
            "{:?}",
            report.checks[0]
        );
        assert_eq!(unnamed.len(), 1);
        assert_eq!(unnamed[0].path, PathBuf::from("/p/.apb/playbooks/other"));
    }

    /// The path in the removal command goes through `shell_quote` byte for
    /// byte: a space, shell syntax and a quote come back exactly as they
    /// went in, one shell word.
    #[test]
    fn an_empty_claim_command_keeps_the_path_whole() {
        let line = stale_claim_line(&claim("/tmp/a b/$(x)/it's", ClaimLeft::Empty));
        assert!(
            line.ends_with("rmdir '/tmp/a b/$(x)/it'\\''s'`"),
            "the command quotes the path byte for byte: {line}"
        );
    }

    /// A path that is not UTF-8, or holds a control or hidden character,
    /// gets no command: the line names it sanitized and asks for a hand
    /// removal instead.
    #[test]
    fn a_path_that_cannot_be_quoted_gets_no_command() {
        // Not UTF-8: a byte sequence a `str` cannot hold.
        #[cfg(unix)]
        {
            use std::os::unix::ffi::OsStrExt;
            let line = stale_claim_line(&StaleClaim {
                path: PathBuf::from(OsStr::from_bytes(b"/tmp/\xffx")),
                left: ClaimLeft::Empty,
            });
            assert!(!line.contains("rmdir"), "{line}");
            assert!(line.contains("remove it by hand"), "{line}");
        }
        // A newline: a control character, never one shell word.
        let line = stale_claim_line(&claim("/tmp/a\nb", ClaimLeft::Empty));
        assert!(!line.contains("rmdir"), "{line}");
        assert!(line.contains("remove it by hand"), "{line}");
    }

    /// Only an empty directory gets a removal command: a draft creation may
    /// resume, a copy whose staged copy is still there may still be running,
    /// and a name apb does not know may be a person's data.
    #[test]
    fn only_an_empty_claim_gets_a_command() {
        let line = stale_claim_line(&claim("/tmp/gone", ClaimLeft::Empty));
        assert!(line.contains("rmdir "), "{line}");
        for left in [ClaimLeft::DraftDebris, ClaimLeft::Staging, ClaimLeft::Other] {
            let line = stale_claim_line(&claim("/tmp/gone", left));
            assert!(!line.contains("rmdir"), "{line}");
            assert!(!line.contains("rm "), "{line}");
        }
    }
}
