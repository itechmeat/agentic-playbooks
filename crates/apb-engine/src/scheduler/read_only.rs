//! A `read_only` node (agent_task or script) must leave the tree it runs
//! in as it found it. Two tiers, decided per attempt:
//!
//! - **enforced**: the attempt runs in the sandbox, which binds the tree
//!   and every git directory of it read-only (`sandbox::view`); a write
//!   fails at the OS level and nothing is checked afterwards, so the node
//!   may share a batch with a writing sibling.
//! - **detected**: no sandbox (a script, `isolation: none`, `best_effort`
//!   without one, a host step). Right before the attempt the engine takes
//!   the tree's state ([`apb_core::fingerprint::TreeState`]: listed files by
//!   digest and executable bit, symlinks by target, `HEAD`, refs, the
//!   staged index and the git control files, every `.gitignore` and the
//!   root's `.apb` definitions; other ignored paths, `.git` and `.apb` left
//!   out) and compares
//!   it after the process ended. A change journals `read_only_violated` and
//!   fails the node at once with `read_only_dirty: ...`: no retry, no
//!   retry advice, no deferral, no profile or host fallback, no timeout
//!   continuation, no infrastructure or quota path. A limit that stopped
//!   the before-state short makes the tier `detected_partial`, never "no
//!   change"; so does one that stops the after-state short (the attempt
//!   grew the tree past it): the violation's tier and the evidence say so.
//!
//! Every detected check writes its evidence to the private
//! `runs/<id>/read_only/<node>/<n>.json` (`node-<digest>` for an id that is
//! not a safe path segment): changed paths by name (scrubbed,
//! at most [`MAX_LISTED`] plus a count) and `git diff --stat` lines of the
//! changed tracked files. No file content is read into it or stored.
//!
//! The snapshot root is the node's own directory (`node_dir`), the tree
//! the node is asked to read, not the attempt's working directory: a
//! `best_effort` attempt without a sandbox runs in a scratch directory and
//! could still write the tree by path.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use apb_core::fingerprint::{TreeChange, TreeLimit, TreeLimits, TreeState};

use crate::error::EngineError;
use crate::event::{EventPayload, ReadOnlyChange, ReadOnlyCheck, ReadOnlyLimit, ReadOnlyTier};
use crate::scheduler::journal::Journal;

/// The failure prefix of a read-only node that changed its tree.
pub const READ_ONLY_DIRTY: &str = "read_only_dirty";

/// At most this many changed paths are listed, in the journal and in the
/// evidence; the rest are counted.
pub const MAX_LISTED: usize = 200;

/// Test-only lowering of the entry and byte limits (a debug build only).
const MAX_ENTRIES_ENV: &str = "APB_READ_ONLY_MAX_ENTRIES";
const MAX_BYTES_ENV: &str = "APB_READ_ONLY_MAX_BYTES";

/// The limits of one before-state: the product's, or a debug build's
/// lowered test values.
fn limits() -> TreeLimits {
    let knob = |key: &str| {
        crate::env_duration::TEST_KNOBS
            .then(|| std::env::var(key).ok())
            .flatten()
            .and_then(|v| v.trim().parse::<u64>().ok())
    };
    let d = TreeLimits::default();
    TreeLimits {
        entries: knob(MAX_ENTRIES_ENV).unwrap_or(d.entries),
        bytes: knob(MAX_BYTES_ENV).unwrap_or(d.bytes),
    }
}

/// The record of an enforced attempt.
pub(crate) fn enforced() -> ReadOnlyCheck {
    ReadOnlyCheck {
        tier: ReadOnlyTier::Enforced,
        entries: 0,
        bytes: 0,
        partial: None,
    }
}

/// Journals `read_only_checked` for `attempt` of `node` when the node is
/// read-only (`check` is its record), right after the attempt's
/// `attempt_started` (a script: right before it runs).
pub(crate) fn journal_check(
    journal: &Journal,
    node: &str,
    attempt: u32,
    check: Option<&ReadOnlyCheck>,
) -> Result<(), EngineError> {
    let Some(check) = check else {
        return Ok(());
    };
    journal.append(EventPayload::ReadOnlyChecked {
        node: node.to_string(),
        attempt,
        check: check.clone(),
    })
}

