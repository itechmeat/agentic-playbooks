//! `list.skip` end to end: items that passed in the previous successful
//! run are not worked again. A script step does the work, so no agent and
//! no model is ever called.
//!
//! `a_plain_list_run_journals_what_main_journals` guards the other side:
//! a list without `skip` journals exactly what main journals, compared
//! with a fixture recorded before any code of the change.

use std::fs;
use std::path::{Path, PathBuf};

use apb_engine::event::{Event, EventPayload};
use apb_engine::scheduler::{RunOptions, RunResult, run};
use apb_engine::state::RunStatus;

use crate::common;
use crate::list_run_test::{Env, events, playbook, run_list, seed, stub};

/// The fixture of a plain list run's normalized journal.
fn plain_list_fixture() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/journal/plain_list_run.jsonl")
}

/// A script list of two inline items, worked one at a time, each passing
/// its output check.
fn plain_list_playbook() -> String {
    let list = "items: { inline: [ { id: a, n: 1 }, { id: b, n: 2 } ] }\nbatch: { size: 2, parallel: 1, retries: 0 }\nitem_outputs: [blocking, id]\ncheck: { output: { field: blocking, equals: \"0\" } }";
    playbook(list, "", "").replace(
        "    type: agent_task\n    prompt: \"Handle one item of the list.\"\n",
        "    type: script\n    script: scripts/item.sh\n    runner: sh\n",
    )
}

/// The script that works one item.
const ITEM_SCRIPT: &str = "printf '{\"blocking\":\"0\",\"id\":\"%s\"}\\n' \"$APB_ITEM_ID\"\n";

/// One run's journal with what differs from run to run taken out: the
/// times, durations, process and binary facts, attempt tokens and
/// sessions, the execution root, the run id and the temporary root (as
/// `external_data_test.rs` `normalized` does, the root replaced too).
fn normalized(root: &Path, id: &str) -> Vec<String> {
    let canonical = fs::canonicalize(root).unwrap();
    let roots = [
        canonical.to_string_lossy().into_owned(),
        root.to_string_lossy().into_owned(),
    ];
    events(root, id)
        .into_iter()
        .map(|e| {
            let mut v = serde_json::to_value(&e).unwrap();
            let o = v.as_object_mut().unwrap();
            for k in [
                "ts",
                "external_data",
                "duration_ms",
                "spawn_ms",
                "execution_root",
                "apb",
                "token",
                "pid",
                "session",
            ] {
                o.remove(k);
            }
            let mut text = serde_json::to_string(&v).unwrap().replace(id, "RUN");
            for r in &roots {
                text = text.replace(r.as_str(), "ROOT");
            }
            text
        })
        .collect()
}

/// A list without `skip` journals, normalized, what main journals: no
/// skip event, no skip field on any list event, no `skipped` count in the
/// list's output. `APB_WRITE_FIXTURES=1` records the fixture instead.
#[test]
fn a_plain_list_run_journals_what_main_journals() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    seed(root, &plain_list_playbook(), &[("item.sh", ITEM_SCRIPT)]);
    let _env = Env::set(&stub(root, ""), &[]);
    let res = run_list(root);
    assert_eq!(res.outcome, RunStatus::Succeeded);
    let lines = normalized(root, &res.run_id);
    let path = plain_list_fixture();
    if std::env::var_os("APB_WRITE_FIXTURES").is_some() {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, format!("{}\n", lines.join("\n"))).unwrap();
        return;
    }
    let want = fs::read_to_string(&path).unwrap();
    let want: Vec<&str> = want.lines().collect();
    assert_eq!(lines, want, "the plain list run's journal changed");
}

// --- the skip ---

/// A script list over `items` (inline YAML) with `skip` (the block body,
/// empty for none) and `extra` lines under `list:`.
fn skip_playbook(items: &str, skip: &str, extra: &str) -> String {
    let skip = if skip.is_empty() {
        String::new()
    } else {
        format!("\nskip: {skip}")
    };
    let list = format!(
        "items: {items}\nbatch: {{ size: 2, parallel: 1, retries: 0 }}\nitem_outputs: [blocking, id]\ncheck: {{ output: {{ field: blocking, equals: \"0\" }} }}{skip}{extra}"
    );
    playbook(&list, "", "").replace(
        "    type: agent_task\n    prompt: \"Handle one item of the list.\"\n",
        "    type: script\n    script: scripts/item.sh\n    runner: sh\n",
    )
}

const SKIP: &str = "{ passed_in: previous_success }";
const TWO: &str = "{ inline: [ { id: a, n: 1 }, { id: b, n: 2 } ] }";

/// The item script: records every item it works in `worked` under the
/// project root (outside the run), then passes, or needs a look when the
/// file `bad-<item>` exists there.
fn worker(root: &Path) -> String {
    format!(
        "printf '%s\\n' \"$APB_ITEM_ID\" >> '{w}'\nb=0; if [ -f '{r}/bad-'\"$APB_ITEM_ID\" ]; then b=1; fi\nprintf '{{\"blocking\":\"%s\",\"id\":\"%s\"}}\\n' \"$b\" \"$APB_ITEM_ID\"\n",
        w = root.join("worked").display(),
        r = root.display()
    )
}

/// The items worked so far, in order, and clears the record.
fn worked(root: &Path) -> Vec<String> {
    let path = root.join("worked");
    let text = fs::read_to_string(&path).unwrap_or_default();
    let _ = fs::remove_file(&path);
    text.lines().map(str::to_string).collect()
}

/// `(item, skipped_from, skip_digest present)` of every settlement of
/// `work`.
fn settlements(events: &[Event]) -> Vec<(String, Option<String>, bool)> {
    events
        .iter()
        .filter_map(|e| match &e.payload {
            EventPayload::ListItemSettled {
                node,
                item,
                skipped_from,
                skip_digest,
                ..
            } if node == "work" => {
                Some((item.clone(), skipped_from.clone(), skip_digest.is_some()))
            }
            _ => None,
        })
        .collect()
}

fn skip_reads(events: &[Event]) -> Vec<(String, apb_engine::list::skip::SkipSet)> {
    events
        .iter()
        .filter_map(|e| match &e.payload {
            EventPayload::ListSkipRead { node, set } => Some((node.clone(), set.clone())),
            _ => None,
        })
        .collect()
}

fn skip_refused(events: &[Event]) -> Vec<Option<String>> {
    events
        .iter()
        .filter_map(|e| match &e.payload {
            EventPayload::ListStarted {
                node, skip_refused, ..
            } if node == "work" => Some(skip_refused.clone()),
            _ => None,
        })
        .collect()
}

/// `(item, state, verdict, check, note)` of one settlement.
type CheckedRow = (String, String, Option<String>, Option<String>, String);

/// [`CheckedRow`] of every settlement of `work`: which check decided each
/// item, and how.
fn checked(events: &[Event]) -> Vec<CheckedRow> {
    events
        .iter()
        .filter_map(|e| match &e.payload {
            EventPayload::ListItemSettled {
                node,
                item,
                state,
                verdict,
                check,
                note,
                ..
            } if node == "work" => Some((
                item.clone(),
                state.clone(),
                verdict.clone(),
                check.clone(),
                note.clone().unwrap_or_default(),
            )),
            _ => None,
        })
        .collect()
}

/// Creates the installation key the way an `apb` entry point does, under
/// the env lock: a test that redirects `APB_CONFIG_DIR` holds that lock, so
/// the key never lands in another test's directory. Only for a caller that
/// does not hold the lock already.
fn key() {
    let _lock = common::env_lock();
    apb_core::run_origin::ensure_key().unwrap();
}

/// Seeds `lst` as `yaml` with the worker script. The installation key
/// exists, as an `apb` entry point creates it: runs are stamped as made
/// here, the history the skip reads.
fn seeded(root: &Path, yaml: &str) {
    key();
    let script = worker(root);
    seed(root, yaml, &[("item.sh", &script)]);
}

/// Publishes `yaml` as a new version `v` of `lst` and makes it current.
fn new_version(root: &Path, v: &str, yaml: &str) {
    let src = root.join(".apb/playbooks/lst/1.0.0");
    let dst = root.join(".apb/playbooks/lst").join(v);
    fs::create_dir_all(dst.join("scripts")).unwrap();
    for e in fs::read_dir(src.join("scripts")).unwrap().flatten() {
        fs::copy(e.path(), dst.join("scripts").join(e.file_name())).unwrap();
    }
    fs::write(
        dst.join("playbook.yaml"),
        yaml.replace("version: 1.0.0", &format!("version: {v}")),
    )
    .unwrap();
    fs::write(root.join(".apb/playbooks/lst/current"), v).unwrap();
}

#[test]
fn items_that_passed_in_the_previous_success_are_skipped() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    seeded(root, &skip_playbook(TWO, SKIP, ""));
    let _env = Env::set(&stub(root, ""), &[]);
    let first = run_list(root);
    assert_eq!(first.outcome, RunStatus::Succeeded);
    assert_eq!(worked(root), ["a", "b"]);
    let ev = events(root, &first.run_id);
    let reads = skip_reads(&ev);
    assert_eq!(reads.len(), 1);
    assert_eq!(reads[0].1.source, None, "no earlier success");
    assert!(
        settlements(&ev)
            .iter()
            .all(|(_, from, stamped)| from.is_none() && *stamped)
    );

    let second = run_list(root);
    assert_eq!(second.outcome, RunStatus::Succeeded);
    let ev = events(root, &second.run_id);
    assert_eq!(
        worked(root),
        Vec::<String>::new(),
        "every item was skipped: {:?} {:?}",
        skip_reads(&ev),
        skip_refused(&ev)
    );
    assert_eq!(
        skip_reads(&ev)[0].1.source.as_deref(),
        Some(first.run_id.as_str())
    );
    let s = settlements(&ev);
    assert_eq!(s.len(), 2);
    assert!(
        s.iter()
            .all(|(_, from, stamped)| from.as_deref() == Some(first.run_id.as_str()) && *stamped),
        "{s:?}"
    );
}

#[test]
fn a_skipped_item_carries_its_stamp_to_the_next_run() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    seeded(root, &skip_playbook(TWO, SKIP, ""));
    let _env = Env::set(&stub(root, ""), &[]);
    let _first = run_list(root);
    let second = run_list(root);
    let third = run_list(root);
    assert_eq!(third.outcome, RunStatus::Succeeded);
    worked(root);
    let ev = events(root, &third.run_id);
    assert_eq!(
        skip_reads(&ev)[0].1.source.as_deref(),
        Some(second.run_id.as_str())
    );
    assert!(
        settlements(&ev)
            .iter()
            .all(|(_, from, _)| from.as_deref() == Some(second.run_id.as_str()))
    );
}

fn run_with(root: &Path, opts: RunOptions) -> RunResult {
    run(root, "lst", None, opts).unwrap()
}

/// The playbook with the list fed by an earlier script node, `feed`, which
/// prints the items held in `items.json` under the root: items that change
/// between runs of one version.
fn fed_playbook(root: &Path, skip: &str, extra: &str) -> (String, String) {
    let list = format!(
        "items: {{ from: {{ node: feed, field: items }} }}\nbatch: {{ size: 2, parallel: 1, retries: 0 }}\nitem_outputs: [blocking, id]\ncheck: {{ output: {{ field: blocking, equals: \"0\" }} }}\nskip: {skip}{extra}"
    );
    let feed = "  - { id: feed, type: script, script: scripts/feed.sh, runner: sh, outputs: { fields: [items] } }\n";
    let yaml = playbook(&list, feed, "feed").replace(
        "    type: agent_task\n    prompt: \"Handle one item of the list.\"\n",
        "    type: script\n    script: scripts/item.sh\n    runner: sh\n",
    );
    (
        yaml,
        format!("cat '{}'\n", root.join("items.json").display()),
    )
}

fn set_items(root: &Path, items: &str) {
    fs::write(root.join("items.json"), format!("{{\"items\": {items}}}")).unwrap();
}

/// Seeds the fed playbook with its two scripts.
fn seeded_fed(root: &Path, skip: &str, extra: &str) {
    key();
    seed_fed(root, skip, extra);
}

/// [`seeded_fed`] for a caller that holds the env lock already: the key is
/// created under that lock.
fn seeded_fed_locked(root: &Path, skip: &str, extra: &str) {
    apb_core::run_origin::ensure_key().unwrap();
    seed_fed(root, skip, extra);
}

fn seed_fed(root: &Path, skip: &str, extra: &str) {
    let (yaml, feed) = fed_playbook(root, skip, extra);
    let item = worker(root);
    seed(root, &yaml, &[("item.sh", &item), ("feed.sh", &feed)]);
}

#[test]
fn a_changed_item_runs_again() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    seeded_fed(root, SKIP, "");
    set_items(root, r#"[{"id":"a","n":1},{"id":"b","n":2}]"#);
    let _env = Env::set(&stub(root, ""), &[]);
    assert_eq!(run_list(root).outcome, RunStatus::Succeeded);
    assert_eq!(worked(root), ["a", "b"]);
    set_items(
        root,
        r#"[{"id":"a","n":1},{"id":"b","n":3},{"id":"c","n":4}]"#,
    );
    assert_eq!(run_list(root).outcome, RunStatus::Succeeded);
    assert_eq!(
        worked(root),
        ["b", "c"],
        "a is skipped, b changed, c is new"
    );
}

#[test]
fn a_changed_field_outside_the_list_is_still_skipped() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    seeded_fed(
        root,
        "{ passed_in: previous_success, unless_changed: [n] }",
        "",
    );
    set_items(
        root,
        r#"[{"id":"a","n":1,"seen":"mon"},{"id":"b","n":2,"seen":"mon"}]"#,
    );
    let _env = Env::set(&stub(root, ""), &[]);
    assert_eq!(run_list(root).outcome, RunStatus::Succeeded);
    worked(root);
    set_items(
        root,
        r#"[{"id":"a","n":1,"seen":"tue"},{"id":"b","n":5,"seen":"tue"}]"#,
    );
    assert_eq!(run_list(root).outcome, RunStatus::Succeeded);
    assert_eq!(worked(root), ["b"], "only the field the skip names counts");
}

/// The agent list of the fingerprint table: two inline items an agent
/// works, an output check, the skip, and two params (one secret).
fn agent_skip_playbook() -> String {
    let list = format!(
        "items: {TWO}\nbatch: {{ size: 2, parallel: 1, retries: 0 }}\nitem_outputs: [blocking]\ncheck: {{ output: {{ field: blocking, equals: \"0\" }} }}\nskip: {SKIP}"
    );
    playbook(&list, "", "").replace(
        "defaults: { profile: main }",
        "params:\n  - { name: city, type: text }\n  - { name: token, type: text, secret: true }\ndefaults: { profile: main }",
    )
}

/// The params every run of the table takes, `city` and `token` given.
fn params(city: &str, token: &str) -> RunOptions {
    RunOptions {
        params: [
            ("city".to_string(), city.to_string()),
            ("token".to_string(), token.to_string()),
        ]
        .into(),
        ..RunOptions::default()
    }
}

fn base_opts(_: &Path) -> RunOptions {
    params("Lyon", "first-token-value")
}

fn noop(_: &Path) {}

/// The agent runs of the table's two items so far.
fn agent_runs(root: &Path) -> u32 {
    crate::list_run_test::runs_of(root, "a") + crate::list_run_test::runs_of(root, "b")
}

/// Edits the definition of `lst` 1.0.0 in place: only the named part of
/// the definition changes, with no new version.
fn edit_definition(root: &Path, from: &str, to: &str) {
    let path = root.join(".apb/playbooks/lst/1.0.0/playbook.yaml");
    let yaml = fs::read_to_string(&path).unwrap();
    assert!(yaml.contains(from), "{from}");
    fs::write(&path, yaml.replace(from, to)).unwrap();
}

fn version_with(root: &Path, from: &str, to: &str) {
    let yaml = fs::read_to_string(root.join(".apb/playbooks/lst/1.0.0/playbook.yaml"))
        .unwrap()
        .replace(from, to);
    new_version(root, "1.0.1", &yaml);
}

fn cfg_dir(root: &Path) -> PathBuf {
    root.join("cfg")
}

/// Writes the machine's `config.yaml` (in the test's config dir).
fn machine_config(root: &Path, body: &str) {
    fs::create_dir_all(cfg_dir(root)).unwrap();
    fs::write(cfg_dir(root).join("config.yaml"), body).unwrap();
}

/// A copy of the stub agent at `name` under the root, its path.
fn stub_copy(root: &Path, name: &str) -> String {
    let path = root.join(name);
    fs::copy(root.join("agent.sh"), &path).unwrap();
    path.to_string_lossy().into_owned()
}

/// One row of [`every_configured_input_reruns_every_item`].
struct Row {
    name: &'static str,
    /// Run before the first run (a setup the row's input needs).
    before: fn(&Path),
    /// The one change made between the two runs.
    change: fn(&Path),
    /// The second run's options (the first run takes [`base_opts`]).
    second: fn(&Path) -> RunOptions,
    /// The agent program comes from the machine's config, not the stub
    /// override.
    configured_program: bool,
    /// Every item runs again; false: every item is skipped.
    reruns: bool,
}

