# sentry: setup for humans

## The short way: let an agent do it

Setting this connector up by hand means editing two files, creating a token with the right scopes, and approving trust. An agent can do all of it for you and will only stop to ask for the token, which is the one thing it cannot obtain on your behalf.

Paste a prompt like this to a coding agent that has `apb` available:

> Install the apb `sentry` connector for my organization, then read `connectors/sentry/INSTALL.md` under the apb config directory (by default `~/.config/apb`) and follow it to the end. Ask me for the token when you get there.

Say so if you run a self-hosted Sentry rather than sentry.io. The agent installs the connector, writes the account config, prepares the secrets file, approves trust, and runs a live healthcheck that lists your projects. What you get back is either a working account or a specific error.

## What you will be asked for

Two things, and one of them is not a secret: your organization slug, which is the short name in your Sentry URLs, and an auth token.

Create the token in Sentry under Settings, then Auth Tokens. Scopes: `project:read` and `event:read` cover reading issues, `event:write` is needed to resolve or ignore them, and `project:releases` is needed if playbooks are going to record releases and deploys. The agent will ask which of those you actually want.

The token is written to a local file with owner-only permissions, and the account config next to it stores only a reference to it, never the value.

## What this connector can and cannot do

It covers issue triage and release bookkeeping: listing and searching issues, reading one, updating its state, and creating releases and deploys.

It does not manage alert rules, does not configure webhooks, and does not link issues across other services. If you want a Sentry issue to become a GitHub issue, that is a playbook joining two connectors, not something this one does.

Worth knowing before you grant it: `update_issue` changes triage state that your team sees, and `create_release` and `create_deploy` write records other tooling may read. None of them destroy data, but all three are visible to colleagues.

## Doing it by hand

If you would rather not involve an agent, `PUBLIC.md` in this folder has the account fields and a config example, and `docs/CONNECTORS.md` in the apb repository covers accounts, secrets, and trust in general. `INSTALL.md` is written for an agent but the steps read fine as a checklist.
