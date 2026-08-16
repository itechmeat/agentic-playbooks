---
display_name: Twenty
summary: Manage companies, people, opportunities, notes, and tasks in the Twenty CRM REST API.
tags: [twenty, crm, sales]
publisher: apb
---

The Twenty connector covers the Twenty CRM REST API: typed CRUD for the five
core objects (companies, people, opportunities, notes, tasks), the
noteTargets/taskTargets join objects that link a note or task to a person,
company, or opportunity, duplicate detection, generic record access for
every other object including custom ones, webhook management, and a
metadata listing of the workspace's data model. 41 functions in total.

Every record delete in this connector (the five typed deletes and the
generic `delete_record`) is a soft delete: Twenty's own DELETE verb defaults
to a hard, unrecoverable destroy when no flag is sent, so these always send
the fixed query `soft_delete=true` instead. A soft-deleted record stops
appearing in list/get calls but is not destroyed, and `restore_record` undoes
it. `delete_webhook` is the one exception: it is a hard, unrecoverable
delete with no restore path. Custom objects and any standard object with no
typed function (`workspaceMembers`, `attachments`, and so on) are reached
through
`list_records`/`get_record`/`create_record`/`update_record`/`delete_record`,
addressed by the object's camelCase plural REST name; `list_objects`
discovers those names from the workspace's data model.

## Account setup

Two account fields: `base_url` (the app origin, no path suffix, e.g.
`https://crm.example.com` for a self-hosted instance or
`https://api.twenty.com` for the cloud product) and `api_key` (secret).

```yaml
accounts:
  - name: default
    base_url: https://crm.example.com
    api_key: "{{env.TWENTY_API_KEY}}"
```

Create the key in Twenty under Settings, API and Webhooks (some versions
label this section Playground instead). The key value is
shown only once, it is scoped to the workspace it was created in, its
permissions follow the role assigned to it under Settings, Members, Roles
(Assignment tab), and every key carries a mandatory expiry (there is no
"never expires" option).

## Healthcheck

`list_companies` confirms the key and base URL work: it renders with zero
arguments and succeeds against any key that can read companies.
