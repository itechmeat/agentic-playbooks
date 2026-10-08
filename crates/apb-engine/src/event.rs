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

/// The `action` names of [`EventPayload::SupervisorAction`] that more than
/// one site writes or reads: the drive writes them, the progress fold and the
/// context assembler read them, so each is spelled once.
pub mod supervisor_action {
    /// A supervisor restarted a failed node.
    pub const NODE_RETRY: &str = "node_retry";
    /// A supervisor moved the run on from another node.
    pub const RUN_CONTINUE_FROM: &str = "run_continue_from";
    /// A note appended to the run context.
    pub const CONTEXT_APPEND: &str = "context_append";
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
        /// The content digest of the run directory's `scripts/` copy as the
        /// start verified it (`none` without scripts). Goal `script`
        /// criteria run only while the copy still matches it (C1).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        scripts_digest: Option<String>,
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
        /// Wall-clock milliseconds the process spawn itself took, measured
        /// around `spawn_in_group` and before any of the child's work runs.
        /// `duration_ms` on the matching `AttemptFinished` covers the whole
        /// attempt and cannot separate a slow agent from an OS that took tens of
        /// seconds just to start the process (a first-exec security scan is the
        /// case this exists for). `None` only for old logs, and for a spawn that
        /// failed before the callback could run.
        #[serde(default)]
        spawn_ms: Option<u64>,
        /// The model of the executor this attempt ran (issue #67 item 1): with
        /// `agent`, the binding a later node's `continue_session` must match to
        /// continue this attempt's session. `None` for old logs and for
        /// attempts journaled outside an agent_task.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        model: Option<String>,
        /// The directory the attempt's process ran in (issue #67 items 1 and
        /// 4): the node's resolved `workdir`, an isolated node's own directory,
        /// or the execution root. `None` for old logs and for attempts
        /// journaled outside an agent_task.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        workdir: Option<String>,
        /// The run-relative directory holding this attempt's raw output
        /// (issue #67 item 10): `stdout.log` and `stderr.log` as the agent
        /// printed them and, for claude, `session.jsonl`, the CLI's own
        /// transcript of the session with every tool call. `None` for old
        /// logs and for attempts journaled outside an agent_task.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        transcript: Option<String>,
    },
    /// The attempt's agent process has exited and the drive is finishing the
    /// attempt: reading its status file, capturing its session, running the
    /// node's `success_check`. The matching `AttemptFinished` follows (issue
    /// #107). Until then the attempt is still open and its pid is gone, and
    /// this event is what tells a reader that the drive saw the exit itself,
    /// so a live drive's attempt reads as running, not lost. Written only for
    /// an attempt whose spawn was journaled with a pid.
    AttemptExited {
        node: String,
        attempt: u32,
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
        /// Token usage the agent CLI reported for this attempt in its machine
        /// output (issue #167, see `apb_core::agent_output`). `None` when the
        /// output reported none (plain-text agents, custom invocation forms,
        /// an attempt that ended without a parsable result), and for old
        /// logs: never an estimate made by apb. Additive. Read leniently: a
        /// usage block this binary cannot decode (a newer apb's shape) reads
        /// as `None` rather than failing the whole journal.
        #[serde(
            default,
            skip_serializing_if = "Option::is_none",
            deserialize_with = "lenient_usage"
        )]
        usage: Option<apb_core::agent_output::AgentUsage>,
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
        /// Why the chain moved on when it was not an ordinary failure:
        /// `routing` for a tier-routing cascade (issue #165 Part 14.5), when
        /// an agent failure on a routed lower tier goes up a tier before the
        /// profile's own fallbacks. `None` for every ordinary fallback and for
        /// old logs. Additive.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reason: Option<String>,
    },
    RunPaused {
        reason: String,
    },
    /// The run was ADMITTED while another write-run held the shared workdir
    /// lock, and its first node cannot start until that holder releases it.
    ///
    /// Not a `RunPaused`: paused is a resting state a human or a supervisor
    /// has to act on, and every observer that watches for it (`apb runs`, the
    /// supervisor wait, the dashboard) would report the run as stopped. A
    /// queued run is running as far as its caller is concerned - it simply has
    /// not reached node one yet - so this folds to nothing and leaves the
    /// status at `Running`. The wait ends at the next event in the journal:
    /// the first `NodeStarted` when the lock is granted, or `RunError` +
    /// `RunFinished` when the queue wait runs out. `reason` carries the
    /// blocking holder verbatim. Additive: old logs never carry the variant.
    RunQueued {
        #[serde(default)]
        reason: String,
    },
    /// The run's working tree was resolved (issue #67 item 8): from here on
    /// every agent_task and script without its own `workdir` runs in `path`
    /// (absolute, canonical), and the run's busy lock covers that tree.
    /// `source` says where it came from: `caller` (passed at start),
    /// `playbook` (the `worktree` template over params, at start) or `node`
    /// (the template over `node`'s output, when that node succeeded). Written
    /// once per run; the fold keeps the first. Additive: old logs never carry
    /// the variant, and a run without it runs in the execution root.
    WorktreeResolved {
        path: String,
        source: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        node: Option<String>,
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
        /// The decision model's recommendation for a park wake (issue #165
        /// Part 10), present only when `supervisor_triage` is in advise or
        /// enforce for the run and the model answered. Advisory unless
        /// `applied`. Additive: old logs and every other wake carry none, and
        /// a shape this binary cannot read is dropped.
        #[serde(
            default,
            skip_serializing_if = "Option::is_none",
            deserialize_with = "lenient_option"
        )]
        triage: Option<WakeTriage>,
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
    // --- candidate trials (issue #192) ---
    /// This run was a trial of the playbook's candidate version (a forward
    /// patch) and its success promoted the candidate per
    /// `promote_supervisor_patches`: `current` now points at `version`.
    /// `successes` counts the trial runs it passed. Written right before
    /// `run_finished`, so it is safe to skip up to that checkpoint.
    CandidatePromoted {
        #[serde(default)]
        version: String,
        #[serde(default)]
        run_id: String,
        #[serde(default)]
        successes: u32,
    },
    /// This trial run of the candidate `version` failed (or a goal criterion
    /// did not hold): the candidate pointer was dropped, so the next runs use
    /// `current` again. Written right before `run_finished`, safe to skip up
    /// to that checkpoint.
    CandidateRejected {
        #[serde(default)]
        version: String,
        #[serde(default)]
        run_id: String,
        #[serde(default)]
        reason: String,
    },
    // --- end candidate trials ---
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
        /// The gate node's optional `prompt:` field, copied from the playbook
        /// (issue #102.9): free-form guidance for the reviewer, already
        /// folded into `instruction` but also carried on its own so a reader
        /// (the web review panel) can render it separately, above the
        /// options. `None` for a promptless node and for old logs. Additive.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        prompt: Option<String>,
        /// The decision model's advisory recommendation for this visit
        /// (issue #165 Part 11), present only when `review_triage` is in
        /// advise or enforce for the run and the model answered. Never
        /// preselected and never applied by itself. Additive.
        #[serde(
            default,
            skip_serializing_if = "Option::is_none",
            deserialize_with = "lenient_option"
        )]
        recommendation: Option<ReviewRecommendation>,
    },
    ReviewDecided {
        node: String,
        decision: String,
        note: String,
    },
    /// An open review request that will never be decided: a supervisor's
    /// directive (`node_retry`, `run_continue_from`) moved the run off the
    /// gate while it waited. It closes the request like a decision would, so
    /// nothing reports the gate as pending any more, and reaching the gate
    /// again asks anew. `reason` names the directive. Additive variant: old
    /// logs never carry it.
    ReviewWithdrawn {
        node: String,
        #[serde(default)]
        reason: String,
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
    /// One decision a decision model was asked (issue #165 Part 3), written
    /// by the decision runner through the attempt journal BEFORE anything
    /// reads the answer, so a resumed run replays it instead of asking again.
    /// In shadow mode nothing acts on it at all (`applied` stays false).
    ///
    /// Safe to skip up to the next checkpoint: an older apb that does not know
    /// the type loses only this record. The attempt it belongs to still ends
    /// with its own `attempt_finished` and `node_finished`, whose status and
    /// output a shadow decision never changes.
    ///
    /// Carries no key, no request or reply body and no state text: the state
    /// is identified by `state_digest`, and the redacted state itself is kept
    /// only in `runs/<id>/decisions/<seq>.json` when `privacy.debug_state` is
    /// on. Optional fields default so a shape a newer apb writes still reads.
    DecisionMade {
        use_site: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        node: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        attempt: Option<u32>,
        /// The configured id of the provider that answered (or was asked last).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        provider: Option<String>,
        /// The model that answered, as the provider named it.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        model: Option<String>,
        #[serde(default)]
        calibrated: bool,
        /// The use's effective mode when it was asked.
        #[serde(default)]
        mode: String,
        #[serde(default)]
        questions_digest: String,
        #[serde(default)]
        state_digest: String,
        /// Bytes of the state as sent (after redaction and clipping).
        #[serde(default)]
        state_bytes: u64,
        /// Characters of the output-class text the use judged, before
        /// redaction and clipping (the completion check: the attempt's raw
        /// reply). The report reads it to set long outputs apart; absent in
        /// journals written before it existed.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        output_chars: Option<u64>,
        /// Compact answers by question id: the value, its probability and the
        /// confidence. Full distributions only in the debug state file.
        #[serde(default, deserialize_with = "lenient_default")]
        answers: std::collections::BTreeMap<String, DecisionAnswer>,
        /// Whether engine behaviour changed because of this answer. Never in
        /// shadow.
        #[serde(default)]
        applied: bool,
        /// Whether a mode above shadow would have changed behaviour.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        would_change: Option<bool>,
        /// A code-only verdict on the same input, recorded for comparison.
        #[serde(
            default,
            skip_serializing_if = "Option::is_none",
            deserialize_with = "lenient_option"
        )]
        baseline: Option<DecisionBaseline>,
        #[serde(default)]
        latency_ms: u64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        input_tokens: Option<u64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        cost_usd: Option<f64>,
        /// `cost_usd` comes from the price table, not the provider.
        #[serde(default, skip_serializing_if = "is_false")]
        cost_estimated: bool,
        /// Answered from the run's cache: no request was made.
        #[serde(default)]
        cached: bool,
        /// `unavailable`, `timeout`, `rate_limited`, `auth`, `budget`,
        /// `invalid`, `cancelled` (the run stopped the ask; a resume asks
        /// again), or `None` when answered. Never a key or a body.
        #[serde(default)]
        error: Option<String>,
        // --- labels and enforce (issue #165 Parts 9-14) ---
        /// Why an enforce-mode decision acted as advise instead:
        /// `no_threshold` (none stored for this use, provider and model),
        /// `uncalibrated`, `cap` (the use spent its automatic actions for
        /// the run), `not_opted_in` (the playbook did not opt in),
        /// `effects` (the gate's effects forbid an automatic decision) or
        /// `no_retry` (a completion rejection would have no retry to
        /// consume).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        enforce_refused: Option<String>,
        /// Join keys a report labels the decision by: `wake_seq`,
        /// `gate_visit`, `profile`, `tier`, `retries_left` and similar.
        #[serde(
            default,
            skip_serializing_if = "std::collections::BTreeMap::is_empty",
            deserialize_with = "lenient_default"
        )]
        join: std::collections::BTreeMap<String, serde_json::Value>,
        // --- end labels and enforce ---
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
    /// A successful node's output lacks fields it declares in
    /// `outputs.fields` (issue #67 item 4): the output is not a JSON object,
    /// or the object has no such key. A warning only; the node keeps its
    /// status. `fields` lists the missing ones in declaration order.
    OutputFieldsMissing {
        #[serde(default)]
        node: String,
        #[serde(default)]
        fields: Vec<String>,
    },
    /// How a node with `continue_session` started (issue #67 item 1): `warm`
    /// when its first attempt continued the session of `from_node`, else cold
    /// (a fresh agent, as without the field) with the `reason`. Journaled once
    /// per execution, before the first attempt.
    SessionHandoff {
        #[serde(default)]
        node: String,
        #[serde(default)]
        from_node: String,
        #[serde(default)]
        warm: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reason: Option<String>,
    },
    // --- host execution mode (0.23.0) ---
    /// An agent attempt of a host-mode run became a host task (see
    /// `crate::host_task`): the engine spawned nothing and waits for the host
    /// session that started the run to execute `task_id` with its own
    /// subagent and submit the reply. Journaled by the drive before it parks.
    /// The prompt texts live in the run directory (`prompt_ref`,
    /// `role_prompt_ref`, relative to it), never in the event.
    ///
    /// Safe to skip up to the next checkpoint: an older apb that does not know
    /// the type loses only this record. The attempt it belongs to still ends
    /// with its own `attempt_finished` and `node_finished`. Every field
    /// defaults, so a shape a newer apb writes still reads.
    HostTaskRequested {
        #[serde(default)]
        task_id: String,
        #[serde(default)]
        node: String,
        #[serde(default)]
        attempt: u32,
        #[serde(default)]
        prompt_ref: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        role_prompt_ref: Option<String>,
        /// Paths of the skills the step may load (materialized from the run
        /// snapshot).
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        skills: Vec<String>,
        #[serde(default)]
        workdir: String,
        /// The node's declared `outputs` contract, as JSON.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        outputs: Option<serde_json::Value>,
        /// Wall-clock milliseconds by which the task must be submitted (the
        /// node's timeout); `None` without a timeout.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        deadline_ms: Option<u64>,
        /// The model a fallback entry or tier routing declares for this
        /// chain step; a hint the host may ignore (the host picks its own
        /// model). The profile's own executor gives none.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        model_hint: Option<String>,
        /// Which step of the profile's executor chain the task runs (its
        /// primary, a fallback entry or a routed tier): the label of
        /// `model_hint`. `None` for old logs. Additive, read leniently.
        #[serde(
            default,
            skip_serializing_if = "Option::is_none",
            deserialize_with = "lenient_option"
        )]
        hint_source: Option<crate::host_task::hint::HintSource>,
        /// For the first task of a later chain step: the attempt that closed
        /// the previous step and how (`failed`, `expired`, ...), so the
        /// journal says why this task exists before `fallback_triggered`
        /// lands at the node's end. Additive, read leniently.
        #[serde(
            default,
            skip_serializing_if = "Option::is_none",
            deserialize_with = "lenient_option"
        )]
        fallback_of: Option<crate::host_task::hint::FallbackOf>,
        /// `model_hint`, `hint_source` and `fallback_of` as the one English
        /// line every surface shows (the same text as a pending task's
        /// `hint_note`). `None` for old logs. Additive.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        hint_note: Option<String>,
    },
    /// A host task was closed: the host submitted it (`submitted_by: host`,
    /// `client` names the MCP host), or the engine closed it (`submitted_by:
    /// engine`, status `expired`, `cancelled`, `interrupted` or `superseded`, the last when a resume re-exposed another open task of the node). The reply
    /// text lives in the run directory (`output_ref`).
    ///
    /// Safe to skip up to the next checkpoint: an older apb that does not know
    /// the type loses only this record; the attempt still ends with its own
    /// `attempt_finished`. Every field defaults.
    HostTaskSubmitted {
        #[serde(default)]
        task_id: String,
        /// `succeeded`, `failed`, `blocked`, or an engine closure.
        #[serde(default)]
        status: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        output_ref: Option<String>,
        /// Token usage the host reported, if any (`source: reported`). Read
        /// leniently like `attempt_finished.usage`.
        #[serde(
            default,
            skip_serializing_if = "Option::is_none",
            deserialize_with = "lenient_usage"
        )]
        usage: Option<apb_core::agent_output::AgentUsage>,
        #[serde(default)]
        submitted_by: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        client: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        note: Option<String>,
        /// The model the host reports it actually ran the task on, when it
        /// says so; never inferred by apb. Additive.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        model: Option<String>,
    },
    /// A `cli` run started by an MCP host session could not start any CLI
    /// of an agent step's chain (every binary missing, or not logged in), so
    /// the step continues as a host task (see `crate::host_task`). `attempt`
    /// is the host task's attempt; `reason` the last start failure. Later
    /// steps keep using their CLIs.
    ///
    /// Safe to skip up to the next checkpoint: an older apb that does not know
    /// the type loses only this record; the attempt still ends with its own
    /// `attempt_finished`. Every field defaults.
    ExecutionFallback {
        #[serde(default)]
        node: String,
        #[serde(default)]
        attempt: u32,
        #[serde(default)]
        reason: String,
    },
    // --- end host execution mode ---
    DeliverableMissing {
        #[serde(default)]
        node: String,
        #[serde(default)]
        globs: Vec<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        detail: Option<String>,
    },
    // --- 0.23.0: run provenance, goal criteria, protected paths ----------
    /// A node moved `HEAD` of the git tree it ran in (C7): the commits it
    /// made, newest first. Written only on a git tree with a commit and only
    /// when `HEAD` moved forward on the branch the node started on, just before the node's `node_finished`, so it is
    /// safe to skip up to that checkpoint: it records history, the engine
    /// never reads it back. `omitted` counts commits past the listed ones.
    ArtifactsCommitted {
        #[serde(default)]
        node: String,
        #[serde(default)]
        before: String,
        #[serde(default)]
        after: String,
        #[serde(default)]
        commits: Vec<CommittedArtifact>,
        #[serde(default, skip_serializing_if = "is_zero")]
        omitted: usize,
    },
    /// One goal criterion checked when the run reached a finish node (C1):
    /// `check` is `script`, `marker` or `manual`; `status` is `passed`,
    /// `failed`, `manual` (left to a person) or `error` (the check could not
    /// run), with `detail` saying why. `enforced` marks a script or marker
    /// criterion under `goal.enforce: true`. Written before the finish
    /// node's `node_finished`, so it is safe to skip up to that checkpoint;
    /// an enforced failure also journals a `run_error` there.
    GoalChecked {
        #[serde(default)]
        index: usize,
        #[serde(default)]
        description: String,
        #[serde(default)]
        check: String,
        #[serde(default)]
        status: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        detail: Option<String>,
        #[serde(default, skip_serializing_if = "is_false")]
        enforced: bool,
    },
    /// An agent_task attempt changed files its node protects (C6): each
    /// path with `modified`, `deleted` or `added`. The engine restored them
    /// from its pre-attempt copy (`restore_failed` names any it could not)
    /// and a reported success was rejected. Written before the attempt's
    /// `attempt_finished`; the next checkpoint is the node's
    /// `node_finished`, and it is safe to skip up to there: the attempt's
    /// own result carries the effect.
    ProtectedPathsModified {
        #[serde(default)]
        node: String,
        #[serde(default)]
        attempt: u32,
        #[serde(default)]
        changes: Vec<ProtectedChange>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        restore_failed: Vec<String>,
        /// Where the snapshot copies were kept because a path could not be
        /// restored; absent when everything was restored and the copies
        /// were removed.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        kept_copies: Option<String>,
    },
    // --- end of the 0.23.0 block -------------------------------------------
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

