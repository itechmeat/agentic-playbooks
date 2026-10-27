//! List mode end to end (`list:` on an agent_task): batches and their
//! settlement order, the item worker pool's cap, the shared prompt prefix,
//! the four check kinds, retries of rejected items, the refusal of a bad
//! source, progress, and two items running side by side. A stub stands in
//! for the agent: it records each invocation (item and prompt) and writes
//! the item's outputs to its status file. No model is ever called.
//!
//! The helpers are shared with `list_resume_test`.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use apb_core::registry::init_project;
use apb_engine::event::{Event, EventPayload, read_all};
use apb_engine::scheduler::{RunOptions, RunResult, run};
use apb_engine::state::RunStatus;

use crate::common;

/// How long a test waits for a condition before it fails.
pub(crate) const DEADLINE: Duration = Duration::from_secs(30);

/// Polls `f` until it yields, failing after [`DEADLINE`].
pub(crate) fn poll_until<T>(what: &str, mut f: impl FnMut() -> Option<T>) -> T {
    let start = Instant::now();
    loop {
        if let Some(v) = f() {
            return v;
        }
        assert!(start.elapsed() < DEADLINE, "timed out waiting for: {what}");
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// A one-list playbook: `start -> work -> done`, `work` an agent_task with
/// the `list` block `list` (YAML, indented under `list:`). `extra` adds
/// nodes before `work`; `from` routes `start` to it instead.
pub(crate) fn playbook(list: &str, extra: &str, from: &str) -> String {
    let list: String = list.lines().map(|l| format!("      {l}\n")).collect();
    format!(
        "schema: 2\nid: lst\nname: List\nversion: 1.0.0\ndefaults: {{ profile: main }}\nnodes:\n  - {{ id: start, type: start }}\n{extra}  - id: work\n    type: agent_task\n    prompt: \"Handle one item of the list.\"\n    list:\n{list}  - {{ id: done, type: finish, outcome: success }}\nedges:\n  - {{ from: start, to: {first} }}\n{rest}  - {{ from: work, to: done }}\n",
        first = if from.is_empty() { "work" } else { from },
        rest = if from.is_empty() {
            String::new()
        } else {
            format!("  - {{ from: {from}, to: work }}\n")
        },
    )
}

/// `n` inline items `i1`..`in`.
pub(crate) fn inline_items(n: usize) -> String {
    let items: Vec<String> = (1..=n).map(|i| format!("{{ id: i{i}, n: {i} }}")).collect();
    format!("[{}]", items.join(", "))
}

/// Seeds the project, the playbook `lst` with its `scripts/`, and profile
/// `main`.
pub(crate) fn seed(root: &Path, yaml: &str, scripts: &[(&str, &str)]) {
    init_project(root).unwrap();
    let vdir = root.join(".apb/playbooks/lst/1.0.0");
    fs::create_dir_all(vdir.join("scripts")).unwrap();
    fs::write(vdir.join("playbook.yaml"), yaml).unwrap();
    fs::write(root.join(".apb/playbooks/lst/current"), "1.0.0").unwrap();
    for (name, body) in scripts {
        fs::write(vdir.join("scripts").join(name), body).unwrap();
    }
    common::seed_profile(root, "main", "claude", "haiku", &[]);
    git_tree(root);
}

/// Makes `root` a git work tree with one commit: an item check script runs
/// only where apb can tell it left the tree as it found it.
fn git_tree(root: &Path) {
    let git = |args: &[&str]| {
        let ok = std::process::Command::new("git")
            .args(args)
            .current_dir(root)
            .output()
            .unwrap()
            .status
            .success();
        assert!(ok, "git {args:?}");
    };
    fs::write(
        root.join(".gitignore"),
        ".apb/
stub/
agent.sh
",
    )
    .unwrap();
    git(&["init", "-q"]);
    git(&["add", ".gitignore"]);
    git(&[
        "-c",
        "user.name=t",
        "-c",
        "user.email=t@example.com",
        "commit",
        "-qm",
        "init",
    ]);
}

/// The stub's working directory.
pub(crate) fn stub_dir(root: &Path) -> PathBuf {
    root.join("stub")
}

/// The stub agent. Per invocation it counts `$N` (this item's run), records
/// the item and argv, marks `started-<item>`, runs `hook` (shell, may exit),
/// then writes `blocking` from `plan-<item>-<N>` (or `plan-<item>`, else
/// `0`) to its status file and reports success.
pub(crate) fn stub(root: &Path, hook: &str) -> String {
    let d = stub_dir(root);
    fs::create_dir_all(d.join("inv")).unwrap();
    let body = format!(
        r#"#!/bin/sh
D='{d}'
I="${{APB_ITEM_ID:-none}}"
N=$(cat "$D/n-$I" 2>/dev/null || echo 0); N=$((N+1)); echo $N > "$D/n-$I"
f=$(mktemp "$D/inv/XXXXXXXX")
{{ printf '%s\0' "$I"; for a in "$@"; do printf '%s\0' "$a"; done; }} > "$f"
touch "$D/started-$I"
{hook}
B=$(cat "$D/plan-$I-$N" 2>/dev/null || cat "$D/plan-$I" 2>/dev/null || echo 0)
printf '{{"status":"success","outputs":{{"blocking":"%s"}}}}' "$B" > "$APB_STATUS_FILE"
printf '\n```yaml\nstatus: success\nsummary: ok\n```\n'
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

/// Sets what item `item` reports as `blocking` (on every run, or on run `n`).
pub(crate) fn plan(root: &Path, item: &str, n: Option<u32>, blocking: &str) {
    let name = match n {
        Some(n) => format!("plan-{item}-{n}"),
        None => format!("plan-{item}"),
    };
    fs::write(stub_dir(root).join(name), blocking).unwrap();
}

/// One recorded agent invocation: the item it ran and its prompt.
pub(crate) struct Inv {
    pub item: String,
    pub prompt: String,
}

/// Every recorded invocation (unordered).
pub(crate) fn invocations(root: &Path) -> Vec<Inv> {
    let dir = stub_dir(root).join("inv");
    fs::read_dir(dir)
        .unwrap()
        .filter_map(|e| e.ok())
        .map(|e| {
            let raw = fs::read(e.path()).unwrap();
            let mut parts: Vec<String> = raw
                .split(|b| *b == 0)
                .map(|s| String::from_utf8_lossy(s).into_owned())
                .collect();
            parts.pop();
            let item = parts.remove(0);
            let prompt = parts
                .windows(2)
                .find(|w| w[0] == "--")
                .map(|w| w[1].clone())
                .unwrap_or_default();
            Inv { item, prompt }
        })
        .collect()
}

/// How many times the stub ran `item`.
pub(crate) fn runs_of(root: &Path, item: &str) -> u32 {
    fs::read_to_string(stub_dir(root).join(format!("n-{item}")))
        .map(|s| s.trim().parse().unwrap())
        .unwrap_or(0)
}

/// Holds the shared env lock with the stub as the agent and a fast
/// infrastructure backoff; every variable is restored on drop, while the
/// lock is still held.
pub(crate) struct Env {
    // Fields drop in order: the variables first, then the lock.
    _vars: Vec<common::EnvVar>,
    _lock: std::sync::MutexGuard<'static, ()>,
}

impl Env {
    pub(crate) fn set(prog: &str, more: &[(&'static str, &str)]) -> Self {
        let lock = common::env_lock();
        let mut vars = vec![
            common::EnvVar::set("APB_AGENT_CMD", prog),
            common::EnvVar::set(apb_engine::failure_class::BACKOFF_ENV, "1,1"),
        ];
        vars.extend(more.iter().map(|(k, v)| common::EnvVar::set(k, v)));
        Env {
            _vars: vars,
            _lock: lock,
        }
    }
}

/// Runs `lst` to its end.
pub(crate) fn run_list(root: &Path) -> RunResult {
    run(root, "lst", None, RunOptions::default()).unwrap()
}

pub(crate) fn events(root: &Path, run_id: &str) -> Vec<Event> {
    read_all(&root.join(".apb/runs").join(run_id)).unwrap()
}

/// One `list_item_settled` of `work`.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Settled {
    pub item: String,
    pub state: String,
    pub round: u32,
    pub reason: Option<String>,
    pub note: Option<String>,
    pub carried: bool,
}

/// Every `list_item_settled` of `work`, in journal order.
pub(crate) fn settled(events: &[Event]) -> Vec<Settled> {
    events
        .iter()
        .filter_map(|e| match &e.payload {
            EventPayload::ListItemSettled {
                node,
                item,
                state,
                round,
                reason,
                note,
                carried,
                ..
            } if node == "work" => Some(Settled {
                item: item.clone(),
                state: state.clone(),
                round: *round,
                reason: reason.clone(),
                note: note.clone(),
                carried: *carried,
            }),
            _ => None,
        })
        .collect()
}

/// The last settlement of `item`.
pub(crate) fn last_state(events: &[Event], item: &str) -> Settled {
    settled(events)
        .into_iter()
        .rev()
        .find(|s| s.item == item)
        .unwrap_or_else(|| panic!("item {item} never settled"))
}

/// The `node_finished` statuses of `work`.
pub(crate) fn work_finishes(events: &[Event]) -> Vec<String> {
    events
        .iter()
        .filter_map(|e| match &e.payload {
            EventPayload::NodeFinished { node, status, .. } if node == "work" => {
                Some(status.clone())
            }
            _ => None,
        })
        .collect()
}

/// The most item attempts of `work` that were open at once.
fn most_open(events: &[Event]) -> usize {
    let (mut open, mut most) = (0usize, 0usize);
    for e in events {
        match &e.payload {
            EventPayload::AttemptStarted {
                node,
                item: Some(_),
                ..
            } if node == "work" => {
                open += 1;
                most = most.max(open);
            }
            EventPayload::AttemptFinished {
                node,
                item: Some(_),
                ..
            } if node == "work" => {
                assert!(open > 0, "an item attempt finished that never started");
                open -= 1;
            }
            _ => {}
        }
    }
    most
}

const OUTPUT_CHECK: &str =
    "item_outputs: [blocking]\ncheck: { output: { field: blocking, equals: \"0\" } }";

/// Batches: every item of batch k settles before batch k+1 starts, the
/// pool never runs more than `min(parallel, max_parallel)` items, and each
/// batch ends with a progress report carrying the counts.
#[test]
fn batches_settle_in_order_under_the_pool_cap_and_report_progress() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let list = format!(
        "items: {{ inline: {} }}\nbatch: {{ size: 4, parallel: 3, retries: 0 }}\n{OUTPUT_CHECK}",
        inline_items(10)
    );
    let yaml = playbook(&list, "", "").replace(
        "defaults: { profile: main }",
        "defaults: { profile: main, max_parallel: 2 }",
    );
    seed(root, &yaml, &[]);
    let _env = Env::set(&stub(root, ""), &[]);
    let res = run_list(root);
    assert_eq!(res.outcome, RunStatus::Succeeded);
    let ev = events(root, &res.run_id);

    let mut seen_settled: Vec<String> = Vec::new();
    let mut batches: Vec<Vec<String>> = Vec::new();
    for e in &ev {
        match &e.payload {
            EventPayload::ListBatchStarted { items, .. } => {
                if let Some(prev) = batches.last() {
                    for id in prev {
                        assert!(
                            seen_settled.contains(id),
                            "item {id} must settle before the next batch starts"
                        );
                    }
                }
                batches.push(items.clone());
            }
            EventPayload::ListItemSettled { item, .. } => seen_settled.push(item.clone()),
            _ => {}
        }
    }
    let sizes: Vec<usize> = batches.iter().map(Vec::len).collect();
    assert_eq!(sizes, [4, 4, 2]);
    assert!(
        most_open(&ev) <= 2,
        "max_parallel 2 caps the pool of parallel 3"
    );
    assert_eq!(
        settled(&ev).iter().filter(|s| s.state == "passed").count(),
        10
    );
    let progress: Vec<(u64, u64)> = ev
        .iter()
        .filter_map(|e| match &e.payload {
            EventPayload::RunProgress {
                node_id,
                done,
                total,
                ..
            } if node_id == "work" => Some((*done, *total)),
            _ => None,
        })
        .collect();
    assert_eq!(progress, [(4, 10), (8, 10), (10, 10)]);
    let pb = apb_core::schema::Playbook::from_yaml(&yaml).unwrap();
    let summary = apb_engine::progress::compute(&pb, &ev);
    assert_eq!(summary.lists.len(), 1);
    assert_eq!(
        (summary.lists[0].processed, summary.lists[0].passed),
        (10, 10)
    );
    let output = ev
        .iter()
        .find_map(|e| match &e.payload {
            EventPayload::NodeFinished { node, output, .. } if node == "work" => {
                Some(output.clone())
            }
            _ => None,
        })
        .unwrap();
    let out: serde_json::Value = serde_json::from_str(&output).unwrap();
    assert_eq!(
        (out["total"].as_u64(), out["passed"].as_u64()),
        (Some(10), Some(10))
    );
    assert_eq!(out["items"].as_array().map(Vec::len), Some(10));
}

/// The prompt is the same for every item up to the item section, and the
/// template is rendered once (its missing input is reported once).
#[test]
fn item_prompts_share_their_prefix_up_to_the_item_section() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let list = format!(
        "items: {{ inline: {} }}\nbatch: {{ size: 2, parallel: 1, retries: 0 }}\n{OUTPUT_CHECK}",
        inline_items(2)
    );
    let yaml = playbook(&list, "", "").replace(
        "Handle one item of the list.",
        "Handle one item of the list. Start said: {{nodes.start.output}}",
    );
    seed(root, &yaml, &[]);
    let _env = Env::set(&stub(root, ""), &[]);
    let res = run_list(root);
    assert_eq!(res.outcome, RunStatus::Succeeded);
    let inv = invocations(root);
    let p = |item: &str| {
        inv.iter()
            .find(|i| i.item == item)
            .map(|i| i.prompt.clone())
            .unwrap()
    };
    let (a, b) = (p("i1"), p("i2"));
    let at = a.find("## item").expect("an item section");
    assert_eq!(b.find("## item"), Some(at));
    assert_eq!(a[..at], b[..at], "the shared prefix is byte-identical");
    assert_ne!(a[at..], b[at..]);
    assert!(a[at..].contains(r#""id":"i1""#) && b[at..].contains(r#""id":"i2""#));
    let ev = events(root, &res.run_id);
    let anomalies = ev
        .iter()
        .filter(|e| {
            matches!(&e.payload, EventPayload::WakeRaised { node, detail, .. }
                if node == "work" && detail.contains("renders empty"))
        })
        .count();
    assert_eq!(anomalies, 1, "the shared template is rendered once");
}

/// Accept, accept with a note, and a reject that re-runs and then passes:
/// only the rejected item runs again, and its retry is briefed on why.
#[test]
fn a_rejected_item_reruns_alone_and_is_briefed_on_the_rejection() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let script = r#"B=$(sed -n 's/.*"blocking":"\([^"]*\)".*/\1/p' "$APB_ITEM_OUTPUT_FILE")
case "$B" in
  0) echo '{"verdict":"accept"}' ;;
  note) echo '{"verdict":"accept_with_note","note":"fine, but terse"}' ;;
  *) echo '{"verdict":"reject","note":"the answer is still blocking"}' ;;
esac
"#;
    let list = "items: { inline: [ { id: a }, { id: b }, { id: c } ] }\nbatch: { size: 3, parallel: 1, retries: 1 }\nitem_outputs: [blocking]\ncheck: { script: scripts/check.sh }";
    seed(root, &playbook(list, "", ""), &[("check.sh", script)]);
    let _env = Env::set(&stub(root, ""), &[]);
    plan(root, "b", None, "note");
    plan(root, "c", Some(1), "1");
    let res = run_list(root);
    assert_eq!(res.outcome, RunStatus::Succeeded);
    assert_eq!(
        (runs_of(root, "a"), runs_of(root, "b"), runs_of(root, "c")),
        (1, 1, 2)
    );
    let ev = events(root, &res.run_id);
    assert_eq!(last_state(&ev, "a").state, "passed");
    let b = last_state(&ev, "b");
    assert_eq!(
        (b.state.as_str(), b.note.as_deref()),
        ("passed", Some("fine, but terse"))
    );
    let rejected: Vec<Settled> = settled(&ev)
        .into_iter()
        .filter(|s| s.state == "rejected")
        .collect();
    assert_eq!(rejected.len(), 1);
    assert_eq!((rejected[0].item.as_str(), rejected[0].round), ("c", 1));
    let c = last_state(&ev, "c");
    assert_eq!((c.state.as_str(), c.round), ("passed", 1));
    let retry = invocations(root)
        .into_iter()
        .filter(|i| i.item == "c")
        .map(|i| i.prompt)
        .find(|p| p.contains("Check rejected"))
        .expect("the re-run of c is briefed");
    assert!(retry.contains(r#"Check rejected round 0, its note: "the answer is still blocking""#));
}

/// Retries spent: the item needs a look (`rejected`); the node succeeds
/// with `on_needs_look: continue` and fails with `fail`.
#[test]
fn spent_retries_need_a_look_and_on_needs_look_decides_the_node() {
    for (policy, want) in [
        ("continue", RunStatus::Succeeded),
        ("fail", RunStatus::Failed),
    ] {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let list = format!(
            "items: {{ inline: [ {{ id: a }}, {{ id: b }} ] }}\nbatch: {{ size: 2, parallel: 1, retries: 1 }}\non_needs_look: {policy}\n{OUTPUT_CHECK}"
        );
        seed(root, &playbook(&list, "", ""), &[]);
        let _env = Env::set(&stub(root, ""), &[]);
        plan(root, "b", None, "1");
        let res = run_list(root);
        assert_eq!(res.outcome, want, "on_needs_look: {policy}");
        let ev = events(root, &res.run_id);
        let b = last_state(&ev, "b");
        assert_eq!(
            (b.state.as_str(), b.reason.as_deref()),
            ("needs_look", Some("rejected"))
        );
        assert_eq!(runs_of(root, "b"), 2, "one run and one re-run");
        assert_eq!(last_state(&ev, "a").state, "passed");
    }
}

/// A script check that exits non-zero rejects the item with its stderr; a
/// judge with no decision provider applies `on_unavailable`.
#[test]
fn a_failing_check_script_rejects_and_an_unavailable_judge_applies_its_fallback() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let list = "items: { inline: [ { id: a } ] }\nbatch: { size: 1, retries: 0 }\nitem_outputs: [blocking]\ncheck: { script: scripts/check.sh }";
    seed(
        root,
        &playbook(list, "", ""),
        &[("check.sh", "echo 'the item is not filed' 1>&2\nexit 3\n")],
    );
    let _env = Env::set(&stub(root, ""), &[]);
    let res = run_list(root);
    let ev = events(root, &res.run_id);
    let a = last_state(&ev, "a");
    assert_eq!(
        (a.state.as_str(), a.reason.as_deref(), a.note.as_deref()),
        (
            "needs_look",
            Some("rejected"),
            Some("the item is not filed")
        )
    );
    drop(_env);

    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let list = "items: { inline: [ { id: a, subject: hello } ] }\nbatch: { size: 1, retries: 0 }\nitem_outputs: [blocking]\ncheck:\n  judge:\n    state: { subject: \"{{item.subject}}\", verdict: \"{{item_output.blocking}}\" }\n    instructions: Is it plausible?\n    criteria: { accept: plausible, reject: not plausible }\n    on_unavailable: accept_with_note";
    seed(root, &playbook(list, "", ""), &[]);
    let _env = Env::set(&stub(root, ""), &[]);
    let res = run_list(root);
    assert_eq!(res.outcome, RunStatus::Succeeded);
    let a = last_state(&events(root, &res.run_id), "a");
    assert_eq!(a.state, "passed");
    assert!(
        a.note
            .as_deref()
            .is_some_and(|n| n.contains("no usable answer")),
        "the note says the judge could not answer"
    );
}

/// A source with duplicate ids fails the node before any item runs, naming
/// the ids.
#[test]
fn duplicate_ids_fail_the_node_before_any_attempt() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let extra = "  - id: emit\n    type: script\n    script: scripts/emit.sh\n    runner: sh\n    outputs: { fields: [messages] }\n";
    let list = format!("items: {{ from: {{ node: emit, field: messages }} }}\n{OUTPUT_CHECK}");
    seed(
        root,
        &playbook(&list, extra, "emit"),
        &[(
            "emit.sh",
            "echo '{\"messages\":[{\"id\":\"x\"},{\"id\":\"y\"},{\"id\":\"x\"}]}'\n",
        )],
    );
    let _env = Env::set(&stub(root, ""), &[]);
    let res = run_list(root);
    assert_eq!(res.outcome, RunStatus::Failed);
    let ev = events(root, &res.run_id);
    assert!(
        !ev.iter().any(|e| matches!(&e.payload,
            EventPayload::AttemptStarted { node, .. } if node == "work")),
        "no item ran"
    );
    let output = ev
        .iter()
        .find_map(|e| match &e.payload {
            EventPayload::NodeFinished { node, output, .. } if node == "work" => {
                Some(output.clone())
            }
            _ => None,
        })
        .unwrap();
    let why = apb_core::list::items::ItemsError::Duplicate(vec!["x".into()]);
    assert_eq!(output, format!("list `work`: {why}"));
}

/// Two items running at once keep their own status files, transcripts and
/// attempt pairing, and liveness sees both open attempts.
#[test]
fn concurrent_items_keep_their_own_files_and_attempts() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let list = format!(
        "items: {{ inline: [ {{ id: a }}, {{ id: b }} ] }}\nbatch: {{ size: 2, parallel: 2, retries: 0 }}\n{OUTPUT_CHECK}"
    );
    seed(root, &playbook(&list, "", ""), &[]);
    // Each item waits until both have started, so they overlap for sure.
    let hook = r#"i=0; while [ ! -f "$D/started-a" ] || [ ! -f "$D/started-b" ]; do i=$((i+1)); [ $i -gt 600 ] && break; sleep 0.05; done
echo "$APB_STATUS_FILE" > "$D/status-$I""#;
    let _env = Env::set(&stub(root, hook), &[]);
    let res = run_list(root);
    assert_eq!(res.outcome, RunStatus::Succeeded);
    let ev = events(root, &res.run_id);
    assert_eq!(most_open(&ev), 2, "the two items overlapped");
    let status = |i: &str| fs::read_to_string(stub_dir(root).join(format!("status-{i}"))).unwrap();
    assert_ne!(status("a"), status("b"));
    let transcripts: Vec<(String, String)> = ev
        .iter()
        .filter_map(|e| match &e.payload {
            EventPayload::AttemptStarted {
                item: Some(item),
                transcript: Some(t),
                ..
            } => Some((item.clone(), t.clone())),
            _ => None,
        })
        .collect();
    assert_eq!(transcripts.len(), 2);
    assert_ne!(transcripts[0].1, transcripts[1].1);
    // The journal up to the second start: both attempts are open, each
    // under its own item.
    let second = ev
        .iter()
        .enumerate()
        .filter(|(_, e)| {
            matches!(
                &e.payload,
                EventPayload::AttemptStarted { item: Some(_), .. }
            )
        })
        .nth(1)
        .map(|(i, _)| i)
        .unwrap();
    let mut open: Vec<Option<String>> = apb_engine::liveness::open_attempts(&ev[..=second])
        .into_iter()
        .map(|a| a.item)
        .collect();
    open.sort();
    assert_eq!(open, [Some("a".to_string()), Some("b".to_string())]);
    for id in ["a", "b"] {
        let finished = ev
            .iter()
            .filter(|e| {
                matches!(&e.payload,
                EventPayload::AttemptFinished { item: Some(i), status, .. }
                    if i == id && status == "succeeded")
            })
            .count();
        assert_eq!(finished, 1, "item {id} has its own attempt_finished");
    }
}

