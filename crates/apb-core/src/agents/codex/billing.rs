//! What codex's work is paid on, from its local login facts.
//!
//! What it reads, by name and by the presence of keys, never a credential
//! value:
//!
//! 1. the login variables that reach the spawned process (`CODEX_API_KEY`,
//!    `OPENAI_API_KEY`) and the endpoint variable (`OPENAI_BASE_URL`);
//! 2. the top-level keys of `auth.json` in codex's config root
//!    (`$CODEX_HOME`, else `~/.codex`): an `OPENAI_API_KEY` key with a value
//!    is an api key login, a `tokens` key with a value is an account login
//!    (a ChatGPT plan);
//! 3. whether `config.toml` there names a `model_provider` in any TOML
//!    spelling of the key (another provider charges what it charges).
//!
//! Then: a key variable is billed; an endpoint variable or a named provider
//! leaves the basis unknown; an api key login is billed; an account login
//! is a subscription equivalent; anything else (no login file, a file that
//! does not read, an unfamiliar shape) is unknown. Any doubt reads as
//! billed or unknown, never as a subscription.
//!
//! codex rewrites `auth.json` when it refreshes a token, so a reading's
//! evidence holds only the facts drawn from the files, never their change
//! times: a refresh while the agent works leaves the basis as it was.

use std::path::{Path, PathBuf};

use crate::agents::CostBasis;
use crate::agents::cost_basis::{CostFacts, CostReading};
use crate::fsutil::SETTINGS_FILE_MAX_BYTES;

/// The variables that carry a key codex sends as is.
const KEY_VARS: &[&str] = &["CODEX_API_KEY", "OPENAI_API_KEY"];

/// The variable that points codex at another endpoint.
const ENDPOINT: &str = "OPENAI_BASE_URL";

pub(super) fn cost_basis(f: &CostFacts) -> Option<CostReading> {
    let mut facts: Vec<String> = Vec::new();
    let basis = decide(f, &mut facts);
    facts.push(basis.key().to_string());
    Some(CostReading::new(
        basis,
        crate::content::short_nonce("codex-basis", &facts),
    ))
}

fn decide(f: &CostFacts, facts: &mut Vec<String>) -> CostBasis {
    if let Some(name) = KEY_VARS.iter().find(|n| (f.present)(n)) {
        facts.push(format!("env {name}"));
        return CostBasis::Billed;
    }
    if (f.present)(ENDPOINT) {
        facts.push(format!("env {ENDPOINT}"));
        return CostBasis::Unknown;
    }
    let Some(root) = root(f) else {
        facts.push("no config root".into());
        return CostBasis::Unknown;
    };
    facts.push(format!("root {}", root.display()));
    match names_a_provider(&root.join("config.toml")) {
        Some(false) => {}
        Some(true) => {
            facts.push("config model_provider".into());
            return CostBasis::Unknown;
        }
        None => {
            facts.push("config unreadable".into());
            return CostBasis::Unknown;
        }
    }
    let login = login_kind(&root.join("auth.json"));
    facts.push(format!("login {login:?}"));
    match login {
        Login::ApiKey => CostBasis::Billed,
        Login::Account => CostBasis::SubscriptionEquivalent,
        Login::None | Login::Unreadable => CostBasis::Unknown,
    }
}

