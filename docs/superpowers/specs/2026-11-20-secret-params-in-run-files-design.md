# Secret params in run files: design

The public record of this change: its proposal, then its design. Line
numbers are those of main efae5822 (Wait park, #290; apb 1.0.0-beta.7).

## Proposal

### Why

A param declared `secret: true` is the operator's private value. apb
keeps it for the life of a run in `runs/<id>/run.yaml`
(`RunConfig.params`, `crates/apb-engine/src/run_config.rs` line 156,
written with mode 0600 by `write_run_config`, line 333), because every
resume reads it there. On main efae5822 (Wait park, #290; apb
1.0.0-beta.7) a playbook may also reference the value in any template:
`{{params.<secret>}}` renders it into a prompt, a host task's
`prompt.md`, a `prompt` node's journaled output, a path, a cache key, a
publish branch or a connector argument, and from there into logs,
session stores and the journal. V99 and V100
(`crates/apb-core/src/validate/params.rs` lines 13 to 31) refuse only a
secret with a default and one that is not text.

The first design of this change scrubbed the value out of every file
after it was rendered. Its review found one blocker and ten major
findings: every pass found another write site, another encoding, another
crash window, and the scrub changed runs that have no secret at all. That
approach does not converge (design "Why the first design was replaced").

This was found in the design review of one-point-oh-fixes (its D4.12).

| Example | Code | Non-code |
|---|---|---|
| a prompt reads a secret | V340 refuses `prompt: "deploy with {{params.token}}"`; the deploy script reads the token from `run.yaml` | V340 refuses a CRM routine's prompt that reads its API token; the connector's account secret carries it |
| a path reads a secret | V340 refuses `workdir: "build/{{params.token}}"` | V340 refuses a `writes` entry that names a mailbox password |
| a stored version | starting a stored review playbook whose prompt reads a secret is refused before any run directory exists | the same for a stored mail triage playbook |

### What changes

1. apb never renders a secret param's value into text. V340 refuses
   every reference to a param declared `secret: true`, in every form the
   renderers resolve, in every string of the definition, keys included;
   a definition that cannot be walked is refused, never passed (design
   D2, D3).
2. A step that needs the value reads it from `run.yaml` through
   `APB_RUN_DIR`, as on main: scripts, plain cli agents and host-mode
   subagents can; a sandboxed agent cannot, and a `success_check` is not
   told where it is (design D5). Every unsandboxed step runs as the
   operator and can read every unpruned run's `run.yaml` in the project;
   `isolation: full` keeps a step away from other runs' files. The file's path, its `params` key (the
   run's resolved values: given, recalled or defaulted), its quoting and
   its mode become a documented contract. No delivery channel is added.
3. A param once declared secret in a run stays secret for that run and
   for every version a patch creates from it: an in-run or forward patch
   that drops the declaration is refused (`secret_declaration_dropped`),
   and every check judges by the run's full secret set (design D11). An
   in-run patch records new secret names only after its snapshot is
   written, so a refused or interrupted patch never makes a valid run
   unresumable; a forward patch rechecks the source run's set under the
   candidate lock it already holds, right before it writes its
   candidate (no new lock). Every check outside validation judges every
   string of the definition, the fields V104 and V210 own at validation
   and `previous_success.when_none` included.
4. A run whose definition renders a secret is refused at resume
   (`secret_param_rendered`), in the synchronous part of every resume
   entry, so the person who asked sees it, before anything is written
   (the legacy manifest of a schema-1 run included, and on the CLI, MCP
   and the dashboard no consent either: each runs the read-only check
   before it records a consent, so nothing is written when the snapshot
   it reads renders a secret; when another process changes the snapshot
   between that check and the engine's own, the engine still refuses and
   a consent may have been recorded); a stored version
   that does is
   refused at start by the validation preparation already runs, and a
   waiting candidate that does is skipped for `current` (design D7).
5. The scenario capture takes the run's declared secret set from the
   manifest and the snapshot, so a declaration dropped before this change
   still keeps the value out (design D6).
6. The run's secret set is read strictly: a manifest or snapshot that
   does not read is a refusal (`secret_set_unreadable`) on every caller,
   never an empty set (design D11).
7. The rule is enforced by this apb and later ones. An older apb never
   had it, so an older binary that resumes or patches a run can render a
   secret param exactly as it could before; mixing binaries on one run is
   outside the guarantee (design D7 "Older binaries").

Capability `secret-param-storage`.

### Out of scope

1. Removing the value from `run.yaml`: every resume and every script
   reads it there.
2. What a step prints or writes after it read the value itself: that is
   the step's output, stored as on main and redacted on the way out as on
   main (design D8).
3. A store-time scrub of any kind; the short value warning stays a
   warning (design D9).
4. A sub-playbook param binding (none exists on main) and an environment
   hand-over to agents (design "Future work").
5. A version bump. The release notes lines go into the next release.

### Impact

1. Crates: `apb-core` (V340 and `secret_param_refs` in
   `validate/params.rs`), `apb-engine` (the run's secret set and the one
   check in `run_redact.rs`, the resume checks in `scheduler/resume.rs`,
   the patch refusals in `scheduler/patch.rs` and `forward_patch.rs`, the
   candidate skip in `gate/mod.rs`, the declared set in
   `scenario/capture.rs`). In the `apb` CLI, `apb-mcp` and `apb-server`,
   one call each: the resume surface runs the engine's read-only secret
   preflight before it records a consent; the refusals reach every
   surface through the mappings it already has.
2. Journal: no new event, event field, run guard name or manifest mark.
   A refused patch is the existing `patch_rejected`. A run with or
   without a secret param writes exactly the journal it wrote before, by
   construction (design D8).
3. Breaking for authors: a playbook that renders a secret param is
   refused at validate, save and start, and its in-flight runs at resume;
   a patch may no longer declassify a param a run holds secret. No
   shipped playbook or example does either (design D7).
4. Validator: V340 (error); V341 to V349 reserved (design D0).
5. Size: 26 changed files, 27 with the release notes (design "Files";
   the first design listed 42).

## Design

Every decision below names its files, its
functions and the line where the code is on main efae5822 (Wait park,
#290; apb 1.0.0-beta.7). Every line was re-read on efae5822 for this
document; the implementer re-verifies each on the main they start from,
finds a site by the function or constant named next to it (never by the
number alone) and notes every moved site in the final report. A site
whose function no longer exists is a stop: report it, do not invent one.

Nothing in this document is left to the implementer: where a choice was
open, it is decided here with its reason; the task list only orders the
work.

### Why the first design was replaced

The first design of this change let a secret param be rendered into any
template and then scrubbed the value out of every file apb writes under
the run directory: 31 write sites, nine fixes, a reversed routing rule.
Its design review (2026-11-20, one blocker, ten major, ten minor; the
dispositions are in "Review record" below) showed that this does not
converge. Every pass found another write site (the instruction header of
`context.md`, commit subjects, the scratch directory of a node without a
sandbox, files a step writes under `$APB_RUN_DIR`), another encoding
(percent-encoding in a URL, other JSON encoders' escapes, a value printed
as a number), another crash window (a killed driver never runs a
post-attempt scrub), another store (nested session stores, list items
sharing one agent home), and the scrub changed runs that have no secret
at all (token-shaped text and connector values replaced in a plain run's
journal, a baseline comparing markers). A scrub applied after a value was
rendered into text has to know every place that text can go, and that
set keeps growing.

### The principle

apb never renders a secret param's value into text. What is never
rendered needs no scrub.

1. The validator refuses every template reference to a param declared
   `secret: true`, at every template site (D2, D3).
2. A step that needs the value reads it from the run's private
   `run.yaml` itself (D5). Nothing new is delivered.
3. apb keeps the value only in `run.yaml`, and no process apb starts
   carries it on its argv (D6). What a step does with
   the value after it read it is the step's output (D8).

### Unchanged from main

1. `run.yaml` keeps the value (`RunConfig.params`,
   `crates/apb-engine/src/run_config.rs` line 156, written 0600 by
   `write_run_config`, line 333), and every resume reads it there.
2. The run redactor (`crates/apb-engine/src/run_redact.rs`) and every
   surface that redacts on the way out (MCP answers, the dashboard, `apb
   runs`, the HTML report, notices, grades, known problems, diagnostics).
3. `apb_core::scrub::Scrubber`, its marker and `MIN_SECRET_LEN` (6 bytes,
   `crates/apb-core/src/scrub.rs` line 45).
4. The short secret warning at start (`short_secret_warning`,
   `crates/apb-engine/src/gate/params.rs` lines 95 to 124), see D9.
5. Every write site, every event, event field, manifest field, signed
   store and routing rule. This change adds no store-time scrub anywhere,
   and no event, event field, run guard name or manifest mark: a run with
   or without a secret param writes the journal it wrote before (D7
   "Older binaries" states what that leaves to an older apb).

### D0. Validator codes

| Code | Severity | Rule |
|---|---|---|
| V340 | error | a string of the definition references a param declared `secret: true` |

V341 to V349 stay reserved for this family. Checked on efae5822: `git grep
-ohE '"V[0-9]+"' crates | tr -d '"V' | sort -n | uniq` ends at 250, and
`git grep -nE '"V3[0-9][0-9]"' crates` finds nothing. The blocks the other
changes take or reserve, read from their documents on 2026-11-20:

| Change | Takes | Reserves |
|---|---|---|
| people-in-the-loop, before-running-alone (merged) | V240 to V247, V249, V250 | (V248 unused) |
| start-rules | V260 to V267 | V268 to V279 |
| eval-doubles | V280, V281 | V282, V287 to V299 for its family |
| incremental-lists-and-session-fork | V283 | none |
| inbox-waits | V284 to V286 | V287 to V299 (shared with eval-doubles' family) |
| mcp-tool-connectors, mcp-http-and-surfaces | V300 to V302 | V303 to V319 |
| one-point-oh-fixes | V320 | V321 to V339 for its family |
| one-point-oh-freeze | none | V321 to V339 named as that family's |
| secret-params-in-run-files (this change) | V340 | V341 to V349 |

V340 lives in the param family module
`crates/apb-core/src/validate/params.rs` (`check_params`, line 11,
called from `validate()` at `validate/mod.rs` line 438), beside V99, V100
and V160.

### D1. What is already refused

1. V99 (`validate/params.rs` lines 13 to 22): a secret param with a
   `default`. V100 (lines 23 to 31): a secret param that is not `type:
   text`. So a secret's value comes only from the start's input, never
   from the definition.
2. V181 (`validate/untrusted.rs` lines 19 to 30): a param both secret and
   untrusted.
3. V160 (`validate/params.rs` lines 92 to 103): a secret param marked
   `search: true`.
4. Three template fields already refuse a secret reference, each with
   its own code: `quality.task_type` (V104, `validate/quality.rs` lines
   117 to 140), `previous_success.when_none.*` (V176,
   `validate/previous_success.rs` lines 92 to 115) and `concurrency.key`
   (V210, `validate/concurrency.rs` lines 26 to 45).
5. Three template fields accept any declared param, secret ones
   included: the connector args and `complete_when` of `baseline_check`
   and `follow_up` (V65, `validate/baseline.rs` lines 262 to 290), the
   item check templates of a list (V87, `validate/list.rs` lines 325 to
   350) and a publish `branch` (V117, `validate/publish.rs` lines 98 to
   108). Every other template site (D3) accepts any `params.<name>` the
   playbook declares (V13, `validate/templates.rs` line 28).

V340 builds on these: it refuses what 5 accepts and what V13 accepts for
a secret param, and leaves the three fields of 4 to their own codes.

### D2. The forms a template engine accepts for a param

apb has two renderers that read `params.*`, and one template syntax that
does not:

1. `apb_engine::context::render` (`crates/apb-engine/src/context.rs`
   line 597): finds `{{`, the next `}}`, trims the text between with
   `str::trim` (Unicode whitespace, newlines included) and resolves it in
   `resolve` (line 731): `["params", name]` after `split('.')`. Every
   prompt, judge state field, review field default and `show` text,
   `workdir`, `writes`, `cache.key`, `worktree`, publish `branch`,
   sub-playbook `instruction`, task type, concurrency key, `when_none`
   time and baseline or follow-up connector argument goes through it
   (callers: `judge_eval/inputs.rs` line 156 behind
   `scheduler/node.rs` `render_node_prompt`, line 25; `scheduler/node.rs`
   lines 4194 and 4892; `scheduler/worktree.rs` line 85;
   `scheduler/prepare.rs` line 86; `baseline/read.rs` line 112;
   `concurrency.rs` line 504; `previous_success.rs` line 180).
2. The list item check's `render_text`
   (`crates/apb-engine/src/scheduler/list/check.rs` lines 371 to 400):
   the same `{{`, `}}` and trim, then `split_once('.')`, so
   `("params", name)` takes everything after the first dot as the name.
3. `apb_core::connector::template` (the connector manifests' URLs,
   headers and bodies) knows only `args`, `account`, `auth`, `env` and
   `cmd` (`crates/apb-core/src/connector/template.rs` lines 98 to 111);
   it has no `params` namespace, and a playbook cannot reach it with a
   template.

So the forms that render a param are exactly:

1. `{{params.<name>}}`;
2. the same with any whitespace between the braces and the reference at
   either end, newlines included (`{{ params.<name> }}`);
3. in a list item check only, a name that holds a dot
   (`{{params.a.b}}` renders the param named `a.b`; apb has no param
   name grammar, only V02's duplicate check, `validate/graph.rs` line 19).

No other form renders a param: whitespace around the dot, another case
(`Params`), a whole-map `{{params}}` (empty in the first renderer, left
as written in the second) and nested braces resolve to nothing in both.
`apb_core::template::refs` (`crates/apb-core/src/template.rs` line 8)
scans exactly as both renderers do (same `{{`, next `}}`, same trim).

The matcher of V340, defined once: a reference returned by
`apb_core::template::refs` whose text, split at the first `.`, is
`params` and a name the definition declares `secret: true`. It covers
forms 1 to 3 and nothing that does not render.

### D3. V340, at every template site

The validator has no single template walk. The texts it scans are spread
over these collectors, one per field family (the list of rendered
fields, with the renderer callers of D2 as its runtime twin):

| Site | Collector |
|---|---|
| an `agent_task` or `prompt` node's `prompt`, a `finish` prompt, a `playbook` node's `instruction`, a `judge` node's `state` fields, a `human_review` field's `default` | `apb_core::schema::template_texts` (`schema.rs` line 2652) |
| a `human_review` `show` text | `schema::review_show_texts` (line 2702) |
| an `agent_task` or `script` `workdir` | `schema::workdir_texts` (line 2717) |
| a node's `cache.key` | `schema::cache_key_texts` (line 2727) |
| a templated `writes` | `validate::templates::writes_texts` (`validate/templates.rs` line 94) |
| the run `worktree` | `Playbook::worktree_template` (`schema.rs` line 478) |
| a publish node's `branch` | `validate/publish.rs` line 99 |
| `baseline_check` and `follow_up` connector args and `complete_when` | `validate/baseline.rs` `strings` and `check_templates`, lines 253 to 290 |
| a list item check's connector args, `accept_when.equals` and `.contains`, judge state | `validate/list.rs` lines 325 to 350 |
| `quality.task_type`, `previous_success.when_none.*`, `concurrency.key` | V104, V176, V210 (D1 item 4) at validation; the shared check of D11 item 2 everywhere else (item 5) |

A list kept by hand drifts when a field is added. So V340 does not walk
these collectors: it walks every string of the definition. The rule:

1. `apb_core::validate::secret_param_refs(playbook, also_secret) ->
   Result<Vec<SecretParamRef>, SecretWalkError>` (new, `validate/params.rs`,
   re-exported from `validate/mod.rs`). The secret set is the names the
   playbook declares `secret: true` united with `also_secret`, a
   `&BTreeSet<String>` the caller passes: empty for the validator, the
   run's full set where a run exists (D7, D11). If the set is empty,
   `Ok(vec![])` at once, before any walk (so a playbook without a secret
   param costs nothing and gets nothing, whatever it holds).
2. Otherwise the walk is over the YAML value tree of the definition:
   `yaml_serde::to_value(playbook)` (a `yaml_serde::Value`, whose mapping
   keys may be of any type, so a null, number, sequence or mapping key in
   a free-form block such as `supervisor.policy`, `schema.rs` line 1205,
   or a kept-as-is `DurationText::Other` and `ExpectedDuration::Invalid`
   value, lines 1046 and 1231, is walked like any other node). Every
   scalar string is checked with the matcher of D2: mapping values,
   sequence items and mapping keys alike (a key that is itself a string
   is checked as a string; a key that is a sequence or mapping is walked
   too). No JSON conversion is made: `serde_json::to_value` refuses a
   non-string key, and a check built on it would be skipped exactly where
   an author put one.
3. Fail closed. `yaml_serde::to_value` can still return an error (an
   empty enum tag, a `Serialize` impl that returns a custom error). Then
   `secret_param_refs` returns `Err(SecretWalkError)`, never an empty
   list, and every caller treats it as a refusal: V340 at validate (field
   `(definition)`, "the definition could not be checked for secret param
   references: <error>"), `secret_param_rendered` at a patch, a resume and
   the gate (D7, D11). There is no `unwrap_or_default`, `if let Ok` or
   `.ok()` on this result anywhere.
4. Each hit is `SecretParamRef { node: Option<String>, field: String,
   param: String }`: `node` is the id of the node a string sits under (the
   walk enters `nodes` entry by entry), `field` the path inside the node
   or the definition, keys joined with `.` and sequence indices as `[i]`
   (for example `prompt`, `list.check.args.query`, `worktree`); a key that
   is not a string is written in the path as its YAML flow text
   (`supervisor.policy.~`), and a hit on a key itself ends its path with
   `{key}`.
5. The walk skips no path: `quality.task_type`,
   `previous_success.when_none.started_at`,
   `previous_success.when_none.finished_at` and `concurrency.key` are
   walked and reported like every other string. V104, V176 and V210
   refuse a secret reference there only when the validator runs, and the
   callers outside validation (the candidate check of D7 item 4, the
   in-run patch of D11 item 3, the forward patch of D11 item 4 and the
   resume of D7 item 5) run no validator, so a skip inside the walk
   would let a candidate, a patch version or a resumed snapshot that
   renders a secret in one of these fields through the shared check
   (and, for a candidate, then fail the whole start at preparation's V104
   instead of falling back to `current`). The duplicate is removed by
   the one validation caller instead: `check_params` drops a hit whose
   `node` is `None` and whose `field` is one of these four paths, since
   V104, V176 or V210 reports that reference already, so an author still
   gets one error per reference. `secret_param_refs` itself and
   `secret_render_refusal` (D11 item 2) never filter. Amended in the
   first fix round: V176 runs only on a definition with no other error
   (`validate/mod.rs`, inside `if r.is_valid()`), so an author would
   get no error for a `when_none` reference while another error stood.
   The two `when_none` paths are therefore not dropped: they report
   V340 whatever else fails, V176 reports no secret reference of its own,
   and `check_params` drops only `quality.task_type` and
   `concurrency.key`.
6. `check_params` calls `secret_param_refs(playbook, &BTreeSet::new())`,
   drops the hits of item 5, and reports each other hit as V340 (error)
   on `node`: "`<field>` references
   secret param `<param>`; apb never renders a secret param into text.
   Read it in a script from run.yaml (`$APB_RUN_DIR`), use a connector
   account secret, or drop `secret: true` if it is not a secret", and an
   `Err` as the one V340 of item 3.

Why every string and not only the rendered fields: a field added later
that renders a template is covered without anyone editing V340, so the
rule is complete by construction. The price is that a text apb shows
literally (a `human_review` or `publish` `prompt`, a param `label`, an
edge's `output_match` pattern) is refused too when it holds a secret
reference. That is a false refusal, never a leak, and no definition needs
a secret reference in a text nobody renders. No templated field of the
schema is `#[serde(skip)]` (checked in task 0), so the serialized tree
holds every string the renderers read.

The renderers stay as they are: V340 keeps every secret reference out of
every definition a run can execute (D7), so a renderer never meets one.

### D4. Sub-playbooks: no pass-through exists

The brief for this redesign allowed one exact pass-through: a
sub-playbook param binding that is exactly the reference, to a child
param that is itself secret. The code has no such binding. A `playbook`
node has three fields, `playbook`, `instruction` and `output`
(`NodeKind::Playbook`, `schema.rs` lines 2190 to 2208); the child run is
started with the node's rendered instruction and no params
(`child_run_options`, `scheduler/node.rs` lines 4735 to 4757, leaves
`params` at its default), and preparation documents it: sub-playbook
children "arrive here with an empty params map" (`scheduler/prepare.rs`
lines 1023 to 1030). A child's secret param is therefore empty in every
run on main, and its `run.yaml` holds no parent value.

So this change allows no pass-through and adds no binding. The only way
a parent's secret reached a child on main was the node's `instruction`
template, which V340 now refuses like every template. A binding that
copies a parent's secret into a child's secret param is future work.

### D5. How a step gets the value

Through the run's `run.yaml`, as on main. Per kind of step:

1. A script node, a goal script and a baseline or follow-up script
   source get `APB_RUN_DIR` (`script::run_env`,
   `crates/apb-engine/src/script.rs` lines 80 to 91; callers
   `scheduler/node.rs` line 3941, `scheduler/goal.rs` line 164,
   `baseline/read.rs` line 285). They run unsandboxed as the operator, so
   they read `$APB_RUN_DIR/run.yaml` (mode 0600, the operator's file).
2. A `success_check` is not handed `APB_RUN_DIR`: `run_check`
   (`script.rs` lines 184 to 191) calls `run_script` with no extra
   environment, or runs it inside the attempt's sandbox. Without a
   sandbox it runs as the operator, with the attempt's working directory
   as its current directory, and can read the project's files, every
   `.apb/runs/*/run.yaml` included; it is only not told which run is its
   own. Inside a sandbox it sees what the sandboxed agent sees (item 4).
   As on main; this change does not rely on either.
3. An agent step in plain cli mode (`isolation: none`, or `best_effort`
   where no sandbox is built): yes. Every spawn sets `APB_RUN_DIR`
   (`adapter/spawn_env.rs` lines 197 to 203) and the agent runs as the
   operator, so it can read `run.yaml`.
4. A sandboxed agent step (`isolation: full`, or `best_effort` with a
   sandbox): no. The sandbox covers the tree's `.apb` with a tmpfs and
   binds back only the attempt's own run paths: its agent home, its
   attempt box, its skill copies and the run files the agent needs
   (`sandbox/view.rs` header step 6; `sandbox::build`,
   `sandbox/mod.rs` lines 206 to 216). `run.yaml` is not among them.
5. Host mode: yes, at the host's choice. The host's subagent gets
   `APB_RUN_DIR` with its task (`task_env`, `scheduler/host.rs` lines 121
   to 140) and runs as the operator. The host started the run and passed
   the params itself (`ExecutionRequest.host_session`,
   `crates/apb-core/src/execution.rs` lines 102 to 109), so it holds the
   value already.
6. A connector call uses its account secrets (`{{account.*}}` in the
   manifest, resolved by the connector layer), as on main; a playbook
   cannot hand it a secret param (D3).

This change adds no delivery channel. Handing a secret param to an agent
in its environment, inside or outside a sandbox, is future work.

A stated boundary, not a mechanism of this change: every unsandboxed
step (items 1, 2, 3 and 5) runs as the operator, so it can read the
`run.yaml` of every run in the project that is not yet pruned (its
parent's, a sibling's, an unrelated earlier run's), not only its own.
`isolation: full` is what keeps a step away from other runs' files
(item 4).

#### The run.yaml contract for steps

Reading `run.yaml` becomes the documented way to get a secret param, so
its place is a contract, stated in `docs/HOWTO-authoring.md` and pinned
by a test (D10 test 8):

1. Path: `$APB_RUN_DIR/run.yaml`, where `APB_RUN_DIR` is the run
   directory every script, goal script, baseline or follow-up script
   source and agent spawn gets (D5 items 1, 3 and 5).
2. Shape: one YAML document whose top-level mapping has the key
   `params`, a mapping from each param name to the run's resolved value,
   a YAML string (`RunConfig.params`, a `BTreeMap<String, String>`,
   `run_config.rs` lines 155 to 156). The resolved values are the ones
   preparation settles before it writes the file
   (`scheduler/prepare.rs` lines 1040 to 1050, stored at line 1410): the
   value the start was given, else a value recalled from the project's
   remembered input (`run_memory::apply`, never for a secret param, which
   `run_params::sensitive` keeps out of memory), else the param's
   `default` (`default_text`, never for a secret param, V99). A param
   with none of the three is absent. Every other key of the file is apb's
   own and not part of the contract.
3. Quoting: the file is written by `yaml_serde::to_string`
   (`write_run_config`, `run_config.rs` lines 333 to 337), which quotes a
   value whenever YAML needs it (a value that reads as a number, a
   boolean or null, one that holds `: ` or ` #`, starts with a special
   character, or spans lines). A step must parse the file as YAML and
   read `params.<name>`; cutting lines with a text tool works only for a
   value YAML leaves plain.
4. Mode 0600, owned by the operator: written through
   `fsutil::atomic_write_private` (`fsutil.rs` line 742).
5. Lifetime: written at preparation before the run's first node
   (`prepare.rs` line 1446), written again with the same params when a
   resume commits its launch record (`resume_launch.rs` line 246), and
   kept until the run directory is removed (`apb runs prune`). A child
   run has its own `run.yaml` whose `params` is empty (D4).

### D6. Where apb keeps the value

After this change apb itself writes a secret param's value only to
`runs/<id>/run.yaml` (0600), and no process apb starts carries it on
its argv: the detached supervised drive (`__drive-supervised`) takes the
params from its launch handoff (`Handoff::Supervise.params`, 0600, read
once and deleted at once; the child refuses to start without it,
`--params-in-handoff`), and an eval case run takes its case params from
its private settings file (`EvalRunSettings.params`, moved into the
run's params before `run.yaml` is written, so the file holds them once).
A value a person types on `apb run --param` is on that command's own
argv while it runs. A child never gets one (D4). The places that
copy run config were checked one by one; each already keeps the value
out by the sensitive-param rule (`apb_core::run_params::sensitive` and
`redacted`, `crates/apb-core/src/run_params.rs` lines 41 to 74) or holds
no params at all:

| Copy | Where | What keeps the value out |
|---|---|---|
| the start key's request | `start_key.rs` `StartRequest::capture`, lines 146 to 152; the signed file under `.apb/start-keys/` | only a keyed MAC of the fields is stored (`StartKeyEntry.request`, line 190) |
| remembered input | `run_memory/mod.rs` line 281 | `run_params::sensitive` refuses a declared secret before storing |
| the manifest | `manifest.rs` lines 355 to 367 | only `secret_params`, the names |
| the estimate | `run_estimate/snapshot.rs` line 29 | figures only, no params |
| the HTML report | `report_html/` | reads no params (no reference in the module) |
| the launch handoff | `launch_handoff.rs` | carries the params of a detached supervised start only, 0600 under `.apb/handoff`, read once and deleted at once (at most `MAX_AGE`, 60 s) |
| an eval case's settings | `apb eval` `start_run`, `eval-settings.json` in the repetition's scratch directory | 0600 (`atomic_write_private`); the case file holds the same params already |
| a run listing, `run_status` params | `scheduler/listing.rs` line 187, `run_redact.rs` line 678 | `run_params::sensitive` and `redacted` over the run's declared set |
| a judge example | `judge_eval/example.rs` lines 285 to 300 and 408 to 418 | the run redactor, then `sandbox::without_secrets` (placeholder) |
| a scenario capture | `scenario/capture.rs` lines 505 to 520 and 991 to 1007 | `run_params::sensitive(p.secret, ...)`, see below |
| a host task | `host_task.rs` `write_task`, lines 350 to 364; `TaskRecord`, lines 130 to 166 | no params in `task.json`; `prompt.md` is a rendered prompt, which can no longer hold the value (D3) |
| a child's `run.yaml` | `scheduler/prepare.rs` lines 1023 to 1030 | a child gets no params (D4) |

One gap, fixed here by not copying: the scenario capture judges a param
secret only by the current definition (`p.secret`). A param declared
secret earlier in the run and dropped by a patch stays secret for the
run (the manifest's `secret_params`, kept by
`run_redact::keep_secret_declarations`), but the capture would copy its
value into the scenario's params (its scrubber replaces it only when the
value is at least 6 bytes). The capture takes the declared set from
`run_redact::secret_names(run_dir)` (`run_redact.rs` line 77: the
manifest's names and the snapshot's declarations) instead, at both
sites.

### D7. Compatibility

This is a breaking validator rule for a playbook that renders a secret
param today. Such a playbook gets:

1. At validate (`apb validate`, MCP `playbook_validate`, the dashboard
   editor's draft validation): V340, one error per reference, naming the
   node and the field.
2. At save: refused with the same errors. Every version write validates
   (`versioning::validate_playbook`, `versioning/mod.rs` line 1600, from
   `create_version_impl` line 228 and `create_patch_version_with` line
   799; `validate_in`, line 1412, from `create_draft_in_by` line 1270 and
   `write_version_in` line 1494). A supervisor's in-run or forward patch
   writes a version the same way, so it is refused too.
3. At run start of a version stored before this change: the run gate
   (`gate::check_run`) does not validate, but preparation does, for every
   start (a top-level run, a trial, an eval case, a scenario run, a
   child): `validate` at `scheduler/prepare.rs` line 860 and the refusal
   "playbook `<id>` is invalid: V340 ..." at lines 861 to 871, before the
   run directory is allocated, so no run is left behind. Every launch
   surface shows that error as it shows any validator error at start.
4. At start, a waiting candidate: when `current` is trusted the gate
   admits a waiting candidate after `check_run_loaded`, which does not
   validate (`gate/mod.rs` lines 785 to 808), and preparation would then
   refuse the whole start with V340 (item 3) instead of falling back to
   `current`. So the candidate branch runs the check of D11 on the
   candidate first, with `also_secret` the declarations of `current`
   (`loaded`): a hit, a walk error or a declaration the candidate drops
   sets `candidate_skipped` to `refused: secret_param_rendered` (or
   `refused: secret_declaration_dropped`), the form `skip_reason` gives
   (`gate/mod.rs` line 1099), and the start continues with `current`, as
   it already does for an untrusted candidate (line 811). The start
   reports the skip as it reports every `candidate_skipped`.
5. At resume of a run started before this change whose definition holds
   such a reference: preparation does not run on a resume, so the resume
   path checks it, in the early, synchronous part of every resume entry,
   on the parsed definition the drive will execute, before anything is
   held or written:
   1. `resume_detached_inner` (`scheduler/resume.rs` line 498), right
      after the snapshot is parsed (line 542) and before the busy-lock
      check and the spawn. Every detached resume goes through it: MCP
      `run_resume`, the dashboard, `apb resume --detach`, and the engine
      resumes of `pause::waker::try_engine_resume` (`pause/waker.rs` line
      88), which serve the pause, release and deadline wakers and `apb
      resume --all` per run.
   2. `resume_inner_checked` (`resume.rs` line 1074), on the `playbook`
      value parsed at line 1127 (the one handed to `drive`). On main the
      legacy shim writes an ephemeral manifest for a schema-1 snapshot
      without one (`legacy_snapshot::build_ephemeral_manifest` and
      `manifest::write`, lines 1121 to 1126) before that parse, so the
      parse moves above the shim: `parse_snapshot_playbook(&yaml)` (the
      shared parser, which already tolerates the schema-1 executors) runs
      first, then the check, then the shim's manifest write, unchanged.
      `secret_names_strict` on a run with no manifest takes the
      snapshot's declarations (D11 item 1), so the check needs nothing
      the shim writes. A refused schema-1 resume therefore leaves no
      `manifest.yaml` behind. The check runs before the journal is
      opened (line 1131), the bytes are
      stored (`run_definitions::on_resume`, line 1137), drift events are
      appended (line 1142), the concurrency slot is reserved
      (`reserve_resume`, line 1225) and the tree lock is taken (line
      1226). The in-process `apb resume` and the detached driver itself
      (which repeats the check its parent made) go through it.
   3. Before a surface records a consent. Three surfaces write into the
      run before they call the engine's resume: they record the
      irreversible-effects consent (`gate::record_resume_consent`,
      `gate/mod.rs` line 307), which for a schema-1 run with no manifest
      first builds and writes the ephemeral legacy manifest (lines 320 to
      329) and then rewrites the manifest's consent (line 335); the need
      check they call first (`resume_consent_need`, line 236) itself
      clears a recorded consent of a run this machine did not create
      (`replace_consent(.., None)`, line 283). Those
      surfaces are the CLI's `apb resume` (`apb-cli/src/run.rs`, the
      `resume_consent_need` match at line 2759 and the
      `record_resume_consent` call at line 2766), MCP `run_resume`
      (`apb-mcp/src/server/run.rs`, `check_resume_consent` at line 1344)
      and the dashboard's resume route (`apb-server/src/routes/runs.rs`,
      `check_resume_consent` at line 1237). Each calls the read-only
      preflight `apb_engine::scheduler::resume_secret_preflight(root,
      run_id) -> Result<(), EngineError>` (new, `scheduler/resume.rs`,
      public beside `resume_detached`) first, before its consent step,
      and maps an `Err` exactly as it maps the engine resume's error
      (the CLI's "resume failed: ..." with exit 2, MCP's
      `ToolError::Conflict`, HTTP 409). The preflight first checks the
      run id with the same safe-segment check as the resume entries
      (`apb_core::registry::is_safe_segment`, as `resume_detached_inner`
      does at `resume.rs` line 513 and `resume_inner_checked` at line
      1090) and answers `EngineError::NotFound("run `<id>`")` for an
      unsafe id before it joins the id to `.apb/runs`, and answers the
      same for a run directory that does not exist (lines 516 to 519 and
      1093 to 1096 there); every surface already maps `NotFound`. It
      then reads `playbook.yaml`, parses it with
      `parse_snapshot_playbook` (a read or parse error is returned as the
      resume entry returns it for the same call), reads
      `secret_names_strict` and runs `secret_render_refusal`, the same
      steps and the same sentence as items 1 and 2 (which call it too,
      on the value they already parsed, through one inner function that
      takes the parsed playbook); it writes nothing, opens no journal
      and takes no lock.
      What this guarantees, exactly: when the snapshot and the run's
      secret set the preflight reads refuse the resume, the surface
      answers the refusal before any consent step, so nothing is
      written: no consent recorded or cleared, no legacy manifest. When
      the snapshot changes between the preflight and the engine's own
      check (another process patched the run in between, so the
      preflight passed on the old snapshot), the engine's check of items
      1 and 2 still refuses the resume and nothing is journaled, but the
      consent step that ran after the passing preflight may already have
      recorded a consent (or cleared one, or written the legacy
      manifest). The no-write guarantee does not cover that case,
      because the consent store has no lock shared with patch writes:
      `record_resume_consent` and `replace_consent` rewrite the manifest
      through `manifest::amend` (`manifest.rs` lines 813 to 826 and from
      line 860: a read, a change, a write, no lock), and a patch replaces
      the snapshot through `snapshot_playbook` (`run_config.rs` line
      401, an atomic rename, no lock); serializing the two would need a
      new lock across every consent surface and every patch writer,
      which this edge does not justify. A consent left that way names
      the sources the person agreed to and changes nothing the engine
      runs: the resume stays refused.
   The check is D11's, with `also_secret` the run's full secret set
   (`run_redact::secret_names_strict`, D11 item 1). A set that cannot be
   read refuses the resume with that function's
   `EngineError::Conflict("secret_set_unreadable: ...")` (D11 item 1),
   mapped like the refusal below. A hit or a walk error refuses the
   resume with `EngineError::Conflict("secret_param_rendered:
   run `<id>` renders secret param `<param>` in `<field>`; this apb never
   renders a secret param. Stop the run and start it again from a version
   that reads the value in a script")`, the same form as
   `deadline_passed` (`run_deadline.rs` lines 264 to 269). Each surface
   maps it as it maps that refusal today: MCP `ToolError::Conflict`
   (`apb-mcp/src/tools/mod.rs` line 112), HTTP 409
   (`apb-server/src/state.rs` line 292), the CLI's "resume failed: ..."
   with exit 2 (`resume_failed`, `apb-cli/src/run.rs` lines 2832 to
   2842), `apb resume --all`'s `ResumeAllAction::Refused` line
   (`pause/resume_all.rs` lines 380 and 396), and a waker's
   `WakeOutcome::Refused` (`pause/waker.rs` line 348). Nothing is
   journaled and nothing new is held: the run stays as its pause left it,
   and the sentence tells the person to stop it, which frees what that
   pause holds (its slot, a `keep_lock` tree). `try_journal_resumed`
   (line 867) gets no check of its own: it would read `playbook.yaml` a
   second time, under the lock, after the drive's copy was parsed.
   Only this check runs, not the whole validator, so a resume is not
   refused for any other rule a newer apb added. Stop and pause still
   work. The only non-test `RunResumed` append outside
   `try_journal_resumed` is the scenario-only engine pause
   (`engine_pause`, `scheduler.rs` lines 560 to 585, taken only when
   `scenario::session::is_scenario`), inside a drive that already passed
   preparation or this check; `scheduler/wait_park.rs` line 162 is in its
   `mod tests` (line 124).

Release notes migration sentence: "A playbook that renders a secret
param into a template (`{{params.<name>}}` where the param is `secret:
true`) is now refused (V340) at validate, at save and at run start, and
a run that does so is refused at resume (`secret_param_rendered`). Read
the value in a script from `$APB_RUN_DIR/run.yaml`, give a connector the
credential as an account secret, or drop `secret: true` if the value is
not a secret."

Shipped content: no playbook under `bundled/playbooks/` and no example
under `examples/` declares a secret param (`rg -n "secret: true" bundled
examples` finds only `meta: { secret: true }` in the Paperclip adapter's
own JavaScript config, which is no playbook). None breaks.

In-repo fixtures, read one by one (`rg -l "secret: true" crates`
crossed with a `params.` reference to a name declared secret in the same
fixture). Each is decided here:

1. `crates/apb-engine/tests/suite/run_secrets_test.rs`. `MONO` (its `p2`
   prompt reads `{{params.message}}`): `p2` becomes `prompt: "second"`.
   `a_patch_that_drops_a_secret_declaration_never_drops_the_redaction`
   is split in three: (a) the same patch (drop `secret: true`) is now
   journaled `patch_rejected` with `secret_declaration_dropped`, the
   snapshot still declares `message`, the manifest still names it, and
   the drive still ends (D10 test 11a); (b) the redaction half keeps its
   meaning on a run whose `playbook.yaml` is rewritten on disk without
   the declaration (the state a patch of an older apb left): a fresh
   redactor still redacts the value in a string and in JSON, and
   `params()` still shows it redacted; (c) the child half (a child run
   with the value in a plain param redacts its parent's value) moves
   unchanged into its own test. `routing_on_an_output_that_holds_the_value_is_unchanged`
   keeps its assertion; its `prompt` node `p` becomes a script node
   (`script: print.sh, runner: sh`, the file written beside the
   version's `playbook.yaml` by `seed_definition`, which gains an
   optional script list) that prints `said <value>` read with `sed -n
   's/^  message: //p' "$APB_RUN_DIR/run.yaml"` (the test's value is a
   plain YAML scalar), so routing still reads the executor's output.
2. `crates/apb-cli/tests/suite/run_secrets_cli_test.rs`. `HOST_PLAYBOOK`
   (its `echo` prompt reads `{{params.message}}`): the prompt becomes
   "Repeat the `message` param. Read it from run.yaml in $APB_RUN_DIR."
   `a_host_task_delivers_the_value_and_no_other_surface_shows_it` is
   renamed `a_host_task_never_carries_the_value_and_no_surface_shows_its_reply`
   and reversed where it asserted delivery: the host task's `prompt` and
   `run_status`'s `pending_tasks` no longer hold the value; the task's
   run directory's `run.yaml` does (`params.message`); the rest stays
   (the host's reply echoes the value, the journal keeps it, no surface
   shows it). `mcp_read_only_cli_test.rs` uses `seed_host` and only
   asserts the value is absent, so it needs no change.
3. `crates/apb-server/tests/support/secret_runs.rs` (shared by the HTTP
   and the CLI secret tests). The parent's `child` node `instruction:
   "{{params.message}}"` becomes `instruction: "{{nodes.echo.output}}"`:
   the child gets the parent's echoed output (`said <value>`) as its own
   instruction, and its `SAY` script prints that instruction, read from
   its own `run.yaml`. (The first fix round replaced a read of the
   parent's `run.yaml` through `parent_run`: a cross-run read is a
   boundary of D5, not a channel a fixture should rely on.) The child's
   output still holds the parent's value, so every assertion of
   `run_secrets_api_test.rs` and `run_secrets_cli_test.rs` over the
   child keeps its meaning; the module comments of `secret_runs.rs` and
   `run_secrets_api_test.rs` say so. The fixture scripts read
   `params.message` under the top-level `params:` key with `awk`, as
   the guide shows it for a plain value.
4. `crates/apb-core/tests/suite/validate_untrusted_test.rs`
   `a_secret_read_by_a_writes_template_is_v180`: removed. V340 is
   reported there and V180 never is, since V179 to V182 run only on a
   definition with no error (`validate/mod.rs`, inside `if
   r.is_valid()`), and the `writes` row of the V340 table already pins
   it. The secret-param entry of `untrusted::secret_entries` could no
   longer fire (every definition `analyze` sees passed V340 or the
   shared check) and is removed with it.
5. Changed with item 4: the `untrusted.rs` unit test
   `the_secrets_entries` loses its secret-param entry. Stay unchanged:
   the `judge_eval/sandbox.rs`
   and `judge_eval/example.rs` unit fixtures (they write the version and
   the run directly into the store and never validate or prepare;
   the judge sandbox rendering a stored version without validating it is
   pre-existing, with secrets replaced by the placeholder); and the
   owned-field tests `validate_quality_test.rs` (`task_type`),
   `validate_previous_success_test.rs`
   (`a_when_none_template_reading_a_secret_param_is_v176`) and
   `validate/concurrency.rs` (`key`), which V340 skips (D3 item 5) and
   which keep asserting exactly their own code.

Older binaries. The rule is enforced by this apb and every later one.
An older apb never had it: it resumes a run without the check of item 5
and applies a patch with its own `apply_patch` (`scheduler/patch.rs`
line 34, loading the patch version at line 84), which has no D11 check.
So an older binary that resumes or patches a run, one this apb started
included, can render a secret param exactly as it could before this
change, for example by applying a patch that removes `secret: true` and
adds `{{params.<name>}}`. Mixing binaries on one run is outside the
guarantee: every apb that drives a run with a secret param should be
this release or newer, and the release notes and the authoring guide say
so. This change writes nothing that would stop an older binary: no new
event, event field, run guard name or manifest mark, and a run with or
without a secret param writes the journal it wrote before. A run guard
name was considered and cut (Review record, "Cut in the sixth pass"):
written after `run_provenance` it leaves a crash window in which an
older binary resumes the run unguarded, and closing that window needs a
marker written before `run.yaml`, more machinery than this edge
deserves.

### D11. A secret stays secret for the run and its patches

A param once declared `secret: true` in a run stays secret for that run
and for every version a patch creates from it. Without this, V340 alone
can be undone: on main a supervisor's patch may drop `secret: true` and
add `{{params.<name>}}` in the same YAML; the patch version validates
alone (`create_patch_version_with`, `versioning/mod.rs` line 756,
`validate_playbook` at line 799), `apply_patch` loads it
(`scheduler/patch.rs` line 84), records the old declaration in the
manifest (`keep_secret_declarations`, line 237, `run_redact.rs` lines 60
to 73) and replaces the snapshot (`snapshot_playbook`, line 269), and the
next attempt renders the value. A forward patch may change `params`
freely (`forward_patch::definition_guard`, `forward_patch.rs` lines 323
to 342, over `protected_fields`, lines 254 to 306, which never name
`params`).

1. The run's secret set. `run_redact::secret_names_strict(run_dir) ->
   Result<BTreeSet<String>, EngineError>` (new, beside `secret_names`,
   `run_redact.rs` line 77): the manifest's `secret_params` united with
   the snapshot's declarations, as `secret_names` gives them, except that
   both reads are strict. On main both are lenient: `secret_names` turns
   a manifest read error into an empty list (`.ok().flatten()`, lines 78
   to 82) and `snapshot_declarations` turns a snapshot read or parse
   error into an empty list (`.ok()`, `.ok()`, `.unwrap_or_default()`,
   lines 102 to 112), so after a crash between a patch's snapshot
   replacement and its manifest update (item 3 write 2) a failed snapshot
   read would drop exactly the names only the snapshot holds. In
   `secret_names_strict` a manifest that exists and does not read, and a
   snapshot (`playbook.yaml`) that is missing, does not read or does not
   parse as its `params` list, are each an error, never an empty set; a
   run with no manifest (an older apb's) gives its snapshot's
   declarations. The error is one refusal,
   `EngineError::Conflict("secret_set_unreadable: run `<id>`: <file>
   does not read (<cause>); the run's secret params cannot be known, so
   this apb refuses rather than judge by part of them")`, with `<file>`
   `manifest.yaml` or `playbook.yaml`. Every caller of
   `secret_names_strict` answers it as its own refusal:
   1. the in-run patch (item 3): journaled
      `append_patch_rejected(log, "secret_set_unreadable: <sentence>")`,
      the snapshot not replaced and the manifest not written, as for
      every refused patch;
   2. the forward patch (item 4): `refused("secret_set_unreadable",
      <sentence>)`, no candidate written;
   3. the resume, both entries (D7 items 5.1 and 5.2): the
      `EngineError::Conflict` returned synchronously, before anything is
      journaled, reserved or locked, mapped by every surface as it maps
      `secret_param_rendered`. A check that already runs earlier in the
      entry and reads the same file (in `resume_detached_inner` the
      launch, drift, consent and budget checks before the parse at line
      542; in either entry the snapshot read itself) may refuse first
      with its own error; either way the resume is refused with nothing
      written;
   4. the consent preflight (D7 item 5.3): the same `Conflict`, returned
      before any consent step and mapped as the engine resume's error.
   The candidate selection at the gate (D7 item 4) reads no run
   directory: it judges the candidate against the declarations of
   `current`, a version the gate has already loaded and parsed
   (`loaded`, `gate/mod.rs` from line 785), so it never calls
   `secret_names_strict` and has no such refusal.
   `secret_names` keeps its lenient form for the scenario capture (D6),
   which only reads.
2. One check, `run_redact::secret_render_refusal(run_secret,
   playbook) -> Option<(code, sentence)>` (new): first every name of
   `run_secret` that `playbook` still has as a param without `secret:
   true`, or no longer has as a param while any of its strings references
   `params.<name>` in a form of D2, gives `secret_declaration_dropped`
   ("param `<name>` is secret in this run; a patch may not remove `secret:
   true` from it, nor remove it while `<field>` still references it");
   then `apb_core::validate::secret_param_refs(playbook, run_secret)`: a
   hit or an `Err` gives `secret_param_rendered` (the sentence of D7 item
   5, or of the walk error). The resume (D7 item 5), the gate (D7 item 4),
   `apply_patch` and the forward patch call this one function, so the
   four agree.
3. In-run patch (`apply_patch`, `scheduler/patch.rs` line 34): after the
   version is loaded (line 84) and beside the existing refusals (the
   deadline's at line 123, the approval's at line 130), the check of item
   2 runs on `loaded.playbook` with `run_secret` =
   `secret_names_strict(run_dir)` united with the declarations of the
   running snapshot (`playbook`). A refusal is journaled as every patch
   refusal is, `append_patch_rejected(log, "<code>: <sentence>")`
   (`patch.rs` line 26), and the snapshot is not replaced. This also
   covers a `Control::Patch` that names a version stored before this
   change or edited on disk after it was saved: the loaded version is
   checked whatever wrote it. The patch version itself was already
   stored by `playbook_patch` (`apb-mcp/src/tools/supervisor.rs` line
   299); like every other refused patch it stays in the store, never
   applied to this run and never `current`.

   The order of the manifest writes. On main `keep_secret_declarations`
   (line 237) records the declarations of both the running and the
   patched definition before the launch policy refusal (`LOOSENED`, lines
   240 to 252) and before the snapshot is replaced (`snapshot_playbook`,
   line 269). Kept that way, a patch that marks a plain param `token`
   secret and loosens `agent_launch` would be refused at line 246 with
   the manifest already naming `token`, while the unchanged snapshot
   calls it plain; a crash between the manifest write and the snapshot
   write would leave the same state. The resume check would then refuse
   `secret_declaration_dropped` for a definition no patch ever changed.
   So the writes are split:
   1. Before the snapshot is replaced, only the running definition's
      declarations are recorded: the call at line 237 becomes
      `keep_secret_declarations(run_dir, &[playbook])`. Those names are
      already declared secret by the snapshot in force, so this write
      can never make the manifest call a param secret that the executing
      snapshot calls plain, whatever refusal or crash follows.
   2. The names the patched definition newly declares secret are
      recorded after the new snapshot is durable:
      `keep_secret_declarations(run_dir, &[&loaded.playbook])` right
      after `snapshot_playbook` (line 269; `run_config.rs` line 401, a
      temp file renamed over `playbook.yaml` by `atomic_write`) returned
      and before `PatchApplied` is journaled. The doc comment of
      `keep_secret_declarations` (`run_redact.rs` lines 55 to 59, "a
      current-run patch calls this before it writes the new snapshot")
      is rewritten to say both calls and their order.
   3. A crash between the two leaves a snapshot that declares the new
      names and a manifest that does not name them yet. Every reader
      takes the union of the manifest and the snapshot
      (`secret_names_strict`, `secret_names`), so the names count as
      secret from the moment the snapshot holds them, and the resume
      check passes, because the snapshot it judges still declares them.
   4. A patch refused at any later step (the launch policy, the
      snapshot write failing) leaves the manifest with the running
      definition's names only, as before the patch.
4. Forward patch (`forward_patch::create`, `forward_patch.rs` line 71).
   On main it reads the source run (`read_all`, line 97) and the base
   version before it takes the candidate lock
   (`apb_core::candidate::lock`, line 113), which it already holds
   across `check_base` (line 114), `check_patch_budget` (line 115) and
   the candidate write (`create_forward_patch`, line 116,
   `apb-core/src/candidate/mod.rs` line 130). A check made before the
   lock could be stale: an in-run patch of the source run could record a
   new secret name (item 3 write 2) between that check and the write.
   So the check is a recheck under the lock the forward patch already
   holds, with no new lock and no longer hold:
   1. `forward_patch::create` runs the check of item 2 under its
      candidate lock, after `check_patch_budget` and immediately before
      `create_forward_patch`, on the patched definition with
      `run_secret` = `secret_names_strict` of the source run, read there
      (not the events or any set read before the lock), united with the
      base version's declarations. A refusal is `refused("<code>",
      <sentence>)` (line 66), the form of every forward patch refusal,
      and no candidate is written. A forward patch can therefore neither
      declassify a param the run held secret when its candidate is
      written nor render one.
   2. `apply_patch` takes no new lock. Its ordering fix of item 3 stays:
      the running definition's names before the snapshot, the patched
      definition's new names after it.
   3. The interleaving that remains, accepted: an in-run patch of the
      source run that declares a param secret after the forward patch's
      recheck and before its candidate write lands is not seen by that
      candidate. This is accepted because the candidate was checked
      against the run as it stood when written; a candidate is its own
      version, judged by its own declarations by V340 at every start
      that takes it (D7 items 3 and 4) and by the shared check against
      `current` at the gate, so a param it declares plain and renders is
      one it never received as secret from that run's values; and the
      run that declared the secret keeps it through its own set (item
      1), which no candidate changes. Making the
      in-run patch hold the candidate lock across its checks and writes
      was cut in the fifth pass (Review record): it put the unattended
      probation and readiness walks inside a lock whose 60-second stale
      rule lets another caller take it over, and it added a lock order
      every caller had to keep.
5. Every other start keeps its own definition's set: a fresh start's run
   set is the definition's declarations, which preparation's V340
   already uses (D7 item 3). A waiting candidate is judged against
   `current`'s declarations (D7 item 4), so a candidate written before
   this change that dropped a declaration is skipped, never taken.

### D8. Stated limits

1. What a step prints or writes after it read the value itself is the
   step's output: an agent that reads `run.yaml` and echoes the value, a
   script that prints it, a file a step writes under `$APB_RUN_DIR`. apb
   stores that output as on main. The redactor every surface applies to
   text that leaves the machine is unchanged and still replaces the
   run's secret values (6 bytes and longer) there.
2. This change adds no store-time scrub and changes no run that has no
   secret param. A run whose definition declares no secret param writes
   exactly the journal it wrote before, and this holds by construction:
   no write site, renderer, event or routing rule changes; V340 and the
   checks of D11 only read the definition and return nothing for a
   playbook without a secret param (D3 item 1, D10 test 14). A run that
   declares a secret param, referenced or not, also writes the journal
   it wrote before: no guard name or other mark is added for it; a
   definition that passes V340 holds no secret reference, so every text
   the renderers produce from it is the text main produced.
3. A run started before this change keeps what it already wrote; nothing
   is rewritten in place.
4. Text a person or a supervisor types (an instruction, a review note,
   an answer) is theirs; apb renders no param into it.
5. An author who writes the value itself into the definition writes it
   as a literal, not as a param.

### D9. Short values

A declared secret shorter than `MIN_SECRET_LEN` stays a warning at
start, as on main (`short_secret_warning`, `gate/params.rs` lines 95 to
124, over `run_params::short_secret_warning`). Nothing in this change
depends on scrubbing: the value is never rendered, so its length matters
only to the redactor surfaces apply on the way out, which is what the
warning already says.

### D10. Tests

Written first, each named in the task list section 1.

1. V340 at every site of the D3 table: one playbook per site with a
   secret param referenced there, each reporting exactly one V340 naming
   the node and the field: an `agent_task` prompt, a `prompt` node, a
   `finish` prompt, a `playbook` node's `instruction`, a `judge` state
   field, a `human_review` field default and `show` text, an `agent_task`
   and a `script` `workdir`, a templated `writes`, a `cache.key`, the run
   `worktree`, a publish `branch`, a `baseline_check` connector argument
   and its `complete_when.equals`, a `follow_up` connector argument, a
   list item check's connector argument, `accept_when.equals`,
   `accept_when.contains` and judge state field.
2. The forms: `{{ params.s }}` with spaces, with a newline and a tab
   inside the braces, and a param named `a.b` in a list item check, each
   V340; `{{params . s}}`, `{{Params.s}}` and `{{params}}` report no
   V340 (they render nothing).
3. Literal fields: a secret reference in a `human_review` `prompt`
   reports V340 (the every-string rule); a plain param referenced
   anywhere and a secret param never referenced report none.
4. The three owned fields: at validation, a secret reference in
   `quality.task_type`, `previous_success.when_none.started_at` and
   `concurrency.key` reports only V104, V340 and V210 (the `when_none`
   path is V340's, D3 item 5 as amended), each also while another error
   stands; and
   `secret_param_refs` itself returns a hit for each (the walk skips
   nothing, D3 item 5). Outside validation, for each of the three
   fields, one case per path: (a) the candidate: a trusted `current` and
   a waiting candidate written to the store directly with that field
   reading a secret param; a plain start runs `current` with
   `candidate_skipped` = `refused: secret_param_rendered`, and the start
   is not refused at preparation; (b) the in-run patch: a
   `Control::Patch` naming a version written to the store directly with
   that field reading a param the run declares secret is journaled
   `patch_rejected` with `secret_param_rendered`, `playbook.yaml`
   byte-equal to before; (c) the resume: a paused run whose
   `playbook.yaml` is replaced by one with that field reading the secret
   is refused `secret_param_rendered` with nothing appended.
5. Sub-playbooks: a `playbook` node whose `instruction` reads a secret
   param reports V340; a child playbook that declares a secret param
   starts with an empty value and its `run.yaml` holds no parent value.
6. A stored version with the old form: a version written to the store
   directly (bypassing the save validation) whose prompt reads a secret
   param; `run` is refused with "playbook `<id>` is invalid: V340" and no
   run directory exists afterwards.
7. A pre-change run, refused where the caller sees it: a run prepared
   from a valid version, its `playbook.yaml` then replaced by one whose
   prompt reads the secret, paused. (a) `resume_detached` returns the
   `secret_param_rendered` conflict to its caller and spawns no driver
   (no `driver.pid` written by this call); (b) the in-process `resume`
   returns it too; after both, no `run_resumed` and no other event was
   appended, the concurrency slot file and the busy lock are as the pause
   left them, and `stop` still ends the run. (c) The same run whose
   snapshot declares the param no longer secret but whose manifest names
   it (the state a pre-change patch left) is refused
   `secret_declaration_dropped` (D11 item 2 runs the declaration check
   before the rendering check): the run's set comes from the manifest,
   not the snapshot alone. (d) A schema-1 run
   (a snapshot with legacy executors and no `manifest.yaml`) whose prompt
   reads a secret param: the in-process `resume` is refused
   `secret_param_rendered` and no `manifest.yaml` exists afterwards (the
   legacy shim's write comes after the check, D7 item 5). (e) The
   surfaces that record a consent first (D7 item 5.3): a run of an
   irreversible tree with no consent recorded whose prompt reads a
   secret param, resumed with the bare confirmation `true`, which on
   main records the consent at once (`Confirmation::Flag(true)`,
   `consent.rs` line 161), through the CLI (`--confirm-irreversible
   true`), MCP `run_resume` (`confirm_irreversible: true`) and the
   dashboard's resume route (`confirm_irreversible: true`): each answers
   `secret_param_rendered` through its own mapping (exit 2, a tool
   conflict, HTTP 409), and `manifest.yaml` is byte-equal to before; the
   same for a schema-1 run with no manifest, after which no
   `manifest.yaml` exists. This pins the guarantee where it holds: the
   snapshot the preflight read is the one the engine judges. (f) The
   case it does not cover, as an on-disk state (no fault seam): a run of
   an irreversible tree whose consent is recorded (the state a consent
   step leaves after a passing preflight) and whose `playbook.yaml` is
   then replaced by one whose prompt reads the secret (the patch another
   process made in between): the engine's resume is refused
   `secret_param_rendered` with nothing journaled, and the recorded
   consent is left as it was; the test asserts that the resume stays
   refused, not that the consent is absent. (g) The preflight's input
   check: `resume_secret_preflight(root, id)` with an unsafe id (`..`,
   `a/b`, an absolute path) answers `EngineError::NotFound` and reads
   nothing outside `.apb/runs`; a safe id with no run directory answers
   `NotFound` too.
8. The value stays in `run.yaml`, at the documented place: a cli run of
   a playbook with a secret param, an agent stub, a `prompt` node, a
   script node that reads the value from `$APB_RUN_DIR/run.yaml` and uses
   it without printing it, and a finish: afterwards the only file under
   the run directory whose name or content holds the value is `run.yaml`,
   mode 0600; parsed as YAML, its top-level `params.message` equals the
   value; the start key file, the memory file, the manifest,
   `estimate.json` and the report page hold no value. A second case
   writes a run config whose value YAML must quote (`"007: #a"`) and
   reads it back as the same string through `params.<name>` (the
   contract of D5). A third case pins which values `params` holds (the
   resolved run values of D5): a playbook with the secret param
   `message` (given at start), a plain param `region` with no default
   whose value the project remembers from an earlier run of the
   playbook (recalled), a plain param `tier` with a `default` (defaulted)
   and a plain param `note` with none of the three; `params` holds
   `message`, `region` and `tier` with those values, and `note` is
   absent.
9. The walk fails closed and sees every key: a playbook declaring a
   secret param whose `supervisor.policy` holds a null key and a sequence
   key with `{{params.token}}` under them, and a string key that is
   itself `{{params.token}}`, reports V340 for each (the paths
   `supervisor.policy.~` and so on); a value whose serialization returns
   an error (a unit test over the walk's inner function, generic over
   `Serialize`, given a type whose `serialize` returns a custom error)
   gives `Err`, and `check_params` turns it into V340 `(definition)`.
10. Copies of run config: a scenario capture of a run whose manifest
    names a secret param the current snapshot no longer declares secret
    leaves the param out and warns; a judge example of a run with a
    secret param holds the placeholder.
11. In-run patches (D11 item 3): a supervised run of a playbook with a
    secret param; (a) a patch whose YAML drops `secret: true` is
    journaled `patch_rejected` with `secret_declaration_dropped`, the
    snapshot still declares the param and the manifest still names it;
    (b) a patch that removes the param while a prompt references it is
    rejected alike; (c) a `Control::Patch` naming a version written to
    the store directly whose prompt references the secret is journaled
    `patch_rejected` with `secret_param_rendered`; in each case
    `playbook.yaml` is byte-equal to before. (d) The later refusal: on a
    run whose plain param `token` is referenced nowhere, a patch that
    declares `token` secret and loosens `agent_launch` is journaled
    `patch_rejected` with `launch_policy_loosened`; the manifest's
    `secret_params` does not name `token`, `playbook.yaml` is byte-equal
    to before, and a later resume of the run is not refused. (e) The
    crash boundary, as on-disk states the test writes (no fault seam in
    the code): after an applied patch that declared `token` secret, the
    manifest's `token` is removed again (the state a crash between the
    new snapshot and the second manifest write leaves); the resume check
    passes, and the run's set (`secret_names_strict`) and the run
    redactor both still count `token` secret. The state before the
    snapshot write (the old snapshot, a manifest with the running
    definition's names only) resumes as the unpatched run. (f) After an
    applied patch that declared `token` secret, the manifest names
    `token`.
12. Forward patches (D11 item 4): `forward_patch::create` from such a run
    with YAML that drops `secret: true` is refused
    `secret_declaration_dropped`, and no candidate is written. The
    recheck: a supervised run whose plain param `token` is referenced
    nowhere; the forward patch's YAML is prepared from the run as it
    started (`token` plain, a prompt rendering `{{params.token}}`); an
    in-run patch that declares `token` secret is then applied, so the
    run's set changed after the YAML was made and before the candidate
    write; `forward_patch::create` with that YAML is refused
    `secret_declaration_dropped` and writes no candidate, because the
    set is read under the candidate lock right before the write, not
    taken from the base version or an earlier read. No test holds or
    contends for the candidate lock: no lock was added (D11 item 4).
13. A waiting candidate (D7 item 4): a trusted `current` and a candidate
    written to the store directly whose prompt references a secret
    param; a plain start runs `current` with `candidate_skipped` =
    `refused: secret_param_rendered`, and a candidate that drops a
    declaration `current` holds gives `refused:
    secret_declaration_dropped`.
14. Construction: `secret_param_refs(p, &BTreeSet::new())` is `Ok` and
    empty for every bundled playbook (`bundled/playbooks/*/playbook.yaml`)
    and every playbook under `examples/`, and the check of D11 item 2 with
    an empty run set passes for each.
15. The strict secret set (D11 item 1): unit tests of
    `secret_names_strict` in `run_redact.rs` `mod tests`: the union of
    the manifest's names and the snapshot's declarations; a run with no
    manifest gives the snapshot's; a manifest that does not read, a
    missing `playbook.yaml`, a `playbook.yaml` replaced by a directory of
    that name (a read error, no fault seam) and one whose bytes do not
    parse each give `secret_set_unreadable` naming the file, never an
    empty or partial set. The crash state of D11 item 3, with a failed
    snapshot read: after an applied in-run patch that declared `token`
    secret, the manifest's `token` is removed again (the state a crash
    between the snapshot replacement and the manifest update leaves),
    then `playbook.yaml` is made unreadable (replaced by a directory) and,
    in a second case, unparsable; `forward_patch::create` with YAML that
    renders `{{params.token}}` with `token` plain is refused
    `secret_set_unreadable` and writes no candidate (on main the lenient
    read would have passed it); `resume_secret_preflight` and the
    in-process resume refuse too (at their own snapshot read, which
    comes first), with nothing journaled and no consent written. An in-run patch
    rejected `secret_set_unreadable`: a supervised run whose manifest is
    made unreadable before a `Control::Patch` is applied journals
    `patch_rejected` with `secret_set_unreadable`, and `playbook.yaml` is
    byte-equal to before.
16. No mark for a run with a secret param: a run of a playbook that
    declares a secret param journals no `run_guard`, and its journal
    carries the same sequence of event types as the same playbook
    without the declaration; its manifest carries no field main's does
    not.

The journal of a run without a secret param is not compared against a
golden file. This change adds no write site, event, event field, run
guard name or manifest field, so there is nothing for such a file to
catch, and
one recorded before the change would break on every version
bump (`run_provenance.apb`), on every timing field (`*_ms`) and on every
in-flight change that adds a field, without ever failing because of this
change. What guards the journal is the existing corpus and the
equal-journal tests, which stay green with their existing rows unchanged
and gain no row:
`crates/apb-engine/tests/suite/journal_compat_test.rs` (over
`tests/fixtures/journal/known_events.jsonl` and
`with_future_events.jsonl`), `crates/apb-cli/tests/suite/journal_corpus_test.rs`
(the journal corpus against older binaries),
`edge_route_test.rs` `diagnostics_on_and_off_give_equal_journals_and_run_states`
(line 585) and `resume_test.rs`
`a_legacy_journal_still_counts_and_resumes_to_the_same_outcome` (line
395), plus test 14 for the claim that a plain definition meets no check.

### Cut

Everything of the first design that existed only to scrub a rendered
value, with the reason:

1. Fix A, the spawn scrub: the run secrets on `SpawnEnv`, the widened
   `KeyScrub` and `capture::Redact`, the session copy scrub, the store
   pass over `agent-home/` and `sandbox/home/`, the supervisor's run
   secrets. No prompt holds the value any more, so an agent's output,
   logs, status file, session copy and stores hold it only if the agent
   read it itself (D8 item 1).
2. Fix S, the script runner scrub and its new argument on every script
   caller: a script's output is the step's output (D8 item 1).
3. Fix P, the `prompt` node scrub: a `prompt` node cannot render a
   secret (V340).
4. Fix C, the connector URL and subject scrub: no playbook template
   reaches a connector argument with a secret (V340).
5. Fix L, `intent_label_secret`: an `--intent` label is text an agent
   types, the step's output.
6. Fix B, the baseline and follow-up scrub: a baseline read cannot render
   a secret into its arguments or `complete_when` (V340); a value a source
   returns is the source's output.
7. Fix W, the write-scope and read-only evidence scrub: a `writes`
   template cannot read a secret (V340); changed paths are the step's
   output.
8. Fix H, `host_prompt_secret`, the refused host fallback and the patch
   refusal: a host prompt cannot hold the value (V340); host mode needs
   no rule of its own.
9. Fix T, `secret_too_short` as a refusal: nothing depends on scrubbing
   (D9); the warning stays.
10. `secrets_unresolved` for agent and script steps: nothing is resolved
    at spawn time any more.
11. The routing reversal (old D4: routing reads the scrubbed output) and
    its changed tests (`routing_on_an_output_that_holds_the_value_is_unchanged`
    reversed, the CLI test reversal): routing reads the executor's output,
    as on main; the test stays as it is apart from its fixture (D7).
12. `Scrubber::needles`, `Needles` and `RunRedactor::run_secrets`: no
    caller remains.
13. The narrow V340 (five path fields): replaced by the rule over every
    string.
14. The 31-row write-site table and the "every file under the run
    directory, with an echoing agent" test: the requirement no longer
    depends on knowing every write site; test 8 checks the case apb
    controls.

### Wrong implementations

1. Scrubbing a rendered text, at any site, instead of refusing the
   reference.
2. Rendering a secret reference as a marker, a placeholder, a hash or an
   empty string at run time instead of refusing the definition: it hides
   an author's mistake and moves cache keys.
3. A V340 matcher of its own (a regex, a different trim) instead of
   `apb_core::template::refs` plus the first-dot split: it would differ
   from the renderers.
4. V340 over a hand list of fields instead of every string of the
   definition.
5. Running the whole validator at resume: a resume would be refused for
   any rule a newer apb added. Only the check of D11 item 2 runs there.
6. Adding a param binding to the `playbook` node, or copying a parent's
   secret into a child, to "keep" a pass-through: there is none on main
   (D4).
7. Handing the value to an agent in its environment or in a file: a new
   delivery channel, out of scope (D5).
8. Removing the value from `run.yaml`, or narrowing its mode handling:
   every resume and every script reads it there.
9. Turning the short value warning into a refusal.
10. A new event, event field, run guard name or manifest mark for this
    rule, for a run with or without a secret param: the journal stays as
    it is, and the limit toward older binaries is stated, not guarded (D7
    "Older binaries").
11. Taking a scenario capture's declared set from the current definition
    only, or from `RunRedactor::for_run` (which resolves every connector
    secret of the workspace): `run_redact::secret_names` is the reader.
12. A walk that skips on error (`if let Ok`, `unwrap_or_default`,
    `.ok()`) or a JSON conversion of the definition (D3 items 2 and 3).
13. The resume check only inside the detached driver or under the event
    lock, or on a second read of `playbook.yaml` (D7 item 5).
14. Judging a patch, a resume or a candidate by the patched or stored
    definition's own declarations alone (D11).
15. Recording the patched definition's new secret names in the manifest
    before its snapshot is written, or both definitions' names in one
    write before the later refusals (D11 item 3): a refused or crashed
    patch would leave a run whose valid resume is refused.
16. Running the forward patch's secret check before it takes the
    candidate lock, or on a secret set read before it, or adding a lock
    to `apply_patch` (D11 item 4).
17. Writing the legacy ephemeral manifest before the resume check
    (D7 item 5).
18. Skipping `quality.task_type`, `previous_success.when_none` or
    `concurrency.key` inside `secret_param_refs` or
    `secret_render_refusal`: only `check_params` drops those hits (D3
    item 5).
19. Recording a consent, clearing one or writing the legacy manifest
    before the secret preflight on a resume surface (D7 item 5.3), or
    claiming the preflight makes the consent and the engine's check
    atomic: it does not when another process changes the snapshot in
    between (D7 item 5.3).
20. Reading the run's secret set leniently in a check (an unreadable
    manifest or snapshot taken as an empty set), or answering
    `secret_set_unreadable` with anything but the caller's own refusal
    (D11 item 1).
21. A preflight that joins a run id to `.apb/runs` before the
    safe-segment check (D7 item 5.3).

### Shared files

| File | Other change and what it does there | This change |
|---|---|---|
| `crates/apb-core/src/validate/mod.rs` | inbox-waits, mcp-tool-connectors, incremental-lists, start-rules, one-point-oh-freeze: their own `mod` lines and calls | the `secret_param_refs` re-export and the comment on the `check_params` call |
| `crates/apb-engine/src/scenario/capture.rs` | inbox-waits: the `Wait` arm (lines 353 to 377) | the declared set at lines 505 to 520 and 991 to 1007 |
| `crates/apb-engine/src/gate/mod.rs` | start-rules, eval-doubles: their own launch checks | the candidate branch (lines 785 to 811) |
| `crates/apb-engine/src/scheduler/patch.rs` | incremental-lists-and-session-fork: two refusals of its own beside the existing ones | the D11 refusal and the split manifest writes (D11 item 3) |
| `crates/apb-engine/src/forward_patch.rs` | none of the changes in flight names it | the D11 recheck under the candidate lock it already holds |
| `crates/apb-engine/src/scheduler/resume.rs` | start-rules (`is_host_mode` at the resume entry), incremental-lists-and-session-fork (reads it only) | the two resume checks and `resume_secret_preflight`, the parse moved above the legacy shim |
| `crates/apb-cli/src/run.rs` | eval-doubles, incremental-lists-and-session-fork, mcp-tool-connectors, one-point-oh-fixes, one-point-oh-freeze, start-rules: their own start, suite and output sites (the lines their designs cite are in the 200s to the 900s, none in the resume consent block) | the preflight call before `resume_consent_need` (D7 item 5.3) |
| `crates/apb-mcp/src/server/run.rs` | one-point-oh-freeze: its own site near line 335 | the preflight call before `check_resume_consent` in `run_resume` (D7 item 5.3) |
| `crates/apb-server/src/routes/runs.rs` | inbox-waits and incremental-lists-and-session-fork: `RunDetail` and their own views | the preflight call before `check_resume_consent` in the resume route (D7 item 5.3) |
| `crates/apb-engine/tests/suite/scenario_capture_test.rs` | inbox-waits: its own tests | one new test |
| `docs/HOWTO-authoring.md` | every change: other sections | the "Secret params" section and the V180 bullet |
| `CLAUDE.md`, `AGENTS.md` | every change: other module map phrases | the `params` phrase of the validate module, the `run_redact.rs` phrase |

All are neighbour edits; no line is edited by two changes. one-point-oh-fixes
shares one code file with this change again, `apb-cli/src/run.rs`, at
sites far from the resume consent block (its adapter rename, spawn envs
and tests are untouched here); it names `validate/params.rs` and
`run_secrets_test.rs` only in its review record. start-rules reads `resume_launch.rs`, which
this change does not touch. No in-flight change edits
`validate/params.rs`, `run_redact.rs`, `resume_detached_inner`,
`resume_inner_checked` or `run_secrets_test.rs`; this change does not
touch `try_journal_resumed`, `scheduler/prepare.rs` or
`validate/guarded.rs`. The implementer re-reads the three surface files
above, `gate/mod.rs` and `scheduler/patch.rs` on the main they start
from.

Merge order: after one-point-oh-fixes (it takes V320 and frees V340 by
the table in D0), before one-point-oh-freeze, which freezes the validator
and refusal codes and so must include V340, `secret_param_rendered`,
`secret_declaration_dropped` and `secret_set_unreadable`.
Against the other changes in flight it merges whenever it is ready.

### Files

Twenty-six changed files, counted the way the 1.0 plan counts them
(tests, docs and records included; the change documents count as
five):

1. the proposal
2. the design
3. the task list
4. the requirements
5. `docs/superpowers/specs/2026-11-20-secret-params-in-run-files-design.md` (new, this record)
6. `crates/apb-core/src/validate/params.rs` (V340, `secret_param_refs`, `SecretParamRef`, `SecretWalkError`)
7. `crates/apb-core/src/validate/mod.rs` (the re-export)
8. `crates/apb-core/tests/suite/validate_params_test.rs` (tests 1 to 5, 9, 14)
9. `crates/apb-core/tests/suite/validate_untrusted_test.rs` (the V340 assertion, D7 fixtures item 4)
10. `crates/apb-engine/src/run_redact.rs` (`secret_names_strict`, `secret_render_refusal`; the unit tests of test 15)
11. `crates/apb-engine/src/scheduler/resume.rs` (the two resume checks)
12. `crates/apb-engine/src/scheduler/patch.rs` (the in-run patch refusal)
13. `crates/apb-engine/src/forward_patch.rs` (the forward patch refusal)
14. `crates/apb-engine/src/gate/mod.rs` (the candidate skip)
15. `crates/apb-engine/src/scenario/capture.rs` (the declared set)
16. `crates/apb-engine/tests/suite/run_secrets_test.rs` (fixtures; tests 4 (a) to (c), 5 to 8 (7 (f) and (g) included), 11 to 13, the integration part of 15, 16)
17. `crates/apb-engine/tests/suite/scenario_capture_test.rs` (test 10)
18. `crates/apb-cli/tests/suite/run_secrets_cli_test.rs` (fixture; the CLI case of test 7 (e))
19. `crates/apb-server/tests/support/secret_runs.rs` (fixture)
20. `crates/apb-server/tests/suite/run_secrets_api_test.rs` (module comment; the MCP and dashboard cases of test 7 (e))
21. `docs/HOWTO-authoring.md`
22. `CLAUDE.md`
23. `AGENTS.md`
24. `crates/apb-cli/src/run.rs` (the resume preflight before the consent, D7 item 5.3)
25. `crates/apb-mcp/src/server/run.rs` (the same in `run_resume`)
26. `crates/apb-server/src/routes/runs.rs` (the same in the resume route)

Conditional, named in the final report when touched: the next release
notes file when the implementing pull request carries a version bump.
The change touches 26 files, 27 with the release notes; the first
design listed 42. The four the fourth pass added for a run guard
(`validate/guarded.rs`, `scheduler/prepare.rs`, `journal_corpus_test.rs`,
`docs/GUARDRAILS.md`) left with that guard in the sixth pass. The three
added by the fifth pass are the surfaces
that record a resume consent before the engine runs (D7 item 5.3): each
gains one call to the engine's preflight and maps its error through the
mapping it already has for the engine resume; no other surface code
changes. `run_secrets_test.rs`, `run_secrets_cli_test.rs` and
`run_secrets_api_test.rs` carry the new tests (D10 tests 4, 7 (e) to
(g), 12, 15 and 16), and `run_redact.rs` the unit tests of test 15, so
no test file is added.

### Code map

| Area | File | What |
|---|---|---|
| core | `src/validate/params.rs`, `src/validate/mod.rs` | V340, `secret_param_refs`, `SecretParamRef`, `SecretWalkError` |
| engine | `src/run_redact.rs` | `secret_names_strict` (strict on the manifest and the snapshot, `secret_set_unreadable`), `secret_render_refusal` (D11 items 1 and 2) |
| engine | `src/scheduler/resume.rs` | `secret_param_rendered` in `resume_detached_inner` and `resume_inner_checked` (the parse above the legacy shim), `resume_secret_preflight` (the safe-segment check first) |
| engine | `src/scheduler/patch.rs`, `src/forward_patch.rs` | the patch refusals of D11 items 3 and 4, the split manifest writes, the forward patch's recheck under its existing candidate lock |
| surfaces | `apb-cli/src/run.rs`, `apb-mcp/src/server/run.rs`, `apb-server/src/routes/runs.rs` | the resume preflight before the consent (D7 item 5.3) |
| engine | `src/gate/mod.rs` | the candidate skip of D7 item 4 |
| engine | `src/scenario/capture.rs` | the declared set from `run_redact::secret_names` |

### Release notes

This change bumps no version. Its lines go into the next release notes:

1. A secret param is never rendered: a playbook that references a param
   declared `secret: true` in any template (a prompt, an instruction, a
   judge state, a path, a cache key, a branch, a connector argument of a
   baseline or item check) is refused with V340, at validate, at save and
   at run start. A run started before this release that does so is
   refused at resume (`secret_param_rendered`), with the reason shown by
   the command, tool or page that asked; stop it and start it again.
2. A param declared secret stays secret for the run: a supervisor's patch
   that removes `secret: true` or removes the param while it is still
   referenced is refused (`secret_declaration_dropped`), and a waiting
   candidate that renders a secret is skipped (`candidate_skipped:
   refused: secret_param_rendered`) and the start runs `current`.
3. A script reads a param from `$APB_RUN_DIR/run.yaml` under the
   top-level `params` key, which holds the run's resolved values (given,
   recalled or defaulted); the file is YAML, mode 0600 (see the authoring
   guide).
4. The limit toward older binaries of D7 "Older binaries", stated
   once there and in the authoring guide.
5. The migration sentence of D7.

### Future work

1. A sub-playbook param binding that copies a parent's secret into a
   child param declared secret (D4).
2. Handing a secret param to an agent step in its environment, sandboxed
   or not (D5).

### Review record

#### Design review d1 (2026-11-20): dispositions

The review of the first design (`apb-secretparams-own-d1`) is the
recorded reason for the redesign. Every finding, with what happened to
it:

| Finding | Disposition |
|---|---|
| B1 plain runs write a different journal | Fixed by the principle: no store-time scrub exists, so no plain run changes (D8 item 2, D10 test 14). |
| M1 host reply stored unscrubbed, host holds the value | Removed with the mechanism. The host started the run with the params (D5 item 5); its reply is step output (D8 item 1). |
| M2 a child bypasses the host check through the instruction | Fixed: a `playbook` node's `instruction` is refused by V340 (D3); a child gets no params at all (D4). |
| M3 missed write sites | Removed with the mechanism: the requirement no longer depends on the list of write sites. The instruction header of `context.md` is operator text; commit subjects, scratch files, files under `$APB_RUN_DIR`, IMAP attachments are step output (D8). |
| M4 nested agent stores | Removed with the mechanism. |
| M5 crash windows | Removed with the mechanism: nothing is scrubbed after the fact. |
| M6 the store pass races list items | Removed with the mechanism. |
| M7 the scrub breaks structured output and routing | Removed with the mechanism; routing reads the executor's output as on main (Cut item 11). |
| M8 percent-encoded values in URLs | Removed with the mechanism: no template can put a secret into a connector argument (V340). |
| M9 old node cache entries replay raw outputs | Fixed by construction: a cache key holds the rendered prompt and declared key, which can no longer hold the value, so no new run matches an entry whose prompt held it. Old entries stay under `.apb/cache` as on main. |
| M10 mixed binaries and the baseline across the change | Fixed: a run this apb starts renders no secret, so an older apb resuming it renders the same (D7); a run an older apb started that renders one is refused at resume here (D7 item 5); no baseline is scrubbed. |
| m1 a truncated value leaves its prefix | Removed with the mechanism. |
| m2 other JSON encoders' escapes | Removed with the mechanism. |
| m3 the scenario adapter bypasses fix A | Removed with the mechanism. |
| m4 `run_check` without a sandbox | Removed with the mechanism; D5 item 2 states what a success check gets. |
| m5 no source for "the tree declares no secret" | Replaced: V340 reads the definition's declarations; the capture reads `run_redact::secret_names` (D6). |
| m6 resolving every connector secret per spawn | Removed with the mechanism; nothing resolves secrets at spawn time. |
| m7 V340 on `cache.key` has no write site | Kept with a reason: a rendered key is hashed into `node_cache_hit.key`, an unsalted digest of a weak secret is guessable offline (the review's own pre-existing item 1), and V340 refuses every rendered field alike. |
| m8 the `secrets_unresolved` attempt failure undecided | Removed with the mechanism (Cut item 10). |
| m9 three `SpawnEnv::for_node` calls expected, two exist | Removed with the mechanism. |
| m10 a short pre-change value cannot be matched | Removed with the mechanism; a pre-change run that renders a secret is refused at resume whatever the value's length (D7 item 5). |
| Pre-existing: unsalted digests of rendered prompts and keys | Fixed by construction: no rendered text holds the value. |
| Pre-existing: MCP `pending_tasks` hands out host prompts unredacted | Fixed for secret params: a host prompt cannot hold one. Step output in a prompt is as on main. |
| Pre-existing: an agent can read `run.yaml` | Kept: it is the delivery for a plain cli step and for host mode (D5). |
| Pre-existing: a prompt on argv is in the process table | Fixed for secret params: no prompt holds one. |
| Pre-existing: the claude session store keeps prompts | Fixed for secret params: no prompt holds one. |

#### What the code contradicted in the redesign brief

1. Pass-through: the brief allowed an exact sub-playbook param binding.
   No such binding exists; a child gets no params (D4). None is allowed
   and none is added.
2. The validator's template walk: there is no single walk. The rendered
   fields are spread over nine collectors and three rules (D3 table).
   V340 walks every string of the definition instead, which is complete
   by construction without a hand list.
3. Delivery: a sandboxed agent cannot read `run.yaml` (D5 item 4), and a
   `success_check` gets no run context at all (D5 item 2). Only plain
   cli agents, scripts and host-mode subagents can.
4. Copies of run config: all but one already kept the value out; the
   scenario capture judged secrecy by the current definition only and is
   fixed by reading the run's declared set (D6).
5. Stored versions: the run gate does not validate; preparation does,
   for every start, before the run directory exists (D7 item 3). A
   resume does not validate at all, so this change adds the one check
   there (D7 item 5).

#### Split from one-point-oh-fixes (2026-11-20)

This change was the decision D4.12 of one-point-oh-fixes, moved here as a
security change with its own review. The first design of this change was
replaced by this one after review d1, above.

#### Third pass: design review d2 (2026-11-20)

The second design review (`apb-secretparams-own-d2`: 1 blocker, 4 major,
5 minor, 7 pre-existing notes) was checked finding by finding against
the code on efae5822; every claim held, with the line shifts noted
below. Dispositions:

| Finding | Disposition |
|---|---|
| B1 a patch declassifies a secret, then renders it | Fixed (D11). A param once secret in a run stays secret for it and for every version a patch creates from it: an in-run patch and a forward patch that drop `secret: true`, or drop the param while a reference remains, are refused `secret_declaration_dropped` beside the existing refusals (`apply_patch`, `scheduler/patch.rs` line 34, refusals at lines 123 and 130; `forward_patch::create` after `definition_guard`, `forward_patch.rs` line 107). The check takes the run's full secret set (`run_redact::secret_names_strict`, over `secret_names`, `run_redact.rs` line 77) at a patch, at resume and at the candidate, and `apply_patch` checks the version it loads, so a version stored before the rule or edited on disk is never applied. Code note: `definition_guard` is at line 323, not 254 (lines 254 to 306 are `protected_fields`); the patch version write validates at `versioning/mod.rs` line 799 as cited. Tests 11 and 12. |
| M1 serializing to JSON can fail and the failure path is open | Fixed (D3 items 2 and 3). The walk is over `yaml_serde::to_value` (any key type), every scalar string, keys included, and an error is a refusal (V340 `(definition)`, `secret_param_rendered`), never a skipped check. Code note: `Supervisor.policy` is at `schema.rs` line 1205, `DurationText::Other` at 1046, `ExpectedDuration::Invalid` at 1231; `yaml_serde::to_value` itself can return an error (`EmptyTag`, custom errors), which is why item 3 is stated. Test 9. |
| M2 the resume refusal happens only in the detached child, after the slot and the lock | Fixed (D7 item 5). The check runs in `resume_detached_inner` after the parse at `resume.rs` line 542 and in `resume_inner_checked` after the parse at line 1127, on the value the drive executes, before the journal is opened, drift is appended, `reserve_resume` (line 1225) and the tree lock; each surface maps the `Conflict` as it maps `deadline_passed` (MCP `tools/mod.rs` line 112, HTTP `state.rs` line 292, CLI `run.rs` lines 2832 to 2842, `resume --all` `resume_all.rs` lines 380 and 396, wakers `waker.rs` line 348). `try_journal_resumed` gets no check. Code note: the in-process parse is at line 1127, not 1109. Test 7 goes through `resume_detached`. |
| M3 a pre-change candidate blocks every start | Fixed (D7 item 4): the gate's candidate branch (`gate/mod.rs` lines 785 to 811) runs the D11 check first and skips the candidate with `refused: secret_param_rendered` (or `secret_declaration_dropped`), the form of `skip_reason` (line 1099), falling back to `current`. Test 13. |
| M4 the golden journal is unstable and cannot fail because of this change | Accepted: test 9 and task 0.7 are cut, with the golden file. The change adds no write site, event or field, so there is nothing to compare; the existing corpus and equal-journal tests stay green (`journal_compat_test.rs`, `journal_corpus_test.rs`, `edge_route_test.rs` line 585, `resume_test.rs` line 395), and the new test 14 checks the construction. The spec scenario changed with it. |
| m1 D5 overstated what a `success_check` cannot do | Fixed (D5 item 2): it is not handed `APB_RUN_DIR`; without a sandbox it runs as the operator and can read the project's files, `run.yaml` of any run included. The proposal says the same. |
| m2 two mislabelled resume references | Fixed (D7 item 5): `scheduler/wait_park.rs` line 162 is inside `mod tests` (line 124); `scheduler.rs` line 577 is the scenario-only `engine_pause`. Task 0.3 item 6 asks for exactly that. |
| m3 tasks left fixture choices open | Fixed (D7 "In-repo fixtures"): every fixture was read and decided per file; tasks 0.6 and 3.1 carry the decisions. |
| m4 `run.yaml` delivered with no stated contract | Fixed (D5 "The run.yaml contract for steps"): path, the top-level `params` mapping of strings, quoting by `yaml_serde`, mode 0600, lifetime; a docs task (5.1) and test 8 pin it. Code check: the server fixture already reads `^  message: ` from `run.yaml`, so the shape is relied on today. |
| m5 a refused pre-change run keeps holding resources with no reason shown | Settled by M2: the refusal now reaches every caller with a sentence that says to stop the run, the resume itself holds nothing new, and the stop frees what the pause held. No extra doctor row. |
| Pre-existing 1 an agent can read every secret of the run from `run.yaml` | Kept: the documented delivery (D5). |
| Pre-existing 2 a forward patch may change `params` freely | Fixed for secret params by D11 item 4; other param changes stay as on main. |
| Pre-existing 3 a pinned child that fails validation fails its parent mid-run | Kept, out of scope; a pre-change child that references its own (always empty) secret now becomes one such child. |
| Pre-existing 4 the judge sandbox renders a stored version unvalidated | Kept, out of scope; secrets are replaced by the placeholder there. |
| Pre-existing 5 `--param` on the person's own argv | Kept, out of scope. |
| Pre-existing 6 a `previous_success` snapshot can carry pre-change output | Kept: a run started before this change keeps what it wrote (D8 item 3). |
| Pre-existing 7 a credential-shaped name not declared secret renders freely | Kept: V340 follows the declaration; the author decides what is secret. |

#### Fourth pass: design review d3 (2026-11-20)

The third design review (1 blocker, 2
major, 2 minor, 2 pre-existing notes) was checked finding by finding
against the code on efae5822, searched with `zg --rg -F` before any file
was read. Every claim held. Dispositions, each accepted as the review
proposed:

| Finding | Disposition |
|---|---|
| B1 an older binary can render a secret after resuming a new run | Fixed (D12, D7 "Older binaries"). Code: `apply_patch` loads the patch version at `scheduler/patch.rs` line 84 and has no secret check in any released binary; the unknown-feature run guard exists (`event/run_guard.rs` line 3, `newer_feature` line 53, added with `run_guard` itself by #253, first released in 1.0.0-beta.3). A run whose secret set is not empty carries the guard name `secret_params`: `GuardedFeature::SecretParams`, last in `ALL` after the names on main (13 on efae5822, so 14; eval-doubles' `eval_doubles` and incremental-lists-and-session-fork's `list_skip` are reconciled at merge by task 4.9), named through a new `run_guard_of` argument (`prepare.rs` line 1973) at start (line 1644) and around a patch (`run_guard`, line 1948, from `patch.rs` line 259), and added at the resume commit for a run an older apb started. It is not a definition feature, so `manifest_guard` (`manifest.rs` line 588) and the manifest stay as they were. A run without a secret param carries no new guard and writes the journal it wrote before. The compatibility section was corrected: an older apb back to 0.25.0 reads and stops such a run, refuses to resume it and refuses to pause it by the unknown-event rule. Journal corpus row P1 with beta.1 and 0.25.0 (test 16); test 15 for this binary. Superseded in the sixth pass: the guard, row P1 and those tests were cut, and the limit toward older binaries is stated instead (D7 "Older binaries", "Cut in the sixth pass"). |
| M1 a rejected or interrupted patch makes a valid run impossible to resume | Fixed (D11 item 3). Code: `keep_secret_declarations(run_dir, &[playbook, &loaded.playbook])` is at `patch.rs` line 237, before the launch policy refusal (`LOOSENED`, line 246, the check from line 240) and before `snapshot_playbook` (line 269), as the review says. The call at line 237 now records the running definition's names only; the patched definition's names are recorded after `snapshot_playbook` returned and before `PatchApplied`; every reader takes the manifest and snapshot union, so a crash between the two counts the new names as secret and refuses no valid resume. Tests 11 (d) the later refusal, (e) the crash boundary as on-disk states, (f) the applied case. The spec scenario's unchanged manifest now holds. |
| M2 a concurrent forward patch can pass a stale secret check | Fixed (D11 item 4). Code: `forward_patch::create` reads the run (`read_all`, line 97) before the candidate lock (line 113) and writes the candidate through `create_forward_patch` (`apb-core/src/candidate/mod.rs` line 130); `apply_patch` holds no lock across its writes (it is called from the control scan, `control_apply.rs` line 211, with no lock taken around it), so no single lock serializes the two on main. The playbook's candidate lock becomes that lock: the forward patch's check moves under it, right before the candidate write, and `apply_patch` holds it from its check through the post-snapshot manifest write. Lock order: candidate lock before the run's event lock, the order the candidate settlement already uses (`candidate/mod.rs` line 238). Test 12 gains the interleaving. The lock half of this fix was cut in the fifth pass (below): the forward patch keeps its recheck under the lock it already holds, and `apply_patch` takes no lock. |
| m1 the in-process resume check is after a legacy manifest write | Fixed (D7 item 5.2). Code: the ephemeral manifest is written at `resume.rs` lines 1121 to 1126 and the snapshot parsed at line 1127. The parse moves above the shim and the check runs before the shim's write; `secret_names_strict` takes the snapshot's declarations when there is no manifest. Test 7 (d): a refused schema-1 resume leaves no `manifest.yaml`. |
| m2 the documented `run.yaml` shape describes the wrong param set | Fixed (D5 "The run.yaml contract for steps" item 2). Code: preparation applies remembered input (`run_memory::apply`, `prepare.rs` line 1043) and defaults (lines 1046 to 1050) before `RunConfig.params` is stored (line 1410). `params` is now defined as the resolved run values: given, recalled or defaulted; a secret param is never recalled (`run_params::sensitive`) and has no default (V99). Test 8 covers the three and an absent param. |
| Pre-existing: an unsandboxed script or agent running as the operator can read other runs' `run.yaml` | Kept, out of scope: the same operator owns every run of the project; D5 item 2 already states it for a `success_check`, and an isolated agent cannot (D5 item 4). |
| Pre-existing: a crash during the atomic `run.yaml` write can leave a private temp file holding the value (`fsutil.rs` line 621) | Kept, out of scope: the temp file is 0600 in the run directory and is removed by `apb runs prune` with it; a sweep of stale temp files is a change to `fsutil`, not to this rule. |

#### What the code contradicted in the third review

Nothing in a finding: every cited line held on efae5822. Two details
around them differed from what the fix would assume:

1. No single lock serializes in-run patch settlement on main, so the
   fix could not name an existing one: `apply_patch` takes no lock
   across its writes. The fourth pass made the candidate lock that lock
   by having `apply_patch` take it; the fifth pass cut that and accepts
   the interleaving a recheck alone leaves (D11 item 4.3).
2. The run guard machinery names a run's contract features from the
   journal (`deadline`, `concurrency`) and definition features from the
   playbook. A secret param fits neither alone: a patch may remove an
   unreferenced secret param, after which the definition declares none
   while the run's set still does. So `secret_params` is a run feature,
   passed to `run_guard_of` as an argument like `deadline`, never named
   by `guarded_features`, and it also needs the resume-time addition for
   runs an older apb started, which have no guard at all. (The guard was
   cut in the sixth pass.)

#### Fifth pass: design review d4 (2026-11-20)

The fourth design review (1 blocker, 2 major, 3 minor, no new
pre-existing note) was checked finding by finding against the code on
efae5822, searched with `zg --rg -F` before any file was read. Every
cited line held. Dispositions:

| Finding | Disposition |
|---|---|
| B1 a resume that also grants a deadline can lose the `secret_params` guard | Accepted, fixed (D12 item 5). Code: `try_journal_resumed` (`scheduler/resume.rs` line 867) reads the journal once under the event lock (line 876); its deadline branch (from line 900) builds `requires` from `latest_run_guard(&events)` (lines 904 to 907) and appends it (line 910), and only the latest `run_guard` counts (`event.rs` line 3510), so a second append built from the same read loses a name. A resume now writes one guard append that carries every name in force: the existing names read under the lock, `deadline` when a grant adds it, `secret_params` when the run's set is not empty. The single builder is `resume_run_guard` in `scheduler/prepare.rs` beside `run_guard` and `run_guard_of`; the deadline branch no longer appends a guard of its own, and for a run without a secret param it yields exactly main's guard. `ResumeJournal` (line 848) gains `secret_params: bool`. Test 15 gains the combined case (a run an older apb started, with a secret param, resumed with a grant: one guard naming both) and unit tests of the builder; Wrong implementations item 19. Superseded in the sixth pass: the guard was cut with its builder and that item ("Cut in the sixth pass"); `try_journal_resumed` is no longer touched. |
| M1 the shared check misses three fields outside validation | Accepted, fixed (D3 item 5). Code: the candidate branch runs `check_run_loaded` and no validator (`gate/mod.rs` lines 785 to 811), the resume runs only the shared check (D7 item 5), and V104, V176 and V210 live only in the validator (`validate/quality.rs`, `validate/previous_success.rs` line 24, `validate/concurrency.rs` line 14). `secret_param_refs` now walks every string with no skipped path, so the candidate check, the in-run patch, the forward patch and the resume see `quality.task_type`, `previous_success.when_none.*` and `concurrency.key`; only `check_params` drops a V340 where V104, V176 or V210 already reports. Test 4 gains, for each of the three fields, a candidate case, an in-run patch case and a resume case; Wrong implementations item 18 (numbered 20 before the sixth pass). |
| M2 the candidate lock can expire while an in-run patch holds it | Accepted as decided: the lock change is cut (see "Cut in the fifth pass"). Code: `candidate::lock` is `lock_dir` (`apb-core/src/candidate/mod.rs` line 96) with the 60-second age rule (`fsutil.rs` line 893) and a steal of a lock older than that (from line 972), and the unattended probation and readiness checks of a patch walk the pinned children (`scheduler/patch.rs` from line 135, `gate/unattended.rs` `check_probation` line 263), so the fourth pass's hold could outlive the rule. `apply_patch` takes no new lock; its ordering fix (the running definition's names before the snapshot, the new names after) stays. The forward patch reads the source run's set and checks it under the candidate lock it already holds, immediately before the candidate write: a recheck, no new lock, no long hold (D11 item 4.1). The remaining interleaving, an in-run patch that declares a param secret after that recheck, is recorded as accepted with its reason (D11 item 4.3): the candidate was checked against the run as it stood when written, a candidate is its own version judged by its own declarations by V340 at every start, and the run that declared the secret keeps it through its own guard and set (the guard was cut in the sixth pass; the set remains). The lock-order precondition, test 12's lock part and the related task text are removed; test 12 keeps a test of the recheck (a set that changed before the write refuses). |
| m1 consent can write a manifest before the resume refusal | Accepted, fixed (D7 item 5.3). Code: the CLI calls `resume_consent_need` (`apb-cli/src/run.rs` line 2759) and `record_resume_consent` (line 2766), MCP `run_resume` calls `check_resume_consent` (`apb-mcp/src/server/run.rs` line 1344) and the dashboard's route does too (`apb-server/src/routes/runs.rs` line 1237), all before the engine resume; `record_resume_consent` (`gate/mod.rs` line 307) writes the legacy ephemeral manifest (lines 320 to 329) and the consent (line 335). One more write the review did not name: `resume_consent_need` itself clears the recorded consent of a run this machine did not create (line 283). Each of the three surfaces now runs the read-only `resume_secret_preflight` first and maps its refusal as it maps the engine resume's, so a refused resume writes nothing, a legacy manifest included. Test 7 (e); three surface files added (30 files, 31 with the release notes, before the sixth pass); Wrong implementations item 19 (numbered 21 before the sixth pass). |
| m2 the lock test does not exercise the forward patch's check-to-write interval | Settled by M2: no lock is added, so the contention test is gone; test 12 tests the recheck instead (an in-run patch applied after the forward patch's YAML was made, then the forward patch refused because the set is read under its lock right before the write). |
| m3 the old-binary test accepts an unrelated parse refusal | Accepted, fixed (D10 test 16). The inline playbook of row P1 uses only fields both old binaries parse; the row runs `apb validate` with each first and asserts success, as row D11 does (`journal_corpus_test.rs` line 2648); the resume refusal is asserted with `resume_refused(reader, run, "run_guard")` (line 1787), which requires "written by a newer apb" and the guarded journal's name and a byte-equal journal. Superseded in the sixth pass: row P1 was cut with the guard. |

#### Cut in the fifth pass

1. `apply_patch` taking the playbook's candidate lock from its secret
   check through its second manifest write (fourth pass, D11 item 4.2),
   and the lock-order rule that came with it (D11 item 4.3 of the
   fourth pass: candidate lock before the run's event lock, the
   implementer's confirmation of every `candidate::lock` caller, task
   0.3 item 14). Reason: the hold put the unattended probation and
   readiness walks under a lock another caller may steal after 60
   seconds, and the recheck under the lock the forward patch already
   holds is enough for what the candidate needs (D11 item 4.3).
2. Test 12's lock part: the test that held the candidate lock while an
   in-run patch waited for it. Replaced by the recheck test.
3. Wrong implementations item 16's second half ("letting `apply_patch`
   record new secret names without holding that lock"), and the spec
   requirement "A forward patch checks the run's secret set under the
   candidate lock" in its lock form; the requirement now states the
   recheck.
4. The skip of the three owned paths inside `secret_param_refs` (D3
   item 5 of the fourth pass): the filter moved to `check_params`.
5. The sentence that no file of `apb-mcp`, `apb-server` (outside its
   tests) or the `apb` CLI changes: the resume preflight adds one call in
   each.

#### What the code contradicted in the fourth review

Nothing in a finding: every cited line held on efae5822. One detail
around m1 went further than the review: besides
`record_resume_consent`, the need check every consent surface calls
first, `resume_consent_need`, can itself write (it clears a recorded
consent of a run this machine did not create, `gate/mod.rs` line 283).
The preflight runs before that call too, so the rule "a refused resume
writes nothing" holds for it as well.

#### Sixth pass: design review d5 (2026-11-20)

The fifth design review (1 blocker, 2 major, 1 minor, 1 pre-existing
note) was checked finding by finding against the code on efae5822,
searched with `zg --rg -F` before any file was read. Every cited line
held. The blocker was again in the run guard this change added in the
fourth pass, so the guard was removed instead of repaired. Dispositions:

| Finding | Disposition |
|---|---|
| B1 a crash can leave a secret run resumable by an older binary | Accepted as a fact, settled by cutting the run guard (see "Cut in the sixth pass"). Code: preparation writes `run.yaml` (`write_run_config`, `scheduler/prepare.rs` line 1446), appends `RunStarted` (from line 1594) and `RunProvenance`, and appends the guard only then (`run_guard_of`, line 1644); `RunStarted` is a resumable lifecycle marker (`lifecycle_seq`, `scheduler/resume.rs` from line 797). A crash in that interval leaves a journal an older binary resumes. Closing it needs a durable marker written before `run.yaml` plus the full guard after provenance and an old-binary crash test per intervening state; with the fourth review's guard finding (a resume that grants a deadline could lose the name), this is the second blocker in the same mechanism, and the machinery is more than the edge deserves. The limit is stated instead, once in D7 "Older binaries" and once in the authoring guide (task 5.1): the rule is enforced by this apb and later; an older apb never had it, so an older binary that resumes or patches a run can render a secret param exactly as it could before this change; mixing binaries on one run is outside the guarantee. Compatibility now says: no new event, field, guard or manifest mark; a run with or without secret params writes the journal it wrote before (D10 test 16). |
| M1 the full secret set can silently lose snapshot names | Accepted, fixed (D11 item 1). Code: `secret_names` turns a manifest read error into an empty list (`run_redact.rs` lines 78 to 82) and `snapshot_declarations` turns a snapshot read or parse error into an empty list (lines 102 to 112); the forward patch reads the source run's journal (`read_all`, `forward_patch.rs` line 97) and the base version from the registry, not the snapshot. `secret_names_strict` is now strict on both reads: a manifest that exists and does not read, and a snapshot that is missing, does not read or does not parse, give `secret_set_unreadable`. The refusal is named per caller: the in-run patch journals `patch_rejected` with it, the forward patch answers `refused("secret_set_unreadable", ...)` with no candidate, both resume entries and the consent preflight return it as an `EngineError::Conflict` before anything is written. The candidate selection reads no run set (see below). Task 4.1 decides the mapping. Test 15 covers each read error and the crash state between the snapshot replacement and the manifest update with a failed and an unparsable snapshot read; Wrong implementations item 20. |
| M2 the consent preflight does not make the no-write refusal atomic | Accepted, settled by stating the exact guarantee (D7 item 5.3). Code: the surfaces record the consent (`gate::record_resume_consent`, `gate/mod.rs` line 307) before the engine's check on its own parse (`resume.rs` line 1127); `replace_consent` and `record_resume_consent` rewrite the manifest through `manifest::amend` (`manifest.rs` lines 813 to 826, `amend` from line 860) and a patch replaces the snapshot through `snapshot_playbook` (`run_config.rs` line 401), neither under a lock the other takes. The preflight refuses before a consent is recorded when the snapshot it reads renders a secret; when the snapshot changes between the preflight and the engine's own check (another process patched the run in between), the engine still refuses and a consent may have been recorded. Serializing the two is not added: it needs a lock shared by every consent surface and every patch writer. The spec requirement and test 7 now claim exactly that: (e) pins no write where the snapshot is unchanged, (f) pins the refusal, not the absence of a consent, in the changed-snapshot state. |
| m1 the preflight lacks an input-validation requirement | Accepted, fixed (D7 item 5.3). Code: `resume_detached_inner` and `resume_inner_checked` refuse an unsafe id with `is_safe_segment` before joining it to `.apb/runs` (`resume.rs` lines 513 and 1090) and answer `NotFound`. The preflight does the same first, and answers `NotFound` for a missing run directory too. Test 7 (g); Wrong implementations item 21. |
| Pre-existing: a step that reads `run.yaml` can print the value into stored step output | Kept, stated: it is D8 item 1 (what a step prints after reading the value is its output, redacted on the way out as on main). |

#### Cut in the sixth pass

1. The run guard name `secret_params`, completely, with the reason of
   B1 above and of the fourth review's guard finding: a guard written
   after `run_provenance` leaves a crash window, and closing it needs a
   marker before `run.yaml`, more machinery than this edge deserves.
   Removed with it:
   1. `GuardedFeature::SecretParams` in `validate/guarded.rs` (its
      doc line, its place last in `ALL`, the module doc sentence, the
      unit tests) and the merge note on the `GuardedFeature::ALL` count
      (task 4.9, the `ALL` length recorded by task 0.1);
   2. the start write (the `secret_params` argument of `run_guard_of`
      and the start's call in `scheduler/prepare.rs`) and the patch
      write (the same argument of `run_guard` from `apply_patch`);
   3. the single resume guard builder `resume_run_guard`, the
      `secret_params` field of `ResumeJournal` and the one guard append
      in `try_journal_resumed`, which this change no longer touches;
   4. the journal corpus row P1 and its old-binary tests (D10 test 16
      of the fifth pass) and the run guard test (D10 test 15 of the fifth
      pass);
   5. the `docs/GUARDRAILS.md` edit (task 5.5), the `guarded` phrase of
      `CLAUDE.md` and `AGENTS.md`, and the spec requirements "A run with
      a secret param is guarded against older binaries" and "A resume
      journals one guard with every name in force";
   6. Wrong implementations items 18 and 19 of the fifth pass (naming
      the guard in `guarded_features`, a second guard builder), and the
      four files the guard needed (`validate/guarded.rs`,
      `scheduler/prepare.rs`, `journal_corpus_test.rs`,
      `docs/GUARDRAILS.md`): 26 files, 27 with the release notes.
2. The claim that the resume preflight makes "a refused resume writes
   nothing" hold in every interleaving: replaced by the exact guarantee
   of D7 item 5.3.

#### What the code contradicted in the fifth review

Nothing in a finding: every cited line held on efae5822. One detail
differed from what the requested fix assumed: the candidate selection
at the gate never reads a run's secret set. It judges a waiting
candidate against the declarations of `current`, a version the gate has
already loaded and parsed (`gate/mod.rs` from line 785), so a strict
read has no caller there and no refusal is named for it; the four
callers of `secret_names_strict` are the in-run patch, the forward
patch, the resume entries and the consent preflight.


### Code review c1 after the implementation (2026-11-20): dispositions

All three findings accepted and fixed.

| Finding | Disposition |
|---|---|
| Blocker: `manifest::read` took a directory or a dangling link at `manifest.yaml` for "no manifest", so after a patch dropped an unused secret declaration (the manifest the only record of it) a later patch could declare the param plain | Fixed: `manifest::read` opens through `fsutil::open_regular_no_follow`; only a path where nothing exists is `Ok(None)`, and a directory, a link (dangling or not), a fifo or an unreadable file is an error naming `manifest.yaml`, which `secret_names_strict` turns into `secret_set_unreadable`. Tests: an in-run patch with a garbage, a directory and a dangling-link manifest is rejected, and a resume, a detached resume and the preflight of a run whose snapshot dropped the declaration refuse with a directory and a dangling link, writing nothing. |
| Major: the surface redactor dropped an unreadable manifest silently, so a run detail showed the value of a param only the manifest still named | Fixed: the redaction's secret set (`secret_names_read`) reports whether its two sources read (an absent file names nothing; a file that is there and does not read or parse does not read), for the run and for every run of its tree; a set that does not read makes the redactor incomplete, so the run's text is withheld as `[secrets_unresolved]` on every surface that goes through `RunRedactor`. Tests: the dashboard run detail and MCP `run_status` (server crate) and `apb runs <id>` in text and JSON withhold such a run. |
| Major: the eval case's `eval-settings.json` was kept with a kept repetition, a second copy of the case params beside `run.yaml` | Fixed: the case run removes the file as soon as it read it (a removal that fails refuses the start), and `keep_repetition` removes a settings file nobody took before it moves the repetition. Tests: unit tests of both, and the eval CLI test scans the project, the config dir (scratch and kept repetitions) and the run directory by bytes and finds the value only in `run.yaml` files and the case file. |

The patch's best-effort second manifest write (fix round 1) can no
longer be driven end to end with a directory at `manifest.yaml`: the
patch's own strict read now refuses that state first. Its branch is
tested at unit level instead (`keep_patched_declarations` in
`scheduler/patch.rs`).

### Code review c2 (2026-11-20): dispositions

Both findings accepted and fixed.

| Finding | Disposition |
|---|---|
| Blocker: a missing `playbook.yaml` read as a complete secret set, so a prepared run with no manifest (a legacy run) whose snapshot was removed showed its `run.yaml` values | Fixed in `run_redact::secret_names_read`: a missing snapshot makes the set unreadable when the run was prepared (`was_prepared`) and its `run.yaml` holds a param value (`holds_params`; a config that does not read counts as holding one). Prepared means there is something at the `run.yaml` path, because preparation writes the snapshot (`snapshot_playbook`, `scheduler/prepare.rs`) before the run config (`write_run_config`, the same function, later), or `config_expected` holds (a manifest, or a `run_started` that names its definition). The surfaces then withhold the run's text as `[secrets_unresolved]`. Only a run never prepared, or one with no param value, keeps "no snapshot names nothing". The strict read already refused a missing snapshot (`secret_names_strict`). Test: `a_prepared_run_without_its_snapshot_withholds_the_run_detail` (server crate) covers the run detail of a legacy run with no manifest and its strict refusal. Six existing tests that seed a prepared run with params and no snapshot now write a snapshot (`params: []`), as a prepared run has one. |
| Major: the launch handoff was left behind when `write_all` or `sync_all` failed | Fixed: `launch_handoff::create_filled` creates the file exclusively, 0600, and removes it on any error after creation before returning the error. Unit test `a_failed_fill_leaves_no_file` (a real `set_len` error after the params were written). Checked for the same gap: `eval-settings.json` and `run.yaml` go through `fsutil::atomic_write_private`, whose `write_via_temp_checked` removes its temp file on every error; a settings file left by a start that failed after the write sits in the repetition directory `run_repetition` removes on that error, and `keep_repetition` removes it (fix round 2). |

### Code review c3 (2026-11-20): disposition

The blocker is accepted and fixed. A start that fixed its choice to the
candidate (`launch_version` with `launch_choice: candidate`, the retry
branch `fixed_choice` in `gate/mod.rs`) reloaded the candidate without
the secret check. It now runs the same check as a waiting candidate,
`gate::candidate_secret_refusal`: `secret_render_refusal` against
`current`'s declarations, which both branches now call. A fixed choice
has no fallback to `current`, so a refusal refuses the start with the
check's code (`secret_param_rendered` or `secret_declaration_dropped`).
Test: `a_fixed_candidate_that_renders_or_declassifies_is_refused`.

Every other place a definition is loaded for execution by an explicit
choice reaches preparation, `prepare_placed`, whose validation of the
effective definition at `scheduler/prepare.rs` line 862 includes V340:

- a pinned version (`check_run` loads `wref.version`, `gate/mod.rs`
  line 724; `run`, `entry.rs` line 410, through `prepare_run`);
- an eval case (`apb run --version` in the case process, the same path);
- a trial (`playbook_trial`, `apb-mcp/src/tools/trial.rs` line 185,
  `run_background_resolved`);
- a scenario run (`scenario/bind.rs` line 68, `run_scenario_of`,
  `prepare.rs` line 639);
- a sub-playbook pin (`scheduler/node.rs` line 5075, `prepare_placed`
  with the pinned version).

These choices run the version the caller named with the values given to
that start, judged by that version's own declarations. The
"once secret, stays secret" rule is about one run's definitions over
time: it is held by the shared check of the in-run patch, the forward
patch, the resumes and the candidates. A child gets no params of its
parent (D4).

### Pull request review (2026-11-20): dispositions

The bot raised twelve threads on the pull request.

| Finding | Disposition |
|---|---|
| A forward patch can write a candidate before an in-run patch of the same run declares a param secret | Rejected, by design: no new lock (Cut). The forward patch rechecks the run's set under the candidate lock right before it writes. The in-run patch writes its snapshot, which declares the name, before its manifest write, and every reader takes both. A check that ran before the in-run patch is ordered before that declaration. The candidate never runs inside the source run. A later run of it starts with its own values and is judged against `current`'s declarations at the gate. |
| A waiting candidate was digested before the secret check | Fixed: the digest runs only on the path that admits the candidate. |
| The secret set read the manifest before the snapshot | Fixed: both readers read the snapshot first. A patch records a name in the manifest before it writes a snapshot that drops it, so a reader in between sees the name in one of the two. |
| The unreadable-run refusal carried the parser's text | Fixed: the refusal says "its params list does not parse" and never quotes the file. Unit test with a wrong-typed scalar. |
| The resume preflight and the in-process resume answered an unreadable snapshot with other errors | Fixed: `run_redact::snapshot_check` maps a snapshot that does not read or parse to `secret_set_unreadable`. The preflight uses it, and `resume_req` runs it before the consent check. |
| A retry-now of a live run skipped the secret check | Fixed: `deferred::expedite_live` runs `run_redact::snapshot_check` before it journals `retry_expedited`, on every surface (CLI, MCP, dashboard). A run nothing drives is answered as before, and the resume its caller then makes checks it. |
| Test helpers: the artifact scan followed links and hid unreadable entries; the no-write tree skipped links; the storage test excluded `.tmp*` subtrees; the sandbox probe had no positive control | Fixed: the scan reads regular files only and fails on an entry it cannot inspect; the tree records links by their target; the storage test uses a value no other test writes and excludes nothing; the sandbox probe reads a visible file with the same reader. |
| The guide's "writes nothing" for a refused resume | Fixed: the guide states that the refusal writes nothing while the snapshot and the secret set stay as checked, with the concurrent-change exception of D7 item 5.3. |
