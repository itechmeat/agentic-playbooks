//! Task 6: resolving profiles into a run, manifest, snapshots, env drift, skill delivery.
//! The stub agent (via APB_AGENT_CMD) dumps the received argv into the file APB_TEST_DUMP -
//! this way the test sees what actually went to the agent (prompt with advisory line, SOUL, etc.).

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::sync::MutexGuard;

use apb_core::registry::init_project;
use apb_engine::event::{EventPayload, read_all};
use apb_engine::scheduler::{RunOptions, resume_with, run};
use apb_engine::state::RunStatus;

use crate::common;

fn lock() -> MutexGuard<'static, ()> {
    common::env_lock()
}

struct EnvGuard;
impl Drop for EnvGuard {
    fn drop(&mut self) {
        unsafe {
            std::env::remove_var("APB_AGENT_CMD");
            std::env::remove_var("APB_CONFIG_DIR");
            std::env::remove_var("HOME");
            std::env::remove_var("APB_TEST_DUMP");
        }
    }
}

fn make_stub(dir: &Path, body: &str) -> String {
    let path = dir.join("stub.sh");
    common::write_sync(&path, &format!("#!/bin/sh\n{body}\n"));
    let mut p = fs::metadata(&path).unwrap().permissions();
    p.set_mode(0o755);
    fs::set_permissions(&path, p).unwrap();
    path.to_string_lossy().to_string()
}

fn seed_profile(root: &Path, name: &str, extra_exec: &str, skills: &str, soul: &str) {
    let dir = root.join(".apb/profiles").join(name);
    fs::create_dir_all(&dir).unwrap();
    let yaml = format!(
        "name: {name}\ndescription: d\nexecutor:\n  agent: claude\n  model: haiku\n{extra_exec}{skills}"
    );
    fs::write(dir.join("profile.yaml"), yaml).unwrap();
    fs::write(dir.join("SOUL.md"), soul).unwrap();
}

fn seed_skill(root: &Path, name: &str, body: &str) {
    let dir = root.join(".agents/skills").join(name);
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("SKILL.md"), body).unwrap();
}

fn seed_playbook(root: &Path, node_extra: &str) {
    init_project(root).unwrap();
    let src = format!(
        "schema: 1\nid: p\nname: P\nversion: 1.0.0\nnodes:\n  - {{ id: start, type: start }}\n  - {{ id: t, type: agent_task, prompt: \"do\", profile: arch{node_extra} }}\n  - {{ id: done, type: finish, outcome: success }}\nedges:\n  - {{ from: start, to: t }}\n  - {{ from: t, to: done }}\n"
    );
    let dir = root.join(".apb/playbooks/p/1.0.0");
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("playbook.yaml"), src).unwrap();
    fs::write(root.join(".apb/playbooks/p/current"), "1.0.0").unwrap();
}

fn set_env(stub: &str, home: &Path, cfg: &Path, dump: Option<&Path>) {
    unsafe {
        std::env::set_var("APB_AGENT_CMD", stub);
        std::env::set_var("HOME", home);
        std::env::set_var("APB_CONFIG_DIR", cfg);
        match dump {
            Some(d) => std::env::set_var("APB_TEST_DUMP", d),
            None => std::env::remove_var("APB_TEST_DUMP"),
        }
    }
}

const DUMP_STUB: &str =
    "if [ -n \"$APB_TEST_DUMP\" ]; then printf '%s\\n' \"$@\" > \"$APB_TEST_DUMP\"; fi\necho done";

#[test]
fn run_with_profile_snapshots_and_writes_manifest() {
    let _l = lock();
    let _g = EnvGuard;
    let proj = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    let cfg = tempfile::tempdir().unwrap();
    let bin = tempfile::tempdir().unwrap();
    seed_playbook(proj.path(), "");
    seed_profile(proj.path(), "arch", "", "", "role");
    set_env(
        &make_stub(bin.path(), DUMP_STUB),
        home.path(),
        cfg.path(),
        None,
    );

    let res = run(proj.path(), "p", None, RunOptions::default()).expect("run ok");
    assert_eq!(res.outcome, RunStatus::Succeeded);

    let run_dir = proj.path().join(".apb/runs").join(&res.run_id);
    assert!(
        run_dir.join("profiles/project/arch/profile.yaml").is_file(),
        "profile snapshot missing"
    );
    let manifest = fs::read_to_string(run_dir.join("manifest.yaml")).expect("manifest exists");
    assert!(manifest.contains("bundle_digest"));
    assert!(manifest.contains("arch"));
}

