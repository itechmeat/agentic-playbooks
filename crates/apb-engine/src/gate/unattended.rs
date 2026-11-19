//! The unattended part of the run gate. An unattended run has nobody to
//! answer a wait, so the modes that need someone by design are refused
//! before any run file exists, and a gate's declared `decide` is checked
//! again when the run reaches it.
//!
//! The guarantee: an unattended run never waits for a person and never
//! acts for one. It is checked at three boundaries:
//! 1. every launch surface calls [`check_start`] before it starts a run;
//! 2. the engine's prepare calls it again before it writes anything, so a
//!    start path that skipped the surface check (a trial, a plan, a child)
//!    is refused the same way;
//! 3. the drive rechecks a gate's `decide` with [`decide_refusal`] before
//!    it posts the decision, and fails the run instead when it is refused.
//!
//! The mode grants nothing: the irreversible-effects consent and the launch
//! policy are decided as for any start. Who may ask for it: anyone but a
//! run step of an attended run ([`check_start`]); a sub-playbook child gets
//! its parent's mode from the drive, whatever its caller passed.

use std::collections::BTreeMap;
use std::collections::BTreeSet;
use std::path::Path;

use apb_core::connector::resolve::ConnectorFacts;
use apb_core::execution::{ExecutionMode, ExecutionRequest};
use apb_core::schema::{Effect, NodeKind, Playbook};
use serde::Serialize;
use serde_json::{Value, json};

use crate::run_config::{PROBATION_REQUIRED, RunMode, UNATTENDED_UNSUPPORTED};

/// Where a probation stands for one definition in one project.
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ProbationView {
    /// Successful attended runs (started without `--unattended`, not from
    /// a run step) asked for: the largest of the playbook's and the two
    /// `policy.yaml` files.
    pub required: u32,
    /// Eligible successful attended runs of this exact definition in this
    /// project's probation ledger. Attended says how a run was started,
    /// never that a person decided anything in it.
    pub counted: u32,
    /// `playbook`, `machine` or `project`: which asked for `required`.
    pub source: String,
    /// The machine's `policy.yaml` waives probation here (an unverifiable
    /// ledger included).
    pub waived: bool,
    /// The ledger verified (or does not exist yet). False: nothing
    /// counts, and an unattended start is refused unless waived.
    pub ledger_ok: bool,
}

/// What an unattended start asks for.
#[derive(Debug, Clone, Copy)]
pub struct UnattendedStart<'a> {
    /// The start asks for unattended mode; nothing is checked otherwise.
    pub unattended: bool,
    /// The run mode it would be driven in.
    pub mode: RunMode,
    /// The execution request, as the start would hand it to the engine.
    pub execution: &'a ExecutionRequest,
}

/// Refuses an unattended start that needs someone by design: a supervised
/// mode (a supervisor park waits for a supervisor) or host execution (every
/// agent step waits for a host task). The refusal is structured like every
/// gate refusal (`policy: unattended_unsupported`, a `requirement` and a
/// `detail`); its start action is a `fix_requirement` with a manual
/// remedy. The CLI to host fallback is not a refusal: an unattended start
/// resolves its execution without a host session ([`execution_request`]).
pub fn check_start(root: &Path, start: &UnattendedStart<'_>) -> Result<(), Value> {
    if !start.unattended {
        return Ok(());
    }
    let refuse = |requirement: &str, detail: String| {
        Err(json!({
            "policy": UNATTENDED_UNSUPPORTED,
            "requirement": requirement,
            "detail": detail,
        }))
    };
    if let Some(detail) = step_refusal(root) {
        return refuse("unattended_caller", detail);
    }
    if start.mode.parks_on_failure() {
        return refuse(
            "autonomous_mode",
            "an unattended run cannot be supervised: a supervisor park waits for a supervisor; start it without supervision".into(),
        );
    }
    let request = execution_request(start.execution, true);
    match apb_core::execution::resolve_for(root, &request) {
        Ok(r) if r.mode == ExecutionMode::Host => refuse(
            "cli_execution",
            "an unattended run cannot use host execution: every agent step would wait for a host task; start it with execution: cli".into(),
        ),
        Ok(_) => Ok(()),
        // The engine refuses an execution that does not resolve on its own.
        Err(_) => Ok(()),
    }
}

