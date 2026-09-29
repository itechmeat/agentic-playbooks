//! `apb eval` (0.24.0): the runner end to end with a stub agent
//! (`APB_AGENT_CMD`), each test in its own config directory.

use std::fs;
use std::path::{Path, PathBuf};

use predicates::prelude::*;
use serde_json::Value;

use crate::common::apb;

/// A review-like playbook: one agent node that writes a report.
const PLAYBOOK: &str = "schema: 2\nid: rev\nname: rev\nversion: 1.0.0\ndefaults: { profile: x }\neffects: [fs_read, fs_write]\ngoal:\n  statement: a report exists\n  criteria:\n    - { description: report written, check: { type: marker, marker: REPORT-OK } }\nnodes:\n  - { id: start, type: start }\n  - { id: review, type: agent_task, prompt: review }\n  - { id: done, type: finish, outcome: success }\n  - { id: failed, type: finish, outcome: failure }\nedges:\n  - { from: start, to: review }\n  - { from: review, to: done, condition: { type: node_status, node: review, equals: success } }\n  - { from: review, to: failed, condition: { type: node_status, node: review, equals: failure } }\n";

const CASE: &str = "schema: 1\nid: writes-report\ninstruction: review it\nfixture:\n  dir: fixtures/base\n  change: fixtures/change\nchecks:\n  run: { outcome: [succeeded] }\n  route: { visits: [review, done], in_order: true, not_visits: [failed] }\n  outputs: [{ node: review, matches: \"REPORT-OK\" }]\n  files:\n    - { path: report.md, matches: \"names lib.txt\" }\n    - { path: lib.txt, unchanged_from_fixture: true }\n  events: { absent: [run_error] }\n  scripts: [scripts/branch.sh]\nrepeat: 1\n";

/// The stub agent: writes `report.md` in its working directory; on the
/// second repetition (`APB_EVAL_SCRATCH` ends in `-2`) it writes the wrong
/// report, so a repeated case passes once.
const STUB: &str = "#!/bin/sh\ncase \"$APB_EVAL_SCRATCH\" in\n  *-2) echo 'nothing useful' > report.md ;;\n  *) echo 'the review names lib.txt' > report.md ;;\nesac\nprintf 'REPORT-OK\\n```yaml\\nstatus: success\\nsummary: done\\n```\\n'\n";

struct Env {
    project: tempfile::TempDir,
    cfg: tempfile::TempDir,
    stub: PathBuf,
}

fn write(path: &Path, body: &str) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, body).unwrap();
}

fn setup(playbook: &str) -> Env {
    let project = tempfile::tempdir().unwrap();
    let cfg = tempfile::tempdir().unwrap();
    let root = project.path();
    apb_core::registry::init_project(root).unwrap();
    let pb = root.join(".apb/playbooks/rev");
    write(&pb.join("1.0.0/playbook.yaml"), playbook);
    write(&pb.join("current"), "1.0.0");
    write(
        &root.join(".apb/profiles/x/profile.yaml"),
        "name: x\nexecutor:\n  agent: claude\n  model: claude-haiku-4-5-20251001\n",
    );
    let ev = pb.join("evals");
    write(&ev.join("writes-report.yaml"), CASE);
    write(&ev.join("fixtures/base/lib.txt"), "base\n");
    write(&ev.join("fixtures/change/lib.txt"), "changed\n");
    write(
        &ev.join("scripts/branch.sh"),
        "#!/bin/sh\n[ \"$(git rev-parse --abbrev-ref HEAD)\" = eval-change ] && git rev-parse --verify -q origin/main >/dev/null && [ -d \"$APB_EVAL_RUN_DIR\" ] && [ \"$APB_EVAL_CASE\" = writes-report ]\n",
    );
    let stub = cfg.path().join("stub.sh");
    write(&stub, STUB);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&stub, fs::Permissions::from_mode(0o755)).unwrap();
    }
    Env { project, cfg, stub }
}

impl Env {
    fn eval(&self, args: &[&str]) -> assert_cmd::assert::Assert {
        apb()
            .arg("eval")
            .args(args)
            .current_dir(self.project.path())
            .env("APB_CONFIG_DIR", self.cfg.path())
            .env("APB_AGENT_CMD", &self.stub)
            .env("APB_NO_REGISTRY", "1")
            .assert()
    }

    fn eval_json(&self, args: &[&str]) -> (i32, Value) {
        let mut a = vec!["rev", "--yes", "--json"];
        a.extend_from_slice(args);
        let out = self.eval(&a).get_output().clone();
        let v = serde_json::from_slice(&out.stdout).unwrap_or_else(|e| {
            panic!(
                "not json ({e}): {}\n{}",
                String::from_utf8_lossy(&out.stdout),
                String::from_utf8_lossy(&out.stderr)
            )
        });
        (out.status.code().unwrap_or(-1), v)
    }
}

/// Every path under `dir`, relative, sorted.
fn listing(dir: &Path) -> Vec<String> {
    fn walk(base: &Path, dir: &Path, out: &mut Vec<String>) {
        for e in fs::read_dir(dir).unwrap().flatten() {
            let p = e.path();
            out.push(p.strip_prefix(base).unwrap().to_string_lossy().into_owned());
            if p.is_dir() && !p.is_symlink() {
                walk(base, &p, out);
            }
        }
    }
    let mut out = Vec::new();
    walk(dir, dir, &mut out);
    out.sort();
    out
}

