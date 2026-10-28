//! The built-in agents as the rest of apb sees them: the detection probes
//! (ids in detect order, binaries, version arguments, home locations), the
//! alias, the agents that continue sessions and each agent's environment set.
//! A golden table: a change here changes `apb detect`, the dashboard or what
//! an allowlisted agent inherits.

use apb_core::detect::{
    self, AgentCategory, BUILTIN_ALIASES, SESSION_CONTINUING_AGENTS, builtin_probes,
};

#[test]
fn the_probes_are_pinned_in_detect_order() {
    use AgentCategory::{Aggregator, Vendor};
    let want: &[(&str, &[&str], AgentCategory, &[&str])] = &[
        ("claude", &["claude"], Vendor, &[]),
        ("codex", &["codex"], Vendor, &[]),
        ("agy", &["agy"], Aggregator, &[]),
        ("opencode", &["opencode"], Aggregator, &[]),
        ("pi", &["pi"], Aggregator, &[]),
        ("hermes", &["hermes"], Aggregator, &[]),
        ("grok", &["grok"], Vendor, &[]),
        ("cursor", &["cursor-agent"], Aggregator, &[]),
        ("qoder", &["qoder"], Aggregator, &[]),
        (
            "zcode",
            &["zcode"],
            Vendor,
            &[".zcode/server/agents/glm/zcode-agent"],
        ),
    ];
    let probes = builtin_probes();
    assert_eq!(probes.len(), want.len());
    for (p, (id, bins, category, home_paths)) in probes.iter().zip(want) {
        assert_eq!(p.id, *id);
        assert_eq!(p.bins, *bins, "{id}");
        assert_eq!(p.category, *category, "{id}");
        assert_eq!(p.version_args, ["--version"], "{id}");
        assert_eq!(p.home_paths, *home_paths, "{id}");
    }
}

#[test]
fn the_alias_and_the_session_continuing_agents_are_pinned() {
    assert_eq!(*BUILTIN_ALIASES, vec![("claude-code", "claude")]);
    assert_eq!(detect::canonical_agent_id("claude-code"), "claude");
    assert_eq!(detect::canonical_agent_id("custom"), "custom");
    assert_eq!(
        *SESSION_CONTINUING_AGENTS,
        vec!["claude", "codex", "opencode", "zcode"]
    );
    for id in [
        "claude", "codex", "agy", "opencode", "pi", "hermes", "grok", "qoder",
    ] {
        assert_eq!(detect::default_program(id), id);
    }
    assert_eq!(detect::default_program("cursor"), "cursor-agent");
    assert_eq!(detect::default_program("claude-code"), "claude");
    assert_eq!(detect::default_program("custom"), "custom");
}

#[test]
fn every_agent_env_set_is_pinned() {
    type Set = (
        &'static [&'static str],
        &'static [(&'static str, &'static [&'static str])],
    );
    let want: &[(&str, Set)] = &[
        (
            "claude",
            (
                &[
                    "ANTHROPIC_*",
                    "CLAUDE_CODE_*",
                    "CLAUDE_CONFIG_DIR",
                    "MCP_TIMEOUT",
                    "MCP_TOOL_TIMEOUT",
                    "DISABLE_TELEMETRY",
                    "DISABLE_AUTOUPDATER",
                    "MAX_THINKING_TOKENS",
                ],
                &[
                    ("CLAUDE_CODE_USE_BEDROCK", &["AWS_*"]),
                    (
                        "CLAUDE_CODE_USE_VERTEX",
                        &[
                            "GOOGLE_APPLICATION_CREDENTIALS",
                            "GOOGLE_CLOUD_PROJECT",
                            "GCLOUD_PROJECT",
                            "CLOUD_ML_REGION",
                            "VERTEX_REGION_*",
                            "CLOUDSDK_CONFIG",
                        ],
                    ),
                    ("CLAUDE_CODE_USE_FOUNDRY", &["AZURE_*"]),
                ],
            ),
        ),
        ("codex", (&["CODEX_*", "OPENAI_*"], &[])),
        ("opencode", (&["OPENCODE_*"], &[])),
        ("zcode", (&["ZCODE_*"], &[])),
        ("hermes", (&["HERMES_*"], &[])),
        ("grok", (&["GROK_*", "XAI_API_KEY"], &[])),
        ("cursor", (&["CURSOR_*"], &[])),
        ("qoder", (&["QODER_*", "QODERCLI_*"], &[])),
        (
            "agy",
            (
                &[
                    "AGY_*",
                    "ANTIGRAVITY_*",
                    "GEMINI_API_KEY",
                    "GOOGLE_API_KEY",
                    "GOOGLE_CLOUD_PROJECT",
                    "GOOGLE_APPLICATION_CREDENTIALS",
                    "CLOUDSDK_CONFIG",
                ],
                &[],
            ),
        ),
        ("pi", (&[], &[])),
        ("custom", (&[], &[])),
    ];
    for (id, (always, when)) in want {
        let set = apb_core::agents::lookup(id)
            .map_or(apb_core::agents::AgentEnvSet::NONE, |a| a.setup().env_set());
        assert_eq!(set.always, *always, "{id}");
        assert_eq!(set.when, *when, "{id}");
    }
}

