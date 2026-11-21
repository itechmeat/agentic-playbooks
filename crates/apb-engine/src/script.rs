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
    pub stderr: String,
}

/// List of candidate runtimes for a runner key. First the global config
/// (`runners: { ts: [bun, deno], ... }`), then the built-in default. This
/// way the registry can be extended via config with new keys without
/// touching code (spec 7.4). An unknown key with no config entry is an error.
fn runner_candidates_in(
    cfg: &apb_core::config::GlobalConfig,
    runner: &str,
) -> Result<Vec<String>, EngineError> {
    cfg.runner_candidates(runner)
        .ok_or_else(|| EngineError::Script(format!("unsupported runner `{runner}`")))
}

/// One runner key as this run resolves it: the candidate list whole (the
/// machine config's `runners` entry or the built-in default), the
/// candidate chosen, the canonical absolute path of the program that is
/// exec'd, the name it is started under (the file name it was found
/// under, so a multi-call binary behind a link behaves as that name), and
/// the identity of the program (`path:size:mtime_ms` of its canonical
/// file, the detector's fingerprint). The same value is what runs and what
/// a list skip's work covers: it is resolved once per run directory and
/// process ([`resolved_runner`]).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct ResolvedRunner {
    pub key: String,
    pub configured: bool,
    pub candidates: Vec<String>,
    pub program: String,
    pub path: String,
    pub argv0: String,
    pub identity: String,
}

/// The runners each run directory resolved in this process.
type RunnerPins =
    std::sync::Mutex<std::collections::HashMap<(std::path::PathBuf, String), ResolvedRunner>>;
static RUNNER_PINS: std::sync::OnceLock<RunnerPins> = std::sync::OnceLock::new();

/// Resolves `runner` from the machine config and `PATH` now, with no pin.
/// A configured candidate given as a relative path (`./runtime`) is taken
/// against one base, the run's project root (the directory holding its
/// `.apb`), never against a script's working directory. The program found
/// is canonicalized, and a program that does not canonicalize fails the
/// resolution: what is exec'd is that absolute path.
fn resolve_runner_now(run_dir: &Path, runner: &str) -> Result<ResolvedRunner, EngineError> {
    // A broken/unreadable config is a clear error, not a silent default:
    // otherwise `runners` overrides would be silently ignored.
    let cfg = apb_core::config::GlobalConfig::load().map_err(EngineError::Script)?;
    let configured = cfg.runners.get(runner).is_some_and(|l| !l.is_empty());
    let candidates = runner_candidates_in(&cfg, runner)?;
    let base = crate::run_config::project_root_of(run_dir).unwrap_or_else(|| run_dir.to_path_buf());
    let locate = |p: &String| {
        let relative = (p.contains('/') || p.contains('\\')) && Path::new(p).is_relative();
        if relative {
            let at = base.join(p);
            apb_core::config::program_location(&at.to_string_lossy())
        } else {
            apb_core::config::program_location(p)
        }
    };
    let (program, found) = candidates
        .iter()
        .find_map(|p| locate(p).map(|at| (p.clone(), at)))
        .ok_or_else(|| {
            EngineError::Script(format!(
                "no runtime available for runner `{runner}` (tried: {})",
                candidates.join(", ")
            ))
        })?;
    let canonical = std::fs::canonicalize(&found).map_err(|e| {
        EngineError::Script(format!(
            "the runtime of runner `{runner}` at {} cannot be canonicalized: {e}",
            found.display()
        ))
    })?;
    let argv0 = found
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| program.clone());
    Ok(ResolvedRunner {
        key: runner.to_string(),
        configured,
        candidates,
        program,
        identity: apb_core::detect::fingerprint(&canonical),
        path: canonical.to_string_lossy().into_owned(),
        argv0,
    })
}

/// `runner` as the run in `run_dir` runs it: resolved the first time this
/// process asks for that run and key, then the same value for every
/// script, script check and work fingerprint of the run in the process. A
/// resolution that fails is not kept, so a later ask resolves again.
pub fn resolved_runner(run_dir: &Path, runner: &str) -> Result<ResolvedRunner, EngineError> {
    let pins = RUNNER_PINS.get_or_init(Default::default);
    let key = (run_dir.to_path_buf(), runner.to_string());
    if let Some(r) = pins.lock().unwrap_or_else(|e| e.into_inner()).get(&key) {
        return Ok(r.clone());
    }
    let resolved = resolve_runner_now(run_dir, runner)?;
    let mut map = pins.lock().unwrap_or_else(|e| e.into_inner());
    Ok(map.entry(key).or_insert(resolved).clone())
}

