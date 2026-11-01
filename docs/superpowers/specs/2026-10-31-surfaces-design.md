# Run surfaces: secrets, next actions, read-only MCP

Date: 2026-10-31. Status: accepted, built. Author-facing usage:
`docs/HOWTO-authoring.md` ("Secret params"), `docs/MCP.md` ("Next actions",
"Read-only server"), `docs/CI.md` ("Following automatic progress"),
`docs/TESTING-GUIDELINES.md` (the journal corpus).

## 1. Goal

Every surface that shows a run (MCP, the HTTP API and dashboard, the CLI)
answers three questions the same way: what the run holds that nobody but
its executor may see, what the run is ready for right now and who may do
it, and whether the caller is allowed to change anything at all. This
document records the design as built:

1. the secret values of a run, redacted on every path that is not the
   executor, with private file modes on disk (section 2);
2. next actions: one model of a run's open waits, rendered by every
   surface, plus start actions for a start refused before a run exists
   (sections 3 and 4);
3. resolving a send of unknown outcome from the dashboard (section 5);
4. a read-only MCP server (section 6);
5. a journal corpus that runs real released binaries against journals the
   current one writes, and the reverse (section 7);
6. the defects fixed on the way (section 8).

Nothing here changes the journal: no event type and no event field is
added. The only new stored fields are the manifest's `secret_params` and
the run config's `shared_workdir` (`run.yaml`, written only when true),
which older binaries ignore.

## 2. Secret values of a run

### 2.1 Which values

A run's sensitive values are:

1. every param value that `apb_core::run_params::sensitive` flags: declared
   `secret: true`, a param whose name looks like a credential's, or a value
   that looks like a credential itself. Remembered run input applies the
   same rule, so a value that is never remembered is also never shown;
2. the connector secrets the workspace scrubber already resolves from the
   environment or a secrets file;
3. the same param values of every ancestor run, followed through
   `parent_run` up to 8 levels. A child receives its parents' values for
   redaction only, never as params, so a value a parent passed down
   through an ordinary child param is still hidden in the child.

A value shorter than 6 characters is not matched by value (it would blank
every occurrence of that text); a credential shape is still replaced.

**The set only grows.** A run records the names of the params declared
secret at start in its write-once manifest (`secret_params`). The effective
set is the union of that record, the declarations of the run's current
snapshot and the ancestors' sets. A current-run patch can add a declaration
and never removes one. A run started by an older binary has no record and
uses its current snapshot's declarations.

### 2.2 Redact on read, never in the journal

The journal, `run.yaml`, node outputs and transcripts keep what the
executor produced, because routing, conditions, resume and child runs read
them. Redaction happens where data leaves apb towards anyone who is not
the executor. One type does it: `apb_engine::run_redact::RunRedactor`, built
for a run from its root and run directory, with two uses:

1. `redact_text` for text that leaves the machine (the HTML report, the
   notices): the full workspace scrubber plus the run's values;
2. `redact_json` for structured answers: known values are replaced inside
   every string, and nothing else is touched, so paths such as `worktree`
   stay usable and numbers such as `duration_ms` and `paused_ms` arrive
   unchanged.

### 2.3 Where it applies

1. MCP: `run_status`, `run_wait`, `run_events`, `run_report`,
   `run_retro_context`, `runs_list` and the supervisor's inspect, wait and
   report context.
2. HTTP: the run detail (events, outputs and params; a hidden param reads
   `<redacted>` and is named in `redacted_params`), the run list (still
   newest first), the report, the saved report page, and every route that
   returns attempt output or log text.
3. CLI: `apb runs`, `apb wait` and `apb intent list`, in text and JSON
   (`apb tasks` is delivery to the host and shows the prompt as the
   executor gets it).
4. The HTML report and the notification texts.
5. Grading: the host grading session reads through the run redactor, and
   the model grader's provider payload is scrubbed before it is packed.
