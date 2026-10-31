//! History discovery over synthetic sessions: what the walk reads and
//! refuses, what the miner finds (a code routine and a mail and CRM
//! routine), what a draft keeps of what a person typed, dismissals, and the
//! signed store.

use std::path::Path;

use apb_core::discover::{
    self, DiscoverError, DiscoverResult, Suggestion, SuggestionStatus, store,
};
use serde_json::json;

use crate::common::config_sandbox;
use crate::history_fixture::{
    History, claude_bash, claude_tool, code_routine, codex_item, codex_mcp, codex_meta, codex_user,
    fixture,
};

const CODE_STEPS: [&str; 5] = [
    "cargo:test",
    "edit:.rs",
    "cargo:test",
    "git:commit",
    "git:push",
];

fn steps(s: &Suggestion) -> Vec<&str> {
    s.steps.iter().map(String::as_str).collect()
}

fn root_report<'a>(r: &'a DiscoverResult, name: &str) -> &'a discover::RootReport {
    r.read.roots.iter().find(|x| x.name == name).unwrap()
}

#[test]
fn three_sessions_on_two_days_give_one_maximal_pattern() {
    let _cfg = config_sandbox();
    let h = History::new();
    let p = h.project("app");
    for (i, age) in [1, 1, 2].iter().enumerate() {
        h.claude(
            &p,
            &format!("s{i}"),
            &code_routine(&p.root, "Ship the fix"),
            *age,
        );
    }
    let r = discover::discover(&h.request(&[&p])).unwrap();
    assert_eq!(r.read.sessions, 3);
    assert_eq!(r.suggestions.len(), 1);
    let s = &r.suggestions[0];
    assert_eq!(steps(s), CODE_STEPS);
    assert_eq!((s.support.sessions, s.support.days), (3, 2));
    assert_eq!(s.support.projects, [p.seen()]);
    assert!(s.sources.iter().all(|x| x.workspace_id == "ws-app"));
    assert_eq!(s.score, 3 * 4);
    assert!(s.slug.starts_with("seq-") && s.slug.len() == 14);
    assert_eq!(
        s.title,
        "Repeated steps: cargo test, edit .rs, cargo test, git commit, git push"
    );
    assert_eq!(s.effects, ["fs_write", "network", "external"]);
    assert_eq!(s.sources.len(), 3);
    assert!(
        s.sources
            .iter()
            .all(|x| x.root == "claude_code" && x.file.starts_with("app/") && x.lines == [2, 5])
    );
    // The same request typed in three sessions is one example.
    assert_eq!(s.examples.len(), 1);
    assert!(
        s.examples
            .iter()
            .all(|e| e.text == "Ship the fix" && e.source.lines == [1, 1])
    );
    assert_eq!(s.status, SuggestionStatus::New);
    // Nothing in the result names a path on this machine.
    let out = serde_json::to_string(&r).unwrap();
    assert!(!out.contains(&*h.dir.path().to_string_lossy()));
    // The same history gives the same suggestions.
    let again = discover::discover(&h.request(&[&p])).unwrap();
    assert_eq!(again.suggestions, r.suggestions);
}

#[test]
fn three_sessions_on_one_day_are_not_a_routine_yet() {
    let _cfg = config_sandbox();
    let h = History::new();
    let p = h.project("app");
    for i in 0..3 {
        h.claude(&p, &format!("s{i}"), &code_routine(&p.root, "x"), 1);
    }
    let r = discover::discover(&h.request(&[&p])).unwrap();
    assert_eq!(r.read.sessions, 3);
    assert!(r.suggestions.is_empty());
}

