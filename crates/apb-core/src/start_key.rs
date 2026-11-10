//! The start key (routine operations, D1): a caller's key that makes a
//! start happen at most once per event. Its pure half: the key check, the
//! request a key binds (captured once by the launch surface and frozen),
//! and the signed per-playbook file of keys under `.apb/start-keys/`.
//!
//! The key is never stored, logged or journaled: only its MAC under this
//! installation's key, so a key that carries an order number or a mail id
//! cannot be read back, and a copied project does not match on another
//! machine. The request is a MAC too: param values may be secret.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::scope::Scope;
use crate::signed_store::SignedDoc;

/// The longest start key, in characters.
pub const KEY_MAX_CHARS: usize = 200;
/// The MAC domain of the key.
pub const KEY_DOMAIN: &str = "apb-start-key-v1";
/// The MAC domain of the request a key stands for.
pub const REQUEST_DOMAIN: &str = "apb-start-request-v1";
/// The default days a key is remembered (`runs.start_key_days`).
pub const DEFAULT_DAYS: u32 = 7;
/// The refusal code of a key that does not pass [`StartKey::parse`].
pub const INVALID: &str = "start_key_invalid";
/// How many hex characters of the key's MAC a run's provenance shows.
pub const SHOWN_TAG_CHARS: usize = 12;

/// A caller's start key, checked: 1 to 200 characters, no control or
/// hidden character, no leading or trailing whitespace, and not
/// credential-shaped.
#[derive(Clone, PartialEq, Eq)]
pub struct StartKey(String);

impl std::fmt::Debug for StartKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("StartKey(***)")
    }
}

impl StartKey {
    /// The key, or why it is refused (the reason never repeats the key).
    pub fn parse(raw: &str) -> Result<StartKey, String> {
        let chars = raw.chars().count();
        if chars == 0 {
            return Err("the key is empty".into());
        }
        if chars > KEY_MAX_CHARS {
            return Err(format!(
                "the key has {chars} characters; at most {KEY_MAX_CHARS}"
            ));
        }
        if raw.chars().any(hidden) {
            return Err("the key holds a control or hidden character".into());
        }
        if raw.trim() != raw {
            return Err("the key starts or ends with whitespace".into());
        }
        if crate::token_shape::refuses_to_store(raw) {
            return Err(
                "the key looks like a credential; use an event or run id, a date or another name"
                    .into(),
            );
        }
        Ok(StartKey(raw.to_string()))
    }

    /// The key as the caller gave it: only to hand it on (a handoff file),
    /// never to store or print.
    pub fn expose(&self) -> &str {
        &self.0
    }
}

/// A control character, or one that hides itself in a terminal.
fn hidden(c: char) -> bool {
    c.is_control()
        || matches!(
            c,
            '\u{200B}'..='\u{200F}' | '\u{202A}'..='\u{202E}' | '\u{2060}'..='\u{2064}' | '\u{FEFF}'
        )
}

/// The keyed MAC of the key under this installation's key.
pub fn key_tag(key: &StartKey) -> std::io::Result<String> {
    crate::run_origin::tag(KEY_DOMAIN, key.0.as_bytes())
}

/// Every explicit, execution-affecting argument the caller passed, as the
/// request a key binds, captured once by the launch surface from the
/// caller's own arguments, before any gate, candidate choice, prompt or
/// default changes them. Values recall and defaults fill in later are not
/// here: a retry that passes the same arguments is the same request even
/// when remembered input changed in between.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct StartRequestFields {
    /// The canonical form's version; bump it when a field is added.
    pub v: u32,
    /// The version asked for, never the one chosen.
    pub version: Option<String>,
    pub params: BTreeMap<String, String>,
    pub instruction: Option<String>,
    pub worktree: Option<String>,
    /// SHA-256 of the overrides' canonical JSON.
    pub overrides: Option<String>,
    /// The requested execution mode (`cli`, `host`).
    pub execution: Option<String>,
    /// `autonomous`, `supervised` or `agent_supervised`.
    pub supervision: String,
    /// `auto`, `off` or `refresh`.
    pub cache: String,
    pub max_parallel: Option<usize>,
    pub max_patches_per_run: Option<u32>,
    pub context_max_bytes: Option<usize>,
    pub context_compact_model: Option<String>,
    pub deadline_ms: Option<u64>,
    /// The budget arguments as the caller typed them.
    pub budget: Option<serde_json::Value>,
    pub unattended: bool,
    pub allow_shared_workdir: bool,
    pub continued_from: Option<String>,
    /// The effective `external_data` flag the surface computed.
    pub external_data: bool,
}

