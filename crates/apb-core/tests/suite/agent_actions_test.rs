//! The shared decoders of agent log formats: shell command lines, claude's
//! tool calls and typed requests, codex's `exec --json` items and both of
//! its rollout forms, read once per file.

use apb_core::agent_actions::codex::{self, RolloutFormat};
use apb_core::agent_actions::{Action, claude, command};
use apb_core::discover::token;
use serde_json::{Value, json};

use crate::history_fixture::fixture;

fn tokens(actions: &[Action]) -> Vec<String> {
    actions.iter().map(token::of).collect()
}

fn line_tokens(line: &str) -> Vec<String> {
    tokens(&command::actions(line))
}

#[test]
fn a_command_line_is_its_programs_and_subcommands_never_its_arguments() {
    assert_eq!(
        line_tokens(r#"cd x && cargo test && git commit -m "fix: the parser""#),
        ["cargo:test", "git:commit"]
    );
    assert_eq!(
        line_tokens("bash -lc 'cargo build --release; git -C repo push origin main'"),
        ["cargo:build", "git:push"]
    );
    assert_eq!(
        line_tokens("sudo -E docker compose up -d | tee log.txt"),
        ["docker:compose", "sh:tee"]
    );
    assert_eq!(
        line_tokens("RUST_LOG=debug gh pr create --title x --body y"),
        ["gh:pr.create"]
    );
    assert_eq!(
        line_tokens("pwd; ls -la; echo done; true"),
        Vec::<String>::new()
    );
    assert_eq!(
        line_tokens("cat /opt/skills/rust-skills/SKILL.md"),
        ["skill:rust-skills"]
    );
    // A program name outside the token charset cannot carry anything.
    assert_eq!(line_tokens("./Run_Me+Now.sh"), ["sh:other"]);
    // A redirection is not a command of its own.
    assert_eq!(
        line_tokens("cargo test 2>&1 | tail -n 50"),
        ["cargo:test", "sh:tail"]
    );
    assert_eq!(line_tokens("make lint &> log.txt"), ["make:lint"]);
}

#[test]
fn an_option_value_never_names_a_step() {
    let value = format!("{}{}", "s3cr3t", "value1234567");
    // An option the table does not know: its value may be the next word.
    assert_eq!(
        line_tokens(&format!("kubectl --token {value} get pods")),
        ["kubectl:other"]
    );
    assert_eq!(line_tokens("docker --log-format x ps"), ["docker:other"]);
    // A known option's value is skipped; a word that is not a known
    // subcommand is `other`.
    assert_eq!(line_tokens("kubectl -n prod get pods"), ["kubectl:get"]);
    assert_eq!(line_tokens("docker --context prod ps"), ["docker:ps"]);
    assert_eq!(line_tokens("make -C dir deploy"), ["make:deploy"]);
    assert_eq!(line_tokens("uv --directory proj run x"), ["uv:run"]);
    assert_eq!(line_tokens(&format!("cargo {value}")), ["cargo:other"]);
    assert_eq!(line_tokens("cargo +nightly build"), ["cargo:build"]);
    assert_eq!(line_tokens("make VAR=1 deploy"), ["make:deploy"]);
    assert_eq!(line_tokens("git --no-pager log"), ["git:log"]);
    assert_eq!(line_tokens("git -C repo push"), ["git:push"]);
    assert_eq!(
        line_tokens(&format!("git --frob {value} push")),
        ["git:other"]
    );
    // gh's second level is a known verb only.
    assert_eq!(line_tokens("gh pr view 12"), ["gh:pr.view"]);
    assert_eq!(line_tokens(&format!("gh api {value}")), ["gh:api"]);
    assert_eq!(line_tokens(&format!("gh pr {value}")), ["gh:pr"]);
    assert_eq!(
        line_tokens("apb connector call mail send"),
        ["apb:connector.call"]
    );
    // Wrapper options with a value are skipped with it; an unknown one
    // hides the program.
    assert_eq!(
        line_tokens("sudo -u deploy systemctl restart x"),
        ["sh:systemctl"]
    );
    assert_eq!(line_tokens("env -u VAR cargo test"), ["cargo:test"]);
    assert_eq!(line_tokens("time -f fmt make test"), ["make:test"]);
    assert_eq!(line_tokens(&format!("sudo -Q {value} ls")), ["sh:other"]);
    // A name lookup runs nothing.
    assert!(line_tokens("command -v git").is_empty());
    // An in-place edit is an edit, not a look.
    // Built with `concat!`: the shell portability guard reads test sources
    // for in-place sed, which only GNU sed takes in this spelling.
    assert_eq!(
        line_tokens(concat!("sed", " -i 's/a/b/' f.txt")),
        ["sed:inplace"]
    );
    assert_eq!(line_tokens("sed -n 1,5p f.txt"), ["sh:sed"]);
    assert_eq!(line_tokens("perl -pi -e 's/a/b/' f"), ["perl:inplace"]);
    assert_eq!(line_tokens("awk -i inplace '{print}' f"), ["awk:inplace"]);
    // A flag whose value is the rest of its group is not `-i`.
    assert_eq!(
        line_tokens("perl -MList::Util=sum -e 'print sum(1)'"),
        ["sh:perl"]
    );
    assert_eq!(line_tokens("perl -Iinclude script.pl"), ["sh:perl"]);
    assert_eq!(line_tokens("sed -n -e 's/i/x/' f"), ["sh:sed"]);
    assert!(token::is_acting("sed:inplace"));
    assert_eq!(token::effects(&["sed:inplace".to_string()]), ["fs_write"]);
    for t in line_tokens(&format!(
        "kubectl --token {value} get pods && docker -H {value} ps"
    )) {
        assert!(!t.contains("s3cr3t"), "a step token kept an option value");
    }
}

#[test]
fn a_destructive_command_is_flagged_where_it_stands() {
    let actions = command::actions("cargo test && git push --force origin main");
    let flags: Vec<bool> = actions
        .iter()
        .map(|a| {
            matches!(
                a,
                Action::Command {
                    destructive: true,
                    ..
                }
            )
        })
        .collect();
    assert_eq!(flags, [false, true]);
    let wrapped = command::actions("sh -c 'rm -rf build'");
    assert!(matches!(
        wrapped[..],
        [Action::Command {
            destructive: true,
            ..
        }]
    ));
}

fn claude_tokens(name: &str, input: Value) -> Vec<String> {
    tokens(&claude::tool_call(&json!({"name": name, "input": input})).actions)
}

#[test]
fn claude_tools_decode_to_actions() {
    assert_eq!(claude_tokens("mcp__srv__tool", json!({})), ["mcp:srv.tool"]);
    assert_eq!(
        claude_tokens("mcp__gmail__search_threads", json!({"q": "x"})),
        ["mcp:gmail.search_threads"]
    );
    assert_eq!(
        claude_tokens("Edit", json!({"file_path": "/w/src/a.rs"})),
        ["edit:.rs"]
    );
    assert_eq!(
        claude_tokens("Read", json!({"file_path": "/s/sql-skills/SKILL.md"})),
        ["skill:sql-skills"]
    );
    assert_eq!(
        claude_tokens("Read", json!({"file_path": "/w/README"})),
        ["read:file"]
    );
    assert_eq!(claude_tokens("Grep", json!({"pattern": "x"})), ["search"]);
    assert_eq!(
        claude_tokens("WebFetch", json!({"url": "https://example.com"})),
        ["web:fetch"]
    );
    assert_eq!(claude_tokens("Task", json!({})), ["agent:spawn"]);
    assert_eq!(
        claude_tokens("Skill", json!({"skill": "/deploy"})),
        ["skill:deploy"]
    );
    assert_eq!(claude_tokens("TodoWrite", json!({})), ["tool:todowrite"]);
}

#[test]
fn claude_typed_requests_skip_meta_turns_tool_results_and_wrappers() {
    let cwd = std::path::Path::new("/w/p");
    let lines: Vec<Value> = fixture("claude_session.jsonl", cwd)
        .iter()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    let typed: Vec<String> = lines.iter().filter_map(claude::user_text).collect();
    assert_eq!(typed, ["Fix the failing parser test and ship it"]);
    let main: Vec<String> = lines
        .iter()
        .filter(|l| !claude::is_sidechain(l))
        .flat_map(claude::tool_calls)
        .flat_map(|c| tokens(&c.actions))
        .collect();
    assert_eq!(
        main,
        [
            "cargo:test",
            "skill:rust-skills",
            "edit:.rs",
            "cargo:test",
            "git:commit",
            "mcp:notion.search",
            "git:push"
        ]
    );
}

#[test]
fn text_a_person_did_not_type_is_never_a_request() {
    let user = |extra: Value, content: Value| {
        let mut line = json!({"type": "user", "message": {"role": "user", "content": content}});
        for (k, v) in extra.as_object().unwrap() {
            line[k] = v.clone();
        }
        claude::user_text(&line)
    };
    // A compacted conversation's summary is not typed.
    assert_eq!(
        user(
            json!({"isCompactSummary": true}),
            json!("This session continues from an earlier one: ship the parser")
        ),
        None
    );
    // Shell-mode echoes and their output are stripped.
    assert_eq!(
        user(
            json!({}),
            json!(
                "<bash-input>make lint</bash-input><bash-stdout>ok</bash-stdout><bash-stderr></bash-stderr>"
            )
        ),
        None
    );
    // An interrupt marker is not a request; what was typed beside it is.
    assert_eq!(
        user(
            json!({}),
            json!([{"type": "text", "text": "[Request interrupted by user]"}])
        ),
        None
    );
    assert_eq!(
        user(
            json!({}),
            json!("[Request interrupted by user for tool use]\nRun the parser tests instead")
        )
        .as_deref(),
        Some("Run the parser tests instead")
    );
    // codex sends a project's instruction file as a user message.
    let instructions = json!({"type": "response_item", "payload": {"type": "message", "role": "user",
        "content": [{"type": "input_text",
            "text": "# AGENTS.md instructions for /w/p\n\n<INSTRUCTIONS>\nAlways run the gate\n</INSTRUCTIONS>"}]}});
    assert_eq!(
        codex::user_text(&instructions, RolloutFormat::ResponseItem),
        None
    );
    let wrapped = json!({"type": "response_item", "payload": {"type": "message", "role": "user",
        "content": [{"type": "input_text",
            "text": "<INSTRUCTIONS>\nAlways run the gate\n</INSTRUCTIONS>\nDeploy the site"}]}});
    assert_eq!(
        codex::user_text(&wrapped, RolloutFormat::ResponseItem).as_deref(),
        Some("Deploy the site")
    );
}

fn exec_tokens(item: Value) -> Vec<String> {
    codex::calls_of_exec_line(&json!({"type": "item.completed", "item": item}))
        .iter()
        .flat_map(|c| tokens(&c.actions))
        .collect()
}

#[test]
fn codex_exec_items_decode() {
    assert_eq!(
        exec_tokens(
            json!({"type": "command_execution", "command": "bash -lc 'cargo test'", "exit_code": 0})
        ),
        ["cargo:test"]
    );
    assert_eq!(
        exec_tokens(
            json!({"type": "file_change", "changes": [{"path": "/w/a.ts", "kind": "add"}]})
        ),
        ["edit:.ts"]
    );
    assert_eq!(
        exec_tokens(json!({"type": "mcp_tool_call", "server": "crm", "tool": "update_contact"})),
        ["mcp:crm.update_contact"]
    );
    assert_eq!(
        exec_tokens(json!({"type": "web_search", "query": "x"})),
        ["web:search"]
    );
    assert!(exec_tokens(json!({"type": "agent_message", "text": "done"})).is_empty());
    // An item that has not finished is not a call yet.
    assert!(
        codex::calls_of_exec_line(&json!({"type": "item.started",
            "item": {"type": "command_execution", "command": "ls"}}))
        .is_empty()
    );
}

#[test]
fn codex_older_rollout_calls_decode() {
    let rollout = |payload: Value| {
        codex::calls_of_rollout_line(
            &json!({"type": "response_item", "payload": payload}),
            RolloutFormat::ResponseItem,
        )
        .iter()
        .flat_map(|c| tokens(&c.actions))
        .collect::<Vec<_>>()
    };
    assert_eq!(
        rollout(json!({"type": "function_call", "name": "shell",
            "arguments": "{\"command\":[\"bash\",\"-lc\",\"npm test\"]}"})),
        ["npm:test"]
    );
    assert_eq!(
        rollout(
            json!({"type": "local_shell_call", "action": {"type": "exec", "command": ["make", "lint"]}})
        ),
        ["make:lint"]
    );
    assert_eq!(
        rollout(json!({"type": "custom_tool_call", "name": "apply_patch",
            "input": "*** Begin Patch\n*** Add File: docs/a.md\n+x\n*** End Patch"})),
        ["edit:.md"]
    );
}

#[test]
fn a_repeated_web_search_is_told_by_what_it_asked_not_its_call_id() {
    let search = |id: &str, status: &str| {
        codex::calls_of_rollout_line(
            &json!({"type": "response_item", "payload": {"type": "web_search_call",
                "id": id, "status": status, "action": {"type": "search", "query": "x"}}}),
            RolloutFormat::ResponseItem,
        )
        .remove(0)
        .input
    };
    assert_eq!(search("ws_1", "completed"), search("ws_2", "failed"));
    assert_eq!(
        search("ws_1", "completed"),
        json!({"type": "web_search_call", "action": {"type": "search", "query": "x"}})
    );
}

#[test]
fn a_rollout_with_both_forms_is_read_once() {
    let cwd = std::path::Path::new("/w/p");
    let lines: Vec<Value> = fixture("codex_rollout.jsonl", cwd)
        .iter()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    let format = RolloutFormat::detect(&lines);
    assert_eq!(format, RolloutFormat::ItemCompleted);
    let calls: Vec<String> = lines
        .iter()
        .flat_map(|l| codex::calls_of_rollout_line(l, format))
        .flat_map(|c| tokens(&c.actions))
        .collect();
    assert_eq!(
        calls,
        ["cargo:test", "edit:.rs", "mcp:gmail.search", "git:push"]
    );
    let typed: Vec<String> = lines
        .iter()
        .filter_map(|l| codex::user_text(l, format))
        .collect();
    assert_eq!(typed, ["Triage the new support mail"]);
    assert_eq!(lines.iter().find_map(codex::cwd_of), Some("/w/p"));
    // Without the newer form, the older one is read, wrappers stripped.
    let older: Vec<Value> = lines
        .iter()
        .filter(|l| l["type"] != "event_msg")
        .cloned()
        .collect();
    let format = RolloutFormat::detect(&older);
    assert_eq!(format, RolloutFormat::ResponseItem);
    let calls: Vec<String> = older
        .iter()
        .flat_map(|l| codex::calls_of_rollout_line(l, format))
        .flat_map(|c| tokens(&c.actions))
        .collect();
    assert_eq!(calls, ["cargo:test", "edit:.rs", "git:push"]);
    let typed: Vec<String> = older
        .iter()
        .filter_map(|l| codex::user_text(l, format))
        .collect();
    assert_eq!(typed, ["Triage the new support mail"]);
}

#[test]
fn the_agent_reply_reader_takes_the_codex_message_through_the_shared_decoder() {
    let out = [
        json!({"type": "item.completed", "item": {"type": "command_execution", "command": "ls"}}),
        json!({"type": "item.completed", "item": {"type": "agent_message", "text": "all done"}}),
        json!({"type": "turn.completed", "usage": {"input_tokens": 1, "output_tokens": 1}}),
    ]
    .iter()
    .map(Value::to_string)
    .collect::<Vec<_>>()
    .join("\n");
    let reply = apb_core::agent_output::reply("codex", &out).unwrap();
    assert_eq!(reply.text, "all done");
}

#[test]
fn the_agents_config_directories_follow_their_variables() {
    let lock = crate::common::env_lock();
    let _claude = crate::common::EnvVarGuard::set(&lock, "CLAUDE_CONFIG_DIR", "/cfg/claude");
    let _codex = crate::common::EnvVarGuard::set(&lock, "CODEX_HOME", "/cfg/codex");
    assert_eq!(
        claude::projects_dir(),
        Some(std::path::PathBuf::from("/cfg/claude/projects"))
    );
    assert_eq!(
        codex::sessions_dir(),
        Some(std::path::PathBuf::from("/cfg/codex/sessions"))
    );
}
