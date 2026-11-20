//! `apb eval` with connector doubles (eval-doubles D1): a case whose
//! `connectors:` answers every call of its run, through the real binary
//! and a stub agent that runs `apb connector call`. The `mock-tracker`
//! fixture account points at the discard port: nothing may reach it.

use std::fs;
use std::path::{Path, PathBuf};

use serde_json::Value;

use crate::common::{apb, apb_bin};

const CONNECTOR: &str =
    include_str!("../../../apb-engine/tests/fixtures/connectors/mock-tracker/connector.yaml");

const PLAYBOOK: &str = "schema: 2\nid: rev\nname: rev\nversion: 1.0.0\ndefaults: { profile: x }\neffects: [network, external]\nnodes:\n  - { id: start, type: start }\n  - { id: look, type: agent_task, prompt: look, connectors: [{ name: mock-tracker, accounts: [main], functions: [list_items, create_item] }] }\n  - { id: done, type: finish, outcome: success }\nedges:\n  - { from: start, to: look }\n  - { from: look, to: done }\n";

/// Both bound functions doubled; `list_items` answers `[1]` once, then `[2]`.
const CASE: &str = "schema: 1\nid: doubled\ninstruction: look\nfixture: { dir: fixtures/base }\nconnectors:\n  mock-tracker:\n    list_items:\n      - answer: { status: 200, body: { items: [1] } }\n        times: 1\n      - answer: { status: 200, body: { items: [2] } }\n    create_item:\n      - answer: { status: 201, body: { id: 7 } }\nchecks:\n  run: { outcome: [succeeded] }\n  connector_calls:\n    - { function: mock-tracker.list_items, args: { q: bug }, min: 1 }\nrepeat: 1\n";

/// The stub agent's report of success.
const REPORT: &str = "printf 'done\\n```yaml\\nstatus: success\\nsummary: done\\n```\\n'";

struct Env {
    project: tempfile::TempDir,
    cfg: tempfile::TempDir,
    stub: PathBuf,
    /// Where the stub agent writes what it saw.
    probe: PathBuf,
}

fn write(path: &Path, body: &str) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, body).unwrap();
    #[cfg(unix)]
    if path.extension().is_some_and(|e| e == "sh") {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
    }
}

/// A project with `rev`, the `mock-tracker` connector installed and
/// approved with account `main` (its token a placeholder variable), the
/// doubled case, and a stub agent running `body` before it reports success.
fn setup(case: &str, body: &str) -> Env {
    let project = tempfile::tempdir().unwrap();
    let cfg = tempfile::tempdir().unwrap();
    let root = project.path();
    apb_core::registry::init_project(root).unwrap();
    write(
        &cfg.path().join("connectors/mock-tracker/connector.yaml"),
        CONNECTOR,
    );
    write(
        &cfg.path().join("connector-config/mock-tracker.yaml"),
        "accounts:\n  - name: main\n    default: true\n    base_url: http://127.0.0.1:9\n    token: \"{{env.TRACKER_TOKEN}}\"\n",
    );
    // The global dotenv: the gate finds the variable, and every redactor of
    // the run (the call process's included) can scrub its value.
    write(
        &cfg.path().join("secrets.env"),
        "TRACKER_TOKEN=placeholder-token-0123456789\n",
    );
    let pb = root.join(".apb/playbooks/rev");
    write(&pb.join("1.0.0/playbook.yaml"), PLAYBOOK);
    write(&pb.join("current"), "1.0.0");
    write(
        &root.join(".apb/profiles/x/profile.yaml"),
        "name: x\nexecutor:\n  agent: claude\n  model: claude-haiku-4-5-20251001\n",
    );
    write(&pb.join("evals/doubled.yaml"), case);
    write(&pb.join("evals/fixtures/base/a.txt"), "a\n");
    let probe = cfg.path().join("probe");
    fs::create_dir_all(&probe).unwrap();
    let stub = cfg.path().join("stub.sh");
    write(
        &stub,
        &format!("#!/bin/sh\nP='{}'\n{body}\n{REPORT}\n", probe.display()),
    );
    let env = Env {
        project,
        cfg,
        stub,
        probe,
    };
    for args in [
        &["connector", "approve", "mock-tracker"][..],
        &["connector", "approve", "mock-tracker", "--account", "main"][..],
    ] {
        env.apb().args(args).assert().success();
    }
    env
}

