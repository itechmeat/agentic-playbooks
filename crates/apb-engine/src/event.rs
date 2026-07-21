use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::error::EngineError;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WakeTrigger {
    NodeFailed,
    NodeTimeout,
    Anomaly,
}

/// Fingerprint of the profile used, for run provenance (spec 6.5).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProfileProvenance {
    pub scope: String,
    pub name: String,
    pub bundle_digest: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum EventPayload {
    RunStarted {
        playbook: String,
        version: String,
    },
    /// Origin and execution location of the run (spec 3). Written right
    /// after `RunStarted`. A separate event (rather than fields on
    /// `RunStarted`) so that old logs without provenance read unchanged, and
    /// existing matches on `RunStarted` remain untouched. All fields are
    /// Option: for local project runs `RunStarted` alone is enough,
    /// provenance fills in the picture for global and cross-workspace runs.
    RunProvenance {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        origin: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        digest: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        execution_root: Option<String>,
        /// Profiles used by the run (spec 6.5). Empty for playbooks without
        /// profiles (the executor path).
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        profiles: Vec<ProfileProvenance>,
    },
    NodeStarted {
        node: String,
        attempt: u32,
    },
    AttemptStarted {
        node: String,
        attempt: u32,
        agent: String,
        /// Actual SOUL delivery method used in this attempt (spec 6.3).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        soul_delivery: Option<String>,
        /// Actual method of providing skills in this attempt (completion-plan
        /// Task 3): `materialized` - skill copies in the node's isolated
        /// workdir; `advisory` - a pointer string with names in the shared workdir.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        skills_mode: Option<String>,
        /// OS process id of the spawned agent, captured at spawn time (from
        /// `child.id()`). Written when the attempt is journaled at spawn so a
        /// mid-attempt crash leaves an identifiable open attempt. `None` only
        /// for old logs: every path that spawns an agent - including the
        /// finish-answer composition - journals the attempt at spawn.
        #[serde(default)]
        pid: Option<u32>,
    },
    AttemptFinished {
        node: String,
        attempt: u32,
        status: String,
        /// Wall-clock milliseconds from the agent spawn to this attempt's
        /// return, measured from the spawn instant. `None` only for old logs:
        /// every path that spawns an agent - including the finish-answer
        /// composition - measures the attempt from its own spawn instant.
        #[serde(default)]
        duration_ms: Option<u64>,
        /// Agent session id captured from a finished attempt, for the
        /// `resume` transport (spec 2026-07-20-interactive-nodes, Transport:
        /// resume). `None` when the agent surfaced no session id or the
        /// transport does not resume. Additive.
        #[serde(default)]
        session: Option<String>,
        /// Display-only one-line summary the agent self-reported in its report
        /// block (spec 6.2, issue #42 finding 1). Kept here for humans; it is
        /// NEVER used as the node output (the reply body is - see
        /// `AgentReport::output`). `None` when the agent gave no summary or the
        /// attempt did not finish through a report. Additive.
        #[serde(default)]
        summary: Option<String>,
    },
    NodeFinished {
        node: String,
        status: String,
        attempt: u32,
        output: String,
        /// Declared node artifacts captured on execution (or replayed from the
        /// cache record on a hit). Additive to existing logs: old events carry
        /// no artifacts and deserialize with an empty list.
        #[serde(default)]
        artifacts: Vec<apb_core::cache::ArtifactRef>,
    },
    RetryStarted {
        node: String,
        attempt: u32,
    },
    FallbackTriggered {
        node: String,
        from: String,
        to: String,
        /// The node's profile (`<scope>/<name>`) within which the fallback occurred.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        profile: Option<String>,
    },
    RunPaused {
        reason: String,
    },
    /// A resume restarted the run from `from_node` (Task 3: resume rework).
    /// Folds to `Running`, replacing the old `RunPaused { reason: "resume
    /// from X" }` marker that used to leave the folded status stuck on paused
    /// for the rest of the run. Old journals that still carry that legacy
    /// `RunPaused` marker fold unchanged.
    RunResumed {
        from_node: String,
    },
    RunFinished {
        outcome: String,
    },
    WakeRaised {
        trigger: WakeTrigger,
        node: String,
        detail: String,
    },
    SupervisorAction {
        action: String,
        node: Option<String>,
        detail: String,
    },
    RunAborted {
        reason: String,
    },
    SupervisorLost {
        detail: String,
    },
    PatchApplied {
        version: String,
        classification: String,
        continue_from: String,
    },
    PatchRejected {
        reason: String,
    },
    RunMigrated {
        from_version: String,
        to_version: String,
        continue_from: String,
    },
    VersionPromoted {
        version: String,
    },
    ReviewRequested {
        node: String,
        options: Vec<String>,
    },
    ReviewDecided {
        node: String,
        decision: String,
        note: String,
    },
    WaitStarted {
        node: String,
        kind: String,
    },
    WaitSignalled {
        node: String,
    },
    WaitTimeout {
        node: String,
    },
    /// Old context sections have been compacted by a cheap model into a
    /// separate file (a materialized artifact outside the primary log). The
    /// event references the file, the model, and the up_to_seq boundary
    /// (sections with seq <= up_to_seq are represented by the summary,
    /// everything newer renders raw). The summary content is NOT written to
    /// the log - it is non-deterministic (LLM), which preserves replay
    /// determinism.
    ContextCompacted {
        compact_file: String,
        model: String,
        up_to_seq: u64,
    },
    /// An explicit cycle-progress report (spec 2026-07-17): the current
    /// iteration `done` of `total` for the cycle group anchored at `node_id`.
    /// Written by drive when it drains a `Control::Progress` command, never by a
    /// tool (single-writer). Fields default so old logs read unchanged.
    RunProgress {
        #[serde(default)]
        node_id: String,
        #[serde(default)]
        done: u64,
        #[serde(default)]
        total: u64,
        #[serde(default)]
        label: Option<String>,
    },
    /// A sub-playbook node started a full child run (spec C). Written by drive
    /// (via run_playbook_node) before it drives the child, so a resume can
    /// reattach to a still-running child by its `run_id`. Fields default so old
    /// logs read unchanged.
    ChildRunStarted {
        #[serde(default)]
        node_id: String,
        #[serde(default)]
        run_id: String,
    },
    /// This run continues from a predecessor run as a fresh run id (issue #42
    /// finding 10). Written when the lineage link is established.
    RunContinuedFrom {
        #[serde(default)]
        from: String,
    },
    /// A successor run has continued from this run (issue #42 finding 10).
    /// Written when the lineage link is established.
    RunSupersededBy {
        #[serde(default)]
        by: String,
    },
    /// Resume proceeded despite a change in the agent binary's fingerprint
    /// between start and resume (spec 3.6, `--allow-environment-drift`).
    /// Recorded in the log rather than swallowed silently.
    EnvironmentDriftAccepted {
        agent_id: String,
        was: String,
        now: String,
    },
    /// A connector call executed by `apb connector call` (spec
    /// 2026-07-18-connectors-design section 6.2). Records only outcome
    /// metadata, never request/response bodies. `url` is the URL rendered
    /// BEFORE auth injection (so `query`-kind auth never reaches the log) and
    /// is `""` for a mock function. Appended for calls that actually executed
    /// (mock or HTTP); never for a dry-run or a gate rejection (config,
    /// permission, invalid_args), so `max_calls` counts only real calls.
    /// Optional fields default so old logs read unchanged.
    ConnectorCall {
        #[serde(default)]
        node_id: String,
        #[serde(default)]
        connector: String,
        #[serde(default)]
        function: String,
        #[serde(default)]
        account: String,
        #[serde(default)]
        url: String,
        /// `"ok"` or the error code (`auth`, `rate_limited`, ...).
        #[serde(default)]
        outcome: String,
        #[serde(default)]
        http_status: Option<u16>,
        #[serde(default)]
        duration_ms: u64,
        /// SMTP-only: the message subject and total recipient count. `None`
        /// for HTTP and mock calls and for an smtp `verify`. Bodies and
        /// credentials are never recorded (spec 4.2).
        #[serde(default)]
        smtp_subject: Option<String>,
        #[serde(default)]
        smtp_recipients: Option<u32>,
    },
    /// Node cache (spec 2026-07-19-node-cache-design). A cache lookup for a
    /// cacheable node always ends in exactly one of `NodeCacheHit` or
    /// `NodeCacheMiss`; `NodeCacheStored`/`NodeCacheRejected` then report the
    /// post-execution admission decision on a miss. Additive variants: old logs
    /// read unchanged and never carry them.
    NodeCacheHit {
        node: String,
        key: String,
        /// The run that originally produced the cached result.
        source_run: String,
    },
    NodeCacheMiss {
        node: String,
        key: String,
    },
    NodeCacheStored {
        node: String,
        key: String,
    },
    NodeCacheRejected {
        node: String,
        reason: String,
    },
    /// A bounded loop edge (one carrying `max_traversals`) was traversed (spec
    /// 2026-07-20-run-reliability). Journaled ONLY for edges that carry
    /// `max_traversals`, so the journal stays lean: `RunState::fold` counts
    /// these per `(from, to)` into `edge_counts`, and edge selection blocks the
    /// edge once the count reaches its cap. A resume restores loop progress
    /// exactly because the counts come from the journal. Additive variant: old
    /// logs never carry it.
    EdgeTraversed {
        from: String,
        to: String,
    },
    /// An interactive node's agent asked the user a question (spec
    /// 2026-07-20-interactive-nodes). Written by drive when it observes a new
    /// `questions.jsonl` entry for the node (single-writer, like
    /// `ReviewRequested`). Additive variant: old logs never carry it.
    QuestionAsked {
        node: String,
        question: String,
        #[serde(default)]
        options: Vec<String>,
    },
    /// The N-th answer matched the N-th asked question for a node
    /// (count-based consumption, like `ReviewDecided`). `answered_by` is one
    /// of `"human"`, `"supervisor"`, `"timeout"`.
    QuestionAnswered {
        node: String,
        answer: String,
        answered_by: String,
    },
    /// An explanatory record for a run that is about to terminate abnormally
    /// (issue #42 finding 3): written immediately before a `run_finished`
    /// whose outcome is `"failed"` on every scheduler drive-loop path (no
    /// matching outgoing edge, a stalled resume, an exceeded step budget) and
    /// every prepare/refusal path (a missing or drifted connector permit, a
    /// profile bundle mismatch, a sub-playbook that failed to resolve or
    /// prepare) that would otherwise leave the log with no record of why.
    /// Carries the verbatim engine error text, and the node id when the
    /// failure is attributable to one node (`None` for a run-level failure,
    /// for example exceeding the step budget). `#[serde(default)]` on both
    /// fields: old logs never carry this variant at all, so there is nothing
    /// to default FROM, but a future additive field on it should still follow
    /// this convention.
    RunError {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        node: Option<String>,
        #[serde(default)]
        reason: String,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Event {
    pub seq: u64,
    pub ts: u128,
    #[serde(flatten)]
    pub payload: EventPayload,
}

pub fn now_millis() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0)
}

