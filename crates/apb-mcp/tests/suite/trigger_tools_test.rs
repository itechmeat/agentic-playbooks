//! The fire and the surfaces through the tools layer (design D1 "Tests"
//! item 5): the dry test writing nothing, the twelve checks in order, a
//! person's approval compared field by field, admission catching a changed
//! binding, preparation catching a changed bundle, and the refusals the
//! fire records as an event's verdict. Real plays go through
//! `tools::trigger::fire`, the same function the sweep calls; starts that
//! reach the engine spawn a detached driver this binary re-execs for, so
//! every started run gets a [`DriverReaper`].

use std::fs;
use std::path::{Path, PathBuf};

use apb_core::clock::Millis;
use apb_core::connector::inbox::Inbox;
use apb_core::registry::init_project;
use apb_core::scope::Scope;
use apb_core::signed_store::read as read_signed;
use apb_core::start_key::StartKey;
use apb_core::trigger::{Approval, RuleEntry, RulesFile, evaluate_event, set_rule, store_approval};
use apb_engine::trigger::{FireOutcome, FireRequest, RuleSweep};
use serde_json::{Value, json};

use crate::common;
use crate::driver_reaper::DriverReaper;

/// The connector manifest of the fixture, with a webhook block naming the
/// event type (the shape the rule checks parse).
const GITHUB_YAML: &str = r#"
name: github
version: 1.0.0
webhook:
  signature:
    scheme: hmac_sha256_hex
    header: X-Signature
    secret: "{{secret.app_secret}}"
  event_type_header: X-GitHub-Event
account_fields:
  - name: app_secret
    required: true
    secret: true
functions:
  - name: inbox_read
    description: Read pending events
    read_only: true
    response_pick: [events, cursor]
    inbox:
      op: read
  - name: create_issue
    description: Create an issue
    method: POST
    url: https://example.com/repos/o/r/issues
"#;

fn now() -> u64 {
    apb_core::clock::now_ms_u64()
}

/// The env a rig isolates for its lifetime: its own config dir, no run
/// step, and project registration on even when the outer CI sets it.
/// Restored on drop.
struct Vars {
    prev: Vec<(&'static str, Option<std::ffi::OsString>)>,
}

impl Vars {
    fn isolate(cfg: &Path) -> Self {
        let names: &[&'static str] = &[
            "APB_CONFIG_DIR",
            "APB_RUN_ID",
            "APB_HOST_TASK",
            "CI",
            "APB_NO_REGISTRY",
        ];
        let prev = names.iter().map(|n| (*n, std::env::var_os(n))).collect();
        // SAFETY: the caller holds the env lock, so no other test reads or
        // writes the process env while this runs.
        unsafe {
            std::env::set_var("APB_CONFIG_DIR", cfg);
            for name in ["APB_RUN_ID", "APB_HOST_TASK", "CI", "APB_NO_REGISTRY"] {
                std::env::remove_var(name);
            }
        }
        Vars { prev }
    }
}

impl Drop for Vars {
    fn drop(&mut self) {
        // SAFETY: the env lock is still held (these drop before the rig).
        unsafe {
            for (name, value) in &self.prev {
                match value {
                    Some(v) => std::env::set_var(name, v),
                    None => std::env::remove_var(name),
                }
            }
        }
    }
}

/// A project at its own root, its own config dir (with the github connector
/// installed and an account configured), the project registered. Holds the
/// env lock while it lives.
struct Rig {
    cfg: tempfile::TempDir,
    dir: tempfile::TempDir,
    _vars: Vars,
    _lock: std::sync::MutexGuard<'static, ()>,
}

impl Rig {
    fn new(id: &str, yaml: &str) -> Self {
        let lock = common::env_lock();
        let cfg = tempfile::tempdir().unwrap();
        let dir = tempfile::tempdir().unwrap();
        let vars = Vars::isolate(cfg.path());
        apb_core::run_origin::ensure_key().unwrap();
        install_connector(cfg.path());
        init_project(dir.path()).unwrap();
        write_playbook(dir.path(), id, yaml);
        seed_main(dir.path());
        seed_profile(dir.path(), "dsh-main", "dsh", "haiku");
        apb_core::projects::bind_id(dir.path()).unwrap();
        trust_current(dir.path(), id);
        Rig {
            cfg,
            dir,
            _vars: vars,
            _lock: lock,
        }
    }

    fn root(&self) -> PathBuf {
        self.dir.path().to_path_buf()
    }

    fn cfg_dir(&self) -> PathBuf {
        self.cfg.path().to_path_buf()
    }

    /// Writes the machine's `policy.yaml` over whatever is there.
    fn policy(&self, text: &str) {
        if text.is_empty() {
            fs::remove_file(self.cfg_dir().join("policy.yaml")).ok();
        } else {
            fs::write(self.cfg_dir().join("policy.yaml"), text).unwrap();
        }
    }
}

fn install_connector(cfg: &Path) {
    let dir = cfg.join("connectors").join("github");
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("connector.yaml"), GITHUB_YAML).unwrap();
    fs::create_dir_all(cfg.join("connector-config")).unwrap();
    fs::write(
        cfg.join("connector-config").join("github.yaml"),
        "accounts:\n  - name: main\n  - name: other\n",
    )
    .unwrap();
}

/// A profile under a never-played executor: preparation snapshots it, and
/// no node of the fixture runs it (the driver is not a real apb). Its
/// bundle is approved, as a profile a person wrote through apb's own tools
/// is (the fire never passes `acknowledge_untrusted`: a rule is no trust
/// approval, for playbooks and profile bundles alike).
fn seed_main(root: &Path) {
    seed_profile(root, "main", "claude-code", "haiku");
}

/// `seed_main` for a named profile: the fixture's second executor, `dsh`
/// with an undeclared model, whose cost basis cannot be read (unknown).
fn seed_profile(root: &Path, name: &str, agent: &str, model: &str) {
    let dir = root.join(".apb/profiles").join(name);
    fs::create_dir_all(&dir).unwrap();
    fs::write(
        dir.join("profile.yaml"),
        format!("name: {name}\ndescription: test\nexecutor:\n  agent: {agent}\n  model: {model}\n"),
    )
    .unwrap();
    fs::write(dir.join("SOUL.md"), "a role\n").unwrap();
    let r = apb_core::profile::QualifiedProfileRef {
        name: name.to_string(),
        scope: apb_core::profile::ProfileScope::Auto,
    };
    let (_loaded, _pairs, bundle) = apb_core::profile_store::compute_bundle(
        root,
        apb_core::profile_store::PlaybookOrigin::Project,
        &r,
    )
    .unwrap();
    let mut store = apb_core::trust::TrustStore::load();
    store
        .approve(
            &bundle,
            &format!("project/{name}"),
            apb_core::trust::OriginKind::LocallyApproved,
        )
        .unwrap();
}

fn write_playbook(root: &Path, id: &str, yaml: &str) {
    let vdir = root.join(".apb/playbooks").join(id).join("1.0.0");
    fs::create_dir_all(vdir.join("scripts")).unwrap();
    fs::write(vdir.join("playbook.yaml"), yaml).unwrap();
    fs::write(
        root.join(".apb/playbooks").join(id).join("current"),
        "1.0.0",
    )
    .unwrap();
}

/// Approves the playbook's current trust digest, as a person's approval of
/// a playbook they saved does. The fire never passes
/// `acknowledge_untrusted`: a rule is no trust approval.
fn trust_current(root: &Path, id: &str) {
    let reg = apb_core::registry::Registry::open_dir(&root.join(".apb")).unwrap();
    let loaded = reg.load(id, Some("1.0.0")).unwrap();
    let digest = loaded.trust_digest().unwrap();
    let mut store = apb_core::trust::TrustStore::load();
    store
        .approve(&digest, id, apb_core::trust::OriginKind::LocallyApproved)
        .unwrap();
}

/// `start -> done`, nothing that needs an agent; `extra` joins the header
/// (a param list, a concurrency block, an unattended policy).
fn simple_pb(id: &str, extra: &str) -> String {
    format!(
        "schema: 2\nid: {id}\nname: {id}\nversion: 1.0.0\n{extra}nodes:\n  - {{ id: start, type: start }}\n  - {{ id: done, type: finish, outcome: success }}\nedges:\n  - {{ from: start, to: done }}\n"
    )
}

/// A playbook whose one node binds the `main` profile, so preparation
/// snapshots its bundle (the anti-TOCTOU hand-over has something to
/// compare) and a change of it changes the permit; `extra` joins the
/// header (a concurrency block).
fn agent_pb(id: &str) -> String {
    agent_pb_with(id, "")
}

fn agent_pb_with(id: &str, extra: &str) -> String {
    format!(
        "schema: 2\nid: {id}\nname: {id}\nversion: 1.0.0\n{extra}nodes:\n  - {{ id: start, type: start }}\n  - {{ id: a, type: agent_task, prompt: x, profile: main }}\n  - {{ id: done, type: finish, outcome: success }}\nedges:\n  - {{ from: start, to: a }}\n  - {{ from: a, to: done }}\n"
    )
}

