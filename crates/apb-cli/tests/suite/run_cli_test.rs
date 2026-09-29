use assert_cmd::Command;
use predicates::prelude::*;
use std::fs;

// A playbook without agent_task: start -> prompt -> finish. No real agent is needed.
// Note: the `params:` entry describing `who` was added beyond the literal brief text -
// V13 (validate.rs) requires that `{{params.X}}` reference a declared playbook
// parameter; without it `run()` rejects the playbook as invalid (see also
// crates/apb-engine/tests/scheduler_test.rs).
const NOAGENT: &str = r#"
schema: 1
id: noagent
name: No Agent
version: 1.0.0
params:
  - { name: who, type: text }
nodes:
  - { id: start, type: start }
  - { id: note, type: prompt, prompt: "hi {{params.who}}" }
  - { id: done, type: finish, outcome: success }
edges:
  - { from: start, to: note }
  - { from: note, to: done }
"#;

fn playbook() -> Command {
    crate::common::apb()
}

fn seeded() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    playbook()
        .arg("init")
        .current_dir(dir.path())
        .assert()
        .success();
    let vdir = dir.path().join(".apb/playbooks/noagent/1.0.0");
    fs::create_dir_all(&vdir).unwrap();
    fs::write(vdir.join("playbook.yaml"), NOAGENT).unwrap();
    fs::write(dir.path().join(".apb/playbooks/noagent/current"), "1.0.0").unwrap();
    dir
}

#[test]
fn run_succeeds_and_writes_events() {
    let dir = seeded();
    playbook()
        .args(["run", "noagent", "--param", "who=world"])
        .current_dir(dir.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("succeeded"));
    // a run has appeared
    let runs_dir = dir.path().join(".apb/runs");
    let count = fs::read_dir(&runs_dir).unwrap().count();
    assert_eq!(count, 1);
}

#[test]
fn runs_command_lists_the_run() {
    let dir = seeded();
    playbook()
        .args(["run", "noagent"])
        .current_dir(dir.path())
        .assert()
        .success();
    playbook()
        .arg("runs")
        .current_dir(dir.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("noagent"))
        .stdout(predicate::str::contains("succeeded"));
}

// Task 4: `apb note <run_id> <text>` posts a supervisor note by appending a
// ContextAppend entry to the run's control.jsonl (dispatches to
// `apb_engine::scheduler::post_supervisor_command`).
#[test]
fn note_command_appends_context_append_to_control_jsonl() {
    let dir = seeded();
    playbook()
        .args(["run", "noagent", "--param", "who=world"])
        .current_dir(dir.path())
        .assert()
        .success();

    let runs_dir = dir.path().join(".apb/runs");
    let run_id = fs::read_dir(&runs_dir)
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .file_name()
        .to_string_lossy()
        .into_owned();

    playbook()
        .args(["note", &run_id, "hello"])
        .current_dir(dir.path())
        .assert()
        .success();

    let control = fs::read_to_string(runs_dir.join(&run_id).join("control.jsonl")).unwrap();
    assert!(
        control.contains("\"cmd\":\"context_append\"") && control.contains("\"note\":\"hello\""),
        "expected control.jsonl to contain a ContextAppend note, got:\n{control}"
    );
}

#[test]
fn run_without_project_fails_env() {
    let dir = tempfile::tempdir().unwrap();
    playbook()
        .args(["run", "ghost"])
        .current_dir(dir.path())
        .assert()
        .code(2);
}

/// Task 8 smoke: `apb stop <run_id>` against a run whose driver is gone
/// finalizes it, and says so.
#[test]
fn stop_finalizes_a_run_whose_driver_is_gone() {
    let dir = seeded();
    let run_dir = dir.path().join(".apb/runs/noagent-dead");
    fs::create_dir_all(&run_dir).unwrap();
    fs::write(
        run_dir.join("events.jsonl"),
        concat!(
            r#"{"seq":0,"ts":1,"type":"run_started","playbook":"noagent","version":"1.0.0"}"#,
            "\n",
            r#"{"seq":1,"ts":2,"type":"node_started","node":"note","attempt":1}"#,
            "\n"
        ),
    )
    .unwrap();

    playbook()
        .args(["stop", "noagent-dead"])
        .current_dir(dir.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("noagent-dead"));

    let journal = fs::read_to_string(run_dir.join("events.jsonl")).unwrap();
    assert!(
        journal.contains("run_aborted"),
        "apb stop must have finalized the abandoned run, journal: {journal}"
    );
}

