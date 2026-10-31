//! The notification target as the journal records it. A leaf module: the
//! journal (`event.rs`) names this type, so it depends on nothing else in
//! the crate.

use serde::{Deserialize, Serialize};

/// A subscription's target with the digests its connector and account
/// were trusted with when it was set. Every send checks the live connector
/// and account against these digests.
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NotifyTargetRecord {
    #[serde(default)]
    pub connector: String,
    #[serde(default)]
    pub account: String,
    #[serde(default)]
    pub function: String,
    #[serde(default)]
    pub to: String,
    #[serde(default)]
    pub connector_digest: String,
    #[serde(default)]
    pub account_digest: String,
}

impl NotifyTargetRecord {
    /// The target without its digests, as a person names it.
    pub fn target(&self) -> apb_core::notify::NotifyTarget {
        apb_core::notify::NotifyTarget {
            connector: self.connector.clone(),
            account: self.account.clone(),
            function: self.function.clone(),
            to: self.to.clone(),
        }
    }
}