/// [`agent_pb`] over the `dsh` executor, whose cost basis cannot be read
/// (unknown): the billing decision a rule cannot confirm is exercised on
/// it, where the claude fixture's subscription basis is always allowed.
fn agent_pb_dsh(id: &str) -> String {
    agent_pb(id).replace("profile: main", "profile: dsh-main")
}

/// A rule document as a person writes it; `params_body` is the rule's
/// `params:` block or empty.
fn rule_doc(
    name: &str,
    id: &str,
    key: &str,
    max_per_hour: u32,
    external_data: bool,
    params_body: &str,
) -> String {
    format!(
        "name: {name}\nplaybook: {{ id: {id}, version: 1.0.0, scope: project }}\nsource:\n  inbox:\n    connector: github\n    account: main\n    types: [issues]\n    key: \"{key}\"\n{params_body}limits: {{ max_per_hour: {max_per_hour} }}\nbudget: {{ tokens: 100000, on_no_estimate: continue }}\nexternal_data: {external_data}\nrequire_ready: false\n"
    )
}

fn save_rule(root: &Path, doc: &str) {
    set_rule(root, doc, "test", Millis(now())).unwrap();
}

fn entry_of(root: &Path, name: &str) -> RuleEntry {
    let file = read_signed::<RulesFile>(root, Scope::Project, "rules")
        .unwrap()
        .file;
    file.rules
        .iter()
        .find(|e| e.rule.name == name)
        .cloned()
        .unwrap()
}

/// Approves the rule for the binding the tools layer computes right now,
/// through the real `approval_need` and `approve` a person's command runs.
fn approve_via_tools(root: &Path, name: &str) -> String {
    let entry = entry_of(root, name);
    let need = apb_mcp::tools::trigger::approval_need(root, &entry).expect("the need");
    apb_mcp::tools::trigger::approve(root, name, &need, "terminal").expect("approved");
    need.binding
}

/// A hand-built approval of another digest (what a stale store holds).
fn approve_for_digest(root: &Path, name: &str, digest: &str) {
    let approval = Approval {
        rule_digest: digest.to_string(),
        playbook_digest: "digest".to_string(),
        binding: "binding".to_string(),
        permit_digest: "permit".to_string(),
        launch_policy: "allow".to_string(),
        consent_nonce: None,
        approved_at: now(),
        approved_by: "test".to_string(),
    };
    store_approval(root, name, approval, Millis(now())).unwrap();
}

/// Appends one delivery of type `issues` to the account's inbox.
fn deliver(account: &str, provider_id: &str, body: Value) {
    Inbox::open("github", account)
        .unwrap()
        .append_typed(provider_id, Some("issues"), &body)
        .unwrap();
}

fn events(account: &str) -> Vec<apb_core::connector::inbox::InboxEvent> {
    Inbox::open("github", account)
        .unwrap()
        .read_events()
        .unwrap()
}

fn source_of(entry: &RuleEntry) -> apb_core::trigger::InboxSource {
    match &entry.rule.source {
        apb_core::trigger::RuleSource::Inbox(inbox) => inbox.clone(),
    }
}

/// The real fire for the newest event of the rule's source: the evaluation
/// the sweep runs, then `tools::trigger::fire`. Panics on a skipped event.
fn fire_newest(root: &Path, name: &str) -> FireOutcome {
    let entry = entry_of(root, name);
    let source = source_of(&entry);
    let event = events(&source.account).last().cloned().unwrap();
    let own_run = |run: &str| apb_engine::trigger::own_run(root, run);
    let (verdict, _) = evaluate_event(&entry.rule, &source, &event, &entry.incarnation, &own_run);
    let start_key = match verdict {
        apb_core::trigger::EventVerdict::Matched { start_key } => start_key,
        apb_core::trigger::EventVerdict::Skipped { code, name } => {
            panic!("the fixture event must match, got skipped {code} {name:?}")
        }
    };
    fire_event(root, &entry, &source, &event, &start_key)
}

/// The real fire for one evaluated event.
fn fire_event(
    root: &Path,
    entry: &RuleEntry,
    source: &apb_core::trigger::InboxSource,
    event: &apb_core::connector::inbox::InboxEvent,
    start_key: &StartKey,
) -> FireOutcome {
    let by = apb_core::launch::TriggeredBy {
        rule: entry.rule.name.clone(),
        incarnation: entry.incarnation.clone(),
        source: "inbox".to_string(),
        connector: Some(source.connector.clone()),
        account: Some(source.account.clone()),
        event_seq: Some(event.seq),
    };
    let req = FireRequest {
        root,
        entry,
        source,
        event,
        start_key: start_key.expose().to_string(),
        triggered_by: by,
        dry_run: false,
        admit: &|| Ok(()),
    };
    apb_mcp::tools::trigger::fire(&req)
}

/// One real sweep pass over the project, the tools layer's fire as the
/// pass's start path (what the listener and `apb resume --all` run).
fn sweep_with_fire(root: &Path) -> Vec<(String, Result<RuleSweep, apb_engine::EngineError>)> {
    fn fire(req: &FireRequest<'_>) -> FireOutcome {
        apb_mcp::tools::trigger::fire(req)
    }
    apb_engine::trigger::sweep(root, &|| apb_core::clock::Millis(now()), &fire)
}

fn started_runs(root: &Path) -> Vec<String> {
    fs::read_dir(root.join(".apb/runs"))
        .map(|entries| {
            entries
                .filter_map(Result::ok)
                .filter(|e| e.path().is_dir())
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .collect()
        })
        .unwrap_or_default()
}

/// The launch record of a run's manifest.
fn launch_of(root: &Path, run_id: &str) -> apb_core::launch::LaunchRecord {
    apb_engine::manifest::read(&root.join(".apb/runs").join(run_id))
        .unwrap()
        .and_then(|m| m.launch)
        .unwrap_or_else(|| panic!("run `{run_id}` records its launch"))
}

/// A path-and-bytes snapshot of the project and the config dir; what a
/// dry test must not change (a rewrite of the same size is a change).
fn snapshot(roots: &[&Path]) -> Vec<(String, Vec<u8>)> {
    fn walk(dir: &Path, prefix: &str, out: &mut Vec<(String, Vec<u8>)>) {
        let Ok(entries) = fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let name = format!("{prefix}/{}", entry.file_name().to_string_lossy());
            let Ok(meta) = entry.metadata() else {
                continue;
            };
            if meta.is_dir() {
                out.push((format!("{name}/"), Vec::new()));
                walk(&path, &name, out);
            } else if meta.is_file() {
                out.push((name, fs::read(&path).unwrap_or_default()));
            }
        }
    }
    let mut out = Vec::new();
    for root in roots {
        walk(root, &root.display().to_string(), &mut out);
    }
    out.sort();
    out
}

fn report_of(root: &Path, rule: &str) -> apb_mcp::tools::trigger::TestReport {
    apb_mcp::tools::trigger::test_rule(root, rule, None, false).expect("the report")
}

fn filter_verdict_of(report: &apb_mcp::tools::trigger::TestReport) -> String {
    serde_json::to_value(&report.filter).unwrap()["verdict"]
        .as_str()
        .unwrap()
        .to_string()
}

fn check_of(
    report: &apb_mcp::tools::trigger::TestReport,
    id: apb_mcp::tools::trigger::CheckId,
) -> &apb_mcp::tools::trigger::CheckLine {
    &report.checks[apb_mcp::tools::trigger::CheckId::ORDER
        .iter()
        .position(|i| *i == id)
        .unwrap()]
}

fn line_id(line: &apb_mcp::tools::trigger::CheckLine) -> String {
    serde_json::to_value(line).unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string()
}

/// The state of a line as the wire shows it: `passed`, `refused` with its
/// code, or `not_run`.
fn line_state(line: &apb_mcp::tools::trigger::CheckLine) -> (String, Option<String>) {
    let v = serde_json::to_value(line).unwrap();
    let state = v["state"].as_str().unwrap().to_string();
    let code = v["code"].as_str().map(str::to_string);
    (state, code)
}

// --- the dry test -----------------------------------------------------------

/// A skipped event is explained: the verdict, the condition marks, and no
/// start.
#[test]
fn trigger_test_explains_a_skipped_event() {
    let rig = Rig::new(
        "tt1",
        &simple_pb(
            "tt1",
            "params:\n  - { name: title, type: text, untrusted: true }\n",
        ),
    );
    let root = rig.root();
    // A `where` condition the event fails: the type passes, the where
    // does not, so the event is skipped `where`.
    save_rule(
        &root,
        r#"name: news
playbook: { id: tt1, version: 1.0.0, scope: project }
source:
  inbox:
    connector: github
    account: main
    types: [issues]
    where:
      - { path: action, equals: opened }
    key: "{{event.seq}}"
params:
  title: "{{body.title}}"
limits: { max_per_hour: 5 }
budget: { tokens: 100000, on_no_estimate: continue }
external_data: true
require_ready: false
"#,
    );
    approve_via_tools(&root, "news");
    // The delivery names `closed`, not the `opened` the rule asks for.
    deliver(
        "main",
        "e1",
        json!({ "title": "an issue", "action": "closed" }),
    );
    let report = report_of(&root, "news");
    let filter = serde_json::to_value(&report.filter).unwrap();
    assert_eq!(filter["verdict"], "skipped", "{filter}");
    assert_eq!(filter["code"], "where", "{filter}");
    // The conditions show what passed and what failed, in order.
    let conditions = serde_json::to_value(&report.conditions).unwrap();
    assert_eq!(conditions.as_array().unwrap().len(), 2, "{conditions}");
    assert_eq!(conditions[0]["passed"], true, "{conditions}");
    assert_eq!(conditions[1]["passed"], false, "{conditions}");
    // No start was attempted: no run directory exists.
    assert!(started_runs(&root).is_empty(), "{:?}", started_runs(&root));
}

