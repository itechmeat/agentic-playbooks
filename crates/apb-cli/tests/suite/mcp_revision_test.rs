//! Wire tests of `apb mcp` per MCP revision (design D6 item 3).
//!
//! They speak an old revision (`initialize`) and the 2026-07-28 revision
//! (`server/discover` with per-request `_meta`) to the same binary, and pin
//! what an old host relies on in a fixture: the envelopes, the sorted tool
//! names and the definitions of two named tools only, never the whole tool
//! list. A tool added or removed changes only the name list; re-record the
//! fixture with `APB_RECORD_MCP_FIXTURE=1` in the change that adds or removes
//! it, never to hide an envelope difference.

use std::collections::BTreeSet;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Stdio};
use std::sync::mpsc;
use std::time::Duration;

use serde_json::{Value, json};

/// The two tools whose full definitions the fixture pins.
const PINNED_TOOLS: [&str; 2] = ["playbook_catalog", "run_status"];

/// The `_meta` every 2026-07-28 request carries.
const META_2026: &str = r#"{"io.modelcontextprotocol/protocolVersion":"2026-07-28","io.modelcontextprotocol/clientCapabilities":{}}"#;

/// Spawns `apb mcp` in `dir` with extra env and no handshake: the test sends
/// its own first message. Response lines arrive on the returned channel.
fn start_mcp_raw(dir: &Path, env: &[(&str, &str)]) -> (Child, ChildStdin, mpsc::Receiver<String>) {
    let mut cmd = crate::common::apb_std();
    cmd.arg("mcp")
        .current_dir(dir)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    for (k, v) in env {
        cmd.env(k, v);
    }
    let mut child = cmd.spawn().unwrap();
    let stdin = child.stdin.take().unwrap();
    let stdout = child.stdout.take().unwrap();
    let (tx, rx) = mpsc::channel::<String>();
    std::thread::spawn(move || {
        let mut reader = BufReader::new(stdout);
        let mut line = String::new();
        loop {
            line.clear();
            match reader.read_line(&mut line) {
                Ok(0) | Err(_) => break,
                Ok(_) => {
                    if tx.send(line.clone()).is_err() {
                        break;
                    }
                }
            }
        }
    });
    (child, stdin, rx)
}

/// A project directory `apb init` prepared.
fn project() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    crate::common::apb_std()
        .arg("init")
        .current_dir(dir.path())
        .output()
        .unwrap();
    dir
}

/// Writes one raw line.
fn send(stdin: &mut ChildStdin, line: &str) {
    writeln!(stdin, "{line}").unwrap();
    stdin.flush().unwrap();
}

