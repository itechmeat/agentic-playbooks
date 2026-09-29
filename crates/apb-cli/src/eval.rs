//! `apb eval` (0.24.0, first half of design C2): runs a playbook's eval
//! cases, each repetition as an ordinary run in a disposable repository, and
//! stores and compares the results.
//!
//! One repetition:
//!
//! 1. A fresh repository under `<config-dir>/evals/scratch/<eval>/<case>-<n>/tree`
//!    (never under the system temporary directory): the fixture committed as
//!    `main` (a `dir:` under `evals/fixtures/`, or `git archive` of a ref of
//!    this repository), the playbook's definition directory and the project
//!    profiles and skills copied into its `.apb` before that commit, and an
//!    optional `change:` overlay committed on its own branch. Its `origin` is
//!    a bare repository next to it, so a push succeeds locally and reaches
//!    no real remote.
//! 2. `apb run <id> --detach --no-cache --execution cli` inside that tree,
//!    through the ordinary run gate, with the case env overlay applied to
//!    the run's processes and `APB_NO_REGISTRY=1` so the scratch tree never
//!    enters the projects registry.
//! 3. The wait: the run is followed with the `apb wait` primitive and
//!    stopped when it waits for a person (gates cannot be answered yet),
//!    when its wall clock, token or cost limit is crossed; a spent
//!    invocation budget keeps the next repetition from starting.
//! 4. The checks (`apb_engine::eval::checks`), then the run directory moves
//!    to `<config-dir>/evals/runs/<playbook>/<run-id>` (outside the project,
//!    so `apb stats` never counts it) and the scratch directory is removed.
//!
//! A playbook with `irreversible` effects, a shipping step, a connector or a
//! sub-playbook is refused before anything is created
//! (`apb_core::eval::refusal`): eval runs never run irreversible playbooks.

use std::collections::BTreeMap;
use std::io::IsTerminal;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode, Stdio};
use std::time::{Duration, Instant};

use apb_core::eval::{self as core_eval, LoadedCase, LoadedSuite};
use apb_core::overrides::{EphemeralExecutor, NodeOverride, RunOverrides};
use apb_core::registry::Registry;
use apb_core::schema::{NodeKind, Playbook};
use apb_engine::eval::checks::{self, CheckInput, RepUsage};
use apb_engine::eval::store::{self, CaseResult, ConfigKey, EvalResult, Repetition};
use apb_engine::run_wait::WaitReason;
use serde_json::json;

use crate::util::print_json;

/// What `apb eval` was asked.
pub(crate) struct EvalArgs {
    pub id: String,
    pub version: Option<String>,
    pub cases: Vec<String>,
    pub tags: Vec<String>,
    pub repeat: Option<u32>,
    pub overrides: Option<PathBuf>,
    pub profile_overrides: Vec<String>,
    pub model: Option<String>,
    pub max_usd: Option<f64>,
    pub compare: bool,
    pub dry_run: bool,
    /// Evaluate a draft: the scratch copy is marked active, the project's
    /// lifecycle is never touched (the trial path's lifecycle exception).
    pub draft: bool,
    pub yes: bool,
    pub json: bool,
}

/// Printed once per invocation: the honest boundary of an eval run.
const NOT_A_SANDBOX: &str = "note: an eval run is not a sandbox: it runs in a scratch repository with no real remote, but agents keep the network and any CLI you are logged in to; use the case `env` to cut known ones (for example GH_CONFIG_DIR)";

/// How often the wait loop re-reads the journal for limits.
const POLL: Duration = Duration::from_secs(2);
/// How long a run that crossed a limit gets to finish on its own.
const LIMIT_GRACE: Duration = Duration::from_secs(5);
/// How long a stopped run's driver gets to exit before the tree is kept.
const DRIVER_EXIT_WAIT: Duration = Duration::from_secs(30);

fn fail(json_out: bool, code: &str, message: String) -> ExitCode {
    if json_out {
        print_json(&json!({ "error": code, "message": message }));
    } else {
        eprintln!("eval: {message}");
    }
    ExitCode::from(2)
}

fn git(dir: &Path, args: &[&str]) -> Result<String, String> {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args([
            "-c",
            "user.name=apb eval",
            "-c",
            "user.email=eval@apb.invalid",
            "-c",
            "commit.gpgsign=false",
            "-c",
            "init.defaultBranch=main",
        ])
        .args(args)
        .stdin(Stdio::null())
        .output()
        .map_err(|e| format!("git {}: {e}", args.join(" ")))?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
    } else {
        Err(format!(
            "git {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&out.stderr).trim()
        ))
    }
}

