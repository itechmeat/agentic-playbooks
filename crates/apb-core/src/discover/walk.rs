//! The bounded walk of one history root: which session files of the
//! selected projects are read, and how far.
//!
//! - A root must be a real directory: a symlinked root is refused.
//! - Entries are listed with `symlink_metadata`; a symlink or a non-regular
//!   file is skipped, never followed. Only `.jsonl` files are read, at
//!   depth 2 for claude (`<encoded cwd>/<session>.jsonl`, main sessions
//!   only) and depth 4 for codex (`YYYY/MM/DD/rollout-*.jsonl`).
//! - A file older than `since_days` (by its modification time, and for
//!   codex also by the date in its path) is not read.
//! - A session belongs to the deepest registered project its `cwd` is in;
//!   it is read only when that project is a selected one, so a project
//!   registered inside a selected one keeps its sessions to itself.
//! - A claude directory is read only when its name is a selected project's
//!   encoded root (or one below it), confirmed by the session's own `cwd`;
//!   a codex file only when the `cwd` its first 64 KiB name belongs to a
//!   selected project, else it is closed right after that header (its
//!   header bytes count toward the byte cap, the file not toward the file
//!   cap).
//! - Files are opened without following a link
//!   ([`crate::fsutil::open_regular_no_follow`]) and read by their prefix
//!   ([`crate::fsutil::read_prefix_capped`]); the candidates of every root
//!   together, newest first, within the file, byte and step caps of the
//!   whole discovery.

use std::fs::Metadata;
use std::path::{Path, PathBuf};

use super::session::{self, Session};
use super::types::{
    DiscoverError, DiscoverRequest, HistoryAgent, HistoryRoot, ProjectRef, ReadLimits, RootReport,
};
use crate::fsutil::{open_regular_no_follow, read_prefix_capped};

const MS_PER_DAY: u64 = 24 * 60 * 60 * 1000;

/// How much of a codex rollout is read to find its working directory.
pub const CODEX_HEADER_BYTES: u64 = 64 * 1024;

/// What the whole discovery has read so far, across roots.
#[derive(Debug, Default)]
struct Budget {
    files: u32,
    bytes: u64,
    steps: u32,
}

/// Refuses a root that is a symbolic link or not a directory.
pub fn check_root(root: &HistoryRoot) -> Result<(), DiscoverError> {
    match std::fs::symlink_metadata(&root.path) {
        Ok(m) if m.file_type().is_symlink() => Err(DiscoverError::RootIsSymlink(root.name.clone())),
        Ok(m) if m.is_dir() => Ok(()),
        _ => Err(DiscoverError::RootMissing(root.name.clone())),
    }
}

/// One session file to read.
struct Candidate {
    /// The index of its root in the request.
    root: usize,
    path: PathBuf,
    /// The project it belongs to, when the directory says so (claude).
    project: Option<usize>,
    file_name: String,
    mtime_ms: u64,
}

fn mtime_ms(m: &Metadata) -> u64 {
    m.modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map_or(0, |d| d.as_millis() as u64)
}

/// The entries of `dir` with their own metadata (links not followed),
/// sorted by name. A directory that does not list is empty.
fn entries(dir: &Path) -> Vec<(String, PathBuf, Metadata)> {
    let mut out: Vec<(String, PathBuf, Metadata)> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .filter_map(Result::ok)
        .filter_map(|e| {
            let path = e.path();
            let meta = std::fs::symlink_metadata(&path).ok()?;
            Some((e.file_name().to_string_lossy().into_owned(), path, meta))
        })
        .collect();
    out.sort_by(|a, b| a.0.cmp(&b.0));
    out
}

/// Who a working directory belongs to: the selected projects and every
/// registered project root.
struct Owners<'a> {
    projects: &'a [ProjectRef],
    registered: &'a [PathBuf],
}

impl Owners<'_> {
    /// The selected project a working directory belongs to: the deepest
    /// project root it is in, when no registered project that was not
    /// selected lies deeper.
    fn of_cwd(&self, cwd: &str) -> Option<usize> {
        let cwd = Path::new(cwd);
        let depth = |root: &Path| root.as_os_str().len();
        let (i, selected) = self
            .projects
            .iter()
            .enumerate()
            .filter(|(_, p)| cwd.starts_with(&p.root))
            .map(|(i, p)| (i, depth(&p.root)))
            .max_by_key(|(_, d)| *d)?;
        let deeper = self
            .registered
            .iter()
            .any(|r| cwd.starts_with(r) && depth(r) > selected);
        (!deeper).then_some(i)
    }
}

/// The selected project a claude directory name belongs to: the one whose
/// encoded root it is, or starts with followed by `-` (a directory below
/// the root); the longest such root wins.
fn project_of_dir(name: &str, projects: &[ProjectRef]) -> Option<usize> {
    projects
        .iter()
        .enumerate()
        .filter_map(|(i, p)| {
            let enc = crate::agent_actions::claude::encode_cwd(&p.root.to_string_lossy());
            (name == enc || name.strip_prefix(&enc).is_some_and(|r| r.starts_with('-')))
                .then_some((i, enc.len()))
        })
        .max_by_key(|(_, len)| *len)
        .map(|(i, _)| i)
}

