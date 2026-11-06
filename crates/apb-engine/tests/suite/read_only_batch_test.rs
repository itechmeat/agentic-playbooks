//! Read-only nodes in concurrent batches (spec loops-and-graphs D1): two
//! detected read-only scripts batch and run at once; a member that writes a tracked file fails beside the member that
//! did not, and each names the other; a writer and a read-only script never
//! overlap, at batch formation or mid-batch; a protect node never overlaps
//! one; an enforced (isolation: full) read-only node still batches with a
//! writer; and an old violation line without `concurrent_with` decodes (in
//! `event.rs`).

use std::fs;
use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};
use std::process::Command;

use apb_core::registry::init_project;
use apb_engine::event::{Event, EventPayload, ReadOnlyTier, read_all};
use apb_engine::scheduler::{RunOptions, run};
use apb_engine::state::RunStatus;

use crate::common::{self, EnvVar};

fn git(dir: &Path, args: &[&str]) {
    let ok = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["-c", "user.name=t", "-c", "user.email=t@example.invalid"])
        .args(["-c", "commit.gpgsign=false"])
        .args(args)
        .status()
        .unwrap()
        .success();
    assert!(ok, "git {args:?}");
}

const AGENT: &str = r#"#!/bin/sh
n=$(cat "$RO_SCRATCH/count" 2>/dev/null || echo 0); echo $((n+1)) > "$RO_SCRATCH/count"
sh -c "$RO_AGENT"
code=$?
printf '{"status":"success"}' > "$APB_STATUS_FILE"
echo "done $code"
exit $code
"#;

/// A project tree (a git repository with tracked `a.txt`), the playbook `ro`
/// with `max_parallel: 2` and the given `nodes`/`edges`, and a stub agent
/// outside the tree that runs `$RO_AGENT` in its cwd.
struct Fixture {
    _dirs: Vec<tempfile::TempDir>,
    root: PathBuf,
    agent: PathBuf,
    /// A directory outside the tree the stub and scripts may write.
    scratch: PathBuf,
}

fn fixture(nodes: &str, edges: &str) -> Fixture {
    let tree = tempfile::tempdir().unwrap();
    let bin = tempfile::tempdir().unwrap();
    let root = tree.path().to_path_buf();
    init_project(&root).unwrap();
    let pb = format!(
        "schema: 2\nid: ro\nname: Ro\nversion: 1.0.0\ndefaults: {{ profile: main, max_parallel: 2 }}\nnodes:\n  - {{ id: start, type: start }}\n{nodes}  - {{ id: done, type: finish, outcome: success }}\nedges:\n{edges}"
    );
    let dir = root.join(".apb/playbooks/ro/1.0.0");
    fs::create_dir_all(dir.join("scripts")).unwrap();
    fs::write(dir.join("playbook.yaml"), pb).unwrap();
    fs::write(root.join(".apb/playbooks/ro/current"), "1.0.0").unwrap();
    common::seed_main(&root);
    fs::write(root.join(".gitignore"), ".apb/\n").unwrap();
    fs::write(root.join("a.txt"), "alpha\n").unwrap();
    git(&root, &["init", "-q"]);
    git(&root, &["add", "."]);
    git(&root, &["commit", "-q", "-m", "base"]);
    let agent = bin.path().join("agent.sh");
    common::write_sync(&agent, AGENT);
    fs::set_permissions(&agent, fs::Permissions::from_mode(0o755)).unwrap();
    let scratch = bin.path().join("scratch");
    fs::create_dir_all(&scratch).unwrap();
    Fixture {
        _dirs: vec![tree, bin],
        root,
        agent,
        scratch,
    }
}

fn write_script(f: &Fixture, name: &str, body: &str) {
    fs::write(
        f.root.join(".apb/playbooks/ro/1.0.0/scripts").join(name),
        body,
    )
    .unwrap();
}

