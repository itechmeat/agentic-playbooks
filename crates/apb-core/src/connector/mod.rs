//! Connector: a declarative HTTP link between a playbook node and an
//! external service (spec 2026-07-18-connectors-design). `def` is the
//! manifest schema and its structural validation; `template` renders
//! placeholders; `config` and `secrets` handle account configuration and
//! secret resolution; `store` is the on-disk connector store; `resolve`
//! validates a playbook's connector bindings against installed connectors
//! and configured accounts and expands them into per-node grants;
//! `text_mode` prepares agent-written string args (`text_args`) before they
//! reach a request body; `idempotency` declares how a send is kept from
//! acting twice, and `marker` renders and matches the hidden send marker.

pub mod common;
pub mod config;
pub mod contract;
pub mod def;
pub mod idempotency;
// Deliberately not glob re-exported: `inbox::{read, depth}` would collide
// with the `pub use store::*` glob below. Callers use `inbox::Inbox`.
pub mod inbox;
pub mod install;
pub mod marker;
// No glob re-export: `official::{list,get}` would collide with `store::{list,...}`
// under the `pub use store::*` glob below. Callers use `official::list()` etc.
pub mod official;
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
