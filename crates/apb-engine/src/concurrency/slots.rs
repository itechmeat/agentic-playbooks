//! The concurrency slot store (routine health D1): the signed
//! per-playbook file and its entries, and the display read. A leaf: the
//! run surfaces read it without the admission and turn logic of
//! [`crate::concurrency`].

use std::path::Path;

use apb_core::scope::Scope;
use serde::{Deserialize, Serialize};

/// The file of one playbook's slots: `.apb/concurrency/<scope>/<id>.json`,
/// signed with the installation key and written only through
/// `signed_store::transact` with `Unsigned::Refuse`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConcurrencyFile {
    pub schema: u32,
    pub scope: String,
    pub playbook: String,
    /// One entry per `key_digest`; empty slots are removed on write. The
    /// signed store counts an array under `slots`.
    #[serde(default)]
    pub slots: Vec<Slot>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mac: Option<String>,
}

impl apb_core::signed_store::SignedDoc for ConcurrencyFile {
    const MAC_DOMAIN: &'static str = "apb-concurrency-v1";
    const SCHEMA: u32 = 1;
    const DIR: &'static str = "concurrency";
    const ENTRIES_KEY: &'static str = "slots";

    fn fresh(scope: Scope, playbook: &str) -> Self {
        ConcurrencyFile {
            schema: Self::SCHEMA,
            scope: scope.as_str().to_string(),
            playbook: playbook.to_string(),
            slots: Vec::new(),
            mac: None,
        }
    }

    fn identity(&self) -> (&str, &str) {
        (&self.scope, &self.playbook)
    }

    fn mac(&self) -> Option<&str> {
        self.mac.as_deref()
    }

    fn set_mac(&mut self, mac: Option<String>) {
        self.mac = mac;
    }

    fn set_schema(&mut self, schema: u32) {
        self.schema = schema;
    }
}

/// One rendered key's state: at most one active and one waiting run, and
/// the stops promised but not posted yet.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Slot {
    pub key_digest: String,
    pub display: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active: Option<Holder>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub waiting: Option<Holder>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub stops: Vec<PendingStop>,
}

/// One run holding or waiting for the key.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Holder {
    pub run_id: String,
    /// Set while the claim is provisional: a start not yet confirmed, a
    /// resume not yet journaled, or a waiter. Its owner holds the `flock`
    /// lock `<reservation>.lock` in the owners directory for as long as
    /// it owns the claim.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reservation: Option<String>,
}

/// A stop a slot promised and did not post yet.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PendingStop {
    pub run_id: String,
    pub reason: String,
}

/// Reads a slot file for display only: empty when it does not verify or
/// does not belong to this playbook.
pub(crate) fn read_slots(root: &Path, scope: Scope, playbook: &str) -> Vec<Slot> {
    match apb_core::signed_store::read::<ConcurrencyFile>(root, scope, playbook) {
        Ok(loaded) if loaded.signed => loaded.file.slots,
        _ => Vec::new(),
    }
}
