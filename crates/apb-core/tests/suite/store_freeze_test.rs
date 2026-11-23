//! The playbook-store freeze: 1.x reads a playbook store written by 1.0
//! with the same meaning - the versions of a playbook, the `current`
//! pointer with its history, the `candidate` pointer, the provenance
//! sidecar (`created_by` included), the trust store and the launch floor
//! store. The freeze holds such a store with nondefault state, written
//! through the public write functions; its tests assert named typed
//! values and the decisions the readers make. See
//! `openspec/changes/one-point-oh-freeze` (design D1.5 item 6) and
//! `docs/COMPATIBILITY.md`.

use std::path::{Path, PathBuf};
use std::sync::MutexGuard;

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

fn fixture_path() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/store-1.0/store.json")
}

fn fixture() -> serde_json::Value {
    let text = std::fs::read_to_string(fixture_path())
        .unwrap_or_else(|e| panic!("cannot read the freeze fixture store.json: {e}"));
    serde_json::from_str(&text).expect("store.json parses")
}

/// `(relative path, text)` of every file of `scope`.
fn files_of(scope: &str) -> Vec<(String, String)> {
    files_in(&fixture(), scope)
}

/// `(relative path, text)` of every file of `scope` in `all`.
fn files_in(all: &serde_json::Value, scope: &str) -> Vec<(String, String)> {
    let empty = serde_json::Map::new();
    let map = all[scope].as_object().unwrap_or(&empty);
    let mut out: Vec<(String, String)> = map
        .iter()
        .map(|(p, t)| (p.clone(), t.as_str().expect("text").to_string()))
        .collect();
    out.sort();
    out
}

/// The fixed 64-character installation key of the freeze; every signed
/// MAC goes through `run_origin::tag`, which reads
/// `<config dir>/run-origin.key`.
const TEST_KEY: &str = "fedcba9876543210fedcba9876543210fedcba9876543210fedcba9876543210";

/// A temporary project and `APB_CONFIG_DIR` with the frozen store placed
/// at its paths.
struct StoreEnv {
    _dir: tempfile::TempDir,
    root: PathBuf,
    config_dir: PathBuf,
    _lock: MutexGuard<'static, ()>,
}

impl Drop for StoreEnv {
    fn drop(&mut self) {
        // SAFETY: env mutation is serialized by ENV_LOCK, held by the guard.
        unsafe { std::env::remove_var("APB_CONFIG_DIR") };
    }
}

fn place_store() -> StoreEnv {
    place_store_from(&fixture())
}

/// The store of `all` (`{"project": .., "config": ..}`) placed at its
/// paths under a temporary project and `APB_CONFIG_DIR`.
fn place_store_from(all: &serde_json::Value) -> StoreEnv {
    let dir = tempfile::tempdir().expect("the store temp dir");
    let root = dir.path().join("project");
    let config_dir = dir.path().join("config");
    std::fs::create_dir_all(&root).expect("the project dir");
    std::fs::create_dir_all(&config_dir).expect("the config dir");
    std::fs::write(config_dir.join("run-origin.key"), TEST_KEY).expect("the key writes");
    for (path, text) in files_in(all, "project") {
        let full = root.join(&path);
        if let Some(parent) = full.parent() {
            std::fs::create_dir_all(parent).expect("the store dir");
        }
        std::fs::write(full, text).expect("the project file writes");
    }
    for (path, text) in files_in(all, "config") {
        let full = config_dir.join(&path);
        if let Some(parent) = full.parent() {
            std::fs::create_dir_all(parent).expect("the store dir");
        }
        std::fs::write(full, text).expect("the config file writes");
    }
    let lock = crate::common::env_lock();
    // SAFETY: as above.
    unsafe { std::env::set_var("APB_CONFIG_DIR", &config_dir) };
    StoreEnv {
        _dir: dir,
        root,
        config_dir,
        _lock: lock,
    }
}

/// The playbook dir of `id` in the placed store.
fn playbook_dir(env: &StoreEnv, id: &str) -> PathBuf {
    env.root.join(".apb/playbooks").join(id)
}

// ---------------------------------------------------------------------------
// The expectations (the meaning rule)
// ---------------------------------------------------------------------------

