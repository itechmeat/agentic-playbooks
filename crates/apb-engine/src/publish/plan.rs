//! The read-only plan of a publish node: exactly what a push would carry,
//! where, and over what the remote holds now. It writes nothing; a plan
//! that cannot be offered refuses with a code ([`Refusal`]).
//!
//! The plan holds when every one of these does: the source node recorded
//! its commits; the tree's repository is the pinned one; the push URL is
//! the pinned one and the operator's config rewrites it nowhere else;
//! `HEAD` is the source's `after`, on the branch it committed on; the
//! branch is a valid name; the remote can be listed; and the push is a
//! fast-forward of what the remote holds.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use apb_core::git::Repo;
use apb_core::publish::url::{PushUrl, Scheme};
use apb_core::publish::{PlanFacts, Refusal, refname};

use super::push::Remote;
use crate::event::{Event, EventPayload, PublishCommit};
use crate::scheduler::provenance;

/// What the plan needs from the drive.
pub struct PlanInput<'a> {
    pub node: &'a str,
    pub from: &'a str,
    pub remote: &'a str,
    pub events: &'a [Event],
    /// The source node's tree (its `workdir`, else the run's tree).
    pub tree: &'a Path,
    /// The rendered `branch` template, `None` when the node sets none.
    pub branch: Option<String>,
    /// The run's directory: each networked call makes its fresh shadow
    /// below it.
    pub run_dir: &'a Path,
    /// The paths a node of the run may write besides the tree (resolved
    /// `writable` entries): a `file` remote there is refused.
    pub writable: &'a [PathBuf],
    pub scrub: &'a [String],
    pub abort: Option<&'a std::sync::atomic::AtomicBool>,
}

/// A plan a person can consent to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Plan {
    pub url: PushUrl,
    pub common_dir: String,
    pub from_seq: u64,
    pub branch: String,
    pub local_branch: String,
    pub after: String,
    pub remote_before: Option<String>,
    pub commits: Vec<PublishCommit>,
    pub omitted: usize,
    /// The commits of the run the push carries, listed or omitted.
    pub this_run: usize,
    pub outside_push_suspected: bool,
    pub digest: String,
    pub nonce: String,
}

impl Plan {
    /// The `publish_planned` record of this plan.
    pub fn event(&self, node: &str, from: &str) -> EventPayload {
        EventPayload::PublishPlanned {
            node: node.to_string(),
            from: from.to_string(),
            from_seq: self.from_seq,
            branch: self.branch.clone(),
            local_branch: self.local_branch.clone(),
            after: self.after.clone(),
            remote_before: self.remote_before.clone(),
            url: self.url.as_str().to_string(),
            commits: self.commits.clone(),
            omitted: self.omitted,
            this_run: self.this_run,
            outside_push_suspected: self.outside_push_suspected,
            plan_digest: self.digest.clone(),
            nonce: self.nonce.clone(),
        }
    }

    /// Where the plan's push goes, from a fresh shadow.
    pub fn remote<'a>(&'a self, input: &'a PlanInput<'a>) -> std::io::Result<Remote<'a>> {
        Remote::open(
            input.run_dir,
            &self.url,
            Path::new(&self.common_dir),
            input.scrub,
            input.abort,
        )
    }
}

/// What planning found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Planned {
    Ready(Box<Plan>),
    /// No commit of the run to publish, and `HEAD` is already on the remote.
    Nothing,
    /// The remote branch already points at the commit.
    AlreadyOnRemote {
        after: String,
    },
    Refused {
        why: Refusal,
        detail: Option<String>,
    },
}

/// A refusal with the detail that names what was found.
struct Refused(Refusal, Option<String>);

impl From<Refusal> for Refused {
    fn from(r: Refusal) -> Self {
        Refused(r, None)
    }
}

