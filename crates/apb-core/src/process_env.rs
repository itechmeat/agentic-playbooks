//! Which of the operator's environment variables an agent process inherits.
//!
//! A profile's `process_env` picks the policy. `inherit` (the default,
//! written as the absence of the key) hands the agent the whole environment
//! apb runs in, as before. `allowlist` hands it only a fixed base set, the
//! variables its agent CLI reads ([`sets`]) and the names the profile adds in
//! `allow`. Connector-referenced names stay withheld either way.
//!
//! Everything here works on variable NAMES. No value is ever read, held or
//! returned: the engine copies the kept values straight from its own
//! environment onto the child process.

use std::ffi::OsString;

use serde::{Deserialize, Serialize};

pub mod sets;

pub use sets::{AgentEnvSet, base_set};

/// At most this many `allow` entries.
pub const MAX_ALLOW: usize = 64;

/// Whether the agent process inherits the operator's environment.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ProcessEnvMode {
    /// The whole environment apb runs in (the default).
    #[default]
    Inherit,
    /// Only the base set, the agent's own set and the profile's `allow`.
    Allowlist,
}

impl ProcessEnvMode {
    /// The spelling in profile.yaml and on every surface.
    pub fn as_str(self) -> &'static str {
        match self {
            ProcessEnvMode::Inherit => "inherit",
            ProcessEnvMode::Allowlist => "allowlist",
        }
    }

    /// Parses `inherit` / `allowlist`.
    pub fn parse(s: &str) -> Result<Self, String> {
        match s {
            "inherit" => Ok(ProcessEnvMode::Inherit),
            "allowlist" => Ok(ProcessEnvMode::Allowlist),
            other => Err(format!(
                "process_env policy `{other}`: expected `inherit` or `allowlist`"
            )),
        }
    }
}

/// One `allow` entry: an exact variable name, or a prefix followed by one
/// trailing `*`. Deserialized as written, so a malformed entry is reported by
/// [`ProcessEnvPolicy::problems`] with the profile's other problems instead
/// of failing the whole document.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Deserialize, Serialize)]
#[serde(transparent)]
pub struct EnvPattern(String);

impl EnvPattern {
    /// A built-in pattern from [`sets`]: known well formed.
    pub(crate) fn builtin(s: &str) -> Self {
        EnvPattern(s.to_string())
    }

    /// The pattern as written.
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// The prefix of a `PREFIX*` pattern, `None` for an exact name.
    fn prefix(&self) -> Option<&str> {
        self.0.strip_suffix('*')
    }

    /// Whether this is an exact name (no trailing `*`).
    pub fn is_exact(&self) -> bool {
        self.prefix().is_none()
    }

    /// Whether variable `name` matches. Case-insensitive only on Windows,
    /// whose environment is.
    pub fn matches(&self, name: &str) -> bool {
        match self.prefix() {
            Some(prefix) => name
                .get(..prefix.len())
                .is_some_and(|head| same(head, prefix)),
            None => same(name, &self.0),
        }
    }

    /// Why the entry is malformed, `None` when it is well formed.
    fn syntax_problem(&self) -> Option<String> {
        let (name, wildcard) = match self.prefix() {
            Some(p) => (p, true),
            None => (self.0.as_str(), false),
        };
        let mut chars = name.chars();
        let well_formed = chars
            .next()
            .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
            && chars.all(|c| c.is_ascii_alphanumeric() || c == '_');
        if !well_formed {
            return Some(format!(
                "`{}`: a variable name is [A-Za-z_][A-Za-z0-9_]*, optionally followed by one `*`",
                self.0
            ));
        }
        if wildcard && name.len() < 2 {
            return Some(format!(
                "`{}`: a prefix before `*` needs at least 2 characters",
                self.0
            ));
        }
        None
    }
}

impl TryFrom<String> for EnvPattern {
    type Error = String;

    fn try_from(s: String) -> Result<Self, Self::Error> {
        let p = EnvPattern(s);
        match p.syntax_problem() {
            Some(problem) => Err(problem),
            None => Ok(p),
        }
    }
}

