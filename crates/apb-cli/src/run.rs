use std::collections::BTreeMap;
use std::path::Path;
use std::process::{ExitCode, Stdio};
use std::time::{Duration, Instant};

use apb_core::fsutil::atomic_write;
use apb_core::registry::{Registry, is_safe_segment};
use apb_core::validate::{Severity, ValidationContext, validate};
use apb_engine::control::Control;
use apb_engine::run_config::CacheRunMode;
use apb_engine::state::RunStatus;
use apb_engine::{
    ReviewCommand, RunMode, RunOptions, StopOutcome, drive_prepared, list_runs, post_review,
    post_supervisor_command, prepare_supervised_background, resume_with, run, stop_run,
};

use crate::util::open_registry;

/// Runs the one run gate every launch surface uses
/// (`apb_engine::gate::check_run`) for a CLI start - foreground `apb run`,
/// `--detach`, and the `__drive-supervised` child alike - and hands the permit
/// to `opts` verbatim (anti-TOCTOU). A draft or retired playbook, unmet
/// `requires`, an untrusted connector or account, or a broken sub-playbook
/// tree is refused before anything is written. The person typing `apb run` is
/// the trust confirmation, so untrusted playbook and profile content is
/// acknowledged (MCP asks the user first); connector trust is never
/// bypassable. `supervised` is true only when an external supervisor agent
/// will be spawned, so its profile joins the verified bundle set. Consent-time
/// warnings go to stderr. On `Err` this returns a ready-to-print, actionable
/// message (see `gate_refusal_message`).
fn gate_run(
    root: &Path,
    name: &str,
    version: Option<&str>,
    supervised: bool,
    opts: &mut RunOptions,
) -> Result<(), String> {
    let wref = apb_core::scope::PlaybookRef {
        origin: apb_core::scope::Origin::Project { workspace_id: None },
        id: name.to_string(),
        version: version.map(str::to_string),
    };
    let permit = apb_engine::gate::check_run(root, &wref, true, supervised)
        .map_err(|refusal| gate_refusal_message(&refusal))?;
    for w in &permit.warnings {
        eprintln!("warning: {w}");
    }
    permit.apply(opts);
    Ok(())
}

/// Turns a run-gate refusal (see `apb_engine::gate::check_run`) into an
/// actionable CLI message: names the policy code and,
/// for a trust refusal, points at the exact `apb connector approve` invocation
/// that clears it; for a missing-env refusal, at `apb connector env --write`.
/// Falls back to printing the refusal verbatim for a policy code this
/// function does not special-case (e.g. `connector_unresolved`, `not_found`),
/// so a future refusal kind still surfaces something useful rather than
/// nothing.
fn gate_refusal_message(refusal: &serde_json::Value) -> String {
    let policy = refusal
        .get("policy")
        .and_then(|v| v.as_str())
        .unwrap_or("unknown");
    let strings = |field: &str| -> Vec<String> {
        refusal
            .get(field)
            .and_then(|v| v.as_array())
            .map(|a| {
                a.iter()
                    .filter_map(|v| v.as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default()
    };
    match policy {
        "untrusted_connector_requires_approve" => {
            let names = strings("connectors");
            format!(
                "run refused ({policy}): connector(s) not approved: {}. Approve each with \
                 `apb connector approve <name>`, then re-run.",
                names.join(", ")
            )
        }
        "unapproved_connector_account" => {
            let ids = strings("accounts");
            let suggestions: Vec<String> = ids
                .iter()
                .map(|id| match id.split_once('/') {
                    Some((conn, account)) => {
                        format!("apb connector approve {conn} --account {account}")
                    }
                    None => format!("apb connector approve {id}"),
                })
                .collect();
            format!(
                "run refused ({policy}): connector account(s) not approved: {}. Approve with: \
                 {}, then re-run.",
                ids.join(", "),
                suggestions.join("; ")
            )
        }
        "connector_env_missing" => {
            let missing = strings("missing");
            format!(
                "run refused ({policy}): missing required env var(s): {}. Fill them via \
                 `apb connector env --write`, then re-run.",
                missing.join(", ")
            )
        }
        other => format!("run refused ({other}): {refusal}"),
    }
}

pub(crate) fn run_list(root: &Path) -> ExitCode {
    let reg = match open_registry(root) {
        Ok(r) => r,
        Err(c) => return c,
    };
    match reg.list() {
        Ok(list) if list.is_empty() => {
            println!("no playbooks in .apb/playbooks");
            ExitCode::SUCCESS
        }
        Ok(list) => {
            for wfs in list {
                println!(
                    "{}\t{}\t(current: {}, versions: {})",
                    wfs.id,
                    wfs.name,
                    wfs.current,
                    wfs.versions.join(", ")
                );
            }
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("list failed: {e}");
            ExitCode::from(2)
        }
    }
}

/// `apb doctor`, and with `--run <id>` the per-run doctor.
///
/// The two reports print the same way (one `[level] subject: detail` line per
/// check, non-zero exit on a blocking one) because they answer the same kind
/// of question at different scopes, and an operator should not have to learn
/// two output formats while debugging a stuck run.
pub(crate) fn run_doctor(root: &Path, run: Option<&str>) -> ExitCode {
    match run {
        Some(run_id) => doctor_run(root, run_id),
        None => doctor_env(root),
    }
}

/// The per-run doctor. Read-only: it names problems and repairs nothing, so
/// the repair verbs (`apb stop`, resume) stay explicit operator decisions.
fn doctor_run(root: &Path, run_id: &str) -> ExitCode {
    use apb_engine::run_doctor::{FAIL, OK, WARN, diagnose_run, has_failure};
    let checks = match diagnose_run(root, run_id) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("doctor: {e}");
            return ExitCode::from(2);
        }
    };
    for c in &checks {
        let marker = match c.status {
            OK => "[ok]  ",
            WARN => "[warn]",
            FAIL => "[fail]",
            other => other,
        };
        println!("{marker} {}: {}", c.subject, c.detail);
    }
    print_pending_question_check(root, run_id);
    if has_failure(&checks) {
        eprintln!("doctor: found blocking problems");
        ExitCode::from(1)
    } else {
        ExitCode::SUCCESS
    }
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
        println!(
            "[warn] pending question: node `{}`: {}",
            pq.node,
            sanitize_for_terminal(&pq.question, QUESTION_TEXT_MAX)
        );
    }
}

