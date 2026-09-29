//! Run provenance on the working tree (C7): which commits a node made.
//!
//! On a git tree the engine reads `HEAD` before an agent_task or script node
//! runs and again when it finishes. When `HEAD` moved forward on the branch
//! it started on, the node's commits are journaled as one
//! `artifacts_committed` event, which the run report lists; any other move
//! (a branch switch, a reset, a rewrite) records nothing. A node that asked a
//! question keeps its starting `HEAD` for the answer round, and a member of
//! a concurrent batch that did not run alone is not tracked (its siblings
//! move the same `HEAD`).
//! Anything that is not a git work tree with a commit (no git, no repository,
//! an unborn branch) is skipped silently, so such a run journals exactly what
//! it did before. The other direction, from a commit to its run, is the
//! `Apb-Run: <run id>` commit trailer a playbook's agents write, with the id
//! from `APB_RUN_ID` or `{{run.id}}`.

use std::path::{Path, PathBuf};
use std::process::Command;

use apb_core::schema::{NodeKind, Playbook};

use super::journal::Journal;
use crate::error::EngineError;
use crate::event::{CommittedArtifact, EventPayload};

/// At most this many commits are listed per event; the rest is counted.
pub(crate) const MAX_LISTED_COMMITS: usize = 50;

/// `git -C dir <args>` stdout, `None` on any failure (no git, not a
/// repository, a non-zero exit). The repository's config-driven programs
/// are switched off (`core.fsmonitor`, `log.showSignature` and through it
/// `gpg.program`), so reading `HEAD` never runs a program the tree names.
fn git(dir: &Path, args: &[&str]) -> Option<String> {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args([
            "-c",
            "core.fsmonitor=false",
            "-c",
            "log.showSignature=false",
        ])
        .args(args)
        .output()
        .ok()?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).into_owned())
}

/// The commit `HEAD` names in `dir`, `None` outside a git work tree or on an
/// unborn branch.
pub(crate) fn head(dir: &Path) -> Option<String> {
    let sha = git(dir, &["rev-parse", "--verify", "--quiet", "HEAD^{commit}"])?;
    let sha = sha.trim();
    (!sha.is_empty()).then(|| sha.to_string())
}

/// Whether `before` is an ancestor of (or equal to) `after`: the node's
/// commits were added on top of where it started.
fn is_ancestor(dir: &Path, before: &str, after: &str) -> bool {
    git(dir, &["merge-base", "--is-ancestor", before, after]).is_some()
}

/// The commits reachable from `after` but not from `before`, newest first,
/// and how many there are in all. Callers check [`is_ancestor`] first: the
/// range means "what the node committed" only when `after` descends from
/// `before`.
pub(crate) fn commits_between(
    dir: &Path,
    before: &str,
    after: &str,
) -> (Vec<CommittedArtifact>, usize) {
    let range = format!("{before}..{after}");
    let total = git(dir, &["rev-list", "--count", &range])
        .and_then(|s| s.trim().parse::<usize>().ok())
        .unwrap_or(0);
    let limit = format!("--max-count={MAX_LISTED_COMMITS}");
    let listed = git(
        dir,
        &[
            "log",
            "--no-show-signature",
            &limit,
            "--format=%H%x09%s",
            &range,
        ],
    )
    .map(|text| {
        text.lines()
            .filter_map(|line| {
                let (sha, subject) = line.split_once('\t')?;
                Some(CommittedArtifact {
                    sha: sha.to_string(),
                    subject: subject.to_string(),
                })
            })
            .collect::<Vec<_>>()
    })
    .unwrap_or_default();
    (listed, total)
}

/// The directory whose `HEAD` a node's commits land in: the node's own
/// `workdir` when it renders, else the run's working tree.
fn node_tree(
    playbook: &Playbook,
    node_id: &str,
    workdir: &Path,
    render: impl FnOnce(&str) -> Result<String, EngineError>,
) -> PathBuf {
    match super::node_workdir::resolve(playbook, node_id, workdir, render) {
        Ok(Ok(dir)) => dir,
        _ => workdir.to_path_buf(),
    }
}

/// The `HEAD` seen before one node execution, for [`Tracker::finish`].
pub(crate) struct Tracker {
    node: String,
    dir: PathBuf,
    before: String,
    /// The branch `HEAD` named then (`refs/heads/...`), empty when detached.
    branch: String,
}

/// The branch `HEAD` names in `dir`, empty when it is detached.
fn branch(dir: &Path) -> String {
    git(dir, &["symbolic-ref", "-q", "HEAD"])
        .map(|s| s.trim().to_string())
        .unwrap_or_default()
}

/// Where a node that asked a question keeps the `HEAD` its execution
/// started from until the answer round finishes it.
fn parked_path(run_dir: &Path, node_id: &str) -> PathBuf {
    run_dir.join("provenance").join(format!("{node_id}.before"))
}

