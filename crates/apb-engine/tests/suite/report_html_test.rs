//! The HTML run report opens from disk and runs nothing: no script, no
//! event handler, no style attribute, no external resource, a policy that
//! pins its one stylesheet by hash, and every string from the run escaped.

use std::path::Path;

use apb_engine::report_html::{ANSWER_MAX_BYTES, JOURNAL_MAX_EVENTS, render};

const SNAPSHOT: &str = "schema: 2\nid: p\nname: P\nversion: 1.0.0\nnodes:\n  - { id: s, type: start }\n  - { id: done, type: finish, outcome: success, prompt: sum up }\nedges:\n  - { from: s, to: done }\n";

const HOSTILE: [&str; 4] = [
    "<script>alert(1)</script>",
    "\"><img src=x onerror=alert(1)>",
    "</style><b>x</b>",
    "Tom &amp; Jerry",
];

/// A run directory written by hand: its journal (`run_started`, `filler`
/// progress events, then `run_error`, the finish node's `node_finished`
/// with `answer` as its output, and `run_finished`) and, when `snapshot`,
/// its playbook copy.
fn run_dir(root: &Path, snapshot: bool, answer: &str, filler: usize) -> std::path::PathBuf {
    let dir = root.join(".apb/runs/p-1");
    std::fs::create_dir_all(&dir).unwrap();
    if snapshot {
        std::fs::write(dir.join("playbook.yaml"), SNAPSHOT).unwrap();
    }
    let mut lines = vec![
        r#"{"seq":0,"ts":1,"type":"run_started","playbook":"p","version":"1.0.0"}"#.to_string(),
    ];
    let mut seq = 1;
    for i in 0..filler {
        lines.push(format!(
            r#"{{"seq":{seq},"ts":2,"type":"run_progress","done":{i},"total":{filler},"label":null}}"#
        ));
        seq += 1;
    }
    let reason = serde_json::to_string(&HOSTILE.join(" ")).unwrap();
    lines.push(format!(
        r#"{{"seq":{seq},"ts":3,"type":"run_error","node":null,"reason":{reason}}}"#
    ));
    seq += 1;
    let output = serde_json::to_string(answer).unwrap();
    lines.push(format!(
        r#"{{"seq":{seq},"ts":4,"type":"node_finished","node":"done","status":"succeeded","attempt":1,"output":{output},"artifacts":[]}}"#
    ));
    seq += 1;
    lines.push(format!(
        r#"{{"seq":{seq},"ts":5,"type":"run_finished","outcome":"success"}}"#
    ));
    std::fs::write(dir.join("events.jsonl"), lines.join("\n") + "\n").unwrap();
    dir
}

fn between<'a>(html: &'a str, open: &str, close: &str) -> &'a str {
    let start = html.find(open).unwrap() + open.len();
    let end = start + html[start..].find(close).unwrap();
    &html[start..end]
}

#[test]
fn the_report_runs_nothing_and_loads_nothing() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = run_dir(tmp.path(), true, &HOSTILE.join("\n"), 3);
    let html = render(tmp.path(), &dir).unwrap();
    let lower = html.to_ascii_lowercase();
    assert!(!lower.contains("<script"));
    assert!(!lower.contains("javascript:"));
    assert!(!lower.contains("style="));
    assert!(!lower.contains("<img"));
    let handler = regex::Regex::new(r"<[^>]*\son[a-z]+\s*=").unwrap();
    assert!(!handler.is_match(&lower));

    let csp_at = html.find("Content-Security-Policy").unwrap();
    let style_at = html.find("<style>").unwrap();
    assert!(csp_at < style_at, "the policy comes before the stylesheet");
    assert_eq!(html.matches("<style>").count(), 1);
    assert_eq!(html.matches("</style>").count(), 1);
    let sheet = between(&html, "<style>", "</style>");
    let policy = between(&html, "Content-Security-Policy\" content=\"", "\">");
    let pinned = format!(
        "style-src &#39;sha256-{}&#39;",
        apb_core::content::sha256_base64(sheet.as_bytes())
    );
    assert!(policy.contains(&pinned), "{policy}");
    assert!(policy.contains("default-src &#39;none&#39;"), "{policy}");
}

#[test]
fn every_string_from_the_run_is_escaped() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = run_dir(tmp.path(), true, &HOSTILE.join("\n"), 0);
    let html = render(tmp.path(), &dir).unwrap();
    assert!(html.contains("&lt;script&gt;alert(1)&lt;/script&gt;"));
    assert!(html.contains("&quot;&gt;&lt;img src=x onerror=alert(1)&gt;"));
    assert!(html.contains("&lt;/style&gt;&lt;b&gt;x&lt;/b&gt;"));
    assert!(html.contains("Tom &amp;amp; Jerry"));
    assert!(!html.contains("<b>x</b>"));
}

