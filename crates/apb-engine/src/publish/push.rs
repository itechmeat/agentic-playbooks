//! Runs the networked git calls of the publish step: the push, and the
//! read-only listings the plan and a reconcile take. Each one runs through
//! [`apb_core::git::transport`] from a fresh shadow git directory (one per
//! plan, push or reconcile, removed afterwards), with
//! stdin from null, in its own session (so ssh cannot prompt on the
//! operator's terminal), bounded by its own timeout, and killed as a group
//! only by an abort: a pause never interrupts a push.

use std::path::Path;
use std::process::Command;
use std::sync::atomic::AtomicBool;
use std::time::Duration;

use apb_core::git::transport::{self, Shadow};
use apb_core::publish::LIST_TIMEOUT_SECONDS;
use apb_core::publish::porcelain::{self, Outcome};
use apb_core::publish::refname;
use apb_core::publish::url::PushUrl;

use crate::proc::Captured;

/// The longest detail a settlement keeps from git's stderr.
const DETAIL_CAP: usize = 300;

/// What a remote branch points at.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RemoteHead {
    At(String),
    Absent,
    /// The remote could not be listed.
    Unreachable,
}

/// Where a networked call runs: a fresh shadow over the tree's objects, the
/// URL, the connector variables it never sees, the abort flag that may kill
/// it.
pub struct Remote<'a> {
    shadow: Shadow,
    url: &'a PushUrl,
    scrub: &'a [String],
    abort: Option<&'a AtomicBool>,
}

impl<'a> Remote<'a> {
    /// A new shadow under `run_dir` for the repository whose common
    /// directory is `common_dir`; an error when it cannot be made.
    pub fn open(
        run_dir: &Path,
        url: &'a PushUrl,
        common_dir: &Path,
        scrub: &'a [String],
        abort: Option<&'a AtomicBool>,
    ) -> std::io::Result<Remote<'a>> {
        Ok(Remote {
            shadow: Shadow::create(run_dir, common_dir)?,
            url,
            scrub,
            abort,
        })
    }

    /// Runs `cmd` bounded by `timeout`.
    fn run(&self, mut cmd: Command, timeout: Duration) -> std::io::Result<Captured> {
        for name in self.scrub {
            cmd.env_remove(name);
        }
        crate::proc::run_capture_in(cmd, Some(timeout), self.abort, true)
    }

    /// Runs a read-only listing bounded by [`LIST_TIMEOUT_SECONDS`].
    fn list(&self, cmd: Command) -> Option<Captured> {
        self.run(cmd, Duration::from_secs(LIST_TIMEOUT_SECONDS))
            .ok()
    }

    /// The remote's branch heads, `(sha, refs/heads/x)`, or only `branch`;
    /// `None` when the remote could not be listed.
    pub fn heads(&self, branch: Option<&str>) -> Option<Vec<(String, String)>> {
        let out = self.list(transport::list_heads(&self.shadow, self.url, branch))?;
        out.status
            .is_some_and(|s| s.success())
            .then(|| transport::parse_heads(&out.stdout))
    }

    /// What the remote's `branch` points at.
    pub fn head_of(&self, branch: &str) -> RemoteHead {
        let full = refname::full(branch);
        match self.heads(Some(branch)) {
            None => RemoteHead::Unreachable,
            Some(heads) => heads
                .into_iter()
                .find(|(_, r)| *r == full)
                .map_or(RemoteHead::Absent, |(s, _)| RemoteHead::At(s)),
        }
    }

    /// Whether the operator's config rewrites the URL (`insteadOf` or
    /// `pushInsteadOf`) to another address; `None` when git could not say.
    pub fn rewritten(&self) -> Option<bool> {
        let out = self.list(transport::rewrite_rules(&self.shadow, self.url))?;
        match out.status.and_then(|s| s.code()) {
            Some(0) => Some(transport::rewrites(&out.stdout, self.url.as_str())),
            // No rule at all.
            Some(1) if out.stdout.is_empty() => Some(false),
            _ => None,
        }
    }

    /// Pushes `after` to `branch` under the exact lease `remote_before`,
    /// bounded by `timeout`, and reads what git reported. A spawn failure
    /// after the intent was recorded is unknown too: apb cannot prove git
    /// never started.
    pub fn push(
        &self,
        branch: &str,
        after: &str,
        remote_before: Option<&str>,
        timeout: Duration,
        scrubber: &apb_core::scrub::Scrubber,
    ) -> Outcome {
        let cmd = transport::push(&self.shadow, self.url, branch, after, remote_before);
        let out = match self.run(cmd, timeout) {
            Ok(out) => out,
            Err(_) => return Outcome::Unknown("git could not be run".into()),
        };
        let killed = out.status.is_none();
        let exit = out.status.and_then(|s| s.code());
        let tail = detail(&out.stderr, scrubber);
        match porcelain::classify(&out.stdout, exit, killed, &refname::full(branch)) {
            Outcome::Confirmed => Outcome::Confirmed,
            Outcome::NotSent(s) => Outcome::NotSent(join(&s, &tail)),
            Outcome::Unknown(s) => Outcome::Unknown(join(&s, &tail)),
        }
    }
}

/// The scrubbed tail of git's stderr, at most [`DETAIL_CAP`] characters.
fn detail(stderr: &str, scrubber: &apb_core::scrub::Scrubber) -> String {
    let clean = scrubber.scrub(stderr.trim());
    let chars: Vec<char> = clean.chars().collect();
    let start = chars.len().saturating_sub(DETAIL_CAP);
    chars[start..].iter().collect()
}

fn join(summary: &str, tail: &str) -> String {
    let joined = match (summary.is_empty(), tail.is_empty()) {
        (_, true) => summary.to_string(),
        (true, false) => tail.to_string(),
        (false, false) => format!("{summary}; {tail}"),
    };
    joined.chars().take(DETAIL_CAP).collect()
}