/// Builds the command for a specific runtime: `bun run <s>`, `deno run -A <s>`,
/// `uv run <s>`, otherwise `<program> <s>` (python3/sh/bash/zsh/node and
/// others). Classification is by the program's basename, so an absolute
/// path like `/opt/homebrew/bin/bun` gets the same arguments as bare `bun`;
/// the full path is still what goes into Command.
fn command_for_runtime(runtime: &ResolvedRunner, script_path: &Path) -> Command {
    let program = runtime.program.as_str();
    let base = Path::new(program)
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or(program);
    let mut c = Command::new(&runtime.path);
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt as _;
        c.arg0(&runtime.argv0);
    }
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
    run_script_with_env(
        version_dir,
        workdir,
        script_rel,
        runner,
        timeout,
        cancel,
        &[],
    )
}

/// The run context a script node gets (C7): `APB_RUN_DIR`, `APB_RUN_ID` and
/// `APB_NODE_ID`, the same names an agent gets. `run_dir`'s name is the id.
pub fn run_env(run_dir: &Path, node_id: Option<&str>) -> Vec<(&'static str, String)> {
    let mut env = vec![("APB_RUN_DIR", run_dir.to_string_lossy().into_owned())];
    if let Some(id) = run_dir.file_name() {
        env.push(("APB_RUN_ID", id.to_string_lossy().into_owned()));
    }
    if let Some(node) = node_id {
        env.push(("APB_NODE_ID", node.to_string()));
    }
    env
}

/// [`run_script`] with extra environment variables for the script.
pub fn run_script_with_env(
    version_dir: &Path,
    workdir: &Path,
    script_rel: &str,
    runner: &str,
    timeout: Option<Duration>,
    cancel: Option<&AtomicBool>,
    env: &[(&str, String)],
) -> Result<ScriptResult, EngineError> {
    run_script_with_env_removed(
        version_dir,
        workdir,
        script_rel,
        runner,
        timeout,
        cancel,
        env,
        &[],
    )
}

/// [`run_script_with_env`] that also removes the `remove` variables from
/// the environment the script inherits. A script never inherits attempt
/// credentials ([`crate::adapter::ATTEMPT_CREDENTIAL_ENV`]): it runs no
/// attempt, and the ones in apb's environment belong to the step of
/// another run that started this drive; `env` sets a list item's own.
#[allow(clippy::too_many_arguments)]
pub fn run_script_with_env_removed(
    version_dir: &Path,
    workdir: &Path,
    script_rel: &str,
    runner: &str,
    timeout: Option<Duration>,
    cancel: Option<&AtomicBool>,
    env: &[(&str, String)],
    remove: &[&str],
) -> Result<ScriptResult, EngineError> {
    let runtime = resolved_runner(version_dir, runner)?;
    run_script_with_runtime(
        &runtime,
        version_dir,
        workdir,
        script_rel,
        timeout,
        cancel,
        env,
        remove,
    )
}

/// [`run_script_with_env_removed`] under a runtime resolved before: a list
/// item of a skip visit runs under the runtime its visit's work names.
#[allow(clippy::too_many_arguments)]
pub(crate) fn run_script_with_runtime(
    runtime: &ResolvedRunner,
    version_dir: &Path,
    workdir: &Path,
    script_rel: &str,
    timeout: Option<Duration>,
    cancel: Option<&AtomicBool>,
    env: &[(&str, String)],
    remove: &[&str],
) -> Result<ScriptResult, EngineError> {
    let script_path = script_at(version_dir, script_rel)?;
    let mut cmd = command_for_runtime(runtime, &script_path);
    cmd.current_dir(workdir);
    for k in crate::adapter::ATTEMPT_CREDENTIAL_ENV.iter().chain(remove) {
        cmd.env_remove(k);
    }
    for (k, v) in env {
        cmd.env(k, v);
    }
    result(run_capture(cmd, timeout, cancel)?)
}

/// The script's path under `version_dir`, which must be a file.
fn script_at(version_dir: &Path, script_rel: &str) -> Result<std::path::PathBuf, EngineError> {
    let script_path = version_dir.join(script_rel);
    if !script_path.is_file() {
        return Err(EngineError::Script(format!(
            "script not found: {script_rel}"
        )));
    }
    Ok(script_path)
}

fn result(captured: crate::proc::Captured) -> Result<ScriptResult, EngineError> {
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
        stderr: captured.stderr,
    })
}