#[test]
fn a_read_only_routine_is_not_offered_and_repeated_steps_collapse() {
    let _cfg = config_sandbox();
    let h = History::new();
    let looks = h.project("looks");
    let acts = h.project("acts");
    for (i, age) in [1, 2, 3].iter().enumerate() {
        let c = &looks.root;
        h.claude(
            &looks,
            &format!("r{i}"),
            &[
                claude_bash(c, "git status"),
                claude_bash(c, "git diff"),
                claude_tool(c, "Grep", json!({"pattern": "x"})),
                claude_tool(c, "Read", json!({"file_path": c.join("a.md")})),
            ],
            *age,
        );
        let c = &acts.root;
        h.claude(
            &acts,
            &format!("a{i}"),
            &[
                claude_bash(c, "cargo test"),
                claude_bash(c, "cargo test"),
                claude_bash(c, "cargo test"),
                claude_tool(c, "Write", json!({"file_path": c.join("notes.md")})),
                claude_bash(c, "git commit -am done"),
            ],
            *age,
        );
    }
    let r = discover::discover(&h.request(&[&looks])).unwrap();
    assert_eq!(r.read.sessions, 3);
    assert!(r.suggestions.is_empty());
    let r = discover::discover(&h.request(&[&acts])).unwrap();
    assert_eq!(r.suggestions.len(), 1);
    assert_eq!(
        steps(&r.suggestions[0]),
        ["cargo:test", "edit:.md", "git:commit"]
    );
}

#[test]
fn a_mail_and_crm_routine_is_found_in_codex_rollouts() {
    let _cfg = config_sandbox();
    let h = History::new();
    let p = h.project("support");
    for (i, age) in [1, 3, 5, 8].iter().enumerate() {
        h.codex(
            &format!("r{i}"),
            &[
                codex_meta(&p.root),
                codex_user("Answer this morning's customer mail and log it"),
                codex_mcp("gmail", "search"),
                codex_mcp("gmail", "read"),
                codex_mcp("crm", "update"),
                codex_item(json!({"type": "AgentMessage", "text": "done"})),
            ],
            *age,
        );
    }
    let r = discover::discover(&h.request(&[&p])).unwrap();
    assert_eq!(r.read.sessions, 4);
    assert_eq!(r.suggestions.len(), 1);
    let s = &r.suggestions[0];
    assert_eq!(
        steps(s),
        ["mcp:gmail.search", "mcp:gmail.read", "mcp:crm.update"]
    );
    assert_eq!((s.support.sessions, s.support.days), (4, 4));
    assert_eq!(s.effects, ["network", "external"]);
    assert_eq!(s.examples.len(), 1);
    assert_eq!(
        s.examples[0].text,
        "Answer this morning's customer mail and log it"
    );
    assert!(
        s.sources
            .iter()
            .all(|x| x.root == "codex" && x.file.starts_with("support/rollout-"))
    );
    // The draft is a playbook the validator accepts.
    let pb = apb_core::schema::Playbook::from_yaml(&s.yaml).unwrap();
    assert_eq!(pb.id, s.slug);
    let report =
        apb_core::validate::validate(&pb, &apb_core::validate::ValidationContext::default());
    assert!(report.is_valid(), "{:?}", report.issues);
    assert_eq!(s.notes, ["needs_profile"]);
    assert_eq!(
        s.synopsis["steps"],
        json!(["mcp gmail search", "mcp gmail read", "mcp crm update"])
    );
}

#[test]
fn the_fixture_sessions_read_as_their_steps() {
    let _cfg = config_sandbox();
    let h = History::new();
    let p = h.project("app");
    for (i, age) in [1, 2, 3].iter().enumerate() {
        h.claude(
            &p,
            &format!("c{i}"),
            &fixture("claude_session.jsonl", &p.root),
            *age,
        );
        h.codex(
            &format!("x{i}"),
            &fixture("codex_rollout.jsonl", &p.root),
            *age,
        );
    }
    let r = discover::discover(&h.request(&[&p])).unwrap();
    assert_eq!(r.read.sessions, 6);
    let by_steps: Vec<Vec<&str>> = r.suggestions.iter().map(steps).collect();
    assert!(by_steps.contains(&vec![
        "cargo:test",
        "skill:rust-skills",
        "edit:.rs",
        "cargo:test",
        "git:commit",
        "mcp:notion.search",
        "git:push"
    ]));
    assert!(by_steps.contains(&vec![
        "cargo:test",
        "edit:.rs",
        "mcp:gmail.search",
        "git:push"
    ]));
    let claude = r
        .suggestions
        .iter()
        .find(|s| s.steps.contains(&"mcp:notion.search".to_string()))
        .unwrap();
    // A forced push was among the steps: the draft says it cannot be undone.
    assert!(claude.effects.contains(&"irreversible".to_string()));
    assert_eq!(
        claude.examples[0].text,
        "Fix the failing parser test and ship it"
    );
    // The sub-agent's publish is not a step of the session.
    assert!(
        r.suggestions
            .iter()
            .all(|s| !s.steps.contains(&"npm:publish".to_string()))
    );
    // A long sequence still drafts a playbook the validator accepts: its
    // trigger line is cut after a whole step, its examples kept once.
    let pb = apb_core::schema::Playbook::from_yaml(&claude.yaml).unwrap();
    let report =
        apb_core::validate::validate(&pb, &apb_core::validate::ValidationContext::default());
    assert!(report.is_valid(), "{:?}", report.issues);
    let trigger = pb.trigger.unwrap();
    assert!(trigger.when[0].chars().count() <= 120);
    // Cut after a whole step, with the slug, so two repeats that share
    // their first steps never share a trigger.
    assert!(
        trigger.when[0].ends_with(&format!(", cargo test, and more ({})", claude.slug)),
        "{}",
        trigger.when[0]
    );
    assert_eq!(
        trigger.examples,
        ["Fix the failing parser test and ship it"]
    );
}

