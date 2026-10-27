//! Run-scoped agent config isolation (spec 2026-07-21 run-reliability).
//!
//! A node agent is spawned as a headless one-shot process that inherits apb's
//! environment, so it also inherits the user's INTERACTIVE agent configuration.
//! For codex that means `~/.codex/config.toml`, whose `[mcp_servers.*]` tables
//! make codex launch the user's whole MCP server zoo on startup - including apb
//! itself, recursively, inside the run. One such spawn wedged a run for over an
//! hour. To stop that, each spawned codex gets a run-scoped, isolated
//! `CODEX_HOME` that carries only its credentials plus a config with every
//! `mcp_servers` table stripped out.
//!
//! The mechanism is per-agent so claude/opencode/hermes can gain a strategy
//! later; only codex is wired today. Every other agent returns `Ok(None)` and
//! keeps its current behavior (claude's live `--mcp-config` injection must keep
//! working, so claude must never gain a config home here).

use std::path::{Path, PathBuf};

use crate::error::EngineError;

mod codex_toml;

/// The environment variable codex reads for its configuration root. Verified
/// against the installed codex CLI: it resolves `$CODEX_HOME` (defaulting to
/// `~/.codex`) and reads `config.toml` and `auth.json` from it.
pub(crate) const CODEX_HOME_ENV: &str = "CODEX_HOME";

/// Prepares an isolated configuration home for `agent` under `run_dir`, scoped
/// to `node`, and returns the single env var (name -> directory) to set on the
/// spawned process. `Ok(None)` for every agent without an isolation strategy
/// (all agents except codex today), which leaves that agent's spawn unchanged.
pub(crate) fn env_for_agent(
    agent: &str,
    run_dir: &Path,
    node: &str,
) -> Result<Option<(&'static str, PathBuf)>, EngineError> {
    prepare(agent, source_codex_home().as_deref(), run_dir, node)
}

/// Core of [`env_for_agent`] with the real source home passed in, so tests can
/// drive it against a controlled source directory instead of the machine's
/// `~/.codex`.
fn prepare(
    agent: &str,
    source: Option<&Path>,
    run_dir: &Path,
    node: &str,
) -> Result<Option<(&'static str, PathBuf)>, EngineError> {
    match agent {
        "codex" => {
            let home = run_dir.join("agent-home").join("codex").join(node);
            prepare_codex_home(source, &home)?;
            Ok(Some((CODEX_HOME_ENV, home)))
        }
        _ => Ok(None),
    }
}

/// The real codex config root the spawned agent would otherwise inherit: an
/// explicit `$CODEX_HOME` if set and non-empty, else `~/.codex`. `None` when
/// neither is resolvable (no `$HOME`), which yields an empty isolated home.
pub(crate) fn source_codex_home() -> Option<PathBuf> {
    if let Ok(explicit) = std::env::var(CODEX_HOME_ENV)
        && !explicit.is_empty()
    {
        return Some(PathBuf::from(explicit));
    }
    std::env::var("HOME")
        .ok()
        .filter(|h| !h.is_empty())
        .map(|h| PathBuf::from(h).join(".codex"))
}

/// Materializes the isolated codex home at `dest`. The directory always exists
/// afterward; `auth.json` is copied verbatim (0600) when the source has one; a
/// `config.toml` is written with every `[mcp_servers.*]` table removed when the
/// source has one. A missing source (or missing files within it) is fine - the
/// home is simply emptier and codex falls back to its built-in defaults, which
/// is exactly the isolation we want. Nothing beyond `auth.json` is ever copied,
/// so no secret is logged or duplicated anywhere else.
fn prepare_codex_home(source: Option<&Path>, dest: &Path) -> Result<(), EngineError> {
    std::fs::create_dir_all(dest)?;
    let Some(source) = source else {
        return Ok(());
    };
    let auth = source.join("auth.json");
    if auth.is_file() {
        // Copied through the private-write path so the credential lands 0600
        // and is never held in a world-readable temp file en route.
        let bytes = std::fs::read(&auth)?;
        apb_core::fsutil::atomic_write_private(&dest.join("auth.json"), &bytes)?;
    }
    let config = source.join("config.toml");
    if config.is_file() {
        let stripped = strip_mcp_servers(&std::fs::read_to_string(&config)?);
        apb_core::fsutil::atomic_write(&dest.join("config.toml"), stripped.as_bytes())?;
    }
    Ok(())
}

