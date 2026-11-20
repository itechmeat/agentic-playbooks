//! The CLI run surfaces never print a run's secret value (design: secrets
//! of a run), on stdout or stderr: `apb run`, `apb resume`, `apb runs`,
//! `apb runs <id>` in text and JSON, `apb wait` and the HTML report, for a
//! run and its child. A host-mode run's task never carries the value: the
//! host reads it from the run's `run.yaml`. A resume of a run that renders a
//! secret param is refused before any consent is recorded.

use std::fs;
use std::path::Path;
use std::time::{Duration, Instant};

use serde_json::Value;

use crate::secret_runs::{SECRET, seed_secret_runs};

fn holds(text: &str) -> bool {
    text.contains(SECRET)
}

/// The stdout of `apb <args>` in `root` (any exit code: `apb wait` on a
/// failed run exits 1); its stderr must not hold the value either.
fn stdout_of(root: &Path, args: &[&str]) -> String {
    let (stdout, stderr) = streams_of(root, args);
    assert!(!holds(&stderr), "apb {args:?} leaked the value on stderr");
    stdout
}

/// The stdout and stderr of `apb <args>` in `root`, any exit code.
fn streams_of(root: &Path, args: &[&str]) -> (String, String) {
    let out = crate::common::apb_std()
        .args(args)
        .current_dir(root)
        .output()
        .unwrap();
    (
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

/// `apb run` and `apb resume` drive the run in the foreground and print
/// its progress and outcome: neither stream holds the value. A declared
/// secret value too short to scrub from text gets a warning at start, which
/// names the param and never the value.
#[test]
fn apb_run_and_resume_print_no_value_and_warn_about_a_short_one() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    // The fixture installs the playbooks and leaves one finished run.
    seed_secret_runs(root);
    let param = format!("message={SECRET}");
    let (stdout, stderr) = streams_of(root, &["run", "secretpb", "--param", &param]);
    assert!(!holds(&stdout), "apb run leaked the value on stdout");
    assert!(!holds(&stderr), "apb run leaked the value on stderr");
    assert!(!stderr.contains("shorter than"), "{stderr}");
    let run_id = stdout
        .lines()
        .find_map(|l| l.strip_prefix("run ")?.split_whitespace().next())
        .unwrap_or_else(|| panic!("a run id: {stdout}"))
        .to_string();
    assert!(run_id.starts_with("secretpb-"), "{stdout}");
    // From the node that echoes the value again.
    let (stdout, stderr) = streams_of(root, &["resume", &run_id, "--from-node", "echo"]);
    assert!(
        stdout.contains(&run_id) || stderr.contains(&run_id),
        "{stdout}{stderr}"
    );
    assert!(!holds(&stdout), "apb resume leaked the value on stdout");
    assert!(!holds(&stderr), "apb resume leaked the value on stderr");

    let (_, stderr) = streams_of(root, &["run", "secretpb", "--param", "message=q7z"]);
    assert!(
        stderr.contains("warning:")
            && stderr.contains("message")
            && stderr.contains("shorter than 6"),
        "{stderr}"
    );
    assert!(
        !stderr.contains("q7z"),
        "the warning names the param only: {stderr}"
    );
}

#[test]
fn no_cli_run_surface_prints_the_value() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let (parent, child) = seed_secret_runs(root);
    let list = stdout_of(root, &["runs"]);
    assert!(list.contains(&parent), "apb runs must list the run");
    assert!(!holds(&list), "apb runs leaked the value");
    for run in [&parent, &child] {
        let text = stdout_of(root, &["runs", run]);
        assert!(
            text.contains(run.as_str()),
            "apb runs <id> must print the run"
        );
        assert!(!holds(&text), "apb runs <id> leaked the value");
        let json = stdout_of(root, &["runs", run, "--json"]);
        assert!(
            serde_json::from_str::<Value>(&json).is_ok(),
            "apb runs <id> --json must print JSON"
        );
        assert!(!holds(&json), "apb runs <id> --json leaked the value");
        let waited = stdout_of(root, &["wait", run, "--timeout", "5"]);
        assert!(!holds(&waited), "apb wait leaked the value");
        let html = stdout_of(root, &["runs", run, "--html", "-"]);
        assert!(
            html.contains("<html"),
            "apb runs <id> --html - must print the page"
        );
        assert!(!holds(&html), "apb runs <id> --html leaked the value");
    }
    // Redact on read: the run's own files keep what the executor produced.
    let journal =
        fs::read_to_string(root.join(".apb/runs").join(&parent).join("events.jsonl")).unwrap();
    assert!(holds(&journal), "the journal must keep the echo");
}

