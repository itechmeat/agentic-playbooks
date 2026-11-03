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
//! from `APB_RUN_ID` or `{{run.id}}`. The trailer values are read back with
//! each commit, so the run surfaces can flag a commit without one; apb never
//! writes or rewrites a trailer itself.
//!
//! Per attempt: while a tracker is live for a node, every attempt that
//! starts reads `HEAD` of the same tree ([`attempt_started`]). The head an
//! attempt left is the head the next attempt started from, and the last
//! attempt's is the execution's final `HEAD`; the event names each
//! attempt's head and the attempt that ended the execution. Display only.

use std::path::{Path, PathBuf};

use apb_core::schema::{NodeKind, Playbook};

use super::journal::Journal;
use crate::error::EngineError;
use crate::event::{AttemptHead, CommittedArtifact, EventPayload};

/// The live trackers of this process, by run directory and node: the tree
/// each reads and the `HEAD` each attempt started from, in attempt order.
type Live = std::collections::BTreeMap<(PathBuf, String), (PathBuf, Vec<(u64, Option<String>)>)>;

static LIVE: std::sync::Mutex<Live> = std::sync::Mutex::new(std::collections::BTreeMap::new());

fn live() -> std::sync::MutexGuard<'static, Live> {
    LIVE.lock().unwrap_or_else(|e| e.into_inner())
}

/// Records that attempt `attempt` (the seq of its `attempt_started`) of
/// `node` started: the tree's `HEAD` now is where the previous attempt left
/// it. Nothing happens unless a tracker is live for the node.
pub(crate) fn attempt_started(run_dir: &Path, node: &str, attempt: u64) {
    let key = (run_dir.to_path_buf(), node.to_string());
    let Some(dir) = live().get(&key).map(|(dir, _)| dir.clone()) else {
        return;
    };
    // Read outside the lock: a git call may take a moment.
    let at = head(&dir);
    if let Some((_, starts)) = live().get_mut(&key) {
        starts.push((attempt, at));
    }
}

/// Each attempt's head, from the heads the attempts started from and the
/// execution's final one. An attempt whose head is unknown is left out.
fn attempt_heads(mut starts: Vec<(u64, Option<String>)>, after: &str) -> Vec<AttemptHead> {
    starts.sort_by_key(|(a, _)| *a);
    let ends = starts
        .iter()
        .skip(1)
        .map(|(_, h)| h.clone())
        .chain(std::iter::once(Some(after.to_string())));
    starts
        .iter()
        .zip(ends)
        .filter_map(|((attempt, _), head)| {
            Some(AttemptHead {
                attempt: *attempt,
                head: head?,
            })
        })
        .collect()
}

/// At most this many commits are listed per event; the rest is counted.
pub(crate) const MAX_LISTED_COMMITS: usize = 50;

/// `git -C dir <args>` stdout, `None` on any failure (no git, not a
/// repository, a non-zero exit). Hardened ([`apb_core::git`]), so reading
/// `HEAD` never runs a program the tree names.
fn git(dir: &Path, args: &[&str]) -> Option<String> {
    apb_core::git::read(dir, args)
}

