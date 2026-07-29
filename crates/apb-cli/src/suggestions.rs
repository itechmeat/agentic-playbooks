//! `apb suggestions` subcommands (spec 2026-07-29-suggestion-decisions-design
//! section "CLI"): a thin dispatch over `apb_core::dismiss` so the user can see
//! and undo what the agent recorded. `list` reads both scopes, `allow` removes
//! a record outright (offers resume immediately), `reset` zeroes a soft
//! record's escalation while keeping its synopsis.

use std::path::Path;
use std::process::ExitCode;

use apb_core::dismiss::{self, DecisionScope};
use clap::Subcommand;
use serde_json::json;

use crate::util::{print_json, print_table};

#[derive(Subcommand)]
pub(crate) enum SuggestionsAction {
    /// Show the suggestion decisions that currently silence an offer, from the
    /// project and the global store
    List {
        /// Machine-readable output for scripts
        #[arg(long)]
        json: bool,
    },
    /// Remove a record so the suggestion can be offered again right away
    Allow {
        pattern: String,
        /// Remove the global record instead of the project one
        #[arg(long)]
        global: bool,
    },
    /// Zero a soft record's decline counter and clear its snooze, keeping the
    /// record so its synopsis stays available. Project scope only; a hard
    /// record is removed with `apb suggestions allow`.
    Reset {
        /// Pattern to reset; omit only with --all
        pattern: Option<String>,
        /// Reset every soft record in the project scope
        #[arg(long, conflicts_with = "pattern")]
        all: bool,
    },
}

pub(crate) fn suggestions_cmd(root: &Path, action: SuggestionsAction) -> ExitCode {
    match action {
        SuggestionsAction::List { json } => list_cmd(root, json),
        SuggestionsAction::Allow { pattern, global } => allow_cmd(root, &pattern, global),
        SuggestionsAction::Reset { pattern, all } => reset_cmd(root, pattern.as_deref(), all),
    }
}

fn list_cmd(root: &Path, as_json: bool) -> ExitCode {
    let view = dismiss::active(root);
    if as_json {
        let rows: Vec<serde_json::Value> = view
            .records
            .iter()
            .map(|s| {
                json!({
                    "pattern": s.record.pattern,
                    "synopsis": s.record.synopsis,
                    "kind": s.record.kind.as_str(),
                    "scope": s.scope.as_str(),
                    "declines": s.record.declines,
                    "snoozed_until": dismiss::iso_utc(s.record.snoozed_until_ms),
                })
            })
            .collect();
        print_json(&json!({ "suggestions": rows, "diagnostics": view.diagnostics }));
        return ExitCode::SUCCESS;
    }
    for diag in &view.diagnostics {
        eprintln!("apb: {diag}");
    }
    if view.records.is_empty() {
        println!("no suggestion decisions recorded (offers are not silenced here)");
        return ExitCode::SUCCESS;
    }
    let mut rows: Vec<Vec<String>> = vec![vec![
        "PATTERN".to_string(),
        "SCOPE".to_string(),
        "KIND".to_string(),
        "DECLINES".to_string(),
        "UNTIL".to_string(),
        "SYNOPSIS".to_string(),
    ]];
    for s in &view.records {
        rows.push(vec![
            s.record.pattern.clone(),
            s.scope.as_str().to_string(),
            s.record.kind.as_str().to_string(),
            s.record.declines.to_string(),
            dismiss::iso_utc(s.record.snoozed_until_ms),
            s.record.synopsis.clone(),
        ]);
    }
    print_table(&rows);
    ExitCode::SUCCESS
}

fn allow_cmd(root: &Path, pattern: &str, global: bool) -> ExitCode {
    let scope = if global {
        DecisionScope::Global
    } else {
        DecisionScope::Project
    };
    match dismiss::remove_record(root, pattern, scope) {
        Ok(outcome) if outcome.removed => {
            let removed = format!("removed `{pattern}` from the {} store", scope.as_str());
            // The same pattern can live in both stores, and this removed only
            // one of them: promising a re-offer while the other scope still
            // silences it would be plainly false the next time the suggestion
            // did not appear.
            match outcome.still_suppressed_by {
                None => println!("{removed}; the suggestion can be offered again"),
                Some(other) => println!(
                    "{removed}, but the {} record still silences it; remove that one with `apb suggestions allow {pattern}{}`",
                    other.as_str(),
                    if other == DecisionScope::Global {
                        " --global"
                    } else {
                        ""
                    }
                ),
            }
            ExitCode::SUCCESS
        }
        Ok(_) => {
            eprintln!(
                "no `{pattern}` record in the {} store (try `apb suggestions list`)",
                scope.as_str()
            );
            ExitCode::from(1)
        }
        Err(e) => {
            eprintln!("could not remove `{pattern}`: {e}");
            ExitCode::from(2)
        }
    }
}

fn reset_cmd(root: &Path, pattern: Option<&str>, all: bool) -> ExitCode {
    if pattern.is_none() && !all {
        eprintln!("name a pattern or pass --all");
        return ExitCode::from(2);
    }
    let outcome = match dismiss::reset_records(root, if all { None } else { pattern }) {
        Ok(o) => o,
        Err(e) => {
            eprintln!("could not reset: {e}");
            return ExitCode::from(2);
        }
    };
    // A skipped hard record is the explanation for a non-zero exit, not a
    // result of the command, so it belongs on stderr: a caller piping stdout
    // into a script must not have to filter this line out of the reset list.
    for hard in &outcome.skipped_hard {
        eprintln!("`{hard}` is a hard record; remove it with `apb suggestions allow {hard}`");
    }
    if outcome.reset.is_empty() {
        if outcome.skipped_hard.is_empty() {
            eprintln!("nothing to reset (try `apb suggestions list`)");
        }
        return ExitCode::from(1);
    }
    for pattern in &outcome.reset {
        println!("reset `{pattern}`: decline counter zeroed, snooze cleared");
    }
    ExitCode::SUCCESS
}
