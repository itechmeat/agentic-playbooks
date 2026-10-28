use std::collections::BTreeMap;
use std::fs::{File, OpenOptions};
use std::io::Write;
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
        /// The param names (and `instruction`) this start took from the
        /// project's remembered input.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        remembered: Vec<String>,
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
        /// around `adapter::command::spawn_agent` and before any of the child's work runs.
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
        /// The random token the attempt's processes carry as
        /// `APB_ATTEMPT_TOKEN`: a connector call names it to prove which
        /// attempt it belongs to, since attempt numbers restart with every
        /// execution of a node. Not a secret, an identity. `None` for old
        /// logs and for attempts that make no connector calls.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        token: Option<String>,
        /// The list item this belongs to, for an item of a list node
        /// (`list:`). Absent everywhere else. Additive.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        item: Option<String>,
        /// What the attempt's isolation held and did not (a node with
        /// `isolation` other than `none`). `None` for every other attempt
        /// and old logs; a shape a newer apb wrote reads as absent.
        #[serde(
            default,
            skip_serializing_if = "Option::is_none",
            deserialize_with = "lenient_option"
        )]
        isolation: Option<AttemptIsolation>,
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
        /// The list item this belongs to, for an item of a list node
        /// (`list:`). Absent everywhere else. Additive.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        item: Option<String>,
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
        /// What was cut from this attempt's captured output when a size limit
        /// hit (bounded agent output): one entry per truncated stream. Empty
        /// when everything fit; additive, old logs read unchanged. Read
        /// leniently like `usage`: a list this binary cannot decode (a newer
        /// apb's shape) reads as empty.
        #[serde(
            default,
            skip_serializing_if = "Vec::is_empty",
            deserialize_with = "lenient_default"
        )]
        output_truncated: Vec<OutputLoss>,
        /// True when the attempt's final agent record could not be read whole
        /// (it fell into a dropped span, the line was over the tail limit, or
        /// the stream ended mid-record), so the attempt must not count as a
        /// success. Additive, old logs read unchanged.
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        transcript_incomplete: bool,
        /// The list item this belongs to, for an item of a list node
        /// (`list:`). Absent everywhere else. Additive.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        item: Option<String>,
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
        /// Who paused the run (issue #200). Absent in older journals; the
        /// views read a missing `by` as `supervisor`, the journal is never
        /// rewritten. Additive.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        by: Option<crate::pause::kind::PauseBy>,
        /// `drain` or `now`; absent in older journals (read as `drain`).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        mode: Option<crate::pause::kind::PauseMode>,
        /// When the run resumes on its own, epoch milliseconds. Additive.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        until: Option<u64>,
        /// The workdir busy lock stays held while paused. Additive.
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        keep_lock: bool,
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
        /// Who resumed the run (issue #200). Absent in older journals.
        /// Additive.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        by: Option<crate::pause::kind::PauseBy>,
        /// The resume named its start node (`--from-node`): no session a
        /// `--now` pause kept carries over into the nodes it re-runs.
        /// Absent (false) in older journals and for every other resume.
        /// Additive.
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        explicit: bool,
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
        /// Who stopped the run: `operator` for `apb stop` and the run_stop
        /// tool, `supervisor` for the supervisor's abort, `engine` for an
        /// eval deadline. Absent in older journals and for a stop that
        /// propagated from a parent. Additive.
        #[serde(
            default,
            skip_serializing_if = "Option::is_none",
            deserialize_with = "lenient_option"
        )]
        by: Option<crate::pause::kind::PauseBy>,
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
        /// What promoted it: `successes` (the trial count of
        /// `promote_supervisor_patches`) or `quality` (the comparison with
        /// `current`). Absent in older journals, which read as `successes`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        basis: Option<String>,
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
    /// The candidate trial `version` was compared with `current` by
    /// quality and is still undecided: `verdict` is `wait` (more trials
    /// needed) or `needs_person` (the trials are spent; a person decides).
    /// Written right before `run_finished`, safe to skip up to that
    /// checkpoint.
    CandidateHeld {
        #[serde(default)]
        version: String,
        #[serde(default)]
        run_id: String,
        #[serde(default)]
        verdict: String,
        #[serde(default)]
        reason: String,
    },
    // --- end candidate trials ---
    // --- run quality ---
    /// The rules grader graded the run as it ended: the stored grade
    /// (`runs/<id>/grades/<grade_id>.json`) and each criterion's level.
    /// Written after the last `node_finished` and right before
    /// `run_finished`, so it is safe to skip up to that checkpoint. An end
    /// replayed after a crash between this event and `run_finished` grades
    /// nothing again (the inline grade looks for this event first). Grades
    /// made after the run ended are not journaled.
    RunGraded {
        #[serde(default)]
        grade_id: String,
        #[serde(default)]
        grader: String,
        #[serde(default)]
        levels: BTreeMap<String, String>,
    },
    // --- end run quality ---
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
        /// The list item this belongs to, for an item of a list node
        /// (`list:`). Absent everywhere else. Additive.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        item: Option<String>,
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
        /// Response completeness (incomplete connector response): `None` for
        /// old events and for kinds where completeness does not apply (smtp,
        /// imap, inbox, mock), `Some(true)` when the response body was read
        /// whole, `Some(false)` when it was cut at the size cap or the read
        /// failed partway. A read-only function with `Some(false)` failed as
        /// `incomplete_response`. Additive.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        response_complete: Option<bool>,
        /// The attempt the call ran in, by number and by the seq of its
        /// `attempt_started` (attempt numbers restart with every execution
        /// of a node; the seq does not). `None` for old events and for a
        /// call outside an attempt (a script node, the supervisor).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        attempt: Option<u32>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        attempt_seq: Option<u64>,
        /// The send intent this call carried out (`send_intent_recorded`).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        intent: Option<String>,
        /// Who made the call: `None` for a node (and old events),
        /// `reconcile` for apb's own lookup of an earlier send.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        origin: Option<String>,
        /// The list item this belongs to, for an item of a list node
        /// (`list:`). Absent everywhere else. Additive.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        item: Option<String>,
    },
    /// A send through a connector function that declares `idempotency` is
    /// about to leave: what it is, who sends it, and the nonce that makes it
    /// unique. Written durably before the request, under the journal lock,
    /// so a repeat of the same intent always sees it.
    ///
    /// Safe to skip up to the next checkpoint: an older apb keeps no send
    /// ledger, so skipping loses nothing it would act on. A send usually
    /// lands before its node's `node_finished`, but not always: a sender
    /// left behind by an ended attempt, or a lookup, can append it after the
    /// run paused or ended. Then no checkpoint follows it, and an older apb
    /// refuses to drive the run on (it asks for an upgrade) while still
    /// stopping it and showing it. Additive variant.
    SendIntentRecorded {
        /// The intent id, unique within the run.
        #[serde(default)]
        intent: String,
        #[serde(default)]
        nonce: String,
        #[serde(default)]
        node: String,
        #[serde(default)]
        attempt: Option<u32>,
        #[serde(default)]
        attempt_seq: Option<u64>,
        #[serde(default)]
        connector: String,
        #[serde(default)]
        function: String,
        #[serde(default)]
        account: String,
        /// The function's idempotency mode: `key`, `marker` or `none`.
        #[serde(default)]
        mode: String,
        /// The caller's `--intent` label, or `args:<hash>` without one.
        #[serde(default)]
        label: String,
        /// The process that sends it.
        #[serde(default)]
        pid: u32,
        /// When a pending intent stops being in flight: the call's timeout
        /// plus a grace period, in wall-clock milliseconds.
        #[serde(default)]
        deadline_ms: u64,
        /// The earlier intent this one repeats with the same nonce (a `key`
        /// send inside its window).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        replay_of: Option<String>,
        /// The earlier intent this one sends again (it did not go out, or
        /// the binding allows a resend).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        resend_of: Option<String>,
        /// The list item this belongs to, for an item of a list node
        /// (`list:`). Absent everywhere else. Additive.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        item: Option<String>,
    },
    /// What became of a send intent: `confirmed`, `not_sent` or `unknown`,
    /// and how that is known (`response`, `reconcile`, `key_replay`, `gate`,
    /// `person`). Safe to skip up to the next checkpoint for the same reason
    /// as `send_intent_recorded`, and with the same exception: a person's
    /// decision (`apb intent resolve`) or a late answer often lands after
    /// the run paused or ended, past its last checkpoint, where an older apb
    /// refuses to drive the run on. Additive variant.
    SendIntentSettled {
        #[serde(default)]
        intent: String,
        #[serde(default)]
        outcome: String,
        #[serde(default)]
        source: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        detail: Option<String>,
        /// The created item's id or link, at most 256 characters.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        effect_ref: Option<String>,
        /// Who decided, for a person's settlement.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        by: Option<String>,
    },
    /// A connector inbox read found unread events that retention had already
    /// dropped (the consumer's cursor pointed past the oldest stored event).
    /// `from` and `through` bound the lost sequence numbers inclusively.
    /// Recorded once per distinct gap per run, when a read first crosses it.
    ///
    /// Safe to skip up to the next checkpoint: it is a reporting record; the
    /// node that made the read still ends with its own `attempt_finished` and
    /// `node_finished`, and retention is not undone by knowing about it.
    /// Additive variant: old logs never carry it, and an older apb skips it.
    InboxGap {
        #[serde(default)]
        node_id: String,
        #[serde(default)]
        connector: String,
        #[serde(default)]
        account: String,
        #[serde(default)]
        from: u64,
        #[serde(default)]
        through: u64,
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
    /// A revival (`NodesRevived`) made an earlier `JoinInputDead` write-off
    /// of the join `node` obsolete: `sources` can arrive again because they
    /// are `from_node` or downstream of it (issue #203). The join is weighed
    /// against the revived view and waits for them.
    ///
    /// Pure observability: the readiness verdict comes from the fold of
    /// `NodesRevived`, never from this record, and the fold ignores it.
    /// Additive: fields default and old logs never carry it. It is written
    /// right after the `NodesRevived` it belongs to, so an older binary that
    /// skips both routes the run as 0.25.0 did (see `NodesRevived`), not as
    /// this binary does. A checkpoint follows it unless the driver is killed
    /// in between.
    JoinInputRevived {
        #[serde(default)]
        node: String,
        #[serde(default)]
        sources: Vec<String>,
        #[serde(default)]
        from_node: String,
    },
    /// A drive is about to execute `from_node` again on an operator's or a
    /// supervisor's word (an explicit `--from-node` resume, a supervisor
    /// retry, continue-from, patch, or a move off a review gate; an automatic
    /// restart of interrupted work revives nothing), and with it
    /// `nodes`: `from_node` and every node only downstream of it (issue
    /// #203). From here on the fold reads each of them as not yet run for
    /// routing and join readiness (`RunState::revived`) until it starts
    /// or finishes again: its earlier result, hops and policy marks stay in
    /// the run's history and are left out of routing on read; its hops are
    /// dropped once it runs again. A `from_node` that has not started again
    /// is a re-run the drive owes: a plain resume re-runs it, and a later
    /// explicit re-run supersedes it (`superseded`). Every later readiness decision of
    /// the drive (each advance, a batch tail, a later resume) sees the same
    /// view, so a join fed by a revived branch waits for it.
    ///
    /// Journaled only when the revival changes something (a revived node has
    /// a result, a hop or a mark), so a fresh run never carries it. Additive:
    /// fields default, old logs never carry it. An older apb skips it and
    /// folds the earlier results back in, which is the routing before this
    /// fix, not a corrupt state; a run revived by this binary must therefore
    /// not be driven by 0.25.0 or older. Every node an older binary did run
    /// leaves the revival on its own `NodeStarted`/`NodeFinished`, so this
    /// binary resuming after it does not mis-route. A checkpoint follows it
    /// unless the driver is killed in between.
    NodesRevived {
        #[serde(default)]
        from_node: String,
        #[serde(default)]
        nodes: Vec<String>,
        /// The owed re-runs (earlier `from_node`s not started again) this
        /// explicit request supersedes: the latest choice wins. Their
        /// revivals end, so the nodes only they covered read their earlier
        /// results again. Carried on the first record of a request.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        superseded: Vec<String>,
    },
    /// A node inside the branches of a fork that declares
    /// `fork.on_branch_failure` (issue #195) finished failed, and the fork's
    /// policy took over: `fail_fast` cancels the other branches and routes to
    /// `target` (the fork's `on_failure`), `cancel_siblings` cancels them and
    /// lets the failed node follow its own routing (`target` absent). Written
    /// right after the failed node's `NodeFinished`. Under `fail_fast` the
    /// failed node's own outgoing edges are not taken (the fold records that),
    /// so a resume does not follow them either. Additive: old logs never carry
    /// it, and a checkpoint (the cancellations' `NodeFinished`, the next node's)
    /// follows it.
    BranchFailed {
        #[serde(default)]
        fork: String,
        #[serde(default)]
        node: String,
        #[serde(default)]
        policy: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        target: Option<String>,
    },
    /// One node a fork's branch-failure policy cancelled (issue #195): a
    /// running branch node that was interrupted, a branch head that never
    /// started, or a fork join that will now never run. Written after that
    /// node's `cancelled` `NodeFinished`; the cancelled node routes nowhere, so
    /// a resume does not run what lies behind it. Additive.
    BranchCancelled {
        #[serde(default)]
        fork: String,
        #[serde(default)]
        node: String,
        /// The branch node whose failure triggered the cancellation.
        #[serde(default)]
        failed_node: String,
    },
    /// A join with `require: all_succeeded` (issue #195) refused to run
    /// because an incoming branch arrived failed or can never arrive (a dead
    /// arrival). Written between the join's `NodeStarted` and its failed
    /// `NodeFinished`; the join then follows only a failure route (a
    /// conditional or fallback edge, or `defaults.on_failure`), and without one
    /// the run fails with `reason`. Additive.
    JoinRefused {
        #[serde(default)]
        node: String,
        /// The inputs that did not arrive succeeded, in incoming-edge order.
        #[serde(default)]
        sources: Vec<String>,
        #[serde(default)]
        reason: String,
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
    // --- baseline check and attempt notes ---------------------------------
    /// One baseline check's snapshot. `phase` is `before` (taken once,
    /// before the first node that can act) or `after` (at the finish node);
    /// `status` is `observed`, `unknown` (the source could not be read, with
    /// `reason`) or `missed` (an acting node started before any before
    /// snapshot, so none can be honest). `input_digest` pins what was read
    /// (rendered connector args, script content, project commands), and a
    /// mismatch between the two phases makes every comparison unknown.
    /// `inherited_from` names the run a `continued_from` successor took its
    /// before snapshot from.
    ///
    /// Safe to skip up to the next checkpoint: a before snapshot is written
    /// before the acting node's `node_started`, whose `node_finished` (or a
    /// `run_paused` / `run_aborted`) follows; an after snapshot is written
    /// before the finish node's `node_finished`.
    BaselineObserved {
        #[serde(default)]
        check: String,
        #[serde(default)]
        phase: String,
        #[serde(default)]
        status: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reason: Option<String>,
        #[serde(
            default,
            skip_serializing_if = "BTreeMap::is_empty",
            deserialize_with = "lenient_default"
        )]
        observations: BTreeMap<String, apb_core::baseline::Observation>,
        #[serde(default)]
        input_digest: String,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        inputs: Vec<String>,
        /// The project commands a `project_checks` source discovered at
        /// before; the after snapshot runs exactly these.
        #[serde(
            default,
            skip_serializing_if = "Vec::is_empty",
            deserialize_with = "lenient_default"
        )]
        commands: Vec<apb_core::project_checks::ProjectCommand>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        workdir: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        inherited_from: Option<String>,
    },
    /// One observation's comparison at the finish: the rule, both values,
    /// the verdict (`met`, `not_met`, `unknown`), and whether the after
    /// value is strictly worse than the before one. `enforced` marks a
    /// comparison under `baseline_check.enforce: true`. Written before the
    /// finish node's `node_finished`, like `goal_checked`, so it is safe to
    /// skip up to that checkpoint.
    BaselineCompared {
        #[serde(default)]
        check: String,
        #[serde(default)]
        observation: String,
        #[serde(default)]
        rule: String,
        #[serde(
            default,
            skip_serializing_if = "Option::is_none",
            deserialize_with = "lenient_option"
        )]
        before: Option<apb_core::baseline::Observation>,
        #[serde(
            default,
            skip_serializing_if = "Option::is_none",
            deserialize_with = "lenient_option"
        )]
        after: Option<apb_core::baseline::Observation>,
        #[serde(default)]
        verdict: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        regressed: Option<bool>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        detail: Option<String>,
        #[serde(default, skip_serializing_if = "is_false")]
        enforced: bool,
    },
    /// The process environment an agent attempt started with under a
    /// profile's `process_env: allowlist`: how many operator variables it
    /// kept and which ones it did not get (names only, never values).
    /// Journaled only for allowlist attempts, right before the spawn. A
    /// report with no state effect; the node's `node_finished` or a
    /// `run_paused` follows, so it is safe to skip. Additive variant.
    AgentEnvApplied {
        node: String,
        #[serde(default)]
        attempt: u32,
        #[serde(default)]
        agent: String,
        #[serde(default)]
        kept: u32,
        /// Operator variables not passed on, sorted, at most the first 200.
        #[serde(default)]
        withheld: Vec<String>,
        /// How many more were withheld beyond `withheld`.
        #[serde(default)]
        withheld_more: u32,
        /// The profile's exact `allow` names the operator's env does not set.
        #[serde(default)]
        absent: Vec<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        item: Option<String>,
        /// What forced the allowlist on a profile that inherits
        /// (`isolation`); `None` when the profile asked for it itself.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        forced_by: Option<String>,
    },
    /// The findings and warnings an attempt reported (its status file, or a
    /// host submission merged into it), after sanitizing: `dropped` counts
    /// the malformed entries, `dropped_reasons` says why (at most five).
    /// Reported only: notes never change a status, an output or a route.
    /// Written right before the attempt's `attempt_finished`; the node's
    /// `node_finished` or a `run_paused` follows, so it is safe to skip.
    AttemptNotes {
        #[serde(default)]
        node: String,
        #[serde(default)]
        attempt: u32,
        #[serde(
            default,
            skip_serializing_if = "Vec::is_empty",
            deserialize_with = "lenient_default"
        )]
        findings: Vec<apb_core::run_notes::Finding>,
        #[serde(
            default,
            skip_serializing_if = "Vec::is_empty",
            deserialize_with = "lenient_default"
        )]
        warnings: Vec<apb_core::run_notes::NoteWarning>,
        #[serde(default, skip_serializing_if = "is_zero_u32")]
        dropped: u32,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        dropped_reasons: Vec<String>,
    },
    /// What an agent attempt really saw differs from what apb handed it: a
    /// tool apb's channels need is missing (the shell a node's connectors
    /// are called through, the ask-server's `ask_user` on a live node), an
    /// MCP server apb passed is absent or not connected, or a server other
    /// than apb's own is present under a strict MCP config. Names only.
    /// Checked only where the agent reports its tools (the stream
    /// transport). Reported only: it changes no status. Written right before
    /// the attempt's `attempt_finished`; the node's `node_finished` or a
    /// `run_paused` follows, so it is safe to skip. Additive variant.
    ToolExposureMismatch {
        #[serde(default)]
        node: String,
        #[serde(default)]
        attempt: u32,
        #[serde(default)]
        agent: String,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        missing_tools: Vec<String>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        missing_servers: Vec<String>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        unexpected_servers: Vec<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        item: Option<String>,
    },
    /// A visit of a list node (`list:`) started: the items it holds after
    /// the filter, how many the filter dropped, and their digest. `reopened`
    /// marks a supervisor retry after a failed visit. Written before any
    /// item runs; safe to skip up to the node's `node_finished` or a
    /// `run_paused`, which always follows. Additive variant.
    ListStarted {
        node: String,
        #[serde(default)]
        total: u32,
        #[serde(default)]
        filtered_out: u32,
        #[serde(default)]
        items_digest: String,
        #[serde(default)]
        batch_size: u32,
        #[serde(default)]
        parallel: u32,
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        reopened: bool,
    },
    /// The ids admitted into batch `batch` (from 1) of a list visit. Safe to
    /// skip like `list_started`. Additive variant.
    ListBatchStarted {
        node: String,
        #[serde(default)]
        batch: u32,
        #[serde(default)]
        items: Vec<String>,
    },
    /// Where one item of a list visit went: `passed`, `rejected` (it runs
    /// again), `needs_look` or `interrupted` (back to pending). `output` is
    /// the compact JSON of the item's declared outputs, clipped. `carried`
    /// marks an item a `rerun: not_passed` visit kept from the previous one.
    /// Every item of a batch settles before the next batch starts. Safe to
    /// skip like `list_started`. Additive variant.
    ListItemSettled {
        node: String,
        #[serde(default)]
        item: String,
        #[serde(default)]
        state: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        verdict: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        note: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reason: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        output: Option<String>,
        #[serde(default)]
        round: u32,
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        carried: bool,
        /// Which check decided (`output`, `script`, `connector`, `judge`).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        check: Option<String>,
        /// The digest of the item's content: `rerun: not_passed` carries a
        /// passed item only while its content is the same, and a later
        /// visit shows an item what it reported before only then.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        digest: Option<String>,
    },
    /// The known problems shown to the run's agents (see
    /// `crate::known_problems`), worked out once at start: each shown
    /// problem's stored texts (already scrubbed and capped) and the agent
    /// nodes it is shown to, and why the other significant ones are hidden.
    /// Written right after `run_provenance`, only for a playbook whose
    /// `known_problems:` block shows anything; the prompt section of every
    /// later attempt, resume included, is rebuilt from it. Safe to skip up to
    /// the next checkpoint: an older apb's prompts simply lack the section.
    /// Additive variant.
    KnownProblemsShown {
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        shown: Vec<apb_core::known_problems::ShownProblem>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        hidden: Vec<apb_core::known_problems::HiddenProblem>,
        /// Problems of a file this installation did not write: only
        /// counted, never named.
        #[serde(default, skip_serializing_if = "is_zero_u32")]
        unsigned: u32,
    },
    /// The run's budget (see `crate::budget`): at start, from the playbook's
    /// block and the run argument, and at each resume that sets or raises
    /// it. The latest one is in force. Safe to skip up to the next
    /// checkpoint: an older apb then drives the run without its budget.
    /// Additive variant.
    BudgetSet {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        tokens: Option<u64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        usd: Option<f64>,
        #[serde(default, deserialize_with = "lenient_default")]
        on_no_estimate: apb_core::budget::OnNoEstimate,
        /// `playbook`, `run` or `resume`; a source a newer apb wrote reads
        /// as absent.
        #[serde(
            default,
            skip_serializing_if = "Option::is_none",
            deserialize_with = "lenient_option"
        )]
        source: Option<apb_core::budget::BudgetSource>,
        #[serde(
            default,
            skip_serializing_if = "Option::is_none",
            deserialize_with = "lenient_option"
        )]
        by: Option<crate::pause::kind::PauseBy>,
    },
    /// One budget gate decision other than a plain admission, before a
    /// gated node or (`batch`) a list batch. A `paused` decision is always
    /// followed by `run_paused`, a checkpoint. Additive variant.
    BudgetGate {
        #[serde(default)]
        node: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        batch: Option<u32>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        items: Option<u32>,
        decision: crate::budget::kind::GateDecision,
        basis: crate::budget::kind::GateBasis,
        /// The node's median spend the gate weighed. Read leniently.
        #[serde(
            default,
            skip_serializing_if = "Option::is_none",
            deserialize_with = "lenient_option"
        )]
        need: Option<crate::budget::kind::BudgetAmount>,
        /// What was left per limited dimension, where known. Read leniently.
        #[serde(
            default,
            skip_serializing_if = "Option::is_none",
            deserialize_with = "lenient_option"
        )]
        remaining: Option<crate::budget::kind::BudgetAmount>,
        /// What the run had spent. Read leniently.
        #[serde(default, deserialize_with = "lenient_default")]
        spent: crate::budget::kind::BudgetSpent,
        #[serde(
            default,
            skip_serializing_if = "Option::is_none",
            deserialize_with = "lenient_option"
        )]
        by: Option<crate::pause::kind::PauseBy>,
    },
    /// The run's notification subscription (see `crate::notify`): at start,
    /// at a sub-playbook's start (`source: parent`), and when a person
    /// changes it on the live run (`by: operator`). The latest one is in
    /// force; `target: None` turns notifications off, with `problem` saying
    /// why when a default could not be used. Safe to skip up to the next
    /// checkpoint: an older apb then sends no notifications. Additive
    /// variant.
    NotifySet {
        #[serde(
            default,
            skip_serializing_if = "Option::is_none",
            deserialize_with = "lenient_option"
        )]
        target: Option<crate::notify::record::NotifyTargetRecord>,
        /// `review`, `question`, `paused`, `finished`; a name a newer apb
        /// wrote is ignored.
        #[serde(default)]
        on: Vec<String>,
        /// `run`, `project`, `machine`, `person` or `parent`.
        #[serde(default)]
        source: String,
        #[serde(
            default,
            skip_serializing_if = "Option::is_none",
            deserialize_with = "lenient_option"
        )]
        by: Option<crate::pause::kind::PauseBy>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        problem: Option<String>,
    },
}

