//! Warm session handoff between nodes (issue #67 item 1): a node with
//! `continue_session: <id>` continues the agent session the named node
//! finished in, instead of starting a fresh agent that rebuilds the same
//! understanding. A recording stub stands in for the agent, so the tests read
//! back the resume flags and prompts a real agent would have received.

use std::fs;
use std::path::Path;

use apb_core::registry::init_project;
use apb_engine::event::{EventPayload, read_all};
use apb_engine::state::RunStatus;

use crate::common;
use crate::token_economy_test::{
    Invocation, invocations, recording_stub, run_with_stub, seed_playbook,
};

const OK: &str = "printf '\\n```yaml\\nstatus: success\\nsummary: ok\\n```\\n'";
const FAIL: &str = "printf '\\n```yaml\\nstatus: failure\\nsummary: not yet\\n```\\n'";

/// `assess` then `implement`, the second continuing the first's session.
/// `implement_extra` is spliced into the second node.
fn playbook(implement_extra: &str) -> String {
    format!(
        r#"schema: 2
id: two
name: Two
version: 1.0.0
defaults: {{ profile: main }}
nodes:
  - {{ id: start, type: start }}
  - {{ id: assess, type: agent_task, prompt: "Assess the change." }}
  - {{ id: implement, type: agent_task, prompt: "Implement what you assessed.", continue_session: assess{implement_extra} }}
  - {{ id: done, type: finish, outcome: success }}
edges:
  - {{ from: start, to: assess }}
  - {{ from: assess, to: implement }}
  - {{ from: implement, to: done }}
"#
    )
}

fn seed(root: &Path, agent: &str, implement_extra: &str) {
    init_project(root).unwrap();
    seed_playbook(root, "two", &playbook(implement_extra));
    common::seed_profile(root, "main", agent, "haiku", &[]);
    fs::write(
        root.join(".apb/profiles/main/SOUL.md"),
        "You are the implementer.",
    )
    .unwrap();
}

fn by_node<'a>(inv: &'a [Invocation], node: &str) -> Vec<&'a Invocation> {
    inv.iter().filter(|i| i.node == node).collect()
}

/// The handoff events of the only run under `root`.
fn handoffs(root: &Path) -> Vec<(String, bool, Option<String>)> {
    let run_dir = fs::read_dir(root.join(".apb/runs"))
        .unwrap()
        .filter_map(|e| e.ok().map(|e| e.path()))
        .find(|p| p.is_dir())
        .unwrap();
    read_all(&run_dir)
        .unwrap()
        .into_iter()
        .filter_map(|e| match e.payload {
            EventPayload::SessionHandoff {
                node,
                from_node,
                warm,
                reason,
            } => {
                assert_eq!(from_node, "assess");
                Some((node, warm, reason))
            }
            _ => None,
        })
        .collect()
}

/// The claude case: the second node resumes the session id apb assigned to
/// the first at launch, and gets its own full task without the SOUL.
#[test]
fn a_node_continues_the_named_nodes_claude_session() {
    let dir = tempfile::tempdir().unwrap();
    seed(dir.path(), "claude", "");
    let stub = recording_stub(dir.path(), OK);
    assert_eq!(
        run_with_stub(dir.path(), "two", &stub, None),
        RunStatus::Succeeded
    );
    let inv = invocations(dir.path());
    let assess = by_node(&inv, "assess");
    let implement = by_node(&inv, "implement");
    assert_eq!((assess.len(), implement.len()), (1, 1));
    let sid = assess[0].flag("--session-id").expect("assigned at launch");
    assert_eq!(
        implement[0].flag("--resume"),
        Some(sid),
        "{:?}",
        implement[0].args
    );
    let prompt = implement[0].prompt();
    assert!(prompt.contains("Implement what you assessed."), "{prompt}");
    assert!(
        !implement[0]
            .args
            .iter()
            .any(|a| a.contains("You are the implementer.")),
        "the session already carries the SOUL: {:?}",
        implement[0].args
    );
    assert!(
        assess[0]
            .args
            .iter()
            .any(|a| a.contains("You are the implementer.")),
        "a fresh node gets the SOUL"
    );
    assert_eq!(
        handoffs(dir.path()),
        vec![("implement".to_string(), true, None)]
    );
}

/// codex prints its session id; the handoff resumes it with `exec resume`.
#[test]
fn a_node_continues_a_printed_codex_session() {
    let dir = tempfile::tempdir().unwrap();
    seed(dir.path(), "codex", "");
    let stub = recording_stub(
        dir.path(),
        &format!("echo 'session id: 0199aa00-1111-7222-8333-444455556666' 1>&2; {OK}"),
    );
    assert_eq!(
        run_with_stub(dir.path(), "two", &stub, None),
        RunStatus::Succeeded
    );
    let inv = invocations(dir.path());
    let implement = by_node(&inv, "implement");
    assert!(
        implement[0].args.starts_with(&[
            "exec".to_string(),
            "resume".to_string(),
            "0199aa00-1111-7222-8333-444455556666".to_string(),
        ]),
        "{:?}",
        implement[0].args
    );
}