/// The source's latest recorded commit within the publish node's scope:
/// `(seq, after, branch)`.
fn source_commit(events: &[Event], node: &str, from: &str) -> Option<(u64, String, String)> {
    let floor =
        crate::connector::intent::effect_scope_floor(events, crate::exec_key::ExecKey::node(node));
    events.iter().rev().find_map(|e| match &e.payload {
        EventPayload::ArtifactsCommitted {
            node: n,
            after,
            branch,
            ..
        } if n == from && e.seq >= floor => {
            Some((e.seq, after.clone(), branch.clone().unwrap_or_default()))
        }
        _ => None,
    })
}

/// The commit this run started from on `local_branch` in this tree: the
/// earliest recorded `before` on it that the tree holds and that is an
/// ancestor of `after` (another tree's branch of the same name is passed
/// over). `Some(None)` when the run's first commit there was made on an
/// unborn branch of this tree (the commit it recorded is in this tree's
/// history up to `after`), so every commit up to `after` is the run's.
/// `Ok(None)` when no event names a base here; `HistoryUnreadable` when git
/// cannot tell whether a candidate is in this tree's history, which stops
/// the search rather than passing over to a later event.
fn run_base(
    repo: &Repo,
    tree: &Path,
    events: &[Event],
    local_branch: &str,
    after: &str,
) -> Result<Option<Option<String>>, Refusal> {
    let in_history = |sha: &str| -> Result<bool, Refusal> {
        let known =
            known_commits(repo, tree, &[sha.to_string()]).ok_or(Refusal::HistoryUnreadable)?;
        Ok(!known.is_empty() && provenance::is_ancestor(tree, sha, after))
    };
    for e in events {
        let EventPayload::ArtifactsCommitted {
            before,
            after: made,
            branch,
            ..
        } = &e.payload
        else {
            continue;
        };
        if branch.as_deref() != Some(local_branch) {
            continue;
        }
        if before.is_empty() {
            if in_history(made)? {
                return Ok(Some(None));
            }
        } else if in_history(before)? {
            return Ok(Some(Some(before.clone())));
        }
    }
    Ok(None)
}

/// Plans the push. Every check runs before anything is shown.
pub fn plan(input: &PlanInput<'_>) -> Planned {
    match plan_inner(input) {
        Ok(p) => p,
        Err(Refused(why, detail)) => Planned::Refused { why, detail },
    }
}

