//! The run-file and signed-store freeze: examples of a run's
//! `manifest.yaml` and `run.yaml` with nondefault state, the journal lines
//! of its start decisions (`run-events.yaml`: the budget, the notification
//! subscription, the deadline and the concurrency key, which the run keeps
//! in its journal, not in those two files), and one entry per
//! `SignedDoc` store kind, each written through the store's own public
//! write function under a fixed installation key. Each frozen document
//! reads through its reader, keeps every key under the schema freeze's
//! rule 3 comparison, and meets its hand-written expectation: named typed
//! values and the decisions its reader makes. See
//! `openspec/changes/one-point-oh-freeze` (design D1.5 items 2 and 4) and
//! `docs/COMPATIBILITY.md`.
//!
//! The writer behind `APB_WRITE_FREEZE=1` starts the runs and writes the
//! stores, records their bytes and only ever adds.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use apb_core::registry::init_project;
use apb_core::scope::Scope;
use apb_core::signed_store;
use apb_core::signed_store::SignedDoc as _;
use apb_engine::event::{Event, EventPayload};
use apb_engine::manifest::{self, RunExecutionManifest};
use apb_engine::run_config::{self, RunConfig};

const MARKER: &str = "# apb-freeze-document: ";

/// Set to `1` to let the test `write_freeze` add what this build has and
/// the freeze lacks. The writer only ever adds: it never removes an entry,
/// never changes a frozen error code, line or argument, and writes
/// nothing when any freeze rule fails.
const WRITE_FREEZE_ENV: &str = "APB_WRITE_FREEZE";

/// Whether the switch is set. Named `write_freeze_enabled`, never
/// `write_freeze`: that name is the writer test's.
fn write_freeze_enabled() -> bool {
    std::env::var(WRITE_FREEZE_ENV).as_deref() == Ok("1")
}

fn fixtures_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/run-files-1.0")
}

fn signed_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/signed-1.0")
}

fn fixture(path: &Path) -> String {
    std::fs::read_to_string(path)
        .unwrap_or_else(|e| panic!("cannot read the freeze fixture {}: {e}", path.display()))
}

/// `(name, text)` of every document, in file order. Panics on a repeated
/// name.
fn documents(file: &Path) -> Vec<(String, String)> {
    let text = fixture(file);
    let mut out: Vec<(String, String)> = Vec::new();
    let mut current: Option<(String, String)> = None;
    for line in text.lines() {
        if let Some(name) = line.strip_prefix(MARKER) {
            if let Some(doc) = current.take() {
                out.push(doc);
            }
            current = Some((name.to_string(), String::new()));
        } else if let Some((_, body)) = current.as_mut() {
            body.push_str(line);
            body.push('\n');
        }
    }
    if let Some(doc) = current.take() {
        out.push(doc);
    }
    let mut seen = std::collections::BTreeSet::new();
    for (name, _) in &out {
        assert!(seen.insert(name.as_str()), "duplicate document name {name}");
    }
    out
}

// ---------------------------------------------------------------------------
// Key retention (the schema freeze's rule 3, over these files)
// ---------------------------------------------------------------------------

fn raw_value(text: &str) -> yaml_serde::Value {
    yaml_serde::from_str(text).expect("the document is YAML")
}

/// The paths of `raw` the re-serialized form dropped, walked structurally
/// (a map key may hold `[` or `.`, so the paths are never parsed back).
fn dropped_paths(
    raw: &yaml_serde::Value,
    serialized: &yaml_serde::Value,
    prefix: &str,
) -> Vec<String> {
    let mut out = Vec::new();
    match (raw, serialized) {
        (yaml_serde::Value::Mapping(m), yaml_serde::Value::Mapping(s)) => {
            for (k, v) in m {
                let key = k.as_str().unwrap_or_default().to_string();
                let path = if prefix.is_empty() {
                    key.clone()
                } else {
                    format!("{prefix}.{key}")
                };
                match s.get(k) {
                    None => out.push(path),
                    Some(sv) => out.extend(dropped_paths(v, sv, &path)),
                }
            }
        }
        (yaml_serde::Value::Sequence(a), yaml_serde::Value::Sequence(b)) => {
            for (i, v) in a.iter().enumerate() {
                let path = format!("{prefix}[{i}]");
                match b.get(i) {
                    None => out.push(path),
                    Some(sv) => out.extend(dropped_paths(v, sv, &path)),
                }
            }
        }
        _ => {}
    }
    out
}

/// The schema freeze's rule 3 over a run file: every place the written
/// document holds is a place of `typed`, the reader's result serialized
/// again, so a reader that drops a frozen key fails.
fn keys_kept(file: &Path, name: &str, text: &str, typed: &yaml_serde::Value) {
    let raw = raw_value(text);
    if let Some(path) = dropped_paths(&raw, typed, "").first() {
        panic!(
            "the frozen document {name} (in {}) holds the key {path}, which its reader's result does not keep",
            file.display()
        );
    }
}

/// The reader's result as the value the key check compares with.
fn typed<T: serde::Serialize>(value: &T) -> yaml_serde::Value {
    yaml_serde::to_value(value).expect("the reader's result serializes")
}

// ---------------------------------------------------------------------------
// The run fixtures
// ---------------------------------------------------------------------------

/// The journal event types that carry what a run decided at its start
/// and that `manifest.yaml` and `run.yaml` do not hold: the budget in
/// force (`budget_set`), the notification subscription (`notify_set`) and
/// the provenance with the deadline, the concurrency key and the run
/// limits (`run_provenance`). Their lines are frozen per run in
/// `run-events.yaml`.
const START_EVENT_TYPES: &[&str] = &["run_provenance", "budget_set", "notify_set"];

/// One frozen run: its name, `manifest.yaml`, `run.yaml` and the journal
/// lines of its start decisions.
struct RunDoc {
    name: String,
    manifest: String,
    config: String,
    events: String,
}

/// The frozen run documents, the three files joined by name.
fn run_documents() -> Vec<RunDoc> {
    let configs = documents(&fixtures_dir().join("run-configs.yaml"));
    let events = documents(&fixtures_dir().join("run-events.yaml"));
    let of = |docs: &[(String, String)], file: &str, name: &str| {
        docs.iter()
            .find(|(n, _)| n == name)
            .unwrap_or_else(|| panic!("no {file} document named {name}"))
            .1
            .clone()
    };
    documents(&fixtures_dir().join("manifests.yaml"))
        .into_iter()
        .map(|(name, manifest)| RunDoc {
            config: of(&configs, "run-configs.yaml", &name),
            events: of(&events, "run-events.yaml", &name),
            manifest,
            name,
        })
        .collect()
}

/// The name of every run document the freeze holds.
fn run_names() -> Vec<String> {
    documents(&fixtures_dir().join("manifests.yaml"))
        .into_iter()
        .map(|(n, _)| n)
        .collect()
}

fn write_run_docs(root: &Path, name: &str, manifest_text: &str, config_text: &str) -> PathBuf {
    let dir = root.join(".apb/runs").join(name);
    std::fs::create_dir_all(&dir).expect("the run dir");
    std::fs::write(dir.join("manifest.yaml"), manifest_text).expect("the manifest writes");
    std::fs::write(dir.join("run.yaml"), config_text).expect("the run config writes");
    dir
}

fn read_manifest(run_dir: &Path, name: &str) -> RunExecutionManifest {
    manifest::read(run_dir)
        .unwrap_or_else(|e| panic!("the frozen manifest of {name} does not read: {e}"))
        .unwrap_or_else(|| panic!("the frozen manifest of {name} is absent"))
}

fn read_config(run_dir: &Path, name: &str) -> RunConfig {
    run_config::read_run_config(run_dir)
        .unwrap_or_else(|e| panic!("the frozen run config of {name} does not read: {e}"))
}

/// The frozen start lines of a run, each decoded by the journal's own
/// event type and checked to keep every member it holds.
fn read_events(name: &str, text: &str) -> Vec<Event> {
    text.lines()
        .filter(|l| !l.trim().is_empty())
        .map(|line| {
            let event: Event = serde_json::from_str(line)
                .unwrap_or_else(|e| panic!("a frozen start line of {name} does not decode: {e}"));
            let raw: serde_json::Value = serde_json::from_str(line).expect("the line is JSON");
            let kept = serde_json::to_value(&event).expect("the event serializes");
            if let Some(path) = dropped_json_paths(&raw, &kept, "").first() {
                panic!(
                    "the frozen start line of {name} holds {path}, which the decoded event does not keep"
                );
            }
            event
        })
        .collect()
}

/// The paths of `raw` the decoded value does not keep, walked
/// structurally like `dropped_paths`.
fn dropped_json_paths(
    raw: &serde_json::Value,
    kept: &serde_json::Value,
    prefix: &str,
) -> Vec<String> {
    let mut out = Vec::new();
    match (raw, kept) {
        (serde_json::Value::Object(m), serde_json::Value::Object(k)) => {
            for (key, v) in m {
                let path = if prefix.is_empty() {
                    key.clone()
                } else {
                    format!("{prefix}.{key}")
                };
                match k.get(key) {
                    None => out.push(path),
                    Some(kv) => out.extend(dropped_json_paths(v, kv, &path)),
                }
            }
        }
        (serde_json::Value::Array(a), serde_json::Value::Array(b)) => {
            for (i, v) in a.iter().enumerate() {
                let path = format!("{prefix}[{i}]");
                match b.get(i) {
                    None => out.push(path),
                    Some(kv) => out.extend(dropped_json_paths(v, kv, &path)),
                }
            }
        }
        _ => {}
    }
    out
}