/// One row per input the run is configured with (design D2 "What
/// `WorkInputs` covers"): a row changes only its input between two runs and
/// asserts that every item runs again; the control row and the agent binary
/// updated in place at the same path change no configured input, and every
/// item is skipped. The execution mode and a granted account's field are
/// compared at the work fingerprint (`scheduler::cache` tests): host mode
/// refuses list nodes at start, and an account needs a connector the agent
/// stub cannot call.
#[test]
fn every_configured_input_reruns_every_item() {
    let rows = [
        Row {
            name: "nothing",
            before: noop,
            change: noop,
            second: base_opts,
            configured_program: false,
            reruns: false,
        },
        Row {
            name: "a new version with a name-only change",
            before: noop,
            change: |root| version_with(root, "name: List", "name: List again"),
            second: base_opts,
            configured_program: false,
            reruns: true,
        },
        Row {
            name: "a defaults change",
            before: noop,
            change: |root| {
                edit_definition(
                    root,
                    "defaults: { profile: main }",
                    "defaults: { profile: main, max_parallel: 3 }",
                )
            },
            second: base_opts,
            configured_program: false,
            reruns: true,
        },
        Row {
            name: "an item check change",
            before: noop,
            change: |root| {
                edit_definition(root, "equals: \"0\"", "equals: \"1\"");
                crate::list_run_test::plan(root, "a", None, "1");
                crate::list_run_test::plan(root, "b", None, "1");
            },
            second: base_opts,
            configured_program: false,
            reruns: true,
        },
        Row {
            name: "an added inline item",
            before: noop,
            change: |root| {
                edit_definition(
                    root,
                    "{ id: b, n: 2 } ]",
                    "{ id: b, n: 2 }, { id: c, n: 3 } ]",
                )
            },
            second: base_opts,
            configured_program: false,
            reruns: true,
        },
        Row {
            name: "a helper script",
            before: |root| {
                fs::write(
                    root.join(".apb/playbooks/lst/1.0.0/scripts/helper.sh"),
                    "echo v1\n",
                )
                .unwrap()
            },
            change: |root| {
                fs::write(
                    root.join(".apb/playbooks/lst/1.0.0/scripts/helper.sh"),
                    "echo v2\n",
                )
                .unwrap()
            },
            second: base_opts,
            configured_program: false,
            reruns: true,
        },
        Row {
            name: "a param value",
            before: noop,
            change: noop,
            second: |_| params("Paris", "first-token-value"),
            configured_program: false,
            reruns: true,
        },
        Row {
            name: "a secret param value",
            before: noop,
            change: noop,
            second: |_| params("Lyon", "second-token-value"),
            configured_program: false,
            reruns: true,
        },
        Row {
            name: "the run instruction",
            before: noop,
            change: noop,
            second: |_| RunOptions {
                instruction: Some("translate each summary into French".into()),
                ..base_opts(Path::new(""))
            },
            configured_program: false,
            reruns: true,
        },
        Row {
            name: "context_max_bytes",
            before: noop,
            change: noop,
            second: |_| RunOptions {
                context_max_bytes: Some(4096),
                ..base_opts(Path::new(""))
            },
            configured_program: false,
            reruns: true,
        },
        Row {
            name: "context_compact_model",
            before: noop,
            change: noop,
            second: |_| RunOptions {
                context_compact_model: Some("other".into()),
                ..base_opts(Path::new(""))
            },
            configured_program: false,
            reruns: true,
        },
        Row {
            name: "the node overrides",
            before: |root| common::seed_profile(root, "other", "claude", "haiku", &[]),
            change: noop,
            second: |_| {
                let mut nodes = std::collections::BTreeMap::new();
                nodes.insert(
                    "work".to_string(),
                    apb_core::overrides::NodeOverride {
                        profile: Some(apb_core::profile::QualifiedProfileRef {
                            name: "other".into(),
                            scope: apb_core::profile::ProfileScope::Auto,
                        }),
                        ..Default::default()
                    },
                );
                RunOptions {
                    overrides: Some(apb_core::overrides::RunOverrides { nodes }),
                    ..base_opts(Path::new(""))
                }
            },
            configured_program: false,
            reruns: true,
        },
        Row {
            name: "the output limits",
            before: noop,
            change: |root| machine_config(root, "output_limits:\n  tail_bytes: 8192\n"),
            second: base_opts,
            configured_program: false,
            reruns: true,
        },
        Row {
            name: "the machine's agents.<id>.invocation.argv",
            before: noop,
            change: |root| {
                machine_config(
                    root,
                    "agents:\n  claude:\n    invocation:\n      argv: [\"-p\", \"--verbose\", \"--model\", \"{model}\", \"--\", \"{prompt}\"]\n",
                )
            },
            second: base_opts,
            configured_program: false,
            reruns: true,
        },
        Row {
            name: "a profile skill's content",
            before: |root| {
                fs::write(
                    root.join(".apb/profiles/main/profile.yaml"),
                    "name: main\ndescription: test\nexecutor:\n  agent: claude\n  model: haiku\nskills:\n  - cs\n",
                )
                .unwrap();
                fs::create_dir_all(root.join(".agents/skills/cs")).unwrap();
                fs::write(root.join(".agents/skills/cs/SKILL.md"), "first body").unwrap();
            },
            change: |root| {
                fs::write(root.join(".agents/skills/cs/SKILL.md"), "second body").unwrap()
            },
            second: base_opts,
            configured_program: false,
            reruns: true,
        },
        Row {
            name: "the configured program of the agent",
            before: |root| {
                let a = stub_copy(root, "prog-a.sh");
                stub_copy(root, "prog-b.sh");
                machine_config(root, &format!("agents:\n  claude:\n    program: \"{a}\"\n"));
            },
            change: |root| {
                let b = root.join("prog-b.sh");
                machine_config(
                    root,
                    &format!("agents:\n  claude:\n    program: \"{}\"\n", b.display()),
                );
            },
            second: base_opts,
            configured_program: true,
            reruns: true,
        },
        Row {
            name: "the agent binary updated in place at the same path",
            before: |root| {
                let a = stub_copy(root, "prog-a.sh");
                machine_config(root, &format!("agents:\n  claude:\n    program: \"{a}\"\n"));
            },
            change: |root| {
                let path = root.join("prog-a.sh");
                let mut body = fs::read_to_string(&path).unwrap();
                body.push_str("# updated in place\n");
                common::write_sync(&path, &body);
            },
            second: base_opts,
            configured_program: true,
            reruns: false,
        },
        Row {
            name: "a decision use",
            before: noop,
            change: |root| {
                fs::create_dir_all(cfg_dir(root)).unwrap();
                fs::write(
                    cfg_dir(root).join("decisions.yaml"),
                    "version: 1\nmode: enforce\nproviders:\n  - { id: fake, kind: fake, answers: {} }\nuses:\n  judge_node: { mode: enforce }\n",
                )
                .unwrap();
            },
            second: base_opts,
            configured_program: false,
            reruns: true,
        },
        Row {
            name: "the resolved worktree path",
            before: |root| {
                for t in ["a", "b"] {
                    fs::create_dir_all(root.join("trees").join(t)).unwrap();
                }
            },
            change: noop,
            second: |root| RunOptions {
                worktree: Some(root.join("trees/b").to_string_lossy().into_owned()),
                ..base_opts(root)
            },
            configured_program: false,
            reruns: true,
        },
    ];
    for row in rows {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let base = |root: &Path| {
            if row.name == "the resolved worktree path" {
                RunOptions {
                    worktree: Some(root.join("trees/a").to_string_lossy().into_owned()),
                    ..base_opts(root)
                }
            } else {
                base_opts(root)
            }
        };
        crate::list_run_test::seed(root, &agent_skip_playbook(), &[]);
        let prog = stub(root, "");
        let cfg = cfg_dir(root);
        fs::create_dir_all(&cfg).unwrap();
        let _env = Env::set(
            &prog,
            &[
                ("APB_CONFIG_DIR", &cfg.to_string_lossy()),
                ("APB_DECISIONS_ALLOW_FAKE", "1"),
            ],
        );
        let _cmd = row
            .configured_program
            .then(|| common::EnvVar::remove("APB_AGENT_CMD"));
        apb_core::run_origin::ensure_key().unwrap();
        (row.before)(root);
        let first = run_with(root, base(root));
        assert_eq!(
            first.outcome,
            RunStatus::Succeeded,
            "{}: the first run",
            row.name
        );
        assert_eq!(
            agent_runs(root),
            2,
            "{}: the first run works both items",
            row.name
        );
        // The control: the same input again, in this row's own setup, skips
        // every item, so skipping works here before the input changes.
        let control = run_with(root, base(root));
        let ev = events(root, &control.run_id);
        assert_eq!(
            settlements(&ev)
                .iter()
                .filter(|(_, f, _)| f.as_deref() == Some(first.run_id.as_str()))
                .count(),
            2,
            "{}: the control run skips both items: {:?} {:?}",
            row.name,
            skip_reads(&ev),
            skip_refused(&ev)
        );
        let before = agent_runs(root);
        assert_eq!(before, 2, "{}: the control worked nothing", row.name);
        // The items of the list the changed run works: the added inline
        // item's row lists a third.
        let current: &[&str] = if row.name == "an added inline item" {
            &["a", "b", "c"]
        } else {
            &["a", "b"]
        };
        let runs_before: Vec<u32> = current
            .iter()
            .map(|id| crate::list_run_test::runs_of(root, id))
            .collect();
        (row.change)(root);
        let changed = run_with(root, (row.second)(root));
        let ev = events(root, &changed.run_id);
        let skipped = settlements(&ev)
            .iter()
            .filter(|(_, from, _)| from.is_some())
            .count();
        let worked_now = agent_runs(root) - before;
        // The set came from the control run with both items, and the visit
        // recorded no refusal: the only reason left to rerun is a skip
        // digest that no longer matches.
        let reads = skip_reads(&ev);
        assert_eq!(reads.len(), 1, "{}", row.name);
        assert_eq!(
            reads[0].1.source.as_deref(),
            Some(control.run_id.as_str()),
            "{}: the source",
            row.name
        );
        assert_eq!(
            reads[0]
                .1
                .items
                .keys()
                .map(String::as_str)
                .collect::<Vec<_>>(),
            ["a", "b"],
            "{}: both items in the set",
            row.name
        );
        assert_eq!(skip_refused(&ev), vec![None], "{}: no refusal", row.name);
        if row.reruns {
            // Every item of the current list was invoked again, each by
            // its own id.
            let ran: Vec<&str> = current
                .iter()
                .zip(&runs_before)
                .filter(|(id, was)| crate::list_run_test::runs_of(root, id) > **was)
                .map(|(id, _)| *id)
                .collect();
            assert!(
                ran == current && skipped == 0,
                "{}: every item runs again (ran {ran:?} of {current:?}, {skipped} skipped)",
                row.name
            );
        } else {
            assert_eq!(
                (worked_now, skipped),
                (0, 2),
                "{}: every item is skipped",
                row.name
            );
        }
    }
}

/// Items accepted with a note while the judge use was off (no decisions
/// file: the judge is unavailable and its fallback accepts with a note)
/// carry no skip digest, since their check never ran to a verdict; the
/// machine's file then enforces the use, and the next run works every item.
#[test]
fn a_judge_turned_from_off_to_enforce_reruns_every_item() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let list = format!(
        "items: {TWO}\nbatch: {{ size: 2, parallel: 1, retries: 0 }}\nitem_outputs: [blocking]\ncheck:\n  judge:\n    state: {{ verdict: \"{{{{item_output.blocking}}}}\" }}\n    instructions: Is it plausible?\n    criteria: {{ accept: plausible, reject: not plausible }}\n    on_unavailable: accept_with_note\nskip: {SKIP}"
    );
    crate::list_run_test::seed(root, &playbook(&list, "", ""), &[]);
    let prog = stub(root, "");
    let cfg = cfg_dir(root);
    fs::create_dir_all(&cfg).unwrap();
    let _env = Env::set(
        &prog,
        &[
            ("APB_CONFIG_DIR", &cfg.to_string_lossy()),
            ("APB_DECISIONS_ALLOW_FAKE", "1"),
        ],
    );
    apb_core::run_origin::ensure_key().unwrap();
    let first = run_list(root);
    assert_eq!(first.outcome, RunStatus::Succeeded);
    let ev = events(root, &first.run_id);
    let fallback: Vec<bool> = ev
        .iter()
        .filter_map(|e| match &e.payload {
            EventPayload::ListItemSettled {
                verdict: Some(v),
                skip_digest,
                ..
            } if v == "accept_with_note" => Some(skip_digest.is_some()),
            _ => None,
        })
        .collect();
    assert_eq!(
        fallback,
        [false, false],
        "accepted by the fallback, never stamped"
    );
    let after_first = agent_runs(root);
    fs::write(
        cfg.join("decisions.yaml"),
        "version: 1\nmode: enforce\nproviders:\n  - { id: fake, kind: fake, answers: { verdict: { type: choice, choice: accept, probabilities: { accept: 0.9, accept_with_note: 0.05, reject: 0.05 }, confidence: 0.9 } } }\nuses:\n  judge_node: { mode: enforce }\n",
    )
    .unwrap();
    let second = run_list(root);
    let s = settlements(&events(root, &second.run_id));
    assert!(s.iter().all(|(_, from, _)| from.is_none()), "{s:?}");
    assert_eq!(agent_runs(root) - after_first, 2, "every item runs again");
}

/// Only the machine's `agents.<id>.program` changes, to another binary of
/// the same version and form: the recorded canonical executable differs,
/// and every item runs again.
#[test]
fn a_program_only_change_reruns_every_item() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    crate::list_run_test::seed(root, &agent_skip_playbook(), &[]);
    let prog = stub(root, "");
    let cfg = cfg_dir(root);
    fs::create_dir_all(&cfg).unwrap();
    let _env = Env::set(&prog, &[("APB_CONFIG_DIR", &cfg.to_string_lossy())]);
    let _cmd = common::EnvVar::remove("APB_AGENT_CMD");
    apb_core::run_origin::ensure_key().unwrap();
    let a = stub_copy(root, "prog-a.sh");
    let b = stub_copy(root, "prog-b.sh");
    machine_config(root, &format!("agents:\n  claude:\n    program: \"{a}\"\n"));
    let first = run_with(root, base_opts(root));
    assert_eq!(first.outcome, RunStatus::Succeeded);
    let same = run_with(root, base_opts(root));
    assert_eq!(
        settlements(&events(root, &same.run_id))
            .iter()
            .filter(|(_, f, _)| f.is_some())
            .count(),
        2,
        "the same program skips"
    );
    machine_config(root, &format!("agents:\n  claude:\n    program: \"{b}\"\n"));
    let before = agent_runs(root);
    let other = run_with(root, base_opts(root));
    assert!(
        settlements(&events(root, &other.run_id))
            .iter()
            .all(|(_, f, _)| f.is_none())
    );
    assert_eq!(agent_runs(root) - before, 2);
}
#[test]
fn an_instruction_only_change_reruns_every_item() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    seeded(root, &skip_playbook(TWO, SKIP, ""));
    let _env = Env::set(&stub(root, ""), &[]);
    let opts = |i: &str| RunOptions {
        instruction: Some(i.to_string()),
        ..RunOptions::default()
    };
    run_with(root, opts("produce summaries"));
    worked(root);
    run_with(root, opts("produce summaries"));
    assert!(worked(root).is_empty(), "the same instruction skips");
    run_with(root, opts("translate each summary into French"));
    assert_eq!(worked(root), ["a", "b"]);
}

#[test]
fn a_worktree_only_change_reruns_every_item() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    seeded(root, &skip_playbook(TWO, SKIP, ""));
    for t in ["a", "b"] {
        fs::create_dir_all(root.join("trees").join(t)).unwrap();
    }
    let _env = Env::set(&stub(root, ""), &[]);
    let opts = |t: &str| RunOptions {
        worktree: Some(root.join("trees").join(t).to_string_lossy().into_owned()),
        ..RunOptions::default()
    };
    assert_eq!(run_with(root, opts("a")).outcome, RunStatus::Succeeded);
    worked(root);
    run_with(root, opts("a"));
    assert!(worked(root).is_empty(), "the same tree skips");
    run_with(root, opts("b"));
    assert_eq!(worked(root), ["a", "b"]);
}

#[test]
fn a_visit_after_a_scripts_rewrite_skips_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    // An earlier node rewrites the run's scripts copy before the visit.
    let rewrite = "  - { id: rewrite, type: script, script: scripts/rewrite.sh, runner: sh }\n";
    let list = format!(
        "items: {TWO}\nbatch: {{ size: 2, parallel: 1, retries: 0 }}\nitem_outputs: [blocking, id]\ncheck: {{ output: {{ field: blocking, equals: \"0\" }} }}\nskip: {SKIP}"
    );
    let yaml = playbook(&list, rewrite, "rewrite").replace(
        "    type: agent_task\n    prompt: \"Handle one item of the list.\"\n",
        "    type: script\n    script: scripts/item.sh\n    runner: sh\n",
    );
    key();
    let item = worker(root);
    let marker = root.join("rewrite-on");
    let rw = format!(
        "if [ -f '{}' ]; then chmod -R u+w \"$APB_RUN_DIR/scripts\"; echo changed >> \"$APB_RUN_DIR/scripts/rewrite.sh\"; fi\n",
        marker.display()
    );
    seed(root, &yaml, &[("item.sh", &item), ("rewrite.sh", &rw)]);
    let _env = Env::set(&stub(root, ""), &[]);
    assert_eq!(run_list(root).outcome, RunStatus::Succeeded);
    worked(root);
    fs::write(&marker, "").unwrap();
    let second = run_list(root);
    assert_eq!(worked(root), ["a", "b"]);
    let ev = events(root, &second.run_id);
    let refused = skip_refused(&ev);
    assert!(
        refused[0]
            .as_deref()
            .is_some_and(|r| r.starts_with("skip_scripts_changed")),
        "{refused:?}"
    );
    assert!(
        settlements(&ev)
            .iter()
            .all(|(_, from, stamped)| from.is_none() && !stamped)
    );
}

#[test]
fn a_scripts_rewrite_mid_visit_stamps_no_digest() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    key();
    // Item a rewrites the run's scripts copy: b's settlement carries no
    // skip digest.
    let script = format!(
        "printf '%s\\n' \"$APB_ITEM_ID\" >> '{w}'\nif [ \"$APB_ITEM_ID\" = a ]; then chmod -R u+w \"$APB_RUN_DIR/scripts\"; echo '# changed' >> \"$APB_RUN_DIR/scripts/item.sh\"; fi\nprintf '{{\"blocking\":\"0\",\"id\":\"%s\"}}\\n' \"$APB_ITEM_ID\"\n",
        w = root.join("worked").display()
    );
    seed(root, &skip_playbook(TWO, SKIP, ""), &[("item.sh", &script)]);
    let _env = Env::set(&stub(root, ""), &[]);
    let res = run_list(root);
    let s = settlements(&events(root, &res.run_id));
    let stamped: Vec<(String, bool)> = s.into_iter().map(|(i, _, st)| (i, st)).collect();
    assert_eq!(
        stamped,
        [("a".to_string(), false), ("b".to_string(), false)],
        "a settles after its own rewrite too"
    );
}

#[test]
fn each_list_node_journals_its_own_set() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let yaml = skip_playbook(TWO, SKIP, "")
        .replace("  - { id: done, type: finish, outcome: success }", "  - id: again\n    type: script\n    script: scripts/item.sh\n    runner: sh\n    list:\n      items: { inline: [ { id: x } ] }\n      check: { output: { field: blocking, equals: \"0\" } }\n      item_outputs: [blocking, id]\n      skip: { passed_in: previous_success }\n  - { id: done, type: finish, outcome: success }")
        .replace("  - { from: work, to: done }", "  - { from: work, to: again }\n  - { from: again, to: done }");
    seeded(root, &yaml);
    let _env = Env::set(&stub(root, ""), &[]);
    let res = run_list(root);
    assert_eq!(res.outcome, RunStatus::Succeeded);
    let ev = events(root, &res.run_id);
    let lines: Vec<&Event> = ev
        .iter()
        .filter(|e| matches!(e.payload, EventPayload::ListSkipRead { .. }))
        .collect();
    assert_eq!(lines.len(), 2);
    // Adjacent: nothing was journaled between the two lines. That they go
    // out in one write is checked where the write is observable, in
    // `list::skip`'s `the_sets_go_out_in_one_write`.
    assert_eq!(lines[1].seq, lines[0].seq + 1, "adjacent");
    let nodes: Vec<String> = skip_reads(&ev).into_iter().map(|(n, _)| n).collect();
    assert_eq!(nodes, ["work", "again"], "definition order");
    let guard = ev.iter().rev().find_map(|e| match &e.payload {
        EventPayload::RunGuard { requires } => Some(requires.clone()),
        _ => None,
    });
    assert!(guard.is_some_and(|g| g.iter().any(|n| n == "list_skip")));
}

#[test]
fn a_goal_over_a_list_counts_skipped_items_as_done() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let yaml = skip_playbook(TWO, SKIP, "").replace(
        "defaults: { profile: main }",
        "goal:\n  statement: Every item is handled\n  criteria:\n    - { description: All handled, check: { type: list, node: work } }\ndefaults: { profile: main }",
    );
    seeded(root, &yaml);
    let _env = Env::set(&stub(root, ""), &[]);
    assert_eq!(run_list(root).outcome, RunStatus::Succeeded);
    worked(root);
    let second = run_list(root);
    assert_eq!(second.outcome, RunStatus::Succeeded);
    assert!(worked(root).is_empty());
    let goal: Vec<(String, Option<String>)> = events(root, &second.run_id)
        .iter()
        .filter_map(|e| match &e.payload {
            EventPayload::GoalChecked { status, detail, .. } => {
                Some((status.clone(), detail.clone()))
            }
            _ => None,
        })
        .collect();
    assert_eq!(goal.len(), 1, "{goal:?}");
    assert_eq!(
        goal[0].0, "passed",
        "the skipped items count as done: {goal:?}"
    );
}

#[test]
fn the_output_and_the_view_carry_the_skipped_count() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    seeded(root, &skip_playbook(TWO, SKIP, ""));
    let _env = Env::set(&stub(root, ""), &[]);
    let first = run_list(root);
    let second = run_list(root);
    let ev = events(root, &second.run_id);
    let output = ev
        .iter()
        .find_map(|e| match &e.payload {
            EventPayload::NodeFinished { node, output, .. } if node == "work" => {
                Some(output.clone())
            }
            _ => None,
        })
        .unwrap();
    let v: serde_json::Value = serde_json::from_str(&output).unwrap();
    assert_eq!(v["skipped"], 2);
    assert_eq!(v["items"][0]["skipped_from"], first.run_id.as_str());
    let pb = apb_core::schema::Playbook::from_yaml(&skip_playbook(TWO, SKIP, "")).unwrap();
    let lists = apb_engine::list::run_lists(Some(&pb), &ev);
    let skip = lists[0].skip.as_ref().expect("the skip view");
    assert_eq!((skip.skipped, skip.read_items), (2, 2));
    assert_eq!(skip.source.as_deref(), Some(first.run_id.as_str()));
    assert_eq!(
        lists[0].items[0].skipped_from.as_deref(),
        Some(first.run_id.as_str())
    );
    // The first run's output, a list with `skip` that skipped nothing, still
    // names the count.
    let ev = events(root, &first.run_id);
    let out = ev
        .iter()
        .find_map(|e| match &e.payload {
            EventPayload::NodeFinished { node, output, .. } if node == "work" => {
                Some(output.clone())
            }
            _ => None,
        })
        .unwrap();
    assert!(out.contains(r#""skipped":0"#), "{out}");
}

#[test]
fn stop_when_judges_only_the_items_worked() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    seeded_fed(
        root,
        SKIP,
        "\nstop_when: { needs_look_share: 0.5, min_settled: 1 }",
    );
    set_items(
        root,
        r#"[{"id":"a","n":1},{"id":"b","n":2},{"id":"c","n":3}]"#,
    );
    let _env = Env::set(&stub(root, ""), &[]);
    assert_eq!(run_list(root).outcome, RunStatus::Succeeded);
    worked(root);
    // d needs a look: 1 of the 2 items of the first batch worked, while 1
    // of 5 settled once the skipped ones would count.
    set_items(
        root,
        r#"[{"id":"a","n":1},{"id":"b","n":2},{"id":"c","n":3},{"id":"d","n":4},{"id":"e","n":5},{"id":"f","n":6}]"#,
    );
    fs::write(root.join("bad-d"), "").unwrap();
    let second = run_list(root);
    let ev = events(root, &second.run_id);
    let s = settlements(&ev);
    assert_eq!(
        s.iter().filter(|(_, from, _)| from.is_some()).count(),
        3,
        "{s:?}"
    );
    assert!(
        ev.iter().any(
            |e| matches!(&e.payload, EventPayload::ListStopped { node, .. } if node == "work")
        ),
        "the share of the items worked stops the visit"
    );
    assert_eq!(worked(root), ["d", "e"], "f never ran");
}

#[test]
fn a_skipped_item_is_never_in_the_pilot_slice() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    seeded_fed(root, SKIP, "\npilot: { items: 1, then: continue }");
    set_items(root, r#"[{"id":"a","n":1},{"id":"b","n":2}]"#);
    let _env = Env::set(&stub(root, ""), &[]);
    assert_eq!(run_list(root).outcome, RunStatus::Succeeded);
    worked(root);
    set_items(
        root,
        r#"[{"id":"a","n":1},{"id":"b","n":2},{"id":"c","n":3}]"#,
    );
    let second = run_list(root);
    assert_eq!(second.outcome, RunStatus::Succeeded);
    assert_eq!(worked(root), ["c"]);
    let slices: Vec<Vec<String>> = events(root, &second.run_id)
        .into_iter()
        .filter_map(|e| match e.payload {
            EventPayload::ListPilot { items, .. } => Some(items),
            _ => None,
        })
        .collect();
    assert_eq!(slices, vec![vec!["c".to_string()]]);
}

// --- recovery and refusals ---

/// The journal of `run` cut right after its `list_started` of `work`
/// (`edit` may change that line), as a driver that died there leaves it;
/// the run is then paused (nothing drives it) and resumed.
fn cut_after_list_started_and_resume(
    root: &Path,
    run: &str,
    edit: impl FnOnce(&mut serde_json::Value),
) -> RunResult {
    let rd = root.join(".apb/runs").join(run);
    let text = fs::read_to_string(rd.join("events.jsonl")).unwrap();
    let mut kept = Vec::new();
    let mut edit = Some(edit);
    for line in text.lines() {
        let mut v: serde_json::Value = serde_json::from_str(line).unwrap();
        let started = v["type"] == "list_started" && v["node"] == "work";
        if started && let Some(f) = edit.take() {
            f(&mut v);
        }
        kept.push(serde_json::to_string(&v).unwrap());
        if started {
            break;
        }
    }
    fs::write(rd.join("events.jsonl"), format!("{}\n", kept.join("\n"))).unwrap();
    resume_driverless(root, run)
}

/// Pauses a run nothing drives and resumes it, as `apb resume` does.
fn resume_driverless(root: &Path, run: &str) -> RunResult {
    use apb_engine::pause::{PauseBy, PauseMode, PauseOutcome, PauseRequest, request_pause};
    let req = PauseRequest {
        by: Some(PauseBy::Operator),
        mode: PauseMode::Drain,
        ..PauseRequest::default()
    };
    let PauseOutcome::Journaled { .. } = request_pause(root, run, &req).unwrap() else {
        panic!("a run nothing drives is paused on the spot");
    };
    apb_engine::scheduler::resume_by(root, run, None, false, PauseBy::Operator).unwrap()
}