#[test]
fn attempt_journaled_at_spawn_with_pid_and_duration() {
    // Task 2 (spawn-time attempt journaling): a real stub-agent node must
    // journal attempt_started at spawn (carrying the child pid) strictly before
    // attempt_finished (carrying duration_ms measured from the spawn instant),
    // with distinct timestamps. This is the write shape that makes a mid-attempt
    // crash observable: attempt_started is on disk before the agent returns. The
    // stub sleeps briefly so the spawn write and the return write land in
    // distinct milliseconds.
    let _l = lock();
    let _g = EnvGuard;
    let proj = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    let cfg = tempfile::tempdir().unwrap();
    let bin = tempfile::tempdir().unwrap();
    seed_playbook(proj.path(), "");
    seed_profile(proj.path(), "arch", "", "", "role");
    set_env(
        &make_stub(bin.path(), "sleep 0.05\necho done"),
        home.path(),
        cfg.path(),
        None,
    );

    let res = run(proj.path(), "p", None, RunOptions::default()).expect("run ok");
    assert_eq!(res.outcome, RunStatus::Succeeded);
    let run_dir = proj.path().join(".apb/runs").join(&res.run_id);
    let events = read_all(&run_dir).expect("events readable");

    let started = events
        .iter()
        .find(|e| matches!(&e.payload, EventPayload::AttemptStarted { node, .. } if node == "t"))
        .expect("attempt_started for node t");
    let finished = events
        .iter()
        .find(|e| matches!(&e.payload, EventPayload::AttemptFinished { node, .. } if node == "t"))
        .expect("attempt_finished for node t");

    // Ordering: started strictly before finished in the journal.
    assert!(
        started.seq < finished.seq,
        "attempt_started (seq {}) must precede attempt_finished (seq {})",
        started.seq,
        finished.seq
    );
    // Distinct, ordered timestamps: the spawn-time write predates the
    // return-time write. With back-to-back writes these could share a ms.
    assert!(
        started.ts < finished.ts,
        "attempt_started ts {} must be strictly before attempt_finished ts {}",
        started.ts,
        finished.ts
    );
    // pid captured at spawn from child.id().
    let EventPayload::AttemptStarted { pid, .. } = &started.payload else {
        unreachable!("matched AttemptStarted above")
    };
    assert!(pid.is_some(), "attempt_started.pid must be Some at spawn");
    // duration_ms measured from the spawn instant.
    let EventPayload::AttemptFinished { duration_ms, .. } = &finished.payload else {
        unreachable!("matched AttemptFinished above")
    };
    assert!(
        duration_ms.is_some(),
        "attempt_finished.duration_ms must be Some"
    );
}

