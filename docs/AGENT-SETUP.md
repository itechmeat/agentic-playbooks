# Agent setup: installing and adapting apb for a user

This is the procedure for a coding agent that a person asked to install apb
and set it up for them. The same text is served by the MCP tool
`setup_howto`. Everything here is local, listed in a plan before it happens,
recorded, and undone by one command.

## What setup does and does not do

`apb setup apply` may do these on the person's install request, because they
are local, listed in the plan and reversible with `apb setup remove`:

1. Install the default playbooks apb ships (ids starting with `apb-`) into the
   global store. Only their exact shipped digests are trusted.
2. Install the process skills apb ships (`apb-usage`, `apb-brainstorm`,
   `apb-delivery`, `apb-diagnosis`, `apb-review`, `apb-test-audit`,
   `apb-security-audit`, `apb-model-selection`) into the global skills directory
   (`~/.agents/skills`, linked into `~/.claude/skills`).
3. Write the role profiles the defaults bind to (`apb-main`, `apb-reviewer`,
   `apb-researcher`) from the agents this machine actually has and the
   models table. The reviewer is another agent when one is usable; with a
   single agent it is the same agent in a fresh session, and the plan says
   so (`same_agent_reviewer`).
4. Register the MCP server `agentic-playbooks` in every detected host:
   Claude Code through its own CLI at user scope, Codex and OpenCode by
   adding one entry to their config file (the original is backed up first,
   nothing else in the file changes). A config that is a symbolic link (a
   dotfiles folder) stays a link: its target is edited and keeps its
   permissions. When the entry cannot be added without changing anything
   else (for example a Codex `mcp_servers` written as an inline table), setup
   writes nothing there and lists a `manual` step with the exact text to
   add by hand.
5. Register the end-of-turn save check `apb hook stop` as a Claude Code
   Stop hook (`register_hook`, target `claude`): one hook group inserted
   into `hooks.Stop` of the user settings (`~/.claude/settings.json`, or
   `settings.json` under `CLAUDE_CONFIG_DIR`), the original backed up first,
   every other key kept in place, a symlinked file kept a link. When the
   agent ends a turn that looks like a repeatable process and no playbook
   tool was used, the hook reminds it once per session to check the
   catalog and offer to save it (see `docs/HOST-INTEGRATION.md`
   "End-of-turn check"). `APB_SAVE_NUDGE=off` in the environment (for
   example the `env` key of the same settings file) silences it; `apb setup
   remove` unregisters it, and must run before apb is uninstalled (the hook
   runs `apb` from PATH). The hook sends nothing anywhere unless the person
   turns on its decision-model question (`needs_person`
   `save_nudge_model`). A settings file that does not parse or has a
   `hooks` shape apb does not edit gets a warning
   (`hook_settings_unreadable`), and a hook that already runs apb in another
   form is left alone (`hook_entry_differs`). Codex and OpenCode have no
   such hook; the standing block below is their channel.
6. With `--project <dir>`: write the standing playbook instruction into that
   project's `CLAUDE.md` and `AGENTS.md`. When one of them is a link to the
   other (`CLAUDE.md -> AGENTS.md`), the block is written once, into the
   file inside the project, and the link stays. A link that leads out of the
   project is refused: the plan warns (`standing_block_link`) and nothing is
   written.
7. With `--replace-modified`: replace a default the person changed with the
   shipped content (see "Upgrades").

Setup never does these; they need the person's own decision, and the plan
lists them under `needs_person`:

1. Lowering a launch floor (`apb launch floor <id> --lower-to <policy>`, at a
   terminal).
2. The feedback-loop block and filing issues about apb. Only the person turns
   it on, at a terminal: `apb init` asks, and `apb init --feedback-loop yes`
   asks once more there. Without a terminal, or inside a run step, `--feedback-loop yes`
   is refused with exit code `3` and writes nothing; `--feedback-loop no`
   works anywhere.
3. Notification targets (`apb notify --via ... --to ...`): they send messages
   to people.
4. Reviewing an adapted copy of a default before it runs. A copy an agent
   saves is approved as locally authored, like any playbook an agent writes
   through MCP. It never gets the shipped trust or the automatic upgrades,
   and it cannot use an `apb-` id. The person checks it with
   `apb adopt <id> --scope global` and can withdraw the approval with
   `apb trust revoke`.