fn is_zero_u32(n: &u32) -> bool {
    *n == 0
}

/// One stream's kept-versus-dropped byte counts for a truncated attempt output
/// ([`EventPayload::AttemptFinished`], bounded agent output). `stream` is one
/// of `stdout`, `stderr`, `stream_log` (the streaming transport's log),
/// `session_copy` (the copied agent session transcript). `dropped_bytes_at_least`
/// is a lower bound: only whole dropped spans are countable.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct OutputLoss {
    #[serde(default)]
    pub stream: String,
    #[serde(default)]
    pub kept_bytes: u64,
    #[serde(default)]
    pub dropped_bytes_at_least: u64,
}

/// One commit of an [`EventPayload::ArtifactsCommitted`].
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommittedArtifact {
    #[serde(default)]
    pub sha: String,
    #[serde(default)]
    pub subject: String,
    /// The values of the commit's `Apb-Run` trailers: `None` when they were
    /// not read (an old event, or a git too old to read trailers),
    /// `Some([])` for a commit without one. Additive.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub apb_run: Option<Vec<String>>,
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
    /// The file's length after this handle's last write (`u64::MAX` before
    /// its first). Any other length means another writer appended in between
    /// (an `apb connector call` or `apb decide` subprocess of an attempt,
    /// issue #193), so the next append re-reads the high-water mark first
    /// instead of reusing a seq.
    known_len: u64,
    /// How far this handle's latest append got into the file, so a caller
    /// whose append failed knows whether its line is there.
    last_append: AppendMark,
}

