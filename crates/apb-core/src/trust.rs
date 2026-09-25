//! Playbook lifecycle and trust (spec 3.1).
//!
//! Two independent axes, deliberately not mixed into one scale:
//! - **lifecycle** (`draft`/`active`/`retired`) - readiness of the definition
//!   for regular matching; stored as a file next to the definition.
//! - **trust** (`approved` for a specific digest) - whether this particular
//!   content is approved for transparent execution; stored in the global
//!   `trust.json`.
//!
//! Trust is tied to the digest (spec 9): any change to the content changes the
//! digest, and the previous approval no longer applies to it - untrusted until
//! a new confirmation.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::fsutil::atomic_write_private;

const TRUST_SCHEMA: u32 = 1;

/// Lifecycle stage of a definition. The absence of the file is treated as
/// `Active` - backward compatibility with playbooks created before this
/// machinery existed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Lifecycle {
    Draft,
    Active,
    Retired,
}

impl Lifecycle {
    /// The wire and on-disk name (`draft` / `active` / `retired`).
    pub fn as_str(self) -> &'static str {
        match self {
            Lifecycle::Draft => "draft",
            Lifecycle::Active => "active",
            Lifecycle::Retired => "retired",
        }
    }
    fn parse(s: &str) -> Option<Lifecycle> {
        match s.trim() {
            "draft" => Some(Lifecycle::Draft),
            "active" => Some(Lifecycle::Active),
            "retired" => Some(Lifecycle::Retired),
            _ => None,
        }
    }
}

/// Where the definition came from (spec 3.1). Affects the starting trust:
/// `repository_provided` always starts out untrusted.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(rename_all = "snake_case")]
pub enum OriginKind {
    Bundled,
    AgentGenerated,
    LocallyApproved,
    RepositoryProvided,
}

impl OriginKind {
    /// The wire and on-disk name (`bundled`, `agent_generated`, ...).
    pub fn as_str(self) -> &'static str {
        match self {
            OriginKind::Bundled => "bundled",
            OriginKind::AgentGenerated => "agent_generated",
            OriginKind::LocallyApproved => "locally_approved",
            OriginKind::RepositoryProvided => "repository_provided",
        }
    }
}

/// Reads the definition's lifecycle from `<playbook_dir>/lifecycle`. No file or
/// an unrecognized value - `Active`.
pub fn read_lifecycle(playbook_dir: &Path) -> Lifecycle {
    let p = playbook_dir.join("lifecycle");
    std::fs::read_to_string(&p)
        .ok()
        .and_then(|s| Lifecycle::parse(&s))
        .unwrap_or(Lifecycle::Active)
}

pub fn write_lifecycle(playbook_dir: &Path, lc: Lifecycle) -> std::io::Result<()> {
    std::fs::create_dir_all(playbook_dir)?;
    crate::fsutil::atomic_write(&playbook_dir.join("lifecycle"), lc.as_str().as_bytes())
}

/// What kind of object is approved. `#[serde(default)]` yields `Playbook` for
/// records created before profiles existed (backward compatibility for
/// trust.json).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    #[default]
    Playbook,
    ProfileBundle,
    Connector,
    ConnectorAccount,
}

impl Kind {
    /// The wire and on-disk name (`playbook`, `profile_bundle`, `connector`,
    /// `connector_account`).
    pub fn as_str(self) -> &'static str {
        match self {
            Kind::Playbook => "playbook",
            Kind::ProfileBundle => "profile_bundle",
            Kind::Connector => "connector",
            Kind::ConnectorAccount => "connector_account",
        }
    }

    /// Parses [`Kind::as_str`]'s names.
    pub fn parse(s: &str) -> Option<Kind> {
        [
            Kind::Playbook,
            Kind::ProfileBundle,
            Kind::Connector,
            Kind::ConnectorAccount,
        ]
        .into_iter()
        .find(|k| k.as_str() == s)
    }
}

/// Trust record id for a connector account approval: `"connector/account"`,
/// e.g. `"jira/project-board"`. Used as the `id` field of a `TrustRecord`
/// with `Kind::ConnectorAccount`; approval itself stays keyed by digest.
pub fn account_trust_id(connector: &str, account: &str) -> String {
    format!("{connector}/{account}")
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TrustRecord {
    pub id: String,
    pub origin_kind: OriginKind,
    pub approved_at_ms: u128,
    #[serde(default)]
    pub kind: Kind,
}

/// One approval as every surface lists it: `apb trust list`, MCP
/// `trust_list` and the dashboard's Trust view.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct TrustEntry {
    /// The approved content digest (`sha256:<hex>`).
    pub digest: String,
    /// What it approves: a playbook id, a profile name, a connector name, or
    /// `connector/account`.
    pub id: String,
    pub kind: Kind,
    pub origin_kind: OriginKind,
    /// When it was approved, in Unix milliseconds.
    pub approved_at_ms: u128,
}