#[cfg(windows)]
fn same(a: &str, b: &str) -> bool {
    a.eq_ignore_ascii_case(b)
}

#[cfg(not(windows))]
fn same(a: &str, b: &str) -> bool {
    a == b
}

/// A profile's `process_env`.
#[derive(Debug, Clone, PartialEq, Eq, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ProcessEnvPolicy {
    #[serde(default)]
    pub policy: ProcessEnvMode,
    /// Names (or `PREFIX*` patterns) added to the built-in sets.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub allow: Vec<EnvPattern>,
}

impl ProcessEnvPolicy {
    /// Whether the policy restricts the agent's environment.
    pub fn is_allowlist(&self) -> bool {
        self.policy == ProcessEnvMode::Allowlist
    }

    /// Structural problems: a malformed or duplicate entry, too many
    /// entries, `allow` under `inherit`.
    pub fn problems(&self) -> Vec<String> {
        let mut out: Vec<String> = self
            .allow
            .iter()
            .filter_map(EnvPattern::syntax_problem)
            .collect();
        let mut seen = std::collections::BTreeSet::new();
        for p in &self.allow {
            if !seen.insert(p.as_str()) {
                out.push(format!("`{}` is listed twice", p.as_str()));
            }
        }
        if self.allow.len() > MAX_ALLOW {
            out.push(format!(
                "`allow` has {} entries, at most {MAX_ALLOW} are allowed",
                self.allow.len()
            ));
        }
        if !self.allow.is_empty() && !self.is_allowlist() {
            out.push("`allow` has no effect under `policy: inherit`".to_string());
        }
        out
    }

    /// The policy a write surface asked for. `None` when neither a policy
    /// nor an `allow` entry was given, which keeps the stored value on an
    /// update. `allow` without a policy means `allowlist`. A malformed
    /// policy is an error naming every problem.
    pub fn from_surface(policy: Option<&str>, allow: &[String]) -> Result<Option<Self>, String> {
        let mode = match (policy, allow.is_empty()) {
            (None, true) => return Ok(None),
            (None, false) => ProcessEnvMode::Allowlist,
            (Some(p), _) => ProcessEnvMode::parse(p)?,
        };
        let out = ProcessEnvPolicy {
            policy: mode,
            allow: allow.iter().map(|s| EnvPattern(s.clone())).collect(),
        };
        let problems = out.problems();
        if problems.is_empty() {
            Ok(Some(out))
        } else {
            Err(format!("process_env: {}", problems.join("; ")))
        }
    }

    /// The policy as stored in profile.yaml: `inherit` is the absence of
    /// the key.
    pub fn stored(policy: Option<Self>) -> Option<Self> {
        policy.filter(Self::is_allowlist)
    }
}

/// Whether a trigger variable's value switches its rule on: `1`, `true`,
/// `yes` or `on` in any case, as the agent CLIs read their `*_USE_*`
/// switches. `0`, `false` or an empty value leave the rule off.
pub fn is_truthy(value: &str) -> bool {
    ["1", "true", "yes", "on"]
        .iter()
        .any(|t| value.trim().eq_ignore_ascii_case(t))
}