/// opencode prints no id: a successful source attempt whose session a later
/// node continues is looked up by its title right away.
#[test]
fn a_node_continues_a_titled_opencode_session() {
    let dir = tempfile::tempdir().unwrap();
    seed(dir.path(), "opencode", "");
    let d = dir.path().display();
    let stub = recording_stub(
        dir.path(),
        &format!(
            r#"if [ "$1" = session ]; then printf '[{{"id":"ses_found","title":"%s"}}]' "$(cat '{d}/title')"; exit 0; fi
prev=""; for a in "$@"; do if [ "$prev" = --title ]; then printf '%s' "$a" > '{d}/title'; fi; prev="$a"; done
{OK}"#
        ),
    );
    assert_eq!(
        run_with_stub(dir.path(), "two", &stub, None),
        RunStatus::Succeeded
    );
    let inv = invocations(dir.path());
    let implement: Vec<&Invocation> = by_node(&inv, "implement")
        .into_iter()
        .filter(|i| i.args.first().map(String::as_str) == Some("run"))
        .collect();
    assert_eq!(implement[0].flag("--session"), Some("ses_found"));
}

/// A different model on the continuing node is a different binding: cold
/// start with the full prompt and the SOUL, and the reason is journaled.
#[test]
fn a_different_executor_starts_cold_and_says_why() {
    let dir = tempfile::tempdir().unwrap();
    seed(dir.path(), "claude", ", profile: other");
    common::seed_profile(dir.path(), "other", "claude", "opus", &[]);
    let stub = recording_stub(dir.path(), OK);
    assert_eq!(
        run_with_stub(dir.path(), "two", &stub, None),
        RunStatus::Succeeded
    );
    let inv = invocations(dir.path());
    let implement = by_node(&inv, "implement");
    assert!(!implement[0].resumed(), "{:?}", implement[0].args);
    let events = handoffs(dir.path());
    assert_eq!(events.len(), 1);
    assert!(!events[0].1);
    assert!(
        events[0].2.as_deref().unwrap_or("").contains("model"),
        "{events:?}"
    );
}

/// The continuing node running in another directory starts cold: an agent
/// keys its sessions by the directory they ran in.
#[test]
fn a_different_workdir_starts_cold() {
    let dir = tempfile::tempdir().unwrap();
    fs::create_dir_all(dir.path().join("elsewhere")).unwrap();
    seed(dir.path(), "claude", ", workdir: elsewhere");
    let stub = recording_stub(dir.path(), OK);
    assert_eq!(
        run_with_stub(dir.path(), "two", &stub, None),
        RunStatus::Succeeded
    );
    let inv = invocations(dir.path());
    assert!(!by_node(&inv, "implement")[0].resumed());
    let events = handoffs(dir.path());
    assert!(!events[0].1, "{events:?}");
}

/// A handed-off session the agent no longer has never reached the model:
/// the node starts fresh with its full prompt, without spending a retry.
#[test]
fn a_lost_handed_off_session_starts_fresh() {
    let dir = tempfile::tempdir().unwrap();
    seed(dir.path(), "claude", "");
    let stub = recording_stub(
        dir.path(),
        &format!(
            "if [ \"$NODE\" = implement ] && [ \"$N\" = 1 ]; then echo 'No conversation found with session ID: x' 1>&2; exit 1; fi; {OK}"
        ),
    );
    assert_eq!(
        run_with_stub(dir.path(), "two", &stub, None),
        RunStatus::Succeeded
    );
    let inv = invocations(dir.path());
    let implement = by_node(&inv, "implement");
    assert_eq!(implement.len(), 2);
    assert!(implement[0].resumed());
    assert!(!implement[1].resumed());
    assert!(
        implement[1]
            .prompt()
            .contains("Implement what you assessed.")
    );
    assert!(
        implement[1]
            .args
            .iter()
            .any(|a| a.contains("You are the implementer.")),
        "a fresh start carries the SOUL again"
    );
}

/// A retry of the warm node continues the same session with the short
/// continuation prompt (the retry path, unchanged).
#[test]
fn a_retry_after_a_warm_start_keeps_the_session() {
    let dir = tempfile::tempdir().unwrap();
    seed(dir.path(), "claude", ", max_retries: 1");
    let stub = recording_stub(
        dir.path(),
        &format!(
            "if [ \"$NODE\" = implement ] && [ \"$N\" = 1 ]; then echo 'tests are red'; {FAIL}; else {OK}; fi"
        ),
    );
    assert_eq!(
        run_with_stub(dir.path(), "two", &stub, None),
        RunStatus::Succeeded
    );
    let inv = invocations(dir.path());
    let sid = by_node(&inv, "assess")[0].flag("--session-id").unwrap();
    let implement = by_node(&inv, "implement");
    assert_eq!(implement.len(), 2);
    assert_eq!(implement[1].flag("--resume"), Some(sid));
    assert!(implement[1].prompt().contains("tests are red"));
}
