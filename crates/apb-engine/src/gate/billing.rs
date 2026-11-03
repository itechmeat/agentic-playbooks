//! The billing part of the launch check: before a run starts, the basis of
//! every agent its profiles bind (primary, fallbacks and tiers, over the
//! whole tree) is predicted from the facts a spawn would see
//! ([`crate::adapter::cost_reading`]), and the `billing:` settings
//! ([`apb_core::billing`]) decide whether that work may start.
//!
//! - `allow` lets it start;
//! - `ask` refuses it with the launch nonce, which then also covers the exact
//!   predicted set ([`BillingCheck::nonce_lines`]): a person confirms at a
//!   terminal or passes the nonce back (the CLI flag, the host relaying the
//!   person's answer, the dashboard's confirmation); a run step and the
//!   engine never confirm it;
//! - `deny` refuses it for everyone; the remedy is a profile on a
//!   subscription login or a changed setting.
//!
//! What passed is recorded in the run's launch record ([`BillingRecord`])
//! and every spawn is checked against it (`crate::scheduler::billing`).

use std::collections::BTreeMap;
use std::path::Path;

use apb_core::agents::CostBasis;
use apb_core::billing::{BillingEntry, BillingRecord, EffectiveBilling};
use apb_core::launch::LaunchPolicy;
use apb_core::profile::{AgentEnvironment, ProfileDoc};
use apb_core::profile_store::{self, PlaybookOrigin};
use apb_core::schema::{NodeKind, Playbook};
use apb_core::scope::Origin;
use serde_json::{Value, json};

use crate::run_config::ChildExpectation;

/// Refusal codes.
pub const BILLING_ASK: &str = "billing_requires_confirmation";
pub const BILLING_DENIED: &str = "billing_denied";
pub const BILLING_STEP: &str = "billing_refused_in_run_step";
pub const BILLING_ENGINE: &str = "billing_refused_for_engine";

/// Whether `policy` is one of the billing refusal codes.
pub fn is_billing_refusal(policy: &str) -> bool {
    matches!(
        policy,
        BILLING_ASK | BILLING_DENIED | BILLING_STEP | BILLING_ENGINE
    )
}

/// The settings a launch is decided under and the predicted bases.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BillingCheck {
    pub effective: EffectiveBilling,
    /// One entry per agent and basis, sorted by label.
    pub predicted: Vec<BillingEntry>,
}

impl BillingCheck {
    /// A launch that binds no agent: nothing to hold.
    pub fn nothing(effective: EffectiveBilling) -> Self {
        BillingCheck {
            effective,
            predicted: Vec::new(),
        }
    }

    /// The strictest policy any predicted entry falls under.
    pub fn required(&self) -> LaunchPolicy {
        self.predicted
            .iter()
            .map(|e| self.effective.policy_for(e.basis))
            .max()
            .unwrap_or_default()
    }

    /// The predicted entries under `policy`.
    pub fn under(&self, policy: LaunchPolicy) -> Vec<&BillingEntry> {
        self.predicted
            .iter()
            .filter(|e| self.effective.policy_for(e.basis) == policy)
            .collect()
    }

    /// What the launch nonce covers on top of the launch: the predicted set
    /// and the settings, only when a person must confirm it (so a launch
    /// that needs no billing confirmation keeps the nonce it always had).
    pub fn nonce_lines(&self) -> Vec<String> {
        if self.required() != LaunchPolicy::Ask {
            return Vec::new();
        }
        let mut lines = vec![format!(
            "billing {} {}",
            self.effective.billed, self.effective.unknown_basis
        )];
        lines.extend(self.predicted.iter().map(BillingEntry::label));
        lines
    }

    /// The record a launch that passed keeps: the bases the settings
    /// allowed, and the entries that needed a confirmation when one was
    /// given (`confirmation`).
    pub fn record(&self, confirmation: Option<String>) -> BillingRecord {
        let confirmed = match &confirmation {
            Some(_) => self.under(LaunchPolicy::Ask).into_iter().cloned().collect(),
            None => Vec::new(),
        };
        BillingRecord {
            allowed: self.effective.allowed(),
            confirmed,
            confirmation,
            sources: self.effective.sources.clone(),
        }
    }

    /// Whether `record` covers every predicted entry.
    pub fn covered_by(&self, record: &BillingRecord) -> bool {
        self.predicted
            .iter()
            .all(|e| record.covers(&e.agent, e.basis))
    }

    /// The `billing` block of a refusal.
    pub fn block(&self) -> Value {
        json!({
            "billed": self.effective.billed,
            "unknown_basis": self.effective.unknown_basis,
            "sources": self.effective.sources,
            "predicted": self.predicted,
            "held": self
                .predicted
                .iter()
                .filter(|e| self.effective.policy_for(e.basis) != LaunchPolicy::Allow)
                .map(BillingEntry::label)
                .collect::<Vec<_>>(),
        })
    }

