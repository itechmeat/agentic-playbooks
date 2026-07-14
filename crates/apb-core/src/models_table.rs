//! Curated models table (spec 2026-07-12, section 8) plus onboarding state.
//! The table is PURELY advisory: a hint to the orchestrating agent when
//! working with profiles, with no hard binding to detection or execution.
//!
//! The built-in table is baked in from `assets/models.yaml`; a user overlay
//! `<config_dir>/models.yaml` is layered on top of it (overriding models
//! and purposes by id, plus a `subscriptions` section that the built-in
//! table doesn't have).

use serde::{Deserialize, Serialize};

/// Error loading the table: the overlay/state file is present but
/// unreadable/broken. This is NOT swallowed into a default (otherwise the
/// user's manual edit would silently get lost, and a broken file would look
/// like "no settings").
#[derive(Debug, thiserror::Error)]
pub enum ModelsError {
    #[error("overlay {0} is invalid: {1}")]
    OverlayInvalid(String, String),
    #[error("state file {0} is corrupt: {1}")]
    StateCorrupt(String, String),
    #[error("io error on {0}: {1}")]
    Io(String, String),
}

/// Model row: facts about a model (spec 8.2). All costs and flags are
/// approximate, updated via PRs. `source_url`/`checked_at` record the
/// price's provenance; `price_basis` is its basis (`list`, `estimate`,
/// `launch-until-YYYY-MM-DD` for introductory prices).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ModelRow {
    pub id: String,
    pub vendor: String,
    #[serde(default)]
    pub cost_in_usd_mtok: Option<f64>,
    #[serde(default)]
    pub cost_out_usd_mtok: Option<f64>,
    #[serde(default)]
    pub reasoning: Option<String>,
    #[serde(default)]
    pub context_tokens: Option<u64>,
    #[serde(default)]
    pub vision: bool,
    #[serde(default)]
    pub stt: bool,
    #[serde(default)]
    pub tts: bool,
    #[serde(default)]
    pub source_url: String,
    #[serde(default)]
    pub checked_at: String,
    #[serde(default)]
    pub price_basis: String,
}

/// Distinguishes "field absent from the patch" (absent) from a YAML `null`
/// for nullable fields: absent -> None (leave untouched), `null` ->
/// Some(None) (reset to unknown), a value -> Some(Some(v)) (set it). Lets
/// the overlay explicitly clear an incorrect builtin value.
fn double_option<'de, D, T>(de: D) -> Result<Option<Option<T>>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: serde::Deserialize<'de>,
{
    Ok(Some(Option::deserialize(de)?))
}

/// Partial model patch for the overlay: id is required, everything else is
/// optional. Per-field merge - setting one price does NOT reset other
/// fields to default. Nullable fields use `Option<Option<T>>` (see
/// `double_option`) to distinguish "not set" from an explicit `null`
/// (reset).
#[derive(Debug, Clone, Default, Deserialize)]
struct ModelPatch {
    id: String,
    #[serde(default)]
    vendor: Option<String>,
    #[serde(default, deserialize_with = "double_option")]
    cost_in_usd_mtok: Option<Option<f64>>,
    #[serde(default, deserialize_with = "double_option")]
    cost_out_usd_mtok: Option<Option<f64>>,
    #[serde(default, deserialize_with = "double_option")]
    reasoning: Option<Option<String>>,
    #[serde(default, deserialize_with = "double_option")]
    context_tokens: Option<Option<u64>>,
    #[serde(default)]
    vision: Option<bool>,
    #[serde(default)]
    stt: Option<bool>,
    #[serde(default)]
    tts: Option<bool>,
    #[serde(default)]
    source_url: Option<String>,
    #[serde(default)]
    checked_at: Option<String>,
    #[serde(default)]
    price_basis: Option<String>,
}

impl ModelPatch {
    /// Applies the set fields onto a row (existing or new).
    fn apply_to(self, row: &mut ModelRow) {
        if let Some(v) = self.vendor {
            row.vendor = v;
        }
        // Nullable fields: apply Some(inner) (inner may be None = reset);
        // None (field absent from the patch) - leave untouched.
        if let Some(v) = self.cost_in_usd_mtok {
            row.cost_in_usd_mtok = v;
        }
        if let Some(v) = self.cost_out_usd_mtok {
            row.cost_out_usd_mtok = v;
        }
        if let Some(v) = self.reasoning {
            row.reasoning = v;
        }
        if let Some(v) = self.context_tokens {
            row.context_tokens = v;
        }
        if let Some(v) = self.vision {
            row.vision = v;
        }
        if let Some(v) = self.stt {
            row.stt = v;
        }
        if let Some(v) = self.tts {
            row.tts = v;
        }
        if let Some(v) = self.source_url {
            row.source_url = v;
        }
        if let Some(v) = self.checked_at {
            row.checked_at = v;
        }
        if let Some(v) = self.price_basis {
            row.price_basis = v;
        }
    }
}

