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
mod exposure;
mod forms;
mod output;
mod policy;
mod setup;
mod types;

pub mod agy;
pub mod claude;
pub mod codex;
pub mod cursor;
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
pub use output::{
    AgentUsage, Conformance, OutputFormat, PlainText, Promises, Reply, UsageSource,
    json_string_field,
};
pub use policy::{ClosedList, ModelPolicy, OpenModels};
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
];

/// Every built-in agent, in detection order.
pub fn all() -> &'static [&'static dyn Agent] {
    ALL
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
