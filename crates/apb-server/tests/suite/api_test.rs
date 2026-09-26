use apb_server::{AppState, build_router};
use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use std::fs;
use tower::ServiceExt;

const VALID: &str = include_str!("../../../apb-core/tests/fixtures/valid.yaml");

fn seed() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    apb_core::registry::init_project(dir.path()).unwrap();
    let vdir = dir.path().join(".apb/playbooks/implement-task/1.0.0");
    fs::create_dir_all(&vdir).unwrap();
    fs::write(vdir.join("playbook.yaml"), VALID).unwrap();
    fs::write(
        dir.path().join(".apb/playbooks/implement-task/current"),
        "1.0.0",
    )
    .unwrap();
    fs::create_dir_all(dir.path().join(".apb/profiles/architect")).unwrap();
    dir
}

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

async fn json_request(
    app: axum::Router,
    method: &str,
    uri: &str,
    body: serde_json::Value,
) -> (StatusCode, serde_json::Value) {
    let req = Request::builder()
        .method(method)
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
async fn health_ok() {
    let dir = seed();
    let app = build_router(AppState::new(dir.path().to_path_buf()));
    let (status, json) = get_json(app, "/api/health").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json["status"], "ok");
}

#[tokio::test]
async fn playbooks_list() {
    let dir = seed();
    let app = build_router(AppState::new(dir.path().to_path_buf()));
    let (status, json) = get_json(app, "/api/playbooks").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json[0]["id"], "implement-task");
    assert_eq!(json[0]["current"], "1.0.0");
}

#[tokio::test]
async fn playbook_detail_includes_model_and_validation() {
    let dir = seed();
    let app = build_router(AppState::new(dir.path().to_path_buf()));
    let (status, json) = get_json(app, "/api/playbooks/implement-task").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json["version"], "1.0.0");
    assert_eq!(json["playbook"]["nodes"][0]["type"], "start");
    assert!(json["yaml"].as_str().unwrap().contains("implement-task"));
    assert!(json["validation"].as_array().is_some());
}

