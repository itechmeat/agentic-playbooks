---
display_name: Asana
summary: Manage Asana tasks, projects, sections, and comments from a playbook.
tags: [asana, tasks, projects]
publisher: apb
---

The Asana connector covers task creation and triage, project and section
listing, task comments (stories), subtasks, and a fuzzy task search
(typeahead) over the REST API. Workspace, project, section, and task gids
are call arguments, not account fields, so one account serves every
workspace the token can reach.

## Account setup

Two account fields: `api_base` (`https://app.asana.com/api/1.0`) and
`token` (secret).

```yaml
accounts:
  - name: default
    api_base: https://app.asana.com/api/1.0
    token: "{{env.ASANA_TOKEN}}"
```

Create the token as a personal access token: in Asana, open your profile
settings, go to Apps, then Developer apps, and create a new personal
access token. A personal access token acts as the user who created it,
with that user's full permissions; there is no separate scope to select.

## Pagination

`list_workspaces`, `list_projects`, and `list_tasks` take an optional
`offset` argument. Read the next page's offset from the call result's
`next_page.offset` field and pass it back as `offset` on the following
call; omit `offset` on the first call.

## Search

`search_tasks` calls the Asana typeahead endpoint: it is a fuzzy match
against task names for quick lookup, not a full-text search over task
content. Use `list_tasks` with a project filter when a complete,
predictable result set matters more than a quick name match.

## Healthcheck

`get_me` confirms the token resolves and reports the authenticated
user's identity.