/// A session file entry: a regular `.jsonl` file, or why it is skipped.
/// Directories and other files are not session files (`None`).
fn session_file(name: &str, meta: &Metadata, report: &mut RootReport) -> bool {
    if !name.ends_with(".jsonl") {
        return false;
    }
    report.files_seen += 1;
    if meta.file_type().is_symlink() {
        report.skip("symlink");
        false
    } else if !meta.is_file() {
        report.skip("not_regular");
        false
    } else {
        true
    }
}

/// The session files of a claude root: `<encoded cwd>/<session>.jsonl`.
fn claude_candidates(
    index: usize,
    root: &Path,
    projects: &[ProjectRef],
    cutoff_ms: u64,
    report: &mut RootReport,
) -> Vec<Candidate> {
    let mut out = Vec::new();
    for (dir_name, dir, meta) in entries(root) {
        if meta.file_type().is_symlink() {
            report.skip("symlink");
            continue;
        }
        if !meta.is_dir() {
            continue;
        }
        let project = project_of_dir(&dir_name, projects);
        for (name, path, meta) in entries(&dir) {
            if !session_file(&name, &meta, report) {
                continue;
            }
            if project.is_none() {
                report.skip("other_project");
            } else if mtime_ms(&meta) < cutoff_ms {
                report.skip("too_old");
            } else {
                out.push(Candidate {
                    root: index,
                    path,
                    project,
                    file_name: name,
                    mtime_ms: mtime_ms(&meta),
                });
            }
        }
    }
    out
}

/// A `YYYY`, `MM` or `DD` directory name as a number.
fn date_part(name: &str, digits: usize) -> Option<u32> {
    (name.len() == digits && name.chars().all(|c| c.is_ascii_digit()))
        .then(|| name.parse().ok())
        .flatten()
}

/// Days since the Unix epoch of a civil date (proleptic Gregorian).
fn days_from_civil(y: i64, m: u32, d: u32) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = (i64::from(m) + 9) % 12;
    let doy = (153 * mp + 2) / 5 + i64::from(d) - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// The session files of a codex root: `YYYY/MM/DD/rollout-*.jsonl`.
fn codex_candidates(
    index: usize,
    root: &Path,
    cutoff_ms: u64,
    report: &mut RootReport,
) -> Vec<Candidate> {
    let cutoff_day = (cutoff_ms / MS_PER_DAY) as i64;
    let mut out = Vec::new();
    let dirs = |dir: &Path, digits: usize, report: &mut RootReport| -> Vec<(u32, PathBuf)> {
        entries(dir)
            .into_iter()
            .filter_map(|(name, path, meta)| {
                if meta.file_type().is_symlink() {
                    report.skip("symlink");
                    return None;
                }
                let n = date_part(&name, digits)?;
                meta.is_dir().then_some((n, path))
            })
            .collect()
    };
    for (y, year) in dirs(root, 4, report) {
        for (m, month) in dirs(&year, 2, report) {
            for (d, day) in dirs(&month, 2, report) {
                let dated_old = days_from_civil(i64::from(y), m, d) < cutoff_day;
                for (name, path, meta) in entries(&day) {
                    if !name.starts_with("rollout-") || !session_file(&name, &meta, report) {
                        continue;
                    }
                    if dated_old || mtime_ms(&meta) < cutoff_ms {
                        report.skip("too_old");
                        continue;
                    }
                    out.push(Candidate {
                        root: index,
                        path,
                        project: None,
                        file_name: name,
                        mtime_ms: mtime_ms(&meta),
                    });
                }
            }
        }
    }
    out
}

/// Reads the session files of `req.projects` under every root of `req`:
/// one report per root, in the request's order, and the sessions read.
/// The candidates of all roots are read together, newest first, so the
/// caps keep the most recent sessions whichever root holds them.
pub fn read_roots(req: &DiscoverRequest, now_ms: u64) -> (Vec<RootReport>, Vec<Session>) {
    let limits = &req.limits;
    let cutoff_ms = now_ms.saturating_sub(u64::from(limits.since_days) * MS_PER_DAY);
    let mut reports = Vec::new();
    let mut candidates = Vec::new();
    for (i, root) in req.roots.iter().enumerate() {
        let mut report = RootReport {
            name: root.name.clone(),
            agent: root.agent.as_str().to_string(),
            ..RootReport::default()
        };
        candidates.extend(match root.agent {
            HistoryAgent::ClaudeCode => {
                claude_candidates(i, &root.path, &req.projects, cutoff_ms, &mut report)
            }
            HistoryAgent::Codex => codex_candidates(i, &root.path, cutoff_ms, &mut report),
        });
        reports.push(report);
    }
    candidates.sort_by(|a, b| {
        b.mtime_ms
            .cmp(&a.mtime_ms)
            .then_with(|| a.path.cmp(&b.path))
    });
    let owners = Owners {
        projects: &req.projects,
        registered: &req.registered,
    };
    let mut budget = Budget::default();
    let mut sessions = Vec::new();
    for c in candidates {
        let report = &mut reports[c.root];
        let cap = if budget.files >= limits.max_files {
            Some("file_cap")
        } else if budget.bytes >= limits.max_total_bytes {
            Some("byte_cap")
        } else if budget.steps >= limits.max_total_steps {
            Some("step_cap")
        } else {
            None
        };
        if let Some(why) = cap {
            report.skip(why);
            continue;
        }
        let agent = req.roots[c.root].agent;
        if let Some(s) = read_one(agent, &c, &owners, limits, &mut budget, report) {
            sessions.push(s);
        }
    }
    (reports, sessions)
}

