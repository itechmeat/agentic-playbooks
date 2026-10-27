//! A distribution of millisecond durations.

use serde::Serialize;

use crate::decision::report::stats::median;

/// A distribution of millisecond durations: how many, and the median.
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize)]
pub struct Waits {
    pub count: usize,
    #[cfg_attr(feature = "ts", ts(optional))]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub median_ms: Option<u64>,
    #[cfg_attr(feature = "ts", ts(optional))]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_ms: Option<u64>,
}

impl Waits {
    pub(crate) fn of(mut values: Vec<u64>) -> Self {
        Waits {
            count: values.len(),
            max_ms: values.iter().copied().max(),
            median_ms: median(&mut values),
        }
    }
}
