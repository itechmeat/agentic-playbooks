//! Where one attempt of an agent_task runs, and inside what: the node's
//! `isolation` evaluated against this machine (`apb_core::isolation::
//! contract`) once per execution and again before every attempt, so a
//! resume or a bwrap that vanished mid-run fails closed.
//!
//! - No isolation: the node's own directory, as always.
//! - A sandbox (bwrap): the working directory is the node's tree; the
//!   attempt's status file and skill copies live in its own box under the
//!   run (`sandbox/attempts/<stem>`), bound into the sandbox. The status
//!   file is copied to `agent-status/<stem>.json` once the attempt is over,
//!   where every reader finds it.
//! - `best_effort` without a sandbox: a fresh scratch directory
//!   `work/<node>/<attempt>` with the skill copies, as before. That is also
//!   where a host step runs (the host fallback), and where an attempt whose
//!   sandbox could not be set up runs.
//!
//! A `full` node this machine cannot isolate fails before anything is
//! spawned, with `isolation_unavailable: <reasons>` as its output.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use apb_core::agents::SandboxNeeds;
use apb_core::isolation::IsolationSpec;
use apb_core::isolation::contract::{self, Backend, Contract, NodeFacts};
use apb_core::process_env::EnvPlan;

use crate::error::EngineError;
use crate::event::{AttemptIsolation, ISOLATION_VIEW_CAP, IsolationViewEntry};
use crate::exec_key::ExecKey;
use crate::manifest::ManifestProfile;
use crate::sandbox::SandboxPlan;

use contract::UNAVAILABLE;

/// A node's isolation for one execution.
pub(super) struct NodeIsolation<'a> {
    spec: Option<&'a IsolationSpec>,
    facts: Option<NodeFacts>,
}

impl<'a> NodeIsolation<'a> {
    /// `agents`: the canonical agents of the chain's CLI steps.
    pub fn new<'b>(
        spec: Option<&'a IsolationSpec>,
        agents: impl Iterator<Item = &'b str>,
        host_mode: bool,
    ) -> Self {
        let spec = spec.filter(|s| s.is_isolated());
        let facts = spec.map(|s| NodeFacts {
            level: s.level,
            network: s.network(),
            agents_without_needs: without_needs(agents),
            host_mode,
        });
        NodeIsolation { spec, facts }
    }

    /// Whether the node runs isolated at all.
    pub fn isolated(&self) -> bool {
        self.spec.is_some()
    }

    /// The contract on this machine now; `None` without isolation.
    pub fn contract(&self) -> Option<Contract> {
        self.facts
            .as_ref()
            .map(|f| contract::evaluate(&crate::sandbox::probe::host_facts(), f))
    }

    /// Whether the node's attempts run in a sandbox (decided now).
    pub fn sandboxed(&self) -> bool {
        self.contract().is_some_and(|c| c.backend == Backend::Bwrap)
    }

    /// The failed node's output when the contract refuses the node now.
    pub fn refusal(&self) -> Option<String> {
        self.contract()
            .filter(|c| !c.allowed())
            .map(|c| unavailable(&c.refusals))
    }
}

/// The agents of `agents` whose module declares no sandbox needs (an agent
/// apb does not know declares none).
fn without_needs<'b>(agents: impl Iterator<Item = &'b str>) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for a in agents {
        let id = apb_core::agents::canonical_id(a).to_string();
        if needs_of(&id).is_none() && !out.contains(&id) {
            out.push(id);
        }
    }
    out
}

fn needs_of(agent: &str) -> Option<SandboxNeeds> {
    apb_core::agents::lookup(agent)?.setup().sandbox_needs()
}

fn unavailable(reasons: &[contract::Reason]) -> String {
    let list: Vec<String> = reasons.iter().map(ToString::to_string).collect();
    format!("{UNAVAILABLE}: {}", list.join("; "))
}

/// Where and how one attempt runs.
pub(super) struct AttemptPlace {
    /// The attempt's working directory.
    pub cwd: PathBuf,
    /// The attempt's status file (`APB_STATUS_FILE`).
    pub status_file: PathBuf,
    /// Where readers look for the status file when the attempt wrote it in
    /// its box: `agent-status/<stem>.json`.
    pub published_status: Option<PathBuf>,
    pub sandbox: Option<Arc<SandboxPlan>>,
    /// The `attempt_started.isolation` record; `None` without isolation.
    pub record: Option<AttemptIsolation>,
}

impl AttemptPlace {
    /// Copies a boxed status file to where every reader looks for it. An
    /// attempt that wrote none has nothing to publish.
    pub fn publish_status(&self) -> std::io::Result<()> {
        let Some(dst) = &self.published_status else {
            return Ok(());
        };
        match std::fs::read(&self.status_file) {
            Ok(bytes) => apb_core::fsutil::atomic_write(dst, &bytes),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(e),
        }
    }
}

impl Drop for AttemptPlace {
    /// The mount points apb created for the attempt go once it is over.
    fn drop(&mut self) {
        if let Some(plan) = &self.sandbox {
            plan.cleanup();
        }
    }
}

