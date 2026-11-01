//! The read-only server: its tool set, a direct call of every removed tool
//! through the real MCP dispatch, and the restricted tools in a read-only
//! child process.

use super::read_only::{READ_ONLY, REMOVED_IN_READ_ONLY, ReadOnlyUse};
use super::*;
use rmcp::handler::server::wrapper::Parameters;
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

fn names(server: &WfMcp) -> BTreeSet<String> {
    server
        .tool_router
        .list_all()
        .iter()
        .map(|t| t.name.to_string())
        .collect()
}

fn text(result: &CallToolResult) -> serde_json::Value {
    let text: String = result
        .content
        .iter()
        .filter_map(|block| block.as_text())
        .map(|t| t.text.clone())
        .collect();
    serde_json::from_str(&text).unwrap_or(serde_json::Value::String(text))
}

/// Every tool of the full server is in exactly one table, and the tables
/// name no tool the server lacks.
#[test]
fn every_tool_is_classified_exactly_once() {
    let all = names(&WfMcp::new(PathBuf::from(".")));
    let allowed: Vec<&str> = READ_ONLY.iter().map(|(n, _)| *n).collect();
    for name in &all {
        let hits = allowed.iter().filter(|n| *n == name).count()
            + REMOVED_IN_READ_ONLY.iter().filter(|n| *n == name).count();
        assert_eq!(hits, 1, "tool `{name}` is in {hits} tables");
    }
    for name in allowed.iter().chain(REMOVED_IN_READ_ONLY) {
        assert!(
            all.contains(*name),
            "the tables name an unknown tool `{name}`"
        );
    }
    assert_eq!(all.len(), allowed.len() + REMOVED_IN_READ_ONLY.len());
    assert!(READ_ONLY.iter().any(|(_, u)| *u == ReadOnlyUse::Restricted));
}

/// The read-only server lists exactly the allowlist, and read-only wins
/// over the supervisor role: no `supervisor_*` tool.
#[test]
fn the_read_only_server_lists_exactly_the_allowlist() {
    let listed = names(&WfMcp::for_read_only(PathBuf::from(".")));
    let allowed: BTreeSet<String> = READ_ONLY.iter().map(|(n, _)| n.to_string()).collect();
    assert_eq!(listed, allowed);
    assert!(!listed.iter().any(|n| n.starts_with("supervisor_")));
}

/// Writes one JSON-RPC message and reads lines until the response with
/// `id` arrives.
async fn exchange(
    w: &mut (impl tokio::io::AsyncWrite + Unpin),
    r: &mut tokio::io::Lines<tokio::io::BufReader<impl tokio::io::AsyncRead + Unpin>>,
    message: serde_json::Value,
) -> Option<serde_json::Value> {
    use tokio::io::AsyncWriteExt;
    let id = message.get("id").cloned();
    w.write_all(format!("{message}\n").as_bytes())
        .await
        .unwrap();
    w.flush().await.unwrap();
    let id = id?;
    loop {
        let line = tokio::time::timeout(std::time::Duration::from_secs(10), r.next_line())
            .await
            .expect("an answer in time")
            .unwrap()
            .expect("the server is still there");
        let v: serde_json::Value = serde_json::from_str(&line).unwrap();
        if v.get("id") == Some(&id) {
            return Some(v);
        }
    }
}