#[test]
fn autonomous_run_does_not_bind_supervisor_from_defaults() {
    // Review P2: an autonomous run (without --supervise) with defaults.profile and
    // WITHOUT a supervisor section must NOT bind defaults.profile as supervisor -
    // otherwise the run would be forced to resolve/trust a profile that never
    // spawns. Check: the manifest carries the node binding but NOT "supervisor".
    let _l = lock();
    let _g = EnvGuard;
    let proj = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    let cfg = tempfile::tempdir().unwrap();
    let bin = tempfile::tempdir().unwrap();
    init_project(proj.path()).unwrap();
    // defaults.profile: arch, agent_task without its own profile, without a supervisor section.
    let src = "schema: 1\nid: p\nname: P\nversion: 1.0.0\ndefaults:\n  profile: arch\nnodes:\n  - { id: start, type: start }\n  - { id: t, type: agent_task, prompt: \"do\" }\n  - { id: done, type: finish, outcome: success }\nedges:\n  - { from: start, to: t }\n  - { from: t, to: done }\n";
    let dir = proj.path().join(".apb/playbooks/p/1.0.0");
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("playbook.yaml"), src).unwrap();
    fs::write(proj.path().join(".apb/playbooks/p/current"), "1.0.0").unwrap();
    seed_profile(proj.path(), "arch", "", "", "role");
    set_env(
        &make_stub(bin.path(), DUMP_STUB),
        home.path(),
        cfg.path(),
        None,
    );

    let res = run(proj.path(), "p", None, RunOptions::default()).expect("run ok");
    assert_eq!(res.outcome, RunStatus::Succeeded);
    let run_dir = proj.path().join(".apb/runs").join(&res.run_id);
    let manifest = fs::read_to_string(run_dir.join("manifest.yaml")).expect("manifest exists");
    // Node is bound, supervisor is not (autonomous run).
    assert!(manifest.contains("t:"), "node binding expected: {manifest}");
    assert!(
        !manifest.contains("supervisor:"),
        "autonomous run must NOT bind a supervisor from defaults.profile: {manifest}"
    );
}

#[test]
fn stale_expected_bundle_is_rejected() {
    // Anti-TOCTOU (spec 5.1): if the bundle captured by the gate does not match
    // the one recomputed from the snapshot, the engine refuses before start.
    let _l = lock();
    let _g = EnvGuard;
    let proj = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    let cfg = tempfile::tempdir().unwrap();
    let bin = tempfile::tempdir().unwrap();
    seed_playbook(proj.path(), "");
    seed_profile(proj.path(), "arch", "", "", "role");
    set_env(
        &make_stub(bin.path(), DUMP_STUB),
        home.path(),
        cfg.path(),
        None,
    );

    let mut expected = std::collections::BTreeMap::new();
    expected.insert("project/arch".to_string(), "sha256:deadbeef".to_string());
    let opts = RunOptions {
        expected_profile_bundles: Some(expected),
        ..Default::default()
    };
    let err = run(proj.path(), "p", None, opts).unwrap_err();
    assert!(
        err.to_string().contains("bundle mismatch"),
        "expected bundle mismatch, got: {err}"
    );
}

#[test]
fn expected_bundle_key_set_must_match_exactly() {
    // An extra key in expected (a profile the run does not use) -> refusal:
    // the key sets must match exactly (fail closed).
    let _l = lock();
    let _g = EnvGuard;
    let proj = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    let cfg = tempfile::tempdir().unwrap();
    let bin = tempfile::tempdir().unwrap();
    seed_playbook(proj.path(), "");
    seed_profile(proj.path(), "arch", "", "", "role");
    set_env(
        &make_stub(bin.path(), DUMP_STUB),
        home.path(),
        cfg.path(),
        None,
    );

    // First learn the real bundle via a run without expected (success),
    // then add an extra key.
    let ok = run(proj.path(), "p", None, RunOptions::default()).expect("baseline run ok");
    let mani = fs::read_to_string(
        proj.path()
            .join(".apb/runs")
            .join(&ok.run_id)
            .join("manifest.yaml"),
    )
    .unwrap();
    let bundle_line = mani
        .lines()
        .find(|l| l.trim_start().starts_with("bundle_digest:"))
        .unwrap();
    let real_bundle = bundle_line
        .split(':')
        .skip(1)
        .collect::<Vec<_>>()
        .join(":")
        .trim()
        .to_string();

    let mut expected = std::collections::BTreeMap::new();
    expected.insert("project/arch".to_string(), real_bundle);
    expected.insert("project/ghost".to_string(), "sha256:zzz".to_string()); // extra
    let opts = RunOptions {
        expected_profile_bundles: Some(expected),
        ..Default::default()
    };
    let err = run(proj.path(), "p", None, opts).unwrap_err();
    assert!(
        err.to_string().contains("key set mismatch"),
        "expected key set mismatch, got: {err}"
    );
}

