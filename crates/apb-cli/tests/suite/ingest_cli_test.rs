//! `apb ingest` and the ingest half of `apb connector doctor`, driven
//! against the real binary with a temp global config dir passed per spawn
//! (never by mutating this process's env: the other suites in this binary
//! spawn concurrently).

use std::path::Path;
use std::process::Command;

fn apb_bin() -> &'static str {
    env!("CARGO_BIN_EXE_apb")
}

fn run(cfg: &Path, args: &[&str]) -> (String, String, bool) {
    let out = Command::new(apb_bin())
        .args(args)
        .env("APB_CONFIG_DIR", cfg)
        .env_remove("CI")
        .env_remove("APB_NO_REGISTRY")
        .output()
        .expect("run apb");
    (
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
        out.status.success(),
    )
}

const CONNECTOR_YAML: &str = r#"
name: echo-hooks
version: 0.1.0
webhook:
  challenge: meta_hub
  verify_token: "{{secret.verify_token}}"
  signature:
    scheme: hmac_sha256_hex
    header: X-Hub-Signature-256
    prefix: "sha256="
    secret: "{{secret.app_secret}}"
  dedupe_path: id
account_fields:
  - name: verify_token
    required: true
    secret: true
  - name: app_secret
    required: true
    secret: true
functions:
  - name: inbox_read
    description: Read pending inbound events
    read_only: true
    response_pick: [events, cursor]
    inbox:
      op: read
"#;

fn seed_connector(cfg: &Path) {
    let cdir = cfg.join("connectors").join("echo-hooks");
    std::fs::create_dir_all(&cdir).unwrap();
    std::fs::write(cdir.join("connector.yaml"), CONNECTOR_YAML).unwrap();
    let adir = cfg.join("connector-config");
    std::fs::create_dir_all(&adir).unwrap();
    std::fs::write(
        adir.join("echo-hooks.yaml"),
        "accounts:\n  - name: main\n    default: true\n    verify_token: \"{{env.APB_T}}\"\n    app_secret: \"{{env.APB_S}}\"\n",
    )
    .unwrap();
}

#[test]
fn ingest_refuses_an_unparseable_bind_address() {
    let cfg = tempfile::tempdir().unwrap();
    let (_out, err, ok) = run(cfg.path(), &["ingest", "--bind", "not-an-ip"]);
    assert!(!ok, "an unparseable bind must fail rather than fall back");
    assert!(
        err.contains("not-an-ip"),
        "the error names the value: {err}"
    );
    assert!(!err.contains('!'), "no exclamation marks: {err}");
    assert!(!err.contains('\u{2014}'), "no em-dashes: {err}");
}

#[test]
fn ingest_is_listed_in_help_with_its_flags() {
    let cfg = tempfile::tempdir().unwrap();
    let (out, _, ok) = run(cfg.path(), &["help"]);
    assert!(ok);
    assert!(out.contains("ingest"), "the command is discoverable: {out}");

    let (out, _, ok) = run(cfg.path(), &["ingest", "--help"]);
    assert!(ok);
    assert!(out.contains("--bind"), "{out}");
    assert!(out.contains("--port"), "{out}");
    assert!(!out.contains('!'), "no exclamation marks: {out}");
}

#[test]
fn doctor_reports_the_ingest_surface_of_a_webhook_connector() {
    let cfg = tempfile::tempdir().unwrap();
    seed_connector(cfg.path());
    std::fs::write(
        cfg.path().join("config.yaml"),
        "ingest:\n  enabled: true\n  public_base_url: https://hooks.example.com\n",
    )
    .unwrap();

    let (out, _err, _ok) = run(cfg.path(), &["connector", "doctor"]);
    assert!(
        out.contains("connector `echo-hooks`: ingest"),
        "the ingest row is present: {out}"
    );
    assert!(
        out.contains("inbox_read"),
        "the row names the inbox functions: {out}"
    );
    assert!(
        out.contains("https://hooks.example.com/hooks/echo-hooks/main"),
        "the exact callback URL is printed for pasting into a provider console: {out}"
    );
    assert!(
        out.contains("account `main`: inbox"),
        "the pending depth is reported: {out}"
    );
    assert!(!out.contains('!'), "no exclamation marks: {out}");
    assert!(!out.contains('\u{2014}'), "no em-dashes: {out}");
}