type StoreExpectation = fn(&StoreEnv);

/// Every file kind with its expectation. A file the writer adds fails
/// here until a person writes its expectation; the writer never writes an
/// expectation.
const EXPECTATIONS: &[(&str, StoreExpectation)] = &[
    ("versions", expect_versions as StoreExpectation),
    (
        "current-history",
        expect_current_history as StoreExpectation,
    ),
    ("candidate", expect_candidate as StoreExpectation),
    ("sidecars", expect_sidecars as StoreExpectation),
    ("lifecycle", expect_lifecycle as StoreExpectation),
    ("trust", expect_trust as StoreExpectation),
    ("launch-floors", expect_floors as StoreExpectation),
    ("diagram", expect_diagrams as StoreExpectation),
    ("current", expect_current_pointers as StoreExpectation),
];

fn kind_of(path: &str) -> Option<&'static str> {
    if path.ends_with("current-history.jsonl") {
        Some("current-history")
    } else if path.ends_with("/candidate") {
        Some("candidate")
    } else if path.contains("/meta/") {
        Some("sidecars")
    } else if path.ends_with("lifecycle") {
        Some("lifecycle")
    } else if path.contains("/1.") && path.ends_with("playbook.yaml") {
        Some("versions")
    } else if path == "trust.json" {
        Some("trust")
    } else if path == "launch-floors.json" {
        Some("launch-floors")
    } else if path.ends_with("playbook.mmd") {
        Some("diagram")
    } else if path.ends_with("/current") {
        Some("current")
    } else {
        None
    }
}

/// Every project and config file has an expectation.
#[test]
fn every_document_has_an_expectation() {
    for (path, _) in files_of("project").into_iter().chain(files_of("config")) {
        let known = EXPECTATIONS.iter().map(|(k, _)| *k).collect::<Vec<_>>();
        let kind = kind_of(&path);
        assert!(
            kind.is_some(),
            "the frozen store file {path} has no expectation (known kinds: {known:?}); write one"
        );
        assert!(
            EXPECTATIONS.iter().any(|(k, _)| Some(*k) == kind),
            "the kind {kind:?} of {path} has no expectation"
        );
    }
    for (kind, _) in EXPECTATIONS {
        let held = files_of("project")
            .into_iter()
            .chain(files_of("config"))
            .any(|(p, _)| kind_of(&p) == Some(*kind));
        assert!(
            held,
            "the expectation {kind} names no file the freeze holds"
        );
    }
}

/// The versions read in order with `is_current` and `is_candidate` as
/// frozen.
fn expect_versions(env: &StoreEnv) {
    let versions = apb_core::versioning::list_versions_with_provenance(&env.root, "deploy")
        .expect("the versions read");
    let names: Vec<&str> = versions.iter().map(|v| v.version.as_str()).collect();
    assert_eq!(
        names,
        vec!["1.0.0", "1.1.0", "1.2.0"],
        "the versions in order"
    );
    let by_version = |v: &str| {
        versions
            .iter()
            .find(|x| x.version == v)
            .expect("the version")
    };
    assert!(!by_version("1.0.0").is_current, "1.0.0 is not current");
    assert!(by_version("1.2.0").is_current, "1.2.0 is current");
    assert!(by_version("1.1.0").is_candidate, "1.1.0 is the candidate");
    assert!(
        !by_version("1.2.0").is_candidate,
        "1.2.0 is not the candidate"
    );
}

/// The history lists the frozen entries, complete and not pending, and
/// `settled` answers the frozen `current` and `last_move_by`.
fn expect_current_history(env: &StoreEnv) {
    let dir = playbook_dir(env, "deploy");
    let history = apb_core::versioning::current::history(&dir, 10).expect("the history reads");
    assert!(!history.incomplete, "the history is complete");
    assert!(!history.pending, "no move is pending");
    assert!(
        history.entries.len() >= 2,
        "the frozen history holds at least the two moves"
    );
    let first = &history.entries[0];
    assert!(
        first.prev.is_none() || history.entries.len() > 1,
        "the entries chain by prev"
    );
    let settled = apb_core::versioning::current::settled(&dir).expect("the pointers settle");
    assert_eq!(
        settled.current.as_deref(),
        Some("1.2.0"),
        "current reads as frozen"
    );
    assert!(!settled.pending, "no pending intent is in the frozen store");
    let pointer = apb_core::versioning::read_current_pointer(&dir);
    assert_eq!(
        pointer.as_deref(),
        Some("1.2.0"),
        "the pointer reads as frozen"
    );
}

