# Start rules

A start rule is a person-written definition that starts a pinned playbook
when a connector account receives a matching delivery. Nothing starts
without a rule a person wrote and approved: the ingest listener stores a
delivery and answers 200 as before, and only the trigger sweep, reading
the rules a person saved, decides that a delivery is worth a run. A rule
is not a playbook and not a profile: it names a playbook at a fixed
version and the params the event fills.

A rule lives in the project it starts runs in, as a signed file (see
"Where rules live"). Writing, approving, enabling, disabling and removing
a rule are a person's acts, at an interactive terminal with the rule's
name typed (`apb trigger`) or in a signed-in dashboard session; MCP lists
and tests rules but never changes one, and a run step can do neither.

## The rule

```yaml
name: triage-new-issues
playbook: { id: issue-triage, version: 1.2.0 }
source:
  inbox:
    connector: github
    account: main
    types: [issues]
    where:
      - { path: action, equals: opened }
    key: "{{body.repository.full_name}}#{{body.issue.number}}"
params:
  repo: "{{body.repository.full_name}}"
  issue: "{{body.issue.number}}"
limits: { max_per_hour: 5 }
budget: { usd: 2, on_no_estimate: continue }
external_data: true
require_ready: true
```

Every field:

| Field | What it is |
|---|---|
| `name` | `[a-z0-9][a-z0-9-]*`, at most 40 characters; the name a person types to change the rule |
| `playbook.id`, `playbook.version` | the playbook a match starts, always pinned to a version: a rule never follows `current` or a candidate |
| `playbook.scope` | `project` (default) or `global`, where the playbook resolves |
| `source.inbox.connector`, `source.inbox.account` | the connector (one with a `webhook:` block; an MCP tool connector has none, so it is never a source) and the configured account whose deliveries the rule listens to |
| `source.inbox.types` | event types to keep, as the connector names them (GitHub's `issues`); empty keeps every type |
| `source.inbox.where` | body conditions, each `{ path, equals }`: the value at the dot path must equal the literal; a list means every condition holds |
| `source.inbox.key` | the template of the start key (see "The start key") |
| `params` | param name to template; a template may read body paths (`{{body.repository.full_name}}`), `{{event.type}}`, `{{event.seq}}` and `{{event.received_at}}`; at most 32 params |
| `limits.max_per_hour` | the most runs the rule starts in any 60 minutes, 1 to 500 |
| `budget` | a run budget argument over the playbook's `budget:` block, per field; some budget with a token or dollar limit is required, and its effective `on_no_estimate` must be `pause` or `continue`, never `ask` |
| `external_data` | default `true`: the run's input is outside data, so the run keeps everything in its own run directory and adds nothing to a learning store (docs/EXTERNAL-DATA.md); set `false` only when the deliveries are really your own data |
| `require_ready` | default `true`: refuse a start whose readiness report does not pass |
| `notify` | leave out for the machine's default notify subscription; `false` turns notifications off for the rule's runs; `true` names the machine's target |

There is no `enabled` field and no `attended` field. `enabled` lives in
the store entry and changes only through `enable` and `disable` (see
"Enabled and approved"). A rule's run is always unattended (see "A
rule's run is always unattended"); a document that names either field is
refused (`trigger_enabled_not_settable` for `enabled`).

## Two examples

In code, the rule above: a GitHub `issues` delivery with
`action: opened` starts `issue-triage` pinned to 1.2.0, the key naming
the repository and issue, at most 5 starts per hour.

Not in code, the same shape for a CRM: a new-lead delivery starts a
`qualify-and-reply` playbook with the lead id mapped to a param marked
`untrusted: true` and a key of the lead id alone, so later updates of
one lead start no second run while the key lives.

## The order of conditions

One stored event is evaluated in this order, and the first check that
fails decides the skip reason:

1. the filter: the event's type against `types` first, then each `where`
   condition in the rule's order. A miss skips with `type` or `where`;
2. the send marker: if a body path the rule's key and param templates
   read ends with the exact hidden send marker of a run this
   installation created, the delivery is apb's own send, skipped
   `own_send`. A marker quoted in other text, one in a field the rule
   does not read, one naming a run another machine made, or one whose
   run is not a plain run id of this project's runs directory (a path,
   a link) is not a skip;