/// Which approvals a revoke removes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TrustSelector {
    /// Exactly one approved digest.
    Digest(String),
    /// Every approval recorded under this id (every version of a playbook,
    /// every bundle of a profile), optionally only of one kind.
    Id { id: String, kind: Option<Kind> },
}

impl TrustSelector {
    /// A target starting with `sha256:` names one digest; anything else is an
    /// id. `kind` narrows an id (it does not apply to a digest, which is
    /// already exact).
    pub fn parse(target: &str, kind: Option<Kind>) -> Self {
        let target = target.trim();
        if target.starts_with("sha256:") {
            TrustSelector::Digest(target.to_string())
        } else {
            TrustSelector::Id {
                id: target.to_string(),
                kind,
            }
        }
    }

    fn matches(&self, digest: &str, record: &TrustRecord) -> bool {
        match self {
            TrustSelector::Digest(d) => d == digest,
            TrustSelector::Id { id, kind } => {
                &record.id == id && kind.is_none_or(|k| k == record.kind)
            }
        }
    }
}

/// Global registry of approved digests (`<config_dir>/trust.json`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TrustStore {
    #[serde(default = "default_schema")]
    schema_version: u32,
    #[serde(default)]
    approved: BTreeMap<String, TrustRecord>,
}

fn default_schema() -> u32 {
    TRUST_SCHEMA
}

fn trust_path() -> Option<PathBuf> {
    crate::config::config_dir().map(|d| d.join("trust.json"))
}

impl Default for TrustStore {
    fn default() -> Self {
        Self {
            schema_version: TRUST_SCHEMA,
            approved: BTreeMap::new(),
        }
    }
}

/// Trust status of one object's current digest: approved, changed since an
/// earlier approval (some OTHER digest of the same id is approved - the
/// content moved), or never approved. The one derivation every surface that
/// shows connector or account trust uses (dashboard, MCP, CLI list and doctor).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TrustStatus {
    Approved,
    Changed,
    Unapproved,
}

impl TrustStatus {
    /// The wire string (`approved` / `changed` / `unapproved`).
    pub fn as_str(self) -> &'static str {
        match self {
            TrustStatus::Approved => "approved",
            TrustStatus::Changed => "changed",
            TrustStatus::Unapproved => "unapproved",
        }
    }
}

fn entry(digest: &str, r: &TrustRecord) -> TrustEntry {
    TrustEntry {
        digest: digest.to_string(),
        id: r.id.clone(),
        kind: r.kind,
        origin_kind: r.origin_kind,
        approved_at_ms: r.approved_at_ms,
    }
}

impl TrustStore {
    /// [`TrustStatus`] of `digest`, the current digest of the object `id` of
    /// `kind`.
    pub fn status(&self, digest: &str, id: &str, kind: Kind) -> TrustStatus {
        if self.is_approved(digest) {
            TrustStatus::Approved
        } else if self.approved.values().any(|r| r.kind == kind && r.id == id) {
            TrustStatus::Changed
        } else {
            TrustStatus::Unapproved
        }
    }

    /// Loads the store; a missing file or config directory yields an empty
    /// store. A corrupt file does not crash the caller: a warning is printed to
    /// stderr and an empty store is returned (the data can be recovered by
    /// re-approving).
    pub fn load() -> Self {
        let Some(path) = trust_path() else {
            return Self::default();
        };
        match std::fs::read_to_string(&path) {
            Ok(raw) => match serde_json::from_str::<TrustStore>(&raw) {
                Ok(store) => store,
                Err(e) => {
                    eprintln!(
                        "apb: ignoring malformed trust store `{}`: {e}",
                        path.display()
                    );
                    Self::default()
                }
            },
            Err(_) => Self::default(),
        }
    }

    pub fn is_approved(&self, digest: &str) -> bool {
        self.approved.contains_key(digest)
    }

    /// Where the approval of `digest` came from, if it is approved at all.
    pub fn origin(&self, digest: &str) -> Option<OriginKind> {
        self.approved.get(digest).map(|r| r.origin_kind)
    }

    /// Marks the digest as approved and persists it. The read-modify-write runs
    /// under a file lock that re-reads the current store from disk - concurrent
    /// approvals from different processes merge instead of clobbering each
    /// other.
    pub fn approve(
        &mut self,
        digest: &str,
        id: &str,
        origin_kind: OriginKind,
    ) -> std::io::Result<()> {
        self.approve_kind(digest, id, Kind::Playbook, origin_kind)
    }

