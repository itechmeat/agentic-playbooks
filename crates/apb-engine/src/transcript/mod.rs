//! Reading what an attempt left behind (issue #67 item 10): the raw output
//! streams in `attempts/<stem>/` and, for claude, the CLI's own session
//! transcript, as records a grader can cite.
//!
//! Each [`Source`] is one stream of one attempt, with its completeness
//! taken from the attempt's `attempt_finished` (`output_truncated` names
//! every cut stream, `transcript_incomplete` a final record that did not
//! read whole) and from the record format itself (a torn last line). A
//! [`View`] is the source parsed into numbered records: claude's session
//! through [`claude`], every other stream line by line through [`plain`].
//! Other agents keep their sessions in their own stores, so their views
//! carry no tool calls; a reader that needs them gets "not enough data".

pub mod claude;
pub mod plain;
mod record;

pub use record::{Record, ToolUse};

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use crate::capture::{STREAM_LOG, STREAM_SESSION_COPY, STREAM_STDERR, STREAM_STDOUT};
use crate::event::{Event, EventPayload};

/// One stream of an attempt's transcript directory.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Stream {
    /// claude's own session transcript (`session.jsonl`).
    Session,
    Stdout,
    Stderr,
}

impl Stream {
    pub fn as_str(self) -> &'static str {
        match self {
            Stream::Session => "session",
            Stream::Stdout => "stdout",
            Stream::Stderr => "stderr",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "session" => Some(Stream::Session),
            "stdout" => Some(Stream::Stdout),
            "stderr" => Some(Stream::Stderr),
            _ => None,
        }
    }

    fn file(self) -> &'static str {
        match self {
            Stream::Session => "session.jsonl",
            Stream::Stdout => "stdout.log",
            Stream::Stderr => "stderr.log",
        }
    }

    /// The `output_truncated` stream names that cut this stream.
    fn loss_names(self) -> &'static [&'static str] {
        match self {
            Stream::Session => &[STREAM_SESSION_COPY],
            Stream::Stdout => &[STREAM_STDOUT, STREAM_LOG],
            Stream::Stderr => &[STREAM_STDERR],
        }
    }
}

/// One stream of one attempt.
#[derive(Debug, Clone, PartialEq)]
pub struct Source {
    pub node: String,
    pub attempt: u32,
    /// The seq of the attempt's `attempt_started`.
    pub attempt_seq: u64,
    /// The attempt's directory name under `attempts/`.
    pub stem: String,
    pub agent: String,
    /// The directory the attempt's process ran in, as journaled.
    pub workdir: Option<String>,
    pub stream: Stream,
    pub path: PathBuf,
    /// Nothing of the stream was cut and the attempt finished.
    pub complete: bool,
    /// apb keeps a transcript directory for the attempt; an untracked one
    /// ([`UNTRACKED`]) is never read.
    pub tracked: bool,
}

impl Source {
    /// The view id and citation prefix: `t:<stem>/<stream>`.
    pub fn id(&self) -> String {
        format!("t:{}/{}", self.stem, self.stream.as_str())
    }

    /// Whether the agent keeps a structured session apb can read.
    pub fn structured(&self) -> bool {
        self.stream == Stream::Session
    }
}

/// A source parsed into records.
#[derive(Debug, Clone, PartialEq)]
pub struct View {
    pub source: Source,
    pub records: Vec<Record>,
    /// The source was complete and every record parsed.
    pub complete: bool,
}

impl View {
    pub fn id(&self) -> String {
        self.source.id()
    }

    /// The citation of record `index`.
    pub fn record_ref(&self, index: u64) -> String {
        format!("{}#{index}", self.id())
    }
}

/// The stem prefix of an attempt apb keeps no readable transcript
/// directory for (none journaled, or one outside `attempts/`): its view is
/// empty and incomplete, so nothing is claimed about what it did. It
/// starts with `@`, which no playbook node id does, so it never meets a
/// real stem (`<node>-<attempt>`).
pub const UNTRACKED: &str = "@untracked";

/// What the journal says about one attempt's output.
struct AttemptFacts {
    node: String,
    attempt: u32,
    seq: u64,
    agent: String,
    workdir: Option<String>,
    stem: String,
    finished: bool,
    cut: Vec<String>,
    incomplete: bool,
    /// The attempt has no transcript directory apb can read.
    untracked: bool,
}