/// Returns `config` with every `mcp_servers` entry removed and every other
/// line kept verbatim: each `[mcp_servers...]` / `[[mcp_servers...]]` table
/// (nested tables like `[mcp_servers.x.env]` included) up to the next
/// header, and in the root table a dotted key (`mcp_servers.x.command = ...`)
/// or an inline table (`mcp_servers = { ... }`), with the continuation lines
/// of a value that spans several (a multi-line array). Line-oriented rather
/// than a full TOML round-trip: it preserves the model-relevant top-level
/// keys and any custom provider tables a user needs to authenticate, and
/// touches only the `mcp_servers` entries that caused the incident.
fn strip_mcp_servers(config: &str) -> String {
    let mut out = String::new();
    let mut in_root = true;
    let mut in_mcp_table = false;
    // Open brackets of a dropped value still running over the next lines.
    let mut open_value = 0isize;
    for line in config.lines() {
        if open_value > 0 {
            open_value += codex_toml::bracket_balance(line);
            continue;
        }
        if let Some(path) = codex_toml::header_path(line) {
            in_root = false;
            in_mcp_table = is_mcp_servers(&path);
        } else if let Some((key, value)) = codex_toml::key_value(line)
            && in_root
            && is_mcp_servers(&key)
        {
            open_value = codex_toml::bracket_balance(value);
            continue;
        }
        if !in_mcp_table {
            out.push_str(line);
            out.push('\n');
        }
    }
    out
}

/// Whether a key path starts at the top-level `mcp_servers` key.
fn is_mcp_servers(path: &[String]) -> bool {
    path.first().is_some_and(|k| k == "mcp_servers")
}

/// Every variable name a codex custom provider reads, from the
/// `[model_providers.*]` tables of the codex config under `source`: each
/// provider's `env_key` and the values of its `env_http_headers`, written as
/// a table, a dotted key or an inline table. A `process_env: allowlist`
/// keeps them. Empty without a config. Names only.
pub(crate) fn provider_env_keys(source: Option<&Path>) -> Vec<String> {
    let Some(config) = source.and_then(|s| std::fs::read_to_string(s.join("config.toml")).ok())
    else {
        return Vec::new();
    };
    provider_env_keys_in(&config)
}

