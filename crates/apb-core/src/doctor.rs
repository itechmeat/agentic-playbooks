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

/// Qualified references to playbook profiles (every node that runs an agent,
/// through the same `NodeKind::effective_profile_ref` the run gate uses, plus
/// the supervisor) - including global-scope ones that may not be among the
/// project profiles.
fn playbook_profile_refs(playbook: &Playbook) -> Vec<QualifiedProfileRef> {
    let mut out: Vec<QualifiedProfileRef> = playbook
        .nodes
        .iter()
        .filter_map(|n| n.kind.effective_profile_ref(&playbook.defaults))
        .collect();
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

    // The `suggestions:` timing section of both config files. `dismiss::timing`
    // is the only validator of that section, and every production caller keeps
    // running on the defaults when it is invalid, so without this check an
    // invalid section is silently ignored everywhere. Reported before the
    // registry check, which returns early on a broken `.apb`.
    let (_, suggestion_diagnostics) = crate::dismiss::timing(root);
    if suggestion_diagnostics.is_empty() {
        r.push(
            CheckStatus::Ok,
            "suggestions config",
            "valid (absent = defaults)",
        );
    } else {
        for diagnostic in suggestion_diagnostics {
            r.push(CheckStatus::Fail, "suggestions config", diagnostic);
        }
    }

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
    // comes from the free detect for the built-in ten, and for the rest -
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
    // Loaded on the first profile that resolves: detection, the models table
    // and the config's model policy (crate::model_check).
    let mut model_cx: Option<crate::model_check::ModelContext> = None;
    // A profile named both `x` and `{ name: x, scope: project }` resolves to
    // the same file twice: judge its models once.
    let mut model_checked: BTreeSet<std::path::PathBuf> = BTreeSet::new();
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
                if !model_checked.insert(lp.dir.clone()) {
                    continue;
                }
                let cx = model_cx.get_or_insert_with(crate::model_check::ModelContext::load);
                let ex = &lp.doc.executor;
                let chain = std::iter::once((&ex.agent, &ex.model))
                    .chain(ex.fallbacks.iter().map(|f| (&f.agent, &f.model)));
                for (agent, model) in chain {
                    use crate::model_check::ModelIssue;
                    let Some(issue) = crate::model_check::check(agent, model, cx) else {
                        continue;
                    };
                    // Installation is the agent check's job below, and an
                    // unverifiable model is not a finding.
                    let status = match issue {
                        _ if issue.is_blocking() => CheckStatus::Fail,
                        ModelIssue::Unknown(_) | ModelIssue::NotAvailable => CheckStatus::Warn,
                        _ => continue,
                    };
                    r.push(
                        status,
                        format!("profile {}", pref.name),
                        format!("{}: {}", issue.code(), issue.describe(agent, model)),
                    );
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
    // the built-in ten is mentioned (otherwise the PATH fallback is enough;
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
        .any(|a| detect_ids.contains(crate::detect::canonical_agent_id(a)));
    let detected = if want_detect {
        crate::agent_catalog::agents(false)
    } else {
        Vec::new()
    };
    for agent in &agents {
        let probe_id = crate::detect::canonical_agent_id(agent);
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
                // zcode's headless CLI needs its own `zcode-agent login`: the
                // desktop app's login does not give it a plan identity, and
                // every run then fails with "Select a model before continuing".
                if probe_id == crate::zcode::AGENT_ID
                    && info
                        .auth
                        .as_ref()
                        .is_some_and(|a| a.kind == crate::detect::AuthKind::None)
                {
                    r.push(
                        CheckStatus::Warn,
                        format!("agent {agent} login"),
                        "standalone CLI not logged in to any Z.ai plan: run \
                         `~/.zcode/server/agents/glm/zcode-agent login` once"
                            .to_string(),
                    );
                }
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

    // What a run of each playbook would need from this machine
    // (crate::preflight): warnings, the run gate refuses on them.
    for (id, playbook) in &loaded {
        for (code, message) in crate::preflight::findings(root, playbook) {
            r.push(
                CheckStatus::Warn,
                format!("playbook {id}"),
                format!("{code}: {message}"),
            );
        }
    }

    let ctx = ValidationContext::for_registry(&reg, PlaybookOrigin::Project);
    for (id, playbook) in &loaded {
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
