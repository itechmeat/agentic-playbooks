# Design: routine operations

Read `proposal.md` first. Every decision below names its files. Line
numbers are from origin/main 7e14c9e3 and were checked; find the code by
the names if they moved. Three decisions, built in the order D0, D2, D3,
D1 (tasks.md): the deadline (D2) reuses D0's graceful stop, and the start
key (D1) hashes the deadline argument into its request.

The second half of this work (the concurrency key, routine health, run
retention) is the separate change `routine-health`, implemented after
this one is merged; nothing here depends on it.

New lower-level seams, each built before its first user (tasks.md
section 1 unless noted):

1. `signed_store::transact` with `Unsigned::Refuse` (D0), for the start
   keys of D1.
2. `pause::interrupt::Graceful` (D0): how a kill is made, apart from
   whether the stop was a pause.
3. `StopRequest.due` (D0): an engine stop decided on the journal under
   the event lock.
4. `pause::parked` and the per-run deadline waker (D2, section 2), over
   the typed `WakeHold`.
5. `run_deadline::DeadlineLatch` (D2, section 2): the one admission check
   where work starts.
6. `start_key::StartRequest`, captured at the surface and immutable (D1,
   section 4), and the pending reservation with its `HeldLock` owner
   (D1, section 4), with `fsutil::try_hold_lock`.

## D0. Shared pieces

### Validator code

| Code | Severity | Decision | Rule |
|---|---|---|---|
| V200 | error | D2 | malformed `deadline` |

V201 to V209 stay reserved for this change; V210 to V219 belong to
`routine-health`; V220 to V222 are the cloud model rules
(`validate/cloud_models`) and V223 to V229 stay reserved for them. V187
is the untrusted-output warning of the Google connectors change
(`validate/connectors.rs`); V188 to V199 are unused, and this change does
not take them. Verify first: `git grep -ohE '"V2[0-9][0-9]"' crates |
sort -u` prints `"V220"`, `"V221"` and `"V222"` only, and `git grep -ohE
'"V1[89][0-9]"' crates | sort -u` stops at V187. The rule is its own module `crates/apb-core/src/validate/deadline.rs`,
declared in `validate/mod.rs` and called from `validate()` next to line
454 (`write_scope::check_write_scope`) as
`deadline::check_deadline(playbook, &mut r); // V200`.

### New state under `.apb/`

`crates/apb-core/src/workspace.rs` `LOCAL_ENTRIES` (line 27) gets
`"start-keys/"` with a one-line comment like the existing ones. The
existing tests at lines 239 and 257 cover it.

### Graceful abort

D2 stops runs the engine decided to stop (the change routine-health
reuses it). It uses one new entry in
`crates/apb-engine/src/stop.rs`:

```rust
/// What a stop says and how it treats the attempts in flight (its `due`
/// field is below, "The stop decided under the event lock").
pub struct StopRequest<'a> { pub by: PauseBy, pub reason: &'a str, pub graceful: bool, /* due */ }
/// [`stop_run_by`] with its own reason and kill: the one stop path.
pub fn stop_run_with(root: &Path, run_id: &str, req: &StopRequest<'_>) -> Result<StopOutcome, EngineError>
```

`stop_run_by` (line 132) becomes `stop_run_with(root, run_id,
&StopRequest { by, reason: STOP_REASON, graceful: false, due: None })`;
the body moves into `stop_run_with`, with `reason` in the posted
`Control::Abort` (line 154) and in the `RunAborted` it appends for a dead
run (line 194). `StopOutcome` gets `NotDue`.

`crates/apb-engine/src/control.rs` `Control::Abort` (line 22) gets:

```rust
/// Kill the attempts in flight with SIGTERM and the grace period of a
/// `--now` pause instead of at once. Absent (false) in older control
/// files and for `apb stop`. Additive.
#[serde(default, skip_serializing_if = "std::ops::Not::not")]
graceful: bool,
```

Fix every constructor the compiler lists with `graceful: false`
(`stop.rs` line 230 `abort_children`, `scheduler/deadline.rs` line 33,
`scheduler/entry.rs` line 760, `run_doctor.rs` line 809, tests).
`abort_children` passes the parent's `graceful` on.

#### How the kill is made is not why the attempt stopped

Today one mark carries two meanings. `pause::interrupt::mark_graceful`
(`crates/apb-engine/src/pause/interrupt.rs` line 34) makes a kill site send
SIGTERM, wait `grace()`, then SIGKILL, and the same mark is read as "this
stop was a pause": `StopContext::reason`
(`scheduler/stop_context.rs` line 53) returns `RunPaused` for a marked
flag when `cancel` and `kill` are the one node flag, which is how node
attempts build it (`scheduler/node.rs` line 1615), and the host transport
keeps a host task open for a marked flag (`scheduler/host.rs` line 554).
Firing the pause fanout for a graceful abort would therefore close the
attempt as `run_paused` and leave its host task open for a resume that
never comes.

Decision: the mark gets a kind, and the two questions read it apart.

```rust
// pause/interrupt.rs
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Graceful { Pause, Abort }
static GRACEFUL: Mutex<BTreeMap<usize, Graceful>>;   // was a BTreeSet
pub(crate) fn mark_graceful(flag: &AtomicBool, kind: Graceful);
/// How to kill: SIGTERM, grace, SIGKILL for either kind. Kill sites only.
pub(crate) fn is_graceful(flag: &AtomicBool) -> bool;
/// Why it stopped: only a `--now` pause. Stop causes only.
pub(crate) fn is_pause(flag: &AtomicBool) -> bool;
```

`CancelFanout.graceful` becomes `Option<Graceful>` (the pause fanout
marks `Pause`). `StopContext::reason` and `host.rs` line 554 call
`is_pause` instead of `is_graceful`; every kill site keeps `is_graceful`.
`StopWatcher::spawn` (`stop.rs` line 398 onward, the `Abort` branch at
line 431) builds a third fanout, `abort_fanout` (`graceful:
Some(Graceful::Abort)`), registered with the same flags as the pause
fanout, and when any pending `Abort` has `graceful: true` fires it before
the stop fanout:

```rust
if entries.iter().any(|e| matches!(e.cmd, Control::Abort { graceful: true, .. })) {
    abort_fanout.fire(); // marks each node kill flag Graceful::Abort
}
halt.store(true, Ordering::SeqCst);
fanout_thread.fire();
```

Do not fire the pause fanout and do not set `pause_now`: the attempt ends
as an abort (`StopReason::RunAborted`, close reason `run_aborted`), only
its kill is graceful.

Host tasks: a host task runs in a subagent the host session owns; the
engine has no process to signal. A graceful abort therefore promises
nothing about the subagent's process. What the engine does is what it
does for any abort: the open task is closed `cancelled` in the task store
(`host.rs` line 556, now reached because `is_pause` is false), the attempt
closes `run_aborted`, and a later `run_task_submit` for that task is
refused as not pending (`host_task.rs` line 620). The docs say so.

Wrong implementations to avoid:

1. Firing only the pause fanout: the run reads as paused, not aborted.
   Test (`stop_run_test.rs`): a graceful abort of a run whose agent
   stand-in traps SIGTERM and writes a marker ends `run_aborted` with the
   given reason, and the marker exists.
2. Setting `pause_now`, or firing the pause fanout first: the node ends
   `interrupted` as a pause and a resume reruns it. Test
   `a_graceful_abort_closes_the_attempt_as_aborted`: same run, the
   attempt's close reason is `run_aborted`, never `run_paused`, and no
   `attempt_finished { interrupted }` stands without `run_aborted`.
3. One mark for both meanings. Unit test in `stop_context.rs`: a node
   flag marked `Graceful::Abort` and latched reads `RunAborted`; marked
   `Graceful::Pause` reads `RunPaused`.
4. A host task left open by a graceful abort. Test
   (`stop_run_test.rs` `a_graceful_abort_cancels_the_host_task`): a
   host-mode run with an open host task; a graceful abort closes the task
   `cancelled`, the attempt closes `run_aborted`, and a `run_task_submit`
   for it afterwards is refused as not pending.
5. A graceful `apb stop`. Test: `stop_run` posts `graceful: false` (read
   `control.jsonl`).

### The stop decided under the event lock

`stop_run_with` reads the journal and appends `run_aborted` under the
run's event lock (`stop.rs` line 143), and a resume commits `run_resumed`
under the same lock (`scheduler/resume.rs` line 855). A stop the engine
decides from the journal (D2's deadline) must decide it under that lock
too, or a resume that commits a time grant between the decision and the
stop is aborted anyway. `StopRequest` therefore carries the condition:

```rust
pub struct StopRequest<'a> {
    pub by: PauseBy, pub reason: &'a str, pub graceful: bool,
    /// Checked on the journal read under the event lock, before anything is
    /// posted; `false` is `StopOutcome::NotDue` and nothing is written.
    /// `None` for a person's stop.
    pub due: Option<&'a dyn Fn(&[Event]) -> bool>,
}
```

The lock is taken with `lock_dir` and its error is returned when `due` is
set (a person's stop keeps today's best effort; an engine stop that
cannot hold the lock does not stop and tries again on its next poll).

### A signed-store transaction that refuses an unsigned file

`signed_store::update` (`crates/apb-core/src/signed_store.rs` line 186)
takes the file's lock itself and replaces an unsigned file with a fresh
one; its writer (line 208) is private. The start key (D1) needs a checked
read and a signed commit under one held lock, and must refuse, not
replace, a file that does not verify. One new entry, scheduled in section
1 before any user:

```rust
/// What a transaction does with a present file that does not verify.
pub enum Unsigned { Replace, Refuse }
/// `update` with the choice: under the file's lock, `change` gets the
/// signed file; `Unsigned::Refuse` returns `StoreError::Unverified(path)`
/// for a present file that does not verify or parse, writing nothing.
pub fn transact<T, R, E>(root: &Path, scope: Scope, playbook: &str, unsigned: Unsigned,
                         change: impl FnOnce(&mut T) -> Result<(R, bool), E>) -> Result<R, E>;
```

`update` becomes `transact(.., Unsigned::Replace, ..)` (no behaviour
change for known problems and corrections). `StoreError` gets
`Unverified(PathBuf)`. The lock stays the existing `DirLock`, held only
for the transaction: it is stolen after 60 s (`fsutil.rs` line 869), so
nothing may hold it across a run's preparation (D1 holds a separate
`HeldLock` for that, below).

Tests (`signed_store.rs`): `Refuse` on garbage and on a valid shape
without a MAC returns `Unverified` and leaves the bytes unchanged;
`Replace` keeps today's behaviour; `transact_serializes_writers` (two
threads each add 100 entries through `transact` while the other holds the
lock; 200 entries at the end, the MAC verifies).

## D1. Start key

### Request

`crates/apb-core/src/start_key.rs` (new):