#[test]
fn examples_are_scrubbed_and_a_keyed_value_drops_its_example() {
    let _cfg = config_sandbox();
    let h = History::new();
    let p = h.project("app");
    let requests = [
        "Ship it, the notes are in /home/someone/notes.txt",
        "Ship it with api_key: abcd1234efgh5678",
        "Ship it and mail dev.person@example.org",
        "Ship it, the build is in /srv/ci-box/out (see C:\\builds\\x)",
    ];
    for (i, req) in requests.iter().enumerate() {
        h.claude(
            &p,
            &format!("s{i}"),
            &code_routine(&p.root, req),
            i as u64 + 1,
        );
    }
    let r = discover::discover(&h.request(&[&p])).unwrap();
    let s = &r.suggestions[0];
    let texts: Vec<&str> = s.examples.iter().map(|e| e.text.as_str()).collect();
    // The most recent sessions first; an absolute path the scrub does not
    // rewrite is `[path]`.
    assert_eq!(
        texts,
        [
            "Ship it, the notes are in ~/notes.txt",
            "Ship it and mail [email]",
            "Ship it, the build is in [path] (see [path])",
        ]
    );
    let out = serde_json::to_string(&r).unwrap();
    for planted in ["abcd1234", "someone", "dev.person", "ci-box", "builds"] {
        assert!(
            !out.contains(planted),
            "a planted fixture reached the result"
        );
    }
}

#[test]
fn a_configured_connector_value_never_reaches_the_result() {
    let _cfg = config_sandbox();
    let config_dir = std::path::PathBuf::from(std::env::var_os("APB_CONFIG_DIR").unwrap());
    let h = History::new();
    let p = h.project("crm");
    let connector = config_dir.join("connectors/mock-crm");
    std::fs::create_dir_all(&connector).unwrap();
    std::fs::write(
        connector.join("connector.yaml"),
        "name: mock-crm\nversion: 0.1.0\naccount_fields:\n  - name: base_url\n    required: true\n  - name: token\n    required: true\n    secret: true\nfunctions:\n  - name: list_items\n    description: List items\n    read_only: true\n    method: GET\n    url: \"{{account.base_url}}/items\"\n",
    )
    .unwrap();
    let accounts = apb_core::connector::config::project_config_path(&p.root, "mock-crm");
    std::fs::create_dir_all(accounts.parent().unwrap()).unwrap();
    std::fs::write(
        &accounts,
        "accounts:\n  - name: main\n    base_url: https://crm.example.com\n    token: \"{{env.APB_DISCOVER_TEST_CRM_VALUE}}\"\n",
    )
    .unwrap();
    let configured = format!("cfg{}", "Q7v".repeat(4));
    std::fs::create_dir_all(p.root.join(".apb")).unwrap();
    std::fs::write(
        p.root.join(".apb/secrets.env"),
        format!("APB_DISCOVER_TEST_CRM_VALUE={configured}\n"),
    )
    .unwrap();
    for i in 0..3 {
        h.claude(
            &p,
            &format!("s{i}"),
            &code_routine(&p.root, &format!("Sync the CRM, the value is {configured}")),
            i + 1,
        );
    }
    let r = discover::discover(&h.request(&[&p])).unwrap();
    assert_eq!(r.suggestions.len(), 1);
    let out = serde_json::to_string(&r).unwrap();
    assert!(
        !out.contains(&configured),
        "a configured value reached the result"
    );
    // A request that held the value is dropped whole, not rewritten.
    assert!(r.suggestions[0].examples.is_empty());
}

