//! dsh (DeepSeek Harness, npm `@deepseek-ai/dsh`). An aggregator: the
//! official DeepSeek API first, plus every provider of its `llm-pi-ai`
//! adapter the person configured. Facts below were verified against dsh
//! 0.2.2-rc.1 (its source and a few live runs, 2026-11-09):
//!
//! - `dsh --profile headless` answers one task and exits: `-` reads the task
//!   from stdin, `--json` prints the run as JSON lines ([`output`]),
//!   `--session-id <id>` adopts a persisted session (an unknown one, or one
//!   recorded in another working directory, is an error), `--permission-mode
//!   <preset>` and `--approval-policy ask|never` set what the agent may do.
//!   The launcher reads its own flags (`--profile`, `--patch`) up to the
//!   first one it does not know and hands the rest to the app; `dsh
//!   headless` abbreviates `--profile headless` only as the first argument,
//!   so apb spells the flag and can put `--patch` in front.
//! - There is no model flag: the model is a row of the composed profile, and
//!   apb passes a run-scoped `--patch` file with that row in front of the
//!   form ([`model`], [`super::HostSetup::scoped_args`]).
//! - Presets: `read-only`, `workspace-write` (both ask for approval, which
//!   fails closed headless) and `danger-full-access` (no confinement, never
//!   asks). The base form runs `read-only` with `--approval-policy never`;
//!   autonomy appends `--permission-mode danger-full-access`, the parser
//!   keeping the last value.
//! - There is no system-prompt flag: the SOUL is a prompt prefix.
//! - dsh cannot be handed the id of a new session; its first event names it
//!   before any model call, so even a killed attempt's session is known.
//! - Skills: dsh reads the project's `.agents/skills` and `~/.agents/skills`,
//!   apb's own canon, so skills reach it by name.
//!
//! Not supported, and why: the live ask transport (it injects claude's
//! `--mcp-config`; dsh takes MCP servers only as profile rows), the tool
//! exposure check (the stream names no tools at start), an end-of-turn
//! hook for `apb setup` (dsh's hook rows are experimental), the hermetic
//! minimal environment and desktop history (nothing to point dsh at), a
//! sandboxed run (never verified: `isolation: full` refuses dsh), and a
//! reported cost (dsh prints none; apb prices a step on a cloud catalog
//! model from the run's catalog row, `crate::cloud_models`).

pub mod actions;
pub mod mcp;
pub mod model;
pub mod output;
mod providers;

use std::path::{Path, PathBuf};

use crate::config::{Interaction, InvocationDef, PromptVia, SoulDelivery, Transport};

use super::{
    Agent, AgentCategory, AgentEnvSet, AuthHint, AuthKind, Capabilities, ClosedList, Descriptor,
    Forms, FreshForm, HostNotes, HostSetup, ModelPolicy, ScopedEnvError, Transcript, Verified,
};

/// The agent id.
pub const ID: &str = "dsh";

static DESCRIPTOR: Descriptor = Descriptor {
    id: ID,
    aliases: &[],
    bins: &["dsh"],
    version_args: &["--version"],
    help_args: &["--profile", "headless", "--help"],
    home_paths: &[],
    category: AgentCategory::Aggregator,
    capabilities: Capabilities {
        transcript: Transcript::DshEvents,
        printed_session: true,
        ..Capabilities::NONE
    },
    verified: Some(Verified {
        version: "0.2.2-rc.1",
        on: "2026-11-09",
    }),
};

/// The base flags every form carries: the headless app, its event stream,
/// and no write or approval unless autonomy is granted.
const BASE: [&str; 7] = [
    "--profile",
    "headless",
    "--json",
    "--permission-mode",
    "read-only",
    "--approval-policy",
    "never",
];

/// The dsh agent.
pub struct Dsh;

