# asana: setup for humans

## The short way: let an agent do it

Setting this connector up by hand means editing two files, creating a token, and approving trust. An agent can do all of it for you and will only stop to ask for the token, which is the one thing it cannot obtain on your behalf.

Paste a prompt like this to a coding agent that has `apb` available:

> Install the apb `asana` connector for my account, then read `connectors/asana/INSTALL.md` under the apb config directory (by default `~/.config/apb`) and follow it to the end. Ask me for the token when you get there.

The agent installs the connector, writes the account config, prepares the secrets file, approves trust, and runs a live healthcheck that asks Asana who you are. What you get back is either a working account or a specific error.

## What you will be asked for

A personal access token. In Asana, open your profile settings, go to Apps, then Developer apps, and create one.

There is one thing worth understanding before you do: the token acts as you, with all of your permissions, and Asana offers no way to narrow that down. There are no scopes to pick. Anything you can see or change in Asana, a playbook holding this token can see or change. That is a property of Asana's tokens, not of this connector, and the way to limit it is to grant only the functions a node needs.

The token is written to a local file with owner-only permissions, and the account config next to it stores only a reference to it, never the value.

## What this connector can and cannot do

It covers task work: listing workspaces, projects, and sections, listing and reading tasks, creating and updating them, comments, subtasks, moving a task into a section, and a fuzzy task search.

Workspace, project, section, and task ids are call arguments rather than account settings, so one account reaches every workspace your token can see.

It does not manage custom fields, portfolios, goals, or team membership, and it cannot delete a task.

One thing to know about search: `search_tasks` is Asana's typeahead, a fuzzy match against task names, not a full-text search over descriptions and comments. When a complete and predictable result set matters, listing a project's tasks is the reliable route.

Worth knowing before you grant it: `create_task`, `create_subtask`, `update_task`, `add_comment`, and `add_task_to_section` all write into a board your colleagues are looking at, and none of them can be undone by this connector, which has no delete. `update_task` is the sharpest of them, since it can reassign a task or mark it complete. Grant those to the nodes that genuinely need them, and give the grant a `max_calls` cap when a loop could reach it.

## Doing it by hand

If you would rather not involve an agent, `PUBLIC.md` in this folder has the account fields and a config example, and `docs/CONNECTORS.md` in the apb repository covers accounts, secrets, and trust in general. `INSTALL.md` is written for an agent but the steps read fine as a checklist.
