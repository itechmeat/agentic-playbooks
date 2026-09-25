//! ZCode (Z.ai's desktop agent) support shared by detection, doctor and the
//! engine adapter.
//!
//! ZCode deploys its headless CLI into the user's home at
//! `~/.zcode/server/agents/glm/zcode-agent` (a `sh` wrapper that execs the
//! bundled node on `zcode.cjs`); it is not on PATH. Facts below were verified
//! against zcode-agent 0.16.9 (ZCode desktop 3.14.3):
//!
//! - `-p <prompt>` runs one headless turn; `--mode build|edit|plan|yolo` picks
//!   the permission mode and DEFAULTS TO `yolo` for `-p`, so apb always passes
//!   one explicitly. In `-p` mode every approval request is denied (there is no
//!   interactive gate), so `build` is the safe non-autonomous mode.
//! - `--json` prints ONE pretty-printed JSON object at the end:
//!   `{sessionId, traceId, turnId, response, usage, eventCount, projection}`.
//!   `--resume sess_...` re-enters a session. Failures exit 1 with
//!   `Error: <message> (traceId: ...)` on stderr and nothing on stdout.
//! - There is no `--model` flag. The model comes from the personal provider
//!   config's `defaultModelSelection` (`{providerId, modelId, options:
//!   {reasoningLevel}}`), read from the file named by
//!   `ZCODE_PERSONAL_PROVIDER_CONFIG_FILE`. The standalone CLI also needs
//!   `ZCODE_BUILTIN_PROVIDER_CONFIG_FILE`, because the WSL deployment does not
//!   ship the `provider/zcode-builtin.json` it looks for next to `zcode.cjs`;
//!   the desktop materializes that file under `~/.zcode/v2/runtime/provider/`.
//! - The same model is served by several plans ("Z.ai Individual Coding Plan",
//!   "Start Plan", ...), each a separate provider id such as
//!   `account:zai-individual-coding-plan`. apb therefore takes a
//!   plan-qualified model string, see [`parse_model`].
//! - The standalone CLI only sees a plan after `zcode-agent login` stored an
//!   `account-provider:<providerId>:identity` credential next to the plan's
//!   api key in `~/.zcode/v2/credentials.json`. Only the KEY NAMES of that file
//!   are ever looked at here, never a value. The desktop's own login does not
//!   write that identity key, and in 0.16.9 the standalone account source
//!   covers ONLY the `individual-coding-plan` providers: the Start (free),
//!   Team and Idle plans the desktop offers are not reachable headless.
//! - A selection naming a plan the CLI cannot use is NOT an error in ZCode: it
//!   silently falls back to the first usable plan. That would spend the paid
//!   plan for a step meant to run on another one, so apb refuses such a
//!   selection before spawning ([`check_plan_usable`]).

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

/// The agent id.
pub const AGENT_ID: &str = "zcode";

/// Binary name probed on PATH first (a user-made symlink or a future PATH
/// install), before the known home location.
pub const PATH_BIN: &str = "zcode";

/// Where ZCode deploys the headless CLI wrapper, relative to `$HOME`.
pub const HOME_REL_BIN: &str = ".zcode/server/agents/glm/zcode-agent";

/// Env var naming the built-in provider config file.
pub const BUILTIN_CONFIG_ENV: &str = "ZCODE_BUILTIN_PROVIDER_CONFIG_FILE";

/// Env var naming the personal provider config file (holds
/// `defaultModelSelection`).
pub const PERSONAL_CONFIG_ENV: &str = "ZCODE_PERSONAL_PROVIDER_CONFIG_FILE";

/// The built-in provider config the desktop materializes, relative to `$HOME`.
pub const HOME_REL_BUILTIN_CONFIG: &str = ".zcode/v2/runtime/provider/bundled/zcode-builtin.json";

/// The user's personal provider config, relative to `$HOME`.
pub const HOME_REL_PERSONAL_CONFIG: &str = ".zcode/v2/provider_config.json";

/// Shared ZCode credential store, relative to `$HOME` (key names only).
pub const HOME_REL_CREDENTIALS: &str = ".zcode/v2/credentials.json";

/// ZCode app settings (account family domain), relative to `$HOME`.
pub const HOME_REL_SETTINGS: &str = ".zcode/v2/setting.json";

