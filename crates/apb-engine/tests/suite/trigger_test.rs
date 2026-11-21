//! The sweep through the engine (design D1 "One pass over one rule"): one
//! pass over one project's rules, each rule under its sweep lock, each new
//! event started at once through the fire, or recorded with its reason.
//! Two start stand-ins: a recording one for the journal and state
//! outcomes, and the real keyed start (the gate with `Initiator::Trigger`,
//! the start key's lookup, then the engine's own start path) for the tests
//! the design marks "(real start)".

use std::collections::VecDeque;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use apb_core::clock::Millis;
use apb_core::connector::inbox::Inbox;
use apb_core::registry::init_project;
use apb_core::scope::{Origin, PlaybookRef, Scope};
use apb_core::signed_store::{self, Unsigned};
use apb_core::start_key::{REQUEST_V, StartKey, StartKeyRequest, StartRequest, StartRequestFields};
use apb_core::trigger::{
    Approval, LogEntry, RulesFile, TriggerStateFile, Verdict, append, journal_ids,
    read as read_journal, remove_rule, rule_digest, set_enabled, set_rule, store_approval,
};
use apb_engine::EngineError;
use apb_engine::gate;
use apb_engine::gate::launch::{Initiator, LaunchContext, LaunchKind};
use apb_engine::scheduler::{RunOptions, run};
use apb_engine::start_key::{self, Lookup};
use apb_engine::trigger::{
    FireOutcome, FireRequest, RuleSweep, run_dirs_strict, starts_in_last_hour, sweep,
};
use serde_json::json;

use crate::common::{self, EnvVar};

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

/// A project at its own root, its own config dir, the github connector
/// installed and two accounts configured, the project registered
/// (`projects::bind_id`). Holds the env lock while it lives.
struct Rig {
    _cfg: tempfile::TempDir,
    dir: tempfile::TempDir,
    _vars: Vec<EnvVar>,
    _lock: std::sync::MutexGuard<'static, ()>,
    id: &'static str,
}

impl Rig {
    fn new(id: &'static str, yaml: &str) -> Self {
        let lock = common::env_lock();
        let cfg = tempfile::tempdir().unwrap();
        let dir = tempfile::tempdir().unwrap();
        let vars = vec![
            EnvVar::set("APB_CONFIG_DIR", cfg.path()),
            EnvVar::remove("APB_RUN_ID"),
            EnvVar::remove("APB_HOST_TASK"),
        ];
        apb_core::run_origin::ensure_key().unwrap();
        install_connector(cfg.path());
        init_project(dir.path()).unwrap();
        write_playbook(dir.path(), id, yaml);
        common::seed_main(dir.path());
        apb_core::projects::bind_id(dir.path()).unwrap();
        trust_current(dir.path(), id);
        Rig {
            _cfg: cfg,
            dir,
            _vars: vars,
            _lock: lock,
            id,
        }
    }

    fn root(&self) -> PathBuf {
        self.dir.path().to_path_buf()
    }

