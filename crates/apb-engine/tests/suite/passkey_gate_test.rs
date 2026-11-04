//! The passkey gate: while this machine holds at least one passkey, a
//! person-only gate and a bound approval take a decision only with a fresh
//! WebAuthn assertion, which only the dashboard route posts, and the drive
//! consumes nothing else for such a gate. The assertion a dashboard
//! decision carries is a self-verifying proof the drive re-checks here, so
//! a forged stamp in the channel file is refused. The engine's own
//! decisions on a bound gate (an unattended run's declared outcome) are
//! still consumed. With no passkey enrolled everything stays as it was.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use apb_core::registry::init_project;
use apb_engine::decision_channel::{CallerRefusal, DecisionChannel};
use apb_engine::error::EngineError;
use apb_engine::event::{Event, EventPayload, read_all};
use base64::Engine as _;
use p256::ecdsa::signature::Signer as _;
use sha2::{Digest, Sha256};

use crate::common;
use apb_engine::review::{ReviewCommand, post_decision_passkey, post_decision_via};
use apb_engine::scheduler::{RunOptions, run};
use apb_engine::state::RunStatus;

const DEADLINE: Duration = Duration::from_secs(60);

// Every test here holds the suite's shared env lock for its whole body:
// they redirect APB_CONFIG_DIR, and a config dir another test deleted
// mid-run is exactly the race that lock avoids.
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

const PLAIN: &str = r#"schema: 2
id: po
name: po
version: 1.0.0
nodes:
  - { id: start, type: start }
  - { id: g, type: human_review, options: [approve, reject] }
  - { id: done, type: finish, outcome: success }
edges:
  - { from: start, to: g }
  - { from: g, to: done }
"#;

const UNATTENDED_APPROVAL: &str = r#"schema: 2
id: ap
name: ap
version: 1.0.0
nodes:
  - { id: start, type: start }
  - { id: w, type: script, script: scripts/w.sh, runner: sh, results: [ { path: report.md } ] }
  - { id: g, type: human_review, options: [approve, reject], approves: { results: [w] }, unattended: { decide: approve, note: ci } }
  - { id: done, type: finish, outcome: success }
edges:
  - { from: start, to: w }
  - { from: w, to: g }
  - { from: g, to: done }
"#;

/// The seed of the software authenticator's P-256 key; the credential id is
/// its digest, so the store record and the proof agree.
const KEY_SEED: [u8; 32] = [
    0x42, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
    0,
];

/// A real, enrolled passkey record and the matching signing key: the same
/// software authenticator the dashboard ceremonies are tested with.
fn enroll_real_passkey(file: &Path) -> p256::ecdsa::SigningKey {
    let signing = p256::ecdsa::SigningKey::from_bytes(&KEY_SEED.into()).unwrap();
    let vk = signing.verifying_key();
    let point = vk.to_encoded_point(false);
    let x = point.x().unwrap().as_slice().to_vec();
    let y = point.y().unwrap().as_slice().to_vec();
    let map = ciborium::Value::Map(vec![
        (ciborium::Value::from(1i64), ciborium::Value::from(2i64)),
        (ciborium::Value::from(3i64), ciborium::Value::from(-7i64)),
        (ciborium::Value::from(-1i64), ciborium::Value::from(1i64)),
        (ciborium::Value::from(-2i64), ciborium::Value::Bytes(x)),
        (ciborium::Value::from(-3i64), ciborium::Value::Bytes(y)),
    ]);
    let mut cose = Vec::new();
    ciborium::ser::into_writer(&map, &mut cose).unwrap();
    let id = b64url(&Sha256::digest(KEY_SEED));
    apb_core::passkey::add_into(
        file,
        "laptop",
        &id,
        &base64::engine::general_purpose::STANDARD.encode(&cose),
        -7,
        0,
    )
    .unwrap();
    signing
}

fn b64url(bytes: &[u8]) -> String {
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
}