impl Env {
    fn apb(&self) -> assert_cmd::Command {
        let mut c = apb();
        c.current_dir(self.project.path())
            .env("APB_CONFIG_DIR", self.cfg.path())
            .env("APB_AGENT_CMD", &self.stub)
            .env("APB_NO_REGISTRY", "1");
        c
    }

    fn eval_json(&self, args: &[&str]) -> (i32, Value) {
        let out = self
            .apb()
            .args(["eval", "rev", "--yes", "--json"])
            .args(args)
            .output()
            .unwrap();
        let v = serde_json::from_slice(&out.stdout).unwrap_or_else(|e| {
            panic!(
                "not json ({e}): {}\n{}",
                String::from_utf8_lossy(&out.stdout),
                String::from_utf8_lossy(&out.stderr)
            )
        });
        (out.status.code().unwrap_or(-1), v)
    }

    fn probe(&self, name: &str) -> String {
        fs::read_to_string(self.probe.join(name)).unwrap_or_default()
    }
}

fn rep(v: &Value) -> &Value {
    &v["result"]["cases"][0]["repetitions"][0]
}

const LIST_BUG: &str =
    "apb connector call mock-tracker list_items --args '{\"q\":\"bug\"}' > \"$P/call-1.json\"";

#[test]
fn a_doubled_case_runs_and_passes() {
    let env = setup(CASE, LIST_BUG);
    let (code, v) = env.eval_json(&[]);
    assert_eq!(code, 0, "{v:#}");
    let rep = rep(&v);
    assert_eq!(rep["verdict"], "passed", "{rep:#}");
    let answer: Value = serde_json::from_str(&env.probe("call-1.json")).unwrap();
    assert_eq!(answer["body"]["items"], serde_json::json!([1]), "{answer}");
    assert_eq!(
        rep["connector_calls"],
        serde_json::json!([{ "function": "mock-tracker.list_items", "site": "look",
            "answered": true, "delivered": true }])
    );
    let run_dir = PathBuf::from(rep["run_dir"].as_str().unwrap());
    let calls = apb_engine::eval::doubles::read_calls(&run_dir).unwrap();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].args, serde_json::json!({ "q": "bug" }));
    // The manifest names the account, nothing to reach it with.
    let m = apb_engine::manifest::read(&run_dir).unwrap().unwrap();
    assert!(m.connectors.iter().all(|c| c.accounts.is_empty()));
}

#[test]
fn times_is_counted_across_call_processes() {
    let env = setup(
        CASE,
        &format!(
            "{LIST_BUG}\napb connector call mock-tracker list_items --args '{{\"q\":\"bug\"}}' > \"$P/call-2.json\""
        ),
    );
    let (code, v) = env.eval_json(&[]);
    assert_eq!(code, 0, "{v:#}");
    let first: Value = serde_json::from_str(&env.probe("call-1.json")).unwrap();
    let second: Value = serde_json::from_str(&env.probe("call-2.json")).unwrap();
    assert_eq!(first["body"]["items"], serde_json::json!([1]));
    assert_eq!(second["body"]["items"], serde_json::json!([2]));
    assert_eq!(rep(&v)["connector_calls"].as_array().unwrap().len(), 2);
}

