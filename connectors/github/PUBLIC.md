---
display_name: GitHub
summary: Manage GitHub issues, pull requests, releases, and workflow runs from a playbook.
tags: [github, issues, pull-requests, ci, releases]
publisher: apb
---

The GitHub connector covers issue and pull request triage, releases, and
Actions workflow dispatch over the REST API. Owner, repository, and issue
or pull request numbers are call arguments, not account fields, so one
account serves every repository the token can reach.

## Account setup

Two account fields: `api_base` (`https://api.github.com` for github.com,
or your GitHub Enterprise Server API base for GHES) and `token` (secret).

The recommended token source is the GitHub CLI, already authenticated on
most developer machines:

```yaml
accounts:
  - name: default
    api_base: https://api.github.com
    token: "{{cmd:gh auth token}}"
```

Run `gh auth login` first if you have not. Without `gh`, use a personal
access token in `GITHUB_TOKEN`:

```yaml
accounts:
  - name: default
    api_base: https://api.github.com
    token: "{{env.GITHUB_TOKEN}}"
```

Classic PATs need the `repo` scope (or `public_repo` for public
repositories only) for the issue, pull request, release, and
workflow-dispatch functions; fine-grained PATs need repository access
with Actions write permission for `dispatch_workflow`.

## Healthcheck

`get_rate_limit` probes the token and reports the remaining API quota.

## Excluded on purpose

GraphQL-only operations (marking a pull request ready for review),
reactions, deployments, and every webhook are out of scope for this
connector; the format is REST-only in this wave.
