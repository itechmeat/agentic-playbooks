//! `apb scenario capture` through the real binary: the preview and the
//! JSON form, the `--save --yes` write, the non-terminal refusal, and the
//! replay of four captured runs (a rejected attempt, declared output
//! fields, a review, a timed-out attempt) at quick level.

use std::fs;
use std::os::unix::fs::PermissionsExt as _;
use std::path::Path;

use serde_json::Value;

use crate::common::apb;

const PLAYBOOK_BASE: &str = "schema: 2\nid: cap\nname: Cap\nversion: 1.0.0\ndefaults: { profile: main }\nnodes:\n  - { id: start, type: start }\n  - { id: work, type: agent_task, prompt: \"Fix {{run.instruction}}\", max_retries: 1{work} }\n{review}{report}  - { id: done, type: finish, outcome: success }\n  - { id: failed, type: finish, outcome: failure }\nedges:\n  - { from: start, to: work }\n  - { from: work, to: failed, condition: { type: node_status, node: work, equals: failure } }\n{review_edge}{report_edge}  - { from: {last}, to: done, condition: { type: node_status, node: {last}, equals: success } }\n";

const REVIEW_NODE: &str =
    "  - { id: approve, type: human_review, unattended: { decide: approve } }\n";
const REVIEW_EDGE: &str = "  - { from: work, to: approve, condition: { type: node_status, node: work, equals: success } }\n";
const REPORT_NODE: &str =
    "  - { id: report, type: agent_task, prompt: \"Branch is {{nodes.work.output.branch}}\" }\n";
const REPORT_EDGE: &str = "  - { from: {prev}, to: report, condition: { type: node_status, node: work, equals: success } }\n";

/// The stub agent: a fixed success unless the mode says otherwise. The
/// first-call markers make the reject and timeout variants two-attempt
/// runs.
fn stub(root: &Path, mode: &str) -> String {
    let body = match mode {
        "reject" => {
            "#!/bin/sh\nif [ -f \"$CAP_DIR/tried\" ]; then printf '{\"status\":\"success\"}' > \"$APB_STATUS_FILE\"; echo 'ALL-DONE, fixed the parser'; else touch \"$CAP_DIR/tried\"; printf '{\"status\":\"success\"}' > \"$APB_STATUS_FILE\"; echo 'not really done'; fi\n"
        }
        "timeout" => {
            "#!/bin/sh\nif [ -f \"$CAP_DIR/tried\" ]; then printf '{\"status\":\"success\"}' > \"$APB_STATUS_FILE\"; echo 'ALL-DONE, fixed the parser'; else touch \"$CAP_DIR/tried\"; sleep 5; fi\n"
        }
        "fields" => {
            "#!/bin/sh\nprintf '{\"status\":\"success\"}' > \"$APB_STATUS_FILE\"; echo '{\"branch\": \"feat/x\", \"pushed\": \"no\"}'\n"
        }
        _ => {
            "#!/bin/sh\nprintf '{\"status\":\"success\"}' > \"$APB_STATUS_FILE\"; echo 'ALL-DONE, fixed the parser'\n"
        }
    };
    let path = root.join(format!("agent-{mode}.sh"));
    fs::write(&path, body).unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
    path.to_string_lossy().into_owned()
}

struct Project {
    dir: tempfile::TempDir,
}

impl Project {
    /// A project with `cap` installed as 1.0.0 and current, profile main,
    /// and the mode's playbook variant.
    fn new(work: &str, report: bool, review: bool) -> Project {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        apb().arg("init").current_dir(root).assert().success();
        let v = root.join(".apb/playbooks/cap/1.0.0");
        fs::create_dir_all(&v).unwrap();
        let last = if report {
            "report"
        } else if review {
            "approve"
        } else {
            "work"
        };
        let prev = if review { "approve" } else { "work" };
        let report_edge = if report {
            REPORT_EDGE.replace("{prev}", prev)
        } else {
            String::new()
        };
        let review_edge = if review { REVIEW_EDGE } else { "" };
        let body = PLAYBOOK_BASE
            .replace("{work}", work)
            .replace("{report}", if report { REPORT_NODE } else { "" })
            .replace("{review}", if review { REVIEW_NODE } else { "" })
            .replace("{report_edge}", &report_edge)
            .replace("{review_edge}", review_edge)
            .replace("{review_edge}", if review { REVIEW_EDGE } else { "" })
            .replace("{last}", last);
        fs::write(v.join("playbook.yaml"), body).unwrap();
        fs::write(root.join(".apb/playbooks/cap/current"), "1.0.0").unwrap();
        let pdir = root.join(".apb/profiles/main");
        fs::create_dir_all(&pdir).unwrap();
        // A fallback on another model: a real timeout breaks to the
        // fallback chain, and the timed-out variant's second attempt runs
        // there.
        fs::write(
            pdir.join("profile.yaml"),
            "name: main\ndescription: test\nexecutor:\n  agent: claude-code\n  model: haiku\n  fallbacks:\n    - { agent: claude-code, model: haiku-fb }\n",
        )
        .unwrap();
        fs::write(pdir.join("SOUL.md"), "").unwrap();
        Project { dir }
    }

