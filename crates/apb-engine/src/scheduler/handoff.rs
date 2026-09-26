//! Warm session handoff between nodes (issue #67 item 1): the decision
//! whether a node with `continue_session` can continue the agent session its
//! source node finished in, read from the run's own journal.

use super::*;

/// How a node with `continue_session` starts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Handoff {
    /// Continue session `id`, which ran in `workdir`.
    Warm { id: String, workdir: PathBuf },
    /// Start a fresh agent, for this reason.
    Cold(String),
}

/// What the journal says about the attempt a node's latest successful
/// execution finished with.
struct SourceAttempt {
    session: Option<String>,
    agent: String,
    model: Option<String>,
    workdir: Option<String>,
}

/// The attempt `source`'s latest successful execution succeeded with, or why
/// there is none to continue.
fn source_attempt(events: &[Event], source: &str) -> Result<SourceAttempt, String> {
    let finished = events
        .iter()
        .rposition(|e| {
            matches!(&e.payload, EventPayload::NodeFinished { node, status, .. }
                if node == source && status == NodeStatus::Succeeded.as_str())
        })
        .ok_or_else(|| format!("`{source}` has not succeeded in this run"))?;
    let (at, attempt, session) = events[..finished]
        .iter()
        .enumerate()
        .rev()
        .find_map(|(i, e)| match &e.payload {
            EventPayload::AttemptFinished {
                node,
                attempt,
                status,
                session,
                ..
            } if node == source && status == NodeStatus::Succeeded.as_str() => {
                Some((i, *attempt, session.clone()))
            }
            // An earlier execution's attempts are not this result's.
            EventPayload::NodeStarted { node, .. } if node == source => Some((usize::MAX, 0, None)),
            _ => None,
        })
        .filter(|(i, _, _)| *i != usize::MAX)
        .ok_or_else(|| {
            format!("`{source}` produced its result without an agent attempt (a cache hit)")
        })?;
    events[..at]
        .iter()
        .rev()
        .find_map(|e| match &e.payload {
            EventPayload::AttemptStarted {
                node,
                attempt: a,
                agent,
                model,
                workdir,
                ..
            } if node == source && *a == attempt => Some(SourceAttempt {
                session: session.clone(),
                agent: agent.clone(),
                model: model.clone(),
                workdir: workdir.clone(),
            }),
            _ => None,
        })
        .ok_or_else(|| format!("`{source}` left no record of its attempt"))
}

/// Whether the node about to start with executor `agent`/`model` in `dir`
/// can continue `source`'s session. Every requirement is checked against the
/// journal, so the answer is the same for a fresh run, a resume and a
/// `continue_from`.
pub(crate) fn decide(
    events: &[Event],
    source: &str,
    agent: &str,
    model: &str,
    dir: &Path,
    isolated: bool,
) -> Handoff {
    let src = match source_attempt(events, source) {
        Ok(s) => s,
        Err(why) => return Handoff::Cold(why),
    };
    let canonical = apb_core::detect::canonical_agent_id;
    if crate::invocation::resume_argv(agent).is_none() {
        return Handoff::Cold(format!("agent `{agent}` cannot resume a session"));
    }
    if canonical(&src.agent) != canonical(agent) || src.model.as_deref() != Some(model) {
        return Handoff::Cold(format!(
            "`{source}` ran on {} model {}, this node runs on {agent} model {model}",
            src.agent,
            src.model.as_deref().unwrap_or("unknown"),
        ));
    }
    let Some(id) = src.session else {
        return Handoff::Cold(format!("`{source}` recorded no session id"));
    };
    if isolated {
        return Handoff::Cold("an isolated node runs in its own directory".to_string());
    }
    let same_dir = src.workdir.as_deref().is_some_and(|w| {
        let w = Path::new(w);
        w == dir || w.canonicalize().ok() == dir.canonicalize().ok()
    });
    if !same_dir {
        return Handoff::Cold(format!(
            "`{source}` ran in {}, this node runs in {}",
            src.workdir.as_deref().unwrap_or("an unknown directory"),
            dir.display()
        ));
    }
    Handoff::Warm {
        id,
        workdir: dir.to_path_buf(),
    }
}

/// Whether any node of `playbook` continues `node_id`'s session, so its
/// successful attempts must leave a session id behind (an opencode session is
/// otherwise looked up only when a retry needs it).
pub(crate) fn is_source(playbook: &Playbook, node_id: &str) -> bool {
    playbook.nodes.iter().any(|n| {
        matches!(&n.kind, NodeKind::AgentTask { continue_session: Some(s), .. } if s == node_id)
    })
}

/// The line that opens a warm node's prompt: the session holds the earlier
/// step's work, and this is a new task in it.
pub(crate) const HANDOFF_PREAMBLE: &str = "Next step of the same run. Your earlier work in this session is context for it; the task for this step follows.";
