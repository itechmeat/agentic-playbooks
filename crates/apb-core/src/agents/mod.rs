//! Everything apb knows about each built-in agent CLI, one module per agent:
//! how it is found and launched, how its output reads, what it needs from
//! the host and which models it takes. Every other part of apb asks the
//! registry here ([`lookup`], [`all`]) instead of matching on an agent id.
//!
//! The modules hold data and pure functions; an effect an agent needs (a
//! run-scoped config file, a session listing) is described as data or a
//! hook that the engine runs the same way for every agent.
//!
//! Adding an agent: a module implementing [`Agent`], one entry in [`all`],
//! and a contract fixture (see `docs/PROFILES.md`, "Adding an agent").

mod agent;
pub mod check;
pub mod cost_basis;
mod exposure;
mod forms;
pub mod hook_host;
pub mod mcp_host;
mod output;
mod policy;
mod sandbox;
mod setup;
mod types;

pub mod agy;
pub mod claude;
pub mod codex;
pub mod cursor;
pub mod dsh;
pub mod grok;
pub mod hermes;
pub mod opencode;
pub mod pi;
pub mod qoder;
pub mod zcode;

pub use agent::{Agent, Capabilities, Descriptor, Transcript, Verified};
pub use exposure::{
    CONNECTED, ExpectedServer, ExpectedTools, ExposedTools, ExposureMismatch, PENDING, compare,
};
pub use forms::{Forms, FreshForm, FreshSession, SessionListing};
pub use hook_host::{HookRegistration, StopEvent, StopHookHost};
pub use mcp_host::{McpHost, McpMethod, McpRegistration, McpServer};
pub use output::{
    AgentUsage, Conformance, CostBasis, OutputFormat, PlainText, PricedFrom, Promises, Reply,
    UsageSource, json_string_field,
};
pub use policy::{ClosedList, ModelPolicy, OpenModels};
pub use sandbox::{Access, HomeBind, HomeCopy, SandboxNeeds};
pub use setup::{HostNotes, HostSetup, NoSetup, ScopedEnvError};
pub use types::{AgentCategory, AgentEnvSet, AuthHint, AuthKind, Authority, ModelsInventory};

/// The built-in agents, in the order detection reports them and the
/// dashboard lists them.
static ALL: &[&dyn Agent] = &[
    &claude::Claude,
    &codex::Codex,
    &agy::Agy,
    &opencode::Opencode,
    &pi::Pi,
    &hermes::Hermes,
    &grok::Grok,
    &cursor::Cursor,
    &qoder::Qoder,
    &zcode::Zcode,
    &dsh::Dsh,
];

/// Every built-in agent, in detection order.
pub fn all() -> &'static [&'static dyn Agent] {
    ALL
}

/// The cloud catalog model `model` names when `agent` runs it, if any: the
/// one question every surface asks the agent's model rules
/// (`ModelPolicy::cloud_model`, decision record 0014).
pub fn cloud_model(agent: &str, model: &str) -> Option<crate::cloud_models::CloudModelRef> {
    lookup(agent)?.models().cloud_model(model)
}

/// The key variable of the cloud provider `model` routes to when `agent`
/// runs it, read from the agent's routes alone: it holds without a catalog
/// snapshot, as for an executor an older apb resolved before it recorded
/// one, so the key is still scrubbed from what such an attempt prints.
pub fn cloud_key_env(agent: &str, model: &str) -> Option<&'static str> {
    crate::cloud_models::lookup(cloud_model(agent, model)?.provider).map(|p| p.key_env)
}

/// The snapshot of the catalog row `model` names when `agent` runs it, when
/// the catalog lists it.
pub fn cloud_snapshot(
    agent: &str,
    model: &str,
    snaps: &[crate::cloud_models::catalog::CatalogSnapshot],
) -> Option<crate::cloud_models::CloudSnapshotRef> {
    crate::cloud_models::CloudSnapshotRef::of(&cloud_model(agent, model)?, snaps)
}

/// The privacy summary of a chain's `(agent, model)` pairs
/// (`crate::cloud_models::privacy`): each pair's class from the catalog, or
/// none for a model outside any catalog.
pub fn chain_privacy<'a>(
    pairs: impl IntoIterator<Item = (&'a str, &'a str)>,
    snaps: &[crate::cloud_models::catalog::CatalogSnapshot],
) -> crate::cloud_models::privacy::PrivacySummary {
    crate::cloud_models::privacy::summarize(
        pairs
            .into_iter()
            .map(|(a, m)| cloud_snapshot(a, m, snaps).map(|c| c.model.privacy_class))
            .collect(),
    )
}

/// The built-in agent `id` names, aliases included; `None` for any other id.
pub fn lookup(id: &str) -> Option<&'static dyn Agent> {
    let id = canonical_id(id);
    ALL.iter().copied().find(|a| a.descriptor().id == id)
}

/// The id a built-in agent alias stands for (`claude-code` is `claude`); any
/// other id is returned unchanged.
pub fn canonical_id(id: &str) -> &str {
    ALL.iter()
        .map(|a| a.descriptor())
        .find(|d| d.aliases.contains(&id))
        .map_or(id, |d| d.id)
}

/// Whether `stdout` of an attempt of `agent` is its machine output, a
/// drifted one, or plain text that is allowed: a built-in form without its
/// machine shape means the CLI's output format changed, while a custom form
/// (`agents.<id>.invocation`) read as plain text is the allowed fallback.
pub fn conformance(agent: &dyn Agent, form_is_builtin: bool, stdout: &str) -> Conformance {
    agent.output().conformance(form_is_builtin, stdout)
}
