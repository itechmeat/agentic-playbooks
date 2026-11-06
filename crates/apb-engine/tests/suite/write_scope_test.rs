//! The declared write scope (spec node-contracts D3) end to end: a change
//! outside `writes`, or more files than `max_changed_files`, fails the node
//! at once with no retry, and the change stays in the tree.

use std::fs;
use std::path::Path;
use std::process::Command;

use apb_core::registry::init_project;
use apb_engine::event::{EventPayload, read_all};
use apb_engine::state::RunStatus;

use crate::common;
use crate::token_economy_test::{invocations, recording_stub, run_with_stub, seed_playbook};

const OK: &str = "printf '\\n```yaml\\nstatus: success\\nsummary: ok\\n```\\n'";

/// The single run directory under `root`.
fn run_dir(root: &Path) -> std::path::PathBuf {
    fs::read_dir(root.join(".apb/runs"))
        .unwrap()
        .filter_map(|e| e.ok().map(|e| e.path()))
        .find(|p| p.is_dir())
        .expect("one run")
}

/// The node's terminal output.
fn node_output(root: &Path, node: &str) -> String {
    read_all(&run_dir(root))
        .unwrap()
        .iter()
        .find_map(|e| match &e.payload {
            EventPayload::NodeFinished {
                node: n,
                output,
                status,
                ..
            } if n == node && status != "succeeded" => Some(output.clone()),
            _ => None,
        })
        .unwrap_or_default()
}

fn failure_kinds(root: &Path, node: &str) -> Vec<Option<String>> {
    read_all(&run_dir(root))
        .unwrap()
        .iter()
        .filter_map(|e| match &e.payload {
            EventPayload::AttemptFinished {
                node: n,
                failure_kind,
                ..
            } if n == node => Some(failure_kind.clone()),
            _ => None,
        })
        .collect()
}

/// Seeds a git project with the playbook YAML and the scripts it names.
fn seed(root: &Path, yaml: &str, scripts: &[(&str, String)]) {
    init_project(root).unwrap();
    fs::create_dir_all(root.join("src")).unwrap();
    fs::create_dir_all(root.join("docs")).unwrap();
    fs::write(root.join("src/seed.rs"), "seed\n").unwrap();
    fs::write(root.join("docs/seed.md"), "seed\n").unwrap();
    fs::write(root.join(".gitignore"), ".apb/\ninv/\nstub.sh\n").unwrap();
    seed_playbook(root, "ws", yaml);
    let vdir = root.join(".apb/playbooks/ws/1.0.0/scripts");
    fs::create_dir_all(&vdir).unwrap();
    for (name, body) in scripts {
        fs::write(vdir.join(name), body).unwrap();
    }
    common::seed_main(root);
    let git = |args: &[&str]| {
        let mut full = vec!["-C", root.to_str().unwrap(), "-c", "commit.gpgsign=false"];
        full.extend_from_slice(args);
        Command::new("git")
            .args(&full)
            .output()
            .unwrap()
            .status
            .success()
    };
    assert!(git(&["init", "-q"]));
    assert!(git(&["config", "user.email", "t@t"]));
    assert!(git(&["config", "user.name", "t"]));
    assert!(git(&["add", "."]));
    assert!(git(&["commit", "-qm", "c1"]));
}

const SCRIPT_PB: &str = r#"
schema: 2
id: ws
name: WS
version: 1.0.0
defaults: { profile: main }
nodes:
  - { id: start, type: start }
  - { id: s, type: script, script: scripts/w.sh, runner: sh, writes: ["src/**"] }
  - { id: done, type: finish, outcome: success }
edges:
  - { from: start, to: s }
  - { from: s, to: done }
"#;

