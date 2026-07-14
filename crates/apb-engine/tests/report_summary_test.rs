use apb_engine::error::EngineError;
use apb_engine::event::{EventLog, EventPayload, WakeTrigger};
use apb_engine::{supervisor_report_or_summary, write_supervisor_report};

fn run_dir(root: &std::path::Path, run_id: &str) -> std::path::PathBuf {
    root.join(".apb/runs").join(run_id)
}

#[test]
fn traversal_and_missing_run_are_not_found() {
    let dir = tempfile::tempdir().unwrap();

    let err = supervisor_report_or_summary(dir.path(), "../../etc").unwrap_err();
    assert!(
        matches!(err, EngineError::NotFound(_)),
        "expected NotFound, got {err:?}"
    );

    let err = supervisor_report_or_summary(dir.path(), "no-such-run").unwrap_err();
    assert!(
        matches!(err, EngineError::NotFound(_)),
        "expected NotFound, got {err:?}"
    );
}

#[test]
fn submitted_report_is_returned_verbatim() {
    let dir = tempfile::tempdir().unwrap();
    let rd = run_dir(dir.path(), "r1");
    EventLog::create(&rd).unwrap();
    write_supervisor_report(dir.path(), "r1", "MY REPORT").unwrap();

    let report = supervisor_report_or_summary(dir.path(), "r1").unwrap();
    assert_eq!(report, "MY REPORT");
}

#[test]
fn summary_is_built_from_wakes_and_interventions_when_no_report_submitted() {
    let dir = tempfile::tempdir().unwrap();
    let rd = run_dir(dir.path(), "r1");
    let mut log = EventLog::create(&rd).unwrap();
    log.append(EventPayload::RunStarted {
        playbook: "w".into(),
        version: "1.0.0".into(),
    })
    .unwrap();
    log.append(EventPayload::WakeRaised {
        trigger: WakeTrigger::NodeFailed,
        node: "impl".into(),
        detail: "boom".into(),
    })
    .unwrap();
    log.append(EventPayload::SupervisorAction {
        action: "node_retry".into(),
        node: Some("impl".into()),
        detail: "retry".into(),
    })
    .unwrap();
    log.append(EventPayload::RunFinished {
        outcome: "succeeded".into(),
    })
    .unwrap();

    let summary = supervisor_report_or_summary(dir.path(), "r1").unwrap();
    assert!(
        summary.contains("node_failed"),
        "summary should mention the wake trigger: {summary}"
    );
    assert!(
        summary.contains("impl"),
        "summary should mention the node: {summary}"
    );
    assert!(
        summary.contains("node_retry"),
        "summary should mention the intervention action: {summary}"
    );
    assert!(
        summary.contains("succeeded"),
        "summary should mention the run status: {summary}"
    );
}

#[test]
fn summary_omits_empty_sections_when_no_wakes_or_interventions() {
    let dir = tempfile::tempdir().unwrap();
    let rd = run_dir(dir.path(), "r1");
    let mut log = EventLog::create(&rd).unwrap();
    log.append(EventPayload::RunStarted {
        playbook: "w".into(),
        version: "1.0.0".into(),
    })
    .unwrap();
    log.append(EventPayload::RunFinished {
        outcome: "succeeded".into(),
    })
    .unwrap();

    let summary = supervisor_report_or_summary(dir.path(), "r1").unwrap();
    assert!(
        summary.contains("succeeded"),
        "summary should mention the run status: {summary}"
    );
    assert!(
        !summary.contains("## Wakes"),
        "should not render an empty wakes section: {summary}"
    );
    assert!(
        !summary.contains("## Interventions"),
        "should not render an empty interventions section: {summary}"
    );
}