#[test]
fn a_partly_doubled_case_is_refused_before_anything_runs() {
    let partial = CASE.replace(
        "    create_item:\n      - answer: { status: 201, body: { id: 7 } }\n",
        "",
    );
    let env = setup(&partial, "touch \"$P/ran\"");
    let out = env
        .apb()
        .args(["eval", "rev", "--yes", "--json"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(2));
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    let v: Value = serde_json::from_slice(&out.stdout).unwrap_or_else(|e| panic!("{e}: {text}"));
    assert_eq!(v["error"], "invalid_suite", "{v:#}");
    assert!(text.contains("V280"), "{text}");
    assert!(text.contains("create_item"), "{text}");
    assert!(!env.probe.join("ran").exists(), "nothing ran");
    assert!(!env.cfg.path().join("evals/runs").exists());
}

#[test]
fn the_dry_run_lists_doubled_functions() {
    let env = setup(CASE, "touch \"$P/ran\"");
    let out = env
        .apb()
        .args(["eval", "rev", "--dry-run"])
        .output()
        .unwrap();
    assert!(out.status.success(), "{out:?}");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("doubled x1 (doubled: mock-tracker.create_item, mock-tracker.list_items)"),
        "{stdout}"
    );
    let (code, v) = env.eval_json(&["--dry-run"]);
    assert_eq!(code, 0);
    assert_eq!(
        v["plan"]["cases"][0]["doubled"],
        serde_json::json!(["mock-tracker.create_item", "mock-tracker.list_items"])
    );
    assert!(!env.probe.join("ran").exists());
}

#[cfg(unix)]
#[test]
fn a_doubled_case_puts_this_binary_first_on_the_step_path() {
    let env = setup(
        CASE,
        &format!("command -v apb > \"$P/apb\"\nprintf '%s' \"$PATH\" > \"$P/path\"\n{LIST_BUG}"),
    );
    let given = "/usr/bin:/bin";
    let out = env
        .apb()
        .env("PATH", given)
        .args(["eval", "rev", "--yes", "--json"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(0), "{out:?}");
    let bin = apb_bin();
    let dir = bin.parent().unwrap();
    assert_eq!(
        fs::canonicalize(env.probe("apb").trim()).unwrap(),
        fs::canonicalize(bin).unwrap()
    );
    assert_eq!(env.probe("path"), format!("{}:{given}", dir.display()));
}

/// An inherited `PATH` that cannot be passed on after this binary's
/// directory refuses the repetition with the reason, never a shorter one.
#[cfg(unix)]
#[test]
fn a_path_that_cannot_be_passed_on_refuses_the_repetition() {
    use std::os::unix::ffi::OsStrExt;
    let env = setup(CASE, "touch \"$P/ran\"");
    let out = env
        .apb()
        .env(
            "PATH",
            std::ffi::OsStr::from_bytes(b"/usr/bin:/bin:/opt/\xff"),
        )
        .args(["eval", "rev", "--yes", "--json"])
        .output()
        .unwrap();
    let v: Value = serde_json::from_slice(&out.stdout).unwrap();
    let rep = rep(&v);
    assert_eq!(rep["verdict"], "error", "{v:#}");
    assert!(
        rep["checks"]
            .to_string()
            .contains("the inherited PATH is not UTF-8"),
        "{v:#}"
    );
    assert!(!env.probe.join("ran").exists(), "nothing ran");
}

/// Every `eval-calls.*` entry under `dir`, at any depth.
fn captures_under(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        let Ok(entries) = fs::read_dir(&d) else {
            continue;
        };
        for e in entries.flatten() {
            let path = e.path();
            if e.file_name().to_string_lossy().starts_with("eval-calls.") {
                out.push(path.clone());
            }
            if e.file_type().is_ok_and(|t| t.is_dir()) {
                stack.push(path);
            }
        }
    }
    out
}