/// How far an append got into `events.jsonl`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AppendMark {
    /// Nothing was written: the append failed (or declined) before its
    /// write began, so its line is known not to be in the file.
    NotWritten,
    /// The write began and failed: part of the line may be in the file,
    /// with no newline after it.
    Torn,
    /// The whole line (JSON and newline) is in the file; a sync after it
    /// may still have failed.
    Whole,
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
        Self::open_with(run_dir, UnknownPolicy::Refuse, false)
    }

    /// [`Self::open`] for a writer outside the drive (an `apb decide` or
    /// `apb connector call` subprocess, the MCP server): the drive may be
    /// in the middle of an append while the journal is read, so a torn last
    /// line is forgiven here. The seq is settled again under the append
    /// lock, where every line is whole ([`Self::append`]).
    pub(crate) fn open_foreign(run_dir: &Path) -> Result<Self, EngineError> {
        Self::open_with(run_dir, UnknownPolicy::Refuse, true)
    }

    /// [`Self::open`] for the stop path, which only ever appends the
    /// `run_aborted` checkpoint: an unknown event newer than the last
    /// checkpoint does not refuse (see [`read_all_for_stop`], which already
    /// warned about it).
    pub(crate) fn open_for_stop(run_dir: &Path) -> Result<Self, EngineError> {
        Self::open_with(run_dir, UnknownPolicy::Allow, false)
    }

    fn open_with(
        run_dir: &Path,
        policy: UnknownPolicy,
        tolerate_torn_tail: bool,
    ) -> Result<Self, EngineError> {
        let path = run_dir.join("events.jsonl");
        // Appending is deciding on the journal, so the engine's rule for
        // unknown events applies (see [`read_all`]); the next seq also clears
        // the skipped ones, so an appended event never reuses their seq.
        let journal = read_journal_with(run_dir, tolerate_torn_tail)?;
        settle_unknown(&journal, policy)?;
        let next_seq = journal
            .events
            .iter()
            .map(|e| e.seq)
            .chain(journal.unknown.iter().map(|u| u.seq))
            .max()
            .map_or(0, |s| s + 1);
        let file = OpenOptions::new().create(true).append(true).open(&path)?;
        // Unknown until the first append: another writer may have appended
        // between the read above and this open, and the length now would
        // hide it. The first append therefore settles the seq from the file
        // under the lock.
        let known_len = u64::MAX;
        Ok(Self {
            path,
            file,
            next_seq,
            known_len,
            last_append: AppendMark::NotWritten,
        })
    }

    /// How far this handle's latest append got into the file: after a
    /// failed append it tells a line known not to be there from one that
    /// may be torn or that is whole with only its sync failed.
    pub(crate) fn last_append(&self) -> AppendMark {
        self.last_append
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

    /// Appends one event. Safe across processes and handles: the seq is
    /// settled and the line written under an advisory lock on the log file
    /// ([`JournalLock`]), and the JSON and its newline go out in one write
    /// from one buffer, so two writers never interleave into one torn line
    /// or share a seq. The lock is held only for that write (never across a
    /// model call or an agent run), so the drive is never blocked for long.
    pub fn append(&mut self, payload: EventPayload) -> Result<Event, EngineError> {
        match self.append_inner(Build::Plain(Box::new(payload)), false)? {
            Some(event) => Ok(event),
            None => unreachable!("an unconditional append always writes"),
        }
    }

    /// [`Self::append`] when `admit` holds for the journal as it stands
    /// under the append lock (read fresh, a torn tail forgiven), else
    /// nothing is written and `None` comes back. A writer outside the drive
    /// uses it to check that the run is still live at the very moment it
    /// appends.
    pub(crate) fn append_checked(
        &mut self,
        payload: EventPayload,
        admit: impl FnOnce(&[Event]) -> bool,
    ) -> Result<Option<Event>, EngineError> {
        let build = move |events: &[Event]| admit(events).then_some(payload);
        self.append_inner(Build::Decided(Box::new(build)), false)
    }

    /// Appends what `build` makes of the journal as it stands under the
    /// append lock (nothing when it returns `None`), and syncs the line to
    /// disk before the lock is released. The lock is required here: a
    /// journal that cannot be locked fails the append before `build` runs.
    /// Synced because a send intent must survive a crash
    /// of the machine, not only of the process, before its request leaves,
    /// and what it records (its nonce, the intent it replays) depends on the
    /// journal at that very moment.
    pub(crate) fn append_decided_synced(
        &mut self,
        build: impl FnOnce(&[Event]) -> Option<EventPayload>,
    ) -> Result<Option<Event>, EngineError> {
        self.append_inner(Build::Decided(Box::new(build)), true)
    }

    fn append_inner(
        &mut self,
        build: Build<'_>,
        durable: bool,
    ) -> Result<Option<Event>, EngineError> {
        self.last_append = AppendMark::NotWritten;
        let file = self.file.try_clone()?;
        // A durable decision (a send intent) must not race another writer:
        // without the lock it is refused, and nothing is sent.
        let _lock = if durable {
            JournalLock::required(&file)?
        } else {
            JournalLock::on(&file)
        };
        // Only a decided append re-reads the journal; the drive's own
        // appends stay O(1).
        let payload = match build {
            Build::Plain(payload) => *payload,
            Build::Decided(decide) => {
                let events = match self.run_dir() {
                    Some(dir) => read_all_lossy_tail(&dir)?,
                    None => Vec::new(),
                };
                match decide(&events) {
                    Some(payload) => payload,
                    None => return Ok(None),
                }
            }
        };
        // Another handle appended since this one last wrote: settle the seq
        // from the file, which is whole under the lock.
        if self
            .file
            .metadata()
            .is_ok_and(|m| m.len() != self.known_len)
        {
            self.resync_seq()?;
        }
        let event = Event {
            seq: self.next_seq,
            ts: apb_core::clock::now_ms(),
            payload,
        };
        let mut buf = serde_json::to_vec(&event).map_err(|e| EngineError::Yaml(e.to_string()))?;
        buf.push(b'\n');
        self.last_append = AppendMark::Torn;
        self.file.write_all(&buf)?;
        self.file.flush()?;
        // The line is in the file: keep this handle in step with it even
        // when the sync below fails.
        self.last_append = AppendMark::Whole;
        self.next_seq += 1;
        self.known_len = self.file.metadata().map_or(0, |m| m.len());
        if durable {
            self.file.sync_data()?;
        }
        Ok(Some(event))
    }
}