/// Every run document reads through its reader, keeps every key it holds
/// in the reader's result serialized again, and meets its expectation.
#[test]
fn every_run_document_reads_and_keeps_its_keys() {
    for doc in run_documents() {
        let tmp = tempfile::tempdir().expect("the run temp dir");
        let dir = write_run_docs(tmp.path(), &doc.name, &doc.manifest, &doc.config);
        let manifest = read_manifest(&dir, &doc.name);
        let config = read_config(&dir, &doc.name);
        keys_kept(
            &fixtures_dir().join("manifests.yaml"),
            &doc.name,
            &doc.manifest,
            &typed(&manifest),
        );
        keys_kept(
            &fixtures_dir().join("run-configs.yaml"),
            &doc.name,
            &doc.config,
            &typed(&config),
        );
        let events = read_events(&doc.name, &doc.events);
        let expectation = EXPECTATIONS
            .iter()
            .find(|(n, _)| *n == doc.name)
            .map(|(_, f)| *f)
            .unwrap_or_else(|| panic!("the run document {} has no expectation", doc.name));
        expectation(&manifest, &config, &events);
    }
}

/// Every frozen start line is of a start-decision type, and every run
/// freezes its provenance.
#[test]
fn every_run_freezes_its_start_decisions() {
    for doc in run_documents() {
        let events = read_events(&doc.name, &doc.events);
        for e in &events {
            let tag = serde_json::to_value(e).expect("serializes")["type"]
                .as_str()
                .expect("a type")
                .to_string();
            assert!(
                START_EVENT_TYPES.contains(&tag.as_str()),
                "run-events.yaml holds a {tag} line for {}",
                doc.name
            );
        }
        assert!(
            events
                .iter()
                .any(|e| matches!(e.payload, EventPayload::RunProvenance { .. })),
            "the run {} freezes no run_provenance",
            doc.name
        );
    }
}

/// No frozen run document holds the machine it was written on: no
/// temporary or home directory, no platform prefix of a canonical path.
/// Every executable path names [`FREEZE_PROJECT_ROOT`].
#[test]
fn no_run_document_holds_machine_state() {
    for doc in run_documents() {
        for (file, text) in [
            ("manifests.yaml", &doc.manifest),
            ("run-configs.yaml", &doc.config),
            ("run-events.yaml", &doc.events),
        ] {
            for marker in ["/private/", "/tmp/", "/var/folders/", "/home/", "/Users/"] {
                assert!(
                    !text.contains(marker),
                    "the frozen {file} document {} holds the machine path {marker}",
                    doc.name
                );
            }
        }
        for line in doc.manifest.lines() {
            if let Some(path) = line.trim().strip_prefix("canonical_executable: ") {
                assert!(
                    path.starts_with(&format!("{FREEZE_PROJECT_ROOT}/")),
                    "{}: the executable path {path} is not under {FREEZE_PROJECT_ROOT}",
                    doc.name
                );
            }
        }
    }
}

/// A document without an expectation fails; the writer never writes an
/// expectation.
#[test]
fn every_run_document_has_an_expectation() {
    let known: Vec<&str> = EXPECTATIONS.iter().map(|(n, _)| *n).collect();
    for name in run_names() {
        assert!(
            known.contains(&name.as_str()),
            "the frozen run document {name} has no expectation: a document without one asserts nothing about what its reader decides"
        );
    }
    for name in known {
        assert!(
            run_names().iter().any(|n| n == name),
            "the expectation {name} names no frozen run document"
        );
    }
}

type RunExpectation = fn(&RunExecutionManifest, &RunConfig, &[Event]);

/// The expectations: named typed values and the decisions the readers
/// make (`RunOrigin::of_read`, `external_data::is_flagged`, the
/// unattended mode, the budget and the subscription in force, the
/// deadline in force, the concurrency key, the run limits).
const EXPECTATIONS: &[(&str, RunExpectation)] = &[
    ("plain", expect_plain),
    ("guarded", expect_guarded),
    ("grants", expect_grants),
    ("unattended", expect_unattended),
    ("triggered", expect_triggered),
    ("child", expect_child),
];

fn origin_of(m: &RunExecutionManifest) -> manifest::RunOrigin {
    manifest::RunOrigin::of_read(&Ok(Some(m.clone())))
}

fn provenance(events: &[Event]) -> &EventPayload {
    events
        .iter()
        .map(|e| &e.payload)
        .find(|p| matches!(p, EventPayload::RunProvenance { .. }))
        .expect("the run freezes its provenance")
}

/// The run limits a start records from its profile: one profile, `main`,
/// whose executor runs one attempt at a time.
fn expect_run_limits(events: &[Event]) {
    let limits = apb_engine::run_view::run_limits(events);
    let rendered = serde_json::to_value(&limits).expect("the limits serialize");
    assert_eq!(
        rendered,
        serde_json::to_value(match provenance(events) {
            EventPayload::RunProvenance { limits, .. } => limits,
            _ => unreachable!(),
        })
        .unwrap(),
        "the run limits read from the frozen provenance"
    );
}

fn expect_plain(m: &RunExecutionManifest, c: &RunConfig, events: &[Event]) {
    assert_eq!(
        origin_of(m),
        manifest::RunOrigin::ExternalData,
        "the external-data run's origin reads as frozen"
    );
    assert!(m.external_data, "the external-data flag is frozen");
    assert!(!m.unattended, "a plain run is not unattended");
    assert_eq!(m.secret_params, vec!["title".to_string()]);
    assert_eq!(c.params.get("title").map(String::as_str), Some("t"));
    assert_eq!(c.parent_run, None, "a top-level run");
    // The budget in force: the start's tokens, continue without an
    // estimate.
    let budget = apb_engine::budget::BudgetFold::of(events).expect("the budget is frozen");
    assert_eq!(
        budget.limit.tokens,
        Some(100_000),
        "the token limit in force"
    );
    assert_eq!(budget.limit.usd, None, "no dollar limit");
    assert_eq!(
        budget.on_no_estimate,
        apb_core::budget::OnNoEstimate::Continue,
        "the frozen on_no_estimate"
    );
    // The subscription in force: the frozen target and outcomes.
    let notify = apb_engine::notify::NotifyState::fold(events);
    let active = notify.active.expect("the notification is on");
    assert_eq!(active.target.connector, "mock-tracker");
    assert_eq!(active.target.account, "main");
    assert_eq!(active.target.function, "list_items");
    assert_eq!(active.target.to, "ops@example.com");
    assert_eq!(
        active.finished,
        vec![
            apb_core::notify::FinishOutcome::Failed,
            apb_core::notify::FinishOutcome::Aborted
        ],
        "the outcomes notified"
    );
    assert_eq!(
        apb_engine::pause::clock::in_force(events),
        None,
        "a plain run has no deadline"
    );
    expect_run_limits(events);
}

fn expect_guarded(m: &RunExecutionManifest, _c: &RunConfig, events: &[Event]) {
    assert!(!m.unattended, "a guarded run is not unattended");
    assert_eq!(
        origin_of(m),
        manifest::RunOrigin::Production,
        "a guarded run is production"
    );
    let deadline = apb_engine::pause::clock::in_force(events).expect("the deadline is frozen");
    assert_eq!(deadline.total_ms, 2_700_000, "the deadline in force: 45m");
    let EventPayload::RunProvenance { concurrency, .. } = provenance(events) else {
        unreachable!()
    };
    let concurrency = concurrency.as_ref().expect("the concurrency key is frozen");
    assert_eq!(concurrency.display, "freeze-guarded", "the key as shown");
    assert_eq!(concurrency.on_busy, "queue_latest", "the frozen on_busy");
    assert!(
        !concurrency.key_digest.is_empty(),
        "the slot identity is frozen"
    );
    assert!(
        apb_engine::budget::BudgetFold::of(events).is_none(),
        "no budget"
    );
    expect_run_limits(events);
}

fn expect_grants(m: &RunExecutionManifest, c: &RunConfig, events: &[Event]) {
    let tracker = m
        .connectors
        .iter()
        .find(|c| c.name == "mock-tracker")
        .expect("the tracker connector is frozen");
    let accounts: Vec<&str> = tracker.accounts.iter().map(|a| a.name.as_str()).collect();
    assert_eq!(accounts, vec!["main"], "the connector's accounts");
    let grant = |site: &str| {
        m.connector_grants
            .get(site)
            .unwrap_or_else(|| panic!("the grant of {site} is frozen"))
            .iter()
            .map(|g| (g.connector.clone(), g.accounts.clone(), g.functions.clone()))
            .collect::<Vec<_>>()
    };
    assert_eq!(
        grant("act"),
        vec![(
            "mock-tracker".to_string(),
            vec!["main".to_string()],
            vec!["inbox_read".to_string()]
        )],
        "the agent node's grant"
    );
    assert_eq!(
        grant("wait#inbox"),
        vec![("mock-tracker".to_string(), vec!["main".to_string()], vec![])],
        "the inbox wait's grant"
    );
    assert_eq!(
        c.expected_connectors.get("mock-tracker"),
        Some(&tracker.digest),
        "the run expects the connector it snapshotted"
    );
    let launch = m.launch.as_ref().expect("the launch record is frozen");
    assert_eq!(launch.initiator, "cli_terminal", "a person's terminal");
    expect_run_limits(events);
}