/// The overrides of this invocation: the file, then `--profile-override
/// NODE=PROFILE`, then `--model AGENT:MODEL` on every agent node.
fn build_overrides(args: &EvalArgs, playbook: &Playbook) -> Result<Option<RunOverrides>, String> {
    let mut ov = match &args.overrides {
        Some(p) => std::fs::read_to_string(p)
            .map_err(|e| e.to_string())
            .and_then(|raw| RunOverrides::from_yaml(&raw))
            .map_err(|e| format!("bad --overrides `{}`: {e}", p.display()))?,
        None => RunOverrides::default(),
    };
    for spec in &args.profile_overrides {
        let (node, profile) = spec
            .split_once('=')
            .ok_or_else(|| format!("bad --profile-override `{spec}` (expected NODE=PROFILE)"))?;
        let pref: apb_core::profile::QualifiedProfileRef =
            serde_yaml_ng::from_str(&serde_json::to_string(profile).unwrap_or_default())
                .map_err(|e| format!("bad --profile-override `{spec}`: {e}"))?;
        ov.nodes.entry(node.to_string()).or_default().profile = Some(pref);
    }
    if let Some(m) = &args.model {
        let (agent, model) = m
            .split_once(':')
            .filter(|(a, m)| !a.is_empty() && !m.is_empty())
            .ok_or_else(|| format!("bad --model `{m}` (expected AGENT:MODEL)"))?;
        for n in &playbook.nodes {
            if matches!(n.kind, NodeKind::AgentTask { .. }) {
                ov.nodes
                    .entry(n.id.clone())
                    .or_insert_with(NodeOverride::default)
                    .ephemeral_executor = Some(EphemeralExecutor {
                    agent: agent.to_string(),
                    model: model.to_string(),
                });
            }
        }
    }
    if ov.is_empty() {
        return Ok(None);
    }
    let mut probe = playbook.clone();
    ov.apply(&mut probe)?;
    Ok(Some(ov))
}

fn selected<'a>(
    args: &EvalArgs,
    suite: &'a LoadedSuite,
    version: &str,
) -> Result<Vec<&'a LoadedCase>, String> {
    for c in &args.cases {
        if !suite.cases.iter().any(|lc| &lc.case.id == c) {
            return Err(format!("no eval case `{c}`"));
        }
    }
    Ok(suite
        .cases
        .iter()
        .filter(|lc| args.cases.is_empty() || args.cases.contains(&lc.case.id))
        .filter(|lc| {
            args.tags.is_empty() || {
                let tags = lc.tags(&suite.suite);
                args.tags.iter().any(|t| tags.contains(t))
            }
        })
        .filter(|lc| core_eval::applies_to(&lc.case, version))
        .collect())
}

/// Asks once on a terminal; refuses elsewhere unless `--yes`.
fn confirm(args: &EvalArgs, question: &str) -> bool {
    if args.yes {
        return true;
    }
    if !(std::io::stdin().is_terminal() && std::io::stderr().is_terminal()) {
        return false;
    }
    eprint!("{question} [y/N] ");
    let mut line = String::new();
    std::io::stdin().read_line(&mut line).is_ok() && line.trim().eq_ignore_ascii_case("y")
}

/// The files that make a playbook runnable in a scratch tree: the playbook's
/// own directory without its suite, and the project profiles, skills and
/// config.
fn copy_definitions(root: &Path, id: &str, tree: &Path, draft: bool) -> std::io::Result<()> {
    let src = root.join(".apb");
    let dst = tree.join(".apb");
    let pb_dst = dst.join("playbooks").join(id);
    if pb_dst.exists() {
        std::fs::remove_dir_all(&pb_dst)?;
    }
    std::fs::create_dir_all(&pb_dst)?;
    for entry in std::fs::read_dir(src.join("playbooks").join(id))? {
        let entry = entry?;
        if entry.file_name() == core_eval::EVALS_DIR {
            continue;
        }
        let to = pb_dst.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            apb_core::fsutil::copy_tree(&entry.path(), &to)?;
        } else {
            std::fs::copy(entry.path(), &to)?;
        }
    }
    if draft {
        apb_core::trust::write_lifecycle(&pb_dst, apb_core::trust::Lifecycle::Active)?;
    }
    for sub in ["profiles", "skills"] {
        let s = src.join(sub);
        if s.is_dir() {
            let d = dst.join(sub);
            if d.exists() {
                std::fs::remove_dir_all(&d)?;
            }
            apb_core::fsutil::copy_tree(&s, &d)?;
        }
    }
    if src.join("config.yaml").is_file() {
        std::fs::copy(src.join("config.yaml"), dst.join("config.yaml"))?;
    }
    Ok(())
}

/// Local state of the scratch tree that must never be committed.
const TREE_EXCLUDES: &str = ".apb/runs/\n.apb/cache/\n.apb/trash/\n.apb/locks/\n.apb/workspace.local\n.apb/workdir.lock\n.apb/decisions.jsonl\n.apb/secrets.env\n";

