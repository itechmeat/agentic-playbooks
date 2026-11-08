//! `apb corrections`: a playbook's corrections ledger (the review notes,
//! node notes, question answers and rejections that recurred across its
//! runs), the draft a `proposed` entry offers for a node's prompt, and a
//! person's decision on one entry: mark a version of the store as carrying
//! the fix (`fixed`), dismiss, reopen. A thin dispatch over
//! `apb_mcp::tools::corrections`, the layer MCP and the dashboard use;
//! every subcommand, the list included, is refused inside a run step there.
//! Marking a fix is a person's statement about a version, so it is offered
//! here and on the dashboard only, never over MCP.

use std::path::Path;
use std::process::ExitCode;

use apb_core::corrections::{Correction, CorrectionStatus};
use apb_mcp::tools::ToolError;
use apb_mcp::tools::corrections::{
    CorrectionView, CorrectionsView, Decision, correction_decide, corrections_view,
};
use clap::{Args, Subcommand};

use crate::util::{QUESTION_TEXT_MAX, StepCaller, print_json, sanitize_for_terminal};

/// Who decided, as the store keeps it.
const DECIDED_BY: &str = "cli";

#[derive(Args)]
pub(crate) struct CorrectionsArgs {
    /// The playbook
    playbook: String,
    #[command(subcommand)]
    action: Option<CorrectionsAction>,
    /// `project` or `global`; defaults to where the playbook lives
    #[arg(long, global = true)]
    scope: Option<String>,
    /// Machine-readable output (the same objects as MCP)
    #[arg(long, global = true)]
    json: bool,
}

#[derive(Subcommand)]
enum CorrectionsAction {
    /// The draft a proposed entry offers, with the node its line belongs in
    Show {
        /// The correction's id (`c-...`, see the list)
        entry: String,
    },
    /// Mark a version as carrying the fix of a proposed entry
    Fixed {
        /// The correction's id (`c-...`, see the list)
        entry: String,
        /// The version whose definition carries the fix
        #[arg(long, value_name = "VERSION")]
        version: String,
        /// Why, kept with the decision
        #[arg(long)]
        reason: Option<String>,
    },
    /// Never show it again
    Dismiss {
        /// The correction's id (`c-...`, see the list)
        entry: String,
        /// Why, kept with the decision
        #[arg(long)]
        reason: Option<String>,
    },
    /// Open a dismissed or fixed entry again
    Reopen {
        /// The correction's id (`c-...`, see the list)
        entry: String,
        /// Why, kept with the decision
        #[arg(long)]
        reason: Option<String>,
    },
}

pub(crate) fn corrections_cmd(root: &Path, args: CorrectionsArgs) -> ExitCode {
    let target = match apb_mcp::tools::problems::Target::resolve(
        root,
        &args.playbook,
        args.scope.as_deref(),
    ) {
        Ok(t) => t,
        Err(e) => return failed("corrections", e),
    };
    let step = StepCaller::from_env();
    match args.action {
        None => list(root, &target, args.json, &step),
        Some(CorrectionsAction::Show { entry }) => show(root, &target, &entry, args.json, &step),
        Some(CorrectionsAction::Fixed {
            entry,
            version,
            reason,
        }) => decide(
            root,
            &target,
            &entry,
            Decision::MarkFixed { version },
            reason.as_deref(),
            args.json,
            &step,
        ),
        Some(CorrectionsAction::Dismiss { entry, reason }) => decide(
            root,
            &target,
            &entry,
            Decision::Dismiss,
            reason.as_deref(),
            args.json,
            &step,
        ),
        Some(CorrectionsAction::Reopen { entry, reason }) => decide(
            root,
            &target,
            &entry,
            Decision::Reopen,
            reason.as_deref(),
            args.json,
            &step,
        ),
    }
}

/// A failed command: exit 2 for a refusal (a conflict, something missing,
/// a channel that may not take it), 1 for anything else.
fn failed(what: &str, e: ToolError) -> ExitCode {
    let code = match &e {
        ToolError::Conflict(_) | ToolError::NotFound(_) | ToolError::Refused { .. } => 2,
        ToolError::Engine(_) => 1,
    };
    eprintln!(
        "{what} failed: {}",
        sanitize_for_terminal(&e.to_string(), 600)
    );
    ExitCode::from(code)
}

fn text(s: &str) -> String {
    sanitize_for_terminal(s, QUESTION_TEXT_MAX)
}

