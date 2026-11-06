//! The declared write scope (spec node-contracts D3): the globs an
//! `agent_task` or `script` attempt may change, relative to the node's
//! directory, and the pure rules one glob or rendered template must satisfy.

/// The most entries one scope may name (V170).
pub const MAX_SCOPE_ENTRIES: usize = 200;

/// Why one glob cannot be a scope entry, or `None`.
pub fn entry_problem(glob: &str) -> Option<&'static str> {
    if glob.trim().is_empty() {
        return Some("an empty entry");
    }
    if glob.starts_with('/') || glob.starts_with('\\') {
        return Some("a path outside the node's directory (it starts with `/`)");
    }
    if glob.as_bytes().get(1) == Some(&b':') {
        return Some("a drive-prefixed path");
    }
    for seg in glob.split(['/', '\\']) {
        if seg == ".." {
            return Some("a path outside the node's directory (`..`)");
        }
        if seg == "." {
            return Some("a `.` segment");
        }
    }
    if glob.contains("{{") {
        return Some("an unresolved template reference");
    }
    globset::Glob::new(glob).err().map(|_| "not a valid glob")
}

/// Whether `glob` names a path outside the node's directory (absolute, a
/// drive prefix or a `..` segment); [`entry_problem`] refuses it too.
fn is_outside(glob: &str) -> bool {
    glob.starts_with('/')
        || glob.starts_with('\\')
        || glob.as_bytes().get(1) == Some(&b':')
        || glob.split(['/', '\\']).any(|seg| seg == "..")
}

/// Why a rendered template cannot be a scope.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RenderProblem {
    /// The rendered text is empty or whitespace only.
    Empty,
    /// The rendered value is a list that names no entry.
    EmptyList,
    /// The rendered text as a whole is unusable (not a JSON array of
    /// strings, too many entries); the text says why.
    Whole(String),
    /// One entry [`entry_problem`] refuses.
    Entry {
        entry: String,
        problem: &'static str,
        /// The entry names a path outside the node's directory.
        outside: bool,
    },
}

/// The first entry of `entries` [`entry_problem`] refuses, as a
/// [`RenderProblem::Entry`].
pub fn first_entry_problem(entries: &[String]) -> Option<RenderProblem> {
    entries.iter().find_map(|entry| {
        entry_problem(entry).map(|problem| RenderProblem::Entry {
            entry: entry.clone(),
            problem,
            outside: is_outside(entry),
        })
    })
}

/// The entries a rendered template names: a JSON array of strings, else one
/// entry per non-empty trimmed line.
pub fn parse_rendered(text: &str) -> Result<Vec<String>, RenderProblem> {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return Err(RenderProblem::Empty);
    }
    let not_array = || RenderProblem::Whole("the rendered value is not a JSON array".to_string());
    let entries: Vec<String> = if trimmed.starts_with('[') {
        let parsed: serde_json::Value = serde_json::from_str(trimmed).map_err(|_| not_array())?;
        let Some(items) = parsed.as_array() else {
            return Err(not_array());
        };
        let mut out = Vec::with_capacity(items.len());
        for item in items {
            let Some(s) = item.as_str() else {
                return Err(RenderProblem::Whole(
                    "the rendered array holds a value that is not a string".to_string(),
                ));
            };
            if s.is_empty() {
                return Err(RenderProblem::Whole(
                    "the rendered array holds an empty entry".to_string(),
                ));
            }
            out.push(s.to_string());
        }
        out
    } else {
        trimmed
            .lines()
            .map(str::trim)
            .filter(|l| !l.is_empty())
            .map(str::to_string)
            .collect()
    };
    if entries.is_empty() {
        return Err(RenderProblem::EmptyList);
    }
    if entries.len() > MAX_SCOPE_ENTRIES {
        return Err(RenderProblem::Whole(format!(
            "the rendered scope names {} entries, more than the {} a scope may name",
            entries.len(),
            MAX_SCOPE_ENTRIES
        )));
    }
    match first_entry_problem(&entries) {
        Some(problem) => Err(problem),
        None => Ok(entries),
    }
}

/// The single template reference of a template-form scope, when the text is
/// exactly one `{{...}}` with only whitespace around it.
pub fn template_ref(text: &str) -> Option<&str> {
    let trimmed = text.trim();
    let inner = trimmed.strip_prefix("{{")?.strip_suffix("}}")?;
    if inner.contains("{{") {
        return None;
    }
    Some(inner.trim())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn entry_problem_names_every_refused_shape() {
        let cases: [(&str, bool); 8] = [
            ("", true),
            ("/a", true),
            ("C:/a", true),
            ("a/../b", true),
            ("./a", true),
            ("{{x}}", true),
            ("[", true),
            ("src/export/**", false),
        ];
        for (glob, bad) in cases {
            assert_eq!(entry_problem(glob).is_some(), bad, "{glob}");
        }
    }

    #[test]
    fn parse_rendered_reads_a_json_array_or_a_line_list() {
        assert_eq!(
            parse_rendered(r#"["src/a.rs", "tests/a.rs"]"#).unwrap(),
            vec!["src/a.rs".to_string(), "tests/a.rs".to_string()]
        );
        assert_eq!(
            parse_rendered("src/a.rs\n\ntests/a.rs\n").unwrap(),
            vec!["src/a.rs".to_string(), "tests/a.rs".to_string()]
        );
        assert_eq!(parse_rendered(""), Err(RenderProblem::Empty));
        assert_eq!(parse_rendered("   \n"), Err(RenderProblem::Empty));
        // An empty list names no file: refused, not an empty scope.
        assert_eq!(parse_rendered("[]"), Err(RenderProblem::EmptyList));
        assert!(matches!(
            parse_rendered("[1, 2]"),
            Err(RenderProblem::Whole(_))
        ));
        assert!(matches!(
            parse_rendered(&format!("[{}]", "\"a\",".repeat(MAX_SCOPE_ENTRIES + 1))),
            Err(RenderProblem::Whole(_))
        ));
        assert_eq!(
            parse_rendered(r#"["src/a.rs", "../x"]"#),
            Err(RenderProblem::Entry {
                entry: "../x".to_string(),
                problem: "a path outside the node's directory (`..`)",
                outside: true,
            })
        );
        assert!(matches!(
            parse_rendered(r#"["["]"#),
            Err(RenderProblem::Entry { outside: false, .. })
        ));
    }

    #[test]
    fn template_ref_takes_exactly_one_reference() {
        assert_eq!(
            template_ref("{{ nodes.plan.output.files }}"),
            Some("nodes.plan.output.files")
        );
        assert_eq!(
            template_ref("{{nodes.plan.output.files}}"),
            Some("nodes.plan.output.files")
        );
        assert_eq!(template_ref("{{a}} {{b}}"), None);
        assert_eq!(template_ref("see {{a}}"), None);
        assert_eq!(template_ref("plain"), None);
    }
}
