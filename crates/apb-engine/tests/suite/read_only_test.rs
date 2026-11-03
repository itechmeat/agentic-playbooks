//! A `read_only` node in the detected tier (no sandbox), through the real
//! drive (`scheduler::run`): each kind of change fails the node at once
//! with `read_only_dirty`, whatever retry, fallback, deferral or quota path
//! would otherwise run it again; the evidence names paths and never holds
//! content; an ignored file is not covered; a limit makes the tier partial;
//! the node never shares a batch; and protect still puts its paths back.

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

/// A project tree (a git repository with tracked `a.txt`, `b.txt` and a
/// symlink `link -> a.txt`), the playbook `ro` with `nodes`/`edges`, and a
/// stub agent outside the tree that runs `$RO_AGENT` in its cwd.
struct Fixture {
    _dirs: Vec<tempfile::TempDir>,
    root: PathBuf,
    agent: PathBuf,
    /// A directory outside the tree the stub and scripts may write.
    scratch: PathBuf,
}

const AGENT: &str = r#"#!/bin/sh
n=$(cat "$RO_SCRATCH/count" 2>/dev/null || echo 0); echo $((n+1)) > "$RO_SCRATCH/count"
sh -c "$RO_AGENT"
code=$?
printf '{"status":"success"}' > "$APB_STATUS_FILE"
echo "done $code"
exit $code
"#;

fn fixture(nodes: &str, edges: &str) -> Fixture {
    fixture_in(tempfile::tempdir().unwrap(), nodes, edges)
}