#[test]
fn manifest_is_write_once() {
    let dir = tempfile::tempdir().unwrap();
    let m = apb_engine::manifest::RunExecutionManifest::default();
    apb_engine::manifest::write(dir.path(), &m).unwrap();
    assert!(
        apb_engine::manifest::write(dir.path(), &m).is_err(),
        "second write must fail"
    );
}

#[test]
fn advisory_skills_line_appended_in_shared_workdir() {
    let _l = lock();
    let _g = EnvGuard;
    let proj = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    let cfg = tempfile::tempdir().unwrap();
    let bin = tempfile::tempdir().unwrap();
    let dump = bin.path().join("dump.txt");
    seed_playbook(proj.path(), "");
    seed_profile(proj.path(), "arch", "", "skills:\n  - cs\n", "role");
    seed_skill(proj.path(), "cs", "SKILLBODY-secret-content");
    set_env(
        &make_stub(bin.path(), DUMP_STUB),
        home.path(),
        cfg.path(),
        Some(&dump),
    );

    let res = run(proj.path(), "p", None, RunOptions::default()).expect("run ok");
    assert_eq!(res.outcome, RunStatus::Succeeded);

    let got = fs::read_to_string(&dump).expect("stub dumped args");
    assert!(
        got.contains("Relevant skills: cs"),
        "advisory line missing: {got}"
    );
    // The skill content must NOT be embedded into the prompt.
    assert!(
        !got.contains("SKILLBODY-secret-content"),
        "skill body leaked into prompt"
    );
}

#[test]
fn isolated_workdir_materializes_skill_copies() {
    // An isolated node (isolation: full) gets REAL copies of skills from the
    // run snapshot into a per-node workdir (.agents/skills/<name>), not a symlink
    // and not content in the prompt. The attempt is marked skills_mode: materialized.
    let _l = lock();
    let _g = EnvGuard;
    let proj = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    let cfg = tempfile::tempdir().unwrap();
    let bin = tempfile::tempdir().unwrap();
    let dump = bin.path().join("dump.txt");
    seed_playbook(proj.path(), ", isolation: full");
    seed_profile(proj.path(), "arch", "", "skills:\n  - cs\n", "role");
    seed_skill(proj.path(), "cs", "SKILLBODY-secret");
    set_env(
        &make_stub(bin.path(), DUMP_STUB),
        home.path(),
        cfg.path(),
        Some(&dump),
    );

    let res = run(proj.path(), "p", None, RunOptions::default()).expect("run ok");
    assert_eq!(res.outcome, RunStatus::Succeeded);
    let run_dir = proj.path().join(".apb/runs").join(&res.run_id);

    // Real skill copy in the node's per-attempt workdir (work/<node>/<attempt>).
    let sk = run_dir.join("work/t/1/.agents/skills/cs/SKILL.md");
    assert!(sk.is_file(), "materialized skill copy missing: {sk:?}");
    let meta = fs::symlink_metadata(&sk).unwrap();
    assert!(
        !meta.file_type().is_symlink(),
        "materialized skill must be a real file, not a symlink"
    );
    assert_eq!(fs::read_to_string(&sk).unwrap(), "SKILLBODY-secret");
    // The .claude/skills bridge is set up.
    assert!(run_dir.join("work/t/1/.claude/skills/cs").exists());

    // The attempt event carries skills_mode: materialized.
    let log = fs::read_to_string(run_dir.join("events.jsonl")).unwrap();
    assert!(
        log.contains("\"skills_mode\":\"materialized\""),
        "attempt event missing materialized skills_mode: {log}"
    );
    // The skill content is not embedded into the prompt (advisory names only).
    let got = fs::read_to_string(&dump).expect("stub dumped args");
    assert!(
        !got.contains("SKILLBODY-secret"),
        "skill body leaked into prompt"
    );
}

