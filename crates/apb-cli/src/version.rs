//! `apb version`: a playbook's versions, which one is current, and moving
//! `current` by hand through the one move every surface uses
//! (`apb_core::versioning::current`).
//!
//! - `apb version list <playbook>`: the versions, with the current one and
//!   the candidate on trial marked;
//! - `apb version use <playbook> <version> [--expect <v>] [--reason <text>]`:
//!   a person makes `version` current; with `--expect`, only while
//!   `current` still names that version. Refused inside a run step;
//! - `apb version history <playbook>`: the moves, newest first, and whether
//!   the record has a gap.
//!
//! `--scope project` (the default) or `--scope global` picks the store.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use apb_core::candidate::read_candidate;
use apb_core::registry::{is_safe_segment, list_versions};
use apb_core::versioning::current::{
    EntryKind, History, HistoryEntry, MoveCurrent, Moved, MovedBy, REASON_MAX, Surface, history,
    move_current,
};
use clap::{Args, Subcommand, ValueEnum};

use crate::util::{QUESTION_TEXT_MAX, StepCaller, print_json, sanitize_for_terminal};

#[derive(Args)]
pub(crate) struct VersionArgs {
    #[command(subcommand)]
    cmd: VersionCmd,
}

#[derive(Subcommand)]
enum VersionCmd {
    /// List the versions of a playbook, marking the current one and the
    /// candidate on trial
    List {
        playbook: String,
        #[arg(long, value_enum, default_value_t = Scope::Project)]
        scope: Scope,
        /// Machine-readable output
        #[arg(long)]
        json: bool,
    },
    /// Make a version the current one
    Use {
        playbook: String,
        version: String,
        /// Move only while `current` still names this version
        #[arg(long, value_name = "VERSION")]
        expect: Option<String>,
        /// Why, kept in the history (scrubbed, at most 256 characters)
        #[arg(long)]
        reason: Option<String>,
        #[arg(long, value_enum, default_value_t = Scope::Project)]
        scope: Scope,
        /// Machine-readable output
        #[arg(long)]
        json: bool,
    },
    /// Show the moves of `current`, newest first
    History {
        playbook: String,
        /// How many moves to show
        #[arg(long, default_value_t = 20)]
        limit: usize,
        #[arg(long, value_enum, default_value_t = Scope::Project)]
        scope: Scope,
        /// Machine-readable output
        #[arg(long)]
        json: bool,
    },
}

/// The definition store a command reads or moves.
#[derive(Clone, Copy, ValueEnum)]
enum Scope {
    Project,
    Global,
}

pub(crate) fn version_cmd(root: &Path, args: VersionArgs) -> ExitCode {
    match args.cmd {
        VersionCmd::List {
            playbook,
            scope,
            json,
        } => with_dir(root, &playbook, scope, json, |dir| {
            list(dir, &playbook, json)
        }),
        VersionCmd::Use {
            playbook,
            version,
            expect,
            reason,
            scope,
            json,
        } => {
            // The CLI runs in its caller's process: from inside a run step
            // the move is refused.
            if let Some(why) = StepCaller::from_env().refusal("moving `current`") {
                return refuse(json, "run_step", &why);
            }
            with_dir(root, &playbook, scope, json, |dir| {
                let mut req = MoveCurrent::new(
                    &version,
                    MovedBy::Person {
                        surface: Surface::Cli,
                    },
                );
                req.expected_from = expect.as_deref().map(Some);
                req.reason = reason.as_deref();
                use_version(dir, &playbook, &req, json)
            })
        }
        VersionCmd::History {
            playbook,
            limit,
            scope,
            json,
        } => with_dir(root, &playbook, scope, json, |dir| {
            show_history(dir, &playbook, limit, json)
        }),
    }
}

/// Runs `f` on the directory of `playbook` in `scope`, or says why there is
/// none.
fn with_dir(
    root: &Path,
    playbook: &str,
    scope: Scope,
    json: bool,
    f: impl FnOnce(&Path) -> ExitCode,
) -> ExitCode {
    let parent: Option<PathBuf> = match scope {
        Scope::Project => Some(root.join(".apb")),
        Scope::Global => apb_core::store::global_playbooks_parent(),
    };
    let dir = parent
        .filter(|_| is_safe_segment(playbook))
        .map(|p| p.join("playbooks").join(playbook))
        .filter(|d| d.is_dir());
    match dir {
        Some(dir) => f(&dir),
        None => refuse(
            json,
            "not_found",
            &format!("playbook `{playbook}` not found"),
        ),
    }
}

