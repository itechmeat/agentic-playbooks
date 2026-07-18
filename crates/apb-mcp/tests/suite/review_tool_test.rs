use apb_mcp::tools::review_decide;
use std::fs;

#[test]
fn review_decide_writes_reviews_channel() {
    let dir = tempfile::tempdir().unwrap();
    let run_dir = dir.path().join(".apb/runs/r1");
    fs::create_dir_all(&run_dir).unwrap();

    let res = review_decide(dir.path(), "r1", "gate", "approved", "lgtm").unwrap();
    assert!(res["posted_seq"].is_number());

    let channel = fs::read_to_string(run_dir.join("reviews.jsonl")).unwrap();
    let line: serde_json::Value = serde_json::from_str(channel.lines().next().unwrap()).unwrap();
    assert_eq!(line["node"], "gate");
    assert_eq!(line["decision"], "approved");
    assert_eq!(line["note"], "lgtm");
}

#[test]
fn review_decide_unknown_run_is_not_found() {
    let dir = tempfile::tempdir().unwrap();
    assert!(review_decide(dir.path(), "ghost", "gate", "approved", "").is_err());
}

#[test]
fn review_decide_rejects_path_traversal() {
    let dir = tempfile::tempdir().unwrap();
    assert!(review_decide(dir.path(), "../evil", "gate", "approved", "").is_err());
}