/// Cap on a sanitized question's rendered length (spec 2026-07-20-interactive-
/// nodes, Security section, fix round 1): long enough to be useful on both
/// the `apb runs` table line and the `apb doctor --run` check line, short
/// enough that a maliciously long question cannot dominate either report.
const QUESTION_TEXT_MAX: usize = 160;

/// Renders agent-generated (untrusted) question text as safe, single-line
/// plain text for a terminal (spec 2026-07-20-interactive-nodes, Security
/// section, fix round 1): the node asking the question is under the
/// playbook author's control, but the question TEXT is model output, so it
/// must not be interpreted as anything other than literal characters.
///
/// Every control character - including the ESC byte that opens an ANSI CSI
/// sequence, embedded `\r`, and `\n` - becomes a space, which both strips
/// the escape channel and guarantees the result cannot break the caller's
/// single-line indent (a raw `\n` would otherwise let the question text
/// forge extra report lines). Runs of whitespace then collapse to one
/// space, the result is trimmed, and it is capped at `max` chars
/// (appending "..." when cut) so one long question cannot dominate the
/// report it appears in. Shared by both print sites that render a pending
/// question's text (`print_waiting_on_question`, `print_pending_question_check`);
/// node ids are not routed through this - they are already validated safe
/// segments, not model output.
fn sanitize_for_terminal(s: &str, max: usize) -> String {
    let cleaned: String = s
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect();
    let collapsed = cleaned.split_whitespace().collect::<Vec<_>>().join(" ");
    let trimmed = collapsed.trim();
    if trimmed.chars().count() <= max {
        trimmed.to_string()
    } else {
        let truncated: String = trimmed.chars().take(max).collect();
        format!("{truncated}...")
    }
}

fn doctor_env(root: &Path) -> ExitCode {
    use apb_core::doctor::{CheckStatus, diagnose};
    let report = diagnose(root);
    for c in &report.checks {
        let marker = match c.status {
            CheckStatus::Ok => "[ok]  ",
            CheckStatus::Warn => "[warn]",
            CheckStatus::Fail => "[fail]",
        };
        println!("{marker} {}: {}", c.name, c.detail);
    }
    match crate::dashboard_check::dashboard_version_line() {
        Some(Ok(detail)) => println!("[ok]   dashboard: {detail}"),
        Some(Err(detail)) => println!("[warn] dashboard: {detail}"),
        None => {}
    }
    if report.has_failure() {
        eprintln!("doctor: found blocking problems");
        ExitCode::from(1)
    } else {
        ExitCode::SUCCESS
    }
}