/// Two runs: the first works every item, the second skips them. Returns
/// the second run.
fn two_runs(root: &Path) -> RunResult {
    assert_eq!(run_list(root).outcome, RunStatus::Succeeded);
    let second = run_list(root);
    assert_eq!(second.outcome, RunStatus::Succeeded);
    worked(root);
    second
}

#[test]
fn a_drive_that_died_after_list_started_skips_the_same_items() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    seeded(root, &skip_playbook(TWO, SKIP, ""));
    let _env = Env::set(&stub(root, ""), &[]);
    let second = two_runs(root);
    let res = cut_after_list_started_and_resume(root, &second.run_id, |_| {});
    assert_eq!(res.outcome, RunStatus::Succeeded);
    assert!(worked(root).is_empty(), "the recovery skips the same items");
    let s = settlements(&events(root, &second.run_id));
    assert_eq!(
        s.iter().filter(|(_, from, _)| from.is_some()).count(),
        2,
        "{s:?}"
    );
}

#[test]
fn a_crash_after_list_started_replays_the_recorded_skip_decision() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    seeded(root, &skip_playbook(TWO, SKIP, ""));
    let _env = Env::set(&stub(root, ""), &[]);
    let second = two_runs(root);
    // The visit's start recorded a refusal: the recovery reads it and works
    // every item, never deciding again.
    let res = cut_after_list_started_and_resume(root, &second.run_id, |v| {
        v["skip_refused"] = "skip_supervisor_note: recorded at the visit's start".into();
    });
    assert_eq!(res.outcome, RunStatus::Succeeded);
    assert_eq!(worked(root), ["a", "b"]);
    let s = settlements(&events(root, &second.run_id));
    assert!(
        s.iter()
            .all(|(_, from, stamped)| from.is_none() && !stamped),
        "{s:?}"
    );
}

#[test]
fn a_crash_then_scripts_rewrite_fails_the_visit() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    seeded(root, &skip_playbook(TWO, SKIP, ""));
    let _env = Env::set(&stub(root, ""), &[]);
    let second = two_runs(root);
    let rd = root.join(".apb/runs").join(&second.run_id);
    fs::write(rd.join("scripts/extra.sh"), "echo rewritten\n").unwrap();
    let res = cut_after_list_started_and_resume(root, &second.run_id, |_| {});
    let ev = events(root, &second.run_id);
    assert_ne!(res.outcome, RunStatus::Succeeded);
    assert!(
        settlements(&ev).iter().all(|(_, from, _)| from.is_none()),
        "no item is settled as skipped"
    );
    let text = fs::read_to_string(rd.join("events.jsonl")).unwrap();
    assert!(text.contains("list_skip_scripts_changed"), "{text}");
}

#[test]
fn a_supervisor_note_before_the_visit_skips_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let yaml = held_playbook(root, true, true);
    seeded_held(root, &yaml);
    let _env = Env::set(&stub(root, ""), &[]);
    let first = run_list(root);
    assert_eq!(first.outcome, RunStatus::Succeeded);
    worked(root);
    // The note is posted through the control channel while the run holds
    // before the list, as a supervisor posts it; the drive applies it
    // before the list's visit opens.
    fs::write(root.join("hold"), "").unwrap();
    let (tx, rx) = std::sync::mpsc::channel();
    {
        let r = root.to_path_buf();
        std::thread::spawn(move || {
            let _ = tx.send(run(&r, "lst", None, RunOptions::default()));
        });
    }
    let second = new_run(root, std::slice::from_ref(&first.run_id), holding);
    apb_engine::scheduler::post_supervisor_command(
        root,
        &second,
        apb_engine::control::Control::ContextAppend {
            note: "summarize in French".into(),
        },
    )
    .unwrap();
    fs::remove_file(root.join("hold")).unwrap();
    let res = rx
        .recv_timeout(std::time::Duration::from_secs(30))
        .unwrap()
        .unwrap();
    assert_eq!(res.outcome, RunStatus::Succeeded);
    assert_eq!(worked(root), ["a", "b"]);
    let ev = events(root, &second);
    let refused = skip_refused(&ev);
    assert!(
        refused
            .last()
            .unwrap()
            .as_deref()
            .is_some_and(|r| r.starts_with("skip_supervisor_note")),
        "{refused:?}"
    );
    assert!(
        settlements(&ev)
            .iter()
            .all(|(_, from, stamped)| from.is_none() && !stamped)
    );
}

#[test]
fn a_crash_before_the_set_write_records_them_at_the_next_drive() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    seeded(root, &skip_playbook(TWO, SKIP, ""));
    let _env = Env::set(&stub(root, ""), &[]);
    let second = two_runs(root);
    // The journal cut right after `run_guard`: no set recorded.
    let rd = root.join(".apb/runs").join(&second.run_id);
    let text = fs::read_to_string(rd.join("events.jsonl")).unwrap();
    let cut = text
        .lines()
        .position(|l| l.contains("\"type\":\"list_skip_read\""))
        .unwrap();
    let kept: Vec<&str> = text.lines().take(cut).collect();
    fs::write(rd.join("events.jsonl"), format!("{}\n", kept.join("\n"))).unwrap();
    let res = resume_driverless(root, &second.run_id);
    assert_eq!(res.outcome, RunStatus::Succeeded);
    let ev = events(root, &second.run_id);
    let reads = skip_reads(&ev);
    assert_eq!(reads.len(), 1, "the drive entry records the set");
    // A script-only run writes no manifest: its type cannot be told after
    // the crash, so the set is recorded refused and reads no history.
    assert!(
        reads[0]
            .1
            .refused
            .as_deref()
            .is_some_and(|r| r.starts_with("skip_run_type_unknown")),
        "{reads:?}"
    );
    assert_eq!(worked(root), ["a", "b"]);
    let read_at = ev
        .iter()
        .position(|e| matches!(e.payload, EventPayload::ListSkipRead { .. }))
        .unwrap();
    let node_at = ev
        .iter()
        .position(
            |e| matches!(&e.payload, EventPayload::NodeStarted { node, .. } if node == "start"),
        )
        .unwrap();
    assert!(read_at < node_at, "before any node runs");
}

#[test]
fn an_unreadable_earlier_run_refuses_the_start() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    seeded(root, &skip_playbook(TWO, SKIP, ""));
    let _env = Env::set(&stub(root, ""), &[]);
    let first = run_list(root);
    let rd = root.join(".apb/runs").join(&first.run_id);
    let mut text = fs::read_to_string(rd.join("events.jsonl")).unwrap();
    text.push_str("{not a record}\n");
    fs::write(rd.join("events.jsonl"), text).unwrap();
    let before = fs::read_dir(root.join(".apb/runs")).unwrap().count();
    let err = run(root, "lst", None, RunOptions::default())
        .unwrap_err()
        .to_string();
    assert!(err.contains("unreadable"), "{err}");
    assert_eq!(
        fs::read_dir(root.join(".apb/runs")).unwrap().count(),
        before,
        "no run is left behind"
    );
}

#[test]
fn a_script_list_without_a_manifest_skips() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    seeded(root, &skip_playbook(TWO, SKIP, ""));
    let _env = Env::set(&stub(root, ""), &[]);
    let second = two_runs(root);
    let rd = root.join(".apb/runs").join(&second.run_id);
    assert!(
        !rd.join("manifest.yaml").exists(),
        "a script-only run writes none"
    );
    let s = settlements(&events(root, &second.run_id));
    assert!(s.iter().all(|(_, from, _)| from.is_some()), "{s:?}");
}

#[test]
fn a_continuation_after_a_cut_between_set_lines_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    // Two skipping lists; the first fails its run (an item needs a look
    // and the list fails on it), so a continuation may follow it.
    let yaml = skip_playbook(TWO, SKIP, "\non_needs_look: fail")
        .replace("  - { id: done, type: finish, outcome: success }", "  - id: again\n    type: script\n    script: scripts/item.sh\n    runner: sh\n    list:\n      items: { inline: [ { id: x } ] }\n      check: { output: { field: blocking, equals: \"0\" } }\n      item_outputs: [blocking, id]\n      skip: { passed_in: previous_success }\n  - { id: done, type: finish, outcome: success }")
        .replace("  - { from: work, to: done }", "  - { from: work, to: again }\n  - { from: again, to: done }");
    seeded(root, &yaml);
    fs::write(root.join("bad-a"), "").unwrap();
    let _env = Env::set(&stub(root, ""), &[]);
    let pred = run_list(root);
    assert_ne!(pred.outcome, RunStatus::Succeeded);
    // Cut between the two set lines.
    let rd = root.join(".apb/runs").join(&pred.run_id);
    let text = fs::read_to_string(rd.join("events.jsonl")).unwrap();
    let kept: Vec<&str> = text
        .lines()
        .filter(|l| {
            !(l.contains("\"type\":\"list_skip_read\"") && l.contains("\"node\":\"again\""))
        })
        .collect();
    fs::write(rd.join("events.jsonl"), format!("{}\n", kept.join("\n"))).unwrap();
    let before = fs::read_dir(root.join(".apb/runs")).unwrap().count();
    let err = run(
        root,
        "lst",
        None,
        RunOptions {
            continued_from: Some(pred.run_id.clone()),
            ..RunOptions::default()
        },
    )
    .unwrap_err()
    .to_string();
    assert!(err.contains("list_skip_incomplete"), "{err}");
    assert_eq!(
        fs::read_dir(root.join(".apb/runs")).unwrap().count(),
        before
    );
}

/// The skip digests a run's settlements journal, by item.
fn digests(ev: &[Event]) -> std::collections::BTreeMap<String, String> {
    ev.iter()
        .filter_map(|e| match &e.payload {
            EventPayload::ListItemSettled {
                item,
                skip_digest: Some(d),
                ..
            } => Some((item.clone(), d.clone())),
            _ => None,
        })
        .collect()
}

/// Each case `run_params::sensitive` names (a declared secret, a
/// secret-shaped name, a credential-shaped value) changes the work: the same
/// value skips, another value reruns every item under new skip digests, and
/// the journal, digests and set included, holds neither value nor its bare
/// hash. Without an installation key nothing is compared or created.
#[test]
fn a_sensitive_param_enters_only_as_a_keyed_tag() {
    let cases = [
        (
            "token",
            "{ name: token, type: text, secret: true }",
            "first-secret-value",
            "second-secret-value",
        ),
        (
            "api_token",
            "{ name: api_token, type: text }",
            "first-named-value",
            "second-named-value",
        ),
        (
            "note",
            "{ name: note, type: text }",
            "ghp_abcdefghijklmnopqrstuvwxyz0123456789",
            "ghp_zyxwvutsrqponmlkjihgfedcba9876543210",
        ),
    ];
    for (name, decl, one, two) in cases {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let yaml = skip_playbook(TWO, SKIP, "").replace(
            "defaults: { profile: main }",
            &format!("params:\n  - {decl}\ndefaults: {{ profile: main }}"),
        );
        seeded(root, &yaml);
        let _env = Env::set(&stub(root, ""), &[]);
        let with = |t: &str| RunOptions {
            params: [(name.to_string(), t.to_string())].into(),
            ..RunOptions::default()
        };
        let first = run_with(root, with(one));
        assert_eq!(first.outcome, RunStatus::Succeeded, "{name}");
        worked(root);
        let second = run_with(root, with(one));
        assert!(worked(root).is_empty(), "{name}: the same value skips");
        let second_ev = events(root, &second.run_id);
        assert_eq!(
            skip_reads(&second_ev)[0].1.source.as_deref(),
            Some(first.run_id.as_str()),
            "{name}"
        );
        let third = run_with(root, with(two));
        assert_eq!(
            worked(root),
            ["a", "b"],
            "{name}: another value reruns every item"
        );
        let first_digests = digests(&events(root, &first.run_id));
        let third_ev = events(root, &third.run_id);
        assert_eq!(skip_refused(&third_ev), vec![None], "{name}: no refusal");
        let third_digests = digests(&third_ev);
        assert_eq!(first_digests.len(), 2, "{name}");
        for item in ["a", "b"] {
            assert_ne!(
                first_digests[item], third_digests[item],
                "{name}: {item}'s digest follows the value"
            );
        }
        for run in [&first.run_id, &second.run_id, &third.run_id] {
            let text =
                fs::read_to_string(root.join(".apb/runs").join(run).join("events.jsonl")).unwrap();
            for value in [one, two] {
                assert!(
                    !text.contains(value),
                    "{name}: the value never reaches the journal"
                );
                let hash = apb_core::content::sha256_hex(value.as_bytes());
                assert!(
                    !text.contains(hash.trim_start_matches("sha256:")),
                    "{name}: nor its bare hash"
                );
            }
        }
        if name != "token" {
            continue;
        }
        // Without an installation key the work cannot be compared: the visit
        // skips nothing and records why, and no key is created.
        let cfg = tempfile::tempdir().unwrap();
        let _cfg = crate::common::EnvVar::set("APB_CONFIG_DIR", cfg.path());
        let keyless = run_with(root, with(one));
        let refused = skip_refused(&events(root, &keyless.run_id));
        let key_files: Vec<_> = fs::read_dir(cfg.path())
            .map(|d| d.flatten().map(|e| e.file_name()).collect())
            .unwrap_or_default();
        assert!(
            refused
                .last()
                .unwrap()
                .as_deref()
                .is_some_and(|r| r.starts_with("skip_no_key")),
            "{refused:?}"
        );
        assert!(
            !key_files
                .iter()
                .any(|n| n.to_string_lossy().contains("run-origin")),
            "{key_files:?}"
        );
        worked(root);
    }
}

// --- the history read once, children, the worktree from a node ---

/// The run's tree comes from an earlier node's output, a new directory
/// each run: such a playbook never skips.
#[test]
fn a_worktree_from_a_node_output_never_skips() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let mk = "  - { id: mk, type: script, script: scripts/mk.sh, runner: sh, outputs: { fields: [working_tree] } }\n";
    let list = format!(
        "items: {TWO}\nbatch: {{ size: 2, parallel: 1, retries: 0 }}\nitem_outputs: [blocking, id]\ncheck: {{ output: {{ field: blocking, equals: \"0\" }} }}\nskip: {SKIP}"
    );
    let yaml = playbook(&list, mk, "mk")
        .replace(
            "    type: agent_task\n    prompt: \"Handle one item of the list.\"\n",
            "    type: script\n    script: scripts/item.sh\n    runner: sh\n",
        )
        .replace(
            "nodes:\n",
            "worktree: \"{{nodes.mk.output.working_tree}}\"\nnodes:\n",
        );
    key();
    let mkdir = format!(
        "d='{r}/trees/t-'$(date +%s%N 2>/dev/null || date +%s)-$$; mkdir -p \"$d\"; printf '{{\"working_tree\":\"%s\"}}\\n' \"$d\"\n",
        r = root.display()
    );
    let item = worker(root);
    seed(root, &yaml, &[("item.sh", &item), ("mk.sh", &mkdir)]);
    let _env = Env::set(&stub(root, ""), &[]);
    for n in 0..2 {
        let res = run_list(root);
        assert_eq!(res.outcome, RunStatus::Succeeded, "run {n}");
        assert_eq!(worked(root), ["a", "b"], "run {n} works every item");
        let ev = events(root, &res.run_id);
        assert!(
            ev.iter()
                .any(|e| matches!(e.payload, EventPayload::WorktreeResolved { .. }))
        );
    }
}

/// A run that recorded its window and crashed before its set write, then
/// a newer success, then the resume: the set comes from the run the
/// window names, never the newer one the history would give now.
#[test]
fn the_skip_reads_the_run_the_window_recorded() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let note = "  - { id: note, type: agent_task, prompt: \"Last {{run.previous_success.finished_at}}\", accept_untrusted: \"the window is data the prompt quotes\" }\n";
    let list = format!(
        "items: {TWO}\nbatch: {{ size: 2, parallel: 1, retries: 0 }}\nitem_outputs: [blocking, id]\ncheck: {{ output: {{ field: blocking, equals: \"0\" }} }}\nskip: {SKIP}"
    );
    let yaml = playbook(&list, note, "note").replace(
        "    type: agent_task\n    prompt: \"Handle one item of the list.\"\n",
        "    type: script\n    script: scripts/item.sh\n    runner: sh\n",
    );
    seeded(root, &yaml);
    let _env = Env::set(&stub(root, ""), &[]);
    let first = run_list(root);
    assert_eq!(first.outcome, RunStatus::Succeeded);
    let second = run_list(root);
    // The second run as a crash right after its run guard leaves it.
    let rd = root.join(".apb/runs").join(&second.run_id);
    let text = fs::read_to_string(rd.join("events.jsonl")).unwrap();
    let cut = text
        .lines()
        .position(|l| l.contains("\"type\":\"list_skip_read\""))
        .unwrap();
    let kept: Vec<&str> = text.lines().take(cut).collect();
    fs::write(rd.join("events.jsonl"), format!("{}\n", kept.join("\n"))).unwrap();
    let window = events(root, &second.run_id)
        .iter()
        .find_map(|e| match &e.payload {
            EventPayload::RunProvenance {
                previous_success: Some(ps),
                ..
            } => ps.run_id.clone(),
            _ => None,
        });
    assert_eq!(window.as_deref(), Some(first.run_id.as_str()));
    let third = run_list(root);
    assert_eq!(third.outcome, RunStatus::Succeeded, "a newer success");
    let res = resume_driverless(root, &second.run_id);
    assert_eq!(res.outcome, RunStatus::Succeeded);
    let reads = skip_reads(&events(root, &second.run_id));
    assert_eq!(reads.len(), 1);
    assert_eq!(
        reads[0].1.source.as_deref(),
        Some(first.run_id.as_str()),
        "the window's run, not {}",
        third.run_id
    );
}

/// An agent list's journal cut right after the run guard (the sets never
/// written), then a resume from a named node, and a plain resume: the
/// drive entry reads the history as the start would (the run's manifest
/// names a production run) and records the sets before any node runs.
#[test]
fn a_crash_before_the_set_write_records_them_at_a_resume_from_a_named_node() {
    for from in [Some("work"), None] {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        crate::list_run_test::seed(root, &agent_skip_playbook(), &[]);
        let _env = Env::set(&stub(root, ""), &[]);
        apb_core::run_origin::ensure_key().unwrap();
        let first = run_with(root, base_opts(root));
        assert_eq!(first.outcome, RunStatus::Succeeded);
        let second = run_with(root, base_opts(root));
        let rd = root.join(".apb/runs").join(&second.run_id);
        let text = fs::read_to_string(rd.join("events.jsonl")).unwrap();
        let cut = text
            .lines()
            .position(|l| l.contains("\"type\":\"list_skip_read\""))
            .unwrap();
        let kept: Vec<&str> = text.lines().take(cut).collect();
        fs::write(rd.join("events.jsonl"), format!("{}\n", kept.join("\n"))).unwrap();
        use apb_engine::pause::{PauseBy, PauseMode, PauseOutcome, PauseRequest, request_pause};
        let req = PauseRequest {
            by: Some(PauseBy::Operator),
            mode: PauseMode::Drain,
            ..PauseRequest::default()
        };
        let PauseOutcome::Journaled { .. } = request_pause(root, &second.run_id, &req).unwrap()
        else {
            panic!("a run nothing drives is paused on the spot");
        };
        let before = agent_runs(root);
        let res =
            apb_engine::scheduler::resume_by(root, &second.run_id, from, false, PauseBy::Operator)
                .unwrap();
        assert_eq!(res.outcome, RunStatus::Succeeded, "{from:?}");
        let ev = events(root, &second.run_id);
        let reads = skip_reads(&ev);
        assert_eq!(reads.len(), 1, "{from:?}: one set");
        assert_eq!(
            reads[0].1.source.as_deref(),
            Some(first.run_id.as_str()),
            "{from:?}"
        );
        let read_at = ev
            .iter()
            .position(|e| matches!(e.payload, EventPayload::ListSkipRead { .. }))
            .unwrap();
        let first_node = ev
            .iter()
            .enumerate()
            .skip(cut)
            .find(|(_, e)| matches!(e.payload, EventPayload::NodeStarted { .. }))
            .map(|(i, _)| i)
            .unwrap();
        assert!(read_at < first_node, "{from:?}: before any node runs");
        assert_eq!(
            agent_runs(root),
            before,
            "{from:?}: the recorded set skips the items"
        );
    }
}

/// A run paused before its list, a newer success that works a new item,
/// then the resume: the run reads its journaled set, never the history
/// again, so the new item is worked here.
#[test]
fn a_resume_keeps_the_journaled_set() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    // `hold` waits while the file `hold` exists, before the list.
    let hold = "  - { id: hold, type: script, script: scripts/hold.sh, runner: sh }\n";
    let (fed, feed) = fed_playbook(root, SKIP, "");
    let yaml = fed
        .replace(
            "  - { from: start, to: feed }",
            "  - { from: start, to: hold }\n  - { from: hold, to: feed }",
        )
        .replace(
            "nodes:\n  - { id: start, type: start }\n",
            &format!("nodes:\n  - {{ id: start, type: start }}\n{hold}"),
        );
    key();
    let hold_sh = format!(
        "while [ -f '{r}/hold' ]; do sleep 0.05; done; touch '{r}/held'\n",
        r = root.display()
    );
    let item = worker(root);
    seed(
        root,
        &yaml,
        &[
            ("item.sh", &item),
            ("feed.sh", &feed),
            ("hold.sh", &hold_sh),
        ],
    );
    set_items(root, r#"[{"id":"a","n":1},{"id":"b","n":2}]"#);
    let _env = Env::set(&stub(root, ""), &[]);
    let first = run_list(root);
    assert_eq!(first.outcome, RunStatus::Succeeded);
    worked(root);
    // The second run reads its set at start, then pauses at `hold`.
    fs::write(root.join("hold"), "").unwrap();
    let rx = {
        let (tx, rx) = std::sync::mpsc::channel();
        let r = root.to_path_buf();
        std::thread::spawn(move || {
            let _ = tx.send(run(&r, "lst", None, RunOptions::default()));
        });
        rx
    };
    let second_id = poll_run(root, &first.run_id);
    use apb_engine::pause::{PauseBy, PauseMode, PauseRequest, request_pause};
    let req = PauseRequest {
        by: Some(PauseBy::Operator),
        mode: PauseMode::Drain,
        ..PauseRequest::default()
    };
    poll_journal(root, &second_id, |ev| {
        ev.iter()
            .any(|e| matches!(&e.payload, EventPayload::NodeStarted { node, .. } if node == "hold"))
    });
    request_pause(root, &second_id, &req).unwrap();
    fs::remove_file(root.join("hold")).unwrap();
    let paused = rx
        .recv_timeout(std::time::Duration::from_secs(30))
        .unwrap()
        .unwrap();
    assert_eq!(paused.outcome, RunStatus::Paused);
    // A newer success works the new item c.
    set_items(
        root,
        r#"[{"id":"a","n":1},{"id":"b","n":2},{"id":"c","n":3}]"#,
    );
    let third = run_list(root);
    assert_eq!(third.outcome, RunStatus::Succeeded);
    assert_eq!(worked(root), ["c"]);
    let res =
        apb_engine::scheduler::resume_by(root, &second_id, None, false, PauseBy::Operator).unwrap();
    assert_eq!(res.outcome, RunStatus::Succeeded);
    let ev = events(root, &second_id);
    let reads = skip_reads(&ev);
    assert_eq!(reads.len(), 1, "read once");
    assert_eq!(reads[0].1.source.as_deref(), Some(first.run_id.as_str()));
    assert_eq!(worked(root), ["c"], "c is not in the journaled set");
}

