//! One module per API resource family. Each holds its own handlers and the
//! request/response shapes only it uses; anything shared between families
//! lives in [`crate::state`].

pub mod auth;
pub mod connectors;
pub mod meta;
pub mod playbooks;
pub mod profiles;
pub mod runs;
pub mod suggestions;
