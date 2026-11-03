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
//!
//! A use whose labels have rule versions ([`versioned_use`]: `retry_advice`)
//! keeps its thresholds in [`THRESHOLDS_V2_FILE`] instead, each with the
//! rule its labels were measured under, and is looked up only there and
//! only under its current rule; the legacy file is never written in a new
//! shape, so an older apb keeps reading it.

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

/// The file of thresholds measured under a versioned labelling rule, a
/// sibling of [`THRESHOLDS_FILE`]. The legacy file is never written in a new
/// shape: its entries refuse unknown fields, here and in every released
/// apb, so one new field would make the whole file unreadable to an older
/// binary. An older apb never opens this one.
pub const THRESHOLDS_V2_FILE: &str = "decisions-thresholds-v2.yaml";

/// The rule `retry_advice` labels were computed under before rule versions:
/// the journal of either attempt has no `exec`.
pub const RETRY_ADVICE_LEGACY_RULE: &str = "exec/0";
/// The current rule of `retry_advice` labels: read from the attempts'
/// execution identity.
pub const RETRY_ADVICE_RULE: &str = "exec/1";

/// The current labelling rule of a use whose labels have rule versions;
/// `None` for every other use, which keeps reading [`THRESHOLDS_FILE`]. A
/// threshold measured under one rule version never enforces under another.
pub fn versioned_use(use_name: &str) -> Option<&'static str> {
    (use_name == "retry_advice").then_some(RETRY_ADVICE_RULE)
}

/// One threshold of [`THRESHOLDS_V2_FILE`]: [`StoredThreshold`] plus the
/// rule version its labels were measured under.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VersionedThreshold {
    #[serde(rename = "use")]
    pub use_name: String,
    pub provider: String,
    pub model: String,
    /// The labelling rule (`exec/1`).
    pub rule: String,
    pub threshold: f64,
    #[serde(default)]
    pub set_at_ms: u64,
}

/// The document of [`THRESHOLDS_V2_FILE`].
#[derive(Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VersionedThresholdsDoc {
    #[serde(default = "version_one")]
    pub version: u32,
    #[serde(default)]
    pub thresholds: Vec<VersionedThreshold>,
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
    let doc: FileDoc =
        yaml_serde::from_str(&raw).map_err(|e| format!("{THRESHOLDS_FILE} does not parse: {e}"))?;
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

/// [`stored_threshold`] with an explicit config dir. A use with rule
/// versions ([`versioned_use`]) is looked up in [`THRESHOLDS_V2_FILE`]
/// only, under its current rule: a threshold measured under another rule,
/// or stored in the legacy file, never enforces it.
pub fn stored_threshold_in(
    config_dir: &Path,
    use_name: &str,
    provider: &str,
    model: &str,
) -> Option<f64> {
    let found = match versioned_use(use_name) {
        Some(rule) => load_v2_in(config_dir)
            .ok()?
            .into_iter()
            .find(|t| {
                t.use_name == use_name
                    && t.provider == provider
                    && t.model == model
                    && t.rule == rule
            })
            .map(|t| t.threshold),
        None => load_in(config_dir)
            .ok()?
            .into_iter()
            .find(|t| t.use_name == use_name && t.provider == provider && t.model == model)
            .map(|t| t.threshold),
    };
    found.filter(|t| t.is_finite())
}

/// Every threshold of [`THRESHOLDS_V2_FILE`] under `config_dir`; empty when
/// the file does not exist. A file that does not parse (an unknown field
/// included) is an error naming the file; the legacy file's uses are not
/// affected by it.
pub fn load_v2_in(config_dir: &Path) -> Result<Vec<VersionedThreshold>, String> {
    let path = config_dir.join(THRESHOLDS_V2_FILE);
    let raw = match std::fs::read_to_string(&path) {
        Ok(raw) => raw,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(format!("cannot read {THRESHOLDS_V2_FILE}: {e}")),
    };
    let doc: VersionedThresholdsDoc = yaml_serde::from_str(&raw)
        .map_err(|e| format!("{THRESHOLDS_V2_FILE} does not parse: {e}"))?;
    if doc.version != 1 {
        return Err(format!(
            "{THRESHOLDS_V2_FILE}: unsupported version {}",
            doc.version
        ));
    }
    Ok(doc.thresholds)
}

