//! `apb run --scope global` (the CLI half of the global-playbooks spec):
//! every check of a start reads the global playbook, never a project
//! playbook of the same id, and the run lands in the project. Each test
//! has a project and its own config dir holding the global playbook.

use std::fs;
use std::path::{Path, PathBuf};

use predicates::prelude::*;

/// A playbook without an agent: start, one prompt naming `{{params.k}}`,
/// finish. Schema 2, the layout a saved definition has.
fn dup(name: &str, prompt: &str, with_budget: bool) -> String {
    let budget = if with_budget {
        "\nbudget:\n  tokens: 100000"
    } else {
        ""
    };
    format!(
        r#"
schema: 2
id: {name}
name: Dup
version: 1.0.0{budget}
params:
  - {{ name: k, type: text }}
nodes:
  - {{ id: start, type: start }}
  - {{ id: note, type: prompt, prompt: "{prompt} {{{{params.k}}}}" }}
  - {{ id: done, type: finish, outcome: success }}
edges:
  - {{ from: start, to: note }}
  - {{ from: note, to: done }}
"#
    )
}

fn apb(cfg: &Path) -> assert_cmd::Command {
    let mut cmd = crate::common::apb();
    cmd.env("APB_CONFIG_DIR", cfg);
    cmd
}

/// A project (`apb init`) with the global playbook `id` in `cfg` (its own
/// config dir), approved for it. The playbook has no agent node, so no
/// agent CLI runs.
fn fixture(id: &str, prompt: &str, with_budget: bool) -> (tempfile::TempDir, tempfile::TempDir) {
    let cfg = tempfile::tempdir().unwrap();
    let dir = tempfile::tempdir().unwrap();
    apb(cfg.path())
        .arg("init")
        .current_dir(dir.path())
        .assert()
        .success();
    seed_global(cfg.path(), dir.path(), id, &dup(id, prompt, with_budget));
    (dir, cfg)
}

/// Writes the global playbook `id` (version 1.0.0, `current`) into `cfg`
/// and approves it there, from the project `cwd`.
fn seed_global(cfg: &Path, cwd: &Path, id: &str, yaml: &str) {
    let vdir = cfg.join("playbooks").join(id).join("1.0.0");
    fs::create_dir_all(&vdir).unwrap();
    fs::write(vdir.join("playbook.yaml"), yaml).unwrap();
    fs::write(cfg.join("playbooks").join(id).join("current"), "1.0.0").unwrap();
    apb(cfg)
        .args(["trust", "approve", id, "--scope", "global"])
        .current_dir(cwd)
        .assert()
        .success();
}

/// How many runs the project holds.
fn run_count(root: &Path) -> usize {
    fs::read_dir(root.join(".apb/runs"))
        .map(|rd| rd.filter_map(Result::ok).count())
        .unwrap_or(0)
}

/// The one run directory under `root/.apb/runs`.
fn only_run(root: &Path) -> PathBuf {
    let mut dirs: Vec<PathBuf> = fs::read_dir(root.join(".apb/runs"))
        .unwrap()
        .filter_map(Result::ok)
        .map(|e| e.path())
        .collect();
    assert_eq!(dirs.len(), 1, "exactly one run directory");
    dirs.pop().unwrap()
}

/// The value of a string field of the event `kind` in the run's journal.
fn event_field(run_dir: &Path, kind: &str, field: &str) -> String {
    let journal = fs::read_to_string(run_dir.join("events.jsonl")).unwrap();
    journal
        .lines()
        .filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
        .find(|v| v["type"] == kind)
        .unwrap_or_else(|| panic!("no {kind} in {:?}", run_dir.join("events.jsonl")))[field]
        .as_str()
        .unwrap_or_default()
        .to_string()
}