/// A refused command: `{ error, message }` on stdout under `--json`, the
/// message on stderr otherwise.
fn refuse(json: bool, code: &str, message: &str) -> ExitCode {
    if json {
        print_json(&serde_json::json!({ "error": code, "message": message }));
    } else {
        eprintln!("{message}");
    }
    ExitCode::from(2)
}

fn list(dir: &Path, playbook: &str, json: bool) -> ExitCode {
    let versions = match list_versions(dir) {
        Ok(v) => v,
        Err(e) => return refuse(json, "io", &format!("version list failed: {e}")),
    };
    let current = apb_core::versioning::read_current_pointer(dir);
    let candidate = read_candidate(dir);
    if json {
        print_json(&serde_json::json!({
            "id": playbook,
            "versions": versions,
            "current": current,
            "candidate": candidate,
        }));
        return ExitCode::SUCCESS;
    }
    for v in versions.iter().rev() {
        let mut marks = Vec::new();
        if current.as_deref() == Some(v.as_str()) {
            marks.push("current");
        }
        if candidate.as_deref() == Some(v.as_str()) {
            marks.push("candidate");
        }
        if marks.is_empty() {
            println!("{v}");
        } else {
            println!("{v}  ({})", marks.join(", "));
        }
    }
    ExitCode::SUCCESS
}

fn use_version(dir: &Path, playbook: &str, req: &MoveCurrent<'_>, json: bool) -> ExitCode {
    match move_current(dir, req) {
        Ok(moved) => {
            if json {
                print_json(&serde_json::json!(moved));
            } else {
                print_moved(playbook, &moved);
            }
            ExitCode::SUCCESS
        }
        Err(e) if json => refuse(json, e.code(), &e.to_string()),
        Err(e) => refuse(json, e.code(), &format!("version use failed: {e}")),
    }
}

fn print_moved(playbook: &str, moved: &Moved) {
    println!(
        "{playbook}: current moved from {} to {}",
        moved.from.as_deref().unwrap_or("unset"),
        moved.to
    );
    if let Some(c) = &moved.superseded_candidate {
        println!("  the trial of candidate {c} ended (superseded)");
    }
    if moved.awaiting_approval {
        println!(
            "  {} waits for an approval of its imported scripts: runs stay refused until a person approves it (apb trust approve)",
            moved.to
        );
    }
    if moved.audit_pending {
        println!("  its history line is not written yet: the next change of current writes it");
    }
}

fn show_history(dir: &Path, playbook: &str, limit: usize, json: bool) -> ExitCode {
    let h = match history(dir, limit) {
        Ok(h) => h,
        Err(e) if json => return refuse(json, e.code(), &e.to_string()),
        Err(e) => return refuse(json, e.code(), &format!("version history failed: {e}")),
    };
    if json {
        print_json(&serde_json::json!({ "id": playbook, "history": h }));
        return ExitCode::SUCCESS;
    }
    print_history(playbook, &h);
    ExitCode::SUCCESS
}

fn print_history(playbook: &str, h: &History) {
    if h.entries.is_empty() {
        println!("{playbook}: no recorded moves of current");
    }
    for e in &h.entries {
        println!("{}", history_line(e));
    }
    if h.incomplete {
        println!(
            "incomplete: the record has a gap (a line lost or edited, or current moved by an older apb or by hand)"
        );
    }
    if h.pending {
        println!("pending: a move was interrupted and is finished by the next change of current");
    }
}

/// One history line as the terminal shows it. Every field comes from a
/// file a person may have edited by hand, so each goes through the
/// terminal sanitizer.
fn history_line(e: &HistoryEntry) -> String {
    let safe = |s: &str| sanitize_for_terminal(s, QUESTION_TEXT_MAX);
    let version = |v: Option<&String>| v.map_or_else(|| "unset".to_string(), |v| safe(v));
    let at = apb_core::clock::iso_utc(e.at_ms);
    if e.kind == EntryKind::Aborted {
        return format!("{at}  aborted move {}", safe(&e.change_id));
    }
    let mut line = format!(
        "{at}  {} -> {}",
        version(e.from.as_ref()),
        version(e.to.as_ref())
    );
    if let Some(by) = &e.by {
        line.push_str(&format!("  by {}", safe(&by.to_string())));
    }
    if let Some(reason) = &e.reason {
        line.push_str(&format!(
            "  ({})",
            sanitize_for_terminal(reason, REASON_MAX)
        ));
    }
    if let Some(c) = &e.superseded_candidate {
        line.push_str(&format!("  [candidate {} superseded]", safe(c)));
    }
    line
}
