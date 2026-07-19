//! Connector: a declarative HTTP link between a playbook node and an
//! external service (spec 2026-07-18-connectors-design). `def` is the
//! manifest schema and its structural validation; `template` renders
//! placeholders; `config` and `secrets` handle account configuration and
//! secret resolution; `store` is the on-disk connector store; `resolve`
//! validates a playbook's connector bindings against installed connectors
//! and configured accounts and expands them into per-node grants.

pub mod common;
pub mod config;
pub mod def;
pub mod resolve;
pub mod secrets;
pub mod store;
pub mod template;
pub use common::*;
pub use config::*;
pub use def::*;
pub use resolve::*;
pub use secrets::*;
pub use store::*;
pub use template::*;
