//! The passkey ceremonies end to end over the real HTTP routes, driven by a
//! software authenticator: the enrollment with the one-time code
//! `apb passkey enroll` printed, then a person-only gate and a bound
//! approval decided through the dashboard's review route, the run going on
//! past them. The authenticator speaks exactly the wire format a browser
//! speaks: `clientDataJSON` posted base64url-encoded, signatures over
//! `authenticatorData || SHA-256(clientDataJSON bytes)`, `rpIdHash` over the
//! RP id the dashboard served, UP and UV set. The negative ends live here
//! too: a replayed assertion, a wrong origin, the default local dashboard
//! origin accepted, an IP-literal RP ID refused, and a passkey-less
//! dashboard refusal for removal.

use apb_server::{AppState, build_router};
use axum::body::Body;
use axum::http::{Request, StatusCode};
use base64::Engine as _;
use ciborium::Value;
use http_body_util::BodyExt;
use p256::ecdsa::signature::Signer as _;
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::Duration;
use tower::ServiceExt;

use crate::common;

const DEADLINE: Duration = Duration::from_secs(60);

// --- the software authenticator ------------------------------------------------

/// The canonical COSE EC2 key (ES256) for a P-256 verifying key.
fn cose_es256(vk: &p256::ecdsa::VerifyingKey) -> Vec<u8> {
    let point = vk.to_encoded_point(false);
    let x = point.x().unwrap().as_slice().to_vec();
    let y = point.y().unwrap().as_slice().to_vec();
    let map = Value::Map(vec![
        (Value::from(1i64), Value::from(2i64)),
        (Value::from(3i64), Value::from(-7i64)),
        (Value::from(-1i64), Value::from(1i64)),
        (Value::from(-2i64), Value::Bytes(x)),
        (Value::from(-3i64), Value::Bytes(y)),
    ]);
    let mut out = Vec::new();
    ciborium::ser::into_writer(&map, &mut out).unwrap();
    out
}

/// The canonical COSE OKP key (EdDSA) for an Ed25519 public key.
fn cose_eddsa(public: &[u8; 32]) -> Vec<u8> {
    let map = Value::Map(vec![
        (Value::from(1i64), Value::from(1i64)),
        (Value::from(3i64), Value::from(-8i64)),
        (Value::from(-1i64), Value::from(6i64)),
        (Value::from(-2i64), Value::Bytes(public.to_vec())),
    ]);
    let mut out = Vec::new();
    ciborium::ser::into_writer(&map, &mut out).unwrap();
    out
}

/// `authenticatorData`: rpIdHash, flags, sign count and, for a
/// registration, the attested credential data (16 zero AAGUID bytes, the
/// credential id, the COSE key).
fn auth_data(rp_id: &str, flags: u8, count: u32, attested: Option<(&[u8], &[u8])>) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(&Sha256::digest(rp_id.as_bytes()));
    out.push(flags);
    out.extend_from_slice(&count.to_be_bytes());
    if let Some((id, cose)) = attested {
        out.extend_from_slice(&[0u8; 16]);
        out.extend_from_slice(&(id.len() as u16).to_be_bytes());
        out.extend_from_slice(id);
        out.extend_from_slice(cose);
    }
    out
}

const FLAG_UP: u8 = 0x01;
const FLAG_UV: u8 = 0x04;
const FLAG_AT: u8 = 0x40;

/// A software passkey of one algorithm, speaking the browser wire format.
enum Authenticator {
    Es256 {
        signing: p256::ecdsa::SigningKey,
        cred_id: Vec<u8>,
        counter: std::cell::Cell<u32>,
    },
    EdDSA {
        signing: ed25519_dalek::SigningKey,
        cred_id: Vec<u8>,
        counter: std::cell::Cell<u32>,
    },
}

impl Authenticator {
    fn es256(seed: u8) -> Self {
        let mut secret = [0u8; 32];
        secret[31] = seed;
        let signing = p256::ecdsa::SigningKey::from_bytes(&secret.into()).unwrap();
        let cred_id = Sha256::digest(secret).to_vec();
        Authenticator::Es256 {
            signing,
            cred_id,
            counter: std::cell::Cell::new(1),
        }
    }

    fn eddsa(seed: u8) -> Self {
        let mut secret = [0u8; 32];
        secret[31] = seed;
        let signing = ed25519_dalek::SigningKey::from_bytes(&secret);
        let cred_id = Sha256::digest(secret).to_vec();
        Authenticator::EdDSA {
            signing,
            cred_id,
            counter: std::cell::Cell::new(1),
        }
    }

    fn id_b64u(&self) -> String {
        let id = match self {
            Authenticator::Es256 { cred_id, .. } => cred_id,
            Authenticator::EdDSA { cred_id, .. } => cred_id,
        };
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(id)
    }