fn plan_inner(input: &PlanInput<'_>) -> Result<Planned, Refused> {
    let pin = super::pin_of(input.events, input.node).ok_or(Refusal::NoPin)?;
    if let Some(code) = pin.refusal {
        return Err(match Refusal::parse(&code) {
            Some(r) => r.into(),
            None => Refused(Refusal::Unrecognized, Some(code)),
        });
    }
    let pinned_url = pin.url.as_deref().ok_or(Refusal::NoPin)?;
    let url = PushUrl::parse(pinned_url).map_err(|_| Refusal::UrlRefused)?;
    let repo = Repo::locate(input.tree).map_err(|_| Refusal::NotARepository)?;
    if repo.common_dir.to_string_lossy() != pin.common_dir {
        return Err(Refusal::RepositoryChanged.into());
    }
    if super::pin::push_url(&repo, input.tree, input.remote).as_deref() != Some(pinned_url) {
        return Err(Refusal::RemoteChanged.into());
    }
    check_file_remote(&url, &repo, input.writable)?;
    let remote = Remote::open(
        input.run_dir,
        &url,
        &repo.common_dir,
        input.scrub,
        input.abort,
    )
    .map_err(|e| Refused(Refusal::RemoteUnreachable, Some(e.to_string())))?;
    match remote.rewritten() {
        Some(false) => {}
        Some(true) => return Err(Refusal::UrlRewritten.into()),
        None => return Err(Refusal::RemoteUnreachable.into()),
    }
    let head = provenance::head(input.tree).ok_or(Refusal::HeadMoved)?;
    let local_branch = provenance::branch(input.tree);
    let heads = remote.heads(None).ok_or(Refusal::RemoteUnreachable)?;
    drop(remote);
    // The remote heads this tree knows the objects of, in one git call.
    let remote_shas: Vec<String> = heads.iter().map(|(s, _)| s.clone()).collect();
    let known = known_commits(&repo, input.tree, &remote_shas).ok_or(Refusal::HistoryUnreadable)?;

    let Some((from_seq, after, source_branch)) =
        source_commit(input.events, input.node, input.from)
    else {
        // No commit recorded: nothing to publish only when HEAD is already
        // on the remote (reachable from a head it holds); a node of a
        // concurrent batch is not tracked.
        if known.is_empty() {
            return Err(Refusal::UntrackedCommits.into());
        }
        let off_remote =
            rev_set(&repo, input.tree, &head, &known).ok_or(Refusal::HistoryUnreadable)?;
        return if off_remote.is_empty() {
            Ok(Planned::Nothing)
        } else {
            Err(Refusal::UntrackedCommits.into())
        };
    };
    if head != after {
        return Err(Refusal::HeadMoved.into());
    }
    if local_branch != source_branch {
        return Err(Refusal::BranchMoved.into());
    }
    let branch = match &input.branch {
        Some(b) => b.trim().to_string(),
        None => refname::short(&source_branch).to_string(),
    };
    if !refname::branch_ok(&branch) {
        return Err(Refusal::BranchInvalid.into());
    }
    let branch = refname::short(&branch).to_string();
    let full = refname::full(&branch);
    let remote_before = heads
        .iter()
        .find(|(_, r)| *r == full)
        .map(|(s, _)| s.clone());
    if remote_before.as_deref() == Some(after.as_str()) {
        return Ok(Planned::AlreadyOnRemote { after });
    }
    if let Some(before) = &remote_before
        && !(known.contains(before) && provenance::is_ancestor(input.tree, before, &after))
    {
        return Err(Refusal::NotFastForward.into());
    }

    let mut revs: Vec<String> = vec![after.clone()];
    revs.extend(known.iter().map(|s| format!("^{s}")));
    let rev_refs: Vec<&str> = revs.iter().map(String::as_str).collect();
    let (listed, total) = provenance::commits_of(input.tree, &rev_refs);
    let (made, made_off_remote) =
        match run_base(&repo, input.tree, input.events, &source_branch, &after)? {
            None => (HashSet::new(), HashSet::new()),
            Some(base) => {
                let base: Vec<String> = base.into_iter().collect();
                let made =
                    rev_set(&repo, input.tree, &after, &base).ok_or(Refusal::HistoryUnreadable)?;
                let mut excl = base;
                excl.extend(known.iter().cloned());
                let off =
                    rev_set(&repo, input.tree, &after, &excl).ok_or(Refusal::HistoryUnreadable)?;
                (made, off)
            }
        };
    let outside_push_suspected = made.len() > made_off_remote.len();
    let commits: Vec<PublishCommit> = listed
        .into_iter()
        .map(|c| PublishCommit {
            this_run: made.contains(&c.sha),
            commit: c,
        })
        .collect();
    // Every commit of the run the push carries, listed or not.
    let this_run = made_off_remote.len();
    let shas: Vec<String> = commits.iter().map(|c| c.commit.sha.clone()).collect();
    let digest = apb_core::publish::plan_digest(&PlanFacts {
        node: input.node,
        url: url.as_str(),
        branch: &branch,
        after: &after,
        remote_before: remote_before.as_deref(),
        commits: &shas,
    });
    Ok(Planned::Ready(Box::new(Plan {
        nonce: apb_core::publish::nonce(&digest),
        digest,
        common_dir: pin.common_dir,
        url,
        from_seq,
        branch,
        local_branch,
        omitted: total.saturating_sub(commits.len()),
        this_run,
        after,
        remote_before,
        commits,
        outside_push_suspected,
    })))
}