/// Who may choose unattended mode: a person (a terminal, the dashboard),
/// a CI job, an agent session that is not a run step, and a step of a run
/// that is itself unattended. A run step is never a person, and a step of
/// an attended run (or a host task, which only an attended run has) would
/// take the person that run waits on out of the gates of the run it
/// starts, so its unattended start is refused. The step's run is the one
/// `APB_RUN_DIR` names when its id matches `APB_RUN_ID`, else
/// `<root>/.apb/runs/<id>`; one whose mode cannot be read counts as
/// attended (fail-closed).
fn step_refusal(root: &Path) -> Option<String> {
    let step = apb_core::step_env::StepCaller::from_env();
    let from = step.describe()?;
    let unattended = step.run_id.as_deref().is_some_and(|id| {
        let named = std::env::var_os("APB_RUN_DIR")
            .map(std::path::PathBuf::from)
            .filter(|d| d.file_name().is_some_and(|n| n == id));
        let dir = named.unwrap_or_else(|| root.join(".apb/runs").join(id));
        crate::manifest::read(&dir)
            .ok()
            .flatten()
            .is_some_and(|m| m.unattended)
    });
    (!unattended).then(|| {
        format!(
            "an unattended start is refused {from}: a step of an attended run does not take the person that run waits on out of the gates of another; start it attended, or start the outer run unattended"
        )
    })
}

fn source_label(src: apb_core::policy_file::PolicySource) -> &'static str {
    match src {
        apb_core::policy_file::PolicySource::Machine => "machine",
        apb_core::policy_file::PolicySource::Project => "project",
    }
}

/// The scope label of a pinned sub-playbook.
fn pin_scope(pin: &crate::run_config::ChildExpectation) -> &'static str {
    match pin.scope {
        apb_core::profile::ProfileScope::Global => "global",
        _ => "project",
    }
}

/// The probation of the loaded definition `loaded` (scope label `scope`)
/// for the project at `root`; `Ok(None)` when nothing asks for one: not
/// the playbook, and neither `policy.yaml` file. `Err` names a
/// `policy.yaml` that does not read: what it asks for cannot be known,
/// so the caller fails closed.
pub fn probation(
    root: &Path,
    scope: &str,
    loaded: &apb_core::registry::LoadedPlaybook,
) -> Result<Option<ProbationView>, String> {
    let policies = apb_core::policy_file::Policies::load(root).map_err(|e| e.to_string())?;
    Ok(probation_with(root, scope, loaded, &policies))
}

/// [`probation`] with the settings already read, once for a whole start.
fn probation_with(
    root: &Path,
    scope: &str,
    loaded: &apb_core::registry::LoadedPlaybook,
    policies: &apb_core::policy_file::Policies,
) -> Option<ProbationView> {
    let playbook_required = loaded
        .playbook
        .unattended
        .map(|u| u.after_attended_successes);
    let floor = policies.probation_floor();
    let (required, source) = match (playbook_required, floor) {
        (Some(p), Some((f, src))) if f > p => (f, source_label(src)),
        (Some(p), _) => (p, "playbook"),
        (None, Some((f, src))) => (f, source_label(src)),
        (None, None) => return None,
    };
    let waived = policies.waives_probation();
    let ledger = apb_core::probation::read(
        root,
        apb_core::scope::Scope::parse(scope).unwrap_or(apb_core::scope::Scope::Project),
        &loaded.playbook.id,
    );
    let (counted, ledger_ok) = match ledger {
        Ok(loaded_ledger) if loaded_ledger.signed => {
            let trust = loaded.trust_digest().ok();
            let definition = crate::run_definitions::digest(loaded.yaml.as_bytes());
            (
                trust
                    .map(|t| apb_core::probation::counted(&loaded_ledger.file, &t, &definition))
                    .unwrap_or(0),
                true,
            )
        }
        _ => (0, false),
    };
    Some(ProbationView {
        required,
        counted,
        source: source.to_string(),
        waived,
        ledger_ok,
    })
}

/// The probation ledger file of playbook `id` under scope label `scope`.
fn ledger_path(root: &Path, scope: &str, id: &str) -> std::path::PathBuf {
    apb_core::signed_store::dir(
        root,
        <apb_core::probation::ProbationFile as apb_core::signed_store::SignedDoc>::DIR,
        apb_core::scope::Scope::parse(scope).unwrap_or(apb_core::scope::Scope::Project),
    )
    .join(format!("{id}.json"))
}