    /// The registration response: clientDataJSON base64url, attestation
    /// object base64url, credential id base64url.
    fn registration(&self, rp_id: &str, origin: &str, challenge: &str) -> serde_json::Value {
        let client = format!(
            r#"{{"type":"webauthn.create","challenge":"{challenge}","origin":"{origin}","crossOrigin":false}}"#
        );
        let cose = match self {
            Authenticator::Es256 { signing, .. } => cose_es256(signing.verifying_key()),
            Authenticator::EdDSA { signing, .. } => cose_eddsa(&signing.verifying_key().to_bytes()),
        };
        let id = match self {
            Authenticator::Es256 { cred_id, .. } => cred_id.as_slice(),
            Authenticator::EdDSA { cred_id, .. } => cred_id.as_slice(),
        };
        let data = auth_data(rp_id, FLAG_UP | FLAG_UV | FLAG_AT, 1, Some((id, &cose)));
        let attestation = Value::Map(vec![
            (Value::Text("fmt".into()), Value::Text("none".into())),
            (Value::Text("attStmt".into()), Value::Map(vec![])),
            (Value::Text("authData".into()), Value::Bytes(data)),
        ]);
        let mut att_bytes = Vec::new();
        ciborium::ser::into_writer(&attestation, &mut att_bytes).unwrap();
        serde_json::json!({
            "id": self.id_b64u(),
            "clientDataJSON": b64u(client.as_bytes()),
            "attestationObject": b64u(&att_bytes),
        })
    }

    /// The assertion response over `authenticatorData || SHA-256(clientDataJSON bytes)`.
    fn assertion(&self, rp_id: &str, origin: &str, challenge: &str) -> serde_json::Value {
        let sign_count = match self {
            Authenticator::Es256 { counter, .. } | Authenticator::EdDSA { counter, .. } => {
                counter.set(counter.get() + 1);
                counter.get()
            }
        };
        let client = format!(
            r#"{{"type":"webauthn.get","challenge":"{challenge}","origin":"{origin}","crossOrigin":false}}"#
        );
        let data = auth_data(rp_id, FLAG_UP | FLAG_UV, sign_count, None);
        let mut signed = data.clone();
        signed.extend_from_slice(&Sha256::digest(client.as_bytes()));
        let signature = match self {
            Authenticator::Es256 { signing, .. } => {
                let sig: p256::ecdsa::Signature = signing.sign(&signed);
                sig.to_der().as_bytes().to_vec()
            }
            Authenticator::EdDSA { signing, .. } => signing.sign(&signed).to_vec(),
        };
        serde_json::json!({
            "id": self.id_b64u(),
            "clientDataJSON": b64u(client.as_bytes()),
            "authenticatorData": b64u(&data),
            "signature": b64u(&signature),
        })
    }
}

fn b64u(bytes: &[u8]) -> String {
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
}

// --- the harness ----------------------------------------------------------------

/// A workspace with a person-only gate playbook and a bound approval
/// playbook installed, and a fresh config dir as `APB_CONFIG_DIR`.
struct Bench {
    _config: common::ConfigSandbox,
    root: PathBuf,
    config_dir: PathBuf,
    /// Run dirs this bench already started, so the next start finds the
    /// new run and not an earlier one.
    started: std::cell::RefCell<Vec<PathBuf>>,
}

const PERSON_ONLY: &str = r#"schema: 2
id: po
name: po
version: 1.0.0
nodes:
  - { id: start, type: start }
  - { id: g, type: human_review, options: [approve, reject], person_only: true }
  - { id: done, type: finish, outcome: success }
edges:
  - { from: start, to: g }
  - { from: g, to: done }
"#;

const APPROVAL: &str = r#"schema: 2
id: ap
name: ap
version: 1.0.0
nodes:
  - { id: start, type: start }
  - { id: w, type: script, script: scripts/w.sh, runner: sh, results: [ { path: report.md } ] }
  - { id: g, type: human_review, options: [approve, reject], approves: { results: [w] } }
  - { id: done, type: finish, outcome: success }
edges:
  - { from: start, to: w }
  - { from: w, to: g }
  - { from: g, to: done }
"#;

/// A publishing playbook, run in a git tree whose `origin` is a local bare
/// remote over `file://`.
const PUBLISH: &str = r#"schema: 2
id: pub
name: pub
version: 1.0.0
nodes:
  - { id: start, type: start }
  - { id: work, type: script, script: scripts/w.sh, runner: sh }
  - { id: push, type: publish, from: work, remote: origin, branch: "apb/out" }
  - { id: done, type: finish, outcome: success }
edges:
  - { from: start, to: work }
  - { from: work, to: push }
  - { from: push, to: done }
"#;

