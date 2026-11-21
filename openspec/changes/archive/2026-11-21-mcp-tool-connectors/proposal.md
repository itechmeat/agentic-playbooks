# Proposal: MCP tool connectors

## Why

A playbook node can reach a service only through the five connector
function kinds apb executes itself: HTTP, mock, smtp, imap and inbox
(`crates/apb-core/src/connector/def.rs` lines 360 to 364, the exactly-one
count at lines 652 to 699). A growing share of services, and many local
tools, are offered as servers of the Model Context Protocol instead of a
documented HTTP API. Today the only way to use one is to hand the server to
the agent CLI, which skips every guarantee apb gives a connector call. And
apb's own MCP server lags the current protocol revision on the host side.
Concretely:

1. No MCP kind. A function is HTTP, mock, smtp, imap or inbox and nothing
   else (`def.rs` line 665 names the five in its error). An MCP server's
   tools cannot be granted to a node, budgeted with `max_calls`, journaled
   as `connector_call`, or tracked by the send ledger.
2. The agent route is the unsafe route. apb passes only its own ask-server
   to claude (`crates/apb-core/src/agents/claude/settings.rs` lines 54 to
   61, `--strict-mcp-config`), on purpose: a server written into an
   agent's MCP config is called by the agent with no grant, no trust check,
   no secret scrubbing and no send-once record. Nothing better exists.
3. Server text is untrusted and changes. A server describes its own tools
   (name, description, input schema, annotations) and may change that list
   at any time; the specification says clients MUST consider annotations
   untrusted unless the server is trusted
   (https://modelcontextprotocol.io/specification/2026-07-28/server/tools,
   "Tool"). apb has no way to pin what a person approved.
4. Issue 57 is open. rmcp is pinned at `=3.9.0`
   (`crates/apb-mcp/Cargo.toml` line 18). The host-independent slice
   shipped (PR 61), but `docs/HOST-INTEGRATION.md` line 18 still says "No
   host ships the new revision yet", while Claude Code negotiates
   2026-07-28 with local stdio servers by default since 2.1.292. The
   regression across old and new revision hosts, the tier-0 re-measurement
   and the `ttlMs` / `cacheScope` position are not done. `docs/MCP.md`
   lines 1225 to 1227 describe an rmcp behaviour that is no longer true
   (rmcp fills cache hints only for 2026-07-28 peers, `src/handler/server.rs`
   lines 316 to 318 in 3.9.0), and `docs/MCP.md` line 1219 and
   `docs/HOST-INTEGRATION.md` line 18 still name rmcp `=3.0.1`.

| Decision | Code example | Non-code example |
|---|---|---|
| D1 mcp kind | a code-search server over stdio: `search` and `read_file` (read only) granted to a review node | a CRM server: `list_contacts` (read only) granted to a follow-up node |
| D2 tool pins | the code-search server's `search` description changes after an upgrade: the next call is refused `mcp_tool_changed` until a person re-approves | the CRM server adds a `delete_all` tool: it is invisible to every node because no function maps it |
| D3 send once | an issue tracker's `create_comment` with `idempotency: marker`: a retry after a dropped connection finds the comment by its marker | the CRM's `add_note` with marker; `delete_contact` with `none` and `irreversible` behind a `human_review` gate |
| D4 stdio | the server is started per call from the program file the approval pinned (the file that was hashed), in a fresh private working directory, with an environment built from a fixed source plus the account's declared variables, as the leader of its own process group, which the close ends | a local document tool started with the account's folder as an argument |
| D5 surfaces | `apb connector approve code-search --account work` at a terminal prints the program, its digest and each tool with its digest, and pins them once the person types the connector name | the CRM's account approval prints `add_note` and `delete_contact` with their annotations, shown and not used |
| D6 apb mcp | one `apb mcp` binary answers a 2026-07-28 client and a 2025-06-18 client, proven by wire tests | the same, for a person who drives a CRM routine from their host |

Examples for the whole change:

1. Code: a code-search MCP server over stdio with read-only `search` and
   `read_file` granted to a review node; `apb connector approve
   code-search --account work` pins both tools, and after an upgrade
   changes `search`'s description the next call is refused
   `mcp_tool_changed` before anything reaches the server.

