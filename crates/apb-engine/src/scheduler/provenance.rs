//! Run provenance on the working tree (C7): which commits a node made.
//!
//! On a git tree the engine reads `HEAD` before an agent_task or script node
//! runs and again when it finishes. When `HEAD` moved, the node's commits are
//! journaled as one `artifacts_committed` event, which the run report lists.
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
/// repository, a non-zero exit).
fn git(dir: &Path, args: &[&str]) -> Option<String> {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
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

/// The commits reachable from `after` but not from `before`, newest first,
/// and how many there are in all. A rewritten history (no path from
/// `before`) lists what `after` adds over their merge base, or `after`
/// alone when there is none.
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
    let listed = git(dir, &["log", &limit, "--format=%H%x09%s", &range])
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
    if listed.is_empty() && total == 0 {
        // No commit of `after` is new over `before` (a reset to an older
        // commit): name the commit HEAD now points at.
        let subject = git(dir, &["log", "-1", "--format=%s", after])
            .map(|s| s.trim().to_string())
            .unwrap_or_default();
        return (
            vec![CommittedArtifact {
                sha: after.to_string(),
                subject,
            }],
            1,
        );
    }
    (listed, total.max(1))
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
}

impl Tracker {
    /// Reads `HEAD` before an agent_task or script node runs; `None` for
    /// every other kind and wherever there is no commit to read.
    pub(crate) fn start(
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
        Some(Self {
            node: node_id.to_string(),
            dir,
            before,
        })
    }

    /// Journals `artifacts_committed` when `HEAD` moved. Best effort: a git
    /// failure after the node ran records nothing rather than failing it.
    pub(crate) fn finish(self, journal: &Journal) -> Result<(), EngineError> {
        let Some(after) = head(&self.dir) else {
            return Ok(());
        };
        if after == self.before {
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
        let (commits, total) = commits_between(d, &before, &after);
        assert_eq!(total, 2);
        let subjects: Vec<&str> = commits.iter().map(|c| c.subject.as_str()).collect();
        assert_eq!(subjects, ["two", "one"]);
        assert_eq!(commits[0].sha, after);
        // A reset back to an older commit names where HEAD points now.
        let (back, n) = commits_between(d, &after, &before);
        assert_eq!(n, 1);
        assert_eq!(back[0].sha, before);
        assert_eq!(back[0].subject, "base");
    }
}
