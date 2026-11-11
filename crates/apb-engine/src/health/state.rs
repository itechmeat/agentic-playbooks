//! The health state file (routine health D2): one signed file per watched
//! playbook, `.apb/health/<scope>/<playbook>.json`, with the rule states,
//! the hold, the mute, the generation that fences every claim, and the
//! notice ledger. Written only through `signed_store::transact` with
//! `Unsigned::Refuse`.

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::OnceLock;

use apb_core::scope::Scope;
use serde::{Deserialize, Serialize};

use crate::notify::record::NotifyTargetRecord;

/// The most notices the ledger keeps.
pub const LEDGER_MAX: usize = 50;

/// One playbook's health state.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HealthFile {
    pub schema: u32,
    pub scope: String,
    pub playbook: String,
    /// The first evaluation's time: the `overdue` reference before any
    /// success and before any clear.
    #[serde(default)]
    pub first_seen_ms: u64,
    /// Set by a person's clear: facts started before it do not count, and
    /// `overdue` measures from it without a later success.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub counted_since_ms: Option<u64>,
    /// Raised by every clear, mute and unmute: a claim made under an older
    /// generation is never dispatched and never settles rule state.
    #[serde(default)]
    pub generation: u64,
    /// By rule name.
    #[serde(default)]
    pub rules: BTreeMap<String, RuleState>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hold: Option<Hold>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub muted_until_ms: Option<u64>,
    /// The notice ledger, oldest first, at most [`LEDGER_MAX`].
    #[serde(default)]
    pub notices: Vec<NoticeRecord>,
    #[serde(default)]
    pub evaluated_at_ms: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mac: Option<String>,
}

/// One rule's state.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RuleState {
    pub tripped: bool,
    /// When the current trip began; meaningful while `tripped`.
    #[serde(default)]
    pub since_ms: u64,
    #[serde(default)]
    pub detail: String,
    /// The trip notice of the current trip, once one was recorded.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trip_notice: Option<String>,
    /// When a trip notice of this rule was last delivered; kept across a
    /// recovery (the cooldown spans trips), removed only by a clear.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delivered_ms: Option<u64>,
}

/// A hold: starts are refused until a person clears it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Hold {
    pub since_ms: u64,
    pub rule: String,
    pub detail: String,
}

/// One notice of the ledger.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NoticeRecord {
    /// `h-` and a nonce; immutable.
    pub id: String,
    pub rule: String,
    /// `trip` or `recovery`.
    pub kind: String,
    pub recorded_ms: u64,
    /// Scrubbed, immutable.
    pub subject: String,
    /// Scrubbed, immutable.
    pub text: String,
    /// The target when recorded, with the digests that pin it.
    pub target: NotifyTargetRecord,
    /// `pending`, `claimed`, `sent`, `failed`, `unknown` or `retired`.
    pub state: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub claimant: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub claimed_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub claim_generation: Option<u64>,
    #[serde(default)]
    pub tries: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

impl apb_core::signed_store::SignedDoc for HealthFile {
    const MAC_DOMAIN: &'static str = "apb-health-v1";
    const SCHEMA: u32 = 1;
    const DIR: &'static str = "health";
    const ENTRIES_KEY: &'static str = "notices";