6. Known problems: a sighting, its wording and its source passage are
   scrubbed with the run's values before they are clipped and stored, so a
   later read never meets the value.
7. The git output the publish step records.
8. Every decision request (section 2.4).

Delivery to the executor is not redaction's business: the agent's prompt, a
script's environment and a host task prompt handed to the host that drives
the run carry the value.

### 2.3a Short values and unresolved connector secrets

The scrubber matches a value by value only from 6 characters on
(`apb_core::scrub::MIN_SECRET_LEN`): a shorter one would blank every
occurrence of that text. A declared secret param with a shorter value is
still shown as `<redacted>` in a params map, but text that echoes it (an
output, an event) keeps it. Every launch surface warns about it at start,
naming the param and never the value: `apb run` on stderr, MCP
`playbook_run` and `playbook_execute_plan` and the dashboard's start in
their `warnings`. It is a warning, not a refusal.

A read-only MCP server (section 6) may lack what the process that drove a
run had: a variable a connector account references is missing from its
environment, or a secret comes from a command helper it may not start. It
cannot then tell whether a run's text holds that secret, so it fails
closed (a connector whose definition or accounts do not load counts the
same: its references are unknown). A structured run answer of that
workspace then follows these rules, applied to the answer's root (each row
of a run listing is a root):

1. at the root, the string values of `run_id`, `parent_run`, `playbook`,
   `version`, `status`, `run_status`, `driver_alive`, `stop` and
   `execution` are kept;
2. in each item of `events`, the strings `type`, `node` and `ts`; of
   `next_actions`, `code`, `run_id` and `tool`; of `children`, `run_id`,
   `node_id` and `status`;
3. a kept string must read as a token (1 to 64 characters of ASCII
   letters, digits, `_`, `.`, `:` and `-`), else it is withheld;
4. numbers, booleans and null stay everywhere they are reached (at the
   root and in the items above);
5. every other value, a string, an object or an array, reads
   `[secrets_unresolved]` as a whole, its keys included.

Every object root carries `secrets_unresolved: true`; a text surface (a
report, a notice) reads `[secrets_unresolved]` alone.

### 2.4 Decision requests

Every request to a decision model, from a run (completion checks, judges,
retry advice, reviews, host task questions) or about one (grading,
known-problem rechecks, catalog ranking), loses the run's sensitive values
from its state, fields and question text before any clip, whatever
`privacy.redact` says. `privacy.redact` keeps governing only the further
scrubbing of paths, e-mail addresses and connector secrets.

### 2.5 File modes

A run directory is created 0700, and every file that can carry an echoed
value (`run.yaml`, `events.jsonl`, the question, answer and review logs,
transcripts and session files, `stdout.log`, `stderr.log`, node outputs,
host task replies) is created 0600. `apb_engine::run_perms::narrow` brings
a run directory an older binary wrote into line when the run is driven or
resumed: it clears the group and other bits only, so an executable helper
an attempt left in its work directory still runs after a resume, and it
walks with `lstat` and skips symbolic links, so a link's target keeps its
mode wherever it points. `apb doctor --run <id>` reports a run directory
that is still readable by others, with the `chmod` line that fixes it, and
writes nothing. A read-only server never narrows.

### 2.6 Compatibility

An older binary keeps creating files readable by the group and its
dashboard keeps returning plaintext params and outputs; the next drive by
this release narrows the files again. The manifest has no strict field
check, so older binaries read a manifest with `secret_params`.

## 3. Next actions

### 3.1 The model

`apb_engine::next_action` derives, from one observation of a run and its
live sub-playbook children (up to 8 levels), one action per open wait: a
question, a human review, a publish gate, a host task, a supervisor park,
a `wait` node, a pause, a dead driver, and on a finished run each send of
unknown outcome and the report. The action codes are a closed set:
`answer_question`, `decide_review`, `decide_publish`, `submit_host_task`,
`supervisor_retry_node`, `supervisor_continue_from`, `supervisor_abort`,
`resume_run`, `resume_with_budget`, `resolve_intent`, `wait_again`,
`read_report`. Every surface maps them with an exhaustive `match`, so a new
code does not compile until each surface renders it.

