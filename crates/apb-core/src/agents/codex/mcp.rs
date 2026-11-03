//! Codex as an MCP host: `[mcp_servers.<name>]` tables in
//! `$CODEX_HOME/config.toml` (else `~/.codex/config.toml`). apb appends its
//! own table and later removes exactly those bytes, so comments, order and
//! formatting stay as they were; the file is never rewritten from a parsed
//! document.

use std::path::PathBuf;

use super::home::source_codex_home;
use super::toml::{header_path, key_value};
use crate::agents::mcp_host::{
    McpHost, McpMethod, McpRegistration, McpServer, blank_line_before, cut_once,
};

/// The table every MCP server lives under.
const SERVERS: &str = "mcp_servers";

pub struct CodexMcp;

fn quoted(s: &str) -> String {
    format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\""))
}

/// The table apb appends for `server`.
pub fn block(server: &McpServer) -> String {
    let (program, args) = server.command.split_first().unwrap_or((&"", &[]));
    let args: Vec<String> = args.iter().map(|a| quoted(a)).collect();
    format!(
        "[{SERVERS}.{}]\ncommand = {}\nargs = [{}]\n",
        server.name,
        quoted(program),
        args.join(", ")
    )
}

/// A raw TOML value with its whitespace dropped, a trailing comment cut
/// and single quotes made double, for comparing with what apb writes.
fn normalize(v: &str) -> String {
    let v = v.split(" #").next().unwrap_or(v);
    v.chars()
        .filter(|c| !c.is_whitespace())
        .map(|c| if c == '\'' { '"' } else { c })
        .collect()
}

/// Whether `server` is registered in the config `text`.
pub fn registration(text: &str, server: &McpServer) -> McpRegistration {
    let (program, args) = server.command.split_first().unwrap_or((&"", &[]));
    let want_command = quoted(program);
    let want_args = normalize(&format!(
        "[{}]",
        args.iter().map(|a| quoted(a)).collect::<Vec<_>>().join(",")
    ));
    let ours = |path: &[String]| ours(path, server);
    let mut table: Vec<String> = Vec::new();
    let (mut found, mut other) = (false, false);
    let (mut command, mut arguments) = (None, None);
    for line in text.lines() {
        if let Some(path) = header_path(line) {
            found |= ours(&path);
            table = path;
            continue;
        }
        let Some((key, value)) = key_value(line) else {
            continue;
        };
        let mut full = table.clone();
        full.extend(key);
        if !ours(&full) {
            continue;
        }
        found = true;
        match full.get(2).map(String::as_str) {
            Some("command") if full.len() == 3 => command = Some(normalize(value)),
            Some("args") if full.len() == 3 => arguments = Some(normalize(value)),
            Some(_) => {}
            // `<name> = { ... }`: an inline table apb never writes.
            None => other = true,
        }
    }
    if !found {
        McpRegistration::Absent
    } else if !other
        && command.as_deref() == Some(want_command.as_str())
        && arguments.as_deref() == Some(want_args.as_str())
    {
        McpRegistration::Same
    } else {
        McpRegistration::Different
    }
}

/// Whether the dotted key `path` lies in `server`'s table.
fn ours(path: &[String], server: &McpServer) -> bool {
    path.len() >= 2 && path[0] == SERVERS && path[1] == server.name
}

/// Every `key = value` of the config `text` with its full dotted path and
/// its value as [`normalize`] compares it, in file order.
fn values(text: &str) -> Vec<(Vec<String>, String)> {
    let mut table: Vec<String> = Vec::new();
    let mut out = Vec::new();
    for line in text.lines() {
        if let Some(path) = header_path(line) {
            table = path;
            continue;
        }
        if let Some((key, value)) = key_value(line) {
            let mut full = table.clone();
            full.extend(key);
            out.push((full, normalize(value)));
        }
    }
    out
}

/// Whether the table `[mcp_servers.<name>]` can be appended to the config
/// `text` and still be valid TOML: not when `mcp_servers` is an inline
/// table (`mcp_servers = { ... }` at the root) or an array of tables
/// (`[[mcp_servers]]`). apb then edits nothing and the person adds the
/// entry by hand.
pub fn appendable(text: &str) -> bool {
    let mut table: Vec<String> = Vec::new();
    for line in text.lines() {
        if let Some(path) = header_path(line) {
            if line.trim_start().starts_with("[[")
                && path.first().map(String::as_str) == Some(SERVERS)
            {
                return false;
            }
            table = path;
            continue;
        }
        let Some((key, _)) = key_value(line) else {
            continue;
        };
        let mut full = table.clone();
        full.extend(key);
        if full.len() == 1 && full[0] == SERVERS {
            return false;
        }
    }
    true
}

impl McpHost for CodexMcp {
    fn config_path(&self) -> Option<PathBuf> {
        source_codex_home().map(|h| h.join("config.toml"))
    }

    fn marker(&self) -> Option<PathBuf> {
        source_codex_home()
    }

    fn registration(&self, text: &str, server: &McpServer) -> McpRegistration {
        registration(text, server)
    }

    fn method(&self, _server: &McpServer) -> McpMethod {
        McpMethod::EditFile
    }