Inside a run step (`APB_RUN_ID` or `APB_HOST_TASK` set) `apb setup apply`
and `apb setup remove` are refused: exit code `3` with `refused: run_step`.
Installing and configuring apb is the person's or their own agent session's,
never a playbook step's. The plan only reads, so `apb setup plan` and the MCP
tool `setup_plan` answer in a step too (the `apb-setup` playbook reads it).

## What apb never requires

The process needs only the project's own checks and review by agents. apb
never asks for a third-party tool to be installed, and setup never installs
one. A tool the project itself declares among its checks (its test, lint or
build commands) is required for that project, because the project says so.
A review helper already installed on the machine is required too (see
below).

### Review helpers

`apb setup plan` lists `suggestions`: review helpers, each with its `role`,
`why`, how to `check` it is there, a few `examples` (named only as
examples), the examples `found`: on PATH (an absolute entry, never the
current directory), with a canonical path outside the working directory,
the project and the git checkout each sits in (its root and a worktree's
main checkout; a checkout rooted at the home directory is the operator's
own), so a program the checkout ships is never run, and answering their
version check, and `required`.
Setup runs each one it finds with `--version` the way agent detection
probes an agent (sanitized environment, a time limit); a name other
programs share must answer as the helper itself (`ocr` must name
open-code-review), else it stays optional. Two roles today: a
code-review CLI that gives a second, independent reading of a diff, and a
structural code checker that flags dependency cycles, complexity and
structural rules.

- `required: true`: a helper of that role is installed and answered its
  version check. The `apb-review`
  skill and the shipped review playbooks then run it as a mandatory step of
  every review cycle. A helper that fails to run (a crash, a missing login,
  a broken configuration) is a defect to fix, never a reason to fall back
  silently to a review without it.
- `required: false`: nothing of that role is installed, or what is
  installed under its name did not answer as the helper. It stays an
  optional suggestion: mention it to the person if it fits; never install
  one on your own.

### Work trackers

`observed.trackers` names the work trackers the project seems to use, for
information only: a `.beads/` directory, a git remote on GitHub, an
installed connector with an account and issue functions, each with `status:
in_project`. `bd` on PATH in a project without `.beads/` is reported with
`status: available`: a program on this machine, not this project's tracker.
When the
project uses a tracker (GitHub issues, Beads, Linear or similar, named only
as examples), the delivery skill puts subtasks and status there; otherwise
into the task record. apb depends on none of them.

## Procedure

1. **Install.** Use the installer from `llms.txt`, then check the binary:
   `apb --version`.
2. **Look at the machine.** Run `apb doctor --json`. Every check has
   `status` (`ok`, `warn`, `fail`), `subject` and `detail`; `ok` is false
   when something blocks. Fix blocking problems before going on.
