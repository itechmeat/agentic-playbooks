//! Connector: a declarative HTTP link between a playbook node and an
//! external service (spec 2026-07-18-connectors-design). `def` is the
//! manifest schema and its structural validation; `template` renders
//! placeholders; `config` and `secrets` handle account configuration and
//! secret resolution; `store` is the on-disk connector store; `resolve`
//! validates a playbook's connector bindings against installed connectors
//! and configured accounts and expands them into per-node grants;
//! `text_mode` prepares agent-written string args (`text_args`) before they
//! reach a request body; `people` names the args of a function that people
//! read; `idempotency` declares how a send is kept from acting twice,
//! `marker` renders and matches the hidden send marker, and `mcp` is the
//! mcp connector kind: the `mcp:` blocks, tool digests and the pin store.

pub mod common;
pub mod config;
pub mod contract;
pub mod def;
// Deliberately not glob re-exported: `EventFilter` names no site the store
// layer needs, and callers use `event_filter::EventFilter`.
pub mod event_filter;
pub mod idempotency;
// Deliberately not glob re-exported: `inbox::{read, depth}` would collide
// with the `pub use store::*` glob below. Callers use `inbox::Inbox`.
pub mod inbox;
pub mod install;
pub mod marker;
// Not glob re-exported: callers name `mcp::` types by their module.
pub mod mcp;
// No glob re-export: `official::{list,get}` would collide with `store::{list,...}`
// under the `pub use store::*` glob below. Callers use `official::list()` etc.
pub mod oauth;
pub mod official;
pub mod paginate;
pub mod people;
pub mod resolve;
pub mod secrets;
pub mod sites;
pub mod store;
pub mod template;
pub mod text_mode;
// Not glob re-exported, for the same reason as `inbox`: `webhook::verify_*`
// and future `store::*` names must not compete under one glob.
pub mod webhook;
pub use common::*;
pub use config::*;
pub use contract::*;
pub use def::*;
pub use install::*;
pub use resolve::*;
pub use secrets::*;
pub use store::*;
pub use template::*;
pub use text_mode::{TextMode, apply_text_args};