fn provider_env_keys_in(config: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut table: Vec<String> = Vec::new();
    for line in config.lines() {
        if let Some(path) = codex_toml::header_path(line) {
            table = path;
            continue;
        }
        for (path, value) in codex_toml::string_leaves(&table, line) {
            let names_a_variable = match path.as_slice() {
                [root, _, key] => root == "model_providers" && key == "env_key",
                [root, _, headers, _] => root == "model_providers" && headers == "env_http_headers",
                _ => false,
            };
            if names_a_variable && !value.is_empty() && !out.contains(&value) {
                out.push(value);
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const CONFIG_WITH_MCP: &str = "model = \"gpt-5.6\"\n\
model_reasoning_effort = \"medium\"\n\
\n\
[model_providers.custom]\n\
base_url = \"https://example.test\"\n\
env_key = \"CUSTOM_KEY\"\n\
\n\
[mcp_servers.example-index]\n\
command = \"example-index\"\n\
args = [ \"serve\", \"--mcp\" ]\n\
\n\
[mcp_servers.example-server.env]\n\
EXAMPLE_AGENT_NAME = \"codex\"\n\
\n\
[desktop]\n\
ambient-suggestions-enabled = true\n";

    #[test]
    fn provider_env_keys_come_from_model_provider_tables_only() {
        let config = format!(
            "{CONFIG_WITH_MCP}[model_providers.other] # trailing comment\nenv_key = 'OTHER_KEY' # comment\n[profiles.x]\nenv_key = \"NOT_A_PROVIDER\"\n[model_providers]\nazure = {{ name = \"Azure\", env_key = \"AZURE_KEY\", env_http_headers = {{ \"X-Org\" = \"ORG_VAR\" }} }}\nlocal.env_key = \"LOCAL_KEY\"\n"
        );
        assert_eq!(
            provider_env_keys_in(&config),
            vec![
                "CUSTOM_KEY".to_string(),
                "OTHER_KEY".to_string(),
                "AZURE_KEY".to_string(),
                "ORG_VAR".to_string(),
                "LOCAL_KEY".to_string(),
            ]
        );
        assert!(provider_env_keys(None).is_empty());
    }

    /// A header with a trailing comment or surrounding whitespace still
    /// opens its table, so its `mcp_servers` body is stripped.
    #[test]
    fn a_commented_mcp_servers_header_is_stripped_too() {
        let out = strip_mcp_servers(
            "model = \"m\"\n  [mcp_servers.x]   # mine\ncommand = \"x\"\n[desktop]\n",
        );
        assert!(!out.contains("command"), "{out}");
        assert!(out.contains("[desktop]"));
    }

    /// MCP servers given as root dotted keys (a multi-line array value
    /// included) or as one inline table are dropped like a table; the same
    /// key under another table is not the top-level one and stays.
    #[test]
    fn root_dotted_and_inline_mcp_servers_are_stripped() {
        let out = strip_mcp_servers(
            "model = \"m\"\nmcp_servers.x.command = \"x-bin\"\nmcp_servers.x.args = [\n  \"serve\", # [\n  \"--mcp\",\n]\n\"mcp_servers\" = { y = { command = \"y-bin\" } }\nkeep = 1\n[profiles.p]\nmcp_servers.z = \"kept\"\n",
        );
        assert_eq!(
            out,
            "model = \"m\"\nkeep = 1\n[profiles.p]\nmcp_servers.z = \"kept\"\n"
        );
    }

    #[test]
    fn strip_drops_every_mcp_servers_table_and_keeps_the_rest() {
        let out = strip_mcp_servers(CONFIG_WITH_MCP);
        assert!(
            !out.contains("mcp_servers"),
            "no mcp_servers table survives: {out}"
        );
        assert!(
            !out.contains("example-index"),
            "an mcp server body must be gone: {out}"
        );
        assert!(
            !out.contains("EXAMPLE_AGENT_NAME"),
            "a nested mcp env table must be gone: {out}"
        );
        // Model-relevant top-level keys and unrelated tables are preserved.
        assert!(out.contains("model = \"gpt-5.6\""));
        assert!(out.contains("model_reasoning_effort"));
        assert!(out.contains("[model_providers.custom]"));
        assert!(out.contains("env_key = \"CUSTOM_KEY\""));
        assert!(out.contains("[desktop]"));
    }

    #[test]
    fn strip_is_idempotent_and_noop_without_mcp_servers() {
        let clean = "model = \"m\"\n\n[desktop]\nx = 1\n";
        assert_eq!(strip_mcp_servers(clean), clean);
    }

    #[test]
    fn prepare_codex_home_copies_auth_and_strips_config() {
        let src = tempfile::tempdir().unwrap();
        std::fs::write(src.path().join("auth.json"), b"{\"token\":\"secret\"}").unwrap();
        std::fs::write(src.path().join("config.toml"), CONFIG_WITH_MCP).unwrap();

        let dst = tempfile::tempdir().unwrap();
        let home = dst.path().join("codex-home");
        prepare_codex_home(Some(src.path()), &home).unwrap();

        let auth = std::fs::read(home.join("auth.json")).unwrap();
        assert_eq!(
            auth, b"{\"token\":\"secret\"}",
            "auth.json is copied verbatim"
        );
        let config = std::fs::read_to_string(home.join("config.toml")).unwrap();
        assert!(
            !config.contains("mcp_servers"),
            "the isolated config carries no mcp_servers"
        );
        assert!(config.contains("model = \"gpt-5.6\""), "model keys survive");

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            let mode = std::fs::metadata(home.join("auth.json"))
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o600, "the copied credential is owner-only");
        }
    }

    #[test]
    fn prepare_codex_home_missing_source_yields_an_empty_home() {
        let dst = tempfile::tempdir().unwrap();
        let home = dst.path().join("codex-home");
        // A source path that does not exist stands in for a machine with no
        // ~/.codex: the home is created but carries neither file, no crash.
        let missing = dst.path().join("does-not-exist");
        prepare_codex_home(Some(&missing), &home).unwrap();
        assert!(home.is_dir(), "the isolated home directory is created");
        assert!(!home.join("auth.json").exists());
        assert!(!home.join("config.toml").exists());

        // A `None` source (no $HOME/$CODEX_HOME resolvable) is equally fine.
        let home2 = dst.path().join("codex-home-2");
        prepare_codex_home(None, &home2).unwrap();
        assert!(home2.is_dir());
    }

    #[test]
    fn env_for_agent_isolates_codex_and_leaves_others_untouched() {
        let src = tempfile::tempdir().unwrap();
        std::fs::write(src.path().join("config.toml"), CONFIG_WITH_MCP).unwrap();
        let run = tempfile::tempdir().unwrap();

        // codex gets a CODEX_HOME under the run dir with a stripped config.
        let (name, home) = prepare("codex", Some(src.path()), run.path(), "build")
            .unwrap()
            .expect("codex has an isolation strategy");
        assert_eq!(name, "CODEX_HOME");
        assert_eq!(
            home,
            run.path().join("agent-home").join("codex").join("build")
        );
        let config = std::fs::read_to_string(home.join("config.toml")).unwrap();
        assert!(!config.contains("mcp_servers"));

        // claude (and every other agent) has no strategy - spawn is unchanged.
        assert!(
            prepare("claude", Some(src.path()), run.path(), "build")
                .unwrap()
                .is_none()
        );
        assert!(
            prepare("opencode", Some(src.path()), run.path(), "build")
                .unwrap()
                .is_none()
        );
    }
}