/// What an append writes: a fixed payload, or the one a decision over the
/// journal under the lock makes (none at all when it declines).
enum Build<'a> {
    // Boxed: a payload is far larger than the boxed decision.
    Plain(Box<EventPayload>),
    #[allow(clippy::type_complexity)]
    Decided(Box<dyn FnOnce(&[Event]) -> Option<EventPayload> + 'a>),
}

/// An advisory exclusive lock on a run's `events.jsonl` (`flock` on unix),
/// released on drop. Every append holds it from the seq check to the end
/// of its write; a decision's budget reservation holds it while it reads
/// the journal's decisions (see `decision::budget`). Separate handles of
/// one process exclude each other too. A file system without locks makes
/// it a no-op: the single-buffer write still keeps lines whole.
pub(crate) struct JournalLock<'a> {
    file: Option<&'a File>,
}

impl<'a> JournalLock<'a> {
    pub(crate) fn on(file: &'a File) -> Self {
        JournalLock {
            file: file.lock().ok().map(|()| file),
        }
    }

    /// [`Self::on`] for a writer whose decision depends on no other writer
    /// appending meanwhile: a lock that cannot be taken is an error, never
    /// a silent no-op.
    pub(crate) fn required(file: &'a File) -> Result<Self, EngineError> {
        file.lock().map_err(|e| {
            EngineError::Io(std::io::Error::new(
                e.kind(),
                format!("the run journal could not be locked: {e}"),
            ))
        })?;
        Ok(JournalLock { file: Some(file) })
    }
}

