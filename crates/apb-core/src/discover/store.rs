//! The local store of discovery results: `<config_dir>/discover/<id>.json`,
//! written atomically and owner-only under the store's lock, the newest
//! [`KEEP`] kept.
//!
//! Each file is signed with this installation's key
//! ([`crate::run_origin::tag`], domain [`MAC_DOMAIN`]). A file that is not
//! signed here (copied, hand-made, edited) can be listed, but nothing is
//! ever saved from it and it is never marked.
//!
//! A person's save or decline of a suggestion goes through [`resolve`]:
//! under the store's lock, the suggestion is checked to be new, the action
//! runs once, and its status is recorded. A save is also recorded by slug
//! in [`SAVED`], whatever id or place the playbook got, so later
//! discoveries leave it out ([`saved_scope`]).

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::types::{DiscoverResult, Suggestion, SuggestionStatus};

/// The store's directory under the config directory.
pub const DIR: &str = "discover";
/// The purpose a result's MAC is made for.
pub const MAC_DOMAIN: &str = "apb-discover-v1";
/// Results kept.
pub const KEEP: usize = 10;

const LOCK: &str = "store.lock";
/// The suggestions saved as playbooks, by slug.
pub const SAVED: &str = "saved.json";
/// The largest saved-slug file read.
const MAX_SAVED_BYTES: u64 = 4 * 1024 * 1024;

/// Why a stored result could not be used.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StoreError {
    /// There is no config directory: nothing is stored.
    NoStore,
    /// No stored result has this id.
    NotFound(String),
    /// The result was not signed by this installation.
    Unsigned(String),
    /// The result has no suggestion with this slug.
    NoSuggestion(String),
    /// The suggestion was saved or declined already.
    Resolved(String, SuggestionStatus),
    Io(String),
}

impl StoreError {
    pub fn code(&self) -> &'static str {
        match self {
            StoreError::NoStore => "no_store",
            StoreError::NotFound(_) => "not_found",
            StoreError::Unsigned(_) => "unsigned",
            StoreError::NoSuggestion(_) => "no_suggestion",
            StoreError::Resolved(..) => "resolved",
            StoreError::Io(_) => "io",
        }
    }
}

impl std::fmt::Display for StoreError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            StoreError::NoStore => write!(f, "there is no config directory to store results in"),
            StoreError::NotFound(id) => write!(f, "no stored discovery `{id}`"),
            StoreError::Unsigned(id) => write!(
                f,
                "discovery `{id}` was not signed by this installation: nothing is saved from it"
            ),
            StoreError::NoSuggestion(slug) => write!(f, "the discovery has no suggestion `{slug}`"),
            StoreError::Resolved(slug, SuggestionStatus::Saved { playbook, .. }) => {
                write!(f, "suggestion `{slug}` was already saved as `{playbook}`")
            }
            StoreError::Resolved(slug, _) => write!(f, "suggestion `{slug}` was dismissed"),
            StoreError::Io(m) => write!(f, "{m}"),
        }
    }
}

impl std::error::Error for StoreError {}

fn io(e: std::io::Error) -> StoreError {
    StoreError::Io(e.to_string())
}

/// A stored result and whether this installation signed it as it is.
#[derive(Debug, Clone, PartialEq)]
pub struct Stored {
    pub result: DiscoverResult,
    pub signed: bool,
}

fn dir() -> Option<PathBuf> {
    crate::config::config_dir().map(|d| d.join(DIR))
}

/// Whether `id` is a result id: `disc-` and 10 lowercase hex characters.
pub fn is_result_id(id: &str) -> bool {
    id.strip_prefix("disc-").is_some_and(|h| {
        h.len() == 10
            && h.chars()
                .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase())
    })
}

/// The bytes a result's MAC covers: the result without its MAC.
fn signed_bytes(result: &DiscoverResult) -> Option<Vec<u8>> {
    serde_json::to_vec(&DiscoverResult {
        mac: None,
        ..result.clone()
    })
    .ok()
}

/// Whether this installation signed `result` as it is.
pub fn verify(result: &DiscoverResult) -> bool {
    match (result.mac.as_deref(), signed_bytes(result)) {
        (Some(mac), Some(bytes)) => crate::run_origin::verify_tag(MAC_DOMAIN, &bytes, mac),
        _ => false,
    }
}

/// Signs `result` and writes it, under the store's lock. Older results
/// past [`KEEP`] are removed. [`StoreError::NoStore`] without a config
/// directory (the result is then only returned).
pub fn save(result: &mut DiscoverResult) -> Result<(), StoreError> {
    let dir = dir().ok_or(StoreError::NoStore)?;
    let _lock = crate::fsutil::lock_dir(&dir, LOCK).map_err(io)?;
    write_signed(&dir, result)?;
    prune(&dir);
    Ok(())
}

