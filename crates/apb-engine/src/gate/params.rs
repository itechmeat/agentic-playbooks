//! The params a start must pass: the one check every launch surface runs
//! after the run gate, so a start the fit report says needs a value is
//! refused the same way on MCP, the dashboard and the CLI.
//!
//! A declared param needs a value from the start when it has no `default`
//! and this project remembers no compatible value for it (the same rule as
//! the fit report's `param_needs_value`, read from the same
//! [`apb_core::run_memory::recall`]). The refusal is structural JSON like
//! every gate refusal, so the surfaces pass it on with its start actions.

use std::collections::BTreeMap;
use std::path::Path;

use apb_core::registry::Registry;
use apb_core::run_memory::{MemoryKey, recall};
use apb_core::scope::PlaybookRef;
use serde_json::{Value, json};

/// The policy of the refusal [`check_params`] returns.
pub const PARAM_NEEDS_VALUE: &str = "param_needs_value";

/// The policy of the refusal a start gets when a given value does not
/// match its param's pattern.
pub const PARAM_PATTERN_MISMATCH: &str = "param_pattern_mismatch";

/// Refuses a start of `wref` (the version that runs) that leaves a param
/// without a value: no default, nothing remembered and nothing in `given`.
/// The refusal names them in `missing`. A playbook that does not resolve
/// passes here; the gate before this check has already answered for it.
pub fn check_params(
    root: &Path,
    wref: &PlaybookRef,
    given: &BTreeMap<String, String>,
) -> Result<(), Value> {
    let missing = unfilled(root, wref, given);
    if !missing.is_empty() {
        // A missing value is answered first.
        return Err(json!({
            "policy": PARAM_NEEDS_VALUE,
            "id": wref.id,
            "missing": missing,
            "detail": format!(
                "no default and nothing remembered for {}: a start must pass a value",
                missing.join(", ")
            ),
        }));
    }
    // Every given value of a declared param must match its pattern.
    // Names and patterns only, never the value (a secret included).
    let Ok(resolved) = apb_core::store::resolve(root, wref) else {
        return Ok(());
    };
    let Ok(loaded) = Registry::open_dir(&resolved.definition_parent)
        .and_then(|reg| reg.load(&wref.id, Some(&resolved.version)))
    else {
        return Ok(());
    };
    let mut invalid = Vec::new();
    let mut patterns = serde_json::Map::new();
    for p in &loaded.playbook.params {
        if let Some(value) = given.get(&p.name)
            && let Err(m) = apb_core::param_pattern::check(p, value)
        {
            patterns.insert(m.name.clone(), json!(m.pattern));
            invalid.push(m.name);
        }
    }
    if invalid.is_empty() {
        return Ok(());
    }
    Err(json!({
        "policy": PARAM_PATTERN_MISMATCH,
        "id": wref.id,
        "invalid": invalid,
        "patterns": patterns,
        "detail": format!(
            "{} does not match its pattern: pass a value that does",
            invalid.join(", ")
        ),
    }))
}

/// The warning a start of `wref` with `given` gets when a declared secret
/// param's value is too short to be scrubbed from text
/// ([`apb_core::run_params::too_short_to_scrub`]); a default counts when
/// `given` has no value. A warning, never a refusal: every launch surface
/// passes it on with its answer.
pub fn short_secret_warning(
    root: &Path,
    wref: &PlaybookRef,
    given: &BTreeMap<String, String>,
) -> Option<String> {
    let resolved = apb_core::store::resolve(root, wref).ok()?;
    let loaded = Registry::open_dir(&resolved.definition_parent)
        .and_then(|reg| reg.load(&wref.id, Some(&resolved.version)))
        .ok()?;
    let declared: std::collections::BTreeSet<String> =
        crate::run_redact::declared_secret(&loaded.playbook)
            .into_iter()
            .collect();
    let mut values = given.clone();
    for p in &loaded.playbook.params {
        // The default as the start renders it (a number included).
        if p.default.is_some() {
            values
                .entry(p.name.clone())
                .or_insert_with(|| p.default_text().unwrap_or_default());
        }
    }
    apb_core::run_params::short_secret_warning(&apb_core::run_params::too_short_to_scrub(
        &values, &declared,
    ))
}

fn unfilled(root: &Path, wref: &PlaybookRef, given: &BTreeMap<String, String>) -> Vec<String> {
    let Ok(resolved) = apb_core::store::resolve(root, wref) else {
        return Vec::new();
    };
    let Ok(loaded) = Registry::open_dir(&resolved.definition_parent)
        .and_then(|reg| reg.load(&wref.id, Some(&resolved.version)))
    else {
        return Vec::new();
    };
    let params = &loaded.playbook.params;
    // Memory that does not read counts as empty: every param without a
    // default then needs a value, which is what the start would get.
    let missing = MemoryKey::for_label(resolved.origin_label, &wref.id)
        .ok()
        .and_then(|key| recall(root, &key, params, None).ok())
        .map(|r| r.missing)
        .unwrap_or_else(|| without_default(params));
    missing
        .into_iter()
        .filter(|name| !given.contains_key(name))
        .collect()
}