/// `git -C dir <args> --stdin` with the revisions `revs` on its stdin, one
/// per line; `None` on any failure.
pub(crate) fn git_revs(dir: &Path, args: &[&str], revs: &[&str]) -> Option<String> {
    let mut all = args.to_vec();
    all.push("--stdin");
    let mut input = revs.join("\n");
    input.push('\n');
    apb_core::git::read_input(dir, &all, &input)
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
pub(crate) fn is_ancestor(dir: &Path, before: &str, after: &str) -> bool {
    apb_core::git::succeeds(dir, &["merge-base", "--is-ancestor", before, after]).unwrap_or(false)
}

/// The commits reachable from `after` but not from `before`, newest first,
/// and how many there are in all. Callers check [`is_ancestor`] first: the
/// range means "what the node committed" only when `after` descends from
/// `before`. Each commit carries its `Apb-Run` trailer values; a git that
/// cannot read trailers lists the commits without them (`apb_run: None`).
pub(crate) fn commits_between(
    dir: &Path,
    before: &str,
    after: &str,
) -> (Vec<CommittedArtifact>, usize) {
    commits_of(dir, &[&format!("{before}..{after}")])
}

/// The commits git's revision arguments `revs` select (`after`, `^base`,
/// `a..b`), newest first, at most [`MAX_LISTED_COMMITS`], and how many there
/// are in all, each with its `Apb-Run` trailer values when git reads them.
/// Every revision is a sha or a range of shas, never an option; they reach
/// git on its stdin, so a remote with many heads never grows the argv.
pub(crate) fn commits_of(dir: &Path, revs: &[&str]) -> (Vec<CommittedArtifact>, usize) {
    let total = git_revs(dir, &["rev-list", "--count"], revs)
        .and_then(|s| s.trim().parse::<usize>().ok())
        .unwrap_or(0);
    let listed = list_commits(dir, revs, true)
        .or_else(|| list_commits(dir, revs, false))
        .unwrap_or_default();
    (listed, total)
}

/// The `Apb-Run` trailer values, unit-separated and unfolded (a folded
/// value stays on one line), between the sha and the subject.
const TRAILER_FORMAT: &str =
    "--format=%H%x09%(trailers:key=Apb-Run,valueonly,unfold,separator=%x1f)%x09%s";

/// `git log` over `revs`, at most [`MAX_LISTED_COMMITS`], with the trailer
/// values when `trailers`; `None` when git fails or cannot read trailers.
fn list_commits(dir: &Path, revs: &[&str], trailers: bool) -> Option<Vec<CommittedArtifact>> {
    let limit = format!("--max-count={MAX_LISTED_COMMITS}");
    let format = if trailers {
        TRAILER_FORMAT
    } else {
        "--format=%H%x09%s"
    };
    let text = git_revs(dir, &["log", "--no-show-signature", &limit, format], revs)?;
    parse_log(&text, trailers)
}

/// The commits of a `git log` printed in [`TRAILER_FORMAT`] (`trailers`) or
/// as sha and subject. `None` when a line shows the trailer placeholder
/// itself: a git too old to know its options prints it literally, and its
/// text is no run id.
fn parse_log(text: &str, trailers: bool) -> Option<Vec<CommittedArtifact>> {
    let mut out = Vec::new();
    for line in text.lines() {
        let Some((sha, rest)) = line.split_once('\t') else {
            continue;
        };
        let (apb_run, subject) = if trailers {
            let Some((values, subject)) = rest.split_once('\t') else {
                continue;
            };
            if values.contains("%(") {
                return None;
            }
            let ids = values
                .split('\u{1f}')
                .map(str::trim)
                .filter(|v| !v.is_empty())
                .map(str::to_string)
                .collect();
            (Some(ids), subject)
        } else {
            (None, rest)
        };
        out.push(CommittedArtifact {
            sha: sha.to_string(),
            subject: subject.to_string(),
            apb_run,
        });
    }
    Some(out)
}

/// The directory whose `HEAD` a node's commits land in: the node's own
/// `workdir` when it renders, else the run's working tree. The publish
/// plan resolves its source's tree through this too.
pub(crate) fn node_tree(
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
    /// The run directory it is registered under for [`attempt_started`].
    run_dir: PathBuf,
}

impl Drop for Tracker {
    fn drop(&mut self) {
        live().remove(&(self.run_dir.clone(), self.node.clone()));
    }
}

/// The branch `HEAD` names in `dir`, empty when it is detached.
pub(crate) fn branch(dir: &Path) -> String {
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
        // A scenario run reads no `HEAD`: nothing it ran can commit.
        if crate::scenario::session::skips(run_dir, crate::scenario::BoundaryKind::Provenance) {
            return None;
        }
        let mut t = Self::start_here(playbook, node_id, workdir, render)?;
        t.run_dir = run_dir.to_path_buf();
        live().insert(
            (t.run_dir.clone(), t.node.clone()),
            (t.dir.clone(), Vec::new()),
        );
        t.restore_parked();
        Some(t)
    }

    /// Takes over what [`Tracker::park`] kept for this node: the `HEAD` the
    /// first round started from, its branch, and the head each attempt of
    /// the earlier rounds started from, so the answer round's record
    /// attributes the commits made before the question to their attempts.
    fn restore_parked(&mut self) {
        let Ok(parked) = std::fs::read_to_string(parked_path(&self.run_dir, &self.node)) else {
            return;
        };
        let is_sha = |s: &str| !s.is_empty() && s.chars().all(|c| c.is_ascii_hexdigit());
        let mut lines = parked.lines();
        let sha = lines.next().unwrap_or_default().trim();
        if !is_sha(sha) {
            return;
        }
        self.before = sha.to_string();
        self.branch = lines.next().unwrap_or_default().trim().to_string();
        // `<attempt>\t<head>` per earlier attempt; an empty head is unknown.
        let starts: Vec<(u64, Option<String>)> = lines
            .filter_map(|l| {
                let (attempt, head) = l.split_once('\t')?;
                let head = head.trim();
                Some((
                    attempt.trim().parse().ok()?,
                    is_sha(head).then(|| head.to_string()),
                ))
            })
            .collect();
        if let Some((_, live_starts)) = live().get_mut(&(self.run_dir.clone(), self.node.clone())) {
            let mut all = starts;
            all.append(live_starts);
            *live_starts = all;
        }
    }

    /// Keeps `before` and the heads the attempts so far started from for
    /// the answer round of a node that just asked a question. Best effort:
    /// without it the answer round records only its own commits.
    pub(crate) fn park(self, run_dir: &Path) {
        let path = parked_path(run_dir, &self.node);
        let starts = live()
            .get(&(self.run_dir.clone(), self.node.clone()))
            .map(|(_, s)| s.clone())
            .unwrap_or_default();
        if let Some(parent) = path.parent()
            && std::fs::create_dir_all(parent).is_ok()
        {
            let mut text = format!("{}\n{}\n", self.before, self.branch);
            for (attempt, head) in starts {
                text.push_str(&format!("{attempt}\t{}\n", head.unwrap_or_default()));
            }
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
            run_dir: PathBuf::new(),
        })
    }

    /// Journals `artifacts_committed` when `HEAD` moved. Best effort: a git
    /// failure after the node ran records nothing rather than failing it.
    pub(crate) fn finish(self, run_dir: &Path, journal: &Journal) -> Result<(), EngineError> {
        let _ = std::fs::remove_file(parked_path(run_dir, &self.node));
        let starts = live()
            .remove(&(self.run_dir.clone(), self.node.clone()))
            .map(|(_, starts)| starts)
            .unwrap_or_default();
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
        let attempts = attempt_heads(starts, &after);
        journal.append(EventPayload::ArtifactsCommitted {
            node: self.node.clone(),
            before: self.before.clone(),
            omitted: total.saturating_sub(commits.len()),
            commits,
            branch: (!self.branch.is_empty()).then(|| self.branch.clone()),
            attempt: attempts.last().map(|a| a.attempt),
            attempts,
            after,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn git_ok(dir: &Path, args: &[&str]) {
        let st = std::process::Command::new("git")
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

    /// A git that cannot read trailers prints the placeholder as text: that
    /// line is not a run id, and the listing falls back to sha and subject.
    #[test]
    fn a_literal_trailer_placeholder_falls_back_to_the_plain_listing() {
        let literal = "abc\t%(trailers:key=Apb-Run,valueonly,unfold,separator=\u{1f})\tFix it\n";
        assert_eq!(parse_log(literal, true), None);
        let plain = parse_log("abc\tFix it\n", false).unwrap();
        assert_eq!(plain[0].subject, "Fix it");
        assert_eq!(plain[0].apb_run, None);
        let read = parse_log("abc\tr-1\u{1f}r-2\tFix it\ndef\t\tOther\n", true).unwrap();
        assert_eq!(
            read.iter().map(|c| c.apb_run.clone()).collect::<Vec<_>>(),
            vec![
                Some(vec!["r-1".to_string(), "r-2".to_string()]),
                Some(Vec::new())
            ]
        );
    }

    /// An attempt's head is where the next one started; the last one's is
    /// the execution's final head; an unknown start leaves out the attempt
    /// before it.
    #[test]
    fn each_attempt_left_the_head_the_next_one_started_from() {
        let heads = attempt_heads(
            vec![
                (30, Some("h2".into())),
                (10, Some("h0".into())),
                (20, Some("h1".into())),
            ],
            "h3",
        );
        let pairs: Vec<(u64, &str)> = heads.iter().map(|h| (h.attempt, h.head.as_str())).collect();
        assert_eq!(pairs, [(10, "h1"), (20, "h2"), (30, "h3")]);
        let gap = attempt_heads(vec![(10, Some("h0".into())), (20, None)], "h3");
        let pairs: Vec<(u64, &str)> = gap.iter().map(|h| (h.attempt, h.head.as_str())).collect();
        assert_eq!(pairs, [(20, "h3")]);
        assert!(attempt_heads(Vec::new(), "h3").is_empty());
    }

    /// A tracker collects the heads of the attempts that start while it is
    /// live for its node, and only those.
    #[test]
    fn a_live_tracker_collects_its_attempts_heads() {
        let tmp = tempfile::tempdir().unwrap();
        let d = tmp.path();
        git_ok(d, &["init", "-q"]);
        git_ok(d, &["commit", "-q", "--allow-empty", "-m", "base"]);
        let run = d.join("run");
        let t = Tracker {
            node: "impl".into(),
            dir: d.to_path_buf(),
            before: head(d).unwrap(),
            branch: branch(d),
            run_dir: run.clone(),
        };
        live().insert((run.clone(), "impl".into()), (d.to_path_buf(), Vec::new()));
        attempt_started(&run, "impl", 5);
        attempt_started(&run, "other", 6);
        git_ok(d, &["commit", "-q", "--allow-empty", "-m", "one"]);
        let mid = head(d).unwrap();
        attempt_started(&run, "impl", 9);
        let starts = live().get(&(run.clone(), "impl".into())).unwrap().1.clone();
        assert_eq!(starts.iter().map(|(a, _)| *a).collect::<Vec<_>>(), [5, 9]);
        assert_eq!(starts[1].1.as_deref(), Some(mid.as_str()));
        drop(t);
        assert!(
            live().get(&(run, "impl".into())).is_none(),
            "dropped with its tracker"
        );
    }

    /// Attempt 1 commits and asks a question; the answer round's attempt
    /// starts from that commit and finishes. The parked heads come back
    /// with the round, so the commit stays attempt 1's.
    #[test]
    fn a_question_round_keeps_the_heads_of_the_attempts_before_it() {
        let tmp = tempfile::tempdir().unwrap();
        let d = tmp.path();
        git_ok(d, &["init", "-q"]);
        git_ok(d, &["commit", "-q", "--allow-empty", "-m", "base"]);
        let run = d.join("run-q");
        let tracker = |run: &Path| {
            let t = Tracker {
                node: "ask".into(),
                dir: d.to_path_buf(),
                before: head(d).unwrap(),
                branch: branch(d),
                run_dir: run.to_path_buf(),
            };
            live().insert(
                (run.to_path_buf(), "ask".into()),
                (d.to_path_buf(), Vec::new()),
            );
            t
        };
        let base = head(d).unwrap();
        let first = tracker(&run);
        attempt_started(&run, "ask", 5);
        git_ok(d, &["commit", "-q", "--allow-empty", "-m", "a"]);
        let a = head(d).unwrap();
        first.park(&run);
        let mut answer = tracker(&run);
        answer.restore_parked();
        assert_eq!(answer.before, base, "the first round's start");
        attempt_started(&run, "ask", 9);
        let starts = live().get(&(run.clone(), "ask".into())).unwrap().1.clone();
        let pairs: Vec<(u64, String)> = attempt_heads(starts, &a)
            .into_iter()
            .map(|h| (h.attempt, h.head))
            .collect();
        assert_eq!(pairs, [(5, a.clone()), (9, a)]);
        drop(answer);
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
        // The trailer is read; a commit without one says so.
        assert_eq!(commits[0].apb_run, Some(vec!["r1".to_string()]));
        assert_eq!(commits[1].apb_run, Some(Vec::new()));
        // Without trailers (a git too old to read them) nothing is claimed.
        let plain = list_commits(d, &[&format!("{before}..{after}")], false).unwrap();
        assert_eq!(plain[0].subject, "two");
        assert_eq!(plain[0].apb_run, None);
        // A reset back to an older commit is not a forward move.
        assert!(!is_ancestor(d, &after, &before));
    }
}
