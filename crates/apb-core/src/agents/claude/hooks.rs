//! Claude Code's Stop hook: the `hooks.Stop` list of its user settings
//! (`$CLAUDE_CONFIG_DIR/settings.json`, else `~/.claude/settings.json`),
//! each item a group `{ "hooks": [{ "type": "command", "command": ... }] }`.
//! The hook reads `{ session_id, transcript_path, cwd, stop_hook_active }`
//! on stdin and keeps the turn going by printing
//! `{ "decision": "block", "reason": ... }`; the reason is shown to the
//! agent. apb inserts its group as text right after the opening bracket or
//! brace it belongs in, so every other key keeps its place and spelling;
//! the result must parse to the old document plus the group, else nothing
//! is inserted. Removal cuts exactly the inserted bytes; when the host
//! rewrote the file since, it cuts the group from the text it finds, but
//! only while the group is still exactly apb's.

use std::path::PathBuf;

use serde_json::{Value, json};

use crate::agents::hook_host::{HookRegistration, StopEvent, StopHookHost};
use crate::discover::HistoryAgent;

/// The hook event apb registers for.
const EVENT: &str = "Stop";
/// How long Claude Code lets the hook run, in seconds.
const TIMEOUT_SECONDS: u64 = 30;

pub struct ClaudeStopHook;

/// The group apb inserts for `command`.
fn group(command: &str) -> Value {
    json!({ "hooks": [{ "type": "command", "command": command, "timeout": TIMEOUT_SECONDS }] })
}

/// The group as the one line of text apb inserts, keys in a fixed order.
fn group_line(command: &str) -> String {
    format!(
        "{{\"hooks\": [{{\"type\": \"command\", \"command\": {}, \"timeout\": {TIMEOUT_SECONDS}}}]}}",
        Value::from(command)
    )
}

/// Whether `cmd` runs apb's Stop hook in some form (another path to apb,
/// a quoted path, `apb.exe`, extra options).
fn is_apb_stop(cmd: &str) -> bool {
    let cmd = cmd.trim_start();
    let (program, rest) = match cmd.chars().next() {
        Some(q @ ('"' | '\'')) => match cmd[1..].split_once(q) {
            Some((program, rest)) => (program, rest),
            None => return false,
        },
        _ => cmd.split_once(char::is_whitespace).unwrap_or((cmd, "")),
    };
    let name = program
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or("")
        .to_ascii_lowercase();
    let words: Vec<&str> = rest.split_whitespace().collect();
    (name == "apb" || name == "apb.exe") && words.starts_with(&["hook", "stop"])
}

/// Whether `command` is in the parsed settings `doc`.
fn registration_in(doc: &Value, command: &str) -> HookRegistration {
    let Some(obj) = doc.as_object() else {
        return HookRegistration::Unreadable;
    };
    let Some(hooks) = obj.get("hooks") else {
        return HookRegistration::Absent;
    };
    let Some(hooks) = hooks.as_object() else {
        return HookRegistration::Unreadable;
    };
    let Some(stop) = hooks.get(EVENT) else {
        return HookRegistration::Absent;
    };
    let Some(groups) = stop.as_array() else {
        return HookRegistration::Unreadable;
    };
    let commands: Vec<&str> = groups
        .iter()
        .filter_map(|g| g.get("hooks").and_then(Value::as_array))
        .flatten()
        .filter_map(|h| h.get("command").and_then(Value::as_str))
        .collect();
    if commands.contains(&command) {
        HookRegistration::Same
    } else if commands.iter().any(|c| is_apb_stop(c)) {
        HookRegistration::Different
    } else {
        HookRegistration::Absent
    }
}

/// The byte offset just past the `open` (`{` or `[`) that starts the value
/// of the member `key` of the object whose members start at `from` (just
/// past its `{`). `text` is valid JSON.
fn member_open(text: &str, from: usize, key: &str, open: u8) -> Option<usize> {
    let bytes = text.as_bytes();
    let mut depth = 0usize;
    let mut i = from;
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
            b @ (b'{' | b'[') => {
                if depth == 0
                    && b == open
                    && let Some((s, e)) = last_string
                    && &text[s..e] == key
                    && text[e + 1..i].trim() == ":"
                {
                    return Some(i + 1);
                }
                depth += 1;
                last_string = None;
            }
            b'}' | b']' => {
                if depth == 0 {
                    return None;
                }
                depth -= 1;
                last_string = None;
            }
            b',' => last_string = None,
            _ => {}
        }
        i += 1;
    }
    None
}

/// Whether the container opened just before `at` is empty.
fn empty_at(text: &str, at: usize) -> bool {
    let rest = text[at..].trim_start();
    rest.starts_with('}') || rest.starts_with(']')
}