/// Sends one request with `id` and returns the parsed response with that id.
fn call(stdin: &mut ChildStdin, rx: &mpsc::Receiver<String>, id: u32, body: &str) -> Value {
    send(stdin, &format!(r#"{{"jsonrpc":"2.0","id":{id},{body}}}"#));
    loop {
        let line = rx
            .recv_timeout(Duration::from_secs(20))
            .unwrap_or_else(|_| panic!("no response to request {id} within 20s"));
        let Ok(v) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        if v["id"] == json!(id) {
            return v;
        }
    }
}

fn stop(mut child: Child) {
    let _ = child.kill();
    let _ = child.wait();
}

/// The `initialize` request of an old revision.
fn initialize(version: &str) -> String {
    format!(
        r#""method":"initialize","params":{{"protocolVersion":"{version}","capabilities":{{}},"clientInfo":{{"name":"test","version":"0"}}}}"#
    )
}

fn sorted_keys(v: &Value) -> Vec<String> {
    let mut keys: Vec<String> = v
        .as_object()
        .map(|o| o.keys().cloned().collect())
        .unwrap_or_default();
    keys.sort();
    keys
}

fn fixture_path() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/mcp/legacy_2025_06_18.jsonl")
}

/// The `tool_names` entry of a fixture's lines.
fn names_of(lines: &[Value]) -> BTreeSet<String> {
    lines
        .iter()
        .find_map(|l| l.get("tool_names"))
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(|n| n.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}

#[test]
fn the_2025_06_18_wire_is_unchanged() {
    let dir = project();
    let (child, mut stdin, rx) = start_mcp_raw(dir.path(), &[]);
    let init = call(&mut stdin, &rx, 1, &initialize("2025-06-18"));
    send(
        &mut stdin,
        r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#,
    );
    let list = call(&mut stdin, &rx, 2, r#""method":"tools/list","params":{}"#);
    let catalog = call(
        &mut stdin,
        &rx,
        3,
        r#""method":"tools/call","params":{"name":"playbook_catalog","arguments":{}}"#,
    );
    stop(child);

    // The initialize result, with what moves on every release replaced.
    let mut init_result = init["result"].clone();
    assert!(init_result.is_object(), "initialize failed: {init}");
    if let Some(v) = init_result.pointer_mut("/serverInfo/version") {
        *v = json!("<version>");
    }
    if let Some(v) = init_result.get_mut("instructions") {
        let text = v.as_str().unwrap_or_default().to_string();
        *v = json!(apb_core::content::sha256_hex(text.as_bytes()));
    }

    // The tools/list envelope, the sorted names and two definitions.
    let list_result = &list["result"];
    for absent in ["ttlMs", "cacheScope", "resultType", "nextCursor"] {
        assert!(
            list_result.get(absent).is_none(),
            "tools/list carries {absent}: {list_result}"
        );
    }
    let tools = list_result["tools"].as_array().expect("a tools array");
    let mut names: Vec<String> = tools
        .iter()
        .filter_map(|t| t["name"].as_str().map(str::to_string))
        .collect();
    names.sort();

    // The tools/call envelope, the block types, the text replaced.
    let call_result = &catalog["result"];
    assert!(
        call_result.get("resultType").is_none(),
        "tools/call carries resultType: {call_result}"
    );
    let block_types: Vec<Value> = call_result["content"]
        .as_array()
        .expect("a content array")
        .iter()
        .map(|b| b["type"].clone())
        .collect();

    let mut lines = vec![
        json!({ "initialize": init_result }),
        json!({ "tools_list_members": sorted_keys(list_result) }),
        json!({ "tool_names": names }),
    ];
    for name in PINNED_TOOLS {
        let def = tools
            .iter()
            .find(|t| t["name"] == json!(name))
            .unwrap_or_else(|| panic!("{name} is not listed"));
        lines.push(json!({ "tool": def }));
    }
    lines.push(json!({
        "tools_call_members": sorted_keys(call_result),
        "content_types": block_types,
        "text": "<text>",
    }));

    let path = fixture_path();
    if std::env::var_os("APB_RECORD_MCP_FIXTURE").is_some_and(|v| v == "1") {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let body: String = lines
            .iter()
            .map(|l| format!("{}\n", serde_json::to_string(l).unwrap()))
            .collect();
        std::fs::write(&path, body).unwrap();
        return;
    }

    let recorded: Vec<Value> = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("no fixture at {}: {e}", path.display()))
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    let (want, got) = (names_of(&recorded), names_of(&lines));
    if want != got {
        let added: Vec<&String> = got.difference(&want).collect();
        let removed: Vec<&String> = want.difference(&got).collect();
        panic!(
            "the tool names changed: added {added:?}, removed {removed:?}; \
             re-record with APB_RECORD_MCP_FIXTURE=1 in the change that adds or removes a tool"
        );
    }
    assert_eq!(recorded.len(), lines.len(), "fixture line count");
    for (want, got) in recorded.iter().zip(&lines) {
        assert_eq!(want, got, "the 2025-06-18 wire moved");
    }
}

#[test]
fn the_2025_11_25_tool_list_has_no_cache_fields() {
    let dir = project();
    let (child, mut stdin, rx) = start_mcp_raw(dir.path(), &[]);
    let init = call(&mut stdin, &rx, 1, &initialize("2025-11-25"));
    assert!(init["result"].is_object(), "initialize failed: {init}");
    send(
        &mut stdin,
        r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#,
    );
    let list = call(&mut stdin, &rx, 2, r#""method":"tools/list","params":{}"#);
    stop(child);

    let result = &list["result"];
    assert!(result["tools"].is_array(), "no tools: {list}");
    for absent in ["ttlMs", "cacheScope", "resultType"] {
        assert!(result.get(absent).is_none(), "{absent} in {result}");
    }
}

#[test]
fn a_2026_07_28_client_gets_discover_tier0_and_cache_hints() {
    let dir = project();
    let (child, mut stdin, rx) = start_mcp_raw(dir.path(), &[]);
    let discover = call(
        &mut stdin,
        &rx,
        1,
        &format!(r#""method":"server/discover","params":{{"_meta":{META_2026}}}"#),
    );
    let list = call(
        &mut stdin,
        &rx,
        2,
        &format!(r#""method":"tools/list","params":{{"_meta":{META_2026}}}"#),
    );
    let catalog = call(
        &mut stdin,
        &rx,
        3,
        &format!(
            r#""method":"tools/call","params":{{"name":"playbook_catalog","arguments":{{}},"_meta":{META_2026}}}"#
        ),
    );
    let prompts = call(
        &mut stdin,
        &rx,
        4,
        &format!(r#""method":"prompts/list","params":{{"_meta":{META_2026}}}"#),
    );
    stop(child);

    let d = &discover["result"];
    let versions: Vec<&str> = d["supportedVersions"]
        .as_array()
        .unwrap_or_else(|| panic!("no supportedVersions: {discover}"))
        .iter()
        .filter_map(Value::as_str)
        .collect();
    assert!(versions.contains(&"2026-07-28"), "{discover}");
    assert_eq!(
        d["instructions"].as_str(),
        Some(apb_mcp::instructions::TIER0),
        "{discover}"
    );

    let l = &list["result"];
    assert_eq!(l["resultType"], json!("complete"), "{list}");
    assert!(l["ttlMs"].is_number(), "{list}");
    assert!(l["cacheScope"].is_string(), "{list}");

    assert_eq!(
        catalog["result"]["resultType"],
        json!("complete"),
        "{catalog}"
    );

    let p = prompts["result"]["prompts"]
        .as_array()
        .unwrap_or_else(|| panic!("no prompts list: {prompts}"));
    assert!(p.is_empty(), "{prompts}");
}

/// The requests of the capabilities `apb mcp` does not advertise, with
/// their 1.0 answers on an old revision and on 2026-07-28. rmcp 3.11
/// answers them `-32601` by default; apb answers them itself.
fn unadvertised() -> [(&'static str, Value, Value, Value); 4] {
    let complete = json!({ "resultType": "complete", "ttlMs": 0, "cacheScope": "private" });
    let listed = |member: &str| {
        let mut modern = complete.clone();
        modern[member] = json!([]);
        (json!({ member: [] }), modern)
    };
    let (prompts, prompts_modern) = listed("prompts");
    let (resources, resources_modern) = listed("resources");
    let (templates, templates_modern) = listed("resourceTemplates");
    let no_values = json!({ "values": [] });
    [
        ("prompts/list", json!({}), prompts, prompts_modern),
        ("resources/list", json!({}), resources, resources_modern),
        (
            "resources/templates/list",
            json!({}),
            templates,
            templates_modern,
        ),
        (
            "completion/complete",
            json!({
                "ref": { "type": "ref/prompt", "name": "x" },
                "argument": { "name": "a", "value": "" }
            }),
            json!({ "completion": no_values }),
            json!({ "resultType": "complete", "completion": no_values }),
        ),
    ]
}

#[test]
fn unadvertised_capabilities_keep_their_1_0_answers() {
    let dir = project();
    let (child, mut stdin, rx) = start_mcp_raw(dir.path(), &[]);
    call(&mut stdin, &rx, 1, &initialize("2025-06-18"));
    send(
        &mut stdin,
        r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#,
    );
    let mut id = 1;
    for (method, params, old, _) in unadvertised() {
        id += 1;
        let answer = call(
            &mut stdin,
            &rx,
            id,
            &format!(r#""method":"{method}","params":{params}"#),
        );
        assert_eq!(answer["result"], old, "{answer}");
    }
    stop(child);

    let (child, mut stdin, rx) = start_mcp_raw(dir.path(), &[]);
    let meta: Value = serde_json::from_str(META_2026).unwrap();
    for (method, mut params, _, modern) in unadvertised() {
        id += 1;
        params["_meta"] = meta.clone();
        let answer = call(
            &mut stdin,
            &rx,
            id,
            &format!(r#""method":"{method}","params":{params}"#),
        );
        assert_eq!(answer["result"], modern, "{answer}");
    }
    stop(child);
}

#[test]
fn a_2026_07_28_request_without_meta_is_refused() {
    let dir = project();
    let (child, mut stdin, rx) = start_mcp_raw(dir.path(), &[]);
    let list = call(&mut stdin, &rx, 1, r#""method":"tools/list","params":{}"#);
    stop(child);
    assert_eq!(list["error"]["code"], json!(-32602), "{list}");
}
