//! Lint over the repository's `bundled/` folder: the default playbooks and
//! skills apb ships must work on any machine. They name no model or provider,
//! hold no personal path, keep to the writing conventions, use the reserved
//! `apb-` prefix and bind only the setup role profiles.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use apb_core::schema::Playbook;

/// Model and provider names, matched as word stems, case-insensitively: a
/// word is denied when it is a stem followed by nothing but digits (`qwen`,
/// `qwen3`, `gpt5`). The agent registry and the models table add their own
/// names to this list (see [`deny_stems`]).
const DENY_WORDS: &[&str] = &[
    "claude",
    "codex",
    "gpt",
    "chatgpt",
    "opus",
    "sonnet",
    "haiku",
    "glm",
    "gemini",
    "grok",
    "qwen",
    "deepseek",
    "openai",
    "anthropic",
    "zhipu",
    "openrouter",
    "mistral",
    "llama",
    "kimi",
    "copilot",
    "opencode",
    "zcode",
    "qoder",
    "qodercli",
    "minimax",
    "moonshot",
    "typesafe",
    "jev",
];

/// Names with punctuation inside, matched as tokens bounded by anything that
/// is not a letter, a digit or a dot.
const DENY_PHRASES: &[&str] = &["z.ai"];

/// Ordinary English words that a registry or models-table name happens to
/// be; they are never derived into the deny list.
const ORDINARY_WORDS: &[&str] = &["pi", "cursor", "whisper", "agent", "code", "mini"];

const DENY_PATHS: &[&str] = &["/home/", "/Users/", "~/"];

const ALLOWED_PROFILES: &[&str] = &["apb-main", "apb-reviewer", "apb-researcher"];

fn bundled_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../bundled")
}

fn files_under(dir: &Path, out: &mut Vec<PathBuf>) {
    for entry in std::fs::read_dir(dir).unwrap_or_else(|e| panic!("read {}: {e}", dir.display())) {
        let path = entry.unwrap().path();
        if path.is_dir() {
            files_under(&path, out);
        } else {
            out.push(path);
        }
    }
}

fn subdirs(dir: &Path) -> Vec<PathBuf> {
    let mut dirs: Vec<PathBuf> = std::fs::read_dir(dir)
        .unwrap_or_else(|e| panic!("read {}: {e}", dir.display()))
        .map(|e| e.unwrap().path())
        .filter(|p| p.is_dir())
        .collect();
    dirs.sort();
    dirs
}

/// The lowercase alphanumeric runs of `name` that can name a product: at
/// least three characters, starting with a letter, with trailing digits cut
/// off, and not an ordinary word.
fn stems_of(name: &str, out: &mut BTreeSet<String>) {
    for run in name
        .to_lowercase()
        .split(|c: char| !c.is_ascii_alphanumeric())
    {
        let stem = run.trim_end_matches(|c: char| c.is_ascii_digit());
        if stem.len() >= 3
            && stem.starts_with(|c: char| c.is_ascii_alphabetic())
            && !ORDINARY_WORDS.contains(&stem)
        {
            out.insert(stem.to_string());
        }
    }
}

/// The deny stems: the static list, every built-in agent's id, aliases and
/// binaries from the agent registry, and the vendors and model id stems of
/// the built-in models table. Only the leading run of a model id counts
/// (`qwen` of `qwen3.8-max`), so tier words such as `flash` or `pro` stay
/// allowed.
fn deny_stems() -> BTreeSet<String> {
    let mut stems: BTreeSet<String> = DENY_WORDS.iter().map(|w| w.to_string()).collect();
    for agent in apb_core::agents::all() {
        let d = agent.descriptor();
        for name in std::iter::once(&d.id).chain(d.aliases).chain(d.bins) {
            stems_of(name, &mut stems);
        }
    }
    let table = apb_core::models_table::builtin();
    for row in &table.models {
        stems_of(&row.vendor, &mut stems);
        if let Some(lead) = row.id.split(|c: char| !c.is_ascii_alphanumeric()).next() {
            stems_of(lead, &mut stems);
        }
    }
    stems
}

