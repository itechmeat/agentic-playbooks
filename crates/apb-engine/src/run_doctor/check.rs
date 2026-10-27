//! One row of `apb doctor --run` and its three levels.

/// One diagnosis line. `status` is one of `"ok"`, `"warn"`, `"fail"` - the
/// same three levels `apb_core::doctor::CheckStatus` uses, as plain strings so
/// this stays a leaf type with no cross-crate enum to keep in sync.
#[derive(Debug, Clone)]
pub struct RunCheck {
    pub status: &'static str,
    pub subject: String,
    pub detail: String,
}

pub const OK: &str = "ok";
pub const WARN: &str = "warn";
pub const FAIL: &str = "fail";

impl RunCheck {
    pub(super) fn new(
        status: &'static str,
        subject: impl Into<String>,
        detail: impl Into<String>,
    ) -> Self {
        RunCheck {
            status,
            subject: subject.into(),
            detail: detail.into(),
        }
    }
}
