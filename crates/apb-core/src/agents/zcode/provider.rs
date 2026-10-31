//! What the zcode headless CLI reads from the home directory: where it is
//! deployed, its built-in and personal provider configs, its credentials
//! (key names only) and settings, and the run-scoped provider config apb
//! points a spawn at.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use super::plan::{
    DEFAULT_FAMILY, FAMILIES, ModelSelection, allowlist_refusal, parse_model, plan_alias,
};

/// Binary name probed on PATH first (a user-made symlink or a future PATH
/// install), before the known home location.
pub const PATH_BIN: &str = "zcode";

/// Where ZCode deploys the headless CLI wrapper, relative to `$HOME`.
pub const HOME_REL_BIN: &str = home_rel_bin!();

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

/// Whether `path` is the ZCode desktop app (an Electron bundle) rather than
/// the headless CLI. The Linux desktop package links `/usr/bin/zcode` to the
/// app, and running it, even with `--version`, boots or wakes the desktop
/// (which then starts its MCP servers in the caller's directory). The
/// bundle is recognised by Electron's files next to the real binary.
pub fn is_desktop_app(path: &Path) -> bool {
    let real = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    real.parent().is_some_and(|dir| {
        dir.join("resources").join("app.asar").exists() || dir.join("chrome-sandbox").exists()
    })
}

/// The PATH `zcode`, unless it is the desktop app ([`is_desktop_app`]).
fn path_cli() -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|dir| dir.join(PATH_BIN))
        .find(|cand| crate::config::program_in_path(&cand.to_string_lossy()))
        .filter(|cand| !is_desktop_app(cand))
}