/// Reads one candidate; `None` when it is not read or holds no step.
fn read_one(
    agent: HistoryAgent,
    c: &Candidate,
    owners: &Owners<'_>,
    limits: &ReadLimits,
    budget: &mut Budget,
    report: &mut RootReport,
) -> Option<Session> {
    let Ok(mut file) = open_regular_no_follow(&c.path) else {
        report.skip("unreadable");
        return None;
    };
    let cap = limits
        .max_file_bytes
        .min(limits.max_total_bytes - budget.bytes);
    let mut bytes = Vec::new();
    let mut cut = false;
    let mut project = c.project;
    if agent == HistoryAgent::Codex {
        let Ok((header, more)) = read_prefix_capped(&mut file, cap.min(CODEX_HEADER_BYTES)) else {
            report.skip("unreadable");
            return None;
        };
        count(budget, report, header.len());
        project = session::codex_header_cwd(&header).and_then(|cwd| owners.of_cwd(&cwd));
        if project.is_none() {
            report.skip("other_project");
            return None;
        }
        bytes = header;
        cut = more;
    }
    // A codex file of another project, closed after its header, does not
    // count toward the file cap; a claude file is read whole before its
    // own `cwd` can confirm it, so it does.
    budget.files += 1;
    report.files_read += 1;
    if bytes.is_empty() || cut {
        let left = cap - bytes.len() as u64;
        let Ok((rest, more)) = read_prefix_capped(&mut file, left) else {
            report.skip("unreadable");
            return None;
        };
        count(budget, report, rest.len());
        bytes.extend(rest);
        cut = more;
    }
    let left = limits.max_total_steps - budget.steps;
    let max_steps = limits.max_steps_per_session.min(left) as usize;
    let parsed = session::parse(
        agent,
        &bytes,
        limits.max_line_bytes,
        max_steps,
        &mut report.skipped,
    );
    drop(bytes);
    // A session the step cap stopped part way is read in part too.
    let step_cut = left < limits.max_steps_per_session && parsed.steps.len() >= left as usize;
    if cut || step_cut {
        report.cut_files += 1;
    }
    // A claude directory is confirmed by the session's own working
    // directory: a sibling project whose encoded name shares the prefix,
    // or a registered project inside the selected one, is another project.
    if agent == HistoryAgent::ClaudeCode {
        let confirmed = parsed.cwd.as_deref().and_then(|cwd| owners.of_cwd(cwd));
        if confirmed.is_none() || confirmed != project {
            report.skip("other_project");
            return None;
        }
    }
    let project = project?;
    if parsed.steps.is_empty() {
        return None;
    }
    budget.steps += parsed.steps.len() as u32;
    let p = &owners.projects[project];
    Some(Session {
        root: c.root,
        rel: format!("{}/{}", p.name, c.file_name),
        project,
        day: (c.mtime_ms / MS_PER_DAY) as i64,
        steps: parsed.steps,
        requests: parsed.requests,
    })
}

fn count(budget: &mut Budget, report: &mut RootReport, n: usize) {
    budget.bytes += n as u64;
    report.bytes_read += n as u64;
}

/// Where the session file a [`super::SourceSpan`] names lies under `root`,
/// found by its file name at the depth the agent keeps sessions at, never
/// through a link. For a person at their own terminal only (`apb discover
/// show --paths`): no other surface ever names an absolute path.
pub fn locate(root: &HistoryRoot, span_file: &str) -> Option<PathBuf> {
    let name = span_file
        .rsplit('/')
        .next()
        .filter(|n| n.ends_with(".jsonl"))?;
    check_root(root).ok()?;
    let depth = match root.agent {
        HistoryAgent::ClaudeCode => 2,
        HistoryAgent::Codex => 4,
    };
    find_file(&root.path, name, depth)
}

fn find_file(dir: &Path, name: &str, depth: usize) -> Option<PathBuf> {
    let list = entries(dir);
    if depth == 1 {
        return list
            .into_iter()
            .find(|(n, _, m)| n == name && m.is_file())
            .map(|(_, p, _)| p);
    }
    list.into_iter()
        .filter(|(_, _, m)| m.is_dir())
        .find_map(|(_, p, _)| find_file(&p, name, depth - 1))
}
