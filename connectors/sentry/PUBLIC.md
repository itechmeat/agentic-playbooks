---
display_name: Sentry
summary: Triage Sentry issues and record releases and deploys from a playbook.
tags: [sentry, error-tracking, releases]
publisher: apb
---

Covers issue search and triage plus release and deploy bookkeeping.
Alert rules, webhooks, and cross-connector issue linking are out of
scope for this connector; do that orchestration in the playbook.

## Account setup

Three account fields: `base_url` (`https://sentry.io`, or your
self-hosted URL), `org` (the organization slug), and `token` (secret).

```yaml
accounts:
  - name: default
    base_url: https://sentry.io
    org: acme
    token: "{{env.SENTRY_TOKEN}}"
```

Create the token at Settings > Auth Tokens with scopes `project:read`,
`event:read`, and `event:write` for the issue functions, plus
`project:releases` for `create_release` and `create_deploy`.

## Pagination

`list_issues` takes an explicit `cursor` argument; read the next
cursor from the call result's `link` field and pass it back on the
following call.

## Healthcheck

`list_projects` confirms the token and organization slug resolve.
