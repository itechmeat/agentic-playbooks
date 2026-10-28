//! History discovery: the step sequences a person's agent sessions repeat,
//! found in the agents' own history files and offered as playbook drafts.
//!
//! Discovery runs only when a person asks for it, over the history roots
//! and the projects the request names. It reads no file outside them,
//! makes no model call and no network call, and keeps nothing of a
//! session but what a suggestion shows:
//!
//! - [`walk`]: the bounded, no-follow walk of a root and its files;
//! - [`session`]: one session file as argument-free steps and the requests
//!   a person typed;
//! - [`token`]: an [`crate::agent_actions::Action`] as a step token;
//! - [`mine`]: the repeated step sequences (contiguous n-grams);
//! - [`draft`]: a sequence as a suggestion with a playbook draft, scrubbed
//!   examples and the places it was seen;
//! - [`store`]: the signed local store of results.
//!
//! Entry point: [`discover`].

pub mod draft;
pub mod mine;
pub mod session;
pub mod store;
pub mod token;
mod types;
pub mod walk;

use std::path::Path;

use crate::scrub::Scrubber;

pub use types::*;

/// How long a discovery may hold the machine's discovery lock before
/// another one counts it as dead.
const LOCK_LIFETIME: std::time::Duration = std::time::Duration::from_secs(30 * 60);

fn check(req: &DiscoverRequest) -> Result<(), DiscoverError> {
    if req.roots.is_empty() {
        return Err(DiscoverError::Invalid("no history root to read".into()));
    }
    if req.projects.is_empty() {
        return Err(DiscoverError::Invalid(
            "no project to read sessions of".into(),
        ));
    }
    if !(1..=365).contains(&req.limits.since_days) {
        return Err(DiscoverError::Invalid(
            "since_days must be between 1 and 365".into(),
        ));
    }
    let m = &req.mine;
    if m.min_n < 2 || m.max_n < m.min_n || m.min_sessions == 0 || m.top == 0 {
        return Err(DiscoverError::Invalid(
            "the mining parameters do not fit".into(),
        ));
    }
    for root in &req.roots {
        walk::check_root(root)?;
    }
    Ok(())
}

/// Finds the repeated step sequences in the sessions of `req.projects`
/// under `req.roots`. One discovery runs at a time on the machine. The
/// result is not stored: [`store::save`] does that.
pub fn discover(req: &DiscoverRequest) -> Result<DiscoverResult, DiscoverError> {
    check(req)?;
    let _lock = match crate::config::config_dir() {
        Some(dir) => Some(
            crate::fsutil::lock_dir_with_lifetime(
                &dir.join(store::DIR),
                "discover.lock",
                LOCK_LIFETIME,
            )
            .map_err(|e| match e.kind() {
                std::io::ErrorKind::WouldBlock => DiscoverError::Busy,
                kind => DiscoverError::Lock(kind.to_string()),
            })?
            .0,
        ),
        None => None,
    };
    let now = crate::clock::now_ms_u64();
    let (roots, sessions) = walk::read_roots(req, now);
    let read = ReadReport {
        roots,
        sessions: sessions.len() as u32,
    };
    let patterns = mine::mine(&sessions, &req.mine);
    let ctx = draft::Context {
        sessions: &sessions,
        projects: &req.projects,
        roots: &req.roots,
        scrubbers: req
            .projects
            .iter()
            .map(|p| Scrubber::for_workspace(&p.root, Vec::new()))
            .collect(),
        profiles: req
            .projects
            .iter()
            .map(|p| draft::first_profile(&p.root.join(".apb")))
            .collect(),
    };
    let views: Vec<crate::dismiss::SuggestionView> = req
        .projects
        .iter()
        .map(|p| crate::dismiss::active(&p.root))
        .collect();
    let saved = store::saved();
    let mut suggestions = Vec::new();
    let mut suppressed = Vec::new();
    let mut dropped_drafts = 0;
    for p in &patterns {
        if suggestions.len() >= req.mine.top {
            break;
        }
        // A save or a decline counts in the projects the repeat was seen
        // in (and machine-wide), not in every project of the request.
        let seen_in = pattern_projects(p, &sessions);
        let seen: Vec<&ProjectRef> = seen_in.iter().map(|i| &req.projects[*i]).collect();
        if let Some(scope) = saved_scope(&saved, &seen, &p.slug) {
            suppressed.push(SuppressedSlug {
                slug: p.slug.clone(),
                scope: scope.to_string(),
                kind: "saved".to_string(),
            });
            continue;
        }
        // Each view holds the machine-wide declines too.
        let declined = seen_in
            .iter()
            .find_map(|i| crate::dismiss::suppressing(&views[*i], &p.slug));
        if let Some(rec) = declined {
            suppressed.push(SuppressedSlug {
                slug: p.slug.clone(),
                scope: rec.scope.as_str().to_string(),
                kind: rec.record.kind.as_str().to_string(),
            });
            continue;
        }
        match draft::suggestion(p, &ctx) {
            Some(s) => suggestions.push(s),
            None => dropped_drafts += 1,
        }
    }
    Ok(DiscoverResult {
        schema: RESULT_SCHEMA,
        id: new_id(),
        created_at: now,
        reader: READER.to_string(),
        miner: MINER.to_string(),
        request: RequestEcho {
            roots: req.roots.iter().map(|r| r.name.clone()).collect(),
            projects: req.projects.iter().map(ProjectRef::seen).collect(),
            since_days: req.limits.since_days,
        },
        read,
        suggestions,
        suppressed,
        dropped_drafts,
        mac: None,
    })
}

/// The projects (by index in the request) whose sessions `p` was seen in,
/// ascending.
fn pattern_projects(p: &mine::Pattern, sessions: &[session::Session]) -> Vec<usize> {
    let mut out: Vec<usize> = p.sessions.iter().map(|s| sessions[*s].project).collect();
    out.sort_unstable();
    out.dedup();
    out
}

/// Where suggestion `slug` was saved before for a repeat seen in `seen`,
/// so it is not offered again: a save the store recorded (`saved`, from
/// [`store::saved`]) for every project or for one of `seen`, whatever id
/// the playbook got; else a playbook named `slug` in one of `seen`
/// (`project`) or the global store (`global`).
fn saved_scope(
    saved: &[store::SavedSlug],
    seen: &[&ProjectRef],
    slug: &str,
) -> Option<&'static str> {
    let ids: Vec<&str> = seen.iter().map(|p| p.workspace_id.as_str()).collect();
    if let Some(s) = saved
        .iter()
        .find(|s| s.slug == slug && s.saved_in.covers(&ids))
    {
        return Some(s.saved_in.scope());
    }
    let exists = |parent: &Path| parent.join("playbooks").join(slug).is_dir();
    if seen.iter().any(|p| exists(&p.root.join(".apb"))) {
        return Some("project");
    }
    crate::store::global_playbooks_parent()
        .filter(|g| exists(g))
        .map(|_| "global")
}

/// A new result id: `disc-` and 10 hex characters.
fn new_id() -> String {
    let u = uuid::Uuid::new_v4().simple().to_string();
    format!("disc-{}", &u[..10])
}
