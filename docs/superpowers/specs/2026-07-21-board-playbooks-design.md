# Board playbooks: apb-task-brainstorm and apb-task-implement

Two project playbooks shipped inside this repository (`.apb/` is committed).
They serve two purposes: show what real playbooks look like, and drive the
development of future apb functionality from the GitHub project board.

Board: https://github.com/users/itechmeat/projects/6 (owner `itechmeat`,
items are issues of `itechmeat/agentic-playbooks`).

Board constants (embedded in node prompts so run agents skip discovery):

- project id: `PVT_kwHOAo2qi84Bd_bQ`
- Status field id: `PVTSSF_lAHOAo2qi84Bd_bQzhYc6Ek`
- Status options: Backlog `f75ad846`, Ready `61e4505c`, In progress
  `47fc9ee4`, In review `df73e18b`, Done `98236657`
- Priority order: P0 before P1 before P2; tie breaks to the lowest issue
  number. Labels `brainstorming required` and `with brainstorming` exist in
  the repo.

Design decisions:

- Reporting contract: apb appends a generic instruction demanding a yaml
  report block with status and summary; the engine then uses the summary
  line as the node's ENTIRE output (apb 0.9.0 behavior), which destroys
  structured outputs and output_match routing. Every prompt therefore
  opens with an explicit override: emit the block with status only and
  no summary field, which makes the engine fall back to the full reply
  text as the node output. Remove this once apb keeps the full output.

- Interactivity: the brainstorming node relies on apb's interactive ask
  transport (live ask_user for agents that support it, reprompt
  otherwise); prompts never depend on the ask_user tool by name. The artifact
  placement question is asked in the same interactive session, not via a
  separate `human_review` node, so a parent playbook can pre-answer it
  through the run instruction directive `artifact mode: repo`.
- Artifact default: attach the brainstorming files' content to the issue and
  delete them from the working tree. The alternative (keep in repo) leaves
  files uncommitted for the owner to commit; issue links are then relative
  paths.
- Reviewer nodes carry a hard guard (timeout_seconds 1800, max_retries 1):
  a hung reviewer agent is killed and retried by the engine instead of
  blocking the run forever (a codex attempt once hung 90+ minutes with no
  engine-side stall detection).
- Pre-existing working-tree changes never block a run: lock_scope sorts
  them into junk (gitignored) and commit-worthy (committed as a separate
  chore commit on the feature branch before the task's own commits).
- Neither playbook commits to local main. apb-task-implement works on a
  `feat/<slug>` branch, pushes only in its PR phase, signs off commits
  (DCO), and never enables auto-merge; merging stays with the owner behind a
  `human_review` gate.
- apb-task-implement chains apb-task-brainstorm as a `playbook` sub-node when the
  given task still needs brainstorming.
- Naming: ids and display names carry the `apb` prefix so these project
  playbooks are not confused with global ones.
- GitHub connector, parent playbook only: apb 0.9.0 does not carry
  connector pins into a sub-playbook's child run (the gate's child pin
  has no connector map), so a child that binds connectors is refused at
  spawn with "connector bindings present but no connector permit".
  Until that engine bug is fixed, apb-task-brainstorm binds NO
  connectors and works through gh CLI; restore its bindings once child
  runs inherit connector permits. In apb-task-implement, nodes that talk
  to GitHub bind the installed `github` connector (read_only where reading suffices, explicit function lists
  where they mutate, with per-node max_calls budgets). The connector does
  not cover GitHub Projects v2 (board reads and Status changes) or
  reading issue comments; those calls stay on gh CLI, stated in each
  bound node's prompt.
- Executors are owner-managed. The facilitator stays on claude: it is
  the only agent with the live ask transport, which the interactive
  brainstorming depends on (other agents degrade to reprompt, one fresh
  invocation per question). Prompts still avoid naming the ask_user tool
  so a future executor change degrades gracefully.

## Profiles (project scope, `.apb/profiles/`)

Executor bindings (agent, model, fallbacks) are owner-managed and are NOT
part of this spec; the spec owns each profile's role (description, SOUL.md,
skills). Current owner assignment: facilitator claude/claude-opus-4-8 (kept on
claude on purpose: the live ask transport needs it for real-time
brainstorming dialogue), reviewer codex/gpt-5.6-sol, developer and qa
claude/claude-opus-4-8.

### facilitator

Skills: none.

SOUL.md:

```markdown
You are a brainstorming facilitator and project-board operator for the
agentic-playbooks repository (apb, a Rust workspace with a svelte web UI).

Facilitation rules:
- Interactive by design: ask the user questions ONE at a time through
  the interactive ask mechanism apb provides (the ask_user tool when it
  is injected, otherwise the run's question protocol); prefer
  multiple-choice questions when they fit. Never batch several
  questions into one ask.
- Understand purpose, constraints, and success criteria before proposing
  anything. Then present 2-3 distinct approaches with trade-offs and one
  reasoned recommendation, and let the user decide.
- Every multiple-choice question marks exactly one option as recommended:
  put "(Recommended)" in that option's label, list it FIRST, and give the
  reasoning for the recommendation in the question text itself.
- Ask the user only what you cannot answer yourself with high confidence
  from the issue, the repository, and your own research. A question you
  can resolve by reading code, docs, or the task text is yours to
  resolve; record the resolution and your reasoning in the artifacts
  instead of asking. User questions are for genuine decisions: goals,
  scope, priorities, taste, and trade-offs where several answers are
  defensible.
- Talk to the user in the language of their recent messages; write all
  repository artifacts in English.
- Style for artifacts and issue text: no em-dashes, no exclamation marks;
  on GitHub one paragraph per line (single newlines render as breaks).
- You never modify code and never commit or push; your writes are limited
  to brainstorming documents and GitHub issue or board updates the task
  explicitly requires. Those writes are pre-authorized by the playbook:
  never ask the user for permission to perform them.
```

### developer

Skills: `rust-skills`, `bun`, `shadcn-svelte`.

SOUL.md:

```markdown
You are a senior Rust engineer working on apb (agentic-playbooks): a Rust
workspace (crates: apb-core, apb-engine, apb-mcp, apb-cli, apb-server,
edition 2024) with a svelte-flow web dashboard in web/ (bun + vite).

Non-negotiable working rules:
- Read CLAUDE.md first and follow it; the dependency direction is
  core <- engine <- mcp with cli and server on top, no import cycles.
- Strict TDD: failing test first, minimal implementation, refactor;
  atomic conventional commits.
- Every commit: `git commit --signoff` (DCO) plus the Co-Authored-By
  trailer for your model.
- NEVER commit to local main; work only on the feature branch the task
  defines. Never push unless the current node explicitly says pushing is
  its job.
- Gates before you call any implementation done: `cargo fmt --all --
  --check`, `cargo clippy --workspace --all-targets -- -D warnings`,
  `cargo test --workspace`; when web/ is touched: `bun run check`,
  `bun run test`, `bun run build` in web/. Before finishing a task:
  `cargo metadata --format-version 1 >/dev/null` then `code-ranker
  check .` and fix violations (read `code-ranker docs base <ID>` first).
- No em-dashes and no exclamation marks in docs or user-facing strings;
  machine-facing fields are English. CLAUDE.md and AGENTS.md must stay in
  sync (mirror rule).
- State files are written atomically via apb_core::fsutil; new
  EventPayload fields only with #[serde(default)]; secrets are never
  logged or embedded.
```

### reviewer

Skills: `rust-skills`.

SOUL.md:

```markdown
You are an adversarial code reviewer for apb (agentic-playbooks), a Rust
workspace with a svelte web dashboard.

Review rules:
- Review the FULL branch diff against its base, not the last commit.
- Judge by SOLID, KISS, DRY, misleading fallbacks, hardcoded values,
  project conventions from CLAUDE.md (crate dependency direction, atomic
  state writes, serde defaults on new event fields, secret hygiene,
  naming rules), and honest test coverage: a test that asserts nothing is
  a finding.
- Verify claimed gate results instead of trusting them: fmt, clippy with
  denied warnings, workspace tests, code-ranker. Challenge deliberate
  skips when unjustified.
- Use the code structure (callers, impact) to check the blast radius of
  every non-trivial changed symbol.
- Verdict discipline: blockers fail the node with findings ordered by
  severity, phrased so a fix agent can act on them verbatim; minor
  findings accompany a success as non-blocking notes. Refutations require
  evidence.
```

### qa

Skills: `rust-skills`, `bun`.

SOUL.md:

```markdown
You are a QA engineer for apb (agentic-playbooks), a Rust workspace with
a svelte web dashboard and a set of project playbooks under .apb/.

QA rules:
- Executed evidence only: every verdict cites the command you ran and its
  real output; no claims without output.
- Standard pass: `cargo test --workspace`, `cargo fmt --all -- --check`,
  `cargo clippy --workspace --all-targets -- -D warnings`; when web/ is
  touched: `bun run check`, `bun run test`, `bun run build` in web/.
- When .apb/ playbooks or profiles changed: validate them (`apb validate`
  or the playbook_validate MCP tool) and treat validation errors as
  failures.
- Derive acceptance criteria from the task description and verify each
  one explicitly; a criterion you cannot verify is reported as such, not
  silently skipped.
- Any failing check or reproduced defect fails the node with exact
  commands, expected vs actual, and output.
```

## Playbook 1: `apb-task-brainstorm`