pub struct EventLog {
    file: File,
    next_seq: u64,
}

impl EventLog {
    pub fn create(run_dir: &Path) -> Result<Self, EngineError> {
        std::fs::create_dir_all(run_dir)?;
        Self::open(run_dir)
    }

    pub fn open(run_dir: &Path) -> Result<Self, EngineError> {
        let path = run_dir.join("events.jsonl");
        let next_seq = if path.is_file() {
            read_all(run_dir)?.last().map(|e| e.seq + 1).unwrap_or(0)
        } else {
            0
        };
        let file = OpenOptions::new().create(true).append(true).open(&path)?;
        Ok(Self { file, next_seq })
    }

    pub fn append(&mut self, payload: EventPayload) -> Result<Event, EngineError> {
        let event = Event {
            seq: self.next_seq,
            ts: now_millis(),
            payload,
        };
        let line = serde_json::to_string(&event).map_err(|e| EngineError::Yaml(e.to_string()))?;
        writeln!(self.file, "{line}")?;
        self.file.flush()?;
        self.next_seq += 1;
        Ok(event)
    }
}

pub fn read_all(run_dir: &Path) -> Result<Vec<Event>, EngineError> {
    let path = run_dir.join("events.jsonl");
    if !path.is_file() {
        return Ok(Vec::new());
    }
    let mut out = Vec::new();
    for line in BufReader::new(File::open(&path)?).lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let ev: Event =
            serde_json::from_str(&line).map_err(|e| EngineError::Yaml(e.to_string()))?;
        out.push(ev);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn question_asked_round_trips_with_snake_case_tag() {
        let payload = EventPayload::QuestionAsked {
            node: "ask".into(),
            question: "which way".into(),
            options: vec!["left".into(), "right".into()],
        };
        let line = serde_json::to_string(&payload).unwrap();
        assert!(
            line.contains("\"type\":\"question_asked\""),
            "expected question_asked tag, got {line}"
        );
        let back: EventPayload = serde_json::from_str(&line).unwrap();
        match back {
            EventPayload::QuestionAsked {
                node,
                question,
                options,
            } => {
                assert_eq!(node, "ask");
                assert_eq!(question, "which way");
                assert_eq!(options, vec!["left".to_string(), "right".to_string()]);
            }
            other => panic!("expected QuestionAsked, got {other:?}"),
        }
    }

    #[test]
    fn question_asked_options_default_to_empty_when_absent() {
        // Old-style payload without `options` at all must still deserialize
        // (additive field, spec: options with #[serde(default)]).
        let line = r#"{"type":"question_asked","node":"ask","question":"q"}"#;
        let back: EventPayload = serde_json::from_str(line).unwrap();
        match back {
            EventPayload::QuestionAsked { options, .. } => {
                assert_eq!(options, Vec::<String>::new());
            }
            other => panic!("expected QuestionAsked, got {other:?}"),
        }
    }

    #[test]
    fn question_answered_round_trips_with_snake_case_tag() {
        let payload = EventPayload::QuestionAnswered {
            node: "ask".into(),
            answer: "left".into(),
            answered_by: "human".into(),
        };
        let line = serde_json::to_string(&payload).unwrap();
        assert!(
            line.contains("\"type\":\"question_answered\""),
            "expected question_answered tag, got {line}"
        );
        let back: EventPayload = serde_json::from_str(&line).unwrap();
        match back {
            EventPayload::QuestionAnswered {
                node,
                answer,
                answered_by,
            } => {
                assert_eq!(node, "ask");
                assert_eq!(answer, "left");
                assert_eq!(answered_by, "human");
            }
            other => panic!("expected QuestionAnswered, got {other:?}"),
        }
    }

    #[test]
    fn attempt_finished_without_session_deserializes_to_none() {
        // An old log line, written before `session` existed.
        let line = r#"{"type":"attempt_finished","node":"a","attempt":1,"status":"succeeded"}"#;
        let back: EventPayload = serde_json::from_str(line).unwrap();
        match back {
            EventPayload::AttemptFinished { session, .. } => assert_eq!(session, None),
            other => panic!("expected AttemptFinished, got {other:?}"),
        }
    }

    #[test]
    fn attempt_finished_with_session_round_trips() {
        let payload = EventPayload::AttemptFinished {
            node: "a".into(),
            attempt: 1,
            status: "succeeded".into(),
            duration_ms: Some(42),
            session: Some("abc".into()),
            summary: Some("did the thing".into()),
        };
        let line = serde_json::to_string(&payload).unwrap();
        let back: EventPayload = serde_json::from_str(&line).unwrap();
        match back {
            EventPayload::AttemptFinished { session, .. } => {
                assert_eq!(session.as_deref(), Some("abc"));
            }
            other => panic!("expected AttemptFinished, got {other:?}"),
        }
    }

    #[test]
    fn run_error_round_trips_with_snake_case_tag() {
        let payload = EventPayload::RunError {
            node: Some("work".into()),
            reason: "node `work` has no outgoing edge and is not finish".into(),
        };
        let line = serde_json::to_string(&payload).unwrap();
        assert!(
            line.contains("\"type\":\"run_error\""),
            "expected run_error tag, got {line}"
        );
        let back: EventPayload = serde_json::from_str(&line).unwrap();
        match back {
            EventPayload::RunError { node, reason } => {
                assert_eq!(node.as_deref(), Some("work"));
                assert!(reason.contains("no outgoing edge"));
            }
            other => panic!("expected RunError, got {other:?}"),
        }
    }

    #[test]
    fn run_error_defaults_both_fields_when_absent() {
        // No existing log carries this variant at all (it is new), but the
        // additive-field convention still applies: a bare tag must still
        // deserialize.
        let line = r#"{"type":"run_error"}"#;
        let back: EventPayload = serde_json::from_str(line).unwrap();
        match back {
            EventPayload::RunError { node, reason } => {
                assert_eq!(node, None);
                assert_eq!(reason, "");
            }
            other => panic!("expected RunError, got {other:?}"),
        }
    }
}