/// The plan kinds ZCode knows, in apb's preference order: paid plans first, so
/// that a listing and an unqualified model both favor the paid plan.
/// `(provider id suffix, short alias suffix)`.
const PLAN_KINDS: &[(&str, &str)] = &[
    ("individual-coding-plan", "individual"),
    ("team-coding-plan", "team"),
    ("start-plan", "start"),
    ("offpeak-idle-plan", "idle"),
];

/// The only plan kind the standalone (headless) CLI's account source covers in
/// zcode-agent 0.16.9.
const STANDALONE_PLAN_KIND: &str = "individual-coding-plan";

/// Account families (`providerFamilyDomain` in ZCode's settings).
const FAMILIES: &[&str] = &["zai", "bigmodel"];

/// Family used when the settings file does not name one.
pub const DEFAULT_FAMILY: &str = "zai";

/// A parsed apb model string for zcode.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelSelection {
    /// ZCode provider id, e.g. `account:zai-individual-coding-plan`.
    pub provider_id: String,
    /// ZCode model id, e.g. `GLM-5.3`.
    pub model_id: String,
    /// Reasoning level (ZCode's "effort"), e.g. `max`. `None` lets ZCode pick
    /// its default (the highest level the model offers).
    pub effort: Option<String>,
}

impl ModelSelection {
    /// The value ZCode's `defaultModelSelection` key takes.
    pub fn to_json(&self) -> serde_json::Value {
        let mut v = serde_json::json!({
            "providerId": self.provider_id,
            "modelId": self.model_id,
        });
        if let Some(e) = &self.effort {
            v["options"] = serde_json::json!({ "reasoningLevel": e });
        }
        v
    }

    /// Billing account this selection spends: one per plan. A quota or auth
    /// failure blocks this key, not the whole agent, so a fallback onto the
    /// same model on another plan stays allowed.
    pub fn account_key(&self) -> String {
        format!("{AGENT_ID}:{}", self.provider_id)
    }
}

/// Short alias of a ZCode provider id: `account:zai-individual-coding-plan`
/// -> `zai-individual`. `None` for a provider id that is not a known plan
/// (custom providers keep their own id).
pub fn plan_alias(provider_id: &str) -> Option<String> {
    let rest = provider_id.strip_prefix("account:")?;
    for family in FAMILIES {
        if let Some(kind) = rest.strip_prefix(&format!("{family}-")) {
            for (suffix, short) in PLAN_KINDS {
                if kind == *suffix {
                    return Some(format!("{family}-{short}"));
                }
            }
        }
    }
    None
}

/// Resolves the plan part of a model string to a ZCode provider id. Accepts
/// the short alias (`zai-individual`, `zai-start`), the provider id without
/// its `account:` prefix (`zai-start-plan`), the full provider id
/// (`account:zai-start-plan`), and a bare kind (`individual`, `start`,
/// `start-plan`) which takes `family`. Anything else is passed through
/// verbatim, so a custom provider from the user's personal config works too.
pub fn resolve_plan(plan: &str, family: &str) -> String {
    let p = plan.trim();
    let lower = p.to_ascii_lowercase();
    if lower.starts_with("account:") {
        return p.to_string();
    }
    for fam in FAMILIES {
        if let Some(kind) = lower.strip_prefix(&format!("{fam}-")) {
            for (suffix, short) in PLAN_KINDS {
                if kind == *suffix || kind == *short {
                    return format!("account:{fam}-{suffix}");
                }
            }
        }
    }
    for (suffix, short) in PLAN_KINDS {
        if lower == *suffix || lower == *short {
            return format!("account:{family}-{suffix}");
        }
    }
    p.to_string()
}

/// Parses an apb zcode model string: `[<plan>/]<model>[@<effort>]`.
///
/// - `zai-individual/GLM-5.3` - the model on the paid Z.ai Individual plan;
/// - `zai-start/GLM-5.3@max` - the same model on the free Start plan, max
///   effort;
/// - `GLM-5.3` - unqualified: resolves deterministically to the PAID
///   individual plan of the account family (`family`, from ZCode's settings,
///   `zai` by default). Qualify the model to use any other plan.
///
/// The model id is matched case-insensitively against `known_models` (the
/// ids from ZCode's built-in provider config) and rewritten to its canonical
/// spelling, so `glm-5.3` works. An empty string is `None`: ZCode then keeps
/// the user's own default selection.
pub fn parse_model(raw: &str, family: &str, known_models: &[String]) -> Option<ModelSelection> {
    let s = raw.trim();
    if s.is_empty() {
        return None;
    }
    let (body, effort) = match s.rsplit_once('@') {
        Some((b, e)) if !e.trim().is_empty() => (b.trim(), Some(e.trim().to_ascii_lowercase())),
        Some((b, _)) => (b.trim(), None),
        None => (s, None),
    };
    let (provider_id, model) = match body.split_once('/') {
        Some((plan, model)) => (resolve_plan(plan, family), model.trim()),
        None => (resolve_plan("individual", family), body),
    };
    if model.is_empty() {
        return None;
    }
    let model_id = known_models
        .iter()
        .find(|k| k.eq_ignore_ascii_case(model))
        .cloned()
        .unwrap_or_else(|| model.to_string());
    Some(ModelSelection {
        provider_id,
        model_id,
        effort,
    })
}

