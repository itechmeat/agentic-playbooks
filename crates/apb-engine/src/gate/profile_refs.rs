//! The profile references a playbook binds: a leaf the gate's trust scan
//! and its billing prediction both read.

use apb_core::schema::Playbook;

/// Collects every profile reference a playbook binds, accounting for defaults.
/// A reference comes from each node that has an effective profile - both
/// `agent_task` nodes and `finish` nodes that carry a `prompt` (a finish
/// prompt is an executor step, see `NodeKind::effective_profile_ref`) - and,
/// when `supervised`, the supervisor profile. The trust decision on these
/// bundles is made by the caller (`check_profile_bundles` / `collect_children`).
///
/// Does not account for the run-local ephemeral executor (`overrides`): the gate only sees
/// the playbook definition. This is safe only because the surfaces do NOT
/// combine `overrides` with the trust gate (`expected_profile_bundles`) - see
/// the invariant in `build_run_manifest`. Otherwise a node's profile key with an ephemeral
/// override would end up in the permit but not in the snapshot, producing a false key-set mismatch.
///
/// `supervised` - whether the run will spawn an external supervisor agent. The supervisor profile
/// (supervisor.profile OR defaults.profile, even without a section) enters the set
/// ONLY when `supervised: true` - the same rule as `build_run_manifest`,
/// otherwise the permit's key set would diverge from the snapshot.
pub fn collect_profile_refs(
    playbook: &Playbook,
    supervised: bool,
) -> Vec<apb_core::profile::QualifiedProfileRef> {
    let mut refs = Vec::new();
    for n in &playbook.nodes {
        if let Some(p) = n.kind.effective_profile_ref(&playbook.defaults) {
            refs.push(p);
        }
    }
    if supervised && let Some(p) = supervisor_profile_ref(playbook) {
        refs.push(p);
    }
    refs
}

/// The profile a supervised run's background supervisor binds:
/// `supervisor.profile`, else `defaults.profile`.
pub fn supervisor_profile_ref(
    playbook: &Playbook,
) -> Option<apb_core::profile::QualifiedProfileRef> {
    playbook
        .supervisor
        .as_ref()
        .and_then(|s| s.profile.clone())
        .or_else(|| playbook.defaults.profile.clone())
}
