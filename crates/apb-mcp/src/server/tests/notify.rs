//! "Notify me" and the HTML report on the run tools: a start that asks for
//! notifications nobody can send is refused before a run exists, a person
//! turns a live run's notifications off with `run_notify` and `run_status`
//! shows it, an ended run refuses the change, and `run_report_html` writes
//! the page next to the run and answers with its path only.

use super::*;
use crate::tools::notify::NotifyArg;

const FINISH_ONLY: &str = "schema: 2\nid: quick\nname: Quick\nversion: 1.0.0\nnodes:\n  - { id: start, type: start }\n  - { id: done, type: finish, outcome: success }\nedges:\n  - { from: start, to: done }\n";

const WITH_GATE: &str = "schema: 2\nid: quick\nname: Quick\nversion: 1.0.0\nnodes:\n  - { id: start, type: start }\n  - { id: gate, type: human_review, title: Ship it }\n  - { id: done, type: finish, outcome: success }\nedges:\n  - { from: start, to: gate }\n  - { from: gate, to: done }\n";

fn seed_quick(root: &Path, yaml: &str) {
    apb_core::registry::init_project(root).unwrap();
    let vdir = root.join(".apb/playbooks/quick/1.0.0");
    fs::create_dir_all(&vdir).unwrap();
    fs::write(vdir.join("playbook.yaml"), yaml).unwrap();
    fs::write(root.join(".apb/playbooks/quick/current"), "1.0.0").unwrap();
}

fn start_args(notify: serde_json::Value) -> PlaybookRunArgs {
    PlaybookRunArgs {
        id: "quick".into(),
        version: None,
        params: BTreeMap::new(),
        instruction: None,
        supervise: None,
        background: None,
        acknowledge_untrusted: Some(true),
        confirm_irreversible: None,
        scope: None,
        continued_from: None,
        worktree: None,
        execution: None,
        budget: None,
        remember: None,
        notify: Some(serde_json::from_value::<NotifyArg>(notify).unwrap()),
    }
}

fn json_of(res: &CallToolResult) -> serde_json::Value {
    serde_json::from_str(&result_text(res)).unwrap()
}

fn run_ids(root: &Path) -> Vec<String> {
    fs::read_dir(root.join(".apb/runs"))
        .map(|rd| {
            rd.filter_map(|e| e.ok())
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .collect()
        })
        .unwrap_or_default()
}

/// Runs `quick` in the foreground to its end and returns the run id.
fn finished_run(root: &Path) -> String {
    let res = apb_engine::run(root, "quick", None, apb_engine::RunOptions::default()).unwrap();
    res.run_id
}

#[tokio::test]
async fn a_start_that_asks_for_notifications_without_a_target_is_refused_before_a_run_exists() {
    let _l = CROSS_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let _cfg = ConfigDirGuard::new();
    let proj = tempfile::tempdir().unwrap();
    seed_quick(proj.path(), FINISH_ONLY);
    let server = WfMcp::new(proj.path().to_path_buf());

    let out = json_of(
        &server
            .playbook_run(Parameters(start_args(json!(true))))
            .await,
    );
    assert_eq!(out["error"], "notify_unavailable", "{out}");
    assert!(out["detail"].as_str().unwrap().contains("notify target"));
    assert!(run_ids(proj.path()).is_empty(), "no run was created");

    let bad = json_of(
        &server
            .playbook_run(Parameters(start_args(json!({ "on": ["sometimes"] }))))
            .await,
    );
    assert_eq!(bad["error"], "invalid_notify");
    assert!(run_ids(proj.path()).is_empty());

    // Turning notifications off needs no target, and the run says nothing
    // about notifications it never had.
    let off = json_of(
        &server
            .playbook_run(Parameters(start_args(json!(false))))
            .await,
    );
    let run_id = off["run_id"].as_str().expect("the run started").to_string();
    let status = json_of(
        &server
            .run_status(Parameters(RunRefArgs {
                run_id,
                workspace: None,
            }))
            .await,
    );
    assert!(status.get("notify").is_none(), "{status}");
}