/// D2.4.1: a script with `writes: ["src/**"]` writing `src/a.rs` succeeds;
/// writing `docs/a.md` fails with `write_scope:` and the file keeps the
/// change.
#[test]
fn an_in_scope_write_passes_and_an_outside_one_fails() {
    // In scope.
    let dir = tempfile::tempdir().unwrap();
    seed(
        dir.path(),
        SCRIPT_PB,
        &[("w.sh", "echo x > src/a.rs\necho done\n".into())],
    );
    assert_eq!(
        run_with_stub(dir.path(), "ws", "/bin/true", None),
        RunStatus::Succeeded
    );
    // Outside the scope.
    let dir = tempfile::tempdir().unwrap();
    seed(
        dir.path(),
        SCRIPT_PB,
        &[("w.sh", "echo x > docs/a.md\necho done\n".into())],
    );
    assert_eq!(
        run_with_stub(dir.path(), "ws", "/bin/true", None),
        RunStatus::Failed
    );
    let out = node_output(dir.path(), "s");
    assert!(out.starts_with("write_scope:"), "{out}");
    assert!(out.contains("docs/a.md"), "{out}");
    assert_eq!(
        fs::read_to_string(dir.path().join("docs/a.md")).unwrap(),
        "x\n",
        "the change stays in the tree"
    );
}

/// D2.4.2: an agent_task stand-in with `max_retries: 2` that writes outside
/// its scope: one attempt only, `failure_kind` `write_scope`, no retry.
#[test]
fn an_outside_write_fails_the_agent_node_without_a_retry() {
    let dir = tempfile::tempdir().unwrap();
    let yaml = r#"
schema: 2
id: ws
name: WS
version: 1.0.0
defaults: { profile: main }
nodes:
  - { id: start, type: start }
  - { id: w, type: agent_task, prompt: "do", max_retries: 2, writes: ["src/**"] }
  - { id: done, type: finish, outcome: success }
edges:
  - { from: start, to: w }
  - { from: w, to: done }
"#;
    seed(dir.path(), yaml, &[]);
    let stub = recording_stub(dir.path(), &format!("echo x > docs/a.md; {OK}"));
    assert_eq!(
        run_with_stub(dir.path(), "ws", &stub, None),
        RunStatus::Failed
    );
    assert_eq!(
        invocations(dir.path())
            .iter()
            .filter(|i| i.node == "w")
            .count(),
        1
    );
    let kinds = failure_kinds(dir.path(), "w");
    assert_eq!(kinds, vec![Some("write_scope".into())], "{kinds:?}");
}

/// D2.4.3: `max_changed_files: 1` and two changed files fails; a commit of
/// one in-scope file passes.
#[test]
fn the_max_changed_files_cap_counts_files() {
    let yaml = r#"
schema: 2
id: ws
name: WS
version: 1.0.0
defaults: { profile: main }
nodes:
  - { id: start, type: start }
  - { id: s, type: script, script: scripts/w.sh, runner: sh, max_changed_files: 1 }
  - { id: done, type: finish, outcome: success }
edges:
  - { from: start, to: s }
  - { from: s, to: done }
"#;
    // Two changed files without a scope: over the cap.
    let dir = tempfile::tempdir().unwrap();
    seed(
        dir.path(),
        yaml,
        &[(
            "w.sh",
            "echo x > src/a.rs\necho y > docs/a.md\necho done\n".into(),
        )],
    );
    assert_eq!(
        run_with_stub(dir.path(), "ws", "/bin/true", None),
        RunStatus::Failed
    );
    let out = node_output(dir.path(), "s");
    assert!(out.starts_with("write_scope:"), "{out}");
    assert!(out.contains("more than its max_changed_files 1"), "{out}");

    // One committed change passes: the repository state is not a file
    // change, and the work-tree change it committed is one file.
    let dir = tempfile::tempdir().unwrap();
    seed(
        dir.path(),
        yaml,
        &[(
            "w.sh",
            "echo x > src/a.rs\ngit add src/a.rs\ngit -c commit.gpgsign=false commit -qm w\necho done\n"
                .into(),
        )],
    );
    assert_eq!(
        run_with_stub(dir.path(), "ws", "/bin/true", None),
        RunStatus::Succeeded
    );
}

