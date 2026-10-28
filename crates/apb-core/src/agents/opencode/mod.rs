//! opencode. An aggregator: it runs models of many providers, listed by
//! `opencode models`.

pub mod output;

use std::path::{Path, PathBuf};

use crate::config::{Interaction, SoulDelivery};

use super::forms::headless;
use super::{
    Agent, AgentCategory, AgentEnvSet, AuthHint, Authority, Capabilities, Descriptor, Forms,
    FreshForm, HostSetup, SessionListing, Verified,
};

/// The agent id.
pub const ID: &str = "opencode";

static DESCRIPTOR: Descriptor = Descriptor {
    id: ID,
    aliases: &[],
    bins: &["opencode"],
    version_args: &["--version"],
    help_args: &["run", "--help"],
    home_paths: &[],
    category: AgentCategory::Aggregator,
    capabilities: Capabilities::NONE,
    verified: Some(Verified {
        version: "1.18.32",
        on: "2026-09-27",
    }),
};

/// The opencode agent.
pub struct Opencode;

impl Agent for Opencode {
    fn descriptor(&self) -> &'static Descriptor {
        &DESCRIPTOR
    }

    fn setup(&self) -> &'static dyn super::HostSetup {
        &OpencodeSetup
    }

    fn output(&self) -> &'static dyn super::OutputFormat {
        &output::OpencodeOutput
    }

    // Verified against the local `opencode run --help`: `--auto`
    // auto-approves permissions that are not explicitly denied.
    fn forms(&self) -> Option<Forms> {
        Some(Forms {
            launch: headless(
                &["run", "--format", "json", "-m", "{model}", "--", "{prompt}"],
                SoulDelivery::Prefix,
                None,
                &["--auto"],
                Interaction::Resume,
            ),
            // opencode re-enters a session via `--session <id>`.
            resume: Some(&[
                "run",
                "--session",
                "{session}",
                "--format",
                "json",
                "-m",
                "{model}",
                "--",
                "{prompt}",
            ]),
            // Verified against `opencode run --help` (1.18): `--title` names
            // the session; `opencode session list --format json` lists `id`
            // and `title`, scoped to the project of the working directory.
            fresh: FreshForm::Titled {
                flag: "--title",
                listing: SessionListing {
                    args: &["session", "list", "--format", "json"],
                    find: titled_session,
                },
            },
            profile_mode: None,
        })
    }
}

/// The id of the session titled `title` in the stdout of `session list
/// --format json`; `None` when it lists no such session (an attempt killed
/// before its first assistant message never persists one).
fn titled_session(stdout: &str, title: &str) -> Option<String> {
    let listed: Vec<serde_json::Value> = serde_json::from_str(stdout.trim()).ok()?;
    listed.into_iter().find_map(|s| {
        (s.get("title")?.as_str()? == title).then(|| s.get("id")?.as_str().map(str::to_string))?
    })
}

/// opencode's host facts and needs.
struct OpencodeSetup;

impl HostSetup for OpencodeSetup {
    fn models_command(&self) -> Option<(&'static [&'static str], Authority)> {
        Some((&["models"], Authority::Full))
    }

    // Only provider names (the top-level keys of auth.json), no values. The
    // first existing file wins.
    fn auth_hint(&self, home: &Path) -> (Option<AuthHint>, Option<Vec<String>>) {
        let providers = auth_paths(home)
            .into_iter()
            .find_map(|path| std::fs::read_to_string(&path).ok())
            .and_then(|raw| serde_json::from_str::<serde_json::Value>(&raw).ok())
            .and_then(|v| v.as_object().map(|o| o.keys().cloned().collect::<Vec<_>>()));
        (None, providers)
    }

    // `opencode models` reads its config and its cached model catalog: a
    // change to either can change the listed models.
    fn memo_files(&self, home: &Path) -> Vec<PathBuf> {
        let mut out = auth_paths(home);
        for rel in [
            ".config/opencode/opencode.json",
            ".config/opencode/opencode.jsonc",
            ".cache/opencode/models.json",
        ] {
            out.push(home.join(rel));
        }
        out
    }

    fn env_set(&self) -> AgentEnvSet {
        AgentEnvSet {
            always: &["OPENCODE_*"],
            when: &[],
        }
    }

    fn model_env(&self, model: &str) -> &'static [&'static str] {
        provider_keys(model)
    }
}

/// Where opencode keeps its credentials: the XDG data dir (current opencode)
/// first, then the legacy config-dir location.
fn auth_paths(home: &Path) -> Vec<PathBuf> {
    vec![
        home.join(".local/share/opencode/auth.json"),
        home.join(".config/opencode/auth.json"),
    ]
}

/// The variables opencode reads for the provider of `model`
/// (`provider/model`), from opencode's own provider catalog. Empty for a
/// provider not listed here: the profile's `allow` names its key then.
pub fn provider_keys(model: &str) -> &'static [&'static str] {
    let provider = model.split_once('/').map_or("", |(p, _)| p);
    match provider {
        "anthropic" => &["ANTHROPIC_API_KEY"],
        "openai" => &["OPENAI_API_KEY"],
        "openrouter" => &["OPENROUTER_API_KEY"],
        "zai" | "zai-coding-plan" | "zhipuai" => &["ZHIPU_API_KEY"],
        "google" => &[
            "GOOGLE_API_KEY",
            "GOOGLE_GENERATIVE_AI_API_KEY",
            "GEMINI_API_KEY",
        ],
        "google-vertex" => &[
            "GOOGLE_VERTEX_PROJECT",
            "GOOGLE_VERTEX_LOCATION",
            "GOOGLE_APPLICATION_CREDENTIALS",
        ],
        "amazon-bedrock" => &["AWS_*"],
        "xai" => &["XAI_API_KEY"],
        "deepseek" => &["DEEPSEEK_API_KEY"],
        "groq" => &["GROQ_API_KEY"],
        "mistral" => &["MISTRAL_API_KEY"],
        "moonshotai" => &["MOONSHOT_API_KEY"],
        "opencode" => &["OPENCODE_API_KEY"],
        "github-copilot" => &["GITHUB_TOKEN"],
        _ => &[],
    }
}
