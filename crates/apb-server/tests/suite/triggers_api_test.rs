//! The start rules on the HTTP API (design D1 "Tests" item 7): every write
//! takes a signed-in session (a keyless dashboard and a bearer key are
//! refused 403 `trigger_requires_session`), the approval posted back is
//! compared as shown (a changed need is refused, an unknown field is 422),
//! `enabled` is not settable through the API, and every blocking step of a
//! route runs off the async runtime. The test route itself is allowed on
//! every channel and refused from a run step (design D1 "HTTP" item 3).
//! Fixtures mirror the MCP suite's
//! (`crates/apb-mcp/tests/suite/trigger_tools_test.rs`).

use std::fs;
use std::path::{Path, PathBuf};

use apb_core::clock::Millis;
use apb_core::registry::init_project;
use apb_core::scope::Scope;
use apb_core::signed_store::read as read_signed;
use apb_core::trigger::{RuleEntry, RulesFile};
use apb_server::auth::{CSRF_HEADER, CSRF_VALUE, SESSION_COOKIE};
use apb_server::{AppState, build_router};
use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tower::ServiceExt;

use crate::common;

/// The playbook id and the rule name of the fixture.
const RULE_PB: &str = "tw1";
const RULE_NAME: &str = "news";

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
"#;

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
    _cfg: tempfile::TempDir,
    dir: tempfile::TempDir,
    _vars: Vars,
    _lock: tokio::sync::MutexGuard<'static, ()>,
}

impl Rig {
    async fn new(id: &str, yaml: &str) -> Self {
        let lock = common::env_lock().await;
        let cfg = tempfile::tempdir().unwrap();
        let dir = tempfile::tempdir().unwrap();
        let vars = Vars::isolate(cfg.path());
        apb_core::run_origin::ensure_key().unwrap();
        install_connector(cfg.path());
        init_project(dir.path()).unwrap();
        write_playbook(dir.path(), id, yaml);
        seed_profile(dir.path(), "main", "claude-code", "haiku");
        apb_core::projects::bind_id(dir.path()).unwrap();
        trust_current(dir.path(), id);
        Rig {
            _cfg: cfg,
            dir,
            _vars: vars,
            _lock: lock,
        }
    }

    fn root(&self) -> PathBuf {
        self.dir.path().to_path_buf()
    }
}

fn install_connector(cfg: &Path) {
    let dir = cfg.join("connectors").join("github");
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("connector.yaml"), GITHUB_YAML).unwrap();
    fs::create_dir_all(cfg.join("connector-config")).unwrap();
    fs::write(
        cfg.join("connector-config").join("github.yaml"),
        "accounts:\n  - name: main\n",
    )
    .unwrap();
}

/// A profile under a never-played executor, approved as a profile a person
/// wrote through apb's own tools is.
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
/// a playbook they saved does.
fn trust_current(root: &Path, id: &str) {
    let reg = apb_core::registry::Registry::open_dir(&root.join(".apb")).unwrap();
    let loaded = reg.load(id, Some("1.0.0")).unwrap();
    let digest = loaded.trust_digest().unwrap();
    let mut store = apb_core::trust::TrustStore::load();
    store
        .approve(&digest, id, apb_core::trust::OriginKind::LocallyApproved)
        .unwrap();
}

/// `start -> agent -> done`, so preparation snapshots a real profile bundle
/// and a change of it changes the permit.
fn agent_pb(id: &str) -> String {
    format!(
        "schema: 2\nid: {id}\nname: {id}\nversion: 1.0.0\nnodes:\n  - {{ id: start, type: start }}\n  - {{ id: a, type: agent_task, prompt: x, profile: main }}\n  - {{ id: done, type: finish, outcome: success }}\nedges:\n  - {{ from: start, to: a }}\n  - {{ from: a, to: done }}\n"
    )
}

