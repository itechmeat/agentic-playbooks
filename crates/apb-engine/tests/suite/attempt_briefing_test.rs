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
