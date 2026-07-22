---
display_name: YouTrack
summary: Search, create, and update YouTrack issues, comments, and commands from a playbook.
tags: [youtrack, issues, project-tracking]
publisher: apb
---

The YouTrack connector covers issue search and CRUD, comments, project
listing, and YouTrack's native command syntax for state changes, tagging, and
field updates, over the REST API. Issue, comment, and project identifiers are
call arguments, not account fields, so one account serves every project the
token can reach.

## Account setup

Two account fields: `api_base` and `token` (secret).

```yaml
accounts:
  - name: default
    api_base: https://example.youtrack.cloud/api
    token: "{{env.YOUTRACK_TOKEN}}"
```

Create the token as a permanent access token: in YouTrack, open your profile
(top-right avatar, Profile), go to Account Security, then under Access Tokens
create a new token with a descriptive name. A permanent token acts as the user
who created it, with that user's full permissions; there is no separate scope
to select. Store the token in an environment variable and reference it as
`{{env.YOUTRACK_TOKEN}}` in the account config.

### Cloud and self-hosted api_base forms

For YouTrack Cloud, `api_base` is `https://<org>.youtrack.cloud/api` (the
`/api` suffix is required). For a self-hosted YouTrack instance, `api_base`
is `https://<host>/api` (or `http://<host>/api` if TLS is not configured),
with the same `/api` suffix.

## The fields= discipline

YouTrack returns only the fields you explicitly request via the `fields=`
query parameter. Every read function in this connector bakes a literal
`fields=` value that matches its `response_pick`, so a response carries
exactly the projection an agent needs and nothing more. Custom fields are not
requested in this wave; the issue projections cover `idReadable`, `summary`,
`resolved`, the project short name, and the reporter login.

## Pagination

`search_issues` takes optional `$skip` and `$top` arguments. Pass `$top` to
limit the page size and increase `$skip` by `$top` on each call to page
forward. Omit both on the first call. Note that `$skip` and `$top` are the
literal YouTrack query key names; the engine percent-encodes the `$` when it
builds the URL (`$skip` becomes `%24skip` on the wire), which YouTrack
accepts.

## Search query syntax

`search_issues` uses YouTrack's native query syntax in its `query` argument.
Common forms:

- `state: Fixed` finds issues in the Fixed state.
- `project: DEMO` finds issues in the DEMO project.
- `for: me #Unresolved` finds unresolved issues assigned to the current user.

## Commands and the apply_command power warning

`apply_command` applies a YouTrack command to one or more issues through the
native command syntax. Common commands:

- `state Fixed` sets the state to Fixed.
- `tag regression` adds the regression tag.
- `for me priority Critical` assigns the issue to the current user and sets
  the priority to Critical.

YouTrack command syntax is powerful enough to change almost anything on an
issue. `apply_command` is one function, so a grant that includes it allows ALL
commands, including state changes, tagging, assignment, priority, and any
custom field update the command syntax can express. Restrict it in grant
allowlists accordingly.

## Project ids

`create_issue` takes the project as a database id (for example `0-0`), not the
short name. Call `list_projects` to find a project's `id` alongside its
`shortName` and `name`.

## Healthcheck

`get_me` confirms the token resolves and reports the authenticated user's
identity.