const TEMPLATE_PB: &str = r#"
schema: 2
id: ws
name: WS
version: 1.0.0
defaults: { profile: main }
nodes:
  - { id: start, type: start }
  - { id: plan, type: agent_task, prompt: "plan", outputs: { fields: [files] } }
  - { id: w, type: script, script: scripts/w.sh, runner: sh, writes: "{{nodes.plan.output.files}}" }
  - { id: done, type: finish, outcome: success }
edges:
  - { from: start, to: plan }
  - { from: plan, to: w }
  - { from: w, to: done }
"#;

/// D2.4.4: the template form from an earlier node's array field scopes the
/// script; an empty render refuses the node before any attempt; a `../x`
/// entry refuses it.
#[test]
fn a_template_scope_renders_once_per_execution() {
    // A render naming an in-scope glob: the script writes it and passes.
    let dir = tempfile::tempdir().unwrap();
    seed(
        dir.path(),
        TEMPLATE_PB,
        &[("w.sh", "echo x > src/a.rs\necho done\n".into())],
    );
    let stub = recording_stub(
        dir.path(),
        &format!(
            "printf '%s' '{{\"status\":\"success\",\"outputs\":{{\"files\":[\"src/**\"]}}}}' > \"$APB_STATUS_FILE\"; {OK}"
        ),
    );
    assert_eq!(
        run_with_stub(dir.path(), "ws", &stub, None),
        RunStatus::Succeeded
    );

    // An empty render refuses the node before any attempt.
    let dir = tempfile::tempdir().unwrap();
    seed(
        dir.path(),
        TEMPLATE_PB,
        &[("w.sh", "echo x > src/a.rs\necho done\n".into())],
    );
    let stub = recording_stub(
        dir.path(),
        &format!(
            "printf '%s' '{{\"status\":\"success\",\"outputs\":{{}}}}' > \"$APB_STATUS_FILE\"; {OK}"
        ),
    );
    assert_eq!(
        run_with_stub(dir.path(), "ws", &stub, None),
        RunStatus::Failed
    );
    let out = node_output(dir.path(), "w");
    assert!(out.contains("rendered empty"), "{out}");
    assert!(
        !invocations(dir.path()).iter().any(|i| i.node == "w"),
        "the script never runs"
    );

    // A rendered `../x` refuses it.
    let dir = tempfile::tempdir().unwrap();
    seed(
        dir.path(),
        TEMPLATE_PB,
        &[("w.sh", "echo x > src/a.rs\necho done\n".into())],
    );
    let stub = recording_stub(
        dir.path(),
        &format!(
            "printf '%s' '{{\"status\":\"success\",\"outputs\":{{\"files\":[\"../x\"]}}}}' > \"$APB_STATUS_FILE\"; {OK}"
        ),
    );
    assert_eq!(
        run_with_stub(dir.path(), "ws", &stub, None),
        RunStatus::Failed
    );
    let out = node_output(dir.path(), "w");
    assert!(out.contains("../x"), "{out}");
    assert!(
        out.contains("not a path inside the node's directory"),
        "{out}"
    );
}