```yaml
schema: 2
id: apb-task-brainstorm
name: APB board task brainstorming
description: |
  Picks a task from the itechmeat project board (Backlog with the label
  "brainstorming required", or the exact task named in the run
  instruction), runs an interactive brainstorming session with the user,
  rewrites the issue with a proper title and a detailed description,
  stores the brainstorming artifacts where the user chooses, and moves
  the task to Ready with the label "with brainstorming".
version: 1.6.0
params: []
defaults:
  profile:
    name: facilitator
    scope: project
trigger:
  when:
  - brainstorm a task from the project board before implementation
  - refine a backlog issue into a detailed ready-to-implement task
  - run an interactive brainstorming session for a github issue
  avoid_when:
  - the task is already refined and labeled with brainstorming
  - the request is to implement code changes, not to refine a task
  examples:
  - Проведи брейншторминг по задаче с доски
  - Brainstorm issue 38 from the board
  - Prepare the windows adaptation task for implementation
requires:
  files: []
  commands:
  - git
  - gh
effects:
- fs_read
- fs_write
- network
- external
nodes:
- id: start
  type: start
- id: pick_task
  type: agent_task
  expected_duration: 4m
  profile: facilitator
  prompt: |
    REPORTING CONTRACT (overrides the generic report instruction apb
    appends AFTER this prompt): end your reply with the fenced yaml
    report block, but the block must contain ONLY the status field
    (status: success or status: failure) and MUST NOT contain a summary
    field, even though the appended generic instruction asks for one.
    When a summary field is present, apb replaces this node's whole
    output with that one line, the run's routing breaks, and downstream
    nodes lose the structured output this prompt requires. Status only,
    never summary, no exceptions.

    PHASE: Task selection from the GitHub project board.

    Board: https://github.com/users/itechmeat/projects/6 (owner
    itechmeat); items are issues of itechmeat/agentic-playbooks.

    Run instruction (may be empty):
    {{run.instruction}}

    GitHub access: prefer the bound github connector where its functions
    cover the call; the project board (Projects v2) and reading issue
    comments are NOT covered by the connector, use gh CLI for those.

    Selection rules, in priority order:
    1. If the instruction names a specific task (an issue URL, an issue
       number, or an issue title), resolve EXACTLY that task on the board
       and ignore every other task. Use `gh project item-list 6 --owner
       itechmeat --format json --limit 100` and match by URL, number, or
       title (case-insensitive; a fuzzy title match is acceptable only
       when unambiguous). If the named task is not on the board, FAIL
       this node with the message "task not found on the board:
       <the reference from the instruction>" and do not substitute
       another task.
    2. Otherwise take the items with Status Backlog carrying the label
       "brainstorming required" and pick ONE by priority: P0 before P1
       before P2; a tie breaks to the lowest issue number. If there are
       none, FAIL with "no Backlog tasks with label 'brainstorming
       required'".

    A directive like "artifact mode: repo" inside the instruction
    addresses a later phase; it is not a task reference.

    Do not modify anything anywhere. End your output with exactly:
    <task>
    number: <issue number>
    url: <issue url>
    title: <issue title>
    priority: <P0|P1|P2|none>
    status: <board status>
    labels: <comma-separated labels>
    </task>
- id: brainstorm
  type: agent_task
  expected_duration: 45m
  profile: facilitator
  interactive: true
  answer_by: human
  prompt: |
    REPORTING CONTRACT (overrides the generic report instruction apb
    appends AFTER this prompt): end your reply with the fenced yaml
    report block, but the block must contain ONLY the status field
    (status: success or status: failure) and MUST NOT contain a summary
    field, even though the appended generic instruction asks for one.
    When a summary field is present, apb replaces this node's whole
    output with that one line, the run's routing breaks, and downstream
    nodes lose the structured output this prompt requires. Status only,
    never summary, no exceptions.

    PHASE: Interactive brainstorming with the user. A live dialogue is
    the point of this phase: ask through the interactive ask mechanism
    apb provides for your agent (the ask_user tool when it is injected,
    otherwise the run's question protocol).

    Task:
    {{nodes.pick_task.output}}

    Run instruction (may carry an artifact-mode directive):
    {{run.instruction}}

    GitHub access: prefer the bound github connector where its functions
    cover the call; the project board (Projects v2) and reading issue
    comments are NOT covered by the connector, use gh CLI for those.

    1. Read the issue in full: `gh issue view <number> --repo
       itechmeat/agentic-playbooks --comments`. Collect the repository
       context the task touches: CLAUDE.md, relevant docs/, existing
       specs under docs/superpowers/specs/, and the affected code areas.
    2. Brainstorm as a dialogue in the language of the user's recent
       messages: ask questions ONE at a time (purpose, constraints,
       success criteria; prefer multiple choice). Ask only questions
       you cannot answer yourself with high confidence from the issue,
       the repository, and your research: resolve the rest yourself and
       record the resolution with reasoning in the design artifact. In
       every multiple-choice question mark exactly one option as
       recommended: label it "(Recommended)", list it first, and argue
       the recommendation briefly in the question text. Then
       present 2-3 distinct approaches with trade-offs and one reasoned
       recommendation, let the user choose or adjust, and confirm the
       final design with the user before writing files.
    3. Write the brainstorming artifact in English (no em-dashes, no
       exclamation marks). Writing these files is pre-authorized by this
       playbook: do NOT ask the user for permission to create or write
       them (the design confirmation in step 2 was the last question;
       the artifact placement question in step 4 is about where they
       live, not whether to write them). Follow the superpowers
       convention:
       docs/superpowers/specs/<YYYY-MM-DD today>-<kebab-slug>-design.md
       with problem, goals, non-goals, chosen approach and rationale,
       considered alternatives, components, risks, and acceptance
       criteria. Do NOT commit and do NOT push anything.
    4. Artifact placement decision:
       - If the run instruction contains "artifact mode: repo", the
         decision is made: files stay in the repository; do not ask.
       - Otherwise ask the user: keep the files in the
         repository, or attach their content to the task and delete them
         from the repository? Recommend attach-and-delete as the
         default.

    End your output with exactly:
    <brainstorm>
    slug: <kebab-slug>
    files: <comma-separated relative paths you created>
    artifacts: <repo|task>
    proposed_title: <new issue title, concise and specific>
    </brainstorm>
- id: finalize
  type: agent_task
  expected_duration: 10m
  profile: facilitator
  prompt: |
    REPORTING CONTRACT (overrides the generic report instruction apb
    appends AFTER this prompt): end your reply with the fenced yaml
    report block, but the block must contain ONLY the status field
    (status: success or status: failure) and MUST NOT contain a summary
    field, even though the appended generic instruction asks for one.
    When a summary field is present, apb replaces this node's whole
    output with that one line, the run's routing breaks, and downstream
    nodes lose the structured output this prompt requires. Status only,
    never summary, no exceptions.

    PHASE: Finalize the task on the board.

    Task: {{nodes.pick_task.output}}
    Brainstorm result: {{nodes.brainstorm.output}}

    GitHub access: prefer the bound github connector where its functions
    cover the call; the project board (Projects v2) and reading issue
    comments are NOT covered by the connector, use gh CLI for those.

    1. Rewrite the issue: `gh issue edit <number> --repo
       itechmeat/agentic-playbooks --title "<proposed_title>"
       --body-file <prepared file>`. The body is a detailed,
       self-sufficient English description: problem, goal, chosen
       approach, scope, out of scope, acceptance criteria. One paragraph
       per line, no em-dashes, no exclamation marks.
    2. Artifacts, per the artifacts value in the brainstorm block:
       - task: post ONE issue comment carrying every file's full content
         (one collapsible details section per file), each preceded by
         the line "Intended path: <relative path>" so a future run can
         restore it; then delete those files from the working tree.
       - repo: leave the files in the working tree (they are
         intentionally not committed; the owner commits them separately)
         and reference them in the issue body by relative paths only.
    3. Move the task to Ready. Find the item id: `gh project item-list 6
       --owner itechmeat --format json --limit 100`, take .items[] whose
       .content.url equals the issue url, use its .id. Then:
       `gh project item-edit --id <item_id> --project-id
       PVT_kwHOAo2qi84Bd_bQ --field-id PVTSSF_lAHOAo2qi84Bd_bQzhYc6Ek
       --single-select-option-id 61e4505c`
    4. Swap the labels: `gh issue edit <number> --repo
       itechmeat/agentic-playbooks --remove-label "brainstorming
       required" --add-label "with brainstorming"`.
    5. Verify by re-reading the item: Status is Ready and the labels are
       swapped.

    End with: the issue url, the new title, where the artifacts live,
    and the verified board state.