#[test]
fn a_passing_case_is_checked_stored_and_leaves_nothing_behind() {
    let env = setup(PLAYBOOK);
    let before = listing(env.project.path());
    let (code, v) = env.eval_json(&[]);
    assert_eq!(code, 0, "{v:#}");
    let case = &v["result"]["cases"][0];
    assert_eq!(case["case"], "writes-report");
    assert_eq!(
        (case["passes"].as_u64(), case["of"].as_u64()),
        (Some(1), Some(1))
    );
    let rep = &case["repetitions"][0];
    assert_eq!(rep["verdict"], "passed", "{rep:#}");
    // The check list itself is the snapshot test's; here: what is stored
    // and what is left behind. The runner reports a kept tree, so a clean
    // run must report none.
    assert!(rep["kept_worktree"].is_null(), "{rep:#}");
    // No write outside the scratch dirs: the project is untouched, the run
    // directory was moved under the config dir, the scratch tree is gone.
    assert_eq!(listing(env.project.path()), before);
    let evals = env.cfg.path().join("evals");
    assert!(!evals.join("scratch").exists(), "scratch left behind");
    let run_dir = PathBuf::from(rep["run_dir"].as_str().unwrap());
    assert!(run_dir.starts_with(evals.join("runs/rev")), "{run_dir:?}");
    assert!(run_dir.join("events.jsonl").is_file());
    let stored = PathBuf::from(v["stored"].as_str().unwrap());
    assert!(stored.starts_with(evals.join("results/rev")) && stored.is_file());
    assert!(v["comparison"].is_null(), "nothing to compare with yet");
}

#[test]
fn repetitions_aggregate_into_a_pass_count() {
    let env = setup(PLAYBOOK);
    let (code, v) = env.eval_json(&["--repeat", "2"]);
    assert_eq!(code, 1, "one repetition fails: {v:#}");
    let case = &v["result"]["cases"][0];
    assert_eq!(
        (case["passes"].as_u64(), case["of"].as_u64()),
        (Some(1), Some(2))
    );
    let verdicts: Vec<&str> = case["repetitions"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["verdict"].as_str().unwrap())
        .collect();
    assert_eq!(verdicts, ["passed", "failed"]);
    let failed: Vec<&str> = case["repetitions"][1]["checks"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|c| c["status"] != "passed")
        .map(|c| c["kind"].as_str().unwrap())
        .collect();
    assert_eq!(failed, ["files[report.md]"]);
    assert!(case["wilson95"].is_array());
}

/// Replaces the fields that differ between runs, so the JSON shape and
/// every deterministic value can be compared exactly.
fn normalize(v: &mut Value) {
    const VOLATILE: [&str; 13] = [
        "eval_id",
        "started_at_ms",
        "finished_at_ms",
        "workspace",
        "config_key",
        "playbook_digest",
        "case_digest",
        "run_id",
        "run_dir",
        "duration_ms",
        "stored",
        "apb_version",
        "profile_bundles",
    ];
    match v {
        Value::Object(m) => {
            for (k, x) in m.iter_mut() {
                if VOLATILE.contains(&k.as_str()) && !x.is_null() {
                    *x = Value::String("*".into());
                } else {
                    normalize(x);
                }
            }
        }
        Value::Array(a) => a.iter_mut().for_each(normalize),
        _ => {}
    }
}

#[test]
fn the_json_output_matches_its_snapshot() {
    let env = setup(PLAYBOOK);
    let (_, mut v) = env.eval_json(&["--model", "claude:claude-haiku-4-5-20251001"]);
    normalize(&mut v);
    // `overrides_digest` is pinned on purpose: it is part of the storage key
    // of `--model` results (`evals/results/<id>/<key>.json`), so a change of
    // its canonical form would orphan them.
    let expected = serde_json::json!({
        "comparison": null,
        "stored": "*",
        "note": "note: an eval run is not a sandbox: it runs in a scratch repository with no real remote, but agents keep the network, your environment (apart from connector variables) and any CLI or git credential helper you are logged in to; use the case `env` to cut known ones (for example GH_CONFIG_DIR)",
        "full_environment_nodes": [],
        "warnings": ["case `writes-report` repetition 1 reported no cost: the invocation budget ($10.00) and max_usd cannot be enforced for this executor"],
        "result": {
            "eval_id": "*", "playbook": "rev", "version": "1.0.0",
            "started_at_ms": "*", "finished_at_ms": "*", "apb_version": "*",
            "workspace": "*", "config_key": "*",
            "config": {
                "playbook_digest": "*",
                "profile_bundles": "*",
                "executors": { "review": "claude/claude-haiku-4-5-20251001" },
                "overrides_digest": "sha256:f86f034919f5145ebcec05cae1cdf709102a0d2d2513ca672994d4e121a7c1da"
            },
            "cases": [{
                "case": "writes-report", "case_digest": "*", "passes": 1, "of": 1,
                "wilson95": [0.2065, 1.0],
                "repetitions": [{
                    "repetition": 1, "run_id": "*", "run_dir": "*",
                    "verdict": "passed", "outcome": "succeeded",
                    "checks": [
                        { "kind": "run.outcome", "status": "passed" },
                        { "kind": "goal", "status": "passed" },
                        { "kind": "route.visits", "status": "passed" },
                        { "kind": "route.not_visits", "status": "passed" },
                        { "kind": "outputs[review]", "status": "passed" },
                        { "kind": "files[report.md]", "status": "passed" },
                        { "kind": "files[lib.txt]", "status": "passed" },
                        { "kind": "events.absent[run_error]", "status": "passed" },
                        { "kind": "script[scripts/branch.sh]", "status": "passed" }
                    ],
                    "goal": [{ "index": 0, "description": "report written", "check": "marker", "status": "passed" }],
                    "usage": { "input_tokens": 0, "output_tokens": 0 },
                    "duration_ms": "*"
                }]
            }],
            "total_cost_usd": 0.0,
            "total_tokens": 0,
            "journal_agent_writable": true
        }
    });
    assert_eq!(v, expected, "{v:#}");
}

