//! Resolving the agent invocation form (spec 2026-07-12, sections 6.2-6.3).
//!
//! The invocation form is data (`InvocationDef`), not code: the built-in five
//! are provided by `builtin`, custom agents come from the global config's
//! `agents:`. `resolve_invocation` fixes the agent, model, invocation form,
//! SOUL delivery method, canonical binary path, and its fingerprint - all of
//! which a run must remember so that resume does not silently pick up a
//! different binary (environment drift, Task 6).

use std::path::{Path, PathBuf};

use apb_core::config::{GlobalConfig, InvocationDef, PromptVia, SoulDelivery, Transport};
use apb_core::profile::SoulRequirement;

use crate::error::EngineError;

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ResolvedInvocation {
    pub agent_id: String,
    pub model: String,
    pub spec: InvocationDef,
    pub soul_delivery: SoulDelivery,
    pub canonical_executable: PathBuf,
    /// Binary fingerprint "size:mtime_ms" for verification on resume.
    pub executable_fingerprint: String,
}

/// Built-in invocation form for the known five. `None` for unknown agents and
/// for pi (details will follow once the binary exists).
pub fn builtin(agent_id: &str) -> Option<InvocationDef> {
    let mk = |argv: &[&str], soul: SoulDelivery, soul_flag: Option<&str>| InvocationDef {
        argv: argv.iter().map(|s| s.to_string()).collect(),
        prompt_via: PromptVia::Argv,
        soul,
        soul_flag: soul_flag.map(|s| s.to_string()),
        transport: Transport::Headless,
    };
    match agent_id {
        "claude" | "claude-code" => Some(mk(
            &["-p", "{prompt}", "--model", "{model}"],
            SoulDelivery::Native,
            Some("--append-system-prompt"),
        )),
        "agy" => Some(mk(
            &["-p", "{prompt}", "--model", "{model}"],
            SoulDelivery::Prefix,
            None,
        )),
        "codex" => Some(mk(
            &["exec", "{prompt}", "-m", "{model}"],
            SoulDelivery::Prefix,
            None,
        )),
        "opencode" => Some(mk(
            &["run", "{prompt}", "-m", "{model}"],
            SoulDelivery::Prefix,
            None,
        )),
        _ => None,
    }
}

/// Agent invocation form: config (`agents:`) overrides the built-in default.
/// Transport is taken from config (compatibility with the former
/// `agent_transport`).
pub fn spec_for(agent_id: &str, global: &GlobalConfig) -> Result<InvocationDef, EngineError> {
    let mut spec = global
        .agents
        .get(agent_id)
        .and_then(|a| a.invocation.clone())
        .or_else(|| builtin(agent_id))
        // Agent is defined in config but without an explicit form and is not
        // one of the built-in five: historical compatibility falls back to
        // the claude form (`-p {prompt} --model {model}`).
        .or_else(|| global.agents.get(agent_id).and(builtin("claude")))
        .ok_or_else(|| {
            EngineError::Adapter(format!(
                "no invocation for agent `{agent_id}` (define `agents.{agent_id}.invocation` in global config)"
            ))
        })?;
    spec.transport = global.agent_transport(agent_id);
    spec.validate()
        .map_err(|e| EngineError::Adapter(format!("invalid invocation for `{agent_id}`: {e}")))?;
    Ok(spec)
}

/// Agent binary name/path, resolved the same way `adapter_for` picks it:
/// APB_AGENT_CMD (override for tests/local runs) has the highest priority,
/// then `agents.<id>.program`, then the default (claude/claude-code ->
/// "claude", otherwise the id itself). Shared source for both the adapter and
/// the manifest fingerprint - otherwise env drift would trigger falsely.
pub fn program_for(agent_id: &str, global: &GlobalConfig) -> String {
    if let Ok(p) = std::env::var("APB_AGENT_CMD") {
        return p;
    }
    global
        .agent_program(agent_id)
        .unwrap_or_else(|| match agent_id {
            "claude" | "claude-code" => "claude".to_string(),
            other => other.to_string(),
        })
}

/// Resolves the invocation for an agent+model pair: form + canonical binary
/// path + fingerprint. `program` is the binary name (from config or the
/// agent_id itself).
pub fn resolve_invocation(
    agent_id: &str,
    model: &str,
    program: &str,
    global: &GlobalConfig,
) -> Result<ResolvedInvocation, EngineError> {
    let spec = spec_for(agent_id, global)?;
    let soul_delivery = spec.soul;
    let (canonical_executable, executable_fingerprint) = fingerprint_program(program);
    Ok(ResolvedInvocation {
        agent_id: agent_id.to_string(),
        model: model.to_string(),
        spec,
        soul_delivery,
        canonical_executable,
        executable_fingerprint,
    })
}

/// Fingerprint of a specific binary path: "size:mtime_ms", empty if the file
/// does not exist. For the resume drift check we fingerprint EXACTLY the
/// `canonical_executable` recorded in the manifest, not a fresh resolution
/// against the live config/PATH.
pub fn fingerprint_path(path: &Path) -> String {
    std::fs::metadata(path)
        .ok()
        .map(|m| {
            let mtime = m
                .modified()
                .ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| d.as_millis())
                .unwrap_or(0);
            format!("{}:{}", m.len(), mtime)
        })
        .unwrap_or_default()
}