/// Without values, no rendered param or key value appears anywhere in the
/// report; the characters and the pattern verdicts still do.
#[test]
fn trigger_test_shows_no_values() {
    let rig = Rig::new(
        "tt2",
        &simple_pb(
            "tt2",
            "params:\n  - { name: title, type: text, untrusted: true }\n",
        ),
    );
    let root = rig.root();
    save_rule(
        &root,
        &rule_doc(
            "news",
            "tt2",
            "{{body.entity}}",
            5,
            true,
            "params:\n  title: \"{{body.title}}\"\n",
        ),
    );
    approve_via_tools(&root, "news");
    deliver(
        "main",
        "e1",
        json!({ "entity": "acme/app", "title": "SECRET-TITLE" }),
    );
    let report = apb_mcp::tools::trigger::test_rule(&root, "news", None, false).unwrap();
    let text = serde_json::to_string(&report).unwrap();
    assert!(!text.contains("SECRET-TITLE"), "{text}");
    assert!(!text.contains("acme/app"), "{text}");
    // The characters and the pattern verdict are there all the same.
    let param = &report.params[0];
    assert_eq!(param.chars, Some("SECRET-TITLE".chars().count()));
    assert!(param.value.is_none(), "{param:?}");
    assert_eq!(param.pattern_ok, Some(true), "{param:?}");
    let key = &report.key;
    let tag = key.tag.as_deref().expect("the key is tagged");
    assert_eq!(tag.len(), 64, "{key:?}");
    // The tag is an HMAC of the key, not the key: it shows without a
    // value ask; only the rendered value is gated by `show_values`.
    assert!(text.contains(tag), "the tag is not a value");
    assert!(key.value.is_none(), "{key:?}");
    // With values, the rendered values are there.
    let shown = apb_mcp::tools::trigger::test_rule(&root, "news", None, true).unwrap();
    assert_eq!(shown.params[0].value.as_deref(), Some("SECRET-TITLE"));
    assert_eq!(shown.key.value.as_deref(), Some("acme/app"));
}

/// A dry test writes nothing: the project tree and the config dir are
/// what they were.
#[test]
fn trigger_test_writes_nothing() {
    let rig = Rig::new("tt3", &simple_pb("tt3", ""));
    let root = rig.root();
    save_rule(
        &root,
        &rule_doc("news", "tt3", "{{event.seq}}", 5, true, ""),
    );
    approve_via_tools(&root, "news");
    deliver("main", "e1", json!({ "title": "an issue" }));
    // Taken before the first dry test: a write made only on first use (a
    // directory, a key, a cache entry) is a write too.
    let before = snapshot(&[&root, &rig.cfg_dir()]);
    let report = report_of(&root, "news");
    let after = snapshot(&[&root, &rig.cfg_dir()]);
    if before != after {
        let names =
            |v: &Vec<(String, Vec<u8>)>| v.iter().map(|(n, _)| n.clone()).collect::<Vec<_>>();
        panic!(
            "the dry test wrote something: before {:?} after {:?}",
            names(&before),
            names(&after)
        );
    }
    let _ = report_of(&root, "news");
    assert!(
        snapshot(&[&root, &rig.cfg_dir()]) == after,
        "a second dry test wrote something"
    );
    assert!(report.params.is_empty());
    assert!(matches!(
        report.key_use,
        Some(apb_mcp::tools::trigger::KeyUse::Free)
    ));
}

/// A dry test works in a read-only process: nothing on its path calls the
/// write guard. The fixture is built before the child enters read-only
/// mode (`in_read_only_child_after`); the child's body only reads, so a
/// write anywhere on the test's path fails it.
#[test]
fn trigger_test_works_in_read_only_mode() {
    let path = "trigger_tools_test::trigger_test_works_in_read_only_mode";
    apb_core::process_mode::testing::in_read_only_child_after(
        path,
        || {
            let rig = Rig::new("tt4", &simple_pb("tt4", ""));
            let root = rig.root();
            save_rule(
                &root,
                &rule_doc("news", "tt4", "{{event.seq}}", 5, true, ""),
            );
            approve_via_tools(&root, "news");
            deliver("main", "e1", json!({ "title": "an issue" }));
            // The child process runs this one test alone and exits, so the
            // rig's temp dirs and its env lock may outlive the setup.
            std::mem::forget(rig);
            root
        },
        |root: PathBuf| {
            let report = report_of(&root, "news");
            assert_eq!(filter_verdict_of(&report), "matched");
            assert_eq!(report.checks.len(), 12);
        },
    );
}

/// A run step is refused by name before any read.
#[test]
fn trigger_test_is_refused_in_a_run_step() {
    let rig = Rig::new("tt5", &simple_pb("tt5", ""));
    let root = rig.root();
    save_rule(
        &root,
        &rule_doc("news", "tt5", "{{event.seq}}", 5, true, ""),
    );
    deliver("main", "e1", json!({ "title": "an issue" }));
    // SAFETY: under the rig's env lock, and restored by its vars.
    unsafe { std::env::set_var("APB_RUN_ID", "parent-1") };
    let err = apb_mcp::tools::trigger::test_rule(&root, "news", None, false).unwrap_err();
    unsafe { std::env::remove_var("APB_RUN_ID") };
    assert!(
        err.to_string()
            .contains(apb_mcp::tools::trigger::TRIGGER_TEST_REFUSED),
        "{err}"
    );
}

/// The report names the four checks a real start makes after the dry ones.
#[test]
fn trigger_test_names_the_checks_at_start() {
    let rig = Rig::new("tt6", &simple_pb("tt6", ""));
    let root = rig.root();
    save_rule(
        &root,
        &rule_doc("news", "tt6", "{{event.seq}}", 5, true, ""),
    );
    approve_via_tools(&root, "news");
    deliver("main", "e1", json!({ "title": "an issue" }));
    let report = report_of(&root, "news");
    assert_eq!(
        report.checked_at_start,
        ["probation", "readiness", "concurrency", "working_tree"]
    );
    // A passing dry run claims nothing about them: the last check is
    // passed, never a promise about the start.
    assert_eq!(line_state(report.checks.last().unwrap()).0, "passed");
}