fn write_signed(dir: &Path, result: &mut DiscoverResult) -> Result<(), StoreError> {
    if !is_result_id(&result.id) {
        return Err(StoreError::NotFound(result.id.clone()));
    }
    let bytes = signed_bytes(result)
        .ok_or_else(|| StoreError::Io("the result does not serialize".into()))?;
    result.mac = Some(crate::run_origin::tag(MAC_DOMAIN, &bytes).map_err(io)?);
    let json = serde_json::to_vec_pretty(result).map_err(|e| StoreError::Io(e.to_string()))?;
    crate::fsutil::atomic_write_private(&dir.join(format!("{}.json", result.id)), &json).map_err(io)
}

/// Every stored result that reads, newest first.
pub fn list() -> Vec<Stored> {
    let Some(dir) = dir() else {
        return Vec::new();
    };
    let mut out: Vec<Stored> = std::fs::read_dir(&dir)
        .into_iter()
        .flatten()
        .filter_map(Result::ok)
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().into_owned();
            let id = name.strip_suffix(".json")?;
            is_result_id(id).then(|| read(&dir, id).ok()).flatten()
        })
        .collect();
    out.sort_by(|a, b| {
        b.result
            .created_at
            .cmp(&a.result.created_at)
            .then_with(|| a.result.id.cmp(&b.result.id))
    });
    out
}

fn read(dir: &Path, id: &str) -> Result<Stored, StoreError> {
    let path = dir.join(format!("{id}.json"));
    let mut file = crate::fsutil::open_regular_no_follow(&path)
        .map_err(|_| StoreError::NotFound(id.to_string()))?;
    let (raw, _) = crate::fsutil::read_prefix_capped(&mut file, MAX_RESULT_BYTES).map_err(io)?;
    let result: DiscoverResult =
        serde_json::from_slice(&raw).map_err(|_| StoreError::NotFound(id.to_string()))?;
    if result.id != id {
        return Err(StoreError::NotFound(id.to_string()));
    }
    Ok(Stored {
        signed: verify(&result),
        result,
    })
}

/// The largest result file read.
const MAX_RESULT_BYTES: u64 = 16 * 1024 * 1024;

/// The stored result `id`, signed or not.
pub fn load(id: &str) -> Result<Stored, StoreError> {
    if !is_result_id(id) {
        return Err(StoreError::NotFound(id.to_string()));
    }
    read(&dir().ok_or(StoreError::NoStore)?, id)
}

/// The stored result `id` when this installation signed it: the only kind a
/// suggestion is saved from.
pub fn load_signed(id: &str) -> Result<DiscoverResult, StoreError> {
    let stored = load(id)?;
    if !stored.signed {
        return Err(StoreError::Unsigned(id.to_string()));
    }
    Ok(stored.result)
}

/// Records what became of suggestion `slug` of the signed result `id`, and
/// signs it again. Returns the updated result.
pub fn mark(id: &str, slug: &str, status: SuggestionStatus) -> Result<DiscoverResult, StoreError> {
    let dir = dir().ok_or(StoreError::NoStore)?;
    let _lock = crate::fsutil::lock_dir(&dir, LOCK).map_err(io)?;
    let mut result = load_signed(id)?;
    set_status(&mut result, slug, status)?;
    write_signed(&dir, &mut result)?;
    Ok(result)
}

fn set_status(
    result: &mut DiscoverResult,
    slug: &str,
    status: SuggestionStatus,
) -> Result<(), StoreError> {
    let s = result
        .suggestions
        .iter_mut()
        .find(|s| s.slug == slug)
        .ok_or_else(|| StoreError::NoSuggestion(slug.to_string()))?;
    s.status = status;
    Ok(())
}

/// What an action on a new suggestion came to.
pub enum Outcome<T> {
    /// It resolved the suggestion: this status is recorded, and for a save,
    /// where it was saved (`SavedIn`).
    Resolved(T, SuggestionStatus, Option<SavedIn>),
    /// It refused (a secret-like value, a duplicate): nothing is recorded.
    Refused(T),
}

/// Where a suggestion was saved: in one project (by workspace id), or for
/// every project.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "scope", content = "workspace_id", rename_all = "snake_case")]
pub enum SavedIn {
    /// A project's playbook; an empty id for a directory that is not a
    /// registered project (no later discovery can match it).
    Project(String),
    Global,
}

