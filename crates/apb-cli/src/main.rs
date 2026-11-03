/// `println!` for a line a run surface prints: inside
/// [`util::with_run_redaction`] the line loses the run's secret values first
/// (`util::shown`); elsewhere it prints as written.
macro_rules! say {
    ($($arg:tt)*) => {
        println!("{}", $crate::util::shown(&format!($($arg)*)))
    };
}

mod actions;
mod budget;
mod cache;
mod candidate;
mod connector;
mod consent;
mod dashboard_check;
mod demo;
// --- issue #193 host-task decisions ---
mod decide;
// --- end issue #193 ---
mod decisions;
mod discover;
mod doctor;
// --- 0.24.0 eval suites ---
mod eval;
mod evidence;
// --- end 0.24.0 eval suites ---
mod grade;
mod hook;
mod import_preview;
mod intent;
mod interrupt;
mod judge;
mod launch;
mod make_global;
mod manage;
mod memory;
mod notify;
mod onboarding;
mod pause;
mod pipe;
mod problems;
mod profile;
mod publish;
mod report;
mod results;
mod run;
mod run_items;
mod run_outputs;
mod scenario;
// --- 0.24.0 run execution mode lines ---
mod run_mode;
// --- end 0.24.0 run execution mode lines ---
mod selfupdate;
mod serve;
mod server;
mod setup;
// --- 0.23.0 stats (C3) ---
mod stats;
// --- end of 0.23.0 stats ---
mod suggestions;
// host execution mode (0.23.0)
mod tasks;
mod trash;
mod trust;
mod util;
mod version;

use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Parser, Subcommand};

use crate::cache::{CacheCmd, cache_cmd};
use crate::connector::{ConnectorAction, connector_cmd};
use crate::decisions::{DecisionsAction, decisions_cmd};
use crate::intent::{IntentAction, intent_cmd};
use crate::manage::{
    ProjectsAction, adopt_cmd, detect_cmd, export_cmd, import_cmd, migrate_cmd, projects_cmd,
    run_init, subscriptions_cmd,
};
use crate::pause::pause_cmd;
use crate::profile::{ProfileAction, profile_cmd};
use crate::run::{
    DriveRunArgs, answer_cmd, drive_run_child, drive_supervised_child, note_cmd, pause_waker_cmd,
    resume_all_cmd, resume_cmd, review_cmd, run_cmd, run_list, run_validate, runs_cmd, stop_cmd,
    wait_cmd,
};
use crate::selfupdate::run_self_update;
use crate::serve::{ask_server_cmd, dashboard, dev_cmd, ingest_cmd, mcp_cmd};
use crate::server::{ServerAction, server_cmd};
use crate::suggestions::{SuggestionsAction, suggestions_cmd};
use crate::tasks::{TasksAction, tasks_cmd};
use crate::trash::{TrashAction, trash_cmd};
use crate::trust::{TrustAction, trust_cmd};
use crate::util::{resolve_bind, resolve_port};

