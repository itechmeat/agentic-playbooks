# Checks for people: judge sandbox, candidate summary, demo

Date: 2026-11-03. Status: accepted. Author-facing usage:
`docs/DECISIONS.md` ("Judge sandbox and labelled examples"),
`docs/QUALITY.md` ("The summary a person reads first"), `docs/EVALS.md`
("On MCP and the dashboard"), `docs/HOWTO-authoring.md` ("Check the task
first"), `docs/MCP.md`. It builds on the scenario runs of
`2026-11-03-scenario-runs-design.md`.

## 1. Goal

A person deciding something about a playbook today has to read the
journal or start a run. Three decisions get a check of their own:

1. **Is this judge right?** The only way to see what a judge node makes
   of an input was a run. The sandbox evaluates one judge node outside
   any run, and labelled examples keep the inputs a person has an
   opinion about, so a threshold change can be checked against them.
2. **Should this candidate become `current`?** The quality comparison
   said whether trials went well, not whether the candidate starts on
   this machine, whether it validates or whether its scenarios pass. The
   candidate summary shows the three apart, and a promotion moves
   exactly what the person saw.
3. **What does a run look like?** A first-time user had nothing to run
   without an agent and a key. The demo is a keyless playbook that runs
   to a review and finishes with a report, and the "check the task
   first" example shows a judge in front of the work.

Beside them, the scenario runs of the earlier design get their MCP, HTTP
and dashboard surfaces.

## 2. Judge sandbox

1. **One evaluation.** The sandbox renders a judge node's state from
   typed inputs through the same evaluation a run uses
   (`judge_eval::evaluate`) and answers three outputs side by side: the
   raw answers, `as_configured` (what the node outputs on this machine
   now, at the `judge_node` use's mode) and `at_enforce`, each with the
   route it would take. Nothing is applied: no run directory, journal,
   cache, trust, quality or threshold record is written.
2. **Given answers are free.** With answers, nothing is asked; they are
   evaluated at this machine's mode (`not_configured`, `off`, the
   fallback below enforce, applied at enforce). Anyone may evaluate
   them, a run step included.
3. **A live ask is bounded.** Without answers, the sandbox asks the
   providers of the machine's `decisions.yaml` through a runner made
   outside any run, with a run's privacy classes, redaction and clip (a
   large state is clipped like a run, never skipped). The request type
   has no provider, URL, key or model field. Sensitive params are sent
   as `<secret>`.
4. **Cap.** 100 requests per project and UTC day, fixed. The count is
   taken from `.apb/decisions.jsonl` lines with `use: judge_sandbox`
   under the log lock, and the runner's request budget is narrowed to
   what is left, so a request past the cap is logged `budget` and never
   sent. A log that is not a plain file counts as a spent day. The line
   keeps `use_site: judge_node`, so no other use's counter sees it, and
   never carries the state.
5. **Who may spend.** A live ask is refused from a run step and from a
   dashboard without keys; a read-only MCP server does not route the
   judge tools.
6. **Emulation.** The configured `llm_emulation` providers are asked
   only where a run would ask them. The profile emulation is never
   started.

## 3. Labelled examples

1. **The file.** `.apb/playbooks/<id>/evals/judge/<node>/ex-NNNN.yaml`:
   scrubbed inputs, the expected answer per labelled question, the
   answers observed when the example was made, and the node, questions
   and state digests. The files are outside every version and inside the
   eval suite digest, and they are committed with the project.
2. **Saving** takes one judge execution of a run: its inputs as they
   were when the execution started, redacted with the run tree's
   redactor and then the workspace scrubber, a secret param as
   `<secret>`. A credential-shaped or configured secret value left in
   the inputs refuses the save instead of being rewritten. At most 200
   examples per node; a save is refused from a run step and from a
   dashboard without keys.
3. **Checking** against `current`: `stale` when the node's definition
   digest moved (questions, thresholds, state templates,
   `on_unavailable`; not the title), `state_changed` when the inputs
   render another state, otherwise per label agree or disagree with what
   the current thresholds make of the observed answers or of a new
   sandbox ask (`live`, inside the cap). Counts and a confusion table per
   node. Nothing is tuned from examples, and they are never labels of
   `apb decisions report` or stored thresholds.
4. **Fill from a run** offers every value a run recorded (node outputs,
   review notes, params and the baseline observations as JSON text),
   through the run tree's redactor, to place into an input.

## 4. Candidate summary

1. **Three axes, never merged.** `fitness` is the fit report of the
   candidate's own version on this machine; `definition` is the
   validator on the candidate, the diff from `current` and the newest
   quick and smoke scenario results; `quality` is the comparison as it
   was. Each axis is `ok`, `attention`, `blocking` or `unknown`.
2. **Fresh results only.** A scenario result counts for the candidate
   when it carries the candidate's trust digest and the scenario file's
   digest now. Anything else is listed as stale or not run.
3. **Items.** Blocking: a validator error, and a candidate edited after
   a run of it. Attention: a failed, invalid, incomplete, stale or
   missing scenario result, a scenario file that does not load, a
   quality verdict other than `promote`, a fitness problem. Fitness
   never blocks.