/// The newest run directory other than `not`, once it exists.
fn poll_run(root: &Path, not: &str) -> String {
    let start = std::time::Instant::now();
    loop {
        if let Some(id) = fs::read_dir(root.join(".apb/runs")).ok().and_then(|d| {
            d.flatten()
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .filter(|n| {
                    n != not
                        && root
                            .join(".apb/runs")
                            .join(n)
                            .join("events.jsonl")
                            .is_file()
                })
                .max()
        }) {
            return id;
        }
        assert!(
            start.elapsed() < std::time::Duration::from_secs(30),
            "no new run"
        );
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
}

fn poll_journal(root: &Path, run: &str, f: impl Fn(&[Event]) -> bool) {
    let start = std::time::Instant::now();
    loop {
        if let Ok(ev) = apb_engine::event::read_all(&root.join(".apb/runs").join(run))
            && f(&ev)
        {
            return;
        }
        assert!(
            start.elapsed() < std::time::Duration::from_secs(30),
            "timed out on {run}"
        );
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
}

/// A child run reads no history: it records an empty set and works every
/// item, though the same playbook passed them as a top-level run.
#[test]
fn a_child_run_skips_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    seeded(root, &skip_playbook(TWO, SKIP, ""));
    let parent = "schema: 2\nid: parent\nname: parent\nversion: 1.0.0\nnodes:\n  - { id: s, type: start }\n  - { id: c, type: playbook, playbook: lst }\n  - { id: f, type: finish, outcome: success }\nedges:\n  - { from: s, to: c }\n  - { from: c, to: f }\n";
    let pdir = root.join(".apb/playbooks/parent/1.0.0");
    fs::create_dir_all(&pdir).unwrap();
    fs::write(pdir.join("playbook.yaml"), parent).unwrap();
    fs::write(root.join(".apb/playbooks/parent/current"), "1.0.0").unwrap();
    let _env = Env::set(&stub(root, ""), &[]);
    assert_eq!(run_list(root).outcome, RunStatus::Succeeded);
    worked(root);
    let res = run(root, "parent", None, RunOptions::default()).unwrap();
    assert_eq!(res.outcome, RunStatus::Succeeded);
    let child = events(root, &res.run_id)
        .iter()
        .find_map(|e| match &e.payload {
            EventPayload::ChildRunStarted { run_id, .. } => Some(run_id.clone()),
            _ => None,
        });
    let ev = events(root, &child.expect("a child run"));
    let reads = skip_reads(&ev);
    assert_eq!(reads.len(), 1);
    assert_eq!(
        reads[0].1,
        apb_engine::list::skip::SkipSet::default(),
        "an empty set"
    );
    assert_eq!(worked(root), ["a", "b"]);
}

/// An agent list whose run lost its manifest before the visit: the visit
/// records `skip_needs_manifest` on its `list_started`, the set itself
/// carries no refusal, and no item is settled as skipped.
#[test]
fn a_node_that_needs_a_manifest_refuses_the_skip_without_one() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    crate::list_run_test::seed(root, &agent_skip_playbook(), &[]);
    let prog = stub(root, "");
    let _env = Env::set(&prog, &[]);
    apb_core::run_origin::ensure_key().unwrap();
    assert_eq!(
        run_with(root, base_opts(root)).outcome,
        RunStatus::Succeeded
    );
    let second = run_with(root, base_opts(root));
    let rd = root.join(".apb/runs").join(&second.run_id);
    let text = fs::read_to_string(rd.join("events.jsonl")).unwrap();
    let kept: Vec<&str> = text
        .lines()
        .take_while(|l| !l.contains("\"type\":\"node_started\",\"node\":\"work\""))
        .collect();
    fs::write(rd.join("events.jsonl"), format!("{}\n", kept.join("\n"))).unwrap();
    fs::remove_file(rd.join("manifest.yaml")).unwrap();
    let _ = resume_driverless(root, &second.run_id);
    let ev = events(root, &second.run_id);
    let refused = skip_refused(&ev);
    assert!(
        refused
            .last()
            .unwrap()
            .as_deref()
            .is_some_and(|r| r.starts_with("skip_needs_manifest")),
        "{refused:?}"
    );
    assert!(
        skip_reads(&ev).iter().all(|(_, s)| s.refused.is_none()),
        "the set carries no refusal"
    );
    let visit_at = ev
        .iter()
        .rposition(|e| matches!(&e.payload, EventPayload::ListStarted { .. }))
        .unwrap();
    assert!(
        ev[visit_at..].iter().all(|e| !matches!(
            &e.payload,
            EventPayload::ListItemSettled {
                skipped_from: Some(_),
                ..
            }
        )),
        "no item is skipped"
    );
}

// --- loop visits ---

/// The fed list on a loop: `work` runs, then once more through a
/// self-loop edge, under `rerun: not_passed`.
fn seeded_fed_loop(root: &Path, extra: &str) {
    key();
    let (yaml, feed) = fed_playbook(root, SKIP, &format!("\nrerun: not_passed{extra}"));
    let yaml = yaml.replace(
        "  - { from: work, to: done }\n",
        "  - { from: work, to: work, max_traversals: 1 }\n  - { from: work, to: done, fallback: true }\n",
    );
    let item = worker(root);
    seed(root, &yaml, &[("item.sh", &item), ("feed.sh", &feed)]);
}

/// The settlements of the run's `visit`th visit of `work` (from 0).
fn visit_rows(events: &[Event], visit: usize) -> Vec<(String, bool, Option<String>, bool)> {
    let starts: Vec<usize> = events
        .iter()
        .enumerate()
        .filter(|(_, e)| matches!(&e.payload, EventPayload::ListStarted { node, reopened: false, .. } if node == "work"))
        .map(|(i, _)| i)
        .collect();
    let from = starts[visit];
    let until = starts.get(visit + 1).copied().unwrap_or(events.len());
    events[from..until]
        .iter()
        .filter_map(|e| match &e.payload {
            EventPayload::ListItemSettled {
                node,
                item,
                carried,
                skipped_from,
                skip_digest,
                ..
            } if node == "work" => Some((
                item.clone(),
                *carried,
                skipped_from.clone(),
                skip_digest.is_some(),
            )),
            _ => None,
        })
        .collect()
}

/// A loop over a list with `rerun: not_passed`: the second visit carries
/// only the rows worked and passed in the first, skips the earlier skipped
/// rows again from the recorded set, and its early-stop share counts no
/// skipped row while a carried row counts as before.
#[test]
fn the_second_loop_visit_decides_skipped_rows_again() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    seeded_fed_loop(
        root,
        "\nstop_when: { needs_look_share: 0.6, min_settled: 1 }",
    );
    set_items(root, r#"[{"id":"a","n":1},{"id":"b","n":2}]"#);
    let _env = Env::set(&stub(root, ""), &[]);
    let first = run_list(root);
    assert_eq!(first.outcome, RunStatus::Succeeded);
    worked(root);
    // c is new and passes, d needs a look in both visits.
    set_items(
        root,
        r#"[{"id":"a","n":1},{"id":"b","n":2},{"id":"c","n":3},{"id":"d","n":4}]"#,
    );
    fs::write(root.join("bad-d"), "").unwrap();
    let second = run_list(root);
    let ev = events(root, &second.run_id);
    let mut second_visit = visit_rows(&ev, 1);
    second_visit.sort();
    let from = Some(first.run_id.clone());
    assert_eq!(
        second_visit
            .iter()
            .filter(|r| r.0 != "d")
            .cloned()
            .collect::<Vec<_>>(),
        vec![
            ("a".to_string(), true, from.clone(), true),
            ("b".to_string(), true, from.clone(), true),
            ("c".to_string(), true, None, true),
        ],
        "a and b skipped again from the recorded set, c carried with the stamp it had"
    );
    assert!(
        !ev.iter()
            .any(|e| matches!(&e.payload, EventPayload::ListStopped { .. })),
        "the share counts the carried c and leaves the skipped rows out: 1 of 2"
    );
    assert_eq!(worked(root), ["c", "d", "d"]);
}

/// The journal of `run` cut right after the first visit of `work` ended
/// (its `node_finished` and the guard lines that follow it in one write),
/// with `extra` records appended.
fn cut_after_first_visit(root: &Path, run: &str, extra: &[serde_json::Value]) {
    let rd = root.join(".apb/runs").join(run);
    let text = fs::read_to_string(rd.join("events.jsonl")).unwrap();
    let lines: Vec<&str> = text.lines().collect();
    let mut at = lines
        .iter()
        .position(|l| l.contains("\"type\":\"node_finished\",\"node\":\"work\""))
        .unwrap()
        + 1;
    while at < lines.len()
        && (lines[at].contains("\"type\":\"run_guard\"")
            || lines[at].contains("\"type\":\"deferred_retry_guard\""))
    {
        at += 1;
    }
    let mut kept: Vec<String> = lines[..at].iter().map(|s| s.to_string()).collect();
    for v in extra {
        let mut v = v.clone();
        v["seq"] = kept.len().into();
        v["ts"] = 0.into();
        kept.push(v.to_string());
    }
    fs::write(rd.join("events.jsonl"), format!("{}\n", kept.join("\n"))).unwrap();
}

/// A supervisor note applied between the two visits: the second visit
/// records `skip_supervisor_note` and works the items the first skipped.
#[test]
fn a_note_between_visits_makes_the_skipped_item_work() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    seeded_fed_loop(root, "");
    set_items(root, r#"[{"id":"a","n":1},{"id":"b","n":2}]"#);
    let _env = Env::set(&stub(root, ""), &[]);
    assert_eq!(run_list(root).outcome, RunStatus::Succeeded);
    let second = run_list(root);
    worked(root);
    cut_after_first_visit(
        root,
        &second.run_id,
        &[
            serde_json::json!({"type": "supervisor_action", "action": "context_append", "detail": "summarize in French"}),
        ],
    );
    let res = resume_driverless(root, &second.run_id);
    assert_eq!(res.outcome, RunStatus::Succeeded);
    let ev = events(root, &second.run_id);
    let refused = skip_refused(&ev);
    assert_eq!(refused.len(), 2);
    assert_eq!(refused[0], None, "the first visit skipped");
    assert!(
        refused[1]
            .as_deref()
            .is_some_and(|r| r.starts_with("skip_supervisor_note")),
        "{refused:?}"
    );
    assert_eq!(
        worked(root),
        ["a", "b"],
        "the skipped items work in the second visit"
    );
    assert!(visit_rows(&ev, 1).iter().all(|r| r.2.is_none() && !r.3));
}

/// Another node rewrites the run's scripts copy after the second visit's
/// `list_started` (a drive that died there, the copy changed, the
/// resume): the pin check fails the visit before any skipped row.
#[test]
fn a_scripts_change_after_the_second_list_started_fails_before_any_skipped_row() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    seeded_fed_loop(root, "");
    set_items(root, r#"[{"id":"a","n":1},{"id":"b","n":2}]"#);
    let _env = Env::set(&stub(root, ""), &[]);
    assert_eq!(run_list(root).outcome, RunStatus::Succeeded);
    let second = run_list(root);
    worked(root);
    let rd = root.join(".apb/runs").join(&second.run_id);
    let text = fs::read_to_string(rd.join("events.jsonl")).unwrap();
    let lines: Vec<&str> = text.lines().collect();
    let starts: Vec<usize> = lines
        .iter()
        .enumerate()
        .filter(|(_, l)| l.contains("\"type\":\"list_started\",\"node\":\"work\""))
        .map(|(i, _)| i)
        .collect();
    assert_eq!(starts.len(), 2);
    fs::write(
        rd.join("events.jsonl"),
        format!("{}\n", lines[..=starts[1]].join("\n")),
    )
    .unwrap();
    fs::write(rd.join("scripts/extra.sh"), "echo rewritten\n").unwrap();
    let res = resume_driverless(root, &second.run_id);
    assert_ne!(res.outcome, RunStatus::Succeeded);
    let ev = events(root, &second.run_id);
    assert!(
        visit_rows(&ev, 1).iter().all(|r| r.2.is_none()),
        "no skipped row in the second visit"
    );
    let text = fs::read_to_string(rd.join("events.jsonl")).unwrap();
    assert!(text.contains("list_skip_scripts_changed"));
}

/// The review's interleaving: the first visit runs with changed scripts
/// and refuses skipping, so its items run and pass without a stamp; the
/// pinned scripts are restored before the second visit, which carries
/// them as before, with no stamp; the next run works them.
#[test]
fn a_carried_item_after_restored_scripts_gets_no_stamp() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    key();
    let marker = root.join("rewrite-on");
    let rewrite = format!(
        "if [ -f '{}' ]; then chmod -R u+w \"$APB_RUN_DIR/scripts\"; echo changed > \"$APB_RUN_DIR/scripts/extra.sh\"; fi\n",
        marker.display()
    );
    let restore =
        "chmod -R u+w \"$APB_RUN_DIR/scripts\"; rm -f \"$APB_RUN_DIR/scripts/extra.sh\"\n";
    let list = format!(
        "items: {TWO}\nbatch: {{ size: 2, parallel: 1, retries: 0 }}\nitem_outputs: [blocking, id]\ncheck: {{ output: {{ field: blocking, equals: \"0\" }} }}\nrerun: not_passed\nskip: {SKIP}"
    );
    let extra = "  - { id: rewrite, type: script, script: scripts/rewrite.sh, runner: sh }\n  - { id: restore, type: script, script: scripts/restore.sh, runner: sh }\n";
    let yaml = playbook(&list, extra, "rewrite")
        .replace(
            "    type: agent_task\n    prompt: \"Handle one item of the list.\"\n",
            "    type: script\n    script: scripts/item.sh\n    runner: sh\n",
        )
        .replace(
            "  - { from: work, to: done }\n",
            "  - { from: work, to: restore, max_traversals: 1 }\n  - { from: restore, to: work }\n  - { from: work, to: done, fallback: true }\n",
        );
    let item = worker(root);
    seed(
        root,
        &yaml,
        &[
            ("item.sh", &item),
            ("rewrite.sh", &rewrite),
            ("restore.sh", restore),
        ],
    );
    let _env = Env::set(&stub(root, ""), &[]);
    assert_eq!(run_list(root).outcome, RunStatus::Succeeded);
    worked(root);
    fs::write(&marker, "").unwrap();
    let second = run_list(root);
    assert_eq!(second.outcome, RunStatus::Succeeded);
    assert_eq!(worked(root), ["a", "b"], "the first visit refused skipping");
    let ev = events(root, &second.run_id);
    let refused = skip_refused(&ev);
    assert!(
        refused[0]
            .as_deref()
            .is_some_and(|r| r.starts_with("skip_scripts_changed")),
        "{refused:?}"
    );
    assert_eq!(refused[1], None, "the second visit may skip");
    let mut rows = visit_rows(&ev, 1);
    rows.sort();
    assert_eq!(
        rows,
        vec![
            ("a".to_string(), true, None, false),
            ("b".to_string(), true, None, false)
        ],
        "carried with no stamp"
    );
    fs::remove_file(&marker).unwrap();
    let third = run_list(root);
    assert_eq!(third.outcome, RunStatus::Succeeded);
    assert_eq!(
        worked(root),
        ["a", "b"],
        "nothing stamped, so the next run works them"
    );
}

// --- patches ---

/// The script list with a `hold` node, `before` the list or after it,
/// that waits while the file `hold` exists under the root.
fn held_playbook(root: &Path, before: bool, skip: bool) -> String {
    held_playbook_of(root, before, skip).replace(
        "    type: agent_task\n    prompt: \"Handle one item of the list.\"\n",
        "    type: script\n    script: scripts/item.sh\n    runner: sh\n",
    )
}

/// [`held_playbook`] with the list an agent's (the stub works it).
fn held_playbook_of(root: &Path, before: bool, skip: bool) -> String {
    let skip_line = if skip {
        format!("\nskip: {SKIP}")
    } else {
        String::new()
    };
    let list = format!(
        "items: {TWO}\nbatch: {{ size: 2, parallel: 1, retries: 0 }}\nitem_outputs: [blocking]\ncheck: {{ output: {{ field: blocking, equals: \"0\" }} }}{skip_line}"
    );
    let hold = "  - { id: hold, type: script, script: scripts/hold.sh, runner: sh }\n";
    let yaml = if before {
        playbook(&list, hold, "hold")
    } else {
        playbook(&list, "", "")
            .replace("  - { id: done,", &format!("{hold}  - {{ id: done,"))
            .replace(
                "  - { from: work, to: done }\n",
                "  - { from: work, to: hold }\n  - { from: hold, to: done }\n",
            )
    };
    let _ = root;
    yaml
}

fn seeded_held(root: &Path, yaml: &str) {
    key();
    let hold = format!(
        "while [ -f '{r}/hold' ]; do sleep 0.05; done\n",
        r = root.display()
    );
    let item = worker(root);
    seed(root, yaml, &[("item.sh", &item), ("hold.sh", &hold)]);
}

/// Starts a run of `lst` that pauses at its `hold` node; returns its id.
fn paused_at_hold(root: &Path, others: &[&str]) -> String {
    fs::write(root.join("hold"), "").unwrap();
    let (tx, rx) = std::sync::mpsc::channel();
    let r = root.to_path_buf();
    std::thread::spawn(move || {
        let _ = tx.send(run(&r, "lst", None, RunOptions::default()));
    });
    let start = std::time::Instant::now();
    let id = loop {
        let found = fs::read_dir(root.join(".apb/runs")).ok().and_then(|d| {
            d.flatten()
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .find(|n| {
                    !others.contains(&n.as_str())
                        && root
                            .join(".apb/runs")
                            .join(n)
                            .join("events.jsonl")
                            .is_file()
                })
        });
        if let Some(id) = found {
            break id;
        }
        assert!(start.elapsed() < std::time::Duration::from_secs(30));
        std::thread::sleep(std::time::Duration::from_millis(20));
    };
    poll_journal(root, &id, |ev| {
        ev.iter()
            .any(|e| matches!(&e.payload, EventPayload::NodeStarted { node, .. } if node == "hold"))
    });
    use apb_engine::pause::{PauseBy, PauseMode, PauseRequest, request_pause};
    let req = PauseRequest {
        by: Some(PauseBy::Operator),
        mode: PauseMode::Drain,
        ..PauseRequest::default()
    };
    request_pause(root, &id, &req).unwrap();
    fs::remove_file(root.join("hold")).unwrap();
    let res = rx
        .recv_timeout(std::time::Duration::from_secs(30))
        .unwrap()
        .unwrap();
    assert_eq!(res.outcome, RunStatus::Paused);
    id
}

/// Posts a patch of `lst` to `yaml` continuing from `from`, then resumes
/// the run; returns the patch version and the resume's result.
fn patch_and_resume(root: &Path, run_id: &str, yaml: &str, from: &str) -> (String, RunResult) {
    let version = apb_core::versioning::create_patch_version(
        root,
        "lst",
        "1.0.0",
        yaml,
        run_id,
        "improvement",
    )
    .unwrap();
    apb_engine::scheduler::post_supervisor_command(
        root,
        run_id,
        apb_engine::control::Control::Patch {
            version: version.clone(),
            classification: "improvement".into(),
            continue_from: from.into(),
        },
    )
    .unwrap();
    let res = apb_engine::scheduler::resume(root, run_id, None).unwrap();
    (version, res)
}

fn patch_rejected(events: &[Event]) -> Vec<String> {
    events
        .iter()
        .filter_map(|e| match &e.payload {
            EventPayload::PatchRejected { reason } => Some(reason.clone()),
            _ => None,
        })
        .collect()
}

fn patch_applied(events: &[Event]) -> bool {
    events
        .iter()
        .any(|e| matches!(e.payload, EventPayload::PatchApplied { .. }))
}

/// After skip settlements any patch, a name-only one included, is refused
/// `list_skip_work_changed`, and the run's definition snapshot is unchanged.
#[test]
fn a_patch_after_skips_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let yaml = held_playbook(root, false, true);
    seeded_held(root, &yaml);
    let _env = Env::set(&stub(root, ""), &[]);
    let first = run_list(root);
    assert_eq!(first.outcome, RunStatus::Succeeded);
    worked(root);
    let run_id = paused_at_hold(root, &[&first.run_id]);
    assert!(
        worked(root).is_empty(),
        "the items were skipped before the pause"
    );
    let snapshot = fs::read(root.join(".apb/runs").join(&run_id).join("playbook.yaml")).unwrap();
    let (_, _) = patch_and_resume(
        root,
        &run_id,
        &yaml.replace("name: List", "name: List patched"),
        "done",
    );
    let ev = events(root, &run_id);
    let rejected = patch_rejected(&ev);
    assert!(
        rejected
            .iter()
            .any(|r| r.starts_with("list_skip_work_changed")),
        "{rejected:?}"
    );
    assert!(!patch_applied(&ev));
    assert_eq!(
        fs::read(root.join(".apb/runs").join(&run_id).join("playbook.yaml")).unwrap(),
        snapshot,
        "the definition snapshot is unchanged"
    );
}

