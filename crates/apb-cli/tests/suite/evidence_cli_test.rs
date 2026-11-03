//! `apb evidence read` end to end: a stand-in agent reads its sources
//! through the real binary during its attempt and writes claims; the run's
//! results show each claim's verdict with the fixed wording, its flags and
//! the cited fragment. `apb evidence read` and MCP `run_evidence_read`
//! answer the same and charge the same budget, and both refuse a session
//! whose attempt ended.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use apb_engine::evidence::session::{self, Opening};
use serde_json::{Value, json};

use crate::mcp_cli_test::{request, start_mcp};

const PLAYBOOK: &str = r#"schema: 2
id: routine
name: Routine
version: 1.0.0
defaults: { profile: main }
nodes:
  - { id: start, type: start }
  - { id: collect, type: script, script: scripts/collect.sh, runner: sh, results: [ { path: mail.jsonl, format: records } ] }
  - { id: read, type: agent_task, prompt: "Summarize the mail.", evidence: { from: [collect] } }
  - { id: done, type: finish, outcome: success }
edges:
  - { from: start, to: collect }
  - { from: collect, to: read }
  - { from: read, to: done }
"#;

const MAIL: &str = r#"{"apb_records": 1, "complete": true, "source": "inbox"}
{"id": "m1", "kind": "mail", "text": "The invoice for March was paid on time by Acme."}
{"id": "t1", "kind": "ticket", "text": "The printer on floor two is jammed again."}
"#;

/// The agent: reads the outline, lists the package, reads `m1` whole, only
/// finds `t1` by a search, and writes three claims.
fn stub(root: &Path) -> PathBuf {
    let apb = crate::common::apb_bin().display().to_string();
    let d = root.join("agent-files");
    fs::create_dir_all(&d).unwrap();
    let d = d.display();
    let body = format!(
        r#"#!/bin/sh
[ "$APB_NODE_ID" = read ] || exit 1
A='{apb}'
D='{d}'
"$A" evidence read outline > "$D/outline.json" || exit 1
view=$(grep -o '"n:[^"]*mail\.jsonl"' "$D/outline.json" | head -1 | tr -d '"' | cut -c3-)
"$A" evidence read children --view "$view" > "$D/children.json" || exit 1
m1=$(grep -o '"r:[^"]*@m1"' "$D/children.json" | head -1 | tr -d '"')
t1=$(grep -o '"r:[^"]*@t1"' "$D/children.json" | head -1 | tr -d '"')
"$A" evidence read read --ref "$m1" > "$D/read.json" || exit 1
"$A" evidence read search --phrase printer > "$D/search.json" || exit 1
printf '{{"status":"success","outputs":{{"claims":[{{"id":"c1","text":"The March invoice was paid on time.","cites":[{{"ref":"%s","span":[4,30]}}]}},{{"id":"c2","text":"The invoice was settled.","cites":[{{"ref":"%s","span":[4,11]}}]}},{{"id":"c3","text":"The printer is jammed.","cites":[{{"ref":"%s"}}]}}]}}}}' "$m1" "$m1" "$t1" > "$APB_STATUS_FILE"
printf '\n```yaml\nstatus: success\nsummary: ok\n```\n'
"#
    );
    let path = root.join("agent.sh");
    {
        use std::io::Write;
        let mut f = fs::File::create(&path).unwrap();
        f.write_all(body.as_bytes()).unwrap();
        f.sync_all().unwrap();
    }
    fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
    path
}

/// A finished run of [`PLAYBOOK`] under the agent; the run id.
fn routine(root: &Path) -> String {
    let out = crate::common::apb_std()
        .arg("init")
        .current_dir(root)
        .output()
        .unwrap();
    assert!(out.status.success(), "{out:?}");
    let v = root.join(".apb/playbooks/routine/1.0.0");
    fs::create_dir_all(v.join("scripts")).unwrap();
    fs::write(v.join("playbook.yaml"), PLAYBOOK).unwrap();
    fs::write(
        v.join("scripts/collect.sh"),
        format!("cat > mail.jsonl <<'APB_EOF'\n{MAIL}APB_EOF\necho ok\n"),
    )
    .unwrap();
    fs::write(root.join(".apb/playbooks/routine/current"), "1.0.0").unwrap();
    let p = root.join(".apb/profiles/main");
    fs::create_dir_all(&p).unwrap();
    fs::write(
        p.join("profile.yaml"),
        "name: main\ndescription: test\nexecutor:\n  agent: claude-code\n  model: haiku\n",
    )
    .unwrap();
    fs::write(p.join("SOUL.md"), "").unwrap();
    let agent = stub(root);
    let out = crate::common::apb_std()
        .args(["run", "routine"])
        .current_dir(root)
        .env("APB_AGENT_CMD", &agent)
        .output()
        .unwrap();
    assert!(out.status.success(), "{out:?}");
    fs::read_dir(root.join(".apb/runs"))
        .unwrap()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .find(|n| n.starts_with("routine"))
        .unwrap()
}

fn results_json(root: &Path, run: &str) -> Value {
    let out = crate::common::apb_std()
        .args(["results", run, "--json"])
        .current_dir(root)
        .output()
        .unwrap();
    assert!(out.status.success(), "{out:?}");
    serde_json::from_slice(&out.stdout).unwrap()
}