fn expect_unattended(m: &RunExecutionManifest, c: &RunConfig, events: &[Event]) {
    assert!(m.unattended, "the unattended flag is frozen");
    assert!(m.probation_waived, "the probation waiver is frozen");
    assert!(c.unattended, "the run config reads unattended");
    assert_eq!(
        origin_of(m),
        manifest::RunOrigin::Production,
        "an unattended run is production"
    );
    expect_run_limits(events);
}

fn expect_triggered(m: &RunExecutionManifest, _c: &RunConfig, events: &[Event]) {
    let launch = m.launch.as_ref().expect("the launch record is frozen");
    assert_eq!(launch.initiator, "trigger:triage-opened-issues");
    let by = launch
        .triggered_by
        .as_ref()
        .expect("the rule that started the run is frozen");
    assert_eq!(by.rule, "triage-opened-issues", "the rule name is frozen");
    assert_eq!(by.incarnation, "inc-1", "the incarnation is frozen");
    assert_eq!(by.source, "inbox", "the source is frozen");
    assert_eq!(by.connector.as_deref(), Some("mock-tracker"));
    assert_eq!(by.account.as_deref(), Some("main"));
    assert_eq!(by.event_seq, Some(41), "the event is frozen");
    assert!(m.unattended, "a rule's run is unattended");
    let EventPayload::RunProvenance { start_key, .. } = provenance(events) else {
        unreachable!()
    };
    // The provenance shows the first characters of the key's MAC, never
    // the key: `trigger:inc-1:41` under the freeze's `TEST_KEY`.
    assert_eq!(
        start_key.as_deref(),
        Some("ebe625cb3725"),
        "the start key's shown tag"
    );
    assert_eq!(
        start_key.as_deref().map(str::len),
        Some(apb_core::start_key::SHOWN_TAG_CHARS),
        "a shown tag, not the key"
    );
    expect_run_limits(events);
}

fn expect_child(m: &RunExecutionManifest, c: &RunConfig, events: &[Event]) {
    assert_eq!(
        c.parent_run.as_deref(),
        Some("plain"),
        "the parent run is frozen"
    );
    let consent = m.consent.as_ref().expect("the consent is frozen");
    assert_eq!(
        consent.inherited_from.as_deref(),
        Some("plain"),
        "the inherited consent names its parent"
    );
    assert!(
        consent.irreversible,
        "the inherited consent covers irreversible"
    );
    assert_eq!(consent.by, "cli_flag", "the consent's channel");
    expect_run_limits(events);
}

// ---------------------------------------------------------------------------
// The signed stores
// ---------------------------------------------------------------------------

/// One entry per `signed_store::SignedDoc` implementor the freeze holds:
/// `(kind, path under the project or config dir)`.
fn store_entries() -> Vec<(String, String)> {
    let text = fixture(&signed_dir().join("stores.json"));
    let value: serde_json::Value = serde_json::from_str(&text).expect("stores.json parses");
    let mut out = Vec::new();
    if let Some(map) = value.as_object() {
        for (kind, entry) in map {
            let path = entry["path"].as_str().expect("a path").to_string();
            out.push((kind.clone(), path));
        }
    }
    out.sort();
    out
}

/// The store kinds this build writes. A new `SignedDoc` implementor joins
/// here with its expectation.
const STORE_KINDS: &[&str] = &[
    "start-keys",
    "concurrency-slots",
    "health-state",
    "probation",
    "known-problems",
    "corrections",
    "trigger-rules",
    "trigger-state",
];

/// Every store kind is frozen, and every frozen entry names a kind this
/// build knows.
#[test]
fn every_store_kind_is_frozen() {
    for kind in STORE_KINDS {
        assert!(
            store_entries().iter().any(|(k, _)| k == kind),
            "the signed store {kind} has no frozen entry: run the persistence writer"
        );
    }
    for (kind, _) in store_entries() {
        assert!(
            STORE_KINDS.contains(&kind.as_str()),
            "stores.json holds the kind {kind}, which this build does not know"
        );
    }
}

type StoreExpectation = fn(&Path);

/// Each frozen store file, placed at its path, verifies, reads through
/// the store's own reader, and meets its expectation.
const STORE_EXPECTATIONS: &[(&str, StoreExpectation)] = &[
    ("start-keys", expect_start_keys as StoreExpectation),
    ("concurrency-slots", expect_slots as StoreExpectation),
    ("health-state", expect_health as StoreExpectation),
    ("probation", expect_probation as StoreExpectation),
    ("known-problems", expect_problems as StoreExpectation),
    ("corrections", expect_corrections as StoreExpectation),
    ("trigger-rules", expect_rules as StoreExpectation),
    ("trigger-state", expect_trigger_state as StoreExpectation),
];

/// Every store expectation exists and names a frozen entry.
#[test]
fn every_store_has_an_expectation() {
    for kind in STORE_KINDS {
        assert!(
            STORE_EXPECTATIONS.iter().any(|(k, _)| k == kind),
            "the signed store {kind} has no expectation"
        );
    }
    for (kind, _) in store_entries() {
        assert!(
            STORE_KINDS.contains(&kind.as_str()),
            "stores.json holds the unknown kind {kind}"
        );
    }
}

/// The frozen stores read back with their decisions.
#[test]
fn every_frozen_store_reads_and_decides() {
    for (kind, path) in store_entries() {
        let tmp = tempfile::tempdir().expect("the store temp dir");
        let config_dir = tmp.path().join("config");
        let root = tmp.path().join("project");
        std::fs::create_dir_all(&config_dir).expect("the config dir");
        std::fs::create_dir_all(&root).expect("the project dir");
        // The key file is the test seam: every MAC of a signed store goes
        // through `run_origin::tag` and `verify_tag`, which read
        // `<config dir>/run-origin.key`.
        std::fs::write(config_dir.join("run-origin.key"), TEST_KEY).expect("the key writes");
        // The frozen file names the project that owns it (its MAC covers
        // the name): a workspace id in `workspace.local` the registry does
        // not need to know. The writer wrote `TEST_WORKSPACE_ID`; the
        // reader plants the same one, so the file belongs here and
        // nowhere else.
        std::fs::create_dir_all(root.join(".apb")).expect("the .apb dir");
        std::fs::write(root.join(".apb/workspace.local"), TEST_WORKSPACE_ID)
            .expect("the workspace id writes");
        let text = {
            let all = fixture(&signed_dir().join("stores.json"));
            let value: serde_json::Value = serde_json::from_str(&all).expect("parses");
            value[&kind]["bytes"].as_str().expect("bytes").to_string()
        };
        let full = if path.starts_with(".apb/") {
            root.join(path.as_str())
        } else {
            config_dir.join(path.as_str())
        };
        if let Some(parent) = full.parent() {
            std::fs::create_dir_all(parent).expect("the store dir");
        }
        std::fs::write(&full, &text).expect("the store bytes write");
        let lock = crate::common::env_lock();
        // SAFETY: env mutation is serialized by ENV_LOCK, held by the guard.
        unsafe { std::env::set_var("APB_CONFIG_DIR", &config_dir) };
        // `belongs_to` reads the project the file names as the workspace id
        // registered under the config dir: register the same fixed one.
        registered_project(&root, &config_dir);
        let expectation = STORE_EXPECTATIONS
            .iter()
            .find(|(k, _)| k == &kind)
            .map(|(_, f)| *f)
            .expect("every kind has an expectation");
        expectation(&root);
        drop(lock);
    }
}

/// The fixed 64-character installation key of the freeze.
const TEST_KEY: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

/// The workspace id the frozen stores name as their project and the reader
/// plants (fixed, so a store belongs to the project that reads it and to
/// no other).
const TEST_WORKSPACE_ID: &str = "ws-feedface000000000000000000000000000000000000000000000000000000";

fn expect_start_keys(root: &Path) {
    let loaded =
        signed_store::read::<apb_core::start_key::StartKeysFile>(root, Scope::Project, "deploy")
            .expect("the start keys read");
    assert!(loaded.signed, "the frozen start keys verify");
    let bound = loaded
        .file
        .entries
        .iter()
        .find(|e| e.state == apb_core::start_key::EntryState::Bound)
        .expect("a bound entry is frozen");
    assert_eq!(
        bound.run_id.as_deref(),
        Some("deploy-2026010100000000"),
        "the bound run reads as frozen"
    );
    let pending = loaded
        .file
        .entries
        .iter()
        .find(|e| e.state == apb_core::start_key::EntryState::Pending)
        .expect("a pending entry is frozen");
    assert_eq!(
        pending.key, "trigger:inc-2:1",
        "the pending key reads as frozen"
    );
}

