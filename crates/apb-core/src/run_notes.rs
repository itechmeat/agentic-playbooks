//! Findings and warnings an attempt reports next to its result: problems
//! it saw but did not fix, and things a person should know. They are
//! reported, never acted on: they do not change a node's status, its
//! outputs, the edges, the run context or any completion check.
//!
//! Both arrive as untrusted JSON (a status file, a host submission), so
//! [`sanitize`] keeps only well-formed entries, drops the rest whole and
//! counts them. Nothing is truncated: a text over its limit drops the entry.

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// The most findings, and separately warnings, one attempt reports.
pub const MAX_ENTRIES: usize = 20;
/// The most drop reasons kept.
pub const MAX_REASONS: usize = 5;
/// The longest `kind` slug.
pub const KIND_MAX: usize = 40;
pub const LOCATION_MAX: usize = 200;
pub const EVIDENCE_MAX: usize = 1000;
pub const WHY_NOT_FIXED_MAX: usize = 500;
pub const WORKAROUND_MAX: usize = 500;
pub const SUBJECT_MAX: usize = 200;
pub const REASON_MAX: usize = 500;
/// The kind of a problem with a tool the run used (a connector, an MCP
/// tool, a program, a skill): its location names the tool in the grammar of
/// [`crate::known_problems::scope_of`].
pub const TOOL_PROBLEM: &str = "tool_problem";
/// The kinds the docs recommend; any slug is accepted.
pub const RECOMMENDED_KINDS: [&str; 11] = [
    "bug",
    "regression",
    "security",
    "data_quality",
    "config",
    "docs",
    "test_gap",
    "performance",
    "dependency",
    TOOL_PROBLEM,
    "other",
];

/// A problem an attempt saw and did not fix.
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Finding {
    pub kind: String,
    pub location: String,
    pub evidence: String,
    pub why_not_fixed: String,
    /// How the attempt got around the problem, when it did.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub workaround: Option<String>,
}

/// Something a person should know about the attempt's result.
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NoteWarning {
    pub subject: String,
    pub reason: String,
}

/// The well-formed entries.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Notes {
    #[serde(default)]
    pub findings: Vec<Finding>,
    #[serde(default)]
    pub warnings: Vec<NoteWarning>,
}

impl Notes {
    pub fn is_empty(&self) -> bool {
        self.findings.is_empty() && self.warnings.is_empty()
    }
}

/// Entries a reader dropped as malformed, and why (at most
/// [`MAX_REASONS`] reasons).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DroppedNotes {
    pub count: u32,
    pub reasons: Vec<String>,
}

/// What [`sanitize`] kept and what it dropped.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SanitizedNotes {
    pub notes: Notes,
    pub dropped: u32,
    /// Why entries were dropped, deduplicated, at most [`MAX_REASONS`].
    pub reasons: Vec<String>,
}

impl SanitizedNotes {
    /// Nothing kept and nothing dropped.
    pub fn is_empty(&self) -> bool {
        self.notes.is_empty() && self.dropped == 0
    }

    /// Counts the entries another reader already dropped, with its reasons
    /// (kept to [`MAX_REASONS`] like the rest).
    pub fn absorb(&mut self, other: &DroppedNotes) {
        self.dropped = self.dropped.saturating_add(other.count);
        for r in &other.reasons {
            if self.reasons.len() < MAX_REASONS && !self.reasons.contains(r) {
                self.reasons.push(r.clone());
            }
        }
    }

    fn drop_entry(&mut self, reason: String) {
        self.dropped = self.dropped.saturating_add(1);
        if self.reasons.len() < MAX_REASONS && !self.reasons.contains(&reason) {
            self.reasons.push(reason);
        }
    }
}

