//! The few TOML forms of a codex `config.toml` the agent home reads, line by
//! line: a table header with its key path (`[a.b]`, `[[a.b]]`, quoted keys,
//! a trailing comment), and a `key = value` line whose value is a string or
//! a one-line inline table, with basic-string escapes decoded. Not a TOML
//! parser: a form it does not recognize (a multi-line string) is passed
//! over; [`bracket_balance`] tells where a multi-line array ends.

/// The key path of a table header line, `None` when the line is not one. A
/// header may be indented and followed by whitespace and a `#` comment.
pub(super) fn header_path(line: &str) -> Option<Vec<String>> {
    let t = line.trim_start();
    let (body, close) = if let Some(b) = t.strip_prefix("[[") {
        (b, "]]")
    } else {
        (t.strip_prefix('[')?, "]")
    };
    let end = find_unquoted(body, close)?;
    let rest = body[end + close.len()..].trim_start();
    if !rest.is_empty() && !rest.starts_with('#') {
        return None;
    }
    Some(key_path(&body[..end]))
}

/// The key path and the raw value of a `key = value` line, `None` for a
/// blank line, a comment, a table header or any other line.
pub(super) fn key_value(line: &str) -> Option<(Vec<String>, &str)> {
    let t = line.trim_start();
    if t.is_empty() || t.starts_with('#') || t.starts_with('[') {
        return None;
    }
    let eq = find_unquoted(t, "=")?;
    Some((key_path(&t[..eq]), t[eq + 1..].trim_start()))
}

/// The string values of a `key = value` line under the table `table`, each
/// with its full key path: a string value as one pair, an inline table
/// flattened into one pair per string leaf. Empty for any other line.
pub(super) fn string_leaves(table: &[String], line: &str) -> Vec<(Vec<String>, String)> {
    let Some((key, value)) = key_value(line) else {
        return Vec::new();
    };
    let mut path = table.to_vec();
    path.extend(key);
    let mut out = Vec::new();
    flatten(path, value, &mut out);
    out
}

/// How many `[` / `{` the text opens minus how many it closes, outside
/// quoted strings and before a `#` comment: above zero, a value goes on
/// over the next lines (a multi-line array).
pub(super) fn bracket_balance(text: &str) -> isize {
    let mut balance = 0isize;
    scan(text, |_, c| {
        match c {
            '#' => return true,
            '{' | '[' => balance += 1,
            '}' | ']' => balance -= 1,
            _ => {}
        }
        false
    });
    balance
}

/// The dotted key path of `s`, each segment trimmed and unquoted.
pub(super) fn key_path(s: &str) -> Vec<String> {
    split_unquoted(s, '.')
        .into_iter()
        .map(|seg| unquote(seg.trim()))
        .collect()
}

fn flatten(path: Vec<String>, value: &str, out: &mut Vec<(Vec<String>, String)>) {
    if let Some(s) = string_value(value) {
        out.push((path, s));
    } else if let Some(inner) = value.strip_prefix('{')
        && let Some(end) = find_unquoted(inner, "}")
    {
        for entry in split_unquoted(&inner[..end], ',') {
            let Some(eq) = find_unquoted(entry, "=") else {
                continue;
            };
            let mut sub = path.clone();
            sub.extend(key_path(&entry[..eq]));
            flatten(sub, entry[eq + 1..].trim(), out);
        }
    }
}

/// The content of a basic (`"..."`) or literal (`'...'`) string at the
/// start of `v`. A basic string's escapes are decoded (`\"`, `\\`, `\t`,
/// `\uXXXX`, `\UXXXXXXXX`, ...); a literal string is taken as written.
/// `None` for anything else, an unterminated string or an invalid escape.
fn string_value(v: &str) -> Option<String> {
    let mut chars = v.chars();
    match chars.next()? {
        '"' => {
            let mut out = String::new();
            while let Some(c) = chars.next() {
                match c {
                    '"' => return Some(out),
                    '\\' => out.push(escape(&mut chars)?),
                    c => out.push(c),
                }
            }
            None
        }
        '\'' => chars.as_str().split_once('\'').map(|(s, _)| s.to_string()),
        _ => None,
    }
}

/// The character a basic-string escape stands for, read after its `\`.
fn escape(chars: &mut std::str::Chars) -> Option<char> {
    let hex = |chars: &mut std::str::Chars, n: usize| {
        let digits: String = chars.take(n).collect();
        if digits.len() != n {
            return None;
        }
        u32::from_str_radix(&digits, 16)
            .ok()
            .and_then(char::from_u32)
    };
    Some(match chars.next()? {
        'b' => '\u{8}',
        't' => '\t',
        'n' => '\n',
        'f' => '\u{c}',
        'r' => '\r',
        'e' => '\u{1b}',
        '"' => '"',
        '\\' => '\\',
        'u' => hex(chars, 4)?,
        'U' => hex(chars, 8)?,
        _ => return None,
    })
}

