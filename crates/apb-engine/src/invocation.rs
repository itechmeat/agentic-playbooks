//! Resolving the agent invocation form (spec 2026-07-12, sections 6.2-6.3).
//!
//! The invocation form is data (`InvocationDef`), not code: the built-in eight
//! are provided by `builtin`, custom agents come from the global config's
//! `agents:`. `resolve_invocation` fixes the agent, model, invocation form,
//! SOUL delivery method, canonical binary path, and its fingerprint - all of
//! which a run must remember so that resume does not silently pick up a
//! different binary (environment drift, Task 6).

use std::path::{Path, PathBuf};

use apb_core::config::{
    GlobalConfig, Interaction, InvocationDef, PromptVia, SoulDelivery, Transport,
};
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

/// Built-in invocation form for the known eight. `None` for unknown agents and
/// for pi (details will follow once the binary exists).
pub fn builtin(agent_id: &str) -> Option<InvocationDef> {
    let mk = |argv: &[&str],
              soul: SoulDelivery,
              soul_flag: Option<&str>,
              autonomous_args: &[&str],
              interaction: Interaction| InvocationDef {
        argv: argv.iter().map(|s| s.to_string()).collect(),
        prompt_via: PromptVia::Argv,
        soul,
        soul_flag: soul_flag.map(|s| s.to_string()),
        transport: Transport::Headless,
        autonomous_args: autonomous_args.iter().map(|s| s.to_string()).collect(),
        interaction,
    };
    match agent_id {
        // claude runs headless one-shot (`-p`); to actually write files and
        // reach the network on an authorized effectful run it needs an explicit
        // non-interactive permission mode, otherwise every tool call blocks
        // waiting for an approval that never comes (spec 8.5).
        //
        // Interaction ceiling per spec 2026-07-20: claude gets `live` (the
        // blocking `ask_user` MCP tool, Task 11); the aggregators that expose a
        // resumable session get `resume`; agy, which does not, gets `reprompt`.
        "claude" | "claude-code" => Some(mk(
            &["-p", "{prompt}", "--model", "{model}"],
            SoulDelivery::Native,
            Some("--append-system-prompt"),
            &["--permission-mode", "bypassPermissions"],
            Interaction::Live,
        )),
        "agy" => Some(mk(
            &["-p", "{prompt}", "--model", "{model}"],
            SoulDelivery::Prefix,
            None,
            &[],
            Interaction::Reprompt,
        )),
        "codex" => Some(mk(
            &["exec", "{prompt}", "-m", "{model}"],
            SoulDelivery::Prefix,
            None,
            &[],
            Interaction::Resume,
        )),
        "opencode" => Some(mk(
            &["run", "{prompt}", "-m", "{model}"],
            SoulDelivery::Prefix,
            None,
            &[],
            Interaction::Resume,
        )),
        // hermes one-shot mode prints only the final response text to
        // stdout and auto-bypasses approvals by design (script mode);
        // the SOUL travels as a prompt prefix like the other aggregators.
        "hermes" => Some(mk(
            &["-z", "{prompt}", "-m", "{model}"],
            SoulDelivery::Prefix,
            None,
            &[],
            Interaction::Resume,
        )),
        // grok runs headless one-shot via `-p/--single` and is the only new
        // agent with a native system-prompt flag (`--system-prompt-override`),
        // so its SOUL does not have to travel as a prompt prefix. Like claude,
        // an authorized effectful run needs an explicit non-interactive
        // permission mode or every tool call blocks on an approval that never
        // arrives.
        "grok" => Some(mk(
            &["-p", "{prompt}", "-m", "{model}"],
            SoulDelivery::Native,
            Some("--system-prompt-override"),
            &["--permission-mode", "bypassPermissions"],
            Interaction::Resume,
        )),
        // cursor's `-p/--print` is a BOOLEAN flag and the prompt is a
        // positional argument, so the prompt slot goes last, after the
        // options. `--force` is the non-interactive approval mode and
        // `--output-format text` pins plain stdout (it only takes effect
        // together with `--print`). No system-prompt flag exists, so the SOUL
        // travels as a prefix like the other aggregators.
        "cursor" => Some(mk(
            &["-p", "--model", "{model}", "{prompt}"],
            SoulDelivery::Prefix,
            None,
            &["--output-format", "text", "--force"],
            Interaction::Resume,
        )),
        _ => None,
    }
}