/// `$HOME`, if set and non-empty.
pub fn home_dir() -> Option<PathBuf> {
    std::env::var("HOME")
        .ok()
        .filter(|h| !h.is_empty())
        .map(PathBuf::from)
}

/// The known home location of the headless CLI, when it exists.
pub fn home_binary(home: &Path) -> Option<PathBuf> {
    let p = home.join(HOME_REL_BIN);
    crate::config::program_in_path(&p.to_string_lossy()).then_some(p)
}

/// The program apb launches for zcode when the config does not name one:
/// `zcode` when it is on PATH, else the known home location when it exists,
/// else plain `zcode` (so the spawn error names the agent).
pub fn default_program() -> String {
    if crate::config::program_in_path(PATH_BIN) {
        return PATH_BIN.to_string();
    }
    home_dir()
        .and_then(|h| home_binary(&h))
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_else(|| PATH_BIN.to_string())
}

/// The built-in provider config file: `$ZCODE_BUILTIN_PROVIDER_CONFIG_FILE`
/// when set, else the desktop-materialized bundled copy when it exists.
pub fn builtin_config_path(home: &Path) -> Option<PathBuf> {
    if let Ok(p) = std::env::var(BUILTIN_CONFIG_ENV)
        && !p.trim().is_empty()
    {
        return Some(PathBuf::from(p));
    }
    let p = home.join(HOME_REL_BUILTIN_CONFIG);
    p.is_file().then_some(p)
}

/// The user's personal provider config file (may not exist).
pub fn personal_config_path(home: &Path) -> PathBuf {
    if let Ok(p) = std::env::var(PERSONAL_CONFIG_ENV)
        && !p.trim().is_empty()
    {
        return PathBuf::from(p);
    }
    home.join(HOME_REL_PERSONAL_CONFIG)
}

/// The account family from ZCode's settings (`providerFamilyDomain`), else
/// [`DEFAULT_FAMILY`].
pub fn account_family(home: &Path) -> String {
    std::fs::read_to_string(home.join(HOME_REL_SETTINGS))
        .ok()
        .and_then(|raw| serde_json::from_str::<serde_json::Value>(&raw).ok())
        .and_then(|v| {
            v.get("providerFamilyDomain")
                .and_then(|d| d.as_str())
                .map(str::to_string)
        })
        .filter(|d| FAMILIES.contains(&d.as_str()))
        .unwrap_or_else(|| DEFAULT_FAMILY.to_string())
}

/// `(provider id, model id)` pairs the built-in config enables for the plans
/// (`builtinProviderModelRules`), in file order. Empty when the file is
/// missing or unreadable.
pub fn builtin_plan_models(builtin_config: &Path) -> Vec<(String, String)> {
    let Ok(raw) = std::fs::read_to_string(builtin_config) else {
        return Vec::new();
    };
    let Ok(doc) = serde_json::from_str::<serde_json::Value>(&raw) else {
        return Vec::new();
    };
    let Some(rules) = doc
        .pointer("/config/modelConfigRules/builtinProviderModelRules")
        .and_then(|r| r.as_array())
    else {
        return Vec::new();
    };
    rules
        .iter()
        .filter(|r| {
            r.pointer("/config/enabled")
                .and_then(|e| e.as_bool())
                .unwrap_or(true)
        })
        .filter_map(|r| {
            Some((
                r.get("providerId")?.as_str()?.to_string(),
                r.get("modelId")?.as_str()?.to_string(),
            ))
        })
        .collect()
}

