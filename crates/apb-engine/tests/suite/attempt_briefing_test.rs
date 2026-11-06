//! The retry briefing end to end: a recording stub stands in for the agent,
//! its first attempt journals send intents the way `apb connector call`
//! does and then fails, and the tests read the prompt the next attempt was
//! actually handed. No model is ever called.

use apb_core::registry::init_project;
use apb_engine::state::RunStatus;

use crate::common;
use crate::token_economy_test::{invocations, recording_stub, run_with_stub, seed_playbook};

const HEADER: &str = "## What earlier attempts of this step did (apb's record)";

/// A report block the adapter parses as the agent's self-assessed status.
const OK: &str = "printf '\\n```yaml\\nstatus: success\\nsummary: ok\\n```\\n'";

/// Appends one event to the run journal from inside the stub the way an
/// `apb connector call` subprocess of the attempt does: under the journal's
/// append lock (`flock`, which apb's own writers take too), with the next
/// seq after the last line read under that lock.
const APPEND: &str = r#"J="$APB_RUN_DIR/events.jsonl"
add() { perl -e 'use Fcntl qw(:flock); open(my $w, ">>", $ARGV[0]) or die; flock($w, LOCK_EX) or die; open(my $r, "<", $ARGV[0]) or die; my $s = -1; while (<$r>) { $s = $1 if /^\{"seq":(\d+),/ } printf $w "{\"seq\":%d,\"ts\":1,%s}\n", $s + 1, $ARGV[1]; close $w;' "$J" "$1"; }
intent() { add "\"type\":\"send_intent_recorded\",\"intent\":\"$1\",\"nonce\":\"n-$1\",\"node\":\"work\",\"attempt\":$APB_ATTEMPT,\"connector\":\"telegram\",\"function\":\"send_message\",\"account\":\"main\",\"mode\":\"none\",\"label\":\"$2\",\"pid\":0,\"deadline_ms\":0"; }
settle() { add "\"type\":\"send_intent_settled\",\"intent\":\"$1\",\"outcome\":\"$2\",\"source\":\"response\",\"detail\":\"timeout\""; }
"#;

/// The first attempt's sends: one confirmed, one unknown, one not sent.
const FIRST_SENDS: &str = r#"intent si-a summary; settle si-a confirmed
intent si-b announce; settle si-b unknown
intent si-c digest; settle si-c not_sent
"#;

/// A one-node playbook, `extra` added to the node.
fn seed(root: &std::path::Path, extra: &str, fallbacks: &[(&str, &str)]) {
    init_project(root).unwrap();
    seed_playbook(
        root,
        "one",
        &format!(
            "schema: 2\nid: one\nname: One\nversion: 1.0.0\ndefaults: {{ profile: main }}\nnodes:\n  - {{ id: start, type: start }}\n  - {{ id: work, type: agent_task, prompt: \"Do the job.\"{extra} }}\n  - {{ id: done, type: finish, outcome: success }}\nedges:\n  - {{ from: start, to: work }}\n  - {{ from: work, to: done }}\n"
        ),
    );
    common::seed_profile(root, "main", "claude", "haiku", fallbacks);
}

/// The stub: the first attempt sends, says `first`, and exits non-zero;
/// every later one succeeds.
fn stub(root: &std::path::Path, first: &str) -> String {
    recording_stub(
        root,
        &format!("{APPEND}if [ \"$N\" = 1 ]; then\n{FIRST_SENDS}{first}\nexit 1\nfi\n{OK}"),
    )
}

/// The briefing is there and tells each of the first attempt's sends under
/// its outcome. Its wording is the unit tests' (`attempt_memory`); this
/// checks only that the real retry paths deliver it.
fn assert_briefed(prompt: &str) {
    let at = |text: &str| {
        prompt
            .find(text)
            .unwrap_or_else(|| panic!("missing {text:?}"))
    };
    let header = at(HEADER);
    assert!(at("Attempt 1 (claude, haiku):") > header);
    for (section, label) in [
        ("Sends apb confirmed:", "intent \"summary\""),
        ("Sends with an unknown outcome:", "intent \"announce\""),
        ("Sends that did not go out:", "intent \"digest\""),
    ] {
        let start = at(section);
        let end = prompt[start..]
            .find("\n\n")
            .map_or(prompt.len(), |n| start + n);
        assert!(
            prompt[start..end].contains(label),
            "{label} must be told under {section:?}"
        );
    }
}

