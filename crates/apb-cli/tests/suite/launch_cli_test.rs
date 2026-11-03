//! The launch policy at the CLI: the CLI without a terminal confirms an
//! `ask` launch only with the refusal's nonce and never a `deny` one, a run
//! step never confirms, `apb resume` checks a run never authorized, a
//! detached driver takes its grant only from its parent's handoff, a
//! person lowers a floor only at a terminal, and at a pseudo-terminal the
//! CLI asks (a detached `--supervise` child included).

use std::fs;
use std::path::Path;

use predicates::prelude::*;

fn apb() -> assert_cmd::Command {
    crate::common::apb()
}

fn pb(id: &str, decl: Option<&str>, extra: &str) -> String {
    let decl = decl
        .map(|d| format!("agent_launch: {d}\n"))
        .unwrap_or_default();
    format!(
        "schema: 2\nid: {id}\nname: {id}\nversion: 1.0.0\n{decl}{extra}nodes:\n  - {{ id: start, type: start }}\n  - {{ id: s, type: script, script: \"scripts/s.sh\", runner: sh }}\n  - {{ id: done, type: finish, outcome: success }}\nedges:\n  - {{ from: start, to: s }}\n  - {{ from: s, to: done }}\n"
    )
}

struct Project {
    dir: tempfile::TempDir,
    cfg: tempfile::TempDir,
}

impl Project {
    fn new(id: &str, body: &str) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let cfg = tempfile::tempdir().unwrap();
        apb_core::registry::init_project(dir.path()).unwrap();
        let v = dir.path().join(".apb/playbooks").join(id).join("1.0.0");
        fs::create_dir_all(v.join("scripts")).unwrap();
        fs::write(v.join("playbook.yaml"), body).unwrap();
        fs::write(
            v.join("scripts/s.sh"),
            "if [ -f \"$APB_TEST_FAIL\" ]; then exit 1; fi\necho ok\n",
        )
        .unwrap();
        fs::write(
            dir.path().join(".apb/playbooks").join(id).join("current"),
            "1.0.0",
        )
        .unwrap();
        Project { dir, cfg }
    }

    fn root(&self) -> &Path {
        self.dir.path()
    }

    fn apb(&self) -> assert_cmd::Command {
        let mut c = apb();
        c.current_dir(self.root())
            .env("APB_CONFIG_DIR", self.cfg.path())
            .env("APB_TEST_FAIL", self.root().join("fail"));
        c
    }

    fn machine_agent(&self, p: &str) {
        fs::write(
            self.cfg.path().join("policy.yaml"),
            format!("launch:\n  agent: {p}\n"),
        )
        .unwrap();
    }

    fn runs(&self) -> Vec<String> {
        fs::read_dir(self.root().join(".apb/runs"))
            .map(|rd| {
                rd.filter_map(|e| e.ok())
                    .map(|e| e.file_name().to_string_lossy().into_owned())
                    .collect()
            })
            .unwrap_or_default()
    }
}

/// The `--confirm-launch=... --launch-version=... --launch-choice=...`
/// flags a refusal printed.
fn printed_flags(stderr: &str) -> Vec<String> {
    let at = stderr
        .find("--confirm-launch=")
        .expect("the refusal prints the flags");
    stderr[at..]
        .split_whitespace()
        .take(3)
        .map(|s| s.trim_end_matches([',', '.']).to_string())
        .collect()
}

fn stderr_of(a: &assert_cmd::assert::Assert) -> String {
    String::from_utf8_lossy(&a.get_output().stderr).into_owned()
}

