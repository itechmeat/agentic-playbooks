//! `apb runs prune`: the run retention of `policy.yaml` (`runs.keep_days`,
//! `runs.keep_last`, `runs.keep_failed_days`). Without `--yes` it prints
//! the plan and writes nothing; with `--yes`, a person's command refused
//! inside a run step, it removes the families the plan removes and leaves
//! a signed summary of each in `.apb/runs-archive/`.

use std::path::Path;
use std::process::ExitCode;

use apb_engine::retention::{self, PruneAction, PruneRow};
use clap::{Args, Subcommand};

use crate::util::print_json;

#[derive(Subcommand)]
pub(crate) enum RunsAction {
    /// Remove old runs the retention settings no longer keep: a dry run
    /// unless --yes
    Prune(PruneArgs),
}

#[derive(Args)]
pub(crate) struct PruneArgs {
    /// Remove the runs the plan removes (without it nothing is written)
    #[arg(long)]
    pub yes: bool,
    /// Print the rows as JSON
    #[arg(long)]
    pub json: bool,
    /// Only the runs of this playbook
    #[arg(long)]
    pub playbook: Option<String>,
}

/// `1.3 GB`, `420 KB`, `12 B`.
fn size(bytes: u64) -> String {
    const UNITS: [&str; 4] = ["KB", "MB", "GB", "TB"];
    if bytes < 1000 {
        return format!("{bytes} B");
    }
    let mut v = bytes as f64;
    let mut unit = "B";
    for u in UNITS {
        if v < 1000.0 {
            break;
        }
        v /= 1000.0;
        unit = u;
    }
    format!("{v:.1} {unit}")
}

/// One plan row; each field read from a run directory is cleaned for
/// the terminal on its own.
fn line(r: &PruneRow) -> String {
    let clean = |s: &str| crate::util::sanitize_for_terminal(s, 120);
    let day = r.started.get(..10).unwrap_or(&r.started);
    let mut fields = vec![
        match r.action {
            PruneAction::Remove => "remove".to_string(),
            PruneAction::Keep(_) => "keep  ".to_string(),
        },
        clean(&r.run_id),
        clean(&r.playbook),
        clean(day),
        clean(&r.outcome),
    ];
    if let PruneAction::Keep(why) = r.action {
        fields.push(why.as_str().to_string());
    }
    fields.join("  ")
}

pub(crate) fn prune_cmd(root: &Path, args: PruneArgs) -> ExitCode {
    if args.yes
        && let Some(why) =
            apb_core::step_env::StepCaller::from_env().refusal("apb runs prune --yes")
    {
        eprintln!("{why}");
        return ExitCode::from(2);
    }
    let settings = match apb_core::policy_file::Policies::load(root) {
        Ok(p) => p.retention(),
        Err(e) => {
            eprintln!("apb: {e}");
            return ExitCode::from(2);
        }
    };
    let now = apb_core::clock::now_ms_u64();
    let only = args.playbook.as_deref();
    let rows = if args.yes {
        match retention::apply(root, &settings, only, now) {
            Ok(report) => {
                if args.json {
                    print_json(&serde_json::to_value(&report.rows).unwrap_or_default());
                } else {
                    println!(
                        "removed {} runs ({}), kept {}",
                        report.removed,
                        size(report.bytes),
                        report.kept
                    );
                }
                return ExitCode::SUCCESS;
            }
            Err(e) => {
                eprintln!("apb: {e}");
                return ExitCode::from(2);
            }
        }
    } else {
        match retention::plan(root, &settings, only, now) {
            Ok(rows) => rows,
            Err(e) => {
                eprintln!("apb: {e}");
                return ExitCode::from(2);
            }
        }
    };
    if args.json {
        print_json(&serde_json::to_value(&rows).unwrap_or_default());
        return ExitCode::SUCCESS;
    }
    for r in &rows {
        println!("{}", line(r));
    }
    let removed: Vec<&PruneRow> = rows
        .iter()
        .filter(|r| r.action == PruneAction::Remove)
        .collect();
    let bytes: u64 = removed.iter().map(|r| r.bytes).sum();
    println!(
        "{} runs would be removed ({}), {} kept; run again with --yes to remove them",
        removed.len(),
        size(bytes),
        rows.len() - removed.len()
    );
    ExitCode::SUCCESS
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sizes_read_as_a_person_writes_them() {
        assert_eq!(size(12), "12 B");
        assert_eq!(size(420_000), "420.0 KB");
        assert_eq!(size(1_300_000_000), "1.3 GB");
    }
}
