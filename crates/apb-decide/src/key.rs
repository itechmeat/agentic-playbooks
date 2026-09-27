//! The API key wrapper.

use std::fmt;

/// A provider key. It prints as `***` through `Debug` and `Display`, is not
/// serializable, and is only read by the adapter that builds the
/// `Authorization` header.
#[derive(Clone)]
pub struct ApiKey(String);

impl ApiKey {
    pub fn new(value: impl Into<String>) -> Self {
        ApiKey(value.into())
    }

    /// The key itself, for the request header only.
    pub(crate) fn expose(&self) -> &str {
        &self.0
    }

    /// `text` with every occurrence of the key replaced by `***`: for any
    /// provider-supplied text that might echo the request back.
    pub fn scrub(&self, text: &str) -> String {
        if self.0.is_empty() {
            text.to_string()
        } else {
            text.replace(&self.0, "***")
        }
    }
}

impl fmt::Debug for ApiKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("***")
    }
}

impl fmt::Display for ApiKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("***")
    }
}