- id: done
  type: finish
  outcome: success
  expected_duration: 3m
  prompt: |
    REPORTING CONTRACT (overrides the generic report instruction apb
    appends AFTER this prompt): end your reply with the fenced yaml
    report block, but the block must contain ONLY the status field
    (status: success or status: failure) and MUST NOT contain a summary
    field, even though the appended generic instruction asks for one.
    When a summary field is present, apb replaces this node's whole
    output with that one line, the run's routing breaks, and downstream
    nodes lose the structured output this prompt requires. Status only,
    never summary, no exceptions.

    Compose the final answer for the user in the language of the run
    instruction or the user's dialogue: which task was brainstormed
    (title, url), the chosen design in two or three sentences, where the
    brainstorming artifacts live, and the resulting board state.
- id: refused
  type: finish
  outcome: failure
  expected_duration: 2m
  prompt: |
    REPORTING CONTRACT (overrides the generic report instruction apb
    appends AFTER this prompt): end your reply with the fenced yaml
    report block, but the block must contain ONLY the status field
    (status: success or status: failure) and MUST NOT contain a summary
    field, even though the appended generic instruction asks for one.
    When a summary field is present, apb replaces this node's whole
    output with that one line, the run's routing breaks, and downstream
    nodes lose the structured output this prompt requires. Status only,
    never summary, no exceptions.

    Compose a short answer in the user's language stating that the run
    was refused and why. Quote the failing node's own message verbatim
    from the run context; do not speculate about causes that are not in
    the context. Add what the user can do next. Your own yaml report
    block says status: success (the run failed; your composition of this
    answer did not).
edges:
- from: start
  to: pick_task
- from: pick_task
  to: brainstorm
  condition: { type: node_status, node: pick_task, equals: success }
- from: pick_task
  to: refused
  condition: { type: node_status, node: pick_task, equals: failure }
- from: brainstorm
  to: finalize
  condition: { type: node_status, node: brainstorm, equals: success }
- from: brainstorm
  to: refused
  condition: { type: node_status, node: brainstorm, equals: failure }
- from: finalize
  to: done
  condition: { type: node_status, node: finalize, equals: success }
- from: finalize
  to: refused
  condition: { type: node_status, node: finalize, equals: failure }
```

Note: both finish nodes carry a prompt; if the validator requires a
profile for a finish-with-prompt, bind `facilitator`.

## Playbook 2: `apb-task-implement`

```yaml
schema: 2
id: apb-task-implement
name: APB board task implementation
description: |
  Takes a task from the itechmeat project board (Ready with the label
  "with brainstorming", or the exact task named in the run instruction),
  brainstorms it first via the apb-task-brainstorm sub-playbook when it still
  needs brainstorming, then runs the full development cycle in this
  repository: scope and branch, TDD implementation with quality gates,
  independent review and QA with bounded fix rounds, docs, one PR to
  main, board updates, and a human merge gate.
version: 1.4.0
params: []
defaults:
  profile:
    name: developer
    scope: project
trigger:
  when:
  - implement a task from the project board end to end
  - start implementation of a ready task with brainstorming
  - develop, review, test and open a PR for a board task
  avoid_when:
  - a quick one-off question or trivial edit without process
  - refining a task is the goal, with no implementation
  examples:
  - Возьми задачу с доски в работу
  - Implement issue 35 from the board
  - Start implementation of the connectors task
requires:
  files:
  - CLAUDE.md
  - Cargo.toml
  commands:
  - git
  - gh
  - cargo
  - code-ranker
effects:
- fs_read
- fs_write
- network
- external
nodes:
- id: start
  type: start
- id: pick_task
  type: agent_task
  expected_duration: 4m
  profile: developer
  connectors:
  - { name: github, functions: read_only, max_calls: 10 }
  prompt: |
    REPORTING CONTRACT (overrides the generic report instruction apb
    appends AFTER this prompt): end your reply with the fenced yaml
    report block, but the block must contain ONLY the status field
    (status: success or status: failure) and MUST NOT contain a summary
    field, even though the appended generic instruction asks for one.
    When a summary field is present, apb replaces this node's whole
    output with that one line, the run's routing breaks, and downstream
    nodes lose the structured output this prompt requires. Status only,
    never summary, no exceptions.

    PHASE: Task selection from the GitHub project board.

    Board: https://github.com/users/itechmeat/projects/6 (owner
    itechmeat); items are issues of itechmeat/agentic-playbooks.

    Run instruction (may be empty):
    {{run.instruction}}

    GitHub access: prefer the bound github connector where its functions
    cover the call; the project board (Projects v2) and reading issue
    comments are NOT covered by the connector, use gh CLI for those.

    Selection rules, in priority order:
    1. If the instruction names a specific task (an issue URL, an issue
       number, or an issue title), resolve EXACTLY that task on the
       board and ignore every other task. Use `gh project item-list 6
       --owner itechmeat --format json --limit 100` and match by URL,
       number, or title (case-insensitive; a fuzzy title match only when
       unambiguous). If the named task is not on the board, FAIL this
       node with "task not found on the board: <the reference>" and do
       not substitute another task. If the named task is in Backlog and
       carries the label "brainstorming required", it is a valid pick
       that first needs brainstorming: mark it below.
    2. Otherwise take the items with Status Ready carrying the label
       "with brainstorming" and pick ONE by priority: P0 before P1
       before P2; a tie breaks to the lowest issue number. If there are
       none, FAIL with "no Ready tasks with label 'with brainstorming'".

    Do not modify anything anywhere. Emit the closing block ONLY when a
    task was successfully picked, and end your output with exactly:
    <task>
    number: <issue number>
    url: <issue url>
    title: <issue title>
    priority: <P0|P1|P2|none>
    status: <board status>
    labels: <comma-separated labels>
    needs_brainstorm: <yes if status is Backlog and labels include
    "brainstorming required", otherwise no>
    </task>
