//! Per-attempt transcripts (issue #67 item 10): every agent attempt keeps
//! what it printed, and for claude the CLI's own session transcript, in
//! `<run>/attempts/<node>-<attempt>/`, referenced from `attempt_started`.
//! Diagnosing an attempt that died then is a file read instead of a
//! reconstruction from the working tree.

use super::*;

/// The run-relative transcript directory of one attempt.
pub(crate) fn relative(node_id: &str, attempt: u32) -> String {
    format!("attempts/{node_id}-{attempt}")
}

/// Creates (empty) the transcript directory of one attempt and returns it. A
/// directory left by an earlier execution whose attempt counter restarted
/// (a resume, a continue_from) is cleared first, so it only ever holds what
/// this attempt wrote.
pub(crate) fn attempt_dir(
    run_dir: &Path,
    node_id: &str,
    attempt: u32,
) -> Result<PathBuf, EngineError> {
    let dir = run_dir.join(relative(node_id, attempt));
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
/// `session.jsonl`, best effort. Only claude keeps one apb can find: a JSONL
/// file named after the session under `projects/` of its config directory
/// (`CLAUDE_CONFIG_DIR`, else `~/.claude`), with every tool call and result.
/// Other agents keep their sessions in their own stores; their raw output is
/// still in `stdout.log` and `stderr.log`.
pub(crate) fn copy_agent_session(agent: &str, session: &str, dir: &Path) {
    if apb_core::detect::canonical_agent_id(agent) != "claude"
        || !apb_core::registry::is_safe_segment(session)
    {
        return;
    }
    let Some(config) = std::env::var_os("CLAUDE_CONFIG_DIR")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".claude")))
    else {
        return;
    };
    let Ok(projects) = std::fs::read_dir(config.join("projects")) else {
        return;
    };
    let file = format!("{session}.jsonl");
    for project in projects.filter_map(|e| e.ok()) {
        let candidate = project.path().join(&file);
        if candidate.is_file() {
            let _ = std::fs::copy(&candidate, dir.join("session.jsonl"));
            return;
        }
    }
}
