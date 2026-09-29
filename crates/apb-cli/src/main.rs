mod cache;
mod connector;
mod dashboard_check;
mod decisions;
mod manage;
mod onboarding;
mod profile;
mod run;
mod selfupdate;
mod serve;
mod server;
mod suggestions;
// host execution mode (0.23.0)
mod tasks;
mod trash;
mod trust;
mod util;

use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Parser, Subcommand};

use crate::cache::{CacheCmd, cache_cmd};
use crate::connector::{ConnectorAction, connector_cmd};
use crate::decisions::{DecisionsAction, decisions_cmd};
use crate::manage::{
    ProjectsAction, adopt_cmd, detect_cmd, export_cmd, import_cmd, migrate_cmd, projects_cmd,
    run_init, subscriptions_cmd,
};
use crate::profile::{ProfileAction, profile_cmd};
use crate::run::{
    answer_cmd, drive_run_child, drive_supervised_child, note_cmd, resume_cmd, review_cmd, run_cmd,
    run_doctor, run_list, run_validate, runs_cmd, stop_cmd, wait_cmd,
};
use crate::selfupdate::run_self_update;
use crate::serve::{ask_server_cmd, dashboard, dev_cmd, ingest_cmd, mcp_cmd};
use crate::server::{ServerAction, server_cmd};
use crate::suggestions::{SuggestionsAction, suggestions_cmd};
use crate::tasks::{TasksAction, tasks_cmd};
use crate::trash::{TrashAction, trash_cmd};
use crate::trust::{TrustAction, trust_cmd};
use crate::util::{resolve_bind, resolve_port};