/// The repetition directories of this eval under the config directory and
/// every `eval-calls.*` entry: what of an external-data repetition is
/// still on disk.
fn remains(env: &Env) -> Vec<PathBuf> {
    let evals = env.cfg.path().join("evals");
    let mut out = captures_under(env.cfg.path());
    for dir in ["runs", "kept"] {
        if evals.join(dir).exists() {
            out.push(evals.join(dir));
        }
    }
    if let Ok(scratch) = fs::read_dir(evals.join("scratch")) {
        for eval in scratch.flatten() {
            if let Ok(reps) = fs::read_dir(eval.path()) {
                out.extend(reps.flatten().map(|e| e.path()).filter(|p| {
                    p.file_name()
                        .is_some_and(|n| n.to_string_lossy().starts_with("doubled-"))
                }));
            }
        }
    }
    out
}

/// What every external-data repetition with doubles shows: never stored or
/// kept, its calls and checks reported, nothing of it left on disk.
fn assert_never_kept(env: &Env, v: &Value) {
    let rep = rep(v);
    assert!(rep["run_dir"].is_null(), "{v:#}");
    assert!(rep["kept_worktree"].is_null(), "{v:#}");
    assert!(
        v["warnings"]
            .to_string()
            .contains("is never kept; the repetition directory was removed"),
        "{v:#}"
    );
    assert!(!v["warnings"].to_string().contains("discarded"), "{v:#}");
    assert_eq!(remains(env), Vec::<PathBuf>::new());
}

/// A passing external-data repetition with doubles: its checks read the
/// capture, then the capture and the repetition are removed.
#[test]
fn a_passing_external_data_repetition_leaves_nothing() {
    let env = setup(CASE, LIST_BUG);
    let (code, v) = env.eval_json(&["--external-data"]);
    assert_eq!(code, 0, "{v:#}");
    let rep = rep(&v);
    assert_eq!(rep["verdict"], "passed", "{rep:#}");
    assert_eq!(rep["connector_calls"].as_array().unwrap().len(), 1);
    assert_never_kept(&env, &v);
    // The text report says why nothing was stored.
    let out = env
        .apb()
        .args(["eval", "rev", "--yes", "--external-data"])
        .output()
        .unwrap();
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(
        text.contains("not stored: an --external-data run keeps no result"),
        "{text}"
    );
    assert_eq!(remains(&env), Vec::<PathBuf>::new());
}

/// A failing one: the failing check is in the result printed, nothing is
/// kept for a look, a step that took the write bit off its directories
/// included.
#[test]
fn a_failing_external_data_repetition_leaves_nothing() {
    let case = CASE.replace("args: { q: bug }, min: 1", "args: { q: bug }, min: 5");
    let env = setup(
        &case,
        &format!("{LIST_BUG}\nchmod a-w \"$APB_RUN_DIR\" \"$APB_RUN_DIR/..\""),
    );
    let (code, v) = env.eval_json(&["--external-data"]);
    assert_ne!(code, 0, "{v:#}");
    let rep = rep(&v);
    assert_eq!(rep["verdict"], "failed", "{rep:#}");
    assert!(
        rep["checks"]
            .to_string()
            .contains("connector_calls[mock-tracker.list_items]"),
        "{rep:#}"
    );
    assert_never_kept(&env, &v);
}