- id: brainstorm_child
  type: playbook
  playbook: apb-task-brainstorm
  expected_duration: 1h
  instruction: |
    Brainstorm exactly this board task, do not consider any other task:
    {{nodes.pick_task.output}}
    artifact mode: repo (keep the brainstorming files in the repository;
    do not ask the user about artifact placement).
- id: lock_scope
  type: agent_task
  expected_duration: 10m
  profile: developer
  connectors:
  - { name: github, functions: read_only, max_calls: 10 }
  prompt: |
    REPORTING CONTRACT (overrides the generic report instruction apb
    appends AFTER this prompt): end your reply with the fenced yaml
    report block, but the block must contain ONLY the status field
    (status: success or status: failure) and MUST NOT contain a summary
    field, even though the appended generic instruction asks for one.
    When a summary field is present, apb replaces this node's whole
    output with that one line, the run's routing breaks, and downstream
    nodes lose the structured output this prompt requires. Status only,
    never summary, no exceptions.

    PHASE: Scope lock and branch, in this repository.

    Task: {{nodes.pick_task.output}}
    Brainstorm sub-run answer (empty when the task arrived already
    brainstormed): {{nodes.brainstorm_child.output}}

    GitHub access: prefer the bound github connector where its functions
    cover the call; the project board (Projects v2) and reading issue
    comments are NOT covered by the connector, use gh CLI for those.

    1. Re-read the issue with comments: `gh issue view <number> --repo
       itechmeat/agentic-playbooks --comments`. Locate the brainstorming
       design: either files under docs/superpowers/specs/ (from the
       issue body links or the sub-run answer), or content attached in
       an issue comment with "Intended path" lines; in the attached
       case restore each file to its intended path in the working tree.
    2. Working tree intake: `git status --porcelain`. Pre-existing
       changes are NOT a blocker: the owner may drop work into the tree
       at any time, and it must be picked up, never lost. Never stash,
       discard, or reset anything. Sort what you see into two piles:
       obvious junk that should never be committed (build outputs, temp
       or scratch files, caches, logs) gets added to .gitignore instead;
       everything else is commit-worthy. After branching in step 5,
       commit the commit-worthy pre-existing changes (including any
       .gitignore additions) as their own commit BEFORE this task's own
       commits: `chore: workspace changes picked up before <slug>`, with
       --signoff and the Co-Authored-By trailer, so the task's commits
       stay clean of unrelated diffs. When in doubt whether something is
       junk, commit it.
    3. Set the board Status to In progress. Find the item id via
       `gh project item-list 6 --owner itechmeat --format json --limit
       100` (match .content.url), then `gh project item-edit --id
       <item_id> --project-id PVT_kwHOAo2qi84Bd_bQ --field-id
       PVTSSF_lAHOAo2qi84Bd_bQzhYc6Ek --single-select-option-id
       47fc9ee4`.
    4. Derive a kebab-case slug (3-5 meaningful words) from the task.
    5. Branch from the fresh default branch WITHOUT committing to local
       main: `git fetch origin --prune`, then `git checkout -b
       feat/<slug> origin/main`. If the branch exists from a prior
       attempt, delete it first with `git branch -D feat/<slug>`.
    6. If brainstorming artifact files are present uncommitted, commit
       them on the feature branch as one commit `docs(spec): <slug>
       brainstorming` with --signoff and the Co-Authored-By trailer.

    Do NOT implement anything yet. End your output with exactly:
    <scope>
    slug: <slug>
    branch: feat/<slug>
    issue: <issue number>
    task: <one-paragraph restatement of what to build>
    acceptance: <semicolon-separated acceptance criteria from the issue>
    </scope>
- id: implement
  type: agent_task
  expected_duration: 2h
  profile: developer
  prompt: |
    REPORTING CONTRACT (overrides the generic report instruction apb
    appends AFTER this prompt): end your reply with the fenced yaml
    report block, but the block must contain ONLY the status field
    (status: success or status: failure) and MUST NOT contain a summary
    field, even though the appended generic instruction asks for one.
    When a summary field is present, apb replaces this node's whole
    output with that one line, the run's routing breaks, and downstream
    nodes lose the structured output this prompt requires. Status only,
    never summary, no exceptions.

    PHASE: Implementation, strictly TDD, on the feature branch.

    Scope: {{nodes.lock_scope.output}}

    Follow the design from the brainstorming spec (docs/superpowers/
    specs/, see the scope). Work item by item: failing test first,
    minimal implementation, refactor; atomic conventional commits with
    --signoff and the Co-Authored-By trailer. Single-PR rule: everything
    stays on the one feat branch and ships as ONE pull request; never
    push and never open a PR in this node.

    After all items, run the full gate set and fix what they catch:
    `cargo fmt --all -- --check`, `cargo clippy --workspace
    --all-targets -- -D warnings`, `cargo test --workspace`; when web/
    was touched: `bun run check`, `bun run test`, `bun run build` in
    web/. Then `cargo metadata --format-version 1 >/dev/null` and
    `code-ranker check .`; fix every violation (read `code-ranker docs
    base <ID>` before fixing) and re-run until clean.

    If a blocker cannot be resolved in this node, fail with a precise
    description.

    End with: the commit list, each gate with its real result, and any
    deliberate deviations from the spec with rationale.
- id: review
  type: agent_task
  expected_duration: 30m
  profile: reviewer
  max_retries: 1
  timeout_seconds: 1800
  prompt: |
    REPORTING CONTRACT (overrides the generic report instruction apb
    appends AFTER this prompt): end your reply with the fenced yaml
    report block, but the block must contain ONLY the status field
    (status: success or status: failure) and MUST NOT contain a summary
    field, even though the appended generic instruction asks for one.
    When a summary field is present, apb replaces this node's whole
    output with that one line, the run's routing breaks, and downstream
    nodes lose the structured output this prompt requires. Status only,
    never summary, no exceptions.

    PHASE: Independent review of the full branch diff.

    Scope: {{nodes.lock_scope.output}}
    Implementation report: {{nodes.implement.output}}

    Review `git diff origin/main...HEAD` per your standing rules; verify
    the implementer's claimed gate results by re-running the cheap ones
    (fmt, clippy) and spot-checking the rest; check the changed symbols'
    blast radius; verify the acceptance criteria from the scope are
    actually covered by tests.

    Blockers: fail the node with findings ordered by severity, actionable
    verbatim. No blockers: succeed with a short review summary and any
    non-blocking notes.
