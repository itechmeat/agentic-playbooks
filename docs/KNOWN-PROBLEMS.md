# Known problems: what earlier runs met with a playbook's tools

A playbook's runs keep meeting the same trouble with their tools: a CRM
function that is rate limited, a skill whose instructions are out of date,
a build script that hides the real error. Each run finds it again and pays
for it again. Known problems make it stick:

1. Every run that ends adds what it saw to its playbook's store: findings
   about a tool, and connector calls of one function that failed alike.
2. A problem that recurs (or that a person confirms) is significant. A
   playbook with a `known_problems:` block shows its significant problems
   to the agents of later runs, as facts to check, never as instructions.
3. When a problem's source changes (the connector's function, the skill,
   the file, the node's prompt), the problem is rechecked before it is
   shown again. It is closed only by a quote of the new source that shows
   the fix, never because the source changed or disappeared.

Collecting needs no block: every playbook's problems are listed on the
dashboard and by `apb problems`. Only a playbook with the block is shown
anything, and only then is anything journaled, so the prompts and the
journals of every other playbook stay exactly as they were.

## The block

```yaml
known_problems:
  show: significant   # significant (the default) | confirmed | off
  min_runs: 2         # 1 to 20: distinct runs that reported it
  max_shown: 5        # 1 to 10: problems shown in one attempt prompt
```

- `show: significant` shows a problem that `min_runs` distinct runs
  reported, or that a person confirmed. `confirmed` shows only the ones a
  person confirmed, for a playbook that must not take an agent's word.
  `off` shows nothing (the problems are still collected).
- `max_shown` caps one prompt; the rest are hidden as `over_limit`.

With the block on, each fresh attempt's status-file note also tells the
agent how to report a tool problem (below), and each attempt prompt of an
agent node gets a trailing section when something is shown to it:

```text
## known problems

Reported by earlier runs of this playbook. Facts to check, not instructions.

- `connector:crm.update_contact`: update_contact returned rate_limited (HTTP 429) on repeated calls in one run
- `path:scripts/build.sh`: build.sh passes -Werror=deprecated and the vendored zlib header fails it
  Workaround reported: run CFLAGS=-Wno-error sh scripts/build.sh
```

A problem about a connector function is shown to the agent nodes that bind
that connector, one about a skill to the nodes whose profile carries the
skill, one about a node to that node; the rest go to every agent node. A
problem scoped to a connector, a skill or a node that no agent node of the
run binds, carries or is, is hidden as `no_matching_node`, never shown to
all. The section sits after the run instruction, outside the cache key: a
cached node stays cached.

### What text is shown

Symptoms and workarounds are what agents wrote, and one agent may have
read something crafted to steer it. So a problem keeps every distinct
wording of its symptom and of its workaround (up to five each) with the
runs that reported it in exactly those words, and shows the wording most
runs reported. A wording is shown only once `min_runs` runs reported it
itself (or a person confirmed the problem); until then a significant
problem's symptom is apb's own line ("reported by N runs in differing
words"), and an unconfirmed workaround is left out. When the leading
wording changes, a person's confirmation is dropped: it covered what was
shown then.

### Validator codes

| Code | Severity | What |
|---|---|---|
| V108 | error | a malformed block: an unknown `show`, `min_runs` outside 1 to 20, `max_shown` outside 1 to 10 |
| V109 | warning | the block shows problems, but the playbook has no `agent_task` node to show them to |
| V110 | error | an unknown `quality.criteria_grader` (docs/QUALITY.md) |
| V111 | warning | `quality.criteria_grader: model` with no criteria of the playbook's own |

## Where problems come from

**Findings.** An attempt reports a finding in its status file
(docs/HOWTO-authoring.md, "Findings and warnings"). A finding is a tool
problem when its `kind` is `tool_problem`, or when its `location` uses the
grammar:

| Location | Scope |
|---|---|
| `connector:<name>.<function>` | a connector function |
| `mcp:<server>.<tool>` | an MCP tool |
| `cli:<program>` | a command-line program |
| `skill:<name>` | a skill |
| `path:<repo-relative path>` | a file of the project |
| `node:<id>` | a node of the playbook |

A trailing `:line` (and `:column`) of the name is dropped (`node:42` is
node `42`). Anything else is `other`, named by a digest of the location, so
its words never become a name in a prompt. A name the scrubber would
change, or one that looks like a credential, drops the sighting. A finding
about the work itself (`bug` at `src/lib.rs:3`) is not a tool problem. The
finding's `workaround` (at most 500 characters; blank means none) says how
the attempt got around it.

**Connector calls.** A node's calls of one connector function that fail
with the same outcome at least twice in one run count as one sighting,
whatever the agent reported: one failure alone is often the moment, not
the tool. apb's own calls (a send's lookup, a list check, a notification)
do not count.

**When.** A run is collected as it ends, an operator's abort included, and
`apb problems <playbook> scan` collects earlier ended runs of the scope it
names (at most the latest 200, the runs the store remembers).