#[test]
fn the_cli_without_a_terminal_confirms_ask_only_with_the_nonce_and_never_deny() {
    let p = Project::new("pb", &pb("pb", Some("ask"), ""));
    let refused = p.apb().args(["run", "pb"]).assert().code(2);
    let err = stderr_of(&refused);
    assert!(err.contains("launch_requires_confirmation"), "{err}");
    assert!(p.runs().is_empty());
    let flags = printed_flags(&err);
    let mut forged = flags.clone();
    forged[0] = "--confirm-launch=launch-0000".into();
    p.apb().args(["run", "pb"]).args(&forged).assert().code(2);
    let ok = p.apb().args(["run", "pb"]).args(&flags).assert().success();
    let out = String::from_utf8_lossy(&ok.get_output().stdout).into_owned();
    let run_id = out
        .lines()
        .find_map(|l| l.strip_prefix("run "))
        .and_then(|l| l.split_whitespace().next())
        .unwrap()
        .to_string();
    p.apb()
        .args(["runs", &run_id])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "launch: start by cli, confirmed by cli_flag; policy ask",
        ));

    let d = Project::new("pb", &pb("pb", Some("deny"), ""));
    let err = stderr_of(&d.apb().args(["run", "pb"]).assert().code(2));
    assert!(err.contains("launch_requires_person"), "{err}");
    d.apb()
        .args([
            "run",
            "pb",
            "--confirm-launch=launch-x",
            "--launch-version=1.0.0",
            "--launch-choice=current",
        ])
        .assert()
        .code(2)
        .stderr(predicate::str::contains("launch_requires_person"));
    assert!(d.runs().is_empty());
}

#[test]
fn a_run_step_never_confirms_a_launch_at_the_cli() {
    let p = Project::new("pb", &pb("pb", Some("ask"), ""));
    let flags = printed_flags(&stderr_of(&p.apb().args(["run", "pb"]).assert().code(2)));
    for marker in ["APB_RUN_ID", "APB_HOST_TASK"] {
        p.apb()
            .env(marker, "parent-1")
            .args(["run", "pb"])
            .args(&flags)
            .assert()
            .code(2)
            .stderr(predicate::str::contains("launch_refused_in_run_step"));
    }
    assert!(p.runs().is_empty());
}

/// `apb resume` of a run never authorized: the CLI's own decision table,
/// with the run's nonce; the resume journals its launch record, which
/// `apb runs <id>` shows.
#[test]
fn a_resume_of_a_run_never_authorized_needs_the_nonce() {
    let p = Project::new("pb", &pb("pb", None, ""));
    fs::write(p.root().join("fail"), "").unwrap();
    // A run nothing authorized, as an older apb started it.
    let res = apb_engine_run(&p, "pb");
    p.machine_agent("ask");
    fs::remove_file(p.root().join("fail")).unwrap();
    let err = stderr_of(
        &p.apb()
            .args(["resume", &res, "--from-node", "s"])
            .assert()
            .code(2),
    );
    assert!(err.contains("launch_requires_confirmation"), "{err}");
    let flags = printed_flags(&err);
    p.apb()
        .args(["resume", &res, "--from-node", "s"])
        .args(&flags)
        .assert()
        .success()
        .stdout(predicate::str::contains("succeeded"));
    let launch = journal(&p, &res)
        .into_iter()
        .filter(|e| e["type"] == "run_resumed")
        .find_map(|e| e.get("launch").cloned())
        .expect("the resume journals its launch record");
    assert_eq!(launch["kind"], "resume", "{launch}");
    assert_eq!(launch["initiator"], "cli", "{launch}");
    assert_eq!(launch["confirmation"], "cli_flag", "{launch}");
    assert_eq!(launch["policy"], "ask", "{launch}");
    p.apb()
        .args(["runs", &res])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "launch: resume by cli, confirmed by cli_flag; policy ask",
        ));
}

/// The events of run `run_id`, each line parsed.
fn journal(p: &Project, run_id: &str) -> Vec<serde_json::Value> {
    fs::read_to_string(p.root().join(".apb/runs").join(run_id).join("events.jsonl"))
        .unwrap()
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect()
}

