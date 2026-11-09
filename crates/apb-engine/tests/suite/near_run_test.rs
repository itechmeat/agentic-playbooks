//! A run on a NEAR AI Cloud model end to end, with a stub dsh that prints
//! dsh's `--json` events: the attempt is priced from the run's catalog row
//! and billed, its key never lands in the run directory, its class is
//! journaled on `attempt_started`, a fallback on another class is marked
//! `privacy_class_left`, and a run budget counts the priced cost.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use apb_core::agent_output::CostBasis;
use apb_core::budget::BudgetRequest;
use apb_core::registry::init_project;
use apb_engine::event::{EventPayload, read_all};
use apb_engine::scheduler::{RunOptions, run};
use apb_engine::state::RunStatus;

use crate::common::{self, EnvVar};

const KEY: &str = "fake-key-1234567";

fn playbook(nodes: &str, edges: &str, head: &str) -> String {
    format!(
        "schema: 2\nid: near\nname: Near\nversion: 1.0.0\n{head}defaults:\n  profile: main\nnodes:\n  - {{ id: start, type: start }}\n{nodes}  - {{ id: done, type: finish, outcome: success }}\nedges:\n{edges}"
    )
}

const ONE: (&str, &str) = (
    "  - { id: w, type: agent_task, prompt: \"do\", max_retries: 0 }\n",
    "  - { from: start, to: w }\n  - { from: w, to: done }\n",
);

