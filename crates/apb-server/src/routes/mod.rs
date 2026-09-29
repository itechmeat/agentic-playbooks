//! One module per API resource family. Each holds its own handlers and the
//! request/response shapes only it uses; anything shared between families
//! lives in [`crate::state`].

pub mod auth;
pub mod connectors;
pub mod meta;
pub mod playbooks;
pub mod profiles;
pub mod runs;
// --- 0.23.0 stats (C3) ---
pub mod stats;
// --- end of 0.23.0 stats ---
pub mod suggestions;
pub mod trash;
pub mod trust;
