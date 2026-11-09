//! The variables each provider of dsh's `llm-pi-ai` adapter reads by
//! default (its key, and the base-URL variables of the providers that take
//! one), from dsh 0.2.2-rc.1's provider catalog
//! (`packages/llm/llm-pi-ai/src/provider-data.generated.ts`). A route the
//! person named after a provider reads these when its profile names no
//! `apiKeyEnv`; the official DeepSeek route's key is in dsh's own set.
//! `amazon-bedrock` also keeps `AWS_SESSION_TOKEN`, which dsh's catalog
//! leaves out but the AWS credential provider it uses reads for temporary
//! (STS) credentials.

/// The variables the provider `route` reads; empty for a route named
/// otherwise.
pub fn provider_env(route: &str) -> &'static [&'static str] {
    match route {
        "amazon-bedrock" => &[
            "AWS_BEARER_TOKEN_BEDROCK",
            "AWS_PROFILE",
            "AWS_ACCESS_KEY_ID",
            "AWS_SECRET_ACCESS_KEY",
            "AWS_SESSION_TOKEN",
            "AWS_CONTAINER_CREDENTIALS_RELATIVE_URI",
            "AWS_CONTAINER_CREDENTIALS_FULL_URI",
            "AWS_WEB_IDENTITY_TOKEN_FILE",
            "AWS_REGION",
            "AWS_DEFAULT_REGION",
        ],
        "ant-ling" => &["ANT_LING_API_KEY"],
        "anthropic" => &[
            "ANTHROPIC_AUTH_TOKEN",
            "ANTHROPIC_OAUTH_TOKEN",
            "ANTHROPIC_API_KEY",
            "ANTHROPIC_FEDERATION_RULE_ID",
            "ANTHROPIC_ORGANIZATION_ID",
            "ANTHROPIC_IDENTITY_TOKEN_FILE",
            "ANTHROPIC_SERVICE_ACCOUNT_ID",
            "ANTHROPIC_WORKSPACE_ID",
        ],
        "azure" => &[
            "AZURE_OPENAI_API_KEY",
            "AZURE_OPENAI_BASE_URL",
            "AZURE_OPENAI_RESOURCE_NAME",
        ],
        "baseten" => &["BASETEN_API_KEY"],
        "cerebras" => &["CEREBRAS_API_KEY"],
        "cloudflare-ai-gateway" => &[
            "CLOUDFLARE_API_KEY",
            "CLOUDFLARE_ACCOUNT_ID",
            "CLOUDFLARE_GATEWAY_ID",
        ],
        "cloudflare-workers-ai" => &["CLOUDFLARE_API_KEY", "CLOUDFLARE_ACCOUNT_ID"],
        "deepseek" => &["DEEPSEEK_API_KEY"],
        "fireworks" => &["FIREWORKS_API_KEY"],
        "github-copilot" => &["COPILOT_GITHUB_TOKEN"],
        "google" => &["GEMINI_API_KEY"],
        "google-vertex" => &[
            "GOOGLE_CLOUD_API_KEY",
            "GOOGLE_APPLICATION_CREDENTIALS",
            "GOOGLE_CLOUD_PROJECT",
            "GCLOUD_PROJECT",
            "GOOGLE_CLOUD_LOCATION",
        ],
        "groq" => &["GROQ_API_KEY"],
        "huggingface" => &["HF_TOKEN"],
        "kimi-coding" => &["KIMI_API_KEY"],
        "meta" => &["META_API_KEY"],
        "minimax" => &["MINIMAX_API_KEY"],
        "minimax-cn" => &["MINIMAX_CN_API_KEY"],
        "mistral" => &["MISTRAL_API_KEY"],
        "moonshotai" => &["MOONSHOT_API_KEY"],
        "moonshotai-cn" => &["MOONSHOT_API_KEY"],
        "nvidia" => &["NVIDIA_API_KEY"],
        "openai" => &["OPENAI_API_KEY"],
        "opencode" => &["OPENCODE_API_KEY"],
        "opencode-go" => &["OPENCODE_API_KEY"],
        "openrouter" => &["OPENROUTER_API_KEY"],
        "qwen-token-plan" => &["QWEN_TOKEN_PLAN_API_KEY"],
        "qwen-token-plan-cn" => &["QWEN_TOKEN_PLAN_CN_API_KEY"],
        "qwen-token-plan-individual" => &["QWEN_TOKEN_PLAN_API_KEY"],
        "radius" => &["RADIUS_API_KEY"],
        "together" => &["TOGETHER_API_KEY"],
        "typesafe" => &["TYPESAFE_API_KEY"],
        "vercel-ai-gateway" => &["AI_GATEWAY_API_KEY"],
        "xai" => &["XAI_API_KEY"],
        "xiaomi" => &["XIAOMI_API_KEY"],
        "xiaomi-token-plan-ams" => &["XIAOMI_TOKEN_PLAN_AMS_API_KEY"],
        "xiaomi-token-plan-cn" => &["XIAOMI_TOKEN_PLAN_CN_API_KEY"],
        "xiaomi-token-plan-sgp" => &["XIAOMI_TOKEN_PLAN_SGP_API_KEY"],
        "zai" => &["ZAI_API_KEY"],
        "zai-coding-cn" => &["ZAI_CODING_CN_API_KEY"],
        _ => &[],
    }
}
