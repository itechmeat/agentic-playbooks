//! The environment doctor (`apb doctor`): the config, the registry, the
//! profiles and their models, the agents (`agents`), the runners and the
//! playbooks, as one report (`report`).

use std::collections::BTreeSet;
use std::path::Path;

use crate::config::{GlobalConfig, program_in_path};
use crate::profile::{ProfileScope, QualifiedProfileRef};
use crate::profile_store::PlaybookOrigin;
use crate::registry::Registry;
use crate::schema::{NodeKind, Playbook};
use crate::validate::{Severity, ValidationContext, validate};

mod agents;
mod report;

pub use agents::diagnose_agents;
pub use report::{Check, CheckStatus, DoctorReport};

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

    // Decision models (issue #165 Part 2): one line, never a key. Reported
    // before the registry check for the same reason as the suggestions line.
    let (decisions_ok, decisions_detail) = crate::decisions::doctor_line(root);
    r.push(
        if decisions_ok {
            CheckStatus::Ok
        } else {
            CheckStatus::Warn
        },
        "decisions",
        decisions_detail,
    );

    // Host execution mode (0.23.0): the machine default for a run started
    // here without a run argument, and in host mode that profile CLIs are
    // ignored.
    let (execution_ok, execution_detail) = crate::execution::doctor_line(root);
    r.push(
        if execution_ok {
            CheckStatus::Ok
        } else {
            CheckStatus::Warn
        },
        "execution",
        execution_detail,
    );

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
    // The version directory of each loaded playbook, for the script check.
    let mut version_dirs: Vec<std::path::PathBuf> = Vec::new();
    for id in &ids {
        match reg.load(id, None) {
            Ok(l) => {
                version_dirs.push(reg.version_dir(id, &l.version));
                loaded.push((id.clone(), l.playbook));
            }
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
                // The executor, its fallbacks and every tier (issue #165
                // Part 12).
                for (agent, _) in lp.doc.executor_pairs() {
                    agents.insert(agent.to_string());
                }
                for (agent, model) in lp.doc.executor_pairs() {
                    use crate::model_check::ModelIssue;
                    let Some(issue) = crate::model_check::check(agent, model, cx) else {
                        continue;
                    };
                    // Installation is the agent check's job below, and an
                    // unverifiable model is not a finding. A newer id of a
                    // known Claude family is a note: it passes.
                    let status = match issue {
                        _ if issue.is_blocking() => CheckStatus::Fail,
                        ModelIssue::Unknown(_) | ModelIssue::NotAvailable => CheckStatus::Warn,
                        ModelIssue::NewInFamily => CheckStatus::Ok,
                        _ => continue,
                    };
                    r.push(
                        status,
                        format!("profile {}", pref.name),
                        format!("{}: {}", issue.code(), issue.describe(agent, model)),
                    );
                }
            }
            // Not found where the global store was the last place looked: a
            // warning, since the global store is machine-local (V91 is a
            // warning for the same reason). Every other failure is a Fail.
            Err(e) => r.push(
                if matches!(e, crate::profile::ProfileError::NotFound(_))
                    && pref.scope != ProfileScope::Project
                {
                    CheckStatus::Warn
                } else {
                    CheckStatus::Fail
                },
                format!("profile {}", pref.name),
                format!("cannot resolve: {e}"),
            ),
        }
    }
    agents::agent_checks(&mut r, &global, &agents);

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
    // A script a run would not find (V92) fails every run of the playbook.
    // An unresolvable profile (V91) is already the `profile <name>` line above.
    // Both count in the playbook's summary line below with their severity, so
    // it never says "valid" next to a finding for the same playbook.
    let mut reference_counts: Vec<(usize, usize)> = Vec::with_capacity(loaded.len());
    for ((id, playbook), version_dir) in loaded.iter().zip(&version_dirs) {
        let issues = crate::preflight::missing_references(
            root,
            PlaybookOrigin::Project,
            version_dir,
            playbook,
        );
        for issue in issues.iter().filter(|i| i.code == "V92") {
            r.push(
                CheckStatus::Fail,
                format!("playbook {id}"),
                format!("{}: {}", issue.code, issue.message),
            );
        }
        let errors = issues
            .iter()
            .filter(|i| i.severity == Severity::Error)
            .count();
        reference_counts.push((errors, issues.len() - errors));
    }

    let ctx = ValidationContext::for_registry(&reg, PlaybookOrigin::Project);
    for ((id, playbook), (ref_errors, ref_warnings)) in loaded.iter().zip(&reference_counts) {
        let report = validate(playbook, &ctx);
        let errors = ref_errors
            + report
                .issues
                .iter()
                .filter(|i| i.severity == Severity::Error)
                .count();
        let warnings = ref_warnings
            + report
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