#[derive(Parser)]
#[command(name = "apb", version, about = "Playbooks CLI")]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    /// Create empty .apb structure
    Init,
    /// List or manage agent profiles (spec 2026-07-12)
    Profile {
        #[command(subcommand)]
        action: ProfileAction,
    },
    /// Migrate playbooks from schema 1 (executors) to schema 2 (profiles).
    /// Dry-run by default; pass --apply to write.
    Migrate {
        #[arg(long)]
        apply: bool,
    },
    /// List or manage connectors (spec 2026-07-18)
    Connector {
        #[command(subcommand)]
        action: ConnectorAction,
    },
    /// Detect installed coding agents. Detection itself is local: apb runs each
    /// agent's --version and reads local config, and makes no network request of
    /// its own. It does not control what a spawned agent does when apb runs.
    Detect {
        #[arg(long)]
        refresh: bool,
    },
    /// Adoption readiness report for a playbook (or all project playbooks)
    Adopt { name: Option<String> },
    /// View or declare agent subscriptions (spec 8). Bare command lists them;
    /// on a terminal with no prior survey it offers an interactive one.
    Subscriptions {
        /// Mark the survey declined (not offered again)
        #[arg(long)]
        decline: bool,
        /// Declare a subscription: agent[:plan[:coverage]] (repeatable)
        #[arg(long = "set", value_name = "AGENT[:PLAN[:COVERAGE]]")]
        set: Vec<String>,
    },
    /// Inspect and undo the suggestion decisions the agent recorded
    /// (spec 2026-07-29)
    Suggestions {
        #[command(subcommand)]
        action: SuggestionsAction,
    },
    /// List playbooks and versions
    List,
    /// Validate playbooks, profile models, requires and connectors
    Validate { name: Option<String> },
    /// Diagnose environment (agents, executors, profiles, runners, playbooks),
    /// or one run's health with --run
    Doctor {
        /// Diagnose this run instead of the environment: folded statuses, open
        /// attempts and their pid liveness, the driver and workdir-lock
        /// holders, unapplied control entries, repeated supervisor actions.
        /// Read-only, like the environment doctor: it repairs nothing.
        #[arg(long, value_name = "ID")]
        run: Option<String>,
    },
    /// Export a playbook (with layout) to a single bundle file
    Export {
        name: String,
        #[arg(long)]
        version: Option<String>,
        /// Output file; stdout if omitted
        #[arg(long)]
        out: Option<PathBuf>,
    },
    /// Import a playbook bundle file into this project
    Import {
        file: PathBuf,
        /// Do not set the imported version as current
        #[arg(long)]
        no_current: bool,
    },
    /// List deleted playbooks or restore one with all its versions
    Trash {
        #[command(subcommand)]
        action: TrashAction,
    },
    /// List the approvals in the trust store, or revoke them
    Trust {
        #[command(subcommand)]
        action: TrustAction,
    },
    /// Run a playbook
    Run {
        name: String,
        #[arg(long)]
        version: Option<String>,
        #[arg(long)]
        instruction: Option<String>,
        /// key=value, repeatable
        #[arg(long = "param", value_name = "K=V")]
        params: Vec<String>,
        #[arg(long)]
        allow_shared_workdir: bool,
        /// Run in the background under supervision: the engine spawns a
        /// background supervisor agent and watches its heartbeat
        #[arg(long)]
        supervise: bool,
        /// Start the run in a detached background process, print its id and
        /// return at once; follow it with `apb wait <run_id>`
        #[arg(long, conflicts_with = "supervise")]
        detach: bool,
        /// Run-level overrides YAML file (spec 11): swap models/executors
        /// without creating a new version
        #[arg(long)]
        overrides: Option<PathBuf>,
        /// Disable the node result cache for this run: no lookup and no
        /// admission anywhere in the run, regardless of what individual
        /// nodes declare
        #[arg(long, conflicts_with = "refresh_cache")]
        no_cache: bool,
        /// Skip cache lookup (never a hit) but still write fresh results, so
        /// a fresh execution overwrites any stale cached result
        #[arg(long)]
        refresh_cache: bool,
        /// Run id to continue as a fresh top-level retry (issue #42 finding 10).
        /// Links the new run to the predecessor in `apb runs`.
        #[arg(long = "continued-from", value_name = "RUN_ID")]
        continued_from: Option<String>,
        /// The run's working tree: agent and script nodes run in this
        /// directory (absolute, or relative to the project) and the run takes
        /// its busy lock instead of the project's, so runs over different git
        /// worktrees do not wait on each other. Overrides the playbook's
        /// `worktree`
        #[arg(long, value_name = "DIR")]
        worktree: Option<String>,
        /// Who executes the agent steps: cli (the default, the profiles'
        /// agent CLIs) or host (no CLI is spawned; every agent step becomes
        /// a host task that `apb tasks` lists and `apb tasks submit`
        /// answers, meant for an MCP host session)
        #[arg(long, value_name = "MODE")]
        execution: Option<String>,
    },
    /// Host tasks of host-execution-mode runs: list what waits for a host
    /// (all runs, or one), or submit a reply
    #[command(args_conflicts_with_subcommands = true)]
    Tasks {
        #[command(subcommand)]
        action: Option<TasksAction>,
        /// Only this run
        run_id: Option<String>,
        /// Print each task's full prompt and role prompt
        #[arg(long)]
        full: bool,
        /// Machine-readable output (the same objects as MCP pending_tasks)
        #[arg(long)]
        json: bool,
    },
    /// List runs, or show one run (its nodes and the token usage its agents
    /// reported)
    Runs {
        /// Show only this run
        run_id: Option<String>,
    },
    /// Resume a paused/interrupted run
    Resume {
        run_id: String,
        #[arg(long)]
        from_node: Option<String>,
        /// Continue even though an agent binary changed on disk since the run
        /// started (environment drift). The run manifest fingerprints every
        /// agent executable at start; a different fingerprint at resume means
        /// the agent that would drive a node is no longer the one recorded, so
        /// resume refuses by default. Set this to override and proceed anyway
        /// (the accepted drift is recorded as an event in the run log).
        #[arg(long = "allow-environment-drift")]
        allow_environment_drift: bool,
    },
    /// Block until a run finishes, needs input (a question, a review, a
    /// supervisor decision) or stops, then print why. A single call that
    /// costs an agent nothing while it blocks, unlike polling `apb runs`.
    /// Exit codes: 0 succeeded, 1 failed or aborted, 3 needs input,
    /// 4 paused or driverless, 5 timeout, 2 error
    Wait {
        run_id: String,
        /// Give up after this many seconds (default: no limit)
        #[arg(long, value_name = "SECS")]
        timeout: Option<u64>,
    },
    /// Stop a run: interrupt whatever node it is executing right now, and
    /// finalize it outright if the process driving it is gone
    Stop { run_id: String },
    /// Post a supervisor note (ContextAppend) to a run's control channel
    Note { run_id: String, text: String },
    /// Decide a human_review node of a running run
    Review {
        run_id: String,
        node_id: String,
        #[arg(long)]
        decision: String,
        #[arg(long, default_value = "")]
        note: String,
    },
    /// Answer an interactive node's pending question in a running run
    Answer {
        run: String,
        /// The interactive node; omit when exactly one question is pending
        #[arg(long)]
        node: Option<String>,
        text: String,
    },
    /// Measure decision-model uses (issue #165): the report, the stored
    /// thresholds, replay against another provider
    Decisions {
        #[command(subcommand)]
        action: DecisionsAction,
    },
    /// Inspect and manage the project-local node result cache
    Cache {
        #[command(subcommand)]
        cmd: CacheCmd,
    },
    /// Start the web dashboard (global, all projects)
    #[command(alias = "serve")]
    Dashboard {
        /// Port: the flag overrides the global config, default 7321.
        #[arg(long)]
        port: Option<u16>,
        /// IP address to bind: the flag overrides `server.bind` in the global
        /// config, default 127.0.0.1. Any non-loopback address requires at
        /// least one key from `apb server key issue`.
        #[arg(long)]
        bind: Option<String>,
        #[arg(long)]
        no_open: bool,
    },
    /// Start only the inbound webhook listener (headless deployments). The
    /// dashboard co-starts it by itself when `ingest.enabled` is true.
    Ingest {
        /// IP address to bind: the flag overrides `ingest.bind` in the global
        /// config, default 127.0.0.1 (behind a reverse proxy on the same host).
        #[arg(long)]
        bind: Option<String>,
        /// Port: the flag overrides `ingest.port`, default 7322.
        #[arg(long)]
        port: Option<u16>,
    },
    /// Manage server mode: the API keys that authenticate a networked dashboard
    Server {
        #[command(subcommand)]
        action: ServerAction,
    },
    /// Dev mode: Vite HMR frontend + API server (source tree only)
    Dev {
        #[arg(long)]
        no_open: bool,
    },
    /// Start stdio MCP server for the current project
    Mcp,
    /// Update apb to the latest released version
    SelfUpdate {
        /// Report whether an update is available without installing it
        #[arg(long)]
        check: bool,
    },
    /// List or manage the workspace registry (spec 6)
    Projects {
        #[command(subcommand)]
        action: Option<ProjectsAction>,
    },
    /// Internal: actually drives a supervised background run to completion.
    /// Spawned as a detached child process by `run --supervise` (see
    /// `spawn_detached_supervised`) so the run survives after the invoking
    /// CLI process exits - std::thread cannot outlive its process, so the
    /// real drive loop has to happen in a separate one. Not part of the
    /// public CLI surface.
    #[command(hide = true, name = "__drive-supervised")]
    DriveSupervised {
        name: String,
        #[arg(long)]
        version: Option<String>,
        #[arg(long)]
        instruction: Option<String>,
        #[arg(long = "param", value_name = "K=V")]
        params: Vec<String>,
        #[arg(long)]
        allow_shared_workdir: bool,
        /// Predecessor run id for lineage (issue #42 finding 10). Forwarded
        /// from `apb run --supervise --continued-from` across the detached
        /// spawn boundary.
        #[arg(long = "continued-from", value_name = "RUN_ID")]
        continued_from: Option<String>,
        /// The run's working tree, forwarded from `apb run --worktree`.
        #[arg(long, value_name = "DIR")]
        worktree: Option<String>,
        /// Handshake file: written with the run_id as soon as the run is
        /// prepared (before drive starts), so the parent process can report
        /// it and exit without waiting for the run itself to finish.
        #[arg(long)]
        handshake: PathBuf,
    },
    /// Drives an already-prepared run at `<root>/.apb/runs/<run-id>` to
    /// completion in THIS process. Spawned detached by
    /// `apb_engine::driver::spawn_detached_driver`, so that a run started from
    /// a chat session (MCP) survives that session dying. Hidden: an internal
    /// re-exec target, not a user-facing command.
    #[command(hide = true, name = "__drive-run")]
    DriveRun {
        /// Project root holding `.apb/runs` (absolute: the parent resolves it).
        #[arg(long)]
        root: PathBuf,
        #[arg(long = "run-id")]
        run_id: String,
        /// Passed through to the resume planner; only meaningful with `--resume`.
        #[arg(long = "from-node")]
        from_node: Option<String>,
        /// Resume an existing run instead of driving a freshly prepared one.
        #[arg(long)]
        resume: bool,
        /// Forwarded from `apb resume --allow-environment-drift` across the
        /// detached spawn boundary: lets the resumed child write its accepted
        /// drift events instead of refusing.
        #[arg(long = "allow-environment-drift")]
        allow_environment_drift: bool,
    },
    /// The live-question sidecar (spec 2026-07-20-interactive-nodes, Task 10):
    /// a stdio MCP server exposing one `ask_user` tool, injected into the
    /// coding agent that runs a live interactive `agent_task` node. Resolves
    /// the run directory from `APB_RUN_DIR` (inherited from the agent). Hidden:
    /// an internal injection target, not a user-facing command.
    #[command(hide = true, name = "__ask-server")]
    AskServer {
        #[arg(long)]
        run: String,
        #[arg(long)]
        node: String,
        #[arg(long)]
        attempt: u32,
    },
}

