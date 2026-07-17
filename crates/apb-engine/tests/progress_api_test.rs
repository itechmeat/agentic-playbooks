use apb_engine::list_runs;

#[test]
fn run_summary_includes_progress_field() {
    let tmp = tempfile::tempdir().unwrap();
    let run_dir = tmp.path().join(".apb/runs/r1");
    std::fs::create_dir_all(&run_dir).unwrap();
    std::fs::write(
        run_dir.join("playbook.yaml"),
        "schema: 2\nid: p\nname: p\nversion: 1.0.0\ndefaults: { profile: x }\nnodes:\n  - { id: s, type: start }\n  - { id: a, type: agent_task, prompt: hi, expected_duration: 100 }\n  - { id: f, type: finish, outcome: success }\nedges:\n  - { from: s, to: a }\n  - { from: a, to: f }\n",
    )
    .unwrap();
    std::fs::write(
        run_dir.join("events.jsonl"),
        "{\"seq\":0,\"ts\":0,\"type\":\"run_started\",\"playbook\":\"p\",\"version\":\"1.0.0\"}\n{\"seq\":1,\"ts\":0,\"type\":\"node_finished\",\"node\":\"a\",\"status\":\"succeeded\",\"attempt\":1,\"output\":\"\"}\n",
    )
    .unwrap();
    let runs = list_runs(tmp.path()).unwrap();
    let r = runs.iter().find(|r| r.run_id == "r1").unwrap();
    let p = r.progress.as_ref().expect("progress present");
    assert_eq!(p.percent, 100);
}
