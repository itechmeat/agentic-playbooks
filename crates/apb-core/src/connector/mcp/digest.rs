//! The digest of one MCP tool definition and of a whole pin binding. A
//! tool digest is taken over the raw JSON object the server's `tools/list`
//! answer carried, never over a typed decoding of it, so a member a later
//! protocol revision adds is pinned from the first day.

use std::collections::BTreeMap;

use serde_json::Value;

/// The domain tag of a tool digest; bump the trailing number to change
/// what is hashed.
pub const TOOL_DIGEST_DOMAIN: &str = "apb-mcp-tool/1";

/// The members of a tool definition the digest leaves out, each with its
/// reason. Everything else the server sends is pinned.
pub const UNPINNED_MEMBERS: [&str; 2] = [
    // Display data; a server may rotate an icon URL per request.
    "icons", // Free metadata; the specification lets it change per request.
    "_meta",
];

/// The domain tag of a pin digest.
pub const PIN_DIGEST_DOMAIN: &str = "apb-mcp-pin/1";

/// `sha256:<hex>` of `TOOL_DIGEST_DOMAIN`, a newline and the canonical JSON
/// of one tool definition with the top-level members of `UNPINNED_MEMBERS`
/// removed. `tool` is the raw JSON object exactly as the server's
/// `tools/list` page carried it. The whole object is taken, never a list of
/// the members to keep. A tool that is not a JSON object is digested as is.
pub fn tool_digest(tool: &Value) -> String {
    let pinned = match tool {
        Value::Object(map) => Value::Object(
            map.iter()
                .filter(|(k, _)| !UNPINNED_MEMBERS.contains(&k.as_str()))
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect(),
        ),
        other => other.clone(),
    };
    domain_digest(TOOL_DIGEST_DOMAIN, &pinned)
}

/// `sha256:<hex>` of `domain`, a newline and the canonical JSON of `value`.
fn domain_digest(domain: &str, value: &Value) -> String {
    crate::content::sha256_hex(format!("{domain}\n{}", canonical_json(value)).as_bytes())
}

/// JSON text with every object's keys sorted (byte order), no whitespace,
/// numbers as serde_json prints them. Recursive. Never relies on the map
/// type's own order.
pub fn canonical_json(value: &Value) -> String {
    let mut out = String::new();
    write_canonical(value, &mut out);
    out
}

fn write_canonical(value: &Value, out: &mut String) {
    match value {
        Value::Object(map) => {
            let mut keys: Vec<&String> = map.keys().collect();
            keys.sort_by(|a, b| a.as_bytes().cmp(b.as_bytes()));
            out.push('{');
            for (i, k) in keys.into_iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                out.push_str(&Value::String(k.clone()).to_string());
                out.push(':');
                write_canonical(&map[k], out);
            }
            out.push('}');
        }
        Value::Array(items) => {
            out.push('[');
            for (i, item) in items.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                write_canonical(item, out);
            }
            out.push(']');
        }
        scalar => out.push_str(&scalar.to_string()),
    }
}

/// `sha256:<hex>` of `PIN_DIGEST_DOMAIN`, a newline and the canonical JSON
/// of `{"connector_digest": .., "account_digest": .., "program": ..,
/// "program_digest": .., "tools": {<tool>: <tool digest>, ..}}`: the whole
/// binding one server pin record approves.
pub fn pin_digest(
    connector_digest: &str,
    account_digest: &str,
    program: &str,
    program_digest: &str,
    tools: &BTreeMap<String, String>,
) -> String {
    let binding = serde_json::json!({
        "connector_digest": connector_digest,
        "account_digest": account_digest,
        "program": program,
        "program_digest": program_digest,
        "tools": tools,
    });
    domain_digest(PIN_DIGEST_DOMAIN, &binding)
}

/// The domain tag of an endpoint digest.
pub const ENDPOINT_DIGEST_DOMAIN: &str = "mcp-http-endpoint/1";