```yaml
mcp:
  transport: stdio
  command: code-search-mcp
  args: ["--root", "{{account.repo_dir}}"]
functions:
  - { name: search, description: Search the code, read_only: true, mcp: { tool: search } }
  - { name: read_file, description: Read one file, read_only: true, mcp: { tool: read_file } }
```

2. Non-code: a local notes MCP server over stdio: `add_note` with
   `idempotency: marker` (a retry finds the note by its marker), and
   `delete_note` declared `irreversible` with `idempotency: none`, granted
   only to a node behind a `human_review` gate.

## What changes

1. MCP connector kind (capability `mcp-connector-kind`): a connector
   manifest gains a connector-level `mcp:` block (`transport: stdio` with a
   literal `command`, `args` and `env` templates, plus `protocol: auto |
   modern | legacy`; `transport: http` is refused by this version)
   and a function-level `mcp: { tool: <name> }`. Such a connector holds only
   `mcp` and `mock` functions. `read_only`, `irreversible` and
   `idempotency` are the author's, per function; server annotations are
   shown, never used. MCP functions count as `untrusted_output`. A function
   is called by apb (`apb connector call`), never by the agent. New
   validator codes: V300 (warning, a binding grants every function of an
   MCP connector), V301 (warning, the stdio command is an interpreter or
   a launcher such as `node` or `npx`, whose approval covers that program
   file only, not the script or package it runs) and V302 (error, an
   MCP connector at an engine site: the baseline check, the follow-up
   check or a list item check); V113's message names the MCP reason. A
   manifest rule refuses `protocol: auto` with a `timeout_sec` under 15
   (`mcp_timeout_too_small`), since rmcp's discover probe waits a fixed
   10 s, and one refuses an `env` name that changes what code a process
   loads (`LD_PRELOAD`, `NODE_OPTIONS`, `PYTHONPATH` and the rest of one
   list; `mcp_env_loads_code`).
2. Program and tool pins (capability `mcp-tool-pins`): approving an MCP
   connector, its tree as well as its account, is a person's act.
   `apb connector approve <c> --account <a>` at an interactive terminal
   resolves the stdio command once to an absolute canonical path, shows
   the path, the file's digest, the rendered non-secret arguments, every
   environment override with its rendered value (secret parts masked with
   their source named) and the account's non-secret fields, and takes a
   first typed confirmation before any account secret is resolved or
   anything starts, then, after comparing
   the reloaded tree and account digests with the confirmed ones and
   before any secret is resolved, starts it and
   shows every mapped tool (with its title and input schema) with the
   digest of its whole definition as the server sent it (minus `icons`
   and `_meta`), its texts after the account's redactions, and pins them
   in one record of a new store `<config-dir>/mcp-tools.json` after the
   person types the connector name a second time. Approval happens only
   at an interactive terminal: without one, inside a run step, over MCP
   and from the dashboard it is refused `mcp_approve_needs_a_person`,
   which prints no digest; there is no flag that names an expected
   digest (out of scope 17).
   The record is one binding: the approved connector tree digest, the
   approved account digest (so a project's account of the same name is
   not covered), the program path, the program digest and the tool
   digests, so approving a
   changed tree alone never makes an old pin usable. The connector path
   without `--account` takes one typed confirmation at a terminal. A
   listing whose raw tool definitions hold any account secret is refused
   before anything is hashed, shown or stored. Listings are bounded (20
   pages, 500 tools, 2 MiB) and a record that would push the store past
   its 4 MiB read cap is refused. Every call
   opens the pinned program file once, compares its digest, starts that
   same file (on Linux from the descriptor it hashed; elsewhere its path
   is hashed again right before the start), lists the tools again and is
   refused
   before anything is sent when the binding differs: the definition or
   the program file changed (`mcp_server_changed`) or the live tool
   digest is not the pinned one (`mcp_tool_unpinned`, `mcp_tool_changed`). A run step whose
   config dir is redirected is refused (`mcp_config_dir_redirected`). The
   run gate refuses a start whose granted MCP functions are not pinned
   (`mcp_tools_unpinned`). No refusal names a command an agent could
   run. Uninstalling a connector, or revoking the approval of a
   connector or an account, forgets its pins.
