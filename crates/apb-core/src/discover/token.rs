//! Step tokens: an [`Action`] as a short name with no argument in it
//! (`git:commit`, `gh:pr.create`, `sh:<program>`, `edit:.rs`, `read:.md`,
//! `search`, `mcp:<server>.<tool>`, `web:fetch`, `agent:spawn`,
//! `skill:<name>`, `tool:<name>`).
//!
//! Every name a token is built from is lowercased and must match
//! `[a-z0-9._-]{1,40}`, else it becomes `other`: a token can never carry a
//! path, a value or a secret.

use crate::agent_actions::command::IN_PLACE;
use crate::agent_actions::{Action, OTHER};

/// The longest name piece a token keeps.
const PIECE_MAX: usize = 40;

/// `s` lowercased when it fits the token charset, else
/// [`crate::agent_actions::OTHER`].
fn piece(s: &str) -> String {
    let lower = s.to_ascii_lowercase();
    let fits = !lower.is_empty()
        && lower.len() <= PIECE_MAX
        && lower
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '.' | '_' | '-'));
    if fits { lower } else { OTHER.to_string() }
}

/// The extension of a path's file name with its dot (`.rs`), or `file`.
fn extension(path: &str) -> String {
    std::path::Path::new(path)
        .extension()
        .and_then(|e| e.to_str())
        .map_or_else(|| "file".to_string(), |e| piece(&format!(".{e}")))
}

/// The token of one action.
pub fn of(action: &Action) -> String {
    match action {
        Action::Command {
            skill: Some(name), ..
        }
        | Action::Read {
            skill: Some(name), ..
        }
        | Action::Skill { name } => format!("skill:{}", piece(name)),
        Action::Command {
            program,
            sub: Some(sub),
            ..
        } => format!("{}:{}", piece(program), piece(sub)),
        Action::Command { program, .. } => format!("sh:{}", piece(program)),
        Action::Edit { paths } => format!(
            "edit:{}",
            paths
                .first()
                .map_or_else(|| "file".to_string(), |p| extension(p))
        ),
        Action::Read { path, .. } => format!("read:{}", extension(path)),
        Action::Search => "search".to_string(),
        Action::Mcp { server, tool } => format!("mcp:{}.{}", piece(server), piece(tool)),
        Action::Web { kind } => format!("web:{}", kind.as_str()),
        Action::Agent => "agent:spawn".to_string(),
        Action::Tool { name } => format!("tool:{}", piece(name)),
    }
}

/// Command tokens that only look: a repeat made of these alone changes
/// nothing a playbook would do. (`cd`, `ls`, `pwd` and `echo` leave no
/// step at all; `sed -i` is `sed:inplace`, an edit.)
const READ_ONLY_COMMANDS: [&str; 41] = [
    "sh:cat",
    "sh:head",
    "sh:tail",
    "sh:less",
    "sh:more",
    "sh:bat",
    "sh:nl",
    "sh:grep",
    "sh:rg",
    "sh:find",
    "sh:fd",
    "sh:wc",
    "sh:sed",
    "sh:awk",
    "sh:tree",
    "sh:which",
    "sh:jq",
    "sh:stat",
    "sh:file",
    "sh:du",
    "sh:df",
    "sh:diff",
    "sh:sort",
    "sh:uniq",
    "sh:cut",
    "git:status",
    "git:log",
    "git:diff",
    "git:show",
    "git:branch",
    "git:rev-parse",
    "git:ls-files",
    "git:blame",
    "git:describe",
    "git:shortlog",
    "cargo:metadata",
    "cargo:tree",
    "gh:pr.view",
    "gh:pr.list",
    "gh:issue.view",
    "gh:issue.list",
];

/// Whether `token` does something: an edit, a command other than a
/// read-only one, an MCP call, a web call or a sub-agent.
pub fn is_acting(token: &str) -> bool {
    let Some((kind, _)) = token.split_once(':') else {
        return false;
    };
    match kind {
        "edit" | "mcp" | "web" | "agent" => true,
        "read" | "skill" | "tool" => false,
        _ => !READ_ONLY_COMMANDS.contains(&token),
    }
}

/// A token as a person reads it: `gh:pr.create` as `gh pr create`.
pub fn label(token: &str) -> String {
    let (kind, name) = token.split_once(':').unwrap_or((token, ""));
    match kind {
        "sh" => name.to_string(),
        _ if name.is_empty() => kind.to_string(),
        "edit" | "read" => format!("{kind} {name}"),
        _ => format!("{kind} {}", name.replace('.', " ")),
    }
}

/// Effects a repeat of `tokens` has: `fs_write` for an edit (an in-place
/// `sed -i` too), `network` and `external` for a push, `gh`, an MCP or web
/// call or a connector call (`apb connector call`).
pub fn effects(tokens: &[String]) -> Vec<&'static str> {
    let mut out = Vec::new();
    let in_place = format!(":{IN_PLACE}");
    if tokens
        .iter()
        .any(|t| t.starts_with("edit:") || t.ends_with(&in_place))
    {
        out.push("fs_write");
    }
    let outward = tokens.iter().any(|t| {
        t == "git:push"
            || t == "apb:connector.call"
            || ["gh:", "mcp:", "web:"].iter().any(|p| t.starts_with(p))
    });
    if outward {
        out.extend(["network", "external"]);
    }
    out
}
