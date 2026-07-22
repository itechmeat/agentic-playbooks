---
display_name: GitLab
summary: Manage GitLab issues, merge requests, releases, and pipelines from a playbook.
tags: [gitlab, issues, merge-requests, ci, releases, pipelines]
publisher: apb
---

The GitLab connector covers issue and merge request triage, releases, and
CI pipeline status plus pipeline triggering over the REST API v4. The
project (numeric id or path) is a call argument on every project-scoped
function, not an account field, so one account serves every project the
token can reach.

## Account setup

Two account fields: `api_base` (normally `https://gitlab.com/api/v4`, or
your self-hosted instance's API base ending in `/api/v4`) and `token`
(secret).

```yaml
accounts:
  - name: default
    api_base: https://gitlab.com/api/v4
    token: "{{env.GITLAB_TOKEN}}"
```

Self-hosted example:

```yaml
accounts:
  - name: default
    api_base: https://gitlab.example.com/api/v4
    token: "{{env.GITLAB_TOKEN}}"
```

### Personal access token

1. In GitLab, open your avatar menu and go to **Preferences** (or
   **Edit profile**), then **Access tokens** (user settings, not project
   or group tokens).
2. Create a personal access token with a name and expiry that fit your
   use.
3. Required scope: `api` for the full connector surface (reads and
   writes). For a read-only subset (`get_user`, `list_*`, `get_*`)
   `read_api` is enough.
4. Store the token value in an environment variable (for example
   `GITLAB_TOKEN`) and reference it from the account as shown above.
   Never paste the raw token into the account file.

## Project id or path

Every project-scoped function takes a `project` argument. Two forms are
accepted:

- Numeric id, for example `"42"`.
- Path form as `group/project` with a **literal slash** between the
  group (or nested groups) and the project name.

Pass the path form unencoded (`group/project`). The engine
percent-encodes substituted URL path values, so the rendered request
uses `group%2Fproject` as a single path segment. Do not pre-encode the
slash yourself (passing `group%2Fproject` as the arg would double-encode
to `group%252Fproject`). Contract and render tests pin this with a
`project: "group/project"` case that expects
`/projects/group%2Fproject/...` in the URL.

## Pagination

List functions that accept `page` and `per_page` follow GitLab's page
pagination (the same idiom as the github connector). Both query pairs
are optional single placeholders: omit them for the first page with the
server default page size; pass `page` (and optionally `per_page`, max
100) to walk further pages. Filter args such as `state`, `labels`,
`ref`, and `status` are likewise optional and drop from the query when
absent.

## Label editing

GitLab exposes label edits on the issue update endpoint, not as separate
add/remove routes. Use `update_issue` with `labels` (replace the full
set), `add_labels`, and/or `remove_labels` (comma-separated strings).
`assignee_ids` is an array of numeric user ids. Every optional body
field is a single placeholder and is dropped when absent, so a call that
only sets `state_event: close` does not touch labels or assignees.

## Pipelines

`list_pipelines` and `get_pipeline` report status for monitoring.
`list_pipeline_jobs` returns jobs under a pipeline. `trigger_pipeline`
creates a new pipeline on a branch or tag; optional `variables` must be
an array of `{key, value}` objects, matching the GitLab API shape:

```yaml
ref: main
variables:
  - key: DEPLOY_ENV
    value: staging
```

Grant `trigger_pipeline` only when a playbook is meant to start CI; the
demo patterns keep it off the allowlist when the playbook only observes
pipeline state.

## Healthcheck

`get_user` probes the token against `GET /user` and reports the
authenticated user's identity.