/// The program apb launches for zcode when the config does not name one:
/// `zcode` when it is on PATH and is not the desktop app, else the known
/// home location when it exists, else plain `zcode` (so the spawn error
/// names the agent).
pub fn default_program() -> String {
    if path_cli().is_some() {
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

/// The reasoning levels ZCode accepts for `model_id`, from the built-in
/// config's `modelRules`: every rule whose `modelMatch` matches the whole id
/// (case-insensitively, as ZCode does) overlays the previous one, so the LAST
/// matching rule that declares `reasoningLevel` wins. `None` when the config
/// is unreadable or no rule matches.
pub fn reasoning_levels(builtin_config: &Path, model_id: &str) -> Option<Vec<String>> {
    let raw = std::fs::read_to_string(builtin_config).ok()?;
    let doc: serde_json::Value = serde_json::from_str(&raw).ok()?;
    let rules = doc
        .pointer("/config/modelConfigRules/modelRules")?
        .as_array()?;
    let mut found = None;
    for rule in rules {
        let Some(pattern) = rule.get("modelMatch").and_then(|m| m.as_str()) else {
            continue;
        };
        let Ok(re) = regex::RegexBuilder::new(&format!("^(?:{pattern})$"))
            .case_insensitive(true)
            .build()
        else {
            continue;
        };
        if !re.is_match(model_id) {
            continue;
        }
        if let Some(values) = rule
            .pointer("/config/optionSpecs/reasoningLevel/values")
            .and_then(|v| v.as_array())
        {
            found = Some(
                values
                    .iter()
                    .filter_map(|v| v.as_str().map(str::to_string))
                    .collect(),
            );
        }
    }
    found
}

/// The reasoning level apb fills in when a zcode model string names none
/// (`GLM-5.3-Flash` without `@...`). ZCode's own default is the model's
/// highest level (`max`), which made medium tasks take many minutes. On a
/// realistic read-only review task (GLM-5.3-Flash, 2026-09) `low` found the
/// same bug as `high` and `max` in 40-75 s instead of 7-9 min, with a third to
/// a half of the tokens, so apb defaults to it. `@high` or `@max` (any
/// offered level) in the profile's model string overrides it per profile. A
/// model that does not offer this level falls back to ZCode's default.
pub const DEFAULT_EFFORT: &str = "low";

/// Makes a selection one ZCode will honor instead of silently replacing.
///
/// ZCode rejects a default selection whose model is not on the plan, or that
/// carries no (or an unsupported) reasoning level, and then quietly runs the
/// first model of the first usable plan instead (GLM-5.3 at max effort, the
/// most expensive one; verified 2026-09-25). So: the model must be one the
/// built-in config enables on that plan, and a missing effort is filled with
/// [`DEFAULT_EFFORT`] (or, when the model does not offer it, ZCode's own
/// default, the model's highest level). When the built-in config names no
/// level list for the model (it is missing or unreadable), the effort is
/// [`DEFAULT_EFFORT`] too: no effort at all is the selection ZCode replaces.
/// Only `account:` plans are checked; a custom provider passes through.
pub fn complete_selection(sel: &mut ModelSelection, builtin_config: &Path) -> Result<(), String> {
    if !sel.provider_id.starts_with("account:") {
        return Ok(());
    }
    // The allowlist is apb's own gate and comes first: a model ZCode offers
    // on the plan but apb does not support (GLM-5.2, GLM-5-Turbo) must fail
    // here with the allowlist named, not run.
    if let Some(refusal) = super::plan::model_refusal(sel) {
        return Err(refusal);
    }
    let pairs = builtin_plan_models(builtin_config);
    let plan = plan_alias(&sel.provider_id).unwrap_or_else(|| sel.provider_id.clone());
    // Model ids match case-insensitively, like everywhere else here; ZCode
    // itself compares exactly, so the config's spelling is written back.
    let offered_here = pairs
        .iter()
        .find(|(p, m)| *p == sel.provider_id && m.eq_ignore_ascii_case(&sel.model_id));
    if let Some((_, spelled)) = offered_here {
        sel.model_id = spelled.clone();
    }
    if !pairs.is_empty() && offered_here.is_none() {
        let offered: Vec<&str> = pairs
            .iter()
            .filter(|(p, _)| *p == sel.provider_id)
            .map(|(_, m)| m.as_str())
            .collect();
        return Err(format!(
            "zcode model `{}` is not offered on plan `{plan}` (offered: {})",
            sel.model_id,
            offered.join(", ")
        ));
    }
    let Some(levels) = reasoning_levels(builtin_config, &sel.model_id) else {
        fill_default_effort(sel);
        return Ok(());
    };
    match &sel.effort {
        None => {
            sel.effort = levels
                .iter()
                .find(|l| *l == DEFAULT_EFFORT)
                .or_else(|| levels.last())
                .cloned();
        }
        Some(e) if !levels.contains(e) => {
            return Err(format!(
                "zcode effort `{e}` is not supported by `{}` (supported: {})",
                sel.model_id,
                levels.join(", ")
            ));
        }
        Some(_) => {}
    }
    Ok(())
}

/// [`DEFAULT_EFFORT`] for a selection on one of ZCode's own plans that names
/// no effort, when no level list is known to pick from.
fn fill_default_effort(sel: &mut ModelSelection) {
    if sel.provider_id.starts_with("account:") && sel.effort.is_none() {
        sel.effort = Some(DEFAULT_EFFORT.to_string());
    }
}

/// [`complete_selection`] against the built-in config under `home`, or the
/// default effort alone when there is none to read.
fn complete_in(sel: &mut ModelSelection, home: &Path) -> Result<(), String> {
    match builtin_config_path(home) {
        Some(builtin) => complete_selection(sel, &builtin),
        None => {
            fill_default_effort(sel);
            Ok(())
        }
    }
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
/// key names are read; every value is skipped while parsing, never held.
pub fn logged_in_providers(home: &Path) -> BTreeSet<String> {
    let Ok(raw) = std::fs::read_to_string(home.join(HOME_REL_CREDENTIALS)) else {
        return BTreeSet::new();
    };
    let Ok(map) =
        serde_json::from_str::<std::collections::BTreeMap<String, serde::de::IgnoredAny>>(&raw)
    else {
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

/// The selection a zcode step with model string `model` runs with, the way
/// [`spawn_env_in`] completes it (the effort filled in), for reporting only:
/// `None` for an empty model (the user's own default) or one that does not
/// resolve. No login check: the step already ran. When the completion
/// refuses the selection now (the built-in config changed since), the parsed
/// selection is reported as it stands.
pub fn effective_selection(home: &Path, model: &str) -> Option<ModelSelection> {
    let parsed = parse_model(model, &account_family(home), &known_model_ids(home))?;
    let mut completed = parsed.clone();
    Some(match complete_in(&mut completed, home) {
        Ok(()) => completed,
        Err(_) => parsed,
    })
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

/// Where the provider config of a spawn without a scoped dir goes: a
/// content-addressed file in apb's own state directory, which only the user
/// owns (a shared temporary directory would let another local user plant
/// the directory first and swap the file).
fn unscoped_config_path(content: &[u8]) -> std::io::Result<PathBuf> {
    let state = crate::config::config_dir()
        .ok_or_else(|| {
            std::io::Error::other("no apb config directory for zcode's provider config")
        })?
        .join("state")
        .join("zcode-provider");
    let digest = crate::content::sha256_hex(content);
    let short: String = digest
        .trim_start_matches("sha256:")
        .chars()
        .take(16)
        .collect();
    Ok(state.join(format!("provider_config-{short}.json")))
}

/// The environment a spawned zcode needs, as `(name, value)` pairs:
///
/// - [`BUILTIN_CONFIG_ENV`] when the caller's env does not set it and the
///   desktop-materialized copy exists (without it the WSL-deployed CLI cannot
///   start a prompt at all);
/// - [`PERSONAL_CONFIG_ENV`] pointing at a run-scoped copy of the user's
///   personal provider config whose `defaultModelSelection` is `model` (see
///   [`parse_model`]). The copy lives in `scoped_dir` when given (the run's
///   agent home), else in a content-addressed file under apb's state
///   directory. An empty `model` leaves the user's own default selection
///   alone.
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
        Some(mut sel) => {
            // The login check keeps its own message (the failure classifier
            // reads "not logged in" as an auth failure), then the allowlist.
            check_plan_usable(&sel, &logged_in_providers(&home)).map_err(std::io::Error::other)?;
            if let Some(refusal) = allowlist_refusal(&sel) {
                return Err(std::io::Error::other(refusal));
            }
            complete_in(&mut sel, &home).map_err(std::io::Error::other)?;
            let dest = match scoped_dir {
                Some(dir) => dir.join("provider_config.json"),
                None => unscoped_config_path(&personal_config_bytes(&source, &sel))?,
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

    /// Without a scoped dir the provider config goes to apb's own state
    /// directory, never to the shared temporary directory, under a name
    /// that follows its content.
    #[test]
    fn an_unscoped_provider_config_lives_in_apb_state() {
        let Some(config) = crate::config::config_dir() else {
            return;
        };
        let a = unscoped_config_path(b"a").unwrap();
        assert!(a.starts_with(config.join("state")), "{}", a.display());
        assert_eq!(a, unscoped_config_path(b"a").unwrap());
        assert_ne!(a, unscoped_config_path(b"b").unwrap());
    }
}