impl SavedIn {
    pub fn scope(&self) -> &'static str {
        match self {
            SavedIn::Project(_) => "project",
            SavedIn::Global => "global",
        }
    }

    /// Whether a save here covers a repeat seen in the projects
    /// `workspace_ids`: a global save covers every project, a project's
    /// save only that project.
    pub fn covers(&self, workspace_ids: &[&str]) -> bool {
        match self {
            SavedIn::Global => true,
            SavedIn::Project(id) => !id.is_empty() && workspace_ids.contains(&id.as_str()),
        }
    }
}

/// Runs `act` on suggestion `slug` of the signed result `id` while it is
/// new, under the store's lock, and records what it resolved to: the
/// status in the result, and a save by its slug and place in [`SAVED`].
/// The action runs at most once: a second caller waits for the lock and
/// finds the suggestion resolved ([`StoreError::Resolved`]). Returns the
/// action's value and, when it resolved but recording that failed, why
/// (the action stands: a playbook it wrote stays written).
pub fn resolve<T, E: From<StoreError>>(
    id: &str,
    slug: &str,
    act: impl FnOnce(&Suggestion) -> Result<Outcome<T>, E>,
) -> Result<(T, Option<StoreError>), E> {
    let dir = dir().ok_or(StoreError::NoStore)?;
    let _lock = crate::fsutil::lock_dir(&dir, LOCK).map_err(io)?;
    let mut result = load_signed(id)?;
    let suggestion = result
        .suggestions
        .iter()
        .find(|s| s.slug == slug)
        .ok_or_else(|| StoreError::NoSuggestion(slug.to_string()))?;
    if suggestion.status != SuggestionStatus::New {
        return Err(StoreError::Resolved(slug.to_string(), suggestion.status.clone()).into());
    }
    let (value, status, saved_in) = match act(suggestion)? {
        Outcome::Refused(v) => return Ok((v, None)),
        Outcome::Resolved(v, status, saved_in) => (v, status, saved_in),
    };
    let recorded = saved_in.map_or(Ok(()), |place| record_saved(&dir, slug, place));
    let marked =
        set_status(&mut result, slug, status).and_then(|()| write_signed(&dir, &mut result));
    Ok((value, recorded.err().or(marked.err())))
}

/// One suggestion saved as a playbook: its slug and where it went.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SavedSlug {
    pub slug: String,
    #[serde(flatten)]
    pub saved_in: SavedIn,
}

/// The recorded saves. A missing file is none; a file that does not read
/// whole is an error, so a writer never replaces saves it could not read.
fn read_saved(dir: &Path) -> Result<Vec<SavedSlug>, StoreError> {
    let mut file = match crate::fsutil::open_regular_no_follow(&dir.join(SAVED)) {
        Ok(f) => f,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(io(e)),
    };
    let (raw, more) = crate::fsutil::read_prefix_capped(&mut file, MAX_SAVED_BYTES).map_err(io)?;
    if more {
        return Err(StoreError::Io(
            "the saved-suggestion record is too large".into(),
        ));
    }
    serde_json::from_slice(&raw)
        .map_err(|_| StoreError::Io("the saved-suggestion record does not read".into()))
}

/// Records `slug` as saved in `place`; the caller holds the store's lock.
fn record_saved(dir: &Path, slug: &str, place: SavedIn) -> Result<(), StoreError> {
    let mut saved = read_saved(dir)?;
    let entry = SavedSlug {
        slug: slug.to_string(),
        saved_in: place,
    };
    if saved.contains(&entry) {
        return Ok(());
    }
    saved.push(entry);
    let json = serde_json::to_vec_pretty(&saved).map_err(|e| StoreError::Io(e.to_string()))?;
    crate::fsutil::atomic_write_private(&dir.join(SAVED), &json).map_err(io)
}

/// Every recorded save; none when the record does not read.
pub fn saved() -> Vec<SavedSlug> {
    dir().and_then(|d| read_saved(&d).ok()).unwrap_or_default()
}

/// Removes every stored result past the newest [`KEEP`].
fn prune(dir: &Path) {
    let mut found: Vec<(u64, String)> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .filter_map(Result::ok)
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().into_owned();
            let id = name.strip_suffix(".json")?.to_string();
            if !is_result_id(&id) {
                return None;
            }
            let at = read(dir, &id).map_or(0, |s| s.result.created_at);
            Some((at, id))
        })
        .collect();
    found.sort_by(|a, b| b.cmp(a));
    for (_, id) in found.into_iter().skip(KEEP) {
        let _ = std::fs::remove_file(dir.join(format!("{id}.json")));
    }
}
