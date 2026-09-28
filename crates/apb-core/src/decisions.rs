//! Decision-model configuration (issue #165 Part 2): the machine's
//! `<config_dir>/decisions.yaml`, the project's narrowing in
//! `.apb/config.yaml`, and the effective settings a run snapshots.
//!
//! A sibling file rather than a `config.yaml` section: `GlobalConfig` is
//! `deny_unknown_fields`, so a new section would make every older apb on the
//! machine (the dashboard service included) refuse its config. Older
//! binaries never read this file, and it carries its own `version`.
//!
//! Keys are never written here: `api_key` is a reference, `{{env.VAR}}` or
//! `{{cmd:...}}` (the connector secrets grammar), resolved at call time. A
//! literal is a load error that names the field and never echoes the value.
//!
//! A project can only narrow: its `decisions:` section may switch the layer
//! off, lower modes, send less or keep requests on local providers. Anything
//! else there (a provider, a URL, a key, a budget, an unknown key) opts the
//! project out altogether, and doctor says why.

use std::collections::BTreeMap;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::connector::secrets::{parse_cmd_ref, parse_env_ref};

/// The file name under the config dir.
pub const DECISIONS_FILE: &str = "decisions.yaml";
/// The process-wide kill switch: `APB_DECISIONS=off`.
pub const KILL_SWITCH_ENV: &str = "APB_DECISIONS";
/// `kind: fake` providers load only with `APB_DECISIONS_ALLOW_FAKE=1`.
pub const ALLOW_FAKE_ENV: &str = "APB_DECISIONS_ALLOW_FAKE";
/// The completion check's default `final_result` cut: an answer below it
/// would be flagged (issue #165 Part 8, the 2026-09-27 Phase 0 addendum). A
/// measured starting point, to be re-fitted on shadow data.
pub const COMPLETION_FINAL_RESULT_CUT: f64 = 0.15;
/// `uses.catalog_rank.max_requests_per_day` when the file sets none.
pub const CATALOG_RANK_MAX_REQUESTS_PER_DAY: u32 = 200;
/// The catalog coverage check's default cut: a suppression record covers
/// the task at `p` at or above it (`uses.catalog_rank.thresholds.covered`).
/// A starting point to be measured, like every default threshold.
pub const CATALOG_COVERED_CUT: f64 = 0.8;
/// The known use names (`uses.<name>`).
pub const USE_NAMES: [&str; 8] = [
    "judge_node",
    "judge_edge",
    "completion_check",
    "retry_advice",
    "supervisor_triage",
    "review_triage",
    "routing",
    "catalog_rank",
];

/// A use's mode, ordered from least to most effect.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DecisionMode {
    Off,
    /// Journal only.
    Shadow,
    /// Shown where a person or supervisor decides, never applied.
    Advise,
    /// Changes engine behaviour.
    Enforce,
}

impl DecisionMode {
    pub fn as_str(self) -> &'static str {
        match self {
            DecisionMode::Off => "off",
            DecisionMode::Shadow => "shadow",
            DecisionMode::Advise => "advise",
            DecisionMode::Enforce => "enforce",
        }
    }
}

/// Where a provider keeps the data it is sent.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DataClass {
    /// A hosted service, under its own terms.
    #[default]
    Hosted,
    /// A server on this machine or network.
    Local,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderKind {
    /// `POST {base_url}/v1/systemone`.
    Systemone,
    /// Scripted answers, no network (tests and dry runs; needs
    /// `APB_DECISIONS_ALLOW_FAKE=1`).
    Fake,
    /// A chat model imitating the interface through structured output
    /// (issue #165 Part 6): `via: openai_compatible`, `POST
    /// {base_url}/chat/completions`. Uncalibrated.
    LlmEmulation,
    // --- issue #165 Part 15: route-specific adapters -----------------------
    /// Vercel AI Gateway `POST {base_url}/v1/evaluate` (base URL defaults to
    /// `https://ai-gateway.vercel.sh`).
    VercelEvaluate,
    /// OpenRouter's alpha Decisions API, `POST {base_url}/api/alpha/decisions`
    /// (base URL defaults to `https://openrouter.ai`).
    OpenrouterDecisions,
    /// Cloudflare Workers AI REST, `POST
    /// {base_url}/accounts/{account_id}/ai/run` (base URL defaults to
    /// `https://api.cloudflare.com/client/v4`; `account_id` required).
    Cloudflare,
    // --- end Part 15 ----------------------------------------------------------
}

impl ProviderKind {
    /// The name as written in `decisions.yaml`.
    pub fn as_str(self) -> &'static str {
        match self {
            ProviderKind::Systemone => "systemone",
            ProviderKind::Fake => "fake",
            ProviderKind::VercelEvaluate => "vercel_evaluate",
            ProviderKind::OpenrouterDecisions => "openrouter_decisions",
            ProviderKind::Cloudflare => "cloudflare",
            ProviderKind::LlmEmulation => "llm_emulation",
        }
    }

    /// The base URL a kind uses when the file names none.
    fn default_base_url(self) -> Option<&'static str> {
        match self {
            ProviderKind::VercelEvaluate => Some("https://ai-gateway.vercel.sh"),
            ProviderKind::OpenrouterDecisions => Some("https://openrouter.ai"),
            ProviderKind::Cloudflare => Some("https://api.cloudflare.com/client/v4"),
            ProviderKind::Systemone | ProviderKind::Fake | ProviderKind::LlmEmulation => None,
        }
    }
}