impl Tracker {
    /// Reads `HEAD` before an agent_task or script node runs; `None` for
    /// every other kind and wherever there is no commit to read. The answer
    /// round of a node that asked a question starts from the `HEAD` its
    /// first round saw (kept by [`Tracker::park`]), so the commits made
    /// before the question are recorded too.
    pub(crate) fn start(
        playbook: &Playbook,
        run_dir: &Path,
        node_id: &str,
        workdir: &Path,
        render: impl FnOnce(&str) -> Result<String, EngineError>,
    ) -> Option<Self> {
        let mut t = Self::start_here(playbook, node_id, workdir, render)?;
        if let Ok(parked) = std::fs::read_to_string(parked_path(run_dir, node_id)) {
            let mut lines = parked.lines();
            let sha = lines.next().unwrap_or_default().trim();
            if !sha.is_empty() && sha.chars().all(|c| c.is_ascii_hexdigit()) {
                t.before = sha.to_string();
                t.branch = lines.next().unwrap_or_default().trim().to_string();
            }
        }
        Some(t)
    }

    /// Keeps `before` for the answer round of a node that just asked a
    /// question. Best effort: without it the answer round records only
    /// its own commits.
    pub(crate) fn park(self, run_dir: &Path) {
        let path = parked_path(run_dir, &self.node);
        if let Some(parent) = path.parent()
            && std::fs::create_dir_all(parent).is_ok()
        {
            let text = format!("{}\n{}\n", self.before, self.branch);
            let _ = apb_core::fsutil::atomic_write(&path, text.as_bytes());
        }
    }

    fn start_here(
        playbook: &Playbook,
        node_id: &str,
        workdir: &Path,
        render: impl FnOnce(&str) -> Result<String, EngineError>,
    ) -> Option<Self> {
        let node = playbook.node(node_id)?;
        if !matches!(
            node.kind,
            NodeKind::AgentTask { .. } | NodeKind::Script { .. }
        ) {
            return None;
        }
        let dir = node_tree(playbook, node_id, workdir, render);
        let before = head(&dir)?;
        let branch = branch(&dir);
        Some(Self {
            node: node_id.to_string(),
            dir,
            before,
            branch,
        })
    }

    /// Journals `artifacts_committed` when `HEAD` moved. Best effort: a git
    /// failure after the node ran records nothing rather than failing it.
    pub(crate) fn finish(self, run_dir: &Path, journal: &Journal) -> Result<(), EngineError> {
        let _ = std::fs::remove_file(parked_path(run_dir, &self.node));
        let Some(after) = head(&self.dir) else {
            return Ok(());
        };
        // A HEAD that moved anywhere but forward on the branch it started on
        // (a branch switch, a reset, a rewrite) says nothing about what the
        // node committed: the range would list another branch's history.
        // Nothing is recorded.
        if after == self.before
            || branch(&self.dir) != self.branch
            || !is_ancestor(&self.dir, &self.before, &after)
        {
            return Ok(());
        }
        let (commits, total) = commits_between(&self.dir, &self.before, &after);
        journal.append(EventPayload::ArtifactsCommitted {
            node: self.node,
            before: self.before,
            after,
            omitted: total.saturating_sub(commits.len()),
            commits,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn git_ok(dir: &Path, args: &[&str]) {
        let st = Command::new("git")
            .arg("-C")
            .arg(dir)
            .args([
                "-c",
                "user.name=t",
                "-c",
                "user.email=t@example.invalid",
                "-c",
                "commit.gpgsign=false",
            ])
            .args(args)
            .status()
            .unwrap();
        assert!(st.success(), "git {args:?}");
    }

    #[test]
    fn head_is_none_outside_git_and_on_an_unborn_branch() {
        let tmp = tempfile::tempdir().unwrap();
        assert_eq!(head(tmp.path()), None);
        git_ok(tmp.path(), &["init", "-q"]);
        assert_eq!(head(tmp.path()), None);
    }

    #[test]
    fn commits_between_lists_the_new_commits_newest_first() {
        let tmp = tempfile::tempdir().unwrap();
        let d = tmp.path();
        git_ok(d, &["init", "-q"]);
        git_ok(d, &["commit", "-q", "--allow-empty", "-m", "base"]);
        let before = head(d).unwrap();
        git_ok(d, &["commit", "-q", "--allow-empty", "-m", "one"]);
        git_ok(
            d,
            &["commit", "-q", "--allow-empty", "-m", "two\n\nApb-Run: r1"],
        );
        let after = head(d).unwrap();
        assert!(is_ancestor(d, &before, &after));
        let (commits, total) = commits_between(d, &before, &after);
        assert_eq!(total, 2);
        let subjects: Vec<&str> = commits.iter().map(|c| c.subject.as_str()).collect();
        assert_eq!(subjects, ["two", "one"]);
        assert_eq!(commits[0].sha, after);
        // A reset back to an older commit is not a forward move.
        assert!(!is_ancestor(d, &after, &before));
    }
}