/// Every check of the fire, in order, with the states a start meets: a
/// full pass; a param that does not render; a target whose version is
/// gone; a changed binding; an unapproved rule.
#[test]
fn trigger_test_reports_every_check_in_order() {
    // A full pass: all twelve lines in ORDER, passed. Each section has a
    // rig of its own, scoped so it is dropped (with the env lock) before
    // the next is built.
    let (ids, want, passed_all, key_free) = {
        let rig = Rig::new("tt7", &simple_pb("tt7", ""));
        let root = rig.root();
        save_rule(
            &root,
            &rule_doc("news", "tt7", "{{event.seq}}", 5, true, ""),
        );
        approve_via_tools(&root, "news");
        deliver("main", "e1", json!({ "title": "an issue" }));
        let report = report_of(&root, "news");
        let ids: Vec<String> = report.checks.iter().map(line_id).collect();
        let want: Vec<String> = apb_mcp::tools::trigger::CheckId::ORDER
            .iter()
            .map(|id| {
                serde_json::to_value(id)
                    .unwrap()
                    .as_str()
                    .unwrap()
                    .to_string()
            })
            .collect();
        let passed_all = report
            .checks
            .iter()
            .all(|line| line_state(line).0 == "passed");
        let key_free = matches!(report.key_use, Some(apb_mcp::tools::trigger::KeyUse::Free));
        (ids, want, passed_all, key_free)
    };
    assert_eq!(ids, want, "{ids:?}");
    assert!(passed_all, "{ids:?}");
    assert!(key_free, "{ids:?}");

    // A param that does not render: its ParamCheck carries the failure,
    // RenderParams is refused with the param's code, the rest not run.
    {
        let rig = Rig::new(
            "tt8",
            &simple_pb(
                "tt8",
                "params:\n  - { name: title, type: text, untrusted: true }\n",
            ),
        );
        let root = rig.root();
        save_rule(
            &root,
            &rule_doc(
                "news",
                "tt8",
                "{{event.seq}}",
                5,
                true,
                "params:\n  title: \"{{body.missing}}\"\n",
            ),
        );
        approve_via_tools(&root, "news");
        deliver("main", "e1", json!({ "title": "an issue" }));
        let report = report_of(&root, "news");
        let failure = report.params[0].failure.as_ref().unwrap();
        assert!(
            matches!(failure, apb_mcp::tools::trigger::RenderFailure::Missing { path } if path == "missing"),
            "{failure:?}"
        );
        assert_eq!(report.params[0].chars, None);
        assert_eq!(report.params[0].pattern_ok, None);
        assert_eq!(
            line_state(check_of(
                &report,
                apb_mcp::tools::trigger::CheckId::RenderParams
            )),
            ("refused".into(), Some("param_missing".into()))
        );
        assert_eq!(
            line_state(check_of(
                &report,
                apb_mcp::tools::trigger::CheckId::StartKey
            )),
            ("not_run".into(), None)
        );
        assert!(report.key_use.is_none());
    }

    // A target whose version is gone: the run gate refuses before
    // everything of the launch.
    {
        let rig = Rig::new("tt9", &simple_pb("tt9", ""));
        let root = rig.root();
        save_rule(
            &root,
            &rule_doc("news", "tt9", "{{event.seq}}", 5, true, ""),
        );
        approve_via_tools(&root, "news");
        let vdir = root.join(".apb/playbooks/tt9/1.0.0");
        fs::rename(&vdir, root.join(".apb/playbooks/tt9/kept")).unwrap();
        deliver("main", "e1", json!({ "title": "an issue" }));
        let report = report_of(&root, "news");
        let (state, code) =
            line_state(check_of(&report, apb_mcp::tools::trigger::CheckId::RunGate));
        assert_eq!(state, "refused", "{report:?}");
        assert!(code.is_some(), "the run gate names its code: {report:?}");
        assert_eq!(
            line_state(check_of(&report, apb_mcp::tools::trigger::CheckId::Binding)),
            ("not_run".into(), None)
        );
    }

    // A changed binding: the binding check refuses, the rule is still
    // approved after the test (a test revokes nothing).
    {
        let rig = Rig::new("tt10", &agent_pb("tt10"));
        let root = rig.root();
        save_rule(
            &root,
            &rule_doc("news", "tt10", "{{event.seq}}", 5, true, ""),
        );
        approve_via_tools(&root, "news");
        let changed = agent_pb("tt10").replace("prompt: x", "prompt: changed");
        write_playbook(&root, "tt10", &changed);
        trust_current(&root, "tt10");
        deliver("main", "e1", json!({ "title": "an issue" }));
        let report = report_of(&root, "news");
        assert_eq!(
            line_state(check_of(&report, apb_mcp::tools::trigger::CheckId::RunGate)),
            ("passed".into(), None),
            "{report:?}"
        );
        assert_eq!(
            line_state(check_of(&report, apb_mcp::tools::trigger::CheckId::Binding)),
            ("refused".into(), Some("binding_changed".into())),
            "{report:?}"
        );
        assert_eq!(
            line_state(check_of(&report, apb_mcp::tools::trigger::CheckId::Launch)),
            ("not_run".into(), None),
            "{report:?}"
        );
        let entry = entry_of(&root, "news");
        assert!(
            entry.approval.is_some(),
            "the dry test revokes nothing: {entry:?}"
        );
    }

    // An unapproved rule: the approved line says so; the other lines say
    // what a start would meet once approved.
    {
        let rig = Rig::new("tt11", &simple_pb("tt11", ""));
        let root = rig.root();
        save_rule(
            &root,
            &rule_doc("news", "tt11", "{{event.seq}}", 5, true, ""),
        );
        deliver("main", "e1", json!({ "title": "an issue" }));
        let report = report_of(&root, "news");
        assert_eq!(
            line_state(&report.checks[0]),
            ("refused".into(), Some("rule_not_approved".into()))
        );
        assert_eq!(
            line_state(check_of(&report, apb_mcp::tools::trigger::CheckId::RunGate)),
            ("passed".into(), None)
        );
        assert_eq!(report.checks.len(), 12);
    }
}

/// A start key that names a run answers Used with that run.
#[test]
fn trigger_test_reports_a_used_key() {
    let rig = Rig::new("tt12", &simple_pb("tt12", ""));
    let root = rig.root();
    save_rule(
        &root,
        &rule_doc("news", "tt12", "{{body.entity}}", 5, true, ""),
    );
    approve_via_tools(&root, "news");
    deliver("main", "e1", json!({ "entity": "acme/app" }));
    // The real start binds the key to its run.
    let run_id = match fire_newest(&root, "news") {
        FireOutcome::Started { run_id } => {
            let _reaper = DriverReaper::new(&root, &run_id);
            run_id
        }
        other => panic!("the first event starts: {other:?}"),
    };
    // A second delivery with the same key: the dry test reports the key
    // as used, naming the run.
    deliver("main", "e2", json!({ "entity": "acme/app" }));
    let report = report_of(&root, "news");
    match &report.key_use {
        Some(apb_mcp::tools::trigger::KeyUse::Used {
            run_id: named,
            conflict,
        }) => {
            assert_eq!(named, &run_id);
            assert!(!conflict);
        }
        other => panic!("the key is used: {other:?}"),
    }
}

/// A journal the reader refuses is the list's `journal_error`: the one
/// sentence with the remedy, carried to MCP and the dashboard as is.
#[test]
fn trigger_list_carries_a_refused_journal() {
    let rig = Rig::new("tt13b", &simple_pb("tt13b", ""));
    let root = rig.root();
    save_rule(
        &root,
        &rule_doc("news", "tt13b", "{{event.seq}}", 5, true, ""),
    );
    let inc = apb_mcp::tools::trigger::list(&root).unwrap()[0]
        .incarnation
        .clone();
    // A directory at the journal's path: the bounded read refuses it.
    let log = root
        .join(".apb/trigger-state/log")
        .join(format!("{inc}.jsonl"));
    std::fs::create_dir_all(log.parent().unwrap()).unwrap();
    std::fs::create_dir(&log).unwrap();
    let views = apb_mcp::tools::trigger::list(&root).unwrap();
    let sentence = views[0].journal_error.as_ref().expect("the refusal");
    assert!(sentence.contains("journal_refused"), "{sentence}");
    assert!(
        sentence.contains("; remove the rule and save it again, or delete .apb/trigger-state/log/"),
        "{sentence}"
    );
}

/// The list shows every rule, with needs_approval filled.
#[test]
fn trigger_list_lists_rules() {
    let rig = Rig::new("tt13", &simple_pb("tt13", ""));
    let root = rig.root();
    save_rule(
        &root,
        &rule_doc("fresh", "tt13", "{{event.seq}}", 5, true, ""),
    );
    save_rule(
        &root,
        &rule_doc("kept", "tt13", "{{event.seq}}", 5, true, ""),
    );
    approve_via_tools(&root, "kept");
    let views = apb_mcp::tools::trigger::list(&root).unwrap();
    assert_eq!(views.len(), 2, "{views:?}");
    let fresh = views.iter().find(|v| v.name == "fresh").unwrap();
    assert!(fresh.needs_approval, "{fresh:?}");
    assert!(!fresh.approved);
    assert_eq!(fresh.source, "inbox github/main [issues]");
    assert_eq!(fresh.playbook, "tt13");
    assert_eq!(fresh.version, "1.0.0");
    let kept = views.iter().find(|v| v.name == "kept").unwrap();
    assert!(!kept.needs_approval, "{kept:?}");
    // A rule whose binding changed lists as needing approval again.
    let changed = simple_pb("tt13", "description: changed\n");
    write_playbook(&root, "tt13", &changed);
    trust_current(&root, "tt13");
    let views = apb_mcp::tools::trigger::list(&root).unwrap();
    let kept = views.iter().find(|v| v.name == "kept").unwrap();
    assert!(kept.needs_approval, "{kept:?}");
}

// --- the fire through the gate ----------------------------------------------

/// A rule is no trust approval: an untrusted playbook refuses the fire.
#[test]
fn the_fire_refuses_an_untrusted_playbook() {
    let rig = Rig::new("tt14", &simple_pb("tt14", ""));
    let root = rig.root();
    save_rule(
        &root,
        &rule_doc("news", "tt14", "{{event.seq}}", 5, true, ""),
    );
    approve_via_tools(&root, "news");
    // The definition changed and nobody approved the new digest.
    let changed = simple_pb("tt14", "").replace("name: tt14", "name: tt14 alt");
    write_playbook(&root, "tt14", &changed);
    deliver("main", "e1", json!({ "title": "an issue" }));
    let outcome = fire_newest(&root, "news");
    match outcome {
        FireOutcome::Refused { code, name } => {
            assert_eq!(code, "untrusted_requires_acknowledge", "{code}");
            assert_eq!(name, None);
        }
        other => panic!("an untrusted playbook refuses the fire: {other:?}"),
    }
    assert!(started_runs(&root).is_empty(), "{:?}", started_runs(&root));
}

