//! Per-attempt transcripts (issue #67 item 10): every agent attempt keeps
//! what it printed, and for claude the CLI's own session transcript, in
//! `<run>/attempts/<node>-<attempt>/` (`<node>.i-<item digest>-<attempt>/` for a
//! list item), referenced from `attempt_started`.
//! Diagnosing an attempt that died then is a file read instead of a
//! reconstruction from the working tree.

use super::*;

/// The run-relative transcript directory of one attempt, named by its
/// [`crate::exec_key::ExecKey::stem`].
pub(crate) fn relative(stem: &str) -> String {
    format!("attempts/{stem}")
}

/// The file in an attempt's transcript directory holding the agent session
/// id of an attempt that was killed before it could journal its finish (a
/// `--now` pause, issue #200).
pub(crate) const SESSION_ID_FILE: &str = "session.id";

/// Records `session` as the session of the attempt whose transcript
/// directory is `dir`, best effort.
pub(crate) fn record_session(run_dir: &Path, dir: &Path, session: &str) {
    let _ = apb_core::fsutil::atomic_write_under(
        run_dir,
        &dir.join(SESSION_ID_FILE),
        session.as_bytes(),
    );
}

/// Creates (empty) the transcript directory of one attempt and returns it. A
/// directory left by an earlier execution whose attempt counter restarted
/// (a resume, a continue_from) is cleared first, so it only ever holds what
/// this attempt wrote.
pub(crate) fn attempt_dir(run_dir: &Path, stem: &str) -> Result<PathBuf, EngineError> {
    let dir = run_dir.join(relative(stem));
    match std::fs::remove_dir_all(&dir) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(e.into()),
    }
    apb_core::fsutil::create_dir_under(run_dir, &run_dir.join("attempts"))?;
    apb_core::fsutil::create_dir_under(run_dir, &dir)?;
    Ok(dir)
}

/// Copies the agent's own record of session `session` into `dir` as
/// `session.jsonl`, best effort, up to `cap` bytes. Only claude keeps one
/// apb can find: a JSONL file named after the session under `projects/` of
/// its config directory (`CLAUDE_CONFIG_DIR`, else `~/.claude`), with every
/// tool call and result. Other agents keep their sessions in their own
/// stores; their raw output is still in `stdout.log` and `stderr.log`.
/// Returns what the cap kept out of the copy.
pub(crate) fn copy_agent_session(
    agent: &str,
    session: &str,
    dir: &Path,
    cap: u64,
) -> Option<crate::event::OutputLoss> {
    if apb_core::detect::canonical_agent_id(agent) != "claude"
        || !apb_core::registry::is_safe_segment(session)
    {
        return None;
    }
    let projects = std::fs::read_dir(apb_core::agent_actions::claude::projects_dir()?).ok()?;
    let file = format!("{session}.jsonl");
    let source = projects
        .filter_map(|e| e.ok())
        .map(|project| project.path().join(&file))
        .find(|candidate| candidate.is_file())?;
    crate::capture::copy_capped(&source, &dir.join("session.jsonl"), cap)
}
