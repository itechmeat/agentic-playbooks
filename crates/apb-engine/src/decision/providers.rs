//! Builds a provider chain from the snapshotted settings: one adapter per
//! configured `kind`, keys resolved now. Shared by the run's decision runner
//! and the standalone decider the MCP catalog ranking uses.

use std::time::Duration;

use apb_core::decisions::{
    EffectiveDecisions, EmulationOutput, KeyRef, ProviderKind, ProviderSpec,
};
use apb_decide::{
    ApiKey, Cloudflare, DecisionProvider, FakeProvider, LlmEmulation, OpenRouterDecisions,
    ProviderChain, StructuredOutput, SystemOne, VercelEvaluate,
};

/// The run's provider chains, built once.
#[derive(Debug)]
pub(crate) struct Chains {
    /// Every configured provider, in order.
    pub(crate) all: ProviderChain,
    /// The decision models proper: every kind but `llm_emulation`.
    pub(crate) native: ProviderChain,
    /// The `llm_emulation` providers.
    pub(crate) emulation: ProviderChain,
    /// The resolved keys (secrets for the redactor).
    pub(crate) keys: Vec<String>,
}

/// The chain and the resolved keys (which the redactor treats as secrets).
/// A provider whose key or account id does not resolve is left out, said
/// once on stderr and never with a value.
pub(crate) fn build_chain(settings: &EffectiveDecisions) -> (ProviderChain, Vec<String>) {
    let c = build_chains(settings);
    (c.all, c.keys)
}

/// [`build_chain`] split by route: every provider, the native decision
/// models and the `llm_emulation` providers.
pub(crate) fn build_chains(settings: &EffectiveDecisions) -> Chains {
    let timeout = Duration::from_millis(settings.timeout_ms);
    let (mut all, mut native, mut emulation) = (Vec::new(), Vec::new(), Vec::new());
    let mut keys = Vec::new();
    for spec in &settings.providers {
        let key = match resolve_key(spec) {
            Ok(k) => k,
            Err(why) => {
                eprintln!("apb: decision provider `{}` left out: {why}", spec.id);
                continue;
            }
        };
        let account = match spec.kind {
            ProviderKind::Cloudflare => match resolve_account_id(spec) {
                Some(a) => Some(a),
                None => {
                    eprintln!(
                        "apb: decision provider `{}` left out: its account id does not resolve",
                        spec.id
                    );
                    continue;
                }
            },
            _ => None,
        };
        if let Some(k) = &key {
            keys.push(k.clone());
        }
        let build = || provider_for(spec, key.clone(), account.clone(), timeout);
        all.push(build());
        match spec.kind {
            ProviderKind::LlmEmulation => emulation.push(build()),
            _ => native.push(build()),
        }
    }
    Chains {
        all: ProviderChain::new(all),
        native: ProviderChain::new(native),
        emulation: ProviderChain::new(emulation),
        keys,
    }
}

/// One adapter for one configured provider (`account` is the resolved
/// Cloudflare account id, `None` for the other kinds).
fn provider_for(
    spec: &ProviderSpec,
    key: Option<String>,
    account: Option<String>,
    timeout: Duration,
) -> Box<dyn DecisionProvider> {
    let key = key.map(ApiKey::new);
    let base_url = spec.base_url.clone().unwrap_or_default();
    let model = spec.model.clone().unwrap_or_default();
    let id = spec.id.clone();
    match spec.kind {
        ProviderKind::Systemone => Box::new(SystemOne::new(id, base_url, model, key, timeout)),
        ProviderKind::Fake => {
            let mut fake = FakeProvider::new(id);
            for (qid, item) in &spec.answers {
                fake = fake.answer(qid.clone(), item.clone());
            }
            Box::new(fake)
        }
        ProviderKind::VercelEvaluate => Box::new(
            VercelEvaluate::new(id, base_url, model, key, timeout)
                .with_zero_data_retention(spec.zero_data_retention),
        ),
        ProviderKind::OpenrouterDecisions => {
            Box::new(OpenRouterDecisions::new(id, base_url, model, key, timeout))
        }
        ProviderKind::Cloudflare => Box::new(Cloudflare::new(
            id,
            base_url,
            account.unwrap_or_default(),
            model,
            key,
            timeout,
        )),
        ProviderKind::LlmEmulation => Box::new(LlmEmulation::new(
            id,
            base_url,
            model,
            key,
            timeout,
            match spec.structured_output.unwrap_or_default() {
                EmulationOutput::JsonSchema => StructuredOutput::JsonSchema,
                EmulationOutput::PromptOnly => StructuredOutput::PromptOnly,
            },
        )),
    }
}

pub(crate) fn resolve_key(spec: &ProviderSpec) -> Result<Option<String>, String> {
    match &spec.key {
        None => Ok(None),
        Some(KeyRef::Env(var)) => apb_core::decisions::resolve_key_var(var)
            .map(Some)
            .ok_or_else(|| format!("variable `{var}` is not set")),
        Some(KeyRef::Cmd(cmd)) => apb_core::connector::secrets::resolve_cmd(
            cmd,
            apb_core::connector::secrets::CMD_SECRET_TIMEOUT,
        )
        .map(|k| Some(k.trim().to_string()))
        .map_err(|_| "its key command failed".to_string()),
    }
}

/// A Cloudflare account id: the literal, or the `{{env.VAR}}` it names
/// (process environment, then the global `secrets.env`).
fn resolve_account_id(spec: &ProviderSpec) -> Option<String> {
    let raw = spec.account_id.as_deref()?;
    match apb_core::connector::secrets::parse_env_ref(raw) {
        Some(var) => apb_core::decisions::resolve_key_var(&var),
        None => Some(raw.to_string()),
    }
    .filter(|id| Cloudflare::valid_account_id(id))
}