/// An approval of another rule digest is not an approval: the fire refuses
/// the start as needing one.
#[test]
fn a_stale_approval_is_refused() {
    let rig = Rig::new("tt15", &simple_pb("tt15", ""));
    let root = rig.root();
    save_rule(
        &root,
        &rule_doc("news", "tt15", "{{event.seq}}", 5, true, ""),
    );
    // A digest that is not the entry's current one.
    approve_for_digest(&root, "news", "sha256:stale");
    deliver("main", "e1", json!({ "title": "an issue" }));
    let outcome = fire_newest(&root, "news");
    match outcome {
        FireOutcome::Refused { code, name } => {
            assert_eq!(code, "launch_requires_rule_approval", "{code}");
            assert_eq!(name, None);
        }
        other => panic!("a stale approval refuses the fire: {other:?}"),
    }
    assert!(started_runs(&root).is_empty());
}

/// The shown need must be the rule as it stands: another rule digest is
/// refused.
#[test]
fn approve_refuses_a_need_of_another_rule_digest() {
    let rig = Rig::new("tt16", &simple_pb("tt16", ""));
    let root = rig.root();
    save_rule(
        &root,
        &rule_doc("news", "tt16", "{{event.seq}}", 5, true, ""),
    );
    let entry = entry_of(&root, "news");
    let mut need = apb_mcp::tools::trigger::approval_need(&root, &entry).unwrap();
    need.rule_digest = "sha256:another".to_string();
    let err = apb_mcp::tools::trigger::approve(&root, "news", &need, "terminal").unwrap_err();
    assert_eq!(
        err["policy"],
        apb_mcp::tools::trigger::TRIGGER_APPROVAL_CHANGED,
        "{err}"
    );
    assert!(entry_of(&root, "news").approval.is_none());
}

/// A permit that changed between showing and approving is refused: the
/// fresh need's binding differs.
#[test]
fn approve_refuses_a_changed_permit() {
    let rig = Rig::new("tt17", &agent_pb("tt17"));
    let root = rig.root();
    save_rule(
        &root,
        &rule_doc("news", "tt17", "{{event.seq}}", 5, true, ""),
    );
    let entry = entry_of(&root, "news");
    let shown = apb_mcp::tools::trigger::approval_need(&root, &entry).unwrap();
    // The target changes while the person looks at the need.
    let changed = agent_pb("tt17").replace("prompt: x", "prompt: changed");
    write_playbook(&root, "tt17", &changed);
    trust_current(&root, "tt17");
    let err = apb_mcp::tools::trigger::approve(&root, "news", &shown, "terminal").unwrap_err();
    assert_eq!(
        err["policy"],
        apb_mcp::tools::trigger::TRIGGER_APPROVAL_CHANGED,
        "{err}"
    );
    assert!(entry_of(&root, "news").approval.is_none());
}

/// Admission catches a changed permit: allow and ask. A start attempted
/// under the changed target revokes the approval; restoring the old target
/// afterwards does not bring it back.
#[test]
fn a_changed_permit_is_caught_in_admission() {
    for (id, policy_text) in [("tt18a", ""), ("tt18b", "launch:\n  agent: ask\n")] {
        let rig = Rig::new(id, &agent_pb(id));
        let root = rig.root();
        rig.policy(policy_text);
        save_rule(&root, &rule_doc("news", id, "{{event.seq}}", 5, true, ""));
        let binding = approve_via_tools(&root, "news");
        // The first pass positions the rule at the head it reads and starts
        // nothing (I5); without it the pass after the change would only
        // position again and no start would ever be attempted.
        let positioned = sweep_with_fire(&root);
        assert_eq!(positioned.len(), 1, "{positioned:?}");
        assert!(positioned[0].1.is_ok(), "{positioned:?}");
        deliver("main", "e1", json!({ "title": "an issue" }));
        // The target changes; the next matching event attempts a start.
        let changed = agent_pb(id).replace("prompt: x", "prompt: changed");
        write_playbook(&root, id, &changed);
        trust_current(&root, id);
        let report = sweep_with_fire(&root);
        assert_eq!(report.len(), 1, "{report:?}");
        assert!(report[0].1.is_ok(), "{report:?}");
        assert!(
            started_runs(&root).is_empty(),
            "nothing starts under the changed binding: {:?}",
            started_runs(&root)
        );
        let entry = entry_of(&root, "news");
        assert!(
            entry.approval.is_none(),
            "the changed binding revoked the approval: {entry:?}"
        );
        // The journal records the revocation's reason, with the binding
        // resolved and the approved one apart.
        let lines = apb_core::trigger::read(&root, &entry.incarnation, 10);
        let changed_line = lines
            .iter()
            .find(|l| l.code.as_deref() == Some("binding_changed"))
            .expect("the binding_changed line");
        assert_ne!(
            changed_line.binding.as_deref(),
            Some(binding.as_str()),
            "the line names the binding resolved, not the approved one"
        );
        assert_eq!(
            changed_line.approved.as_deref(),
            Some(binding.as_str()),
            "the line names what was approved"
        );
        // Restoring the old target does not bring the approval back.
        write_playbook(&root, id, &agent_pb(id));
        trust_current(&root, id);
        deliver("main", "e2", json!({ "title": "another" }));
        let report = sweep_with_fire(&root);
        assert_eq!(
            report.len(),
            0,
            "an unapproved rule is not swept: {report:?}"
        );
        assert!(started_runs(&root).is_empty(), "{:?}", started_runs(&root));
    }
}

/// A change after admission fails preparation's own bundle check and
/// starts nothing: the fire is held at the concurrency key's admission and
/// the profile changes under the pinned one.
#[test]
fn a_change_after_admission_fails_preparation() {
    // A concurrency key, so the start is held at the key's admission (the
    // hook lives inside preparation's key branch).
    let rig = Rig::new(
        "tt19",
        &agent_pb_with("tt19", "concurrency:\n  key: triage\n"),
    );
    let root = rig.root();
    save_rule(
        &root,
        &rule_doc("news", "tt19", "{{event.seq}}", 5, true, ""),
    );
    approve_via_tools(&root, "news");
    deliver("main", "e1", json!({ "title": "an issue" }));
    // At the key's admission (inside preparation) the profile changes.
    let hook_root = root.clone();
    apb_engine::deadline_hooks::set(
        apb_engine::deadline_hooks::Hook::KeyAdmitted,
        &root,
        move |_| {
            fs::write(
                hook_root.join(".apb/profiles/main/SOUL.md"),
                "a changed role\n",
            )
            .unwrap();
        },
    );
    let outcome = fire_newest(&root, "news");
    apb_engine::deadline_hooks::clear(&root);
    assert!(
        !matches!(outcome, FireOutcome::Started { .. }),
        "a changed bundle starts nothing: {outcome:?}"
    );
    // No run got a manifest: preparation refused before it wrote one.
    for run in started_runs(&root) {
        let manifest = apb_engine::manifest::read(&root.join(".apb/runs").join(&run));
        assert!(
            manifest.as_ref().map_or(true, |m| m.is_none()),
            "run `{run}` has a manifest: {manifest:?}"
        );
    }
}

/// An `ask` launch needs a person's confirmation; the rule's approval is
/// that confirmation, and no launch nonce is stored.
#[test]
fn an_ask_rule_starts_without_a_launch_nonce() {
    let rig = Rig::new("tt20", &agent_pb("tt20"));
    let root = rig.root();
    rig.policy("launch:\n  agent: ask\n");
    save_rule(
        &root,
        &rule_doc("news", "tt20", "{{event.seq}}", 5, true, ""),
    );
    let binding = approve_via_tools(&root, "news");
    assert!(!binding.is_empty());
    deliver("main", "e1", json!({ "title": "an issue" }));
    let outcome = fire_newest(&root, "news");
    let run_id = match outcome {
        FireOutcome::Started { run_id } => run_id,
        other => panic!("an approved ask rule starts: {other:?}"),
    };
    let _reaper = DriverReaper::new(&root, &run_id);
    let record = launch_of(&root, &run_id);
    assert_eq!(
        record.policy,
        apb_core::launch::LaunchPolicy::Ask,
        "{record:?}"
    );
    assert_eq!(record.confirmation.as_deref(), Some("rule_approval"));
    assert_eq!(record.initiator, "trigger:news", "{record:?}");
    assert!(record.triggered_by.is_some(), "{record:?}");
}

/// The binding the gate resolves for the rule's target right now, from the
/// `launch_requires_rule_approval` refusal an unbound rule start meets.
/// A person's `approve` computes it through [`approve_via_tools`], which a
/// billing `ask` target refuses; a hand approval (what a rule store may
/// hold) binds it directly.
fn binding_of(root: &Path, name: &str) -> String {
    let entry = entry_of(root, name);
    let wref = apb_core::scope::PlaybookRef {
        origin: apb_core::scope::Origin::Project { workspace_id: None },
        id: entry.rule.playbook.id.clone(),
        version: Some(entry.rule.playbook.version.clone()),
    };
    let by = Box::new(apb_core::launch::TriggeredBy {
        rule: entry.rule.name.clone(),
        incarnation: entry.incarnation.clone(),
        source: "inbox".to_string(),
        connector: None,
        account: None,
        event_seq: None,
    });
    let launch = apb_engine::gate::launch::LaunchContext::new(
        apb_engine::gate::launch::Initiator::Trigger { by },
        apb_engine::gate::launch::LaunchKind::Start,
    )
    .with_external_data(entry.rule.external_data);
    let refusal = apb_engine::gate::check_run(root, &wref, false, false, &launch).unwrap_err();
    assert_eq!(
        refusal["policy"],
        apb_engine::gate::launch::TRIGGER_POLICY,
        "the probe refusal: {refusal}"
    );
    refusal["binding"].as_str().unwrap().to_string()
}

