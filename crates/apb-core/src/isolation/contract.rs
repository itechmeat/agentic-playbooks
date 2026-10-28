//! The isolation contract: what this machine can enforce for one node, as
//! a pure function of the host's facts and the node's. `full` refuses when
//! any line of the contract fails, `best_effort` runs and lists what it
//! could not enforce, `none` is untouched. The gate, prepare, every attempt,
//! fit and the doctor all ask [`evaluate`], so they never disagree.
//!
//! Every code has one English sentence ([`text`]) that every surface shows.

use std::path::PathBuf;

use super::{Level, Network};

/// The refusal every surface reports for a `full` node this machine cannot
/// isolate: the gate's `policy`, prepare's error and a refused node's
/// output prefix.
pub const UNAVAILABLE: &str = "isolation_unavailable";

/// The operating system apb runs on, as far as isolation cares.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Os {
    Linux,
    MacOs,
    Other,
}

impl Os {
    /// The system this binary was built for.
    pub fn current() -> Self {
        if cfg!(target_os = "linux") {
            Os::Linux
        } else if cfg!(target_os = "macos") {
            Os::MacOs
        } else {
            Os::Other
        }
    }
}

/// What the probe learned about bubblewrap.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BwrapFacts {
    pub path: PathBuf,
    /// `bwrap --version`, without the program name.
    pub version: Option<String>,
    /// The binary is setuid root (refused: apb uses unprivileged bwrap).
    pub setuid: bool,
    /// One trial sandbox: `Ok` when user namespaces work, else bwrap's own
    /// reason.
    pub trial: Result<(), String>,
}

/// The host's side of the contract.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostFacts {
    pub os: Os,
    pub bwrap: Option<BwrapFacts>,
    /// The kernel's Landlock ABI, 0 when Landlock is unavailable.
    pub landlock_abi: u32,
    /// `APB_SANDBOX=off`: the sandbox may not be used.
    pub disabled: bool,
}

/// The Landlock ABI that scopes abstract unix sockets and signals.
pub const SCOPE_ABI: u32 = 6;

impl HostFacts {
    /// Whether the helper can apply the Landlock scope.
    pub fn scope_available(&self) -> bool {
        self.landlock_abi >= SCOPE_ABI
    }
}

/// The node's side of the contract.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NodeFacts {
    pub level: Level,
    pub network: Network,
    /// The agents of the node's executor chain (primary and fallbacks)
    /// whose module declares no sandbox needs.
    pub agents_without_needs: Vec<String>,
    /// The run executes its agent steps in the host (host mode).
    pub host_mode: bool,
}

/// The mechanism that enforces the contract.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Backend {
    Bwrap,
    None,
}

/// What a node's attempt is guaranteed (`holds`) or not (`limits`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Code {
    // holds
    UserNamespaces,
    UserNoPrivileges,
    UserNewSession,
    FilesDeclaredView,
    FilesGitControlReadOnly,
    FilesApbMasked,
    CredentialsEnvAllowlist,
    CredentialsSocketsDropped,
    CredentialsGitConfigReplaced,
    NetworkNone,
    NetworkScoped,
    // limits
    NoSandbox,
    UserHostUid,
    UserNotSeparated,
    FilesSystemReadable,
    FilesNotSeparated,
    CredentialsAgentLoginReadable,
    CredentialsHomeReadable,
    NetworkOpenAnyHost,
    NetworkAbstractSocketsOpen,
    NetworkNotSeparated,
    KernelEscapes,
    ScriptsOutside,
}

/// Every code, for the exported table.
pub const ALL_CODES: [Code; 23] = [
    Code::UserNamespaces,
    Code::UserNoPrivileges,
    Code::UserNewSession,
    Code::FilesDeclaredView,
    Code::FilesGitControlReadOnly,
    Code::FilesApbMasked,
    Code::CredentialsEnvAllowlist,
    Code::CredentialsSocketsDropped,
    Code::CredentialsGitConfigReplaced,
    Code::NetworkNone,
    Code::NetworkScoped,
    Code::NoSandbox,
    Code::UserHostUid,
    Code::UserNotSeparated,
    Code::FilesSystemReadable,
    Code::FilesNotSeparated,
    Code::CredentialsAgentLoginReadable,
    Code::CredentialsHomeReadable,
    Code::NetworkOpenAnyHost,
    Code::NetworkAbstractSocketsOpen,
    Code::NetworkNotSeparated,
    Code::KernelEscapes,
    Code::ScriptsOutside,
];

