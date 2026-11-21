//! A `project_checks` source: the project's own commands, run in the run's
//! tree, each with a fixed observation pair and the fixed rule "no new
//! failures":
//!
//! - `<name>.status`: `passed` or `failed`;
//! - `<name>.failed_tests`: the failing tests, parsed only for `cargo test`
//!   (`test x ... FAILED`), each named by the test binary it ran in
//!   (`unittests src/lib.rs [crate]: tests::it_works`; a doctest without its
//!   `(line N)`, which moves with every edit above it). The list is read
//!   only when every binary cargo announced printed its `test result:`
//!   line; a run cut short, or more than [`MAX_LIST_ITEMS`] failing tests,
//!   reads unknown.
//!
//! A command that passed before must pass after. One that failed before
//! may fail after only with a subset of its failing tests; when either side
//! could not list them, the comparison is unknown. With `junit`, the
//! failing tests of the other `test` commands come from the JUnit XML
//! reports the glob matches that the command wrote; without it, runners
//! other than `cargo test` read unknown while they keep failing.
//!
//! Each command runs without the connector secrets in its environment, and
//! must leave the working tree as it found it (see `ctx.rs`); after an abort
//! or a `--now` pause the remaining commands are not started.

use std::collections::BTreeMap;
use std::path::Path;
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant, SystemTime};

use apb_core::baseline::extract::checked_text;
use apb_core::baseline::{Comparison, MAX_LIST_ITEMS, Observation, ObservedValue, Verdict};
use apb_core::content::sha256_hex;
use apb_core::fsutil::beneath::{Beneath, CappedRead, EntryKind, ListBudget, ReadRefusal};
use apb_core::project_checks::{
    FailingTest, MAX_REPORT_BYTES, MAX_REPORT_FILES, ProjectChecksSpec, ProjectCommand,
    TOO_MANY_FAILING, discover, failed_tests_within, is_wildcard, junit_items, matcher,
    render_item, source_digest,
};
use apb_core::text::strip_ansi;

use super::ctx::{ReadCtx, unknown};
use super::{Snapshot, status};

/// The rule every project command is compared under.
pub(crate) const RULE: &str = "no_new_failures";
const PASSED: &str = "passed";
const FAILED: &str = "failed";
/// The `tests` reason of a failed `junit` command whose reports name no
/// failing test.
const UNEXPLAINED_FAILURE: &str =
    "the command failed, but the JUnit reports it wrote name no failing test";
/// How many new failing tests a detail names.
const NAMED_TESTS: usize = 10;

pub(crate) fn status_key(cmd: &ProjectCommand) -> String {
    format!("{}.status", cmd.name)
}

fn tests_key(cmd: &ProjectCommand) -> String {
    format!("{}.failed_tests", cmd.name)
}

/// What the input of a project check is: its commands, the files they
/// are defined in, and the JUnit report glob. Without one the digest is
/// exactly what it hashed before, so an existing run's before snapshot
/// keeps comparing.
fn input_digest(ctx: &ReadCtx<'_>, commands: &[ProjectCommand], junit: Option<&str>) -> String {
    let argv = serde_json::to_string(commands).unwrap_or_default();
    let defined = source_digest(ctx.workdir, commands);
    let body = match junit {
        Some(glob) => format!("{argv}\n{defined}\njunit={glob}"),
        None => format!("{argv}\n{defined}"),
    };
    sha256_hex(body.as_bytes())
}

/// The test binary a cargo `Running`/`Doc-tests` header announces:
/// `unittests src/lib.rs [crate]`, `tests/api.rs [api]`, `doctests crate`.
fn binary_of(header: &str) -> Option<String> {
    let h = header.trim();
    if let Some(name) = h.strip_prefix("Doc-tests ") {
        return Some(format!("doctests {}", name.trim()));
    }
    let rest = h.strip_prefix("Running ")?;
    let (target, path) = match rest.rsplit_once(" (") {
        Some((t, p)) => (t, p.trim_end_matches(')')),
        None => (rest, ""),
    };
    let file = path.rsplit(['/', '\\']).next().unwrap_or("");
    // cargo names a test binary `<name>-<hash>`, with `.exe` on Windows;
    // the hash changes with the build, so it is not part of the key.
    let file = std::path::Path::new(file)
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or(file);
    let bin = match file.rsplit_once('-') {
        Some((name, hash)) if !hash.is_empty() && hash.chars().all(|c| c.is_ascii_hexdigit()) => {
            name
        }
        _ => file,
    };
    Some(if bin.is_empty() {
        target.to_string()
    } else {
        format!("{target} [{bin}]")
    })
}

/// `running 3 tests`, the line each test binary opens its stdout with.
fn opens_binary(line: &str) -> bool {
    line.strip_prefix("running ")
        .and_then(|r| r.strip_suffix(" tests").or_else(|| r.strip_suffix(" test")))
        .is_some_and(|n| !n.is_empty() && n.chars().all(|c| c.is_ascii_digit()))
}

/// The failing tests `cargo test` printed, each named by its binary, or
/// why the list cannot be read. cargo announces each binary on stderr and
/// the binary prints its tests on stdout, in the same order; the list is
/// whole only when every announced binary printed its `test result:` line.
/// Escape sequences are stripped first, should a command color its output
/// anyway.
fn cargo_failed_tests(stdout: &str, stderr: &str) -> Result<Vec<String>, String> {
    let (stdout, stderr) = (strip_ansi(stdout), strip_ansi(stderr));
    let binaries: Vec<String> = stderr.lines().filter_map(binary_of).collect();
    let mut blocks: Vec<(bool, Vec<String>)> = Vec::new();
    for line in stdout.lines() {
        let l = line.trim();
        if opens_binary(l) {
            blocks.push((false, Vec::new()));
            continue;
        }
        let Some((done, failed)) = blocks.last_mut() else {
            continue;
        };
        if l.starts_with("test result:") {
            *done = true;
        } else if let Some(name) = l
            .strip_prefix("test ")
            .and_then(|t| t.strip_suffix(" ... FAILED"))
        {
            failed.push(name.to_string());
        }
    }
    if binaries.is_empty() || blocks.len() != binaries.len() || blocks.iter().any(|(d, _)| !*d) {
        return Err("cargo test did not run every test binary to its result".into());
    }
    let mut out: Vec<String> = Vec::new();
    for (bin, (_, failed)) in binaries.iter().zip(blocks) {
        for name in failed {
            let name = match name.rsplit_once(" (line ") {
                Some((head, tail)) if tail.ends_with(')') => head.to_string(),
                _ => name,
            };
            out.push(format!("{bin}: {name}"));
        }
    }
    out.sort();
    let out = numbered_repeats(out);
    if out.len() > MAX_LIST_ITEMS {
        return Err(format!(
            "{} failing tests, more than the {MAX_LIST_ITEMS} an observation keeps",
            out.len()
        ));
    }
    Ok(out)
}

/// A sorted list of `cargo test` items with every repeat of a name
/// numbered: several doctests failing on one item share a name once their
/// line is dropped, and one more failure there is still one more item. The
/// numbering is injective, so a test literally named `x #2` never reads
/// as the second `x`: an item is `<name>` for a first occurrence whose
/// name does not itself end with ` #<digits>`, and `<name> #<n>` for
/// every other one (a first occurrence of such a name is `<name> #1`). An
/// item decodes back as its last ` #<digits>` suffix, or occurrence 1
/// without one. Every name an item had before keeps it, unless it ends
/// with ` #<digits>`, so a before snapshot an older apb journaled still
/// compares.
fn numbered_repeats(sorted: Vec<String>) -> Vec<String> {
    let mut out: Vec<String> = Vec::with_capacity(sorted.len());
    let mut seen = 0usize;
    for (i, name) in sorted.iter().enumerate() {
        seen = if i > 0 && sorted[i - 1] == *name {
            seen + 1
        } else {
            1
        };
        out.push(if seen == 1 && !ends_with_number(name) {
            name.clone()
        } else {
            format!("{name} #{seen}")
        });
    }
    out
}

