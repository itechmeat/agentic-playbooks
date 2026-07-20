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
    post_supervisor_command, prepare_supervised_background, resume, run, stop_run,
};

use crate::util::open_registry;

/// Resolves the two connector permit maps for a playbook before it runs
/// through the CLI (foreground `apb run` and the `__drive-supervised` child
/// alike). A playbook without any connector binding never calls the gate and
/// gets the same empty maps `RunOptions` always defaulted to; this is what
/// keeps a non-connector playbook's behavior byte-for-byte unchanged.
///
/// For a connector-binding playbook this is the same seam the dashboard's
/// `run_playbook_handler` uses (`apb-server/src/lib.rs`) and the same trust
/// gate an MCP-started run goes through (`policy::check_run`): without it the
/// engine would see empty `expected_connectors`/`expected_connector_accounts`
/// and refuse ANY connector-binding run with the opaque "connector bindings
/// present but no connector permit" message, even though nothing was actually
/// checked. On `Err` this returns a ready-to-print, actionable message (see
/// `connector_refusal_message`) instead of the raw refusal JSON.
fn connector_permits_for(
    root: &Path,
    name: &str,
    version: Option<&str>,
) -> Result<apb_mcp::policy::ConnectorPermitMaps, String> {
    let reg = Registry::open(root).map_err(|e| format!("no project here: {e} (run `apb init`)"))?;
    let loaded = reg
        .load(name, version)
        .map_err(|e| format!("cannot load playbook `{name}`: {e}"))?;
    let binds = loaded
        .playbook
        .nodes
        .iter()
        .any(|n| !n.kind.connector_bindings().is_empty());
    if !binds {
        return Ok((BTreeMap::new(), BTreeMap::new()));
    }
    apb_mcp::policy::connector_permit_maps(root, &loaded.playbook)
        .map_err(|refusal| connector_refusal_message(&refusal))
}

/// Turns a connector-gate refusal (see `apb_mcp::policy::check_run`'s
/// connector step) into an actionable CLI message: names the policy code and,
/// for a trust refusal, points at the exact `apb connector approve` invocation
/// that clears it; for a missing-env refusal, at `apb connector env --write`.
/// Falls back to printing the refusal verbatim for a policy code this
/// function does not special-case (e.g. `connector_unresolved`, `not_found`),
/// so a future refusal kind still surfaces something useful rather than
/// nothing.
fn connector_refusal_message(refusal: &serde_json::Value) -> String {
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
    if has_failure(&checks) {
        eprintln!("doctor: found blocking problems");
        ExitCode::from(1)
    } else {
        ExitCode::SUCCESS
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
    let names: Vec<String> = match name {
        Some(n) => vec![n],
        None => match reg.list() {
            Ok(l) => l.into_iter().map(|w| w.id).collect(),
            Err(e) => {
                eprintln!("list failed: {e}");
                return ExitCode::from(2);
            }
        },
    };
    let ctx = ValidationContext {
        profiles: reg.profiles(),
        ..Default::default()
    };
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
    if failed {
        ExitCode::from(1)
    } else {
        ExitCode::SUCCESS
    }
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
    overrides_path: Option<&Path>,
    no_cache: bool,
    refresh_cache: bool,
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
        );
    }
    let (expected_connectors, expected_connector_accounts) =
        match connector_permits_for(root, name, version) {
            Ok(maps) => maps,
            Err(msg) => {
                eprintln!("run failed: {msg}");
                return ExitCode::from(2);
            }
        };
    let opts = RunOptions {
        instruction,
        params: parsed,
        allow_shared_workdir,
        mode: RunMode::Autonomous,
        supervisor_expected: false,
        max_patches_per_run: None,
        context_max_bytes: None,
        context_compact_model: None,
        overrides,
        expected_digest: None,
        expected_profile_bundles: None,
        parent_run: None,
        depth: 0,
        expected_children: None,
        expected_connectors,
        expected_connector_accounts,
        cache,
    };
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
pub(crate) fn spawn_detached_supervised(
    root: &Path,
    name: &str,
    version: Option<&str>,
    instruction: Option<&str>,
    param_args: &[String],
    allow_shared_workdir: bool,
) -> ExitCode {
    let exe = match std::env::current_exe() {
        Ok(e) => e,
        Err(e) => {
            eprintln!("run failed: cannot resolve own executable: {e}");
            return ExitCode::from(2);
        }
    };
    let handshake = std::env::temp_dir().join(format!(
        "apb-supervise-handshake-{}-{}.txt",
        std::process::id(),
        apb_engine::event::now_millis(),
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
    cmd.arg("--handshake").arg(&handshake);
    cmd.current_dir(root);
    cmd.stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());

    let child = match cmd.spawn() {
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
pub(crate) fn drive_supervised_child(
    root: &Path,
    name: &str,
    version: Option<&str>,
    instruction: Option<String>,
    params: Vec<String>,
    allow_shared_workdir: bool,
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
    let (expected_connectors, expected_connector_accounts) =
        match connector_permits_for(root, name, version) {
            Ok(maps) => maps,
            Err(msg) => {
                let _ = atomic_write(handshake, format!("ERR: {msg}").as_bytes());
                return ExitCode::from(2);
            }
        };
    let opts = RunOptions {
        instruction,
        params: parsed,
        allow_shared_workdir,
        mode: RunMode::Supervised,
        supervisor_expected: true,
        max_patches_per_run: None,
        context_max_bytes: None,
        context_compact_model: None,
        overrides: None,
        expected_digest: None,
        expected_profile_bundles: None,
        parent_run: None,
        depth: 0,
        expected_children: None,
        expected_connectors,
        expected_connector_accounts,
        cache: Default::default(),
    };
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
) -> ExitCode {
    let res = if resume {
        apb_engine::resume(root, run_id, from_node)
    } else {
        apb_engine::drive_run_from_dir(root, run_id)
    };
    match res {
        Ok(r) => match r.outcome {
            RunStatus::Succeeded => ExitCode::SUCCESS,
            _ => ExitCode::from(1),
        },
        Err(e) => {
            eprintln!("drive of run `{run_id}` failed: {e}");
            ExitCode::from(2)
        }
    }
}

pub(crate) fn runs_cmd(root: &Path) -> ExitCode {
    match list_runs(root) {
        Ok(runs) if runs.is_empty() => {
            println!("no runs yet");
            ExitCode::SUCCESS
        }
        Ok(runs) => {
            for r in runs {
                println!("{}\t{}\t{}", r.run_id, r.playbook, r.status);
            }
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("runs failed: {e}");
            ExitCode::from(2)
        }
    }
}

pub(crate) fn resume_cmd(root: &Path, run_id: &str, from_node: Option<&str>) -> ExitCode {
    match resume(root, run_id, from_node) {
        Ok(res) => {
            println!("resume {} finished: {}", res.run_id, res.outcome.as_str());
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