pub(crate) fn run_validate(root: &Path, name: Option<String>) -> ExitCode {
    let reg = match open_registry(root) {
        Ok(r) => r,
        Err(c) => return c,
    };
    let validate_all = name.is_none();
    // Whole-project validation enumerates by directory, not through the
    // listing: `Registry::list` drops a playbook that fails to load, which
    // would hide exactly the definitions a validator exists to report.
    let names: Vec<String> = match name {
        Some(n) => vec![n],
        None => reg.playbook_ids(),
    };
    let ctx =
        ValidationContext::for_registry(&reg, apb_core::profile_store::PlaybookOrigin::Project);
    let mut failed = false;
    for id in names {
        match reg.load(&id, None) {
            Ok(loaded) => {
                let report = validate(&loaded.playbook, &ctx);
                for issue in &report.issues {
                    let sev = match issue.severity {
                        Severity::Error => "error",
                        Severity::Warning => "warning",
                    };
                    println!(
                        "{id}: {sev} {} {}{}",
                        issue.code,
                        issue.message,
                        issue
                            .node
                            .as_ref()
                            .map(|n| format!(" (node `{n}`)"))
                            .unwrap_or_default()
                    );
                }
                // The local preflight (requires, connectors): warnings, since
                // another machine may well meet them; the run gate refuses.
                for (code, message) in apb_core::preflight::findings(root, &loaded.playbook) {
                    println!("{id}: warning {code} {message}");
                }
                if report.is_valid() {
                    println!("{id}: OK");
                } else {
                    failed = true;
                }
            }
            Err(e) => {
                println!("{id}: error {e}");
                failed = true;
            }
        }
    }
    if validate_all && !validate_profile_models(root, &ctx.profiles) {
        failed = true;
    }
    if failed {
        ExitCode::from(1)
    } else {
        ExitCode::SUCCESS
    }
}