/// `apb resume --all` without a terminal meets a due paused run that was
/// never authorized, of a playbook that is `deny` for agents: it lists the
/// run as left by the launch policy and leaves it paused.
#[test]
fn resume_all_leaves_a_run_the_launch_policy_refuses() {
    use apb_engine::event::{EventLog, EventPayload};
    let p = Project::new("pb", &pb("pb", None, ""));
    fs::write(p.root().join("fail"), "").unwrap();
    let res = apb_engine_run(&p, "pb");
    fs::remove_file(p.root().join("fail")).unwrap();
    // The run resumed, then paused by its operator until a time long past:
    // a due pause `--all` considers.
    let mut log = EventLog::open(&p.root().join(".apb/runs").join(&res)).unwrap();
    log.append(EventPayload::resumed(
        "s",
        Some(apb_engine::pause::PauseBy::Operator),
        false,
    ))
    .unwrap();
    log.append(EventPayload::RunPaused {
        reason: "operator".into(),
        by: Some(apb_engine::pause::PauseBy::Operator),
        mode: None,
        until: Some(1),
        keep_lock: false,
    })
    .unwrap();
    drop(log);
    let before = journal(&p, &res).len();
    p.machine_agent("deny");
    p.apb()
        .args(["resume", "--all"])
        .assert()
        .success()
        .stdout(predicate::str::contains(format!(
            "{res}  left: launch policy: launch_requires_person"
        )));
    let after = journal(&p, &res);
    assert_eq!(after.len(), before, "nothing journaled: {after:?}");
    assert_eq!(after.last().unwrap()["type"], "run_paused", "still paused");
}

/// Starts `pb` the way an older apb did (no launch record) and returns the
/// run id: a failed run that `--from-node s` resumes.
fn apb_engine_run(p: &Project, id: &str) -> String {
    // The CLI records an authorization on every start; a run without one
    // is written by the engine alone, here through the library.
    let _lock = ENV.lock().unwrap_or_else(|e| e.into_inner());
    // SAFETY: the env is changed under ENV only, for this call.
    unsafe {
        std::env::set_var("APB_CONFIG_DIR", p.cfg.path());
        std::env::set_var("APB_TEST_FAIL", p.root().join("fail"));
    }
    let res = apb_engine::scheduler::run(p.root(), id, None, Default::default()).unwrap();
    unsafe {
        std::env::remove_var("APB_TEST_FAIL");
    }
    res.run_id
}

static ENV: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// The detached driver of a resume continues a run never authorized only
/// with a grant from its own parent's handoff.
#[test]
fn a_detached_driver_resumes_only_with_its_parents_handoff() {
    let p = Project::new("pb", &pb("pb", None, ""));
    fs::write(p.root().join("fail"), "").unwrap();
    let run_id = apb_engine_run(&p, "pb");
    p.machine_agent("ask");
    fs::remove_file(p.root().join("fail")).unwrap();
    let drive = |handoff: Option<&Path>| {
        let mut c = p.apb();
        c.args(["__drive-run", "--root"]).arg(p.root()).args([
            "--run-id",
            &run_id,
            "--resume",
            "--from-node",
            "s",
        ]);
        if let Some(h) = handoff {
            c.arg("--launch-handoff").arg(h);
        }
        c.assert()
    };
    let journal = || {
        fs::read_to_string(
            p.root()
                .join(".apb/runs")
                .join(&run_id)
                .join("events.jsonl"),
        )
        .unwrap()
    };
    // Without a grant the driver refuses (its error exit, with the reason
    // journaled as `run_error` since its stdio goes nowhere).
    drive(None)
        .code(2)
        .stderr(predicate::str::contains("launch_unchecked"));
    assert!(!journal().contains("run_resumed"), "no grant, no resume");
    assert!(
        journal().contains("launch_unchecked"),
        "the refusal is visible"
    );
    // This test process is the driver's parent: its handoff is accepted.
    let grant = {
        let _lock = ENV.lock().unwrap_or_else(|e| e.into_inner());
        unsafe { std::env::set_var("APB_CONFIG_DIR", p.cfg.path()) };
        use apb_engine::gate::launch::{Initiator, LaunchConfirmation, LaunchContext, LaunchKind};
        let kind = LaunchKind::Resume {
            run_id: run_id.clone(),
        };
        let agent = || LaunchContext::new(Initiator::AgentMcp { client: None }, kind.clone());
        let r = apb_engine::resume_launch::check_resume_launch(p.root(), &run_id, &agent())
            .unwrap_err();
        apb_engine::resume_launch::check_resume_launch(
            p.root(),
            &run_id,
            &agent()
                .fixed(Some("1.0.0".into()), Some("explicit"))
                .confirmed(Some(LaunchConfirmation::HostRelayed {
                    nonce: r["launch_nonce"].as_str().unwrap().into(),
                })),
        )
        .unwrap()
        .unwrap()
    };
    let handoff = apb_engine::launch_handoff::write(
        p.root(),
        &apb_engine::launch_handoff::Handoff::Resume(grant),
    )
    .unwrap();
    drive(Some(&handoff)).success();
    assert!(!handoff.exists(), "the driver took the handoff");
    assert!(
        journal().contains("\"initiator\":\"agent_mcp\""),
        "{}",
        journal()
    );
}