/// Word-stem hits of the deny list in `text`, each as `line:word`.
fn denied_words(text: &str) -> Vec<String> {
    let stems = deny_stems();
    let pattern = stems
        .iter()
        .map(|s| regex::escape(s))
        .collect::<Vec<_>>()
        .join("|");
    let word = regex::Regex::new(&format!("^(?:{pattern})[0-9]*$")).unwrap();
    let mut hits = Vec::new();
    for (n, line) in text.lines().enumerate() {
        let lower = line.to_lowercase();
        for token in lower.split(|c: char| !c.is_ascii_alphanumeric()) {
            if word.is_match(token) {
                hits.push(format!("{}:{token}", n + 1));
            }
        }
        for phrase in DENY_PHRASES {
            for (at, _) in lower.match_indices(phrase) {
                let before = lower[..at].chars().next_back();
                let after = lower[at + phrase.len()..].chars().next();
                let bounded =
                    |c: Option<char>| c.is_none_or(|c| !(c.is_ascii_alphanumeric() || c == '.'));
                if bounded(before) && bounded(after) {
                    hits.push(format!("{}:{phrase}", n + 1));
                }
            }
        }
    }
    hits
}

/// Exclamation marks in markdown prose: outside fenced blocks and inline
/// code, and not an image link (`![`).
fn prose_exclamations(text: &str) -> Vec<usize> {
    let mut hits = Vec::new();
    let mut fenced = false;
    for (n, line) in text.lines().enumerate() {
        if line.trim_start().starts_with("```") {
            fenced = !fenced;
            continue;
        }
        if fenced {
            continue;
        }
        let mut in_code = false;
        let chars: Vec<char> = line.chars().collect();
        for (i, c) in chars.iter().enumerate() {
            match c {
                '`' => in_code = !in_code,
                '!' if !in_code && chars.get(i + 1) != Some(&'[') => {
                    hits.push(n + 1);
                    break;
                }
                _ => {}
            }
        }
    }
    hits
}

fn frontmatter(skill_md: &str) -> Option<yaml_serde::Value> {
    let rest = skill_md.strip_prefix("---\n")?;
    let end = rest.find("\n---")?;
    yaml_serde::from_str(&rest[..end]).ok()
}

/// Every profile name a playbook document references, wherever it sits
/// (`defaults.profile`, a node's `profile`, a judge's or finish's profile),
/// in the string form or the `{ name, scope }` form.
fn profile_refs(value: &yaml_serde::Value, out: &mut Vec<String>) {
    match value {
        yaml_serde::Value::Mapping(map) => {
            for (k, v) in map {
                if k.as_str() == Some("profile") {
                    match v {
                        yaml_serde::Value::String(s) => out.push(s.clone()),
                        yaml_serde::Value::Mapping(m) => {
                            if let Some(name) = m.get("name").and_then(|n| n.as_str()) {
                                out.push(name.to_string());
                            }
                        }
                        _ => {}
                    }
                }
                profile_refs(v, out);
            }
        }
        yaml_serde::Value::Sequence(seq) => seq.iter().for_each(|v| profile_refs(v, out)),
        _ => {}
    }
}

#[test]
fn bundled_content_has_no_model_provider_or_personal_content() {
    let root = bundled_root();
    let mut files = Vec::new();
    files_under(&root, &mut files);
    assert!(!files.is_empty(), "no files under {}", root.display());
    let mut problems = Vec::new();
    for file in &files {
        let rel = file.strip_prefix(&root).unwrap().display().to_string();
        let text = std::fs::read_to_string(file).unwrap_or_else(|e| panic!("read {rel}: {e}"));
        for hit in denied_words(&text) {
            problems.push(format!("{rel}:{hit}: model or provider name"));
        }
        for path in DENY_PATHS {
            if text.contains(path) {
                problems.push(format!("{rel}: personal path `{path}`"));
            }
        }
        if text.contains('\u{2014}') || text.contains('\u{2013}') {
            problems.push(format!("{rel}: em or en dash"));
        }
        if file.extension().is_some_and(|e| e == "md") {
            for line in prose_exclamations(&text) {
                problems.push(format!("{rel}:{line}: exclamation mark in prose"));
            }
        }
    }
    assert!(
        problems.is_empty(),
        "bundled content lint:\n{}",
        problems.join("\n")
    );
}

#[test]
fn bundled_skills_use_the_reserved_prefix() {
    let skills = subdirs(&bundled_root().join("skills"));
    assert!(!skills.is_empty());
    for dir in skills {
        let folder = dir.file_name().unwrap().to_str().unwrap().to_string();
        assert!(
            folder.starts_with("apb-"),
            "skill folder `{folder}` lacks the apb- prefix"
        );
        let md = std::fs::read_to_string(dir.join("SKILL.md"))
            .unwrap_or_else(|e| panic!("{folder}/SKILL.md: {e}"));
        let doc = frontmatter(&md).unwrap_or_else(|| panic!("{folder}: no frontmatter"));
        let name = doc.get("name").and_then(|n| n.as_str());
        assert_eq!(
            name,
            Some(folder.as_str()),
            "skill name must match its folder"
        );
        let version = doc
            .get("metadata")
            .and_then(|m| m.get("version"))
            .and_then(|v| v.as_str())
            .unwrap_or_else(|| panic!("{folder}: metadata.version missing"));
        assert!(
            apb_core::registry::parse_version(version).is_some(),
            "{folder}: metadata.version `{version}` is not a semver version"
        );
    }
}