    /// The held entries as text: `codex: billed, my-agent: unknown`.
    pub fn held_text(&self) -> String {
        self.predicted
            .iter()
            .filter(|e| self.effective.policy_for(e.basis) != LaunchPolicy::Allow)
            .map(BillingEntry::label)
            .collect::<Vec<_>>()
            .join(", ")
    }
}

/// The billing refusal `code` for `check`, with the launch fields a retry
/// needs (`launch_nonce` when a confirmation can be given).
pub(crate) fn refusal(code: &str, check: &BillingCheck, nonce: Option<&str>) -> Value {
    let held = check.held_text();
    let sources = check.effective.sources.join(", ");
    let detail = match code {
        BILLING_DENIED => format!(
            "this run would start agent work that is billed or of unknown basis ({held}), and the billing settings deny it ({sources}); bind a profile whose agent runs on a subscription login, or change `billing:` in the machine's policy.yaml"
        ),
        BILLING_STEP => format!(
            "this run would start agent work that is billed or of unknown basis ({held}), and a run step cannot confirm it ({sources}); a person starts it from a terminal or the dashboard"
        ),
        BILLING_ENGINE => format!(
            "the engine does not start agent work that is billed or of unknown basis ({held}) without a person's confirmation ({sources}); a person resumes the run"
        ),
        _ => format!(
            "this run would start agent work that is billed or of unknown basis ({held}; {sources}); ask the person whether to start it, then call again with confirm_launch set to the launch_nonce and the same launch_version and launch_choice (CLI: --confirm-launch)"
        ),
    };
    let mut v = json!({
        "policy": code,
        "detail": detail,
        "billing": check.block(),
    });
    match nonce {
        Some(n) => v["launch_nonce"] = json!(n),
        None => v["launch_by_person"] = json!(code != BILLING_DENIED),
    }
    v
}

/// The settings a launch is decided under, read at the workspace `root`
/// ([`EffectiveBilling::load`]: a file that does not read denies). A
/// `checked` caller (a run step or a CLI without a terminal,
/// `Initiator::checks_redirect`) whose config dir was redirected away from
/// the default one also reads the default one's, and the stricter of each
/// setting holds (an eval case keeps the config dir its `apb eval` handed).
pub fn settings_for(
    root: &Path,
    checked: bool,
    handed_config_dir: Option<&Path>,
) -> EffectiveBilling {
    let here_dir = apb_core::config::config_dir();
    let redirected = checked
        && apb_core::config::config_dir_redirected()
        && !(handed_config_dir.is_some() && handed_config_dir == here_dir.as_deref());
    let default_dir = redirected
        .then(apb_core::config::default_config_dir)
        .flatten();
    settings_in(root, here_dir.as_deref(), default_dir.as_deref())
}

/// The settings at `root` under the config dir `here` (`None`: none), held
/// to the default config dir `default` too when it is given: the stricter
/// of each setting, the sources of both named.
fn settings_in(root: &Path, here: Option<&Path>, default: Option<&Path>) -> EffectiveBilling {
    let read = |dir: Option<&Path>| {
        let machine = dir.map(|d| d.join(apb_core::policy_file::POLICY_FILE));
        EffectiveBilling::load(Some(root), machine.as_deref())
    };
    let here = read(here);
    let Some(default) = default else {
        return here;
    };
    let default = read(Some(default));
    let mut sources = here.sources.clone();
    sources.extend(
        default
            .sources
            .iter()
            .map(|s| format!("default config dir {s}")),
    );
    sources.sort();
    sources.dedup();
    EffectiveBilling {
        billed: here.billed.max(default.billed),
        unknown_basis: here.unknown_basis.max(default.unknown_basis),
        budget_counts_subscription: here.budget_counts_subscription
            || default.budget_counts_subscription,
        sources,
    }
}

/// The check of a run over the tree at `root`: the settings ([`settings_for`],
/// `checked` and `handed_config_dir` as there) and the predicted basis of
/// every executor the tree's profiles bind.
pub fn check_tree(
    root: &Path,
    playbook: &Playbook,
    origin: &Origin,
    children: &BTreeMap<String, ChildExpectation>,
    supervised: bool,
    checked: bool,
    handed_config_dir: Option<&Path>,
) -> BillingCheck {
    let effective = settings_for(root, checked, handed_config_dir);
    let mut predicted = Vec::new();
    predict_playbook(
        root,
        playbook,
        origin_of(origin),
        supervised,
        &mut predicted,
    );
    predict_children(root, playbook, children, &mut predicted);
    predicted.sort_by_key(BillingEntry::label);
    predicted.dedup();
    BillingCheck {
        effective,
        predicted,
    }
}

fn origin_of(origin: &Origin) -> PlaybookOrigin {
    match origin {
        Origin::Global => PlaybookOrigin::Global,
        _ => PlaybookOrigin::Project,
    }
}

