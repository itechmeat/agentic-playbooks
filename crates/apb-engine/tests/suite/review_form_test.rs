//! Typed fields on a review gate (people-in-the-loop D3): the values a
//! decision carries reach the next node and `output_field` edges, are
//! checked when posted and again when consumed, scrubbed of the run's
//! secrets, journaled, covered by a passkey assertion, and a gate without
//! fields keeps the bare decision word.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use apb_core::registry::init_project;
use apb_engine::RunResult;
use apb_engine::decision_channel::DecisionChannel;
use apb_engine::event::{EventPayload, read_all};
use apb_engine::review::passkey_proof;
use apb_engine::run_view::RunView;
use apb_engine::scheduler::{RunOptions, run};
use apb_engine::state::RunStatus;
use base64::Engine as _;
use p256::ecdsa::signature::Signer as _;
use sha2::{Digest, Sha256};

use crate::common;

const POLL_DEADLINE: Duration = Duration::from_secs(20);
const POLL_STEP: Duration = Duration::from_millis(10);

/// The prep script prints the amount the gate defaults to: below the
/// threshold of the `output_field` edges, so a decided value above it
/// routes the other way.
const PREP: &str = "echo 900\n";

/// A fielded gate: the extracted total shown and defaulted, a remark, and
/// the values read downstream by a `prompt` node and an `output_field`
/// edge.
const FIELDS_WF: &str = r#"schema: 2
id: rform
name: RForm
version: 1.0.0
nodes:
  - { id: start, type: start }
  - { id: prep, type: script, script: "scripts/prep.sh", runner: sh }
  - id: gate
    type: human_review
    options: [approve, reject]
    show:
      - { label: Total, text: "{{nodes.prep.output}}" }
    fields:
      - { name: amount, type: number, required: [approve], default: "{{nodes.prep.output}}" }
      - { name: remark, type: text, max_len: 200 }
  - { id: echo, type: prompt, prompt: "the amount is {{nodes.gate.output.amount}}" }
  - { id: big, type: finish, outcome: success }
  - { id: small, type: finish, outcome: failure }
edges:
  - { from: start, to: prep }
  - { from: prep, to: gate }
  - { from: gate, to: echo }
  - { from: echo, to: big, condition: { type: output_field, node: gate, field: amount, gt: 1000 } }
  - { from: echo, to: small, condition: { type: output_field, node: gate, field: amount, lte: 1000 } }
"#;

/// A gate without fields: the output stays the decision word.
const PLAIN_WF: &str = r#"schema: 2
id: rform
name: RForm
version: 1.0.0
nodes:
  - { id: start, type: start }
  - { id: gate, type: human_review, options: [approve, reject] }
  - { id: ok, type: finish, outcome: success }
edges:
  - { from: start, to: gate }
  - { from: gate, to: ok, condition: { type: review_status, equals: approve } }
"#;

/// A secret param typed into a field reaches neither journal nor output.
const SECRET_WF: &str = r#"schema: 2
id: rform
name: RForm
version: 1.0.0
params:
  - { name: token, type: text, secret: true }
nodes:
  - { id: start, type: start }
  - id: gate
    type: human_review
    options: [approve, reject]
    fields:
      - { name: note, type: text, max_len: 200 }
      - { name: count, type: number }
  - { id: ok, type: finish, outcome: success }
edges:
  - { from: start, to: gate }
  - { from: gate, to: ok, condition: { type: review_status, equals: approve } }
"#;

/// An untrusted param flows into a field default; the reader after the
/// gate is a follower.
const UNTRUSTED_WF: &str = r#"schema: 2
id: rform
name: RForm
version: 1.0.0
defaults: { profile: main }
params:
  - { name: body, type: text, untrusted: true }
nodes:
  - { id: start, type: start }
  - { id: fetch, type: prompt, prompt: "fetched {{params.body}}" }
  - id: gate
    type: human_review
    options: [approve, reject]
    show:
      - { label: Fetched, text: "{{nodes.fetch.output}}" }
    fields:
      - { name: note, type: text, default: "{{nodes.fetch.output}}" }
  - { id: reader, type: agent_task, prompt: "read {{nodes.gate.output.note}}", accept_untrusted: "summarizing fetched text" }
  - { id: ok, type: finish, outcome: success }
edges:
  - { from: start, to: fetch }
  - { from: fetch, to: gate }
  - { from: gate, to: reader, condition: { type: review_status, equals: approve } }
  - { from: gate, to: ok, condition: { type: review_status, equals: reject } }
  - { from: reader, to: ok }
"#;

fn poll_until<T>(what: &str, mut f: impl FnMut() -> Option<T>) -> T {
    let started = Instant::now();
    loop {
        if let Some(value) = f() {
            return value;
        }
        if started.elapsed() > POLL_DEADLINE {
            panic!("timed out after {POLL_DEADLINE:?} waiting for {what}");
        }
        std::thread::sleep(POLL_STEP);
    }
}