    fn cfg_dir(&self) -> PathBuf {
        self._cfg.path().to_path_buf()
    }
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

fn write_script(root: &Path, id: &str, name: &str, body: &str) {
    let path = root
        .join(".apb/playbooks")
        .join(id)
        .join("1.0.0/scripts")
        .join(name);
    fs::write(path, body).unwrap();
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

/// Writes the target's A or B variant and keeps it trusted (the flip the
/// permit tests use).
fn flip_target(root: &Path, id: &str, to_b: bool) {
    let body = if to_b {
        simple_pb(id).replace(&format!("name: {id}"), &format!("name: {id}-b"))
    } else {
        simple_pb(id)
    };
    write_playbook(root, id, &body);
    trust_current(root, id);
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

/// `start -> done`, nothing that needs an agent.
fn simple_pb(id: &str) -> String {
    format!(
        "schema: 2\nid: {id}\nname: {id}\nversion: 1.0.0\nnodes:\n  - {{ id: start, type: start }}\n  - {{ id: done, type: finish, outcome: success }}\nedges:\n  - {{ from: start, to: done }}\n"
    )
}

/// A rule document as a person writes it.
fn rule_doc(
    name: &str,
    id: &str,
    key: &str,
    max_per_hour: u32,
    external_data: bool,
    account: &str,
) -> String {
    format!(
        "name: {name}\nplaybook: {{ id: {id}, version: 1.0.0, scope: project }}\nsource:\n  inbox:\n    connector: github\n    account: {account}\n    types: [issues]\n    key: \"{key}\"\nlimits: {{ max_per_hour: {max_per_hour} }}\nbudget: {{ tokens: 100000, on_no_estimate: continue }}\nexternal_data: {external_data}\nrequire_ready: false\n"
    )
}

fn entry_of(root: &Path, name: &str) -> apb_core::trigger::RuleEntry {
    let file = signed_store::read::<RulesFile>(root, Scope::Project, "rules")
        .unwrap()
        .file;
    file.rules
        .iter()
        .find(|e| e.rule.name == name)
        .cloned()
        .unwrap()
}

fn incarnation_of(root: &Path, name: &str) -> String {
    entry_of(root, name).incarnation
}

fn approve(root: &Path, name: &str) {
    let entry = entry_of(root, name);
    let approval = Approval {
        rule_digest: rule_digest(&entry),
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

/// The binding the gate resolves for the rule's target right now: what a
/// person's approval is bound to (D1 "Approval" item 1).
fn binding_of(root: &Path, entry: &apb_core::trigger::RuleEntry) -> String {
    let wref = PlaybookRef {
        origin: Origin::Project { workspace_id: None },
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
    let launch = LaunchContext::new(Initiator::Trigger { by }, LaunchKind::Start)
        .with_external_data(entry.rule.external_data);
    let refusal = gate::check_run(root, &wref, false, false, &launch).unwrap_err();
    assert_eq!(
        refusal["policy"], "launch_requires_rule_approval",
        "the probe refusal: {refusal}"
    );
    refusal["binding"].as_str().unwrap().to_string()
}

/// Approves the rule for the binding the gate resolves now (the real keyed
/// start compares it in admission); returns the binding.
fn approve_real(root: &Path, name: &str) -> String {
    let entry = entry_of(root, name);
    let binding = binding_of(root, &entry);
    let approval = Approval {
        rule_digest: rule_digest(&entry),
        playbook_digest: "digest".to_string(),
        binding: binding.clone(),
        permit_digest: "permit".to_string(),
        launch_policy: "allow".to_string(),
        consent_nonce: None,
        approved_at: now(),
        approved_by: "test".to_string(),
    };
    store_approval(root, name, approval, Millis(now())).unwrap();
    binding
}

/// Appends one delivery of type `issues` to the account's inbox.
fn deliver(account: &str, provider_id: &str, body: serde_json::Value) {
    Inbox::open("github", account)
        .unwrap()
        .append_typed(provider_id, Some("issues"), &body)
        .unwrap();
}

fn seen_through(root: &Path, name: &str) -> u64 {
    let inc = incarnation_of(root, name);
    let state = signed_store::read::<TriggerStateFile>(root, Scope::Project, "state")
        .unwrap()
        .file;
    state
        .rules
        .iter()
        .find(|s| s.rule == name && s.incarnation == inc)
        .map(|s| s.seen_through)
        .unwrap_or(0)
}

fn journal(root: &Path, name: &str) -> Vec<LogEntry> {
    read_journal(root, &incarnation_of(root, name), 500)
}

fn ids_of(root: &Path, name: &str) -> std::collections::HashSet<String> {
    journal_ids(root, &incarnation_of(root, name)).unwrap()
}

fn run_dirs(root: &Path) -> Vec<String> {
    fs::read_dir(root.join(".apb/runs"))
        .map(|d| {
            d.filter_map(Result::ok)
                .filter(|e| e.path().is_dir())
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .collect()
        })
        .unwrap_or_default()
}

fn delete_state_entry(root: &Path, name: &str) {
    signed_store::transact(
        root,
        Scope::Project,
        "state",
        Unsigned::Refuse,
        |file: &mut TriggerStateFile| -> Result<((), bool), signed_store::StoreError> {
            let before = file.rules.len();
            file.rules.retain(|s| s.rule != name);
            Ok(((), file.rules.len() != before))
        },
    )
    .unwrap();
}

fn wait_for(what: &str, mut f: impl FnMut() -> Option<()>) {
    let start = Instant::now();
    loop {
        if f().is_some() {
            return;
        }
        assert!(
            start.elapsed() < Duration::from_secs(20),
            "timed out waiting for {what}"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
}

// --- the recording stand-in -------------------------------------------------

/// Answers the fire hands back, in order; the default is `Started` for a
/// made-up id. Records every request the sweep made.
#[derive(Default)]
struct Script {
    answers: Mutex<VecDeque<FireOutcome>>,
    seen: Mutex<Vec<(String, u64, String)>>,
}

impl Script {
    fn answer(&self, outcome: FireOutcome) {
        self.answers.lock().unwrap().push_back(outcome);
    }

    fn seen(&self) -> Vec<(String, u64, String)> {
        self.seen.lock().unwrap().clone()
    }

    /// The real fire's order: a key that names a run (`AlreadyStarted`)
    /// or a start still preparing (`Busy`) answers before any limit; any
    /// other answer asks `admit` first, and a limit holds the event without
    /// spending the scripted answer (the event is tried again).
    fn fire(&self, req: &FireRequest<'_>) -> FireOutcome {
        let mut answers = self.answers.lock().unwrap();
        let keyed = matches!(
            answers.front(),
            Some(FireOutcome::AlreadyStarted { .. } | FireOutcome::Busy)
        );
        if !keyed && let Err(code) = (req.admit)() {
            return FireOutcome::Limited { code };
        }
        self.seen.lock().unwrap().push((
            req.start_key.clone(),
            req.triggered_by.event_seq.unwrap_or(0),
            req.triggered_by.rule.clone(),
        ));
        answers.pop_front().unwrap_or_else(|| FireOutcome::Started {
            run_id: format!("run-{}", req.event.seq),
        })
    }
}

fn sweep_once(root: &Path, script: &Script) -> Vec<(String, Result<RuleSweep, EngineError>)> {
    sweep_once_at(root, now(), script)
}

fn sweep_once_at(
    root: &Path,
    at: u64,
    script: &Script,
) -> Vec<(String, Result<RuleSweep, EngineError>)> {
    sweep(root, &move || Millis(at), &|req: &FireRequest<'_>| {
        script.fire(req)
    })
}

// --- the real keyed start ---------------------------------------------------

/// The real keyed start: the start key's lookup, the gate with
/// `Initiator::Trigger` and the approval's binding, then the engine's own
/// start path with `RunOptions.start_key`. In-process, the pattern the
/// engine's own integration tests use (`scheduler::run`).
fn keyed_start(
    root: &Path,
    id: &str,
    version: &str,
    start_key_text: &str,
    by: &apb_core::launch::TriggeredBy,
    entry: &apb_core::trigger::RuleEntry,
    admit: &dyn Fn() -> Result<(), String>,
) -> FireOutcome {
    let external_data = entry.rule.external_data;
    let approved_binding = entry.approval.as_ref().map(|a| a.binding.as_str());
    let fields = StartRequestFields {
        v: REQUEST_V,
        version: Some(version.to_string()),
        execution: Some("cli".to_string()),
        supervision: "autonomous".to_string(),
        cache: "auto".to_string(),
        unattended: true,
        external_data,
        ..Default::default()
    };
    let Ok(key) = StartKey::parse(start_key_text) else {
        return FireOutcome::Refused {
            code: "key_invalid".to_string(),
            name: None,
        };
    };
    let skr = StartKeyRequest {
        key,
        reuse: false,
        request: StartRequest::capture(&fields).unwrap(),
    };
    match start_key::lookup(root, "project", id, &skr) {
        // A start still preparing under the key holds the event.
        Ok(Lookup::Started { bound: false, .. } | Lookup::Conflict { bound: false, .. }) => {
            return FireOutcome::Busy;
        }
        Ok(Lookup::Started { run_id, .. }) => {
            return FireOutcome::AlreadyStarted {
                run_id,
                conflict: false,
            };
        }
        Ok(Lookup::Conflict { run_id, .. }) => {
            return FireOutcome::AlreadyStarted {
                run_id,
                conflict: true,
            };
        }
        Err(EngineError::Conflict(m)) if m.starts_with("start_key_busy") => {
            return FireOutcome::Busy;
        }
        Err(EngineError::Conflict(m)) if m.starts_with("start_key_conflict") => {
            let run_id = m.split('`').nth(1).unwrap_or_default().to_string();
            return FireOutcome::AlreadyStarted {
                run_id,
                conflict: true,
            };
        }
        Err(_) => {
            return FireOutcome::Refused {
                code: "start_key_unreadable".to_string(),
                name: None,
            };
        }
        Ok(Lookup::Free) => {}
    }
    // The limits on new starts, after the key said the start is new.
    if let Err(code) = admit() {
        return FireOutcome::Limited { code };
    }
    let wref = PlaybookRef {
        origin: Origin::Project { workspace_id: None },
        id: id.to_string(),
        version: Some(version.to_string()),
    };
    // The approval's binding travels on the launch context; admission
    // compares it with the binding it resolves and refuses a mismatch as
    // `binding_changed` (D1 "Approval" item 5).
    let mut launch = LaunchContext::new(
        Initiator::Trigger {
            by: Box::new(by.clone()),
        },
        LaunchKind::Start,
    )
    .with_external_data(external_data);
    if let Some(binding) = approved_binding {
        launch = launch.with_approved_binding(binding.to_string());
    }
    let permit = match gate::check_run(root, &wref, false, false, &launch) {
        Ok(permit) => permit,
        Err(v) => {
            if let Some(binding) = apb_engine::gate::launch::binding_changed_of(&v) {
                return FireOutcome::BindingChanged { binding };
            }
            return FireOutcome::Refused {
                code: v["policy"].as_str().unwrap_or("refused").to_string(),
                name: None,
            };
        }
    };
    let mut opts = RunOptions {
        start_key: Some(skr),
        unattended: true,
        external_data,
        ..Default::default()
    };
    permit.apply(&mut opts);
    match run(root, id, Some(version), opts) {
        Ok(res) => FireOutcome::Started { run_id: res.run_id },
        Err(EngineError::AlreadyStarted(run_id)) => FireOutcome::AlreadyStarted {
            run_id,
            conflict: false,
        },
        Err(EngineError::Conflict(m)) if m.starts_with("start_key_busy") => FireOutcome::Busy,
        Err(EngineError::Conflict(m)) if m.starts_with("start_key_conflict") => {
            let run_id = m.split('`').nth(1).unwrap_or_default().to_string();
            FireOutcome::AlreadyStarted {
                run_id,
                conflict: true,
            }
        }
        Err(_) => FireOutcome::Failed {
            code: "start_failed".to_string(),
        },
    }
}

fn real_event_fire(req: &FireRequest<'_>, id: &str) -> FireOutcome {
    keyed_start(
        req.root,
        id,
        &req.entry.rule.playbook.version,
        &req.start_key,
        &req.triggered_by,
        req.entry,
        req.admit,
    )
}

fn sweep_real(rig: &Rig) -> Vec<(String, Result<RuleSweep, EngineError>)> {
    let root = rig.root();
    let id = rig.id;
    sweep(&root, &|| Millis(now()), &|req: &FireRequest<'_>| {
        real_event_fire(req, id)
    })
}

/// The crashed-pass stand-in: what a pass that died after the start (and,
/// for `with_line`, after its synced line) leaves behind. The key is the
/// one the pass would have built for `seq`.
fn crashed_pass_start(rig: &Rig, name: &str, seq: u64, with_line: bool) -> String {
    let root = rig.root();
    let entry = entry_of(&root, name);
    let inc = entry.incarnation.clone();
    let key = format!("trigger:{inc}:{seq}");
    let by = apb_core::launch::TriggeredBy {
        rule: name.to_string(),
        incarnation: inc.clone(),
        source: "inbox".to_string(),
        connector: Some("github".to_string()),
        account: Some("main".to_string()),
        event_seq: Some(seq),
    };
    let outcome = keyed_start(&root, rig.id, "1.0.0", &key, &by, &entry, &|| Ok(()));
    let FireOutcome::Started { run_id } = outcome else {
        panic!("the crashed pass had started: {outcome:?}");
    };
    if with_line {
        let mut line = LogEntry::for_event(&inc, seq, now(), Verdict::Started, None, None);
        line.run_id = Some(run_id.clone());
        let mut ids = journal_ids(&root, &inc).unwrap();
        append(&root, &inc, &mut ids, &[line]).unwrap();
    }
    run_id
}

/// Drops one seq from the account's events file, the way age retention
/// would have removed it.
fn drop_event(account: &str, seq: u64) {
    let dir = Inbox::open("github", account).unwrap().dir().to_path_buf();
    let kept: String = fs::read_to_string(dir.join("events.jsonl"))
        .unwrap()
        .lines()
        .filter(|l| !l.contains(&format!("\"seq\":{seq},")))
        .collect::<Vec<_>>()
        .join("\n");
    fs::write(dir.join("events.jsonl"), kept).unwrap();
}

// --- 1. a matching event starts once ---------------------------------------

#[test]
fn a_matching_event_starts_once() {
    let rig = Rig::new("tg1", &simple_pb("tg1"));
    let root = rig.root();
    set_rule(
        &root,
        &rule_doc("news", "tg1", "{{event.seq}}", 10, true, "main"),
        "test",
        Millis(now()),
    )
    .unwrap();
    approve(&root, "news");
    let script = Script::default();
    // The first pass positions at the empty head.
    let out = sweep_once(&root, &script);
    assert_eq!(out.len(), 1, "{out:?}");
    assert_eq!(out[0].0, "news");
    let report = out[0].1.as_ref().unwrap();
    assert_eq!(report.evaluated, 0);
    assert!(report.started.is_empty());
    // A matching event starts.
    deliver("main", "m1", json!({ "entity": "a" }));
    let out = sweep_once(&root, &script);
    let report = out[0].1.as_ref().unwrap();
    assert_eq!(report.evaluated, 1);
    assert_eq!(report.started, vec!["run-1".to_string()]);
    assert_eq!(seen_through(&root, "news"), 1);
    // The same event is not evaluated again.
    let out = sweep_once(&root, &script);
    let report = out[0].1.as_ref().unwrap();
    assert_eq!(report.evaluated, 0);
    assert_eq!(script.seen().len(), 1, "one fire: {:?}", script.seen());
    // The line is the started line with the run id.
    let lines = journal(&root, "news");
    let started: Vec<&LogEntry> = lines
        .iter()
        .filter(|l| l.verdict == Verdict::Started)
        .collect();
    assert_eq!(started.len(), 1);
    assert_eq!(
        started[0].id,
        format!("{}:1:started", incarnation_of(&root, "news"))
    );
    assert_eq!(started[0].run_id.as_deref(), Some("run-1"));
}

// --- 2. a resent delivery with a new id starts once (real start) ------------

#[test]
fn a_resent_delivery_with_a_new_id_starts_once() {
    let rig = Rig::new("tg2", &simple_pb("tg2"));
    let root = rig.root();
    set_rule(
        &root,
        &rule_doc("news", "tg2", "{{body.issue_id}}", 10, true, "main"),
        "test",
        Millis(now()),
    )
    .unwrap();
    approve_real(&root, "news");
    sweep_real(&rig);
    deliver("main", "m1", json!({ "issue_id": "778" }));
    let out = sweep_real(&rig);
    assert_eq!(out[0].1.as_ref().unwrap().started.len(), 1, "{out:?}");
    let first = out[0].1.as_ref().unwrap().started[0].clone();
    assert_eq!(run_dirs(&root).len(), 1);
    // The same delivery again under another provider id: the key answers
    // already started, one run.
    deliver("main", "m1-retry", json!({ "issue_id": "778" }));
    let out = sweep_real(&rig);
    let report = out[0].1.as_ref().unwrap();
    assert_eq!(report.started.len(), 0, "{report:?}");
    assert_eq!(report.evaluated, 1);
    assert_eq!(run_dirs(&root).len(), 1, "one run");
    let lines = journal(&root, "news");
    assert!(
        lines
            .iter()
            .any(|l| l.verdict == Verdict::AlreadyStarted
                && l.run_id.as_deref() == Some(first.as_str())),
        "{lines:?}"
    );
    assert_eq!(seen_through(&root, "news"), 2);
}

// --- 3. an entity key starts once per entity (real start) -------------------

#[test]
fn an_entity_key_starts_once_per_entity() {
    let rig = Rig::new("tg3", &simple_pb("tg3"));
    let root = rig.root();
    set_rule(
        &root,
        &rule_doc("news", "tg3", "{{body.entity}}", 10, true, "main"),
        "test",
        Millis(now()),
    )
    .unwrap();
    approve_real(&root, "news");
    sweep_real(&rig);
    deliver("main", "e1", json!({ "entity": "acme/app" }));
    deliver("main", "e2", json!({ "entity": "other/app" }));
    deliver("main", "e3", json!({ "entity": "acme/app" }));
    let out = sweep_real(&rig);
    let report = out[0].1.as_ref().unwrap();
    assert_eq!(report.started.len(), 2, "{report:?}");
    assert_eq!(run_dirs(&root).len(), 2);
    let lines = journal(&root, "news");
    assert_eq!(
        lines
            .iter()
            .filter(|l| l.verdict == Verdict::Started)
            .count(),
        2
    );
    assert_eq!(seen_through(&root, "news"), 3);
}

// --- 4. the hourly cap holds the position (real start) ----------------------

#[test]
fn the_hourly_cap_holds_the_position() {
    let rig = Rig::new("tg4", &simple_pb("tg4"));
    let root = rig.root();
    set_rule(
        &root,
        &rule_doc("news", "tg4", "{{event.seq}}", 1, true, "main"),
        "test",
        Millis(now()),
    )
    .unwrap();
    approve_real(&root, "news");
    sweep_real(&rig);
    deliver("main", "e1", json!({}));
    deliver("main", "e2", json!({}));
    let out = sweep_real(&rig);
    let report = out[0].1.as_ref().unwrap();
    assert_eq!(report.started.len(), 1, "{report:?}");
    assert_eq!(report.stopped.as_deref(), Some("rate_cap"));
    assert_eq!(
        seen_through(&root, "news"),
        1,
        "the position stays before the held event"
    );
    let inc = incarnation_of(&root, "news");
    let held = format!("{inc}:2:held:rate_cap");
    assert!(
        ids_of(&root, "news").contains(&held),
        "{:?}",
        journal(&root, "news")
    );

    // The journal emptied between passes changes nothing: the cap reads the
    // runs' launch records, not the journal.
    let log = root.join(".apb/trigger-state/log");
    fs::remove_file(log.join(format!("{inc}.jsonl"))).unwrap();
    let out = sweep_real(&rig);
    let report = out[0].1.as_ref().unwrap();
    assert_eq!(report.started.len(), 0, "{report:?}");
    assert_eq!(report.stopped.as_deref(), Some("rate_cap"));
    assert_eq!(seen_through(&root, "news"), 1);
    assert_eq!(run_dirs(&root).len(), 1, "still one run");
    // The held line was written once over the two passes.
    assert!(ids_of(&root, "news").contains(&held));
    assert_eq!(
        journal(&root, "news").len(),
        1,
        "{:?}",
        journal(&root, "news")
    );
}

// --- 5. the first pass after approval only positions ------------------------

#[test]
fn the_first_pass_after_approval_only_positions() {
    let rig = Rig::new("tg5", &simple_pb("tg5"));
    let root = rig.root();
    set_rule(
        &root,
        &rule_doc("news", "tg5", "{{event.seq}}", 10, true, "main"),
        "test",
        Millis(now()),
    )
    .unwrap();
    approve(&root, "news");
    // An event stored before the next pass.
    deliver("main", "m1", json!({}));
    let script = Script::default();
    let out = sweep_once(&root, &script);
    let report = out[0].1.as_ref().unwrap();
    assert_eq!(report.evaluated, 0, "the pass starts nothing");
    assert_eq!(report.started.len(), 0);
    assert_eq!(
        seen_through(&root, "news"),
        1,
        "positioned at the head it read"
    );
    let lines = journal(&root, "news");
    assert_eq!(lines.len(), 1);
    assert_eq!(lines[0].verdict, Verdict::Positioned);
    assert_eq!(lines[0].range, Some((1, 1)));
    assert!(script.seen().is_empty());
    // An event after it starts.
    deliver("main", "m2", json!({}));
    let out = sweep_once(&root, &script);
    let report = out[0].1.as_ref().unwrap();
    assert_eq!(report.started, vec!["run-2".to_string()], "{report:?}");
}

// --- 6. a removed state restarts nothing eight days later -------------------

#[test]
fn a_removed_state_restarts_nothing_eight_days_later() {
    let rig = Rig::new("tg6", &simple_pb("tg6"));
    let root = rig.root();
    set_rule(
        &root,
        &rule_doc("news", "tg6", "{{event.seq}}", 10, true, "main"),
        "test",
        Millis(now()),
    )
    .unwrap();
    approve(&root, "news");
    deliver("main", "m1", json!({}));
    let script = Script::default();
    sweep_once(&root, &script);
    assert_eq!(seen_through(&root, "news"), 1);
    // A person removes the state after it did not verify.
    delete_state_entry(&root, "news");
    let days = 8 * 24 * 60 * 60 * 1000u64;
    let out = sweep_once_at(&root, now() + days, &script);
    let report = out[0].1.as_ref().unwrap();
    assert_eq!(report.evaluated, 0, "the old event never starts");
    assert_eq!(seen_through(&root, "news"), 1);
    assert!(script.seen().is_empty());
    let lines = journal(&root, "news");
    assert!(
        lines
            .iter()
            .any(|l| l.verdict == Verdict::Positioned && l.range == Some((1, 1))),
        "{lines:?}"
    );
    // A new event starts normally.
    deliver("main", "m2", json!({}));
    let out = sweep_once(&root, &script);
    assert_eq!(
        out[0].1.as_ref().unwrap().started,
        vec!["run-2".to_string()]
    );
}

// --- 7. a second sweeper skips a held rule ----------------------------------

#[test]
fn a_second_sweeper_skips_a_held_rule() {
    let rig = Rig::new("tg7", &simple_pb("tg7"));
    let root = rig.root();
    set_rule(
        &root,
        &rule_doc("news", "tg7", "{{event.seq}}", 10, true, "main"),
        "test",
        Millis(now()),
    )
    .unwrap();
    approve(&root, "news");
    sweep_once(&root, &Script::default());
    deliver("main", "m1", json!({}));
    let trigger_dir = root.join(".apb/trigger-state");
    let held = apb_core::fsutil::hold_lock(&trigger_dir, "news.sweep.lock").unwrap();
    let script = Script::default();
    let out = sweep_once(&root, &script);
    assert!(out.is_empty(), "no entry for a skipped rule: {out:?}");
    assert!(script.seen().is_empty());
    assert_eq!(seen_through(&root, "news"), 0);
    drop(held);
    let out = sweep_once(&root, &script);
    assert_eq!(
        out[0].1.as_ref().unwrap().started,
        vec!["run-1".to_string()]
    );
}

// --- 8. a crash between start and line starts once (real start) -------------

#[test]
fn a_crash_between_start_and_line_starts_once() {
    let rig = Rig::new("tg8", &simple_pb("tg8"));
    let root = rig.root();
    set_rule(
        &root,
        &rule_doc("news", "tg8", "{{event.seq}}", 10, true, "main"),
        "test",
        Millis(now()),
    )
    .unwrap();
    approve_real(&root, "news");
    sweep_real(&rig);
    // The pass started the run for seq 1 and died before its line and its
    // transaction.
    deliver("main", "m1", json!({}));
    let run_id = crashed_pass_start(&rig, "news", 1, false);
    assert_eq!(run_dirs(&root).len(), 1);
    // The next pass evaluates the event again: the key answers
    // already_started, one run.
    let out = sweep_real(&rig);
    let report = out[0].1.as_ref().unwrap();
    assert_eq!(report.started.len(), 0, "{report:?}");
    assert_eq!(run_dirs(&root).len(), 1);
    let lines = journal(&root, "news");
    assert!(
        lines.iter().any(|l| l.verdict == Verdict::AlreadyStarted
            && l.run_id.as_deref() == Some(run_id.as_str())),
        "{lines:?}"
    );
    assert_eq!(seen_through(&root, "news"), 1);
}

// --- 9. a start lost to a crash and retention still counts (real start) -----

#[test]
fn a_start_lost_to_a_crash_and_retention_still_counts() {
    let rig = Rig::new("tg9", &simple_pb("tg9"));
    let root = rig.root();
    set_rule(
        &root,
        &rule_doc("news", "tg9", "{{event.seq}}", 1, true, "main"),
        "test",
        Millis(now()),
    )
    .unwrap();
    approve_real(&root, "news");
    sweep_real(&rig);
    // The pass started the run for seq 1 and died before its line; the
    // event was then lost to retention.
    deliver("main", "e1", json!({}));
    deliver("main", "e2", json!({}));
    crashed_pass_start(&rig, "news", 1, false);
    drop_event("main", 1);
    // The next pass: a gap for the lost event, then the cap counts the run
    // from its launch record and holds the second.
    let out = sweep_real(&rig);
    let report = out[0].1.as_ref().unwrap();
    assert_eq!(report.started.len(), 0, "{report:?}");
    assert_eq!(report.stopped.as_deref(), Some("rate_cap"));
    let lines = journal(&root, "news");
    assert!(
        lines
            .iter()
            .any(|l| l.verdict == Verdict::Gap && l.range == Some((1, 1))),
        "{lines:?}"
    );
    assert!(
        lines
            .iter()
            .any(|l| l.verdict == Verdict::Held && l.code.as_deref() == Some("rate_cap")),
        "{lines:?}"
    );
    assert_eq!(run_dirs(&root).len(), 1, "no second run");
}

// --- 10. a crash between line and transaction repeats the event once --------

#[test]
fn a_crash_between_line_and_transaction_repeats_the_event_once() {
    let rig = Rig::new("tg10", &simple_pb("tg10"));
    let root = rig.root();
    set_rule(
        &root,
        &rule_doc("news", "tg10", "{{event.seq}}", 10, true, "main"),
        "test",
        Millis(now()),
    )
    .unwrap();
    approve_real(&root, "news");
    sweep_real(&rig);
    // The pass started the run, synced its line and died before the state
    // transaction moved the position.
    deliver("main", "m1", json!({}));
    let run_id = crashed_pass_start(&rig, "news", 1, true);
    assert_eq!(seen_through(&root, "news"), 0, "the position never moved");
    // The next pass repeats the event: the key answers already_started and
    // the started line is not appended twice.
    let out = sweep_real(&rig);
    let report = out[0].1.as_ref().unwrap();
    assert_eq!(report.started.len(), 0, "{report:?}");
    assert_eq!(run_dirs(&root).len(), 1);
    assert_eq!(seen_through(&root, "news"), 1);
    let inc = incarnation_of(&root, "news");
    let ids = ids_of(&root, "news");
    assert!(ids.contains(&format!("{inc}:1:started")), "{ids:?}");
    assert!(ids.contains(&format!("{inc}:1:already_started")), "{ids:?}");
    // The deduping reader folds the answer into the start it names.
    let lines = journal(&root, "news");
    let started = lines
        .iter()
        .find(|l| l.verdict == Verdict::Started && l.seq == Some(1))
        .unwrap();
    assert_eq!(started.run_id.as_deref(), Some(run_id.as_str()));
}

// --- 11. a journal write failure leaves the position ------------------------

#[test]
fn a_journal_write_failure_leaves_the_position() {
    let rig = Rig::new("tg11", &simple_pb("tg11"));
    let root = rig.root();
    set_rule(
        &root,
        &rule_doc("news", "tg11", "{{event.seq}}", 10, true, "main"),
        "test",
        Millis(now()),
    )
    .unwrap();
    approve(&root, "news");
    let script = Script::default();
    sweep_once(&root, &script);
    // The journal file cannot be written.
    let inc = incarnation_of(&root, "news");
    let log = root.join(".apb/trigger-state/log");
    fs::remove_file(log.join(format!("{inc}.jsonl"))).unwrap();
    fs::create_dir(log.join(format!("{inc}.jsonl"))).unwrap();
    deliver("main", "m1", json!({}));
    // A file no reader reads stops the rule for the pass (round 2, S4):
    // the refusal is the pass's answer, nothing is evaluated or started,
    // and the position stays where it was.
    let out = sweep_once(&root, &script);
    let err = out[0].1.as_ref().unwrap_err();
    assert!(err.to_string().contains("journal_refused"), "{err}");
    // One sentence (W4): the problem named once, then the remedy, with the
    // path a person can delete.
    let sentence = err.to_string();
    assert!(
        sentence.contains("; remove the rule and save it again, or delete .apb/trigger-state/log/"),
        "{sentence}"
    );
    assert!(
        !sentence.contains("cannot be read: the trigger journal of"),
        "the problem is named once: {sentence}"
    );
    assert_eq!(seen_through(&root, "news"), 0, "the position stays");
    // Repaired: the next pass starts the event.
    fs::remove_dir(log.join(format!("{inc}.jsonl"))).unwrap();
    let out = sweep_once(&root, &script);
    assert_eq!(
        out[0].1.as_ref().unwrap().started,
        vec!["run-1".to_string()]
    );
    assert_eq!(seen_through(&root, "news"), 1);
}

// --- 11a. an append that cannot open the journal stops as journal_write -----

/// The journal reads (0444) and the read-write open fails: the started
/// event's line is never durable, so the pass answers `journal_write`, the
/// position stays and no later event of the pass is evaluated.
#[cfg(unix)]
#[test]
fn a_write_failure_at_a_started_event_reports_journal_write() {
    use std::os::unix::fs::PermissionsExt;
    let rig = Rig::new("tg11a", &simple_pb("tg11a"));
    let root = rig.root();
    set_rule(
        &root,
        &rule_doc("news", "tg11a", "{{event.seq}}", 10, true, "main"),
        "test",
        Millis(now()),
    )
    .unwrap();
    approve(&root, "news");
    let script = Script::default();
    sweep_once(&root, &script);
    deliver("main", "m1", json!({}));
    deliver("main", "m2", json!({}));
    let inc = incarnation_of(&root, "news");
    let log = root
        .join(".apb/trigger-state/log")
        .join(format!("{inc}.jsonl"));
    fs::set_permissions(&log, fs::Permissions::from_mode(0o444)).unwrap();
    let out = sweep_once(&root, &script);
    let report = out[0].1.as_ref().unwrap();
    assert_eq!(report.started.len(), 0, "{report:?}");
    assert_eq!(
        report.stopped.as_deref(),
        Some("journal_write"),
        "{report:?}"
    );
    assert_eq!(
        script.seen().len(),
        1,
        "no later event is fired: {:?}",
        script.seen()
    );
    assert_eq!(seen_through(&root, "news"), 0, "the position stays");
    // Nothing reached the journal: the next pass reads the ids, starts both
    // events in order and moves the position past them.
    fs::set_permissions(&log, fs::Permissions::from_mode(0o644)).unwrap();
    let out = sweep_once(&root, &script);
    let report = out[0].1.as_ref().unwrap();
    assert_eq!(
        report.started,
        vec!["run-1".to_string(), "run-2".to_string()]
    );
    assert_eq!(seen_through(&root, "news"), 2);
}

/// The same failure at a held event: `start_key_busy` is not reported when
/// the hold line could not be written; the pass answers `journal_write`,
/// the position stays and the journal keeps no held line.
#[cfg(unix)]
#[test]
fn a_write_failure_at_a_held_event_reports_journal_write() {
    use std::os::unix::fs::PermissionsExt;
    let rig = Rig::new("tg11b", &simple_pb("tg11b"));
    let root = rig.root();
    set_rule(
        &root,
        &rule_doc("news", "tg11b", "{{event.seq}}", 10, true, "main"),
        "test",
        Millis(now()),
    )
    .unwrap();
    approve(&root, "news");
    sweep_once(&root, &Script::default());
    deliver("main", "m1", json!({}));
    let inc = incarnation_of(&root, "news");
    let log = root
        .join(".apb/trigger-state/log")
        .join(format!("{inc}.jsonl"));
    fs::set_permissions(&log, fs::Permissions::from_mode(0o444)).unwrap();
    let script = Script::default();
    script.answer(FireOutcome::Busy);
    let out = sweep_once(&root, &script);
    let report = out[0].1.as_ref().unwrap();
    assert_eq!(report.started.len(), 0, "{report:?}");
    assert_eq!(
        report.stopped.as_deref(),
        Some("journal_write"),
        "start_key_busy is not reported when the hold line could not be written: {report:?}"
    );
    assert_eq!(seen_through(&root, "news"), 0, "the position stays");
    assert!(
        journal(&root, "news")
            .iter()
            .all(|l| l.verdict != Verdict::Held),
        "{:?}",
        journal(&root, "news")
    );
    // The journal writable again: the same event is held, with its line.
    fs::set_permissions(&log, fs::Permissions::from_mode(0o644)).unwrap();
    let script = Script::default();
    script.answer(FireOutcome::Busy);
    let out = sweep_once(&root, &script);
    let report = out[0].1.as_ref().unwrap();
    assert_eq!(
        report.stopped.as_deref(),
        Some("start_key_busy"),
        "{report:?}"
    );
    assert!(
        ids_of(&root, "news").contains(&format!("{inc}:1:held:start_key_busy")),
        "{:?}",
        journal(&root, "news")
    );
    assert_eq!(seen_through(&root, "news"), 0);
}

/// The same failure at a rate-limited event: the hold line of the cap
/// could not be written, so the pass answers `journal_write` and not the
/// cap's own code; the position stays and the journal keeps no held line.
#[cfg(unix)]
#[test]
fn a_write_failure_at_a_rate_capped_event_reports_journal_write() {
    use std::os::unix::fs::PermissionsExt;
    let rig = Rig::new("tg11c", &simple_pb("tg11c"));
    let root = rig.root();
    set_rule(
        &root,
        &rule_doc("news", "tg11c", "{{event.seq}}", 10, true, "main"),
        "test",
        Millis(now()),
    )
    .unwrap();
    approve(&root, "news");
    sweep_once(&root, &Script::default());
    deliver("main", "m1", json!({}));
    let inc = incarnation_of(&root, "news");
    let log = root
        .join(".apb/trigger-state/log")
        .join(format!("{inc}.jsonl"));
    fs::set_permissions(&log, fs::Permissions::from_mode(0o444)).unwrap();
    let script = Script::default();
    script.answer(FireOutcome::Limited {
        code: "rate_cap".to_string(),
    });
    let out = sweep_once(&root, &script);
    let report = out[0].1.as_ref().unwrap();
    assert_eq!(report.started.len(), 0, "{report:?}");
    assert_eq!(
        report.stopped.as_deref(),
        Some("journal_write"),
        "the cap is not reported when the hold line could not be written: {report:?}"
    );
    assert_eq!(seen_through(&root, "news"), 0, "the position stays");
    assert!(
        journal(&root, "news")
            .iter()
            .all(|l| l.verdict != Verdict::Held),
        "{:?}",
        journal(&root, "news")
    );
}

// --- 12. a busy start key is tried again next pass --------------------------

#[test]
fn a_busy_start_key_is_tried_again_next_pass() {
    let rig = Rig::new("tg12", &simple_pb("tg12"));
    let root = rig.root();
    set_rule(
        &root,
        &rule_doc("news", "tg12", "{{event.seq}}", 10, true, "main"),
        "test",
        Millis(now()),
    )
    .unwrap();
    approve(&root, "news");
    sweep_once(&root, &Script::default());
    deliver("main", "m1", json!({}));
    let script = Script::default();
    script.answer(FireOutcome::Busy);
    let out = sweep_once(&root, &script);
    let report = out[0].1.as_ref().unwrap();
    assert_eq!(report.started.len(), 0);
    assert_eq!(report.stopped.as_deref(), Some("start_key_busy"));
    assert_eq!(
        seen_through(&root, "news"),
        0,
        "the position stays before the event"
    );
    let inc = incarnation_of(&root, "news");
    assert!(ids_of(&root, "news").contains(&format!("{inc}:1:held:start_key_busy")));
    assert_eq!(
        journal(&root, "news")
            .iter()
            .filter(|l| l.verdict == Verdict::Held)
            .count(),
        1
    );
    // The next pass tries again and starts.
    let out = sweep_once(&root, &script);
    let report = out[0].1.as_ref().unwrap();
    assert_eq!(report.started, vec!["run-1".to_string()], "{report:?}");
    assert_eq!(report.stopped, None);
    assert_eq!(seen_through(&root, "news"), 1);
}

// --- 13. a refusal is the event's verdict -----------------------------------

#[test]
fn a_refusal_is_the_events_verdict() {
    let rig = Rig::new("tg13", &simple_pb("tg13"));
    let root = rig.root();
    set_rule(
        &root,
        &rule_doc("news", "tg13", "{{event.seq}}", 10, true, "main"),
        "test",
        Millis(now()),
    )
    .unwrap();
    approve(&root, "news");
    sweep_once(&root, &Script::default());
    deliver("main", "e1", json!({}));
    deliver("main", "e2", json!({}));
    let script = Script::default();
    script.answer(FireOutcome::Refused {
        code: "probation_required".to_string(),
        name: None,
    });
    let out = sweep_once(&root, &script);
    let report = out[0].1.as_ref().unwrap();
    assert_eq!(
        report.started,
        vec!["run-2".to_string()],
        "the next event starts: {report:?}"
    );
    assert_eq!(report.stopped, None);
    assert_eq!(seen_through(&root, "news"), 2);
    let lines = journal(&root, "news");
    let refused = lines
        .iter()
        .find(|l| l.verdict == Verdict::Refused && l.seq == Some(1))
        .unwrap();
    assert_eq!(refused.code.as_deref(), Some("probation_required"));
    assert_eq!(refused.run_id, None);
}

// --- 14. a busy concurrency key leaves no value in the journal --------------

#[test]
fn a_busy_concurrency_key_leaves_no_value_in_the_journal() {
    let rig = Rig::new("tg14", &simple_pb("tg14"));
    let root = rig.root();
    set_rule(
        &root,
        &rule_doc("news", "tg14", "{{body.entity}}", 10, true, "main"),
        "test",
        Millis(now()),
    )
    .unwrap();
    approve(&root, "news");
    sweep_once(&root, &Script::default());
    let secret = "SECRET-VALUE-9f2a";
    deliver("main", "e1", json!({ "entity": secret }));
    let script = Script::default();
    // The engine refusal whose sentence renders the key's display.
    script.answer(FireOutcome::Refused {
        code: "concurrency_busy".to_string(),
        name: None,
    });
    let out = sweep_once(&root, &script);
    let report = out[0].1.as_ref().unwrap();
    assert_eq!(report.stopped, None);
    assert_eq!(seen_through(&root, "news"), 1);
    let inc = incarnation_of(&root, "news");
    let raw = fs::read_to_string(
        root.join(".apb/trigger-state/log")
            .join(format!("{inc}.jsonl")),
    )
    .unwrap();
    assert!(!raw.contains(secret), "the journal holds a value: {raw}");
    // The pass's own line is the last one, and it holds the code only.
    let last = raw.lines().last().unwrap();
    let entry: LogEntry = serde_json::from_str(last).unwrap();
    assert_eq!(entry.verdict, Verdict::Refused);
    assert_eq!(entry.code.as_deref(), Some("concurrency_busy"));
    assert_eq!(entry.run_id, None);
    assert_eq!(entry.name, None);
    assert_eq!(entry.binding, None);
    assert!(entry.conditions.is_empty() || entry.verdict == Verdict::Refused);
}

// --- 15. an unverified state starts nothing ---------------------------------

#[test]
fn an_unverified_state_starts_nothing() {
    let rig = Rig::new("tg15", &simple_pb("tg15"));
    let root = rig.root();
    set_rule(
        &root,
        &rule_doc("news", "tg15", "{{event.seq}}", 10, true, "main"),
        "test",
        Millis(now()),
    )
    .unwrap();
    approve(&root, "news");
    let script = Script::default();
    sweep_once(&root, &script);
    deliver("main", "m1", json!({}));
    // A hand edited state file does not verify.
    let path = root.join(".apb/trigger-state/project/state.json");
    let body = fs::read_to_string(&path)
        .unwrap()
        .replace("seen_through", "seen_through_x");
    fs::write(&path, body).unwrap();
    let out = sweep_once(&root, &script);
    assert_eq!(out.len(), 1);
    assert_eq!(out[0].0, "*");
    assert!(out[0].1.is_err(), "{out:?}");
    assert!(script.seen().is_empty());
    assert_eq!(run_dirs(&root).len(), 0);
    // The rules file is untouched: the approval stands.
    assert!(entry_of(&root, "news").approval.is_some());
}

// --- 16. a gap is logged once and the pass goes on --------------------------

#[test]
fn a_gap_is_logged_once_and_the_pass_goes_on() {
    let rig = Rig::new("tg16", &simple_pb("tg16"));
    let root = rig.root();
    set_rule(
        &root,
        &rule_doc("news", "tg16", "{{event.seq}}", 10, true, "main"),
        "test",
        Millis(now()),
    )
    .unwrap();
    approve(&root, "news");
    let script = Script::default();
    sweep_once(&root, &script);
    deliver("main", "e1", json!({}));
    deliver("main", "e2", json!({}));
    deliver("main", "e3", json!({}));
    // Retention removed the event the rule had not seen.
    drop_event("main", 1);
    let out = sweep_once(&root, &script);
    let report = out[0].1.as_ref().unwrap();
    assert_eq!(
        report.started,
        vec!["run-2".to_string(), "run-3".to_string()]
    );
    let lines = journal(&root, "news");
    assert_eq!(
        lines.iter().filter(|l| l.verdict == Verdict::Gap).count(),
        1
    );
    assert_eq!(
        lines
            .iter()
            .find(|l| l.verdict == Verdict::Gap)
            .unwrap()
            .range,
        Some((1, 1))
    );
    assert_eq!(seen_through(&root, "news"), 3);
    // The next pass logs no second gap.
    let out = sweep_once(&root, &script);
    assert_eq!(out[0].1.as_ref().unwrap().evaluated, 0);
    assert_eq!(
        journal(&root, "news")
            .iter()
            .filter(|l| l.verdict == Verdict::Gap)
            .count(),
        1
    );
}

// --- 17. a rule registers no consumer ---------------------------------------

#[test]
fn a_rule_registers_no_consumer() {
    let rig = Rig::new("tg17", &simple_pb("tg17"));
    let root = rig.root();
    set_rule(
        &root,
        &rule_doc("news", "tg17", "{{event.seq}}", 10, true, "main"),
        "test",
        Millis(now()),
    )
    .unwrap();
    approve(&root, "news");
    let script = Script::default();
    sweep_once(&root, &script);
    deliver("main", "m1", json!({}));
    sweep_once(&root, &script);
    assert!(!script.seen().is_empty());
    // No cursors file, no consumer named anywhere in the account dir.
    let account = Inbox::open("github", "main").unwrap().dir().to_path_buf();
    assert!(
        !account.join("cursors.yaml").exists(),
        "the rule registered a consumer"
    );
    for entry in fs::read_dir(&account).unwrap().filter_map(Result::ok) {
        let body = fs::read_to_string(entry.path()).unwrap_or_default();
        assert!(
            !body.contains("consumer"),
            "{}: {body}",
            entry.path().display()
        );
    }
}

// --- 18. an unapproved rule starts nothing ----------------------------------

#[test]
fn an_unapproved_rule_starts_nothing() {
    let rig = Rig::new("tg18", &simple_pb("tg18"));
    let root = rig.root();
    set_rule(
        &root,
        &rule_doc("news", "tg18", "{{event.seq}}", 10, true, "main"),
        "test",
        Millis(now()),
    )
    .unwrap();
    deliver("main", "m1", json!({}));
    let script = Script::default();
    let out = sweep_once(&root, &script);
    assert!(out.is_empty(), "{out:?}");
    assert!(script.seen().is_empty());
    assert_eq!(run_dirs(&root).len(), 0);
    // A stale approval, one of another digest, is no approval either.
    approve(&root, "news");
    signed_store::transact(
        &root,
        Scope::Project,
        "rules",
        Unsigned::Refuse,
        |file: &mut RulesFile| -> Result<((), bool), signed_store::StoreError> {
            let mut changed = false;
            if let Some(entry) = file.rules.iter_mut().find(|e| e.rule.name == "news")
                && let Some(approval) = entry.approval.as_mut()
            {
                approval.rule_digest = "stale".to_string();
                changed = true;
            }
            Ok(((), changed))
        },
    )
    .unwrap();
    let out = sweep_once(&root, &script);
    assert!(out.is_empty(), "{out:?}");
    assert!(script.seen().is_empty());
}

// --- 19. a reenabled rule starts nothing that arrived while disabled --------

#[test]
fn a_reenabled_rule_starts_nothing_that_arrived_while_disabled() {
    let rig = Rig::new("tg19", &simple_pb("tg19"));
    let root = rig.root();
    set_rule(
        &root,
        &rule_doc("news", "tg19", "{{event.seq}}", 10, true, "main"),
        "test",
        Millis(now()),
    )
    .unwrap();
    approve(&root, "news");
    let script = Script::default();
    sweep_once(&root, &script);
    set_enabled(&root, "news", false, "test", Millis(now())).unwrap();
    deliver("main", "m1", json!({}));
    let out = sweep_once(&root, &script);
    assert!(out.is_empty(), "a disabled rule is not swept: {out:?}");
    // Enabling deletes the state: the first pass positions at the head it
    // reads, skipping what arrived while disabled.
    set_enabled(&root, "news", true, "test", Millis(now())).unwrap();
    let out = sweep_once(&root, &script);
    let report = out[0].1.as_ref().unwrap();
    assert_eq!(report.evaluated, 0);
    assert_eq!(seen_through(&root, "news"), 1);
    assert!(script.seen().is_empty());
    // A new event starts.
    deliver("main", "m2", json!({}));
    let out = sweep_once(&root, &script);
    assert_eq!(
        out[0].1.as_ref().unwrap().started,
        vec!["run-2".to_string()]
    );
}

// --- 20. a source change never reuses a line id -----------------------------

#[test]
fn a_source_change_never_reuses_a_line_id() {
    let rig = Rig::new("tg20", &simple_pb("tg20"));
    let root = rig.root();
    set_rule(
        &root,
        &rule_doc("news", "tg20", "{{event.seq}}", 10, true, "main"),
        "test",
        Millis(now()),
    )
    .unwrap();
    approve(&root, "news");
    let script = Script::default();
    sweep_once(&root, &script);
    deliver("main", "m1", json!({}));
    sweep_once(&root, &script);
    let first_inc = incarnation_of(&root, "news");
    let first_ids = ids_of(&root, "news");
    assert!(first_ids.iter().all(|id| id.starts_with(&first_inc)));
    // The source changes: a new incarnation and a dropped approval.
    set_rule(
        &root,
        &rule_doc("news", "tg20", "{{event.seq}}", 10, true, "other"),
        "test",
        Millis(now()),
    )
    .unwrap();
    let second_inc = incarnation_of(&root, "news");
    assert_ne!(first_inc, second_inc);
    assert!(entry_of(&root, "news").approval.is_none());
    // The old journal stays, inert, with its line and id.
    let old_path = root
        .join(".apb/trigger-state/log")
        .join(format!("{first_inc}.jsonl"));
    assert!(old_path.is_file());
    let old = fs::read_to_string(&old_path).unwrap();
    assert!(old.contains(&format!("{first_inc}:1:started")), "{old}");
    // Approved again, the new incarnation's pass positions (the state was
    // deleted) and a new event starts, under ids of the new incarnation.
    approve(&root, "news");
    let out = sweep_once(&root, &script);
    let report = out[0].1.as_ref().unwrap();
    assert_eq!(report.evaluated, 0, "{report:?}");
    assert_eq!(
        seen_through(&root, "news"),
        0,
        "positioned at the new source's head"
    );
    deliver("other", "o1", json!({}));
    let out = sweep_once(&root, &script);
    assert_eq!(
        out[0].1.as_ref().unwrap().started,
        vec!["run-1".to_string()],
        "{out:?}"
    );
    let second_ids = ids_of(&root, "news");
    assert!(second_ids.iter().all(|id| id.starts_with(&second_inc)));
    assert!(
        second_ids.iter().all(|id| !first_ids.contains(id)),
        "{second_ids:?} vs {first_ids:?}"
    );
}

// --- 21. a changed permit revokes the approval ------------------------------

#[test]
fn a_changed_permit_revokes_the_approval() {
    for (policy, id) in [("allow", "tg21a"), ("ask", "tg21b")] {
        let rig = Rig::new(id, &simple_pb(id));
        let root = rig.root();
        fs::write(
            rig.cfg_dir().join("policy.yaml"),
            format!("launch:\n  agent: {policy}\n"),
        )
        .unwrap();
        set_rule(
            &root,
            &rule_doc("news", id, "{{event.seq}}", 10, true, "main"),
            "test",
            Millis(now()),
        )
        .unwrap();
        let binding_a = approve_real(&root, "news");
        sweep_real(&rig);
        // A starts.
        deliver("main", "e1", json!({}));
        let out = sweep_real(&rig);
        assert_eq!(
            out[0].1.as_ref().unwrap().started.len(),
            1,
            "{policy}: {out:?}"
        );
        // The target's permit changes (the version's content edited in
        // place): B.
        flip_target(&root, id, true);
        deliver("main", "e2", json!({}));
        let out = sweep_real(&rig);
        let report = out[0].1.as_ref().unwrap();
        assert_eq!(report.started.len(), 0, "{policy}: {report:?}");
        assert_eq!(report.stopped, None);
        assert_eq!(
            seen_through(&root, "news"),
            1,
            "the position stays before the event"
        );
        assert!(
            entry_of(&root, "news").approval.is_none(),
            "{policy}: the approval is cleared"
        );
        let inc = incarnation_of(&root, "news");
        let lines = journal(&root, "news");
        let changed = lines
            .iter()
            .find(|l| l.code.as_deref() == Some("binding_changed"))
            .expect("one binding_changed line");
        assert_eq!(changed.seq, Some(2));
        assert!(changed.binding.is_some());
        assert_eq!(changed.approved.as_deref(), Some(binding_a.as_str()));
        assert!(
            lines.iter().any(|l| l.id
                == format!(
                    "{inc}:2:binding_changed:{}",
                    changed.binding.as_deref().unwrap()
                )),
            "{lines:?}"
        );
        // Across a restart (a fresh pass), back to A: the approval does not
        // come back and nothing starts.
        flip_target(&root, id, false);
        deliver("main", "e3", json!({}));
        let out = sweep_real(&rig);
        assert!(out.is_empty(), "an unapproved rule is not swept: {out:?}");
        assert!(entry_of(&root, "news").approval.is_none());
        assert_eq!(run_dirs(&root).len(), 1, "still one run");
        // The rig holds the process env lock; release it before the next
        // iteration's rig takes it.
        drop(rig);
    }
}

// --- 21b. a restored binding with no start keeps the approval ---------------

#[test]
fn a_restored_binding_with_no_start_keeps_the_approval() {
    let rig = Rig::new("tg21c", &simple_pb("tg21c"));
    let root = rig.root();
    set_rule(
        &root,
        &rule_doc("news", "tg21c", "{{body.entity}}", 10, true, "main"),
        "test",
        Millis(now()),
    )
    .unwrap();
    approve_real(&root, "news");
    sweep_real(&rig);
    // A starts: one run for entity a.
    deliver("main", "e1", json!({ "entity": "a" }));
    let out = sweep_real(&rig);
    assert_eq!(out[0].1.as_ref().unwrap().started.len(), 1);
    // B: a matching event the start key answers already_started. No start
    // is attempted, so nothing is revoked.
    flip_target(&root, "tg21c", true);
    deliver("main", "e2", json!({ "entity": "a" }));
    let out = sweep_real(&rig);
    let report = out[0].1.as_ref().unwrap();
    assert_eq!(report.started.len(), 0, "{report:?}");
    assert_eq!(report.stopped, None, "{report:?}");
    assert!(
        entry_of(&root, "news").approval.is_some(),
        "nothing was revoked"
    );
    assert!(
        !journal(&root, "news")
            .iter()
            .any(|l| l.code.as_deref() == Some("binding_changed")),
        "{:?}",
        journal(&root, "news")
    );
    // Restored to A, the next new key starts under the standing approval.
    flip_target(&root, "tg21c", false);
    deliver("main", "e3", json!({ "entity": "b" }));
    let out = sweep_real(&rig);
    assert_eq!(out[0].1.as_ref().unwrap().started.len(), 1, "{out:?}");
    assert_eq!(run_dirs(&root).len(), 2);
}

// --- 21a. a billing deny revokes the approval -------------------------------

#[test]
fn a_billing_deny_revokes_the_approval() {
    let rig = Rig::new("tg22", &simple_pb("tg22"));
    let root = rig.root();
    // The machine policy allows billed use (production configuration).
    let policy = rig.cfg_dir().join("policy.yaml");
    fs::write(
        &policy,
        "billing:\n  billed: allow\n  unknown_basis: allow\n",
    )
    .unwrap();
    set_rule(
        &root,
        &rule_doc("news", "tg22", "{{event.seq}}", 10, true, "main"),
        "test",
        Millis(now()),
    )
    .unwrap();
    approve_real(&root, "news");
    sweep_real(&rig);
    deliver("main", "e1", json!({}));
    let out = sweep_real(&rig);
    assert_eq!(out[0].1.as_ref().unwrap().started.len(), 1, "{out:?}");
    // The billing decision switches to deny: the next event starts nothing
    // and the approval is cleared.
    fs::write(&policy, "billing:\n  billed: deny\n  unknown_basis: deny\n").unwrap();
    deliver("main", "e2", json!({}));
    let out = sweep_real(&rig);
    let report = out[0].1.as_ref().unwrap();
    assert_eq!(report.started.len(), 0, "{report:?}");
    assert!(
        entry_of(&root, "news").approval.is_none(),
        "the approval is cleared"
    );
    assert!(
        journal(&root, "news")
            .iter()
            .any(|l| l.code.as_deref() == Some("binding_changed")),
        "{:?}",
        journal(&root, "news")
    );
    // Back to allow: the approval does not come back.
    fs::write(
        &policy,
        "billing:\n  billed: allow\n  unknown_basis: allow\n",
    )
    .unwrap();
    deliver("main", "e3", json!({}));
    let out = sweep_real(&rig);
    assert!(out.is_empty(), "an unapproved rule is not swept: {out:?}");
    assert!(entry_of(&root, "news").approval.is_none());
}

// --- 22. a crash before the revocation is completed or moot -----------------

#[test]
fn a_crash_before_the_revocation_is_completed_or_moot() {
    // Completed: the line of the crashed pass is there, the revocation is
    // not; the next pass compares again, writes no second line and clears
    // the approval.
    let rig = Rig::new("tg23", &simple_pb("tg23"));
    let root = rig.root();
    set_rule(
        &root,
        &rule_doc("news", "tg23", "{{event.seq}}", 10, true, "main"),
        "test",
        Millis(now()),
    )
    .unwrap();
    approve_real(&root, "news");
    sweep_real(&rig);
    // The target changes to B; the crashed pass wrote the line and died
    // before the revocation.
    flip_target(&root, "tg23", true);
    deliver("main", "e1", json!({}));
    let entry = entry_of(&root, "news");
    let resolved = binding_of(&root, &entry);
    let inc = entry.incarnation.clone();
    let line = LogEntry::for_binding_changed(&inc, 1, now(), &resolved, "binding");
    let mut ids = journal_ids(&root, &inc).unwrap();
    append(&root, &inc, &mut ids, &[line]).unwrap();
    let out = sweep_real(&rig);
    let report = out[0].1.as_ref().unwrap();
    assert_eq!(report.started.len(), 0, "{report:?}");
    assert!(
        entry_of(&root, "news").approval.is_none(),
        "the revocation completed"
    );
    assert_eq!(
        journal(&root, "news")
            .iter()
            .filter(|l| l.code.as_deref() == Some("binding_changed"))
            .count(),
        1,
        "the line is not appended twice: {:?}",
        journal(&root, "news")
    );

    // Moot: the binding is restored before the next pass, the approval
    // stands and the event starts. The rig holds the process env lock;
    // release it before the second rig of this test takes it.
    drop(rig);
    let rig = Rig::new("tg23b", &simple_pb("tg23b"));
    let root = rig.root();
    set_rule(
        &root,
        &rule_doc("news", "tg23b", "{{event.seq}}", 10, true, "main"),
        "test",
        Millis(now()),
    )
    .unwrap();
    approve_real(&root, "news");
    sweep_real(&rig);
    flip_target(&root, "tg23b", true);
    deliver("main", "e1", json!({}));
    let entry = entry_of(&root, "news");
    let resolved = binding_of(&root, &entry);
    let inc = entry.incarnation.clone();
    let line = LogEntry::for_binding_changed(&inc, 1, now(), &resolved, "binding");
    let mut ids = journal_ids(&root, &inc).unwrap();
    append(&root, &inc, &mut ids, &[line]).unwrap();
    flip_target(&root, "tg23b", false);
    let out = sweep_real(&rig);
    let report = out[0].1.as_ref().unwrap();
    assert_eq!(report.started.len(), 1, "{report:?}");
    assert!(
        entry_of(&root, "news").approval.is_some(),
        "the approval stands"
    );
    assert_eq!(
        journal(&root, "news")
            .iter()
            .filter(|l| l.code.as_deref() == Some("binding_changed"))
            .count(),
        1,
        "the stale line stays"
    );
    drop(rig);
}

// --- 23. a revocation to a binding seen before is logged --------------------

#[test]
fn a_revocation_to_a_binding_seen_before_is_logged() {
    let rig = Rig::new("tg24", &simple_pb("tg24"));
    let root = rig.root();
    let flip = |root: &Path, to_b: bool| flip_target(root, "tg24", to_b);
    set_rule(
        &root,
        &rule_doc("news", "tg24", "{{event.seq}}", 10, true, "main"),
        "test",
        Millis(now()),
    )
    .unwrap();
    approve_real(&root, "news");
    sweep_real(&rig);
    deliver("main", "e1", json!({}));
    let out = sweep_real(&rig);
    assert_eq!(out[0].1.as_ref().unwrap().started.len(), 1, "{out:?}");
    // A to B: revoked, one line with the resolved binding B.
    flip(&root, true);
    deliver("main", "e2", json!({}));
    let out = sweep_real(&rig);
    assert_eq!(out[0].1.as_ref().unwrap().started.len(), 0);
    assert!(entry_of(&root, "news").approval.is_none());
    // Approved again at B (the state is deleted, the pass repositions),
    // back to A: revoked again, resolved A.
    approve_real(&root, "news");
    sweep_real(&rig);
    flip(&root, false);
    deliver("main", "e3", json!({}));
    let out = sweep_real(&rig);
    assert_eq!(out[0].1.as_ref().unwrap().started.len(), 0);
    // Approved again at A, to B once more: the third revocation resolves a
    // binding seen before, with its own id.
    approve_real(&root, "news");
    sweep_real(&rig);
    flip(&root, true);
    deliver("main", "e4", json!({}));
    let out = sweep_real(&rig);
    assert_eq!(out[0].1.as_ref().unwrap().started.len(), 0);
    let lines: Vec<LogEntry> = journal(&root, "news")
        .into_iter()
        .filter(|l| l.code.as_deref() == Some("binding_changed"))
        .collect();
    assert_eq!(lines.len(), 3, "{lines:?}");
    assert_ne!(
        lines[2].id, lines[0].id,
        "a distinct id despite the binding seen before"
    );
    assert_eq!(lines[2].binding, lines[0].binding, "the same binding");
    assert_ne!(lines[2].seq, lines[0].seq);
}

// --- 24a. an unreadable runs directory holds the cap ------------------------

#[cfg(unix)]
#[test]
fn an_unreadable_runs_directory_holds_the_cap() {
    use std::os::unix::fs::PermissionsExt;
    let rig = Rig::new("tg25", &simple_pb("tg25"));
    let root = rig.root();
    set_rule(
        &root,
        &rule_doc("news", "tg25", "{{event.seq}}", 10, true, "main"),
        "test",
        Millis(now()),
    )
    .unwrap();
    approve(&root, "news");
    let script = Script::default();
    sweep_once(&root, &script);
    deliver("main", "m1", json!({}));
    let runs = root.join(".apb/runs");
    fs::set_permissions(&runs, fs::Permissions::from_mode(0o000)).unwrap();
    let out = sweep_once(&root, &script);
    let report = out[0].1.as_ref().unwrap();
    assert_eq!(report.started.len(), 0, "{report:?}");
    assert_eq!(report.stopped.as_deref(), Some("rate_cap"));
    assert_eq!(seen_through(&root, "news"), 0);
    let inc = incarnation_of(&root, "news");
    let held = format!("{inc}:1:held:rate_cap_unknown");
    assert!(
        ids_of(&root, "news").contains(&held),
        "{:?}",
        journal(&root, "news")
    );
    // The second pass over the still unreadable directory: the held line is
    // not written twice, nothing starts.
    let out = sweep_once(&root, &script);
    let report = out[0].1.as_ref().unwrap();
    assert_eq!(report.started.len(), 0);
    assert_eq!(report.stopped.as_deref(), Some("rate_cap"));
    assert_eq!(
        journal(&root, "news")
            .iter()
            .filter(|l| l.verdict == Verdict::Held)
            .count(),
        1,
        "{:?}",
        journal(&root, "news")
    );
    // Readable again: the event starts.
    fs::set_permissions(&runs, fs::Permissions::from_mode(0o755)).unwrap();
    let out = sweep_once(&root, &script);
    assert_eq!(
        out[0].1.as_ref().unwrap().started,
        vec!["run-1".to_string()],
        "{out:?}"
    );
}

// --- 24. an unattributable run counts toward the cap ------------------------

#[test]
fn an_unattributable_run_counts_toward_the_cap() {
    let rig = Rig::new("tg26", &simple_pb("tg26"));
    let root = rig.root();
    set_rule(
        &root,
        &rule_doc("news", "tg26", "{{event.seq}}", 2, true, "main"),
        "test",
        Millis(now()),
    )
    .unwrap();
    approve_real(&root, "news");
    let script = Script::default();
    sweep_once(&root, &script);
    // A real run, then a stamped directory whose manifest bytes do not read
    // (the manifest::amend window).
    deliver("main", "m0", json!({}));
    let out = sweep_real(&rig);
    assert_eq!(out[0].1.as_ref().unwrap().started.len(), 1, "{out:?}");
    let real = run_dirs(&root).pop().unwrap();
    let forged = root.join(".apb/runs/tg26-forged");
    fs::create_dir_all(&forged).unwrap();
    fs::write(forged.join("events.jsonl"), "{\"seq\":1,\"ts\":1}\n").unwrap();
    apb_core::run_origin::stamp(&forged, "tg26-forged").unwrap();
    let mut manifest =
        fs::read_to_string(root.join(".apb/runs").join(&real).join("manifest.yaml")).unwrap();
    manifest.push_str("  broken: [unclosed\n");
    fs::write(forged.join("manifest.yaml"), manifest).unwrap();
    // The cap counts it: a second matching start is held at the cap of 2.
    assert_eq!(
        starts_in_last_hour(&root, &incarnation_of(&root, "news"), Millis(now())).unwrap(),
        2
    );
    deliver("main", "m1", json!({}));
    let out = sweep_real(&rig);
    let report = out[0].1.as_ref().unwrap();
    assert_eq!(report.started.len(), 0, "{report:?}");
    assert_eq!(report.stopped.as_deref(), Some("rate_cap"));
    // Gone: the cap frees and the held event starts.
    fs::remove_dir_all(&forged).unwrap();
    assert_eq!(
        starts_in_last_hour(&root, &incarnation_of(&root, "news"), Millis(now())).unwrap(),
        1
    );
    let out = sweep_real(&rig);
    let report = out[0].1.as_ref().unwrap();
    assert_eq!(report.started.len(), 1, "{report:?}");
}

// --- 25. a lost launch record counts toward every cap -----------------------

#[test]
fn a_lost_launch_record_counts_toward_every_cap() {
    let rig = Rig::new("tg27", &simple_pb("tg27"));
    let root = rig.root();
    set_rule(
        &root,
        &rule_doc("news", "tg27", "{{event.seq}}", 10, true, "main"),
        "test",
        Millis(now()),
    )
    .unwrap();
    approve_real(&root, "news");
    let script = Script::default();
    sweep_once(&root, &script);
    deliver("main", "m0", json!({}));
    let out = sweep_real(&rig);
    assert_eq!(out[0].1.as_ref().unwrap().started.len(), 1, "{out:?}");
    // A manifest an older apb's amend left: the known string field
    // `initiator` kept, `triggered_by` gone, in its own stamped directory.
    let lost = root.join(".apb/runs/tg27-lost");
    fs::create_dir_all(&lost).unwrap();
    fs::write(lost.join("events.jsonl"), "{\"seq\":1,\"ts\":1}\n").unwrap();
    apb_core::run_origin::stamp(&lost, "tg27-lost").unwrap();
    fs::write(
        lost.join("manifest.yaml"),
        "profiles: []\nnode_bindings: {}\nlaunch:\n  kind: start\n  initiator: trigger:news\n  policy: allow\n",
    )
    .unwrap();
    let written = fs::read_to_string(lost.join("manifest.yaml")).unwrap();
    assert!(written.contains("trigger:news"), "{written}");
    assert!(!written.contains("triggered_by"), "{written}");
    // It counts toward the cap of this rule's incarnation...
    assert_eq!(
        starts_in_last_hour(&root, &incarnation_of(&root, "news"), Millis(now())).unwrap(),
        2
    );
    // ...and toward the cap of every other rule of the project.
    assert_eq!(
        starts_in_last_hour(&root, "another-incarnation", Millis(now())).unwrap(),
        1
    );
}

// --- 26. a stamped directory without manifest does not count ----------------

#[test]
fn a_stamped_directory_without_manifest_does_not_count() {
    let rig = Rig::new("tg28", &simple_pb("tg28"));
    let root = rig.root();
    set_rule(
        &root,
        &rule_doc("news", "tg28", "{{event.seq}}", 1, true, "main"),
        "test",
        Millis(now()),
    )
    .unwrap();
    approve_real(&root, "news");
    let script = Script::default();
    sweep_once(&root, &script);
    // A failed preparation's leftover: stamped, `run_started`, no manifest.
    let failed = root.join(".apb/runs/tg28-unstarted");
    fs::create_dir_all(&failed).unwrap();
    fs::write(
        failed.join("events.jsonl"),
        "{\"seq\":1,\"ts\":1,\"type\":\"run_started\"}\n",
    )
    .unwrap();
    apb_core::run_origin::stamp(&failed, "tg28-unstarted").unwrap();
    assert_eq!(run_dirs_strict(&root).unwrap().len(), 1);
    assert_eq!(
        starts_in_last_hour(&root, &incarnation_of(&root, "news"), Millis(now())).unwrap(),
        0
    );
    // An event starts under the free cap.
    deliver("main", "m1", json!({}));
    let out = sweep_real(&rig);
    assert_eq!(out[0].1.as_ref().unwrap().started.len(), 1, "{out:?}");
    assert_eq!(run_dirs(&root).len(), 2);
}

// --- 27. a failed hand over counts at once ----------------------------------

#[test]
fn a_failed_hand_over_counts_at_once() {
    let rig = Rig::new("tg29", &simple_pb("tg29"));
    let root = rig.root();
    set_rule(
        &root,
        &rule_doc("news", "tg29", "{{event.seq}}", 1, true, "main"),
        "test",
        Millis(now()),
    )
    .unwrap();
    approve_real(&root, "news");
    sweep_real(&rig);
    // Two events in one pass: the first starts, the second is held, because
    // the count is read before every matching start and the run the first
    // outcome left behind counts at once.
    deliver("main", "e1", json!({}));
    deliver("main", "e2", json!({}));
    let out = sweep_real(&rig);
    let report = out[0].1.as_ref().unwrap();
    assert_eq!(report.started.len(), 1, "{report:?}");
    assert_eq!(report.stopped.as_deref(), Some("rate_cap"));
    assert_eq!(seen_through(&root, "news"), 1);
    assert_eq!(run_dirs(&root).len(), 1);
}

// --- 28. a disable during a pass starts nothing after it --------------------

#[test]
fn a_disable_during_a_pass_starts_nothing_after_it() {
    let rig = Rig::new("tg30", &simple_pb("tg30"));
    let root = rig.root();
    set_rule(
        &root,
        &rule_doc("news", "tg30", "{{event.seq}}", 10, true, "main"),
        "test",
        Millis(now()),
    )
    .unwrap();
    approve(&root, "news");
    sweep_once(&root, &Script::default());
    deliver("main", "e1", json!({}));
    deliver("main", "e2", json!({}));
    // The person's disable, started while the pass is inside its first
    // start: it takes the change lock and then waits for the sweep lock the
    // pass holds, so it commits after the current start only.
    let (done_tx, done_rx) = mpsc::channel::<()>();
    let done_slot: Mutex<Option<mpsc::Sender<()>>> = Mutex::new(Some(done_tx));
    let fire = {
        let root = root.clone();
        move |req: &FireRequest<'_>| {
            if req.event.seq == 1 {
                let root2 = root.clone();
                let done_tx = done_slot.lock().unwrap().take();
                std::thread::spawn(move || {
                    set_enabled(&root2, "news", false, "person", Millis(now())).unwrap();
                    if let Some(done_tx) = done_tx {
                        let _ = done_tx.send(());
                    }
                });
                let dir = root.join(".apb/trigger-state");
                wait_for("the change lock", || {
                    apb_core::fsutil::flock_held(&dir, "news.change.lock")
                        .ok()
                        .filter(|held| *held)
                        .map(|_| ())
                });
            }
            FireOutcome::Started {
                run_id: format!("run-{}", req.event.seq),
            }
        }
    };
    let out = sweep(&root, &|| Millis(now()), &fire);
    let report = out[0].1.as_ref().unwrap();
    assert_eq!(
        report.started.len(),
        1,
        "one start, made before the disable committed: {report:?}"
    );
    assert_eq!(
        report.stopped.as_deref(),
        Some("change_waiting"),
        "{report:?}"
    );
    assert_eq!(
        seen_through(&root, "news"),
        1,
        "the position never passes the second event"
    );
    done_rx
        .recv_timeout(Duration::from_secs(30))
        .expect("the disable committed after the pass ended");
    assert!(!entry_of(&root, "news").enabled, "the disable committed");
    // A later pass starts nothing: the rule is disabled.
    let out = sweep_once(&root, &Script::default());
    assert!(out.is_empty(), "{out:?}");
}

// --- 29. leftovers of a crashed remove are inert ----------------------------

#[test]
fn leftovers_of_a_crashed_remove_are_inert() {
    let rig = Rig::new("tg31", &simple_pb("tg31"));
    let root = rig.root();
    set_rule(
        &root,
        &rule_doc("news", "tg31", "{{event.seq}}", 10, true, "main"),
        "test",
        Millis(now()),
    )
    .unwrap();
    approve(&root, "news");
    let script = Script::default();
    sweep_once(&root, &script);
    deliver("main", "m1", json!({}));
    sweep_once(&root, &script);
    let first_inc = incarnation_of(&root, "news");
    // The remove crashed after the rules write, before the state and the
    // journal were deleted: the entry is gone, the leftovers stay.
    signed_store::transact(
        &root,
        Scope::Project,
        "rules",
        Unsigned::Refuse,
        |file: &mut RulesFile| -> Result<((), bool), signed_store::StoreError> {
            file.rules.retain(|e| e.rule.name != "news");
            Ok(((), true))
        },
    )
    .unwrap();
    let old_path = root
        .join(".apb/trigger-state/log")
        .join(format!("{first_inc}.jsonl"));
    assert!(old_path.is_file());
    // A rule created again under the same name has another incarnation; its
    // approval deletes the old state entry.
    set_rule(
        &root,
        &rule_doc("news", "tg31", "{{event.seq}}", 10, true, "main"),
        "test",
        Millis(now()),
    )
    .unwrap();
    let second_inc = incarnation_of(&root, "news");
    assert_ne!(first_inc, second_inc);
    approve(&root, "news");
    let state = signed_store::read::<TriggerStateFile>(&root, Scope::Project, "state")
        .unwrap()
        .file;
    assert!(
        state.rules.iter().all(|s| s.incarnation == second_inc),
        "{:?}",
        state.rules
    );
    // The new rule works; the old journal never mixes in.
    let out = sweep_once(&root, &script);
    assert_eq!(out[0].1.as_ref().unwrap().evaluated, 0, "{out:?}");
    deliver("main", "m2", json!({}));
    let out = sweep_once(&root, &script);
    assert_eq!(
        out[0].1.as_ref().unwrap().started,
        vec!["run-2".to_string()],
        "{out:?}"
    );
    let new_ids = ids_of(&root, "news");
    assert!(
        new_ids.iter().all(|id| id.starts_with(&second_inc)),
        "{new_ids:?}"
    );
    let old = fs::read_to_string(&old_path).unwrap();
    assert!(old.contains(&format!("{first_inc}:1:started")), "{old}");
}

// --- 30. a rule start records its rule (real start) -------------------------

#[test]
fn a_rule_start_records_its_rule() {
    let rig = Rig::new("tg32", &simple_pb("tg32"));
    let root = rig.root();
    set_rule(
        &root,
        &rule_doc("news", "tg32", "{{event.seq}}", 10, true, "main"),
        "test",
        Millis(now()),
    )
    .unwrap();
    approve_real(&root, "news");
    sweep_real(&rig);
    deliver("main", "m1", json!({}));
    let out = sweep_real(&rig);
    let run_id = out[0].1.as_ref().unwrap().started[0].clone();
    let run_dir = root.join(".apb/runs").join(&run_id);
    let manifest = apb_engine::manifest::read(&run_dir).unwrap().unwrap();
    let launch = manifest.launch.expect("a launch record");
    assert_eq!(launch.initiator, "trigger:news");
    assert_eq!(launch.kind, "start");
    let by = launch.triggered_by.expect("the rule");
    assert_eq!(by.rule, "news");
    assert_eq!(by.incarnation, incarnation_of(&root, "news"));
    assert_eq!(by.source, "inbox");
    assert_eq!(by.connector.as_deref(), Some("github"));
    assert_eq!(by.account.as_deref(), Some("main"));
    assert_eq!(by.event_seq, Some(1));
    // The manifest carries no other mark: the run is an ordinary unattended
    // run to every reader.
    let body = fs::read_to_string(run_dir.join("manifest.yaml")).unwrap();
    assert!(!body.contains("rule_start"), "{body}");
}

// --- 31. a rule start is unattended (real start) ----------------------------

#[test]
fn a_rule_start_is_unattended() {
    // A gate that needs a person fails the run hitl_required.
    let gate_pb = "schema: 2\nid: tg33\nname: tg33\ndefaults: { profile: main, on_failure: rescue }\nversion: 1.0.0\nnodes:\n  - { id: start, type: start }\n  - { id: gate, type: human_review }\n  - { id: rescue, type: script, script: scripts/r.sh, runner: sh }\n  - { id: done, type: finish, outcome: success }\n  - { id: failed, type: finish, outcome: failure }\nedges:\n  - { from: start, to: gate }\n  - { from: gate, to: done }\n  - { from: rescue, to: failed }\n";
    let rig = Rig::new("tg33", gate_pb);
    let root = rig.root();
    write_script(&root, "tg33", "r.sh", "exit 0\n");
    trust_current(&root, "tg33");
    set_rule(
        &root,
        &rule_doc("news", "tg33", "{{event.seq}}", 10, true, "main"),
        "test",
        Millis(now()),
    )
    .unwrap();
    approve_real(&root, "news");
    sweep_real(&rig);
    deliver("main", "m1", json!({}));
    let out = sweep_real(&rig);
    let run_id = out[0].1.as_ref().unwrap().started[0].clone();
    let run_dir = root.join(".apb/runs").join(&run_id);
    let manifest = apb_engine::manifest::read(&run_dir).unwrap().unwrap();
    assert!(manifest.unattended, "a rule's start is unattended");
    wait_for("the run error", || {
        apb_engine::event::read_all(&run_dir)
            .ok()?
            .iter()
            .any(|e| {
                matches!(&e.payload,
                apb_engine::event::EventPayload::RunError { reason, .. }
                if reason.contains("hitl_required"))
            })
            .then_some(())
    });
    let events = apb_engine::event::read_all(&run_dir).unwrap();
    let reason = events.iter().rev().find_map(|e| match &e.payload {
        apb_engine::event::EventPayload::RunError { reason, .. } => Some(reason.clone()),
        _ => None,
    });
    assert_eq!(
        reason.as_deref(),
        Some("hitl_required: human_review `gate` has no unattended outcome")
    );

    // The rig holds the process env lock; release it before the second
    // rig of this test takes it.
    drop(rig);
    // A long timer wait keeps the driver: no park.
    let wait_pb = "schema: 2\nid: tg34\nname: tg34\nversion: 1.0.0\nnodes:\n  - { id: start, type: start }\n  - { id: work, type: script, script: scripts/go.sh, runner: sh }\n  - { id: pause, type: wait, wait_for: { type: timer, seconds: 3 }, timeout_seconds: 3600 }\n  - { id: done, type: finish, outcome: success }\nedges:\n  - { from: start, to: work }\n  - { from: work, to: pause }\n  - { from: pause, to: done }\n";
    let rig = Rig::new("tg34", wait_pb);
    let root = rig.root();
    write_script(&root, "tg34", "go.sh", "echo ok\n");
    trust_current(&root, "tg34");
    let _release = EnvVar::set("APB_RELEASE_AFTER_MS", "300");
    let _poll = EnvVar::set("APB_WAKER_POLL_MS", "50");
    set_rule(
        &root,
        &rule_doc("news", "tg34", "{{event.seq}}", 10, true, "main"),
        "test",
        Millis(now()),
    )
    .unwrap();
    approve_real(&root, "news");
    sweep_real(&rig);
    deliver("main", "m1", json!({}));
    let out = sweep_real(&rig);
    let run_id = out[0].1.as_ref().unwrap().started[0].clone();
    let run_dir = root.join(".apb/runs").join(&run_id);
    let manifest = apb_engine::manifest::read(&run_dir).unwrap().unwrap();
    assert!(manifest.unattended);
    wait_for("the wait", || {
        apb_engine::event::read_all(&run_dir)
            .ok()?
            .iter()
            .any(|e| {
                matches!(
                    e.payload,
                    apb_engine::event::EventPayload::WaitStarted { .. }
                )
            })
            .then_some(())
    });
    // The positive marker after every park decision: the 3 s timer ends
    // and the run finishes (a park would have come at 300 ms, well before).
    wait_for("the run to finish", || {
        apb_engine::event::read_all(&run_dir)
            .ok()?
            .iter()
            .any(|e| {
                matches!(
                    e.payload,
                    apb_engine::event::EventPayload::RunFinished { .. }
                )
            })
            .then_some(())
    });
    let events = apb_engine::event::read_all(&run_dir).unwrap();
    assert!(
        !events
            .iter()
            .any(|e| matches!(e.payload, apb_engine::event::EventPayload::RunPaused { .. })),
        "no run_paused: the driver keeps the run"
    );
}

// --- 32. the state lock is free while starting ------------------------------

#[test]
fn the_state_lock_is_free_while_starting() {
    let rig = Rig::new("tg35", &simple_pb("tg35"));
    let root = rig.root();
    set_rule(
        &root,
        &rule_doc("news", "tg35", "{{event.seq}}", 10, true, "main"),
        "test",
        Millis(now()),
    )
    .unwrap();
    approve(&root, "news");
    sweep_once(&root, &Script::default());
    deliver("main", "m1", json!({}));
    let checks = Arc::new(Mutex::new(Vec::new()));
    let root2 = root.clone();
    let fire_checks = Arc::clone(&checks);
    let fire = move |req: &FireRequest<'_>| {
        let dir = root2.join(".apb/trigger-state");
        // The sweep lock is held; the state transaction's lock is free.
        let sweep_held = apb_core::fsutil::flock_held(&dir, "news.sweep.lock").unwrap();
        let state_free = apb_core::fsutil::lock_dir(&dir, "state.json.lock").is_ok();
        fire_checks.lock().unwrap().push((sweep_held, state_free));
        FireOutcome::Started {
            run_id: format!("run-{}", req.event.seq),
        }
    };
    let out = sweep(&root, &|| Millis(now()), &fire);
    assert_eq!(out[0].1.as_ref().unwrap().started.len(), 1, "{out:?}");
    let checks = checks.lock().unwrap().clone();
    assert_eq!(checks, vec![(true, true)], "{checks:?}");
}

// --- 33. a rule run earns no probation credit (real start) ------------------

#[test]
fn a_rule_run_earns_no_probation_credit() {
    let parent = "schema: 2\nid: tg36\nname: tg36\nversion: 1.0.0\nnodes:\n  - { id: start, type: start }\n  - { id: c, type: playbook, playbook: tg36child, instruction: \"child input\" }\n  - { id: done, type: finish, outcome: success }\nedges:\n  - { from: start, to: c }\n  - { from: c, to: done }\n";
    let child = "schema: 2\nid: tg36child\nname: tg36child\nversion: 1.0.0\nnodes:\n  - { id: start, type: start }\n  - { id: n, type: prompt, prompt: \"{{run.instruction}}\" }\n  - { id: done, type: finish, outcome: success }\nedges:\n  - { from: start, to: n }\n  - { from: n, to: done }\n";
    let rig = Rig::new("tg36", parent);
    let root = rig.root();
    write_playbook(&root, "tg36child", child);
    trust_current(&root, "tg36child");
    // external_data: false (the V266 warning only), with a sub-playbook.
    set_rule(
        &root,
        &rule_doc("news", "tg36", "{{event.seq}}", 10, false, "main"),
        "test",
        Millis(now()),
    )
    .unwrap();
    approve_real(&root, "news");
    sweep_real(&rig);
    deliver("main", "m1", json!({}));
    let out = sweep_real(&rig);
    let run_id = out[0].1.as_ref().unwrap().started[0].clone();
    let parent_dir = root.join(".apb/runs").join(&run_id);
    assert_eq!(
        apb_engine::run_config::read_run_config(&parent_dir)
            .unwrap()
            .parent_run,
        None
    );
    let events = apb_engine::event::read_all(&parent_dir).unwrap();
    let child_id = events
        .iter()
        .find_map(|e| match &e.payload {
            apb_engine::event::EventPayload::ChildRunStarted { run_id, .. } => Some(run_id.clone()),
            _ => None,
        })
        .expect("a child run");
    let child_dir = root.join(".apb/runs").join(&child_id);
    assert_eq!(
        apb_engine::run_config::read_run_config(&child_dir)
            .unwrap()
            .parent_run
            .as_deref(),
        Some(run_id.as_str())
    );
    wait_for("both ledgers", || {
        let parent_ok = apb_core::probation::read(&root, Scope::Project, "tg36")
            .ok()
            .map(|f| !f.file.entries.is_empty())
            .unwrap_or(false);
        let child_ok = apb_core::probation::read(&root, Scope::Project, "tg36child")
            .ok()
            .map(|f| !f.file.entries.is_empty())
            .unwrap_or(false);
        (parent_ok && child_ok).then_some(())
    });
    for playbook in ["tg36", "tg36child"] {
        let file = apb_core::probation::read(&root, Scope::Project, playbook)
            .unwrap()
            .file;
        assert_eq!(file.entries.len(), 1, "{playbook}: {:?}", file.entries);
        let entry = &file.entries[0];
        assert!(entry.unattended, "{playbook}: {:?}", entry);
        assert_eq!(entry.outcome, "succeeded");
        // Attended credit takes unattended == false: the count stays 0.
        assert_eq!(
            apb_core::probation::counted(&file, &entry.trust_digest, &entry.definition),
            0,
            "{playbook}"
        );
    }
}

// --- 34. a default rule run writes no probation entry (real start) ----------

#[test]
fn a_default_rule_run_writes_no_probation_entry() {
    let rig = Rig::new("tg37", &simple_pb("tg37"));
    let root = rig.root();
    // external_data defaults to true: a default rule run. A second rule
    // without it is the positive control: its run writes the ledger.
    set_rule(
        &root,
        &rule_doc("news", "tg37", "{{event.seq}}", 10, true, "main"),
        "test",
        Millis(now()),
    )
    .unwrap();
    set_rule(
        &root,
        &rule_doc("plain", "tg37", "{{event.seq}}", 10, false, "main"),
        "test",
        Millis(now()),
    )
    .unwrap();
    approve_real(&root, "news");
    approve_real(&root, "plain");
    sweep_real(&rig);
    deliver("main", "m1", json!({}));
    // One pass starts both, in the rules' order, each run driven to its end
    // in this process: the default run ends, and its terminal observers
    // run, before the control run starts.
    let out = sweep_real(&rig);
    let started: Vec<String> = out
        .iter()
        .flat_map(|(_, r)| r.as_ref().unwrap().started.clone())
        .collect();
    assert_eq!(started.len(), 2, "{out:?}");
    let default_dir = root.join(".apb/runs").join(&started[0]);
    assert!(
        fs::read_to_string(default_dir.join("manifest.yaml"))
            .unwrap()
            .contains("external_data: true")
    );
    // The positive marker: the control run's ledger entry is written, so
    // every observer of the earlier run has certainly run too.
    wait_for("the control run's ledger entry", || {
        apb_core::probation::read(&root, Scope::Project, "tg37")
            .ok()
            .filter(|f| !f.file.entries.is_empty())
            .map(|_| ())
    });
    let file = apb_core::probation::read(&root, Scope::Project, "tg37")
        .unwrap()
        .file;
    assert_eq!(
        file.entries.len(),
        1,
        "only the control run wrote an entry: {:?}",
        file.entries
    );
}

// --- review round 1: a pass reads the state under its lock -------------------

/// Two sweepers interleaved through the fire: while sweeper A holds the
/// first rule's lock, sweeper B positions the second rule and starts an
/// event of it; a later event arrives. When A reaches the second rule it
/// reads the state B committed (I4, I5): the later event is evaluated and
/// started once, nothing is positioned past it, and B's event is not
/// started again.
#[test]
fn two_sweepers_never_lose_or_repeat_an_event() {
    let rig = Rig::new("tg40", &simple_pb("tg40"));
    let root = rig.root();
    set_rule(
        &root,
        &rule_doc("alpha", "tg40", "{{event.seq}}", 10, true, "main"),
        "test",
        Millis(now()),
    )
    .unwrap();
    approve(&root, "alpha");
    sweep_once(&root, &Script::default());
    deliver("main", "a1", json!({}));
    set_rule(
        &root,
        &rule_doc("beta", "tg40", "{{event.seq}}", 10, true, "main"),
        "test",
        Millis(now()),
    )
    .unwrap();
    approve(&root, "beta");
    let inner = Script::default();
    let outer: Mutex<Vec<(String, u64)>> = Mutex::new(Vec::new());
    let interleaved = std::cell::Cell::new(false);
    let out = sweep(&root, &|| Millis(now()), &|req: &FireRequest<'_>| {
        if req.triggered_by.rule == "alpha" && !interleaved.replace(true) {
            // Sweeper B, while A holds alpha's lock: beta is positioned at
            // the head (event 1), then starts event 2; event 3 arrives.
            sweep_once(&root, &inner);
            deliver("main", "b2", json!({}));
            sweep_once(&root, &inner);
            deliver("main", "b3", json!({}));
        }
        outer
            .lock()
            .unwrap()
            .push((req.triggered_by.rule.clone(), req.event.seq));
        FireOutcome::Started {
            run_id: format!("{}-{}", req.triggered_by.rule, req.event.seq),
        }
    });
    assert!(interleaved.get(), "the interleaving ran: {out:?}");
    let inner_beta: Vec<u64> = inner
        .seen()
        .into_iter()
        .filter(|(_, _, rule)| rule == "beta")
        .map(|(_, seq, _)| seq)
        .collect();
    assert_eq!(inner_beta, vec![2], "sweeper B started beta's event 2");
    let outer_beta: Vec<u64> = outer
        .lock()
        .unwrap()
        .iter()
        .filter(|(rule, _)| rule == "beta")
        .map(|(_, seq)| *seq)
        .collect();
    assert_eq!(
        outer_beta,
        vec![3],
        "sweeper A started beta's event 3 from B's position, never event 2 again"
    );
    assert_eq!(seen_through(&root, "beta"), 3);
    assert!(
        journal(&root, "beta")
            .iter()
            .any(|l| l.seq == Some(3) && l.verdict == Verdict::Started),
        "{:?}",
        journal(&root, "beta")
    );
}

// --- review round 1: the hourly cap fails closed ---------------------------

/// A run of this incarnation whose journal is gone, while its manifest and
/// origin stamp remain, cannot show when it started: it counts as a start
/// inside the hour, and the cap holds the next event.
#[test]
fn a_run_whose_journal_is_lost_still_counts() {
    let rig = Rig::new("tg41", &simple_pb("tg41"));
    let root = rig.root();
    set_rule(
        &root,
        &rule_doc("news", "tg41", "{{event.seq}}", 1, true, "main"),
        "test",
        Millis(now()),
    )
    .unwrap();
    approve_real(&root, "news");
    sweep_once(&root, &Script::default());
    deliver("main", "m0", json!({}));
    let out = sweep_real(&rig);
    assert_eq!(out[0].1.as_ref().unwrap().started.len(), 1, "{out:?}");
    let real = run_dirs(&root).pop().unwrap();
    fs::remove_file(root.join(".apb/runs").join(&real).join("events.jsonl")).unwrap();
    assert_eq!(
        starts_in_last_hour(&root, &incarnation_of(&root, "news"), Millis(now())).unwrap(),
        1,
        "the attributed run without a journal counts"
    );
    deliver("main", "m1", json!({}));
    let out = sweep_real(&rig);
    let report = out[0].1.as_ref().unwrap();
    assert!(report.started.is_empty(), "{report:?}");
    assert_eq!(report.stopped.as_deref(), Some("rate_cap"));
}

// --- review round 1: a marker's run never leaves the runs directory --------

/// A marker naming a run of this project this installation created is the
/// rule's own send; one naming a stamped run of another project by its
/// absolute path, or by a path that climbs out, is not: the event is
/// evaluated as any other.
#[test]
fn a_marker_run_must_be_a_run_of_this_project() {
    let rig = Rig::new("tg42", &simple_pb("tg42"));
    let root = rig.root();
    set_rule(
        &root,
        &rule_doc("news", "tg42", "{{body.text}}", 10, true, "main"),
        "test",
        Millis(now()),
    )
    .unwrap();
    approve(&root, "news");
    sweep_once(&root, &Script::default());
    // A stamped run of this project, and one of another project.
    let mine = root.join(".apb/runs/tg42-mine");
    fs::create_dir_all(&mine).unwrap();
    apb_core::run_origin::stamp(&mine, "tg42-mine").unwrap();
    let other = tempfile::tempdir().unwrap();
    let theirs = other.path().join(".apb/runs/tg42-theirs");
    fs::create_dir_all(&theirs).unwrap();
    apb_core::run_origin::stamp(&theirs, "tg42-theirs").unwrap();
    let marked = |run: &str| {
        let fields = apb_core::connector::marker::MarkerFields {
            run: run.to_string(),
            node: "act".to_string(),
            attempt: Some(1),
            nonce: "abcdefghijklmnopqrstuvwx".to_string(),
        };
        format!(
            "hello {}",
            apb_core::connector::marker::render(
                apb_core::connector::marker::MarkerFormat::HtmlComment,
                &fields
            )
        )
    };
    deliver("main", "own", json!({ "text": marked("tg42-mine") }));
    deliver(
        "main",
        "abs",
        json!({ "text": marked(&theirs.display().to_string()) }),
    );
    deliver(
        "main",
        "climb",
        json!({ "text": marked("../../x/.apb/runs/tg42-theirs") }),
    );
    sweep_once(&root, &Script::default());
    let lines = journal(&root, "news");
    let code_of = |seq: u64| {
        lines
            .iter()
            .find(|l| l.seq == Some(seq))
            .and_then(|l| l.code.clone())
    };
    assert_eq!(code_of(1).as_deref(), Some("own_send"), "{lines:?}");
    assert_ne!(code_of(2).as_deref(), Some("own_send"), "{lines:?}");
    assert_ne!(code_of(3).as_deref(), Some("own_send"), "{lines:?}");
    assert!(lines.iter().any(|l| l.seq == Some(2)), "{lines:?}");
    assert!(lines.iter().any(|l| l.seq == Some(3)), "{lines:?}");
}

// --- review round 2: the rules store never grows past its read cap ---------

/// A save that would make the rules file unreadable is refused (S2): saves
/// of valid rules up to the reader's limit, the next one refused
/// `rules_store_full` with the file exactly as it was, and every earlier
/// rule still read, swept, approved, disabled and removed.
#[test]
fn a_save_past_the_rules_read_cap_is_refused_and_the_store_stays_usable() {
    let rig = Rig::new("tgcap", &simple_pb("tgcap"));
    let root = rig.root();
    let path = signed_store::file::<RulesFile>(&root, Scope::Project, "rules");
    let cap = apb_core::signed_store::NO_LINKS_CAP;
    // A valid rule that is mostly bulk: a static key of about half a
    // megabyte (validation checks that the template parses, not its
    // length).
    let bulk = "k".repeat(512 * 1024);
    let mut names: Vec<String> = Vec::new();
    for n in 1..40 {
        let name = format!("bulk-{n}");
        let doc = rule_doc(&name, "tgcap", &bulk, 5, true, "main");
        let before = fs::read(&path).unwrap_or_default();
        match set_rule(&root, &doc, "test", Millis(now())) {
            Ok(_) => {
                assert!(
                    (before.len() as u64) < cap,
                    "save {n} went past the cap: {}",
                    before.len()
                );
                names.push(name);
            }
            Err(e) => {
                assert!(
                    e.to_string().contains("rules_store_full"),
                    "the refusal names itself: {e}"
                );
                assert_eq!(
                    fs::read(&path).unwrap(),
                    before,
                    "the refused write left the file as it was"
                );
                assert!(names.len() >= 2, "the cap was reached too early: {names:?}");
                // Every earlier rule still reads as this installation
                // wrote it.
                let file = read_rules_signed(&root);
                assert_eq!(file.rules.len(), names.len(), "every earlier rule");
                // The sweep reads the same store: no unreadable-store
                // error for the project, and one entry per approved rule
                // (an unapproved rule is skipped, never an error).
                let out = sweep_once(&root, &Script::default());
                assert!(!out.iter().any(|(name, _)| name == "*"), "{out:?}");
                approve(&root, &names[0]);
                approve(&root, &names[1]);
                assert!(entry_of(&root, &names[0]).approval.is_some());
                let out = sweep_once(&root, &Script::default());
                assert_eq!(
                    out.iter().map(|(n, _)| n.clone()).collect::<Vec<_>>(),
                    vec![names[0].clone(), names[1].clone()],
                    "{out:?}"
                );
                assert!(out.iter().all(|(_, r)| r.is_ok()), "{out:?}");
                // Disabled and removed, each writing through the same
                // capped store.
                set_enabled(&root, &names[0], false, "test", Millis(now())).unwrap();
                assert!(!entry_of(&root, &names[0]).enabled);
                assert!(remove_rule(&root, &names[0]).unwrap().removed);
                let file = read_rules_signed(&root);
                assert_eq!(file.rules.len(), names.len() - 1, "the rule is gone");
                return;
            }
        }
    }
    panic!("no save was refused past the read cap");
}

/// The rules file, only as this installation signed it: what every earlier
/// save wrote is what a pass and a person's command read.
fn read_rules_signed(root: &Path) -> RulesFile {
    let loaded = signed_store::read::<RulesFile>(root, Scope::Project, "rules").unwrap();
    assert!(loaded.signed, "the rules file verifies for this project");
    loaded.file
}

// --- review round 2: a journal over its cap stops its rule ------------------

/// A journal the reader refuses stops its rule (S4): the rule is not
/// evaluated, nothing is started for it, the position does not move and
/// the file does not grow across two passes, while the other rule of the
/// project still fires and its own refusal is the pass's answer for it.
#[test]
fn a_journal_over_its_cap_stops_its_rule_and_not_the_others() {
    let rig = Rig::new("tgbig", &simple_pb("tgbig"));
    let root = rig.root();
    set_rule(
        &root,
        &rule_doc("capped", "tgbig", "{{event.seq}}", 10, true, "main"),
        "test",
        Millis(now()),
    )
    .unwrap();
    approve(&root, "capped");
    set_rule(
        &root,
        &rule_doc("other", "tgbig", "{{event.seq}}", 10, true, "other"),
        "test",
        Millis(now()),
    )
    .unwrap();
    approve(&root, "other");
    // Both rules positioned.
    sweep_once(&root, &Script::default());
    let path = root
        .join(".apb/trigger-state/log")
        .join(format!("{}.jsonl", incarnation_of(&root, "capped")));
    fs::write(&path, vec![b'x'; 17 << 20]).unwrap();
    let size = fs::metadata(&path).unwrap().len();
    deliver("main", "e1", json!({ "entity": "a" }));
    deliver("other", "e2", json!({ "entity": "b" }));
    let script = Script::default();
    let out = sweep_once(&root, &script);
    let capped = out
        .iter()
        .find(|(n, _)| n == "capped")
        .expect("the capped rule's entry");
    assert!(
        capped
            .1
            .as_ref()
            .is_err_and(|e| e.to_string().contains("journal_refused")),
        "{out:?}"
    );
    let other = out
        .iter()
        .find(|(n, _)| n == "other")
        .expect("the other rule's entry");
    assert_eq!(other.1.as_ref().unwrap().started.len(), 1, "{out:?}");
    assert!(
        !script.seen().iter().any(|(_, _, r)| r == "capped"),
        "nothing started for the capped rule: {:?}",
        script.seen()
    );
    assert_eq!(
        seen_through(&root, "capped"),
        0,
        "the position did not move"
    );
    assert_eq!(fs::metadata(&path).unwrap().len(), size, "no growth");
    // A second pass: the same refusal, still no growth, the other rule
    // going on with what remains.
    let out = sweep_once(&root, &Script::default());
    let capped = out.iter().find(|(n, _)| n == "capped").unwrap();
    assert!(capped.1.is_err(), "{out:?}");
    assert_eq!(fs::metadata(&path).unwrap().len(), size, "still no growth");
}
