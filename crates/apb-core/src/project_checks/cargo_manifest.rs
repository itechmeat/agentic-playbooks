//! The part of a Cargo manifest that decides which tests `cargo test` runs:
//! the `[package]` auto-discovery flags, `edition` and `build`, the `[lib]`
//! and every `[[test]]`, `[[bin]]`, `[[example]]` and `[[bench]]` target,
//! `[features]`, and the `[workspace]` members, exclusions and default
//! members. A version bump, a description edit or a dependency change
//! leaves it the same.
//!
//! No TOML parser is in the dependency tree, so this is a statement
//! scanner: it follows strings (basic, literal and their multi-line forms)
//! and open brackets across lines, so a line inside a multi-line array or
//! string is never read as a table header, and a `#` inside quotes is never
//! read as a comment.

/// The `[package]` keys that decide which targets exist and how they build.
const PACKAGE_KEYS: [&str; 6] = [
    "autotests",
    "autobins",
    "autoexamples",
    "autobenches",
    "edition",
    "build",
];

/// The `[workspace]` keys that decide which members `cargo test` covers.
const WORKSPACE_KEYS: [&str; 3] = ["members", "exclude", "default-members"];

/// The tables that are kept whole.
const WHOLE_TABLES: [&str; 6] = ["lib", "test", "bin", "example", "bench", "features"];

/// Whether the key at full dotted `path` (table segments, then key
/// segments) decides which tests run.
fn decides_tests(path: &[String]) -> bool {
    let second = || path.get(1).map(String::as_str).unwrap_or("");
    match path.first().map(String::as_str) {
        Some(t) if WHOLE_TABLES.contains(&t) => true,
        Some("package") => PACKAGE_KEYS.contains(&second()),
        Some("workspace") => WORKSPACE_KEYS.contains(&second()),
        _ => false,
    }
}

/// What the scanner carries from one line to the next: the closing
/// delimiter of an open string, and how many brackets are open.
#[derive(Default)]
struct Scan {
    string: Option<&'static str>,
    depth: usize,
}

impl Scan {
    /// Whether the next line starts a new statement.
    fn at_statement(&self) -> bool {
        self.string.is_none() && self.depth == 0
    }

    /// Follows `line`, returning it without its comment.
    fn line<'a>(&mut self, line: &'a str) -> &'a str {
        let b = line.as_bytes();
        let mut i = 0;
        while i < b.len() {
            if let Some(close) = self.string {
                if close.starts_with('"') && b[i] == b'\\' {
                    i += 2;
                } else if b[i..].starts_with(close.as_bytes()) {
                    self.string = None;
                    i += close.len();
                } else {
                    i += 1;
                }
                continue;
            }
            match b[i] {
                // An ASCII byte is always a character boundary in UTF-8.
                b'#' => return &line[..i],
                q @ (b'"' | b'\'') => {
                    let (single, triple) = if q == b'"' {
                        ("\"", "\"\"\"")
                    } else {
                        ("'", "'''")
                    };
                    let open = if b[i..].starts_with(triple.as_bytes()) {
                        triple
                    } else {
                        single
                    };
                    self.string = Some(open);
                    i += open.len();
                    continue;
                }
                b'[' | b'{' => self.depth += 1,
                b']' | b'}' => self.depth = self.depth.saturating_sub(1),
                _ => {}
            }
            i += 1;
        }
        // A single-line string never runs past its line.
        if matches!(self.string, Some("\"" | "'")) {
            self.string = None;
        }
        line
    }
}

/// The dotted segments of a TOML key or table name, quotes kept whole.
fn split_key(name: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let (mut start, mut quote) = (0, None);
    for (i, c) in name.char_indices() {
        match (quote, c) {
            (None, '"' | '\'') => quote = Some(c),
            (Some(q), _) if c == q => quote = None,
            (None, '.') => {
                out.push(name[start..i].trim());
                start = i + 1;
            }
            _ => {}
        }
    }
    out.push(name[start..].trim());
    out
}

/// The segments of a key or table name, unquoted.
fn path(name: &str) -> Vec<String> {
    split_key(name)
        .into_iter()
        .map(|s| {
            let unquoted = ['"', '\'']
                .iter()
                .find_map(|q| s.strip_prefix(*q).and_then(|s| s.strip_suffix(*q)));
            unquoted.unwrap_or(s).to_string()
        })
        .collect()
}

/// The byte offset of the first `=` outside quotes in `s`.
fn key_end(s: &str) -> Option<usize> {
    let mut quote = None;
    for (i, c) in s.char_indices() {
        match (quote, c) {
            (None, '"' | '\'') => quote = Some(c),
            (Some(q), _) if c == q => quote = None,
            (None, '=') => return Some(i),
            _ => {}
        }
    }
    None
}

/// One statement of the manifest: a table header, or a key with its value
/// (which may span lines).
enum Statement {
    Header { array: bool, path: Vec<String> },
    Pair { key: Vec<String>, value: String },
}

/// The statements of `text`, comments removed.
fn statements(text: &str) -> Vec<Statement> {
    let mut out = Vec::new();
    let mut scan = Scan::default();
    for line in text.lines() {
        let starts = scan.at_statement();
        let code = scan.line(line).trim();
        if !starts {
            if let Some(Statement::Pair { value, .. }) = out.last_mut() {
                value.push('\n');
                value.push_str(code);
            }
            continue;
        }
        if code.starts_with('[') {
            // A header's own brackets open no value.
            scan.depth = 0;
            let array = code.starts_with("[[");
            let name = code.trim_start_matches('[').trim_end_matches(']');
            out.push(Statement::Header {
                array,
                path: path(name),
            });
        } else if let Some(eq) = key_end(code) {
            out.push(Statement::Pair {
                key: path(&code[..eq]),
                value: code[eq + 1..].trim().to_string(),
            });
        }
    }
    out
}

/// The manifest reduced to what decides which tests run (see the module
/// doc), one normalized statement per line.
pub(super) fn test_selection(text: &str) -> String {
    let mut out = Vec::new();
    let mut table: Vec<String> = Vec::new();
    for statement in statements(text) {
        match statement {
            Statement::Header { array, path } => {
                if array && decides_tests(&path) {
                    out.push(format!("[[{}]]", path.join(".")));
                }
                table = path;
            }
            Statement::Pair { key, value } => {
                let full: Vec<String> = table.iter().cloned().chain(key).collect();
                if decides_tests(&full) {
                    out.push(format!("{} = {value}", full.join(".")));
                }
            }
        }
    }
    out.join("\n")
}