/// One commit of an [`EventPayload::ArtifactsCommitted`].
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommittedArtifact {
    #[serde(default)]
    pub sha: String,
    #[serde(default)]
    pub subject: String,
}

/// One path of an [`EventPayload::ProtectedPathsModified`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProtectedChange {
    #[serde(default)]
    pub path: String,
    /// `modified`, `deleted` or `added`.
    #[serde(default)]
    pub change: String,
}

fn is_zero(n: &usize) -> bool {
    *n == 0
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
    /// The run directory this log writes into (the parent of `events.jsonl`).
    pub fn run_dir(&self) -> Option<PathBuf> {
        self.path.parent().map(Path::to_path_buf)
    }

    pub fn create(run_dir: &Path) -> Result<Self, EngineError> {
        std::fs::create_dir_all(run_dir)?;
        Self::open(run_dir)
    }

    pub fn open(run_dir: &Path) -> Result<Self, EngineError> {
        Self::open_with(run_dir, UnknownPolicy::Refuse)
    }

    /// [`Self::open`] for the stop path, which only ever appends the
    /// `run_aborted` checkpoint: an unknown event newer than the last
    /// checkpoint does not refuse (see [`read_all_for_stop`], which already
    /// warned about it).
    pub(crate) fn open_for_stop(run_dir: &Path) -> Result<Self, EngineError> {
        Self::open_with(run_dir, UnknownPolicy::Allow)
    }

    fn open_with(run_dir: &Path, policy: UnknownPolicy) -> Result<Self, EngineError> {
        let path = run_dir.join("events.jsonl");
        // Appending is deciding on the journal, so the engine's rule for
        // unknown events applies (see [`read_all`]); the next seq also clears
        // the skipped ones, so an appended event never reuses their seq.
        let journal = read_journal_with(run_dir, false)?;
        settle_unknown(&journal, policy)?;
        let next_seq = journal
            .events
            .iter()
            .map(|e| e.seq)
            .chain(journal.unknown.iter().map(|u| u.seq))
            .max()
            .map_or(0, |s| s + 1);
        let file = OpenOptions::new().create(true).append(true).open(&path)?;
        Ok(Self {
            path,
            file,
            next_seq,
        })
    }

    /// The seq the next appended event gets.
    pub(crate) fn peek_next_seq(&self) -> u64 {
        self.next_seq
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
        // Only the seq is needed, so an event of a type this binary does not
        // know still counts: its seq is taken all the same.
        let ev: EventHeader =
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
    triage: Option<&WakeTriage>,
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
        triage: triage.cloned(),
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
    raise_wake_with_triage(run_dir, log, trigger, node, detail, None)
}

/// [`raise_wake`] carrying a decision model's `triage` (issue #165 Part 10),
/// mirrored to the parent run with the wake.
pub(crate) fn raise_wake_with_triage(
    run_dir: &Path,
    log: &mut EventLog,
    trigger: WakeTrigger,
    node: &str,
    detail: impl Into<String>,
    triage: Option<WakeTriage>,
) -> Result<(), EngineError> {
    let detail = detail.into();
    log.append(EventPayload::WakeRaised {
        trigger,
        node: node.to_string(),
        detail: detail.clone(),
        triage: triage.clone(),
    })?;
    // Propagation is best-effort for the parent; never fail the child on it.
    let _ = propagate_wake_to_parent(run_dir, trigger, node, &detail, triage.as_ref());
    Ok(())
}

/// How many decisions a gate has already had recorded. The drive consumes the
/// N-th posted decision for a node once N of these exist.
pub(crate) fn review_decided_count(events: &[Event], node: &str) -> usize {
    events
        .iter()
        .filter(|e| matches!(&e.payload, EventPayload::ReviewDecided { node: n, .. } if n == node))
        .count()
}

/// How many open requests of a gate were withdrawn without a decision
/// ([`EventPayload::ReviewWithdrawn`]).
pub(crate) fn review_withdrawn_count(events: &[Event], node: &str) -> usize {
    events
        .iter()
        .filter(
            |e| matches!(&e.payload, EventPayload::ReviewWithdrawn { node: n, .. } if n == node),
        )
        .count()
}

/// How many of a gate's review requests are still open: asked, and neither
/// decided nor withdrawn. The one definition of "pending" the drive loop, the
/// progress fold and `post_review`'s node validation all judge against.
pub(crate) fn review_open_count(events: &[Event], node: &str) -> usize {
    review_requested_count(events, node)
        .saturating_sub(review_decided_count(events, node) + review_withdrawn_count(events, node))
}

/// How many times a gate has asked for a decision (see [`review_open_count`]).
pub(crate) fn review_requested_count(events: &[Event], node: &str) -> usize {
    events
        .iter()
        .filter(
            |e| matches!(&e.payload, EventPayload::ReviewRequested { node: n, .. } if n == node),
        )
        .count()
}

/// `AttemptFinished.usage` as optional data: a block that does not decode as
/// this binary's [`apb_core::agent_output::AgentUsage`] (a `source` or a
/// required field a newer apb added) is dropped instead of making the known
/// event, and with it the whole journal, unreadable.
fn lenient_usage<'de, D>(d: D) -> Result<Option<apb_core::agent_output::AgentUsage>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let v = Option::<serde_json::Value>::deserialize(d)?;
    Ok(v.and_then(|v| serde_json::from_value(v).ok()))
}

