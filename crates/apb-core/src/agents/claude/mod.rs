//! claude (Claude Code). Vendor-tied to Anthropic; the one agent with a live
//! ask-server, hermetic settings and a session record apb reads.

pub mod actions;
pub mod output;
pub mod settings;
pub mod state;

use std::path::{Path, PathBuf};

use crate::config::{Interaction, SoulDelivery};

use super::forms::headless;
use super::{Access, HomeBind, HomeCopy, SandboxNeeds};
use super::{
    Agent, AgentCategory, AgentEnvSet, AuthHint, AuthKind, Capabilities, ClosedList, Descriptor,
    Forms, FreshForm, HostSetup, ModelPolicy, Transcript, Verified,
};

/// The agent id.
pub const ID: &str = "claude";

static DESCRIPTOR: Descriptor = Descriptor {
    id: ID,
    // Profiles saved before `claude-code` was renamed `claude` still name it.
    aliases: &["claude-code"],
    bins: &["claude"],
    version_args: &["--version"],
    help_args: &["--help"],
    home_paths: &[],
    category: AgentCategory::Vendor,
    capabilities: Capabilities {
        hermetic_settings: true,
        live_ask_mcp: true,
        long_tool_wait: true,
        transcript: Transcript::SessionRecord,
        tool_exposure: true,
        desktop_history: false,
    },
    verified: Some(Verified {
        version: "2.1.283",
        on: "2026-09-27",
    }),
};

/// The claude agent.
pub struct Claude;

impl Agent for Claude {
    fn descriptor(&self) -> &'static Descriptor {
        &DESCRIPTOR
    }

    fn models(&self) -> &'static dyn super::ModelPolicy {
        &ClaudeModels
    }

    fn setup(&self) -> &'static dyn super::HostSetup {
        &ClaudeSetup
    }

    fn output(&self) -> &'static dyn super::OutputFormat {
        &output::ClaudeOutput
    }

    // claude runs headless one-shot (`-p`); to actually write files and
    // reach the network on an authorized effectful run it needs an explicit
    // non-interactive permission mode, otherwise every tool call blocks
    // waiting for an approval that never comes (spec 8.5). Its interaction
    // ceiling is `live` (the blocking `ask_user` MCP tool, spec 2026-07-20
    // Task 11).
    fn forms(&self) -> Option<Forms> {
        Some(Forms {
            launch: headless(
                &[
                    "-p",
                    "--output-format",
                    "json",
                    "--model",
                    "{model}",
                    "--",
                    "{prompt}",
                ],
                SoulDelivery::Native,
                Some("--append-system-prompt"),
                &["--permission-mode", "bypassPermissions"],
                Interaction::Live,
            ),
            // claude resumes a prior session with `--resume <id>` and takes
            // the follow-up as a fresh `-p` prompt.
            resume: Some(&[
                "--resume",
                "{session}",
                "-p",
                "--output-format",
                "json",
                "--model",
                "{model}",
                "--",
                "{prompt}",
            ]),
            // Verified against `claude --help`: `--session-id <uuid>` uses a
            // specific session id; `--resume <id>` continues it.
            fresh: FreshForm::Assigned {
                flag: "--session-id",
            },
            profile_mode: None,
        })
    }
}

/// claude's host facts and needs.
struct ClaudeSetup;

impl HostSetup for ClaudeSetup {
    // `~/.claude.json` is a sign of an oauth login, else an api key in the
    // environment.
    fn auth_hint(&self, home: &Path) -> (Option<AuthHint>, Option<Vec<String>>) {
        let kind = if home.join(".claude.json").is_file() {
            AuthKind::Oauth
        } else if std::env::var("ANTHROPIC_API_KEY").is_ok() {
            AuthKind::ApiKey
        } else {
            AuthKind::None
        };
        (Some(AuthHint { kind }), None)
    }

    fn memo_files(&self, home: &Path) -> Vec<PathBuf> {
        vec![home.join(".claude.json")]
    }