/// Runs `ro` with the stub agent running `agent_cmd`, plus `extra` env.
fn drive(f: &Fixture, agent_cmd: &str) -> (RunStatus, Vec<Event>) {
    let _lock = common::env_lock();
    let _env = [
        EnvVar::set("APB_AGENT_CMD", &f.agent),
        EnvVar::set("RO_AGENT", agent_cmd),
        EnvVar::set("RO_SCRATCH", &f.scratch),
    ];
    let res = run(&f.root, "ro", None, RunOptions::default()).unwrap();
    let events = read_all(&f.root.join(".apb/runs").join(&res.run_id)).unwrap();
    (res.outcome, events)
}

fn finished(events: &[Event], node: &str) -> Vec<(String, String)> {
    events
        .iter()
        .filter_map(|e| match &e.payload {
            EventPayload::NodeFinished {
                node: n,
                status,
                output,
                ..
            } if n == node => Some((status.to_string(), output.clone())),
            _ => None,
        })
        .collect()
}

/// The `concurrent_with` of every `read_only_violated`, by node: which of
/// two violations is journaled first is up to the scheduler.
fn concurrent_with(events: &[Event]) -> std::collections::BTreeMap<String, Vec<String>> {
    events
        .iter()
        .filter_map(|e| match &e.payload {
            EventPayload::ReadOnlyViolated {
                node,
                concurrent_with,
                ..
            } => Some((node.clone(), concurrent_with.clone())),
            _ => None,
        })
        .collect()
}

/// A script that marks itself started in `scratch` and waits (at most 6 s)
/// until `peer` did the same, then reads the tracked file: two of them can
/// end only once both ran at the same time.
fn handshake(scratch: &std::path::Display<'_>, me: &str, peer: &str) -> String {
    format!(
        "touch '{scratch}/{me}_started'\ni=0\nwhile [ ! -f '{scratch}/{peer}_started' ] && [ $i -lt 120 ]; do i=$((i+1)); sleep 0.05; done\n[ -f '{scratch}/{peer}_started' ] || exit 1\ncat a.txt\n"
    )
}

fn violations(events: &[Event]) -> usize {
    events
        .iter()
        .filter(|e| matches!(&e.payload, EventPayload::ReadOnlyViolated { .. }))
        .count()
}

/// Whether `node`'s `node_started` .. `node_finished` window on the journal
/// holds any event of `other`. Two nodes overlapped when either direction
/// holds.
fn overlap(events: &[Event], node: &str, other: &str) -> bool {
    let of = |e: &Event, n: &str| {
        matches!(
            &e.payload,
            EventPayload::NodeStarted { node: x, .. } | EventPayload::NodeFinished { node: x, .. }
                if x == n
        )
    };
    let start = events.iter().position(|e| of(e, node));
    let end = events.iter().rposition(|e| of(e, node));
    let (Some(start), Some(end)) = (start, end) else {
        return false;
    };
    events[start..=end].iter().any(|e| of(e, other))
}

/// 6.1.1: two read-only scripts that only read batch: each waits until the
/// other started, so both succeed only when they ran at once (serialized,
/// the first would give up waiting and fail), and one's window on the
/// journal holds the other's start.
#[test]
fn two_read_only_scripts_batch_and_run_at_once() {
    let f = fixture(
        "  - { id: ra, type: script, script: \"scripts/ra.sh\", runner: sh, read_only: true }\n  - { id: rb, type: script, script: \"scripts/rb.sh\", runner: sh, read_only: true }\n",
        "  - { from: start, to: ra }\n  - { from: start, to: rb }\n  - { from: ra, to: done }\n  - { from: rb, to: done }\n",
    );
    let scratch = f.scratch.display();
    write_script(&f, "ra.sh", &handshake(&scratch, "ra", "rb"));
    write_script(&f, "rb.sh", &handshake(&scratch, "rb", "ra"));
    let (outcome, events) = drive(&f, "true");
    assert_eq!(outcome, RunStatus::Succeeded, "{events:#?}");
    assert_eq!(violations(&events), 0, "{events:#?}");
    let checked = events
        .iter()
        .filter(|e| matches!(&e.payload, EventPayload::ReadOnlyChecked { .. }))
        .count();
    assert_eq!(checked, 2, "a script journals its check, no attempt start");
    assert!(
        overlap(&events, "ra", "rb") || overlap(&events, "rb", "ra"),
        "the two read-only scripts must overlap: {events:#?}"
    );
    for (node, tier) in [
        ("ra", ReadOnlyTier::Detected),
        ("rb", ReadOnlyTier::Detected),
    ] {
        let row = apb_engine::run_view::read_only_rows(&events)
            .into_iter()
            .find(|r| r.node == node)
            .unwrap_or_else(|| panic!("no row for {node}"));
        assert_eq!(row.check.tier, tier);
        assert!(row.violated.is_none());
    }
}