/// Model score for a purpose (1-10).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PurposeScore {
    pub model: String,
    pub score: u8,
}

/// A purpose (kind of work) with a list of scored models.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Purpose {
    pub id: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub scores: Vec<PurposeScore>,
}

/// How well a subscription covers a model (spec 8.4). Default is `Unknown`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Coverage {
    Full,
    Partial,
    #[default]
    Unknown,
}

/// A subscription declared by the user (from the overlay only). For
/// aggregators (opencode, pi) there may be several subscriptions - one per
/// provider.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Subscription {
    /// The agent or provider the subscription applies to.
    pub agent: String,
    #[serde(default)]
    pub plan: Option<String>,
    #[serde(default)]
    pub coverage: Coverage,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ModelsTable {
    #[serde(default)]
    pub as_of: String,
    #[serde(default)]
    pub models: Vec<ModelRow>,
    #[serde(default)]
    pub purposes: Vec<Purpose>,
    #[serde(default)]
    pub claude_static_models: Vec<String>,
    /// Populated only from the overlay (declared subscriptions).
    #[serde(default)]
    pub subscriptions: Vec<Subscription>,
}

/// Overlay: the same sections, but `models` are partial patches (per-field
/// merge).
#[derive(Debug, Clone, Default, Deserialize)]
struct ModelsOverlay {
    #[serde(default)]
    as_of: Option<String>,
    #[serde(default)]
    models: Vec<ModelPatch>,
    #[serde(default)]
    purposes: Vec<Purpose>,
    #[serde(default)]
    claude_static_models: Vec<String>,
    #[serde(default)]
    subscriptions: Vec<Subscription>,
}

const BUILTIN_YAML: &str = include_str!("../../../assets/models.yaml");

/// The built-in table. Parsing the baked-in asset must not fail at
/// runtime - this is guaranteed by the CI test `builtin_parses`.
pub fn builtin() -> ModelsTable {
    serde_yaml_ng::from_str(BUILTIN_YAML).expect("builtin models.yaml must parse")
}

/// The table with the user overlay `<config_dir>/models.yaml` applied.
/// Model patches are merged per-field by `id` (setting one price doesn't
/// reset other fields), purposes are replaced by `id`; `subscriptions` are
/// taken only from the overlay. A missing overlay - the built-in table. A
/// present but unreadable/broken overlay - an error (not a silent fallback
/// to the built-in table).
pub fn load_merged() -> Result<ModelsTable, ModelsError> {
    let mut table = builtin();
    let Some(dir) = crate::config::config_dir() else {
        return Ok(table);
    };
    let path = dir.join("models.yaml");
    let raw = match std::fs::read_to_string(&path) {
        Ok(r) => r,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(table),
        Err(e) => return Err(ModelsError::Io(path.display().to_string(), e.to_string())),
    };
    let overlay: ModelsOverlay = serde_yaml_ng::from_str(&raw)
        .map_err(|e| ModelsError::OverlayInvalid(path.display().to_string(), e.to_string()))?;
    if let Some(as_of) = overlay.as_of {
        table.as_of = as_of;
    }
    for patch in overlay.models {
        if let Some(slot) = table.models.iter_mut().find(|x| x.id == patch.id) {
            patch.apply_to(slot);
        } else {
            let mut row = ModelRow {
                id: patch.id.clone(),
                vendor: String::new(),
                cost_in_usd_mtok: None,
                cost_out_usd_mtok: None,
                reasoning: None,
                context_tokens: None,
                vision: false,
                stt: false,
                tts: false,
                source_url: String::new(),
                checked_at: String::new(),
                price_basis: String::new(),
            };
            patch.apply_to(&mut row);
            table.models.push(row);
        }
    }
    for p in overlay.purposes {
        upsert_by(&mut table.purposes, p, |x| x.id.clone());
    }
    if !overlay.claude_static_models.is_empty() {
        table.claude_static_models = overlay.claude_static_models;
    }
    table.subscriptions = overlay.subscriptions;
    Ok(table)
}

/// Replaces the element with the same key (by `key`), otherwise appends.
fn upsert_by<T, K: PartialEq>(items: &mut Vec<T>, incoming: T, key: impl Fn(&T) -> K) {
    let k = key(&incoming);
    if let Some(slot) = items.iter_mut().find(|x| key(x) == k) {
        *slot = incoming;
    } else {
        items.push(incoming);
    }
}