#[test]
fn bundled_playbooks_use_the_prefix_and_role_profiles_only() {
    let playbooks = subdirs(&bundled_root().join("playbooks"));
    assert!(!playbooks.is_empty());
    for dir in playbooks {
        let folder = dir.file_name().unwrap().to_str().unwrap().to_string();
        let yaml = std::fs::read_to_string(dir.join("playbook.yaml"))
            .unwrap_or_else(|e| panic!("{folder}/playbook.yaml: {e}"));
        let pb = Playbook::from_yaml(&yaml).unwrap_or_else(|e| panic!("{folder}: {e}"));
        assert!(
            pb.id.starts_with("apb-"),
            "playbook id `{}` lacks the apb- prefix",
            pb.id
        );
        assert_eq!(pb.id, folder, "playbook id must match its folder");
        assert_eq!(pb.schema, 2, "{folder}: schema 2 expected");

        let doc: yaml_serde::Value = yaml_serde::from_str(&yaml).unwrap();
        assert_eq!(
            doc.get("agent_launch").and_then(|v| v.as_str()),
            Some("ask"),
            "{folder}: agent_launch must be ask"
        );
        let mut refs = Vec::new();
        profile_refs(&doc, &mut refs);
        assert!(!refs.is_empty(), "{folder}: names no profile");
        for name in refs {
            assert!(
                ALLOWED_PROFILES.contains(&name.as_str()),
                "{folder}: profile `{name}` is not one of {ALLOWED_PROFILES:?}"
            );
        }
        let nodes = doc.get("nodes").and_then(|n| n.as_sequence()).unwrap();
        for node in nodes {
            let kind = node.get("type").and_then(|t| t.as_str()).unwrap_or("");
            assert_ne!(
                kind, "publish",
                "{folder}: a bundled playbook has no publish node"
            );
            assert_ne!(
                kind, "script",
                "{folder}: a bundled playbook runs no script"
            );
        }
    }
}