/// Whole-project `apb validate` also checks every project profile's models
/// ([`apb_core::model_check`]): a model zcode's allowlist or the config's
/// `model_policy` refuses is an error (`zcode_model_not_allowed`,
/// `model_policy_violation`); one outside apb's list for its agent, or one the
/// installed agent does not list, is a warning. Returns whether no profile had
/// an error. An unreadable profile is left to the run-time resolver, which
/// reports it with its own error.
fn validate_profile_models(root: &Path, names: &[String]) -> bool {
    use apb_core::model_check::{self, ModelIssue};
    let docs: Vec<(&String, apb_core::profile::ProfileDoc)> = names
        .iter()
        .filter_map(|name| {
            let path = root.join(".apb/profiles").join(name).join("profile.yaml");
            let doc = std::fs::read_to_string(&path)
                .ok()
                .and_then(|y| apb_core::profile::ProfileDoc::from_yaml(&y).ok())?;
            Some((name, doc))
        })
        .collect();
    if docs.is_empty() {
        return true;
    }
    let cx = model_check::ModelContext::load();
    let mut ok = true;
    for (name, doc) in &docs {
        // The executor, its fallbacks and every tier (issue #165 Part 12).
        for problem in doc.tiers.problems() {
            println!("profile {name}: error profile_tiers_invalid {problem}");
            ok = false;
        }
        for (agent, model) in doc.executor_pairs() {
            let Some(issue) = model_check::check(agent, model, &cx) else {
                continue;
            };
            match &issue {
                ModelIssue::NotAllowed(refusal) => {
                    println!("profile {name}: error zcode_model_not_allowed {refusal}");
                }
                ModelIssue::PolicyViolation(_) => println!(
                    "profile {name}: error {} {}",
                    issue.code(),
                    issue.describe(agent, model)
                ),
                ModelIssue::Unknown(_) | ModelIssue::NotAvailable => println!(
                    "profile {name}: warning {} {}",
                    issue.code(),
                    issue.describe(agent, model)
                ),
                ModelIssue::AgentNotInstalled | ModelIssue::Unverifiable => {}
            }
            if issue.is_blocking() {
                ok = false;
            }
        }
    }
    ok
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn run_cmd(
    root: &Path,
    name: &str,
    version: Option<&str>,
    instruction: Option<String>,
    params: Vec<String>,
    allow_shared_workdir: bool,
    supervise: bool,
    detach: bool,
    overrides_path: Option<&Path>,
    no_cache: bool,
    refresh_cache: bool,
    continued_from: Option<String>,
    worktree: Option<String>,
) -> ExitCode {
    if Registry::open(root).is_err() {
        eprintln!("no project here (run `apb init`)");
        return ExitCode::from(2);
    }
    // clap's `conflicts_with` already refuses `--no-cache --refresh-cache`
    // together before we get here; this is just the flags-to-enum mapping.
    let cache = if no_cache {
        CacheRunMode::Off
    } else if refresh_cache {
        CacheRunMode::Refresh
    } else {
        CacheRunMode::Auto
    };
    let mut parsed = BTreeMap::new();
    for p in params {
        match p.split_once('=') {
            Some((k, v)) => {
                parsed.insert(k.to_string(), v.to_string());
            }
            None => {
                eprintln!("bad --param `{p}` (expected key=value)");
                return ExitCode::from(2);
            }
        }
    }
    // Run-level overrides from a yaml file (spec 11).
    let overrides = match overrides_path {
        Some(path) => match std::fs::read_to_string(path)
            .map_err(|e| e.to_string())
            .and_then(|raw| apb_core::overrides::RunOverrides::from_yaml(&raw))
        {
            Ok(o) => Some(o),
            Err(e) => {
                eprintln!("bad --overrides `{}`: {e}", path.display());
                return ExitCode::from(2);
            }
        },
        None => None,
    };
    if supervise && overrides.is_some() {
        eprintln!("--overrides is not yet supported together with --supervise");
        return ExitCode::from(2);
    }
    if supervise && cache != CacheRunMode::Auto {
        eprintln!("--no-cache/--refresh-cache is not yet supported together with --supervise");
        return ExitCode::from(2);
    }
    if supervise {
        // Background (non-blocking) supervised run: the engine itself spawns
        // a background agent and watches its heartbeat. The drive loop
        // itself cannot stay in the current process - std::thread does not
        // outlive its own process, and this CLI invocation must return right
        // after printing the run_id (see spawn_detached_supervised) - so the
        // drive loop moves into a separate OS process detached from the
        // parent (the hidden `__drive-supervised` subcommand).
        let param_args: Vec<String> = parsed
            .into_iter()
            .map(|(k, v)| format!("{k}={v}"))
            .collect();
        return spawn_detached_supervised(
            root,
            name,
            version,
            instruction.as_deref(),
            &param_args,
            allow_shared_workdir,
            continued_from.as_deref(),
            worktree.as_deref(),
        );
    }
    let mut opts = RunOptions {
        instruction,
        params: parsed,
        allow_shared_workdir,
        mode: RunMode::Autonomous,
        max_patches_per_run: None,
        context_max_bytes: None,
        context_compact_model: None,
        overrides,
        parent_run: None,
        continued_from,
        depth: 0,
        cache,
        max_parallel: None,
        // Fail-fast on a busy workdir: this caller is a person waiting on the
        // answer, who can retry, not an event source whose event dies with the
        // refusal (see `RunOptions::workdir_queue_wait`).
        workdir_queue_wait: None,
        worktree,
        // The `expected_*` pins come from the run gate (`gate_run`).
        ..Default::default()
    };
    if let Err(msg) = gate_run(root, name, version, false, &mut opts) {
        eprintln!("run failed: {msg}");
        return ExitCode::from(2);
    }
    if detach {
        return match apb_engine::start_detached(root, name, version, opts) {
            Ok(run_id) => {
                println!("run started: {run_id}");
                ExitCode::SUCCESS
            }
            Err(e) => {
                eprintln!("run failed: {e}");
                ExitCode::from(2)
            }
        };
    }
    match run(root, name, version, opts) {
        Ok(res) => {
            println!("run {} finished: {}", res.run_id, res.outcome.as_str());
            match res.outcome {
                RunStatus::Succeeded => ExitCode::SUCCESS,
                _ => ExitCode::from(1),
            }
        }
        Err(e) => {
            eprintln!("run failed: {e}");
            ExitCode::from(2)
        }
    }
}

/// Spawns `playbook __drive-supervised ...` as a separate OS process detached
/// from the current one (null stdio, we do not wait for it to finish) - it is
/// this child, not a thread of the current process, that actually drives the
/// run, and it will outlive this CLI invocation. Waits for a handshake file
/// (short polling, on the order of seconds, not the duration of the run
/// itself) with the run_id, which the child writes right after preparing the
/// run (before drive starts) - and only then prints
/// "supervised run started: <run_id>" and returns control without waiting
/// for the run itself to finish.
#[allow(clippy::too_many_arguments)]
pub(crate) fn spawn_detached_supervised(
    root: &Path,
    name: &str,
    version: Option<&str>,
    instruction: Option<&str>,
    param_args: &[String],
    allow_shared_workdir: bool,
    continued_from: Option<&str>,
    worktree: Option<&str>,
) -> ExitCode {
    let exe = match apb_core::fsutil::reexec_exe() {
        Ok(e) => e,
        Err(e) => {
            eprintln!("run failed: cannot resolve own executable: {e}");
            return ExitCode::from(2);
        }
    };
    let handshake = std::env::temp_dir().join(format!(
        "apb-supervise-handshake-{}-{}.txt",
        std::process::id(),
        apb_core::clock::now_ms(),
    ));

    let mut cmd = std::process::Command::new(&exe);
    cmd.arg("__drive-supervised").arg(name);
    if let Some(v) = version {
        cmd.arg("--version").arg(v);
    }
    if let Some(instr) = instruction {
        cmd.arg("--instruction").arg(instr);
    }
    for p in param_args {
        cmd.arg("--param").arg(p);
    }
    if allow_shared_workdir {
        cmd.arg("--allow-shared-workdir");
    }
    if let Some(pred) = continued_from {
        cmd.arg("--continued-from").arg(pred);
    }
    if let Some(tree) = worktree {
        cmd.arg("--worktree").arg(tree);
    }
    cmd.arg("--handshake").arg(&handshake);
    cmd.current_dir(root);
    cmd.stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());

    let child = match apb_core::fsutil::spawn_when_not_busy(&mut cmd) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("run failed: cannot spawn supervised drive process: {e}");
            return ExitCode::from(2);
        }
    };
    // We do not wait for the child (`wait`) - this is exactly what makes the
    // run non-blocking for the caller; dropping `Child` orphans the process,
    // which is intentional (the same trick as in
    // ClaudeAdapter::spawn_supervisor).
    drop(child);

    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if let Ok(content) = std::fs::read_to_string(&handshake)
            && !content.is_empty()
        {
            let _ = std::fs::remove_file(&handshake);
            if let Some(msg) = content.strip_prefix("ERR: ") {
                eprintln!("run failed: {msg}");
                return ExitCode::from(2);
            }
            println!("supervised run started: {content}");
            return ExitCode::SUCCESS;
        }
        if Instant::now() > deadline {
            let _ = std::fs::remove_file(&handshake);
            eprintln!("run failed: supervised drive process did not report a run_id in time");
            return ExitCode::from(2);
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// Body of the hidden `__drive-supervised` subcommand - runs in a separate
/// process detached from the parent (see `spawn_detached_supervised`).
/// Synchronously prepares the run (the same path as `run_background`:
/// registration, validation, run_dir, workdir lock, initial spawn of the
/// background agent), reports the run_id through the handshake file, and
/// only then drives the run forward - the whole drive loop is synchronous in
/// THIS process, which is what lets the run outlive the original CLI
/// invocation.
#[allow(clippy::too_many_arguments)]
pub(crate) fn drive_supervised_child(
    root: &Path,
    name: &str,
    version: Option<&str>,
    instruction: Option<String>,
    params: Vec<String>,
    allow_shared_workdir: bool,
    continued_from: Option<String>,
    worktree: Option<String>,
    handshake: &Path,
) -> ExitCode {
    let mut parsed = BTreeMap::new();
    for p in params {
        match p.split_once('=') {
            Some((k, v)) => {
                parsed.insert(k.to_string(), v.to_string());
            }
            None => {
                let _ = atomic_write(
                    handshake,
                    format!("ERR: bad --param `{p}` (expected key=value)").as_bytes(),
                );
                return ExitCode::from(2);
            }
        }
    }
    let mut opts = RunOptions {
        instruction,
        params: parsed,
        allow_shared_workdir,
        mode: RunMode::AgentSupervised,
        max_patches_per_run: None,
        context_max_bytes: None,
        context_compact_model: None,
        overrides: None,
        parent_run: None,
        continued_from,
        depth: 0,
        cache: Default::default(),
        max_parallel: None,
        // Fail-fast on a busy workdir: this caller is a person waiting on the
        // answer, who can retry, not an event source whose event dies with the
        // refusal (see `RunOptions::workdir_queue_wait`).
        workdir_queue_wait: None,
        worktree,
        // The `expected_*` pins come from the run gate (`gate_run`).
        ..Default::default()
    };
    if let Err(msg) = gate_run(root, name, version, true, &mut opts) {
        let _ = atomic_write(handshake, format!("ERR: {msg}").as_bytes());
        return ExitCode::from(2);
    }
    let prepared = match prepare_supervised_background(root, name, version, opts) {
        Ok(p) => p,
        Err(e) => {
            let _ = atomic_write(handshake, format!("ERR: {e}").as_bytes());
            return ExitCode::from(2);
        }
    };
    if atomic_write(handshake, prepared.run_id().as_bytes()).is_err() {
        // The parent can no longer learn the run_id - best effort: we keep
        // driving the run forward anyway, it just won't show up in the
        // parent's stdout (it's visible via `apb runs`/`.apb/runs`).
    }
    match drive_prepared(root, prepared) {
        Ok(res) => match res.outcome {
            RunStatus::Succeeded => ExitCode::SUCCESS,
            _ => ExitCode::from(1),
        },
        Err(_) => ExitCode::from(1),
    }
}

/// Body of the hidden `__drive-run` subcommand: the detached driver process.
/// The run was already prepared (or already ran, for `--resume`) by whoever
/// spawned us - CLI, MCP server, anything that calls
/// `apb_engine::driver::spawn_detached_driver` - and everything this process
/// needs is on disk under `runs/<run_id>`. The whole drive loop is synchronous
/// HERE, which is what lets the run outlive the process that launched it.
///
/// Stdio is normally nulled by the spawner, so the exit code carries the
/// outcome; diagnostics still go to stderr for the case where the command is
/// invoked directly.
pub(crate) fn drive_run_child(
    root: &Path,
    run_id: &str,
    from_node: Option<&str>,
    resume: bool,
    allow_environment_drift: bool,
) -> ExitCode {
    let res = if resume {
        apb_engine::resume_with(root, run_id, from_node, allow_environment_drift)
    } else {
        apb_engine::drive_run_from_dir(root, run_id)
    };
    match res {
        Ok(r) => match r.outcome {
            RunStatus::Succeeded => ExitCode::SUCCESS,
            _ => ExitCode::from(1),
        },
        Err(e) => {
            // A detached driver's stdio is nulled by the spawner, so an error
            // here would vanish with the process. Record it as a RunError event
            // so `run_status` and `apb doctor --run` show why the resume never
            // moved the run (issue #45 finding 3). Best effort: a write failure
            // must not mask the original error we are about to report on stderr.
            let reason = e.to_string();
            if let Err(write_err) = apb_engine::record_run_error(root, run_id, None, &reason) {
                eprintln!(
                    "drive of run `{run_id}` failed: {e}; could not record startup error to run log: {write_err}"
                );
            } else {
                eprintln!("drive of run `{run_id}` failed: {e}");
            }
            ExitCode::from(2)
        }
    }
}

pub(crate) fn runs_cmd(root: &Path, run_id: Option<&str>) -> ExitCode {
    if let Some(run_id) = run_id {
        return run_detail_cmd(root, run_id);
    }
    match list_runs(root) {
        Ok(runs) if runs.is_empty() => {
            println!("no runs yet");
            ExitCode::SUCCESS
        }
        Ok(runs) => {
            for r in runs {
                // The status column stays exactly as it always has (a script
                // parsing it must keep working); a dead driver and events a
                // newer apb wrote are called out as appended markers rather
                // than a rewrite of that text (#85 finding 4).
                let mut line = format!("{}\t{}\t{}", r.run_id, r.playbook, r.status);
                if r.driver_dead {
                    line.push_str("\tdriver dead");
                }
                if r.unknown_events > 0 {
                    line.push('\t');
                    line.push_str(&unknown_events_note(r.unknown_events));
                }
                println!("{line}");
                print_waiting_on_question(r.progress.as_ref());
            }
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("runs failed: {e}");
            ExitCode::from(2)
        }
    }
}

/// The note for events a newer apb wrote that this binary skipped.
fn unknown_events_note(n: usize) -> String {
    let events = if n == 1 { "event" } else { "events" };
    format!("{n} unknown {events} (newer apb?)")
}

/// `apb runs <run_id>`: one run through the same run view as `run_status`,
/// with the token usage its attempts reported when there is any.
fn run_detail_cmd(root: &Path, run_id: &str) -> ExitCode {
    if !is_safe_segment(run_id) {
        eprintln!("runs: invalid run id `{run_id}`");
        return ExitCode::from(2);
    }
    let run_dir = root.join(".apb/runs").join(run_id);
    if !run_dir.is_dir() {
        eprintln!("runs: run `{run_id}` not found");
        return ExitCode::from(2);
    }
    let view = match apb_engine::run_view::RunView::load(&run_dir, run_id) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("runs failed: {e}");
            return ExitCode::from(2);
        }
    };
    let playbook = view
        .events
        .iter()
        .find_map(|e| match &e.payload {
            apb_engine::event::EventPayload::RunStarted {
                playbook, version, ..
            } => Some(format!("{playbook} {version}")),
            _ => None,
        })
        .unwrap_or_default();
    println!("{run_id}\t{playbook}\t{}", view.run_status.as_str());
    if view.driver_alive == Some(false) {
        println!("  driver dead");
    }
    for (node, status) in view.nodes() {
        println!("  {node}\t{status}");
    }
    if let Some(reason) = view.failure_reason() {
        println!(
            "  failure: {}",
            sanitize_for_terminal(&reason, QUESTION_TEXT_MAX)
        );
    }
    if let Some(u) = view.usage() {
        println!("  usage: {}", usage_line(&u));
    }
    if let Some(d) = view.decisions() {
        println!("  decisions: {}", d.line());
    }
    if !view.unknown.is_empty() {
        println!("  {}", unknown_events_note(view.unknown.len()));
    }
    print_waiting_on_question(view.progress.as_ref());
    ExitCode::SUCCESS
}