/// The attempt's place, or the refusal that fails the node.
pub(super) enum Placed {
    Run(Box<AttemptPlace>),
    Refused(String),
}

/// What [`place`] needs of the attempt.
pub(super) struct AttemptInput<'a> {
    pub run_dir: &'a Path,
    pub node_dir: &'a Path,
    pub node_id: &'a str,
    pub key: &'a ExecKey<'a>,
    pub attempt: u32,
    pub entry: &'a ManifestProfile,
    /// The directory of the session the attempt continues, if any.
    pub continued: Option<&'a Path>,
    pub agent: &'a str,
    /// The agent program the attempt runs.
    pub program: &'a str,
    /// The step's `process_env` plan.
    pub process: Option<&'a EnvPlan>,
    /// claude's apb-owned settings file, when the step has one.
    pub settings: Option<&'a Path>,
    /// The step runs in the host agent (the host fallback).
    pub host: bool,
}

/// Decides where attempt `input` runs, laying down its directory, its skill
/// copies and, in a sandbox, the sandbox.
pub(super) fn place(iso: &NodeIsolation, input: &AttemptInput) -> Result<Placed, EngineError> {
    let Some(contract) = iso.contract() else {
        return Ok(Placed::Run(Box::new(AttemptPlace {
            cwd: input
                .continued
                .map_or_else(|| input.node_dir.to_path_buf(), Path::to_path_buf),
            status_file: status_file(input)?,
            published_status: None,
            sandbox: None,
            record: None,
        })));
    };
    let spec = iso.spec.expect("an isolated node has a spec");
    // A host step runs in the host agent, outside anything apb starts.
    let contract = if input.host && contract.allowed() {
        contract::without_sandbox(spec.level, vec![host_step()])
    } else {
        contract
    };
    if !contract.allowed() {
        return Ok(Placed::Refused(unavailable(&contract.refusals)));
    }
    let passed = input.process.map_or_else(Vec::new, |p| {
        let logins = needs_of(input.agent)
            .map(|n| n.env_logins)
            .unwrap_or_default();
        apb_core::process_env::passed_credentials(p, &logins)
    });
    if contract.backend == Backend::None {
        return unsandboxed(input, spec, &contract, &passed);
    }
    let attempt_box = attempt_box(input)?;
    // Always laid down (empty when the profile grants none), so the tree's
    // own skills are covered either way.
    super::super::skills_copy::materialize_isolated_skills(
        input.run_dir,
        input.entry,
        &attempt_box,
    )?;
    let needs = needs_of(input.agent).unwrap_or_default();
    let login_from_env = input
        .process
        .is_some_and(|p| needs.env_logins.iter().any(|n| p.keeps(n)));
    let host = crate::sandbox::probe::host_facts();
    let built = crate::sandbox::build(&crate::sandbox::Request {
        host: &host,
        run_dir: input.run_dir,
        tree: input.node_dir,
        agent: apb_core::agents::canonical_id(input.agent),
        node: input.node_id,
        attempt_box: &attempt_box,
        program: input.program,
        spec,
        scope: contract.scope,
        needs: &needs,
        login_from_env,
        run_files: input.settings.map(Path::to_path_buf).into_iter().collect(),
        skills: Some(attempt_box.join(".agents/skills")),
    });
    let plan = match built {
        Ok(plan) => plan,
        Err(why) => {
            let reason = contract::Reason::new(contract::ReasonCode::SandboxSetup, why);
            let degraded = contract::without_sandbox(spec.level, vec![reason]);
            if !degraded.allowed() {
                return Ok(Placed::Refused(unavailable(&degraded.refusals)));
            }
            return unsandboxed(input, spec, &degraded, &passed);
        }
    };
    Ok(Placed::Run(Box::new(AttemptPlace {
        cwd: plan.cwd.clone(),
        status_file: attempt_box.join("status.json"),
        // Cleared now, so a verdict a prior execution left there is never
        // read for this attempt.
        published_status: Some(status_file(input)?),
        record: Some(record(spec, &contract, Some(&plan), &passed)),
        sandbox: Some(Arc::new(plan)),
    })))
}

/// The reason a host step of an isolated node runs without a sandbox.
fn host_step() -> contract::Reason {
    contract::Reason::new(
        contract::ReasonCode::HostMode,
        "the host fallback runs in the host agent",
    )
}

/// An isolated attempt that runs without a sandbox: a scratch directory.
fn unsandboxed(
    input: &AttemptInput,
    spec: &IsolationSpec,
    contract: &Contract,
    passed: &[String],
) -> Result<Placed, EngineError> {
    Ok(Placed::Run(Box::new(AttemptPlace {
        cwd: scratch_dir(input)?,
        status_file: status_file(input)?,
        published_status: None,
        sandbox: None,
        record: Some(record(spec, contract, None, passed)),
    })))
}

/// The status file outside a sandbox: `agent-status/<stem>.json`, any file
/// a prior execution left at that path removed (a resume or a re-run can
/// restart the attempt counter, so only a file THIS attempt wrote may be
/// adopted).
fn status_file(input: &AttemptInput) -> Result<PathBuf, EngineError> {
    let dir = input.run_dir.join("agent-status");
    apb_core::fsutil::create_dir_under(input.run_dir, &dir)?;
    let file = dir.join(format!("{}.json", input.key.stem(input.attempt)));
    let _ = std::fs::remove_file(&file);
    Ok(file)
}