#[test]
fn a_retry_is_briefed_on_what_the_failed_attempt_sent() {
    let dir = tempfile::tempdir().unwrap();
    seed(dir.path(), ", max_retries: 1", &[]);
    let prog = stub(dir.path(), "echo 'tests are red'");
    assert_eq!(
        run_with_stub(dir.path(), "one", &prog, None),
        RunStatus::Succeeded
    );
    let inv = invocations(dir.path());
    assert_eq!(inv.len(), 2);
    assert!(
        !inv[0].prompt().contains(HEADER),
        "no briefing on a first attempt"
    );
    // The retry continues the session: the briefing follows the
    // continuation note.
    assert!(inv[1].resumed());
    assert_briefed(inv[1].prompt());
}

#[test]
fn an_infrastructure_retry_is_briefed_too() {
    let dir = tempfile::tempdir().unwrap();
    seed(dir.path(), "", &[]);
    let prog = stub(dir.path(), "echo 'fetch failed: ECONNRESET' 1>&2");
    assert_eq!(
        run_with_stub(dir.path(), "one", &prog, None),
        RunStatus::Succeeded
    );
    let inv = invocations(dir.path());
    assert_eq!(inv.len(), 2, "one infrastructure retry");
    let prompt = inv[1].prompt();
    assert_briefed(prompt);
    assert!(prompt.contains("Attempt 1 (claude, haiku): ended with a transient failure"));
}

#[test]
fn a_fallback_step_is_briefed_in_its_fresh_session() {
    let dir = tempfile::tempdir().unwrap();
    seed(dir.path(), "", &[("claude", "sonnet")]);
    let prog = stub(dir.path(), "echo 'cannot do it'");
    assert_eq!(
        run_with_stub(dir.path(), "one", &prog, None),
        RunStatus::Succeeded
    );
    let inv = invocations(dir.path());
    assert_eq!(inv.len(), 2);
    assert!(!inv[1].resumed(), "another model starts fresh");
    let prompt = inv[1].prompt();
    assert!(prompt.contains("Do the job."));
    assert_briefed(prompt);
}

