//! The privacy class along a profile's executor chain: the one rule the
//! resolved profile, the profile write, `apb validate` and the node rule
//! V221 share. The classes of a chain's `(agent, model)` pairs come from
//! `crate::agents::chain_privacy`.

use serde::Serialize;

use super::*;

/// The warning of a chain that spans classes.
pub const CLASS_CHANGES: &str = "profile_privacy_class_changes";

/// The classes of a chain, in chain order, and what they add up to.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PrivacySummary {
    /// One entry per executor, fallback and tier; `None` outside a catalog.
    pub classes: Vec<Option<PrivacyClass>>,
    /// Every entry has the same class.
    pub uniform: bool,
    /// The least private class, `None` when any entry is outside a catalog.
    pub least_private: Option<PrivacyClass>,
    pub warnings: Vec<&'static str>,
}

/// Sums up the classes of a chain.
pub fn summarize(classes: Vec<Option<PrivacyClass>>) -> PrivacySummary {
    let uniform = classes.windows(2).all(|w| w[0] == w[1]);
    let least_private = if classes.iter().any(Option::is_none) {
        None
    } else {
        classes.iter().flatten().max().copied()
    };
    let mut warnings = Vec::new();
    if !uniform && classes.iter().any(Option::is_some) {
        warnings.push(CLASS_CHANGES);
    }
    PrivacySummary {
        classes,
        uniform,
        least_private,
        warnings,
    }
}

/// The class names in chain order, for a sentence.
pub fn describe(classes: &[Option<PrivacyClass>]) -> String {
    classes
        .iter()
        .map(|c| c.map_or("unclassified", PrivacyClass::as_str))
        .collect::<Vec<_>>()
        .join(" -> ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use PrivacyClass::*;

    #[test]
    fn uniform_least_private_and_the_warning() {
        let s = summarize(vec![Some(ConfidentialTee), Some(ConfidentialTee)]);
        assert!(s.uniform && s.warnings.is_empty());
        assert_eq!(s.least_private, Some(ConfidentialTee));
        let s = summarize(vec![Some(ConfidentialTee), Some(Incognito)]);
        assert!(!s.uniform);
        assert_eq!(s.least_private, Some(Incognito));
        assert_eq!(s.warnings, [CLASS_CHANGES]);
        let s = summarize(vec![Some(ConfidentialTee), None]);
        assert_eq!(s.least_private, None);
        assert_eq!(s.warnings, [CLASS_CHANGES]);
        let s = summarize(vec![None, None]);
        assert!(s.uniform && s.warnings.is_empty());
        assert_eq!(
            describe(&[Some(ConfidentialTee), None]),
            "confidential_tee -> unclassified"
        );
    }
}
