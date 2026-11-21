//! The MCP tool pin store, `<config-dir>/mcp-tools.json`: one record per
//! connector account, holding the binding a person approved at a terminal
//! (the connector tree, the account, the program and every mapped tool).
//! Machine-wide, like `trust.json`, but its own file: an older apb never
//! reads it, and `trust.json` never changes shape for it.
//!
//! What it guards against: a server or a program file that changes after a
//! person approved it, a caller choosing the program or its environment,
//! and an agent reaching pins through apb's own surfaces. It does not guard
//! against another process of the same user editing the file directly.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// The file name of the store in the config dir.
pub const PIN_STORE_FILE: &str = "mcp-tools.json";

/// The read cap of the store, 4 MiB; a record is refused when the whole
/// store would be longer, so every record that succeeds loads again.
pub const PIN_STORE_CAP: usize = 4 * 1024 * 1024;

/// What a surface outside the terminal says of a pin store that does not
/// read: fixed words, never the reader's diagnostic, which can quote a
/// stored value and names the local path. `apb connector doctor` prints
/// the diagnostic itself.
pub const PIN_STORE_UNREADABLE: &str = "the MCP tool pin store does not read; run `apb connector doctor` at a terminal for the details";

/// The lock file of the store, next to it.
const PIN_STORE_LOCK: &str = "mcp-tools.json.lock";

fn schema_one() -> u32 {
    1
}

/// `<config-dir>/mcp-tools.json`: one pin record per connector account,
/// holding the program and every mapped tool the person approved.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PinStore {
    #[serde(default = "schema_one")]
    pub schema_version: u32,
    /// Key `<connector>/<account>` (`server_key`).
    #[serde(default)]
    pub servers: BTreeMap<String, ServerPin>,
}

/// One approval: the binding of the connector tree, the program that
/// receives the account's secrets and the tools it may be asked to run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServerPin {
    /// The tree digest of the connector the person approved: it covers
    /// `mcp.command`, `mcp.args`, `mcp.env`, `mcp.protocol` and every other
    /// field of the definition.
    pub connector_digest: String,
    /// The digest of the merged account the person approved, so a pin made
    /// for one project's account never serves another project's account of
    /// the same name.
    pub account_digest: String,
    /// The definition's `command` as written when approved (for an http
    /// server, its `url` template). Display only.
    pub command: String,
    /// The absolute canonical path the approval resolved it to; for an
    /// http server, the rendered endpoint (no file is opened by it).
    pub program: String,
    /// `sha256:<hex>` of the program file's bytes at approval; for an http
    /// server, `digest::endpoint_digest` of the rendered endpoint and token
    /// URL, recomputed and compared on every call.
    pub program_digest: String,
    /// Tool name -> its pin.
    pub tools: BTreeMap<String, ToolPin>,
    /// `digest::pin_digest` of the whole binding.
    pub pin_digest: String,
    pub approved_at_ms: u128,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolPin {
    /// `tool_digest` of the approved definition, over the raw JSON.
    pub digest: String,
    /// The approved description after the account's redactions, at most
    /// 300 characters, for display only.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub description: String,
    /// Each top-level member of the approved definition (minus
    /// `UNPINNED_MEMBERS`) -> `sha256:<hex>` of its raw canonical JSON, so
    /// a changed tool can name the members that differ. Display only.
    #[serde(default)]
    pub members: BTreeMap<String, String>,
}

/// The key of a record: `<connector>/<account>`.
pub fn server_key(connector: &str, account: &str) -> String {
    format!("{connector}/{account}")
}

/// What a checker knows of the program file now.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProgramFact<'a> {
    /// Not read: the run gate hashes no file.
    NotRead,
    /// Missing, or not a regular file.
    Gone,
    /// `sha256:<hex>` of the file's bytes, streamed now.
    Digest(&'a str),
}