/// One line of token totals: `1200 input, 300 output, 5000 cache read, 0
/// cache write tokens over 2 attempts, $0.0123 reported`.
fn usage_line(u: &apb_engine::run_view::RunUsage) -> String {
    let attempts = if u.attempts == 1 {
        "attempt"
    } else {
        "attempts"
    };
    let mut line = format!(
        "{} input, {} output, {} cache read, {} cache write tokens over {} {attempts}",
        u.input_tokens, u.output_tokens, u.cache_read_tokens, u.cache_write_tokens, u.attempts
    );
    if u.estimated {
        line.push_str(" (partly estimated by apb)");
    }
    if let Some(cost) = u.cost_usd {
        line.push_str(&format!(", ${cost:.4} reported"));
        if u.cost_attempts < u.attempts {
            line.push_str(&format!(" by {} of them", u.cost_attempts));
        }
    }
    line
}

/// Prints a waiting-on-question marker line (node id and question text,
/// plain text - no shell interpolation, just `println!`) when `progress`
/// carries a pending question (spec 2026-07-20-interactive-nodes, Task 9).
/// A no-op otherwise, so a run not parked on a question prints nothing extra.
fn print_waiting_on_question(progress: Option<&apb_engine::ProgressSummary>) {
    if let Some(pq) = progress.and_then(|p| p.pending_question.as_ref()) {
        println!(
            "  waiting on question (node `{}`): {}",
            pq.node,
            sanitize_for_terminal(&pq.question, QUESTION_TEXT_MAX)
        );
    }
}

