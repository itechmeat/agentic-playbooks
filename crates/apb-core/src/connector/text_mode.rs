//! Text modes for string arguments of a connector function (`text_args`).
//!
//! An agent writes the text a connector sends, and some services act on that
//! text: a line that starts with a slash command, a mention that pings a
//! whole team, Markdown that changes what a one-line title says. A function
//! declares, per string argument, how the engine prepares the value before it
//! is substituted into the request body or form, so a template cannot bypass
//! it. The functions here are pure; the engine only calls `apply_text_args`.

use std::borrow::Cow;
use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::common::ConnectorError;

/// How one string argument is prepared before it reaches the request.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(try_from = "String", into = "&'static str")]
pub enum TextMode {
    /// Sent exactly as given: formatting and commands are intended.
    Markdown,
    /// Multi-line Markdown kept as written, except that a slash command at
    /// the start of any line is escaped so the service shows it as text.
    MarkdownNoCommands,
    /// One line of plain text: whitespace collapsed, every
    /// Markdown-significant character backslash-escaped.
    Literal,
}

impl TextMode {
    const ALL: [TextMode; 3] = [
        TextMode::Markdown,
        TextMode::MarkdownNoCommands,
        TextMode::Literal,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            TextMode::Markdown => "markdown",
            TextMode::MarkdownNoCommands => "markdown_no_commands",
            TextMode::Literal => "literal",
        }
    }

    /// Prepares `text` for this mode. Borrows when nothing changes.
    pub fn apply(self, text: &str) -> Cow<'_, str> {
        match self {
            TextMode::Markdown => Cow::Borrowed(text),
            TextMode::MarkdownNoCommands => escape_line_commands(text),
            TextMode::Literal => escape_literal(text),
        }
    }
}

impl TryFrom<String> for TextMode {
    type Error = String;

    fn try_from(name: String) -> Result<Self, Self::Error> {
        TextMode::ALL
            .into_iter()
            .find(|mode| mode.as_str() == name)
            .ok_or_else(|| {
                let known: Vec<&str> = TextMode::ALL.iter().map(|m| m.as_str()).collect();
                format!(
                    "unknown text mode `{name}`; expected one of: {}",
                    known.join(", ")
                )
            })
    }
}

impl From<TextMode> for &'static str {
    fn from(mode: TextMode) -> Self {
        mode.as_str()
    }
}

/// Returns `args` with every string argument named in `modes` prepared by its
/// mode. Arguments that are absent, null, or not strings are left alone (the
/// args schema already rejected a wrong type). Borrows when nothing changes.
pub fn apply_text_args<'a>(modes: &BTreeMap<String, TextMode>, args: &'a Value) -> Cow<'a, Value> {
    let Some(object) = args.as_object() else {
        return Cow::Borrowed(args);
    };
    let mut changed: Option<serde_json::Map<String, Value>> = None;
    for (name, mode) in modes {
        if let Some(Value::String(text)) = object.get(name)
            && let Cow::Owned(prepared) = mode.apply(text)
        {
            changed
                .get_or_insert_with(|| object.clone())
                .insert(name.clone(), Value::String(prepared));
        }
    }
    changed.map_or(Cow::Borrowed(args), |map| Cow::Owned(Value::Object(map)))
}

/// Manifest check for a function's `text_args`: modes apply only where a
/// request body is rendered (HTTP functions), every key must name a string
/// property of the function's `args_schema`, and no key may also appear in
/// one of the function's `outside` templates (its url, query and headers).
/// A mode is applied only to the body, so the same argument outside it would
/// go out unprepared while the manifest reads as if it were covered.
pub fn validate_text_args<'a>(
    function: &str,
    modes: &BTreeMap<String, TextMode>,
    args_schema: Option<&Value>,
    is_http: bool,
    outside: impl IntoIterator<Item = &'a str>,
) -> Result<(), ConnectorError> {
    if modes.is_empty() {
        return Ok(());
    }
    if !is_http {
        return Err(ConnectorError::Invalid(format!(
            "function `{function}` sets `text_args` but is not an HTTP function; text modes apply only to a rendered request body"
        )));
    }
    for name in modes.keys() {
        if !super::common::is_string_arg(args_schema, name) {
            return Err(ConnectorError::Invalid(format!(
                "function `{function}` sets `text_args.{name}`, but `{name}` is not a string property of its args_schema"
            )));
        }
    }
    for template in outside {
        // A malformed template is reported by the template check itself.
        let Ok(found) = super::template::placeholders(template) else {
            continue;
        };
        if let Some((_, name)) = found
            .iter()
            .find(|(ns, name)| *ns == super::template::Namespace::Args && modes.contains_key(name))
        {
            return Err(ConnectorError::Invalid(format!(
                "function `{function}` sets `text_args.{name}`, but `{name}` is also used in its url, query or headers, where a text mode is not applied; use it in the body only"
            )));
        }
    }
    Ok(())
}

