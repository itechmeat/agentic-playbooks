//! The built-in variable sets an agent process keeps under `allowlist`.
//!
//! Each agent's set was checked against the variable names its installed CLI
//! reads (claude 2.1.311, codex 0.162.1, opencode and its provider catalog,
//! qodercli 1.1.76, grok, agy, the zcode CLI bundle and the hermes source).
//! Entries marked `unverified` come from the vendor's documentation only.

/// What every agent process keeps: the login basics, the locale (the POSIX
/// category names one by one, never an `LC_` prefix that would also pass an
/// `LC_DEPLOY_TOKEN`), temp dirs, XDG
/// dirs, proxies, certificate bundles and `APB_CONFIG_DIR` (a nested
/// `apb connector call` or `apb __ask-server` must resolve the same config,
/// trust store and global secrets as the run).
pub fn base_set() -> &'static [&'static str] {
    &[
        "PATH",
        "HOME",
        "USER",
        "LOGNAME",
        "SHELL",
        "LANG",
        "LANGUAGE",
        "LC_ALL",
        "LC_CTYPE",
        "LC_COLLATE",
        "LC_MESSAGES",
        "LC_MONETARY",
        "LC_NUMERIC",
        "LC_TIME",
        "LC_ADDRESS",
        "LC_IDENTIFICATION",
        "LC_MEASUREMENT",
        "LC_NAME",
        "LC_PAPER",
        "LC_TELEPHONE",
        "TZ",
        "TERM",
        "TMPDIR",
        "TMP",
        "TEMP",
        "XDG_CONFIG_HOME",
        "XDG_DATA_HOME",
        "XDG_STATE_HOME",
        "XDG_CACHE_HOME",
        "XDG_RUNTIME_DIR",
        "HTTP_PROXY",
        "HTTPS_PROXY",
        "NO_PROXY",
        "ALL_PROXY",
        "http_proxy",
        "https_proxy",
        "no_proxy",
        "all_proxy",
        "SSL_CERT_FILE",
        "SSL_CERT_DIR",
        "NODE_EXTRA_CA_CERTS",
        "REQUESTS_CA_BUNDLE",
        "CURL_CA_BUNDLE",
        "APB_CONFIG_DIR",
        "SYSTEMROOT",
        "USERPROFILE",
        "APPDATA",
        "LOCALAPPDATA",
        "PATHEXT",
        "COMSPEC",
    ]
}

/// The variables one agent CLI reads: `always` kept, and each `when` group
/// kept only while its trigger variable is switched on (a cloud provider's
/// credentials only when the CLI is switched to that provider; see
/// [`super::is_truthy`]).
#[derive(Debug, Clone, Copy)]
pub struct AgentEnvSet {
    pub always: &'static [&'static str],
    pub when: &'static [(&'static str, &'static [&'static str])],
}

const NONE: AgentEnvSet = AgentEnvSet {
    always: &[],
    when: &[],
};

/// The set of the canonical agent id `agent` (the caller resolves an alias
/// such as `claude-code` first, see `detect::canonical_agent_id`). A
/// custom agent from the config's `agents:` gets nothing beyond the base
/// set and the profile's `allow`.
pub fn agent_set(agent: &str) -> AgentEnvSet {
    match agent {
        "claude" => AgentEnvSet {
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
        },
        // Plus every `env_key` of the `[model_providers.*]` tables in the
        // operator's codex config, derived by the engine.
        "codex" => AgentEnvSet {
            always: &["CODEX_*", "OPENAI_*"],
            when: &[],
        },
        // Plus the provider key of the model, see `opencode_provider_keys`.
        "opencode" => AgentEnvSet {
            always: &["OPENCODE_*"],
            when: &[],
        },
        // apb sets the two provider-config variables itself after the plan.
        "zcode" => AgentEnvSet {
            always: &["ZCODE_*"],
            when: &[],
        },
        // hermes reads its keys from `~/.hermes/.env`, found through HOME.
        "hermes" => AgentEnvSet {
            always: &["HERMES_*"],
            when: &[],
        },
        "grok" => AgentEnvSet {
            always: &["GROK_*", "XAI_API_KEY"],
            when: &[],
        },
        // unverified: no cursor-agent was installed where the sets were
        // checked; the prefix follows the vendor's documentation.
        "cursor" => AgentEnvSet {
            always: &["CURSOR_*"],
            when: &[],
        },
        "qoder" => AgentEnvSet {
            always: &["QODER_*", "QODERCLI_*"],
            when: &[],
        },
        // CLOUDSDK_CONFIG is not read by agy itself (unverified there); it
        // locates the gcloud credentials its Google login can rely on.
        "agy" => AgentEnvSet {
            always: &[
                "AGY_*",
                "ANTIGRAVITY_*",
                "GEMINI_API_KEY",
                "GOOGLE_API_KEY",
                "GOOGLE_CLOUD_PROJECT",
                "GOOGLE_APPLICATION_CREDENTIALS",
                "CLOUDSDK_CONFIG",
            ],
            when: &[],
        },
        _ => NONE,
    }
}

/// The variables opencode reads for the provider of `model`
/// (`provider/model`), from opencode's own provider catalog. Empty for a
/// provider not listed here: the profile's `allow` names its key then.
pub fn opencode_provider_keys(model: &str) -> &'static [&'static str] {
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