/// Every agent attempt, in journal order, one per transcript directory: a
/// re-run that restarts the attempt counter reuses (and clears) its stem,
/// so only the latest attempt of a stem is kept, as only its output is on
/// disk.
fn attempts(events: &[Event]) -> Vec<AttemptFacts> {
    let mut out: Vec<AttemptFacts> = Vec::new();
    let mut open: BTreeMap<(String, u32, Option<String>), usize> = BTreeMap::new();
    for e in events {
        match &e.payload {
            EventPayload::AttemptStarted {
                node,
                attempt,
                agent,
                workdir,
                transcript,
                item,
                ..
            } => {
                let readable = transcript
                    .as_deref()
                    .and_then(|dir| dir.strip_prefix("attempts/"))
                    .filter(|stem| apb_core::registry::is_safe_segment(stem));
                let (stem, untracked) = match readable {
                    Some(stem) => (stem.to_string(), false),
                    None => (format!("{UNTRACKED}-{}", e.seq), true),
                };
                open.insert((node.clone(), *attempt, item.clone()), out.len());
                out.push(AttemptFacts {
                    node: node.clone(),
                    attempt: *attempt,
                    seq: e.seq,
                    agent: agent.clone(),
                    workdir: workdir.clone(),
                    stem,
                    finished: false,
                    cut: Vec::new(),
                    incomplete: false,
                    untracked,
                });
            }
            EventPayload::AttemptFinished {
                node,
                attempt,
                output_truncated,
                transcript_incomplete,
                item,
                ..
            } => {
                if let Some(i) = open.remove(&(node.clone(), *attempt, item.clone())) {
                    let a = &mut out[i];
                    a.finished = true;
                    a.cut = output_truncated.iter().map(|l| l.stream.clone()).collect();
                    a.incomplete = *transcript_incomplete;
                }
            }
            _ => {}
        }
    }
    let mut seen = std::collections::BTreeSet::new();
    let mut latest: Vec<AttemptFacts> = out
        .into_iter()
        .rev()
        .filter(|a| seen.insert((a.stem.clone(), a.untracked)))
        .collect();
    latest.reverse();
    latest
}

/// Every transcript source of the run at `run_dir`: per attempt its
/// stdout and stderr, and claude's session.
pub fn sources(run_dir: &Path, events: &[Event]) -> Vec<Source> {
    let mut out = Vec::new();
    for a in attempts(events) {
        let claude = apb_core::detect::canonical_agent_id(&a.agent) == "claude";
        let streams: &[Stream] = if a.untracked {
            &[Stream::Stdout]
        } else if claude {
            &[Stream::Session, Stream::Stdout, Stream::Stderr]
        } else {
            &[Stream::Stdout, Stream::Stderr]
        };
        for &stream in streams {
            let path = run_dir.join("attempts").join(&a.stem).join(stream.file());
            let cut = stream
                .loss_names()
                .iter()
                .any(|n| a.cut.iter().any(|c| c == n));
            let final_record = stream != Stream::Stderr && a.incomplete;
            out.push(Source {
                node: a.node.clone(),
                attempt: a.attempt,
                attempt_seq: a.seq,
                stem: a.stem.clone(),
                agent: a.agent.clone(),
                workdir: a.workdir.clone(),
                stream,
                complete: a.finished && !cut && !final_record && !a.untracked && path.is_file(),
                tracked: !a.untracked,
                path,
            });
        }
    }
    out
}

/// Reads one source into its view. A file that does not read is an empty,
/// incomplete view.
pub fn read(source: &Source) -> View {
    let raw = if !source.tracked {
        Err(std::io::ErrorKind::NotFound.into())
    } else {
        std::fs::read(&source.path)
    };
    let Ok(raw) = raw else {
        return View {
            source: source.clone(),
            records: Vec::new(),
            complete: false,
        };
    };
    let text = String::from_utf8_lossy(&raw);
    let (records, parsed_whole) = match source.stream {
        Stream::Session => claude::parse(&text),
        Stream::Stdout | Stream::Stderr => (plain::parse(&text), true),
    };
    View {
        complete: source.complete && parsed_whole,
        source: source.clone(),
        records,
    }
}
