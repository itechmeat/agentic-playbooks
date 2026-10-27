//! `apb problems`: a playbook's known problems (trouble with its tools that
//! earlier runs reported), a person's decision on one (confirm, dismiss,
//! reopen, close with a quote of new source text), a recheck of the
//! problems whose source changed, and a backfill scan of earlier runs. A
//! thin dispatch over `apb_mcp::tools::problems`, the layer MCP and the
//! dashboard use; every subcommand that writes is refused inside a run
//! step there.

use std::path::Path;
use std::process::ExitCode;

use apb_core::known_problems::{KnownProblem, RecheckRecord};
use apb_engine::known_problems::{ProblemView, ProblemsView, RecheckBy, SourceNow};
use apb_mcp::tools::ToolError;
use apb_mcp::tools::problems::{
    ProblemDetail, Target, action_of, changed, person_holds, problem_decide, problem_detail,
    problem_recheck, problems_scan, problems_view, recheck_changed_by_model,
};
use clap::{Args, Subcommand};

use crate::util::{QUESTION_TEXT_MAX, StepCaller, print_json, print_table, sanitize_for_terminal};

/// Who decided or rechecked, as the store keeps it.
const DECIDED_BY: &str = "cli";
/// How many ended runs `scan` looks at when `--last` is not given.
const SCAN_LAST: usize = 20;

#[derive(Args)]
pub(crate) struct ProblemsArgs {
    /// The playbook
    playbook: String,
    /// `project` or `global`; defaults to where the playbook lives
    #[arg(long, global = true)]
    scope: Option<String>,
    /// Machine-readable output (the same objects as MCP)
    #[arg(long, global = true)]
    json: bool,
    #[command(subcommand)]
    action: Option<ProblemsAction>,
}

#[derive(Subcommand)]
enum ProblemsAction {
    /// Show it to runs even before it recurs
    Confirm(Decision),
    /// Never show it
    Dismiss(Decision),
    /// Open a closed or dismissed problem again
    Reopen(Decision),
    /// Close it: quote text of its source's new version that shows the fix
    Close {
        id: String,
        /// Text copied exactly from the source's new text (see `recheck`)
        #[arg(long)]
        quote: String,
        #[arg(long)]
        reason: Option<String>,
    },
    /// The problems whose source changed since they were checked, with the
    /// new passages; --model asks the project's decision model instead,
    /// --holds records that the problem still holds after the change
    Recheck {
        /// One problem; all changed ones when omitted
        id: Option<String>,
        /// Ask the decision model (its `problem_recheck` use must be on)
        #[arg(long, conflicts_with = "holds")]
        model: bool,
        /// The problem still holds after its source changed (your word)
        #[arg(long, requires = "id")]
        holds: bool,
    },
    /// Collect the problems of earlier ended runs (a run counts once)
    Scan {
        /// How many of the latest ended runs to look at (default 20, at
        /// most 200)
        #[arg(long)]
        last: Option<usize>,
    },
}

#[derive(Args)]
struct Decision {
    id: String,
    /// Why, kept with the decision
    #[arg(long)]
    reason: Option<String>,
}

pub(crate) fn problems_cmd(root: &Path, args: ProblemsArgs) -> ExitCode {
    let json = args.json;
    let target = match Target::resolve(root, &args.playbook, args.scope.as_deref()) {
        Ok(t) => t,
        Err(e) => return failed("problems", e),
    };
    let step = StepCaller::from_env();
    match args.action {
        None => list(root, &target, json),
        Some(ProblemsAction::Confirm(d)) => decide(root, &target, "confirm", d, None, json, &step),
        Some(ProblemsAction::Dismiss(d)) => decide(root, &target, "dismiss", d, None, json, &step),
        Some(ProblemsAction::Reopen(d)) => decide(root, &target, "reopen", d, None, json, &step),
        Some(ProblemsAction::Close { id, quote, reason }) => decide(
            root,
            &target,
            "close",
            Decision { id, reason },
            Some(quote),
            json,
            &step,
        ),
        Some(ProblemsAction::Recheck {
            id: Some(id),
            holds: true,
            ..
        }) => match problem_recheck(root, &target, &id, person_holds(), DECIDED_BY, &step) {
            Ok(r) => print_check(&id, &r, json),
            Err(e) => failed("problems recheck", e),
        },
        Some(ProblemsAction::Recheck {
            id, model: true, ..
        }) => recheck_by_model(root, &target, id.as_deref(), json, &step),
        Some(ProblemsAction::Recheck { id, .. }) => {
            show_changed(root, &target, id.as_deref(), json)
        }
        Some(ProblemsAction::Scan { last }) => {
            match problems_scan(root, &target, last.unwrap_or(SCAN_LAST), &step) {
                Ok(r) if json => {
                    print_json(&serde_json::json!(r));
                    ExitCode::SUCCESS
                }
                Ok(r) => {
                    println!(
                        "{}: looked at {} ended runs, {} sightings",
                        target.id, r.runs, r.sightings
                    );
                    ExitCode::SUCCESS
                }
                Err(e) => failed("problems scan", e),
            }
        }
    }
}