/// Issue text is untrusted, so in the triage playbook the agent only names a
/// category: each category has its own label step that takes only that
/// category's items, adds the label from its own param and is checked by
/// the engine against that same param. No agent output names a label.
#[test]
fn triage_labels_come_from_params_never_from_agent_output() {
    let yaml =
        std::fs::read_to_string(bundled_root().join("playbooks/apb-issue-triage/playbook.yaml"))
            .unwrap();
    let doc: yaml_serde::Value = yaml_serde::from_str(&yaml).unwrap();
    let nodes = doc.get("nodes").and_then(|n| n.as_sequence()).unwrap();
    let node = |id: &str| {
        nodes
            .iter()
            .find(|n| n.get("id").and_then(|v| v.as_str()) == Some(id))
            .unwrap_or_else(|| panic!("no node `{id}`"))
    };
    let outputs = |n: &yaml_serde::Value| -> Vec<String> {
        n.get("list")
            .and_then(|l| l.get("item_outputs"))
            .and_then(|o| o.as_sequence())
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap().to_string())
            .collect()
    };
    assert!(
        !outputs(node("classify"))
            .iter()
            .any(|o| o.contains("label")),
        "classify must not output a label"
    );
    // The step that reads the issue to classify it has no connector and is
    // isolated where the machine can do it.
    let classify = node("classify");
    assert!(
        classify.get("connectors").is_none(),
        "classify has no connector"
    );
    let level = classify
        .get("isolation")
        .and_then(|i| i.get("level"))
        .and_then(|v| v.as_str());
    assert!(
        level.is_some_and(|l| l != "none"),
        "classify is isolated: {level:?}"
    );
    // Every issue the listing can return fits the classify list: one
    // list_issues call per page, GitHub's default 30 issues a page.
    let pages = node("list_open")
        .get("connectors")
        .and_then(|c| c.as_sequence())
        .and_then(|c| c.first())
        .and_then(|b| b.get("max_calls"))
        .and_then(|v| v.as_u64())
        .expect("list_open caps its calls");
    let max_items = classify
        .get("list")
        .and_then(|l| l.get("items"))
        .and_then(|i| i.get("max_items"))
        .and_then(|v| v.as_u64())
        .expect("classify caps its items");
    assert!(
        max_items >= pages * 30,
        "classify holds {max_items} items, the listing returns up to {}",
        pages * 30
    );
    for cat in ["bug", "feature", "question", "duplicate", "needs_info"] {
        let step = node(&format!("label_{cat}"));
        let param = format!("{{{{params.label_{cat}}}}}");
        let list = step.get("list").unwrap();
        let wh = list.get("items").and_then(|i| i.get("where")).unwrap();
        assert_eq!(wh.get("field").and_then(|v| v.as_str()), Some("category"));
        assert_eq!(wh.get("equals").and_then(|v| v.as_str()), Some(cat));
        assert_eq!(wh.get("state").and_then(|v| v.as_str()), Some("passed"));
        let prompt = step.get("prompt").and_then(|v| v.as_str()).unwrap();
        assert!(prompt.contains(&param), "label_{cat}: prompt lacks {param}");
        let check = list.get("check").and_then(|c| c.get("connector")).unwrap();
        let contains = check
            .get("accept_when")
            .and_then(|a| a.get("contains"))
            .and_then(|v| v.as_str());
        assert_eq!(contains, Some(param.as_str()), "label_{cat}: check");
        assert_eq!(
            check
                .get("args")
                .and_then(|a| a.get("number"))
                .and_then(|v| v.as_str()),
            Some("{{item.number}}")
        );
        // One write, at most two calls, and no resend policy: add_labels is
        // repeatable, so `on_unknown` would act on nothing (V69).
        let bindings = step
            .get("connectors")
            .and_then(|c| c.as_sequence())
            .unwrap();
        assert_eq!(bindings.len(), 1, "label_{cat}");
        let b = &bindings[0];
        let functions: Vec<&str> = b
            .get("functions")
            .and_then(|f| f.as_sequence())
            .unwrap()
            .iter()
            .filter_map(|f| f.as_str())
            .collect();
        assert_eq!(functions, ["add_labels"], "label_{cat}");
        assert!(
            b.get("max_calls")
                .and_then(|v| v.as_u64())
                .is_some_and(|n| n <= 2),
            "label_{cat}"
        );
        assert!(b.get("on_unknown").is_none(), "label_{cat}");
    }
    assert!(!yaml.contains("item.output.label") && !yaml.contains("item_output.label"));
    // The description states the whole reach of an injected instruction.
    let description = doc.get("description").and_then(|v| v.as_str()).unwrap();
    for reach in [
        "shell",
        "network",
        "another issue or pull request",
        "without a sandbox",
    ] {
        assert!(description.contains(reach), "description lacks `{reach}`");
    }
}

#[test]
fn deny_list_matches_whole_words_only() {
    assert_eq!(denied_words("Uses GPT-4 here"), vec!["1:gpt"]);
    // A stem followed by digits is the same name.
    assert_eq!(
        denied_words("qwen3, gpt5, llama3 and sonnet4"),
        vec!["1:qwen3", "1:gpt5", "1:llama3", "1:sonnet4"]
    );
    // Names the registry and the models table supply.
    assert_eq!(denied_words("hermes and agy"), vec!["1:hermes", "1:agy"]);
    assert_eq!(
        denied_words("from Google or xAI"),
        vec!["1:google", "1:xai"]
    );
    // Ordinary words, tier words and a stem inside a longer word pass.
    assert!(
        denied_words("move the cursor, whisper, pi, a flash pro mini agent, gptx, grokking")
            .is_empty()
    );
    assert_eq!(denied_words("ask z.ai now"), vec!["1:z.ai"]);
    assert!(denied_words("an opusculum, a glimmer, magnum opuses").is_empty());
    assert!(denied_words("gptx and zz.ai and z.air").is_empty());
    assert_eq!(
        prose_exclamations("ok `a != b` and ![img](x)"),
        Vec::<usize>::new()
    );
    assert_eq!(prose_exclamations("```\nrun!\n```\nDone!"), vec![4]);
}