/// Where [`set_threshold_with_rule_in`] stored a threshold: the file, and
/// the rule its labels were measured under for a use with rule versions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SetOutcome {
    pub file: &'static str,
    pub rule: Option<&'static str>,
}

/// Whether an enforce path of `use_name` reads a stored threshold (issue
/// #165 Part 14): every engine use but the judge node and edge (declared
/// thresholds in the playbook) and the catalog ranking (advisory, its cut
/// in `decisions.yaml`) and host-task decisions (the asker reads the
/// answer itself; the engine never acts on it).
pub fn reads_stored_threshold(use_name: &str) -> bool {
    !matches!(
        use_name,
        "judge_node" | "judge_edge" | "catalog_rank" | "host_task"
    )
}

/// Stores (or replaces) the threshold for `(use, provider, model)`.
pub fn set_threshold_in(
    config_dir: &Path,
    use_name: &str,
    provider: &str,
    model: &str,
    threshold: f64,
) -> Result<(), String> {
    set_threshold_with_rule_in(config_dir, use_name, provider, model, threshold, None).map(|_| ())
}

/// Stores (or replaces) the threshold for `(use, provider, model)`: a use
/// with rule versions in [`THRESHOLDS_V2_FILE`] under its current rule
/// (`rule`, when given, must name it), every other use in the legacy file,
/// which is never written in a new shape.
pub fn set_threshold_with_rule_in(
    config_dir: &Path,
    use_name: &str,
    provider: &str,
    model: &str,
    threshold: f64,
    rule: Option<&str>,
) -> Result<SetOutcome, String> {
    check_set(use_name, provider, model, threshold)?;
    let current = versioned_use(use_name);
    match (rule, current) {
        (Some(r), Some(c)) if r != c => {
            return Err(format!(
                "`{use_name}` labels are measured under rule {c} now; a threshold for rule {r} would never enforce"
            ));
        }
        (Some(r), None) => {
            return Err(format!(
                "`{use_name}` has no rule versions; drop --rule {r}"
            ));
        }
        _ => {}
    }
    std::fs::create_dir_all(config_dir).map_err(|e| e.to_string())?;
    match current {
        Some(rule) => {
            set_v2(config_dir, use_name, provider, model, threshold, rule)?;
            Ok(SetOutcome {
                file: THRESHOLDS_V2_FILE,
                rule: Some(rule),
            })
        }
        None => {
            set_legacy(config_dir, use_name, provider, model, threshold)?;
            Ok(SetOutcome {
                file: THRESHOLDS_FILE,
                rule: None,
            })
        }
    }
}

/// The checks every stored threshold passes before it is written.
fn check_set(use_name: &str, provider: &str, model: &str, threshold: f64) -> Result<(), String> {
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
                "host_task" => "the host task that asked reads the answer itself",
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
    Ok(())
}

fn set_legacy(
    config_dir: &Path,
    use_name: &str,
    provider: &str,
    model: &str,
    threshold: f64,
) -> Result<(), String> {
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
    let text = yaml_serde::to_string(&doc).map_err(|e| e.to_string())?;
    crate::fsutil::atomic_write(&config_dir.join(THRESHOLDS_FILE), text.as_bytes())
        .map_err(|e| format!("cannot write {THRESHOLDS_FILE}: {e}"))
}

