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
//! could not list them, the comparison is unknown. Runners other than
//! `cargo test` therefore read unknown while they keep failing.
//!
//! Each command runs without the connector secrets in its environment, and
//! must leave the working tree as it found it (see `ctx.rs`); after an abort
//! or a `--now` pause the remaining commands are not started.

use std::process::Command;
use std::time::{Duration, Instant};

use apb_core::baseline::{Comparison, MAX_LIST_ITEMS, Observation, ObservedValue, Verdict};
use apb_core::content::sha256_hex;
use apb_core::project_checks::{ProjectChecksSpec, ProjectCommand, discover, source_digest};
use apb_core::text::strip_ansi;

use super::ctx::{ReadCtx, unknown};
use super::{Snapshot, status};

/// The rule every project command is compared under.
pub(crate) const RULE: &str = "no_new_failures";
const PASSED: &str = "passed";
const FAILED: &str = "failed";
/// How many new failing tests a detail names.
const NAMED_TESTS: usize = 10;

pub(crate) fn status_key(cmd: &ProjectCommand) -> String {
    format!("{}.status", cmd.name)
}

fn tests_key(cmd: &ProjectCommand) -> String {
    format!("{}.failed_tests", cmd.name)
}

/// What the input of a project check is: its commands and the files they
/// are defined in.
fn input_digest(ctx: &ReadCtx<'_>, commands: &[ProjectCommand]) -> String {
    let argv = serde_json::to_string(commands).unwrap_or_default();
    sha256_hex(format!("{argv}\n{}", source_digest(ctx.workdir, commands)).as_bytes())
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

/// A sorted list with every repeat of a name numbered (`name #2`): several
/// doctests failing on one item share a name once their line is dropped,
/// and one more failure there is still one more item.
fn numbered_repeats(sorted: Vec<String>) -> Vec<String> {
    let mut out: Vec<String> = Vec::with_capacity(sorted.len());
    let mut seen = 0usize;
    for (i, name) in sorted.iter().enumerate() {
        seen = if i > 0 && sorted[i - 1] == *name {
            seen + 1
        } else {
            1
        };
        out.push(if seen == 1 {
            name.clone()
        } else {
            format!("{name} #{seen}")
        });
    }
    out
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
    let digest = input_digest(ctx, &commands);
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
            let (st, tests) = run(cmd, deadline, ctx);
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

/// Runs one command: its status observation and, for `cargo test`, its
/// failing tests (or why they cannot be read).
fn run(
    cmd: &ProjectCommand,
    deadline: Instant,
    ctx: &ReadCtx<'_>,
) -> (Observation, Option<Observation>) {
    let Some((program, args)) = cmd.argv.split_first() else {
        return (Observation::unknown("the command is empty"), None);
    };
    let left = deadline.saturating_duration_since(Instant::now());
    if left.is_zero() {
        return (Observation::unknown("the check ran out of time"), None);
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
    let captured = match crate::proc::run_capture(c, Some(left), Some(ctx.cancel)) {
        Ok(c) => c,
        Err(e) => {
            return (
                Observation::unknown(format!("`{}` could not start: {e}", cmd.argv.join(" "))),
                None,
            );
        }
    };
    let passed = match captured.status {
        Some(s) => s.success(),
        None if captured.cancelled => return (Observation::unknown("cancelled"), None),
        None => return (Observation::unknown("ran out of time"), None),
    };
    let tests =
        cmd.is_cargo_test().then(
            || match cargo_failed_tests(&captured.stdout, &captured.stderr) {
                Ok(t) => Observation::value(ObservedValue::List(t)),
                Err(why) => Observation::unknown(why),
            },
        );
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

/// `; new failing test(s): a, b` for a non-empty list.
fn named(tests: &[String]) -> String {
    if tests.is_empty() {
        return String::new();
    }
    let mut out = format!(
        "; new failing test(s): {}",
        tests
            .iter()
            .take(NAMED_TESTS)
            .cloned()
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
}