/// The attempt's own box under the run, emptied: the status file and the
/// skill copies of a sandboxed attempt.
fn attempt_box(input: &AttemptInput) -> Result<PathBuf, EngineError> {
    let parent = input.run_dir.join("sandbox").join("attempts");
    apb_core::fsutil::create_dir_under(input.run_dir, &parent)?;
    let dir = parent.join(input.key.stem(input.attempt));
    clear(&dir)?;
    std::fs::create_dir_all(&dir)?;
    Ok(dir)
}

/// The working directory of `best_effort` without a sandbox. A continued
/// session keeps the directory it was started in (agents key their
/// sessions by it); otherwise a FRESH per-attempt directory
/// `work/<node>/<attempt>`. Either way the skill copies are laid down fresh
/// from the snapshot, so an earlier attempt cannot hand a modified bundle to
/// the next one.
fn scratch_dir(input: &AttemptInput) -> Result<PathBuf, EngineError> {
    use super::super::skills_copy::{clear_isolated_skills, materialize_isolated_skills};
    let scratch_root = input.run_dir.join("work").join(input.node_id);
    // Only a session that itself ran in a scratch directory is continued
    // there; one that ran in the tree (a sandboxed attempt) starts afresh,
    // so the tree's own skills are never cleared or overwritten.
    if let Some(wd) = input.continued.filter(|wd| lies_under(wd, &scratch_root)) {
        clear_isolated_skills(wd)?;
        materialize_isolated_skills(input.run_dir, input.entry, wd)?;
        return Ok(wd.to_path_buf());
    }
    let wd = scratch_root.join(input.key.work_segment(input.attempt));
    clear(&wd)?;
    materialize_isolated_skills(input.run_dir, input.entry, &wd)?;
    Ok(wd)
}

/// Whether `path` lies under `root`, comparing resolved paths when both
/// exist: a session's directory is recorded as the agent saw it, which on
/// macOS is the `/private/var` form of a `/var` temp path.
fn lies_under(path: &Path, root: &Path) -> bool {
    if path.starts_with(root) {
        return true;
    }
    match (path.canonicalize(), root.canonicalize()) {
        (Ok(p), Ok(r)) => p.starts_with(r),
        _ => false,
    }
}

/// Removes `dir`. Fail-closed: a missing directory is normal, any other
/// error is not swallowed, so nothing is laid down on top of what an
/// earlier (possibly hostile) attempt left.
fn clear(dir: &Path) -> Result<(), EngineError> {
    match std::fs::remove_dir_all(dir) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e.into()),
    }
}

/// The attempt's `isolation` record.
fn record(
    spec: &IsolationSpec,
    c: &Contract,
    plan: Option<&SandboxPlan>,
    passed: &[String],
) -> AttemptIsolation {
    let mut limits: Vec<String> = c.limits.iter().map(|l| l.id().to_string()).collect();
    limits.extend(
        passed
            .iter()
            .map(|n| format!("{}{n}", contract::ENV_PASSED_PREFIX)),
    );
    AttemptIsolation {
        level: spec.level.as_str().to_string(),
        backend: match c.backend {
            Backend::Bwrap => "bwrap",
            Backend::None => "none",
        }
        .to_string(),
        bwrap_version: plan.and_then(|p| p.bwrap_version.clone()),
        // Without a sandbox the attempt has the host network, whatever the
        // node asked for.
        network: match c.backend {
            Backend::Bwrap => spec.network(),
            Backend::None => apb_core::isolation::Network::Open,
        }
        .as_str()
        .to_string(),
        holds: c.holds.iter().map(|h| h.id().to_string()).collect(),
        limits,
        fallbacks: c.fallbacks.iter().map(ToString::to_string).collect(),
        view: plan.map_or_else(Vec::new, |p| {
            p.shown(ISOLATION_VIEW_CAP)
                .into_iter()
                .map(|(path, access)| IsolationViewEntry {
                    path,
                    access: access.to_string(),
                })
                .collect()
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::lies_under;

    /// A session directory recorded through a link (macOS reports `/var`
    /// temp paths as `/private/var`) still lies under the run's scratch root.
    #[cfg(unix)]
    #[test]
    fn a_session_directory_seen_through_a_link_lies_under_its_root() {
        let dir = tempfile::tempdir().unwrap();
        let real = dir.path().join("real");
        let session = real.join("work/node/1");
        std::fs::create_dir_all(&session).unwrap();
        let link = dir.path().join("link");
        std::os::unix::fs::symlink(&real, &link).unwrap();
        assert!(lies_under(&session, &link.join("work/node")));
        assert!(lies_under(
            &link.join("work/node/1"),
            &real.join("work/node")
        ));
        let other = dir.path().join("other");
        std::fs::create_dir_all(&other).unwrap();
        assert!(!lies_under(&other, &link.join("work/node")));
    }
}
