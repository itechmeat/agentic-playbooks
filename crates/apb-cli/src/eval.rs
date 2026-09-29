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
//!    through the ordinary run gate, with `APB_NO_REGISTRY=1` so the scratch
//!    tree never enters the projects registry. The case env overlay and the
//!    wall-clock deadline go to the engine as run settings
//!    (`--eval-settings`): the overlay reaches only the agents and scripts
//!    the run spawns, never apb itself.
//! 3. The wait: the run is followed with the `apb wait` primitive and
//!    stopped when it waits for a person (gates cannot be answered yet),
//!    when its wall clock, token or cost limit is crossed; a spent
//!    invocation budget keeps the next repetition from starting.
//! 4. The checks (`apb_engine::eval::checks`), then the run directory moves
//!    to `<config-dir>/evals/runs/<playbook>/<run-id>` (outside the project,
//!    so `apb stats` never counts it) and the scratch directory is removed.
//!    A repetition whose driver does not exit is moved to
//!    `<config-dir>/evals/kept/` instead: a tree is never deleted under a
//!    live driver.
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
const NOT_A_SANDBOX: &str = "note: an eval run is not a sandbox: it runs in a scratch repository with no real remote, but agents keep the network, your environment (apart from connector variables) and any CLI or git credential helper you are logged in to; use the case `env` to cut known ones (for example GH_CONFIG_DIR)";

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

/// Git for the runner, hardened (`apb_engine::eval::git`): the operator's
/// `GIT_DIR` and friends are removed, hooks come from the empty `hooks`
/// directory, no filesystem monitor runs and only the local transport is
/// allowed.
fn git(hooks: &Path, dir: &Path, args: &[&str]) -> Result<String, String> {
    let out = apb_engine::eval::git::command(hooks)
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
/// config. Nothing is removed or written through a symlink: the fixture's
/// `.apb` was checked by [`check_fixture_layer`], and every write below
/// goes through the no-follow copier.
fn copy_definitions(root: &Path, id: &str, tree: &Path, draft: bool) -> std::io::Result<()> {
    use apb_core::fsutil::{copy_tree_no_follow, ensure_no_symlink_below};
    let src = root.join(".apb");
    let dst = tree.join(".apb");
    let pb_dst = dst.join("playbooks").join(id);
    ensure_no_symlink_below(tree, &pb_dst)?;
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
        let ft = entry.file_type()?;
        if ft.is_dir() {
            copy_tree_no_follow(&entry.path(), tree, &to)?;
        } else if ft.is_file() {
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
            ensure_no_symlink_below(tree, &d)?;
            if d.exists() {
                std::fs::remove_dir_all(&d)?;
            }
            copy_tree_no_follow(&s, tree, &d)?;
        }
    }
    if src.join("config.yaml").is_file() {
        let to = dst.join("config.yaml");
        ensure_no_symlink_below(tree, &to)?;
        std::fs::copy(src.join("config.yaml"), to)?;
    }
    Ok(())
}

/// Where a symlink at `link` (inside `tree`) points, refused when the
/// target is absolute or resolves outside `tree`. A link whose target does
/// not exist yet is judged lexically.
fn link_stays_inside(tree_canon: &Path, tree: &Path, link: &Path) -> Result<(), String> {
    let target = std::fs::read_link(link).map_err(|e| e.to_string())?;
    let rel = link.strip_prefix(tree).unwrap_or(link);
    if target.is_absolute() || target.has_root() {
        return Err(format!(
            "`{}` is a symlink to the absolute path `{}`",
            rel.display(),
            target.display()
        ));
    }
    let base = link.parent().unwrap_or(tree);
    let inside = match std::fs::canonicalize(base.join(&target)) {
        Ok(c) => c.starts_with(tree_canon),
        Err(_) => {
            // Lexically: from the link's directory, never above the tree.
            let mut depth: i64 = rel.components().count() as i64 - 1;
            let mut ok = true;
            for c in target.components() {
                match c {
                    std::path::Component::ParentDir => depth -= 1,
                    std::path::Component::Normal(_) => depth += 1,
                    std::path::Component::CurDir => {}
                    _ => ok = false,
                }
                if depth < 0 {
                    ok = false;
                }
            }
            ok
        }
    };
    if inside {
        Ok(())
    } else {
        Err(format!(
            "`{}` is a symlink that leaves the tree (`{}`)",
            rel.display(),
            target.display()
        ))
    }
}

/// The first `.git` entry anywhere under `dir`, relative to it.
fn carries_git(dir: &Path) -> Option<PathBuf> {
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        for e in std::fs::read_dir(&d).ok()?.flatten() {
            let p = e.path();
            if e.file_name() == ".git" {
                return Some(p.strip_prefix(dir).unwrap_or(&p).to_path_buf());
            }
            if e.file_type().is_ok_and(|t| t.is_dir()) {
                stack.push(p);
            }
        }
    }
    None
}