/// Seeds the project playbook `id` (version 1.0.0, `current`) from `yaml`.
fn seed_project(root: &Path, id: &str, yaml: &str) -> PathBuf {
    let vdir = root.join(".apb/playbooks").join(id).join("1.0.0");
    fs::create_dir_all(&vdir).unwrap();
    fs::write(vdir.join("playbook.yaml"), yaml).unwrap();
    fs::write(
        root.join(".apb/playbooks").join(id).join("current"),
        "1.0.0",
    )
    .unwrap();
    vdir
}

/// Whether `stdout` holds a table row whose cells are exactly `cells`.
fn has_row(stdout: &str, cells: &[&str]) -> bool {
    stdout
        .lines()
        .any(|l| l.split_whitespace().collect::<Vec<_>>() == cells)
}

/// The run id of a `run started: <id>` line.
fn started_id(stdout: &str) -> String {
    stdout
        .lines()
        .find_map(|l| l.strip_prefix("run started: "))
        .expect("run started: <id>")
        .trim()
        .to_string()
}

#[test]
fn apb_run_scope_global_runs_the_global_playbook_here() {
    let (dir, cfg) = fixture("gdup", "global prompt", false);
    apb(cfg.path())
        .args(["run", "gdup", "--scope", "global", "--param", "k=v"])
        .current_dir(dir.path())
        .assert()
        .success();
    let run_dir = only_run(dir.path());
    assert!(run_dir.starts_with(dir.path().join(".apb/runs")));
    assert_eq!(event_field(&run_dir, "run_provenance", "origin"), "global");
}

#[test]
fn apb_run_scope_global_detach_starts_and_finishes() {
    let (dir, cfg) = fixture("gdup", "global prompt", false);
    let out = apb(cfg.path())
        .args([
            "run", "gdup", "--scope", "global", "--detach", "--param", "k=v",
        ])
        .current_dir(dir.path())
        .assert()
        .success();
    let stdout = String::from_utf8_lossy(&out.get_output().stdout).into_owned();
    let run_id = stdout
        .lines()
        .find_map(|l| l.strip_prefix("run started: "))
        .expect("run started: <id>")
        .trim()
        .to_string();
    apb(cfg.path())
        .args(["wait", &run_id])
        .current_dir(dir.path())
        .assert()
        .success();
}

#[test]
fn a_project_playbook_of_the_same_id_is_not_taken() {
    let (dir, cfg) = fixture("dup", "global prompt", false);
    // The project holds `dup` too, its prompt text different.
    let vdir = dir.path().join(".apb/playbooks/dup/1.0.0");
    fs::create_dir_all(&vdir).unwrap();
    fs::write(
        vdir.join("playbook.yaml"),
        dup("dup", "project prompt", false),
    )
    .unwrap();
    fs::write(dir.path().join(".apb/playbooks/dup/current"), "1.0.0").unwrap();

    apb(cfg.path())
        .args(["run", "dup", "--scope", "global", "--param", "k=v"])
        .current_dir(dir.path())
        .assert()
        .success();
    let global_run = only_run(dir.path());
    let global_yaml = fs::read(cfg.path().join("playbooks/dup/1.0.0/playbook.yaml")).unwrap();
    let digest = event_field(&global_run, "run_started", "definition");
    assert!(
        apb_core::content::sha256_hex(&global_yaml).ends_with(&digest),
        "the global definition is what ran ({digest})"
    );

    fs::remove_dir_all(dir.path().join(".apb/runs")).unwrap();
    apb(cfg.path())
        .args(["run", "dup", "--param", "k=v"])
        .current_dir(dir.path())
        .assert()
        .success();
    let project_run = only_run(dir.path());
    let project_yaml = fs::read(vdir.join("playbook.yaml")).unwrap();
    let digest = event_field(&project_run, "run_started", "definition");
    assert!(
        apb_core::content::sha256_hex(&project_yaml).ends_with(&digest),
        "the project playbook is what a plain start runs ({digest})"
    );
}

#[test]
fn an_unknown_scope_exits_2() {
    let (dir, cfg) = fixture("gdup", "global prompt", false);
    apb(cfg.path())
        .args(["run", "gdup", "--scope", "team", "--param", "k=v"])
        .current_dir(dir.path())
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "--scope must be project or global",
        ));
    assert_eq!(run_count(dir.path()), 0, "nothing is started");
}

