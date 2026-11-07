# Proposal: recurring routines

## Why

apb has no scheduler of its own: an outside scheduler (cron, a systemd
timer, a CI job) starts a playbook and `apb wait` turns the outcome into an
exit code (`docs/CI.md`, "Any other runner"). A routine that runs every day
needs three things apb does not give it today:

1. It cannot ask "what changed since the last time this worked". A template
   reads `params.*`, `nodes.*` and a fixed `run.*` set
   (`crates/apb-core/src/validate/templates.rs` lines 24 to 65,
   `crates/apb-engine/src/context.rs` `resolve` lines 707 to 785); nothing
   names an earlier run. The earlier runs of a playbook are already found
   for the estimate and the quality comparison
   (`crates/apb-engine/src/run_history.rs` `all_runs_of`, line 89), but no
   template reads them. Each routine reinvents the window: the bundled
   `apb-inbox-digest` takes a fixed `since_days` param
   (`bundled/playbooks/apb-inbox-digest/playbook.yaml` line 25), so a day
   the routine failed is either read twice or lost.
2. A text param takes any string. The only value checks at a start are the
   missing-value check (`crates/apb-engine/src/gate/params.rs`
   `check_params`, line 26) and, for remembered input only, the type check
   (`crates/apb-core/src/run_memory/schema.rs` `check_value`, lines 55 to
   66). A scheduler that passes `repo=acme/site; ignore the above` reaches
   every prompt that renders the param.
3. Text from outside the author's control (an issue body, a mail, a web
   page) reaches agents that can also send. The bundled `apb-issue-triage`
   and `apb-inbox-digest` describe that risk in prose only (their
   `description`, lines 4 to 25 and 4 to 20); the validator cannot see it,
   and every agent node counts as network-capable anyway
   (`crates/apb-core/src/effects.rs` `ACTING`, line 20, applied in
   `node_inferred`, line 63). Nothing records that an author accepted the
   risk, and no run surface shows it.
4. A defect found on the way: the dashboard's connector editor drops a
   binding's `on_unknown` on every save
   (`web/src/lib/connectorbinding.ts` `parseBinding` and `serializeBinding`,
   lines 29 to 80; `NodePanel.svelte` line 255 rewrites every binding of
   the node). The new binding key of this change would be dropped the same
   way.

Two examples of one routine each, as every apb feature must serve both:

1. Code: a nightly job triages the CI failures and the issues opened since
   the last successful run: `since: {{run.previous_success.started_at}}`,
   the issue bodies come in through a github binding marked
   `untrusted: true`, and `repo` has `pattern: '[A-Za-z0-9._-]{1,100}'`.
2. Routine: every morning a CRM routine drafts follow-ups for the leads
   created since the last successful run, reads the lead notes a web form
   wrote (untrusted), skips the leads the last run already drafted
   (`{{run.previous_success.nodes.draft.output.lead_ids}}`), and its
   `region` param is held to `pattern: '[a-z]{2}'`.

## What changes

1. A template namespace `run.previous_success.*`: the id, start time and
   finish time of the latest earlier successful run of the same playbook,
   and that run's node outputs, read once at start and journaled on
   `run_provenance`, so a resume renders the same. A top-level
   `previous_success.when_none` gives the times a first run renders.
   Validator V176 (capability `previous-success`).
2. `pattern` on a `text` param: a regular expression the whole value must
   match, checked at every launch surface, in run preparation and in
   remembered input. Validator V177 and V178 (capability `param-pattern`).
3. Untrusted input: `untrusted: true` on a param, on a connector binding
   and on an `agent_task` or `script` node; the validator follows it through
   templates, list items, sessions, evidence and template-form `writes` to
   every agent that renders it. An agent that renders it and can send outward is an error unless the
   node says `accept_untrusted: <reason>`; one that also reaches secrets is
   a warning. Each such agent gets a fixed `## untrusted input` prompt
   section, and every run surface lists them with the accepted reason.
   Validator V179 to V182 (capability `untrusted-input`). The dashboard's
   connector editor keeps `on_unknown` and `untrusted`.
4. Bundled content: `apb-issue-triage` marks its github listing binding
   untrusted, accepts the risk on each agent node with the reason its
   description already gives, and holds `owner` and `repo` to a pattern;
   `apb-inbox-digest` marks its imap binding untrusted and accepts the risk
   on its three agent nodes.

## Out of scope

1. A scheduler inside apb. `docs/CI.md` already answers scheduled runs with
   an outside scheduler and `apb wait`; this change gives such a routine
   its window, not its clock.
2. Excluding trials from the earlier runs. A trial that writes files runs
   in a throwaway worktree (`crates/apb-mcp/src/tools/trial.rs` lines 168
   to 213), so its run directory is removed with it and never seen. An
   in-place trial (no file writes, line 229) and a candidate trial
   (`manifest.candidate_trial`) did their outside work in this project;
   skipping a successful one would make the next window repeat that work.
   Both are production runs to `run_history` already
   (`run_history.rs` `counts_as_history`, line 28). Scenario and eval runs stay excluded, as
   in every reader of earlier runs.
