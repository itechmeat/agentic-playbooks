//! MCP `playbook_stats`: the report `apb stats --json` prints, interventions
//! included, over the runs apb created in the workspace.

use std::fs;
use std::path::Path;

use apb_mcp::tools::ToolError;
use apb_mcp::tools::stats::playbook_stats;
use serde_json::{Value, json};

fn write_run(root: &Path, id: &str, version: &str, rest: &[Value]) {
    let dir = root.join(".apb/runs").join(id);
    fs::create_dir_all(&dir).unwrap();
    let mut lines = vec![
        json!({"type": "run_started", "playbook": "p", "version": version}),
        json!({"type": "run_provenance", "apb": "1.0.0-beta.3"}),
    ];
    lines.extend(rest.iter().cloned());
    lines.push(json!({"type": "run_finished", "outcome": "succeeded"}));
    let body: String = lines
        .iter()
        .enumerate()
        .map(|(i, l)| {
            let mut l = l.clone();
            l["seq"] = json!(i + 1);
            l["ts"] = json!(i * 1000);
            format!("{l}\n")
        })
        .collect();
    fs::write(dir.join("events.jsonl"), body).unwrap();
    apb_core::run_origin::ensure_key().unwrap();
    apb_core::run_origin::stamp(&dir, id).unwrap();
}

#[test]
fn playbook_stats_reports_interventions_like_the_engine() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    write_run(
        root,
        "r1",
        "1.0.0",
        &[
            json!({"type": "review_requested", "node": "g", "options": ["approve"]}),
            json!({"type": "review_decided", "node": "g", "decision": "approve", "note": "", "via": "terminal"}),
        ],
    );
    write_run(
        root,
        "r2",
        "1.0.0",
        &[json!({"type": "run_paused", "reason": "x", "by": "operator"})],
    );
    let out = playbook_stats(root, Some("p".into()), None, None).unwrap();
    let engine = apb_engine::run_stats::stats(
        &[root.to_path_buf()],
        &apb_engine::run_stats::StatsFilter {
            playbook: Some("p".into()),
            ..Default::default()
        },
    );
    assert_eq!(out, serde_json::to_value(&engine).unwrap());
    let i = &out["versions"][0]["interventions"];
    assert_eq!(i["total"]["value"], 2);
    assert_eq!(i["without_any"]["value"], 0);
    // An unknown playbook gives the empty report.
    let none = playbook_stats(root, Some("other".into()), None, None).unwrap();
    assert_eq!(none["runs"], 0);
}

/// A secret param value that reaches the report through a finding location
/// is redacted, with the run's own redactor, like every other answer.
#[test]
fn playbook_stats_redacts_a_secret_param_in_a_finding_location() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    let secret = "correct-horse-staple";
    let dir = root.join(".apb/runs").join("r1");
    fs::create_dir_all(&dir).unwrap();
    // The run's snapshot declares the param secret; the value sits in the
    // run config and is echoed in a finding location.
    fs::write(
        dir.join("playbook.yaml"),
        "schema: 2\nid: p\nname: p\nversion: 1.0.0\nparams:\n  - { name: token, type: text, secret: true }\nnodes:\n  - { id: start, type: start }\n  - { id: done, type: finish, outcome: success }\nedges:\n  - { from: start, to: done }\n",
    )
    .unwrap();
    apb_engine::run_config::write_run_config(
        &dir,
        &apb_engine::run_config::RunConfig {
            params: [("token".to_string(), secret.to_string())]
                .into_iter()
                .collect(),
            ..Default::default()
        },
    )
    .unwrap();
    write_run(
        root,
        "r1",
        "1.0.0",
        &[json!({"type": "attempt_notes", "node": "w", "attempt": 1,
                 "findings": [{"kind": "security", "location": format!("src/{secret}.rs"), "evidence": "", "why_not_fixed": ""}]})],
    );
    let out = playbook_stats(root, Some("p".into()), None, None).unwrap();
    let text = serde_json::to_string(&out).unwrap();
    assert!(!text.contains(secret), "{text}");
    let location = out["versions"][0]["findings"]["top"][0]["location"]
        .as_str()
        .unwrap_or("");
    assert!(
        location.starts_with("src/") && location.ends_with(".rs"),
        "the finding survives redacted: {location}"
    );
}

#[test]
fn playbook_stats_refuses_bad_arguments() {
    let tmp = tempfile::tempdir().unwrap();
    let e = playbook_stats(tmp.path(), None, None, Some("1.0.0".into())).unwrap_err();
    assert!(matches!(e, ToolError::Conflict(_)), "{e:?}");
    let e = playbook_stats(tmp.path(), None, Some("yesterday-ish"), None).unwrap_err();
    assert!(matches!(e, ToolError::Conflict(_)), "{e:?}");
}