const HOST_PLAYBOOK: &str = r#"
schema: 1
id: hm
name: Host mode
version: 1.0.0
defaults:
  profile: main
params:
  - { name: message, type: text, secret: true }
nodes:
  - { id: start, type: start }
  - { id: echo, type: agent_task, prompt: "Repeat the `message` param. Read it from run.yaml in $APB_RUN_DIR." }
  - { id: done, type: finish, outcome: success }
edges:
  - { from: start, to: echo }
  - { from: echo, to: done }
"#;

pub(crate) fn seed_host(root: &Path) {
    crate::common::apb_std()
        .arg("init")
        .current_dir(root)
        .output()
        .unwrap();
    let v = root.join(".apb/playbooks/hm/1.0.0");
    fs::create_dir_all(&v).unwrap();
    fs::write(v.join("playbook.yaml"), HOST_PLAYBOOK).unwrap();
    fs::write(root.join(".apb/playbooks/hm/current"), "1.0.0").unwrap();
    let p = root.join(".apb/profiles/main");
    fs::create_dir_all(&p).unwrap();
    fs::write(
        p.join("profile.yaml"),
        "name: main\ndescription: d\nexecutor:\n  agent: claude-code\n  model: haiku\n",
    )
    .unwrap();
    fs::write(p.join("SOUL.md"), "You repeat.").unwrap();
}

pub(crate) fn poll<T>(what: &str, mut f: impl FnMut() -> Option<T>) -> T {
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        if let Some(v) = f() {
            return v;
        }
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// `value` without its top-level `pending_tasks`, the executor's delivery.
fn without_delivery(mut value: Value) -> Value {
    if let Some(map) = value.as_object_mut() {
        map.remove("pending_tasks");
    }
    value
}

#[test]
fn a_host_task_never_carries_the_value_and_no_surface_shows_its_reply() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    seed_host(root);
    let out = crate::common::apb_std()
        .args(["run", "hm", "--execution", "host", "--detach"])
        .arg("--param")
        .arg(format!("message={SECRET}"))
        .current_dir(root)
        .output()
        .unwrap();
    assert!(out.status.success(), "the host-mode run must start");
    let run_id = String::from_utf8_lossy(&out.stdout)
        .lines()
        .find_map(|l| l.strip_prefix("run started: "))
        .expect("run id")
        .trim()
        .to_string();

    // No delivery: the prompt the host executes names where the value is,
    // and the run's `run.yaml` holds it.
    let task = poll("the host task", || {
        let json = stdout_of(root, &["tasks", &run_id, "--json"]);
        serde_json::from_str::<Vec<Value>>(&json)
            .ok()?
            .into_iter()
            .find(|t| t["node"] == "echo")
    });
    let prompt = task["prompt"].as_str().unwrap_or_default();
    assert!(!holds(prompt), "the host task prompt carries the value");
    assert!(prompt.contains("run.yaml"), "{prompt}");
    let status: Value =
        serde_json::from_str(&stdout_of(root, &["runs", &run_id, "--json"])).unwrap();
    assert!(
        !status["pending_tasks"].to_string().contains(SECRET),
        "pending_tasks carries the value"
    );
    assert!(!holds(&without_delivery(status).to_string()));
    let config: yaml_serde::Value = yaml_serde::from_str(
        &fs::read_to_string(root.join(".apb/runs").join(&run_id).join("run.yaml")).unwrap(),
    )
    .unwrap();
    assert_eq!(
        config["params"]["message"].as_str(),
        Some(SECRET),
        "the run's run.yaml holds the value"
    );

    // The host's reply echoes the value; once the run ends nothing shows it.
    let reply = root.join("reply.md");
    fs::write(&reply, format!("I repeat: {SECRET}\n")).unwrap();
    let task_id = task["task_id"].as_str().unwrap().to_string();
    let out = crate::common::apb_std()
        .args([
            "tasks",
            "submit",
            &run_id,
            &task_id,
            "--status",
            "succeeded",
        ])
        .arg("--output-file")
        .arg(&reply)
        .current_dir(root)
        .output()
        .unwrap();
    assert!(out.status.success(), "the reply must be accepted");
    let waited = stdout_of(root, &["wait", &run_id, "--timeout", "30"]);
    assert!(waited.contains("finished"), "the run must finish");
    assert!(!holds(&waited), "apb wait leaked the value");
    let text = stdout_of(root, &["runs", &run_id]);
    assert!(!holds(&text), "apb runs <id> leaked the host reply");
    let json = stdout_of(root, &["runs", &run_id, "--json"]);
    assert!(!holds(&json), "apb runs <id> --json leaked the host reply");
    let journal =
        fs::read_to_string(root.join(".apb/runs").join(&run_id).join("events.jsonl")).unwrap();
    assert!(holds(&journal), "the journal must keep the host reply");
}