fn seed(root: &Path, yaml: &str) {
    init_project(root).unwrap();
    let dir = root.join(".apb/playbooks/rform/1.0.0");
    fs::create_dir_all(dir.join("scripts")).unwrap();
    fs::write(dir.join("playbook.yaml"), yaml).unwrap();
    fs::write(dir.join("scripts/prep.sh"), PREP).unwrap();
    fs::write(root.join(".apb/playbooks/rform/current"), "1.0.0").unwrap();
}

fn run_in_background(root: &Path) -> mpsc::Receiver<RunResult> {
    run_in_background_with(root, RunOptions::default())
}

fn run_in_background_with(root: &Path, opts: RunOptions) -> mpsc::Receiver<RunResult> {
    let (tx, rx) = mpsc::channel();
    let root = root.to_path_buf();
    std::thread::spawn(move || {
        let res = run(&root, "rform", None, opts).unwrap();
        let _ = tx.send(res);
    });
    rx
}

fn latest_run_dir(root: &Path) -> PathBuf {
    poll_until("run dir to appear", || {
        let runs = root.join(".apb/runs");
        let entry = fs::read_dir(&runs)
            .ok()?
            .filter_map(|e| e.ok())
            .find(|e| e.path().is_dir())?;
        Some(entry.path())
    })
}

fn wait_result(rx: &mpsc::Receiver<RunResult>) -> RunResult {
    rx.recv_timeout(POLL_DEADLINE)
        .unwrap_or_else(|_| panic!("run did not finish within {POLL_DEADLINE:?}"))
}

fn run_id_of(run_dir: &Path) -> String {
    run_dir.file_name().unwrap().to_string_lossy().into_owned()
}

fn pending_of(run_dir: &Path, gate: &str) -> apb_engine::progress::PendingReview {
    poll_until("pending review", || {
        let view = RunView::load(run_dir, &run_id_of(run_dir)).ok()?;
        view.progress?.pending_review.filter(|p| p.node == gate)
    })
}

fn cmd(fields: &[(&str, serde_json::Value)]) -> apb_engine::review::DecisionPost {
    apb_engine::review::DecisionPost {
        cmd: apb_engine::ReviewCommand {
            node: "gate".into(),
            decision: "approve".into(),
            note: String::new(),
        },
        nonce: None,
        request: None,
        channel: DecisionChannel::Terminal,
        fields: fields
            .iter()
            .map(|(k, v)| (k.to_string(), v.clone()))
            .collect(),
        passkey: None,
    }
}

/// The values reach the next prompt and an `output_field` edge; the gate's
/// output is one JSON object; the request's `review_fields` is journaled.
/// Both outcomes are routed by `output_field` edges with no fallback, and
/// the decided amount (1180.5) sits on the other side of the threshold
/// from the default (900), so reaching `big` proves that an edge matched
/// and that it read the decided value.
#[test]
fn fields_reach_the_next_prompt_and_an_output_field_edge() {
    let tmp = tempfile::tempdir().unwrap();
    seed(tmp.path(), FIELDS_WF);
    let rx = run_in_background(tmp.path());
    let run_dir = latest_run_dir(tmp.path());
    let pending = pending_of(&run_dir, "gate");
    // The default rendered from the prep node's output.
    let amount = pending.fields.iter().find(|f| f.name == "amount").unwrap();
    assert_eq!(amount.default.as_ref(), Some(&serde_json::json!(900)));
    assert_eq!(amount.required, vec!["approve"]);
    // The show entry rendered and not marked.
    assert_eq!(pending.show[0].label, "Total");
    assert_eq!(pending.show[0].text, "900");
    assert!(!pending.show[0].untrusted);

    apb_engine::review::post(
        &run_dir,
        cmd(&[
            ("amount", serde_json::json!(1180.5)),
            ("remark", serde_json::json!("ok")),
        ]),
    )
    .unwrap();
    let result = wait_result(&rx);
    assert_eq!(
        result.outcome,
        RunStatus::Succeeded,
        "the decided 1180.5 routes to big, the default 900 would route to small"
    );
    let events = read_all(&run_dir).unwrap();
    // The gate's output is one JSON object: decision and every field.
    let gate_out = events
        .iter()
        .rev()
        .find_map(|e| match &e.payload {
            EventPayload::NodeFinished { node, output, .. } if node == "gate" => {
                Some(output.clone())
            }
            _ => None,
        })
        .unwrap();
    let object: serde_json::Value = serde_json::from_str(&gate_out).expect("JSON output");
    assert_eq!(object["decision"], "approve");
    assert_eq!(object["amount"], 1180.5);
    assert_eq!(object["remark"], "ok");
    // The next prompt read it.
    let echo_out = events
        .iter()
        .rev()
        .find_map(|e| match &e.payload {
            EventPayload::NodeFinished { node, output, .. } if node == "echo" => {
                Some(output.clone())
            }
            _ => None,
        })
        .unwrap();
    assert_eq!(echo_out, "the amount is 1180.5");
    // The values are journaled right before the decision.
    assert!(events.iter().any(
        |e| matches!(&e.payload, EventPayload::ReviewFields { node, fields, .. }
                if node == "gate" && fields["amount"] == 1180.5)
    ));
}

