//! The last task of a host session as the save check reads it: the tail
//! of the session's history file, parsed by the discovery decoder into
//! argument-free step tokens and the requests a person typed. Only the
//! steps after the last request count (none when the tail holds no
//! request); the whole tail counts for whether a playbook tool was used.

use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

use crate::discover::HistoryAgent;
use crate::discover::session;
use crate::discover::token;

/// The most of a history file the check reads, from its end.
pub const MAX_TAIL_BYTES: u64 = 4 * 1024 * 1024;
/// The most steps one tail yields.
const MAX_STEPS: usize = 20_000;

/// apb's playbook tools: once one was called in the session, the agent
/// already knows about playbooks and the check stays quiet.
pub const PLAYBOOK_TOOLS: [&str; 4] = [
    "playbook_catalog",
    "playbook_run",
    "playbook_capture",
    "suggestion_dismiss",
];

/// The command steps that are apb's own playbook work (`apb run`).
const PLAYBOOK_COMMANDS: [&str; 1] = ["apb:run"];

/// What the check reads of a session.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LastTask {
    /// The steps since the last request, in order (consecutive repeats are
    /// one step).
    pub steps: Vec<String>,
    /// The last request a person typed, when the tail holds one.
    pub request: Option<String>,
    /// A playbook tool was called anywhere in the tail.
    pub playbook_tool_used: bool,
}

/// Whether `token` is a call of one of apb's playbook tools, on any server
/// name the host gave apb.
fn is_playbook_step(token: &str) -> bool {
    if PLAYBOOK_COMMANDS.contains(&token) {
        return true;
    }
    token
        .strip_prefix("mcp:")
        .and_then(|rest| rest.rsplit_once('.'))
        .is_some_and(|(_, tool)| PLAYBOOK_TOOLS.contains(&tool))
}

/// The step kind of `token`: the program of a plain command
/// (`sh:cargo` is `cargo`), else what comes before the colon (`git:commit`
/// is `git`, `edit:.rs` is `edit`).
pub fn kind(token: &str) -> &str {
    match token.split_once(':') {
        Some(("sh", program)) => program,
        Some((kind, _)) => kind,
        None => token,
    }
}

/// The last task of the history `bytes` of `agent`.
pub fn read(agent: HistoryAgent, bytes: &[u8]) -> LastTask {
    let mut skipped = Default::default();
    let parsed = session::parse(agent, bytes, MAX_TAIL_BYTES, MAX_STEPS, &mut skipped);
    let playbook_tool_used = parsed.steps.iter().any(|s| is_playbook_step(&s.token));
    // A tail without a request starts inside a task whose request was cut
    // off: its steps may span several tasks, so none of them count.
    let Some(since) = parsed.requests.last().map(|(line, _)| *line) else {
        return LastTask {
            playbook_tool_used,
            ..LastTask::default()
        };
    };
    LastTask {
        playbook_tool_used,
        steps: parsed
            .steps
            .into_iter()
            .filter(|s| s.line > since)
            .map(|s| s.token)
            .collect(),
        request: parsed.requests.last().map(|(_, t)| t.clone()),
    }
}

/// The last [`MAX_TAIL_BYTES`] of the file at `path`, starting at a whole
/// line. A link or a file that is not a regular file is an error; the open
/// never follows a link and never waits for a FIFO's writer.
pub fn tail(path: &Path) -> std::io::Result<Vec<u8>> {
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    #[cfg(not(unix))]
    if std::fs::symlink_metadata(path)?.file_type().is_symlink() {
        return Err(std::io::Error::other("a link"));
    }
    let mut file = options.open(path)?;
    let meta = file.metadata()?;
    if !meta.is_file() {
        return Err(std::io::Error::other("not a regular file"));
    }
    let start = meta.len().saturating_sub(MAX_TAIL_BYTES);
    file.seek(SeekFrom::Start(start))?;
    let mut bytes = Vec::new();
    file.take(MAX_TAIL_BYTES).read_to_end(&mut bytes)?;
    if start > 0 {
        // The first line was cut: it starts after the first newline.
        let cut = bytes
            .iter()
            .position(|b| *b == b'\n')
            .map_or(bytes.len(), |p| p + 1);
        bytes.drain(..cut);
    }
    Ok(bytes)
}

/// Whether any step of `steps` acts (an edit, a command that changes
/// something, an MCP or web call, a sub-agent), see [`token::is_acting`].
pub fn acts(steps: &[String]) -> bool {
    steps.iter().any(|s| token::is_acting(s))
}
