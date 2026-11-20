//! The scope of a review request (people-in-the-loop D2): what its
//! decision covers, counted since the gate's previous decision on the
//! same node, from the journal and, for changed lines, from git. Counts
//! only, never content.

use std::path::Path;

use apb_core::review_form::MaxScope;

use crate::event::{Event, EventPayload};

pub use super::views::ReviewScope;

/// The seq the scope window of `node` starts after: the latest
/// `ReviewDecided` of the node, or 0 when there is none, then the larger
/// of that and `floor_seq`. A withdrawn request decided nothing, so a
/// `ReviewWithdrawn` never moves the window: the next request still
/// covers the work the withdrawn one did. Every gate of this change passes
/// 0; the parameter stays so that a later visit-scoped gate can start its
/// window at its visit's `started_seq` without changing this signature.
pub fn window_start(events: &[Event], node: &str, floor_seq: u64) -> u64 {
    let mut start = 0u64;
    for e in events {
        if let EventPayload::ReviewDecided { node: n, .. } = &e.payload
            && n == node
        {
            start = e.seq;
        }
    }
    start.max(floor_seq)
}

/// The scope of everything `events` journal after `since_seq`: the
/// commits, changed files and lines the run's nodes recorded, the list
/// items settled, the connector writes, the send intents and the result
/// snapshots. Counts only: no path, no content. `workdir` is the git tree
/// the commits live in; `run_dir` names the run whose connector snapshots
/// classify the calls; `now_ms` ages the open sends.
pub fn scope_since(
    workdir: &Path,
    run_dir: &Path,
    events: &[Event],
    since_seq: u64,
    now_ms: u64,
) -> ReviewScope {
    let mut s = ReviewScope {
        since_seq,
        ..Default::default()
    };
    let in_window = |e: &Event| e.seq > since_seq;

    // Commits, files and lines: every commit the window's events
    // recorded, each once, whatever the branch looks like now.
    let ranges: Vec<RecordedRange<'_>> = events
        .iter()
        .filter(|e| in_window(e))
        .filter_map(|e| match &e.payload {
            EventPayload::ArtifactsCommitted {
                before,
                after,
                commits,
                omitted,
                ..
            } => Some(RecordedRange {
                before,
                after,
                listed: commits.iter().map(|c| c.sha.as_str()).collect(),
                total: commits.len() + *omitted,
            }),
            _ => None,
        })
        .collect();
    let counted = count_recorded(workdir, &ranges);
    s.commits = counted.commits;
    s.files = counted.files;
    s.lines_added = counted.lines_added;
    s.lines_removed = counted.lines_removed;

    // List items settled passed or needs_look; carried ones left out.
    for e in events.iter().filter(|e| in_window(e)) {
        if let EventPayload::ListItemSettled { state, carried, .. } = &e.payload
            && !*carried
            && (state == "passed" || state == "needs_look")
        {
            s.items_settled += 1;
        }
    }

    // Connector writes over the classified calls; apb's own notifications
    // left out.
    for c in crate::run_outcome::calls::classified_calls(run_dir, events) {
        if !in_window_seq(c.seq, since_seq) || c.node == apb_core::connector::sites::NOTIFY_SITE {
            continue;
        }
        match c.class {
            crate::run_outcome::calls::CallClass::WriteOk
            | crate::run_outcome::calls::CallClass::WriteIncomplete
            | crate::run_outcome::calls::CallClass::WriteUnknown => {
                s.writes += 1;
                if c.class == crate::run_outcome::calls::CallClass::WriteUnknown {
                    s.writes_unknown += 1;
                }
            }
            _ => {}
        }
    }

    // Send intents recorded; apb's own notifications left out.
    let mut ids: Vec<&str> = Vec::new();
    for e in events.iter().filter(|e| in_window(e)) {
        if let EventPayload::SendIntentRecorded { intent, node, .. } = &e.payload
            && node != apb_core::connector::sites::NOTIFY_SITE
        {
            ids.push(intent.as_str());
        }
    }
    s.sends = ids.len() as u32;
    let intents = crate::run_outcome::intents::intents_from(events, now_ms);
    s.sends_open = ids
        .iter()
        .filter(|id| intents.iter().any(|i| i.id == **id && i.open))
        .count() as u32;

    // Result snapshots.
    for e in events.iter().filter(|e| in_window(e)) {
        if let EventPayload::NodeFinished { result, .. } = &e.payload
            && result.is_some()
        {
            s.results += 1;
        }
    }
    s
}

fn in_window_seq(seq: u64, since_seq: u64) -> bool {
    seq > since_seq
}

/// One `ArtifactsCommitted` of the window: the range it recorded, the
/// shas it listed and how many commits it counted in all.
struct RecordedRange<'a> {
    before: &'a str,
    after: &'a str,
    listed: Vec<&'a str>,
    total: usize,
}

/// What the recorded commits add up to.
struct Counted {
    commits: u32,
    files: Option<u32>,
    lines_added: Option<u32>,
    lines_removed: Option<u32>,
}

/// Each counted commit's changed files: `(path, added, removed)`, the
/// path held in memory for the count only.
type Changes = std::collections::BTreeMap<String, Vec<(String, u32, u32)>>;