/// The channel refuses a value of the wrong type and an unknown field.
#[test]
fn a_value_of_the_wrong_type_is_refused_by_the_channel() {
    let tmp = tempfile::tempdir().unwrap();
    seed(tmp.path(), FIELDS_WF);
    let rx = run_in_background(tmp.path());
    let run_dir = latest_run_dir(tmp.path());
    let _ = pending_of(&run_dir, "gate");
    let refused = apb_engine::review::post(&run_dir, cmd(&[("amount", serde_json::json!("ten"))]))
        .unwrap_err();
    assert!(
        refused.to_string().contains("gate `gate`")
            && refused.to_string().contains("is not a number"),
        "{refused}"
    );
    let unknown =
        apb_engine::review::post(&run_dir, cmd(&[("total", serde_json::json!(1))])).unwrap_err();
    assert!(
        unknown
            .to_string()
            .contains("a posted value names no field of this gate"),
        "{unknown}"
    );
    // A required field without a value and without a default? The amount
    // default fills it: approve with no fields at all is accepted.
    apb_engine::review::post(&run_dir, cmd(&[])).unwrap();
    let result = wait_result(&rx);
    // The default 900 is the amount, and it routes to `small`.
    assert_eq!(result.outcome, RunStatus::Failed);
}

/// A hand-written channel line with bad field values ends the run
/// `review_fields_refused`.
#[test]
fn a_hand_written_entry_with_bad_fields_ends_the_run() {
    let tmp = tempfile::tempdir().unwrap();
    seed(tmp.path(), FIELDS_WF);
    let rx = run_in_background(tmp.path());
    let run_dir = latest_run_dir(tmp.path());
    let _ = pending_of(&run_dir, "gate");
    // A whole, well-formed channel line that the checks refuse.
    let line = serde_json::json!({
        "seq": 0,
        "node": "gate",
        "decision": "approve",
        "note": "",
        "fields": { "amount": "ten" }
    });
    use std::io::Write as _;
    let mut file = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(run_dir.join("reviews.jsonl"))
        .unwrap();
    writeln!(file, "{line}").unwrap();
    let result = wait_result(&rx);
    assert_eq!(result.outcome, RunStatus::Failed);
    let events = read_all(&run_dir).unwrap();
    let reason = events
        .iter()
        .rev()
        .find_map(|e| match &e.payload {
            EventPayload::RunError { reason, .. } => Some(reason.clone()),
            _ => None,
        })
        .unwrap();
    assert!(reason.contains("review_fields_refused"), "{reason}");
}

/// A secret param's value typed into a field is refused when it is
/// posted, on the field's name and without the value, before the channel
/// holds anything: no channel entry, journal event or gate output carries
/// it. A clean value is then taken.
#[test]
fn a_secret_value_in_a_field_is_refused_before_the_channel_holds_it() {
    let tmp = tempfile::tempdir().unwrap();
    seed(tmp.path(), SECRET_WF);
    let secret = "s3cret-token-value-9f2";
    let rx = run_in_background_with(
        tmp.path(),
        RunOptions {
            params: [("token".to_string(), secret.to_string())].into(),
            ..RunOptions::default()
        },
    );
    let run_dir = latest_run_dir(tmp.path());
    let _ = pending_of(&run_dir, "gate");
    let reviews = || fs::read_to_string(run_dir.join("reviews.jsonl")).unwrap_or_default();
    for (key, value) in [
        ("note", serde_json::json!(secret)),
        ("note", serde_json::json!(format!("see {secret} here"))),
        ("count", serde_json::json!(secret)),
    ] {
        let refused = apb_engine::review::post(&run_dir, cmd(&[(key, value)]))
            .unwrap_err()
            .to_string();
        assert!(
            refused.contains(&format!(
                "field `{key}` holds a value this run keeps secret"
            )),
            "{refused}"
        );
        assert!(!refused.contains(secret), "{refused}");
        assert!(!reviews().contains(secret), "the channel holds the secret");
    }
    // A key the gate does not declare is not repeated either.
    let refused = apb_engine::review::post(&run_dir, cmd(&[(secret, serde_json::json!(secret))]))
        .unwrap_err()
        .to_string();
    assert!(
        refused.contains("a posted value holds a value this run keeps secret"),
        "{refused}"
    );
    assert!(
        !refused.contains(secret) && !reviews().contains(secret),
        "{refused}"
    );
    apb_engine::review::post(&run_dir, cmd(&[("note", serde_json::json!("all fine"))])).unwrap();
    let result = wait_result(&rx);
    assert_eq!(result.outcome, RunStatus::Succeeded);
    let raw = fs::read_to_string(run_dir.join("events.jsonl")).unwrap();
    assert!(!raw.contains(secret), "the journal carries the secret");
    let events = read_all(&run_dir).unwrap();
    let gate_out = events
        .iter()
        .rev()
        .find_map(|e| match &e.payload {
            EventPayload::NodeFinished { node, output, .. } if node == "gate" => {
                Some(output.clone())
            }
            _ => None,
        })
        .unwrap();
    assert!(!gate_out.contains(secret), "the output carries the secret");
}