/// Builds the repetition's repository; returns the fixture commit.
fn materialize(
    root: &Path,
    id: &str,
    lc: &LoadedCase,
    git_commit: Option<&str>,
    suite_copy: &Path,
    rep_dir: &Path,
    draft: bool,
) -> Result<String, String> {
    let tree = rep_dir.join("tree");
    std::fs::create_dir_all(&tree).map_err(|e| e.to_string())?;
    let fx = &lc.case.fixture;
    if let Some(commit) = git_commit {
        let tar = rep_dir.join("fixture.tar");
        git(
            root,
            &[
                "archive",
                "--format=tar",
                "-o",
                &tar.to_string_lossy(),
                "--end-of-options",
                commit,
            ],
        )?;
        let st = Command::new("tar")
            .arg("-xf")
            .arg(&tar)
            .arg("-C")
            .arg(&tree)
            .status()
            .map_err(|e| format!("tar: {e}"))?;
        let _ = std::fs::remove_file(&tar);
        if !st.success() {
            return Err("tar could not unpack the fixture".into());
        }
    }
    if let Some(d) = &fx.dir {
        apb_core::fsutil::copy_tree(&suite_copy.join(d), &tree).map_err(|e| e.to_string())?;
    }
    git(&tree, &["init", "-q", "-b", "main"])?;
    copy_definitions(root, id, &tree, draft)
        .map_err(|e| format!("copying the definitions: {e}"))?;
    apb_core::registry::init_project(&tree).map_err(|e| e.to_string())?;
    std::fs::write(tree.join(".git/info/exclude"), TREE_EXCLUDES).map_err(|e| e.to_string())?;
    git(&tree, &["add", "-A"])?;
    git(
        &tree,
        &["commit", "-q", "--no-verify", "-m", "eval fixture"],
    )?;
    let remote = rep_dir.join("remote.git");
    git(
        rep_dir,
        &["init", "-q", "--bare", &remote.to_string_lossy()],
    )?;
    git(
        &tree,
        &["remote", "add", "origin", &remote.to_string_lossy()],
    )?;
    git(&tree, &["push", "-q", "origin", "main"])?;
    if let Some(change) = &fx.change {
        let branch = fx
            .branch
            .clone()
            .unwrap_or_else(|| core_eval::DEFAULT_CHANGE_BRANCH.to_string());
        git(&tree, &["checkout", "-q", "-b", &branch])?;
        apb_core::fsutil::copy_tree(&suite_copy.join(change), &tree).map_err(|e| e.to_string())?;
        git(&tree, &["add", "-A"])?;
        git(
            &tree,
            &["commit", "-q", "--no-verify", "-m", "eval fixture change"],
        )?;
    }
    git(&tree, &["rev-parse", "HEAD"])
}

/// The case env with `{{eval.scratch}}` expanded.
fn overlay(env: &BTreeMap<String, String>, rep_dir: &Path) -> Vec<(String, String)> {
    let scratch = rep_dir.to_string_lossy();
    env.iter()
        .map(|(k, v)| (k.clone(), v.replace("{{eval.scratch}}", &scratch)))
        .collect()
}

struct Started {
    run_id: String,
}

fn start_run(
    args: &EvalArgs,
    lc: &LoadedCase,
    version: &str,
    tree: &Path,
    rep_dir: &Path,
    overrides_file: Option<&Path>,
    env: &[(String, String)],
) -> Result<Started, String> {
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let mut cmd = Command::new(exe);
    cmd.current_dir(tree)
        .args([
            "run",
            &args.id,
            "--version",
            version,
            "--detach",
            "--no-cache",
        ])
        .args(["--execution", "cli"])
        .env("APB_NO_REGISTRY", "1")
        .env("APB_EVAL_SCRATCH", rep_dir)
        .stdin(Stdio::null());
    if let Some(i) = &lc.case.instruction {
        cmd.args(["--instruction", i]);
    }
    for (k, v) in &lc.case.params {
        cmd.args(["--param", &format!("{k}={v}")]);
    }
    if let Some(f) = overrides_file {
        cmd.arg("--overrides").arg(f);
    }
    for (k, v) in env {
        cmd.env(k, v);
    }
    let out = cmd
        .output()
        .map_err(|e| format!("cannot start apb run: {e}"))?;
    let stdout = String::from_utf8_lossy(&out.stdout);
    match stdout
        .lines()
        .find_map(|l| l.strip_prefix("run started: "))
        .map(|s| s.split_whitespace().next().unwrap_or_default().to_string())
    {
        Some(run_id) if out.status.success() && !run_id.is_empty() => Ok(Started { run_id }),
        _ => Err(format!(
            "the run did not start: {}{}",
            String::from_utf8_lossy(&out.stderr).trim(),
            stdout.trim()
        )),
    }
}

