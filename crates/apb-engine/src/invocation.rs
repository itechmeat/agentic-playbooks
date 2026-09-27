//! Resolving the agent invocation form (spec 2026-07-12, sections 6.2-6.3).
//!
//! The invocation form is data (`InvocationDef`), not code: the built-in ten
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

/// Built-in invocation form for the known ten. `None` for unknown agents and
/// for pi (details will follow once the binary exists).
///
/// claude, codex, opencode and zcode are asked for their machine output
/// (`--output-format json`, `exec --json`, `run --format json`, `--json`):
/// it carries the token usage and the session id of the attempt, and the
/// adapter unwraps the reply from it (`apb_core::agent_output`), falling back
/// to the raw stdout when the shape is absent.
///
/// The prompt is never parsed as an option. Where an agent takes it as a
/// positional argument (claude, codex, opencode, cursor, qoder) the form ends
/// with `--`, `{prompt}` and the adapter keeps that pair last. Verified
/// against claude 2.1.283, `codex exec` and `codex exec resume` 0.157.0 and
/// `opencode run` 1.18.32: each reads a dash-led prompt after `--` as text and refuses
/// it without one. cursor and qoder are not installed where this was checked;
/// `--` is the standard end of options of their parsers. Where the prompt is an option's value
/// (`-p <text>` for grok, zcode and agy, `-z <text>` for hermes) there is no
/// `--` to put in front of it; the adapter sends a dash-led prompt with a
/// leading newline instead (zcode refuses a dash-led `-p` value outright).
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
    match apb_core::detect::canonical_agent_id(agent_id) {
        // claude runs headless one-shot (`-p`); to actually write files and
        // reach the network on an authorized effectful run it needs an explicit
        // non-interactive permission mode, otherwise every tool call blocks
        // waiting for an approval that never comes (spec 8.5).
        //
        // Interaction ceiling per spec 2026-07-20: claude gets `live` (the
        // blocking `ask_user` MCP tool, Task 11); the aggregators that expose a
        // resumable session get `resume`; agy, which does not, gets `reprompt`.
        "claude" => Some(mk(
            &[
                "-p",
                "--output-format",
                "json",
                "--model",
                "{model}",
                "--",
                "{prompt}",
            ],
            SoulDelivery::Native,
            Some("--append-system-prompt"),
            &["--permission-mode", "bypassPermissions"],
            Interaction::Live,
        )),
        // Verified against the local `agy --help`: `--dangerously-skip-permissions`
        // auto-approves all tool permission requests, the exact equivalent of
        // claude's flag.
        "agy" => Some(mk(
            &["-p", "{prompt}", "--model", "{model}"],
            SoulDelivery::Prefix,
            None,
            &["--dangerously-skip-permissions"],
            Interaction::Reprompt,
        )),
        // Verified against the local `codex exec --help`:
        // `--dangerously-bypass-approvals-and-sandbox` skips all confirmation
        // prompts and runs without sandboxing, the one-shot equivalent of
        // claude's bypassPermissions.
        "codex" => Some(mk(
            &["exec", "--json", "-m", "{model}", "--", "{prompt}"],
            SoulDelivery::Prefix,
            None,
            &["--dangerously-bypass-approvals-and-sandbox"],
            Interaction::Resume,
        )),
        // Verified against the local `opencode run --help`: `--auto`
        // auto-approves permissions that are not explicitly denied.
        "opencode" => Some(mk(
            &["run", "--format", "json", "-m", "{model}", "--", "{prompt}"],
            SoulDelivery::Prefix,
            None,
            &["--auto"],
            Interaction::Resume,
        )),
        // hermes one-shot mode prints only the final response text to
        // stdout and auto-bypasses approvals by design (script mode);
        // the SOUL travels as a prompt prefix like the other aggregators.
        //
        // No autonomous flag: hermes documents `--yolo` at the same top level
        // as `-z`, but it could not be verified in combination with the
        // one-shot `-z` form against a local binary, and shipping an
        // unverified flag into every hermes invocation is worse than the
        // documented gap. `apb doctor --run` warns on a node bound to hermes
        // instead (#85 finding 1).
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
            &["-p", "--model", "{model}", "--", "{prompt}"],
            SoulDelivery::Prefix,
            None,
            &["--output-format", "text", "--force"],
            Interaction::Resume,
        )),
        // qoder's `-p/--print` is a BOOLEAN flag and the prompt is a
        // positional argument, so the prompt slot goes last, after the
        // options, like cursor. Unlike cursor it has a real system-prompt
        // flag; `--append-system-prompt` is used (not `--system-prompt`) so
        // the SOUL rides alongside qoder's own agentic system prompt instead
        // of replacing it. `--permission-mode bypass_permissions` (snake_case,
        // unlike claude's camelCase) is the non-interactive approval mode and
        // `--output-format text` pins plain stdout.
        "qoder" => Some(mk(
            &[
                "-p",
                "--output-format",
                "text",
                "--model",
                "{model}",
                "--",
                "{prompt}",
            ],
            SoulDelivery::Native,
            Some("--append-system-prompt"),
            &["--permission-mode", "bypass_permissions"],
            Interaction::Resume,
        )),
        // zcode (Z.ai's ZCode headless CLI, verified against zcode-agent
        // 0.16.9). `--mode` DEFAULTS TO `yolo` for `-p`, so the base form pins
        // `build` (every approval request is denied in `-p` mode, there is no
        // interactive gate) and only an authorized effectful run gets `yolo`:
        // the autonomy flags come later in argv and zcode's option parser
        // keeps the last value. `--json` prints one JSON object with
        // `sessionId` and `response`, which the adapter unwraps. There is no
        // `--model` flag and no system-prompt flag: the model travels through
        // a run-scoped provider config (see `adapter::apply_zcode_env`) and
        // the SOUL as a prompt prefix.
        "zcode" => Some(mk(
            &["-p", "{prompt}", "--json", "--mode", "build"],
            SoulDelivery::Prefix,
            None,
            &["--mode", "yolo"],
            Interaction::Resume,
        )),
        _ => None,
    }
}

