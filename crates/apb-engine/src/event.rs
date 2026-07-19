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
    },
    AttemptFinished {
        node: String,
        attempt: u32,
        status: String,
    },
    NodeFinished {
        node: String,
        status: String,
        attempt: u32,
        output: String,
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