/// codex's config root for the spawned process: its `CODEX_HOME`, else
/// `<home>/.codex`. A run-scoped home holds a verbatim copy of the source
/// login, so reading the source tells the same.
fn root(f: &CostFacts) -> Option<PathBuf> {
    match (f.setting)(super::home::CODEX_HOME_ENV) {
        Some(v) if !v.is_empty() => Some(PathBuf::from(v)),
        _ => f.home.map(|h| h.join(".codex")),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Login {
    ApiKey,
    Account,
    None,
    Unreadable,
}

/// The login `auth.json` at `path` records, by its top-level key names.
fn login_kind(path: &Path) -> Login {
    let raw = match crate::fsutil::read_settings_file(path, SETTINGS_FILE_MAX_BYTES) {
        Ok(Some(raw)) => raw,
        Ok(None) => return Login::None,
        Err(_) => return Login::Unreadable,
    };
    let Ok(serde_json::Value::Object(map)) = serde_json::from_str::<serde_json::Value>(&raw) else {
        return Login::Unreadable;
    };
    let has = |k: &str| map.get(k).is_some_and(|v| !v.is_null());
    if has("OPENAI_API_KEY") {
        Login::ApiKey
    } else if has("tokens") {
        Login::Account
    } else {
        Login::Unreadable
    }
}

/// The key that chooses the provider codex sends its requests to.
const PROVIDER_KEY: &str = "model_provider";

/// Whether `raw` holds the key's name as a whole bare key (not the start of
/// a longer one, such as the `model_providers` table that defines
/// providers without choosing one).
fn mentions_key(raw: &str) -> bool {
    raw.match_indices(PROVIDER_KEY).any(|(at, _)| {
        let bare = |c: char| c.is_ascii_alphanumeric() || c == '_' || c == '-';
        let before = raw[..at].chars().next_back().is_none_or(|c| !bare(c));
        let after = raw[at + PROVIDER_KEY.len()..]
            .chars()
            .next()
            .is_none_or(|c| !bare(c));
        before && after
    })
}

/// Whether `config.toml` at `path` names a `model_provider` anywhere (the
/// top level, a profile table, a dotted key or an inline table, in any key
/// spelling TOML allows); `Some(false)` when the file is absent, `None`
/// when it does not read. Read through the key paths of [`super::toml`],
/// and any mention of the key's name counts too, so a form the line reader
/// passes over reads as a provider (unknown), never as none.
fn names_a_provider(path: &Path) -> Option<bool> {
    let raw = match crate::fsutil::read_settings_file(path, SETTINGS_FILE_MAX_BYTES) {
        Ok(Some(raw)) => raw,
        Ok(None) => return Some(false),
        Err(_) => return None,
    };
    let names = |segments: &[String]| segments.iter().any(|s| s == PROVIDER_KEY);
    Some(
        mentions_key(&raw)
            || raw.lines().any(|line| {
                super::toml::header_path(line).is_some_and(|p| names(&p))
                    || super::toml::key_value(line).is_some_and(|(p, _)| names(&p))
                    || super::toml::string_leaves(&[], line)
                        .iter()
                        .any(|(p, _)| names(p))
            }),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reading(home: &Path, vars: &[&str]) -> CostBasis {
        let vars: Vec<String> = vars.iter().map(|s| s.to_string()).collect();
        let present = |n: &str| vars.iter().any(|v| v == n);
        let facts = CostFacts {
            present: &present,
            setting: &|_| None,
            home: Some(home),
            workdir: None,
            hermetic: false,
            system_root: Path::new("/nonexistent"),
        };
        cost_basis(&facts).unwrap().basis
    }

    fn home_with(auth: Option<&str>, config: Option<&str>) -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join(".codex");
        std::fs::create_dir_all(&root).unwrap();
        if let Some(a) = auth {
            std::fs::write(root.join("auth.json"), a).unwrap();
        }
        if let Some(c) = config {
            std::fs::write(root.join("config.toml"), c).unwrap();
        }
        dir
    }

    #[test]
    fn an_account_login_is_a_subscription_and_an_api_key_login_is_billed() {
        let account = home_with(Some(r#"{"OPENAI_API_KEY":null,"tokens":{"x":1}}"#), None);
        assert_eq!(
            reading(account.path(), &[]),
            CostBasis::SubscriptionEquivalent
        );
        let key = home_with(Some(r#"{"OPENAI_API_KEY":"k","tokens":null}"#), None);
        assert_eq!(reading(key.path(), &[]), CostBasis::Billed);
    }

    #[test]
    fn a_key_variable_bills_and_an_endpoint_or_provider_is_unknown() {
        let account = home_with(Some(r#"{"tokens":{"x":1}}"#), None);
        assert_eq!(
            reading(account.path(), &["CODEX_API_KEY"]),
            CostBasis::Billed
        );
        assert_eq!(
            reading(account.path(), &["OPENAI_BASE_URL"]),
            CostBasis::Unknown
        );
        // Every TOML spelling of the key that names a provider: bare, quoted,
        // literal, escaped, dotted, under a table, inside an inline table.
        for config in [
            "[profiles.fast]\nmodel_provider = \"other\"\n",
            "\"model_provider\" = \"custom\"\n",
            "'model_provider' = \"custom\"\n",
            "\"model\\u005fprovider\" = \"custom\"\n",
            "profiles.fast.model_provider = \"custom\"\n",
            "[profiles.\"fast\"]\n  \"model_provider\"=\"custom\" # metered\n",
            "profiles = { fast = { \"model_provider\" = \"custom\" } }\n",
        ] {
            let provider = home_with(Some(r#"{"tokens":{"x":1}}"#), Some(config));
            assert_eq!(
                reading(provider.path(), &[]),
                CostBasis::Unknown,
                "{config}"
            );
        }
        // A provider table that is defined but not chosen changes nothing.
        let defined = home_with(
            Some(r#"{"tokens":{"x":1}}"#),
            Some("[model_providers.custom]\nname = \"c\"\n"),
        );
        assert_eq!(
            reading(defined.path(), &[]),
            CostBasis::SubscriptionEquivalent
        );
    }

    #[test]
    fn no_login_or_an_unfamiliar_file_is_unknown() {
        assert_eq!(
            reading(home_with(None, None).path(), &[]),
            CostBasis::Unknown
        );
        assert_eq!(
            reading(home_with(Some("[1]"), None).path(), &[]),
            CostBasis::Unknown
        );
        assert_eq!(
            reading(home_with(Some(r#"{"last_refresh":"t"}"#), None).path(), &[]),
            CostBasis::Unknown
        );
    }
}
