//! Channels for interactive `agent_task` questions (`questions.jsonl`) and
//! answers (`answers.jsonl`) (spec 2026-07-20-interactive-nodes). Mirrors
//! review.rs: only `drive` writes `events.jsonl` (`QuestionAsked`/
//! `QuestionAnswered`); every facade that can raise or resolve a question
//! (the `__ask-server` sidecar, MCP `run_answer`, `apb answer`, the web API)
//! appends to these channels instead, and drive observes new entries when it
//! parks or resumes an interactive node.
//!
//! The `answer_by` policy (spec Exact answer semantics) is enforced here, in
//! `post_answer`, rather than in each facade: every caller - MCP, CLI, web -
//! goes through this one function, so the policy cannot be bypassed by
//! adding a new facade that forgets to check it.

use std::collections::BTreeMap;
use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::Path;

use apb_core::schema::{AnswerBy, NodeKind};
use serde::{Deserialize, Serialize};

use crate::error::EngineError;
// Sourced from `legacy_snapshot` rather than `progress` (its public
// re-export path) to avoid a mutual module cycle: `progress.rs` also depends
// on this module for its channel reads (spec 2026-07-20, Task 5
// dependency-cycle fix).
use crate::legacy_snapshot::load_run_playbook;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PostedQuestion {
    pub seq: u64,
    pub node: String,
    pub attempt: u32,
    pub question: String,
    #[serde(default)]
    pub options: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PostedAnswer {
    pub seq: u64,
    pub node: String,
    pub answer: String,
    pub answered_by: String,
}

/// Appends a question to `questions.jsonl`. `seq` is the current line count,
/// matching `post_review`'s numbering (0, 1, 2, ...; never reused, even
/// across process restarts, since it is recomputed from the file itself).
pub fn post_question(
    run_dir: &Path,
    node: &str,
    attempt: u32,
    question: &str,
    options: Vec<String>,
) -> Result<u64, EngineError> {
    std::fs::create_dir_all(run_dir)?;

    let seq = read_questions_after(run_dir, None)?.len() as u64;

    let entry = PostedQuestion {
        seq,
        node: node.to_string(),
        attempt,
        question: question.to_string(),
        options,
    };
    let line = serde_json::to_string(&entry).map_err(|e| EngineError::Yaml(e.to_string()))?;

    let path = run_dir.join("questions.jsonl");
    let mut file = OpenOptions::new().create(true).append(true).open(&path)?;
    writeln!(file, "{line}")?;
    file.flush()?;

    Ok(seq)
}

pub fn read_questions_after(
    run_dir: &Path,
    after_seq: Option<u64>,
) -> Result<Vec<PostedQuestion>, EngineError> {
    let path = run_dir.join("questions.jsonl");
    if !path.is_file() {
        return Ok(Vec::new());
    }

    let mut out = Vec::new();
    for line in BufReader::new(File::open(&path)?).lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let entry: PostedQuestion =
            serde_json::from_str(&line).map_err(|e| EngineError::Yaml(e.to_string()))?;

        if let Some(threshold) = after_seq {
            if entry.seq > threshold {
                out.push(entry);
            }
        } else {
            out.push(entry);
        }
    }
    Ok(out)
}