/// A scenario run's read-only check (the tree states are not taken, no
/// `read_only_checked` is journaled): the paths the scenario scripts as
/// changed fail the attempt exactly as a detected change does, journaled
/// `read_only_violated` (tier `detected`). `None` when it scripts none.
pub(crate) fn scripted(
    journal: &Journal,
    node: &str,
    attempt: u32,
    dirty: &[String],
) -> Result<Option<String>, EngineError> {
    let Some(first) = dirty.first() else {
        return Ok(None);
    };
    let changes: Vec<ReadOnlyChange> = dirty
        .iter()
        .take(MAX_LISTED)
        .map(|p| ReadOnlyChange {
            path: p.clone(),
            change: apb_core::fingerprint::ChangeKind::Modified
                .as_str()
                .to_string(),
        })
        .collect();
    journal.append(EventPayload::ReadOnlyViolated {
        node: node.to_string(),
        attempt,
        tier: ReadOnlyTier::Detected,
        changes,
        truncated: dirty.len().saturating_sub(MAX_LISTED) as u64,
        evidence: None,
        concurrent_with: Vec::new(),
    })?;
    let more = match dirty.len() {
        1 => String::new(),
        n => format!(" (and {} more)", n - 1),
    };
    Ok(Some(format!(
        "{READ_ONLY_DIRTY}: node `{node}` changed the tree it may only read: {first} {}{more}",
        apb_core::fingerprint::ChangeKind::Modified.as_str()
    )))
}

/// The message of a read-only node on a list (V138 refuses it; this is the
/// engine's own refusal when the validator was skipped).
pub(crate) fn list_refusal(node: &str) -> String {
    format!(
        "read_only: node `{node}` is read_only on a list, whose items apb does not check; remove read_only or the list"
    )
}

/// Who ran beside whom in one concurrent batch (spec loops-and-graphs D1):
/// a monotonic tick counter and, per attempt, the node, its start tick and
/// an optional end tick. One instance per logical batch, carried by
/// `ExecOptions.batch_peers`; a sequential execution carries none, and no
/// global state exists. A detected read-only check names the members whose
/// attempts overlapped its own, so a change cannot be attributed to one of
/// them; an enforced member keeps its interval too, so a detected peer can
/// name it.
pub(crate) struct BatchIntervals {
    book: Mutex<Book>,
}

#[derive(Default)]
struct Book {
    tick: u64,
    intervals: Vec<Interval>,
}

struct Interval {
    node: String,
    start: u64,
    end: Option<u64>,
}

impl BatchIntervals {
    pub(crate) fn new() -> BatchIntervals {
        BatchIntervals {
            book: Mutex::new(Book::default()),
        }
    }

    /// Opens the interval of one attempt and returns the tick it started
    /// with. Called only by [`BatchIntervals::open`], whose guard closes it.
    pub(crate) fn start(&self, node: &str) -> u64 {
        let mut book = self.book.lock().unwrap_or_else(|e| e.into_inner());
        book.tick += 1;
        let start = book.tick;
        book.intervals.push(Interval {
            node: node.to_string(),
            start,
            end: None,
        });
        start
    }

    /// Opens the interval of one attempt as a guard that closes it on drop,
    /// so every exit of the attempt (an error return, an engine pause)
    /// closes it, not only the path that reaches [`OpenInterval::close`].
    /// Called immediately before `Before::take`.
    pub(crate) fn open(self: &Arc<Self>, node: &str) -> OpenInterval {
        OpenInterval {
            book: Arc::clone(self),
            node: node.to_string(),
            start: self.start(node),
            closed: false,
        }
    }

    /// Closes the interval opened with `start_tick`: by
    /// [`OpenInterval::close`] after the attempt's after state has been
    /// taken, or by the guard's drop on any other exit. An interval still
    /// open (an attempt under way) counts as running to infinity, so it can
    /// only list a member, never hide one.
    pub(crate) fn finish(&self, node: &str, start_tick: u64) {
        let mut book = self.book.lock().unwrap_or_else(|e| e.into_inner());
        book.tick += 1;
        let end = book.tick;
        if let Some(open) = book
            .intervals
            .iter_mut()
            .find(|i| i.node == node && i.start == start_tick)
        {
            open.end = Some(end);
        }
    }