#[tokio::test]
async fn run_notify_turns_a_live_run_off_and_refuses_an_ended_one() {
    let _l = CROSS_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let _cfg = ConfigDirGuard::new();
    let proj = tempfile::tempdir().unwrap();
    seed_quick(proj.path(), WITH_GATE);
    let root = proj.path().to_path_buf();
    let driver = {
        let root = root.clone();
        std::thread::spawn(move || {
            apb_engine::run(&root, "quick", None, apb_engine::RunOptions::default()).unwrap()
        })
    };
    // The run waits at its review gate.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
    let run_id = loop {
        let waiting = run_ids(&root).into_iter().find(|id| {
            apb_engine::event::read_all_lossy_tail(&root.join(".apb/runs").join(id))
                .unwrap_or_default()
                .iter()
                .any(|e| {
                    matches!(
                        e.payload,
                        apb_engine::event::EventPayload::ReviewRequested { .. }
                    )
                })
        });
        if let Some(id) = waiting {
            break id;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "the gate never opened"
        );
        std::thread::sleep(std::time::Duration::from_millis(50));
    };
    let server = WfMcp::new(root.clone());
    let notify = |v: serde_json::Value| RunNotifyArgs {
        run_id: run_id.clone(),
        notify: serde_json::from_value(v).unwrap(),
        workspace: None,
    };

    // Asking for notifications with no target anywhere changes nothing.
    let refused = json_of(&server.run_notify(Parameters(notify(json!(true)))).await);
    assert_eq!(refused["error"], "notify_unavailable");

    let off = json_of(&server.run_notify(Parameters(notify(json!(false)))).await);
    assert_eq!(off["notify"]["active"], false, "{off}");
    let status = json_of(
        &server
            .run_status(Parameters(RunRefArgs {
                run_id: run_id.clone(),
                workspace: None,
            }))
            .await,
    );
    assert_eq!(status["notify"]["active"], false);
    assert_eq!(status["notify"]["sent"], json!([]));

    apb_engine::review::post_review(
        &root.join(".apb/runs").join(&run_id),
        apb_engine::review::ReviewCommand {
            node: "gate".into(),
            decision: "approve".into(),
            note: String::new(),
        },
    )
    .unwrap();
    driver.join().unwrap();

    let ended = json_of(&server.run_notify(Parameters(notify(json!(false)))).await);
    assert_eq!(ended["error"], "run_ended", "{ended}");

    let missing = server
        .run_notify(Parameters(RunNotifyArgs {
            run_id: "no-such-run".into(),
            notify: crate::tools::notify::NotifyChangeArg::Flag(false),
            workspace: None,
        }))
        .await;
    assert_eq!(missing.is_error, Some(true));
}

#[tokio::test]
async fn run_report_html_writes_the_page_next_to_the_run_and_answers_with_its_path() {
    let _l = CROSS_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let _cfg = ConfigDirGuard::new();
    let proj = tempfile::tempdir().unwrap();
    seed_quick(proj.path(), FINISH_ONLY);
    let run_id = finished_run(proj.path());
    let server = WfMcp::new(proj.path().to_path_buf());
    let report = || RunRefArgs {
        run_id: run_id.clone(),
        workspace: None,
    };

    let res = server.run_report_html(Parameters(report())).await;
    let text = result_text(&res);
    assert!(
        !text.contains("<html") && !text.contains("<!doctype"),
        "{text}"
    );
    let out: serde_json::Value = serde_json::from_str(&text).unwrap();
    let path = std::path::PathBuf::from(out["path"].as_str().unwrap());
    assert_eq!(
        path,
        proj.path()
            .join(".apb/runs")
            .join(&run_id)
            .join("report.html")
    );
    let page = fs::read_to_string(&path).unwrap();
    assert_eq!(out["bytes"], page.len());
    assert!(page.starts_with("<!doctype html>"));
    assert!(!page.contains("<script"));

    let as_report = |format: Option<serde_json::Value>| RunReportArgs {
        run_id: run_id.clone(),
        workspace: None,
        format,
    };
    let json_report = json_of(&server.run_report(Parameters(as_report(None))).await);
    assert_eq!(json_report["run_status"], "succeeded");
    // A host that still asks run_report for the page is told where it went.
    let old = json_of(
        &server
            .run_report(Parameters(as_report(Some(json!("html")))))
            .await,
    );
    assert_eq!(old["error"], "unknown_format");
    assert!(old["detail"].as_str().unwrap().contains("run_report_html"));
}
