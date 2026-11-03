//! The launch check of a resume, one rule for every resume path (`apb
//! resume` and `--all`, MCP `run_resume`, the dashboard, the pause waker and
//! the quota probe): a run that holds an authorization
//! ([`run_authorization`]) needs none; any other run (started by an older
//! apb) goes through the decision table of [`crate::gate::launch`] on its own
//! snapshot and child tree, and a run without pins gets the tree that check
//! verified. The grant is committed under the run's event lock right before
//! `run_resumed` ([`commit_resume_launch`]), after a recheck of the snapshot,
//! the pins and the policy state.
//!
//! It sits beside the gate rather than inside it: it reads the gate's tree
//! walk ([`crate::gate::resolve_tree`]) and the launch decision, while the
//! gate itself depends on neither.

use std::collections::BTreeMap;
use std::path::Path;

use apb_core::launch::{LaunchPolicy, LaunchRecord};
use apb_core::schema::{NodeKind, Playbook};
use apb_core::scope::Origin;
use serde_json::{Value, json};

use crate::gate::billing::BillingCheck;
use crate::gate::launch::{
    Choice, EngineResume, LaunchContext, LaunchGrant, POLICY_CHANGED, Subject, TREE_UNVERIFIED,
    Tree, decide, effective, permit_digest, policy_state, refusal_line, run_authorization,
};
use crate::run_config::ChildExpectation;

/// The resolved tree of a run being resumed: its snapshot, its origin and
/// the pins it runs (persisted, or freshly resolved for a run that has
/// none), with the digest a grant binds.
pub struct ResumeSubject {
    pub playbook: Playbook,
    pub origin: Origin,
    pub children: BTreeMap<String, ChildExpectation>,
    /// The run had no pins: the grant persists `children` before the run
    /// continues.
    pub pins_missing: bool,
    pub snapshot_digest: String,
}

impl ResumeSubject {
    /// The canonical digest of what the resume authorizes.
    pub fn permit_digest(&self) -> String {
        permit_digest(
            &self.snapshot_digest,
            &BTreeMap::new(),
            &BTreeMap::new(),
            &BTreeMap::new(),
            &self.children,
        )
    }
}

/// What a resume of run `run_id` would run: its snapshot and the child
/// tree, the persisted pins (each verified at its pinned digest) or, for a
/// run without pins, the tree resolved and verified now from the run's own
/// origin.
pub fn resume_subject(root: &Path, run_id: &str) -> Result<ResumeSubject, Value> {
    let run_dir = root.join(".apb/runs").join(run_id);
    let yaml = std::fs::read_to_string(run_dir.join("playbook.yaml"))
        .map_err(|e| json!({ "policy": "not_found", "detail": e.to_string() }))?;
    let playbook = crate::legacy_snapshot::parse_snapshot_playbook(&yaml)
        .map_err(|e| json!({ "policy": "snapshot_unreadable", "detail": e.to_string() }))?;
    let snapshot_digest = apb_core::scope::definition_digest(&yaml, &run_dir)
        .map_err(|e| json!({ "policy": "snapshot_unreadable", "detail": e.to_string() }))?;
    let origin = crate::run_origin::parent_run_origin(&run_dir);
    let pins = crate::run_config::read_run_config(&run_dir)
        .ok()
        .and_then(|c| c.expected_children);
    let has_children = playbook
        .nodes
        .iter()
        .any(|n| matches!(n.kind, NodeKind::Playbook { .. }));
    let (children, pins_missing) = match pins {
        Some(p) => (p, false),
        None if has_children => {
            let tree = crate::gate::resolve_tree(root, &playbook, &origin, &playbook.id, true).map_err(
                |mut v| {
                    v["detail"] = json!(format!(
                        "{TREE_UNVERIFIED}: the run has no pinned children and its child tree does not resolve now: {}",
                        v["detail"].as_str().unwrap_or_default()
                    ));
                    v
                },
            )?;
            (tree.children, true)
        }
        None => (BTreeMap::new(), false),
    };
    Ok(ResumeSubject {
        playbook,
        origin,
        children,
        pins_missing,
        snapshot_digest,
    })
}

/// The launch check of a resume (one rule for every resume path): an
/// authorized run needs none (`Ok(None)`); any other run goes through the
/// decision table on its own snapshot and child tree, and the grant
/// carries the tree a run without pins must persist.
pub fn check_resume_launch(
    root: &Path,
    run_id: &str,
    ctx: &LaunchContext,
) -> Result<Option<LaunchGrant>, Value> {
    if !apb_core::registry::is_safe_segment(run_id) {
        return Err(json!({ "policy": "not_found", "detail": format!("run `{run_id}`") }));
    }
    let run_dir = root.join(".apb/runs").join(run_id);
    if !run_dir.is_dir() {
        return Err(json!({ "policy": "not_found", "detail": format!("run `{run_id}`") }));
    }
    if let Some(auth) = run_authorization(&run_dir) {
        return billing_resume(root, &run_dir, run_id, auth, ctx);
    }
    let subject = resume_subject(root, run_id)?;
    let tolerant = !ctx.initiator.is_agent_channel();
    let (effective, _) = crate::gate::launch::effective_for(
        subject.tree(root),
        tolerant,
        &ctx.initiator,
        ctx.handed_config_dir(),
    )?;
    let billing = crate::gate::billing::check_tree(
        root,
        &subject.playbook,
        &subject.origin,
        &subject.children,
        false,
        ctx.initiator.checks_redirect(),
        ctx.handed_config_dir(),
    );
    let mut grant = decide(
        ctx,
        &subject.as_subject(root),
        &effective,
        &billing,
        tolerant,
    )?;
    if subject.pins_missing {
        grant.pins = Some(subject.children.clone());
    }
    Ok(Some(grant))
}