    /// The node ids of every member whose attempt overlapped the attempt
    /// that opened with `start_tick`, sorted and deduplicated. Two
    /// intervals overlap when each starts before the other ends; an end
    /// not set yet is open (infinity). Self is excluded.
    pub(crate) fn overlapping(&self, node: &str, start_tick: u64) -> Vec<String> {
        let book = self.book.lock().unwrap_or_else(|e| e.into_inner());
        let end = |i: &Interval| i.end.unwrap_or(u64::MAX);
        let Some(mine) = book
            .intervals
            .iter()
            .find(|i| i.node == node && i.start == start_tick)
        else {
            return Vec::new();
        };
        let mut peers: Vec<String> = book
            .intervals
            .iter()
            .filter(|other| other.node != node)
            .filter(|other| mine.start < end(other) && other.start < end(mine))
            .map(|other| other.node.clone())
            .collect();
        peers.sort();
        peers.dedup();
        peers
    }
}

/// One attempt's open interval in a batch's book ([`BatchIntervals::open`]):
/// closed once, by [`OpenInterval::close`] after the after state was taken,
/// or on drop when the attempt left by any other way.
pub(crate) struct OpenInterval {
    book: Arc<BatchIntervals>,
    node: String,
    start: u64,
    closed: bool,
}

impl OpenInterval {
    /// The book and the start tick, as `Before::take` carries them.
    pub(crate) fn peers(&self) -> (Arc<BatchIntervals>, u64) {
        (Arc::clone(&self.book), self.start)
    }

    /// Closes the interval now; a second call does nothing.
    pub(crate) fn close(&mut self) {
        if !self.closed {
            self.closed = true;
            self.book.finish(&self.node, self.start);
        }
    }
}

impl Drop for OpenInterval {
    fn drop(&mut self) {
        self.close();
    }
}

/// The tree's state before one detected attempt.
pub(crate) struct Before {
    pub(crate) node: String,
    pub(crate) attempt: u32,
    root: PathBuf,
    state: TreeState,
    /// The batch peers of the execution and the tick this attempt opened
    /// with, when it runs in a concurrent batch (`ExecOptions.batch_peers`);
    /// none on the sequential arm.
    peers: Option<(Arc<BatchIntervals>, u64)>,
}

impl Before {
    /// Takes the state of `root` now. `peers` carries the batch interval
    /// book and the tick the attempt opened with (see
    /// [`BatchIntervals::start`]); `None` outside a batch.
    pub(crate) fn take(
        node: &str,
        attempt: u32,
        root: &Path,
        peers: Option<(Arc<BatchIntervals>, u64)>,
    ) -> Before {
        Before {
            node: node.to_string(),
            attempt,
            root: root.to_path_buf(),
            state: TreeState::take(root, limits()),
            peers,
        }
    }

    /// The `attempt_started.read_only` record.
    pub(crate) fn record(&self) -> ReadOnlyCheck {
        ReadOnlyCheck {
            tier: self.tier(),
            entries: self.state.entries(),
            bytes: self.state.bytes(),
            partial: self.state.partial().map(limit),
        }
    }

    fn tier(&self) -> ReadOnlyTier {
        tier(self.state.partial().is_some())
    }

    /// Compares the tree with the state taken before, writes the evidence
    /// and, on a change, journals `read_only_violated` with the batch
    /// members whose attempts overlapped this one. The failure reason when
    /// anything changed. `project` is the workspace whose scrubber cleans
    /// the paths.
    /// The after state against the before state, with the limit the after
    /// walk hit (`None` when it read the tree whole). Split from `finish`
    /// so the write-scope check and the proof reuse the one comparison.
    pub(crate) fn compare(&self) -> Comparison {
        let after = TreeState::take(&self.root, limits());
        let after_partial = after.partial().map(limit);
        let changes = self.state.changes(&after);
        Comparison {
            changes,
            before_partial: self.state.partial().map(limit),
            after_partial,
        }
    }