/// Why the runner stopped a run, and whether it was a limit (verdict
/// `incomplete`) rather than the run's own behaviour.
struct StopReason {
    text: String,
    limit: bool,
}

/// A per-repetition limit the run's reported usage crossed.
fn over_limit(usage: &RepUsage, limits: &core_eval::Limits) -> Option<String> {
    if let Some(max) = limits.max_tokens
        && usage.tokens() > max
    {
        return Some(format!(
            "limit: {} tokens > max_tokens {max}",
            usage.tokens()
        ));
    }
    let cost = usage.cost_usd.unwrap_or(0.0);
    if let Some(max) = limits.max_usd
        && cost > max
    {
        return Some(format!("limit: ${cost:.4} > max_usd {max}"));
    }
    None
}

/// Follows the run until it is terminal; stops it when it must be stopped.
///
/// Usage is only known once an attempt finishes, so a limit can be crossed
/// by up to one attempt's spend. Crossing one stops the run unless it ends
/// within [`LIMIT_GRACE`] (the spend is already made, and stopping right
/// before the finish node would only lose the result). The invocation
/// budget never stops a running repetition; it keeps the next one from
/// starting.
fn follow(tree: &Path, run_id: &str, limits: &core_eval::Limits) -> Option<StopReason> {
    let timeout = Duration::from_secs(
        limits
            .timeout_secs()
            .unwrap_or(core_eval::DEFAULT_TIMEOUT_SECS),
    );
    let deadline = Instant::now() + timeout;
    let run_dir = tree.join(".apb/runs").join(run_id);
    let reason = loop {
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            break Some(StopReason {
                text: format!("timeout after {}s", timeout.as_secs()),
                limit: true,
            });
        }
        match apb_engine::run_wait::wait_run(tree, run_id, left.min(POLL)) {
            Ok(r) if r.reason == WaitReason::Finished => break None,
            Ok(r) if r.reason == WaitReason::NeedsInput => {
                let needs = r
                    .needs
                    .map(|n| format!("{n:?}").to_lowercase())
                    .unwrap_or_else(|| "input".into());
                break Some(StopReason {
                    text: format!("eval_gate_unanswered: the run waits for {needs}"),
                    limit: false,
                });
            }
            Ok(r) if r.reason == WaitReason::Stopped => {
                break Some(StopReason {
                    text: "the run stopped progressing".into(),
                    limit: false,
                });
            }
            Ok(_) => {
                let events = apb_engine::run_view::read_events(&run_dir).unwrap_or_default();
                if let Some(text) = over_limit(&RepUsage::from_events(&events), limits) {
                    match apb_engine::run_wait::wait_run(tree, run_id, LIMIT_GRACE) {
                        Ok(r) if r.reason == WaitReason::Finished => break None,
                        _ => break Some(StopReason { text, limit: true }),
                    }
                }
            }
            Err(e) => {
                break Some(StopReason {
                    text: format!("wait failed: {e}"),
                    limit: false,
                });
            }
        }
    };
    if reason.is_some() {
        let _ = apb_engine::stop_run(tree, run_id);
    }
    reason
}

/// Waits for the run's driver process to exit; `false` when it is still
/// alive at the deadline (the tree is then kept).
fn driver_gone(run_dir: &Path, run_id: &str) -> bool {
    let deadline = Instant::now() + DRIVER_EXIT_WAIT;
    while apb_engine::liveness::driver_alive(run_dir, run_id) == Some(true) {
        if Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    true
}

/// Moves `from` to `to`, copying when a rename cannot cross filesystems.
fn move_dir(from: &Path, to: &Path) -> std::io::Result<()> {
    if let Some(p) = to.parent() {
        std::fs::create_dir_all(p)?;
    }
    if std::fs::rename(from, to).is_ok() {
        return Ok(());
    }
    apb_core::fsutil::copy_tree(from, to)?;
    std::fs::remove_dir_all(from)
}

/// Copies every file a `files` check names that exists in the tree into
/// `<stored run>/eval-files/`, so a failed check can be read after the
/// scratch tree is gone.
fn keep_checked_files(case: &core_eval::EvalCase, tree: &Path, stored: &Path) {
    for f in &case.checks.files {
        let from = tree.join(&f.path);
        if from.is_file() {
            let to = stored.join("eval-files").join(&f.path);
            if let Some(p) = to.parent()
                && std::fs::create_dir_all(p).is_ok()
            {
                let _ = std::fs::copy(&from, &to);
            }
        }
    }
}

/// One selected case of the invocation.
struct Planned<'a> {
    lc: &'a LoadedCase,
    repeat: u32,
    /// The commit a `git:` fixture resolved to, once for every repetition.
    fixture_commit: Option<Result<String, String>>,
    digest: String,
}