#[test]
fn supervise_with_scope_global_is_refused() {
    let (dir, cfg) = fixture("gdup", "global prompt", false);
    apb(cfg.path())
        .args([
            "run",
            "gdup",
            "--scope",
            "global",
            "--supervise",
            "--param",
            "k=v",
        ])
        .current_dir(dir.path())
        .assert()
        .failure()
        .stderr(predicate::str::contains("supervise_global_unsupported"));
    assert_eq!(run_count(dir.path()), 0, "no run directory");
}

#[test]
fn remember_with_scope_global_keys_the_global_playbook() {
    let (dir, cfg) = fixture("dup", "global prompt", false);
    seed_project(dir.path(), "dup", &dup("dup", "project prompt", false));
    apb(cfg.path())
        .args([
            "run",
            "dup",
            "--scope",
            "global",
            "--remember",
            "--param",
            "k=g-value",
        ])
        .current_dir(dir.path())
        .assert()
        .success();
    let global = apb(cfg.path())
        .args(["memory", "show", "dup", "--scope", "global"])
        .current_dir(dir.path())
        .assert()
        .success();
    let global = String::from_utf8_lossy(&global.get_output().stdout).into_owned();
    assert!(
        has_row(&global, &["k", "compatible", "g-value"]),
        "{global}"
    );
    // The project `dup` exists, so its memory answers, without the value.
    let project = apb(cfg.path())
        .args(["memory", "show", "dup"])
        .current_dir(dir.path())
        .assert()
        .success();
    let project = String::from_utf8_lossy(&project.get_output().stdout).into_owned();
    assert!(project.contains("no remembered param values"), "{project}");
    assert!(!project.contains("g-value"), "{project}");
}

#[test]
fn a_start_key_with_scope_global_answers_the_same_run() {
    let (dir, cfg) = fixture("gdup", "global prompt", false);
    let args = [
        "run",
        "gdup",
        "--scope",
        "global",
        "--detach",
        "--start-key",
        "e1",
    ];
    let first = apb(cfg.path())
        .args(args)
        .args(["--param", "k=v"])
        .current_dir(dir.path())
        .assert()
        .success();
    let run_id = started_id(&String::from_utf8_lossy(&first.get_output().stdout));
    apb(cfg.path())
        .args(args)
        .args(["--param", "k=v"])
        .current_dir(dir.path())
        .assert()
        .success()
        .stdout(predicate::str::contains(format!(
            "run already started: {run_id}"
        )));
    let runs = fs::read_dir(dir.path().join(".apb/runs")).unwrap().count();
    assert_eq!(runs, 1, "the keyed repeat starts no second run");
}

/// A start key is held per scope: with `dup` in both scopes, a key a
/// project start took leaves a global start with the same key free to
/// start its own run, and the reverse.
#[test]
fn a_start_key_is_held_per_scope() {
    let (dir, cfg) = fixture("dup", "global prompt", false);
    seed_project(dir.path(), "dup", &dup("dup", "project prompt", false));
    let start = |scope: Option<&str>, key: &str| -> String {
        let mut cmd = apb(cfg.path());
        cmd.args([
            "run",
            "dup",
            "--detach",
            "--start-key",
            key,
            "--param",
            "k=v",
        ]);
        if let Some(scope) = scope {
            cmd.args(["--scope", scope]);
        }
        let out = cmd.current_dir(dir.path()).assert().success();
        String::from_utf8_lossy(&out.get_output().stdout).into_owned()
    };
    let p1 = started_id(&start(None, "e1"));
    let g1 = start(Some("global"), "e1");
    assert!(!g1.contains("already started"), "{g1}");
    assert_ne!(started_id(&g1), p1);
    let g2 = started_id(&start(Some("global"), "e2"));
    let p2 = start(None, "e2");
    assert!(!p2.contains("already started"), "{p2}");
    assert_ne!(started_id(&p2), g2);
    assert_eq!(run_count(dir.path()), 4, "each scope started its own runs");
}

