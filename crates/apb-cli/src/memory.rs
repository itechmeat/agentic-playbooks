//! `apb memory`: the run input a playbook remembers in this project (the
//! param values and instruction a start that passes none uses). A thin
//! dispatch over `apb_mcp::tools::memory`, the same layer MCP
//! `playbook_memory` and the dashboard use.

use std::path::Path;
use std::process::ExitCode;

use apb_core::run_memory::{InstructionSource, Recall, ValueStatus};
use apb_mcp::tools::memory::{MemoryAction, MemoryTarget, parse_what, playbook_memory};
use clap::Subcommand;

use crate::util::{print_json, print_table, sanitize_for_terminal};

/// Longest remembered value or instruction printed on one line.
const VALUE_MAX: usize = 120;

#[derive(Subcommand)]
pub(crate) enum MemoryCmd {
    /// Show what a playbook remembers in this project, checked against its
    /// params now: each value and whether it can be used, the params a
    /// start still has to pass, and where the instruction comes from
    Show {
        id: String,
        /// Definition scope: project (default) or global
        #[arg(long, default_value = "project")]
        scope: String,
        /// Machine-readable output (the same object as MCP playbook_memory get)
        #[arg(long)]
        json: bool,
    },
    /// Forget what a playbook remembers in this project
    Clear {
        id: String,
        /// Definition scope: project (default) or global
        #[arg(long, default_value = "project")]
        scope: String,
        /// What to forget: all (default), params or instruction
        #[arg(long, default_value = "all")]
        what: String,
    },
}

pub(crate) fn memory_cmd(root: &Path, cmd: MemoryCmd) -> ExitCode {
    match cmd {
        MemoryCmd::Show { id, scope, json } => show(root, &id, &scope, json),
        MemoryCmd::Clear { id, scope, what } => {
            let Some(what) = parse_what(Some(&what)) else {
                eprintln!("unknown --what `{what}`; use all, params or instruction");
                return ExitCode::from(2);
            };
            let target = MemoryTarget {
                id: &id,
                scope: &scope,
                version: None,
            };
            match playbook_memory(root, &target, MemoryAction::Clear(what)) {
                Ok(v) => {
                    println!(
                        "forgot {} for {id} ({scope})",
                        v["cleared"].as_str().unwrap_or("all")
                    );
                    ExitCode::SUCCESS
                }
                Err(e) => {
                    eprintln!("memory clear failed: {e}");
                    ExitCode::from(2)
                }
            }
        }
    }
}

fn show(root: &Path, id: &str, scope: &str, as_json: bool) -> ExitCode {
    let target = MemoryTarget {
        id,
        scope,
        version: None,
    };
    let v = match playbook_memory(root, &target, MemoryAction::Get) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("memory show failed: {e}");
            return ExitCode::from(2);
        }
    };
    if as_json {
        print_json(&v);
        return ExitCode::SUCCESS;
    }
    let recall: Recall = match serde_json::from_value(v.clone()) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("memory show failed: {e}");
            return ExitCode::from(2);
        }
    };
    println!(
        "{id} ({scope}) @ {}",
        v["version"].as_str().unwrap_or_default()
    );
    print_recall(&recall);
    ExitCode::SUCCESS
}

fn print_recall(recall: &Recall) {
    if recall.values.is_empty() {
        println!("no remembered param values");
    } else {
        let mut rows = vec![vec![
            "PARAM".to_string(),
            "STATUS".to_string(),
            "VALUE".to_string(),
        ]];
        for value in &recall.values {
            let status = match value.status {
                ValueStatus::Compatible => "compatible".to_string(),
                ValueStatus::Incompatible { reason } => {
                    format!("incompatible ({})", reason.as_str())
                }
            };
            rows.push(vec![
                sanitize_for_terminal(&value.name, 64),
                status,
                value
                    .value
                    .as_deref()
                    .map(|s| sanitize_for_terminal(s, VALUE_MAX))
                    .unwrap_or_else(|| "(hidden)".to_string()),
            ]);
        }
        print_table(&rows);
    }
    if recall.schema_changed {
        println!("the params changed since these values were saved");
    }
    if !recall.missing.is_empty() {
        let names: Vec<String> = recall
            .missing
            .iter()
            .map(|n| sanitize_for_terminal(n, 64))
            .collect();
        println!("a start must pass: {}", names.join(", "));
    }
    match (&recall.instruction, recall.instruction_source) {
        (Some(text), InstructionSource::Memory) => println!(
            "instruction (remembered): {}",
            sanitize_for_terminal(text, VALUE_MAX)
        ),
        (Some(text), InstructionSource::LegacyDraft) => println!(
            "instruction (legacy draft, read only): {}",
            sanitize_for_terminal(text, VALUE_MAX)
        ),
        _ => println!("no remembered instruction"),
    }
    for d in &recall.diagnostics {
        eprintln!("note: {}", sanitize_for_terminal(d, 200));
    }
}