/// The routine of the design: one claim structurally valid with the fixed
/// wording and its fragment, one flagged `short_span`, one unsupported
/// because its record was only found by a search.
#[test]
fn the_routine_reads_through_the_cli_and_its_claims_get_verdicts() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let run = routine(root);
    let read: Value =
        serde_json::from_slice(&fs::read(root.join("agent-files/read.json")).unwrap()).unwrap();
    assert_eq!(read["items"][0]["citable"], true);
    let v = results_json(root, &run);
    let row = &v["evidence"][0];
    assert_eq!(row["node"], "read");
    assert_eq!(row["closed"], true);
    let claim = |id: &str| -> Value {
        row["claims"]
            .as_array()
            .unwrap()
            .iter()
            .find(|c| c["id"] == id)
            .unwrap()
            .clone()
    };
    let c1 = claim("c1");
    assert_eq!(c1["verdict"], "structurally_valid", "{v}");
    assert_eq!(c1["cites"][0]["fragment"], "invoice for March was paid");
    assert_eq!(
        c1["wording"],
        apb_engine::evidence::STRUCTURALLY_VALID_WORDING
    );
    assert_eq!(claim("c2")["flags"], json!(["short_span"]));
    let c3 = claim("c3");
    assert_eq!(c3["verdict"], "unsupported");
    assert!(
        c3["problems"][0]
            .as_str()
            .unwrap()
            .contains("only seen in a listing or a search"),
        "{c3}"
    );
    let out = crate::common::apb_std()
        .args(["results", &run])
        .current_dir(root)
        .output()
        .unwrap();
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(
        text.contains(apb_engine::evidence::STRUCTURALLY_VALID_WORDING),
        "{text}"
    );
    assert!(text.contains(apb_engine::evidence::UNSUPPORTED_WORDING));
    assert!(text.contains("Worth a look: short_span"), "{text}");
    assert!(text.contains("\"invoice for March was paid\""), "{text}");
}

/// The text of a tool answer over MCP stdio, and whether it is an error.
fn tool_call(
    stdin: &mut std::process::ChildStdin,
    rx: &std::sync::mpsc::Receiver<String>,
    id: u32,
    args: Value,
) -> (bool, String) {
    let body = format!(
        r#""method":"tools/call","params":{}"#,
        json!({"name": "run_evidence_read", "arguments": args})
    );
    let line = request(stdin, rx, id, &body);
    let v: Value = serde_json::from_str(&line).unwrap();
    let result = &v["result"];
    (
        result["isError"].as_bool().unwrap_or(false),
        result["content"][0]["text"]
            .as_str()
            .unwrap_or_default()
            .to_string(),
    )
}

#[test]
fn cli_and_mcp_reads_answer_the_same_and_a_closed_session_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let run = routine(root);
    let rd = root.join(".apb/runs").join(&run);
    // A session as the engine opens one for an attempt.
    let spec = apb_core::schema::Evidence {
        from: vec!["collect".into()],
        kinds: None,
        claims: Default::default(),
        on_invalid: Default::default(),
        budget_chars: Some(5000),
    };
    let s = session::open(
        &rd,
        &Opening {
            run_id: &run,
            node: "read",
            item: None,
            attempt_number: 9,
            stem: "read-9".into(),
            spec: &spec,
        },
    )
    .unwrap();
    let m1 = session::load(&rd, &s)
        .unwrap()
        .views
        .iter()
        .find(|v| v.kind == "records")
        .unwrap()
        .records[0]
        .r#ref
        .clone();
    let cli = crate::common::apb_std()
        .args(["evidence", "read", "--session", &s, "read", "--ref", &m1])
        .current_dir(root)
        .output()
        .unwrap();
    assert!(cli.status.success(), "{cli:?}");
    let a: Value = serde_json::from_slice(&cli.stdout).unwrap();
    let (mut child, mut stdin, rx) = start_mcp(root, &[]);
    let (err, text) = tool_call(
        &mut stdin,
        &rx,
        2,
        json!({"session": s, "op": "read", "ref": m1}),
    );
    assert!(!err, "{text}");
    let b: Value = serde_json::from_str(&text).unwrap();
    assert_eq!(a["items"], b["items"]);
    assert_eq!(a["budget_chars"], 5000);
    assert_eq!(a["budget_chars"], b["budget_chars"]);
    let charged = a["read_chars"].as_u64().unwrap();
    assert!(charged > 0);
    assert_eq!(b["read_chars"].as_u64().unwrap(), 2 * charged);

    session::close(&rd, &s).unwrap();
    let refused = crate::common::apb_std()
        .args(["evidence", "read", "--session", &s, "outline"])
        .current_dir(root)
        .output()
        .unwrap();
    assert_eq!(refused.status.code(), Some(3), "{refused:?}");
    assert!(String::from_utf8_lossy(&refused.stderr).contains("is closed"));
    let (err, text) = tool_call(&mut stdin, &rx, 3, json!({"session": s, "op": "outline"}));
    assert!(err && text.contains("is closed"), "{text}");
    let _ = child.kill();
    let _ = child.wait();
}