#[test]
fn isolated_retry_gets_fresh_skill_snapshot_per_attempt() {
    // Review P1: the first (failed) attempt modifies its own skill directory, but
    // the next attempt (fallback) must get a CLEAN skill snapshot -
    // per-attempt materialization in work/<node>/<attempt>.
    let _l = lock();
    let _g = EnvGuard;
    let proj = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    let cfg = tempfile::tempdir().unwrap();
    let bin = tempfile::tempdir().unwrap();
    let dump = bin.path().join("dump.txt");
    seed_playbook(proj.path(), ", isolation: full");
    // Profile with a skill and a fallback: the 1st attempt (primary) will fail, the
    // 2nd (fallback) must see the original skill.
    seed_profile(
        proj.path(),
        "arch",
        "  fallbacks:\n    - { agent: codex, model: haiku }\n",
        "skills:\n  - cs\n",
        "role",
    );
    seed_skill(proj.path(), "cs", "PRISTINE");
    // Stub: on the 1st invocation appends to its own skill directory and fails; afterwards - ok.
    let body = "c=\"$APB_TEST_DUMP.count\"; n=$(cat \"$c\" 2>/dev/null || echo 0); n=$((n+1)); echo \"$n\" > \"$c\"\nif [ \"$n\" = \"1\" ]; then echo MUTATED >> .agents/skills/cs/SKILL.md; echo boom 1>&2; exit 1; fi\necho done";
    set_env(
        &make_stub(bin.path(), body),
        home.path(),
        cfg.path(),
        Some(&dump),
    );

    let res = run(proj.path(), "p", None, RunOptions::default()).expect("run returns");
    let run_dir = proj.path().join(".apb/runs").join(&res.run_id);

    // Attempt 1 ran in work/t/1 and modified its own copy.
    let a1 = run_dir.join("work/t/1/.agents/skills/cs/SKILL.md");
    assert!(a1.is_file(), "attempt-1 skill copy missing");
    assert!(
        fs::read_to_string(&a1).unwrap().contains("MUTATED"),
        "attempt 1 was expected to mutate its own copy"
    );
    // Attempt 2 (fallback) got a CLEAN snapshot - without MUTATED.
    let a2 = run_dir.join("work/t/2/.agents/skills/cs/SKILL.md");
    assert!(a2.is_file(), "attempt-2 fresh skill copy missing");
    assert_eq!(
        fs::read_to_string(&a2).unwrap(),
        "PRISTINE",
        "attempt 2 must see a pristine per-attempt skill snapshot"
    );
}

#[test]
fn non_isolated_node_uses_advisory_skills_mode() {
    // isolation: none -> advisory line with names, no materialized copies,
    // skills_mode: advisory.
    let _l = lock();
    let _g = EnvGuard;
    let proj = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    let cfg = tempfile::tempdir().unwrap();
    let bin = tempfile::tempdir().unwrap();
    let dump = bin.path().join("dump.txt");
    seed_playbook(proj.path(), "");
    seed_profile(proj.path(), "arch", "", "skills:\n  - cs\n", "role");
    seed_skill(proj.path(), "cs", "SKILLBODY-secret");
    set_env(
        &make_stub(bin.path(), DUMP_STUB),
        home.path(),
        cfg.path(),
        Some(&dump),
    );

    let res = run(proj.path(), "p", None, RunOptions::default()).expect("run ok");
    assert_eq!(res.outcome, RunStatus::Succeeded);
    let run_dir = proj.path().join(".apb/runs").join(&res.run_id);

    let got = fs::read_to_string(&dump).expect("stub dumped args");
    assert!(
        got.contains("Relevant skills: cs"),
        "advisory line missing: {got}"
    );
    assert!(
        !got.contains("SKILLBODY-secret"),
        "skill body leaked into prompt"
    );
    // There is no materialized isolated workdir.
    assert!(!run_dir.join("work/t").exists());
    let log = fs::read_to_string(run_dir.join("events.jsonl")).unwrap();
    assert!(
        log.contains("\"skills_mode\":\"advisory\""),
        "attempt event missing advisory skills_mode: {log}"
    );
}