/// An unknown run id fails loudly rather than pretending to stop something.
#[test]
fn stop_of_an_unknown_run_fails() {
    let dir = seeded();
    playbook()
        .args(["stop", "nope-1"])
        .current_dir(dir.path())
        .assert()
        .failure()
        .stderr(predicate::str::contains("not found"));
}

/// `--continued-from` threads into RunOptions and establishes run lineage.
#[test]
fn run_continued_from_establishes_lineage() {
    let dir = seeded();
    playbook()
        .args(["run", "noagent", "--param", "who=world"])
        .current_dir(dir.path())
        .assert()
        .success();

    let runs_dir = dir.path().join(".apb/runs");
    let first_id = fs::read_dir(&runs_dir)
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .file_name()
        .to_string_lossy()
        .into_owned();

    playbook()
        .args([
            "run",
            "noagent",
            "--param",
            "who=world",
            "--continued-from",
            &first_id,
        ])
        .current_dir(dir.path())
        .assert()
        .success();

    let second_id = fs::read_dir(&runs_dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .find(|id| id != &first_id)
        .expect("successor run dir");

    let pred_cfg = apb_engine::run_config::read_run_config(&runs_dir.join(&first_id)).unwrap();
    let succ_cfg = apb_engine::run_config::read_run_config(&runs_dir.join(&second_id)).unwrap();
    assert_eq!(pred_cfg.superseded_by.as_deref(), Some(second_id.as_str()));
    assert_eq!(succ_cfg.continued_from.as_deref(), Some(first_id.as_str()));
}

#[test]
fn run_continued_from_rejects_unknown_predecessor() {
    let dir = seeded();
    playbook()
        .args([
            "run",
            "noagent",
            "--param",
            "who=world",
            "--continued-from",
            "ghost-1",
        ])
        .current_dir(dir.path())
        .assert()
        .failure()
        .stderr(predicate::str::contains("run `ghost-1`"));
}

const OTHER: &str = r#"
schema: 1
id: other
name: Other
version: 1.0.0
params:
  - { name: who, type: text }
nodes:
  - { id: start, type: start }
  - { id: note, type: prompt, prompt: "hi {{params.who}}" }
  - { id: done, type: finish, outcome: success }
edges:
  - { from: start, to: note }
  - { from: note, to: done }
"#;

#[test]
fn run_continued_from_rejects_cross_playbook() {
    let dir = seeded();
    let vdir = dir.path().join(".apb/playbooks/other/1.0.0");
    fs::create_dir_all(&vdir).unwrap();
    fs::write(vdir.join("playbook.yaml"), OTHER).unwrap();
    fs::write(dir.path().join(".apb/playbooks/other/current"), "1.0.0").unwrap();

    playbook()
        .args(["run", "noagent", "--param", "who=world"])
        .current_dir(dir.path())
        .assert()
        .success();

    let runs_dir = dir.path().join(".apb/runs");
    let first_id = fs::read_dir(&runs_dir)
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .file_name()
        .to_string_lossy()
        .into_owned();

    playbook()
        .args([
            "run",
            "other",
            "--param",
            "who=world",
            "--continued-from",
            &first_id,
        ])
        .current_dir(dir.path())
        .assert()
        .failure()
        .stderr(predicate::str::contains("noagent"))
        .stderr(predicate::str::contains("other"));
}

/// `apb run` goes through the same run gate as MCP `playbook_run`: a playbook
/// whose `requires` is not met in this project is refused before anything is
/// written, naming the gate's policy code and the missing file. It used to run
/// anyway, because the CLI only checked connector trust.
#[test]
fn run_refuses_a_playbook_whose_requires_is_unmet() {
    let dir = seeded();
    let yaml = NOAGENT.replace("params:", "requires: { files: [NEEDED.md] }\nparams:");
    fs::write(
        dir.path()
            .join(".apb/playbooks/noagent/1.0.0/playbook.yaml"),
        yaml,
    )
    .unwrap();
    playbook()
        .args(["run", "noagent", "--param", "who=world"])
        .current_dir(dir.path())
        .assert()
        .code(2)
        .stderr(predicate::str::contains("requires_unmet"))
        .stderr(predicate::str::contains("NEEDED.md"));
    assert!(
        !dir.path().join(".apb/runs").exists()
            || fs::read_dir(dir.path().join(".apb/runs")).unwrap().count() == 0,
        "a refused start writes no run"
    );
}

// --- 0.24.0 irreversible consent ---

fn seeded_irreversible() -> tempfile::TempDir {
    let dir = seeded();
    let vdir = dir.path().join(".apb/playbooks/rel/1.0.0");
    fs::create_dir_all(&vdir).unwrap();
    fs::write(
        vdir.join("playbook.yaml"),
        NOAGENT
            .replace("id: noagent", "id: rel")
            .replace("nodes:", "effects: [irreversible]\nnodes:"),
    )
    .unwrap();
    fs::write(dir.path().join(".apb/playbooks/rel/current"), "1.0.0").unwrap();
    dir
}

/// The consent nonce an `irreversible_requires_confirmation` refusal printed.
pub(crate) fn printed_nonce(stderr: &str) -> String {
    let at = stderr
        .find("consent_nonce: ")
        .expect("the refusal prints a nonce")
        + "consent_nonce: ".len();
    stderr[at..]
        .chars()
        .take_while(|c| c.is_ascii_alphanumeric() || *c == '-')
        .collect()
}

fn only_run(root: &std::path::Path) -> std::path::PathBuf {
    let mut runs: Vec<_> = fs::read_dir(root.join(".apb/runs"))
        .unwrap()
        .map(|e| e.unwrap().path())
        .collect();
    assert_eq!(runs.len(), 1, "{runs:?}");
    runs.pop().unwrap()
}

fn no_runs(root: &std::path::Path) -> bool {
    fs::read_dir(root.join(".apb/runs"))
        .map(|mut d| d.next().is_none())
        .unwrap_or(true)
}

fn consent_by(run: &std::path::Path) -> String {
    apb_engine::manifest::read(run)
        .unwrap()
        .and_then(|m| m.consent)
        .expect("consent recorded")
        .by
}

/// Polls until the run in `run` has finished (a detached driver), bounded.
fn wait_finished(run: &std::path::Path) {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    while std::time::Instant::now() < deadline {
        if fs::read_to_string(run.join("events.jsonl"))
            .is_ok_and(|t| t.contains("\"type\":\"run_finished\""))
        {
            return;
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    panic!("run {} did not finish within 30s", run.display());
}

/// A headless `apb run` (no terminal, as in a test or CI) of an irreversible
/// playbook is refused with the sources and a consent nonce, foreground and
/// `--detach` alike; `--confirm-irreversible=<nonce>` is the script author's
/// consent and lands in the manifest, a stale nonce is refused, and a bare
/// flag still works for one release with a deprecation warning.
#[test]
fn a_headless_run_of_an_irreversible_playbook_needs_the_consent_nonce() {
    for detach in [false, true] {
        let dir = seeded_irreversible();
        let base = |extra: &[&str]| {
            let mut cmd = playbook();
            cmd.args(["run", "rel", "--param", "who=x"]);
            if detach {
                cmd.arg("--detach");
            }
            cmd.args(extra).current_dir(dir.path());
            cmd
        };
        let refused = base(&[]).assert().code(2);
        let stderr = String::from_utf8_lossy(&refused.get_output().stderr).to_string();
        assert!(
            stderr.contains(
                "run refused (irreversible_requires_confirmation): irreversible effects (playbook)"
            ),
            "detach={detach}: {stderr}"
        );
        assert!(
            stderr.contains("--confirm-irreversible=consent-"),
            "{stderr}"
        );
        let nonce = printed_nonce(&stderr);
        assert!(
            no_runs(dir.path()),
            "detach={detach}: a refusal writes no run"
        );

        base(&["--confirm-irreversible=consent-stale"])
            .assert()
            .code(2)
            .stderr(predicate::str::contains("consent_nonce_mismatch"));
        assert!(no_runs(dir.path()));

        let flag = format!("--confirm-irreversible={nonce}");
        base(&[&flag])
            .assert()
            .success()
            .stderr(predicate::str::contains("deprecated").not());
        let run = only_run(dir.path());
        if detach {
            wait_finished(&run);
        }
        assert_eq!(consent_by(&run), "cli_flag", "detach={detach}");
    }
    let dir = seeded_irreversible();
    playbook()
        .args(["run", "rel", "--param", "who=x", "--confirm-irreversible"])
        .current_dir(dir.path())
        .assert()
        .success()
        .stderr(predicate::str::contains("deprecated"));
    assert_eq!(consent_by(&only_run(dir.path())), "cli_flag");
}

/// Inside a run (`APB_RUN_ID` set) the terminal is never taken as consent;
/// the refusal says so. The interactive `[y/N]` question itself needs a
/// pseudo-terminal, which this suite has no helper for, so the `cli` path is
/// covered only by reading `crate::consent::ask`.
#[test]
fn a_start_from_inside_a_run_cannot_consent_at_the_terminal() {
    let dir = seeded_irreversible();
    playbook()
        .args(["run", "rel", "--param", "who=x"])
        .env("APB_RUN_ID", "outer-1")
        .current_dir(dir.path())
        .assert()
        .code(2)
        .stderr(predicate::str::contains("APB_RUN_ID is set"));
    assert!(no_runs(dir.path()));
}

/// Inside a run the flag is no consent either: a step that read the
/// refusal could echo its nonce. The refusal names the parent run and
/// points at the host, and no run is written; the same nonce works outside
/// the run.
#[test]
fn a_start_from_inside_a_run_cannot_consent_with_the_flag() {
    let dir = seeded_irreversible();
    let refused = playbook()
        .args(["run", "rel", "--param", "who=x"])
        .current_dir(dir.path())
        .assert()
        .code(2);
    let nonce = printed_nonce(&String::from_utf8_lossy(&refused.get_output().stderr));
    let flag = format!("--confirm-irreversible={nonce}");
    for detach in [false, true] {
        let mut cmd = playbook();
        cmd.args(["run", "rel", "--param", "who=x", &flag]);
        if detach {
            cmd.arg("--detach");
        }
        cmd.env("APB_RUN_ID", "outer-1")
            .current_dir(dir.path())
            .assert()
            .code(2)
            .stderr(predicate::str::contains("inside run `outer-1`"))
            .stderr(predicate::str::contains("confirm_irreversible"));
        assert!(
            no_runs(dir.path()),
            "detach={detach}: a refusal writes no run"
        );
    }
    playbook()
        .args(["run", "rel", "--param", "who=x", &flag])
        .current_dir(dir.path())
        .assert()
        .success();
}

/// The hidden `__drive-supervised --consent` accepts only `cli` and
/// `cli_flag`, and only together with the nonce.
#[test]
fn drive_supervised_accepts_only_the_cli_consents() {
    let dir = seeded_irreversible();
    let handshake = dir.path().join("hs.txt");
    playbook()
        .args([
            "__drive-supervised",
            "rel",
            "--consent",
            "dashboard",
            "--consent-nonce",
            "x",
            "--handshake",
        ])
        .arg(&handshake)
        .current_dir(dir.path())
        .assert()
        .code(2)
        .stderr(predicate::str::contains("invalid value 'dashboard'"));
    playbook()
        .args([
            "__drive-supervised",
            "rel",
            "--consent",
            "cli",
            "--handshake",
        ])
        .arg(&handshake)
        .current_dir(dir.path())
        .assert()
        .code(2)
        .stderr(predicate::str::contains("--consent-nonce"));
    assert!(no_runs(dir.path()));
}

/// `apb resume` of a run whose snapshot is irreversible and whose manifest
/// has no consent (as a run an older apb started) asks once: refused
/// headless with the nonce, then resumed with it, the consent recorded.
#[test]
fn a_resume_of_a_run_without_consent_asks_once() {
    let dir = seeded();
    playbook()
        .args(["run", "noagent", "--param", "who=x"])
        .current_dir(dir.path())
        .assert()
        .success();
    let run = only_run(dir.path());
    let run_id = run.file_name().unwrap().to_string_lossy().to_string();
    let snap = run.join("playbook.yaml");
    let yaml = fs::read_to_string(&snap).unwrap();
    fs::write(
        &snap,
        yaml.replace("nodes:", "effects: [irreversible]\nnodes:"),
    )
    .unwrap();

    let refused = playbook()
        .args(["resume", &run_id, "--from-node", "note"])
        .current_dir(dir.path())
        .assert()
        .code(2);
    let stderr = String::from_utf8_lossy(&refused.get_output().stderr).to_string();
    assert!(
        stderr.contains("irreversible_requires_confirmation"),
        "{stderr}"
    );
    let nonce = printed_nonce(&stderr);

    playbook()
        .args(["resume", &run_id, "--from-node", "note"])
        .arg(format!("--confirm-irreversible={nonce}"))
        .current_dir(dir.path())
        .assert()
        .success();
    assert_eq!(consent_by(&run), "cli_flag");
    // Recorded: the next resume asks nothing.
    playbook()
        .args(["resume", &run_id, "--from-node", "note"])
        .current_dir(dir.path())
        .assert()
        .success();
}