/// A connector item check through the `<node>#check` grant: a read that
/// confirms the item accepts it, a read that fails decides nothing, so the
/// item needs a look (`check_unknown`), never an accept.
#[test]
fn a_connector_check_accepts_on_a_confirming_read_and_needs_a_look_on_a_failed_one() {
    const CONNECTOR: &str = r#"
name: tracker
version: 0.1.0
account_fields:
  - name: base_url
    required: true
functions:
  - name: get_task
    description: Reads a task
    read_only: true
    mock: { status: 200, body: { data: { gid: "77" } } }
  - name: broken
    description: A read that fails
    read_only: true
    mock: { status: 500, body: { error: down } }
"#;
    for (function, state, reason) in [
        ("get_task", "passed", None),
        ("broken", "needs_look", Some("check_unknown")),
    ] {
        let cfg = tempfile::tempdir().unwrap();
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let cdir = cfg.path().join("connectors/tracker");
        fs::create_dir_all(&cdir).unwrap();
        fs::write(cdir.join("connector.yaml"), CONNECTOR).unwrap();
        let list = format!(
            "items: {{ inline: [ {{ id: a }} ] }}\nbatch: {{ size: 1, retries: 0 }}\nitem_outputs: [blocking]\ncheck:\n  connector:\n    name: tracker\n    function: {function}\n    args: {{ task: \"{{{{item_output.blocking}}}}\" }}\n    accept_when: {{ path: data.gid, exists: true }}"
        );
        let yaml = playbook(&list, "", "");
        seed(root, &yaml, &[]);
        let acct = apb_core::connector::config::project_config_path(root, "tracker");
        fs::create_dir_all(acct.parent().unwrap()).unwrap();
        fs::write(
            &acct,
            "accounts:\n  - name: main\n    base_url: http://127.0.0.1:9\n",
        )
        .unwrap();
        let cfg_path = cfg.path().to_string_lossy().into_owned();
        let _env = Env::set(&stub(root, ""), &[("APB_CONFIG_DIR", &cfg_path)]);
        let pb = apb_core::schema::Playbook::from_yaml(&yaml).unwrap();
        let resolved = apb_core::connector::resolve::resolve_playbook(root, &pb).unwrap();
        let mut opts = RunOptions::default();
        for (name, c) in &resolved.connectors {
            opts.expected_connectors
                .insert(name.clone(), c.loaded.digest.clone());
            for a in &c.accounts {
                opts.expected_connector_accounts.insert(
                    format!("{name}/{}", a.name),
                    apb_core::connector::config::account_digest(a),
                );
            }
        }
        let res = run(root, "lst", None, opts).unwrap();
        let ev = events(root, &res.run_id);
        let a = last_state(&ev, "a");
        assert_eq!(
            (a.state.as_str(), a.reason.as_deref()),
            (state, reason),
            "check through `{function}`"
        );
        let checks = ev
            .iter()
            .filter(|e| {
                matches!(&e.payload,
                EventPayload::ConnectorCall { node_id, origin: Some(o), item: Some(i), .. }
                    if node_id == "work#check" && o == "check" && i == "a")
            })
            .count();
        assert_eq!(checks, 1, "one check call, journaled for the item");
    }
}