    fn insert(&self, text: Option<&str>, server: &McpServer) -> Option<(String, String)> {
        let text = text.unwrap_or("");
        if registration(text, server) != McpRegistration::Absent || !appendable(text) {
            return None;
        }
        let inserted = format!("{}{}", blank_line_before(text), block(server));
        Some((format!("{text}{inserted}"), inserted))
    }

    fn remove(
        &self,
        text: &str,
        inserted: &str,
        _created: bool,
        server: &McpServer,
    ) -> Option<String> {
        let out = cut_once(text, inserted).or_else(|| cut_once(text, &block(server)))?;
        // Every value outside apb's table keeps its table and value: a key a
        // person added under apb's table would otherwise fall under the
        // table before it once the table's lines are cut.
        let unrelated: Vec<_> = values(text)
            .into_iter()
            .filter(|(path, _)| !ours(path, server))
            .collect();
        (registration(&out, server) == McpRegistration::Absent && values(&out) == unrelated)
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
    const CONFIG: &str = "# my settings\nmodel = \"x\" # keep this comment\n\n[projects.\"/w\"]\ntrust_level = \"trusted\"\n\n[mcp_servers.other]\ncommand = \"other\"\nargs = []\n";

    #[test]
    fn the_append_keeps_every_other_byte_and_the_removal_restores_it() {
        assert_eq!(registration(CONFIG, &SERVER), McpRegistration::Absent);
        let (edited, inserted) = CodexMcp.insert(Some(CONFIG), &SERVER).unwrap();
        assert!(edited.starts_with(CONFIG));
        assert_eq!(registration(&edited, &SERVER), McpRegistration::Same);
        assert_eq!(
            CodexMcp
                .remove(&edited, &inserted, false, &SERVER)
                .as_deref(),
            Some(CONFIG)
        );
        // Content added after the entry survives its removal.
        let later = format!("{edited}\n[mcp_servers.more]\ncommand = \"m\"\n");
        let out = CodexMcp.remove(&later, &inserted, false, &SERVER).unwrap();
        assert!(out.starts_with(CONFIG) && out.contains("[mcp_servers.more]"));
        assert_eq!(registration(&out, &SERVER), McpRegistration::Absent);
        assert!(CodexMcp.insert(Some(&edited), &SERVER).is_none());
    }

    /// A key a person added under apb's table would move to the table
    /// before it once apb's lines are cut (`enabled = false` would switch
    /// another server off): such a removal is refused.
    #[test]
    fn a_removal_that_would_move_a_persons_key_is_refused() {
        let (edited, inserted) = CodexMcp.insert(Some(CONFIG), &SERVER).unwrap();
        let customized = format!("{edited}enabled = false\n");
        assert_eq!(registration(&customized, &SERVER), McpRegistration::Same);
        assert_eq!(
            CodexMcp.remove(&customized, &inserted, false, &SERVER),
            None
        );
        // A key after apb's table under a table of its own stays put.
        let later = format!("{edited}\n[tui]\nenabled = false\n");
        let out = CodexMcp.remove(&later, &inserted, false, &SERVER).unwrap();
        assert_eq!(out, format!("{CONFIG}\n[tui]\nenabled = false\n"));
    }

    #[test]
    fn entries_apb_did_not_write_are_recognized_and_kept() {
        let quoted = "[mcp_servers.\"srv\"]\ncommand = 'prog'\nargs = [ \"serve\" ]\n";
        assert_eq!(registration(quoted, &SERVER), McpRegistration::Same);
        let other = "[mcp_servers.srv]\ncommand = \"/opt/prog\"\nargs = [\"serve\"]\n";
        assert_eq!(registration(other, &SERVER), McpRegistration::Different);
        let inline = "[mcp_servers]\nsrv = { command = \"prog\", args = [\"serve\"] }\n";
        assert_eq!(registration(inline, &SERVER), McpRegistration::Different);
        let edited = format!("{CONFIG}\n{}", block(&SERVER).replace("prog", "/opt/prog"));
        assert_eq!(
            CodexMcp.remove(&edited, &format!("\n{}", block(&SERVER)), false, &SERVER),
            None
        );
        assert_eq!(CodexMcp.insert(None, &SERVER).unwrap().1, block(&SERVER));
    }

    #[test]
    fn an_inline_or_array_servers_table_is_never_appended_to() {
        for text in [
            "model = \"x\"\nmcp_servers = { other = { command = \"o\" } }\n",
            "[[mcp_servers]]\nname = \"other\"\n",
        ] {
            assert_eq!(registration(text, &SERVER), McpRegistration::Absent);
            assert!(!appendable(text), "{text}");
            assert!(CodexMcp.insert(Some(text), &SERVER).is_none(), "{text}");
        }
        // A `[mcp_servers]` table, or a key of that name in another table,
        // is fine.
        assert!(appendable("[mcp_servers]\nother = { command = \"o\" }\n"));
        assert!(appendable("[profile]\nmcp_servers = 1\n"));
        assert_eq!(CodexMcp.snippet(&SERVER), Some(block(&SERVER)));
    }
}
