//! `apb decisions` subcommands (issue #165 Part 13): the report over the
//! journaled decisions, the measured-threshold store, and replay against
//! another provider. Thin dispatch over `apb_engine::decision::report` and
//! `apb_core::decision_thresholds`; none of them writes a run journal.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use apb_engine::decision::report::{self, ReportFilter, ReportSettings, replay};
use clap::Subcommand;

use crate::util::print_json;

#[derive(Subcommand)]
pub(crate) enum DecisionsAction {
    /// How well each use's journaled decisions would have acted, per use and
    /// provider/model, against labels the run journals already hold; and
    /// whether each is eligible for enforce. Reads journals only
    Report {
        /// Only this use (completion_check, retry_advice, ...)
        #[arg(long = "use", value_name = "USE")]
        use_site: Option<String>,
        /// Only decisions since a date (2026-09-20, UTC) or a duration back
        /// from now (7d, 24h)
        #[arg(long)]
        since: Option<String>,
        /// Only runs of this playbook id
        #[arg(long)]
        playbook: Option<String>,
        /// Only decisions answered by (or asked of) this provider id
        #[arg(long)]
        provider: Option<String>,
        /// Every registered project, not only this one
        #[arg(long)]
        all_projects: bool,
        /// Machine-readable output
        #[arg(long)]
        json: bool,
    },
    /// The measured thresholds the enforce modes read
    /// (`<config_dir>/decisions-thresholds.yaml`)
    Thresholds {
        #[command(subcommand)]
        action: ThresholdsAction,
    },
    /// Re-ask decisions whose run kept its debug state against another
    /// provider, for evaluation only: prints agreement and labelled accuracy
    /// side by side and saves the results under
    /// `<config_dir>/decisions-replay/`. Never writes a run journal
    Replay {
        /// The provider id from decisions.yaml to re-ask (required)
        #[arg(long)]
        provider: Option<String>,
        #[arg(long = "use", value_name = "USE")]
        use_site: Option<String>,
        #[arg(long)]
        since: Option<String>,
        /// At most this many requests
        #[arg(long, default_value_t = 200)]
        max: usize,
        #[arg(long)]
        json: bool,
    },
}

#[derive(Subcommand)]
pub(crate) enum ThresholdsAction {
    /// Store the threshold for exactly one use, provider id and model id; a
    /// different model id never inherits it
    Set {
        #[arg(long = "use", value_name = "USE")]
        use_site: String,
        #[arg(long)]
        provider: String,
        #[arg(long)]
        model: String,
        #[arg(long)]
        threshold: f64,
    },
    /// List the stored thresholds
    List {
        #[arg(long)]
        json: bool,
    },
}

pub(crate) fn decisions_cmd(root: &Path, action: DecisionsAction) -> ExitCode {
    let config_dir = apb_core::config::config_dir();
    match action {
        DecisionsAction::Report {
            use_site,
            since,
            playbook,
            provider,
            all_projects,
            json,
        } => {
            let Some(filter) = filter(use_site, since.as_deref(), playbook, provider) else {
                return ExitCode::from(2);
            };
            let roots = roots(root, all_projects);
            let settings = ReportSettings::load(config_dir.as_deref());
            let r = report::report(&roots, &filter, &settings);
            if json {
                print_json(&serde_json::to_value(&r).unwrap_or_default());
            } else {
                print!("{}", report::render_text(&r));
            }
            ExitCode::SUCCESS
        }
        DecisionsAction::Thresholds { action } => {
            let Some(dir) = config_dir else {
                eprintln!("decisions: no config directory (set APB_CONFIG_DIR or HOME)");
                return ExitCode::from(2);
            };
            thresholds_cmd(&dir, action)
        }
        DecisionsAction::Replay {
            provider,
            use_site,
            since,
            max,
            json,
        } => {
            let Some(dir) = config_dir else {
                eprintln!("decisions: no config directory (set APB_CONFIG_DIR or HOME)");
                return ExitCode::from(2);
            };
            let Some(filter) = filter(use_site, since.as_deref(), None, None) else {
                return ExitCode::from(2);
            };
            match replay::replay(
                &[root.to_path_buf()],
                &dir,
                provider.as_deref(),
                &filter,
                max,
            ) {
                Ok(s) if json => {
                    print_json(&serde_json::to_value(&s).unwrap_or_default());
                    ExitCode::SUCCESS
                }
                Ok(s) => {
                    print_replay(&s);
                    ExitCode::SUCCESS
                }
                Err(e) => {
                    eprintln!("decisions replay: {e}");
                    ExitCode::from(2)
                }
            }
        }
    }
}