3. Send once over MCP (capability `mcp-send-once`): a non-read-only MCP
   function must declare `idempotency` (`marker`, `none` or `repeatable`;
   `key` is refused, the protocol has no key). A failure before the
   `tools/call` request is handed over settles `not_sent` and journals no
   call (the send intent is recorded only after the server started,
   listed and passed the pin checks, so nothing is recorded before);
   every path checks the pin store, the binding and the program file
   before it resolves a secret, so a `{{cmd:...}}` helper never runs for
   a call the pins refuse; arguments that are not a JSON object are refused `invalid_args`
   before anything starts; for a write every answer received after the
   hand-over that is not a success (`isError: true`, a task handle, an
   `input_required` answer, a JSON-RPC error of any code, -32602
   included) is the new outcome `tool_error` and settles `unknown`; a transport
   failure or timeout after hand-over settles `unknown`, and a call no
   byte of which reached the server settles `not_sent`. The session of a
   successful call is closed after the call is journaled and settled; a
   failed call closes it first, so what the close proved reaches the
   settlement.
4. Stdio transport (capability `mcp-stdio-transport`): a new crate
   `apb-mcp-client` (depends on `apb-core` only) wraps the rmcp 3.9.0
   client (the version `apb-mcp` already pins) in a blocking session
   that owns its runtime on its own thread. One server process per call
   from the pinned program file; an environment from a fixed source (the
   account database's home and name, the XDG defaults under it, a fixed
   `PATH` of the system directories with the program's directory last)
   plus the declared variables, and nothing of the calling process; a
   fresh private working directory, never the project root; the
   program-start guard of `apb_core::process_mode` before
   the spawn; one deadline (`timeout_sec`) over connect (the discover
   probe included), list and call; a bounded line reader under rmcp with
   its own line limit (seven times `BODY_CAP` plus 64 KiB, so a body at
   the cap fits in the worst encoder escaping) that refuses a longer
   message by name (`mcp_message_too_large`), and a writer that
   registers each `tools/list` id before its request reaches the server,
   so each page is kept as the raw JSON the server sent; and an end that
   closes stdin, waits, then kills the server's own process group while
   the server is unreaped, reaps it and looks for the group empty; no
   other process is signalled. A failed handshake runs the same bounded
   close before it returns. A killed call's server ends with its caller
   on Linux; elsewhere only on end of its stdin (out of scope 15). The
   server runs as the person, unsandboxed, and the approval says so.
5. Run-side surfaces (capability `mcp-connector-surfaces`): the pin lines
   of `apb connector approve`, the `tool_digest` field of
   `connector_call`, the refusal's next action, the connector doubles of
   eval-doubles answering an mcp function without starting its server
   (no new double mechanism), and a test that a node granted an MCP
   connector still hands the agent no MCP server beyond apb's own. Outside
   a run no MCP tool is called: the dashboard's healthcheck of an MCP
   connector checks the pins without a tool call (start, list, compare,
   close), and the playground refuses an MCP function
   (`mcp_live_call_unsupported`).
6. apb mcp on the current revision (capability `mcp-server-revision`):
   no rmcp change (3.9.0 has every API this change needs); automated
   wire tests for an old-revision client
   (2025-06-18 and 2025-11-25 `initialize`) and a 2026-07-28 client
   (`server/discover` then `tools/list` with per-request `_meta`);
   `docs/MCP.md` and `docs/HOST-INTEGRATION.md` corrected to what the wire
   tests prove. Issue 57 stays open for its manual host matrix (out of
   scope 14); the pull request says "Refs #57".

## Out of scope

1. A connector broker for isolated nodes. V113 keeps refusing any
   connector on a node with `isolation`
   (`crates/apb-core/src/validate/isolation.rs` lines 38 to 47); a broker
   opens a channel out of the sandbox and is its own security design.
   Decided below.
2. MCP resources, prompts, completion, subscriptions (`subscriptions/listen`),
   and the deprecated roots, sampling and logging
   (https://modelcontextprotocol.io/specification/2026-07-28/deprecated).
   apb declares no client capability, so a server may not rely on any
   (https://modelcontextprotocol.io/specification/2026-07-28/basic/index,
   "_meta": "A server MUST NOT rely on capabilities the client has not
   declared").
3. Multi round-trip requests, MCP Tasks for runs and MCP Apps (issue 57
   follow-ups, filed as new issues when it closes). A tool that answers
   `input_required` or a task handle is reported, not driven, and its
   write settles `unknown`.
4. Streamable HTTP with authorization, `apb connector tools`, the
   `apb connector doctor` rows, the MCP `connectors_list` fields, the
   dashboard section and the contract expectation kind `mcp`. They are the
   next change, mcp-http-and-surfaces, which builds on this one. Until it
   merges, `transport: http` is refused ("mcp transport `http` is not
   supported by this apb"), and a person sees a server's tools through the
   approve output.
5. Session reuse across calls. Each `apb connector call` is its own
   process (`crates/apb-engine/src/connector/call/mod.rs` lines 1 to 5), so
   a server is started per call; the stateless core makes this correct
   (https://modelcontextprotocol.io/specification/2026-07-28/basic/index,
   "Statelessness"). A pool is a performance change for later.
6. MCP functions as notification targets. `FunctionSpec::carries_notices`
   answers false for them, so `apb notify` never sends through a server
   it would have to start from the drive.
7. Bundled MCP connectors and an official MCP connector. None ships; the
   docs carry the examples.
8. Widening the run's irreversible consent from connector function flags.
   The plan asked for MCP irreversible functions "in the run's
   irreversible consent (consent.rs)"; the code derives that consent from
   the playbook's declared and inferred effects only
   (`crates/apb-core/src/effects.rs` lines 120 to 129) and no connector's
   `irreversible` flag feeds it today. MCP functions get exactly the reach
   HTTP functions have: V73 (`validate/decisions.rs` `shipping_reason`,
   line 95, reading `irreversible_functions` at line 119) and the
   unattended rules (`validate/unattended.rs`). Changing that for every
   connector is a separate decision.
9. The plan's "apb connector test lists the server's tools". `apb connector
   test` is the offline contract runner (`crates/apb-cli/src/connector.rs`
   lines 106 to 115) and stays offline; the live listing is `apb connector tools` of
   mcp-http-and-surfaces, and here the approve output.
10. The plan's "the expected tools of an attempt include the granted MCP
    functions". An agent calls a connector only through the shell
    (`crates/apb-engine/src/scheduler/node/exposure.rs` lines 4 to 6), so
    an MCP grant adds no tool to the agent; the expected set is unchanged,
    and a test pins that no MCP server reaches the agent. The leftover of
    #224 (statuses against a real claude init capture) needs a model call
    on the owner's login and has nothing to do with MCP connectors: it
    moves out (item 14).
11. The plan's "the import-approval rule of scripts" for the stdio
    command. Bundles carry no connectors (`crates/apb-core/src/bundle/` has
    no connector reference), and a connector's tree digest is already
    approved by exact digest, which `acknowledge_untrusted` never bypasses
    (`crates/apb-engine/src/gate/mod.rs` lines 1290 to 1323, the acknowledge ignored at line 1296). The command
    is part of that digest; approval output shows the command line. A digest of the string does
   not cover the file a bare name finds, so the approval also pins the
   program by its absolute path and file digest (design D2), and the
   server's environment never comes from the calling process.
12. The plan's "emit ttlMs and cacheScope only on 2026-07-28 requests".
    rmcp 3.9.0 already does exactly that (`fill_missing_cache_hints`
    for 2026-07-28 peers, nothing for older ones); apb keeps rmcp's values
    (`ttlMs: 0`, `cacheScope: private`) and pins the behaviour with wire
    tests instead of overriding `list_tools`.
13. The plan's "byte-identical old-revision transcript test from PR 61".
    No such automated test exists (PR 61 compared binaries by hand); this
    change adds it (`mcp_revision_test.rs`).
14. The manual host matrix and the tier-0 re-measurement of issue 57
    (Claude Code on both revisions, Codex, opencode, Hermes, the
    supervisor long-poll) and the claude init capture of #224. Each needs
    real host CLIs and model calls on the owner's login, which an
    implementer cannot do unattended. They move to a later change the
    owner schedules (recommended: next to mcp-http-and-surfaces, which
    edits the same two docs). Issue 57 stays open until then.
15. Ending what leaves the server's process group, and the server of a
    killed call on every platform. apb starts the server as the leader of
    its own process group and, at the close, kills that group while the
    server is unreaped, so a worker the server forked in its group ends
    with it; a process that left the group (`setsid`, its own group) is
    not chased, and the close only reports when the group was not seen
    empty. When the calling process itself is killed, on Linux its server
    gets SIGKILL with it (`PR_SET_PDEATHSIG`), which does not reach the
    server's own workers; on other platforms the server ends only if it
    exits on end of its stdin, so a killed caller whose server ignores
    end of input leaves it running. A cleanup of survivors, a scope that
    owns every descendant (a cgroup or a pidfd tree), is future work.
16. A playground call of an MCP function, and any live MCP call outside
    a run beyond the healthcheck's pin check. Refused by name in this
    version, before any secret is resolved.
17. Approval of an MCP connector without a terminal, for example in
    provisioning scripts. Any non-terminal form has to check an expected
    digest the caller supplies; a refusal that printed the expected
    digest would let the caller supply it on a retry, with no person
    having seen it, so this change has no such form and its refusal
    prints no digest. A form that keeps a person in the loop is future
    work.
18. Running MCP servers under apb's isolation sandbox. A server runs as
    the person, unsandboxed: it can read and change what the person can,
    including apb's own files and the secrets of other connectors. This
    version starts it in a fresh private working directory and says so
    once at the approval; running MCP servers under apb's isolation
    sandbox is future work.

## Decisions taken in this design

1. MCP connectors on isolated nodes: refused in 1.0 (V113 stays, its
   message now names the MCP reason); a broker is designed after 1.0. A
   broker in this change would roughly double it and open a channel out
   of the sandbox before 1.0.
2. The manual host matrix of issue 57 and the claude init capture leave
   this change (out of scope 14): they need model calls on the owner's
   login, which the implementer cannot make.
3. The server leads its own process group, which the close kills while
   the server is unreaped (out of scope 15). The first design kept it in
   the caller's group; the owner reversed that in fix round 1 (design,
   review record).
4. The program that receives an account's secrets is fixed at approval
   (absolute path and file digest in the pin record), and its
   environment is built from a fixed source, never the caller's.
5. Pinning, and every approval of an MCP connector (its tree included),
   is a person's act at an interactive terminal: typed confirmations
   only, with no non-terminal form; refused without a terminal, in a run
   step, over MCP and over HTTP. The program is confirmed before any secret
   is resolved, the tools after the listing. A pin binds the approved
   tree digest, the account digest, the program and the tools as one
   binding compared in one place. Other
   connector kinds keep main's approve.
6. One call runs in the order: admission decision, start and pin
   checks, send intent, dispatch, journal and settlement, then the
   close; a failed call closes before its journal and settlement. No
   intent is recorded before the server started and its pins were
   checked, on any path.
7. rmcp stays at 3.9.0 for both crates.
8. An MCP function at an engine site is refused (V302) in this version;
   lifting it is future work.
9. No run guard for MCP runs: an older apb fails every MCP call closed
   before any process starts (it cannot parse the run's connector
   snapshot), which a journal corpus row records against the released
   binaries.

## Impact

1. Crates: new `apb-mcp-client` (rmcp client over the child's pipes, a
   blocking session, a stub server for tests); `apb-core` (connector `mcp`
   schema and rules, `FunctionKind::Mcp`, `connector/mcp/` with the tool
   digest and the pin store, `ConnectorFacts.mcp_transport` and
   `mcp_launcher`, validator module `validate/mcp.rs` with V300 to V302,
   the V113 message, `config::account_entry`); `apb-engine`
   (`connector/mcp/`: connect, server, map, pin; `call/mod.rs` the order
   of one live call and the close after settlement; `send_once::preview`;
   `result.rs` `CallOk::Mcp` and `ToolError`; `effect.rs`;
   `grade/rules/consent.rs`; `event.rs` `tool_digest`; gate trust scan and
   refusal; next actions; fit; prompt; an eval doubles test); `apb` CLI
   (the person's approval at a terminal, a journal corpus row, the check
   that `testing` features stay in test builds); `apb-server` (one struct
   literal, one test literal, the approve route's refusal of an MCP
   connector, and the client with `testing` as a dev-dependency only);
   `apb-core` also forgets the pins in `install::uninstall` and
   `TrustStore::revoke`. `apb-mcp` is not changed (no rmcp
   change). No web change.
2. Bundled content: none.
3. Journal: additive field `connector_call.tool_digest`
   (`#[serde(default, skip_serializing_if = "Option::is_none")]`); a new
   string `tool_error` in `connector_call.outcome`, a free `String`
   (`crates/apb-engine/src/event.rs` line 1125). No new event type, no new
   run guard, no manifest mark. A run that uses no MCP connector writes
   the same journal as before. An older apb reads `tool_error` without
   error and counts it a failed call in its outcome view; the send
   ledger's own `send_intent_settled` stays the authority in both
   binaries. An older apb cannot load a connector manifest with an `mcp:`
   block (`ConnectorDoc` and `FunctionSpec` deny unknown fields,
   `crates/apb-core/src/connector/def.rs` lines 557 and 366, and so do
   v0.25.0 and v1.0.0-beta.1): it refuses to start such a run
   (`connector_unresolved`), and in a run it resumes, every call of such
   a function fails on the run's connector snapshot
   (`crates/apb-engine/src/connector/call/mod.rs` line 1191) before any
   process starts and sends nothing. Journal corpus row M1 checks it
   against the released binaries.
4. New state: `<config-dir>/mcp-tools.json` (machine-wide, like
   `trust.json`): one record per connector account with the approved
   tree digest, the program path, the program digest, the tool digests
   and the pin digest. Nothing new under `.apb/`.
5. Behaviour changes for release notes:
   1. Connectors can map the tools of an MCP server over stdio. An older
      apb cannot load a connector manifest with an `mcp:` block.
   2. `apb connector approve <c> --account <a>` also pins the program and
      the tools of an MCP connector under its tree digest, as a person's
      act at an interactive terminal in two typed confirmations;
      approving an MCP connector without `--account` is a person's act at
      a terminal too, and no script, run step, MCP tool or dashboard
      approves one; a changed
      definition, program or tool is refused until the account is
      approved again.
   3. A run start whose granted MCP tools are not pinned is refused
      `mcp_tools_unpinned`, with a manual next action.
   4. A new call outcome `tool_error`: an MCP tool reported an error,
      asked for input or started a task, or the server answered with a
      JSON-RPC server error, and whether it acted is unknown.
   5. Wire tests pin what `apb mcp` (rmcp 3.9.0, unchanged) serves to
      old-revision and 2026-07-28 hosts.
   6. The healthcheck of an MCP connector checks its pins without a tool
      call; the playground does not call MCP functions yet.
   7. The program that runs is the file that was hashed; the server runs
      in a fresh private working directory, as the leader of its own
      process group, which the close ends; the approval warns for an
      interpreter or a launcher and says once that the program runs as
      you, unsandboxed; an `env` name that changes what code the server
      loads is refused at install.
6. Built on main efae5822 (1.0.0-beta.7, after #287 typed inbox reads,
   #288 people in the loop, #289 before running alone, #290 wait park),
   checked there. Of the in-flight changes, eval-doubles, inbox-waits,
   start-rules and incremental-lists-and-session-fork merge first or
   beside this one; every line is re-verified on merged main (tasks.md
   0). eval-doubles is the one precondition: its connector doubles answer
   an mcp function without starting the server (design D5 "Eval
   doubles"). The shared files and who owns which symbol are in design
   "Overlap with the in-flight changes". mcp-http-and-surfaces follows and
   depends on this change.
7. Validator codes V300 to V302 of the family range V300 to V319 (free on
   main, whose highest code is V250; the in-flight changes take V251 to
   V299 or reserve them). V303 to V319 stay reserved for this family.
8. 84 changed files by the list of design "Files": the ceiling for this
   change was 80, and fix round 1 passed it by four with the files its
   fixes needed (design "Files" and the review record).