/// A host-mode run with a list node is refused before it starts: list
/// items are agent attempts the engine spawns itself.
#[test]
fn a_host_mode_run_with_a_list_node_is_refused_at_start() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let list = format!("items: {{ inline: [ {{ id: a }} ] }}\n{OUTPUT_CHECK}");
    seed(root, &playbook(&list, "", ""), &[]);
    let _env = Env::set(&stub(root, ""), &[]);
    let opts = RunOptions {
        execution: apb_core::execution::ExecutionRequest {
            mode: Some(apb_core::execution::ExecutionMode::Host),
            host_session: true,
            client: None,
            inherited: false,
        },
        ..RunOptions::default()
    };
    let err = run(root, "lst", None, opts).unwrap_err().to_string();
    assert!(err.contains("list nodes need execution: cli"), "{err}");
    assert!(
        !root.join(".apb/runs").is_dir()
            || fs::read_dir(root.join(".apb/runs"))
                .unwrap()
                .next()
                .is_none(),
        "no run is left behind"
    );
}

/// A script step runs once per item with the item in its environment: each
/// run gets its own item's id and the file holding its own JSON; its stdout
/// is the item's output.
#[test]
fn a_script_step_runs_once_per_item_with_the_item_in_its_env() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let list = "items: { inline: [ { id: a, n: 1 }, { id: b, n: 2 } ] }\nbatch: { size: 2, retries: 0 }\nitem_outputs: [blocking, id, n, file]\ncheck: { output: { field: blocking, equals: \"0\" } }";
    let yaml = playbook(list, "", "").replace(
        "    type: agent_task\n    prompt: \"Handle one item of the list.\"\n",
        "    type: script\n    script: scripts/item.sh\n    runner: sh\n",
    );
    let script = r#"n=$(sed -n 's/.*"n":\([0-9]*\).*/\1/p' "$APB_ITEM_FILE")