/// `sha256:<hex>` of `ENDPOINT_DIGEST_DOMAIN`, a newline and the canonical
/// JSON of `{"endpoint": .., "token_url": ..}` (the token URL empty when
/// `None`), the encoding `pin_digest` uses: what an http server pin
/// record keeps in `program_digest`, so the pin binds the URL a call
/// reaches and the URL its token request reaches.
pub fn endpoint_digest(endpoint: &str, token_url: Option<&str>) -> String {
    let binding = serde_json::json!({
        "endpoint": endpoint,
        "token_url": token_url.unwrap_or_default(),
    });
    domain_digest(ENDPOINT_DIGEST_DOMAIN, &binding)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn tool() -> Value {
        json!({
            "name": "search_issues",
            "title": "Search issues",
            "description": "Search issues by text",
            "inputSchema": {
                "type": "object",
                "properties": { "query": { "type": "string" } },
                "required": ["query"]
            },
            "annotations": { "readOnlyHint": true }
        })
    }

    #[test]
    fn key_order_does_not_change_the_digest() {
        let a: Value = serde_json::from_str(
            r#"{"name":"t","inputSchema":{"type":"object","properties":{"b":{"type":"string"},"a":{"type":"number"}}},"description":"d"}"#,
        )
        .unwrap();
        let b: Value = serde_json::from_str(
            r#"{"description":"d","inputSchema":{"properties":{"a":{"type":"number"},"b":{"type":"string"}},"type":"object"},"name":"t"}"#,
        )
        .unwrap();
        assert_eq!(tool_digest(&a), tool_digest(&b));
        assert_eq!(
            canonical_json(&a),
            r#"{"description":"d","inputSchema":{"properties":{"a":{"type":"number"},"b":{"type":"string"}},"type":"object"},"name":"t"}"#
        );
    }

    #[test]
    fn icons_and_meta_are_not_pinned() {
        let mut with = tool();
        with["icons"] = json!([{ "src": "https://example.test/a.png" }]);
        with["_meta"] = json!({ "trace": "1" });
        assert_eq!(tool_digest(&with), tool_digest(&tool()));
        // Only at the top level: a nested `_meta` is the tool's own data.
        let mut nested = tool();
        nested["inputSchema"]["_meta"] = json!({ "x": 1 });
        assert_ne!(tool_digest(&nested), tool_digest(&tool()));
    }

    #[test]
    fn a_changed_description_changes_the_digest() {
        let mut changed = tool();
        changed["description"] = json!("Search issues by text and delete them");
        assert_ne!(tool_digest(&changed), tool_digest(&tool()));
    }

    #[test]
    fn a_member_no_list_names_is_pinned() {
        let mut later = tool();
        later["execution"] = json!({ "taskSupport": "optional" });
        assert_ne!(tool_digest(&later), tool_digest(&tool()));
        let mut hint = tool();
        hint["annotations"]["newHint"] = json!(true);
        assert_ne!(tool_digest(&hint), tool_digest(&tool()));
    }

    #[test]
    fn an_absent_title_and_an_empty_title_differ() {
        let mut absent = tool();
        absent.as_object_mut().unwrap().remove("title");
        let mut empty = tool();
        empty["title"] = json!("");
        assert_ne!(tool_digest(&absent), tool_digest(&empty));
    }

    #[test]
    fn the_digest_starts_with_sha256() {
        let d = tool_digest(&tool());
        assert!(d.starts_with("sha256:"), "{d}");
        assert_eq!(d.len(), "sha256:".len() + 64, "{d}");
        let expected = crate::content::sha256_hex(
            format!("{TOOL_DIGEST_DOMAIN}\n{}", canonical_json(&tool())).as_bytes(),
        );
        assert_eq!(d, expected);
        // Not an object: digested as is.
        assert!(tool_digest(&json!("x")).starts_with("sha256:"));
    }

    #[test]
    fn the_pin_digest_covers_program_and_tools() {
        let tools = BTreeMap::from([("search_issues".to_string(), tool_digest(&tool()))]);
        let base = pin_digest("sha256:c", "sha256:a", "/usr/bin/srv", "sha256:p", &tools);
        assert!(base.starts_with("sha256:"), "{base}");
        let mut other_tools = tools.clone();
        other_tools.insert("search_issues".to_string(), "sha256:other".to_string());
        let variants = [
            pin_digest("sha256:c2", "sha256:a", "/usr/bin/srv", "sha256:p", &tools),
            pin_digest("sha256:c", "sha256:a2", "/usr/bin/srv", "sha256:p", &tools),
            pin_digest("sha256:c", "sha256:a", "/usr/bin/srv2", "sha256:p", &tools),
            pin_digest("sha256:c", "sha256:a", "/usr/bin/srv", "sha256:p2", &tools),
            pin_digest(
                "sha256:c",
                "sha256:a",
                "/usr/bin/srv",
                "sha256:p",
                &other_tools,
            ),
        ];
        for v in variants {
            assert_ne!(v, base);
        }
        assert_eq!(
            base,
            pin_digest("sha256:c", "sha256:a", "/usr/bin/srv", "sha256:p", &tools)
        );
    }

    #[test]
    fn the_endpoint_digest_covers_the_token_url() {
        let endpoint = "https://crm.example/mcp";
        let token = Some("https://crm.example/oauth/token");
        let base = endpoint_digest(endpoint, token);
        assert!(base.starts_with("sha256:"), "{base}");
        assert_eq!(base.len(), "sha256:".len() + 64, "{base}");
        let variants = [
            endpoint_digest("https://crm.example/mcp2", token),
            endpoint_digest(endpoint, Some("https://crm.example/oauth/token2")),
            endpoint_digest(endpoint, None),
        ];
        for v in &variants {
            assert_ne!(*v, base);
        }
        assert_ne!(variants[0], variants[2]);
        // The boundary between the two URLs is part of what is hashed.
        assert_ne!(
            endpoint_digest("https://a/b", Some("c")),
            endpoint_digest("https://a/", Some("bc"))
        );
        assert_eq!(base, endpoint_digest(endpoint, token));
        assert_eq!(
            endpoint_digest(endpoint, None),
            endpoint_digest(endpoint, None)
        );
    }
}