/// The live facts one check holds, compared with a record as one binding.
#[derive(Debug, Clone, Copy)]
pub struct LiveBinding<'a> {
    /// The tree digest the checker runs under.
    pub connector_digest: &'a str,
    /// The account digest the checker runs under.
    pub account_digest: &'a str,
    pub program: ProgramFact<'a>,
    /// The tool the function maps.
    pub tool: &'a str,
    /// `None` when no server was started (the gate): presence only.
    pub tool_digest: Option<&'a str>,
}

/// The first difference between a record and the live facts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BindingDiff {
    Tree { pinned: String },
    Account { pinned: String },
    ProgramGone,
    Program { pinned: String },
    ToolUnpinned,
    Tool { pinned: String },
}

#[derive(Debug, thiserror::Error)]
pub enum PinError {
    #[error(
        "the MCP tool pin store {0} does not parse: {1}; move it aside and approve the tools again"
    )]
    Unreadable(String, String),
    #[error(
        "mcp_pin_store_too_large: the MCP tool pin store would be {0} bytes, over its cap of {1}; nothing was pinned"
    )]
    TooLarge(usize, usize),
    #[error("the MCP tool pin store could not be written: {0}")]
    Write(String),
}

impl Default for PinStore {
    fn default() -> Self {
        PinStore {
            schema_version: schema_one(),
            servers: BTreeMap::new(),
        }
    }
}

impl PinStore {
    /// The store of this process's config dir. The config dir itself is
    /// resolved once with `std::fs::canonicalize` (a config dir that is a
    /// link is followed at the root only); the file is then read through
    /// `fsutil::beneath` (no link followed below the root, a regular file
    /// with one link only), at most `PIN_STORE_CAP` bytes. A missing file
    /// or config dir is an empty store; a refused read, a file over the cap
    /// or one that does not parse is an error, never an empty store.
    pub fn load() -> Result<PinStore, PinError> {
        match canonical_config_dir()? {
            Some(dir) => load_in(&dir),
            None => Ok(PinStore::default()),
        }
    }

    pub fn server(&self, connector: &str, account: &str) -> Option<&ServerPin> {
        self.servers.get(&server_key(connector, account))
    }

    /// Replaces the whole record of `connector`/`account` under the store's
    /// lock (a lock that cannot be taken is an error), re-reading the file
    /// first (a store that does not read stops the write), atomically and
    /// private, then syncs the config dir. The whole new store is refused
    /// `PinError::TooLarge` when longer than `PIN_STORE_CAP`, the file left
    /// as it was, so every record that succeeds loads again.
    pub fn record(connector: &str, account: &str, pin: ServerPin) -> Result<(), PinError> {
        mutate(|store| {
            store.servers.insert(server_key(connector, account), pin);
        })
    }

    /// Removes every record of `connector` (and `account` when given):
    /// called when the connector is uninstalled and when the approval of
    /// the connector or of the account is revoked. Writes nothing when no
    /// record matches.
    pub fn forget(connector: &str, account: Option<&str>) -> Result<(), PinError> {
        let prefix = server_key(connector, "");
        let hit = |key: &str| match account {
            Some(a) => key == server_key(connector, a),
            None => key.starts_with(&prefix),
        };
        if !PinStore::load()?.servers.keys().any(|k| hit(k)) {
            return Ok(());
        }
        mutate(|store| store.servers.retain(|key, _| !hit(key)))
    }

    /// Removes the records of `connector` (and `account` when given) bound
    /// to `digest`: their tree digest for a connector approval, their
    /// account digest for an account approval. Called when that one
    /// approval is revoked, so a record bound to another approval of the
    /// same name (a newer tree or account) stays. Writes nothing when no
    /// record matches.
    pub fn forget_bound(
        connector: &str,
        account: Option<&str>,
        digest: &str,
    ) -> Result<(), PinError> {
        let prefix = server_key(connector, "");
        let hit = |key: &str, pin: &ServerPin| match account {
            Some(a) => key == server_key(connector, a) && pin.account_digest == digest,
            None => key.starts_with(&prefix) && pin.connector_digest == digest,
        };
        if !PinStore::load()?.servers.iter().any(|(k, p)| hit(k, p)) {
            return Ok(());
        }
        mutate(|store| store.servers.retain(|key, pin| !hit(key, pin)))
    }
}