/// While the run's secret values cannot be read, a text value is refused
/// when it is posted, the way the run redactor withholds text: it could
/// hold a value the run keeps secret. Once the values read again, the
/// same text is taken.
#[test]
fn a_text_value_is_refused_while_the_runs_secrets_do_not_read() {
    let tmp = tempfile::tempdir().unwrap();
    seed(tmp.path(), SECRET_WF);
    let rx = run_in_background_with(
        tmp.path(),
        RunOptions {
            params: [("token".to_string(), "s3cret-token-value-9f2".to_string())].into(),
            ..RunOptions::default()
        },
    );
    let run_dir = latest_run_dir(tmp.path());
    let _ = pending_of(&run_dir, "gate");
    let config = run_dir.join("run.yaml");
    let kept = fs::read(&config).unwrap();
    fs::write(&config, "params: [unclosed\n").unwrap();
    let refused =
        apb_engine::review::post(&run_dir, cmd(&[("note", serde_json::json!("plain words"))]))
            .unwrap_err()
            .to_string();
    assert!(
        refused.contains("field `note`: this run's secret values cannot be read"),
        "{refused}"
    );
    assert!(!refused.contains("plain words"), "{refused}");
    let reviews = fs::read_to_string(run_dir.join("reviews.jsonl")).unwrap_or_default();
    assert!(!reviews.contains("plain words"), "{reviews}");
    fs::write(&config, kept).unwrap();
    apb_engine::review::post(&run_dir, cmd(&[("note", serde_json::json!("plain words"))])).unwrap();
    assert_eq!(wait_result(&rx).outcome, RunStatus::Succeeded);
}

/// A gate with a small context budget shows an earlier output that holds
/// a secret: the text is scrubbed whole before any cut, so no part of the
/// secret reaches the request's detail or the pending review.
#[test]
fn a_secret_is_scrubbed_before_the_shown_text_is_cut() {
    let tmp = tempfile::tempdir().unwrap();
    let secret = "s3cret-token-value-9f2";
    let yaml = r#"schema: 2
id: rform
name: RForm
version: 1.0.0
params:
  - { name: token, type: text, secret: true }
nodes:
  - { id: start, type: start }
  - { id: prep, type: script, script: "scripts/prep.sh", runner: sh }
  - id: gate
    type: human_review
    options: [approve, reject]
    context: { output_max_bytes: 8 }
    show:
      - { label: Out, text: "{{nodes.prep.output}}" }
  - { id: ok, type: finish, outcome: success }
edges:
  - { from: start, to: prep }
  - { from: prep, to: gate }
  - { from: gate, to: ok, condition: { type: review_status, equals: approve } }
"#;
    seed(tmp.path(), yaml);
    fs::write(
        tmp.path()
            .join(".apb/playbooks/rform/1.0.0/scripts/prep.sh"),
        format!("printf '{secret}'\n"),
    )
    .unwrap();
    let rx = run_in_background_with(
        tmp.path(),
        RunOptions {
            params: [("token".to_string(), secret.to_string())].into(),
            ..RunOptions::default()
        },
    );
    let run_dir = latest_run_dir(tmp.path());
    let pending = pending_of(&run_dir, "gate");
    let shown = &pending.show[0].text;
    assert!(
        !shown.contains(&secret[..8]),
        "a part of the secret is shown: {shown}"
    );
    let raw = fs::read_to_string(run_dir.join("events.jsonl")).unwrap();
    // The step's own output is journaled as it printed it; the request's
    // detail must hold no part of the secret.
    let hits: Vec<&str> = raw
        .lines()
        .filter(|l| l.contains("\"review_detail\"") && l.contains(&secret[..8]))
        .collect();
    assert!(
        hits.is_empty(),
        "the review detail carries a part of the secret: {hits:?}"
    );
    apb_engine::review::post(&run_dir, cmd(&[])).unwrap();
    assert_eq!(wait_result(&rx).outcome, RunStatus::Succeeded);
}