```rust
/// The longest start key, in characters.
pub const KEY_MAX_CHARS: usize = 200;
/// The MAC domains of the key and of the request it stands for.
pub const KEY_DOMAIN: &str = "apb-start-key-v1";
pub const REQUEST_DOMAIN: &str = "apb-start-request-v1";
/// The default days a key is remembered (`runs.start_key_days`).
pub const DEFAULT_DAYS: u32 = 7;

/// A caller's start key, checked: 1 to 200 characters, no control or
/// hidden character, no leading or trailing whitespace, and not
/// credential-shaped (`token_shape::refuses_to_store`).
pub struct StartKey(String);
impl StartKey { pub fn parse(raw: &str) -> Result<StartKey, String>; }

/// The keyed MAC of the key (`run_origin::tag(KEY_DOMAIN, key)`).
pub fn key_tag(key: &StartKey) -> std::io::Result<String>;

/// Every explicit, execution-affecting argument the caller passed, as the
/// request a key binds, captured once by the launch surface from the
/// caller's own arguments, before any gate, candidate choice, prompt or
/// default changes them. Values recall and defaults fill in later are not
/// here: a retry that passes the same arguments is the same request even
/// when remembered input changed in between.
#[derive(Serialize)]
pub struct StartRequestFields {
    /// The canonical form's version; bump it when a field is added.
    pub v: u32, // 1
    pub version: Option<String>,          // the version asked for, never the one chosen
    pub params: BTreeMap<String, String>,
    pub instruction: Option<String>,
    pub worktree: Option<String>,
    pub overrides: Option<String>,        // sha256 of the overrides' canonical JSON
    pub execution: Option<String>,        // the requested mode (`cli`, `host`)
    pub supervision: String,              // `autonomous`, `supervised` or `agent_supervised` (RunMode)
    pub cache: String,                    // `auto`, `off` or `refresh` (CacheRunMode)
    pub max_parallel: Option<usize>,
    pub max_patches_per_run: Option<u32>,
    pub context_max_bytes: Option<usize>,
    pub context_compact_model: Option<String>,
    pub deadline_ms: Option<u64>,
    pub budget: Option<serde_json::Value>, // the budget flags as the caller typed them
    pub unattended: bool,
    pub allow_shared_workdir: bool,
    pub continued_from: Option<String>,
    /// The effective `external_data` flag (the value
    /// `external_data::for_start` returned at the surface).
    pub external_data: bool,
}

/// The request, frozen: built only by [`StartRequest::capture`], no
/// setter, no public field. Its tag is computed once at capture.
#[derive(Clone)]
pub struct StartRequest { tag: String }
impl StartRequest {
    /// The keyed MAC (`run_origin::tag(REQUEST_DOMAIN, ...)`) of the
    /// fields' canonical JSON (`serde_json::to_vec` of the struct: field
    /// order is fixed, `params` is a sorted map).
    pub fn capture(fields: &StartRequestFields) -> std::io::Result<StartRequest>;
    pub fn tag(&self) -> &str;
}

/// What a keyed start carries from its surface to the engine.
#[derive(Clone)]
pub struct StartKeyRequest { pub key: StartKey, pub reuse: bool, pub request: StartRequest }

/// The start-key file of one playbook, a `signed_store::SignedDoc`
/// (`MAC_DOMAIN = "apb-start-keys-v1"`, `SCHEMA = 1`,
/// `DIR = "start-keys"`, `ENTRIES_KEY = "entries"`).
#[derive(Serialize, Deserialize, Clone)]
pub struct StartKeysFile {
    pub schema: u32, pub scope: String, pub playbook: String,
    #[serde(default)] pub entries: Vec<StartKeyEntry>,
    #[serde(default, skip_serializing_if = "Option::is_none")] pub mac: Option<String>,
}
#[derive(Serialize, Deserialize, Clone)]
pub struct StartKeyEntry {
    pub key: String,      // key_tag
    pub request: String,  // StartRequest::tag
    /// `pending` while a preparation owns the entry, `bound` once its run
    /// is verifiable (below).
    pub state: EntryState,
    /// The reservation: a random id naming the owner's `HeldLock` file
    /// `.apb/start-keys/<scope>/<id>.pending/<reservation>.lock`. Set while
    /// `pending`, cleared at `bound`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reservation: Option<String>,
    /// Absent until preparation allocated the run directory.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_id: Option<String>,
    pub at_ms: u64,
    /// How many times `--start-key-reuse` moved this key.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub reused: u32,
}
#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum EntryState { Pending, Bound }
impl StartKeysFile {
    /// The live entry of `key`: within `days` of `at_ms`.
    pub fn find(&self, key: &str, now_ms: u64, days: u32) -> Option<&StartKeyEntry>;
    /// Drops every entry older than `days`.
    pub fn expire(&mut self, now_ms: u64, days: u32);
}
```

The key is never stored, logged or journaled: only its MAC under this
installation's key (`crates/apb-core/src/run_origin.rs` `tag`, line 126),
so a key that carries an order number or a mail id cannot be read back
from the project, and a copied project does not match on another machine.
The request is a MAC too: param values may be secret.

#### The request is captured once, at the surface

The engine cannot rebuild the caller's request from what reaches it: the
MCP start replaces an omitted version with the candidate the gate chose
(`crates/apb-mcp/src/server/run.rs` line 534, `permit.run_version`), a
resolved start always hands preparation a concrete version
(`scheduler/entry.rs` line 304, `run_resolved`), and the CLI's
`on_no_estimate: ask` question can replace the budget request
(`crates/apb-cli/src/budget.rs` line 234). A request rebuilt from
`RunOptions` would hash differently at the surface's early lookup and at
the engine's claim, and between a first start and its retry whenever a
candidate appeared in between.

Decision: each surface fills `StartRequestFields` from its own parsed
arguments right after it parsed them and computed the effective
`external_data` flag, and before its gate, candidate choice, budget
prompt or any default, and freezes it with `StartRequest::capture`. The
`StartRequest` travels unchanged: in `RunOptions.start_key` to
preparation, and in the `Supervise` handoff to `__drive-supervised`. The
early `lookup` and the engine's `claim` both read `request.tag()`; nothing
in the engine computes a request. One helper per surface family builds
the fields (`apb_cli::run::start_request`, `apb_mcp::server::run::start_request`,
`apb_server::routes::playbooks::start_request`), each a plain mapping of
that surface's argument struct.

Which arguments count, decided field by field: everything that changes
what the run executes or how it may act is in (`supervision` decides
failure handling, `run_config.rs` line 115; `cache` decides whether a
node reruns; `max_parallel`, `max_patches_per_run` and the compaction
settings change the work). Left out on purpose: `workdir_queue_wait`
(when the run starts, not what it does), `remember` (what the surface
keeps, not what runs), the notify arguments (who is told), and every
consent or confirmation argument (`acknowledge_untrusted`,
`confirm_irreversible`, `confirm_launch`: they answer a gate, they do not
change the request).

External data runs (`docs/EXTERNAL-DATA.md`) touch the start key in two
ways. First, the flag decides what a run may keep, so it is part of the
request: a keyed start with `--external-data` and a keyed start without
it are two requests, and the second is a `start_key_conflict`, never an
`already_started` that silently hands a caller a run with the other
contract. The field is the effective flag, because the surfaces already
compute it first (`external_data::for_start`, `crates/apb-cli/src/run.rs`
line 746, `crates/apb-mcp/src/server/run.rs` line 390,
`crates/apb-server/src/routes/playbooks.rs` line 316) and `RunOptions`
carries only that value; it is a pure function of what the caller passed
and of the runs it names (`continued_from`, a parent, the run step of
D3), so a retry computes the same value. Second, the start-key file is
not a learning store: it holds MACs, a run id and times, nothing a later
run, statistic or model reads to decide anything, so a flagged run binds
its key like any other run and no rule of `external_data::learns_from`
applies to it.

Cloud executors need no rule here: preparation refuses a start on an
unknown or unusable catalog model or a missing provider key
(`gate::cloud::check_tree`, `scheduler/prepare.rs` line 830) before the
claim is taken (line 1047), so such a refusal binds no key and leaves no
entry.

Why `refuses_to_store` and not `looks_like_credential`: the key is a text
a writer would keep (its MAC), and the wider rule is the one apb applies
to every stored text (`token_shape.rs` line 110). A refused key is
`start_key_invalid` with the reason, never the key.