/// Whether `s` ends with ` #<digits>`, the suffix [`numbered_repeats`]
/// numbers with.
fn ends_with_number(s: &str) -> bool {
    s.rsplit_once(" #")
        .is_some_and(|(_, n)| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
}

/// The scan limits of one command: both JUnit listings are charged to the
/// command's own deadline and stop when the run is cancelled; no time is
/// added for them.
struct ScanLimits<'a> {
    deadline: Instant,
    cancel: &'a AtomicBool,
}

impl ScanLimits<'_> {
    /// `Err` with why the scan may not continue: the run was cancelled, or
    /// the command's deadline passed. One read is at most
    /// [`MAX_REPORT_BYTES`], so the deadline is overrun by at most one
    /// capped read.
    fn check(&self) -> Result<(), String> {
        if self.cancel.load(Ordering::SeqCst) {
            return Err("cancelled".into());
        }
        if self.deadline <= Instant::now() {
            return Err("the JUnit scan ran out of time".into());
        }
        Ok(())
    }
}

/// What a JUnit report looked like at one listing: its length, its last
/// modification, and the content digest its capped read gave. The
/// comparison of before and after is made on these, never against the wall
/// clock (a file system stamps with a coarser clock than `SystemTime::now`).
#[derive(Debug)]
struct ReportStamp {
    modified: Option<SystemTime>,
    len: u64,
    digest: Option<String>,
}

/// The reports one listing found, or why the listing failed.
type ReportScan = Result<BTreeMap<String, ReportStamp>, String>;

/// What the after listing parses: the before listing it compares each
/// report with, and the failing items of every report the command wrote.
struct Parse<'a> {
    before: &'a BTreeMap<String, ReportStamp>,
    written: usize,
    /// The reports handed to the reader so far.
    parsed: usize,
    /// At most [`MAX_LIST_ITEMS`] items, each at most `MAX_ITEM_CHARS`
    /// characters: the next report may add only what is left, and one more
    /// ends the walk unknown, reading no further report.
    items: Vec<(String, FailingTest)>,
}

/// What a report walk collects: the matcher and the glob it came from,
/// the reports matched so far and, for the after listing, what it parses.
struct ReportWalk<'a, 'p> {
    matcher: &'a globset::GlobMatcher,
    glob: &'a str,
    out: &'a mut BTreeMap<String, ReportStamp>,
    parse: Option<&'a mut Parse<'p>>,
    /// [`match_depth`] of the glob.
    dirs: Option<usize>,
}

/// The longest literal directory prefix of the glob: the `/`-separated
/// components before the first component that holds a wildcard
/// ([`is_wildcard`], the set V320 reads components by), and never the last
/// component (the file name position). A glob of one component, literal or
/// not, gives the tree's root, but a wildcard one never walks (V320 refuses
/// it, and a `\` that could hide a component boundary), so the walk root is
/// always a literal directory.
fn walk_root(glob: &str) -> &str {
    let mut comps: Vec<(usize, usize)> = Vec::new();
    let mut start = 0usize;
    for (i, c) in glob.char_indices() {
        if c == '/' {
            comps.push((start, i));
            start = i + 1;
        }
    }
    comps.push((start, glob.len()));
    let last = comps.len() - 1;
    let first_wild = comps
        .iter()
        .position(|(s, e)| glob[*s..*e].chars().any(is_wildcard));
    let keep = first_wild.map_or(last, |w| w.min(last));
    let end = if keep == 0 { 0 } else { comps[keep - 1].1 };
    &glob[..end]
}

/// Every JUnit XML report the glob matches, keyed by its path from the
/// tree, each with its [`ReportStamp`]. A glob without a wildcard names
/// one file and is not walked; otherwise the walk starts at
/// [`walk_root`]'s prefix, descends only where a match can lie and at most
/// [`MAX_WALK_DEPTH`] directory levels below it (a match that could lie
/// deeper is the walk's error),
/// counts every examined entry against one [`ListBudget`], skips entries
/// of kind `Link` and `Other`, and digests at most [`MAX_REPORT_FILES`]
/// reports. Every file access goes through [`Beneath`], so a link is
/// never followed. With `parse` (the after listing), every report the
/// command wrote is parsed from the very bytes its digest came from.
fn junit_reports(
    tree: &Path,
    glob: &str,
    limits: &ScanLimits<'_>,
    parse: Option<&mut Parse<'_>>,
) -> Result<BTreeMap<String, ReportStamp>, String> {
    let matcher = matcher(glob).map_err(|e| e.to_string())?;
    let beneath = Beneath::open(tree).map_err(|e| e.to_string())?;
    let mut out = BTreeMap::new();
    let mut walk = ReportWalk {
        matcher: &matcher,
        glob,
        out: &mut out,
        parse,
        dirs: match_depth(glob, walk_root(glob)),
    };
    if !glob.chars().any(is_wildcard) {
        // One literal file, never walked: what the tree holds at that path
        // decides; a link or anything but a regular file there is a report
        // the reader cannot examine.
        match beneath.kind(glob).map_err(|e| e.to_string())? {
            Some(EntryKind::File) => stamp(&beneath, glob, &mut walk, limits)?,
            None => {}
            Some(_) => {
                return Err(format!(
                    "`{glob}` is a link or not a regular file; links are never followed"
                ));
            }
        }
        return Ok(out);
    }
    let root = walk_root(glob);
    if !root.is_empty() && beneath.kind(root).map_err(|e| e.to_string())?.is_none() {
        // A walk root that does not exist: no report matches.
        return Ok(out);
    }
    let mut budget = ListBudget { remaining: 10_000 };
    walk_reports(&beneath, root, 0, &mut walk, &mut budget, limits)?;
    Ok(out)
}

/// How many directory levels below the walk root a match can lie: a glob
/// without `**`, `{...}` or a character class has a fixed number of
/// components, so no match lies deeper; `None` for one that can match at
/// any depth. A class counts as unbounded because the matcher lets a
/// negated class such as `[!x]` match `/`, literal separator or not.
fn match_depth(glob: &str, root: &str) -> Option<usize> {
    if glob.contains("**") || glob.contains('{') || glob.contains('[') {
        return None;
    }
    let rest = glob[root.len()..].trim_start_matches('/');
    Some(rest.split('/').count().saturating_sub(1))
}

/// The deepest directory level below the walk root a walk examines.
const MAX_WALK_DEPTH: usize = 8;

/// The walk itself: lists `rel`, matches every regular file's path from
/// the tree against the walk's matcher, and descends into the directories
/// a match can lie in. The walk examines every path the glob can match or
/// fails: an exhausted listing, a directory past [`MAX_WALK_DEPTH`] a match
/// could lie in, a name that is not UTF-8, or a link or special file where
/// a report could lie is the walk's error, never a shorter list.
fn walk_reports(
    beneath: &Beneath,
    rel: &str,
    depth: usize,
    walk: &mut ReportWalk<'_, '_>,
    budget: &mut ListBudget,
    limits: &ScanLimits<'_>,
) -> Result<(), String> {
    limits.check()?;
    let listing = beneath.list_dir(rel, budget).map_err(|e| e.to_string())?;
    if listing.exhausted {
        return Err("the JUnit walk examined more than 10000 entries".into());
    }
    // Whether a file here may match, and whether a directory here may hold
    // a match.
    let file_here = walk.dirs.is_none_or(|n| depth == n);
    let below = walk.dirs.is_none_or(|n| depth < n);
    for entry in listing.entries {
        let could_matter = match entry.kind {
            EntryKind::File => file_here,
            EntryKind::Dir => below,
            _ => file_here || below,
        };
        if !could_matter {
            continue;
        }
        let Some(name) = entry.name.to_str() else {
            return Err(format!(
                "the JUnit walk met a name that is not UTF-8 where a report `{}` matches could lie",
                walk.glob
            ));
        };
        let child = if rel.is_empty() {
            name.to_string()
        } else {
            format!("{rel}/{name}")
        };
        match entry.kind {
            EntryKind::File => {
                if walk.matcher.is_match(&child) {
                    if walk.out.len() >= MAX_REPORT_FILES {
                        return Err(format!(
                            "more than {MAX_REPORT_FILES} JUnit reports match `{}`",
                            walk.glob
                        ));
                    }
                    stamp(beneath, &child, walk, limits)?;
                }
            }
            EntryKind::Dir if depth >= MAX_WALK_DEPTH => {
                return Err(format!(
                    "the JUnit walk stops {MAX_WALK_DEPTH} directories below its root, before every path `{}` matches was examined",
                    walk.glob
                ));
            }
            EntryKind::Dir => walk_reports(beneath, &child, depth + 1, walk, budget, limits)?,
            // Links are never followed: one where a report or a directory
            // of reports could lie leaves the search incomplete.
            _ => {
                return Err(format!(
                    "the JUnit walk met a link or a special file where a report `{}` matches could lie; links are never followed",
                    walk.glob
                ));
            }
        }
    }
    Ok(())
}