/// A keyed start and its repeat answer the same for both scopes, on the
/// detached and the synchronous start: the started or finished line, the
/// recalled input, the repeat's `run already started` line and every exit
/// code come from one tail per start form. The repeat reaches the engine's
/// `AlreadyStarted` arm of that tail: its lookup answers the key free and
/// stops at the test barrier, another start claims the key, and only then
/// does the repeat go on to its own start.
#[test]
fn an_already_started_answer_is_the_same_for_both_scopes() {
    let (dir, cfg) = fixture("dup", "global prompt", false);
    seed_project(dir.path(), "dup", &dup("dup", "project prompt", false));
    let cmd = |scope: Option<&str>, key: &str, detach: bool| {
        let mut cmd = crate::common::apb_std();
        cmd.env("APB_CONFIG_DIR", cfg.path());
        cmd.args(["run", "dup", "--start-key", key, "--param", "k=v"]);
        if detach {
            cmd.arg("--detach");
        }
        if let Some(scope) = scope {
            cmd.args(["--scope", scope]);
        }
        cmd.current_dir(dir.path());
        cmd
    };
    // (stdout, stderr, exit code) of a finished start, every run id
    // replaced.
    let masked = |out: std::process::Output| {
        let mut stdout = String::from_utf8_lossy(&out.stdout).into_owned();
        let mut stderr = String::from_utf8_lossy(&out.stderr).into_owned();
        for entry in fs::read_dir(dir.path().join(".apb/runs")).unwrap() {
            let id = entry.unwrap().file_name().to_string_lossy().into_owned();
            stdout = stdout.replace(&id, "<run>");
            stderr = stderr.replace(&id, "<run>");
        }
        (stdout, stderr, out.status.code())
    };
    // A repeat whose lookup passes: it stops at the barrier after a free
    // lookup, a first start takes the key, then the repeat goes on.
    let raced = |scope: Option<&str>, key: &str, detach: bool| {
        let barrier = tempfile::tempdir().unwrap();
        let mut repeat = cmd(scope, key, detach);
        repeat
            .env(
                apb_engine::start_key::testing::AFTER_FREE_LOOKUP,
                barrier.path(),
            )
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());
        let mut child = repeat.spawn().unwrap();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
        while !barrier.path().join("reached").exists() {
            if let Some(status) = child.try_wait().unwrap() {
                panic!("the repeat ended ({status}) without stopping at the lookup barrier");
            }
            assert!(
                std::time::Instant::now() < deadline,
                "the lookup barrier was never reached"
            );
            std::thread::yield_now();
        }
        let first = masked(cmd(scope, key, detach).output().unwrap());
        assert_eq!(first.2, Some(0), "{first:?}");
        fs::write(barrier.path().join("go"), b"").unwrap();
        masked(child.wait_with_output().unwrap())
    };
    for detach in [true, false] {
        let tag = if detach { "d" } else { "s" };
        let (pk, gk) = (format!("p{tag}"), format!("g{tag}"));
        let project_first = masked(cmd(None, &pk, detach).output().unwrap());
        let global_first = masked(cmd(Some("global"), &gk, detach).output().unwrap());
        assert_eq!(project_first.2, Some(0), "{project_first:?}");
        assert_eq!(project_first, global_first, "first start, detach={detach}");
        let project = raced(None, &format!("r{pk}"), detach);
        let global = raced(Some("global"), &format!("r{gk}"), detach);
        assert!(
            project
                .0
                .starts_with("run already started: <run> (start key)\n"),
            "{project:?}"
        );
        assert_eq!(project.2, Some(0), "{project:?}");
        assert_eq!(project, global, "repeat, detach={detach}");
    }
}

#[test]
fn estimate_with_scope_global_reads_the_global_playbook() {
    let (dir, cfg) = fixture("gdup", "global prompt", false);
    apb(cfg.path())
        .args(["run", "gdup", "--scope", "global", "--estimate"])
        .current_dir(dir.path())
        .assert()
        .success();
    assert_eq!(run_count(dir.path()), 0, "an estimate starts nothing");
}