/// Writes the v2 entry privately (0600), through the write guard of a
/// read-only or scenario process like every private state file.
fn set_v2(
    config_dir: &Path,
    use_name: &str,
    provider: &str,
    model: &str,
    threshold: f64,
    rule: &str,
) -> Result<(), String> {
    let mut all = load_v2_in(config_dir)?;
    all.retain(|t| {
        !(t.use_name == use_name && t.provider == provider && t.model == model && t.rule == rule)
    });
    all.push(VersionedThreshold {
        use_name: use_name.to_string(),
        provider: provider.to_string(),
        model: model.to_string(),
        rule: rule.to_string(),
        threshold,
        set_at_ms: crate::clock::now_ms_u64(),
    });
    all.sort_by(|a, b| {
        (&a.use_name, &a.provider, &a.model, &a.rule).cmp(&(
            &b.use_name,
            &b.provider,
            &b.model,
            &b.rule,
        ))
    });
    let doc = VersionedThresholdsDoc {
        version: 1,
        thresholds: all,
    };
    let text = yaml_serde::to_string(&doc).map_err(|e| e.to_string())?;
    crate::fsutil::atomic_write_private(&config_dir.join(THRESHOLDS_V2_FILE), text.as_bytes())
        .map_err(|e| format!("cannot write {THRESHOLDS_V2_FILE}: {e}"))
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

    /// Design 2.10 items 3 to 5: `retry_advice` is looked up and stored in
    /// the v2 file under its current rule only; a legacy entry never
    /// enforces it, and the legacy file is left byte for byte as it was.
    #[test]
    fn a_versioned_use_reads_and_writes_only_the_v2_file_under_its_rule() {
        let dir = tempfile::tempdir().unwrap();
        let legacy = "version: 1\nthresholds:\n- use: retry_advice\n  provider: main\n  model: m\n  threshold: 0.6\n";
        std::fs::write(dir.path().join(THRESHOLDS_FILE), legacy).unwrap();
        assert_eq!(
            stored_threshold_in(dir.path(), "retry_advice", "main", "m"),
            None
        );
        let set =
            set_threshold_with_rule_in(dir.path(), "retry_advice", "main", "m", 0.7, None).unwrap();
        assert_eq!(
            (set.file, set.rule),
            (THRESHOLDS_V2_FILE, Some(RETRY_ADVICE_RULE))
        );
        assert_eq!(
            std::fs::read_to_string(dir.path().join(THRESHOLDS_FILE)).unwrap(),
            legacy
        );
        assert_eq!(
            stored_threshold_in(dir.path(), "retry_advice", "main", "m"),
            Some(0.7)
        );
        let v2 = load_v2_in(dir.path()).unwrap();
        assert_eq!(v2.len(), 1);
        assert_eq!(v2[0].rule, RETRY_ADVICE_RULE);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(dir.path().join(THRESHOLDS_V2_FILE))
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o600);
        }
        // An entry measured under another rule version never enforces.
        std::fs::write(
            dir.path().join(THRESHOLDS_V2_FILE),
            "version: 1\nthresholds:\n- use: retry_advice\n  provider: main\n  model: m\n  rule: exec/0\n  threshold: 0.6\n",
        )
        .unwrap();
        assert_eq!(
            stored_threshold_in(dir.path(), "retry_advice", "main", "m"),
            None
        );
        // A use without rule versions keeps the legacy file.
        let set =
            set_threshold_with_rule_in(dir.path(), "completion_check", "main", "m", 0.2, None)
                .unwrap();
        assert_eq!((set.file, set.rule), (THRESHOLDS_FILE, None));
    }

    /// A rule other than the current one, a rule for a use without rule
    /// versions and a hand-edited v2 file with an unknown field are refused.
    #[test]
    fn a_stale_rule_and_an_unknown_v2_field_are_refused() {
        let dir = tempfile::tempdir().unwrap();
        let e = set_threshold_with_rule_in(
            dir.path(),
            "retry_advice",
            "main",
            "m",
            0.6,
            Some("exec/0"),
        )
        .unwrap_err();
        assert!(e.contains("exec/1"), "{e}");
        assert!(
            set_threshold_with_rule_in(
                dir.path(),
                "completion_check",
                "main",
                "m",
                0.2,
                Some("exec/1")
            )
            .is_err()
        );
        assert!(!dir.path().join(THRESHOLDS_V2_FILE).exists());
        std::fs::write(
            dir.path().join(THRESHOLDS_V2_FILE),
            "version: 1\nthresholds:\n- use: retry_advice\n  provider: main\n  model: m\n  rule: exec/1\n  threshold: 0.6\n  extra: 1\n",
        )
        .unwrap();
        let e = load_v2_in(dir.path()).unwrap_err();
        assert!(e.contains(THRESHOLDS_V2_FILE), "{e}");
        assert_eq!(
            stored_threshold_in(dir.path(), "retry_advice", "main", "m"),
            None
        );
    }
}