/// The listing step cuts each issue body to a number of characters, and the
/// engine settles an item larger than `ITEM_MAX_BYTES` (its JSON, in bytes)
/// as needing a look before it runs. The cut must hold in bytes for any
/// text: a three-byte character, a four-byte one, and the escapes JSON
/// writes for a quote, a backslash or a control character (six bytes for
/// `\u0001`), with the longest title and url GitHub allows.
#[test]
fn triage_body_cut_keeps_every_item_under_the_item_byte_limit() {
    use apb_core::list::ITEM_MAX_BYTES;
    use apb_core::list::items::{Item, item_bytes};

    let yaml =
        std::fs::read_to_string(bundled_root().join("playbooks/apb-issue-triage/playbook.yaml"))
            .unwrap();
    let doc: yaml_serde::Value = yaml_serde::from_str(&yaml).unwrap();
    let prompt = doc
        .get("nodes")
        .and_then(|n| n.as_sequence())
        .unwrap()
        .iter()
        .find(|n| n.get("id").and_then(|v| v.as_str()) == Some("list_open"))
        .and_then(|n| n.get("prompt"))
        .and_then(|p| p.as_str())
        .unwrap()
        .to_string();
    let words: Vec<&str> = prompt.split_whitespace().collect();
    let at = words
        .windows(4)
        .position(|w| w[0] == "its" && w[1] == "first" && w[3] == "characters,")
        .expect("the prompt names the body cut as `its first N characters`");
    let cut: usize = words[at + 2].parse().unwrap();

    let item = |title: String, body: String| Item {
        id: "1".into(),
        value: serde_json::json!({
            "number": u64::MAX,
            "title": title,
            "url": format!(
                "https://github.com/{}/{}/issues/{}",
                "o".repeat(39),
                "r".repeat(100),
                u64::MAX
            ),
            "body": body,
        }),
        digest: String::new(),
    };
    // A GitHub title holds at most 256 characters; the worst one is all
    // control characters.
    let title = "\u{1}".repeat(256);
    let long = |c: char| std::iter::repeat_n(c, 3000).collect::<String>();
    let cut_to = |s: &str| s.chars().take(cut).collect::<String>();

    // The bug this guards: 3000 three-byte characters overflow the item.
    assert!(
        item(String::new(), long('\u{4e2d}')).value["body"]
            .as_str()
            .unwrap()
            .len()
            > ITEM_MAX_BYTES
    );
    for c in ['\u{4e2d}', '\u{1f600}', '"', '\\', '\n', '\u{1}'] {
        let body = cut_to(&long(c));
        let bytes = item_bytes(&item(title.clone(), body));
        assert!(
            bytes <= ITEM_MAX_BYTES,
            "a body of {cut} x {c:?} gives {bytes} bytes, over {ITEM_MAX_BYTES}"
        );
    }
}

/// The tests and security review roles always run their audit skill: the
/// skills ship, the review skill and both review playbooks name them for
/// those roles, and the reviewer profile carries them.
#[test]
fn review_roles_always_run_their_audit_skills() {
    let root = bundled_root();
    for skill in [
        "apb-test-audit",
        "apb-security-audit",
        "apb-model-selection",
    ] {
        assert!(
            root.join("skills").join(skill).join("SKILL.md").is_file(),
            "{skill} is not shipped"
        );
    }
    let review = std::fs::read_to_string(root.join("skills/apb-review/SKILL.md")).unwrap();
    for skill in ["apb-test-audit", "apb-security-audit"] {
        assert!(review.contains(skill), "apb-review does not name {skill}");
        assert!(
            apb_core::bundled::role_skills("apb-reviewer").contains(&skill),
            "apb-reviewer does not carry {skill}"
        );
    }
    for playbook in ["apb-code-review", "apb-feature-delivery"] {
        let yaml =
            std::fs::read_to_string(root.join("playbooks").join(playbook).join("playbook.yaml"))
                .unwrap();
        let doc: yaml_serde::Value = yaml_serde::from_str(&yaml).unwrap();
        let prompt = |id: &str| -> String {
            doc.get("nodes")
                .and_then(|n| n.as_sequence())
                .and_then(|nodes| {
                    nodes
                        .iter()
                        .find(|n| n.get("id").and_then(|v| v.as_str()) == Some(id))
                })
                .and_then(|n| n.get("prompt"))
                .and_then(|p| p.as_str())
                .unwrap_or_else(|| panic!("{playbook}: no prompt on {id}"))
                .to_string()
        };
        assert!(
            prompt("review_tests").contains("apb-test-audit"),
            "{playbook}: the tests reviewer does not run apb-test-audit"
        );
        assert!(
            prompt("review_security").contains("apb-security-audit"),
            "{playbook}: the security reviewer does not run apb-security-audit"
        );
    }
}