#[test]
fn fallback_event_carries_profile_ref() {
    let _l = lock();
    let _g = EnvGuard;
    let proj = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    let cfg = tempfile::tempdir().unwrap();
    let bin = tempfile::tempdir().unwrap();
    seed_playbook(proj.path(), "");
    // Profile with a fallback; the stub always fails -> after primary a FallbackTriggered is emitted.
    seed_profile(
        proj.path(),
        "arch",
        "  fallbacks:\n    - { agent: codex, model: haiku }\n",
        "",
        "role",
    );
    set_env(
        &make_stub(bin.path(), "echo boom 1>&2\nexit 1"),
        home.path(),
        cfg.path(),
        None,
    );

    // The run outcome does not matter (an unconditional edge leads to finish); what matters is that
    // on primary failure a FallbackTriggered with a profile reference is emitted.
    let res = run(proj.path(), "p", None, RunOptions::default()).expect("run returns");
    let run_dir = proj.path().join(".apb/runs").join(&res.run_id);
    let log = fs::read_to_string(run_dir.join("events.jsonl")).unwrap();
    assert!(
        log.contains("fallback_triggered"),
        "no fallback event: {log}"
    );
    assert!(
        log.contains("project/arch"),
        "fallback event missing profile ref: {log}"
    );
}

#[test]
fn fallback_skips_identical_agent_and_model_binding() {
    // Task 6: a chain claude(haiku) -> claude(haiku) - the fallback resolves
    // to the exact same (agent, model) pair as the primary that just failed
    // (e.g. a token-permission failure a model swap cannot help). The guard
    // must skip the fallback step entirely: no attempt, no
    // fallback_triggered, and the node fails once the one real step is
    // exhausted.
    let _l = lock();
    let _g = EnvGuard;
    let proj = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    let cfg = tempfile::tempdir().unwrap();
    let bin = tempfile::tempdir().unwrap();
    seed_playbook(proj.path(), "");
    seed_profile(
        proj.path(),
        "arch",
        "  fallbacks:\n    - { agent: claude, model: haiku }\n",
        "",
        "role",
    );
    set_env(
        &make_stub(bin.path(), "echo boom 1>&2\nexit 1"),
        home.path(),
        cfg.path(),
        None,
    );

    let res = run(proj.path(), "p", None, RunOptions::default()).expect("run returns");
    let run_dir = proj.path().join(".apb/runs").join(&res.run_id);
    let events = read_all(&run_dir).unwrap();

    assert!(
        !events
            .iter()
            .any(|e| matches!(&e.payload, EventPayload::FallbackTriggered { .. })),
        "identical fallback binding must not emit fallback_triggered"
    );
    let attempt_count = events
        .iter()
        .filter(|e| matches!(&e.payload, EventPayload::AttemptStarted { node, .. } if node == "t"))
        .count();
    assert_eq!(
        attempt_count, 1,
        "only the first (real) step should be attempted; the identical fallback must be skipped silently"
    );
    assert!(
        events.iter().any(|e| matches!(
            &e.payload,
            EventPayload::NodeFinished { node, status, .. } if node == "t" && status == "failed"
        )),
        "node must fail once its only real step is exhausted"
    );
}

#[test]
fn fallback_triggers_when_model_differs() {
    // Companion to the identical-binding test above: a chain claude(haiku) ->
    // claude(sonnet) differs on model, so the fallback is a genuinely
    // different attempt and must still run normally (one fallback_triggered,
    // an attempt for each step).
    let _l = lock();
    let _g = EnvGuard;
    let proj = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    let cfg = tempfile::tempdir().unwrap();
    let bin = tempfile::tempdir().unwrap();
    seed_playbook(proj.path(), "");
    seed_profile(
        proj.path(),
        "arch",
        "  fallbacks:\n    - { agent: claude, model: sonnet }\n",
        "",
        "role",
    );
    set_env(
        &make_stub(bin.path(), "echo boom 1>&2\nexit 1"),
        home.path(),
        cfg.path(),
        None,
    );

    let res = run(proj.path(), "p", None, RunOptions::default()).expect("run returns");
    let run_dir = proj.path().join(".apb/runs").join(&res.run_id);
    let events = read_all(&run_dir).unwrap();

    let fallback_count = events
        .iter()
        .filter(|e| matches!(&e.payload, EventPayload::FallbackTriggered { .. }))
        .count();
    assert_eq!(
        fallback_count, 1,
        "a genuinely different (agent, model) fallback must still trigger"
    );
    let attempt_count = events
        .iter()
        .filter(|e| matches!(&e.payload, EventPayload::AttemptStarted { node, .. } if node == "t"))
        .count();
    assert_eq!(
        attempt_count, 2,
        "both steps of a non-identical chain must be attempted"
    );
    assert!(
        events.iter().any(|e| matches!(
            &e.payload,
            EventPayload::NodeFinished { node, status, .. } if node == "t" && status == "failed"
        )),
        "node must fail once the whole chain is exhausted"
    );
}