/// A stub dsh: records its environment's names, prints the key on stderr,
/// fails with a dsh quota error when its patch names `$FAIL_MODEL`, else
/// answers with 1,000,000 input and 100,000 output tokens and a reply that
/// echoes the key.
fn stub(dir: &Path) -> String {
    let ok = dir.join("ok.jsonl");
    fs::write(
        &ok,
        concat!(
            r#"{"type":"session","sessionId":"s1","cwd":"/w"}"#,
            "\n",
            r#"{"type":"status","phase":"step_end","turn":1,"step":1,"usage":{"inputTokens":1000000,"outputTokens":100000,"totalTokens":1100000}}"#,
            "\n",
            r#"{"type":"final","sessionId":"s1","reason":{"kind":"completed"},"text":"echo __KEY__\n\n```yaml\nstatus: success\nsummary: done\n```"}"#,
            "\n"
        ),
    )
    .unwrap();
    let quota = dir.join("quota.jsonl");
    fs::write(
        &quota,
        concat!(
            r#"{"type":"session","sessionId":"s0","cwd":"/w"}"#,
            "\n",
            r#"{"type":"final","sessionId":"s0","reason":{"kind":"error","error":{"code":"RATE_LIMIT","message":"slow down","status":429}},"text":""}"#,
            "\n"
        ),
    )
    .unwrap();
    let names = dir.join("env-names.txt");
    let path = dir.join("dsh.sh");
    common::write_sync(
        &path,
        &format!(
            r#"#!/bin/sh
patch=""
while [ $# -gt 0 ]; do [ "$1" = "--patch" ] && patch="$2"; shift; done
cat > /dev/null
env | cut -d= -f1 > '{names}'
if [ -n "$FAIL_MODEL" ] && grep -q "$FAIL_MODEL" "$patch"; then cat '{quota}'; exit 1; fi
if [ -f '{need_route}' ] && ! grep -q "NEAR AI Cloud" "$patch"; then echo "no provider route nearai" >&2; exit 1; fi
echo "stderr $NEARAI_API_KEY" >&2
if [ -n "$APB_STATUS_FILE" ] && [ -f '{flag}' ]; then
  printf '{{"status":"success","outputs":{{"said":"%s"}},"findings":[{{"kind":"bug","location":"a","evidence":"saw %s","why_not_fixed":"w"}}],"warnings":[{{"subject":"key %s","reason":"r"}}]}}' "$NEARAI_API_KEY" "$NEARAI_API_KEY" "$NEARAI_API_KEY" > "$APB_STATUS_FILE"
elif [ -n "$APB_STATUS_FILE" ]; then printf '{{"status":"success"}}' > "$APB_STATUS_FILE"; fi
sed "s/__KEY__/$NEARAI_API_KEY/" '{ok}'
"#,
            names = names.display(),
            quota = quota.display(),
            ok = ok.display(),
            flag = dir.join("status-echo").display(),
            need_route = dir.join("need-route").display(),
        ),
    );
    let mut perm = fs::metadata(&path).unwrap().permissions();
    perm.set_mode(0o755);
    fs::set_permissions(&path, perm).unwrap();
    path.to_string_lossy().into_owned()
}

struct Project {
    dir: tempfile::TempDir,
    _env: Vec<EnvVar>,
    _lock: std::sync::MutexGuard<'static, ()>,
}

impl Project {
    fn root(&self) -> &Path {
        self.dir.path()
    }
    fn run_dir(&self, id: &str) -> PathBuf {
        self.root().join(".apb/runs").join(id)
    }
}

/// A project with playbook `near` and profile `main` (`profile_yaml`), the
/// stub dsh and the key set (billed work is allowed for the whole test
/// binary). The config dir stays the binary's own: tests that take no env
/// lock read it at the same time.
fn project(yaml: &str, profile_yaml: &str, extra: &[(&'static str, &str)]) -> Project {
    let lock = common::env_lock();
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let mut env = vec![
        EnvVar::set("NEARAI_API_KEY", KEY),
        EnvVar::set("APB_AGENT_CMD", stub(root)),
        EnvVar::remove("FAIL_MODEL"),
    ];
    env.extend(extra.iter().map(|(k, v)| EnvVar::set(k, v)));
    apb_core::run_origin::ensure_key().unwrap();
    init_project(root).unwrap();
    let vdir = root.join(".apb/playbooks/near/1.0.0");
    fs::create_dir_all(&vdir).unwrap();
    fs::write(vdir.join("playbook.yaml"), yaml).unwrap();
    fs::write(root.join(".apb/playbooks/near/current"), "1.0.0").unwrap();
    let pdir = root.join(".apb/profiles/main");
    fs::create_dir_all(&pdir).unwrap();
    fs::write(pdir.join("profile.yaml"), profile_yaml).unwrap();
    fs::write(pdir.join("SOUL.md"), "").unwrap();
    Project {
        dir,
        _env: env,
        _lock: lock,
    }
}

const ALLOWLISTED: &str = "name: main\ndescription: t\nexecutor:\n  agent: dsh\n  model: nearai/Qwen/Qwen3.8-27B\nprocess_env: { policy: allowlist }\n";

/// Every file under `dir` that holds `needle`.
fn holding(dir: &Path, needle: &str) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        for e in fs::read_dir(&d).unwrap().filter_map(Result::ok) {
            let p = e.path();
            if p.is_dir() {
                stack.push(p);
            } else if fs::read(&p)
                .is_ok_and(|b| b.windows(needle.len()).any(|w| w == needle.as_bytes()))
            {
                out.push(p);
            }
        }
    }
    out
}

#[test]
fn a_near_step_is_priced_billed_scrubbed_and_classed() {
    let p = project(&playbook(ONE.0, ONE.1, ""), ALLOWLISTED, &[]);
    let res = run(p.root(), "near", None, RunOptions::default()).unwrap();
    assert_eq!(res.outcome, RunStatus::Succeeded);
    let run_dir = p.run_dir(&res.run_id);
    let events = read_all(&run_dir).unwrap();

    // Priced from the run's catalog row, billed on the key.
    let usage = events
        .iter()
        .find_map(|e| match &e.payload {
            EventPayload::AttemptFinished { usage, .. } => usage.clone(),
            _ => None,
        })
        .expect("usage");
    assert_eq!(usage.cost_usd, Some(0.175));
    assert_eq!(usage.cost_basis, CostBasis::Billed);
    assert_eq!(usage.priced_from.as_ref().unwrap().catalog, "near-ai-cloud");
    let est = apb_engine::run_estimate::snapshot::read(&run_dir);
    let spent = apb_engine::budget::spent::Spent::of(&run_dir, &events, est.as_ref(), true);
    assert!((spent.usd.known - 0.175).abs() < 1e-9, "{spent:?}");
    let view = apb_engine::run_view::RunView::load(&run_dir, &res.run_id).unwrap();
    let total = view.usage().unwrap();
    assert_eq!(
        total.priced_from,
        ["priced from NEAR AI Cloud catalog 2026-11-09"]
    );

    // The class on the attempt.
    let cloud = events
        .iter()
        .find_map(|e| match &e.payload {
            EventPayload::AttemptStarted { cloud, .. } => cloud.clone(),
            _ => None,
        })
        .expect("attempt_started.cloud");
    assert_eq!(cloud.provider, "near-ai-cloud");
    assert_eq!(
        cloud.privacy_class,
        apb_core::cloud_models::PrivacyClass::ConfidentialTee
    );

    // The key reached the allowlisted process, by name in the journal.
    let names = fs::read_to_string(p.root().join("env-names.txt")).unwrap();
    assert!(names.lines().any(|n| n == "NEARAI_API_KEY"), "{names}");
    assert!(
        events
            .iter()
            .any(|e| matches!(&e.payload, EventPayload::AgentEnvApplied { .. }))
    );

    // Nothing under the run directory holds the value; the reply holds the
    // marker.
    assert_eq!(holding(&run_dir, KEY), Vec::<PathBuf>::new());
    assert!(
        view.state.outputs["w"].contains("[redacted:NEARAI_API_KEY]"),
        "{}",
        view.state.outputs["w"]
    );

    // Read-time redaction while the key is set: text an older apb stored
    // with the value reads redacted.
    let redactor = apb_engine::run_redact::RunRedactor::for_run(p.root(), &run_dir);
    assert!(
        !redactor
            .redact_text(&format!("old {KEY} text"))
            .contains(KEY)
    );

    // A rotated key: the stored reply still shows the marker.
    let _rotated = EnvVar::set("NEARAI_API_KEY", "another-key-7654321");
    let again = apb_engine::run_view::RunView::load(&run_dir, &res.run_id).unwrap();
    assert!(again.state.outputs["w"].contains("[redacted:NEARAI_API_KEY]"));
}

#[test]
fn a_status_file_that_holds_the_key_is_scrubbed_before_it_is_read() {
    let p = project(&playbook(ONE.0, ONE.1, ""), ALLOWLISTED, &[]);
    // The stub writes the key into the status file's outputs, findings and
    // warnings.
    fs::write(p.root().join("status-echo"), "").unwrap();
    let res = run(p.root(), "near", None, RunOptions::default()).unwrap();
    assert_eq!(res.outcome, RunStatus::Succeeded);
    let run_dir = p.run_dir(&res.run_id);
    assert_eq!(holding(&run_dir, KEY), Vec::<PathBuf>::new());
    assert_eq!(holding(p.root(), KEY), Vec::<PathBuf>::new());
    let view = apb_engine::run_view::RunView::load(&run_dir, &res.run_id).unwrap();
    assert!(
        view.state.outputs["w"].contains("[redacted:NEARAI_API_KEY]"),
        "{}",
        view.state.outputs["w"]
    );
}

#[test]
fn a_manifest_without_a_catalog_snapshot_still_redacts_the_key_on_read() {
    let p = project(&playbook(ONE.0, ONE.1, ""), ALLOWLISTED, &[]);
    let res = run(p.root(), "near", None, RunOptions::default()).unwrap();
    let run_dir = p.run_dir(&res.run_id);
    // The manifest as an older apb wrote it: no `cloud` snapshot on the
    // executor, whose model still routes to NEAR AI Cloud.
    let path = run_dir.join("manifest.yaml");
    let text = fs::read_to_string(&path).unwrap();
    let mut doc: serde_json::Value = yaml_serde::from_str(&text).unwrap();
    let mut removed = 0;
    for prof in doc["profiles"].as_array_mut().unwrap() {
        for ri in prof["chain"].as_array_mut().unwrap() {
            removed += usize::from(ri.as_object_mut().unwrap().remove("cloud").is_some());
        }
    }
    assert!(removed > 0, "{text}");
    fs::write(&path, yaml_serde::to_string(&doc).unwrap()).unwrap();
    let redactor = apb_engine::run_redact::RunRedactor::for_run(p.root(), &run_dir);
    let out = redactor.redact_text(&format!("old {KEY} text"));
    assert!(!out.contains(KEY), "{out}");
}

/// The wake-up probe of a quota pause runs the executor as its attempts
/// do: on apb's NEAR AI Cloud route, from the run's catalog snapshot.
#[test]
fn the_quota_probe_runs_on_the_cloud_route() {
    let profile =
        "name: main\ndescription: t\nexecutor:\n  agent: dsh\n  model: nearai/Qwen/Qwen3.8-27B\n";
    let cfg = tempfile::tempdir().unwrap();
    let p = project(
        &playbook(ONE.0, ONE.1, ""),
        profile,
        &[
            ("FAIL_MODEL", "Qwen/Qwen3.8-27B"),
            ("APB_INFRA_BACKOFF_MS", "20,20"),
            ("APB_QUOTA_BACKOFF_MS", "600000,1200000"),
        ],
    );
    let _cfg = EnvVar::set("APB_CONFIG_DIR", cfg.path());
    apb_core::run_origin::ensure_key().unwrap();
    let res = run(p.root(), "near", None, RunOptions::default()).unwrap();
    assert_eq!(res.outcome, RunStatus::Paused);
    let run_dir = p.run_dir(&res.run_id);
    let seq = read_all(&run_dir)
        .unwrap()
        .iter()
        .rev()
        .find_map(|e| match &e.payload {
            EventPayload::RunPaused { reason, .. } if reason.starts_with("quota") => Some(e.seq),
            _ => None,
        })
        .expect("a quota pause");
    // The quota is back; the stub now answers only on apb's route.
    let _ok = EnvVar::remove("FAIL_MODEL");
    fs::write(p.root().join("need-route"), "").unwrap();
    apb_engine::pause::probe_before_wake(p.root(), &res.run_id, seq).unwrap();
    let detail = read_all(&run_dir)
        .unwrap()
        .iter()
        .rev()
        .find_map(|e| match &e.payload {
            EventPayload::SupervisorAction { action, detail, .. }
                if action == apb_engine::pause::quota::QUOTA_PROBE_ACTION =>
            {
                Some(detail.clone())
            }
            _ => None,
        })
        .expect("the probe's outcome");
    assert!(detail.contains("answered"), "{detail}");
}

#[test]
fn a_short_key_is_not_replaced() {
    let p = project(
        &playbook(ONE.0, ONE.1, ""),
        ALLOWLISTED,
        &[("NEARAI_API_KEY", "short")],
    );
    let res = run(p.root(), "near", None, RunOptions::default()).unwrap();
    let view = apb_engine::run_view::RunView::load(&p.run_dir(&res.run_id), &res.run_id).unwrap();
    assert!(
        view.state.outputs["w"].contains("echo short"),
        "{}",
        view.state.outputs["w"]
    );
}

#[test]
fn a_fallback_on_another_class_is_marked_privacy_class_left() {
    let profile = "name: main\ndescription: t\nexecutor:\n  agent: dsh\n  model: nearai/Qwen/Qwen3.8-27B\n  fallbacks:\n    - { agent: dsh, model: nearai/anthropic/claude-sonnet-5-5 }\n";
    let p = project(
        &playbook(ONE.0, ONE.1, ""),
        profile,
        &[("FAIL_MODEL", "Qwen/Qwen3.8-27B")],
    );
    let res = run(p.root(), "near", None, RunOptions::default()).unwrap();
    let run_dir = p.run_dir(&res.run_id);
    let events = read_all(&run_dir).unwrap();
    assert_eq!(
        res.outcome,
        RunStatus::Succeeded,
        "{:#?}",
        events
            .iter()
            .map(|e| serde_json::to_string(&e.payload).unwrap())
            .collect::<Vec<_>>()
    );
    let classes: Vec<_> = events
        .iter()
        .filter_map(|e| match &e.payload {
            EventPayload::AttemptStarted { cloud, .. } => cloud.as_ref().map(|c| c.privacy_class),
            _ => None,
        })
        .collect();
    use apb_core::cloud_models::PrivacyClass::*;
    assert_eq!(classes, [ConfidentialTee, Incognito]);
    let views = apb_engine::execution_identity::fold::views(&run_dir, &events);
    assert_eq!(views.len(), 2);
    assert!(!views[0].privacy_class_left);
    assert!(views[1].privacy_class_left);
    let json = serde_json::to_value(&views[1]).unwrap();
    assert_eq!(json["cloud"]["privacy_class"], "incognito");
    assert_eq!(json["privacy_class_left"], true);
}

#[test]
fn a_budget_counts_the_priced_cost_and_pauses_before_the_next_node() {
    let nodes = "  - { id: a, type: agent_task, prompt: \"one\", max_retries: 0 }\n  - { id: b, type: agent_task, prompt: \"two\", max_retries: 0 }\n";
    let edges = "  - { from: start, to: a }\n  - { from: a, to: b }\n  - { from: b, to: done }\n";
    let p = project(&playbook(nodes, edges, ""), ALLOWLISTED, &[]);
    let res = run(
        p.root(),
        "near",
        None,
        RunOptions {
            budget: Some(BudgetRequest {
                usd: Some(0.10),
                ..BudgetRequest::default()
            }),
            ..RunOptions::default()
        },
    )
    .unwrap();
    assert_eq!(res.outcome, RunStatus::Paused);
    let events = read_all(&p.run_dir(&res.run_id)).unwrap();
    assert!(
        events.iter().any(|e| matches!(&e.payload, EventPayload::RunPaused { reason, .. } if reason.starts_with("budget"))),
        "the budget gate paused the run"
    );
    assert!(
        !events
            .iter()
            .any(|e| matches!(&e.payload, EventPayload::NodeStarted { node, .. } if node == "b")),
        "node b never started"
    );
}
