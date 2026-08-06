use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::EngineError;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WakeTrigger {
    NodeFailed,
    NodeTimeout,
    Anomaly,
}

/// `skip_serializing_if` helper for additive `bool` payload fields: a false
/// flag stays off the wire, so an event that does not use it serializes exactly
/// as it did before the field existed.
fn is_false(b: &bool) -> bool {
    !*b
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
        /// The agent's raw report text that a `success_check` rejected
        /// (spec field-report-robustness). A rejected success report is
        /// recorded as a `failed` attempt - it consumes a retry and advances
        /// the fallback chain like any other failure - but the discarded text
        /// is preserved here and folded into `RunState.rejected_outputs`, so a
        /// downstream node can read it via `nodes.<id>.rejected_output`.
        /// `None` for every attempt not rejected by a success_check, and for
        /// old logs. Additive.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        rejected_output: Option<String>,
        /// Whatever the attempt produced before it ended without recording a
        /// verdict (spec 2026-08-05 section 2.2): the agent's mid-work text on
        /// an `interrupted` attempt, or the adapter's failure detail when the
        /// process died. Kept so an interruption is observable and the work is
        /// not silently dropped; the next attempt is told to look for work
        /// already done rather than being handed this text. `None` for every
        /// attempt that recorded a verdict or a report, and for old logs.
        /// Additive.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        partial_output: Option<String>,
        /// How the attempt's failure was classified (spec 2026-08-05 section
        /// 2.3): `transient` (infrastructure, retried on the same executor
        /// after a backoff), `auth`, `budget` (both non-transient: no further
        /// retry on this step, same-agent fallback steps suppressed), or
        /// `agent`. `None` for a successful attempt, for a failure the agent
        /// itself reported through a verdict or a report block (a written
        /// verdict decides the attempt, so nothing is classified), for an
        /// attempt a supervisor interrupted, and for old logs. Additive.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        failure_kind: Option<String>,
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
        /// Model of the chain step that just failed, and model of the step taken
        /// instead (spec 2026-08-05 section 2.3, issue #74 finding 2). Without
        /// them a claude -> claude fallback that only changed the model reads
        /// like a pointless retry of the identical binding in the journal.
        /// `None` only for old logs. Additive.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        from_model: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        to_model: Option<String>,
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
    /// A supervisor rebound a node's executor profile mid-run (issue #45
    /// finding 5). The node's EFFECTIVE binding becomes `profile`
    /// (`<scope>/<name>`) for every future attempt, recorded in the journaled
    /// rebind overlay - the immutable run manifest stays intact as the record of
    /// what the run started with. `bundle` is the profile bundle digest that the
    /// policy gate trust-verified and that was re-verified from the run snapshot
    /// at apply time (anti-TOCTOU pinning). `reason` carries the supervisor's
    /// optional note, empty when none. Fields default so old logs read unchanged.
    ProfileRebound {
        #[serde(default)]
        node: String,
        #[serde(default)]
        profile: String,
        #[serde(default)]
        bundle: String,
        #[serde(default)]
        reason: String,
    },
    /// A mid-run profile rebind was refused at apply time (issue #45 finding 5):
    /// the new profile no longer resolves, or its bundle drifted from the digest
    /// the policy gate verified between gate and apply (TOCTOU). Non-terminal -
    /// the node keeps its existing binding, mirroring `PatchRejected`. Fields
    /// default so old logs read unchanged.
    RebindRejected {
        #[serde(default)]
        node: String,
        #[serde(default)]
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
        /// The gate node's title, copied from the playbook so a reader of the
        /// log alone can name the gate without the snapshot (issue #42 finding
        /// 4). `None` for a titleless node and for old logs. Additive.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        title: Option<String>,
        /// Owner-facing pending instruction (issue #42 finding 4): a single
        /// self-contained line naming the gate, its options, and how to decide
        /// (apb review CLI / review_decide MCP tool). A supervising agent
        /// relays this verbatim so the owner is never left waiting without
        /// knowing an action is expected. Empty for old logs. Additive.
        #[serde(default)]
        instruction: String,
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
    /// A node succeeded but a deliverable it DECLARED in `outputs.files` was not
    /// captured (spec 2026-08-05 section 2.6, issue #74 finding 4).
    ///
    /// A warning, never a failure: prompt-driven drift (the agent wrote
    /// `findings.md` where the playbook declared `report-*.md`) must be visible,
    /// but hard-failing a node on a glob is too brittle - the declaration is a
    /// statement of intent, not a contract the engine can verify semantically.
    /// `globs` carries the declaration verbatim so the journal shows what was
    /// expected without a reader having to fetch the playbook version.
    ///
    /// `detail` is `None` for the ordinary case (the globs matched no file) and
    /// carries the reason when capture itself failed (an unreadable match, a path
    /// escaping its scope root). Fields default per the additive convention; old
    /// logs never carry the variant at all.
    DeliverableMissing {
        #[serde(default)]
        node: String,
        #[serde(default)]
        globs: Vec<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        detail: Option<String>,
    },
    /// Every hop the drive loop actually took out of a node (spec
    /// 2026-07-20-run-reliability, widened by #82): a declared edge (bounded or
    /// not), or a `defaults.on_failure` policy hop that consulted no edge at
    /// all. `RunState::fold` puts every record into `journaled_hops`, and
    /// additionally counts it into `edge_counts` when it is neither a policy
    /// route nor `uncounted` - that is the single site a bounded edge's
    /// `max_traversals` budget is spent, unchanged from before. A resume
    /// restores loop progress exactly because the counts come from the
    /// journal.
    EdgeTraversed {
        from: String,
        to: String,
        /// True when the hop was taken by the `defaults.on_failure` policy
        /// rather than by a declared edge (spec 2026-08-05 section 1.5 /
        /// Task 4). The policy pushes its handler onto the frontier without
        /// consulting any edge, so nothing in the journal used to record where
        /// the run went and no reconstruction from the journal could see the
        /// handler (`parallel::pending_heads`). Recording it as a traversal
        /// makes it visible to that one reconstruction rather than duplicating
        /// the failure-policy predicate in a second place; the flag keeps the
        /// record honest about there being no such edge, and keeps the fold
        /// from spending a bounded edge's `max_traversals` budget on it.
        /// Additive: absent in every log written before, and omitted from the
        /// wire whenever false.
        #[serde(default, skip_serializing_if = "is_false")]
        via_policy: bool,
        /// True when this record must NOT consume a bounded edge's
        /// `max_traversals` budget: an unbounded declared edge (there is no cap
        /// to spend), or a hop journaled outside the single counting site in
        /// `advance_frontier`. Polarity is dictated by back-compatibility:
        /// every record written before this field existed was a counted bounded
        /// traversal, so the serde default has to read as "counted". Named
        /// `uncounted` rather than `unbounded` because the `max_loops` fallback
        /// hop may cross an edge that genuinely IS bounded while still needing
        /// not to change accounting. Additive: absent in every log written
        /// before, and omitted from the wire whenever false.
        #[serde(default, skip_serializing_if = "is_false")]
        uncounted: bool,
    },
    /// A join proceeded WITHOUT one or more of its declared inputs, because no
    /// node the run can still execute reaches them (spec 2026-08-05, Task 4).
    /// Written by drive at the moment it acts on the readiness verdict, listing
    /// every source written off for that decision.
    ///
    /// Its own variant rather than a `SupervisorAction`, for two reasons. It is
    /// engine bookkeeping, so a consumer that reads `SupervisorAction` as "a
    /// supervisor acted" (the dashboard's intervention journal does) would report
    /// a false class. And the same decision is legitimately journaled twice - a
    /// resume re-advancing through `advance_frontier`, or a loop re-entering an
    /// either-or fork - which `run_doctor`'s repeated-action check would read as a
    /// looping supervisor. Fields default per the additive convention; old logs
    /// never carry the variant at all.
    JoinInputDead {
        #[serde(default)]
        node: String,
        #[serde(default)]
        sources: Vec<String>,
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

pub struct EventLog {
    /// Absolute path of `events.jsonl` - retained so [`Self::resync_seq`] can
    /// re-read the on-disk high-water mark after another writer (a nested
    /// child mirroring a wake onto this parent run) has appended.
    path: PathBuf,
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
        Ok(Self {
            path,
            file,
            next_seq,
        })
    }

    /// Re-reads the last on-disk seq and advances `next_seq` past it when a
    /// concurrent append (child-to-parent wake mirror) raced ahead of this
    /// handle. Call after nested child work returns and before any further
    /// appends on a parent log that was open for the whole child drive.
    pub fn resync_seq(&mut self) -> Result<(), EngineError> {
        if let Some(last) = last_seq_on_disk(&self.path)? {
            let next = last.saturating_add(1);
            if next > self.next_seq {
                self.next_seq = next;
            }
        }
        Ok(())
    }

    pub fn append(&mut self, payload: EventPayload) -> Result<Event, EngineError> {
        let event = Event {
            seq: self.next_seq,
            ts: apb_core::clock::now_ms(),
            payload,
        };
        let line = serde_json::to_string(&event).map_err(|e| EngineError::Yaml(e.to_string()))?;
        writeln!(self.file, "{line}")?;
        self.file.flush()?;
        self.next_seq += 1;
        Ok(event)
    }
}

/// Last seq recorded in an events.jsonl file, if any.
fn last_seq_on_disk(path: &Path) -> Result<Option<u64>, EngineError> {
    if !path.is_file() {
        return Ok(None);
    }
    let mut last: Option<u64> = None;
    for line in BufReader::new(File::open(path)?).lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let ev: Event =
            serde_json::from_str(&line).map_err(|e| EngineError::Yaml(e.to_string()))?;
        last = Some(ev.seq);
    }
    Ok(last)
}

/// Mirrors a child-run wake into the parent run's event log so the parent's
/// `supervisor_wait_event` observes it (issue #45 finding 8). No-op when this
/// run has no `parent_run`. Best-effort: a missing or unreadable parent is
/// ignored so a forensics orphan cannot abort the child drive.
///
/// The mirrored event keeps the child's trigger, names the parent's playbook
/// node that started this child (falling back to `child_node`), and encodes
/// `child_run=<id> child_node=<node>: <detail>` in the detail so the
/// controlling agent can identify the nested run and node.
pub fn propagate_wake_to_parent(
    child_run_dir: &Path,
    trigger: WakeTrigger,
    child_node: &str,
    detail: &str,
) -> Result<(), EngineError> {
    let cfg = match crate::run_config::read_run_config(child_run_dir) {
        Ok(c) => c,
        Err(_) => return Ok(()),
    };
    let Some(parent_id) = cfg.parent_run.as_deref() else {
        return Ok(());
    };
    if !apb_core::registry::is_safe_segment(parent_id) {
        return Ok(());
    }
    let Some(runs_dir) = child_run_dir.parent() else {
        return Ok(());
    };
    let parent_dir = runs_dir.join(parent_id);
    if !parent_dir.is_dir() {
        return Ok(());
    }
    let child_run_id = child_run_dir
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    if child_run_id.is_empty() {
        return Ok(());
    }
    let parent_events = match read_all(&parent_dir) {
        Ok(e) => e,
        Err(_) => return Ok(()),
    };
    let parent_node = parent_events
        .iter()
        .rev()
        .find_map(|e| match &e.payload {
            EventPayload::ChildRunStarted { node_id, run_id } if run_id == &child_run_id => {
                Some(node_id.clone())
            }
            _ => None,
        })
        .unwrap_or_else(|| child_node.to_string());
    let mirrored_detail = format!("child_run={child_run_id} child_node={child_node}: {detail}");
    // Fresh handle: the parent drive holds its own EventLog open, so we rely on
    // the parent calling `resync_seq` after the child returns (see
    // `run_playbook_node`). Append-only + flush keeps both writers coherent.
    let mut parent_log = match EventLog::open(&parent_dir) {
        Ok(l) => l,
        Err(_) => return Ok(()),
    };
    parent_log.append(EventPayload::WakeRaised {
        trigger,
        node: parent_node,
        detail: mirrored_detail,
    })?;
    Ok(())
}

/// Journals a `WakeRaised` on this run and, when nested, mirrors it to the
/// parent run's supervisor channel (issue #45 finding 8).
pub fn raise_wake(
    run_dir: &Path,
    log: &mut EventLog,
    trigger: WakeTrigger,
    node: &str,
    detail: impl Into<String>,
) -> Result<(), EngineError> {
    let detail = detail.into();
    log.append(EventPayload::WakeRaised {
        trigger,
        node: node.to_string(),
        detail: detail.clone(),
    })?;
    // Propagation is best-effort for the parent; never fail the child on it.
    let _ = propagate_wake_to_parent(run_dir, trigger, node, &detail);
    Ok(())
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
    fn attempt_finished_without_rejected_output_deserializes_to_none() {
        // An old log line, written before `rejected_output` existed.
        let line = r#"{"type":"attempt_finished","node":"a","attempt":1,"status":"failed"}"#;
        let back: EventPayload = serde_json::from_str(line).unwrap();
        match back {
            EventPayload::AttemptFinished {
                rejected_output, ..
            } => assert_eq!(rejected_output, None),
            other => panic!("expected AttemptFinished, got {other:?}"),
        }
    }

    #[test]
    fn attempt_finished_with_rejected_output_round_trips() {
        let payload = EventPayload::AttemptFinished {
            node: "a".into(),
            attempt: 1,
            status: "failed".into(),
            duration_ms: Some(42),
            session: None,
            summary: None,
            rejected_output: Some("interim progress only".into()),
            partial_output: None,
            failure_kind: None,
        };
        let line = serde_json::to_string(&payload).unwrap();
        let back: EventPayload = serde_json::from_str(&line).unwrap();
        match back {
            EventPayload::AttemptFinished {
                rejected_output, ..
            } => assert_eq!(rejected_output.as_deref(), Some("interim progress only")),
            other => panic!("expected AttemptFinished, got {other:?}"),
        }
    }

    /// An old journal line, written before `failure_kind` existed, still parses
    /// (spec 2026-08-05 section 2.3: every new payload field is additive).
    #[test]
    fn attempt_finished_without_failure_kind_deserializes_to_none() {
        let line = r#"{"type":"attempt_finished","node":"a","attempt":1,"status":"failed"}"#;
        let back: EventPayload = serde_json::from_str(line).unwrap();
        match back {
            EventPayload::AttemptFinished { failure_kind, .. } => assert_eq!(failure_kind, None),
            other => panic!("expected AttemptFinished, got {other:?}"),
        }
    }

    #[test]
    fn attempt_finished_with_failure_kind_round_trips() {
        let payload = EventPayload::AttemptFinished {
            node: "a".into(),
            attempt: 1,
            status: "failed".into(),
            duration_ms: Some(42),
            session: None,
            summary: None,
            rejected_output: None,
            partial_output: None,
            failure_kind: Some("transient".into()),
        };
        let line = serde_json::to_string(&payload).unwrap();
        let back: EventPayload = serde_json::from_str(&line).unwrap();
        match back {
            EventPayload::AttemptFinished { failure_kind, .. } => {
                assert_eq!(failure_kind.as_deref(), Some("transient"));
            }
            other => panic!("expected AttemptFinished, got {other:?}"),
        }
    }

    /// Old `fallback_triggered` lines carry agent ids only; the models are
    /// additive and default to `None`.
    #[test]
    fn fallback_triggered_without_models_deserializes_to_none() {
        let line = r#"{"type":"fallback_triggered","node":"a","from":"claude","to":"claude-code"}"#;
        let back: EventPayload = serde_json::from_str(line).unwrap();
        match back {
            EventPayload::FallbackTriggered {
                from_model,
                to_model,
                ..
            } => {
                assert_eq!(from_model, None);
                assert_eq!(to_model, None);
            }
            other => panic!("expected FallbackTriggered, got {other:?}"),
        }
    }

    #[test]
    fn fallback_triggered_with_models_round_trips() {
        let payload = EventPayload::FallbackTriggered {
            node: "a".into(),
            from: "claude".into(),
            to: "claude".into(),
            profile: Some("project/main".into()),
            from_model: Some("haiku".into()),
            to_model: Some("opus".into()),
        };
        let line = serde_json::to_string(&payload).unwrap();
        let back: EventPayload = serde_json::from_str(&line).unwrap();
        match back {
            EventPayload::FallbackTriggered {
                from_model,
                to_model,
                ..
            } => {
                assert_eq!(from_model.as_deref(), Some("haiku"));
                assert_eq!(to_model.as_deref(), Some("opus"));
            }
            other => panic!("expected FallbackTriggered, got {other:?}"),
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
            rejected_output: None,
            partial_output: None,
            failure_kind: None,
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
