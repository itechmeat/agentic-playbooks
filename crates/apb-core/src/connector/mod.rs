//! Connector: a declarative HTTP link between a playbook node and an
//! external service (spec 2026-07-18-connectors-design). This task only
//! covers the manifest schema and its structural validation (`def.rs`);
//! template rendering, account config, and the on-disk store are added by
//! later tasks in sibling modules.

pub mod common;
pub mod config;
pub mod def;
pub mod secrets;
pub mod store;
pub mod template;
pub use common::*;
pub use config::*;
pub use def::*;
pub use secrets::*;
pub use store::*;
pub use template::*;