/// A fixture ref of this repository as a commit id.
fn resolve_ref(root: &Path, r: &str) -> Result<String, String> {
    git(
        root,
        &[
            "rev-parse",
            "--verify",
            "--end-of-options",
            &format!("{r}^{{commit}}"),
        ],
    )
}

struct RepContext<'a> {
    root: &'a Path,
    args: &'a EvalArgs,
    playbook: &'a Playbook,
    version: &'a str,
    suite: &'a LoadedSuite,
    suite_copy: &'a Path,
    scratch: &'a Path,
    evals_home: &'a Path,
    overrides_file: Option<&'a Path>,
}

fn error_rep(n: u32, detail: String, kept: Option<String>) -> Repetition {
    Repetition {
        repetition: n,
        run_id: None,
        run_dir: None,
        verdict: "error".into(),
        outcome: "not_started".into(),
        stopped: None,
        checks: vec![checks::CheckResult {
            kind: "start".into(),
            status: checks::CheckStatus::Error,
            detail: Some(detail),
        }],
        goal: Vec::new(),
        usage: RepUsage::default(),
        duration_ms: None,
        kept_worktree: kept,
    }
}

/// One repetition end to end. Returns the repetition and, when the run
/// started, its stored run directory.
fn run_repetition(cx: &RepContext, p: &Planned, n: u32) -> (Repetition, Option<PathBuf>) {
    let lc = p.lc;
    let rep_dir = cx.scratch.join(format!("{}-{n}", lc.case.id));
    let tree = rep_dir.join("tree");
    let fixture_commit = match p.fixture_commit.clone().transpose().and_then(|commit| {
        materialize(
            cx.root,
            &cx.args.id,
            lc,
            commit.as_deref(),
            cx.suite_copy,
            &rep_dir,
            cx.args.draft,
        )
    }) {
        Ok(c) => c,
        Err(e) => {
            let _ = std::fs::remove_dir_all(&rep_dir);
            return (error_rep(n, format!("fixture: {e}"), None), None);
        }
    };
    let env = overlay(&lc.env(&cx.suite.suite), &rep_dir);
    let started = match start_run(
        cx.args,
        lc,
        cx.version,
        &tree,
        &rep_dir,
        cx.overrides_file,
        &env,
    ) {
        Ok(s) => s,
        Err(e) => {
            let _ = std::fs::remove_dir_all(&rep_dir);
            return (error_rep(n, e, None), None);
        }
    };
    let limits = lc.case.limits.over(&cx.suite.suite.limits);
    let stop = follow(&tree, &started.run_id, &limits);
    let run_dir = tree.join(".apb/runs").join(&started.run_id);
    let clean = driver_gone(&run_dir, &started.run_id);
    let events = apb_engine::run_view::read_events(&run_dir).unwrap_or_default();
    let raw_types = checks::raw_event_types(&run_dir);
    let script_env = vec![
        ("APB_EVAL_RUN_DIR", run_dir.to_string_lossy().into_owned()),
        ("APB_RUN_ID", started.run_id.clone()),
        ("APB_EVAL_CASE", lc.case.id.clone()),
        ("APB_EVAL_REPETITION", n.to_string()),
        ("APB_EVAL_SCRATCH", rep_dir.to_string_lossy().into_owned()),
    ];
    let input = CheckInput {
        case: &lc.case,
        playbook: cx.playbook,
        events: &events,
        raw_types: &raw_types,
        tree: &tree,
        fixture_commit: &fixture_commit,
        suite_dir: cx.suite_copy,
        stopped: stop.as_ref().map(|s| s.text.as_str()),
        script_env,
    };
    let (check_results, goal) = checks::evaluate(&input);
    let verdict = if stop.as_ref().is_some_and(|s| s.limit) {
        "incomplete".to_string()
    } else {
        checks::verdict(&check_results).to_string()
    };
    let stored = cx
        .evals_home
        .join("runs")
        .join(&cx.args.id)
        .join(&started.run_id);
    let moved = move_dir(&run_dir, &stored).is_ok();
    if moved {
        keep_checked_files(&lc.case, &tree, &stored);
    }
    let kept = if clean {
        let _ = std::fs::remove_dir_all(&rep_dir);
        None
    } else {
        Some(tree.to_string_lossy().into_owned())
    };
    let rep = Repetition {
        repetition: n,
        run_id: Some(started.run_id.clone()),
        run_dir: moved.then(|| stored.to_string_lossy().into_owned()),
        verdict,
        outcome: checks::outcome(&events, stop.as_ref().map(|s| s.text.as_str())),
        stopped: stop.map(|s| s.text),
        checks: check_results,
        goal,
        usage: RepUsage::from_events(&events),
        duration_ms: checks::duration_ms(&events),
        kept_worktree: kept,
    };
    (rep, moved.then_some(stored))
}