/// A `file` remote inside the tree's repository or a path a node may write
/// is refused: an agent could plant hooks there, which `receive-pack` would
/// run outside any sandbox at push time. A path that cannot be placed is
/// refused too (fail-closed).
fn check_file_remote(url: &PushUrl, repo: &Repo, writable: &[PathBuf]) -> Result<(), Refused> {
    if url.scheme() != Scheme::File {
        return Ok(());
    }
    let Some(path) = url.local_path().and_then(|p| {
        p.canonicalize()
            .ok()
            .or_else(|| apb_core::isolation::paths::resolve_loose(&p))
    }) else {
        return Err(Refused(
            Refusal::RemoteWritable,
            Some("(the remote's path could not be resolved)".into()),
        ));
    };
    let inside = [&repo.work_tree, &repo.common_dir]
        .into_iter()
        .chain(writable)
        .find(|dir| path.starts_with(dir));
    match inside {
        Some(dir) => Err(Refused(
            Refusal::RemoteWritable,
            Some(format!("(inside {})", dir.display())),
        )),
        None => Ok(()),
    }
}

/// The commits of `shas` the tree's repository holds, in order: one
/// `cat-file --batch-check` however many there are. `None` when git could
/// not answer, which is never read as "none of them".
fn known_commits(repo: &Repo, tree: &Path, shas: &[String]) -> Option<Vec<String>> {
    let hex: Vec<&String> = shas
        .iter()
        .filter(|s| !s.is_empty() && s.chars().all(|c| c.is_ascii_hexdigit()))
        .collect();
    if hex.is_empty() {
        return Some(Vec::new());
    }
    let input: String = hex.iter().map(|s| format!("{s}\n")).collect();
    let out = repo.read_input(
        tree,
        &["cat-file", "--batch-check=%(objectname) %(objecttype)"],
        &input,
    )?;
    let commits: HashSet<&str> = out
        .lines()
        .filter_map(|l| l.strip_suffix(" commit"))
        .collect();
    Some(
        hex.into_iter()
            .filter(|s| commits.contains(s.as_str()))
            .cloned()
            .collect(),
    )
}

/// Every commit reachable from `tip` and from none of `not`; `None` when
/// git could not list them. The exclusions reach git on its stdin.
fn rev_set(repo: &Repo, tree: &Path, tip: &str, not: &[String]) -> Option<HashSet<String>> {
    let mut input = format!("{tip}\n");
    for s in not {
        input.push('^');
        input.push_str(s);
        input.push('\n');
    }
    repo.read_input(tree, &["rev-list", "--stdin"], &input)
        .map(|t| t.lines().map(|l| l.trim().to_string()).collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_history_git_cannot_read_stops_the_run_base_search() {
        let tmp = tempfile::tempdir().unwrap();
        let tree = tmp.path().join("tree");
        std::fs::create_dir_all(&tree).unwrap();
        let ok = std::process::Command::new("git")
            .args(["init", "-q"])
            .current_dir(&tree)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .status()
            .unwrap()
            .success();
        assert!(ok);
        let repo = Repo::locate(&tree).unwrap();
        // The tree goes away after it was located: every git call fails.
        std::fs::remove_dir_all(&tree).unwrap();
        let committed = |before: &str| Event {
            seq: 1,
            ts: 1,
            payload: EventPayload::ArtifactsCommitted {
                node: "work".into(),
                before: before.into(),
                after: "b".repeat(40),
                commits: Vec::new(),
                omitted: 0,
                branch: Some("refs/heads/main".into()),
            },
        };
        let events = [committed(&"a".repeat(40)), committed(&"c".repeat(40))];
        assert_eq!(
            run_base(&repo, &tree, &events, "refs/heads/main", &"b".repeat(40)),
            Err(Refusal::HistoryUnreadable)
        );
    }
}
