# github: setup for humans

## The short way: let an agent do it

Setting this connector up by hand means editing two files, creating a token with the right permissions, and approving trust. An agent can do all of it for you and will only stop to ask for the token, which is the one thing it cannot obtain on your behalf.

Paste a prompt like this to a coding agent that has `apb` available:

> Install the apb `github` connector for my account, then read `connectors/github/INSTALL.md` under the apb config directory (by default `~/.config/apb`) and follow it to the end. Ask me for the token when you get there.

Say so if you are on GitHub Enterprise Server rather than github.com, and the agent will point the account at your own API base. It installs the connector, writes the account config, prepares the secrets file, approves trust, and runs a live healthcheck. What you get back is either a working account or a specific error.

## What you will be asked for

A token, and possibly nothing at all. If you already use the `gh` CLI and have run `gh auth login`, the agent can reference that session instead of storing a token, and there is nothing for you to paste.

Otherwise you need a personal access token from your GitHub settings. A classic token needs the `repo` scope, or `public_repo` if you only touch public repositories. A fine-grained token needs access to the specific repositories plus Actions write permission if playbooks are going to trigger workflows.

Whatever you paste is written to a local file with owner-only permissions, and the account config next to it stores only a reference to it, never the value.

## What this connector can and cannot do

It covers issue and pull request triage, releases, and Actions: reading, creating, and updating issues, adding and removing labels and assignees, comments, opening and merging pull requests, requesting reviewers, creating reviews, cutting releases, dispatching workflows, and reading run and check status.

It does not touch repository contents. No commits, no branches, no file edits, no repository creation or deletion, no settings. Git work stays with git.

Worth knowing before you grant it: `merge_pull`, `create_release`, and `dispatch_workflow` are not read-only and are not easily undone. A node holding `merge_pull` can merge a pull request without a human in the loop. Grant those to the nodes that genuinely need them, and give the grant a `max_calls` cap when a loop could reach it.

## Doing it by hand

If you would rather not involve an agent, `PUBLIC.md` in this folder has the account fields and a config example, and `docs/CONNECTORS.md` in the apb repository covers accounts, secrets, and trust in general. `INSTALL.md` is written for an agent but the steps read fine as a checklist.