fn probation_refusal(detail: String) -> Value {
    json!({
        "policy": PROBATION_REQUIRED,
        "requirement": "attended_successes",
        "detail": detail,
    })
}

/// The refusal of one definition's probation, `None` when it passes:
/// done, or waived by the start (`waive`, the typed confirmation) or by
/// the machine's `policy.yaml` (`waived`), which passes an unverifiable
/// ledger too.
fn one_refusal(
    p: Option<&ProbationView>,
    waive: bool,
    what: &str,
    version: &str,
    path: &Path,
) -> Option<Value> {
    let p = p?;
    if waive || p.waived {
        return None;
    }
    if !p.ledger_ok {
        return Some(probation_refusal(format!(
            "the probation ledger of {what} in this project ({}) does not verify, so its probation cannot be counted; move the file aside (the count starts again from zero) or waive probation at a terminal with --waive-probation",
            path.display()
        )));
    }
    (p.counted < p.required).then(|| {
        probation_refusal(format!(
            "{what} {version} runs unattended only after {} successful attended runs (started without --unattended) of this definition in this project; {} so far here (probation is counted per project from its probation ledger, also for a global playbook; runs started from a run step and runs that ended before this apb kept the ledger do not count). Run it attended in this project, or waive probation at a terminal with --waive-probation",
            p.required, p.counted
        ))
    })
}

/// Refuses an unattended start whose probation, or the probation of a
/// sub-playbook it pins, is not done and not waived. One check in
/// preparation, on the definition preparation itself loaded; no launch
/// surface decides it on a definition of its own. The settings are read
/// once; a `policy.yaml` that does not read refuses (fail closed) unless
/// the start is waived. A start that carries run overrides (`overridden`)
/// runs another executor than the one its probation counted and is
/// refused unless waived.
pub fn check_probation(
    root: &Path,
    scope: &str,
    loaded: &apb_core::registry::LoadedPlaybook,
    pins: Option<&BTreeMap<String, crate::run_config::ChildExpectation>>,
    waive: bool,
    overridden: bool,
) -> Result<(), Value> {
    if waive {
        return Ok(());
    }
    let policies = apb_core::policy_file::Policies::load(root).map_err(|e| {
        probation_refusal(format!(
            "{e}; the probation it may ask for cannot be known, so an unattended start is refused until it is fixed, or waive probation at a terminal with --waive-probation"
        ))
    })?;
    if overridden && !policies.waives_probation() {
        return Err(probation_refusal(
            "an unattended start runs the definition its probation counted; start it without --overrides or waive probation at a terminal".into(),
        ));
    }
    // The playbook itself.
    let own = probation_with(root, scope, loaded, &policies);
    if let Some(refusal) = one_refusal(
        own.as_ref(),
        false,
        &format!("playbook `{}`", loaded.playbook.id),
        &loaded.version,
        &ledger_path(root, scope, &loaded.playbook.id),
    ) {
        return Err(refusal);
    }
    // Every pinned sub-playbook, in the parent's node order, each with
    // its own definition, scope and ledger.
    let Some(pins) = pins else {
        return Ok(());
    };
    let mut ordered: Vec<(&String, &crate::run_config::ChildExpectation)> = loaded
        .playbook
        .nodes
        .iter()
        .filter_map(|n| pins.get_key_value(&n.id))
        .collect();
    for (k, v) in pins {
        if !ordered.iter().any(|(o, _)| *o == k) {
            ordered.push((k, v));
        }
    }
    for (node, pin) in ordered {
        let Some(child) = pin.load_definition(root) else {
            return Err(probation_refusal(format!(
                "sub-playbook `{}` (node `{node}`) cannot be read to count its probation; start the run again once it reads",
                pin.id
            )));
        };
        let child_scope = pin_scope(pin);
        let view = probation_with(root, child_scope, &child, &policies);
        if let Some(refusal) = one_refusal(
            view.as_ref(),
            false,
            &format!("sub-playbook `{}` {}", pin.id, child.version),
            &format!("(node `{node}`)"),
            &ledger_path(root, child_scope, &pin.id),
        ) {
            return Err(refusal);
        }
    }
    Ok(())
}