3. the start key: the template renders against the event and must give
   a valid key. `key_missing`, `key_not_scalar`, `key_too_long` or
   `key_invalid` skips;
4. every param in name order: `param_missing`, `param_not_scalar` or
   `param_too_long` skips.

The skip reasons are a closed set: `type`, `where`, `own_send`,
`key_missing`, `key_not_scalar`, `key_too_long`, `key_invalid`,
`param_missing`, `param_not_scalar`, `param_too_long`. A rendered param
that then misses its `pattern` refuses the start as
`param_pattern_mismatch`. Event text reaches a param only when the
playbook declares that param `untrusted: true`; a `pattern` narrows the
value but never replaces the declaration (validator code V262). A
playbook param that is `secret: true` and has no default refuses the rule
(V262 again): a rule cannot fill a secret, so no event could start the
playbook.

## The event is the unit

The sweep walks each rule's new events in order. For each one it starts
the run now, through the ordinary start sequence every surface uses, or
records the outcome as the event's verdict. Nothing is queued, held for
later or merged. Exactly three verdicts leave the rule's position before
the event and end the pass, to be tried again on the next pass: a busy
start key (`start_key_busy`: another start with the key is still being
prepared, its run not yet bound to the key, so it may never start), the
rule's own hourly cap, and a changed approval binding
(`binding_changed`, which also revokes the approval). A pass that ends
early for another reason (a person's change waiting for the rule, the
pass's start bound, a journal that cannot be written, a rule that changed
under the pass) also leaves the position before the event it did not
decide. Every other outcome is final for that event: a skip with its
reason, a started run, a run the start key answers as already started,
and every refusal with its code. A refusal
is not retried; the journal says why, and `apb trigger test` checks the
event again once the cause is fixed. One pass evaluates at most 500
events of a rule and starts at most 10 of its runs, so one busy rule
cannot starve the others; what it did not reach waits 5 seconds, not
forever.

## The start key

The key template is the dedupe. A matching event renders the template
and prefixes it, `trigger:<incarnation>:<rendered key>`, and that whole
string is the run's start key. One rendered key starts one run while it
lives, exactly as any start key does: a resent delivery, a delivery the
provider replayed, or a crash between a start and its record all answer
`already_started` with the first run, and that answer comes before the
hourly cap and the pass's start bound are counted, so a pass that crashed
after a start records it at once. A key of the event's id gives once
per event; a key of an entity id, the repository and issue number, the
lead id, gives once per entity, so updates of one entity start no second
run while the key lives.

The key lives for `runs.start_key_days` (default 7, the setting of any
start key). After that, a delivery of the same key starts a new run; if the
rule should not run twice for one entity, raise the setting. A failed
run holds its key until a person says `--start-key-reuse`, as for any
start. Neither the state nor the journal keeps the key; `apb trigger
test` shows its tag, never the key, unless you asked for values at a
terminal.

## Runs at once

How many runs of a playbook may run at once is the playbook's own
`concurrency:` block, unchanged; a rule adds no limit of its own. With
`on_busy: refuse`, an event whose start hits a busy key is refused with
`concurrency_busy` as its verdict, recorded like any refusal; the pass
goes on to the next event. With the default queueing, the run is
admitted and waits for the tree or its turn in its own drive.

## A rule's run is always unattended

A rule always starts `--unattended`. A playbook whose gate needs a
person fails that run `hitl_required`: a routine whose gates a person
answers is not a rule's job. Because the run is unattended it never
earns attended probation credit, and neither does a sub-playbook child
of it; probation counts attended successes only, and a rule's run needs
no exclusion of its own.

On a long timer wait a rule's run keeps its detached driver, exactly as
`apb run --unattended` does today. Letting such a run release its driver
is future work; until then a rule whose playbook parks for hours holds a
driver for those hours.