/// The launch check of a resume the engine makes on its own (the waker,
/// the quota probe): an authorized run passes (`Ok(None)`), any other run
/// goes through the decision table as the `engine` initiator, which only an
/// `allow` policy admits. A refusal is `Conflict` with its policy code.
pub fn check_engine_resume(
    root: &Path,
    run_id: &str,
    by: EngineResume,
) -> Result<Option<LaunchGrant>, crate::error::EngineError> {
    check_resume_launch(root, run_id, &LaunchContext::engine(by, run_id))
        .map_err(|v| crate::error::EngineError::Conflict(refusal_line(&v)))
}

/// The engine's commit of a resume's authorization, under the run's event
/// lock right before `run_resumed`: the record to journal, `None` for a run
/// already authorized (nothing to add) or one an unchecked resume may
/// continue (its policy is `allow`, and nothing is recorded). A grant is
/// rechecked against the run as it is now: the same run, the same snapshot
/// and child tree, the same policy state; a run without pins gets the
/// grant's tree persisted as its pins here, before the drive can spawn a
/// child.
pub fn commit_resume_launch(
    root: &Path,
    run_id: &str,
    grant: Option<&LaunchGrant>,
) -> Result<Option<LaunchRecord>, String> {
    settle_resume_launch(root, run_id, grant, true)
}

/// [`commit_resume_launch`] without writing anything: what a detached
/// resume checks before it hands the grant to its driver, so a refusal
/// reaches the caller.
pub fn verify_resume_launch(
    root: &Path,
    run_id: &str,
    grant: Option<&LaunchGrant>,
) -> Result<(), String> {
    settle_resume_launch(root, run_id, grant, false).map(|_| ())
}

fn settle_resume_launch(
    root: &Path,
    run_id: &str,
    grant: Option<&LaunchGrant>,
    persist: bool,
) -> Result<Option<LaunchRecord>, String> {
    let run_dir = root.join(".apb/runs").join(run_id);
    if run_authorization(&run_dir).is_some() {
        return settle_billing_resume(&run_dir, run_id, grant);
    }
    let text = |v: Value| refusal_line(&v);
    let mut subject = resume_subject(root, run_id).map_err(text)?;
    let Some(g) = grant else {
        // Strict: an unchecked resume never reads past a damaged settings
        // or floor file, which an agent can write.
        let (now, _) = effective(subject.tree(root), false).map_err(text)?;
        if now.policy == LaunchPolicy::Allow {
            return Ok(None);
        }
        return Err(format!(
            "launch_unchecked: run `{run_id}` was never authorized and playbook `{}` is `{}` for agents ({}); resume it through apb resume, MCP run_resume or the dashboard",
            subject.playbook.id,
            now.policy,
            now.sources.join(", ")
        ));
    };
    if g.run_id.as_deref() != Some(run_id) {
        return Err(format!(
            "{POLICY_CHANGED}: the launch grant is not for run `{run_id}`"
        ));
    }
    if let Some(pins) = &g.pins {
        subject.children = pins.clone();
    }
    if g.permit_digest.as_deref() != Some(subject.permit_digest().as_str()) {
        return Err(format!(
            "{POLICY_CHANGED}: run `{run_id}`'s snapshot or child tree changed since its launch was checked; resume it again"
        ));
    }
    let (now, _) = effective(subject.tree(root), g.tolerant).map_err(text)?;
    if g.policy_state.as_deref() != Some(policy_state(&now).as_str()) {
        return Err(format!(
            "{POLICY_CHANGED}: the launch policy of run `{run_id}` changed since its resume was checked (now `{}`: {}); resume it again",
            now.policy,
            now.sources.join(", ")
        ));
    }
    // The resume commits when it persists: a billing confirmation is spent
    // then, not by the check before it.
    if persist {
        g.consume_challenge()?;
    }
    if let Some(pins) = g.pins.as_ref().filter(|_| persist) {
        let mut cfg = crate::run_config::read_run_config(&run_dir).map_err(|e| e.to_string())?;
        if cfg.expected_children.is_none() {
            cfg.expected_children = Some(pins.clone());
            crate::run_config::write_run_config(&run_dir, &cfg).map_err(|e| e.to_string())?;
        }
    }
    Ok(Some(g.record.clone()))
}