/// Resolves the target node for an answer. An explicit `node` is used
/// verbatim (no existence check here: an unknown node simply cannot have a
/// pending question, so the append below is harmless and the drive loop
/// will never observe a matching `QuestionAsked` count for it). `None`
/// resolves to the single node whose asked-question count exceeds its
/// answered-answer count; zero or more than one such node is an error, since
/// there is nothing sensible to default to.
fn resolve_pending_node(run_dir: &Path, node: Option<&str>) -> Result<String, EngineError> {
    if let Some(explicit) = node {
        return Ok(explicit.to_string());
    }

    let mut asked: BTreeMap<String, u64> = BTreeMap::new();
    for q in read_questions_after(run_dir, None)? {
        *asked.entry(q.node).or_insert(0) += 1;
    }
    let mut answered: BTreeMap<String, u64> = BTreeMap::new();
    for a in read_answers_after(run_dir, None)? {
        *answered.entry(a.node).or_insert(0) += 1;
    }

    let pending: Vec<&String> = asked
        .iter()
        .filter(|(node, count)| **count > answered.get(*node).copied().unwrap_or(0))
        .map(|(node, _)| node)
        .collect();

    match pending.as_slice() {
        [only] => Ok((*only).clone()),
        [] => Err(EngineError::NotFound(
            "no pending question to answer (specify a node explicitly)".into(),
        )),
        many => Err(EngineError::Invalid(format!(
            "{} nodes have a pending question ({}); specify one explicitly",
            many.len(),
            many.iter()
                .map(|s| s.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        ))),
    }
}

/// The `answer_by` a node declares in the run's immutable playbook snapshot.
/// Defaults to `AnswerBy::Human` when the snapshot or the node is missing:
/// an unknown node cannot be answered by a supervisor (fail safe, spec
/// Security section).
fn answer_by_for(run_dir: &Path, node: &str) -> AnswerBy {
    let Some(pb) = load_run_playbook(run_dir) else {
        return AnswerBy::Human;
    };
    pb.node(node)
        .and_then(|n| match &n.kind {
            NodeKind::AgentTask { answer_by, .. } => Some(*answer_by),
            _ => None,
        })
        .unwrap_or(AnswerBy::Human)
}

/// Appends an answer to `answers.jsonl`, after enforcing the `answer_by`
/// policy: a node declaring `answer_by: human` rejects an answer arriving
/// through the supervisor-token path (`answered_by == "supervisor"`). Every
/// facade (MCP `run_answer`, `apb answer`, the web API) calls this function,
/// so the policy applies uniformly regardless of caller.
pub fn post_answer(
    run_dir: &Path,
    node: Option<&str>,
    answer: &str,
    answered_by: &str,
) -> Result<u64, EngineError> {
    let target = resolve_pending_node(run_dir, node)?;
    if answered_by == "supervisor" && answer_by_for(run_dir, &target) == AnswerBy::Human {
        return Err(EngineError::Invalid(format!(
            "node `{target}` is answer_by: human; relay this question to the user and post their answer verbatim rather than answering as the supervisor"
        )));
    }

    std::fs::create_dir_all(run_dir)?;
    let seq = read_answers_after(run_dir, None)?.len() as u64;

    let entry = PostedAnswer {
        seq,
        node: target,
        answer: answer.to_string(),
        answered_by: answered_by.to_string(),
    };
    let line = serde_json::to_string(&entry).map_err(|e| EngineError::Yaml(e.to_string()))?;

    let mut f = OpenOptions::new()
        .create(true)
        .append(true)
        .open(run_dir.join("answers.jsonl"))?;
    writeln!(f, "{line}")?;
    f.flush()?;

    Ok(seq)
}

pub fn read_answers_after(
    run_dir: &Path,
    after_seq: Option<u64>,
) -> Result<Vec<PostedAnswer>, EngineError> {
    let path = run_dir.join("answers.jsonl");
    if !path.is_file() {
        return Ok(Vec::new());
    }

    let mut out = Vec::new();
    for line in BufReader::new(File::open(&path)?).lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let entry: PostedAnswer =
            serde_json::from_str(&line).map_err(|e| EngineError::Yaml(e.to_string()))?;

        if let Some(threshold) = after_seq {
            if entry.seq > threshold {
                out.push(entry);
            }
        } else {
            out.push(entry);
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn post_and_read_questions_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let s0 = post_question(
            dir.path(),
            "ask",
            1,
            "what next",
            vec!["a".into(), "b".into()],
        )
        .unwrap();
        let s1 = post_question(dir.path(), "ask", 1, "anything else", Vec::new()).unwrap();
        assert_eq!(s0, 0);
        assert_eq!(s1, 1);

        let all = read_questions_after(dir.path(), None).unwrap();
        assert_eq!(all.len(), 2);
        assert_eq!(all[0].node, "ask");
        assert_eq!(all[0].attempt, 1);
        assert_eq!(all[0].question, "what next");
        assert_eq!(all[0].options, vec!["a".to_string(), "b".to_string()]);
        // Empty `options` round-trips as an empty vec, not an error or null.
        assert_eq!(all[1].options, Vec::<String>::new());

        let after = read_questions_after(dir.path(), Some(s0)).unwrap();
        assert_eq!(after.len(), 1);
        assert_eq!(after[0].question, "anything else");
    }

    #[test]
    fn post_and_read_answers_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let s0 = post_answer(dir.path(), Some("ask"), "go left", "human").unwrap();
        let s1 = post_answer(dir.path(), Some("ask2"), "42", "timeout").unwrap();
        assert_eq!(s0, 0);
        assert_eq!(s1, 1);

        let all = read_answers_after(dir.path(), None).unwrap();
        assert_eq!(all.len(), 2);
        assert_eq!(all[0].node, "ask");
        assert_eq!(all[0].answer, "go left");
        assert_eq!(all[0].answered_by, "human");

        let after = read_answers_after(dir.path(), Some(s0)).unwrap();
        assert_eq!(after.len(), 1);
        assert_eq!(after[0].node, "ask2");
        assert_eq!(after[0].answered_by, "timeout");
    }

    #[test]
    fn post_answer_resolves_the_single_pending_node_when_node_is_none() {
        let dir = tempfile::tempdir().unwrap();
        post_question(dir.path(), "ask", 1, "q", Vec::new()).unwrap();

        let seq = post_answer(dir.path(), None, "hi", "human").unwrap();
        assert_eq!(seq, 0);

        let answers = read_answers_after(dir.path(), None).unwrap();
        assert_eq!(answers.len(), 1);
        assert_eq!(answers[0].node, "ask");
    }

    #[test]
    fn post_answer_with_node_none_requires_exactly_one_pending() {
        let dir = tempfile::tempdir().unwrap();

        // Zero pending questions: nothing to default to.
        let err = post_answer(dir.path(), None, "hi", "human").unwrap_err();
        match err {
            EngineError::NotFound(_) => {}
            other => panic!("expected NotFound for zero pending questions, got {other:?}"),
        }

        // Two different nodes pending: ambiguous.
        post_question(dir.path(), "ask1", 1, "q1", Vec::new()).unwrap();
        post_question(dir.path(), "ask2", 1, "q2", Vec::new()).unwrap();
        let err = post_answer(dir.path(), None, "hi", "human").unwrap_err();
        match err {
            EngineError::Invalid(_) => {}
            other => panic!("expected Invalid for ambiguous pending questions, got {other:?}"),
        }

        // Answering one leaves exactly one pending, so `None` now resolves.
        post_answer(dir.path(), Some("ask1"), "done", "human").unwrap();
        let seq = post_answer(dir.path(), None, "hi", "human").unwrap();
        let answers = read_answers_after(dir.path(), Some(seq.saturating_sub(1))).unwrap();
        assert_eq!(answers.last().unwrap().node, "ask2");
    }
}