/// One whose driver still seems to live (the stub's detached helper takes
/// the driver's place, as `eval_cli_test::a_kept_tree_survives_the_scratch_cleanup`
/// does, and takes the write bit off the run directory): it is not kept
/// either, and nothing is stopped for it. The helper still runs afterwards
/// and is stopped by the test.
#[cfg(unix)]
#[test]
fn an_external_data_repetition_with_a_live_driver_leaves_nothing() {
    let perl = std::process::Command::new("perl")
        .args(["-MPOSIX", "-e", "1"])
        .status()
        .is_ok_and(|s| s.success());
    if !perl {
        eprintln!("skipped: perl with POSIX is not installed");
        return;
    }
    let case = CASE.replace("repeat: 1\n", "repeat: 1\nlimits: { timeout: 10s }\n");
    // Bounded, so a helper a failed test leaves behind ends by itself.
    let holder = "echo $$ > \"$RUN/driver.pid\"\nchmod a-w \"$RUN\"\necho $$ > \"$P/helper.pid\"\ni=0\nwhile [ $i -lt 120 ]; do sleep 1; i=$((i+1)); done\n";
    let env = setup(
        &case,
        &format!(
            "{LIST_BUG}\ncat > \"$P/holder.sh\" <<'EOF'\n{holder}EOF\nRUN=\"$APB_RUN_DIR\" P=\"$P\" perl -MPOSIX -e 'defined(POSIX::setsid()) or die \"setsid: $!\"; exec {{\"/bin/sh\"}} \"apb\", @ARGV or die \"exec: $!\"' \"$P/holder.sh\" </dev/null >/dev/null 2>&1 &\ni=0\nwhile [ ! -s \"$P/helper.pid\" ] && [ $i -lt 100 ]; do sleep 0.05; i=$((i+1)); done\necho $$ > \"$P/agent.pid\"\nexec sleep 45"
        ),
    );
    // Stops the helper and the agent this test started, on a panic too.
    struct KillOnDrop<'a>(&'a Env);
    impl Drop for KillOnDrop<'_> {
        fn drop(&mut self) {
            for f in ["helper.pid", "agent.pid"] {
                if let Ok(pid) = self.0.probe(f).trim().parse::<i32>() {
                    // SAFETY: plain kill(2) of a process this test started.
                    unsafe { libc::kill(pid, libc::SIGKILL) };
                }
            }
        }
    }
    let stop = KillOnDrop(&env);
    let (_, v) = env.eval_json(&["--external-data"]);
    drop(stop);
    assert!(
        !env.probe("helper.pid").is_empty(),
        "the helper never ran: {v:#}"
    );
    assert_eq!(rep(&v)["connector_calls"].as_array().unwrap().len(), 1);
    assert_never_kept(&env, &v);
}

/// A repetition that cannot be removed (a step left a directory its owner
/// cannot open) says so with the path, fails, and never says "discarded".
#[cfg(unix)]
#[test]
fn an_external_data_repetition_that_cannot_be_removed_fails_and_says_where() {
    use std::os::unix::fs::PermissionsExt;
    // Whether this user can open a directory of mode 000 (root can):
    // probed before the eval, so its result is always checked otherwise.
    let probe = tempfile::tempdir().unwrap();
    let closed = probe.path().join("closed");
    fs::create_dir(&closed).unwrap();
    fs::set_permissions(&closed, fs::Permissions::from_mode(0o000)).unwrap();
    let can_open = fs::read_dir(&closed).is_ok();
    fs::set_permissions(&closed, fs::Permissions::from_mode(0o700)).unwrap();
    if can_open {
        eprintln!("skipped: this user can open any directory");
        return;
    }
    let env = setup(
        CASE,
        &format!(
            "{LIST_BUG}\nmkdir -p \"$APB_RUN_DIR/stuck/x\"\nchmod 000 \"$APB_RUN_DIR/stuck\"\necho \"$APB_RUN_DIR/stuck\" > \"$P/stuck\""
        ),
    );
    let (code, v) = env.eval_json(&["--external-data"]);
    let stuck = env.probe("stuck");
    assert!(
        !stuck.trim().is_empty(),
        "the step never wrote the probe: {v:#}"
    );
    // Give the directory its mode back for the cleanup.
    let _ = std::process::Command::new("chmod")
        .arg("700")
        .arg(stuck.trim())
        .status();
    assert_ne!(code, 0, "{v:#}");
    let rep = rep(&v);
    assert_eq!(rep["verdict"], "error", "{rep:#}");
    assert!(
        rep["run_dir"].is_null() && rep["kept_worktree"].is_null(),
        "{rep:#}"
    );
    let warnings = v["warnings"].to_string();
    assert!(warnings.contains("could not be removed"), "{v:#}");
    assert!(warnings.contains("doubled-1"), "the path is named: {v:#}");
    assert!(!warnings.contains("discarded"), "{v:#}");
    assert!(
        rep["checks"].to_string().contains("external_data_removal"),
        "{rep:#}"
    );
}

