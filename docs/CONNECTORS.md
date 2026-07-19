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
field must be exactly one `{{env.VAR}}` reference; a literal secret in a config
file is a validation error. At most one `default: true` per merged list.

## Secrets

An `{{env.VAR}}` reference resolves at call time, in order: the process
environment, then the project `<project>/.apb/secrets.env`, then the global
`<config-dir>/secrets.env`. Dotenv files are `KEY=value` lines read only by
`apb`. Secret values never appear in the run manifest, the event log, CLI output,
or generated prompts, and the connector env names are scrubbed from every spawned
agent's environment.

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