/// zcode's autonomy flags for a profile's `zcode_mode`: the `--mode` that
/// follows the base form's `build` (zcode keeps the last value). The builtin
/// form's `yolo` is `ZcodeMode::Yolo`.
pub fn zcode_autonomous_args(mode: apb_core::profile::ZcodeMode) -> Vec<String> {
    vec!["--mode".to_string(), mode.as_str().to_string()]
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
    match apb_core::detect::canonical_agent_id(agent_id) {
        // claude resumes a prior session with `--resume <id>` and takes the
        // follow-up as a fresh `-p` prompt.
        "claude" => Some(v(&[
            "--resume",
            "{session}",
            "-p",
            "--output-format",
            "json",
            "--model",
            "{model}",
            "--",
            "{prompt}",
        ])),
        // codex re-enters a conversation via `exec resume <id>`.
        "codex" => Some(v(&[
            "exec",
            "resume",
            "{session}",
            "--json",
            "-m",
            "{model}",
            "--",
            "{prompt}",
        ])),
        // opencode re-enters a session via `--session <id>`.
        "opencode" => Some(v(&[
            "run",
            "--session",
            "{session}",
            "--format",
            "json",
            "-m",
            "{model}",
            "--",
            "{prompt}",
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
            "--",
            "{prompt}",
        ])),
        // qoder resumes a session via `--resume <id>`; the follow-up prompt
        // stays positional and therefore last, like cursor.
        "qoder" => Some(v(&[
            "--resume",
            "{session}",
            "-p",
            "--output-format",
            "text",
            "--model",
            "{model}",
            "--",
            "{prompt}",
        ])),
        // zcode re-enters a persisted session via `--resume sess_...`; the mode
        // is pinned again (a resumed `-p` turn would otherwise run as yolo).
        "zcode" => Some(v(&[
            "--resume",
            "{session}",
            "-p",
            "{prompt}",
            "--json",
            "--mode",
            "build",
        ])),
        _ => None,
    }
}

/// `base` turned into its resume form for `session` (the agent's declarative
/// resume argv with the id substituted, spec 2026-07-20 Task 7). The binary,
/// autonomy flags and transport stay; the follow-up always travels as argv
/// `{prompt}`. `None` for an agent with no resume form.
pub fn resume_spec(
    base: &apb_core::config::InvocationDef,
    agent: &str,
    session: &str,
) -> Option<apb_core::config::InvocationDef> {
    let argv = resume_argv(agent)?
        .into_iter()
        .map(|a| {
            if a == "{session}" {
                session.to_string()
            } else {
                a
            }
        })
        .collect();
    Some(apb_core::config::InvocationDef {
        argv,
        prompt_via: apb_core::config::PromptVia::Argv,
        ..base.clone()
    })
}

/// How a fresh attempt makes its agent session findable, so a retry, a
/// fallback back onto the same binding or a deadline continuation can resume
/// it instead of re-sending the whole prompt (issue #136 items 2 and 3).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FreshSession {
    /// apb picks the id and hands it over at launch (claude `--session-id`),
    /// so the session is known even when the attempt is killed at its
    /// deadline, before it printed anything.
    Assigned { id: String, args: Vec<String> },
    /// The session gets a unique title at launch and is looked up by it
    /// afterwards (opencode `--title`, then `session list --format json`), so
    /// it is found even when the attempt printed no id.
    Titled { title: String, args: Vec<String> },
    /// Only an id the agent prints itself (on a normal exit) is known.
    Printed,
}

