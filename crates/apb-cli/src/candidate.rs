//! `apb candidate`: a playbook's candidate version (a forward patch on
//! trial), how it compares with `current` by quality, and a person's
//! decision on it: `--promote` makes it `current`, `--keep-current` drops
//! it and changes nothing. A thin dispatch over `apb_mcp::tools::candidate`,
//! the layer MCP and the dashboard use.

use std::path::Path;
use std::process::ExitCode;

use apb_core::quality::compare::{CriterionComparison, Tally};
use apb_engine::candidate::{CandidateQualityView, PersonDecision};
use apb_mcp::tools::candidate::{candidate_decide, candidate_view};
use clap::Args;

use crate::util::{QUESTION_TEXT_MAX, print_json, print_table, sanitize_for_terminal};

/// Who decided, as the trial record keeps it.
const DECIDED_BY: &str = "cli";

#[derive(Args)]
pub(crate) struct CandidateArgs {
    /// The project playbook
    playbook: String,
    /// Make the candidate the current version
    #[arg(long, conflicts_with = "keep_current")]
    promote: bool,
    /// Keep the current version and drop the candidate (change nothing)
    #[arg(long)]
    keep_current: bool,
    /// With --keep-current: why, kept on the trial record
    #[arg(long, requires = "keep_current")]
    reason: Option<String>,
    /// Machine-readable output (the same object as MCP playbook_candidate)
    #[arg(long)]
    json: bool,
}

pub(crate) fn candidate_cmd(root: &Path, args: CandidateArgs) -> ExitCode {
    let id = args.playbook.as_str();
    let decision = if args.promote {
        Some(PersonDecision::Promote)
    } else if args.keep_current {
        Some(PersonDecision::KeepCurrent {
            reason: args.reason,
        })
    } else {
        None
    };
    let Some(decision) = decision else {
        return show(root, id, args.json);
    };
    // The CLI runs in its caller's process: from inside a run step the
    // decision is refused.
    match candidate_decide(
        root,
        id,
        decision,
        DECIDED_BY,
        &crate::util::StepCaller::from_env(),
    ) {
        Ok(rec) => {
            if args.json {
                print_json(&serde_json::json!(rec));
            } else {
                println!(
                    "{id}: {} ({})",
                    rec.verdict.as_str(),
                    sanitize_for_terminal(&rec.reason, QUESTION_TEXT_MAX)
                );
            }
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("candidate decision failed: {e}");
            ExitCode::from(2)
        }
    }
}

fn show(root: &Path, id: &str, json: bool) -> ExitCode {
    let view = match candidate_view(root, id) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("candidate failed: {e}");
            return ExitCode::from(2);
        }
    };
    if json {
        print_json(&serde_json::json!({ "id": id, "candidate": view }));
        return ExitCode::SUCCESS;
    }
    match view {
        Some(v) => print_view(id, &v),
        None => println!("{id}: no candidate on trial"),
    }
    ExitCode::SUCCESS
}

fn print_view(id: &str, v: &CandidateQualityView) {
    println!(
        "{id}: candidate {} against current {}, {} trial runs",
        v.version, v.current, v.trials
    );
    match &v.gate {
        Some(g) => println!(
            "  gate: {}, min_runs {}, max_trials {}, window {}, margin {}",
            serde_json::json!(g.compare).as_str().unwrap_or_default(),
            g.min_runs,
            g.max_trials,
            g.window,
            g.margin
        ),
        None => println!("  promoted by successes, not by quality"),
    }
    if let Some(d) = &v.decision {
        println!(
            "  now: {} ({})",
            d.verdict.as_str(),
            sanitize_for_terminal(&d.reason, QUESTION_TEXT_MAX)
        );
        print_comparison(&d.per_criterion);
    }
    if let Some(r) = &v.recorded {
        println!(
            "  recorded: {} by {} at {} ({})",
            r.verdict.as_str(),
            r.decided_by,
            apb_core::clock::iso_utc(r.decided_at),
            sanitize_for_terminal(&r.reason, QUESTION_TEXT_MAX)
        );
    }
    println!("  decide: apb candidate {id} --promote | --keep-current [--reason <why>]");
}

/// `good/partial/poor`, then the counts without a level; a side with no
/// grade at all says so.
fn tally(t: &Tally) -> String {
    if t.total() == 0 {
        return "no graded runs yet".to_string();
    }
    format!(
        "{}/{}/{} (+{} n/a, {} no data)",
        t.good, t.partial, t.poor, t.not_applicable, t.not_enough_data
    )
}

fn print_comparison(rows: &[CriterionComparison]) {
    if rows.is_empty() {
        return;
    }
    let mut table = vec![vec![
        "  criterion".to_string(),
        "grader".to_string(),
        "candidate good/partial/poor".to_string(),
        "current good/partial/poor".to_string(),
        "result".to_string(),
    ]];
    for c in rows {
        table.push(vec![
            format!("  {}", c.key.criterion),
            c.key.grader.clone(),
            tally(&c.candidate),
            tally(&c.current),
            serde_json::json!(c.result)
                .as_str()
                .unwrap_or_default()
                .to_string(),
        ]);
    }
    print_table(&table);
}
