//! Answer-matching rule for the `apb __ask-server` live-question sidecar
//! (spec 2026-07-20-interactive-nodes, Task 10).
//!
//! The end-to-end child-process test (spawning the built `apb` binary, driving
//! a real MCP `initialize` + `tools/call ask_user` over stdio) lives in the
//! `apb-cli` crate: `CARGO_BIN_EXE_apb` is only defined for the integration
//! binary of the package that declares the `apb` bin, exactly as the engine's
//! `detached_driver_test` documents. Here we pin the pure matching rule, which
//! is what makes a repeated question on one node pick up its own answer.

use apb_engine::PostedAnswer;
use apb_mcp::ask_server::resolve_answer;

fn ans(node: &str, answer: &str) -> PostedAnswer {
    PostedAnswer {
        seq: 0,
        node: node.to_string(),
        answer: answer.to_string(),
        answered_by: "human".to_string(),
    }
}

#[test]
fn resolves_the_first_answer_appended_after_the_recorded_tail() {
    // `tail` is the per-node answer count observed at post time. With two prior
    // answers for `ask`, the resolving answer is the third one for that node -
    // by position, not by content - and answers for other nodes never shift
    // the index. This mirrors the scheduler's positional `nth(answered)`
    // consumption, so the sidecar and the drive agree on which answer belongs
    // to which question round.
    let answers = vec![
        ans("ask", "old-1"),
        ans("other", "noise"),
        ans("ask", "old-2"),
        ans("ask", "pg"),
    ];
    assert_eq!(resolve_answer(&answers, "ask", 2), Some("pg"));

    // Not yet landed: only two answers exist for `ask`, nothing at index 2.
    let partial = vec![ans("ask", "old-1"), ans("ask", "old-2")];
    assert_eq!(resolve_answer(&partial, "ask", 2), None);
}