/// The rule document of the fixture, as JSON the PUT body carries (the wire
/// form of what a person writes as YAML).
fn rule_json() -> Value {
    json!({
        "name": RULE_NAME,
        "playbook": { "id": RULE_PB, "version": "1.0.0", "scope": "project" },
        "source": { "inbox": {
            "connector": "github",
            "account": "main",
            "types": ["issues"],
            "key": "{{event.seq}}",
        } },
        "limits": { "max_per_hour": 5 },
        "budget": { "tokens": 100000, "on_no_estimate": "continue" },
        "external_data": true,
        "require_ready": false,
    })
}

fn save_rule(root: &Path, doc: &str) {
    apb_core::trigger::set_rule(root, doc, "test", Millis(now())).unwrap();
}

/// The fixture rule as YAML, what `set_rule` reads.
fn rule_doc() -> String {
    format!(
        "name: {RULE_NAME}\nplaybook: {{ id: {RULE_PB}, version: 1.0.0, scope: project }}\nsource:\n  inbox:\n    connector: github\n    account: main\n    types: [issues]\n    key: \"{{{{event.seq}}}}\"\nlimits: {{ max_per_hour: 5 }}\nbudget: {{ tokens: 100000, on_no_estimate: continue }}\nexternal_data: true\nrequire_ready: false\n"
    )
}

fn now() -> u64 {
    apb_core::clock::now_secs()
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

/// A rig with the fixture rule saved (never approved).
async fn rig_with_rule() -> Rig {
    let rig = Rig::new(RULE_PB, &agent_pb(RULE_PB)).await;
    save_rule(&rig.root(), &rule_doc());
    rig
}

/// What the approval route would show for the rule right now.
fn need_of(root: &Path, name: &str) -> Value {
    let entry = entry_of(root, name);
    let need = apb_mcp::tools::trigger::approval_need(root, &entry).expect("the need");
    serde_json::to_value(&need).unwrap()
}

/// Sends one request; `cookie` and `bearer` are optional credentials.
async fn send(
    app: axum::Router,
    method: &str,
    uri: &str,
    body: Option<Value>,
    cookie: Option<&str>,
    bearer: Option<&str>,
) -> (StatusCode, Value) {
    let mut builder = Request::builder()
        .method(method)
        .uri(uri)
        .header("content-type", "application/json");
    if let Some(cookie) = cookie {
        builder = builder
            .header("cookie", cookie)
            .header(CSRF_HEADER, CSRF_VALUE);
    }
    if let Some(key) = bearer {
        builder = builder.header("authorization", format!("Bearer {key}"));
    }
    let payload = body.map(|b| b.to_string().into_bytes()).unwrap_or_default();
    let req = builder.body(Body::from(payload)).unwrap();
    let res = app.oneshot(req).await.unwrap();
    let status = res.status();
    let bytes = res.into_body().collect().await.unwrap().to_bytes();
    let json = serde_json::from_slice(&bytes)
        .unwrap_or_else(|_| Value::String(String::from_utf8_lossy(&bytes).to_string()));
    (status, json)
}

/// Logs in on a keyed state and returns the session cookie.
async fn session_cookie(state: &AppState, key: &str) -> String {
    let req = Request::builder()
        .method("POST")
        .uri("/api/auth/login")
        .header("content-type", "application/json")
        .header(CSRF_HEADER, CSRF_VALUE)
        .body(Body::from(
            serde_json::to_vec(&json!({ "key": key })).unwrap(),
        ))
        .unwrap();
    let res = build_router(state.clone()).oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK, "the login works");
    let set_cookie = res
        .headers()
        .get(axum::http::header::SET_COOKIE)
        .and_then(|v| v.to_str().ok())
        .expect("a session cookie is set")
        .to_string();
    assert!(set_cookie.starts_with(&format!("{SESSION_COOKIE}=")));
    set_cookie.split(';').next().unwrap().to_string()
}

/// The keyed harness of the fixture root: the key and the state.
fn keyed(root: &Path) -> (String, AppState) {
    common::keyed(root.to_path_buf())
}

