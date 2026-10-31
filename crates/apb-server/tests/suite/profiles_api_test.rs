//! /api/profiles: the list of profiles with trust status and creation via
//! POST. Mutates process-wide env (APB_CONFIG_DIR/HOME), so it takes
//! `common::env_lock()` to serialize against other env-mutating tests in
//! this consolidated binary (see `crate::common`). POST auto-approves the
//! bundle in the TrustStore, so we isolate the config into a temp directory.

use crate::common::EnvVar;
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

async fn post_json(
    app: axum::Router,
    uri: &str,
    body: serde_json::Value,
) -> (StatusCode, serde_json::Value) {
    let req = Request::builder()
        .method("POST")
        .uri(uri)
        .header("content-type", "application/json")
        .body(Body::from(serde_json::to_vec(&body).unwrap()))
        .unwrap();
    let res = app.oneshot(req).await.unwrap();
    let status = res.status();
    let bytes = res.into_body().collect().await.unwrap().to_bytes();
    let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null);
    (status, json)
}

#[tokio::test]
async fn profiles_list_then_create_then_trusted() {
    let _guard = crate::common::env_lock().await;
    let proj = tempfile::tempdir().unwrap();
    let cfg = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    unsafe {
        std::env::set_var("APB_CONFIG_DIR", cfg.path());
        std::env::set_var("HOME", home.path());
    }
    apb_core::registry::init_project(proj.path()).unwrap();
    // Seed a profile on disk directly.
    let dir = proj.path().join(".apb/profiles/seeded");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("profile.yaml"),
        "name: seeded\ndescription: d\nexecutor:\n  agent: claude\n  model: haiku\n",
    )
    .unwrap();
    std::fs::write(dir.join("SOUL.md"), "").unwrap();

    let root = proj.path().to_path_buf();

    // GET lists the seeded profile.
    let app = build_router(AppState::new(root.clone()));
    let (status, json) = get_json(app, "/api/profiles").await;
    assert_eq!(status, StatusCode::OK);
    let profiles = json["profiles"].as_array().expect("profiles array");
    assert!(
        profiles.iter().any(|p| p["name"] == "seeded"),
        "GET must list the seeded profile: {json}"
    );

    // POST creates a new profile (auto-approves its bundle).
    let app = build_router(AppState::new(root.clone()));
    let (status, created) = post_json(
        app,
        "/api/profiles",
        serde_json::json!({
            "name": "made",
            "scope": "project",
            "agent": "claude",
            "model": "sonnet",
            "description": "via api"
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "POST failed: {created}");
    assert!(
        proj.path()
            .join(".apb/profiles/made/profile.yaml")
            .is_file()
    );

    // Subsequent GET shows it as trusted (bundle auto-approved on create).
    let app = build_router(AppState::new(root.clone()));
    let (_s, json) = get_json(app, "/api/profiles").await;
    let made = json["profiles"]
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p["name"] == "made")
        .expect("made profile present");
    assert_eq!(
        made["trusted"],
        serde_json::json!(true),
        "created profile must be trusted: {made}"
    );

    unsafe {
        std::env::remove_var("APB_CONFIG_DIR");
        std::env::remove_var("HOME");
    }
}