/// A literal default, a show label and an enum option go through the
/// same scrub and cut as a rendered text: a token-shaped literal never
/// reaches the request's detail, and a literal default past the byte cap
/// is cut like a rendered one.
#[test]
fn literal_texts_are_scrubbed_and_cut_like_rendered_ones() {
    let tmp = tempfile::tempdir().unwrap();
    let token = "ghp_0123456789abcdefABCDEF0123456789abcd";
    let long = "a".repeat(3000);
    let yaml = format!(
        r#"schema: 2
id: rform
name: RForm
version: 1.0.0
nodes:
  - {{ id: start, type: start }}
  - id: gate
    type: human_review
    options: [approve, reject]
    show:
      - {{ label: "key {token}", text: "plain" }}
    fields:
      - {{ name: note, type: text, default: "token {token}" }}
      - {{ name: essay, type: text, max_len: 5000, default: "{long}" }}
      - {{ name: pick, type: enum, options: [plain, "{token}"], default: "{token}" }}
  - {{ id: ok, type: finish, outcome: success }}
edges:
  - {{ from: start, to: gate }}
  - {{ from: gate, to: ok, condition: {{ type: review_status, equals: approve }} }}
"#
    );
    seed(tmp.path(), &yaml);
    let rx = run_in_background(tmp.path());
    let run_dir = latest_run_dir(tmp.path());
    let pending = pending_of(&run_dir, "gate");
    assert!(!pending.show[0].label.contains(token), "{:?}", pending.show);
    let note = pending.fields.iter().find(|f| f.name == "note").unwrap();
    let note_default = note.default.as_ref().and_then(|v| v.as_str()).unwrap();
    assert!(!note_default.contains(token), "{note_default}");
    assert!(note_default.starts_with("token "), "{note_default}");
    let essay = pending.fields.iter().find(|f| f.name == "essay").unwrap();
    let essay_default = essay.default.as_ref().and_then(|v| v.as_str()).unwrap();
    assert_eq!(
        essay_default.len(),
        apb_core::review_form::RENDERED_MAX_BYTES,
        "a literal default is cut at the same cap"
    );
    let pick = pending.fields.iter().find(|f| f.name == "pick").unwrap();
    assert!(
        pick.options.iter().all(|o| !o.contains(token)),
        "{:?}",
        pick.options
    );
    // The problem of a default that no longer fits names the options as
    // the detail shows them, scrubbed.
    let problem = pick.default_problem.as_deref().unwrap_or_default();
    assert!(!problem.contains(token), "{problem}");
    let raw = fs::read_to_string(run_dir.join("events.jsonl")).unwrap();
    let hits: Vec<&str> = raw
        .lines()
        .filter(|l| l.contains("\"review_detail\"") && l.contains(token))
        .collect();
    assert!(
        hits.is_empty(),
        "the review detail carries the token: {hits:?}"
    );
    apb_engine::review::post(&run_dir, cmd(&[])).unwrap();
    assert_eq!(wait_result(&rx).outcome, RunStatus::Succeeded);
}

/// No refusal the channel returns carries definition text or a posted
/// value unscrubbed: an enum's token-shaped option, a token posted as a
/// key or as a value, a token-shaped gate id.
#[test]
fn a_channel_refusal_carries_no_raw_definition_text_or_posted_value() {
    let tmp = tempfile::tempdir().unwrap();
    let token = "ghp_0123456789abcdefABCDEF0123456789abcd";
    let yaml = format!(
        r#"schema: 2
id: rform
name: RForm
version: 1.0.0
nodes:
  - {{ id: start, type: start }}
  - id: gate
    type: human_review
    options: [approve, reject]
    fields:
      - {{ name: pick, type: enum, options: [plain, "{token}"] }}
      - {{ name: count, type: number }}
  - {{ id: ok, type: finish, outcome: success }}
edges:
  - {{ from: start, to: gate }}
  - {{ from: gate, to: ok }}
"#
    );
    seed(tmp.path(), &yaml);
    let rx = run_in_background(tmp.path());
    let run_dir = latest_run_dir(tmp.path());
    let _ = pending_of(&run_dir, "gate");
    let posts = [
        cmd(&[("pick", serde_json::json!("other"))]),
        cmd(&[("pick", serde_json::json!(token.to_uppercase()))]),
        cmd(&[(token, serde_json::json!(1))]),
        cmd(&[("count", serde_json::json!(token))]),
    ];
    for post in posts {
        let refused = apb_engine::review::post(&run_dir, post)
            .unwrap_err()
            .to_string();
        assert!(
            !refused.contains(token)
                && !refused.contains(&token.to_uppercase())
                && !refused.contains("plain"),
            "{refused}"
        );
        assert!(refused.contains("gate `gate`"), "{refused}");
    }
    let enum_refusal = apb_engine::review::post(&run_dir, cmd(&[("pick", serde_json::json!("x"))]))
        .unwrap_err()
        .to_string();
    assert!(
        enum_refusal.contains("field `pick`: is not one of the field's options"),
        "{enum_refusal}"
    );
    apb_engine::review::post(&run_dir, cmd(&[("pick", serde_json::json!("plain"))])).unwrap();
    assert_eq!(wait_result(&rx).outcome, RunStatus::Succeeded);
}

