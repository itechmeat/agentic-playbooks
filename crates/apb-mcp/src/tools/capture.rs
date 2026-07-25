//! Turning a finished session into a playbook draft, plus the secret scan
//! that keeps captured text from carrying credentials into a definition.

use std::path::Path;

use super::ToolError;
use serde_json::{Value, json};

/// A "looks like a secret" heuristic (spec 8.3): a crude scan without a regex crate.
/// Catches `key: value` with an indicator key and a value of length >= 8 with no
/// whitespace, as well as long (>= 32) contiguous base64/hex-like tokens.
/// Returns a masked fragment. This is an extra safety net, not a
/// guarantee: the main contract is that the host does not put secrets into the synopsis.
fn secret_like(text: &str) -> Option<String> {
    const KEYS: [&str; 6] = [
        "api_key", "apikey", "api-key", "secret", "token", "password",
    ];
    for raw in text.lines() {
        let line = raw.trim();
        let lower = line.to_lowercase();
        // Look for the indicator key anywhere in the line (robust to a JSON wrapper
        // like `"note":"api_key: ..."`), then take the value after the nearest ':'/'='.
        // We take offsets and slice using the same `lower` string throughout - otherwise on Unicode
        // (to_lowercase can change length) a byte index from `lower` could
        // point into the middle of a character in `line` and panic.
        for key in KEYS {
            let Some(kpos) = lower.find(key) else {
                continue;
            };
            let after = &lower[kpos + key.len()..];
            if let Some(sep) = after.find([':', '=']) {
                let val = after[sep + 1..].trim();
                let val: &str = val
                    .split(|c: char| {
                        c.is_whitespace() || c == '"' || c == '\'' || c == ',' || c == '}'
                    })
                    .find(|s| !s.is_empty())
                    .unwrap_or("");
                if val.len() >= 8 {
                    return Some(mask(val));
                }
            }
        }
        for tok in line.split(|c: char| c.is_whitespace() || c == '"' || c == '\'') {
            if tok.len() >= 32
                && tok
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '/' | '=' | '_' | '-'))
            {
                return Some(mask(tok));
            }
        }
    }
    None
}

/// A candidate secret is reported by shape only. Echoing even a prefix would
/// put part of the value into a capture report that is meant to prove the
/// value was kept out.
fn mask(s: &str) -> String {
    format!("redacted ({} chars)", s.chars().count())
}

fn normalize(s: &str) -> String {
    s.trim().to_lowercase()
}

/// Accepts an action synopsis and creates a draft playbook from it in the chosen
/// scope (spec 8.3). Draft: does not pass the run gate until it goes through trial
/// or explicit confirmation. Secrets and duplicates are rejected before writing.
pub fn playbook_capture(
    root: &Path,
    synopsis: &Value,
    selected_scope: &str,
    yaml: &str,
) -> Result<Value, ToolError> {
    // Secret scan over the synopsis and over the yaml (spec 8.3).
    let synopsis_text = serde_json::to_string(synopsis).unwrap_or_default();
    for src in [synopsis_text.as_str(), yaml] {
        if let Some(m) = secret_like(src) {
            return Ok(json!({ "rejected": "secret_like_value", "match": m }));
        }
    }

    // Take the id from the yaml itself (the canonical source).
    let parsed = apb_core::schema::Playbook::from_yaml(yaml)
        .map_err(|e| ToolError::Engine(format!("invalid yaml: {e}")))?;
    let id = parsed.id.clone();

    let parent = match selected_scope {
        "project" => root.join(".apb"),
        "global" => apb_core::store::global_playbooks_parent()
            .ok_or_else(|| ToolError::Engine("no global config dir".into()))?,
        other => return Err(ToolError::Engine(format!("unknown scope `{other}`"))),
    };

    // Dedup: a close trigger among the existing ones (both scopes). An exact
    // match of the normalized when string -> possible_duplicate.
    let catalog = crate::catalog::build(root, None, None, None, Vec::new());
    if let Some(entries) = catalog["entries"].as_array() {
        let new_whens: Vec<String> = synopsis
            .get("trigger")
            .and_then(|t| t.get("when"))
            .and_then(|w| w.as_array())
            .map(|a| a.iter().filter_map(|v| v.as_str().map(normalize)).collect())
            .unwrap_or_default();
        for e in entries {
            let existing: Vec<String> = e
                .get("trigger")
                .and_then(|t| t.get("when"))
                .and_then(|w| w.as_array())
                .map(|a| a.iter().filter_map(|v| v.as_str().map(normalize)).collect())
                .unwrap_or_default();
            if new_whens.iter().any(|w| existing.contains(w)) {
                return Ok(json!({ "rejected": "possible_duplicate", "candidate": e["ref"] }));
            }
        }
    }

    // Draft creation. A Conflict from core -> duplicate_id.
    let origin = if selected_scope == "global" {
        apb_core::profile_store::PlaybookOrigin::Global
    } else {
        apb_core::profile_store::PlaybookOrigin::Project
    };
    let version = match apb_core::versioning::create_draft_in(&parent, &id, yaml, origin) {
        Ok(v) => v,
        Err(apb_core::versioning::VersioningError::Conflict(_)) => {
            return Ok(json!({ "rejected": "duplicate_id", "id": id }));
        }
        Err(e) => return Err(ToolError::from(e)),
    };

    // Mark it draft and write provenance. The digest is NOT approved - capture is not a
    // local approval (spec 8.3).
    let playbook_dir = parent.join("playbooks").join(&id);
    apb_core::trust::write_lifecycle(&playbook_dir, apb_core::trust::Lifecycle::Draft)
        .map_err(|e| ToolError::Engine(e.to_string()))?;
    let provenance = json!({
        "created_by": "agent-capture",
        "title": synopsis.get("title").and_then(|t| t.as_str()).unwrap_or(""),
    });
    let _ = apb_core::fsutil::atomic_write(
        &playbook_dir.join("provenance.json"),
        provenance.to_string().as_bytes(),
    );

    Ok(json!({
        "id": id,
        "version": version,
        "scope": selected_scope,
        "lifecycle": "draft",
        "trusted": false,
        "provenance": provenance,
    }))
}

/// Records the user's decline of the suggestion to save a playbook (spec 8.2):
/// "no, and don't suggest that again". The pattern is an English kebab-slug.
pub fn suggestion_dismiss(pattern: &str, ttl_days: Option<u64>) -> Result<Value, ToolError> {
    apb_core::dismiss::record(pattern, ttl_days).map_err(|e| ToolError::Engine(e.to_string()))?;
    Ok(json!({ "dismissed": pattern }))
}