fn filter(
    use_site: Option<String>,
    since: Option<&str>,
    playbook: Option<String>,
    provider: Option<String>,
) -> Option<ReportFilter> {
    let since_ms = match since {
        None => None,
        Some(s) => match report::parse_since(s, apb_core::clock::now_ms()) {
            Some(ms) => Some(ms),
            None => {
                eprintln!("decisions: --since takes a date (2026-09-20) or a duration (7d, 24h)");
                return None;
            }
        },
    };
    Some(ReportFilter {
        use_site,
        since_ms,
        playbook,
        provider,
    })
}

fn roots(root: &Path, all_projects: bool) -> Vec<PathBuf> {
    if !all_projects {
        return vec![root.to_path_buf()];
    }
    let mut roots: Vec<PathBuf> = apb_core::projects::list_reachable()
        .into_iter()
        .map(|p| PathBuf::from(p.path))
        .collect();
    if !roots.iter().any(|r| r == root) && root.join(".apb").is_dir() {
        roots.push(root.to_path_buf());
    }
    roots
}

fn thresholds_cmd(dir: &Path, action: ThresholdsAction) -> ExitCode {
    use apb_core::decision_thresholds::{THRESHOLDS_FILE, load_in, set_threshold_in};
    match action {
        ThresholdsAction::Set {
            use_site,
            provider,
            model,
            threshold,
        } => match set_threshold_in(dir, &use_site, &provider, &model, threshold) {
            Ok(()) => {
                println!(
                    "stored {use_site} {provider}/{model}: {threshold} in {THRESHOLDS_FILE} (applies to this exact model id only)"
                );
                ExitCode::SUCCESS
            }
            Err(e) => {
                eprintln!("decisions thresholds set: {e}");
                ExitCode::from(2)
            }
        },
        ThresholdsAction::List { json } => match load_in(dir) {
            Ok(all) if json => {
                print_json(&serde_json::to_value(&all).unwrap_or_default());
                ExitCode::SUCCESS
            }
            Ok(all) if all.is_empty() => {
                println!("no stored thresholds");
                ExitCode::SUCCESS
            }
            Ok(all) => {
                for t in all {
                    println!(
                        "{}\t{}/{}\t{}",
                        t.use_name, t.provider, t.model, t.threshold
                    );
                }
                ExitCode::SUCCESS
            }
            Err(e) => {
                eprintln!("decisions thresholds list: {e}");
                ExitCode::from(2)
            }
        },
    }
}

fn print_replay(s: &replay::ReplaySummary) {
    let pct = |x: Option<f64>| x.map_or("n/a".to_string(), |v| format!("{:.1}%", v * 100.0));
    println!(
        "replayed {} of {} matching decisions against `{}` ({} without a debug state, {} errors)",
        s.asked,
        s.matched,
        s.provider,
        s.matched - s.with_state,
        s.errors
    );
    if s.refused > 0 {
        println!(
            "{} skipped: their project's decision settings do not allow sending them to `{}` now",
            s.refused, s.provider
        );
    }
    println!("agreement with the original answers: {}", pct(s.agreement));
    println!(
        "labelled accuracy over {}: original {}, replay {}",
        s.labelled,
        pct(s.original_accuracy),
        pct(s.replay_accuracy)
    );
    if let Some(path) = &s.results {
        println!("results: {}", path.display());
    }
    println!("evaluation only: nothing was written to any run journal");
}