/// D2.4.5: two scoped scripts with disjoint globs and `max_parallel: 2`
/// never overlap in time; a scoped script and an unscoped one never
/// overlap; two unscoped scripts still do.
#[test]
fn scoped_writers_never_share_a_batch() {
    let node = |id: &str, writes: &str| {
        format!("  - {{ id: {id}, type: script, script: scripts/{id}.sh, runner: sh{writes} }}\n")
    };
    let pb = |a: &str, b: &str| {
        format!(
            "schema: 2\nid: ws\nname: WS\nversion: 1.0.0\ndefaults: {{ profile: main, max_parallel: 2 }}\nnodes:\n  - {{ id: start, type: start }}\n{}{}  - {{ id: done, type: finish, outcome: success }}\nedges:\n  - {{ from: start, to: a }}\n  - {{ from: start, to: b }}\n  - {{ from: a, to: done }}\n  - {{ from: b, to: done }}\n",
            node("a", a),
            node("b", b)
        )
    };
    let overlap = |root: &Path| -> bool {
        let events = read_all(&run_dir(root)).unwrap();
        let started = |node: &str| {
            events.iter().find_map(|e| match &e.payload {
                EventPayload::NodeStarted { node: n, .. } if n == node => Some(e.seq),
                _ => None,
            })
        };
        let finished = |node: &str| {
            events.iter().find_map(|e| match &e.payload {
                EventPayload::NodeFinished { node: n, .. } if n == node => Some(e.seq),
                _ => None,
            })
        };
        // The attempts' intervals overlap when each started before the
        // other's node finished (order-agnostic).
        match (started("a"), finished("a"), started("b"), finished("b")) {
            (Some(sa), Some(fa), Some(sb), Some(fb)) => sa.max(sb) < fa.min(fb),
            _ => false,
        }
    };
    let script_body = "sleep 0.5\necho done\n".to_string();

    // Two scoped scripts with disjoint globs: never overlap.
    let dir = tempfile::tempdir().unwrap();
    seed(
        dir.path(),
        &pb(", writes: [\"src/**\"]", ", writes: [\"docs/**\"]"),
        &[("a.sh", script_body.clone()), ("b.sh", script_body.clone())],
    );
    assert_eq!(
        run_with_stub(dir.path(), "ws", "/bin/true", None),
        RunStatus::Succeeded
    );
    assert!(!overlap(dir.path()), "scoped siblings must not overlap");

    // A scoped and an unscoped one: never overlap.
    let dir = tempfile::tempdir().unwrap();
    seed(
        dir.path(),
        &pb(", writes: [\"src/**\"]", ""),
        &[("a.sh", script_body.clone()), ("b.sh", script_body.clone())],
    );
    assert_eq!(
        run_with_stub(dir.path(), "ws", "/bin/true", None),
        RunStatus::Succeeded
    );
    assert!(!overlap(dir.path()), "scoped vs unscoped must not overlap");

    // Two unscoped scripts still overlap (today's behaviour).
    let dir = tempfile::tempdir().unwrap();
    seed(
        dir.path(),
        &pb("", ""),
        &[("a.sh", script_body.clone()), ("b.sh", script_body.clone())],
    );
    assert_eq!(
        run_with_stub(dir.path(), "ws", "/bin/true", None),
        RunStatus::Succeeded
    );
    assert!(overlap(dir.path()), "unscoped siblings keep batching");
}