/// The prefix of the limit naming a credential-shaped variable an
/// allowlist still passes: `credentials.env_passed:<NAME>`.
pub const ENV_PASSED_PREFIX: &str = "credentials.env_passed:";

impl Code {
    /// The stable id every surface and the journal carry.
    pub fn id(self) -> &'static str {
        match self {
            Code::UserNamespaces => "user.namespaces",
            Code::UserNoPrivileges => "user.no_privileges",
            Code::UserNewSession => "user.new_session",
            Code::FilesDeclaredView => "files.declared_view",
            Code::FilesGitControlReadOnly => "files.git_control_read_only",
            Code::FilesApbMasked => "files.apb_masked",
            Code::CredentialsEnvAllowlist => "credentials.env_allowlist",
            Code::CredentialsSocketsDropped => "credentials.sockets_dropped",
            Code::CredentialsGitConfigReplaced => "credentials.git_config_replaced",
            Code::NetworkNone => "network.none",
            Code::NetworkScoped => "network.scoped",
            Code::NoSandbox => "no_sandbox",
            Code::UserHostUid => "user.host_uid",
            Code::UserNotSeparated => "user.not_separated",
            Code::FilesSystemReadable => "files.system_readable",
            Code::FilesNotSeparated => "files.not_separated",
            Code::CredentialsAgentLoginReadable => "credentials.agent_login_readable",
            Code::CredentialsHomeReadable => "credentials.home_readable",
            Code::NetworkOpenAnyHost => "network.open_any_host",
            Code::NetworkAbstractSocketsOpen => "network.abstract_sockets_open",
            Code::NetworkNotSeparated => "network.not_separated",
            Code::KernelEscapes => "kernel.escapes",
            Code::ScriptsOutside => "scripts.outside",
        }
    }

    /// The one sentence every surface shows for this code.
    pub fn text(self) -> &'static str {
        match self {
            Code::UserNamespaces => {
                "Own PID, IPC and UTS namespaces: no operator process is visible, signalable or traceable."
            }
            Code::UserNoPrivileges => "No capabilities and no new privileges.",
            Code::UserNewSession => "A new session with no controlling terminal.",
            Code::FilesDeclaredView => {
                "Only the declared view of the filesystem; the home directory is hidden except declared paths."
            }
            Code::FilesGitControlReadOnly => {
                "The repository's config, hooks and info are read-only."
            }
            Code::FilesApbMasked => {
                "The project's .apb directory is masked except the attempt's own paths."
            }
            Code::CredentialsEnvAllowlist => {
                "The environment is an allowlist, even when the profile says inherit; connector variables are withheld."
            }
            Code::CredentialsSocketsDropped => {
                "The ssh agent, session bus and display variables are dropped."
            }
            Code::CredentialsGitConfigReplaced => {
                "The operator's git config is replaced by one holding only the user name and email."
            }
            Code::NetworkNone => "Own network namespace: loopback only.",
            Code::NetworkScoped => {
                "Host abstract unix sockets and signals outside the sandbox are closed (Landlock scope)."
            }
            Code::NoSandbox => "No sandbox: the agent runs as the operator in a scratch directory.",
            Code::UserHostUid => {
                "The host uid is unchanged: created files belong to the operator, and the operator's groups still apply."
            }
            Code::UserNotSeparated => "Operator processes are visible to the agent.",
            Code::FilesSystemReadable => "/usr, /etc, /opt and /sys are readable.",
            Code::FilesNotSeparated => "The agent can read and write everything the operator can.",
            Code::CredentialsAgentLoginReadable => {
                "The agent's own provider login is readable by everything in the sandbox, including code the agent runs."
            }
            Code::CredentialsHomeReadable => {
                "The operator's home directory, with its credentials, stays readable."
            }
            Code::NetworkOpenAnyHost => {
                "The network reaches any host, including services on the host's loopback."
            }
            Code::NetworkAbstractSocketsOpen => {
                "Host abstract unix sockets (X11, session bus) stay reachable: the kernel offers no Landlock scope."
            }
            Code::NetworkNotSeparated => "The network is the operator's own.",
            Code::KernelEscapes => "Kernel escapes are outside the contract.",
            Code::ScriptsOutside => {
                "Script nodes, goal scripts, project checks and baseline reads run outside the sandbox."
            }
        }
    }
}

