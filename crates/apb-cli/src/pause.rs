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
            say!("pausing {run_id}: {how}");
            print_resume_hint(run_id, until_ms);
            ExitCode::SUCCESS
        }
        Ok(PauseOutcome::Journaled { .. }) => {
            say!("paused {run_id}: no driver was running, the run is now paused");
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
        Some(ms) => say!(
            "it resumes on its own at {}; `apb resume {run_id}` continues it earlier",
            apb_core::dismiss::iso_utc(ms)
        ),
        None => say!("`apb resume {run_id}` continues it"),
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
        // A park: resuming early only parks the run again, at the same
        // wake time.
        if p.cause == Some(apb_engine::pause::PauseCause::Wait)
            && let Some(node) = apb_engine::pause::cause::parse_wait_reason(&p.reason)
        {
            return format!(
                "the wait on timer `{node}` is due at {}; the run waits again until then",
                apb_core::dismiss::iso_utc(until)
            );
        }
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
    say!("  {}", marker(p));
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A park's early-resume note says the run waits again until the wake
    /// time, not that the pause was set until then.
    #[test]
    fn a_park_resumed_early_says_it_waits_again() {
        let dir = tempfile::tempdir().unwrap();
        let run_dir = dir.path().join(".apb/runs/w-r");
        std::fs::create_dir_all(&run_dir).unwrap();
        std::fs::write(
            run_dir.join("playbook.yaml"),
            "schema: 2\nid: p\nname: p\nversion: 1.0.0\nnodes:\n  - { id: s, type: start }\n  - { id: pause, type: wait, wait_for: { type: timer, seconds: 60 }, timeout_seconds: 120 }\n  - { id: f, type: finish, outcome: success }\nedges:\n  - { from: s, to: pause }\n  - { from: pause, to: f }\n",
        )
        .unwrap();
        let until = apb_core::clock::now_ms_u64() + 3_600_000;
        std::fs::write(
            run_dir.join("events.jsonl"),
            format!(
                "{{\"seq\":0,\"ts\":0,\"type\":\"run_started\",\"playbook\":\"p\",\"version\":\"1.0.0\"}}\n{{\"seq\":1,\"ts\":1,\"type\":\"run_paused\",\"reason\":\"wait (pause)\",\"by\":\"engine\",\"mode\":\"drain\",\"until\":{until},\"keep_lock\":true}}\n"
            ),
        )
        .unwrap();
        let note = early_resume_note(dir.path(), "w-r").unwrap();
        assert!(
            note.starts_with("the wait on timer `pause` is due at ")
                && note.ends_with("; the run waits again until then"),
            "{note}"
        );

        // A person's pause keeps the plain wording.
        std::fs::write(
            run_dir.join("events.jsonl"),
            format!(
                "{{\"seq\":0,\"ts\":0,\"type\":\"run_started\",\"playbook\":\"p\",\"version\":\"1.0.0\"}}\n{{\"seq\":1,\"ts\":1,\"type\":\"run_paused\",\"reason\":\"looking\",\"by\":\"operator\",\"mode\":\"drain\",\"until\":{until},\"keep_lock\":true}}\n"
            ),
        )
        .unwrap();
        let note = early_resume_note(dir.path(), "w-r").unwrap();
        assert!(
            note.starts_with("resuming early: the pause was set until "),
            "{note}"
        );
    }
}
