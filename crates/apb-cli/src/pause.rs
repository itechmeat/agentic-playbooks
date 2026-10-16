//! `apb pause` and the pause lines the run commands print (issue #200).

use std::path::Path;
use std::process::ExitCode;

use apb_engine::pause::{PauseInfo, PauseOutcome, PauseRequest};

use crate::util::{QUESTION_TEXT_MAX, sanitize_for_terminal};

/// `apb pause <run_id>`: pauses the run as an operator. A run a live driver
/// owns gets the pause posted to it (drain applies at the next boundary, now
/// right away); a run nothing drives is journaled paused on the spot.
pub(crate) fn pause_cmd(
    root: &Path,
    run_id: &str,
    now: bool,
    reason: Option<String>,
    until: Option<&str>,
    keep_lock: bool,
) -> ExitCode {
    let req = match PauseRequest::operator(
        Some(if now { "now" } else { "drain" }),
        reason,
        until,
        keep_lock,
        apb_core::clock::now_ms_u64(),
    ) {
        Ok(r) => r,
        Err(msg) => {
            eprintln!("pause failed: {msg}");
            return ExitCode::from(2);
        }
    };
    let until_ms = req.until;
    match apb_engine::pause::request_pause(root, run_id, &req) {
        Ok(PauseOutcome::Posted { .. }) => {
            let how = if now {
                "running nodes are being interrupted and run again on resume"
            } else {
                "no new node starts, running nodes finish first"
            };
            println!("pausing {run_id}: {how}");
            print_resume_hint(run_id, until_ms);
            ExitCode::SUCCESS
        }
        Ok(PauseOutcome::Journaled { .. }) => {
            println!("paused {run_id}: no driver was running, the run is now paused");
            print_resume_hint(run_id, until_ms);
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("pause failed: {e}");
            ExitCode::from(2)
        }
    }
}

fn print_resume_hint(run_id: &str, until_ms: Option<u64>) {
    match until_ms {
        Some(ms) => println!(
            "it resumes on its own at {}; `apb resume {run_id}` continues it earlier",
            apb_core::dismiss::iso_utc(ms)
        ),
        None => println!("`apb resume {run_id}` continues it"),
    }
}

/// The pause of `run_id` as a note for `apb resume`, when the run is paused
/// with an `until` still ahead: resuming now is an early resume.
pub(crate) fn early_resume_note(root: &Path, run_id: &str) -> Option<String> {
    if !apb_core::registry::is_safe_segment(run_id) {
        return None;
    }
    let run_dir = root.join(".apb/runs").join(run_id);
    let view = apb_engine::run_view::RunView::load(&run_dir, run_id).ok()?;
    let p = view.pause()?;
    let until = p.until?;
    (!p.pausing && until > apb_core::clock::now_ms_u64()).then(|| {
        format!(
            "resuming early: the pause was set until {}",
            apb_core::dismiss::iso_utc(until)
        )
    })
}

/// The pause as one terminal-safe marker, for the `apb runs` list.
pub(crate) fn marker(p: &PauseInfo) -> String {
    let mut safe = p.clone();
    safe.reason = sanitize_for_terminal(&p.reason, QUESTION_TEXT_MAX);
    safe.line()
}

/// The pause block of `apb runs <id>` and `apb wait`.
pub(crate) fn print_block(p: &PauseInfo) {
    println!("  {}", marker(p));
}
