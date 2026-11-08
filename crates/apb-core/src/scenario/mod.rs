//! Scenario runs: a playbook driven by the real engine with every answer
//! from outside apb scripted (see [`format`]).
//!
//! A scenario lives at `.apb/playbooks/<id>/evals/scenarios/<name>.yaml`,
//! next to the eval cases and outside every version, so editing one never
//! changes a version's trust. This module owns the loader and the digest;
//! [`validate`] holds the V146 to V151 rules. The file and the playbook are
//! untrusted data: the loader reads only regular files below the playbook
//! directory (no link, no special file), at most [`MAX_FILE_BYTES`] each, and
//! a name that is not `[a-z0-9][a-z0-9-]*` of at most 64 characters never
//! becomes a path.

pub mod format;
pub mod validate;

use std::io::Read;
use std::path::{Path, PathBuf};

pub use format::*;
pub use validate::{file_problems, validate_all, validate_scenario};

use crate::validate::{Issue, Severity};

/// The scenario directory under a playbook's `evals/`.
pub const SCENARIOS_DIR: &str = "scenarios";
/// The longest scenario name.
pub const MAX_NAME_LEN: usize = 64;
/// The largest scenario file apb reads.
pub const MAX_FILE_BYTES: u64 = 1 << 20;

/// `<playbook_dir>/evals/scenarios`.
pub fn scenarios_dir(playbook_dir: &Path) -> PathBuf {
    crate::eval::suite_dir(playbook_dir).join(SCENARIOS_DIR)
}