fn overrides_digest(ov: Option<&RunOverrides>) -> Option<String> {
    ov.map(|o| {
        apb_core::content::sha256_hex(serde_json::to_string(o).unwrap_or_default().as_bytes())
    })
}

fn print_compare(r: &EvalResult, all: &[EvalResult], json_out: bool) -> Option<serde_json::Value> {
    let base = store::baseline_for(all, r)?;
    let c = store::compare(base, r);
    if json_out {
        Some(serde_json::to_value(&c).unwrap_or_default())
    } else {
        print!("{}", store::render_comparison(&c));
        None
    }
}

/// `apb eval <id> --compare`: the latest stored result against the one
/// before it, nothing run.
fn compare_only(evals_home: &Path, args: &EvalArgs) -> ExitCode {
    let all = store::load_all(evals_home, &args.id);
    let Some(latest) = all.last() else {
        return fail(
            args.json,
            "no_results",
            format!("no stored eval results for `{}`", args.id),
        );
    };
    if store::baseline_for(&all, latest).is_none() {
        let msg = format!(
            "only one stored eval result for `{}`: nothing to compare",
            args.id
        );
        if args.json {
            print_json(&json!({ "latest": latest.eval_id, "comparison": null, "note": msg }));
        } else {
            println!("{msg}");
        }
        return ExitCode::SUCCESS;
    }
    if args.json {
        let c = print_compare(latest, &all, true);
        print_json(&json!({ "latest": latest.eval_id, "comparison": c }));
    } else {
        println!("latest: {}", latest.eval_id);
        print_compare(latest, &all, false);
    }
    ExitCode::SUCCESS
}

pub(crate) fn eval_cmd(root: &Path, args: EvalArgs) -> ExitCode {
    let Some(evals_home) = core_eval::evals_home() else {
        return fail(args.json, "no_config_dir", "no apb config directory".into());
    };
    if args.compare {
        return compare_only(&evals_home, &args);
    }
    let reg = match Registry::open(root) {
        Ok(r) => r,
        Err(e) => return fail(args.json, "no_project", format!("no project here: {e}")),
    };
    let loaded = match reg.load(&args.id, args.version.as_deref()) {
        Ok(l) => l,
        Err(e) => return fail(args.json, "not_found", e.to_string()),
    };
    let playbook = loaded.playbook.clone();
    let version = loaded.version.clone();
    let reasons = core_eval::refusal(&playbook);
    if !reasons.is_empty() {
        if args.json {
            print_json(
                &json!({ "refused": "eval_refused_effects", "id": args.id, "reasons": reasons }),
            );
        } else {
            eprintln!("eval: refused, `{}` cannot be evaluated:", args.id);
            for r in &reasons {
                eprintln!("  {r}");
            }
        }
        return ExitCode::from(2);
    }
    let playbook_dir = root.join(".apb/playbooks").join(&args.id);
    match apb_core::trust::read_lifecycle(&playbook_dir) {
        apb_core::trust::Lifecycle::Active => {}
        apb_core::trust::Lifecycle::Draft if args.draft => {}
        apb_core::trust::Lifecycle::Draft => {
            return fail(
                args.json,
                "draft_requires_draft_flag",
                format!(
                    "`{}` is a draft: pass --draft to evaluate it (only the scratch copy is marked active)",
                    args.id
                ),
            );
        }
        other => {
            return fail(
                args.json,
                "lifecycle",
                format!(
                    "`{}` is {}: it cannot be evaluated",
                    args.id,
                    other.as_str()
                ),
            );
        }
    }
    let issues = core_eval::validate_suite(&playbook_dir, &playbook);
    let errors: Vec<String> = issues
        .iter()
        .filter(|i| i.severity == apb_core::validate::Severity::Error)
        .map(|i| format!("{} {}", i.code, i.message))
        .collect();
    if !core_eval::has_suite(&playbook_dir) {
        return fail(
            args.json,
            "no_suite",
            format!("`{}` has no evals/ directory", args.id),
        );
    }
    if !errors.is_empty() {
        return fail(
            args.json,
            "invalid_suite",
            format!("the suite is invalid:\n  {}", errors.join("\n  ")),
        );
    }
    let overrides = match build_overrides(&args, &playbook) {
        Ok(o) => o,
        Err(e) => return fail(args.json, "bad_overrides", e),
    };
    let eval_id = format!("eval-{}", apb_core::clock::now_ms());
    let scratch = evals_home.join("scratch").join(&eval_id);
    let suite_copy = scratch.join(core_eval::EVALS_DIR);
    // The suite runs from a copy made now, and the digest a person approves
    // is computed in the same pass over the same bytes, so it is the content
    // that runs.
    let suite_digest = match apb_core::content::snapshot_tree(
        &core_eval::suite_dir(&playbook_dir),
        &suite_copy,
        &core_eval::suite_limits(),
    ) {
        Ok(d) => d,
        Err(e) => {
            let _ = std::fs::remove_dir_all(&scratch);
            return fail(
                args.json,
                "invalid_suite",
                format!("the suite cannot be digested: {e}"),
            );
        }
    };
    let outcome = run_eval(
        root,
        &args,
        &playbook,
        &version,
        &evals_home,
        &scratch,
        &suite_copy,
        &suite_digest,
        overrides.as_ref(),
        &eval_id,
    );
    let _ = std::fs::remove_dir_all(&scratch);
    let _ = std::fs::remove_dir(evals_home.join("scratch"));
    outcome
}

