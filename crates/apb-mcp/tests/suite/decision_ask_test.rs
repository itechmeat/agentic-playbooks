//! `decision_ask` (issue #193) through the tool layer, outside a run: the
//! answer shapes of the item kinds, the project log, the per-day cap, and the
//! refusals with their reasons. The in-run path (journal, run budget,
//! playbook opt-out) is covered in apb-engine's host mode tests.

use std::path::Path;

use apb_core::registry::init_project;
use apb_mcp::tools::{DecisionAskInput, decision_ask};
use serde_json::Value;

use crate::common::env_lock as lock;

const VARS: [&str; 3] = [
    "APB_CONFIG_DIR",
    "APB_DECISIONS_ALLOW_FAKE",
    "APB_DECISIONS",
];

struct Env(Vec<(&'static str, Option<std::ffi::OsString>)>);

impl Env {
    fn new(cfg: &Path) -> Self {
        let saved = VARS.iter().map(|k| (*k, std::env::var_os(k))).collect();
        unsafe {
            std::env::set_var("APB_CONFIG_DIR", cfg);
            std::env::set_var("APB_DECISIONS_ALLOW_FAKE", "1");
            std::env::remove_var("APB_DECISIONS");
        }
        Env(saved)
    }
}

impl Drop for Env {
    fn drop(&mut self) {
        for (k, v) in &self.0 {
            unsafe {
                match v {
                    Some(v) => std::env::set_var(k, v),
                    None => std::env::remove_var(k),
                }
            }
        }
    }
}

/// A fake provider: item 0 fails the question, items 1 and 2 pass.
fn config(cfg: &Path, extra: &str) {
    std::fs::write(
        cfg.join("decisions.yaml"),
        format!(
            "mode: advise\nproviders:\n  - id: fake\n    kind: fake\n    answers:\n      item_0: {{ type: noul, noul: 0.2 }}\n      item_1: {{ type: noul, noul: 0.9 }}\n      item_2: {{ type: noul, noul: 0.6 }}\n{extra}"
        ),
    )
    .unwrap();
}

fn filter(items: &[&str]) -> DecisionAskInput {
    DecisionAskInput {
        kind: "filter".into(),
        question: "Is this photo usable as a cover?".into(),
        items: items.iter().map(|s| s.to_string()).collect(),
        ..Default::default()
    }
}

fn project() -> tempfile::TempDir {
    let root = tempfile::tempdir().unwrap();
    init_project(root.path()).unwrap();
    root
}

#[test]
fn filter_and_rank_answer_per_item_and_log_outside_a_run() {
    let _l = lock();
    let (root, cfg) = (project(), tempfile::tempdir().unwrap());
    config(cfg.path(), "");
    let _env = Env::new(cfg.path());
    let out = decision_ask(root.path(), filter(&["blurry", "sharp", "ok"])).unwrap();
    assert_eq!(out["answered"], true, "{out}");
    assert_eq!(out["kept"], serde_json::json!(["sharp", "ok"]));
    assert_eq!(out["provider"], "fake");
    let mut rank = filter(&["blurry", "sharp", "ok"]);
    rank.kind = "rank".into();
    let out = decision_ask(root.path(), rank).unwrap();
    let order: Vec<&str> = out["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["item"].as_str().unwrap())
        .collect();
    assert_eq!(order, ["sharp", "ok", "blurry"]);
    let log = std::fs::read_to_string(root.path().join(".apb/decisions.jsonl")).unwrap();
    let lines: Vec<Value> = log
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    assert_eq!(lines.len(), 2);
    assert!(lines.iter().all(|l| l["use_site"] == "host_task"));
}

#[test]
fn refusals_name_their_reason() {
    let _l = lock();
    let (root, cfg) = (project(), tempfile::tempdir().unwrap());
    let _env = Env::new(cfg.path());
    let refused = |input| {
        let out = decision_ask(root.path(), input).unwrap();
        assert_eq!(out["answered"], false, "{out}");
        assert!(
            out["reason"].as_str().is_some_and(|r| !r.is_empty()),
            "{out}"
        );
        out["refused"].as_str().unwrap_or_default().to_string()
    };
    assert_eq!(refused(filter(&["a"])), "no_provider");
    config(
        cfg.path(),
        "uses: { host_task: { mode: advise, max_requests_per_day: 1 } }\n",
    );
    assert_eq!(
        decision_ask(root.path(), filter(&["a"])).unwrap()["answered"],
        true
    );
    assert_eq!(refused(filter(&["b"])), "budget");
    unsafe { std::env::set_var("APB_DECISIONS", "off") };
    assert_eq!(refused(filter(&["c"])), "off");
    unsafe { std::env::remove_var("APB_DECISIONS") };
    config(cfg.path(), "uses: { host_task: { mode: off } }\n");
    assert_eq!(refused(filter(&["d"])), "use_off");
    // A malformed request and an unknown kind are errors, not refusals.
    let out = decision_ask(root.path(), filter(&[])).unwrap();
    assert_eq!(out["error"], "invalid_request");
    let mut bad = filter(&["a"]);
    bad.kind = "guess".into();
    assert_eq!(
        decision_ask(root.path(), bad).unwrap()["error"],
        "unknown_kind"
    );
}

#[test]
fn an_unknown_run_is_not_found() {
    let _l = lock();
    let (root, cfg) = (project(), tempfile::tempdir().unwrap());
    config(cfg.path(), "");
    let _env = Env::new(cfg.path());
    let mut input = filter(&["a"]);
    input.run_id = Some("no-such-run".into());
    assert!(decision_ask(root.path(), input).is_err());
}