#[test]
fn a_floor_is_lowered_only_by_a_person_at_a_terminal() {
    let p = Project::new("pb", &pb("pb", Some("deny"), ""));
    p.apb()
        .args(["launch", "floor", "pb"])
        .assert()
        .success()
        .stdout(predicate::str::contains("launch floor of `pb`: deny"));
    p.apb()
        .args(["launch", "floor", "pb", "--lower-to", "allow"])
        .assert()
        .code(2)
        .stderr(predicate::str::contains("interactive terminal"));
    p.apb()
        .env("APB_RUN_ID", "parent-1")
        .args(["launch", "floor", "pb", "--lower-to", "allow"])
        .assert()
        .code(2)
        .stderr(predicate::str::contains("inside run `parent-1`"));
}

/// Runs `apb <args>` on a pseudo-terminal in `p`, answering `y` to the
/// question it asks; returns the exit status and what the terminal showed.
#[cfg(target_os = "linux")]
fn on_a_terminal(p: &Project, args: &[&str], extra_env: &[(&str, &str)]) -> Option<(i32, String)> {
    crate::common::on_a_terminal(p.root(), p.cfg.path(), args, extra_env, b"y\n")
}

/// A `deny` playbook started with `--supervise` from an interactive
/// terminal: the parent asks, the detached child starts from the handoff,
/// and the run records the terminal's confirmation.
#[cfg(target_os = "linux")]
#[test]
fn an_interactive_supervised_start_of_a_deny_playbook_asks_once() {
    let p = Project::new(
        "pb",
        &pb(
            "pb",
            Some("deny"),
            "defaults:\n  profile: main\nsupervisor:\n  profile: main\n",
        ),
    );
    let pdir = p.root().join(".apb/profiles/main");
    fs::create_dir_all(&pdir).unwrap();
    fs::write(
        pdir.join("profile.yaml"),
        "name: main\ndescription: test\nexecutor:\n  agent: claude\n  model: haiku\n",
    )
    .unwrap();
    fs::write(pdir.join("SOUL.md"), "").unwrap();
    let stub = p.root().join("agent.sh");
    fs::write(&stub, "#!/bin/sh\nexit 0\n").unwrap();
    {
        use std::os::unix::fs::PermissionsExt as _;
        fs::set_permissions(&stub, fs::Permissions::from_mode(0o755)).unwrap();
    }
    let Some((code, shown)) = on_a_terminal(
        &p,
        &["run", "pb", "--supervise"],
        &[("APB_AGENT_CMD", stub.to_str().unwrap())],
    ) else {
        eprintln!("skipped: no `script` to make a pseudo-terminal");
        return;
    };
    assert_eq!(code, 0, "{shown}");
    assert!(shown.contains("Launch it? [y/N]"), "{shown}");
    assert!(shown.contains("supervised run started"), "{shown}");
    let runs = p.runs();
    assert_eq!(runs.len(), 1, "{shown}");
    let manifest = fs::read_to_string(
        p.root()
            .join(".apb/runs")
            .join(&runs[0])
            .join("manifest.yaml"),
    )
    .unwrap();
    assert!(manifest.contains("initiator: cli_terminal"), "{manifest}");
    assert!(
        manifest.contains("confirmation: terminal_prompt"),
        "{manifest}"
    );
    let doc: serde_json::Value = yaml_serde::from_str(&manifest).unwrap();
    let mut keys: Vec<&str> = doc["launch"]
        .as_object()
        .expect("a launch record")
        .keys()
        .map(String::as_str)
        .collect();
    keys.sort();
    assert_eq!(
        keys,
        [
            "billing",
            "confirmation",
            "initiator",
            "kind",
            "policy",
            "sources"
        ],
        "the record holds no nonce: {manifest}"
    );
}

