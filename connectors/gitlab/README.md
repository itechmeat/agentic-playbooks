# gitlab: setup for humans

## The short way: let an agent do it

Setting this connector up by hand means editing two files, creating a token with the right scope, and approving trust. An agent can do all of it for you and will only stop to ask for the token, which is the one thing it cannot obtain on your behalf.

Paste a prompt like this to a coding agent that has `apb` available:

> Install the apb `gitlab` connector for my account, then read `connectors/gitlab/INSTALL.md` under the apb config directory (by default `~/.config/apb`) and follow it to the end. Ask me for the token when you get there.

Say so if you are on a self-hosted GitLab rather than gitlab.com, and the agent will point the account at your own API base. It installs the connector, writes the account config, prepares the secrets file, approves trust, and runs a live healthcheck. What you get back is either a working account or a specific error.

## What you will be asked for

A personal access token, created under Preferences, then Access tokens, in your user settings. It has to be a personal token: a project or group token will not cover the whole connector.

Two scopes matter. `api` grants everything this connector can do. `read_api` is enough if playbooks only read issues, merge requests, and pipelines and never write. The agent will ask which one you want and recommend the narrower option when writing is not needed.

If you are on a self-hosted GitLab rather than gitlab.com, the agent also needs that instance's own API base URL, which you confirm off your own instance rather than the agent guessing it. And it will ask whether the account should live in your global config (available to every project) or in this one project, since that decides who else can use it.

The token is written to a local file with owner-only permissions, and the account config next to it stores only a reference to it, never the value.

## What this connector can and cannot do

It covers issue and merge request triage, releases, and CI: reading and creating issues, comments, labels, opening merge requests, approving and merging them, cutting releases, reading pipelines and their jobs, and triggering a pipeline on a branch or tag.

It does not touch repository contents. No commits, no branches, no file edits, no project creation or deletion, no settings. Git work stays with git.

Worth knowing before you grant it: `merge_merge_request`, `create_release`, and `trigger_pipeline` are not read-only and are not easily undone. Triggering a pipeline can deploy something. Grant those to the nodes that genuinely need them, and give the grant a `max_calls` cap when a loop could reach it.

## Doing it by hand

If you would rather not involve an agent, `PUBLIC.md` in this folder has the account fields and a config example, and `docs/CONNECTORS.md` in the apb repository covers accounts, secrets, and trust in general. `INSTALL.md` is written for an agent but the steps read fine as a checklist.