/// A hand-written channel line whose number field carries a secret
/// param's value ends the run `review_fields_refused`, and neither the
/// refusal nor anything else in the journal holds the value.
#[test]
fn a_refused_value_is_not_journaled() {
    let tmp = tempfile::tempdir().unwrap();
    seed(tmp.path(), SECRET_WF);
    let secret = "s3cret-token-value-9f2";
    let rx = run_in_background_with(
        tmp.path(),
        RunOptions {
            params: [("token".to_string(), secret.to_string())].into(),
            ..RunOptions::default()
        },
    );
    let run_dir = latest_run_dir(tmp.path());
    let _ = pending_of(&run_dir, "gate");
    let line = serde_json::json!({
        "seq": 0,
        "node": "gate",
        "decision": "approve",
        "note": "",
        "fields": { "count": secret }
    });
    use std::io::Write as _;
    let mut file = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(run_dir.join("reviews.jsonl"))
        .unwrap();
    writeln!(file, "{line}").unwrap();
    let result = wait_result(&rx);
    assert_eq!(result.outcome, RunStatus::Failed);
    let events = read_all(&run_dir).unwrap();
    let reason = events
        .iter()
        .rev()
        .find_map(|e| match &e.payload {
            EventPayload::RunError { reason, .. } => Some(reason.clone()),
            _ => None,
        })
        .unwrap();
    assert!(reason.contains("review_fields_refused"), "{reason}");
    assert!(reason.contains("field `count`"), "{reason}");
    let raw = fs::read_to_string(run_dir.join("events.jsonl")).unwrap();
    assert!(
        !raw.contains(secret),
        "the journal carries the refused value"
    );
}

/// A default over untrusted text is marked, warns (V247), and taints the
/// gate's output for its readers.
#[test]
fn an_untrusted_default_is_marked_and_taints_readers() {
    let tmp = tempfile::tempdir().unwrap();
    seed(tmp.path(), UNTRUSTED_WF);
    // The validator warns about the default.
    let playbook = apb_core::schema::Playbook::from_yaml(UNTRUSTED_WF).unwrap();
    let report =
        apb_core::validate::validate(&playbook, &apb_core::validate::ValidationContext::default());
    assert!(
        report
            .issues
            .iter()
            .any(|i| i.code == "V247" && i.message.contains("field `note`")),
        "{:?}",
        report.issues
    );
    // An agent reading the gate's output is a follower.
    let analysis = apb_core::untrusted::analyze(&playbook);
    assert!(
        analysis.followers.iter().any(|f| f.node == "reader"),
        "{:?}",
        analysis.followers
    );

    // The run marks the field view untrusted. The stub agent stands in for
    // the reader after the gate.
    let _lock = common::env_lock();
    let _agent = common::EnvVar::set("APB_AGENT_CMD", stub(tmp.path()));
    let _backoff = common::EnvVar::set(apb_engine::failure_class::BACKOFF_ENV, "1,1");
    common::seed_profile(tmp.path(), "main", "claude-code", "haiku", &[]);
    let rx = run_in_background_with(
        tmp.path(),
        RunOptions {
            params: [("body".to_string(), "outside text".to_string())].into(),
            ..RunOptions::default()
        },
    );
    let run_dir = latest_run_dir(tmp.path());
    let pending = pending_of(&run_dir, "gate");
    let note = pending.fields.iter().find(|f| f.name == "note").unwrap();
    assert!(note.untrusted, "{note:?}");
    // The shown entry over the same output is marked too.
    assert!(pending.show[0].untrusted, "{:?}", pending.show);
    apb_engine::review::post(&run_dir, cmd(&[("note", serde_json::json!("mine"))])).unwrap();
    let result = wait_result(&rx);
    assert_eq!(result.outcome, RunStatus::Succeeded);
}

/// `required: [approve]` without a default, through the channel: an
/// approve without a value is refused, a reject without one is accepted.
#[test]
fn a_value_required_for_approve_only_is_checked_by_the_channel() {
    let tmp = tempfile::tempdir().unwrap();
    seed(
        tmp.path(),
        &FIELDS_WF.replace(", default: \"{{nodes.prep.output}}\"", ""),
    );
    let rx = run_in_background(tmp.path());
    let run_dir = latest_run_dir(tmp.path());
    let pending = pending_of(&run_dir, "gate");
    assert!(pending.fields.iter().all(|f| f.default.is_none()));
    let refused = apb_engine::review::post(&run_dir, cmd(&[])).unwrap_err();
    assert!(
        refused
            .to_string()
            .contains("field `amount` is required for this decision"),
        "{refused}"
    );
    let mut reject = cmd(&[]);
    reject.cmd.decision = "reject".into();
    apb_engine::review::post(&run_dir, reject).unwrap();
    // No amount: neither output_field edge matches.
    assert_eq!(wait_result(&rx).outcome, RunStatus::Failed);
}