fn unquote(s: &str) -> String {
    string_value(s).unwrap_or_else(|| s.to_string())
}

/// Walks `s` outside quoted strings, calling `visit` with the byte index
/// and the character of each unquoted character (quotes included) until it
/// returns `true`. Shared by every reader here so they agree on quoting.
fn scan(s: &str, mut visit: impl FnMut(usize, char) -> bool) {
    let mut quote: Option<char> = None;
    let mut escaped = false;
    for (i, c) in s.char_indices() {
        if let Some(q) = quote {
            if escaped {
                escaped = false;
            } else if c == '\\' && q == '"' {
                escaped = true;
            } else if c == q {
                quote = None;
            }
            continue;
        }
        if visit(i, c) {
            return;
        }
        if c == '"' || c == '\'' {
            quote = Some(c);
        }
    }
}

/// The byte index of the first `pat` outside quoted strings and nested
/// `{}` / `[]`.
fn find_unquoted(s: &str, pat: &str) -> Option<usize> {
    let mut depth = 0usize;
    let mut found = None;
    scan(s, |i, c| {
        if depth == 0 && s[i..].starts_with(pat) {
            found = Some(i);
            return true;
        }
        match c {
            '{' | '[' => depth += 1,
            '}' | ']' => depth = depth.saturating_sub(1),
            _ => {}
        }
        false
    });
    found
}

/// `s` split on `sep` outside quoted strings and nested brackets.
fn split_unquoted(s: &str, sep: char) -> Vec<&str> {
    let mut out = Vec::new();
    let mut rest = s;
    let pat = sep.to_string();
    while let Some(i) = find_unquoted(rest, &pat) {
        out.push(&rest[..i]);
        rest = &rest[i + sep.len_utf8()..];
    }
    out.push(rest);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn path(p: &[&str]) -> Vec<String> {
        p.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn a_header_may_carry_whitespace_quotes_and_a_comment() {
        for line in [
            "[model_providers.custom]",
            "  [model_providers.custom]  ",
            "[model_providers.custom] # a comment",
            "[ model_providers . \"custom\" ]",
            "[[model_providers.custom]]#c",
        ] {
            assert_eq!(
                header_path(line),
                Some(path(&["model_providers", "custom"])),
                "{line}"
            );
        }
        assert_eq!(header_path("[a] b"), None);
        assert_eq!(header_path("a = [1]"), None);
        assert_eq!(header_path("[\"a]b\".c]"), Some(path(&["a]b", "c"])));
    }

    #[test]
    fn dotted_keys_and_inline_tables_flatten_to_their_leaves() {
        let table = path(&["model_providers"]);
        assert_eq!(
            string_leaves(&table, "azure.env_key = \"K1\" # c"),
            vec![(path(&["model_providers", "azure", "env_key"]), "K1".into())]
        );
        let leaves = string_leaves(
            &table,
            "az = { name = \"A, B\", env_key = 'K2', env_http_headers = { \"X-Key\" = \"K3\" } }",
        );
        assert!(leaves.contains(&(path(&["model_providers", "az", "env_key"]), "K2".into())));
        assert!(leaves.contains(&(
            path(&["model_providers", "az", "env_http_headers", "X-Key"]),
            "K3".into()
        )));
        assert!(leaves.contains(&(path(&["model_providers", "az", "name"]), "A, B".into())));
        assert!(string_leaves(&table, "n = 3").is_empty());
    }

    #[test]
    fn basic_strings_decode_escapes_and_literal_strings_stay_raw() {
        let table = path(&["model_providers"]);
        let value = |line: &str| string_leaves(&table, line).pop().map(|(_, v)| v);
        assert_eq!(
            value(r#"x.env_key = "\u004FRG_\U00000054OKEN""#).as_deref(),
            Some("ORG_TOKEN")
        );
        assert_eq!(
            value(r#"x = "a\"b\\c\td\ne""#).as_deref(),
            Some("a\"b\\c\td\ne")
        );
        assert_eq!(value(r#"x = '\u004FRG\n'"#).as_deref(), Some(r"\u004FRG\n"));
        for bad in [r#"x = "\q""#, r#"x = "\u12""#, r#"x = "\uD800""#] {
            assert_eq!(value(bad), None, "{bad}");
        }
        assert_eq!(header_path(r#"["a\u002Eb".c]"#), Some(path(&["a.b", "c"])));
    }

    #[test]
    fn bracket_balance_counts_unquoted_brackets_before_a_comment() {
        assert_eq!(bracket_balance("[ \"a\","), 1);
        assert_eq!(bracket_balance("[\"]\", { k = '[' }] # ["), 0);
        assert_eq!(bracket_balance("]"), -1);
    }
}