/// D2.4.6: with `APB_READ_ONLY_MAX_ENTRIES` lowered (debug build), a tree
/// over the limit before the attempt fails the node with the
/// `too large to compare whole` reason even when the attempt shrinks it
/// back under the limit (only the before state is partial), and so does a
/// tree the attempt grows past it (only the after state is).
#[test]
fn a_partial_comparison_fails_the_node() {
    use apb_core::fingerprint::{TreeLimits, TreeState};
    let yaml = r#"
schema: 2
id: ws
name: WS
version: 1.0.0
defaults: { profile: main }
nodes:
  - { id: start, type: start }
  - { id: s, type: script, script: scripts/w.sh, runner: sh, writes: ["src/**"] }
  - { id: done, type: finish, outcome: success }
edges:
  - { from: start, to: s }
  - { from: s, to: done }
"#;
    let git = |root: &Path, args: &[&str]| {
        let mut full = vec!["-C", root.to_str().unwrap(), "-c", "commit.gpgsign=false"];
        full.extend_from_slice(args);
        assert!(Command::new("git").args(&full).status().unwrap().success());
    };
    // The smallest entries limit the seeded tree fits under whole: five more
    // files take any state past it.
    let probe = tempfile::tempdir().unwrap();
    seed(probe.path(), yaml, &[("w.sh", "echo done\n".into())]);
    let fits = |entries: u64| {
        TreeState::take(
            probe.path(),
            TreeLimits {
                entries,
                ..TreeLimits::default()
            },
        )
        .partial()
        .is_none()
    };
    let limit = (1..200)
        .find(|n| fits(*n))
        .expect("the seeded tree fits a limit");
    // The limit is process-global, so the env lock is held for the whole
    // test (the guards put the vars back even when an assertion fails) and
    // the runs go inline (run_with_stub would take the same lock).
    let _env = common::env_lock();
    let _limit = common::EnvVar::set("APB_READ_ONLY_MAX_ENTRIES", limit.to_string());
    let _agent = common::EnvVar::set("APB_AGENT_CMD", "/bin/true");
    let run_and_read = |root: &Path| {
        let res = apb_engine::scheduler::run(root, "ws", None, Default::default()).unwrap();
        assert_eq!(res.outcome, RunStatus::Failed);
        let out = node_output(root, "s");
        assert!(out.starts_with("write_scope:"), "{out}");
        assert!(out.contains("too large to compare whole"), "{out}");
        let evidence: serde_json::Value =
            serde_json::from_slice(&fs::read(run_dir(root).join("writes/s/1.json")).unwrap())
                .unwrap();
        evidence
    };

    // The before state is over the limit; the script deletes the extra files
    // again, so the after walk reads the tree whole: only the before state
    // is partial.
    let dir = tempfile::tempdir().unwrap();
    seed(
        dir.path(),
        yaml,
        &[("w.sh", "rm src/f*.rs\necho done\n".into())],
    );
    for i in 0..5 {
        fs::write(dir.path().join(format!("src/f{i}.rs")), "x\n").unwrap();
    }
    git(dir.path(), &["add", "."]);
    git(dir.path(), &["commit", "-qm", "more"]);
    let evidence = run_and_read(dir.path());
    assert_eq!(evidence["partial"], "entries", "{evidence}");
    assert!(evidence["after_partial"].is_null(), "{evidence}");

    // The attempt grows the tree past the limit: only the after state is
    // partial.
    let dir = tempfile::tempdir().unwrap();
    seed(
        dir.path(),
        yaml,
        &[(
            "w.sh",
            "for i in 1 2 3 4 5; do echo x > src/g$i.rs; done\necho done\n".into(),
        )],
    );
    let evidence = run_and_read(dir.path());
    assert_eq!(evidence["after_partial"], "entries", "{evidence}");
}

/// D2.4.7: a write to a git-ignored path outside the scope passes (the
/// tree comparison observes what git tracks or would track).
#[test]
fn a_git_ignored_write_outside_the_scope_passes() {
    let dir = tempfile::tempdir().unwrap();
    let yaml = r#"
schema: 2
id: ws
name: WS
version: 1.0.0
defaults: { profile: main }
nodes:
  - { id: start, type: start }
  - { id: s, type: script, script: scripts/w.sh, runner: sh, writes: ["src/**"] }
  - { id: done, type: finish, outcome: success }
edges:
  - { from: start, to: s }
  - { from: s, to: done }
"#;
    seed(
        dir.path(),
        yaml,
        &[("w.sh", "echo x > build/out.bin\necho done\n".into())],
    );
    // `build/` is git-ignored.
    fs::write(
        dir.path().join(".gitignore"),
        ".apb/\ninv/\nstub.sh\nbuild/\n",
    )
    .unwrap();
    assert_eq!(
        run_with_stub(dir.path(), "ws", "/bin/true", None),
        RunStatus::Succeeded
    );
}