/// Distinct model ids named in the built-in config, for case-insensitive
/// canonicalization in [`parse_model`].
pub fn known_model_ids(home: &Path) -> Vec<String> {
    let mut seen = BTreeSet::new();
    builtin_config_path(home)
        .map(|p| builtin_plan_models(&p))
        .unwrap_or_default()
        .into_iter()
        .filter_map(|(_, m)| seen.insert(m.clone()).then_some(m))
        .collect()
}

/// Provider ids the standalone CLI is logged in to: those with an
/// `account-provider:<providerId>:identity` key in the credential store. Only
/// key names are inspected; values are never read out of the parsed map.
pub fn logged_in_providers(home: &Path) -> BTreeSet<String> {
    let Ok(raw) = std::fs::read_to_string(home.join(HOME_REL_CREDENTIALS)) else {
        return BTreeSet::new();
    };
    let Ok(serde_json::Value::Object(map)) = serde_json::from_str::<serde_json::Value>(&raw) else {
        return BTreeSet::new();
    };
    map.keys()
        .filter_map(|k| {
            k.strip_prefix("account-provider:")
                .and_then(|r| r.strip_suffix(":identity"))
                .map(str::to_string)
        })
        .collect()
}

/// Rank of a provider id in [`PLAN_KINDS`] order (unknown last).
fn plan_rank(provider_id: &str) -> usize {
    PLAN_KINDS
        .iter()
        .position(|(suffix, _)| provider_id.ends_with(suffix))
        .unwrap_or(PLAN_KINDS.len())
}

/// The plan-qualified model list apb offers for zcode: `alias/ModelId` for
/// every model the built-in config enables on a plan of the account `family`
/// that the headless CLI can use - the plans it is logged in to, or, before
/// any login, the individual coding plan a login would enable. Paid plans
/// first; deterministic (stable sort by plan rank, then file order).
pub fn plan_model_list(
    pairs: &[(String, String)],
    family: &str,
    logged_in: &BTreeSet<String>,
) -> Vec<String> {
    let prefix = format!("account:{family}-");
    let mut rows: Vec<&(String, String)> = pairs
        .iter()
        .filter(|(p, _)| p.starts_with(&prefix))
        .filter(|(p, _)| {
            if logged_in.is_empty() {
                p.ends_with(STANDALONE_PLAN_KIND)
            } else {
                logged_in.contains(p)
            }
        })
        .collect();
    rows.sort_by_key(|(p, _)| plan_rank(p));
    let mut seen = BTreeSet::new();
    rows.into_iter()
        .filter_map(|(p, m)| {
            let plan = plan_alias(p).unwrap_or_else(|| p.clone());
            let id = format!("{plan}/{m}");
            seen.insert(id.clone()).then_some(id)
        })
        .collect()
}

/// Extracts the final reply text from zcode's stdout. `--json` prints one
/// pretty JSON object with a `response` string; `--output-format stream-json`
/// prints NDJSON events ending in a `{"type":"result",...,"response":...}`
/// line. `None` when neither shape is present (plain text output), so the
/// caller keeps the raw stdout.
pub fn response_text(stdout: &str) -> Option<String> {
    let trimmed = stdout.trim();
    if trimmed.starts_with('{')
        && let Ok(v) = serde_json::from_str::<serde_json::Value>(trimmed)
        && let Some(r) = v.get("response").and_then(|r| r.as_str())
    {
        return Some(r.to_string());
    }
    let mut found = None;
    for line in trimmed.lines() {
        let line = line.trim();
        if !line.starts_with('{') {
            continue;
        }
        let Ok(v) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        if v.get("type").and_then(|t| t.as_str()) == Some("result")
            && let Some(r) = v.get("response").and_then(|r| r.as_str())
        {
            found = Some(r.to_string());
        }
    }
    found
}

/// The session id (`sess_...`) from zcode's stdout, in either output shape
/// (see [`response_text`]).
pub fn session_id(stdout: &str) -> Option<String> {
    let trimmed = stdout.trim();
    let pick = |v: &serde_json::Value| {
        v.get("sessionId")
            .and_then(|s| s.as_str())
            .filter(|s| !s.is_empty())
            .map(str::to_string)
    };
    if trimmed.starts_with('{')
        && let Ok(v) = serde_json::from_str::<serde_json::Value>(trimmed)
        && let Some(s) = pick(&v)
    {
        return Some(s);
    }
    let mut found = None;
    for line in trimmed.lines() {
        let line = line.trim();
        if line.starts_with('{')
            && let Ok(v) = serde_json::from_str::<serde_json::Value>(line)
            && v.get("type").and_then(|t| t.as_str()) == Some("result")
            && let Some(s) = pick(&v)
        {
            found = Some(s);
        }
    }
    found
}