/// Canonicalizes the program path and takes its fingerprint. If the binary is
/// not found in PATH/at the given path, the fingerprint is empty (checking
/// existence is the job of detect/adoption; here we just record what is
/// there).
fn fingerprint_program(program: &str) -> (PathBuf, String) {
    let resolved = resolve_program_path(program);
    let fp = resolved
        .as_ref()
        .and_then(|p| std::fs::metadata(p).ok())
        .map(|m| {
            let mtime = m
                .modified()
                .ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| d.as_millis())
                .unwrap_or(0);
            format!("{}:{}", m.len(), mtime)
        })
        .unwrap_or_default();
    (resolved.unwrap_or_else(|| PathBuf::from(program)), fp)
}

/// Finds the canonical path to the program: a direct path containing a
/// separator is canonicalized; otherwise the first EXECUTABLE candidate from
/// PATH (in PATH order). We check the execute bit specifically, not just
/// is_file - otherwise a same-named non-executable file earlier in PATH would
/// point the fingerprint at the wrong binary.
fn resolve_program_path(program: &str) -> Option<PathBuf> {
    if program.contains('/') || program.contains('\\') {
        return std::fs::canonicalize(program).ok();
    }
    let path = std::env::var("PATH").ok()?;
    std::env::split_paths(&path)
        .map(|dir| dir.join(program))
        .find(|cand| is_executable_file(cand))
        .and_then(|p| std::fs::canonicalize(p).ok())
}

/// The file exists and is executable. On Unix, by the `x` bit (any of
/// u/g/o); on other platforms, just a regular file.
fn is_executable_file(path: &Path) -> bool {
    let Ok(meta) = std::fs::metadata(path) else {
        return false;
    };
    if !meta.is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        meta.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        true
    }
}

/// Filters the chain by SOUL requirement (spec 6.3): `native_required`
/// removes prefix elements. An empty SOUL means no filtering (nothing to
/// deliver). An empty chain after filtering is an error.
pub fn filter_chain(
    chain: Vec<ResolvedInvocation>,
    req: SoulRequirement,
    soul_empty: bool,
) -> Result<Vec<ResolvedInvocation>, EngineError> {
    if req == SoulRequirement::NativeRequired && !soul_empty {
        let filtered: Vec<ResolvedInvocation> = chain
            .into_iter()
            .filter(|i| i.soul_delivery == SoulDelivery::Native)
            .collect();
        if filtered.is_empty() {
            return Err(EngineError::Invalid(
                "profile requires native SOUL delivery but no executor in the chain supports it"
                    .into(),
            ));
        }
        return Ok(filtered);
    }
    Ok(chain)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ri(agent: &str, soul: SoulDelivery) -> ResolvedInvocation {
        ResolvedInvocation {
            agent_id: agent.into(),
            model: "m".into(),
            spec: builtin("claude").unwrap(),
            soul_delivery: soul,
            canonical_executable: PathBuf::from("/bin/true"),
            executable_fingerprint: "0:0".into(),
        }
    }

    #[test]
    fn validate_rejects_two_prompt_slots_and_partial_placeholders() {
        let two = InvocationDef {
            argv: vec!["{prompt}".into(), "{prompt}".into()],
            prompt_via: PromptVia::Argv,
            soul: SoulDelivery::Prefix,
            soul_flag: None,
            transport: Transport::Headless,
        };
        assert!(two.validate().is_err());

        let partial = InvocationDef {
            argv: vec!["x{model}".into(), "{prompt}".into()],
            prompt_via: PromptVia::Argv,
            soul: SoulDelivery::Prefix,
            soul_flag: None,
            transport: Transport::Headless,
        };
        assert!(partial.validate().is_err());

        let stdin_with_slot = InvocationDef {
            argv: vec!["{prompt}".into()],
            prompt_via: PromptVia::Stdin,
            soul: SoulDelivery::Prefix,
            soul_flag: None,
            transport: Transport::Headless,
        };
        assert!(stdin_with_slot.validate().is_err());

        let native_no_flag = InvocationDef {
            argv: vec!["{prompt}".into()],
            prompt_via: PromptVia::Argv,
            soul: SoulDelivery::Native,
            soul_flag: None,
            transport: Transport::Headless,
        };
        assert!(native_no_flag.validate().is_err());
    }

    #[test]
    fn builtin_five_agents_present_and_valid() {
        for id in ["claude", "agy", "codex", "opencode"] {
            builtin(id).unwrap().validate().unwrap();
        }
        assert!(builtin("pi").is_none());
        assert!(builtin("unknown").is_none());
    }

    #[test]
    fn native_required_filters_prefix_but_not_when_soul_empty() {
        let chain = vec![
            ri("agy", SoulDelivery::Prefix),
            ri("claude", SoulDelivery::Native),
        ];
        let filtered = filter_chain(chain.clone(), SoulRequirement::NativeRequired, false).unwrap();
        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0].soul_delivery, SoulDelivery::Native);

        // Empty SOUL - no filtering.
        let all = filter_chain(chain.clone(), SoulRequirement::NativeRequired, true).unwrap();
        assert_eq!(all.len(), 2);

        // Any - no filtering.
        let any = filter_chain(chain, SoulRequirement::Any, false).unwrap();
        assert_eq!(any.len(), 2);
    }

    #[test]
    fn native_required_empty_chain_errors() {
        let chain = vec![ri("agy", SoulDelivery::Prefix)];
        assert!(filter_chain(chain, SoulRequirement::NativeRequired, false).is_err());
    }
}