/// The agents whose sandboxed run is verified declare their needs; every
/// other agent declares none, so `isolation: full` refuses it.
#[test]
fn sandbox_needs_are_declared_for_claude_and_codex_only() {
    use apb_core::agents::Access;
    for a in apb_core::agents::all() {
        let id = a.descriptor().id;
        let needs = a.setup().sandbox_needs();
        match id {
            "claude" => {
                let n = needs.expect("claude declares its needs");
                assert_eq!(n.binds.len(), 1);
                assert_eq!(n.binds[0].home_rel, ".claude/.credentials.json");
                assert_eq!(n.binds[0].access, Access::ReadWrite);
                assert_eq!(n.copies.len(), 1);
                assert_eq!(n.copies[0].home_rel, ".claude.json");
                assert!(n.env_logins.contains(&"ANTHROPIC_API_KEY"));
                assert!(n.env_logins.contains(&"CLAUDE_CODE_OAUTH_TOKEN"));
            }
            "codex" => {
                let n = needs.expect("codex declares its needs");
                assert!(n.binds.is_empty() && n.copies.is_empty());
                assert_eq!(n.env_logins, ["OPENAI_API_KEY", "CODEX_API_KEY"]);
            }
            _ => assert_eq!(needs, None, "{id}"),
        }
    }
}

/// Registry invariants: ids are unique, an alias never names another agent,
/// every module with forms has a valid launch form whose resume form takes
/// the session, and a verified version carries a `YYYY-MM-DD` date.
#[test]
fn the_registry_is_consistent() {
    let all = apb_core::agents::all();
    let ids: Vec<&str> = all.iter().map(|a| a.descriptor().id).collect();
    for (i, id) in ids.iter().enumerate() {
        assert!(!ids[i + 1..].contains(id), "{id} is registered twice");
        assert_eq!(
            apb_core::agents::lookup(id).map(|a| a.descriptor().id),
            Some(*id)
        );
    }
    for a in all {
        let d = a.descriptor();
        assert!(!d.bins.is_empty(), "{}", d.id);
        for alias in d.aliases {
            assert!(!ids.contains(alias), "{alias} is both an alias and an id");
            assert_eq!(apb_core::agents::canonical_id(alias), d.id);
        }
        if let Some(v) = d.verified {
            let date = v.on.as_bytes();
            assert!(
                date.len() == 10 && date[4] == b'-' && date[7] == b'-',
                "{}: {}",
                d.id,
                v.on
            );
        }
        if let Some(forms) = a.forms() {
            forms
                .launch
                .validate()
                .unwrap_or_else(|e| panic!("{}: {e}", d.id));
            if let Some(resume) = forms.resume {
                assert!(resume.contains(&"{session}"), "{}", d.id);
                assert!(resume.contains(&"{prompt}"), "{}", d.id);
            }
        }
    }
}