/// A rejected success report reaches the fresh retry: the reason apb
/// recorded is quoted in the prompt with the repair-scope paragraph, and
/// the entry the retried execution stored is reusable: a second run of the
/// same playbook, with no rejection, hits it under the very key it was
/// stored with. (The key is probed before attempt 1, which is never
/// briefed; the next test probes one for a briefed attempt.)
#[test]
fn a_rejection_reason_reaches_the_fresh_retry_prompt() {
    let dir = tempfile::tempdir().unwrap();
    // The agent cache needs a git work tree for the workspace fingerprint;
    // keep apb's state and the stub's files out of it.
    std::fs::write(dir.path().join(".gitignore"), ".apb/\ninv/\nstub.sh\n").unwrap();
    seed(
        dir.path(),
        ", max_retries: 1, cache: auto, success_check: { marker: \"WAVE-COMPLETE\" }",
        &[],
    );
    for args in [
        &["init", "-q"][..],
        &["add", "."],
        &[
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@t",
            "commit",
            "-qm",
            "c1",
        ],
    ] {
        let ok = std::process::Command::new("git")
            .args(args)
            .current_dir(dir.path())
            .status()
            .unwrap()
            .success();
        assert!(ok, "git {args:?}");
    }
    // Every attempt self-reports success; attempt 1 omits the marker, so
    // the success_check rejects it and apb retries in a fresh session.
    let prog = recording_stub(
        dir.path(),
        "if [ \"$N\" = 1 ]; then\necho 'ST6 dispatched. interim only'\nelse\necho 'done WAVE-COMPLETE'\nfi\n",
    );
    assert_eq!(
        run_with_stub(dir.path(), "one", &prog, None),
        RunStatus::Succeeded
    );
    let inv = invocations(dir.path());
    assert_eq!(inv.len(), 2, "one retry after the rejection");
    let prompt = inv[1].prompt();
    assert!(
        prompt.contains(r#"reported success; apb rejected it: "success report rejected: completion marker `WAVE-COMPLETE` not found in output""#),
        "the retry prompt must quote the rejection:\n{prompt}"
    );
    assert!(
        prompt.contains("Repair scope:"),
        "the retry prompt must carry the repair scope:\n{prompt}"
    );

    let first = only_run(dir.path());
    let stored = payloads(dir.path(), &first)
        .into_iter()
        .find_map(|p| match p {
            apb_engine::event::EventPayload::NodeCacheStored { node, key } if node == "work" => {
                Some(key)
            }
            _ => None,
        })
        .expect("the retried execution stores its result");

    // A clean second run: no agent call, a hit on the stored key.
    assert_eq!(
        run_with_stub(dir.path(), "one", &prog, None),
        RunStatus::Succeeded
    );
    assert_eq!(
        invocations(dir.path()).len(),
        2,
        "the second run is served from the cache"
    );
    let second = std::fs::read_dir(dir.path().join(".apb/runs"))
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .find(|id| *id != first)
        .expect("a second run dir");
    let hit = payloads(dir.path(), &second)
        .into_iter()
        .find_map(|p| match p {
            apb_engine::event::EventPayload::NodeCacheHit { node, key, .. } if node == "work" => {
                Some(key)
            }
            _ => None,
        });
    assert_eq!(
        hit.as_deref(),
        Some(stored.as_str()),
        "the clean run's key is the one the rejected run stored"
    );
}

/// The briefing stays outside the node cache key, proved on a key probed
/// for a briefed attempt: visit 1 of `work` is rejected and fails (no
/// retry), the failure edge loops back, and visit 2's first attempt is
/// briefed on the rejection. Its probe must give visit 1's key.
#[test]
fn a_briefed_visit_probes_the_same_cache_key_as_the_unbriefed_one() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    std::fs::write(root.join(".gitignore"), ".apb/\ninv/\nstub.sh\n").unwrap();
    init_project(root).unwrap();
    seed_playbook(
        root,
        "one",
        "schema: 2\nid: one\nname: One\nversion: 1.0.0\ndefaults: { profile: main }\nnodes:\n  - { id: start, type: start }\n  - { id: work, type: agent_task, prompt: \"Do the job.\", max_retries: 0, cache: auto, success_check: { marker: \"WAVE-COMPLETE\" } }\n  - { id: done, type: finish, outcome: success }\nedges:\n  - { from: start, to: work }\n  - { from: work, to: work, condition: { type: node_status, node: work, equals: failure }, max_traversals: 1 }\n  - { from: work, to: done, fallback: true }\n",
    );
    common::seed_profile(root, "main", "claude", "haiku", &[]);
    for args in [
        &["init", "-q"][..],
        &["add", "."],
        &[
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@t",
            "commit",
            "-qm",
            "c1",
        ],
    ] {
        let ok = std::process::Command::new("git")
            .args(args)
            .current_dir(root)
            .status()
            .unwrap()
            .success();
        assert!(ok, "git {args:?}");
    }
    let prog = recording_stub(
        root,
        "if [ \"$N\" = 1 ]; then\necho 'interim only'\nelse\necho 'done WAVE-COMPLETE'\nfi\n",
    );
    assert_eq!(
        run_with_stub(root, "one", &prog, None),
        RunStatus::Succeeded
    );
    let inv = invocations(root);
    assert_eq!(inv.len(), 2, "one attempt per visit");
    assert!(
        inv[1].prompt().contains("apb rejected it: "),
        "visit 2's first attempt is briefed:\n{}",
        inv[1].prompt()
    );
    let run_id = only_run(root);
    let misses: Vec<String> = payloads(root, &run_id)
        .into_iter()
        .filter_map(|p| match p {
            apb_engine::event::EventPayload::NodeCacheMiss { node, key } if node == "work" => {
                Some(key)
            }
            _ => None,
        })
        .collect();
    assert_eq!(misses.len(), 2, "one probe per visit: {misses:?}");
    assert_eq!(
        misses[1], misses[0],
        "the briefing must not move the cache key"
    );
}

