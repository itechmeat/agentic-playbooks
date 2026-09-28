//! Measured decision thresholds (issue #165 Part 13): the machine's
//! `<config_dir>/decisions-thresholds.yaml`, written by
//! `apb decisions thresholds set` once `apb decisions report` shows a use
//! eligible, and read by the enforce paths (Part 14) through
//! [`stored_threshold`].
//!
//! A threshold is stored for one exact `(use, provider, model)` and applies
//! to nothing else: answers drift between model versions and thresholds do
//! not transfer across providers, so a new model id never inherits an older
//! one's value (it needs its own shadow period and report). There is no
//! wildcard, alias or fallback lookup.
//!
//! A sibling file of `decisions.yaml` (which stays hand-edited), with its own
//! `version`. It holds no key and no state, only numbers and names.

use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::decisions::USE_NAMES;

/// The file name under the config dir.
pub const THRESHOLDS_FILE: &str = "decisions-thresholds.yaml";

/// One stored threshold.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StoredThreshold {
    /// The use name, as in `decisions.yaml` (`completion_check`, ...).
    #[serde(rename = "use")]
    pub use_name: String,
    /// The configured provider id that answered the measured decisions.
    pub provider: String,
    /// The exact model id the provider reported, as journaled.
    pub model: String,
    /// The use's own threshold value (for the completion check: the
    /// `final_result` cut, flag below it).
    pub threshold: f64,
    /// When it was stored, epoch milliseconds.
    #[serde(default)]
    pub set_at_ms: u64,
}

#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct FileDoc {
    #[serde(default = "version_one")]
    version: u32,
    #[serde(default)]
    thresholds: Vec<StoredThreshold>,
}

fn version_one() -> u32 {
    1
}

/// Every stored threshold under `config_dir`; empty when the file does not
/// exist. A file that does not parse is an error, so a typo never reads as
/// "no threshold" silently on the write path.
pub fn load_in(config_dir: &Path) -> Result<Vec<StoredThreshold>, String> {
    let path = config_dir.join(THRESHOLDS_FILE);
    let raw = match std::fs::read_to_string(&path) {
        Ok(raw) => raw,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(format!("cannot read {THRESHOLDS_FILE}: {e}")),
    };
    let doc: FileDoc = serde_yaml_ng::from_str(&raw)
        .map_err(|e| format!("{THRESHOLDS_FILE} does not parse: {e}"))?;
    if doc.version != 1 {
        return Err(format!(
            "{THRESHOLDS_FILE}: unsupported version {}",
            doc.version
        ));
    }
    Ok(doc.thresholds)
}

/// The stored threshold for exactly this `(use, provider, model)`, or `None`
/// (no file, no entry, an unreadable file). The one lookup every enforce
/// path calls; `None` means the path refuses and journals
/// `enforce_refused: no_threshold`.
pub fn stored_threshold(use_name: &str, provider: &str, model: &str) -> Option<f64> {
    stored_threshold_in(&crate::config::config_dir()?, use_name, provider, model)
}

/// [`stored_threshold`] with an explicit config dir.
pub fn stored_threshold_in(
    config_dir: &Path,
    use_name: &str,
    provider: &str,
    model: &str,
) -> Option<f64> {
    load_in(config_dir)
        .ok()?
        .into_iter()
        .find(|t| t.use_name == use_name && t.provider == provider && t.model == model)
        .map(|t| t.threshold)
        .filter(|t| t.is_finite())
}

/// Whether an enforce path of `use_name` reads a stored threshold (issue
/// #165 Part 14): every engine use but the judge node and edge (declared
/// thresholds in the playbook) and the catalog ranking (advisory, its cut
/// in `decisions.yaml`).
pub fn reads_stored_threshold(use_name: &str) -> bool {
    !matches!(use_name, "judge_node" | "judge_edge" | "catalog_rank")
}