#[test]
fn eval_settings_with_scope_global_is_refused() {
    let (dir, cfg) = fixture("gdup", "global prompt", false);
    let settings = dir.path().join("eval-settings.json");
    fs::write(&settings, "{}").unwrap();
    apb(cfg.path())
        .args([
            "run",
            "gdup",
            "--scope",
            "global",
            "--eval-settings",
            settings.to_str().unwrap(),
        ])
        .current_dir(dir.path())
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "--eval-settings is only for a project playbook",
        ));
    assert_eq!(run_count(dir.path()), 0, "no run directory");
}

#[test]
fn a_global_host_start_runs_in_the_foreground() {
    let (dir, cfg) = fixture("gdup", "global prompt", false);
    apb(cfg.path())
        .args([
            "run",
            "gdup",
            "--scope",
            "global",
            "--execution",
            "host",
            "--param",
            "k=v",
        ])
        .current_dir(dir.path())
        .assert()
        .success();
    let run_dir = only_run(dir.path());
    assert_eq!(event_field(&run_dir, "run_provenance", "origin"), "global");
}

#[test]
fn recall_with_scope_global_reads_the_global_memory_only() {
    let (dir, cfg) = fixture("dup", "global prompt", false);
    let vdir = dir.path().join(".apb/playbooks/dup/1.0.0");
    fs::create_dir_all(&vdir).unwrap();
    fs::write(
        vdir.join("playbook.yaml"),
        dup("dup", "project prompt", false),
    )
    .unwrap();
    fs::write(dir.path().join(".apb/playbooks/dup/current"), "1.0.0").unwrap();

    // A value remembered for the project playbook is not recalled by a
    // global start: the param then needs a value.
    apb(cfg.path())
        .args(["run", "dup", "--remember", "--param", "k=project-value"])
        .current_dir(dir.path())
        .assert()
        .success();
    apb(cfg.path())
        .args(["run", "dup", "--scope", "global"])
        .current_dir(dir.path())
        .assert()
        .failure()
        .stderr(predicate::str::contains("param_needs_value"));

    // A value remembered for the global playbook is recalled by the next
    // global start and not by a project start.
    apb(cfg.path())
        .args([
            "run",
            "dup",
            "--scope",
            "global",
            "--remember",
            "--param",
            "k=global-value",
        ])
        .current_dir(dir.path())
        .assert()
        .success()
        .stderr(predicate::str::contains("remembered for this project: k"));
    apb(cfg.path())
        .args(["run", "dup", "--scope", "global"])
        .current_dir(dir.path())
        .assert()
        .success()
        .stderr(predicate::str::contains("used remembered input: k"));
    // With the global memory cleared, a global start falls back to nothing:
    // it never reads the project's memory.
    apb(cfg.path())
        .args(["memory", "clear", "dup", "--scope", "global"])
        .current_dir(dir.path())
        .assert()
        .success();
    apb(cfg.path())
        .args(["run", "dup", "--scope", "global"])
        .current_dir(dir.path())
        .assert()
        .failure()
        .stderr(predicate::str::contains("param_needs_value"));
    // The project's memory still answers a project start.
    apb(cfg.path())
        .args(["run", "dup"])
        .current_dir(dir.path())
        .assert()
        .success()
        .stderr(predicate::str::contains("used remembered input: k"));
}

#[test]
fn a_budget_check_reads_the_global_playbook() {
    let (dir, cfg) = fixture("gdup", "global prompt", false);
    apb(cfg.path())
        .args([
            "run",
            "gdup",
            "--scope",
            "global",
            "--budget-tokens",
            "100000",
            "--param",
            "k=v",
        ])
        .current_dir(dir.path())
        .assert()
        .success()
        .stderr(predicate::str::contains("budget check skipped").not());
    assert!(dir.path().join(".apb/runs").is_dir());
}

