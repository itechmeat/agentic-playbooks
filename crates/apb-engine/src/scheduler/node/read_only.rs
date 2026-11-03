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

use apb_core::fingerprint::{TreeChange, TreeLimit, TreeLimits, TreeState};

use super::super::journal::Journal;
use crate::error::EngineError;
use crate::event::{EventPayload, ReadOnlyChange, ReadOnlyCheck, ReadOnlyLimit, ReadOnlyTier};

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

/// The tree's state before one detected attempt.
pub(crate) struct Before {
    node: String,
    attempt: u32,
    root: PathBuf,
    state: TreeState,
}

impl Before {
    /// Takes the state of `root` now.
    pub(crate) fn take(node: &str, attempt: u32, root: &Path) -> Before {
        Before {
            node: node.to_string(),
            attempt,
            root: root.to_path_buf(),
            state: TreeState::take(root, limits()),
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
    /// and, on a change, journals `read_only_violated`. The failure reason
    /// when anything changed. `project` is the workspace whose scrubber
    /// cleans the paths.
    pub(crate) fn finish(
        self,
        run_dir: &Path,
        project: &Path,
        journal: &Journal,
    ) -> Result<Option<String>, EngineError> {
        let after = TreeState::take(&self.root, limits());
        let after_partial = after.partial().map(limit);
        let changes = self.state.changes(&after);
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
        journal.append(EventPayload::ReadOnlyViolated {
            node: self.node.clone(),
            attempt: self.attempt,
            tier,
            changes: listed.clone(),
            truncated,
            evidence,
        })?;
        let first = &listed[0];
        let more = match changes.len() {
            1 => String::new(),
            n => format!(" (and {} more)", n - 1),
        };
        Ok(Some(format!(
            "{READ_ONLY_DIRTY}: node `{}` changed the tree it may only read: {} {}{more}",
            self.node, first.path, first.change
        )))
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

/// The directory name of node `node`'s evidence: the id itself when it is a
/// safe path segment, else `node-<digest>` of it, so an id like `../x` never
/// leads the evidence out of the run's `read_only/` directory.
fn evidence_stem(node: &str) -> String {
    if apb_core::registry::is_safe_segment(node) {
        node.to_string()
    } else {
        let digest = apb_core::content::sha256_hex(node.as_bytes());
        format!("node-{}", &digest[..16])
    }
}

/// One more than the highest `<n>.json` in `dir`.
fn next_ordinal(dir: &Path) -> u64 {
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