- id: fix_review
  type: agent_task
  expected_duration: 45m
  profile: developer
  prompt: |
    REPORTING CONTRACT (overrides the generic report instruction apb
    appends AFTER this prompt): end your reply with the fenced yaml
    report block, but the block must contain ONLY the status field
    (status: success or status: failure) and MUST NOT contain a summary
    field, even though the appended generic instruction asks for one.
    When a summary field is present, apb replaces this node's whole
    output with that one line, the run's routing breaks, and downstream
    nodes lose the structured output this prompt requires. Status only,
    never summary, no exceptions.

    FIX ROUND after review.

    Scope: {{nodes.lock_scope.output}}
    Review findings: {{nodes.review.output}}

    Verify each finding against the code before acting. Apply confirmed
    blockers as additional atomic TDD commits on the same branch
    (--signoff, trailer): failing test first, minimal diff, no rewrites,
    no scope expansion, no pushes. Re-run the gates the fixes touch. If
    a finding is wrong, refute it with evidence in your report instead
    of implementing it. An unresolvable blocker fails the node with a
    precise explanation.

    End with: what you fixed or refuted, and the resulting gate status.
- id: review2
  type: agent_task
  expected_duration: 20m
  profile: reviewer
  max_retries: 1
  timeout_seconds: 1800
  prompt: |
    REPORTING CONTRACT (overrides the generic report instruction apb
    appends AFTER this prompt): end your reply with the fenced yaml
    report block, but the block must contain ONLY the status field
    (status: success or status: failure) and MUST NOT contain a summary
    field, even though the appended generic instruction asks for one.
    When a summary field is present, apb replaces this node's whole
    output with that one line, the run's routing breaks, and downstream
    nodes lose the structured output this prompt requires. Status only,
    never summary, no exceptions.

    PHASE: Second and FINAL review round. Exactly one fix round is
    allowed; remaining blockers abort the run for the owner.

    Scope: {{nodes.lock_scope.output}}
    Original findings: {{nodes.review.output}}
    Fix report: {{nodes.fix_review.output}}

    Verify each original blocker is genuinely resolved in the current
    diff against origin/main (not merely claimed) and that the fixes
    introduced nothing new; re-run fmt and clippy as a spot check. A
    finding refuted with convincing evidence counts as resolved.

    All clear: succeed with a short confirmation. Anything remaining:
    fail with the precise findings.
- id: qa
  type: agent_task
  expected_duration: 30m
  profile: qa
  prompt: |
    REPORTING CONTRACT (overrides the generic report instruction apb
    appends AFTER this prompt): end your reply with the fenced yaml
    report block, but the block must contain ONLY the status field
    (status: success or status: failure) and MUST NOT contain a summary
    field, even though the appended generic instruction asks for one.
    When a summary field is present, apb replaces this node's whole
    output with that one line, the run's routing breaks, and downstream
    nodes lose the structured output this prompt requires. Status only,
    never summary, no exceptions.

    PHASE: QA on the feature branch.

    Scope: {{nodes.lock_scope.output}}
    Implementation report: {{nodes.implement.output}}

    Derive concrete acceptance criteria from the scope block and the
    issue, then verify with executed evidence per your standing rules:
    the full standard pass, the web checks when web/ was touched,
    playbook validation when .apb/ changed, plus targeted functional
    checks of the changed behavior (run the built binary where
    practical). Any failing check or reproduced defect fails the node
    with exact commands, expected vs actual, and output.

    End with: each check and its result, plus the verdict.
- id: fix_qa
  type: agent_task
  expected_duration: 40m
  profile: developer
  prompt: |
    REPORTING CONTRACT (overrides the generic report instruction apb
    appends AFTER this prompt): end your reply with the fenced yaml
    report block, but the block must contain ONLY the status field
    (status: success or status: failure) and MUST NOT contain a summary
    field, even though the appended generic instruction asks for one.
    When a summary field is present, apb replaces this node's whole
    output with that one line, the run's routing breaks, and downstream
    nodes lose the structured output this prompt requires. Status only,
    never summary, no exceptions.

    FIX ROUND after QA.

    Scope: {{nodes.lock_scope.output}}
    QA report: {{nodes.qa.output}}

    Apply exactly the reported failures as additional atomic TDD commits
    on the same branch (--signoff, trailer): failing test first, minimal
    diff, no rewrites, no scope expansion, no pushes. Re-run the touched
    checks before each commit. An unresolvable defect fails the node
    with a precise explanation.

    End with: what you fixed and the resulting check status.
- id: qa2
  type: agent_task
  expected_duration: 20m
  profile: qa
  prompt: |
    REPORTING CONTRACT (overrides the generic report instruction apb
    appends AFTER this prompt): end your reply with the fenced yaml
    report block, but the block must contain ONLY the status field
    (status: success or status: failure) and MUST NOT contain a summary
    field, even though the appended generic instruction asks for one.
    When a summary field is present, apb replaces this node's whole
    output with that one line, the run's routing breaks, and downstream
    nodes lose the structured output this prompt requires. Status only,
    never summary, no exceptions.

    PHASE: Second and FINAL QA round. Exactly one fix round is allowed;
    anything still failing aborts the run for the owner.

    Scope: {{nodes.lock_scope.output}}
    Original QA report: {{nodes.qa.output}}
    Fix report: {{nodes.fix_qa.output}}

    Re-execute every check that failed originally, plus a regression
    pass: `cargo test --workspace` and, when web/ was touched, the web
    checks. Executed evidence only.

    Everything green: succeed with the verdict. Anything red: fail with
    exact commands and output.