#[derive(Parser)]
#[command(name = "apb", version, about = "Playbooks CLI")]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    /// Create empty .apb structure. At a terminal without flags it asks
    /// about the standing blocks and subscriptions; the flags answer
    /// without a terminal (a missing flag is never a yes)
    Init {
        /// Write the standing playbook instruction into CLAUDE.md and AGENTS.md
        #[arg(long, value_name = "yes|no", value_parser = ["yes", "no"])]
        standing_block: Option<String>,
        /// Write the feedback-loop block (agents may report apb defects as
        /// issues): only on the person's own answer, so `yes` asks again at
        /// a terminal and is refused (exit 3) without one or inside a run
        /// step; `no` works anywhere
        #[arg(long, value_name = "yes|no", value_parser = ["yes", "no"])]
        feedback_loop: Option<String>,
        /// Declare a subscription, as `apb subscriptions --set` (repeatable)
        #[arg(long = "subscriptions", value_name = "AGENT[:PLAN[:COVERAGE]]")]
        subscriptions: Vec<String>,
        /// Machine-readable output
        #[arg(long)]
        json: bool,
    },
    /// Host hooks: commands an agent host runs on its own events, not
    /// meant to be typed by people
    Hook {
        #[command(subcommand)]
        event: hook::HookEvent,
    },
    /// Agent-first setup: plan (read only), apply a plan by its digest,
    /// remove what setup did
    Setup {
        #[command(subcommand)]
        action: setup::SetupAction,
    },
    /// List or manage agent profiles (spec 2026-07-12)
    Profile {
        #[command(subcommand)]
        action: ProfileAction,
    },
    /// Migrate playbooks from schema 1 (executors) to schema 2 (profiles).
    /// Dry-run by default; pass --apply to write.
    Migrate {
        #[arg(long)]
        apply: bool,
    },
    /// List or manage connectors (spec 2026-07-18)
    Connector {
        #[command(subcommand)]
        action: ConnectorAction,
    },
    /// Detect installed coding agents. Detection itself is local: apb runs each
    /// agent's --version and reads local config, and makes no network request of
    /// its own. It does not control what a spawned agent does when apb runs.
    Detect {
        #[arg(long)]
        refresh: bool,
        /// Machine-readable output only (no survey hint)
        #[arg(long)]
        json: bool,
    },
    /// Will it fit here: the readiness report of a playbook (or of every
    /// project and global playbook) for a run in this project, in four
    /// sections (compatibility, setup, trust, consent) plus the profile and
    /// model findings
    Adopt {
        name: Option<String>,
        /// Only this definition scope: project or global (with NAME: the
        /// playbook's scope, default project)
        #[arg(long)]
        scope: Option<String>,
    },
    /// Show or forget the run input a playbook remembers in this project
    Memory {
        #[command(subcommand)]
        cmd: memory::MemoryCmd,
    },
    /// Make a project playbook available in every project: copies it with
    /// all versions to the global store (digests and trust carry over),
    /// carries this project's remembered input along and applies the chosen
    /// fate of the project copy; a failed step undoes the earlier ones.
    /// Exit codes: 0 done (or a dry run with no blockers), 2 blocked or
    /// refused
    MakeGlobal {
        id: String,
        /// What happens to the project copy: trash (restorable), retire
        /// (the global one wins here) or keep (with its remembered input;
        /// the pair reads ambiguous while both are active and trusted)
        #[arg(long, required_unless_present = "dry_run")]
        variant: Option<String>,
        /// Also copy the project profiles it binds to the global scope
        #[arg(long)]
        move_profiles: bool,
        /// Only print the plan: blockers, actions and warnings
        #[arg(long)]
        dry_run: bool,
        /// Machine-readable output
        #[arg(long)]
        json: bool,
    },
    /// View or declare agent subscriptions (spec 8). Bare command lists them;
    /// on a terminal with no prior survey it offers an interactive one.
    Subscriptions {
        /// Mark the survey declined (not offered again)
        #[arg(long)]
        decline: bool,
        /// Declare a subscription: agent[:plan[:coverage]] (repeatable)
        #[arg(long = "set", value_name = "AGENT[:PLAN[:COVERAGE]]")]
        set: Vec<String>,
        /// Machine-readable output, never a survey
        #[arg(long)]
        json: bool,
    },
    /// Inspect and undo the suggestion decisions the agent recorded
    /// (spec 2026-07-29)
    Suggestions {
        #[command(subcommand)]
        action: SuggestionsAction,
    },
    /// List playbooks and versions
    List,
    /// Validate playbooks, the profiles and scripts they reference, profile
    /// models, requires and connectors
    Validate { name: Option<String> },
    /// Diagnose environment (agents, executors, profiles, runners, playbooks),
    /// or one run's health with --run
    Doctor {
        /// Diagnose this run instead of the environment: folded statuses, open
        /// attempts and their pid liveness, the driver and workdir-lock
        /// holders, unapplied control entries, repeated supervisor actions.
        /// Read-only, like the environment doctor: it repairs nothing.
        #[arg(long, value_name = "ID", conflicts_with_all = ["agents", "probe_agent"])]
        run: Option<String>,
        /// Check every installed built-in agent, whether or not a profile
        /// names it: its version against the verified one and the flags of
        /// its built-in form in its help. Free: no model call.
        #[arg(long, conflicts_with = "probe_agent")]
        agents: bool,
        /// Send one short prompt to this built-in agent and check its output
        /// against what its built-in form promises. One paid model call;
        /// refused inside a run step. Exit 1 when the format drifted.
        #[arg(long, value_name = "AGENT")]
        probe_agent: Option<String>,
        /// The model for --probe-agent (default: the first the catalog lists).
        #[arg(long, requires = "probe_agent")]
        model: Option<String>,
        /// Machine-readable output: `{ ok, checks: [{ status, subject,
        /// detail }] }`
        #[arg(long, conflicts_with = "probe_agent")]
        json: bool,
    },
    /// Export a playbook version (YAML, layout and scripts) to a single bundle file
    Export {
        name: String,
        #[arg(long)]
        version: Option<String>,
        /// Output file; stdout if omitted
        #[arg(long)]
        out: Option<PathBuf>,
    },
    /// Import a playbook bundle file into this project. Always prints a
    /// preview first (definition diff, scripts added, removed or changed,
    /// validation errors, the bundle digest). On a terminal it then asks;
    /// without a terminal it imports only with --accept naming the bundle
    /// digest, and otherwise exits 3 without writing anything. Exit codes:
    /// 0 imported or previewed, 2 refused or error, 3 needs --accept
    Import {
        file: PathBuf,
        /// Do not set the imported version as current
        #[arg(long)]
        no_current: bool,
        /// Only print the preview; write nothing
        #[arg(long, conflicts_with = "accept")]
        preview: bool,
        /// Accept this exact file: its bundle digest as the preview prints
        /// it (`sha256:...`)
        #[arg(long, value_name = "BUNDLE_DIGEST")]
        accept: Option<String>,
    },
    /// List deleted playbooks or restore one with all its versions
    Trash {
        #[command(subcommand)]
        action: TrashAction,
    },
    /// The send intents of a run: list what became of each send, or record
    /// a person's decision on one whose outcome is not known
    Intent {
        #[command(subcommand)]
        action: IntentAction,
    },
    /// List the approvals in the trust store, or revoke them
    Trust {
        #[command(subcommand)]
        action: TrustAction,
    },
    /// Run a playbook
    Run(run::RunArgs),
    /// Host tasks of host-execution-mode runs: list what waits for a host
    /// (all runs, or one), or submit a reply
    #[command(args_conflicts_with_subcommands = true)]
    Tasks {
        #[command(subcommand)]
        action: Option<TasksAction>,
        /// Only this run
        run_id: Option<String>,
        /// Print each task's full prompt and role prompt
        #[arg(long)]
        full: bool,
        /// Machine-readable output (the same objects as MCP pending_tasks)
        #[arg(long)]
        json: bool,
    },
    /// Read an attempt's evidence session: the stored results of the
    /// node's evidence sources, through the bounded reader its claims are
    /// checked against
    Evidence {
        #[command(subcommand)]
        action: evidence::EvidenceAction,
    },
    /// The result snapshots of a run: list them, show one entry's stored
    /// text, or clean up what no stamp names
    #[command(args_conflicts_with_subcommands = true)]
    Results {
        #[command(subcommand)]
        action: Option<results::ResultsAction>,
        /// The run
        run_id: Option<String>,
        /// Only this node's snapshots
        #[arg(long)]
        node: Option<String>,
        /// Print JSON, the same object as MCP run_results
        #[arg(long)]
        json: bool,
    },
    /// List runs, or show one run (its nodes and the token usage its agents
    /// reported)
    Runs {
        /// Show only this run
        run_id: Option<String>,
        /// With a run id: the run as JSON, the same object as MCP run_status
        #[arg(long, requires = "run_id", conflicts_with = "items")]
        json: bool,
        /// With a run id: one line per item of the run's list nodes
        #[arg(long, requires = "run_id")]
        items: bool,
        /// With --items: only this list node
        #[arg(long, requires = "items")]
        node: Option<String>,
        /// With --items: only items in this state (pending, running,
        /// passed, needs_look)
        #[arg(long, requires = "items")]
        state: Option<String>,
        /// With a run id: write the run report as one self-contained HTML
        /// page (no script, no network) to this file, or `-` for standard
        /// output
        #[arg(long, value_name = "PATH", requires = "run_id", conflicts_with_all = ["json", "items"])]
        html: Option<PathBuf>,
        /// With a run id: print exactly the run's projected outputs as JSON
        /// (the playbook's `outputs:` block), for a pipeline to read
        #[arg(long, requires = "run_id", conflicts_with_all = ["json", "items", "html"])]
        outputs_json: bool,
    },
    /// Notifications about runs ("Notify me"): without arguments, the
    /// machine's notify target and whether runs here notify by default;
    /// with a run id, that run's notifications, or a change of them with
    /// --on, --off, --via or --to; `apb notify test` sends one test message
    /// through the target
    Notify {
        /// A run id, or `test`
        run: Option<String>,
        /// Turn notifications on; with a comma-separated list joined by `=`
        /// (--on=review,finished) only those transitions
        #[arg(long, value_name = "LIST", num_args = 0..=1, require_equals = true, default_missing_value = "", conflicts_with = "off")]
        on: Option<String>,
        /// Turn notifications off for the run
        #[arg(long, conflicts_with_all = ["via", "to", "finished"])]
        off: bool,
        /// Send the end-of-run notice only for these outcomes, comma
        /// separated: succeeded, needs_look, failed, aborted
        #[arg(long, value_name = "LIST", require_equals = true)]
        finished: Option<String>,
        /// Send through this connector account: <connector>/<account>[:function]
        #[arg(long, value_name = "CONNECTOR/ACCOUNT[:FUNCTION]")]
        via: Option<String>,
        /// The recipient (a chat id, an address)
        #[arg(long, value_name = "TO", allow_hyphen_values = true)]
        to: Option<String>,
    },
    /// Pause a run so it can continue later (issue #200). Default `--drain`:
    /// no new node starts, running nodes finish, then the run is paused and
    /// holds no process. `--now` interrupts running nodes, which run again
    /// on resume. Clocks (node and host-task deadlines, stats) freeze while
    /// paused. Continue with `apb resume`
    Pause {
        run_id: String,
        /// Admit no new node, let the running ones finish (the default)
        #[arg(long, conflicts_with = "now")]
        drain: bool,
        /// Interrupt the running nodes (SIGTERM, a grace period, then
        /// SIGKILL); they run again on resume and no attempt counts as failed
        #[arg(long)]
        now: bool,
        /// Why the run is paused; shown by `apb runs` and `apb wait`
        #[arg(long)]
        reason: Option<String>,
        /// Resume on its own at this time: an RFC 3339 timestamp or a
        /// duration from now (`90m`, `2h`)
        #[arg(long, value_name = "TIME|DURATION")]
        until: Option<String>,
        /// Keep the workdir busy lock while paused, so no other run can touch
        /// the tree (default: released)
        #[arg(long)]
        keep_lock: bool,
    },
    /// Resume a paused/interrupted run
    Resume {
        #[arg(required_unless_present = "all")]
        run_id: Option<String>,
        /// Resume every run of this workspace a lost driver or a passed
        /// pause `until` left waiting (runs reading `running` with no live
        /// driver, paused runs whose `until` has passed); sub-playbook
        /// children follow their parent. A driverless run that cannot be
        /// resumed is marked `paused (driverless)`.
        #[arg(long, conflicts_with_all = ["run_id", "from_node", "retry_now"])]
        all: bool,
        #[arg(long)]
        from_node: Option<String>,
        /// Start a deferred retry now instead of at its due time: on a run a
        /// live driver holds the retry starts at once, a released or paused
        /// run is resumed with the retry due
        #[arg(long, conflicts_with = "from_node")]
        retry_now: bool,
        /// With --retry-now: only this node's waiting retry (default: every
        /// waiting retry of the run)
        #[arg(long, requires = "retry_now")]
        node: Option<String>,
        /// Continue even though an agent binary changed on disk since the run
        /// started (environment drift). The run manifest fingerprints every
        /// agent executable at start; a different fingerprint at resume means
        /// the agent that would drive a node is no longer the one recorded, so
        /// resume refuses by default. Set this to override and proceed anyway
        /// (the accepted drift is recorded as an event in the run log).
        #[arg(long = "allow-environment-drift")]
        allow_environment_drift: bool,
        /// Consent to the run's irreversible effects when it has no valid
        /// consent recorded (a run an older apb started, or a run directory
        /// apb did not create here): the consent_nonce the refusal printed.
        /// At a terminal, apb asks instead
        #[arg(
            long,
            value_name = "CONSENT_NONCE",
            num_args = 0..=1,
            default_missing_value = "true",
            require_equals = true
        )]
        confirm_irreversible: Option<String>,
        #[command(flatten)]
        budget: budget::ResumeBudgetFlags,
        #[command(flatten)]
        launch: launch::LaunchFlags,
    },
    /// Block until a run finishes, needs input (a question, a review, a
    /// supervisor decision) or stops, then print why. A single call that
    /// costs an agent nothing while it blocks, unlike polling `apb runs`.
    /// Exit codes: 0 succeeded, 1 failed or aborted, 3 needs input,
    /// 4 paused, driverless or released while a deferred retry waits,
    /// 5 timeout, 2 error
    Wait {
        run_id: String,
        /// Give up after this many seconds (default: no limit)
        #[arg(long, value_name = "SECS")]
        timeout: Option<u64>,
        /// Keep waiting while the run and its live sub-playbook runs go on by
        /// themselves (a timer, a question that gets its default, a
        /// deferred retry, a pause with `until` or a release up to a minute
        /// past its time); return when they finish (0
        /// or 1), need input, also a paused run with an open gate or a
        /// child's gate (3), or stop (4)
        #[arg(long)]
        follow_auto: bool,
    },
    /// Stop a run: interrupt whatever node it is executing right now, and
    /// finalize it outright if the process driving it is gone
    Stop { run_id: String },
    /// Post a supervisor note (ContextAppend) to a run's control channel
    Note { run_id: String, text: String },
    /// Decide a human_review or publish node of a running run. A gate
    /// marked person_only is decided only at an interactive terminal
    /// outside a run step, after typing the option to confirm
    Review {
        run_id: String,
        node_id: String,
        #[arg(long)]
        decision: String,
        #[arg(long, default_value = "")]
        note: String,
        /// The nonce of what you checked: a publish node's plan (see `apb
        /// publish show`), or the result snapshots a gate with `approves`
        /// binds (an approving decision needs it; see `apb runs <id>`)
        #[arg(long)]
        nonce: Option<String>,
        /// The request this answers, as a next action names it
        /// (`<kind>:<seq>`): refused when a later round replaced it or it
        /// already has an answer
        #[arg(long, value_name = "KIND:SEQ")]
        request: Option<crate::actions::RequestArg>,
    },
    /// The publish gate of a run: show the exact plan, consent to it by its
    /// nonce, or skip the push
    Publish {
        #[command(subcommand)]
        action: crate::publish::PublishAction,
    },
    /// Answer an interactive node's pending question in a running run
    Answer {
        run: String,
        /// The interactive node; omit when exactly one question is pending
        #[arg(long)]
        node: Option<String>,
        /// The request this answers, as a next action names it
        /// (`<kind>:<seq>`): refused when a later round replaced it or it
        /// already has an answer
        #[arg(long, value_name = "KIND:SEQ")]
        request: Option<crate::actions::RequestArg>,
        text: String,
    },
    /// Ask the configured decision providers a bounded question (choose,
    /// rank, filter, map, is, score), from a host task or a script; inside a
    /// run (APB_RUN_ID) it is journaled there and the run's budget applies
    Decide(crate::decide::DecideArgs),
    /// Measure decision-model uses (issue #165): the report, the stored
    /// thresholds, replay against another provider
    Decisions {
        #[command(subcommand)]
        action: DecisionsAction,
    },
    /// Grade an ended run by rules (the built-in quality criteria: outcome,
    /// loops, scope, consent, skill use) or, with --model, its playbook's
    /// own criteria by the project's decision model; show its stored
    /// grades, or grade a playbook's earlier runs so a candidate comparison
    /// has a history. A stored grade re-decides the playbook's candidate
    /// under a quality policy. By rules no model is called
    Grade(crate::grade::GradeArgs),
    /// A playbook's known problems: trouble with its tools that earlier
    /// runs reported. Without a subcommand, list them; confirm, dismiss,
    /// reopen or close one (a close quotes the source's new text); recheck
    /// the ones whose source changed; scan earlier runs to collect them
    Problems(crate::problems::ProblemsArgs),
    /// Find step sequences your past agent sessions repeat (Claude Code and
    /// Codex history of the named projects) and offer them as playbook
    /// drafts; list or show stored results, save or decline a suggestion.
    /// No model and no network call
    Discover(crate::discover::DiscoverCmdArgs),
    /// Show a playbook's candidate version and how it compares with
    /// current by quality, or decide it: --promote makes it current,
    /// --keep-current drops it and changes nothing
    Candidate(crate::candidate::CandidateArgs),
    /// Install, run or remove the keyless demo
    Demo(crate::demo::DemoArgs),
    /// Try a judge node outside any run (eval), list and check its
    /// labelled examples (examples), save a run's judge execution as one
    /// (save-example), or show what a run's judge execution read (values)
    Judge(crate::judge::JudgeArgs),
    /// A playbook's versions and its current one: list them, make one
    /// current (use, with --expect and --reason), or show the moves of
    /// current (history)
    Version(crate::version::VersionArgs),
    // --- 0.23.0 stats (C3) ---
    /// Cross-run metrics per playbook version and node, from the run
    /// journals only: outcome rate, first-pass success, retries, fallbacks
    /// and loops per run, gate and question waits, durations against
    /// expected_duration, tokens and cost, goal results. Read-only
    Stats {
        /// Only runs of this playbook id
        #[arg(long)]
        playbook: Option<String>,
        /// Only runs started since a date (2026-09-20, UTC) or a duration
        /// back from now (30d, 24h)
        #[arg(long)]
        since: Option<String>,
        /// Compare this version with the latest other version (needs
        /// --playbook)
        #[arg(long, value_name = "VERSION")]
        compare: Option<String>,
        /// Every registered project, not only this one
        #[arg(long)]
        all_projects: bool,
        /// Machine-readable output
        #[arg(long)]
        json: bool,
    },
    // --- end of 0.23.0 stats ---
    // --- 0.24.0 eval suites ---
    /// Run a playbook's eval cases (`.apb/playbooks/<id>/evals/`), each
    /// repetition as a run in a disposable repository, check the outcomes,
    /// store the result per configuration and compare it with the previous
    /// one. Opt-in and paid: every repetition is a full agent run
    Eval {
        /// Playbook id (a project playbook)
        id: String,
        /// Version to evaluate (default: current)
        #[arg(long)]
        version: Option<String>,
        /// Only these cases (repeatable)
        #[arg(long = "case", value_name = "NAME")]
        cases: Vec<String>,
        /// Only cases with one of these tags (repeatable)
        #[arg(long = "tag")]
        tags: Vec<String>,
        /// Repetitions per case, over the case and suite `repeat`
        #[arg(long)]
        repeat: Option<u32>,
        /// Run-level overrides YAML file, as for `apb run --overrides`
        #[arg(long)]
        overrides: Option<PathBuf>,
        /// NODE=PROFILE: another profile for one agent node (repeatable)
        #[arg(long = "profile-override", value_name = "NODE=PROFILE")]
        profile_overrides: Vec<String>,
        /// AGENT:MODEL: one ephemeral executor for every agent node
        #[arg(long, value_name = "AGENT:MODEL")]
        model: Option<String>,
        /// Invocation budget in USD (default: suite.yaml, else 10)
        #[arg(long)]
        max_usd: Option<f64>,
        /// Run nothing: compare the latest stored result with the one before
        #[arg(long)]
        compare: bool,
        /// Print the plan without starting anything
        #[arg(long)]
        dry_run: bool,
        /// Evaluate a draft playbook (only the scratch copy is marked active)
        #[arg(long)]
        draft: bool,
        /// Approve the suite and its cost and start without asking. It is
        /// not a launch confirmation: a playbook that asks before an agent
        /// launches it needs --confirm-launch (or the terminal question)
        #[arg(long)]
        yes: bool,
        /// Machine-readable output
        #[arg(long)]
        json: bool,
        #[command(flatten)]
        launch: launch::LaunchFlags,
        /// deep: the eval cases as real runs (default); quick: the
        /// scenarios with every answer scripted, nothing started or sent;
        /// smoke: quick plus this machine's decision providers, capped
        #[arg(long, value_enum, default_value_t = eval::EvalLevel::Deep)]
        level: eval::EvalLevel,
        /// Only these scenarios (repeatable; quick and smoke)
        #[arg(long = "scenario", value_name = "NAME")]
        scenarios: Vec<String>,
        /// List the scenarios with their latest result per level (takes
        /// --version and --json only: it runs nothing)
        #[arg(
            long = "scenarios",
            conflicts_with_all = [
                "cases", "tags", "repeat", "overrides", "profile_overrides", "model",
                "max_usd", "compare", "dry_run", "draft", "yes", "level", "scenarios",
                "max_decision_requests", "allow_stale", "confirm_launch",
                "launch_version", "launch_choice",
            ]
        )]
        list_scenarios: bool,
        /// Smoke: decision requests per scenario (default 20, at most 200)
        #[arg(long)]
        max_decision_requests: Option<u32>,
        /// Play a scenario whose versions or digest pin exclude this version
        #[arg(long)]
        allow_stale: bool,
    },
    // --- end 0.24.0 eval suites ---
    /// Inspect and manage the project-local node result cache
    Cache {
        #[command(subcommand)]
        cmd: CacheCmd,
    },
    /// Start the web dashboard (global, all projects)
    #[command(alias = "serve")]
    Dashboard {
        /// Port: the flag overrides the global config, default 7321.
        #[arg(long)]
        port: Option<u16>,
        /// IP address to bind: the flag overrides `server.bind` in the global
        /// config, default 127.0.0.1. Any non-loopback address requires at
        /// least one key from `apb server key issue`.
        #[arg(long)]
        bind: Option<String>,
        #[arg(long)]
        no_open: bool,
    },
    /// Start only the inbound webhook listener (headless deployments). The
    /// dashboard co-starts it by itself when `ingest.enabled` is true.
    Ingest {
        /// IP address to bind: the flag overrides `ingest.bind` in the global
        /// config, default 127.0.0.1 (behind a reverse proxy on the same host).
        #[arg(long)]
        bind: Option<String>,
        /// Port: the flag overrides `ingest.port`, default 7322.
        #[arg(long)]
        port: Option<u16>,
    },
    /// Manage server mode: the API keys that authenticate a networked dashboard
    Server {
        #[command(subcommand)]
        action: ServerAction,
    },
    /// Dev mode: Vite HMR frontend + API server (source tree only)
    Dev {
        #[arg(long)]
        no_open: bool,
    },
    /// Start stdio MCP server for the current project
    Mcp {
        /// Serve only the tools that read, in a process that writes nothing,
        /// starts no program and sends no request
        #[arg(long)]
        read_only: bool,
    },
    /// Update apb to the latest released version
    SelfUpdate {
        /// Report whether an update is available without installing it
        #[arg(long)]
        check: bool,
    },
    /// A playbook's launch floor: the strictest `agent_launch` any saved
    /// version declared, which keeps an agent from launching a looser copy.
    /// Lowering it is a person's decision, asked at an interactive terminal
    Launch {
        #[command(subcommand)]
        cmd: LaunchCmd,
    },
    /// List or manage the workspace registry (spec 6)
    Projects {
        #[command(subcommand)]
        action: Option<ProjectsAction>,
    },
    /// Internal: actually drives a supervised background run to completion.
    /// Spawned as a detached child process by `run --supervise` (see
    /// `spawn_detached_supervised`) so the run survives after the invoking
    /// CLI process exits - std::thread cannot outlive its process, so the
    /// real drive loop has to happen in a separate one. Not part of the
    /// public CLI surface.
    #[command(hide = true, name = "__drive-supervised")]
    DriveSupervised {
        name: String,
        #[arg(long)]
        version: Option<String>,
        #[arg(long)]
        instruction: Option<String>,
        #[arg(long = "param", value_name = "K=V")]
        params: Vec<String>,
        #[arg(long)]
        allow_shared_workdir: bool,
        /// Predecessor run id for lineage (issue #42 finding 10). Forwarded
        /// from `apb run --supervise --continued-from` across the detached
        /// spawn boundary.
        #[arg(long = "continued-from", value_name = "RUN_ID")]
        continued_from: Option<String>,
        /// The run's working tree, forwarded from `apb run --worktree`.
        #[arg(long, value_name = "DIR")]
        worktree: Option<String>,
        /// The handoff file holding the launch decision and the irreversible
        /// consent `apb run --supervise` obtained (`apb_engine::launch_handoff`):
        /// read once, deleted, checked against this process's own gate.
        #[arg(long = "launch-handoff", value_name = "FILE")]
        launch_handoff: Option<PathBuf>,
        /// Handshake file: written with the run_id as soon as the run is
        /// prepared (before drive starts), so the parent process can report
        /// it and exit without waiting for the run itself to finish.
        #[arg(long)]
        handshake: PathBuf,
        /// The run's budget argument, forwarded from `apb run --supervise`.
        #[command(flatten)]
        budget: budget::BudgetFlags,
        /// The run's notify flags, forwarded from `apb run --supervise`.
        #[command(flatten)]
        notify: notify::NotifyFlags,
    },
    /// Drives an already-prepared run at `<root>/.apb/runs/<run-id>` to
    /// completion in THIS process. Spawned detached by
    /// `apb_engine::driver::spawn_detached_driver`, so that a run started from
    /// a chat session (MCP) survives that session dying. Hidden: an internal
    /// re-exec target, not a user-facing command.
    #[command(hide = true, name = "__drive-run")]
    DriveRun {
        /// Project root holding `.apb/runs` (absolute: the parent resolves it).
        #[arg(long)]
        root: PathBuf,
        #[arg(long = "run-id")]
        run_id: String,
        /// Passed through to the resume planner; only meaningful with `--resume`.
        #[arg(long = "from-node")]
        from_node: Option<String>,
        /// Resume an existing run instead of driving a freshly prepared one.
        #[arg(long)]
        resume: bool,
        /// Forwarded from `apb resume --allow-environment-drift` across the
        /// detached spawn boundary: lets the resumed child write its accepted
        /// drift events instead of refusing.
        #[arg(long = "allow-environment-drift")]
        allow_environment_drift: bool,
        /// Who resumes the run (`operator`, `supervisor`, `engine`), journaled
        /// on `run_resumed` (issue #200). Only meaningful with `--resume`.
        #[arg(long)]
        by: Option<String>,
        /// The engine's resume of one pause (the waker, `resume --all`): the
        /// journal sequence of the `run_paused` that must still hold the run
        /// when `run_resumed` is journaled. Only meaningful with `--resume`.
        #[arg(long = "pause-seq", conflicts_with_all = ["release_seq", "retry_now", "budget_json"])]
        pause_seq: Option<u64>,
        /// The engine's resume of one release (the waker, `resume --all`):
        /// the journal sequence of the `drive_released` that must still hold
        /// the run when `run_resumed` is journaled. The engine's resume
        /// takes no retry-now and no budget change.
        #[arg(long = "release-seq", conflicts_with_all = ["retry_now", "budget_json"])]
        release_seq: Option<u64>,
        /// Start the waiting retries now, journaled with `run_resumed`.
        /// Takes `--retry-now-by`: a retry-now that names nobody is refused,
        /// never recorded as a person's.
        #[arg(long = "retry-now", requires = "retry_now_by")]
        retry_now: bool,
        /// With `--retry-now`: only this node's waiting retry.
        #[arg(long = "retry-now-node", requires = "retry_now")]
        retry_now_node: Option<String>,
        /// With `--retry-now`: who asked (`operator`, `host`, `supervisor`,
        /// `engine`); any other value is refused, never recorded as a person.
        #[arg(long = "retry-now-by", requires = "retry_now", value_parser = parse_expedite_by)]
        retry_now_by: Option<String>,
        /// The budget change of the resume, as JSON, journaled next to its
        /// `run_resumed`. Only meaningful with `--resume`.
        #[arg(long = "budget-json")]
        budget_json: Option<String>,
        /// The handoff file holding the resume's launch grant
        /// (`apb_engine::launch_handoff`). Only meaningful with `--resume`.
        #[arg(long = "launch-handoff", value_name = "FILE")]
        launch_handoff: Option<PathBuf>,
    },
    /// The waker of a paused run with an `until` (issue #200): sleeps until
    /// then and resumes the run only if the same pause still holds it. Spawned
    /// detached by the driver that journaled the pause. Holds no lock and runs
    /// no node. Hidden: an internal re-exec target.
    #[command(hide = true, name = "__pause-waker")]
    PauseWaker {
        /// Project root holding `.apb/runs` (absolute).
        #[arg(long)]
        root: PathBuf,
        #[arg(long = "run-id")]
        run_id: String,
        /// The journal sequence of the `run_paused` this waker belongs to.
        #[arg(
            long,
            required_unless_present_any = ["release", "notify"],
            conflicts_with_all = ["release", "notify"]
        )]
        seq: Option<u64>,
        /// The journal sequence of the `drive_released` this waker belongs
        /// to, instead of a pause.
        #[arg(long, conflicts_with = "notify")]
        release: Option<u64>,
        /// Retry the notifications the drive's last sweep could not send,
        /// instead of resuming anything.
        #[arg(long)]
        notify: bool,
    },
    /// One scenario run (`apb eval --level quick|smoke`), in scenario
    /// mode: writes only below `.apb/scenario-runs/`, starts no program and
    /// prints its report. Spawned by `apb eval`. Hidden: an internal re-exec
    /// target.
    #[command(hide = true, name = "__scenario-run")]
    ScenarioRun {
        /// Project root (absolute).
        #[arg(long)]
        root: PathBuf,
        #[arg(long)]
        playbook: String,
        #[arg(long)]
        version: String,
        #[arg(long)]
        scenario: String,
        #[arg(long, value_enum)]
        level: eval::EvalLevel,
        #[arg(long)]
        scenario_digest: String,
        #[arg(long)]
        playbook_digest: String,
        #[arg(long)]
        max_decision_requests: Option<u32>,
    },
    /// The live-question sidecar (spec 2026-07-20-interactive-nodes, Task 10):
    /// a stdio MCP server exposing one `ask_user` tool, injected into the
    /// coding agent that runs a live interactive `agent_task` node. Resolves
    /// the run directory from `APB_RUN_DIR` (inherited from the agent). Hidden:
    /// an internal injection target, not a user-facing command.
    #[command(hide = true, name = "__ask-server")]
    AskServer {
        #[arg(long)]
        run: String,
        #[arg(long)]
        node: String,
        #[arg(long)]
        attempt: u32,
    },
    /// Brings the default playbooks and skills setup installed in line with
    /// this binary. `apb self-update` runs it in the updated binary, so the
    /// new content comes from the new build. Hidden: an internal re-exec
    /// target.
    #[command(hide = true, name = "__reconcile-defaults")]
    ReconcileDefaults,
}

