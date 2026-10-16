//! The operator pause request, built from the loosely typed arguments the
//! operator surfaces receive (`apb pause`, MCP `run_pause`, the dashboard), so
//! they validate `mode` and `until` the same way.

use super::{PauseBy, PauseMode, PauseRequest};

impl PauseRequest {
    /// An operator pause from surface arguments. `mode` is `drain` (the
    /// default when absent or empty) or `now`; `until` is an RFC 3339
    /// timestamp or a duration from `now_ms` (see
    /// [`apb_core::duration::parse_until`]) and must lie ahead.
    pub fn operator(
        mode: Option<&str>,
        reason: Option<String>,
        until: Option<&str>,
        keep_lock: bool,
        now_ms: u64,
    ) -> Result<Self, String> {
        let mode = match mode.map(str::trim) {
            None | Some("") | Some("drain") => PauseMode::Drain,
            Some("now") => PauseMode::Now,
            Some(other) => return Err(format!("mode must be `drain` or `now`, got `{other}`")),
        };
        let until = match until.map(str::trim).filter(|u| !u.is_empty()) {
            None => None,
            Some(u) => {
                let ms = apb_core::duration::parse_until(u, now_ms)
                    .map_err(|e| format!("until: {e}"))?;
                if ms <= now_ms {
                    return Err("until is in the past".to_string());
                }
                Some(ms)
            }
        };
        let reason = reason
            .map(|r| r.trim().to_string())
            .filter(|r| !r.is_empty());
        Ok(PauseRequest {
            reason,
            by: Some(PauseBy::Operator),
            mode,
            until,
            keep_lock,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn operator_request_validates_mode_and_until() {
        let r =
            PauseRequest::operator(None, Some("  lunch ".into()), Some("90m"), true, 1000).unwrap();
        assert_eq!(r.mode, PauseMode::Drain);
        assert_eq!(r.reason.as_deref(), Some("lunch"));
        assert_eq!(r.until, Some(1000 + 5_400_000));
        assert_eq!(r.by, Some(PauseBy::Operator));
        assert!(r.keep_lock);
        let now = PauseRequest::operator(Some("now"), None, None, false, 0).unwrap();
        assert_eq!(now.mode, PauseMode::Now);
        assert!(PauseRequest::operator(Some("later"), None, None, false, 0).is_err());
        assert!(
            PauseRequest::operator(None, None, Some("1970-01-01T00:00:01Z"), false, 5000).is_err()
        );
        assert!(PauseRequest::operator(None, None, Some("soon"), false, 0).is_err());
    }
}