/// `__drive-supervised` takes a launch decision only from a handoff its own
/// parent wrote, fresh, for the same permit: without one, with a stale one,
/// one bound to another nonce, or one another process wrote, it is the CLI
/// without a terminal, and a `deny` playbook is refused before a run exists.
#[test]
fn a_supervised_driver_without_a_valid_handoff_is_the_cli_without_a_terminal() {
    use apb_engine::gate::launch::{Choice, HandoffBinding, HandoffGrant};
    use apb_engine::launch_handoff::Handoff;
    let p = Project::new("pb", &pb("pb", Some("deny"), ""));
    let forged = |nonce: &str| Handoff::Supervise {
        grant: HandoffGrant {
            record: apb_core::launch::LaunchRecord {
                kind: "start".into(),
                initiator: "cli_terminal".into(),
                confirmation: Some("terminal_prompt".into()),
                policy: apb_core::launch::LaunchPolicy::Deny,
                sources: vec!["declared deny".into()],
                billing: None,
            },
            binding: HandoffBinding::Nonce {
                nonce: nonce.into(),
            },
            config_dir: None,
        },
        version: "1.0.0".into(),
        choice: Choice::Current,
        consent: None,
    };
    let handshake = p.root().join("handshake");
    let drive = |via_shell: bool, handoff: Option<&Path>| {
        let _ = fs::remove_file(&handshake);
        let mut args = vec![
            "__drive-supervised".to_string(),
            "pb".into(),
            "--handshake".into(),
            handshake.display().to_string(),
        ];
        if let Some(h) = handoff {
            args.push("--launch-handoff".into());
            args.push(h.display().to_string());
        }
        let mut c = if via_shell {
            // Another process writes nothing here, but spawns the driver:
            // the handoff's writer (this test) is then not its parent.
            let mut c = assert_cmd::Command::new("sh");
            let line = std::iter::once(crate::common::apb_bin().display().to_string())
                .chain(args)
                .map(|a| format!("'{a}'"))
                .collect::<Vec<_>>()
                .join(" ");
            c.args(["-c", &format!("{line}; exit $?")]);
            c
        } else {
            let mut c = apb();
            c.args(&args);
            c
        };
        c.current_dir(p.root())
            .env("APB_CONFIG_DIR", p.cfg.path())
            .env_remove("APB_RUN_ID")
            .env_remove("APB_HOST_TASK");
        let out = c.output().unwrap();
        let shake = fs::read_to_string(&handshake).unwrap_or_default();
        (out, shake)
    };
    let refused = |what: &str, (out, shake): (std::process::Output, String)| {
        assert_eq!(out.status.code(), Some(2), "{what}: {out:?}");
        assert!(shake.contains("launch_requires_person"), "{what}: {shake}");
        assert!(p.runs().is_empty(), "{what}: no run");
        String::from_utf8_lossy(&out.stderr).into_owned()
    };

    refused("no handoff", drive(false, None));

    let h = apb_engine::launch_handoff::write(p.root(), &forged("launch-forged")).unwrap();
    refused("another nonce", drive(false, Some(&h)));
    assert!(!h.exists(), "a refused handoff is deleted");

    let h = apb_engine::launch_handoff::write(p.root(), &forged("launch-forged")).unwrap();
    let err = refused("not its parent", drive(true, Some(&h)));
    assert!(
        err.contains("not written by this process's parent"),
        "{err}"
    );

    // A path outside the project's handoff directory is refused and left
    // as it is, whatever it holds (staleness is the handoff module's own
    // unit test).
    let outside = p.root().join("keep-me");
    fs::write(&outside, "not a handoff").unwrap();
    let err = refused("outside", drive(false, Some(&outside)));
    assert!(
        err.contains("not a launch handoff of this project"),
        "{err}"
    );
    assert_eq!(fs::read_to_string(&outside).unwrap(), "not a handoff");
}

