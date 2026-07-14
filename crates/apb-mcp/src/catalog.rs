//! The structural playbook catalog (spec 4, tier 1). A read-only overview
//! of the current project's and global store's definitions with trust-aware shadowing
//! (spec 5.1), effective effects (spec 8.5), and diagnostics for broken
//! definitions. This tool is the source of truth for agent matching, not
//! free-form text in instructions.

use std::collections::BTreeMap;
use std::path::Path;

use apb_core::effects::effective;
use apb_core::registry::Registry;
use apb_core::schema::{Effect, Requires, Trigger};
use apb_core::scope::{Origin, PlaybookRef, digest_str};
use apb_core::store::global_playbooks_parent;
use apb_core::trust::{Lifecycle, TrustStore, read_lifecycle};
use serde::Serialize;
use serde_json::{Value, json};

/// A catalog entry: a qualified ref plus everything needed for matching and
/// the run policy.
#[derive(Debug, Clone, Serialize)]
pub struct CatalogEntry {
    #[serde(rename = "ref")]
    pub playbook_ref: PlaybookRef,
    pub name: String,
    pub lifecycle: String,
    pub trusted: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub trigger: Option<Trigger>,
    pub effective_effects: Vec<Effect>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub requires: Option<Requires>,
    /// A shadowed entry (spec 5.1): visible in the catalog, but lost the
    /// id-collision resolution.
    pub shadowed: bool,
    /// An ambiguous collision (spec 5.1): both the project and global definitions are
    /// active+approved - the agent must clarify which one to run (nothing is
    /// silently shadowed).
    pub ambiguous: bool,
    /// Content digest - not serialized into the response, needed for the revision.
    #[serde(skip)]
    pub digest: String,
}

fn lifecycle_str(lc: Lifecycle) -> &'static str {
    match lc {
        Lifecycle::Draft => "draft",
        Lifecycle::Active => "active",
        Lifecycle::Retired => "retired",
    }
}

/// Collects entries for one scope. Broken definitions do not crash the catalog -
/// they land in `diagnostics`. `origin` builds the ref (project origin leaves
/// workspace_id=None - "current workspace").
fn collect_scope(
    reg: &Registry,
    parent: &Path,
    origin_of: impl Fn(&str) -> Origin,
    trust: &TrustStore,
    entries: &mut Vec<CatalogEntry>,
    diagnostics: &mut Vec<Value>,
) {
    for id in reg.playbook_ids() {
        match reg.load(&id, None) {
            Ok(loaded) => {
                let digest = digest_str(&loaded.yaml);
                let playbook_dir = parent.join("playbooks").join(&id);
                let lifecycle = read_lifecycle(&playbook_dir);
                let effects: Vec<Effect> = effective(&loaded.playbook).into_iter().collect();
                entries.push(CatalogEntry {
                    playbook_ref: PlaybookRef {
                        origin: origin_of(&id),
                        id: id.clone(),
                        version: Some(loaded.version.clone()),
                    },
                    name: loaded.playbook.name.clone(),
                    lifecycle: lifecycle_str(lifecycle).to_string(),
                    trusted: trust.is_approved(&digest),
                    trigger: loaded.playbook.trigger.clone(),
                    effective_effects: effects,
                    requires: loaded.playbook.requires.clone(),
                    shadowed: false,
                    ambiguous: false,
                    digest,
                });
            }
            Err(e) => {
                diagnostics.push(json!({ "id": id, "error": e.to_string() }));
            }
        }
    }
}

fn is_active_approved(e: &CatalogEntry) -> bool {
    e.lifecycle == "active" && e.trusted
}

/// Trust-aware resolution of id collisions between the project and global scopes
/// (spec 5.1): the project entry wins only if it is active+approved; otherwise
/// it is the one marked shadowed, not the approved global one.
fn apply_shadowing(entries: &mut [CatalogEntry]) {
    // Indexes by id -> (project position, global position).
    let mut by_id: BTreeMap<String, (Option<usize>, Option<usize>)> = BTreeMap::new();
    for (i, e) in entries.iter().enumerate() {
        let slot = by_id.entry(e.playbook_ref.id.clone()).or_default();
        match e.playbook_ref.origin {
            Origin::Project { .. } => slot.0 = Some(i),
            Origin::Global => slot.1 = Some(i),
        }
    }
    for (_, (proj, glob)) in by_id {
        let (Some(pi), Some(gi)) = (proj, glob) else {
            continue;
        };
        let proj_ok = is_active_approved(&entries[pi]);
        let glob_ok = is_active_approved(&entries[gi]);
        match (proj_ok, glob_ok) {
            // Both trusted and active - ambiguity, nothing is silently
            // shadowed; the agent clarifies (spec 5.1).
            (true, true) => {
                entries[pi].ambiguous = true;
                entries[gi].ambiguous = true;
            }
            // The project entry wins only if it is active+approved.
            (true, false) => entries[gi].shadowed = true,
            // Otherwise the project entry (untrusted/draft) does not hide the global one.
            _ => entries[pi].shadowed = true,
        }
    }
}