/// The candidate pointer names the frozen version.
fn expect_candidate(env: &StoreEnv) {
    let dir = playbook_dir(env, "deploy");
    assert_eq!(
        apb_core::candidate_pointer::read_candidate(&dir).as_deref(),
        Some("1.1.0"),
        "the candidate reads as frozen"
    );
}

/// The provenance sidecars read with their `created_by` and the other
/// frozen fields.
fn expect_sidecars(env: &StoreEnv) {
    let template = apb_core::versioning::read_provenance(&env.root, "deploy", "1.0.0")
        .expect("the sidecar reads")
        .expect("the template sidecar is there");
    assert_eq!(template.created_by, "template", "the template sidecar");
    assert_eq!(
        template.shipped_version.as_deref(),
        Some("1.0.0"),
        "the shipped version reads as frozen"
    );
    let supervisor = apb_core::versioning::read_provenance(&env.root, "deploy", "1.1.0")
        .expect("the sidecar reads")
        .expect("the supervisor sidecar is there");
    assert_eq!(
        supervisor.created_by, "supervisor",
        "the supervisor sidecar"
    );
    assert_eq!(
        supervisor.scope.as_deref(),
        Some("next_runs"),
        "the patch scope"
    );
    assert_eq!(
        supervisor.base_version.as_deref(),
        Some("1.0.0"),
        "the base version"
    );
    assert!(supervisor.rationale.is_some(), "the rationale is frozen");
    assert!(!supervisor.evidence.is_empty(), "the evidence is frozen");
    assert!(supervisor.trial.is_some(), "the trial record is frozen");
    let imported = apb_core::versioning::read_provenance(&env.root, "imported", "1.0.0")
        .expect("the sidecar reads")
        .expect("the import sidecar is there");
    assert_eq!(imported.created_by, "import", "the import sidecar");
    assert!(
        imported.imported.is_some(),
        "the import provenance is frozen"
    );
}

/// A lifecycle `retired` reads as frozen.
fn expect_lifecycle(env: &StoreEnv) {
    let lc = apb_core::trust::read_lifecycle(&playbook_dir(env, "imported"));
    assert_eq!(
        lc,
        apb_core::trust::Lifecycle::Retired,
        "the lifecycle is retired"
    );
}

/// The trust store decides: a frozen digest is approved, another digest
/// of a frozen id is changed, an unknown id is unapproved; each origin
/// reads as frozen.
fn expect_trust(_env: &StoreEnv) {
    let store = apb_core::trust::TrustStore::load();
    assert_eq!(
        store.status(
            FROZEN_PLAYBOOK_DIGEST,
            PLAYBOOK_ID,
            apb_core::trust::Kind::Playbook
        ),
        apb_core::trust::TrustStatus::Approved,
        "the frozen digest is approved"
    );
    assert_eq!(
        store.status(
            OTHER_PLAYBOOK_DIGEST,
            PLAYBOOK_ID,
            apb_core::trust::Kind::Playbook
        ),
        apb_core::trust::TrustStatus::Changed,
        "another digest of the same id is changed"
    );
    assert_eq!(
        store.status(
            "d-other",
            "unknown-playbook",
            apb_core::trust::Kind::Playbook
        ),
        apb_core::trust::TrustStatus::Unapproved,
        "an unknown id is unapproved"
    );
    let entries = store.entries();
    let bundle = entries
        .iter()
        .find(|e| e.id == BUNDLE_NAME && e.kind == apb_core::trust::Kind::ProfileBundle)
        .expect("the bundle approval is frozen");
    assert_eq!(
        bundle.origin_kind,
        apb_core::trust::OriginKind::AgentGenerated
    );
    let connector = entries
        .iter()
        .find(|e| e.id == CONNECTOR_NAME && e.kind == apb_core::trust::Kind::Connector)
        .expect("the connector approval is frozen");
    assert_eq!(
        connector.digest, FROZEN_CONNECTOR_DIGEST,
        "the connector digest"
    );
    assert_eq!(
        connector.origin_kind,
        apb_core::trust::OriginKind::LocallyApproved
    );
    let account = entries
        .iter()
        .find(|e| e.id == ACCOUNT_TRUST_ID && e.kind == apb_core::trust::Kind::ConnectorAccount)
        .expect("the account approval is frozen");
    assert_eq!(account.digest, FROZEN_ACCOUNT_DIGEST, "the account digest");
}