/// A damaged `policy.yaml` refuses an agent channel's launch with
/// `policy_file_invalid`, naming the key it does not know (a person at a
/// terminal still launches: see the tolerant read tests below).
#[test]
fn a_damaged_settings_file_refuses_agent_launches_and_names_the_key() {
    let p = Project::new("pb", &pb("pb", None, ""));
    fs::write(p.cfg.path().join("policy.yaml"), "launch:\n  agents: ask\n").unwrap();
    let err = stderr_of(&p.apb().args(["run", "pb"]).assert().code(2));
    assert!(err.contains("policy_file_invalid"), "{err}");
    assert!(err.contains("agents"), "the unknown key is named: {err}");
    assert!(p.runs().is_empty());
}

/// The tolerant read: a person at a terminal launches past a damaged
/// project or machine `policy.yaml` with a `policy_file_invalid` warning,
/// while the CLI without a terminal is refused by the same file.
#[cfg(target_os = "linux")]
#[test]
fn a_person_at_a_terminal_launches_past_a_damaged_settings_file_with_a_warning() {
    for machine in [false, true] {
        let p = Project::new("pb", &pb("pb", None, ""));
        let file = if machine {
            p.cfg.path().join("policy.yaml")
        } else {
            p.root().join(".apb/policy.yaml")
        };
        fs::write(&file, "launch:\n  agents: ask\n").unwrap();
        let err = stderr_of(&p.apb().args(["run", "pb"]).assert().code(2));
        assert!(
            err.contains("policy_file_invalid"),
            "machine {machine}: {err}"
        );
        assert!(p.runs().is_empty(), "machine {machine}");
        let Some((code, shown)) = on_a_terminal(&p, &["run", "pb"], &[]) else {
            eprintln!("skipped: no `script` to make a pseudo-terminal");
            return;
        };
        assert_eq!(code, 0, "machine {machine}: {shown}");
        assert!(
            shown.contains("warning: policy_file_invalid")
                && shown.contains("launched without its launch settings"),
            "machine {machine}: {shown}"
        );
        assert_eq!(p.runs().len(), 1, "machine {machine}: {shown}");
    }
}

/// A damaged `launch-floors.json`: a person at a terminal launches with a
/// `launch_floor_invalid` warning, the CLI without a terminal is refused,
/// and `apb doctor` names it.
#[cfg(target_os = "linux")]
#[test]
fn a_damaged_floor_file_warns_a_person_and_refuses_an_agent_channel() {
    let p = Project::new("pb", &pb("pb", None, ""));
    fs::write(p.cfg.path().join("launch-floors.json"), "{ not json").unwrap();
    let err = stderr_of(&p.apb().args(["run", "pb"]).assert().code(2));
    assert!(err.contains("launch_floor_invalid"), "{err}");
    assert!(p.runs().is_empty());
    let doctor = p.apb().arg("doctor").output().unwrap();
    let shown = String::from_utf8_lossy(&doctor.stdout);
    assert!(
        shown.contains("launch floors") && shown.contains("launch_floor_invalid"),
        "{shown}"
    );
    let Some((code, shown)) = on_a_terminal(&p, &["run", "pb"], &[]) else {
        eprintln!("skipped: no `script` to make a pseudo-terminal");
        return;
    };
    assert_eq!(code, 0, "{shown}");
    assert!(
        shown.contains("warning: launch_floor_invalid")
            && shown.contains("launched without the launch floors"),
        "{shown}"
    );
    assert_eq!(p.runs().len(), 1, "{shown}");
}