/// The HTTP `set` and `enabled` routes store the rule's time in
/// milliseconds: the stored `updated_at` is the wall clock in
/// milliseconds, never in seconds.
#[tokio::test]
async fn the_http_routes_store_the_rule_time_in_milliseconds() {
    let rig = Rig::new(RULE_PB, &agent_pb(RULE_PB)).await;
    let root = rig.root();
    let (key, state) = keyed(&root);
    let cookie = session_cookie(&state, &key).await;

    let before = apb_core::clock::now_ms_u64();
    let (status, json) = send(
        build_router(state.clone()),
        "PUT",
        &format!("/api/triggers/{RULE_NAME}"),
        Some(json!({ "rule": rule_json() })),
        Some(&cookie),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{json:#}");
    let stored = entry_of(&root, RULE_NAME).updated_at;
    assert!(
        stored >= before.saturating_sub(60_000) && stored <= before + 60_000,
        "the set route stored the time in milliseconds: {stored}"
    );

    let before = apb_core::clock::now_ms_u64();
    let (status, json) = send(
        build_router(state.clone()),
        "POST",
        &format!("/api/triggers/{RULE_NAME}/enable"),
        Some(json!({})),
        Some(&cookie),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{json:#}");
    let stored = entry_of(&root, RULE_NAME).updated_at;
    assert!(
        stored >= before.saturating_sub(60_000) && stored <= before + 60_000,
        "the enable route stored the time in milliseconds: {stored}"
    );
}

/// Every write answers 403 `trigger_requires_session` on a keyless
/// dashboard and on a bearer key, and works on a session.
#[tokio::test]
async fn writes_need_a_session() {
    let rig = rig_with_rule().await;
    let root = rig.root();

    let writes: Vec<(&'static str, String, Option<Value>)> = vec![
        (
            "PUT",
            format!("/api/triggers/{RULE_NAME}"),
            Some(json!({ "rule": rule_json() })),
        ),
        (
            "POST",
            format!("/api/triggers/{RULE_NAME}/enable"),
            Some(json!({})),
        ),
        (
            "POST",
            format!("/api/triggers/{RULE_NAME}/disable"),
            Some(json!({})),
        ),
        (
            "POST",
            format!("/api/triggers/{RULE_NAME}/approve"),
            Some(json!({ "shown": need_of(&root, RULE_NAME) })),
        ),
        (
            "DELETE",
            format!("/api/triggers/{RULE_NAME}"),
            Some(json!({})),
        ),
    ];

    // The rules file as it stands: no refused write may change a byte.
    let rules_file = apb_core::signed_store::file::<apb_core::trigger::RulesFile>(
        &root,
        apb_core::scope::Scope::Project,
        "rules",
    );
    let before = std::fs::read(&rules_file).unwrap();
    let entry_before = serde_json::to_value(entry_of(&root, RULE_NAME)).unwrap();

    // Keyless: the plain local dashboard. Every write is refused.
    for (method, uri, body) in &writes {
        let (status, json) = send(
            build_router(AppState::new(root.clone())),
            method,
            uri,
            body.clone(),
            None,
            None,
        )
        .await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{method} {uri}: {json:#}");
        assert_eq!(
            json["error"], "trigger_requires_session",
            "{method} {uri}: {json:#}"
        );
    }

    // A bearer key is a machine's credential, not a person's session: every
    // write is refused the same way.
    let (key, state) = keyed(&root);
    for (method, uri, body) in &writes {
        let (status, json) = send(
            build_router(state.clone()),
            method,
            uri,
            body.clone(),
            None,
            Some(&key),
        )
        .await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{method} {uri}: {json:#}");
        assert_eq!(
            json["error"], "trigger_requires_session",
            "{method} {uri}: {json:#}"
        );
    }
    assert_eq!(
        std::fs::read(&rules_file).unwrap(),
        before,
        "the refused writes left the rules file as it was"
    );
    assert_eq!(
        serde_json::to_value(entry_of(&root, RULE_NAME)).unwrap(),
        entry_before,
        "the rule is unchanged"
    );

    // A signed-in session writes. The set answers the saved rule's view.
    let cookie = session_cookie(&state, &key).await;
    let (status, json) = send(
        build_router(state.clone()),
        "PUT",
        &format!("/api/triggers/{RULE_NAME}"),
        Some(json!({ "rule": rule_json() })),
        Some(&cookie),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{json:#}");
    assert_eq!(json["name"], RULE_NAME, "{json:#}");
    assert_eq!(json["playbook"], RULE_PB, "{json:#}");

    // The approval with the need shown right before it stores one.
    let (status, json) = send(
        build_router(state.clone()),
        "POST",
        &format!("/api/triggers/{RULE_NAME}/approve"),
        Some(json!({ "shown": need_of(&root, RULE_NAME) })),
        Some(&cookie),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{json:#}");
    assert_eq!(json["approved"], true, "{json:#}");
    assert!(
        entry_of(&root, RULE_NAME).approval.is_some(),
        "the approval is stored"
    );
}

/// A journal the reader refuses reaches the list as the rule's
/// `journal_error`: the one sentence with the remedy, what the dashboard's
/// rule row shows.
#[tokio::test]
async fn the_list_shows_a_refused_journal() {
    let rig = rig_with_rule().await;
    let root = rig.root();
    let inc = entry_of(&root, RULE_NAME).incarnation;
    // A directory at the journal's path: the bounded read refuses it.
    let log = root
        .join(".apb/trigger-state/log")
        .join(format!("{inc}.jsonl"));
    std::fs::create_dir_all(log.parent().unwrap()).unwrap();
    std::fs::create_dir(&log).unwrap();
    let (key, state) = keyed(&root);
    let cookie = session_cookie(&state, &key).await;
    let (status, json) = send(
        build_router(state.clone()),
        "GET",
        "/api/triggers",
        None,
        Some(&cookie),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{json:#}");
    let sentence = json["rules"][0]["journal_error"]
        .as_str()
        .expect("the refusal is the row's")
        .to_string();
    assert!(sentence.contains("journal_refused"), "{sentence}");
    assert!(
        sentence.contains("; remove the rule and save it again, or delete .apb/trigger-state/log/"),
        "{sentence}"
    );
}

/// A state entry the removal cannot delete never fails the DELETE: the
/// answer says the rule was removed and carries `state_left`.
#[tokio::test]
async fn remove_answers_a_state_entry_it_could_not_delete() {
    let rig = rig_with_rule().await;
    let root = rig.root();
    // The state file stops verifying: the store refuses to write it.
    let state = root.join(".apb/trigger-state/project/state.json");
    std::fs::create_dir_all(state.parent().unwrap()).unwrap();
    std::fs::write(&state, b"not a signed state").unwrap();
    let (key, state) = keyed(&root);
    let cookie = session_cookie(&state, &key).await;
    let (status, json) = send(
        build_router(state.clone()),
        "DELETE",
        &format!("/api/triggers/{RULE_NAME}"),
        Some(json!({})),
        Some(&cookie),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{json:#}");
    assert_eq!(json["removed"], true, "{json:#}");
    let left = json["state_left"]
        .as_str()
        .expect("the answer names what stays behind");
    assert!(
        left.starts_with(".apb/trigger-state/project/state.json"),
        "{left}"
    );
    let rules = apb_core::signed_store::read::<apb_core::trigger::RulesFile>(
        &root,
        apb_core::scope::Scope::Project,
        "rules",
    )
    .expect("the rules file reads");
    assert!(rules.file.rules.is_empty(), "the rule is gone either way");
}

/// A journal the removal cannot delete beneath the anchor (a link at
/// `log`) never fails the DELETE: the answer says the rule was removed and
/// carries `journal_left`, never `trigger_rules_unreadable`.
#[tokio::test]
async fn remove_answers_a_journal_it_could_not_delete() {
    let rig = rig_with_rule().await;
    let root = rig.root();
    let elsewhere = tempfile::tempdir().unwrap();
    let log = root.join(".apb/trigger-state/log");
    std::fs::create_dir_all(&log).unwrap();
    std::fs::create_dir_all(elsewhere.path().join("log")).unwrap();
    std::fs::rename(&log, elsewhere.path().join("log-real")).unwrap();
    std::os::unix::fs::symlink(elsewhere.path().join("log"), &log).unwrap();
    let (key, state) = keyed(&root);
    let cookie = session_cookie(&state, &key).await;
    let (status, json) = send(
        build_router(state.clone()),
        "DELETE",
        &format!("/api/triggers/{RULE_NAME}"),
        Some(json!({})),
        Some(&cookie),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{json:#}");
    assert_eq!(json["removed"], true, "{json:#}");
    let left = json["journal_left"]
        .as_str()
        .expect("the answer names what stays behind");
    assert!(left.starts_with(".apb/trigger-state/log/"), "{left}");
    assert!(
        !json.to_string().contains("trigger_rules_unreadable"),
        "{json:#}"
    );
    let rules = apb_core::signed_store::read::<apb_core::trigger::RulesFile>(
        &root,
        apb_core::scope::Scope::Project,
        "rules",
    )
    .expect("the rules file reads");
    assert!(rules.file.rules.is_empty(), "the rule is gone either way");
}

/// The approval posted back must be the need as it stands: a need of a
/// changed rule (here, of the changed target) is refused and stores nothing.
#[tokio::test]
async fn approve_refuses_a_changed_need() {
    let rig = Rig::new(RULE_PB, &agent_pb(RULE_PB)).await;
    let root = rig.root();
    save_rule(&root, &rule_doc());
    let shown = need_of(&root, RULE_NAME);

    // The target changes while the person looks at the need.
    let changed = agent_pb(RULE_PB).replace("prompt: x", "prompt: changed");
    write_playbook(&root, RULE_PB, &changed);
    trust_current(&root, RULE_PB);

    let (key, state) = keyed(&root);
    let cookie = session_cookie(&state, &key).await;
    let (status, json) = send(
        build_router(state.clone()),
        "POST",
        &format!("/api/triggers/{RULE_NAME}/approve"),
        Some(json!({ "shown": shown })),
        Some(&cookie),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{json:#}");
    assert!(
        json.to_string().contains("trigger_approval_changed"),
        "{json:#}"
    );
    assert!(
        entry_of(&root, RULE_NAME).approval.is_none(),
        "nothing was approved"
    );
}

/// An unknown field in the shown need (or in the body) is 422.
#[tokio::test]
async fn approve_refuses_an_unknown_field() {
    let rig = rig_with_rule().await;
    let root = rig.root();
    let (key, state) = keyed(&root);
    let cookie = session_cookie(&state, &key).await;

    let mut need = need_of(&root, RULE_NAME);
    need["extra_field"] = json!("surprise");
    let (status, json) = send(
        build_router(state.clone()),
        "POST",
        &format!("/api/triggers/{RULE_NAME}/approve"),
        Some(json!({ "shown": need })),
        Some(&cookie),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{json:#}");

    // An unknown field on the body itself is 422 too.
    let need = need_of(&root, RULE_NAME);
    let (status, _) = send(
        build_router(state.clone()),
        "POST",
        &format!("/api/triggers/{RULE_NAME}/approve"),
        Some(json!({ "shown": need, "force": true })),
        Some(&cookie),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
}

/// A rule document naming `enabled` is 422 `trigger_enabled_not_settable`:
/// only enable and disable change it.
#[tokio::test]
async fn set_refuses_enabled() {
    let rig = rig_with_rule().await;
    let root = rig.root();
    let (key, state) = keyed(&root);
    let cookie = session_cookie(&state, &key).await;

    for enabled in [true, false] {
        let mut rule = rule_json();
        rule["enabled"] = json!(enabled);
        let (status, json) = send(
            build_router(state.clone()),
            "PUT",
            &format!("/api/triggers/{RULE_NAME}"),
            Some(json!({ "rule": rule })),
            Some(&cookie),
            None,
        )
        .await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{json:#}");
        assert!(
            json.to_string().contains("trigger_enabled_not_settable"),
            "{json:#}"
        );
    }
    // Nothing was written: the entry is still the saved one, unapproved.
    let entry = entry_of(&root, RULE_NAME);
    assert!(entry.enabled, "a new rule starts enabled");
    assert!(entry.approval.is_none());
}

/// A write whose locks are held elsewhere blocks in `spawn_blocking`, not
/// on the runtime: while the write waits, the runtime still serves reads.
#[tokio::test]
async fn trigger_routes_run_blocking_work_off_the_runtime() {
    let rig = rig_with_rule().await;
    let root = rig.root();
    let (key, state) = keyed(&root);
    let cookie = session_cookie(&state, &key).await;

    // Hold the rule's change lock from the test, so the write's
    // `hold_rule_lock` has to wait for it.
    let lock_dir = root.join(".apb/trigger-state");
    let held = apb_core::fsutil::try_hold_lock(&lock_dir, &format!("{RULE_NAME}.change.lock"))
        .expect("the change lock is free");

    let app = build_router(state.clone());
    let cookie_owned = cookie.clone();
    let write = tokio::spawn(async move {
        send(
            app,
            "POST",
            &format!("/api/triggers/{RULE_NAME}/disable"),
            Some(json!({})),
            Some(&cookie_owned),
            None,
        )
        .await
    });

    // While the write waits for the lock, a read still answers: the write
    // did not pin the runtime. The read goes out with the session, as the
    // keyed server requires a credential for it too.
    let read = send(
        build_router(state.clone()),
        "GET",
        "/api/triggers",
        None,
        Some(&cookie),
        None,
    )
    .await;
    assert_eq!(read.0, StatusCode::OK, "{:#}", read.1);

    drop(held);
    let (status, json) = tokio::time::timeout(std::time::Duration::from_secs(30), write)
        .await
        .expect("the write finished once the lock was free")
        .unwrap();
    assert_eq!(status, StatusCode::OK, "{json:#}");
    assert_eq!(json["enabled"], false, "{json:#}");
    assert!(!entry_of(&root, RULE_NAME).enabled);
}

/// The dry test is allowed on every channel and answers the report without
/// values; from a run step it is refused 409 (design D1 "HTTP" item 3).
#[tokio::test]
async fn the_test_route_answers_on_every_channel() {
    let rig = rig_with_rule().await;
    let root = rig.root();
    apb_core::connector::inbox::Inbox::open("github", "main")
        .unwrap()
        .append_typed("e1", Some("issues"), &json!({ "title": "an issue" }))
        .unwrap();

    // Keyless, no session: the test is a read.
    let (status, json) = send(
        build_router(AppState::new(root.clone())),
        "POST",
        &format!("/api/triggers/{RULE_NAME}/test"),
        Some(json!({})),
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{json:#}");
    assert_eq!(json["rule"], RULE_NAME, "{json:#}");
    assert!(
        json.get("params").and_then(|p| p[0].get("value")).is_none(),
        "the report carries no values: {json:#}"
    );

    // From a run step: 409 with the design's code.
    let _run = common::EnvVar::set("APB_RUN_ID", "tw-run-9");
    let (status, json) = send(
        build_router(AppState::new(root.clone())),
        "POST",
        &format!("/api/triggers/{RULE_NAME}/test"),
        Some(json!({})),
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{json:#}");
    assert!(
        json.to_string()
            .contains("trigger_test_refused_in_run_step"),
        "{json:#}"
    );
}