#[test]
fn a_run_without_a_playbook_snapshot_still_renders() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = run_dir(tmp.path(), false, "answer", 0);
    let html = render(tmp.path(), &dir).unwrap();
    assert!(html.contains("The run has no playbook snapshot."));
    assert!(html.contains("<h1>p run p-1</h1>"));
}

#[test]
fn the_journal_and_the_answer_are_capped() {
    let tmp = tempfile::tempdir().unwrap();
    let long = "a".repeat(ANSWER_MAX_BYTES + 4096);
    let dir = run_dir(tmp.path(), true, &long, JOURNAL_MAX_EVENTS + 10);
    let html = render(tmp.path(), &dir).unwrap();
    // The answer's own cut keeps only its head: its marker has no tail.
    let cut = long.len() - ANSWER_MAX_BYTES;
    assert!(html.contains(&format!("[... {cut} bytes cut]")));
    assert!(!html.contains(&long));
    assert!(html.contains(&format!(
        "Journal: the last {JOURNAL_MAX_EVENTS} events (14 earlier ones left out)"
    )));
}

const CONNECTOR: &str = "mock-tracker";
const SECRET_VAR: &str = "APB_REPORT_TEST_TOKEN";
/// A known secret with a quote in it: serialised, it is written `\"`.
const QUOTED_SECRET: &str = "plain\"quote-value";
const QUOTED_TAIL: &str = "quote-value";

/// A project whose installed connector references `SECRET_VAR`, set to
/// `value` in its secrets file, so the redactor knows the value.
fn with_known_secret(cfg: &Path, root: &Path, value: &str) {
    let src = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/connectors")
        .join(CONNECTOR);
    let dst = cfg.join("connectors").join(CONNECTOR);
    std::fs::create_dir_all(&dst).unwrap();
    std::fs::copy(src.join("connector.yaml"), dst.join("connector.yaml")).unwrap();
    let accounts = apb_core::connector::config::project_config_path(root, CONNECTOR);
    std::fs::create_dir_all(accounts.parent().unwrap()).unwrap();
    std::fs::write(
        accounts,
        format!(
            "accounts:\n  - name: me\n    default: true\n    base_url: http://127.0.0.1:9\n    token: \"{{{{env.{SECRET_VAR}}}}}\"\n"
        ),
    )
    .unwrap();
    std::fs::write(
        root.join(".apb/secrets.env"),
        format!("{SECRET_VAR}={value}\n"),
    )
    .unwrap();
}

#[test]
fn a_secret_is_redacted_before_the_journal_line_is_serialised() {
    let _lock = crate::common::env_lock();
    let cfg = tempfile::tempdir().unwrap();
    let _cfg_var = crate::common::EnvVar::set("APB_CONFIG_DIR", cfg.path());
    let tmp = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(tmp.path().join(".apb")).unwrap();
    with_known_secret(cfg.path(), tmp.path(), QUOTED_SECRET);
    let dir = run_dir(
        tmp.path(),
        true,
        &format!("the password is {QUOTED_SECRET} today"),
        0,
    );
    let html = render(tmp.path(), &dir).unwrap();
    assert!(
        !html.contains(QUOTED_TAIL),
        "a secret with a quote must not reach the page"
    );
    assert!(html.contains("[redacted]"));
}

#[test]
fn a_secret_the_cut_runs_through_is_redacted_whole() {
    const AT_CUT: &str = "cutsecret-midvalue";
    let _lock = crate::common::env_lock();
    let cfg = tempfile::tempdir().unwrap();
    let _cfg_var = crate::common::EnvVar::set("APB_CONFIG_DIR", cfg.path());
    let tmp = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(tmp.path().join(".apb")).unwrap();
    with_known_secret(cfg.path(), tmp.path(), AT_CUT);
    // The answer's cut falls inside the secret.
    let answer = format!(
        "{}{AT_CUT}{}",
        "a".repeat(ANSWER_MAX_BYTES - 6),
        "b".repeat(100)
    );
    let dir = run_dir(tmp.path(), true, &answer, 0);
    let html = render(tmp.path(), &dir).unwrap();
    assert!(
        !html.contains("cutsec"),
        "no part of a secret cut through may reach the page"
    );
}