/// `apb launch` subcommands.
#[derive(Subcommand)]
enum LaunchCmd {
    /// Print a playbook's launch floor, or lower it (a person, at an
    /// interactive terminal; refused inside a run step)
    Floor {
        id: String,
        /// The global playbook of that id
        #[arg(long)]
        global: bool,
        /// The policy to lower the floor to: allow, ask or deny
        #[arg(long = "lower-to", value_name = "POLICY")]
        lower_to: Option<String>,
    },
}

/// Whether this invocation auto-registers its cwd in the project registry.
/// The hidden re-exec targets (`__drive-supervised`, `__drive-run`,
/// `__ask-server`) are spawned by an apb process that already registered the
/// project, so they must not: `__drive-run` works on its `--root`, not its cwd,
/// and `__ask-server` inherits the coding agent's cwd, which can be any
/// directory.
/// `--retry-now-by`: one of the names `retry_expedited.by` records.
fn parse_expedite_by(s: &str) -> Result<String, String> {
    apb_engine::scheduler::deferred::ExpediteBy::parse(s)
        .map(|b| b.as_str().to_string())
        .ok_or_else(|| format!("`{s}` is not one of operator, host, supervisor, engine"))
}

fn registers_workspace(command: Option<&Command>) -> bool {
    !matches!(
        command,
        Some(
            Command::DriveSupervised { .. }
                | Command::DriveRun { .. }
                | Command::PauseWaker { .. }
                | Command::ScenarioRun { .. }
                | Command::AskServer { .. }
                | Command::ReconcileDefaults
                | Command::Setup {
                    action: setup::SetupAction::Plan { .. }
                }
        )
    )
}