/// [`fixture`] with the tree in `tree`.
fn fixture_in(tree: tempfile::TempDir, nodes: &str, edges: &str) -> Fixture {
    let bin = tempfile::tempdir().unwrap();
    let root = tree.path().to_path_buf();
    init_project(&root).unwrap();
    let pb = format!(
        "schema: 2\nid: ro\nname: Ro\nversion: 1.0.0\ndefaults: {{ profile: main }}\nnodes:\n  - {{ id: start, type: start }}\n{nodes}  - {{ id: done, type: finish, outcome: success }}\nedges:\n{edges}"
    );
    let dir = root.join(".apb/playbooks/ro/1.0.0");
    fs::create_dir_all(dir.join("scripts")).unwrap();
    fs::write(dir.join("playbook.yaml"), pb).unwrap();
    fs::write(root.join(".apb/playbooks/ro/current"), "1.0.0").unwrap();
    common::seed_main(&root);
    fs::write(root.join(".gitignore"), ".apb/\nignored.txt\n").unwrap();
    fs::write(root.join("a.txt"), "alpha\n").unwrap();
    fs::write(root.join("b.txt"), "beta\n").unwrap();
    std::os::unix::fs::symlink("a.txt", root.join("link")).unwrap();
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

/// `start -> work -> done`, with `work` the given node.
fn line(work: &str) -> Fixture {
    fixture(
        &format!("  - {work}\n"),
        "  - { from: start, to: work }\n  - { from: work, to: done }\n",
    )
}

fn write_script(f: &Fixture, name: &str, body: &str) {
    fs::write(
        f.root.join(".apb/playbooks/ro/1.0.0/scripts").join(name),
        body,
    )
    .unwrap();
}

/// Runs `ro` with the stub agent running `agent_cmd`, plus `extra` env.
fn drive(f: &Fixture, agent_cmd: &str) -> (RunStatus, Vec<Event>, String) {
    let _lock = common::env_lock();
    let _env = [
        EnvVar::set("APB_AGENT_CMD", &f.agent),
        EnvVar::set("RO_AGENT", agent_cmd),
        EnvVar::set("RO_SCRATCH", &f.scratch),
    ];
    let res = run(&f.root, "ro", None, RunOptions::default()).unwrap();
    let events = read_all(&f.root.join(".apb/runs").join(&res.run_id)).unwrap();
    (res.outcome, events, res.run_id)
}

fn attempts(events: &[Event], node: &str) -> usize {
    events
        .iter()
        .filter(|e| matches!(&e.payload, EventPayload::AttemptStarted { node: n, .. } if n == node))
        .count()
}

fn violations(events: &[Event]) -> Vec<Vec<(String, String)>> {
    events
        .iter()
        .filter_map(|e| match &e.payload {
            EventPayload::ReadOnlyViolated { changes, .. } => Some(
                changes
                    .iter()
                    .map(|c| (c.path.clone(), c.change.clone()))
                    .collect(),
            ),
            _ => None,
        })
        .collect()
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

fn has(events: &[Event], pred: impl Fn(&EventPayload) -> bool) -> bool {
    events.iter().any(|e| pred(&e.payload))
}

fn pairs(v: &[(&str, &str)]) -> Vec<(String, String)> {
    v.iter()
        .map(|(p, c)| (p.to_string(), c.to_string()))
        .collect()
}

/// One mutation: its name, the shell it runs, and the changes it must show.
type Case = (&'static str, String, Vec<(String, String)>);

const GIT_ID: &str = "git -c user.name=t -c user.email=t@example.invalid -c commit.gpgsign=false";

/// Every kind of change the detected tier covers, made by an agent that
/// reports success, fails the node on the first attempt with the change
/// named: `max_retries: 3` never runs it again.
#[test]
fn each_kind_of_change_fails_the_node_at_once() {
    let cases: [Case; 12] = [
        (
            "modify",
            "echo more >> a.txt".into(),
            pairs(&[("a.txt", "modified")]),
        ),
        (
            "create",
            "echo n > new.txt".into(),
            pairs(&[("new.txt", "added")]),
        ),
        ("delete", "rm b.txt".into(), pairs(&[("b.txt", "deleted")])),
        (
            "chmod",
            "chmod +x b.txt".into(),
            pairs(&[("b.txt", "mode")]),
        ),
        (
            "symlink",
            "ln -sfn b.txt link".into(),
            pairs(&[("link", "link")]),
        ),
        (
            "commit",
            format!("{GIT_ID} commit -q --allow-empty -m sneaky"),
            pairs(&[("HEAD", "git_head"), ("refs", "git_refs")]),
        ),
        (
            "add",
            format!("echo n > new.txt && {GIT_ID} add new.txt"),
            pairs(&[("new.txt", "added"), ("index", "git_index")]),
        ),
        (
            "hide a new file with info/exclude",
            "printf 'hidden.txt\\n' >> .git/info/exclude; echo x > hidden.txt".into(),
            pairs(&[(".git/info/exclude", "git_control")]),
        ),
        (
            "plant a hook",
            "printf '#!/bin/sh\\n' > .git/hooks/pre-commit; chmod +x .git/hooks/pre-commit".into(),
            pairs(&[(".git/hooks/pre-commit", "git_control")]),
        ),
        (
            "configure git",
            format!("{GIT_ID} config core.hooksPath elsewhere"),
            pairs(&[(".git/config", "git_control")]),
        ),
        (
            "hide a new file with a self-ignoring .gitignore",
            "mkdir tools && printf '*\\n' > tools/.gitignore && echo x > tools/conftest.py".into(),
            pairs(&[("tools/.gitignore", "added")]),
        ),
        (
            "edit an apb definition git ignores",
            "echo 9.9.9 > .apb/playbooks/ro/current".into(),
            pairs(&[(".apb/playbooks/ro/current", "modified")]),
        ),
    ];
    for (name, cmd, want) in cases {
        let f = line(
            "{ id: work, type: agent_task, prompt: \"read\", read_only: true, max_retries: 3 }",
        );
        let (outcome, events, _) = drive(&f, &cmd);
        assert_eq!(outcome, RunStatus::Failed, "{name}");
        assert_eq!(attempts(&events, "work"), 1, "{name}: retried");
        assert_eq!(violations(&events), [want], "{name}");
        let fin = finished(&events, "work");
        assert_eq!(fin.len(), 1, "{name}");
        assert_eq!(fin[0].0, "failed", "{name}");
        assert!(
            fin[0].1.starts_with("read_only_dirty: "),
            "{name}: {}",
            fin[0].1
        );
        let tier = events.iter().find_map(|e| match &e.payload {
            EventPayload::ReadOnlyChecked { check, .. } => Some(check.clone()),
            _ => None,
        });
        assert_eq!(tier.map(|t| t.tier), Some(ReadOnlyTier::Detected), "{name}");
        // The attempt that changed the tree is closed failed, then the node.
        let order: Vec<&str> = events
            .iter()
            .filter_map(|e| match &e.payload {
                EventPayload::ReadOnlyViolated { .. } => Some("violated"),
                EventPayload::AttemptFinished { status, .. } if status == "failed" => {
                    Some("attempt_failed")
                }
                EventPayload::NodeFinished { node, .. } if node == "work" => Some("node_finished"),
                _ => None,
            })
            .collect();
        assert_eq!(
            order,
            ["violated", "attempt_failed", "node_finished"],
            "{name}"
        );
    }
}

/// A change inside an initialized submodule fails the node too, though the
/// parent repository's own state does not move.
#[test]
fn a_change_inside_a_submodule_fails_the_node() {
    let f = line("{ id: work, type: agent_task, prompt: \"read\", read_only: true }");
    let inner = f.scratch.join("inner");
    fs::create_dir_all(&inner).unwrap();
    fs::write(inner.join("s.txt"), "inner\n").unwrap();
    git(&inner, &["init", "-q"]);
    git(&inner, &["add", "."]);
    git(&inner, &["commit", "-q", "-m", "inner"]);
    git(
        &f.root,
        &[
            "-c",
            "protocol.file.allow=always",
            "submodule",
            "add",
            "-q",
            inner.to_str().unwrap(),
            "sub",
        ],
    );
    git(&f.root, &["commit", "-q", "-m", "with sub"]);
    let (outcome, events, run_id) = drive(&f, "echo more >> sub/s.txt; echo more >> a.txt");
    assert_eq!(outcome, RunStatus::Failed, "{events:#?}");
    assert_eq!(
        violations(&events),
        [pairs(&[("a.txt", "modified"), ("sub/s.txt", "modified")])]
    );
    // The parent's own changed file keeps its diff stat next to the
    // submodule's change.
    let evidence: serde_json::Value = serde_json::from_slice(
        &fs::read(
            f.root
                .join(".apb/runs")
                .join(run_id)
                .join("read_only/work/1.json"),
        )
        .unwrap(),
    )
    .unwrap();
    let stat = evidence["diff_stat"].as_array().unwrap();
    assert!(
        stat.iter().any(|l| l.as_str().unwrap().contains("a.txt")),
        "{stat:?}"
    );
}

/// A read-only script: a change fails it the same way; a script that only
/// reads passes, and its clean check leaves its evidence too.
#[test]
fn a_read_only_script_is_checked_around_its_run() {
    let f =
        line("{ id: work, type: script, script: \"scripts/w.sh\", runner: sh, read_only: true }");
    write_script(&f, "w.sh", "cat a.txt >/dev/null\nrm a.txt\n");
    let (outcome, events, _) = drive(&f, "true");
    assert_eq!(outcome, RunStatus::Failed);
    assert_eq!(violations(&events), [pairs(&[("a.txt", "deleted")])]);
    assert!(
        finished(&events, "work")[0]
            .1
            .starts_with("read_only_dirty: ")
    );

    let f =
        line("{ id: work, type: script, script: \"scripts/r.sh\", runner: sh, read_only: true }");
    write_script(&f, "r.sh", "cat a.txt\n");
    let (outcome, events, run_id) = drive(&f, "true");
    assert_eq!(outcome, RunStatus::Succeeded);
    assert!(violations(&events).is_empty());
    let evidence: serde_json::Value = serde_json::from_slice(
        &fs::read(
            f.root
                .join(".apb/runs")
                .join(run_id)
                .join("read_only/work/1.json"),
        )
        .unwrap(),
    )
    .unwrap();
    assert_eq!(evidence["clean"], true);
    assert_eq!(evidence["tier"], "detected");
    // A script journals no attempt start; its tier is journaled all the
    // same, so every surface shows the clean check too.
    let rows = apb_engine::run_view::read_only_rows(&events);
    assert_eq!(rows.len(), 1, "{rows:?}");
    assert_eq!((rows[0].node.as_str(), rows[0].attempt), ("work", 1));
    assert_eq!(rows[0].check.tier, ReadOnlyTier::Detected);
    assert!(rows[0].check.entries > 0, "{rows:?}");
    assert!(rows[0].violated.is_none());
}

/// The evidence is a private file of names: a changed `.env` is listed by
/// name, the secret written into it appears nowhere under the run, and the
/// changed paths are capped with a count of the rest.
#[test]
fn the_evidence_is_private_names_only_and_capped() {
    let f = line("{ id: work, type: agent_task, prompt: \"read\", read_only: true }");
    let marker = "ro-marker-7f3a9c";
    let cmd = format!(
        "echo 'API_KEY={marker}' > .env; echo '{marker}' >> a.txt; i=0; while [ $i -lt 205 ]; do echo x > f$i.txt; i=$((i+1)); done"
    );
    let (outcome, events, run_id) = drive(&f, &cmd);
    assert_eq!(outcome, RunStatus::Failed);
    let (changes, truncated, evidence) = events
        .iter()
        .find_map(|e| match &e.payload {
            EventPayload::ReadOnlyViolated {
                changes,
                truncated,
                evidence,
                ..
            } => Some((changes.clone(), *truncated, evidence.clone())),
            _ => None,
        })
        .unwrap();
    assert_eq!(changes.len(), 200);
    assert_eq!(truncated, 7, "207 changes: .env, a.txt and 205 new files");
    assert!(
        changes
            .iter()
            .any(|c| c.path == ".env" && c.change == "added")
    );
    let run_dir = f.root.join(".apb/runs").join(&run_id);
    let rel = evidence.expect("the evidence path is journaled");
    assert_eq!(rel, "read_only/work/1.json");
    let file = run_dir.join(&rel);
    assert_eq!(
        fs::metadata(&file).unwrap().permissions().mode() & 0o777,
        0o600
    );
    let body: serde_json::Value = serde_json::from_slice(&fs::read(&file).unwrap()).unwrap();
    assert_eq!(body["clean"], false);
    assert_eq!(body["truncated"], 7);
    assert_eq!(body["changes"].as_array().unwrap().len(), 200);
    let stat = body["diff_stat"].as_array().unwrap();
    assert!(
        stat.iter().any(|l| l.as_str().unwrap().contains("a.txt")),
        "a changed tracked file is in the diff stat: {stat:?}"
    );
    // No file under the run holds the content.
    let mut stack = vec![run_dir.clone()];
    while let Some(dir) = stack.pop() {
        for entry in fs::read_dir(&dir).unwrap().flatten() {
            let p = entry.path();
            if entry.file_type().unwrap().is_dir() {
                stack.push(p);
            } else if let Ok(text) = fs::read(&p) {
                assert!(
                    !String::from_utf8_lossy(&text).contains(marker),
                    "{} holds the written content",
                    p.display()
                );
            }
        }
    }
}

/// A changed path whose name is credential-shaped is listed only in its
/// scrubbed form: the token appears in no journal line, no evidence and no
/// other file under the run.
#[test]
fn a_credential_shaped_path_name_is_scrubbed() {
    let f = line("{ id: work, type: agent_task, prompt: \"read\", read_only: true }");
    let token = "ghp_A1b2C3d4E5f6G7h8I9j0K1l2M3n4O5p6Q7r8";
    let (outcome, events, run_id) = drive(&f, &format!("echo x > {token}.txt"));
    assert_eq!(outcome, RunStatus::Failed);
    let listed = violations(&events);
    assert_eq!(listed.len(), 1, "{listed:?}");
    assert_eq!(listed[0].len(), 1, "{listed:?}");
    let (path, change) = &listed[0][0];
    assert_eq!(change, "added");
    assert!(!path.contains(token) && path.ends_with(".txt"), "{path}");
    let run_dir = f.root.join(".apb/runs").join(&run_id);
    let mut stack = vec![run_dir];
    while let Some(dir) = stack.pop() {
        for entry in fs::read_dir(&dir).unwrap().flatten() {
            let p = entry.path();
            if entry.file_type().unwrap().is_dir() {
                stack.push(p);
            } else if let Ok(text) = fs::read(&p) {
                assert!(
                    !String::from_utf8_lossy(&text).contains(token),
                    "{} holds the token",
                    p.display()
                );
            }
        }
    }
}

/// A node id that is no safe path segment never leads the evidence out of
/// the run's `read_only/` directory: it is filed under a digest of the id.
#[test]
fn an_unsafe_node_id_keeps_the_evidence_inside_the_run() {
    let f = fixture(
        "  - { id: \"../up\", type: agent_task, prompt: \"read\", read_only: true }\n",
        "  - { from: start, to: \"../up\" }\n  - { from: \"../up\", to: done }\n",
    );
    let (outcome, events, run_id) = drive(&f, "echo more >> a.txt");
    assert_eq!(outcome, RunStatus::Failed, "{events:#?}");
    let evidence = events
        .iter()
        .find_map(|e| match &e.payload {
            EventPayload::ReadOnlyViolated { evidence, .. } => evidence.clone(),
            _ => None,
        })
        .expect("the evidence path is journaled");
    assert!(
        evidence.starts_with("read_only/node-") && !evidence.contains(".."),
        "{evidence}"
    );
    let run_dir = f.root.join(".apb/runs").join(&run_id);
    assert!(run_dir.join(&evidence).is_file());
    assert!(!run_dir.join("up").exists());
}

/// A change to a file git ignores is not covered by the detected tier
/// (documented): the node passes.
#[test]
fn an_ignored_file_change_passes() {
    let f = line("{ id: work, type: agent_task, prompt: \"read\", read_only: true }");
    let (outcome, events, run_id) = drive(&f, "echo scratch > ignored.txt; cat a.txt");
    assert_eq!(outcome, RunStatus::Succeeded);
    assert!(violations(&events).is_empty());
    // The attempt was checked, and the check found the tree unchanged.
    let evidence: serde_json::Value = serde_json::from_slice(
        &fs::read(
            f.root
                .join(".apb/runs")
                .join(run_id)
                .join("read_only/work/1.json"),
        )
        .unwrap(),
    )
    .unwrap();
    assert_eq!(evidence["clean"], true);
    assert_eq!(
        fs::read_to_string(f.root.join("ignored.txt")).unwrap(),
        "scratch\n"
    );
}

/// A limit that stops the before-state short is `detected_partial` with
/// its counts, never "no change": the doctor says so, and a change inside
/// what was covered still fails the node.
#[test]
fn a_limit_makes_the_tier_partial() {
    let f = line("{ id: work, type: agent_task, prompt: \"read\", read_only: true }");
    let _lock = common::env_lock();
    let _env = [
        EnvVar::set("APB_AGENT_CMD", &f.agent),
        EnvVar::set("RO_AGENT", "cat a.txt"),
        EnvVar::set("RO_SCRATCH", &f.scratch),
        EnvVar::set("APB_READ_ONLY_MAX_ENTRIES", "2"),
    ];
    let res = run(&f.root, "ro", None, RunOptions::default()).unwrap();
    assert_eq!(res.outcome, RunStatus::Succeeded);
    let events = read_all(&f.root.join(".apb/runs").join(&res.run_id)).unwrap();
    let check = events
        .iter()
        .find_map(|e| match &e.payload {
            EventPayload::ReadOnlyChecked { check, .. } => Some(check.clone()),
            _ => None,
        })
        .unwrap();
    assert_eq!(check.tier, ReadOnlyTier::DetectedPartial);
    assert_eq!(check.entries, 2);
    assert_eq!(
        check.partial,
        Some(apb_engine::event::ReadOnlyLimit::Entries)
    );
    let rows = apb_engine::run_doctor::diagnose_run(&f.root, &res.run_id).unwrap();
    assert!(
        rows.iter().any(|r| r.status == "warn"
            && r.detail
                .contains("read_only detected after the attempt, partial")),
        "{:?}",
        rows.iter().map(|r| &r.detail).collect::<Vec<_>>()
    );

    // `.apb/config.yaml` and the version's `playbook.yaml` are covered
    // (sorted first): a change there fails.
    let _agent = EnvVar::set("RO_AGENT", "echo '# x' >> .apb/config.yaml");
    let res = run(&f.root, "ro", None, RunOptions::default()).unwrap();
    assert_eq!(res.outcome, RunStatus::Failed);
    let events = read_all(&f.root.join(".apb/runs").join(&res.run_id)).unwrap();
    assert!(has(&events, |p| matches!(
        p,
        EventPayload::ReadOnlyViolated {
            tier: ReadOnlyTier::DetectedPartial,
            ..
        }
    )));
    let rows = apb_engine::run_doctor::diagnose_run(&f.root, &res.run_id).unwrap();
    assert!(
        rows.iter().any(|r| r.status == "fail"
            && r.detail
                .contains("read_only, the tree changed: .apb/config.yaml modified")),
        "{:?}",
        rows.iter().map(|r| &r.detail).collect::<Vec<_>>()
    );
}

/// A limit that stops only the after-state short (the attempt grew the
/// tree past it) makes the check partial too: the evidence of a clean
/// attempt says so, with the after-state's limit, and so does the tier of
/// a violation.
#[test]
fn a_limit_hit_only_after_the_attempt_makes_the_check_partial() {
    let f = line("{ id: work, type: agent_task, prompt: \"read\", read_only: true }");
    let _lock = common::env_lock();
    let _env = [
        EnvVar::set("APB_AGENT_CMD", &f.agent),
        EnvVar::set("RO_AGENT", "cat a.txt"),
        EnvVar::set("RO_SCRATCH", &f.scratch),
    ];
    let res = run(&f.root, "ro", None, RunOptions::default()).unwrap();
    let events = read_all(&f.root.join(".apb/runs").join(&res.run_id)).unwrap();
    let covered = events
        .iter()
        .find_map(|e| match &e.payload {
            EventPayload::ReadOnlyChecked { check, .. } => Some(check.entries),
            _ => None,
        })
        .unwrap();
    // The before-state lists exactly the limit; the new file is past it.
    let _limit = EnvVar::set("APB_READ_ONLY_MAX_ENTRIES", covered.to_string());
    let _agent = EnvVar::set("RO_AGENT", "echo x > zz-new.txt");
    let res = run(&f.root, "ro", None, RunOptions::default()).unwrap();
    assert_eq!(res.outcome, RunStatus::Succeeded);
    let run_dir = f.root.join(".apb/runs").join(&res.run_id);
    let evidence: serde_json::Value =
        serde_json::from_slice(&fs::read(run_dir.join("read_only/work/1.json")).unwrap()).unwrap();
    assert_eq!(evidence["clean"], true);
    assert_eq!(evidence["tier"], "detected_partial", "{evidence}");
    assert_eq!(evidence["partial"], "entries", "{evidence}");
    assert_eq!(evidence["after_partial"], "entries", "{evidence}");

    fs::remove_file(f.root.join("zz-new.txt")).unwrap();
    let _agent = EnvVar::set("RO_AGENT", "echo x >> a.txt; echo x > zz-new.txt");
    let res = run(&f.root, "ro", None, RunOptions::default()).unwrap();
    assert_eq!(res.outcome, RunStatus::Failed);
    let events = read_all(&f.root.join(".apb/runs").join(&res.run_id)).unwrap();
    assert!(has(&events, |p| matches!(
        p,
        EventPayload::ReadOnlyViolated {
            tier: ReadOnlyTier::DetectedPartial,
            ..
        }
    )));
}

/// The after-check comes before every path that would run the node again:
/// a profile fallback after a failed attempt, a quota pause, and a deferred
/// retry. Each attempt that changed the tree is the node's last.
#[test]
fn a_violation_bypasses_fallback_quota_and_deferral() {
    // A fallback executor the chain would walk to after a failure.
    let f =
        line("{ id: work, type: agent_task, prompt: \"read\", read_only: true, max_retries: 2 }");
    common::seed_profile(
        &f.root,
        "main",
        "claude-code",
        "haiku",
        &[("claude-code", "sonnet")],
    );
    let (outcome, events, _) = drive(&f, "echo x >> a.txt; exit 1");
    assert_eq!(outcome, RunStatus::Failed);
    assert_eq!(attempts(&events, "work"), 1, "fallback ran");
    assert_eq!(violations(&events).len(), 1);

    // A quota failure would pause the run for a later retry.
    let f =
        line("{ id: work, type: agent_task, prompt: \"read\", read_only: true, max_retries: 2 }");
    let (outcome, events, _) = drive(
        &f,
        "echo x >> a.txt; echo 'Error: 429 Too Many Requests, rate limit reached' >&2; exit 1",
    );
    assert_eq!(outcome, RunStatus::Failed);
    assert_eq!(attempts(&events, "work"), 1);
    assert!(!has(&events, |p| matches!(
        p,
        EventPayload::RunPaused { .. }
    )));
    assert_eq!(violations(&events).len(), 1);

    // A deferred retry would come back after its delay.
    let f = line(
        "{ id: work, type: agent_task, prompt: \"read\", read_only: true, max_retries: 2, retry_delay: { initial: 1s } }",
    );
    let (outcome, events, _) = drive(&f, "echo x >> a.txt; exit 1");
    assert_eq!(outcome, RunStatus::Failed);
    assert_eq!(attempts(&events, "work"), 1);
    assert!(!has(&events, |p| matches!(
        p,
        EventPayload::RetryDeferred { .. }
    )));
    assert_eq!(violations(&events).len(), 1);
    assert_eq!(
        fs::read_to_string(f.scratch.join("count")).unwrap().trim(),
        "1"
    );
}

/// A violation takes the node's failure route like any failure; under
/// `on_failure: stop` the run stops on the node with its `read_only_dirty`
/// reason.
#[test]
fn a_violation_routes_like_a_failure() {
    let f = fixture(
        "  - { id: work, type: agent_task, prompt: \"read\", read_only: true }\n  - { id: handled, type: finish, outcome: success }\n",
        "  - { from: start, to: work }\n  - { from: work, to: done, condition: { type: node_status, node: work, equals: success } }\n  - { from: work, to: handled, condition: { type: node_status, node: work, equals: failure } }\n",
    );
    let (outcome, events, _) = drive(&f, "echo x >> a.txt");
    assert_eq!(outcome, RunStatus::Succeeded);
    assert!(has(&events, |p| matches!(
        p,
        EventPayload::NodeFinished { node, .. } if node == "handled"
    )));

    // `on_failure: stop` ends the run with the node's own reason.
    let f = fixture(
        "  - { id: work, type: agent_task, prompt: \"read\", read_only: true }\n",
        "  - { from: start, to: work }\n  - { from: work, to: done, condition: { type: node_status, node: work, equals: success } }\n",
    );
    let pb = f.root.join(".apb/playbooks/ro/1.0.0/playbook.yaml");
    let yaml = fs::read_to_string(&pb).unwrap().replace(
        "defaults: { profile: main }",
        "defaults: { profile: main, on_failure: stop }",
    );
    fs::write(&pb, yaml).unwrap();
    let (outcome, events, _) = drive(&f, "echo x >> a.txt");
    assert_eq!(outcome, RunStatus::Failed);
    let error = events
        .iter()
        .find_map(|e| match &e.payload {
            EventPayload::RunError { node, reason } => Some((node.clone(), reason.clone())),
            _ => None,
        })
        .expect("a run error");
    assert_eq!(error.0.as_deref(), Some("work"));
    assert!(error.1.contains("read_only_dirty"), "{}", error.1);
}

/// A read-only node with `protect`: a change to a protected path is the
/// read-only violation (the node fails at once, no retry), and protect
/// still puts the file back.
#[test]
fn protect_still_restores_a_read_only_node_s_change() {
    let f = line(
        "{ id: work, type: agent_task, prompt: \"read\", read_only: true, max_retries: 3, protect: [\"a.txt\"] }",
    );
    let (outcome, events, _) = drive(&f, "echo tampered > a.txt");
    assert_eq!(outcome, RunStatus::Failed);
    assert_eq!(attempts(&events, "work"), 1);
    assert_eq!(violations(&events), [pairs(&[("a.txt", "modified")])]);
    assert!(has(&events, |p| matches!(
        p,
        EventPayload::ProtectedPathsModified { .. }
    )));
    assert_eq!(fs::read_to_string(f.root.join("a.txt")).unwrap(), "alpha\n");
    let out = &finished(&events, "work")[0].1;
    assert!(out.starts_with("read_only_dirty: "), "{out}");
    assert!(out.contains("protected path modified: a.txt"), "{out}");
}

/// A detected-tier read-only node never joins a batch, so a sibling's write
/// during its attempt cannot be blamed on it: `ro` waits (bounded) for the
/// writer to finish, which in a batch would happen while `ro` runs.
#[test]
fn a_detected_read_only_node_never_joins_a_batch() {
    let f = fixture(
        "  - { id: w, type: script, script: \"scripts/w.sh\", runner: sh }\n  - { id: ro, type: script, script: \"scripts/ro.sh\", runner: sh, read_only: true }\n  - { id: j, type: prompt, prompt: \"joined\" }\n",
        "  - { from: start, to: w }\n  - { from: start, to: ro }\n  - { from: w, to: j }\n  - { from: ro, to: j }\n  - { from: j, to: done }\n",
    );
    let scratch = f.scratch.display();
    write_script(
        &f,
        "w.sh",
        &format!("echo out > written.txt\ntouch '{scratch}/w_done'\n"),
    );
    write_script(
        &f,
        "ro.sh",
        &format!(
            "i=0\nwhile [ ! -f '{scratch}/w_done' ] && [ $i -lt 100 ]; do i=$((i+1)); sleep 0.05; done\ncat a.txt\n"
        ),
    );
    let (outcome, events, _) = drive(&f, "true");
    assert!(violations(&events).is_empty(), "{:?}", violations(&events));
    assert_eq!(outcome, RunStatus::Succeeded);
    // `ro`'s execution contains no event of `w`.
    let pos = |pred: &dyn Fn(&EventPayload) -> bool| events.iter().position(|e| pred(&e.payload));
    let ro_start =
        pos(&|p| matches!(p, EventPayload::NodeStarted { node, .. } if node == "ro")).unwrap();
    let ro_end =
        pos(&|p| matches!(p, EventPayload::NodeFinished { node, .. } if node == "ro")).unwrap();
    let w_events: Vec<usize> = events
        .iter()
        .enumerate()
        .filter(|(_, e)| match &e.payload {
            EventPayload::NodeStarted { node, .. } | EventPayload::NodeFinished { node, .. } => {
                node == "w"
            }
            _ => false,
        })
        .map(|(i, _)| i)
        .collect();
    assert!(
        w_events.iter().all(|i| *i < ro_start || *i > ro_end),
        "w ran while ro did: w at {w_events:?}, ro {ro_start}..{ro_end}"
    );
}

/// Adds `outputs` (a top-level `outputs:` block) to the fixture's playbook.
fn declare_outputs(f: &Fixture, outputs: &str) {
    let path = f.root.join(".apb/playbooks/ro/1.0.0/playbook.yaml");
    let mut yaml = fs::read_to_string(&path).unwrap();
    yaml.push_str(&format!("outputs:\n{outputs}"));
    fs::write(&path, yaml).unwrap();
}

/// A read-only execution that changed its tree failed, so an output from
/// it is missing `not_succeeded`; with `pick: last` over a node that
/// succeeded before, the output is the earlier success, never the dirty
/// execution's text.
#[test]
fn an_output_from_a_violating_execution_is_missing_or_the_earlier_success() {
    let f =
        line("{ id: work, type: script, script: \"scripts/w.sh\", runner: sh, read_only: true }");
    write_script(&f, "w.sh", "echo dirty >> a.txt\necho wrote\n");
    declare_outputs(&f, "  out: { from: { node: work } }\n");
    let (outcome, events, run_id) = drive(&f, "true");
    assert_eq!(outcome, RunStatus::Failed);
    let run_dir = f.root.join(".apb/runs").join(&run_id);
    let o = serde_json::to_value(apb_engine::run_outputs::of_run(&run_dir, &events)).unwrap();
    assert_eq!(o["outputs"][0]["name"], "out", "{o:#}");
    assert_eq!(o["outputs"][0]["state"], "missing", "{o:#}");
    assert_eq!(o["outputs"][0]["reason"], "not_succeeded", "{o:#}");

    // `work` runs twice: clean, then dirty on its second visit.
    let f = fixture(
        "  - { id: work, type: agent_task, prompt: \"read\", read_only: true }\n  - { id: again, type: script, script: \"scripts/again.sh\", runner: sh }\n",
        "  - { from: start, to: work }\n  - { from: work, to: again, condition: { type: node_status, node: work, equals: success }, max_traversals: 1 }\n  - { from: again, to: work }\n  - { from: work, to: done, fallback: true }\n",
    );
    write_script(&f, "again.sh", "echo again\n");
    declare_outputs(&f, "  last: { from: { node: work } }\n");
    let (_, events, run_id) = drive(
        &f,
        "[ \"$(cat \"$RO_SCRATCH/count\")\" -ge 2 ] && echo dirty >> a.txt; true",
    );
    let runs = finished(&events, "work");
    assert_eq!(runs.len(), 2, "{runs:?}");
    assert_eq!(runs[0].0, "succeeded");
    assert_eq!(runs[1].0, "failed");
    assert!(runs[1].1.starts_with("read_only_dirty: "), "{runs:?}");
    let run_dir = f.root.join(".apb/runs").join(&run_id);
    let o = serde_json::to_value(apb_engine::run_outputs::of_run(&run_dir, &events)).unwrap();
    let last = &o["outputs"][0];
    assert_eq!(last["state"], "value", "{o:#}");
    assert_eq!(last["value"], runs[0].1.as_str(), "{o:#}");
    assert_eq!(last["source"]["execution"]["ordinal"], 1, "{o:#}");
}

/// An interactive read-only node asks, is resumed with the answer, and the
/// resumed attempt writes the tree and fails. The failed resume would
/// otherwise be re-run as a reprompt attempt (with no retries left to
/// spend): the violation is final, so the node fails `read_only_dirty`
/// and nothing runs it again.
#[test]
fn a_resumed_attempt_that_changed_the_tree_is_never_reprompted() {
    let f = line(
        "{ id: work, type: agent_task, prompt: p, profile: arch, interactive: true, read_only: true }",
    );
    common::seed_profile(&f.root, "arch", "claude", "haiku", &[]);
    let cfg = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    fs::write(
        cfg.path().join("config.yaml"),
        "agents:\n  claude:\n    invocation:\n      argv: [\"-p\", \"{prompt}\", \"--model\", \"{model}\"]\n      interaction: resume\n",
    )
    .unwrap();
    // The first invocation captures a session and asks; the resume writes
    // `dirty.txt` into its working directory (the tree) and fails; any
    // other invocation succeeds. Every invocation is counted.
    let stub = f.scratch.join("interactive.sh");
    common::write_sync(
        &stub,
        &format!(
            "#!/bin/sh\nc=\"{0}/count\"\nn=$(cat \"$c\" 2>/dev/null || echo 0); echo $((n+1)) > \"$c\"\ncase \"$*\" in\n  *--resume*) echo dirty > dirty.txt; echo boom 1>&2; exit 1 ;;\nesac\nif [ \"$n\" = \"0\" ]; then\n  printf '%s\\n' '{{\"session_id\":\"sess-ro\"}}'\n  printf '%s\\n' '<<<apb:question>>>'\n  printf '%s\\n' '{{\"question\":\"Which DB?\"}}'\n  exit 0\nfi\necho done\nexit 0\n",
            f.scratch.display()
        ),
    );
    fs::set_permissions(&stub, fs::Permissions::from_mode(0o755)).unwrap();
    let _lock = common::env_lock();
    let _env = [
        EnvVar::set("APB_AGENT_CMD", &stub),
        EnvVar::set("APB_CONFIG_DIR", cfg.path()),
        EnvVar::set("HOME", home.path()),
    ];
    let root = f.root.clone();
    let drive = std::thread::spawn(move || run(&root, "ro", None, RunOptions::default()).unwrap());
    let runs = f.root.join(".apb/runs");
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
    let run_dir = loop {
        let asked = fs::read_dir(&runs).ok().and_then(|rd| {
            rd.flatten().map(|e| e.path()).find(|dir| {
                read_all(dir).is_ok_and(|ev| {
                    has(
                        &ev,
                        |p| matches!(p, EventPayload::QuestionAsked { node, .. } if node == "work"),
                    )
                })
            })
        });
        if let Some(dir) = asked {
            break dir;
        }
        assert!(
            std::time::Instant::now() < deadline && !drive.is_finished(),
            "the node never asked its question"
        );
        std::thread::yield_now();
    };
    apb_engine::question::post_answer(&run_dir, Some("work"), "pg", "human").unwrap();
    let res = drive.join().unwrap();
    let events = read_all(&run_dir).unwrap();
    assert_eq!(res.outcome, RunStatus::Failed, "{events:#?}");
    let fin = finished(&events, "work");
    assert_eq!(fin.len(), 1, "{fin:?}");
    assert!(fin[0].1.starts_with("read_only_dirty: "), "{fin:?}");
    assert_eq!(violations(&events), vec![pairs(&[("dirty.txt", "added")])]);
    assert!(
        !has(
            &events,
            |p| matches!(p, EventPayload::SupervisorAction { action, .. } if action == "interaction_downgraded")
        ),
        "a read-only violation is never downgraded to a reprompt"
    );
    assert_eq!(
        attempts(&events, "work"),
        2,
        "the asking attempt and the resume only"
    );
    assert_eq!(
        fs::read_to_string(f.scratch.join("count")).unwrap().trim(),
        "2"
    );
}

/// A writable extra path inside a read-only node's tree cannot hold at any
/// isolation level: without a sandbox (`best_effort` here, the sandbox
/// off) the attempt is refused as it is in one, and the agent never runs.
/// The sandboxed twin is `sandbox_run_test::a_writable_extra_inside_a_read_only_tree_is_refused`.
#[test]
fn a_writable_extra_inside_the_tree_is_refused_without_a_sandbox() {
    // A tree outside `/tmp`, which no extra path may reach (V114).
    let f = fixture_in(
        tempfile::tempdir_in(env!("CARGO_TARGET_TMPDIR")).unwrap(),
        "  - { id: work, type: agent_task, prompt: p, read_only: true, isolation: { level: best_effort, writable: [__TREE__] } }\n",
        "  - { from: start, to: work }\n  - { from: work, to: done }\n",
    );
    let pb = f.root.join(".apb/playbooks/ro/1.0.0/playbook.yaml");
    let text = fs::read_to_string(&pb).unwrap().replace(
        "__TREE__",
        &format!("'{}'", f.root.canonicalize().unwrap().display()),
    );
    fs::write(&pb, text).unwrap();
    let (outcome, events) = {
        let _lock = common::env_lock();
        let _env = [
            EnvVar::set("APB_AGENT_CMD", &f.agent),
            EnvVar::set("RO_AGENT", "echo n > new.txt"),
            EnvVar::set("RO_SCRATCH", &f.scratch),
            EnvVar::set("APB_SANDBOX", "off"),
        ];
        let res = run(&f.root, "ro", None, RunOptions::default()).unwrap();
        let events = read_all(&f.root.join(".apb/runs").join(&res.run_id)).unwrap();
        (res.outcome, events)
    };
    assert_eq!(outcome, RunStatus::Failed, "{events:#?}");
    let fin = finished(&events, "work");
    assert_eq!(fin.len(), 1, "{fin:?}");
    assert!(
        fin[0].1.starts_with("isolation_unavailable")
            && fin[0]
                .1
                .contains("lies inside the tree of a read_only node"),
        "{fin:?}"
    );
    assert!(!f.scratch.join("count").exists(), "the agent never runs");
    assert!(!f.root.join("new.txt").exists());
}