/// The proof a dashboard decision would carry: an authenticator signature
/// over `authenticatorData || SHA-256(clientDataJSON)` whose clientDataJSON
/// carries the challenge the drive derives from the stamped decision.
fn proof_for(
    signing: &p256::ecdsa::SigningKey,
    run_id: &str,
    node: &str,
    decision: &str,
    request: Option<u64>,
    nonce: Option<&str>,
    sign_count: u32,
) -> apb_engine::review::passkey_proof::PasskeyProof {
    let challenge = apb_engine::review::passkey_proof::decision_challenge(
        run_id, node, decision, request, nonce,
    );
    let client = format!(
        r#"{{"type":"webauthn.get","challenge":"{challenge}","origin":"http://localhost:7777","crossOrigin":false}}"#
    );
    let mut data = Vec::new();
    data.extend_from_slice(&Sha256::digest(b"localhost"));
    data.push(0x05); // UP | UV
    data.extend_from_slice(&sign_count.to_be_bytes());
    let mut signed = data.clone();
    signed.extend_from_slice(&Sha256::digest(client.as_bytes()));
    let signature: p256::ecdsa::Signature = signing.sign(&signed);
    let signature = signature.to_der();
    let credential_id = b64url(&Sha256::digest(KEY_SEED));
    apb_engine::review::passkey_proof::PasskeyProof {
        credential_id: credential_id.clone(),
        client_data_json: b64url(client.as_bytes()),
        authenticator_data: b64url(&data),
        signature: b64url(signature.as_bytes()),
    }
}

/// Points APB_CONFIG_DIR at a fresh directory with one passkey enrolled,
/// for the duration of `f`. The real config dir is never touched. Returns
/// the signing key of the enrolled credential.
fn with_passkey_enrolled(f: impl FnOnce(&Path, &p256::ecdsa::SigningKey)) {
    let cfg = tempfile::tempdir().unwrap();
    let signing = enroll_real_passkey(&cfg.path().join("passkeys.yaml"));
    let old = std::env::var("APB_CONFIG_DIR").ok();
    // SAFETY (tests): the suites already redirect APB_CONFIG_DIR elsewhere.
    unsafe { std::env::set_var("APB_CONFIG_DIR", cfg.path()) };
    f(cfg.path(), &signing);
    match old {
        Some(v) => unsafe { std::env::set_var("APB_CONFIG_DIR", v) },
        None => unsafe { std::env::remove_var("APB_CONFIG_DIR") },
    }
}

/// The shape of the old branch's test: a made-up prefix in the store with
/// no usable key, for tests that only need `count_strict() > 0`.
fn with_fake_passkey_enrolled(f: impl FnOnce(&Path)) {
    let cfg = tempfile::tempdir().unwrap();
    apb_core::passkey::add_into(
        &cfg.path().join("passkeys.yaml"),
        "laptop",
        "Q0VER0lKUg",
        "A0VER0lKUg==",
        -7,
        0,
    )
    .unwrap();
    let old = std::env::var("APB_CONFIG_DIR").ok();
    // SAFETY (tests): the suites already redirect APB_CONFIG_DIR elsewhere.
    unsafe { std::env::set_var("APB_CONFIG_DIR", cfg.path()) };
    f(cfg.path());
    match old {
        Some(v) => unsafe { std::env::set_var("APB_CONFIG_DIR", v) },
        None => unsafe { std::env::remove_var("APB_CONFIG_DIR") },
    }
}

fn seed(root: &Path, id: &str, yaml: &str, scripts: &[(&str, &str)]) {
    init_project(root).unwrap();
    let vdir = root.join(format!(".apb/playbooks/{id}/1.0.0"));
    fs::create_dir_all(vdir.join("scripts")).unwrap();
    fs::write(vdir.join("playbook.yaml"), yaml).unwrap();
    fs::write(root.join(format!(".apb/playbooks/{id}/current")), "1.0.0").unwrap();
    for (name, body) in scripts {
        fs::write(vdir.join("scripts").join(name), body).unwrap();
    }
}