impl Drop for JournalLock<'_> {
    fn drop(&mut self) {
        if let Some(f) = self.file {
            let _ = f.unlock();
        }
    }
}

/// Runs `f` under the append lock of the journal in `run_dir`.
pub(crate) fn with_journal_lock<T>(
    run_dir: &Path,
    f: impl FnOnce() -> T,
) -> Result<T, EngineError> {
    let file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(run_dir.join("events.jsonl"))?;
    let _lock = JournalLock::on(&file);
    Ok(f())
}

/// The lines of a journal read as ONE snapshot of its bytes. A reader
/// outside the append lock races the writers: reading line by line from
/// the open file, a line still being written comes back as a short "last"
/// line at a momentary end-of-file, and its rest then arrives as one more
/// line, so a torn tail looked like corruption in the middle. Splitting a
/// single read keeps every torn byte at the end of the snapshot, where it
/// is forgiven. A line that is not UTF-8 (a write torn inside a multi-byte
/// character) is a parse error like any other torn line.
fn journal_lines(bytes: &[u8]) -> impl Iterator<Item = Result<&str, EngineError>> {
    let body = bytes.strip_suffix(b"\n").unwrap_or(bytes);
    body.split(|b| *b == b'\n').map(|line| {
        let line = line.strip_suffix(b"\r").unwrap_or(line);
        std::str::from_utf8(line).map_err(|e| EngineError::Yaml(e.to_string()))
    })
}