pub(crate) fn resume_cmd(
    root: &Path,
    run_id: &str,
    from_node: Option<&str>,
    allow_environment_drift: bool,
) -> ExitCode {
    // Read BEFORE the drive: a resume of a run with a pending stop applies that
    // stop before it executes anything and returns immediately, which otherwise
    // looks like a resume that silently did nothing. Best effort - an
    // unreadable control queue must not fail the resume itself.
    let pending_stop = apb_engine::control::pending_stop_seq(&root.join(".apb/runs").join(run_id))
        .ok()
        .flatten()
        .is_some();
    match resume_with(root, run_id, from_node, allow_environment_drift) {
        Ok(res) => {
            println!("resume {} finished: {}", res.run_id, res.outcome.as_str());
            if pending_stop && res.outcome == RunStatus::Aborted {
                println!(
                    "this resume only applied a stop that was still pending, so nothing else ran; resume again to continue past it"
                );
            }
            match res.outcome {
                RunStatus::Succeeded => ExitCode::SUCCESS,
                _ => ExitCode::from(1),
            }
        }
        Err(e) => {
            eprintln!("resume failed: {e}");
            ExitCode::from(2)
        }
    }
}

/// Stops a run: `apb stop <run_id>`.
///
/// Posts the abort, which the driving process picks up within a fraction of a
/// second and uses to kill whatever agent the run has in flight. When no
/// process is driving the run any more - a driver that crashed, taking the run
/// down with it and leaving it reading `running` forever - the stop finalizes
/// the run itself. `stop_run` validates `run_id` and existence.
/// `apb wait`: blocks on the run with no model in the loop and reports why it
/// returned. The exit code carries the verdict so a shell caller needs no
/// parsing: 0 succeeded, 1 failed/aborted, 3 needs input, 4 stopped, 5 timeout.
pub(crate) fn wait_cmd(root: &Path, run_id: &str, timeout_secs: Option<u64>) -> ExitCode {
    use apb_engine::run_wait::{NeedsInput, WaitReason, wait_run};
    // No limit by default: an agent runs this as one background command and is
    // notified when it exits. A century is "no limit" without overflow.
    let timeout = Duration::from_secs(timeout_secs.unwrap_or(100 * 365 * 24 * 3600));
    let res = match wait_run(root, run_id, timeout) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("wait failed: {e}");
            return ExitCode::from(2);
        }
    };
    let status = res.status.as_str();
    let decisions = res.view.decisions();
    let print_decisions = || {
        if let Some(d) = &decisions {
            println!("  decisions: {}", d.line());
        }
    };
    match res.reason {
        WaitReason::Finished => {
            println!("run {run_id} finished: {status}");
            print_decisions();
            if res.status == RunStatus::Succeeded {
                ExitCode::SUCCESS
            } else {
                ExitCode::from(1)
            }
        }
        WaitReason::NeedsInput => {
            let how = match res.needs {
                Some(NeedsInput::Question) => {
                    format!("a question is pending: `apb answer {run_id} <text>`")
                }
                Some(NeedsInput::Review) => format!(
                    "a human review is pending: `apb review {run_id} <node> --decision <option>`"
                ),
                _ => "a supervisor decision is pending".to_string(),
            };
            println!("run {run_id} needs input ({status}): {how}; then `apb wait {run_id}` again");
            print_decisions();
            ExitCode::from(3)
        }
        WaitReason::Stopped => {
            if res.driver_alive == Some(false) {
                println!(
                    "run {run_id} stopped ({status}): its driver is dead; `apb resume {run_id}` continues it"
                );
            } else {
                println!("run {run_id} stopped ({status}): `apb resume {run_id}` continues it");
            }
            print_decisions();
            ExitCode::from(4)
        }
        WaitReason::Timeout => {
            println!("run {run_id} still {status} after the timeout");
            print_decisions();
            ExitCode::from(5)
        }
    }
}

