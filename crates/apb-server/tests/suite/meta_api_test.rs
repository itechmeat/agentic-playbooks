//! /api/agents, /api/models, /api/skills - the read endpoints that feed the
//! profile form's agent/model combobox and skills toggle list. Mutates
//! process-wide env (APB_CONFIG_DIR / HOME / probe timeout), so it takes
//! `common::env_lock()` to serialize against other env-mutating tests in
//! this consolidated binary (see `crate::common`).

use apb_server::{AppState, build_router};
use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use tower::ServiceExt;

async fn get_json(app: axum::Router, uri: &str) -> (StatusCode, serde_json::Value) {
    let res = app
        .oneshot(Request::get(uri).body(Body::empty()).unwrap())
        .await
        .unwrap();
    let status = res.status();
    let bytes = res.into_body().collect().await.unwrap().to_bytes();
    let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null);
    (status, json)
}

#[tokio::test]
async fn agents_models_and_skills_endpoints() {
    let _guard = crate::common::env_lock().await;
    let proj = tempfile::tempdir().unwrap();
    let cfg = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    unsafe {
        std::env::set_var("APB_CONFIG_DIR", cfg.path());
        std::env::set_var("HOME", home.path());
        // Keep detection fast: a missing binary resolves instantly, but cap the
        // probe timeout so a present one cannot stall the test.
        std::env::set_var("APB_PROBE_TIMEOUT_MS", "300");
    }
    apb_core::registry::init_project(proj.path()).unwrap();

    // A project skill and a global skill, plus an invalid-named dir to confirm
    // it is filtered out.
    let proj_skills = proj.path().join(".agents/skills");
    std::fs::create_dir_all(proj_skills.join("proj-skill")).unwrap();
    std::fs::create_dir_all(proj_skills.join("Bad Name")).unwrap();
    let global_skills = home.path().join(".agents/skills");
    std::fs::create_dir_all(global_skills.join("glob-skill")).unwrap();

    let root = proj.path().to_path_buf();

    // /api/agents: the eight built-in probes are always enumerated (claude,
    // codex, agy, opencode, pi, hermes, grok, cursor).
    let app = build_router(AppState::new(root.clone()));
    let (status, json) = get_json(app, "/api/agents").await;
    assert_eq!(status, StatusCode::OK);
    let agents = json["agents"].as_array().expect("agents array");
    assert_eq!(
        agents.len(),
        8,
        "expected the eight built-in probes: {json}"
    );
    assert!(agents.iter().any(|a| a["agent"] == "claude"));
    assert!(agents.iter().any(|a| a["agent"] == "grok"));
    assert!(agents.iter().any(|a| a["agent"] == "cursor"));

    // /api/models: the curated table and the claude static list.
    let app = build_router(AppState::new(root.clone()));
    let (status, json) = get_json(app, "/api/models").await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        !json["models"].as_array().expect("models array").is_empty(),
        "models table must not be empty: {json}"
    );
    assert!(!json["claude_static"].as_array().unwrap().is_empty());

    // options_by_agent (issue #42 finding 9): codex ties to the curated
    // table's openai rows - detection must only annotate, never narrow the
    // set to the single model string-scanned from config.toml.
    let codex_opts = json["options_by_agent"]["codex"]
        .as_array()
        .expect("options_by_agent.codex array");
    let table_len = json["models"].as_array().unwrap().len();
    let openai_rows = json["models"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|m| m["vendor"] == "openai")
        .count();
    assert!(
        openai_rows > 1,
        "fixture assumption: the curated table carries more than one openai row"
    );
    assert_eq!(
        codex_opts.len(),
        openai_rows,
        "codex's option set is the curated openai rows, not a single detected model: {json}"
    );
    assert!(
        codex_opts.iter().all(|o| o["vendor"] == "openai"),
        "every codex option is tied to the openai vendor: {json}"
    );
    assert!(
        codex_opts.iter().all(|o| o["detected"] == false),
        "an empty ~/.codex/config.toml annotates nothing detected: {json}"
    );

    // An aggregator (no single vendor tie) keeps the whole curated table.
    let opencode_opts = json["options_by_agent"]["opencode"]
        .as_array()
        .expect("options_by_agent.opencode array");
    assert_eq!(
        opencode_opts.len(),
        table_len,
        "an aggregator keeps every curated row: {json}"
    );

    // Now write a codex config.toml naming a curated model AND a model the
    // table does not carry, and re-fetch: the curated model must be
    // annotated `detected`, the option COUNT must stay unchanged for it (not
    // narrowed to it), and the config-only model must still appear as its
    // own extra entry rather than being dropped.
    let curated_openai_id = json["models"]
        .as_array()
        .unwrap()
        .iter()
        .find(|m| m["vendor"] == "openai")
        .unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();
    let codex_dir = home.path().join(".codex");
    std::fs::create_dir_all(&codex_dir).unwrap();
    std::fs::write(
        codex_dir.join("config.toml"),
        format!("model = \"{curated_openai_id}\"\n[model_providers.openai]\n"),
    )
    .unwrap();
    let app = build_router(AppState::new(root.clone()));
    let (status, json) = get_json(app, "/api/models").await;
    assert_eq!(status, StatusCode::OK);
    let codex_opts = json["options_by_agent"]["codex"]
        .as_array()
        .expect("options_by_agent.codex array");
    assert_eq!(
        codex_opts.len(),
        openai_rows,
        "one detected model annotates a row, it must not shrink the option set: {json}"
    );
    let detected_row = codex_opts
        .iter()
        .find(|o| o["id"] == curated_openai_id.as_str())
        .expect("the config-named model is still offered");
    assert_eq!(
        detected_row["detected"], true,
        "it is annotated detected: {json}"
    );
    assert!(
        codex_opts
            .iter()
            .any(|o| o["id"] != curated_openai_id.as_str() && o["detected"] == false),
        "a curated sibling model stays offered, undetected: {json}"
    );

    std::fs::write(
        codex_dir.join("config.toml"),
        "model = \"gpt-5-codex-not-yet-in-table\"\n",
    )
    .unwrap();
    let app = build_router(AppState::new(root.clone()));
    let (status, json) = get_json(app, "/api/models").await;
    assert_eq!(status, StatusCode::OK);
    let codex_opts = json["options_by_agent"]["codex"]
        .as_array()
        .expect("options_by_agent.codex array");
    assert!(
        codex_opts
            .iter()
            .any(|o| o["id"] == "gpt-5-codex-not-yet-in-table" && o["detected"] == true),
        "a config-only model absent from the curated table is still present, as a detected extra: {json}"
    );
    assert_eq!(
        codex_opts.len(),
        openai_rows + 1,
        "the curated rows are kept AND the config-only model is appended, not swapped in: {json}"
    );

    // /api/skills project scope: project skills first, then global; invalid
    // names filtered.
    let app = build_router(AppState::new(root.clone()));
    let (status, json) = get_json(app, "/api/skills?scope=project").await;
    assert_eq!(status, StatusCode::OK);
    let names: Vec<&str> = json["skills"]
        .as_array()
        .expect("skills array")
        .iter()
        .map(|s| s["name"].as_str().unwrap_or(""))
        .collect();
    assert!(
        names.contains(&"proj-skill"),
        "project skill listed: {json}"
    );
    assert!(
        names.contains(&"glob-skill"),
        "global skill visible in project scope: {json}"
    );
    assert!(
        !names.contains(&"Bad Name"),
        "invalid name filtered: {json}"
    );

    // /api/skills global scope: only global skills.
    let app = build_router(AppState::new(root.clone()));
    let (status, json) = get_json(app, "/api/skills?scope=global").await;
    assert_eq!(status, StatusCode::OK);
    let names: Vec<&str> = json["skills"]
        .as_array()
        .expect("skills array")
        .iter()
        .map(|s| s["name"].as_str().unwrap_or(""))
        .collect();
    assert!(names.contains(&"glob-skill"));
    assert!(
        !names.contains(&"proj-skill"),
        "project skill must not leak into global scope: {json}"
    );
}