fn expect_slots(root: &Path) {
    let loaded = signed_store::read::<apb_engine::concurrency::slots::ConcurrencyFile>(
        root,
        Scope::Project,
        "deploy",
    )
    .expect("the slots read");
    assert!(loaded.signed, "the frozen slots verify");
    let slot = loaded.file.slots.first().expect("a slot is frozen");
    assert_eq!(
        slot.active.as_ref().map(|h| h.run_id.as_str()),
        Some("deploy-2026010100000100"),
        "the active run reads as frozen"
    );
    assert_eq!(
        slot.waiting.as_ref().map(|h| h.run_id.as_str()),
        Some("deploy-2026010100000200"),
        "the waiting run reads as frozen"
    );
}

fn expect_health(root: &Path) {
    use apb_core::health::class_of;
    let loaded =
        signed_store::read::<apb_engine::health::state::HealthFile>(root, Scope::Project, "deploy")
            .expect("the health state reads");
    assert!(loaded.signed, "the frozen health state verifies");
    let hold = loaded.file.hold.as_ref().expect("the hold is frozen");
    assert_eq!(
        hold.rule, "consecutive_failures",
        "the hold's rule is frozen"
    );
    assert!(
        !loaded.file.notices.is_empty(),
        "the notice ledger is not empty"
    );
    // The decision the drive makes: a held playbook refuses starts.
    let refusal = apb_engine::health::state::hold_refusal(root, Scope::Project, "deploy");
    let value = refusal.expect_err("a held playbook refuses starts");
    assert_eq!(
        value["policy"].as_str(),
        Some("playbook_on_hold"),
        "the hold refusal is frozen"
    );
    let _ = class_of;
}

fn expect_probation(root: &Path) {
    let loaded =
        signed_store::read::<apb_core::probation::ProbationFile>(root, Scope::Project, "deploy")
            .expect("the probation reads");
    assert!(loaded.signed, "the frozen probation verifies");
    let counted = apb_core::probation::counted(&loaded.file, "d1", "p1");
    assert_eq!(counted, 1, "the frozen eligible successes count as frozen");
    assert!(
        loaded.file.entries.iter().any(|e| !e.eligible),
        "the ineligible entry is frozen"
    );
}

fn expect_problems(root: &Path) {
    let loaded = signed_store::read::<apb_core::known_problems::store::ProblemFile>(
        root,
        Scope::Project,
        "deploy",
    )
    .expect("the problems read");
    assert!(loaded.signed, "the frozen problems verify");
    let problem = loaded
        .file
        .problems
        .iter()
        .find(|p| p.id == "the-shell-exits-1")
        .expect("the known problem is frozen");
    let check = problem
        .check
        .as_ref()
        .expect("the recheck record is frozen");
    assert_eq!(
        check.verdict,
        apb_core::known_problems::RecheckVerdict::ClosedProposed,
        "the recheck's verdict reads as frozen"
    );
}

fn expect_corrections(root: &Path) {
    let loaded =
        signed_store::read::<apb_core::corrections::CorrectionFile>(root, Scope::Project, "deploy")
            .expect("the corrections read");
    assert!(loaded.signed, "the frozen corrections verify");
    let entry = loaded
        .file
        .entries
        .iter()
        .find(|e| e.status == apb_core::corrections::CorrectionStatus::Proposed)
        .expect("a proposed correction is frozen");
    assert!(entry.text.is_some(), "the draft wording is frozen");
}

fn expect_rules(root: &Path) {
    let loaded = signed_store::read::<apb_core::trigger::RulesFile>(
        root,
        Scope::Project,
        "triage-opened-issues",
    )
    .expect("the rules read");
    assert!(loaded.signed, "the frozen rules verify");
    let entry = loaded.file.rules.first().expect("a rule is frozen");
    assert!(entry.enabled, "the rule reads enabled");
    let approval = entry.approval.as_ref().expect("the approval is frozen");
    assert!(
        !approval.binding.is_empty(),
        "the approval's binding reads as frozen"
    );
    let apb_core::trigger::RuleSource::Inbox(src) = &entry.rule.source;
    assert_eq!(
        src.connector, "mock-tracker",
        "the rule's connector is frozen"
    );
}

fn expect_trigger_state(root: &Path) {
    let loaded = signed_store::read::<apb_core::trigger::TriggerStateFile>(
        root,
        Scope::Project,
        "triage-opened-issues",
    )
    .expect("the trigger state reads");
    assert!(loaded.signed, "the frozen trigger state verifies");
    let state = loaded.file.rules.first().expect("a state is frozen");
    assert_eq!(
        state.seen_through, 41,
        "the position reads past the frozen event"
    );
}

// ---------------------------------------------------------------------------
// The writer
// ---------------------------------------------------------------------------

/// The stub agent: answers with a report block, so a run completes.
fn stub_agent(root: &Path) -> PathBuf {
    let path = root.join("stub-agent.sh");
    crate::common::write_sync(
        &path,
        "#!/bin/sh\ncat >/dev/null\necho 'report: done'\necho '## Report'\necho done\n",
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perm = std::fs::metadata(&path).unwrap().permissions();
        perm.set_mode(0o755);
        std::fs::set_permissions(&path, perm).unwrap();
    }
    path
}

struct RunRig {
    dir: tempfile::TempDir,
    _vars: Vec<crate::common::EnvVar>,
    _lock: std::sync::MutexGuard<'static, ()>,
}

fn rig(id: &str, playbook_body: &str) -> RunRig {
    let lock = crate::common::env_lock();
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("project");
    let config_dir = dir.path().join("config");
    std::fs::create_dir_all(&root).unwrap();
    std::fs::create_dir_all(&config_dir).unwrap();
    let mut vars = vec![
        crate::common::EnvVar::set("APB_CONFIG_DIR", &config_dir),
        crate::common::EnvVar::set("APB_AGENT_CMD", stub_agent(&root)),
        crate::common::EnvVar::remove("APB_RUN_ID"),
        crate::common::EnvVar::remove("APB_HOST_TASK"),
    ];
    vars.push(crate::common::EnvVar::set("APB_DECISIONS_ALLOW_FAKE", "1"));
    // The fixed installation key, so every MAC a run records (the
    // concurrency key's slot identity) is the same on every machine.
    std::fs::write(config_dir.join("run-origin.key"), TEST_KEY).unwrap();
    apb_core::run_origin::ensure_key().unwrap();
    init_project(&root).unwrap();
    let vdir = root.join(".apb/playbooks").join(id).join("1.0.0");
    std::fs::create_dir_all(vdir.join("scripts")).unwrap();
    std::fs::write(vdir.join("playbook.yaml"), playbook_body).unwrap();
    std::fs::write(
        root.join(".apb/playbooks").join(id).join("current"),
        "1.0.0",
    )
    .unwrap();
    crate::common::seed_main(&root);
    RunRig {
        dir,
        _vars: vars,
        _lock: lock,
    }
}

fn base_playbook(id: &str, extra: &str) -> String {
    format!(
        "schema: 2\nid: {id}\nname: {id}\nversion: 1.0.0\ndefaults:\n  profile: main\nparams:\n  - {{ name: title, type: text, secret: true }}\nnodes:\n  - {{ id: start, type: start }}\n  - {{ id: act, type: agent_task, prompt: \"do the work\" }}\n  - {{ id: done, type: finish, outcome: success }}\nedges:\n  - {{ from: start, to: act }}\n  - {{ from: act, to: done }}\n{extra}"
    )
}

/// Installs the freeze's `mock-tracker` connector in `root` with its
/// account in `config_dir`, and approves it in the freeze's trust store,
/// so a run that names it (a notify target, a binding) passes its checks.
fn install_mock_tracker(root: &Path, config_dir: &Path) {
    let cdir = root.join(".apb/connectors/mock-tracker");
    std::fs::create_dir_all(&cdir).unwrap();
    std::fs::write(
        cdir.join("connector.yaml"),
        "name: mock-tracker\nversion: 0.1.0\nauth:\n  kind: header\n  header: Authorization\n  value_template: \"Bearer {{secret.token}}\"\nwebhook:\n  event_type_header: X-Mock-Event\n  signature:\n    scheme: hmac_sha256_hex\n    header: X-Mock-Signature\n    secret: \"{{secret.token}}\"\naccount_fields:\n  - name: base_url\n    required: true\n  - name: token\n    required: true\n    secret: true\nfunctions:\n  - name: list_items\n    description: List items\n    method: POST\n    url: \"{{account.base_url}}/items\"\n    args_schema:\n      type: object\n      properties:\n        to: { type: string, description: the recipient }\n        text: { type: string, description: the message }\n      required: [to, text]\n    people: { to: to, text: text }\n    idempotency: { mode: none, result_ref: id }\n  - name: inbox_read\n    description: Read pending events\n    read_only: true\n    inbox:\n      op: read\n",
    )
    .unwrap();
    std::fs::create_dir_all(config_dir.join("connector-config")).unwrap();
    std::fs::write(
        config_dir.join("connector-config/mock-tracker.yaml"),
        "accounts:\n  - name: main\n    default: true\n    base_url: http://127.0.0.1:9\n    token: \"{{env.APB_CC_TEST_TOKEN}}\"\n",
    )
    .unwrap();
    std::fs::write(root.join(".apb/secrets.env"), "APB_CC_TEST_TOKEN=t\n").unwrap();
    let cfg_connectors = config_dir.join("connectors/mock-tracker");
    std::fs::create_dir_all(&cfg_connectors).unwrap();
    std::fs::copy(
        root.join(".apb/connectors/mock-tracker/connector.yaml"),
        cfg_connectors.join("connector.yaml"),
    )
    .unwrap();
    let loaded = apb_core::connector::store::load("mock-tracker").expect("the connector loads");
    let accounts =
        apb_core::connector::config::load_merged(root, "mock-tracker").expect("the accounts read");
    let digest = apb_core::connector::config::account_digest(&accounts[0]);
    let mut trust = apb_core::trust::TrustStore::load();
    trust
        .approve_kind(
            &loaded.digest,
            "mock-tracker",
            apb_core::trust::Kind::Connector,
            apb_core::trust::OriginKind::LocallyApproved,
        )
        .expect("the connector is approved");
    trust
        .approve_kind(
            &digest,
            &apb_core::trust::account_trust_id("mock-tracker", "main"),
            apb_core::trust::Kind::ConnectorAccount,
            apb_core::trust::OriginKind::LocallyApproved,
        )
        .expect("the account is approved");
}