/// The [`FreshSession`] form of `agent_id` for a fresh attempt; `title` must be
/// unique to the attempt (run, node and attempt number).
pub fn fresh_session(agent_id: &str, title: &str) -> FreshSession {
    match apb_core::detect::canonical_agent_id(agent_id) {
        // Verified against `claude --help`: `--session-id <uuid>` uses a
        // specific session id; `--resume <id>` continues it.
        "claude" => {
            let id = uuid::Uuid::new_v4().to_string();
            FreshSession::Assigned {
                args: vec!["--session-id".to_string(), id.clone()],
                id,
            }
        }
        // Verified against `opencode run --help` (1.18): `--title` names the
        // session; `opencode session list --format json` lists `id` + `title`.
        "opencode" => FreshSession::Titled {
            args: vec!["--title".to_string(), title.to_string()],
            title: title.to_string(),
        },
        _ => FreshSession::Printed,
    }
}

/// Finds the id of the opencode session titled `title` by running
/// `<program> session list --format json` in `workdir` (the listing is scoped
/// to the project the directory belongs to). `None` when the listing fails or
/// holds no such session: an attempt killed before its first assistant
/// message never persists one, and then there is nothing to resume.
pub fn lookup_titled_session(program: &Path, workdir: &Path, title: &str) -> Option<String> {
    let mut cmd = std::process::Command::new(program);
    cmd.args(["session", "list", "--format", "json"])
        .current_dir(workdir)
        .stdin(std::process::Stdio::null());
    let out = crate::proc::run_capture(cmd, Some(std::time::Duration::from_secs(15)), None).ok()?;
    if !out.status.is_some_and(|s| s.success()) {
        return None;
    }
    let listed: Vec<serde_json::Value> = serde_json::from_str(out.stdout.trim()).ok()?;
    listed.into_iter().find_map(|s| {
        (s.get("title")?.as_str()? == title).then(|| s.get("id")?.as_str().map(str::to_string))?
    })
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
        // one of the built-in ten: historical compatibility falls back to
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
/// then `agents.<id>.program`, then the default binary of the built-in agent
/// table (`apb_core::detect::default_program`; detection probes the same
/// program). Shared source for both the adapter and
/// the manifest fingerprint - otherwise env drift would trigger falsely.
pub fn program_for(agent_id: &str, global: &GlobalConfig) -> String {
    if let Ok(p) = std::env::var("APB_AGENT_CMD") {
        return p;
    }
    global
        .agent_program(agent_id)
        .unwrap_or_else(|| apb_core::detect::default_program(agent_id))
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

    /// The agents core names as able to continue a session (what `apb
    /// validate` warns on for `continue_session`) are exactly those that have
    /// a resume form here and whose session id apb learns: assigned or titled
    /// at launch, or printed (codex, zcode).
    #[test]
    fn session_continuing_agents_have_a_resume_form() {
        for agent in apb_core::detect::SESSION_CONTINUING_AGENTS {
            assert!(resume_argv(agent).is_some(), "{agent}");
        }
        assert!(matches!(
            fresh_session("claude", "t"),
            FreshSession::Assigned { .. }
        ));
        assert!(matches!(
            fresh_session("opencode", "t"),
            FreshSession::Titled { .. }
        ));
        assert!(!apb_core::detect::continues_sessions("grok"));
        assert!(apb_core::detect::continues_sessions("claude-code"));
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
            "claude", "agy", "codex", "opencode", "hermes", "grok", "cursor", "qoder", "zcode",
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
    /// the prompt slot comes last, after `--` and every option.
    #[test]
    fn builtin_cursor_form() {
        let spec = builtin("cursor").expect("cursor builtin spec");
        assert_eq!(
            spec.argv,
            vec!["-p", "--model", "{model}", "--", "{prompt}"]
        );
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
                "--",
                "{prompt}"
            ]
        );
    }

    /// qoder's `-p` is a boolean print flag and the prompt is POSITIONAL, so
    /// the prompt slot must come last, after every option, like cursor. It
    /// delivers the SOUL natively via `--append-system-prompt` and resumes
    /// with `--resume`, both verified against `@qoder-ai/qodercli` 1.1.22
    /// `--help`.
    #[test]
    fn builtin_qoder_form() {
        let spec = builtin("qoder").expect("qoder builtin spec");
        assert_eq!(
            spec.argv,
            vec![
                "-p",
                "--output-format",
                "text",
                "--model",
                "{model}",
                "--",
                "{prompt}"
            ]
        );
        assert_eq!(spec.soul, SoulDelivery::Native);
        assert_eq!(spec.soul_flag.as_deref(), Some("--append-system-prompt"));
        assert_eq!(spec.transport, Transport::Headless);
        assert_eq!(
            spec.autonomous_args,
            vec!["--permission-mode", "bypass_permissions"]
        );
        assert_eq!(spec.interaction, Interaction::Resume);
        assert_eq!(
            resume_argv("qoder").expect("qoder resume argv"),
            vec![
                "--resume",
                "{session}",
                "-p",
                "--output-format",
                "text",
                "--model",
                "{model}",
                "--",
                "{prompt}"
            ]
        );
    }

    /// zcode must never fall into its implicit `yolo` default: the base form
    /// and the resume form both pin `--mode build`, and only the autonomy
    /// flags (appended after them, last value wins) switch to `yolo`.
    #[test]
    fn builtin_zcode_form_pins_an_explicit_mode() {
        let spec = builtin("zcode").expect("zcode builtin spec");
        assert_eq!(
            spec.argv,
            vec!["-p", "{prompt}", "--json", "--mode", "build"]
        );
        assert_eq!(spec.soul, SoulDelivery::Prefix);
        assert_eq!(spec.soul_flag, None);
        assert_eq!(spec.transport, Transport::Headless);
        assert_eq!(spec.autonomous_args, vec!["--mode", "yolo"]);
        assert_eq!(spec.interaction, Interaction::Resume);
        assert!(!spec.argv.iter().any(|a| a == "{model}"));
        assert_eq!(
            resume_argv("zcode").expect("zcode resume argv"),
            vec![
                "--resume",
                "{session}",
                "-p",
                "{prompt}",
                "--json",
                "--mode",
                "build"
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

    /// claude, codex and opencode are asked for their machine output, in the
    /// launch form and the resume form alike: it carries the attempt's token
    /// usage and session id (issue #167). Verified against claude 2.1.283,
    /// `codex exec` / `codex exec resume` 0.157.0 and `opencode run` 1.18.32.
    #[test]
    fn builtin_forms_ask_for_machine_output() {
        let argv = |a: &str| builtin(a).expect("builtin").argv;
        assert_eq!(
            argv("claude"),
            vec![
                "-p",
                "--output-format",
                "json",
                "--model",
                "{model}",
                "--",
                "{prompt}"
            ]
        );
        assert_eq!(
            argv("codex"),
            vec!["exec", "--json", "-m", "{model}", "--", "{prompt}"]
        );
        assert_eq!(
            argv("opencode"),
            vec!["run", "--format", "json", "-m", "{model}", "--", "{prompt}"]
        );
        assert_eq!(
            resume_argv("claude").unwrap(),
            vec![
                "--resume",
                "{session}",
                "-p",
                "--output-format",
                "json",
                "--model",
                "{model}",
                "--",
                "{prompt}"
            ]
        );
        assert_eq!(
            resume_argv("codex").unwrap(),
            vec![
                "exec",
                "resume",
                "{session}",
                "--json",
                "-m",
                "{model}",
                "--",
                "{prompt}"
            ]
        );
        assert_eq!(
            resume_argv("opencode").unwrap(),
            vec![
                "run",
                "--session",
                "{session}",
                "--format",
                "json",
                "-m",
                "{model}",
                "--",
                "{prompt}"
            ]
        );
    }

    /// Verified against the local `opencode run --help`: `--auto` auto-approves
    /// permissions that are not explicitly denied.
    #[test]
    fn builtin_opencode_passes_a_non_interactive_flag() {
        let spec = builtin("opencode").expect("opencode is a builtin");
        assert_eq!(spec.autonomous_args, vec!["--auto"]);
    }

    /// Verified against the local `codex exec --help`: skips all confirmation
    /// prompts and runs without sandboxing, the one-shot equivalent of claude's
    /// bypassPermissions.
    #[test]
    fn builtin_codex_passes_a_non_interactive_flag() {
        let spec = builtin("codex").expect("codex is a builtin");
        assert_eq!(
            spec.autonomous_args,
            vec!["--dangerously-bypass-approvals-and-sandbox"]
        );
    }

    /// Verified against the local `agy --help`: auto-approves all tool
    /// permission requests, the exact equivalent of claude's flag.
    #[test]
    fn builtin_agy_passes_a_non_interactive_flag() {
        let spec = builtin("agy").expect("agy is a builtin");
        assert_eq!(spec.autonomous_args, vec!["--dangerously-skip-permissions"]);
    }

    /// hermes deliberately carries NO autonomous flag: its `--yolo` could not be
    /// verified in combination with the `-z` one-shot form, and an unverified
    /// flag on every invocation is worse than a documented gap. The doctor warns
    /// instead. Change this test only together with a live verification.
    #[test]
    fn builtin_hermes_carries_no_unverified_autonomous_flag() {
        let spec = builtin("hermes").expect("hermes is a builtin");
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