/// The launch floors read with the frozen records, and `floor_in`
/// answers the frozen policy for each key.
fn expect_floors(env: &StoreEnv) {
    let records =
        apb_core::launch::floor::read_from(Some(&env.config_dir.join("launch-floors.json")))
            .expect("the floors read");
    assert_eq!(records.len(), 2, "two floor records are frozen");
    let project = apb_core::launch::floor::FloorKey::Project {
        workspace_id: WORKSPACE_ID.to_string(),
        id: PLAYBOOK_ID.to_string(),
    };
    assert_eq!(
        apb_core::launch::floor::floor_in(&records, Some(&project), None),
        Some(apb_core::launch::LaunchPolicy::Deny),
        "the project floor is frozen"
    );
    let global = apb_core::launch::floor::FloorKey::global(GLOBAL_ID);
    assert_eq!(
        apb_core::launch::floor::floor_in(&records, Some(&global), None),
        Some(apb_core::launch::LaunchPolicy::Ask),
        "the global floor is frozen"
    );
}

/// Every version's derived diagram is there and not empty: a version
/// write produces it beside the YAML, and a move keeps the diagram that
/// belongs to the bytes.
fn expect_diagrams(env: &StoreEnv) {
    for version in ["1.0.0", "1.1.0", "1.2.0"] {
        let path = playbook_dir(env, "deploy")
            .join(version)
            .join("playbook.mmd");
        let text = std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("the diagram of {version} reads: {e}"));
        assert!(
            !text.trim().is_empty(),
            "the diagram of {version} is not empty"
        );
    }
}

/// The `current` pointers: `deploy` names 1.2.0, `imported` names its
/// 1.0.0.
fn expect_current_pointers(env: &StoreEnv) {
    for (id, version) in [("deploy", "1.2.0"), ("imported", "1.0.0")] {
        let dir = playbook_dir(env, id);
        let text = std::fs::read_to_string(dir.join("current"))
            .unwrap_or_else(|e| panic!("the current pointer of {id} reads: {e}"));
        assert_eq!(text.trim(), version, "the current pointer of {id}");
    }
}

// The frozen digests and ids the trust and floor expectations name. The
// writer computes them from the store it wrote; the reader test asserts
// against the same constants.
const PLAYBOOK_ID: &str = "deploy";
const WORKSPACE_ID: &str = "ws-fixed";
const GLOBAL_ID: &str = "triage";
const BUNDLE_NAME: &str = "apb-main";
const CONNECTOR_NAME: &str = "mock-tracker";
const ACCOUNT_TRUST_ID: &str = "mock-tracker/main";
const FROZEN_PLAYBOOK_DIGEST: &str =
    "sha256:170ccd849bfba65723884158773bfa6466544d162a3a2cdb363445065574f6dc";
const OTHER_PLAYBOOK_DIGEST: &str =
    "0006a2f0c1e34a4f9d0c2b5a7e8f1234a5b6c7d8e9f0a1b2c3d4e5f6a7b8c9d0";
const FROZEN_CONNECTOR_DIGEST: &str =
    "c1c6a2f0c1e34a4f9d0c2b5a7e8f1234a5b6c7d8e9f0a1b2c3d4e5f6a7b8c9d0";
const FROZEN_ACCOUNT_DIGEST: &str =
    "a1a6a2f0c1e34a4f9d0c2b5a7e8f1234a5b6c7d8e9f0a1b2c3d4e5f6a7b8c9d0";