/// D2.4.8: the engine guard holds with validation bypassed. A resume does
/// not re-validate the run's own `playbook.yaml`: two runs stop at `gate`
/// before the scoped node ever ran, their definitions then give `s` both
/// `cache: auto` and `writes` (V170 would refuse a fresh start), and the
/// two resumes run `s` one after the other. Without the guard the first
/// stores an entry and the second is served from it; with it neither
/// journals a `node_cache_hit`, and the scope check runs in both.
#[test]
fn a_scoped_node_is_never_served_from_the_cache() {
    let unscoped = r#"
schema: 2
id: ws
name: WS
version: 1.0.0
defaults: { profile: main }
nodes:
  - { id: start, type: start }
  - { id: gate, type: script, script: scripts/gate.sh, runner: sh }
  - { id: s, type: script, script: scripts/w.sh, runner: sh }
  - { id: done, type: finish, outcome: success }
  - { id: stop, type: finish, outcome: failure }
edges:
  - { from: start, to: gate }
  - { from: gate, to: s, condition: { type: node_status, node: gate, equals: success } }
  - { from: gate, to: stop, condition: { type: node_status, node: gate, equals: failure } }
  - { from: s, to: done }
"#;
    let dir = tempfile::tempdir().unwrap();
    seed(
        dir.path(),
        unscoped,
        &[
            ("gate.sh", "test -f inv/go\n".into()),
            ("w.sh", "echo done\n".into()),
        ],
    );
    let _env = common::env_lock();
    unsafe { std::env::set_var("APB_AGENT_CMD", "/bin/true") };
    // Two runs that stop at the gate before `s` ever ran.
    let stopped: Vec<String> = (0..2)
        .map(|_| {
            let res =
                apb_engine::scheduler::run(dir.path(), "ws", None, Default::default()).unwrap();
            assert_eq!(res.outcome, RunStatus::Failed, "the gate stops the run");
            res.run_id
        })
        .collect();
    fs::create_dir_all(dir.path().join("inv")).unwrap();
    fs::write(dir.path().join("inv/go"), "").unwrap();
    let scoped = unscoped.replace(
        "script: scripts/w.sh, runner: sh }",
        "script: scripts/w.sh, runner: sh, cache: auto, writes: [\"src/**\"] }",
    );
    for run_id in &stopped {
        let rd = dir.path().join(".apb/runs").join(run_id);
        fs::write(rd.join("playbook.yaml"), &scoped).unwrap();
        let again = apb_engine::scheduler::resume(dir.path(), run_id, Some("gate")).unwrap();
        assert_eq!(again.outcome, RunStatus::Succeeded, "run {run_id}");
        let all = read_all(&rd).unwrap();
        assert!(
            !all.iter()
                .any(|e| matches!(&e.payload, EventPayload::NodeCacheHit { .. })),
            "run {run_id}: a scoped node is never served from the cache"
        );
        assert!(
            rd.join("writes/s/1.json").exists(),
            "run {run_id}: the scope check ran"
        );
    }
    unsafe { std::env::remove_var("APB_AGENT_CMD") };
}

/// D2.4.9: the agent's prompt holds the write scope note on attempt 1.
#[test]
fn the_prompt_names_the_write_scope() {
    let dir = tempfile::tempdir().unwrap();
    let yaml = r#"
schema: 2
id: ws
name: WS
version: 1.0.0
defaults: { profile: main }
nodes:
  - { id: start, type: start }
  - { id: w, type: agent_task, prompt: "do", writes: ["src/**"], max_changed_files: 5 }
  - { id: done, type: finish, outcome: success }
edges:
  - { from: start, to: w }
  - { from: w, to: done }
"#;
    seed(dir.path(), yaml, &[]);
    let stub = recording_stub(dir.path(), OK);
    assert_eq!(
        run_with_stub(dir.path(), "ws", &stub, None),
        RunStatus::Succeeded
    );
    let prompt = invocations(dir.path())
        .iter()
        .find(|i| i.node == "w")
        .unwrap()
        .prompt()
        .to_string();
    assert!(
        prompt.contains(
            "Write scope: change only files matching `src/**`, and at most 5 files in all."
        ),
        "{prompt}"
    );
    assert!(
        prompt.contains("fails the step without a retry"),
        "{prompt}"
    );
}