impl Agent for Dsh {
    fn descriptor(&self) -> &'static Descriptor {
        &DESCRIPTOR
    }

    fn mcp_host(&self) -> Option<&'static dyn super::McpHost> {
        Some(&mcp::DshMcp)
    }

    fn models(&self) -> &'static dyn ModelPolicy {
        &DshModels
    }

    fn setup(&self) -> &'static dyn HostSetup {
        &DshSetup
    }

    fn output(&self) -> &'static dyn super::OutputFormat {
        &output::DshOutput
    }

    // The task on stdin (`-`): no argument length limit, and a prompt is
    // never read as an option. A resume carries its prompt after `--`.
    fn forms(&self) -> Option<Forms> {
        let mut argv: Vec<String> = BASE.iter().map(|s| s.to_string()).collect();
        argv.push("-".to_string());
        Some(Forms {
            launch: InvocationDef {
                argv,
                prompt_via: PromptVia::Stdin,
                soul: SoulDelivery::Prefix,
                soul_flag: None,
                transport: Transport::Headless,
                autonomous_args: vec!["--permission-mode".into(), "danger-full-access".into()],
                interaction: Interaction::Resume,
            },
            resume: Some(&[
                "--profile",
                "headless",
                "--json",
                "--permission-mode",
                "read-only",
                "--approval-policy",
                "never",
                "--session-id",
                "{session}",
                "--",
                "{prompt}",
            ]),
            fresh: FreshForm::Printed,
            profile_mode: None,
        })
    }
}

/// dsh's model rules: the official DeepSeek models as the closed list, any
/// other route the person configured, one billing account per route.
struct DshModels;

impl ModelPolicy for DshModels {
    fn closed_list(&self) -> ClosedList {
        ClosedList::Fixed(model::DEEPSEEK_MODELS)
    }

    fn list_id(&self, m: &str, _account_family: &str) -> Result<String, String> {
        Ok(model::parse(m)?.map(|s| s.list_id()).unwrap_or_default())
    }

    // A model on another route is one of the person's own provider
    // profiles, which apb cannot list.
    fn on_closed_list(&self, id: &str, list: &[String]) -> bool {
        list.iter().any(|m| m == id) || id.contains('/')
    }

    fn newer_in_family(&self, id: &str) -> bool {
        id.starts_with("deepseek-") && !id.contains('/')
    }

    fn canonical_model(&self, m: &str) -> Result<String, String> {
        Ok(model::parse(m)?.map(|s| s.canonical()).unwrap_or_default())
    }

    fn billing_account(&self, m: &str) -> Option<String> {
        model::parse(m).ok().flatten().map(|s| s.account_key())
    }

    fn cloud_routes(&self) -> &'static [crate::cloud_models::CloudRoute] {
        model::CLOUD_ROUTES
    }
}

/// dsh's home: `$DSH_HOME`, else `.dsh` under `home`.
fn home_of(home: &Path) -> PathBuf {
    std::env::var_os("DSH_HOME")
        .filter(|d| !d.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| home.join(".dsh"))
}

/// The patch layers apb reads names from: the home layer and the headless
/// profile's own layer.
fn layers(dsh_home: &Path) -> [PathBuf; 2] {
    [
        dsh_home.join("cordis.patch.yml"),
        dsh_home.join("profiles/headless/cordis.patch.yml"),
    ]
}

fn parsed_layers(dsh_home: &Path) -> Vec<yaml_serde::Value> {
    layers(dsh_home)
        .iter()
        .filter_map(|p| std::fs::read_to_string(p).ok())
        .filter_map(|t| yaml_serde::from_str(&t).ok())
        .collect()
}

/// Every string under a key named `key`, anywhere in `v`.
fn strings_under(v: &yaml_serde::Value, key: &str, out: &mut Vec<String>) {
    match v {
        yaml_serde::Value::Mapping(m) => {
            for (k, val) in m {
                if k.as_str() == Some(key)
                    && let Some(s) = val.as_str()
                {
                    out.push(s.to_string());
                }
                strings_under(val, key, out);
            }
        }
        yaml_serde::Value::Sequence(items) => {
            items.iter().for_each(|i| strings_under(i, key, out));
        }
        yaml_serde::Value::Tagged(t) => strings_under(&t.value, key, out),
        _ => {}
    }
}