/// The personal provider config for one run: the user's own file (when
/// readable) with `config.defaultModelSelection` replaced by `sel`, or a
/// minimal valid config around `sel` when there is none. The user's file is
/// never modified.
pub fn personal_config_bytes(source: &Path, sel: &ModelSelection) -> Vec<u8> {
    let mut doc = std::fs::read_to_string(source)
        .ok()
        .and_then(|raw| serde_json::from_str::<serde_json::Value>(&raw).ok())
        .filter(|v| v.get("config").is_some_and(|c| c.is_object()))
        .unwrap_or_else(|| {
            serde_json::json!({
                "schemaVersion": 1,
                "config": {
                    "providerConfigRules": { "providerRules": [] },
                    "modelConfigRules": {
                        "providerModelRules": [],
                        "manualProviderModelRules": []
                    }
                }
            })
        });
    doc["config"]["defaultModelSelection"] = sel.to_json();
    let mut body = serde_json::to_vec_pretty(&doc).unwrap_or_default();
    body.push(b'\n');
    body
}

/// Writes [`personal_config_bytes`] to `dest` (0600: the personal config can
/// carry custom provider settings).
pub fn write_personal_config(
    source: &Path,
    sel: &ModelSelection,
    dest: &Path,
) -> std::io::Result<()> {
    let body = personal_config_bytes(source, sel);
    if std::fs::read(dest).is_ok_and(|cur| cur == body) {
        return Ok(());
    }
    crate::fsutil::atomic_write_private(dest, &body)
}

/// Refuses a selection the headless CLI cannot honor: an `account:` plan it is
/// not logged in to. ZCode itself would silently run the first usable plan
/// instead, so a free-plan fallback step would quietly spend the paid plan.
/// The message says "not logged in", which apb's failure classifier reads as
/// an auth failure: that plan is skipped for the rest of the fallback chain.
/// Custom (non-`account:`) providers are not checked.
pub fn check_plan_usable(sel: &ModelSelection, logged_in: &BTreeSet<String>) -> Result<(), String> {
    if !sel.provider_id.starts_with("account:") || logged_in.contains(&sel.provider_id) {
        return Ok(());
    }
    let plan = plan_alias(&sel.provider_id).unwrap_or_else(|| sel.provider_id.clone());
    Err(format!(
        "zcode headless CLI is not logged in to plan `{plan}` ({}): run `~/{HOME_REL_BIN} login` \
         (it only covers the individual coding plan; the desktop's other plans are not reachable headless)",
        sel.provider_id
    ))
}

/// The environment a spawned zcode needs, as `(name, value)` pairs:
///
/// - [`BUILTIN_CONFIG_ENV`] when the caller's env does not set it and the
///   desktop-materialized copy exists (without it the WSL-deployed CLI cannot
///   start a prompt at all);
/// - [`PERSONAL_CONFIG_ENV`] pointing at a run-scoped copy of the user's
///   personal provider config whose `defaultModelSelection` is `model` (see
///   [`parse_model`]). The copy lives in `scoped_dir` when given (the run's
///   agent home), else in a content-addressed file under the system temp
///   dir. An empty `model` leaves the user's own default selection alone.
pub fn spawn_env(
    model: &str,
    scoped_dir: Option<&Path>,
) -> std::io::Result<Vec<(String, PathBuf)>> {
    match home_dir() {
        Some(home) => spawn_env_in(&home, model, scoped_dir),
        None => Ok(Vec::new()),
    }
}