printf '{"blocking":"0","id":"%s","n":"%s","file":"%s"}\n' "$APB_ITEM_ID" "$n" "$APB_ITEM_FILE"
"#;
    seed(root, &yaml, &[("item.sh", script)]);
    let _env = Env::set(&stub(root, ""), &[]);
    let res = run_list(root);
    assert_eq!(res.outcome, RunStatus::Succeeded);
    let out = work_output(&events(root, &res.run_id));
    let items = out["items"].as_array().unwrap();
    let seen: Vec<(&str, &str)> = items
        .iter()
        .map(|i| {
            (
                i["output"]["id"].as_str().unwrap_or(""),
                i["output"]["n"].as_str().unwrap_or(""),
            )
        })
        .collect();
    assert_eq!(
        seen,
        [("a", "1"), ("b", "2")],
        "each item saw its own id and file"
    );
    let files: Vec<&str> = items
        .iter()
        .map(|i| i["output"]["file"].as_str().unwrap_or(""))
        .collect();
    assert_ne!(files[0], files[1], "each item has a file of its own");
    assert_eq!(out["passed"], 2);
}

/// The parsed output of `work`'s (last) `node_finished`.
pub(crate) fn work_output(events: &[Event]) -> serde_json::Value {
    let output = events
        .iter()
        .rev()
        .find_map(|e| match &e.payload {
            EventPayload::NodeFinished { node, output, .. } if node == "work" => {
                Some(output.clone())
            }
            _ => None,
        })
        .expect("work finished");
    serde_json::from_str(&output).unwrap()
}