    /// Like `approve`, but with an explicit object kind (playbook or profile
    /// bundle).
    pub fn approve_kind(
        &mut self,
        digest: &str,
        id: &str,
        kind: Kind,
        origin_kind: OriginKind,
    ) -> std::io::Result<()> {
        let record = TrustRecord {
            id: id.to_string(),
            origin_kind,
            approved_at_ms: crate::clock::now_ms(),
            kind,
        };
        let digest = digest.to_string();
        self.locked_mutate(move |s| {
            s.approved.insert(digest, record);
        })
    }

    /// Every approval, ordered by kind, then id, then approval time.
    pub fn entries(&self) -> Vec<TrustEntry> {
        let mut out: Vec<TrustEntry> = self
            .approved
            .iter()
            .map(|(digest, r)| entry(digest, r))
            .collect();
        out.sort_by(|a, b| {
            (a.kind, &a.id, a.approved_at_ms).cmp(&(b.kind, &b.id, b.approved_at_ms))
        });
        out
    }

    /// Removes the approvals `selector` names and returns them: the one path
    /// behind `apb trust revoke`, MCP `trust_revoke` and the dashboard's
    /// Revoke. Revoking only ever lowers trust (the content then needs a new
    /// approval or an acknowledge to run). Selecting nothing is not an error;
    /// the result is empty. Under the same lock as every other write.
    pub fn revoke(&mut self, selector: &TrustSelector) -> std::io::Result<Vec<TrustEntry>> {
        let mut removed: Vec<TrustEntry> = Vec::new();
        self.locked_mutate(|s| {
            s.approved.retain(|digest, r| {
                let hit = selector.matches(digest, r);
                if hit {
                    removed.push(entry(digest, r));
                }
                !hit
            });
        })?;
        removed.sort_by_key(|e| e.approved_at_ms);
        Ok(removed)
    }

    /// Shared mutation path: under the config-directory lock, re-reads the
    /// current store, applies the change, persists the merged state, and
    /// syncs the in-memory `self`. Without a config directory - in-memory only
    /// (nowhere to store, and hence no races either).
    fn locked_mutate(&mut self, f: impl FnOnce(&mut TrustStore)) -> std::io::Result<()> {
        let Some(dir) = crate::config::config_dir() else {
            f(self);
            return Ok(());
        };
        let _lock = crate::fsutil::lock_dir(&dir, "trust.json.lock").ok();
        let mut latest = Self::load();
        f(&mut latest);
        latest.persist()?;
        *self = latest;
        Ok(())
    }