Each action names:

1. the run it acts on (a child's id for a child's wait), its target node,
   and the request it answers: the journal seq of the question or review
   that opened it, and for a publish gate the nonce of its plan;
2. its arguments, each either given, one of a fixed set with the engine's
   default, or supplied by the caller with a hint. An argument never holds
   a param value or a token, and a nonce appears only on the gate it
   answers;
3. whom the run accepts it from (`person`, `operator`, `host`,
   `supervisor`); whether the current caller may take it is the surface's
   answer, given as `available_to_caller` and `caller_refusal`;
4. what happens without anyone: a time at which it progresses (a question
   with a default under a live driver, a timer, a pause with `until`, never
   assured for a pause), or frozen with the remaining time while the run is
   paused. Deadlines run on the scheduler's own paused-interval clock, the
   one its question and wait deadlines use;
5. one reason line, redacted with the secret values of the run it acts on.

`stop` is one verdict over all waits: `finished`, `needs_input`, `stopped`
or `progresses_by_itself`, and `primary` the action a surface leads with.
The surface that executes an action checks everything again, so an action
shown as available can still be refused.

### 3.2 Request identity

`run_answer`, `review_decide` and the HTTP answer and review routes take
an optional `request`, the journal seq of the question or review; `apb
answer` and `apb review` take it as `--request=<kind>:<seq>` (`question:12`,
`review:7`, `publish:9`). It is checked inside the existing answer and
review locks. An
answer to a request that a later round of a looping gate replaced, or that
already has an answer, is refused as a conflict instead of deciding the
next round.

### 3.3 Surfaces

1. MCP: `run_status`, `run_wait` and `run_report` carry `next_actions`,
   `primary` and `stop`; `run_wait` adds `next`, the reason of the primary
   action. Each action names the tool and arguments that take it.