/// A patch before any skip settlement is applied as before, and the next
/// visit skips nothing: its work is the patched definition's.
#[test]
fn a_patch_before_any_skip_is_applied_and_skips_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let yaml = held_playbook(root, true, true);
    seeded_held(root, &yaml);
    let _env = Env::set(&stub(root, ""), &[]);
    let first = run_list(root);
    assert_eq!(first.outcome, RunStatus::Succeeded);
    worked(root);
    let run_id = paused_at_hold(root, &[&first.run_id]);
    let (_, res) = patch_and_resume(
        root,
        &run_id,
        &yaml.replace("name: List", "name: List patched"),
        "work",
    );
    assert_eq!(res.outcome, RunStatus::Succeeded);
    let ev = events(root, &run_id);
    assert!(patch_applied(&ev), "{:?}", patch_rejected(&ev));
    assert!(patch_rejected(&ev).is_empty());
    assert_eq!(worked(root), ["a", "b"], "the patched work skips nothing");
    assert!(
        settlements(&ev)
            .iter()
            .all(|(_, from, stamped)| from.is_none() && *stamped)
    );
}

/// The items a patched run works are stamped under the patched definition:
/// the next run of the patched version skips them.
#[test]
fn a_patched_run_stamps_the_patched_definition() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let yaml = held_playbook(root, true, true);
    seeded_held(root, &yaml);
    let _env = Env::set(&stub(root, ""), &[]);
    let run_id = paused_at_hold(root, &[]);
    let (version, res) = patch_and_resume(
        root,
        &run_id,
        &yaml.replace("name: List", "name: List patched"),
        "work",
    );
    assert_eq!(res.outcome, RunStatus::Succeeded);
    assert!(patch_applied(&events(root, &run_id)));
    assert_eq!(worked(root), ["a", "b"]);
    let next = run(root, "lst", Some(&version), RunOptions::default()).unwrap();
    assert_eq!(next.outcome, RunStatus::Succeeded);
    assert!(
        worked(root).is_empty(),
        "the patched version's run skips them"
    );
    let s = settlements(&events(root, &next.run_id));
    assert!(
        s.iter()
            .all(|(_, from, _)| from.as_deref() == Some(run_id.as_str())),
        "{s:?}"
    );
    // The unpatched version skips nothing: its work is another definition.
    let plain = run(root, "lst", Some("1.0.0"), RunOptions::default()).unwrap();
    assert_eq!(plain.outcome, RunStatus::Succeeded);
    assert_eq!(worked(root), ["a", "b"]);
}

/// A patch that adds a skip the start never read is refused
/// `list_skip_unrecorded`.
#[test]
fn a_patch_adding_a_skip_is_refused_in_a_run() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let plain = held_playbook(root, true, false);
    seeded_held(root, &plain);
    let _env = Env::set(&stub(root, ""), &[]);
    let run_id = paused_at_hold(root, &[]);
    let (_, _) = patch_and_resume(root, &run_id, &held_playbook(root, true, true), "work");
    let rejected = patch_rejected(&events(root, &run_id));
    assert!(
        rejected
            .iter()
            .any(|r| r.starts_with("list_skip_unrecorded: node work")),
        "{rejected:?}"
    );
}

// --- queued and keyed starts ---

/// The held playbook (hold before the list) with a concurrency key over
/// the param `account`; `hold` fails when the file `fail` exists.
/// The env of a keyed test: the stub agent and a config dir of its own
/// (the concurrency slots and the installation key live there).
fn keyed_env(root: &Path) -> Env {
    let cfg = cfg_dir(root);
    fs::create_dir_all(&cfg).unwrap();
    Env::set(
        &stub(root, ""),
        &[("APB_CONFIG_DIR", &cfg.to_string_lossy())],
    )
}

fn seeded_keyed(root: &Path) {
    seeded_keyed_of(root, false)
}

/// [`seeded_keyed`], the list an agent's when `agent`.
fn seeded_keyed_of(root: &Path, agent: bool) {
    // Called under `keyed_env`, which holds the env lock.
    apb_core::run_origin::ensure_key().unwrap();
    let base = if agent {
        held_playbook_of(root, true, true)
    } else {
        held_playbook(root, true, true)
    };
    let yaml = base.replace(
        "defaults: { profile: main }",
        "params:\n  - { name: account, type: text }\nconcurrency:\n  key: \"{{params.account}}\"\n  on_busy: queue_latest\ndefaults: { profile: main }",
    );
    let hold = format!(
        "while [ -f '{r}/hold' ]; do sleep 0.05; done\nif [ -f '{r}/fail' ]; then exit 1; fi\n",
        r = root.display()
    );
    let item = worker(root);
    seed(root, &yaml, &[("item.sh", &item), ("hold.sh", &hold)]);
}

fn keyed(account: &str) -> RunOptions {
    RunOptions {
        params: [("account".to_string(), account.to_string())].into(),
        ..RunOptions::default()
    }
}

fn spawn_keyed(
    root: &Path,
) -> std::sync::mpsc::Receiver<Result<RunResult, apb_engine::error::EngineError>> {
    let (tx, rx) = std::sync::mpsc::channel();
    let r = root.to_path_buf();
    std::thread::spawn(move || {
        let _ = tx.send(run(&r, "lst", None, keyed("acme")));
    });
    rx
}

/// The run of `lst` not in `known` once one exists, with a predicate on
/// its journal.
fn new_run(root: &Path, known: &[String], f: impl Fn(&[Event]) -> bool) -> String {
    let start = std::time::Instant::now();
    loop {
        if let Ok(d) = fs::read_dir(root.join(".apb/runs")) {
            for e in d.flatten() {
                let n = e.file_name().to_string_lossy().into_owned();
                if known.contains(&n) {
                    continue;
                }
                if let Ok(ev) = apb_engine::event::read_all(&e.path())
                    && f(&ev)
                {
                    return n;
                }
            }
        }
        assert!(
            start.elapsed() < std::time::Duration::from_secs(30),
            "no such run"
        );
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
}

fn queued(ev: &[Event]) -> bool {
    ev.iter().any(|e| matches!(&e.payload, EventPayload::RunProvenance { concurrency: Some(r), .. } if r.queued))
}

fn holding(ev: &[Event]) -> bool {
    ev.iter()
        .any(|e| matches!(&e.payload, EventPayload::NodeStarted { node, .. } if node == "hold"))
}

fn first_success(root: &Path) -> String {
    let first = run(root, "lst", None, keyed("acme")).unwrap();
    assert_eq!(first.outcome, RunStatus::Succeeded);
    worked(root);
    first.run_id
}

/// A run that queued for its key reads its set once, after its turn, from
/// the history as it stands then: the holder's success, never the one
/// before it.
#[test]
fn a_queued_run_reads_its_set_after_the_turn() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let _env = keyed_env(root);
    seeded_keyed(root);
    let first = first_success(root);
    fs::write(root.join("hold"), "").unwrap();
    let a_rx = spawn_keyed(root);
    let a = new_run(root, std::slice::from_ref(&first), holding);
    let b_rx = spawn_keyed(root);
    let b = new_run(root, &[first.clone(), a.clone()], queued);
    // B is recorded queued: at that point it has read nothing.
    assert!(
        skip_reads(&events(root, &b)).is_empty(),
        "a queued run reads nothing early"
    );
    assert_eq!(
        skip_reads(&events(root, &a))[0].1.source.as_deref(),
        Some(first.as_str())
    );
    fs::remove_file(root.join("hold")).unwrap();
    assert_eq!(
        a_rx.recv_timeout(std::time::Duration::from_secs(30))
            .unwrap()
            .unwrap()
            .outcome,
        RunStatus::Succeeded
    );
    let b_res = b_rx
        .recv_timeout(std::time::Duration::from_secs(30))
        .unwrap()
        .unwrap();
    assert_eq!(b_res.run_id, b);
    assert_eq!(b_res.outcome, RunStatus::Succeeded);
    let reads = skip_reads(&events(root, &b));
    assert_eq!(reads.len(), 1);
    assert_eq!(
        reads[0].1.source.as_deref(),
        Some(a.as_str()),
        "the holder's success, after the turn"
    );
}

/// A queued run whose read after its turn does not read ends failed with
/// the named reason, as a failed window lookup does.
#[test]
fn a_queued_run_whose_read_fails_ends_failed() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let _env = keyed_env(root);
    seeded_keyed(root);
    let first = first_success(root);
    fs::write(root.join("hold"), "").unwrap();
    fs::write(root.join("fail"), "").unwrap();
    let a_rx = spawn_keyed(root);
    let a = new_run(root, std::slice::from_ref(&first), holding);
    let b_rx = spawn_keyed(root);
    let b = new_run(root, &[first.clone(), a.clone()], queued);
    // The earlier success stops reading while B waits; A fails, so B's
    // read after its turn goes to it.
    let j = root.join(".apb/runs").join(&first).join("events.jsonl");
    let mut text = fs::read_to_string(&j).unwrap();
    text.push_str("{not a record}\n");
    fs::write(&j, text).unwrap();
    fs::remove_file(root.join("hold")).unwrap();
    assert_ne!(
        a_rx.recv_timeout(std::time::Duration::from_secs(30))
            .unwrap()
            .unwrap()
            .outcome,
        RunStatus::Succeeded
    );
    let b_res = b_rx
        .recv_timeout(std::time::Duration::from_secs(30))
        .unwrap();
    let ev = events(root, &b);
    let error = ev.iter().find_map(|e| match &e.payload {
        EventPayload::RunError { reason, .. } => Some(reason.clone()),
        _ => None,
    });
    assert!(
        error
            .as_deref()
            .is_some_and(|r| r.contains("list_skip_unreadable")),
        "{error:?} {b_res:?}"
    );
    assert!(ev.iter().any(
        |e| matches!(&e.payload, EventPayload::RunFinished { outcome } if outcome == "failed")
    ));
    assert!(
        !ev.iter()
            .any(|e| matches!(&e.payload, EventPayload::NodeStarted { .. })),
        "no node ran"
    );
}

/// A keyed start admitted at once whose history does not read is refused
/// and leaves no run; its slot is free for the next start.
#[test]
fn a_keyed_start_whose_read_fails_leaves_no_run() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let _env = keyed_env(root);
    seeded_keyed(root);
    // The earlier success holds another key, so the slot of `acme` is free
    // and the start is admitted at once.
    let first = run(root, "lst", None, keyed("other")).unwrap();
    assert_eq!(first.outcome, RunStatus::Succeeded);
    worked(root);
    let first = first.run_id;
    let j = root.join(".apb/runs").join(&first).join("events.jsonl");
    let good = fs::read_to_string(&j).unwrap();
    fs::write(&j, format!("{good}{{not a record}}\n")).unwrap();
    let count = || {
        fs::read_dir(root.join(".apb/runs"))
            .unwrap()
            .filter(|e| e.as_ref().unwrap().path().is_dir())
            .count()
    };
    let before = count();
    let err = run(root, "lst", None, keyed("acme"))
        .unwrap_err()
        .to_string();
    assert!(err.contains("list_skip_unreadable"), "{err}");
    assert_eq!(count(), before, "no run is left behind");
    fs::write(&j, good).unwrap();
    let next = run(root, "lst", None, keyed("acme")).unwrap();
    assert_eq!(next.outcome, RunStatus::Succeeded, "the slot was freed");
}

/// A queued run paused before its turn, then resumed after the holder
/// ended: the drive entry records every set in one write before the first
/// node. The waiter is a detached start whose driver never came (its
/// prepared run dropped); the resume goes straight to the drive.
#[test]
fn a_queued_run_paused_before_its_turn_records_its_sets_when_resumed() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let _env = keyed_env(root);
    // An agent list: the run's manifest names a production run, so the
    // drive entry reads the history (a script-only run, without a
    // manifest, fails closed there).
    seeded_keyed_of(root, true);
    let first = first_success(root);
    fs::write(root.join("hold"), "").unwrap();
    let a_rx = spawn_keyed(root);
    let a = new_run(root, std::slice::from_ref(&first), holding);
    let b = {
        let prepared =
            apb_engine::scheduler::prepare_supervised_background(root, "lst", None, keyed("acme"))
                .unwrap();
        prepared.run_id().to_string()
    };
    assert!(queued(&events(root, &b)), "B waits for the key");
    use apb_engine::pause::{PauseBy, PauseMode, PauseRequest, request_pause};
    let req = PauseRequest {
        by: Some(PauseBy::Operator),
        mode: PauseMode::Drain,
        ..PauseRequest::default()
    };
    request_pause(root, &b, &req).unwrap();
    assert!(
        skip_reads(&events(root, &b)).is_empty(),
        "paused before its turn: no set"
    );
    fs::remove_file(root.join("hold")).unwrap();
    assert_eq!(
        a_rx.recv_timeout(std::time::Duration::from_secs(30))
            .unwrap()
            .unwrap()
            .outcome,
        RunStatus::Succeeded
    );
    // A plain resume of a run that ran no node has nothing to resume from
    // (`resume.rs`, `last_node_or_err`): it is resumed from its start node.
    // The pause was posted to the waiter's driver claim, so the resumed
    // drive records the sets at its entry and then applies the pause; a
    // waiter whose driver ended is superseded by the concurrency rules,
    // which this change does not touch.
    let _ = apb_engine::scheduler::resume_by(root, &b, Some("start"), false, PauseBy::Operator)
        .unwrap();
    let ev = events(root, &b);
    let reads: Vec<&Event> = ev
        .iter()
        .filter(|e| matches!(e.payload, EventPayload::ListSkipRead { .. }))
        .collect();
    assert_eq!(reads.len(), 1);
    assert!(
        ev.iter()
            .filter(|e| matches!(e.payload, EventPayload::NodeStarted { .. }))
            .all(|e| e.seq > reads[0].seq),
        "recorded before any node"
    );
    let resumed = ev
        .iter()
        .position(|e| matches!(e.payload, EventPayload::RunResumed { .. }))
        .unwrap();
    assert!(
        matches!(ev[resumed + 1].payload, EventPayload::ListSkipRead { .. }),
        "at the drive's entry"
    );
    assert_eq!(
        skip_reads(&ev)[0].1.source.as_deref(),
        Some(a.as_str()),
        "the history as it stands at the resume"
    );
}

// --- rebinds ---

/// Through the drive: a run whose agent list skipped its items, paused
/// after it, gets a rebind of that node to another profile; the resumed
/// drive journals `RebindRejected` with `list_skip_work_changed` and keeps
/// the binding. (The single-input cases are `scheduler::rebind` tests.)
#[test]
fn a_rebind_of_a_node_with_skipped_items_is_refused_in_a_run() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    key();
    let yaml = held_playbook_of(root, false, true);
    let hold = format!(
        "while [ -f '{r}/hold' ]; do sleep 0.05; done\n",
        r = root.display()
    );
    crate::list_run_test::seed(root, &yaml, &[("hold.sh", &hold)]);
    common::seed_profile(root, "backup", "claude", "opus", &[]);
    let _env = Env::set(&stub(root, ""), &[]);
    let first = run_list(root);
    assert_eq!(first.outcome, RunStatus::Succeeded);
    let run_id = paused_at_hold(root, &[&first.run_id]);
    let skipped = settlements(&events(root, &run_id))
        .iter()
        .filter(|(_, f, _)| f.is_some())
        .count();
    assert_eq!(
        skipped,
        2,
        "the items were skipped before the pause: {:?} {:?}",
        skip_refused(&events(root, &run_id)),
        skip_reads(&events(root, &run_id))
    );
    let (_, _, bundle) = apb_core::profile_store::compute_bundle(
        root,
        apb_core::profile_store::PlaybookOrigin::Project,
        &apb_core::profile::QualifiedProfileRef {
            name: "backup".into(),
            scope: apb_core::profile::ProfileScope::Auto,
        },
    )
    .unwrap();
    apb_engine::scheduler::post_supervisor_command(
        root,
        &run_id,
        apb_engine::control::Control::Rebind {
            node: "work".into(),
            profile: "backup".into(),
            scope: apb_core::profile::ProfileScope::Auto,
            bundle,
            reason: None,
        },
    )
    .unwrap();
    let _ = apb_engine::scheduler::resume(root, &run_id, None).unwrap();
    let ev = events(root, &run_id);
    let rejected: Vec<String> = ev
        .iter()
        .filter_map(|e| match &e.payload {
            EventPayload::RebindRejected { reason, .. } => Some(reason.clone()),
            _ => None,
        })
        .collect();
    assert!(
        rejected
            .iter()
            .any(|r| r.starts_with("list_skip_work_changed: node work")),
        "{rejected:?}"
    );
    assert!(
        !ev.iter()
            .any(|e| matches!(e.payload, EventPayload::ProfileRebound { .. }))
    );
}

// --- the one stamp rule ---

/// A supervisor retry reopens a failed visit after a note was applied: the
/// items reworked under the note carry no skip digest (the visit's original
/// decision allowed skipping), so the next run works them again; an item
/// that passed before the note keeps its stamp and is skipped.
#[test]
fn a_reopen_under_a_supervisor_note_stamps_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    seeded(root, &skip_playbook(TWO, SKIP, "\non_needs_look: fail"));
    fs::write(root.join("bad-b"), "").unwrap();
    let _env = Env::set(&stub(root, ""), &[]);
    let opts = RunOptions {
        mode: apb_engine::scheduler::RunMode::Supervised,
        ..RunOptions::default()
    };
    let run_id = apb_engine::scheduler::run_background(root, "lst", None, opts).unwrap();
    poll_journal(root, &run_id, |ev| {
        ev.iter()
            .any(|e| matches!(e.payload, EventPayload::WakeRaised { .. }))
    });
    fs::remove_file(root.join("bad-b")).unwrap();
    for c in [
        apb_engine::control::Control::ContextAppend {
            note: "summarize in French".into(),
        },
        apb_engine::control::Control::Retry {
            node: "work".into(),
            prompt_override: None,
        },
    ] {
        apb_engine::scheduler::post_supervisor_command(root, &run_id, c).unwrap();
    }
    poll_journal(root, &run_id, |ev| {
        apb_engine::state::RunState::fold(ev).run_status == RunStatus::Succeeded
    });
    let ev = events(root, &run_id);
    assert!(
        ev.iter()
            .any(|e| matches!(&e.payload, EventPayload::ListStarted { reopened: true, .. })),
        "the retry reopened the visit"
    );
    let last: std::collections::BTreeMap<String, bool> = settlements(&ev)
        .into_iter()
        .map(|(i, _, stamped)| (i, stamped))
        .collect();
    assert_eq!(last.get("a"), Some(&true), "a passed before the note");
    assert_eq!(last.get("b"), Some(&false), "b was reworked under the note");
    worked(root);
    let next = run_when_free(root);
    assert_eq!(next.outcome, RunStatus::Succeeded);
    assert_eq!(worked(root), ["b"], "only the unstamped item works again");
}

/// Items worked after a revised pilot carry the reviewer's guidance in
/// their prompt: none of them is stamped, and the next run works them.
#[test]
fn a_revised_pilot_stamps_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let three = "{ inline: [ { id: a }, { id: b }, { id: c } ] }";
    seeded(root, &skip_playbook(three, SKIP, "\npilot: { items: 1 }"));
    let _env = Env::set(&stub(root, ""), &[]);
    let (tx, rx) = std::sync::mpsc::channel();
    {
        let r = root.to_path_buf();
        std::thread::spawn(move || {
            let _ = tx.send(run(&r, "lst", None, RunOptions::default()));
        });
    }
    let run_id = new_run(root, &[], |ev| {
        ev.iter().any(
            |e| matches!(&e.payload, EventPayload::ReviewRequested { node, .. } if node == "work"),
        )
    });
    let rd = root.join(".apb/runs").join(&run_id);
    let post = |decision: &str, note: &str| {
        let seq = apb_engine::event::read_all(&rd)
            .ok()
            .and_then(|e| apb_engine::review::open_request(&e, "work").map(|(s, _)| s));
        apb_engine::review::post_decision_via(
            &rd,
            apb_engine::review::ReviewCommand {
                node: "work".into(),
                decision: decision.into(),
                note: note.into(),
            },
            None,
            seq,
            apb_engine::decision_channel::DecisionChannel::Terminal,
        )
        .unwrap();
    };
    post("revise", "summarize in French");
    poll_journal(root, &run_id, |ev| {
        ev.iter().filter(|e| matches!(&e.payload, EventPayload::ReviewRequested { node, .. } if node == "work")).count() >= 2
    });
    post("continue", "");
    let res = rx
        .recv_timeout(std::time::Duration::from_secs(30))
        .unwrap()
        .unwrap();
    assert_eq!(res.outcome, RunStatus::Succeeded);
    let ev = events(root, &run_id);
    let revise_at = ev
        .iter()
        .position(|e| matches!(&e.payload, EventPayload::ReviewDecided { decision, .. } if decision == "revise"))
        .unwrap();
    let after: Vec<(String, bool)> = ev[revise_at..]
        .iter()
        .filter_map(|e| match &e.payload {
            EventPayload::ListItemSettled {
                item,
                state,
                skip_digest,
                ..
            } if state == "passed" => Some((item.clone(), skip_digest.is_some())),
            _ => None,
        })
        .collect();
    assert!(!after.is_empty());
    assert!(after.iter().all(|(_, stamped)| !stamped), "{after:?}");
    worked(root);
    // The next run stops at its own pilot gate too: answered continue.
    let (tx, rx) = std::sync::mpsc::channel();
    {
        let r = root.to_path_buf();
        std::thread::spawn(move || {
            let _ = tx.send(run(&r, "lst", None, RunOptions::default()));
        });
    }
    let next = new_run(root, std::slice::from_ref(&run_id), |ev| {
        ev.iter().any(
            |e| matches!(&e.payload, EventPayload::ReviewRequested { node, .. } if node == "work"),
        )
    });
    let nd = root.join(".apb/runs").join(&next);
    let seq = apb_engine::event::read_all(&nd)
        .ok()
        .and_then(|e| apb_engine::review::open_request(&e, "work").map(|(s, _)| s));
    apb_engine::review::post_decision_via(
        &nd,
        apb_engine::review::ReviewCommand {
            node: "work".into(),
            decision: "continue".into(),
            note: String::new(),
        },
        None,
        seq,
        apb_engine::decision_channel::DecisionChannel::Terminal,
    )
    .unwrap();
    assert_eq!(
        rx.recv_timeout(std::time::Duration::from_secs(30))
            .unwrap()
            .unwrap()
            .outcome,
        RunStatus::Succeeded
    );
    let mut w = worked(root);
    w.sort();
    assert_eq!(
        w,
        ["a", "b", "c"],
        "nothing guided was stamped, so every item works"
    );
}