To an older apb, a rule's run is an ordinary unattended run: it resumes,
pauses and stops it as one, correctly, because nothing about the run's
behaviour depends on the field that names the rule. The run's launch
record names the rule (`triggered_by`), and every run surface shows it:
`apb runs <id>`, MCP `run_status`, the dashboard's run page and the HTML
report. A run whose record lost the field, as an older apb's manifest
amendment can, shows no rule.

## Billed use

A rule needs `allow` in the `billing:` section of `policy.yaml` for the
basis of every executor its run binds (`billed` and `unknown_basis`
alike). A billing `ask` needs a person's confirmation of a single-use
challenge, and a rule cannot give one: nobody is there to ask. Such a
start is refused `billing_refused_for_trigger`; the remedy is allowing
the basis in `policy.yaml`, or starting the playbook from a terminal.
The billing decision is part of the rule's approval binding (below), so
a settings change that makes billing stricter is not a way around the
approval either.

## Approval

Every rule needs an approval to start anything, an `allow` playbook's
too. A rule starts paid runs with nobody at the keyboard, so the approval
is the person's decision. `apb trigger approve <rule>` shows exactly
what is approved:

1. the rule's digest: every field that changes whether or how the rule
   fires, and the rule's incarnation. Any change of the rule, its
   params, its limits, its budget, its key template, drops the approval,
   and the rule starts nothing until it is approved again;
2. one binding over the target as it stands when approved: the permit
   digest (the playbook version, its profile bundles, its connectors and
   connector accounts, its sub-playbook children), the launch policy in
   force after the floors and `policy.yaml`, and the full billing
   decision for the tree's executors.

The binding is what an `ask` launch policy needs: for a rule, the
approval itself is the confirmation, compared at every start in the
gate's admission. There is no separate confirmation question and no
stored nonce; `deny` still refuses a rule's start whatever the approval
says.

The binding makes the guarantee precise: a rule never starts a run under
a binding other than the approved one. A start attempted under a changed
binding, a stricter policy included, starts nothing, records one
`binding_changed` line and revokes the approval for good; restoring the
old target afterwards does not bring the approval back, only a fresh
`approve` does. A change that was restored before any start was
attempted revokes nothing: an event the start key answers
`already_started` attempts no start, and while every run still starts
under the approved binding, the approval stands.

A change of the rule's connector or account is a change of source and
makes it a new rule: a new incarnation, a fresh position, a journal of
its own and no approval, even if nothing else changed.

The boundary of an approval, said once and plainly: like apb's launch
floors and its other signed stores, a rule approval guards against
mistakes and against agents acting through apb's surfaces. It does not
guard against another process of the same user that writes the signed
file: the installation key it is signed with is readable by that user.
Binding an approval to a passkey is future work.

## Enabled and approved

`enabled` is changed only by `enable` and `disable`, never through
`set` (`trigger_enabled_not_settable`), so a save can never switch a
rule on and see what arrived while it was off. A new rule is enabled but
not approved, and starts nothing until its first `approve`.

`approve` and `enable` delete the rule's stored position, and the first
pass after the command positions the rule at the head it reads then and
starts nothing in that pass. A rule acts only on what arrives once it is
approved or enabled; events that arrived within one sweep period of the
command may be skipped, which is the safe direction: an old event
replayed after its start key expired would start a second run.

## The hourly cap

`limits.max_per_hour` counts the runs the rule's incarnation started in
the last 60 minutes, read from the runs' own launch records, so a crash,
or an event lost to retention, cannot lose a start from the count, and
an `already_started` answer adds nothing. The cap is 1 to 500; V264 refuses
a higher one, because the count reads at most the newest 500 run
directories of the project written in the last hour.

The count is conservative. A run of the last hour whose manifest cannot
be attributed, a stamp that does not verify, a manifest that does not
read, or a launch record that names a rule but lost the detail, counts
toward the cap of every rule of the project. A stamped directory with no
manifest counts for none: no work started there, and one failed start
should not hold every rule for an hour. When the count itself cannot be
read (the runs directory cannot be listed), the cap counts as reached.
A run of the rule whose journal is gone or does not read, while its
launch record names the rule, counts as a start inside the hour: when it
started cannot be read, so it is never assumed to be older. When the
count cannot be read at all, the cap counts as reached:
the event is held `rate_cap_unknown` and the rule starts nothing this
pass. A cap hold leaves the position before the event and journals one
`held` line; the event starts once the oldest counted start is an hour
old.