/// A compact answer in [`EventPayload::DecisionMade`]: `value` is the chosen
/// option (choice) or the expected level (score), `p` its probability (the
/// "yes" probability for a noul), `invalid` the reason an item was refused.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct DecisionAnswer {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub value: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub p: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub confidence: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub invalid: Option<String>,
}

/// A decision model's recommendation on a park wake
/// ([`EventPayload::WakeRaised`] `triage`, issue #165 Part 10).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct WakeTriage {
    /// The recommended action: `retry_same`, `retry_with_note`,
    /// `switch_executor`, `continue_from_next`, `pause_for_human` or
    /// `needs_supervisor`.
    pub action: String,
    #[serde(default)]
    pub p: f64,
    #[serde(default)]
    pub confidence: f64,
    /// The probability that the output repeats a failure already tried.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub looping_p: Option<f64>,
    #[serde(default)]
    pub provider: String,
    #[serde(default)]
    pub model: String,
    /// The engine posted the retry itself (enforce, Part 14.3).
    #[serde(default, skip_serializing_if = "is_false")]
    pub applied: bool,
}

/// A decision model's recommendation at a review gate
/// ([`EventPayload::ReviewRequested`] `recommendation`, issue #165 Part 11).
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct ReviewRecommendation {
    /// One of the gate's options.
    pub option: String,
    #[serde(default)]
    pub p: f64,
    #[serde(default)]
    pub confidence: f64,
    #[serde(default)]
    pub provider: String,
    #[serde(default)]
    pub model: String,
    #[serde(default)]
    pub calibrated: bool,
    /// The engine posted this option as the decision itself (enforce,
    /// `auto_decide`, Part 14.4).
    #[serde(default, skip_serializing_if = "is_false")]
    #[cfg_attr(feature = "ts", ts(as = "Option<bool>", optional))]
    pub applied: bool,
}