4. **`summary_id`** is a digest over the candidate, `current`, every
   scenario result and every item, never over a time.
5. **Promotion re-check.** A person's promotion names the version and
   digest seen (`--expect <version>@<digest>`, the HTTP body
   `expected_version`, `expected_digest`) and, with attention items, the
   summary id (`--ack`, `acknowledged`). Under the candidate lock the
   engine recomputes the definition and quality and refuses `stale`,
   `blocked` or `needs_acknowledgement`; fitness is taken before the
   lock, since it may probe agent CLIs, and still counts in the id. The
   promotion records `summary_id` in the trial record. A body-less
   promote is refused `expected_required`. Automatic promotion by a
   policy never reads the summary, and MCP stays read-only for
   candidates.

## 5. Demo and the "check the task first" example

1. **The demo** is shipped in the binary and installed into `<config
   dir>/demo` as an ordinary project playbook, `demo-report` 1.0.0: a
   script writes `report.md`, a review waits for a person, the run
   finishes with the `report` output. No agent, connector, network or
   decision model. The `apb-` prefix is reserved for the content apb
   ships, which gets `Bundled` trust; the demo deliberately does not:
   it is a person-channel version, neither imported nor approved, and
   its script needs the same acknowledgement as any script. Nothing in
   the gate, the trust store or the launch policy knows its id. `apb
   demo --run` starts the real `apb run demo-report --detach`; `apb
   demo --remove` unregisters and deletes it; `POST /api/demo` installs
   it for the dashboard's empty state.
2. **The example** `examples/playbooks/task-check-first.yaml` puts a
   judge on `{{run.instruction}}` in front of the work, with a review
   loop for an unclear task capped on the edges out of the judge. It
   ships three quick scenarios, one of them unattended and ending
   `hitl_required`.

## 6. Scenario surfaces

MCP `playbook_scenarios` and `playbook_scenario_run`, and `GET
/api/playbooks/{id}/scenarios`, `POST .../scenario-runs` and `GET
.../scenario-runs/{run}`, over one tool layer that `apb eval --level`
uses too: the same `apb __scenario-run` child, the same timeouts and the
same process-group kill. One scenario per call. A stale scenario answers
a `stale` report and never plays. A smoke run needs the cap named (1 to
200), an approved version and an approved import, and is refused from a
run step and from a dashboard without keys. Neither tool is routed by a
read-only MCP server. The answer is the report and the run graph of the
scenario run directory.

## 7. Compatibility

1. No journal event and no event field changes.
2. `decisions.jsonl` gains lines with `use: judge_sandbox`; older
   binaries count their caps by their own use names and skip them.
3. `QualityRecord.summary_id` is optional and left out when absent, so
   an older trial record reads and writes back unchanged.
4. A body-less `POST .../candidate/promote` (an older dashboard) is
   refused `expected_required` (422, JSON `{ "code": ... }`), and `apb
   candidate --promote` needs `--expect`.

## 8. Tests

Every check drives a shipped entry point.

1. The sandbox: at shadow the raw answers, the fallback and the enforce
   output each route; at `off` nothing is asked and given answers still
   evaluate; the 101st request of a day is logged `budget` and never
   sent, while other uses' caps stay whole; a large state is clipped and
   a secret param never leaves; the sandbox and a run send the same
   state and output the same.
2. Examples: a save stores scrubbed inputs and refuses a credential
   shape; a check says `stale` after a threshold edit, `state_changed`
   after an input edit, and counts agreement otherwise; a link is never
   followed.
3. Surfaces: the HTTP evaluation equals the tool layer's; a run step and
   a dashboard without keys evaluate given answers but never spend or
   save; read-only MCP neither lists nor serves the judge and scenario
   tools.
4. The summary: each axis state; a result for another digest or an
   edited scenario file is stale; a missing profile is attention; a
   validator error blocks; a promotion is refused `stale`, then
   `needs_acknowledgement`, a new scenario result changes the id, and
   the acknowledged promotion moves `current` with a history line and
   records the id.
5. The demo: `apb demo` installs once, `--run` reaches the review,
   approving finishes it with `report.md`, `--remove` unregisters it;
   the HTTP route installs once; the example's scenarios pass at quick.
6. End to end through the binary: after `apb demo`, `apb mcp` lists the
   demo's scenario, plays it at quick (passed, with the graph, nothing
   under `.apb/runs`), lists the result fresh, and the judge sandbox
   evaluates the example's judge with given answers and routes them.
7. The scenario routes list and read a stored result, refuse a run of
   another playbook, and refuse a cap at quick, a smoke run without a
   cap and a smoke run without keys before anything runs.

## 9. Not covered

A sandbox for judge edges (scenario runs cover them); fitting
thresholds from examples; saving an example from typed inputs (the file
format allows it, no surface offers it); a summary for moving `current`
to a version that is not the candidate; running scenarios on their own
when a candidate is created; an HTTP test of a played scenario run (the
route spawns the `apb` binary, which an in-process test does not have;
the binary test of item 8.6 plays one through MCP over the same tool
layer).