    fn root(&self) -> &Path {
        self.dir.path()
    }

    /// One real run of `cap` with the mode's stub; returns the run id.
    fn run(&self, mode: &str) -> String {
        let agent = stub(self.root(), mode);
        if mode == "review" {
            return self.reviewed_run(&agent);
        }
        let out = apb()
            .args(["run", "cap", "--instruction", "the parser"])
            .env("APB_AGENT_CMD", &agent)
            .env("CAP_DIR", self.root())
            .current_dir(self.root())
            .timeout(std::time::Duration::from_secs(120))
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "the run failed: {} {}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        let runs: Value = serde_json::from_str(&String::from_utf8_lossy(
            &apb()
                .args(["runs", "--json", "--playbook", "cap"])
                .current_dir(self.root())
                .output()
                .unwrap()
                .stdout,
        ))
        .unwrap();
        let list = runs
            .as_array()
            .cloned()
            .unwrap_or_else(|| runs["runs"].as_array().cloned().unwrap_or_default());
        list.last()
            .and_then(|r| r["run_id"].as_str().or_else(|| r["id"].as_str()))
            .expect("a run id in the listing")
            .to_string()
    }

    /// A run whose review a person decides through the CLI: the run goes
    /// in the background, the test polls for the request and answers.
    fn reviewed_run(&self, agent: &str) -> String {
        let mut child = std::process::Command::new(crate::common::apb_bin())
            .args(["run", "cap", "--instruction", "the parser"])
            .env("APB_CONFIG_DIR", crate::common::sandbox_config_dir())
            .env("APB_AGENT_CMD", agent)
            .env("CAP_DIR", self.root())
            .current_dir(self.root())
            .spawn()
            .expect("the run started");
        let started = std::time::Instant::now();
        let run_id = loop {
            assert!(
                started.elapsed() < std::time::Duration::from_secs(90),
                "the run did not ask for a review"
            );
            if let Ok(runs) = serde_json::from_str::<Value>(&String::from_utf8_lossy(
                &apb()
                    .args(["runs", "--json", "--playbook", "cap"])
                    .current_dir(self.root())
                    .output()
                    .unwrap()
                    .stdout,
            )) {
                let list = runs
                    .as_array()
                    .cloned()
                    .unwrap_or_else(|| runs["runs"].as_array().cloned().unwrap_or_default());
                if let Some(id) = list
                    .last()
                    .and_then(|r| r["run_id"].as_str().or_else(|| r["id"].as_str()))
                {
                    break id.to_string();
                }
            }
            std::thread::sleep(std::time::Duration::from_millis(200));
        };
        loop {
            assert!(
                started.elapsed() < std::time::Duration::from_secs(90),
                "the review request did not land"
            );
            let asked = apb()
                .args(["runs", &run_id, "--json"])
                .current_dir(self.root())
                .output()
                .unwrap();
            if String::from_utf8_lossy(&asked.stdout).contains("pending_review") {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(200));
        }
        let decided = apb()
            .args(["review", &run_id, "approve", "--decision", "approve"])
            .current_dir(self.root())
            .output()
            .unwrap();
        assert!(
            decided.status.success(),
            "the review decision failed: {}",
            String::from_utf8_lossy(&decided.stderr)
        );
        let status = child.wait().expect("the run ended");
        if !status.success() {
            let why = apb()
                .args(["runs", &run_id])
                .current_dir(self.root())
                .output()
                .unwrap();
            panic!(
                "the reviewed run failed: {}",
                String::from_utf8_lossy(&why.stdout)
            );
        }
        run_id
    }

    /// `apb scenario capture <run> --name <slug> <extra>`; the stdout.
    fn capture(&self, run: &str, slug: &str, extra: &[&str]) -> (i32, String, String) {
        let out = apb()
            .args(["scenario", "capture", run, "--name", slug])
            .args(extra)
            .current_dir(self.root())
            .env_remove("APB_RUN_ID")
            .output()
            .unwrap();
        (
            out.status.code().unwrap_or(-1),
            String::from_utf8_lossy(&out.stdout).to_string(),
            String::from_utf8_lossy(&out.stderr).to_string(),
        )
    }

    /// The quick replay of the saved scenario `slug`; (exit, report).
    fn replay(&self, slug: &str) -> (i32, Value) {
        let out = apb()
            .args([
                "eval",
                "cap",
                "--level",
                "quick",
                "--scenario",
                slug,
                "--json",
            ])
            .current_dir(self.root())
            .output()
            .unwrap();
        let v: Value = serde_json::from_slice(&out.stdout).unwrap_or(Value::Null);
        (out.status.code().unwrap_or(-1), v["reports"][0].clone())
    }
}