#[test]
fn a_dismissed_slug_is_suppressed_and_listed_in_either_scope() {
    use apb_core::dismiss::{DecisionInput, DecisionKind, DecisionScope, record_decision};
    let _cfg = config_sandbox();
    let h = History::new();
    let p = h.project("app");
    for (i, age) in [1, 2, 3].iter().enumerate() {
        h.claude(&p, &format!("s{i}"), &code_routine(&p.root, "x"), *age);
    }
    let slug = discover::discover(&h.request(&[&p])).unwrap().suggestions[0]
        .slug
        .clone();
    for (scope, name) in [
        (DecisionScope::Project, "project"),
        (DecisionScope::Global, "global"),
    ] {
        apb_core::dismiss::reset_records(&p.root, None).unwrap();
        record_decision(
            &p.root,
            DecisionInput {
                pattern: slug.clone(),
                synopsis: "repeated code routine".into(),
                kind: DecisionKind::Hard,
                scope,
                hard_ttl_days_override: None,
            },
        )
        .unwrap();
        let r = discover::discover(&h.request(&[&p])).unwrap();
        assert!(r.suggestions.is_empty(), "{name}");
        assert_eq!(r.suppressed.len(), 1, "{name}");
        assert_eq!(r.suppressed[0].slug, slug);
        assert_eq!(r.suppressed[0].scope, name);
        assert_eq!(r.suppressed[0].kind, "hard");
        apb_core::dismiss::remove_record(&p.root, &slug, scope).unwrap();
    }
}

#[test]
fn the_walk_refuses_links_and_old_files_and_keeps_to_its_caps() {
    let _cfg = config_sandbox();
    let h = History::new();
    let p = h.project("app");
    for (i, age) in [1, 2, 3].iter().enumerate() {
        h.claude(&p, &format!("s{i}"), &code_routine(&p.root, "x"), *age);
    }
    let old = h.claude(&p, "old", &code_routine(&p.root, "x"), 200);
    #[cfg(unix)]
    std::os::unix::fs::symlink(&old, old.with_file_name("link.jsonl")).unwrap();
    // A line over the line cap is skipped, the rest of its file read.
    let long = format!(
        "{{\"type\":\"user\",\"message\":{{\"content\":\"{}\"}}}}",
        "y".repeat(4096)
    );
    let mut lines = code_routine(&p.root, "x");
    lines.insert(0, long);
    h.claude(&p, "long", &lines, 4);
    let mut req = h.request(&[&p]);
    req.limits.max_line_bytes = 2048;
    let r = discover::discover(&req).unwrap();
    let c = root_report(&r, "claude_code");
    assert_eq!(c.skipped.get("too_old"), Some(&1));
    #[cfg(unix)]
    assert_eq!(c.skipped.get("symlink"), Some(&1));
    assert_eq!(c.skipped.get("line_too_long"), Some(&1));
    assert_eq!(r.read.sessions, 4);
    // A file read by its prefix only is marked cut.
    let mut req = h.request(&[&p]);
    req.limits.max_file_bytes = 300;
    let r = discover::discover(&req).unwrap();
    let c = root_report(&r, "claude_code");
    assert_eq!(c.cut_files, 4);
    assert!(c.bytes_read <= 4 * 300);
    // The file cap stops reading.
    let mut req = h.request(&[&p]);
    req.limits.max_files = 2;
    let r = discover::discover(&req).unwrap();
    let c = root_report(&r, "claude_code");
    assert_eq!(c.files_read, 2);
    assert_eq!(c.skipped.get("file_cap"), Some(&2));
    // A symlinked root is refused, never followed.
    #[cfg(unix)]
    {
        let link = h.dir.path().join("linked-root");
        std::os::unix::fs::symlink(h.claude_root(), &link).unwrap();
        let mut req = h.request(&[&p]);
        req.roots[0].path = link;
        assert_eq!(
            discover::discover(&req).unwrap_err(),
            DiscoverError::RootIsSymlink("claude_code".into())
        );
    }
}