/// A scenario run resolves no scope, takes no tree state and checks
/// nothing for a scoped agent, as for a scoped script: no scope evidence is
/// written, a template the scripted answers never feed refuses nothing, and
/// the scripted success stands.
#[test]
fn a_scenario_run_of_a_scoped_agent_checks_nothing() {
    use apb_core::scenario::{Scenario, ScenarioLevel};
    use apb_engine::scenario::session::ScenarioPrep;
    use apb_engine::scheduler::run_scenario;

    let dir = tempfile::tempdir().unwrap();
    let yaml = r#"
schema: 2
id: ws
name: WS
version: 1.0.0
defaults: { profile: main }
nodes:
  - { id: start, type: start }
  - { id: w, type: agent_task, prompt: "do", writes: ["src/**"] }
  - { id: done, type: finish, outcome: success }
edges:
  - { from: start, to: w }
  - { from: w, to: done }
"#;
    seed(dir.path(), yaml, &[]);
    let scenario_yaml = r#"schema: 1
id: ws
instruction: scope
answers:
  w:
    - { visit: 1, attempts: [ { status: succeeded, output: "ok" } ] }
"#;
    let prep = ScenarioPrep {
        scenario: yaml_serde::from_str::<Scenario>(scenario_yaml).unwrap(),
        level: ScenarioLevel::Quick,
        scenario_digest: "sha256:ws".into(),
        max_requests: 20,
        sent: Default::default(),
    };
    let quick = run_scenario(dir.path(), "ws", None, &prep).unwrap();
    assert!(
        !quick.run_dir.join("writes").exists(),
        "a scenario run writes no scope evidence"
    );

    // The template form reading a param left empty renders empty: a real
    // run refuses the node, a scenario run resolves nothing.
    let dir = tempfile::tempdir().unwrap();
    seed(
        dir.path(),
        &yaml
            .replace("defaults: { profile: main }", "defaults: { profile: main }\nparams:\n  - { name: files, type: text, default: \"\" }")
            .replace("writes: [\"src/**\"]", "writes: \"{{params.files}}\""),
        &[],
    );
    let quick = run_scenario(dir.path(), "ws", None, &prep).unwrap();
    assert_eq!(quick.outcome, RunStatus::Succeeded);
}

/// A scoped writer is not a read-only node: it journals no
/// `read_only_checked`, on the agent arm and on the script arm, while its
/// scope check runs.
#[test]
fn a_scoped_writer_is_never_journaled_as_read_only() {
    let agent = r#"
schema: 2
id: ws
name: WS
version: 1.0.0
defaults: { profile: main }
nodes:
  - { id: start, type: start }
  - { id: w, type: agent_task, prompt: "do", writes: ["src/**"] }
  - { id: done, type: finish, outcome: success }
edges:
  - { from: start, to: w }
  - { from: w, to: done }
"#;
    for (yaml, scripts, cmd) in [
        (agent, vec![], None),
        (
            SCRIPT_PB,
            vec![("w.sh", "echo x > src/a.rs\necho done\n".to_string())],
            Some("/bin/true"),
        ),
    ] {
        let dir = tempfile::tempdir().unwrap();
        seed(dir.path(), yaml, &scripts);
        let stub = match cmd {
            Some(c) => c.to_string(),
            None => recording_stub(dir.path(), OK),
        };
        assert_eq!(
            run_with_stub(dir.path(), "ws", &stub, None),
            RunStatus::Succeeded
        );
        let all = read_all(&run_dir(dir.path())).unwrap();
        assert!(
            !all.iter()
                .any(|e| matches!(&e.payload, EventPayload::ReadOnlyChecked { .. })),
            "no read_only_checked for a scoped writer"
        );
        assert!(
            run_dir(dir.path()).join("writes").exists(),
            "the scope check ran"
        );
    }
}