/// One entry per store kind, each written through its public write
/// function under the fixed key, with the nondefault state of the
/// design.
fn write_stores(root: &Path, config_dir: &Path) -> Vec<(String, String, String)> {
    ensure_test_key(config_dir);
    name_project(root);
    let project = registered_project(root, config_dir).expect("the project registers");
    let mut out = Vec::new();
    let scope = Scope::Project;
    // 1. Start keys: one bound to a run, one pending.
    signed_store::update::<apb_core::start_key::StartKeysFile, _, apb_core::signed_store::StoreError>(
        root,
        scope,
        "deploy",
        |f| {
            f.entries.push(apb_core::start_key::StartKeyEntry {
                key: "trigger:inc-1:7".to_string(),
                request: "req-digest-1".to_string(),
                state: apb_core::start_key::EntryState::Bound,
                reservation: None,
                run_id: Some("deploy-2026010100000000".to_string()),
                at_ms: 1_790_000_000_000,
                reused: 0,
            });
            f.entries.push(apb_core::start_key::StartKeyEntry {
                key: "trigger:inc-2:1".to_string(),
                request: "req-digest-2".to_string(),
                state: apb_core::start_key::EntryState::Pending,
                reservation: Some("res-1".to_string()),
                run_id: None,
                at_ms: 1_790_000_100_000,
                reused: 0,
            });
            Ok(((), true))
        },
    )
    .expect("the start keys write");
    out.push(store_bytes(
        "start-keys",
        root,
        apb_core::start_key::StartKeysFile::DIR,
        "deploy",
    ));
    // 2. Concurrency slots: an active and a waiting run.
    signed_store::update::<
        apb_engine::concurrency::slots::ConcurrencyFile,
        _,
        apb_core::signed_store::StoreError,
    >(root, scope, "deploy", |f| {
        f.slots.push(apb_engine::concurrency::slots::Slot {
            key_digest: "kd1".to_string(),
            display: "main".to_string(),
            active: Some(apb_engine::concurrency::slots::Holder {
                run_id: "deploy-2026010100000100".to_string(),
                reservation: None,
            }),
            waiting: Some(apb_engine::concurrency::slots::Holder {
                run_id: "deploy-2026010100000200".to_string(),
                reservation: Some("res-wait".to_string()),
            }),
            stops: Vec::new(),
        });
        Ok(((), true))
    })
    .expect("the slots write");
    out.push(store_bytes(
        "concurrency-slots",
        root,
        apb_engine::concurrency::slots::ConcurrencyFile::DIR,
        "deploy",
    ));
    // 3. Health state: on hold with a notice in its ledger.
    signed_store::update::<
        apb_engine::health::state::HealthFile,
        _,
        apb_core::signed_store::StoreError,
    >(root, scope, "deploy", |f| {
        f.hold = Some(apb_engine::health::state::Hold {
            since_ms: 1_790_000_300_000,
            rule: "consecutive_failures".to_string(),
            detail: "3 failed runs".to_string(),
        });
        f.notices.push(apb_engine::health::state::NoticeRecord {
            id: "h-n1".to_string(),
            rule: "consecutive_failures".to_string(),
            kind: "trip".to_string(),
            recorded_ms: 1_790_000_300_000,
            subject: "playbook deploy".to_string(),
            text: "the failures tripped consecutive_failures".to_string(),
            target: apb_engine::notify::record::NotifyTargetRecord {
                connector: "mock-tracker".to_string(),
                account: "main".to_string(),
                function: "inbox_read".to_string(),
                to: "ops@example.com".to_string(),
                connector_digest: "cd1".to_string(),
                account_digest: "ad1".to_string(),
            },
            state: "sent".to_string(),
            claimant: None,
            claimed_ms: None,
            claim_generation: None,
            tries: 1,
            detail: None,
        });
        Ok(((), true))
    })
    .expect("the health state writes");
    out.push(store_bytes(
        "health-state",
        root,
        apb_engine::health::state::HealthFile::DIR,
        "deploy",
    ));
    // 4. Probation: one entry that counts, one that does not.
    signed_store::update::<apb_core::probation::ProbationFile, _, apb_core::signed_store::StoreError>(
        root,
        scope,
        "deploy",
        |f| {
            f.project = project.clone();
            f.entries.push(apb_core::probation::ProbationEntry {
                run: "deploy-2026010100000300".to_string(),
                version: "1.0.0".to_string(),
                trust_digest: "d1".to_string(),
                definition: "p1".to_string(),
                unattended: false,
                outcome: "succeeded".to_string(),
                eligible: true,
                ended_ms: 1_790_000_400_000,
            });
            f.entries.push(apb_core::probation::ProbationEntry {
                run: "deploy-2026010100000301".to_string(),
                version: "1.0.0".to_string(),
                trust_digest: "d1".to_string(),
                definition: "p1".to_string(),
                unattended: true,
                outcome: "succeeded".to_string(),
                eligible: false,
                ended_ms: 1_790_000_400_100,
            });
            Ok(((), true))
        },
    )
    .expect("the probation writes");
    out.push(store_bytes(
        "probation",
        root,
        apb_core::probation::ProbationFile::DIR,
        "deploy",
    ));
    // 5. Known problems: a close proposed by a recheck with its record.
    signed_store::update::<
        apb_core::known_problems::store::ProblemFile,
        _,
        apb_core::signed_store::StoreError,
    >(root, scope, "deploy", |f| {
        f.problems.push(apb_core::known_problems::KnownProblem {
            id: "the-shell-exits-1".to_string(),
            scope: apb_core::known_problems::ProblemScope {
                kind: apb_core::known_problems::ScopeKind::Node,
                name: "act".to_string(),
            },
            symptom: "the shell exits 1".to_string(),
            workaround: None,
            symptoms: Vec::new(),
            workarounds: Vec::new(),
            origin: apb_core::known_problems::ProblemOrigin::Finding {
                kind: "tool_problem".to_string(),
            },
            evidence: Vec::new(),
            runs: 2,
            first_seen_ms: 1_790_000_500_000,
            last_seen_ms: 1_790_000_500_100,
            source: None,
            status: apb_core::known_problems::ProblemStatus::Open,
            confirmed: None,
            decided: None,
            check: Some(apb_core::known_problems::RecheckRecord {
                digest: "sd1".to_string(),
                verdict: apb_core::known_problems::RecheckVerdict::ClosedProposed,
                basis: apb_core::known_problems::RecheckBasis::Person,
                quote: Some(apb_core::known_problems::Quote {
                    text: "the shell exits 1".to_string(),
                    lines: [3, 3],
                }),
                reason_code: "quote".to_string(),
                mode: None,
                at_ms: 1_790_000_500_200,
                by: "person".to_string(),
            }),
            checks: Vec::new(),
        });
        Ok(((), true))
    })
    .expect("the problems write");
    out.push(store_bytes(
        "known-problems",
        root,
        apb_core::known_problems::store::ProblemFile::DIR,
        "deploy",
    ));
    // 6. Corrections: proposed with a draft.
    signed_store::update::<
        apb_core::corrections::CorrectionFile,
        _,
        apb_core::signed_store::StoreError,
    >(root, scope, "deploy", |f| {
        f.entries.push(apb_core::corrections::Correction {
            id: "c-k1".to_string(),
            kind: apb_core::corrections::CorrectionKind::ReviewNote,
            node: Some("act".to_string()),
            gate: None,
            key: "k1".to_string(),
            text: Some("say the answer, not the plan".to_string()),
            texts: Vec::new(),
            runs: 2,
            runs_by_version: BTreeMap::from([("1.0.0".to_string(), 2)]),
            first_seen_ms: 1_790_000_550_000,
            last_seen_ms: 1_790_000_550_100,
            evidence: Vec::new(),
            via_supervisor: false,
            status: apb_core::corrections::CorrectionStatus::Proposed,
            fix: None,
            decided: None,
            last_version: "1.0.0".to_string(),
        });
        Ok(((), true))
    })
    .expect("the corrections write");
    out.push(store_bytes(
        "corrections",
        root,
        apb_core::corrections::CorrectionFile::DIR,
        "deploy",
    ));
    // 7. Trigger rules: one rule, enabled and approved.
    signed_store::update::<apb_core::trigger::RulesFile, _, apb_core::signed_store::StoreError>(
        root,
        scope,
        "triage-opened-issues",
        |f| {
            f.project = project.clone();
            f.rules.push(apb_core::trigger::RuleEntry {
                rule: apb_core::trigger::Rule {
                    name: "triage-opened-issues".to_string(),
                    playbook: apb_core::trigger::RulePlaybook {
                        id: "deploy".to_string(),
                        version: "1.0.0".to_string(),
                        scope: apb_core::trigger::RuleScope::Project,
                    },
                    source: apb_core::trigger::RuleSource::Inbox(apb_core::trigger::InboxSource {
                        connector: "mock-tracker".to_string(),
                        account: "main".to_string(),
                        types: vec!["issues".to_string()],
                        conditions: Vec::new(),
                        key: "{{event.seq}}".to_string(),
                    }),
                    params: BTreeMap::new(),
                    limits: apb_core::trigger::RuleLimits { max_per_hour: 5 },
                    budget: None,
                    external_data: true,
                    require_ready: true,
                    notify: None,
                },
                incarnation: "inc-1".to_string(),
                enabled: true,
                created_at: 1_790_000_600_000,
                updated_at: 1_790_000_600_100,
                updated_by: "terminal".to_string(),
                approval: Some(apb_core::trigger::Approval {
                    rule_digest: "rd1".to_string(),
                    playbook_digest: "pd1".to_string(),
                    binding: "b1".to_string(),
                    permit_digest: "pd1".to_string(),
                    launch_policy: "allow".to_string(),
                    consent_nonce: None,
                    approved_at: 1_790_000_600_100,
                    approved_by: "person".to_string(),
                }),
            });
            Ok(((), true))
        },
    )
    .expect("the rules write");
    out.push(store_bytes(
        "trigger-rules",
        root,
        apb_core::trigger::RulesFile::DIR,
        "triage-opened-issues",
    ));
    // 8. Trigger state: positioned past one event.
    signed_store::update::<
        apb_core::trigger::TriggerStateFile,
        _,
        apb_core::signed_store::StoreError,
    >(root, scope, "triage-opened-issues", |f| {
        f.project = project.clone();
        f.rules.push(apb_core::trigger::RuleState {
            rule: "triage-opened-issues".to_string(),
            incarnation: "inc-1".to_string(),
            source: apb_core::trigger::SourceId {
                connector: "mock-tracker".to_string(),
                account: "main".to_string(),
            },
            seen_through: 41,
        });
        Ok(((), true))
    })
    .expect("the trigger state writes");
    out.push(store_bytes(
        "trigger-state",
        root,
        apb_core::trigger::TriggerStateFile::DIR,
        "triage-opened-issues",
    ));
    out
}