/// Approves the rule for the binding the gate resolves now, without the
/// need's second pass (which a billing `ask` target refuses).
fn approve_with_resolved_binding(root: &Path, name: &str) {
    let entry = entry_of(root, name);
    let approval = Approval {
        rule_digest: apb_core::trigger::rule_digest(&entry),
        playbook_digest: "digest".to_string(),
        binding: binding_of(root, name),
        permit_digest: "permit".to_string(),
        launch_policy: "ask".to_string(),
        consent_nonce: None,
        approved_at: now(),
        approved_by: "test".to_string(),
    };
    store_approval(root, name, approval, Millis(now())).unwrap();
}

/// A billing `ask` a rule cannot confirm refuses the fire. The target's
/// executor is `dsh` with an undeclared model, whose basis is unknown, so
/// `unknown_basis: ask` makes the billing decision ask; the claude fixture
/// runs on a subscription, which is never held.
#[test]
fn a_billing_ask_refuses_a_rule_start() {
    let rig = Rig::new("tt21", &agent_pb_dsh("tt21"));
    let root = rig.root();
    rig.policy("billing:\n  billed: ask\n  unknown_basis: ask\n");
    save_rule(
        &root,
        &rule_doc("news", "tt21", "{{event.seq}}", 5, true, ""),
    );
    // A person's `approve` refuses a billing ask target; the store holds
    // the approval bound to the binding as it stands, and the launch
    // decision refuses the start.
    approve_with_resolved_binding(&root, "news");
    deliver("main", "e1", json!({ "title": "an issue" }));
    let outcome = fire_newest(&root, "news");
    match outcome {
        FireOutcome::Refused { code, name } => {
            assert_eq!(code, "billing_refused_for_trigger", "{code}");
            assert_eq!(name, None);
        }
        other => panic!("a billing ask refuses the fire: {other:?}"),
    }
    assert!(started_runs(&root).is_empty());
}

/// A stricter launch policy after the approval revokes it at the first
/// attempted start; loosening back does not restore it.
#[test]
fn a_stricter_policy_revokes_the_approval() {
    let rig = Rig::new("tt22", &agent_pb("tt22"));
    let root = rig.root();
    save_rule(
        &root,
        &rule_doc("news", "tt22", "{{event.seq}}", 5, true, ""),
    );
    approve_via_tools(&root, "news");
    // The first pass positions the rule (I5); without it no start would
    // ever be attempted.
    let positioned = sweep_with_fire(&root);
    assert_eq!(positioned.len(), 1, "{positioned:?}");
    deliver("main", "e1", json!({ "title": "an issue" }));
    // The machine's policy tightens to deny.
    rig.policy("launch:\n  agent: deny\n");
    let report = sweep_with_fire(&root);
    assert_eq!(report.len(), 1, "{report:?}");
    assert!(started_runs(&root).is_empty(), "{:?}", started_runs(&root));
    let entry = entry_of(&root, "news");
    assert!(entry.approval.is_none(), "revoked: {entry:?}");
    // The policy loosens back; the approval stays gone.
    rig.policy("");
    deliver("main", "e2", json!({ "title": "another" }));
    let report = sweep_with_fire(&root);
    assert_eq!(
        report.len(),
        0,
        "an unapproved rule is not swept: {report:?}"
    );
    assert!(started_runs(&root).is_empty());
}

/// A changed billing decision after the approval revokes it the same way.
#[test]
fn a_changed_billing_decision_revokes_the_approval() {
    let rig = Rig::new("tt23", &agent_pb("tt23"));
    let root = rig.root();
    rig.policy("billing:\n  billed: allow\n  unknown_basis: allow\n");
    save_rule(
        &root,
        &rule_doc("news", "tt23", "{{event.seq}}", 5, true, ""),
    );
    approve_via_tools(&root, "news");
    // The first pass positions the rule (I5); without it no start would
    // ever be attempted.
    let positioned = sweep_with_fire(&root);
    assert_eq!(positioned.len(), 1, "{positioned:?}");
    deliver("main", "e1", json!({ "title": "an issue" }));
    // The machine's billing decision tightens to deny.
    rig.policy("billing:\n  billed: deny\n  unknown_basis: deny\n");
    let report = sweep_with_fire(&root);
    assert_eq!(report.len(), 1, "{report:?}");
    assert!(started_runs(&root).is_empty(), "{:?}", started_runs(&root));
    let entry = entry_of(&root, "news");
    assert!(entry.approval.is_none(), "revoked: {entry:?}");
    // The billing decision returns to what was approved; the approval
    // stays gone.
    rig.policy("billing:\n  billed: allow\n  unknown_basis: allow\n");
    deliver("main", "e2", json!({ "title": "another" }));
    let report = sweep_with_fire(&root);
    assert_eq!(
        report.len(),
        0,
        "an unapproved rule is not swept: {report:?}"
    );
    assert!(started_runs(&root).is_empty());
}

/// A param that misses the playbook's declared pattern refuses the fire,
/// the param's name with it, never a value.
#[test]
fn a_pattern_refusal_names_the_param() {
    let id = "tt24";
    let rig = Rig::new(
        id,
        &simple_pb(
            id,
            "params:\n  - { name: title, type: text, untrusted: true, pattern: '[a-z ]{1,40}' }\n",
        ),
    );
    let root = rig.root();
    save_rule(
        &root,
        &rule_doc(
            "news",
            id,
            "{{event.seq}}",
            5,
            true,
            "params:\n  title: \"{{body.title}}\"\n",
        ),
    );
    approve_via_tools(&root, "news");
    deliver("main", "e1", json!({ "title": "UPPER CASE TITLE" }));
    let outcome = fire_newest(&root, "news");
    match outcome {
        FireOutcome::Refused { code, name } => {
            assert_eq!(code, "param_pattern_mismatch", "{code}");
            assert_eq!(name.as_deref(), Some("title"), "{code} {name:?}");
        }
        other => panic!("a pattern mismatch refuses the fire: {other:?}"),
    }
    assert!(started_runs(&root).is_empty());
}

/// The dry test shows the pattern verdict on the param, and the check
/// refused with the code the start would record.
#[test]
fn the_fire_refuses_a_param_that_misses_its_pattern() {
    let id = "tt25";
    let rig = Rig::new(
        id,
        &simple_pb(
            id,
            "params:\n  - { name: title, type: text, untrusted: true, pattern: '[a-z ]{1,40}' }\n",
        ),
    );
    let root = rig.root();
    save_rule(
        &root,
        &rule_doc(
            "news",
            id,
            "{{event.seq}}",
            5,
            true,
            "params:\n  title: \"{{body.title}}\"\n",
        ),
    );
    approve_via_tools(&root, "news");
    deliver("main", "e1", json!({ "title": "UPPER CASE TITLE" }));
    let report = report_of(&root, "news");
    assert_eq!(report.params[0].pattern_ok, Some(false), "{report:?}");
    assert_eq!(
        report.params[0].chars,
        Some("UPPER CASE TITLE".chars().count())
    );
    assert_eq!(
        line_state(check_of(
            &report,
            apb_mcp::tools::trigger::CheckId::ParamPatterns
        )),
        ("refused".into(), Some("param_pattern_mismatch".into())),
        "{report:?}"
    );
    assert_eq!(
        line_state(check_of(&report, apb_mcp::tools::trigger::CheckId::Budget)),
        ("not_run".into(), None)
    );
}

/// Preparation checks probation for an unattended start, which a rule's
/// start always is; the fire sets `waive_probation` false and preparation
/// refuses.
#[test]
fn an_unattended_fire_is_refused_by_probation() {
    let id = "tt26";
    let rig = Rig::new(
        id,
        &simple_pb(id, "unattended:\n  after_attended_successes: 1\n"),
    );
    let root = rig.root();
    save_rule(&root, &rule_doc("news", id, "{{event.seq}}", 5, true, ""));
    approve_via_tools(&root, "news");
    deliver("main", "e1", json!({ "title": "an issue" }));
    let outcome = fire_newest(&root, "news");
    match outcome {
        FireOutcome::Refused { code, name } => {
            assert_eq!(code, "probation_required", "{code}");
            assert_eq!(name, None);
        }
        other => panic!("probation refuses the fire: {other:?}"),
    }
    assert!(started_runs(&root).is_empty());
}