/// The preview prints the YAML, the digest line, and the same shape as
/// JSON; nothing is written.
#[test]
fn the_preview_prints_and_writes_nothing() {
    let p = Project::new("", false, false);
    let run = p.run("plain");
    let (code, out, err) = p.capture(&run, "green-after-one-fix", &[]);
    assert_eq!(code, 0, "{out}{err}");
    assert!(out.contains("id: green-after-one-fix"), "{out}");
    assert!(out.contains("digest: sha256:"), "{out}");
    assert!(
        !p.root()
            .join(".apb/playbooks/cap/evals/scenarios/green-after-one-fix.yaml")
            .exists()
    );
    let (code, out, err) = p.capture(&run, "green-after-one-fix", &["--json"]);
    assert_eq!(code, 0, "{err}");
    let v: Value = serde_json::from_str(&out).unwrap();
    assert_eq!(v["playbook"], "cap");
    assert_eq!(v["name"], "green-after-one-fix");
    assert!(v["yaml"].as_str().unwrap().contains("green-after-one-fix"));
    assert!(v["digest"].as_str().unwrap().starts_with("sha256:"));
    assert!(v["saved"].is_null());
}

/// `--save --yes` writes the file; a second save is refused.
#[test]
fn a_save_with_yes_writes_once() {
    let p = Project::new("", false, false);
    let run = p.run("plain");
    let (code, out, err) = p.capture(&run, "green-after-one-fix", &["--save", "--yes"]);
    assert_eq!(code, 0, "{out}{err}");
    let path = p
        .root()
        .join(".apb/playbooks/cap/evals/scenarios/green-after-one-fix.yaml");
    assert!(path.is_file(), "{out}{err}");
    let (code, _, err) = p.capture(&run, "green-after-one-fix", &["--save", "--yes"]);
    assert_eq!(code, 2, "{err}");
    assert!(err.contains("scenario_exists"), "{err}");
}

/// `--save` at a non-terminal without `--yes` is exit 2 and writes
/// nothing.
#[test]
fn a_save_without_a_terminal_needs_yes() {
    let p = Project::new("", false, false);
    let run = p.run("plain");
    let (code, out, err) = p.capture(&run, "green-after-one-fix", &["--save"]);
    assert_eq!(code, 2, "{out}{err}");
    assert!(!p.root().join(".apb/playbooks/cap/evals/scenarios").exists());
}

/// A run whose success check rejected attempt 1 replays: the same node
/// status and visit count, and attempt 1's answer carries `check: reject`.
#[test]
fn a_rejected_attempt_run_replays() {
    let p = Project::new(", success_check: { marker: \"ALL-DONE\" }", false, false);
    let run = p.run("reject");
    let (code, out, err) = p.capture(&run, "rejected-once", &["--save", "--yes"]);
    assert_eq!(code, 0, "{out}{err}");
    let yaml = fs::read_to_string(
        p.root()
            .join(".apb/playbooks/cap/evals/scenarios/rejected-once.yaml"),
    )
    .unwrap();
    assert!(yaml.contains("check: reject"), "{yaml}");
    let (code, r) = p.replay("rejected-once");
    assert_eq!((code, r["verdict"].as_str()), (0, Some("passed")), "{r:#}");
}

/// A run whose output is a JSON object replays through `outputs`, and the
/// later node's template renders the same field value (no empty template).
#[test]
fn a_declared_fields_run_replays() {
    let p = Project::new("", true, false);
    let run = p.run("fields");
    let (code, out, err) = p.capture(&run, "fields-run", &["--save", "--yes"]);
    assert_eq!(code, 0, "{out}{err}");
    let yaml = fs::read_to_string(
        p.root()
            .join(".apb/playbooks/cap/evals/scenarios/fields-run.yaml"),
    )
    .unwrap();
    assert!(yaml.contains("branch:"), "{yaml}");
    assert!(yaml.contains("feat/x"), "{yaml}");
    let (code, r) = p.replay("fields-run");
    assert_eq!((code, r["verdict"].as_str()), (0, Some("passed")), "{r:#}");
    assert_eq!(
        r["templates_empty"].as_array().map(Vec::len),
        Some(0),
        "the later template rendered empty: {r:#}"
    );
}

/// A review run replays with its decision; a timed-out attempt replays as
/// a timeout without waiting.
#[test]
fn a_review_and_a_timed_out_run_replay() {
    let p = Project::new("", false, true);
    let run = p.run("review");
    let (code, out, err) = p.capture(&run, "reviewed", &["--save", "--yes"]);
    assert_eq!(code, 0, "{out}{err}");
    let (code, r) = p.replay("reviewed");
    assert_eq!((code, r["verdict"].as_str()), (0, Some("passed")), "{r:#}");

    let p = Project::new(", timeout_seconds: 1", false, false);
    let run = p.run("timeout");
    let (code, out, err) = p.capture(&run, "timed-out", &["--save", "--yes"]);
    assert_eq!(code, 0, "{out}{err}");
    let yaml = fs::read_to_string(
        p.root()
            .join(".apb/playbooks/cap/evals/scenarios/timed-out.yaml"),
    )
    .unwrap();
    assert!(yaml.contains("timed_out: true"), "{yaml}");
    let started = std::time::Instant::now();
    let (code, r) = p.replay("timed-out");
    assert_eq!((code, r["verdict"].as_str()), (0, Some("passed")), "{r:#}");
    assert!(
        started.elapsed() < std::time::Duration::from_secs(30),
        "the replay waited for the node timeout"
    );
}