/// Walks `tree` without following links and refuses any symlink that is
/// absolute or resolves outside it. With `fixture` set (a layer that came
/// from the fixture, not from apb): also refuses a `.git` anywhere and any
/// symlink at or below `.apb`, the paths apb itself writes next.
fn check_layer(tree: &Path, fixture: bool) -> Result<(), String> {
    let canon = std::fs::canonicalize(tree).map_err(|e| e.to_string())?;
    let mut stack = vec![tree.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let entries = std::fs::read_dir(&dir).map_err(|e| e.to_string())?;
        for e in entries {
            let e = e.map_err(|e| e.to_string())?;
            let p = e.path();
            let ft = e.file_type().map_err(|e| e.to_string())?;
            let rel = p.strip_prefix(tree).unwrap_or(&p).to_path_buf();
            if fixture && e.file_name() == ".git" {
                return Err(format!(
                    "the fixture carries `{}`; a fixture may not hold a git directory",
                    rel.display()
                ));
            }
            if ft.is_symlink() {
                if fixture && rel.starts_with(".apb") {
                    return Err(format!(
                        "`{}` is a symlink; a fixture may not hold symlinks at or below `.apb`",
                        rel.display()
                    ));
                }
                link_stays_inside(&canon, tree, &p)?;
            } else if ft.is_dir() {
                // The repository apb created is its own; nothing in it came
                // from the fixture.
                if !(dir == tree && e.file_name() == ".git") {
                    stack.push(p);
                }
            }
        }
    }
    Ok(())
}

/// Local state of the scratch tree that must never be committed.
const TREE_EXCLUDES: &str = ".apb/runs/\n.apb/cache/\n.apb/trash/\n.apb/locks/\n.apb/workspace.local\n.apb/workdir.lock\n.apb/decisions.jsonl\n.apb/secrets.env\n";

/// Builds the repetition's repository; returns the fixture commit.
#[allow(clippy::too_many_arguments)]
fn materialize(
    hooks: &Path,
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
            hooks,
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
        apb_core::fsutil::copy_tree_no_follow(&suite_copy.join(d), rep_dir, &tree)
            .map_err(|e| e.to_string())?;
    }
    // Every layer is checked before anything else writes into the tree: a
    // link the fixture planted must never redirect apb's own writes.
    check_layer(&tree, true)?;
    // The run uses the project's definitions only: whatever `.apb` the
    // fixture brought (playbooks, profiles, skills, config, a git ref's run
    // history) goes before they are copied, so it can neither replace the
    // definition the eval refusal checked nor add profiles or config the
    // project does not have. It holds no symlink (checked above).
    let fixture_apb = tree.join(".apb");
    if fixture_apb.exists() {
        std::fs::remove_dir_all(&fixture_apb)
            .map_err(|e| format!("removing the fixture's .apb: {e}"))?;
    }
    git(hooks, &tree, &["init", "-q", "-b", "main"])?;
    copy_definitions(root, id, &tree, draft)
        .map_err(|e| format!("copying the definitions: {e}"))?;
    check_layer(&tree, false)?;
    apb_core::registry::init_project(&tree).map_err(|e| e.to_string())?;
    std::fs::write(tree.join(".git/info/exclude"), TREE_EXCLUDES).map_err(|e| e.to_string())?;
    git(hooks, &tree, &["add", "-A"])?;
    git(
        hooks,
        &tree,
        &["commit", "-q", "--no-verify", "-m", "eval fixture"],
    )?;
    let remote = rep_dir.join("remote.git");
    git(
        hooks,
        rep_dir,
        &["init", "-q", "--bare", &remote.to_string_lossy()],
    )?;
    git(
        hooks,
        &tree,
        &["remote", "add", "origin", &remote.to_string_lossy()],
    )?;
    // Only `origin` is local: a push anywhere else must not find the
    // operator's credential helper, and a bare `git push` goes to origin.
    for (k, v) in [
        ("credential.helper", ""),
        ("remote.pushDefault", "origin"),
        ("push.default", "current"),
    ] {
        git(hooks, &tree, &["config", k, v])?;
    }
    git(hooks, &tree, &["push", "-q", "origin", "main"])?;
    if let Some(change) = &fx.change {
        let branch = fx
            .branch
            .clone()
            .unwrap_or_else(|| core_eval::DEFAULT_CHANGE_BRANCH.to_string());
        git(hooks, &tree, &["checkout", "-q", "-b", &branch])?;
        let overlay_src = suite_copy.join(change);
        if let Some(g) = carries_git(&overlay_src) {
            return Err(format!(
                "the change overlay carries `{}`; a fixture may not hold a git directory",
                g.display()
            ));
        }
        // The overlay may not replace what runs: the definitions the eval
        // checked live in `.apb`.
        if std::fs::symlink_metadata(overlay_src.join(".apb")).is_ok() {
            return Err(
                "the change overlay carries `.apb`; a change may not replace the playbook, profiles, skills or config the eval runs".into(),
            );
        }
        apb_core::fsutil::copy_tree_no_follow(&overlay_src, rep_dir, &tree)
            .map_err(|e| e.to_string())?;
        check_layer(&tree, false)?;
        git(hooks, &tree, &["add", "-A"])?;
        git(
            hooks,
            &tree,
            &["commit", "-q", "--no-verify", "-m", "eval fixture change"],
        )?;
    }
    git(hooks, &tree, &["rev-parse", "HEAD"])
}

