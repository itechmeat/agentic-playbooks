//! A count over a total, the one shape every rate in `apb stats` takes.

use serde::Serialize;

use crate::decision::report::stats::rate;

/// `count` of `of`, and the share when `of` is not zero.
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize)]
pub struct Rate {
    pub count: usize,
    pub of: usize,
    #[cfg_attr(feature = "ts", ts(optional))]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rate: Option<f64>,
}

impl Rate {
    pub(crate) fn new(count: usize, of: usize) -> Self {
        Rate {
            count,
            of,
            rate: rate(count, of),
        }
    }

    /// `7/9 (78%)`, or `0/0`.
    pub fn text(&self) -> String {
        match self.rate {
            Some(r) => format!("{}/{} ({:.0}%)", self.count, self.of, r * 100.0),
            None => format!("{}/{}", self.count, self.of),
        }
    }
}

/// `x` rounded to two decimals.
pub(super) fn round2(x: f64) -> f64 {
    (x * 100.0).round() / 100.0
}

/// `b - a`, rounded to two decimals, when both are known.
pub(super) fn delta(a: Option<f64>, b: Option<f64>) -> Option<f64> {
    Some(round2(b? - a?))
}