/// 6.1.2: two read-only scripts in a batch, one writes the tracked file
/// after both are under way: both fail `read_only_dirty`, each names the
/// other, and nothing retries either.
#[test]
fn a_member_that_writes_fails_beside_its_peer_and_each_names_the_other() {
    let f = fixture(
        "  - { id: ra, type: script, script: \"scripts/ra.sh\", runner: sh, read_only: true }\n  - { id: rb, type: script, script: \"scripts/rb.sh\", runner: sh, read_only: true }\n",
        "  - { from: start, to: ra }\n  - { from: start, to: rb }\n  - { from: ra, to: done }\n  - { from: rb, to: done }\n",
    );
    let scratch = f.scratch.display();
    // `ra` waits until `rb` is under way, writes the tracked file, and
    // marks the write; `rb` waits until `ra` is under way and until the
    // write landed, then ends. The write sits inside both engines'
    // before-to-after windows.
    write_script(
        &f,
        "ra.sh",
        &format!(
            "touch '{scratch}/ra_started'\ni=0\nwhile [ ! -f '{scratch}/rb_started' ] && [ $i -lt 120 ]; do i=$((i+1)); sleep 0.05; done\necho more >> a.txt\ntouch '{scratch}/ra_wrote'\n"
        ),
    );
    write_script(
        &f,
        "rb.sh",
        &format!(
            "touch '{scratch}/rb_started'\ni=0\nwhile [ ! -f '{scratch}/ra_started' ] && [ $i -lt 120 ]; do i=$((i+1)); sleep 0.05; done\ni=0\nwhile [ ! -f '{scratch}/ra_wrote' ] && [ $i -lt 120 ]; do i=$((i+1)); sleep 0.05; done\ncat a.txt\n"
        ),
    );
    let (outcome, events) = drive(&f, "true");
    assert_eq!(outcome, RunStatus::Failed, "{events:#?}");
    assert_eq!(violations(&events), 2, "both members saw the write");
    assert_eq!(
        concurrent_with(&events),
        [
            ("ra".to_string(), vec!["rb".to_string()]),
            ("rb".to_string(), vec!["ra".to_string()]),
        ]
        .into(),
        "{events:#?}"
    );
    assert!(
        !events
            .iter()
            .any(|e| matches!(&e.payload, EventPayload::RetryStarted { .. })),
        "a violation never retries: {events:#?}"
    );
    for node in ["ra", "rb"] {
        let fin = finished(&events, node);
        assert_eq!(fin.len(), 1, "{node}: {events:#?}");
        assert_eq!(fin[0].0, "failed");
        assert!(
            fin[0]
                .1
                .starts_with("read_only_dirty: the tree changed while node `"),
            "{node}: {}",
            fin[0].1
        );
        assert!(fin[0].1.contains("ran beside `"), "{}", fin[0].1);
    }
    assert_eq!(
        fs::read_to_string(f.root.join("a.txt")).unwrap(),
        "alpha\nmore\n",
        "the write stays: no restore, no retry"
    );
}