## Where rules live

A rule is saved only in a registered project (`trigger_project_unregistered`
otherwise): the listener sweeps registered projects only, so a rule of a
project the registry does not know would never run. The rules and the
sweep state are two signed files under `.apb/` (git-ignored), bound to
the project's registered workspace id and this installation, read with
no link followed; a rules file copied into another project, or reached
through a link, reads as unsigned and starts nothing, and `apb doctor`
names it. No write can repair such a file, so every surface gives the
same remedy: `trigger_rules_unreadable: <path> does not verify (changed
outside apb, written by another installation or for another project);
move it aside to write rules again`. `apb trigger set` says it before it
asks anything. The journal of each rule is read and written with no link
followed too, and a line already in it is synced again before the
position moves past the event it explains.

Each creation of a rule, and each change of its connector or account,
gets a random incarnation. The rule's state (its position), its journal
and its start keys are bound to that incarnation, so a rule removed and
created again under the same name inherits nothing: no position, no
journal lines, no start keys.

A rule registers no inbox consumer and takes no reading position in the
connector's cursors. It reads the account's events above its own
position; when retention removed events it had not seen (the inbox's
size cap drops the oldest events whatever consumers say), the rule
journals one `gap` line with the range and continues from the oldest
kept event.

## The journal

Each rule incarnation has a trigger journal, `.apb/trigger-state/log/<incarnation>.jsonl`
in the project, the newest 1000 lines. A line holds codes, condition
indexes and pass or fail, never values, never a condition's literal,
never an engine's sentence: a busy concurrency key's message, for one,
would name the key's display, which renders params. Per condition of the
filter, in the rule's order, the line keeps the index and whether it
passed. Every surface renders a line's words from its code, so a value
cannot come back through the wording. The journal decides nothing: the
position lives in the signed state, and the hourly cap reads the runs'
launch records. It answers "why did this event not start a run".

## Where the sweep runs

The trigger sweep runs inside the ingest listener: `apb ingest`, or
`apb dashboard` with `ingest.enabled` in the global config. It runs
every 5 seconds and at once after a stored delivery. Besides it, `apb
resume --all` runs one synchronous pass over its project, so what
arrived while the machine was off is evaluated when the machine is back.
Nothing else runs it: no daemon, no service, never a run step (the
listener is a person's process and `apb resume --all` inside a run step
skips the sweep before it reads anything). `apb doctor` warns when
enabled rules exist and no sweep runs.

Two hosts never start one event twice: every pass holds the rule's
sweep lock, a second sweeper skips a held rule, and the start key
covers the rest. A pass yields to a person's change of the rule: a
mutation holds the rule's change lock, a pass probes it before every
start and ends its pass rather than start the old rule after the change
committed; a mutation waits for the start in progress at most.

## The commands

Writing, approving, enabling, disabling and removing are a person's
commands: they run at an interactive terminal and ask you to type the
rule's name, and they refuse inside a run step. There is no `--yes`.

| Command | What it does |
|---|---|
| `apb trigger set <rule> --file <rule.yaml>` | Create or change a rule. The file's `name` must equal `<rule>`. Every issue is printed and an error refuses the save before the confirmation. Changing an approved rule prints that the approval was dropped and that the rule needs approval again |
| `apb trigger list` | The rules: enabled, source, playbook and version, starts in the last hour, the last verdict, and which need approval |
| `apb trigger show <rule>` | The rule as YAML, its incarnation and rule digest, the approval, its position and the last 20 journal lines |
| `apb trigger approve <rule>` | Show what is approved (the digest and the binding above) and ask for the name typed. The rule starts nothing before this |
| `apb trigger enable <rule>` / `apb trigger disable <rule>` | The only way to change `enabled`. `enable` also repositions the rule at the head (see "Enabled and approved") |
| `apb trigger remove <rule>` | Remove the entry, its state and its journal. A crash in between leaves inert leftovers that decide nothing and that `apb doctor` lists with their paths |
| `apb trigger log <rule> [--limit N]` | The journal lines, newest last: each with its event seq, the words of its code, each `where` condition's index and whether it passed (`conditions #0 passed, #1 failed`, never the condition's literal), the param it names, the validator codes of a `check_rule` refusal and the run id. `show` prints its lines the same way |
| `apb trigger test <rule> [--event <seq>] [--json]` | The dry test (below). Exit 0 when the event matches and passes the checks before start, 3 when it does not, 2 on a usage error |

