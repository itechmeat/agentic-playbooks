//! The models attempts actually ran on, per node and per version (issue
//! #193), from [`crate::attempt_models`].

use std::collections::BTreeMap;

use serde::Serialize;

use crate::attempt_models::attempt_models;
use crate::event::Event;

/// The key of an attempt whose host reported no model.
pub const UNREPORTED: &str = "unreported";

/// Attempts by the model they ran on, and how many of them differ from the
/// profile's primary model.
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct ModelUse {
    /// Attempts per model actually used ([`UNREPORTED`] when a host said
    /// nothing).
    pub models: BTreeMap<String, usize>,
    /// Attempts whose model differs from the node profile's primary model.
    pub model_mismatch: usize,
}

impl ModelUse {
    fn add(&mut self, model: Option<&str>, mismatch: bool) {
        *self
            .models
            .entry(model.unwrap_or(UNREPORTED).to_string())
            .or_default() += 1;
        self.model_mismatch += usize::from(mismatch);
    }

    /// `glm-5.3-flash x5, opus x1 (5 differ from the profile)`, or `None`
    /// when no attempt ran.
    pub fn text(&self) -> Option<String> {
        if self.models.is_empty() {
            return None;
        }
        let mut t = self
            .models
            .iter()
            .map(|(m, n)| format!("{m} x{n}"))
            .collect::<Vec<_>>()
            .join(", ");
        if self.model_mismatch > 0 {
            t.push_str(&format!(
                " ({} differ from the profile)",
                self.model_mismatch
            ));
        }
        Some(t)
    }
}

/// The model use of runs (each its journal and its nodes' expected models),
/// per version and per node.
pub(super) fn model_use<'a>(
    runs: impl IntoIterator<Item = (&'a [Event], &'a BTreeMap<String, String>)>,
) -> (ModelUse, BTreeMap<String, ModelUse>) {
    let mut version = ModelUse::default();
    let mut nodes: BTreeMap<String, ModelUse> = BTreeMap::new();
    for (events, expected) in runs {
        for a in attempt_models(events, expected) {
            version.add(a.model.as_deref(), a.mismatch);
            nodes
                .entry(a.node.clone())
                .or_default()
                .add(a.model.as_deref(), a.mismatch);
        }
    }
    (version, nodes)
}
