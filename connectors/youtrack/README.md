# youtrack: setup for humans

## The short way: let an agent do it

Setting this connector up by hand means editing two files, creating a token, and approving trust. An agent can do all of it for you and will only stop to ask for the token, which is the one thing it cannot obtain on your behalf.

Paste a prompt like this to a coding agent that has `apb` available:

> Install the apb `youtrack` connector for my YouTrack, then read `connectors/youtrack/INSTALL.md` under the apb config directory (by default `~/.config/apb`) and follow it to the end. Ask me for the token when you get there.

Tell it your YouTrack address, cloud or self-hosted. The agent installs the connector, writes the account config, prepares the secrets file, approves trust, and runs a live healthcheck that asks YouTrack who you are. What you get back is either a working account or a specific error.

## What you will be asked for

A permanent access token. In YouTrack, open your profile, go to Account Security, then Access Tokens, and create one.

As with Asana, the token acts as you with your full permissions, and there are no scopes to choose from. Anything you can do in YouTrack, a playbook holding this token can do. The way to limit that is to grant a node only the functions it needs.

The token is written to a local file with owner-only permissions, and the account config next to it stores only a reference to it, never the value.

## What this connector can and cannot do

It covers issue work: searching issues with YouTrack's own query syntax, reading one, creating and updating them, listing projects, reading and adding comments, and applying YouTrack commands.

That last one deserves attention. `apply_command` runs YouTrack's native command syntax, which is deliberately powerful: one command can change state, tags, assignee, priority, and custom fields at once. It is the single function here that is worth being deliberate about granting.

It does not manage workflows, agile boards, users, or project configuration, and it cannot delete an issue.

## Doing it by hand

If you would rather not involve an agent, `PUBLIC.md` in this folder has the account fields and a config example, and `docs/CONNECTORS.md` in the apb repository covers accounts, secrets, and trust in general. `INSTALL.md` is written for an agent but the steps read fine as a checklist.