/// The sentence of a code id as journaled, `None` for an unknown id. A
/// `credentials.env_passed:<NAME>` id has its own sentence.
pub fn text(id: &str) -> Option<&'static str> {
    if id.starts_with(ENV_PASSED_PREFIX) {
        return Some(ENV_PASSED_TEXT);
    }
    ALL_CODES.iter().find(|c| c.id() == id).map(|c| c.text())
}

/// Why `full` cannot run (or `best_effort` runs without a sandbox).
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReasonCode {
    OsUnsupported,
    BwrapMissing,
    BwrapSetuid,
    UsernsDenied,
    ScopeUnavailable,
    AgentNeedsUnknown,
    HostMode,
    Disabled,
    SandboxSetup,
}

impl ReasonCode {
    /// The wire id.
    pub fn id(self) -> &'static str {
        match self {
            ReasonCode::OsUnsupported => "os_unsupported",
            ReasonCode::BwrapMissing => "bwrap_missing",
            ReasonCode::BwrapSetuid => "bwrap_setuid",
            ReasonCode::UsernsDenied => "userns_denied",
            ReasonCode::ScopeUnavailable => "scope_unavailable",
            ReasonCode::AgentNeedsUnknown => "agent_needs_unknown",
            ReasonCode::HostMode => "host_mode",
            ReasonCode::Disabled => "disabled",
            ReasonCode::SandboxSetup => "sandbox_setup",
        }
    }

    /// The one sentence every surface shows for this reason.
    pub fn text(self) -> &'static str {
        match self {
            ReasonCode::OsUnsupported => {
                "The sandbox needs Linux with bubblewrap; this system is not supported."
            }
            ReasonCode::BwrapMissing => "Bubblewrap (bwrap) is not installed.",
            ReasonCode::BwrapSetuid => {
                "The bwrap binary is setuid root; apb uses unprivileged bwrap only."
            }
            ReasonCode::UsernsDenied => {
                "This machine refuses unprivileged user namespaces to bwrap."
            }
            ReasonCode::ScopeUnavailable => {
                "Network open under full needs the Landlock scope, which this kernel does not offer."
            }
            ReasonCode::AgentNeedsUnknown => {
                "An agent of the node's executor chain declares no sandbox needs."
            }
            ReasonCode::HostMode => {
                "A host-mode step runs in the host agent, outside any sandbox apb starts."
            }
            ReasonCode::Disabled => "The sandbox is turned off on this machine (APB_SANDBOX=off).",
            ReasonCode::SandboxSetup => {
                "The sandbox of this attempt could not be set up (a declared path, the git directory, the home or bwrap itself)."
            }
        }
    }
}

/// Every reason, for the exported table.
pub const ALL_REASONS: [ReasonCode; 9] = [
    ReasonCode::OsUnsupported,
    ReasonCode::BwrapMissing,
    ReasonCode::BwrapSetuid,
    ReasonCode::UsernsDenied,
    ReasonCode::ScopeUnavailable,
    ReasonCode::AgentNeedsUnknown,
    ReasonCode::HostMode,
    ReasonCode::Disabled,
    ReasonCode::SandboxSetup,
];

/// The sentence of the `credentials.env_passed:<NAME>` limit.
pub const ENV_PASSED_TEXT: &str =
    "A credential-shaped variable the allowlist names is passed to the agent.";

/// One reason with its detail (the bwrap error, the agent names).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Reason {
    pub code: ReasonCode,
    pub detail: String,
}

impl Reason {
    pub fn new(code: ReasonCode, detail: impl Into<String>) -> Self {
        Reason {
            code,
            detail: detail.into(),
        }
    }
}

impl std::fmt::Display for Reason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.detail.is_empty() {
            f.write_str(self.code.id())
        } else {
            write!(f, "{} ({})", self.code.id(), self.detail)
        }
    }
}

/// What one node gets on this machine.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Contract {
    pub backend: Backend,
    pub holds: Vec<Code>,
    pub limits: Vec<Code>,
    /// Non-empty only under `full`: the node must not run.
    pub refusals: Vec<Reason>,
    /// Under `best_effort`, why no sandbox is used (empty with one).
    pub fallbacks: Vec<Reason>,
    /// The sandbox applies the Landlock scope (and fails closed without it).
    pub scope: bool,
}

impl Contract {
    /// The contract of a node with `isolation: none`: nothing changes.
    fn untouched() -> Self {
        Contract {
            backend: Backend::None,
            holds: Vec::new(),
            limits: Vec::new(),
            refusals: Vec::new(),
            fallbacks: Vec::new(),
            scope: false,
        }
    }