#[test]
fn another_projects_rollout_is_closed_after_its_header() {
    let _cfg = config_sandbox();
    let h = History::new();
    let mine = h.project("mine");
    let other = h.project("other");
    let mut lines = vec![codex_meta(&other.root), codex_user("x")];
    for _ in 0..4000 {
        lines.push(codex_mcp("crm", "update"));
    }
    let path = h.codex("big", &lines, 1);
    assert!(std::fs::metadata(&path).unwrap().len() > 3 * discover::walk::CODEX_HEADER_BYTES);
    // A claude directory of a sibling whose encoded name shares the prefix
    // is not the project's.
    let sibling = h.project("mine-two");
    h.claude(&sibling, "s", &code_routine(&sibling.root, "x"), 1);
    let r = discover::discover(&h.request(&[&mine])).unwrap();
    let x = root_report(&r, "codex");
    assert_eq!(x.skipped.get("other_project"), Some(&1));
    assert!(x.bytes_read <= discover::walk::CODEX_HEADER_BYTES);
    assert_eq!(
        root_report(&r, "claude_code").skipped.get("other_project"),
        Some(&1)
    );
    assert_eq!(r.read.sessions, 0);
}

fn stored_result(h: &History) -> DiscoverResult {
    let p = h.project("app");
    for (i, age) in [1, 2, 3].iter().enumerate() {
        h.claude(&p, &format!("s{i}"), &code_routine(&p.root, "x"), *age);
    }
    discover::discover(&h.request(&[&p])).unwrap()
}

#[test]
fn the_store_signs_keeps_ten_and_marks_only_signed_results() {
    let _cfg = config_sandbox();
    let h = History::new();
    let mut r = stored_result(&h);
    store::save(&mut r).unwrap();
    let back = store::load(&r.id).unwrap();
    assert!(back.signed);
    assert_eq!(back.result, r);
    let slug = r.suggestions[0].slug.clone();
    let saved = SuggestionStatus::Saved {
        playbook: slug.clone(),
        version: "0.1.0".into(),
        scope: "project".into(),
    };
    let marked = store::mark(&r.id, &slug, saved.clone()).unwrap();
    assert_eq!(marked.suggestions[0].status, saved);
    assert!(store::load(&r.id).unwrap().signed);
    assert_eq!(
        store::mark(&r.id, "seq-0000000000", SuggestionStatus::Dismissed).unwrap_err(),
        store::StoreError::NoSuggestion("seq-0000000000".into())
    );
    // An edited file still lists, but nothing is saved from it.
    let dir =
        std::path::PathBuf::from(std::env::var_os("APB_CONFIG_DIR").unwrap()).join(store::DIR);
    let path = dir.join(format!("{}.json", r.id));
    let mut edited = marked.clone();
    edited.suggestions[0].yaml.push_str("# edited\n");
    std::fs::write(&path, serde_json::to_vec(&edited).unwrap()).unwrap();
    assert!(!store::load(&r.id).unwrap().signed);
    assert!(
        store::list()
            .iter()
            .any(|s| s.result.id == r.id && !s.signed)
    );
    assert_eq!(
        store::load_signed(&r.id).unwrap_err(),
        store::StoreError::Unsigned(r.id.clone())
    );
    assert_eq!(
        store::mark(&r.id, &slug, SuggestionStatus::Dismissed).unwrap_err(),
        store::StoreError::Unsigned(r.id.clone())
    );
    // Only the newest ten are kept.
    for i in 0..11u64 {
        let mut next = r.clone();
        next.id = format!("disc-{:010x}", i + 1);
        next.created_at = r.created_at + 1 + i;
        next.mac = None;
        store::save(&mut next).unwrap();
    }
    let ids: Vec<String> = store::list().into_iter().map(|s| s.result.id).collect();
    assert_eq!(ids.len(), store::KEEP);
    assert_eq!(ids[0], "disc-000000000b");
    assert!(!ids.contains(&r.id));
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(dir.join("disc-000000000b.json"))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600);
    }
    // A planted file under a result name that is a link is not read: the
    // target is a whole result of that very id, so only the link refusal
    // keeps it out.
    #[cfg(unix)]
    {
        let mut planted = marked.clone();
        planted.id = "disc-ffffffffff".into();
        let target = h.dir.path().join("planted.json");
        std::fs::write(&target, serde_json::to_vec(&planted).unwrap()).unwrap();
        let link = dir.join("disc-ffffffffff.json");
        std::os::unix::fs::symlink(&target, &link).unwrap();
        assert!(matches!(
            store::load("disc-ffffffffff"),
            Err(store::StoreError::NotFound(_))
        ));
        // The same bytes as a regular file do read (unsigned).
        std::fs::remove_file(&link).unwrap();
        std::fs::copy(&target, &link).unwrap();
        assert!(!store::load("disc-ffffffffff").unwrap().signed);
    }
}

