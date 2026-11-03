//! OpenCode as an MCP host: entries of the `mcp` object of
//! `$XDG_CONFIG_HOME/opencode/opencode.json` (else
//! `~/.config/opencode/opencode.json`). apb inserts its entry as text right
//! after the opening brace it belongs in, so every other key keeps its
//! place and spelling; the result must parse to the old document plus the
//! entry, else nothing is inserted.

use std::path::PathBuf;

use serde_json::{Value, json};

use crate::agents::mcp_host::{McpHost, McpMethod, McpRegistration, McpServer, cut_once};

/// The object every MCP server lives in.
const SERVERS: &str = "mcp";

pub struct OpencodeMcp;

fn config_dir() -> Option<PathBuf> {
    std::env::var_os("XDG_CONFIG_HOME")
        .filter(|d| !d.is_empty())
        .map(PathBuf::from)
        .or_else(|| {
            std::env::var_os("HOME")
                .filter(|h| !h.is_empty())
                .map(|h| PathBuf::from(h).join(".config"))
        })
        .map(|d| d.join("opencode"))
}

/// The entry apb writes for `server`.
pub fn entry(server: &McpServer) -> Value {
    json!({ "type": "local", "command": server.command, "enabled": true })
}

/// The entry as the one line of text apb inserts.
fn entry_line(server: &McpServer) -> String {
    format!(
        "{}: {}",
        Value::from(server.name),
        serde_json::to_string(&entry(server)).unwrap_or_default()
    )
}

/// Whether `server` is registered in the parsed config `doc`.
pub fn registration_in(doc: &Value, server: &McpServer) -> McpRegistration {
    match doc.get(SERVERS).and_then(|m| m.get(server.name)) {
        None => McpRegistration::Absent,
        Some(e) if e == &entry(server) => McpRegistration::Same,
        Some(_) => McpRegistration::Different,
    }
}

/// The byte offset just past the `{` that opens the value of the top-level
/// key `key` when that value is an object; with `key == ""` the
/// document's own opening brace. `text` is valid JSON.
fn object_open(text: &str, key: &str) -> Option<usize> {
    let bytes = text.as_bytes();
    let mut depth = 0usize;
    let mut i = 0usize;
    let mut last_string: Option<(usize, usize)> = None;
    while i < bytes.len() {
        match bytes[i] {
            b'"' => {
                let start = i + 1;
                i += 1;
                while i < bytes.len() && bytes[i] != b'"' {
                    if bytes[i] == b'\\' {
                        i += 1;
                    }
                    i += 1;
                }
                last_string = Some((start, i));
            }
            b'{' => {
                if depth == 0 && key.is_empty() {
                    return Some(i + 1);
                }
                if depth == 1
                    && let Some((s, e)) = last_string
                    && &text[s..e] == key
                    && text[e + 1..i]
                        .trim()
                        .trim_start_matches(':')
                        .trim()
                        .is_empty()
                {
                    return Some(i + 1);
                }
                depth += 1;
                last_string = None;
            }
            b'[' => {
                depth += 1;
                last_string = None;
            }
            b'}' | b']' => {
                depth = depth.saturating_sub(1);
                last_string = None;
            }
            b',' => last_string = None,
            _ => {}
        }
        i += 1;
    }
    None
}

fn empty_at(text: &str, open: usize) -> bool {
    text[open..].trim_start().starts_with('}')
}

/// Where the entry goes into `text` and the bytes that go there.
fn insertion(text: &str, server: &McpServer) -> Option<(usize, String)> {
    let line = entry_line(server);
    if let Some(open) = object_open(text, SERVERS) {
        let tail = if empty_at(text, open) { "\n  " } else { "," };
        return Some((open, format!("\n    {line}{tail}")));
    }
    let open = object_open(text, "")?;
    let tail = if empty_at(text, open) { "\n" } else { "," };
    Some((
        open,
        format!("\n  \"{SERVERS}\": {{\n    {line}\n  }}{tail}"),
    ))
}

/// `doc` with the entry of `server`.
fn with_entry(doc: &Value, server: &McpServer) -> Option<Value> {
    let mut out = doc.clone();
    out.as_object_mut()?
        .entry(SERVERS)
        .or_insert_with(|| json!({}))
        .as_object_mut()?
        .insert(server.name.to_string(), entry(server));
    Some(out)
}

/// `doc` without the entry of `server`, and without the `mcp` object when
/// it is left empty and `after` has none.
fn without_entry(doc: &Value, after: &Value, server: &McpServer) -> Option<Value> {
    let mut out = doc.clone();
    let obj = out.as_object_mut()?;
    let servers = obj.get_mut(SERVERS)?.as_object_mut()?;
    servers.remove(server.name);
    if servers.is_empty() && after.get(SERVERS).is_none() {
        obj.remove(SERVERS);
    }
    Some(out)
}

impl McpHost for OpencodeMcp {
    fn config_path(&self) -> Option<PathBuf> {
        config_dir().map(|d| d.join("opencode.json"))
    }