/// Stores (or replaces) the threshold for `(use, provider, model)`.
pub fn set_threshold_in(
    config_dir: &Path,
    use_name: &str,
    provider: &str,
    model: &str,
    threshold: f64,
) -> Result<(), String> {
    if !USE_NAMES.contains(&use_name) {
        return Err(format!(
            "unknown use `{use_name}` (one of {})",
            USE_NAMES.join(", ")
        ));
    }
    if !reads_stored_threshold(use_name) {
        return Err(format!(
            "`{use_name}` reads no stored threshold: {}",
            match use_name {
                "catalog_rank" => "its cut is `uses.catalog_rank.thresholds` in decisions.yaml",
                _ => "a judge node uses its declared `thresholds` and a judge edge its `min_p`",
            }
        ));
    }
    if provider.trim().is_empty() || model.trim().is_empty() {
        return Err("provider and model must not be empty".into());
    }
    if !(0.0..=1.0).contains(&threshold) {
        return Err(format!(
            "threshold must be between 0 and 1, got {threshold}"
        ));
    }
    let mut all = load_in(config_dir)?;
    all.retain(|t| !(t.use_name == use_name && t.provider == provider && t.model == model));
    all.push(StoredThreshold {
        use_name: use_name.to_string(),
        provider: provider.to_string(),
        model: model.to_string(),
        threshold,
        set_at_ms: crate::clock::now_ms_u64(),
    });
    all.sort_by(|a, b| {
        (&a.use_name, &a.provider, &a.model).cmp(&(&b.use_name, &b.provider, &b.model))
    });
    let doc = FileDoc {
        version: 1,
        thresholds: all,
    };
    let text = serde_yaml_ng::to_string(&doc).map_err(|e| e.to_string())?;
    std::fs::create_dir_all(config_dir).map_err(|e| e.to_string())?;
    crate::fsutil::atomic_write(&config_dir.join(THRESHOLDS_FILE), text.as_bytes())
        .map_err(|e| format!("cannot write {THRESHOLDS_FILE}: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_threshold_round_trips_for_its_exact_model_only() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(
            stored_threshold_in(dir.path(), "completion_check", "main", "jev-1.13.0"),
            None
        );
        set_threshold_in(dir.path(), "completion_check", "main", "jev-1.13.0", 0.15).unwrap();
        assert_eq!(
            stored_threshold_in(dir.path(), "completion_check", "main", "jev-1.13.0"),
            Some(0.15)
        );
        // No inheritance: another model id, provider or use has none.
        for (u, p, m) in [
            ("completion_check", "main", "jev-1.14.0"),
            ("completion_check", "main", "jev-1.13"),
            ("completion_check", "other", "jev-1.13.0"),
            ("retry_advice", "main", "jev-1.13.0"),
        ] {
            assert_eq!(
                stored_threshold_in(dir.path(), u, p, m),
                None,
                "{u} {p} {m}"
            );
        }
        // Setting again replaces instead of adding a second entry.
        set_threshold_in(dir.path(), "completion_check", "main", "jev-1.13.0", 0.2).unwrap();
        set_threshold_in(dir.path(), "completion_check", "main", "jev-1.14.0", 0.1).unwrap();
        let all = load_in(dir.path()).unwrap();
        assert_eq!(all.len(), 2);
        assert_eq!(
            stored_threshold_in(dir.path(), "completion_check", "main", "jev-1.13.0"),
            Some(0.2)
        );
    }

    #[test]
    fn bad_input_is_refused_and_nothing_is_written() {
        let dir = tempfile::tempdir().unwrap();
        assert!(set_threshold_in(dir.path(), "nope", "main", "m", 0.1).is_err());
        assert!(set_threshold_in(dir.path(), "completion_check", "main", "m", 1.5).is_err());
        assert!(set_threshold_in(dir.path(), "completion_check", "", "m", 0.1).is_err());
        assert!(!dir.path().join(THRESHOLDS_FILE).exists());
        std::fs::write(dir.path().join(THRESHOLDS_FILE), "thresholds: 3\n").unwrap();
        assert!(load_in(dir.path()).is_err());
        assert_eq!(
            stored_threshold_in(dir.path(), "completion_check", "main", "m"),
            None
        );
    }
}
