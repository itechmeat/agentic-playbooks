# Using apb from Paperclip

Paperclip (the `paperclipai` package) is an agent platform that assigns issues to background agents. This recipe installs the apb adapter (`examples/paperclip/apb-adapter/`), an external Paperclip `ServerAdapterModule` that dispatches a Paperclip agent's work to a locally running apb engine instead of to a hosted coding agent.

The adapter is a self-contained Node package: plain ESM, zero runtime dependencies, Node 24 or newer. It imports nothing from the rest of this repository, so it can be used in place or copied elsewhere unchanged.

## What you get

- A Paperclip adapter type (`adapterType: "apb"`) that starts an apb run with `POST /api/playbooks/{id}/run` and follows it with `GET /api/runs/{id}`.
- The apb run journal streamed back into the Paperclip run log as live events, so a run is observable from the Paperclip side while it executes.
- Re-attach to a run that is already live, instead of starting a duplicate when Paperclip wakes the same session again.
- Playbook selection per agent, per task key, or per issue identifier through `playbookMap`, with `PREFIX-*` globs and a `default` key.
- Provenance parameters passed into the playbook, so a run can tell which Paperclip issue and session it came from.

## Prerequisites

- apb installed and running in server mode, reachable over HTTP from the Paperclip process. The adapter was verified against apb 0.20.2.
- At least one apb project (`apb projects list`) and one trusted playbook in it. Trust is granted in the project, not from Paperclip.
- A Paperclip instance that loads external server adapters. The adapter was verified against paperclipai 2026.824.1.
- Node 24 or newer for the Paperclip process, since the adapter is ESM and uses a modern `fetch`.

## Setup

1. Copy `examples/paperclip/apb-adapter/` somewhere stable, or use it in place.
2. Register it with Paperclip as an external server adapter so the entry point `src/index.js` is loaded. Paperclip discovers the config form through the adapter's `getConfigSchema()`.
3. In the Paperclip agent form, set `adapterType` to `apb` and fill in the configuration. At minimum set `apbBaseUrl` and `project`.
4. Set `playbook`, or `playbookMap` when one agent should route different task keys to different playbooks.
5. Wake the agent. The first successful run proves the whole chain: adapter load, config resolution, project lookup, run start, journal streaming, and terminal-state mapping.

The full configuration reference, the resolution order, and the event mapping live in the package README: [examples/paperclip/apb-adapter/README.md](../../examples/paperclip/apb-adapter/README.md).

## Scope and secrets

- `project` has no default. A blank or mistyped project is a hard configuration error rather than a silent dispatch into the wrong project.
- `apbApiKey` is declared as a secret field, which is what gives it first-class Paperclip secret handling. A plain text field would be stored in cleartext on the agent row.
- Credentials embedded in `apbBaseUrl` userinfo are stripped at construction and resent as a `Basic` header, because Node's `fetch` refuses a credentialed URL and quotes the password into its error text.
- `allowTextDirectives` is off by default. Turning it on makes the adapter honour `apb:` directives found in issue text, which is an injection surface: anyone who can write issue text can then influence run parameters.
- `logParamValues` is off by default, so only parameter keys are logged. With values enabled, keys matching `token`, `key`, `secret`, `password`, `credential` or `auth` stay masked.
- A non-loopback apb host with no API key raises a warning rather than sending unauthenticated traffic silently.

## Testing

The package ships two suites, run from `examples/paperclip/apb-adapter/`:

- `npm test` runs 57 offline unit tests with an injected `fetch`. No network and no apb engine are required.
- `npm run test:live` runs 6 tests against a real apb engine using the throwaway project in `test-fixture/`. The fixture tracks its playbook definition only; run state under `test-fixture/.apb/runs/` is git-ignored, matching how this repository treats its own `.apb/runs/`.

## Known limitations

- Paperclip treats any non-zero exit as a failed run, so a playbook that exits non-zero for a soft outcome surfaces as a failure on the Paperclip side.
- The adapter sets no issue disposition. On issue assignment Paperclip can therefore start a corrective-run cascade, so drive the adapter through explicit wakeups or routines rather than through assignment.
- Hot reload cache-busts the entry file only. Changes deeper in the module graph need a full Paperclip restart.
- apb reports no per-run token counts through this path, so no usage or cost object is emitted.
