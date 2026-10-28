# Repeats in session history: `apb discover`

You often do the same thing in several agent sessions before you think of
it as a routine: run the tests, fix the code, commit, push; or search the
mail, read a thread, update a CRM record. `apb discover` reads the history
your coding agents already keep, finds the step sequences that repeat, and
offers each one as a playbook draft you can save, decline for now, or
decline for good.

Discovery runs only when a person asks for it. It reads only the history
of the projects it is given, within fixed bounds, and makes no model call
and no network call. It keeps nothing of a session but what a suggestion
shows: argument-free step names, a short scrubbed example of what was
asked, and where the repeat was seen.

## What it reads

A **history root** is a directory one agent keeps its sessions in:

| Agent | Default root | Session files |
|---|---|---|
| Claude Code | `$CLAUDE_CONFIG_DIR/projects`, else `~/.claude/projects` | `<encoded project path>/<session>.jsonl`, main sessions only |
| Codex | `$CODEX_HOME/sessions`, else `~/.codex/sessions` | `YYYY/MM/DD/rollout-*.jsonl` |

Every surface names a root by its name, never by its path. The defaults are
named after the agent (`claude_code`, `codex`). To read other directories,
name them in the global config; the list then replaces the defaults:

```yaml
# <config dir>/config.yaml
discover:
  roots:
    - { name: claude_code, agent: claude_code, path: ~/.claude/projects }
    - { name: codex_work, agent: codex, path: ~/work-home/.codex/sessions }
  since_days: 90
```

A root name is `[a-z0-9][a-z0-9_-]*`, at most 64 characters; `agent` is
`claude_code` or `codex`; `path` is absolute or starts with `~/`;
`since_days` is 1 to 365. A wrong section stops the config from loading,
as any other.

A **project** is a registered workspace (`apb projects`). A session
belongs to the deepest registered project its `cwd` is in, and is read
only when that project is one of the selected ones: a project registered
inside a selected one keeps its sessions (and the secrets its scrubber
knows) to itself. A Claude Code session is looked at when its directory
is a selected project's encoded path (or one below it), then confirmed by
its own `cwd`; a Codex rollout when the `cwd` its first 64 KiB name
belongs to a selected project. Codex keeps every project's rollouts in
one tree, so another project's rollout is opened and closed right after
that header: its header bytes count toward the byte bound, the file not
toward the file bound. A Claude Code file is read whole before its `cwd`
can confirm it, so it counts toward both.

## Bounds

| Bound | Value |
|---|---|
| Age | sessions written in the last `since_days` days (default 90), by modification time and, for Codex, by the date in the path |
| Files | 2000 session files of the selected projects in one discovery |
| Bytes | 512 MiB in all; 32 MiB of one file (a longer file is read by its start and counted as cut) |
| Lines | a line over 4 MiB is skipped; lines are parsed one at a time |
| Steps | 2000 per session, 100000 in all (what the miner holds in memory); a session the total stops part way counts as cut |
| Order | the session files of all roots together, newest first, so the bounds keep the most recent sessions whichever root holds them |
| Depth | 2 levels under a Claude Code root, 4 under a Codex root |
| Links | a root that is a symbolic link is refused; a linked directory, a linked file or a non-regular file (a fifo) is skipped, never followed |
| One at a time | a second discovery while one runs is refused (`busy`) |

What was read is reported per root: files seen and read, bytes read, files
cut, and files skipped by why (`symlink`, `not_regular`, `too_old`,
`other_project`, `file_cap`, `byte_cap`, `step_cap`, `unreadable`, and
lines skipped as `line_too_long`). Sub-agent sessions are not read yet:
one session with ten sub-agents must not look like ten repeats.

## Steps

Each tool call becomes a step token with no argument in it:

| Token | From |
|---|---|
| `git:commit`, `gh:pr.create`, `cargo:test`, `bun:test`, `apb:connector.call` | a shell command with a known program and one of its known subcommands (git, gh, cargo, npm, pnpm, bun, docker, kubectl, make, uv, apb) |
| `git:other` | a known program whose subcommand is not on its list, or comes after an option the list does not know |
| `sed:inplace` | an in-place edit (`sed -i`, `perl -pi`, `awk -i inplace`): it acts and writes files |
| `sh:<program>` | any other shell command (`cd`, `ls`, `pwd`, `echo`, `true` and a `command -v` lookup are dropped) |
| `edit:.rs`, `read:.md` | a file edit or read, by the file's extension |
| `search` | a grep or glob |
| `mcp:<server>.<tool>` | an MCP tool call |
| `web:fetch`, `web:search` | a web call |
| `agent:spawn` | a sub-agent |
| `skill:<name>` | a skill used or its `SKILL.md` read |
| `tool:<name>` | any other tool, by name |

