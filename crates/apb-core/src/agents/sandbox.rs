//! What an agent needs inside an isolation sandbox: the files of its own
//! login under the operator's home, and the variables that carry a login
//! instead. Declared by the agent's module ([`super::HostSetup::sandbox_needs`]);
//! an agent that declares nothing cannot run under `isolation: full`.

/// How a home file is visible in the sandbox.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Access {
    ReadOnly,
    ReadWrite,
}

/// One file or directory of the operator's home bound at the same place
/// in the sandbox home.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HomeBind {
    /// Relative to the home directory, without `..`.
    pub home_rel: &'static str,
    pub access: Access,
}

/// A home file the sandbox home gets a filtered copy of, written once (the
/// agent may rewrite its copy; the operator's file never changes).
#[derive(Clone, Copy)]
pub struct HomeCopy {
    /// Relative to the home directory, without `..`.
    pub home_rel: &'static str,
    /// The copy's bytes from the operator's file: only what the agent
    /// needs to start, never another service's settings or tokens.
    pub sanitize: fn(&[u8]) -> Vec<u8>,
}

impl PartialEq for HomeCopy {
    /// Two copies are the same when they name the same file.
    fn eq(&self, other: &Self) -> bool {
        self.home_rel == other.home_rel
    }
}

impl Eq for HomeCopy {}

impl std::fmt::Debug for HomeCopy {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HomeCopy")
            .field("home_rel", &self.home_rel)
            .finish_non_exhaustive()
    }
}

/// An agent's needs inside the sandbox.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SandboxNeeds {
    /// Home files bound into the sandbox home. Skipped when one of
    /// [`Self::env_logins`] is passed to the agent (the login comes from
    /// the environment then), and when the file does not exist.
    pub binds: Vec<HomeBind>,
    /// Home files the sandbox home gets a filtered copy of.
    pub copies: Vec<HomeCopy>,
    /// Variables that carry the agent's login. One that is passed makes
    /// the login binds unnecessary; none of them is listed as a passed
    /// credential, since the agent's own login is a stated limit.
    pub env_logins: Vec<&'static str>,
}

impl SandboxNeeds {
    /// The needs of an agent whose login lives in its run-scoped home.
    pub fn none() -> Self {
        SandboxNeeds::default()
    }
}