/// Where the group goes into `text` and the bytes that go there.
fn insertion(text: &str, command: &str) -> Option<(usize, String)> {
    let line = group_line(command);
    let doc = text.find('{')? + 1;
    if let Some(hooks) = member_open(text, doc, "hooks", b'{') {
        if let Some(stop) = member_open(text, hooks, EVENT, b'[') {
            let tail = if empty_at(text, stop) { "\n    " } else { "," };
            return Some((stop, format!("\n      {line}{tail}")));
        }
        let tail = if empty_at(text, hooks) { "\n  " } else { "," };
        return Some((
            hooks,
            format!("\n    \"{EVENT}\": [\n      {line}\n    ]{tail}"),
        ));
    }
    let tail = if empty_at(text, doc) { "\n" } else { "," };
    Some((
        doc,
        format!("\n  \"hooks\": {{\n    \"{EVENT}\": [\n      {line}\n    ]\n  }}{tail}"),
    ))
}

/// The end of the JSON value that starts at `from` in `text` (valid JSON):
/// the offset of the `,` or closing bracket that follows it.
fn value_end(text: &str, from: usize) -> usize {
    let bytes = text.as_bytes();
    let mut depth = 0usize;
    let mut i = from;
    while i < bytes.len() {
        match bytes[i] {
            b'"' => {
                i += 1;
                while i < bytes.len() && bytes[i] != b'"' {
                    if bytes[i] == b'\\' {
                        i += 1;
                    }
                    i += 1;
                }
            }
            b'{' | b'[' => depth += 1,
            b'}' | b']' if depth == 0 => return i,
            b'}' | b']' => depth -= 1,
            b',' if depth == 0 => return i,
            _ => {}
        }
        i += 1;
    }
    i
}

/// `text` without the first element of `hooks.Stop` that is exactly apb's
/// group for `command`, cut with its comma; the rest keeps its bytes.
fn cut_group(text: &str, command: &str) -> Option<String> {
    let doc = text.find('{')? + 1;
    let hooks = member_open(text, doc, "hooks", b'{')?;
    let stop = member_open(text, hooks, EVENT, b'[')?;
    let want = group(command);
    let mut at = stop;
    let mut prev_comma: Option<usize> = None;
    loop {
        let start = at + (text[at..].len() - text[at..].trim_start().len());
        if text[start..].starts_with(']') {
            return None;
        }
        let end = value_end(text, start);
        let element: Value = serde_json::from_str(&text[start..end]).ok()?;
        let next_comma = text[end..].starts_with(',').then_some(end);
        if element == want {
            let (cut_from, cut_to) = match (next_comma, prev_comma) {
                (Some(c), _) => (start, c + 1),
                (None, Some(p)) => (p, end),
                (None, None) => (start, end),
            };
            let mut out = text.to_string();
            out.replace_range(cut_from..cut_to, "");
            return Some(out);
        }
        at = next_comma? + 1;
        prev_comma = next_comma;
    }
}

/// `doc` with apb's group first in `hooks.Stop`.
fn with_group(doc: &Value, command: &str) -> Option<Value> {
    let mut out = doc.clone();
    let stop = out
        .as_object_mut()?
        .entry("hooks")
        .or_insert_with(|| json!({}))
        .as_object_mut()?
        .entry(EVENT)
        .or_insert_with(|| json!([]))
        .as_array_mut()?;
    stop.insert(0, group(command));
    Some(out)
}

/// `doc` without apb's group, and without `Stop` and `hooks` when they are
/// left empty and `after` has none.
fn without_group(doc: &Value, after: &Value, command: &str) -> Option<Value> {
    let mut out = doc.clone();
    let obj = out.as_object_mut()?;
    let hooks = obj.get_mut("hooks")?.as_object_mut()?;
    let stop = hooks.get_mut(EVENT)?.as_array_mut()?;
    let at = stop.iter().position(|g| g == &group(command))?;
    stop.remove(at);
    if stop.is_empty() && after.get("hooks").and_then(|h| h.get(EVENT)).is_none() {
        hooks.remove(EVENT);
    }
    if hooks.is_empty() && after.get("hooks").is_none() {
        obj.remove("hooks");
    }
    Some(out)
}

impl StopHookHost for ClaudeStopHook {
    fn settings_path(&self) -> Option<PathBuf> {
        super::actions::config_dir().map(|d| d.join("settings.json"))
    }

    fn registration(&self, text: &str, command: &str) -> HookRegistration {
        match serde_json::from_str::<Value>(text) {
            Ok(doc) => registration_in(&doc, command),
            Err(_) => HookRegistration::Unreadable,
        }
    }

