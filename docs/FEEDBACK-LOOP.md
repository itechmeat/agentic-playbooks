# apb feedback loop: reporting instruction

This is the full reporting instruction behind the short feedback-loop section
that `apb init` adds to a project's `CLAUDE.md` and `AGENTS.md`. It tells a
coding agent how to report apb problems observed during supervised playbook
runs.

## What to collect

On every playbook run you supervise, collect flaws, bugs, and improvement
ideas that belong to apb itself: the MCP server, the playbook engine, the
validator, the supervisor tools, the run lifecycle. Keep those separate from
problems that belong to the project's own playbooks, profiles, or prompts,
which are the project's to fix and do not belong in an apb issue.

## When and how to report

When the run is finished (PRs created, run closed) and the list is not empty,
first search the open issues at
https://github.com/itechmeat/agentic-playbooks/issues for each finding.
Comment on an existing issue when one already covers it. File the remainder
as ONE consolidated issue per run, in English, with `gh issue create --repo
itechmeat/agentic-playbooks`.

## What each finding should carry

- the symptom, stated concretely, with the exact error text or tool output
- the apb version (`apb --version`) and how the run was driven (MCP or CLI)
- what you expected instead, and why
- the reproduction context: the node kind, the playbook shape that triggered
  it, and whether it survived a resume
- a suggestion, when you have one

Attach the run id and any relevant lines from `runs/<id>/events.jsonl` when
they make the report reproducible. Never include secrets, tokens, or private
prompt content in an issue.