fn status_word(s: CorrectionStatus) -> &'static str {
    match s {
        CorrectionStatus::Open => "open",
        CorrectionStatus::Proposed => "proposed",
        CorrectionStatus::Fixed => "fixed",
        CorrectionStatus::Reopened => "reopened",
        CorrectionStatus::Dismissed => "dismissed",
    }
}

/// One entry as the list shows it:
/// `c-1a2b3c4d5e6f proposed review_note at node implement (gate review):
/// 3 runs, last <run>: "<text>"`. The gate part is omitted without a gate,
/// and the text of a sighting that held a secret is never shown.
fn entry_line(e: &Correction) -> String {
    let mut line = format!("{} {} {}", e.id, status_word(e.status), e.kind.as_word());
    if let Some(node) = &e.node {
        line.push_str(&format!(" at node {node}"));
    }
    if let Some(gate) = &e.gate {
        line.push_str(&format!(" (gate {gate})"));
    }
    let last = e
        .evidence
        .last()
        .map(|r| r.run_id.as_str())
        .unwrap_or("unknown");
    line.push_str(&format!(
        ": {} run{}, last {last}: ",
        e.runs,
        if e.runs == 1 { "" } else { "s" }
    ));
    match &e.text {
        Some(t) => line.push_str(&format!("\"{}\"", text(t))),
        None => line.push_str("(text withheld)"),
    }
    line
}

fn list(
    root: &Path,
    t: &apb_mcp::tools::problems::Target,
    json: bool,
    step: &StepCaller,
) -> ExitCode {
    let view: CorrectionsView = match corrections_view(root, t, step) {
        Ok(v) => v,
        Err(e) => return failed("corrections", e),
    };
    if json {
        print_json(&serde_json::json!(view));
        return ExitCode::SUCCESS;
    }
    if !view.signed {
        println!(
            "{} ({}): the corrections file was not written by this apb installation; its {} entries are neither shown nor listed",
            view.playbook, view.scope, view.unsigned_entries
        );
    }
    if view.entries.is_empty() {
        println!("{} ({}): no corrections", view.playbook, view.scope);
        return ExitCode::SUCCESS;
    }
    println!(
        "{} ({}): {} corrections",
        view.playbook,
        view.scope,
        view.entries.len()
    );
    for e in &view.entries {
        println!("{}", entry_line(&e.correction));
    }
    ExitCode::SUCCESS
}

/// One entry of the playbook's ledger, through the same read the list takes.
fn one_entry(
    root: &Path,
    t: &apb_mcp::tools::problems::Target,
    entry: &str,
    step: &StepCaller,
) -> Result<CorrectionView, ToolError> {
    let view = corrections_view(root, t, step)?;
    view.entries
        .into_iter()
        .find(|e| e.correction.id == entry)
        .ok_or_else(|| ToolError::NotFound(format!("correction `{entry}`")))
}

fn show(
    root: &Path,
    t: &apb_mcp::tools::problems::Target,
    entry: &str,
    json: bool,
    step: &StepCaller,
) -> ExitCode {
    match one_entry(root, t, entry, step) {
        Ok(e) if json => {
            print_json(&serde_json::json!(e));
            ExitCode::SUCCESS
        }
        Ok(e) => {
            match e.draft {
                Some(d) => {
                    println!("draft: {}", text(&d.line));
                    println!("node: {}", d.node);
                }
                None => {
                    println!("no draft (the node is unknown, ambiguous, or the text was withheld)")
                }
            }
            ExitCode::SUCCESS
        }
        Err(e) => failed("corrections show", e),
    }
}

fn decide(
    root: &Path,
    t: &apb_mcp::tools::problems::Target,
    entry: &str,
    decision: Decision,
    reason: Option<&str>,
    json: bool,
    step: &StepCaller,
) -> ExitCode {
    let what = match &decision {
        Decision::MarkFixed { .. } => "corrections fixed",
        Decision::Dismiss => "corrections dismiss",
        Decision::Reopen => "corrections reopen",
    };
    match correction_decide(root, t, entry, decision, DECIDED_BY, reason, step) {
        Ok(e) if json => {
            print_json(&serde_json::json!(e));
            ExitCode::SUCCESS
        }
        Ok(e) => {
            let c = &e.correction;
            match &c.fix {
                Some(fix) if c.status == CorrectionStatus::Fixed => {
                    println!("{}: fixed in {}", c.id, fix.version);
                }
                _ => println!("{}: {}", c.id, status_word(c.status)),
            }
            ExitCode::SUCCESS
        }
        Err(e) => failed(what, e),
    }
}