    fn memo_env(&self) -> Option<(&'static str, &'static str)> {
        Some(("ANTHROPIC_API_KEY", "anthropic"))
    }

    // The login is a key or token in the environment, else the oauth
    // credentials file, bound read-write so a token refresh written in
    // place reaches the operator's file. Checked with a stub agent
    // (apb-engine `sandbox_run_test`): a write in place reaches the file, a
    // rename onto it fails, since it is a single-file bind. Whether claude
    // 2.1.312 refreshes in place is not verified against the real CLI; if it
    // renames, the refresh fails inside the sandbox and the file keeps its
    // token, so an env token is the reliable sandbox login. `.claude.json`
    // is a minimal copy ([`state::minimal_state`]): the start flags and the
    // account, never the MCP servers, projects or caches; the agent's edits
    // stay in the sandbox.
    fn sandbox_needs(&self) -> Option<SandboxNeeds> {
        Some(SandboxNeeds {
            binds: vec![HomeBind {
                home_rel: ".claude/.credentials.json",
                access: Access::ReadWrite,
            }],
            copies: vec![HomeCopy {
                home_rel: ".claude.json",
                sanitize: state::minimal_state,
            }],
            env_logins: vec!["ANTHROPIC_API_KEY", "CLAUDE_CODE_OAUTH_TOKEN"],
        })
    }

    // A JSONL file named after the session under `projects/` of claude's
    // config directory (`CLAUDE_CONFIG_DIR`, else `~/.claude`), with every
    // tool call and result.
    fn session_record(&self, session: &str) -> Option<PathBuf> {
        let file = format!("{session}.jsonl");
        std::fs::read_dir(actions::projects_dir()?)
            .ok()?
            .filter_map(|e| e.ok())
            .map(|project| project.path().join(&file))
            .find(|candidate| candidate.is_file())
    }

    // Checked against the variable names claude 2.1.311 reads.
    fn env_set(&self) -> AgentEnvSet {
        AgentEnvSet {
            always: &[
                "ANTHROPIC_*",
                "CLAUDE_CODE_*",
                "CLAUDE_CONFIG_DIR",
                // Also what apb's supervisor spawn checks before setting its
                // own MCP tool timeout: the operator's value must survive.
                "MCP_TIMEOUT",
                "MCP_TOOL_TIMEOUT",
                "DISABLE_TELEMETRY",
                "DISABLE_AUTOUPDATER",
                "MAX_THINKING_TOKENS",
            ],
            when: &[
                ("CLAUDE_CODE_USE_BEDROCK", &["AWS_*"]),
                (
                    "CLAUDE_CODE_USE_VERTEX",
                    &[
                        "GOOGLE_APPLICATION_CREDENTIALS",
                        "GOOGLE_CLOUD_PROJECT",
                        "GCLOUD_PROJECT",
                        "CLOUD_ML_REGION",
                        "VERTEX_REGION_*",
                        "CLOUDSDK_CONFIG",
                    ],
                ),
                ("CLAUDE_CODE_USE_FOUNDRY", &["AZURE_*"]),
            ],
        }
    }
}

/// claude's model rules.
struct ClaudeModels;

impl ModelPolicy for ClaudeModels {
    fn vendor(&self) -> Option<&'static str> {
        Some("anthropic")
    }

    fn closed_list(&self) -> ClosedList {
        ClosedList::ClaudeTable
    }

    // A listed id, one of the CLI's own aliases, or either with the CLI's
    // long-context suffix.
    fn on_closed_list(&self, id: &str, list: &[String]) -> bool {
        let base = id.strip_suffix(settings::LONG_CONTEXT_SUFFIX).unwrap_or(id);
        list.iter().any(|m| m == id || m == base) || settings::ALIASES.contains(&base)
    }

    fn newer_in_family(&self, id: &str) -> bool {
        settings::is_family_id(id.strip_suffix(settings::LONG_CONTEXT_SUFFIX).unwrap_or(id))
    }
}