    fn fresh(scope: Scope, playbook: &str) -> Self {
        HealthFile {
            schema: Self::SCHEMA,
            scope: scope.as_str().to_string(),
            playbook: playbook.to_string(),
            first_seen_ms: 0,
            counted_since_ms: None,
            generation: 0,
            rules: BTreeMap::new(),
            hold: None,
            muted_until_ms: None,
            notices: Vec::new(),
            evaluated_at_ms: 0,
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

/// `<id>` or `global:<id>`, the reference every command names.
pub fn reference(scope: Scope, id: &str) -> String {
    match scope {
        Scope::Global => format!("{}{id}", apb_core::policy_file::GLOBAL_PREFIX),
        Scope::Project => id.to_string(),
    }
}

/// The `playbook_on_hold` refusal of a held playbook: its hold, or a
/// state file that does not verify. A hold refuses whether or not the
/// playbook is still watched.
pub fn hold_refusal(root: &Path, scope: Scope, id: &str) -> Result<(), serde_json::Value> {
    if !apb_core::registry::is_safe_segment(id) {
        return Ok(());
    }
    let path = apb_core::signed_store::file::<HealthFile>(root, scope, id);
    if !path.exists() {
        return Ok(());
    }
    let reference = reference(scope, id);
    let refuse = |rule: &str, since: String, why: String| {
        serde_json::json!({
            "policy": "playbook_on_hold",
            "id": reference,
            "detail": format!(
                "playbook `{id}` is on hold{}: {why}; a person clears it with `apb health clear {reference}`",
                if since.is_empty() { String::new() } else { format!(" since {since}") }
            ),
            "rule": rule,
            "since": since,
        })
    };
    match apb_core::signed_store::read::<HealthFile>(root, scope, id) {
        Ok(l) if l.signed => match l.file.hold {
            Some(h) => Err(refuse(
                &h.rule,
                apb_core::clock::iso_utc(h.since_ms),
                format!("{} ({})", h.rule, h.detail),
            )),
            None => Ok(()),
        },
        _ => Err(refuse(
            "unreadable",
            String::new(),
            format!("health state unreadable: {}", path.display()),
        )),
    }
}

/// This process's claim nonce, generated once.
pub fn process_nonce() -> &'static str {
    static NONCE: OnceLock<String> = OnceLock::new();
    NONCE.get_or_init(apb_core::connector::marker::new_nonce)
}

/// Drops the oldest final notices first until the ledger holds at most
/// [`LEDGER_MAX`].
pub fn trim_ledger(notices: &mut Vec<NoticeRecord>) {
    while notices.len() > LEDGER_MAX {
        let oldest_final = notices.iter().position(NoticeRecord::is_final);
        match oldest_final {
            Some(i) => {
                notices.remove(i);
            }
            None => {
                notices.remove(0);
            }
        }
    }
}

impl NoticeRecord {
    /// A state that never changes again: `sent`, `unknown`, `retired`, or
    /// `failed` with three tries.
    pub fn is_final(&self) -> bool {
        matches!(self.state.as_str(), "sent" | "unknown" | "retired")
            || (self.state == "failed" && self.tries >= 3)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use apb_core::signed_store::{self, StoreError, Unsigned};

    fn notice(id: &str, state: &str, tries: u32) -> NoticeRecord {
        NoticeRecord {
            id: id.into(),
            rule: "overdue".into(),
            kind: "trip".into(),
            recorded_ms: 1,
            subject: "s".into(),
            text: "t".into(),
            target: NotifyTargetRecord {
                connector: "c".into(),
                account: "a".into(),
                function: "f".into(),
                to: "42".into(),
                connector_digest: "cd".into(),
                account_digest: "ad".into(),
            },
            state: state.into(),
            claimant: None,
            claimed_ms: None,
            claim_generation: None,
            tries,
            detail: None,
        }
    }

    #[test]
    fn a_fresh_file_round_trips_signed() {
        apb_core::run_origin::ensure_key().unwrap();
        let t = tempfile::tempdir().unwrap();
        signed_store::transact(
            t.path(),
            Scope::Project,
            "p",
            Unsigned::Refuse,
            |h: &mut HealthFile| {
                h.first_seen_ms = 7;
                h.generation = 2;
                h.notices.push(notice("h-1", "pending", 0));
                Ok::<_, StoreError>(((), true))
            },
        )
        .unwrap();
        let read = signed_store::read::<HealthFile>(t.path(), Scope::Project, "p").unwrap();
        assert!(read.signed);
        assert_eq!(read.file.first_seen_ms, 7);
        assert_eq!(read.file.generation, 2);
        assert_eq!(read.file.notices, vec![notice("h-1", "pending", 0)]);
    }

    #[test]
    fn a_changed_byte_reads_unverified_under_refuse() {
        apb_core::run_origin::ensure_key().unwrap();
        let t = tempfile::tempdir().unwrap();
        signed_store::transact(
            t.path(),
            Scope::Project,
            "p",
            Unsigned::Refuse,
            |h: &mut HealthFile| {
                h.first_seen_ms = 7;
                Ok::<_, StoreError>(((), true))
            },
        )
        .unwrap();
        let path = signed_store::file::<HealthFile>(t.path(), Scope::Project, "p");
        let text = std::fs::read_to_string(&path)
            .unwrap()
            .replace("\"first_seen_ms\": 7", "\"first_seen_ms\": 8");
        std::fs::write(&path, text).unwrap();
        let e = signed_store::transact(
            t.path(),
            Scope::Project,
            "p",
            Unsigned::Refuse,
            |_: &mut HealthFile| Ok::<_, StoreError>(((), false)),
        )
        .unwrap_err();
        assert!(matches!(e, StoreError::Unverified(_)), "{e}");
    }

    #[test]
    fn the_ledger_keeps_fifty_dropping_final_records_first() {
        let mut ledger: Vec<NoticeRecord> = Vec::new();
        ledger.push(notice("open-0", "pending", 0));
        for i in 0..30 {
            ledger.push(notice(&format!("sent-{i}"), "sent", 0));
        }
        ledger.push(notice("failing", "failed", 1));
        ledger.push(notice("done-failing", "failed", 3));
        for i in 0..20 {
            ledger.push(notice(&format!("new-{i}"), "retired", 0));
        }
        assert_eq!(ledger.len(), 53);
        trim_ledger(&mut ledger);
        assert_eq!(ledger.len(), LEDGER_MAX);
        let ids: Vec<&str> = ledger.iter().map(|n| n.id.as_str()).collect();
        assert_eq!(ids[0], "open-0", "an open record is never dropped");
        assert!(!ids.contains(&"sent-0") && !ids.contains(&"sent-1") && !ids.contains(&"sent-2"));
        assert!(ids.contains(&"sent-3"));
        assert!(ids.contains(&"failing"));
        assert_eq!(*ids.last().unwrap(), "new-19");
    }
}
