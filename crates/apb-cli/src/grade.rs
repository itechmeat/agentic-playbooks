//! `apb grade`: grade an ended run by rules (the built-in criteria), show
//! its stored grades, reopen a host-graded run so a host may grade it once
//! more, or backfill the rules grades of a playbook's earlier runs so a
//! candidate comparison has a history. A thin dispatch over
//! `apb_mcp::tools::grade`, the layer MCP and the dashboard use, which
//! re-decides the playbook's candidate after every stored grade. Every
//! subcommand but `--show` is refused inside a run step.

use std::path::Path;
use std::process::ExitCode;

use apb_core::quality::grade::Grade;
use apb_mcp::tools::grade::{
    BackfillReport, CandidateAfter, GradeLine, backfill, grade_by_rules, grade_reopen, run_grades,
};
use clap::Args;

use crate::util::{QUESTION_TEXT_MAX, StepCaller, print_json, print_table, sanitize_for_terminal};

/// How many runs `--backfill` grades when `--last` is not given.
const BACKFILL_LAST: usize = 20;
/// Who reopened a run's grading, as its marker keeps it.
const DECIDED_BY: &str = "cli";

#[derive(Args)]
pub(crate) struct GradeArgs {
    /// The ended run to grade
    #[arg(required_unless_present = "backfill", conflicts_with = "backfill")]
    run: Option<String>,
    /// Only show the run's stored grades; grade nothing
    #[arg(long, conflicts_with = "reopen")]
    show: bool,
    /// Let a host grade the run once more: a run takes one host grade
    /// until a person reopens it
    #[arg(long)]
    reopen: bool,
    /// Grade by rules the latest ended runs of this playbook that have no
    /// rules grade yet
    #[arg(long, value_name = "PLAYBOOK")]
    backfill: Option<String>,
    /// With --backfill: only runs of this version
    #[arg(long)]
    version: Option<String>,
    /// With --backfill: how many ungraded runs to grade at most (default 20)
    #[arg(long)]
    last: Option<usize>,
    /// Machine-readable output (the same objects as MCP)
    #[arg(long)]
    json: bool,
}

pub(crate) fn grade_cmd(root: &Path, args: GradeArgs) -> ExitCode {
    if let Some(id) = &args.backfill {
        if args.show || args.reopen {
            eprintln!("--show and --reopen take a run, not --backfill");
            return ExitCode::from(2);
        }
        let last = args.last.unwrap_or(BACKFILL_LAST);
        return backfill_cmd(root, id, args.version.as_deref(), last, args.json);
    }
    if args.version.is_some() || args.last.is_some() {
        eprintln!("--version and --last go with --backfill <playbook>");
        return ExitCode::from(2);
    }
    let run = args.run.unwrap_or_default();
    if args.show {
        return show(root, &run, args.json);
    }
    let step = StepCaller::from_env();
    if args.reopen {
        return match grade_reopen(root, &run, DECIDED_BY, &step) {
            Ok(v) => {
                if args.json {
                    print_json(&v);
                } else {
                    println!("{run}: reopened; a host may grade it once more (run_grade_start)");
                }
                ExitCode::SUCCESS
            }
            Err(e) => {
                eprintln!("grade --reopen failed: {e}");
                ExitCode::from(2)
            }
        };
    }
    match grade_by_rules(root, &run, &step) {
        Ok(graded) => {
            if args.json {
                print_json(&serde_json::json!(graded));
            } else {
                print_grade(&graded.grade);
                print_after(&graded.after);
            }
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("grade failed: {e}");
            ExitCode::from(2)
        }
    }
}

fn show(root: &Path, run: &str, json: bool) -> ExitCode {
    let grades = match run_grades(root, run) {
        Ok(g) => g,
        Err(e) => {
            eprintln!("grade --show failed: {e}");
            return ExitCode::from(2);
        }
    };
    if json {
        print_json(&serde_json::json!({ "run_id": run, "grades": grades }));
    } else if grades.is_empty() {
        println!("{run}: not graded (apb grade {run} grades it by rules)");
    } else {
        for g in &grades {
            print_grade(g);
        }
    }
    ExitCode::SUCCESS
}

fn backfill_cmd(root: &Path, id: &str, version: Option<&str>, last: usize, json: bool) -> ExitCode {
    let report: BackfillReport = match backfill(root, id, version, last, &StepCaller::from_env()) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("grade --backfill failed: {e}");
            return ExitCode::from(2);
        }
    };
    if json {
        print_json(&serde_json::json!(report));
        return ExitCode::SUCCESS;
    }
    let graded = report.runs.iter().filter(|r| r.grade_id.is_some()).count();
    println!(
        "{id}: graded {graded} of {} runs by rules",
        report.runs.len()
    );
    for r in &report.runs {
        let what = match (&r.grade_id, &r.skipped) {
            (Some(g), _) => format!("graded {g}"),
            (None, Some(why)) => {
                format!("skipped: {}", sanitize_for_terminal(why, QUESTION_TEXT_MAX))
            }
            (None, None) => "skipped".to_string(),
        };
        println!("  {}  {what}", r.run_id);
    }
    print_after(&report.after);
    ExitCode::SUCCESS
}

/// One grade: its header, then a row per criterion.
fn print_grade(g: &Grade) {
    println!(
        "{}  {} {}  {}  {}",
        g.run_id,
        g.playbook,
        g.version,
        g.grader,
        apb_core::clock::iso_utc(g.created_at)
    );
    let mut rows = vec![vec![
        "  criterion".to_string(),
        "level".to_string(),
        "reason".to_string(),
        "cites".to_string(),
        "why".to_string(),
    ]];
    for c in &g.criteria {
        rows.push(vec![
            format!("  {}", c.id),
            c.level.as_str().to_string(),
            sanitize_for_terminal(&c.reason_code, QUESTION_TEXT_MAX),
            c.cites.len().to_string(),
            sanitize_for_terminal(&c.why, QUESTION_TEXT_MAX),
        ]);
    }
    print_table(&rows);
    println!(
        "  read {} of {} chars in {} steps",
        g.reading.read_chars,
        g.reading.budget_chars,
        g.reading.ops.len()
    );
}

/// What the stored grade did to the playbook's candidate, when it did
/// anything.
fn print_after(after: &CandidateAfter) {
    if let Some(rec) = &after.candidate {
        println!(
            "candidate: {} ({})",
            rec.verdict.as_str(),
            sanitize_for_terminal(&rec.reason, QUESTION_TEXT_MAX)
        );
    }
    if let Some(e) = &after.candidate_error {
        eprintln!(
            "candidate not re-decided: {}",
            sanitize_for_terminal(e, QUESTION_TEXT_MAX)
        );
    }
}

/// The `grade` lines of `apb runs <id>`: the latest grade of each grader.
pub(crate) fn run_lines(run_dir: &Path) -> Vec<String> {
    apb_mcp::tools::grade::grade_lines(run_dir)
        .iter()
        .map(GradeLine::line)
        .collect()
}