/// Last seq recorded in an events.jsonl file, if any.
fn last_seq_on_disk(path: &Path) -> Result<Option<u64>, EngineError> {
    if !path.is_file() {
        return Ok(None);
    }
    let mut last: Option<u64> = None;
    // A line torn by a writer that died mid-write is forgiven at the tail
    // only, as in [`read_all_lossy_tail`].
    let mut torn: Option<EngineError> = None;
    let bytes = std::fs::read(path)?;
    for line in journal_lines(&bytes) {
        if let Some(e) = torn.take() {
            return Err(e);
        }
        let line = match line {
            Ok(line) => line,
            Err(e) => {
                torn = Some(e);
                continue;
            }
        };
        if line.trim().is_empty() {
            continue;
        }
        // Only the seq is needed, so an event of a type this binary does not
        // know still counts: its seq is taken all the same.
        match serde_json::from_str::<EventHeader>(line) {
            Ok(ev) => last = Some(ev.seq),
            Err(e) => torn = Some(EngineError::Yaml(e.to_string())),
        }
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

/// At most this many view entries an attempt's isolation record keeps.
pub const ISOLATION_VIEW_CAP: usize = 64;

/// What one attempt's isolation held (`holds`) and did not (`limits`), by
/// the code ids of `apb_core::isolation::contract`, and the paths it saw
/// beyond the system.
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct AttemptIsolation {
    /// `full` or `best_effort`.
    pub level: String,
    /// `bwrap`, or `none` when the attempt ran without a sandbox.
    pub backend: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub bwrap_version: Option<String>,
    /// `open` or `none`.
    pub network: String,
    #[serde(default)]
    pub holds: Vec<String>,
    /// Including `credentials.env_passed:<NAME>` for each credential-shaped
    /// variable the allowlist still passes (a name, never a value).
    #[serde(default)]
    pub limits: Vec<String>,
    /// Why no sandbox was used, under `best_effort` (`code (detail)`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    #[cfg_attr(feature = "ts", ts(as = "Option<Vec<String>>", optional))]
    pub fallbacks: Vec<String>,
    /// The bound paths (`~/...` under the home) and their access, at most
    /// [`ISOLATION_VIEW_CAP`].
    #[serde(default)]
    pub view: Vec<IsolationViewEntry>,
}

/// One path of an attempt's sandbox view.
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct IsolationViewEntry {
    pub path: String,
    /// `read_only`, `read_write` or `hidden`.
    pub access: String,
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
    if !path.is_file() {
        return Ok(JournalRead::default());
    }
    let bytes = std::fs::read(&path)?;
    match parse_journal(&bytes, tolerate_torn_tail) {
        // A strict reader outside the append lock can catch another writer
        // mid-line (a batch member's attempt journaling while the drive
        // routes a sibling). Read again once that write is done: under the
        // lock no line is half written, so a tail still torn then is a
        // writer that died mid-line, and stays an error.
        Err(Unreadable::Tail(_)) => {
            let bytes = with_journal_lock(run_dir, || std::fs::read(&path))??;
            parse_journal(&bytes, false).map_err(Unreadable::into_error)
        }
        other => other.map_err(Unreadable::into_error),
    }
}

/// Why a journal's bytes did not parse: a bad final line (possibly a write
/// still in progress) or a bad line with more after it.
enum Unreadable {
    Tail(EngineError),
    Middle(EngineError),
}

impl Unreadable {
    fn into_error(self) -> EngineError {
        match self {
            Unreadable::Tail(e) | Unreadable::Middle(e) => e,
        }
    }
}

/// Parses a journal snapshot. An unparsable line is only forgiven once
/// nothing follows it, so the verdict is deferred: the next line proves it
/// was not the tail. Strict, a bad tail is [`Unreadable::Tail`].
fn parse_journal(bytes: &[u8], tolerate_torn_tail: bool) -> Result<JournalRead, Unreadable> {
    let mut out = JournalRead::default();
    let mut torn: Option<EngineError> = None;
    for line in journal_lines(bytes) {
        // Any further line at all proves the stored candidate was not the
        // tail, a blank one included: only end-of-file makes a torn line
        // forgivable, so this must run before the blank-line skip.
        if let Some(e) = torn.take() {
            return Err(Unreadable::Middle(e));
        }
        let parsed = line.and_then(|line| {
            if line.trim().is_empty() {
                Ok(())
            } else {
                push_line(line, &mut out)
            }
        });
        if let Err(err) = parsed {
            torn = Some(err);
        }
    }
    match torn {
        Some(e) if !tolerate_torn_tail => Err(Unreadable::Tail(e)),
        _ => Ok(out),
    }
}

#[cfg(test)]
mod tests {
    /// A journal that cannot be locked fails a durable append before its
    /// decision runs, and writes nothing. On Linux a file opened as a path
    /// only (`O_PATH`) cannot be locked.
    #[cfg(target_os = "linux")]
    #[test]
    fn a_durable_append_needs_the_journal_lock() {
        use std::os::unix::fs::OpenOptionsExt;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("events.jsonl");
        std::fs::write(&path, "").unwrap();
        let unlockable = std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_PATH)
            .open(&path)
            .unwrap();
        let err = super::JournalLock::required(&unlockable)
            .err()
            .expect("an O_PATH handle cannot be locked");
        assert!(err.to_string().contains("could not be locked"));
        let lockable = std::fs::File::open(&path).unwrap();
        assert!(super::JournalLock::required(&lockable).is_ok());
    }

    use super::*;

    /// Writers on separate handles (as separate processes would be) never
    /// share a seq and never tear a line, however their appends interleave.
    #[test]
    fn concurrent_appends_from_separate_handles_keep_seqs_unique_and_lines_whole() {
        let dir = tempfile::tempdir().unwrap();
        EventLog::create(dir.path()).unwrap();
        const WRITERS: usize = 6;
        const EACH: usize = 40;
        std::thread::scope(|s| {
            for w in 0..WRITERS {
                let run_dir = dir.path();
                s.spawn(move || {
                    // A long payload makes a split write likelier.
                    let detail = format!("writer {w} ").repeat(200);
                    let mut log = EventLog::open_foreign(run_dir).unwrap();
                    for _ in 0..EACH {
                        log.append(EventPayload::SupervisorLost {
                            detail: detail.clone(),
                        })
                        .unwrap();
                    }
                });
            }
        });
        let raw = std::fs::read_to_string(dir.path().join("events.jsonl")).unwrap();
        let mut seqs: Vec<u64> = raw
            .lines()
            .map(|l| serde_json::from_str::<Event>(l).expect("a whole line").seq)
            .collect();
        assert_eq!(seqs.len(), WRITERS * EACH);
        seqs.sort_unstable();
        seqs.dedup();
        assert_eq!(seqs.len(), WRITERS * EACH, "a seq was shared");
    }

    /// A writer outside the drive opens a journal whose last line is still
    /// being written, and settles its seq after it under the lock.
    #[test]
    fn a_foreign_writer_opens_past_a_torn_last_line() {
        let dir = tempfile::tempdir().unwrap();
        let mut log = EventLog::create(dir.path()).unwrap();
        log.append(EventPayload::SupervisorLost { detail: "a".into() })
            .unwrap();
        let path = dir.path().join("events.jsonl");
        let mut f = OpenOptions::new().append(true).open(&path).unwrap();
        f.write_all(b"{\"seq\":1,\"ts\":1,\"type\":\"super")
            .unwrap();
        assert!(EventLog::open(dir.path()).is_err());
        assert!(EventLog::open_foreign(dir.path()).is_ok());
    }

    /// A write torn inside a multi-byte character is a torn tail like any
    /// other: forgiven at the end, refused once a line follows it.
    #[test]
    fn a_tail_torn_inside_a_multibyte_character_is_forgiven_only_at_the_end() {
        let dir = tempfile::tempdir().unwrap();
        let mut log = EventLog::create(dir.path()).unwrap();
        log.append(EventPayload::SupervisorLost { detail: "a".into() })
            .unwrap();
        let path = dir.path().join("events.jsonl");
        let mut f = OpenOptions::new().append(true).open(&path).unwrap();
        let torn = "{\"seq\":1,\"ts\":1,\"type\":\"supervisor_lost\",\"detail\":\"\u{e9}";
        f.write_all(&torn.as_bytes()[..torn.len() - 1]).unwrap();
        assert!(EventLog::open(dir.path()).is_err());
        let foreign = EventLog::open_foreign(dir.path()).unwrap();
        assert_eq!(foreign.peek_next_seq(), 1);
        assert_eq!(read_journal(dir.path()).unwrap().events.len(), 1);
        f.write_all(b"\n{\"seq\":2,\"ts\":2,\"type\":\"supervisor_lost\",\"detail\":\"b\"}\n")
            .unwrap();
        assert!(read_journal(dir.path()).is_err());
    }

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
            item: None,
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
            token: None,
            isolation: None,
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
            item: None,
            output_truncated: Vec::new(),
            transcript_incomplete: false,
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
            item: None,
            output_truncated: Vec::new(),
            transcript_incomplete: false,
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
            item: None,
            output_truncated: Vec::new(),
            transcript_incomplete: false,
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

    /// A strict reader that catches a line another writer is still writing
    /// waits for that write under the append lock and reads it whole.
    #[test]
    fn read_all_reads_a_line_a_live_writer_is_finishing() {
        let dir = tempfile::tempdir().unwrap();
        torn_log(dir.path());
        let path = dir.path().join("events.jsonl");
        let mut writer = OpenOptions::new().append(true).open(&path).unwrap();
        let held = writer.try_clone().unwrap();
        let lock = JournalLock::required(&held).unwrap();
        let reader = {
            let dir = dir.path().to_path_buf();
            std::thread::spawn(move || read_all(&dir))
        };
        std::thread::sleep(std::time::Duration::from_millis(100));
        writer
            .write_all(b"ted\",\"node\":\"a\",\"attempt\":1}\n")
            .unwrap();
        drop(lock);
        let events = reader.join().unwrap().unwrap();
        assert_eq!(events.len(), 2);
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

    #[test]
    fn budget_events_decode_without_optional_fields_and_a_future_amount_shape_reads_as_absent() {
        use crate::budget::kind::{GateBasis, GateDecision};
        let set: Event =
            serde_json::from_str(r#"{"seq":1,"ts":1,"type":"budget_set","tokens":1000}"#).unwrap();
        let EventPayload::BudgetSet {
            tokens,
            usd,
            on_no_estimate,
            source,
            by,
        } = set.payload
        else {
            panic!("a budget_set")
        };
        assert_eq!((tokens, usd, source, by), (Some(1000), None, None, None));
        assert_eq!(on_no_estimate, apb_core::budget::OnNoEstimate::Ask);
        // A policy and a source a newer apb wrote read as the defaults.
        let newer: Event = serde_json::from_str(
            r#"{"seq":1,"ts":1,"type":"budget_set","usd":2.5,"on_no_estimate":"ask_twice","source":"schedule"}"#,
        )
        .unwrap();
        assert!(matches!(
            newer.payload,
            EventPayload::BudgetSet {
                source: None,
                usd: Some(_),
                ..
            }
        ));
        let gate: Event = serde_json::from_str(
            r#"{"seq":2,"ts":1,"type":"budget_gate","node":"a","decision":"paused","basis":"median"}"#,
        )
        .unwrap();
        assert!(matches!(
            gate.payload,
            EventPayload::BudgetGate {
                decision: GateDecision::Paused,
                basis: GateBasis::Median,
                need: None,
                remaining: None,
                ..
            }
        ));
        // A future amount shape, spent shape and decision read as absent.
        let future: Event = serde_json::from_str(
            r#"{"seq":3,"ts":1,"type":"budget_gate","node":"a","decision":"deferred","basis":"forecast","need":{"tokens":"lots"},"remaining":"left","spent":"x"}"#,
        )
        .unwrap();
        let EventPayload::BudgetGate {
            decision,
            basis,
            need,
            remaining,
            spent,
            ..
        } = future.payload
        else {
            panic!("a budget_gate")
        };
        assert_eq!(
            (decision, basis),
            (GateDecision::Unknown, GateBasis::Unknown)
        );
        assert!(need.is_none() && remaining.is_none());
        assert_eq!(spent, crate::budget::BudgetSpent::default());
        // Neither is a checkpoint: each is safe to skip up to the next one.
        let raw = serde_json::from_str::<Event>(r#"{"seq":4,"ts":1,"type":"budget_set"}"#);
        assert!(!raw.unwrap().payload.is_checkpoint());
    }
}