/// One string field of an entry: present, a non-empty string after trim,
/// within `max` characters, no control or invisible format character
/// ([`crate::text::has_hidden`]).
fn text(entry: &Value, field: &str, max: usize) -> Result<String, String> {
    let v = entry
        .get(field)
        .ok_or_else(|| format!("`{field}` is missing"))?;
    let s = v
        .as_str()
        .ok_or_else(|| format!("`{field}` is not a string"))?
        .trim();
    if s.is_empty() {
        return Err(format!("`{field}` is empty"));
    }
    if s.chars().count() > max {
        return Err(format!("`{field}` is longer than {max} characters"));
    }
    if crate::text::has_hidden(s) {
        return Err(format!("`{field}` contains a control or format character"));
    }
    Ok(s.to_string())
}

fn is_kind_slug(s: &str) -> bool {
    (1..=KIND_MAX).contains(&s.len())
        && s.chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' || c == '-')
}

fn finding(entry: &Value) -> Result<Finding, String> {
    if !entry.is_object() {
        return Err("a finding is not an object".into());
    }
    let kind = text(entry, "kind", KIND_MAX)?;
    if !is_kind_slug(&kind) {
        return Err("`kind` is not a lowercase slug".into());
    }
    Ok(Finding {
        kind,
        location: text(entry, "location", LOCATION_MAX)?,
        evidence: text(entry, "evidence", EVIDENCE_MAX)?,
        why_not_fixed: text(entry, "why_not_fixed", WHY_NOT_FIXED_MAX)?,
        // Optional: absent, null and a blank string all mean "none".
        workaround: match entry.get("workaround") {
            None | Some(Value::Null) => None,
            Some(Value::String(w)) if w.trim().is_empty() => None,
            Some(_) => Some(text(entry, "workaround", WORKAROUND_MAX)?),
        },
    })
}

fn warning(entry: &Value) -> Result<NoteWarning, String> {
    if !entry.is_object() {
        return Err("a warning is not an object".into());
    }
    Ok(NoteWarning {
        subject: text(entry, "subject", SUBJECT_MAX)?,
        reason: text(entry, "reason", REASON_MAX)?,
    })
}

/// Keeps the well-formed entries of one list, up to [`MAX_ENTRIES`].
fn keep<T>(
    list: &Value,
    what: &str,
    parse: fn(&Value) -> Result<T, String>,
    out: &mut SanitizedNotes,
) -> Vec<T> {
    let items = match list {
        Value::Null => return Vec::new(),
        Value::Array(items) => items,
        _ => {
            out.drop_entry(format!("{what} is not a list"));
            return Vec::new();
        }
    };
    let mut kept = Vec::new();
    for item in items {
        if kept.len() == MAX_ENTRIES {
            out.drop_entry(format!("more than {MAX_ENTRIES} {what}"));
            continue;
        }
        match parse(item) {
            Ok(v) => kept.push(v),
            Err(why) => out.drop_entry(format!("{what}: {why}")),
        }
    }
    kept
}

/// Keeps the well-formed findings and warnings, and counts the rest.
pub fn sanitize(findings: &Value, warnings: &Value) -> SanitizedNotes {
    let mut out = SanitizedNotes::default();
    out.notes.findings = keep(findings, "findings", finding, &mut out);
    out.notes.warnings = keep(warnings, "warnings", warning, &mut out);
    out
}

/// A location for grouping across runs: trimmed, without a trailing
/// `:line` or `:line:col`.
pub fn location_key(location: &str) -> String {
    strip_line(location.trim()).to_string()
}

