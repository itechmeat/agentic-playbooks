//! Turning a finished session into a playbook draft, plus the secret check
//! that keeps captured text from carrying credentials into a definition.

use std::path::Path;

use super::ToolError;
use serde_json::{Value, json};

/// The secret check of captured text: the workspace scrubber's wide refusal
/// ([`apb_core::scrub::Scrubber::refuses`]: token and keyed-value shapes, bare
/// keys, and the values installed connectors reference). Reported by kind
/// only, never by value: echoing even a prefix would put part of the value
/// into a capture report that is meant to prove the value was kept out.
/// An extra safety net, not a guarantee: the main contract is that the host
/// does not put secrets into the synopsis.
/// A structured synopsis is checked string by string
/// ([`apb_core::scrub::Scrubber::refuses_value`]), where escaping cannot hide
/// a configured value, and serialised as well, where a key name and its
/// value (`{"password": "..."}`) sit together.
fn secret_flag(root: &Path, synopsis: Option<&Value>, texts: &[&str]) -> Option<Value> {
    let scrubber = apb_core::scrub::Scrubber::for_workspace(root, Vec::new());
    synopsis
        .and_then(|v| scrubber.refuses_value(v))
        .or_else(|| texts.iter().find_map(|t| scrubber.refuses(t)))
        .map(|flag| json!({ "rejected": "secret_like_value", "match": flag.as_str() }))
}

fn normalize(s: &str) -> String {
    s.trim().to_lowercase()
}

/// Where a captured draft came from, as its provenance records it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CaptureOrigin<'a> {
    /// A host agent captured an action it just performed (`playbook_capture`).
    Agent,
    /// A person saved a suggestion of a stored discovery result: the
    /// suggestion's slug, the result's id, and whether the host rewrote
    /// the draft (the pattern still names where the playbook came from).
    Discover {
        pattern: &'a str,
        result: &'a str,
        edited: bool,
    },
}

impl CaptureOrigin<'_> {
    /// The provenance a draft of this origin is written with.
    fn provenance(&self, title: &str) -> Value {
        match self {
            CaptureOrigin::Agent => json!({ "created_by": "agent-capture", "title": title }),
            CaptureOrigin::Discover {
                pattern,
                result,
                edited,
            } => json!({
                "created_by": "discover",
                "title": title,
                "pattern": pattern,
                "result": result,
                "edited": edited,
            }),
        }
    }
}

/// One capture: the synopsis, the scope it is saved in, the draft and
/// where it came from.
#[derive(Debug, Clone, Copy)]
pub struct CaptureRequest<'a> {
    pub synopsis: &'a Value,
    pub scope: &'a str,
    pub yaml: &'a str,
    pub origin: CaptureOrigin<'a>,
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
    capture_draft(
        root,
        CaptureRequest {
            synopsis,
            scope: selected_scope,
            yaml,
            origin: CaptureOrigin::Agent,
        },
    )
}

