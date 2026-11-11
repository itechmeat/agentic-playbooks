//! `apb health`: the routine health of watched playbooks (D2). Without a
//! subcommand, the state and a read-only evaluation; `check` evaluates now
//! (notices go out) and exits 3 on hold; `clear`, `mute` and `unmute` are
//! a person's, refused inside a run step.

use std::path::Path;
use std::process::ExitCode;

use apb_engine::health::{self, HealthView};
use clap::{Args, Subcommand};

use crate::util::{print_json, sanitize_for_terminal};

/// How long a printed field may be.
const FIELD_MAX: usize = 300;

#[derive(Args)]
#[command(args_conflicts_with_subcommands = true)]
pub(crate) struct HealthArgs {
    #[command(subcommand)]
    action: Option<HealthAction>,
    /// `<id>` or `global:<id>`; without it, every watched playbook of the
    /// project
    playbook: Option<String>,
    /// Print JSON
    #[arg(long)]
    json: bool,
}

#[derive(Subcommand)]
enum HealthAction {
    /// Evaluate now (notices go out); exit 0 when a start would be
    /// admitted, 3 when a playbook is on hold
    Check {
        playbook: Option<String>,
        #[arg(long)]
        json: bool,
    },
    /// A person's: drop the hold and every trip, start the count and
    /// overdue over
    Clear { playbook: String },
    /// A person's: send no notice until a time (2026-11-12T08:00:00Z) or
    /// for a duration (2h)
    Mute {
        playbook: String,
        #[arg(long, value_name = "TIME|DURATION")]
        until: String,
    },
    /// A person's: end a mute
    Unmute { playbook: String },
}

fn now() -> u64 {
    apb_core::clock::now_ms_u64()
}

fn name(v: &HealthView) -> String {
    health::reference(
        apb_core::scope::Scope::parse(&v.scope).unwrap_or(apb_core::scope::Scope::Project),
        &v.playbook,
    )
}

/// `<ref>  <status>  <rules>`; each field read from a file is cleaned
/// for the terminal on its own.
fn line(v: &HealthView) -> String {
    let clean = |s: &str| sanitize_for_terminal(s, FIELD_MAX);
    let rules: Vec<String> = v
        .rules
        .iter()
        .map(|r| {
            if r.tripped {
                format!("{} tripped ({})", r.rule, clean(&r.detail))
            } else {
                format!("{} ok", r.rule)
            }
        })
        .collect();
    let mut out = format!("{}  {}", clean(&name(v)), v.status);
    if !rules.is_empty() {
        out.push_str(&format!("  {}", rules.join(", ")));
    }
    if let Some(h) = &v.hold {
        out.push_str(&format!(
            "; on hold: {}; {}",
            clean(&h.detail),
            clean(&h.clear)
        ));
    }
    if let Some(m) = &v.muted_until {
        out.push_str(&format!("; muted until {m}"));
    }
    out
}

fn unwatched(r: &str) -> HealthView {
    let (scope, id) = health::parse_reference(r);
    HealthView {
        playbook: id.to_string(),
        scope: scope.as_str().to_string(),
        status: "unwatched".into(),
        rules: Vec::new(),
        hold: None,
        muted_until: None,
        last_notice: None,
    }
}

fn show(root: &Path, playbook: Option<&str>, json: bool) -> ExitCode {
    let views = match playbook {
        Some(r) => {
            let (scope, id) = health::parse_reference(r);
            match health::view(root, scope, id, now()) {
                Ok(v) => vec![v.unwrap_or_else(|| unwatched(r))],
                Err(e) => {
                    eprintln!("apb: {e}");
                    return ExitCode::from(2);
                }
            }
        }
        None => health::all_views(root, now()),
    };
    if json {
        print_json(&serde_json::to_value(&views).unwrap_or_default());
    } else if views.is_empty() {
        println!("no playbook here has a health block");
    } else {
        for v in &views {
            println!("{}", line(v));
        }
    }
    ExitCode::SUCCESS
}

fn check(root: &Path, playbook: Option<&str>, json: bool) -> ExitCode {
    let results: Vec<(String, Result<HealthView, apb_engine::error::EngineError>)> = match playbook
    {
        Some(r) => {
            let (scope, id) = health::parse_reference(r);
            vec![(r.to_string(), health::evaluate(root, scope, id, &now))]
        }
        None => health::sweep(root, &now),
    };
    let mut held = false;
    let mut views = Vec::new();
    for (r, res) in results {
        match res {
            Ok(v) => {
                for t in health::tripped_rules(&v) {
                    eprintln!("health: {}: {}", r, sanitize_for_terminal(&t, FIELD_MAX));
                }
                held |= v.status == "held";
                views.push(v);
            }
            Err(e) => {
                eprintln!("apb: health of `{r}`: {e}");
                return ExitCode::from(2);
            }
        }
    }
    if json {
        print_json(&serde_json::to_value(&views).unwrap_or_default());
    } else {
        for v in &views {
            println!("{}", line(v));
        }
    }
    if held {
        ExitCode::from(3)
    } else {
        ExitCode::SUCCESS
    }
}

/// The refusal of a person's command inside a run step.
fn refused(what: &str) -> Option<ExitCode> {
    let why = apb_core::step_env::StepCaller::from_env().refusal(what)?;
    eprintln!("{why}");
    Some(ExitCode::from(2))
}

pub(crate) fn health_cmd(root: &Path, args: HealthArgs) -> ExitCode {
    match args.action {
        None => show(root, args.playbook.as_deref(), args.json),
        Some(HealthAction::Check { playbook, json }) => check(root, playbook.as_deref(), json),
        Some(HealthAction::Clear { playbook }) => {
            if let Some(code) = refused("apb health clear") {
                return code;
            }
            let (scope, id) = health::parse_reference(&playbook);
            match health::clear(root, scope, id, now()) {
                Ok(()) => {
                    println!(
                        "cleared {playbook}: the hold and every trip are gone, the count starts over"
                    );
                    ExitCode::SUCCESS
                }
                Err(e) => {
                    eprintln!("apb: {e}");
                    ExitCode::from(2)
                }
            }
        }
        Some(HealthAction::Mute { playbook, until }) => {
            if let Some(code) = refused("apb health mute") {
                return code;
            }
            let until_ms = match apb_core::duration::parse_until(&until, now()) {
                Ok(ms) => ms,
                Err(e) => {
                    eprintln!("apb: --until: {e}");
                    return ExitCode::from(2);
                }
            };
            let (scope, id) = health::parse_reference(&playbook);
            match health::mute(root, scope, id, Some(until_ms), now()) {
                Ok(()) => {
                    println!(
                        "muted {playbook} until {}",
                        apb_core::clock::iso_utc(until_ms)
                    );
                    ExitCode::SUCCESS
                }
                Err(e) => {
                    eprintln!("apb: {e}");
                    ExitCode::from(2)
                }
            }
        }
        Some(HealthAction::Unmute { playbook }) => {
            if let Some(code) = refused("apb health unmute") {
                return code;
            }
            let (scope, id) = health::parse_reference(&playbook);
            match health::mute(root, scope, id, None, now()) {
                Ok(()) => {
                    println!("unmuted {playbook}");
                    ExitCode::SUCCESS
                }
                Err(e) => {
                    eprintln!("apb: {e}");
                    ExitCode::from(2)
                }
            }
        }
    }
}