/// The hidden name of the sandbox helper: bwrap runs `apb __sandbox-exec
/// [--scope] [--require-scope] -- <agent argv>` inside an isolation
/// sandbox (`apb_engine::sandbox::scope::exec_main`). Dispatched before
/// argument parsing and before anything touches the config or the registry:
/// inside the sandbox neither is visible, and the helper must only scope
/// itself and exec the agent.
const SANDBOX_EXEC: &str = "__sandbox-exec";

fn main() -> ExitCode {
    pipe::install();
    let args: Vec<std::ffi::OsString> = std::env::args_os().collect();
    if args.get(1).is_some_and(|a| a == SANDBOX_EXEC) {
        let code = apb_engine::sandbox::scope::exec_main(&args[2..]);
        return ExitCode::from(u8::try_from(code).unwrap_or(1));
    }
    let cli = Cli::parse();
    // A host hook answers before anything touches the working directory,
    // the config or the registry, and never fails its host.
    if let Some(Command::Hook { event }) = &cli.command {
        return hook::run(event);
    }
    let root = std::env::current_dir().expect("cwd");
    // A read-only server enters read-only mode before anything else runs:
    // no workspace registration, no installation key, nothing written.
    if matches!(cli.command, Some(Command::Mcp { read_only: true })) {
        apb_engine::read_only::enter();
        return serve::mcp_cmd(&root, apb_mcp::server::ServeMode::ReadOnly);
    }
    // Auto-register the workspace in the project registry (spec 6.2). Only for
    // existing projects, so we don't clutter the registry with every directory
    // where `playbook` was run. Best-effort: does not fail or slow down the
    // command. Done at the process entry point rather than in WfMcp::new, so
    // that constructing the server in tests does not write to the real
    // ~/.config/playbook.
    if registers_workspace(cli.command.as_ref()) && root.join(".apb").is_dir() {
        apb_core::projects::touch(&root);
    }
    // The key that stamps the runs this installation creates (see
    // `apb_core::run_origin`), so an MCP resume can refuse a run directory
    // that came with a repository. Created on first use; best effort, like the
    // registration above: without it runs are simply unstamped.
    if registers_workspace(cli.command.as_ref()) {
        let _ = apb_core::run_origin::ensure_key();
    }
    match cli.command {
        Some(Command::Init {
            standing_block,
            feedback_loop,
            subscriptions,
            json,
        }) => run_init(
            &root,
            &crate::manage::InitFlags {
                standing_block: standing_block.map(|v| v == "yes"),
                feedback_loop: feedback_loop.map(|v| v == "yes"),
                subscriptions,
                json,
            },
        ),
        Some(Command::Setup { action }) => setup::setup_cmd(&root, action),
        Some(Command::Hook { .. }) => unreachable!("a hook returns before the workspace setup"),
        Some(Command::List) => run_list(&root),
        Some(Command::Validate { name }) => run_validate(&root, name),
        Some(Command::Doctor {
            run,
            agents,
            probe_agent,
            model,
            json,
        }) => doctor::doctor_cmd(
            &root,
            doctor::DoctorArgs {
                run,
                agents,
                probe_agent,
                model,
                json,
            },
        ),
        Some(Command::Export { name, version, out }) => {
            export_cmd(&root, &name, version.as_deref(), out.as_deref())
        }
        Some(Command::Import {
            file,
            no_current,
            preview,
            accept,
        }) => import_cmd(
            &root,
            &file,
            &crate::manage::ImportFlags {
                make_current: !no_current,
                preview_only: preview,
                accept: accept.as_deref(),
            },
        ),
        Some(Command::Trash { action }) => trash_cmd(&root, action),
        Some(Command::Trust { action }) => trust_cmd(&root, action),
        Some(Command::Intent { action }) => intent_cmd(&root, action),
        Some(Command::Run(args)) => run_cmd(&root, args),
        Some(Command::Tasks {
            action,
            run_id,
            full,
            json,
        }) => tasks_cmd(&root, action, run_id, full, json),
        Some(Command::Evidence { action }) => evidence::evidence_cmd(&root, action),
        Some(Command::Results {
            action,
            run_id,
            node,
            json,
        }) => results::results_cmd(&root, action, run_id, node, json),
        Some(Command::Runs {
            run_id: Some(run_id),
            items: true,
            node,
            state,
            ..
        }) => run_items::items_cmd(
            &root,
            &run_id,
            &run_items::ItemFilter {
                node: node.as_deref(),
                state: state.as_deref(),
            },
        ),
        Some(Command::Runs {
            run_id: Some(run_id),
            html: Some(out),
            ..
        }) => report::html_cmd(&root, &run_id, &out),
        Some(Command::Runs {
            run_id: Some(run_id),
            outputs_json: true,
            ..
        }) => run_outputs::outputs_json_cmd(&root, &run_id),
        Some(Command::Runs { run_id, json, .. }) => runs_cmd(&root, run_id.as_deref(), json),
        Some(Command::Notify {
            run,
            on,
            off,
            finished,
            via,
            to,
        }) => notify::notify_cmd(
            &root,
            run.as_deref(),
            &notify::NotifyChange {
                on: on.as_deref(),
                off,
                finished: finished.as_deref(),
                via: via.as_deref(),
                to: to.as_deref(),
            },
        ),
        Some(Command::Pause {
            run_id,
            now,
            reason,
            until,
            keep_lock,
            ..
        }) => pause_cmd(&root, &run_id, now, reason, until.as_deref(), keep_lock),
        Some(Command::Resume {
            all: true,
            budget,
            launch,
            ..
        }) => match budget.request() {
            Ok(None) => resume_all_cmd(&root, &launch),
            _ => {
                eprintln!(
                    "--all takes no budget flags: change a run's budget with `apb resume <run-id>`"
                );
                ExitCode::from(2)
            }
        },
        Some(Command::Resume {
            run_id,
            from_node,
            retry_now,
            node,
            allow_environment_drift,
            confirm_irreversible,
            budget,
            launch,
            ..
        }) => resume_cmd(
            &root,
            run_id.as_deref().unwrap_or_default(),
            from_node.as_deref(),
            retry_now.then_some(run::RetryNowArg {
                node: node.as_deref(),
            }),
            allow_environment_drift,
            confirm_irreversible,
            &budget,
            &launch,
        ),
        Some(Command::Launch {
            cmd:
                LaunchCmd::Floor {
                    id,
                    global,
                    lower_to,
                },
        }) => launch::floor_cmd(&root, &id, global, lower_to.as_deref()),
        Some(Command::Stop { run_id }) => stop_cmd(&root, &run_id),
        Some(Command::Wait {
            run_id,
            timeout,
            follow_auto,
        }) => wait_cmd(&root, &run_id, timeout, follow_auto),
        Some(Command::Note { run_id, text }) => note_cmd(&root, &run_id, &text),
        Some(Command::Review {
            run_id,
            node_id,
            decision,
            note,
            nonce,
            request,
        }) => review_cmd(
            &root,
            &run_id,
            &node_id,
            &decision,
            &note,
            nonce.as_deref(),
            request,
        ),
        Some(Command::Publish { action }) => crate::publish::publish_cmd(&root, action),
        Some(Command::Answer {
            run,
            node,
            request,
            text,
        }) => answer_cmd(&root, &run, node.as_deref(), &text, request),
        Some(Command::Dashboard {
            port,
            bind,
            no_open,
        }) => match resolve_bind(bind.as_deref()) {
            Ok(addr) => dashboard(addr, resolve_port(port), no_open),
            Err(e) => {
                eprintln!("dashboard failed: {e}");
                ExitCode::from(2)
            }
        },
        Some(Command::Ingest { bind, port }) => ingest_cmd(bind.as_deref(), port),
        Some(Command::Server { action }) => server_cmd(action),
        Some(Command::Dev { no_open }) => dev_cmd(root, no_open),
        Some(Command::Mcp { .. }) => mcp_cmd(&root, apb_mcp::server::ServeMode::Normal),
        Some(Command::SelfUpdate { check }) => run_self_update(check),
        Some(Command::Projects { action }) => projects_cmd(action),
        Some(Command::Profile { action }) => profile_cmd(&root, action),
        Some(Command::Connector { action }) => connector_cmd(&root, action),
        Some(Command::Cache { cmd }) => cache_cmd(&root, cmd),
        Some(Command::Decide(args)) => crate::decide::decide_cmd(&root, args),
        Some(Command::Decisions { action }) => decisions_cmd(&root, action),
        Some(Command::Grade(args)) => grade::grade_cmd(&root, args),
        Some(Command::Candidate(args)) => candidate::candidate_cmd(&root, args),
        Some(Command::Demo(args)) => demo::demo_cmd(args),
        Some(Command::Judge(args)) => judge::judge_cmd(&root, args),
        Some(Command::Version(args)) => version::version_cmd(&root, args),
        Some(Command::Problems(args)) => problems::problems_cmd(&root, args),
        Some(Command::Discover(args)) => discover::discover_cmd(&root, args),
        // --- 0.23.0 stats (C3) ---
        Some(Command::Stats {
            playbook,
            since,
            compare,
            all_projects,
            json,
        }) => stats::stats_cmd(
            &root,
            playbook,
            since.as_deref(),
            compare,
            all_projects,
            json,
        ),
        // --- end of 0.23.0 stats ---
        // --- 0.24.0 eval suites ---
        Some(Command::Eval {
            id,
            version,
            cases,
            tags,
            repeat,
            overrides,
            profile_overrides,
            model,
            max_usd,
            compare,
            dry_run,
            draft,
            yes,
            json,
            launch,
            level,
            scenarios,
            list_scenarios,
            max_decision_requests,
            allow_stale,
        }) => scenario::eval_entry(
            &root,
            eval::EvalArgs {
                id,
                version,
                cases,
                tags,
                repeat,
                overrides,
                profile_overrides,
                model,
                max_usd,
                compare,
                dry_run,
                draft,
                yes,
                json,
                launch,
                launch_grant: None,
                level,
                scenarios,
                list_scenarios,
                max_decision_requests,
                allow_stale,
            },
        ),
        // --- end 0.24.0 eval suites ---
        Some(Command::Migrate { apply }) => migrate_cmd(&root, apply),
        Some(Command::Detect { refresh, json }) => detect_cmd(refresh, json),
        Some(Command::Adopt { name, scope }) => adopt_cmd(&root, name.as_deref(), scope.as_deref()),
        Some(Command::Memory { cmd }) => memory::memory_cmd(&root, cmd),
        Some(Command::MakeGlobal {
            id,
            variant,
            move_profiles,
            dry_run,
            json,
        }) => make_global::make_global_cmd(
            &root,
            make_global::MakeGlobalArgs {
                id,
                variant,
                move_profiles,
                dry_run,
                json,
            },
        ),
        Some(Command::Subscriptions { decline, set, json }) => {
            subscriptions_cmd(set, decline, json)
        }
        Some(Command::Suggestions { action }) => suggestions_cmd(&root, action),
        Some(Command::DriveSupervised {
            name,
            version,
            instruction,
            params,
            allow_shared_workdir,
            continued_from,
            worktree,
            launch_handoff,
            handshake,
            budget,
            notify,
        }) => drive_supervised_child(
            &root,
            &name,
            version.as_deref(),
            instruction,
            params,
            allow_shared_workdir,
            continued_from,
            worktree,
            launch_handoff.as_deref(),
            &handshake,
            &budget,
            &notify,
        ),
        // Deliberately uses the `--root` it was given, not the process cwd:
        // the spawning parent knows which project the run belongs to.
        Some(Command::DriveRun {
            root: run_root,
            run_id,
            from_node,
            resume,
            allow_environment_drift,
            by,
            pause_seq,
            release_seq,
            retry_now,
            retry_now_node,
            retry_now_by,
            budget_json,
            launch_handoff,
        }) => drive_run_child(
            &run_root,
            &run_id,
            &DriveRunArgs {
                from_node: from_node.as_deref(),
                resume,
                allow_environment_drift,
                by: by.as_deref(),
                hold: match (pause_seq, release_seq) {
                    (Some(seq), _) => Some(apb_engine::pause::WakeHold::Pause(seq)),
                    (None, Some(seq)) => Some(apb_engine::pause::WakeHold::Release(seq)),
                    (None, None) => None,
                },
                retry_now: retry_now.then(|| apb_engine::scheduler::RetryNow {
                    node: retry_now_node.as_deref(),
                    // Required with `--retry-now` and validated by
                    // `parse_expedite_by`, so never taken for a person's;
                    // inside a run step never a person's, whatever it says.
                    by: match retry_now_by
                        .as_deref()
                        .and_then(apb_engine::scheduler::deferred::ExpediteBy::parse)
                    {
                        Some(apb_engine::scheduler::deferred::ExpediteBy::Operator)
                            if apb_core::step_env::StepCaller::from_env()
                                .describe()
                                .is_some() =>
                        {
                            apb_engine::scheduler::deferred::ExpediteBy::Host
                        }
                        Some(by) => by,
                        None => apb_engine::scheduler::deferred::ExpediteBy::Host,
                    },
                }),
                budget_json: budget_json.as_deref(),
                launch_handoff: launch_handoff.as_deref(),
            },
        ),
        Some(Command::PauseWaker {
            root: run_root,
            run_id,
            seq,
            release,
            notify,
        }) => pause_waker_cmd(
            &run_root,
            &run_id,
            match (seq, release) {
                _ if notify => crate::run::WakerFor::Notify,
                (_, Some(r)) => crate::run::WakerFor::Release(r),
                (s, None) => crate::run::WakerFor::Pause(s.unwrap_or_default()),
            },
        ),
        Some(Command::ScenarioRun {
            root: run_root,
            playbook,
            version,
            scenario: name,
            level,
            scenario_digest,
            playbook_digest,
            max_decision_requests,
        }) => match level.scenario() {
            Some(level) => scenario::child_cmd(scenario::ChildArgs {
                root: run_root,
                playbook,
                version,
                scenario: name,
                level,
                max_decision_requests,
                scenario_digest,
                playbook_digest,
            }),
            None => {
                eprintln!("__scenario-run: deep is not a scenario level");
                ExitCode::from(2)
            }
        },
        Some(Command::AskServer { run, node, attempt }) => ask_server_cmd(&run, &node, attempt),
        Some(Command::ReconcileDefaults) => selfupdate::reconcile_defaults_cmd(),
        None => match resolve_bind(None) {
            Ok(addr) => dashboard(addr, resolve_port(None), false),
            Err(e) => {
                eprintln!("dashboard failed: {e}");
                ExitCode::from(2)
            }
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn registers(args: &[&str]) -> bool {
        let cli = Cli::try_parse_from(args).expect("args parse");
        registers_workspace(cli.command.as_ref())
    }

    /// E3: an agent installing apb for a person reads llms.txt first; its
    /// first link is the setup procedure.
    #[test]
    fn llms_txt_links_the_agent_setup_procedure_first() {
        let llms = include_str!("../../../llms.txt");
        let first = llms
            .split("](")
            .nth(1)
            .and_then(|rest| rest.split(')').next())
            .expect("llms.txt has a link");
        assert!(first.ends_with("docs/AGENT-SETUP.md"), "{first}");
    }

    /// F18: llms.txt is what an agent reads to learn the CLI; its command
    /// list must name every public subcommand clap registers.
    #[test]
    fn llms_txt_lists_every_public_command() {
        use clap::CommandFactory;
        let llms = include_str!("../../../llms.txt");
        let section = llms
            .split("## CLI commands")
            .nth(1)
            .and_then(|rest| rest.split("\n## ").next())
            .expect("llms.txt has a `## CLI commands` section");
        let missing: Vec<String> = Cli::command()
            .get_subcommands()
            .filter(|c| !c.is_hide_set())
            .map(|c| c.get_name().to_string())
            .filter(|name| {
                !section
                    .lines()
                    .any(|l| l.starts_with(&format!("apb {name} ")) || l == format!("apb {name}"))
            })
            .collect();
        assert!(
            missing.is_empty(),
            "commands missing from llms.txt: {missing:?}"
        );
    }

    #[test]
    fn user_facing_commands_register_the_workspace() {
        assert!(registers(&["apb"]));
        assert!(registers(&["apb", "list"]));
        assert!(registers(&["apb", "mcp"]));
    }

    #[test]
    fn internal_reexec_targets_do_not_register_the_workspace() {
        assert!(!registers(&[
            "apb",
            "__drive-run",
            "--root",
            "/r",
            "--run-id",
            "x"
        ]));
        assert!(!registers(&[
            "apb",
            "__drive-supervised",
            "pb",
            "--handshake",
            "/h"
        ]));
        assert!(!registers(&[
            "apb",
            "__ask-server",
            "--run",
            "r",
            "--node",
            "n",
            "--attempt",
            "1"
        ]));
    }
}