/// The params a start must pass when nothing is remembered (or memory
/// does not read): every one without a default. The gate and the fit
/// report share it, so a report never says ready to a start the gate
/// refuses.
pub(crate) fn without_default(params: &[apb_core::schema::Param]) -> Vec<String> {
    params
        .iter()
        .filter(|p| p.default.is_none())
        .map(|p| p.name.clone())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use apb_core::scope::Origin;

    pub(crate) fn project_for_patterns(params: &str) -> tempfile::TempDir {
        project(params)
    }

    pub(crate) fn wref_for_patterns() -> PlaybookRef {
        wref()
    }

    fn project(params: &str) -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        let vdir = dir.path().join(".apb/playbooks/p/1.0.0");
        std::fs::create_dir_all(&vdir).unwrap();
        std::fs::write(
            vdir.join("playbook.yaml"),
            format!(
                "schema: 2\nid: p\nname: p\nversion: 1.0.0\nparams:\n{params}nodes:\n  - {{ id: s, type: start }}\n  - {{ id: done, type: finish, outcome: success }}\nedges:\n  - {{ from: s, to: done }}\n"
            ),
        )
        .unwrap();
        std::fs::write(dir.path().join(".apb/playbooks/p/current"), "1.0.0").unwrap();
        dir
    }

    fn wref() -> PlaybookRef {
        PlaybookRef {
            origin: Origin::Project { workspace_id: None },
            id: "p".into(),
            version: None,
        }
    }

    #[test]
    fn a_param_without_default_or_value_is_refused_with_its_name() {
        let dir = project(
            "  - { name: target, type: text }\n  - { name: mode, type: text, default: fast }\n",
        );
        let refusal = check_params(dir.path(), &wref(), &BTreeMap::new()).unwrap_err();
        assert_eq!(refusal["policy"], PARAM_NEEDS_VALUE);
        assert_eq!(refusal["missing"], json!(["target"]));
        let actions = crate::next_action::start::from_refusal(
            crate::next_action::start::StartOf::of(&wref()),
            &refusal,
        );
        assert_eq!(actions.len(), 1);
        assert_eq!(
            actions[0].remedy,
            Some(crate::next_action::start::Remedy::ProvideParam {
                name: "target".into()
            })
        );
    }

    #[test]
    fn a_given_value_passes() {
        let dir = project("  - { name: target, type: text }\n");
        let given = BTreeMap::from([("target".to_string(), "x".to_string())]);
        assert!(check_params(dir.path(), &wref(), &given).is_ok());
    }
    /// A given value that does not match its param's pattern is refused
    /// with `param_pattern_mismatch`, naming the params and patterns and
    /// never the value; a missing value is answered first.
    #[test]
    fn a_value_that_does_not_match_its_pattern_is_refused() {
        let dir = project_for_patterns(
            "  - { name: token, type: text, secret: true, pattern: '[a-z]{8,}' }\n  - { name: repo, type: text, pattern: '[A-Za-z0-9._-]{1,100}' }\n",
        );
        let mut given = BTreeMap::new();
        given.insert("token".into(), "s3cr3t-value".into());
        given.insert("repo".into(), "a b".into());
        let refusal = check_params(dir.path(), &wref_for_patterns(), &given).unwrap_err();
        assert_eq!(refusal["policy"], PARAM_PATTERN_MISMATCH);
        assert_eq!(refusal["id"], "p");
        assert_eq!(refusal["invalid"], json!(["token", "repo"]));
        assert_eq!(refusal["patterns"]["token"], "[a-z]{8,}");
        assert_eq!(refusal["patterns"]["repo"], "[A-Za-z0-9._-]{1,100}");
        let text = serde_json::to_string(&refusal).unwrap();
        assert!(!text.contains("s3cr3t-value"), "{text}");
        // The start actions give one remedy per invalid name.
        let actions = crate::next_action::start::from_refusal(
            crate::next_action::start::StartOf::of(&wref_for_patterns()),
            &refusal,
        );
        assert_eq!(actions.len(), 2);
        assert_eq!(
            actions[1].remedy,
            Some(crate::next_action::start::Remedy::ProvideParam {
                name: "repo".into()
            })
        );
    }

    /// A missing value is answered before a mismatching one: the
    /// `param_needs_value` refusal wins.
    #[test]
    fn a_missing_value_is_answered_first() {
        let dir = project_for_patterns(
            "  - { name: need, type: text }\n  - { name: repo, type: text, pattern: '[a-z]+' }\n",
        );
        let mut given = BTreeMap::new();
        given.insert("repo".into(), "a b".into());
        let refusal = check_params(dir.path(), &wref_for_patterns(), &given).unwrap_err();
        assert_eq!(refusal["policy"], PARAM_NEEDS_VALUE);
    }
}
