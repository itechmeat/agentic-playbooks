//! codex (OpenAI Codex CLI). Vendor-tied to OpenAI; runs with a run-scoped
//! config home that carries no MCP servers.

pub mod actions;
pub mod home;
pub mod output;
mod toml;

use std::path::{Path, PathBuf};

use crate::config::{Interaction, SoulDelivery};

use super::forms::headless;
use super::{
    Agent, AgentCategory, AgentEnvSet, AuthHint, AuthKind, Capabilities, ClosedList, Descriptor,
    Forms, FreshForm, HostNotes, HostSetup, ModelPolicy, ScopedEnvError, Transcript, Verified,
};

/// The agent id.
pub const ID: &str = "codex";

static DESCRIPTOR: Descriptor = Descriptor {
    id: ID,
    aliases: &[],
    bins: &["codex"],
    version_args: &["--version"],
    help_args: &["exec", "--help"],
    home_paths: &[],
    category: AgentCategory::Vendor,
    capabilities: Capabilities {
        transcript: Transcript::JsonEvents,
        ..Capabilities::NONE
    },
    verified: Some(Verified {
        version: "0.157.0",
        on: "2026-09-27",
    }),
};

/// The codex agent.
pub struct Codex;

impl Agent for Codex {
    fn descriptor(&self) -> &'static Descriptor {
        &DESCRIPTOR
    }

    fn models(&self) -> &'static dyn super::ModelPolicy {
        &CodexModels
    }

    fn setup(&self) -> &'static dyn super::HostSetup {
        &CodexSetup
    }

    fn output(&self) -> &'static dyn super::OutputFormat {
        &output::CodexOutput
    }

    // Verified against the local `codex exec --help`:
    // `--dangerously-bypass-approvals-and-sandbox` skips all confirmation
    // prompts and runs without sandboxing, the one-shot equivalent of
    // claude's bypassPermissions.
    fn forms(&self) -> Option<Forms> {
        Some(Forms {
            launch: headless(
                &["exec", "--json", "-m", "{model}", "--", "{prompt}"],
                SoulDelivery::Prefix,
                None,
                &["--dangerously-bypass-approvals-and-sandbox"],
                Interaction::Resume,
            ),
            // codex re-enters a conversation via `exec resume <id>`.
            resume: Some(&[
                "exec",
                "resume",
                "{session}",
                "--json",
                "-m",
                "{model}",
                "--",
                "{prompt}",
            ]),
            fresh: FreshForm::Printed,
            profile_mode: None,
        })
    }
}

/// codex's host facts and needs.
struct CodexSetup;

impl HostSetup for CodexSetup {
    // Classified by the KEY NAMES in auth.json (not values): `tokens` or
    // `account_id` means an account login, `OPENAI_API_KEY` an api key.
    fn auth_hint(&self, home: &Path) -> (Option<AuthHint>, Option<Vec<String>>) {
        let kind = auth_kind(&codex_root(home));
        (Some(AuthHint { kind }), None)
    }

    // The `[model_providers.*]` section names of codex's `config.toml` are
    // the providers, readable without the binary on PATH. The model list
    // itself is apb's static one (`crate::agent_catalog`).
    fn host_notes(&self, home: Option<&Path>, _installed: bool) -> HostNotes {
        let providers = home.map(|h| providers(&codex_root(h))).unwrap_or_default();
        HostNotes {
            providers: (!providers.is_empty()).then_some(providers),
            notes: Vec::new(),
        }
    }

    fn memo_files(&self, home: &Path) -> Vec<PathBuf> {
        let root = codex_root(home);
        vec![root.join("auth.json"), root.join("config.toml")]
    }

    // Checked against the variable names codex 0.162.1 reads; every
    // `env_key` of the operator's custom providers comes on top
    // ([`HostSetup::derived_env`]).
    fn env_set(&self) -> AgentEnvSet {
        AgentEnvSet {
            always: &["CODEX_*", "OPENAI_*"],
            when: &[],
        }
    }

    fn derived_env(&self) -> Vec<String> {
        home::provider_env_keys(home::source_codex_home().as_deref())
    }