/// A code-only verdict recorded next to a decision (the completion check's
/// generic regex baseline): whether it flags, and the pattern that did.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct DecisionBaseline {
    #[serde(default)]
    pub regex_flag: bool,
    #[serde(default)]
    pub pattern: Option<String>,
}

/// Decodes a structured field of a known event, or its default when the
/// shape is one this binary cannot read (a newer apb's).
fn lenient_default<'de, D, T>(d: D) -> Result<T, D::Error>
where
    D: serde::Deserializer<'de>,
    T: serde::de::DeserializeOwned + Default,
{
    let v = serde_json::Value::deserialize(d)?;
    Ok(serde_json::from_value(v).unwrap_or_default())
}

/// [`lenient_default`] for an optional field.
pub(crate) fn lenient_option<'de, D, T>(d: D) -> Result<Option<T>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: serde::de::DeserializeOwned,
{
    let v = Option::<serde_json::Value>::deserialize(d)?;
    Ok(v.and_then(|v| serde_json::from_value(v).ok()))
}

/// An event whose `type` this binary does not know: a newer apb wrote it.
/// Only its position is kept; the line itself stays on disk untouched.
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct UnknownEvent {
    pub seq: u64,
    pub ts: u128,
    /// The event's `type` tag, as written.
    pub kind: String,
}

