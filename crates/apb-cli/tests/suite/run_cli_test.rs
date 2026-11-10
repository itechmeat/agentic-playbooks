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

/// `apb run --remember` keeps the given input for this project; the next
/// start without `--param` uses it, and a credential-shaped value is refused
/// by name without being stored or echoed.
#[test]
fn run_remember_keeps_the_input_for_the_next_start() {
    let dir = seeded();
    playbook()
        .args(["run", "noagent", "--param", "who=world", "--remember"])
        .current_dir(dir.path())
        .assert()
        .success()
        .stderr(predicate::str::contains("remembered for this project: who"));
    playbook()
        .args(["run", "noagent"])
        .current_dir(dir.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("succeeded"))
        .stderr(predicate::str::contains("used remembered input: who"));
    let stored: serde_json::Value = serde_json::from_str(
        &fs::read_to_string(dir.path().join(".apb/memory/project/noagent.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(stored["params"], serde_json::json!({ "who": "world" }));

    let token = format!("ghp_{}", "a".repeat(36));
    let out = playbook()
        .args(["run", "noagent", "--remember", "--param"])
        .arg(format!("who={token}"))
        .current_dir(dir.path())
        .output()
        .unwrap();
    // The messages never print the output: it would carry the token if the
    // refusal broke.
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("not remembered: who (value_looks_secret)"),
        "the refusal is named"
    );
    assert!(!stderr.contains(&token), "a refused value is never echoed");
    let memory = fs::read_to_string(dir.path().join(".apb/memory/project/noagent.json")).unwrap();
    assert!(
        memory.contains("world") && !memory.contains(&token),
        "the stored value stays, the refused one is not stored"
    );
}

#[test]
fn runs_command_lists_the_run() {
    let dir = seeded();
    playbook()
        .args(["run", "noagent", "--param", "who=world"])
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

/// Polls until the run in `run` has finished (a detached driver) and its
/// driver let go of the workdir ([`crate::common::wait_released`]), bounded.
fn wait_finished(run: &std::path::Path) {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    while std::time::Instant::now() < deadline {
        if fs::read_to_string(run.join("events.jsonl"))
            .is_ok_and(|t| t.contains("\"type\":\"run_finished\""))
        {
            let id = run.file_name().unwrap().to_string_lossy().into_owned();
            let root = run.ancestors().nth(3).unwrap();
            crate::common::wait_released(root, &id);
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

/// The detached supervised child takes its consent and its launch decision
/// only from a handoff its own parent wrote: the consent never travels on
/// argv any more, and a handoff another process wrote is refused, leaving
/// the child without a consent, so nothing starts.
#[test]
fn drive_supervised_takes_its_consent_only_from_its_parents_handoff() {
    let dir = seeded_irreversible();
    let handshake = dir.path().join("hs.txt");
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
        .stderr(predicate::str::contains("--consent"));
    // A handoff whose writer (this test) is not the child's parent.
    use apb_engine::gate::launch::{Choice, HandoffBinding, HandoffGrant};
    let handoff = apb_engine::launch_handoff::write(
        dir.path(),
        &apb_engine::launch_handoff::Handoff::Supervise {
            grant: HandoffGrant {
                record: apb_core::launch::LaunchRecord {
                    kind: "start".into(),
                    initiator: "cli_terminal".into(),
                    confirmation: None,
                    policy: apb_core::launch::LaunchPolicy::Allow,
                    sources: vec![],
                    billing: None,
                    started_by: None,
                },
                binding: HandoffBinding::Nonce { nonce: "x".into() },
                config_dir: None,
            },
            version: "1.0.0".into(),
            choice: Choice::Current,
            consent: Some(apb_engine::launch_handoff::ForwardedConsent {
                by: "cli".into(),
                nonce: "x".into(),
            }),
            deadline_ms: None,
            start_key: None,
        },
    )
    .unwrap();
    // Spawned through a shell, so its parent is not the handoff's writer.
    let line = [
        crate::common::apb_bin().display().to_string(),
        "__drive-supervised".into(),
        "rel".into(),
        "--param".into(),
        "who=x".into(),
        "--launch-handoff".into(),
        handoff.display().to_string(),
        "--handshake".into(),
        handshake.display().to_string(),
    ]
    .map(|a| format!("'{a}'"))
    .join(" ");
    assert_cmd::Command::new("sh")
        .args(["-c", &format!("{line}; exit $?")])
        .env("APB_CONFIG_DIR", crate::common::sandbox_config_dir())
        .env_remove("APB_RUN_ID")
        .env_remove("APB_HOST_TASK")
        .current_dir(dir.path())
        .assert()
        .code(2)
        .stderr(predicate::str::contains(
            "not written by this process's parent",
        ));
    let reported = std::fs::read_to_string(&handshake).unwrap();
    assert!(reported.contains("irreversible"), "{reported}");
    assert!(!handoff.exists(), "a refused handoff is still deleted");
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

// --- unattended mode: the start surface ---

/// `apb run --unattended` with `--supervise` or `--execution host` is
/// refused before anything is written: exit 2, the policy
/// `unattended_unsupported` and the change a person makes by hand, and no
/// run directory.
#[test]
fn an_unattended_start_that_needs_a_watcher_is_refused_before_any_run_file() {
    let dir = seeded();
    for (flags, says) in [
        (
            vec!["--supervise"],
            "start the unattended run without supervision",
        ),
        (
            vec!["--execution", "host"],
            "start the unattended run with execution: cli",
        ),
    ] {
        let mut args = vec!["run", "noagent", "--param", "who=world", "--unattended"];
        args.extend(flags.iter().copied());
        playbook()
            .args(&args)
            .current_dir(dir.path())
            .assert()
            .code(2)
            .stderr(predicate::str::contains(
                "run refused (unattended_unsupported)",
            ))
            .stderr(predicate::str::contains(
                "next: playbook `noagent` cannot start unattended this way",
            ))
            .stderr(predicate::str::contains(says));
        assert!(no_runs(dir.path()), "{flags:?} wrote a run");
    }
}

/// `apb run --unattended` records the mode in the run's write-once
/// manifest, and `apb runs <id>` (text and `--json`) shows it; an attended
/// run shows neither.
#[test]
fn an_unattended_run_records_and_shows_its_mode() {
    let dir = seeded();
    playbook()
        .args(["run", "noagent", "--param", "who=world", "--unattended"])
        .current_dir(dir.path())
        .assert()
        .success();
    let run = only_run(dir.path());
    let id = run.file_name().unwrap().to_string_lossy().into_owned();
    assert!(
        apb_engine::manifest::read(&run)
            .unwrap()
            .unwrap()
            .unattended
    );
    playbook()
        .args(["runs", &id])
        .current_dir(dir.path())
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "  unattended: every wait without an unattended outcome fails the run (hitl_required)",
        ));
    let json = playbook()
        .args(["runs", &id, "--json"])
        .current_dir(dir.path())
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let v: serde_json::Value = serde_json::from_slice(&json).unwrap();
    assert_eq!(v["unattended"], true, "{v}");

    fs::remove_dir_all(&run).unwrap();
    playbook()
        .args(["run", "noagent", "--param", "who=world"])
        .current_dir(dir.path())
        .assert()
        .success();
    let run = only_run(dir.path());
    let id = run.file_name().unwrap().to_string_lossy().into_owned();
    playbook()
        .args(["runs", &id])
        .current_dir(dir.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("unattended").not());
    let json = playbook()
        .args(["runs", &id, "--json"])
        .current_dir(dir.path())
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let v: serde_json::Value = serde_json::from_slice(&json).unwrap();
    assert!(v.get("unattended").is_none(), "{v}");
}

/// The run's `--deadline` is recorded on `run_provenance` and guarded; an
/// unparsable value is refused `deadline_invalid` before any run file.
#[test]
fn deadline_flag_is_recorded() {
    let dir = seeded();
    playbook()
        .args(["run", "noagent", "--param", "who=x", "--deadline", "soon"])
        .current_dir(dir.path())
        .assert()
        .code(2)
        .stderr(predicate::str::contains("deadline_invalid"));
    assert!(no_runs(dir.path()));
    playbook()
        .args(["run", "noagent", "--param", "who=x", "--deadline", "45m"])
        .current_dir(dir.path())
        .assert()
        .success();
    let run = only_run(dir.path());
    let text = fs::read_to_string(run.join("events.jsonl")).unwrap();
    assert!(
        text.contains("\"deadline\":{\"total_ms\":2700000,\"source\":\"run\"}"),
        "{text}"
    );
    assert!(text.contains("\"requires\":[\"deadline\"]"), "{text}");
}

/// `apb resume <id> --deadline` reaches a detached driver: a host-free
/// failed run resumed with more time journals the grant on `run_resumed`.
#[test]
fn resume_deadline_flag_reaches_the_driver() {
    let dir = seeded();
    playbook()
        .args(["run", "noagent", "--param", "who=x", "--deadline", "10m"])
        .current_dir(dir.path())
        .assert()
        .success();
    let run = only_run(dir.path());
    let id = run.file_name().unwrap().to_string_lossy().into_owned();
    playbook()
        .args(["resume", &id, "--from-node", "note", "--deadline", "5m"])
        .current_dir(dir.path())
        .assert()
        .success();
    let text = fs::read_to_string(run.join("events.jsonl")).unwrap();
    let resumed = text
        .lines()
        .find(|l| l.contains("\"type\":\"run_resumed\""))
        .expect("a run_resumed");
    assert!(resumed.contains("\"source\":\"resume\""), "{resumed}");
}

/// A run `apb run` starts from a run step names its starter on `apb runs
/// <id>`, and `apb runs <starter>` lists it under the runs its steps
/// started in this project.
#[test]
fn a_step_started_run_and_its_starter_name_each_other() {
    let dir = seeded();
    let root = dir.path();
    // A first run makes the installation key the starter is stamped with.
    playbook()
        .args(["run", "noagent", "--param", "who=x"])
        .current_dir(root)
        .assert()
        .success();
    let starter = root.join(".apb/runs/t-1");
    fs::create_dir_all(&starter).unwrap();
    fs::write(
        starter.join("events.jsonl"),
        concat!(
            "{\"seq\":1,\"ts\":1,\"type\":\"run_started\",\"playbook\":\"triage\",\"version\":\"1.0.0\"}\n",
            "{\"seq\":2,\"ts\":2,\"type\":\"node_started\",\"node\":\"triage\",\"attempt\":1}\n",
            "{\"seq\":3,\"ts\":3,\"type\":\"attempt_started\",\"node\":\"triage\",\"attempt\":2,\"agent\":\"claude\",\"token\":\"tok-cli-1\"}\n",
        ),
    )
    .unwrap();
    apb_core::run_origin::stamp_in(crate::common::sandbox_config_dir(), &starter, "t-1").unwrap();
    let out = playbook()
        .args(["run", "noagent", "--param", "who=y", "--detach"])
        .env("APB_RUN_ID", "t-1")
        .env("APB_RUN_DIR", &starter)
        .env("APB_NODE_ID", "triage")
        .env("APB_ATTEMPT_TOKEN", "tok-cli-1")
        .current_dir(root)
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout).to_string();
    assert!(
        out.status.success(),
        "{stdout} {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let id = stdout
        .lines()
        .find_map(|l| l.strip_prefix("run started: "))
        .expect("run id")
        .trim()
        .to_string();
    let shown = |run: &str| {
        let out = playbook()
            .args(["runs", run])
            .current_dir(root)
            .output()
            .unwrap();
        String::from_utf8_lossy(&out.stdout).to_string()
    };
    let new = shown(&id);
    assert!(
        new.contains("started by run t-1 (node triage, attempt 2)"),
        "{new}"
    );
    let starter_text = shown("t-1");
    assert!(
        starter_text.contains(&format!("started runs in this project: {id} (noagent)")),
        "{starter_text}"
    );
    let manifest = fs::read_to_string(root.join(".apb/runs").join(&id).join("manifest.yaml"))
        .unwrap_or_default();
    assert!(!manifest.contains("tok-cli-1"), "the token was stored");
}

/// An agent of run A (chain 1) starts run B; B's script step starts run C.
/// The script inherits the environment of the `apb run` that drove B, A's
/// attempt credentials included, yet it is verified by its own run context:
/// C names B's script node, verified, at chain 3.
#[test]
fn a_script_step_of_a_step_started_run_is_verified_by_its_own_run() {
    let dir = seeded();
    let root = dir.path();
    playbook()
        .args(["run", "noagent", "--param", "who=x"])
        .current_dir(root)
        .assert()
        .success();
    let vdir = root.join(".apb/playbooks/chainer/1.0.0");
    fs::create_dir_all(vdir.join("scripts")).unwrap();
    fs::write(
        vdir.join("playbook.yaml"),
        "schema: 2\nid: chainer\nname: chainer\nversion: 1.0.0\nnodes:\n  - { id: start, type: start }\n  - { id: kick, type: script, script: \"scripts/kick.sh\", runner: sh }\n  - { id: done, type: finish, outcome: success }\nedges:\n  - { from: start, to: kick }\n  - { from: kick, to: done }\n",
    )
    .unwrap();
    fs::write(
        vdir.join("scripts/kick.sh"),
        "\"$APB_TEST_BIN\" run noagent --param who=c --allow-shared-workdir > \"$APB_TEST_OUT\" 2>&1\n",
    )
    .unwrap();
    fs::write(root.join(".apb/playbooks/chainer/current"), "1.0.0").unwrap();
    let a = root.join(".apb/runs/a-1");
    fs::create_dir_all(&a).unwrap();
    fs::write(
        a.join("events.jsonl"),
        concat!(
            "{\"seq\":1,\"ts\":1,\"type\":\"run_started\",\"playbook\":\"triage\",\"version\":\"1.0.0\"}\n",
            "{\"seq\":2,\"ts\":2,\"type\":\"node_started\",\"node\":\"triage\",\"attempt\":1}\n",
            "{\"seq\":3,\"ts\":3,\"type\":\"attempt_started\",\"node\":\"triage\",\"attempt\":1,\"agent\":\"claude\",\"token\":\"tok-chain-a\"}\n",
        ),
    )
    .unwrap();
    let manifest = serde_json::json!({
        "profiles": [], "node_bindings": {},
        "launch": {
            "kind": "start", "initiator": "run_step:cli", "policy": "allow", "sources": [],
            "started_by": { "run_id": "earlier", "verified": true, "chain": 1 }
        }
    });
    fs::write(a.join("manifest.yaml"), manifest.to_string()).unwrap();
    apb_core::run_origin::stamp_in(crate::common::sandbox_config_dir(), &a, "a-1").unwrap();
    let out_file = root.join("c-out.txt");
    let out = playbook()
        .args(["run", "chainer"])
        .env("APB_RUN_ID", "a-1")
        .env("APB_RUN_DIR", &a)
        .env("APB_NODE_ID", "triage")
        .env("APB_ATTEMPT", "1")
        .env("APB_ATTEMPT_TOKEN", "tok-chain-a")
        .env("APB_TEST_BIN", crate::common::apb_bin())
        .env("APB_TEST_OUT", &out_file)
        .current_dir(root)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{} {} | C: {}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr),
        fs::read_to_string(&out_file).unwrap_or_default()
    );
    let by_of = |id: &str| apb_engine::run_view::started_by(&root.join(".apb/runs").join(id));
    let ids: Vec<String> = fs::read_dir(root.join(".apb/runs"))
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    let b = ids
        .iter()
        .find(|id| id.starts_with("chainer-"))
        .expect("run B");
    let b_by = by_of(b).expect("B names its starter");
    assert_eq!(
        (b_by.run_id.as_str(), b_by.verified, b_by.chain),
        ("a-1", true, 2)
    );
    let c = ids
        .iter()
        .find(|id| by_of(id).is_some_and(|by| &by.run_id == b))
        .unwrap_or_else(|| {
            panic!(
                "no run names B; C said: {}",
                fs::read_to_string(&out_file).unwrap_or_default()
            )
        });
    let c_by = by_of(c).unwrap();
    assert_eq!(c_by.node.as_deref(), Some("kick"));
    assert_eq!(
        (c_by.verified, c_by.chain, c_by.attempt),
        (true, 3, None),
        "{c_by:?}"
    );
}

// ---- the start key ----

/// Runs `apb run <args>` in `root`; returns (exit code, stdout, stderr).
fn apb_run(root: &std::path::Path, args: &[&str]) -> (i32, String, String) {
    let out = playbook()
        .arg("run")
        .args(args)
        .current_dir(root)
        .output()
        .unwrap();
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).to_string(),
        String::from_utf8_lossy(&out.stderr).to_string(),
    )
}

fn run_count(root: &std::path::Path) -> usize {
    fs::read_dir(root.join(".apb/runs"))
        .map(|r| r.count())
        .unwrap_or(0)
}

#[test]
fn start_key_repeat_detach_prints_the_run() {
    let dir = seeded();
    let root = dir.path();
    let args = [
        "noagent",
        "--param",
        "who=x",
        "--start-key",
        "ci-42",
        "--detach",
    ];
    let (code, out, err) = apb_run(root, &args);
    assert_eq!(code, 0, "{out} {err}");
    let id = out
        .lines()
        .find_map(|l| l.strip_prefix("run started: "))
        .expect("run id")
        .trim()
        .to_string();
    let (code, out, err) = apb_run(root, &args);
    assert_eq!(code, 0, "{out} {err}");
    assert!(
        out.contains(&format!("run already started: {id} (start key)")),
        "{out}"
    );
    assert_eq!(run_count(root), 1);
    let shown = playbook()
        .args(["runs", &id])
        .current_dir(root)
        .output()
        .unwrap();
    assert!(String::from_utf8_lossy(&shown.stdout).contains("start key: "));
}

/// The start key is looked up before the checks that depend on the
/// environment: an unattended host start the kill switch turned into cli
/// started, and the same arguments without the switch answer that run
/// instead of being refused `unattended_unsupported`.
#[test]
fn start_key_repeat_is_answered_before_runtime_checks() {
    let dir = seeded();
    let root = dir.path();
    let args = [
        "run",
        "noagent",
        "--param",
        "who=x",
        "--unattended",
        "--execution",
        "host",
        "--start-key",
        "env-1",
        "--detach",
    ];
    let first = playbook()
        .args(args)
        .env("APB_EXECUTION", "cli")
        .current_dir(root)
        .output()
        .unwrap();
    let out = String::from_utf8_lossy(&first.stdout).to_string();
    assert_eq!(
        first.status.code(),
        Some(0),
        "{out} {}",
        String::from_utf8_lossy(&first.stderr)
    );
    let id = out
        .lines()
        .find_map(|l| l.strip_prefix("run started: "))
        .expect("run id")
        .trim()
        .to_string();
    let again = playbook()
        .args(args)
        .env_remove("APB_EXECUTION")
        .current_dir(root)
        .output()
        .unwrap();
    let (out, err) = (
        String::from_utf8_lossy(&again.stdout),
        String::from_utf8_lossy(&again.stderr),
    );
    assert_eq!(again.status.code(), Some(0), "{out} {err}");
    assert!(
        out.contains(&format!("run already started: {id} (start key)")),
        "{out} {err}"
    );
    assert_eq!(run_count(root), 1);
}

const FAILS: &str = "schema: 2\nid: fails\nname: fails\nversion: 1.0.0\nnodes:\n  - { id: start, type: start }\n  - { id: s, type: script, script: \"scripts/s.sh\", runner: sh }\n  - { id: done, type: finish, outcome: success }\nedges:\n  - { from: start, to: s }\n  - { from: s, to: done }\n";

#[test]
fn start_key_repeat_of_a_failed_run_exits_1() {
    let dir = seeded();
    let root = dir.path();
    let v = root.join(".apb/playbooks/fails/1.0.0");
    fs::create_dir_all(v.join("scripts")).unwrap();
    fs::write(v.join("playbook.yaml"), FAILS).unwrap();
    fs::write(v.join("scripts/s.sh"), "exit 1\n").unwrap();
    fs::write(root.join(".apb/playbooks/fails/current"), "1.0.0").unwrap();
    let (code, _, _) = apb_run(root, &["fails", "--start-key", "k"]);
    assert_eq!(code, 1);
    let (code, out, err) = apb_run(root, &["fails", "--start-key", "k"]);
    assert_eq!(code, 1, "{out} {err}");
    assert!(out.contains("run already started:"), "{out}");
    assert_eq!(run_count(root), 1);
    let (code, out, err) = apb_run(root, &["fails", "--start-key", "k", "--start-key-reuse"]);
    assert_eq!(code, 1, "{out} {err}");
    assert_eq!(run_count(root), 2, "a reuse starts a new run");
}

#[test]
fn start_key_conflict_exits_2() {
    let dir = seeded();
    let root = dir.path();
    let (code, ..) = apb_run(root, &["noagent", "--param", "who=x", "--start-key", "k"]);
    assert_eq!(code, 0);
    for extra in [
        vec!["--param", "who=y"],
        vec!["--param", "who=x", "--version", "1.0.0"],
        vec!["--param", "who=x", "--deadline", "10m"],
        vec!["--param", "who=x", "--external-data"],
        vec!["--param", "who=x", "--worktree", "."],
    ] {
        let mut args = vec!["noagent", "--start-key", "k"];
        args.extend(extra.iter().copied());
        let (code, out, err) = apb_run(root, &args);
        assert_eq!(code, 2, "{extra:?}: {out} {err}");
        assert!(
            err.contains("run refused (start_key_conflict)"),
            "{extra:?}: {err}"
        );
    }
    assert_eq!(run_count(root), 1);
    let token = format!(
        "ghp_{}",
        "a1B2c3D4e5".repeat(4).chars().take(36).collect::<String>()
    );
    let (code, _, err) = apb_run(
        root,
        &["noagent", "--param", "who=x", "--start-key", &token],
    );
    assert_eq!(code, 2);
    assert!(err.contains("start_key_invalid"), "{err}");
    assert!(!err.contains(&token), "the key was printed");
}

#[test]
fn start_key_conflict_on_no_cache() {
    let dir = seeded();
    let root = dir.path();
    let (code, ..) = apb_run(root, &["noagent", "--param", "who=x", "--start-key", "k"]);
    assert_eq!(code, 0);
    let (code, _, err) = apb_run(
        root,
        &[
            "noagent",
            "--param",
            "who=x",
            "--start-key",
            "k",
            "--no-cache",
        ],
    );
    assert_eq!(code, 2, "{err}");
    assert!(err.contains("start_key_conflict"), "{err}");
}

#[test]
fn start_key_conflict_on_supervise() {
    let dir = seeded();
    let root = dir.path();
    let (code, ..) = apb_run(root, &["noagent", "--param", "who=x", "--start-key", "k"]);
    assert_eq!(code, 0);
    let (code, _, err) = apb_run(
        root,
        &[
            "noagent",
            "--param",
            "who=x",
            "--start-key",
            "k",
            "--supervise",
        ],
    );
    assert_eq!(code, 2, "{err}");
    assert!(err.contains("start_key_conflict"), "{err}");
    assert_eq!(run_count(root), 1);
}

/// The request is what the caller passed: a keyed start without params
/// recalls remembered input; the input remembered changes; the same keyed
/// start answers the first run instead of a conflict.
#[test]
fn the_request_is_what_the_caller_passed() {
    let dir = seeded();
    let root = dir.path();
    let (code, ..) = apb_run(root, &["noagent", "--param", "who=a", "--remember"]);
    assert_eq!(code, 0);
    let (code, out, err) = apb_run(root, &["noagent", "--start-key", "k", "--detach"]);
    assert_eq!(code, 0, "{out} {err}");
    let id = out
        .lines()
        .find_map(|l| l.strip_prefix("run started: "))
        .expect("run id")
        .trim()
        .to_string();
    let (code, ..) = apb_run(root, &["noagent", "--param", "who=b", "--remember"]);
    assert_eq!(code, 0);
    let (code, out, err) = apb_run(root, &["noagent", "--start-key", "k", "--detach"]);
    assert_eq!(code, 0, "{out} {err}");
    assert!(
        out.contains(&format!("run already started: {id} (start key)")),
        "{out} {err}"
    );
}