/// Whether a model id is an alias that moves with releases (a leading `~`,
/// a `-latest` or `-preview` suffix) rather than a pinned version.
/// Thresholds tuned on one version do not transfer, so doctor warns.
pub fn is_model_alias(model: &str) -> bool {
    let m = model.trim();
    m.starts_with('~') || m.ends_with("-latest") || m.ends_with("-preview")
}

// --- issue #165 Part 6: LLM emulation settings ------------------------------

/// How an `llm_emulation` provider asks for structured output.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EmulationOutput {
    /// `response_format: {type: json_schema, strict: true}`.
    #[default]
    JsonSchema,
    /// The schema in the prompt, the first JSON object of the reply parsed.
    PromptOnly,
}

/// The only `via` a `decisions.yaml` provider takes. The other emulation
/// backend, an APB agent profile, is declared on the judge node itself
/// (`on_unavailable: emulate` with `profile`), never in this file.
const VIA_OPENAI_COMPATIBLE: &str = "openai_compatible";

// --- end LLM emulation settings ---------------------------------------------

/// A material class a use may send (`privacy.send`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SendClass {
    /// Rendered node prompts.
    Prompts,
    /// Agent outputs.
    Outputs,
    /// Diffs (opt-in).
    Diffs,
}

/// Where a provider key comes from. Only the reference is ever stored.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum KeyRef {
    /// `{{env.VAR}}`: the variable name, resolved through the process
    /// environment and the dotenv chain.
    Env(String),
    /// `{{cmd:...}}`: the command line whose stdout is the key.
    Cmd(String),
}

/// One provider as a run snapshots it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProviderSpec {
    pub id: String,
    pub kind: ProviderKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default)]
    pub data_class: DataClass,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key: Option<KeyRef>,
    /// `kind: fake` only: reply items by question id, in the wire shape.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub answers: BTreeMap<String, serde_json::Value>,
    /// `kind: cloudflare` only: the account id, letters and digits, or a
    /// `{{env.VAR}}` reference to one (resolved at call time).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub account_id: Option<String>,
    /// `kind: vercel_evaluate` only: ask the gateway for zero data
    /// retention on every request.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub zero_data_retention: bool,
    /// `kind: llm_emulation` only: how structured output is asked for.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub structured_output: Option<EmulationOutput>,
}

