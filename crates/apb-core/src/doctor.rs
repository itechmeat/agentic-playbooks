use std::collections::BTreeSet;
use std::path::Path;

use crate::config::{GlobalConfig, program_in_path};
use crate::profile::{ProfileScope, QualifiedProfileRef};
use crate::profile_store::PlaybookOrigin;
use crate::registry::Registry;
use crate::schema::{NodeKind, Playbook};
use crate::validate::{Severity, ValidationContext, validate};

/// Result of a single environment check.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CheckStatus {
    Ok,
    Warn,
    Fail,
}

#[derive(Debug, Clone)]
pub struct Check {
    pub name: String,
    pub status: CheckStatus,
    pub detail: String,
}

#[derive(Debug, Default, Clone)]
pub struct DoctorReport {
    pub checks: Vec<Check>,
}

impl DoctorReport {
    fn push(&mut self, status: CheckStatus, name: impl Into<String>, detail: impl Into<String>) {
        self.checks.push(Check {
            name: name.into(),
            status,
            detail: detail.into(),
        });
    }

    /// Whether at least one check failed (for the CLI exit code).
    pub fn has_failure(&self) -> bool {
        self.checks.iter().any(|c| c.status == CheckStatus::Fail)
    }
}

/// Qualified references to playbook profiles (nodes + supervisor, accounting
/// for defaults) - including global-scope ones that may not be among the
/// project profiles.
fn playbook_profile_refs(playbook: &Playbook) -> Vec<QualifiedProfileRef> {
    let mut out = Vec::new();
    for n in &playbook.nodes {
        if let NodeKind::AgentTask { profile, .. } = &n.kind
            && let Some(p) = profile
                .clone()
                .or_else(|| playbook.defaults.profile.clone())
        {
            out.push(p);
        }
    }
    if let Some(s) = &playbook.supervisor
        && let Some(p) = s
            .profile
            .clone()
            .or_else(|| playbook.defaults.profile.clone())
    {
        out.push(p);
    }
    out
}

/// Normalizes an agent id to a detect probe id the same way the invocation
/// resolver does: `claude-code` -> `claude` (shared `claude` binary). Other
/// ids pass through as-is.
fn detect_probe_id(agent: &str) -> &str {
    match agent {
        "claude-code" => "claude",
        other => other,
    }
}