/// 6.1.3: a writer and a read-only script ready together never overlap,
/// whichever of the two is `current`.
#[test]
fn a_writer_and_a_read_only_script_never_overlap() {
    let cases: [(&str, &str); 2] = [
        (
            "  - { id: w, type: script, script: \"scripts/w.sh\", runner: sh }\n  - { id: ro, type: script, script: \"scripts/ro.sh\", runner: sh, read_only: true }\n",
            "  - { from: start, to: w }\n  - { from: start, to: ro }\n  - { from: w, to: done }\n  - { from: ro, to: done }\n",
        ),
        (
            "  - { id: ro, type: script, script: \"scripts/ro.sh\", runner: sh, read_only: true }\n  - { id: w, type: script, script: \"scripts/w.sh\", runner: sh }\n",
            "  - { from: start, to: ro }\n  - { from: start, to: w }\n  - { from: ro, to: done }\n  - { from: w, to: done }\n",
        ),
    ];
    for (nodes, edges) in cases {
        let f = fixture(nodes, edges);
        let scratch = f.scratch.display();
        write_script(
            &f,
            "w.sh",
            &format!("echo out > written.txt\ntouch '{scratch}/w_done'\n"),
        );
        // Where the two overlapped, `ro` would wait for the write inside its
        // own window and see it. Kept apart (the right outcome), `ro` running
        // first never sees the file, so the wait is capped at 1 s.
        write_script(
            &f,
            "ro.sh",
            &format!(
                "i=0\nwhile [ ! -f '{scratch}/w_done' ] && [ $i -lt 20 ]; do i=$((i+1)); sleep 0.05; done\ncat a.txt\n"
            ),
        );
        let (outcome, events) = drive(&f, "true");
        assert_eq!(outcome, RunStatus::Succeeded, "{events:#?}");
        assert_eq!(violations(&events), 0, "{events:#?}");
        assert!(
            !overlap(&events, "ro", "w") && !overlap(&events, "w", "ro"),
            "the writer ran inside the read-only node's window: {events:#?}"
        );
    }
}

/// 6.1.4: a read-only member finishes first and its successor is a batchable
/// writer, while another read-only member still runs: the writer waits for
/// the member that still runs.
#[test]
fn a_writer_succeeding_a_finished_read_only_member_waits_for_the_still_running_one() {
    let f = fixture(
        "  - { id: fast, type: script, script: \"scripts/fast.sh\", runner: sh, read_only: true }\n  - { id: slow, type: script, script: \"scripts/slow.sh\", runner: sh, read_only: true }\n  - { id: mid, type: script, script: \"scripts/mid.sh\", runner: sh }\n",
        "  - { from: start, to: fast }\n  - { from: start, to: slow }\n  - { from: fast, to: mid }\n  - { from: mid, to: done }\n  - { from: slow, to: done }\n",
    );
    write_script(&f, "fast.sh", "cat a.txt\n");
    write_script(&f, "slow.sh", "sleep 0.6\ncat a.txt\n");
    write_script(&f, "mid.sh", "echo x > mid.txt\n");
    let (outcome, events) = drive(&f, "true");
    assert_eq!(outcome, RunStatus::Succeeded, "{events:#?}");
    assert_eq!(violations(&events), 0, "{events:#?}");
    let slow_end = events
        .iter()
        .rposition(
            |e| matches!(&e.payload, EventPayload::NodeFinished { node, .. } if node == "slow"),
        )
        .unwrap();
    let mid_start = events
        .iter()
        .position(|e| matches!(&e.payload, EventPayload::NodeStarted { node, .. } if node == "mid"))
        .unwrap();
    assert!(
        mid_start > slow_end,
        "`mid` started while `slow` still ran: {events:#?}"
    );
}