/// An item whose JSON is larger than a step is handed needs a look before
/// anything runs for it; the rest of the list runs.
#[test]
fn an_item_too_large_needs_a_look_without_running() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let big = "x".repeat(apb_core::list::ITEM_MAX_BYTES + 1);
    let list = format!(
        "items: {{ inline: [ {{ id: big, s: {big} }}, {{ id: ok }} ] }}\nbatch: {{ size: 2, retries: 0 }}\n{OUTPUT_CHECK}"
    );
    seed(root, &playbook(&list, "", ""), &[]);
    let _env = Env::set(&stub(root, ""), &[]);
    let res = run_list(root);
    assert_eq!(res.outcome, RunStatus::Succeeded);
    let ev = events(root, &res.run_id);
    let big = last_state(&ev, "big");
    assert_eq!(
        (big.state.as_str(), big.reason.as_deref()),
        ("needs_look", Some("item_too_large"))
    );
    assert_eq!(runs_of(root, "big"), 0);
    assert_eq!(last_state(&ev, "ok").state, "passed");
}

/// A list on a loop: `work` runs, then once more through a self-loop edge,
/// then finishes.
pub(crate) fn looped(list: &str) -> String {
    playbook(list, "", "").replace(
        "  - { from: work, to: done }\n",
        "  - { from: work, to: work, max_traversals: 1 }\n  - { from: work, to: done, fallback: true }\n",
    )
}

/// `rerun: not_passed` on a real loop: the second visit carries the item
/// that passed in the first (`carried`) and runs only the one that did not.
#[test]
fn a_loop_visit_under_rerun_not_passed_carries_the_passed_items() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let list = format!(
        "items: {{ inline: [ {{ id: a }}, {{ id: b }} ] }}\nbatch: {{ size: 2, parallel: 1, retries: 0 }}\nrerun: not_passed\n{OUTPUT_CHECK}"
    );
    seed(root, &looped(&list), &[]);
    let _env = Env::set(&stub(root, ""), &[]);
    plan(root, "b", Some(1), "1");
    let res = run_list(root);
    assert_eq!(res.outcome, RunStatus::Succeeded);
    let ev = events(root, &res.run_id);
    assert_eq!(work_finishes(&ev).len(), 2, "two visits");
    assert_eq!((runs_of(root, "a"), runs_of(root, "b")), (1, 2));
    let a = last_state(&ev, "a");
    assert_eq!((a.state.as_str(), a.carried), ("passed", true));
    let b = last_state(&ev, "b");
    assert_eq!((b.state.as_str(), b.carried), ("passed", false));
}
