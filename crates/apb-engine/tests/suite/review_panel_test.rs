//! The two shipped review-panel examples, run end to end with a stub agent
//! that answers per node (and per item) from planned outputs: the graph
//! template keeps a finished reviewer out of later rounds, hands an
//! exhausted bound to a person and rejects a fix that deletes a protected
//! test; the list form keeps a passed reviewer as carried and names the
//! open one in the outcome; a re-review on another model starts cold with
//! the saved remarks in its prompt. No model is ever called.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::mpsc;

use apb_core::registry::init_project;
use apb_engine::RunResult;
use apb_engine::error::EngineError;
use apb_engine::event::{Event, EventPayload, read_all};
use apb_engine::review::{ReviewCommand, post_review};
use apb_engine::run_outcome::summary::GapKind;
use apb_engine::run_view::RunView;
use apb_engine::scheduler::{RunOptions, run};
use apb_engine::state::RunStatus;

use crate::common;
use crate::list_run_test::{DEADLINE, Env, poll_until};

fn example(file: &str) -> String {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../examples/playbooks")
        .join(file);
    fs::read_to_string(path).expect("the shipped example")
}

/// Seeds `yaml` as playbook `id` in a git work tree with one protected test
/// file, and profiles `main` and `other` on different models.
fn seed(root: &Path, id: &str, yaml: &str) {
    init_project(root).unwrap();
    let vdir = root.join(".apb/playbooks").join(id).join("1.0.0");
    fs::create_dir_all(&vdir).unwrap();
    fs::write(vdir.join("playbook.yaml"), yaml).unwrap();
    fs::write(
        root.join(".apb/playbooks").join(id).join("current"),
        "1.0.0",
    )
    .unwrap();
    common::seed_profile(root, "main", "claude", "haiku", &[]);
    common::seed_profile(root, "other", "claude", "sonnet", &[]);
    fs::create_dir_all(root.join("tests")).unwrap();
    fs::write(root.join("tests/panel.txt"), "keep me\n").unwrap();
    fs::write(root.join(".gitignore"), ".apb/\nstub/\nagent.sh\n").unwrap();
    let git = |args: &[&str]| {
        let ok = std::process::Command::new("git")
            .args(args)
            .current_dir(root)
            .output()
            .unwrap()
            .status
            .success();
        assert!(ok, "git {args:?}");
    };
    git(&["init", "-q"]);
    git(&["add", "."]);
    git(&[
        "-c",
        "user.name=t",
        "-c",
        "user.email=t@example.com",
        "commit",
        "-qm",
        "init",
    ]);
}

fn stub_dir(root: &Path) -> PathBuf {
    root.join("stub")
}

/// The stub agent. Its key is the node, or `node.item` for a list item. Per
/// invocation it counts `$N` for the key, appends `key N` to `log`, keeps
/// the prompt as `prompt-<key>-<N>`, runs `hook`, and writes the outputs
/// planned in `out-<key>-<N>` (else `out-<key>`, else no blocking finding).
fn stub(root: &Path, hook: &str) -> String {
    let d = stub_dir(root);
    fs::create_dir_all(&d).unwrap();
    let body = format!(
        r#"#!/bin/sh
D='{d}'
K="$APB_NODE_ID${{APB_ITEM_ID:+.$APB_ITEM_ID}}"
N=$(cat "$D/n-$K" 2>/dev/null || echo 0); N=$((N+1)); echo $N > "$D/n-$K"
echo "$K $N" >> "$D/log"
P=''; take=0; for a in "$@"; do if [ $take = 1 ]; then P="$a"; take=0; fi; [ "$a" = -- ] && take=1; done
printf '%s' "$P" > "$D/prompt-$K-$N"
{hook}
O=$(cat "$D/out-$K-$N" 2>/dev/null || cat "$D/out-$K" 2>/dev/null || echo '{{"blocking":"0","findings":""}}')
printf '{{"status":"success","outputs":%s}}' "$O" > "$APB_STATUS_FILE"
printf '\n```yaml\nstatus: success\nsummary: ok\n```\n'
"#,
        d = d.display()
    );
    let path = root.join("agent.sh");
    common::write_sync(&path, &body);
    let mut p = fs::metadata(&path).unwrap().permissions();
    p.set_mode(0o755);
    fs::set_permissions(&path, p).unwrap();
    path.to_string_lossy().into_owned()
}

/// Plans the outputs of `key` on its run `n` (every run when `None`).
fn plan(root: &Path, key: &str, n: Option<u32>, outputs: &str) {
    let name = match n {
        Some(n) => format!("out-{key}-{n}"),
        None => format!("out-{key}"),
    };
    fs::write(stub_dir(root).join(name), outputs).unwrap();
}

/// How many times the stub ran `key`.
fn runs_of(root: &Path, key: &str) -> u32 {
    fs::read_to_string(stub_dir(root).join(format!("n-{key}")))
        .map(|s| s.trim().parse().unwrap())
        .unwrap_or(0)
}

fn prompt_of(root: &Path, key: &str, n: u32) -> String {
    fs::read_to_string(stub_dir(root).join(format!("prompt-{key}-{n}"))).unwrap_or_default()
}

