//! `apb decide` (issue #193): a bounded decision from the configured
//! decision providers, for a host task, a script or a CLI agent step. The
//! engine function is shared with the MCP tool `decision_ask`.
//!
//! Inside a run the engine sets `APB_RUN_ID`, `APB_RUN_DIR` and
//! `APB_NODE_ID` (a host task carries them in its `env`): the decision is
//! then journaled in that run and the run's budget applies. Prints one JSON
//! line; exits 0 when answered, 1 when refused or failed, 2 on a malformed
//! request.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use apb_engine::decision::host_task::{AskKind, AskRequest, ask};
use serde_json::json;

/// The arguments of `apb decide`.
#[derive(clap::Args)]
pub(crate) struct DecideArgs {
    /// choose, rank, filter, map, is or score
    pub kind: String,
    /// The question, in plain words
    pub question: String,
    /// An option (choose, map) or a level, lowest first (score); repeat it
    #[arg(long = "option", value_name = "TEXT")]
    pub options: Vec<String>,
    /// An item (rank, filter, map); repeat it
    #[arg(long = "item", value_name = "TEXT")]
    pub items: Vec<String>,
    /// What a good answer looks like
    #[arg(long)]
    pub criteria: Option<String>,
    /// The run to journal in (default: APB_RUN_ID)
    #[arg(long = "run", value_name = "ID")]
    pub run_id: Option<String>,
    /// The asking node (default: APB_NODE_ID)
    #[arg(long = "node", value_name = "ID")]
    pub node_id: Option<String>,
    /// Ask outside any run even when APB_RUN_ID is set
    #[arg(long, conflicts_with = "run_id")]
    pub no_run: bool,
}

fn env(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|v| !v.trim().is_empty())
}

/// The project root of `run_id` when `APB_RUN_DIR` names it
/// (`<root>/.apb/runs/<id>`): a subagent may work in another directory (a
/// worktree, a node workdir) than the project the run belongs to.
fn root_of_run_dir(run_id: &str) -> Option<PathBuf> {
    let dir = PathBuf::from(env("APB_RUN_DIR")?);
    if dir.file_name()?.to_str()? != run_id {
        return None;
    }
    let runs = dir.parent()?;
    let apb = runs.parent()?;
    (runs.file_name()? == "runs" && apb.file_name()? == ".apb")
        .then(|| apb.parent().map(Path::to_path_buf))
        .flatten()
}

pub(crate) fn decide_cmd(root: &Path, args: DecideArgs) -> ExitCode {
    let Some(kind) = AskKind::parse(&args.kind) else {
        println!(
            "{}",
            json!({ "error": "unknown_kind", "detail": format!("kind must be choose, rank, filter, map, is or score, got `{}`", args.kind) })
        );
        return ExitCode::from(2);
    };
    let from_env = args.run_id.is_none();
    let run_id = if args.no_run {
        None
    } else {
        args.run_id.or_else(|| env("APB_RUN_ID"))
    };
    let node_id = args.node_id.or_else(|| {
        (from_env && run_id.is_some())
            .then(|| env("APB_NODE_ID"))
            .flatten()
    });
    let root = run_id
        .as_deref()
        .and_then(root_of_run_dir)
        .unwrap_or_else(|| root.to_path_buf());
    let req = AskRequest {
        kind,
        question: args.question,
        options: args.options,
        items: args.items,
        criteria: args.criteria,
        run_id,
        node_id,
    };
    match ask(&root, &req) {
        Ok(outcome) => {
            println!("{}", outcome.to_json());
            if outcome.answered() {
                ExitCode::SUCCESS
            } else {
                ExitCode::FAILURE
            }
        }
        Err(apb_engine::EngineError::Invalid(m)) => {
            println!("{}", json!({ "error": "invalid_request", "detail": m }));
            ExitCode::from(2)
        }
        Err(e) => {
            println!("{}", json!({ "error": "engine", "detail": e.to_string() }));
            ExitCode::FAILURE
        }
    }
}
