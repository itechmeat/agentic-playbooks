//! Connector execution: everything that happens after `apb-core` has parsed a
//! connector manifest and resolved a grant.
//!
//! The submodules follow the call's own path. [`call`] renders and performs an
//! HTTP call; [`smtp`] and [`imap`] are the two non-HTTP transports; [`inbox`]
//! is the local-store-only kind (no network, no secret); [`result`] is the
//! shared result taxonomy and redaction sink the four call kinds point at
//! (which keeps the file graph acyclic); [`run`] carries the per-run snapshot
//! lookup; [`prompt`] renders the agent-facing description of the granted
//! functions; and [`contract_test`] is the offline runner for a connector's
//! `tests.yaml` cases, reusing [`call`]'s render path with no network.
//!
//! The manifest parsing, grant resolution, secret chain, and account config
//! that these modules consume live in `apb_core::connector`.

pub mod call;
pub mod contract_test;
pub mod imap;
pub mod inbox;
pub mod prompt;
pub mod result;
pub mod run;
pub mod smtp;
