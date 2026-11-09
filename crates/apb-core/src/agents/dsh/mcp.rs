//! dsh as an MCP host. An MCP server is a `@deepseek-ai/dsh-mcp-client`
//! row of the composed profile (dsh's `docs/user/guide/mcp-memory.md`); the
//! home patch layer `$DSH_HOME/cordis.patch.yml` (default `~/.dsh`) is read
//! by every profile, after the profile's own layer. apb appends one `insert`
//! patch holding its row at the end of that file, and the removal takes
//! exactly those bytes back; the result must parse to the old patch list
//! plus that one patch, else nothing is written. The row's tools reach the
//! model as `mcp__<serverName>__<tool>`.

use std::path::PathBuf;

use yaml_serde::Value;

use crate::agents::mcp_host::{
    McpHost, McpMethod, McpRegistration, McpServer, blank_line_before, cut_once,
};

/// The plugin an MCP server row names.
const CLIENT: &str = "@deepseek-ai/dsh-mcp-client";
/// The file of the home patch layer.
const HOME_PATCH: &str = "cordis.patch.yml";

pub struct DshMcp;

/// dsh's home: `$DSH_HOME`, else `~/.dsh`.
pub fn dsh_home() -> Option<PathBuf> {
    std::env::var_os("DSH_HOME")
        .filter(|d| !d.is_empty())
        .map(PathBuf::from)
        .or_else(|| {
            std::env::var_os("HOME")
                .filter(|h| !h.is_empty())
                .map(|h| PathBuf::from(h).join(".dsh"))
        })
}

fn quoted(s: &str) -> String {
    serde_json::Value::from(s).to_string()
}

/// The patch apb appends for `server`, as text.
fn block(server: &McpServer) -> String {
    let (program, args) = server.command.split_first().unwrap_or((&"", &[]));
    let args: Vec<String> = args.iter().map(|a| quoted(a)).collect();
    format!(
        "# The {name} MCP server, added by `apb setup`.\n\
         - insert:\n    - id: {id}\n      name: {client}\n      config:\n        \
         serverName: {name_q}\n        transport: stdio\n        command: {program}\n        \
         args: [{args}]\n",
        name = server.name,
        id = quoted(&format!("mcp-{}", server.name)),
        client = quoted(CLIENT),
        name_q = quoted(server.name),
        program = quoted(program),
        args = args.join(", "),
    )
}

/// The rows of a parsed patch list: its top-level rows and the rows of its
/// `insert` patches.
fn rows(doc: &Value) -> Vec<&Value> {
    let Some(items) = doc.as_sequence() else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for item in items {
        match item.get("insert").and_then(Value::as_sequence) {
            Some(inner) => out.extend(inner.iter()),
            None => out.push(item),
        }
    }
    out
}

fn str_of<'a>(v: &'a Value, key: &str) -> Option<&'a str> {
    v.get(key).and_then(Value::as_str)
}

/// Whether `server` is registered in the parsed patch list `doc`.
fn registration_in(doc: &Value, server: &McpServer) -> McpRegistration {
    let Some(row) = rows(doc).into_iter().find(|r| {
        r.get("config")
            .and_then(|c| str_of(c, "serverName"))
            .is_some_and(|n| n == server.name)
    }) else {
        return McpRegistration::Absent;
    };
    let config = row.get("config");
    let command: Option<Vec<&str>> = config.map(|c| {
        let mut out: Vec<&str> = str_of(c, "command").into_iter().collect();
        if let Some(args) = c.get("args").and_then(Value::as_sequence) {
            out.extend(args.iter().filter_map(Value::as_str));
        }
        out
    });
    let same = str_of(row, "name") == Some(CLIENT)
        && config.and_then(|c| str_of(c, "transport")) == Some("stdio")
        && command.as_deref() == Some(server.command);
    if same {
        McpRegistration::Same
    } else {
        McpRegistration::Different
    }
}

/// The patch list of `text`: an empty file (or one with comments only) is
/// an empty list; anything but a list is unreadable.
fn parse(text: &str) -> Option<Value> {
    match yaml_serde::from_str::<Value>(text).ok()? {
        Value::Null => Some(Value::Sequence(Vec::new())),
        v @ Value::Sequence(_) => Some(v),
        _ => None,
    }
}