/// The execution request an unattended start resolves: the same, without a
/// host session, so the CLI to host fallback is off.
pub fn execution_request(request: &ExecutionRequest, unattended: bool) -> ExecutionRequest {
    let mut out = request.clone();
    if unattended {
        out.host_session = false;
    }
    out
}

/// Why the declared `decide: option` of `gate` is refused at run time, or
/// `None` when the run may decide it unattended. The rule is the
/// validator's own (`apb_core::validate::unattended_decide_refusal`, V142),
/// with what only the run knows: the effects of every sub-playbook node in
/// the option's reach at its pinned version (`child_effects` of the node,
/// `None` when they cannot be known), and the connectors as installed here,
/// where one whose effects are unknown refuses. Fail-closed: a child whose
/// effects cannot be known refuses.
pub fn decide_refusal(
    playbook: &Playbook,
    gate: &str,
    option: &str,
    child_effects: &dyn Fn(&str) -> Option<BTreeSet<Effect>>,
    connectors: &BTreeMap<String, ConnectorFacts>,
) -> Option<String> {
    let mut inherited = BTreeSet::new();
    for id in apb_core::validate::unattended_reach(playbook, gate, option) {
        if !matches!(
            playbook.node(&id).map(|n| &n.kind),
            Some(NodeKind::Playbook { .. })
        ) {
            continue;
        }
        match child_effects(&id) {
            Some(effects) => inherited.extend(effects),
            None => {
                return Some(format!(
                    "the effects of sub-playbook node `{id}` are unknown"
                ));
            }
        }
    }
    let inherited: Vec<Effect> = inherited.into_iter().collect();
    apb_core::validate::unattended_decide_refusal(
        playbook, gate, option, &inherited, connectors, true,
    )
}

/// [`decide_refusal`] for the run at `run_dir`: a sub-playbook in reach
/// weighs the version the run's gate pinned (`run.yaml`), the one the run
/// executes; without a pin its effects are unknown (fail-closed); the
/// connectors are the ones installed here. The one recheck the drive makes
/// before it posts the decision and the next actions show as the
/// `unattended` outcome of a pending gate.
pub(crate) fn run_decide_refusal(
    root: &Path,
    run_dir: &Path,
    playbook: &Playbook,
    gate: &str,
    option: &str,
) -> Option<String> {
    let pins = crate::run_config::read_run_config(run_dir)
        .ok()
        .and_then(|c| c.expected_children);
    let child_effects = |node: &str| {
        let pin = pins.as_ref()?.get(node)?;
        crate::gate::pinned_tree_effects(root, &pin.load(root)?, &pin.children)
    };
    decide_refusal(
        playbook,
        gate,
        option,
        &child_effects,
        &apb_core::connector::resolve::validation_facts(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn playbook(yaml: &str) -> Playbook {
        Playbook::from_yaml(yaml).unwrap()
    }

    const GATED: &str = r#"
schema: 2
id: p
name: p
version: 1.0.0
nodes:
  - { id: start, type: start }
  - { id: gate, type: human_review, options: [approve, reject] }
  - { id: work, type: prompt, prompt: x }
  - { id: ship, type: prompt, prompt: x, effects: [irreversible] }
  - { id: second, type: human_review }
  - { id: after, type: prompt, prompt: x, effects: [irreversible] }
  - { id: done, type: finish, outcome: success }
edges:
  - { from: start, to: gate }
  - { from: gate, to: work, condition: { type: review_status, equals: approve } }
  - { from: gate, to: ship, condition: { type: review_status, equals: reject } }
  - { from: work, to: second }
  - { from: second, to: after }
  - { from: after, to: done }
  - { from: ship, to: done }
"#;

    #[test]
    fn a_risky_node_behind_another_option_or_another_gate_does_not_refuse() {
        let p = playbook(GATED);
        let facts = BTreeMap::new();
        let none = |_: &str| None;
        assert_eq!(decide_refusal(&p, "gate", "approve", &none, &facts), None);
        let why = decide_refusal(&p, "gate", "reject", &none, &facts).unwrap();
        assert!(why.contains("`ship`"), "{why}");
    }
}