/// The id of the one run in the project.
fn only_run(root: &std::path::Path) -> String {
    let ids: Vec<String> = std::fs::read_dir(root.join(".apb/runs"))
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(ids.len(), 1, "one run: {ids:?}");
    ids.into_iter().next().unwrap()
}

/// Every journaled payload of the run.
fn payloads(root: &std::path::Path, run_id: &str) -> Vec<apb_engine::event::EventPayload> {
    apb_engine::event::read_all(&root.join(".apb/runs").join(run_id))
        .unwrap()
        .into_iter()
        .map(|e| e.payload)
        .collect()
}

/// A value no token-shape rule recognizes: only the run's declared secret
/// param makes it a secret.
const SECRET: &str = "quietharborquietharborquietharbor";

/// The reason a rejection records is scrubbed of the run's declared secret
/// values before it is journaled and quoted to the retry. The marker is the
/// simplest deterministic way to put the value into a reason.
#[test]
fn a_declared_secret_value_never_reaches_rejected_for() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    init_project(root).unwrap();
    seed_playbook(
        root,
        "one",
        &format!(
            "schema: 2\nid: one\nname: One\nversion: 1.0.0\ndefaults: {{ profile: main }}\nparams:\n  - {{ name: token, type: text, secret: true }}\nnodes:\n  - {{ id: start, type: start }}\n  - {{ id: work, type: agent_task, prompt: \"Do the job.\", max_retries: 1, success_check: {{ marker: \"{SECRET}\" }} }}\n  - {{ id: done, type: finish, outcome: success }}\nedges:\n  - {{ from: start, to: work }}\n  - {{ from: work, to: done }}\n"
        ),
    );
    common::seed_profile(root, "main", "claude", "haiku", &[]);
    let prog = recording_stub(
        root,
        &format!("if [ \"$N\" = 1 ]; then\necho 'interim only'\nelse\necho 'done {SECRET}'\nfi\n"),
    );
    let res = {
        let _env = common::env_lock();
        let _cmd = common::EnvVar::set("APB_AGENT_CMD", &prog);
        let opts = apb_engine::scheduler::RunOptions {
            params: [("token".to_string(), SECRET.to_string())].into(),
            ..Default::default()
        };
        apb_engine::scheduler::run(root, "one", None, opts).unwrap()
    };
    assert_eq!(res.outcome, RunStatus::Succeeded);
    let reasons: Vec<String> = payloads(root, &res.run_id)
        .into_iter()
        .filter_map(|p| match p {
            apb_engine::event::EventPayload::AttemptFinished { rejected_for, .. } => rejected_for,
            _ => None,
        })
        .collect();
    assert_eq!(reasons.len(), 1, "one rejection: {reasons:?}");
    assert!(
        !reasons[0].contains(SECRET) && reasons[0].contains("[redacted]"),
        "the secret is replaced: {:?}",
        reasons[0]
    );
    // The prompt names the marker itself; the briefing's quote of the
    // rejection is what must not carry the value.
    let retry = invocations(root)[1].prompt().to_string();
    let quote = retry
        .split_once("apb rejected it: ")
        .map(|(_, rest)| rest.lines().next().unwrap_or_default())
        .expect("the briefing quotes the rejection");
    assert!(
        !quote.contains(SECRET) && quote.contains("[redacted]"),
        "the briefing quotes the scrubbed reason: {quote:?}"
    );
}