#[test]
fn the_notify_policy_of_a_global_start_is_the_global_key() {
    let (dir, cfg) = fixture("dup", "global prompt", false);
    let vdir = dir.path().join(".apb/playbooks/dup/1.0.0");
    fs::create_dir_all(&vdir).unwrap();
    fs::write(
        vdir.join("playbook.yaml"),
        dup("dup", "project prompt", false),
    )
    .unwrap();
    fs::write(dir.path().join(".apb/playbooks/dup/current"), "1.0.0").unwrap();
    // The machine's policy subscribes only the global `dup`, with a target
    // that cannot be used: the default subscription of a global start then
    // warns, and a project start (not covered) stays silent.
    fs::write(
        cfg.path().join("policy.yaml"),
        "notify:\n  playbooks:\n  - global:dup\n",
    )
    .unwrap();
    let machine_cfg = fs::read_to_string(cfg.path().join("config.yaml")).unwrap_or_default();
    fs::write(
        cfg.path().join("config.yaml"),
        format!(
            "{machine_cfg}notify:\n  default: on\n  target:\n    connector: nosuch\n    account: nosuch\n    function: send_somewhere\n    to: \"#chan\"\n"
        ),
    )
    .unwrap();

    apb(cfg.path())
        .args(["run", "dup", "--scope", "global", "--param", "k=v"])
        .current_dir(dir.path())
        .assert()
        .success()
        .stderr(predicate::str::contains(
            "notifications are off for this run",
        ));
    fs::remove_dir_all(dir.path().join(".apb/runs")).unwrap();
    apb(cfg.path())
        .args(["run", "dup", "--param", "k=v"])
        .current_dir(dir.path())
        .assert()
        .success()
        .stderr(predicate::str::contains("notifications are off for this run").not());
}

#[test]
fn an_unattended_global_start_reads_the_global_readiness() {
    // The project `dup` is ready to run alone: an enforced script goal, a
    // budget, nothing acting, and probation waived by this machine. The
    // global `dup` has no goal, so it is not.
    let (dir, cfg) = fixture("dup", "global prompt", false);
    let ready = dup("dup", "project prompt", true).replace(
        "params:",
        "goal:\n  statement: the note is written\n  enforce: true\n  criteria:\n    - description: the check passes\n      check: { type: script, path: scripts/check.sh }\nparams:",
    );
    let vdir = seed_project(dir.path(), "dup", &ready);
    fs::create_dir_all(vdir.join("scripts")).unwrap();
    fs::write(vdir.join("scripts/check.sh"), "exit 0\n").unwrap();
    // The waiver is the machine file's; this config dir is the default one
    // for these commands, so it is no redirection that hides settings.
    fs::write(
        cfg.path().join("policy.yaml"),
        "unattended:\n  waive_probation: true\n",
    )
    .unwrap();
    let apb = |cfg: &Path| {
        let mut cmd = apb(cfg);
        cmd.env("APB_TEST_DEFAULT_CONFIG_DIR", cfg);
        cmd
    };

    apb(cfg.path())
        .args(["run", "dup", "--unattended", "--param", "k=v"])
        .current_dir(dir.path())
        .assert()
        .success()
        .stderr(predicate::str::contains("not ready").not());
    fs::remove_dir_all(dir.path().join(".apb/runs")).unwrap();

    apb(cfg.path())
        .args([
            "run",
            "dup",
            "--scope",
            "global",
            "--unattended",
            "--require-ready",
            "--param",
            "k=v",
        ])
        .current_dir(dir.path())
        .assert()
        .failure()
        .stderr(predicate::str::contains("not ready"));
    apb(cfg.path())
        .args([
            "run",
            "dup",
            "--scope",
            "global",
            "--unattended",
            "--param",
            "k=v",
        ])
        .current_dir(dir.path())
        .assert()
        .success()
        .stderr(predicate::str::contains("warning: not ready to run alone"));
    let run_dir = only_run(dir.path());
    assert_eq!(event_field(&run_dir, "run_provenance", "origin"), "global");
}
