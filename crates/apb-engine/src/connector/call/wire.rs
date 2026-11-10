//! Leaf constants of the call path (ADP): the response-read cap shared by
//! the HTTP call, the page walk and the content-type guard. No imports (the
//! point of the file) (ADP: the call units must not close a cycle through
//! their shared constants).
#![allow(dead_code)]

/// The maximum response body read into memory (spec 6.1). A longer body is
/// cut at the cap: a read-only call fails as `incomplete_response`, a call
/// that may have changed the world stays a success but carries
/// `response_complete: false` on its result.
pub(crate) const BODY_CAP: usize = 1024 * 1024;

/// The default User-Agent (spec 4.4): ureq sends its own, and GitHub rejects
/// requests without one it recognizes. Overridden by a function `headers`
/// entry naming `User-Agent`.
pub(crate) const USER_AGENT: &str = concat!("apb/", env!("CARGO_PKG_VERSION"));
