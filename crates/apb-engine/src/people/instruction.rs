//! apb's own text for the person a run waits on: what a human-review gate
//! and a supervised failure wake ask, and how to answer. The run surfaces,
//! the journal and the notifications all read these, so the wording never
//! drifts between them. Each one leads with the conclusion: what waits, for
//! whom, and the one way to answer.

/// The decision mechanics for a human-review gate: how an owner (or an agent
/// relaying on their behalf) actually records a decision. Single source of the
/// how-to wording, composed into the full `review_instruction` and also
/// surfaced on its own in `PendingReview::how_to_decide`.
pub fn review_how_to_decide(node_id: &str) -> String {
    format!(
        "To decide, run `apb review <run-id> {node_id} --decision <option>` \
         or call the review_decide MCP tool with the same node and decision."
    )
}

/// The owner-facing pending instruction for a human-review gate (issue #42
/// finding 4): one self-contained line naming the gate, listing the available
/// decisions, and appending `review_how_to_decide`. Shared by the
/// `ReviewRequested` event and the `run_status` `pending_review` block so the
/// two never drift.
///
/// `prompt` is the node's optional `prompt:` field (issue #102.9): free-form
/// author guidance for the reviewer, surfaced verbatim ahead of the options.
/// Template placeholders inside `prompt` are NOT rendered here - they are
/// carried through as literal text, exactly like the option strings.
pub fn review_instruction(
    node_id: &str,
    title: Option<&str>,
    options: &[String],
    prompt: Option<&str>,
) -> String {
    let label = title
        .map(str::trim)
        .filter(|t| !t.is_empty())
        .unwrap_or(node_id);
    let opts = apb_core::schema::effective_review_options(options).join(", ");
    let guidance = prompt
        .map(str::trim)
        .filter(|p| !p.is_empty())
        .map(|p| format!(" {p}"))
        .unwrap_or_default();
    format!(
        "The run is paused at the human-review gate \"{label}\" and needs your decision.{guidance} \
         Available decisions: {opts}. {}",
        review_how_to_decide(node_id)
    )
}

/// The decision mechanics for a publish gate: a consent carries the nonce
/// of the exact plan the person saw.
pub fn publish_how_to_decide(node_id: &str, nonce: &str) -> String {
    let node = apb_core::publish::shell_word(node_id);
    format!(
        "To publish exactly this plan, run `apb publish approve <run-id> --node {node} --nonce {nonce}` \
         or call the review_decide MCP tool with node `{node_id}`, decision `publish` and that nonce; \
         to decline, `apb publish skip <run-id> --node {node}` or decision `skip`."
    )
}

/// The owner-facing instruction of a publish gate: what would be pushed
/// where, over what the remote holds now, and how to answer.
#[allow(clippy::too_many_arguments)]
pub fn publish_instruction(
    node_id: &str,
    title: Option<&str>,
    url: &str,
    branch: &str,
    after: &str,
    remote_before: Option<&str>,
    commits: usize,
    outside_push_suspected: bool,
    prompt: Option<&str>,
    nonce: &str,
) -> String {
    let label = title
        .map(str::trim)
        .filter(|t| !t.is_empty())
        .unwrap_or(node_id);
    let short = |s: &str| s.chars().take(12).collect::<String>();
    let over = match remote_before {
        Some(b) => format!("over {}", short(b)),
        None => "as a new branch".to_string(),
    };
    let guidance = prompt
        .map(str::trim)
        .filter(|p| !p.is_empty())
        .map(|p| format!(" {p}"))
        .unwrap_or_default();
    let warning = if outside_push_suspected {
        " Warning: commits of this run are already on the remote, so something pushed them outside this step."
    } else {
        ""
    };
    format!(
        "The run is paused at the publish step \"{label}\" and needs your consent: push {commits} commit(s) up to {} to `{url}` branch `{branch}` {over}.{warning}{guidance} \
         Available decisions: publish, skip. {}",
        short(after),
        publish_how_to_decide(node_id, nonce)
    )
}

/// Supervisor options available after a node failure/timeout wake (issue #45
/// finding 4). Shared by the progress block and doctor so the lists never drift.
pub fn supervisor_decision_options() -> Vec<String> {
    vec![
        "retry".to_string(),
        "continue_from".to_string(),
        "abort".to_string(),
    ]
}

/// Decision mechanics for a supervised failure/timeout wake: how a supervisor
/// (or an operator) actually posts a command. Single source of the how-to
/// wording.
pub fn supervisor_how_to_decide(node_id: &str) -> String {
    format!(
        "To decide, use the supervisor tools: node_retry (re-run node `{node_id}`), \
         run_continue_from (advance from a chosen node), or run_abort."
    )
}

/// Owner/supervisor-facing pending instruction for a failure/timeout wake.
pub fn supervisor_instruction(node_id: &str, trigger: &str) -> String {
    let what = match trigger {
        "node_timeout" => "timed out",
        _ => "failed",
    };
    let opts = supervisor_decision_options().join(", ");
    format!(
        "The supervised run is waiting for a supervisor decision after node `{node_id}` {what}. \
         Available options: {opts}. {}",
        supervisor_how_to_decide(node_id)
    )
}