/// A step moved `tree/.apb` out of the repetition and put a link in its
/// place: the removal follows no link, so the moved run (its capture
/// included) is left where it went, the eval fails naming the path and says
/// the run tree was moved.
#[cfg(unix)]
#[test]
fn an_external_data_repetition_whose_run_tree_was_moved_fails_and_touches_nothing_outside() {
    let env = setup(
        CASE,
        &format!(
            "{LIST_BUG}\nT=\"${{APB_RUN_DIR%/.apb/runs/*}}\"\nmkdir -p \"$P/outside\"\nmv \"$T/.apb\" \"$P/outside/apb\"\nln -s \"$P/outside/apb\" \"$T/.apb\""
        ),
    );
    let (code, v) = env.eval_json(&["--external-data"]);
    assert_ne!(code, 0, "{v:#}");
    let rep = rep(&v);
    assert_eq!(rep["verdict"], "error", "{rep:#}");
    assert!(
        rep["run_dir"].is_null() && rep["kept_worktree"].is_null(),
        "{rep:#}"
    );
    let warnings = v["warnings"].to_string();
    assert!(warnings.contains("the run tree was moved"), "{v:#}");
    assert!(warnings.contains("tree/.apb is a link"), "{v:#}");
    assert!(!warnings.contains("discarded"), "{v:#}");
    let run_id = rep["run_id"].as_str().unwrap();
    let outside = env.probe.join("outside/apb/runs").join(run_id);
    assert!(
        outside
            .join(apb_engine::eval::doubles::CALLS_FILE)
            .is_file(),
        "the file outside the repetition survives"
    );
}

/// Without `--external-data` a doubled repetition that cannot be stored
/// is kept as before: its tree and its capture stay for a look.
#[test]
fn a_doubled_repetition_without_external_data_keeps_its_tree_and_capture() {
    let env = setup(CASE, LIST_BUG);
    write(&env.cfg.path().join("evals/runs/rev"), "not a directory\n");
    let (_, v) = env.eval_json(&[]);
    let rep = rep(&v);
    let kept = PathBuf::from(
        rep["kept_worktree"]
            .as_str()
            .unwrap_or_else(|| panic!("kept: {v:#}")),
    );
    let run_dir = kept.join(".apb/runs").join(rep["run_id"].as_str().unwrap());
    assert!(run_dir.join("events.jsonl").is_file());
    assert_eq!(
        apb_engine::eval::doubles::read_calls(&run_dir)
            .unwrap()
            .len(),
        1
    );
}

/// A doubled case whose run makes no call: the capture exists, empty, from
/// the run's start, so journal and capture agree there was none and
/// `count: 0` passes.
#[test]
fn a_doubled_case_without_a_call_passes_count_zero() {
    let case = CASE.replace(
        "    - { function: mock-tracker.list_items, args: { q: bug }, min: 1 }\n",
        "    - { function: mock-tracker.list_items, count: 0 }\n",
    );
    let env = setup(&case, "true");
    let (code, v) = env.eval_json(&[]);
    assert_eq!(code, 0, "{v:#}");
    let rep = rep(&v);
    assert_eq!(rep["verdict"], "passed", "{rep:#}");
    let run_dir = PathBuf::from(rep["run_dir"].as_str().unwrap());
    assert!(
        run_dir
            .join(apb_engine::eval::doubles::CALLS_FILE)
            .is_file()
    );
    assert!(
        apb_engine::eval::doubles::read_calls(&run_dir)
            .unwrap()
            .is_empty()
    );
}