/// A node's `success_check` (an `sh` script under `version_dir`), run in
/// `workdir`. When the attempt ran in a sandbox, the check runs inside the
/// same sandbox (`wrap`), with the script itself bound read-only: it judges
/// a tree the agent wrote, and may run code the agent wrote.
pub fn run_check(
    version_dir: &Path,
    workdir: &Path,
    script_rel: &str,
    wrap: Option<&crate::sandbox::SandboxPlan>,
) -> Result<ScriptResult, EngineError> {
    let Some(plan) = wrap else {
        return run_script(version_dir, workdir, script_rel, "sh", None, None);
    };
    let script_path = script_at(version_dir, script_rel)?;
    let program = sandboxed_check_program(version_dir)?;
    let mut plan = plan.clone();
    plan.mounts.push(crate::sandbox::view::Mount::ReadOnly(
        script_path.clone(),
        script_path.clone(),
    ));
    let mut cmd = plan.command(program.as_ref(), &[script_path.into_os_string()]);
    // Only the base variables reach the sandbox, never apb's credentials.
    cmd.env_clear();
    for name in apb_core::process_env::base_set() {
        if let Some(v) = std::env::var_os(name) {
            cmd.env(name, v);
        }
    }
    plan.overlay(&mut cmd);
    result(crate::proc::run_capture_in(cmd, None, None, true)?)
}

/// The program a sandboxed `success_check` is started as: the `sh`
/// runner's candidate as written (a bare name the sandbox finds on its
/// `PATH`, or the configured path), never its canonical target, so a
/// multicall binary behind a link (BusyBox) is still started under the
/// name that picks its applet. A skip never covers such a check: `skip`
/// is refused on an isolated node (V288).
fn sandboxed_check_program(version_dir: &Path) -> Result<String, EngineError> {
    Ok(resolved_runner(version_dir, "sh")?.program)
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    /// A runner key resolves once per run directory in a process: the run
    /// keeps the runtime it was fingerprinted with when the machine config
    /// moves the key to another runtime, and another run resolves anew.
    #[test]
    fn a_run_keeps_the_runtime_it_resolved() {
        apb_core::process_mode::testing::in_plain_child(
            "script::tests::a_run_keeps_the_runtime_it_resolved",
            || {
                let cfg = tempfile::tempdir().unwrap();
                // SAFETY: a child process runs this one test on one thread.
                unsafe { std::env::set_var("APB_CONFIG_DIR", cfg.path()) };
                let bin = tempfile::tempdir().unwrap();
                let wrapper = |name: &str| {
                    let p = bin.path().join(name);
                    std::fs::write(&p, "#!/bin/sh\nexec /bin/sh \"$@\"\n").unwrap();
                    use std::os::unix::fs::PermissionsExt;
                    std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
                    p.to_string_lossy().into_owned()
                };
                let (a, b) = (wrapper("rt-a"), wrapper("rt-b"));
                let write_cfg = |p: &str| {
                    std::fs::write(
                        cfg.path().join("config.yaml"),
                        format!("runners:\n  sh: ['{p}']\n"),
                    )
                    .unwrap()
                };
                write_cfg(&a);
                let (one, two) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
                let first = resolved_runner(one.path(), "sh").unwrap();
                assert_eq!(
                    (first.program.as_str(), first.configured),
                    (a.as_str(), true)
                );
                assert!(!first.identity.is_empty());
                write_cfg(&b);
                assert_eq!(
                    resolved_runner(one.path(), "sh").unwrap(),
                    first,
                    "the run keeps its runtime"
                );
                assert_eq!(
                    resolved_runner(two.path(), "sh").unwrap().program,
                    b,
                    "another run resolves anew"
                );
            },
        );
    }

    /// A sandboxed check starts the `sh` runner under the name it is
    /// configured by, not the canonical target of a link, so a multicall
    /// binary behind the link still picks its applet from that name.
    #[test]
    fn a_sandboxed_check_starts_sh_under_its_configured_name() {
        apb_core::process_mode::testing::in_plain_child(
            "script::tests::a_sandboxed_check_starts_sh_under_its_configured_name",
            || {
                let cfg = tempfile::tempdir().unwrap();
                // SAFETY: a child process runs this one test on one thread.
                unsafe { std::env::set_var("APB_CONFIG_DIR", cfg.path()) };
                let bin = tempfile::tempdir().unwrap();
                let target = bin.path().join("multicall");
                std::fs::write(&target, "#!/bin/sh\nexec /bin/sh \"$@\"\n").unwrap();
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o755)).unwrap();
                let link = bin.path().join("sh");
                std::os::unix::fs::symlink(&target, &link).unwrap();
                let link = link.to_string_lossy().into_owned();
                std::fs::write(
                    cfg.path().join("config.yaml"),
                    format!("runners:\n  sh: ['{link}']\n"),
                )
                .unwrap();
                let version = tempfile::tempdir().unwrap();
                assert_eq!(sandboxed_check_program(version.path()).unwrap(), link);
            },
        );
    }
}