fn ensure_test_key(config_dir: &Path) {
    std::fs::create_dir_all(config_dir).expect("the config dir");
    std::fs::write(config_dir.join("run-origin.key"), TEST_KEY).expect("the key writes");
}

/// Names the project at `root` with the freeze's workspace id, so a signed
/// store the writer produces belongs to it.
fn name_project(root: &Path) {
    std::fs::create_dir_all(root.join(".apb")).expect("the .apb dir");
    std::fs::write(root.join(".apb/workspace.local"), TEST_WORKSPACE_ID)
        .expect("the workspace id writes");
}

/// Names the project at `root` with the freeze's id and registers it in
/// `config_dir`'s registry by hand, so `registered_id` answers it
/// (`belongs_to` reads that). The registry is written directly, not through
/// `bind_id`: that reads the config dir from the environment and takes the
/// registry lock, and the writer runs while the rig holds other locks.
fn registered_project(root: &Path, config_dir: &Path) -> Option<String> {
    name_project(root);
    let id = apb_core::projects::registered_id_in(Some(config_dir), root);
    if id.is_some() {
        return id;
    }
    let path = config_dir.join("projects.json");
    let canon = std::fs::canonicalize(root).unwrap_or_else(|_| root.to_path_buf());
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);
    let mut file = std::fs::read_to_string(&path)
        .ok()
        .and_then(|raw| serde_json::from_str::<serde_json::Value>(&raw).ok())
        .unwrap_or_else(|| serde_json::json!({ "schema_version": 2, "entries": {} }));
    file["entries"][TEST_WORKSPACE_ID] = serde_json::json!({
        "workspace_id": TEST_WORKSPACE_ID,
        "path": canon.display().to_string(),
        "last_seen_ms": now,
        "state": { "kind": "active" },
    });
    std::fs::write(&path, serde_json::to_vec(&file).ok()?).ok()?;
    let id = apb_core::projects::registered_id_in(Some(config_dir), root);
    assert!(
        id.is_some(),
        "the freeze workspace id does not read back from the registry"
    );
    id
}

/// `(kind, path, bytes)` of the store file of `playbook`.
fn store_bytes(
    kind: &str,
    root: &Path,
    dir: &'static str,
    playbook: &str,
) -> (String, String, String) {
    let path = root
        .join(".apb")
        .join(dir)
        .join("project")
        .join(format!("{playbook}.json"));
    let bytes = std::fs::read_to_string(&path).expect("the store bytes read");
    (
        kind.to_string(),
        format!(".apb/{dir}/project/{playbook}.json"),
        bytes,
    )
}

/// The fixed executable path and fingerprint of every frozen manifest, and
/// the fixed run id of every frozen run document. The writer records the
/// fixture run's live values and rewrites them to these before it writes:
/// `canonical_executable` holds the agent binary's path under the writer's
/// temporary project root (one machine's temp dir), `executable_fingerprint`
/// is the file's `size:mtime_ms` on the machine that ran the fixture run,
/// and the run id the engine allocated ends in the allocation wall clock
/// (`freezechild-1795368816508`). All three are machine state a fixture
/// must not carry; the run's contract is the field's shape, and the reader
/// test reads every frozen document through the real reader, which parses
/// these fields as written data and never checks them against the disk.
const FREEZE_PROJECT_ROOT: &str = "/apb-freeze-project";
const FREEZE_FINGERPRINT: &str = "72:1700000000000";

/// Rewrites the machine state out of a recorded run document: the
/// temporary project root becomes [`FREEZE_PROJECT_ROOT`], every
/// `executable_fingerprint` value becomes [`FREEZE_FINGERPRINT`], and each
/// run id the engine allocated for the fixture run (they end in the
/// allocation wall clock, `freezechild-1795368816508`) becomes the fixed
/// name its `subs` pair names. All three are machine state a fixture must
/// not carry: the run's contract is the field's shape, never one
/// developer's temp dir, a file's timestamp, or the second a writer
/// happened to run. A writer step, not a `NORMALIZED` entry (D1.1 rule 3):
/// a `NORMALIZED` entry is for a value the serializer spells differently,
/// and here the values themselves are machine state the fixture replaces
/// before it freezes.
fn normalize_manifest(text: &str, root: &Path, subs: &[(&str, &str)]) -> String {
    // A path may be written as the project root or its canonical form (on
    // macOS `/var/folders/..` reads as `/private/var/folders/..`, and
    // `/tmp/..` as `/private/tmp/..`). The longer one is replaced first, so
    // the shorter never matches inside it and leaves a platform prefix.
    let canon = std::fs::canonicalize(root).unwrap_or_else(|_| root.to_path_buf());
    let mut projects = vec![
        root.to_string_lossy().to_string(),
        canon.to_string_lossy().to_string(),
    ];
    projects.sort_by_key(|p| std::cmp::Reverse(p.len()));
    projects.dedup();
    let mut out = String::with_capacity(text.len());
    for line in text.lines() {
        let trimmed = line.trim_start();
        let line = if let Some(_rest) = trimmed.strip_prefix("executable_fingerprint:") {
            let indent = &line[..line.len() - trimmed.len()];
            format!("{indent}executable_fingerprint: {FREEZE_FINGERPRINT}")
        } else {
            let mut replaced = line.to_string();
            for project in &projects {
                replaced = replaced.replace(project.as_str(), FREEZE_PROJECT_ROOT);
            }
            for (from, to) in subs {
                replaced = replaced.replace(from, to);
            }
            replaced
        };
        out.push_str(&line);
        out.push('\n');
    }
    out
}

/// The one rewrite the writer makes to a frozen run document: a document
/// recorded on macOS before the roots were replaced longest first holds
/// `/private` in front of [`FREEZE_PROJECT_ROOT`]. That prefix is machine
/// state, never run data, so it is dropped; nothing else of a frozen
/// document changes.
fn without_platform_prefix(text: &str) -> String {
    text.replace(
        &format!("/private{FREEZE_PROJECT_ROOT}"),
        FREEZE_PROJECT_ROOT,
    )
}