fn predict_children(
    root: &Path,
    playbook: &Playbook,
    pins: &BTreeMap<String, ChildExpectation>,
    out: &mut Vec<BillingEntry>,
) {
    for n in &playbook.nodes {
        if !matches!(n.kind, NodeKind::Playbook { .. }) {
            continue;
        }
        let Some(pin) = pins.get(&n.id) else { continue };
        let Some(child) = pin.load(root) else {
            continue;
        };
        let origin = match pin.scope {
            apb_core::profile::ProfileScope::Global => PlaybookOrigin::Global,
            _ => PlaybookOrigin::Project,
        };
        predict_playbook(root, &child, origin, false, out);
        predict_children(root, &child, &pin.children, out);
    }
}

/// Every executor of every profile `playbook` binds. A profile that does
/// not resolve is left out: the gate refuses it on its own. A node attempt
/// runs under apb's minimal agent settings unless its profile asks for the
/// full environment; the background supervisor (when `supervised`) always
/// loads the agent's own settings, so it is predicted on those.
fn predict_playbook(
    root: &Path,
    playbook: &Playbook,
    origin: PlaybookOrigin,
    supervised: bool,
    out: &mut Vec<BillingEntry>,
) {
    let mut predict_ref = |r: &apb_core::profile::QualifiedProfileRef, supervisor: bool| {
        if let Ok(p) = profile_store::resolve_profile(root, origin, r) {
            let hermetic =
                !supervisor && !matches!(p.doc.environment, Some(AgentEnvironment::Full));
            for (agent, model) in executors(&p.doc) {
                out.push(predict(root, &p.doc, &agent, &model, hermetic));
            }
        }
    };
    for r in super::profile_refs::collect_profile_refs(playbook, false) {
        predict_ref(&r, false);
    }
    if supervised && let Some(r) = super::profile_refs::supervisor_profile_ref(playbook) {
        predict_ref(&r, true);
    }
}

/// The executors a profile binds: its primary, its fallbacks and the
/// executors its tiers name.
fn executors(doc: &ProfileDoc) -> Vec<(String, String)> {
    let e = &doc.executor;
    let mut out = vec![(e.agent.clone(), e.model.clone())];
    out.extend(
        e.fallbacks
            .iter()
            .map(|f| (f.agent.clone(), f.model.clone())),
    );
    for (_, tier) in doc.tiers.iter() {
        if let (Some(a), Some(m)) = (&tier.agent, &tier.model) {
            out.push((a.clone(), m.clone()));
        }
    }
    out
}

/// The basis `agent` on `model` would be paid on, spawned for a profile
/// `doc` in the workspace at `root` (`hermetic`: under apb's minimal agent
/// settings).
fn predict(
    root: &Path,
    doc: &ProfileDoc,
    agent: &str,
    model: &str,
    hermetic: bool,
) -> BillingEntry {
    let env = crate::adapter::SpawnEnv {
        process: crate::agent_env::plan_for_entry(doc.process_env.as_ref(), agent, model, &[]),
        ..Default::default()
    };
    let reading = crate::adapter::cost_reading(agent, model, &env, Some(root), hermetic);
    BillingEntry {
        agent: canonical(agent),
        basis: reading.basis,
    }
}

/// The id a spawn and a prediction name an agent by.
pub fn canonical(agent: &str) -> String {
    apb_core::agents::lookup(agent)
        .map(|a| a.descriptor().id.to_string())
        .unwrap_or_else(|| agent.to_string())
}

/// Whether a basis needs anything from a person under `effective`.
pub fn held(effective: &EffectiveBilling, basis: CostBasis) -> bool {
    effective.policy_for(basis) != LaunchPolicy::Allow
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A config dir holding `policy` as its `policy.yaml`.
    fn dir_with(policy: &str) -> tempfile::TempDir {
        let d = tempfile::tempdir().unwrap();
        std::fs::write(d.path().join(apb_core::policy_file::POLICY_FILE), policy).unwrap();
        d
    }

    #[test]
    fn a_redirected_dir_is_held_to_the_default_dirs_stricter_settings() {
        let root = tempfile::tempdir().unwrap();
        let here = dir_with("billing:\n  billed: allow\n  unknown_basis: allow\n");
        let alone = settings_in(root.path(), Some(here.path()), None);
        assert_eq!(alone.billed, LaunchPolicy::Allow);
        // (default dir's file, billed, unknown_basis)
        for (default, billed, unknown) in [
            ("", LaunchPolicy::Ask, LaunchPolicy::Ask),
            (
                "billing:\n  billed: deny\n",
                LaunchPolicy::Deny,
                LaunchPolicy::Ask,
            ),
            (
                "billing:\n  billed: allow\n  unknown_basis: allow\n",
                LaunchPolicy::Allow,
                LaunchPolicy::Allow,
            ),
            (
                "billing: [damaged]\n",
                LaunchPolicy::Deny,
                LaunchPolicy::Deny,
            ),
        ] {
            let d = dir_with(default);
            let e = settings_in(root.path(), Some(here.path()), Some(d.path()));
            assert_eq!((e.billed, e.unknown_basis), (billed, unknown), "{default}");
            assert!(
                e.sources
                    .iter()
                    .any(|s| s.starts_with("default config dir")),
                "{default}: {:?}",
                e.sources
            );
        }
    }
}