/// Files and literals allowed outside the agent modules, each with the reason
/// it is not knowledge about a built-in agent.
const LITERALS_ALLOWED: &[(&str, &str, &str)] = &[
    (
        "apb-core/src/config.rs",
        "\"codex\"",
        "HISTORY_AGENTS names the session stores history discovery reads, not an executor",
    ),
    (
        "apb-core/src/discover/types.rs",
        "\"codex\"",
        "a history agent's id, the store history discovery reads",
    ),
    (
        "apb-core/src/discover/types.rs",
        "\"claude-code\"",
        "a history agent's config spelling, the store history discovery reads",
    ),
    (
        "apb-engine/src/attempt_models.rs",
        "\"claude\"",
        "a vendor word inside model ids, not the agent",
    ),
    (
        "apb-engine/src/connector/inbox.rs",
        "\"cursor\"",
        "a connector inbox's pagination cursor, not the agent",
    ),
    (
        "apb-engine/src/connector/contract_test.rs",
        "\"cursor\"",
        "a connector contract's pagination cursor, not the agent",
    ),
    (
        "apb-server/src/routes/connectors/inbox.rs",
        "\"cursor\"",
        "a connector inbox's pagination cursor, not the agent",
    ),
];

/// Knowledge about a built-in agent lives only in its module: no non-test
/// source outside `apb-core/src/agents/` names a built-in agent id or alias
/// as a string literal, or `zcode::AGENT_ID`, except the allowed entries
/// above (each still in use). Test code is skipped: an item under
/// `#[cfg(test)]`, and the split-out `tests.rs` modules.
#[test]
fn agent_knowledge_stays_in_the_agent_modules() {
    let crates = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
    let mut needles: Vec<String> = vec!["zcode::AGENT_ID".to_string()];
    for a in apb_core::agents::all() {
        let d = a.descriptor();
        needles.push(format!("\"{}\"", d.id));
        needles.extend(d.aliases.iter().map(|alias| format!("\"{alias}\"")));
    }
    let mut files = Vec::new();
    for krate in std::fs::read_dir(&crates).unwrap() {
        collect_rs(&krate.unwrap().path().join("src"), &mut files);
    }
    let mut found = Vec::new();
    let mut used = vec![false; LITERALS_ALLOWED.len()];
    for file in files {
        let rel = file
            .strip_prefix(&crates)
            .unwrap()
            .to_string_lossy()
            .replace('\\', "/");
        if rel.starts_with("apb-core/src/agents/") || rel.ends_with("/tests.rs") {
            continue;
        }
        let text = std::fs::read_to_string(&file).unwrap();
        for (n, line) in non_test_lines(&text) {
            if line.trim_start().starts_with("//") {
                continue;
            }
            for needle in needles.iter().filter(|nd| line.contains(nd.as_str())) {
                match LITERALS_ALLOWED
                    .iter()
                    .position(|(f, lit, _)| *f == rel && lit == needle)
                {
                    Some(at) => used[at] = true,
                    None => found.push(format!("{rel}:{n}: {needle}")),
                }
            }
        }
    }
    assert!(
        found.is_empty(),
        "agent knowledge outside apb-core/src/agents (ask the agent registry instead):\n{}",
        found.join("\n")
    );
    for ((file, literal, _), used) in LITERALS_ALLOWED.iter().zip(used) {
        assert!(used, "stale allowlist entry {file} {literal}");
    }
}

fn collect_rs(dir: &std::path::Path, out: &mut Vec<std::path::PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries {
        let path = entry.unwrap().path();
        if path.is_dir() {
            collect_rs(&path, out);
        } else if path.extension().is_some_and(|e| e == "rs") {
            out.push(path);
        }
    }
}

/// The lines of `text` outside items marked `#[cfg(test)]`, numbered from 1.
/// An item ends where its braces balance, or at its `;` when it has none.
/// Braces inside string, raw string and char literals and inside comments
/// do not count.
fn non_test_lines(text: &str) -> Vec<(usize, &str)> {
    let lines: Vec<&str> = text.lines().collect();
    let mut out = Vec::new();
    let mut i = 0;
    while i < lines.len() {
        if lines[i].trim() == "#[cfg(test)]" {
            let mut scan = Scan::default();
            let mut depth = 0i64;
            let mut opened = false;
            i += 1;
            while i < lines.len() {
                let l = lines[i];
                let (delta, any) = scan.braces(l);
                depth += delta;
                opened |= any;
                i += 1;
                if (opened && depth <= 0) || (!opened && l.trim_end().ends_with(';')) {
                    break;
                }
            }
            continue;
        }
        out.push((i + 1, lines[i]));
        i += 1;
    }
    out
}