/// The start-decision lines of a run's journal ([`START_EVENT_TYPES`]),
/// each with its wall-clock `ts` replaced by `1790000000000 + seq` (the
/// journal fixture's convention) and the machine state rewritten out like
/// the run's other documents.
fn start_events_of(run_dir: &Path, root: &Path, subs: &[(&str, &str)]) -> String {
    let journal = std::fs::read_to_string(run_dir.join("events.jsonl")).expect("the journal reads");
    let mut out = String::new();
    for line in journal.lines() {
        let mut value: serde_json::Value = serde_json::from_str(line).expect("a journal line");
        let tag = value["type"].as_str().unwrap_or_default().to_string();
        if !START_EVENT_TYPES.contains(&tag.as_str()) {
            continue;
        }
        let seq = value["seq"].as_u64().expect("a seq");
        value["ts"] = serde_json::json!(1_790_000_000_000u64 + seq);
        out.push_str(&serde_json::to_string(&value).expect("the line serializes"));
        out.push('\n');
    }
    normalize_manifest(&out, root, subs)
}

/// The fixture run's documents with its machine state rewritten out: the
/// run's own allocated id becomes the document's name; `parent` names the
/// run id of a second run the document points at (the child's parent), to
/// become its own fixed name.
fn run_document_of(root: &Path, run_id: &str, name: &str, parent: Option<(&str, &str)>) -> RunDoc {
    let dir = root.join(".apb/runs").join(run_id);
    let manifest_text =
        std::fs::read_to_string(dir.join("manifest.yaml")).expect("the manifest reads");
    let config_text = std::fs::read_to_string(dir.join("run.yaml")).expect("the run config reads");
    let mut subs = vec![(run_id, name)];
    if let Some((from, to)) = parent {
        subs.push((from, to));
    }
    RunDoc {
        name: name.to_string(),
        manifest: normalize_manifest(&manifest_text, root, &subs),
        config: normalize_manifest(&config_text, root, &subs),
        events: start_events_of(&dir, root, &subs),
    }
}