fn poll<T>(what: &str, mut f: impl FnMut() -> Option<T>) -> T {
    let t = Instant::now();
    loop {
        if let Some(v) = f() {
            return v;
        }
        assert!(t.elapsed() < DEADLINE, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn started(
    root: &Path,
    id: &str,
    yaml: &str,
    scripts: &[(&str, &str)],
    opts: RunOptions,
) -> (PathBuf, u64, mpsc::Receiver<(RunStatus, Vec<Event>)>) {
    seed(root, id, yaml, scripts);
    let (tx, rx) = mpsc::channel();
    let owned = root.to_path_buf();
    let playbook = id.to_string();
    std::thread::spawn(move || {
        let r = run(&owned, &playbook, None, opts).unwrap();
        let rd = owned.join(".apb/runs").join(&r.run_id);
        let _ = tx.send((r.outcome, read_all(&rd).unwrap()));
    });
    let rd = poll("the run dir", || {
        fs::read_dir(root.join(".apb/runs"))
            .ok()?
            .filter_map(Result::ok)
            .map(|e| e.path())
            .find(|p| p.is_dir())
    });
    let seq = poll("the request", || {
        read_all(&rd).ok()?.iter().find_map(|e| match &e.payload {
            EventPayload::ReviewRequested { node, .. } if node == "g" => Some(e.seq),
            _ => None,
        })
    });
    (rd, seq, rx)
}

fn approve() -> ReviewCommand {
    ReviewCommand {
        node: "g".into(),
        decision: "approve".into(),
        note: String::new(),
    }
}

/// With a passkey enrolled, the terminal is no longer a channel for a
/// person-only gate: the refusal names the passkey and points at the
/// dashboard, and nothing is queued.
#[test]
fn a_passkey_enrolled_takes_the_terminal_off_a_person_only_gate() {
    let _guard = common::env_lock();
    with_fake_passkey_enrolled(|_| {
        let dir = tempfile::tempdir().unwrap();
        let (rd, _, _rx) = started(dir.path(), "po", PERSON_ONLY, &[], RunOptions::default());
        for channel in [DecisionChannel::Terminal, DecisionChannel::CliUnattended] {
            match post_decision_via(&rd, approve(), None, None, channel) {
                Err(EngineError::ChannelRefused { refusal, message }) => {
                    assert_eq!(refusal, CallerRefusal::PasskeyRequired, "{channel:?}");
                    assert!(message.contains("dashboard"), "{message}");
                    assert!(message.contains("passkey"), "{message}");
                }
                other => panic!("{channel:?}: {other:?}"),
            }
        }
        let queued = fs::read_to_string(rd.join("reviews.jsonl")).unwrap_or_default();
        assert!(
            !queued.contains("approve"),
            "a refused decision must not be queued: {queued}"
        );
    });
}

/// The dashboard route's decision, posted through `post_decision_passkey`
/// with the assertion it verified, is consumed: the run finishes.
#[test]
fn a_dashboard_decision_with_a_verified_proof_decides_the_gate() {
    let _guard = common::env_lock();
    with_passkey_enrolled(|_, signing| {
        let dir = tempfile::tempdir().unwrap();
        let (rd, seq, rx) = started(dir.path(), "po", PERSON_ONLY, &[], RunOptions::default());
        let run_id = rd.file_name().unwrap().to_str().unwrap().to_string();
        let proof = proof_for(signing, &run_id, "g", "approve", Some(seq), None, 1);
        post_decision_passkey(
            &rd,
            approve(),
            None,
            Some(seq),
            DecisionChannel::DashboardKeyless,
            proof.credential_id[..8].to_string(),
            proof,
        )
        .unwrap();
        let (outcome, _) = rx.recv_timeout(DEADLINE).unwrap();
        assert_eq!(outcome, RunStatus::Succeeded, "the gate decided the run");
    });
}

/// A forged stamp in the channel file is never consumed: a hand-written
/// line with a passkey prefix but no assertion, and one with an assertion
/// that does not verify, both leave the run waiting; the doctor names the
/// reason. A real proof decides afterwards.
#[test]
fn a_forged_passkey_stamp_is_never_consumed() {
    let _guard = common::env_lock();
    with_passkey_enrolled(|_, signing| {
        let dir = tempfile::tempdir().unwrap();
        let (rd, seq, rx) = started(dir.path(), "po", PERSON_ONLY, &[], RunOptions::default());
        {
            use std::io::Write;
            let mut f = fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(rd.join("reviews.jsonl"))
                .unwrap();
            // What a same-user process writes by hand: the prefix, the
            // request and a dashboard channel, but no assertion at all.
            writeln!(
                f,
                "{{\"seq\":1,\"node\":\"g\",\"decision\":\"approve\",\"request\":{seq},\"channel\":\"dashboard_session\",\"passkey\":\"Q0VER0lK\"}}"
            )
            .unwrap();
            // And one carrying a proof that signs a different decision.
            let wrong = proof_for(signing, "other-run", "g", "approve", Some(seq), None, 2);
            writeln!(
                f,
                "{{\"seq\":2,\"node\":\"g\",\"decision\":\"approve\",\"request\":{seq},\"channel\":\"dashboard_session\",\"passkey\":\"{}\",\"passkey_proof\":{}}}",
                &wrong.credential_id[..8],
                serde_json::to_string(&wrong).unwrap()
            )
            .unwrap();
        }
        // Well past the point a forged stamp would have ended the run.
        std::thread::sleep(Duration::from_millis(300));
        assert!(
            !matches!(rx.try_recv(), Ok((RunStatus::Succeeded, _))),
            "a forged stamp must not decide a gated run"
        );
        let run_id = rd.file_name().unwrap().to_str().unwrap().to_string();
        let proof = proof_for(signing, &run_id, "g", "approve", Some(seq), None, 3);
        post_decision_passkey(
            &rd,
            approve(),
            None,
            Some(seq),
            DecisionChannel::DashboardKeyless,
            proof.credential_id[..8].to_string(),
            proof,
        )
        .unwrap();
        let (outcome, _) = rx.recv_timeout(DEADLINE).unwrap();
        assert_eq!(outcome, RunStatus::Succeeded, "the real proof decides");
    });
}

/// A channel entry a passkey-less machine left behind (a terminal entry
/// with no passkey stamp) is never consumed once this machine holds a
/// passkey: the run keeps waiting until the dashboard's stamped decision
/// arrives, and the doctor names the unstamped line.
#[test]
fn an_unstamped_entry_is_never_consumed_once_a_passkey_is_enrolled() {
    let _guard = common::env_lock();
    with_passkey_enrolled(|_, signing| {
        let dir = tempfile::tempdir().unwrap();
        let (rd, seq, rx) = started(dir.path(), "po", PERSON_ONLY, &[], RunOptions::default());
        // The line an older apb (or a passkey-less machine) left in the
        // channel file: stamped with the request and the terminal channel,
        // but with no passkey assertion.
        {
            use std::io::Write;
            let mut f = fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(rd.join("reviews.jsonl"))
                .unwrap();
            writeln!(
                f,
                "{{\"seq\":1,\"node\":\"g\",\"decision\":\"approve\",\"request\":{seq},\"channel\":\"terminal\"}}"
            )
            .unwrap();
        }
        // Well past the point the unstamped path would have ended the run.
        std::thread::sleep(Duration::from_millis(300));
        assert!(
            !matches!(rx.try_recv(), Ok((RunStatus::Succeeded, _))),
            "an unstamped entry must not decide a gated run once a passkey is enrolled"
        );
        let run_id = rd.file_name().unwrap().to_str().unwrap().to_string();
        let proof = proof_for(signing, &run_id, "g", "approve", Some(seq), None, 4);
        post_decision_passkey(
            &rd,
            approve(),
            None,
            Some(seq),
            DecisionChannel::DashboardKeyless,
            proof.credential_id[..8].to_string(),
            proof,
        )
        .unwrap();
        let (outcome, events) = rx.recv_timeout(DEADLINE).unwrap();
        assert_eq!(outcome, RunStatus::Succeeded);
        assert!(
            events.iter().any(
                |e| matches!(&e.payload, EventPayload::ReviewDecided { node, .. } if node == "g")
            ),
            "the stamped decision is what the drive consumed"
        );
    });
}

/// The engine's own decision on a bound gate (an unattended run's declared
/// outcome) is still consumed while a passkey is enrolled: the exemption
/// holds at consumption, not only at append (the regression F8).
#[test]
fn an_unattended_bound_gate_decision_is_consumed_with_a_passkey_enrolled() {
    let _guard = common::env_lock();
    with_fake_passkey_enrolled(|_| {
        let dir = tempfile::tempdir().unwrap();
        let (rd, _seq, rx) = started(
            dir.path(),
            "ap",
            UNATTENDED_APPROVAL,
            &[("w.sh", "printf 'draft\\n' > report.md\necho ok\n")],
            RunOptions {
                unattended: true,
                ..RunOptions::default()
            },
        );
        let (outcome, events) = rx.recv_timeout(DEADLINE).unwrap();
        assert_eq!(
            outcome,
            RunStatus::Succeeded,
            "the unattended approval ran on"
        );
        assert!(
            events.iter().any(
                |e| matches!(&e.payload, EventPayload::ReviewDecided { node, .. } if node == "g")
            ),
            "the bound gate was decided"
        );
        let _ = rd;
    });
}

/// The requirement itself: a person-only or publish gate always needs the
/// assertion while a passkey is enforced, an ordinary gate with no bound
/// request does not, a node the playbook does not have is not a passkey
/// matter, and a playbook that does not read fails closed.
#[test]
fn passkey_required_follows_the_gate() {
    let _guard = common::env_lock();
    with_fake_passkey_enrolled(|_| {
        let dir = tempfile::tempdir().unwrap();
        let rd = dir.path().join("r");
        fs::create_dir_all(&rd).unwrap();
        let cmd = |node: &str| ReviewCommand {
            node: node.into(),
            decision: "approve".into(),
            note: String::new(),
        };
        fs::write(rd.join("playbook.yaml"), PERSON_ONLY).unwrap();
        assert!(apb_engine::review::passkey_required(&rd, &cmd("g")));
        fs::write(rd.join("playbook.yaml"), PLAIN).unwrap();
        assert!(
            !apb_engine::review::passkey_required(&rd, &cmd("g")),
            "an ordinary gate with no bound request stays as it was"
        );
        assert!(
            !apb_engine::review::passkey_required(&rd, &cmd("no-such-node")),
            "an unknown node is refused as unknown, not as passkey_required"
        );
        fs::write(rd.join("playbook.yaml"), "schema: 2\nid: x\n").unwrap();
        assert!(
            apb_engine::review::passkey_required(&rd, &cmd("g")),
            "a playbook that does not read fails closed"
        );
    });
}

/// A store that exists but does not read switches nothing off: the
/// person-only gate refuses a decision through any channel (fail closed,
/// the regression F6), and the requirement stands for an ordinary gate too.
#[test]
fn a_corrupt_passkey_store_refuses_person_only_decisions() {
    let _guard = common::env_lock();
    let cfg = tempfile::tempdir().unwrap();
    std::fs::write(cfg.path().join("passkeys.yaml"), "passkeys: [broken").unwrap();
    let old = std::env::var("APB_CONFIG_DIR").ok();
    // SAFETY (tests): the suites already redirect APB_CONFIG_DIR elsewhere.
    unsafe { std::env::set_var("APB_CONFIG_DIR", cfg.path()) };
    let dir = tempfile::tempdir().unwrap();
    let (rd, _, _rx) = started(dir.path(), "po", PERSON_ONLY, &[], RunOptions::default());
    for post in [
        |rd: &Path| post_decision_via(rd, approve(), None, None, DecisionChannel::Terminal),
        |rd: &Path| post_decision_via(rd, approve(), None, None, DecisionChannel::DashboardSession),
    ] {
        match post(&rd) {
            Err(EngineError::ChannelRefused { refusal, message }) => {
                assert_eq!(refusal, CallerRefusal::PasskeyRequired);
                assert!(message.contains("passkey"), "{message}");
            }
            other => panic!("{other:?}"),
        }
    }
    match old {
        Some(v) => unsafe { std::env::set_var("APB_CONFIG_DIR", v) },
        None => unsafe { std::env::remove_var("APB_CONFIG_DIR") },
    }
}

/// With no passkey enrolled, nothing changes: the terminal decides a
/// person-only gate after the typed confirmation, as before.
#[test]
fn no_passkey_enrolled_keeps_today_behaviour() {
    let _guard = common::env_lock();
    let dir = tempfile::tempdir().unwrap();
    let (rd, _seq, rx) = started(dir.path(), "po", PERSON_ONLY, &[], RunOptions::default());
    post_decision_via(&rd, approve(), None, None, DecisionChannel::Terminal).unwrap();
    let (outcome, _) = rx.recv_timeout(DEADLINE).unwrap();
    assert_eq!(outcome, RunStatus::Succeeded);
}