/// `apb doctor` names the launch floor of each playbook of the project.
#[test]
fn the_doctor_names_the_launch_floors() {
    let p = Project::new("pb", &pb("pb", Some("deny"), ""));
    let out = p.apb().arg("doctor").output().unwrap();
    let shown = String::from_utf8_lossy(&out.stdout);
    let line = shown
        .lines()
        .find(|l| l.contains("launch floors"))
        .unwrap_or_else(|| panic!("no launch floors line: {shown}"));
    assert!(line.contains("pb: deny"), "{line}");
}

/// A person lowers a floor at a terminal, for a project playbook and with
/// `--global` for a global one; a step marked only by `APB_HOST_TASK` is
/// refused like one marked by `APB_RUN_ID`.
#[cfg(target_os = "linux")]
#[test]
fn a_person_at_a_terminal_lowers_a_floor_project_or_global() {
    let p = Project::new("pb", &pb("pb", Some("deny"), ""));
    p.apb()
        .env("APB_HOST_TASK", "task-1")
        .args(["launch", "floor", "pb", "--lower-to", "allow"])
        .assert()
        .code(2)
        .stderr(predicate::str::contains(
            "lowering a launch floor is refused",
        ));
    let Some((code, shown)) =
        on_a_terminal(&p, &["launch", "floor", "pb", "--lower-to", "allow"], &[])
    else {
        eprintln!("skipped: no `script` to make a pseudo-terminal");
        return;
    };
    assert_eq!(code, 0, "{shown}");
    assert!(shown.contains("Lower it? [y/N]"), "{shown}");
    assert!(
        shown.contains("launch floor of `pb` set to allow"),
        "{shown}"
    );
    p.apb()
        .args(["launch", "floor", "pb"])
        .assert()
        .success()
        .stdout(predicate::str::contains("launch floor of `pb`: allow"));

    // A global playbook, in the config dir's store.
    let g = p.cfg.path().join("playbooks/gp/1.0.0");
    fs::create_dir_all(&g).unwrap();
    fs::write(g.join("playbook.yaml"), pb("gp", Some("deny"), "")).unwrap();
    fs::write(p.cfg.path().join("playbooks/gp/current"), "1.0.0").unwrap();
    p.apb()
        .args(["launch", "floor", "gp", "--global"])
        .assert()
        .success()
        .stdout(predicate::str::contains("launch floor of `gp`: deny"));
    let (code, shown) = on_a_terminal(
        &p,
        &["launch", "floor", "gp", "--global", "--lower-to", "ask"],
        &[],
    )
    .unwrap();
    assert_eq!(code, 0, "{shown}");
    p.apb()
        .args(["launch", "floor", "gp", "--global"])
        .assert()
        .success()
        .stdout(predicate::str::contains("launch floor of `gp`: ask"));
    // The project key of the same id is untouched.
    let floors = fs::read_to_string(p.cfg.path().join("launch-floors.json")).unwrap();
    let floors: serde_json::Value = serde_json::from_str(&floors).unwrap();
    assert_eq!(floors["floors"]["global/gp"], "ask", "{floors}");
}