- id: docs
  type: agent_task
  expected_duration: 15m
  profile: developer
  prompt: |
    REPORTING CONTRACT (overrides the generic report instruction apb
    appends AFTER this prompt): end your reply with the fenced yaml
    report block, but the block must contain ONLY the status field
    (status: success or status: failure) and MUST NOT contain a summary
    field, even though the appended generic instruction asks for one.
    When a summary field is present, apb replaces this node's whole
    output with that one line, the run's routing breaks, and downstream
    nodes lose the structured output this prompt requires. Status only,
    never summary, no exceptions.

    PHASE: Docs and push-readiness.

    Scope: {{nodes.lock_scope.output}}

    1. Update the documentation the change touched: CLAUDE.md and
       AGENTS.md together when shared guidance changed (mirror rule),
       README.md and docs/ where behavior is user-visible, llms.txt when
       commands changed. Commit with --signoff and the trailer.
    2. Push-readiness: `git diff --stat origin/main...HEAD` shows ONLY
       in-scope files. Do not re-run full suites; only re-check what
       this node itself changed.

    End with: docs touched and the push-ready confirmation.
- id: pr
  type: agent_task
  expected_duration: 10m
  profile: developer
  connectors:
  - { name: github, functions: [create_pull, get_pull], max_calls: 10 }
  prompt: |
    REPORTING CONTRACT (overrides the generic report instruction apb
    appends AFTER this prompt): end your reply with the fenced yaml
    report block, but the block must contain ONLY the status field
    (status: success or status: failure) and MUST NOT contain a summary
    field, even though the appended generic instruction asks for one.
    When a summary field is present, apb replaces this node's whole
    output with that one line, the run's routing breaks, and downstream
    nodes lose the structured output this prompt requires. Status only,
    never summary, no exceptions.

    PHASE: Push and open the PR. This is the FIRST moment anything is
    pushed; all reviews and QA are already done.

    Scope: {{nodes.lock_scope.output}}

    GitHub access: prefer the bound github connector where its functions
    cover the call; the project board (Projects v2) and reading issue
    comments are NOT covered by the connector, use gh CLI for those.

    1. `git push -u origin feat/<slug>`. If a PR for the branch already
       exists, the push updates it; do not create a duplicate.
    2. `gh pr create --base main --title "<conventional summary>"
       --body-file <prepared body>`. Body in English: Summary (1-3
       sentences), What ships (bulleted), Test plan (the actually
       executed checks and results), and the line "Closes #<issue
       number>". One paragraph per line; no em-dashes, no exclamation
       marks, no AI-authorship markers. Do NOT enable auto-merge; the
       owner merges.
    3. Move the board Status to In review: item id via `gh project
       item-list 6 --owner itechmeat --format json --limit 100` (match
       .content.url), then `gh project item-edit --id <item_id>
       --project-id PVT_kwHOAo2qi84Bd_bQ --field-id
       PVTSSF_lAHOAo2qi84Bd_bQzhYc6Ek --single-select-option-id
       df73e18b`.

    End with: the PR number, URL, and base branch.
- id: post_pr
  type: agent_task
  expected_duration: 30m
  profile: developer
  connectors:
  - { name: github, functions: [get_pull, list_check_runs, get_combined_status, comment_issue], max_calls: 40 }
  prompt: |
    REPORTING CONTRACT (overrides the generic report instruction apb
    appends AFTER this prompt): end your reply with the fenced yaml
    report block, but the block must contain ONLY the status field
    (status: success or status: failure) and MUST NOT contain a summary
    field, even though the appended generic instruction asks for one.
    When a summary field is present, apb replaces this node's whole
    output with that one line, the run's routing breaks, and downstream
    nodes lose the structured output this prompt requires. Status only,
    never summary, no exceptions.

    PHASE: Post-PR checks.

    PR: {{nodes.pr.output}}

    GitHub access: prefer the bound github connector where its functions
    cover the call; the project board (Projects v2) and reading issue
    comments are NOT covered by the connector, use gh CLI for those.

    1. Required checks are a HARD gate: poll `gh pr checks <n>` (the
       repo runs a test gate, dist plan, CodeQL, dependency review, and
       DCO). Diagnose failures via `gh run view --log`; fix the root
       cause with the smallest diff (never delete, skip, or weaken a
       test to pass), commit with --signoff and the trailer, push,
       re-poll. At most 3 fix attempts, then fail the node with the
       failing check and the key error line.
    2. Bot review is best-effort, never a gate: wait about 2 minutes,
       then poll `gh pr view <n> --comments` every ~90 seconds for
       external review bots (for example coderabbitai). Address valid
       findings with minimal-diff commits and push; document deliberate
       skips in a PR comment. No review within ~15 minutes, or an
       explicit decline: skip without failing.

    Success: required checks green and bot review addressed or skipped,
    with every fix actually pushed (verify via `gh pr view <n> --json
    commits`). Otherwise fail with details.

    End with: check status and what was fixed or skipped.
- id: merge_gate
  type: human_review
  options:
  - merged
  - abort
- id: finalize
  type: agent_task
  expected_duration: 10m
  profile: developer
  connectors:
  - { name: github, functions: [get_pull, get_issue, update_issue], max_calls: 10 }
  prompt: |
    REPORTING CONTRACT (overrides the generic report instruction apb
    appends AFTER this prompt): end your reply with the fenced yaml
    report block, but the block must contain ONLY the status field
    (status: success or status: failure) and MUST NOT contain a summary
    field, even though the appended generic instruction asks for one.
    When a summary field is present, apb replaces this node's whole
    output with that one line, the run's routing breaks, and downstream
    nodes lose the structured output this prompt requires. Status only,
    never summary, no exceptions.

    FINALIZE. The owner confirmed the merge; verify and clean up. A
    cleanup hiccup must not fail this node; an unmerged PR must.

    Scope: {{nodes.lock_scope.output}}
    PR: {{nodes.pr.output}}

    GitHub access: prefer the bound github connector where its functions
    cover the call; the project board (Projects v2) and reading issue
    comments are NOT covered by the connector, use gh CLI for those.

    1. Verify the PR is actually MERGED: `gh pr view <n> --json state`.
       Not merged: fail immediately, no cleanup.
    2. `git checkout main`, `git pull`; delete this run's feat branch
       locally (`git branch -D feat/<slug> || true`) and on origin
       (`git push origin --delete feat/<slug> || true`, skip if GitHub
       already deleted it).
    3. Verify the issue closed via the PR's Closes line; close it
       manually if not. Move the board Status to Done: item id as
       before, then `gh project item-edit --id <item_id> --project-id
       PVT_kwHOAo2qi84Bd_bQ --field-id PVTSSF_lAHOAo2qi84Bd_bQzhYc6Ek
       --single-select-option-id 98236657`.

    End with: merge verification, cleanup status, issue and board state.