/// Counts every commit `ranges` recorded, each once and by its own
/// change against its first parent (a root commit against the empty
/// tree): what a merge commit brings in or adds counts like any other
/// change. The commits of a range are the ones git lists for it now (a
/// range names commit objects, so it reads the same after a reset or a
/// rebase moved the branch away), deduplicated by sha; the files are
/// deduplicated by path and the lines summed over the distinct commits; a
/// binary file is a file with 0 lines. A range the journal did not record
/// whole, one git cannot list, one that no longer holds the number of
/// commits it recorded or a listed sha, and a commit git cannot show make
/// files and lines unknown, never 0; the commit count then is what the
/// journal recorded (listed shas once, plus the commits past the listed
/// ones).
fn count_recorded(workdir: &Path, ranges: &[RecordedRange<'_>]) -> Counted {
    use std::collections::{BTreeMap, HashSet};
    let journaled = {
        let mut listed: HashSet<&str> = HashSet::new();
        let mut omitted = 0usize;
        for r in ranges {
            listed.extend(r.listed.iter().copied());
            omitted += r.total.saturating_sub(r.listed.len());
        }
        u32::try_from(listed.len() + omitted).unwrap_or(u32::MAX)
    };
    let unknown = Counted {
        commits: journaled,
        files: None,
        lines_added: None,
        lines_removed: None,
    };
    if ranges.is_empty() {
        return Counted {
            commits: 0,
            files: Some(0),
            lines_added: Some(0),
            lines_removed: Some(0),
        };
    }
    // Every commit of the window -> its first parent, `None` for a root.
    let mut commits: BTreeMap<String, Option<String>> = BTreeMap::new();
    for r in ranges {
        let Some(listed) = commits_in(workdir, r) else {
            return unknown;
        };
        for (sha, parent) in listed {
            commits.entry(sha).or_insert(parent);
        }
    }
    // Lines count only where git keeps the work tree's attributes out of
    // the diff (`apb_core::git::no_tree_attributes`): otherwise a
    // `.gitattributes` edited after the commits could turn their text
    // changes into binary ones, so the lines are unknown there.
    let pinned = apb_core::git::no_tree_attributes(workdir);
    let Some(changes) = changes_of(workdir, &commits, pinned.as_deref()) else {
        return unknown;
    };
    let mut paths: HashSet<&str> = HashSet::new();
    let mut added = 0u32;
    let mut removed = 0u32;
    for lines in changes.values() {
        for (path, a, r) in lines {
            paths.insert(path);
            added = added.saturating_add(*a);
            removed = removed.saturating_add(*r);
        }
    }
    let pinned = pinned.is_some();
    Counted {
        commits: u32::try_from(changes.len()).unwrap_or(u32::MAX),
        files: Some(u32::try_from(paths.len()).unwrap_or(u32::MAX)),
        lines_added: pinned.then_some(added),
        lines_removed: pinned.then_some(removed),
    }
}

/// Whether `s` is a full commit id as git prints it (SHA-1 or SHA-256).
fn is_object_id(s: &str) -> bool {
    matches!(s.len(), 40 | 64) && s.bytes().all(|b| b.is_ascii_hexdigit())
}

/// The commits of one recorded range with their first parents, as git
/// lists them now (`rev-list --parents before..after`); `None` when the
/// journal did not record both ends as commit ids, git cannot list the
/// range, or the list no longer holds the number of commits the range
/// recorded or one of its listed shas.
fn commits_in(workdir: &Path, r: &RecordedRange<'_>) -> Option<Vec<(String, Option<String>)>> {
    if !is_object_id(r.before) || !is_object_id(r.after) {
        return None;
    }
    let range = format!("{}..{}", r.before, r.after);
    let text = apb_core::git::read(workdir, &["rev-list", "--parents", &range, "--"])?;
    let mut out = Vec::new();
    for line in text.lines().filter(|l| !l.trim().is_empty()) {
        let mut ids = line.split(' ');
        let sha = ids.next().filter(|s| is_object_id(s))?;
        let parent = match ids.next() {
            Some(p) if is_object_id(p) => Some(p.to_string()),
            Some(_) => return None,
            None => None,
        };
        out.push((sha.to_string(), parent));
    }
    if out.len() != r.total || r.listed.iter().any(|l| !out.iter().any(|(s, _)| s == l)) {
        return None;
    }
    Some(out)
}

/// The numstat of each of `commits` against its first parent (a root
/// against the empty tree), read in one `git diff-tree --stdin` with the
/// flags pinned: no rename detection, no textconv, no external diff, no
/// attributes from the work tree or the operator's global file when
/// `attributes` holds those options (`apb_core::git::no_tree_attributes`,
/// so a binary file is told by the commits' content alone), NUL-separated
/// paths, a header for every
/// commit even one with no change. `None` when git fails, the output does not parse or it does
/// not hold every commit exactly once.
fn changes_of(
    workdir: &Path,
    commits: &std::collections::BTreeMap<String, Option<String>>,
    attributes: Option<&[String]>,
) -> Option<Changes> {
    let mut input = String::new();
    for (sha, parent) in commits {
        input.push_str(sha);
        if let Some(p) = parent {
            input.push(' ');
            input.push_str(p);
        }
        input.push('\n');
    }
    let mut args: Vec<&str> = attributes
        .unwrap_or_default()
        .iter()
        .map(String::as_str)
        .collect();
    args.extend([
        "diff-tree",
        "--stdin",
        "-r",
        "-z",
        "--numstat",
        "--no-renames",
        "--root",
        "--always",
        "--no-color",
    ]);
    args.extend(apb_core::git::DIFF_FLAGS);
    let text = apb_core::git::read_stored_diff(workdir, &args, &input)?;
    let changes = read_numstat(&text)?;
    let all = changes.len() == commits.len() && commits.keys().all(|k| changes.contains_key(k));
    all.then_some(changes)
}

/// Parses `git diff-tree -z --numstat --always` output: a commit id field
/// opens each commit's block, then one `<added>\t<removed>\t<path>` field
/// per changed file. A `-` in a count (a binary file) adds 0 lines.
/// `None` on a numstat field before the first commit, one that does not
/// split into three parts with numbers or `-`, or a commit seen twice.
fn read_numstat(text: &str) -> Option<Changes> {
    let mut out = Changes::new();
    let mut current: Option<String> = None;
    for field in text.split('\0') {
        let field = field.trim_start_matches('\n');
        if field.is_empty() {
            continue;
        }
        if !field.contains('\t') {
            let sha = field.trim();
            if !is_object_id(sha) || out.insert(sha.to_string(), Vec::new()).is_some() {
                return None;
            }
            current = Some(sha.to_string());
            continue;
        }
        let sha = current.as_ref()?;
        let mut parts = field.splitn(3, '\t');
        let a = count_of(parts.next()?)?;
        let r = count_of(parts.next()?)?;
        let path = parts.next().filter(|p| !p.is_empty())?;
        out.get_mut(sha)?.push((path.to_string(), a, r));
    }
    Some(out)
}

fn count_of(part: &str) -> Option<u32> {
    if part == "-" {
        return Some(0);
    }
    part.parse::<u32>().ok()
}

/// The least active time a person needs to have read the scope; below it
/// a person's decision counts as decided without review. An unknown count
/// reads as 0.
pub fn fast_floor_ms(s: &ReviewScope) -> u64 {
    let floor = 5_000u64
        + 1_000 * u64::from(s.files.unwrap_or(0))
        + 50 * u64::from(s.lines_added.unwrap_or(0) + s.lines_removed.unwrap_or(0))
        + 2_000 * u64::from(s.writes + s.sends)
        + 1_000 * u64::from(s.items_settled);
    floor.clamp(5_000, 300_000)
}

/// The one scope line every surface prints: parts with a zero count left
/// out except commits, in this order.
pub fn scope_line(s: &ReviewScope) -> String {
    let all_quiet = s.commits == 0
        && s.files.unwrap_or(0) == 0
        && s.lines_added.unwrap_or(0) + s.lines_removed.unwrap_or(0) == 0
        && s.items_settled == 0
        && s.writes == 0
        && s.sends == 0
        && s.results == 0;
    if all_quiet {
        return "nothing changed since the last decision".into();
    }
    let mut parts: Vec<String> = Vec::new();
    parts.push(format!(
        "{} commit{}",
        s.commits,
        if s.commits == 1 { "" } else { "s" }
    ));
    match s.files {
        Some(files) if files > 0 => parts.push(format!(
            "{} file{}",
            files,
            if files == 1 { "" } else { "s" }
        )),
        Some(_) => {}
        None => parts.push("files not read".into()),
    }
    match (s.lines_added, s.lines_removed) {
        (Some(a), Some(r)) if a + r > 0 => {
            parts.push(format!("+{a} -{r} lines"));
        }
        (Some(_), Some(_)) => {}
        _ => parts.push("lines not read".into()),
    }
    if s.items_settled > 0 {
        parts.push(format!(
            "{} list item{}",
            s.items_settled,
            if s.items_settled == 1 { "" } else { "s" }
        ));
    }
    if s.writes > 0 {
        let mut part = format!(
            "{} connector write{}",
            s.writes,
            if s.writes == 1 { "" } else { "s" }
        );
        if s.writes_unknown > 0 {
            part.push_str(&format!(" ({} unknown)", s.writes_unknown));
        }
        parts.push(part);
    }
    if s.sends > 0 {
        let mut part = format!("{} send{}", s.sends, if s.sends == 1 { "" } else { "s" });
        if s.sends_open > 0 {
            part.push_str(&format!(" ({} open)", s.sends_open));
        }
        parts.push(part);
    }
    if s.results > 0 {
        parts.push(format!(
            "{} result{}",
            s.results,
            if s.results == 1 { "" } else { "s" }
        ));
    }
    parts.join(", ")
}

/// The options of `max.withhold` to refuse while the scope passes one of
/// the limits, with the reason naming the counts that passed and the
/// limits they passed. A limit whose count nobody read counts as passed
/// (fail-closed); the counts in the reason are the ones that passed.
pub fn withheld(
    max: &MaxScope,
    s: &ReviewScope,
    options: &[String],
) -> (Vec<String>, Option<String>) {
    // One passed limit: (count text, limit text).
    let mut passed: Vec<(String, String)> = Vec::new();
    if let Some(limit) = max.files {
        match s.files {
            Some(n) if n > limit => passed.push((count_words(n, "file"), format!("files {limit}"))),
            Some(_) => {}
            // Unknown: not read, passes fail-closed.
            None => passed.push(("files not read".into(), format!("files {limit}"))),
        }
    }
    if let Some(limit) = max.lines {
        match (s.lines_added, s.lines_removed) {
            (Some(a), Some(r)) if a + r > limit => {
                passed.push((count_words(a + r, "line"), format!("lines {limit}")));
            }
            (Some(_), Some(_)) => {}
            _ => passed.push(("lines not read".into(), format!("lines {limit}"))),
        }
    }
    if let Some(limit) = max.writes {
        let n = s.writes + s.sends;
        if n > limit {
            passed.push((count_words(n, "write"), format!("writes {limit}")));
        }
    }
    if passed.is_empty() {
        return (Vec::new(), None);
    }
    let counts: Vec<String> = passed.iter().map(|(c, _)| c.clone()).collect();
    let limits: Vec<String> = passed.iter().map(|(_, l)| l.clone()).collect();
    let reason = format!(
        "the scope ({}) is above max_scope ({}); split the work or choose another option",
        counts.join(", "),
        limits.join(", ")
    );
    (max.withhold_for(options), Some(reason))
}

/// `<n> <noun>` with the noun pluralized the way the scope line does.
fn count_words(n: u32, noun: &str) -> String {
    if n == 1 {
        format!("{n} {noun}")
    } else {
        format!("{n} {noun}s")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::{CommittedArtifact, EventPayload};

    fn ev(seq: u64, payload: EventPayload) -> Event {
        Event {
            seq,
            ts: seq as u128 * 1000,
            payload,
        }
    }

    fn committed(seq: u64, node: &str, before: &str, after: &str, shas: &[&str]) -> Event {
        ev(
            seq,
            EventPayload::ArtifactsCommitted {
                node: node.into(),
                before: before.into(),
                after: after.into(),
                commits: shas
                    .iter()
                    .map(|s| CommittedArtifact {
                        sha: (*s).into(),
                        subject: String::new(),
                        apb_run: None,
                    })
                    .collect(),
                omitted: 0,
                branch: None,
                attempt: None,
                attempts: Vec::new(),
            },
        )
    }

    fn decided(seq: u64, node: &str) -> Event {
        ev(
            seq,
            EventPayload::ReviewDecided {
                node: node.into(),
                decision: "approve".into(),
                note: String::new(),
                plan_digest: None,
                decided_by: None,
                binding: None,
                via: None,
            },
        )
    }

    /// The window starts after the latest decision of the same node (a
    /// withdrawal does not move it), and never before `floor_seq`.
    #[test]
    fn the_window_starts_after_the_last_decision() {
        let events = vec![
            committed(1, "build", "", "a1", &["a1"]),
            decided(2, "gate"),
            committed(3, "build", "a1", "a2", &["a2"]),
            ev(
                4,
                EventPayload::ReviewWithdrawn {
                    node: "gate".into(),
                    reason: "moved".into(),
                },
            ),
            committed(5, "build", "a2", "a3", &["a3"]),
        ];
        // A withdrawal decided nothing: the window still starts after the
        // decision at seq 2, so the reopened request covers seq 3 too.
        assert_eq!(window_start(&events, "gate", 0), 2);
        // Another gate's decision does not move the window.
        let mut other = events.clone();
        other.push(decided(6, "other"));
        assert_eq!(window_start(&other, "gate", 0), 2);
        // No decision at all: the run's start.
        let plain = vec![committed(1, "build", "", "a1", &["a1"])];
        assert_eq!(window_start(&plain, "gate", 0), 0);
        // The floor holds the window back.
        assert_eq!(window_start(&events, "gate", 9), 9);
    }

    /// Commits, files and lines are counted over the window only, from a
    /// real git repository with two commits.
    #[test]
    fn commits_and_counts_come_from_the_window() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        let git = |args: &[&str]| {
            let out = std::process::Command::new("git")
                .args(args)
                .current_dir(dir)
                .output()
                .unwrap();
            assert!(out.status.success(), "git {args:?}");
            String::from_utf8(out.stdout).unwrap().trim().to_string()
        };
        // A base commit, so the first recorded commit has a real `before`
        // as the writer journals it.
        std::fs::write(dir.join("a.txt"), "one\n").unwrap();
        git(&["init", "-q"]);
        git(&["add", "a.txt"]);
        git(&[
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@e",
            "commit",
            "-qm",
            "base",
        ]);
        let base = git(&["rev-parse", "HEAD"]);
        // The first commit replaces a line and adds a file: +2 -1.
        std::fs::write(dir.join("a.txt"), "uno\n").unwrap();
        std::fs::write(dir.join("c.txt"), "c\n").unwrap();
        git(&["add", "-A"]);
        git(&[
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@e",
            "commit",
            "-qm",
            "first",
        ]);
        let first = git(&["rev-parse", "HEAD"]);

        let events = vec![committed(1, "build", &base, &first, &[first.as_str()])];
        // The window starts after seq 1: nothing counts.
        let s = scope_since(dir, tmp.path(), &events, 1, 0);
        assert_eq!(s.commits, 0);
        assert_eq!(s.files, Some(0));
        assert_eq!(s.lines_added, Some(0));
        // The first commit alone: 2 files, +2 -1.
        let s = scope_since(dir, tmp.path(), &events, 0, 0);
        assert_eq!(s.files, Some(2));
        assert_eq!(s.lines_added, pinned(dir, 2));
        assert_eq!(s.lines_removed, pinned(dir, 1));

        // The second commit adds a line to a.txt and a 3-line file: +4 -0.
        std::fs::write(dir.join("b.txt"), "x\ny\nz\n").unwrap();
        std::fs::write(dir.join("a.txt"), "uno\ndos\n").unwrap();
        git(&["add", "-A"]);
        git(&[
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@e",
            "commit",
            "-qm",
            "second",
        ]);
        let second = git(&["rev-parse", "HEAD"]);
        let events = vec![
            committed(1, "build", &base, &first, &[first.as_str()]),
            committed(2, "build", &first, &second, &[second.as_str()]),
        ];
        // The whole window, base..second: 2 commits, 3 files, +6 -1 lines.
        let s = scope_since(dir, tmp.path(), &events, 0, 0);
        assert_eq!(s.commits, 2);
        assert_eq!(s.files, Some(3));
        assert_eq!(s.lines_added, pinned(dir, 6));
        assert_eq!(s.lines_removed, pinned(dir, 1));
        // The window after the first commit, first..second: 1 commit,
        // 2 files, +4 -0 lines.
        let s = scope_since(dir, tmp.path(), &events, 1, 0);
        assert_eq!(s.commits, 1);
        assert_eq!(s.files, Some(2));
        assert_eq!(s.lines_added, pinned(dir, 4));
        assert_eq!(s.lines_removed, pinned(dir, 0));
    }

    /// A change a later commit takes back still counts: 3 files added,
    /// then removed, are 3 files and +3 -3 lines over 2 commits, never an
    /// empty scope from the endpoints alone.
    #[test]
    fn a_change_taken_back_still_counts() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        let git = |args: &[&str]| {
            let out = std::process::Command::new("git")
                .args([
                    "-c",
                    "user.name=t",
                    "-c",
                    "user.email=t@e",
                    "-c",
                    "commit.gpgsign=false",
                ])
                .args(args)
                .current_dir(dir)
                .output()
                .unwrap();
            assert!(out.status.success(), "git {args:?}");
            String::from_utf8(out.stdout).unwrap().trim().to_string()
        };
        git(&["init", "-q"]);
        git(&["commit", "-q", "--allow-empty", "-m", "base"]);
        let base = git(&["rev-parse", "HEAD"]);
        for name in ["x.txt", "y.txt", "z.txt"] {
            std::fs::write(dir.join(name), "one\n").unwrap();
        }
        git(&["add", "-A"]);
        git(&["commit", "-qm", "add"]);
        let added = git(&["rev-parse", "HEAD"]);
        git(&["rm", "-q", "x.txt", "y.txt", "z.txt"]);
        git(&["commit", "-qm", "remove"]);
        let removed = git(&["rev-parse", "HEAD"]);
        let events = vec![
            committed(1, "build", &base, &added, &[added.as_str()]),
            committed(2, "build", &added, &removed, &[removed.as_str()]),
        ];
        let s = scope_since(dir, dir, &events, 0, 0);
        assert_eq!(s.commits, 2);
        assert_eq!(s.files, Some(3));
        assert_eq!(s.lines_added, pinned(dir, 3));
        assert_eq!(s.lines_removed, pinned(dir, 3));
    }

    /// A small git repository for the history tests: a runner with a
    /// fixed identity and an empty base commit.
    /// `n` counted lines where the installed git keeps the work tree's
    /// attributes out of the diff (`apb_core::git::no_tree_attributes`),
    /// else unknown: a git older than 2.43 cannot count them honestly.
    fn pinned(dir: &std::path::Path, n: u32) -> Option<u32> {
        apb_core::git::no_tree_attributes(dir).map(|_| n)
    }

    fn repo(dir: &std::path::Path) -> impl Fn(&[&str]) -> String + '_ {
        move |args: &[&str]| {
            let out = std::process::Command::new("git")
                .args([
                    "-c",
                    "user.name=t",
                    "-c",
                    "user.email=t@e",
                    "-c",
                    "commit.gpgsign=false",
                ])
                .args(args)
                .current_dir(dir)
                .output()
                .unwrap();
            assert!(out.status.success(), "git {args:?}");
            String::from_utf8(out.stdout).unwrap().trim().to_string()
        }
    }

    /// History reset between two nodes: the first node's commit of 3
    /// files is no longer on the branch when the second node commits 1
    /// file from the base. The journal recorded both, so the scope counts
    /// both: 2 commits, 4 files, +4 lines, never the last range alone.
    #[test]
    fn a_reset_between_nodes_still_counts_every_recorded_commit() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        let git = repo(dir);
        git(&["init", "-q"]);
        git(&["commit", "-q", "--allow-empty", "-m", "base"]);
        let base = git(&["rev-parse", "HEAD"]);
        for name in ["x.txt", "y.txt", "z.txt"] {
            std::fs::write(dir.join(name), "one\n").unwrap();
        }
        git(&["add", "-A"]);
        git(&["commit", "-qm", "big"]);
        let big = git(&["rev-parse", "HEAD"]);
        // Another node resets the branch to the base (nothing recorded).
        git(&["reset", "-q", "--hard", &base]);
        std::fs::write(dir.join("w.txt"), "one\n").unwrap();
        git(&["add", "-A"]);
        git(&["commit", "-qm", "small"]);
        let small = git(&["rev-parse", "HEAD"]);
        let events = vec![
            committed(1, "build", &base, &big, &[big.as_str()]),
            committed(2, "fix", &base, &small, &[small.as_str()]),
        ];
        let s = scope_since(dir, dir, &events, 0, 0);
        assert_eq!(s.commits, 2);
        assert_eq!(s.files, Some(4));
        assert_eq!(s.lines_added, pinned(dir, 4));
        assert_eq!(s.lines_removed, pinned(dir, 0));
    }

    /// The one counting rule over real repositories: every recorded
    /// commit once, by its own change against its first parent (a root
    /// against the empty tree), files by path, lines summed, a binary file
    /// a file with 0 lines, a rename the two paths, anything git cannot
    /// show unknown. Each case builds its history and the events the
    /// writer would journal, and states (commits, files, +lines, -lines).
    #[test]
    fn every_recorded_commit_counts_by_its_own_change() {
        type Case = (
            &'static str,
            fn(&dyn Fn(&[&str]) -> String, &std::path::Path) -> Vec<Event>,
            (u32, Option<u32>, Option<u32>, Option<u32>),
        );
        fn head(git: &dyn Fn(&[&str]) -> String) -> String {
            git(&["rev-parse", "HEAD"])
        }
        fn put(dir: &std::path::Path, name: &str, body: &[u8]) {
            std::fs::write(dir.join(name), body).unwrap();
        }
        fn commit(git: &dyn Fn(&[&str]) -> String, msg: &str) -> String {
            git(&["add", "-A"]);
            git(&["commit", "-q", "--allow-empty", "-m", msg]);
            head(git)
        }
        fn shas(git: &dyn Fn(&[&str]) -> String, range: &str) -> Vec<String> {
            git(&["rev-list", range])
                .lines()
                .map(str::to_string)
                .collect()
        }
        fn recorded(
            git: &dyn Fn(&[&str]) -> String,
            seq: u64,
            node: &str,
            before: &str,
            after: &str,
        ) -> Event {
            let listed = shas(git, &format!("{before}..{after}"));
            let refs: Vec<&str> = listed.iter().map(String::as_str).collect();
            committed(seq, node, before, after, &refs)
        }
        let cases: Vec<Case> = vec![
            (
                "linear commits",
                |git, dir| {
                    let base = head(git);
                    put(dir, "a.txt", b"1\n2\n");
                    commit(git, "one");
                    put(dir, "a.txt", b"1\n3\n");
                    put(dir, "b.txt", b"1\n");
                    let after = commit(git, "two");
                    vec![recorded(git, 1, "build", &base, &after)]
                },
                (2, Some(2), Some(4), Some(1)),
            ),
            (
                "a revert pair",
                |git, dir| {
                    let base = head(git);
                    put(dir, "a.txt", b"1\n2\n");
                    let one = commit(git, "add");
                    git(&["revert", "--no-edit", &one]);
                    let after = head(git);
                    vec![recorded(git, 1, "build", &base, &after)]
                },
                (2, Some(1), Some(2), Some(2)),
            ),
            (
                "a reset between nodes",
                |git, dir| {
                    let base = head(git);
                    put(dir, "x.txt", b"1\n");
                    put(dir, "y.txt", b"1\n");
                    let big = commit(git, "big");
                    let first = recorded(git, 1, "build", &base, &big);
                    git(&["reset", "-q", "--hard", &base]);
                    put(dir, "w.txt", b"1\n");
                    let small = commit(git, "small");
                    vec![first, recorded(git, 2, "fix", &base, &small)]
                },
                (2, Some(3), Some(3), Some(0)),
            ),
            (
                "a merge with changes on the side branch",
                |git, dir| {
                    let base = head(git);
                    git(&["checkout", "-q", "-b", "side"]);
                    put(dir, "s.txt", b"1\n2\n");
                    commit(git, "side");
                    git(&["checkout", "-q", "main"]);
                    git(&["merge", "-q", "--no-ff", "--no-edit", "side"]);
                    let after = head(git);
                    vec![recorded(git, 1, "build", &base, &after)]
                },
                (2, Some(1), Some(4), Some(0)),
            ),
            (
                "a merge with changes only in the merge commit",
                |git, dir| {
                    let base = head(git);
                    git(&["checkout", "-q", "-b", "side"]);
                    commit(git, "empty side");
                    git(&["checkout", "-q", "main"]);
                    git(&["merge", "-q", "--no-ff", "--no-commit", "side"]);
                    for name in ["m1.txt", "m2.txt", "m3.txt"] {
                        put(dir, name, b"1\n");
                    }
                    let after = commit(git, "merge");
                    vec![recorded(git, 1, "build", &base, &after)]
                },
                (2, Some(3), Some(3), Some(0)),
            ),
            (
                "an octopus merge",
                |git, dir| {
                    let base = head(git);
                    for (branch, name) in [("b1", "x.txt"), ("b2", "y.txt")] {
                        git(&["checkout", "-q", "-b", branch, &base]);
                        put(dir, name, b"1\n");
                        commit(git, branch);
                    }
                    // A commit of main's own, so the merge has three parents
                    // and main is the first.
                    git(&["checkout", "-q", "main"]);
                    put(dir, "z.txt", b"1\n");
                    commit(git, "main");
                    git(&["merge", "-q", "--no-edit", "b1", "b2"]);
                    let after = head(git);
                    assert_eq!(
                        git(&["rev-list", "--parents", "-n1", "HEAD"])
                            .split(' ')
                            .count(),
                        4
                    );
                    vec![recorded(git, 1, "build", &base, &after)]
                },
                (4, Some(3), Some(5), Some(0)),
            ),
            (
                "a root commit merged in",
                |git, dir| {
                    let base = head(git);
                    git(&["checkout", "-q", "--orphan", "other"]);
                    git(&["rm", "-rq", "--cached", "--ignore-unmatch", "."]);
                    put(dir, "r.txt", b"1\n");
                    git(&["add", "r.txt"]);
                    git(&["commit", "-qm", "root"]);
                    git(&["checkout", "-q", "-f", "main"]);
                    git(&[
                        "merge",
                        "-q",
                        "--no-edit",
                        "--allow-unrelated-histories",
                        "other",
                    ]);
                    let after = head(git);
                    vec![recorded(git, 1, "build", &base, &after)]
                },
                (2, Some(1), Some(2), Some(0)),
            ),
            (
                "a binary file",
                |git, dir| {
                    let base = head(git);
                    put(dir, "blob.bin", &[0u8, 1, 2, 0, 255]);
                    put(dir, "a.txt", b"1\n");
                    let after = commit(git, "bin");
                    vec![recorded(git, 1, "build", &base, &after)]
                },
                (1, Some(2), Some(1), Some(0)),
            ),
            (
                "a rename",
                |git, dir| {
                    put(dir, "old.txt", b"1\n2\n3\n");
                    let base = commit(git, "old");
                    // Rename detection asked for by the repository's own config
                    // still counts the two paths git stores.
                    git(&["config", "diff.renames", "copies"]);
                    git(&["mv", "old.txt", "new.txt"]);
                    let after = commit(git, "rename");
                    vec![recorded(git, 1, "build", &base, &after)]
                },
                (1, Some(2), Some(3), Some(3)),
            ),
            (
                "a commit git can no longer show",
                |git, dir| {
                    let base = head(git);
                    put(dir, "a.txt", b"1\n");
                    let after = commit(git, "gone");
                    let event = recorded(git, 1, "build", &base, &after);
                    git(&["reset", "-q", "--hard", &base]);
                    git(&["reflog", "expire", "--expire=now", "--all"]);
                    git(&["gc", "-q", "--prune=now"]);
                    vec![event]
                },
                (1, None, None, None),
            ),
        ];
        for (name, build, (commits, files, added, removed)) in cases {
            let tmp = tempfile::tempdir().unwrap();
            let dir = tmp.path();
            let git = repo(dir);
            git(&["init", "-q", "-b", "main"]);
            git(&["commit", "-q", "--allow-empty", "-m", "base"]);
            let events = build(&git, dir);
            let s = scope_since(dir, dir, &events, 0, 0);
            // Counted lines need a git that keeps the work tree's
            // attributes out; without one they are unknown.
            let (added, removed) = match (added, removed) {
                (Some(a), Some(r)) if commits > 0 => (pinned(dir, a), pinned(dir, r)),
                other => other,
            };
            assert_eq!(
                (s.commits, s.files, s.lines_added, s.lines_removed),
                (commits, files, added, removed),
                "{name}"
            );
        }
    }

    /// A commit two events both recorded counts once, its files once and
    /// its lines once.
    #[test]
    fn a_commit_recorded_twice_counts_once() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        let git = repo(dir);
        git(&["init", "-q"]);
        git(&["commit", "-q", "--allow-empty", "-m", "base"]);
        let base = git(&["rev-parse", "HEAD"]);
        std::fs::write(dir.join("a.txt"), "one\ntwo\n").unwrap();
        git(&["add", "-A"]);
        git(&["commit", "-qm", "first"]);
        let first = git(&["rev-parse", "HEAD"]);
        std::fs::write(dir.join("a.txt"), "one\n").unwrap();
        git(&["add", "-A"]);
        git(&["commit", "-qm", "second"]);
        let second = git(&["rev-parse", "HEAD"]);
        let events = vec![
            committed(1, "build", &base, &first, &[first.as_str()]),
            // An answer round that recorded from the parked base again.
            committed(
                2,
                "build",
                &base,
                &second,
                &[second.as_str(), first.as_str()],
            ),
        ];
        let s = scope_since(dir, dir, &events, 0, 0);
        assert_eq!(s.commits, 2);
        assert_eq!(s.files, Some(1));
        assert_eq!(s.lines_added, pinned(dir, 2));
        assert_eq!(s.lines_removed, pinned(dir, 1));
    }

    /// A recorded commit git no longer shows in its recorded range (the
    /// history was rewritten, the object pruned) makes the count unknown,
    /// never a smaller number.
    #[test]
    fn a_recorded_commit_git_cannot_show_is_not_read() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        let git = repo(dir);
        git(&["init", "-q"]);
        git(&["commit", "-q", "--allow-empty", "-m", "base"]);
        let base = git(&["rev-parse", "HEAD"]);
        std::fs::write(dir.join("a.txt"), "one\n").unwrap();
        git(&["add", "-A"]);
        git(&["commit", "-qm", "first"]);
        let first = git(&["rev-parse", "HEAD"]);
        let gone = "0123456789abcdef0123456789abcdef01234567";
        let events = vec![committed(
            1,
            "build",
            &base,
            &first,
            &[first.as_str(), gone],
        )];
        let s = scope_since(dir, dir, &events, 0, 0);
        assert_eq!(s.commits, 2);
        assert_eq!(s.files, None);
        assert_eq!(s.lines_added, None);
        assert_eq!(s.lines_removed, None);
    }

    /// A range git cannot read is not read: never 0.
    #[test]
    fn an_unreadable_range_is_not_read() {
        let tmp = tempfile::tempdir().unwrap();
        let events = vec![committed(
            1,
            "build",
            "deadbeefcafe",
            "not-a-commit",
            &["x"],
        )];
        let s = scope_since(tmp.path(), tmp.path(), &events, 0, 0);
        assert_eq!(s.commits, 1);
        assert_eq!(s.files, None);
        assert_eq!(s.lines_added, None);
        assert_eq!(s.lines_removed, None);
    }

    /// apb's own notifications are neither writes nor sends.
    #[test]
    fn notify_sends_are_left_out() {
        let tmp = tempfile::tempdir().unwrap();
        let site = apb_core::connector::sites::NOTIFY_SITE;
        let events = vec![
            call_event(1, site, "ok"),
            intent_event(2, "i1", site),
            call_event(3, "work", "ok"),
        ];
        let s = scope_since(tmp.path(), tmp.path(), &events, 0, 0);
        assert_eq!(s.writes, 1);
        assert_eq!(s.sends, 0);
        assert_eq!(s.sends_open, 0);
    }

    /// The fast floor grows with the scope and is clamped at 5 s and
    /// 300 s; an unknown count reads as 0.
    #[test]
    fn the_fast_floor_grows_with_the_scope() {
        assert_eq!(fast_floor_ms(&ReviewScope::default()), 5_000);
        let big = ReviewScope {
            files: Some(400),
            ..Default::default()
        };
        assert_eq!(fast_floor_ms(&big), 300_000);
        let s = ReviewScope {
            files: Some(2),
            lines_added: Some(30),
            lines_removed: Some(10),
            writes: 1,
            sends: 2,
            items_settled: 3,
            ..Default::default()
        };
        // 5000 + 2*1000 + 40*50 + 3*2000 + 3*1000 = 18000
        assert_eq!(fast_floor_ms(&s), 18_000);
        let unknown = ReviewScope {
            files: None,
            lines_added: None,
            lines_removed: None,
            ..s
        };
        // 5000 + 0 + 0 + 3*2000 + 3*1000 = 14000
        assert_eq!(fast_floor_ms(&unknown), 14_000);
    }

    /// The one scope line, its singular forms, an unread line count and
    /// the empty scope.
    #[test]
    fn the_scope_line_reads_like_the_docs() {
        let s = ReviewScope {
            commits: 3,
            files: Some(6),
            lines_added: Some(41),
            lines_removed: Some(38),
            items_settled: 12,
            writes: 2,
            writes_unknown: 1,
            sends: 3,
            sends_open: 1,
            results: 1,
            ..Default::default()
        };
        assert_eq!(
            scope_line(&s),
            "3 commits, 6 files, +41 -38 lines, 12 list items, 2 connector writes (1 unknown), 3 sends (1 open), 1 result"
        );
        let one = ReviewScope {
            commits: 1,
            files: Some(1),
            lines_added: Some(3),
            lines_removed: Some(0),
            items_settled: 1,
            writes: 1,
            sends: 1,
            results: 1,
            ..Default::default()
        };
        assert_eq!(
            scope_line(&one),
            "1 commit, 1 file, +3 -0 lines, 1 list item, 1 connector write, 1 send, 1 result"
        );
        let unread = ReviewScope {
            commits: 2,
            ..Default::default()
        };
        assert_eq!(
            scope_line(&unread),
            "2 commits, files not read, lines not read"
        );
        // Each unknown count is named for what it is.
        let files_only = ReviewScope {
            commits: 1,
            lines_added: Some(2),
            lines_removed: Some(0),
            ..Default::default()
        };
        assert_eq!(
            scope_line(&files_only),
            "1 commit, files not read, +2 -0 lines"
        );
        assert_eq!(
            scope_line(&ReviewScope::default()),
            "nothing changed since the last decision"
        );
    }

    /// A limit on a count nobody read counts as passed.
    #[test]
    fn an_unknown_count_passes_a_limit() {
        let max = MaxScope {
            files: Some(5),
            lines: None,
            writes: None,
            withhold: Some(vec!["approve".into()]),
        };
        let s = ReviewScope {
            commits: 1,
            ..Default::default()
        };
        let (w, reason) = withheld(&max, &s, &["approve".to_string(), "reject".to_string()]);
        assert_eq!(w, vec!["approve"]);
        assert_eq!(
            reason.unwrap(),
            "the scope (files not read) is above max_scope (files 5); split the work or choose another option"
        );
        // Both limits on unread counts: each named for its own count.
        let both = MaxScope {
            lines: Some(10),
            ..max.clone()
        };
        let (_, reason) = withheld(&both, &s, &["approve".to_string()]);
        assert_eq!(
            reason.unwrap(),
            "the scope (files not read, lines not read) is above max_scope (files 5, lines 10); split the work or choose another option"
        );
    }

    /// The reason names the counts that passed and the limits they
    /// passed, and withholds only options the gate offers.
    #[test]
    fn withheld_names_the_counts_and_the_limits() {
        let max = MaxScope {
            files: Some(5),
            lines: Some(500),
            writes: None,
            withhold: Some(vec!["approve".into(), "ship".into()]),
        };
        let s = ReviewScope {
            commits: 3,
            files: Some(6),
            lines_added: Some(500),
            lines_removed: Some(400),
            ..Default::default()
        };
        let (w, reason) = withheld(&max, &s, &["approve".to_string(), "reject".to_string()]);
        assert_eq!(w, vec!["approve"]);
        assert_eq!(
            reason.unwrap(),
            "the scope (6 files, 900 lines) is above max_scope (files 5, lines 500); split the work or choose another option"
        );
        // Below every limit: nothing withheld.
        let small = ReviewScope {
            commits: 1,
            files: Some(2),
            lines_added: Some(10),
            lines_removed: Some(0),
            ..Default::default()
        };
        assert_eq!(
            withheld(&max, &small, &["approve".to_string()]),
            (vec![], None)
        );
    }

    fn call_event(seq: u64, node: &str, outcome: &str) -> Event {
        ev(
            seq,
            EventPayload::ConnectorCall {
                node_id: node.into(),
                connector: "mail".into(),
                function: "send".into(),
                outcome: outcome.into(),
                account: String::new(),
                url: String::new(),
                http_status: None,
                duration_ms: 0,
                smtp_subject: None,
                smtp_recipients: None,
                response_complete: None,
                pages: None,
                attempt: None,
                attempt_seq: None,
                intent: None,
                origin: None,
                item: None,
            },
        )
    }

    fn intent_event(seq: u64, intent: &str, node: &str) -> Event {
        ev(
            seq,
            EventPayload::SendIntentRecorded {
                intent: intent.into(),
                node: node.into(),
                nonce: String::new(),
                attempt: None,
                attempt_seq: None,
                connector: "mail".into(),
                function: "send".into(),
                account: String::new(),
                mode: String::new(),
                label: String::new(),
                pid: 0,
                deadline_ms: 0,
                replay_of: None,
                resend_of: None,
                item: None,
                target_digest: None,
            },
        )
    }
}