pub(crate) fn stop_cmd(root: &Path, run_id: &str) -> ExitCode {
    match stop_run(root, run_id) {
        Ok(StopOutcome::SignaledLiveDriver) => {
            println!("stopping {run_id}: abort sent to the running driver");
            ExitCode::SUCCESS
        }
        Ok(StopOutcome::FinalizedDeadRun) => {
            println!("stopped {run_id}: no driver was running, the run is now aborted");
            ExitCode::SUCCESS
        }
        Ok(StopOutcome::AlreadyTerminal) => {
            // Deliberately not "nothing to stop": this outcome also covers a
            // run that finished while the stop was in flight, in which case an
            // abort has already been posted. What is true in both cases is
            // that the run had reached a terminal state on its own.
            println!("{run_id} had already finished, so no run was stopped");
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("stop failed: {e}");
            ExitCode::from(2)
        }
    }
}

/// Posts a supervisor note (`Control::ContextAppend`) to a run's control
/// channel: `runs/<id>/control.jsonl`. Applied at the nearest drive-loop
/// iteration boundary (top-of-loop scan, or immediately if the run is
/// currently waiting in `await_control`) - the note lands in context.md and
/// every subsequent `{{run.context}}` render, same as the MCP
/// `supervisor_context_append` tool. `post_supervisor_command` validates
/// `run_id` and existence itself; no separate check needed here.
pub(crate) fn note_cmd(root: &Path, run_id: &str, text: &str) -> ExitCode {
    match post_supervisor_command(
        root,
        run_id,
        Control::ContextAppend {
            note: text.to_string(),
        },
    ) {
        Ok(seq) => {
            println!("note posted for {run_id} (seq {seq})");
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("note failed: {e}");
            ExitCode::from(2)
        }
    }
}