/// The collector's state: `open` names the open reviewers.
fn collected(open: &[&str]) -> String {
    let state = |r: &str| if open.contains(&r) { "open" } else { "clean" };
    let remarks = |r: &str| {
        if open.contains(&r) {
            format!("{r} remark: tighten the check")
        } else {
            String::new()
        }
    };
    serde_json::json!({
        "correctness": state("correctness"),
        "security": state("security"),
        "style": state("style"),
        "open_count": open.len().to_string(),
        "remarks_correctness": remarks("correctness"),
        "remarks_security": remarks("security"),
        "remarks_style": remarks("style"),
    })
    .to_string()
}

/// Runs `id` on a thread; its result, an error included, comes back on
/// the channel, so a run that fails to start fails the test at once.
fn spawn(root: &Path, id: &str) -> mpsc::Receiver<Result<RunResult, EngineError>> {
    let (tx, rx) = mpsc::channel();
    let (root, id) = (root.to_path_buf(), id.to_string());
    std::thread::spawn(move || {
        let _ = tx.send(run(&root, &id, None, RunOptions::default()));
    });
    rx
}

fn run_dir(root: &Path) -> PathBuf {
    poll_until("the run directory", || {
        fs::read_dir(root.join(".apb/runs"))
            .ok()?
            .filter_map(|e| e.ok())
            .find(|e| e.path().is_dir())
            .map(|e| e.path())
    })
}

/// Waits for the `human_review` gate and decides `decision` on it. A run
/// that ends without reaching the gate fails the test at once.
fn decide_gate(dir: &Path, decision: &str) {
    let gate = poll_until("the human_review gate or the run's end", || {
        read_all(dir).ok()?.iter().find_map(|e| match &e.payload {
            EventPayload::ReviewRequested { node, .. } if node == "human_review" => Some(true),
            EventPayload::RunFinished { .. } => Some(false),
            _ => None,
        })
    });
    assert!(gate, "the run ended before the human_review gate");
    post_review(
        dir,
        ReviewCommand {
            node: "human_review".into(),
            decision: decision.into(),
            note: String::new(),
        },
    )
    .unwrap();
}

fn joined(rx: &mpsc::Receiver<Result<RunResult, EngineError>>) -> RunResult {
    rx.recv_timeout(DEADLINE)
        .expect("the run ends")
        .expect("the run starts")
}

fn reached(events: &[Event], node: &str) -> bool {
    events
        .iter()
        .any(|e| matches!(&e.payload, EventPayload::NodeStarted { node: n, .. } if n == node))
}

#[test]
fn the_template_keeps_a_finished_reviewer_out_and_rounds_end_at_the_bound() {
    // One reviewer finishes in round 1, another in round 3.
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let _dump = DumpOnPanic(root.to_path_buf());
    seed(root, "review-panel", &example("review-panel.yaml"));
    let _env = Env::set(&stub(root, ""), &[]);
    plan(root, "collect", Some(1), &collected(&["security"]));
    plan(root, "collect", Some(2), &collected(&["security"]));
    plan(root, "collect", Some(3), &collected(&[]));
    let res = run(root, "review-panel", None, RunOptions::default()).unwrap();
    assert_eq!(res.outcome, RunStatus::Succeeded);
    assert_eq!(runs_of(root, "review_correctness"), 1);
    assert_eq!(
        runs_of(root, "rereview_correctness"),
        0,
        "a clean reviewer is not re-run"
    );
    assert_eq!(runs_of(root, "rereview_security"), 2);
    assert_eq!(runs_of(root, "collect"), 3);
    let ev = read_all(&root.join(".apb/runs").join(&res.run_id)).unwrap();
    assert!(!reached(&ev, "human_review"));
    // The reviewers of a round run side by side, and the collector of rounds
    // 2 and 3 waits for the security re-review of that round.
    let at = |finished: bool, node: &str| -> Vec<usize> {
        ev.iter()
            .enumerate()
            .filter(|(_, e)| match &e.payload {
                EventPayload::NodeStarted { node: n, .. } => !finished && n == node,
                EventPayload::NodeFinished { node: n, .. } => finished && n == node,
                _ => false,
            })
            .map(|(i, _)| i)
            .collect()
    };
    let rereviewed = at(true, "rereview_security");
    let collected_at = at(false, "collect");
    assert!(rereviewed[0] < collected_at[1] && rereviewed[1] < collected_at[2]);
    let first_reviews = at(false, "review_style")[0];
    assert!(
        first_reviews < at(true, "review_correctness")[0],
        "round 1 reviews overlap"
    );
}

#[test]
fn the_template_hands_a_reviewer_never_clean_to_a_person_after_three_rounds() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let _dump = DumpOnPanic(root.to_path_buf());
    seed(root, "review-panel", &example("review-panel.yaml"));
    let _env = Env::set(&stub(root, ""), &[]);
    plan(root, "collect", None, &collected(&["security"]));
    let rx = spawn(root, "review-panel");
    let rd = run_dir(root);
    decide_gate(&rd, "stop");
    let res = joined(&rx);
    assert_eq!(res.outcome, RunStatus::Failed);
    assert_eq!(runs_of(root, "rereview_security"), 2, "three rounds in all");
    assert_eq!(runs_of(root, "fix"), 2, "every fix is reviewed");
}