/// Arguments that satisfy a tool's input schema: every required property
/// with a value of its type (an enum's first value, a `$ref` followed), so
/// a refusal of the call cannot come from its arguments.
fn routable_arguments(schema: &serde_json::Map<String, serde_json::Value>) -> serde_json::Value {
    use serde_json::{Value, json};
    fn value_of(s: &Value, root: &serde_json::Map<String, Value>) -> Value {
        if let Some(r) = s.get("$ref").and_then(Value::as_str) {
            let name = r.rsplit('/').next().unwrap_or_default();
            let def = root
                .get("$defs")
                .or_else(|| root.get("definitions"))
                .and_then(|d| d.get(name))
                .cloned()
                .unwrap_or(Value::Null);
            return value_of(&def, root);
        }
        if let Some(first) = s.get("enum").and_then(|e| e.get(0)) {
            return first.clone();
        }
        if let Some(c) = s.get("const") {
            return c.clone();
        }
        for key in ["oneOf", "anyOf", "allOf"] {
            if let Some(first) = s.get(key).and_then(|v| v.get(0)) {
                return value_of(first, root);
            }
        }
        let ty = match s.get("type") {
            Some(Value::Array(types)) => types.iter().find(|t| *t != "null").cloned(),
            other => other.cloned(),
        };
        match ty.as_ref().and_then(Value::as_str) {
            Some("string") => json!("x"),
            Some("integer" | "number") => json!(1),
            Some("boolean") => json!(false),
            Some("array") => json!([]),
            Some("object") => object_of(s, root),
            _ => Value::Null,
        }
    }
    fn object_of(s: &Value, root: &serde_json::Map<String, Value>) -> Value {
        let mut out = serde_json::Map::new();
        for name in s
            .get("required")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
        {
            let prop = s
                .get("properties")
                .and_then(|p| p.get(name))
                .cloned()
                .unwrap_or(Value::Null);
            out.insert(name.to_string(), value_of(&prop, root));
        }
        Value::Object(out)
    }
    object_of(&Value::Object(schema.clone()), schema)
}

/// A direct `tools/call` of every removed tool, with arguments its schema
/// accepts, through the MCP dispatch of a served read-only server over a
/// duplex stream, is a JSON-RPC error of the unknown tool; an allowed tool
/// answers, and one called with arguments it rejects fails otherwise.
#[tokio::test]
async fn a_direct_call_of_every_removed_tool_is_refused() {
    use tokio::io::AsyncBufReadExt;
    let (server_io, client_io) = tokio::io::duplex(1 << 16);
    let service = WfMcp::for_read_only(PathBuf::from("."));
    let served = tokio::spawn(async move {
        let running = service.serve(server_io).await.unwrap();
        let _ = running.waiting().await;
    });
    let (client_r, mut client_w) = tokio::io::split(client_io);
    let mut lines = tokio::io::BufReader::new(client_r).lines();
    let init = serde_json::json!({
        "jsonrpc": "2.0", "id": 0, "method": "initialize",
        "params": { "protocolVersion": "2024-11-05", "capabilities": {},
                    "clientInfo": { "name": "t", "version": "0" } }
    });
    assert!(exchange(&mut client_w, &mut lines, init).await.is_some());
    let initialized =
        serde_json::json!({ "jsonrpc": "2.0", "method": "notifications/initialized" });
    exchange(&mut client_w, &mut lines, initialized).await;
    let full = WfMcp::new(PathBuf::from("."));
    let schemas: BTreeMap<String, serde_json::Value> = full
        .tool_router
        .list_all()
        .iter()
        .map(|t| (t.name.to_string(), routable_arguments(&t.input_schema)))
        .collect();
    let unknown_tool = |answer: &serde_json::Value| {
        answer["error"]["code"] == serde_json::json!(-32602)
            && answer["error"]["message"] == "tool not found"
    };
    for (i, name) in REMOVED_IN_READ_ONLY.iter().enumerate() {
        let arguments = schemas[*name].clone();
        let call = serde_json::json!({
            "jsonrpc": "2.0", "id": i + 1, "method": "tools/call",
            "params": { "name": name, "arguments": arguments }
        });
        let answer = exchange(&mut client_w, &mut lines, call).await.unwrap();
        assert!(
            unknown_tool(&answer),
            "`{name}` was not refused as an unknown tool: {answer}"
        );
    }
    // The control: an allowed tool that rejects its arguments answers a
    // tool error, never the dispatcher's unknown tool.
    let call = serde_json::json!({
        "jsonrpc": "2.0", "id": 999, "method": "tools/call",
        "params": { "name": "run_status", "arguments": { "run_id": 7 } }
    });
    let answer = exchange(&mut client_w, &mut lines, call).await.unwrap();
    assert_eq!(
        answer["result"]["isError"],
        serde_json::json!(true),
        "{answer}"
    );
    assert!(!unknown_tool(&answer), "{answer}");
    let call = serde_json::json!({
        "jsonrpc": "2.0", "id": 1000, "method": "tools/call",
        "params": { "name": "playbook_howto", "arguments": {} }
    });
    let answer = exchange(&mut client_w, &mut lines, call).await.unwrap();
    assert_eq!(
        answer["result"]["isError"],
        serde_json::json!(false),
        "{answer}"
    );
    drop(client_w);
    drop(lines);
    let _ = tokio::time::timeout(std::time::Duration::from_secs(5), served).await;
}