/// A journal as read: the events this binary knows, in order, and the ones
/// it skipped because their type is newer than this binary.
#[derive(Debug, Default)]
pub struct JournalRead {
    pub events: Vec<Event>,
    pub unknown: Vec<UnknownEvent>,
}

/// The envelope every event line carries, whatever its type: what is left
/// to go on for a line whose payload this binary cannot decode.
#[derive(Deserialize)]
struct EventHeader {
    seq: u64,
    #[serde(default)]
    ts: u128,
    #[serde(rename = "type")]
    kind: String,
}

/// Whether `kind` names an [`EventPayload`] variant this binary knows. Asked
/// of serde itself, so a variant added later is known without a list to keep
/// in sync: a bare `{"type": kind}` fails with "unknown variant" only when no
/// variant carries that tag (a known one fails on its missing fields, or
/// decodes when all of them are optional).
fn is_known_kind(kind: &str) -> bool {
    match serde_json::from_value::<EventPayload>(serde_json::json!({ "type": kind })) {
        Ok(_) => true,
        Err(e) => !e.to_string().starts_with("unknown variant"),
    }
}

/// Whether `kind` has the shape of an event `type` tag (snake_case, at most
/// 64 characters). Only such a tag is skipped as a newer event type; any
/// other value is corruption, and it is kept out of the messages that name
/// skipped types.
fn is_event_tag(kind: &str) -> bool {
    (1..=64).contains(&kind.len())
        && kind.starts_with(|c: char| c.is_ascii_lowercase())
        && kind
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
}

