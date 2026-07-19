# Connectors

A connector links an `agent_task` node and its agent to an external service (a
task tracker, a messenger, mail, and similar). It gives the node a named set of
callable functions, similar in spirit to MCP but scoped to one service and
granted per node. Secrets never leave the `apb` process: functions are
declarative HTTP defined in `connector.yaml` and executed by `apb`, so a
connector cannot run arbitrary code and a token is only ever referenced by name.

Full design: [superpowers/specs/2026-07-18-connectors-design.md](superpowers/specs/2026-07-18-connectors-design.md).

## Installing a connector

A connector is a folder; installation is copying it and removal is deleting it:

- global scope: `<config-dir>/connectors/<name>/`

with:

- `connector.yaml` - the machine part, the only file the engine reads at run
  time (auth block, `account_fields`, and `functions`);
- `PUBLIC.md` - the storefront (YAML frontmatter plus a markdown body), rendered
  by the dashboard, never read at run time;
- `skills/` - reserved and covered by the digest, not delivered to prompts yet.

The folder name is the connector name and must pass the same slug rule as
profiles and skills (`[a-z0-9][a-z0-9-]*`, at most 64 chars). Scaffold a fresh
one with `apb connector init <name>`.

## Configuring accounts

An account tells the connector where to send a call and which secret to use. The
config files are non-secret and safe to commit and share:

- global: `<config-dir>/connector-config/<connector>.yaml`
- project: `<project>/.apb/connector-config/<connector>.yaml`

```yaml
accounts:
  - name: project-board          # slug, unique within the merged list
    default: true                # used when a grant has several accounts and no --account
    base_url: https://client.example.net
    token: "{{env.PROJECT_BOARD_TOKEN}}"
```

The merged list is global accounts plus project accounts; a project account with
the same name replaces the global one, all others are additive. A `secret: true`
field must be exactly one reference, either `{{env.VAR}}` or `{{cmd:<command>}}`
(a command whose stdout is the secret, resolved at call time, e.g.
`token: "{{cmd:gh auth token}}"`); a literal secret in a config file is a
validation error. At most one `default: true` per merged list.

## Secrets

An `{{env.VAR}}` reference resolves at call time, in order: the process
environment, then the project `<project>/.apb/secrets.env`, then the global
`<config-dir>/secrets.env`. Dotenv files are `KEY=value` lines read only by
`apb`. A `{{cmd:<command>}}` reference instead runs the command (shell-words
argv, no shell, 10 second timeout) at call and healthcheck time and uses its
trimmed stdout as the value; the command string is part of the account digest,
so changing it requires re-approval. Secret values never appear in the run
manifest, the event log, CLI output, or generated prompts, and the connector
env names are scrubbed from every spawned agent's environment.

List the variables an account still needs (names only, never values) with:

```sh
apb connector env <name>          # print the KEY= template lines to stdout
apb connector env <name> --write  # write them into .apb/secrets.env instead
```

Plain `env` prints the missing `KEY=` lines. With `--write`, `apb` appends those
lines to the project `<project>/.apb/secrets.env`, creating the file at mode 0600
when it does not exist, preserving anything already there, and never duplicating a
key the file already lists; it then makes sure `.gitignore` covers
`.apb/secrets.env`. Either way the values stay empty for you to fill in by hand;
`apb` never writes a secret value.

## Approving trust

A foreign `connector.yaml` (URL templates plus secrets) is as dangerous as
foreign code, and the account config decides where a token is sent. Both are
digest-pinned in the trust store, so a run refuses until you approve them, and
any later edit drops that approval:

```sh
apb connector approve <name>                    # approve the connector tree digest
apb connector approve <name> --account <acct>   # approve one account's non-secret fields
```

Approving an account shows the concrete field values so you see exactly where
secrets will be sent. `apb connector doctor` reports the trust status of every
connector and account (approved, changed since approval, or never approved)
alongside manifest, config, and env checks.

## Binding a connector to a node

An `agent_task` node gets an optional `connectors` list, the same two-form
pattern as `skills`:

```yaml
- id: triage
  type: agent_task
  profile: dev
  connectors:
    - mock-tracker                    # everything allowed
    - name: telegram
      accounts: [team-bot]            # optional allowlist; absent = all
      functions: [send_message]       # optional allowlist; absent = all
      max_calls: 50                   # optional per-node call budget (across retries)
    - name: github
      functions: read_only            # shorthand: every read_only function
```

`functions: read_only` grants exactly the functions the manifest marks
`read_only: true`, resolved at run start and frozen in the manifest grant, so a
later connector edit cannot widen a running grant. `max_calls` is a safety budget
against a looping agent, not a rate limiter; exceeding it returns a `permission`
error. The binding is part of the playbook YAML and is covered by the playbook
digest, so grants need no separate approval.

## The `apb connector` CLI

```text
apb connector list              installed connectors, trust and config status
apb connector show <name>       manifest summary and per-account status
apb connector call <name> <fn>  the agent-facing call channel (--account, --args, --dry-run)
apb connector approve <name>    approve a connector or (--account) an account digest
apb connector doctor            check every connector: manifest, config, env, trust, healthcheck
apb connector env [<name>]      unresolved env var names as ready-to-paste KEY= lines
apb connector init <name>       scaffold a new connector folder from a template
```

