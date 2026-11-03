//! One module per API resource family. Each holds its own handlers and the
//! request/response shapes only it uses; anything shared between families
//! lives in [`crate::state`].

pub mod auth;
pub mod connectors;
pub mod current;
pub mod demo;
pub mod discover;
pub mod estimate;
pub mod fit;
pub mod graph;
pub mod judge;
pub mod memory;
pub mod meta;
pub mod notify;
pub mod playbooks;
pub mod problems;
pub mod profiles;
pub mod quality;
pub mod runs;
pub mod scenario;
// --- 0.23.0 stats (C3) ---
pub mod stats;
// --- end of 0.23.0 stats ---
pub mod suggestions;
pub mod trash;
pub mod trust;