/// Environment diagnostics: global config, playbook and profile registry,
/// availability of agent programs and runner runtimes in PATH, playbook
/// validity. Returns a structured report; formatting and the exit code are
/// the caller's responsibility (CLI).
pub fn diagnose(root: &Path) -> DoctorReport {
    let mut r = DoctorReport::default();

    let global = match GlobalConfig::load() {
        Ok(g) => {
            r.push(
                CheckStatus::Ok,
                "global config",
                "loaded (absent = defaults)",
            );
            g
        }
        Err(e) => {
            r.push(CheckStatus::Fail, "global config", e);
            GlobalConfig::default()
        }
    };

    let reg = match Registry::open(root) {
        Ok(reg) => reg,
        Err(e) => {
            r.push(
                CheckStatus::Fail,
                "project registry",
                format!("cannot open .apb: {e}"),
            );
            return r;
        }
    };
    // Enumerate playbooks by directory (playbook_ids does not fail because of
    // one broken entry), then load each one INDEPENDENTLY: a load failure
    // (e.g. unparseable YAML) is reported as a Fail with the id instead of
    // being swallowed - otherwise a broken playbook would be invisible and
    // would sink the enumeration of the rest.
    let ids = reg.playbook_ids();
    r.push(
        CheckStatus::Ok,
        "playbooks",
        format!("{} registered", ids.len()),
    );
    let profiles = reg.profiles();
    r.push(
        CheckStatus::Ok,
        "profiles",
        format!("{} found", profiles.len()),
    );

    let mut loaded: Vec<(String, Playbook)> = Vec::new();
    for id in &ids {
        match reg.load(id, None) {
            Ok(l) => loaded.push((id.clone(), l.playbook)),
            Err(e) => r.push(
                CheckStatus::Fail,
                format!("playbook {id}"),
                format!("load failed: {e}"),
            ),
        }
    }

    // Agents: collect the ones mentioned through node profiles; status
    // comes from the free detect for the built-in six, and for the rest -
    // a fallback to checking the program in PATH.
    let mut agents: BTreeSet<String> = BTreeSet::new();
    // Resolve the union of: (a) the flat list of project profiles (catches
    // broken standalone profiles) and (b) the QUALIFIED references of every
    // loaded playbook, including global-scope ones - otherwise a profile
    // pointing at a global executor that is not among the project profiles
    // would go unchecked. Dedup by (scope, name); a resolve failure is a
    // separate Fail (not swallowed).
    let mut refs: Vec<QualifiedProfileRef> = profiles
        .iter()
        .map(|name| QualifiedProfileRef {
            name: name.clone(),
            scope: ProfileScope::Auto,
        })
        .collect();
    for (_, playbook) in &loaded {
        refs.extend(playbook_profile_refs(playbook));
    }
    let mut seen_refs: BTreeSet<String> = BTreeSet::new();
    for pref in refs {
        let key = format!("{:?}/{}", pref.scope, pref.name);
        if !seen_refs.insert(key) {
            continue;
        }
        match crate::profile_store::resolve_profile(root, PlaybookOrigin::Project, &pref) {
            Ok(lp) => {
                agents.insert(lp.doc.executor.agent.clone());
                for f in &lp.doc.executor.fallbacks {
                    agents.insert(f.agent.clone());
                }
            }
            Err(e) => r.push(
                CheckStatus::Fail,
                format!("profile {}", pref.name),
                format!("cannot resolve: {e}"),
            ),
        }
    }
    // Detect spawns binaries - we call it only if at least one agent from
    // the built-in six is mentioned (otherwise the PATH fallback is enough;
    // tests stay fast).
    let detect_ids: BTreeSet<String> = crate::detect::builtin_probes()
        .iter()
        .map(|p| p.id.clone())
        .collect();
    // Normalize the mentioned agent ids to detect probe ids (claude-code ->
    // claude), otherwise detect would not find the probe and everything
    // would fall back to a PATH lookup for the nonexistent `claude-code`
    // binary.
    let want_detect = agents
        .iter()
        .any(|a| detect_ids.contains(detect_probe_id(a)));
    let detected = if want_detect {
        crate::detect::detect(false)
    } else {
        Vec::new()
    };
    for agent in &agents {
        let probe_id = detect_probe_id(agent);
        // An agent program explicitly set in the config takes priority over
        // detect: detect probes the fixed names of the six, but here the
        // agent may point at a custom binary (agents.<id>.program).
        if let Some(program) = global.agent_program(agent) {
            if program_in_path(&program) {
                r.push(
                    CheckStatus::Ok,
                    format!("agent {agent}"),
                    format!("program `{program}` found"),
                );
            } else {
                r.push(
                    CheckStatus::Warn,
                    format!("agent {agent}"),
                    format!("program `{program}` not found in PATH"),
                );
            }
        } else if let Some(info) = detected.iter().find(|a| a.agent == probe_id) {
            if info.installed {
                let ver = info.version.as_deref().unwrap_or("unknown version");
                // Print the models list authority - the boundary of what
                // detect can confirm about launchability (spec 8.4).
                let authority = info
                    .models
                    .as_ref()
                    .map(|m| format!(", models authority: {:?}", m.authority))
                    .unwrap_or_default();
                r.push(
                    CheckStatus::Ok,
                    format!("agent {agent}"),
                    format!("installed ({ver}){authority}"),
                );
            } else {
                r.push(
                    CheckStatus::Warn,
                    format!("agent {agent}"),
                    "not installed (free detect)".to_string(),
                );
            }
        } else if program_in_path(agent) {
            r.push(
                CheckStatus::Ok,
                format!("agent {agent}"),
                format!("program `{agent}` found"),
            );
        } else {
            r.push(
                CheckStatus::Warn,
                format!("agent {agent}"),
                format!("program `{agent}` not found in PATH"),
            );
        }
    }

    let mut runners: BTreeSet<String> = BTreeSet::new();
    for (_, playbook) in &loaded {
        for n in &playbook.nodes {
            if let NodeKind::Script { runner, .. } = &n.kind {
                runners.insert(runner.clone());
            }
        }
    }
    for runner in &runners {
        match global.runner_candidates(runner) {
            None => r.push(
                CheckStatus::Fail,
                format!("runner {runner}"),
                "unknown runner (no default, none in config)",
            ),
            Some(list) => match list.iter().find(|p| program_in_path(p)) {
                Some(found) => r.push(
                    CheckStatus::Ok,
                    format!("runner {runner}"),
                    format!("using `{found}`"),
                ),
                None => r.push(
                    CheckStatus::Warn,
                    format!("runner {runner}"),
                    format!("no runtime in PATH (tried: {})", list.join(", ")),
                ),
            },
        }
    }

    for (id, playbook) in &loaded {
        let ctx = ValidationContext {
            profiles: profiles.clone(),
            ..Default::default()
        };
        let report = validate(playbook, &ctx);
        let errors = report
            .issues
            .iter()
            .filter(|i| i.severity == Severity::Error)
            .count();
        let warnings = report
            .issues
            .iter()
            .filter(|i| i.severity == Severity::Warning)
            .count();
        if errors > 0 {
            r.push(
                CheckStatus::Fail,
                format!("playbook {id}"),
                format!("{errors} error(s), {warnings} warning(s)"),
            );
        } else if warnings > 0 {
            r.push(
                CheckStatus::Warn,
                format!("playbook {id}"),
                format!("{warnings} warning(s)"),
            );
        } else {
            r.push(
                CheckStatus::Ok,
                format!("playbook {id}"),
                "valid".to_string(),
            );
        }
    }

    r
}
