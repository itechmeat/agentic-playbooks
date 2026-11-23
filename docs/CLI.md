# Command line reference

A map of the `apb` commands. Every command prints its own flags with `--help`. For agents there is a machine-oriented entry point in [llms.txt](../llms.txt).

## Everyday commands

```text
apb init            create the .apb structure (--standing-block, --feedback-loop, --subscriptions answer without a terminal; --json)
apb setup           plan (read only), apply --plan <digest>, remove: default playbooks, skills, role profiles, MCP registration
apb list            playbooks and versions
apb validate        validate playbooks, the profiles and scripts they reference, requires and connectors
apb run <id>        run a playbook (--param k=v, --overrides, --supervise, --detach, --execution, --confirm-irreversible, --scope project|global)
apb templates       list / show / install the example playbooks apb ships, as drafts
apb trigger          start rules a person writes and approves (apb trigger set <rule> --file <path>; list, show <rule>, approve <rule>, enable <rule>, disable <rule>, remove <rule>, log <rule>, test <rule> [--event <seq>]; a person changes a rule at an interactive terminal or in a signed-in dashboard)
apb wait <run>      block until a run finishes, needs input or stops (exit code = verdict)
apb runs [run]      list runs, or one run with its execution mode and tokens (--json)
apb pause <run>     pause a run (--drain default, --now, --reason, --until, --keep-lock)
apb resume <run>    resume a paused or interrupted run (--all: runs a lost driver or a passed pause left waiting)
apb stop <run>      stop a run: interrupt the running node, finalize if the driver is gone
apb note <run> <t>  post a note (context append) to a run's control channel
apb review          decide a pending human_review node
apb answer <run> <t> answer an interactive node's pending question (--node)
apb tasks [run]     host tasks of host-mode runs; `apb tasks submit` replies to one
apb trash list      deleted playbooks; `apb trash restore <name|id>` brings one back
apb trust list      approvals in the trust store; `apb trust revoke <digest|id>` removes them
apb dashboard       web UI (port 7321)
apb mcp             stdio MCP server for coding agents (--read-only: look, never act)
apb self-update     update apb to the latest release (--check only reports)
```

## Profiles, environment and analysis

```text
apb detect          detect installed coding agents and their models (local checks only)
apb profile         list / show / write / edit / move / delete agent profiles
apb subscriptions   view or declare the agent subscriptions you pay for
apb connector       list / show / call / approve / doctor / env / init / install / test - connectors to external services
apb migrate         migrate schema 1 playbooks (executors) to schema 2 (profiles)
apb adopt           adoption readiness report for a playbook
apb doctor          diagnose agents, profiles, runners, playbooks, the defaults (--run <id> one run's health, --agents every installed agent, --probe-agent <id> one paid format check, --json)
apb export/import   move a playbook version (YAML, layout, scripts) as one bundle file
apb suggestions     list / allow / reset save-as-playbook suggestion declines
apb discover        find step sequences your past agent sessions repeat; list / show / save / dismiss
apb stats           cross-run metrics per playbook version and node (read-only)
apb grade <run>     grade an ended run by rules (--model: by the decision model); --backfill <id> grades earlier runs
apb candidate <id>  a candidate version compared with current by quality, with the summary to read first; --promote --expect <v>@<digest> [--ack <id>], --keep-current
apb problems <id>   a playbook's known problems; confirm / dismiss / reopen / close --quote / recheck / scan
apb eval <id>       run a playbook's eval cases in scratch repositories (opt-in, paid); --level quick plays its scenarios in seconds, free
apb judge           evaluate one judge node outside any run; keep and check labelled examples
apb demo            install (--run: start) the keyless demo project; --remove deletes it
apb cache           status / inspect / prune / clear the node result cache
apb decide          ask the configured decision models a bounded question
apb decisions       decision-model report, stored thresholds, replay
apb projects        list / remove / prune [--dry-run] entries of the workspace registry
apb ingest          only the inbound webhook listener (headless)
apb server key      issue / list / revoke the keys that protect a networked dashboard
```

## The workspace registry

Every project you run apb in is recorded in `projects.json` under the config
dir, which the dashboard's project pickers and `apb projects list` read. An
entry whose directory is gone turns `unreachable`; `apb projects prune`
removes every such entry (unreachable or tombstoned) at once, printing each
one, and `--dry-run` only prints them. Any registry write also drops an entry
that has been unreachable for 30 days. A git linked worktree of a registered
project records that project's workspace id, and both `apb projects list` and
the dashboard's project pickers list it indented under its parent project
instead of as a peer.


## Developing apb itself

```sh
(cd web && bun install && bun run build)   # the binary embeds web/dist
cargo test --workspace                      # engine, core, server, CLI tests
cd web && bun run test                      # frontend tests (vitest)
cd web && bun run dev                       # dev frontend with a proxy to :7321
apb dev                                     # Vite HMR + API from the source tree
```

Contributor guide, gates and the third-party notices workflow:
[CONTRIBUTING.md](../CONTRIBUTING.md).