/// The serde name of a unit enum value (`autonomous`, `off`), as a
/// request field holds a mode.
pub fn label<T: Serialize>(v: &T) -> String {
    serde_json::to_value(v)
        .ok()
        .and_then(|v| v.as_str().map(str::to_string))
        .unwrap_or_default()
}

/// The canonical form's version of [`StartRequestFields`].
pub const REQUEST_V: u32 = 1;

/// The request, frozen: built only by [`StartRequest::capture`], no
/// setter, no public field. Its tag is computed once at capture.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StartRequest {
    tag: String,
}

impl StartRequest {
    /// The keyed MAC of the fields' canonical JSON (field order is fixed,
    /// `params` a sorted map).
    pub fn capture(fields: &StartRequestFields) -> std::io::Result<StartRequest> {
        let bytes = serde_json::to_vec(fields).map_err(std::io::Error::other)?;
        Ok(StartRequest {
            tag: crate::run_origin::tag(REQUEST_DOMAIN, &bytes)?,
        })
    }

    /// The request a handoff carried as its tag: the receiving side of a
    /// private, single-use handoff file only.
    pub fn from_handoff(tag: String) -> StartRequest {
        StartRequest { tag }
    }

    pub fn tag(&self) -> &str {
        &self.tag
    }
}

/// What a keyed start carries from its surface to the engine.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StartKeyRequest {
    pub key: StartKey,
    pub reuse: bool,
    pub request: StartRequest,
}

/// Whether an entry's owner still prepares its run.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EntryState {
    Pending,
    Bound,
}

/// One key of one playbook.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StartKeyEntry {
    /// [`key_tag`].
    pub key: String,
    /// [`StartRequest::tag`].
    pub request: String,
    /// `pending` while a preparation owns the entry, `bound` once its run
    /// is verifiable.
    pub state: EntryState,
    /// The random id naming the owner's lock file while `pending`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reservation: Option<String>,
    /// Absent until preparation allocated the run directory.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_id: Option<String>,
    pub at_ms: u64,
    /// How many times `--start-key-reuse` moved this key.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub reused: u32,
}

fn is_zero(n: &u32) -> bool {
    *n == 0
}

/// The start-key file of one playbook: `.apb/start-keys/<scope>/<id>.json`,
/// signed with the installation key and written only through
/// `signed_store::transact` with `Unsigned::Refuse`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StartKeysFile {
    pub schema: u32,
    pub scope: String,
    pub playbook: String,
    #[serde(default)]
    pub entries: Vec<StartKeyEntry>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mac: Option<String>,
}

impl SignedDoc for StartKeysFile {
    const MAC_DOMAIN: &'static str = "apb-start-keys-v1";
    const SCHEMA: u32 = 1;
    const DIR: &'static str = "start-keys";
    const ENTRIES_KEY: &'static str = "entries";

    fn fresh(scope: Scope, playbook: &str) -> Self {
        StartKeysFile {
            schema: Self::SCHEMA,
            scope: scope.as_str().to_string(),
            playbook: playbook.to_string(),
            entries: Vec::new(),
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

const DAY_MS: u64 = 86_400_000;

fn live(e: &StartKeyEntry, now_ms: u64, days: u32) -> bool {
    now_ms.saturating_sub(e.at_ms) < u64::from(days) * DAY_MS
}

impl StartKeysFile {
    /// The live entry of `key`: within `days` of its `at_ms`.
    pub fn find(&self, key: &str, now_ms: u64, days: u32) -> Option<&StartKeyEntry> {
        self.entries
            .iter()
            .find(|e| e.key == key && live(e, now_ms, days))
    }

    /// Drops every entry older than `days`.
    pub fn expire(&mut self, now_ms: u64, days: u32) {
        self.entries.retain(|e| live(e, now_ms, days));
    }
}

#[cfg(test)]
mod tests;