/// Whether the command wrote a report: absent before, or another length,
/// digest or modification time (the time is a shortcut that can say
/// "changed", never "unchanged": a rewrite within one tick keeps the time);
/// a side whose read was refused has no digest and counts as written, so
/// its refusal is reported.
fn is_written(before: Option<&ReportStamp>, after: &ReportStamp) -> bool {
    match before {
        None => true,
        Some(b) => {
            b.len != after.len
                || b.modified != after.modified
                || b.digest.is_none()
                || after.digest.is_none()
                || b.digest != after.digest
        }
    }
}

/// Stamps one report into the walk: its length and modification from
/// [`Beneath::file_stamp`] (a descriptor-relative `lstat`, never a
/// `symlink_metadata` on a joined path, which would follow a directory
/// swapped for a link after the listing), and its digest from the capped
/// read's own, no second hash. In the after listing a report the command
/// wrote is parsed from that one read: the bytes whose digest decided it
/// was written are the bytes parsed. A path that does not hold to the
/// observation text rules ([`checked_text`]) is refused before it names
/// anything.
fn stamp(
    beneath: &Beneath,
    rel: &str,
    walk: &mut ReportWalk<'_, '_>,
    limits: &ScanLimits<'_>,
) -> Result<(), String> {
    checked_text(rel, "a JUnit report path")?;
    let (modified, len) = match beneath.file_stamp(rel).map_err(|e| e.to_string())? {
        Some(fs) => (Some(fs.modified), fs.len),
        None => (None, 0),
    };
    limits.check()?;
    let read = beneath
        .read_regular_capped(rel, MAX_REPORT_BYTES)
        .map_err(|e| e.to_string())?;
    let digest = match &read {
        CappedRead::Read { digest, .. } => Some(digest.clone()),
        CappedRead::Refused(_) => None,
    };
    let stamped = ReportStamp {
        modified,
        len,
        digest,
    };
    if let Some(parse) = walk.parse.as_deref_mut()
        && is_written(parse.before.get(rel), &stamped)
    {
        parse.written += 1;
        let bytes = match read {
            CappedRead::Read { bytes, .. } => bytes,
            CappedRead::Refused(refusal) => {
                return Err(format!(
                    "{rel}: the JUnit report cannot be read ({})",
                    refusal_why(refusal)
                ));
            }
        };
        parse.parsed += 1;
        let left = MAX_LIST_ITEMS - parse.items.len();
        let names = failed_tests_within(&bytes, left, &mut || limits.check()).map_err(|why| {
            if why.starts_with(TOO_MANY_FAILING) {
                format!(
                    "more than the {MAX_LIST_ITEMS} failing tests an observation keeps (stopped at {rel})"
                )
            } else {
                format!("{rel}: {why}")
            }
        })?;
        parse
            .items
            .extend(names.into_iter().map(|t| (rel.to_string(), t)));
    }
    walk.out.insert(rel.to_string(), stamped);
    Ok(())
}

/// Why a report was not read, for the reason that names it.
fn refusal_why(refusal: ReadRefusal) -> &'static str {
    match refusal {
        ReadRefusal::Missing => "it is gone",
        ReadRefusal::Link => "it goes through a link",
        ReadRefusal::NotRegular => "it is not a regular file",
        ReadRefusal::HardLink => "it has more than one link",
        ReadRefusal::TooLarge { .. } => "it is larger than the report cap",
    }
}

/// The failing tests of the reports the command wrote ([`is_written`]
/// against `before`), read by the after listing itself. Every failing test
/// is one item of [`junit_items`], its comparison key (report, classname,
/// name and occurrence, injectively encoded), the list sorted and capped
/// like `cargo test`'s; the human text is [`render_item`]'s, never
/// compared.
fn junit_failed_tests(
    tree: &Path,
    glob: &str,
    before: &BTreeMap<String, ReportStamp>,
    limits: &ScanLimits<'_>,
) -> Result<Vec<String>, String> {
    let mut parse = Parse {
        before,
        written: 0,
        parsed: 0,
        items: Vec::new(),
    };
    junit_reports(tree, glob, limits, Some(&mut parse))?;
    if parse.written == 0 {
        return Err(format!(
            "no JUnit report written by the command matched `{glob}`"
        ));
    }
    let out = junit_items(parse.items)?;
    if out.len() > MAX_LIST_ITEMS {
        return Err(format!(
            "{} failing tests, more than the {MAX_LIST_ITEMS} an observation keeps",
            out.len()
        ));
    }
    Ok(out)
}

/// Runs the discovered (or, for the after snapshot, the recorded) commands.
pub(crate) fn read(
    spec: &ProjectChecksSpec,
    timeout: Duration,
    ctx: &ReadCtx<'_>,
    recorded: Option<&[ProjectCommand]>,
) -> Snapshot {
    let commands = match recorded {
        Some(c) => c.to_vec(),
        None => discover(ctx.workdir, &spec.only),
    };
    let digest = input_digest(ctx, &commands, spec.junit.as_deref());
    let inputs: Vec<String> = commands.iter().map(|c| c.argv.join(" ")).collect();
    if commands.is_empty() {
        return unknown(
            "the project defines no matching check command",
            digest,
            inputs,
            ctx,
        );
    }
    let deadline = Instant::now() + timeout;
    let mut snap = Snapshot {
        status: status::OBSERVED.into(),
        input_digest: digest,
        inputs,
        workdir: Some(ctx.workdir_text()),
        ..Snapshot::default()
    };
    let ran = ctx.tree_guarded(|| {
        for cmd in &commands {
            if ctx.cancel.load(std::sync::atomic::Ordering::SeqCst) {
                snap.observations
                    .insert(status_key(cmd), Observation::unknown("cancelled"));
                continue;
            }
            let (st, tests) = run(cmd, deadline, ctx, spec.junit.as_deref());
            if let Some(tests) = tests {
                snap.observations.insert(tests_key(cmd), tests);
            }
            snap.observations.insert(status_key(cmd), st);
        }
    });
    if let Err(why) = ran {
        let mut snap = unknown(why, snap.input_digest, snap.inputs, ctx);
        snap.commands = commands;
        return snap;
    }
    snap.commands = commands;
    snap
}

/// Asks a project command for plain output, whatever the driver's own
/// environment says (CI often sets `CARGO_TERM_COLOR=always`): the output
/// is parsed, not shown. Set last, so no overlay turns color back on.
fn without_color(c: &mut Command) {
    c.env("CARGO_TERM_COLOR", "never")
        .env("NO_COLOR", "1")
        .env("TERM", "dumb")
        .env_remove("FORCE_COLOR")
        .env_remove("CLICOLOR_FORCE");
}

/// The `tests` observation of an early return, before anything was read:
/// the before scan's reason when it failed, else none.
fn early_tests(scan: &Option<(&str, ReportScan)>) -> Option<Observation> {
    match scan {
        Some((_, Err(why))) => Some(Observation::unknown(format!(
            "the JUnit listing before the command failed: {why}"
        ))),
        _ => None,
    }
}

