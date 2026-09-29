//! `apb stats` (C3): cross-run metrics per playbook version and node, from
//! the run journals only. Thin dispatch over `apb_engine::run_stats`; it
//! asks no model and writes nothing.

use std::path::Path;
use std::process::ExitCode;

use apb_engine::run_stats::{self, StatsFilter};

use crate::util::print_json;

pub(crate) fn stats_cmd(
    root: &Path,
    playbook: Option<String>,
    since: Option<&str>,
    compare: Option<String>,
    all_projects: bool,
    json: bool,
) -> ExitCode {
    if compare.is_some() && playbook.is_none() {
        eprintln!("stats: --compare needs --playbook");
        return ExitCode::from(2);
    }
    let since_ms = match since {
        None => None,
        Some(s) => match apb_engine::decision::report::parse_since(s, apb_core::clock::now_ms()) {
            Some(ms) => Some(ms),
            None => {
                eprintln!("stats: --since takes a date (2026-09-20) or a duration (30d, 24h)");
                return ExitCode::from(2);
            }
        },
    };
    let roots = crate::decisions::roots(root, all_projects);
    let filter = StatsFilter {
        playbook,
        since_ms,
        compare,
    };
    let r = run_stats::stats(&roots, &filter);
    let skipped = apb_engine::decision::report::unverified_run_dirs(&roots);
    if skipped > 0 {
        eprintln!(
            "stats: skipped {skipped} run director{} not created by apb on this machine",
            if skipped == 1 { "y" } else { "ies" }
        );
    }
    if json {
        print_json(&serde_json::to_value(&r).unwrap_or_default());
    } else {
        print!("{}", run_stats::render_text(&r));
    }
    ExitCode::SUCCESS
}
