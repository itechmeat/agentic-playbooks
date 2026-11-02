//! List mode and send-once through the real binary: a list item's step
//! sends through `apb connector call` against a scripted HTTP service, and
//! the journal and the service's own record of requests say what went out.
//!
//! A `none` send whose answer is lost leaves the item needing a look; it is
//! never sent again, not by the resume of a paused run and not by a later
//! loop visit. A `key` send whose answer is lost gets one free reconcile run
//! of its item, whose repeat carries the same nonce. Once a person confirms
//! a lost send as sent, the item's next run answers `already_sent` even when
//! it writes the payload differently. A binding that allows a resend lets
//! the item pass on its resend, and every surface that lists items says the
//! earlier send is still unresolved.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use apb_engine::event::{Event, EventPayload, read_all};

use crate::common::{RunGuard, apb_bin, apb_std};
use crate::http_service::{Script, Service};

const DEADLINE: Duration = Duration::from_secs(60);

fn poll<T>(what: &str, mut f: impl FnMut() -> Option<T>) -> T {
    let start = Instant::now();
    loop {
        if let Some(v) = f() {
            return v;
        }
        assert!(start.elapsed() < DEADLINE, "timed out waiting for: {what}");
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// `send` cannot be recognised again (`none`); `post` dedupes by a key.
const CONNECTOR: &str = r#"name: outbox
version: 0.1.0
account_fields:
  - name: api_base
    required: true
functions:
  - name: send
    description: Send a message the service cannot recognise again
    method: POST
    url: "{{account.api_base}}/send"
    body: { text: "{{args.text}}" }
    idempotency: { mode: none }
    args_schema: { type: object, properties: { text: { type: string } }, required: [text] }
  - name: post
    description: Post a message the service deduplicates by key
    method: POST
    url: "{{account.api_base}}/post"
    body: { text: "{{args.text}}" }
    idempotency: { mode: key, field: nonce, window_sec: 120 }
    args_schema: { type: object, properties: { text: { type: string } }, required: [text] }
"#;

/// A project with the `outbox` connector approved on account `default`
/// (at `base_url`), profile `main`, and playbook `lst` whose `work` node
/// lists `items` (a self-loop runs it twice when `looped`).
struct Project {
    dir: tempfile::TempDir,
}

impl Project {
    fn new(base_url: &str, items: &str, looped: bool) -> Project {
        Project::with_binding(base_url, items, looped, "")
    }

    /// [`Project::new`] with `binding` added to the node's connector
    /// binding (`, on_unknown: resend`).
    fn with_binding(base_url: &str, items: &str, looped: bool, binding: &str) -> Project {
        let dir = tempfile::tempdir().unwrap();
        let p = Project { dir };
        let root = p.root();
        fs::create_dir_all(root.join("cfg/connectors/outbox")).unwrap();
        fs::create_dir_all(root.join("home")).unwrap();
        fs::write(root.join("cfg/connectors/outbox/connector.yaml"), CONNECTOR).unwrap();
        p.ok(&["init"]);
        let acct = root.join(".apb/connector-config/outbox.yaml");
        fs::create_dir_all(acct.parent().unwrap()).unwrap();
        fs::write(
            &acct,
            format!("accounts:\n  - name: default\n    api_base: {base_url}\n"),
        )
        .unwrap();
        p.ok(&["connector", "approve", "outbox"]);
        p.ok(&["connector", "approve", "outbox", "--account", "default"]);
        let pdir = root.join(".apb/profiles/main");
        fs::create_dir_all(&pdir).unwrap();
        fs::write(
            pdir.join("profile.yaml"),
            "name: main\ndescription: test\nexecutor:\n  agent: claude\n  model: haiku\n",
        )
        .unwrap();
        fs::write(pdir.join("SOUL.md"), "test soul").unwrap();
        let edges = if looped {
            "  - { from: work, to: work, max_traversals: 1 }\n  - { from: work, to: done, fallback: true }\n"
        } else {
            "  - { from: work, to: done }\n"
        };
        let yaml = format!(
            "schema: 2\nid: lst\nname: List\nversion: 1.0.0\ndefaults: {{ profile: main }}\nnodes:\n  - {{ id: start, type: start }}\n  - id: work\n    type: agent_task\n    prompt: Send the item's message.\n    connectors: [{{ name: outbox, accounts: [default], functions: [send, post]{binding} }}]\n    list:\n      items: {{ inline: {items} }}\n      batch: {{ size: 3, parallel: 1, retries: 0 }}\n      item_outputs: [blocking]\n      check: {{ output: {{ field: blocking, equals: \"0\" }} }}\n  - {{ id: done, type: finish, outcome: success }}\nedges:\n  - {{ from: start, to: work }}\n{edges}"
        );
        let vdir = root.join(".apb/playbooks/lst/1.0.0");
        fs::create_dir_all(&vdir).unwrap();
        fs::write(vdir.join("playbook.yaml"), yaml).unwrap();
        fs::write(root.join(".apb/playbooks/lst/current"), "1.0.0").unwrap();
        fs::create_dir_all(p.stub_dir()).unwrap();
        p
    }

    fn root(&self) -> &Path {
        self.dir.path()
    }

    fn stub_dir(&self) -> PathBuf {
        self.root().join("stub")
    }

    /// `apb <args>` in the project with its own config dir.
    fn apb(&self, args: &[&str]) -> std::process::Output {
        apb_std()
            .args(args)
            .current_dir(self.root())
            .env("APB_CONFIG_DIR", self.root().join("cfg"))
            .env("HOME", self.root().join("home"))
            .env("APB_NO_REGISTRY", "1")
            .env("APB_AGENT_CMD", self.stub_dir().join("agent.sh"))
            .env_remove("APB_RUN_DIR")
            .env_remove("APB_NODE_ID")
            .output()
            .unwrap()
    }

    fn ok(&self, args: &[&str]) -> String {
        let out = self.apb(args);
        assert!(
            out.status.success(),
            "`apb {}` failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).into_owned()
    }

    /// The stub agent: counts its runs of each item (`n-<item>`), marks
    /// `started-<item>`, runs `hook`, then reports `blocking: 0`.
    fn stub(&self, hook: &str) {
        let body = format!(
            r#"#!/bin/sh
D='{d}'
APB='{apb}'
I="${{APB_ITEM_ID:-none}}"
N=$(cat "$D/n-$I" 2>/dev/null || echo 0); N=$((N+1)); echo $N > "$D/n-$I"
touch "$D/started-$I"
{hook}
printf '{{"status":"success","outputs":{{"blocking":"0"}}}}' > "$APB_STATUS_FILE"
printf '\n```yaml\nstatus: success\nsummary: ok\n```\n'
"#,
            d = self.stub_dir().display(),
            apb = apb_bin().display(),
        );
        let path = self.stub_dir().join("agent.sh");
        let mut f = fs::File::create(&path).unwrap();
        f.write_all(body.as_bytes()).unwrap();
        f.sync_all().unwrap();
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
    }

    fn runs_of(&self, item: &str) -> u32 {
        fs::read_to_string(self.stub_dir().join(format!("n-{item}")))
            .map(|s| s.trim().parse().unwrap())
            .unwrap_or(0)
    }

    fn events(&self, run_id: &str) -> Vec<Event> {
        read_all(&self.root().join(".apb/runs").join(run_id)).unwrap()
    }
}

/// `(state, reason)` of every `list_item_settled` of `item`, in order.
fn settlements(events: &[Event], item: &str) -> Vec<(String, Option<String>)> {
    events
        .iter()
        .filter_map(|e| match &e.payload {
            EventPayload::ListItemSettled {
                item: i,
                state,
                reason,
                ..
            } if i == item => Some((state.clone(), reason.clone())),
            _ => None,
        })
        .collect()
}

/// The run id in `run started: <id>` or `run <id> finished: ...`.
fn run_id_of(stdout: &str) -> String {
    stdout
        .lines()
        .find_map(|l| l.strip_prefix("run started: "))
        .map(str::to_string)
        .or_else(|| stdout.split_whitespace().nth(1).map(str::to_string))
        .expect("a run id")
        .trim()
        .to_string()
}

/// Item b's `none` send loses its answer and its step reports success: b
/// needs a look (`unknown_send`), since nobody knows whether it went out.
/// A `--now` pause interrupts c; the resume runs c alone, and the loop
/// visit after it runs a and c again but parks b: the send went out once.
#[test]
fn an_unknown_none_send_is_sent_once_across_a_resume_and_a_loop_visit() {
    let service = Service::spawn(Script::from([("/send", vec![None, None, None])]));
    let p = Project::new(
        &service.base_url,
        "[ { id: a }, { id: b }, { id: c } ]",
        true,
    );
    p.stub(
        r#"if [ "$I" = b ]; then "$APB" connector call outbox send --args '{"text":"hi"}' > "$D/call-$I-$N"; fi
if [ "$I" = c ] && [ "$N" = 1 ]; then while [ -f "$D/gate" ]; do sleep 0.05; done; fi"#,
    );
    fs::write(p.stub_dir().join("gate"), "").unwrap();
    let id = run_id_of(&p.ok(&["run", "lst", "--detach"]));
    let _guard = RunGuard::new(p.root(), &id);
    poll("item c to start", || {
        p.stub_dir().join("started-c").is_file().then_some(())
    });
    p.ok(&["pause", &id, "--now"]);
    let waited = p.apb(&["wait", &id, "--timeout", "60"]);
    assert_eq!(waited.status.code(), Some(4), "the run is paused");
    fs::remove_file(p.stub_dir().join("gate")).unwrap();
    assert_eq!(service.bodies("/send").len(), 1);
    let ev = p.events(&id);
    assert_eq!(
        settlements(&ev, "b").last(),
        Some(&("needs_look".to_string(), Some("unknown_send".to_string())))
    );

    let resumed = p.ok(&["resume", &id]);
    assert!(resumed.contains("finished: succeeded"), "{resumed}");
    assert_eq!(service.bodies("/send").len(), 1, "the send went out once");
    assert_eq!(
        (p.runs_of("a"), p.runs_of("b"), p.runs_of("c")),
        (2, 1, 3),
        "a and c ran in both visits (c once more after the pause); b never again"
    );
    let ev = p.events(&id);
    let visits = ev
        .iter()
        .filter(|e| {
            matches!(
                &e.payload,
                EventPayload::ListStarted {
                    reopened: false,
                    ..
                }
            )
        })
        .count();
    assert_eq!(visits, 2);
    assert_eq!(
        settlements(&ev, "b").last(),
        Some(&("needs_look".to_string(), Some("unknown_send".to_string()))),
        "the loop visit parks b"
    );
}

/// Item b's `key` send loses its answer and its step fails: b gets one free
/// reconcile run, whose repeat replays the send with the same nonce; the
/// service answers it and b passes.
#[test]
fn an_unknown_key_send_is_reconciled_by_one_free_run() {
    let service = Service::spawn(Script::from([(
        "/post",
        vec![None, Some((200, r#"{"id":"m-1"}"#))],
    )]));
    let p = Project::new(&service.base_url, "[ { id: a }, { id: b } ]", false);
    p.stub(
        r#"if [ "$I" = b ]; then "$APB" connector call outbox post --args '{"text":"hi"}' > "$D/call-$I-$N" || exit 1; fi"#,
    );
    let out = p.ok(&["run", "lst"]);
    assert!(out.contains("finished: succeeded"), "{out}");
    let id = run_id_of(&out);
    let posts = service.bodies("/post");
    assert_eq!(posts.len(), 2, "the send and its replay");
    assert!(posts[0]["nonce"].is_string());
    assert_eq!(
        posts[0]["nonce"], posts[1]["nonce"],
        "the replay keeps the nonce"
    );
    assert_eq!(
        settlements(&p.events(&id), "b"),
        [
            ("interrupted".to_string(), Some("reconcile".to_string())),
            ("passed".to_string(), None)
        ]
    );
    assert_eq!((p.runs_of("a"), p.runs_of("b")), (1, 2));
}

/// Item b's `none` send loses its answer; while the run is paused a person
/// confirms it went out. The loop visit runs b again, and its send, made
/// without `--intent` and with different text, is the same send of the
/// item: it answers `already_sent` and nothing reaches the service.
#[test]
fn a_send_confirmed_by_a_person_is_not_resent_when_the_rerun_rewrites_it() {
    let service = Service::spawn(Script::from([(
        "/send",
        vec![None, Some((200, r#"{"id":"m-2"}"#))],
    )]));
    let p = Project::new(&service.base_url, "[ { id: b }, { id: c } ]", true);
    p.stub(
        r#"if [ "$I" = b ]; then "$APB" connector call outbox send --args "{\"text\":\"try $N\"}" > "$D/call-$I-$N"; touch "$D/sent-$I-$N"; fi
if [ "$I" = c ] && [ "$N" = 1 ]; then while [ -f "$D/gate" ]; do sleep 0.05; done; fi"#,
    );
    fs::write(p.stub_dir().join("gate"), "").unwrap();
    let id = run_id_of(&p.ok(&["run", "lst", "--detach"]));
    let _guard = RunGuard::new(p.root(), &id);
    poll("item c to start", || {
        p.stub_dir().join("started-c").is_file().then_some(())
    });
    poll("b's send", || {
        p.stub_dir().join("sent-b-1").is_file().then_some(())
    });
    p.ok(&["pause", &id, "--now"]);
    let waited = p.apb(&["wait", &id, "--timeout", "60"]);
    assert_eq!(waited.status.code(), Some(4), "the run is paused");
    fs::remove_file(p.stub_dir().join("gate")).unwrap();
    let intent = p
        .events(&id)
        .iter()
        .find_map(|e| match &e.payload {
            EventPayload::SendIntentRecorded { intent, .. } => Some(intent.clone()),
            _ => None,
        })
        .expect("b recorded its send");
    p.ok(&["intent", "resolve", &id, &intent, "--sent"]);

    let resumed = p.ok(&["resume", &id]);
    assert!(resumed.contains("finished: succeeded"), "{resumed}");
    assert_eq!(p.runs_of("b"), 2, "the loop visit ran b again");
    assert_eq!(service.bodies("/send").len(), 1, "the send went out once");
    let again: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(p.stub_dir().join("call-b-2")).unwrap()).unwrap();
    assert_eq!(again["already_sent"], true);
    assert_eq!(again["intent"]["id"], intent.as_str());
}

/// Item b's `none` send loses its answer and the binding allows a resend,
/// which goes out: b passes on its send chain, while the first send stays
/// open for a person. `apb runs --items`, `apb runs --json` (the MCP
/// `run_status` view) and the item's JSON row say the earlier send is
/// unresolved; item a, which sent nothing, says nothing of the kind.
#[test]
fn a_passed_item_whose_earlier_send_is_unknown_says_so_on_its_row() {
    let service = Service::spawn(Script::from([(
        "/send",
        vec![None, Some((200, r#"{"id":"m-3"}"#))],
    )]));
    let p = Project::with_binding(
        &service.base_url,
        "[ { id: a }, { id: b } ]",
        false,
        ", on_unknown: resend",
    );
    p.stub(
        r#"if [ "$I" = b ]; then "$APB" connector call outbox send --args '{"text":"hi"}' > "$D/call-$I-1" || true; "$APB" connector call outbox send --args '{"text":"hi"}' > "$D/call-$I-2"; fi"#,
    );
    let out = p.apb(&["run", "lst"]);
    let id = run_id_of(&String::from_utf8_lossy(&out.stdout));
    assert_eq!(service.bodies("/send").len(), 2, "the send and its resend");
    assert_eq!(
        settlements(&p.events(&id), "b").last(),
        Some(&("passed".to_string(), None))
    );

    let items = p.ok(&["runs", &id, "--items"]);
    let row = |item: &str| {
        items
            .lines()
            .find(|l| l.trim_start().starts_with(&format!("{item}\t")))
            .unwrap_or_else(|| panic!("no row of {item} in: {items}"))
            .to_string()
    };
    assert_eq!(
        row("b").trim(),
        "b\tpassed\tround 0\taccept: earlier send unresolved"
    );
    assert_eq!(row("a").trim(), "a\tpassed\tround 0\taccept");

    let status: serde_json::Value = serde_json::from_str(&p.ok(&["runs", &id, "--json"])).unwrap();
    let named: Vec<(&str, bool)> = status["lists"][0]["items"]
        .as_array()
        .unwrap_or_else(|| panic!("no list items in: {status}"))
        .iter()
        .map(|i| {
            (
                i["id"].as_str().unwrap(),
                i["unresolved_send"].as_bool().unwrap_or(false),
            )
        })
        .collect();
    assert_eq!(named, [("b", true)], "the status names b for a look");
}