#[tokio::test]
async fn profile_update_preserves_environment_when_body_omits_it() {
    let _guard = crate::common::env_lock().await;
    let proj = tempfile::tempdir().unwrap();
    let cfg = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    let _env = [
        EnvVar::set("APB_CONFIG_DIR", cfg.path()),
        EnvVar::set("HOME", home.path()),
    ];
    apb_core::registry::init_project(proj.path()).unwrap();
    // Seed a profile that opted into the full environment.
    let dir = proj.path().join(".apb/profiles/herm");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("profile.yaml"),
        "name: herm\ndescription: d\nexecutor:\n  agent: claude\n  model: haiku\nenvironment: full\nprocess_env:\n  policy: allowlist\n  allow: [GH_TOKEN]\n",
    )
    .unwrap();
    std::fs::write(dir.join("SOUL.md"), "").unwrap();

    let root = proj.path().to_path_buf();

    // The web edit flow reads the profile (for its digest) before it writes.
    let app = build_router(AppState::new(root.clone()));
    let (status, got) = get_json(app, "/api/profiles/herm?scope=project").await;
    assert_eq!(status, StatusCode::OK, "GET failed: {got}");
    let digest = got["profile_digest"].as_str().expect("digest").to_string();

    // POST an update that changes the description but carries no
    // `environment` (the web body cannot express it). Resetting it here would
    // silently strip the owner's opt-in, so the stored value must survive.
    let app = build_router(AppState::new(root.clone()));
    let (status, updated) = post_json(
        app,
        "/api/profiles",
        serde_json::json!({
            "name": "herm",
            "scope": "project",
            "agent": "claude",
            "model": "haiku",
            "description": "edited",
            "expected_digest": digest,
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "POST update failed: {updated}");

    let written = std::fs::read_to_string(dir.join("profile.yaml")).unwrap();
    assert!(
        written.contains("environment: full"),
        "web update must preserve environment: full, got: {written}"
    );
    assert!(
        written.contains("policy: allowlist") && written.contains("GH_TOKEN"),
        "web update must preserve process_env, got: {written}"
    );
}

/// POST writes a `process_env` allowlist, `allow` alone implying it;
/// refuses a malformed one and `allow` under `inherit` with 400, writing
/// nothing; and an explicit `inherit` on an update drops a stored
/// allowlist.
#[tokio::test]
async fn profile_post_writes_validates_and_reverts_process_env() {
    let _lock = crate::common::env_lock().await;
    let proj = tempfile::tempdir().unwrap();
    let cfg = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    let _env = [
        EnvVar::set("APB_CONFIG_DIR", cfg.path()),
        EnvVar::set("HOME", home.path()),
    ];
    apb_core::registry::init_project(proj.path()).unwrap();
    let root = proj.path().to_path_buf();
    let body = |name: &str, process_env: serde_json::Value| {
        serde_json::json!({
            "name": name,
            "scope": "project",
            "agent": "claude",
            "model": "haiku",
            "process_env": process_env,
        })
    };
    let profile_yaml = |name: &str| {
        std::fs::read_to_string(
            proj.path()
                .join(format!(".apb/profiles/{name}/profile.yaml")),
        )
        .unwrap()
    };

    let app = build_router(AppState::new(root.clone()));
    let (status, _) = post_json(
        app,
        "/api/profiles",
        body("envok", serde_json::json!({ "allow": ["NPM_CONFIG_*"] })),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let written = profile_yaml("envok");
    assert!(written.contains("policy: allowlist"), "{written}");
    assert!(written.contains("NPM_CONFIG_*"), "{written}");

    for (name, bad) in [
        (
            "envbad",
            serde_json::json!({ "policy": "allowlist", "allow": ["*"] }),
        ),
        (
            "envmix",
            serde_json::json!({ "policy": "inherit", "allow": ["GH_TOKEN"] }),
        ),
    ] {
        let app = build_router(AppState::new(root.clone()));
        let (status, _) = post_json(app, "/api/profiles", body(name, bad)).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{name}");
        assert!(!proj.path().join(format!(".apb/profiles/{name}")).exists());
    }

    let app = build_router(AppState::new(root.clone()));
    let (_, got) = get_json(app, "/api/profiles/envok?scope=project").await;
    let digest = got["profile_digest"].as_str().expect("digest").to_string();
    let mut revert = body("envok", serde_json::json!({ "policy": "inherit" }));
    revert["expected_digest"] = serde_json::json!(digest);
    let app = build_router(AppState::new(root.clone()));
    let (status, _) = post_json(app, "/api/profiles", revert).await;
    assert_eq!(status, StatusCode::OK);
    let written = profile_yaml("envok");
    assert!(!written.contains("process_env"), "{written}");
}