pub(crate) fn review_cmd(
    root: &Path,
    run_id: &str,
    node_id: &str,
    decision: &str,
    note: &str,
) -> ExitCode {
    if !is_safe_segment(run_id) {
        eprintln!("review failed: invalid run id");
        return ExitCode::from(2);
    }
    let run_dir = root.join(".apb/runs").join(run_id);
    if !run_dir.is_dir() {
        eprintln!("review failed: run `{run_id}` not found");
        return ExitCode::from(2);
    }
    let cmd = ReviewCommand {
        node: node_id.to_string(),
        decision: decision.to_string(),
        note: note.to_string(),
    };
    match post_review(&run_dir, cmd) {
        Ok(seq) => {
            println!("review posted for {run_id}/{node_id}: {decision} (seq {seq})");
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("review failed: {e}");
            ExitCode::from(2)
        }
    }
}

/// Answers an interactive `agent_task` node's pending question (spec
/// 2026-07-20-interactive-nodes, Task 9): appends to the run's
/// `answers.jsonl` channel via `apb_engine::post_answer`, always as
/// `answered_by: "human"` - this is the plain/operator path, the same one
/// the MCP `run_answer` tool's `run_id` branch takes (`token` is the
/// separate supervisor-session path, not exposed here). `node` omitted
/// resolves to the single pending question; an ambiguous or absent pending
/// question is `post_answer`'s own error, which already names what is wrong
/// (which nodes are pending, or that none are) - this only adds the run id,
/// so the error is actionable without an operator having to guess which run
/// it came from.
///
/// The existence check (mirroring `review_cmd`, not `note_cmd`) is needed
/// here because `post_answer` itself does not validate `run_id` or the run
/// directory's existence - unlike `post_supervisor_command`, which `note_cmd`
/// leans on for that. Without it an unknown or path-traversing run id would
/// fall through to `post_answer`'s "no pending question" error instead of a
/// clear "not found".
pub(crate) fn answer_cmd(root: &Path, run_id: &str, node: Option<&str>, text: &str) -> ExitCode {
    if !is_safe_segment(run_id) {
        eprintln!("answer failed: run `{run_id}` not found");
        return ExitCode::from(2);
    }
    let run_dir = root.join(".apb/runs").join(run_id);
    if !run_dir.is_dir() {
        eprintln!("answer failed: run `{run_id}` not found");
        return ExitCode::from(2);
    }
    match apb_engine::post_answer(&run_dir, node, text, "human") {
        Ok(seq) => {
            println!("answer posted for {run_id} (seq {seq})");
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("answer failed for {run_id}: {e}");
            ExitCode::from(2)
        }
    }
}
