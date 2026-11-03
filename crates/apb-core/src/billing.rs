//! Billed use: whether agent work whose cost is billed (or whose basis is
//! not known) may start, and whether a run budget counts the list price of
//! work a subscription covers.
//!
//! The `billing:` section of `policy.yaml` ([`crate::policy_file`]) sets it,
//! the machine's file first and a project's file only narrowing it, like
//! the `launch:` section:
//!
//! ```yaml
//! billing:
//!   billed: ask                       # allow | ask | deny, default ask
//!   unknown_basis: ask                # allow | ask | deny, default ask
//!   budget_counts_subscription: true  # default true
//! ```
//!
//! `billed` and `unknown_basis` decide agent work by its predicted cost basis
//! ([`crate::agents::CostBasis`]): `allow` runs it, `ask` runs it only after
//! a person's explicit confirmation of the exact predicted set, `deny`
//! never runs it. Work on a subscription is never held. The stricter of the
//! two files wins, so a project can turn `allow` into `ask` or `deny`, and
//! never the other way round.
//!
//! `budget_counts_subscription` says whether a run budget's dollar limit
//! counts a subscription equivalent: `true` (the default) counts every
//! reported cost, whatever its basis, since a subscription can bill extra
//! usage under the same login; `false` leaves subscription equivalents out,
//! for a person who knows their plan never bills extra. A project can set it
//! to `true` over a machine's `false`, never to `false`.
//!
//! A run records what was allowed or confirmed for it once
//! ([`BillingRecord`], inside its launch record), and every spawn is checked
//! against that record ([`BillingRecord::covers`]).

pub mod section;

use serde::{Deserialize, Serialize};

pub use section::BillingSection;

use crate::agents::CostBasis;
use crate::launch::policy::LaunchPolicy;
use crate::policy_file::Policies;

/// The source of a setting nothing sets.
const DEFAULT_SOURCE: &str = "default";

/// The billing settings a run is decided under, with every setting that
/// produced them named.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EffectiveBilling {
    pub billed: LaunchPolicy,
    pub unknown_basis: LaunchPolicy,
    pub budget_counts_subscription: bool,
    /// One phrase per setting that holds a value, `default` when nothing is
    /// set, sorted.
    pub sources: Vec<String>,
}

/// The policy of a key nothing sets.
pub const DEFAULT_POLICY: LaunchPolicy = LaunchPolicy::Ask;

/// The defaults: `ask`, `ask`, `true`. In apb's own test builds
/// `APB_TEST_BILLING_DEFAULT=allow` makes the two policies `allow`, so the
/// test suites that start runs on stand-in agents (whose basis is unknown)
/// are not all held; the billing tests clear it or set the file. The
/// override exists only with debug assertions, and the `testing` feature is
/// refused without them (`lib.rs`), so no release or dist binary reads it.
fn default_policy() -> LaunchPolicy {
    #[cfg(all(debug_assertions, any(test, feature = "testing")))]
    if std::env::var("APB_TEST_BILLING_DEFAULT").as_deref() == Ok("allow") {
        return LaunchPolicy::Allow;
    }
    DEFAULT_POLICY
}

impl EffectiveBilling {
    /// The machine's settings narrowed by the project's: for each policy the
    /// stricter of the two (the machine's default `ask` when it sets none),
    /// and counting subscription equivalents when either file says so (the
    /// machine's default `true` when it sets none).
    pub fn resolve(settings: &Policies) -> Self {
        let machine = &settings.machine.billing;
        let project = &settings.project.billing;
        let mut sources = Vec::new();
        let mut policy = |key: &str, m: Option<LaunchPolicy>, p: Option<LaunchPolicy>| {
            let base = m.unwrap_or_else(default_policy);
            let out = p.map_or(base, |p| p.max(base));
            if m.is_some() && Some(out) == m {
                sources.push(format!("machine policy.yaml billing.{key}"));
            }
            if p.is_some() && Some(out) == p && Some(out) != m {
                sources.push(format!("project policy.yaml billing.{key}"));
            }
            out
        };
        let billed = policy("billed", machine.billed, project.billed);
        let unknown_basis = policy(
            "unknown_basis",
            machine.unknown_basis,
            project.unknown_basis,
        );
        let base = machine.budget_counts_subscription.unwrap_or(true);
        let budget_counts_subscription = base || project.budget_counts_subscription == Some(true);
        if machine.budget_counts_subscription.is_some() {
            sources.push("machine policy.yaml billing.budget_counts_subscription".into());
        }
        if !base && budget_counts_subscription {
            sources.push("project policy.yaml billing.budget_counts_subscription".into());
        }
        sources.sort();
        sources.dedup();
        if sources.is_empty() {
            sources.push(DEFAULT_SOURCE.to_string());
        }
        EffectiveBilling {
            billed,
            unknown_basis,
            budget_counts_subscription,
            sources,
        }
    }

