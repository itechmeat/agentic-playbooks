//! `run_status` and `run_report` carry a run's lists: the status the counts
//! and at most 50 items that need a look, the report at most 500 items.

use std::fs;
use std::path::Path;

use apb_mcp::tools::{run_report, run_status};
use serde_json::json;

const PLAYBOOK: &str = r#"
schema: 2
id: lists
name: lists
version: 1.0.0
defaults: { profile: x }
nodes:
  - { id: start, type: start }
  - id: rows
    type: agent_task
    title: Check each row
    prompt: Check the row in the item section.
    list:
      items: { inline: [ { id: a } ] }
      item_outputs: [ok]
      check: { output: { field: ok, equals: "1" } }
  - { id: done, type: finish, outcome: success }
edges:
  - { from: start, to: rows }
  - { from: rows, to: done }
"#;

/// A run of `rows` with `total` items: 5 passed, the rest need a look.
fn seed(root: &Path, total: usize) {
    let dir = root.join(".apb/runs/r1");
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("playbook.yaml"), PLAYBOOK).unwrap();
    let mut events = vec![
        json!({ "type": "list_started", "node": "rows", "total": total,
        "items_digest": "d", "batch_size": total, "parallel": 1 }),
    ];
    events.extend((0..total).map(|i| {
        let (state, reason) = if i < 5 {
            ("passed", None)
        } else {
            ("needs_look", Some("failed"))
        };
        json!({ "type": "list_item_settled", "node": "rows", "item": format!("i{i:03}"),
                "state": state, "reason": reason, "round": 0 })
    }));
    let journal: String = events
        .iter()
        .enumerate()
        .map(|(i, e)| {
            let mut e = e.clone();
            e["seq"] = json!(i + 1);
            e["ts"] = json!(i);
            format!("{e}\n")
        })
        .collect();
    fs::write(dir.join("events.jsonl"), journal).unwrap();
}

#[test]
fn the_status_names_at_most_fifty_items_that_need_a_look_and_the_report_all() {
    let tmp = tempfile::tempdir().unwrap();
    seed(tmp.path(), 65);
    let status = run_status(tmp.path(), "r1").unwrap();
    let list = &status["lists"][0];
    assert_eq!(list["node"], "rows");
    assert_eq!(list["title"], "Check each row");
    assert_eq!(
        (list["total"].as_u64(), list["needs_look"].as_u64()),
        (Some(65), Some(60))
    );
    let items = list["items"].as_array().unwrap();
    assert_eq!(items.len(), 50);
    assert!(items.iter().all(|i| i["state"] == "needs_look"));
    assert_eq!(list["items_truncated"], 10, "the needs-look items left out");

    let report = run_report(tmp.path(), "r1").unwrap();
    assert_eq!(report["lists"][0]["items"].as_array().unwrap().len(), 65);
    assert_eq!(report["outcome"]["lists"][0]["needs_look"], 60);
    assert_eq!(report["outcome"]["needs_look"]["list_items"], 60);
    assert!(report["lists"][0].get("items_truncated").is_none());
}

#[test]
fn the_report_names_at_most_five_hundred_items_and_counts_the_rest() {
    let tmp = tempfile::tempdir().unwrap();
    seed(tmp.path(), 520);
    let report = run_report(tmp.path(), "r1").unwrap();
    let list = &report["lists"][0];
    assert_eq!(list["items"].as_array().unwrap().len(), 500);
    assert_eq!(list["items_truncated"], 20);
    assert_eq!(list["total"], 520);
}

#[test]
fn a_run_without_a_list_carries_no_lists_block() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join(".apb/runs/r2");
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("playbook.yaml"), PLAYBOOK).unwrap();
    fs::write(
        dir.join("events.jsonl"),
        "{\"seq\":1,\"ts\":0,\"type\":\"run_started\",\"playbook\":\"lists\",\"version\":\"1.0.0\"}\n",
    )
    .unwrap();
    assert!(run_status(tmp.path(), "r2").unwrap().get("lists").is_none());
}