fn failed(what: &str, e: ToolError) -> ExitCode {
    eprintln!(
        "{what} failed: {}",
        sanitize_for_terminal(&e.to_string(), 600)
    );
    ExitCode::from(2)
}

fn text(s: &str) -> String {
    sanitize_for_terminal(s, QUESTION_TEXT_MAX)
}

/// A passage as its source has it, so it can be copied into `--quote`
/// whole: line breaks and tabs kept, never cut, and only a hidden or
/// control character (which no quote may hold anyway) replaced.
fn verbatim(s: &str) -> String {
    s.split('\n')
        .map(|line| {
            line.split('\t')
                .map(|part| apb_core::text::replace_hidden(part, '\u{fffd}'))
                .collect::<Vec<_>>()
                .join("\t")
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn source_label(p: &ProblemView) -> &'static str {
    match p.source_now {
        Some(SourceNow::Unchanged) => "unchanged",
        Some(SourceNow::Changed) => "changed",
        Some(SourceNow::Unreadable) => "unreadable",
        None => "none",
    }
}

fn list(root: &Path, t: &Target, json: bool) -> ExitCode {
    let view: ProblemsView = match problems_view(root, t) {
        Ok(v) => v,
        Err(e) => return failed("problems", e),
    };
    if json {
        print_json(&serde_json::json!(view));
        return ExitCode::SUCCESS;
    }
    if !view.signed {
        println!(
            "{} ({}): the problem file was not written by this apb installation; its {} problems are neither shown to runs nor listed",
            view.playbook, view.scope, view.unsigned_problems
        );
    }
    if view.problems.is_empty() {
        println!(
            "{} ({}): no known problems (apb problems {} scan collects earlier runs)",
            view.playbook, view.scope, view.playbook
        );
        return ExitCode::SUCCESS;
    }
    let shown = view.problems.iter().filter(|p| p.significant).count();
    println!(
        "{} ({}): {} known problems, {shown} shown to runs",
        view.playbook,
        view.scope,
        view.problems.len()
    );
    let mut rows = vec![
        [
            "  id", "status", "runs", "shown", "source", "scope", "symptom",
        ]
        .map(str::to_string)
        .to_vec(),
    ];
    for p in &view.problems {
        let k = &p.problem;
        let status = if p.needs_look {
            format!("{} (needs a look)", status_of(k))
        } else {
            status_of(k).to_string()
        };
        rows.push(vec![
            format!("  {}", text(&k.id)),
            status,
            k.runs.to_string(),
            if p.significant { "yes" } else { "no" }.to_string(),
            source_label(p).to_string(),
            text(&k.scope.key()),
            text(&k.symptom),
        ]);
    }
    print_table(&rows);
    ExitCode::SUCCESS
}

fn status_of(k: &KnownProblem) -> &'static str {
    match k.status {
        apb_core::known_problems::ProblemStatus::Open => "open",
        apb_core::known_problems::ProblemStatus::Closed => "closed",
        apb_core::known_problems::ProblemStatus::Dismissed => "dismissed",
    }
}

fn decide(
    root: &Path,
    t: &Target,
    action: &str,
    d: Decision,
    quote: Option<String>,
    json: bool,
    step: &StepCaller,
) -> ExitCode {
    let what = format!("problems {action}");
    let act = match action_of(action, quote) {
        Ok(a) => a,
        Err(e) => return failed(&what, e),
    };
    match problem_decide(root, t, &d.id, act, d.reason.as_deref(), DECIDED_BY, step) {
        Ok(k) if json => {
            print_json(&serde_json::json!(k));
            ExitCode::SUCCESS
        }
        Ok(k) => {
            let confirmed = if k.confirmed.is_some() {
                ", confirmed"
            } else {
                ""
            };
            println!("{}: {}{confirmed}", text(&k.id), status_of(&k));
            ExitCode::SUCCESS
        }
        Err(e) => failed(&what, e),
    }
}

fn print_check(id: &str, r: &RecheckRecord, json: bool) -> ExitCode {
    if json {
        print_json(&serde_json::json!(r));
    } else {
        print_record(id, r);
    }
    ExitCode::SUCCESS
}

fn print_record(id: &str, r: &RecheckRecord) {
    let verdict = serde_json::to_value(r.verdict)
        .ok()
        .and_then(|v| v.as_str().map(str::to_string))
        .unwrap_or_default();
    println!(
        "{}: {verdict} ({}, {})",
        text(id),
        r.basis.as_str(),
        text(&r.reason_code)
    );
    if let Some(q) = &r.quote {
        println!(
            "  quote (lines {}-{}): {}",
            q.lines[0],
            q.lines[1],
            text(&q.text)
        );
    }
}

fn recheck_by_model(
    root: &Path,
    t: &Target,
    id: Option<&str>,
    json: bool,
    step: &StepCaller,
) -> ExitCode {
    match id {
        Some(id) => match problem_recheck(root, t, id, RecheckBy::Model, DECIDED_BY, step) {
            Ok(r) => print_check(id, &r, json),
            Err(e) => failed("problems recheck", e),
        },
        None => match recheck_changed_by_model(root, t, DECIDED_BY, step) {
            Ok(all) => {
                // Any problem not rechecked fails the command, so a script
                // can tell "nothing changed" from "nothing was rechecked".
                let failed_any = all.iter().any(|r| r.error.is_some());
                if json {
                    print_json(&serde_json::json!(all));
                } else {
                    if all.is_empty() {
                        println!("{}: no open problem's source changed", t.id);
                    }
                    for r in &all {
                        match (&r.check, &r.error) {
                            (Some(c), _) => print_record(&r.id, c),
                            (None, Some(e)) => {
                                eprintln!("{}: not rechecked: {}", text(&r.id), text(e))
                            }
                            (None, None) => {}
                        }
                    }
                }
                if failed_any {
                    ExitCode::from(2)
                } else {
                    ExitCode::SUCCESS
                }
            }
            Err(e) => failed("problems recheck", e),
        },
    }
}

/// The problems whose source changed (or the one named), each with the
/// passages a close may quote.
fn show_changed(root: &Path, t: &Target, id: Option<&str>, json: bool) -> ExitCode {
    let ids: Vec<String> = match id {
        Some(id) => vec![id.to_string()],
        None => match problems_view(root, t) {
            Ok(v) => changed(&v).iter().map(|p| p.problem.id.clone()).collect(),
            Err(e) => return failed("problems recheck", e),
        },
    };
    let mut details: Vec<ProblemDetail> = Vec::new();
    for id in &ids {
        match problem_detail(root, t, id) {
            Ok(d) => details.push(d),
            Err(e) => return failed("problems recheck", e),
        }
    }
    if json {
        print_json(&serde_json::json!(details));
        return ExitCode::SUCCESS;
    }
    if details.is_empty() {
        println!("{}: no open problem's source changed", t.id);
        return ExitCode::SUCCESS;
    }
    for d in &details {
        let k = &d.problem.problem;
        println!(
            "{}  {}  source {}",
            text(&k.id),
            text(&k.scope.key()),
            source_label(&d.problem)
        );
        println!("  symptom: {}", text(&k.symptom));
        if d.changed_passages.is_empty() {
            println!("  no new passage to quote");
        }
        // Each passage on lines of its own, exactly as the source has it:
        // what is printed is what `--quote` must carry.
        for q in &d.changed_passages {
            println!(
                "  passage {} (lines {}-{}):\n{}",
                q.index,
                q.lines[0],
                q.lines[1],
                verbatim(&q.text)
            );
        }
    }
    println!(
        "close one with: apb problems {} close <id> --quote \"<text copied from a passage>\"; or keep it: apb problems {} recheck <id> --holds",
        t.id, t.id
    );
    ExitCode::SUCCESS
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_passage_prints_whole_with_its_lines_and_tabs() {
        let passage = format!(
            "make all 2>&1 | tee build.log\n\tcp build.log out/\n{}",
            "x".repeat(450)
        );
        assert_eq!(verbatim(&passage), passage);
        assert_eq!(verbatim("a\u{1b}[2Jb\u{202e}c"), "a\u{fffd}[2Jb\u{fffd}c");
    }
}
