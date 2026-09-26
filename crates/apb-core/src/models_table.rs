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
    /// The model ids codex accepts, user-visible ones only (detection's
    /// Static authority list for codex, and the profile editor's option set).
    #[serde(default)]
    pub codex_static_models: Vec<String>,
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
    codex_static_models: Vec<String>,
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
    if !overlay.codex_static_models.is_empty() {
        table.codex_static_models = overlay.codex_static_models;
    }
    table.subscriptions = overlay.subscriptions;
    Ok(table)
}

/// Vendor a known single-vendor agent is tied to (issue #42 finding 9): used
/// to narrow the curated table to that vendor's rows for the profile
/// editor's model selector. An aggregator (opencode, pi, agy, hermes, cursor,
/// qoder) or an unrecognized agent id has no entry here and keeps the whole
/// table - it is not pinned to one vendor. The legacy `claude-code` id (still found in
/// profiles saved before the agent id was renamed) resolves to the same
/// vendor as `claude`.
pub fn agent_vendor(agent: &str) -> Option<&'static str> {
    match crate::detect::canonical_agent_id(agent) {
        "claude" => Some("anthropic"),
        "codex" => Some("openai"),
        "grok" => Some("xai"),
        "zcode" => Some("zhipu"),
        _ => None,
    }
}

/// The closed, apb-owned model list of `agent`, or `None` when the agent
/// has none and its options come from the curated table instead.
///
/// This is the ONE definition of "the models apb offers for this agent":
/// detection reports it as the agent's `Static` inventory
/// (`agent_catalog::assemble`) and the profile editor offers exactly
/// it ([`model_options_for_agent`]), so the two can never disagree.
/// - zcode: apb's allowlist ([`crate::zcode::model_list`]), a Rust constant
///   because the spawn path enforces it;
/// - claude (and the legacy `claude-code` id): `claude_static_models`;
/// - codex: `codex_static_models`.
///
/// An empty table list (an overlay may clear one) means no closed list.
pub fn static_models_for_agent(agent: &str, table: &ModelsTable) -> Option<Vec<String>> {
    let list = match crate::detect::canonical_agent_id(agent) {
        a if a == crate::zcode::AGENT_ID => crate::zcode::model_list(),
        "claude" => table.claude_static_models.clone(),
        "codex" => table.codex_static_models.clone(),
        _ => return None,
    };
    (!list.is_empty()).then_some(list)
}

/// Whether `agent`'s option set is a closed apb-side list
/// ([`static_models_for_agent`]) rather than the curated table filtered to a
/// vendor. For such an agent a detected item outside the list must NOT be
/// appended as an option: the list is exactly what apb supports.
pub fn agent_models_are_closed_list(agent: &str, table: &ModelsTable) -> bool {
    static_models_for_agent(agent, table).is_some()
}

/// A short, stable digest of the models data baked into this binary
/// (`assets/models.yaml`). Part of the detection memo's build id, so a binary
/// carrying different model data never reuses a memo written by another.
pub fn builtin_digest() -> String {
    use sha2::{Digest, Sha256};
    let d = Sha256::digest(BUILTIN_YAML.as_bytes());
    d.iter().take(8).map(|b| format!("{b:02x}")).collect()
}

/// One model choice offered for a specific agent in the profile editor
/// (issue #42 finding 9). The curated table (or, for zcode and codex, a
/// closed apb list) drives the option SET; detection
/// only annotates it - `detected` marks a curated row also named by the
/// agent's local config/detected model list, and never limits which rows are
/// offered.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ModelOption {
    pub id: String,
    pub vendor: String,
    pub detected: bool,
}