impl ProviderSpec {
    /// The URL's host (and port), for display: never a path or query.
    pub fn host(&self) -> Option<String> {
        self.base_url
            .as_deref()
            .and_then(parse_origin)
            .map(|o| match o.port {
                Some(p) => format!("{}:{p}", o.host),
                None => o.host,
            })
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Budget {
    pub max_requests_per_run: u32,
    pub max_usd_per_run: f64,
}

impl Default for Budget {
    fn default() -> Self {
        Budget {
            max_requests_per_run: 200,
            max_usd_per_run: 0.05,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Privacy {
    /// The material classes that may be sent; everything else is emptied.
    pub send: Vec<SendClass>,
    /// Redact secret values, token-shaped strings, absolute paths and
    /// e-mail addresses before sending.
    pub redact: bool,
    /// The byte budget for one request's state.
    pub max_state_bytes: usize,
    /// Keep the redacted state of each decision under
    /// `runs/<id>/decisions/<seq>.json`.
    pub debug_state: bool,
}

impl Default for Privacy {
    fn default() -> Self {
        Privacy {
            send: vec![SendClass::Prompts, SendClass::Outputs],
            redact: true,
            max_state_bytes: 24_000,
            debug_state: false,
        }
    }
}

/// One use's settings.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UseSettings {
    pub mode: DecisionMode,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub thresholds: BTreeMap<String, f64>,
    /// `catalog_rank` only (issue #165 Part 16): the most requests per
    /// project and UTC day (default [`CATALOG_RANK_MAX_REQUESTS_PER_DAY`]).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_requests_per_day: Option<u32>,
}

/// The settings a run works with: the machine's file, capped by its
/// ceiling and narrowed by the project. Snapshotted into the run manifest,
/// so a mid-run edit of `decisions.yaml` does not apply.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EffectiveDecisions {
    /// The global ceiling.
    pub mode: DecisionMode,
    pub timeout_ms: u64,
    pub providers: Vec<ProviderSpec>,
    pub budget: Budget,
    pub privacy: Privacy,
    /// Effective per-use settings, already capped. Uses that end up off are
    /// left out.
    pub uses: BTreeMap<String, UseSettings>,
}

impl EffectiveDecisions {
    /// The effective mode of `use_name` (off when absent).
    pub fn mode_for(&self, use_name: &str) -> DecisionMode {
        self.uses
            .get(use_name)
            .map_or(DecisionMode::Off, |u| u.mode.min(self.mode))
    }

    /// A use's threshold by name.
    pub fn threshold(&self, use_name: &str, name: &str) -> Option<f64> {
        self.uses.get(use_name)?.thresholds.get(name).copied()
    }

    pub fn sends(&self, class: SendClass) -> bool {
        self.privacy.send.contains(&class)
    }
}

/// What the configuration amounts to for one project.
#[derive(Debug, Clone, PartialEq)]
pub enum Resolution {
    /// No `decisions.yaml` (or no config dir): today's behaviour.
    NotConfigured,
    /// `decisions.yaml` does not load; the layer stays off.
    Invalid(String),
    /// `APB_DECISIONS=off`.
    KilledBySwitch,
    /// The project switched the layer off or asked for more than it may.
    OptedOut(String),
    /// Configured, but no use is above off.
    AllOff,
    Active(EffectiveDecisions),
}

impl Resolution {
    pub fn active(self) -> Option<EffectiveDecisions> {
        match self {
            Resolution::Active(e) => Some(e),
            _ => None,
        }
    }
}

/// Whether the kill switch is set for this process.
pub fn killed_by_switch() -> bool {
    std::env::var(KILL_SWITCH_ENV).is_ok_and(|v| v.trim().eq_ignore_ascii_case("off"))
}

// --- the machine file ------------------------------------------------------

fn default_version() -> u32 {
    1
}
fn default_mode() -> DecisionMode {
    DecisionMode::Shadow
}
fn default_timeout_ms() -> u64 {
    3000
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct FileDoc {
    #[serde(default = "default_version")]
    version: u32,
    #[serde(default = "default_mode")]
    mode: DecisionMode,
    #[serde(default = "default_timeout_ms")]
    timeout_ms: u64,
    #[serde(default)]
    providers: Vec<ProviderDoc>,
    #[serde(default)]
    budget: Budget,
    #[serde(default)]
    uses: BTreeMap<String, UseSettings>,
    #[serde(default)]
    privacy: Privacy,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ProviderDoc {
    id: String,
    kind: ProviderKind,
    #[serde(default)]
    base_url: Option<String>,
    #[serde(default)]
    model: Option<String>,
    /// Kept untyped so a wrongly typed literal never reaches a serde error
    /// message (which would quote it).
    #[serde(default)]
    api_key: Option<serde_yaml_ng::Value>,
    #[serde(default)]
    data_class: DataClass,
    #[serde(default)]
    answers: BTreeMap<String, serde_json::Value>,
    #[serde(default)]
    account_id: Option<String>,
    #[serde(default)]
    zero_data_retention: bool,
    /// `kind: llm_emulation` only.
    #[serde(default)]
    via: Option<String>,
    #[serde(default)]
    structured_output: Option<EmulationOutput>,
}

/// A URL's parts that matter here.
struct Origin {
    host: String,
    port: Option<u16>,
    https: bool,
}

/// Parses `http(s)://host[:port][/path]`. `None` for any other shape,
/// including a URL with credentials, a query or a fragment.
fn parse_origin(url: &str) -> Option<Origin> {
    let (https, rest) = if let Some(r) = url.strip_prefix("https://") {
        (true, r)
    } else {
        (false, url.strip_prefix("http://")?)
    };
    if url.contains(['?', '#', '@', ' ']) {
        return None;
    }
    let authority = rest.split('/').next().unwrap_or("");
    let (host, port) = match authority.rsplit_once(':') {
        Some((h, p)) if !h.ends_with(']') || h.starts_with('[') => (h, Some(p.parse().ok()?)),
        _ => (authority, None),
    };
    if host.is_empty() {
        return None;
    }
    Some(Origin {
        host: host.to_string(),
        port,
        https,
    })
}

fn valid_id(id: &str) -> bool {
    (1..=64).contains(&id.len())
        && id.starts_with(|c: char| c.is_ascii_lowercase() || c.is_ascii_digit())
        && id
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_')
}

fn key_ref(field: &str, value: &serde_yaml_ng::Value) -> Result<KeyRef, String> {
    let refusal = || {
        format!(
            "{field} must be a reference ({{{{env.VAR}}}} or {{{{cmd:...}}}}), not a literal value"
        )
    };
    let text = value.as_str().ok_or_else(refusal)?;
    if let Some(var) = parse_env_ref(text) {
        return Ok(KeyRef::Env(var));
    }
    if let Some(cmd) = parse_cmd_ref(text) {
        return Ok(KeyRef::Cmd(cmd));
    }
    Err(refusal())
}

/// A serde error message about the file, stripped of anything that could
/// quote a value: only the message up to the first backtick-quoted value is
/// kept when it mentions a value at all.
fn describe_parse_error(e: &serde_yaml_ng::Error) -> String {
    let text = e.to_string();
    // "unknown field `x`, expected one of ..." and "unknown variant `x`"
    // quote a key or a mode name, never a secret: the key field is untyped.
    // Anything else keeps only its location.
    if text.contains("unknown field")
        || text.contains("unknown variant")
        || text.contains("missing field")
    {
        text
    } else {
        match e.location() {
            Some(l) => format!("invalid YAML at line {}, column {}", l.line(), l.column()),
            None => "invalid YAML".to_string(),
        }
    }
}

/// Loads and checks the machine file. `Ok(None)` when it does not exist.
pub fn load_file(config_dir: &Path) -> Result<Option<EffectiveDecisions>, String> {
    let path = config_dir.join(DECISIONS_FILE);
    let raw = match std::fs::read_to_string(&path) {
        Ok(raw) => raw,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(format!("cannot read {DECISIONS_FILE}: {e}")),
    };
    let doc: FileDoc = serde_yaml_ng::from_str(&raw).map_err(|e| describe_parse_error(&e))?;
    if doc.version != 1 {
        return Err(format!(
            "unsupported version {} (this apb reads 1)",
            doc.version
        ));
    }
    if doc.timeout_ms == 0 {
        return Err("timeout_ms must be positive".into());
    }
    if doc.privacy.max_state_bytes < 1024 {
        return Err("privacy.max_state_bytes must be at least 1024".into());
    }
    if doc.budget.max_usd_per_run.is_nan() || doc.budget.max_usd_per_run < 0.0 {
        return Err("budget.max_usd_per_run must not be negative".into());
    }
    if let Some(name) = doc.uses.keys().find(|k| !USE_NAMES.contains(&k.as_str())) {
        return Err(format!("unknown use `{name}` under uses"));
    }
    if let Some((name, _)) = doc
        .uses
        .iter()
        .find(|(k, u)| u.max_requests_per_day.is_some() && k.as_str() != "catalog_rank")
    {
        return Err(format!(
            "uses.{name}.max_requests_per_day is only for catalog_rank"
        ));
    }
    let mut providers = Vec::new();
    for (i, p) in doc.providers.into_iter().enumerate() {
        let at = format!("providers[{i}]");
        if !valid_id(&p.id) {
            return Err(format!("{at}.id must match [a-z0-9][a-z0-9_-]*"));
        }
        if providers.iter().any(|q: &ProviderSpec| q.id == p.id) {
            return Err(format!("{at}.id `{}` is used twice", p.id));
        }
        let key = match &p.api_key {
            Some(v) => Some(key_ref(&format!("{at}.api_key"), v)?),
            None => None,
        };
        if p.account_id.is_some() && p.kind != ProviderKind::Cloudflare {
            return Err(format!("{at}.account_id is only for kind cloudflare"));
        }
        if p.zero_data_retention && p.kind != ProviderKind::VercelEvaluate {
            return Err(format!(
                "{at}.zero_data_retention is only for kind vercel_evaluate"
            ));
        }
        if p.kind != ProviderKind::LlmEmulation
            && (p.via.is_some() || p.structured_output.is_some())
        {
            return Err(format!(
                "{at}: via and structured_output are only for kind llm_emulation"
            ));
        }
        let base_url = p
            .base_url
            .clone()
            .or_else(|| p.kind.default_base_url().map(str::to_string));
        match p.kind {
            ProviderKind::VercelEvaluate
            | ProviderKind::OpenrouterDecisions
            | ProviderKind::Cloudflare
            | ProviderKind::LlmEmulation => {
                let kind = p.kind.as_str();
                if p.kind == ProviderKind::LlmEmulation {
                    match p.via.as_deref() {
                        Some(VIA_OPENAI_COMPATIBLE) => {}
                        Some("profile") => {
                            return Err(format!(
                                "{at}: via profile is declared on the judge node (on_unavailable: emulate with a profile), not in {DECISIONS_FILE}"
                            ));
                        }
                        _ => {
                            return Err(format!(
                                "{at}.via must be {VIA_OPENAI_COMPATIBLE} for kind llm_emulation"
                            ));
                        }
                    }
                }
                let url = base_url
                    .as_deref()
                    .ok_or_else(|| format!("{at}.base_url is required for kind {kind}"))?;
                if parse_origin(url).is_none() {
                    return Err(format!(
                        "{at}.base_url must be an http(s) URL without credentials, query or fragment"
                    ));
                }
                if p.model.as_deref().is_none_or(str::is_empty) {
                    return Err(format!("{at}.model is required for kind {kind}"));
                }
                if !p.answers.is_empty() {
                    return Err(format!("{at}.answers is only for kind fake"));
                }
                if p.kind == ProviderKind::Cloudflare {
                    let id = p.account_id.as_deref().unwrap_or_default();
                    let literal = !id.is_empty()
                        && id.len() <= 64
                        && id.chars().all(|c| c.is_ascii_alphanumeric());
                    if !literal && parse_env_ref(id).is_none() {
                        return Err(format!(
                            "{at}.account_id is required for kind cloudflare: letters and digits, or {{{{env.VAR}}}}"
                        ));
                    }
                }
            }
            ProviderKind::Systemone => {
                let url = p
                    .base_url
                    .as_deref()
                    .ok_or_else(|| format!("{at}.base_url is required for kind systemone"))?;
                if parse_origin(url).is_none() {
                    return Err(format!(
                        "{at}.base_url must be an http(s) URL without credentials, query or fragment"
                    ));
                }
                if p.model.as_deref().is_none_or(str::is_empty) {
                    return Err(format!("{at}.model is required for kind systemone"));
                }
                if !p.answers.is_empty() {
                    return Err(format!("{at}.answers is only for kind fake"));
                }
            }
            ProviderKind::Fake => {
                if std::env::var(ALLOW_FAKE_ENV).as_deref() != Ok("1") {
                    return Err(format!("{at}: kind fake needs {ALLOW_FAKE_ENV}=1"));
                }
            }
        }
        providers.push(ProviderSpec {
            id: p.id,
            kind: p.kind,
            base_url: base_url.map(|u| u.trim_end_matches('/').to_string()),
            model: p.model,
            data_class: p.data_class,
            key,
            answers: p.answers,
            account_id: p.account_id,
            zero_data_retention: p.zero_data_retention,
            structured_output: match p.kind {
                ProviderKind::LlmEmulation => Some(p.structured_output.unwrap_or_default()),
                _ => None,
            },
        });
    }
    if providers.is_empty() {
        return Err("no providers configured".into());
    }
    let mut uses = doc.uses;
    if let Some(cc) = uses.get_mut("completion_check") {
        cc.thresholds
            .entry("final_result".into())
            .or_insert(COMPLETION_FINAL_RESULT_CUT);
    }
    if let Some(cr) = uses.get_mut("catalog_rank") {
        cr.thresholds
            .entry("covered".into())
            .or_insert(CATALOG_COVERED_CUT);
    }
    Ok(Some(EffectiveDecisions {
        mode: doc.mode,
        timeout_ms: doc.timeout_ms,
        providers,
        budget: doc.budget,
        privacy: doc.privacy,
        uses,
    }))
}

// --- project narrowing -----------------------------------------------------

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct ProjectDecisions {
    enabled: Option<bool>,
    mode: Option<DecisionMode>,
    send: Option<Vec<SendClass>>,
    data_class: Option<DataClass>,
    #[serde(default)]
    uses: BTreeMap<String, ProjectUse>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ProjectUse {
    mode: DecisionMode,
}

/// The project's `decisions:` section, `Ok(None)` when absent. Tolerant of
/// every other key in `.apb/config.yaml`; strict inside the section.
fn project_section(root: &Path) -> Result<Option<ProjectDecisions>, String> {
    let path = root.join(".apb/config.yaml");
    let Ok(raw) = std::fs::read_to_string(&path) else {
        return Ok(None);
    };
    let doc: serde_yaml_ng::Value = match serde_yaml_ng::from_str(&raw) {
        Ok(v) => v,
        Err(_) => return Err("`.apb/config.yaml` is not valid YAML".into()),
    };
    let Some(section) = doc.get("decisions") else {
        return Ok(None);
    };
    if section.is_null() {
        return Ok(None);
    }
    serde_yaml_ng::from_value::<ProjectDecisions>(section.clone())
        .map(Some)
        .map_err(|e| {
            format!(
                "the project's decisions section may only lower or disable ({})",
                describe_parse_error(&e)
            )
        })
}

fn narrow(mut eff: EffectiveDecisions, project: ProjectDecisions) -> Resolution {
    if project.enabled == Some(false) {
        return Resolution::OptedOut("disabled by the project (`decisions.enabled: false`)".into());
    }
    if let Some(name) = project
        .uses
        .keys()
        .find(|k| !USE_NAMES.contains(&k.as_str()))
    {
        return Resolution::OptedOut(format!("the project names an unknown use `{name}`"));
    }
    if let Some(m) = project.mode {
        eff.mode = eff.mode.min(m);
    }
    if let Some(send) = project.send {
        eff.privacy.send.retain(|c| send.contains(c));
    }
    if project.data_class == Some(DataClass::Local) {
        eff.providers.retain(|p| p.data_class == DataClass::Local);
        if eff.providers.is_empty() {
            return Resolution::OptedOut(
                "the project requires local providers and none is configured".into(),
            );
        }
    }
    for (name, u) in project.uses {
        if let Some(s) = eff.uses.get_mut(&name) {
            s.mode = s.mode.min(u.mode);
        }
    }
    finish(eff)
}

/// Caps every use at the ceiling and drops the uses that end up off.
fn finish(mut eff: EffectiveDecisions) -> Resolution {
    let ceiling = eff.mode;
    eff.uses.retain(|_, u| {
        u.mode = u.mode.min(ceiling);
        u.mode > DecisionMode::Off
    });
    if eff.uses.is_empty() {
        Resolution::AllOff
    } else {
        Resolution::Active(eff)
    }
}

/// The configuration for the project at `root` under the machine's config
/// dir (`config::config_dir`).
pub fn resolve(root: &Path) -> Resolution {
    let Some(dir) = crate::config::config_dir() else {
        return Resolution::NotConfigured;
    };
    resolve_in(&dir, root)
}

/// [`resolve`] with an explicit config dir.
pub fn resolve_in(config_dir: &Path, root: &Path) -> Resolution {
    let eff = match load_file(config_dir) {
        Ok(Some(eff)) => eff,
        Ok(None) => return Resolution::NotConfigured,
        Err(e) => return Resolution::Invalid(e),
    };
    if killed_by_switch() {
        return Resolution::KilledBySwitch;
    }
    match project_section(root) {
        Ok(Some(p)) => narrow(eff, p),
        Ok(None) => finish(eff),
        Err(reason) => Resolution::OptedOut(reason),
    }
}

/// Resolves a provider key variable from the process environment, then the
/// GLOBAL `secrets.env`. Never from the project's `.apb/secrets.env`: the
/// repository is untrusted, and a key it planted would route this machine's
/// prompts through the repository owner's provider account.
pub fn resolve_key_var(var: &str) -> Option<String> {
    if let Ok(v) = std::env::var(var)
        && !v.is_empty()
    {
        return Some(v);
    }
    let path = crate::connector::secrets::global_secrets_path()?;
    let raw = std::fs::read_to_string(path).ok()?;
    crate::connector::secrets::parse_dotenv(&raw)
        .remove(var)
        .filter(|v| !v.is_empty())
}

// --- doctor ----------------------------------------------------------------

/// Whether a provider's key reference resolves, without running anything:
/// `Some(true/false)` for `{{env.VAR}}`, `None` for `{{cmd:...}}` (only a
/// run executes it) and for a provider without a key.
fn key_resolves(key: &Option<KeyRef>) -> Option<bool> {
    match key {
        Some(KeyRef::Env(var)) => Some(resolve_key_var(var).is_some()),
        _ => None,
    }
}

/// Whether a TCP connection to the provider's host opens within a second.
/// Sends nothing: no request, no key.
fn reachable(spec: &ProviderSpec) -> Option<bool> {
    use std::net::ToSocketAddrs;
    let origin = parse_origin(spec.base_url.as_deref()?)?;
    let port = origin.port.unwrap_or(if origin.https { 443 } else { 80 });
    let host = origin
        .host
        .trim_start_matches('[')
        .trim_end_matches(']')
        .to_string();
    let addr = (host.as_str(), port).to_socket_addrs().ok()?.next()?;
    Some(std::net::TcpStream::connect_timeout(&addr, std::time::Duration::from_secs(1)).is_ok())
}

/// The one doctor line: `(ok, detail)`. Never prints a key; the only
/// network use is a TCP connect to each configured host.
pub fn doctor_line(root: &Path) -> (bool, String) {
    match resolve(root) {
        Resolution::NotConfigured => (true, "not configured".into()),
        Resolution::Invalid(e) => (false, format!("{DECISIONS_FILE} ignored: {e}")),
        Resolution::KilledBySwitch => (true, format!("off ({KILL_SWITCH_ENV}=off)")),
        Resolution::OptedOut(why) => (true, format!("off for this project: {why}")),
        Resolution::AllOff => (true, "configured, every use off".into()),
        Resolution::Active(eff) => {
            let uses: Vec<String> = eff
                .uses
                .iter()
                .map(|(n, u)| format!("{n} {}", u.mode.as_str()))
                .collect();
            let providers: Vec<String> = eff
                .providers
                .iter()
                .map(|p| {
                    let kind = match p.kind {
                        ProviderKind::LlmEmulation => "llm_emulation (uncalibrated)",
                        k => k.as_str(),
                    };
                    let mut parts = vec![kind.to_string()];
                    if let Some(h) = p.host() {
                        parts.push(h);
                    }
                    parts.push(match (&p.key, key_resolves(&p.key)) {
                        (None, _) => "no key".into(),
                        (_, Some(true)) => "key set".into(),
                        (_, Some(false)) => "key missing".into(),
                        (_, None) => "key from command".into(),
                    });
                    if let Some(r) = reachable(p) {
                        parts.push(if r { "reachable" } else { "unreachable" }.into());
                    }
                    if p.model.as_deref().is_some_and(is_model_alias) {
                        parts.push("model is an alias, pin a version".into());
                    }
                    format!("{} ({})", p.id, parts.join(", "))
                })
                .collect();
            (
                true,
                format!(
                    "ceiling {}; {}; providers: {}",
                    eff.mode.as_str(),
                    uses.join(", "),
                    providers.join("; ")
                ),
            )
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SENTINEL: &str = "sk-sentinel-literal-9d2f";

    fn write(dir: &Path, body: &str) {
        std::fs::write(dir.join(DECISIONS_FILE), body).unwrap();
    }

    const FULL: &str = r#"
version: 1
mode: shadow
timeout_ms: 2500
providers:
  - { id: typesafe, kind: systemone, base_url: "https://api.example.test/", model: m-1, api_key: "{{env.EXAMPLE_KEY}}" }
  - { id: helper, kind: systemone, base_url: "https://other.example.test", model: m-2, api_key: "{{cmd:pass show key}}" }
  - { id: local, kind: systemone, base_url: "http://127.0.0.1:8080", model: laya, data_class: local }
budget: { max_requests_per_run: 5, max_usd_per_run: 0.01 }
uses:
  completion_check: { mode: enforce }
  judge_node: { mode: off }
privacy: { send: [prompts, outputs], redact: true, max_state_bytes: 20000, debug_state: true }
"#;

    #[test]
    fn a_full_file_loads_with_references_and_capped_modes() {
        let cfg = tempfile::tempdir().unwrap();
        let root = tempfile::tempdir().unwrap();
        write(cfg.path(), FULL);
        let eff = resolve_in(cfg.path(), root.path()).active().unwrap();
        assert_eq!(eff.timeout_ms, 2500);
        assert_eq!(
            eff.providers[0].key,
            Some(KeyRef::Env("EXAMPLE_KEY".into()))
        );
        assert_eq!(
            eff.providers[1].key,
            Some(KeyRef::Cmd("pass show key".into()))
        );
        assert_eq!(
            eff.providers[0].base_url.as_deref(),
            Some("https://api.example.test")
        );
        assert_eq!(eff.providers[2].host().as_deref(), Some("127.0.0.1:8080"));
        // enforce capped at the shadow ceiling; judge_node off is dropped.
        assert_eq!(eff.mode_for("completion_check"), DecisionMode::Shadow);
        assert_eq!(eff.mode_for("judge_node"), DecisionMode::Off);
        assert!(!eff.uses.contains_key("judge_node"));
        assert_eq!(
            eff.threshold("completion_check", "final_result"),
            Some(COMPLETION_FINAL_RESULT_CUT)
        );
        assert_eq!(eff.budget.max_requests_per_run, 5);
        assert!(eff.privacy.debug_state);
    }

    #[test]
    fn an_llm_emulation_provider_loads_and_its_profile_form_is_refused_here() {
        let cfg = tempfile::tempdir().unwrap();
        let root = tempfile::tempdir().unwrap();
        write(
            cfg.path(),
            "providers:\n  - { id: emu, kind: llm_emulation, via: openai_compatible, base_url: \"https://llm.example.test/v1\", model: small-1, api_key: \"{{env.EMU_KEY}}\", structured_output: prompt_only }\nuses:\n  judge_node: { mode: shadow }\n",
        );
        let eff = resolve_in(cfg.path(), root.path()).active().unwrap();
        assert_eq!(eff.providers[0].kind, ProviderKind::LlmEmulation);
        assert_eq!(
            eff.providers[0].structured_output,
            Some(EmulationOutput::PromptOnly)
        );
        for (body, needle) in [
            (
                "providers:\n  - { id: emu, kind: llm_emulation, via: profile, base_url: \"https://x.test\", model: m }\n",
                "declared on the judge node",
            ),
            (
                "providers:\n  - { id: emu, kind: llm_emulation, base_url: \"https://x.test\", model: m }\n",
                "via must be openai_compatible",
            ),
            (
                "providers:\n  - { id: s, kind: systemone, base_url: \"https://x.test\", model: m, via: openai_compatible }\n",
                "only for kind llm_emulation",
            ),
        ] {
            write(cfg.path(), body);
            match resolve_in(cfg.path(), root.path()) {
                Resolution::Invalid(e) => assert!(e.contains(needle), "{e}"),
                other => panic!("{other:?}"),
            }
        }
    }

    #[test]
    fn a_missing_file_is_not_configured_and_defaults_apply() {
        let cfg = tempfile::tempdir().unwrap();
        let root = tempfile::tempdir().unwrap();
        assert_eq!(
            resolve_in(cfg.path(), root.path()),
            Resolution::NotConfigured
        );
        write(
            cfg.path(),
            "providers: [{ id: a, kind: systemone, base_url: 'http://127.0.0.1:1', model: m }]\nuses: { completion_check: { mode: shadow, thresholds: { final_result: 0.2 } } }\n",
        );
        let eff = resolve_in(cfg.path(), root.path()).active().unwrap();
        assert_eq!(eff.timeout_ms, 3000);
        assert_eq!(eff.mode, DecisionMode::Shadow);
        assert_eq!(eff.budget, Budget::default());
        assert_eq!(eff.privacy, Privacy::default());
        assert_eq!(eff.threshold("completion_check", "final_result"), Some(0.2));
        write(
            cfg.path(),
            "providers: [{ id: a, kind: systemone, base_url: 'http://127.0.0.1:1', model: m }]\n",
        );
        assert_eq!(resolve_in(cfg.path(), root.path()), Resolution::AllOff);
    }

    #[test]
    fn bad_files_fail_without_echoing_a_literal_key() {
        let cfg = tempfile::tempdir().unwrap();
        let root = tempfile::tempdir().unwrap();
        let provider = |key: &str| {
            format!(
                "providers: [{{ id: a, kind: systemone, base_url: 'https://h.test', model: m, api_key: {key} }}]\n"
            )
        };
        let cases = [
            provider(&format!("\"{SENTINEL}\"")),
            provider(&format!("\"{{{{env.X}}}} {SENTINEL}\"")),
            provider(&format!("[{SENTINEL}]")),
            provider("123456789"),
            format!("{}bogus_key: 1\n", provider("\"{{env.X}}\"")),
            format!("{}mode: sometimes\n", provider("\"{{env.X}}\"")),
            format!(
                "{}uses: {{ other: {{ mode: shadow }} }}\n",
                provider("\"{{env.X}}\"")
            ),
            "version: 2\n".to_string(),
            "providers: []\n".to_string(),
            format!(
                "providers: [{{ id: a, kind: systemone, base_url: 'https://u:{SENTINEL}@h.test', model: m }}]\n"
            ),
            "providers: [{ id: a, kind: systemone, model: m }]\n".to_string(),
            "providers: [{ id: A, kind: systemone, base_url: 'https://h.test', model: m }]\n"
                .to_string(),
            "providers: [{ id: a, kind: fake }]\n".to_string(),
            format!(
                "providers: [{{ id: a, kind: systemone, base_url: 'https://h.test', model: m }}]\nx: \"{SENTINEL}\n"
            ),
        ];
        for body in cases {
            write(cfg.path(), &body);
            match resolve_in(cfg.path(), root.path()) {
                Resolution::Invalid(e) => assert!(!e.contains(SENTINEL), "{e}"),
                other => panic!("{body} must be invalid, got {other:?}"),
            }
        }
    }

    fn project(root: &Path, body: &str) {
        std::fs::create_dir_all(root.join(".apb")).unwrap();
        std::fs::write(root.join(".apb/config.yaml"), body).unwrap();
    }

    #[test]
    fn a_project_can_only_narrow() {
        let cfg = tempfile::tempdir().unwrap();
        write(cfg.path(), FULL);
        let root = tempfile::tempdir().unwrap();

        project(
            root.path(),
            "skills_dir: x\ndecisions: { enabled: false }\n",
        );
        assert!(matches!(
            resolve_in(cfg.path(), root.path()),
            Resolution::OptedOut(_)
        ));

        project(root.path(), "decisions: { send: [], data_class: local }\n");
        let eff = resolve_in(cfg.path(), root.path()).active().unwrap();
        assert!(eff.privacy.send.is_empty());
        assert_eq!(eff.providers.len(), 1);
        assert_eq!(eff.providers[0].id, "local");

        project(
            root.path(),
            "decisions: { uses: { completion_check: { mode: off } } }\n",
        );
        assert_eq!(resolve_in(cfg.path(), root.path()), Resolution::AllOff);

        project(
            root.path(),
            "decisions: { mode: enforce, send: [prompts, outputs, diffs] }\n",
        );
        let eff = resolve_in(cfg.path(), root.path()).active().unwrap();
        assert_eq!(
            eff.mode,
            DecisionMode::Shadow,
            "a project cannot raise the ceiling"
        );
        assert!(!eff.sends(SendClass::Diffs), "a project cannot add a class");

        for refused in [
            "decisions: { providers: [{ id: evil, kind: systemone }] }\n",
            "decisions: { base_url: 'https://evil.test' }\n",
            "decisions: { api_key: '{{env.EVIL}}' }\n",
            "decisions: { budget: { max_requests_per_run: 9999 } }\n",
            "decisions: { uses: { invented: { mode: shadow } } }\n",
        ] {
            project(root.path(), refused);
            assert!(
                matches!(resolve_in(cfg.path(), root.path()), Resolution::OptedOut(_)),
                "{refused} must opt the project out"
            );
        }
    }

    #[test]
    fn origins_parse_only_plain_http_urls() {
        assert!(parse_origin("https://api.example.test").is_some());
        assert_eq!(
            parse_origin("http://127.0.0.1:8080/x").unwrap().port,
            Some(8080)
        );
        for bad in [
            "ftp://x",
            "https://",
            "https://u:p@h",
            "https://h/?k=v",
            "https://h/#f",
            "h.test",
        ] {
            assert!(parse_origin(bad).is_none(), "{bad}");
        }
    }

    // --- issue #165 Parts 15 and 16 ------------------------------------------

    #[test]
    fn the_route_kinds_load_with_their_defaults_and_own_fields() {
        let cfg = tempfile::tempdir().unwrap();
        let root = tempfile::tempdir().unwrap();
        write(
            cfg.path(),
            r#"
providers:
  - { id: vercel, kind: vercel_evaluate, model: typesafe-ai/jev, api_key: "{{env.AI_GATEWAY_API_KEY}}", zero_data_retention: true }
  - { id: or, kind: openrouter_decisions, model: typesafe/jev-1.13, api_key: "{{env.OPENROUTER_API_KEY}}" }
  - { id: cf, kind: cloudflare, model: typesafe/jev, account_id: "{{env.CF_ACCOUNT}}", api_key: "{{env.CF_TOKEN}}" }
  - { id: cf2, kind: cloudflare, base_url: "https://cf.example.test/v4/", model: typesafe/jev, account_id: abc123 }
uses: { catalog_rank: { mode: advise, max_requests_per_day: 10 } }
"#,
        );
        let eff = resolve_in(cfg.path(), root.path()).active().unwrap();
        let urls: Vec<_> = eff
            .providers
            .iter()
            .map(|p| p.base_url.as_deref())
            .collect();
        assert_eq!(
            urls,
            [
                Some("https://ai-gateway.vercel.sh"),
                Some("https://openrouter.ai"),
                Some("https://api.cloudflare.com/client/v4"),
                Some("https://cf.example.test/v4"),
            ]
        );
        assert!(eff.providers[0].zero_data_retention);
        assert_eq!(
            eff.providers[2].account_id.as_deref(),
            Some("{{env.CF_ACCOUNT}}")
        );
        let rank = &eff.uses["catalog_rank"];
        assert_eq!(rank.max_requests_per_day, Some(10));
        assert_eq!(rank.thresholds.get("covered"), Some(&CATALOG_COVERED_CUT));
    }

    #[test]
    fn route_fields_are_refused_on_the_wrong_kind() {
        let cfg = tempfile::tempdir().unwrap();
        for body in [
            "providers: [{ id: a, kind: systemone, base_url: 'https://h.test', model: m, account_id: abc }]\n",
            "providers: [{ id: a, kind: openrouter_decisions, model: m, zero_data_retention: true }]\n",
            "providers: [{ id: a, kind: cloudflare, model: m }]\n",
            "providers: [{ id: a, kind: cloudflare, model: m, account_id: '../x' }]\n",
            "providers: [{ id: a, kind: vercel_evaluate }]\n",
            "providers: [{ id: a, kind: vercel_evaluate, model: m }]\nuses: { completion_check: { mode: shadow, max_requests_per_day: 3 } }\n",
        ] {
            write(cfg.path(), body);
            assert!(load_file(cfg.path()).is_err(), "must refuse: {body}");
        }
    }

    #[test]
    fn model_aliases_are_told_apart_from_pinned_ids() {
        for alias in ["~typesafe/jev-latest", "jev-latest", "jev-preview"] {
            assert!(is_model_alias(alias), "{alias}");
        }
        for pinned in [
            "typesafe/jev-1.13",
            "jev-1.13.0",
            "typesafe-ai/jev",
            "typesafe/jev",
        ] {
            assert!(!is_model_alias(pinned), "{pinned}");
        }
    }

    #[test]
    fn a_route_spec_without_route_fields_serializes_as_before() {
        let spec = ProviderSpec {
            id: "a".into(),
            kind: ProviderKind::Systemone,
            base_url: Some("https://h.test".into()),
            model: Some("m".into()),
            data_class: DataClass::Hosted,
            key: None,
            answers: BTreeMap::new(),
            account_id: None,
            zero_data_retention: false,
            structured_output: None,
        };
        let yaml = serde_yaml_ng::to_string(&spec).unwrap();
        assert!(
            !yaml.contains("account_id") && !yaml.contains("zero_data_retention"),
            "{yaml}"
        );
    }
}