    /// The settings of the workspace at `root` (`None`: no workspace) under
    /// the machine file `machine` (a config dir's `policy.yaml`; `None`:
    /// none). Each file is read on its own, and one that is present and
    /// does not read fails closed: everything that is not a subscription is
    /// denied and a budget counts every cost, since the file may have said
    /// so. A damaged project file therefore never drops a machine's
    /// restriction, and a damaged machine file never loosens anything.
    pub fn load(root: Option<&std::path::Path>, machine: Option<&std::path::Path>) -> Self {
        Self::load_checked(root, machine).0
    }

    /// [`EffectiveBilling::load`] with the problem of a file that does not
    /// read, for the doctor.
    pub fn load_checked(
        root: Option<&std::path::Path>,
        machine: Option<&std::path::Path>,
    ) -> (Self, Option<String>) {
        use crate::policy_file::{PolicyFile, PolicySource, project_path, read};
        let machine = machine.map_or(Ok(PolicyFile::default()), |p| {
            read(p, PolicySource::Machine).map_err(|e| format!("machine {e}"))
        });
        let project = root.map_or(Ok(PolicyFile::default()), |r| {
            read(&project_path(r), PolicySource::Project).map_err(|e| format!("project {e}"))
        });
        match (machine, project) {
            (Ok(machine), Ok(project)) => (Self::resolve(&Policies { machine, project }), None),
            (machine, project) => {
                let problem = [machine.err(), project.err()]
                    .into_iter()
                    .flatten()
                    .collect::<Vec<_>>()
                    .join("; ");
                let closed = EffectiveBilling {
                    billed: LaunchPolicy::Deny,
                    unknown_basis: LaunchPolicy::Deny,
                    budget_counts_subscription: true,
                    sources: vec![format!("policy.yaml does not read ({problem})")],
                };
                (closed, Some(problem))
            }
        }
    }

    /// The defaults, as when no file sets anything.
    pub fn defaults() -> Self {
        Self::resolve(&Policies::default())
    }

    /// The policy work on `basis` falls under: a subscription is always
    /// allowed.
    pub fn policy_for(&self, basis: CostBasis) -> LaunchPolicy {
        match basis {
            CostBasis::Billed => self.billed,
            CostBasis::Unknown => self.unknown_basis,
            CostBasis::SubscriptionEquivalent => LaunchPolicy::Allow,
        }
    }

    /// The bases the settings allow for every agent.
    pub fn allowed(&self) -> Vec<CostBasis> {
        CostBasis::ALL
            .into_iter()
            .filter(|b| self.policy_for(*b) == LaunchPolicy::Allow)
            .collect()
    }

    /// The effective section as one line, for the doctor.
    pub fn line(&self) -> String {
        format!(
            "billed {}, unknown_basis {}, budget_counts_subscription {} ({})",
            self.billed,
            self.unknown_basis,
            self.budget_counts_subscription,
            self.sources.join(", ")
        )
    }
}

/// One agent a run's profiles bind, with the basis its work is predicted to
/// be paid on.
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct BillingEntry {
    pub agent: String,
    pub basis: CostBasis,
}

impl BillingEntry {
    /// `<agent>: <basis>`.
    pub fn label(&self) -> String {
        format!("{}: {}", self.agent, self.basis.key())
    }
}

/// What a run may spend on, recorded once in its launch record: the bases
/// the settings allowed for every agent, and the agents and bases a person
/// confirmed. Never holds a nonce.
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct BillingRecord {
    /// The bases the settings allowed for every agent when the run was
    /// admitted.
    #[serde(default)]
    pub allowed: Vec<CostBasis>,
    /// The agents and bases a person confirmed.
    #[serde(default)]
    pub confirmed: Vec<BillingEntry>,
    /// How the confirmation was given (`terminal_prompt`, `cli_flag`,
    /// `host_relayed`, `dashboard_nonce`, `forwarded:<kind>`); absent when
    /// nothing needed one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub confirmation: Option<String>,
    /// The settings that decided it.
    #[serde(default)]
    pub sources: Vec<String>,
}