/// Builds `agent`'s model option list.
///
/// The option SET, in offer order:
/// - an agent with a closed apb list ([`static_models_for_agent`]: zcode,
///   claude, codex): exactly that list, in its order (the first entry is the
///   default). The curated zhipu rows would offer models the zcode allowlist
///   refuses.
/// - otherwise: the curated rows tied to the agent's vendor (or every row
///   for an aggregator/unrecognized agent, which is not pinned to a single
///   vendor).
///
/// Each entry is annotated `detected` when `detected_items` (the agent's
/// local config/detected model list, e.g. `~/.codex/config.toml`'s `model`
/// line) also names it. On the vendor path a detected item absent from the
/// curated set is appended as its own `detected`-only entry, so a model the
/// agent reports but the curated table does not carry yet is never hidden; on
/// a closed list ([`agent_models_are_closed_list`]) it is not - the list is
/// exactly what apb supports.
pub fn model_options_for_agent(
    agent: &str,
    detected_items: &[String],
    table: &ModelsTable,
) -> Vec<ModelOption> {
    let vendor = agent_vendor(agent);
    let agent_vendor_str = vendor.unwrap_or_default().to_string();
    // (id, vendor) pairs of the option set, in offer order.
    let set: Vec<(String, String)> = match static_models_for_agent(agent, table) {
        Some(list) => list
            .into_iter()
            .map(|id| (id, agent_vendor_str.clone()))
            .collect(),
        None => table
            .models
            .iter()
            .filter(|m| vendor.is_none_or(|v| m.vendor == v))
            .map(|m| (m.id.clone(), m.vendor.clone()))
            .collect(),
    };
    let detected_set: std::collections::BTreeSet<&str> =
        detected_items.iter().map(String::as_str).collect();
    let offered: std::collections::BTreeSet<String> =
        set.iter().map(|(id, _)| id.clone()).collect();
    let mut out: Vec<ModelOption> = set
        .into_iter()
        .map(|(id, vendor)| ModelOption {
            detected: detected_set.contains(id.as_str()),
            id,
            vendor,
        })
        .collect();
    if !agent_models_are_closed_list(agent, table) {
        for item in detected_items {
            if !offered.contains(item) {
                out.push(ModelOption {
                    id: item.clone(),
                    vendor: agent_vendor_str.clone(),
                    detected: true,
                });
            }
        }
    }
    out
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

    fn table_of(rows: &[(&str, &str)]) -> ModelsTable {
        ModelsTable {
            as_of: String::new(),
            models: rows
                .iter()
                .map(|(id, vendor)| ModelRow {
                    id: (*id).to_string(),
                    vendor: (*vendor).to_string(),
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
                })
                .collect(),
            purposes: Vec::new(),
            claude_static_models: Vec::new(),
            codex_static_models: Vec::new(),
            subscriptions: Vec::new(),
        }
    }

    #[test]
    fn agent_vendor_ties_known_vendor_agents_only() {
        assert_eq!(agent_vendor("claude"), Some("anthropic"));
        assert_eq!(agent_vendor("claude-code"), Some("anthropic"));
        assert_eq!(agent_vendor("codex"), Some("openai"));
        assert_eq!(agent_vendor("grok"), Some("xai"));
        assert_eq!(agent_vendor("opencode"), None);
        assert_eq!(agent_vendor("cursor"), None);
        assert_eq!(agent_vendor("qoder"), None);
        assert_eq!(agent_vendor("zcode"), Some("zhipu"));
        assert_eq!(agent_vendor("some-custom-agent"), None);
    }

    #[test]
    fn model_options_for_agent_curated_table_drives_the_set() {
        let t = table_of(&[
            ("gpt-5.6-sol", "openai"),
            ("gpt-5.6-terra", "openai"),
            ("claude-opus-5-5", "anthropic"),
        ]);
        // codex ties to openai: only the two openai rows are offered, in
        // table order, none detected (an empty local config).
        let opts = model_options_for_agent("codex", &[], &t);
        assert_eq!(
            opts,
            vec![
                ModelOption {
                    id: "gpt-5.6-sol".into(),
                    vendor: "openai".into(),
                    detected: false
                },
                ModelOption {
                    id: "gpt-5.6-terra".into(),
                    vendor: "openai".into(),
                    detected: false
                },
            ]
        );
    }

    #[test]
    fn model_options_for_agent_annotation_flag_is_correct() {
        let t = table_of(&[("gpt-5.6-sol", "openai"), ("gpt-5.6-terra", "openai")]);
        // config.toml's `model` line names exactly one of the two curated
        // rows: detection ANNOTATES that one row, it does not shrink the list
        // to it (finding 9 of issue #42 - the defect this guards against).
        let opts = model_options_for_agent("codex", &["gpt-5.6-sol".to_string()], &t);
        assert_eq!(opts.len(), 2, "detection must not narrow the option set");
        assert!(
            opts.iter().any(|o| o.id == "gpt-5.6-sol" && o.detected),
            "the model named in the local config is annotated detected"
        );
        assert!(
            opts.iter().any(|o| o.id == "gpt-5.6-terra" && !o.detected),
            "a curated sibling model absent from the local config stays offered, undetected"
        );
    }

    #[test]
    fn model_options_for_agent_keeps_a_config_only_model_present() {
        let t = table_of(&[("gpt-5.6-sol", "openai")]);
        // The local config names a model the curated table does not carry
        // yet (e.g. a release too new for the table): it must still be
        // offered, as its own detected-only entry, not silently dropped.
        let opts = model_options_for_agent("codex", &["gpt-5-codex-preview".to_string()], &t);
        assert_eq!(opts.len(), 2);
        assert_eq!(opts[0].id, "gpt-5.6-sol");
        assert!(!opts[0].detected);
        assert_eq!(
            opts[1],
            ModelOption {
                id: "gpt-5-codex-preview".into(),
                vendor: "openai".into(),
                detected: true,
            }
        );
    }

    #[test]
    fn model_options_for_agent_keeps_an_aggregator_on_the_full_table() {
        let t = table_of(&[("gpt-5.6-sol", "openai"), ("claude-opus-5-5", "anthropic")]);
        // opencode is an aggregator (no single vendor tie): it keeps every
        // curated row, same as an unrecognized agent id.
        let opts = model_options_for_agent("opencode", &[], &t);
        assert_eq!(opts.len(), 2);
        assert_eq!(opts[0].vendor, "openai", "each row keeps its own vendor");
        assert_eq!(opts[1].vendor, "anthropic", "each row keeps its own vendor");
        let unknown = model_options_for_agent("some-custom-agent", &[], &t);
        assert_eq!(unknown.len(), 2);
    }

    /// zcode's option set is apb's allowlist, always: bare ids, annotated
    /// `detected` from the detected list, never extended by a detected item
    /// outside the allowlist (the curated zhipu row `glm-5.2` must not
    /// appear either).
    #[test]
    fn model_options_for_zcode_come_from_the_allowlist() {
        let t = table_of(&[("glm-5.2", "zhipu"), ("gpt-5.6-sol", "openai")]);
        let detected = vec!["GLM-5.3".to_string(), "GLM-5.3-Flash".to_string()];
        let opts = model_options_for_agent("zcode", &detected, &t);
        assert_eq!(
            opts,
            vec![
                ModelOption {
                    id: "GLM-5.3".into(),
                    vendor: "zhipu".into(),
                    detected: true
                },
                ModelOption {
                    id: "GLM-5.3-Flash".into(),
                    vendor: "zhipu".into(),
                    detected: true
                },
            ]
        );

        // No detection yet: the allowlist is still the whole offer.
        let bare = model_options_for_agent("zcode", &[], &t);
        let ids: Vec<&str> = bare.iter().map(|o| o.id.as_str()).collect();
        assert_eq!(ids, vec!["GLM-5.3", "GLM-5.3-Flash"]);

        // A detected id outside the allowlist must not leak into the offer.
        let leaked = model_options_for_agent("zcode", &["GLM-5-Turbo".to_string()], &t);
        let ids: Vec<&str> = leaked.iter().map(|o| o.id.as_str()).collect();
        assert_eq!(ids, vec!["GLM-5.3", "GLM-5.3-Flash"]);
    }

    /// codex's option set is the static list from the table, in list order;
    /// detection only annotates. With no static list in the table the old
    /// vendor-rows path applies (an overlay may clear or omit it).
    #[test]
    fn model_options_for_codex_come_from_the_static_list() {
        let mut t = table_of(&[("gpt-5.6-sol", "openai"), ("gpt-5.4-nano", "openai")]);
        let seven = [
            "gpt-6-sol",
            "gpt-6-astra",
            "gpt-6-luna",
            "gpt-5.6-sol",
            "gpt-5.6-terra",
            "gpt-5.6-luna",
            "gpt-5.5",
        ];
        t.codex_static_models = seven.iter().map(|s| s.to_string()).collect();
        let opts = model_options_for_agent(
            "codex",
            &["gpt-5.6-terra".to_string(), "gpt-reserve".to_string()],
            &t,
        );
        let ids: Vec<&str> = opts.iter().map(|o| o.id.as_str()).collect();
        assert_eq!(ids, seven, "exactly the static list, default first");
        assert!(
            !opts
                .iter()
                .any(|o| o.id == "gpt-reserve" || o.id == "gpt-5.4-nano"),
            "neither a config-only model nor a curated row outside the list may join it"
        );
        let terra = opts.iter().find(|o| o.id == "gpt-5.6-terra").unwrap();
        assert!(terra.detected);
        assert!(!opts.iter().find(|o| o.id == "gpt-6-sol").unwrap().detected);
    }

    /// claude is a closed list too: exactly `claude_static_models`, even when
    /// the curated table carries other anthropic rows; an overlay that clears
    /// the list falls back to the vendor rows.
    #[test]
    fn model_options_for_claude_come_from_the_static_list() {
        let mut t = table_of(&[
            ("claude-opus-5-5", "anthropic"),
            ("claude-old-1", "anthropic"),
        ]);
        t.claude_static_models = vec!["claude-opus-5-5".into(), "claude-sonnet-5".into()];
        for agent in ["claude", "claude-code"] {
            let ids: Vec<String> = model_options_for_agent(agent, &["claude-x".into()], &t)
                .into_iter()
                .map(|o| o.id)
                .collect();
            assert_eq!(ids, vec!["claude-opus-5-5", "claude-sonnet-5"], "{agent}");
        }
        t.claude_static_models.clear();
        assert_eq!(static_models_for_agent("claude", &t), None);
        assert_eq!(model_options_for_agent("claude", &[], &t).len(), 2);
    }

    #[test]
    fn static_models_for_agent_covers_exactly_the_closed_list_agents() {
        let t = builtin();
        assert_eq!(
            static_models_for_agent("zcode", &t),
            Some(crate::zcode::model_list())
        );
        assert_eq!(
            static_models_for_agent("claude", &t),
            Some(t.claude_static_models.clone())
        );
        assert_eq!(
            static_models_for_agent("codex", &t),
            Some(t.codex_static_models.clone())
        );
        for open in ["opencode", "grok", "cursor", "qoder", "some-agent"] {
            assert_eq!(static_models_for_agent(open, &t), None, "{open}");
        }
    }

    /// Table-wide invariants a model refresh must keep, without naming any
    /// model: ids are unique, every metered row is priced, and every model the
    /// table's own closed lists offer has a full row (provenance is checked for
    /// every row above). zcode's list is the Rust allowlist and is deliberately
    /// unpriced: GLM-5.3 has no published metered price.
    #[test]
    fn rows_are_unique_priced_and_back_every_static_list() {
        let t = builtin();
        let mut seen = std::collections::BTreeSet::new();
        for m in &t.models {
            assert!(seen.insert(m.id.as_str()), "duplicate model id `{}`", m.id);
            if !(m.stt || m.tts) {
                assert!(
                    m.cost_in_usd_mtok.is_some() && m.cost_out_usd_mtok.is_some(),
                    "model `{}` is missing a price",
                    m.id
                );
            }
        }
        for id in t.claude_static_models.iter().chain(&t.codex_static_models) {
            let m = t
                .models
                .iter()
                .find(|m| &m.id == id)
                .unwrap_or_else(|| panic!("static-list model `{id}` has no row"));
            assert!(
                m.context_tokens.is_some() && m.reasoning.is_some(),
                "static-list model `{id}` is missing context or reasoning"
            );
        }
    }
}