    pub(crate) fn finish(
        self,
        run_dir: &Path,
        project: &Path,
        journal: &Journal,
    ) -> Result<Option<String>, EngineError> {
        let Comparison {
            changes,
            after_partial,
            ..
        } = self.compare();
        let scrubber = (!changes.is_empty())
            .then(|| apb_core::scrub::Scrubber::for_workspace(project, Vec::new()));
        let listed: Vec<ReadOnlyChange> = changes
            .iter()
            .take(MAX_LISTED)
            .map(|c| ReadOnlyChange {
                path: scrubber
                    .as_ref()
                    .map_or_else(|| c.path.clone(), |s| s.scrub(&c.path)),
                change: c.kind.as_str().to_string(),
            })
            .collect();
        let truncated = changes.len().saturating_sub(MAX_LISTED) as u64;
        let diff_stat = self.diff_stat(&changes, scrubber.as_ref());
        let evidence =
            self.write_evidence(run_dir, after_partial, &listed, truncated, &diff_stat)?;
        if changes.is_empty() {
            return Ok(None);
        }
        let tier = tier(self.state.partial().is_some() || after_partial.is_some());
        let concurrent_with = self
            .peers
            .as_ref()
            .map_or_else(Vec::new, |(p, tick)| p.overlapping(&self.node, *tick));
        journal.append(EventPayload::ReadOnlyViolated {
            node: self.node.clone(),
            attempt: self.attempt,
            tier,
            changes: listed.clone(),
            truncated,
            evidence,
            concurrent_with: concurrent_with.clone(),
        })?;
        let first = &listed[0];
        let more = match changes.len() {
            1 => String::new(),
            n => format!(" (and {} more)", n - 1),
        };
        Ok(Some(format!(
            "{READ_ONLY_DIRTY}: {} {} {}{more}",
            self.beside(&concurrent_with),
            first.path,
            first.change
        )))
    }