/// The work node commits with the run's trailer, which is what the publish
/// plan picks up.
const PUBLISH_WORK: &str = r#"printf '%s' "$APB_RUN_ID" > work.txt
git add work.txt
git -c user.name=t -c user.email=t@example.invalid -c commit.gpgsign=false \
  commit -q -m "work of the run" -m "Apb-Run: $APB_RUN_ID"
"#;

impl Bench {
    async fn new() -> Bench {
        let config = common::config_sandbox().await;
        let config_dir = config.path().to_path_buf();
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().to_path_buf();
        let vdir = root.join(".apb/playbooks/ap/1.0.0");
        std::fs::create_dir_all(vdir.join("scripts")).unwrap();
        std::fs::write(
            vdir.join("scripts/w.sh"),
            "printf 'draft one\\n' > report.md\necho ok\n",
        )
        .unwrap();
        for (id, yaml) in [("po", PERSON_ONLY), ("ap", APPROVAL)] {
            let vdir = root.join(format!(".apb/playbooks/{id}/1.0.0"));
            std::fs::create_dir_all(&vdir).unwrap();
            std::fs::write(vdir.join("playbook.yaml"), yaml).unwrap();
            std::fs::write(root.join(format!(".apb/playbooks/{id}/current")), "1.0.0").unwrap();
        }
        std::mem::forget(dir); // lives as long as the bench
        Bench {
            _config: config,
            root,
            config_dir,
            started: std::cell::RefCell::new(Vec::new()),
        }
    }

    fn app(&self) -> axum::Router {
        build_router(AppState::new(self.root.clone()))
    }

    /// A bench whose workspace is a git tree with a bare `origin` over
    /// `file://`, seeded with the publishing playbook: everything a publish
    /// node needs to plan and push, with nothing on the network.
    async fn publish() -> Bench {
        let base_dir = tempfile::tempdir().unwrap();
        let base = base_dir.path().canonicalize().unwrap();
        let root = base.join("tree");
        let bare = base.join("remote.git");
        let home = base.join("home");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::create_dir_all(&home).unwrap();
        std::fs::write(home.join(".gitconfig"), "").unwrap();
        let git = |dir: &Path, args: &[&str]| {
            let out = std::process::Command::new("git")
                .arg("-C")
                .arg(dir)
                .args([
                    "-c",
                    "user.name=t",
                    "-c",
                    "user.email=t@example.invalid",
                    "-c",
                    "commit.gpgsign=false",
                ])
                .args(args)
                .env("HOME", &home)
                .env("GIT_CONFIG_GLOBAL", home.join(".gitconfig"))
                .env("GIT_CONFIG_NOSYSTEM", "1")
                .output()
                .expect("git runs");
            assert!(
                out.status.success(),
                "git {args:?} failed: {}",
                String::from_utf8_lossy(&out.stderr)
            );
        };
        git(&root, &["init", "-q", "-b", "main"]);
        std::fs::write(root.join(".gitignore"), ".apb/\n").unwrap();
        git(&root, &["add", ".gitignore"]);
        git(&root, &["commit", "-q", "-m", "base"]);
        git(&base, &["init", "-q", "--bare", "remote.git"]);
        let url = format!("file://{}", bare.display());
        git(&root, &["remote", "add", "origin", &url]);
        git(&root, &["push", "-q", "origin", "main"]);
        let config = common::config_sandbox().await;
        let config_dir = config.path().to_path_buf();
        apb_core::registry::init_project(&root).unwrap();
        let vdir = root.join(".apb/playbooks/pub/1.0.0");
        std::fs::create_dir_all(vdir.join("scripts")).unwrap();
        std::fs::write(vdir.join("playbook.yaml"), PUBLISH).unwrap();
        std::fs::write(vdir.join("scripts/w.sh"), PUBLISH_WORK).unwrap();
        std::fs::write(root.join(".apb/playbooks/pub/current"), "1.0.0").unwrap();
        std::mem::forget(base_dir); // the whole bench lives as long as the test
        std::mem::forget(base);
        Bench {
            _config: config,
            root,
            config_dir,
            started: std::cell::RefCell::new(Vec::new()),
        }
    }