2. CLI: `apb runs <id>` lists the actions in text and `--json`; `apb wait`
   prints them after its verdict. One renderer
   (`apb_engine::next_action::command`) prints every command line: a named
   value is one word `--name=value`, positional values follow `--` when
   any of them starts with `-`, and every word is quoted for a POSIX
   shell, a placeholder `'<...>'` included: its text can come from a
   playbook or an agent (a question's options), and a line pasted before
   it is filled in runs nothing but `apb`. A rendered command counts as
   runnable only when the real CLI parser reads it back to the same
   values. Each action a surface shows carries that line as `command`;
   the dashboard prints it and renders no command line of its own.
3. `apb wait --follow-auto` takes its exit code from the verdict over the
   run and its live children: 0 or 1 when finished; 3 when something needs
   a person or a host, also on a paused run that still has a gate open;
   it keeps waiting while the run goes on by itself, including a pause
   with `until` and nothing else open, up to one minute past `until` for
   the same pause; 4 for any other stop. The default `apb wait` keeps its
   exit codes.
4. HTTP: the run detail carries `next_actions`.
5. Dashboard: a next actions panel in the right column of the run page,
   with buttons for the actions the dashboard executes, a copyable command
   line for the rest, and the automatic progress line. Each action posts to
   its own `run_id`, so a child's action reaches the child, and the
   question, review and publish controls keep the request identity they
   answer. The panel leaves out an action of the page's own run that
   another control on the page already takes, so each gate has one
   control; a child's actions and the ones nothing else offers stay.

## 4. Start actions

A start refused before a run exists answers `start_actions` beside its
policy refusal, on every launch surface (`playbook_run`,
`playbook_execute_plan`, the dashboard start, `apb run`). The codes are
`confirm_irreversible_start` (carrying the consent nonce, a person only),
`acknowledge_untrusted` and `fix_requirement` with a `remedy`:
`approve_digest`, `provide_param`, `install_agent`, `set_env`,
`run_doctor` or `manual`. A remedy never carries a value or a token. An
irreversible and untrusted start gets both actions in one refusal, and one
retry with both answers starts. The run gate keeps its signature: the
actions are built from the refusals it already returns.

The fit report (`playbook_adopt_report`, the catalog's fit, `apb adopt`,
the dashboard's fit card) carries the same start actions for what a start
would need, through one table of actionable fit codes that does not depend
on an item's severity. A code the table does not know gives no action, and
a test keeps the table complete over every code the fit report emits.

## 5. Resolving an unknown send from the dashboard

`POST /api/runs/{id}/intents/{intent}/resolve` with `{ sent, note? }`
records a person's decision on a send whose outcome nobody knows, through
the same tool layer as `run_intent_resolve` and `apb intent resolve`. It
answers 404 for an unknown run or intent and 409 for an intent that is not
open and unknown. A dashboard without keys refuses it with 403 naming the
`apb intent resolve` line, as it refuses a publish consent: anything on the
machine reaches a keyless dashboard, a step's agent included, and the
decision lets a repeat of the send go out again. The `resolve_intent`
action renders as two buttons with a confirmation that states the
consequence of each answer (a keyless dashboard shows the command line
instead); they sit with the other gate controls and stay shown after the
run ended, since such a send is usually resolved on a finished run.

## 6. Read-only MCP server

### 6.1 Guarantee

`apb mcp --read-only` serves a fixed allowlist of tools. From its start to
its exit the process writes nothing under any project root or the machine
config directory, starts no program (no process probe, agent probe,
sandbox probe, git, script, connector or secret helper), makes no network
request and sends no decision request. It shows run content like a normal
server except host task prompts and redacted secret values.

### 6.2 How it holds

Three layers together:

1. **The allowlist.** `crates/apb-mcp/src/server/read_only.rs` names 23 full tools and
   3 restricted ones, and a second table names every other tool; a test
   fails when a tool of the full router is in neither. The server is built
   with every other route removed, so a removed tool is unknown both in
   `tools/list` and in a direct `tools/call`. The supervisor role does not
   apply: read-only wins.
2. **A process-wide mode at the operation.** `main` dispatches the
   read-only server right after argument parsing, before the workspace is
   registered or the installation key is created, and enters
   `apb_core::process_mode` (with the decision client's own request ban,
   `apb_engine::read_only::enter`). While it holds, each guarded operation
   refuses at the operation itself: the atomic writers and directory
   locks, capture files, the workspace identity and `.gitignore` upkeep,
   the installation key, registry writes, opening the journal for writing
   and every append, every git command, the detection probe, the sandbox
   probe, the process probe, secret helper programs, connector calls and
   every decision request, including one from a provider chain built
   earlier. A test per operation calls it directly in a read-only child
   process; the mode is never entered inside a test thread.
3. **An end-to-end snapshot.** A test prepares a cold config directory and
   two projects with runs (a live driver, a host task, an isolated node, a
   publish node), snapshots both trees and the config directory (path,
   bytes, mode) before the server starts, calls every allowed tool, and
   finds everything unchanged after exit, with stub programs on `PATH` that
   leave a marker if they ever run.

### 6.3 What the allowed tools answer instead of acting

1. Liveness: the journal's status and `driver_alive: "unknown"`, never a
   guess from a process probe.
2. Agent detection: the memo as it is, never rewritten; without one, every
   agent reads not installed with the note that it was not probed.
3. Fit: an isolated node's sandbox item and a publish node's remote item
   are notes saying they were not checked.
4. Restricted tools: the catalog does not rank a `query` and says so;
   `agents_detect` refuses `refresh: true`; `playbook_memory` answers `get`
   only.
5. Host tasks: the task's id, node, attempt and deadline without its
   prompt, with `prompt_withheld: "read_only"`, withheld on the progress
   value itself before any copy is serialized.
6. Next actions: every action that changes a run reads
   `available_to_caller: false` with `caller_refusal: "read_only_server"`.

### 6.4 Limits

No authentication, no read-only HTTP or CLI mode. A connector secret that
only a command helper produces is never resolved for redaction, by any
server, so an agent's echo of it is not replaced by the writable server; a
read-only server withholds the run text of such a workspace instead
(section 2.3a). An older binary refuses
the flag, so a host configured for read-only against it fails to start
instead of starting writable.

## 7. Journal corpus

`crates/apb-cli/tests/suite/journal_corpus_test.rs` runs real binaries
against each other's journals. `scripts/old-apb.sh <tag> <dir>` builds a
released `apb` (1.0.0-beta.1 and 0.25.0) from a worktree of its tag; the
test reads them from `APB_OLD_BINARIES`. Every binary of a case shares one
temporary `HOME` and config directory, never the real ones, and every
operation runs on its own copy of the run directory. Writers use
script-only playbooks (a script, a human review that leaves the run
waiting, a sub-playbook node), so no agent is needed.

| # | Writer | Reader | Checks |
|---|---|---|---|
| 1 | 0.25.0 | current | read, answer and resume to the end, stop |
| 2 | 1.0.0-beta.1 | current | read, resume to the end, stop |
| 3 | current | 1.0.0-beta.1 | read, resume to the end, stop |
| 4 | current | 0.25.0 | read, resume, stop |
| 5 | current, driver killed mid-node | both old binaries | stop |
| 6 | 1.0.0-beta.1 starts and pauses, current resumes | 1.0.0-beta.1 | the journal holds records of both writers before the old binary reads it whole |
| 7 | synthetic: an unknown event after the last checkpoint | current, 1.0.0-beta.1 | read counts it, resume refuses, stop goes through |
| 8 | synthetic: a new record followed by a checkpoint | current, 1.0.0-beta.1 | resume follows the checkpoint rule |

Without `APB_OLD_BINARIES` each case prints one line and passes; with
`APB_CORPUS_REQUIRED=1` a missing binary fails the test, which is how CI
runs it (job `journal-corpus`, binaries cached by tag and toolchain).
Corpus data is synthetic; a real journal would enter it only after a
secret check.

## 8. Defects fixed

1. **A start without a param value.** The fit report said a param without
   a default and without a remembered value needs one, but every launch
   surface started the run anyway and rendered the param empty. One check,
   `apb_engine::gate::check_params`, runs after the run gate on the
   dashboard, the CLI and MCP and refuses with policy `param_needs_value`
   and the `provide_param` start action the fit report shows.
2. **Driver dead on a finished run.** A sub-playbook child driven inside
   its parent keeps a drive claim naming a process that is gone once the
   run ends, so finished children were listed as driver dead.
   `RunView::driver_dead` is now the one predicate: a dead claim on a run
   that has not ended.
3. **A closed stdout.** `apb runs | head` panicked on the broken pipe. One
   panic hook for the whole binary (`crates/apb-cli/src/pipe.rs`) turns a failed print
   to a closed pipe into a quiet exit with status 141; SIGPIPE stays
   ignored, so the servers still treat a closed socket as an error.
4. **A shared workdir for a detached start.** `apb run --detach
   --allow-shared-workdir` took no workdir lock, but its detached driver
   did not know it and waited for the holder's lock, then failed the run as
   busy. The run config keeps the choice (`shared_workdir`), and the driver
   takes no lock for it.
5. **One command renderer.** The run outcome, the publish step's
   unknown-push output, `apb intent list` and `apb publish show` printed
   `apb intent resolve` with the flag in another place than the rendered
   next action. All of them now print the engine's one renderer, so the
   terminal, the outcome text and the run page show the same line.
6. **One control per gate on the run page.** The run page showed the
   review card and the next actions panel buttons for the same gate (and
   likewise for questions, publish gates and resume). The panel now leaves
   out what another control on the page takes.

## 9. Non-goals

Authentication of MCP clients; a read-only HTTP or CLI mode; hiding run
content other than host task prompts and secret values.