    fn insert(&self, text: Option<&str>, command: &str) -> Option<(String, String)> {
        let Some(text) = text else {
            let body = format!(
                "{{\n  \"hooks\": {{\n    \"{EVENT}\": [\n      {}\n    ]\n  }}\n}}\n",
                group_line(command)
            );
            return Some((body.clone(), body));
        };
        let doc: Value = serde_json::from_str(text).ok()?;
        if registration_in(&doc, command) != HookRegistration::Absent {
            return None;
        }
        let (at, inserted) = insertion(text, command)?;
        let mut out = text.to_string();
        out.insert_str(at, &inserted);
        let parsed: Value = serde_json::from_str(&out).ok()?;
        (Some(parsed) == with_group(&doc, command)).then_some((out, inserted))
    }

    fn remove(&self, text: &str, inserted: &str, created: bool, command: &str) -> Option<String> {
        if created && text == inserted {
            return Some(String::new());
        }
        let doc: Value = serde_json::from_str(text).ok()?;
        if registration_in(&doc, command) != HookRegistration::Same {
            return None;
        }
        let line = group_line(command);
        let needles = [
            inserted.to_string(),
            format!("\n      {line},"),
            format!("\n      {line}"),
        ];
        let exact = needles.iter().find_map(|needle| {
            let out = crate::agents::mcp_host::cut_once(text, needle)?;
            let after: Value = serde_json::from_str(&out).ok()?;
            (Some(&after) == without_group(&doc, &after, command).as_ref()).then_some(out)
        });
        // The host rewrote the file (re-indented, keys added): the group is
        // cut from the parsed text only while it is still exactly apb's.
        exact.or_else(|| {
            let out = cut_group(text, command)?;
            let after: Value = serde_json::from_str(&out).ok()?;
            (Some(&after) == without_group(&doc, &after, command).as_ref()).then_some(out)
        })
    }

    fn read_event(&self, stdin: &[u8]) -> Option<StopEvent> {
        let v: Value = serde_json::from_slice(stdin).ok()?;
        let s = |k: &str| v.get(k).and_then(Value::as_str).filter(|s| !s.is_empty());
        Some(StopEvent {
            session_id: s("session_id")?.to_string(),
            transcript: PathBuf::from(s("transcript_path")?),
            cwd: s("cwd").map(PathBuf::from),
            hook_active: v.get("stop_hook_active").and_then(Value::as_bool) == Some(true),
        })
    }

    fn block(&self, reason: &str) -> String {
        json!({ "decision": "block", "reason": reason }).to_string()
    }