/// The provider routes the person configured for `llm-pi-ai` (the keys of
/// its `providers` mapping), names only.
fn routes(dsh_home: &Path) -> Vec<String> {
    let mut out = vec![model::DEFAULT_ROUTE.to_string()];
    for doc in parsed_layers(dsh_home) {
        for row in doc.as_sequence().into_iter().flatten() {
            let inserted = row.get("insert").and_then(|i| i.as_sequence());
            for r in inserted.map_or_else(|| vec![row], |i| i.iter().collect()) {
                let providers = r
                    .get("config")
                    .and_then(|c| c.get("providers"))
                    .and_then(|p| p.as_mapping());
                for key in providers.into_iter().flat_map(|m| m.keys()) {
                    if let Some(k) = key.as_str()
                        && !out.iter().any(|o| o == k)
                    {
                        out.push(k.to_string());
                    }
                }
            }
        }
    }
    out
}

fn env_name(s: &str) -> bool {
    !s.is_empty()
        && s.chars()
            .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_')
}

/// dsh's host facts and needs.
struct DshSetup;

impl HostSetup for DshSetup {
    // The credentials store's presence, or a DeepSeek key in the
    // environment; values are never read.
    fn auth_hint(&self, home: &Path) -> (Option<AuthHint>, Option<Vec<String>>) {
        let stored = home_of(home).join(".credentials.yaml").is_file();
        let kind = if stored || std::env::var_os("DEEPSEEK_API_KEY").is_some_and(|v| !v.is_empty())
        {
            AuthKind::ApiKey
        } else {
            AuthKind::None
        };
        (Some(AuthHint { kind }), None)
    }

    fn host_notes(&self, home: Option<&Path>, _installed: bool) -> HostNotes {
        HostNotes {
            providers: home.map(|h| routes(&home_of(h))),
            notes: Vec::new(),
        }
    }

    fn memo_files(&self, home: &Path) -> Vec<PathBuf> {
        let dsh = home_of(home);
        let mut out = vec![dsh.join(".credentials.yaml")];
        out.extend(layers(&dsh));
        out
    }

    fn memo_env(&self) -> Option<(&'static str, &'static str)> {
        Some(("DEEPSEEK_API_KEY", "deepseek"))
    }