impl ServerPin {
    /// The one comparison of a pin binding, used by the call, the
    /// healthcheck, the run gate and the listing's row statuses: the tree
    /// digest first, then the account digest, then the program file
    /// (skipped for `NotRead`), then the tool (its presence, and its digest
    /// when one is given). The first difference answers; nothing else
    /// compares a pin.
    pub fn compare(&self, live: &LiveBinding<'_>) -> Result<(), BindingDiff> {
        if live.connector_digest != self.connector_digest {
            return Err(BindingDiff::Tree {
                pinned: self.connector_digest.clone(),
            });
        }
        if live.account_digest != self.account_digest {
            return Err(BindingDiff::Account {
                pinned: self.account_digest.clone(),
            });
        }
        match live.program {
            ProgramFact::NotRead => {}
            ProgramFact::Gone => return Err(BindingDiff::ProgramGone),
            ProgramFact::Digest(d) if d != self.program_digest => {
                return Err(BindingDiff::Program {
                    pinned: self.program_digest.clone(),
                });
            }
            ProgramFact::Digest(_) => {}
        }
        let Some(tool) = self.tools.get(live.tool) else {
            return Err(BindingDiff::ToolUnpinned);
        };
        match live.tool_digest {
            Some(d) if d != tool.digest => Err(BindingDiff::Tool {
                pinned: tool.digest.clone(),
            }),
            _ => Ok(()),
        }
    }
}

/// How the program a listing or an approval found stands against the
/// record, for display: `new` without a record, `same` when the record
/// pins this path and digest, else `changed`. Display only: a call's
/// decision goes through `ServerPin::compare`.
pub fn program_status(record: Option<&ServerPin>, program: &str, digest: &str) -> &'static str {
    match record {
        None => "new",
        Some(r) if r.program == program && r.program_digest == digest => "same",
        Some(_) => "changed",
    }
}