impl BillingRecord {
    /// Whether work of `agent` on `basis` may start under this record: a
    /// subscription always, else a basis the settings allowed or the exact
    /// agent and basis a person confirmed.
    pub fn covers(&self, agent: &str, basis: CostBasis) -> bool {
        basis == CostBasis::SubscriptionEquivalent
            || self.allowed.contains(&basis)
            || self
                .confirmed
                .iter()
                .any(|e| e.agent == agent && e.basis == basis)
    }
}

/// The doctor line: the effective section for the workspace at `root`.
/// `(ok, detail)`, not ok when a settings file does not read (then billed
/// and unknown-basis work is denied until it is fixed).
pub fn doctor_line(root: &std::path::Path) -> (bool, String) {
    let machine = crate::policy_file::machine_path();
    let (effective, problem) = EffectiveBilling::load_checked(Some(root), machine.as_deref());
    (problem.is_none(), effective.line())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::policy_file::PolicyFile;
    use LaunchPolicy::*;

    fn files(machine: BillingSection, project: BillingSection) -> Policies {
        Policies {
            machine: PolicyFile {
                billing: machine,
                ..Default::default()
            },
            project: PolicyFile {
                billing: project,
                ..Default::default()
            },
        }
    }

    #[test]
    fn nothing_set_asks_for_billed_and_unknown_and_counts_subscriptions() {
        let e = EffectiveBilling::resolve(&Policies::default());
        assert_eq!((e.billed, e.unknown_basis), (Ask, Ask));
        assert!(e.budget_counts_subscription);
        assert_eq!(e.sources, vec!["default"]);
        assert_eq!(e.allowed(), vec![CostBasis::SubscriptionEquivalent]);
    }

    #[test]
    fn a_project_narrows_and_never_loosens() {
        let machine = BillingSection {
            billed: Some(Allow),
            unknown_basis: Some(Ask),
            budget_counts_subscription: Some(false),
        };
        let project = BillingSection {
            billed: Some(Deny),
            unknown_basis: Some(Allow),
            budget_counts_subscription: Some(true),
        };
        let e = EffectiveBilling::resolve(&files(machine.clone(), project));
        assert_eq!((e.billed, e.unknown_basis), (Deny, Ask));
        assert!(e.budget_counts_subscription);
        assert!(
            e.sources
                .contains(&"project policy.yaml billing.billed".to_string())
        );
        // A project that tries to loosen everything changes nothing.
        let loosen = BillingSection {
            billed: Some(Allow),
            unknown_basis: Some(Allow),
            budget_counts_subscription: Some(false),
        };
        let none = EffectiveBilling::resolve(&files(BillingSection::default(), loosen));
        assert_eq!((none.billed, none.unknown_basis), (Ask, Ask));
        assert!(none.budget_counts_subscription);
        assert!(
            !EffectiveBilling::resolve(&files(machine, BillingSection::default()))
                .budget_counts_subscription
        );
    }

    #[test]
    fn a_file_that_does_not_read_denies_whichever_file_it_is() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::create_dir_all(root.join(".apb")).unwrap();
        let machine = root.join("machine.yaml");
        let project = crate::policy_file::project_path(root);
        let allow = "billing:\n  billed: allow\n  unknown_basis: allow\n  budget_counts_subscription: false\n";
        let damaged = "billing:\n  billed: allow\n  extra: 1\n";
        for (m, p) in [(damaged, allow), (allow, damaged)] {
            std::fs::write(&machine, m).unwrap();
            std::fs::write(&project, p).unwrap();
            let (e, problem) = EffectiveBilling::load_checked(Some(root), Some(&machine));
            assert_eq!((e.billed, e.unknown_basis), (Deny, Deny), "{m} / {p}");
            assert!(e.budget_counts_subscription, "{m} / {p}");
            assert!(problem.is_some_and(|p| p.contains("extra")), "{m} / {p}");
        }
        std::fs::write(&project, allow).unwrap();
        std::fs::write(&machine, allow).unwrap();
        let e = EffectiveBilling::load(Some(root), Some(&machine));
        assert_eq!((e.billed, e.unknown_basis), (Allow, Allow));
    }

    #[test]
    fn a_record_covers_allowed_bases_and_the_confirmed_agent_only() {
        let r = BillingRecord {
            allowed: vec![CostBasis::Unknown],
            confirmed: vec![BillingEntry {
                agent: "codex".into(),
                basis: CostBasis::Billed,
            }],
            ..Default::default()
        };
        assert!(r.covers("claude", CostBasis::SubscriptionEquivalent));
        assert!(r.covers("claude", CostBasis::Unknown));
        assert!(r.covers("codex", CostBasis::Billed));
        assert!(!r.covers("claude", CostBasis::Billed));
    }
}