/// Whether this invocation auto-registers its cwd in the project registry.
/// The hidden re-exec targets (`__drive-supervised`, `__drive-run`,
/// `__ask-server`) are spawned by an apb process that already registered the
/// project, so they must not: `__drive-run` works on its `--root`, not its cwd,
/// and `__ask-server` inherits the coding agent's cwd, which can be any
/// directory.
fn registers_workspace(command: Option<&Command>) -> bool {
    !matches!(
        command,
        Some(
            Command::DriveSupervised { .. } | Command::DriveRun { .. } | Command::AskServer { .. }
        )
    )
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    let root = std::env::current_dir().expect("cwd");
    // Auto-register the workspace in the project registry (spec 6.2). Only for
    // existing projects, so we don't clutter the registry with every directory
    // where `playbook` was run. Best-effort: does not fail or slow down the
    // command. Done at the process entry point rather than in WfMcp::new, so
    // that constructing the server in tests does not write to the real
    // ~/.config/playbook.
    if registers_workspace(cli.command.as_ref()) && root.join(".apb").is_dir() {
        apb_core::projects::touch(&root);
    }
    // The key that stamps the runs this installation creates (see
    // `apb_core::run_origin`), so an MCP resume can refuse a run directory
    // that came with a repository. Created on first use; best effort, like the
    // registration above: without it runs are simply unstamped.
    if registers_workspace(cli.command.as_ref()) {
        let _ = apb_core::run_origin::ensure_key();
    }
    match cli.command {
        Some(Command::Init) => run_init(&root),
        Some(Command::List) => run_list(&root),
        Some(Command::Validate { name }) => run_validate(&root, name),
        Some(Command::Doctor { run }) => run_doctor(&root, run.as_deref()),
        Some(Command::Export { name, version, out }) => {
            export_cmd(&root, &name, version.as_deref(), out.as_deref())
        }
        Some(Command::Import { file, no_current }) => import_cmd(&root, &file, !no_current),
        Some(Command::Trash { action }) => trash_cmd(&root, action),
        Some(Command::Trust { action }) => trust_cmd(action),
        Some(Command::Run {
            name,
            version,
            instruction,
            params,
            allow_shared_workdir,
            supervise,
            detach,
            overrides,
            no_cache,
            refresh_cache,
            continued_from,
            worktree,
            execution,
        }) => run_cmd(
            &root,
            &name,
            version.as_deref(),
            instruction,
            params,
            allow_shared_workdir,
            supervise,
            detach,
            overrides.as_deref(),
            no_cache,
            refresh_cache,
            continued_from,
            worktree,
            execution.as_deref(),
        ),
        Some(Command::Tasks {
            action,
            run_id,
            full,
            json,
        }) => tasks_cmd(&root, action, run_id, full, json),
        Some(Command::Runs { run_id }) => runs_cmd(&root, run_id.as_deref()),
        Some(Command::Resume {
            run_id,
            from_node,
            allow_environment_drift,
        }) => resume_cmd(
            &root,
            &run_id,
            from_node.as_deref(),
            allow_environment_drift,
        ),
        Some(Command::Stop { run_id }) => stop_cmd(&root, &run_id),
        Some(Command::Wait { run_id, timeout }) => wait_cmd(&root, &run_id, timeout),
        Some(Command::Note { run_id, text }) => note_cmd(&root, &run_id, &text),
        Some(Command::Review {
            run_id,
            node_id,
            decision,
            note,
        }) => review_cmd(&root, &run_id, &node_id, &decision, &note),
        Some(Command::Answer { run, node, text }) => {
            answer_cmd(&root, &run, node.as_deref(), &text)
        }
        Some(Command::Dashboard {
            port,
            bind,
            no_open,
        }) => match resolve_bind(bind.as_deref()) {
            Ok(addr) => dashboard(addr, resolve_port(port), no_open),
            Err(e) => {
                eprintln!("dashboard failed: {e}");
                ExitCode::from(2)
            }
        },
        Some(Command::Ingest { bind, port }) => ingest_cmd(bind.as_deref(), port),
        Some(Command::Server { action }) => server_cmd(action),
        Some(Command::Dev { no_open }) => dev_cmd(root, no_open),
        Some(Command::Mcp) => mcp_cmd(&root),
        Some(Command::SelfUpdate { check }) => run_self_update(check),
        Some(Command::Projects { action }) => projects_cmd(action),
        Some(Command::Profile { action }) => profile_cmd(&root, action),
        Some(Command::Connector { action }) => connector_cmd(&root, action),
        Some(Command::Cache { cmd }) => cache_cmd(&root, cmd),
        Some(Command::Decisions { action }) => decisions_cmd(&root, action),
        Some(Command::Migrate { apply }) => migrate_cmd(&root, apply),
        Some(Command::Detect { refresh }) => detect_cmd(refresh),
        Some(Command::Adopt { name }) => adopt_cmd(&root, name.as_deref()),
        Some(Command::Subscriptions { decline, set }) => subscriptions_cmd(set, decline),
        Some(Command::Suggestions { action }) => suggestions_cmd(&root, action),
        Some(Command::DriveSupervised {
            name,
            version,
            instruction,
            params,
            allow_shared_workdir,
            continued_from,
            worktree,
            handshake,
        }) => drive_supervised_child(
            &root,
            &name,
            version.as_deref(),
            instruction,
            params,
            allow_shared_workdir,
            continued_from,
            worktree,
            &handshake,
        ),
        // Deliberately uses the `--root` it was given, not the process cwd:
        // the spawning parent knows which project the run belongs to.
        Some(Command::DriveRun {
            root: run_root,
            run_id,
            from_node,
            resume,
            allow_environment_drift,
        }) => drive_run_child(
            &run_root,
            &run_id,
            from_node.as_deref(),
            resume,
            allow_environment_drift,
        ),
        Some(Command::AskServer { run, node, attempt }) => ask_server_cmd(&run, &node, attempt),
        None => match resolve_bind(None) {
            Ok(addr) => dashboard(addr, resolve_port(None), false),
            Err(e) => {
                eprintln!("dashboard failed: {e}");
                ExitCode::from(2)
            }
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn registers(args: &[&str]) -> bool {
        let cli = Cli::try_parse_from(args).expect("args parse");
        registers_workspace(cli.command.as_ref())
    }

    /// F18: llms.txt is what an agent reads to learn the CLI; its command
    /// list must name every public subcommand clap registers.
    #[test]
    fn llms_txt_lists_every_public_command() {
        use clap::CommandFactory;
        let llms = include_str!("../../../llms.txt");
        let section = llms
            .split("## CLI commands")
            .nth(1)
            .and_then(|rest| rest.split("\n## ").next())
            .expect("llms.txt has a `## CLI commands` section");
        let missing: Vec<String> = Cli::command()
            .get_subcommands()
            .filter(|c| !c.is_hide_set())
            .map(|c| c.get_name().to_string())
            .filter(|name| {
                !section
                    .lines()
                    .any(|l| l.starts_with(&format!("apb {name} ")) || l == format!("apb {name}"))
            })
            .collect();
        assert!(
            missing.is_empty(),
            "commands missing from llms.txt: {missing:?}"
        );
    }

    #[test]
    fn user_facing_commands_register_the_workspace() {
        assert!(registers(&["apb"]));
        assert!(registers(&["apb", "list"]));
        assert!(registers(&["apb", "mcp"]));
    }

    #[test]
    fn internal_reexec_targets_do_not_register_the_workspace() {
        assert!(!registers(&[
            "apb",
            "__drive-run",
            "--root",
            "/r",
            "--run-id",
            "x"
        ]));
        assert!(!registers(&[
            "apb",
            "__drive-supervised",
            "pb",
            "--handshake",
            "/h"
        ]));
        assert!(!registers(&[
            "apb",
            "__ask-server",
            "--run",
            "r",
            "--node",
            "n",
            "--attempt",
            "1"
        ]));
    }
}