/// Reads one journal line into `out`. An event of an unknown type is not an
/// error: it goes to `out.unknown` so the caller can skip and count it. A
/// known type whose fields do not decode, and anything that is not an event
/// at all, stays an error.
fn push_line(line: &str, out: &mut JournalRead) -> Result<(), EngineError> {
    let err = match serde_json::from_str::<Event>(line) {
        Ok(ev) => {
            out.events.push(ev);
            return Ok(());
        }
        Err(e) => EngineError::Yaml(e.to_string()),
    };
    match serde_json::from_str::<EventHeader>(line) {
        Ok(h) if is_event_tag(&h.kind) && !is_known_kind(&h.kind) => {
            out.unknown.push(UnknownEvent {
                seq: h.seq,
                ts: h.ts,
                kind: h.kind,
            });
            Ok(())
        }
        _ => Err(err),
    }
}

impl EventPayload {
    /// A checkpoint: an event after which everything earlier in the journal
    /// is settled (a node's result is recorded, the drive stopped at a
    /// boundary, the run ended). The engine skips an unknown event only when
    /// a checkpoint follows it (see [`read_all`]).
    pub fn is_checkpoint(&self) -> bool {
        matches!(
            self,
            EventPayload::NodeFinished { .. }
                | EventPayload::RunPaused { .. }
                | EventPayload::RunFinished { .. }
                | EventPayload::RunAborted { .. }
        )
    }
}

/// The whole journal, for the engine: any unparsable line at all is an error,
/// and so is an event of an unknown type that no checkpoint follows.
///
/// Everything that decides on the journal (the drive loop, the folds, resume,
/// every writer that reads before appending) must fail loudly rather than act
/// on a log it could only read in part. An event type a newer apb wrote is
/// the one exception, and only while a checkpoint follows it
/// ([`EventPayload::is_checkpoint`]): whatever it meant was settled by the
/// time the run reached that checkpoint, so the fold of the known events is
/// still correct. The contract for every new event type is exactly that: it
/// must be safe to skip up to the next checkpoint. An unknown event newer
/// than the last checkpoint may be state the engine needs to continue, so
/// this refuses with a message naming the version mismatch instead.
pub fn read_all(run_dir: &Path) -> Result<Vec<Event>, EngineError> {
    let journal = read_journal_with(run_dir, false)?;
    settle_unknown(&journal, UnknownPolicy::Refuse)?;
    Ok(journal.events)
}

/// [`read_all`] for stopping a run (`apb run stop`, `run_cancel`, the abort
/// propagated to children): an unknown event newer than the last checkpoint
/// does not refuse. `warn` prints one warning about it; the stop path passes
/// it on its first read only, so one stop warns once.
///
/// Stopping is the one decision that is safe on a journal this binary reads
/// only in part. It never re-drives the run: all it can write is
/// `run_aborted`, itself a checkpoint, so whatever the unknown event meant is
/// settled by the abort rather than acted on. Refusing here would leave an
/// older binary unable to stop a run a newer one started. The line format
/// stays strict, as in [`read_all`].
pub(crate) fn read_all_for_stop(run_dir: &Path, warn: bool) -> Result<Vec<Event>, EngineError> {
    let journal = read_journal_with(run_dir, false)?;
    let policy = if warn {
        UnknownPolicy::Warn
    } else {
        UnknownPolicy::Allow
    };
    settle_unknown(&journal, policy)?;
    Ok(journal.events)
}

/// What a writer does with an unknown event newer than the last checkpoint.
#[derive(Clone, Copy)]
enum UnknownPolicy {
    /// Refuse (drive, resume, every other writer).
    Refuse,
    /// Warn and go on (the stop path, see [`read_all_for_stop`]).
    Warn,
    /// Go on silently (the stop path after its first read has warned).
    Allow,
}

fn settle_unknown(journal: &JournalRead, policy: UnknownPolicy) -> Result<(), EngineError> {
    match (check_unknown_settled(journal), policy) {
        (Err(EngineError::Conflict(msg)), UnknownPolicy::Warn) => {
            eprintln!(
                "apb: warning: {msg}; stopping the run anyway, which only appends `run_aborted`"
            );
            Ok(())
        }
        (Err(EngineError::Conflict(_)), UnknownPolicy::Allow) => Ok(()),
        (res, _) => res,
    }
}

/// Refuses a journal holding an unknown event that no checkpoint follows
/// (see [`read_all`]).
fn check_unknown_settled(journal: &JournalRead) -> Result<(), EngineError> {
    let checkpoint = journal
        .events
        .iter()
        .rev()
        .find(|e| e.payload.is_checkpoint())
        .map(|e| e.seq);
    let unsettled: Vec<&UnknownEvent> = journal
        .unknown
        .iter()
        .filter(|u| checkpoint.is_none_or(|c| u.seq > c))
        .collect();
    if unsettled.is_empty() {
        return Ok(());
    }
    let mut kinds: Vec<&str> = unsettled.iter().map(|u| u.kind.as_str()).collect();
    kinds.sort_unstable();
    kinds.dedup();
    let after = match checkpoint {
        Some(seq) => format!("after its last checkpoint (seq {seq})"),
        None => "before any checkpoint".to_string(),
    };
    Err(EngineError::Conflict(format!(
        "the run journal has {} event(s) of a type this apb {} does not know ({}) {after}; \
         they were written by a newer apb, and continuing without them could act on \
         state this binary cannot see: upgrade apb and retry",
        unsettled.len(),
        env!("CARGO_PKG_VERSION"),
        kinds.join(", "),
    )))
}

/// The journal for a reader that may be racing the writer: identical to
/// [`read_all`] except that a single unparsable LAST line is dropped instead
/// of failing the read (issue #103.3), and that unknown event types are
/// skipped wherever they are.
///
/// `EventLog::append` writes one line at a time and a reader can open the file
/// between the bytes of a line and its newline, so a torn tail is a normal
/// transient state of a live run, not corruption. Before this, the HTTP run
/// detail answered 500 for the whole request whenever it landed in that
/// window, which is exactly the "every poll during execution failed" shape the
/// field report describes.
///
/// Only the tail is forgiven. An unparsable line with any further line after
/// it - a blank line included, since a torn append leaves no line behind it at
/// all - is real corruption, and skipping it would hand the caller a journal
/// with a silent hole. Reserved for read-only reporting surfaces; engine
/// consumers stay on [`read_all`]. A surface that shows how many events it
/// skipped reads [`read_journal`] instead.
pub fn read_all_lossy_tail(run_dir: &Path) -> Result<Vec<Event>, EngineError> {
    Ok(read_journal(run_dir)?.events)
}