/// A field `unless_changed` names that no item has (a misspelling) would
/// make every item's part constant: the visit skips nothing and says why,
/// and nothing is stamped, so a changed item is never skipped for good.
#[test]
fn an_unless_changed_field_no_item_has_skips_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    seeded_fed(
        root,
        "{ passed_in: previous_success, unless_changed: [updatd_at] }",
        "",
    );
    set_items(
        root,
        r#"[{"id":"a","updated_at":1},{"id":"b","updated_at":1}]"#,
    );
    let _env = Env::set(&stub(root, ""), &[]);
    let first = run_list(root);
    assert_eq!(worked(root), ["a", "b"]);
    let ev = events(root, &first.run_id);
    let refused = skip_refused(&ev);
    assert!(
        refused[0].as_deref().is_some_and(|r| r.starts_with("skip_unless_changed_absent") && r.contains("updatd_at")),
        "{refused:?}"
    );
    assert!(settlements(&ev).iter().all(|(_, _, stamped)| !stamped));
    set_items(
        root,
        r#"[{"id":"a","updated_at":2},{"id":"b","updated_at":1}]"#,
    );
    run_list(root);
    assert_eq!(worked(root), ["a", "b"]);
}

/// A run the recorded window names that is no longer an earlier success
/// (revived since) is judged on the very read the sets come from and gives
/// no skip: the node records why and works every item.
#[test]
fn a_window_source_no_longer_settled_gives_no_skip() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let note = "  - { id: note, type: agent_task, prompt: \"Last {{run.previous_success.finished_at}}\", accept_untrusted: \"the window is data the prompt quotes\" }\n";
    let list = format!(
        "items: {TWO}\nbatch: {{ size: 2, parallel: 1, retries: 0 }}\nitem_outputs: [blocking, id]\ncheck: {{ output: {{ field: blocking, equals: \"0\" }} }}\nskip: {SKIP}"
    );
    let yaml = playbook(&list, note, "note").replace(
        "    type: agent_task\n    prompt: \"Handle one item of the list.\"\n",
        "    type: script\n    script: scripts/item.sh\n    runner: sh\n",
    );
    seeded(root, &yaml);
    let _env = Env::set(&stub(root, ""), &[]);
    let first = run_list(root);
    let second = run_list(root);
    let rd = root.join(".apb/runs").join(&second.run_id);
    let text = fs::read_to_string(rd.join("events.jsonl")).unwrap();
    let cut = text
        .lines()
        .position(|l| l.contains("\"type\":\"list_skip_read\""))
        .unwrap();
    let kept: Vec<&str> = text.lines().take(cut).collect();
    fs::write(rd.join("events.jsonl"), format!("{}\n", kept.join("\n"))).unwrap();
    // The window's run is revived after its finish: no longer settled.
    let j1 = root
        .join(".apb/runs")
        .join(&first.run_id)
        .join("events.jsonl");
    let t1 = fs::read_to_string(&j1).unwrap();
    let next = t1.lines().count();
    fs::write(
        &j1,
        format!(
            "{t1}{}\n",
            serde_json::json!({"seq": next, "ts": 0, "type": "run_resumed", "from_node": "work"})
        ),
    )
    .unwrap();
    let res = resume_driverless(root, &second.run_id);
    assert_eq!(res.outcome, RunStatus::Succeeded);
    let ev = events(root, &second.run_id);
    let reads = skip_reads(&ev);
    assert_eq!(reads.len(), 1, "{reads:?}");
    assert!(
        reads[0]
            .1
            .refused
            .as_deref()
            .is_some_and(|r| r.starts_with("skip_source_ineligible")
                && r.contains("no longer an earlier success")),
        "{reads:?}"
    );
    assert!(reads[0].1.items.is_empty(), "nothing was read from it");
    assert!(
        settlements(&ev).iter().all(|(_, from, _)| from.is_none()),
        "every item works"
    );
}

/// A continued run whose predecessor's definition snapshot does not read
/// is refused: the predecessor's coverage cannot be checked.
#[test]
fn a_continuation_without_the_predecessors_definition_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    seeded(root, &skip_playbook(TWO, SKIP, "\non_needs_look: fail"));
    fs::write(root.join("bad-a"), "").unwrap();
    let _env = Env::set(&stub(root, ""), &[]);
    let pred = run_list(root);
    assert_ne!(pred.outcome, RunStatus::Succeeded);
    fs::write(
        root.join(".apb/runs")
            .join(&pred.run_id)
            .join("playbook.yaml"),
        "not: [a playbook",
    )
    .unwrap();
    let before = fs::read_dir(root.join(".apb/runs")).unwrap().count();
    let err = run(
        root,
        "lst",
        None,
        RunOptions {
            continued_from: Some(pred.run_id.clone()),
            ..RunOptions::default()
        },
    )
    .unwrap_err()
    .to_string();
    assert!(
        err.contains("list_skip_incomplete") && err.contains("definition snapshot does not read"),
        "{err}"
    );
    assert_eq!(
        fs::read_dir(root.join(".apb/runs")).unwrap().count(),
        before
    );
}

/// An external-data, an eval and a scenario success are never a source:
/// after each, the next production run reads the production success
/// before it.
#[test]
fn external_data_eval_and_scenario_successes_are_never_a_source() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    // An agent list: a scenario's scripted agent answers its items.
    let list = format!(
        "items: {TWO}\nbatch: {{ size: 2, parallel: 1, retries: 0 }}\nitem_outputs: [blocking]\ncheck: {{ output: {{ field: blocking, equals: \"0\" }} }}\nskip: {SKIP}"
    );
    crate::list_run_test::seed(root, &playbook(&list, "", ""), &[]);
    key();
    let _env = Env::set(&stub(root, ""), &[]);
    let source_of = |res: &RunResult| skip_reads(&events(root, &res.run_id))[0].1.source.clone();
    let first = run_list(root);
    assert_eq!(first.outcome, RunStatus::Succeeded);
    let external = run_with(
        root,
        RunOptions {
            external_data: true,
            ..RunOptions::default()
        },
    );
    assert_eq!(external.outcome, RunStatus::Succeeded);
    let after_external = run_list(root);
    assert_eq!(
        source_of(&after_external),
        Some(first.run_id.clone()),
        "not the external-data run"
    );
    let eval = run_with(
        root,
        RunOptions {
            eval: Some(apb_engine::run_config::EvalRunSettings::default()),
            ..RunOptions::default()
        },
    );
    assert_eq!(eval.outcome, RunStatus::Succeeded);
    assert_eq!(source_of(&eval), None, "an eval reads no history");
    let after_eval = run_list(root);
    assert_eq!(
        source_of(&after_eval),
        Some(after_external.run_id.clone()),
        "not the eval run"
    );
    let scenario = "schema: 1\nid: lst\nanswers:\n  work:\n    - visit: 1\n      items: { \"*\": { status: succeeded, outputs: { blocking: \"0\" } } }\n";
    let prep = apb_engine::scenario::session::ScenarioPrep {
        scenario: yaml_serde::from_str::<apb_core::scenario::Scenario>(scenario).unwrap(),
        level: apb_core::scenario::ScenarioLevel::Quick,
        scenario_digest: "sha256:s".into(),
        max_requests: 20,
        sent: Default::default(),
        external_data: false,
    };
    let quick = apb_engine::scheduler::run_scenario(root, "lst", None, &prep).unwrap();
    assert_eq!(quick.outcome, RunStatus::Succeeded);
    let after_scenario = run_list(root);
    assert_eq!(
        source_of(&after_scenario),
        Some(after_eval.run_id.clone()),
        "not the scenario run"
    );
}

/// A run of `lst` once the tree is free (a background driver that just
/// ended may still hold it).
fn run_when_free(root: &Path) -> RunResult {
    let start = std::time::Instant::now();
    loop {
        match run(root, "lst", None, RunOptions::default()) {
            Err(apb_engine::error::EngineError::WorkdirBusy(_))
                if start.elapsed() < std::time::Duration::from_secs(30) =>
            {
                std::thread::sleep(std::time::Duration::from_millis(50));
            }
            other => return other.unwrap(),
        }
    }
}

// --- fix round 2: the work is what executes ---

/// Item a settles first with its stamp; item b then writes a helper under
/// the run's scripts copy, runs it and passes its check. b's settlement is
/// checked against the pin at that settlement, never through an answer
/// kept from a's: b carries no stamp, so the next run (whose scripts copy
/// is the version's again) skips a and works b.
#[test]
fn a_scripts_rewrite_after_the_first_settlement_stamps_no_later_item() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    key();
    let script = format!(
        "printf '%s\\n' \"$APB_ITEM_ID\" >> '{w}'\nif [ \"$APB_ITEM_ID\" = b ]; then chmod -R u+w \"$APB_RUN_DIR/scripts\"; printf 'echo helper\\n' > \"$APB_RUN_DIR/scripts/helper.sh\"; sh \"$APB_RUN_DIR/scripts/helper.sh\" >/dev/null; fi\nprintf '{{\"blocking\":\"0\",\"id\":\"%s\"}}\\n' \"$APB_ITEM_ID\"\n",
        w = root.join("worked").display()
    );
    seed(root, &skip_playbook(TWO, SKIP, ""), &[("item.sh", &script)]);
    let _env = Env::set(&stub(root, ""), &[]);
    let first = run_list(root);
    assert_eq!(first.outcome, RunStatus::Succeeded);
    let stamped: Vec<(String, bool)> = settlements(&events(root, &first.run_id))
        .into_iter()
        .map(|(i, _, st)| (i, st))
        .collect();
    assert_eq!(stamped, [("a".to_string(), true), ("b".to_string(), false)]);
    worked(root);
    let next = run_list(root);
    assert_eq!(next.outcome, RunStatus::Succeeded);
    assert_eq!(
        worked(root),
        ["b"],
        "only the item that ran under other scripts works again"
    );
}

/// A wrapper runtime for the `sh` runner: it records its name, then runs
/// the script with the system shell.
fn runtime(root: &Path, name: &str) -> PathBuf {
    let path = root.join(name);
    fs::write(
        &path,
        format!(
            "#!/bin/sh\necho {name} >> '{u}'\nexec /bin/sh \"$@\"\n",
            u = root.join("runtime-used").display()
        ),
    )
    .unwrap();
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
    path
}

/// The runtime the run resolved is replaced at the same path while the
/// visit runs (item a appends to it): the settlement after the change
/// mints no skip digest and journals why once, so no stamp describes the
/// runtime the run no longer has; a's own settlement comes after it too.
#[test]
fn a_runtime_replaced_mid_visit_stamps_no_digest() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let rt = runtime(root, "runtime-a");
    machine_config(root, &format!("runners:\n  sh: ['{}']\n", rt.display()));
    let cfg = cfg_dir(root);
    let _env = Env::set(
        &stub(root, ""),
        &[("APB_CONFIG_DIR", &cfg.to_string_lossy())],
    );
    apb_core::run_origin::ensure_key().unwrap();
    let script = format!(
        "printf '%s\\n' \"$APB_ITEM_ID\" >> '{w}'\nif [ \"$APB_ITEM_ID\" = a ]; then echo '# replaced' >> '{rt}'; fi\nprintf '{{\"blocking\":\"0\",\"id\":\"%s\"}}\\n' \"$APB_ITEM_ID\"\n",
        w = root.join("worked").display(),
        rt = rt.display()
    );
    seed(root, &skip_playbook(TWO, SKIP, ""), &[("item.sh", &script)]);
    let res = run_list(root);
    assert_eq!(res.outcome, RunStatus::Succeeded);
    let ev = events(root, &res.run_id);
    let stamped: Vec<(String, bool)> = settlements(&ev)
        .into_iter()
        .map(|(i, _, st)| (i, st))
        .collect();
    assert_eq!(
        stamped,
        [("a".to_string(), false), ("b".to_string(), false)]
    );
    let latched: Vec<String> = ev
        .iter()
        .filter_map(|e| match &e.payload {
            EventPayload::ListItemSettled {
                skip_latched: Some(w),
                ..
            } => Some(w.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(latched.len(), 1, "{latched:?}");
    assert!(
        latched[0].starts_with(apb_engine::list::skip::RUNTIME_CHANGED),
        "{latched:?}"
    );
}

/// The machine's `runners.sh` moves to another runtime between two runs:
/// the runtime resolved for the run is part of its work, so every item runs
/// again, and runs under the new runtime.
#[test]
fn a_changed_runner_runtime_reruns_every_item() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let (a, b) = (runtime(root, "runtime-a"), runtime(root, "runtime-b"));
    machine_config(root, &format!("runners:\n  sh: ['{}']\n", a.display()));
    let cfg = cfg_dir(root);
    let _env = Env::set(
        &stub(root, ""),
        &[("APB_CONFIG_DIR", &cfg.to_string_lossy())],
    );
    apb_core::run_origin::ensure_key().unwrap();
    seed(
        root,
        &skip_playbook(TWO, SKIP, ""),
        &[("item.sh", &worker(root))],
    );
    let first = run_list(root);
    assert_eq!(first.outcome, RunStatus::Succeeded);
    assert_eq!(worked(root), ["a", "b"]);
    let control = run_list(root);
    assert_eq!(worked(root), Vec::<String>::new(), "the same runtime skips");
    let _ = fs::remove_file(root.join("runtime-used"));
    machine_config(root, &format!("runners:\n  sh: ['{}']\n", b.display()));
    let changed = run_list(root);
    assert_eq!(changed.outcome, RunStatus::Succeeded);
    let ev = events(root, &changed.run_id);
    assert_eq!(
        skip_reads(&ev)[0].1.source.as_deref(),
        Some(control.run_id.as_str())
    );
    assert_eq!(skip_refused(&ev), vec![None], "no refusal");
    assert_eq!(
        worked(root),
        ["a", "b"],
        "another runtime reruns every item"
    );
    let used = fs::read_to_string(root.join("runtime-used")).unwrap();
    assert!(
        used.lines().all(|l| l == "runtime-b") && !used.is_empty(),
        "{used}"
    );
}

/// Copies `from` whole to `to`, the project's `.apb` included.
fn copy_project(from: &Path, to: &Path) {
    let out = std::process::Command::new("cp")
        .arg("-a")
        .arg(format!("{}/.", from.display()))
        .arg(to)
        .output()
        .unwrap();
    assert!(out.status.success(), "{out:?}");
}

/// The project, its `.apb` included, copied to another directory on this
/// machine: the copied run still verifies as an earlier success here, and
/// is read, but its items ran in another directory, so every item works.
#[test]
fn a_copied_project_works_every_item_in_its_new_directory() {
    let a = tempfile::tempdir().unwrap();
    let b = tempfile::tempdir().unwrap();
    seeded(a.path(), &skip_playbook(TWO, SKIP, ""));
    let _env = Env::set(&stub(a.path(), ""), &[]);
    let first = run_list(a.path());
    assert_eq!(first.outcome, RunStatus::Succeeded);
    worked(a.path());
    copy_project(a.path(), b.path());
    let moved = run_list(b.path());
    assert_eq!(moved.outcome, RunStatus::Succeeded);
    let ev = events(b.path(), &moved.run_id);
    assert_eq!(
        skip_reads(&ev)[0].1.source.as_deref(),
        Some(first.run_id.as_str()),
        "the copied run is read"
    );
    assert_eq!(skip_refused(&ev), vec![None], "no refusal");
    assert!(
        settlements(&ev).iter().all(|(_, from, _)| from.is_none()),
        "every item works"
    );
    assert_eq!(worked(a.path()), ["a", "b"]);
}

/// Run A checks both items with a script; run B skips both and finishes.
/// B's outcome carries no executed item check evidence (it checked nothing),
/// and its list line says the items passed in A, not checked again.
#[test]
fn an_all_skipped_list_claims_no_executed_check() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    key();
    let yaml = skip_playbook(TWO, SKIP, "").replace(
        "check: { output: { field: blocking, equals: \"0\" } }",
        "check: { script: scripts/check.sh }",
    );
    assert!(yaml.contains("scripts/check.sh"));
    seed(
        root,
        &yaml,
        &[
            ("item.sh", &worker(root)),
            ("check.sh", "echo '{\"verdict\":\"accept\"}'\n"),
        ],
    );
    let _env = Env::set(&stub(root, ""), &[]);
    let outcome = |id: &str| {
        let d = root.join(".apb/runs").join(id);
        let view = apb_engine::run_view::RunView::load(&d, id).unwrap();
        apb_engine::run_outcome::OutcomeSummary::build(&view, &d)
    };
    let is_check = |o: &apb_engine::run_outcome::OutcomeSummary| {
        o.verified
            .iter()
            .any(|e| e.source == apb_engine::run_outcome::summary::EvidenceSource::ItemCheck)
    };
    let first = run_list(root);
    assert_eq!(first.outcome, RunStatus::Succeeded);
    assert!(is_check(&outcome(&first.run_id)), "A checked both items");
    let second = run_list(root);
    assert_eq!(second.outcome, RunStatus::Succeeded);
    assert!(
        settlements(&events(root, &second.run_id))
            .iter()
            .all(|(_, from, _)| from.is_some()),
        "B skipped both"
    );
    let o = outcome(&second.run_id);
    assert!(!is_check(&o), "{:?}", o.verified);
    let line = o.lists[0].line();
    assert!(
        line.contains(&format!(
            "2 skipped (passed in run {}, not checked again)",
            first.run_id
        )),
        "{line}"
    );
}

// --- fix round 3: skipped items carry their output; the scripts copy is read-only ---

/// `up` produces a value per item; `down` takes its items from `up`'s
/// output and records the value each one saw; `sum`, an agent step after
/// both, renders `up`'s output into its prompt.
fn chain_playbook() -> String {
    "schema: 2\nid: lst\nname: List\nversion: 1.0.0\ndefaults: { profile: main }\nnodes:\n  - { id: start, type: start }\n  - id: up\n    type: script\n    script: scripts/up.sh\n    runner: sh\n    list:\n      items: { inline: [ { id: x }, { id: y } ] }\n      item_outputs: [v]\n      skip: { passed_in: previous_success }\n  - id: down\n    type: script\n    script: scripts/down.sh\n    runner: sh\n    list:\n      items: { from: { node: up, field: items } }\n      item_outputs: [seen]\n      skip: { passed_in: previous_success }\n  - { id: sum, type: agent_task, prompt: \"Summarize {{nodes.up.output}}\" }\n  - { id: done, type: finish, outcome: success }\nedges:\n  - { from: start, to: up }\n  - { from: up, to: down }\n  - { from: down, to: sum }\n  - { from: sum, to: done }\n".to_string()
}

/// Seeds the chain: `up` prints `val-<id>` (or a value past the per-item
/// cap for the item named in `big` under the root), `down` appends
/// `<id>:<value it saw>` to `down-worked`; the agent records its argv.
fn seed_chain(root: &Path) {
    key();
    let r = root.display();
    let up = format!(
        "printf '%s\\n' \"$APB_ITEM_ID\" >> '{r}/up-worked'\nif [ \"$(cat '{r}/big' 2>/dev/null)\" = \"$APB_ITEM_ID\" ]; then v=$(head -c 3000 /dev/zero | tr '\\0' a); else v=\"val-$APB_ITEM_ID\"; fi\nprintf '{{\"v\":\"%s\"}}\\n' \"$v\"\n"
    );
    let down = format!(
        "v=$(sed -n 's/.*\"v\":\"\\([^\"]*\\)\".*/\\1/p' \"$APB_ITEM_FILE\" | cut -c1-12)\nprintf '%s:%s\\n' \"$APB_ITEM_ID\" \"$v\" >> '{r}/down-worked'\nprintf '{{\"seen\":\"%s\"}}\\n' \"$v\"\n"
    );
    seed(
        root,
        &chain_playbook(),
        &[("up.sh", &up), ("down.sh", &down)],
    );
}

fn take(root: &Path, name: &str) -> Vec<String> {
    let path = root.join(name);
    let text = fs::read_to_string(&path).unwrap_or_default();
    let _ = fs::remove_file(&path);
    text.lines().map(str::to_string).collect()
}

/// `(item, digest)` of every settlement of `node`.
fn item_digests(
    events: &[Event],
    node: &str,
) -> std::collections::BTreeMap<String, Option<String>> {
    events
        .iter()
        .filter_map(|e| match &e.payload {
            EventPayload::ListItemSettled {
                node: n,
                item,
                digest,
                ..
            } if n == node => Some((item.clone(), digest.clone())),
            _ => None,
        })
        .collect()
}

/// The review's chain over three runs: run 1 works both lists; in run 2
/// `up` skips its items and carries their outputs, so `down`'s items are the
/// ones of run 1 (same digests) and `down` skips them too; run 3 the same.
/// `down` never works an item on a missing value, and the agent after the
/// lists sees the values in every run.
#[test]
fn a_skipped_item_carries_its_output_down_the_chain() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    seed_chain(root);
    let hook = format!(
        "if [ \"$I\" = none ]; then printf '%s' \"$*\" > '{}/sum-argv'; fi",
        root.display()
    );
    let _env = Env::set(&stub(root, &hook), &[]);
    let mut runs = Vec::new();
    for n in 1..=3 {
        let res = run_list(root);
        assert_eq!(res.outcome, RunStatus::Succeeded, "run {n}");
        // Consumed after every run: each run's agent writes its own.
        let argv = take(root, "sum-argv").join("\n");
        assert!(
            argv.contains("val-x") && argv.contains("val-y"),
            "run {n}: the agent sees the outputs: {argv}"
        );
        runs.push(res.run_id);
        let down = take(root, "down-worked");
        assert!(
            down.iter().all(|l| !l.ends_with(':')),
            "run {n}: down worked on a missing value: {down:?}"
        );
        if n == 1 {
            assert_eq!(take(root, "up-worked"), ["x", "y"]);
            assert_eq!(down, ["x:val-x", "y:val-y"]);
        } else {
            assert!(take(root, "up-worked").is_empty(), "run {n}: up skips");
            assert!(down.is_empty(), "run {n}: down skips its items too");
        }
    }
    let first = item_digests(&events(root, &runs[0]), "down");
    for later in &runs[1..] {
        let ev = events(root, later);
        assert_eq!(item_digests(&ev, "down"), first, "down's items are run 1's");
        assert!(
            ev.iter().all(|e| match &e.payload {
                EventPayload::ListItemSettled {
                    skipped_from: Some(_),
                    output,
                    ..
                } => {
                    output
                        .as_deref()
                        .is_some_and(|o| o.contains("val-") || o.contains("seen"))
                }
                _ => true,
            }),
            "every skipped row carries its output"
        );
    }
}