`apb connector call` needs a run context (`APB_RUN_DIR` and `APB_NODE_ID`, set by
the engine when a node executes a call); outside a run use `--dry-run` to render
a call without executing it, or the dashboard healthcheck to probe an account.
`--args -` reads the JSON arguments from stdin.

## Official connectors

Six official connectors ship inside the `apb` binary and install with
`apb connector install <name>`: `github`, `telegram`, `smtp`, `sentry`,
`asana`, `imap`. Installing from the binary records trust for the
connector's tree digest in the same action, since the bytes are already
part of the binary you are running; `apb connector install --from-dir
<path>` (the development loop for this repository, `connectors/<name>/`)
keeps the normal approve flow.

### github

Account fields: `api_base` (`https://api.github.com`, or your GHES API
base) and `token` (secret). Prefer `token: "{{cmd:gh auth token}}"` when
`gh auth login` has already run; otherwise `{{env.GITHUB_TOKEN}}` with a
personal access token: classic PATs need `repo` (or `public_repo` for
public repositories); fine-grained PATs need repository access with
Actions write permission for `dispatch_workflow`. Healthcheck:
`get_rate_limit`.

### telegram

Account fields: `api_base` (`https://api.telegram.org`, overridable for
a self-hosted Bot API server) and `token` (secret) - the token
[@BotFather](https://t.me/BotFather) gives you for a new bot. The bot
must already be a member of a chat before `send_message` reaches it.
Healthcheck: `get_me`.

### smtp

Account fields: `host`, `port`, `from_email` (all required), and
`username`, `password` (secret), `from_name`, `use_tls` (all optional).
Set `use_tls` explicitly (there is no engine-level default for account
fields): `true` for STARTTLS on port 587, the common case. Healthcheck:
`verify` (connects, negotiates STARTTLS, authenticates, sends nothing).

### sentry

Account fields: `base_url` (`https://sentry.io`, or self-hosted),
`org` (the organization slug), and `token` (secret). Create the token at
Settings > Auth Tokens with scopes `project:read`, `event:read`,
`event:write` for issue functions and `project:releases` for
`create_release`/`create_deploy`. `list_issues` paginates through the
call result's `link` field: pass the cursor it returns back into the
next call's `cursor` argument. Healthcheck: `list_projects`.

### asana

Account fields: `api_base` (`https://app.asana.com/api/1.0`) and
`token` (secret). Workspace, project, section, and task gids are call
arguments, not account fields, so one account serves every workspace
the token can reach. Create the token as a personal access token: in
Asana, open your profile settings, go to Apps, then Developer apps, and
create a new personal access token; it acts as the user who created it,
with that user's full permissions, and there is no separate scope to
select. `list_workspaces`, `list_projects`, and `list_tasks` take an
optional `offset` argument: read the next page's offset from the call
result's `next_page.offset` field and pass it back on the following
call, omitting it on the first call. `search_tasks` is a fuzzy typeahead
match against task names, not a full-text search; use `list_tasks` with
a project filter when a complete, predictable result set matters more.
Healthcheck: `get_me`.

### imap

Account fields: `host`, `port`, `auth_method` (`password` or `xoauth2`),
`username` (all required), `password` (secret), and `use_tls` (optional,
default `true`). One connector serves any IMAP provider, since the
protocol is identical everywhere and only the connection settings
differ. `search_messages` and `get_message` open the folder read-only
with `EXAMINE` and fetch content with `BODY.PEEK[]`, so reading a
message never marks it seen; only `mark_read` and `mark_unread` change
anything on the server, and each is a separate, independently grantable
function. `use_tls` defaults to `true` when omitted; only set it to
`false` for a local plaintext test fixture, never for a real provider.
Gmail (`imap.gmail.com`, port `993`) needs 2-Step Verification enabled
before an app password can be generated; a Google Workspace account
with app passwords disabled by policy instead uses `auth_method:
xoauth2` with an access token sourced via `{{cmd:...}}`. Outlook and
Microsoft 365 (`outlook.office365.com`, port `993`) only accept
`auth_method: xoauth2`, sourced from an external token helper such as
`oama` or `mutt_oauth2` with the same `{{cmd:...}}` mechanism; `apb`
does not implement an OAuth consent flow itself. Yandex Mail
(`imap.yandex.com`, port `993`) needs IMAP access enabled in the Yandex
Mail web settings before an app password can be generated. iCloud
(`imap.mail.me.com`, port `993`) uses an app-specific password from the
Apple ID account page. No message deletion, no move between folders,
and no sending: this connector only reads and marks read/unread, and is
meant to be installed alongside `smtp` for a read-and-reply workflow.
Healthcheck: `verify` (connects, negotiates TLS, authenticates, without
opening or reading any mailbox).

### Demo playbooks

`examples/playbooks/sentry-triage.yaml` and
`examples/playbooks/release-announce.yaml` exercise the github,
telegram, smtp, and sentry connectors end to end;
`examples/playbooks/inbox-triage.yaml` exercises imap and asana the
same way. All three double as reference examples for grant allowlists
and `max_calls`. They validate in CI against fake accounts and are not
run against real services there; run them manually once your own
accounts are configured and approved.

### Coverage note

Write functions (issue creation, merges, releases, sends) are verified
by the offline contract tests in each connector's `tests.yaml`; the
env-gated live smoke tests exercise each connector's healthcheck plus
one read-only function against the real service. Write paths are not
called against real services by any automated test.