/// `apb runs <id>` withholds a run's text when its manifest, the only
/// record of a secret param whose unused declaration a patch dropped, does
/// not read.
#[test]
fn apb_runs_withholds_a_run_whose_manifest_does_not_read() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let (parent, _) = seed_secret_runs(root);
    let run_dir = root.join(".apb/runs").join(&parent);
    let snapshot = fs::read_to_string(run_dir.join("playbook.yaml")).unwrap();
    fs::write(
        run_dir.join("playbook.yaml"),
        snapshot.replace(
            "params:\n  - { name: message, type: text, secret: true }\n",
            "",
        ),
    )
    .unwrap();
    fs::write(run_dir.join("manifest.yaml"), "secret_params: [unclosed\n").unwrap();
    for args in [
        vec!["runs", parent.as_str()],
        vec!["runs", parent.as_str(), "--json"],
    ] {
        let out = stdout_of(root, &args);
        assert!(!holds(&out), "apb {args:?} leaked the value");
        assert!(out.contains("[secrets_unresolved]"), "apb {args:?}: {out}");
    }
}

/// D10 test 7 (e), the CLI: `apb resume` with the bare consent of a run
/// that renders a secret param is refused before the consent is recorded,
/// so the manifest stays as it was (and a schema-1 run gets none).
#[test]
fn apb_resume_refuses_a_rendering_run_before_it_records_a_consent() {
    for legacy in [false, true] {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let run_id = crate::secret_runs::seed_rendering_run(root, legacy);
        let manifest = root.join(".apb/runs").join(&run_id).join("manifest.yaml");
        let before = fs::read(&manifest).ok();
        let out = crate::common::apb_std()
            .args(["resume", &run_id, "--confirm-irreversible=true"])
            .current_dir(root)
            .output()
            .unwrap();
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert_eq!(out.status.code(), Some(2), "{stderr}");
        assert!(stderr.contains("secret_param_rendered"), "{stderr}");
        assert_eq!(fs::read(&manifest).ok(), before, "legacy: {legacy}");
        if legacy {
            assert!(!manifest.exists());
        }
    }
}

/// D10 test 7 (e), MCP `run_resume` (here, where the `apb` binary serves
/// a real MCP session): `confirm_irreversible: true` on a run that renders
/// a secret param answers the conflict, and the manifest is unchanged.
#[test]
fn mcp_run_resume_refuses_a_rendering_run_before_it_records_a_consent() {
    for legacy in [false, true] {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let run_id = crate::secret_runs::seed_rendering_run(root, legacy);
        // Stamped with the key of the `apb` binary's config dir (made by
        // its first command), so MCP takes it as a run created here.
        crate::common::apb_std()
            .arg("runs")
            .current_dir(root)
            .output()
            .unwrap();
        apb_core::run_origin::stamp_in(
            crate::common::sandbox_config_dir(),
            &root.join(".apb/runs").join(&run_id),
            &run_id,
        )
        .unwrap();
        let manifest = root.join(".apb/runs").join(&run_id).join("manifest.yaml");
        let before = fs::read(&manifest).ok();
        let out = crate::common::mcp_call(
            root,
            "run_resume",
            serde_json::json!({
                "run_id": run_id,
                "confirm_irreversible": true,
                "acknowledge_untrusted": true,
            }),
        );
        assert_eq!(out["result"]["isError"], true, "{out}");
        assert!(out.to_string().contains("secret_param_rendered"), "{out}");
        assert_eq!(fs::read(&manifest).ok(), before, "legacy: {legacy}");
        if legacy {
            assert!(!manifest.exists());
        }
    }
}
