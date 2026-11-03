//! The `billing:` section of one `policy.yaml`, on its own so the settings
//! reader names it without depending on how the two files combine
//! ([`crate::billing::EffectiveBilling`]).

use serde::Deserialize;

use crate::launch::policy::LaunchPolicy;

/// The `billing:` section of one `policy.yaml`. Absent keys set nothing.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct BillingSection {
    pub billed: Option<LaunchPolicy>,
    pub unknown_basis: Option<LaunchPolicy>,
    pub budget_counts_subscription: Option<bool>,
}

impl BillingSection {
    /// Whether the section sets anything.
    pub fn is_empty(&self) -> bool {
        *self == BillingSection::default()
    }

    /// What it sets, for the doctor.
    pub fn describe(&self) -> Vec<String> {
        let mut out = Vec::new();
        if let Some(p) = self.billed {
            out.push(format!("billing.billed {p}"));
        }
        if let Some(p) = self.unknown_basis {
            out.push(format!("billing.unknown_basis {p}"));
        }
        if let Some(b) = self.budget_counts_subscription {
            out.push(format!("billing.budget_counts_subscription {b}"));
        }
        out
    }
}