/// Runs one command: its status observation and, when its failing tests
/// can be read, that list. For `cargo test` they are parsed from the
/// output; for the other `test` commands, with a `junit` glob and in a run
/// that holds its tree, they come from the JUnit reports the command
/// wrote. A report the command did not write, or one that cannot be read,
/// makes the list unknown.
fn run(
    cmd: &ProjectCommand,
    deadline: Instant,
    ctx: &ReadCtx<'_>,
    junit: Option<&str>,
) -> (Observation, Option<Observation>) {
    let Some((program, args)) = cmd.argv.split_first() else {
        return (Observation::unknown("the command is empty"), None);
    };
    // Both JUnit listings are charged to the command's own deadline; no
    // time is added for them. A run that does not hold its tree never
    // scans: a report there may be another process's.
    let limits = ScanLimits {
        deadline,
        cancel: ctx.cancel,
    };
    let scan = match junit {
        Some(glob) if cmd.kind == "test" && !cmd.is_cargo_test() && ctx.holds_tree => {
            Some((glob, junit_reports(ctx.workdir, glob, &limits, None)))
        }
        _ => None,
    };
    // A `junit` command in a run that does not hold its tree: the command
    // still runs for its status, its failing tests read unknown.
    let junit_command = junit.is_some() && cmd.kind == "test" && !cmd.is_cargo_test();
    // The child's allowance, computed after the before scan: a scan that
    // spent it, or one during which the run was cancelled, never starts a
    // child.
    let left = deadline.saturating_duration_since(Instant::now());
    if left.is_zero() {
        return (
            Observation::unknown("the check ran out of time"),
            early_tests(&scan),
        );
    }
    let mut c = Command::new(program);
    c.args(args).current_dir(ctx.workdir);
    for name in ctx.scrub {
        c.env_remove(name);
    }
    c.env("APB_BASELINE_PHASE", ctx.phase);
    for (k, v) in ctx.spawn_env {
        c.env(k, v);
    }
    without_color(&mut c);
    // `run_capture` spawns first and reads the flag only afterwards, so a
    // cancel that arrived during the last scan operation would otherwise
    // still start the test program.
    if ctx.cancel.load(Ordering::SeqCst) {
        return (Observation::unknown("cancelled"), early_tests(&scan));
    }
    let captured = match crate::proc::run_capture(c, Some(left), Some(ctx.cancel)) {
        Ok(c) => c,
        Err(e) => {
            return (
                Observation::unknown(format!("`{}` could not start: {e}", cmd.argv.join(" "))),
                early_tests(&scan),
            );
        }
    };
    let passed = match captured.status {
        Some(s) => s.success(),
        None if captured.cancelled => {
            return (Observation::unknown("cancelled"), early_tests(&scan));
        }
        None => return (Observation::unknown("ran out of time"), early_tests(&scan)),
    };
    let tests = if junit_command && !ctx.holds_tree {
        Some(Observation::unknown(
            "the run does not hold its working tree (it shares it, or takes no lock), so a JUnit report there may be another process's",
        ))
    } else {
        match &scan {
            None => cmd.is_cargo_test().then(|| {
                match cargo_failed_tests(&captured.stdout, &captured.stderr) {
                    Ok(t) => Observation::value(ObservedValue::List(t)),
                    Err(why) => Observation::unknown(why),
                }
            }),
            Some((_, Err(why))) => Some(Observation::unknown(format!(
                "the JUnit listing before the command failed: {why}"
            ))),
            Some((glob, Ok(before))) => Some(
                match junit_failed_tests(ctx.workdir, glob, before, &limits) {
                    // A list only when it explains the outcome: a command
                    // that failed while its reports name no failing test
                    // failed for a reason no test case carries (a coverage
                    // gate, a lint step, a suite that crashed before its
                    // report), so "no failing test" would be a guess. The
                    // mirror case, a command that passed while a report
                    // names a failing test (a flaky retry, an ignored
                    // failure), keeps its list: `compare` reads a list only
                    // when both sides failed, so it can never turn that
                    // into "met".
                    Ok(t) if t.is_empty() && !passed => Observation::unknown(UNEXPLAINED_FAILURE),
                    Ok(t) => Observation::value(ObservedValue::List(t)),
                    Err(why) => Observation::unknown(why),
                },
            ),
        }
    };
    let word = if passed { PASSED } else { FAILED };
    (Observation::value(ObservedValue::Text(word.into())), tests)
}

fn words(o: &Observation) -> Option<&str> {
    match o.as_value()? {
        ObservedValue::Text(t) => Some(t.as_str()),
        _ => None,
    }
}

fn list(o: &Observation) -> Option<&[String]> {
    match o.as_value()? {
        ObservedValue::List(l) => Some(l.as_slice()),
        _ => None,
    }
}

fn unknown_cmp(detail: impl Into<String>) -> Comparison {
    Comparison {
        verdict: Verdict::Unknown,
        regressed: None,
        detail: Some(detail.into()),
    }
}

/// One command's comparison under "no new failures": the before and after
/// status observations, and the comparison.
pub(crate) fn compare(
    cmd: &ProjectCommand,
    before: &Snapshot,
    after: &Snapshot,
) -> (Observation, Observation, Comparison) {
    let (b, a) = (
        before.observation(&status_key(cmd)),
        after.observation(&status_key(cmd)),
    );
    let cmp = match (words(&b), words(&a), &b, &a) {
        (_, _, Observation::Unknown { reason }, _) => unknown_cmp(format!("before: {reason}")),
        (_, _, _, Observation::Unknown { reason }) => unknown_cmp(format!("after: {reason}")),
        (_, Some(PASSED), ..) => Comparison {
            verdict: Verdict::Met,
            regressed: Some(false),
            detail: Some(format!("{} -> passed", words(&b).unwrap_or("?"))),
        },
        (Some(PASSED), Some(_), ..) => {
            let failing = list(&after.observation(&tests_key(cmd)))
                .map(named)
                .unwrap_or_default();
            Comparison {
                verdict: Verdict::NotMet,
                regressed: Some(true),
                detail: Some(format!("passed -> failed{failing}")),
            }
        }
        _ => {
            let (bt, at) = (
                before.observation(&tests_key(cmd)),
                after.observation(&tests_key(cmd)),
            );
            match (list(&bt), list(&at)) {
                (Some(bt), Some(at)) => {
                    let new: Vec<String> = at.iter().filter(|t| !bt.contains(t)).cloned().collect();
                    Comparison {
                        verdict: if new.is_empty() {
                            Verdict::Met
                        } else {
                            Verdict::NotMet
                        },
                        regressed: Some(!new.is_empty()),
                        detail: Some(if new.is_empty() {
                            format!(
                                "{} failing test(s) -> {}, no new failures",
                                bt.len(),
                                at.len()
                            )
                        } else {
                            format!(
                                "{} failing test(s) -> {}{}",
                                bt.len(),
                                at.len(),
                                named(&new)
                            )
                        }),
                    }
                }
                _ => unknown_cmp("it failed on both sides and its failing tests could not be read"),
            }
        }
    };
    (b, a, cmp)
}