    /// A dashboard with server keys on, and a signed-in browser session:
    /// a publish consent is a signed-in browser's, never a keyless call's.
    async fn keyed_app_with_session(&self) -> (axum::Router, String) {
        use apb_core::server_auth;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("server-auth.yaml");
        let (key, _) = server_auth::issue_into(&path).unwrap();
        let file = server_auth::load_from(&path).unwrap();
        let auth = std::sync::Arc::new(
            apb_server::auth::AuthState::new(
                None,
                file.keys,
                &apb_core::config::ServerConfig::default(),
            )
            .unwrap(),
        );
        std::mem::forget(dir); // the key file lives as long as the test
        let app = build_router(AppState::new(self.root.clone()).with_auth(auth));
        let login = Request::builder()
            .method("POST")
            .uri("/api/auth/login")
            .header("content-type", "application/json")
            .header(apb_server::auth::CSRF_HEADER, apb_server::auth::CSRF_VALUE)
            .body(Body::from(
                serde_json::json!({ "key": key.expose_secret() }).to_string(),
            ))
            .unwrap();
        let res = app.clone().oneshot(login).await.unwrap();
        let cookie = res
            .headers()
            .get(axum::http::header::SET_COOKIE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or_default()
            .split(';')
            .next()
            .unwrap_or_default()
            .to_string();
        assert!(!cookie.is_empty(), "the login minted a session cookie");
        (app, cookie)
    }

    /// Starts a playbook with the person's start consent, as a publish
    /// node's `irreversible` effect demands.
    fn start_with_consent(&self, playbook: &str) -> (String, PathBuf, mpsc::Receiver<bool>) {
        let (tx, rx) = mpsc::channel();
        let drive_root = self.root.clone();
        let root = self.root.clone();
        let id = playbook.to_string();
        std::thread::spawn(move || {
            let opts = apb_engine::scheduler::RunOptions {
                consent: Some(apb_engine::consent::RunConsent::irreversible("cli")),
                ..apb_engine::scheduler::RunOptions::default()
            };
            let _ = apb_engine::scheduler::run(&drive_root, &id, None, opts).map(|r| {
                tx.send(r.outcome == apb_engine::state::RunStatus::Succeeded)
                    .ok()
            });
        });
        let seen = self.started.borrow().clone();
        let rd = poll("the run dir", || {
            std::fs::read_dir(root.join(".apb/runs"))
                .ok()?
                .filter_map(Result::ok)
                .map(|e| e.path())
                .find(|p| p.is_dir() && !seen.contains(p))
        });
        self.started.borrow_mut().push(rd.clone());
        let run_id = rd.file_name().unwrap().to_str().unwrap().to_string();
        (run_id, rd, rx)
    }

    /// Starts the playbook through the engine on a drive thread and returns
    /// (run id, run dir, receiver of the finished run).
    fn start(&self, playbook: &str) -> (String, PathBuf, mpsc::Receiver<bool>) {
        let (tx, rx) = mpsc::channel();
        let id = playbook.to_string();
        let drive_root = self.root.clone();
        let root = self.root.clone();
        std::thread::spawn(move || {
            let opts = apb_engine::scheduler::RunOptions::default();
            let _ = apb_engine::scheduler::run(&drive_root, &id, None, opts).map(|r| {
                tx.send(r.outcome == apb_engine::state::RunStatus::Succeeded)
                    .ok()
            });
        });
        let seen = self.started.borrow().clone();
        let rd = poll("the run dir", || {
            std::fs::read_dir(root.join(".apb/runs"))
                .ok()?
                .filter_map(Result::ok)
                .map(|e| e.path())
                .find(|p| p.is_dir() && !seen.contains(p))
        });
        self.started.borrow_mut().push(rd.clone());
        let run_id = rd.file_name().unwrap().to_str().unwrap().to_string();
        (run_id, rd, rx)
    }
}

fn poll_debug<T>(rd: &Path, what: &str, mut f: impl FnMut() -> Option<T>) -> T {
    let t = std::time::Instant::now();
    loop {
        if let Some(v) = f() {
            return v;
        }
        if t.elapsed() >= DEADLINE {
            let events = apb_engine::event::read_all(rd).unwrap_or_default();
            for e in events {
                println!("EVENT {} {:?}", e.seq, e.payload);
            }
            panic!("timed out waiting for {what}");
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn poll<T>(what: &str, mut f: impl FnMut() -> Option<T>) -> T {
    let t = std::time::Instant::now();
    loop {
        if let Some(v) = f() {
            return v;
        }
        assert!(t.elapsed() < DEADLINE, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(20));
    }
}

async fn request_json(
    app: &axum::Router,
    method: &str,
    host: &str,
    uri: &str,
    body: serde_json::Value,
) -> (StatusCode, serde_json::Value) {
    request_json_as(app, method, host, uri, body, None).await
}

/// [`request_json`] with an optional dashboard session cookie and the CSRF
/// marker header a cookie-authenticated write must carry.
async fn request_json_as(
    app: &axum::Router,
    method: &str,
    host: &str,
    uri: &str,
    body: serde_json::Value,
    cookie: Option<&str>,
) -> (StatusCode, serde_json::Value) {
    let mut builder = Request::builder()
        .method(method)
        .uri(uri)
        .header("host", host)
        .header("content-type", "application/json");
    if let Some(cookie) = cookie {
        builder = builder
            .header("cookie", cookie)
            .header(apb_server::auth::CSRF_HEADER, apb_server::auth::CSRF_VALUE);
    }
    let req = builder.body(Body::from(body.to_string())).unwrap();
    let res = app.clone().oneshot(req).await.unwrap();
    let status = res.status();
    let bytes = res.into_body().collect().await.unwrap().to_bytes();
    let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null);
    (status, json)
}

async fn request_raw(
    app: &axum::Router,
    method: &str,
    host: &str,
    uri: &str,
    body: serde_json::Value,
) -> (StatusCode, String) {
    let req = Request::builder()
        .method(method)
        .uri(uri)
        .header("host", host)
        .header("content-type", "application/json")
        .body(Body::from(body.to_string()))
        .unwrap();
    let res = app.clone().oneshot(req).await.unwrap();
    let status = res.status();
    let text = String::from_utf8(res.into_body().collect().await.unwrap().to_bytes().to_vec())
        .unwrap_or_default();
    (status, text)
}

/// The host the dashboard is "served" under in these tests.
const HOST: &str = "localhost:7777";
const ORIGIN: &str = "http://localhost:7777";
const RP_ID: &str = "localhost";

fn decision_purpose(run: &str, node: &str, decision: &str, request: u64) -> serde_json::Value {
    serde_json::json!({
        "decision": { "run": run, "node": node, "decision": decision, "request": request, "nonce": null }
    })
}

/// The full assertion ceremony against the real routes.
async fn assert_token(
    app: &axum::Router,
    auth: &Authenticator,
    purpose: serde_json::Value,
) -> Result<((String, String), serde_json::Value), String> {
    let (status, start) = request_json(
        app,
        "POST",
        HOST,
        "/api/passkeys/assert/challenge",
        serde_json::json!({ "purpose": purpose }),
    )
    .await;
    if status != StatusCode::OK {
        return Err(format!("assert/challenge: {status} {start}"));
    }
    let challenge = start["challenge"].as_str().unwrap().to_string();
    let assertion = auth.assertion(RP_ID, ORIGIN, &challenge);
    let body = serde_json::json!({
        "purpose": purpose,
        "challenge": challenge,
        "id": assertion["id"],
        "clientDataJSON": assertion["clientDataJSON"],
        "authenticatorData": assertion["authenticatorData"],
        "signature": assertion["signature"],
    });
    let (status, verify) =
        request_json(app, "POST", HOST, "/api/passkeys/assert/verify", body).await;
    if status != StatusCode::OK {
        return Err(format!("assert/verify: {status} {verify:?}"));
    }
    Ok((
        (
            verify["token"].as_str().unwrap().to_string(),
            verify["credential_prefix"].as_str().unwrap().to_string(),
        ),
        assertion,
    ))
}

/// [`assert_token`] with an optional dashboard session cookie.
async fn assert_token_as(
    app: &axum::Router,
    auth: &Authenticator,
    purpose: serde_json::Value,
    cookie: Option<&str>,
) -> Result<((String, String), serde_json::Value), String> {
    let (status, start) = request_json_as(
        app,
        "POST",
        HOST,
        "/api/passkeys/assert/challenge",
        serde_json::json!({ "purpose": purpose }),
        cookie,
    )
    .await;
    if status != StatusCode::OK {
        return Err(format!("assert/challenge: {status} {start}"));
    }
    let challenge = start["challenge"].as_str().unwrap().to_string();
    let assertion = auth.assertion(RP_ID, ORIGIN, &challenge);
    let body = serde_json::json!({
        "purpose": purpose,
        "challenge": challenge,
        "id": assertion["id"],
        "clientDataJSON": assertion["clientDataJSON"],
        "authenticatorData": assertion["authenticatorData"],
        "signature": assertion["signature"],
    });
    let (status, verify) = request_json_as(
        app,
        "POST",
        HOST,
        "/api/passkeys/assert/verify",
        body,
        cookie,
    )
    .await;
    if status != StatusCode::OK {
        return Err(format!("assert/verify: {status} {verify:?}"));
    }
    Ok((
        (
            verify["token"].as_str().unwrap().to_string(),
            verify["credential_prefix"].as_str().unwrap().to_string(),
        ),
        assertion,
    ))
}

/// The first passkey on the machine, enrolled with the one-time code a
/// person got from the terminal, exactly as the dashboard's page posts it.
async fn enroll_first(bench: &Bench, app: &axum::Router, auth: &Authenticator) {
    let code = apb_core::passkey::write_enroll_code(&bench.config_dir).unwrap();
    let (status, start) = request_json(
        app,
        "POST",
        HOST,
        "/api/passkeys/enroll/start",
        serde_json::json!({ "code": code }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{start}");
    assert_eq!(start["rpId"], RP_ID, "{start}");
    assert_eq!(start["algorithms"], serde_json::json!([-7, -8]), "{start}");
    let challenge = start["challenge"].as_str().unwrap();
    let reg = auth.registration(RP_ID, ORIGIN, challenge);
    let body = serde_json::json!({
        "challenge": challenge,
        "id": reg["id"],
        "clientDataJSON": reg["clientDataJSON"],
        "attestationObject": reg["attestationObject"],
        "label": "laptop",
    });
    let (status, finish) =
        request_json(app, "POST", HOST, "/api/passkeys/enroll/finish", body).await;
    assert_eq!(status, StatusCode::OK, "{finish}");
    let listed = apb_core::passkey::try_load().unwrap();
    assert_eq!(listed.passkeys.len(), 1);
}

/// A publish consent decided from the dashboard with a fresh assertion is
/// consumed: the entry is stamped with the open request seq the assertion
/// signed, the drive re-derives the same challenge and the run pushes.
/// Without the request stamp on the entry the challenge the drive derives
/// can never match the one the assertion signed, and the consent queues
/// forever (the regression G2).
#[tokio::test]
async fn a_publish_consent_from_the_dashboard_pushes_with_a_passkey_enrolled() {
    let bench = Bench::publish().await;
    let (app, cookie) = bench.keyed_app_with_session().await;
    let auth = Authenticator::es256(11);

    // Enroll through the same signed-in dashboard.
    let code = apb_core::passkey::write_enroll_code(&bench.config_dir).unwrap();
    let (status, start) = request_json_as(
        &app,
        "POST",
        HOST,
        "/api/passkeys/enroll/start",
        serde_json::json!({ "code": code }),
        Some(&cookie),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{start}");
    let challenge = start["challenge"].as_str().unwrap().to_string();
    let reg = auth.registration(RP_ID, ORIGIN, &challenge);
    let (status, finish) = request_json_as(
        &app,
        "POST",
        HOST,
        "/api/passkeys/enroll/finish",
        serde_json::json!({
            "challenge": challenge,
            "id": reg["id"],
            "clientDataJSON": reg["clientDataJSON"],
            "attestationObject": reg["attestationObject"],
            "label": "laptop",
        }),
        Some(&cookie),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{finish}");
    assert_eq!(apb_core::passkey::count(), 1);

    let (run_id, rd, rx) = bench.start_with_consent("pub");
    let (_request, nonce) = poll_debug(&rd, "the publish plan", || {
        apb_engine::event::read_all(&rd)
            .ok()?
            .iter()
            .rev()
            .find_map(|e| match &e.payload {
                apb_engine::event::EventPayload::PublishPlanned { nonce, .. } => {
                    let request =
                        apb_engine::event::read_all(&rd)
                            .ok()?
                            .iter()
                            .rev()
                            .find_map(|ev| match &ev.payload {
                                apb_engine::event::EventPayload::ReviewRequested {
                                    node, ..
                                } if node == "push" => Some(ev.seq),
                                _ => None,
                            })?;
                    Some((request, nonce.clone()))
                }
                _ => None,
            })
    });

    let purpose = serde_json::json!({
        "decision": { "run": run_id, "node": "push", "decision": "publish", "request": null, "nonce": nonce }
    });
    let ((token, _prefix), assertion) = assert_token_as(&app, &auth, purpose, Some(&cookie))
        .await
        .unwrap_or_else(|e| panic!("{e}"));
    let (status, posted) = request_json_as(
        &app,
        "POST",
        HOST,
        &format!("/api/runs/{run_id}/review"),
        serde_json::json!({
            "node": "push", "decision": "publish", "nonce": nonce,
            "passkey_token": token,
            "assertionId": assertion["id"],
            "assertionClientDataJSON": assertion["clientDataJSON"],
            "assertionAuthenticatorData": assertion["authenticatorData"],
            "assertionSignature": assertion["signature"],
        }),
        Some(&cookie),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{posted}");

    // The entry the route queued carries the request the assertion signed,
    // which is what the drive re-derives the challenge from.
    let channel = poll("the stamped entry", || {
        let text = std::fs::read_to_string(rd.join("reviews.jsonl")).ok()?;
        text.lines()
            .find(|l| l.contains("passkey_proof"))
            .map(str::to_string)
    });
    assert!(channel.contains("\"request\""), "{channel}");

    let ok = rx.recv_timeout(DEADLINE).unwrap();
    assert!(ok, "the publish consent pushed and the run finished");
}

#[tokio::test]
async fn a_person_enrolls_with_the_terminal_code_and_decides_from_the_dashboard() {
    let bench = Bench::new().await;
    let app = bench.app();
    let laptop = Authenticator::es256(7);
    enroll_first(&bench, &app, &laptop).await;

    // A person-only gate decided through the dashboard route with a fresh
    // assertion: the run journals the decision and goes on.
    let (run_id, rd, rx) = bench.start("po");
    let request = poll("the request", || {
        apb_engine::event::read_all(&rd)
            .ok()?
            .iter()
            .rev()
            .find_map(|e| match &e.payload {
                apb_engine::event::EventPayload::ReviewRequested { node, .. } if node == "g" => {
                    Some(e.seq)
                }
                _ => None,
            })
    });
    let purpose = decision_purpose(&run_id, "g", "approve", request);
    let ((token, _prefix), assertion) = assert_token(&app, &laptop, purpose)
        .await
        .unwrap_or_else(|e| panic!("{e}"));
    let req = Request::builder()
        .method("POST")
        .uri(format!("/api/runs/{run_id}/review"))
        .header("host", HOST)
        .header("content-type", "application/json")
        .body(Body::from(
            serde_json::json!({
                "node": "g", "decision": "approve", "request": request,
                "passkey_token": token,
                "assertionId": assertion["id"],
                "assertionClientDataJSON": assertion["clientDataJSON"],
                "assertionAuthenticatorData": assertion["authenticatorData"],
                "assertionSignature": assertion["signature"],
            })
            .to_string(),
        ))
        .unwrap();
    let res = app.clone().oneshot(req).await.unwrap();
    let status = res.status();
    let text = String::from_utf8(res.into_body().collect().await.unwrap().to_bytes().to_vec())
        .unwrap_or_default();
    assert_eq!(status, StatusCode::OK, "{text}");
    println!(
        "CHANNEL: {}",
        std::fs::read_to_string(rd.join("reviews.jsonl")).unwrap_or_default()
    );
    let ok = rx.recv_timeout(DEADLINE).unwrap();
    assert!(ok, "the person-only gate decided the run");

    // A bound approval decided the same way, with an Ed25519 passkey added
    // through the dashboard with an assertion from the one that exists.
    let ((assertion_token, _), _) =
        assert_token(&app, &laptop, serde_json::json!({ "enroll_new": {} }))
            .await
            .unwrap_or_else(|e| panic!("{e}"));
    let (status, start_text) = request_raw(
        &app,
        "POST",
        HOST,
        "/api/passkeys/enroll/start",
        serde_json::json!({ "assertion_token": assertion_token }),
    )
    .await;
    if status != StatusCode::OK {
        panic!("enroll/start with token: {status} {start_text}");
    }
    let start: serde_json::Value = serde_json::from_str(&start_text).unwrap();
    let challenge = start["challenge"].as_str().unwrap().to_string();
    let ed = Authenticator::eddsa(9);
    let reg = ed.registration(RP_ID, ORIGIN, &challenge);
    let (status, finish) = request_json(
        &app,
        "POST",
        HOST,
        "/api/passkeys/enroll/finish",
        serde_json::json!({
            "challenge": challenge,
            "id": reg["id"],
            "clientDataJSON": reg["clientDataJSON"],
            "attestationObject": reg["attestationObject"],
            "label": "tablet",
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{status} {finish:?}");

    let (run_id, rd, rx) = bench.start("ap");
    // The open binding's nonce, exactly what the run page shows the person.
    let nonce = poll_debug(&rd, "the bound request", || {
        apb_engine::event::read_all(&rd)
            .ok()?
            .iter()
            .rev()
            .find_map(|e| match &e.payload {
                apb_engine::event::EventPayload::ReviewRequested { binding, node, .. }
                    if node == "g" && binding.is_some() =>
                {
                    binding.as_ref().map(|b| b.nonce.clone())
                }
                _ => None,
            })
    });

    let purpose = serde_json::json!({
        "decision": { "run": run_id, "node": "g", "decision": "approve", "request": null, "nonce": nonce }
    });
    let ((token, _prefix), assertion) = assert_token(&app, &ed, purpose)
        .await
        .unwrap_or_else(|e| panic!("{e}"));
    let (status, posted) = request_json(
        &app,
        "POST",
        HOST,
        &format!("/api/runs/{run_id}/review"),
        serde_json::json!({
            "node": "g", "decision": "approve", "request": null, "nonce": nonce,
            "passkey_token": token,
            "assertionId": assertion["id"],
            "assertionClientDataJSON": assertion["clientDataJSON"],
            "assertionAuthenticatorData": assertion["authenticatorData"],
            "assertionSignature": assertion["signature"],
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{posted}");
    let ok = rx.recv_timeout(DEADLINE).unwrap();
    assert!(ok, "the approval decided the run");
}

#[tokio::test]
async fn a_replayed_assertion_and_a_wrong_origin_are_refused() {
    let bench = Bench::new().await;
    let app = bench.app();
    let auth = Authenticator::es256(7);
    enroll_first(&bench, &app, &auth).await;

    let purpose = decision_purpose("r1", "g", "approve", 3);
    let (status, start) = request_json(
        &app,
        "POST",
        HOST,
        "/api/passkeys/assert/challenge",
        serde_json::json!({ "purpose": purpose }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{start}");
    let challenge = start["challenge"].as_str().unwrap().to_string();
    let assertion = auth.assertion(RP_ID, ORIGIN, &challenge);
    let body = serde_json::json!({
        "purpose": purpose,
        "challenge": challenge,
        "id": assertion["id"],
        "clientDataJSON": assertion["clientDataJSON"],
        "authenticatorData": assertion["authenticatorData"],
        "signature": assertion["signature"],
    });
    let (status, first) = request_json(
        &app,
        "POST",
        HOST,
        "/api/passkeys/assert/verify",
        body.clone(),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{first}");
    // The same assertion replayed with the same challenge is refused: the
    // verify consumed the challenge. Decision challenges are deterministic
    // and are re-issued on a fresh challenge request, so a stored assertion
    // can mint a second token; that stays harmless because the token is
    // bound to the same run, gate, decision and request, which is already
    // decided, so the second token decides nothing new.
    let (status, replay) =
        request_json(&app, "POST", HOST, "/api/passkeys/assert/verify", body).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{replay}");

    // A fresh challenge asserted with the wrong origin in clientDataJSON.
    let (status, start) = request_json(
        &app,
        "POST",
        HOST,
        "/api/passkeys/assert/challenge",
        serde_json::json!({ "purpose": decision_purpose("r1", "g", "approve", 3) }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{start}");
    let challenge = start["challenge"].as_str().unwrap().to_string();
    let wrong = auth.assertion(RP_ID, "https://evil.example", &challenge);
    let (status, refused) = request_json(
        &app,
        "POST",
        HOST,
        "/api/passkeys/assert/verify",
        serde_json::json!({
            "purpose": decision_purpose("r1", "g", "approve", 3),
            "challenge": challenge,
            "id": wrong["id"],
            "clientDataJSON": wrong["clientDataJSON"],
            "authenticatorData": wrong["authenticatorData"],
            "signature": wrong["signature"],
        }),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{refused}");
}

#[tokio::test]
async fn the_local_dashboard_origin_is_accepted_and_an_ip_literal_rp_id_is_refused() {
    let bench = Bench::new().await;
    let app = bench.app();
    let auth = Authenticator::es256(7);
    enroll_first(&bench, &app, &auth).await;

    // A dashboard served with no port in the Host header still works.
    let purpose = decision_purpose("r1", "g", "approve", 3);
    let (status, start) = request_json(
        &app,
        "POST",
        "localhost",
        "/api/passkeys/assert/challenge",
        serde_json::json!({ "purpose": purpose }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{start}");
    let challenge = start["challenge"].as_str().unwrap().to_string();
    let assertion = auth.assertion("localhost", "http://localhost", &challenge);
    let (status, verify) = request_json(
        &app.clone(),
        "POST",
        "localhost",
        "/api/passkeys/assert/verify",
        serde_json::json!({
            "purpose": purpose,
            "challenge": challenge,
            "id": assertion["id"],
            "clientDataJSON": assertion["clientDataJSON"],
            "authenticatorData": assertion["authenticatorData"],
            "signature": assertion["signature"],
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{verify}");

    // The dashboard reached on an IP address refuses to hand out a
    // ceremony: passkeys cannot bind to an IP-literal RP id.
    let (status, refused) = request_raw(
        &app,
        "POST",
        "127.0.0.1:7777",
        "/api/passkeys/enroll/start",
        serde_json::json!({ "code": "AAAAAAAA" }),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "got {refused}");
    assert!(refused.contains("IP address"), "{refused}");
}

#[tokio::test]
async fn removal_of_the_last_passkey_stays_terminal_only_and_needs_an_assertion_otherwise() {
    let bench = Bench::new().await;
    let app = bench.app();
    enroll_first(&bench, &app, &Authenticator::es256(7)).await;
    // One passkey on the machine: the dashboard refuses a removal even with
    // a token, the same rule the CLI enforces with a typed code.
    let ((_, token), _) = assert_token(
        &app,
        &Authenticator::es256(7),
        serde_json::json!({ "remove": "laptop" }),
    )
    .await
    .unwrap_or_else(|e| panic!("{e}"));
    let res = app
        .oneshot(
            Request::builder()
                .method("DELETE")
                .uri("/api/passkeys/laptop")
                .header("host", HOST)
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::json!({ "token": token }).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::FORBIDDEN, "{}", res.status());
    assert_eq!(apb_core::passkey::count(), 1, "the last passkey stays");
}