/// A bug fix goes through the diagnosis skill: it ships, the delivery skill
/// and the delivery playbook's design and implement stages name it, and the
/// main profile, which runs those stages, carries it.
#[test]
fn bug_fixes_go_through_the_diagnosis_skill() {
    let root = bundled_root();
    assert!(
        root.join("skills/apb-diagnosis/SKILL.md").is_file(),
        "apb-diagnosis is not shipped"
    );
    let delivery = std::fs::read_to_string(root.join("skills/apb-delivery/SKILL.md")).unwrap();
    assert!(
        delivery.contains("apb-diagnosis"),
        "apb-delivery does not name apb-diagnosis"
    );
    assert!(
        apb_core::bundled::role_skills("apb-main").contains(&"apb-diagnosis"),
        "apb-main does not carry apb-diagnosis"
    );
    let yaml =
        std::fs::read_to_string(root.join("playbooks/apb-feature-delivery/playbook.yaml")).unwrap();
    let doc: yaml_serde::Value = yaml_serde::from_str(&yaml).unwrap();
    for stage in ["design", "implement", "implement_subtasks"] {
        let prompt = doc
            .get("nodes")
            .and_then(|n| n.as_sequence())
            .and_then(|nodes| {
                nodes
                    .iter()
                    .find(|n| n.get("id").and_then(|v| v.as_str()) == Some(stage))
            })
            .and_then(|n| n.get("prompt"))
            .and_then(|p| p.as_str())
            .unwrap_or_else(|| panic!("apb-feature-delivery: no prompt on {stage}"));
        assert!(
            prompt.contains("apb-diagnosis"),
            "apb-feature-delivery: the {stage} stage does not name apb-diagnosis"
        );
    }
}

/// Every shipped playbook carries its scenarios in the embedded `evals/`,
/// and every scenario file parses and holds to the scenario rules against
/// its playbook (V146 to V150 clean), so setup installs a suite that plays.
#[test]
fn every_shipped_playbook_ships_valid_scenarios() {
    let shipped = apb_core::bundled::playbooks();
    assert!(!shipped.is_empty());
    for pb in shipped {
        let playbook = Playbook::from_yaml(&pb.yaml).unwrap();
        let scenarios: Vec<(&String, &Vec<u8>)> = pb
            .evals
            .iter()
            .filter(|(path, _)| path.starts_with("scenarios/") && path.ends_with(".yaml"))
            .collect();
        assert!(
            scenarios.len() >= 2,
            "{}: ships {} scenario(s) in evals/, at least 2 are required",
            pb.id,
            scenarios.len()
        );
        for (path, bytes) in scenarios {
            let name = path
                .trim_start_matches("scenarios/")
                .trim_end_matches(".yaml");
            let raw = std::str::from_utf8(bytes).unwrap();
            let scenario = apb_core::scenario::parse(raw, name)
                .unwrap_or_else(|e| panic!("{}/{name}: {e}", pb.id));
            let errors: Vec<String> =
                apb_core::scenario::validate_scenario(name, &scenario, &playbook)
                    .into_iter()
                    .filter(|i| i.severity == apb_core::validate::Severity::Error)
                    .map(|i| format!("{} {}", i.code, i.message))
                    .collect();
            assert!(errors.is_empty(), "{}/{name}: {errors:#?}", pb.id);
        }
    }
}

/// The connector function names a playbook binds: node grants and item
/// checks, per connector.
fn bound_functions(doc: &yaml_serde::Value) -> Vec<(String, String)> {
    let mut out = Vec::new();
    for node in doc.get("nodes").and_then(|n| n.as_sequence()).unwrap() {
        for c in node
            .get("connectors")
            .and_then(|c| c.as_sequence())
            .into_iter()
            .flatten()
        {
            let name = c.get("name").and_then(|v| v.as_str()).unwrap().to_string();
            for f in c
                .get("functions")
                .and_then(|f| f.as_sequence())
                .into_iter()
                .flatten()
            {
                out.push((name.clone(), f.as_str().unwrap().to_string()));
            }
        }
        if let Some(check) = node
            .get("list")
            .and_then(|l| l.get("check"))
            .and_then(|c| c.get("connector"))
        {
            out.push((
                check
                    .get("name")
                    .and_then(|v| v.as_str())
                    .unwrap()
                    .to_string(),
                check
                    .get("function")
                    .and_then(|v| v.as_str())
                    .unwrap()
                    .to_string(),
            ));
        }
    }
    out
}

fn inbox_digest() -> yaml_serde::Value {
    let yaml =
        std::fs::read_to_string(bundled_root().join("playbooks/apb-inbox-digest/playbook.yaml"))
            .unwrap();
    yaml_serde::from_str(&yaml).unwrap()
}