/// Writes the `subscriptions` section into the user overlay
/// `<config_dir>/models.yaml`, preserving other keys (models/purposes).
/// Single write source for the MCP tool and the CLI survey.
pub fn write_subscriptions(subs: &[Subscription]) -> std::io::Result<()> {
    let Some(dir) = crate::config::config_dir() else {
        return Ok(());
    };
    let path = dir.join("models.yaml");
    // Parse the existing overlay and do NOT wipe it on error (otherwise
    // we'd lose the user's manual models/purposes); a missing file - an
    // empty map.
    let mut doc: serde_yaml_ng::Value = match std::fs::read_to_string(&path) {
        Ok(raw) => serde_yaml_ng::from_str(&raw).map_err(|e| {
            std::io::Error::other(format!(
                "existing {} is not valid YAML: {e}",
                path.display()
            ))
        })?,
        // No file - an empty map; a different IO error (e.g. permissions)
        // is NOT treated as "file missing", otherwise we'd blindly overwrite
        // an inaccessible file.
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            serde_yaml_ng::Value::Mapping(Default::default())
        }
        Err(e) => return Err(e),
    };
    if !doc.is_mapping() {
        return Err(std::io::Error::other(format!(
            "existing {} is not a mapping",
            path.display()
        )));
    }
    if let Some(map) = doc.as_mapping_mut() {
        let val = serde_yaml_ng::to_value(subs).map_err(std::io::Error::other)?;
        map.insert(serde_yaml_ng::Value::from("subscriptions"), val);
    }
    let out = serde_yaml_ng::to_string(&doc).map_err(std::io::Error::other)?;
    crate::fsutil::atomic_write(&path, out.as_bytes())
}

/// State of the onboarding survey (spec 8.6). `Uninitialized` - the survey
/// hasn't been taken yet; `Configured` - subscriptions have been declared;
/// `Declined` - the user declined (don't offer it again).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OnboardingState {
    #[default]
    Uninitialized,
    Configured,
    Declined,
}

pub mod onboarding {
    use super::OnboardingState;

    #[derive(serde::Serialize, serde::Deserialize)]
    struct Stored {
        state: OnboardingState,
    }

    fn path() -> Option<std::path::PathBuf> {
        crate::config::config_dir().map(|d| d.join("state/onboarding.json"))
    }

    use super::ModelsError;

    /// Reads the state. A missing file/directory - `Uninitialized`. A
    /// present but broken file - an error (not a silent `Uninitialized`,
    /// otherwise the corruption would look like "survey not taken" and
    /// we'd offer it again, overwriting the prior decision).
    pub fn read() -> Result<OnboardingState, ModelsError> {
        let Some(p) = path() else {
            return Ok(OnboardingState::Uninitialized);
        };
        let raw = match std::fs::read_to_string(&p) {
            Ok(r) => r,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Ok(OnboardingState::Uninitialized);
            }
            Err(e) => return Err(ModelsError::Io(p.display().to_string(), e.to_string())),
        };
        serde_json::from_str::<Stored>(&raw)
            .map(|s| s.state)
            .map_err(|e| ModelsError::StateCorrupt(p.display().to_string(), e.to_string()))
    }

    /// Writes the state atomically.
    pub fn write(state: OnboardingState) -> std::io::Result<()> {
        let Some(p) = path() else {
            return Ok(());
        };
        let json = serde_json::to_vec_pretty(&Stored { state }).map_err(std::io::Error::other)?;
        crate::fsutil::atomic_write(&p, &json)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtin_parses() {
        let t = builtin();
        assert!(!t.models.is_empty());
        assert!(!t.purposes.is_empty());
        assert!(!t.claude_static_models.is_empty());
        assert!(
            t.subscriptions.is_empty(),
            "builtin carries no subscriptions"
        );
    }

    #[test]
    fn every_purpose_model_exists() {
        let t = builtin();
        for p in &t.purposes {
            for s in &p.scores {
                assert!(
                    t.models.iter().any(|m| m.id == s.model),
                    "purpose `{}` references unknown model `{}`",
                    p.id,
                    s.model
                );
                assert!(
                    (1..=10).contains(&s.score),
                    "score out of range for {}",
                    s.model
                );
            }
        }
    }

    #[test]
    fn every_row_carries_provenance() {
        let t = builtin();
        assert!(t.models.len() >= 20, "table must list 20+ models");
        for m in &t.models {
            assert!(
                !m.source_url.is_empty(),
                "model `{}` missing source_url",
                m.id
            );
            assert!(
                !m.checked_at.is_empty(),
                "model `{}` missing checked_at",
                m.id
            );
            assert!(
                !m.price_basis.is_empty(),
                "model `{}` missing price_basis",
                m.id
            );
        }
    }
}