    fn login_warning(&self, auth: Option<&AuthHint>) -> Option<&'static str> {
        auth.is_some_and(|a| a.kind == AuthKind::None).then_some(
            "no DeepSeek key found: set DEEPSEEK_API_KEY or store a key with dsh's Models page",
        )
    }

    // dsh reads its home, its own switches and the official route's key.
    fn env_set(&self) -> AgentEnvSet {
        AgentEnvSet {
            always: &["DSH_*", "DEEPSEEK_*"],
            when: &[],
        }
    }

    // The variables a route of dsh's `llm-pi-ai` provider catalog reads by
    // default, when the route is named after its provider.
    fn model_env(&self, m: &str) -> &'static [&'static str] {
        model::parse(m).ok().flatten().map_or(&[], |s| {
            let cloud = model::cloud_key_env(&s.route);
            if cloud.is_empty() {
                providers::provider_env(&s.route)
            } else {
                cloud
            }
        })
    }

    // Every `apiKeyEnv` the person's patch layers name (names only).
    fn derived_env(&self) -> Vec<String> {
        let Some(home) = std::env::var_os("HOME").map(PathBuf::from) else {
            return Vec::new();
        };
        let mut out = Vec::new();
        for doc in parsed_layers(&home_of(&home)) {
            strings_under(&doc, "apiKeyEnv", &mut out);
        }
        out.retain(|n| env_name(n));
        out.sort();
        out.dedup();
        out
    }

    fn scoped_args(
        &self,
        m: &str,
        scoped_dir: Option<&Path>,
        cloud: Option<&crate::cloud_models::CloudSnapshotRef>,
    ) -> Result<Vec<String>, ScopedEnvError> {
        model::patch_args(m, scoped_dir, cloud).map_err(|source| ScopedEnvError {
            what: format!("{ID} model patch"),
            source,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_forms_keep_the_prompt_off_the_options() {
        let forms = Dsh.forms().unwrap();
        assert_eq!(forms.launch.prompt_via, PromptVia::Stdin);
        assert_eq!(forms.launch.argv.last().map(String::as_str), Some("-"));
        forms.launch.validate().unwrap();
        let resume = forms.resume.unwrap();
        assert_eq!(&resume[resume.len() - 2..], ["--", "{prompt}"]);
        assert!(Dsh.continues_sessions());
    }

    #[test]
    fn the_model_rules_list_deepseek_and_accept_other_routes() {
        let m = &DshModels;
        let list: Vec<String> = model::DEEPSEEK_MODELS
            .iter()
            .map(|s| s.to_string())
            .collect();
        assert_eq!(
            m.list_id("deepseek-v4-pro@high", "").unwrap(),
            "deepseek-v4-pro"
        );
        assert!(m.on_closed_list("deepseek-flash", &list));
        assert!(m.on_closed_list("zai/glm-5.3", &list));
        let near = m.cloud_model("nearai/Qwen/Qwen3.8-27B@high").unwrap();
        assert_eq!(
            (near.provider, near.id.as_str(), near.effort.as_deref()),
            ("near-ai-cloud", "Qwen/Qwen3.8-27B", Some("high"))
        );
        assert!(m.cloud_model("deepseek-flash").is_none());
        assert!(m.cloud_model("zai/glm-5.3").is_none());
        assert!(!m.on_closed_list("gpt-6-sol", &list));
        assert!(m.newer_in_family("deepseek-v5"));
        assert_eq!(
            m.canonical_model("deepseek-official/deepseek-flash")
                .unwrap(),
            "deepseek-flash"
        );
        assert!(m.canonical_model("bad model").is_err());
        assert_eq!(m.billing_account("zai/glm-5.3").as_deref(), Some("dsh:zai"));
        assert_ne!(
            m.billing_account("deepseek-flash"),
            m.billing_account("zai/glm-5.3")
        );
    }

    /// Every provider of dsh 0.2.2-rc.1's `llm-pi-ai` catalog keeps the
    /// variables it reads when the route is named after it.
    #[test]
    fn a_route_named_after_a_provider_keeps_that_providers_variables() {
        let s = &DshSetup;
        assert_eq!(s.model_env("cerebras/llama-4"), ["CEREBRAS_API_KEY"]);
        assert!(
            s.model_env("anthropic/claude")
                .contains(&"ANTHROPIC_API_KEY")
        );
        // The whole temporary-credential set an STS login needs.
        for name in [
            "AWS_ACCESS_KEY_ID",
            "AWS_SECRET_ACCESS_KEY",
            "AWS_SESSION_TOKEN",
            "AWS_REGION",
            "AWS_PROFILE",
        ] {
            assert!(s.model_env("amazon-bedrock/m").contains(&name), "{name}");
        }
        assert!(s.model_env("azure/m").contains(&"AZURE_OPENAI_BASE_URL"));
        assert_eq!(
            s.model_env("qwen-token-plan-cn/m"),
            ["QWEN_TOKEN_PLAN_CN_API_KEY"]
        );
        assert!(s.model_env("deepseek-flash").is_empty());
        assert_eq!(
            s.model_env("nearai/Qwen/Qwen3.8-27B@high"),
            ["NEARAI_API_KEY"]
        );
        assert!(s.model_env("my-own-route/m").is_empty());
    }

    #[test]
    fn routes_and_key_names_come_from_the_patch_layers_by_name_only() {
        let dir = tempfile::tempdir().unwrap();
        let profile = dir.path().join("profiles/headless");
        std::fs::create_dir_all(&profile).unwrap();
        std::fs::write(
            profile.join("cordis.patch.yml"),
            "- id: llm-pi-ai\n  config:\n    providers:\n      zai:\n        apiKeyEnv: ZAI_KEY\n      openrouter:\n        apiKeyEnv: \"not a name\"\n- id: x\n  disabled: !!js true\n",
        )
        .unwrap();
        assert_eq!(
            routes(dir.path()),
            ["deepseek-official", "zai", "openrouter"]
        );
        let mut names = Vec::new();
        for doc in parsed_layers(dir.path()) {
            strings_under(&doc, "apiKeyEnv", &mut names);
        }
        names.retain(|n| env_name(n));
        assert_eq!(names, ["ZAI_KEY"]);
    }
}