/// A passkey minted over the bare decision is not consumed when the
/// decision carries field values; minted over the values it is.
#[test]
fn a_passkey_minted_without_the_fields_is_not_consumed() {
    let _guard = common::env_lock();
    let cfg = tempfile::tempdir().unwrap();
    let signing = enroll_real_passkey(&cfg.path().join("passkeys.yaml"));
    let _var = common::EnvVar::set("APB_CONFIG_DIR", cfg.path());
    let tmp = tempfile::tempdir().unwrap();
    // A person-only gate: with a passkey enrolled it decides only with an
    // assertion.
    let yaml = FIELDS_WF.replace(
        "options: [approve, reject]\n    show:",
        "options: [approve, reject]\n    person_only: true\n    show:",
    );
    seed(tmp.path(), &yaml);
    let rx = run_in_background(tmp.path());
    let run_dir = latest_run_dir(tmp.path());
    let pending = pending_of(&run_dir, "gate");
    assert!(pending.person_only, "{pending:?}");
    let run_id = run_id_of(&run_dir);
    let request = pending.requested_at.map(|_| ());
    let _ = request;
    let seq = read_all(&run_dir)
        .unwrap()
        .iter()
        .rev()
        .find_map(|e| match &e.payload {
            EventPayload::ReviewRequested { node, .. } if node == "gate" => Some(e.seq),
            _ => None,
        })
        .unwrap();
    // Minted over the bare decision word only.
    let wrong = proof_for_material(&signing, &run_id, "gate", "approve", Some(seq), 1);
    apb_engine::review::post(
        &run_dir,
        apb_engine::review::DecisionPost {
            cmd: apb_engine::ReviewCommand {
                node: "gate".into(),
                decision: "approve".into(),
                note: "minted without the fields".into(),
            },
            nonce: None,
            request: Some(seq),
            channel: DecisionChannel::DashboardSession,
            fields: [("amount".to_string(), serde_json::json!(1180.5))].into(),
            passkey: Some((wrong.credential_id[..8].to_string(), wrong)),
        },
    )
    .unwrap();
    // Minted over the decision and its values. The drive takes the first
    // entry that answers the request, so a consumed wrong entry would show
    // as its note on the decision.
    let fields = [("amount".to_string(), serde_json::json!(1180.5))].into();
    let right = proof_for_fields(&signing, &run_id, "gate", "approve", &fields, Some(seq), 2);
    apb_engine::review::post(
        &run_dir,
        apb_engine::review::DecisionPost {
            cmd: apb_engine::ReviewCommand {
                node: "gate".into(),
                decision: "approve".into(),
                note: "minted over the fields".into(),
            },
            nonce: None,
            request: Some(seq),
            channel: DecisionChannel::DashboardSession,
            fields,
            passkey: Some((right.credential_id[..8].to_string(), right)),
        },
    )
    .unwrap();
    let result = wait_result(&rx);
    assert_eq!(result.outcome, RunStatus::Succeeded);
    let decided: Vec<String> = read_all(&run_dir)
        .unwrap()
        .iter()
        .filter_map(|e| match &e.payload {
            EventPayload::ReviewDecided { node, note, .. } if node == "gate" => Some(note.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(
        decided,
        vec!["minted over the fields"],
        "a stamp over the bare decision must not decide"
    );
}

/// A gate without fields keeps the decision word as its output and
/// journals no `review_fields`.
#[test]
fn a_gate_without_fields_keeps_the_decision_word() {
    let tmp = tempfile::tempdir().unwrap();
    seed(tmp.path(), PLAIN_WF);
    let rx = run_in_background(tmp.path());
    let run_dir = latest_run_dir(tmp.path());
    let _ = pending_of(&run_dir, "gate");
    apb_engine::review::post(&run_dir, cmd(&[])).unwrap();
    let result = wait_result(&rx);
    assert_eq!(result.outcome, RunStatus::Succeeded);
    let events = read_all(&run_dir).unwrap();
    let gate_out = events
        .iter()
        .rev()
        .find_map(|e| match &e.payload {
            EventPayload::NodeFinished { node, output, .. } if node == "gate" => {
                Some(output.clone())
            }
            _ => None,
        })
        .unwrap();
    assert_eq!(gate_out, "approve");
    assert!(
        !events
            .iter()
            .any(|e| matches!(&e.payload, EventPayload::ReviewFields { .. }))
    );
}

// --- the enrolled-passkey setup of passkey_gate_test.rs (private there) --

const KEY_SEED: [u8; 32] = *b"0123456789abcdef0123456789abcdef";

fn enroll_real_passkey(file: &Path) -> p256::ecdsa::SigningKey {
    let signing = p256::ecdsa::SigningKey::from_bytes(&KEY_SEED.into()).unwrap();
    let vk = signing.verifying_key();
    let point = vk.to_sec1_point(false);
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

fn proof_for_fields(
    signing: &p256::ecdsa::SigningKey,
    run_id: &str,
    node: &str,
    decision: &str,
    fields: &std::collections::BTreeMap<String, serde_json::Value>,
    request: Option<u64>,
    sign_count: u32,
) -> passkey_proof::PasskeyProof {
    let material = apb_core::review_form::decision_material(decision, fields);
    proof_for_material(signing, run_id, node, &material, request, sign_count)
}

fn proof_for_material(
    signing: &p256::ecdsa::SigningKey,
    run_id: &str,
    node: &str,
    decision: &str,
    request: Option<u64>,
    sign_count: u32,
) -> passkey_proof::PasskeyProof {
    let challenge = passkey_proof::decision_challenge(run_id, node, decision, request, None);
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
    passkey_proof::PasskeyProof {
        credential_id: credential_id.clone(),
        client_data_json: b64url(client.as_bytes()),
        authenticator_data: b64url(&data),
        signature: b64url(signature.as_bytes()),
    }
}

/// The stub agent of list_run_test: reports success.
fn stub(root: &Path) -> String {
    let d = root.join("stub");
    fs::create_dir_all(&d).unwrap();
    let body = r#"#!/bin/sh
printf '{"status":"success","outputs":{}}' > "$APB_STATUS_FILE"
printf '\n```yaml\nstatus: success\nsummary: ok\n```\n'
"#;
    let path = root.join("agent.sh");
    common::write_sync(&path, body);
    let mut p = fs::metadata(&path).unwrap().permissions();
    p.set_mode(0o755);
    fs::set_permissions(&path, p).unwrap();
    path.to_string_lossy().into_owned()
}

/// The `type` of every event of a run's journal, in order.
fn event_types(run_dir: &Path) -> Vec<String> {
    fs::read_to_string(run_dir.join("events.jsonl"))
        .unwrap()
        .lines()
        .map(|l| {
            serde_json::from_str::<serde_json::Value>(l).unwrap()["type"]
                .as_str()
                .unwrap()
                .to_string()
        })
        .collect()
}

/// A gate that uses no form feature writes the journal an apb before
/// this change writes: while the run waits there, `review_requested` is
/// the last line, no `review_detail` beside it, so an older apb still
/// resumes the run.
#[test]
fn a_plain_gate_journals_its_request_alone() {
    let tmp = tempfile::tempdir().unwrap();
    seed(tmp.path(), PLAIN_WF);
    let rx = run_in_background(tmp.path());
    let run_dir = latest_run_dir(tmp.path());
    let _ = pending_of(&run_dir, "gate");
    let waiting = event_types(&run_dir);
    assert_eq!(
        waiting.last().map(String::as_str),
        Some("review_requested"),
        "{waiting:?}"
    );
    assert!(!waiting.iter().any(|t| t == "review_detail"), "{waiting:?}");
    apb_engine::review::post(&run_dir, cmd(&[])).unwrap();
    assert_eq!(wait_result(&rx).outcome, RunStatus::Succeeded);
    let all = event_types(&run_dir);
    assert!(
        !all.iter()
            .any(|t| t == "review_detail" || t == "review_fields"),
        "{all:?}"
    );
    // The decision follows the request with nothing of this change between.
    let at = all.iter().position(|t| t == "review_requested").unwrap();
    assert_eq!(all[at + 1], "node_started", "{all:?}");
    assert_eq!(all[at + 2], "review_decided", "{all:?}");
}

/// A gate that uses a form feature journals its request with its detail;
/// when the detail is missing (a write torn between the two lines, not
/// yet repaired by a drive) the channel fails closed: a decision is
/// refused, never taken as one for a gate without the feature.
#[test]
fn a_form_gate_without_its_detail_fails_closed() {
    let tmp = tempfile::tempdir().unwrap();
    seed(tmp.path(), FIELDS_WF);
    let rx = run_in_background(tmp.path());
    let run_dir = latest_run_dir(tmp.path());
    let _ = pending_of(&run_dir, "gate");
    let types = event_types(&run_dir);
    let at = types.iter().rposition(|t| t == "review_requested").unwrap();
    assert_eq!(types.get(at + 1).map(String::as_str), Some("review_detail"));
    // A copy of the waiting run, no drive behind it, with the detail cut
    // off its journal.
    let copy = run_dir.with_file_name(format!("{}-torn", run_id_of(&run_dir)));
    let status = std::process::Command::new("cp")
        .arg("-a")
        .arg(&run_dir)
        .arg(&copy)
        .status()
        .unwrap();
    assert!(status.success());
    let journal = fs::read_to_string(copy.join("events.jsonl")).unwrap();
    let kept: Vec<&str> = journal
        .lines()
        .filter(|l| !l.contains("\"review_detail\""))
        .collect();
    assert_eq!(kept.len() + 1, journal.lines().count());
    fs::write(copy.join("events.jsonl"), format!("{}\n", kept.join("\n"))).unwrap();
    let refused = apb_engine::review::post(&copy, cmd(&[])).unwrap_err();
    assert!(
        refused.to_string().contains("is not recorded yet"),
        "{refused}"
    );
    // A run whose definition does not read takes no field values: they
    // could not be checked, and the drive would refuse them later.
    fs::remove_file(copy.join("playbook.yaml")).unwrap();
    let unread =
        apb_engine::review::post(&copy, cmd(&[("remark", serde_json::json!("ok"))])).unwrap_err();
    assert!(
        unread.to_string().contains("cannot be read right now"),
        "{unread}"
    );
    // The live run, with its detail, takes the same decision.
    apb_engine::review::post(&run_dir, cmd(&[])).unwrap();
    let _ = wait_result(&rx);
}