/// A playbook whose concurrency block refuses when busy refuses the real
/// fire, and the sweep records that refusal as the event's verdict: the
/// journal line holds the code only and the position moves past the
/// event (every refusal is final for its event).
#[test]
fn a_refuse_concurrency_block_refuses_the_fire() {
    let id = "tt27";
    let yaml = format!(
        "schema: 2\nid: {id}\nname: {id}\nversion: 1.0.0\nconcurrency:\n  key: triage\n  on_busy: refuse\nnodes:\n  - {{ id: start, type: start }}\n  - {{ id: work, type: script, script: \"scripts/work.sh\", runner: sh }}\n  - {{ id: done, type: finish, outcome: success }}\nedges:\n  - {{ from: start, to: work }}\n  - {{ from: work, to: done }}\n"
    );
    let rig = Rig::new(id, &yaml);
    let root = rig.root();
    let script = root
        .join(".apb/playbooks")
        .join(id)
        .join("1.0.0/scripts/work.sh");
    // The holder's script says it runs, then waits while `hold` exists.
    let r = root.display();
    fs::write(
        &script,
        format!(
            "#!/bin/sh\ntouch \"{r}/started\"\nwhile [ -f \"{r}/hold\" ]; do sleep 0.05; done\n"
        ),
    )
    .unwrap();
    fs::write(root.join("hold"), "").unwrap();
    // The script is part of the definition's trust digest: approve the
    // definition as it stands now, script included.
    trust_current(&root, id);
    save_rule(&root, &rule_doc("news", id, "{{event.seq}}", 5, true, ""));
    approve_via_tools(&root, "news");
    sweep_with_fire(&root);
    // A holder run of the same key, started in-process and joined before
    // the test ends.
    let holder_root = root.clone();
    let holder = std::thread::spawn(move || {
        apb_engine::run(
            &holder_root,
            id,
            Some("1.0.0"),
            apb_engine::RunOptions::default(),
        )
    });
    // The positive marker: the holder's work runs, so it holds the key.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
    while !root.join("started").is_file() {
        assert!(
            std::time::Instant::now() < deadline,
            "the holder never started"
        );
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    deliver("main", "e1", json!({ "title": "an issue" }));
    let out = sweep_with_fire(&root);
    fs::remove_file(root.join("hold")).unwrap();
    let joined = holder.join().expect("the holder finishes");
    assert!(joined.is_ok(), "{joined:?}");
    let report = out[0].1.as_ref().unwrap();
    assert!(report.started.is_empty(), "{out:?}");
    assert_eq!(report.stopped, None, "a refusal does not end the pass");
    let lines = journal_of(&root, "news");
    let line = lines
        .iter()
        .find(|l| l.seq == Some(1))
        .unwrap_or_else(|| panic!("the event's line: {lines:?}"));
    assert_eq!(line.verdict, apb_core::trigger::Verdict::Refused);
    assert_eq!(line.code.as_deref(), Some("concurrency_busy"));
    assert_eq!(line.name, None);
    assert_eq!(seen_through_of(&root, "news"), 1, "the position moved past");
    // The refusal freed the fire's own start key: only the holder ran.
    assert_eq!(started_runs(&root).len(), 1, "{:?}", started_runs(&root));
}

/// A busy tree queues the start instead of refusing it: with the
/// dashboard's `workdir_queue_wait` the fire answers Started and the run
/// waits for the tree in its drive.
#[test]
fn a_busy_tree_queues_the_start() {
    let id = "tt28";
    let rig = Rig::new(id, &simple_pb(id, ""));
    let root = rig.root();
    fs::write(
        rig.cfg_dir().join("config.yaml"),
        "server:\n  workdir_queue_wait_seconds: 20\n",
    )
    .unwrap();
    save_rule(&root, &rule_doc("news", id, "{{event.seq}}", 5, true, ""));
    approve_via_tools(&root, "news");
    deliver("main", "e1", json!({ "title": "an issue" }));
    // A live pid holds the tree lock.
    let lock = root.join(".apb/workdir.lock");
    fs::create_dir_all(lock.parent().unwrap()).unwrap();
    fs::write(&lock, std::process::id().to_string()).unwrap();
    let outcome = fire_newest(&root, "news");
    match outcome {
        FireOutcome::Started { run_id } => {
            let _reaper = DriverReaper::new(&root, &run_id);
            let dir = root.join(".apb/runs").join(&run_id);
            assert!(
                apb_engine::manifest::read(&dir).unwrap().is_some(),
                "the run was admitted: {run_id}"
            );
            fs::remove_file(&lock).ok();
        }
        FireOutcome::Refused { code, .. } if code == "workdir_busy" => {
            panic!("a queued start is not a workdir_busy refusal")
        }
        other => panic!("the start is admitted and waits: {other:?}"),
    }
}

/// A key whose stored request differs answers already started with the
/// conflict named: one run per rendered key is the rule's contract.
#[test]
fn a_key_conflict_is_recorded_as_already_started() {
    let id = "tt29";
    let rig = Rig::new(
        id,
        &simple_pb(
            id,
            "params:\n  - { name: title, type: text, untrusted: true }\n",
        ),
    );
    let root = rig.root();
    save_rule(
        &root,
        &rule_doc(
            "news",
            id,
            "{{body.entity}}",
            5,
            true,
            "params:\n  title: \"{{body.title}}\"\n",
        ),
    );
    approve_via_tools(&root, "news");
    deliver(
        "main",
        "e1",
        json!({ "entity": "acme/app", "title": "first" }),
    );
    let first = match fire_newest(&root, "news") {
        FireOutcome::Started { run_id } => {
            let _reaper = DriverReaper::new(&root, &run_id);
            run_id
        }
        other => panic!("the first event starts: {other:?}"),
    };
    // The same entity, another title: the same key, another request.
    deliver(
        "main",
        "e2",
        json!({ "entity": "acme/app", "title": "second" }),
    );
    let outcome = fire_newest(&root, "news");
    match outcome {
        FireOutcome::AlreadyStarted { run_id, conflict } => {
            assert_eq!(run_id, first, "{run_id} {first}");
            assert!(conflict, "a different request is a conflict");
        }
        other => panic!("the same key answers already started: {other:?}"),
    }
    assert_eq!(started_runs(&root).len(), 1, "{:?}", started_runs(&root));
}

// --- review round 1: the start key's pending entries, dedupe first ---------

/// The rule's position in the signed state.
fn seen_through_of(root: &Path, name: &str) -> u64 {
    let inc = entry_of(root, name).incarnation;
    read_signed::<apb_core::trigger::TriggerStateFile>(root, Scope::Project, "state")
        .unwrap()
        .file
        .rules
        .iter()
        .find(|s| s.rule == name && s.incarnation == inc)
        .map_or(0, |s| s.seen_through)
}

/// The rule's journal, newest last.
fn journal_of(root: &Path, name: &str) -> Vec<apb_core::trigger::LogEntry> {
    apb_core::trigger::read(root, &entry_of(root, name).incarnation, 100)
}

/// A start of the same key still being prepared (claimed and attached, its
/// owner alive, not bound) never consumes the event: the sweep holds it as
/// `start_key_busy` with the position before it. Once that start is bound,
/// the next pass answers `already_started` and moves past it.
#[test]
fn a_start_still_being_prepared_holds_the_event() {
    let rig = Rig::new("tt40", &simple_pb("tt40", ""));
    let root = rig.root();
    save_rule(
        &root,
        &rule_doc("news", "tt40", "{{body.entity}}", 5, true, ""),
    );
    approve_via_tools(&root, "news");
    sweep_with_fire(&root);
    deliver("main", "e1", json!({ "entity": "acme/app" }));
    // Another start with the rule's own key, held right after its claim's
    // attach: its entry names its run and its owner is alive.
    let hook = apb_engine::start_key::test_hooks::hold_after_attach(&root);
    let (tx, rx) = std::sync::mpsc::channel();
    let held_root = root.clone();
    std::thread::spawn(move || {
        let _ = tx.send(fire_newest(&held_root, "news"));
    });
    hook.wait();
    let out = sweep_with_fire(&root);
    let report = out[0].1.as_ref().unwrap();
    assert_eq!(report.stopped.as_deref(), Some("start_key_busy"), "{out:?}");
    assert_eq!(seen_through_of(&root, "news"), 0, "the position stays");
    let lines = journal_of(&root, "news");
    assert!(
        lines
            .iter()
            .any(|l| l.seq == Some(1) && l.code.as_deref() == Some("start_key_busy")),
        "{lines:?}"
    );
    assert!(
        !lines
            .iter()
            .any(|l| l.verdict == apb_core::trigger::Verdict::AlreadyStarted),
        "{lines:?}"
    );
    // The held start goes on and binds; the next pass answers it.
    apb_engine::start_key::test_hooks::clear(&root);
    hook.wait();
    let run_id = match rx.recv().unwrap() {
        FireOutcome::Started { run_id } => run_id,
        other => panic!("the held start starts: {other:?}"),
    };
    let _reaper = DriverReaper::new(&root, &run_id);
    sweep_with_fire(&root);
    assert_eq!(seen_through_of(&root, "news"), 1);
    assert!(
        journal_of(&root, "news").iter().any(|l| l.seq == Some(1)
            && l.verdict == apb_core::trigger::Verdict::AlreadyStarted
            && l.run_id.as_deref() == Some(run_id.as_str())),
        "{:?}",
        journal_of(&root, "news")
    );
}

/// Deduplication before limits: a run the key is bound to answers
/// `already_started` even at the hourly cap (a pass that crashed after the
/// start and before its outcome recovers at once, not an hour later).
#[test]
fn a_bound_key_answers_before_the_hourly_cap() {
    let rig = Rig::new("tt41", &simple_pb("tt41", ""));
    let root = rig.root();
    save_rule(
        &root,
        &rule_doc("news", "tt41", "{{body.entity}}", 1, true, ""),
    );
    approve_via_tools(&root, "news");
    sweep_with_fire(&root);
    deliver("main", "e1", json!({ "entity": "acme/app" }));
    // The start a crashed pass made: the run exists, the outcome was never
    // recorded and the position never moved.
    let run_id = match fire_newest(&root, "news") {
        FireOutcome::Started { run_id } => run_id,
        other => panic!("the event starts: {other:?}"),
    };
    let _reaper = DriverReaper::new(&root, &run_id);
    assert_eq!(seen_through_of(&root, "news"), 0);
    let out = sweep_with_fire(&root);
    let report = out[0].1.as_ref().unwrap();
    assert_eq!(report.stopped, None, "{out:?}");
    assert_eq!(seen_through_of(&root, "news"), 1);
    let lines = journal_of(&root, "news");
    assert!(
        lines.iter().any(|l| l.seq == Some(1)
            && l.verdict == apb_core::trigger::Verdict::AlreadyStarted
            && l.run_id.as_deref() == Some(run_id.as_str())),
        "{lines:?}"
    );
    assert!(
        !lines.iter().any(|l| l.code.as_deref() == Some("rate_cap")),
        "{lines:?}"
    );
}

/// The dry test's key use comes from the fire's own lookup: a rule with
/// `external_data: false` whose key started a run reports the key used by
/// that run for the same request, never a conflict.
#[test]
fn trigger_test_reports_the_key_the_fire_reads() {
    let rig = Rig::new("tt42", &simple_pb("tt42", ""));
    let root = rig.root();
    save_rule(
        &root,
        &rule_doc("news", "tt42", "{{body.entity}}", 5, false, ""),
    );
    approve_via_tools(&root, "news");
    deliver("main", "e1", json!({ "entity": "acme/app" }));
    let run_id = match fire_newest(&root, "news") {
        FireOutcome::Started { run_id } => run_id,
        other => panic!("the first event starts: {other:?}"),
    };
    let _reaper = DriverReaper::new(&root, &run_id);
    deliver("main", "e2", json!({ "entity": "acme/app" }));
    let report = report_of(&root, "news");
    match &report.key_use {
        Some(apb_mcp::tools::trigger::KeyUse::Used {
            run_id: named,
            conflict,
        }) => {
            assert_eq!(named, &run_id);
            assert!(!conflict, "the same request, not another one");
        }
        other => panic!("the key is used: {other:?}"),
    }
}

/// Two claims of one key (S5): the fire whose lookup said the key is free
/// loses the claim to a start that claimed and attached while it was
/// parked after its lookup; its claim answers the winner's run through
/// `claim_answer`, which is still preparing, so the event is held
/// (`Busy`), and once the winner is bound the next pass answers it and
/// moves past.
#[test]
fn a_key_claimed_between_lookup_and_start_holds_the_event() {
    let rig = Rig::new("tt42", &simple_pb("tt42", ""));
    let root = rig.root();
    save_rule(
        &root,
        &rule_doc("news", "tt42", "{{body.entity}}", 5, true, ""),
    );
    approve_via_tools(&root, "news");
    sweep_with_fire(&root);
    deliver("main", "e1", json!({ "entity": "acme/app" }));
    // B parks right after its lookup answered free.
    let parked = apb_mcp::tools::trigger::test_hooks::hold_after_lookup(&root);
    let (tx, rx) = std::sync::mpsc::channel();
    let loser_root = root.clone();
    std::thread::spawn(move || {
        let _ = tx.send(fire_newest(&loser_root, "news"));
    });
    parked.wait();
    // A claims the key and parks right after its attach.
    let attached = apb_engine::start_key::test_hooks::hold_after_attach(&root);
    let (tx2, rx2) = std::sync::mpsc::channel();
    let winner_root = root.clone();
    std::thread::spawn(move || {
        let _ = tx2.send(fire_newest(&winner_root, "news"));
    });
    attached.wait();
    // B goes on: its claim answers A's attached, still preparing run.
    apb_mcp::tools::trigger::test_hooks::clear(&root);
    parked.wait();
    assert_eq!(rx.recv().unwrap(), FireOutcome::Busy, "the event is held");
    // A goes on and binds.
    apb_engine::start_key::test_hooks::clear(&root);
    attached.wait();
    let run_id = match rx2.recv().unwrap() {
        FireOutcome::Started { run_id } => run_id,
        other => panic!("the winner starts: {other:?}"),
    };
    let _reaper = DriverReaper::new(&root, &run_id);
    // The loser never consumed the event: the next pass answers the
    // winner's bound run and moves past.
    assert_eq!(seen_through_of(&root, "news"), 0, "the position stays");
    sweep_with_fire(&root);
    assert_eq!(seen_through_of(&root, "news"), 1);
    let lines = journal_of(&root, "news");
    assert!(
        lines.iter().any(|l| {
            l.verdict == apb_core::trigger::Verdict::AlreadyStarted
                && l.run_id.as_deref() == Some(run_id.as_str())
        }),
        "{lines:?}"
    );
    assert_eq!(started_runs(&root).len(), 1, "{:?}", started_runs(&root));
}

// --- after the rebase: a rule whose target is a global playbook ------------

/// Writes the global playbook `id` (version 1.0.0, `current`) into the
/// config dir and approves its trust digest, as `apb trust approve
/// --scope global` does.
fn seed_global(cfg: &Path, id: &str, yaml: &str) {
    let vdir = cfg.join("playbooks").join(id).join("1.0.0");
    fs::create_dir_all(vdir.join("scripts")).unwrap();
    fs::write(vdir.join("playbook.yaml"), yaml).unwrap();
    fs::write(cfg.join("playbooks").join(id).join("current"), "1.0.0").unwrap();
    let reg = apb_core::registry::Registry::open_dir(cfg).unwrap();
    let digest = reg.load(id, Some("1.0.0")).unwrap().trust_digest().unwrap();
    apb_core::trust::TrustStore::load()
        .approve(&digest, id, apb_core::trust::OriginKind::LocallyApproved)
        .unwrap();
}

/// A rule whose playbook is global starts the global playbook in the
/// project, never the project playbook of the same id: its run names the
/// global origin and executes the global bytes, and its start key lives in
/// the global scope, so the next event of the same key answers the run it
/// started.
#[test]
fn a_global_rule_starts_the_global_playbook_and_keys_it_by_scope() {
    let id = "tt40";
    // The project playbook of the same id differs from the global one.
    let rig = Rig::new(id, &simple_pb(id, "description: project\n"));
    let root = rig.root();
    let global_yaml = simple_pb(id, "description: global\n");
    seed_global(&rig.cfg_dir(), id, &global_yaml);
    let doc = rule_doc("news", id, "{{body.entity}}", 5, true, "")
        .replace("scope: project", "scope: global");
    save_rule(&root, &doc);
    approve_via_tools(&root, "news");
    deliver("main", "e1", json!({ "entity": "acme/app" }));
    let run_id = match fire_newest(&root, "news") {
        FireOutcome::Started { run_id } => run_id,
        other => panic!("the global rule starts: {other:?}"),
    };
    let _reaper = DriverReaper::new(&root, &run_id);
    let run_dir = root.join(".apb/runs").join(&run_id);
    let events = apb_engine::event::read_all(&run_dir).unwrap();
    let origin = events.iter().find_map(|e| match &e.payload {
        apb_engine::event::EventPayload::RunProvenance { origin, .. } => origin.clone(),
        _ => None,
    });
    assert_eq!(
        origin.as_deref(),
        Some("global"),
        "the run names its origin"
    );
    assert_eq!(
        apb_engine::run_origin::run_scope(&run_dir),
        apb_engine::run_origin::RunScope::Global,
        "the run reads as a global run"
    );
    let executed = events.iter().find_map(|e| match &e.payload {
        apb_engine::event::EventPayload::RunStarted { definition, .. } => definition.clone(),
        _ => None,
    });
    let executed = executed.expect("a definition digest");
    assert!(
        apb_core::content::sha256_hex(global_yaml.as_bytes()).ends_with(&executed),
        "the run executed the global playbook ({executed})"
    );
    assert_eq!(launch_of(&root, &run_id).initiator, "trigger:news");
    // The same key again: the global scope's key answers the run it bound.
    deliver("main", "e2", json!({ "entity": "acme/app" }));
    match fire_newest(&root, "news") {
        FireOutcome::AlreadyStarted { run_id: again, .. } => assert_eq!(again, run_id),
        other => panic!("the same key answers already started: {other:?}"),
    }
    assert_eq!(started_runs(&root).len(), 1, "{:?}", started_runs(&root));
}