/// [`spawn_env`] against an explicit home directory (tests drive it with a
/// temporary one).
pub fn spawn_env_in(
    home: &Path,
    model: &str,
    scoped_dir: Option<&Path>,
) -> std::io::Result<Vec<(String, PathBuf)>> {
    let home = home.to_path_buf();
    let mut env = Vec::new();
    if std::env::var(BUILTIN_CONFIG_ENV).map_or(true, |v| v.trim().is_empty())
        && let Some(p) = builtin_config_path(&home)
    {
        env.push((BUILTIN_CONFIG_ENV.to_string(), p));
    }
    let source = personal_config_path(&home);
    let known = known_model_ids(&home);
    let family = account_family(&home);
    match parse_model(model, &family, &known) {
        Some(sel) => {
            check_plan_usable(&sel, &logged_in_providers(&home)).map_err(std::io::Error::other)?;
            let dest = match scoped_dir {
                Some(dir) => dir.join("provider_config.json"),
                None => {
                    let digest = crate::content::sha256_hex(&personal_config_bytes(&source, &sel));
                    let short: String = digest
                        .trim_start_matches("sha256:")
                        .chars()
                        .take(16)
                        .collect();
                    std::env::temp_dir()
                        .join(format!("apb-zcode-{}", std::process::id()))
                        .join(format!("provider_config-{short}.json"))
                }
            };
            write_personal_config(&source, &sel, &dest)?;
            env.push((PERSONAL_CONFIG_ENV.to_string(), dest));
        }
        None => {
            // ZCode needs both files named, or it tries to locate the built-in
            // one next to zcode.cjs and fails; name the user's own personal
            // config so the desktop's default selection applies.
            if std::env::var(PERSONAL_CONFIG_ENV).map_or(true, |v| v.trim().is_empty()) {
                env.push((PERSONAL_CONFIG_ENV.to_string(), source));
            }
        }
    }
    Ok(env)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn known() -> Vec<String> {
        ["GLM-5.3", "GLM-5.3-Flash", "GLM-5.2", "GLM-5-Turbo"]
            .iter()
            .map(|s| s.to_string())
            .collect()
    }

    #[test]
    fn plan_aliases_round_trip() {
        for (pid, alias) in [
            ("account:zai-individual-coding-plan", "zai-individual"),
            ("account:zai-team-coding-plan", "zai-team"),
            ("account:zai-start-plan", "zai-start"),
            ("account:zai-offpeak-idle-plan", "zai-idle"),
            ("account:bigmodel-start-plan", "bigmodel-start"),
        ] {
            assert_eq!(plan_alias(pid).as_deref(), Some(alias));
            assert_eq!(resolve_plan(alias, "zai"), pid);
        }
        assert_eq!(plan_alias("my-openai"), None);
    }

    #[test]
    fn resolve_plan_accepts_every_spelling() {
        let want = "account:zai-start-plan";
        for s in [
            "zai-start",
            "zai-start-plan",
            "account:zai-start-plan",
            "start",
            "start-plan",
            "ZAI-Start",
        ] {
            assert_eq!(resolve_plan(s, "zai"), want, "{s}");
        }
        // A bare kind takes the account family.
        assert_eq!(
            resolve_plan("individual", "bigmodel"),
            "account:bigmodel-individual-coding-plan"
        );
        // Unknown providers pass through verbatim.
        assert_eq!(resolve_plan("my-provider", "zai"), "my-provider");
    }

    #[test]
    fn qualified_models_keep_their_plan() {
        let paid = parse_model("zai-individual/glm-5.3", "zai", &known()).unwrap();
        assert_eq!(paid.provider_id, "account:zai-individual-coding-plan");
        assert_eq!(paid.model_id, "GLM-5.3");
        assert_eq!(paid.effort, None);
        let free = parse_model("zai-start/GLM-5.3@Max", "zai", &known()).unwrap();
        assert_eq!(free.provider_id, "account:zai-start-plan");
        assert_eq!(free.model_id, "GLM-5.3");
        assert_eq!(free.effort.as_deref(), Some("max"));
        assert_ne!(paid.account_key(), free.account_key());
    }

    #[test]
    fn unqualified_model_resolves_to_the_paid_individual_plan() {
        let s = parse_model("GLM-5.3-Flash", "zai", &known()).unwrap();
        assert_eq!(s.provider_id, "account:zai-individual-coding-plan");
        assert_eq!(s.model_id, "GLM-5.3-Flash");
        let b = parse_model("glm-5.2@high", "bigmodel", &known()).unwrap();
        assert_eq!(b.provider_id, "account:bigmodel-individual-coding-plan");
        assert_eq!(b.model_id, "GLM-5.2");
        assert_eq!(b.effort.as_deref(), Some("high"));
    }

    #[test]
    fn empty_or_planless_models_are_none() {
        assert_eq!(parse_model("", "zai", &known()), None);
        assert_eq!(parse_model("  ", "zai", &known()), None);
        assert_eq!(parse_model("zai-start/", "zai", &known()), None);
    }

    #[test]
    fn unknown_model_ids_pass_through() {
        let s = parse_model("my-provider/some-model", "zai", &known()).unwrap();
        assert_eq!(s.provider_id, "my-provider");
        assert_eq!(s.model_id, "some-model");
    }

    #[test]
    fn selection_json_matches_zcode_schema() {
        let s = parse_model("zai-start/GLM-5.3@low", "zai", &known()).unwrap();
        assert_eq!(
            s.to_json(),
            serde_json::json!({
                "providerId": "account:zai-start-plan",
                "modelId": "GLM-5.3",
                "options": {"reasoningLevel": "low"}
            })
        );
        let d = parse_model("GLM-5.3", "zai", &known()).unwrap();
        assert!(d.to_json().get("options").is_none());
    }

    #[test]
    fn plans_the_cli_is_not_logged_in_to_are_refused() {
        let sel = parse_model("zai-start/GLM-5.3", "zai", &known()).unwrap();
        let paid: BTreeSet<String> = ["account:zai-individual-coding-plan".to_string()].into();
        let err = check_plan_usable(&sel, &paid).unwrap_err();
        assert!(err.contains("not logged in"), "{err}");
        assert!(err.contains("zai-start"), "{err}");
        let ok = parse_model("zai-individual/GLM-5.3", "zai", &known()).unwrap();
        assert!(check_plan_usable(&ok, &paid).is_ok());
        let custom = parse_model("my-provider/model", "zai", &known()).unwrap();
        assert!(check_plan_usable(&custom, &BTreeSet::new()).is_ok());
    }

    #[test]
    fn plan_model_list_orders_paid_first_and_filters_family_and_login() {
        let pairs: Vec<(String, String)> = [
            ("account:zai-start-plan", "GLM-5.3"),
            ("account:zai-individual-coding-plan", "GLM-5.3"),
            ("account:zai-individual-coding-plan", "GLM-5.3-Flash"),
            ("account:bigmodel-start-plan", "GLM-5.3"),
            ("account:zai-offpeak-idle-plan", "GLM-5.3"),
        ]
        .iter()
        .map(|(a, b)| (a.to_string(), b.to_string()))
        .collect();
        // Before any login: the individual plan a login would enable.
        let none = plan_model_list(&pairs, "zai", &BTreeSet::new());
        assert_eq!(
            none,
            vec!["zai-individual/GLM-5.3", "zai-individual/GLM-5.3-Flash"]
        );
        // Logged in to several plans: all of them, paid first.
        let both: BTreeSet<String> = [
            "account:zai-start-plan".to_string(),
            "account:zai-individual-coding-plan".to_string(),
        ]
        .into();
        assert_eq!(
            plan_model_list(&pairs, "zai", &both),
            vec![
                "zai-individual/GLM-5.3",
                "zai-individual/GLM-5.3-Flash",
                "zai-start/GLM-5.3",
            ]
        );
    }

    #[test]
    fn logged_in_providers_reads_key_names_only() {
        let home = tempfile::tempdir().unwrap();
        let v2 = home.path().join(".zcode/v2");
        std::fs::create_dir_all(&v2).unwrap();
        std::fs::write(
            v2.join("credentials.json"),
            r#"{"oauth:zai:access_token":"x","account-provider:account:zai-start-plan:identity":"id","account-provider:coding-plan:account:zai-start-plan:account:id:api-key":"k"}"#,
        )
        .unwrap();
        let got = logged_in_providers(home.path());
        assert_eq!(got, BTreeSet::from(["account:zai-start-plan".to_string()]));
        assert!(logged_in_providers(tempfile::tempdir().unwrap().path()).is_empty());
    }

    /// Lays out a fake ZCode home: built-in config with two plans, a personal
    /// config, and credentials logged in to the individual plan only.
    fn fake_home() -> tempfile::TempDir {
        let home = tempfile::tempdir().unwrap();
        let v2 = home.path().join(".zcode/v2");
        let bundled = home.path().join(HOME_REL_BUILTIN_CONFIG);
        std::fs::create_dir_all(bundled.parent().unwrap()).unwrap();
        std::fs::write(
            &bundled,
            r#"{"schemaVersion":1,"revision":30,"config":{"modelConfigRules":{"builtinProviderModelRules":[
                {"modelId":"GLM-5.3","config":{"enabled":true},"providerId":"account:zai-individual-coding-plan"},
                {"modelId":"GLM-5.3-Flash","config":{"enabled":true},"providerId":"account:zai-individual-coding-plan"},
                {"modelId":"GLM-5.3","config":{"enabled":true},"providerId":"account:zai-start-plan"}]}}}"#,
        )
        .unwrap();
        std::fs::write(
            v2.join("provider_config.json"),
            r#"{"schemaVersion":1,"config":{"providerConfigRules":{"providerRules":[]},"modelConfigRules":{"providerModelRules":[],"manualProviderModelRules":[]}}}"#,
        )
        .unwrap();
        std::fs::write(v2.join("setting.json"), r#"{"providerFamilyDomain":"zai"}"#).unwrap();
        std::fs::write(
            v2.join("credentials.json"),
            r#"{"account-provider:account:zai-individual-coding-plan:identity":"id"}"#,
        )
        .unwrap();
        home
    }

    #[test]
    fn spawn_env_points_zcode_at_a_scoped_model_selection() {
        if std::env::var_os(BUILTIN_CONFIG_ENV).is_some()
            || std::env::var_os(PERSONAL_CONFIG_ENV).is_some()
        {
            return; // the caller's own override wins; nothing to assert here
        }
        let home = fake_home();
        let scoped = home.path().join("run/agent-home/zcode/n1");
        let env = spawn_env_in(
            home.path(),
            "zai-individual/glm-5.3-flash@low",
            Some(&scoped),
        )
        .unwrap();
        let get = |k: &str| env.iter().find(|(n, _)| n == k).map(|(_, v)| v.clone());
        assert_eq!(
            get(BUILTIN_CONFIG_ENV).unwrap(),
            home.path().join(HOME_REL_BUILTIN_CONFIG)
        );
        let personal = get(PERSONAL_CONFIG_ENV).unwrap();
        assert_eq!(personal, scoped.join("provider_config.json"));
        let doc: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&personal).unwrap()).unwrap();
        assert_eq!(
            doc["config"]["defaultModelSelection"],
            serde_json::json!({
                "providerId": "account:zai-individual-coding-plan",
                "modelId": "GLM-5.3-Flash",
                "options": {"reasoningLevel": "low"}
            })
        );
        // The user's own personal config is untouched.
        let own = std::fs::read_to_string(home.path().join(HOME_REL_PERSONAL_CONFIG)).unwrap();
        assert!(!own.contains("defaultModelSelection"));

        // No model: the user's own personal config is named as-is.
        let env = spawn_env_in(home.path(), "", None).unwrap();
        assert_eq!(
            env.iter()
                .find(|(n, _)| n == PERSONAL_CONFIG_ENV)
                .unwrap()
                .1,
            home.path().join(HOME_REL_PERSONAL_CONFIG)
        );

        // A plan the CLI is not logged in to is refused before any spawn.
        let err = spawn_env_in(home.path(), "zai-start/GLM-5.3", Some(&scoped)).unwrap_err();
        assert!(err.to_string().contains("not logged in"), "{err}");
    }

    #[test]
    fn personal_config_copy_overrides_only_the_default_selection() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("src.json");
        std::fs::write(
            &src,
            r#"{"schemaVersion":1,"config":{"providerConfigRules":{"providerRules":[{"x":1}]},"modelConfigRules":{"providerModelRules":[],"manualProviderModelRules":[]},"defaultModelSelection":{"providerId":"a","modelId":"b"}}}"#,
        )
        .unwrap();
        let before = std::fs::read(&src).unwrap();
        let sel = parse_model("zai-start/GLM-5.3", "zai", &known()).unwrap();
        let dest = dir.path().join("out/provider_config.json");
        write_personal_config(&src, &sel, &dest).unwrap();
        assert_eq!(std::fs::read(&src).unwrap(), before, "source untouched");
        let out: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&dest).unwrap()).unwrap();
        assert_eq!(out["config"]["defaultModelSelection"], sel.to_json());
        assert_eq!(
            out["config"]["providerConfigRules"]["providerRules"][0]["x"],
            1
        );

        // No source: a minimal valid config.
        let dest2 = dir.path().join("fresh.json");
        write_personal_config(&dir.path().join("missing.json"), &sel, &dest2).unwrap();
        let out2: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&dest2).unwrap()).unwrap();
        assert_eq!(out2["schemaVersion"], 1);
        assert_eq!(out2["config"]["defaultModelSelection"], sel.to_json());
    }
}