/// An output past the per-item cap was clipped when it settled, so it
/// cannot be carried: that item is worked again, and the skip line counts
/// it; the other item is skipped with its output.
#[test]
fn an_output_over_the_cap_means_the_item_is_worked() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    seed_chain(root);
    fs::write(root.join("big"), "y").unwrap();
    let _env = Env::set(&stub(root, ""), &[]);
    let first = run_list(root);
    assert_eq!(first.outcome, RunStatus::Succeeded);
    take(root, "up-worked");
    let second = run_list(root);
    assert_eq!(second.outcome, RunStatus::Succeeded);
    assert_eq!(take(root, "up-worked"), ["y"], "the clipped item is worked");
    let ev = events(root, &second.run_id);
    let set = skip_reads(&ev)
        .into_iter()
        .find(|(n, _)| n == "up")
        .unwrap()
        .1;
    assert_eq!(set.not_inherited, 1, "{set:?}");
    assert!(set.items.contains_key("x") && !set.items.contains_key("y"));
    let d = root.join(".apb/runs").join(&second.run_id);
    let view = apb_engine::run_view::RunView::load(&d, &second.run_id).unwrap();
    let line = apb_engine::run_outcome::OutcomeSummary::build(&view, &d)
        .lists
        .iter()
        .find(|l| l.node == "up")
        .unwrap()
        .line();
    assert!(
        line.contains("1 not skipped (the output they passed with could not be carried)"),
        "{line}"
    );
}

fn is_root() -> bool {
    std::process::Command::new("id")
        .arg("-u")
        .output()
        .is_ok_and(|o| String::from_utf8_lossy(&o.stdout).trim() == "0")
}

/// In a run that may skip, the scripts copy is read-only while it runs: a
/// script that writes next to itself fails on its own write (loudly, the
/// item needs a look) instead of quietly turning stamping off; nothing
/// latches. After the run ends the copy has its write bits again. Root is
/// not stopped by the mode, so the refusal is not asserted as root.
#[test]
fn a_script_writing_next_to_itself_fails_on_its_own_write() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    key();
    let r = root.display();
    let script = format!(
        "if [ -w \"$APB_RUN_DIR/scripts/item.sh\" ]; then echo writable >> '{r}/modes'; else echo read-only >> '{r}/modes'; fi\nif ! ( echo x > \"$APB_RUN_DIR/scripts/scratch\" ) 2>/dev/null; then echo refused >> '{r}/refused'; exit 1; fi\nprintf '{{\"blocking\":\"0\",\"id\":\"%s\"}}\\n' \"$APB_ITEM_ID\"\n"
    );
    seed(root, &skip_playbook(TWO, SKIP, ""), &[("item.sh", &script)]);
    let _env = Env::set(&stub(root, ""), &[]);
    let res = run_list(root);
    let ev = events(root, &res.run_id);
    assert!(
        ev.iter().all(|e| !matches!(
            &e.payload,
            EventPayload::ListItemSettled {
                skip_latched: Some(_),
                ..
            }
        )),
        "nothing latches"
    );
    if !is_root() {
        assert_eq!(take(root, "modes"), ["read-only", "read-only"]);
        assert_eq!(
            take(root, "refused"),
            ["refused", "refused"],
            "each write fails on its own"
        );
        assert!(settlements(&ev).iter().all(|(_, _, stamped)| !stamped));
    }
    let item = root
        .join(".apb/runs")
        .join(&res.run_id)
        .join("scripts/item.sh");
    use std::os::unix::fs::PermissionsExt;
    assert_ne!(
        fs::metadata(&item).unwrap().permissions().mode() & 0o200,
        0,
        "the ended run gave the bits back"
    );
}

/// A run without `skip` is untouched: its scripts copy keeps its modes while
/// it runs.
#[test]
fn a_run_without_skip_keeps_its_scripts_writable() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    key();
    let r = root.display();
    let script = format!(
        "if [ -w \"$APB_RUN_DIR/scripts/item.sh\" ] && [ -w \"$APB_RUN_DIR/scripts\" ]; then echo writable >> '{r}/modes'; else echo read-only >> '{r}/modes'; fi\nprintf '{{\"blocking\":\"0\",\"id\":\"%s\"}}\\n' \"$APB_ITEM_ID\"\n"
    );
    seed(root, &skip_playbook(TWO, "", ""), &[("item.sh", &script)]);
    let _env = Env::set(&stub(root, ""), &[]);
    let res = run_list(root);
    assert_eq!(res.outcome, RunStatus::Succeeded);
    assert_eq!(take(root, "modes"), ["writable", "writable"]);
}

/// A deliberate change of the copy (its mode changed first, then a file
/// added) after an item settled is seen at the next item's settlement:
/// stamping stops, and the reason is journaled once and shown on the list
/// line every surface reads.
#[test]
fn a_latch_is_journaled_once_and_shown() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    key();
    let three = "{ inline: [ { id: a }, { id: b }, { id: c } ] }";
    let script = format!(
        "printf '%s\\n' \"$APB_ITEM_ID\" >> '{w}'\nif [ \"$APB_ITEM_ID\" = b ]; then chmod -R u+w \"$APB_RUN_DIR/scripts\"; echo x > \"$APB_RUN_DIR/scripts/helper.sh\"; fi\nprintf '{{\"blocking\":\"0\",\"id\":\"%s\"}}\\n' \"$APB_ITEM_ID\"\n",
        w = root.join("worked").display()
    );
    seed(
        root,
        &skip_playbook(three, SKIP, ""),
        &[("item.sh", &script)],
    );
    let _env = Env::set(&stub(root, ""), &[]);
    let res = run_list(root);
    let ev = events(root, &res.run_id);
    let latched: Vec<String> = ev
        .iter()
        .filter_map(|e| match &e.payload {
            EventPayload::ListItemSettled {
                skip_latched: Some(w),
                ..
            } => Some(w.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(latched.len(), 1, "once per visit: {latched:?}");
    assert!(latched[0].starts_with("skip_scripts_diverged"));
    let d = root.join(".apb/runs").join(&res.run_id);
    let view = apb_engine::run_view::RunView::load(&d, &res.run_id).unwrap();
    let line = apb_engine::run_outcome::OutcomeSummary::build(&view, &d).lists[0].line();
    assert!(
        line.contains("stamping stopped: skip_scripts_diverged"),
        "{line}"
    );
}

/// An external-data run reads the history like any run: when it crashed
/// between its run guard and its set write, the drive entry reads the
/// history as preparation would, never an empty set.
#[test]
fn an_external_data_run_that_crashed_before_its_set_write_reads_the_history() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    crate::list_run_test::seed(root, &agent_skip_playbook(), &[]);
    let cfg = cfg_dir(root);
    fs::create_dir_all(&cfg).unwrap();
    let _env = Env::set(
        &stub(root, ""),
        &[
            ("APB_CONFIG_DIR", &cfg.to_string_lossy()),
            ("APB_DECISIONS_ALLOW_FAKE", "1"),
        ],
    );
    apb_core::run_origin::ensure_key().unwrap();
    let first = run_with(root, base_opts(root));
    assert_eq!(first.outcome, RunStatus::Succeeded);
    let second = run_with(
        root,
        RunOptions {
            external_data: true,
            ..base_opts(root)
        },
    );
    let rd = root.join(".apb/runs").join(&second.run_id);
    let text = fs::read_to_string(rd.join("events.jsonl")).unwrap();
    let cut = text
        .lines()
        .position(|l| l.contains("\"type\":\"list_skip_read\""))
        .unwrap();
    let kept: Vec<&str> = text.lines().take(cut).collect();
    fs::write(rd.join("events.jsonl"), format!("{}\n", kept.join("\n"))).unwrap();
    let _ = resume_driverless(root, &second.run_id);
    let reads = skip_reads(&events(root, &second.run_id));
    assert_eq!(reads.len(), 1);
    assert_eq!(
        reads[0].1.source.as_deref(),
        Some(first.run_id.as_str()),
        "{reads:?}"
    );
}

/// A queued external-data run paused before its turn, then resumed after
/// the holder ended: its drive entry reads the history as a production
/// run's would (the holder is its source), never an empty set.
#[test]
fn a_queued_external_data_run_resumed_reads_the_history() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let _env = keyed_env(root);
    seeded_keyed_of(root, true);
    let first = first_success(root);
    fs::write(root.join("hold"), "").unwrap();
    let a_rx = spawn_keyed(root);
    let a = new_run(root, std::slice::from_ref(&first), holding);
    let b = {
        let opts = RunOptions {
            external_data: true,
            ..keyed("acme")
        };
        let prepared =
            apb_engine::scheduler::prepare_supervised_background(root, "lst", None, opts).unwrap();
        prepared.run_id().to_string()
    };
    assert!(queued(&events(root, &b)), "B waits for the key");
    use apb_engine::pause::{PauseBy, PauseMode, PauseRequest, request_pause};
    let req = PauseRequest {
        by: Some(PauseBy::Operator),
        mode: PauseMode::Drain,
        ..PauseRequest::default()
    };
    request_pause(root, &b, &req).unwrap();
    fs::remove_file(root.join("hold")).unwrap();
    assert_eq!(
        a_rx.recv_timeout(std::time::Duration::from_secs(30))
            .unwrap()
            .unwrap()
            .outcome,
        RunStatus::Succeeded
    );
    let _ = apb_engine::scheduler::resume_by(root, &b, Some("start"), false, PauseBy::Operator)
        .unwrap();
    let reads = skip_reads(&events(root, &b));
    assert_eq!(reads.len(), 1);
    assert_eq!(reads[0].1.source.as_deref(), Some(a.as_str()), "{reads:?}");
}

// --- fix round 3 follow-up: the read-only copy never outlives the drive ---

/// The directories under `root` that lack the owner write bit.
fn read_only_dirs(root: &Path) -> usize {
    use std::os::unix::fs::PermissionsExt;
    fn walk(p: &Path, n: &mut usize) {
        let Ok(m) = fs::symlink_metadata(p) else {
            return;
        };
        if !m.is_dir() {
            return;
        }
        if m.permissions().mode() & 0o200 == 0 {
            *n += 1;
        }
        if let Ok(d) = fs::read_dir(p) {
            for e in d.flatten() {
                walk(&e.path(), n);
            }
        }
    }
    let mut n = 0;
    walk(root, &mut n);
    n
}

/// A skipping list after the `hold` node, whose items record whether the
/// scripts copy is writable while they run.
fn seeded_held_probe(root: &Path) {
    key();
    let r = root.display();
    let hold = format!("while [ -f '{r}/hold' ]; do sleep 0.05; done\n");
    let item = format!(
        "if [ -w \"$APB_RUN_DIR/scripts\" ]; then echo writable >> '{r}/modes'; else echo read-only >> '{r}/modes'; fi\nprintf '{{\"blocking\":\"0\",\"id\":\"%s\"}}\\n' \"$APB_ITEM_ID\"\n"
    );
    seed(
        root,
        &held_playbook(root, true, true),
        &[("item.sh", &item), ("hold.sh", &hold)],
    );
}

/// After a drain pause the drive has left: the copy is writable and nothing
/// under the test's root is read-only (no leftover a temp dir cannot
/// remove). A resume takes the read-only copy again while its items run,
/// and gives it back when it ends.
#[test]
fn a_paused_run_leaves_its_scripts_writable_and_a_resume_protects_them() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    seeded_held_probe(root);
    let _env = Env::set(&stub(root, ""), &[]);
    let id = paused_at_hold(root, &[]);
    assert_eq!(
        read_only_dirs(root),
        0,
        "a paused run leaves nothing read-only"
    );
    let res = apb_engine::scheduler::resume(root, &id, None).unwrap();
    assert_eq!(res.outcome, RunStatus::Succeeded);
    if !is_root() {
        assert_eq!(
            take(root, "modes"),
            ["read-only", "read-only"],
            "read-only while items run"
        );
    }
    assert_eq!(read_only_dirs(root), 0, "the ended run gave the bits back");
}

/// A `--now` pause interrupts the attempt in flight; the drive leaves and
/// the copy is writable.
#[test]
fn a_now_pause_leaves_its_scripts_writable() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    seeded_held_probe(root);
    let _env = Env::set(&stub(root, ""), &[]);
    fs::write(root.join("hold"), "").unwrap();
    let (tx, rx) = std::sync::mpsc::channel();
    {
        let r = root.to_path_buf();
        std::thread::spawn(move || {
            let _ = tx.send(run(&r, "lst", None, RunOptions::default()));
        });
    }
    let id = new_run(root, &[], holding);
    use apb_engine::pause::{PauseBy, PauseMode, PauseRequest, request_pause};
    let req = PauseRequest {
        by: Some(PauseBy::Operator),
        mode: PauseMode::Now,
        ..PauseRequest::default()
    };
    request_pause(root, &id, &req).unwrap();
    let res = rx
        .recv_timeout(std::time::Duration::from_secs(30))
        .unwrap()
        .unwrap();
    assert_eq!(res.outcome, RunStatus::Paused);
    let _ = fs::remove_file(root.join("hold"));
    assert_eq!(
        read_only_dirs(root),
        0,
        "a paused run leaves nothing read-only"
    );
}

// --- fix round 4: an item executes the value its work was computed from ---

/// A script list running in `workdir: current`, a link to `A`; item a
/// points the link at `B` while the visit runs. Item b runs in the directory
/// the visit resolved once (`A`), the one its stamp names, never in `B`.
#[test]
fn an_item_runs_in_the_directory_its_visit_resolved() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    key();
    for d in ["A", "B"] {
        fs::create_dir_all(root.join(d)).unwrap();
    }
    std::os::unix::fs::symlink("A", root.join("current")).unwrap();
    let r = root.display();
    let script = format!(
        "printf '%s:%s\\n' \"$APB_ITEM_ID\" \"$(basename \"$(pwd -P)\")\" >> '{r}/pwds'\nif [ \"$APB_ITEM_ID\" = a ]; then ln -sfn B '{r}/current'; fi\nprintf '{{\"blocking\":\"0\",\"id\":\"%s\"}}\\n' \"$APB_ITEM_ID\"\n"
    );
    let yaml = skip_playbook(TWO, SKIP, "")
        .replace("    runner: sh\n", "    runner: sh\n    workdir: current\n");
    assert!(yaml.contains("workdir: current"));
    seed(root, &yaml, &[("item.sh", &script)]);
    let _env = Env::set(&stub(root, ""), &[]);
    let res = run_list(root);
    assert_eq!(res.outcome, RunStatus::Succeeded);
    assert_eq!(
        take(root, "pwds"),
        ["a:A", "b:A"],
        "b runs where its stamp says"
    );
}

/// A configured runtime given as a relative path is taken against the
/// project root once and exec'd by its canonical path: a different file of
/// the same name under the node's own directory never runs.
#[test]
fn a_relative_runner_runs_the_program_its_work_names() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    fs::create_dir_all(root.join("work")).unwrap();
    let rt = |path: &Path, name: &str| {
        fs::write(
            path,
            format!(
                "#!/bin/sh\necho {name} >> '{}'\nexec /bin/sh \"$@\"\n",
                root.join("rt-used").display()
            ),
        )
        .unwrap();
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
    };
    rt(&root.join("rt"), "root");
    rt(&root.join("work/rt"), "work");
    machine_config(root, "runners:\n  sh: ['./rt']\n");
    let cfg = cfg_dir(root);
    let _env = Env::set(
        &stub(root, ""),
        &[("APB_CONFIG_DIR", &cfg.to_string_lossy())],
    );
    apb_core::run_origin::ensure_key().unwrap();
    let yaml = skip_playbook(TWO, SKIP, "")
        .replace("    runner: sh\n", "    runner: sh\n    workdir: work\n");
    seed(root, &yaml, &[("item.sh", &worker(root))]);
    let res = run_list(root);
    assert_eq!(res.outcome, RunStatus::Succeeded);
    assert_eq!(take(root, "rt-used"), ["root", "root"]);
}

// --- fix round 5: the check's directory and PATH are what apb configures ---

/// The Codex trigger: the list node runs in a fixed absolute `workdir`, and
/// its script check accepts only when `./approved` exists where the check
/// runs (the run's tree). A run with `--worktree A` (approved) passes; a run
/// with `--worktree B` (not approved) must not skip: the check's directory is
/// part of the work, and the check runs there and rejects.
#[test]
fn the_check_directory_is_part_of_the_work() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    key();
    for t in ["trees/a", "trees/b", "fixed"] {
        fs::create_dir_all(root.join(t)).unwrap();
    }
    fs::write(root.join("trees/a/approved"), "").unwrap();
    let fixed = root.join("fixed").canonicalize().unwrap();
    let yaml = skip_playbook(TWO, SKIP, "")
        .replace(
            "check: { output: { field: blocking, equals: \"0\" } }",
            "check: { script: scripts/check.sh }",
        )
        .replace(
            "    runner: sh\n",
            &format!("    runner: sh\n    workdir: '{}'\n", fixed.display()),
        );
    assert!(yaml.contains("scripts/check.sh") && yaml.contains("workdir:"));
    let check = "if [ -f approved ]; then echo '{\"verdict\":\"accept\"}'; else echo '{\"verdict\":\"reject\"}'; fi\n";
    seed(
        root,
        &yaml,
        &[("item.sh", &worker(root)), ("check.sh", check)],
    );
    let _env = Env::set(&stub(root, ""), &[]);
    let at = |t: &str| RunOptions {
        worktree: Some(root.join(t).to_string_lossy().into_owned()),
        ..RunOptions::default()
    };
    let first = run_with(root, at("trees/a"));
    assert_eq!(first.outcome, RunStatus::Succeeded);
    assert_eq!(worked(root), ["a", "b"]);
    let second = run_with(root, at("trees/b"));
    let ev = events(root, &second.run_id);
    assert_eq!(
        skip_reads(&ev)[0].1.source.as_deref(),
        Some(first.run_id.as_str()),
        "the set is read"
    );
    assert!(
        settlements(&ev).iter().all(|(_, from, _)| from.is_none()),
        "nothing is skipped"
    );
    assert_eq!(worked(root), ["a", "b"]);
    // Both items were settled by the script check's own rejection in B.
    let rows = checked(&ev);
    let rejected: Vec<&str> = rows
        .iter()
        .filter(|(_, state, verdict, check, _)| {
            state != "passed"
                && verdict.as_deref() == Some("reject")
                && check.as_deref() == Some("script")
        })
        .map(|(item, ..)| item.as_str())
        .collect();
    assert_eq!(
        rejected,
        ["a", "b"],
        "the check rejects both in B: {rows:?}"
    );
}

/// The Codex trigger for `PATH`: the item script runs `publish-item` found on
/// `PATH`. A run with `PATH` selecting one `publish-item` passes; a run with
/// `PATH` selecting another must not skip, and runs the other one.
#[test]
fn path_is_part_of_the_work() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    key();
    let r = root.display();
    for name in ["bin-a", "bin-b"] {
        let d = root.join(name);
        fs::create_dir_all(&d).unwrap();
        let p = d.join("publish-item");
        fs::write(&p, format!("#!/bin/sh\necho {name} >> '{r}/published'\n")).unwrap();
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&p, fs::Permissions::from_mode(0o755)).unwrap();
    }
    let script = format!(
        "publish-item\nprintf '%s\\n' \"$APB_ITEM_ID\" >> '{r}/worked'\nprintf '{{\"blocking\":\"0\",\"id\":\"%s\"}}\\n' \"$APB_ITEM_ID\"\n"
    );
    seed(root, &skip_playbook(TWO, SKIP, ""), &[("item.sh", &script)]);
    let base = std::env::var("PATH").unwrap_or_default();
    let with = |bin: &str| format!("{}:{base}", root.join(bin).display());
    let first = {
        let _env = Env::set(&stub(root, ""), &[("PATH", &with("bin-a"))]);
        run_list(root)
    };
    assert_eq!(first.outcome, RunStatus::Succeeded);
    assert_eq!(take(root, "published"), ["bin-a", "bin-a"]);
    worked(root);
    let _env = Env::set(&stub(root, ""), &[("PATH", &with("bin-b"))]);
    let second = run_list(root);
    assert_eq!(second.outcome, RunStatus::Succeeded);
    let ev = events(root, &second.run_id);
    assert_eq!(skip_refused(&ev), vec![None], "no refusal");
    assert!(
        settlements(&ev).iter().all(|(_, from, _)| from.is_none()),
        "nothing is skipped"
    );
    assert_eq!(take(root, "published"), ["bin-b", "bin-b"]);
}

// --- fix round 6: check policy captured; skips rechecked on resume ---