- id: done
  type: finish
  outcome: success
  expected_duration: 3m
  prompt: |
    REPORTING CONTRACT (overrides the generic report instruction apb
    appends AFTER this prompt): end your reply with the fenced yaml
    report block, but the block must contain ONLY the status field
    (status: success or status: failure) and MUST NOT contain a summary
    field, even though the appended generic instruction asks for one.
    When a summary field is present, apb replaces this node's whole
    output with that one line, the run's routing breaks, and downstream
    nodes lose the structured output this prompt requires. Status only,
    never summary, no exceptions.

    Compose the final answer for the owner in the language of the run
    instruction or dialogue: which task shipped (title, issue url), the
    merged PR link, the executed-check evidence in one compact list, the
    board state, and any follow-ups or deliberate skips noted during the
    run.
- id: aborted
  type: finish
  outcome: failure
  expected_duration: 2m
  prompt: |
    REPORTING CONTRACT (overrides the generic report instruction apb
    appends AFTER this prompt): end your reply with the fenced yaml
    report block, but the block must contain ONLY the status field
    (status: success or status: failure) and MUST NOT contain a summary
    field, even though the appended generic instruction asks for one.
    When a summary field is present, apb replaces this node's whole
    output with that one line, the run's routing breaks, and downstream
    nodes lose the structured output this prompt requires. Status only,
    never summary, no exceptions.

    Compose a short answer in the user's language stating that the run
    stopped and why. Quote the failing node's own message verbatim from
    the run context; do not speculate about causes that are not in the
    context. Describe the current state of the branch, PR, and board,
    and what the owner can do next. Your own yaml report block says
    status: success (the run failed; your composition of this answer
    did not).
edges:
- from: start
  to: pick_task
- from: pick_task
  to: brainstorm_child
  condition: { type: output_match, node: pick_task, pattern: "needs_brainstorm: yes" }
- from: pick_task
  to: lock_scope
  condition: { type: output_match, node: pick_task, pattern: "needs_brainstorm: no" }
- from: pick_task
  to: aborted
  condition: { type: node_status, node: pick_task, equals: failure }
- from: brainstorm_child
  to: lock_scope
  condition: { type: node_status, node: brainstorm_child, equals: success }
- from: brainstorm_child
  to: aborted
  condition: { type: node_status, node: brainstorm_child, equals: failure }
- from: lock_scope
  to: implement
  condition: { type: node_status, node: lock_scope, equals: success }
- from: lock_scope
  to: aborted
  condition: { type: node_status, node: lock_scope, equals: failure }
- from: implement
  to: review
  condition: { type: node_status, node: implement, equals: success }
- from: implement
  to: aborted
  condition: { type: node_status, node: implement, equals: failure }
- from: review
  to: qa
  condition: { type: node_status, node: review, equals: success }
- from: review
  to: fix_review
  condition: { type: node_status, node: review, equals: failure }
- from: fix_review
  to: review2
  condition: { type: node_status, node: fix_review, equals: success }
- from: fix_review
  to: aborted
  condition: { type: node_status, node: fix_review, equals: failure }
- from: review2
  to: qa
  condition: { type: node_status, node: review2, equals: success }
- from: review2
  to: aborted
  condition: { type: node_status, node: review2, equals: failure }
- from: qa
  to: docs
  condition: { type: node_status, node: qa, equals: success }
- from: qa
  to: fix_qa
  condition: { type: node_status, node: qa, equals: failure }
- from: fix_qa
  to: qa2
  condition: { type: node_status, node: fix_qa, equals: success }
- from: fix_qa
  to: aborted
  condition: { type: node_status, node: fix_qa, equals: failure }
- from: qa2
  to: docs
  condition: { type: node_status, node: qa2, equals: success }
- from: qa2
  to: aborted
  condition: { type: node_status, node: qa2, equals: failure }
- from: docs
  to: pr
  condition: { type: node_status, node: docs, equals: success }
- from: docs
  to: aborted
  condition: { type: node_status, node: docs, equals: failure }
- from: pr
  to: post_pr
  condition: { type: node_status, node: pr, equals: success }
- from: pr
  to: aborted
  condition: { type: node_status, node: pr, equals: failure }
- from: post_pr
  to: merge_gate
  condition: { type: node_status, node: post_pr, equals: success }
- from: post_pr
  to: aborted
  condition: { type: node_status, node: post_pr, equals: failure }
- from: merge_gate
  to: finalize
  condition: { type: review_status, equals: merged }
- from: merge_gate
  to: aborted
  condition: { type: review_status, equals: abort }
- from: finalize
  to: done
  condition: { type: node_status, node: finalize, equals: success }
- from: finalize
  to: aborted
  condition: { type: node_status, node: finalize, equals: failure }
```

Notes for the implementer:

- If `{{nodes.brainstorm_child.output}}` in lock_scope fails validation
  because the node may be skipped on the direct path, replace that template
  reference with a plain instruction to read the issue (the sub-run answer
  is also visible in the issue after the child run); do the same kind of
  minimal adjustment for any other V-error, and report every deviation.
- If `schema: 2` is rejected by the validator, drop the field (schema 1
  with profiles is what current playbooks use); report it.
- Both finish-with-prompt nodes may need an explicit profile if the
  validator demands one; bind `facilitator` (apb-task-brainstorm) and
  `developer` (apb-task-implement).