The file lives at `.apb/start-keys/<scope>/<id>.json` and goes through
the signed store (`crates/apb-core/src/signed_store.rs`, the MAC under
this installation's key), the store known problems and corrections
already use, through the new `signed_store::transact` with
`Unsigned::Refuse` (D0) for every write and `read` (line 95) for every
read.

A file that is present but does not verify or does not parse
(`Loaded.signed == false`, line 53) would read as empty there, and
`update` would replace it, which would silently turn deduplication off.
For start keys that is refused: every
keyed start of that playbook answers `start_key_state_unreadable: <path>
does not verify (it was changed outside apb or written by another
installation); move it aside to start again with keys` until a person
moves the file; unkeyed starts are unaffected. `apb doctor` adds a WARN
line naming such a file (`crates/apb-cli/src/doctor.rs` `doctor_env`,
line 136).

#### Pending and bound entries

The run directory exists long before it can be verified: the origin
stamp, `run_started` and `run_provenance` are written near the end of
preparation (`scheduler/prepare.rs` lines 1365 to 1377), and a
preparation that fails early writes `run_started` and the stamp but no
`run_provenance` (`prep_try_unstarted`, line 160). An entry bound right
after `allocate_run` would name a directory a concurrent lookup must call
void, and that lookup would start a second run. A fixed age rule ("void
after 60 s") would also void a slow but live preparation. So an entry has
an owner while it is not verifiable, and ownership, not age, decides:

1. `claim` writes the entry `pending` with a fresh `reservation` and no
   `run_id`, inside one `transact`, and before it returns takes a
   `fsutil::HeldLock` (the `flock` lock the kernel drops when the process
   ends, `fsutil.rs` line 800) on the reservation file. The store's own
   `DirLock` is released when the transaction ends.
2. `Claim::attach(run_id)` right after `allocate_run` records the run id
   in the pending entry (one `transact`).
3. `Claim::bind()` right after `run_provenance` (and the start's run
   guard) is appended sets `state: bound`, clears `reservation`, then
   drops the `HeldLock` and removes its file, all inside one `transact`.
   Removing a `HeldLock` file is safe here, unlike in general (`fsutil.rs`
   line 794): every probe of a reservation runs under the store lock, and
   the entry stops naming the reservation in the same transaction, so no
   later probe opens that path.
4. `Claim` dropped without `bind` (preparation returned an error or
   panicked): inside one `transact`, an entry whose run directory holds a
   `run_started` (written by `prep_try_unstarted`) is bound to that
   failed run; any other pending entry of this reservation is removed, so
   a start refused before it allocated anything (`WorkdirBusy` without a
   queue, an invalid `continued_from`) leaves the key free.
5. A reader that finds a `pending` entry probes its owner while it holds
   the store lock (a new `fsutil::try_hold_lock`, one attempt, no wait):
   1. owner alive and `run_id` set: the run, status `running`
      ("preparing" on the surfaces);
   2. owner alive and no `run_id` yet (the few milliseconds between
      `claim` and `allocate_run`, which cover only the tree check): the
      engine's claim and the surfaces' lookup retry for up to 2 s (the
      `hold_lock` budget) and then answer `start_key_busy: another start
      with this key is being prepared; try again` (`EngineError::Conflict`,
      exit 2 on the CLI, 409 over HTTP);
   3. owner gone: the entry is judged as if bound when its run directory
      passes the run check below, else it is void.

The run check, before a bound (or orphaned pending) entry is acted on:
the directory `<root>/.apb/runs/<run_id>` is a real directory (not a
link), carries this installation's origin stamp (`run_origin::verify`,
line 101), its `run_started.playbook` is the file's playbook, and either
its `run_provenance.origin` is the file's scope, or it has no
`run_provenance` and its journal ends with `run_finished` (a preparation
that failed before provenance: the failed run holds the key). An entry
that fails any check is void (as if absent) and is dropped by the next
write. No rule uses the entry's age except retention.

### Retention

`policy.yaml` gains a `runs:` section with one key in this change,
`start_key_days` (1 to 365, default 7); `crates/apb-core/src/policy_file.rs`
`PolicyFile` (line 48) gets `pub runs: RunsSection`
(`#[serde(default, deny_unknown_fields)] pub struct RunsSection {
pub start_key_days: Option<u32> }`, validated in the file's `validate`;
`doctor_line` and `summary` name it). The later change routine-health adds
its retention keys to the same section. The machine file sets it; a
project file may only lengthen it: in force is
`max(machine, project)`, each defaulting to 7. An entry older than that
is ignored by readers and dropped by the next write (`expire`).

### Outcomes

`crates/apb-engine/src/start_key.rs` (new):

```rust
pub enum Lookup {
    /// No live entry: the start goes ahead.
    Free,
    /// The same request started this run; nothing starts.
    Started { run_id: String, status: RunStatus },
    /// The key names another request.
    Conflict { run_id: String },
}
/// Read-only answer for a launch surface, before any gate or prompt.
pub fn lookup(root: &Path, scope_label: &str, id: &str, req: &StartKeyRequest)
    -> Result<Lookup, EngineError>;

/// The pending reservation preparation owns from before the tree lock
/// until its run is verifiable ("Pending and bound entries" above).
pub struct Claim { owner: HeldLock, scope: Scope, id: String, reservation: String,
                   run_id: Option<String>, bound: bool }
pub fn claim(root: &Path, scope_label: &str, id: &str, req: &StartKeyRequest,
             step: &StepCaller) -> Result<Claim, EngineError>;
impl Claim {
    pub fn attach(&mut self, run_id: &str) -> Result<(), EngineError>;
    pub fn bind(self) -> Result<(), EngineError>;
}
impl Drop for Claim { /* rule 4 above, best effort */ }
```

The rules, for one live entry `e` of the key (expired entries do not
exist; a void entry (above) does not exist; an entry whose run directory
is gone does not exist either: a deleted run frees its key; a pending
entry is judged by its owner, above, never by its age):

| Entry | `reuse` | Answer |
|---|---|---|
| none | any | start; `bind` records `{key, request, run_id, at_ms: now}` |
| same request | false | `already_started` with `e.run_id`, whatever its status |
| other request | false | `start_key_conflict` naming `e.run_id` |
| any | true, `e`'s run failed or aborted | start; `bind` replaces `e`, `reused + 1` |
| any | true, `e`'s run running, paused or succeeded | `start_key_reuse_refused`: "run `X` is <status>; a key is reused only after a failed or aborted run" |
| any | true, from a run step | `start_key_reuse_refused`: `StepCaller::refusal("reusing a start key")` |

The run's status is `RunState::fold(read_all_lossy_tail(run_dir))`, with
`liveness::reported_run_status` when the driver is gone; terminal
`Succeeded` is success, `Failed` and `Aborted` are the two that allow
reuse.

Why a failed or aborted run holds its key: a start key says "this event
happened once". A routine that failed after sending half its mail must not
be repeated by a scheduler's automatic retry under the same key; the
retry reads the failure (`already_started`, and the CLI exits like `apb
wait`, so CI sees exit 1). A person who decides to run it again passes
`--start-key-reuse` (or the scheduler uses a new key for a new event).

`EngineError` (`crates/apb-engine/src/error.rs`) gets one variant:

```rust
/// A start key named a run that already started; nothing was started.
#[error("already started: {0}")]
AlreadyStarted(String),
```

A conflict and a refused reuse are `EngineError::Conflict` with a message
that starts with the code (`start_key_conflict: ...`,
`start_key_reuse_refused: ...`), and an invalid key is
`EngineError::Invalid("start_key_invalid: ...")`.

### Where it is checked

1. The engine, authoritative and race-safe: `RunOptions`
   (`crates/apb-engine/src/scheduler/entry.rs` line 11) gets
   `pub start_key: Option<apb_core::start_key::StartKeyRequest>` (doc: a
   top-level start only; ignored for a child, a scenario and an eval).
   In `prepare_placed` (`scheduler/prepare.rs` line 643):
   - the request is the `StartRequest` in `opts.start_key`, captured by
     the surface ("The request is captured once, at the surface");
     preparation never builds or changes one;
   - immediately before `let is_write = ...` (line 1047), when
     `opts.start_key` is set and `scenario.is_none() && opts.eval.is_none()
     && opts.parent_run.is_none()`: `let mut claim = start_key::claim(...)?`.
     `Started` returns `Err(EngineError::AlreadyStarted(run_id))`; nothing
     is written. The claim is taken before the tree lock, so a repeat
     while the first run holds the tree answers `already_started`, not
     `workdir busy`;
   - immediately after `allocate_run` (line 1070): `claim.attach(&run_id)?`;
   - immediately after the start's `run_provenance` and run guard are
     appended (line 1377 onward): `claim.bind()?`. Between `claim` and
     `bind` the entry is pending and its owner's `HeldLock` is held, so a
     concurrent start with the key answers this run (or
     `start_key_busy` before `attach`), however long preparation takes,
     and two concurrent starts with one key make one run. A preparation
     that returns an error drops the claim (rule 4: a run that has
     `run_started` holds the key as a failed run; otherwise the key is
     free); a process that dies leaves a pending entry whose owner is
     gone, judged by the run check;
   - `run_provenance` (line 1377) gets the first 12 hex characters of the
     key tag (below).
2. Every launch surface, read-only, before its gate, prompts, consent,
   budget check or `remember`, before the checks that read the
   environment or the settings (the unattended mode's check, the
   execution resolution), and after it captured the `StartRequest`:
   `start_key::lookup`. A repeat answers its run even when the
   environment changed since the first start. `Started` answers at
   once (below); `Conflict` refuses at once; `Free` goes on. The engine
   check stays the one that counts.

The surfaces:

1. CLI `apb run` (`crates/apb-cli/src/run.rs` `RunArgs`, line 458):

   ```rust
   /// Start this run at most once per KEY (a CI run id, an event id, a
   /// date): a repeat with the same key and the same params and
   /// instruction answers the run already started instead of starting
   /// another; another request under the key is refused. Kept 7 days
   /// (policy.yaml runs.start_key_days); never stored in clear
   #[arg(long, value_name = "KEY", conflicts_with = "estimate")]
   pub start_key: Option<String>,
   /// With --start-key: start a new run under a key whose run failed or
   /// was aborted
   #[arg(long, requires = "start_key")]
   pub start_key_reuse: bool,
   ```

   In `run_cmd` (line 698), right after the params parse (line 807, so
   after the `external_data::for_start` call at line 746) and before the
   launch context and the budget question: parse the key
   (`StartKey::parse`; an error prints `run refused (start_key_invalid):
   <reason>` and exits 2), capture the request from `RunArgs` as typed
   (`--version` as given or absent, the budget flags as typed, never the
   `Prestart` budget the `on_no_estimate: ask` question may produce), then
   `lookup`.
   - `Started`: print `run already started: <run_id> (start key)` on
     stdout. With `--detach` or `--supervise`: exit 0. Otherwise call
     `wait_cmd(root, &run_id, None, false)` (line 2526) and return its
     exit code: the caller gets exactly what `apb wait <run_id>` gives
     (0 succeeded, 1 failed or aborted, 3 needs input, 4 stopped). The
     `--estimate` flag conflicts with `--start-key`.
   - `Conflict`: `run refused (start_key_conflict): the key names run
     <id>, started with other params or instruction; use another key`,
     exit 2.
   - The engine's `AlreadyStarted` (a race lost after `lookup`) is handled
     the same as `Started` at each place `run`, `start_detached`, and
     `run_host_foreground` return it.
   - `--supervise`: the key crosses to the `__drive-supervised` child in
     the handoff, never on argv: `launch_handoff::Handoff::Supervise`
     (`crates/apb-engine/src/launch_handoff.rs` line 39) gets
     `#[serde(default, skip_serializing_if = "Option::is_none")] start_key:
     Option<HandoffStartKey { key: String, reuse: bool, request: String }>`
     (`request` the captured tag, carried verbatim and turned back into a
     `StartRequest` by a crate-private constructor of the receiving side;
     the file is private and single-use). `drive_supervised_child` reads it into
     `RunOptions.start_key`. When its preparation returns
     `AlreadyStarted(id)`, it writes `ALREADY: <id>` to the handshake;
     `spawn_detached_supervised` (around line 1249) prints `run already
     started: <id> (start key)` for that prefix and exits 0.
2. MCP `playbook_run` (`crates/apb-mcp/src/server/args.rs`
   `PlaybookRunArgs`, line 79): `start_key: Option<String>` and
   `start_key_reuse: Option<bool>`, documented in the tool description
   (one sentence: "start_key: pass the scheduler's or event's id so a
   repeat answers the run already started (already_started: true) instead
   of starting another"). In `playbook_run_for`
   (`crates/apb-mcp/src/server/run.rs`, after the scope parse and the
   `external_data::for_start` call at line 390, whose value is a request
   field, and before `check_run` and `permit.run_version` at line 534):
   capture the request from the tool arguments (`version` as passed, never
   the candidate), then the same `lookup`. `Started` answers `{"run_id": <id>, "already_started": true,
   "status": "<status>"}` for every mode (blocking, background,
   supervise); the agent follows it with `run_wait`. `Conflict` answers
   the policy refusal shape `{"policy": "start_key_conflict", "detail":
   ..., "run_id": <id>}` through `tools::actions::policy_refusal`. Where
   `playbook_run_for` turns the engine's start result into an answer,
   match `Err(EngineError::AlreadyStarted(id))` first and give the same
   success answer (a race lost after `lookup`); `From<EngineError> for
   ToolError` (`crates/apb-mcp/src/tools/mod.rs` line 106) maps a stray
   one to `ToolError::Conflict("already_started: <id>")`.
3. HTTP `POST /api/playbooks/{id}/run`
   (`crates/apb-server/src/routes/playbooks.rs` `run_playbook_handler`,
   line 300): `RunBody` gets `start_key: Option<String>` and
   `#[serde(default)] start_key_reuse: bool`; the request is captured
   from `RunBody` before the gate. `Started` is `200` with the
   MCP answer; `Conflict` is `409` with the refusal JSON; an invalid key
   is `400`.

Not on `apb eval`, `playbook_trial`, `playbook_execute_plan` and
children (proposal Out of scope 8): those paths never set
`RunOptions.start_key`.

### Journal and surfaces

`EventPayload::RunProvenance` (`crates/apb-engine/src/event.rs` line 152)
gets:

```rust
/// The first 12 hex characters of the start key's MAC when the run was
/// started with one (display and pruning only; never the key). Additive.
#[serde(default, skip_serializing_if = "Option::is_none")]
start_key: Option<String>,
```

`run_view::start_key(events) -> Option<String>`. `apb runs <id>` prints
`start key: <tag12>` next to the previous-success line
(`crates/apb-cli/src/run.rs` line 1910); MCP `run_status`
(`crates/apb-mcp/src/tools/run.rs` near line 288, where
`previous_success` is added) adds `start_key`; the HTTP run detail
(`crates/apb-server/src/routes/runs.rs` line 593) adds `start_key`; the
HTML report adds the line in `report_html/sections.rs` next to line 156.
The dashboard's `RunDriveNotes.svelte` shows `Started under a start key`.

Docs: `docs/CI.md`, a section "Starting once: --start-key" after "The two
commands" (line 12) with the GitHub Actions form
(`--start-key "${{ github.run_id }}"` stays the same across re-runs of a
job, so a re-run follows the first run; adding `${{ github.run_attempt }}`
makes every re-run a new start) and the timer form
(`--start-key "digest-$(date +%F)"`); `docs/MCP.md` the two arguments.

Wrong implementations to avoid:

1. Reading a tampered or corrupt file as empty. Test: write garbage, then
   a validly shaped file without a MAC, at the playbook's start-key path;
   a keyed start is refused `start_key_state_unreadable` both times and
   an unkeyed start runs.
2. Storing the key or a plain SHA-256 of it. Test: after a keyed start
   the bytes under `.apb/start-keys/` do not contain the key and do not
   contain `sha256(key)` hex.
3. The request digest over params after recall and defaults: a
   remembered value changing between two runs would make a retry a
   conflict. Test: a playbook with a param default; start with
   `--start-key k` and no params, change the remembered input, start
   again with `k`: `already_started`.
4. Checking after the tree lock. Test (`start_key_test.rs`): a write
   playbook whose first run is still running; a repeat with the key
   answers `already_started`, not `workdir busy`.
5. Two concurrent starts both starting. Test: 8 threads call
   `run_background` with the same key and request; exactly one run
   directory exists for the playbook, the other 7 return
   `AlreadyStarted` with its id (or `start_key_busy`, then
   `AlreadyStarted` on a retry).
6. A failed run freeing its key. Test: a run that fails; a repeat answers
   `already_started` with status failed; `--start-key-reuse` starts a new
   run; reuse of a succeeded run is refused.
7. A credential-shaped key accepted. Test: `--start-key ghp_<36 chars>`
   is refused `start_key_invalid` and nothing is written.
8. The CLI repeat exiting 0 for a failed run. Test (`run_cli_test.rs`):
   a foreground repeat of a failed keyed run exits 1 and prints `run
   already started:`.
9. The key on argv of `__drive-supervised`. Test: `spawn_detached_supervised`
   argv (captured through the command builder) holds no `--start-key`.
10. Ignoring explicit arguments. Unit test (`start_key.rs`
   `every_field_changes_the_request`): from one base `StartRequestFields`,
   a copy that differs in exactly one field gives another tag, for every
   field; the test destructures the struct with no `..`, so a field added
   without a row does not compile. E2e: keyed starts that differ only in
   `--version`, only in `--worktree`, only in `--deadline`, only in
   `--no-cache`, only in `--supervise`, or only in `--external-data` are
   each a `start_key_conflict`.
11. Acting on a forged entry. Test: an entry naming a run of another
    playbook (or a directory without an origin stamp) is void: the keyed
    start runs.
12. An age rule instead of an owner. Tests (`start_key_test.rs`):
    `a_slow_preparation_keeps_its_key` (a `testing` hook holds preparation
    after `attach`; the test sets the pending entry's `at_ms` two minutes
    back through `transact`; a second keyed start answers
    `already_started` with the first run's id, and one run directory
    exists); `a_dead_owner_without_run_started_frees_its_key` (a pending
    entry whose reservation lock nobody holds and whose run directory has
    an empty journal is void: the keyed start runs).
13. Publishing the key before its run verifies. Test
    `a_lookup_during_preparation_answers_the_preparing_run`: a `testing`
    hook stops preparation after `attach` and before the origin stamp; a
    concurrent `lookup` and a concurrent keyed start both answer the
    first run, and after the hook is released exactly one run exists. A
    wrong implementation that binds at `allocate_run` and checks the stamp
    voids the entry and starts a second run.
14. A failure before provenance freeing or orphaning the key. Test
    `a_failure_before_provenance_holds_the_key`: a keyed start whose
    preparation fails at `check_launch` (after `allocate_run`, before
    `run_provenance`); a repeat answers `already_started` with status
    failed and the first run's id. A start refused before `allocate_run`
    (the tree busy, no queue) leaves no entry: the repeat after the tree
    is free starts.
15. Rebuilding the request in the engine. Tests through the real surfaces
    (`run_tools_test.rs`, `run_cli_test.rs`):
    `a_candidate_does_not_change_the_request` (a keyed MCP start without a
    version; a candidate version is added; the repeat without a version
    answers `already_started`, not `start_key_conflict`);
    `a_global_start_repeats` (a keyed start of a global playbook without
    `--version`, which reaches preparation through `run_resolved` with a
    concrete version; the repeat answers `already_started`);
    `a_budget_answer_does_not_change_the_request` (a playbook with
    `on_no_estimate: ask`, a keyed CLI start whose question is answered;
    the repeat with the same flags answers `already_started`).
16. Replacing an unreadable file. Test: after
    `a_tampered_store_refuses_keyed_starts`, the file's bytes are
    unchanged (the start-key writer uses `Unsigned::Refuse`).

## D2. Run deadline

### Schema and arguments

`Playbook` (`crates/apb-core/src/schema.rs` line 139) gets, after
`follow_up`:

```rust
/// The run's deadline: it ends aborted (`deadline_exceeded`) once it has
/// run this long (a duration, `45m`), the time a person paused it left
/// out (see `apb_engine::run_deadline`). A run argument overrides it.
/// Additive to schema 2; no migration.
#[serde(default, skip_serializing_if = "Option::is_none")]
pub deadline: Option<String>,
```

V200 (error): not a duration (`duration::parse_duration_str`), zero, under
1 minute, or over 30 days.

Run arguments, all "time from now":

1. `apb run --deadline <duration>`; MCP `playbook_run.deadline`; HTTP
   start `deadline`. Parsed by the surface (`parse_duration_str`; invalid
   is refused `deadline_invalid` before anything else). `RunOptions` gets
   `pub deadline_ms: Option<u64>`. `--supervise` forwards it in the
   `Supervise` handoff (`launch_handoff.rs` line 39) as a field
   `#[serde(default, skip_serializing_if = "Option::is_none")]
   deadline_ms: Option<u64>`.
2. `apb resume <id> --deadline <duration>`; MCP `run_resume.deadline`
   (`server/args.rs` `RunResumeArgs`, line 508); HTTP resume body
   `deadline` (`routes/runs.rs` `post_resume_handler`, line 1079).
   `ResumeRequest` (`scheduler/resume.rs` line 433) gets `pub deadline_ms:
   Option<u64>`. A detached resume carries it to its driver:
   `driver::DriverArgs` (`crates/apb-engine/src/driver.rs` line 256) gets
   `pub deadline_ms: Option<u64>`, forwarded as `--deadline-ms <n>` to the
   hidden `__drive-run` (`crates/apb-cli/src/main.rs` line 857), whose
   `DriveRunArgs` (`crates/apb-cli/src/run.rs` line 1392) reads it back
   into the `ResumeRequest` the child runs, exactly as `budget_json` is
   carried. Not on `apb resume --all`.

Which runs take one: a top-level production run. A scenario run ignores
`deadline` (its timing is scripted; no watch, no field), an eval run keeps
its own deadline (`EvalRunSettings.deadline_ms`, `run_config.rs` line 283)
and ignores the routine one, and a child takes none (its parent's abort
reaches it through `abort_children`).

A run with `external_data: true` takes a deadline like any other: the
flag governs what a run keeps, the deadline how long it runs, and neither
reads the other. A cloud catalog executor (an agent on a catalog route,
such as dsh on `nearai/<id>`) is an agent process like any other: the
graceful kill reaches it through its process group, and the usage of the
attempt it cut is priced as for any interrupted attempt. No rule is added
for either.

### The clock

`crates/apb-engine/src/pause.rs` gets:

```rust
/// The deadline clock of a run: wall time since `run_started`, minus the
/// spans a person paused it (`run_paused.by == operator`) and the spans
/// it sat ended before a resume ([`ended_intervals`]). While the run is
/// ended (the fold is terminal), `now` is clamped to its latest terminal
/// event: an ended run does not age. Engine and supervisor pauses, waits
/// on gates and questions, and the workdir queue all count.
pub fn deadline_elapsed_ms(events: &[Event], now_ms: u128) -> u128;
```

A pause's owner can change without a resume: a later `run_paused`
replaces the pause in force (the quota probe's re-pause,
`pause/probe.rs` line 229, an engine budget pause over a person's pause,
a person's `apb pause` of a run the engine paused), `paused_intervals`
keeps the first start of such a chain (`open.get_or_insert`, line 233),
and the owner the surfaces show is the latest pause's (`pause_info`,
line 149). Decision: the owner of every instant is the `by` of the latest
`run_paused` in force at that instant. One private fold yields owned
segments `(start, end, by)`: a `run_paused` while a segment is open closes
it at that event's `ts` and opens the next with the new `by`; a closing
event closes it. `paused_intervals` merges adjacent segments, so its
output is unchanged; the deadline clock subtracts only segments whose
`by` is `Some(PauseBy::Operator)`. Do not copy the loop; both public
functions call the fold. Implement it with that fold, plus
`ended_intervals` (line 251), merged so an overlap is not subtracted
twice. `ended_intervals` leaves out the final open ended span (an end no
resume follows, line 247); the clamp covers exactly that span, so a resume
preflight a day after a failure reads the time the run had when it ended.

Why: a deadline bounds how long the world waits for a routine. A quota
pause, a review nobody answers and a queue are exactly what it must bound
(a digest that waited out a quota until noon is stale). A person's pause
and a person's resume of an ended run are deliberate; counting them would
abort a run the moment a person resumes it.

### What is in force

`crates/apb-engine/src/run_deadline.rs` (new):

```rust
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunDeadline {
    /// The total on the deadline clock, milliseconds.
    pub total_ms: u64,
    pub source: DeadlineSource, // playbook | run | resume (snake_case)
}
/// The latest of `run_provenance.deadline` and every `run_resumed.deadline`.
pub fn in_force(events: &[Event]) -> Option<RunDeadline>;
/// `total - elapsed`, saturating at 0; `None` without a deadline.
pub fn remaining_ms(events: &[Event], now_ms: u128) -> Option<u64>;
/// Whether a deadline is in force and nothing is left.
pub fn expired(events: &[Event], now_ms: u128) -> bool;
/// A drive's expiry as a wall-clock instant (its clock cannot stop while
/// it drives); the one admission check ("Enforcement in every state").
pub struct DeadlineLatch { pub expires_at_ms: u128 }
impl DeadlineLatch { pub fn expired(&self) -> bool; pub fn left_secs(&self) -> u64; }
pub const DEADLINE_EXCEEDED: &str = "deadline_exceeded";
pub fn reason(d: &RunDeadline) -> String; // "deadline_exceeded: the run passed its deadline of 45m"
/// Which state the caller saw the run in; rechecked under the event lock.
pub enum Expect { Driving, Parked(WakeHold), Any }
/// Ends the run through the one stop path when its deadline passed:
/// `stop::stop_run_with(root, run_id, &StopRequest { by: Engine, reason,
/// graceful: true, due: Some(&|events| ...) })`, where `due` is true only
/// when, on the journal read under the event lock, the fold is not
/// terminal, `expired(events, now)` holds (so a grant a resume committed a
/// moment ago counts) and `expect` still holds (`Parked(h)`:
/// `h.holds(events)`, the same `WakeHold` the waker was armed for).
/// `Ok(true)` when it stopped the run, `Ok(false)` on `NotDue`.
pub fn stop_if_expired(root: &Path, run_id: &str, expect: Expect) -> Result<bool, EngineError>;
/// The supervisor-patch rule: a patch may not add, drop or change
/// `deadline` (the run's contract; only a person changes it, at a resume).
pub fn patch_refusal(old: &Playbook, new: &Playbook) -> Option<String>;
```

At start (`prepare_placed`, a top-level production run): `opts.deadline_ms`
wins, else the playbook's `deadline`; journaled on `run_provenance`
(`event.rs` line 152):

```rust
/// The run's deadline at start. Additive.
#[serde(default, skip_serializing_if = "Option::is_none", deserialize_with = "lenient_option")]
deadline: Option<crate::run_deadline::RunDeadline>,
```

At a resume with `deadline_ms = Some(d)`: `try_journal_resumed`
(`scheduler/resume.rs` line 849), where it already sets `launch` on the
payload under the event lock, sets the new `RunResumed.deadline =
Some(RunDeadline { total_ms: deadline_elapsed_ms(events, now) + d, source:
Resume })` (same serde attributes), and, when the latest `run_guard`
(`latest_guard`, as `patch.rs` reads it) does not name `deadline`,
appends a `RunGuard` with it added before `run_resumed` in the same lock.
A resume without it journals none, and the start's deadline stays in
force.

`EventPayload::resumed` (`event.rs` line 3574) and every `RunResumed`
literal get `deadline: None`.

### Guard and patches

`crates/apb-core/src/validate/guarded.rs`: `GuardedFeature::Deadline`
(`"deadline"`), after `UntrustedInput` in the enum and in `ALL` (line 47,
now 8 entries); `guarded_features` sets it when `playbook.deadline` is
set. Module doc: an apb that does not know `deadline` would continue the
run without its limit.

A run's guard must follow its contract, not only the definition in force:

1. `run_guard` (`prepare.rs` line 1682) becomes
   `run_guard(playbook: &Playbook, events: &[Event])`: the definition's
   features plus `Deadline` when `run_deadline::in_force(events)` is set.
   The start calls it after `run_provenance` is appended (so a deadline
   from the run argument alone is named); `scheduler/patch.rs` line 157
   passes the run's journal, so a patch never drops a deadline guard.
2. Supervisor patches: `scheduler/patch.rs` calls
   `crate::run_deadline::patch_refusal(playbook, &loaded.playbook)` next
   to `previous_success::patch_refusal` (line 117); the forward patch's
   `protected_fields` (`crates/apb-engine/src/forward_patch.rs` line 254)
   gets a ninth entry `("deadline_changed", "the run's `deadline` is not
   a supervisor's to change", |a, b| a.deadline == b.deadline)`.

### Enforcement in every state

A run with a deadline in force is in one of four states; each one ends it
at expiry through `stop::stop_run_with` (D0), which posts the graceful
abort and reaches children through `abort_children`:

1. Driving. A drive's deadline clock cannot stop while it drives (a
   person's pause ends the drive), so the drive computes its expiry once,
   as a wall-clock instant: `run_deadline::DeadlineLatch { expires_at_ms }`
   (`expires_at_ms = now + remaining_ms(events, now)`), built in
   `scheduler.rs` next to the eval watch (line 718) and shared (`Arc`)
   with every place below through the run config the drive hands down.
   `DeadlineLatch::expired()` is `clock::now_ms() >= expires_at_ms`: no
   journal read, no watcher needed. A second `DeadlineWatch` starts at
   `expires_at_ms`. `DeadlineWatch::start` (`scheduler/deadline.rs` line
   23) takes what to do at expiry: the eval watch keeps posting
   `Control::Abort { reason: DEADLINE_REASON, by: Engine, graceful: false }`
   (its test pins the reason, `eval_cli_test.rs` line 934); the run watch
   calls `run_deadline::stop_if_expired(root, run_id, Expect::Driving)`.
   A drive that starts past the deadline stops at once.
2. Admitting work. The watch polls at most every 500 ms and a batch may
   have formed before expiry and dispatch queued members long after it
   (`scheduler.rs` line 1547), so the check sits where work actually
   starts, not where it is planned: every execution of a node passes
   `node::execute_node_kind` (`scheduler/node.rs` line 830; sequential,
   batch members dequeued at line 1547, pipeline members, list items,
   deferred retries and resumes all reach it), and every attempt passes
   the attempt loop before its spawn. At both, `latch.expired()` means:
   call `stop_if_expired(.., Expect::Driving)` once (idempotent), and the
   execution is written off `cancelled` exactly like a member the run
   abort reached before it started (`write_off_cancelled`), with no
   `node_started`, no `attempt_started` and no list item started. The
   drive loop also checks the latch at its top, before the baseline's
   before snapshot (`scheduler.rs` line 1120) and before picking the next
   node, so no baseline read starts after expiry either.
3. Queued for the tree. `Prepared::claim_queued_workdir`
   (`scheduler/entry.rs` line 208): the `stopped` closure also returns
   true once `expired`, and the give-up is journaled by
   `close_out_unclaimed` as an abort with the deadline reason (call
   `stop_if_expired(.., Expect::Any)` first, so a stop is pending).
   `drive_run_from_dir` caps the handover wait (line 584) at the remaining
   time, the handover's own floor included, and, when the wait ran out
   because of it, stops the run the same way. A detached driver
   (`apb __drive-run`) that gives up there never drove the run, so it
   first removes the `driver.pid` its spawner published for it
   (`driver::let_go_unclaimed`): the stop then finds no live driver and
   journals the abort itself instead of posting one nobody consumes.
4. Parked (paused, or released, with no driver). One deadline waker per
   run, not per park: `WakerFor::Deadline` (`crates/apb-engine/src/waker.rs`
   line 24; lease `deadline`, args `--deadline`, `what` "a run's deadline
   waits for its time", `instead` "`apb resume --all` ends it"). Its body
   loops on the pause waker's poll (`pause::waker::POLL_ENV`): it reads
   the journal; the run's park is `pause::parked(events) ->
   Option<WakeHold>` (new, the typed `WakeHold` of the latest `run_paused`
   or `drive_released` that still holds the run, through
   `WakeHold::holds`, `pause.rs` line 355, never the paused-only
   `pause::holds`); it ends when the run is terminal, when a live driver
   holds it (that drive has its own watch), or when the park is a
   person's pause (the clock is frozen); otherwise, once `expired`, it
   calls `stop_if_expired(.., Expect::Parked(hold))` and ends. A park
   replaced while it waits (the quota probe's re-pause, a supervisor's
   pause over the engine's) is just the next value of `parked`: the
   waker keeps watching, so no replacement site needs to re-arm it. It is
   armed (`waker::spawn`, a no-op when the lease is held) wherever a run
   with a deadline in force parks: `drive`'s result match (`scheduler.rs`
   line 476: the released arm and the `Paused` arm), `pause/request.rs`
   where a driverless run is paused (lines 104 and 205), and the resume
   path's refusal of a person's resume that leaves the run parked. CLI:
   `PauseWaker` (`main.rs` line 916) gets `--deadline` the way it got
   `--follow-up`.
5. After a reboot. No waker survives a reboot, and `apb resume --all`
   leaves an indefinite pause untouched (`pause/resume_all.rs` line 268,
   `LeftPaused`). So `resume_all`, for every run it lists, before its
   candidate filtering: a run with a deadline in force, not terminal, with
   no live driver, is stopped through `stop_if_expired(.., Expect::Any)`
   when expired (a new entry `ResumeAllAction::DeadlineStopped`), and
   otherwise gets its deadline waker re-armed (unless the park is a
   person's pause); then the existing filtering runs for the runs still
   parked.

Children. `abort_children` (`stop.rs` line 216) reaches the children the
parent journaled with `child_run_started`, but a child is prepared before
that event is appended and driven right after it (`scheduler/node.rs`
line 5058), in the parent's process with its own stop watcher; a stop
during the child's preparation would miss it, and the child would run to
its end. Decision: publish first, then check. Right after
`child_run_started` is appended and before `drive` of the child, the
playbook node reads the parent's stop state (its run-level `cancel` flag,
which the parent's stop fanout latches, or its `DeadlineLatch`); when
stopped, it posts `Control::Abort { reason: PARENT_ABORTED_REASON, by:
Engine, graceful: true }` to the child before driving it, so the child's
first control scan ends it `run_aborted` before any node starts. A stop
that lands after the append finds the child through `abort_children`; one
that landed before it is caught by the check. Children take no deadline of
their own. A parked child (an engine quota pause handed back to its
parent, a released drive) has no driver to apply a posted abort, and
`apb resume --all` never wakes a child, so `abort_children` ends it
through the one stop path, decided under the child's event lock while it
is still parked (`pause::clock::parked`), its own children with it; it
posts the abort only when a resume committed first. A parked run counts
as driven only by a drive that claims it (`liveness::drive_claimed`: a
live `driver.pid`, or the `driven_by` marker of a parent's in-process
drive), never by the `parent_run` of its run config alone, so a parked
child of a parent that drives on into a wait is ended, not signalled.

Resumes, one check for all of them, in `resume_inner_checked`
(`scheduler/resume.rs` line 997), which both `resume_req` (line 702) and
`resume_held` (line 729) reach, as a synchronous preflight in
`resume_detached_inner` (line 492) before any spawn, and again, final,
under the event lock in `try_journal_resumed` (line 849) next to the
lifecycle and `WakeHold` rechecks, on the journal read there:

1. A resume with `deadline_ms` set (a person granted time) goes on.
2. Else, when `expired`: `by == Operator` is refused
   `EngineError::Conflict("deadline_passed: run `<id>` passed its deadline
   of 45m; resume it with --deadline <duration> to give it more time")`
   with nothing journaled; `by == Engine` or `Supervisor` returns
   `EngineError::Conflict("deadline_exceeded: ...")` and, once the event
   lock is released, calls `stop_if_expired(.., Expect::Any)`.
   The final check under the lock is the one that counts: a run that
   expired between the preflight and the commit is refused there, before
   `run_resumed` is appended, and a stop that is decided under the same
   lock (D0, `StopRequest.due`) sees a grant committed before it.
3. Before a quota probe: the pause waker (`pause/waker.rs` line 157) and
   `apb resume --all` (`pause/resume_all.rs` line 333) call
   `stop_if_expired(.., Expect::Parked(hold))` first and skip the probe
   when it stopped the run, so no paid probe runs for a run that is over.
   The probe itself (`scheduler/quota_probe.rs`) reads the deadline again
   right before it spawns, after its preflight, sends nothing when no
   time is left and bounds its executor by the time left, so a probe
   admitted just before expiry does not outlive the run. While it runs,
   a watch re-reads the journal and cancels the executor through the
   attempt's own cancellation flag once the pause it probes for no longer
   holds the run (a resume, the only way the deadline changes, a stop),
   and the probe journals nothing.

At expiry: no node starts, the attempts in flight get SIGTERM, the grace
period, then SIGKILL, and the run ends `run_aborted { reason:
"deadline_exceeded: ...", by: engine }`. A send in flight stays in the
send ledger as `unknown`.

### Engine reads on the drive thread

The graceful abort reaches agents and scripts as processes: a connector
call an agent or a script makes (`apb connector call`) runs inside the
attempt's process group and ends with it, an OAuth token minted there
lives only in that process, and a send it had recorded stays `unknown`.
The engine also reads on its own drive thread: the baseline check reads
its sources before the first acting node and again at the finish
(`scheduler/baseline.rs`). Its scripts and project checks stop on the
snapshot's kill flag, which the abort latches (`baseline/read.rs` line
292, `baseline/project.rs` lines 199 and 255). A connector read does not:
it is one blocking call (`baseline/read.rs` line 179,
`execute_with_within`) bounded only by its one deadline
(`connector/call/mod.rs` line 682), the function's `timeout_sec` lowered
to the check's `timeout_seconds`, which may be up to 1800 s
(`apb_core::baseline::MAX_TIMEOUT_SECONDS`). Since the Google connectors
change that one deadline covers the access token's minting, the service
request, the refresh and retry after a 401, and every page of a paged
walk, so neither the token refresh nor paging needs a bound of its own:
bounding the call bounds them.

So a run with a deadline lowers that call's bound to the time it has
left. `ReadCtx` (`crates/apb-engine/src/baseline/ctx.rs` line 14) gets

```rust
/// At most this many seconds for one connector read (the run's deadline
/// left, rounded up, at least 1); `None` without a deadline.
pub cap_secs: Option<u64>,
```

`call_connector_json` passes `read.timeout_seconds.min(cap)` to
`execute_with_within` (line 192); `BaselineRun::ctx`
(`scheduler/baseline.rs` line 54) sets it from the drive's
`DeadlineLatch` (the time left, rounded up). The follow-up reader
(`follow_up/read.rs` line 149) sets `None`: it reads after the run ended,
outside its deadline. The notify sweep after a run's end is not bounded
either: telling a person the run hit its deadline is part of ending it.

Credential helpers. Before the request, the call resolves the account's
secrets (`connector/call/mod.rs` line 746, `resolve_secrets`), and each
`{{cmd:...}}` helper runs with its own fixed bound,
`CMD_SECRET_TIMEOUT` = 10 s (`apb_core::connector::secrets` line 177,
`auth.rs` line 51), one after another. The call's one deadline (line
682) is computed before that step, so it is passed on:
`resolve_secrets(root, account, deadline: Instant)` gives each helper
`CMD_SECRET_TIMEOUT.min(deadline - now)`, and a helper with no time left
is not started and fails as a timeout (`CallErrorCode::Timeout`, the same
code a slow service gets). Nothing else changes for a call without a
lowered bound: with the function's own `timeout_sec` the helper keeps at
most its 10 s.

A read cut short by the deadline. Today a snapshot the cancel flag
interrupts journals nothing (`scheduler/baseline.rs` lines 111 and 191),
so the next drive takes it; that is honest because no acting node ran
before a missing before snapshot, and an after snapshot is taken at the
finish. A deadline abort latches that flag, and a read the cap cut short
ends at the same instant. Decision: keep that rule and extend it to the
cap. After each `read`, `ensure_before` and `after_and_compare` treat
`cancel` set or `latch.expired()` alike: interrupted, nothing journaled
for that check. The run then ends aborted with the check unobserved: the
outcome view (`run_outcome/baseline.rs`) shows no `before_status` (or no
after) and a verdict that is never `met`, and every surface reads that as
"not observed". A person who resumes the run with `--deadline` gets a
fresh read, because nothing stale was journaled. Journaling `unknown`
instead would be wrong here: a before snapshot once journaled is never
read again (line 98), so a resumed run could never get a real one.

### Surfaces

```rust
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct RunDeadlineView {
    pub total_ms: u64, pub elapsed_ms: u64, pub remaining_ms: u64,
    pub source: DeadlineSource,
    /// A person paused the run, or it ended: the clock does not run.
    pub frozen: bool,
    /// Wall-clock end while it runs; absent while frozen.
    #[serde(skip_serializing_if = "Option::is_none")] #[cfg_attr(feature = "ts", ts(optional))]
    pub ends_at: Option<String>,
}
pub fn run_view::deadline(events: &[Event], now_ms: u128) -> Option<RunDeadlineView>;
```

1. `apb runs <id>` (`crates/apb-cli/src/run.rs` next to line 1910):
   `deadline: 45m, 12m left (ends <UTC>)`, `deadline: 45m, 12m left,
   frozen (paused by a person)`, or `deadline: 45m, exceeded`.
2. MCP `run_status` (`crates/apb-mcp/src/tools/run.rs` near line 288)
   `deadline`; HTTP run detail (`crates/apb-server/src/routes/runs.rs` line
   593) `deadline`; HTML report line (`report_html/model.rs` line 102,
   `report_html/sections.rs` line 156).
3. Dashboard: the detail type the page reads is generated
   (`web/src/lib/api.gen.ts`) or handwritten (`web/src/lib/types.ts`);
   add `deadline?: RunDeadlineView` to whichever holds the run detail
   (search `previous_success` there). `web/src/pages/RunView.svelte` line
   634 passes `deadline={detail.deadline}` to `RunDriveNotes`, which shows
   the line from a pure `web/src/lib/rundeadline.ts`.
4. Next actions: `AutoOutcome` (`next_action/types.rs` line 176) gets
   `RunDeadline`; `RunActions` (line 312) gets
   `#[serde(skip_serializing_if = "Option::is_none")] run_deadline:
   Option<AutoProgress>` (TS optional), set by a new
   `deadlines::run_deadline(clock, events)` (`next_action/deadlines.rs`):
   `AutoProgress::At { at_ms: now + remaining, outcome: RunDeadline,
   assured: driver_alive }`, or `Frozen` while frozen. The CLI's
   next-action printer (`crates/apb-cli/src/actions.rs`) prints `the run
   ends at its deadline at <UTC>`; `run_wait` carries it unchanged.
5. `web/src/lib/journal.ts`: a `run_aborted` whose reason starts with
   `deadline_exceeded` reads `deadline exceeded`.

Docs: `docs/HOWTO-authoring.md` `## deadline (a run's wall-clock limit)`
after `## budget` (line 4140): the field, the run argument, the clock and
what it leaves out, the four states, the resume rules, scenarios and
evals, the V200 row in the format of the V177 table (line 249), the
older-apb note; `docs/CI.md` "Unattended runs" one paragraph (a deadline
turns a stuck CI job into an exit code); `docs/MCP.md` the arguments.

Wrong implementations to avoid:

1. The clock leaving out every pause. Test (`run_deadline_test.rs`): a
   journal with an engine pause of 10 min and an operator pause of 10 min
   inside 30 min: `deadline_elapsed_ms` is 20 min.
2. An ended run aging. Test: `run_finished failed` at 10 min and no
   `run_resumed`; `deadline_elapsed_ms` a day later is 10 min (the resume
   preflight reads this, not a journal that already has `run_resumed`).
3. The resume's grant as a total. Test: deadline 30 min, elapsed 40 min,
   resume with `--deadline 10m`: `total_ms` 50 min, remaining 10 min.
4. An abrupt kill, or a parent left waiting on its child. Test: a parent
   whose `playbook` node runs a child whose agent stand-in traps SIGTERM
   and writes a marker; the parent's deadline passes; the marker exists,
   the child ends aborted and the parent `run_aborted` with
   `deadline_exceeded:`.
5. A plain resume driving a run past its deadline. Test: refused
   `deadline_passed`, nothing journaled, no driver spawned
   (`resume_detached` returns the refusal synchronously).
6. A parked run never ending. Test: a run paused by the engine without
   `until` (a budget pause) with a deadline of a few seconds: its deadline
   waker ends it `run_aborted` with `deadline_exceeded` and no resume
   happens.
7. A run queued for the tree outliving its deadline. Test: a start queued
   behind a tree held by a script that waits for a marker; its deadline
   passes; it ends aborted `deadline_exceeded` without taking the lock.
8. A work item admitted after expiry. Test
   `no_work_starts_after_expiry`, with the run watch held back by the
   `testing` variable `APB_TEST_DEADLINE_WATCH_DELAY_MS=5000` so only the
   admission checks can stop the work: one run with a concurrent batch of
   three members and `max_parallel: 1` (two queued at expiry), and one
   with a list node of many items and `batch.parallel: 1`; the deadline
   passes mid-work. Asserted against the deadline clock, not the abort:
   no `node_started`, `attempt_started` or list item start has a `ts` at
   or after `run_started.ts + total_ms`, and the run ends `run_aborted`
   `deadline_exceeded`. Checking only where a batch forms, or only before
   the abort, fails it.
9. The grant lost on a detached resume. Test: `resume_detached_req` with
   `deadline_ms` set: the child's `run_resumed.deadline` is set (CLI `apb
   resume <id> --deadline 5m` e2e in `run_cli_test.rs`, MCP `run_resume`
   in `run_tools_test.rs`, HTTP resume in `runs_api_test.rs`).
10. A quota probe for a run that is over. Test: an engine quota pause
    with `until` and a passed deadline; the waker stops the run and the
    probe stand-in counts no call.
11. A guard that a patch or a resume grant drops. Tests: a run started
    with `--deadline` only (no playbook field) journals `run_guard` with
    `deadline`; a supervisor patch of that run keeps it; a run without a
    deadline resumed with `--deadline` gets the guard before its
    `run_resumed`; a supervisor patch that changes `deadline` is refused.
12. A routine deadline in a scenario or an eval. Test
    (`scenario_parity_test.rs` `a_scenario_ignores_the_deadline`): one
    scenario played against two copies of a fixture playbook that differ
    only in `deadline: 1m`; the two reports' checks are equal, the two
    journals are equal after the normalization `external_data_test.rs`
    uses (`normalized`, line 329: run ids, times, spawn times and digests
    of the definition removed), and neither `run_provenance` has a
    `deadline` field. An eval case's run keeps its own deadline reason
    (`eval_cli_test.rs` line 934 still passes).
13. A connector read outliving the deadline. Test (`run_deadline_test.rs`
    `a_baseline_read_is_bounded_by_the_deadline`): a playbook whose
    `baseline_check` reads a connector stand-in that answers after 30 s,
    with `timeout_seconds: 300` and a run deadline of 3 s; the run ends
    aborted `deadline_exceeded` within a few seconds of its deadline, no
    `baseline_observed` is journaled for the check, and the outcome view
    shows it not observed (no `before_status`, verdict not `met`).
14. A credential helper outliving the deadline. Test
    `a_slow_credential_helper_is_bounded_by_the_deadline`: the same
    playbook whose account's secret is `{{cmd:...}}` of a helper that
    sleeps 20 s; the run ends within a few seconds of its 3 s deadline.
    Bounding only the HTTP part fails it (the helper keeps its 10 s).
15. Journaling a cut read as a value, or as an `unknown` that blocks a
    fresh read. Tests: `a_cut_before_read_journals_nothing` and
    `a_cut_after_read_journals_nothing` (cancel already latched, and the
    latch expired, before `read` returns: no snapshot event for the check
    in either phase); `a_resume_with_more_time_reads_afresh` (the run of
    test 13 resumed with `--deadline 10m` and a stand-in that now answers
    at once journals a `before` with status `observed`).
16. Deciding the stop outside the event lock. Test
    `a_grant_committed_first_is_not_aborted`: a `testing` hook runs inside
    `stop_if_expired` between its decision and its commit; in it the test
    starts `resume` with `deadline_ms` in another thread and waits up to
    500 ms for it. The journal must never hold a `run_aborted`
    `deadline_exceeded` after a `run_resumed` whose grant left time. A
    stop that checks expiry and then calls the stop path lets the resume
    commit inside the hook and fails it.
17. Checking the deadline only at the resume's entry. Test
    `a_resume_that_expires_before_its_commit_is_refused`: a plain resume
    whose preflight sees 1 s left and whose `testing` hook before the
    commit waits 2 s is refused `deadline_passed` under the lock, and no
    `run_resumed` is journaled.
18. A person's pause that the stop ignores. Test
    `a_person_pause_before_the_stop_wins`: a `testing` hook between the run
    watch's firing and its stop journals a person's pause through
    `pause::request_pause`; the stop answers `NotDue` (`Expect::Driving`
    no longer holds), the run ends paused, not aborted, and a plain resume
    of it is refused `deadline_passed`.
19. A deadline watch that only sees pauses. Tests:
    `a_released_run_ends_at_its_deadline` (a run released with no driver,
    `drive_released`, ends `deadline_exceeded`; a waker that tests with
    `pause::holds` ignores a release);
    `a_quota_repause_keeps_the_deadline_watch` (an engine quota pause
    with `until`; its probe re-pauses; the deadline passes after the
    re-pause; the run ends `deadline_exceeded` with no further probe).
20. A reboot that loses the waker. Test
    (`pause_waker_cli_test.rs`
    `resume_all_ends_an_expired_indefinite_pause`): a budget pause (no
    `until`) with a deadline of a few seconds and its deadline waker
    killed; after the deadline `apb resume --all` reports the run
    `deadline_stopped` and the run ends `run_aborted` `deadline_exceeded`;
    a second run with time left gets its deadline waker back
    (`pgrep -f "__pause-waker .*--deadline"` finds it).
21. A child that escapes its parent's stop. Test
    `a_child_prepared_during_expiry_never_runs`: a `testing` hook blocks
    the child's preparation in the parent's `playbook` node until the
    parent's deadline passed and its stop fanout fired, then releases it;
    the child's journal holds `run_aborted` and no `node_started`, and the
    parent ends `deadline_exceeded`.
22. A pause owner change that does not split the clock. Tests
    (`pause.rs`): a person's pause of 10 min replaced by an engine pause
    of 10 min with no resume in between reads 10 min elapsed of those 20;
    an engine pause of 10 min replaced by a person's pause of 10 min reads
    10 min as well; `paused_intervals` of both journals is unchanged (one
    interval of 20 min).

## D3. Runs started from a run step

### Record

`crates/apb-core/src/launch.rs` `LaunchRecord` (line 48) gets:

```rust
/// The run step this run was started from (`run_step:cli` or
/// `run_step:mcp` initiators). Absent for every other start. Additive.
#[serde(default, skip_serializing_if = "Option::is_none")]
#[cfg_attr(feature = "ts", ts(optional))]
pub started_by: Option<StartedBy>,
// in launch.rs
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StartedBy {
    pub run_id: String,
    /// The registered workspace id of the starting run's project
    /// (`projects::registered_id`); absent when it has none.
    #[serde(default, skip_serializing_if = "Option::is_none")] #[cfg_attr(feature = "ts", ts(optional))]
    pub workspace_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")] #[cfg_attr(feature = "ts", ts(optional))]
    pub node: Option<String>,
    /// The attempt whose token the caller carried (an agent step or a host
    /// task's subagent); absent for a script step.
    #[serde(default, skip_serializing_if = "Option::is_none")] #[cfg_attr(feature = "ts", ts(optional))]
    pub attempt: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")] #[cfg_attr(feature = "ts", ts(optional))]
    pub host_task: Option<String>,
    /// The starting run was found, created by this installation, not
    /// ended, and the node (and the token, when given) match its journal.
    pub verified: bool,
    /// 1 for a run a step of a run outside any chain started; the
    /// starter's chain + 1 after that.
    pub chain: u32,
}
/// The deepest chain of runs started from run steps.
pub const MAX_START_CHAIN: u32 = crate::schema::MAX_SUBPLAYBOOK_DEPTH as u32;
```

Every `LaunchRecord` literal gets `started_by: None` (the compiler lists
them; `gate/launch.rs` lines 439 and 825 among them).

The attempt token is never stored: it is a capability (`APB_ATTEMPT_TOKEN`
binds a connector call to its attempt, `adapter/spawn_env.rs` line 22).
The record keeps the attempt number the token resolves to.

### The caller

`StepCaller` (`crates/apb-core/src/step_env.rs`) holds only the run id and
the host task id; the start needs more, read once in the same process:

```rust
// crates/apb-engine/src/step_start.rs (new)
/// What this process knows about the run step it runs in.
pub struct CallerSnapshot {
    pub run_id: Option<String>,        // APB_RUN_ID
    pub run_dir: Option<PathBuf>,      // APB_RUN_DIR
    pub node: Option<String>,          // APB_NODE_ID
    /// APB_ATTEMPT, APB_ATTEMPT_TOKEN (agents and host tasks carry it,
    /// `scheduler/host.rs` line 144), APB_HOST_TASK and APB_ITEM_ID, read
    /// by the connector layer's own `AttemptClaim::from_env`.
    pub claim: crate::connector::call::attempt::AttemptClaim,
}
impl CallerSnapshot { pub fn from_env() -> Self; }

/// The starter of a run-step start, or the refusal `start_chain_too_deep`.
pub fn resolve(root: &Path, caller: &CallerSnapshot) -> Result<Option<StartedBy>, Value>;
/// The chain depth of the run at `run_dir`: its launch record's
/// `started_by.chain`, else its parent's (a child, through
/// `RunConfig.parent_run`, at most `MAX_SUBPLAYBOOK_DEPTH` steps), else 0.
pub fn chain_of(run_dir: &Path) -> u32;
```

`resolve`:

1. No `run_id`: `Ok(None)`.
2. The starting run's directory: `APB_RUN_DIR` when set, else
   `<root>/.apb/runs/<run_id>`. It is accepted only when the path is not a link
   (`symlink_metadata`), its canonical path (`std::fs::canonicalize`) is
   `<project>/.apb/runs/<run_id>` (the directory name equals `run_id`, a
   safe segment) for a project root that is `root` or a registered
   project (`apb_core::projects::list_active`), it is a directory and not
   a link, it carries this installation's origin stamp
   (`run_origin::verify`), and its fold is not terminal. A starter in
   another project is as good as one in this project: the chain never
   resets because the target project changed.
3. The attempt, through the one binding connector calls already use:
   `connector::call::attempt::bind(events, node, &caller.claim)` (`bind`,
   `connector/call/attempt.rs` line 127, made `pub(crate)` for this
   module, with `caller.claim` the `AttemptClaim::from_env()` of the same
   process). Verified when the binding is `may_send` (an open attempt of
   that node, and of the caller's list item, that the claim matches);
   `attempt` is the binding's attempt number. That rule already accepts
   a host task a resumed drive adopted (`claim_matches`, line 232: the
   task was requested again within the open attempt, and the subagent
   keeps the environment of the attempt that first opened it), so a host
   task that survived a pause and resume stays verified and keeps its
   chain. Do not compare the token with the open attempt's token
   directly: an adopted task carries the earlier attempt's token. Without
   an attempt, a token and a host task (a script step): verified only
   when the node is a script node of the run's definition and has a
   `node_started` with no `node_finished` after it. A claim that carries
   attempt credentials no attempt matches, or names an open visit of any
   other kind of node, verifies nobody. A script never inherits attempt
   credentials (`adapter::ATTEMPT_CREDENTIAL_ENV`, cleared at every script
   spawn): a drive started from another run's step carries that step's,
   and they would otherwise disqualify the script's own claim.
4. `chain = chain_of(starter_dir) + 1` for a verified starter, 1
   otherwise; `workspace_id = projects::registered_id(starter_root)`.
5. `chain > MAX_START_CHAIN` refuses:

   ```json
   { "policy": "start_chain_too_deep", "detail": "run `<id>` is the 5th in a chain of runs started from run steps; a 6th is refused. Start it outside a run step", "chain": 6 }
   ```

The one place every surface passes: `gate::launch::decide`
(`crates/apb-engine/src/gate/launch.rs`, the grant built around line
825). When `ctx.initiator` is `Initiator::RunStep { .. }` and `ctx.kind`
is a new start (`LaunchKind::Start`, `Trial`, `ExecutePlan` or
`ContinueFrom`; never `Resume`, `ResumeAll` or `Eval`), call
`step_start::resolve(subject.root, &CallerSnapshot::from_env())` before
the grant (its refusal is the launch refusal) and set
`record.started_by`. A resume of a run keeps the record its start wrote
and is never refused for its chain. The `__drive-supervised` child gets
the record in its handoff unchanged. Preparation's `check_launch`
(`prepare.rs` line 1481) refuses a record whose `started_by.chain >
MAX_START_CHAIN` (backstop: the grant can come from a handoff file).

`gate::launch::is_launch_refusal` lists `start_chain_too_deep`, so every
surface prints it the way it prints launch refusals.

No event is written into the starting run: the record in the new run's
write-once, stamped manifest is the link, and reverse links are derived
from it (below). A foreign writer into a live journal, and an event type
an older live driver would refuse at the journal's tail, are both
avoided.

### External data

A run started from a step of a run with `external_data: true` can carry
that run's third-party data in its params or instruction, which is what
the flag keeps out of learning stores. Today `external_data::for_start`
(`crates/apb-engine/src/external_data.rs` line 148) inherits the flag only
from a parent run and a `continued_from` predecessor, so such a start is
unflagged unless the step passed the flag. Now that the starter is
resolved, it inherits from the step's run too:

1. `refuse_step_if_flagged` (line 110) already finds the step's run
   (`APB_RUN_DIR` when its directory name is the step's run id, else the
   run of that id under the project root). That lookup becomes `pub fn
   step_run_dir(root: &Path, step: &StepCaller) -> Option<PathBuf>`, which
   both functions call (do not copy it). `for_start` gains a parameter
   `step_run: Option<&Path>` and treats a flagged step run like a flagged
   parent: the start is flagged, and an explicit `false` is refused
   `external_data_immutable` naming that run. A step run found but not
   verified for `started_by` (finished, or another installation's) still
   passes its flag on: inheriting can only keep more out. A step run not
   found passes nothing on, as for any start outside a run.
2. Every surface of a new start resolves the flag through `for_start`
   before its gate, keeping the caller's tri-state value (`None`,
   `Some(false)`, `Some(true)`) until then. Today two MCP launch kinds
   collapse it first: `playbook_execute_plan` turns it into a boolean
   with `external_data == Some(true)` before `mcp_launch` and `check_run`
   (`crates/apb-mcp/src/server/run.rs` line 189), and `playbook_trial`
   takes it from `launch.external_data`, the same collapsed value built at
   line 58 and read at `crates/apb-mcp/src/tools/trial.rs` line 153. Both
   become a `for_start(root, requested, None, None, step_run)` call whose
   result (or `external_data_immutable` refusal, through
   `tools::actions::policy_refusal`) feeds `with_external_data`, the
   params check and `RunOptions.external_data`. The CLI
   (`crates/apb-cli/src/run.rs` line 746) and MCP `playbook_run`
   (`crates/apb-mcp/src/server/run.rs` line 390) already call it and pass
   `step_run_dir(root, &StepCaller::from_env()).as_deref()` too. The HTTP
   start (`routes/playbooks.rs` line 316) passes `None`: the dashboard
   server is never a run step, and its environment is not a step's. The
   preparation backstop (below) can flag a run late but cannot refuse an
   explicit `false` or undo a gate decision taken on the wrong flag, so it
   is not a substitute.
3. Preparation's backstop `external_data_start` (`scheduler/prepare.rs`
   line 468) also flags the run when its launch record carries a
   `started_by` whose run (under this project, or under the registered
   root of its `workspace_id`) is flagged, so a handoff that lost the
   surface's value still records the flag.

Why here and not in the external data change: the starter of a run-step
start had no resolved, journaled identity before this change; with
`started_by` it has one, and the flag follows it exactly as it follows a
parent. The start key (D1) hashes the effective flag, so a keyed start
from a flagged step and the same keyed start from outside it are two
requests.

### Reverse links

```rust
/// The runs a step of `run_id` started in this project: runs created by
/// this installation (`run_origin::verify`), whose manifest launch record
/// has a verified `started_by` naming `run_id` (and this project's
/// workspace id, when the record has one). Newest first, at most 50.
pub fn run_view::started_runs(root: &Path, run_id: &str) -> StartedRuns;
pub struct StartedRuns {
    pub runs: Vec<StartedRunView>,
    /// The project holds more run directories than the scan's bound
    /// (500): entries among those it did not read may be missing.
    pub truncated: bool,
}
pub struct StartedRunView { pub run_id: String, pub playbook: String, pub scope: String,
                            pub node: Option<String>, pub attempt: Option<u32> }
pub fn run_view::started_by(run_dir: &Path) -> Option<StartedBy>; // from the manifest
```

Scope, decided: the reverse list covers the starter's own project only.
A step can start a run in another registered project (D3 keeps the chain
across projects), but finding it would mean scanning every registered
project's runs on every status read; the record of such a run already
names its starter and workspace, so that direction stays complete. The
requirement says "in the same project", and the surfaces say what the
list does not cover instead of hiding it.

`started_runs` reads run directories newest first
(`decision::report::newest_run_dirs`, line 250), at most 500, skips those
whose `run_started` time is before the starter's (`run_history::started`,
the journal head, line 52) and reads a manifest only for the others.
`truncated` is true whenever the project holds more than 500 run
directories: the order is the journal's last write, so no directory
inside the bound proves the ones beyond it hold no started run.

Links across workspaces. `started_by.workspace_id` names the starter's
project when it is not the current one. Surfaces resolve it through the
projects registry (`apb_core::projects::list_active`): the CLI prints
`in workspace <ws> (<root>)`, MCP and HTTP return `workspace_id` with the
starter, and the dashboard links to the starter's run page only when
`workspace_id` is absent or the current project's; otherwise it shows the
run id and workspace as text. A link never opens a run of another
project under the current project's id.

### Surfaces

1. `apb runs <id>` (`crates/apb-cli/src/run.rs` next to line 1910):
   `started by run <id> (node <n>, attempt <k>)` (`, not verified` when
   so; `, in workspace <ws> (<root>)` for another project) and `started
   runs: <id> (<playbook>), ...` (`started runs in this project`, with
   `, more may exist beyond the newest 500 runs` when `truncated`).
2. MCP `run_status`: `started_by` and `started_runs` (with
   `started_runs_truncated` when set); HTTP run detail the same fields;
   HTML report two lines.
3. Dashboard: the run detail type (as in D2) gets `started_by?` and
   `started_runs?`; `RunView.svelte` line 634 passes both to
   `RunDriveNotes`, which shows them with each run id of this project a
   link to its run page, a starter in another workspace as text, and the
   truncation note when set.

Docs: `docs/GUARDRAILS.md` "Launch policy for agents" one paragraph (the
record, the chain bound across projects); `docs/HOWTO-authoring.md` a
short section "Starting a run from a run step" before "Linking runs,
commits and tracker records" (line 4909).

Wrong implementations to avoid:

1. Reading the starter from the driver's env in preparation: a detached
   driver inherits the step's `APB_RUN_ID`, so every sub-playbook child
   of the new run would be marked as started by the step. Test
   (`step_start_test.rs`): a run started with `APB_RUN_ID` set whose
   playbook has a `playbook` node: the child's manifest has no
   `started_by`.
2. Storing the token. Test: the manifest bytes of the new run do not
   contain the token.
3. Trusting `APB_RUN_ID` blindly. Test: a forged `APB_RUN_ID` of a
   finished run, and an `APB_RUN_DIR` that is a link to a real run
   directory: both `verified: false`, `chain: 1`.
4. A chain that resets across projects. Test: two registered projects A
   and B; a chain of starts alternating A, B, A, B, A from run steps; the
   6th start (in B) is refused `start_chain_too_deep`.
5. A host task treated as tokenless, or an adopted host task treated as
   forged. Tests: a host task's env (run id, node, token, task id)
   resolves `attempt` and `host_task`, verified;
   `an_adopted_host_task_keeps_its_chain` (`step_start_test.rs`): a run at
   chain 5 whose host task is open, the run paused and resumed (the drive
   adopts the task, a new attempt is open), then the task's subagent
   environment (the first attempt's token) starts another run: verified,
   and refused `start_chain_too_deep` (a wrong implementation reads it
   unverified, chain 1, and lets it start).
6. A chain bound on resumes or counting sub-playbook depth. Tests: a run
   at chain 5 resumed from a run step is resumed; a run at chain 5 with a
   `playbook` node still runs its child.
7. A write into the starter. Test: after a step start, the starter's
   `events.jsonl` bytes are unchanged apart from what its own drive
   wrote (no event names the new run), and `apb runs <starter>` still
   lists the new run.
8. A step of a flagged run starting an unflagged run. Tests
   (`step_start_test.rs`): a step of a run started with `external_data:
   true` starts another playbook without the flag; the new run's manifest
   has `external_data: true`; the same start with an explicit
   `external_data: false` is refused `external_data_immutable`; a step of
   an unflagged run starts an unflagged run.
9. A trial or an executed plan that loses the step's flag. Tests
   (`run_tools_test.rs`): `a_trial_from_a_flagged_step_is_flagged` and
   `an_executed_plan_from_a_flagged_step_is_flagged`: each started from a
   step of a flagged run, once with `external_data` omitted (the new
   run's manifest records `external_data: true`, and its gate saw `true`:
   no waiting candidate taken, memory counted empty) and once with an
   explicit `false` (refused `external_data_immutable`, no run directory).
10. A truncated or cross-workspace list that looks complete or links
    wrongly. Unit tests (`run_view`): with 520 run directories newer than
    the starter and the started run among the oldest, `started_runs`
    returns it missing and `truncated: true`, also when a run older than
    the starter was written among the newest; with every directory read,
    `truncated: false`. E2e
    (`step_start_test.rs` `a_starter_in_another_workspace_is_named_there`):
    a run in project B started from a step of a run in project A names A's
    run and A's workspace id, `apb runs <id>` in B prints `in workspace`,
    and B's started-runs list of that id is empty, not an error.

## Release notes

The change is not tagged here. List in the final report: start keys
(`--start-key`, `start_key`, `start_key_conflict`, `start_key_state_unreadable`),
`deadline:` and `--deadline` with `deadline_exceeded` and
`deadline_passed`, the `started_by` record with reverse links and
`start_chain_too_deep`, the `policy.yaml` section `runs:` (an older apb
reads a file with it as invalid: it refuses launches through agent
channels and creates no default notify subscription until updated), the
graceful kill of engine aborts (`apb stop` stays immediate; a host task
is closed `cancelled`), `start_key_busy` for a start that races a
preparing one, the list of runs a step started limited to its own
project, the
`external_data` flag a run started from a step of a flagged run now
inherits, and the baseline's connector reads bounded by a run's
deadline.

## Implementation notes (decisions taken while building)

These record where the code led to a simpler correct shape than the text
above. Each was checked by tests and by `code-ranker check .`.

1. The latch is reached through a per-process registry, not the run
   config. `run_deadline::register` keeps each drive's `DeadlineLatch`
   (with its root and run id) keyed by run directory for the life of the
   drive (`LatchGuard`), and `run_deadline::refuses_work(run_dir)` is the
   one admission check every work site calls: the drive loop's top, the
   batch dequeue (before `node_started` of a queued member), the start of
   `execute_node_kind` and the attempt loop before each spawn. The node
   and list layers already carry `run_dir`, so no new parameter crosses
   them. A queued member past the deadline is not written off: the batch
   stops admitting (no `node_started`), and the drive applies the abort.
2. `stop_if_expired` is idempotent through its `due` check: besides the
   expiry and `expect`, it is not due while an abort is already pending in
   `control.jsonl` past the cursor. Without it two admission checks of one
   drive post two aborts, and a resume meets the second one.
3. The deadline clock and the deadline read side live in the leaf
   `pause/clock.rs` (the owned-segment fold, `paused_intervals`,
   `deadline_elapsed_ms`, `ended_intervals`, `holds`, `release_wake`,
   `parked`, `paused_by`, `in_force`, `remaining_ms`, `expired`), and the
   journal types `RunDeadline` and `DeadlineSource` live in `event.rs`.
   `pause.rs` and `run_deadline.rs` re-export them. With them in
   `pause.rs` and `run_deadline.rs`, `event.rs`, `run_view.rs` and
   `stop.rs` formed file cycles through the pause module's waker and resume
   paths.
4. The deadline waker's body is `pause::waker::run_deadline_waker` (next
   to the other waker bodies); `run_deadline::arm_waker` arms it. The
   `testing` barriers are the leaf module `deadline_hooks`, re-exported as
   `run_deadline::test_hooks`, keyed by a project or run directory so
   parallel tests do not see each other's hooks.
5. `run_guard` takes the journal as designed; the start calls the private
   `run_guard_of(playbook, deadline_in_force)` with the value it just
   journaled, and the definition's own `Deadline` feature counts only when
   a deadline is in force for the run, so a scenario, an eval and a child
   of a playbook with `deadline:` carry no deadline guard (their journals
   equal those of the same playbook without it).
6. `a_graceful_abort_cancels_the_host_task` lives in `host_mode_test.rs`,
   whose harness is the host; the rest of the D0 tests are in
   `stop_run_test.rs`. The rendered dashboard test mounts
   `RunDriveNotes.svelte` (`RunDriveNotes.svelte.test.ts`), the component
   that renders the line, instead of the whole `RunView` page.
7. MCP `run_resume_with` takes an `apb_engine::ResumeRequest` instead of
   one more positional argument (clippy's argument limit).
8. A secret helper with no time left fails as `CallErrorCode::Timeout`;
   one that runs out of its shortened bound keeps today's helper error.
9. A list visit's batch admission (`scheduler/list/batch.rs` `stopped`)
   is one more admission site: without it a batch was journaled
   `list_batch_started` after the deadline (found by
   `no_list_item_starts_after_expiry`).
10. A refused resume is followed by one call,
    `run_deadline::after_refusal`: an engine resume past the deadline ends
    the run, a person's refused resume arms the deadline waker (so a
    parked run whose waker died is still ended at its deadline).
11. Where the tests of a detached path need the real binary they live in
    the CLI crate: `a_detached_resume_carries_the_grant` in
    `host_mode_cli_test.rs` (a host-mode run resumes in a detached
    driver), the parked-run waker tests in `pause_waker_cli_test.rs`. The
    patch test is a unit test over `apply_patch` (`scheduler/patch.rs`),
    and MCP `run_resume` is tested on the server handler
    (`server/tests/deadline.rs`), where the `deadline` argument is parsed.
12. `run_view::started_runs` skips a run that started before the starter
    instead of stopping at it: the newest-first order is by the journal's
    last write, so an older run that is still writing sits among the newer
    ones and a stop there would hide runs the starter did start. The bound
    (500 directories) is as designed; `truncated` is set whenever the
    project holds more directories than the bound, since for the same
    reason no older run inside the bound proves the rest complete. The views live in
    `run_view/started.rs` with the two lines every surface prints
    (`started_by_line`, `started_runs_line`).
13. The CLI and MCP start surfaces resolve the flag through one call,
    `external_data::for_surface_start(root, requested, continued_from)`,
    which finds the step's run from the environment; `for_start` takes the
    step run as designed and the dashboard passes none. The MCP trial and
    executed-plan tests are on the server handlers
    (`server/tests/external_data.rs`), where the tri-state argument is
    resolved.
14. The starting runs of `step_start_test.rs` are seeded, stamped
    journals (the step is the test process with a step's variables); the
    real starts go through the run gate and the engine. The CLI e2e
    (`run_cli_test.rs` `a_step_started_run_and_its_starter_name_each_other`)
    starts through `apb run` with a step's environment.
15. `runs.start_key_days` in force is the machine's value (7 when unset),
    lengthened by the project's: a project file that sets a shorter value
    leaves the machine's, and a machine file can shorten the default (the
    text's "each defaulting to 7" would have kept every key at least 7
    days whatever the machine says).
16. A lookup holds the store lock too (`signed_store::transact` with no
    change), so its owner probe runs under the lock like every reader's.
    A probe that finds an owner gone removes that reservation's lock file.
    The engine side's refusal codes are message prefixes of
    `EngineError::Conflict` (`start_key_conflict`, `start_key_reuse_refused`,
    `start_key_busy`, `start_key_state_unreadable`); MCP and HTTP share one
    `apb_mcp::tools::run::keyed_lookup` and `already_started`, and the CLI
    maps the same prefixes to `run refused (<code>)`.
17. `apb run` has no global scope, so `a_global_start_repeats` is an MCP
    test (`scope: global`); `a_candidate_does_not_change_the_request` is
    on the MCP server handler with a forward-patch candidate. The
    on_no_estimate question of the CLI needs a terminal, so
    `a_budget_answer_does_not_change_the_request` (`budget_cli_test.rs`)
    answers it on a pseudo terminal, then repeats the same arguments
    without one and gets the first run back, not `start_key_conflict`
    (the request is captured before `budget::prestart`).
    `another_explicit_version_is_a_conflict` and
    `another_external_data_flag_is_a_conflict` are rows of
    `another_request_is_a_conflict` (engine) and `start_key_conflict_exits_2`
    (CLI). The argv test (`start_key_is_not_on_the_child_argv`,
    `supervise_cli_test.rs`) reads the `__drive-supervised` command line
    through the supervisor stub's parent process.
18. The key reaches a supervised child as `Handoff::Supervise.start_key`
    (`HandoffStartKey { key, reuse, request }`); the receiving side turns
    the tag back into a `StartRequest` with `StartRequest::from_handoff`,
    which is public because the receiver is in another crate.