#[tokio::test]
async fn unknown_playbook_404() {
    let dir = seed();
    let app = build_router(AppState::new(dir.path().to_path_buf()));
    let (status, _) = get_json(app, "/api/playbooks/ghost").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn playbook_id_path_traversal_is_rejected() {
    let dir = seed();
    let app = build_router(AppState::new(dir.path().to_path_buf()));
    let (status, _) = get_json(app.clone(), "/api/playbooks/..%2F..%2Fetc").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, _) = get_json(app, "/api/playbooks/%2Fetc%2Fpasswd").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn playbook_version_path_traversal_is_rejected() {
    let dir = seed();
    // A marker file outside the playbook directory - if traversal succeeds,
    // its content will leak into the response via the `layout` field.
    fs::write(dir.path().join("secret.yaml"), "leaked: true\n").unwrap();
    let app = build_router(AppState::new(dir.path().to_path_buf()));
    let (status, _) = get_json(
        app.clone(),
        "/api/playbooks/implement-task?version=..%2F..%2F..%2Fsecret",
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    let res = app
        .oneshot(
            Request::get("/api/playbooks/implement-task?version=..%2F..%2F..%2Fsecret")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::NOT_FOUND);
    let bytes = res.into_body().collect().await.unwrap().to_bytes();
    let body = String::from_utf8_lossy(&bytes);
    assert!(
        !body.contains("leaked"),
        "response leaked file contents: {body}"
    );
}

#[tokio::test]
async fn run_report_returns_seeded_text() {
    let dir = seed();
    let run_dir = dir.path().join(".apb/runs/run-1/supervisor");
    fs::create_dir_all(&run_dir).unwrap();
    fs::write(
        run_dir.join("report.md"),
        "# Supervisor report\n\nall good\n",
    )
    .unwrap();
    let app = build_router(AppState::new(dir.path().to_path_buf()));
    let (status, json) = get_json(app, "/api/runs/run-1/report").await;
    assert_eq!(status, StatusCode::OK);
    assert!(json["report"].as_str().unwrap().contains("all good"));
}

#[tokio::test]
async fn run_report_unknown_run_404() {
    let dir = seed();
    let app = build_router(AppState::new(dir.path().to_path_buf()));
    let (status, _) = get_json(app, "/api/runs/ghost/report").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn run_report_path_traversal_is_rejected() {
    let dir = seed();
    let app = build_router(AppState::new(dir.path().to_path_buf()));
    let (status, _) = get_json(app, "/api/runs/..%2F..%2Fetc/report").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

/// F26: `<config_dir>/serve.lock` names the one dashboard serving a config
/// dir. A second dashboard (another port) must not take it over from a live
/// one, and no dashboard may delete a lock it does not own; a lock left by a
/// dead process is replaced.
#[test]
fn global_lock_is_owned_by_one_live_dashboard() {
    use apb_server::lock::{GlobalLock, LockError};
    let cfg = tempfile::tempdir().unwrap();
    let path = cfg.path().join("serve.lock");
    // Another dashboard: a live process whose program is named `apb`.
    use std::os::unix::process::CommandExt;
    struct Reap(std::process::Child);
    impl Drop for Reap {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
    let mut other = Reap(
        std::process::Command::new("/bin/sh")
            .arg0("apb")
            .args(["-c", "sleep 30; :"])
            .spawn()
            .unwrap(),
    );
    let theirs = format!(
        r#"{{"port":7321,"pid":{},"root_fingerprint":"x","instance_id":"theirs"}}"#,
        other.0.id()
    );
    fs::write(&path, &theirs).unwrap();

    let refused = GlobalLock::acquire(cfg.path(), 7400);
    assert!(
        matches!(refused, Err(LockError::Held { port: 7321, .. })),
        "a live dashboard's lock must be refused"
    );
    assert_eq!(fs::read_to_string(&path).unwrap(), theirs, "left untouched");

    // Once that process is gone its lock is stale and is replaced.
    other.0.kill().unwrap();
    other.0.wait().unwrap();
    let ours = GlobalLock::acquire(cfg.path(), 7400).expect("stale lock replaced");
    assert!(fs::read_to_string(&path).unwrap().contains("7400"));
    // Someone else's lock in its place is not ours to delete.
    fs::write(&path, &theirs).unwrap();
    drop(ours);
    assert!(path.exists(), "a lock the dashboard does not own is kept");

    fs::remove_file(&path).unwrap();
    let ours = GlobalLock::acquire(cfg.path(), 7400).unwrap();
    drop(ours);
    assert!(!path.exists(), "the owner removes its own lock");
}

#[tokio::test]
async fn post_playbook_creates_then_get_finds_it() {
    let _cfg = crate::common::config_sandbox().await;
    let dir = seed();
    let app = build_router(AppState::new(dir.path().to_path_buf()));
    let yaml = VALID.replace("id: implement-task", "id: brand-new");
    let (status, json) = json_request(
        app.clone(),
        "POST",
        "/api/playbooks",
        serde_json::json!({ "id": "brand-new", "yaml": yaml }),
    )
    .await;
    assert!(
        status == StatusCode::CREATED || status == StatusCode::OK,
        "status={status}"
    );
    assert_eq!(json["id"], "brand-new");
    assert_eq!(json["version"], "1.0.0");

    let (status, json) = get_json(app, "/api/playbooks/brand-new").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json["version"], "1.0.0");
}

#[tokio::test]
async fn put_playbook_creates_new_minor_version() {
    let _cfg = crate::common::config_sandbox().await;
    let dir = seed();
    let app = build_router(AppState::new(dir.path().to_path_buf()));
    let yaml = VALID.replace("name: Implement Task", "name: Implement Task v2");
    let (status, json) = json_request(
        app.clone(),
        "PUT",
        "/api/playbooks/implement-task",
        serde_json::json!({ "yaml": yaml }),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json["id"], "implement-task");
    assert_eq!(json["version"], "1.1.0");

    let (status, json) = get_json(app, "/api/playbooks/implement-task").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json["version"], "1.1.0");
}

/// The dashboard's trash round trip over HTTP: a deleted playbook is listed
/// with its deletion time, restores with a 200, and a restore after the id was
/// taken again is a 409 whose body says why (the Trash view shows it as is).
#[tokio::test]
async fn trash_lists_restores_and_reports_a_conflict() {
    let _cfg = crate::common::config_sandbox().await;
    let dir = seed();
    let app = build_router(AppState::new(dir.path().to_path_buf()));
    let (status, _) = json_request(
        app.clone(),
        "DELETE",
        "/api/playbooks/implement-task",
        serde_json::json!({}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let (status, _) = get_json(app.clone(), "/api/playbooks/implement-task").await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    let (status, trash) = get_json(app.clone(), "/api/trash").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(trash[0]["id"], "implement-task");
    assert_eq!(trash[0]["versions"], serde_json::json!(["1.0.0"]));
    assert_eq!(trash[0]["conflict"], false);
    assert!(trash[0]["deleted_at_ms"].as_u64().unwrap() > 0);
    let name = trash[0]["name"].as_str().unwrap().to_string();

    let (status, restored) = json_request(
        app.clone(),
        "POST",
        &format!("/api/trash/{name}/restore"),
        serde_json::json!({}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(restored["id"], "implement-task");
    let (status, _) = get_json(app.clone(), "/api/playbooks/implement-task").await;
    assert_eq!(status, StatusCode::OK);
    let (_, trash) = get_json(app.clone(), "/api/trash").await;
    assert_eq!(trash, serde_json::json!([]));

    // Deleted again, and a new playbook takes the id.
    json_request(
        app.clone(),
        "DELETE",
        "/api/playbooks/implement-task",
        serde_json::json!({}),
    )
    .await;
    let (status, _) = json_request(
        app.clone(),
        "POST",
        "/api/playbooks",
        serde_json::json!({ "id": "implement-task", "yaml": VALID }),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    let (_, trash) = get_json(app.clone(), "/api/trash").await;
    assert_eq!(trash[0]["conflict"], true);
    let res = app
        .oneshot(
            Request::post("/api/trash/implement-task/restore")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::CONFLICT);
    let body = res.into_body().collect().await.unwrap().to_bytes();
    let body = String::from_utf8_lossy(&body);
    assert!(body.contains("exists again"), "{body}");
}

#[tokio::test]
async fn get_playbook_diff_between_versions() {
    let _cfg = crate::common::config_sandbox().await;
    let dir = seed();
    let app = build_router(AppState::new(dir.path().to_path_buf()));
    let yaml = VALID
        .replace(
            "Write a plan: {{params.task}}",
            "Write a detailed plan: {{params.task}}",
        )
        .replace(
            "  - { from: fix, to: lint }",
            "  - { from: fix, to: check }",
        );
    let (status, _) = json_request(
        app.clone(),
        "PUT",
        "/api/playbooks/implement-task",
        serde_json::json!({ "yaml": yaml }),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    let (status, json) = get_json(
        app,
        "/api/playbooks/implement-task/diff?from=1.0.0&to=1.1.0",
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        json["nodes_changed"]
            .as_array()
            .unwrap()
            .contains(&serde_json::json!("plan"))
    );
    assert!(!json["yaml_diff"].as_str().unwrap().is_empty());
}

#[tokio::test]
async fn put_layout_saves_canvas_layout() {
    let dir = seed();
    let app = build_router(AppState::new(dir.path().to_path_buf()));
    let (status, _) = json_request(
        app.clone(),
        "PUT",
        "/api/playbooks/implement-task/layout?version=1.0.0",
        serde_json::json!({ "layout": "nodes:\n  - { id: plan, x: 11, y: 22 }\n" }),
    )
    .await;
    assert!(
        status == StatusCode::NO_CONTENT || status == StatusCode::OK,
        "status={status}"
    );

    let (status, json) = get_json(app, "/api/playbooks/implement-task?version=1.0.0").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json["layout"]["nodes"][0]["x"], 11);
}

#[tokio::test]
async fn post_playbook_invalid_yaml_is_400() {
    let dir = seed();
    let app = build_router(AppState::new(dir.path().to_path_buf()));
    let invalid = VALID.replace(
        "  - id: plan",
        "  - id: start2\n    type: start\n    title: Second start\n  - id: plan",
    );
    let (status, json) = json_request(
        app,
        "POST",
        "/api/playbooks",
        serde_json::json!({ "id": "implement-task", "yaml": invalid }),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let body = json.to_string();
    assert!(
        body.contains("V03"),
        "expected validation codes in body: {body}"
    );
}

#[tokio::test]
async fn get_versions_returns_provenance() {
    let dir = seed();
    let patch = apb_core::versioning::create_patch_version(
        dir.path(),
        "implement-task",
        "1.0.0",
        VALID,
        "run-x",
        "improvement",
    )
    .unwrap();
    let app = build_router(AppState::new(dir.path().to_path_buf()));
    let (status, json) = get_json(app, "/api/playbooks/implement-task/versions").await;
    assert_eq!(status, StatusCode::OK);
    let arr = json.as_array().unwrap();
    let base = arr.iter().find(|v| v["version"] == "1.0.0").unwrap();
    assert_eq!(base["is_current"], true);
    let patched = arr
        .iter()
        .find(|v| v["version"] == serde_json::json!(patch))
        .unwrap();
    assert_eq!(patched["is_current"], false);
    assert_eq!(patched["provenance"]["classification"], "improvement");
    assert_eq!(patched["provenance"]["run_id"], "run-x");
}

#[tokio::test]
async fn post_promote_moves_current() {
    let dir = seed();
    let patch = apb_core::versioning::create_patch_version(
        dir.path(),
        "implement-task",
        "1.0.0",
        VALID,
        "run-x",
        "improvement",
    )
    .unwrap();
    let app = build_router(AppState::new(dir.path().to_path_buf()));
    let (status, json) = json_request(
        app.clone(),
        "POST",
        &format!("/api/playbooks/implement-task/versions/{patch}/promote"),
        serde_json::json!({}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json["promoted"], serde_json::json!(patch));

    let (status, json) = get_json(app, "/api/playbooks/implement-task/versions").await;
    assert_eq!(status, StatusCode::OK);
    let patched = json
        .as_array()
        .unwrap()
        .iter()
        .find(|v| v["version"] == serde_json::json!(patch))
        .unwrap()
        .clone();
    assert_eq!(patched["is_current"], true);
}

#[tokio::test]
async fn promote_unknown_version_404() {
    let dir = seed();
    let app = build_router(AppState::new(dir.path().to_path_buf()));
    let (status, _) = json_request(
        app,
        "POST",
        "/api/playbooks/implement-task/versions/9.9.9/promote",
        serde_json::json!({}),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn versions_endpoint_rejects_path_traversal() {
    let dir = seed();
    let app = build_router(AppState::new(dir.path().to_path_buf()));
    let (status, _) = get_json(app, "/api/playbooks/..%2F..%2Fetc/versions").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn write_endpoints_reject_path_traversal() {
    let dir = seed();
    let app = build_router(AppState::new(dir.path().to_path_buf()));
    let (status, _) = json_request(
        app.clone(),
        "POST",
        "/api/playbooks",
        serde_json::json!({ "id": "../evil", "yaml": VALID }),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    let (status, _) = json_request(
        app,
        "PUT",
        "/api/playbooks/..%2Fevil",
        serde_json::json!({ "yaml": VALID }),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

/// A save through the dashboard editor follows the one save rule
/// (`save_definition`): the saved digest is approved, exactly as a save
/// through MCP `playbook_update` is, so the edited playbook stays trusted for
/// MCP runs and the catalog. It used to drop trust.
#[tokio::test]
async fn put_playbook_approves_the_saved_digest() {
    let _cfg = crate::common::config_sandbox().await;
    let dir = seed();
    let app = build_router(AppState::new(dir.path().to_path_buf()));
    let yaml = VALID.replace("name: Implement Task", "name: Implement Task v2");
    let (status, json) = json_request(
        app,
        "PUT",
        "/api/playbooks/implement-task",
        serde_json::json!({ "yaml": yaml }),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let version = json["version"].as_str().unwrap();
    let saved = fs::read_to_string(
        dir.path()
            .join(".apb/playbooks/implement-task")
            .join(version)
            .join("playbook.yaml"),
    )
    .unwrap();
    assert!(
        apb_core::trust::TrustStore::load().is_approved(&apb_core::scope::digest_str(&saved)),
        "the dashboard save must approve the digest it wrote"
    );
}

// --- HTTP caching and build identity (issue #139 F8) ------------------------

async fn raw_get(app: axum::Router, uri: &str) -> axum::response::Response {
    app.oneshot(Request::get(uri).body(Body::empty()).unwrap())
        .await
        .unwrap()
}

fn header<'a>(res: &'a axum::response::Response, name: &str) -> Option<&'a str> {
    res.headers().get(name).and_then(|v| v.to_str().ok())
}

/// An unknown API route or asset is a real 404, never the SPA shell with a
/// 200: an old bundle calling a removed endpoint must see an error, not HTML
/// it then fails to parse as JSON, and a lazy chunk that no longer exists
/// must not be answered with HTML as JavaScript.
#[tokio::test]
async fn unknown_api_routes_and_assets_are_404() {
    let dir = seed();
    let app = build_router(AppState::new(dir.path().to_path_buf()));
    for uri in ["/api/nope", "/api/playbooks/x/nope", "/assets/nope.js"] {
        let res = raw_get(app.clone(), uri).await;
        assert_eq!(res.status(), StatusCode::NOT_FOUND, "{uri}");
    }
}

/// The shell is revalidated on every load, hashed assets are cached for
/// good, the API is never served from cache, and every response names the
/// build that served it. The shell carries the same build id, so a tab can
/// tell when the server it talks to was rebuilt under it.
#[tokio::test]
async fn cache_policy_and_build_identity() {
    let dir = seed();
    let app = build_router(AppState::new(dir.path().to_path_buf()));

    let health = raw_get(app.clone(), "/api/health").await;
    assert_eq!(header(&health, "cache-control"), Some("no-cache"));
    let served_by = header(&health, "x-apb-build")
        .expect("build header")
        .to_string();
    let bytes = health.into_body().collect().await.unwrap().to_bytes();
    let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(body["build_id"], served_by.as_str());

    let shell = raw_get(app.clone(), "/").await;
    assert_eq!(shell.status(), StatusCode::OK);
    assert_eq!(header(&shell, "cache-control"), Some("no-cache"));
    let html = String::from_utf8(
        shell
            .into_body()
            .collect()
            .await
            .unwrap()
            .to_bytes()
            .to_vec(),
    )
    .unwrap();
    assert!(
        html.contains(&format!(r#"<meta name="apb-build" content="{served_by}">"#)),
        "the shell names its build: {html}"
    );

    let asset = html
        .split('"')
        .find(|s| s.starts_with("/assets/") && s.ends_with(".js"))
        .expect("the shell references a hashed script")
        .to_string();
    let res = raw_get(app, &asset).await;
    assert_eq!(res.status(), StatusCode::OK);
    assert_eq!(
        header(&res, "cache-control"),
        Some("public, max-age=31536000, immutable")
    );
}

/// The dashboard shell (at `/` and at any client route) carries a
/// Content-Security-Policy that only runs the server's own scripts: no inline
/// script, no eval, no plugins, no foreign base URL, no framing. Style
/// elements need this load's nonce, which the shell hands to the page and
/// which differs per load; style attributes are refused except the empty one.
/// The shell itself has no inline script, so the policy is one the build meets.
#[tokio::test]
async fn the_shell_carries_a_strict_content_security_policy() {
    let dir = seed();
    let app = build_router(AppState::new(dir.path().to_path_buf()));
    let mut nonces = Vec::new();
    for uri in ["/", "/runs/some-run", "/"] {
        let res = raw_get(app.clone(), uri).await;
        assert_eq!(res.status(), StatusCode::OK, "{uri}");
        let csp = header(&res, "content-security-policy")
            .unwrap_or_else(|| panic!("{uri}: no content-security-policy"))
            .to_string();
        let directive = |name: &str| -> Vec<String> {
            csp.split(';')
                .map(str::trim)
                .find_map(|d| d.strip_prefix(name).filter(|r| r.starts_with(' ')))
                .map(|r| r.split_whitespace().map(str::to_string).collect())
                .unwrap_or_default()
        };
        assert_eq!(directive("default-src"), ["'self'"], "{uri}: {csp}");
        assert_eq!(directive("script-src"), ["'self'"], "{uri}: {csp}");
        assert_eq!(directive("object-src"), ["'none'"], "{uri}: {csp}");
        assert_eq!(directive("base-uri"), ["'none'"], "{uri}: {csp}");
        assert_eq!(directive("frame-ancestors"), ["'none'"], "{uri}: {csp}");
        assert!(!csp.contains("unsafe-eval"), "{uri}: {csp}");
        assert!(!csp.contains("'unsafe-inline'"), "{uri}: {csp}");
        // The only style attribute admitted is the empty one (its SHA-256).
        assert_eq!(
            directive("style-src-attr"),
            [
                "'unsafe-hashes'",
                "'sha256-47DEQpj8HBSa+/TImW+5JCeuQeRkm5NMpJWZG3hSuFU='"
            ],
            "{uri}: {csp}"
        );

        let html = String::from_utf8(res.into_body().collect().await.unwrap().to_bytes().to_vec())
            .unwrap();
        for tag in html.split("<script").skip(1) {
            let open = tag.split('>').next().unwrap_or_default();
            assert!(
                open.contains(" src="),
                "{uri}: inline script in the shell: {tag}"
            );
        }
        let nonce = html
            .split(r#"<meta name="csp-nonce" content=""#)
            .nth(1)
            .and_then(|r| r.split('"').next())
            .unwrap_or_else(|| panic!("{uri}: no csp-nonce meta"))
            .to_string();
        assert!(nonce.len() >= 22, "{uri}: nonce too short: {nonce}");
        assert_eq!(
            directive("style-src"),
            ["'self'".to_string(), format!("'nonce-{nonce}'")],
            "{uri}: {csp}"
        );
        nonces.push(nonce);
    }
    nonces.sort();
    nonces.dedup();
    assert_eq!(nonces.len(), 3, "a nonce is never reused: {nonces:?}");
}

/// The dashboard's Trust view over HTTP: the listing shows every approval, a
/// revoke by id removes all of that id's approvals of the kind and returns
/// them, and a target that matches nothing is a 404.
#[tokio::test]
async fn trust_lists_and_revokes() {
    use apb_core::trust::{Kind, OriginKind, TrustStore};
    let _cfg = crate::common::config_sandbox().await;
    let mut store = TrustStore::load();
    for (digest, id, kind) in [
        ("sha256:t1", "demo", Kind::Playbook),
        ("sha256:t2", "demo", Kind::Playbook),
        ("sha256:t3", "demo", Kind::ProfileBundle),
    ] {
        store
            .approve_kind(digest, id, kind, OriginKind::LocallyApproved)
            .unwrap();
    }
    let dir = seed();
    let app = build_router(AppState::new(dir.path().to_path_buf()));

    let (status, listed) = get_json(app.clone(), "/api/trust").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(listed.as_array().unwrap().len(), 3, "{listed}");

    let (status, out) = json_request(
        app.clone(),
        "POST",
        "/api/trust/revoke",
        serde_json::json!({ "target": "demo", "kind": "playbook" }),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(out["revoked"].as_array().unwrap().len(), 2, "{out}");
    let (_, listed) = get_json(app.clone(), "/api/trust").await;
    assert_eq!(listed[0]["kind"], "profile_bundle", "{listed}");
    assert_eq!(listed.as_array().unwrap().len(), 1, "{listed}");

    let (status, _) = json_request(
        app,
        "POST",
        "/api/trust/revoke",
        serde_json::json!({ "target": "sha256:t1" }),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}
