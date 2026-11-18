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

## Receiving events

The connector can also receive GitHub's webhooks instead of polling. In your repository or organization settings, add a webhook whose payload URL is your machine's ingest endpoint (`/hooks/github/<account>` on the port `apb ingest` serves), whose content type is `application/json`, and whose secret is the value you store as the account's `webhook_secret` field. GitHub signs every delivery with that secret under `X-Hub-Signature-256`, and the ingest listener verifies the signature before anything is stored; a delivery that does not verify is refused and nothing reaches the inbox.

Each delivery is typed by the `X-GitHub-Event` header (`issues`, `pull_request`, `push`, `workflow_run` and the rest), and the stored event keeps that type. GitHub's signature covers the body only, not this header, so the type is as trustworthy as the delivery's transport; a reader that acts on a type should also check the body fields it relies on with `where`. `inbox_read` takes a `types` list so a reader that only wants new issues, say `types: [issues]`, does not see the pushes and status updates at all, and a `where` list filters the delivery body further; see the `inbox_read` function for the exact shapes. Deliveries are stored locally under the account and read with `inbox_read`, advanced with `inbox_ack`, and counted with `inbox_peek_depth`.

## What this connector can and cannot do

It covers issue and pull request triage, releases, and Actions: reading, creating, and updating issues, adding and removing labels and assignees, comments, opening and merging pull requests, requesting reviewers, creating reviews, cutting releases, dispatching workflows, and reading run and check status.

It does not touch repository contents. No commits, no branches, no file edits, no repository creation or deletion, no settings. Git work stays with git.

Worth knowing before you grant it: `merge_pull`, `create_release`, and `dispatch_workflow` are not read-only and are not easily undone. A node holding `merge_pull` can merge a pull request without a human in the loop. Grant those to the nodes that genuinely need them, and give the grant a `max_calls` cap when a loop could reach it.

Creating an issue and commenting are sent at most once even when a step is retried. apb appends a hidden HTML comment to the issue or comment body (invisible on GitHub), and when an earlier attempt's answer was lost, it reads the token's own account and the recent issues or comments to find that marker before deciding: found means the earlier send went through, and the retry gets its link instead of posting again. This lookup needs a token that can read `/user`, which a personal access token can and a GitHub App installation token cannot; with an installation token, a send whose answer was lost stays unknown until a person decides. `create_release` is protected only by the tag being unique, so a lost answer there is also left for a person. `add_labels` and `remove_label` need no such lookup: a repeat of one changes nothing on the issue, so they declare `idempotency: { mode: repeatable }`. They are not tracked as sends, a call whose answer was lost is retried like before, and a step that labels issues one by one in a list may use them.

## Doing it by hand

If you would rather not involve an agent, `PUBLIC.md` in this folder has the account fields and a config example, and `docs/CONNECTORS.md` in the apb repository covers accounts, secrets, and trust in general. `INSTALL.md` is written for an agent but the steps read fine as a checklist.
