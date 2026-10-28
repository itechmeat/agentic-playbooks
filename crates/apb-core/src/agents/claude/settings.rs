//! What claude itself accepts and loads: its model aliases and families,
//! and the settings apb hands it for the minimal environment.

/// Claude Code's own model aliases: it accepts them for `--model` and
/// resolves them itself, so they are known models, not typos.
pub const ALIASES: &[&str] = &["default", "opus", "sonnet", "haiku", "fable", "opusplan"];

/// The Claude model families apb knows. An unlisted id of one of them in
/// Anthropic's id pattern is accepted with a note.
pub const FAMILIES: &[&str] = &["opus", "sonnet", "haiku", "fable"];

/// Claude Code's suffix for the long-context variant of a model.
pub const LONG_CONTEXT_SUFFIX: &str = "[1m]";

/// Whether `id` is `claude-<family>-<major>-<minor>` or
/// `claude-<family>-<major>` with a known family and one- or two-digit
/// version parts, optionally followed by a `-YYYYMMDD` date
/// (`claude-sonnet-6-0`, `claude-opus-6`, `claude-haiku-5-5-20261007`).
pub fn is_family_id(id: &str) -> bool {
    let Some(rest) = id.strip_prefix("claude-") else {
        return false;
    };
    let parts: Vec<&str> = rest.split('-').collect();
    let digits = |s: &str, min: usize, max: usize| {
        (min..=max).contains(&s.len()) && s.bytes().all(|b| b.is_ascii_digit())
    };
    let Some((family, version)) = parts.split_first() else {
        return false;
    };
    if !FAMILIES.contains(family) {
        return false;
    }
    // The version: a major, an optional minor, then an optional date.
    let numbers = match version.split_last() {
        Some((last, rest)) if digits(last, 8, 8) => rest,
        _ => version,
    };
    (1..=2).contains(&numbers.len()) && numbers.iter().all(|n| digits(n, 1, 2))
}

/// The apb-owned minimal claude settings written for a hermetic run. It is the
/// highest-precedence settings source handed to claude via `--settings`, and it
/// disables the user-scope surfaces that would otherwise leak into the run:
/// - `hooks: {}` - no user Stop/PreToolUse/etc. hooks fire;
/// - `enabledPlugins: {}` - no user plugins load;
/// - `enableAllProjectMcpServers: false` - project MCP servers are not
///   auto-enabled.
///
/// Kept intentionally small and apb-owned; extend deliberately. The user
/// settings file itself is not loaded at all (see [`HERMETIC_FLAGS`]).
pub const HERMETIC_SETTINGS_JSON: &str =
    "{\n  \"hooks\": {},\n  \"enabledPlugins\": {},\n  \"enableAllProjectMcpServers\": false\n}\n";

/// claude's flags for the minimal environment, after `--settings <file>`:
/// only the project and local setting sources (the user's settings, CLAUDE.md,
/// skills and agents stay out) and only the MCP servers passed with
/// `--mcp-config` (apb's own ask-server on a live node). Checked with
/// `claude -p /context`, which reports the loaded context without a model
/// call: about 24k tokens of harness context before, 18k with these flags, on
/// one development setup (2026-09-25).
pub const HERMETIC_FLAGS: &[&str] = &["--setting-sources", "project,local", "--strict-mcp-config"];