## The dry test

`apb trigger test` explains one stored event, the newest unless
`--event <seq>` names one: how the filter and each condition judged it,
every param's render (with values only at a terminal outside a run
step), the start key's tag, whether that key is free, used (by which
run) or busy, and the checks a start surface runs before preparation,
each passed, refused with its code, or not run because an earlier one
refused. The test runs the same code the real start runs, in the same
order; it writes nothing, claims no lock, and works in a read-only
process. MCP `trigger_test` and the dashboard's Test button answer the
same report without values.

It never says "would start". Preparation's own checks do not run in a
dry run, and the report names them as checked at start:
`probation`, `readiness`, `concurrency` and `working tree`. Those four
are decided only when a run actually starts, and the last two depend on
what else is running at that moment.

## Validator codes V260 to V267

The codes check a rule, not a playbook: a person writes a rule outside
the playbook, so an agent that edits a playbook can never add one. A
rule with an error is never saved; warnings are saved and shown. The
sweep re-checks the rules that can drift (the playbook, the connector)
before each start and records a `check_rule` refusal with the codes.

| Code | Severity | Rule |
|---|---|---|
| V260 | error | the source does not resolve: unknown connector, a connector without a `webhook:` block (an MCP tool connector included), unknown account, `types` on a connector whose webhook names no event type, a `where` condition the event filter refuses, a template that does not parse, or a name that is not `[a-z0-9][a-z0-9-]*` of at most 40 characters |
| V261 | error | the playbook does not resolve (unknown id or version) or its lifecycle is `draft` or `retired` |
| V262 | error | a mapped param is not declared or is `secret: true`; a param that reads event text (`{{body.*}}` or `{{event.type}}`) is not `untrusted: true`; a required param without a default is not mapped; a `secret: true` param has no default (no rule can fill it, so no event can start the playbook) |
| V263 | error | no budget: neither the rule's `budget` nor the playbook's `budget:` limits tokens or dollars, or the effective `on_no_estimate` is `ask` |
| V264 | error | a limit out of range: `max_per_hour` not 1 to 500, an empty `key`, more than 32 params |
| V265 | warning | the playbook can write through the connector and account the rule listens on with a function whose `idempotency` is not `marker`: its own writes come back as new events. The skip of a marked send (`own_send`) and the hourly cap bound the loop |
| V266 | warning | `external_data: false`: what arrives from outside is kept for learning |
| V267 | warning | the effective `on_no_estimate` is `pause` while the rule's runs are external data (the default) |

Why V267: an external data run never enters estimate history, so a
version with no learnable run has no estimate for the budget gate, and
`on_no_estimate: pause` holds each of the rule's runs at its first
gated node until a person resumes it. A rule runs unattended; nobody is
there to resume. Give the rule `budget: { ..., on_no_estimate: continue }`
as the example does, or a limit the playbook can meet without an
estimate.

## Time-based starts

A rule that starts a run at a time (a cron expression or a one-off
instant) is not in this release; it comes after 1.0, as a new kind of
`source:` that leaves every rule written now valid. Until then a
time-based start is an outside timer calling apb directly, with a key
that names the slot so a timer that fires twice starts one run:

```sh
apb run <playbook> --unattended --start-key nightly-$(date +%F) --detach
```

Pair it with `apb wait` as docs/CI.md describes, and keep the key
stable per slot (`nightly-2026-11-23`, not a fresh random value) so the
retry of a timer step answers `already_started` instead of running the
routine twice.
