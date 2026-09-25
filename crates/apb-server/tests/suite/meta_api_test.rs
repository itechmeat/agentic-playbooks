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

    // /api/agents: the ten built-in probes are always enumerated (claude,
    // codex, agy, opencode, pi, hermes, grok, cursor, qoder, zcode).
    let app = build_router(AppState::new(root.clone()));
    let (status, json) = get_json(app, "/api/agents").await;
    assert_eq!(status, StatusCode::OK);
    let agents = json["agents"].as_array().expect("agents array");
    assert_eq!(agents.len(), 10, "expected the ten built-in probes: {json}");
    assert!(agents.iter().any(|a| a["agent"] == "claude"));
    assert!(agents.iter().any(|a| a["agent"] == "grok"));
    assert!(agents.iter().any(|a| a["agent"] == "cursor"));
    assert!(agents.iter().any(|a| a["agent"] == "qoder"));
    assert!(agents.iter().any(|a| a["agent"] == "zcode"));

    // /api/models: the curated table and the claude static list.
    let app = build_router(AppState::new(root.clone()));
    let (status, json) = get_json(app, "/api/models").await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        !json["models"].as_array().expect("models array").is_empty(),
        "models table must not be empty: {json}"
    );
    assert!(!json["claude_static"].as_array().unwrap().is_empty());

    // options_by_agent (issue #42 finding 9): codex's option set is its
    // closed static list (`codex_static`), in order, the default first.
    let ids = |opts: &serde_json::Value| -> Vec<String> {
        opts.as_array()
            .expect("options array")
            .iter()
            .map(|o| o["id"].as_str().unwrap().to_string())
            .collect()
    };
    let codex_static: Vec<String> = json["codex_static"]
        .as_array()
        .expect("codex_static array")
        .iter()
        .map(|m| m.as_str().unwrap().to_string())
        .collect();
    assert_eq!(
        codex_static,
        [
            "gpt-6-sol",
            "gpt-6-astra",
            "gpt-6-luna",
            "gpt-5.6-sol",
            "gpt-5.6-terra",
            "gpt-5.6-luna",
            "gpt-5.5"
        ]
    );
    assert_eq!(ids(&json["options_by_agent"]["codex"]), codex_static);
    assert!(
        json["options_by_agent"]["codex"]
            .as_array()
            .unwrap()
            .iter()
            .all(|o| o["vendor"] == "openai"),
        "every codex option is tied to the openai vendor: {json}"
    );
    // claude's option set is its closed static list, the same list the
    // response reports as `claude_static`.
    let claude_static: Vec<String> = json["claude_static"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| m.as_str().unwrap().to_string())
        .collect();
    assert_eq!(ids(&json["options_by_agent"]["claude"]), claude_static);
    // One snapshot: `/api/models` carries the same agents `/api/agents` serves.
    assert_eq!(
        json["agents"]
            .as_array()
            .expect("agents in /api/models")
            .len(),
        10
    );
    // zcode's option set is apb's allowlist, bare ids.
    assert_eq!(
        ids(&json["options_by_agent"]["zcode"]),
        ["GLM-5.3", "GLM-5.3-Flash"]
    );

    // An aggregator (no single vendor tie) keeps the whole curated table.
    let table_len = json["models"].as_array().unwrap().len();
    let opencode_opts = json["options_by_agent"]["opencode"]
        .as_array()
        .expect("options_by_agent.opencode array");
    assert_eq!(
        opencode_opts.len(),
        table_len,
        "an aggregator keeps every curated row: {json}"
    );

    // A codex config.toml naming a model outside the static list does not
    // extend the closed list.
    let codex_dir = home.path().join(".codex");
    std::fs::create_dir_all(&codex_dir).unwrap();
    std::fs::write(
        codex_dir.join("config.toml"),
        "model = \"gpt-5-codex-not-yet-in-table\"\n",
    )
    .unwrap();
    let app = build_router(AppState::new(root.clone()));
    let (status, json) = get_json(app, "/api/models").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        ids(&json["options_by_agent"]["codex"]),
        codex_static,
        "a config-only model must not join codex's closed list: {json}"
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

/// The lists are decided by the server alone: neither endpoint may be kept by
/// an HTTP cache.
#[tokio::test]
async fn agent_and_model_lists_are_never_http_cached() {
    let _guard = crate::common::env_lock().await;
    let proj = tempfile::tempdir().unwrap();
    let cfg = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    unsafe {
        std::env::set_var("APB_CONFIG_DIR", cfg.path());
        std::env::set_var("HOME", home.path());
        std::env::set_var("APB_PROBE_TIMEOUT_MS", "300");
    }
    apb_core::registry::init_project(proj.path()).unwrap();
    for uri in ["/api/models", "/api/agents"] {
        let app = build_router(AppState::new(proj.path().to_path_buf()));
        let res = app
            .oneshot(Request::get(uri).body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        assert_eq!(
            res.headers()
                .get(axum::http::header::CACHE_CONTROL)
                .map(|v| v.to_str().unwrap()),
            Some("no-store"),
            "{uri}"
        );
    }
}