/// Every pattern an agent process under `allowlist` may inherit: the base
/// set, the agent's own `set`, the names its trigger rules switch on (a
/// trigger counts when `is_on` says the operator switched it on, see
/// [`is_truthy`]), the variables its module names for the model
/// (`model_env`, an opencode provider's key), the exact names derived from
/// its configuration (`derived`) and the profile's own well-formed `allow`.
/// The caller takes `set`, `model_env` and `derived` from the agent's module
/// (`crate::agents`); a custom agent has none of them.
pub fn allowed_patterns(
    set: AgentEnvSet,
    model_env: &[&str],
    allow: &[EnvPattern],
    is_on: impl Fn(&str) -> bool,
    derived: &[String],
) -> Vec<EnvPattern> {
    let mut out: Vec<EnvPattern> = base_set().iter().map(|s| EnvPattern::builtin(s)).collect();
    out.extend(set.always.iter().map(|s| EnvPattern::builtin(s)));
    for (trigger, names) in set.when {
        if is_on(trigger) {
            out.extend(names.iter().map(|s| EnvPattern::builtin(s)));
        }
    }
    out.extend(model_env.iter().map(|s| EnvPattern::builtin(s)));
    // A derived name comes from a config file, not from the profile: only an
    // exact, well-formed name, never a `PREFIX*` that would widen the set.
    out.extend(
        derived
            .iter()
            .map(|n| EnvPattern(n.to_string()))
            .filter(|p| p.is_exact() && p.syntax_problem().is_none()),
    );
    // The profile's own entries were checked at run start, but a resume
    // rebuilds them from the manifest: a malformed one (`*` alone would match
    // every name) is dropped rather than trusted.
    out.extend(
        allow
            .iter()
            .filter(|p| p.syntax_problem().is_none())
            .cloned(),
    );
    out
}

/// Which of the parent's variables a child keeps, by name only.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct EnvPlan {
    /// Names passed on, sorted.
    pub keep: Vec<String>,
    /// Names not passed on, sorted. A name that is not valid UTF-8 is
    /// always withheld and listed in its lossy form.
    pub withheld: Vec<String>,
    /// The profile's own exact `allow` names the parent does not set.
    pub absent: Vec<String>,
}

impl EnvPlan {
    /// Whether `name` is passed on.
    pub fn keeps(&self, name: &str) -> bool {
        self.keep.binary_search_by(|k| k.as_str().cmp(name)).is_ok()
    }
}

/// Splits the parent's variable names into kept and withheld. A name is
/// kept when a pattern of `allow` matches it and `scrub` does not name it
/// (the connector scrub always wins). `absent` lists the `explicit` exact
/// names the parent does not set. Pure: names only, values are never seen.
pub fn plan(
    parent: impl IntoIterator<Item = OsString>,
    allow: &[EnvPattern],
    scrub: &[String],
    explicit: &[EnvPattern],
) -> EnvPlan {
    let mut out = EnvPlan::default();
    let mut present = std::collections::BTreeSet::new();
    for name in parent {
        match name.into_string() {
            Ok(name) => {
                present.insert(name.clone());
                let scrubbed = scrub.iter().any(|s| same(s, &name));
                if !scrubbed && allow.iter().any(|p| p.matches(&name)) {
                    out.keep.push(name);
                } else {
                    out.withheld.push(name);
                }
            }
            Err(raw) => out.withheld.push(raw.to_string_lossy().into_owned()),
        }
    }
    out.absent = explicit
        .iter()
        .filter(|p| p.is_exact() && !present.iter().any(|n| p.matches(n)))
        .map(|p| p.as_str().to_string())
        .collect();
    out.keep.sort();
    out.keep.dedup();
    out.withheld.sort();
    out.withheld.dedup();
    out.absent.sort();
    out.absent.dedup();
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pat(s: &str) -> EnvPattern {
        EnvPattern::try_from(s.to_string()).unwrap()
    }

    #[test]
    fn a_prefix_pattern_matches_by_prefix_and_an_exact_one_by_name() {
        assert!(pat("NPM_CONFIG_*").matches("NPM_CONFIG_REGISTRY"));
        assert!(!pat("NPM_CONFIG_*").matches("NPM_TOKEN"));
        assert!(pat("GH_TOKEN").matches("GH_TOKEN"));
        assert!(!pat("GH_TOKEN").matches("GH_TOKEN_2"));
    }

    #[test]
    fn non_utf8_names_are_withheld() {
        #[cfg(unix)]
        {
            use std::os::unix::ffi::OsStringExt as _;
            let raw = OsString::from_vec(vec![b'A', b'B', 0xff]);
            let p = plan([raw], &[pat("AB*")], &[], &[]);
            assert!(p.keep.is_empty());
            assert_eq!(p.withheld.len(), 1);
        }
    }
}