/// Whether `name` is a scenario name: `[a-z0-9][a-z0-9-]*`, at most
/// [`MAX_NAME_LEN`] characters. The one check before a name joins a path.
pub fn is_valid_name(name: &str) -> bool {
    let mut chars = name.chars();
    name.len() <= MAX_NAME_LEN
        && chars
            .next()
            .is_some_and(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
        && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

/// One scenario as loaded: its name (the file stem), where it was read,
/// the parsed file and its digest.
#[derive(Debug, Clone, PartialEq)]
pub struct LoadedScenario {
    pub name: String,
    pub path: PathBuf,
    pub scenario: Scenario,
    /// [`scenario_digest`] of `scenario`.
    pub digest: String,
}

/// Every scenario of a playbook that parsed, by name, and one V146 issue per
/// file that did not.
#[derive(Debug, Clone, Default)]
pub struct LoadedScenarios {
    pub scenarios: Vec<LoadedScenario>,
    pub issues: Vec<Issue>,
}

/// Parses one scenario file's text. `name` is the file stem the `id` must
/// equal. Every problem of the file on its own (it does not parse, an
/// unknown field, another `schema`, a malformed `versions`, a name outside
/// the grammar, a visit with no answer or two kinds, a wait with not exactly
/// one outcome) is an error here; the rules that need the playbook are
/// [`validate_scenario`].
pub fn parse(raw: &str, name: &str) -> Result<Scenario, String> {
    if !is_valid_name(name) {
        return Err(format!(
            "name `{name}` must match [a-z0-9][a-z0-9-]* (at most {MAX_NAME_LEN} characters)"
        ));
    }
    let scenario: Scenario = yaml_serde::from_str(raw).map_err(|e| e.to_string())?;
    let problems = file_problems(&scenario, name);
    if problems.is_empty() {
        Ok(scenario)
    } else {
        Err(problems.join("; "))
    }
}

fn read_capped(playbook_dir: &Path, path: &Path) -> Result<String, String> {
    crate::fsutil::ensure_no_symlink_below(playbook_dir, path)
        .map_err(|e| format!("cannot be read: {e}"))?;
    let file =
        crate::fsutil::open_regular_no_follow(path).map_err(|e| format!("cannot be read: {e}"))?;
    let mut bytes = Vec::new();
    file.take(MAX_FILE_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| format!("cannot be read: {e}"))?;
    if bytes.len() as u64 > MAX_FILE_BYTES {
        return Err(format!("is larger than {MAX_FILE_BYTES} bytes"));
    }
    String::from_utf8(bytes).map_err(|_| "is not UTF-8 text".to_string())
}

/// Loads the scenario `name` of the playbook at `playbook_dir`. A name
/// outside the grammar is refused before any path is built.
pub fn load(playbook_dir: &Path, name: &str) -> Result<LoadedScenario, String> {
    if !is_valid_name(name) {
        return Err(format!(
            "scenario name `{name}` must match [a-z0-9][a-z0-9-]* (at most {MAX_NAME_LEN} characters)"
        ));
    }
    let path = scenarios_dir(playbook_dir).join(format!("{name}.yaml"));
    let raw = read_capped(playbook_dir, &path).map_err(|e| format!("scenario `{name}` {e}"))?;
    let scenario = parse(&raw, name).map_err(|e| format!("scenario `{name}`: {e}"))?;
    Ok(LoadedScenario {
        name: name.to_string(),
        digest: scenario_digest(&scenario),
        path,
        scenario,
    })
}

/// Loads every `*.yaml` of the playbook's scenario directory, sorted by
/// name. A file that cannot be read or parsed becomes a V146 issue, never a
/// panic or a silent skip. A missing directory is no scenario.
pub fn load_all(playbook_dir: &Path) -> LoadedScenarios {
    let mut out = LoadedScenarios::default();
    let dir = scenarios_dir(playbook_dir);
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return out;
    };
    let mut names: Vec<String> = entries
        .flatten()
        .filter_map(|e| {
            let name = e.file_name().into_string().ok()?;
            name.strip_suffix(".yaml").map(str::to_string)
        })
        .collect();
    names.sort();
    for name in names {
        match load(playbook_dir, &name) {
            Ok(loaded) => out.scenarios.push(loaded),
            Err(e) => out
                .issues
                .push(Issue::new::<String>("V146", Severity::Error, None, e)),
        }
    }
    out
}

/// The scenario digest: `sha256:` over the canonical JSON of the parsed
/// file (fields in declaration order, every map sorted, defaults written
/// out), so comments, key order, quoting and spelling a default do not
/// change it, and any answer or expectation does.
pub fn scenario_digest(scenario: &Scenario) -> String {
    let value = serde_json::to_value(scenario).unwrap_or(serde_json::Value::Null);
    crate::content::sha256_hex(canonical(&value).to_string().as_bytes())
}

fn canonical(v: &serde_json::Value) -> serde_json::Value {
    match v {
        serde_json::Value::Object(map) => {
            let mut keys: Vec<&String> = map.keys().collect();
            keys.sort();
            let mut out = serde_json::Map::new();
            for k in keys {
                out.insert(k.clone(), canonical(&map[k]));
            }
            serde_json::Value::Object(out)
        }
        serde_json::Value::Array(items) => {
            serde_json::Value::Array(items.iter().map(canonical).collect())
        }
        other => other.clone(),
    }
}

/// Whether a scenario's `versions` range admits `version` (no range:
/// always). A malformed range admits nothing (and is a V146 error).
pub fn applies_to(scenario: &Scenario, version: &str) -> bool {
    match &scenario.versions {
        None => true,
        Some(r) => crate::eval::VersionRange::parse(r).is_some_and(|range| range.contains(version)),
    }
}

/// Writes a new scenario file `<playbook_dir>/evals/scenarios/<name>.yaml`
/// atomically. Refuses an invalid name, an existing file, and a link below
/// `playbook_dir`. Never overwrites.
pub fn write_new(playbook_dir: &Path, name: &str, yaml: &str) -> Result<PathBuf, String> {
    if !is_valid_name(name) {
        return Err(format!(
            "scenario name `{name}` must match [a-z0-9][a-z0-9-]* (at most {MAX_NAME_LEN} characters)"
        ));
    }
    let dir = scenarios_dir(playbook_dir);
    crate::fsutil::ensure_no_symlink_below(playbook_dir, &dir)
        .map_err(|e| format!("scenario `{name}` cannot be written: {e}"))?;
    std::fs::create_dir_all(&dir)
        .map_err(|e| format!("scenario `{name}` cannot be written: {e}"))?;
    let path = dir.join(format!("{name}.yaml"));
    crate::fsutil::write_new_no_clobber(&path, yaml.as_bytes()).map_err(|e| match e.kind() {
        std::io::ErrorKind::AlreadyExists => "scenario_exists".to_string(),
        _ => format!("scenario `{name}` cannot be written: {e}"),
    })?;
    Ok(path)
}

/// Why a scenario does not fit a version: its `versions` range excludes it,
/// or its `playbook_digest` pin names another digest. `None` when it fits.
/// A stale scenario runs only when the person asks for it.
pub fn staleness(scenario: &Scenario, version: &str, playbook_digest: &str) -> Option<String> {
    if !applies_to(scenario, version) {
        return Some(format!(
            "version {version} is outside the scenario's versions `{}`",
            scenario.versions.as_deref().unwrap_or_default()
        ));
    }
    match &scenario.playbook_digest {
        Some(pin) if pin != playbook_digest => Some(format!(
            "the scenario is pinned to digest {pin}; version {version} has {playbook_digest}"
        )),
        _ => None,
    }
}

#[cfg(test)]
mod write_new_tests {
    use super::*;

    /// A save writes a new file and refuses an existing one without
    /// touching its bytes; a name outside the grammar is refused before any
    /// path is built, and a link below the playbook directory is refused.
    #[test]
    fn write_new_writes_a_new_file_and_refuses_the_rest() {
        let dir = tempfile::tempdir().unwrap();
        let pb = dir.path().join("playbooks/p");
        std::fs::create_dir_all(&pb).unwrap();
        let path = write_new(
            &pb,
            "green-after-one-fix",
            "schema: 1\nid: green-after-one-fix\n",
        )
        .unwrap();
        assert_eq!(path, pb.join("evals/scenarios/green-after-one-fix.yaml"));
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "schema: 1\nid: green-after-one-fix\n"
        );
        let e = write_new(
            &pb,
            "green-after-one-fix",
            "schema: 1\nid: green-after-one-fix\n",
        )
        .unwrap_err();
        assert_eq!(e, "scenario_exists");
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "schema: 1\nid: green-after-one-fix\n",
            "the existing file was replaced"
        );
        assert!(
            write_new(&pb, "Not A Name", "schema: 1\nid: x\n")
                .unwrap_err()
                .contains("must match")
        );
        // A link below the playbook directory: the scenarios dir is a link
        // to outside, so the write is refused.
        #[cfg(unix)]
        {
            let elsewhere = tempfile::tempdir().unwrap();
            let p2 = dir.path().join("playbooks/q");
            std::fs::create_dir_all(p2.join("evals")).unwrap();
            std::os::unix::fs::symlink(elsewhere.path(), p2.join("evals/scenarios")).unwrap();
            let e = write_new(&p2, "green", "schema: 1\nid: green\n").unwrap_err();
            assert!(e.contains("cannot be written"), "{e}");
            assert_eq!(std::fs::read_dir(elsewhere.path()).unwrap().count(), 0);
        }
    }
}