#[test]
fn a_fix_that_deletes_a_protected_test_fails_its_attempt() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let _dump = DumpOnPanic(root.to_path_buf());
    seed(root, "review-panel", &example("review-panel.yaml"));
    let hook = r#"if [ "$K" = fix ] && [ "$N" = 1 ]; then rm -f tests/panel.txt; fi"#;
    let _env = Env::set(&stub(root, hook), &[]);
    plan(root, "collect", Some(1), &collected(&["style"]));
    plan(root, "collect", Some(2), &collected(&[]));
    let res = run(root, "review-panel", None, RunOptions::default()).unwrap();
    let ev = read_all(&root.join(".apb/runs").join(&res.run_id)).unwrap();
    let rejected = ev.iter().any(|e| {
        matches!(&e.payload, EventPayload::AttemptFinished { node, attempt: 1, status, .. }
            if node == "fix" && status == "failed")
    });
    assert!(rejected, "the deleting attempt fails");
    assert!(
        root.join("tests/panel.txt").is_file(),
        "the protected test is restored"
    );
    assert_eq!(res.outcome, RunStatus::Succeeded, "the retry fixes it");
}

#[test]
fn the_list_form_carries_the_passed_reviewer_and_names_the_open_one() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let _dump = DumpOnPanic(root.to_path_buf());
    seed(
        root,
        "review-panel-list",
        &example("review-panel-list.yaml"),
    );
    let _env = Env::set(&stub(root, ""), &[]);
    plan(
        root,
        "panel.security",
        None,
        r#"{"blocking":"2","findings":"auth.rs:10 token logged"}"#,
    );
    let rx = spawn(root, "review-panel-list");
    let rd = run_dir(root);
    decide_gate(&rd, "stop");
    assert_eq!(joined(&rx).outcome, RunStatus::Failed);
    assert_eq!(
        runs_of(root, "panel.correctness"),
        1,
        "a passed reviewer is kept"
    );
    assert_eq!(runs_of(root, "panel.security"), 3);
    assert!(
        prompt_of(root, "panel.security", 2).contains("auth.rs:10 token logged"),
        "a re-run reviewer sees its previous findings"
    );
    let ev = read_all(&rd).unwrap();
    let carried = ev
        .iter()
        .filter(|e| {
            matches!(&e.payload, EventPayload::ListItemSettled { item, carried: true, .. }
                if item == "correctness")
        })
        .count();
    assert_eq!(carried, 2, "carried into both later rounds");
    let id = rd.file_name().unwrap().to_string_lossy().into_owned();
    let outcome = RunView::load(&rd, &id).unwrap().outcome(&rd);
    let open: Vec<&str> = outcome
        .unverified
        .iter()
        .filter(|g| g.kind == GapKind::ItemNeedsLook)
        .map(|g| g.subject.as_str())
        .collect();
    assert_eq!(open, ["panel/security"]);
}

#[test]
fn a_rereview_on_another_model_starts_cold_with_the_saved_remarks() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let _dump = DumpOnPanic(root.to_path_buf());
    let yaml = example("review-panel.yaml").replacen(
        "    continue_session: review_security\n",
        "    continue_session: review_security\n    profile: other\n",
        1,
    );
    seed(root, "review-panel", &yaml);
    let _env = Env::set(&stub(root, ""), &[]);
    plan(root, "collect", Some(1), &collected(&["security"]));
    plan(root, "collect", Some(2), &collected(&[]));
    let res = run(root, "review-panel", None, RunOptions::default()).unwrap();
    assert_eq!(res.outcome, RunStatus::Succeeded);
    let ev = read_all(&root.join(".apb/runs").join(&res.run_id)).unwrap();
    let handoff = ev.iter().find_map(|e| match &e.payload {
        EventPayload::SessionHandoff { node, warm, .. } if node == "rereview_security" => {
            Some(*warm)
        }
        _ => None,
    });
    assert_eq!(handoff, Some(false), "another model starts a fresh session");
    assert!(
        prompt_of(root, "rereview_security", 1).contains("security remark: tighten the check"),
        "the fresh session gets the saved remarks"
    );
}

/// Prints the run's journal and the stub's log when a test panics, so a
/// failure seen once under load can be read without running it again.
struct DumpOnPanic(PathBuf);

impl Drop for DumpOnPanic {
    fn drop(&mut self) {
        if !std::thread::panicking() {
            return;
        }
        let runs = fs::read_dir(self.0.join(".apb/runs")).into_iter().flatten();
        for run in runs.filter_map(|e| e.ok()) {
            let journal = fs::read_to_string(run.path().join("events.jsonl")).unwrap_or_default();
            eprintln!("--- journal {} ---\n{journal}", run.path().display());
        }
        let log = fs::read_to_string(stub_dir(&self.0).join("log")).unwrap_or_default();
        eprintln!("--- stub log ---\n{log}");
    }
}