/// The one writer of the run-file and signed-store group.
#[test]
fn write_freeze() {
    if !write_freeze_enabled() {
        return;
    }
    let mut run_docs: Vec<RunDoc> = Vec::new();
    let mut store_docs: Vec<(String, String, String)> = Vec::new();

    // 1. plain: a budget, a notify request, remembered input off and
    //    `external_data: true`.
    {
        let r = rig("freezeplain", &base_playbook("freezeplain", ""));
        let opts = apb_engine::scheduler::RunOptions {
            external_data: true,
            budget: Some(apb_core::budget::BudgetRequest {
                tokens: Some(100_000),
                usd: None,
                on_no_estimate: Some(apb_core::budget::OnNoEstimate::Continue),
            }),
            notify: Some(apb_core::notify::NotifyRequest::On {
                target: Some(apb_core::notify::NotifyTarget {
                    connector: "mock-tracker".to_string(),
                    account: "main".to_string(),
                    function: "list_items".to_string(),
                    to: "ops@example.com".to_string(),
                }),
                on: None,
                finished: Some(vec![
                    apb_core::notify::FinishOutcome::Failed,
                    apb_core::notify::FinishOutcome::Aborted,
                ]),
            }),
            params: BTreeMap::from([("title".to_string(), "t".to_string())]),
            ..Default::default()
        };
        install_mock_tracker(
            r.dir.path().join("project").as_path(),
            r.dir.path().join("config").as_path(),
        );
        let res = apb_engine::scheduler::run(
            r.dir.path().join("project").as_path(),
            "freezeplain",
            None,
            opts,
        )
        .expect("the plain run runs");
        run_docs.push(run_document_of(
            &r.dir.path().join("project"),
            &res.run_id,
            "plain",
            None,
        ));
        store_docs.extend(write_stores(
            r.dir.path().join("project").as_path(),
            r.dir.path().join("config").as_path(),
        ));
    }
    // 2. guarded: a `deadline` and a `concurrency:` block.
    {
        let r = rig(
            "freezeguarded",
            &base_playbook(
                "freezeguarded",
                "deadline: 45m\nconcurrency:\n  key: freeze-guarded\n  on_busy: queue_latest\n",
            ),
        );
        let opts = apb_engine::scheduler::RunOptions {
            deadline_ms: Some(2_700_000),
            params: BTreeMap::from([("title".to_string(), "t".to_string())]),
            ..Default::default()
        };
        let res = apb_engine::scheduler::run(
            r.dir.path().join("project").as_path(),
            "freezeguarded",
            None,
            opts,
        )
        .expect("the guarded run runs");
        run_docs.push(run_document_of(
            &r.dir.path().join("project"),
            &res.run_id,
            "guarded",
            None,
        ));
    }
    // 3. unattended.
    {
        let r = rig(
            "freezeunattended",
            &base_playbook(
                "freezeunattended",
                "unattended:\n  after_attended_successes: 1\n",
            ),
        );
        let opts = apb_engine::scheduler::RunOptions {
            unattended: true,
            waive_probation: true,
            params: BTreeMap::from([("title".to_string(), "t".to_string())]),
            ..Default::default()
        };
        let res = apb_engine::scheduler::run(
            r.dir.path().join("project").as_path(),
            "freezeunattended",
            None,
            opts,
        )
        .expect("the unattended run runs");
        run_docs.push(run_document_of(
            &r.dir.path().join("project"),
            &res.run_id,
            "unattended",
            None,
        ));
    }
    // 4. triggered: a rule's start key and its `TriggeredBy` on the launch
    //    context.
    {
        let r = rig("freezetrig", &base_playbook("freezetrig", ""));
        let root = r.dir.path().join("project");
        let by = apb_core::launch::TriggeredBy {
            rule: "triage-opened-issues".to_string(),
            incarnation: "inc-1".to_string(),
            source: "inbox".to_string(),
            connector: Some("mock-tracker".to_string()),
            account: Some("main".to_string()),
            event_seq: Some(41),
        };
        let fields = apb_core::start_key::StartRequestFields {
            v: apb_core::start_key::REQUEST_V,
            supervision: "autonomous".into(),
            cache: "auto".into(),
            unattended: true,
            ..Default::default()
        };
        let skr = apb_core::start_key::StartKeyRequest {
            key: apb_core::start_key::StartKey::parse("trigger:inc-1:41").unwrap(),
            reuse: false,
            request: apb_core::start_key::StartRequest::capture(&fields).unwrap(),
        };
        let wref = apb_core::scope::PlaybookRef {
            origin: apb_core::scope::Origin::Project { workspace_id: None },
            id: "freezetrig".to_string(),
            version: Some("1.0.0".to_string()),
        };
        // The gate refuses a trigger start without the approval binding it
        // resolves; the refusal names it, so the freeze takes it from there
        // (a person's approval would bind exactly this value) and asks
        // again with it set.
        let make_launch = |binding: Option<String>| {
            let mut ctx = apb_engine::gate::launch::LaunchContext::new(
                apb_engine::gate::launch::Initiator::Trigger {
                    by: Box::new(by.clone()),
                },
                apb_engine::gate::launch::LaunchKind::Start,
            );
            if let Some(d) = binding {
                ctx = ctx.with_approved_binding(d);
            }
            ctx
        };
        let binding = apb_engine::gate::check_run(&root, &wref, true, false, &make_launch(None))
            .expect_err("the unapproved trigger start is refused")["binding"]
            .as_str()
            .expect("the refusal names the binding")
            .to_string();
        let launch = make_launch(Some(binding));
        let permit = apb_engine::gate::check_run(&root, &wref, true, false, &launch)
            .expect("the trigger start passes the gate");
        let mut opts = apb_engine::scheduler::RunOptions {
            start_key: Some(skr),
            unattended: true,
            params: BTreeMap::from([("title".to_string(), "t".to_string())]),
            ..Default::default()
        };
        permit.apply(&mut opts);
        let res = apb_engine::scheduler::run(&root, "freezetrig", Some("1.0.0"), opts)
            .expect("the triggered run runs");
        run_docs.push(run_document_of(&root, &res.run_id, "triggered", None));
    }
    // 5. child: a parent with consent, then the child inheriting it.
    {
        let r = rig(
            "freezechild",
            &base_playbook("freezechild", "effects: [irreversible]\n"),
        );
        let root = r.dir.path().join("project");
        let parent = apb_engine::scheduler::run(
            &root,
            "freezechild",
            None,
            apb_engine::scheduler::RunOptions {
                consent: Some(apb_engine::consent::RunConsent {
                    irreversible: true,
                    by: "cli_flag".to_string(),
                    inherited_from: None,
                    sources: Vec::new(),
                }),
                params: BTreeMap::from([("title".to_string(), "t".to_string())]),
                ..Default::default()
            },
        )
        .expect("the parent runs");
        let child = apb_engine::scheduler::run(
            &root,
            "freezechild",
            None,
            apb_engine::scheduler::RunOptions {
                parent_run: Some(parent.run_id.clone()),
                consent: Some(apb_engine::consent::RunConsent {
                    irreversible: true,
                    by: "cli_flag".to_string(),
                    inherited_from: Some(parent.run_id.clone()),
                    sources: Vec::new(),
                }),
                params: BTreeMap::from([("title".to_string(), "t".to_string())]),
                ..Default::default()
            },
        )
        .expect("the child runs");
        run_docs.push(run_document_of(
            &root,
            &child.run_id,
            "child",
            // The parent's allocated id is wall-clock machine state; the
            // freeze freezes it as the fixed `plain` the child expectation
            // asserts (the parent run itself is not a frozen document).
            Some((&parent.run_id, "plain")),
        ));
    }
    // 6. grants: a connector binding and an inbox wait. The connector is
    //    installed in the project and approved in the freeze's trust
    //    store.
    {
        let r = rig(
            "freezegrants",
            "schema: 2\nid: freezegrants\nname: freezegrants\nversion: 1.0.0\ndefaults:\n  profile: main\nparams:\n  - { name: title, type: text, secret: true }\nnodes:\n  - { id: start, type: start }\n  - { id: act, type: agent_task, prompt: \"do the work\", connectors: [{ name: mock-tracker, functions: read_only }] }\n  - { id: wait, type: wait, wait_for: { type: inbox, connector: mock-tracker, account: main }, timeout_seconds: 1 }\n  - { id: done, type: finish, outcome: success }\nedges:\n  - { from: start, to: act }\n  - { from: act, to: wait }\n  - { from: wait, to: done, on_failure: route }\n",
        );
        let root = r.dir.path().join("project");
        install_mock_tracker(root.as_path(), r.dir.path().join("config").as_path());
        // The connector permit comes from the gate: a run that binds
        // connectors never starts without it (the engine refuses), so the
        // freeze goes through `check_run` like a surface does.
        let wref = apb_core::scope::PlaybookRef {
            origin: apb_core::scope::Origin::Project { workspace_id: None },
            id: "freezegrants".to_string(),
            version: Some("1.0.0".to_string()),
        };
        let launch = apb_engine::gate::launch::LaunchContext::new(
            apb_engine::gate::launch::Initiator::CliUnattested { terminal: true },
            apb_engine::gate::launch::LaunchKind::Start,
        );
        let permit = apb_engine::gate::check_run(&root, &wref, true, false, &launch)
            .expect("the grants start passes the gate");
        let mut opts = apb_engine::scheduler::RunOptions {
            params: BTreeMap::from([("title".to_string(), "t".to_string())]),
            ..Default::default()
        };
        permit.apply(&mut opts);
        let res = apb_engine::scheduler::run(&root, "freezegrants", Some("1.0.0"), opts)
            .expect("the grants run runs");
        run_docs.push(run_document_of(&root, &res.run_id, "grants", None));
    }

    // Merge: frozen documents kept byte for byte, new ones added, each
    // file in the frozen order with new names appended. The one rewrite of
    // a frozen document is `without_platform_prefix`. A frozen run that
    // has no start lines yet (`run-events.yaml` came after it) takes the
    // lines this writer recorded for its name.
    let frozen_file = |file: &str| -> Vec<(String, String)> {
        let path = fixtures_dir().join(file);
        if path.exists() {
            documents(&path)
        } else {
            Vec::new()
        }
    };
    let frozen_manifests = frozen_file("manifests.yaml");
    let frozen_configs = frozen_file("run-configs.yaml");
    let frozen_events = frozen_file("run-events.yaml");
    let merge =
        |frozen: &[(String, String)], fresh: &dyn Fn(&RunDoc) -> String| -> Vec<(String, String)> {
            let mut out: Vec<(String, String)> = frozen
                .iter()
                .map(|(n, t)| (n.clone(), without_platform_prefix(t)))
                .collect();
            for doc in &run_docs {
                if !out.iter().any(|(n, _)| *n == doc.name) {
                    out.push((doc.name.clone(), fresh(doc)));
                }
            }
            out
        };
    let manifests = merge(&frozen_manifests, &|d| d.manifest.clone());
    let configs = merge(&frozen_configs, &|d| d.config.clone());
    let events = merge(&frozen_events, &|d| d.events.clone());
    // A name in one file is in all three, in the same order.
    let names = |docs: &[(String, String)]| docs.iter().map(|(n, _)| n.clone()).collect::<Vec<_>>();
    assert_eq!(
        names(&manifests),
        names(&configs),
        "the run files list the same runs"
    );
    for (name, _) in &manifests {
        assert!(
            events.iter().any(|(n, _)| n == name),
            "no start lines for the run {name}"
        );
    }
    let render = |docs: &[(String, String)]| -> String {
        let mut out = String::new();
        for (name, text) in docs {
            out.push_str(MARKER);
            out.push_str(name);
            out.push('\n');
            out.push_str(text);
            if !out.ends_with('\n') {
                out.push('\n');
            }
        }
        out
    };
    let manifests = render(&manifests);
    let configs = render(&configs);
    let events = render(&events);
    // The stores: frozen entries kept, new kinds added, a removed kind is
    // a break.
    let frozen_stores: serde_json::Value = if signed_dir().join("stores.json").exists() {
        serde_json::from_str(&fixture(&signed_dir().join("stores.json")))
            .unwrap_or_else(|e| panic!("stores.json parses: {e}"))
    } else {
        serde_json::json!({})
    };
    let mut stores = serde_json::Map::new();
    for (kind, path, bytes) in &store_docs {
        if let Some(entry) = frozen_stores.get(kind) {
            assert!(
                entry["path"].as_str() == Some(path.as_str()),
                "the frozen store {kind} changed its path"
            );
            assert!(
                entry["bytes"].as_str() == Some(bytes.as_str()),
                "the frozen store {kind} changed its bytes"
            );
            stores.insert(
                kind.clone(),
                serde_json::json!({ "path": entry["path"], "bytes": entry["bytes"] }),
            );
        } else {
            stores.insert(
                kind.clone(),
                serde_json::json!({ "path": path, "bytes": bytes }),
            );
        }
    }
    // A frozen kind this writer run does not produce stays in the file:
    // the writer only ever adds, and leaving a frozen kind out here would
    // erase its entry. Every frozen kind keeps its frozen bytes.
    let frozen_kinds = frozen_stores
        .as_object()
        .expect("the frozen stores.json holds an object");
    for (kind, entry) in frozen_kinds {
        if !stores.contains_key(kind.as_str()) {
            stores.insert(kind.clone(), entry.clone());
        }
    }
    for (kind, entry) in frozen_kinds {
        assert!(
            stores[kind.as_str()]["bytes"] == entry["bytes"],
            "the frozen store {kind} lost its frozen bytes"
        );
    }
    // Every check over the proposed snapshot, then write: each run reads,
    // keeps its keys, holds no machine state and meets its expectation
    // when it has one (a run this writer adds has none yet; it fails the
    // reader tests until a person writes it).
    {
        let docs = |text: &str| -> Vec<(String, String)> {
            let tmp = tempfile::NamedTempFile::new().unwrap();
            std::fs::write(tmp.path(), text).unwrap();
            documents(tmp.path())
        };
        let (m, c, e) = (docs(&manifests), docs(&configs), docs(&events));
        for (name, manifest_text) in &m {
            let config_text = &c.iter().find(|(n, _)| n == name).unwrap().1;
            let events_text = &e.iter().find(|(n, _)| n == name).unwrap().1;
            for text in [manifest_text, config_text, events_text] {
                for marker in ["/private/", "/tmp/", "/var/folders/", "/home/", "/Users/"] {
                    assert!(
                        !text.contains(marker),
                        "{name} holds the machine path {marker}"
                    );
                }
            }
            let tmp = tempfile::tempdir().unwrap();
            let dir = write_run_docs(tmp.path(), name, manifest_text, config_text);
            let manifest = read_manifest(&dir, name);
            let config = read_config(&dir, name);
            let file = fixtures_dir().join("manifests.yaml");
            keys_kept(&file, name, manifest_text, &typed(&manifest));
            keys_kept(&file, name, config_text, &typed(&config));
            let events = read_events(name, events_text);
            if let Some((_, expect)) = EXPECTATIONS.iter().find(|(n, _)| n == name) {
                expect(&manifest, &config, &events);
            }
        }
    }

    std::fs::create_dir_all(fixtures_dir()).unwrap();
    std::fs::create_dir_all(signed_dir()).unwrap();
    std::fs::write(fixtures_dir().join("manifests.yaml.tmp-freeze"), &manifests).unwrap();
    std::fs::rename(
        fixtures_dir().join("manifests.yaml.tmp-freeze"),
        fixtures_dir().join("manifests.yaml"),
    )
    .unwrap();
    std::fs::write(fixtures_dir().join("run-configs.yaml.tmp-freeze"), &configs).unwrap();
    std::fs::rename(
        fixtures_dir().join("run-configs.yaml.tmp-freeze"),
        fixtures_dir().join("run-configs.yaml"),
    )
    .unwrap();
    std::fs::write(fixtures_dir().join("run-events.yaml.tmp-freeze"), &events).unwrap();
    std::fs::rename(
        fixtures_dir().join("run-events.yaml.tmp-freeze"),
        fixtures_dir().join("run-events.yaml"),
    )
    .unwrap();
    std::fs::write(
        signed_dir().join("stores.json.tmp-freeze"),
        serde_json::to_string_pretty(&serde_json::Value::Object(stores)).unwrap(),
    )
    .unwrap();
    std::fs::rename(
        signed_dir().join("stores.json.tmp-freeze"),
        signed_dir().join("stores.json"),
    )
    .unwrap();
    panic!(
        "freeze written: manifests.yaml, run-configs.yaml, run-events.yaml, stores.json; review the diff and run the test again without APB_WRITE_FREEZE"
    );
}