/// The mail digest only reads: every imap function it binds is
/// `read_only: true` in the official connector, and its effects hold
/// neither `external` nor `irreversible`.
#[test]
fn inbox_digest_binds_only_read_only_mail_functions() {
    let doc = inbox_digest();
    let manifest: yaml_serde::Value = yaml_serde::from_str(
        &std::fs::read_to_string(bundled_root().join("../connectors/imap/connector.yaml")).unwrap(),
    )
    .unwrap();
    let read_only: BTreeSet<String> = manifest
        .get("functions")
        .and_then(|f| f.as_sequence())
        .unwrap()
        .iter()
        .filter(|f| f.get("read_only").and_then(|v| v.as_bool()) == Some(true))
        .map(|f| f.get("name").and_then(|v| v.as_str()).unwrap().to_string())
        .collect();
    let bound = bound_functions(&doc);
    assert!(!bound.is_empty(), "the digest binds the imap connector");
    for (connector, function) in bound {
        assert_eq!(
            connector, "imap",
            "the digest binds only the imap connector"
        );
        assert!(
            read_only.contains(&function),
            "imap `{function}` is bound but not read_only"
        );
    }
    let effects: Vec<&str> = doc
        .get("effects")
        .and_then(|e| e.as_sequence())
        .unwrap()
        .iter()
        .filter_map(|e| e.as_str())
        .collect();
    for denied in ["external", "irreversible"] {
        assert!(!effects.contains(&denied), "the digest declares `{denied}`");
    }
}

/// The step that reads message text binds no connector.
#[test]
fn inbox_digest_classify_binds_no_connector() {
    let doc = inbox_digest();
    let classify = doc
        .get("nodes")
        .and_then(|n| n.as_sequence())
        .unwrap()
        .iter()
        .find(|n| n.get("id").and_then(|v| v.as_str()) == Some("classify"))
        .expect("the digest has a classify step");
    assert!(classify.get("connectors").is_none());
    assert!(
        classify
            .get("list")
            .and_then(|l| l.get("check"))
            .and_then(|c| c.get("connector"))
            .is_none()
    );
}

/// The cuts the search prompt names (`<field> cut to its first N
/// characters`) keep one message item under the item byte limit, even when
/// every character is one JSON writes as six bytes, with the largest uid.
#[test]
fn inbox_digest_cuts_keep_every_item_under_the_item_byte_limit() {
    use apb_core::list::ITEM_MAX_BYTES;
    use apb_core::list::items::{Item, item_bytes};

    let doc = inbox_digest();
    let prompt = doc
        .get("nodes")
        .and_then(|n| n.as_sequence())
        .unwrap()
        .iter()
        .find(|n| n.get("id").and_then(|v| v.as_str()) == Some("search"))
        .and_then(|n| n.get("prompt"))
        .and_then(|p| p.as_str())
        .unwrap()
        .to_string();
    let words: Vec<&str> = prompt.split_whitespace().collect();
    let cut_of = |field: &str| -> usize {
        let at = words
            .windows(6)
            .position(|w| w[0] == field && w[1] == "cut" && w[3] == "its" && w[4] == "first")
            .unwrap_or_else(|| panic!("the prompt names the cut of `{field}`"));
        words[at + 5].parse().unwrap()
    };
    let (from, subject) = (cut_of("from"), cut_of("subject"));
    // The snippet is "the message text cut to its first N characters".
    let at = words
        .windows(5)
        .position(|w| w[0] == "text" && w[1] == "cut" && w[3] == "its" && w[4] == "first")
        .expect("the prompt names the snippet cut");
    let snippet: usize = words[at + 5].parse().unwrap();

    let long = |c: char, n: usize| std::iter::repeat_n(c, n).collect::<String>();
    for c in ['\u{4e2d}', '\u{1f600}', '"', '\\', '\n', '\u{1}'] {
        let item = Item {
            id: "1".into(),
            value: serde_json::json!({
                "uid": u64::MAX,
                "date": "Wed, 04 Nov 2026 10:00:00 +0000 (Coordinated Universal Time)",
                "from": long(c, from),
                "subject": long(c, subject),
                "snippet": long(c, snippet),
            }),
            digest: String::new(),
        };
        let bytes = item_bytes(&item);
        assert!(
            bytes <= ITEM_MAX_BYTES,
            "cuts {from}/{subject}/{snippet} of {c:?} give {bytes} bytes, over {ITEM_MAX_BYTES}"
        );
    }
}