    fn persist(&self) -> std::io::Result<()> {
        let Some(path) = trust_path() else {
            // No-config environment: nowhere to store trust, silently skip.
            return Ok(());
        };
        let bytes = serde_json::to_vec_pretty(self).map_err(std::io::Error::other)?;
        atomic_write_private(&path, &bytes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct EnvGuard;
    impl Drop for EnvGuard {
        fn drop(&mut self) {
            unsafe {
                std::env::remove_var("APB_CONFIG_DIR");
            }
        }
    }

    #[test]
    fn lifecycle_defaults_to_active() {
        let tmp = tempfile::tempdir().unwrap();
        assert_eq!(read_lifecycle(tmp.path()), Lifecycle::Active);
        write_lifecycle(tmp.path(), Lifecycle::Draft).unwrap();
        assert_eq!(read_lifecycle(tmp.path()), Lifecycle::Draft);
    }

    #[test]
    fn approve_then_check_survives_reload() {
        let _lock = crate::env_test_lock();
        let cfg = tempfile::tempdir().unwrap();
        unsafe {
            std::env::set_var("APB_CONFIG_DIR", cfg.path());
        }
        let _g = EnvGuard;

        let mut store = TrustStore::load();
        assert!(!store.is_approved("sha256:aa"));
        store
            .approve("sha256:aa", "review", OriginKind::LocallyApproved)
            .unwrap();

        let reloaded = TrustStore::load();
        assert!(reloaded.is_approved("sha256:aa"));
        assert!(!reloaded.is_approved("sha256:bb"));
    }

    #[test]
    fn revoke_removes_approval() {
        let _lock = crate::env_test_lock();
        let cfg = tempfile::tempdir().unwrap();
        unsafe {
            std::env::set_var("APB_CONFIG_DIR", cfg.path());
        }
        let _g = EnvGuard;

        let mut store = TrustStore::load();
        store
            .approve("sha256:cc", "x", OriginKind::AgentGenerated)
            .unwrap();
        assert!(TrustStore::load().is_approved("sha256:cc"));
        let removed = store
            .revoke(&TrustSelector::parse("sha256:cc", None))
            .unwrap();
        assert_eq!(removed.len(), 1);
        assert!(!TrustStore::load().is_approved("sha256:cc"));
    }

    /// Revoking by id removes every approval under that id (of the given
    /// kind), leaves everything else, and reports exactly what it removed.
    #[test]
    fn revoke_by_id_removes_every_version_of_that_id_only() {
        let _lock = crate::env_test_lock();
        let cfg = tempfile::tempdir().unwrap();
        unsafe {
            std::env::set_var("APB_CONFIG_DIR", cfg.path());
        }
        let _g = EnvGuard;

        let mut store = TrustStore::load();
        for (digest, id, kind) in [
            ("sha256:v1", "demo", Kind::Playbook),
            ("sha256:v2", "demo", Kind::Playbook),
            ("sha256:p1", "demo", Kind::ProfileBundle),
            ("sha256:o1", "other", Kind::Playbook),
        ] {
            store
                .approve_kind(digest, id, kind, OriginKind::LocallyApproved)
                .unwrap();
        }

        let removed = store
            .revoke(&TrustSelector::parse("demo", Some(Kind::Playbook)))
            .unwrap();
        let digests: Vec<&str> = removed.iter().map(|e| e.digest.as_str()).collect();
        assert_eq!(digests, ["sha256:v1", "sha256:v2"]);
        let left: Vec<String> = TrustStore::load()
            .entries()
            .into_iter()
            .map(|e| e.digest)
            .collect();
        assert_eq!(left, ["sha256:o1", "sha256:p1"]);
        assert!(
            store
                .revoke(&TrustSelector::parse("nothing-here", None))
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn account_trust_id_formats_connector_and_account() {
        assert_eq!(
            account_trust_id("jira", "project-board"),
            "jira/project-board"
        );
    }

    #[test]
    fn connector_and_account_kinds_approve_and_survive_reload() {
        let _lock = crate::env_test_lock();
        let cfg = tempfile::tempdir().unwrap();
        unsafe {
            std::env::set_var("APB_CONFIG_DIR", cfg.path());
        }
        let _g = EnvGuard;

        let mut store = TrustStore::load();
        assert!(!store.is_approved("sha256:connector-ee"));
        assert!(!store.is_approved("sha256:account-ff"));

        store
            .approve_kind(
                "sha256:connector-ee",
                "jira",
                Kind::Connector,
                OriginKind::LocallyApproved,
            )
            .unwrap();
        let account_id = account_trust_id("jira", "project-board");
        store
            .approve_kind(
                "sha256:account-ff",
                &account_id,
                Kind::ConnectorAccount,
                OriginKind::LocallyApproved,
            )
            .unwrap();

        let reloaded = TrustStore::load();
        assert!(reloaded.is_approved("sha256:connector-ee"));
        assert!(reloaded.is_approved("sha256:account-ff"));

        // Serialization roundtrip keeps the kind (serde snake_case).
        let raw = std::fs::read_to_string(cfg.path().join("trust.json")).unwrap();
        assert!(raw.contains("\"connector\""));
        assert!(raw.contains("\"connector_account\""));
        let parsed: TrustStore = serde_json::from_str(&raw).unwrap();
        assert_eq!(
            parsed.approved.get("sha256:connector-ee").unwrap().kind,
            Kind::Connector
        );
        assert_eq!(
            parsed.approved.get("sha256:account-ff").unwrap().kind,
            Kind::ConnectorAccount
        );
        assert_eq!(
            parsed.approved.get("sha256:account-ff").unwrap().id,
            "jira/project-board"
        );
    }

    #[test]
    fn status_tells_approved_changed_and_unapproved_apart_per_kind() {
        let _lock = crate::env_test_lock();
        let cfg = tempfile::tempdir().unwrap();
        unsafe {
            std::env::set_var("APB_CONFIG_DIR", cfg.path());
        }
        let _g = EnvGuard;

        let mut store = TrustStore::load();
        store
            .approve_kind(
                "sha256:widget-old",
                "widget",
                Kind::Connector,
                OriginKind::LocallyApproved,
            )
            .unwrap();
        store
            .approve_kind(
                "sha256:acct",
                "gadget",
                Kind::ConnectorAccount,
                OriginKind::LocallyApproved,
            )
            .unwrap();

        let status = |d: &str, id: &str| store.status(d, id, Kind::Connector);
        assert_eq!(status("sha256:widget-old", "widget"), TrustStatus::Approved);
        assert_eq!(status("sha256:widget-new", "widget"), TrustStatus::Changed);
        // An approval of another kind under the same id is not "changed".
        assert_eq!(status("sha256:gadget", "gadget"), TrustStatus::Unapproved);
    }

    #[test]
    #[cfg(unix)]
    fn trust_file_is_private() {
        use std::os::unix::fs::PermissionsExt;
        let _lock = crate::env_test_lock();
        let cfg = tempfile::tempdir().unwrap();
        unsafe {
            std::env::set_var("APB_CONFIG_DIR", cfg.path());
        }
        let _g = EnvGuard;

        let mut store = TrustStore::load();
        store
            .approve("sha256:dd", "x", OriginKind::LocallyApproved)
            .unwrap();
        let mode = std::fs::metadata(cfg.path().join("trust.json"))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600);
    }
}