/// 6.1.5: a protect node and a read-only node never overlap, whichever of
/// the two is `current`.
#[test]
fn a_protect_node_and_a_read_only_node_never_overlap() {
    let cases: [(&str, &str); 2] = [
        (
            "  - { id: prot, type: agent_task, prompt: p, protect: [\"a.txt\"] }\n  - { id: ro, type: script, script: \"scripts/ro.sh\", runner: sh, read_only: true }\n",
            "  - { from: start, to: prot }\n  - { from: start, to: ro }\n  - { from: prot, to: done }\n  - { from: ro, to: done }\n",
        ),
        (
            "  - { id: ro, type: script, script: \"scripts/ro.sh\", runner: sh, read_only: true }\n  - { id: prot, type: agent_task, prompt: p, protect: [\"a.txt\"] }\n",
            "  - { from: start, to: ro }\n  - { from: start, to: prot }\n  - { from: ro, to: done }\n  - { from: prot, to: done }\n",
        ),
    ];
    for (nodes, edges) in cases {
        let f = fixture(nodes, edges);
        write_script(&f, "ro.sh", "cat a.txt\n");
        let (outcome, events) = drive(&f, "cat a.txt");
        assert_eq!(outcome, RunStatus::Succeeded, "{events:#?}");
        assert_eq!(violations(&events), 0, "{events:#?}");
        assert!(
            !overlap(&events, "ro", "prot") && !overlap(&events, "prot", "ro"),
            "the protect node ran inside the read-only node's window: {events:#?}"
        );
    }
}

/// 6.1.6: an `isolation: full` read-only node batches with a writer: the
/// enforced attempt sees the writer's file appear while it runs. Linux with
/// bubblewrap only, skipped like the sandbox tests.
#[test]
fn an_enforced_read_only_node_batches_with_a_writer() {
    let _lock = common::env_lock();
    if !common::sandbox_or_skip(false) {
        return;
    }
    let home = tempfile::tempdir().unwrap();
    let f = fixture(
        "  - { id: rofull, type: agent_task, prompt: p, read_only: true, isolation: { level: full, network: none } }\n  - { id: w, type: script, script: \"scripts/w.sh\", runner: sh }\n",
        "  - { from: start, to: rofull }\n  - { from: start, to: w }\n  - { from: rofull, to: done }\n  - { from: w, to: done }\n",
    );
    write_script(&f, "w.sh", "echo written > w_done.txt\n");
    // The enforced attempt polls the tree (bound read-only) for the file
    // the writer creates, and says so on its stdout when it saw it appear.
    // The poll is part of the stub itself: the sandbox passes no test env
    // through, so `$RO_AGENT` would arrive empty.
    common::write_sync(
        &f.agent,
        "#!/bin/sh\ni=0\nwhile [ ! -f w_done.txt ] && [ $i -lt 120 ]; do i=$((i+1)); sleep 0.05; done\nif [ -f w_done.txt ]; then echo saw-w; else echo never-saw; fi\nprintf '{\"status\":\"success\"}' > \"$APB_STATUS_FILE\"\necho done\nexit 0\n",
    );
    let _env = [
        EnvVar::set("APB_AGENT_CMD", &f.agent),
        EnvVar::set("HOME", home.path()),
        EnvVar::remove("APB_SANDBOX"),
    ];
    let res = run(&f.root, "ro", None, RunOptions::default()).unwrap();
    let events = read_all(&f.root.join(".apb/runs").join(&res.run_id)).unwrap();
    assert_eq!(res.outcome, RunStatus::Succeeded, "{events:#?}");
    let fin = finished(&events, "rofull");
    assert_eq!(fin.len(), 1, "{events:#?}");
    assert!(
        fin[0].1.contains("saw-w"),
        "the enforced attempt did not overlap the writer: {}",
        fin[0].1
    );
    assert_eq!(fin[0].0, "succeeded");
    let rows = apb_engine::run_view::read_only_rows(&events);
    let row = rows.iter().find(|r| r.node == "rofull").expect("a row");
    assert_eq!(
        row.check.tier,
        ReadOnlyTier::Enforced,
        "the full tier enforces, it does not detect: {rows:?}"
    );
    assert!(row.violated.is_none());
}