/// True when `rest` starts with a slash command: `/` followed by an ASCII
/// letter, digit, or underscore.
fn starts_with_command(rest: &str) -> bool {
    let mut chars = rest.chars();
    chars.next() == Some('/')
        && chars
            .next()
            .is_some_and(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// `markdown_no_commands`: on every line, a slash command after optional
/// spaces and tabs gets a backslash. A slash in the middle of a line is not a
/// command and stays.
///
/// Lines inside fenced code blocks are escaped as well: the public
/// documentation of the services this mode targets does not promise that a
/// command inside a fence is ignored, so the fence gives no protection here.
/// The escape is visible only inside a fence, where Markdown keeps it.
///
/// A lone `\r` ends a line too: CommonMark reads it as a line ending, and a
/// service that strips `\r` before it looks for commands would otherwise see
/// a command at the start of a line this check never saw.
fn escape_line_commands(text: &str) -> Cow<'_, str> {
    let lines = || text.split_inclusive(['\n', '\r']);
    if !lines().any(line_has_command) {
        return Cow::Borrowed(text);
    }
    let mut out = String::with_capacity(text.len() + 8);
    for line in lines() {
        if line_has_command(line) {
            let rest = line.trim_start_matches([' ', '\t']);
            out.push_str(&line[..line.len() - rest.len()]);
            out.push('\\');
            out.push_str(rest);
        } else {
            out.push_str(line);
        }
    }
    Cow::Owned(out)
}

fn line_has_command(line: &str) -> bool {
    starts_with_command(line.trim_start_matches([' ', '\t']))
}

/// Characters a `literal` value escapes anywhere: CommonMark punctuation that
/// starts formatting, links, HTML, tables, or code, and the sigils services
/// use for references, mentions, and labels.
const LITERAL_ESCAPES: [char; 16] = [
    '\\', '`', '*', '_', '[', ']', '<', '>', '|', '~', '#', '&', '@', '!', '$', '%',
];

/// Characters that end a bare URL inside a `literal` value: past them the
/// text is escaped normally again.
const URL_STOPS: [char; 9] = ['<', '>', '`', '\\', '|', '[', ']', '"', '*'];

/// `literal`: one line, whitespace runs collapsed and the ends trimmed, every
/// Markdown-significant character escaped with a backslash (never an HTML
/// entity), and a value-start that would open a list, a setext heading, or a
/// slash command escaped too. A bare `http://` or `https://` URL is copied
/// as is, so the service still links it.
fn escape_literal(text: &str) -> Cow<'_, str> {
    let line = text.split_whitespace().collect::<Vec<_>>().join(" ");
    let start_marker = value_start_marker(&line);
    let mut out = String::with_capacity(line.len() * 2);
    let mut url_end = 0;
    let mut prev: Option<char> = None;
    for (i, c) in line.char_indices() {
        if i >= url_end
            && prev.is_none_or(|p| !p.is_alphanumeric())
            && let Some(len) = bare_url_len(&line[i..])
        {
            url_end = i + len;
        }
        if i >= url_end && (Some(i) == start_marker || LITERAL_ESCAPES.contains(&c)) {
            out.push('\\');
        }
        out.push(c);
        prev = Some(c);
    }
    if out == text {
        Cow::Borrowed(text)
    } else {
        Cow::Owned(out)
    }
}

/// The byte index of the character a value-start rule escapes, if any: a
/// leading `-`, `+`, or `=`; the `.` or `)` after a leading run of digits (an
/// ordered-list marker); a leading slash command.
fn value_start_marker(line: &str) -> Option<usize> {
    let first = line.chars().next()?;
    if matches!(first, '-' | '+' | '=') || starts_with_command(line) {
        return Some(0);
    }
    let digits = line.len() - line.trim_start_matches(|c: char| c.is_ascii_digit()).len();
    (digits > 0 && matches!(line[digits..].chars().next(), Some('.' | ')'))).then_some(digits)
}