    /// Whether the node may run.
    pub fn allowed(&self) -> bool {
        self.refusals.is_empty()
    }
}

/// Why the sandbox cannot be used for this node, in a fixed order.
fn obstacles(host: &HostFacts, node: &NodeFacts) -> Vec<Reason> {
    let mut out = Vec::new();
    if node.host_mode {
        out.push(Reason::new(
            ReasonCode::HostMode,
            "a host-mode step runs in the host agent, outside any sandbox apb starts",
        ));
    }
    if host.os != Os::Linux {
        out.push(Reason::new(
            ReasonCode::OsUnsupported,
            "the sandbox needs Linux with bubblewrap",
        ));
    }
    if host.disabled {
        out.push(Reason::new(ReasonCode::Disabled, "APB_SANDBOX=off"));
    }
    if host.os == Os::Linux {
        match &host.bwrap {
            None => out.push(Reason::new(
                ReasonCode::BwrapMissing,
                "install bubblewrap (bwrap)",
            )),
            Some(b) if b.setuid => out.push(Reason::new(
                ReasonCode::BwrapSetuid,
                format!(
                    "{} is setuid; apb uses unprivileged bwrap only",
                    b.path.display()
                ),
            )),
            Some(b) => {
                if let Err(e) = &b.trial {
                    out.push(Reason::new(ReasonCode::UsernsDenied, e.clone()));
                }
            }
        }
    }
    if !node.agents_without_needs.is_empty() {
        out.push(Reason::new(
            ReasonCode::AgentNeedsUnknown,
            node.agents_without_needs.join(", "),
        ));
    }
    out
}

/// The contract of an isolated node that cannot have a sandbox for the
/// `reasons`: `full` refuses, `best_effort` runs without one and lists what
/// it does not get. The engine also asks it for one attempt (a host step, a
/// sandbox that could not be set up).
pub fn without_sandbox(level: Level, reasons: Vec<Reason>) -> Contract {
    match level {
        Level::Full => Contract {
            refusals: reasons,
            ..Contract::untouched()
        },
        _ => Contract {
            holds: vec![Code::CredentialsEnvAllowlist],
            limits: vec![
                Code::NoSandbox,
                Code::UserNotSeparated,
                Code::FilesNotSeparated,
                Code::CredentialsHomeReadable,
                Code::NetworkNotSeparated,
                Code::ScriptsOutside,
            ],
            fallbacks: reasons,
            ..Contract::untouched()
        },
    }
}

/// The contract one node gets on this machine.
pub fn evaluate(host: &HostFacts, node: &NodeFacts) -> Contract {
    if node.level == Level::None {
        return Contract::untouched();
    }
    let mut blocked = obstacles(host, node);
    let open = node.network == Network::Open;
    // Under `full` with the host network, the scope keeps the host's
    // abstract sockets closed; without it, `full` cannot hold. Listed with
    // any other obstacle, so the operator learns every reason at once.
    if node.level == Level::Full && open && host.os == Os::Linux && !host.scope_available() {
        blocked.push(Reason::new(
            ReasonCode::ScopeUnavailable,
            format!(
                "network: open needs Landlock ABI {SCOPE_ABI} (this kernel: {}); use network: none",
                host.landlock_abi
            ),
        ));
    }
    if !blocked.is_empty() {
        return without_sandbox(node.level, blocked);
    }
    let scope = host.scope_available();
    let mut holds = vec![
        Code::UserNamespaces,
        Code::UserNoPrivileges,
        Code::UserNewSession,
        Code::FilesDeclaredView,
        Code::FilesGitControlReadOnly,
        Code::FilesApbMasked,
        Code::CredentialsEnvAllowlist,
        Code::CredentialsSocketsDropped,
        Code::CredentialsGitConfigReplaced,
    ];
    let mut limits = vec![
        Code::UserHostUid,
        Code::FilesSystemReadable,
        Code::CredentialsAgentLoginReadable,
    ];
    if open {
        limits.push(Code::NetworkOpenAnyHost);
    } else {
        holds.push(Code::NetworkNone);
    }
    if scope {
        holds.push(Code::NetworkScoped);
    } else if open {
        limits.push(Code::NetworkAbstractSocketsOpen);
    }
    limits.extend([Code::KernelEscapes, Code::ScriptsOutside]);
    Contract {
        backend: Backend::Bwrap,
        holds,
        limits,
        refusals: Vec::new(),
        fallbacks: Vec::new(),
        scope,
    }
}
