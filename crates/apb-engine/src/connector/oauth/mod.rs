//! The OAuth2 refresh support: `token` is the in-memory token source a call
//! mints its bearer access token from, `consent` is the loopback consent
//! flow behind `apb connector oauth`. The pure helpers both use live in
//! `apb_core::connector::oauth`.

pub mod consent;
pub(crate) mod token;

pub(crate) use token::TokenSource;

// `consent` is reached as `connector::oauth::consent::{plan, start, finish}`.

/// The warning a rotated refresh token adds to the call result. Exact text:
/// the person must keep the newest token themselves, because apb stores no
/// secrets.
pub(crate) const ROTATION_WARNING: &str = "the token endpoint returned a new refresh token; apb does not store tokens, so the one in your secret store may stop working; keep the newest one with a `{{cmd:...}}` helper";