/// The journal as a read-only surface sees it (see [`read_all_lossy_tail`]),
/// with the events of unknown types it skipped.
pub fn read_journal(run_dir: &Path) -> Result<JournalRead, EngineError> {
    read_journal_with(run_dir, true)
}

fn read_journal_with(run_dir: &Path, tolerate_torn_tail: bool) -> Result<JournalRead, EngineError> {
    let path = run_dir.join("events.jsonl");
    let mut out = JournalRead::default();
    if !path.is_file() {
        return Ok(out);
    }
    // An unparsable line is only forgiven once nothing follows it, so the
    // verdict is deferred: the next line proves it was not the tail.
    let mut torn: Option<EngineError> = None;
    for line in BufReader::new(File::open(&path)?).lines() {
        let line = line?;
        // Any further line at all proves the stored candidate was not the
        // tail, a blank one included: only end-of-file makes a torn line
        // forgivable, so this must run before the blank-line skip.
        if let Some(e) = torn.take() {
            return Err(e);
        }
        if line.trim().is_empty() {
            continue;
        }
        if let Err(err) = push_line(&line, &mut out) {
            if !tolerate_torn_tail {
                return Err(err);
            }
            torn = Some(err);
        }
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

    /// Additive per the workspace rule: an old log line has no `spawn_ms` and
    /// must deserialize, not fail.
    #[test]
    fn attempt_started_spawn_ms_defaults_to_none_on_an_old_line() {
        let line =
            r#"{"seq":0,"ts":1,"type":"attempt_started","node":"a","attempt":1,"agent":"claude"}"#;
        let e: Event = serde_json::from_str(line).unwrap();
        match e.payload {
            EventPayload::AttemptStarted { spawn_ms, .. } => assert_eq!(spawn_ms, None),
            other => panic!("wrong payload: {other:?}"),
        }
    }

    #[test]
    fn attempt_started_spawn_ms_round_trips() {
        let p = EventPayload::AttemptStarted {
            node: "a".into(),
            attempt: 1,
            agent: "claude".into(),
            soul_delivery: None,
            skills_mode: None,
            pid: Some(4242),
            spawn_ms: Some(37),
            model: None,
            workdir: None,
            transcript: None,
        };
        let s = serde_json::to_string(&p).unwrap();
        assert!(s.contains("\"spawn_ms\":37"), "got {s}");
        let back: EventPayload = serde_json::from_str(&s).unwrap();
        match back {
            EventPayload::AttemptStarted {
                node,
                attempt,
                agent,
                soul_delivery,
                skills_mode,
                pid,
                spawn_ms,
                ..
            } => {
                assert_eq!(node, "a");
                assert_eq!(attempt, 1);
                assert_eq!(agent, "claude");
                assert_eq!(soul_delivery, None);
                assert_eq!(skills_mode, None);
                assert_eq!(pid, Some(4242));
                assert_eq!(spawn_ms, Some(37));
            }
            other => panic!("expected AttemptStarted, got {other:?}"),
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
    fn host_task_events_without_the_hint_labels_still_decode() {
        // A line an apb before the hint labels wrote: no `hint_source`,
        // `fallback_of` or submission `model`.
        let old = r#"{"type":"host_task_requested","task_id":"w-2","node":"w","attempt":2,"prompt_ref":"tasks/w-2/prompt.md","workdir":"/w","model_hint":"sonnet"}"#;
        match serde_json::from_str::<EventPayload>(old).unwrap() {
            EventPayload::HostTaskRequested {
                model_hint,
                hint_source,
                fallback_of,
                ..
            } => {
                assert_eq!(model_hint.as_deref(), Some("sonnet"));
                assert_eq!((hint_source, fallback_of), (None, None));
            }
            other => panic!("expected HostTaskRequested, got {other:?}"),
        }
        let old = r#"{"type":"host_task_submitted","task_id":"w-2","status":"failed","submitted_by":"host"}"#;
        match serde_json::from_str::<EventPayload>(old).unwrap() {
            EventPayload::HostTaskSubmitted { model, .. } => assert_eq!(model, None),
            other => panic!("expected HostTaskSubmitted, got {other:?}"),
        }
        // A shape a newer apb might write reads as absent instead of failing
        // the journal.
        let newer = r#"{"type":"host_task_requested","task_id":"w-2","node":"w","attempt":2,"hint_source":{"kind":"something_new"},"fallback_of":{"attempt":"one"}}"#;
        match serde_json::from_str::<EventPayload>(newer).unwrap() {
            EventPayload::HostTaskRequested {
                hint_source,
                fallback_of,
                ..
            } => assert_eq!((hint_source, fallback_of), (None, None)),
            other => panic!("expected HostTaskRequested, got {other:?}"),
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
            usage: None,
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
            usage: None,
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
            reason: None,
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
            usage: None,
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

    // --- torn-tail tolerance (#103.3) --------------------------------------

    /// One complete event line plus a half-written one, which is exactly what
    /// a reader sees while the drive is appending to `events.jsonl`.
    fn torn_log(dir: &std::path::Path) {
        std::fs::write(
            dir.join("events.jsonl"),
            concat!(
                r#"{"seq":0,"ts":1,"type":"run_started","playbook":"p","version":"1.0.0"}"#,
                "\n",
                r#"{"seq":1,"ts":2,"type":"node_star"#,
            ),
        )
        .unwrap();
    }

    #[test]
    fn read_all_stays_strict_on_a_torn_final_line() {
        // The engine's own consumers must keep failing loudly on a log they
        // cannot read in full: this pins that the lossy variant below is an
        // addition, not a relaxation of the strict contract.
        let dir = tempfile::tempdir().unwrap();
        torn_log(dir.path());
        assert!(read_all(dir.path()).is_err());
    }

    #[test]
    fn read_all_lossy_tail_drops_a_torn_final_line() {
        let dir = tempfile::tempdir().unwrap();
        torn_log(dir.path());
        let events = read_all_lossy_tail(dir.path()).unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].seq, 0);
    }

    #[test]
    fn read_all_lossy_tail_still_errors_on_a_malformed_middle_line() {
        // Only the very last line can be torn by a concurrent append. An
        // unparsable line with anything after it is real corruption, and
        // silently skipping it would hand the caller a journal with a hole.
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("events.jsonl"),
            concat!(
                r#"{"seq":0,"ts":1,"type":"run_started","playbook":"p","version":"1.0.0"}"#,
                "\n",
                r#"{"seq":1,"ts":2,"type":"node_star"#,
                "\n",
                r#"{"seq":2,"ts":3,"type":"run_finished","outcome":"succeeded"}"#,
                "\n",
            ),
        )
        .unwrap();
        assert!(read_all_lossy_tail(dir.path()).is_err());
    }

    #[test]
    fn read_all_lossy_tail_errors_when_a_blank_line_follows_the_torn_one() {
        // A blank line IS a further line: the unparsable one is then not the
        // tail a concurrent append leaves behind, it is a hole in the middle of
        // the journal. Forgiving it would hide real corruption behind the
        // read-only reporting surfaces.
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("events.jsonl"),
            concat!(
                r#"{"seq":0,"ts":1,"type":"run_started","playbook":"p","version":"1.0.0"}"#,
                "\n",
                r#"{"seq":1,"ts":2,"type":"node_star"#,
                "\n",
                "\n",
            ),
        )
        .unwrap();
        assert!(
            read_all_lossy_tail(dir.path()).is_err(),
            "a torn line followed by a blank line is not a tolerable torn tail"
        );
    }

    #[test]
    fn read_all_lossy_tail_still_forgives_a_torn_line_ended_by_a_newline() {
        // The other side of the same rule: a complete-looking newline after the
        // torn line, with nothing at all after it, is still the tail.
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("events.jsonl"),
            concat!(
                r#"{"seq":0,"ts":1,"type":"run_started","playbook":"p","version":"1.0.0"}"#,
                "\n",
                r#"{"seq":1,"ts":2,"type":"node_star"#,
                "\n",
            ),
        )
        .unwrap();
        let events = read_all_lossy_tail(dir.path()).unwrap();
        assert_eq!(events.len(), 1);
    }

    #[test]
    fn read_all_lossy_tail_reads_an_intact_log_exactly_like_read_all() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("events.jsonl"),
            concat!(
                r#"{"seq":0,"ts":1,"type":"run_started","playbook":"p","version":"1.0.0"}"#,
                "\n",
                r#"{"seq":1,"ts":3,"type":"run_finished","outcome":"succeeded"}"#,
                "\n",
            ),
        )
        .unwrap();
        let strict = read_all(dir.path()).unwrap();
        let lossy = read_all_lossy_tail(dir.path()).unwrap();
        assert_eq!(strict.len(), 2);
        assert_eq!(
            serde_json::to_string(&strict).unwrap(),
            serde_json::to_string(&lossy).unwrap()
        );
    }

    #[test]
    fn a_decision_record_parses_and_reads_leniently() {
        let line = r#"{"seq":3,"ts":1,"type":"decision_made","use_site":"completion_check","node":"w","attempt":1,"provider":"p","model":"m","calibrated":true,"mode":"shadow","questions_digest":"sha256:q","state_digest":"sha256:s","state_bytes":10,"answers":{"final_result":{"p":0.9}},"applied":false,"would_change":false,"baseline":{"regex_flag":false,"pattern":null},"latency_ms":5,"input_tokens":7,"cost_usd":0.1,"cached":false,"error":null}"#;
        let ev: Event = serde_json::from_str(line).unwrap();
        assert!(is_known_kind("decision_made"));
        assert!(
            !ev.payload.is_checkpoint(),
            "skipping it relies on the next checkpoint"
        );
        // Structured fields of a shape this binary cannot read decode as
        // empty instead of failing the journal.
        let newer = line
            .replace(
                r#""answers":{"final_result":{"p":0.9}}"#,
                r#""answers":["a newer shape"]"#,
            )
            .replace(
                r#""baseline":{"regex_flag":false,"pattern":null}"#,
                r#""baseline":"newer""#,
            );
        let ev: Event = serde_json::from_str(&newer).unwrap();
        let EventPayload::DecisionMade {
            answers, baseline, ..
        } = ev.payload
        else {
            panic!("a decision_made")
        };
        assert!(answers.is_empty() && baseline.is_none());
    }

    #[test]
    fn the_new_optional_fields_leave_old_lines_byte_identical() {
        // Lines as an apb before issue #165 Parts 10-12 wrote them: they
        // parse, and written back they are the same bytes (the new fields
        // are absent when unset).
        for line in [
            r#"{"seq":1,"ts":1,"type":"wake_raised","trigger":"node_failed","node":"w","detail":"boom"}"#,
            r#"{"seq":2,"ts":1,"type":"review_requested","node":"g","options":["approve","reject"],"instruction":"x"}"#,
            r#"{"seq":3,"ts":1,"type":"fallback_triggered","node":"w","from":"a","to":"b","from_model":"m","to_model":"n"}"#,
        ] {
            let ev: Event = serde_json::from_str(line).unwrap();
            assert_eq!(serde_json::to_string(&ev).unwrap(), line);
        }
        // A triage of a shape this binary cannot read is dropped, not fatal.
        let newer = r#"{"seq":1,"ts":1,"type":"wake_raised","trigger":"node_failed","node":"w","detail":"d","triage":"newer"}"#;
        let ev: Event = serde_json::from_str(newer).unwrap();
        assert!(matches!(
            ev.payload,
            EventPayload::WakeRaised { triage: None, .. }
        ));
    }
}