/// The one way a draft is captured, whatever offered it: the secret check,
/// the duplicate check, the draft and its provenance.
pub fn capture_draft(root: &Path, req: CaptureRequest<'_>) -> Result<Value, ToolError> {
    let CaptureRequest {
        synopsis,
        scope: selected_scope,
        yaml,
        origin,
    } = req;
    // Secret scan over the synopsis and over the yaml (spec 8.3).
    let synopsis_text = serde_json::to_string(synopsis).unwrap_or_default();
    if let Some(rejected) = secret_flag(root, Some(synopsis), &[synopsis_text.as_str(), yaml]) {
        return Ok(rejected);
    }

    // Take the id from the yaml itself (the canonical source).
    let parsed = apb_core::schema::Playbook::from_yaml(yaml)
        .map_err(|e| ToolError::Engine(format!("invalid yaml: {e}")))?;
    let id = parsed.id.clone();

    let (_, parent) = super::scope_parent(root, selected_scope)?;

    // Dedup: a close trigger among the existing ones (both scopes). An exact
    // match of the normalized when string -> possible_duplicate.
    let catalog = crate::catalog::build(
        root,
        &crate::catalog::CatalogRequest::default(),
        Vec::new(),
        Vec::new(),
    );
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
    let store_origin = if selected_scope == "global" {
        apb_core::profile_store::PlaybookOrigin::Global
    } else {
        apb_core::profile_store::PlaybookOrigin::Project
    };
    let version = match apb_core::versioning::create_draft_in(&parent, &id, yaml, store_origin) {
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
    let provenance =
        origin.provenance(synopsis.get("title").and_then(|t| t.as_str()).unwrap_or(""));
    let _ = apb_core::fsutil::atomic_write(
        &playbook_dir.join("provenance.json"),
        provenance.to_string().as_bytes(),
    );

    Ok(json!({
        "id": id,
        "version": version,
        "scope": selected_scope,
        "lifecycle": apb_core::trust::Lifecycle::Draft.as_str(),
        "trusted": false,
        "provenance": provenance,
    }))
}

/// One `suggestion_dismiss` call. `kind` and `scope` are the raw strings the
/// tool received (`None` means the argument was absent, which is the
/// backward-compatible default: hard, project scope).
#[derive(Debug, Clone)]
pub struct DismissRequest<'a> {
    pub pattern: &'a str,
    pub synopsis: &'a str,
    pub kind: Option<&'a str>,
    pub scope: Option<&'a str>,
    /// Legacy hard-TTL override in days (v1 argument). Applies to a hard
    /// dismissal only.
    pub ttl_days: Option<u64>,
}

/// Records the user's decline of a save-as-playbook suggestion (spec
/// 2026-07-29). A soft decline escalates the snooze, a hard one silences the
/// suggestion for the hard TTL; the response reports the stored record,
/// including the server-computed `snoozed_until`, so the agent can tell the
/// user how long the silence lasts. An absent `kind`/`scope` reproduces v1
/// behavior exactly (hard, project scope).
pub fn suggestion_dismiss(root: &Path, req: DismissRequest<'_>) -> Result<Value, ToolError> {
    let kind = match req.kind {
        None => apb_core::dismiss::DecisionKind::Hard,
        Some(raw) => apb_core::dismiss::DecisionKind::parse(raw)
            .ok_or_else(|| ToolError::Engine(format!("unknown kind `{raw}` (soft or hard)")))?,
    };
    let scope = match req.scope {
        None => apb_core::dismiss::DecisionScope::Project,
        Some(raw) => apb_core::dismiss::DecisionScope::parse(raw).ok_or_else(|| {
            ToolError::Engine(format!("unknown scope `{raw}` (project or global)"))
        })?,
    };
    // A synopsis is prose the user will see again in `apb suggestions list`;
    // the same secret-shape net that guards a capture synopsis applies here.
    if let Some(rejected) = secret_flag(root, None, &[req.synopsis]) {
        return Ok(rejected);
    }
    let outcome = apb_core::dismiss::record_decision(
        root,
        apb_core::dismiss::DecisionInput {
            pattern: req.pattern.to_string(),
            synopsis: req.synopsis.to_string(),
            kind,
            scope,
            hard_ttl_days_override: req.ttl_days,
        },
    )
    .map_err(|e| ToolError::Engine(e.to_string()))?;
    let stored = outcome.record;
    let mut out = json!({
        // Kept from v1 so an existing client reading `dismissed` still works.
        "dismissed": stored.pattern,
        "pattern": stored.pattern,
        "synopsis": stored.synopsis,
        "kind": stored.kind.as_str(),
        "scope": scope.as_str(),
        "declines": stored.declines,
        "snoozed_until": apb_core::dismiss::iso_utc(stored.snoozed_until_ms),
        "snoozed_until_ms": stored.snoozed_until_ms,
    });
    // The decision itself was recorded, so this is not an error, but a broken
    // `suggestions:` config section or a store that had to be moved aside must
    // reach a human. This is the only place the agent ever sees them, so the
    // field is present only when there is something to say (an always-empty
    // array would train the agent to ignore it).
    if !outcome.diagnostics.is_empty() {
        out["diagnostics"] = json!(outcome.diagnostics);
    }
    Ok(out)
}
