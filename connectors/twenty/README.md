# twenty: setup for humans

## The short way: let an agent do it

Setting this connector up by hand means editing two files, creating an API
key, and approving trust. An agent can do all of it for you and will only
stop to ask for the key, which is the one thing it cannot obtain on your
behalf.

Paste a prompt like this to a coding agent that has `apb` available:

> Install the apb `twenty` connector for my account, then read
> `connectors/twenty/INSTALL.md` under the apb config directory (by default
> `~/.config/apb`) and follow it to the end. Ask me for the base URL and API
> key when you get there.

The agent installs the connector, writes the account config, prepares the
secrets file, approves trust, and runs a live healthcheck (`list_companies`)
against your workspace. What you get back is either a working account or a
specific error.

## What you will be asked for

A base URL and an API key.

The base URL is the app origin only, no path suffix: your own host for a
self-hosted instance (for example `https://crm.example.com`), or
`https://api.twenty.com` for the cloud product. Every function is templated
under `<base_url>/rest/...`.

Create the API key in Twenty under Settings, API and Webhooks (some
versions label this section Playground instead), then Create key. The key
value is shown only once, so copy it immediately. A key is
scoped to the workspace it was created in, and its permissions follow the
role assigned to it under Settings, Members, Roles (Assignment tab); Twenty
has no separate scope selection beyond that role. Every key carries a
mandatory expiry set at creation time; there is no "never expires" option,
so plan to rotate it before it lapses.

## What this connector can and cannot do

It covers typed CRUD for the five core CRM objects (companies, people,
opportunities, notes, tasks), the noteTargets/taskTargets join objects used
to link a note or task to a person, company, or opportunity, duplicate
detection for companies and people, generic record access for every other
object including custom ones (addressed by camelCase plural REST name, for
example `workspaceMembers` or `attachments`), webhook registration and
listing, and a metadata listing of the workspace's data model. 41 functions
in total.

Every record delete in this connector is a soft delete. Twenty's own DELETE
verb defaults to a hard, unrecoverable destroy when no flag is sent; this
connector deliberately never exposes that default on record deletes and
always sends the fixed query `soft_delete=true`, so a delete here sets
`deletedAt` and stops the record from appearing in list/get calls without
destroying it. `restore_record` undoes any of the five typed deletes or the
generic `delete_record`, by the same object name and id. `delete_webhook` is
the one exception in this connector: it is a hard, unrecoverable delete with
no restore path.

23 functions are effectful: the five typed `create_<singular>` and
`update_<singular>` and `delete_<singular>` functions (companies, people,
opportunities, notes, tasks), `create_note_target`, `create_task_target`,
the generic `create_record`, `update_record`, `delete_record`,
`restore_record`, and `create_webhook`/`delete_webhook`. None of the record
deletes above are truly irreversible thanks to the soft-delete stance, but
`delete_webhook` is hard and unrecoverable, and a create or update still
writes into data your colleagues see, so grant those to the nodes that
genuinely need them.

Rate limits: Twenty's defaults are 100 requests per second and 100 requests
per minute (two separate windows, both env-overridable on a self-hosted
instance), plus a separate cap of 100 records per mutation. None of that is
enforced by this connector; back off on a 429 and keep loops bounded with
`max_calls`.

A few caveats worth knowing before relying on a response shape:

- `depth` only accepts `0` (default) or `1`; any other value is a 400.
- Error bodies carry a `messages` array (plural), not a single `message`
  string.
- A missing `Authorization` header returns 403; an invalid or expired key
  returns 401 - different status codes for the two failure modes.
- `limit` on list functions defaults to 60 and caps at 200.
- Batch endpoints (`/rest/batch/*`), `groupBy`, merge, attachment binary
  upload, GraphQL, and API-key management are out of scope for this
  connector's 0.1 surface and are not exposed as functions.
- Custom objects have no typed functions of their own; they are reached
  through
  `list_records`/`get_record`/`create_record`/`update_record`/`delete_record`,
  addressed by the object's own `namePlural` (find it with `list_objects`).
- `find_duplicate_companies` and `find_duplicate_people` are `POST` calls
  marked `read_only: true`, because they are searches with no side effect;
  a node granted `functions: read_only` can call either of them.

## Doing it by hand

If you would rather not involve an agent, `PUBLIC.md` in this folder has the
account fields and a config example, and `docs/CONNECTORS.md` in the apb
repository covers accounts, secrets, and trust in general. `INSTALL.md` is
written for an agent but the steps read fine as a checklist.
