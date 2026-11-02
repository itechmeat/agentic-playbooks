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
        redactor: &crate::run_redact::RunRedactor,
    ) -> Outcome {
        let cmd = transport::push(&self.shadow, self.url, branch, after, remote_before);
        let out = match self.run(cmd, timeout) {
            Ok(out) => out,
            Err(_) => return Outcome::Unknown("git could not be run".into()),
        };
        let killed = out.status.is_none();
        let exit = out.status.and_then(|s| s.code());
        reported(&out.stdout, &out.stderr, exit, killed, branch, redactor)
    }
}

/// What a finished push of `branch` reported, its text as the ledger keeps
/// it: git's summary of the ref and the tail of its stderr.
fn reported(
    stdout: &str,
    stderr: &str,
    exit: Option<i32>,
    killed: bool,
    branch: &str,
    redactor: &crate::run_redact::RunRedactor,
) -> Outcome {
    let tail = detail(stderr, redactor);
    match porcelain::classify(stdout, exit, killed, &refname::full(branch)) {
        Outcome::Confirmed => Outcome::Confirmed,
        Outcome::NotSent(s) => Outcome::NotSent(join(&scrubbed(&s, redactor), &tail)),
        Outcome::Unknown(s) => Outcome::Unknown(join(&scrubbed(&s, redactor), &tail)),
    }
}

/// `text` git printed, scrubbed whole before anything clips it. Withheld
/// whole while the run tree's secrets cannot all be read: a clipped text
/// could keep part of a value a later, complete scrub no longer
/// recognizes.
fn scrubbed(text: &str, redactor: &crate::run_redact::RunRedactor) -> String {
    match redactor.scrubber() {
        Ok(scrubber) => scrubber.scrub(text.trim()),
        Err(_) => crate::run_redact::WITHHELD.to_string(),
    }
}

/// The scrubbed tail of git's stderr, at most [`DETAIL_CAP`] characters
/// ([`scrubbed`]).
fn detail(stderr: &str, redactor: &crate::run_redact::RunRedactor) -> String {
    let clean = scrubbed(stderr, redactor);
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

#[cfg(test)]
mod tests {
    use super::*;

    /// A remote's refusal reason is git's summary of the ref, not stderr:
    /// it holds the run's secret value as freely as stderr does.
    #[test]
    fn a_refusal_summary_is_scrubbed_or_withheld_before_it_is_clipped() {
        let tmp = tempfile::tempdir().unwrap();
        let run_dir = tmp.path().join("run");
        std::fs::create_dir_all(&run_dir).unwrap();
        // The clip falls inside the value: `orchard` is kept, `-candle` is not.
        let prefix = "[remote rejected] (".len();
        let reason = format!("{}orchard-candle", "x".repeat(DETAIL_CAP - prefix - 7));
        let stdout = format!(
            "To file:///r.git\n!\tabc:refs/heads/apb/out\t[remote rejected] ({reason})\nDone\n"
        );
        let shown = |redactor| match reported(&stdout, "", Some(1), false, "apb/out", &redactor) {
            Outcome::NotSent(text) => text,
            other => panic!("{other:?}"),
        };
        let cfg = crate::run_config::RunConfig {
            params: [("password".to_string(), "orchard-candle".to_string())].into(),
            ..Default::default()
        };
        crate::run_config::write_run_config(&run_dir, &cfg).unwrap();
        let scrubbed = shown(crate::run_redact::RunRedactor::for_run(
            tmp.path(),
            &run_dir,
        ));
        assert!(!scrubbed.contains("orchard"), "{scrubbed}");
        assert!(scrubbed.starts_with("[remote rejected]"), "{scrubbed}");

        std::fs::write(run_dir.join("run.yaml"), "params: [unclosed\n").unwrap();
        let withheld = shown(crate::run_redact::RunRedactor::for_run(
            tmp.path(),
            &run_dir,
        ));
        assert!(!withheld.contains("orchard"), "{withheld}");
        assert!(withheld.contains(crate::run_redact::WITHHELD), "{withheld}");
    }
}