/// The refusal of a plain resume of a run a billing pause holds.
pub const BILLING_PAUSED: &str = "billing_paused";

/// The resume of an authorized run: nothing to check (`Ok(None)`), unless
/// a billing pause holds it (`crate::pause::billing`). Then the agent and
/// basis it paused on need what a start would need: the settings allow it
/// now, or a person confirms it (the same table as a start, with a nonce
/// over the run, the pause and the entry); `deny` refuses. The grant
/// carries the run's record with the entry added, journaled on the resume.
fn billing_resume(
    root: &Path,
    run_dir: &Path,
    run_id: &str,
    auth: LaunchRecord,
    ctx: &LaunchContext,
) -> Result<Option<LaunchGrant>, Value> {
    let events = crate::event::read_all(run_dir).unwrap_or_default();
    let Some((entry, seq)) = crate::pause::billing::held_entry(&events) else {
        return Ok(None);
    };
    let check = BillingCheck {
        effective: crate::gate::billing::settings_for(
            root,
            ctx.initiator.checks_redirect(),
            ctx.handed_config_dir(),
        ),
        predicted: vec![entry.clone()],
    };
    let expected = apb_core::content::short_nonce(
        "billing-resume",
        &[
            root.display().to_string(),
            run_id.to_string(),
            seq.to_string(),
            entry.label(),
        ],
    );
    let refuse = |code: &str, nonce: Option<&str>| {
        let mut v = crate::gate::billing::refusal(code, &check, nonce);
        v["run_id"] = json!(run_id);
        v["launch_kind"] = json!(ctx.kind.label());
        v
    };
    let mut record = auth;
    let mut billing = record.billing.clone().unwrap_or_default();
    match check.required() {
        LaunchPolicy::Deny => return Err(refuse(crate::gate::billing::BILLING_DENIED, None)),
        LaunchPolicy::Allow => {
            if !billing.allowed.contains(&entry.basis) {
                billing.allowed.push(entry.basis);
            }
            billing
                .sources
                .extend(check.effective.sources.iter().cloned());
            billing.sources.sort();
            billing.sources.dedup();
        }
        LaunchPolicy::Ask => {
            // Bound to this run and this pause already: once the run
            // resumes, the pause no longer holds it and the nonce confirms
            // nothing again.
            let nonce_ok = |n: &str| apb_core::secret::ct_eq_str(n, &expected);
            let label =
                crate::gate::launch::billing_confirmation(ctx, &nonce_ok).map_err(|code| {
                    refuse(
                        code,
                        (code == crate::gate::billing::BILLING_ASK).then_some(expected.as_str()),
                    )
                })?;
            billing.confirmed.push(entry);
            billing.confirmation = Some(label.to_string());
        }
    }
    record.billing = Some(billing);
    Ok(Some(LaunchGrant {
        record,
        permit_digest: None,
        policy_state: None,
        tolerant: false,
        run_id: Some(run_id.to_string()),
        pins: None,
        nonce: Some(expected),
        version: None,
        choice: None,
        challenge: None,
    }))
}

/// [`settle_resume_launch`] for an authorized run: nothing to record,
/// unless a billing pause holds it; then only a grant for this run whose
/// record covers the paused entry lets it continue, and that record is
/// journaled.
fn settle_billing_resume(
    run_dir: &Path,
    run_id: &str,
    grant: Option<&LaunchGrant>,
) -> Result<Option<LaunchRecord>, String> {
    let events = crate::event::read_all(run_dir).map_err(|e| e.to_string())?;
    let Some((entry, _)) = crate::pause::billing::held_entry(&events) else {
        return Ok(None);
    };
    match grant {
        Some(g)
            if g.run_id.as_deref() == Some(run_id)
                && g.record
                    .billing
                    .as_ref()
                    .is_some_and(|b| b.covers(&entry.agent, entry.basis)) =>
        {
            Ok(Some(g.record.clone()))
        }
        _ => Err(format!(
            "{BILLING_PAUSED}: run `{run_id}` paused before agent work that needs a person's confirmation ({}); a person resumes it: `apb resume {run_id}` at a terminal or with --confirm-launch, MCP run_resume with confirm_launch after asking the person, or the dashboard",
            entry.label()
        )),
    }
}

impl ResumeSubject {
    fn tree<'a>(&'a self, root: &'a Path) -> Tree<'a> {
        Tree {
            root,
            playbook: &self.playbook,
            origin: &self.origin,
            children: &self.children,
            require_pins: true,
        }
    }

    fn as_subject<'a>(&'a self, root: &'a Path) -> Subject<'a> {
        Subject {
            root,
            id: &self.playbook.id,
            global: matches!(self.origin, Origin::Global),
            version: &self.playbook.version,
            choice: Choice::Explicit,
            definition_digest: &self.snapshot_digest,
            permit_digest: self.permit_digest(),
            children_digest: crate::gate::launch::children_digest(&self.children),
        }
    }
}