    /// The clause that says who moved the tree: an empty list is today's
    /// wording; a batch member names the peers it ran beside, whose change
    /// cannot be attributed to one of them.
    fn beside(&self, concurrent_with: &[String]) -> String {
        match concurrent_with {
            [] => format!("node `{}` changed the tree it may only read:", self.node),
            [one] => format!(
                "the tree changed while node `{}` ran beside `{one}`; the change cannot be attributed to one of them:",
                self.node
            ),
            many => format!(
                "the tree changed while node `{}` ran beside {}; the change cannot be attributed to one of them:",
                self.node,
                many.iter()
                    .map(|p| format!("`{p}`"))
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        }
    }

    /// `git diff --stat` against the commit `HEAD` named before, limited
    /// to the changed paths: names and line counts only.
    fn diff_stat(
        &self,
        changes: &[TreeChange],
        scrubber: Option<&apb_core::scrub::Scrubber>,
    ) -> Vec<String> {
        let Some(commit) = self.state.head_commit() else {
            return Vec::new();
        };
        let paths: Vec<String> = changes
            .iter()
            .filter(|c| {
                matches!(
                    c.kind,
                    apb_core::fingerprint::ChangeKind::Modified
                        | apb_core::fingerprint::ChangeKind::Deleted
                        | apb_core::fingerprint::ChangeKind::Mode
                )
            })
            .take(MAX_LISTED)
            .map(|c| format!(":(literal){}", c.path))
            .collect();
        if paths.is_empty() {
            return Vec::new();
        }
        let Ok(repo) = apb_core::git::Repo::locate(&self.root) else {
            return Vec::new();
        };
        let mut cmd = repo.diff(&self.root);
        cmd.args(["--stat=200", commit, "--"]).args(&paths);
        let Ok(out) = cmd
            .stdin(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .output()
        else {
            return Vec::new();
        };
        if !out.status.success() {
            return Vec::new();
        }
        String::from_utf8_lossy(&out.stdout)
            .lines()
            .take(MAX_LISTED + 1)
            .map(|l| scrubber.map_or_else(|| l.to_string(), |s| s.scrub(l)))
            .collect()
    }

    /// Writes `read_only/<node>/<n>.json` (the next free `n`), 0600 under
    /// the run's private directory; the run-relative path. The check is
    /// partial when either state was (`partial` the before-state's limit,
    /// else the after-state's; `after_partial` the after-state's).
    fn write_evidence(
        &self,
        run_dir: &Path,
        after_partial: Option<ReadOnlyLimit>,
        changes: &[ReadOnlyChange],
        truncated: u64,
        diff_stat: &[String],
    ) -> Result<Option<String>, EngineError> {
        let stem = evidence_stem(&self.node);
        let dir = run_dir.join("read_only").join(&stem);
        apb_core::fsutil::create_dir_under(run_dir, &dir)?;
        let n = next_ordinal(&dir);
        let record = self.record();
        let body = serde_json::json!({
            "node": self.node,
            "attempt": self.attempt,
            "tier": tier(record.partial.is_some() || after_partial.is_some()),
            "entries": record.entries,
            "bytes": record.bytes,
            "partial": record.partial.or(after_partial),
            "after_partial": after_partial,
            "clean": changes.is_empty(),
            "changes": changes,
            "truncated": truncated,
            "diff_stat": diff_stat,
        });
        let bytes = serde_json::to_vec_pretty(&body)
            .map_err(|e| EngineError::Invalid(format!("read_only evidence: {e}")))?;
        let path = dir.join(format!("{n}.json"));
        apb_core::fsutil::atomic_write_private_under(run_dir, &path, &bytes)?;
        Ok(Some(format!("read_only/{stem}/{n}.json")))
    }
}

/// The tier of a detected check, partial when a limit stopped either state
/// short.
fn tier(partial: bool) -> ReadOnlyTier {
    if partial {
        ReadOnlyTier::DetectedPartial
    } else {
        ReadOnlyTier::Detected
    }
}

/// The journal's name of a limit a state hit.
fn limit(l: TreeLimit) -> ReadOnlyLimit {
    match l {
        TreeLimit::Entries => ReadOnlyLimit::Entries,
        TreeLimit::Bytes => ReadOnlyLimit::Bytes,
    }
}

/// What [`Before::compare`] answers: the changed paths and the limit, if
/// any, that cut the before or the after walk short.
pub(crate) struct Comparison {
    pub changes: Vec<TreeChange>,
    pub before_partial: Option<ReadOnlyLimit>,
    pub after_partial: Option<ReadOnlyLimit>,
}

impl Comparison {
    /// The limit that cut either walk short (the before state's first):
    /// a comparison over a tree neither walk saw whole shows nothing.
    pub(crate) fn partial(&self) -> Option<ReadOnlyLimit> {
        self.before_partial.or(self.after_partial)
    }
}

/// The words a message names a limit with.
pub(crate) fn limit_words(limit: ReadOnlyLimit) -> &'static str {
    match limit {
        ReadOnlyLimit::Entries => "the entries limit",
        ReadOnlyLimit::Bytes => "the bytes limit",
    }
}

/// The directory name of node `node`'s evidence: the id itself when it is a
/// safe path segment, else `node-<digest>` of it, so an id like `../x` never
/// leads the evidence out of the run's `read_only/` directory.
pub(super) fn evidence_stem(node: &str) -> String {
    if apb_core::registry::is_safe_segment(node) {
        node.to_string()
    } else {
        let digest = apb_core::content::sha256_hex(node.as_bytes());
        format!("node-{}", &digest[..16])
    }
}

/// One more than the highest `<n>.json` in `dir`.
pub(super) fn next_ordinal(dir: &Path) -> u64 {
    std::fs::read_dir(dir)
        .map(|rd| {
            rd.flatten()
                .filter_map(|e| {
                    e.file_name()
                        .to_str()?
                        .strip_suffix(".json")?
                        .parse::<u64>()
                        .ok()
                })
                .max()
                .unwrap_or(0)
        })
        .unwrap_or(0)
        + 1
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    /// An attempt that leaves without reaching its close (an error return,
    /// an engine pause) still closes its interval when the guard drops: a
    /// member that starts afterwards does not list it.
    #[test]
    fn a_dropped_interval_is_closed_and_never_listed_by_a_later_member() {
        let book = Arc::new(BatchIntervals::new());
        drop(book.open("gone"));
        let later = book.open("later");
        assert!(book.overlapping("later", later.peers().1).is_empty());
    }

    /// An explicit close and the drop after it close once: a member that
    /// starts after the close does not list it, and one that ran beside it
    /// does.
    #[test]
    fn a_closed_interval_is_listed_only_by_members_that_overlapped_it() {
        let book = Arc::new(BatchIntervals::new());
        let mut first = book.open("first");
        let beside = book.open("beside");
        first.close();
        drop(first);
        assert_eq!(book.overlapping("beside", beside.peers().1), ["first"]);
        let after = book.open("after");
        assert_eq!(book.overlapping("after", after.peers().1), ["beside"]);
    }
}