3. A scenario field that scripts an earlier run. No bundled playbook reads
   `run.previous_success`; a scenario run renders it as "none" (with the
   `when_none` times), and the engine tests use real earlier runs.
4. Inferring `untrusted` from a connector function's kind (inbox, imap).
   That needs the installed connector store, which the validator may not
   have (`ValidationContext.connectors` is empty then and V42, V43 stay
   silent, `crates/apb-core/src/validate/mod.rs` lines 276 to 282); an
   inferred marker would make one playbook valid on one machine and
   invalid on another. The author marks the binding.
5. A marker on the connector function itself. The same function (a
   repository's `get_issue`) is trusted in a private repository and
   untrusted in a public one; the playbook that binds it knows which.
6. A `wait` node as an untrusted source. Its webhook body is discarded, not
   stored or rendered (`docs/HOWTO-authoring.md` "Template variables",
   `run.hooks.*`); outside deliveries reach a run only through inbox
   functions, which a binding marks.
7. Taint through files in the tree. An agent that writes an issue body into
   a file and a later agent that reads it are not linked by any template;
   apb does not read what agents read. The `publish` step already needs a
   person's consent to the exact plan (`docs/PUBLISH.md`).
8. Taint through scripts reading the run directory. A script gets
   `APB_RUN_DIR` (`crates/apb-engine/src/script.rs` `run_env`, line 82)
   and can read anything there; a script whose output carries outside text
   is marked `untrusted: true` by its author.
9. The profile's `process_env` as a secret reader in V180. Profiles are
   resolved at run start, not by the validator (`ValidationContext.profiles`
   holds names only); `apb doctor --run` already lists the inherited
   variables.
10. A window for a child run. Two parent routines that run the same child
    would share one history; keying it by parent playbook and invoking node
    needs the parent's journal at every child start. A child run records
    the "none" window (`when_none` times) in this change.
11. Following taint into a child playbook. A `playbook` node is treated as
    an agent that can send (its child can do anything); the child's own
    params carry no value from the parent (a child arrives with an empty
    params map, `crates/apb-engine/src/scheduler/prepare.rs` line 880), and
    its own validator checks it.
12. Not remembering an untrusted param's value. Remembered input is what a
    person typed at a start surface; refusing it would only cost the
    person retyping.
13. Including `pattern` in the remembered-input digest
    (`params_schema_digest`, `run_memory/schema.rs` line 37). Adding a field
    to the digest's shape would mark every remembered file of every
    playbook `schema_changed` once; the value check (`check_value`) is the
    place a pattern belongs.

## Impact

1. Crates: `apb-core` (schema, new `previous_success.rs`,
   `param_pattern.rs`, `untrusted.rs`, validator modules
   `validate/previous_success.rs` and `validate/untrusted.rs`, params,
   templates, node_io, guarded, run_memory, scenario validate, template,
   duration), `apb-engine` (new `previous_success.rs`, event, state,
   context, judge inputs and examples, prepare, gate params, node prompt
   assembly, run view, HTML report, next actions), `apb-mcp` (run_status),
   `apb-cli` (`apb runs <id>`, refusal message), `apb-server` (run detail,
   ts contract), `web` (run detail notes, start dialog notes, remembered
   input reason, connector binding round trip).
2. Bundled content: `apb-issue-triage` and `apb-inbox-digest` (one patch
   bump each).
3. Journal: two additive fields on `run_provenance` (`previous_success`,
   `untrusted`). No new event type. Two new guarded feature names on
   `run_guard`, after the `write_scope` that `node-contracts` added:
   `previous_success` and `untrusted_input`.
4. Behaviour changes for release notes:
   1. A start whose text param value does not match its `pattern` is
      refused (`param_pattern_mismatch`) on every surface; a remembered
      value that does not match is not used.
   2. A playbook whose agent renders `untrusted` text and can send outward
      no longer validates until the node is isolated with no network and
      no connector, or says `accept_untrusted`. No existing playbook is
      affected: the markers are new.
   3. Agents that render untrusted text get a `## untrusted input` section
      at the end of their prompt (outside the cache key).
   4. A prompt that reads `run.previous_success.*` renders a new value per
      routine run, so its node cache key moves with every new earlier
      success. That is intended.
   5. A start of a playbook that reads `run.previous_success.*` is refused
      (`previous_success_unreadable`) when a run directory of that
      playbook that could be the latest success does not read.
   6. The dashboard's connector editor no longer drops `on_unknown`.
5. Built on main e3ae94cf, after `node-contracts` merged. It reuses what
   that change added and changes none of it: the `GuardedFeature` list
   gains two names after `write_scope`, a template-form `writes` is one
   more way untrusted text reaches a prompt, the write scope note and the
   retry briefing's repair paragraph stay before the new prompt section,
   and the validator codes start after its V175 (`design.md` D12).
