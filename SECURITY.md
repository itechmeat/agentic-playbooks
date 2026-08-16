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