/// A stable catalog revision: the digest of a canonical concatenation of all entries
/// PLUS the dismissed patterns and diagnostics. Including dismissed/diagnostics matters:
/// otherwise, after `suggestion_dismiss`, a client with the previous revision would get
/// `unchanged: true` and never see the new state.
fn compute_revision(
    entries: &[CatalogEntry],
    dismissed: &[String],
    diagnostics: &[Value],
) -> String {
    let mut lines: Vec<String> = entries
        .iter()
        .map(|e| {
            let scope = match e.playbook_ref.origin {
                Origin::Global => "global",
                Origin::Project { .. } => "project",
            };
            format!(
                "e|{scope}|{}|{}|{}|{}|{}|{}|{}",
                e.playbook_ref.id,
                e.playbook_ref.version.as_deref().unwrap_or(""),
                e.digest,
                e.lifecycle,
                e.trusted,
                e.shadowed,
                e.ambiguous
            )
        })
        .collect();
    for d in dismissed {
        lines.push(format!("d|{d}"));
    }
    for diag in diagnostics {
        lines.push(format!("x|{diag}"));
    }
    lines.sort();
    digest_str(&lines.join("\n"))
}

/// Builds the catalog for the project root `root` plus the global store.
/// `revision` - if it matches, returns `{ unchanged: true }`. `limit` -
/// an optional cap on the number of entries (after sorting and shadowing).
pub fn build(
    root: &Path,
    workspace_id: Option<&str>,
    revision: Option<&str>,
    limit: Option<usize>,
    dismissed_patterns: Vec<String>,
) -> Value {
    let trust = TrustStore::load();
    let mut entries: Vec<CatalogEntry> = Vec::new();
    let mut diagnostics: Vec<Value> = Vec::new();

    // Project scope. For a foreign workspace's catalog, stamp its workspace_id
    // so the refs are qualified and usable from the originating session; for
    // the current workspace - None.
    if let Ok(reg) = Registry::open(root) {
        let parent = root.join(".apb");
        let ws = workspace_id.map(|s| s.to_string());
        collect_scope(
            &reg,
            &parent,
            |_| Origin::Project {
                workspace_id: ws.clone(),
            },
            &trust,
            &mut entries,
            &mut diagnostics,
        );
    }

    if let Some(parent) = global_playbooks_parent()
        && let Ok(reg) = Registry::open_dir(&parent)
    {
        collect_scope(
            &reg,
            &parent,
            |_| Origin::Global,
            &trust,
            &mut entries,
            &mut diagnostics,
        );
    }

    // Stable order: project entries first, then global, sorted by id within each.
    entries.sort_by(|a, b| {
        let sa = matches!(a.playbook_ref.origin, Origin::Global);
        let sb = matches!(b.playbook_ref.origin, Origin::Global);
        sa.cmp(&sb).then(a.playbook_ref.id.cmp(&b.playbook_ref.id))
    });
    apply_shadowing(&mut entries);

    let catalog_revision = compute_revision(&entries, &dismissed_patterns, &diagnostics);
    // profiles_hint - a volatile counter outside the revision, so we return it in the
    // unchanged response too: otherwise a client with the previous revision would not learn that
    // the profile count changed (creating a profile intentionally does not move the revision).
    let profiles_count = count_profiles(root);
    if let Some(rev) = revision
        && rev == catalog_revision
    {
        return json!({
            "unchanged": true,
            "catalog_revision": catalog_revision,
            "profiles_hint": { "count": profiles_count },
        });
    }

    if let Some(n) = limit {
        entries.truncate(n);
    }

    // profiles_hint - a hint about the profile count, NOT part of the revision (otherwise
    // creating a profile would invalidate the playbook catalog without any change to it).
    json!({
        "catalog_revision": catalog_revision,
        "entries": entries,
        "diagnostics": diagnostics,
        "dismissed_patterns": dismissed_patterns,
        "profiles_hint": { "count": profiles_count },
    })
}

/// A cheap count of profiles (project + global) - the number of subdirectories with
/// a profile.yaml. Does not resolve or compute a digest.
fn count_profiles(root: &Path) -> usize {
    let mut dirs = vec![apb_core::profile_store::project_dir(root)];
    if let Some(g) = apb_core::profile_store::global_dir() {
        dirs.push(g);
    }
    let mut n = 0;
    for dir in dirs {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for e in entries.filter_map(Result::ok) {
            if e.path().join("profile.yaml").is_file() {
                n += 1;
            }
        }
    }
    n
}