/// A run never authorized whose child tree no longer resolves: its resume
/// is refused with `launch_tree_unverified` before anything runs.
#[test]
fn a_resume_whose_child_tree_no_longer_resolves_is_unverified() {
    let parent = "schema: 2\nid: pp\nname: pp\nversion: 1.0.0\nnodes:\n  - { id: start, type: start }\n  - { id: sub, type: playbook, playbook: cc }\n  - { id: s, type: script, script: \"scripts/s.sh\", runner: sh }\n  - { id: done, type: finish, outcome: success }\nedges:\n  - { from: start, to: sub }\n  - { from: sub, to: s }\n  - { from: s, to: done }\n";
    let p = Project::new("pp", parent);
    let c = p.root().join(".apb/playbooks/cc/1.0.0");
    fs::create_dir_all(&c).unwrap();
    fs::write(
        c.join("playbook.yaml"),
        "schema: 2\nid: cc\nname: cc\nversion: 1.0.0\nnodes:\n  - { id: start, type: start }\n  - { id: done, type: finish, outcome: success }\nedges:\n  - { from: start, to: done }\n",
    )
    .unwrap();
    fs::write(p.root().join(".apb/playbooks/cc/current"), "1.0.0").unwrap();
    fs::write(p.root().join("fail"), "").unwrap();
    let res = apb_engine_run(&p, "pp");
    fs::remove_file(p.root().join("fail")).unwrap();
    fs::remove_dir_all(p.root().join(".apb/playbooks/cc")).unwrap();
    let before = journal(&p, &res).len();
    // A child that no longer resolves counts as irreversible: the consent
    // question comes first, and its answer meets the launch check.
    let err = stderr_of(
        &p.apb()
            .args(["resume", &res, "--from-node", "s"])
            .assert()
            .code(2),
    );
    let consent = err
        .split_whitespace()
        .find(|w| w.starts_with("--confirm-irreversible="))
        .map(|w| w.trim_end_matches([',', '.', ')']).to_string());
    let mut args = vec![
        "resume".to_string(),
        res.clone(),
        "--from-node".into(),
        "s".into(),
    ];
    args.extend(consent);
    let err = stderr_of(&p.apb().args(&args).assert().code(2));
    assert!(err.contains("launch_tree_unverified"), "{err}");
    assert_eq!(journal(&p, &res).len(), before, "nothing ran");
}

/// The machine's exact entry matches only the workspace id registered at
/// this root: another project's id, or `global`, written into
/// `.apb/workspace.local` does not let a run step start a `deny` playbook.
#[test]
fn a_forged_workspace_id_does_not_match_another_projects_machine_entry() {
    let p = Project::new("pb", &pb("pb", Some("deny"), ""));
    let other = tempfile::tempdir().unwrap();
    apb_core::registry::init_project(other.path()).unwrap();
    let (mine, theirs) = {
        let _lock = ENV.lock().unwrap_or_else(|e| e.into_inner());
        // SAFETY: the env is changed under ENV only.
        unsafe { std::env::set_var("APB_CONFIG_DIR", p.cfg.path()) };
        (
            apb_core::projects::bind_id(p.root()).unwrap(),
            apb_core::projects::bind_id(other.path()).unwrap(),
        )
    };
    let step_run = || p.apb().env("APB_RUN_ID", "r1").args(["run", "pb"]).assert();
    // The entry for this very project matches. The config dir of a test is
    // never the default one, so an entry that loosens is refused for a run
    // step there (`policy_file_invalid`, finding 1.2): proof it applied.
    fs::write(
        p.cfg.path().join("policy.yaml"),
        format!("launch:\n  playbooks:\n    \"{mine}/pb\": allow\n"),
    )
    .unwrap();
    step_run()
        .code(2)
        .stderr(predicate::str::contains("policy_file_invalid"))
        .stderr(predicate::str::contains("redirected"));
    let started = p.runs().len();
    assert_eq!(started, 0);
    // Entries for another project and for the global key do not match: the
    // playbook stays `deny` and the step is refused as a step.
    fs::write(
        p.cfg.path().join("policy.yaml"),
        format!("launch:\n  playbooks:\n    \"{theirs}/pb\": allow\n    \"global/pb\": allow\n"),
    )
    .unwrap();
    let id_file = p.root().join(".apb/workspace.local");
    for forged in [theirs.as_str(), "global"] {
        fs::write(&id_file, forged).unwrap();
        step_run()
            .code(2)
            .stderr(predicate::str::contains("launch_refused_in_run_step"));
        assert_eq!(p.runs().len(), started, "{forged}: no run");
    }
}