/// 6.1.7: mid-batch admission weighs one candidate at a time. A batch of two
/// enforced members holds no writer and no detected member; when the fast
/// one ends, a detected read-only script and a writer become ready
/// together, and only one of them may join (a single filter pass would
/// admit both). The slow member stays up until it sees the writer's file
/// (at most 1 s), so the batch is still open when the two become ready.
/// Linux with bubblewrap only, skipped like the sandbox tests: only the
/// enforced class can build a batch with no writer and no detected member,
/// so where the sandbox is missing (macOS, no user namespaces) only the
/// `batch_admits` unit test covers this rule.
#[test]
fn two_fresh_candidates_of_opposite_classes_never_join_a_batch_together() {
    let _lock = common::env_lock();
    if !common::sandbox_or_skip(false) {
        return;
    }
    let home = tempfile::tempdir().unwrap();
    let f = fixture(
        "  - { id: fast, type: agent_task, prompt: FAST, read_only: true, isolation: { level: full, network: none } }\n  - { id: slow, type: agent_task, prompt: SLOW, read_only: true, isolation: { level: full, network: none } }\n  - { id: d, type: script, script: \"scripts/d.sh\", runner: sh, read_only: true }\n  - { id: w, type: script, script: \"scripts/w.sh\", runner: sh }\n",
        "  - { from: start, to: fast }\n  - { from: start, to: slow }\n  - { from: fast, to: d }\n  - { from: fast, to: w }\n  - { from: d, to: done }\n  - { from: w, to: done }\n  - { from: slow, to: done }\n",
    );
    // Room for every member at once, so only the admission rule keeps the
    // two candidates apart.
    let pb = f.root.join(".apb/playbooks/ro/1.0.0/playbook.yaml");
    let yaml = fs::read_to_string(&pb)
        .unwrap()
        .replace("max_parallel: 2", "max_parallel: 4");
    fs::write(&pb, yaml).unwrap();
    // A handshake that only completes when the two run at once: `w` waits
    // for `d` to start before it writes, `d` waits for the write before it
    // ends, each at most 1 s. Wrongly admitted together, one's start sits
    // inside the other's window on the journal, whichever started first.
    let scratch = f.scratch.display();
    write_script(
        &f,
        "w.sh",
        &format!(
            "i=0\nwhile [ ! -f '{scratch}/d_started' ] && [ $i -lt 20 ]; do i=$((i+1)); sleep 0.05; done\necho out > written.txt\ntouch '{scratch}/w_done'\n"
        ),
    );
    write_script(
        &f,
        "d.sh",
        &format!(
            "touch '{scratch}/d_started'\ni=0\nwhile [ ! -f '{scratch}/w_done' ] && [ $i -lt 20 ]; do i=$((i+1)); sleep 0.05; done\ncat a.txt\n"
        ),
    );
    // The sandbox passes no test env through, so the stub tells the two
    // members apart by their prompt.
    common::write_sync(
        &f.agent,
        "#!/bin/sh\ncase \"$*\" in *SLOW*) i=0; while [ ! -f written.txt ] && [ $i -lt 20 ]; do i=$((i+1)); sleep 0.05; done ;; esac\nprintf '{\"status\":\"success\"}' > \"$APB_STATUS_FILE\"\necho done\nexit 0\n",
    );
    let _env = [
        EnvVar::set("APB_AGENT_CMD", &f.agent),
        EnvVar::set("HOME", home.path()),
        EnvVar::remove("APB_SANDBOX"),
    ];
    let res = run(&f.root, "ro", None, RunOptions::default()).unwrap();
    let events = read_all(&f.root.join(".apb/runs").join(&res.run_id)).unwrap();
    assert_eq!(res.outcome, RunStatus::Succeeded, "{events:#?}");
    assert_eq!(violations(&events), 0, "{events:#?}");
    assert!(
        !overlap(&events, "d", "w") && !overlap(&events, "w", "d"),
        "the writer and the detected script ran at once: {events:#?}"
    );
}
