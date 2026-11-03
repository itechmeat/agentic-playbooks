---
name: apb-usage
description: How a host agent uses apb to find, run, follow and supervise playbooks, and when to offer saving a repeated process as a playbook. Use when a task names a doable action that a playbook may cover, when a run waits for input, or when the user asks about apb.
metadata:
  version: "1.1.0"
---

# Using apb

apb runs playbooks: graphs of steps with checks, gates, retries and resume.
You are the host. You find the right playbook, start it, follow it, relay the
decisions only a person may take, and report the outcome.

The MCP tool descriptions are the reference for arguments and answers. This
skill only says when to use which tool. If a tool is not listed in your
session, use the matching `apb` command instead.

## 1. Discover

- Call `playbook_catalog` once per task that names a doable action, before
  you act. Skip it for chit-chat.
- Read `trigger`, `effects` and `trust` of each match.
- A confident match to an active, trusted playbook: name it in one line and
  run it.
- An ambiguous match: ask one short question.
- A playbook of another project: confirm with the user first.

## 2. Start

- Start with `playbook_run` (CLI: `apb run`).
- A refusal names what is missing. Common ones:
  - untrusted playbook or profile: ask the person, then retry with the
    acknowledgement the refusal asks for;
  - launch policy `ask`: ask the person, then retry with the launch nonce,
    version and choice the refusal named;
  - irreversible effects: ask the person, then retry with the consent nonce;
  - billed use (`billing_requires_confirmation`): the run would start agent
    work that is billed, or whose basis is unknown. Name the agents and
    bases the refusal lists, ask the person, and retry with the launch
    nonce only after an explicit yes;
  - `billing_denied`: report it; only the person changes the settings or
    the profile;
  - a missing param: ask for the value.
- Never invent a consent. Never retry with a nonce the person did not approve.
- Never start billed agent work without the person's explicit command for
  this run. Approval of another run, another playbook or an earlier version
  does not carry over. Some people accept billed work and some never do;
  ask every time apb refuses.
- Effects beyond what the user asked for (network, secrets, deploys,
  irreversible steps) need a confirmation even when apb does not refuse.

## 3. Follow

- Follow a background run with `run_wait`. Do not poll `run_status`.
- `run_wait` returns when the run ends or needs input:
  - `pending_review`: relay the instruction and the options in the user's
    language, then record the answer with `review_decide`. The run is frozen
    until then.
  - a pending question: relay it verbatim and answer with `run_answer`.
  - `pending_tasks` (host execution mode): see section 4.
  - a pause (quota or budget): report it and the next action apb names.
  - a billing pause (`billing (<agent>: <basis>)`): the run stopped before
    agent work it has no confirmation for. Ask the person; resume with the
    nonce of the `billing_requires_confirmation` refusal only after an
    explicit yes.
- `next_actions` in an answer are ready commands. Prefer them over guessing.

## 4. Host execution mode

- Use `execution: host` only when the person asks for it (single agent,
  host mode, or "use your own subagents").
- Every agent step becomes a host task. Run each one with your own subagent
  tool in this session, with the task's prompt, role prompt, skills and
  working directory.
- Never launch another agent CLI for a host task. Never use flags that bypass
  permissions.
- Submit the subagent's final reply verbatim with `run_task_submit`.
- List nodes do not run in host mode. apb refuses such a run at start.

## 5. Supervise

- Supervise only when the user asks you to, or the run was started as
  supervised.
- Watch with `supervisor_wait_event`. Act with the `supervisor_*` tools:
  add context, retry a node, rebind a profile, pause, or abort.
- Never weaken a goal criterion. Only a person may change it.
- Never answer a question meant for a human on your own judgment.

## 6. Report

- Report the outcome from `run_report` or the final `run_wait` answer.
- Keep apart what apb checked (goal criteria, list counts, baseline) and
  what nobody checked (manual criteria).
- Report sends with an unknown outcome. Only a person resolves them.

## 6a. Clean up

The work a run or you did leaves things behind, in code work and in any
other routine. Before you report done, and after a merge:

- Remove the worktrees and merged branches of a finished change (in git:
  `git worktree remove`, `git branch -d`, `git worktree prune`) and the
  build output they used. A squash-merged branch is not reachable from the
  base, so `git branch -d` refuses it: use `git branch -D` only after you
  checked that the base holds its work. Keep a worktree a paused run or an open pull
  request still needs.
- Delete the temporary files, logs, downloads, browser profiles and
  sessions the task created. Keep what must survive in a named cache
  directory.
- Create temporary data under one parent directory named for the task and
  remove it on exit, also on failure.
- Delete only what the task created, never by a wide pattern: other
  sessions may share the machine. Leave apb's own run records alone.
- Stop the background processes the task started.
- Prune old build artifacts rather than wiping a build cache (a build cache
  pruner such as cargo sweep for Rust projects).

## 7. Offer to save a playbook

- After you finished a multi-step action that will recur, and no playbook
  matched, offer once to save it with `playbook_capture`. One short question,
  project scope first when it is project specific.
- First compare with `suppressed_suggestions` by meaning. A covering record
  means no offer.
- A decline without "never": `suggestion_dismiss` with kind `soft`.
- When the user describes a process to automate, offer a short interview
  (`playbook_interview`).

## 8. Where to read more

- Authoring playbooks: `playbook_howto`.
- Profiles (agent, model, fallbacks, role prompt, skills): `profile_howto`.
- Setting up or re-adapting apb on this machine: `setup_howto`, or run the
  `apb-setup` playbook.
- Every command and its flags: `apb --help` and `apb <command> --help`.

## Rules

- Speak the user's language. Machine fields stay in English.
- Do not change trust, launch floors, notification targets or consents on
  your own. A person decides those.
- A run's journal proves what ran, not that the result is good.