/// The frozen store, placed at its paths, reads with the frozen meaning:
/// every expectation runs against it.
#[test]
fn the_frozen_store_still_reads() {
    let env = place_store();
    for (kind, check) in EXPECTATIONS {
        check(&env);
        let _ = kind;
    }
}

// ---------------------------------------------------------------------------
// Machine state: the values a store records from the clock or at random
// ---------------------------------------------------------------------------

/// The fixed time a normalized store records: the journal fixture's epoch.
const FREEZE_MS: u64 = 1_790_000_000_000;

/// The store file with the values it took from the clock or at random
/// replaced by fixed ones, so the fixture is the same on every machine and
/// every run of the writer. `current-history.jsonl`: each line's
/// `change_id` (random) becomes its line number in 32 hex digits, its
/// `at_ms` (the clock) `FREEZE_MS` plus a second per line, and its `prev`
/// the digest of the normalized line before it, the same rule
/// `versioning::current` chains by. `trust.json`: every `approved_at_ms`
/// becomes `FREEZE_MS`. No production seam fixes them, so the writer
/// rewrites them and `no_store_file_holds_machine_state` requires every
/// frozen file to read back unchanged through this function. Every other
/// byte stays as the production writer wrote it.
fn normalize_machine_state(path: &str, text: &str) -> String {
    if path.ends_with("current-history.jsonl") {
        let change = regex::Regex::new(r#""change_id":"[^"]*""#).expect("compiles");
        let at = regex::Regex::new(r#""at_ms":\d+"#).expect("compiles");
        let prev = regex::Regex::new(r#""prev":"sha256:[0-9a-f]+""#).expect("compiles");
        let mut out = String::with_capacity(text.len());
        let mut last: Option<String> = None;
        for (i, line) in text.lines().enumerate() {
            let line = change.replace(line, format!(r#""change_id":"{:032x}""#, i + 1));
            let line = at.replace(&line, format!(r#""at_ms":{}"#, FREEZE_MS + 1000 * i as u64));
            let line = match &last {
                Some(before) => prev
                    .replace(
                        &line,
                        format!(
                            r#""prev":"{}""#,
                            apb_core::content::sha256_hex(before.as_bytes())
                        ),
                    )
                    .into_owned(),
                None => line.into_owned(),
            };
            out.push_str(&line);
            out.push('\n');
            last = Some(line);
        }
        out
    } else if path == "trust.json" {
        regex::Regex::new(r#""approved_at_ms": \d+"#)
            .expect("compiles")
            .replace_all(text, format!(r#""approved_at_ms": {FREEZE_MS}"#))
            .into_owned()
    } else {
        text.to_string()
    }
}

/// Every frozen store file is already free of machine state: it reads
/// back unchanged through `normalize_machine_state`.
#[test]
fn no_store_file_holds_machine_state() {
    for scope in ["project", "config"] {
        for (path, text) in files_of(scope) {
            assert_eq!(
                normalize_machine_state(&path, &text),
                text,
                "the frozen store file {path} holds a clock or random value: run the store writer"
            );
            for marker in ["/private/", "/tmp/", "/var/folders/", "/home/", "/Users/"] {
                assert!(
                    !text.contains(marker),
                    "the frozen store file {path} holds the machine path {marker}"
                );
            }
        }
    }
}

// ---------------------------------------------------------------------------
// The writer
// ---------------------------------------------------------------------------

/// Every file under `dir`, as `(relative path, text)`.
fn collect_dir(dir: &Path, base: &Path, out: &mut Vec<(String, String)>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_dir(&path, base, out);
        } else {
            let relative = path
                .strip_prefix(base)
                .expect("under the base")
                .to_string_lossy()
                .to_string();
            let text = std::fs::read_to_string(&path).unwrap_or_default();
            out.push((relative, text));
        }
    }
}

fn playbook_yaml(id: &str, name: &str) -> String {
    format!(
        "schema: 2\nid: {id}\nname: {name}\nversion: \"{{V}}\"\ndefaults:\n  profile: main\nnodes:\n  - {{ id: start, type: start }}\n  - {{ id: act, type: agent_task, prompt: \"do it\" }}\n  - {{ id: done, type: finish, outcome: success }}\nedges:\n  - {{ from: start, to: act }}\n  - {{ from: act, to: done }}\n"
    )
}

/// The one writer of the playbook-store group. It writes the store under
/// a temporary project and config dir through the public write functions,
/// then records every file's bytes.
#[test]
fn write_freeze() {
    if !write_freeze_enabled() {
        return;
    }
    let dir = tempfile::tempdir().expect("the writer temp dir");
    let root = dir.path().join("project");
    let config_dir = dir.path().join("config");
    std::fs::create_dir_all(&root).unwrap();
    std::fs::create_dir_all(&config_dir).unwrap();
    std::fs::write(config_dir.join("run-origin.key"), TEST_KEY).unwrap();
    let lock = crate::common::env_lock();
    // SAFETY: env mutation is serialized by ENV_LOCK, held by the guard.
    unsafe { std::env::set_var("APB_CONFIG_DIR", &config_dir) };
    apb_core::run_origin::ensure_key().unwrap();
    // A project layout: `create_version` validates through the registry.
    apb_core::registry::init_project(&root).unwrap();

    // 1. Three versions of a playbook, and a retired second playbook.
    let yaml_v1 = playbook_yaml("deploy", "Deploy").replace("{V}", "1.0.0");
    apb_core::versioning::create_version(&root, "deploy", &yaml_v1, None, true)
        .expect("the first version");
    let yaml_v2 = playbook_yaml("deploy", "Deploy").replace("{V}", "1.1.0");
    apb_core::versioning::create_version_with_override(
        &root,
        "deploy",
        &yaml_v2,
        Some("1.0.0"),
        Some("1.1.0"),
        false,
    )
    .expect("the second version");
    let yaml_v3 = playbook_yaml("deploy", "Deploy").replace("{V}", "1.2.0");
    apb_core::versioning::save_definition_by(
        &root,
        "deploy",
        &yaml_v3,
        Some("1.2.0"),
        false,
        apb_core::launch::floor::SaveChannel::Person,
    )
    .expect("the third version");
    let yaml_imp = playbook_yaml("imported", "Imported").replace("{V}", "1.0.0");
    apb_core::versioning::create_version_with_override(
        &root,
        "imported",
        &yaml_imp,
        None,
        Some("1.0.0"),
        true,
    )
    .expect("the imported playbook");

    // 2. The sidecars: template (with its shipped version), supervisor
    //    (a forward patch with scope, base, rationale, evidence and a
    //    trial record), import.
    let deploy_dir = root.join(".apb/playbooks/deploy");
    apb_core::versioning::write_provenance(
        &root,
        "deploy",
        "1.0.0",
        &apb_core::versioning::VersionProvenance {
            created_by: "template".to_string(),
            run_id: None,
            classification: None,
            scope: None,
            base_version: None,
            rationale: None,
            evidence: Vec::new(),
            trial: None,
            imported: None,
            shipped_version: Some("1.0.0".to_string()),
            external_data_run: None,
        },
    )
    .expect("the template sidecar");
    apb_core::versioning::write_provenance(
        &root,
        "deploy",
        "1.1.0",
        &apb_core::versioning::VersionProvenance {
            created_by: "supervisor".to_string(),
            run_id: Some("deploy-run-1".to_string()),
            classification: None,
            scope: Some("next_runs".to_string()),
            base_version: Some("1.0.0".to_string()),
            rationale: Some("the prompt leaked the plan; say the answer".to_string()),
            evidence: vec!["nodes[2]:12".to_string()],
            trial: Some(apb_core::versioning::TrialRecord {
                successes: 1,
                outcome: None,
                run_id: Some("deploy-run-2".to_string()),
                reason: None,
                quality: None,
            }),
            imported: None,
            shipped_version: None,
            external_data_run: None,
        },
    )
    .expect("the supervisor sidecar");
    apb_core::versioning::write_provenance(
        &root,
        "imported",
        "1.0.0",
        &apb_core::versioning::VersionProvenance {
            created_by: "import".to_string(),
            run_id: None,
            classification: None,
            scope: None,
            base_version: None,
            rationale: None,
            evidence: Vec::new(),
            trial: None,
            imported: Some(apb_core::versioning::ImportProvenance {
                bundle_digest:
                    "sha256:0000000000000000000000000000000000000000000000000000000000000000"
                        .to_string(),
                file_name: Some("a-bundle.zip".to_string()),
                source_id: "imported".to_string(),
                source_version: "1.0.0".to_string(),
                source: None,
                imported_at_ms: 1_790_000_000_000,
                scripts: Vec::new(),
                digest: String::new(),
            }),
            shipped_version: None,
            external_data_run: None,
        },
    )
    .expect("the import sidecar");

    // 3. `current` moved twice: once by a person with a reason (to
    //    1.0.0's successor view), once superseding the candidate.
    let first = apb_core::versioning::current::move_current(
        &deploy_dir,
        &apb_core::versioning::current::MoveCurrent {
            to: "1.0.0",
            expected_from: None,
            by: apb_core::versioning::current::MovedBy::Person {
                surface: apb_core::versioning::current::Surface::Cli,
            },
            reason: Some("roll back to the shipped default"),
            trial: None,
        },
    )
    .expect("the person's move");
    let _ = first;
    let second = apb_core::versioning::current::move_current(
        &deploy_dir,
        &apb_core::versioning::current::MoveCurrent {
            to: "1.2.0",
            expected_from: Some(Some("1.0.0")),
            by: apb_core::versioning::current::MovedBy::Person {
                surface: apb_core::versioning::current::Surface::Cli,
            },
            reason: Some("take the reviewed fix"),
            trial: Some(apb_core::versioning::current::TrialFinal {
                run_id: Some("deploy-run-3".to_string()),
                quality: None,
            }),
        },
    )
    .expect("the superseding move");
    let _ = second;

    // The candidate pointer: the supervisor's forward patch on trial,
    // written after the moves (a move past it supersedes and clears it).
    {
        let candidate_path = deploy_dir.join(apb_core::candidate_pointer::CANDIDATE_FILE);
        std::fs::write(&candidate_path, "1.1.0").unwrap();
    }

    // 4. A retired lifecycle on the second playbook.
    apb_core::trust::write_lifecycle(
        &root.join(".apb/playbooks/imported"),
        apb_core::trust::Lifecycle::Retired,
    )
    .expect("the lifecycle");

    // 5. The trust store: approvals of four kinds, different origins.
    //    The playbook digest is the real digest of the frozen 1.0.0
    //    version, so a reader's content digest decides exactly as frozen.
    let mut trust = apb_core::trust::TrustStore::load();
    let v1_digest = {
        let yaml = std::fs::read_to_string(deploy_dir.join("1.0.0/playbook.yaml")).unwrap();
        apb_core::scope::definition_digest(&yaml, &deploy_dir.join("1.0.0")).expect("the digest")
    };
    trust
        .approve(
            &v1_digest,
            PLAYBOOK_ID,
            apb_core::trust::OriginKind::LocallyApproved,
        )
        .expect("the playbook approval");
    trust
        .approve_kind(
            "0aa6a2f0c1e34a4f9d0c2b5a7e8f1234a5b6c7d8e9f0a1b2c3d4e5f6a7b8c9d0",
            BUNDLE_NAME,
            apb_core::trust::Kind::ProfileBundle,
            apb_core::trust::OriginKind::AgentGenerated,
        )
        .expect("the bundle approval");
    trust
        .approve_kind(
            FROZEN_CONNECTOR_DIGEST,
            CONNECTOR_NAME,
            apb_core::trust::Kind::Connector,
            apb_core::trust::OriginKind::LocallyApproved,
        )
        .expect("the connector approval");
    trust
        .approve_kind(
            FROZEN_ACCOUNT_DIGEST,
            ACCOUNT_TRUST_ID,
            apb_core::trust::Kind::ConnectorAccount,
            apb_core::trust::OriginKind::LocallyApproved,
        )
        .expect("the account approval");

    // 6. The launch floors: a project record and a global record at two
    //    different policies.
    apb_core::launch::floor::set_by_person(
        &apb_core::launch::floor::FloorKey::Project {
            workspace_id: WORKSPACE_ID.to_string(),
            id: PLAYBOOK_ID.to_string(),
        },
        apb_core::launch::LaunchPolicy::Deny,
    )
    .expect("the project floor");
    apb_core::launch::floor::set_by_person(
        &apb_core::launch::floor::FloorKey::global(GLOBAL_ID),
        apb_core::launch::LaunchPolicy::Ask,
    )
    .expect("the global floor");

    // Every reader decision over the written store must hold before the
    // bytes are recorded.
    let env = StoreEnv {
        _dir: dir,
        root: root.clone(),
        config_dir: config_dir.clone(),
        _lock: lock,
    };
    // The frozen digest constant must be the written version's digest.
    assert_eq!(
        v1_digest, FROZEN_PLAYBOOK_DIGEST,
        "the frozen playbook digest constant must be the digest of the frozen 1.0.0 bytes; update the constant"
    );
    let _ = &env;
    expect_versions(&env);
    expect_current_history(&env);
    expect_candidate(&env);
    expect_sidecars(&env);
    expect_lifecycle(&env);
    expect_trust(&env);
    expect_floors(&env);

    // Record the bytes: the project store files and the config files.
    let mut project = serde_json::Map::new();
    let mut collected = Vec::new();
    collect_dir(&root.join(".apb/playbooks"), &root, &mut collected);
    collected.sort();
    let frozen = fixture();
    let frozen_project = frozen["project"].as_object().cloned().unwrap_or_default();
    // A frozen file is kept byte for byte except for
    // `normalize_machine_state`, the one rewrite the writer makes.
    for (path, text) in collected {
        let text = match frozen_project.get(&path).and_then(|t| t.as_str()) {
            Some(frozen_text) => frozen_text.to_string(),
            None => text,
        };
        let normalized = normalize_machine_state(&path, &text);
        project.insert(path, serde_json::Value::String(normalized));
    }
    // A frozen file the new store no longer writes is a break: the
    // writer only adds.
    for path in frozen_project.keys() {
        assert!(
            project.contains_key(path),
            "the writer never removes a file: store.json holds {path}, which this build no longer writes"
        );
    }
    let mut config = serde_json::Map::new();
    let mut collected = Vec::new();
    collect_dir(&config_dir, &config_dir, &mut collected);
    collected.sort();
    let frozen_config = frozen["config"].as_object().cloned().unwrap_or_default();
    for (path, text) in collected {
        if path == "run-origin.key" {
            continue; // the key file is the test seam, not the store
        }
        if path == "projects.json" {
            // The registry binds a workspace id to the absolute path of
            // the directory it was minted for, so no frozen copy can read
            // back at another machine's root: the writer's temp path would
            // be machine-dependent fixture data. A reader's own
            // registration (workspace.local plus its registry) is covered
            // by the projects tests, not by a frozen file.
            continue;
        }
        let text = match frozen_config.get(&path).and_then(|t| t.as_str()) {
            Some(frozen_text) => frozen_text.to_string(),
            None => text,
        };
        let normalized = normalize_machine_state(&path, &text);
        config.insert(path, serde_json::Value::String(normalized));
    }
    for path in frozen_config.keys() {
        if path == "run-origin.key" || path == "projects.json" {
            continue;
        }
        assert!(
            config.contains_key(path),
            "the writer never removes a file: store.json holds config {path}, which this build no longer writes"
        );
    }

    let proposed = serde_json::json!({
        "project": serde_json::Value::Object(project),
        "config": serde_json::Value::Object(config),
    });
    // Every check over the proposed snapshot before it is written: the
    // normalized history still chains, and every expectation holds on the
    // store exactly as it will be frozen.
    drop(env);
    {
        let placed = place_store_from(&proposed);
        for (_, check) in EXPECTATIONS {
            check(&placed);
        }
    }
    let tmp = fixture_path().with_extension("tmp-freeze");
    std::fs::write(&tmp, serde_json::to_string_pretty(&proposed).unwrap()).unwrap();
    std::fs::rename(&tmp, fixture_path()).unwrap();
    panic!(
        "freeze written: store.json under {}; review the diff and run the test again without APB_WRITE_FREEZE",
        fixture_path().display()
    );
}