/// A narrow source guard: the discovery and decoder modules themselves
/// name no network client, no decision model and no process spawn. It
/// reads only these two directories, not what they call (the registry,
/// the dismiss and result stores, the scrubber), which their own tests
/// cover.
#[test]
fn the_discover_and_decoder_sources_name_no_network_model_or_process() {
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut files = Vec::new();
    for dir in ["discover", "agent_actions"] {
        for e in std::fs::read_dir(src.join(dir)).unwrap() {
            files.push(e.unwrap().path());
        }
    }
    assert!(files.len() >= 10);
    // String literals are left out: a program name such as `apb
    // decisions` in a table is not a use of the module.
    let literal = regex::Regex::new(r#""(?:[^"\\]|\\.)*""#).unwrap();
    for f in files {
        let text = std::fs::read_to_string(&f).unwrap();
        let code = literal.replace_all(&text, "\"\"");
        for banned in [
            "ureq",
            "std::net",
            "TcpStream",
            "decision",
            "apb_decide",
            "Command::new",
        ] {
            assert!(
                !code.contains(banned),
                "{} names {banned}",
                f.file_name().unwrap().to_string_lossy()
            );
        }
    }
}

#[test]
fn the_discover_section_names_roots_and_is_checked() {
    use apb_core::config::GlobalConfig;
    let ok: GlobalConfig = yaml_serde::from_str(
        "discover:\n  roots: [{ name: work_claude, agent: claude_code, path: /h/projects }]\n  since_days: 30\n",
    )
    .unwrap();
    ok.discover.validate().unwrap();
    let home: GlobalConfig = yaml_serde::from_str(
        "discover:\n  roots: [{ name: c, agent: codex, path: ~/.codex/sessions }]\n",
    )
    .unwrap();
    home.discover.validate().unwrap();
    let roots = discover::HistoryRoot::configured(&ok.discover);
    assert_eq!(roots.len(), 1);
    assert_eq!(roots[0].name, "work_claude");
    assert_eq!(roots[0].agent, discover::HistoryAgent::ClaudeCode);
    assert_eq!(roots[0].path, Path::new("/h/projects"));
    // A root's path is never part of what a surface shows.
    assert_eq!(
        serde_json::to_value(&roots[0]).unwrap(),
        json!({"name": "work_claude", "agent": "claude_code"})
    );
    for bad in [
        "discover:\n  since_days: 0\n",
        "discover:\n  since_days: 400\n",
        "discover:\n  roots: [{ name: Work, agent: codex, path: /x }]\n",
        "discover:\n  roots: [{ name: w, agent: other, path: /x }]\n",
        "discover:\n  roots: [{ name: w, agent: codex, path: /x }, { name: w, agent: codex, path: /y }]\n",
        "discover:\n  roots: [{ name: w, agent: codex, path: '' }]\n",
        "discover:\n  roots: [{ name: w, agent: codex, path: '~' }]\n",
        "discover:\n  roots: [{ name: w, agent: codex, path: projects }]\n",
    ] {
        let cfg: GlobalConfig = yaml_serde::from_str(bad).unwrap();
        assert!(cfg.discover.validate().is_err(), "{bad}");
    }
}
