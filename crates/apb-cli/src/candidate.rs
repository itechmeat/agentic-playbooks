//! `apb candidate`: a playbook's candidate version (a forward patch on
//! trial), how it compares with `current` by quality, and a person's
//! decision on it: `--promote` makes it `current`, `--keep-current` drops
//! it and changes nothing. A thin dispatch over `apb_mcp::tools::candidate`,
//! the layer MCP and the dashboard use.

use std::path::Path;
use std::process::ExitCode;

use apb_core::quality::compare::{CriterionComparison, Tally};
use apb_engine::candidate::summary::{
    Axis, AxisState, CandidateSummary, SummaryItem, VersionDigest,
};
use apb_engine::candidate::{CandidateQualityView, PersonDecision};
use apb_mcp::tools::candidate::{candidate_decide, candidate_summary};
use clap::Args;

use crate::util::{QUESTION_TEXT_MAX, print_json, print_table, sanitize_for_terminal};

/// Who decided, as the trial record keeps it.
const DECIDED_BY: &str = "cli";

#[derive(Args)]
pub(crate) struct CandidateArgs {
    /// The project playbook
    playbook: String,
    /// Make the candidate the current version
    #[arg(long, conflicts_with = "keep_current", requires = "expect")]
    promote: bool,
    /// With --promote: the candidate's <version>@<digest> as shown, so a
    /// candidate replaced or edited since is not promoted
    #[arg(long, requires = "promote", value_name = "VERSION@DIGEST")]
    expect: Option<VersionDigest>,
    /// With --promote: the summary id the attention items were read under
    #[arg(long, requires = "promote", value_name = "SUMMARY_ID")]
    ack: Option<String>,
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
    let decision = if let (true, Some(expected)) = (args.promote, args.expect) {
        Some(PersonDecision::Promote {
            expected,
            acknowledged: args.ack,
        })
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
    let summary = match candidate_summary(root, id) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("candidate failed: {e}");
            return ExitCode::from(2);
        }
    };
    if json {
        let view = summary.as_ref().map(|s| &s.quality.detail.view);
        print_json(&serde_json::json!({ "id": id, "candidate": view, "summary": summary }));
        return ExitCode::SUCCESS;
    }
    match summary {
        Some(s) => print_summary(id, &s),
        None => println!("{id}: no candidate on trial"),
    }
    ExitCode::SUCCESS
}

fn state(s: AxisState) -> &'static str {
    match s {
        AxisState::Ok => "ok",
        AxisState::Attention => "attention",
        AxisState::Blocking => "blocking",
        AxisState::Unknown => "unknown",
    }
}

fn axis_line<T>(name: &str, a: &Axis<T>) {
    println!("  {name}: {}", state(a.state));
}

fn print_items(title: &str, items: &[SummaryItem]) {
    if items.is_empty() {
        return;
    }
    println!("  {title}:");
    for i in items {
        println!(
            "    {} {}",
            i.code.as_str(),
            sanitize_for_terminal(&i.detail, QUESTION_TEXT_MAX)
        );
    }
}

/// The summary: the candidate and its digest, the three axes apart, the
/// items, then the quality comparison as before and how to decide.
fn print_summary(id: &str, s: &CandidateSummary) {
    let v = &s.quality.detail.view;
    println!(
        "{id}: candidate {} against current {}, {} trial runs",
        s.candidate, s.current.version, v.trials
    );
    axis_line("fitness on this machine", &s.fitness);
    axis_line("definition", &s.definition);
    for r in &s.definition.detail.scenarios {
        println!(
            "    scenario {} at {}: {}{}",
            r.scenario,
            r.level.as_str(),
            r.verdict.as_str(),
            if r.fresh { "" } else { " (stale)" }
        );
    }
    axis_line("quality", &s.quality);
    print_items("blocking", &s.blocking);
    print_items("needs acknowledging", &s.attention);
    print_view(v);
    if s.blocking.is_empty() {
        let ack = if s.attention.is_empty() {
            String::new()
        } else {
            format!(" --ack {}", s.summary_id)
        };
        println!(
            "  decide: apb candidate {id} --promote --expect {}{ack} | --keep-current [--reason <why>]",
            s.candidate
        );
    } else {
        println!("  decide: apb candidate {id} --keep-current [--reason <why>]");
    }
}

fn print_view(v: &CandidateQualityView) {
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
