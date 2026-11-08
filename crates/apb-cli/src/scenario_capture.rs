//! `apb scenario capture`: the preview of a finished run as a scenario,
//! and the person's save of it. The logic is the shared tool layer
//! ([`apb_mcp::tools::scenario_capture`]); this module adds the terminal
//! question and the exit codes (0 saved or previewed, 2 refused, 1 an
//! error).

use std::io::IsTerminal as _;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use apb_core::launch::floor::SaveChannel;
use apb_core::step_env::StepCaller;
use apb_mcp::tools::scenario_capture::{CaptureArgs, capture, playbook_dir_for};
use clap::Subcommand;

/// `apb scenario <action>`.
#[derive(Subcommand)]
pub(crate) enum ScenarioAction {
    /// Show a finished run as a scenario (a preview first; the save is a
    /// person's act at the terminal, or --yes)
    Capture {
        /// The finished run's id
        run_id: String,
        /// The scenario's name: [a-z0-9][a-z0-9-]*, at most 64 characters
        #[arg(long = "name", value_name = "SLUG")]
        name: String,
        /// Only assert these nodes' statuses and visits (repeatable;
        /// default: every node that ran)
        #[arg(long = "node", value_name = "ID")]
        nodes: Vec<String>,
        /// Write the scenario file without the terminal question
        #[arg(long)]
        save: bool,
        /// Confirm the save without a terminal
        #[arg(long)]
        yes: bool,
        /// Machine-readable output
        #[arg(long)]
        json: bool,
    },
}

/// The one dispatch of `apb scenario`.
pub(crate) fn scenario_action(root: &Path, action: ScenarioAction) -> ExitCode {
    match action {
        ScenarioAction::Capture {
            run_id,
            name,
            nodes,
            save,
            yes,
            json,
        } => capture_cmd(
            root,
            &CaptureCommand {
                run_id,
                name,
                nodes,
                save,
                yes,
                json,
            },
        ),
    }
}

/// `apb scenario capture <run-id> --name <slug> [--node <id> ...] [--save]
/// [--yes] [--json]`.
pub(crate) struct CaptureCommand {
    pub run_id: String,
    pub name: String,
    pub nodes: Vec<String>,
    pub save: bool,
    pub yes: bool,
    pub json: bool,
}

pub(crate) fn capture_cmd(root: &Path, args: &CaptureCommand) -> ExitCode {
    let channel = channel_of(args);
    let preview = match capture(
        root,
        &args.run_id,
        &CaptureArgs {
            name: args.name.clone(),
            nodes: args.nodes.clone(),
            save: false,
            digest: None,
        },
        channel,
        &StepCaller::from_env(),
    ) {
        Ok(p) => p,
        Err(e) => return refused(e),
    };
    if args.json {
        match serde_json::to_string_pretty(&preview) {
            Ok(s) => println!("{s}"),
            Err(e) => {
                eprintln!("scenario capture: the preview could not be serialized: {e}");
                return ExitCode::from(1);
            }
        }
    } else {
        println!("{}", preview.yaml);
        for w in &preview.warnings {
            println!("warning: {w}");
        }
        if preview.redacted > 0 {
            println!(
                "{} text(s) were redacted; routing that read a redacted value may differ on the replay",
                preview.redacted
            );
        }
        println!("digest: {}", preview.digest);
    }
    // A save without --save goes through the terminal question.
    let save = if args.save {
        true
    } else if !std::io::stdin().is_terminal() {
        false
    } else {
        let path = save_path(root, &preview);
        print!("Save as {}? [y/N] ", path.display());
        use std::io::Write as _;
        let _ = std::io::stdout().flush();
        let mut line = String::new();
        let _ = std::io::stdin().read_line(&mut line);
        let answer = line.trim().to_ascii_lowercase();
        answer == "y" || answer == "yes"
    };
    if !save {
        return ExitCode::SUCCESS;
    }
    // A save without a terminal needs --yes: a save is a person's act.
    if !args.yes && !std::io::stdin().is_terminal() {
        eprintln!("scenario capture: saving needs a terminal question or --yes; previewed only");
        return ExitCode::from(2);
    }
    let saved = capture(
        root,
        &args.run_id,
        &CaptureArgs {
            name: args.name.clone(),
            nodes: args.nodes.clone(),
            save: true,
            digest: Some(preview.digest.clone()),
        },
        channel,
        &StepCaller::from_env(),
    );
    match saved {
        Ok(p) => {
            if let Some(path) = &p.saved {
                println!("saved {}", path);
            }
            ExitCode::SUCCESS
        }
        Err(e) => refused(e),
    }
}

/// The channel the CLI acts through: a person at a terminal or with
/// --yes; the preview alone needs neither, but a non-terminal CLI is not
/// a person, so its save is refused above.
fn channel_of(args: &CaptureCommand) -> SaveChannel {
    if args.yes || std::io::stdin().is_terminal() {
        SaveChannel::Person
    } else {
        SaveChannel::Agent
    }
}

/// Where a save of this preview would land, for the question's wording.
fn save_path(root: &Path, preview: &apb_mcp::tools::scenario_capture::CapturePreview) -> PathBuf {
    let scope = preview
        .scope
        .as_deref()
        .and_then(apb_core::scope::Scope::parse);
    playbook_dir_for(root, &preview.playbook, scope)
        .map(|dir| {
            dir.join("evals/scenarios")
                .join(format!("{}.yaml", preview.name))
        })
        .unwrap_or_else(|_| PathBuf::from(format!("evals/scenarios/{}.yaml", preview.name)))
}

/// A refusal exits 2 with its line; an unexpected error exits 1.
fn refused(e: apb_mcp::tools::ToolError) -> ExitCode {
    match e {
        apb_mcp::tools::ToolError::Engine(_) => {
            eprintln!("scenario capture: {e}");
            ExitCode::from(1)
        }
        _ => {
            eprintln!("scenario capture: {e}");
            ExitCode::from(2)
        }
    }
}