/// `name` without up to two trailing `:<digits>` segments (a line and a
/// column): the one rule the grouping key and a known problem's scope name
/// share.
pub fn strip_line(name: &str) -> &str {
    let mut name = name;
    for _ in 0..2 {
        match name.rsplit_once(':') {
            Some((head, tail))
                if !head.is_empty()
                    && !tail.is_empty()
                    && tail.chars().all(|c| c.is_ascii_digit()) =>
            {
                name = head;
            }
            _ => break,
        }
    }
    name
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn f(kind: &str) -> Value {
        json!({"kind": kind, "location": "src/a.rs:3", "evidence": "e", "why_not_fixed": "out of scope"})
    }

    #[test]
    fn keeps_well_formed_entries() {
        let s = sanitize(
            &json!([f("bug")]),
            &json!([{"subject": "deploy", "reason": "not verified"}]),
        );
        assert_eq!(s.notes.findings.len(), 1);
        assert_eq!(s.notes.warnings[0].subject, "deploy");
        assert_eq!(s.dropped, 0);
    }

    #[test]
    fn more_than_twenty_entries_drop_the_extra_ones() {
        let many: Vec<Value> = (0..21).map(|_| f("bug")).collect();
        let s = sanitize(&Value::Array(many), &Value::Null);
        assert_eq!(s.notes.findings.len(), MAX_ENTRIES);
        assert_eq!(s.dropped, 1);
        assert_eq!(s.reasons, ["more than 20 findings"]);
    }

    #[test]
    fn an_over_length_control_or_bidi_text_drops_only_that_entry() {
        let long = json!({"kind": "bug", "location": "x", "evidence": "e".repeat(EVIDENCE_MAX + 1), "why_not_fixed": "w"});
        let control =
            json!({"kind": "bug", "location": "a\u{7}b", "evidence": "e", "why_not_fixed": "w"});
        let bidi = json!({"kind": "bug", "location": "a.rs\u{202E}sr.b", "evidence": "e", "why_not_fixed": "w"});
        let s = sanitize(&json!([long, f("bug"), control, bidi]), &Value::Null);
        assert_eq!(s.notes.findings.len(), 1);
        assert_eq!(s.dropped, 3);
        // Nothing is cut to fit.
        assert_eq!(s.notes.findings[0].location, "src/a.rs:3");
    }

    #[test]
    fn wrong_types_are_dropped_and_counted() {
        let s = sanitize(
            &json!(["text", {"kind": "Bad Kind", "location": "x", "evidence": "e", "why_not_fixed": "w"}, {"kind": "bug", "location": 3, "evidence": "e", "why_not_fixed": "w"}]),
            &json!({"subject": "not a list"}),
        );
        assert!(s.notes.is_empty());
        assert_eq!(s.dropped, 4);
        assert!(s.reasons.contains(&"warnings is not a list".to_string()));
        assert!(s.reasons.len() <= MAX_REASONS);
    }

    #[test]
    fn a_workaround_is_kept_and_a_malformed_one_drops_the_entry() {
        let with = json!({"kind": "tool_problem", "location": "connector:crm.update_contact", "evidence": "429 three times", "why_not_fixed": "not ours", "workaround": "wait 60 s and retry once"});
        let bad = json!({"kind": "tool_problem", "location": "cli:make", "evidence": "e", "why_not_fixed": "w", "workaround": 3});
        let s = sanitize(&json!([with, bad, f("bug")]), &Value::Null);
        assert_eq!(s.notes.findings.len(), 2);
        assert_eq!(
            s.notes.findings[0].workaround.as_deref(),
            Some("wait 60 s and retry once")
        );
        assert_eq!(s.notes.findings[1].workaround, None);
        assert_eq!(s.dropped, 1);
        // An entry without the field serializes as before.
        let plain = serde_json::to_string(&s.notes.findings[1]).unwrap();
        assert!(!plain.contains("workaround"), "{plain}");
        // A blank workaround is none: the finding stays.
        let blank = json!({"kind": "tool_problem", "location": "cli:make", "evidence": "exits 2", "why_not_fixed": "w", "workaround": "  "});
        let s = sanitize(&json!([blank]), &Value::Null);
        assert_eq!((s.notes.findings.len(), s.dropped), (1, 0));
        assert_eq!(s.notes.findings[0].workaround, None);
    }

    #[test]
    fn location_key_drops_a_trailing_line_and_column() {
        assert_eq!(location_key(" src/a.rs:12:4 "), "src/a.rs");
        assert_eq!(location_key("src/a.rs:12"), "src/a.rs");
        assert_eq!(location_key("deal 42"), "deal 42");
        assert_eq!(location_key("https://x.test/p"), "https://x.test/p");
    }
}