    fn registration(&self, text: &str, server: &McpServer) -> McpRegistration {
        match serde_json::from_str::<Value>(text) {
            Ok(doc) if doc.is_object() => registration_in(&doc, server),
            _ => McpRegistration::Unreadable,
        }
    }

    fn method(&self, _server: &McpServer) -> McpMethod {
        McpMethod::EditFile
    }

    fn insert(&self, text: Option<&str>, server: &McpServer) -> Option<(String, String)> {
        let Some(text) = text else {
            let body = format!(
                "{{\n  \"{SERVERS}\": {{\n    {}\n  }}\n}}\n",
                entry_line(server)
            );
            return Some((body.clone(), body));
        };
        let doc: Value = serde_json::from_str(text).ok()?;
        if registration_in(&doc, server) != McpRegistration::Absent {
            return None;
        }
        let (at, inserted) = insertion(text, server)?;
        let mut out = text.to_string();
        out.insert_str(at, &inserted);
        let parsed: Value = serde_json::from_str(&out).ok()?;
        (Some(parsed) == with_entry(&doc, server)).then_some((out, inserted))
    }

    fn remove(
        &self,
        text: &str,
        inserted: &str,
        created: bool,
        server: &McpServer,
    ) -> Option<String> {
        if created && text == inserted {
            return Some(String::new());
        }
        let doc: Value = serde_json::from_str(text).ok()?;
        if registration_in(&doc, server) != McpRegistration::Same {
            return None;
        }
        let needles = [
            inserted.to_string(),
            format!("\n    {},", entry_line(server)),
            format!("\n    {}", entry_line(server)),
        ];
        needles.iter().find_map(|needle| {
            let out = cut_once(text, needle)?;
            let after: Value = serde_json::from_str(&out).ok()?;
            (Some(&after) == without_entry(&doc, &after, server).as_ref()).then_some(out)
        })
    }

    fn unsupported(&self) -> Option<String> {
        let dir = config_dir()?;
        (dir.join("opencode.jsonc").exists() && !dir.join("opencode.json").exists()).then(|| {
            "the config is opencode.jsonc, which apb does not edit; register the server by hand (see llms.txt)".to_string()
        })
    }

    fn snippet(&self, server: &McpServer) -> Option<String> {
        Some(format!("\"{SERVERS}\": {{ {} }}", entry_line(server)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SERVER: McpServer = McpServer {
        name: "srv",
        command: &["prog", "serve"],
    };

    #[test]
    fn the_insert_keeps_other_keys_in_place_and_the_removal_restores_the_text() {
        let cases = [
            "{\n  \"$schema\": \"https://example.test/config.json\",\n  \"theme\": \"x\",\n  \"mcp\": {\n    \"other\": { \"type\": \"local\", \"command\": [\"o\"] }\n  },\n  \"z\": [1, {\"mcp\": {}}]\n}\n",
            "{\n  \"theme\": \"a {brace} \\\" in a string\",\n  \"provider\": {\"mcp\": {\"nested\": true}}\n}\n",
            "{}",
            "{\"mcp\": {}}",
        ];
        for text in cases {
            let before: Value = serde_json::from_str(text).unwrap();
            let (edited, inserted) = OpencodeMcp.insert(Some(text), &SERVER).unwrap();
            let after: Value = serde_json::from_str(&edited).unwrap();
            assert_eq!(registration_in(&after, &SERVER), McpRegistration::Same);
            assert_eq!(Some(after), with_entry(&before, &SERVER), "{edited}");
            assert_eq!(
                OpencodeMcp
                    .remove(&edited, &inserted, false, &SERVER)
                    .as_deref(),
                Some(text)
            );
        }
    }

    /// The entry apb writes, spelled out once instead of derived from the
    /// helpers the insert itself checks against.
    #[test]
    fn the_inserted_entry_is_the_documented_opencode_local_server() {
        let (edited, _) = OpencodeMcp.insert(Some("{}"), &SERVER).unwrap();
        let after: Value = serde_json::from_str(&edited).unwrap();
        assert_eq!(
            after,
            serde_json::json!({
                "mcp": {
                    "srv": { "type": "local", "command": ["prog", "serve"], "enabled": true }
                }
            })
        );
    }

    #[test]
    fn an_edited_entry_is_never_taken_back() {
        let text = "{\n  \"theme\": \"x\"\n}\n";
        let (edited, inserted) = OpencodeMcp.insert(Some(text), &SERVER).unwrap();
        let changed = edited.replace("\"enabled\":true", "\"enabled\":false");
        assert_ne!(changed, edited);
        assert_eq!(
            OpencodeMcp.remove(&changed, &inserted, false, &SERVER),
            None
        );
        assert!(OpencodeMcp.insert(Some(&edited), &SERVER).is_none());
        let (created, body) = OpencodeMcp.insert(None, &SERVER).unwrap();
        assert_eq!(created, body);
        assert_eq!(
            OpencodeMcp
                .remove(&created, &body, true, &SERVER)
                .as_deref(),
            Some("")
        );
    }
}