#[test]
fn a_second_configuration_is_compared_with_the_first() {
    let env = setup(PLAYBOOK);
    let (_, _) = env.eval_json(&["--model", "claude:model-a"]);
    let (_, v) = env.eval_json(&["--model", "claude:model-b"]);
    let c = &v["comparison"];
    assert_eq!(c["same_configuration"], false, "{c:#}");
    let changes: Vec<&str> = c["configuration_changes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|x| x.as_str().unwrap())
        .collect();
    assert!(
        changes.contains(&"executor review: claude/model-a -> claude/model-b"),
        "{changes:?}"
    );
    assert_eq!(c["cases"][0]["baseline"], "1/1");
    assert_eq!(c["cases"][0]["candidate"], "1/1");
    // `--compare` runs nothing and prints the same comparison as text.
    env.eval(&["rev", "--compare"])
        .success()
        .stdout(predicate::str::contains("configuration changed:"))
        .stdout(predicate::str::contains(
            "executor review: claude/model-a -> claude/model-b",
        ))
        .stdout(predicate::str::contains(
            "writes-report: baseline 1/1 -> candidate 1/1 (delta +0.00)",
        ));
    assert!(!env.cfg.path().join("evals/scratch").exists());
}

#[test]
fn an_irreversible_playbook_is_refused_before_anything_runs() {
    let env = setup(&PLAYBOOK.replace("effects: [fs_read, fs_write]", "effects: [irreversible]"));
    let (code, v) = env.eval_json(&[]);
    assert_eq!(code, 2);
    assert_eq!(v["refused"], "eval_refused_effects");
    assert!(v["reasons"][0].as_str().unwrap().contains("irreversible"));
    assert!(
        !env.cfg.path().join("evals").exists(),
        "nothing was created"
    );
    // `apb validate` says the same as V81.
    apb()
        .args(["validate", "rev"])
        .current_dir(env.project.path())
        .env("APB_CONFIG_DIR", env.cfg.path())
        .assert()
        .code(1)
        .stdout(predicate::str::contains("rev: error V81"));
}

#[test]
fn a_run_waiting_for_a_person_is_stopped_and_judged_by_the_case() {
    let gated = PLAYBOOK
        .replace(
            "  - { id: done, type: finish, outcome: success }",
            "  - { id: gate, type: human_review }\n  - { id: done, type: finish, outcome: success }",
        )
        .replace(
            "{ from: review, to: done, condition",
            "{ from: review, to: gate, condition",
        )
        .replace("edges:\n", "edges:\n  - { from: gate, to: done }\n");
    let env = setup(&gated);
    let case = CASE
        .replace(
            "run: { outcome: [succeeded] }",
            "run: { outcome: [stopped] }",
        )
        .replace("visits: [review, done]", "visits: [review, gate]");
    write(
        &env.project
            .path()
            .join(".apb/playbooks/rev/evals/writes-report.yaml"),
        &case,
    );
    let (code, v) = env.eval_json(&[]);
    let rep = &v["result"]["cases"][0]["repetitions"][0];
    assert_eq!(code, 0, "{v:#}");
    assert_eq!(rep["outcome"], "stopped");
    assert!(
        rep["stopped"]
            .as_str()
            .unwrap()
            .starts_with("eval_gate_unanswered"),
        "{rep:#}"
    );
    assert_eq!(
        rep["goal"],
        Value::Null,
        "the finish node was never reached"
    );
    assert!(!env.cfg.path().join("evals/scratch").exists());
}

#[test]
fn drafts_bad_cases_and_unknown_cases_are_refused() {
    let env = setup(PLAYBOOK);
    let pb = env.project.path().join(".apb/playbooks/rev");
    fs::write(pb.join("lifecycle"), "draft").unwrap();
    env.eval(&["rev", "--yes"])
        .code(2)
        .stderr(predicate::str::contains("pass --draft"));
    fs::write(pb.join("lifecycle"), "active").unwrap();
    env.eval(&["rev", "--yes", "--case", "nope"])
        .code(2)
        .stderr(predicate::str::contains("no eval case `nope`"));
    // Without --yes on a non-terminal nothing starts.
    env.eval(&["rev"])
        .code(2)
        .stderr(predicate::str::contains("refused without confirmation"));
    fs::write(
        pb.join("evals/broken.yaml"),
        "schema: 1\nid: other\nfixture: { dir: fixtures/base }\n",
    )
    .unwrap();
    env.eval(&["rev", "--yes"])
        .code(2)
        .stderr(predicate::str::contains("V80"));
    apb()
        .args(["validate", "rev"])
        .current_dir(env.project.path())
        .env("APB_CONFIG_DIR", env.cfg.path())
        .assert()
        .code(1)
        .stdout(predicate::str::contains(
            "does not match the file name `broken.yaml`",
        ));
}

/// The repository's own suite for `branch-quality-review` runs with a stub
/// reviewer: both first cases pass, the planted-defect script finds the
/// line the stub names. That the scripts also fail where they should is
/// the table test below. The playbook's `requires.commands` is cut to the
/// tools every CI runner has (the review tools are the reviewer's business).
#[test]
fn the_branch_quality_review_suite_runs_with_a_stub_reviewer() {
    let repo = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.apb");
    let project = tempfile::tempdir().unwrap();
    let cfg = tempfile::tempdir().unwrap();
    let root = project.path();
    apb_core::registry::init_project(root).unwrap();
    let dst = root.join(".apb/playbooks/branch-quality-review");
    apb_core::fsutil::copy_tree(&repo.join("playbooks/branch-quality-review"), &dst).unwrap();
    apb_core::fsutil::copy_tree(
        &repo.join("profiles/branch-reviewer"),
        &root.join(".apb/profiles/branch-reviewer"),
    )
    .unwrap();
    let current = fs::read_to_string(dst.join("current")).unwrap();
    let yaml_path = dst.join(current.trim()).join("playbook.yaml");
    let original = fs::read_to_string(&yaml_path).unwrap();
    let yaml = original.replace("  - code-ranker\n  - bun\n", "");
    assert_ne!(yaml, original, "the requires.commands cut matched nothing");
    fs::write(&yaml_path, yaml).unwrap();
    // The suite's fixtures carry the files the playbook requires.
    let stub = cfg.path().join("reviewer.sh");
    write(
        &stub,
        "#!/bin/sh\nmkdir -p docs/reviews\nprintf '# Review\\n\\n## Findings\\n\\n1. Low: src/lib.rs:16 skips the last window.\\n' > docs/reviews/_date_time_review.md\nprintf 'ok\\n```yaml\\nstatus: success\\nsummary: ok\\n```\\n'\n",
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&stub, fs::Permissions::from_mode(0o755)).unwrap();
    }
    let out = apb()
        .args([
            "eval",
            "branch-quality-review",
            "--yes",
            "--draft",
            "--json",
        ])
        .current_dir(root)
        .env("APB_CONFIG_DIR", cfg.path())
        .env("APB_AGENT_CMD", &stub)
        .env("APB_NO_REGISTRY", "1")
        .assert()
        .get_output()
        .clone();
    let v: Value = serde_json::from_slice(&out.stdout).unwrap();
    let cases = v["result"]["cases"].as_array().unwrap();
    let summary: Vec<(String, u64)> = cases
        .iter()
        .map(|c| {
            (
                c["case"].as_str().unwrap().to_string(),
                c["passes"].as_u64().unwrap(),
            )
        })
        .collect();
    assert_eq!(
        summary,
        [
            ("review-clean-branch".to_string(), 1),
            ("review-planted-defect".to_string(), 1)
        ],
        "{v:#}"
    );
    assert_eq!(out.status.code(), Some(0));
}