    fn scoped_env(
        &self,
        _model: &str,
        scoped_dir: Option<&Path>,
    ) -> Result<Vec<(String, PathBuf)>, ScopedEnvError> {
        home::scoped_env(scoped_dir).map_err(|source| ScopedEnvError {
            what: format!("isolated agent home for `{ID}`"),
            source,
        })
    }
}

/// The codex config root for the user whose home is `home`: `$CODEX_HOME`
/// when set and non-empty (what codex itself reads, see
/// [`home::source_codex_home`]), else `<home>/.codex`.
fn codex_root(home: &Path) -> PathBuf {
    codex_root_from(home, std::env::var(home::CODEX_HOME_ENV).ok().as_deref())
}

/// [`codex_root`] with the `$CODEX_HOME` value passed in.
fn codex_root_from(home: &Path, explicit: Option<&str>) -> PathBuf {
    match explicit {
        Some(e) if !e.is_empty() => PathBuf::from(e),
        _ => home.join(".codex"),
    }
}

/// The login kind of the codex config root `root`, classified by the KEY
/// NAMES in its `auth.json` (not values).
fn auth_kind(root: &Path) -> AuthKind {
    match std::fs::read_to_string(root.join("auth.json")) {
        Ok(raw) => match serde_json::from_str::<serde_json::Value>(&raw) {
            Ok(v) => {
                // A key set to `null` names no login.
                let has = |k: &str| v.get(k).is_some_and(|x| !x.is_null());
                if has("tokens") || has("account_id") {
                    AuthKind::Oauth
                } else {
                    // `OPENAI_API_KEY`, or a file whose shape is
                    // unfamiliar: conservatively an api key.
                    AuthKind::ApiKey
                }
            }
            Err(_) => AuthKind::ApiKey,
        },
        Err(_) => AuthKind::None,
    }
}

/// The `[model_providers.<name>]` table names of the `config.toml` under the
/// codex config root `root`, once each, read with the same header reader as
/// the provider keys.
fn providers(root: &Path) -> Vec<String> {
    let Ok(raw) = std::fs::read_to_string(root.join("config.toml")) else {
        return Vec::new();
    };
    let mut out: Vec<String> = Vec::new();
    for path in raw.lines().filter_map(toml::header_path) {
        if let [table, name] = path.as_slice()
            && table == "model_providers"
            && !out.contains(name)
        {
            out.push(name.clone());
        }
    }
    out
}

/// codex's model rules.
struct CodexModels;

impl ModelPolicy for CodexModels {
    fn vendor(&self) -> Option<&'static str> {
        Some("openai")
    }

    fn closed_list(&self) -> ClosedList {
        ClosedList::CodexTable
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The codex root of a fresh home holding `file`, as codex reads it
    /// without `$CODEX_HOME`.
    fn root_with(file: &str, body: &str) -> (tempfile::TempDir, PathBuf) {
        let home = tempfile::tempdir().unwrap();
        let root = codex_root_from(home.path(), None);
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join(file), body).unwrap();
        (home, root)
    }

    #[test]
    fn codex_home_wins_over_the_home_directory_when_set() {
        let home = Path::new("/h");
        assert_eq!(codex_root_from(home, None), home.join(".codex"));
        assert_eq!(codex_root_from(home, Some("")), home.join(".codex"));
        assert_eq!(codex_root_from(home, Some("/c")), Path::new("/c"));
    }

    #[test]
    fn a_null_login_key_is_no_account_login() {
        let kind = |body: &str| auth_kind(&root_with("auth.json", body).1);
        assert_eq!(kind(r#"{"tokens":{"a":1}}"#), AuthKind::Oauth);
        assert_eq!(
            kind(r#"{"OPENAI_API_KEY":"x","tokens":null,"account_id":null}"#),
            AuthKind::ApiKey
        );
    }

    /// Provider tables are read like the provider keys: a trailing comment
    /// and a quoted name are fine, a nested table is no provider of its own,
    /// and each name counts once.
    #[test]
    fn providers_are_the_model_provider_tables() {
        let (_home, root) = root_with(
            "config.toml",
            "[model_providers.a] # comment\n[model_providers.\"b\"]\n[model_providers.a.http_headers]\n[profiles.c]\n",
        );
        assert_eq!(providers(&root), ["a", "b"]);
    }
}