3. **Read the plan.** Run `apb setup plan --json` (add `--project <dir>` for
   the project the person works in). It writes nothing. Read:
   1. `observed.agents`, `observed.subscriptions`: what can run work;
   2. `observed.playbooks`, `observed.skills`: each default's `state`
      (`absent`, `up_to_date`, `pristine`, `modified`, `foreign`);
   3. `observed.role_profiles`: the planned agent and model per role and
      `independence`;
   4. `observed.hosts`: each MCP host, `detected` and `registration`;
   5. `observed.sandbox`: whether `isolation: full` can run here;
   6. `observed.trackers`: the work trackers seen (information only);
   7. `actions`: what apply would do, each with `kind`, `target`,
      `reversible` and `undo`;
   8. `manual`: host entries the person adds by hand, with the `snippet`;
   9. `suggestions`: review helpers; the installed ones are `required`
      in every review cycle (setup never installs one);
   10. `warnings`, `refusal`, `needs_person` and `digest`. The digest also
       covers the host programs apply starts (the Claude Code CLI's path), so
       another binary on PATH is another plan.
4. **Ask the person, at most five short questions,** only what the plan
   cannot answer:
   1. What do you want playbooks for (goals and domains: code, research,
      routine work such as mail or documents)?
   2. Which of the detected agents may apb use?
   3. How careful should runs be with spend (a budget per run, or ask
      first)?
   4. May runs write outside the repository?
   5. Do you want to be notified when a run waits for you or ends?

   If the person already said it, do not ask again. A missing answer is never
   a yes.
5. **Apply the plan.** Run `apb setup apply --plan <digest> --json` with the
   digest you read, and the same `--project` and `--replace-modified` you
   read the plan with. Exit codes: `0` done (a second apply of the same plan is
   a no-op with `0`), `1` an action failed (the others ran and are
   recorded), `2` the plan changed since you read it (nothing written: read
   it again; after a partial failure the plan changes too, so read it again
   and apply the new digest), `3` refused (for example no usable agent:
   install an agent CLI, or run playbooks in host mode; or a run step).
6. **Adapt to the answers.** Never edit a default (`apb-*`) in place. To
   change one, save a copy under the person's own id (for example
   `playbook_get` with `detail: full`, then `playbook_create` with a new id),
   and adjust profiles with `profile_write` under the person's own names.
   Budgets, isolation and launch policy go into those copies. A copy is
   approved as locally authored, like any playbook an agent saves; it never
   gets the shipped trust or automatic upgrades, and it cannot take an
   `apb-` id. Tell the person which copies you saved, so they review them.
   `apb-issue-triage` reads and labels issues through the official `github`
   connector: the person installs it (`apb connector install github`), adds
   an account and approves it.
7. **Verify.** Run `apb adopt <id> --scope global` for the playbooks the
   person will use (the defaults live in the global store; `apb validate`
   reads only the project's), then one dry trial (`playbook_trial`) of the
   main one.
8. **Report** to the person: what was installed and registered (from
   `done` of the apply), the warnings (an update available, a reviewer on the
   same agent), what still needs them (`needs_person`, with the answers they
   gave), and that `apb setup remove` undoes setup.

## Undo

`apb setup remove [--json]` takes back what the setup record holds
(`<config_dir>/setup/record.json`). A default a replace
(`--replace-modified`) took over goes back to the person's copy instead of
going away, listed under `restored`: a skill's changed copy comes back from
its backup folder (and is the person's own from then on), and a playbook's
`current` moves back to the person's version, while the shipped copy is
still unchanged since the replace.

1. default playbooks go to the global trash (restorable);
2. skills and role profiles are removed only while unchanged since setup
   wrote them;
3. a host registration is removed only while it is exactly the entry apb
   wrote (a Codex table a person added keys to is kept, since cutting it
   would move those keys to another table); Codex and OpenCode backups stay
   in `<config_dir>/setup/backups`. A host config is written only while it
   still holds what setup read, and a host CLI command is stopped after two
   minutes;
4. the end-of-turn hook is removed only while it is exactly the group apb
   wrote; a settings file setup created and that holds nothing else is
   deleted;
5. the standing block is removed only where its bytes are unchanged, from the
   file it was written into; a link is never replaced by a file.

Anything changed since, and every copy the person made, is kept and listed
under `kept`.

## Upgrades

After an upgrade of apb, an unmodified default playbook or skill moves to
the new shipped content (with a history line, so `apb version use` rolls it
back) right after `apb self-update`, when `apb mcp` or the dashboard starts,
or on the next `apb setup apply`. `apb self-update` and `apb mcp` do this
only on a machine where setup was applied, and `apb mcp` reports what it
did on stderr. A default is never moved back to older content than the
newest shipped version installed. A changed one, or one a person moved to
another version, is never replaced on its own: `apb doctor` and
`apb setup plan` report an update as available and name the option.

To take the shipped content over a changed default, on the person's
request: `apb setup plan --replace-modified <name>` (several names separated
by commas; no name means every changed default, which apply takes only from
a person at a terminal) adds a
`replace_modified_playbook` or `replace_modified_skill` action; apply it with
`apb setup apply --plan <digest> --replace-modified <name>`. A playbook gets
the shipped content as a new version and keeps the changed one in its
history (`apb version use <id> <version> --scope global` returns to it). A
skill's changed copy is moved aside to a backup folder in the skills
directory. The apply output names both under `backup`, and `apb setup
remove` gives either back.