/// The agent nodes whose profile (after `overrides`) declares
/// `environment: full`, as `(node, profile)`. Such a node loads the
/// operator's user-scope hooks, plugins and MCP servers in the scratch run
/// (docs/PROFILES.md, "Agent environment"), so the plan names them. An
/// unresolvable profile is skipped here: the run gate reports it.
fn full_environment_nodes(
    root: &Path,
    playbook: &Playbook,
    overrides: Option<&RunOverrides>,
) -> Vec<(String, String)> {
    let mut out = Vec::new();
    for n in &playbook.nodes {
        let overridden = overrides
            .and_then(|o| o.nodes.get(&n.id))
            .and_then(|o| o.profile.clone());
        let Some(pref) = overridden.or_else(|| n.kind.effective_profile_ref(&playbook.defaults))
        else {
            continue;
        };
        if let Ok(p) = apb_core::profile_store::resolve_profile(
            root,
            apb_core::profile_store::PlaybookOrigin::Project,
            &pref,
        ) && p.doc.environment() == apb_core::profile::AgentEnvironment::Full
        {
            out.push((n.id.clone(), p.name));
        }
    }
    out
}

#[allow(clippy::too_many_arguments)]
fn run_eval(
    root: &Path,
    args: &EvalArgs,
    playbook: &Playbook,
    version: &str,
    evals_home: &Path,
    scratch: &Path,
    suite_copy: &Path,
    suite_digest: &str,
    overrides: Option<&RunOverrides>,
    eval_id: &str,
) -> ExitCode {
    let suite = core_eval::load_suite(scratch);
    let cases = match selected(args, &suite, version) {
        Ok(c) => c,
        Err(e) => return fail(args.json, "unknown_case", e),
    };
    if cases.is_empty() {
        return fail(
            args.json,
            "no_cases",
            format!(
                "no eval case of `{}` applies to version {version} with this selection",
                args.id
            ),
        );
    }
    let budget = args
        .max_usd
        .or(suite.suite.budget.max_usd_per_invocation)
        .unwrap_or(core_eval::DEFAULT_MAX_USD_PER_INVOCATION);
    let mut planned: Vec<Planned> = Vec::new();
    for lc in &cases {
        // A `git:` fixture ref is resolved once per case: every repetition
        // materializes the same commit, and the case digest names it.
        let fixture_commit = lc.case.fixture.git.as_ref().map(|r| resolve_ref(root, r));
        let resolved = fixture_commit.as_ref().and_then(|c| c.as_ref().ok());
        let digest = match core_eval::case_digest(lc, resolved.map(String::as_str)) {
            Ok(d) => d,
            Err(e) => {
                return fail(
                    args.json,
                    "invalid_suite",
                    format!("the case `{}` cannot be digested: {e}", lc.case.id),
                );
            }
        };
        planned.push(Planned {
            lc,
            repeat: args
                .repeat
                .unwrap_or_else(|| lc.repeat(&suite.suite))
                .max(1),
            fixture_commit,
            digest,
        });
    }
    let total: u32 = planned.iter().map(|p| p.repeat).sum();
    let mut approvals = core_eval::SuiteApprovals::load();
    let approved = approvals.is_approved(suite_digest);
    if !args.json {
        eprintln!("{NOT_A_SANDBOX}");
        for (node, profile) in full_environment_nodes(root, playbook, overrides) {
            eprintln!(
                "note: node `{node}` runs profile `{profile}` with `environment: full`: the operator's own agent setup (user-scope hooks, plugins, MCP servers) loads in its eval runs and can act outside the scratch repository"
            );
        }
    }
    let plan_line = format!(
        "plan: {} {version}, {} case(s), {total} run(s), invocation budget ${budget:.2}; suite {} {}",
        args.id,
        planned.len(),
        &suite_digest[..suite_digest.len().min(19)],
        if approved {
            "approved"
        } else {
            "not approved on this machine"
        }
    );
    if args.dry_run {
        if args.json {
            print_json(&json!({
                "plan": {
                    "playbook": args.id, "version": version, "budget_usd": budget,
                    "suite_digest": suite_digest, "suite_approved": approved,
                    "cases": planned.iter().map(|p| json!({"case": p.lc.case.id, "repeat": p.repeat})).collect::<Vec<_>>(),
                }
            }));
        } else {
            println!("{plan_line}");
            for p in &planned {
                println!("  {} x{}", p.lc.case.id, p.repeat);
            }
        }
        return ExitCode::SUCCESS;
    }
    if !args.json {
        eprintln!("{plan_line}");
    }
    let question = if approved {
        format!("start {total} run(s)?")
    } else {
        "the suite's scripts are not approved on this machine; approve and start?".to_string()
    };
    if !confirm(args, &question) {
        return fail(
            args.json,
            "not_confirmed",
            "refused without confirmation (pass --yes to approve the suite and start)".into(),
        );
    }
    if !approved && let Err(e) = approvals.approve(suite_digest, &args.id) {
        return fail(
            args.json,
            "approval",
            format!("cannot record the suite approval: {e}"),
        );
    }
    let overrides_file = match overrides {
        Some(o) => {
            let f = scratch.join("overrides.yaml");
            match serde_yaml_ng::to_string(o)
                .map_err(|e| e.to_string())
                .and_then(|y| std::fs::write(&f, y).map_err(|e| e.to_string()))
            {
                Ok(()) => Some(f),
                Err(e) => {
                    return fail(
                        args.json,
                        "scratch",
                        format!("cannot write the overrides: {e}"),
                    );
                }
            }
        }
        None => None,
    };
    let cx = RepContext {
        root,
        args,
        playbook,
        version,
        suite: &suite,
        suite_copy,
        scratch,
        evals_home,
        overrides_file: overrides_file.as_deref(),
    };
    let started_at_ms = apb_core::clock::now_ms();
    let mut spent = 0.0_f64;
    let mut tokens = 0_u64;
    let mut incomplete: Option<String> = None;
    let mut key: Option<ConfigKey> = None;
    let mut results = Vec::new();
    for p in &planned {
        let lc = p.lc;
        let mut reps = Vec::new();
        for n in 1..=p.repeat {
            if incomplete.is_some() {
                break;
            }
            if spent >= budget {
                incomplete = Some(format!("invocation budget ${budget:.2} spent"));
                break;
            }
            if !args.json {
                eprintln!("running {} #{n}", lc.case.id);
            }
            let (rep, stored) = run_repetition(&cx, p, n);
            spent += rep.usage.cost_usd.unwrap_or(0.0);
            tokens += rep.usage.tokens();
            if key.is_none()
                && let Some(dir) = &stored
            {
                let events = apb_engine::run_view::read_events(dir).unwrap_or_default();
                key = Some(ConfigKey::from_run(
                    dir,
                    &events,
                    overrides_digest(overrides),
                ));
            }
            reps.push(rep);
        }
        if !reps.is_empty() {
            results.push(CaseResult::new(&lc.case.id, &p.digest, reps));
        }
    }
    let config = key.unwrap_or_else(|| ConfigKey {
        playbook_digest: Registry::open(root)
            .ok()
            .and_then(|r| r.load(&args.id, Some(version)).ok())
            .and_then(|l| l.trust_digest().ok())
            .unwrap_or_default(),
        overrides_digest: overrides_digest(overrides),
        ..Default::default()
    });
    let result = EvalResult {
        eval_id: eval_id.to_string(),
        playbook: args.id.clone(),
        version: version.to_string(),
        started_at_ms,
        finished_at_ms: apb_core::clock::now_ms(),
        apb_version: env!("CARGO_PKG_VERSION").to_string(),
        workspace: root.to_string_lossy().into_owned(),
        config_key: config.key(),
        config,
        cases: results,
        incomplete,
        total_cost_usd: (spent * 1e6).round() / 1e6,
        total_tokens: tokens,
    };
    let stored_at = store::store(evals_home, &result);
    let all = store::load_all(evals_home, &args.id);
    let (p, o) = result.passes();
    if args.json {
        let cmp = print_compare(&result, &all, true);
        print_json(&json!({
            "result": result,
            "comparison": cmp,
            "stored": stored_at.as_ref().ok().map(|p| p.to_string_lossy().into_owned()),
        }));
    } else {
        print!("{}", store::render_result(&result));
        match &stored_at {
            Ok(p) => println!("stored: {}", p.display()),
            Err(e) => eprintln!("eval: the result could not be stored: {e}"),
        }
        if print_compare(&result, &all, false).is_none()
            && store::baseline_for(&all, &result).is_none()
        {
            println!("no earlier result to compare with");
        }
    }
    if p == o && o > 0 && result.incomplete.is_none() {
        ExitCode::SUCCESS
    } else {
        ExitCode::from(1)
    }
}