/// A resumed open visit with nothing changed keeps its skipped rows: the
/// work its execution resolves again equals the one a was skipped under.
#[test]
fn a_resume_with_nothing_changed_keeps_the_skips() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    key();
    let (yaml, feed) = fed_playbook(root, SKIP, "");
    let yaml = yaml.replace(
        "batch: { size: 2, parallel: 1",
        "batch: { size: 1, parallel: 1",
    );
    let r = root.display();
    let item = format!(
        "printf '%s\\n' \"$APB_ITEM_ID\" >> '{r}/worked'\nif [ \"$APB_ITEM_ID\" = b ]; then touch '{r}/started-b'; while [ -f '{r}/hold' ]; do sleep 0.05; done; fi\nprintf '{{\"blocking\":\"0\",\"id\":\"%s\"}}\\n' \"$APB_ITEM_ID\"\n"
    );
    seed(root, &yaml, &[("item.sh", &item), ("feed.sh", &feed)]);
    let _env = Env::set(&stub(root, ""), &[]);
    set_items(root, r#"[{"id":"a"}]"#);
    let first = run_list(root);
    assert_eq!(first.outcome, RunStatus::Succeeded);
    worked(root);
    set_items(root, r#"[{"id":"a"},{"id":"b"},{"id":"c"}]"#);
    fs::write(root.join("hold"), "").unwrap();
    let (tx, rx) = std::sync::mpsc::channel();
    {
        let r = root.to_path_buf();
        std::thread::spawn(move || {
            let _ = tx.send(run(&r, "lst", None, RunOptions::default()));
        });
    }
    let start = std::time::Instant::now();
    while !root.join("started-b").exists() {
        assert!(start.elapsed() < std::time::Duration::from_secs(30));
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    let id = new_run(root, std::slice::from_ref(&first.run_id), |_| true);
    use apb_engine::pause::{PauseBy, PauseMode, PauseRequest, request_pause};
    let req = PauseRequest {
        by: Some(PauseBy::Operator),
        mode: PauseMode::Drain,
        ..PauseRequest::default()
    };
    request_pause(root, &id, &req).unwrap();
    fs::remove_file(root.join("hold")).unwrap();
    assert_eq!(
        rx.recv_timeout(std::time::Duration::from_secs(30))
            .unwrap()
            .unwrap()
            .outcome,
        RunStatus::Paused
    );
    worked(root);
    let res = apb_engine::scheduler::resume(root, &id, None).unwrap();
    assert_eq!(res.outcome, RunStatus::Succeeded);
    assert_eq!(worked(root), ["c"], "a stays skipped, only c works");
    let ev = events(root, &id);
    assert!(
        ev.iter().all(|e| !matches!(&e.payload,
            EventPayload::ListItemSettled { reason: Some(r), .. } if r == "skip_config_changed")),
        "nothing went back to pending"
    );
}

/// The Codex trigger for the check policy: with two workers the tree guard
/// of a script check is off, so a check that changes a tracked file
/// accepts; the same definition with one worker guards the tree. The
/// policy is part of the work, so the one-worker run skips nothing and its
/// check finds the change.
#[test]
fn the_check_policy_is_part_of_the_work() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    key();
    let git = |args: &[&str]| {
        let ok = std::process::Command::new("git")
            .arg("-C")
            .arg(root)
            .args([
                "-c",
                "user.name=t",
                "-c",
                "user.email=t@example.invalid",
                "-c",
                "commit.gpgsign=false",
            ])
            .args(args)
            .status()
            .unwrap()
            .success();
        assert!(ok, "git {args:?}");
    };
    let yaml = skip_playbook(TWO, SKIP, "")
        .replace(
            "batch: { size: 2, parallel: 1",
            "batch: { size: 2, parallel: 2",
        )
        .replace(
            "check: { output: { field: blocking, equals: \"0\" } }",
            "check: { script: scripts/check.sh }",
        );
    let check = "echo x >> tracked.txt\necho '{\"verdict\":\"accept\"}'\n";
    seed(
        root,
        &yaml,
        &[("item.sh", &worker(root)), ("check.sh", check)],
    );
    fs::write(root.join(".gitignore"), ".apb/\nworked\n").unwrap();
    fs::write(root.join("tracked.txt"), "base\n").unwrap();
    git(&["init", "-q"]);
    git(&["add", ".gitignore", "tracked.txt"]);
    git(&["commit", "-q", "-m", "base"]);
    let _env = Env::set(&stub(root, ""), &[]);
    let with = |n: usize| RunOptions {
        max_parallel: Some(n),
        ..RunOptions::default()
    };
    let first = run_with(root, with(2));
    assert_eq!(first.outcome, RunStatus::Succeeded);
    let ev = events(root, &first.run_id);
    // Two workers settle in either order.
    let mut passed: Vec<String> = checked(&ev)
        .into_iter()
        .filter(|(_, state, ..)| state == "passed")
        .map(|(item, ..)| item)
        .collect();
    passed.sort();
    assert_eq!(passed, ["a", "b"], "two workers: the check accepts both");
    let mut stamped = settlements(&ev);
    stamped.sort();
    assert_eq!(
        stamped,
        [("a".to_string(), None, true), ("b".to_string(), None, true)],
        "both passes are stamped"
    );
    git(&["checkout", "-q", "tracked.txt"]);
    let second = run_with(root, with(1));
    let ev = events(root, &second.run_id);
    assert_eq!(
        skip_reads(&ev)[0].1.source.as_deref(),
        Some(first.run_id.as_str())
    );
    assert!(
        settlements(&ev).iter().all(|(_, from, _)| from.is_none()),
        "nothing is skipped"
    );
    // One worker guards the tree: the check's write to the tracked file
    // fails both items, each by the script check.
    let rows = checked(&ev);
    let guarded: Vec<&str> = rows
        .iter()
        .filter(|(_, state, _, check, note)| {
            state != "passed"
                && check.as_deref() == Some("script")
                && note.contains("the check changed the working tree")
        })
        .map(|(item, ..)| item.as_str())
        .collect();
    assert_eq!(guarded, ["a", "b"], "the guard finds the change: {rows:?}");
}

// --- fix round 7: isolation narrowed, notes and outputs on a recheck ---

/// A skip list on a node that declares `isolation: none` skips as before;
/// the same list under `best_effort` is refused at run start (V288).
#[test]
fn skip_holds_under_isolation_none_and_is_refused_when_isolated() {
    let with = |level: &str| {
        agent_skip_playbook().replace(
            "    prompt: \"Handle one item of the list.\"\n",
            &format!("    prompt: \"Handle one item of the list.\"\n    isolation: {{ level: {level} }}\n"),
        )
    };
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    crate::list_run_test::seed(root, &with("none"), &[]);
    let prog = stub(root, "");
    let cfg = cfg_dir(root);
    fs::create_dir_all(&cfg).unwrap();
    let _env = Env::set(&prog, &[("APB_CONFIG_DIR", &cfg.to_string_lossy())]);
    apb_core::run_origin::ensure_key().unwrap();
    let first = run_with(root, base_opts(root));
    assert_eq!(first.outcome, RunStatus::Succeeded);
    let second = run_with(root, base_opts(root));
    assert_eq!(second.outcome, RunStatus::Succeeded);
    let skipped = settlements(&events(root, &second.run_id))
        .iter()
        .filter(|(_, from, _)| from.is_some())
        .count();
    assert_eq!(skipped, 2, "both items are skipped under isolation none");
    let iso = tempfile::tempdir().unwrap();
    crate::list_run_test::seed(iso.path(), &with("best_effort"), &[]);
    let refused = run(iso.path(), "lst", None, base_opts(iso.path()))
        .map(|r| r.outcome)
        .unwrap_err()
        .to_string();
    assert!(refused.contains("V288"), "{refused}");
}

/// Run A passes a; run B skips a while b needs a look; a supervisor retry,
/// after a note when `note`, reopens the visit. Returns run B's events
/// and the items worked after the retry.
fn reopen_after_a_skip(root: &Path, note: bool) -> (Vec<Event>, Vec<String>) {
    seeded_fed_locked(root, SKIP, "\non_needs_look: fail");
    set_items(root, r#"[{"id":"a"}]"#);
    let first = run_list(root);
    assert_eq!(first.outcome, RunStatus::Succeeded);
    worked(root);
    set_items(root, r#"[{"id":"a"},{"id":"b"}]"#);
    fs::write(root.join("bad-b"), "").unwrap();
    let opts = RunOptions {
        mode: apb_engine::scheduler::RunMode::Supervised,
        ..RunOptions::default()
    };
    let run_id = apb_engine::scheduler::run_background(root, "lst", None, opts).unwrap();
    poll_journal(root, &run_id, |ev| {
        ev.iter()
            .any(|e| matches!(e.payload, EventPayload::WakeRaised { .. }))
    });
    assert_eq!(worked(root), ["b"], "a was skipped");
    fs::remove_file(root.join("bad-b")).unwrap();
    let mut controls = Vec::new();
    if note {
        controls.push(apb_engine::control::Control::ContextAppend {
            note: "summarize in French".into(),
        });
    }
    controls.push(apb_engine::control::Control::Retry {
        node: "work".into(),
        prompt_override: None,
    });
    for c in controls {
        apb_engine::scheduler::post_supervisor_command(root, &run_id, c).unwrap();
    }
    poll_journal(root, &run_id, |ev| {
        apb_engine::state::RunState::fold(ev).run_status == RunStatus::Succeeded
    });
    let mut w = worked(root);
    w.sort();
    (events(root, &run_id), w)
}

/// The Codex trigger for a note: the note is in force at the reopen, so the
/// retained skip of a goes back to pending with the note's reason and a
/// works under the note with b.
#[test]
fn a_note_in_force_takes_back_the_retained_skips_of_a_reopen() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let _env = Env::set(&stub(root, ""), &[]);
    let (ev, w) = reopen_after_a_skip(root, true);
    assert!(
        ev.iter().any(|e| matches!(&e.payload,
            EventPayload::ListItemSettled { item, reason: Some(r), .. }
                if item == "a" && r == "skip_supervisor_note")),
        "a went back to pending under the note"
    );
    assert_eq!(w, ["a", "b"], "a works under the note too");
    let fold = apb_engine::list::ListFold::of(&ev, "work").unwrap();
    assert_eq!(fold.skip_reworked, 1);
    assert!(fold.items["a"].skipped_from.is_none());
}

/// The other side: a reopen with no note and the same work keeps the
/// retained skip of a; only b works.
#[test]
fn a_reopen_with_nothing_changed_keeps_the_retained_skips() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let _env = Env::set(&stub(root, ""), &[]);
    let (ev, w) = reopen_after_a_skip(root, false);
    assert_eq!(w, ["b"], "a stays skipped");
    let fold = apb_engine::list::ListFold::of(&ev, "work").unwrap();
    assert_eq!(fold.skip_reworked, 0);
    assert!(fold.items["a"].skipped_from.is_some());
}

/// The Codex trigger for an inherited output: a paused visit holds a as
/// skipped with `{"value":"old"}`; `PATH` changes and the run resumes. The
/// recheck takes a back; paused again before a is worked, a is pending with
/// no output on every surface; resumed, a's new execution prints plain
/// text, so a passes with no output, and the next run skips a with none.
#[test]
fn a_skip_taken_back_keeps_no_inherited_output() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    key();
    let (yaml, feed) = fed_playbook(root, SKIP, "");
    let yaml = yaml
        .replace(
            "batch: { size: 2, parallel: 1",
            "batch: { size: 1, parallel: 1",
        )
        .replace("item_outputs: [blocking, id]", "item_outputs: []")
        .replace(
            "check: { output: { field: blocking, equals: \"0\" } }",
            "# no item check",
        );
    assert!(!yaml.contains("blocking"), "{yaml}");
    let r = root.display();
    let item = format!(
        "printf '%s\\n' \"$APB_ITEM_ID\" >> '{r}/worked'\ntouch '{r}/started-'\"$APB_ITEM_ID\"\nwhile [ -f '{r}/hold-'\"$APB_ITEM_ID\" ]; do sleep 0.05; done\nif [ -f '{r}/plain' ]; then echo done; else printf '{{\"value\":\"old\"}}\\n'; fi\n"
    );
    seed(root, &yaml, &[("item.sh", &item), ("feed.sh", &feed)]);
    let base = std::env::var("PATH").unwrap_or_default();
    let other = root.join("other-bin");
    fs::create_dir_all(&other).unwrap();
    let changed = format!("{}:{base}", other.display());
    let started = |id: &str| {
        let p = root.join(format!("started-{id}"));
        let t = std::time::Instant::now();
        while !p.exists() {
            assert!(t.elapsed() < std::time::Duration::from_secs(30));
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        fs::remove_file(&p).unwrap();
    };
    let pause = |id: &str, mode| {
        use apb_engine::pause::{PauseBy, PauseRequest, request_pause};
        let req = PauseRequest {
            by: Some(PauseBy::Operator),
            mode,
            ..PauseRequest::default()
        };
        request_pause(root, id, &req).unwrap();
    };
    let in_thread = |f: Box<dyn FnOnce() -> RunResult + Send>| {
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let _ = tx.send(f());
        });
        rx
    };
    let _env = Env::set(&stub(root, ""), &[]);
    set_items(root, r#"[{"id":"a"}]"#);
    let first = run_list(root);
    assert_eq!(first.outcome, RunStatus::Succeeded);
    worked(root);
    let _ = fs::remove_file(root.join("started-a"));
    // Run B: a is skipped with the old output, b holds, the run pauses.
    set_items(root, r#"[{"id":"a"},{"id":"b"}]"#);
    fs::write(root.join("hold-b"), "").unwrap();
    let rx = {
        let r = root.to_path_buf();
        in_thread(Box::new(move || {
            run(&r, "lst", None, RunOptions::default()).unwrap()
        }))
    };
    started("b");
    let id = new_run(root, std::slice::from_ref(&first.run_id), |_| true);
    pause(&id, apb_engine::pause::PauseMode::Drain);
    fs::remove_file(root.join("hold-b")).unwrap();
    let secs = std::time::Duration::from_secs(30);
    assert_eq!(rx.recv_timeout(secs).unwrap().outcome, RunStatus::Paused);
    assert_eq!(worked(root), ["b"]);
    let fold = apb_engine::list::ListFold::of(&events(root, &id), "work").unwrap();
    assert_eq!(
        fold.items["a"].output.as_deref(),
        Some(r#"{"value":"old"}"#)
    );
    // The configured PATH changes; the resume takes a back, a holds and
    // the run pauses before a is worked.
    let _path = common::EnvVar::set("PATH", &changed);
    fs::write(root.join("plain"), "").unwrap();
    fs::write(root.join("hold-a"), "").unwrap();
    let rx = {
        let (r, i) = (root.to_path_buf(), id.clone());
        in_thread(Box::new(move || {
            apb_engine::scheduler::resume(&r, &i, None).unwrap()
        }))
    };
    started("a");
    pause(&id, apb_engine::pause::PauseMode::Now);
    assert_eq!(rx.recv_timeout(secs).unwrap().outcome, RunStatus::Paused);
    fs::remove_file(root.join("hold-a")).unwrap();
    let ev = events(root, &id);
    let fold = apb_engine::list::ListFold::of(&ev, "work").unwrap();
    let a = &fold.items["a"];
    assert_eq!(a.output, None, "no inherited output while pending");
    assert!(a.skipped_from.is_none());
    let pb = apb_core::schema::Playbook::from_yaml(&yaml).unwrap();
    let lists = apb_engine::list::run_lists(Some(&pb), &ev);
    let row = lists[0].items.iter().find(|i| i.id == "a").unwrap();
    assert_eq!(
        (row.state.as_str(), row.skipped_from.as_deref()),
        ("pending", None)
    );
    // Resumed: a works and prints plain text, so it passes with no output.
    worked(root);
    let res = apb_engine::scheduler::resume(root, &id, None).unwrap();
    assert_eq!(res.outcome, RunStatus::Succeeded);
    assert_eq!(worked(root), ["a"]);
    let fold = apb_engine::list::ListFold::of(&events(root, &id), "work").unwrap();
    assert_eq!(
        fold.items["a"].output, None,
        "the new execution printed none"
    );
    assert!(fold.items["a"].skip_digest.is_some(), "a is stamped");
    // The next run skips a and inherits no old JSON.
    let next = run_list(root);
    assert_eq!(next.outcome, RunStatus::Succeeded);
    let fold = apb_engine::list::ListFold::of(&events(root, &next.run_id), "work").unwrap();
    assert_eq!(fold.items["a"].skipped_from.as_deref(), Some(id.as_str()));
    assert_eq!(fold.items["a"].output, None, "no old JSON inherited");
}

// --- fix round 8 ---

/// A loop over an agent list with `rerun: not_passed`: the first visit skips
/// a with the output it passed with in run A; a note before the second
/// visit refuses the skip, so a is worked there. Its item section names no
/// earlier attempt: the skipped row never ran in this run, so its inherited
/// output is not what a did before.
#[test]
fn a_skipped_row_is_never_the_items_earlier_attempt() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    key();
    let list = format!(
        "items: {TWO}\nbatch: {{ size: 2, parallel: 1, retries: 0 }}\nitem_outputs: [blocking]\ncheck: {{ output: {{ field: blocking, equals: \"0\" }} }}\nskip: {SKIP}\nrerun: not_passed"
    );
    let yaml = playbook(&list, "", "").replace(
        "  - { from: work, to: done }\n",
        "  - { from: work, to: work, max_traversals: 1 }\n  - { from: work, to: done, fallback: true }\n",
    );
    seed(root, &yaml, &[]);
    let _env = Env::set(&stub(root, ""), &[]);
    assert_eq!(run_list(root).outcome, RunStatus::Succeeded);
    let second = run_list(root);
    cut_after_first_visit(
        root,
        &second.run_id,
        &[
            serde_json::json!({"type": "supervisor_action", "action": "context_append", "detail": "summarize in French"}),
        ],
    );
    let inv = crate::list_run_test::stub_dir(root).join("inv");
    fs::remove_dir_all(&inv).unwrap();
    fs::create_dir_all(&inv).unwrap();
    let res = resume_driverless(root, &second.run_id);
    assert_eq!(res.outcome, RunStatus::Succeeded);
    let a: Vec<String> = crate::list_run_test::invocations(root)
        .into_iter()
        .filter(|i| i.item == "a")
        .map(|i| i.prompt)
        .collect();
    assert_eq!(a.len(), 1, "a works once, in the second visit");
    assert!(a[0].contains("## item"), "{}", a[0]);
    assert!(!a[0].contains("previous_outputs"), "{}", a[0]);
    assert!(!a[0].contains("It ran before"), "{}", a[0]);
}

/// An eval run skips nothing, so its drive never takes the read-only
/// scripts copy: each item sees the copy writable while it runs.
#[test]
fn an_eval_run_leaves_its_scripts_modes_untouched() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    key();
    let r = root.display();
    let item = format!(
        "if [ -w \"$APB_RUN_DIR/scripts\" ]; then echo writable >> '{r}/modes'; else echo read-only >> '{r}/modes'; fi\nprintf '{{\"blocking\":\"0\",\"id\":\"%s\"}}\\n' \"$APB_ITEM_ID\"\n"
    );
    seed(root, &skip_playbook(TWO, SKIP, ""), &[("item.sh", &item)]);
    let _env = Env::set(&stub(root, ""), &[]);
    let eval = run_with(
        root,
        RunOptions {
            eval: Some(apb_engine::run_config::EvalRunSettings::default()),
            ..RunOptions::default()
        },
    );
    assert_eq!(eval.outcome, RunStatus::Succeeded);
    if !is_root() {
        assert_eq!(take(root, "modes"), ["writable", "writable"]);
    }
}

/// Set in the child test process of
/// [`a_scenario_run_leaves_its_scripts_modes_untouched`]: the file its
/// terminal observer writes the count of read-only directories to. The
/// binary's init (`tests/main.rs`) leaves both observer slots free in that
/// process, so the child's own observer is the one the journal calls.
pub(crate) const SCENARIO_CHILD: &str = "APB_LIST_SKIP_SCENARIO_CHILD";

/// The child's terminal observer: called by the journal once the run's
/// terminal event is in the file, while the drive (and any scripts
/// protection it took) is still on the stack, since a scenario run's
/// finish gives no write bits back before it.
fn scenario_observed(run_dir: &Path) {
    if let Ok(out) = std::env::var(SCENARIO_CHILD) {
        let _ = fs::write(out, read_only_dirs(run_dir).to_string());
    }
}

/// A scenario run skips nothing, so its drive never takes the read-only
/// scripts copy. No item program runs in a scenario (the scenario guard
/// refuses every program start), so the copy is looked at from the
/// journal's terminal observer, which runs synchronously inside the drive
/// before a protection it held would be dropped. The observer is process
/// wide and taken by the binary's init everywhere else, so the run happens
/// in a child test process of its own.
#[test]
fn a_scenario_run_leaves_its_scripts_modes_untouched() {
    if std::env::var_os(SCENARIO_CHILD).is_none() {
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("observed");
        let child = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "list_skip_test::a_scenario_run_leaves_its_scripts_modes_untouched",
                "--exact",
                "--test-threads=1",
                "--nocapture",
            ])
            .env(SCENARIO_CHILD, &out)
            .output()
            .unwrap();
        let log = format!(
            "{}{}",
            String::from_utf8_lossy(&child.stdout),
            String::from_utf8_lossy(&child.stderr)
        );
        assert!(child.status.success(), "child failed: {log}");
        let seen = fs::read_to_string(&out)
            .unwrap_or_else(|e| panic!("the terminal observer never ran ({e}): {log}"));
        assert_eq!(seen, "0", "no read-only directory as the scenario ended");
        return;
    }
    apb_engine::terminal_hook::install(scenario_observed);
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let list = format!(
        "items: {TWO}\nbatch: {{ size: 2, parallel: 1, retries: 0 }}\nitem_outputs: [blocking]\ncheck: {{ output: {{ field: blocking, equals: \"0\" }} }}\nskip: {SKIP}"
    );
    crate::list_run_test::seed(
        root,
        &playbook(&list, "", ""),
        &[("note.sh", "echo note\n")],
    );
    key();
    let _env = Env::set(&stub(root, ""), &[]);
    let scenario = "schema: 1\nid: lst\nanswers:\n  work:\n    - visit: 1\n      items: { \"*\": { status: succeeded, outputs: { blocking: \"0\" } } }\n";
    let prep = apb_engine::scenario::session::ScenarioPrep {
        scenario: yaml_serde::from_str::<apb_core::scenario::Scenario>(scenario).unwrap(),
        level: apb_core::scenario::ScenarioLevel::Quick,
        scenario_digest: "sha256:s".into(),
        max_requests: 20,
        sent: Default::default(),
        external_data: false,
    };
    let res = apb_engine::scheduler::run_scenario(root, "lst", None, &prep).unwrap();
    assert_eq!(res.outcome, RunStatus::Succeeded);
}

/// The drive gives every entry of the scripts copy the exact mode it had:
/// a copy given odd, write-less modes while the run sat paused (the owner
/// executable bit kept, so the pin holds) has those modes again after the
/// resumed drive, not owner write added.
#[test]
fn a_drive_gives_the_scripts_copy_its_exact_modes_back() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    seeded_held_probe(root);
    let _env = Env::set(&stub(root, ""), &[]);
    let id = paused_at_hold(root, &[]);
    let scripts = root.join(".apb/runs").join(&id).join("scripts");
    let mut set = Vec::new();
    for name in ["item.sh", "hold.sh"] {
        let p = scripts.join(name);
        let exec = fs::metadata(&p).unwrap().permissions().mode() & 0o111;
        // Owner bits only: a run directory's modes are private, and a
        // resume narrows group and other bits away (`run_perms`).
        let odd = if exec != 0 {
            0o400 | (exec & 0o100)
        } else {
            0o400
        };
        fs::set_permissions(&p, fs::Permissions::from_mode(odd)).unwrap();
        set.push((p, odd));
    }
    let res = apb_engine::scheduler::resume(root, &id, None).unwrap();
    assert_eq!(res.outcome, RunStatus::Succeeded);
    for (p, odd) in set {
        assert_eq!(
            fs::metadata(&p).unwrap().permissions().mode() & 0o7777,
            odd,
            "{}",
            p.display()
        );
    }
}