/// The account directory a stored delivery would create, for tests that need
/// the doctor to read something back.
fn inbox_dir(cfg: &Path) -> std::path::PathBuf {
    let dir = cfg.join("connector-inbox").join("echo-hooks").join("main");
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

#[test]
fn doctor_reports_a_nonzero_pending_depth() {
    let cfg = tempfile::tempdir().unwrap();
    seed_connector(cfg.path());
    std::fs::write(
        cfg.path().join("config.yaml"),
        "ingest:\n  enabled: true\n  public_base_url: https://hooks.example.com\n",
    )
    .unwrap();
    // Two stored deliveries and no consumer cursor, so both are pending. The
    // line has to carry the counts, not just its own name: the row that only
    // asserted the name passed just as happily when the detail read "inbox
    // unreadable".
    std::fs::write(
        inbox_dir(cfg.path()).join("events.jsonl"),
        "{\"seq\":1,\"received_at\":1700000000000,\"provider_id\":\"e1\",\"body\":{}}\n{\"seq\":2,\"received_at\":1700000000001,\"provider_id\":\"e2\",\"body\":{}}\n",
    )
    .unwrap();

    let (out, _err, _ok) = run(cfg.path(), &["connector", "doctor"]);
    let line = out
        .lines()
        .find(|l| l.contains("account `main`: inbox"))
        .expect("inbox row");
    assert!(
        line.contains("2 pending of 2 stored"),
        "the row carries the depth: {line}"
    );
    assert!(line.starts_with("[ok]"), "a readable inbox is ok: {line}");
}

#[test]
fn doctor_warns_when_the_inbox_cannot_be_read() {
    let cfg = tempfile::tempdir().unwrap();
    seed_connector(cfg.path());
    std::fs::write(
        cfg.path().join("config.yaml"),
        "ingest:\n  enabled: true\n  public_base_url: https://hooks.example.com\n",
    )
    .unwrap();
    // A corrupt persisted counter makes `depth` fail, which is what the
    // inbox row reports on.
    std::fs::write(inbox_dir(cfg.path()).join("dropped.count"), "not-a-number").unwrap();

    let (out, _err, _ok) = run(cfg.path(), &["connector", "doctor"]);
    let line = out
        .lines()
        .find(|l| l.contains("account `main`: inbox"))
        .expect("inbox row");
    assert!(
        line.contains("inbox unreadable"),
        "the row says what is wrong: {line}"
    );
    assert!(
        line.starts_with("[warn]"),
        "an unreadable inbox is a warning, not a green line saying it is broken: {line}"
    );
    assert!(!out.contains('!'), "no exclamation marks: {out}");
}

/// The callback row names the real obstacle. It used to say
/// `public_base_url is not set` for both of its `None` causes, so an operator
/// with a correct base and an account name that cannot appear in a URL was
/// sent to fix a config key that was already right.
#[test]
fn doctor_names_an_unroutable_account_rather_than_the_public_base_url() {
    let cfg = tempfile::tempdir().unwrap();
    seed_connector(cfg.path());
    std::fs::write(
        cfg.path()
            .join("connector-config")
            .join("echo-hooks.yaml"),
        "accounts:\n  - name: Main Account\n    default: true\n    verify_token: \"{{env.APB_T}}\"\n    app_secret: \"{{env.APB_S}}\"\n",
    )
    .unwrap();
    std::fs::write(
        cfg.path().join("config.yaml"),
        "ingest:\n  enabled: true\n  public_base_url: https://hooks.example.com\n",
    )
    .unwrap();

    let (out, _err, _ok) = run(cfg.path(), &["connector", "doctor"]);
    let line = out
        .lines()
        .find(|l| l.contains(": callback"))
        .expect("callback row");
    assert!(
        line.contains("not a routable path segment"),
        "the segment is the problem, and the row says so: {line}"
    );
    assert!(
        !line.contains("public_base_url is not set"),
        "a base URL that is set must not be blamed: {line}"
    );
    assert!(line.starts_with("[warn]"), "was: {line}");
}

#[test]
fn doctor_reports_no_drops_ok_when_nothing_was_dropped() {
    let cfg = tempfile::tempdir().unwrap();
    seed_connector(cfg.path());
    std::fs::write(
        cfg.path().join("config.yaml"),
        "ingest:\n  enabled: true\n  public_base_url: https://hooks.example.com\n",
    )
    .unwrap();

    let (out, _err, _ok) = run(cfg.path(), &["connector", "doctor"]);
    assert!(
        out.contains("account `main`: dropped: no deliveries dropped"),
        "an absent counter reads as no drops, reported ok: {out}"
    );
}

#[test]
fn doctor_warns_when_deliveries_were_dropped_by_the_accept_cap() {
    let cfg = tempfile::tempdir().unwrap();
    seed_connector(cfg.path());
    std::fs::write(
        cfg.path().join("config.yaml"),
        "ingest:\n  enabled: true\n  public_base_url: https://hooks.example.com\n",
    )
    .unwrap();

    // Seeds the persisted counter directly, the way `Inbox::note_dropped`
    // would after a real accept-cap drop, without needing this test to send
    // 600+ requests at the listener.
    let inbox_dir = cfg
        .path()
        .join("connector-inbox")
        .join("echo-hooks")
        .join("main");
    std::fs::create_dir_all(&inbox_dir).unwrap();
    std::fs::write(inbox_dir.join("dropped.count"), "3").unwrap();

    let (out, _err, _ok) = run(cfg.path(), &["connector", "doctor"]);
    assert!(
        out.contains("account `main`: dropped"),
        "the dropped row is present: {out}"
    );
    assert!(
        out.contains("3 deliveries were dropped by the accept cap"),
        "the row names the count and the cause: {out}"
    );
    let dropped_line = out
        .lines()
        .find(|l| l.contains("account `main`: dropped"))
        .expect("dropped row line");
    assert!(
        dropped_line.starts_with("[warn]"),
        "a nonzero dropped count is a warning, not merely informational: {dropped_line}"
    );
    assert!(!out.contains('!'), "no exclamation marks: {out}");
    assert!(!out.contains('\u{2014}'), "no em-dashes: {out}");
}

#[test]
fn doctor_warns_that_a_project_only_account_cannot_be_addressed() {
    let cfg = tempfile::tempdir().unwrap();
    let project = tempfile::tempdir().unwrap();
    seed_connector(cfg.path());
    // Remove the global account and define the same name project-side only.
    std::fs::remove_file(cfg.path().join("connector-config").join("echo-hooks.yaml")).unwrap();
    let pdir = project.path().join(".apb/connector-config");
    std::fs::create_dir_all(&pdir).unwrap();
    std::fs::write(
        pdir.join("echo-hooks.yaml"),
        "accounts:\n  - name: main\n    default: true\n    verify_token: \"{{env.APB_T}}\"\n    app_secret: \"{{env.APB_S}}\"\n",
    )
    .unwrap();

    let out = Command::new(apb_bin())
        .args(["connector", "doctor"])
        .current_dir(project.path())
        .env("APB_CONFIG_DIR", cfg.path())
        .env("APB_NO_REGISTRY", "1")
        .env_remove("CI")
        .output()
        .expect("run apb connector doctor");
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    assert!(
        stdout.contains("no delivery can address it"),
        "a project-only account must be called out: {stdout}"
    );
    assert!(stdout.contains("[warn]"), "and as a warning: {stdout}");
    assert!(!stdout.contains('!'), "no exclamation marks: {stdout}");
}

#[test]
fn doctor_warns_when_no_public_base_url_is_configured() {
    let cfg = tempfile::tempdir().unwrap();
    seed_connector(cfg.path());
    std::fs::write(cfg.path().join("config.yaml"), "ingest:\n  enabled: true\n").unwrap();

    let (out, _err, _ok) = run(cfg.path(), &["connector", "doctor"]);
    // Located first, then asserted on: every doctor run against a fresh temp
    // config already emits an unrelated `[warn] ... trust: not approved`, so
    // a bare `contains("[warn]")` would pass with this row missing entirely.
    let line = out
        .lines()
        .find(|l| l.contains("account `main`: callback"))
        .expect("callback row");
    assert!(
        line.contains("ingest.public_base_url is not set"),
        "the missing base URL is named: {line}"
    );
    assert!(
        line.starts_with("[warn]"),
        "an unprintable callback URL is a warning, not a failure: {line}"
    );
}

#[test]
fn doctor_warns_when_ingest_is_enabled_but_nothing_can_receive() {
    let cfg = tempfile::tempdir().unwrap();
    // A connector with no webhook block at all.
    let cdir = cfg.path().join("connectors").join("plain");
    std::fs::create_dir_all(&cdir).unwrap();
    std::fs::write(
        cdir.join("connector.yaml"),
        "name: plain\nversion: 0.1.0\nfunctions:\n  - name: ping\n    description: d\n    mock: { status: 200, body: {} }\n",
    )
    .unwrap();
    std::fs::write(cfg.path().join("config.yaml"), "ingest:\n  enabled: true\n").unwrap();

    let (out, _err, _ok) = run(cfg.path(), &["connector", "doctor"]);
    assert!(out.contains("ingest: config"), "{out}");
    assert!(
        out.contains("no installed connector declares a webhook block"),
        "the pointless listener is called out: {out}"
    );
}

#[test]
fn doctor_says_nothing_about_ingest_when_it_is_disabled() {
    let cfg = tempfile::tempdir().unwrap();
    seed_connector(cfg.path());
    let (out, _err, _ok) = run(cfg.path(), &["connector", "doctor"]);
    assert!(
        !out.contains("ingest: listener"),
        "a disabled listener is not probed: {out}"
    );
    // The per-connector ingest row is still shown, because the connector's
    // ability to receive does not depend on this machine's config.
    assert!(out.contains("connector `echo-hooks`: ingest"), "{out}");
}