/// Where a scan over Rust source stands at the end of a line: inside a
/// string, a raw string with this many `#`, or a block comment.
#[derive(Default)]
struct Scan {
    string: bool,
    raw: Option<usize>,
    block_comment: usize,
}

impl Scan {
    /// The brace balance of `line` in code, and whether it opened a brace.
    fn braces(&mut self, line: &str) -> (i64, bool) {
        let c: Vec<char> = line.chars().collect();
        let (mut delta, mut opened) = (0i64, false);
        let mut i = 0;
        while i < c.len() {
            if let Some(hashes) = self.raw {
                if c[i] == '"' && c[i + 1..].iter().take_while(|h| **h == '#').count() >= hashes {
                    self.raw = None;
                    i += hashes;
                }
            } else if self.string {
                match c[i] {
                    '\\' => i += 1,
                    '"' => self.string = false,
                    _ => {}
                }
            } else if self.block_comment > 0 {
                if c[i] == '*' && c.get(i + 1) == Some(&'/') {
                    self.block_comment -= 1;
                    i += 1;
                } else if c[i] == '/' && c.get(i + 1) == Some(&'*') {
                    self.block_comment += 1;
                    i += 1;
                }
            } else {
                match c[i] {
                    '/' if c.get(i + 1) == Some(&'/') => break,
                    '/' if c.get(i + 1) == Some(&'*') => {
                        self.block_comment = 1;
                        i += 1;
                    }
                    '"' => self.string = true,
                    'r' if matches!(c.get(i + 1), Some('"') | Some('#'))
                        && raw_prefix_starts(&c[..i]) =>
                    {
                        let hashes = c[i + 1..].iter().take_while(|h| **h == '#').count();
                        if c.get(i + 1 + hashes) == Some(&'"') {
                            self.raw = Some(hashes);
                            i += 1 + hashes;
                        }
                    }
                    // A char literal: `'{'` or an escape like `'\''`; a
                    // lifetime has no closing quote right after.
                    '\'' if c.get(i + 1) == Some(&'\\') => {
                        // Past the quote, the backslash and the escaped
                        // character, which may itself be a quote.
                        i += 3;
                        while i < c.len() && c[i] != '\'' {
                            i += 1;
                        }
                    }
                    '\'' if c.get(i + 2) == Some(&'\'') => i += 2,
                    '{' => {
                        delta += 1;
                        opened = true;
                    }
                    '}' => delta -= 1,
                    _ => {}
                }
            }
            i += 1;
        }
        (delta, opened)
    }
}

/// Whether an `r` after `before` starts a raw string: it is not part of an
/// identifier, or it follows a lone `b` or `c` prefix (`br"..."`, `cr"..."`).
fn raw_prefix_starts(before: &[char]) -> bool {
    let ident = |p: &char| p.is_alphanumeric() || *p == '_';
    match before.split_last() {
        None => true,
        Some((p, rest)) if *p == 'b' || *p == 'c' => !rest.last().is_some_and(ident),
        Some((p, _)) => !ident(p),
    }
}

/// Braces in literals and comments of a test item do not end it early or
/// keep it open.
#[test]
fn the_test_item_scan_skips_braces_in_literals_and_comments() {
    let text = "#[cfg(test)]\nmod t {\n    let a = \"}\";\n    let b = '}';\n    let c = r#\"}\"#; // }\n    /* } */\n}\nfn after() {}\n";
    let kept: Vec<&str> = non_test_lines(text).into_iter().map(|(_, l)| l).collect();
    assert_eq!(kept, ["fn after() {}"]);
    let escapes = "#[cfg(test)]\nmod t {\n    let q = ('\\'','}');\n    let r = br#\"x\"}\"#;\n    let s = cr\"}\";\n}\nfn after() {}\n";
    let kept: Vec<&str> = non_test_lines(escapes)
        .into_iter()
        .map(|(_, l)| l)
        .collect();
    assert_eq!(kept, ["fn after() {}"]);
    let lifetime = "#[cfg(test)]\nfn f<'a>(x: &'a str) -> &'a str {\n    x\n}\nfn after() {}\n";
    let kept: Vec<&str> = non_test_lines(lifetime)
        .into_iter()
        .map(|(_, l)| l)
        .collect();
    assert_eq!(kept, ["fn after() {}"]);
}