/// The case env with `{{eval.scratch}}` expanded.
fn overlay(env: &BTreeMap<String, String>, rep_dir: &Path) -> BTreeMap<String, String> {
    let scratch = rep_dir.to_string_lossy();
    env.iter()
        .map(|(k, v)| (k.clone(), v.replace("{{eval.scratch}}", &scratch)))
        .collect()
}

/// `apb run --eval-settings FILE`: the settings `start_run` wrote.
pub(crate) fn read_run_settings(
    path: &Path,
) -> Result<apb_engine::run_config::EvalRunSettings, String> {
    let raw = std::fs::read(path).map_err(|e| format!("`{}`: {e}", path.display()))?;
    serde_json::from_slice(&raw).map_err(|e| format!("`{}`: {e}", path.display()))
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
    settings: &apb_engine::run_config::EvalRunSettings,
) -> Result<Started, String> {
    // The case env never reaches this process: it goes to the engine as a
    // run setting, applied only where the run spawns agents and scripts.
    let settings_file = rep_dir.join("eval-settings.json");
    let body = serde_json::to_vec(settings).map_err(|e| e.to_string())?;
    apb_core::fsutil::atomic_write_private(&settings_file, &body).map_err(|e| e.to_string())?;
    let exe = apb_core::fsutil::reexec_exe().map_err(|e| e.to_string())?;
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
        .arg("--eval-settings")
        .arg(&settings_file)
        .env("APB_NO_REGISTRY", "1")
        .env("APB_EVAL_SCRATCH", rep_dir)
        .stdin(Stdio::null());
    // The child reads the same config directory (trust store, agents,
    // connectors) as this process resolved, whatever its env says.
    if let Some(dir) = apb_core::config::config_dir() {
        cmd.env("APB_CONFIG_DIR", dir);
    }
    if let Some(i) = &lc.case.instruction {
        cmd.arg(format!("--instruction={i}"));
    }
    for (k, v) in &lc.case.params {
        cmd.args(["--param", &format!("{k}={v}")]);
    }
    if let Some(f) = overrides_file {
        cmd.arg("--overrides").arg(f);
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
        Some(run_id) if out.status.success() && apb_core::registry::is_safe_segment(&run_id) => {
            Ok(Started { run_id })
        }
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
        if interrupted() {
            break Some(StopReason {
                text: INTERRUPTED_REASON.into(),
                limit: true,
            });
        }
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

/// The deadline the engine itself enforces: the repetition's wall clock
/// plus the grace the runner gives a run that crossed a limit, so the
/// runner's own stop comes first while it is alive, and the run still ends
/// when it is not.
fn engine_deadline_ms(limits: &core_eval::Limits) -> u64 {
    let secs = limits
        .timeout_secs()
        .unwrap_or(core_eval::DEFAULT_TIMEOUT_SECS);
    let now = u64::try_from(apb_core::clock::now_ms()).unwrap_or(u64::MAX);
    now.saturating_add(secs.saturating_mul(1000))
        .saturating_add(LIMIT_GRACE.as_millis() as u64)
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

/// Moves a run directory out of the tree. The agent could have replaced
/// it, or any directory between the tree and it (`.apb`, `.apb/runs`), with
/// a symlink to a directory outside: that is refused before anything moves,
/// so an outside directory is never moved or deleted, and so is a
/// destination that is not a real directory afterwards.
fn move_run_dir(tree: &Path, from: &Path, to: &Path) -> std::io::Result<()> {
    apb_core::fsutil::ensure_no_symlink_below(tree, from).map_err(|e| {
        std::io::Error::other(format!("the run directory is behind a symlink: {e}"))
    })?;
    if std::fs::symlink_metadata(from)?.file_type().is_symlink() {
        return Err(std::io::Error::other("the run directory is a symlink"));
    }
    move_dir(from, to)?;
    if !std::fs::symlink_metadata(to)?.is_dir() {
        return Err(std::io::Error::other("the stored run is not a directory"));
    }
    Ok(())
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

/// Moves a repetition whose driver may still be alive out of the scratch
/// directory. Only a rename: a copy followed by a removal would delete the
/// tree under the live driver. On failure the repetition stays where it is,
/// and `keep_scratch` tells the scratch guard not to remove it.
fn keep_repetition(
    rep_dir: &Path,
    kept_rep: &Path,
    keep_scratch: &std::sync::atomic::AtomicBool,
) -> Result<PathBuf, String> {
    let moved = kept_rep
        .parent()
        .map_or(Ok(()), std::fs::create_dir_all)
        .and_then(|()| std::fs::rename(rep_dir, kept_rep));
    match moved {
        Ok(()) => Ok(kept_rep.to_path_buf()),
        Err(e) => {
            keep_scratch.store(true, std::sync::atomic::Ordering::SeqCst);
            Err(format!(
                "cannot move {} to {}: {e}; it is kept in place, and the scratch directory is not removed",
                rep_dir.display(),
                kept_rep.display()
            ))
        }
    }
}

/// Copies every file a `files` check names that exists in the tree into
/// `<stored run>/eval-files/`, so a failed check can be read after the
/// scratch tree is gone.
fn keep_checked_files(case: &core_eval::EvalCase, tree: &Path, stored: &Path) {
    for f in &case.checks.files {
        let from = tree.join(&f.path);
        // Never through a link the agent planted.
        let plain = apb_core::fsutil::ensure_no_symlink_below(tree, &from).is_ok()
            && std::fs::symlink_metadata(&from).is_ok_and(|m| m.is_file());
        if plain {
            let to = stored.join("eval-files").join(&f.path);
            if let Some(p) = to.parent()
                && std::fs::create_dir_all(p).is_ok()
            {
                let _ = std::fs::copy(&from, &to);
            }
        }
    }
}

// --- interruption, the live run and the scratch lifecycle --------------------

/// The `stopped` text of a repetition the operator interrupted.
const INTERRUPTED_REASON: &str = "interrupted: apb eval received a signal";

static INTERRUPTED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

fn interrupted() -> bool {
    INTERRUPTED.load(std::sync::atomic::Ordering::SeqCst)
}

#[cfg(unix)]
extern "C" fn on_signal(sig: libc::c_int) {
    // A second signal ends the process the ordinary way.
    if INTERRUPTED.swap(true, std::sync::atomic::Ordering::SeqCst) {
        // SAFETY: signal() and raise() are async-signal-safe.
        unsafe {
            libc::signal(sig, libc::SIG_DFL);
            libc::raise(sig);
        }
    }
}

/// SIGINT and SIGTERM stop the live repetition's run, wait for its driver
/// and clean up instead of leaving a detached run with no limit behind (the
/// driver is in its own process group, so a terminal's Ctrl-C does not
/// reach it). A second signal ends the process at once.
fn install_signal_handlers() {
    #[cfg(unix)]
    // SAFETY: the handler only touches an atomic and async-signal-safe calls.
    unsafe {
        let h = on_signal as extern "C" fn(libc::c_int) as libc::sighandler_t;
        libc::signal(libc::SIGINT, h);
        libc::signal(libc::SIGTERM, h);
    }
}

/// The repetition whose run is in flight, for the scratch guard.
struct LiveRun {
    tree: PathBuf,
    run_id: String,
}

static LIVE: std::sync::Mutex<Option<LiveRun>> = std::sync::Mutex::new(None);

fn set_live(run: Option<LiveRun>) {
    if let Ok(mut l) = LIVE.lock() {
        *l = run;
    }
}

/// Kills what is left of a finished driver's process group (the driver led
/// it; an agent helper it spawned may linger). The driver is gone, so its
/// pid, and with it the group id, may already name someone else's group:
/// only members whose working directory lies inside the repetition are
/// killed, one by one. Linux only (it reads `/proc`); elsewhere a leftover
/// helper is left to the operator.
fn kill_leftover_group(driver_pid: Option<u32>, rep_dir: &Path) {
    #[cfg(target_os = "linux")]
    {
        let Some(pgid) = driver_pid.filter(|p| *p > 1) else {
            return;
        };
        let Ok(rep) = std::fs::canonicalize(rep_dir) else {
            return;
        };
        let Ok(procs) = std::fs::read_dir("/proc") else {
            return;
        };
        for e in procs.flatten() {
            let Some(pid) = e.file_name().to_str().and_then(|s| s.parse::<u32>().ok()) else {
                continue;
            };
            // `/proc/<pid>/stat`: `pid (comm) state ppid pgrp ...`; comm may
            // hold spaces, so the fields are counted after the last `)`.
            let Ok(stat) = std::fs::read_to_string(e.path().join("stat")) else {
                continue;
            };
            let pgrp = stat
                .rsplit_once(')')
                .and_then(|(_, rest)| rest.split_whitespace().nth(2))
                .and_then(|g| g.parse::<u32>().ok());
            let inside =
                std::fs::read_link(e.path().join("cwd")).is_ok_and(|c| c.starts_with(&rep));
            if pgrp == Some(pgid)
                && inside
                && let Ok(p) = libc::pid_t::try_from(pid)
            {
                // SAFETY: plain kill(2) on a process we just identified.
                unsafe { libc::kill(p, libc::SIGKILL) };
            }
        }
    }
    #[cfg(not(target_os = "linux"))]
    let _ = (driver_pid, rep_dir);
}

/// Whether the process `pid` exists.
fn pid_alive(pid: u32) -> bool {
    #[cfg(unix)]
    {
        let Ok(p) = libc::pid_t::try_from(pid) else {
            return false;
        };
        // SAFETY: kill(pid, 0) only probes.
        let r = unsafe { libc::kill(p, 0) };
        r == 0 || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
    }
    #[cfg(not(unix))]
    {
        let _ = pid;
        true
    }
}

/// The file in an invocation's scratch directory naming its owner.
const OWNER_FILE: &str = "owner.pid";

/// Owns an invocation's scratch directory: removes it on every exit path,
/// early returns and panics included. A run still in flight at that point
/// is stopped first; a repetition whose driver will not exit is moved to
/// `<evals>/kept/` instead of being deleted under it.
struct ScratchGuard {
    scratch: PathBuf,
    evals_home: PathBuf,
    /// Set when a repetition that had to be kept could not be moved out:
    /// the scratch directory then stays, since it still holds that tree.
    keep: std::sync::Arc<std::sync::atomic::AtomicBool>,
}

impl ScratchGuard {
    fn new(evals_home: &Path, scratch: &Path) -> std::io::Result<Self> {
        std::fs::create_dir_all(scratch)?;
        std::fs::write(scratch.join(OWNER_FILE), std::process::id().to_string())?;
        Ok(ScratchGuard {
            scratch: scratch.to_path_buf(),
            evals_home: evals_home.to_path_buf(),
            keep: std::sync::Arc::default(),
        })
    }
}

impl Drop for ScratchGuard {
    fn drop(&mut self) {
        let live = LIVE.lock().ok().and_then(|mut l| l.take());
        if let Some(run) = live {
            let _ = apb_engine::stop_run(&run.tree, &run.run_id);
            let run_dir = run.tree.join(".apb/runs").join(&run.run_id);
            if !driver_gone(&run_dir, &run.run_id)
                && let (Some(rep), Some(eval)) = (run.tree.parent(), self.scratch.file_name())
                && let Some(name) = rep.file_name()
            {
                let kept = self.evals_home.join("kept").join(eval).join(name);
                if let Err(e) = keep_repetition(rep, &kept, &self.keep) {
                    eprintln!("warning: {e}");
                }
            }
        }
        if self.keep.load(std::sync::atomic::Ordering::SeqCst) {
            return;
        }
        let _ = std::fs::remove_dir_all(&self.scratch);
        let _ = std::fs::remove_dir(self.evals_home.join("scratch"));
    }
}

/// Removes the scratch directories of earlier invocations that ended
/// without cleaning up (killed, crashed): an owner that is gone and no live
/// driver in any of its trees. Anything else is left alone.
fn sweep_stale_scratch(evals_home: &Path) {
    let Ok(entries) = std::fs::read_dir(evals_home.join("scratch")) else {
        return;
    };
    for e in entries.flatten() {
        let dir = e.path();
        let is_eval = e.file_name().to_string_lossy().starts_with("eval-");
        if !is_eval || !e.file_type().is_ok_and(|t| t.is_dir()) {
            continue;
        }
        let owner = std::fs::read_to_string(dir.join(OWNER_FILE))
            .ok()
            .and_then(|s| s.trim().parse::<u32>().ok());
        match owner {
            Some(pid) if pid_alive(pid) => continue,
            Some(_) => {}
            // No owner file: an invocation that is just creating its
            // scratch, or one that died before it wrote the file. Only an
            // old one is stale.
            None => {
                let recent = std::fs::metadata(&dir)
                    .and_then(|m| m.modified())
                    .ok()
                    .and_then(|t| t.elapsed().ok())
                    .is_none_or(|age| age < Duration::from_secs(600));
                if recent {
                    continue;
                }
            }
        }
        let live_driver = std::fs::read_dir(&dir)
            .into_iter()
            .flatten()
            .flatten()
            .any(|rep| {
                let runs = rep.path().join("tree/.apb/runs");
                std::fs::read_dir(runs)
                    .into_iter()
                    .flatten()
                    .flatten()
                    .any(|r| {
                        let id = r.file_name().to_string_lossy().into_owned();
                        apb_engine::liveness::driver_alive(&r.path(), &id) == Some(true)
                    })
            });
        if !live_driver {
            let _ = std::fs::remove_dir_all(&dir);
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
fn resolve_ref(hooks: &Path, root: &Path, r: &str) -> Result<String, String> {
    git(
        hooks,
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
    /// The empty directory every hardened git call takes its hooks from.
    hooks: &'a Path,
    root: &'a Path,
    args: &'a EvalArgs,
    playbook: &'a Playbook,
    version: &'a str,
    suite: &'a LoadedSuite,
    suite_copy: &'a Path,
    scratch: &'a Path,
    evals_home: &'a Path,
    eval_id: &'a str,
    overrides_file: Option<&'a Path>,
    /// The scratch guard's keep flag (see [`keep_repetition`]).
    keep_scratch: &'a std::sync::atomic::AtomicBool,
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

/// One repetition end to end. Returns the repetition, its stored run
/// directory when the run started, and a warning when its tree had to stay
/// in place.
fn run_repetition(
    cx: &RepContext,
    p: &Planned,
    n: u32,
) -> (Repetition, Option<PathBuf>, Option<String>) {
    let lc = p.lc;
    let rep_dir = cx.scratch.join(format!("{}-{n}", lc.case.id));
    let tree = rep_dir.join("tree");
    let fixture_commit = match p.fixture_commit.clone().transpose().and_then(|commit| {
        materialize(
            cx.hooks,
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
            return (error_rep(n, format!("fixture: {e}"), None), None, None);
        }
    };
    let env = overlay(&lc.env(&cx.suite.suite), &rep_dir);
    let limits = lc.case.limits.over(&cx.suite.suite.limits);
    let settings = apb_engine::run_config::EvalRunSettings {
        spawn_env: env.clone(),
        deadline_ms: Some(engine_deadline_ms(&limits)),
    };
    let started = match start_run(
        cx.args,
        lc,
        cx.version,
        &tree,
        &rep_dir,
        cx.overrides_file,
        &settings,
    ) {
        Ok(s) => s,
        Err(e) => {
            let _ = std::fs::remove_dir_all(&rep_dir);
            return (error_rep(n, e, None), None, None);
        }
    };
    let run_dir = tree.join(".apb/runs").join(&started.run_id);
    // Read now: the driver removes its pid file when it exits.
    let driver_pid = apb_engine::driver::read_driver_pid(&run_dir);
    set_live(Some(LiveRun {
        tree: tree.clone(),
        run_id: started.run_id.clone(),
    }));
    let stop = follow(&tree, &started.run_id, &limits);
    let clean = driver_gone(&run_dir, &started.run_id);
    set_live(None);
    let events = apb_engine::run_view::read_events(&run_dir).unwrap_or_default();
    let raw_types = checks::raw_event_types(&run_dir);
    // The scripts get the case env overlay, as the agents did, then the
    // runner's own variables (an overlay cannot name `APB_*`, V80).
    let mut script_env: Vec<(String, String)> = env.into_iter().collect();
    script_env.extend(
        [
            ("APB_EVAL_RUN_DIR", run_dir.to_string_lossy().into_owned()),
            ("APB_RUN_ID", started.run_id.clone()),
            ("APB_EVAL_CASE", lc.case.id.clone()),
            ("APB_EVAL_REPETITION", n.to_string()),
            ("APB_EVAL_SCRATCH", rep_dir.to_string_lossy().into_owned()),
        ]
        .map(|(k, v)| (k.to_string(), v)),
    );
    // The agent had the operator's filesystem: the hooks directory is
    // emptied again before any check runs git.
    let _ = apb_engine::eval::git::empty_hooks_dir(cx.hooks);
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
        hooks_dir: cx.hooks,
    };
    let (check_results, goal) = checks::evaluate(&input);
    let verdict = if stop.as_ref().is_some_and(|s| s.limit) {
        "incomplete".to_string()
    } else {
        checks::verdict(&check_results).to_string()
    };
    let kept_rep = cx
        .evals_home
        .join("kept")
        .join(cx.eval_id)
        .join(format!("{}-{n}", lc.case.id));
    // The driver still lives, or the run directory could not be moved out
    // safely: the tree is never deleted. The whole repetition moves out of
    // the scratch directory, run directory included, and the result names
    // where; when even that fails it stays in the scratch directory.
    let keep = |why: Option<String>| -> (Option<PathBuf>, Option<String>, Option<String>) {
        match keep_repetition(&rep_dir, &kept_rep, cx.keep_scratch) {
            Ok(kept_rep) => {
                let kept_tree = kept_rep.join("tree");
                let run = kept_tree.join(".apb/runs").join(&started.run_id);
                (
                    Some(run),
                    Some(kept_tree.to_string_lossy().into_owned()),
                    why,
                )
            }
            Err(e) => {
                let run = tree.join(".apb/runs").join(&started.run_id);
                let w = match why {
                    Some(why) => format!("{why}; {e}"),
                    None => e,
                };
                (
                    Some(run),
                    Some(tree.to_string_lossy().into_owned()),
                    Some(w),
                )
            }
        }
    };
    let (stored, kept, warning) = if clean {
        // The driver is gone; whatever it left in its process group goes
        // with it before the tree is removed.
        kill_leftover_group(driver_pid, &rep_dir);
        let stored = cx
            .evals_home
            .join("runs")
            .join(&cx.args.id)
            .join(&started.run_id);
        match move_run_dir(&tree, &run_dir, &stored) {
            Ok(()) => {
                keep_checked_files(&lc.case, &tree, &stored);
                let _ = std::fs::remove_dir_all(&rep_dir);
                (Some(stored), None, None)
            }
            Err(e) => keep(Some(format!(
                "case `{}` repetition {n}: the run directory was not stored ({e})",
                lc.case.id
            ))),
        }
    } else {
        keep(None)
    };
    let rep = Repetition {
        repetition: n,
        run_id: Some(started.run_id.clone()),
        run_dir: stored.as_ref().map(|s| s.to_string_lossy().into_owned()),
        verdict,
        outcome: checks::outcome(&events, stop.as_ref().map(|s| s.text.as_str())),
        stopped: stop.map(|s| s.text),
        checks: check_results,
        goal,
        usage: RepUsage::from_events(&events),
        duration_ms: checks::duration_ms(&events),
        kept_worktree: kept,
    };
    (rep, stored, warning)
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
fn compare_only(root: &Path, evals_home: &Path, args: &EvalArgs) -> ExitCode {
    if !apb_core::registry::is_safe_segment(&args.id) || args.id.starts_with('.') {
        return fail(
            args.json,
            "bad_id",
            format!("`{}` is not a playbook id", args.id),
        );
    }
    let all = store::load_all(evals_home, &args.id);
    let Some(latest) = store::latest_for(&all, &root.to_string_lossy()) else {
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
        return compare_only(root, &evals_home, &args);
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
    let issues = checks::validate_suite(&playbook_dir, &playbook);
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
    // The pid keeps two invocations started in the same millisecond apart.
    let eval_id = format!("eval-{}-{}", apb_core::clock::now_ms(), std::process::id());
    let scratch = evals_home.join("scratch").join(&eval_id);
    sweep_stale_scratch(&evals_home);
    let guard = match ScratchGuard::new(&evals_home, &scratch) {
        Ok(g) => g,
        Err(e) => {
            return fail(
                args.json,
                "scratch",
                format!("cannot create the scratch: {e}"),
            );
        }
    };
    install_signal_handlers();
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
        &guard.keep,
    );
    drop(guard);
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

fn full_env_json(nodes: &[(String, String)]) -> serde_json::Value {
    nodes
        .iter()
        .map(|(node, profile)| json!({ "node": node, "profile": profile }))
        .collect()
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
    keep_scratch: &std::sync::atomic::AtomicBool,
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
    let hooks = scratch.join("no-hooks");
    if let Err(e) = apb_engine::eval::git::empty_hooks_dir(&hooks) {
        return fail(
            args.json,
            "scratch",
            format!("cannot prepare the scratch: {e}"),
        );
    }
    let mut planned: Vec<Planned> = Vec::new();
    for lc in &cases {
        // A `git:` fixture ref is resolved once per case: every repetition
        // materializes the same commit, and the case digest names it.
        let fixture_commit = lc
            .case
            .fixture
            .git
            .as_ref()
            .map(|r| resolve_ref(&hooks, root, r));
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
    let full_env = full_environment_nodes(root, playbook, overrides);
    if !args.json {
        eprintln!("{NOT_A_SANDBOX}");
        for (node, profile) in &full_env {
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
                "note": NOT_A_SANDBOX,
                "full_environment_nodes": full_env_json(&full_env),
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
        "the suite's scripts and its env overlay are not approved on this machine; approve them and start?".to_string()
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
        hooks: &hooks,
        root,
        args,
        playbook,
        version,
        suite: &suite,
        suite_copy,
        scratch,
        evals_home,
        eval_id,
        overrides_file: overrides_file.as_deref(),
        keep_scratch,
    };
    let started_at_ms = apb_core::clock::now_ms();
    let mut spent = 0.0_f64;
    let mut tokens = 0_u64;
    let mut incomplete: Option<String> = None;
    let mut key: Option<ConfigKey> = None;
    let mut results = Vec::new();
    let mut warnings: Vec<String> = Vec::new();
    let mut cost_warned = false;
    for p in &planned {
        let lc = p.lc;
        let mut reps = Vec::new();
        for n in 1..=p.repeat {
            if interrupted() {
                incomplete = Some(INTERRUPTED_REASON.into());
            }
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
            let (rep, stored, kept_warning) = run_repetition(&cx, p, n);
            if let Some(w) = kept_warning {
                if !args.json {
                    eprintln!("warning: {w}");
                }
                warnings.push(w);
            }
            if rep.run_id.is_some() && rep.usage.cost_usd.is_none() && !cost_warned {
                cost_warned = true;
                let w = format!(
                    "case `{}` repetition {n} reported no cost: the invocation budget (${budget:.2}) and max_usd cannot be enforced for this executor",
                    lc.case.id
                );
                if !args.json {
                    eprintln!("warning: {w}");
                }
                warnings.push(w);
            }
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
    // With no repetition started there is no configuration to key the
    // result by: it is reported, not stored, so no made-up key ever shows
    // up as a configuration change in a later comparison.
    let started = key.is_some();
    let config = key.unwrap_or_default();
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
        journal_agent_writable: true,
    };
    let stored_at = if started {
        Some(store::store(evals_home, &result))
    } else {
        None
    };
    let all = if started {
        store::load_all(evals_home, &args.id)
    } else {
        Vec::new()
    };
    let (p, o) = result.passes();
    if args.json {
        let cmp = print_compare(&result, &all, true);
        print_json(&json!({
            "result": result,
            "note": NOT_A_SANDBOX,
            "full_environment_nodes": full_env_json(&full_env),
            "warnings": warnings,
            "comparison": cmp,
            "stored": stored_at.as_ref().and_then(|s| s.as_ref().ok()).map(|p| p.to_string_lossy().into_owned()),
        }));
    } else {
        print!("{}", store::render_result(&result));
        match &stored_at {
            Some(Ok(p)) => println!("stored: {}", p.display()),
            Some(Err(e)) => eprintln!("eval: the result could not be stored: {e}"),
            None => println!("not stored: no repetition started a run"),
        }
        if started
            && print_compare(&result, &all, false).is_none()
            && store::baseline_for(&all, &result).is_none()
        {
            println!("no earlier result to compare with");
        }
    }
    if interrupted() {
        ExitCode::from(130)
    } else if p == o && o > 0 && result.incomplete.is_none() {
        ExitCode::SUCCESS
    } else {
        ExitCode::from(1)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::Ordering;

    /// An agent that replaced `tree/.apb` with a link to a directory outside
    /// the tree does not get that directory moved into the eval store, nor
    /// deleted by the copy fallback.
    #[cfg(unix)]
    #[test]
    fn a_run_directory_behind_a_symlinked_apb_is_refused_and_the_outside_stays() {
        let t = tempfile::tempdir().unwrap();
        let outside = t.path().join("outside");
        let outside_run = outside.join("runs/r1");
        std::fs::create_dir_all(&outside_run).unwrap();
        std::fs::write(outside_run.join("keep.txt"), "mine").unwrap();
        let tree = t.path().join("tree");
        std::fs::create_dir_all(&tree).unwrap();
        std::os::unix::fs::symlink(&outside, tree.join(".apb")).unwrap();
        let from = tree.join(".apb/runs/r1");
        let to = t.path().join("store/r1");

        let err = move_run_dir(&tree, &from, &to).expect_err("a symlinked .apb is refused");

        assert!(err.to_string().contains("symlink"), "{err}");
        assert_eq!(
            std::fs::read_to_string(outside_run.join("keep.txt")).unwrap(),
            "mine"
        );
        assert!(!to.exists(), "nothing was moved into the store");
    }

    #[test]
    fn a_plain_run_directory_is_moved() {
        let t = tempfile::tempdir().unwrap();
        let tree = t.path().join("tree");
        let from = tree.join(".apb/runs/r1");
        std::fs::create_dir_all(&from).unwrap();
        std::fs::write(from.join("events.jsonl"), "{}").unwrap();
        let to = t.path().join("store/r1");

        move_run_dir(&tree, &from, &to).unwrap();

        assert!(to.join("events.jsonl").is_file());
        assert!(!from.exists());
    }

    /// A repetition that has to be kept but cannot be moved stays in the
    /// scratch directory, and the scratch guard then leaves the scratch
    /// directory alone instead of deleting the tree under a live driver.
    #[test]
    fn a_kept_repetition_that_cannot_move_stays_and_the_scratch_survives() {
        let t = tempfile::tempdir().unwrap();
        let evals_home = t.path().join("evals");
        let scratch = evals_home.join("scratch/eval-1");
        let guard = ScratchGuard::new(&evals_home, &scratch).unwrap();
        let rep_dir = scratch.join("case-1");
        std::fs::create_dir_all(rep_dir.join("tree")).unwrap();
        // The kept directory's parent is a file, so the move cannot happen.
        std::fs::write(evals_home.join("kept"), "not a directory").unwrap();
        let kept_rep = evals_home.join("kept/eval-1/case-1");

        let err = keep_repetition(&rep_dir, &kept_rep, &guard.keep).expect_err("the move fails");
        assert!(err.contains("kept in place"), "{err}");
        assert!(guard.keep.load(Ordering::SeqCst));
        drop(guard);

        assert!(rep_dir.join("tree").is_dir(), "the kept tree was deleted");
    }

    #[test]
    fn a_kept_repetition_moves_and_the_scratch_is_removed() {
        let t = tempfile::tempdir().unwrap();
        let evals_home = t.path().join("evals");
        let scratch = evals_home.join("scratch/eval-1");
        let guard = ScratchGuard::new(&evals_home, &scratch).unwrap();
        let rep_dir = scratch.join("case-1");
        std::fs::create_dir_all(rep_dir.join("tree")).unwrap();
        let kept_rep = evals_home.join("kept/eval-1/case-1");

        let kept = keep_repetition(&rep_dir, &kept_rep, &guard.keep).unwrap();
        assert_eq!(kept, kept_rep);
        assert!(!guard.keep.load(Ordering::SeqCst));
        drop(guard);

        assert!(kept_rep.join("tree").is_dir());
        assert!(!scratch.exists(), "the scratch directory stayed");
    }
}
