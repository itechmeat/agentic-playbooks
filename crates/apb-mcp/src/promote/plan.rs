//! What making a project playbook global would do, and what stops it: a
//! read-only plan over the current version, its profiles, sub-playbooks and
//! connectors, the global store and the other projects on this machine.

use std::collections::BTreeSet;
use std::path::Path;

use apb_core::profile::{ProfileScope, QualifiedProfileRef};
use apb_core::profile_store::{PlaybookOrigin, compute_bundle, resolve_profile};
use apb_core::registry::{Registry, is_safe_segment, list_versions};
use apb_core::schema::{NodeKind, Playbook};
use apb_core::validate::{Severity, ValidationContext, validate};
use serde::Serialize;

use super::MakeGlobalOpts;

/// One blocker, action or warning of a plan.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Item {
    pub code: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub subject: Option<String>,
    pub detail: String,
}

fn item(code: &'static str, subject: Option<&str>, detail: impl Into<String>) -> Item {
    Item {
        code,
        subject: subject.map(str::to_string),
        detail: detail.into(),
    }
}

/// The plan for one playbook. Nothing happens while `blockers` is not
/// empty.
#[derive(Debug, Clone, Default, Serialize)]
pub struct GlobalPlan {
    pub id: String,
    pub versions: Vec<String>,
    pub current: Option<String>,
    /// The current version's trust digest, which the global copy must
    /// match.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub digest: Option<String>,
    pub blockers: Vec<Item>,
    pub actions: Vec<Item>,
    pub warnings: Vec<Item>,
    /// The project profiles to copy to the global scope.
    #[serde(skip)]
    pub profiles_to_copy: Vec<String>,
}

/// Plans making project playbook `id` of the project at `root` global.
pub fn plan(root: &Path, id: &str, opts: &MakeGlobalOpts) -> GlobalPlan {
    let mut plan = GlobalPlan {
        id: id.to_string(),
        ..Default::default()
    };
    let project_dir = apb_core::versioning::playbooks_dir(root).join(id);
    if !is_safe_segment(id) || !project_dir.is_dir() {
        plan.blockers.push(item(
            "not_found",
            Some(id),
            "no project playbook with this id",
        ));
        return plan;
    }
    let loaded = match Registry::open(root).and_then(|reg| reg.load(id, None)) {
        Ok(l) => l,
        Err(e) => {
            plan.blockers
                .push(item("not_found", Some(id), format!("unloadable: {e}")));
            return plan;
        }
    };
    plan.versions = list_versions(&project_dir).unwrap_or_default();
    plan.current = Some(loaded.version.clone());
    match loaded.trust_digest() {
        Ok(d) => plan.digest = Some(d),
        Err(e) => plan.blockers.push(item(
            "scripts_undigestable",
            None,
            format!("the current version cannot be digested: {e}"),
        )),
    }
    match apb_core::store::global_playbooks_parent() {
        None => plan.blockers.push(item(
            "no_global_store",
            None,
            "this machine has no global config directory",
        )),
        Some(parent) if parent.join("playbooks").join(id).exists() => {
            plan.blockers.push(item(
                "global_id_taken",
                Some(id),
                "a global playbook with this id exists; rename one of them first",
            ));
        }
        Some(_) => {}
    }
    if let Some(v) = apb_core::candidate_pointer::read_candidate(&project_dir) {
        plan.blockers.push(item(
            "candidate_on_trial",
            Some(&v),
            "a candidate version is on trial; decide it first",
        ));
    }
    let ctx = ValidationContext {
        profiles: Vec::new(),
        playbook_origin: PlaybookOrigin::Global,
        connectors: apb_core::connector::resolve::validation_facts(),
    };
    for issue in validate(&loaded.playbook, &ctx).issues {
        if issue.severity == Severity::Error {
            plan.blockers.push(item(
                "invalid_as_global",
                issue.node.as_deref(),
                format!("{}: {}", issue.code, issue.message),
            ));
        }
    }
    check_profiles(root, &loaded.playbook, opts, &mut plan);
    check_children(root, &loaded.playbook, &mut plan);
    check_connectors(root, &loaded.playbook, &mut plan);
    check_elsewhere(root, id, &mut plan);
    if !opts.dry_run && opts.variant.is_none() {
        plan.blockers.push(item(
            "variant_fate_required",
            None,
            "choose what happens to the project copy: trash, retire or keep",
        ));
    }
    add_actions(opts, &mut plan);
    plan
}

