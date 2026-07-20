mod cache;
mod connector;
mod manage;
mod profile;
mod run;
mod serve;
mod util;

use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Parser, Subcommand};

use crate::cache::{CacheCmd, cache_cmd};
use crate::connector::{ConnectorAction, connector_cmd};
use crate::manage::{
    ProjectsAction, adopt_cmd, detect_cmd, export_cmd, import_cmd, migrate_cmd, projects_cmd,
    run_init, subscriptions_cmd,
};
use crate::profile::{ProfileAction, profile_cmd};
use crate::run::{
    drive_run_child, drive_supervised_child, note_cmd, resume_cmd, review_cmd, run_cmd, run_doctor,
    run_list, run_validate, runs_cmd, stop_cmd,
};
use crate::serve::{dev_cmd, mcp_cmd, serve};
use crate::util::resolve_port;

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
    /// List playbooks and versions
    List,
    /// Validate playbook schema
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
    },
    /// List runs
    Runs,
    /// Resume a paused/interrupted run
    Resume {
        run_id: String,
        #[arg(long)]
        from_node: Option<String>,
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
    /// Inspect and manage the project-local node result cache
    Cache {
        #[command(subcommand)]
        cmd: CacheCmd,
    },
    /// Start web server (see Task 8/13)
    Serve {
        /// Port: the flag overrides the global config, default 7321.
        #[arg(long)]
        port: Option<u16>,
        #[arg(long)]
        no_open: bool,
    },
    /// Dev mode: Vite HMR frontend + API server (source tree only)
    Dev {
        #[arg(long)]
        no_open: bool,
    },
    /// Start stdio MCP server for the current project
    Mcp,
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
    },
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
    if root.join(".apb").is_dir() {
        apb_core::projects::touch(&root);
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
        Some(Command::Run {
            name,
            version,
            instruction,
            params,
            allow_shared_workdir,
            supervise,
            overrides,
            no_cache,
            refresh_cache,
        }) => run_cmd(
            &root,
            &name,
            version.as_deref(),
            instruction,
            params,
            allow_shared_workdir,
            supervise,
            overrides.as_deref(),
            no_cache,
            refresh_cache,
        ),
        Some(Command::Runs) => runs_cmd(&root),
        Some(Command::Resume { run_id, from_node }) => {
            resume_cmd(&root, &run_id, from_node.as_deref())
        }
        Some(Command::Stop { run_id }) => stop_cmd(&root, &run_id),
        Some(Command::Note { run_id, text }) => note_cmd(&root, &run_id, &text),
        Some(Command::Review {
            run_id,
            node_id,
            decision,
            note,
        }) => review_cmd(&root, &run_id, &node_id, &decision, &note),
        Some(Command::Serve { port, no_open }) => serve(resolve_port(port), no_open),
        Some(Command::Dev { no_open }) => dev_cmd(root, no_open),
        Some(Command::Mcp) => mcp_cmd(&root),
        Some(Command::Projects { action }) => projects_cmd(action),
        Some(Command::Profile { action }) => profile_cmd(&root, action),
        Some(Command::Connector { action }) => connector_cmd(&root, action),
        Some(Command::Cache { cmd }) => cache_cmd(&root, cmd),
        Some(Command::Migrate { apply }) => migrate_cmd(&root, apply),
        Some(Command::Detect { refresh }) => detect_cmd(refresh),
        Some(Command::Adopt { name }) => adopt_cmd(&root, name.as_deref()),
        Some(Command::Subscriptions { decline, set }) => subscriptions_cmd(set, decline),
        Some(Command::DriveSupervised {
            name,
            version,
            instruction,
            params,
            allow_shared_workdir,
            handshake,
        }) => drive_supervised_child(
            &root,
            &name,
            version.as_deref(),
            instruction,
            params,
            allow_shared_workdir,
            &handshake,
        ),
        // Deliberately uses the `--root` it was given, not the process cwd:
        // the spawning parent knows which project the run belongs to.
        Some(Command::DriveRun {
            root: run_root,
            run_id,
            from_node,
            resume,
        }) => drive_run_child(&run_root, &run_id, from_node.as_deref(), resume),
        None => serve(resolve_port(None), false),
    }
}
