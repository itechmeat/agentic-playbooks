# Security Policy

## Supported versions

Until `apb` reaches a stable release, security fixes are provided for the latest
published release and the current default branch on a best-effort basis. Older
pre-release versions may not receive fixes.

## Reporting a vulnerability

Please use GitHub's private vulnerability reporting for this repository. Do not
open a public issue for a suspected vulnerability and do not include secrets,
private repositories, or personal data in a report.

A useful report includes:

- the affected version or commit;
- the operating system and installation method;
- a minimal reproduction;
- the security impact;
- any suggested mitigation.

## Security model

`apb` intentionally runs commands and coding agents described by playbooks.
Executing a command explicitly declared by a playbook that a user chose to run
is expected behavior, not by itself a security vulnerability.

Examples of issues that should be reported privately include:

- command execution not authorized by the selected playbook or policy;
- escaping an intended project or workspace boundary;
- unauthorized access to the web or MCP interface;
- exposure of credentials, prompts, run logs, or private file contents;
- bypassing supervisor capability restrictions or human-review gates;
- unsafe handling of imported playbook bundles or untrusted paths.

The inbound webhook listener (`apb ingest`, and `apb dashboard` when
`ingest.enabled` is true) is a separate socket with a separate router
carrying only `GET`/`POST /hooks/{connector}/{account}` and `GET /healthz`.
It is deliberately incapable of reaching the dashboard API, and a test
asserts that. Every delivery must carry a valid HMAC signature over the exact
bytes received; there is no unsigned mode and no opt-out flag. Bodies are
capped at 256 KiB, refusals are flat 401, 403 or 404 responses with no
detail, rejections are logged as `apb ingest_rejected ip=<ip> connector=<c>
account=<a>` for fail2ban, and accepted deliveries are capped per account at
600 appends in a rolling 60 second window and dropped with a 200 beyond the
cap, with the drop counted in a persisted per-account counter.

## Safe use

Treat third-party playbooks and imported bundles as executable code. Review them
before running.

The web dashboard binds `127.0.0.1` and runs unauthenticated by default. Before
exposing it to a network, issue an authorization key with `apb server key issue`
and place it behind a reverse proxy that terminates TLS; with a key present,
every `/api` route requires a bearer key or a session cookie, and binding a
non-loopback address without one is refused at startup. See
[docs/DEPLOYMENT.md](docs/DEPLOYMENT.md) for the supported topology.

The MCP interface speaks stdio and carries no authentication of its own. Do not
expose it to untrusted users or networks.

Treat the content of a connector inbox as hostile input. It is written by
whoever can reach your callback URL and it is fed to an agent that holds
connector grants and filesystem access. Inbox content is the first apb input
authored by arbitrary internet users, not by the operator, so give
inbox-reading nodes the narrowest `functions:` allowlist and the smallest
`max_calls` budget that still works, and do not pair an inbox read with a
grant you would not hand a stranger. Stored bodies are kept at mode 0600
under the global config directory and are never written to a run's event log
or to stdout.