/// Declarative resume-form argv for an agent's `resume` transport (spec
/// 2026-07-20, Task 7). The answer round substitutes the placeholders as whole
/// argv elements - `{session}` (the captured session id), `{prompt}` (the
/// user's answer, delivered as the follow-up), `{model}` - the same way the
/// primary `argv` template is filled. `None` for an agent with no resume form,
/// which forces the runtime downgrade to `reprompt`. Kept beside `builtin` so
/// the resume argv stays as declarative as the launch argv, never string-built
/// in the scheduler.
pub fn resume_argv(agent_id: &str) -> Option<Vec<String>> {
    let v = |parts: &[&str]| -> Vec<String> { parts.iter().map(|s| s.to_string()).collect() };
    match agent_id {
        // claude resumes a prior session with `--resume <id>` and takes the
        // follow-up as a fresh `-p` prompt.
        "claude" | "claude-code" => Some(v(&[
            "--resume",
            "{session}",
            "-p",
            "{prompt}",
            "--model",
            "{model}",
        ])),
        // codex re-enters a conversation via `exec resume <id>`.
        "codex" => Some(v(&[
            "exec",
            "resume",
            "{session}",
            "{prompt}",
            "-m",
            "{model}",
        ])),
        // opencode re-enters a session via `--session <id>`.
        "opencode" => Some(v(&[
            "run",
            "--session",
            "{session}",
            "{prompt}",
            "-m",
            "{model}",
        ])),
        // hermes re-enters a session via `--resume <id>`, still in script mode.
        "hermes" => Some(v(&[
            "-z",
            "--resume",
            "{session}",
            "{prompt}",
            "-m",
            "{model}",
        ])),
        // grok resumes by id via `-r <id>` and takes the follow-up as a fresh
        // `-p` single-turn prompt.
        "grok" => Some(v(&["-r", "{session}", "-p", "{prompt}", "-m", "{model}"])),
        // cursor resumes a chat via `--resume <chatId>`; the follow-up prompt
        // stays positional and therefore last.
        "cursor" => Some(v(&[
            "--resume",
            "{session}",
            "-p",
            "--model",
            "{model}",
            "{prompt}",
        ])),
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
        // one of the built-in eight: historical compatibility falls back to
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
            // cursor is installed as `cursor-agent`; the bare `cursor` binary
            // is the GUI editor CLI, not the headless agent.
            "cursor" => "cursor-agent".to_string(),
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
            autonomous_args: vec![],
            interaction: Interaction::default(),
        };
        assert!(two.validate().is_err());

        let partial = InvocationDef {
            argv: vec!["x{model}".into(), "{prompt}".into()],
            prompt_via: PromptVia::Argv,
            soul: SoulDelivery::Prefix,
            soul_flag: None,
            transport: Transport::Headless,
            autonomous_args: vec![],
            interaction: Interaction::default(),
        };
        assert!(partial.validate().is_err());

        let stdin_with_slot = InvocationDef {
            argv: vec!["{prompt}".into()],
            prompt_via: PromptVia::Stdin,
            soul: SoulDelivery::Prefix,
            soul_flag: None,
            transport: Transport::Headless,
            autonomous_args: vec![],
            interaction: Interaction::default(),
        };
        assert!(stdin_with_slot.validate().is_err());

        let native_no_flag = InvocationDef {
            argv: vec!["{prompt}".into()],
            prompt_via: PromptVia::Argv,
            soul: SoulDelivery::Native,
            soul_flag: None,
            transport: Transport::Headless,
            autonomous_args: vec![],
            interaction: Interaction::default(),
        };
        assert!(native_no_flag.validate().is_err());
    }

    /// cursor's detected binary is `cursor-agent`, not `cursor` (the GUI
    /// editor CLI). `program_for` must map the agent id to the real binary so
    /// the manifest fingerprint and the spawned adapter target the same file;
    /// every other built-in agent id equals its binary name.
    #[test]
    fn program_for_maps_cursor_to_its_binary() {
        let global = GlobalConfig::default();
        assert_eq!(program_for("cursor", &global), "cursor-agent");
        assert_eq!(program_for("grok", &global), "grok");
        assert_eq!(program_for("codex", &global), "codex");
        assert_eq!(program_for("claude", &global), "claude");
        assert_eq!(program_for("claude-code", &global), "claude");
    }

    #[test]
    fn builtin_agents_present_and_valid() {
        for id in [
            "claude", "agy", "codex", "opencode", "hermes", "grok", "cursor",
        ] {
            builtin(id).unwrap().validate().unwrap();
        }
        assert!(builtin("pi").is_none());
        assert!(builtin("unknown").is_none());
    }

    /// grok delivers the SOUL natively via `--system-prompt-override` and
    /// resumes with `-r`, both verified against Grok Build 0.2.x `--help`.
    #[test]
    fn builtin_grok_form() {
        let spec = builtin("grok").expect("grok builtin spec");
        assert_eq!(spec.argv, vec!["-p", "{prompt}", "-m", "{model}"]);
        assert_eq!(spec.soul, SoulDelivery::Native);
        assert_eq!(spec.soul_flag.as_deref(), Some("--system-prompt-override"));
        assert_eq!(spec.transport, Transport::Headless);
        assert_eq!(
            spec.autonomous_args,
            vec!["--permission-mode", "bypassPermissions"]
        );
        assert_eq!(spec.interaction, Interaction::Resume);
        assert_eq!(
            resume_argv("grok").expect("grok resume argv"),
            vec!["-r", "{session}", "-p", "{prompt}", "-m", "{model}"]
        );
    }

    /// cursor's `-p` is a boolean print flag and the prompt is POSITIONAL, so
    /// the prompt slot must come last, after every option.
    #[test]
    fn builtin_cursor_form() {
        let spec = builtin("cursor").expect("cursor builtin spec");
        assert_eq!(spec.argv, vec!["-p", "--model", "{model}", "{prompt}"]);
        assert_eq!(spec.soul, SoulDelivery::Prefix);
        assert_eq!(spec.soul_flag, None);
        assert_eq!(spec.transport, Transport::Headless);
        assert_eq!(
            spec.autonomous_args,
            vec!["--output-format", "text", "--force"]
        );
        assert_eq!(spec.interaction, Interaction::Resume);
        assert_eq!(
            resume_argv("cursor").expect("cursor resume argv"),
            vec![
                "--resume",
                "{session}",
                "-p",
                "--model",
                "{model}",
                "{prompt}"
            ]
        );
    }

    #[test]
    fn builtin_hermes_form() {
        let spec = builtin("hermes").expect("hermes builtin spec");
        assert_eq!(spec.argv, vec!["-z", "{prompt}", "-m", "{model}"]);
        assert_eq!(spec.soul, SoulDelivery::Prefix);
        assert_eq!(spec.soul_flag, None);
        assert_eq!(spec.transport, Transport::Headless);
        assert!(spec.autonomous_args.is_empty());
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