/// The profiles: an `auto` reference that resolves only in the project
/// needs its profile copied (with `move_profiles`), and one the global
/// scope has in another form changes what runs.
fn check_profiles(root: &Path, playbook: &Playbook, opts: &MakeGlobalOpts, plan: &mut GlobalPlan) {
    let mut seen = BTreeSet::new();
    for r in apb_engine::gate::collect_profile_refs(playbook, true) {
        if r.scope != ProfileScope::Auto || !seen.insert(r.name.clone()) {
            continue;
        }
        let in_project = QualifiedProfileRef {
            name: r.name.clone(),
            scope: ProfileScope::Project,
        };
        let Ok(project) = resolve_profile(root, PlaybookOrigin::Project, &in_project) else {
            continue;
        };
        match resolve_profile(root, PlaybookOrigin::Global, &r) {
            Ok(global) => {
                // Without a resolvable bundle on either side, the profiles'
                // own digests still tell a difference.
                let differs = match (
                    what_runs(root, PlaybookOrigin::Project, &in_project),
                    what_runs(root, PlaybookOrigin::Global, &r),
                ) {
                    (Some(a), Some(b)) => a != b,
                    _ => global.profile_digest != project.profile_digest,
                };
                if differs {
                    plan.warnings.push(item(
                        "profile_shadow_change",
                        Some(&r.name),
                        "the global profile of this name, or a skill it uses, differs from the project one; the global playbook runs with the global profile",
                    ));
                }
            }
            Err(_) if !opts.move_profiles => plan.blockers.push(item(
                "profile_project_only",
                Some(&r.name),
                "the profile exists only in this project; pass move_profiles to copy it to the global scope",
            )),
            Err(_) => match crate::profile_tools::project_skill(&project.dir) {
                Ok(Some(skill)) => plan.blockers.push(item(
                    "profile_has_project_skills",
                    Some(&r.name),
                    format!("the profile uses project skill `{skill}`, which other projects lack"),
                )),
                Ok(None) => plan.profiles_to_copy.push(r.name.clone()),
                Err(e) => plan.blockers.push(item(
                    "profile_unreadable",
                    Some(&r.name),
                    e.to_string(),
                )),
            },
        }
    }
}

/// What a profile reference runs with: the profile's own digest and each
/// skill's content digest by skill name (not by scope, since the same
/// skill lives in another scope on each side). `None` when it does not
/// resolve with its skills.
fn what_runs(
    root: &Path,
    origin: PlaybookOrigin,
    r: &QualifiedProfileRef,
) -> Option<(String, Vec<(String, String)>)> {
    let (profile, pairs, _) = compute_bundle(root, origin, r).ok()?;
    let mut skills: Vec<(String, String)> = pairs
        .into_iter()
        .map(|(qualified, digest)| {
            let name = qualified.rsplit('/').next().unwrap_or_default().to_string();
            (name, digest)
        })
        .collect();
    skills.sort();
    Some((profile.profile_digest, skills))
}

/// The sub-playbooks: an `auto` child found only in this project would not
/// resolve from the global playbook in another project; an explicit
/// project child resolves per project, by design.
fn check_children(root: &Path, playbook: &Playbook, plan: &mut GlobalPlan) {
    let global = apb_core::store::global_playbooks_parent().map(|p| p.join("playbooks"));
    let mut seen = BTreeSet::new();
    for node in &playbook.nodes {
        let NodeKind::Playbook {
            playbook: child, ..
        } = &node.kind
        else {
            continue;
        };
        if !seen.insert(child.id.clone()) {
            continue;
        }
        match child.scope {
            ProfileScope::Project => plan.warnings.push(item(
                "sub_playbook_per_project",
                Some(&child.id),
                "a scope: project sub-playbook resolves in each project the playbook runs in",
            )),
            ProfileScope::Auto => {
                let in_global = global.as_ref().is_some_and(|g| g.join(&child.id).is_dir());
                let in_project = root.join(".apb/playbooks").join(&child.id).is_dir();
                if !in_global && in_project {
                    plan.blockers.push(item(
                        "sub_playbook_not_global",
                        Some(&child.id),
                        "the sub-playbook exists only in this project; make it global first",
                    ));
                }
            }
            ProfileScope::Global => {}
        }
    }
}

/// Connectors whose accounts this project configures: other projects have
/// none of them.
fn check_connectors(root: &Path, playbook: &Playbook, plan: &mut GlobalPlan) {
    let names: BTreeSet<&str> = playbook
        .nodes
        .iter()
        .flat_map(|n| n.kind.connector_bindings())
        .map(|b| b.name.as_str())
        .collect();
    for name in names {
        if apb_core::connector::config::project_config_path(root, name).is_file() {
            plan.warnings.push(item(
                "connector_accounts_project_only",
                Some(name),
                "this project configures accounts for the connector; other projects need their own or a global one",
            ));
        }
    }
}

/// Other projects on this machine with a project playbook of the same id:
/// reported, never changed.
fn check_elsewhere(root: &Path, id: &str, plan: &mut GlobalPlan) {
    let here = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
    for entry in apb_core::projects::list_reachable() {
        let path = Path::new(&entry.path);
        if path.canonicalize().unwrap_or_else(|_| path.to_path_buf()) == here {
            continue;
        }
        if path.join(".apb/playbooks").join(id).is_dir() {
            plan.warnings.push(item(
                "same_id_elsewhere",
                Some(&entry.name),
                format!(
                    "project {} has its own playbook with this id; there it keeps winning or becomes ambiguous",
                    entry.path
                ),
            ));
        }
    }
}

fn add_actions(opts: &MakeGlobalOpts, plan: &mut GlobalPlan) {
    for name in &plan.profiles_to_copy {
        plan.actions.push(item(
            "copy_profile",
            Some(name),
            "copy the project profile to the global scope",
        ));
    }
    plan.actions.push(item(
        "copy_playbook",
        Some(&plan.id),
        format!(
            "copy {} version(s) to the global store; digests and trust carry over",
            plan.versions.len()
        ),
    ));
    let (code, detail) = match opts.variant {
        Some(super::Variant::Keep) => (
            "copy_memory",
            "copy this project's remembered input to the global playbook; the kept project copy keeps its own",
        ),
        _ => (
            "move_memory",
            "move this project's remembered input to the global playbook",
        ),
    };
    plan.actions.push(item(code, None, detail));
    if let Some(v) = opts.variant {
        plan.actions
            .push(item("project_copy", Some(v.as_str()), v.detail()));
    }
}