/// The delivery report describes the candidate the panel reviewed: every
/// agent step on a route from `collect` to `report` that does not pass
/// through `fix` is read-only (`protect: ["**"]` or `read_only`), and so is
/// `report` itself. Nothing after the last review changes the tree, which
/// is why the playbook no longer calls itself "assessment only".
#[test]
fn the_delivery_report_follows_only_read_only_nodes() {
    let yaml = std::fs::read_to_string(
        bundled_root().join("playbooks/apb-feature-delivery/playbook.yaml"),
    )
    .unwrap();
    let doc: yaml_serde::Value = yaml_serde::from_str(&yaml).unwrap();
    let edges: Vec<(String, String)> = doc
        .get("edges")
        .and_then(|e| e.as_sequence())
        .unwrap()
        .iter()
        .map(|e| {
            (
                e.get("from").and_then(|v| v.as_str()).unwrap().to_string(),
                e.get("to").and_then(|v| v.as_str()).unwrap().to_string(),
            )
        })
        .collect();
    let reach = |from: &str, forward: bool| -> BTreeSet<String> {
        let mut seen = BTreeSet::from([from.to_string()]);
        let mut stack = vec![from.to_string()];
        while let Some(n) = stack.pop() {
            for (a, b) in &edges {
                let (src, dst) = if forward { (a, b) } else { (b, a) };
                if *src == n && dst != "fix" && seen.insert(dst.clone()) {
                    stack.push(dst.clone());
                }
            }
        }
        seen
    };
    let after_collect = reach("collect", true);
    let before_report = reach("report", false);
    let on_route: BTreeSet<&String> = after_collect.intersection(&before_report).collect();
    assert!(
        on_route.contains(&"report".to_string()),
        "collect reaches report"
    );
    let nodes = doc.get("nodes").and_then(|n| n.as_sequence()).unwrap();
    for id in on_route {
        let node = nodes
            .iter()
            .find(|n| n.get("id").and_then(|v| v.as_str()) == Some(id.as_str()))
            .unwrap();
        if node.get("type").and_then(|t| t.as_str()) != Some("agent_task") {
            continue;
        }
        let protected = node
            .get("protect")
            .and_then(|p| p.as_sequence())
            .is_some_and(|p| p.iter().any(|g| g.as_str() == Some("**")));
        let read_only = node.get("read_only").and_then(|r| r.as_bool()) == Some(true);
        assert!(
            protected || read_only,
            "`{id}` runs between the last review and the report and may change the tree"
        );
    }
}

/// Message text reaches the listing step, which must bind the connector and
/// so cannot be isolated, and every later agent step through its item
/// section or a `{{nodes...}}` reference. Every such later step binds no
/// connector and is isolated, so outside the listing step the text reaches
/// only sandboxed steps where this machine has a sandbox.
#[test]
fn inbox_digest_steps_that_read_mail_text_are_isolated() {
    let doc = inbox_digest();
    let nodes = doc.get("nodes").and_then(|n| n.as_sequence()).unwrap();
    let mut readers = Vec::new();
    for node in nodes {
        let id = node.get("id").and_then(|v| v.as_str()).unwrap();
        let prompt = node.get("prompt").and_then(|v| v.as_str()).unwrap_or("");
        if node.get("connectors").is_some() {
            assert_eq!(id, "search", "only the listing step binds a connector");
            continue;
        }
        if !(prompt.contains("{{nodes.") || node.get("list").is_some()) {
            continue;
        }
        readers.push(id.to_string());
        let level = node
            .get("isolation")
            .and_then(|i| i.get("level"))
            .and_then(|v| v.as_str());
        assert!(
            matches!(level, Some("best_effort" | "full")),
            "`{id}` reads message text and is not isolated"
        );
    }
    assert_eq!(readers, ["classify", "digest"]);
}

/// Issue text and mail text are untrusted, and both shipped playbooks mark
/// their listing connector as the entry point. Every agent that renders
/// that text and can send outward must say why it accepts the risk (no
/// V179), and no acceptance may hang on a node with nothing to accept
/// (no V182). The V180 warnings about reached secrets are expected and
/// fine.
#[test]
fn bundled_agents_that_read_untrusted_text_say_why() {
    use apb_core::validate::{ValidationContext, validate};
    for id in ["apb-issue-triage", "apb-inbox-digest"] {
        let yaml = std::fs::read_to_string(
            bundled_root()
                .join("playbooks")
                .join(id)
                .join("playbook.yaml"),
        )
        .unwrap_or_else(|e| panic!("{id}/playbook.yaml: {e}"));
        let pb = Playbook::from_yaml(&yaml).unwrap_or_else(|e| panic!("{id}: {e}"));
        let report = validate(&pb, &ValidationContext::default());
        let blocking: Vec<String> = report
            .issues
            .iter()
            .filter(|i| i.code == "V179" || i.code == "V182")
            .map(|i| match &i.node {
                Some(node) => format!("{} {node}: {}", i.code, i.message),
                None => format!("{}: {}", i.code, i.message),
            })
            .collect();
        assert!(
            blocking.is_empty(),
            "{id}: untrusted input not accounted for:\n{}",
            blocking.join("\n")
        );
    }
}
