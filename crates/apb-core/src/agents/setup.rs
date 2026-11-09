//! What an agent needs from the host and what the host tells about it:
//! where detection finds its models and login, which variables its process
//! keeps under `process_env: allowlist`, and the run-scoped files it is
//! pointed at.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

use super::output::CostBasis;
use super::sandbox::SandboxNeeds;
use super::types::{AgentEnvSet, AuthHint, Authority};

/// One agent's host facts and needs. Every hook defaults to "nothing".
pub trait HostSetup: Send + Sync {
    /// The arguments that make the agent list its models, with how
    /// authoritative that list is; `None` when the agent has no free listing.
    fn models_command(&self) -> Option<(&'static [&'static str], Authority)> {
        None
    }

    /// The login hint and the provider names the agent's files under `home`
    /// show. Secret values are never read: only a file's presence, or key
    /// names.
    fn auth_hint(&self, _home: &Path) -> (Option<AuthHint>, Option<Vec<String>>) {
        (None, None)
    }

    /// What the agent's own files tell detection beyond its login: provider
    /// names and notes. `installed` says whether the binary was found.
    fn host_notes(&self, _home: Option<&Path>, _installed: bool) -> HostNotes {
        HostNotes::default()
    }

    /// The files under `home` detection reads for this agent (its own and
    /// the ones its model listing reads); editing one invalidates the
    /// detection memo.
    fn memo_files(&self, _home: &Path) -> Vec<PathBuf> {
        Vec::new()
    }

    /// An environment variable whose presence (never its value) changes the
    /// login hint, with the label the detection memo key gives it.
    fn memo_env(&self) -> Option<(&'static str, &'static str)> {
        None
    }

    /// The program apb launches when the config names none, when it is not
    /// simply the first binary of the descriptor.
    fn default_program(&self) -> Option<String> {
        None
    }

    /// Whether a binary found on PATH under the agent's name is something
    /// else that must not be probed or launched.
    fn rejects_binary(&self, _path: &Path) -> bool {
        false
    }

    /// A warning `apb doctor` prints for an installed agent whose login hint
    /// shows it cannot run headless.
    fn login_warning(&self, _auth: Option<&AuthHint>) -> Option<&'static str> {
        None
    }

    /// The agent's own record of session `session` (a safe path segment),
    /// when it keeps one apb can read.
    fn session_record(&self, _session: &str) -> Option<PathBuf> {
        None
    }

    /// The variables the agent CLI reads, kept under `process_env:
    /// allowlist`.
    fn env_set(&self) -> AgentEnvSet {
        AgentEnvSet::NONE
    }

    /// The variables the agent reads for `model` (a provider's key).
    fn model_env(&self, _model: &str) -> &'static [&'static str] {
        &[]
    }

    /// Exact variable names the agent's own configuration names (custom
    /// providers' keys). Names only.
    fn derived_env(&self) -> Vec<String> {
        Vec::new()
    }

    /// The run-scoped environment of one spawn of the agent on `model`:
    /// files prepared in `scoped_dir` (the run's agent home for the node, or
    /// `None` outside a run) and the variables that point the agent at them.
    fn scoped_env(
        &self,
        _model: &str,
        _scoped_dir: Option<&Path>,
    ) -> Result<Vec<(String, PathBuf)>, ScopedEnvError> {
        Ok(Vec::new())
    }

    /// The leading arguments of one spawn of the agent on `model`, put in
    /// front of every form's own (start and resume alike): a file prepared
    /// in `scoped_dir` (as for [`Self::scoped_env`]) that the agent only
    /// takes as an argument. dsh's model patch (`--patch <file>`) is one.
    fn scoped_args(
        &self,
        _model: &str,
        _scoped_dir: Option<&Path>,
    ) -> Result<Vec<String>, ScopedEnvError> {
        Ok(Vec::new())
    }

    /// What the agent needs inside an isolation sandbox (its login), `None`
    /// when nobody has verified a sandboxed run of it: `isolation: full`
    /// then refuses it and `best_effort` runs it without a sandbox.
    fn sandbox_needs(&self) -> Option<SandboxNeeds> {
        None
    }

    /// What a cost this agent reports is, from facts about the spawned
    /// process (see [`CostFacts`]); `None` when the module cannot tell it
    /// reliably, and the person's declared subscription decides
    /// (`crate::agents::cost_basis::resolve`).
    fn cost_basis(&self, _facts: &CostFacts) -> Option<CostReading> {
        None
    }
}

/// What a module may read about the process an attempt spawned, to tell its
/// cost basis (`crate::agents::cost_basis`). `present` says whether a
/// variable reaches the process (never its value); `setting` gives the value
/// of a variable that is a path or a switch, and must not be asked for a
/// credential; `home` is the operator's home directory; `workdir` is where
/// the agent runs; `hermetic` says the agent runs in apb's minimal
/// environment (its module's `hermetic_settings`), which leaves the user's
/// own settings out; `system_root` is the root the machine-wide files
/// (managed settings) are read under, `/` outside apb's own tests.
pub struct CostFacts<'a> {
    pub present: &'a dyn Fn(&str) -> bool,
    pub setting: &'a dyn Fn(&str) -> Option<OsString>,
    pub home: Option<&'a Path>,
    pub workdir: Option<&'a Path>,
    pub hermetic: bool,
    pub system_root: &'a Path,
}

/// A basis with a digest of every fact it was decided from (never a
/// credential value), so the readings taken before an attempt's spawn and
/// after its exit can be compared ([`CostReading::across`], `crate::agents::cost_basis`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CostReading {
    pub basis: CostBasis,
    evidence: String,
}

impl CostReading {
    pub fn new(basis: CostBasis, evidence: String) -> Self {
        CostReading { basis, evidence }
    }

    /// The basis of an attempt read before its spawn and after its exit:
    /// the one both readings agree on, from the same facts. A login or a
    /// settings file that changed while the agent worked cannot tell how
    /// its earlier calls were paid for, so the basis is unknown.
    pub fn across(before: &CostReading, after: &CostReading) -> CostBasis {
        if before == after {
            before.basis
        } else {
            CostBasis::Unknown
        }
    }
}

/// The setup of an agent that needs nothing.
pub struct NoSetup;

impl HostSetup for NoSetup {}

/// Provider names and notes an agent's files give detection.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct HostNotes {
    pub providers: Option<Vec<String>>,
    pub notes: Vec<String>,
}

/// A run-scoped environment that could not be prepared: what was being
/// prepared, and why it failed.
#[derive(Debug)]
pub struct ScopedEnvError {
    pub what: String,
    pub source: std::io::Error,
}

impl std::fmt::Display for ScopedEnvError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "prepare {} failed: {}", self.what, self.source)
    }
}

impl std::error::Error for ScopedEnvError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.source)
    }
}
