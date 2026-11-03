//! The routes of the shipped `apb-feature-delivery` playbook, run end to end
//! on the real definition from `bundled/` with a stub agent: the single
//! implementation when nothing is decomposed (or the answer is unreadable),
//! the subtask list in order when it is, the `join: all` on
//! `candidate_ready` waiting on a branch that never ran, and the subtask
//! list's `needs_look` and empty-list exits to `reconsider`. The stub writes
//! per node (and per item) the outputs a test plans; no model is called.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use apb_core::registry::init_project;
use apb_engine::event::{Event, EventPayload};
use apb_engine::scheduler::{RunOptions, run};

use crate::common;
use crate::list_run_test::{Env, events, git_tree};

const ID: &str = "apb-feature-delivery";

fn shipped_yaml() -> String {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../bundled/playbooks")
        .join(ID)
        .join("playbook.yaml");
    fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
}

fn stub_dir(root: &Path) -> PathBuf {
    root.join("stub")
}

/// Seeds the project with the shipped playbook, the two role profiles it
/// binds and a git tree, and returns the stub agent's path.
fn seed(root: &Path) -> String {
    init_project(root).unwrap();
    // The launch policy is not under test here: the copy runs as `allow`.
    let yaml = shipped_yaml().replace("agent_launch: ask", "agent_launch: allow");
    let version = yaml
        .lines()
        .find_map(|l| l.strip_prefix("version: "))
        .expect("the shipped playbook names its version")
        .trim()
        .to_string();
    let vdir = root.join(".apb/playbooks").join(ID).join(&version);
    fs::create_dir_all(&vdir).unwrap();
    fs::write(vdir.join("playbook.yaml"), &yaml).unwrap();
    fs::write(
        root.join(".apb/playbooks").join(ID).join("current"),
        version,
    )
    .unwrap();
    common::seed_profile(root, "apb-main", "claude", "haiku", &[]);
    common::seed_profile(root, "apb-reviewer", "claude", "haiku", &[]);
    git_tree(root);
    stub(root)
}

/// The stub agent: it logs `node` or `node:item` per invocation and writes
/// as its outputs the JSON in `out-<node>-<item>-<n>`, `out-<node>-<item>`
/// or `out-<node>` (the first that exists, else `{}`), where `n` counts
/// this node and item's runs.
fn stub(root: &Path) -> String {
    let d = stub_dir(root);
    fs::create_dir_all(&d).unwrap();
    let body = format!(
        r#"#!/bin/sh
D='{d}'
K="$APB_NODE_ID"
[ -n "$APB_ITEM_ID" ] && K="$APB_NODE_ID-$APB_ITEM_ID"
N=$(cat "$D/n-$K" 2>/dev/null || echo 0); N=$((N+1)); echo $N > "$D/n-$K"
echo "$K" >> "$D/log"
O=$(cat "$D/out-$K-$N" 2>/dev/null || cat "$D/out-$K" 2>/dev/null || cat "$D/out-$APB_NODE_ID" 2>/dev/null || echo '{{}}')
printf '{{"status":"success","outputs":%s}}' "$O" > "$APB_STATUS_FILE"
printf 'DELIVERY REPORT: stub\n```yaml\nstatus: success\nsummary: ok\n```\n'
"#,
        d = d.display()
    );
    let path = root.join("agent.sh");
    common::write_sync(&path, &body);
    let mut p = fs::metadata(&path).unwrap().permissions();
    p.set_mode(0o755);
    fs::set_permissions(&path, p).unwrap();
    path.to_string_lossy().into_owned()
}

fn plan(root: &Path, key: &str, outputs: &str) {
    fs::write(stub_dir(root).join(format!("out-{key}")), outputs).unwrap();
}

