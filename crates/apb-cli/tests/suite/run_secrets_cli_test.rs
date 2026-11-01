//! The CLI run surfaces never print a run's secret value (design: secrets
//! of a run), on stdout or stderr: `apb run`, `apb resume`, `apb runs`,
//! `apb runs <id>` in text and JSON, `apb wait` and the HTML report, for a
//! run and its child. A host-mode run's task prompt is the one place the
//! value is delivered, to the host that executes it.

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
  - { id: echo, type: agent_task, prompt: "Repeat this: {{params.message}}" }
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
fn a_host_task_delivers_the_value_and_no_other_surface_shows_it() {
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

    // Delivery: the prompt the host executes carries the value.
    let task = poll("the host task", || {
        let json = stdout_of(root, &["tasks", &run_id, "--json"]);
        serde_json::from_str::<Vec<Value>>(&json)
            .ok()?
            .into_iter()
            .find(|t| t["node"] == "echo")
    });
    assert!(
        task["prompt"].as_str().is_some_and(holds),
        "the host task prompt must deliver the value"
    );
    let status: Value =
        serde_json::from_str(&stdout_of(root, &["runs", &run_id, "--json"])).unwrap();
    assert!(
        status["pending_tasks"].to_string().contains(SECRET),
        "run_status must deliver the value in pending_tasks"
    );
    assert!(
        !holds(&without_delivery(status).to_string()),
        "run_status leaked the value outside pending_tasks"
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