#[test]
fn fallback_guard_compares_consecutive_steps_only() {
    // The guard compares a step against the step that JUST failed, not
    // against every previously tried step: a chain X -> Y -> X (haiku ->
    // sonnet -> haiku) must still attempt the third step, because the step
    // immediately before it (sonnet) differs, even though the first step
    // shares its binding.
    let _l = lock();
    let _g = EnvGuard;
    let proj = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    let cfg = tempfile::tempdir().unwrap();
    let bin = tempfile::tempdir().unwrap();
    seed_playbook(proj.path(), "");
    seed_profile(
        proj.path(),
        "arch",
        "  fallbacks:\n    - { agent: claude, model: sonnet }\n    - { agent: claude, model: haiku }\n",
        "",
        "role",
    );
    set_env(
        &make_stub(bin.path(), "echo boom 1>&2\nexit 1"),
        home.path(),
        cfg.path(),
        None,
    );

    let res = run(proj.path(), "p", None, RunOptions::default()).expect("run returns");
    let run_dir = proj.path().join(".apb/runs").join(&res.run_id);
    let events = read_all(&run_dir).unwrap();

    let fallback_count = events
        .iter()
        .filter(|e| matches!(&e.payload, EventPayload::FallbackTriggered { .. }))
        .count();
    assert_eq!(
        fallback_count, 2,
        "X -> Y -> X must trigger both fallbacks: the guard only skips a step identical to the one that just failed"
    );
    let attempt_count = events
        .iter()
        .filter(|e| matches!(&e.payload, EventPayload::AttemptStarted { node, .. } if node == "t"))
        .count();
    assert_eq!(
        attempt_count, 3,
        "all three steps of X -> Y -> X must be attempted"
    );
}

#[test]
fn env_drift_stops_resume_unless_allowed() {
    let _l = lock();
    let _g = EnvGuard;
    let proj = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    let cfg = tempfile::tempdir().unwrap();
    let bin = tempfile::tempdir().unwrap();
    seed_playbook(proj.path(), "");
    seed_profile(proj.path(), "arch", "", "", "role");
    let stub = make_stub(bin.path(), DUMP_STUB);
    set_env(&stub, home.path(), cfg.path(), None);

    let res = run(proj.path(), "p", None, RunOptions::default()).expect("run ok");
    assert_eq!(res.outcome, RunStatus::Succeeded);

    // Change the stub binary (different size -> different fingerprint) - simulates env drift.
    fs::write(
        &stub,
        "#!/bin/sh\n# changed binary, different size now\necho done\n",
    )
    .unwrap();
    let mut p = fs::metadata(&stub).unwrap().permissions();
    p.set_mode(0o755);
    fs::set_permissions(&stub, p).unwrap();

    let err = resume_with(proj.path(), &res.run_id, Some("t"), false).unwrap_err();
    assert!(
        err.to_string().contains("environment drift"),
        "expected drift error, got: {err}"
    );

    // With permission - it proceeds.
    let ok = resume_with(proj.path(), &res.run_id, Some("t"), true);
    assert!(ok.is_ok(), "resume with allow should succeed: {ok:?}");
}