/// The outputs every route shares: a ready design and a clean panel.
fn plan_common(root: &Path) {
    plan(root, "intake", r#"{"expected":["a"],"checks":"none"}"#);
    plan(root, "design", r#"{"ready":true,"design":"d"}"#);
    plan(
        root,
        "implement",
        r#"{"changed_paths":[],"evidence":"e","gaps":""}"#,
    );
    for role in ["review_correctness", "review_security", "review_tests"] {
        plan(root, role, r#"{"role":"r","blocking":0,"findings":[]}"#);
    }
    plan(
        root,
        "collect",
        r#"{"open_count":0,"findings":[],"roles":[]}"#,
    );
    plan(root, "report", r#"{"outcomes":"DELIVERY REPORT: ok"}"#);
}

const SUBTASKS: &str = r#"[{"id":"s1","summary":"one","depends_on":[]},{"id":"s2","summary":"two","depends_on":["s1"]}]"#;

fn subtask_pass() -> &'static str {
    r#"{"changed_paths":[],"evidence":"e","review":"r","gaps":"","self_check":"pass"}"#
}

/// Runs the playbook to its end and returns its journal and the stub log.
fn drive(root: &Path, agent: &str) -> (Vec<Event>, Vec<String>) {
    let _env = Env::set(agent, &[]);
    let opts = RunOptions {
        params: [
            ("task".to_string(), "add a flag".to_string()),
            ("change_kind".to_string(), "feature".to_string()),
        ]
        .into_iter()
        .collect(),
        ..RunOptions::default()
    };
    let result = run(root, ID, None, opts).unwrap();
    let log = fs::read_to_string(stub_dir(root).join("log")).unwrap_or_default();
    (
        events(root, &result.run_id),
        log.lines().map(str::to_string).collect(),
    )
}

/// The nodes that started or that an edge reached (a finish without a
/// prompt journals no start), in journal order, each once.
fn started(events: &[Event]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for e in events {
        let node = match &e.payload {
            EventPayload::NodeStarted { node, .. } => node,
            EventPayload::EdgeTraversed { to, .. } => to,
            _ => continue,
        };
        if !out.contains(node) {
            out.push(node.clone());
        }
    }
    out
}

fn ran(events: &[Event], node: &str) -> bool {
    started(events).iter().any(|n| n == node)
}

#[test]
fn no_trigger_takes_the_single_implementation_and_the_join_does_not_wait_on_subtasks() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let agent = seed(root);
    plan_common(root);
    plan(
        root,
        "decompose",
        r#"{"triggers":"none","decomposed":false,"subtasks":[]}"#,
    );
    let (ev, _) = drive(root, &agent);
    assert!(ran(&ev, "implement"), "{:?}", started(&ev));
    assert!(!ran(&ev, "implement_subtasks"), "{:?}", started(&ev));
    // The join on candidate_ready counts the never-run subtask branch as
    // satisfied: the panel and the report ran.
    for node in [
        "candidate_ready",
        "review_correctness",
        "collect",
        "report",
        "assessed",
    ] {
        assert!(ran(&ev, node), "{node} did not run: {:?}", started(&ev));
    }
    assert!(!ran(&ev, "reconsider"));
}

#[test]
fn an_unreadable_decomposition_answer_falls_back_to_the_single_implementation() {
    for answer in [r#""yes""#, "1", r#""0""#, "null"] {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let agent = seed(root);
        plan_common(root);
        plan(
            root,
            "decompose",
            &format!(r#"{{"triggers":"x","decomposed":{answer},"subtasks":{SUBTASKS}}}"#),
        );
        let (ev, _) = drive(root, &agent);
        assert!(ran(&ev, "implement"), "{answer}: {:?}", started(&ev));
        assert!(
            !ran(&ev, "implement_subtasks"),
            "{answer}: {:?}",
            started(&ev)
        );
        assert!(ran(&ev, "assessed"), "{answer}: {:?}", started(&ev));
    }
}

#[test]
fn a_decomposition_runs_the_subtasks_in_order_and_joins_without_the_single_implementation() {
    // The boolean and its string form route alike, and so do the subtasks
    // as an array and as a JSON array written as a string.
    let as_string = serde_json::to_string(SUBTASKS).unwrap();
    for (decomposed, subtasks) in [
        ("true", SUBTASKS),
        (r#""true""#, SUBTASKS),
        ("true", as_string.as_str()),
    ] {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let agent = seed(root);
        plan_common(root);
        plan(
            root,
            "decompose",
            &format!(r#"{{"triggers":"layers","decomposed":{decomposed},"subtasks":{subtasks}}}"#),
        );
        plan(root, "implement_subtasks", subtask_pass());
        let (ev, log) = drive(root, &agent);
        assert!(!ran(&ev, "implement"), "{subtasks}: {:?}", started(&ev));
        let items: Vec<&String> = log
            .iter()
            .filter(|l| l.starts_with("implement_subtasks-"))
            .collect();
        assert_eq!(
            items,
            ["implement_subtasks-s1", "implement_subtasks-s2"],
            "subtasks run once each, in the declared order"
        );
        for node in [
            "candidate_ready",
            "review_security",
            "collect",
            "report",
            "assessed",
        ] {
            assert!(ran(&ev, node), "{node} did not run: {:?}", started(&ev));
        }
        assert!(!ran(&ev, "reconsider"));
    }
}

#[test]
fn a_subtask_that_still_fails_after_its_repair_round_reconsiders_before_the_panel() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let agent = seed(root);
    plan_common(root);
    plan(
        root,
        "decompose",
        &format!(r#"{{"triggers":"layers","decomposed":true,"subtasks":{SUBTASKS}}}"#),
    );
    plan(root, "implement_subtasks", subtask_pass());
    plan(
        root,
        "implement_subtasks-s2",
        r#"{"changed_paths":[],"evidence":"e","review":"r","gaps":"g","self_check":"fail"}"#,
    );
    let (ev, log) = drive(root, &agent);
    let s2_runs = log.iter().filter(|l| *l == "implement_subtasks-s2").count();
    assert_eq!(s2_runs, 2, "one first round and one repair round: {log:?}");
    assert!(ran(&ev, "reconsider"), "{:?}", started(&ev));
    for node in ["candidate_ready", "review_correctness", "report"] {
        assert!(!ran(&ev, node), "{node} ran: {:?}", started(&ev));
    }
}

#[test]
fn a_decomposition_with_no_subtask_reconsiders_instead_of_reviewing_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let agent = seed(root);
    plan_common(root);
    plan(
        root,
        "decompose",
        r#"{"triggers":"layers","decomposed":true,"subtasks":[]}"#,
    );
    let (ev, _) = drive(root, &agent);
    assert!(ran(&ev, "implement_subtasks"), "{:?}", started(&ev));
    assert!(!ran(&ev, "implement"));
    assert!(ran(&ev, "reconsider"), "{:?}", started(&ev));
    assert!(!ran(&ev, "candidate_ready"), "{:?}", started(&ev));
}