/// Length in bytes of a bare URL at the start of `rest`, if one starts there.
fn bare_url_len(rest: &str) -> Option<usize> {
    let scheme = ["https://", "http://"].into_iter().find(|scheme| {
        rest.get(..scheme.len())
            .is_some_and(|head| head.eq_ignore_ascii_case(scheme))
    })?;
    let len = rest
        .find(|c: char| c.is_whitespace() || URL_STOPS.contains(&c))
        .unwrap_or(rest.len());
    (len > scheme.len()).then_some(len)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn no_commands(text: &str) -> String {
        TextMode::MarkdownNoCommands.apply(text).into_owned()
    }

    fn literal(text: &str) -> String {
        TextMode::Literal.apply(text).into_owned()
    }

    #[test]
    fn markdown_is_a_pass_through() {
        let text = "/close\n**bold** @all";
        assert!(matches!(TextMode::Markdown.apply(text), Cow::Borrowed(t) if t == text));
    }

    #[test]
    fn no_commands_escapes_a_command_at_any_line_start() {
        assert_eq!(no_commands("/close"), "\\/close");
        assert_eq!(no_commands("  \t/merge now"), "  \t\\/merge now");
        assert_eq!(
            no_commands("Done.\r\n/assign @me\nthanks"),
            "Done.\r\n\\/assign @me\nthanks"
        );
        assert_eq!(no_commands("/_x\n/9"), "\\/_x\n\\/9");
        // A `\r` is a line ending of its own, alone or before a newline.
        assert_eq!(no_commands("x\n\r/close"), "x\n\r\\/close");
        assert_eq!(no_commands("Done.\r/close"), "Done.\r\\/close");
    }

    #[test]
    fn no_commands_keeps_everything_that_is_not_a_command() {
        for text in [
            "see a/b and /close mid-line",
            "`/close` in a code span",
            "/ alone, //, /-x",
            "already \\/escaped",
            "**bold** @user #12 ~label",
        ] {
            assert!(
                matches!(TextMode::MarkdownNoCommands.apply(text), Cow::Borrowed(_)),
                "{text}"
            );
        }
    }

    #[test]
    fn no_commands_escapes_inside_a_fenced_block_too() {
        assert_eq!(no_commands("```\n/close\n```"), "```\n\\/close\n```");
    }

    #[test]
    fn literal_collapses_whitespace_to_one_line() {
        assert_eq!(literal("  Fix\n\tthe   build \r\n"), "Fix the build");
        assert!(matches!(
            TextMode::Literal.apply("Plain title"),
            Cow::Borrowed(_)
        ));
    }

    #[test]
    fn literal_escapes_every_markdown_significant_character() {
        assert_eq!(
            literal(r"a\b`c*d_e[f]g<h>i|j~k#l&m@n!o$p%q"),
            r"a\\b\`c\*d\_e\[f\]g\<h\>i\|j\~k\#l\&m\@n\!o\$p\%q"
        );
        assert_eq!(literal("Bug #12 @user ~label"), r"Bug \#12 \@user \~label");
    }

    #[test]
    fn literal_escapes_block_starts_only_at_the_value_start() {
        assert_eq!(literal("- item"), r"\- item");
        assert_eq!(literal("+ item"), r"\+ item");
        assert_eq!(literal("=== x"), r"\=== x");
        assert_eq!(literal("12. step"), r"12\. step");
        assert_eq!(literal("3) step"), r"3\) step");
        assert_eq!(literal("/close it"), r"\/close it");
        assert_eq!(literal("a - b + c = d 1. /x"), "a - b + c = d 1. /x");
        assert_eq!(literal("12 apples"), "12 apples");
        assert_eq!(literal("/ x"), "/ x");
    }

    #[test]
    fn literal_keeps_bare_urls_working() {
        assert_eq!(
            literal("See https://example.com/a_b?x=1&y=%20#frag now_ok"),
            r"See https://example.com/a_b?x=1&y=%20#frag now\_ok"
        );
        assert_eq!(
            literal("(HTTP://x.org/~u) <https://y>"),
            r"(HTTP://x.org/~u) \<https://y\>"
        );
        assert_eq!(literal("xhttps://a_b"), r"xhttps://a\_b");
        assert_eq!(literal("https:// x_y"), r"https:// x\_y");
    }

    #[test]
    fn apply_text_args_prepares_only_named_string_args() {
        let modes = BTreeMap::from([
            ("body".to_string(), TextMode::MarkdownNoCommands),
            ("title".to_string(), TextMode::Literal),
            ("count".to_string(), TextMode::Literal),
        ]);
        let args = json!({ "body": "/close", "title": "#1", "other": "/close", "count": 3 });
        assert_eq!(
            apply_text_args(&modes, &args).into_owned(),
            json!({ "body": "\\/close", "title": "\\#1", "other": "/close", "count": 3 })
        );
        let untouched = json!({ "body": "fine", "title": null });
        assert!(matches!(
            apply_text_args(&modes, &untouched),
            Cow::Borrowed(_)
        ));
    }
}