#[test]
fn the_plan_names_nodes_that_load_the_operators_full_environment() {
    let env = setup(PLAYBOOK);
    let note = "node `review` runs profile `x` with `environment: full`";
    env.eval(&["rev", "--dry-run"])
        .success()
        .stderr(predicate::str::contains("environment: full").not());
    write(
        &env.project.path().join(".apb/profiles/x/profile.yaml"),
        "name: x\nexecutor:\n  agent: claude\n  model: claude-haiku-4-5-20251001\nenvironment: full\n",
    );
    env.eval(&["rev", "--dry-run"])
        .success()
        .stderr(predicate::str::contains(note));
    // `--json` automation is told the same, with the not-a-sandbox note.
    let out = env
        .eval(&["rev", "--dry-run", "--json"])
        .success()
        .get_output()
        .stdout
        .clone();
    let v: Value = serde_json::from_slice(&out).unwrap();
    assert_eq!(
        v["full_environment_nodes"],
        serde_json::json!([{ "node": "review", "profile": "x" }])
    );
    assert!(v["note"].as_str().unwrap().contains("not a sandbox"));
}

/// Runs git in `dir` with a fixed identity, panicking on failure.
fn git_in(dir: &Path, args: &[&str]) {
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(dir)
        .args([
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@t.invalid",
            "-c",
            "commit.gpgsign=false",
        ])
        .args(args)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// Runs git in `dir` with `input` on stdin; returns its trimmed stdout.
fn git_out(dir: &Path, args: &[&str], input: &str) -> String {
    use std::io::Write;
    let mut child = std::process::Command::new("git")
        .arg("-C")
        .arg(dir)
        .args([
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@t.invalid",
            "-c",
            "commit.gpgsign=false",
        ])
        .args(args)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(input.as_bytes())
        .unwrap();
    let out = child.wait_with_output().unwrap();
    assert!(out.status.success(), "git {args:?}");
    String::from_utf8(out.stdout).unwrap().trim().to_string()
}

/// A `git:` fixture whose tree plants a symlink out of the scratch tree is
/// refused before apb writes anything through it: the definitions copy
/// would otherwise delete and rewrite `<config>/profiles` through a `.apb`
/// link, and a directory link would take later writes outside.
#[cfg(unix)]
#[test]
fn a_symlinked_fixture_cannot_leave_the_tree() {
    let env = setup(PLAYBOOK);
    let root = env.project.path();
    git_in(root, &["init", "-q", "-b", "main"]);
    write(&root.join("README.md"), "project\n");
    git_in(root, &["add", "README.md"]);
    git_in(root, &["commit", "-q", "-m", "base"]);
    // Each branch is one commit whose tree holds only the planted link,
    // built with plumbing so the project's own `.apb` is not in the way.
    // The scratch tree is <config>/evals/scratch/<eval>/<case>-<n>/tree, so
    // five levels up is the config directory itself.
    for (branch, link, target) in [
        ("evil-apb", ".apb", "../../../../.."),
        ("evil-dir", "docs", "../../../../../profiles"),
        ("evil-abs", "abs", "/tmp"),
    ] {
        let blob = git_out(root, &["hash-object", "-w", "--stdin"], target);
        let tree = git_out(root, &["mktree"], &format!("120000 blob {blob}\t{link}\n"));
        let commit = git_out(root, &["commit-tree", &tree, "-m", branch], "");
        git_in(root, &["branch", branch, &commit]);
    }
    let sentinel = env.cfg.path().join("profiles/keep/profile.yaml");
    write(&sentinel, "name: keep\n");
    let case_path = root.join(".apb/playbooks/rev/evals/writes-report.yaml");
    for branch in ["evil-apb", "evil-dir", "evil-abs"] {
        write(
            &case_path,
            &CASE.replace(
                "  dir: fixtures/base\n  change: fixtures/change\n",
                &format!("  git: {branch}\n"),
            ),
        );
        let (code, v) = env.eval_json(&[]);
        assert_eq!(code, 1, "{branch}: {v:#}");
        let rep = &v["result"]["cases"][0]["repetitions"][0];
        assert_eq!(rep["verdict"], "error", "{branch}: {rep:#}");
        let detail = rep["checks"][0]["detail"].as_str().unwrap();
        assert!(
            detail.starts_with("fixture:") && detail.contains("symlink"),
            "{branch}: {detail}"
        );
        assert_eq!(
            fs::read_to_string(&sentinel).unwrap(),
            "name: keep\n",
            "{branch}"
        );
        assert!(!env.cfg.path().join("playbooks").exists(), "{branch}");
        assert!(!env.cfg.path().join("profiles/x").exists(), "{branch}");
        assert!(!env.cfg.path().join("evals/scratch").exists(), "{branch}");
    }
}

/// The runner's git ignores a `GIT_DIR` the operator's shell carries (as it
/// does inside a git hook): the fixture is committed to the scratch
/// repository, never to the operator's. The scratch repository clears the
/// credential helper and pushes to its local origin, and a case script's
/// own git calls get the hardening through `GIT_CONFIG_PARAMETERS`.
#[test]
fn the_runner_git_ignores_the_operators_git_dir_and_cuts_credentials() {
    let env = setup(PLAYBOOK);
    let other = tempfile::tempdir().unwrap();
    git_in(other.path(), &["init", "-q", "-b", "main"]);
    write(&other.path().join("a"), "a\n");
    git_in(other.path(), &["add", "a"]);
    git_in(other.path(), &["commit", "-q", "-m", "only"]);
    let ev = env.project.path().join(".apb/playbooks/rev/evals");
    write(
        &ev.join("scripts/config.sh"),
        "#!/bin/sh\nset -eu\n[ \"$(git config --get push.default)\" = current ]\n[ \"$(git config --get remote.pushDefault)\" = origin ]\n[ -z \"$(git config --get-all credential.helper)\" ]\n[ -z \"${GIT_DIR:-}\" ]\ncase \"$GIT_CONFIG_PARAMETERS\" in *core.fsmonitor=false*core.hooksPath=*) ;; *) exit 1 ;; esac\n",
    );
    write(
        &ev.join("writes-report.yaml"),
        &CASE.replace(
            "scripts: [scripts/branch.sh]",
            "scripts: [scripts/branch.sh, scripts/config.sh]",
        ),
    );
    let out = apb()
        .args(["eval", "rev", "--yes", "--json"])
        .current_dir(env.project.path())
        .env("APB_CONFIG_DIR", env.cfg.path())
        .env("APB_AGENT_CMD", &env.stub)
        .env("APB_NO_REGISTRY", "1")
        .env("GIT_DIR", other.path().join(".git"))
        .env("GIT_WORK_TREE", other.path())
        .output()
        .unwrap();
    let v: Value = serde_json::from_slice(&out.stdout).unwrap();
    let rep = &v["result"]["cases"][0]["repetitions"][0];
    assert_eq!(rep["verdict"], "passed", "{rep:#}");
    let log = std::process::Command::new("git")
        .args([
            "-C",
            &other.path().to_string_lossy(),
            "rev-list",
            "--count",
            "HEAD",
        ])
        .env_remove("GIT_DIR")
        .output()
        .unwrap();
    assert_eq!(String::from_utf8_lossy(&log.stdout).trim(), "1");
}

/// The case env overlay reaches the agent and the case scripts, with
/// `{{eval.scratch}}` expanded; it is a run setting, not the environment of
/// the `apb run` that starts the run.
#[test]
fn the_env_overlay_reaches_the_agent_and_the_scripts() {
    let env = setup(PLAYBOOK);
    write(
        &env.stub,
        "#!/bin/sh\necho \"agent saw $EVAL_PROBE\" > report.md\nprintf 'REPORT-OK\\n```yaml\\nstatus: success\\nsummary: done\\n```\\n'\n",
    );
    let ev = env.project.path().join(".apb/playbooks/rev/evals");
    write(
        &ev.join("scripts/probe.sh"),
        "#!/bin/sh\nset -eu\n[ \"$(cat report.md)\" = \"agent saw $EVAL_PROBE\" ]\ncase \"$EVAL_PROBE\" in \"v-$APB_EVAL_SCRATCH\") ;; *) exit 1 ;; esac\n",
    );
    write(
        &ev.join("writes-report.yaml"),
        &CASE
            .replace(
                "scripts: [scripts/branch.sh]",
                "scripts: [scripts/probe.sh]",
            )
            .replace("matches: \"names lib.txt\"", "matches: \"agent saw v-/\"")
            .replace(
                "repeat: 1\n",
                "repeat: 1\nenv: { EVAL_PROBE: \"v-{{eval.scratch}}\" }\n",
            ),
    );
    let (code, v) = env.eval_json(&[]);
    assert_eq!(code, 0, "{v:#}");
    let rep = &v["result"]["cases"][0]["repetitions"][0];
    assert_eq!(rep["verdict"], "passed", "{rep:#}");
    let settings = Path::new(rep["run_dir"].as_str().unwrap()).join("run.yaml");
    let cfg = fs::read_to_string(&settings).unwrap_or_default();
    assert!(cfg.contains("EVAL_PROBE"), "a run setting: {cfg}");
}

// --- limits, interruption and cleanup ---------------------------------------

/// Whether process `pid` exists.
#[cfg(unix)]
fn alive(pid: i32) -> bool {
    // SAFETY: kill(pid, 0) only probes.
    unsafe { libc::kill(pid, 0) == 0 }
}

/// Polls `f` every 50 ms for up to `secs` seconds.
fn eventually(secs: u64, mut f: impl FnMut() -> bool) -> bool {
    let end = std::time::Instant::now() + std::time::Duration::from_secs(secs);
    while std::time::Instant::now() < end {
        if f() {
            return true;
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    f()
}

fn read_pid(path: &Path) -> i32 {
    fs::read_to_string(path).unwrap().trim().parse().unwrap()
}

/// A stub agent that records its pid in `$PROBE_DIR/agent.pid`, then runs
/// `tail` (the rest of the script).
fn slow_stub(env: &Env, tail: &str) {
    write(
        &env.stub,
        &format!("#!/bin/sh\necho $$ > \"$PROBE_DIR/agent.pid\"\n{tail}\n"),
    );
}

/// The case with a wall clock and the probe directory in its env.
fn with_limits(env: &Env, timeout: &str) -> PathBuf {
    let probe = env.cfg.path().join("probe");
    fs::create_dir_all(&probe).unwrap();
    write(
        &env.project
            .path()
            .join(".apb/playbooks/rev/evals/writes-report.yaml"),
        &CASE.replace(
            "repeat: 1\n",
            &format!(
                "repeat: 1\nlimits: {{ timeout: {timeout} }}\nenv: {{ PROBE_DIR: \"{}\" }}\n",
                probe.display()
            ),
        ),
    );
    probe
}

/// E3: the wall clock stops a run that never ends; the agent's process is
/// gone afterwards and nothing is kept.
#[cfg(unix)]
#[test]
fn a_run_past_its_timeout_is_stopped_and_its_agent_is_gone() {
    let env = setup(PLAYBOOK);
    let probe = with_limits(&env, "2s");
    slow_stub(&env, "exec sleep 30");
    let (code, v) = env.eval_json(&[]);
    assert_eq!(code, 1, "{v:#}");
    let rep = &v["result"]["cases"][0]["repetitions"][0];
    assert_eq!(rep["verdict"], "incomplete", "{rep:#}");
    assert!(
        rep["stopped"]
            .as_str()
            .unwrap()
            .starts_with("timeout after 2s"),
        "{rep:#}"
    );
    assert!(rep["kept_worktree"].is_null(), "{rep:#}");
    let agent = read_pid(&probe.join("agent.pid"));
    assert!(
        eventually(10, || !alive(agent)),
        "the agent outlived the eval"
    );
    assert!(!env.cfg.path().join("evals/scratch").exists());
}

/// E4: a repetition whose driver will not exit after the stop keeps its
/// tree, moved out of the scratch directory, and the path the result names
/// still exists after `apb eval` returned.
#[cfg(unix)]
#[test]
fn a_kept_tree_survives_the_scratch_cleanup() {
    let env = setup(PLAYBOOK);
    let probe = with_limits(&env, "1s");
    // A detached helper that reads as an apb driver of this run (argv[0]
    // `apb`, its pid in the run's `driver.pid`) for as long as it lives, so
    // the runner sees a driver that does not exit after the stop.
    write(
        &probe.join("holder.sh"),
        "echo $$ > \"$PROBE_DIR/helper.pid\"\nwhile :; do echo $$ > \"$RUN/driver.pid\" 2>/dev/null; sleep 0.2; done\n",
    );
    slow_stub(
        &env,
        "RUN=\"$APB_RUN_DIR\" setsid bash -c 'exec -a apb sh \"$0\"' \"$PROBE_DIR/holder.sh\" >/dev/null 2>&1 &\nexec sleep 45",
    );
    let (code, v) = env.eval_json(&[]);
    let helper = read_pid(&probe.join("helper.pid"));
    let rep = v["result"]["cases"][0]["repetitions"][0].clone();
    // Clean up before asserting: the helper holds the driver.
    // SAFETY: plain kill(2).
    unsafe { libc::kill(helper, libc::SIGKILL) };
    assert_eq!(code, 1, "{v:#}");
    let kept = PathBuf::from(rep["kept_worktree"].as_str().expect("kept_worktree"));
    assert!(
        kept.starts_with(env.cfg.path().join("evals/kept")),
        "{kept:?}"
    );
    assert!(kept.join(".apb").is_dir(), "the kept tree was deleted");
    let run_dir = PathBuf::from(rep["run_dir"].as_str().unwrap());
    assert!(run_dir.starts_with(&kept), "{run_dir:?}");
    assert!(!env.cfg.path().join("evals/scratch").exists());
}

/// E1: a run the gate refuses to start is an `error` repetition with a
/// `start` check, and nothing is left behind.
#[test]
fn a_run_that_cannot_start_is_an_error_and_leaves_nothing() {
    let env = setup(&PLAYBOOK.replace(
        "effects: [fs_read, fs_write]",
        "effects: [fs_read, fs_write]\nrequires: { commands: [apb-eval-definitely-not-installed] }",
    ));
    let (code, v) = env.eval_json(&[]);
    assert_eq!(code, 1, "{v:#}");
    let rep = &v["result"]["cases"][0]["repetitions"][0];
    assert_eq!(rep["verdict"], "error", "{rep:#}");
    assert_eq!(rep["checks"][0]["kind"], "start", "{rep:#}");
    assert!(rep["kept_worktree"].is_null());
    assert!(!env.cfg.path().join("evals/scratch").exists());
    // No run started, so there is no configuration to store it under.
    assert!(v["stored"].is_null(), "{v:#}");
    assert!(!env.cfg.path().join("evals/results").exists());
}

/// E2: a fixture ref that does not resolve is an `error` repetition named
/// `fixture:`, and nothing is left behind.
#[test]
fn a_fixture_that_cannot_materialize_is_an_error_and_leaves_nothing() {
    let env = setup(PLAYBOOK);
    write(
        &env.project
            .path()
            .join(".apb/playbooks/rev/evals/writes-report.yaml"),
        &CASE.replace(
            "  dir: fixtures/base\n  change: fixtures/change\n",
            "  git: no-such-ref\n",
        ),
    );
    let (code, v) = env.eval_json(&[]);
    assert_eq!(code, 1, "{v:#}");
    let rep = &v["result"]["cases"][0]["repetitions"][0];
    assert_eq!(rep["verdict"], "error", "{rep:#}");
    assert!(
        rep["checks"][0]["detail"]
            .as_str()
            .unwrap()
            .starts_with("fixture:"),
        "{rep:#}"
    );
    assert!(!env.cfg.path().join("evals/scratch").exists());
}

/// The engine enforces an eval run's deadline itself: a run started with
/// one is aborted by its own driver, with nobody following it.
#[cfg(unix)]
#[test]
fn the_driver_aborts_an_eval_run_at_its_deadline() {
    let env = setup(PLAYBOOK);
    let probe = env.cfg.path().join("probe");
    fs::create_dir_all(&probe).unwrap();
    slow_stub(&env, "exec sleep 30");
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64;
    let settings = env.cfg.path().join("settings.json");
    write(
        &settings,
        &serde_json::json!({
            "spawn_env": { "PROBE_DIR": probe },
            "deadline_ms": now + 1500,
        })
        .to_string(),
    );
    let out = apb()
        .args(["run", "rev", "--detach", "--eval-settings"])
        .arg(&settings)
        .current_dir(env.project.path())
        .env("APB_CONFIG_DIR", env.cfg.path())
        .env("APB_AGENT_CMD", &env.stub)
        .env("APB_NO_REGISTRY", "1")
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    let run_id = stdout
        .lines()
        .find_map(|l| l.strip_prefix("run started: "))
        .unwrap_or_else(|| panic!("{stdout}{}", String::from_utf8_lossy(&out.stderr)))
        .split_whitespace()
        .next()
        .unwrap()
        .to_string();
    apb()
        .args(["wait", &run_id, "--timeout", "30"])
        .current_dir(env.project.path())
        .env("APB_CONFIG_DIR", env.cfg.path())
        .assert()
        .code(1);
    let journal = fs::read_to_string(
        env.project
            .path()
            .join(".apb/runs")
            .join(&run_id)
            .join("events.jsonl"),
    )
    .unwrap();
    assert!(
        journal.contains(apb_engine::scheduler::DEADLINE_REASON),
        "{journal}"
    );
    let agent = read_pid(&probe.join("agent.pid"));
    assert!(
        eventually(10, || !alive(agent)),
        "the agent outlived the deadline"
    );
}

/// SIGINT stops the live repetition's run, waits for its driver and
/// removes the scratch directory; the process ends with 130.
#[cfg(unix)]
#[test]
fn an_interrupted_eval_stops_its_run_and_cleans_up() {
    let env = setup(PLAYBOOK);
    let probe = with_limits(&env, "60s");
    slow_stub(&env, "exec sleep 30");
    let mut child = crate::common::apb_std()
        .args(["eval", "rev", "--yes", "--json"])
        .current_dir(env.project.path())
        .env("APB_CONFIG_DIR", env.cfg.path())
        .env("APB_AGENT_CMD", &env.stub)
        .env("APB_NO_REGISTRY", "1")
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
        .unwrap();
    assert!(
        eventually(20, || probe.join("agent.pid").is_file()),
        "the agent never started"
    );
    let agent = read_pid(&probe.join("agent.pid"));
    // SAFETY: plain kill(2) on our own child.
    unsafe { libc::kill(child.id() as i32, libc::SIGINT) };
    let finished = eventually(40, || child.try_wait().unwrap().is_some());
    if !finished {
        let _ = child.kill();
    }
    let out = child.wait_with_output().unwrap();
    assert!(finished, "apb eval did not exit after SIGINT");
    assert_eq!(out.status.code(), Some(130));
    let v: Value = serde_json::from_slice(&out.stdout).unwrap();
    let rep = &v["result"]["cases"][0]["repetitions"][0];
    assert!(
        rep["stopped"].as_str().unwrap().starts_with("interrupted"),
        "{rep:#}"
    );
    assert!(
        eventually(10, || !alive(agent)),
        "the agent outlived the eval"
    );
    assert!(!env.cfg.path().join("evals/scratch").exists());
}

/// The scratch of an earlier invocation that died is swept at the next
/// start; one whose owner still runs is left alone.
#[cfg(unix)]
#[test]
fn stale_scratch_directories_are_swept_at_start() {
    let env = setup(PLAYBOOK);
    let scratch = env.cfg.path().join("evals/scratch");
    let dead = std::process::Command::new("true").spawn().unwrap();
    let dead_pid = dead.id();
    let _ = dead.wait_with_output();
    write(&scratch.join("eval-1/owner.pid"), &dead_pid.to_string());
    write(&scratch.join("eval-1/writes-report-1/tree/x"), "x");
    write(
        &scratch.join("eval-2/owner.pid"),
        &std::process::id().to_string(),
    );
    let (code, v) = env.eval_json(&[]);
    assert_eq!(code, 0, "{v:#}");
    assert!(!scratch.join("eval-1").exists(), "the stale scratch stayed");
    assert!(
        scratch.join("eval-2").exists(),
        "a live owner's scratch went"
    );
}

// --- stored results and comparisons ------------------------------------------

/// E5: the comparison names what moved between configurations (a profile
/// edit, then a playbook edit), and `--compare` says when there is nothing
/// or only one result to compare.
#[test]
fn compare_names_profile_and_playbook_changes() {
    let env = setup(PLAYBOOK);
    env.eval(&["rev", "--compare", "--json"])
        .code(2)
        .stdout(predicate::str::contains("\"no_results\""));
    let (_, _) = env.eval_json(&[]);
    env.eval(&["rev", "--compare"])
        .success()
        .stdout(predicate::str::contains("nothing to compare"));
    write(
        &env.project.path().join(".apb/profiles/x/profile.yaml"),
        "name: x\nexecutor:\n  agent: claude\n  model: claude-sonnet-4-5\n",
    );
    let (_, v) = env.eval_json(&[]);
    let changes = v["comparison"]["configuration_changes"].to_string();
    assert!(changes.contains("profile bundle project/x:"), "{changes}");
    let pb = env
        .project
        .path()
        .join(".apb/playbooks/rev/1.0.0/playbook.yaml");
    write(&pb, &PLAYBOOK.replace("name: rev\n", "name: rev renamed\n"));
    let (code, v) = env.eval_json(&[]);
    assert_eq!(code, 0, "{v:#}");
    let out = env
        .eval(&["rev", "--compare", "--json"])
        .success()
        .get_output()
        .stdout
        .clone();
    let c: Value = serde_json::from_slice(&out).unwrap();
    assert_eq!(c["latest"], v["result"]["eval_id"]);
    let changes = c["comparison"]["configuration_changes"].to_string();
    assert!(changes.contains("playbook digest"), "{changes}");
    env.eval(&["../rev", "--compare"])
        .code(2)
        .stderr(predicate::str::contains("is not a playbook id"));
}

/// E6: an invocation in which no repetition started is not stored, so the
/// next comparison sees the same configuration as the last real one.
#[test]
fn an_invocation_that_started_nothing_adds_no_configuration_change() {
    let env = setup(PLAYBOOK);
    let (code, _) = env.eval_json(&[]);
    assert_eq!(code, 0);
    let pb = env
        .project
        .path()
        .join(".apb/playbooks/rev/1.0.0/playbook.yaml");
    write(
        &pb,
        &PLAYBOOK.replace(
            "effects: [fs_read, fs_write]",
            "effects: [fs_read, fs_write]\nrequires: { commands: [apb-eval-definitely-not-installed] }",
        ),
    );
    let (code, v) = env.eval_json(&[]);
    assert_eq!(code, 1, "{v:#}");
    assert!(v["comparison"].is_null(), "{v:#}");
    write(&pb, PLAYBOOK);
    let (code, v) = env.eval_json(&[]);
    assert_eq!(code, 0, "{v:#}");
    let c = &v["comparison"];
    assert_eq!(c["same_configuration"], true, "{c:#}");
    assert_eq!(c["configuration_changes"], serde_json::json!([]), "{c:#}");
}

/// V1: V82 is a warning in `apb validate`: the playbook still validates.
#[test]
fn v82_is_a_warning_in_apb_validate() {
    let gated = PLAYBOOK
        .replace(
            "  - { id: done, type: finish, outcome: success }",
            "  - { id: gate, type: human_review }\n  - { id: done, type: finish, outcome: success }",
        )
        .replace(
            "{ from: review, to: done, condition",
            "{ from: review, to: gate, condition",
        )
        .replace("edges:\n", "edges:\n  - { from: gate, to: done }\n");
    let env = setup(&gated);
    apb()
        .args(["validate", "rev"])
        .current_dir(env.project.path())
        .env("APB_CONFIG_DIR", env.cfg.path())
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "rev: warning V82 node(s) `gate` wait for a person",
        ))
        .stdout(predicate::str::contains("rev: OK"));
}

/// E7: the negative controls of the repository's review scripts. Each
/// script runs with `sh` in a scratch git tree over a written report and
/// must both pass and fail on the inputs its comment promises.
#[test]
fn the_review_scripts_pass_and_fail_on_their_documented_inputs() {
    let scripts = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../.apb/playbooks/branch-quality-review/evals/scripts");
    let run = |script: &str, report: Option<&str>, edit_source: bool| -> bool {
        let t = tempfile::tempdir().unwrap();
        git_in(t.path(), &["init", "-q", "-b", "main"]);
        write(&t.path().join("src/lib.rs"), "fn x() {}\n");
        git_in(t.path(), &["add", "-A"]);
        git_in(t.path(), &["commit", "-q", "-m", "fixture"]);
        if let Some(r) = report {
            write(&t.path().join("docs/reviews/_date_time_review.md"), r);
        }
        if edit_source {
            write(&t.path().join("src/lib.rs"), "fn y() {}\n");
        }
        std::process::Command::new("sh")
            .arg(scripts.join(script))
            .current_dir(t.path())
            .output()
            .unwrap()
            .status
            .success()
    };
    let planted = "names-the-planted-defect.sh";
    for (report, pass) in [
        ("1. Low: src/lib.rs:16 skips the last window.", true),
        ("src/lib.rs#L15: the loop bound", true),
        ("In src/lib.rs, lines 14-18 skip the last window.", true),
        ("**Line:** 17 of src/lib.rs", true),
        ("src/lib.rs:30 is fine", false),
        ("src/lib.rs has a problem somewhere", false),
        ("the loop on line 16 is wrong", false),
    ] {
        assert_eq!(
            run(planted, Some(report), false),
            pass,
            "{planted}: {report}"
        );
    }
    let severe = "no-severe-finding.sh";
    for (report, pass) in [
        (
            "# Review\n\n## High-level summary\n\n1. Low: src/lib.rs:16 nit.\n",
            true,
        ),
        (
            "## Findings\n\n- Low: naming\n\nThe high cost of this is low.\n",
            true,
        ),
        ("## High\n\n- src/lib.rs:16\n", false),
        ("### Critical findings\n", false),
        ("- **[P1]** src/lib.rs:16 skips a window\n", false),
        ("- src/lib.rs:16, severity: critical\n", false),
        ("1. High: src/lib.rs:16 skips a window\n", false),
        ("- [major] the loop bound\n", false),
    ] {
        assert_eq!(
            run(severe, Some(report), false),
            pass,
            "{severe}: {report:?}"
        );
    }
    let only = "only-the-review-file.sh";
    assert!(
        run(only, Some("review\n"), false),
        "{only}: only the report"
    );
    assert!(
        !run(only, Some("review\n"), true),
        "{only}: an edited source"
    );
    assert!(!run(only, None, false), "{only}: no report");
}

/// E8: the invocation budget counts the cost the agent reports; once it is
/// spent the next repetition does not start.
#[test]
fn a_spent_invocation_budget_stops_the_next_repetition() {
    let env = setup(PLAYBOOK);
    // claude's `--output-format json` result object, with a cost.
    write(
        &env.stub,
        "#!/bin/sh\necho 'the review names lib.txt' > report.md\nprintf '%s\\n' '{\"type\":\"result\",\"is_error\":false,\"result\":\"REPORT-OK\\n```yaml\\nstatus: success\\nsummary: done\\n```\",\"total_cost_usd\":0.6,\"usage\":{\"input_tokens\":10,\"output_tokens\":5}}'\n",
    );
    let (code, v) = env.eval_json(&["--repeat", "3", "--max-usd", "1"]);
    assert_eq!(code, 1, "{v:#}");
    let case = &v["result"]["cases"][0];
    assert_eq!(case["of"], 2, "{v:#}");
    assert_eq!(case["repetitions"][0]["usage"]["cost_usd"], 0.6, "{v:#}");
    assert_eq!(v["result"]["incomplete"], "invocation budget $1.00 spent");
    assert_eq!(v["result"]["total_cost_usd"], 1.2);
    assert_eq!(
        v["warnings"],
        serde_json::json!([]),
        "the cost was reported"
    );
}