/// The length of the patch list `doc`.
fn len(doc: &Value) -> usize {
    doc.as_sequence().map_or(0, Vec::len)
}

impl McpHost for DshMcp {
    fn config_path(&self) -> Option<PathBuf> {
        dsh_home().map(|d| d.join(HOME_PATCH))
    }

    // The home patch layer is optional: the home directory shows dsh is set
    // up.
    fn marker(&self) -> Option<PathBuf> {
        dsh_home()
    }

    fn registration(&self, text: &str, server: &McpServer) -> McpRegistration {
        match parse(text) {
            Some(doc) => registration_in(&doc, server),
            None => McpRegistration::Unreadable,
        }
    }

    fn method(&self, _server: &McpServer) -> McpMethod {
        McpMethod::EditFile
    }

    fn insert(&self, text: Option<&str>, server: &McpServer) -> Option<(String, String)> {
        let body = block(server);
        let Some(text) = text else {
            return Some((body.clone(), body));
        };
        let doc = parse(text)?;
        if registration_in(&doc, server) != McpRegistration::Absent {
            return None;
        }
        let inserted = format!("{}{body}", blank_line_before(text));
        let out = format!("{text}{inserted}");
        let after = parse(&out)?;
        let kept = after.as_sequence()?.get(..len(&doc))? == doc.as_sequence()?.as_slice();
        (kept
            && len(&after) == len(&doc) + 1
            && registration_in(&after, server) == McpRegistration::Same)
            .then_some((out, inserted))
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
        let doc = parse(text)?;
        if registration_in(&doc, server) != McpRegistration::Same {
            return None;
        }
        let out = cut_once(text, inserted)?;
        let after = parse(&out)?;
        (len(&after) + 1 == len(&doc) && registration_in(&after, server) == McpRegistration::Absent)
            .then_some(out)
    }

    fn snippet(&self, server: &McpServer) -> Option<String> {
        Some(block(server))
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
    fn the_insert_appends_one_patch_and_the_removal_restores_the_text() {
        let cases = [
            "# my layer\n- id: session-log-deepseek\n  config:\n    enabled: false\n",
            "- id: x\n  disabled: !!js process.platform === 'win32'\n",
            "# only a comment\n",
            "",
            "- id: y\n  config: {}",
        ];
        for text in cases {
            let (edited, inserted) = DshMcp.insert(Some(text), &SERVER).unwrap();
            assert!(edited.starts_with(text));
            assert_eq!(DshMcp.registration(&edited, &SERVER), McpRegistration::Same);
            assert_eq!(
                DshMcp.remove(&edited, &inserted, false, &SERVER).as_deref(),
                Some(text),
                "{edited}"
            );
        }
    }

    /// The row apb writes, spelled out once.
    #[test]
    fn the_inserted_row_is_a_stdio_mcp_client() {
        let (body, _) = DshMcp.insert(None, &SERVER).unwrap();
        let doc: serde_json::Value = yaml_serde::from_str(&body).unwrap();
        assert_eq!(
            doc,
            serde_json::json!([{ "insert": [{
                "id": "mcp-srv",
                "name": "@deepseek-ai/dsh-mcp-client",
                "config": {
                    "serverName": "srv",
                    "transport": "stdio",
                    "command": "prog",
                    "args": ["serve"]
                }
            }]}])
        );
    }

    #[test]
    fn another_entry_of_that_name_is_never_touched() {
        let (edited, inserted) = DshMcp.insert(Some(""), &SERVER).unwrap();
        let changed = edited.replace("\"serve\"", "\"other\"");
        assert_eq!(
            DshMcp.registration(&changed, &SERVER),
            McpRegistration::Different
        );
        assert!(DshMcp.insert(Some(&changed), &SERVER).is_none());
        assert_eq!(DshMcp.remove(&changed, &inserted, false, &SERVER), None);
        assert_eq!(
            DshMcp.registration("{a: 1}", &SERVER),
            McpRegistration::Unreadable
        );
        assert_eq!(
            DshMcp.remove(&edited, &inserted, true, &SERVER).as_deref(),
            Some("")
        );
    }
}