#[test]
fn legacy_run_resume_via_ephemeral_snapshot() {
    // A run that started BEFORE profiles carries snapshot-executors and has no
    // manifest. Resume must build an ephemeral manifest from the snapshot and
    // carry the run through, rather than fail. Retrofit a valid run into legacy shape:
    // rewrite the snapshot playbook.yaml to schema-1 executors and remove
    // manifest.yaml, then resume from the agent node.
    let _l = lock();
    let _g = EnvGuard;
    let proj = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    let cfg = tempfile::tempdir().unwrap();
    let bin = tempfile::tempdir().unwrap();
    seed_playbook(proj.path(), "");
    seed_profile(proj.path(), "arch", "", "", "role");
    set_env(
        &make_stub(bin.path(), DUMP_STUB),
        home.path(),
        cfg.path(),
        None,
    );

    let res = run(proj.path(), "p", None, RunOptions::default()).expect("run ok");
    assert_eq!(res.outcome, RunStatus::Succeeded);
    let run_dir = proj.path().join(".apb/runs").join(&res.run_id);

    // Retrofit: snapshot -> schema-1 executors, without a manifest.
    let legacy = "schema: 1\nid: p\nname: P\nversion: 1.0.0\nexecutors:\n  main: { agent: claude, model: haiku }\ndefaults:\n  executor: main\nnodes:\n  - { id: start, type: start }\n  - { id: t, type: agent_task, prompt: \"do\", executor: main }\n  - { id: done, type: finish, outcome: success }\nedges:\n  - { from: start, to: t }\n  - { from: t, to: done }\n";
    fs::write(run_dir.join("playbook.yaml"), legacy).unwrap();
    fs::remove_file(run_dir.join("manifest.yaml")).unwrap();

    // Resume from the agent node: the shim builds an ephemeral manifest and carries the run through.
    let ok = resume_with(proj.path(), &res.run_id, Some("t"), false).expect("legacy resume ok");
    assert_eq!(ok.outcome, RunStatus::Succeeded);

    let manifest =
        fs::read_to_string(run_dir.join("manifest.yaml")).expect("ephemeral manifest written");
    assert!(
        manifest.contains("legacy-main"),
        "ephemeral manifest missing legacy profile: {manifest}"
    );
    assert!(
        manifest.contains("t: legacy/legacy-main") || manifest.contains("legacy/legacy-main"),
        "node binding missing: {manifest}"
    );

    // SECOND resume of the same legacy run (review P1): the choice of legacy parsing is based on
    // the snapshot content, NOT on manifest presence. The first resume already created an
    // ephemeral manifest; the second must not fail on strict Playbook::from_yaml
    // (LegacyExecutors), but simply re-read the existing manifest.
    let ok2 = resume_with(proj.path(), &res.run_id, Some("t"), false)
        .expect("second legacy resume must not fail");
    assert_eq!(ok2.outcome, RunStatus::Succeeded);
}

#[test]
fn live_profile_edit_after_start_does_not_affect_resume() {
    let _l = lock();
    let _g = EnvGuard;
    let proj = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    let cfg = tempfile::tempdir().unwrap();
    let bin = tempfile::tempdir().unwrap();
    let dump = bin.path().join("dump.txt");
    seed_playbook(proj.path(), "");
    seed_profile(proj.path(), "arch", "", "", "OLD-SOUL-MARKER");
    set_env(
        &make_stub(bin.path(), DUMP_STUB),
        home.path(),
        cfg.path(),
        Some(&dump),
    );

    let res = run(proj.path(), "p", None, RunOptions::default()).expect("run ok");
    assert_eq!(res.outcome, RunStatus::Succeeded);

    // Edit the live profile AFTER start.
    fs::write(
        proj.path().join(".apb/profiles/arch/SOUL.md"),
        "NEW-SOUL-MARKER",
    )
    .unwrap();

    // Resume from the agent node: must use the SOUL from the manifest (OLD), not from disk.
    resume_with(proj.path(), &res.run_id, Some("t"), false).expect("resume ok");
    let got = fs::read_to_string(&dump).expect("stub dumped args on resume");
    assert!(
        got.contains("OLD-SOUL-MARKER"),
        "resume did not use snapshot SOUL: {got}"
    );
    assert!(
        !got.contains("NEW-SOUL-MARKER"),
        "resume picked up live-edited SOUL: {got}"
    );
}