    fn history(&self) -> HistoryAgent {
        HistoryAgent::ClaudeCode
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const CMD: &str = "apb hook stop";

    #[test]
    fn the_insert_keeps_other_keys_in_place_and_the_removal_restores_the_text() {
        let cases = [
            "{\n  \"model\": \"x\",\n  \"permissions\": {\"allow\": [\"Bash(ls)\"]}\n}\n",
            "{\n  \"hooks\": {\n    \"PreToolUse\": [{\"matcher\": \"Bash\", \"hooks\": []}]\n  },\n  \"z\": \"a {brace} \\\" [x]\"\n}\n",
            "{\"hooks\": {\"Stop\": [{\"hooks\": [{\"type\": \"command\", \"command\": \"other\"}]}]}}",
            "{\"hooks\": {\"Stop\": []}}",
            "{\"hooks\": {}}",
            "{}",
        ];
        for text in cases {
            let before: Value = serde_json::from_str(text).unwrap();
            let (edited, inserted) = ClaudeStopHook.insert(Some(text), CMD).unwrap();
            let after: Value = serde_json::from_str(&edited).unwrap();
            assert_eq!(registration_in(&after, CMD), HookRegistration::Same);
            assert_eq!(after["hooks"]["Stop"][0], group(CMD), "{edited}");
            let mut rest = after.clone();
            rest["hooks"]["Stop"].as_array_mut().unwrap().remove(0);
            let mut expected = before.clone();
            if !expected["hooks"]["Stop"].is_array() {
                expected["hooks"]["Stop"] = json!([]);
            }
            assert_eq!(rest, expected, "nothing else changed: {edited}");
            assert_eq!(
                ClaudeStopHook
                    .remove(&edited, &inserted, false, CMD)
                    .as_deref(),
                Some(text),
                "{edited}"
            );
        }
    }

    #[test]
    fn a_created_file_goes_and_an_edited_entry_stays() {
        let (body, inserted) = ClaudeStopHook.insert(None, CMD).unwrap();
        assert_eq!(
            ClaudeStopHook.registration(&body, CMD),
            HookRegistration::Same
        );
        assert_eq!(
            ClaudeStopHook
                .remove(&body, &inserted, true, CMD)
                .as_deref(),
            Some("")
        );
        let edited = body.replace("\"timeout\": 30", "\"timeout\": 5");
        assert_eq!(ClaudeStopHook.remove(&edited, &inserted, true, CMD), None);
    }

    /// The host rewrote the file after setup (re-indented, another key
    /// added): the group is taken out of the parsed text when it is still
    /// exactly apb's, and left when anything in it changed.
    #[test]
    fn a_rewritten_file_loses_only_an_unchanged_group() {
        let text = "{\n  \"model\": \"x\"\n}\n";
        let (edited, inserted) = ClaudeStopHook.insert(Some(text), CMD).unwrap();
        let mut doc: Value = serde_json::from_str(&edited).unwrap();
        doc["theme"] = json!("dark");
        doc["hooks"]["Stop"]
            .as_array_mut()
            .unwrap()
            .push(json!({ "hooks": [{ "type": "command", "command": "other" }] }));
        let rewritten = serde_json::to_string_pretty(&doc).unwrap();
        let out = ClaudeStopHook
            .remove(&rewritten, &inserted, false, CMD)
            .expect("an unchanged group is removed");
        let out: Value = serde_json::from_str(&out).unwrap();
        assert_eq!(
            out,
            json!({
                "model": "x",
                "theme": "dark",
                "hooks": { "Stop": [{ "hooks": [{ "type": "command", "command": "other" }] }] }
            })
        );
        let mut changed = doc.clone();
        changed["hooks"]["Stop"][0]["hooks"][0]["timeout"] = json!(5);
        let changed = serde_json::to_string_pretty(&changed).unwrap();
        assert_eq!(ClaudeStopHook.remove(&changed, &inserted, false, CMD), None);
        let mut extra = doc;
        extra["hooks"]["Stop"][0]["matcher"] = json!("x");
        let extra = serde_json::to_string_pretty(&extra).unwrap();
        assert_eq!(ClaudeStopHook.remove(&extra, &inserted, false, CMD), None);
    }

    #[test]
    fn another_form_of_the_hook_is_different_and_a_strange_shape_unreadable() {
        let other = r#"{"hooks":{"Stop":[{"hooks":[{"type":"command","command":"/opt/bin/apb hook stop"}]}]}}"#;
        assert_eq!(
            ClaudeStopHook.registration(other, CMD),
            HookRegistration::Different
        );
        assert_eq!(ClaudeStopHook.insert(Some(other), CMD), None);
        for cmd in [
            "C:\\tools\\APB.exe hook stop",
            "\"/opt/my tools/apb\" hook stop",
        ] {
            let text = json!({ "hooks": { "Stop": [{ "hooks": [{ "type": "command", "command": cmd }] }] } })
                .to_string();
            assert_eq!(
                ClaudeStopHook.registration(&text, CMD),
                HookRegistration::Different,
                "{cmd}"
            );
        }
        // A duplicate or escaped `hooks` key: the text edit would not parse
        // to the document plus the group, so nothing is inserted.
        for text in [
            r#"{"hooks": {}, "hooks": {"PreToolUse": []}}"#,
            r#"{"ho\u006fks": {"PreToolUse": []}}"#,
        ] {
            assert_eq!(ClaudeStopHook.insert(Some(text), CMD), None, "{text}");
        }
        for text in [r#"{"hooks":{"Stop":{}}}"#, r#"{"hooks":[]}"#, "[", "[]"] {
            assert_eq!(
                ClaudeStopHook.registration(text, CMD),
                HookRegistration::Unreadable,
                "{text}"
            );
        }
    }

    #[test]
    fn the_event_and_the_answer_have_the_host_shape() {
        let ev = ClaudeStopHook
            .read_event(br#"{"session_id":"s1","transcript_path":"/t/s1.jsonl","cwd":"/p","hook_event_name":"Stop","stop_hook_active":true}"#)
            .unwrap();
        assert_eq!(ev.session_id, "s1");
        assert_eq!(ev.transcript, PathBuf::from("/t/s1.jsonl"));
        assert_eq!(ev.cwd, Some(PathBuf::from("/p")));
        assert!(ev.hook_active);
        assert_eq!(ClaudeStopHook.read_event(b"{}"), None);
        assert_eq!(ClaudeStopHook.read_event(b"not json"), None);
        let out: Value = serde_json::from_str(&ClaudeStopHook.block("why")).unwrap();
        assert_eq!(out, json!({ "decision": "block", "reason": "why" }));
    }
}
