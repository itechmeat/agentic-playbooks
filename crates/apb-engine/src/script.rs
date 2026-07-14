use std::path::Path;
use std::process::Command;
use std::sync::atomic::AtomicBool;
use std::time::Duration;

use crate::error::EngineError;
use crate::proc::run_capture;
use crate::state::NodeStatus;

#[derive(Debug)]
pub struct ScriptResult {
    pub status: NodeStatus,
    pub stdout: String,
}

/// List of candidate runtimes for a runner key. First the global config
/// (`runners: { ts: [bun, deno], ... }`), then the built-in default. This
/// way the registry can be extended via config with new keys without
/// touching code (spec 7.4). An unknown key with no config entry is an error.
fn runner_candidates(runner: &str) -> Result<Vec<String>, EngineError> {
    // A broken/unreadable config is a clear error, not a silent default:
    // otherwise `runners` overrides would be silently ignored.
    let cfg = apb_core::config::GlobalConfig::load().map_err(EngineError::Script)?;
    cfg.runner_candidates(runner)
        .ok_or_else(|| EngineError::Script(format!("unsupported runner `{runner}`")))
}

/// Whether the program is on PATH (or reachable by a direct path). Delegates
/// to the shared helper in apb-core (reused by `apb doctor`).
fn is_in_path(program: &str) -> bool {
    apb_core::config::program_in_path(program)
}

/// Builds the command for a specific runtime: `bun run <s>`, `deno run -A <s>`,
/// `uv run <s>`, otherwise `<program> <s>` (python3/sh/bash/zsh/node and
/// others). Classification is by the program's basename, so an absolute
/// path like `/opt/homebrew/bin/bun` gets the same arguments as bare `bun`;
/// the full path is still what goes into Command.
fn command_for_runtime(program: &str, script_path: &Path) -> Command {
    let base = Path::new(program)
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or(program);
    let mut c = Command::new(program);
    match base {
        "bun" | "uv" => {
            c.arg("run").arg(script_path);
        }
        "deno" => {
            // -A: no access restrictions, so behavior matches bun.
            c.arg("run").arg("-A").arg(script_path);
        }
        _ => {
            c.arg(script_path);
        }
    }
    c
}

pub fn run_script(
    version_dir: &Path,
    workdir: &Path,
    script_rel: &str,
    runner: &str,
    timeout: Option<Duration>,
    cancel: Option<&AtomicBool>,
) -> Result<ScriptResult, EngineError> {
    let script_path = version_dir.join(script_rel);
    if !script_path.is_file() {
        return Err(EngineError::Script(format!(
            "script not found: {script_rel}"
        )));
    }
    let candidates = runner_candidates(runner)?;
    let program = candidates.iter().find(|p| is_in_path(p)).ok_or_else(|| {
        EngineError::Script(format!(
            "no runtime available for runner `{runner}` (tried: {})",
            candidates.join(", ")
        ))
    })?;
    let mut cmd = command_for_runtime(program, &script_path);
    cmd.current_dir(workdir);
    let captured = run_capture(cmd, timeout, cancel)?;
    let status = match captured.status {
        // Cancellation (another join:any branch won) - neither a failure nor a timeout.
        None if captured.cancelled => NodeStatus::Cancelled,
        None => NodeStatus::TimedOut,
        Some(s) if s.success() => NodeStatus::Succeeded,
        Some(_) => NodeStatus::Failed,
    };
    Ok(ScriptResult {
        status,
        stdout: captured.stdout,
    })
}