/// The canonical config dir, resolved once at its root: `None` when no
/// config dir is set or it does not exist yet.
fn canonical_config_dir() -> Result<Option<PathBuf>, PinError> {
    let Some(dir) = crate::config::config_dir() else {
        return Ok(None);
    };
    match std::fs::canonicalize(&dir) {
        Ok(canon) => Ok(Some(canon)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(PinError::Unreadable(
            dir.join(PIN_STORE_FILE).display().to_string(),
            e.to_string(),
        )),
    }
}

/// The store of the canonical config dir `dir`.
fn load_in(dir: &Path) -> Result<PinStore, PinError> {
    use crate::fsutil::beneath::{Beneath, CappedRead, ReadRefusal};
    let shown = dir.join(PIN_STORE_FILE).display().to_string();
    let unreadable = |why: String| PinError::Unreadable(shown.clone(), why);
    let anchor = match Beneath::open(dir) {
        Ok(a) => a,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(PinStore::default()),
        Err(e) => return Err(unreadable(e.to_string())),
    };
    let bytes = match anchor.read_regular_capped(PIN_STORE_FILE, PIN_STORE_CAP as u64) {
        Ok(CappedRead::Read { bytes, .. }) => bytes,
        Ok(CappedRead::Refused(ReadRefusal::Missing)) => return Ok(PinStore::default()),
        Ok(CappedRead::Refused(ReadRefusal::Link)) => {
            return Err(unreadable("it is a link, which is never followed".into()));
        }
        Ok(CappedRead::Refused(ReadRefusal::NotRegular)) => {
            return Err(unreadable("it is not a regular file".into()));
        }
        Ok(CappedRead::Refused(ReadRefusal::HardLink)) => {
            return Err(unreadable("it has more than one hard link".into()));
        }
        Ok(CappedRead::Refused(ReadRefusal::TooLarge { size })) => {
            return Err(unreadable(format!(
                "it is {size} bytes, over its cap of {PIN_STORE_CAP}"
            )));
        }
        Err(e) => return Err(unreadable(e.to_string())),
    };
    serde_json::from_slice(&bytes).map_err(|e| unreadable(e.to_string()))
}

/// One change of the store under its lock: re-read, change, refuse a store
/// over the cap, write atomically and private, sync the directory.
fn mutate(change: impl FnOnce(&mut PinStore)) -> Result<(), PinError> {
    let write = |e: std::io::Error| PinError::Write(e.to_string());
    let dir = crate::config::config_dir()
        .ok_or_else(|| PinError::Write("no config dir is set".to_string()))?;
    std::fs::create_dir_all(&dir).map_err(write)?;
    let dir = std::fs::canonicalize(&dir).map_err(write)?;
    let _lock = crate::fsutil::lock_dir(&dir, PIN_STORE_LOCK).map_err(write)?;
    let mut store = load_in(&dir)?;
    change(&mut store);
    let bytes = serde_json::to_vec_pretty(&store).map_err(|e| PinError::Write(e.to_string()))?;
    if bytes.len() > PIN_STORE_CAP {
        return Err(PinError::TooLarge(bytes.len(), PIN_STORE_CAP));
    }
    crate::fsutil::atomic_write_private(&dir.join(PIN_STORE_FILE), &bytes).map_err(write)?;
    crate::fsutil::sync_dir(&dir).map_err(write)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pin(tree: &str, tools: &[(&str, &str)]) -> ServerPin {
        let tools: BTreeMap<String, ToolPin> = tools
            .iter()
            .map(|(name, digest)| {
                (
                    name.to_string(),
                    ToolPin {
                        digest: digest.to_string(),
                        description: String::new(),
                        members: BTreeMap::new(),
                    },
                )
            })
            .collect();
        let digests = tools
            .iter()
            .map(|(k, v)| (k.clone(), v.digest.clone()))
            .collect();
        ServerPin {
            connector_digest: tree.to_string(),
            account_digest: "sha256:acct".to_string(),
            command: "srv".to_string(),
            program: "/usr/bin/srv".to_string(),
            program_digest: "sha256:prog".to_string(),
            pin_digest: super::super::digest::pin_digest(
                tree,
                "sha256:acct",
                "/usr/bin/srv",
                "sha256:prog",
                &digests,
            ),
            tools,
            approved_at_ms: 1,
        }
    }

    fn live<'a>(tool: &'a str, digest: Option<&'a str>) -> LiveBinding<'a> {
        LiveBinding {
            connector_digest: "sha256:tree",
            account_digest: "sha256:acct",
            program: ProgramFact::Digest("sha256:prog"),
            tool,
            tool_digest: digest,
        }
    }

    fn config_path() -> PathBuf {
        crate::config::config_dir().unwrap()
    }

    #[test]
    fn a_missing_store_is_empty() {
        let _cfg = crate::test_config();
        let store = PinStore::load().unwrap();
        assert!(store.servers.is_empty());
        assert_eq!(store.schema_version, 1);
        // A config dir that does not exist yet is empty too.
        unsafe { std::env::set_var("APB_CONFIG_DIR", config_path().join("not-made")) };
        assert!(PinStore::load().unwrap().servers.is_empty());
    }

    #[test]
    fn record_then_compare_is_equal() {
        let _cfg = crate::test_config();
        let p = pin("sha256:tree", &[("search", "sha256:t1")]);
        PinStore::record("tracker", "work", p.clone()).unwrap();
        let store = PinStore::load().unwrap();
        let got = store.server("tracker", "work").expect("the record");
        assert_eq!(got, &p);
        assert_eq!(got.compare(&live("search", Some("sha256:t1"))), Ok(()));
        assert!(store.server("tracker", "other").is_none());
    }

    #[test]
    fn compare_answers_the_first_difference() {
        let p = pin("sha256:tree", &[("search", "sha256:t1")]);
        let base = live("search", Some("sha256:t1"));
        let tree = LiveBinding {
            connector_digest: "sha256:tree2",
            ..base
        };
        assert_eq!(
            p.compare(&tree),
            Err(BindingDiff::Tree {
                pinned: "sha256:tree".into()
            })
        );
        let account = LiveBinding {
            account_digest: "sha256:acct2",
            ..base
        };
        assert_eq!(
            p.compare(&account),
            Err(BindingDiff::Account {
                pinned: "sha256:acct".into()
            })
        );
        let gone = LiveBinding {
            program: ProgramFact::Gone,
            ..base
        };
        assert_eq!(p.compare(&gone), Err(BindingDiff::ProgramGone));
        let program = LiveBinding {
            program: ProgramFact::Digest("sha256:prog2"),
            ..base
        };
        assert_eq!(
            p.compare(&program),
            Err(BindingDiff::Program {
                pinned: "sha256:prog".into()
            })
        );
        let unpinned = LiveBinding {
            tool: "delete",
            ..base
        };
        assert_eq!(p.compare(&unpinned), Err(BindingDiff::ToolUnpinned));
        let tool = LiveBinding {
            tool_digest: Some("sha256:t2"),
            ..base
        };
        assert_eq!(
            p.compare(&tool),
            Err(BindingDiff::Tool {
                pinned: "sha256:t1".into()
            })
        );
        // Two differences: the first in binding order answers.
        let two = LiveBinding {
            account_digest: "sha256:acct2",
            tool_digest: Some("sha256:t2"),
            ..base
        };
        assert!(matches!(p.compare(&two), Err(BindingDiff::Account { .. })));
        let tree_and_program = LiveBinding {
            connector_digest: "sha256:tree2",
            program: ProgramFact::Gone,
            ..base
        };
        assert!(matches!(
            p.compare(&tree_and_program),
            Err(BindingDiff::Tree { .. })
        ));
    }

    #[test]
    fn compare_without_a_tool_digest_is_presence_only() {
        let p = pin("sha256:tree", &[("search", "sha256:t1")]);
        assert_eq!(p.compare(&live("search", None)), Ok(()));
        assert_eq!(
            p.compare(&live("delete", None)),
            Err(BindingDiff::ToolUnpinned)
        );
    }

    #[test]
    fn compare_skips_a_program_not_read() {
        let p = pin("sha256:tree", &[("search", "sha256:t1")]);
        let not_read = LiveBinding {
            program: ProgramFact::NotRead,
            ..live("search", None)
        };
        assert_eq!(p.compare(&not_read), Ok(()));
    }

    #[test]
    fn a_record_under_another_tree_differs() {
        let _cfg = crate::test_config();
        PinStore::record(
            "tracker",
            "work",
            pin("sha256:old", &[("search", "sha256:t1")]),
        )
        .unwrap();
        let store = PinStore::load().unwrap();
        let got = store.server("tracker", "work").unwrap();
        assert_eq!(
            got.compare(&live("search", Some("sha256:t1"))),
            Err(BindingDiff::Tree {
                pinned: "sha256:old".into()
            })
        );
    }

    #[test]
    fn record_replaces_the_whole_record() {
        let _cfg = crate::test_config();
        let first = pin("sha256:tree", &[("a", "sha256:a"), ("b", "sha256:b")]);
        PinStore::record("tracker", "work", first).unwrap();
        let second = pin("sha256:tree", &[("c", "sha256:c")]);
        PinStore::record("tracker", "work", second.clone()).unwrap();
        let store = PinStore::load().unwrap();
        assert_eq!(store.server("tracker", "work"), Some(&second));
        assert_eq!(store.servers.len(), 1);
    }

    #[test]
    fn an_unparsable_store_is_an_error() {
        let _cfg = crate::test_config();
        let path = config_path().join(PIN_STORE_FILE);
        std::fs::write(&path, b"{ not json").unwrap();
        let err = PinStore::load().unwrap_err();
        assert!(matches!(err, PinError::Unreadable(..)), "{err}");
        assert!(err.to_string().contains(PIN_STORE_FILE), "{err}");
        // A record does not reset it either.
        let rec = PinStore::record("tracker", "work", pin("sha256:tree", &[]));
        assert!(rec.is_err());
        assert_eq!(std::fs::read(&path).unwrap(), b"{ not json");
    }

    #[cfg(unix)]
    #[test]
    fn a_linked_store_is_refused() {
        let _cfg = crate::test_config();
        let elsewhere = tempfile::tempdir().unwrap();
        let real = elsewhere.path().join("pins.json");
        std::fs::write(&real, br#"{"schema_version":1,"servers":{}}"#).unwrap();
        std::os::unix::fs::symlink(&real, config_path().join(PIN_STORE_FILE)).unwrap();
        let err = PinStore::load().unwrap_err();
        assert!(matches!(err, PinError::Unreadable(..)), "{err}");
        assert!(PinStore::record("tracker", "work", pin("sha256:tree", &[])).is_err());
        assert_eq!(
            std::fs::read(&real).unwrap(),
            br#"{"schema_version":1,"servers":{}}"#
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_linked_config_dir_is_followed_once() {
        let _cfg = crate::test_config();
        let base = tempfile::tempdir().unwrap();
        let real = base.path().join("real");
        std::fs::create_dir(&real).unwrap();
        let link = base.path().join("link");
        std::os::unix::fs::symlink(&real, &link).unwrap();
        unsafe { std::env::set_var("APB_CONFIG_DIR", &link) };
        let p = pin("sha256:tree", &[("search", "sha256:t1")]);
        PinStore::record("tracker", "work", p.clone()).unwrap();
        assert!(real.join(PIN_STORE_FILE).is_file());
        assert_eq!(
            PinStore::load().unwrap().server("tracker", "work"),
            Some(&p)
        );
    }

    #[test]
    fn record_merges_with_a_concurrent_writer() {
        let _cfg = crate::test_config();
        let threads: Vec<_> = ["one", "two"]
            .into_iter()
            .map(|account| {
                std::thread::spawn(move || {
                    for i in 0..5 {
                        let tool = format!("t{i}");
                        PinStore::record(
                            "tracker",
                            account,
                            pin("sha256:tree", &[(tool.as_str(), "sha256:x")]),
                        )
                        .unwrap();
                    }
                })
            })
            .collect();
        for t in threads {
            t.join().unwrap();
        }
        let store = PinStore::load().unwrap();
        assert!(store.server("tracker", "one").is_some());
        assert!(store.server("tracker", "two").is_some());
    }

    #[cfg(unix)]
    #[test]
    fn the_store_file_is_private() {
        use std::os::unix::fs::PermissionsExt;
        let _cfg = crate::test_config();
        PinStore::record("tracker", "work", pin("sha256:tree", &[])).unwrap();
        let mode = std::fs::metadata(config_path().join(PIN_STORE_FILE))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600);
    }

    #[test]
    fn the_trust_store_is_untouched() {
        let _cfg = crate::test_config();
        PinStore::record("tracker", "work", pin("sha256:tree", &[])).unwrap();
        assert!(config_path().join(PIN_STORE_FILE).is_file());
        assert!(!config_path().join("trust.json").exists());
    }

    /// A pin of `n` tools, each near the listing's display limits.
    fn big_pin(n: usize) -> ServerPin {
        let mut p = pin("sha256:tree", &[]);
        for i in 0..n {
            p.tools.insert(
                format!("tool_{i:04}_{}", "x".repeat(100)),
                ToolPin {
                    digest: format!("sha256:{}", "a".repeat(64)),
                    description: "d".repeat(300),
                    members: ["annotations", "description", "inputSchema", "name", "title"]
                        .into_iter()
                        .map(|m| (m.to_string(), format!("sha256:{}", "b".repeat(64))))
                        .collect(),
                },
            );
        }
        p
    }

    #[test]
    fn a_record_over_the_cap_is_refused_and_leaves_the_file() {
        let _cfg = crate::test_config();
        PinStore::record("tracker", "work", pin("sha256:tree", &[("a", "sha256:a")])).unwrap();
        let path = config_path().join(PIN_STORE_FILE);
        let before = std::fs::read(&path).unwrap();
        let err = PinStore::record("tracker", "huge", big_pin(5000)).unwrap_err();
        assert!(matches!(err, PinError::TooLarge(_, PIN_STORE_CAP)), "{err}");
        assert!(
            err.to_string().starts_with("mcp_pin_store_too_large:"),
            "{err}"
        );
        assert_eq!(std::fs::read(&path).unwrap(), before);
    }

    #[test]
    fn every_successful_record_loads_again() {
        let _cfg = crate::test_config();
        let listing = big_pin(500);
        let mut stored = 0;
        let mut refused = false;
        for i in 0..64 {
            match PinStore::record("tracker", &format!("acct-{i}"), listing.clone()) {
                Ok(()) => {
                    stored += 1;
                    let store = PinStore::load().expect("a stored record loads");
                    assert_eq!(store.servers.len(), stored);
                }
                Err(PinError::TooLarge(..)) => {
                    refused = true;
                    break;
                }
                Err(e) => panic!("{e}"),
            }
        }
        assert!(stored > 0 && refused, "stored {stored}, refused {refused}");
        assert_eq!(PinStore::load().unwrap().servers.len(), stored);
    }

    #[test]
    fn forget_removes_the_records_of_a_connector() {
        let _cfg = crate::test_config();
        PinStore::record("tracker", "a", pin("sha256:tree", &[])).unwrap();
        PinStore::record("tracker", "b", pin("sha256:tree", &[])).unwrap();
        PinStore::record("other", "a", pin("sha256:tree", &[])).unwrap();
        PinStore::forget("tracker", Some("a")).unwrap();
        let store = PinStore::load().unwrap();
        assert!(store.server("tracker", "a").is_none());
        assert!(store.server("tracker", "b").is_some());
        PinStore::forget("tracker", None).unwrap();
        let store = PinStore::load().unwrap();
        assert!(store.server("tracker", "b").is_none());
        assert!(store.server("other", "a").is_some());
    }

    #[test]
    fn a_revoked_approval_takes_its_pins_with_it() {
        use crate::trust::{Kind, OriginKind, TrustSelector, TrustStore, account_trust_id};
        let _cfg = crate::test_config();
        let with_account = |tree: &str, account: &str| ServerPin {
            account_digest: account.to_string(),
            ..pin(tree, &[])
        };
        PinStore::record("tracker", "a", with_account("sha256:tree", "sha256:acct-a")).unwrap();
        PinStore::record("tracker", "b", with_account("sha256:tree", "sha256:acct-b")).unwrap();
        PinStore::record("other", "a", pin("sha256:tree-other", &[])).unwrap();
        PinStore::record("other", "b", pin("sha256:tree-newer", &[])).unwrap();
        let mut trust = TrustStore::load();
        trust
            .approve_kind(
                "sha256:acct-a",
                &account_trust_id("tracker", "a"),
                Kind::ConnectorAccount,
                OriginKind::LocallyApproved,
            )
            .unwrap();
        trust
            .approve_kind(
                "sha256:tree-other",
                "other",
                Kind::Connector,
                OriginKind::LocallyApproved,
            )
            .unwrap();
        trust
            .revoke(&TrustSelector::Digest("sha256:acct-a".into()))
            .unwrap();
        let store = PinStore::load().unwrap();
        assert!(store.server("tracker", "a").is_none(), "the account's pins");
        assert!(
            store.server("tracker", "b").is_some(),
            "another account's stay"
        );
        trust
            .revoke(&TrustSelector::Digest("sha256:tree-other".into()))
            .unwrap();
        let store = PinStore::load().unwrap();
        assert!(store.server("other", "a").is_none());
        // A record bound to a newer approval of the same connector stays:
        // revoking an older tree digest never drops the current pins.
        assert!(
            store.server("other", "b").is_some(),
            "bound to another tree"
        );
    }

    #[test]
    fn forgetting_nothing_writes_nothing() {
        let cfg = crate::test_config();
        PinStore::forget("tracker", None).unwrap();
        let dir = crate::config::config_dir().unwrap();
        assert!(!dir.join(PIN_STORE_FILE).exists());
        drop(cfg);
    }
}
