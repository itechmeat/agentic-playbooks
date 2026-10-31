//! "Notify me" and the HTML report through the real `apb` binary: a start
//! that asks for notifications nobody can send is refused before a run
//! exists, `apb notify <run> --off` changes a live run and `apb runs <id>`
//! shows it, an ended run refuses the change, `apb notify test` says why
//! it cannot send, and `apb runs <id> --html` writes the page to a file or
//! standard output.

use std::fs;
use std::path::Path;

const QUICK: &str = "schema: 2\nid: quick\nname: Quick\nversion: 1.0.0\nnodes:\n  - { id: start, type: start }\n  - { id: done, type: finish, outcome: success }\nedges:\n  - { from: start, to: done }\n";

fn seed(root: &Path) {
    let out = apb_in(root, &["init"]);
    assert!(out.status.success(), "apb init failed: {out:?}");
    let vdir = root.join(".apb/playbooks/quick/1.0.0");
    fs::create_dir_all(&vdir).unwrap();
    fs::write(vdir.join("playbook.yaml"), QUICK).unwrap();
    fs::write(root.join(".apb/playbooks/quick/current"), "1.0.0").unwrap();
}

fn apb_in(root: &Path, args: &[&str]) -> std::process::Output {
    crate::common::apb_std()
        .args(args)
        .current_dir(root)
        .output()
        .unwrap()
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

fn runs(root: &Path) -> usize {
    fs::read_dir(root.join(".apb/runs")).map_or(0, |rd| rd.count())
}

/// Runs `quick` to its end in the foreground and returns its id.
fn finished_run(root: &Path) -> String {
    let out = apb_in(root, &["run", "quick"]);
    assert!(out.status.success(), "run failed: {out:?}");
    let stdout = text(&out.stdout);
    stdout
        .trim()
        .strip_prefix("run ")
        .and_then(|rest| rest.split_whitespace().next())
        .unwrap_or_else(|| panic!("unexpected output: {stdout}"))
        .to_string()
}

#[test]
fn a_start_asking_for_notifications_nobody_can_send_is_refused_before_a_run_exists() {
    let dir = tempfile::tempdir().unwrap();
    seed(dir.path());
    let out = apb_in(dir.path(), &["run", "quick", "--notify"]);
    assert_eq!(out.status.code(), Some(2), "{out:?}");
    assert!(text(&out.stderr).contains("cannot be sent"), "{out:?}");
    let out = apb_in(dir.path(), &["run", "quick", "--notify-on", "sometimes"]);
    assert_eq!(out.status.code(), Some(2));
    assert!(text(&out.stderr).contains("sometimes"));
    assert_eq!(runs(dir.path()), 0);

    // `--no-notify` needs no target.
    let out = apb_in(dir.path(), &["run", "quick", "--no-notify"]);
    assert!(out.status.success(), "{out:?}");
}

#[test]
fn notify_off_changes_a_live_run_and_an_ended_run_refuses_it() {
    let dir = tempfile::tempdir().unwrap();
    seed(dir.path());
    // A run that started and is still going.
    let run_dir = dir.path().join(".apb/runs/r-live");
    fs::create_dir_all(&run_dir).unwrap();
    let mut log = apb_engine::event::EventLog::create(&run_dir).unwrap();
    log.append(apb_engine::event::EventPayload::RunStarted {
        playbook: "quick".into(),
        version: "1.0.0".into(),
    })
    .unwrap();

    let out = apb_in(dir.path(), &["notify", "r-live"]);
    assert!(text(&out.stdout).contains("never set"), "{out:?}");
    let out = apb_in(dir.path(), &["notify", "r-live", "--on"]);
    assert_eq!(out.status.code(), Some(2), "no target anywhere: {out:?}");
    assert!(text(&out.stderr).contains("cannot be sent"));

    let out = apb_in(dir.path(), &["notify", "r-live", "--off"]);
    assert!(out.status.success(), "{out:?}");
    assert_eq!(text(&out.stdout).trim(), "notify: off");
    let out = apb_in(dir.path(), &["runs", "r-live"]);
    assert!(text(&out.stdout).contains("  notify: off"), "{out:?}");

    let ended = finished_run(dir.path());
    let out = apb_in(dir.path(), &["notify", &ended, "--off"]);
    assert_eq!(out.status.code(), Some(2));
    assert!(text(&out.stderr).contains("the run has ended"), "{out:?}");
}

#[test]
fn notify_without_a_target_says_so_and_test_cannot_send() {
    let dir = tempfile::tempdir().unwrap();
    seed(dir.path());
    let out = apb_in(dir.path(), &["notify"]);
    assert!(out.status.success(), "{out:?}");
    let stdout = text(&out.stdout);
    assert!(stdout.contains("target: none"), "{stdout}");
    assert!(stdout.contains("only when asked"));

    let out = apb_in(dir.path(), &["notify", "test"]);
    assert_eq!(out.status.code(), Some(1), "{out:?}");
    assert!(text(&out.stderr).contains("no notify target"), "{out:?}");
}

#[test]
fn runs_html_writes_the_report_page_to_a_file_or_stdout() {
    let dir = tempfile::tempdir().unwrap();
    seed(dir.path());
    let run_id = finished_run(dir.path());
    let file = dir.path().join("report.html");
    let out = apb_in(
        dir.path(),
        &["runs", &run_id, "--html", file.to_str().unwrap()],
    );
    assert!(out.status.success(), "{out:?}");
    let page = fs::read_to_string(&file).unwrap();
    assert!(page.starts_with("<!doctype html>"));
    assert!(page.contains("Content-Security-Policy"));
    assert!(!page.contains("<script"));

    let out = apb_in(dir.path(), &["runs", &run_id, "--html", "-"]);
    assert!(out.status.success());
    assert_eq!(text(&out.stdout), page);

    let out = apb_in(dir.path(), &["runs", "no-such-run", "--html", "-"]);
    assert_eq!(out.status.code(), Some(2));
}

#[test]
fn a_step_inside_a_run_cannot_change_or_test_notifications() {
    let dir = tempfile::tempdir().unwrap();
    seed(dir.path());
    let run_dir = dir.path().join(".apb/runs/r-live");
    fs::create_dir_all(&run_dir).unwrap();
    let mut log = apb_engine::event::EventLog::create(&run_dir).unwrap();
    log.append(apb_engine::event::EventPayload::RunStarted {
        playbook: "quick".into(),
        version: "1.0.0".into(),
    })
    .unwrap();
    for (var, value) in [("APB_RUN_ID", "r-live"), ("APB_HOST_TASK", "t-1")] {
        for args in [
            &["notify", "r-live", "--off"][..],
            &[
                "notify",
                "r-live",
                "--via",
                "telegram/me:send_message",
                "--to",
                "7",
            ][..],
            &[
                "notify",
                "test",
                "--via",
                "telegram/me:send_message",
                "--to",
                "7",
            ][..],
        ] {
            let out = crate::common::apb_std()
                .args(args)
                .current_dir(dir.path())
                .env(var, value)
                .output()
                .unwrap();
            assert_eq!(out.status.code(), Some(2), "{var}: {out:?}");
            assert!(
                text(&out.stderr).contains("only a person"),
                "{var}: {out:?}"
            );
        }
        // Reading them stays open.
        let out = crate::common::apb_std()
            .args(["notify", "r-live"])
            .current_dir(dir.path())
            .env(var, value)
            .output()
            .unwrap();
        assert!(out.status.success(), "{var}: {out:?}");
    }
    // Nothing was journaled.
    let events = apb_engine::event::read_all(&run_dir).unwrap();
    assert_eq!(events.len(), 1);
}

#[test]
fn notify_names_a_run_that_does_not_exist() {
    let dir = tempfile::tempdir().unwrap();
    seed(dir.path());
    let out = apb_in(dir.path(), &["notify", "no-such-run"]);
    assert_eq!(out.status.code(), Some(2), "{out:?}");
    assert!(text(&out.stderr).contains("not found"), "{out:?}");
    // A transition list needs `=`, so a run id after `--on` stays the run.
    let out = apb_in(dir.path(), &["notify", "--on", "no-such-run"]);
    assert_eq!(out.status.code(), Some(2), "{out:?}");
    assert!(text(&out.stderr).contains("not found"), "{out:?}");
}

#[test]
fn a_run_step_cannot_start_a_run_with_its_own_notify_target() {
    let dir = tempfile::tempdir().unwrap();
    seed(dir.path());
    let out = crate::common::apb_std()
        .args([
            "run",
            "quick",
            "--notify-via",
            "telegram/me:send_message",
            "--notify-to",
            "7",
        ])
        .current_dir(dir.path())
        .env("APB_RUN_ID", "r-outer")
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(2), "{out:?}");
    assert!(
        text(&out.stderr).contains("does not choose where notifications go"),
        "{out:?}"
    );
    assert_eq!(runs(dir.path()), 0);
}

#[test]
fn a_broken_machine_config_does_not_stop_turning_a_run_off() {
    let dir = tempfile::tempdir().unwrap();
    seed(dir.path());
    let run_dir = dir.path().join(".apb/runs/r-live");
    fs::create_dir_all(&run_dir).unwrap();
    let mut log = apb_engine::event::EventLog::create(&run_dir).unwrap();
    log.append(apb_engine::event::EventPayload::RunStarted {
        playbook: "quick".into(),
        version: "1.0.0".into(),
    })
    .unwrap();
    let cfg = tempfile::tempdir().unwrap();
    fs::write(cfg.path().join("config.yaml"), "notify: [not, a, map]\n").unwrap();
    let apb = |args: &[&str]| {
        crate::common::apb_std()
            .args(args)
            .current_dir(dir.path())
            .env("APB_CONFIG_DIR", cfg.path())
            .output()
            .unwrap()
    };
    let out = apb(&["notify", "r-live", "--off"]);
    assert!(out.status.success(), "{out:?}");
    assert_eq!(text(&out.stdout).trim(), "notify: off");
}