/// `; new failing test(s): a, b` for a non-empty list, each item shown as
/// [`render_item`] renders it (display only: the comparison above ran on
/// the items themselves).
fn named(tests: &[String]) -> String {
    if tests.is_empty() {
        return String::new();
    }
    let mut out = format!(
        "; new failing test(s): {}",
        tests
            .iter()
            .take(NAMED_TESTS)
            .map(|t| render_item(t))
            .collect::<Vec<_>>()
            .join(", ")
    );
    if tests.len() > NAMED_TESTS {
        out.push_str(&format!(" and {} more", tests.len() - NAMED_TESTS));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const STDERR: &str = "   Compiling x v0.1.0\n     Running unittests src/lib.rs (target/debug/deps/x-0123abcd)\n     Running tests/api.rs (target/debug/deps/api-89ef0123)\n   Doc-tests x\n";

    #[test]
    fn cargo_failures_are_named_by_their_binary_and_read_only_when_whole() {
        let stdout = "\nrunning 2 tests\ntest tests::it_works ... FAILED\ntest a::ok ... ok\n\ntest result: FAILED. 1 passed; 1 failed\n\nrunning 1 test\ntest tests::it_works ... FAILED\n\ntest result: FAILED. 0 passed; 1 failed\n\nrunning 1 test\ntest src/lib.rs - f (line 12) ... FAILED\n\ntest result: FAILED. 0 passed; 1 failed\n";
        assert_eq!(
            cargo_failed_tests(stdout, STDERR).unwrap(),
            [
                "doctests x: src/lib.rs - f",
                "tests/api.rs [api]: tests::it_works",
                "unittests src/lib.rs [x]: tests::it_works",
            ]
        );
        // A binary that never printed its result (it crashed, or cargo
        // stopped) leaves the list unknown, never short.
        let cut = stdout.rsplit_once("test result").unwrap().0;
        assert!(cargo_failed_tests(cut, STDERR).is_err());
        assert!(cargo_failed_tests("", "error[E0425]: cannot find value").is_err());
    }

    #[test]
    fn colored_cargo_output_names_the_same_failures() {
        let stdout = "running 2 tests\ntest tests::it_works ... FAILED\ntest a::ok ... ok\ntest result: FAILED. 1 passed; 1 failed\nrunning 1 test\ntest tests::it_works ... FAILED\ntest result: FAILED.\nrunning 1 test\ntest src/lib.rs - f (line 12) ... FAILED\ntest result: FAILED.\n";
        let paint = |s: &str| {
            s.replace("FAILED", "\u{1b}[31mFAILED\u{1b}[0m")
                .replace(" ok", " \u{1b}[32mok\u{1b}[0m")
                .replace("Running", "\u{1b}[1m\u{1b}[92mRunning\u{1b}[0m")
                .replace("Doc-tests", "\u{1b}[1m\u{1b}[92mDoc-tests\u{1b}[0m")
        };
        assert!(paint(STDERR).contains('\u{1b}'));
        assert_eq!(
            cargo_failed_tests(&paint(stdout), &paint(STDERR)).unwrap(),
            cargo_failed_tests(stdout, STDERR).unwrap()
        );
    }

    #[test]
    fn two_doctests_failing_on_one_item_stay_two_items() {
        let one =
            "running 1 test\ntest src/lib.rs - f (line 12) ... FAILED\ntest result: FAILED.\n";
        let two = "running 2 tests\ntest src/lib.rs - f (line 12) ... FAILED\ntest src/lib.rs - f (line 40) ... FAILED\ntest result: FAILED.\n";
        let stderr = "   Doc-tests x\n";
        let before = cargo_failed_tests(one, stderr).unwrap();
        let after = cargo_failed_tests(two, stderr).unwrap();
        assert_eq!(before, ["doctests x: src/lib.rs - f"]);
        assert_eq!(
            after,
            [
                "doctests x: src/lib.rs - f",
                "doctests x: src/lib.rs - f #2"
            ]
        );
    }

    #[test]
    fn a_test_binary_is_keyed_without_its_build_hash_or_extension() {
        let key = |path: &str| binary_of(&format!("Running tests/api.rs ({path})")).unwrap();
        assert_eq!(key("target/debug/deps/api-89ef0123"), "tests/api.rs [api]");
        assert_eq!(
            key("target\\debug\\deps\\api-89ef0123.exe"),
            "tests/api.rs [api]"
        );
        // No hash after the last `-` keeps the whole name.
        assert_eq!(key("target/debug/deps/api-"), "tests/api.rs [api-]");
        assert_eq!(key("target/debug/deps/my-tool"), "tests/api.rs [my-tool]");
    }

    #[test]
    fn more_failing_tests_than_a_list_keeps_read_unknown() {
        let mut stdout = String::from("running 9 tests\n");
        for i in 0..=MAX_LIST_ITEMS {
            stdout.push_str(&format!("test t{i} ... FAILED\n"));
        }
        stdout.push_str("test result: FAILED.\n");
        let stderr = "     Running unittests src/lib.rs (target/debug/deps/x-00ff)\n";
        assert!(cargo_failed_tests(&stdout, stderr).is_err());
    }

    fn snap(cmd: &ProjectCommand, status: &str, tests: Option<&[&str]>) -> Snapshot {
        let mut s = Snapshot {
            status: super::status::OBSERVED.into(),
            ..Snapshot::default()
        };
        s.observations.insert(
            status_key(cmd),
            Observation::value(ObservedValue::Text(status.into())),
        );
        if let Some(t) = tests {
            s.observations.insert(
                tests_key(cmd),
                Observation::value(ObservedValue::List(
                    t.iter().map(|x| x.to_string()).collect(),
                )),
            );
        }
        s
    }

    #[test]
    fn no_new_failures_rule() {
        let cmd = ProjectCommand {
            name: "cargo_test".into(),
            kind: "test".into(),
            argv: vec!["cargo".into(), "test".into(), "--no-fail-fast".into()],
            source_file: "Cargo.toml".into(),
        };
        let v = |b: Snapshot, a: Snapshot| compare(&cmd, &b, &a).2;
        assert_eq!(
            v(snap(&cmd, PASSED, None), snap(&cmd, PASSED, None)).verdict,
            Verdict::Met
        );
        let c = v(
            snap(&cmd, PASSED, None),
            snap(&cmd, FAILED, Some(&["x::new"])),
        );
        assert_eq!((c.verdict, c.regressed), (Verdict::NotMet, Some(true)));
        assert!(c.detail.unwrap().contains("x::new"));
        let c = v(
            snap(&cmd, FAILED, Some(&["a", "b"])),
            snap(&cmd, FAILED, Some(&["a"])),
        );
        assert_eq!(c.verdict, Verdict::Met);
        let c = v(
            snap(&cmd, FAILED, Some(&["a", "b"])),
            snap(&cmd, FAILED, Some(&["a", "c"])),
        );
        assert_eq!((c.verdict, c.regressed), (Verdict::NotMet, Some(true)));
        assert!(c.detail.unwrap().contains("new failing test(s): c"));
        let c = v(snap(&cmd, FAILED, None), snap(&cmd, FAILED, Some(&["a"])));
        assert_eq!(c.verdict, Verdict::Unknown);
    }

    // --- JUnit reports ------------------------------------------------------

    type Dummies = (
        std::collections::BTreeMap<String, String>,
        Vec<(String, String)>,
        Vec<String>,
        Vec<crate::event::Event>,
    );

    /// A `ReadCtx` for the unit tests of `run`: no git tree guard (the
    /// workdirs here are not git trees), the `holds_tree` the test asks for.
    fn unit_ctx<'a>(
        workdir: &'a Path,
        cancel: &'a AtomicBool,
        dummies: &'a Dummies,
        holds_tree: bool,
    ) -> ReadCtx<'a> {
        ReadCtx {
            run_dir: workdir,
            root: workdir,
            workdir,
            params: &dummies.0,
            spawn_env: &dummies.1,
            scrub: &dummies.2,
            cancel,
            phase: "before",
            events: &dummies.3,
            guard_tree: false,
            scripts_label: "unit tests",
            site: apb_core::baseline::BASELINE_SITE,
            cap_secs: None,
            holds_tree,
        }
    }

    /// The `make_test` command the JUnit unit tests run.
    fn make_test(what: &'static str) -> ProjectCommand {
        ProjectCommand {
            name: "make_test".into(),
            kind: "test".into(),
            argv: vec!["sh".into(), "-c".into(), what.into()],
            source_file: "Makefile".into(),
        }
    }

    /// One failing case, as a JUnit report body.
    fn one_failure(class: &str, name: &str) -> String {
        format!(
            "<testsuite><testcase classname=\"{class}\" name=\"{name}\"><failure/></testcase></testsuite>"
        )
    }

    fn limits<'a>(deadline: Instant, cancel: &'a AtomicBool) -> ScanLimits<'a> {
        ScanLimits { deadline, cancel }
    }

    /// The unit `run` with a JUnit glob and its command's deadline.
    fn run_junit(
        cmd: &ProjectCommand,
        deadline: Instant,
        ctx: &ReadCtx<'_>,
    ) -> (Observation, Option<Observation>) {
        run(cmd, deadline, ctx, Some("reports/*.xml"))
    }

    /// A report the run's tree holds only through a link is never read:
    /// the link is skipped, so nothing reads as written.
    #[test]
    fn a_linked_report_is_not_read() {
        let dir = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let root = dir.path();
        fs_create(root, "reports");
        std::fs::write(
            outside.path().join("real.xml"),
            one_failure("suite", "it_fails"),
        )
        .unwrap();
        std::os::unix::fs::symlink(outside.path().join("real.xml"), root.join("reports/x.xml"))
            .unwrap();
        let cancel = AtomicBool::new(false);
        let l = limits(Instant::now() + std::time::Duration::from_secs(60), &cancel);
        // A link where a report could lie leaves the search incomplete:
        // unknown, never a list without it.
        let why = junit_reports(root, "reports/*.xml", &l, None).unwrap_err();
        assert!(why.contains("links are never followed"), "{why}");
        let why = junit_reports(root, "reports/x.xml", &l, None).unwrap_err();
        assert!(why.contains("links are never followed"), "{why}");
    }

    /// The reviewer's example: a failing report past the walk's depth limit
    /// makes the list unknown, never a list without it; a glob of fixed
    /// depth never descends where no match can lie, so a deep tree beside
    /// it is no error.
    #[test]
    fn a_walk_cut_by_its_depth_limit_is_unknown() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        fs_create(root, "reports/a/b/c/d/e/f/g/h/i");
        std::fs::write(
            root.join("reports/ok.xml"),
            one_failure("s", "x").replace("<failure/>", ""),
        )
        .unwrap();
        std::fs::write(
            root.join("reports/a/b/c/d/e/f/g/h/i/fail.xml"),
            one_failure("s", "f"),
        )
        .unwrap();
        let cancel = AtomicBool::new(false);
        let l = limits(Instant::now() + std::time::Duration::from_secs(60), &cancel);
        let why = junit_reports(root, "reports/**/*.xml", &l, None).unwrap_err();
        assert!(why.contains("directories below its root"), "{why}");
        let shallow = junit_reports(root, "reports/*.xml", &l, None).unwrap();
        assert_eq!(shallow.keys().collect::<Vec<_>>(), ["reports/ok.xml"]);
        assert_eq!(match_depth("reports/*.xml", "reports"), Some(0));
        assert_eq!(match_depth("a/*/c.xml", "a"), Some(1));
        assert_eq!(match_depth("out/**/TEST-*.xml", "out"), None);
        assert_eq!(match_depth("r/{a,b/c}.xml", "r"), None);
    }

    /// A character class can match `/` in the matcher (`[!x]` does, even
    /// with a literal separator), so a glob with one has no fixed depth:
    /// the walk descends, and every path the matcher matches is listed.
    #[test]
    fn a_character_class_glob_is_walked_to_every_depth_it_matches() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        fs_create(root, "reports/a");
        std::fs::write(root.join("reports/aYb.xml"), one_failure("s", "x")).unwrap();
        std::fs::write(root.join("reports/a/b.xml"), one_failure("s", "f")).unwrap();
        let cancel = AtomicBool::new(false);
        let l = limits(Instant::now() + std::time::Duration::from_secs(60), &cancel);
        let glob = "reports/a[!x]b.xml";
        let m = matcher(glob).unwrap();
        let expected: Vec<&str> = ["reports/a/b.xml", "reports/aYb.xml"]
            .into_iter()
            .filter(|p| m.is_match(p))
            .collect();
        let found = junit_reports(root, glob, &l, None).unwrap();
        assert_eq!(
            found.keys().map(String::as_str).collect::<Vec<_>>(),
            expected
        );
        assert_eq!(match_depth(glob, "reports"), None);
        assert_eq!(match_depth("reports/[ab].xml", "reports"), None);
    }

    /// A name that is not UTF-8 where a report could lie leaves the search
    /// incomplete: unknown.
    #[test]
    fn a_name_that_is_not_utf8_where_a_report_could_lie_is_unknown() {
        use std::os::unix::ffi::OsStrExt;
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        fs_create(root, "reports");
        let odd = root
            .join("reports")
            .join(std::ffi::OsStr::from_bytes(b"\xffx.xml"));
        if std::fs::write(&odd, "x").is_err() {
            return; // a file system that refuses such a name
        }
        let cancel = AtomicBool::new(false);
        let l = limits(Instant::now() + std::time::Duration::from_secs(60), &cancel);
        let why = junit_reports(root, "reports/*.xml", &l, None).unwrap_err();
        assert!(why.contains("not UTF-8"), "{why}");
    }

    /// A rewritten report counts as written; an untouched one does not.
    /// Pure over `junit_reports` and `junit_failed_tests`, the untouched
    /// report pinned back to the time `before` recorded.
    #[test]
    fn a_rewritten_report_counts_and_an_untouched_one_does_not() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        fs_create(root, "reports");
        std::fs::write(
            root.join("reports/ok.xml"),
            one_failure("suite", "ok_fails"),
        )
        .unwrap();
        std::fs::write(
            root.join("reports/changed.xml"),
            one_failure("suite", "old"),
        )
        .unwrap();
        let cancel = AtomicBool::new(false);
        let l = limits(Instant::now() + std::time::Duration::from_secs(60), &cancel);
        let before = junit_reports(root, "reports/*.xml", &l, None).unwrap();
        assert_eq!(before.len(), 2);
        std::fs::write(
            root.join("reports/changed.xml"),
            one_failure("suite", "new"),
        )
        .unwrap();
        let pin = std::fs::File::options()
            .write(true)
            .open(root.join("reports/ok.xml"))
            .unwrap();
        pin.set_modified(before["reports/ok.xml"].modified.unwrap())
            .unwrap();
        let got = junit_failed_tests(root, "reports/*.xml", &before, &l).unwrap();
        assert_eq!(got, [key("reports/changed.xml", "suite", "new")]);
    }

    /// An equal-length rewrite stamped back to the recorded modification
    /// time still counts: the digest is the difference, never the clock.
    #[test]
    fn an_equal_length_rewrite_within_one_timestamp_tick_counts() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        fs_create(root, "reports");
        std::fs::write(root.join("reports/junit.xml"), one_failure("suite", "aaa")).unwrap();
        let cancel = AtomicBool::new(false);
        let l = limits(Instant::now() + std::time::Duration::from_secs(60), &cancel);
        let before = junit_reports(root, "reports/*.xml", &l, None).unwrap();
        std::fs::write(root.join("reports/junit.xml"), one_failure("suite", "bbb")).unwrap();
        let pin = std::fs::File::options()
            .write(true)
            .open(root.join("reports/junit.xml"))
            .unwrap();
        pin.set_modified(before["reports/junit.xml"].modified.unwrap())
            .unwrap();
        let got = junit_failed_tests(root, "reports/*.xml", &before, &l).unwrap();
        assert_eq!(got, [key("reports/junit.xml", "suite", "bbb")]);
    }

    /// The walk starts at the longest literal directory prefix, never the
    /// last component.
    #[test]
    fn the_walk_root_is_the_literal_directory_prefix() {
        assert_eq!(walk_root("reports/junit.xml"), "reports");
        assert_eq!(walk_root("reports/*.xml"), "reports");
        assert_eq!(walk_root("junit.xml"), "");
        assert_eq!(walk_root("out/**/TEST-*.xml"), "out");
        assert_eq!(walk_root("a/b/c.xml"), "a/b");
    }

    /// A literal report path is read even though it is not walked: written
    /// after the before listing, it counts as the command's.
    #[test]
    fn a_literal_report_path_is_read() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        fs_create(root, "reports");
        let cancel = AtomicBool::new(false);
        let l = limits(Instant::now() + std::time::Duration::from_secs(60), &cancel);
        let before = junit_reports(root, "reports/junit.xml", &l, None).unwrap();
        assert!(before.is_empty());
        std::fs::write(
            root.join("reports/junit.xml"),
            one_failure("suite", "it_fails"),
        )
        .unwrap();
        let got = junit_failed_tests(root, "reports/junit.xml", &before, &l).unwrap();
        assert_eq!(got, [key("reports/junit.xml", "suite", "it_fails")]);
    }

    /// `*` never crosses a directory: `reports/*.xml` matches the report
    /// beside it, never the one one level down.
    #[test]
    fn a_star_stays_inside_one_component() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        fs_create(root, "reports/sub");
        std::fs::write(root.join("reports/a.xml"), one_failure("suite", "a_fails")).unwrap();
        std::fs::write(
            root.join("reports/sub/b.xml"),
            one_failure("suite", "b_fails"),
        )
        .unwrap();
        let cancel = AtomicBool::new(false);
        let l = limits(Instant::now() + std::time::Duration::from_secs(60), &cancel);
        let before = junit_reports(root, "reports/*.xml", &l, None).unwrap();
        assert_eq!(before.keys().collect::<Vec<_>>(), ["reports/a.xml"]);
        // The command rewrites its report: the deeper one is still never
        // matched, let alone read.
        std::fs::write(root.join("reports/a.xml"), one_failure("suite", "a_new")).unwrap();
        let got = junit_failed_tests(root, "reports/*.xml", &before, &l).unwrap();
        assert_eq!(got, [key("reports/a.xml", "suite", "a_new")]);
    }

    /// The scans are charged to the command's own deadline: a before scan
    /// that spends it leaves the command unstarted, its status the
    /// pre-start check's words (design D4.2 item 2). A deadline already
    /// past when the scan starts is the deterministic form of a scan that
    /// spent the allowance.
    #[test]
    fn a_scan_that_uses_up_the_allowance_does_not_start_the_command() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        fs_create(root, "reports");
        std::fs::write(root.join("reports/a.xml"), one_failure("suite", "a")).unwrap();
        let dummies = Dummies::default();
        let cancel = AtomicBool::new(false);
        let ctx = unit_ctx(root, &cancel, &dummies, true);
        let (status, tests) = run_junit(&make_test("touch started; exit 1"), Instant::now(), &ctx);
        assert!(
            matches!(&status, Observation::Unknown { reason, .. }
                if reason == "the check ran out of time"),
            "{status:?}"
        );
        let tests = tests.expect("the tests observation of the failed scan");
        assert!(
            matches!(&tests, Observation::Unknown { reason, .. }
                if reason.contains("the JUnit scan ran out of time")),
            "{tests:?}"
        );
        assert!(!root.join("started").exists(), "the command started");
    }

    /// A cancel flag set before the scan stops it: `cancelled`, nothing
    /// read.
    #[test]
    fn a_cancelled_scan_stops() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        fs_create(root, "reports");
        std::fs::write(root.join("reports/a.xml"), one_failure("suite", "a")).unwrap();
        let cancel = AtomicBool::new(true);
        let l = limits(Instant::now() + std::time::Duration::from_secs(60), &cancel);
        assert_eq!(
            junit_reports(root, "reports/*.xml", &l, None).unwrap_err(),
            "cancelled"
        );
    }

    /// A cancel the scan saw still stops the command: the check right
    /// before the spawn catches what `run_capture`'s own late flag read
    /// would miss (it spawns first). The flag is set before the call, and
    /// the program does not exist: with the check the status is
    /// `cancelled`; without it the spawn is tried and the status reads
    /// "could not start", whatever the scheduling.
    #[test]
    fn a_cancel_during_the_last_scan_operation_starts_no_command() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        fs_create(root, "reports");
        std::fs::write(root.join("reports/a.xml"), "x").unwrap();
        let dummies = Dummies::default();
        let cancel = AtomicBool::new(true);
        let ctx = unit_ctx(root, &cancel, &dummies, true);
        let missing = ProjectCommand {
            argv: vec!["/nonexistent/apb-no-such-test-program".into()],
            ..make_test("")
        };
        let (status, _tests) = run(
            &missing,
            Instant::now() + std::time::Duration::from_secs(60),
            &ctx,
            Some("reports/*.xml"),
        );
        assert!(
            matches!(&status, Observation::Unknown { reason, .. } if reason == "cancelled"),
            "{status:?}"
        );
    }

    /// A failing test name with a control or invisible format character
    /// makes the list unknown, and the reason never repeats the name: the
    /// observation text rules hold for JUnit names as for every observed
    /// list item.
    #[test]
    fn a_hidden_character_in_a_test_name_reads_unknown() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        fs_create(root, "reports");
        let cancel = AtomicBool::new(false);
        let l = limits(Instant::now() + std::time::Duration::from_secs(60), &cancel);
        let before = junit_reports(root, "reports/*.xml", &l, None).unwrap();
        for (file, name) in [("a.xml", "x&#x200B;y"), ("b.xml", "x&#x202E;y")] {
            std::fs::write(root.join("reports").join(file), one_failure("s", name)).unwrap();
        }
        let why = junit_failed_tests(root, "reports/*.xml", &before, &l).unwrap_err();
        assert!(why.contains("control or format character"), "{why}");
        assert!(
            !why.contains('\u{200b}') && !why.contains('\u{202e}'),
            "{why}"
        );
    }

    /// A report whose path holds a control character is refused before the
    /// path names anything, and a name too long for an item with its deep
    /// path makes the list unknown, never a cut item.
    #[test]
    fn a_report_path_and_a_long_item_hold_to_the_bound() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        fs_create(root, "reports");
        let cancel = AtomicBool::new(false);
        let l = limits(Instant::now() + std::time::Duration::from_secs(60), &cancel);
        let before = junit_reports(root, "reports/*.xml", &l, None).unwrap();
        std::fs::write(root.join("reports/a\u{7}.xml"), one_failure("s", "t")).unwrap();
        let why = junit_failed_tests(root, "reports/*.xml", &before, &l).unwrap_err();
        assert!(why.contains("a JUnit report path"), "{why}");
        assert!(!why.contains('\u{7}'), "{why}");
        std::fs::remove_file(root.join("reports/a\u{7}.xml")).unwrap();

        let deep = "d/".repeat(7);
        fs_create(root, &format!("reports/{deep}"));
        let glob = "reports/**/*.xml";
        let before = junit_reports(root, glob, &l, None).unwrap();
        let name = "\u{e9}".repeat(400);
        std::fs::write(
            root.join(format!("reports/{deep}x.xml")),
            one_failure("s", &name),
        )
        .unwrap();
        let why = junit_failed_tests(root, glob, &before, &l).unwrap_err();
        assert!(why.starts_with(&format!("reports/{deep}x.xml: ")), "{why}");
        assert!(why.contains("longer than"), "{why}");
        assert!(
            !why.contains("\u{e9}\u{e9}"),
            "the reason never repeats the name: {why}"
        );
    }

    /// A run that does not hold its tree never scans: the command runs for
    /// its status and the failing tests read unknown with the reason naming
    /// the tree (design D4.2 "Who may read a report").
    #[test]
    fn a_shared_tree_reads_junit_unknown() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        fs_create(root, "reports");
        let dummies = Dummies::default();
        let cancel = AtomicBool::new(false);
        let ctx = unit_ctx(root, &cancel, &dummies, false);
        let cmd = make_test(
            "printf '%s' '<testsuite><testcase classname=\"s\" name=\"f\"><failure/></testcase></testsuite>' > reports/a.xml; exit 1",
        );
        let (status, tests) = run_junit(
            &cmd,
            Instant::now() + std::time::Duration::from_secs(60),
            &ctx,
        );
        assert!(
            matches!(&status, Observation::Value { value: ObservedValue::Text(t) } if t == "failed"),
            "{status:?}"
        );
        let tests = tests.expect("the tests observation names the tree");
        assert!(
            matches!(&tests, Observation::Unknown { reason, .. }
                if reason.contains("does not hold its working tree")),
            "{tests:?}"
        );
    }

    /// A before scan that cannot read the report place does not stop the
    /// command: its status is kept, the tests observation carries the
    /// listing's reason.
    #[test]
    fn an_unreadable_before_scan_still_runs_the_command() {
        let dir = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::os::unix::fs::symlink(outside.path(), root.join("reports")).unwrap();
        let dummies = Dummies::default();
        let cancel = AtomicBool::new(false);
        let ctx = unit_ctx(root, &cancel, &dummies, true);
        let (status, tests) = run_junit(
            &make_test("touch started; exit 1"),
            Instant::now() + std::time::Duration::from_secs(60),
            &ctx,
        );
        assert!(root.join("started").exists(), "the command did not run");
        assert!(
            matches!(&status, Observation::Value { value: ObservedValue::Text(t) } if t == "failed"),
            "{status:?}"
        );
        let tests = tests.expect("the tests observation of the failed listing");
        assert!(
            matches!(&tests, Observation::Unknown { reason, .. }
                if reason.starts_with("the JUnit listing before the command failed")),
            "{tests:?}"
        );
    }

    /// A walk that examines more entries than its budget runs the command
    /// anyway and reads unknown with the walk's reason.
    #[test]
    fn an_exhausted_before_scan_still_runs_the_command() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        fs_create(root, "reports");
        for n in 0..10_001 {
            std::fs::write(root.join("reports").join(format!("f{n}")), "").unwrap();
        }
        let dummies = Dummies::default();
        let cancel = AtomicBool::new(false);
        let ctx = unit_ctx(root, &cancel, &dummies, true);
        let (status, tests) = run_junit(
            &make_test("touch started; exit 1"),
            Instant::now() + std::time::Duration::from_secs(600),
            &ctx,
        );
        assert!(root.join("started").exists(), "the command did not run");
        assert!(
            matches!(&status, Observation::Value { value: ObservedValue::Text(t) } if t == "failed"),
            "{status:?}"
        );
        let tests = tests.expect("the tests observation of the exhausted walk");
        assert!(
            matches!(&tests, Observation::Unknown { reason, .. }
                if reason.contains("examined more than 10000 entries")),
            "{tests:?}"
        );
    }

    /// Without `junit` the input digest hashes exactly what it hashed
    /// before the glob existed, `<argv>\n<defined>`, so an existing run's
    /// before snapshot keeps comparing (design D4.2 item 6); with one, the
    /// glob is appended.
    #[test]
    fn without_junit_the_digest_is_unchanged() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::write(root.join("Makefile"), "test:\n\texit 1\n").unwrap();
        let dummies = Dummies::default();
        let cancel = AtomicBool::new(false);
        let ctx = unit_ctx(root, &cancel, &dummies, true);
        let cmds = [make_test("exit 1")];
        let argv = serde_json::to_string(&cmds[..]).unwrap();
        let defined = source_digest(root, &cmds);
        assert_eq!(
            input_digest(&ctx, &cmds, None),
            sha256_hex(format!("{argv}\n{defined}").as_bytes())
        );
        assert_eq!(
            input_digest(&ctx, &cmds, Some("reports/*.xml")),
            sha256_hex(format!("{argv}\n{defined}\njunit=reports/*.xml").as_bytes())
        );
    }

    /// Many reports with many failing tests each: the engine hands each
    /// next report only what is left of the observation's cap, so the walk
    /// stops unknown at the report that goes past it and reads no further
    /// one. 64 reports of half the cap each stop at the third.
    #[test]
    fn the_failing_cap_stops_the_walk_at_the_report_that_passes_it() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        fs_create(root, "reports");
        let cancel = AtomicBool::new(false);
        let l = limits(Instant::now() + std::time::Duration::from_secs(60), &cancel);
        let before = junit_reports(root, "reports/*.xml", &l, None).unwrap();
        let half = MAX_LIST_ITEMS / 2;
        for r in 0..64 {
            let body: String = (0..half)
                .map(|i| format!("<testcase name=\"t{i}\"><failure/></testcase>"))
                .collect();
            std::fs::write(
                root.join(format!("reports/r{r:02}.xml")),
                format!("<testsuite>{body}</testsuite>"),
            )
            .unwrap();
        }
        let mut parse = Parse {
            before: &before,
            written: 0,
            parsed: 0,
            items: Vec::new(),
        };
        let why = junit_reports(root, "reports/*.xml", &l, Some(&mut parse)).unwrap_err();
        assert!(why.contains("failing tests an observation keeps"), "{why}");
        assert!(why.contains("reports/r02.xml"), "{why}");
        assert_eq!(parse.parsed, 3, "the walk read no report past the cap");
        assert_eq!(parse.items.len(), 2 * half);
    }

    /// The comparison key of one failing test, first occurrence.
    fn key(report: &str, classname: &str, name: &str) -> String {
        apb_core::project_checks::encode_item(
            report,
            &FailingTest {
                classname: Some(classname.into()),
                name: name.into(),
            },
            1,
        )
    }

    /// The reviewer's examples through the comparison itself: a swap of
    /// the classname "A::B" with name "C" for the classname "A" with name
    /// "B::C" between two failing runs is a new failure, and so is a swap of
    /// a second failing "x" for a failing "x #2"; the detail renders the
    /// text, which may agree.
    #[test]
    fn a_swap_between_two_failing_runs_is_a_new_failure() {
        let cmd = make_test("exit 1");
        let items = |tests: Vec<(&str, Option<&str>, &str)>| {
            junit_items(
                tests
                    .into_iter()
                    .map(|(r, c, n)| {
                        (
                            r.to_string(),
                            FailingTest {
                                classname: c.map(str::to_string),
                                name: n.to_string(),
                            },
                        )
                    })
                    .collect(),
            )
            .unwrap()
        };
        let snap_of = |list: Vec<String>| {
            let mut s = Snapshot {
                status: super::status::OBSERVED.into(),
                ..Snapshot::default()
            };
            s.observations.insert(
                status_key(&cmd),
                Observation::value(ObservedValue::Text(FAILED.into())),
            );
            s.observations.insert(
                tests_key(&cmd),
                Observation::value(ObservedValue::List(list)),
            );
            s
        };
        for (before, after) in [
            (
                vec![("r.xml", Some("A::B"), "C")],
                vec![("r.xml", Some("A"), "B::C")],
            ),
            (
                vec![("r.xml", None, "x"), ("r.xml", None, "x")],
                vec![("r.xml", None, "x"), ("r.xml", None, "x #2")],
            ),
        ] {
            let c = compare(&cmd, &snap_of(items(before)), &snap_of(items(after))).2;
            assert_eq!(
                (c.verdict, c.regressed),
                (Verdict::NotMet, Some(true)),
                "{c:?}"
            );
            assert!(c.detail.unwrap().contains("new failing test(s): r.xml: "));
        }
    }

    /// `cargo test` items stay as they were, and a literal ` #2` name is
    /// never the second occurrence of its prefix.
    #[test]
    fn cargo_repeat_numbering_is_injective() {
        let two_x = numbered_repeats(vec!["b: x".into(), "b: x".into()]);
        let x_and_x2 = numbered_repeats(vec!["b: x".into(), "b: x #2".into()]);
        assert_eq!(two_x, ["b: x", "b: x #2"]);
        assert_eq!(x_and_x2, ["b: x", "b: x #2 #1"]);
        assert_ne!(two_x, x_and_x2);
        assert_eq!(numbered_repeats(vec!["b: plain".into()]), ["b: plain"]);
    }

    /// A command that fails while the report it wrote names no failing
    /// test: the failing tests are unknown, never an empty list, so a
    /// comparison against an earlier failing list is unknown, never met. A
    /// command that passes with a failing case in its report keeps the list.
    #[test]
    fn a_failure_no_report_explains_reads_unknown() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        fs_create(root, "reports");
        let dummies = Dummies::default();
        let cancel = AtomicBool::new(false);
        let ctx = unit_ctx(root, &cancel, &dummies, true);
        let deadline = Instant::now() + std::time::Duration::from_secs(60);
        let passing = make_test(
            "printf '%s' '<testsuite><testcase name=\"ok\"/></testsuite>' > reports/a.xml; exit 1",
        );
        let (status, tests) = run_junit(&passing, deadline, &ctx);
        assert!(
            matches!(&status, Observation::Value { value: ObservedValue::Text(t) } if t == FAILED),
            "{status:?}"
        );
        let tests = tests.expect("a tests observation");
        assert!(
            matches!(&tests, Observation::Unknown { reason, .. } if reason == UNEXPLAINED_FAILURE),
            "{tests:?}"
        );
        // Before failed [X], after failed with no explaining list: unknown.
        let before = snap(&passing, FAILED, Some(&["r.xml: x"]));
        let mut after = snap(&passing, FAILED, None);
        after.observations.insert(tests_key(&passing), tests);
        let c = compare(&passing, &before, &after).2;
        assert_eq!(c.verdict, Verdict::Unknown, "{c:?}");
        // The mirror: a pass whose report names a failing case keeps the list.
        let flaky = make_test(
            "printf '%s' '<testsuite><testcase name=\"f\"><failure/></testcase></testsuite>' > reports/b.xml; exit 0",
        );
        let (status, tests) = run_junit(&flaky, deadline, &ctx);
        assert!(
            matches!(&status, Observation::Value { value: ObservedValue::Text(t) } if t == PASSED),
            "{status:?}"
        );
        assert!(
            matches!(tests, Some(Observation::Value { value: ObservedValue::List(ref l) }) if l.len() == 1),
            "{tests:?}"
        );
    }

    fn fs_create(root: &Path, rel: &str) {
        std::fs::create_dir_all(root.join(rel)).unwrap();
    }
}