/// Every file under `root` with its bytes.
fn files(root: &Path) -> BTreeMap<PathBuf, Vec<u8>> {
    let mut out = BTreeMap::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        for e in std::fs::read_dir(&dir).unwrap().flatten() {
            let p = e.path();
            if e.file_type().unwrap().is_dir() {
                stack.push(p);
            } else {
                out.insert(p.clone(), std::fs::read(&p).unwrap_or_default());
            }
        }
    }
    out
}

apb_core::read_only_test! {
    /// The restricted tools: catalog ranking against a configured provider
    /// sends nothing and logs nothing, a detection refresh is refused, and
    /// remembered input is only read.
    fn the_restricted_tools_refuse_what_would_act() {
        apb_engine::read_only::enter();
        let cfg = tempfile::tempdir().unwrap();
        // SAFETY: a read-only child runs this one test, and no other thread
        // has started yet (the stub server below starts one).
        unsafe {
            std::env::set_var("APB_CONFIG_DIR", cfg.path());
            std::env::remove_var("APB_DECISIONS");
        }
        let stub = apb_decide::testing::StubServer::start(vec![]);
        std::fs::write(
            cfg.path().join("decisions.yaml"),
            format!(
                "mode: advise\ntimeout_ms: 5000\nproviders: [{{ id: s, kind: systemone, base_url: '{}', model: m }}]\nuses: {{ catalog_rank: {{ mode: advise }} }}\n",
                stub.base_url
            ),
        )
        .unwrap();
        let dir = tempfile::tempdir().unwrap();
        // The project is written before the server reads it; the child
        // process is read-only, so the fixture goes through `std::fs`.
        let root = dir.path();
        project(root);
        let before = (files(root), files(cfg.path()));
        let server = WfMcp::for_read_only(root.to_path_buf());
        let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        rt.block_on(async {
            let catalog = text(
                &server
                    .playbook_catalog(Parameters(PlaybookCatalogArgs {
                        revision: None,
                        limit: None,
                        workspace: None,
                        query: Some("implement the task".into()),
                        fit: None,
                    }))
                    .await,
            );
            assert_eq!(catalog["ranking"], serde_json::json!({ "skipped": "read_only" }), "{catalog}");
            let detect = text(
                &server
                    .agents_detect(Parameters(AgentsDetectArgs { refresh: true }))
                    .await,
            );
            assert_eq!(detect["error"], "read_only", "{detect}");
            for action in ["set", "clear"] {
                let memory = text(
                    &server
                        .playbook_memory(Parameters(PlaybookMemoryArgs {
                            id: "p".into(),
                            scope: None,
                            action: Some(action.into()),
                            params: Some([("who".to_string(), "me".to_string())].into()),
                            instruction: None,
                            what: Some("all".into()),
                            workspace: None,
                        }))
                        .await,
                );
                assert_eq!(memory["error"], "read_only", "{action}: {memory}");
            }
        });
        assert_eq!(stub.count(), 0, "a ranking request was sent");
        assert!(!root.join(".apb/decisions.jsonl").exists());
        assert_eq!((files(root), files(cfg.path())), before, "something was written");
    }
}

/// A project with one playbook, `p`, whose param `who` can be remembered,
/// written with `std::fs` only (a read-only child writes its fixture so).
fn project(root: &Path) {
    let v = root.join(".apb/playbooks/p/1.0.0");
    std::fs::create_dir_all(&v).unwrap();
    std::fs::write(
        v.join("playbook.yaml"),
        "schema: 2\nid: p\nname: P\nversion: 1.0.0\nparams:\n  - { name: who, type: text }\nnodes:\n  - { id: start, type: start }\n  - { id: done, type: finish, outcome: success }\nedges:\n  - { from: start, to: done }\n",
    )
    .unwrap();
    std::fs::write(root.join(".apb/playbooks/p/current"), "1.0.0").unwrap();
}