Every name in a token is lowercased and must match `[a-z0-9._-]{1,40}`,
else it becomes `other`. A subcommand is named only when it is on its
program's list, after the options the list knows with their values
(`kubectl -n prod get` is `kubectl:get`); any other word is `other`
(`kubectl --token <value> get` is `kubectl:other`), so a token cannot
carry a path, a value or a secret. A command line is split on `&&`, `;`,
`|`, `||` and line breaks (a redirection such as `2>&1` stays in its
command), and `bash -lc`, `sh -c` and the wrappers `sudo`, `env`, `time`,
`nohup`, `command` and `exec` are unwrapped first, with the options each
is known to take; after an option a wrapper is not known to take, the
program is `other`. Consecutive identical steps count once.

## Finding repeats

The miner (`ngram/1`) takes the contiguous step sequences of 3 to 8 steps
in each session and keeps one when it appears in at least 3 sessions
written on at least 2 days. Of sequences seen in the same sessions only the
longest stays. A sequence needs two distinct steps and one that acts (an
edit, a command that is not read-only, an MCP or web call, a sub-agent): a
routine of reads alone changes nothing a playbook would do. The score is
the sessions times the steps minus one, and the 20 best are offered. The
same history always gives the same suggestions in the same order.

## A suggestion

| Field | What |
|---|---|
| `slug` | `seq-` and 10 hex characters of the steps: the same steps always get the same slug |
| `steps`, `title` | the tokens, and a title made of them |
| `support` | sessions, days, and the projects by workspace id and name (two projects may share a name; the id tells them apart) |
| `effects` | `fs_write` for an edit; `network` and `external` for a push, `gh`, an MCP or web call or a connector call; `irreversible` when any occurrence ran a destructive command |
| `examples` | up to three requests typed right before an occurrence, from distinct sessions |
| `sources` | up to ten places: the root name, `<project>/<file name>`, the project's workspace id and a line range |
| `yaml` | a schema 2 draft: a `task` parameter and one `agent_task` that takes the steps in order, with the first profile of the project it was seen in first (or `main`, noted as `needs_profile`); its trigger names the steps, and one too long for a trigger line ends `, and more (<slug>)` so repeats that share their first steps keep distinct triggers |
| `status` | `new`, `saved` (playbook, version, scope) or `dismissed` |

An example is what a person typed: the text an agent CLI adds to a turn
(reminders, environment notes, an instruction file such as codex's
`AGENTS.md` message, shell-mode echoes and their output, the summary of a
compacted conversation, an interrupt marker) is never one. It goes through
the project's scrubber (secret-shaped text, values of configured connector
accounts, the project root, the home directory as `~`, e-mail addresses),
every other absolute path in it becomes `[path]`, and it is cut to 240
characters. An example that looks like it holds a secret before or after
the scrub is dropped whole, and a draft whose text still does is dropped
and counted (`dropped_drafts`).

## Saving and declining

A suggestion is saved through the same path as `playbook_capture`: the
secret check, the duplicate check (an existing playbook with the same
trigger), and a draft that runs only after a trial or an approval. Its
provenance says `created_by: discover`, with the slug (`pattern`) and the
result id. A host may rewrite the draft first (`yaml`), for a better title
or prompt; the provenance then says `edited: true`. The suggestion's own
draft is saved naming the first profile of the place it is saved in: the
project's (else the machine's) for a project playbook, the machine's for a
global one; with none there it keeps the profile it was drafted with.

A save or a decline runs once: under the discovery store's lock the
suggestion is checked to be new, the action runs, and its status is
recorded in the result. When that last record fails, the save or decline
stands and the answer carries a `warning`.