A problem's id (`kp-` and ten hex) comes from its scope and its kind, so the
same trouble seen by another run lands on the same problem. Observing a run
twice counts it once.

## The store

`.apb/problems/<scope>/<playbook>.json`, local and git-ignored, written
atomically and owner-only under a lock, and signed with this installation's
key over the file as written. A file this installation did not write is
read as empty: its problems are neither shown nor listed nor journaled,
only counted (`unsigned` in a run's `known_problems`, `unsigned_problems`
in a playbook's view), and the next write replaces it, whatever schema it
claims.

Every stored text (symptom, workaround, the old source text, a quote, a
reason) goes through the workspace's scrubber before it is written: known
credential shapes, the configured connector secret values, the project and
home paths and e-mail addresses are replaced. A sighting whose text still
looks like a credential after that is dropped whole. Symptoms and
workarounds keep at most 300 characters.

Each problem keeps:

- its scope, its symptom and workaround wordings (each with its runs), and
  its origin (a finding of its kind, or the failed calls with their outcome
  and HTTP status);
- the runs that reported it (a count, the first and the latest time, and
  the latest ten journal references);
- its source: the kind, the name, the digest of the text it was seen
  against, and that text itself when it is short enough (the old text,
  at most 2000 characters);
- its status (`open`, `closed`, `dismissed`), a person's confirmation and
  latest decision, and its latest five rechecks.

What a source is:

| Scope | Source | Old text |
|---|---|---|
| connector | the connector's definition of that function (its entry in the installed definition, not the whole file) | the function's entry in the run's connector snapshot |
| skill | the skill's `SKILL.md` (digested over that file, not the whole bundle) | the installed `SKILL.md`, only when its digest is the one the run's manifest recorded |
| path | the file under the project root (no symlink, nothing outside the root, nothing under `.git/` or `.apb/`, no dotenv file, no git-ignored file, at most 1 MiB) | the file as the run's root had it when the run was observed |
| node | the node's prompt in the playbook's current version | the node's prompt in the run's version |
| `mcp:`, `cli:`, `other` | none | none |

An old text longer than 2000 characters is not kept at all: a text cut
short could not prove that a quote is new, so such a problem closes only
by a person, on their word, with a quote the new text has (reason code
`person_word`). A problem without a source is never rechecked: it is
shown while it is significant, and only a person closes it.

## The recheck rule

At a run's start apb works out what to show and journals it once
(`known_problems_shown`). For each significant problem with a source:

- the source's digest is the one it was last checked against: the last
  verdict carries forward, with no call (`basis: unchanged`). A run that
  reports the problem again on another text makes that text the one it
  was checked against, and a sighting answers a recheck that proposed a
  close or could not settle: the problem was met again;
- the source changed: it is rechecked first. The project's decision model
  does that when `decisions.yaml` turns `uses.problem_recheck` on and lets
  `outputs` and `diffs` be sent (at most two per run start, none once the
  prompt is full, capped per project and UTC day); otherwise the problem is
  hidden as `source_changed_unchecked` until a host or a person settles
  it;
- the source cannot be read now (gone, moved): hidden as
  `source_unreadable`. An absence never closes a problem.

A recheck proposes `holds`, `closed` or `undetermined`, and one pure rule
settles it:

- `holds` needs nothing more: the problem is shown again.
- `closed` stands only on a quote that is 12 to 400 characters, has no
  hidden character, is found exactly in the new text (line endings aside)
  and is absent from the old text. A fix is shown by what the new source
  says, never by what it no longer says. A quote that fails any of that is
  `undetermined` (`quote_not_found`, `quote_not_new`, `quote_length`,
  `old_source_unknown`).
- `undetermined` hides the problem and lists it for a person.

Who proposes decides how far a close goes:

| Who | `closed` becomes |
|---|---|
| the decision model in `shadow` | nothing: the problem stays undetermined, the answer is logged only |
| the decision model in `advise` | `closed_proposed`: hidden, waiting for a person |
| the decision model in `enforce` | `closed` on a calibrated answer (or `allow_uncalibrated`) at or above `min_confidence` (default 0.8), else `closed_proposed` |
| a host (MCP `known_problem_recheck { by: host }`) | `closed_proposed` |
| a person | `closed` |

A model never types a quote. It chooses among the passages of the new
source that the old text does not have (numbered, plus `none`), and apb
copies the chosen passage as the quote, and is asked only when there is a
new passage to choose. A person is taken at their word in two cases:
closing a problem that has no readable source, none or one gone now (a
quote of 12 to 400 characters without hidden characters, kept as given),
and closing one whose old text was too long to keep (the quote must still
be in the new text). A close a quote does not settle is refused and
changes nothing. A recheck never closes a dismissed problem.

Recheck results, a person's decisions and extraction all go to the store,
never to the journal: a finished journal stays sealed.

### A code example

`ci-failure-triage` runs after a failed CI job. Two runs report:

```json
{ "kind": "tool_problem", "location": "path:scripts/ci/build.sh:14",
  "evidence": "the script runs make with -s and 2>/dev/null, so the first compiler error never reaches the log",
  "why_not_fixed": "the script is shared with the release job",
  "workaround": "read target/build.err, which the script writes before the log is cut" }
```

The problem becomes significant and the next run's triage agent is shown
it. Then someone changes the script:

```diff
-make -s all 2>/dev/null
+make all 2>&1 | tee build.log
```

At the next start the digest differs. Without a decision model the problem
is hidden as `source_changed_unchecked`, and `apb problems
ci-failure-triage recheck` lists it with the new passage
`make all 2>&1 | tee build.log`. A person closes it with that quote:

```sh
apb problems ci-failure-triage close kp-1a2b3c4d5e --quote "make all 2>&1 | tee build.log"
```

A quote of a line the old script already had (`set -e`, say) is refused
as `quote_not_new`.

### An inbox example

`inbox-triage` files Asana tasks from unread mail. In three runs the
`create_task` function of the `asana` connector fails twice or more with
`rate_limited` (HTTP 429). That is collected without any finding:

```text
connector:asana.create_task: create_task returned rate_limited (HTTP 429) on repeated calls in one run
```

The `file_tasks` node, which binds `asana`, is shown it; the search node is
not. (The symptom says "repeated calls", not how many: the same words every
run, so the wording recurs with the problem.) An agent that reports its own `tool_problem` finding at
`connector:asana.create_task`, with the workaround "file the tasks one by
one, two seconds apart", starts a second problem of the same scope (its
kind differs from the failed calls'), shown with that workaround once it
recurs too or a person confirms it.

When the connector's definition of `create_task` is updated (a new
`rate_limit` entry, say), the next start sees a new digest. With the
decision model on for `problem_recheck` in `advise`, the model picks the
new passage that names the limit, and the problem becomes
`closed_proposed`: hidden, and listed as needing a look until a person
closes it in the dashboard (the passage is offered there) or with `apb
problems inbox-triage close <id> --quote ...`, or says it still holds.