A decline is recorded where `suggestion_dismiss` records it: `soft` (not
now: the silence grows with each decline) or `hard` (never, for one long
window), in the project or for every project. Later discoveries leave the
suggestion out by its slug when a decline of it holds in a project the
repeat was seen in, or for every project, and list it under `suppressed`
(`kind` `soft` or `hard`). A saved suggestion is recorded by its slug and
where it was saved (the project's workspace id, or every project) in the
discovery store, whatever id the playbook was given. Later discoveries
list it with `kind` `saved` when it was saved globally or in a project the
repeat was seen in (so is one whose slug is a playbook in such a project
or the global store); another project, which cannot run that project's
playbook, is still offered it. Deleting the playbook does not bring it
back. A host also gets the person's
declines that hold now (`suppressed_suggestions`), with a fresh discovery
and with a stored one, to compare by meaning.

## The stored result

Each result is stored as `<config dir>/discover/<id>.json`, owner-only,
signed with this installation's key; the newest 10 are kept. A result that
is not signed here (copied, edited) can be listed and read, but nothing is
saved from it. The slugs saved as playbooks, each with where it was
saved, are kept beside the results (`<config dir>/discover/saved.json`). Without a config directory the
result is only returned.

## What is never read or stored

- No file outside the named roots, no linked file, no other project's
  session past its header, no sub-agent session.
- No tool argument: no command line, path, query or file content.
- No model output and no tool result.
- No absolute path in anything returned over MCP or HTTP, and none in the
  stored result: sources and projects are named, and an absolute path in
  an example is `[path]`. Only `apb discover show <id> --paths`, at your
  own terminal, prints where a source file of a configured root lies; a
  `--sessions` directory's path is not kept, so its sources are not
  located.
- Only counts are logged, never session text.

## Surfaces

| Surface | What |
|---|---|
| CLI | `apb discover [--project <workspace id or dir>]... [--all-projects] [--root <name>]... [--sessions <dir> --agent claude-code\|codex] [--since 30d] [--json]`; `apb discover list`; `apb discover show <id> [--paths]`; `apb discover save <id> <slug> [--scope project\|global]`; `apb discover dismiss <id> <slug> [--hard] [--global]`. A `--project` value is a registered workspace id first, else a directory. `--sessions` is the one way to name a directory that is not a configured root, typed by a person; it is read instead of the configured roots (not with `--root`), under the root name `@sessions`. A refused save or decline exits 2, with `--json` too |
| MCP | `playbook_discover`, `playbook_discover_results`, `playbook_discover_save`, `playbook_discover_dismiss` (docs/MCP.md). A host calls them only when the user asks to look for repeats |
| HTTP | `GET /api/discover/roots`, `GET /api/discover`, `GET /api/discover/{id}`, `POST /api/discover` with `{ workspaces, roots, since_days? }`, `POST /api/discover/{id}/{slug}/save?workspace=` with `{ scope }`, `POST /api/discover/{id}/{slug}/dismiss?workspace=` with `{ kind, scope? }`. Without `workspace` on the machine-wide dashboard, a project-scope save or decline lands in the project the suggestion was seen in first, by its workspace id. See "Who the HTTP API answers" below; a body with any other field (a path) is 422; a second discovery while one runs is 409 `busy` |
| Dashboard | the Discover page (`#/discover`): pick projects, roots and days, run, then save, decline for now or never per card |

Every discovery entry, reading ones included, is refused when it runs as a
step of a run (`APB_RUN_ID` or `APB_HOST_TASK` set): reading a person's
session history is the person's act. The CLI and MCP check the caller's
own environment. The HTTP API checks the dashboard server's environment:
an agent of a run step that reaches a dashboard on this machine is not
told apart from a person there (it could read the history files directly
anyway).

### Who the HTTP API answers

Before it reads a request body, every discovery route refuses with 403
`discover_local_only` a request that does not come straight from this
machine: a peer off the loopback interface, a peer that is a configured
trusted proxy, any forwarding header (`Forwarded`, `X-Forwarded-For`,
`X-Forwarded-Host`, `X-Forwarded-Proto`, `X-Real-IP`, `Via`,
`CF-Connecting-IP`, `True-Client-IP`), a missing `Host` or one that is not
a loopback name (`localhost`, `127.0.0.1`, `[::1]`), and an `Origin` that
is not one. A forwarder on this machine that adds no header and keeps the
`Host` (an ssh tunnel, a raw TCP relay) cannot be told apart from a local
client: exposing the dashboard that way exposes this API too.

## Example: code work

Over a few weeks you asked your agent several times to fix a failing test
and ship it. The sessions each ran the tests, read a skill, edited a Rust
file, ran the tests again, committed and pushed:

```text
$ apb discover --since 30d
discovery disc-3f1a9c0b2e over app (30 days): 14 sessions
  root claude_code (claude_code): 14 files read of 31, 2201733 bytes, 0 cut, skipped: other_project 17

seq-3c15c22849 [new] Repeated steps: cargo test, skill rust-skills, edit .rs, cargo test, git commit, git push
  seen in 4 sessions on 3 days (app), score 20
  steps: cargo:test > skill:rust-skills > edit:.rs > cargo:test > git:commit > git:push
  effects: fs_write, network, external
  asked: Fix the failing parser test and ship it
  source: claude_code:app/5d1e....jsonl:212-240
```

`apb discover save disc-3f1a9c0b2e seq-3c15c22849` makes a draft playbook
`seq-3c15c22849`; rename it, tighten its prompt and add a review gate
before the push (later discoveries still leave the suggestion out). Like any captured draft it runs only after a trial
(`playbook_trial`) or an approval (docs/MCP.md, "Run policy and trust").

## Example: mail and CRM

A support routine in Codex: search the inbox for a customer, read the
thread, update the customer's CRM record. The steps are MCP calls, so the
suggestion reads:

```text
seq-91b0e4d7aa [new] Repeated steps: mcp gmail search, mcp gmail read, mcp crm update
  seen in 5 sessions on 4 days (support), score 10
  steps: mcp:gmail.search > mcp:gmail.read > mcp:crm.update
  effects: network, external
  asked: Log the reply from the customer in the CRM
```

Nothing of the mail or the record is kept: only the tool names. Saved, the
draft is a starting point for a playbook that calls the same services
through connectors (docs/CONNECTORS.md), with the CRM update behind the
irreversible consent if your connector declares it so.