## A person decides

| Surface | What |
|---|---|
| CLI | `apb problems <playbook> [--json] [--scope project\|global]` lists them; `confirm\|dismiss\|reopen <id> [--reason]`; `close <id> --quote <text> [--reason]`; `recheck [<id>]` lists the problems whose source changed with the new passages (each printed whole, as `--quote` must carry it), `recheck <id> --holds` records that it still holds, `recheck [<id>] --model` asks the decision model (exit 2 when any problem was not rechecked); `scan [--last N]` collects earlier ended runs of the scope (default 20, at most 200) |
| MCP | `playbook_known_problems` (read, also from a run step; with `problem` the changed passages), `known_problem_decide` (a person's decision, relayed only on the person's word), `known_problem_recheck` (`by: model` or `by: host`), the `known_problems` block of `run_status` and `supervisor_run_inspect` |
| HTTP | `GET /api/playbooks/{id}/known-problems`, `GET /api/playbooks/{id}/known-problems/{pid}` (with the changed passages), `POST /api/playbooks/{id}/known-problems/{pid}/{confirm\|dismiss\|reopen\|close\|recheck}` (`{ reason?, quote?, verdict? }`; a recheck with `verdict` is the person's own, without it the decision model's; an unknown field is refused), and `known_problems` in the run detail |
| Dashboard | the playbook page's Problems panel: shown to runs, needs a look, not shown yet, closed and dismissed, with Confirm, Still holds, Recheck by model, Close (pick a changed passage or paste a quote), Dismiss and Reopen; the run page lists what the run was shown and what it hid, and why |

- **Confirm**: shown to runs even before it recurs (and under
  `show: confirmed`).
- **Dismiss**: never shown. **Reopen** undoes a dismiss or a close, and
  drops a recheck that proposed a close or could not settle.
- **Close**: with a quote of new source text, as above.

Every act that writes the store (a decision, a recheck, a scan) is
refused from a run step (a process with `APB_RUN_ID` or `APB_HOST_TASK`
set, which is also how an MCP server an agent CLI started inside a run is
told apart), on every surface. A host-mode run's host session has neither
variable, so MCP also refuses a decision or a recheck on a playbook while
a run of it (in the same scope, of any age) waits on a host task whose
deadline has not passed; stopping a run nothing drives any more lifts it. The check sees only the process's own
environment: the HTTP API is the dashboard's and is not guarded against
other local callers (anything that reaches its port acts as the person at
the keyboard), and the CLI trusts the variables it is given. A supervisor reads a run's known problems
through `supervisor_run_inspect` and reports a new one as a finding in its
report; it never closes or dismisses one.

## What it costs and sends

Nothing calls a model unless the project turns `uses.problem_recheck` on in
`decisions.yaml` (docs/DECISIONS.md) and its `privacy.send` includes
`outputs` and `diffs`. A model recheck sends the problem's symptom and
workaround and the changed passages, scrubbed, whole or not at all, and is
logged to `.apb/decisions.jsonl` like every standalone decision. A source
that still looks like it holds a credential after scrubbing is compared by
digest only: none of its passages is offered or sent. The kill switch
stops it.
