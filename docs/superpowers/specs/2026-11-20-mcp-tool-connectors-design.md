# MCP tool connectors

Design record of the change, from its proposal and design. Built on main
at 1.0.0-beta.7 (after "Typed inbox reads" #287, "People in the loop" #288,
"Before running alone" #289 and "Wait park" #290), rebased onto "Eval
doubles" #291 and "Secret params are never rendered" #292. Where the text
says "the work plan", it means the ordered list of work items the change
kept next to this design.

---

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
6. Built on main at 1.0.0-beta.7 ( after #287 typed inbox reads,
   #288 people in the loop, #289 before running alone, #290 wait park),
   checked there. Of the in-flight changes, eval-doubles, inbox-waits,
   start-rules and incremental-lists-and-session-fork merge first or
   beside this one; every line is re-verified on merged main (work plan item
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

---

# Design: MCP tool connectors

Read the proposal above first. Every decision below names its files, its
functions and the line where the code is today, all checked on main at
1.0.0-beta.7, which holds typed-inbox-reads (#287),
people-in-the-loop (#288), before-running-alone (#289) and wait-park
(#290). Four changes are in flight and merge before or beside this one:
eval-doubles (implemented, in review), start-rules (being implemented),
inbox-waits and incremental-lists-and-session-fork (specified). Every line
number here is re-verified on merged main (work plan item 0); where one of them
moves or adds a symbol this design uses, "Overlap with the in-flight
changes" below says which. If a line moved, find the site by the name
given next to it, never by the number alone. Every rule of the Model Context
Protocol this design relies on is cited with its page of the 2026-07-28
revision (the current one,
https://modelcontextprotocol.io/specification/versioning: "The current
protocol version is 2026-07-28").

Six decisions and the shared pieces, built in the order D6 (the wire
tests of `apb mcp` on the rmcp it already pins, first, so they record the
wire before anything else changes), D0, D1, D2, D4, D3, then D5 with the
docs of D6 (the work plan). D4 (the client and the stdio call) comes before D3
(the send-once mapping) because D3 maps the outcomes D4 produces.

This change ships a complete stdio feature: the kind, the pins, send
once, the stdio call path and the gate refusal are one trust boundary.
The next change, mcp-http-and-surfaces, adds the Streamable HTTP
transport with authorization (its D7) and the listing surfaces (its D5:
`apb connector tools`, the `apb connector doctor` rows, the MCP
`connectors_list` fields, the dashboard section and the contract
expectation kind `mcp`). It builds on the types and functions named here
and redefines none of them; nothing in this change refers to a piece it
adds, except the one refusal of `transport: http` (D1), which that change
removes.

Nothing in this document is left to the implementer. Where a choice was
open it is decided here and the reason is given; the work plan only orders the
work.

## Protocol facts this design relies on

| Rule | Where |
|---|---|
| A stdio client launches the server as a subprocess; messages are newline-delimited JSON-RPC; the server MAY log to stderr and the client SHOULD NOT read stderr as an error | https://modelcontextprotocol.io/specification/2026-07-28/basic/transports/stdio |
| Shutdown: close the server's stdin, wait, then SIGTERM, then SIGKILL (this design skips the SIGTERM step, D4 item 6) | same page, "Shutdown" |
| A dual-era client SHOULD probe with `server/discover` first; a correlated non-modern error or no answer means a legacy server, then fall back to `initialize`; the fallback MUST NOT be keyed to one error code | same page, "Backward Compatibility"; https://modelcontextprotocol.io/specification/2026-07-28/basic/versioning |
| Every 2026-07-28 request carries `io.modelcontextprotocol/protocolVersion` and `clientCapabilities` in `_meta`; a server MUST NOT rely on capabilities the client did not declare | https://modelcontextprotocol.io/specification/2026-07-28/basic/index ("_meta") |
| An absent `resultType` MUST be treated as `complete` | same page, "ResultType" |
| Servers MUST NOT rely on prior requests over the same connection (stateless) | same page, "Statelessness" |
| `tools/list` is paginated by `cursor` / `nextCursor` and carries `ttlMs` and `cacheScope` on 2026-07-28 | https://modelcontextprotocol.io/specification/2026-07-28/server/tools ("Listing Tools"); https://modelcontextprotocol.io/specification/2026-07-28/changelog (minor change 5) |
| The tool list MUST NOT vary per connection and MAY vary by the authorization presented | tools page, "Capabilities" |
| Clients MUST consider tool annotations untrusted unless they come from trusted servers | tools page, "Tool" |
| Two error channels: protocol errors are JSON-RPC errors (unknown tool, malformed request, server errors); tool execution errors are results with `isError: true` (API failures, invalid input, business logic) | tools page, "Error Handling" |
| A `tools/call` MAY answer `resultType: input_required` (multi round-trip) | tools page, "Input Required Tool Results" |
| A tool MAY return structured content (any JSON value) and SHOULD also return it serialized as text | tools page, "Structured Content" |
| HTTP+SSE, roots, sampling, logging and dynamic client registration are deprecated (apb uses none of them) | https://modelcontextprotocol.io/specification/2026-07-28/deprecated |

## The rmcp client this design uses

rmcp 3.9.0, the version `apb-mcp` already pins exactly
(`crates/apb-mcp/Cargo.toml` line 18, `=3.9.0`; `Cargo.lock` holds 3.9.0
and nothing newer). The new client crate pins the same `=3.9.0`, since
Cargo cannot hold two semver-compatible exact pins of one crate. No rmcp
change: every client API this design needs is in 3.9.0, read in the local
cargo registry (`~/.cargo/registry/src/*/rmcp-3.9.0/`). The table below is
that reading; every line was opened.

| API | Where in rmcp 3.9.0 |
|---|---|
| `rmcp::service::ClientServiceExt::serve_with_lifecycle(self, transport, ClientLifecycleMode)` | `src/service/client.rs` lines 726 to 738 |
| `ClientLifecycleMode::{Initialize, Discover { preferred_versions }, Auto { preferred_versions, legacy_version }}` (`#[non_exhaustive]`); `Auto` falls back to `initialize` on a correlated non-modern error or after `DEFAULT_AUTO_DISCOVER_TIMEOUT`, a fixed 10 s the public API does not let a caller change | `src/service/client.rs` lines 707 to 722, the fallback at 864 to 900, the fixed timeout passed at 820 |
| `ClientInitializeError` (with `LegacyFallbackFailed { discover, fallback }`) | `src/service/client.rs` line 45 |
| `Peer<RoleClient>::list_tools(Option<PaginatedRequestParams>) -> ListToolsResult` (one page; the typed result, cached per cursor by the peer) | `src/service/client.rs` lines 1888 to 1935 |
| `RunningService::call_tool_once(CallToolRequestParams) -> CallToolResponse` | `src/service/client.rs` line 2139 (and the `Peer` form at 1475) |
| `CallToolResponse::{Complete(CallToolResult), InputRequired(InputRequiredResult), Task(CreateTaskResult)}` | `src/model/mrtr.rs` lines 105 to 113 |
| `CallToolRequestParams` | `src/model.rs` line 4413 |
| `CallToolResult { content, structured_content: Option<Value>, is_error: Option<bool>, meta, .. }` (an explicit `"structuredContent": null` is kept as `Some(Value::Null)`) | `src/model.rs` lines 4139 to 4180 |
| `Tool { name, title, description, input_schema, output_schema, annotations, icons, meta }`: fixed fields, no catch-all member, so a member the struct does not name is dropped on decode (the reason D2 digests the raw JSON, never this struct) | `src/model/tool.rs` lines 17 to 40 |
| `ServerResult` is `#[serde(untagged)]` with `CustomResult` last, so a `tools/list` answer always decodes as the typed `ListToolsResult` first, a custom request included | `src/model.rs` lines 1093 to 1098 (`CustomResult`) and 5009 to 5034 (`ServerResult`); the same reason is written at 1874 to 1878 |
| `ServiceError::{McpError(ErrorData), TransportSend(_), TransportClosed, UnexpectedResponse, SubscriptionLagged { .. }, Cancelled { .. }, Timeout { .. }, InputRequiredRoundsExceeded { .. }}` | `src/service.rs` lines 79 to 97 |
| `Peer::peer_info()` | `src/service.rs` line 1024 |
| `ProtocolVersion::V_2026_07_28`, `V_2025_11_25` | `src/model.rs` lines 169 to 170 |
| `ClientConfig` (= `InitializeRequestParams`) | `src/model.rs` lines 1236 and 1363 |
| `impl IntoTransport<Role, io::Error, TransportAdapterAsyncRW> for (R, W)` where `R: AsyncRead`, `W: AsyncWrite`: the child's pipes are a transport (feature `transport-async-rw`) | `src/transport/async_rw.rs` line 24 |
| `AsyncRwTransport` caps one incoming line at `DEFAULT_MAX_LINE_LENGTH` (16 MiB) and drops a longer line and keeps reading, so an oversize answer never arrives and its call would only time out (the reason D4 puts its own bounded reader under rmcp) | `src/transport/async_rw.rs` lines 58, 116 and 283 to 290 |

Features: `default-features = false`, `features = ["client",
"transport-async-rw"]`. Not `transport-child-process`: its
`TokioChildProcess::graceful_shutdown` sends SIGTERM by pid
(`child.signal(libc::SIGTERM)`, `src/transport/child_process.rs` line
115), which this design never does (D4 item 6). apb spawns the child
itself and hands rmcp the two pipes, each wrapped (D4 item 3). No HTTP
transport feature and no `auth` feature: this change speaks stdio only
(mcp-http-and-surfaces adds the Streamable HTTP client features, and
moves both crates to a newer rmcp together if it needs one).

## D0. Shared pieces

### The new crate `apb-mcp-client`

The engine has no async runtime (`crates/apb-engine/Cargo.toml` lists no
`tokio`) and its connector path is blocking end to end (`ureq`, `lettre`,
`imap`). The dashboard calls that blocking path straight from an async
handler (`crates/apb-server/src/routes/connectors/mod.rs` line 441,
`healthcheck_connector_handler`), so a client that ran `block_on` on the
caller's thread would panic there ("Cannot start a runtime from within a
runtime"). The client therefore lives in its own crate with its own
runtime thread:

1. `crates/apb-mcp-client/`, package `apb-mcp-client`, edition and version
   from the workspace. Dependencies: `apb-core` (path, for
   `apb_core::secret::Secret` only), `rmcp = { version = "=3.9.0",
   default-features = false, features = ["client",
   "transport-async-rw"] }` (the version `apb-mcp` pins, "The rmcp
   client this design uses"), `tokio.workspace = true` (the workspace
   enables `full`, which holds `process`, `io-util`, `rt`, `time` and
   `sync`), `serde_json.workspace = true`, `thiserror.workspace = true`,
   `sha2.workspace = true` (the digest of a program file, read from the
   descriptor that starts it) and `libc = "0.2"` (Unix only, like every
   target apb ships: the descriptor a program starts from, the server's
   process group, `killpg`, and the unreaped exit check, `waitid`).
   Feature `testing = []` (the stub server below); `lib.rs` carries
   `#[cfg(all(feature = "testing", not(debug_assertions)))]
   compile_error!(..)`, as apb-core, apb-engine and apb-decide do, so no
   build without debug assertions carries the stub. Dev-dependencies:
   itself with `testing`, `apb-core` with `testing` (for
   `process_mode::testing`), `tempfile = "3.27.0"`.
2. Add it to `[workspace] members` in the root `Cargo.toml` (line 3),
   after `crates/apb-decide`.
3. `apb-engine` depends on it (`apb-mcp-client = { path =
   "../apb-mcp-client" }` next to `apb-decide`, `crates/apb-engine/Cargo.toml`
   line 12, and with `features = ["testing"]` in `[dev-dependencies]` next
   to line 70). The `apb` CLI package and `apb-server` get the same
   dependency with `testing` for their tests, under `[dev-dependencies]`
   only: a `testing` feature turned on under `[dependencies]` would reach
   every build of the `apb` binary through feature unification.
   `release_workflow_test::testing_features_stay_in_test_builds` checks
   that every workspace crate that defines a `testing` feature has the
   guard and that no crate turns a workspace `testing` feature on outside
   its dev-dependencies.
4. Direction: `apb-core <- apb-mcp-client <- apb-engine`. The client
   never imports the engine; it knows nothing about runs, grants, pins or
   the send ledger. It speaks the protocol and returns plain
   `serde_json::Value` data; every apb rule lives in the engine.

### Validator codes

| Code | Severity | Decision | Rule |
|---|---|---|---|
| V300 | warning | D1 | a binding of an MCP connector names no `functions` allowlist, so it grants every function the manifest maps |
| V301 | warning | D1 | a binding of an MCP connector whose stdio `command` is a known interpreter or launcher (`MCP_LAUNCHERS`): the approval covers that program file only, not the script or package it runs |
| V302 | error | D1 | an MCP connector bound at an engine site (the baseline check, the follow-up check or a list's item check `<node>#check`): those reads run from the drive or a detached waker, whose server environment, working directory and stop path this version does not define |

V113 keeps its code and severity; D1 adds an MCP sentence to its message.
The 1.0 plan assigns V300 to V319 to this family. The codes below it, on
main and in flight (read from each change's design):

| Codes | Owner | State |
|---|---|---|
| V240 to V247, V249, V250 | people-in-the-loop, before-running-alone | on main (highest V250) |
| V248, V251 to V259 | people-in-the-loop family | reserved |
| V260 to V267 (V268 to V279 reserved) | start-rules | in flight |
| V280, V281 (V282 reserved) | eval-doubles | in flight |
| V283 | incremental-lists-and-session-fork | in flight |
| V284 to V286 (V287 to V299 reserved) | inbox-waits | in flight |
| V300 to V302 | this change | new |
| V303 to V319 | this family | reserved (mcp-http-and-surfaces takes none) |

No in-flight change uses or reserves a code at V300 or above; the work plan
0.2 checks it again on merged main. The connector manifest rules of D1
(refused when the manifest loads) carry no V code, as every manifest rule
on main; the two that a person meets often start their message with a
stable name instead (`mcp_timeout_too_small:`, D1 rule 3.4.4, and the
call refusals below).

### Refusal and error codes

| Code | Kind | Where |
|---|---|---|
| `mcp_tool_unpinned` | call error, `CallErrorCode::Permission`, nothing sent | D2, every call |
| `mcp_tool_changed` | call error, `CallErrorCode::Permission`, nothing sent | D2, every call |
| `mcp_tool_missing` | call error, `CallErrorCode::Config`, nothing sent | D2, the server no longer lists the mapped tool |
| `mcp_server_unpinned` | call error, `CallErrorCode::Permission`, nothing sent | D2, every call: no server pin for the connector and account |
| `mcp_server_changed` | call error, `CallErrorCode::Permission`, nothing sent | D2, every call: the definition's `command` is not the pinned one, or the pinned program file is gone, a link or not a regular file, or its digest differs; on a platform that starts the program by its path (D4), also the path hashed again right before the spawn |
| `mcp_config_dir_redirected` | call error, `CallErrorCode::Permission`, nothing sent | D2, a call from a run step whose config dir is redirected |
| `mcp_message_too_large` | call error (`CallErrorCode::Service` before hand-over, the lost-answer codes after it, D3) | D4, a server line over the line cap |
| `mcp_tools_unpinned` | run gate policy refusal | D2, run start |
| `mcp_approve_needs_a_person` | CLI refusal, exit 2; dashboard 403 | D2, an approval of an MCP connector anywhere but at an interactive terminal; it names no digest |
| `mcp_approve_definition_changed`, `mcp_approve_account_changed` | CLI refusal, exit 2 | D2, the connector or the account changed between the program confirmation and the listing |
| `tool_error` | new `CallErrorCode::ToolError`, a call outcome | D3 |
| `mcp_env_loads_code` | manifest rule, `ConnectorError::Invalid` when the manifest loads (install included) | D1 rule 3.4.6, an `mcp.env` name that changes what code a process loads |

The call refusals are `CallError` values whose message starts with the
code and a colon (`mcp_tool_changed: ...`); `error.code` stays the coarse
class (`permission`, `config` or `service`), so every existing reader of
the code keeps working. The CLI refusals print the same way on stderr.
No refusal text names a command line an agent could run to clear it: an
approval is a person's act at a terminal (D2 "Capturing pins"), and the
texts point to `docs/CONNECTORS.md` "MCP servers" instead.

### Test server (`apb_mcp_client::testing`)

Every test in this change that needs a stdio server uses one stub, a
POSIX `sh` script, so no extra binary is built and the engine and CLI
tests use it through the dev-dependency:

```rust
/// A scripted stdio MCP server for tests. `write` puts an executable
/// `stub-mcp.sh` in `dir`. The script reads its behaviour from files in
/// `dir` on every request, so a test changes the server between two calls
/// by rewriting a file.
pub struct StubServer {
    pub dir: std::path::PathBuf,
}

pub enum StubEra {
    Modern,
    Legacy,
    LegacySilent,
}

pub enum StubCall {
    /// Answer with this JSON object as the `result` member, after
    /// appending the request line to `calls.log`.
    Result(serde_json::Value),
    /// Answer with this JSON-RPC error (after logging the line).
    Error { code: i64, message: String },
    /// Log the line, then exit 1 without answering.
    ExitAfterRead,
    /// Log the line, then never answer.
    Hang,
    /// Log the line, then answer one result line of about this many bytes
    /// (a text block of `a` characters), for the line cap of D4 item 3.
    Oversize(usize),
    /// Log the line, then answer with this text as the `result` member,
    /// byte for byte (a test writes its own escaping or a large dropped
    /// block, D4 item 3.1).
    RawResult(String),
}

impl StubServer {
    pub fn write(dir: &std::path::Path, era: StubEra) -> std::io::Result<StubServer>;
    /// `dir/stub-mcp.sh`
    pub fn program(&self) -> std::path::PathBuf;
    /// Writes `dir/tools.json`, the JSON array `tools/list` answers.
    pub fn set_tools(&self, tools: &serde_json::Value) -> std::io::Result<()>;
    /// Writes `dir/call.mode` and `dir/call.json` (`dir/call.raw` for
    /// `RawResult`).
    pub fn set_call(&self, call: StubCall) -> std::io::Result<()>;
    /// Lines of `dir/calls.log` (one per tools/call request received).
    pub fn calls(&self) -> Vec<String>;
    /// Variable names the last start saw (`dir/env.log`).
    pub fn env_seen(&self) -> Vec<String>;
    /// The value the last start saw for `name` (`dir/env.values`, the
    /// whole `env` output; test values only, never a real secret).
    pub fn env_value_seen(&self, name: &str) -> Option<String>;
    /// Arguments the last start got (`dir/args.log`), one per line.
    pub fn args_seen(&self) -> Vec<String>;
    /// Lines of `dir/starts.log` (one per server start); empty when absent.
    pub fn starts(&self) -> Vec<String>;
    /// Lines of `dir/methods.log` (one per JSON-RPC message received).
    pub fn methods(&self) -> Vec<String>;
    /// The working directory of the last start (`dir/cwd.log`).
    pub fn cwd_seen(&self) -> Option<std::path::PathBuf>;
    /// The pid of the last start (`dir/pid.log`), for a liveness or
    /// group check in tests only: `is_alive(pid)` runs `sh -c 'kill -0
    /// <pid>'` (the null signal: it checks existence and sends nothing, on
    /// a process the test itself started) and `process_group(pid)` runs
    /// `ps -o pgid= -p <pid>`; both are POSIX and work on Linux and macOS,
    /// so no test marked `#[cfg(unix)]` reads `/proc`. Never used to send
    /// a signal.
    pub fn last_pid(&self) -> Option<u32>;
    /// Whether `pid` (one of `last_pid`'s) still exists: `kill -0`.
    pub fn is_alive(pid: u32) -> bool;
    /// The process group of `pid`: `ps -o pgid= -p <pid>`.
    pub fn process_group(pid: u32) -> Option<u32>;
    /// Creates or removes `dir/ignore_eof.flag`.
    pub fn set_ignore_eof(&self, on: bool) -> std::io::Result<()>;
    /// Creates or removes `dir/fork.flag`: each start then leaves a worker
    /// in its process group that sleeps and outlives the server (its pid in
    /// `dir/worker.pid`), for the group close of D4.
    pub fn set_fork_worker(&self, on: bool) -> std::io::Result<()>;
    pub fn worker_pid(&self) -> Option<u32>;
    /// The `$0` of the last start (`dir/argv0.log`).
    pub fn argv0_seen(&self) -> Option<String>;
    /// Creates or removes `dir/exit_after_list.flag`: the stub closes its
    /// stdin, answers a `tools/list` and exits, so no later request
    /// reaches it (a call that is never handed over, D3).
    pub fn set_exit_after_list(&self, on: bool) -> std::io::Result<()>;
    /// What the working directory held at the last start (`ls -A`).
    pub fn cwd_entries_seen(&self) -> Vec<String>;
    /// Whether `pid` has exited (gone, or a zombie): `ps -o stat=`.
    pub fn has_exited(pid: u32) -> bool;
}
```

The script, embedded as `const STUB_SH: &str`:

1. At start it finds its own directory: started from its descriptor
   (Linux, D4), its `$0` is `/proc/self/fd/<n>`, so it resolves that
   name through the link in its own `/proc/$$/fd` (a replaced file reads
   `<path> (deleted)`, whose directory is the same); else it takes `$0`.
   It appends one line to `starts.log`, writes `$0` to `argv0.log`, with
   `fork.flag` present starts a sleeping worker in the stub's process group
   (`worker.pid`), writes `$$` to `pid.log`, the working directory
   (`pwd`) to `cwd.log` and its entries to `cwd_entries.log`, the variable
   names (`env | sed 's/=.*//'`) to `env.log`, the whole `env` output to
   `env.values`, and each argument on its own
   line to `args.log`; when the variable `STUB_STDERR` is set it prints
   its value on stderr.
2. It reads stdin line by line. For each line it extracts the numeric id
   with `sed -n 's/.*"id":\([0-9][0-9]*\).*/\1/p'` and the method with a
   `case` on the line (rmcp writes compact JSON, so `"method":"tools/list"`
   appears literally), and appends the method to `methods.log`.
3. `server/discover`: `Modern` answers `{"resultType":"complete",
   "supportedVersions":["2026-07-28"],"capabilities":{"tools":{}},
   "_meta":{"io.modelcontextprotocol/serverInfo":{"name":"stub",
   "version":"1"}},"ttlMs":0,"cacheScope":"private"}`; `Legacy` answers
   the JSON-RPC error `-32601` "Method not found"; `LegacySilent` answers
   nothing.
4. `initialize` (legacy eras): answers `{"protocolVersion":"2025-11-25",
   "capabilities":{"tools":{}},"serverInfo":{"name":"stub",
   "version":"1"}}`. `notifications/initialized` gets no answer.
5. `tools/list`: answers `{"tools": <tools.json>}` (when `tools.json` is
   an array of arrays, page `i` with `"nextCursor":"p<i+1>"` until the
   last; a request with `"cursor":"pN"` gets page `N`), plus
   `"resultType":"complete","ttlMs":0,"cacheScope":"private"` in the
   modern era. With `exit_after_list.flag` present it closes its stdin
   before the answer and exits after it.
6. `tools/call`: appends the line to `calls.log`, then acts on
   `call.mode` (`result`, `error`, `exit`, `hang`, `oversize`, `raw`;
   `raw` prints `call.raw` as written in place of the result; `oversize`
   prints `{"content":[{"type":"text","text":"` then `head -c <n>
   /dev/zero | tr '\0' a` then `"}]}` as one result line).
   `tools.json` is printed as written, so a test can list a tool with any
   member (an `execution` object a struct would drop, M1 of the review
   record).
7. Any other request: the error `-32601`. A notification: no answer.
8. On end of input it exits 0, unless `dir/ignore_eof.flag` exists: then
   it runs `trap '' TERM` and `while :; do sleep 1; done` (a server that
   ignores both the closed stdin and SIGTERM, for the close test).

Answers are printed with `printf '{"jsonrpc":"2.0","id":%s,"result":%s}\n'`
and `cat` of the JSON files, so no JSON tool is needed. Tests that need it
are `#[cfg(unix)]`, as the existing agent stubs are
(`crates/apb-engine/tests/suite/tool_exposure_test.rs` line 79,
`write_agent`).

### State

`<config-dir>/mcp-tools.json` (D2), next to `trust.json`
(`crates/apb-core/src/trust.rs` line 227, `trust_path`). Nothing under
`.apb/`, so `workspace.rs` `LOCAL_ENTRIES` is unchanged.

## D1. The mcp connector kind

### Manifest schema

A connector-level block and a function-level block. Both are new types in
a new module `crates/apb-core/src/connector/mcp/mod.rs` (declared `pub mod
mcp;` in `crates/apb-core/src/connector/mod.rs` after `pub mod marker;`,
line 25, with a module doc line in the list at the top of that file,
lines 1 to 11).

```rust
/// The connector-level `mcp:` block: how apb reaches the server. One flat
/// struct (serde's `flatten` does not work with `deny_unknown_fields`);
/// `validate_server_shape` enforces which fields go with which transport.
/// mcp-http-and-surfaces adds the http fields `url` and `auth` here.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct McpServerSpec {
    pub transport: McpTransportKind,
    /// Stdio only, required: a literal program, an absolute path or a bare
    /// name. No placeholder. A bare name is resolved once, by the approval
    /// at a person's terminal (D2 "Capturing pins"), to an absolute
    /// canonical path that the pin records; a call never looks it up.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
    /// Stdio only. Each entry may use `{{account.*}}` only.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub args: Vec<String>,
    /// Stdio only. Variable name -> template (`{{account.*}}`, `{{secret.*}}`).
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub env: BTreeMap<String, String>,
    /// How the client opens the conversation; `auto` by default.
    #[serde(default, skip_serializing_if = "McpProtocol::is_auto")]
    pub protocol: McpProtocol,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum McpTransportKind {
    Stdio,
    /// Streamable HTTP. This change refuses it in `validate_server_shape`:
    /// "mcp transport `http` is not supported by this apb";
    /// mcp-http-and-surfaces removes the refusal and adds the http fields.
    Http,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum McpProtocol {
    /// Probe with `server/discover`, fall back to `initialize`.
    #[default]
    Auto,
    /// 2026-07-28 or later only: a legacy server is an error.
    Modern,
    /// `initialize` only (skips the probe and its 10 s silence timeout on
    /// a legacy server that ignores unknown methods).
    Legacy,
}

/// The function-level `mcp:` block: which tool a function calls.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct McpFunctionSpec {
    /// The tool's `name` as the server lists it (case-sensitive).
    pub tool: String,
}
```

`McpProtocol::is_auto(&self) -> bool` is a one-line helper. Two
constants live next to the types:

```rust
/// The smallest `timeout_sec` of an mcp function under `protocol: auto`
/// (rule 3.4.4): rmcp's fixed 10 s discover probe plus 5 s.
pub const MCP_AUTO_MIN_TIMEOUT_SEC: u64 = 15;

/// Program file names that run other code than their own file: launchers
/// that fetch or pick a package at each start, and interpreters that run a
/// script their arguments name (V301, and a line of the approval screen).
/// The approval pins the interpreter's or launcher's own file only.
pub const MCP_LAUNCHERS: [&str; 30] = [
    "npx", "pnpx", "bunx", "uvx", "pipx", "uv", "npm", "pnpm", "yarn", "docker", "podman", "env",
    "node", "nodejs", "deno", "bun", "python", "python3", "sh", "bash", "zsh", "dash", "ksh",
    "fish", "ruby", "perl", "php", "java", "lua", "pwsh",
];

/// The file name of a stdio `command` when it is in `MCP_LAUNCHERS`; a
/// versioned interpreter name (`python3.12`) answers its base.
pub fn launcher_name(command: &str) -> Option<&'static str>;

/// Environment variables a definition may not set on a server because
/// they change what code a process loads (rule 3.4.6): exact names.
pub const LOADER_ENV: [&str; 24] = [/* NODE_OPTIONS .. LUA_PATH, rule 3.4.6 */];
/// Prefixes of the dynamic loader's variables.
pub const LOADER_ENV_PREFIXES: [&str; 2] = ["LD_", "DYLD_"];
/// Whether `name` is in `LOADER_ENV` or under a prefix of
/// `LOADER_ENV_PREFIXES`.
pub fn is_loader_env(name: &str) -> bool;
```

`ConnectorDoc` (`crates/apb-core/src/connector/def.rs` lines 555 to 582,
the struct at line 558) gains, after `webhook` (line 573):

```rust
    /// The MCP server this connector maps (`mcp` functions). Present
    /// exactly when the connector declares `mcp` functions.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mcp: Option<super::mcp::McpServerSpec>,
```

`FunctionSpec` (lines 360 to 456, the struct at line 367) gains, after
`inbox` (line 418):

```rust
    /// The MCP tool this function calls (`super::mcp`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mcp: Option<super::mcp::McpFunctionSpec>,
```

and the methods, next to `is_inbox` (line 480):

```rust
    /// An mcp function calls a tool of the connector's MCP server.
    pub fn is_mcp(&self) -> bool {
        self.mcp.is_some()
    }

    /// Whether the function's results are text written outside the run:
    /// the author's `untrusted_output`, or any mcp function (its server
    /// writes every byte of the answer). Every reader goes through this,
    /// never through the raw field.
    pub fn is_untrusted_output(&self) -> bool {
        self.untrusted_output || self.is_mcp()
    }
```

`carries_notices` (line 502) returns `false` when `self.is_mcp()`
(first line of the body). An MCP server would have to be started from the
drive's notification sweep; 1.0 does not do that (proposal, out of scope
6).

Example (stdio, read and write):

```yaml
name: tracker-mcp
version: 0.1.0
mcp:
  transport: stdio
  command: tracker-mcp-server
  args: ["--workspace", "{{account.workspace}}"]
  env:
    TRACKER_TOKEN: "{{secret.token}}"
account_fields:
  - { name: workspace, required: true }
  - { name: token, required: true, secret: true }
identity: { function: whoami, path: id }
functions:
  - name: whoami
    description: The account the server acts as
    read_only: true
    mcp: { tool: whoami }
  - name: search_issues
    description: Search issues by text
    read_only: true
    mcp: { tool: search_issues }
    args_schema:
      type: object
      properties: { query: { type: string } }
      required: [query]
  - name: list_comments
    description: Recent comments of one issue
    read_only: true
    mcp: { tool: list_comments }
    args_schema:
      type: object
      properties: { issue: { type: string }, since: { type: string } }
      required: [issue]
  - name: create_comment
    description: Add a comment to an issue
    mcp: { tool: create_comment }
    args_schema:
      type: object
      properties: { issue: { type: string }, body: { type: string } }
      required: [issue, body]
    idempotency:
      mode: marker
      arg: body
      format: html_comment
      reconcile:
        function: list_comments
        args_from_send: [issue]
        since_arg: since
        items: comments
        text: body
        author: author.id
        item_ref: url
```

### Manifest rules

`ConnectorDoc::from_yaml` (`def.rs` line 624) gains these rules; each
failure is `ConnectorError::Invalid` with the exact message given. Add
them to the numbered rule list in its doc comment (lines 590 to 623, whose
last rule is 14) as rule 15.

1. The kind count (lines 652 to 699: the flags from line 652, the match
   at line 658, the count-0 message at line 665, the count-above-1
   message at line 695) gains `is_mcp = f.mcp.is_some()` as
   a sixth flag. Count 0: the message becomes "function `{}` is not an
   HTTP call (method + url), a mock, an smtp block, an imap block, an
   inbox block, or an mcp block". Count above 1: "function `{}` must be
   exactly one of: an HTTP call, a mock, an smtp block, an imap block, an
   inbox block, or an mcp block". Count 1 and `is_mcp`: call
   `super::mcp::validate_function_shape(f)`.
2. `validate_function_shape(f: &FunctionSpec) -> Result<(), ConnectorError>`
   in `connector/mcp/mod.rs`:
   1. `tool` is 1 to 128 characters of `[A-Za-z0-9_.-]` (the naming rule
      the specification recommends, tools page "Tool Names"): else
      "mcp function `{f}`: tool name `{t}` must be 1 to 128 of A-Z a-z 0-9 _ - .",
      with `{t}` printed escaped (`escape_debug`), so the message holds no
      control character.
   2. None of `query`, `headers`, `body`, `body_form`, `text_args`,
      `scopes`, `paginate`, `mime`, `decode_base64url`, `content_types` is
      set: "mcp function `{f}` must not set `{field}`; it is an HTTP
      field" (the first one found, in that order).
   3. A function that is not `read_only` declares `idempotency`: "mcp
      function `{f}` changes something and must declare idempotency
      (`marker`, `none` or `repeatable`); the protocol has no
      idempotency key". The mode `key` is refused by rule 4 below.
3. Connector-level, after the webhook rules (they end at line 929):
   1. `doc.mcp.is_some()` exactly when some function `is_mcp`: "connector
      `{c}` declares mcp function(s) {list} but no `mcp:` block" and
      "connector `{c}` declares an `mcp:` block but no mcp function".
   2. With `doc.mcp` present, every function is `mcp` or `mock`: "connector
      `{c}` has an `mcp:` block, so its functions are mcp or mock
      functions only; `{f}` is not". `auth`, `webhook` and `error_when`
      are absent: "connector `{c}` has an `mcp:` block and must not set
      `{field}`" (mcp-http-and-surfaces appends "; MCP authorization lives
      in `mcp.auth`" when it adds that field).
   3. Two mcp functions may name the same tool (a narrower `args_schema`
      for one use) only when both are `read_only`; else "functions `{a}`
      and `{b}` map the same tool `{t}`; only read-only functions may share
      a tool".
   4. `super::mcp::validate_server_shape(&doc.name, mcp)`:
      1. `transport: http` is refused first, before any other check of
         the block: "mcp transport `http` is not supported by this apb"
         (the test `http_transport_is_refused_by_this_apb`; a manifest
         with only `transport: http` reaches this message, since the
         block has no `url` field to deny). `transport: stdio` requires
         `command`: "mcp transport `{t}` requires `{field}`".
      2. Stdio `command`: not empty; no `{{`; contains no `/` or is an
         absolute path (`Path::is_absolute`): "mcp command `{cmd}` must be
         an absolute path or a bare program name".
      3. Stdio `env` names match `[A-Z_][A-Z0-9_]*` and do not start with
         `APB_`: "mcp env name `{n}` must be upper case letters, digits and
         _, and must not start with APB_". A proxy or certificate
         variable (`HTTPS_PROXY`, `SSL_CERT_FILE` and the like) reaches
         the server only this way, with its value from the definition or
         an account field, never from the calling process (D4 "The
         server's environment").
      4. With `protocol: auto` (the default), every mcp function's
         `timeout_sec` is at least `MCP_AUTO_MIN_TIMEOUT_SEC` (15):
         "mcp_timeout_too_small: function `{f}` has timeout_sec {t}, and
         with protocol auto a server that ignores `server/discover` is
         only asked `initialize` after a fixed 10 s probe; set
         timeout_sec to 15 or more, or protocol to modern or legacy".
         rmcp's probe timeout is fixed (`DEFAULT_AUTO_DISCOVER_TIMEOUT`,
         rmcp 3.9.0 `src/service/client.rs` line 722, passed at 820, with
         no public way to change it); 15 leaves 5 s for `initialize`, the
         listing and the call after it. The probe itself is bounded by
         the call's deadline too (D4 item 2), so a smaller timeout would
         fail every call against such a server, never succeed. `modern`
         and `legacy` skip the probe and keep any `timeout_sec`.
      5. No control character (`char::is_control`: U+0000 to U+001F,
         U+007F and U+0080 to U+009F) in `command`, in any `args` entry, in
         any `env` name or in any `env` value template: "mcp `{field}`
         holds a control character" (`{field}` is `command`, `args[{i}]` or
         `env.{name}`; the text names no character). These are the strings
         both confirmations show; the CLI sanitizes them too (D5). The
         check of `command` runs first, before rule 2's message, so no
         install error prints a control character
         (`install_errors_never_print_a_control_character`).
      6. No stdio `env` name that changes what code a process loads:
         "mcp_env_loads_code: mcp env name `{n}` changes what code the
         server loads, which the approval of its program does not cover;
         it cannot be set". The one list is `LOADER_ENV` in
         `connector/mcp/mod.rs` (`NODE_OPTIONS`, `NODE_PATH`,
         `PYTHONPATH`, `PYTHONSTARTUP`, `PYTHONHOME`, `PYTHONUSERBASE`,
         `PYTHONINSPECT`, `RUBYOPT`, `RUBYLIB`, `GEM_HOME`, `GEM_PATH`,
         `PERL5OPT`, `PERL5LIB`, `PERLLIB`, `PERL5DB`, `BASH_ENV`, `ENV`,
         `ZDOTDIR`, `CLASSPATH`, `JAVA_TOOL_OPTIONS`, `_JAVA_OPTIONS`,
         `JDK_JAVA_OPTIONS`, `GCONV_PATH`, `LUA_PATH`) plus every name
         under `LOADER_ENV_PREFIXES` (`LD_`, `DYLD_`), checked by
         `is_loader_env`. Such a variable would change the code that gets
         the account's secrets without changing the pinned program digest
         (fix round 1, audit M2). The test
         `the_loader_list_is_refused_in_mcp_env_and_nothing_else_is`.
4. `validate_templates` (line 1293), the secret-placement policy: add the
   new locations to the allowed and refused sets. `{{secret.*}}` is
   allowed in stdio `env` values only; `args` takes `{{account.*}}` only,
   and `command` takes no placeholder at all (mcp-http-and-surfaces adds
   the rules of `url` and `mcp.auth`). `{{args.*}}` is refused in the
   whole `mcp` block (the arguments go to the tool, not to the
   transport). Messages follow the existing ones of that function.
5. `idempotency.rs`: `FunctionKind` (lines 215 to 223) gains `Mcp`;
   `native_transport` (line 227) returns `None` for it.
   `FunctionSpec::idempotency_view` (def.rs lines 510 to 534) sets
   `FunctionKind::Mcp` when `self.is_mcp()`, checked before the
   `Other` fallback (an inbox function falls to `Other` today and keeps
   doing so). `check_function` refuses `read_only` together with
   `idempotency` at line 355, before the kind match, so a read-only mcp
   function never reaches the `Mcp` arm. `check_function` (lines 348 to 387), in the kind
   match (line 360): `(FunctionKind::Mcp, Idempotency::Key { .. })` is
   refused with "mcp function `{name}` cannot declare idempotency mode
   `key`: the protocol has no idempotency key; use `marker` or `none`";
   `(FunctionKind::Mcp, _)` passes. In `check_marker` (lines 440 to 519)
   the `body_uses` check (line 453) is skipped for `FunctionKind::Mcp`
   (the arguments are the tool's arguments; the marker arg only has to be
   a string property, which line 448 already checks), and the reconcile
   function kind check (line 474) accepts `FunctionKind::Mcp` when the
   send is `FunctionKind::Mcp` (a read-only mcp function of the same
   connector) and keeps `Http` otherwise. The identity check (line 320)
   accepts a read-only `Mcp` function too; its message becomes "identity
   function `{}` must be a read_only HTTP, mock or mcp function".

### Validator (playbook)

`ConnectorFacts` (`crates/apb-core/src/connector/resolve.rs` lines 415 to
459) gains (eval-doubles adds `reconcile_functions` and
`identity_function` to the same struct; inbox-waits adds
`names_event_type`, `inbox_ops` and `inbox_args_schemas`: put this field
after whichever of them merged, and fill it in `of_manifest`):

```rust
    /// The transport name (`Some("stdio")` in this change) for a connector
    /// with an `mcp:` block; `None` otherwise.
    pub mcp_transport: Option<String>,
    /// `mcp::launcher_name` of the stdio `command`, for V301.
    pub mcp_launcher: Option<String>,
```

filled in `of_manifest` (lines 465 to 488, `Self {` at line 473) from
`doc.mcp`, and the
`untrusted_output_functions` line (485) uses
`names(&|f| f.is_untrusted_output())`, so V187 sees MCP functions.

New module `crates/apb-core/src/validate/mcp.rs`, declared `mod mcp;` in
`validate/mod.rs` between `mod loop_items;` (line 29) and `mod node_io;`
(line 30), called in `validate()` right after the V187 line (line 421):

```rust
mcp::check_mcp_bindings(playbook, ctx, &mut r); // V300 to V302
```

```rust
/// V300 (warning): a binding of an MCP connector (its `ConnectorFacts`
/// carry `mcp_transport`) with no `functions` list grants every tool the
/// manifest maps, destructive ones included. Silent when `ctx.connectors`
/// is empty or does not know the connector.
pub(super) fn check_mcp_bindings(playbook: &Playbook, ctx: &ValidationContext, r: &mut ValidationReport);
```

It walks `playbook.connector_sites()` (`crates/apb-core/src/connector/sites.rs`
line 44) like `check_connectors` (`validate/connectors.rs` line 41, the
loop at line 46), for every binding whose facts carry `mcp_transport`:

1. V302 (error) first, when the site is an engine site: `is_engine_site`
   (`sites.rs` line 30: `@baseline`, `@follow_up`) or a list item check
   (`crate::list::check_site_node`, used at `sites.rs` line 130):
   `{site label} reads MCP connector `{name}`; an MCP server is started
   only for a node's own call in this version (the baseline check, the
   follow-up check and list item checks run from the drive or a waker)`,
   the label through `site_label` (`sites.rs` line 123). The notify
   target needs no rule here: `carries_notices` answers false for an mcp
   function (above), so `apb notify` refuses it. Lifting V302 needs the
   environment, working directory and stop path of a server started from
   the drive and the waker; it is future work.
2. V300 (warning), for a binding whose `functions` is absent: `node
   `{site}` grants every function of MCP connector `{name}`; name the
   functions it needs (`functions: [..]` or `functions: read_only`)`.
   `functions: read_only` counts as a list.
3. V301 (warning), when the facts carry `mcp_launcher`: `node `{site}`
   binds MCP connector `{name}`, whose command `{launcher}` is an
   interpreter or a launcher: its approval covers the `{launcher}`
   program file only, not the script or package it runs; point the
   command at the installed server program instead`. Once per connector
   and site. `MCP_LAUNCHERS` (30 names) lists the launchers that fetch
   or pick a package at each start (`npx`, `pnpx`, `bunx`, `uvx`,
   `pipx`, `uv`, `npm`, `pnpm`, `yarn`, `docker`, `podman`, `env`) and
   the interpreters and runtimes that run the script their arguments
   name (`node`, `nodejs`, `deno`, `bun`, `python`, `python3`, `sh`,
   `bash`, `zsh`, `dash`, `ksh`, `fish`, `ruby`, `perl`, `php`, `java`,
   `lua`, `pwsh`); `launcher_name` also answers a versioned interpreter
   (`python3.12` answers `python3`).

V113 (`validate/isolation.rs` lines 38 to 47): `check_isolation(playbook,
r)` (line 15) gets `ctx: &ValidationContext` as a new second parameter
(its one caller, `mod.rs` line 481, passes `ctx`). When any bound connector's facts carry
`mcp_transport`, the message gains, after the existing text: `; an MCP
connector also starts a server program or reaches a remote server with
those credentials, which the sandbox cannot broker in this version`.
Code and severity unchanged.

### The agent never calls the server

The function list the agent sees is rendered from the manifest
(`crates/apb-engine/src/connector/prompt.rs` lines 146 to 194):
`description`, `args_schema` and `examples` are the author's, never the
server's. The server's description, schema and annotations appear only to
a person (`apb connector approve` here; `apb connector tools` and the
dashboard in mcp-http-and-surfaces).
The untrusted paragraph (prompt.rs line 89) uses
`f.is_untrusted_output()`, so it is said once for any granted MCP
function. Nothing is added to any agent MCP configuration. What that
proves depends on the profile's `environment`: in the minimal one (the
default, `AgentEnvironment::Minimal`, `crates/apb-core/src/profile.rs`
lines 399 to 403) claude is started with `HERMETIC_FLAGS`
(`crates/apb-core/src/agents/claude/settings.rs` line 61, appended only
by `inject_hermetic_settings`, `crates/apb-engine/src/adapter.rs` line
975), whose `--strict-mcp-config` keeps every MCP server but apb's
ask-server out. Under `environment: full` claude loads the person's own
MCP configuration, as on main; apb adds no server there either, but it
cannot say which servers the person configured. The test and the claim
are scoped to the minimal environment.

### Wrong implementations (D1)

1. Writing the MCP server into the agent's MCP configuration (a
   `--mcp-config` entry, a codex `[mcp_servers.x]` table) so the agent
   calls it directly. Caught by
   `connector_mcp.rs::an_mcp_grant_hands_the_agent_no_mcp_server` (a
   profile in the minimal environment: the stub agent's argv carries
   `--strict-mcp-config` and no config names the connector; the test
   says in its doc line that `environment: full` is out of its reach) and
   `tool_exposure_test.rs::an_mcp_grant_adds_no_expected_server`.
2. Deriving `read_only` from `annotations.readOnlyHint`, or `irreversible`
   from `destructiveHint`. Caught by
   `connector_mcp.rs::annotations_never_change_what_a_call_may_do` (a tool
   annotated read-only behind a function without `read_only` still goes
   through the send ledger; a read-only function whose tool says
   `destructiveHint: true` still needs no idempotency).
3. Rendering the server's tool description into the agent prompt. Caught
   by `prompt.rs` test `an_mcp_function_shows_the_manifest_description_only`.
4. Allowing `{{secret.*}}` in `args` (it would land in the process list).
   Caught by `mcp::tests::a_secret_in_args_is_refused`.
5. Letting `idempotency_view` fall through to `FunctionKind::Other` for an
   mcp function, so `check_function` refuses every declaration. Caught by
   `mcp::tests::marker_and_none_are_accepted_on_mcp`.
6. Reading `f.untrusted_output` directly in `resolve.rs` or `prompt.rs`.
   Caught by `mcp::tests::mcp_functions_count_as_untrusted_output` and the
   V187 test `v187_warns_for_an_unmarked_mcp_grant`.
7. Letting an MCP function run at an engine site (a baseline read, a
   list item check, the follow-up check), where a server would start from
   the drive or a waker with an environment nobody defined. Caught by
   `validate::mcp::tests::v302_refuses_an_mcp_baseline_read`,
   `v302_refuses_an_mcp_item_check` and `v302_refuses_an_mcp_follow_up`.
8. Accepting a timeout under `protocol: auto` that the fixed 10 s probe
   always outlasts. Caught by
   `mcp::tests::a_timeout_below_the_probe_is_refused_under_auto` and
   `a_short_timeout_is_accepted_under_modern`.
9. Approving an interpreter or a launcher silently. Caught by
   `validate::mcp::tests::v301_warns_for_a_launcher_command` and
   `v301_is_silent_for_a_server_program`.

## D2. Server trust and the pinned tool list

### What is trusted, and by what

1. The connector tree (`connector.yaml`, the stdio `command` and `args`
   templates included) is approved by exact digest, as today
   (`apb connector approve <c>`, `crates/apb-cli/src/connector.rs`
   `approve_cmd`, lines 345 to 406). `acknowledge_untrusted` never bypasses it
   (`crates/apb-engine/src/gate/mod.rs` lines 1290 to 1323, the acknowledge ignored at line 1296). A changed
   `command`, `args` or `env` changes the digest and drops the approval,
   and it also drops every pin made under the old tree: a pin record names
   the tree digest it was approved under (point 3), so approving the
   changed tree again never makes an old pin usable. For a connector with
   an `mcp:` block this approval is a person's act like the pin's (D2
   "Capturing pins").
2. The account (non-secret fields, `{{cmd:...}}` lines included) is
   approved by digest, as today (`approve_cmd`, the account branch).
3. New: the program. The tree digest covers the `command` string only,
   not the file a bare name finds or what lives at an absolute path. So
   the approval at a person's terminal resolves `command` once to an
   absolute canonical path and records it with the sha256 of the file's
   bytes and the digest of the approved connector tree in the connector
   and account's pin record (one binding: the tree digest, the program
   path, the program digest and the tool digests). A call starts exactly
   that file, the one it hashed (below), never a name looked up in the
   caller's `PATH`, and refuses
   (`mcp_server_changed`) when the tree digest is not the pinned one (the
   tree covers `mcp.command`, `mcp.args`, `mcp.env` and every other
   field of the definition) or the file's digest is not the pinned one. This is the
   "a stdio server is code" rule: the code that receives the account's
   secrets is fixed at approval, not by the environment of whoever calls.
4. New: the server's environment. It is built from a fixed source (D4
   "The server's environment"), never from the calling process, so a
   caller's `HOME`, `XDG_*`, proxy or certificate variable cannot point
   the pinned program at another configuration or intercept its traffic.
5. New: the tools. For every function of an MCP connector, the definition
   of the tool it maps, as the server lists it to that account, is pinned
   by digest. A call compares the live definition with the pin on every
   call.

Tool annotations are part of what is pinned and shown, never of what
decides (tools page, "Tool": annotations are untrusted).

Interpreters and launchers. When `command` is a launcher (`npx`, `uvx`,
`docker` and the others of `MCP_LAUNCHERS`, D1) or an interpreter
(`node`, `python3`, `sh` and the others), point 3 pins that program's
own file only: the script it runs, the code it fetches or selects at
each start (a package, an image tag) and the modules it loads are not
covered, and can change with no change to anything pinned. The tool
pins of point 5 still refuse a call whose tools changed, but a server
that changes its behaviour and not its tool list is not caught. The
approve output says so in one line, V301 warns at every binding, and
`docs/CONNECTORS.md` "MCP servers" says it plainly, with the advice to
install the server and name its program. The definition cannot set a
variable that changes what code a process loads (`LD_PRELOAD`,
`NODE_OPTIONS`, `PYTHONPATH` and the rest of `LOADER_ENV`, D1 rule
3.4.6), and the fixed environment of D4 passes none from the caller.

The program that runs is the file that was hashed. The program file is
opened once (`apb_mcp_client::ProgramFile::open`: no link followed at
its last component, `O_NONBLOCK` so a FIFO never blocks it, a regular
file only), its digest is read from that descriptor, and the descriptor
is kept until the start. On Linux the start executes
`/proc/self/fd/<n>` of that descriptor (`StartFrom::Descriptor`), so a
rename over the path, a new file or a link swap after the hash changes
nothing: the hashed file runs. On other platforms (macOS,
`StartFrom::PathCheckedAgain`) the path is opened again without
following a link and hashed again immediately before the spawn, and a
different digest refuses the start (`mcp_server_changed`, nothing
starts); the window between that second hash and the exec remains
there. On no platform does it cover a rewrite in place of the same file
(the same inode) between the hash and the exec; only a replacement is
covered. The libraries the program loads are not digested, and neither
is a script's interpreter named by its first line (on Linux the kernel
starts that interpreter with `/proc/self/fd/<n>` as the script's name,
so the interpreter reads the hashed file). These are the same-user
boundary the pin store states below.

### The tool digest

In `crates/apb-core/src/connector/mcp/digest.rs` (`pub mod digest;` in
`connector/mcp/mod.rs`):

```rust
/// The domain tag of a tool digest; bump the trailing number to change
/// what is hashed.
pub const TOOL_DIGEST_DOMAIN: &str = "apb-mcp-tool/1";

/// The members of a tool definition the digest leaves out, each with its
/// reason. Everything else the server sends is pinned.
pub const UNPINNED_MEMBERS: [&str; 2] = [
    // Display data; a server may rotate an icon URL per request.
    "icons",
    // Free metadata; the specification lets it change per request.
    "_meta",
];

/// `sha256:<hex>` of `TOOL_DIGEST_DOMAIN`, a newline and the canonical JSON
/// of one tool definition with the top-level members of
/// `UNPINNED_MEMBERS` removed. `tool` is the raw JSON object exactly as
/// the server's `tools/list` page carried it (D4 item 4: read off the
/// wire, never a typed struct converted back to JSON). The whole object is
/// taken, never a list of the members to keep, so a member a later
/// protocol revision adds (an execution hint, a new annotation) is pinned
/// from the first day. A tool that is not a JSON object is digested as is.
pub fn tool_digest(tool: &serde_json::Value) -> String;

/// JSON text with every object's keys sorted (byte order), no
/// whitespace, numbers as serde_json prints them. Recursive. Never relies
/// on the map type's own order (a dependency could turn on serde_json's
/// `preserve_order` feature for the whole build).
pub fn canonical_json(value: &serde_json::Value) -> String;

/// The domain tag of a pin digest.
pub const PIN_DIGEST_DOMAIN: &str = "apb-mcp-pin/1";

/// `sha256:<hex>` of `PIN_DIGEST_DOMAIN`, a newline and the canonical JSON
/// of `{"connector_digest": .., "account_digest": .., "program": ..,
/// "program_digest": .., "tools": {<tool>: <tool digest>, ..}}`: the whole
/// binding one server pin record approves. The approve output prints it
/// whole at the terminal, for reference; no argument takes it.
pub fn pin_digest(connector_digest: &str, account_digest: &str, program: &str, program_digest: &str, tools: &BTreeMap<String, String>) -> String;
```

Why the raw object: rmcp's `Tool` (3.9.0 `src/model/tool.rs` lines 17 to
40) has fixed fields and no catch-all member, and `ToolAnnotations` keeps
only the hints it names, so a server could add or change a member such as
`execution` and a digest of `serde_json::to_value(&tool)` would not move.
The digest is computed from the bytes the server sent, decoded to
`serde_json::Value` (which keeps every member) and canonicalized.

Use `crate::content::sha256_hex` (`crates/apb-core/src/content.rs` line
419) for the hash. The same rule (take the structure whole, remove named
members with a reason) governs the approve output: it prints the
definition's `title`, `description`, `annotations` and `inputSchema` for
reading, but the digest the person approves covers everything but
`UNPINNED_MEMBERS`, so a change in a member the output does not print
still refuses the next call until a person approves again.

### The pin store

`crates/apb-core/src/connector/mcp/pins.rs`:

```rust
/// `<config-dir>/mcp-tools.json`: one pin record per connector account,
/// holding the program and every mapped tool the person approved.
/// Machine-wide, like `trust.json`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PinStore {
    #[serde(default = "schema_one")]
    pub schema_version: u32,
    /// Key `<connector>/<account>` (`server_key`).
    #[serde(default)]
    pub servers: BTreeMap<String, ServerPin>,
}

/// One approval: the binding of the connector tree, the program that
/// receives the account's secrets and the tools it may be asked to run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServerPin {
    /// The tree digest of the connector the person approved
    /// (`LoadedConnector.digest`, `content::tree_digest`): it covers
    /// `mcp.command`, `mcp.args`, `mcp.env`, `mcp.protocol` and every
    /// other field of the definition.
    pub connector_digest: String,
    /// The digest of the account the person approved (`config::account_digest`
    /// of the merged account, the value `ManifestAccount.digest` snapshots
    /// in a run): a project account that replaces a global one of the same
    /// name (`connector/config.rs` lines 77 to 80) has another digest, so
    /// a pin made for one project's `work` never serves another project's.
    pub account_digest: String,
    /// The definition's `command` as written when approved. Display only:
    /// `connector_digest` covers it, and nothing compares it on its own.
    pub command: String,
    /// The absolute canonical path the approval resolved it to.
    pub program: String,
    /// `sha256:<hex>` of the program file's bytes at approval.
    pub program_digest: String,
    /// Tool name -> its pin. A name is the server's raw name: a listing
    /// whose tool names hold a secret is refused, so no name is redacted.
    pub tools: BTreeMap<String, ToolPin>,
    /// `digest::pin_digest` of `connector_digest`, `program`,
    /// `program_digest` and the tool digests.
    pub pin_digest: String,
    pub approved_at_ms: u128,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolPin {
    /// `tool_digest` of the approved definition, over the raw JSON.
    pub digest: String,
    /// The approved description after the account's redactions, at most
    /// 300 characters, for display only.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub description: String,
    /// Each top-level member of the approved definition (minus
    /// `UNPINNED_MEMBERS`), its name after the account's redactions ->
    /// `sha256:<hex>` of its raw canonical JSON, so a changed tool can name
    /// the members that differ. Display only: the call decides on
    /// `digest` alone.
    #[serde(default)]
    pub members: BTreeMap<String, String>,
}

pub fn server_key(connector: &str, account: &str) -> String; // "c/a"

/// What a checker knows of the program file now.
pub enum ProgramFact<'a> {
    /// Not read: the run gate hashes no file.
    NotRead,
    /// Missing, a link, or not a regular file.
    Gone,
    /// `sha256:<hex>` of the file's bytes, read now from the descriptor
    /// the start will use (`ProgramFile`).
    Digest(&'a str),
}

/// The live facts one check holds, compared with a record as one binding.
pub struct LiveBinding<'a> {
    /// The tree digest the checker runs under: the run's
    /// `ManifestConnector.digest` for a call in a run, the installed
    /// connector's digest for the gate and the healthcheck.
    pub connector_digest: &'a str,
    /// The account digest the checker runs under: the run's
    /// `ManifestAccount.digest` for a call in a run, `config::account_digest`
    /// of the merged account for the gate and the healthcheck.
    pub account_digest: &'a str,
    pub program: ProgramFact<'a>,
    /// The tool the function maps.
    pub tool: &'a str,
    /// `None` when no server was started (the gate): presence only.
    pub tool_digest: Option<&'a str>,
}

/// The first difference between a record and the live facts.
pub enum BindingDiff {
    Tree { pinned: String },
    Account { pinned: String },
    ProgramGone,
    Program { pinned: String },
    ToolUnpinned,
    Tool { pinned: String },
}

impl PinStore {
    /// The store of this process's config dir. The config dir itself is
    /// resolved once with `std::fs::canonicalize` (a config dir that is a
    /// link, as a dotfile manager makes, is followed at the root only);
    /// the file is then read through
    /// `crate::fsutil::beneath::Beneath::open(<canonical dir>)` and
    /// `read_regular_capped("mcp-tools.json", PIN_STORE_CAP)`, 4 MiB (no link followed
    /// below the root, a regular file with one link only). A missing file
    /// or config dir is an empty store. A refused read, a file over the cap
    /// or one that does not parse is an error, never an empty store: an
    /// empty store would only refuse calls, but a silent reset would hide
    /// that someone edited it.
    pub fn load() -> Result<PinStore, PinError>;
    pub fn server(&self, connector: &str, account: &str) -> Option<&ServerPin>;
    /// Replaces the whole record of `connector`/`account` under the
    /// store's lock, re-reading the file first (through `load`, so a store
    /// that does not read stops the write), atomically
    /// (`crate::fsutil::atomic_write_private`), then
    /// `crate::fsutil::sync_dir` of the config dir. Before the write the
    /// whole new store is serialized and refused when it is longer than
    /// the read cap of `load` (`PIN_STORE_CAP`, 4 MiB):
    /// `PinError::TooLarge`, the file left as it was, so every record that
    /// succeeds loads again.
    pub fn record(connector: &str, account: &str, pin: ServerPin) -> Result<(), PinError>;
    /// Removes every record of `connector` (and `account` when given):
    /// called by `install::uninstall` (every record of the connector) and
    /// by `TrustStore::revoke` (the records of a revoked connector, or of
    /// a revoked connector account). Writes nothing when no record
    /// matches. Best effort at both callers: a store that does not read
    /// keeps its records, and a kept record still binds the tree digest.
    pub fn forget(connector: &str, account: Option<&str>) -> Result<(), PinError>;
}

/// `new` without a record, `same` when the record pins this path and
/// digest, else `changed`. Display only (the listing's program status and
/// the CLI); a call decides through `ServerPin::compare`.
pub fn program_status(record: Option<&ServerPin>, program: &str, digest: &str) -> &'static str;

impl ServerPin {
    /// The one comparison of a pin binding, used by the call, the
    /// healthcheck, the run gate and the listing's row statuses: the tree
    /// digest first, then the account digest, then the program file (skipped for `NotRead`), then
    /// the tool (its presence, and its digest when one is given). The first
    /// difference answers; nothing else compares a pin.
    pub fn compare(&self, live: &LiveBinding<'_>) -> Result<(), BindingDiff>;
}

#[derive(Debug, thiserror::Error)]
pub enum PinError {
    #[error("the MCP tool pin store {0} does not parse: {1}; move it aside and approve the tools again")]
    Unreadable(String, String),
    #[error("mcp_pin_store_too_large: the MCP tool pin store would be {0} bytes, over its cap of {1}; nothing was pinned")]
    TooLarge(usize, usize),
    #[error("the MCP tool pin store could not be written: {0}")]
    Write(String),
}
```

A record is replaced whole, never merged per tool: the person confirmed
one pin digest, and the store holds exactly what it covers. A tool the
server no longer lists is not pinned (its function is refused
`mcp_tool_missing` until the server lists it and a person approves
again).

The lock: `crate::fsutil::lock_dir(&dir, "mcp-tools.json.lock")`
(`crates/apb-core/src/fsutil.rs` line 901), then re-read, change,
`atomic_write_private` (line 742) and `sync_dir`: the pattern of
`TrustStore::locked_mutate` (`trust.rs` lines 389 to 400, `persist` to
line 409) with two differences, both on purpose: a lock that cannot be
taken is an error (`locked_mutate` ignores it with `.ok()`), and the read
follows no link below the canonical config dir (`TrustStore::load` is a
plain `read_to_string`). Mode 0600 like every state file. Each
`atomic_write_private` passes `process_mode::guard_write`, so a read-only
or scenario process never writes a pin.

Which config dir a call reads. `config_dir()` honours `APB_CONFIG_DIR`
(`crates/apb-core/src/config.rs` lines 488 to 500), and `base_set()`
hands that variable to every agent (`process_env/sets.rs` line 56), so a
run step could point the pin check at a store it wrote. The rule of
`gate::launch::effective_for` (`crates/apb-engine/src/gate/launch.rs`
lines 555 to 605: a run step, or a CLI without a terminal, whose config
dir is redirected away from `default_config_dir` is held to the default
one, `Initiator::checks_redirect` at line 144) is reused for pins with no
exception: a call made from a run step (`StepCaller::from_env()` names a
run or a host task, `crates/apb-core/src/step_env.rs` lines 16 to 32)
while `config::config_dir_redirected()` (`config.rs` line 803) is true is
refused before anything starts: `CallError::new(Permission,
"mcp_config_dir_redirected: the config dir is redirected to `{here}`
away from `{default}`; a run step reads MCP pins only from the default
config dir")`. For launch settings `effective_for` refuses only when the
redirect hides something stricter; for pins any redirect can admit a
forged store, so it always refuses. `effective_for`'s one exception, an eval case under the config dir its `apb eval` handed over, is not known
at call time and is not taken: an eval case that calls an mcp function
doubles it (D5 "Eval doubles"), or it is refused, even with the pins in
its own config dir (`connector_mcp.rs::an_eval_case_with_its_own_config_dir_calls_no_server_live`). A person's own call
(no run step) reads the config dir it was given, as every other command
does.

What the pin store guards against, and what not. It is not a signed
store: it lives in the config dir like `trust.json`, not under a
project's `.apb/`, and a MAC keyed by `<config-dir>/run-origin.key` would
sit next to its own key. It guards against (1) a server or a program
file that changes after a person approved it, (2) a caller choosing the
program or its environment, and (3) an agent or a mistake reaching pins
through apb's own surfaces: no MCP tool and no HTTP route writes a pin,
`apb connector approve` writes one only after a person confirmed at a
terminal, in two typed confirmations, and refuses a run step and any
caller without an interactive terminal. It does
not guard against another process of the same user editing the file
directly, exactly as `trust.json` does not: it is an unsigned file a
same-user process can write, so "approved at a terminal" is no
protection against a run step of the same user with a shell; isolation
(`docs/ISOLATION.md`) is the answer to an agent with a shell. The
terminal check itself is `isatty` plus `/dev/tty`: an agent with a
pseudo-terminal that clears `APB_RUN_ID` passes it, as it passes every
typed confirmation of apb. Both limits are stated in
`docs/CONNECTORS.md` "MCP servers" and accepted (fix round 1, audit m1
and m2).

Why not `trust.json`: a new `Kind::McpTool` variant would make an older apb
fail to parse the whole trust store, and `TrustStore::load` answers a
parse failure with an empty store (`trust.rs` lines 290 to 306): every
playbook, connector and account would read as unapproved after a
downgrade. A separate file costs an older apb nothing.

Why keyed by names, with the tree digest inside the record: a record is
found by `<connector>/<account>`, and its binding names the tree it was
approved under, so a changed definition (its `command`, `args`, `env`
or any other field) refuses every call and every start
(`mcp_server_changed`, `mcp_tools_unpinned`) until a person pins the
account again under the new tree, whatever approved the tree itself.
Approving the changed tree alone (D2 "Capturing pins", the connector
path) never makes an old pin usable. The account digest is part of the
binding too: the key `<connector>/<account>` is machine-wide while a
project's account file may replace a global account of the same name
with other fields (`connector/config.rs` lines 77 to 80), so a record
made for one project's account refuses (`mcp_server_changed`,
`mcp_tools_unpinned`) under another project's account of that name
until a person pins it there; the last approval of a name replaces the
record whole. Re-approving an unchanged account keeps the pins.

Forgetting pins. A pin is part of a person's approval, so it goes when
the approval or the connector goes: `install::uninstall` calls
`PinStore::forget(name, None)` after the connector's directory is
removed (a reinstall asks a person to approve its tools again at a
terminal; the trust record stays, as before), and `TrustStore::revoke`
(behind `apb trust revoke`, MCP `trust_revoke` and the dashboard) calls
it for every removed entry of kind `Connector` (all its records) or
`ConnectorAccount` (that account's record). Both are best effort: a
store that does not read keeps them, a kept pin still binds the tree
digest, and a call still needs the trust the revoke removed. Accounts
have no removal command (a person edits the account file); a pin of a
removed account binds an account digest nothing matches, and the next
approval of an account of that name replaces it. Tests
`pins::tests::a_revoked_approval_takes_its_pins_with_it`,
`forgetting_nothing_writes_nothing` and
`install::tests::uninstall_forgets_the_connectors_mcp_pins`.

### Capturing pins (a person's act)

`crates/apb-engine/src/connector/mcp/pin.rs`:

```rust
/// One mapped tool as the server lists it now, against its pin.
#[derive(Debug, Clone, Serialize)]
pub struct ToolRow {
    pub function: String,
    pub tool: String,
    /// `None` when the server does not list the tool.
    pub digest: Option<String>,
    /// `pinned`, `unpinned`, `changed` or `missing`.
    pub status: &'static str,
    pub title: Option<String>,
    pub description: Option<String>,
    /// For a `changed` row: the pinned description (display only).
    pub pinned_description: Option<String>,
    /// For a `changed` row: the top-level members whose digest differs
    /// from the pinned one, or that one side lacks (`ToolPin.members`).
    pub members: Vec<String>,
    /// The server's annotations, shown and never used.
    pub annotations: Option<serde_json::Value>,
    pub input_schema: Option<serde_json::Value>,
}

/// The program a listing started.
#[derive(Debug, Clone, Serialize)]
pub struct ProgramFacts {
    pub command: String,
    /// Absolute canonical path.
    pub program: String,
    pub program_digest: String,
    /// `MCP_LAUNCHERS` name when it is one.
    pub launcher: Option<String>,
    /// `same`, `new` (no record yet) or `changed` against the record.
    pub status: &'static str,
}

/// What a live listing found: one row per function of the connector that
/// maps a tool, then the server's tools no function maps (`unmapped`, by
/// name only, never callable), and the pin digest the rows would record.
#[derive(Debug, Clone, Serialize)]
pub struct ToolListing {
    pub connector: String,
    pub account: String,
    /// The approved tree digest the listing ran under (`LiveTarget`'s
    /// digest); the record binds it.
    pub connector_digest: String,
    /// `config::account_digest` of the merged account; the record binds it.
    pub account_digest: String,
    /// `modern <version>` or `legacy <version>`, the version one of
    /// `apb_mcp_client::SUPPORTED_VERSIONS`, never a string the server sent.
    pub era: String,
    /// Set when the server started for this listing could not be shown
    /// ended (`Ended::Unconfirmed`, or `Reaped` with `NotShownEmpty`):
    /// plain words, shown before the second confirmation (fix round 2,
    /// review Minor 4).
    pub cleanup: Option<String>,
    pub program: ProgramFacts,
    pub rows: Vec<ToolRow>,
    pub unmapped: Vec<String>,
    /// `digest::pin_digest` over the tree digest, the account digest, the
    /// program and every listed mapped tool.
    pub pin_digest: String,
    /// The pins `record` writes, prepared from the raw tool objects
    /// before the person's second confirmation: tool name -> `ToolPin`
    /// with its raw `digest`, its redacted description and its member
    /// digests (each top-level member minus `UNPINNED_MEMBERS`, over the
    /// raw canonical JSON). Private (not serialized, not printed): the
    /// raw objects are dropped once this map and the rows are built, and
    /// `record` writes exactly this map, never anything recomputed.
    #[serde(skip)]
    pins: BTreeMap<String, ToolPin>,
}

/// Resolves `command` for an approval: an absolute `command` as is, a
/// bare name through `path` (the approving person's `PATH`, the first
/// entry whose file exists and is executable,
/// `apb_core::content::is_executable`, `content.rs` line 328), then
/// `std::fs::canonicalize` and the digest through `mcp::program_file`
/// (the one open of a program file, D4).
/// Not found: `CallError::new(Config, "mcp command `{cmd}` was not found")`.
pub fn resolve_program(command: &str, path: &std::ffi::OsStr) -> Result<(String, String), CallError>;

/// What the first confirmation of an approval showed and the person
/// confirmed (D2 "Capturing pins" item 4).
pub struct ConfirmedProgram {
    pub connector_digest: String,
    pub account_digest: String,
    pub program: String,
    pub program_digest: String,
}

/// Reloads the installed connector and the merged account and compares
/// their digests with `confirmed` before anything else: a tree digest
/// that differs is `mcp_approve_definition_changed: the definition of
/// `{c}` changed after it was confirmed; nothing was started`, an
/// account digest that differs is `mcp_approve_account_changed: account
/// `{a}` of `{c}` changed after it was confirmed; nothing was started`
/// (both `Permission`; nothing is resolved, nothing starts). Then the
/// program file is opened once through `mcp::program_file` and its digest
/// compared with `confirmed` (`mcp_server_changed` on a difference); that
/// same `ProgramFile` is what starts. Only then connects to the server
/// with the confirmed program, the account's fields and secrets (`call::auth::resolve_secrets`, `auth.rs`
/// line 35, `pub(crate)`, which also answers the account's redactions),
/// lists every tool and builds the rows against the pin store
/// (`ServerPin::compare`, display only). Writes nothing. Called only
/// after the person confirmed the program (below, item 4), so
/// no secret is resolved and nothing starts for a program nobody
/// confirmed. A listing whose raw tool JSON holds any resolved secret
/// value of the account, in any member name or value, is refused before
/// anything is hashed, shown or stored ("Reflected secrets"); every text
/// the listing keeps also passes `mcp::redact_tree`, a second line for
/// text that reaches it from elsewhere. The connector's tree digest must be
/// approved (`TrustStore::is_approved` of the connector digest, the first
/// half of `LiveTarget::require_trust`, `call/target.rs` lines 183 to
/// 195); the account's own approval is not required, since this listing
/// is the step of the person's approval that shows what will be approved.
/// The target is `LiveTarget::load(root, connector, account, first)`
/// (`call/target.rs` line 133), where `first` is the first function of
/// the manifest, in manifest order, that `is_mcp`: `load` uses the
/// function name only to select a `FunctionSpec`, and the doc, the
/// account and the digest it returns do not depend on it. `call/mod.rs`
/// declares `mod target;` (line 31) privately; this change makes it
/// `pub(crate) mod target;` so `connector::mcp::pin` reaches `LiveTarget`
/// (`pub(crate)` already). No item of `target.rs` changes.
pub fn list_live(root: &Path, connector: &str, account: &str, confirmed: &ConfirmedProgram) -> Result<ToolListing, CallError>;



/// Writes the record of `listing` (`PinStore::record`): its tree and
/// account digests, the program, the prepared `pins` map (every tool that
/// is not `missing`), and `listing.pin_digest`.
/// Called only after the person's confirmation (below).
pub fn record(listing: &ToolListing) -> Result<(), CallError>;
```

`list_live` reaches the server through the same connect path a call
uses (D4 `Session::connect`, the environment of D4 "The server's
environment" built for `program`), bounded by the `MCP_PIN_CHECK_TIMEOUT` (30 s, `connector/mcp/pin.rs`), always: a
function's `timeout_sec` bounds calls and the healthcheck only.

Listing limits, the same for an approval, a call and the healthcheck
(`Session::list_tools` arguments, constants in `connector/mcp/pin.rs`):
at most `MCP_LIST_MAX_PAGES` = 20 pages, `MCP_LIST_MAX_TOOLS` = 500
tools, and `MCP_LIST_MAX_BYTES` = 2 MiB of raw `tools/list` result
bytes summed over the pages (counted by the line tap as it stores each
raw result). Past any of them the listing fails `TooLarge`
(`mcp_message_too_large: the tool list is over its limit of {what}`),
before anything is hashed or stored. The aggregate is below the pin
store's read cap (`PIN_STORE_CAP`, 4 MiB), and `PinStore::record`
still refuses a whole store over that cap before it replaces the file
(`mcp_pin_store_too_large`), so a store that several records filled
never becomes one `load` refuses.

Reflected secrets. `list_live` resolves the account's secrets before it
starts the server (after the program confirmation), so a server can echo
one back in a tool's name, description, annotation or a member name or
value anywhere in its definition. Hashing such a definition would store
a digest (the tool digest and the per-member digests) that a person who
knows the description's template could test offline against guesses of
a short secret (`Secret` has no minimum length,
`crates/apb-core/src/secret.rs` lines 38 to 45). So, right after the
raw pages are read and before anything is hashed, shown or stored:

1. Each raw tool object is walked by `mcp::redact_tree` (in
   `connector/mcp/mod.rs`, the one walk: the account's redactions over
   every string, every object member name and the JSON text of every
   number, at any depth; a number whose text holds a secret becomes the
   redacted text, a string; it answers the redacted value and whether
   anything was replaced). `reflects_secret` is whether that walk
   replaced anything; when it did, the whole listing is refused:
   `CallError::new(Permission, "mcp_tools_reflect_a_secret: the server of
   `{c}` lists for account `{a}` a tool definition that holds a secret of
   the account; nothing was hashed, approved or pinned")`. The text names
   no tool and no member. The call path and the healthcheck run the same
   walk over the tool they compare (the same refusal, before the digest).
2. `redact_tree` still runs over everything the listing keeps as
   text (`ToolRow.title`, `description`, `pinned_description`,
   `members`, `annotations`, `input_schema`, `ToolListing.unmapped`,
   `ToolPin.description`, `ToolPin.members`' names): a second line for
   text that reaches the output from elsewhere (a pinned description
   recorded by an older listing). A description is cut to 300
   characters after the redaction, never before.

The CLI: `apb connector approve <c> [--account <a>]`, with no new flag
(`ConnectorAction::Approve` in `crates/apb-cli/src/connector.rs` lines 67
to 72 is unchanged). `approve_cmd` loads the connector first (as today,
line 346); when `loaded.doc.mcp` is `None` nothing below applies and
main's behaviour is unchanged, with or without `--account` (main refuses
no run step there; that stays a pre-existing note, review record). When
it is `Some`, every approval path, the connector path without
`--account` (`approve_cmd` lines 353 to 366 on main, which records the
tree digest with no question) included, is a person's act at an
interactive terminal and nowhere else, and the account path takes two
typed confirmations in this order, the program first and the tools
second, so no secret is resolved and nothing is started for a program
nobody confirmed:

1. Inside a run step it is refused first, before anything is read or
   started: `StepCaller::from_env().refusal("approving an MCP connector")`
   (`crates/apb-core/src/step_env.rs` line 37), printed on stderr, exit
   2, on both paths.
2. Without a terminal (stdin and stderr, the test `consent::ask_typed`
   makes, `crates/apb-cli/src/consent.rs` line 136) it is refused before
   anything else is read, resolved or started, on both paths:
   `mcp_approve_needs_a_person: approving an MCP connector or its tools
   is a person's act at an interactive terminal (docs/CONNECTORS.md, MCP
   servers)` (exit 2). The refusal prints no digest, no program and no
   tool. There is no non-terminal form: a form that took an expected
   digest would have to refuse a wrong one without revealing the right
   one at every stage, and one that printed it would let the caller
   supply it on a retry, with no person in between (review record,
   sixth pass). Approval without a terminal, for example in provisioning
   scripts, is future work (proposal, out of scope 17).
3. Without `--account` (the tree): `print_tree` (D5 "The approve
   output") prints the tree digest and the `mcp:` block (transport,
   `command`, each `args` template, every `env` name with its template,
   `protocol`), then the typed confirmation of item 8. Then
   `approve_kind` of the tree digest as today. Nothing is started, no
   program is resolved, no secret is read and no pin record is touched: a
   record bound to another tree stays refused by every call and start
   until the account path below pins it again under this tree (D2 "The
   pin store"). The lines `approved connector <c> (digest <d>)` and `the
   MCP tools of its accounts need a new approval: apb connector approve
   <c> --account <a>` (one per account whose record names another tree)
   end it; this output is the person's, never a refusal text.
4. With `--account`, the first confirmation, the program and what it
   will be started with: `resolve_program(command, <this process's
   PATH>)` (the person's own terminal is the fixed source at approval;
   it reads and hashes the file and nothing else), the account digest of
   the merged account, and `preview_start` (below), which renders the
   `args` and the `env` overrides with the account's non-secret fields
   and no secret. `print_program` (D5) prints the command, the absolute
   path, the file digest, the interpreter or launcher line when it is
   one, the line that says the program runs as the person, unsandboxed
   (D5), every
   rendered `arg`, every declared `env` override with its rendered value
   (a value or a part of one that comes from a secret shown as
   `[secret <field> from <source>]`, the source being `env <VAR>` or
   `cmd`, never its value and never running a helper), and every
   non-secret field of the account, and, for each secret field of the
   account whose value comes from a command, the command line that
   resolving it will run (`config::cmd_refs`, `crates/apb-core/src/connector/config.rs`
   line 259, which extracts the reference text and never executes it);
   every string sanitized for the terminal. So an account field that sets a proxy, a `PATH` or a
   certificate file is seen before anything starts. Then the typed
   confirmation of item 8 (`Start <path> with these arguments and this
   environment, and the secrets of account `<a>`? Type the connector name
   to confirm:`). A declined confirmation ends the command here: no
   secret is resolved, no program starts, nothing is written.
5. Only then `list_live(root, c, a, &ConfirmedProgram { .. })` (which
   needs the tree approved, item 3 first), handed the tree digest of the
   connector item 4 loaded, the account digest, the program path and the
   file digest exactly as the person confirmed them. It reloads the
   connector and the merged account and compares both digests with the
   confirmed ones before it resolves any secret
   (`mcp_approve_definition_changed`, `mcp_approve_account_changed`), so
   an account edit made while the prompt waited (another `{{cmd:...}}`
   line, another secret reference, another field an `args` or `env`
   template reads) never decides which secret is resolved or where it
   goes; then it opens the program file once and compares its digest
   (`mcp_server_changed`), and starts that same descriptor.
   Any of these ends the command with nothing resolved, started or
   written. Only then are the account's secrets resolved and the
   confirmed program started and listed.
6. `print_pins` (D5 "The approve output") prints every row with its
   full digest, its title, description and input schema, the
   description of a `changed` row pinned and new side by side, the
   annotations, the unmapped names, and the pin digest for reference.
   Every server text passes `redact_tree` and `sanitize_for_terminal`
   (`crates/apb-cli/src/util.rs` line 85) and is bounded in length with a
   marker, each field to its own bound and all of them together to
   `SERVER_TEXT_TOTAL`; the program digest, the status counts and the
   pin digest are printed again right above the question (D5).
7. The second confirmation, the tools: the typed confirmation of item 8
   (`Pin these tools for `<c>` account `<a>`? Type the connector name to
   confirm:`).
8. A typed confirmation is `consent::ask_typed(&[], "<question>")` with
   the question of its item (`Approve connector `<c>` (digest <d>)? Type
   the connector name to confirm:` on the tree path); the typed text must
   equal the connector name (the pattern of `--waive-probation`,
   `crates/apb-cli/src/run.rs` lines 881 to 900). Anything else: "not
   confirmed: nothing was approved or pinned" and exit 2.
9. Then, on the account path, in this order: the account approval
   (`approve_kind`, lines 380 to 391, as today), then `pin::record` of
   the prepared map. A `missing` row prints a warning and does not fail
   the command. Exit 0 when both writes succeeded.

The steps after item 2 live in `crates/apb-cli/src/connector_mcp.rs` as
`approve_mcp(confirm: &mut dyn Confirm, ..)`, where `Confirm` is the
one call `ask_typed` makes; the binary passes the terminal, so its unit
tests drive both confirmations with a scripted `Confirm` and the binary
itself is only tested refusing (no test drives a terminal).

`preview_start` is in `connector/mcp/server.rs` next to `stdio_server`:

```rust
/// What the server would be started with, rendered from the definition
/// and the account's non-secret fields only: each `args` entry, and each
/// `env` entry with every `{{secret.<field>}}` replaced by
/// `[secret <field> from <source>]`. Resolves no secret and runs no
/// helper. Also carries `config::cmd_refs(doc, account)` (`config.rs`
/// line 259): each command-sourced secret field with the command line
/// its resolution will run, extracted without executing it. The rows
/// `print_program` shows; `stdio_server` renders the same templates with
/// the secrets after the confirmation.
pub(crate) fn preview_start(spec: &McpServerSpec, account: &config::Account) -> StartPreview;
```

No MCP tool approves a connector or writes a pin, and none is added. The
dashboard route `POST /api/connectors/approve`
(`crates/apb-server/src/routes/connectors/mod.rs` lines 519 to 560) keeps
main's behaviour for every other kind; for a connector whose
`loaded.doc.mcp` is `Some` it refuses both the tree and the account
before it writes anything: HTTP 403 with `mcp_approve_needs_a_person:
approving an MCP connector or its tools is a person's act at a terminal
(docs/CONNECTORS.md, MCP servers)`. It never writes a pin record.

### Checking pins (every call)

In the call path (D4 "The engine side"), in this order. Item 1 is
`mcp::check_pin` and runs before `resolve_secrets`, so a refusal there
starts nothing at all, a `{{cmd:...}}` secret helper of the account
(`call/auth.rs` lines 55 to 70, `secrets::resolve_cmd`) included; item 2
is `mcp::connect`, after the secrets are resolved. The healthcheck takes
the same two steps (D4 "Live calls outside a run").

1. Before anything starts: the config dir rule above
   (`mcp_config_dir_redirected`), then `PinStore::load` (`Config` with the
   `PinError` text on failure), then the server record:
   1. No record for `(req.connector, account)`: `CallError::new(Permission,
      "mcp_server_unpinned: the MCP server of `{c}` is not approved for
      account `{a}`; a person approves it at a terminal (docs/CONNECTORS.md,
      MCP servers)")`.
   2. `record.compare(&LiveBinding { connector_digest, account_digest,
      program, tool, tool_digest: None })` with the account digest the
      call runs under (in a run the granted account's
      `ManifestAccount.digest`, `manifest.rs` lines 239 to 258; the
      healthcheck uses `config::account_digest` of the merged account),
      the tree digest the call runs under (in
      a run the run's `ManifestConnector.digest`, `manifest.rs` line 264,
      read through `RunExecutionManifest::connector`, line 740: the tree
      the run snapshotted and the gate approved; the healthcheck uses the
      installed one) and the program file at `record.program`, opened
      once through `mcp::program_file` and hashed from that descriptor
      (`Gone` when it is missing, a link or not a regular file); the
      `ProgramFile` is kept in `CheckedPin` across the secret resolution
      and is the file `connect` starts (D4). `Tree`,
      `Account`, `ProgramGone` or `Program`: `CallError::new(Permission,
      "mcp_server_changed: the server of `{c}` ({program}) is not the one
      approved for account `{a}` ({why}); a person approves it again at a
      terminal (docs/CONNECTORS.md, MCP servers)")`, where `why` names
      what differs and nothing else: `the connector definition changed`,
      `the account changed`, `the program file is gone, a link or not a
      regular file` or `the program file changed`. No refusal carries a digest or a digest prefix;
      full digests appear only in the terminal confirmations (D5).
      `ToolUnpinned` here is item 2.2 below, answered before anything
      starts. A run that started before a person re-approved the
      definition and re-pinned the account is refused here, since its
      snapshot is the old tree.
2. After connecting: list the tools (all pages, raw, within the listing
   limits of D2 "Capturing pins"), find the one named
   `function.mcp.tool`, refuse it when its raw definition holds a
   resolved secret of the account (`mcp_tools_reflect_a_secret`, the walk
   of "Reflected secrets", before any digest), compute `tool_digest` and compare the whole
   binding again with that digest (`record.compare(&LiveBinding { ..,
   tool_digest: Some(d) })`; the tree and program facts are the ones of
   item 1.2, held, not read again; on a platform that starts by path the
   start itself hashed the path again, D4):
   1. Not listed: `CallError::new(Config, "mcp_tool_missing: the server
      no longer lists tool `{t}` that `{c}.{f}` maps")`.
   2. `ToolUnpinned`: `CallError::new(Permission, "mcp_tool_unpinned: tool
      `{t}` of `{c}` is not approved for account `{a}`; a person approves
      it at a terminal (docs/CONNECTORS.md, MCP servers)")`.
   3. `Tool`: `CallError::new(Permission, "mcp_tool_changed: tool `{t}`
      of `{c}` changed since it was approved for account `{a}`; a person
      approves it again at a terminal, which shows the new definition
      (docs/CONNECTORS.md, MCP servers)")`. The tool is named; no digest
      or digest prefix is.
      mcp-http-and-surfaces, which adds `apb connector tools`, may add
      that read-only command to the text; the code prefix and the class
      stay, and no text names the approve command.

Every one of them happens before the send intent is recorded and before
the request is handed over (D4 "The order of one call"): no intent, no
`connector_call`, no `max_calls` unit. Every comparison of live facts
with a pin goes through `ServerPin::compare`; nothing compares again
after the call. Between the listing and the `tools/call` of one server process
the server could still change the tool; nothing on the client side can
close that window, and a server started per call keeps it to one
conversation. The check runs on every call, never once per run: a server
that changes a description between two calls of one attempt is refused
on the second.

A tool the server lists that no function maps is never callable: the
grant names functions (`call/target.rs` lines 72 to 81), and a function
maps exactly one tool.

### The run gate

`ConnectorScan` (`crates/apb-engine/src/gate/trust_scan.rs` lines 19 to
37) gains:

```rust
    /// `"<connector>/<account>/<tool>"` for every granted mcp function
    /// whose account has no server record, or whose record has no pin for
    /// the function's tool.
    pub unpinned_mcp_tools: Vec<String>,
```

filled in `scan_connectors` (lines 42 to 89): for each resolved connector
with `loaded.doc.mcp`, for every grant of it (`resolution.grants`), for
every function in the grant that `is_mcp`, for every account of the
grant (an empty account list: none), `PinStore::server` and
`record.compare(&LiveBinding { connector_digest: &resolved.loaded.digest,
account_digest: <the merged account's digest, the one the trust scan
already reads for the account approval>, program: ProgramFact::NotRead,
tool, tool_digest: None })`: no record, a record bound to another tree
or another account (a project's account of the same name), or no pin
for the tool is an entry. A store that does not read is not an entry:
`unpinned_mcp_tools` answers `Err(<store path>)` and the scan carries it
in its own field, `mcp_pin_store_unreadable: Option<String>`, with
`unpinned_mcp_tools` empty (fix round 1, spec review Minor 2: a path
read as `c/a/t` made a nonsense remedy). The
gate starts no server and hashes no program file; the program and tool
digests are compared on every call.

Why the gate also checks, when every call compares anyway: the gate and
the call answer different facts, so this is not a check before and a
check after of one comparison. The gate reads only whether a pin for
this tree and tool exists (a file read, no server, no file digest),
through the same `compare` with the facts it has; without it a run would start, spend
agent work and fail at its first call on a refusal only a person can
clear. The program and tool digest comparison runs only in the call
(and the healthcheck, D4 "Live calls outside a run"). The presence
fact is collected in the trust scan with the other trust facts before
any refusal (`trust_scan.rs`), and decided inside `check_connectors`,
the gate's admission, in the same place as the account approval.

`check_connectors` in `gate/mod.rs` (after the
`unapproved_connector_account` return, before the `Ok` at line 1322):

```rust
if let Some(store) = &scan.mcp_pin_store_unreadable {
    return Err(json!({
        "policy": "mcp_tools_unpinned",
        "tools": [],
        "store_unreadable": store,
        "detail": "the MCP pin store does not read; a person moves it aside and approves the MCP tools again at a terminal (docs/CONNECTORS.md, MCP servers)",
    }));
}
if !scan.unpinned_mcp_tools.is_empty() {
    return Err(json!({
        "policy": "mcp_tools_unpinned",
        "tools": scan.unpinned_mcp_tools,
        "detail": "a person approves the MCP tools at a terminal (docs/CONNECTORS.md, MCP servers); acknowledge_untrusted does not bypass tool approval",
    }));
}
```

`next_action/start.rs` `policy_actions` (line 217) gets an arm next to
`"unapproved_connector_account"` (line 266), the same shape that arm
builds: `"mcp_tools_unpinned"` maps each connector and account of the
`tools` entries (`c/a/t`, deduplicated by `c/a`) to `of.fix(Remedy::Manual
{ detail: format!("a person approves the MCP tools of connector `{c}`
for account `{a}` at a terminal") }, format!("nobody approved the MCP
tools of `{c}` for account `{a}`; an acknowledge does not cover it"))`.
`Remedy::Manual` carries no command line (`next_action/start_action.rs`
line 67 and the account arm at `start.rs` lines 266 to 276), so no
surface offers an agent a command to run. A refusal that carries
`store_unreadable` gets one manual remedy of its own instead: "move the
MCP pin store `<path>` aside, then a person approves the MCP tools
again at a terminal". The fit report shows the list through the same
scan: `crates/apb-engine/src/fit/trust.rs` reads `ConnectorScan` (line
94 reads `unapproved_accounts`); add one `mcp_tools` row per unpinned
entry next to the account rows, with the same manual remedy, and one row
"pin store <path> does not read" when the store does not read.

### Wrong implementations (D2)

1. Checking the tool list once per run (at start, or on the first call)
   and caching it. Caught by
   `connector_mcp.rs::a_description_changed_between_two_calls_is_refused`
   (two calls in one attempt, `set_tools` between them, the second is
   `mcp_tool_changed` and the stub's `calls.log` has one line).
2. Hashing the definition with `serde_json::to_string` on the raw value
   (key order follows the server or the map type). Caught by
   `digest::tests::key_order_does_not_change_the_digest`.
3. Pinning `icons` or `_meta` (a server that rotates an icon URL would be
   refused every call). Caught by `digest::tests::icons_and_meta_are_not_pinned`.
4. Writing pins into `trust.json` with a new `Kind`. Caught by
   `pins::tests::the_trust_store_is_untouched` (after `record`, `trust.json`
   parses with the 1.0.0-beta.7 `TrustStore` shape: no unknown kind).
5. Making an unparsable pin store read as empty and letting the calls
   then fail `unpinned` (a person re-approves and silently loses the old
   pins). Caught by `pins::tests::an_unparsable_store_is_an_error`.
6. Treating a `Changed` tool as pinned when the new description is "only
   longer". Caught by the `set_tools` test above (it appends one word).
7. Starting the server in the run gate to check digests (code runs at
   every start, even a refused one). Caught by
   `connector_mcp_gate.rs::the_gate_starts_no_server` (the stub's
   `args.log` does not exist after a refused and an admitted gate check).
8. Pinning a `missing` tool as an empty digest. Caught by
   `connector_mcp_cli_test.rs::approve_reports_a_missing_tool_and_pins_the_rest`.
9. Digesting a hand-kept list of members (`name`, `title`,
   `description`, ...) instead of the whole object minus
   `UNPINNED_MEMBERS`, so a member a later revision adds goes unpinned.
   Caught by `digest::tests::a_member_no_list_names_is_pinned` (adding
   `"execution": {"taskSupport": "optional"}` changes the digest).
10. Digesting rmcp's typed `Tool` converted back with
    `serde_json::to_value`, which drops every member the struct does not
    name, so the unit test above passes while the live path loses the
    member. Caught end to end by
    `connector_mcp.rs::a_changed_execution_member_refuses_the_next_call`
    (the stub lists a tool with an `execution` member; after `set_tools`
    changes only that member, the next call is `mcp_tool_changed` and
    `calls.log` holds one line).
11. Reading the pin store through a symbolic link below the config dir,
    or answering a link with an empty store. Caught by
    `pins::tests::a_linked_store_is_refused` (`mcp-tools.json` a symlink
    to a valid store: `load` is an error).
12. Refusing a config dir that is itself a link (every call and every
    approval would fail on a dotfile-managed machine). Caught by
    `pins::tests::a_linked_config_dir_is_followed_once` (the config dir a
    symlink to a real directory holding a valid store: `load` reads it).
13. Ignoring a lock that cannot be taken, as `TrustStore::locked_mutate`
    does. Caught by `pins::tests::record_merges_with_a_concurrent_writer`
    (two threads writing two accounts' records, both present after both
    writes).
14. Looking a bare `command` up in the caller's `PATH` at call time, or
    starting anything but the pinned path (B1 of the review record).
    Caught by `connector_mcp.rs::a_shadow_program_on_the_callers_path_is_not_started`
    (approve with the stub's directory on `PATH`; call with a directory
    first on `PATH` that holds a program of the same name writing a
    marker file: the marker does not exist and the real stub's
    `starts.log` grew by one).
15. Starting a pinned program whose file changed. Caught by
    `connector_mcp.rs::a_changed_program_file_is_refused` (one byte
    appended to the stub after approval: `mcp_server_changed`, no new
    line in `starts.log`, nothing journaled) and
    `a_changed_command_is_refused` (the definition's `command` names
    another program and the tree was approved again: `mcp_server_changed`,
    neither program started; added in fix round 1, spec review Minor 4).
    A file replaced after the hash is caught by
    `a_program_swapped_while_the_secrets_resolve_never_runs_with_them` (a
    `{{cmd:}}` helper renames another script over the program during the
    secret resolution: on Linux the hashed stub runs, elsewhere the start
    is refused `mcp_server_changed`; the swapped-in script never runs) and
    a link by `a_program_turned_into_a_link_is_refused`.
16. Pinning without a person: approving an MCP connector without an
    interactive terminal, or inside a run step. Caught by
    `connector_mcp_cli_test.rs::approve_without_a_terminal_is_refused`
    (no pin record, no account approval, the stub's `starts.log` absent),
    `a_retry_without_a_terminal_learns_nothing` and
    `approve_in_a_run_step_is_refused`.
17. Naming the approve command in a refusal or a start action, so the
    agent that reads it runs it. Caught by
    `connector_mcp.rs::no_refusal_names_the_approve_command` (the texts of
    `mcp_server_unpinned`, `mcp_server_changed`, `mcp_tool_unpinned` and
    `mcp_tool_changed` do not contain `apb connector approve`) and
    `connector_mcp_gate.rs::the_refusal_offers_a_manual_remedy` (the start
    action is `Remedy::Manual` with no command).
18. Reading pins from a redirected config dir inside a run step. Caught
    by `connector_mcp.rs::a_run_step_with_a_redirected_config_dir_is_refused`
    (`APB_RUN_ID` set, `APB_CONFIG_DIR` away from the test default
    config dir: `mcp_config_dir_redirected`, the stub never started).
19. Changing main's approve for a connector of another kind. Caught by
    `connector_mcp_cli_test.rs::approve_of_an_http_connector_is_unchanged`
    (an HTTP connector's account approval inside `APB_RUN_ID` and without
    a terminal still succeeds, as on main).
20. Hashing the program file by path and then starting the path, so a
    file replaced between the two runs with the secrets (fix round 1,
    audit M1). Caught by the swap test of item 15 and the client tests
    `stdio_test::a_program_replaced_after_the_hash_never_runs_from_the_descriptor`
    (Linux), `a_program_replaced_after_the_hash_is_refused_on_the_path_checked_again`
    (the portable path, driven on Linux through the `StartFrom` argument,
    no seam), `an_unchanged_program_starts_on_the_path_checked_again` and
    `a_program_that_is_missing_or_a_link_is_refused_before_any_start`.
21. Keeping pins after the connector is uninstalled or its approval
    revoked. Caught by `install::tests::uninstall_forgets_the_connectors_mcp_pins`
    and `pins::tests::a_revoked_approval_takes_its_pins_with_it`.

## D3. Effects, consent and send once

### Who decides the effect

The author, per function, exactly as for HTTP functions: `read_only`,
`irreversible` and `idempotency` (D1 rules). The server's
`readOnlyHint`, `destructiveHint` and `idempotentHint` are shown to a person (the approve output of D2) and read by nothing
else. `irreversible` MCP functions reach
V73 and the unattended rules through `ConnectorFacts.irreversible_functions`
(`resolve.rs` line 477) like any other connector function; the run's
irreversible consent is unchanged (proposal, out of scope 8).

### Send-once modes

| Mode | Allowed on an mcp function | How a repeat is decided |
|---|---|---|
| `key` | no (D1 rule 5): the protocol has no idempotency key field | |
| `marker` | yes, on a string argument of `args_schema`; needs `identity` and a read-only mcp `reconcile` function of the same connector | the ledger reconciles an unknown send by listing recent items through the reconcile function and matching the marker byte for byte (`intent/reconcile.rs`, unchanged) |
| `none` | yes | an unknown send stays unknown until a person resolves it (`apb intent resolve`), or the binding's `on_unknown: resend` allows a resend (`docs/CONNECTORS.md` lines 728 to 755) |
| `repeatable` | yes | not tracked (a repeat is harmless) |

The ledger, its admission table and its settlement are unchanged
(`call/send_once.rs` `gate` line 109, `finish` line 461). The marker rides
in the tool's arguments (D4 "The order of one call" step 4). The reconcile read goes
through `read_back` (line 406) as apb's own call with `full: true`, so it
gets the MCP body shape below, and its `items`, `text`, `author` and
`item_ref` paths are dot paths into that body.

### When a call counts as handed over

The one line that decides `not_sent` against `unknown`: the moment
`McpCall::send` calls `Session::call_tool`. Before it (the pin record
and program check, spawn, connect, discover, initialize, list, the tool
pin check, argument shaping), nothing can have reached the tool. The
order of one call is D4 "The order of one call"; for the settlement:

1. Every failure before the send intent is recorded (everything up to
   and including the tool pin check) is a `CallError` returned before
   `send_once::gate` records anything: no intent, no `connector_call`, no
   `max_calls` unit. Nothing to settle.
2. A failure after the intent is recorded and before `call_tool` (a
   deadline already passed) takes the pre-dispatch path of `prepare`
   (`call/mod.rs` lines 564 to 573, `settle_unsent` at line 570): the
   intent settles `not_sent` with source `unsent`, no `connector_call` is
   journaled. Starting the server and checking the pins are never in this
   window: a live mcp call records its intent only after both (D4 "The
   order of one call", the `NeedsStart` answer of the gate), and the
   marker stamping that follows cannot fail.
3. From `call_tool` on, the result is journaled as a `connector_call` and
   the intent is settled by `send_once::finish` from the call's facts
   (`outcome`, `http_status`, `response_complete`) through
   `effect::classify` (`crates/apb-engine/src/connector/effect.rs` lines
   36 to 65). A call that succeeded closes its session after the
   settlement; a call that failed closes it before its result is
   journaled and its intent settled, so the close's warning reaches the
   settlement (D4 "The order of one call", step 7).
4. One failure from `call_tool` on is still known to precede the
   hand-over: `CallFailure::NotSent`, when no byte of the `tools/call`
   request line reached the server's stdin (the session closed, its
   worker gone, or the pipe refused the first write; the line tap counts
   the `tools/call` lines the pipe accepted at least one byte of,
   `calls_handed`). It maps to code `service` with the message "the call
   never reached the MCP server; nothing was sent: ...", which classifies
   `Failed`, so a recorded intent settles `not_sent` and a repeat goes
   out (fix round 1, audit M8 and spec review Minor 14). Any byte handed
   over makes the failure `Lost`, which stays `unknown`. The count is
   trustworthy only because every line is classified before it is
   forwarded (D4 "The client", item 3.2): a line the tap could not read
   is refused unforwarded, never passed on uncounted (fix round 3).
5. `NotSent` needs proof that the request can no longer be transmitted,
   not only that it was not transmitted yet (fix round 2, review
   Blocker 1). A `tools/call` that ends without an answer (a timeout, a
   dropped future, a broken transport) may still sit in rmcp's queue or
   in the tap writer's buffer, and rmcp's writer task would write it
   later, even during the close. So every unanswered outcome first seals
   the line tap (`LineTap::seal`): the tap writer is the only holder of
   the child's stdin, and it forwards bytes only inside `forward`, which
   checks the seal, makes one non-blocking pipe write and counts the
   `tools/call` lines handed over, all under the tap's `WriteGate` lock;
   `seal` takes the same lock, sets the seal and reads the count. Once
   `seal` returns, either a write that began before it is fully counted,
   or no byte ever reaches the pipe again, whatever rmcp still holds or
   hands the writer later. Only a count unchanged since before the
   request is `NotSent`; anything else is `Lost`. The writer is sealed
   rather than stdin closed because the close cannot be made atomic with
   the count read, while the seal is one locked step with it; a sealed
   writer's shutdown closes the pipe without forwarding what it holds.
   A sealed session writes nothing more, and the engine closes it after
   the failure as before. The invariant is stated at the top of
   `Conn::call_tool`.

### Mapping (`connector/mcp/map.rs`)

```rust
/// A connect, list or pin failure, before the call is handed over.
pub(crate) fn connect_error(f: apb_mcp_client::Failure, redactions: &[Redaction]) -> CallError;

/// The answer of a call that was handed over.
pub(crate) fn answer(
    answer: Result<apb_mcp_client::ToolAnswer, apb_mcp_client::CallFailure>,
    read_only: bool,
    response_pick: &[String],
    redactions: &[Redaction],
) -> Result<CallOk, CallError>;
```

`connect_error`: `Spawn` and `Transport` are `network` ("the MCP server
could not be reached: ..."), `Timeout` is `timeout`, `Unsupported` is
`config`, `TooLarge` is `service` (its message starts
`mcp_message_too_large:` when the line cap or the session cap was hit,
D4 item 3), `Rejected` is `service`, `Refused` and `ProgramChanged` are
`permission` (`Refused` with the guard's own text; `ProgramChanged`, the
path hashed again before the spawn did not match, is worded by the
engine as its own `mcp_server_changed`) (mcp-http-and-surfaces adds
`Auth` as `auth`). The client's message may carry server data, so it
passes `server_text` (below). All of them happen
before the send intent is recorded (D4 "The order of one call"), so
nothing is settled; the code only names the failure.

`answer`, the table the tests check row by row:

| Answer | `CallOk` or `CallError` code | `classify` for a write | Settlement of a tracked send |
|---|---|---|---|
| `Complete`, `is_error: false` | `CallOk::Mcp` | `WriteOk` (or `WriteIncomplete` when cut) | `confirmed` |
| `Complete`, `is_error: true` | `tool_error` (new) | `WriteUnknown` | `unknown` |
| `Task` | `tool_error`, message "the server started a task apb does not follow; its effect is unknown" | `WriteUnknown` | `unknown` |
| `InputRequired` | `tool_error`, message "the server asked for input apb cannot give (multi round-trip); whether the tool acted before it asked is not known" | `WriteUnknown` | `unknown` |
| `Answered(Rejected { code })` of a write, any code (-32700, -32600, -32601 and -32602 included) | `tool_error`, message "the server answered the call with JSON-RPC error {code} ({message}); whether the tool acted before it failed is not known" | `WriteUnknown` | `unknown` |
| `Answered(Rejected { code })` of a `read_only` function | `service` for -32700 and -32600, `config` for -32601, `invalid_args` for -32602, `tool_error` for any other code | `Failed` (read) | none (a read is not tracked) |
| `Lost(Timeout)` | `timeout` | `WriteUnknown` | `unknown` |
| `Lost(Transport)` (the pipe broke or the server exited after the request was written, a send that may have written part of it, an answer over the line cap) | `network` | `WriteUnknown` | `unknown` |
| `NotSent(_)` (no byte of the request line reached the server's stdin) | `service`, message "the call never reached the MCP server; nothing was sent: ..." | `Failed` | `not_sent` |

For a `read_only` function every error row is `Failed` (classify's read
branch), and `tool_error` is a failed read. For a write, `not_sent` comes
only from failures known to precede the hand-over ("When a call counts
as handed over", items 1, 2 and 4); every answer received after it, a
JSON-RPC error of any code included, settles `unknown`.

Why `input_required` is `unknown`. The multi round-trip pattern
(https://modelcontextprotocol.io/specification/2026-07-28/basic/patterns/mrtr)
says the request is incomplete and needs more input; it does not say the
server did nothing before it asked. Only an answer that proves nothing
happened settles `not_sent`, so `input_required` on a write is an
unknown write, like a task handle.

Why no JSON-RPC code is `not_sent` for a write. JSON-RPC 2.0 gives
-32700, -32600, -32601 and -32602 to a request that was not executed,
but a code is the server's own word, received after the request was
handed over: a tool handler can act in part, find an invalid parameter
and answer `-32602`, and SDKs commonly answer an exception thrown inside
a tool handler with `-32603` after part of the work was done. On main a
`Failed` settlement is `not_sent` (`call/send_once.rs` line 479) and a
`not_sent` tip admits a new send (`intent/ledger/mod.rs` lines 584 to
586), so a code taken at its word would let a write act twice. Only a
failure apb itself knows came before the hand-over proves nothing
reached the tool. On main the
HTTP twin of that case, a 5xx answer to a write, is `WriteUnknown`
(`effect.rs` lines 47 to 51: `server_error` joins `timeout` and `network`
in `undetermined`); an MCP server error is treated the same, through the
`tool_error` outcome, which `classify` already counts as undetermined
(below), so no MCP code reaches `classify` by an HTTP status it does not
have.

Why `isError` is unknown for a write: the specification puts "API
failures" among tool execution errors (tools page, "Error Handling"), so
a tool that reports an error may have acted before it failed. A marker
send gets an unknown settlement reconciled automatically on its repeat;
a `none` send waits for a person, or a binding's `on_unknown: resend`.

`CallErrorCode` (`crates/apb-engine/src/connector/result.rs` lines 17 to
39; on main its last variant is `AttemptClosed`, and eval-doubles adds
`DoubleUnmatched`, `"double_unmatched"`, after it) gains, as the new last
variant:

```rust
    /// An MCP call whose effect is not known: the tool reported an error
    /// (`isError`), answered with a task or `input_required`, or the
    /// server answered a write with a JSON-RPC error of any code (D3
    /// table); for a read, a JSON-RPC error with no request-error code.
    ToolError,
```

with `as_str` `"tool_error"` (the match at line 43). `effect::classify`
(lines 36 to 65): `undetermined` also holds for `outcome == "tool_error"`
(the `timeout` / `network` / 5xx line, 49 to 51). The other reader of
outcome strings that asks whether a call may have acted,
`crates/apb-engine/src/grade/rules/consent.rs` `MAY_HAVE_ACTED` (line 28,
`[&str; 6]`), gains `"tool_error"` (`[&str; 7]`), so the consent rule
counts an MCP tool error as a call that may have acted. The readers that
count every non-ok outcome (`known_problems/extract.rs` line 90,
`grade/rules/loops.rs` line 95) need no change. An older apb that reads a
journal with `tool_error` classifies it `Failed` in its outcome view and
leaves it out of `MAY_HAVE_ACTED`; the send ledger reads the journaled
`send_intent_settled` outcome, not the call class, so its verdict is the
same in both binaries.

`CallOk` (lines 146 to 188) gains:

```rust
    /// An MCP tool's answer (`connector::mcp::map`).
    Mcp {
        /// `structuredContent` when present; else the text blocks, joined
        /// with a newline, parsed as JSON when they parse; else
        /// `{"text": <joined text>}`.
        body: Value,
        /// Set when the serialized body passed `BODY_CAP` (D4 "Size").
        incomplete: Option<BodyIncomplete>,
        picked: bool,
        /// Non-text content blocks (image, audio, resource, resource link)
        /// left out of `body`, by count.
        dropped: u32,
    },
```

`to_success_json` (line 199): `{ "ok": true, "body": body }`, plus
`"picked": true` when picked and `"dropped_content": n` when `n > 0`;
`incompleteness` (line 258) maps `incomplete` like `Http`; `body` (line
268) returns it; `response_complete` (line 280) is `Some(incomplete.is_none())`.

Body rules, in `map::body`:

1. `structured: Some(v)`: `v` (any JSON value, explicit `null` included).
2. Else the `content` blocks whose `type` is `"text"`, their `text`
   joined with `"\n"`; when the joined text parses as JSON
   (`serde_json::from_str`), that value; else `json!({ "text": joined })`.
   No text block at all: `json!({ "text": "" })`.
3. Every other block counts in `dropped` and is not returned (binary data
   and links the agent cannot use through `apb connector call`).
4. Then the redactions over the body through `mcp::redact_tree`, the
   walk the listing check uses: every string, every object member name
   and the JSON text of every number (a number whose text holds a secret
   becomes the redacted text, a string), so a secret a server reflects
   as a key of `structuredContent` or as a JSON number is redacted too
   (fix round 1, audit M6). HTTP calls keep `call/response.rs`
   `redact_value`. Then `response_pick` when it is not empty (the HTTP
   projection over the same dot paths), `picked: true`.

Size: the serialized body (before projection) longer than `BODY_CAP`
(1 MiB, `call/wire.rs` line 11) is cut: for a `read_only` function the
call fails `incomplete_response`
(`CallError::new(IncompleteResponse, ..)` with `incomplete:
Some(BodyIncomplete::cut_at_cap(BODY_CAP))`); for a write it succeeds with
`body: Value::Null` and `incomplete` set, which classifies
`WriteIncomplete` and settles `confirmed`, as an HTTP write with a cut
body does.

Redaction and bounds of server text: every text the server provides
(a tool's `isError` text, a JSON-RPC error message, the client message
of a lost or not-sent call, every connect or listing failure message,
which may carry server data) passes `mcp::server_text`: the call's
redactions (every resolved secret of the account) over the whole text
first, then a cut to `SERVER_TEXT_CHARS` = 500 characters on a
character boundary with a `[cut: N more characters]` marker. Never cut
before the redaction: the redactions match exact values, so a secret
across the cut would leave its prefix unredacted (fix round 1: spec review Major 2 and Minor 5, audit M5 and m6).
The `isError` text is the text blocks joined with a newline as they
are, never parsed as JSON, so an error text such as `{"code":7}` or
`42` is kept as text (spec review Minor 6). `Failure.message` never
carries arguments.

The protocol version a server reports is compared whole with
`SUPPORTED_VERSIONS` right after the handshake, before anything keeps,
cuts, shows or stores it; only the matching constant goes into `Era`,
and the healthcheck's `mcp.era` and the approval's header are built from
that constant. Any other string ends the session with `FailureKind::
Unsupported` and the fixed message `mcp_protocol_unsupported: the MCP
server answered with a protocol version this client does not support`,
which never echoes it; rmcp's `NoCompatibleProtocolVersion`, whose text
lists the versions a server offered, maps to the same fixed message,
also inside `LegacyFallbackFailed` (fix round 2, review Major 2).

Every other server-provided string apb keeps or shows, and what it
passes before use:

| String | Kept or shown where | Whole-string redaction | Bound |
|---|---|---|---|
| protocol version | `Era`, healthcheck `mcp.era`, approval header | not needed: only a supported constant is kept | the constant itself |
| server name, title, version, instructions | not read: apb never reads `peer_info` beyond the version | not kept | not kept |
| capability names | not read | not kept | not kept |
| tool `name` of an unmapped tool | listing `unmapped`, approval | `server_name` (account redactions over the whole name) | `SERVER_NAME_CHARS` = 200 per name, then the approval's `UNMAPPED_CHARS` |
| tool `name` of a mapped tool | compared with the definition's tool name; the row shows the definition's string | not kept from the server | not kept from the server |
| tool `title` | listing row, approval | `server_name` | `SERVER_NAME_CHARS`, then `TITLE_CHARS` |
| tool `description` | listing row, the pin record, approval | `display_description` | `DESCRIPTION_CHARS` = 300 |
| tool member names | the pin record's `members`, a changed tool's member list | `server_name` | `SERVER_NAME_CHARS` each, then `MEMBERS_CHARS` |
| `inputSchema`, `annotations` | listing row, approval | `redact_tree` (every string, member name and number) | the approval's `SCHEMA_CHARS` and `ANNOTATIONS_CHARS` over the field, the listing's 2 MiB byte cap |
| the whole tool object | never shown; its digest is kept | the reflected-secret walk refuses the listing on any secret | `max_bytes` of the listing |
| JSON-RPC error message, rmcp error text, `isError` text | `CallError.message`, the settlement's `detail`, the approval's error line | `server_text` over the whole composed message (`map::composed`) | `SERVER_TEXT_CHARS` = 500 |
| a tool's answer body | the call's result | `redact_tree` | `BODY_CAP` (1 MiB) |

Every place a server-provided number or member name reaches a result, a
journal detail or the terminal (fix round 3, review Major 3; the class
is "a server value interpolated after the redaction"):

| Server number or key | Where it is interpolated | Through |
|---|---|---|
| JSON-RPC error code of a call | `map::answer`, "JSON-RPC error <code>": `CallError.message`, the settlement's `detail` | `map::composed`: the whole composed message redacted, then bounded; the code stays an `i64` only to classify (`FailureKind::Rejected`) |
| JSON-RPC error code of a connect or listing request | `Failure.message` (rmcp's text) inside `map::connect_error`'s message | `map::composed` |
| any number inside rmcp's error text (`ServiceError`, `ClientInitializeError`) | `Failure.message` of every client failure | `map::composed` at every `CallError` the mcp kind builds from a `Failure` (connect, list, not sent, lost, answered) |
| numbers and member names of an `isError` text | the tool error message | `map::composed` (the text is never parsed) |
| numbers and member names of a tool's answer body | the call's result | `redact_tree` (every string, member name and number's JSON text) |
| numbers and member names of `inputSchema`, `annotations` | listing rows, the approval | `redact_tree` |
| a tool's member names | the pin record, a changed tool's members | `server_name` |
| the whole tool object, numbers included | its digest only | the reflected-secret walk (`redact_tree`) refuses the listing first |
| a healthcheck refusal's message | the healthcheck answer | `map::composed`, then `mod::redacted` over the whole message again |

The counts apb shows (`tools_checked`, `dropped_content`, the tools
per status) are apb's own counts, not server values.

Before this round the title, the unmapped names and the member names
were redacted but bounded only on the terminal; they are now bounded in
the listing itself, on the compared and the stored side alike.

### Wrong implementations (D3)

1. Settling `not_sent` on a transport error after the request was written
   (treating "the pipe broke" as "nothing went out"). Caught by
   `connector_mcp_send.rs::a_server_that_exits_after_reading_the_call_leaves_it_unknown`
   (`StubCall::ExitAfterRead`: `send_intent_settled.outcome == "unknown"`,
   the result has `"effect": "unknown"`, a repeat is refused
   `unknown_effect` for mode `none`).
2. Settling a timeout `not_sent`. Caught by
   `connector_mcp_send.rs::a_call_that_times_out_is_unknown` (`StubCall::Hang`,
   `timeout_sec: 2`, `protocol: modern`, since rule 3.4.4 refuses 2 under
   `auto`).
3. Settling `isError: true` as `not_sent` (a repeat would act twice).
   Caught by `connector_mcp_send.rs::a_tool_error_on_a_write_is_unknown_and_a_marker_repeat_reconciles`
   (the second attempt's repeat lists comments through the reconcile
   function, finds the marker in the stub's canned list and answers
   `already_sent` with no second `tools/call` in `calls.log`).
4. Settling `unknown` for a failure before hand-over, or recording an
   intent before the server is known to start and list (a killed call
   would leave a pending intent that reads unknown and blocks every
   repeat). Caught by
   `connector_mcp_send.rs::a_server_that_cannot_start_records_no_intent`
   (the pinned program's execute bit cleared after approval, so its
   digest still matches and the spawn fails: no `send_intent_recorded`,
   no `connector_call`) and
   `a_pin_refusal_records_no_intent`.
5. Allowing `idempotency: { mode: key }` on an mcp function. Caught by
   `mcp::tests::key_is_refused_on_mcp`.
6. Appending the marker before neutralizing forged ones, or not
   neutralizing at all for MCP. Caught by
   `connector_mcp_send.rs::a_forged_marker_in_the_arguments_is_neutralized`
   (the `tools/call` line in `calls.log` carries `apb-quoted-intent` for
   the agent's text and exactly one `apb-intent`).
7. Using the server's text blocks without the redactions, cutting a
   server text before the redaction, or leaving it unbounded. Caught by
   `connector_mcp.rs::a_secret_echoed_by_the_tool_is_redacted` and the
   `map` tests `a_secret_across_the_cut_is_redacted_whole_before_the_cut`
   (the secret at character 495), `server_text_has_a_bound_everywhere`
   and `an_error_text_that_reads_as_json_is_kept_as_text`.
8. Settling an `InputRequired` write `not_sent`, so the ledger admits a
   repeat of a write the server may have acted on. Caught by the `map`
   table test `map::tests::every_answer_maps_as_the_table_says` and
   `connector_mcp_send.rs::input_required_settles_unknown`.
9. Leaving `tool_error` out of the consent grader's `MAY_HAVE_ACTED`, so
   a run whose MCP write reported an error grades as if nothing acted.
   Caught by `grade_rules_test.rs::a_tool_error_call_may_have_acted`.
10. Settling a JSON-RPC server error on a write `not_sent`, so the ledger
    admits a fresh send and a `none` or `marker` write acts twice (M2 of
    the review record). Caught by
    `connector_mcp_send.rs::a_server_error_on_a_write_is_unknown`
    (`StubCall::Error { code: -32603 }`: outcome `tool_error`,
    `send_intent_settled.outcome == "unknown"`, a `none` repeat refused
    `unknown_effect`), `a_request_error_after_acting_is_unknown` (a stub
    that appends to `calls.log`, acts, then answers `-32602`: `tool_error`,
    `unknown`, a `none` repeat refused) and the request-error rows of
    `map::tests::every_answer_maps_as_the_table_says`.
11. Redacting string values of an answer only, so a secret reflected as
    a member name or a number reaches the journal and the agent. Caught
    by `map::tests::a_secret_as_an_answer_key_or_number_is_redacted` and
    `mcp::tests::the_walk_redacts_strings_member_names_and_numbers`.
12. Settling `unknown` for a call that never reached the server after its
    intent was recorded, so a repeat is blocked for nothing. Caught by
    `connector_mcp_send.rs::a_server_gone_before_the_call_settles_not_sent`
    (the stub closes its stdin, answers the listing and exits: one
    `not_sent` settlement, and the repeat goes out), the client test
    `a_call_to_a_server_that_already_exited_is_not_sent` and the tap tests
    `a_call_line_the_pipe_took_is_handed_over` and
    `a_call_line_the_pipe_refused_is_not_handed_over`.

## D4. Transports and the call (stdio)

### The client (`crates/apb-mcp-client/src/`)

`lib.rs` re-exports the session types and declares `mod session;`,
`mod stdio;`, `mod tap;` (the line tap of item 3), `mod frame;` (the pure frame
classification the tap uses and `check_arguments`, fix round 3), and `#[cfg(feature = "testing")] pub mod testing;`. The
public API, all blocking:

```rust
/// A value of the server's environment: a plain account field, or a
/// resolved secret, exposed only when the process is spawned.
pub enum EnvValue {
    Plain(String),
    Secret(apb_core::secret::Secret),
}

/// How a session starts a `ProgramFile`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StartFrom {
    /// Linux: executed through `/proc/self/fd/<n>`, the descriptor the
    /// digest was read from. The file that runs is the file that was
    /// hashed, whatever happens to its path afterwards.
    Descriptor,
    /// Every other platform (macOS): the path opened again without
    /// following a link and hashed again immediately before the spawn; a
    /// different digest refuses the start. The window between that second
    /// hash and the exec remains.
    PathCheckedAgain,
}

impl StartFrom {
    /// `Descriptor` on Linux, `PathCheckedAgain` everywhere else.
    pub fn platform() -> StartFrom;
}

/// Why a program file cannot be used.
pub enum ProgramRefusal {
    /// Missing, a link (never followed), or not a regular file.
    NotARegularFile,
    /// It could not be opened or read.
    Unreadable,
    /// `PathCheckedAgain`: the path no longer holds the hashed bytes.
    Changed,
}

/// The one open of a program file: opened once with `O_NOFOLLOW |
/// O_NONBLOCK` (no link followed at its last component, never blocked on
/// a FIFO), close-on-exec, a regular file only (`fstat`), its sha256 read
/// from that descriptor, the descriptor kept open until the session
/// starts the program. Every MCP start (a call, the pin-check
/// healthcheck, an approval's listing) and every digest of a program file
/// goes through it.
pub struct ProgramFile { /* private: the path, the descriptor, the digest, the StartFrom */ }

impl ProgramFile {
    pub fn open(path: &std::path::Path, start: StartFrom) -> Result<ProgramFile, ProgramRefusal>;
    pub fn path(&self) -> &std::path::Path;
    /// `sha256:<hex>` of the file's bytes as this descriptor read them.
    pub fn digest(&self) -> &str;
    pub fn start_from(&self) -> StartFrom;
}

/// A stdio server to start. `env` is the whole environment: the client
/// clears the inherited one first. The working directory is a fresh empty
/// private directory the session makes and removes; it is never the
/// caller's.
pub struct StdioServer {
    /// The program file the person's approval pinned (D2), opened and
    /// hashed once. The client never looks a name up.
    pub program: ProgramFile,
    pub args: Vec<String>,
    pub env: Vec<(String, EnvValue)>,
}

pub enum Server {
    Stdio(StdioServer),
    // mcp-http-and-surfaces adds `Http(HttpServer)`.
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Lifecycle {
    Auto,
    Modern,
    Legacy,
}

/// The protocol versions this client speaks, the only strings an `Era`
/// holds (fix round 2, review Major 2).
pub const SUPPORTED_VERSIONS: [&str; 5] =
    ["2026-07-28", "2025-11-25", "2025-06-18", "2025-03-26", "2024-11-05"];

/// The revision the conversation runs on, with the protocol version: one
/// of `SUPPORTED_VERSIONS`, never a string the server sent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Era {
    /// `server/discover` answered; the negotiated version.
    Modern(&'static str),
    /// `initialize`; the version the server answered with.
    Legacy(&'static str),
}

/// One `tools/call` answer the server completed.
#[derive(Debug, Clone)]
pub enum ToolAnswer {
    /// `content` blocks as JSON objects, `structuredContent` when present
    /// (an explicit `null` is `Some(Value::Null)`), `isError` (absent is
    /// `false`).
    Complete {
        content: Vec<serde_json::Value>,
        structured: Option<serde_json::Value>,
        is_error: bool,
    },
    /// `resultType: input_required`: the server needs input apb does not give.
    InputRequired,
    /// The server answered with a task handle (Tasks extension).
    Task,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FailureKind {
    /// The program could not be started.
    Spawn,
    /// The deadline passed.
    Timeout,
    /// The transport broke (the process exited, a pipe closed, a read failed).
    Transport,
    /// The server answered with a JSON-RPC error.
    Rejected { code: i64 },
    /// The server is legacy under `Lifecycle::Modern`, or speaks no
    /// version this client knows.
    Unsupported,
    /// A list went past its bounds (`list_tools` limits), or a line from
    /// the server passed `MAX_LINE_BYTES` (the message then starts
    /// `mcp_message_too_large:`).
    TooLarge,
    /// `apb_core::process_mode::guard_program_start` refused the spawn (a
    /// read-only or a scenario process).
    Refused,
    /// `StartFrom::PathCheckedAgain`: the program's path no longer held
    /// the hashed bytes (or no regular file) right before the spawn;
    /// nothing was started. The engine words it as `mcp_server_changed`.
    ProgramChanged,
}

/// What the close saw of the server's process group after the server was
/// reaped.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GroupEnd {
    /// No process of the group was left once the server was reaped.
    Empty,
    /// The group was not seen empty within the close's bound: a process
    /// the server started may still run. Nothing is claimed about a
    /// process that left the group.
    NotShownEmpty,
}

/// How a session's server process ended. Only `Reaped` says it is gone:
/// the child handle's `wait` returned its exit status.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Ended {
    /// The server exited after its stdin closed, or after the group kill,
    /// and was reaped; `killed` when it had not exited by itself. `group`
    /// is what the close saw of the rest of its process group.
    Reaped { killed: bool, group: GroupEnd },
    /// The kill was sent through the handle but `wait` did not return
    /// within its bound; whether the process is gone is not known.
    Unconfirmed,
    /// No process was started (a spawn failure) or `close` already ran.
    NotStarted,
}

#[derive(Debug, Clone)]
pub struct Failure {
    pub kind: FailureKind,
    /// Plain text; never contains an argument or a secret the caller passed
    /// (the engine redacts again anyway).
    pub message: String,
    /// What the bounded close after a failure proved. `NotStarted` when
    /// the failure came before the spawn or the session stays open (a
    /// `list_tools` failure, which the engine closes through the session).
    pub ended: Ended,
}

/// Why a `tools/call` has no answer.
#[derive(Debug, Clone)]
pub enum CallFailure {
    /// No byte of the request reached the server's stdin: the session was
    /// closed, its worker gone, or the pipe refused the first write.
    NotSent(Failure),
    /// The server answered the call with a JSON-RPC error.
    Answered(Failure),
    /// The request was handed to the transport and no answer came: a
    /// timeout, a closed or broken transport, an unexpected answer.
    Lost(Failure),
}

pub struct Session { /* private: the worker thread and its channels */ }

impl Session {
    /// Starts the server and opens the conversation in `lifecycle`, as client `client_name` /
    /// `client_version` with no client capabilities. Bounded by `deadline`.
    /// Every failure after the spawn (a handshake error, the deadline, a
    /// refused version, an oversize line) runs the bounded close of
    /// `close` inside the worker before `Failure` is returned: stdin
    /// closed, up to 3 s for the exit (unreaped), the kill of the server's
    /// process group, up to 2 s to reap, up to 1 s to see the group
    /// empty. `Failure.ended` says what that close proved
    /// (`Reaped` or `Unconfirmed`; `NotStarted` for a failure before the
    /// spawn), and the engine words it through `end_warning` like every
    /// other end. `kill_on_drop` stays a backstop only, never the path a
    /// failure takes.
    pub fn connect(
        server: Server,
        lifecycle: Lifecycle,
        client_name: &str,
        client_version: &str,
        deadline: std::time::Instant,
    ) -> Result<Session, Failure>;

    pub fn era(&self) -> &Era;

    /// Every tool, following `nextCursor`, as the raw JSON objects the
    /// server sent (read off the wire by the line tap, item 4, never
    /// converted back from rmcp's typed `Tool`). At most `max_pages` pages,
    /// `max_tools` tools and `max_bytes` raw result bytes summed over the
    /// pages, else `TooLarge`; the engine passes `MCP_LIST_MAX_PAGES` (20),
    /// `MCP_LIST_MAX_TOOLS` (500) and `MCP_LIST_MAX_BYTES` (2 MiB) on every
    /// path (D2 "Capturing pins", "Listing limits").
    pub fn list_tools(
        &self,
        deadline: std::time::Instant,
        max_pages: u32,
        max_tools: usize,
        max_bytes: usize,
    ) -> Result<Vec<serde_json::Value>, Failure>;

    /// One `tools/call` with no multi round-trip follow-up.
    pub fn call_tool(
        &self,
        name: &str,
        arguments: serde_json::Map<String, serde_json::Value>,
        deadline: std::time::Instant,
    ) -> Result<ToolAnswer, CallFailure>;

    /// Ends the conversation. The engine calls it after a successful
    /// call's result is journaled and its intent settled, and before a
    /// failed call's result is journaled (D4 "The order of one call").
    /// Closes the server's stdin (by ending the rmcp service, which drops
    /// the write half) and waits up to 3 s for the server to exit without
    /// reaping it (`waitid` with `WNOWAIT`); then, the server still
    /// unreaped, so its pid, which is its group's id, cannot have been
    /// reused, sends SIGKILL to its whole process group (`killpg`); reaps
    /// the server within 2 s; then looks up to 1 s for the group to be
    /// empty (`killpg(pgid, 0)` answering `ESRCH` proves it). At most 6 s.
    /// It signals no process group but its own server's, and that one only
    /// while the server is unreaped. Idempotent (a second call answers
    /// `NotStarted`); `Drop` calls it and ignores the answer. A process
    /// that is killed runs no `Drop` and no `kill_on_drop`: what then ends
    /// its server is D4 "The order of one call", "The crash limit".
    pub fn close(&mut self) -> Ended;
}

/// The longest line the client accepts from a server, its own limit next
/// to the engine's body cap (`BODY_CAP`, 1 MiB, `call/wire.rs` line 11):
/// seven times the body cap plus 64 KiB for the JSON-RPC envelope, 7 405 568
/// bytes, so the worst escaping a JSON encoder writes of a body at the cap
/// fits (worked out under item 3). A constant here because the client
/// does not import the engine; a unit test of the engine checks
/// `MAX_LINE_BYTES == 7 * BODY_CAP + 64 * 1024`.
pub const MAX_LINE_BYTES: usize = 7 * 1024 * 1024 + 64 * 1024;

/// The most bytes one session reads from a server over all its lines:
/// four of the longest lines (a handshake, at most 2 MiB of listing pages
/// and one answer).
pub const MAX_SESSION_BYTES: usize = 4 * MAX_LINE_BYTES;
```

Implementation rules:

1. Threading. `connect` spawns one OS thread named `apb-mcp-client` that
   builds `tokio::runtime::Builder::new_current_thread().enable_all()` and
   runs the whole conversation inside one `block_on`. The caller talks to
   it through a `tokio::sync::mpsc::UnboundedSender<Command>` and waits on
   a `std::sync::mpsc` reply channel with `recv_timeout(remaining +
   REPLY_CLEANUP_ALLOWANCE)`, `REPLY_CLEANUP_ALLOWANCE` = 7 s: the bounded
   close a failure runs inside the worker before it answers (3 s for the
   exit after end of input, 2 s to reap after the group kill, 1 s to see
   the group empty) plus 1 s for the reply to cross the channel. The deadline bounds the conversation; the
   allowance is the cleanup on top of it, so a handshake that fails at
   its deadline is answered after its close and wait, never before: the
   worker sends the failure only once the close returned, and the caller
   waits for that answer. If even the allowance passes (a worker that
   cannot run), `connect` answers `Failure { kind: Timeout, ended:
   Unconfirmed }`, which `end_warning` words as a server not confirmed
   ended, never as a stop. The same allowance is added to the reply wait
   of `list_tools`, `call_tool` and `close` (6 s of close plus 1 s). No method ever calls `block_on` on the caller's thread, so the session
   works from inside an async handler.
2. Lifecycle to rmcp: `Auto` is `ClientLifecycleMode::Auto {
   preferred_versions: vec![ProtocolVersion::V_2026_07_28], legacy_version:
   Some(ProtocolVersion::V_2025_11_25) }`; `Modern` is `Discover {
   preferred_versions: vec![ProtocolVersion::V_2026_07_28] }`; `Legacy` is
   `Initialize` with the client info's protocol version set to
   `V_2025_11_25`. The whole `serve_with_lifecycle` future runs under
   `tokio::time::timeout(remaining)`, so the deadline bounds the discover
   probe too: rmcp's own 10 s probe timeout is fixed (rmcp 3.9.0
   `src/service/client.rs` lines 722 and 820) and only shortens, never
   lengthens, the wait. An elapsed deadline here is `Timeout`. D1 rule
   3.4.4 keeps `auto` from being configured with a deadline the probe
   always outlasts. The service is `ClientConfig::new(ClientCapabilities::default(),
   Implementation::new(client_name, client_version))`. The era comes from
   `peer().peer_info()` (`rmcp src/service.rs` line 1024):
   `protocol_version >= "2026-07-28"` (string compare of `YYYY-MM-DD`) is
   `Modern`, else `Legacy`. A `ClientInitializeError` maps to `Unsupported`
   when the server is legacy under `Modern`, to `Timeout` on the deadline,
   else to `Transport`; `LegacyFallbackFailed` keeps the fallback's message.
3. Stdio spawn: first `apb_core::process_mode::guard_program_start()`
   (`crates/apb-core/src/process_mode.rs` line 145), the guard every
   program start in apb passes (`proc.rs` line 291, `adapter/command.rs`
   line 32); an `Err` is `Refused` and nothing starts. Then the program
   is started from its `ProgramFile`:
   1. `StartFrom::Descriptor` (Linux): `Command::new("/proc/self/fd/<n>")`
      of the descriptor the digest was read from, with `arg0` the approved
      path, and a `pre_exec` that clears the descriptor's close-on-exec
      flag in the child only (between fork and exec), so it is inherited
      for exactly this purpose. The file that runs is the file that was
      hashed, whatever happened to the path since (a rename over it, a
      new file, a link swap). A script with an interpreter line: the
      kernel starts the interpreter with `/proc/self/fd/<n>` as the
      script's name, and the interpreter reads the hashed file through the
      inherited descriptor. The script's own name (`$0`, `sys.argv[0]`,
      `__file__`) is then `/proc/self/fd/<n>`, a link that resolves to
      the approved file (or `<path> (deleted)` when it was replaced); a
      script that finds its files next to its own name resolves that link
      first (node resolves it itself; a shell script can `readlink
      /proc/$$/fd/<n>`, as the stub does).
   2. `StartFrom::PathCheckedAgain` (every other platform): the path is
      opened again without following a link and hashed again immediately
      before the spawn; a different digest, or no regular file, is
      `ProgramChanged` and nothing starts; else `Command::new(path)`. The
      window between that second hash and the exec remains on these
      platforms.
   3. On no platform is a rewrite in place of the same file (the same
      inode) between the hash and the exec covered; only a replacement
      is.

   Then `env_clear()`, each `env` pair (`EnvValue::Secret` through
   `Secret::expose_secret`, here and nowhere else), `args`, the working
   directory, `process_group(0)`, `kill_on_drop(true)` (the handle kills
   the child if the worker unwinds), stdin and stdout piped, stderr
   `Stdio::null()` (the specification lets the client ignore it; reading
   it could block, and it may echo secrets). The working directory is a
   fresh, empty, private (0700) directory `apb-mcp-<pid>-<n>-<nanos>`
   under the caller's temporary directory, created new (`create`, never
   an existing path) and removed with everything in it once the server
   was reaped (or after a late reap); never the project root. The server
   is the leader of its own process group, so the close can end what it
   forked (item 6). On Linux the child also gets `PR_SET_PDEATHSIG` =
   SIGKILL before the exec, with a `getppid` check against the parent
   that may already have ended: when the thread that started it ends (a
   caller killed mid-call, an attempt stopped), the server is killed.
   This keeps what the shared group of the first design gave (an
   attempt's stop ends the server) now that the server has its own
   group. Other platforms have no such signal: a killed caller there
   leaves a server that ignores end of input running, and the server's
   own workers are not covered by that signal on any platform (proposal,
   out of scope 15). The descriptor is dropped once the child holds its
   copy. The worker keeps the `Child`
   and hands rmcp `(LineTap::reader(child.stdout.take()),
   LineTap::writer(child.stdin.take()))` as the transport
   (`transport-async-rw`), two thin wrappers in `src/tap.rs` that pass
   every byte through unchanged:
   1. The reader is a bounded line reader. It counts the bytes of the
      current line and, past `MAX_LINE_BYTES` with no newline, fails the
      read with an `io::Error` whose message is `mcp_message_too_large`
      and sets a flag the session reads. rmcp's own cap (16 MiB) drops an
      oversize line and keeps reading (3.9.0 `src/transport/async_rw.rs`
      lines 283 to 290), so without this reader an oversize answer would
      cost 16 MiB of memory and end as a timeout. A failed read ends
      rmcp's transport (`TransportClosed`); the session maps it by its
      flag: to `TooLarge` with the `mcp_message_too_large:` message before
      the call is handed over, to `Lost(Transport)` with the same message
      after it (D3: an answer that could not be read is a lost answer).
      The reader also counts lines in one session and fails the same way
      after 10 000 (`mcp_message_too_large: the server sent more than
      10000 messages`), so a flood of small notifications is bounded too,
      and counts bytes over all lines, failing past `MAX_SESSION_BYTES`
      (`mcp_message_too_large: the server sent more than N bytes`), so
      many lines just under the line cap cannot burn memory and time until
      the deadline (fix round 1, audit m8).
      Why seven times the body cap. The body cap counts the serialized
      body (D3 "Body rules"); a server sends that body once as
      `structuredContent` and, as the specification asks, once more as
      its serialization in a text block. A JSON encoder writes a
      character as itself, as a two-character escape or as one
      `\uXXXX` per UTF-16 unit (ASCII-only output included). The worst
      case per body byte is a two-byte UTF-8 character: 2 bytes in the
      body, 6 as `\u00XX` in `structuredContent`, 7 as `\\u00XX` in the
      text block (the escape of the escape), 13 bytes for 2, a factor of
      6.5; a four-byte character is 26 for 4, a quote or a control
      character less. So 7 times the cap plus the envelope fits every
      answer whose body is at the cap in that shape. What it does not
      cover, on purpose: bytes outside the body, which no factor bounds
      (a server that escapes characters that need no escape, `\u0061` for
      `a`; whitespace inside the text block's JSON, which the parsed body
      drops; text blocks beyond the body's serialization; image, audio and
      resource blocks the body drops and counts in `dropped_content`;
      `_meta`). Such a line can be refused by the line limit with a body
      under the cap; the spec says so and a test pins it. Memory: the tap
      buffers at most one line (`MAX_LINE_BYTES`) and rmcp copies the line
      it reads into its own buffer (3.9.0 `src/transport/async_rw.rs` line
      169), so one call process holds about 15 MiB of line buffers at
      most, below rmcp's own 16 MiB cap of one line (line 58).
   2. The writer holds the bytes rmcp writes until their line is whole,
      and registers before it forwards. rmcp encodes one compact JSON
      message per line, ending it with a newline (3.9.0
      `src/transport/async_rw.rs` lines 613 to 615), and writes it through
      a `FramedWrite` (line 49, `send` at lines 234 to 242), which hands
      the pipe as many bytes as each `poll_write` takes, so one request
      can arrive at the writer in several pieces (the test at line 963
      suspends a flush after part of a line). So `poll_write` copies the
      bytes it is given into the writer's own line buffer and reports
      them written; when that buffer holds a newline, the writer parses
      the completed line and, when it is a `tools/list` request, inserts
      its `id` into a set shared with the reader, and only then writes the
      line to the child's stdin, looping over partial pipe writes. A
      `tools/call` line counts as handed over once the pipe accepted at
      least one byte of it (`calls_handed`, read by `call_tool` to tell
      `NotSent` from `Lost`);
      `poll_flush` and `poll_shutdown` first write every completed line
      still pending. No byte of a request reaches the server before its
      id is in the set, so an answer written at once is always recognized,
      and a request split across writes is registered once, whole.
      Nothing reaches the server's stdin unclassified (fix round 3,
      review Blocker 1): `frame::classify` reads each whole line as one
      JSON object whose members are each named once, with a string
      `method` when it has one, and answers `Call`, `Listing` or
      `Other`; a parse failure is its own answer, never "not a call". A
      line it cannot classify, and a line over `MAX_LINE_BYTES`, is
      refused before any byte of it is forwarded: the writer keeps the
      named reason (`mcp_request_unclassified:`, `mcp_request_too_large:`),
      seals itself, so nothing after it goes out either, and fails the
      write. The call that wrote it then ends through the seal as every
      unanswered call does, so it is `NotSent` with that reason exactly
      when no call byte was handed over. A legal call never reaches
      either limit: the engine's edge refuses, `invalid_args` before any
      secret or server, arguments nested deeper than
      `MAX_ARGUMENT_DEPTH` = 64 (the request line adds two levels, far
      below serde_json's 128) or longer than `MAX_ARGUMENT_BYTES` = 1 MiB
      as JSON (far below the line limit), through
      `apb_mcp_client::check_arguments`, which `Session::call_tool`
      applies again before it queues anything.
   3. The reader passes rmcp only whole lines it read completely, byte
      for byte (fix round 4, review pass 3): it holds every byte until its
      line is whole, reads the line with `frame::incoming`, and only then
      hands the line and its newline on; a line still without its newline
      at the end of the output is never handed on. A whole line whose
      first byte is not `{` (an empty line, a line of spaces, log text a
      server prints on stdout) is dropped: neither parser reads it, it is
      not parsed and not passed on, it counts against the session byte
      limit and against `MAX_DROPPED_LINES` = 1000 (past it the session
      ends with `mcp_message_invalid:`), and the session's note says once,
      as a count only, how many lines were dropped (`dropped_note`, added
      by `SessionHandle::close_noted` to the call's warnings, a failed
      call's error and the healthcheck's warnings). A line that starts
      with the byte order mark bytes is checked before that rule and
      refused, never dropped. Every other line, the ones starting with
      `{`, must pass the strict rule or the session ends. The strict rule: at
      most one carriage return before the newline (which rmcp strips the
      same way, 3.9.0 `src/transport/async_rw.rs` line 386) and no other
      byte around the object, no whitespace included; no byte order mark
      (rmcp strips one, line 464; the tap refuses it rather than strip
      it); valid UTF-8; one JSON object with no member named twice in any
      object at any depth (`StrictValue`, a duplicate-refusing visitor
      over serde_json); `jsonrpc` `"2.0"`; and one frame shape by rmcp's
      rule (`src/model.rs` lines 853 to 910): a request (a string method
      and an integer or string id), a notification (a method, no id), a
      response (a result, no method, no error, a typed id) or an error
      (an error, no method, no result, its id absent, null or typed). A
      line that fails any of these is refused with `mcp_message_invalid:`
      and a fixed reason that never echoes the bytes: nothing of it, or of
      any line after it, reaches rmcp, and the reads end with an error.
      rmcp then fails what is in flight; the session's own rules settle it
      (the seal of item 5, `Lost` once a call byte was handed over,
      `NotSent` before) and the engine closes the session, which kills the
      server's group through the held child handle. So rmcp and the tap
      read the same bytes with the same JSON grammar, and no frame exists
      that one reads and the other does not. Of the frames read, an
      awaited id's raw `result` is kept only from a whole successful
      response (exactly `jsonrpc`, a typed `id` and a `result` object;
      fix round 3, review Major 2); a server request reusing the id is not
      counted; any other response or error frame for a watched id is
      never captured, and a second frame for one id makes the listing
      fail closed (`mcp_listing_ambiguous:`). When rmcp's listing returns,
      every frame up to the one it took was counted, since the reader
      hands rmcp nothing it did not read first.
   A spawn error is `Spawn` with the program's file name and the OS error
   kind, never its arguments.
4. `list_tools` calls `peer.list_tools(Some(PaginatedRequestParams { meta:
   None, cursor }))` page by page, one at a time, so rmcp sends the
   request with the metadata its lifecycle needs. rmcp's typed answer
   checks the raw one: the raw `result`, read as rmcp's own
   `ListToolsResult`, must hold the same `tools` and `nextCursor` as
   the typed answer, else the page fails `mcp_listing_mismatch:` (fix
   round 3). Since fix round 4 this is a cross-check only: the strict
   reader of item 3.3 is the guard that the raw result hashed is the frame
   rmcp accepted, because rmcp reads only lines the tap read whole; the
   tap keys it by the request id it registered from the written line,
   the id rmcp matches the answer by. The tools and the next cursor are
   taken from the raw `result` the line tap stored for that request
   (`result.tools` as `Vec<serde_json::Value>`, `result.nextCursor` as a
   string). A page whose raw result the tap did not see, or whose `tools`
   is not an array, is `Transport` ("the tools/list answer was not read
   off the wire"). rmcp caches list results per cursor (3.9.0
   `src/service/client.rs` lines 1888 to 1935), which cannot answer a
   page here without a request: the session is new, and a repeated cursor
   ends with `TooLarge` ("the server's tool list did not end") before it
   is asked again. Why raw: rmcp's `Tool` drops members it does not name
   ("The rmcp client this design uses"), and the tool digest must cover
   them (D2).
5. `call_tool` calls `call_tool_once(CallToolRequestParams::new(name.to_string())`
   with `arguments: Some(arguments)``)` under
   `tokio::time::timeout(remaining)`. Mapping:
   1. `Ok(CallToolResponse::Complete(r))`: `Complete` with
      `r.content` as JSON, `r.structured_content`, `r.is_error ==
      Some(true)`.
   2. `Ok(InputRequired(_))`: `InputRequired`. `Ok(Task(_))`: `Task`.
   3. `Err(ServiceError::McpError(e))`: `Answered(Rejected { code:
      e.code.0 as i64 })` with `e.message`.
   4. Every other `ServiceError` (`TransportSend(_)`, `TransportClosed`,
      `UnexpectedResponse`, `SubscriptionLagged`, `Cancelled`, `Timeout`,
      `InputRequiredRoundsExceeded`; the enum is rmcp 3.9.0
      `src/service.rs` lines 79 to 97) and the elapsed
      `tokio::time::timeout`: `Lost(..)` (`Timeout` kind for the two
      timeouts, `Transport` for the rest, with the
      `mcp_message_too_large:` message when the line tap's flag is set),
      or `NotSent(..)` when no byte of the request line was handed over
      (`calls_handed` did not grow). A call on a session already closed,
      or whose worker is gone, is `NotSent` too.
      `TransportSend` is lost too: a write that failed may have written
      part of the request. Dropping the timed-out
      future makes rmcp send `notifications/cancelled` (rmcp 3.6.0
      changelog, "cancel an abandoned tools/call").
6. `close`: the worker cancels the rmcp service (its write half drops, so
   the server reads end of input), then polls up to 3 s whether the
   server exited, without reaping it (`waitid(P_PID, pid, WEXITED |
   WNOHANG | WNOWAIT)`: a zombie keeps its pid, and so its group id,
   reserved). Then, the server still unreaped, `killpg(pid, SIGKILL)` on
   its group, which also ends every process the server forked that
   stayed in it; `timeout(2 s, child.wait())` reaps the server; then up
   to 1 s of `killpg(pid, 0)` until it answers `ESRCH`. A returned `wait`
   is `Ended::Reaped { killed, group }`, `killed` when the server had not
   exited by itself, `group` `Empty` when `ESRCH` proved the group empty
   and `NotShownEmpty` otherwise (a reused group id reads as not empty,
   the safe direction). The end state claims only what is proven: the
   server child reaped, and whether its group was seen empty; nothing is
   claimed about a process that left the group (`setsid`, its own
   group). A reap that does not come in time is `Ended::Unconfirmed`, and
   the worker thread, already detached from the caller, keeps waiting on
   the child handle until it is reaped (`reap_late`), so no zombie stays
   behind in the long-lived dashboard or `apb mcp` processes (fix round
   1, audit m9). Every cleanup branch of the worker that nobody waits
   for ends through `Conn::finish` (`close`, then `reap_late`): the end
   of the command inbox and the abandoned connect, where the caller's
   reply timeout passed before the handshake's answer reached it (fix
   round 2, review Minor 5); a failed open and `Command::Close` reap late
   after they answered. The specification's shutdown asks for SIGTERM between
   end of input and the kill; it is left out (a server gets 3 s after end
   of input, which is what SIGTERM would ask of it). The worker thread is
   joined with a 1 s bound after it answered and left detached after
   that.

### The engine side (`crates/apb-engine/src/connector/mcp/`)

`pub mod mcp;` in `crates/apb-engine/src/connector/mod.rs` after `pub mod
intent;` (line 26), with one line in the module doc (lines 1 to 16).
Every path that opened a session and fails before the call closes it at
once; an `Unconfirmed` end there appends "; the MCP server process could
not be confirmed ended" to the error message, and a `Reaped` end whose
group was not seen empty appends "; the MCP server was reaped, but its
process group could not be shown empty: a process it started may still
run and act" (`GROUP_NOT_SHOWN_EMPTY`). No path says the server stopped
unless `close` answered `Reaped`. `with_warning` is the one place that
appends a warning to an error; `with_end` goes through it.
Files: `mod.rs` (`connect`, `McpCall`, `SessionHandle`), `server.rs`
(the `StdioServer` of a pin record, a spec and an account), `map.rs`
(D3: answers and failures to results) and `pin.rs` (D2).

```rust
/// What the call path hands the mcp kind before the send intent exists.
/// It holds no secret: the binding is checked before any is resolved.
pub(crate) struct McpConnectInput<'a> {
    pub doc: &'a ConnectorDoc,
    /// The tree digest the call runs under, compared with the pin's
    /// binding. In a run: the run manifest's `ManifestConnector.digest`
    /// (`manifest.rs` line 264, through `RunExecutionManifest::connector`,
    /// line 740). In the healthcheck: the installed connector's digest
    /// (`LiveTarget`'s, `call/target.rs` line 171 `pins`).
    pub connector_digest: &'a str,
    /// The account digest the call runs under, compared with the pin's
    /// binding. In a run: the granted account's `ManifestAccount.digest`
    /// from the same manifest (`manifest.rs` lines 244 to 258; the
    /// `maccount` the call's `Target` already holds, `call/target.rs`
    /// line 100 and below), never recomputed from the fields and secrets,
    /// which do not carry the secret references and the default flag the
    /// digest covers. In the healthcheck: `config::account_digest` of the
    /// merged account (`LiveTarget::pins`).
    pub account_digest: &'a str,
    pub function: &'a FunctionSpec,
    pub connector: &'a str,
    pub account_name: &'a str,
    pub account_fields: &'a BTreeMap<String, String>,
    pub full: bool,
    pub deadline: Instant,
}

/// The dry-run answer: the pinned program (or the `command` when no
/// record exists), the rendered args, the env names (sorted), the tool
/// and the prepared arguments. Starts nothing and resolves no secret.
pub(crate) fn dry_run(doc: &ConnectorDoc, function: &FunctionSpec, connector: &str, account: &config::Account, args: &Value, stamp: Option<&Stamp<'_>>) -> Value;

/// The checks that need no secret and start nothing, in this order: the
/// config dir rule, `PinStore::load`, the record, `ServerPin::compare`
/// of the tree digest, the account digest and the program file (opened
/// once through `program_file`, its digest read from that descriptor),
/// and the presence of the tool's pin (D2 "Checking pins" item 1). Every `Err` comes before `resolve_secrets`, so no
/// `{{cmd:...}}` helper of the account runs for a call the pins refuse.
pub(crate) fn check_pin(input: &McpConnectInput<'_>) -> Result<CheckedPin, CallError>;

/// The record and the facts `check_pin` held, handed to `connect`: the
/// `ProgramFile` it opened stays in it, its descriptor open across the
/// secret resolution, until `connect` starts it.
pub(crate) struct CheckedPin { /* the record, the ProgramFile, the input's fields */ }

/// The program file at `path` through `apb_mcp_client::ProgramFile::open`
/// with `StartFrom::platform()`; `None` when it is missing, a link or not
/// a regular file. The one way the engine reads a program file: a call,
/// the healthcheck, an approval's listing and its `resolve_program`
/// (it replaces the first design's `program_digest`).
pub(crate) fn program_file(path: &Path) -> Option<ProgramFile>;

/// The longest server-provided text a result, the journal or the
/// terminal keeps (D3 "Mapping").
pub(crate) const SERVER_TEXT_CHARS: usize = 500;
/// The account's redactions over the whole `text`, then the cut to
/// `SERVER_TEXT_CHARS` with a `[cut: N more characters]` marker.
pub(crate) fn server_text(text: &str, redactions: &[Redaction]) -> String;
/// The account's redactions over every string, member name and number of
/// `value`; answers the value and whether anything was replaced. The one
/// walk of the listing check, the listing's display and a tool's answer.
pub(crate) fn redact_tree(value: Value, redactions: &[Redaction]) -> (Value, bool);

/// The start, the listing, the reflected-secret walk and the tool pin
/// check (D2 "Checking pins" item 2), with the secrets resolved after
/// `check_pin`. Every `Err` is a failure before the send intent is
/// recorded. Answers a connected session whose tool is verified, with no
/// argument sent yet.
pub(crate) fn connect(checked: CheckedPin, secrets: &BTreeMap<String, Secret>, redactions: Vec<Redaction>) -> Result<Connected, CallError>;

pub(crate) struct Connected {
    session: SessionHandle,
    tool: String,
    tool_digest: String,
    endpoint: String,
    read_only: bool,
    response_pick: Vec<String>,
    redactions: Vec<Redaction>,
    deadline: Instant,
}

impl Connected {
    /// Puts the prepared arguments in (after the send intent is recorded
    /// and the marker stamped) and gives the call to dispatch. The call
    /// path checked them an object (step 0) and stamping keeps that shape;
    /// `send` hands over only an object, anything else would be
    /// `CallFailure::NotSent` (nothing sent).
    pub(crate) fn with_arguments(self, arguments: Value) -> McpCall;
    /// Closes the session of a call that will not be made (the gate
    /// answered or refused after the connect), with `end_warning`.
    pub(crate) fn abandon(self) -> Option<String>;
}

/// The one open session of a call, shared by the call and the call path
/// so the session outlives `dispatch` and is closed only after the result
/// is journaled and settled (`Arc<Mutex<Option<Session>>>` inside).
#[derive(Clone)]
pub(crate) struct SessionHandle(/* private */);

impl SessionHandle {
    /// Closes the session (`Session::close`) once; later calls answer
    /// `Ended::NotStarted`.
    pub(crate) fn close(&self) -> apb_mcp_client::Ended;
}

pub(crate) struct McpCall { /* Connected's fields and the arguments */ }

impl McpCall {
    /// `mcp+stdio://<connector>/<tool>`.
    pub(crate) fn endpoint(&self) -> String;
    /// The verified pin digest, for `connector_call.tool_digest`.
    pub(crate) fn tool_digest(&self) -> String;
    pub(crate) fn session(&self) -> SessionHandle;
    pub(crate) fn cap_deadline(&mut self, at: Instant);
    /// Sends the call and maps the answer (D3 "Mapping"). Does not close
    /// the session.
    pub(crate) fn send(self) -> Dispatched;
}

/// The warning an end adds: `Some` for `Ended::Unconfirmed` and for a
/// `Reaped` end whose group is `NotShownEmpty`. The one place that turns
/// an end into text, so no path can word an unconfirmed end as a stop.
pub(crate) fn end_warning(ended: &apb_mcp_client::Ended) -> Option<String>;
```

### The server's environment

`server::stdio_server(program, spec, account_fields, secrets, home)`
builds the `StdioServer` from the pin record (D2) and the definition
only. The `program` is the `ProgramFile` `check_pin` opened and hashed
(D2 "Checking pins"), so the file that starts is the file that was
checked. The `args` are each rendered with
`apb_core::connector::template::render_raw`
(`crates/apb-core/src/connector/template.rs` line 227; `RenderCtx` at
line 32 has the fields `account`, `args`, `secrets`) and a `RenderCtx {
account: account_fields, args: &Value::Null, secrets: &BTreeMap::new() }`.
The working directory is not the project root: the client makes a
fresh, empty, private (0700) directory for each start under the
caller's temporary directory and removes it once the server is reaped
(D4 item 3), so a relative path the server opens finds nothing of the
project (fix round 1, audit M4). The server still runs as the person,
unsandboxed: it can read and change what the person can, including
apb's own files and the secrets of other connectors, and the approval
says so once (D5). Running MCP servers under apb's isolation sandbox is future work (proposal, out of scope 18). The environment is built from a
fixed source and nothing of the calling process passes; the client
clears the inherited environment first (`env_clear`, D4 item 3):

| Variable | Value | Source |
|---|---|---|
| `HOME` | the user's home directory | the account database: `getpwuid_r` of the real user id, the lookup `config.rs` already makes for `default_config_dir` (`real_home`, `crates/apb-core/src/config.rs` lines 771 to 795), made public as `apb_core::config::account_entry() -> Option<AccountEntry { name, home }>` (one `getpwuid_r` call that also reads `pw_name`) |
| `USER`, `LOGNAME` | the user's name | the same account entry |
| `XDG_CONFIG_HOME`, `XDG_DATA_HOME`, `XDG_STATE_HOME`, `XDG_CACHE_HOME` | `<home>/.config`, `<home>/.local/share`, `<home>/.local/state`, `<home>/.cache` | the account home and the defaults of the XDG base directory rules, never the caller's values |
| `PATH` | `/usr/local/bin:/usr/bin:/bin`, then `/opt/homebrew/bin` on macOS, then `<program dir>` | fixed (`server::server_path`): the system directories first, so a name the server looks up never resolves first in the program's own directory, which may be one a project can write; then on macOS the Homebrew directory, so a `#!/usr/bin/env node` server finds its interpreter; the pinned program's own directory last, when it is not one of them already, so a sibling helper is still found (fix round 1, audit m4) |
| each name of `mcp.env` | its rendered template (`{{account.*}}`, `{{secret.*}}` through `EnvValue::Secret`) | the approved definition and the approved account; a declared name overrides a row above |
| every other variable of the calling process (`PATH`, `HOME`, `XDG_*`, `LANG`, `TZ`, `TMPDIR`, `HTTP_PROXY`, `HTTPS_PROXY`, `ALL_PROXY`, `NO_PROXY` in either case, `SSL_CERT_FILE`, `SSL_CERT_DIR`, `NODE_EXTRA_CA_CERTS`, `REQUESTS_CA_BUNDLE`, `CURL_CA_BUNDLE`, `APB_*`, an agent's token) | dropped | none |

Proxy and certificate variables reach the server only when the
definition declares them by name in `mcp.env` (D1 rule 3.4.3), with a
value from the definition or an account field. A server that needs a
locale or a temp dir gets one the same way. Where the account database
answers nothing (no entry, or a build that is not unix, where
`real_home` is `None`, `config.rs` line 797), the call is refused before
anything starts: `CallError::new(Config, "mcp: the user's home could not
be read from the account database")`.

What this replaces: the earlier draft built the environment from
`process_env::base_set()` set in the calling process (`sets.rs` lines 11
to 62), which, for a call from an agent's shell, let the caller choose
the proxy, the certificate bundle and the home the server reads (review
record B1).

### The order of one call

`call/mod.rs` `prepare` (from line 434 on main) keeps its first
steps for every kind: the target (`target::resolve`), the attempt
binding, the read-only origin check. A doubled mcp function (eval-doubles'
`of_run` answers it) and a dry-run take main's path unchanged (the gate,
then `build_prepared`, whose step 5b answers the double and whose branch
7a answers `mcp::dry_run`); no server is started on either. A live mcp
call takes this order, one step at a time:

| Step | What | Writes | A crash or kill right after it |
|---|---|---|---|
| 0 | The arguments' shape: for an mcp function the call's `args` must be a JSON object (a `tools/call` takes `arguments` as an object); a scalar, an array or `null` is refused `CallError::new(InvalidArgs, "mcp: the arguments of `{c}.{f}` must be a JSON object, not {kind}")`, whether or not the function declares `args_schema` (`build_prepared` validates only a declared schema, `call/mod.rs` lines 735 to 737, and the CLI accepts any JSON value, `crates/apb-cli/src/connector.rs` line 453). A dry-run gets the same refusal. | nothing | nothing: no intent, no call, no server |
| 1 | The admission decision: `send_once::preview(&gate_input)` (new, `send_once.rs`): the same `ledger::admit` as `decide` (line 187) over one journal read without the lock and without recording, and the `max_calls` budget check. `AlreadySent`, `InFlight`, `RefuseUnknown`, `AttemptClosed` and `Reconcile` are handed to `send_once::gate` as today, which answers or refuses before any server of this call starts (a reconcile runs its read function as apb's own call, as on main); a spent budget refuses. `New`, `Replay` and an untracked call go on. | nothing | nothing to clean: no intent, no call, no server |
| 2 | The arguments' schema check, `require_call_arguments` (an object, nested at most `MAX_ARGUMENT_DEPTH` and at most `MAX_ARGUMENT_BYTES` long, `invalid_args` otherwise; fix round 3), then `mcp::check_pin`: the config dir rule, the pin store, the record, the binding (tree digest, account digest, the program file opened once and hashed from that descriptor, which stays open) and the tool's pin, all before any secret | nothing (nothing started; no secret read) | nothing to clean |
| 2a | `resolve_secrets` (a `{{cmd:...}}` helper may run here, only for a call whose pins passed step 2), then `mcp::connect`: `Session::connect` of the `ProgramFile` step 2 kept (on Linux from its descriptor; elsewhere its path hashed again right before the spawn, `mcp_server_changed` on a difference), the raw listing, the reflected-secret walk and the tool digest compared | nothing (a server process runs) | nothing in the journal; the server is left to "The crash limit" below |
| 3 | `send_once::gate(input, may_record)` records the intent under the journal lock, deciding again on the journal as it is then (it is the authority; step 1 only avoided starting a server for a call that would not go). A live mcp call asks with `may_record` false while its server is not started: a call that would go out (`New` or `Replay`) then records nothing and is answered `SendGate::NeedsStart`. That happens when step 1 handed the call to the gate (no start yet) and the gate would now admit it (the ledger changed between the unlocked read and the lock); the call path then runs steps 2 and 2a and asks the gate again with `may_record` true. The locked checks a recording gate makes (a reminder still due, the `max_calls` budget and the node's other limits through `input.budget`) run before `NeedsStart` is answered, so a budget another call spent in between refuses here, before any secret helper or server of this call starts (fix round 2, review Major 3). So an intent is recorded only after the server started and its pins were checked, on every path (fix round 1, spec review Minor 1). An answer or a refusal now closes the session (`Connected::abandon`) and returns it, with the session's end warning (`Staged::Answer` carries it, spec review Minor 8). | `send_intent_recorded` (synced) | a pending intent of a dead process, read `unknown` (orphan) by `effective_state_with` (`intent/ledger/mod.rs` lines 495 to 515), as for every kind on main; the window is now the time from this write to step 5, a few milliseconds, instead of the whole start, discover and listing |
| 4 | `prepare_body_args` stamps the marker into the arguments (`intent/stamp.rs` line 42); `Connected::with_arguments` | nothing | as step 3 |
| 5 | `dispatch`: `McpCall::send` hands `tools/call` to the transport and maps the answer | the request on the server's stdin | `unknown`, correctly: the tool may have acted |
| 6 | `append_event` journals the `connector_call` and `settle_send` settles the intent (`send_once::finish`, line 461) | `connector_call`, `send_intent_settled` | between the two writes: the call is journaled and the intent reads `unknown` until a person or a reconcile settles it, as for an HTTP call on main |
| 7 | `SessionHandle::close`, then `end_warning` added to the printed warnings. For a call that failed this step comes before step 6 (`close_mcp_into`): the session is closed first and the end's warning is put in front of the error's message, so the settlement's `detail` (cut at 300 characters from the end) carries it; a person's `apb intent list` and `apb intent resolve` read it there. A success still closes after the settlement. | nothing (warnings of a success are never journaled, `add_warnings`, `call/mod.rs` line 205) | nothing: the outcome is already journaled and settled; the server is left to "The crash limit" below |

The crash limit. What ends the server of a call process that is killed
(rows 2a to 7) is not apb's close, since a killed process runs no
`Drop`, no `kill_on_drop` and no `close`. On Linux the server ends with
the thread that started it (`PR_SET_PDEATHSIG` = SIGKILL, D4 item 3),
so a caller killed mid-call or an attempt stopped ends the server; the
server's own workers in its group are not covered by that signal. On
other platforms the server ends only if it exits on end of its stdin,
which it reads once the killed process's pipe descriptors are closed by
the kernel: a killed caller there leaves a server that ignores end of
input running, since the server leads its own group and an attempt's
group stop (`proc.rs` `stop_process_tree`) no longer reaches it. apb
signals no process group but its own server's, and only while that
server is unreaped, so nothing in apb reaches such a server afterwards;
a cleanup of survivors (a scope that owns every descendant) is future
work (proposal, out of scope 15). Nothing in the journal or the ledger
depends on the server ending: rows 2a to 7 say what each kill leaves
there.

A failure in step 0, 1, 2 or 2a journals nothing as a send (no intent, no
`connector_call`, no `max_calls` unit). Step 4 cannot fail: the server
was started and checked before any intent was recorded, and the first
design's "connect after the intent, settle `not_sent` on a start
failure" branch and its "arguments are not an object" branch are gone
(spec review Minor 7). Because a success closes after step 6, the time a
server takes to exit (up to 6 s, plus the 1 s join) never delays its
settlement: a concurrent repeat with `on_unknown: resend` cannot find
the intent past its deadline while the first call's answer is already in
hand. A failure waits for the close first, so its settlement carries
what the close proved; an unknown write stays unknown either way.

`call/mod.rs` changes:

1. `PreparedCall` (lines 307 to 334; eval-doubles adds `Double {
   account, reservation }`) gains `Mcp { account: String, call:
   Box<crate::connector::mcp::McpCall> }`; `cap_timeout` (line 339) calls
   `call.cap_deadline(lowered)`; `account` (line 349), `pre_auth_url`
   (line 362, `call.endpoint()`) and `event_extra` (line 374, `(None,
   None)`) get their arms; two new methods, `fn tool_digest(&self) ->
   Option<String>` (`Some(call.tool_digest())` for `Mcp`) and `fn
   mcp_session(&self) -> Option<SessionHandle>` (`Some(call.session())`
   for `Mcp`), answer `None` for the rest; `dispatch` (line 386) answers
   `call.send()` (after eval-doubles, `dispatch` returns `(Dispatched,
   Option<Reservation>)`: the `Mcp` arm answers `(call.send(), None)`).
2. `prepare`: for a live mcp function that is not doubled and not a
   dry-run, the order above: step 1 before the existing gate call (line
   509), step 2 (`mcp::check_pin`, no secret), step 2a (`resolve_secrets`
   as `build_prepared` does at line 786, then `mcp::connect`), the
   existing gate call as step 3 (with `may_record`, and on `NeedsStart`
   steps 2 and 2a and the gate once more), then step 4 (`mcp_ready`,
   which cannot fail) and the
   `Staged::Ready(Prepared::Call(PreparedCall::Mcp { .. }))` answer.
   `build_prepared` (line 696) keeps a branch `7a` after the inbox
   branch (line 753) and before secret resolution (lines 781 to 787):
   `if function.mcp.is_some()` it answers
   `Prepared::DryRun(mcp::dry_run(..))` for a dry-run, and refuses a live
   call (`mcp_live_call_unsupported`, "Live calls outside a run" below).
   A live mcp call in a run never reaches it (`prepare` takes the order
   above first); a live call outside a run does (the playground and the
   pinned notice path reach `build_prepared` straight from
   `prepare_play_call` and `play_call_pinned`, `call/mod.rs` lines 1146
   and 1089) and is refused there. The order inside
   `build_prepared` on merged main is: the argument schema check,
   eval-doubles' step 5b (a doubled non-inbox function is answered there,
   before any secret), the mock branch (line 741), the inbox branch (line
   753), then 7a, then secret resolution. So a doubled mcp function never
   reaches 7a or `mcp::connect`, an mcp function never reaches secret
   resolution in `build_prepared` (a refused live call runs no
   `{{cmd:...}}` helper, `call/auth.rs` lines 55 to 70), and it never
   reaches the smtp, imap or HTTP branches.
3. `run` (line 223): read `let tool_digest = prepared.tool_digest();` and
   `let mcp_session = prepared.mcp_session();` next to `event_extra`
   (line 240) and carry both in `EventMeta` (lines 91 to 112, new fields
   `tool_digest: Option<String>` and `mcp_session:
   Option<SessionHandle>`); `response_complete` (lines 254 to 262)
   treats `PreparedCall::Mcp` like `Http` (`http_like`), since
   `CallOk::Mcp` reports its completeness (D3).
4. `execute_bounded` (lines 149 to 179): in the `Outcome::Ok` arm,
   after `append_event` and `settle_send` and before `add_warnings`, `if
   let Some(h) = &meta.mcp_session` close it and push `end_warning` into
   the warnings printed; in the `Outcome::Executed` arm (a failed call),
   `close_mcp_into` closes it before `append_event` and puts the warning
   in front of the error's message.
5. `append_event` (line 1209, the construction at line 1211) passes
   `tool_digest: meta.tool_digest.clone()`.

`EventPayload::ConnectorCall` (`crates/apb-engine/src/event.rs` lines 1104
to 1168, the variant at line 1112) gains, after `item` (line 1165, the
last field):

```rust
        /// The digest of the MCP tool definition this call verified against
        /// its pin (`sha256:<hex>`); absent for every other kind. Additive.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        tool_digest: Option<String>,
```

Every full construction of the variant adds `tool_digest: None`. On main
they are six: `connector/call/mod.rs` line 1211 (which takes
`meta.tool_digest` instead), `event.rs` lines 4271 and 4293,
`review/scope.rs` line 1257, `crates/apb-engine/tests/suite/event_test.rs`
line 58 and `crates/apb-server/tests/suite/connectors_api_test.rs` line
927; every other `ConnectorCall {` hit is a pattern ending in `..`.
They are found again on merged main with `rg -n 'ConnectorCall \{' crates`. The
known-events fixture (`crates/apb-engine/tests/fixtures/journal/known_events.jsonl`)
keeps its line unchanged (the field is skipped when absent) and gets no
new line; `journal_compat_test` passes as is.

### Live calls outside a run

Main has three live calls outside a run, all of which reach
`build_prepared` with no run, no journal and no send ledger: the
healthcheck (`call/mod.rs` `healthcheck` line 1001, `prepare_healthcheck`
line 1040, behind the dashboard's `healthcheck_connector_handler`,
`crates/apb-server/src/routes/connectors/mod.rs` line 432), the
playground (`play_call` line 1073 over `prepare_play_call` line 1146,
also the dry-run of `apb connector call` outside a run,
`crates/apb-cli/src/connector.rs` lines 470 to 487) and the pinned
notice call (`play_call_pinned` line 1089, routine health). The live
mcp order of "The order of one call" is run-scoped (an attempt, a
journal, a ledger) and none of them has one. This change:

1. Refuses a live call of an mcp function on every one of them by name,
   in branch 7a of `build_prepared`: `CallError::new(Config,
   "mcp_live_call_unsupported: `{c}.{f}` calls an MCP tool, which this
   apb calls only from a run; outside a run the healthcheck checks the
   pins without a call")`. It is reached after the trust gate of
   `prepare_play_call` (`require_trust`, which reads digests only) and
   after the dry-run and eval-doubles decisions, and before
   `resolve_secrets`: no secret helper runs and nothing starts. The
   healthcheck's MCP branch is taken at the top of `healthcheck`, before
   `prepare_healthcheck`, so it never reaches `build_prepared` either.
   A dry-run stays answered by `mcp::dry_run`. A notice never reaches it
   (`carries_notices` is false for mcp functions, proposal out of scope
   6); the refusal is the backstop. A playground call is future work.
2. Gives the healthcheck of an MCP connector its own small path, the
   pin check without a tool call. `healthcheck` answers
   `guard_refusal()` first (line 143, as `answer_live` does), then, when
   the installed connector's `doc.mcp` is `Some`, returns
   `mcp::pin::healthcheck(root, name, account)` instead of
   `answer_live(|| prepare_healthcheck(..))`:

   ```rust
   /// Starts the pinned server of `connector`/`account`, lists its tools,
   /// compares the whole binding of every mcp function's tool and closes
   /// the session. Makes no `tools/call`, journals nothing, writes
   /// nothing. The same `{ "ok": bool, .. }` shape `healthcheck` returns.
   pub(crate) fn healthcheck(root: &Path, connector: &str, account: &str) -> (Value, bool);
   ```

   Its steps are list 3 of "The four orders" below. The `healthcheck`
   field is optional: manifest rule 5 checks it only when present
   (`def.rs` line 596). An MCP connector that declares none keeps main's
   refusal for an absent one (`connector `{name}` declares no
   healthcheck`, `call/mod.rs` lines 1043 to 1047), answered first,
   before any target, secret or program; one that names a function takes
   that function's `timeout_sec` as the deadline and nothing else from
   it. Success is `{ "ok": true, "mcp": { "era": "<era>",
   "tools_checked": <n> } }`; a refusal is the `CallError::to_json()` of
   the call path, its message through `redact_message`. The session is
   closed on every path that opened it, as on the call path.

### The four orders

Every path that can start anything (a secret helper or the server)
checks first what needs no secret and starts nothing, then resolves the
secrets, then starts the server. A refused live call outside a run (list 4) stops before the secrets. These are the only orders; every other
section of this design describes one of their steps.

1. Approval of an account (`apb connector approve <c> --account <a>`,
   D2 "Capturing pins"):
   1. Refuse inside a run step, and without an interactive terminal
      (`mcp_approve_needs_a_person`, no digest printed).
   2. Load the installed connector (its tree digest) and the merged
      account (its digest); refuse an unapproved tree.
   3. `resolve_program`: resolve the path and hash the file (through
      `program_file`), and
      `preview_start`: render the `args` and `env` overrides with the
      account's non-secret fields, secrets masked by source, and
      `config::cmd_refs`, the command lines of command-sourced secrets,
      extracted and not run. Nothing else is read; no secret, no start.
   4. `print_program` (path, file digest, the interpreter or launcher
      line, the runs-as-you line, rendered args, rendered env overrides,
      the account's non-secret fields, the secret command lines, all
      sanitized) and the first typed confirmation.
   5. `list_live`: reload the connector and the account and compare both
      digests with the confirmed ones, open the program file once and
      compare its digest with the confirmed one, all before any secret
      (`mcp_approve_definition_changed`, `mcp_approve_account_changed`,
      `mcp_server_changed`).
   6. `resolve_secrets`, then `Session::connect` of that same program
      file (its descriptor on Linux, its path hashed again elsewhere)
      under the fixed 30 s, the raw listing within its limits, the
      reflected-secret walk, then the digests, the rows and the prepared
      `pins` map; the session is closed.
   7. `print_pins` and the second typed confirmation.
   8. The account approval, then `pin::record` of the prepared map (the
      store's cap checked before the file is replaced).
2. A call of an mcp function inside a run (D4 "The order of one call"):
   1. The arguments are a JSON object (`invalid_args` otherwise).
   2. The admission decision (`send_once::preview`) and the `max_calls`
      budget.
   3. The argument schema, then `check_pin`: the config dir rule, the pin
      store, the record, `ServerPin::compare` of the run's tree digest and
      account digest (the manifest's `ManifestConnector.digest` and
      `ManifestAccount.digest`), the program file opened once and hashed
      from that descriptor, which stays open, and the tool's pin. Nothing
      has started and no secret is read.
   4. `resolve_secrets`, then `connect`: `Session::connect` of the kept
      program file under the
      function's deadline (plus the cleanup allowance for the reply), the
      raw listing within its limits, the reflected-secret walk, the tool
      digest compared.
   5. `send_once::gate` records the intent; the marker is stamped. When
      step 2 sent the call to the gate unstarted and the gate would now
      admit it, the gate records nothing (`NeedsStart`), steps 3 and 4
      run, and the gate decides again.
   6. `tools/call` is handed over (from here every error of a write is
      `unknown`, except `NotSent`: no byte of the request reached the
      server).
   7. A success: the `connector_call` is journaled and the intent
      settled, then `close` of the session, its warning printed. A
      failure: `close` first, its warning put in front of the error, then
      the `connector_call` and the settlement.
3. The pin-check healthcheck (D4 "Live calls outside a run"):
   1. The read-only guard; a connector without a `healthcheck` function
      is refused as on main.
   2. `LiveTarget::load` and `require_trust` (the tree and the account
      approved).
   3. `check_pin` with the installed tree digest and the merged
      account's digest: the config dir rule, the pin store, the record,
      the binding and the program file, opened once and kept. Nothing has
      started and no secret is read.
   4. `resolve_secrets`, then `Session::connect` of the kept program file
      under the named function's `timeout_sec`, the raw listing within its
      limits, the
      reflected-secret walk, and `ServerPin::compare` of every mcp
      function's tool digest.
   5. `close`, its warning in `warnings`; no `tools/call`, no journal, no
      write.
4. A refused live call of an mcp function outside a run (the
   playground, a pinned notice; D4 "Live calls outside a run"):
   1. The read-only guard (`answer_live`).
   2. `LiveTarget::load`, and `require_trust` unless it is a dry-run
      (digests only).
   3. `build_prepared`: the argument schema, eval-doubles' step 5b (a
      doubled function answered), the mock and inbox branches.
   4. Branch 7a: a dry-run answered by `mcp::dry_run`; a live call
      refused `mcp_live_call_unsupported`. No secret is resolved, no
      helper runs, nothing starts.

### Wrong implementations (D4)

1. Running the client with `Runtime::block_on` on the caller's thread.
   Caught by `stdio_test.rs::a_session_works_inside_a_tokio_runtime` (a
   session opened inside `#[tokio::test]`); mcp-http-and-surfaces adds
   the dashboard test of the same rule.
2. Letting the server inherit anything of the calling process (no
   `env_clear`, the base set, a caller's `HOME`, proxy or certificate
   variable). Caught by
   `connector_mcp.rs::the_server_sees_only_the_fixed_set_and_its_declared_env`
   (with `APB_RUN_DIR`, `APB_CONFIG_DIR`, `SOME_AGENT_TOKEN`,
   `HTTPS_PROXY`, `NODE_EXTRA_CA_CERTS`, `XDG_CONFIG_HOME` and a `HOME`
   other than the account's set in the test process: `env_seen` is
   exactly the fixed names of D4 "The server's environment" plus
   `TRACKER_TOKEN`, `env_value_seen("HOME")` is the account database's
   home, `env_value_seen("PATH")` is `/usr/local/bin:/usr/bin:/bin`,
   then `/opt/homebrew/bin` on macOS, then the stub's directory last) and
   `a_declared_proxy_reaches_the_server` (`HTTPS_PROXY:
   "{{account.proxy}}"` in `mcp.env`: the server sees the account's value,
   not the caller's).
3. Putting a secret into `args` or the dry-run output. Caught by
   `connector_mcp.rs::a_dry_run_starts_nothing_and_names_env_only`.
4. Reading the server's stderr into the result. Caught by
   `stdio_test.rs::stderr_never_reaches_the_answer` (the stub writes the
   token to stderr; the answer and the `Failure` text lack it).
5. Signalling a process the session does not own (rmcp's
   `graceful_shutdown`, a signal by pid, a group signal after the server
   was reaped, when its group id may have been reused), or reporting the
   server ended without a reaped `wait`, or its group empty without
   `ESRCH`. Caught by
   `stdio_test.rs::a_server_that_ignores_end_of_input_is_killed_and_reaped`
   (`set_ignore_eof(true)`: `close` answers `Reaped { killed: true, .. }`
   and `StubServer::is_alive(last_pid)` is false),
   `a_worker_the_server_forked_is_ended_with_its_group` (the stub forks a
   sleeping worker that outlives the server; the close ends it and
   answers `GroupEnd::Empty`), and by `rg -n
   'libc::kill|\.signal\(|graceful_shutdown|process_group|setsid'
   crates/apb-mcp-client crates/apb-engine/src/connector/mcp` finding
   only the expected hits (work plan item 5.1): the group calls of
   `session.rs` `mod group` (`libc::killpg` of the server's own group)
   and the spawn's `process_group(0)`, the test-only `ps` helper
   `StubServer::process_group` in `testing.rs`, and the client tests
   that call it; no `setsid`, no `.signal(`, no `graceful_shutdown`.
   The first design had no `libc` dependency and no own group; fix round
   1 reversed that (D4 item 3 and "Wrong implementations"
   at the end, item 6).
6. Starting one server per run and reusing it across attempts (state
   leaks between attempts and a crashed server blocks the run). Caught by
   `connector_mcp.rs::each_call_starts_its_own_server` (two calls, two
   lines in a `starts.log` the stub appends at start).
7. Starting the server before the admission decision, so a repeat the
   ledger answers still starts it, or recording the intent before the
   server started and listed, so a kill in that phase leaves a pending
   intent that reads unknown (review record M4). Caught by
   `connector_mcp_send.rs::an_already_sent_repeat_starts_no_server`,
   `a_spent_budget_starts_no_server`,
   `a_kill_between_connect_and_call_leaves_no_intent` (a stub whose
   `tools/list` blocks on a FIFO; the call, run again in a child process
   with `APB_MCP_SEND_KILL_CHILD` naming the test, `child_call`, is
   killed through its own handle by the test while it waits: no
   `send_intent_recorded` in the journal, and a repeat goes out),
   `a_kill_once_the_intent_is_recorded_is_never_sent_again_blindly` (the
   child killed as soon as the intent is journaled, before or right
   after the hand-over; the repeat with no marker listed is refused
   `unknown_effect`, nothing sent again),
   `a_kill_after_the_hand_over_is_reconciled_by_its_marker` (the child
   killed while the tool hangs; the marker is listed; the repeat is
   `already_sent`, never sent twice; on Linux the test also checks that
   the server ended with its caller, `PR_SET_PDEATHSIG`) and
   `the_settlement_is_journaled_before_the_close` (a stub with
   `set_ignore_eof(true)`: `send_intent_settled` of a successful call is
   in the journal while the server still runs, before the close bound
   ends, read through a second thread that polls the journal). Drop
   guards keep a failed kill test from leaking: `CallChild` kills and
   reaps the child, `Fifo` releases a blocked stub (fix round 1, test
   audit t1, t2 and t5; the first design's `kill_child_helper`, a
   `#[test]` that passed by returning at once, is gone).
11. Hashing the program by path and starting the path, or running the
    server in the project root. Caught by the program tests of D2 "Wrong
    implementations" item 20 and
    `stdio_test.rs::the_working_directory_is_fresh_private_and_removed`.
12. Leaving the server in the caller's process group, so a worker it
    forked outlives the close with the secrets in its environment and may
    act after the call settled (fix round 1, audit M3). Caught by
    `stdio_test.rs::the_server_leads_its_own_process_group`,
    `a_worker_the_server_forked_is_ended_with_its_group`,
    `connector_mcp.rs::the_server_leads_its_own_process_group` (it
    replaces the first design's `the_server_shares_the_callers_process_group`)
    and `connector_mcp_send.rs::a_worker_the_server_forked_ends_with_the_call`.
13. Timing bounds with no headroom, which flake on a loaded runner.
    The client tests bound an end by the deadline plus
    `REPLY_CLEANUP_ALLOWANCE` plus 5 s (test audit t6).
8. Spawning the server without `guard_program_start`. Caught by
   `stdio_test.rs::a_read_only_process_starts_no_server` (run through
   `apb_core::process_mode::testing::in_read_only_child`, process_mode.rs
   line 227, so read-only mode is entered in a child test process only:
   `connect` is `Refused` and the stub's `starts.log` does not exist).
9. Trusting rmcp's own line cap, so one huge line costs 16 MiB and ends
   as a timeout. Caught by `stdio_test.rs::an_oversize_answer_is_refused_by_name`
   (`StubCall::Oversize(MAX_LINE_BYTES + 1)`: `Lost` with a message
   starting `mcp_message_too_large:`, well before the deadline) and
   `an_oversize_listing_is_too_large` (an oversize `tools.json`:
   `TooLarge` with the same prefix).
10. Letting the discover probe outlast the call's deadline. Caught by
    `stdio_test.rs::the_deadline_bounds_the_discover_probe`
    (`LegacySilent` with a 3 s deadline: `Timeout` in under 5 s).

## D5. Tool exposure, the approve output and the run surfaces

The run side of the surfaces. The listing surfaces (`apb connector tools`,
the doctor rows, `connectors_list`, the dashboard and the contract
expectation kind `mcp`) are mcp-http-and-surfaces.

### The approve output

A new file `crates/apb-cli/src/connector_mcp.rs` (declared `mod
connector_mcp;` in `crates/apb-cli/src/main.rs` next to `mod connector;`)
holds `print_tree(name: &str, digest: &str, spec: &McpServerSpec)`,
`print_program(facts: &ProgramFacts, binding: &str)` and
`print_pins(listing: &ToolListing)`, used by `approve_cmd` (D2
"Capturing pins") before the question, on stderr (the question's
stream). `print_tree` prints `connector <c> digest <full digest>`, then
`mcp: <transport> <command>`, one `arg: <template>` line per `args`
entry, one `env: <NAME>=<template>` line per declared `env` entry, in
name order, every one of them (a declared `PATH`, `HOME`, proxy or
certificate variable decides where the confirmed program looks and
sends; its value is shown as the unrendered template, so `{{secret.*}}`
and `{{account.*}}` placeholders stay as written and no secret or
account value is read), `protocol: <p>`, and the interpreter or
launcher warning of item 3 below. Every string these lines and `print_program` take from the
definition, the account or the file system (connector and function
names, `command`, the resolved path, `args` templates, `env` names and
templates, account names) passes `sanitize_for_terminal` before it is
printed, as server text does; the manifest rule refuses control
characters there in the first place (D1 rule 3.4.5), and the sanitizer
covers a resolved path or an account name the rule does not reach. `print_program` (the first
confirmation, before any secret is resolved) prints items 1 to 3, then
once `warning: this program runs as you, unsandboxed: it can read and
change what you can, including apb's own files and the secrets of other
connectors` (`RUNS_AS_YOU`; fix round 1, audit M4), then from
`preview_start` one `arg: <rendered>` line per `args` entry, one
`env: <NAME>=<rendered>` line per declared `env` override (a secret part
shown as `[secret <field> from env <VAR>]` or `[secret <field> from
cmd]`), one `field: <name>=<value>` line per non-secret field of the
account, and one `secret command: <field>: <command line>` line per
command-sourced secret field (the line `resolve_secrets` will run after
the confirmation, from `config::cmd_refs`, not run here), every value
sanitized for the terminal.
`print_pins` (the second confirmation), in order:

1. `connector <c> account <a> (<era>)` (`print_program` leaves out the
   era, which no server has told yet).
2. `program: <command> -> <absolute path>`, `program digest: <full
   digest> (<same|new|changed>)`.
3. For an interpreter or a launcher (`launcher_name`), one line:
   `warning: <name> is an interpreter or a launcher; this approval covers
   the <name> file only, not the script or package it runs`. The tree,
   the program and the tools confirmations all print it.
4. One line per row: `<function>  <tool>  <status before>  <full
   digest>  <description up to 80 chars>`; for a `changed` row two more
   lines, `pinned: <pinned description>` and `now: <new description>`
   (each at most 300 characters), and `changed members: <names>` (at
   most 300). Then, per row that has them, `title: <title>` (at most 200
   characters) and `input schema: <compact json>` (at most 2000
   characters); a value cut there ends with ` [cut: <n> more
   characters]`, cut on a character boundary after the redaction and the
   sanitizing.
5. One line `annotations (shown, not used): <compact json>` per row that
   has them (at most 500 characters).
6. `unmapped: <names>` when any (at most 1000 characters).
7. A `missing` row: `warning: tool <t> of <c> is not listed by the
   server; its function cannot be called`.
8. The lines of items 4 to 6 carry server text; all of them together
   take at most `SERVER_TEXT_TOTAL` = 20 000 characters, and past it the
   rest is left out with one line `[<n> more lines of server text left
   out: past the screen's limit of 20000 characters]`. The era of item 1
   is a supported constant (D3 "Mapping").
9. When the listing's own server could not be shown ended (`cleanup`),
   one line right above the summary: `warning: a process started for
   this listing may still be running with the secrets of account `<a>`
   (the server could not be shown ended); check for it before you
   confirm` (fix round 2, review Minor 4).
10. Right above the question, again: `program digest: <full digest>
   (<status>)`, `tools: <n> pinned, <n> unpinned, <n> changed, <n>
   missing` and `pin digest: <full digest>`, so no server text can
   scroll them off the screen (fix round 1, audit m3; the test
   `server_text_cannot_scroll_the_digests_away`).

Every text that came from the server (titles, descriptions, input
schemas, annotations, tool names) is already redacted by `list_live` (D2 "Capturing pins",
"Reflected secrets") and passes `sanitize_for_terminal` (`crates/apb-cli/src/util.rs` line
85). After the writes, `approved: <c>/<a>` and the pinned tool names on
stdout. mcp-http-and-surfaces adds `tools_cmd` to the same file and
reuses the row lines.

### The run surfaces

1. `apb runs <id>`, MCP `run_status` and the run page show
   `connector_call` events through the existing connector call views; the
   new `tool_digest` is shown only in the journal itself (MCP `run_events`
   and the raw `events.jsonl`). The call's `url` is the `endpoint` (`mcp+stdio://<c>/<t>`),
   which the existing views print as they print a URL.
2. The outcome (`run_outcome/calls.rs`) reads `classify`, so `tool_error`
   shows as an unknown write with no change there.
3. The start refusal `mcp_tools_unpinned` reaches every launch surface
   through the gate, with its start action (D2).

### Tool exposure (`scheduler/node/exposure.rs`)

No change to `expected` (lines 35 to 58, the function at line 36): an MCP
grant is a connector grant, so the attempt expects the shell tool (the
`handed.connectors` branch) and nothing more. One test pins it,
`crates/apb-engine/tests/suite/tool_exposure_test.rs`
`an_mcp_grant_adds_no_expected_server`: a node granting an MCP connector,
the stub agent reports `mcp_servers: []` and `tools: ["Bash"]`, and no
`tool_exposure_mismatch` is journaled; reporting a server named like the
connector journals one (`unexpected_servers`).

The leftover of #224 (statuses against a real claude init capture) is not
part of this change (proposal, out of scope 14).

### Eval doubles

eval-doubles (in flight, merged before this change) answers connector
calls of an eval run from the case file. Its mechanism, read from its
design and its code in the eval-doubles worktree:

1. A case declares doubles in a top-level `connectors:` block
   (`EvalCase.doubles`, `#[serde(rename = "connectors")]`,
   `crates/apb-core/src/eval.rs`): connector, then function, then a list
   of answers `{ match?: { args?, contains?, site? }, answer: { status,
   body }, times? }` (`answer` is the connector `MockSpec`), checked by
   `checks.connector_calls`. The types live in
   `crates/apb-core/src/eval_doubles.rs` (`Doubles`, `DoubleAnswer`,
   `DoubleMatch`, `CallCheck`, `bound_functions`, `coverage_gaps`); the
   engine side is `crates/apb-engine/src/eval/doubles.rs` (`of_run`,
   `reserve`, `deliver`, the capture file `eval-calls.jsonl`). Its suite
   checks are V280 and V281 (V282 reserved).
2. The function lookup is by connector and function name only; nothing in
   it reads `FunctionKind` or any kind flag, and no kind is refused.
3. In the call path, `prepare` asks `eval::doubles::of_run` first, then
   runs target resolution, the attempt binding and `send_once::gate` as
   for a live call; `build_prepared` then answers every doubled function
   that is not an inbox function at its step 5b (`DoubleSite::prepare`,
   `PreparedCall::Double`), after the argument schema check and before
   any secret is resolved. A `--dry-run` call is never doubled.
4. The run gate's trust checks are unchanged for a doubled run: the
   connector installed and approved, the account configured and
   approved.

So an mcp function is doubled by the existing mechanism with no new
mechanism and no code change in either doubles module: a case names
`tracker-mcp.create_comment` exactly as it names an HTTP function, step
5b answers it before branch 7a of D4 could start a server, and no secret
is resolved. What this means for an author, said once in
`docs/CONNECTORS.md` "MCP servers":

1. The double's `answer` is `{ status, body }` as for any function; the
   agent reads `body` where a live call would return the MCP body shape
   of D3 "Body rules", so a case writes `body` in that shape (the
   structured content, or `{ "text": ... }`). `status` is the double's
   own field and means nothing to the MCP server.
2. The journaled `connector_call` of a doubled call has the url
   `eval-double` and no `tool_digest`: no tool was verified.
3. The run gate's pin presence check of D2 applies to an eval run as it
   does to a live run, since it is a trust check and eval-doubles keeps
   them all: a doubled eval needs its tools approved once, as it needs
   its connector and account approved. A person approves them once
   against the real server.

What this change adds: one test,
`crates/apb-engine/tests/suite/connector_mcp.rs`
`an_mcp_function_is_answered_by_its_double` (an eval run config whose
doubles answer `tracker-mcp.create_comment` of the D1 example, `command`
set to the stub: the call answers the double's body, the capture line
names `create_comment`, the journaled call has no `tool_digest`, and the
stub's `starts.log` does not exist). It lives in this change's own test
file, so no file of eval-doubles changes.

### Wrong implementations (D5)

1. Adding the granted MCP server to the attempt's expected servers.
   Caught by `tool_exposure_test.rs::an_mcp_grant_adds_no_expected_server`
   (reporting a server named like the connector journals
   `tool_exposure_mismatch`).
2. Writing `tool_digest` on calls of other kinds, or a placeholder digest
   on an mcp call. Caught by
   `connector_mcp.rs::the_journal_carries_the_tool_digest` (the field
   equals the pinned digest) and `journal_compat_test` (the HTTP call line
   of the known-events fixture stays unchanged).
3. Starting the server for a doubled call (putting branch 7a before
   eval-doubles' step 5b in `build_prepared`). Caught by
   `connector_mcp.rs::an_mcp_function_is_answered_by_its_double`.
4. Adding a kind check, an `Mcp` arm or an mcp-specific double type to
   eval-doubles. Nothing needs it (item 2 above); the review of the
   final diff checks that `crates/apb-core/src/eval_doubles.rs` and
   `crates/apb-engine/src/eval/doubles.rs` are unchanged.
5. Approving an interpreter without saying so, or a program without
   saying it runs as the person, unsandboxed. Caught by
   `connector_mcp.rs` (CLI)
   `approve_warns_for_an_interpreter_and_says_the_program_runs_as_you`
   (the warning for `node`, and the runs-as-you line exactly once).

## D6. apb mcp on the current revision (the automated part of issue 57)

### What is already true on main

1. rmcp `=3.9.0` (`crates/apb-mcp/Cargo.toml` line 18) speaks 2026-07-28
   and negotiates older revisions at `initialize`.
2. Tier 0 reaches both channels from one `get_info`
   (`crates/apb-mcp/src/server/mod.rs` lines 667 to 676), pinned by
   `tier0_reaches_both_the_initialize_and_discover_channels`
   (`crates/apb-mcp/src/server/tests.rs` lines 2072 to 2098).
3. The cache hints are already per revision: rmcp's server handler calls
   `fill_missing_cache_hints` (`ttlMs: 0`, `cacheScope: private` when the
   handler set none) only for a 2026-07-28 or later peer, and strips
   `resultType` for older peers (rmcp 3.9.0 `src/handler/server.rs` lines
   313 to 319). So the `tools/list` an old host gets carries neither
   field, and a new host gets both. The paragraphs of `docs/MCP.md` lines
   1217 to 1219 that say rmcp does not gate them are out of date.
4. A method of a capability apb does not advertise answers rmcp's default
   handler: `prompts/list` answers an empty list (rmcp 3.9.0
   `src/handler/server.rs` lines 482 to 487, `Ok(ListPromptsResult::default())`;
   apb advertises `tools` only, `enable_tools()`, `server/mod.rs` line
   669, and overrides no prompt method).
5. A 2026-07-28 request without its required `_meta` is refused with
   invalid params, `-32602` (rmcp 3.9.0 `src/service/server.rs` lines 519
   to 527, `missing_request_metadata_error`).

### The change

1. No rmcp change. The earlier draft bumped to 3.11.0 on the belief that
   3.9.0 had no client discover lifecycle; it has one ("The rmcp client
   this design uses"), so `crates/apb-mcp/Cargo.toml` is not touched and
   the new client crate pins the same `=3.9.0`.
2. apb keeps rmcp's hints (`ttlMs: 0`, `cacheScope: private`) on
   2026-07-28 and sets nothing itself: a zero TTL tells a host to ask
   again, which is right for a server whose read-only and supervisor modes
   answer different tool lists (`serve_stdio`, line 691), and it needs no
   `list_tools` override of the `#[tool_handler]` macro. `playbook_catalog`
   keeps its revision-digest `unchanged` answers (a tool result carries no
   cache fields, tools page "Calling Tools").
3. Wire tests, `crates/apb-cli/tests/suite/mcp_revision_test.rs`, over the
   helpers of `mcp_cli_test.rs` (`start_mcp` line 22 and `request` line
   80; add a `start_mcp_raw(dir, env)` that skips the handshake, so a test
   sends its own first message). They pin the wire of the rmcp in use, so
   a later rmcp change that moves it fails here first:
   1. `the_2025_06_18_wire_is_unchanged`: `initialize` with
      `protocolVersion: "2025-06-18"`, `notifications/initialized`,
      `tools/list`, `tools/call playbook_catalog {}`. The fixture
      `crates/apb-cli/tests/fixtures/mcp/legacy_2025_06_18.jsonl` holds,
      per response, only what an old host relies on and what this change
      could move, never apb's whole tool list: the `initialize` result
      (the server version string replaced by `<version>`, `instructions`
      replaced by its sha256), the `tools/list` envelope (its member names,
      no `ttlMs`, `cacheScope` or `resultType`, no `nextCursor`) with the
      sorted tool names and the full definitions of two named tools only,
      `playbook_catalog` and `run_status`, and the `tools/call` envelope
      (its member names, the content block types, no `resultType`, the
      text replaced by `<text>`). A change to any other apb MCP tool, such
      as the two start-rules adds, changes only the sorted name list; the
      test then prints the added and removed names, and the fixture is
      re-recorded with `APB_RECORD_MCP_FIXTURE=1` in the same change that
      adds or removes a tool, never to hide an envelope difference.
   2. `the_2025_11_25_tool_list_has_no_cache_fields`: `initialize` with
      `"2025-11-25"`; the `tools/list` result has no `ttlMs`, no
      `cacheScope` and no `resultType`.
   3. `a_2026_07_28_client_gets_discover_tier0_and_cache_hints`: first
      message `server/discover` with `_meta`
      `{"io.modelcontextprotocol/protocolVersion":"2026-07-28",
      "io.modelcontextprotocol/clientCapabilities":{}}` and no
      `initialize`: `supportedVersions` contains `"2026-07-28"` and
      `instructions` equals `apb_mcp::instructions::TIER0`; then
      `tools/list` with the same `_meta`: `resultType: "complete"`, a
      number `ttlMs` and a string `cacheScope`; then `tools/call
      playbook_catalog {}` with `_meta`: `resultType: "complete"`; then
      `prompts/list` with `_meta`: the empty list of item 4 above. A
      different answer stops the task and is reported, never written into
      the test.
   4. `a_2026_07_28_request_without_meta_is_refused`: `tools/list` as the
      first message without `_meta`: a JSON-RPC error with code `-32602`
      (item 5 above; index page, "_meta": a missing required field is
      malformed).
4. The host matrix and the tier-0 re-measurement of issue 57 (Claude Code
   on 2026-07-28 and with `MCP_PROTOCOL_NEGOTIATION=legacy`, Codex,
   opencode, Hermes, the `supervisor_wait_event` long-poll) are not part
   of this change (proposal, out of scope 14): every row runs a host CLI
   and its model on the owner's login. The wire tests above are the
   automated half of issue 57's regression: they speak both revisions to
   the same binary.
5. Docs: `docs/HOST-INTEGRATION.md` line 18 (which still says "No host
   ships the new revision yet" and names rmcp 3.0.1) is replaced with what
   the wire tests prove per revision (old revisions: `initialize`, tier 0
   in `instructions`, no cache fields; 2026-07-28: `server/discover`, tier
   0 in `instructions`, `ttlMs: 0` and `cacheScope: private` from rmcp),
   one sentence that the per-host matrix and the tier-0 size limit on
   2026-07-28 are not measured yet (issue 57), and the existing measured
   contract kept as the pre-2026-07-28 one. `docs/MCP.md` section
   "Protocol compatibility and adoption plan" (line 1217) is rewritten:
   current position (rmcp 3.9.0, both eras on one binary, hints per
   revision from rmcp, nothing deprecated used), the stale `=3.0.1` pin
   (line 1219) and the cache paragraph (lines 1225 to 1227) corrected, and
   the open item (the host matrix) named with issue 57.
6. Issue 57 stays open: the pull request body says "Refs #57" (written by
   the person who opens it), not "Closes". The implementer drafts, in the
   final report, the comment for issue 57 (what this change proved, what
   is left) and the three follow-up issue texts the issue lists (MCP
   Tasks for runs, multi round-trip for human gates, MCP Apps), and files
   nothing.

### Wrong implementations (D6)

1. Setting `ttl_ms` or `cache_scope` on a `ListToolsResult` for every
   peer. Caught by `the_2025_11_25_tool_list_has_no_cache_fields` and
   `the_2025_06_18_wire_is_unchanged`.
2. A fixture of apb's whole tool list or the catalog's text, which every
   later tool change breaks. Caught by review of the fixture: it holds the
   envelopes, the sorted names and two named definitions only (item 3.1).
3. Overriding `discover` and dropping `instructions`. Caught by
   `a_2026_07_28_client_gets_discover_tier0_and_cache_hints`.

## Docs to update

1. `docs/CONNECTORS.md`: a new section "MCP servers" after "Receiving
   events (webhooks and the inbox)" (line 339) and before "What a call
   returns" (line 499): the `mcp:` block for stdio, `mcp: { tool }`, which
   fields an mcp function takes, the server environment, `protocol`, the
   per-call server start, the body shape and `dropped_content`, pins
   (`apb connector approve --account` as a person's act at an interactive
   terminal only, in two typed confirmations, what the first one shows,
   the program pinned by path and digest,
   the refusals, `mcp-tools.json`, the run-step and redirected config dir
   rules), the server's environment table of D4 and how to declare a
   proxy or a certificate bundle, interpreters and launchers (the
   approval covers that program file only, not the script or package it
   runs; install the server and name its program; V301), the loader
   variables a definition cannot set (`mcp_env_loads_code`), the program
   started from the hashed file on Linux and its path checked again
   elsewhere with the window that remains, a script's `$0` on Linux, the
   private working directory, the `PATH` order, that the server runs as
   the person, unsandboxed, and can read and change what the person can
   (said once), the
   `protocol: auto` timeout rule, the engine sites an MCP function cannot
   use yet (V302), the outcome table of D3 in words, untrusted output, why
   the agent never gets the server (and that `environment: full` loads
   the person's own MCP servers, as on main), the three eval-doubles
   points of D5, both examples of the proposal, what the pin store guards
   against (D2: an unsigned file a same-user process can write, said
   next to "approved at a terminal", and the terminal check of `isatty`
   plus `/dev/tty`), that pins are forgotten when the connector is
   uninstalled or its approval revoked, that an eval case with its own
   config dir calls no server live and doubles its MCP calls instead,
   that a pin binds the connector's tree digest (a changed
   definition needs the account approved again, whatever approved the
   tree), that a server's reflected secret is redacted in the approve
   output and the pin file and a tool name holding one is refused, that
   apb starts the server as the leader of its own process group and ends
   the group at the close, while the server is unreaped, and the crash
   limit of D4 in plain words (on Linux the server ends with its caller;
   elsewhere a killed caller whose server ignores end of input leaves it
   running). "Sending
   once" (line 605) gets one paragraph on MCP modes (no `key`;
   `isError`, `input_required`, a task handle and server errors are
   unknown). The healthcheck paragraph: for an MCP connector it checks
   the pins without a tool call, and the playground refuses an MCP
   function (`mcp_live_call_unsupported`). "The `apb connector` CLI"
   (line 875): `approve` of an MCP connector, with or without
   `--account`, is a person's act at an interactive terminal only (no
   flag, no non-terminal form; provisioning scripts are future work), and `approve --account` also pins
   its program and tools; the dashboard does not approve MCP connectors. mcp-http-and-surfaces adds the http part, the MCP
   subsection of "OAuth accounts" and the `tools` line.
2. `docs/HOWTO-authoring.md` "Connectors (external services)" (line 512):
   one paragraph and a binding example for an MCP connector, V300 to
   V302 (V301 for an interpreter or a launcher), and the install rule
   `mcp_env_loads_code`.
3. `docs/ISOLATION.md`: the V113 row (line 34) and the out-of-scope line
   (line 563) name MCP connectors.
4. `docs/MCP.md` and `docs/HOST-INTEGRATION.md`: D6 item 5.
5. `CLAUDE.md` and `AGENTS.md` (identical shared text), limited to what
   this change adds: the crate list ("Six crates" becomes "Seven crates",
   `apb-mcp-client` described next to `apb-decide`, the direction `core <-
   mcp-client <- engine`); in `apb-core` `connector/mcp/` (schema,
   `digest`, `pins`) and `validate/mcp.rs` (V300 to V302); in
   `apb-engine` `connector/mcp/` (`connect`, `server`, `map`, `pin`), and
   after fix round 1 the client's program file opened once and started
   from its descriptor on Linux (checked again by path elsewhere), its
   own process group killed at the close while the server is unreaped
   (`Ended::Reaped` with `GroupEnd`), the private working directory,
   `CallFailure::NotSent`, `MAX_SESSION_BYTES`, and `MCP_LAUNCHERS` with
   the interpreters and `LOADER_ENV`; in
   the CLI `connector_mcp.rs` (the approve output); a concepts paragraph
   "MCP tool connectors" (called by apb through `apb connector call`,
   never handed to an agent; the program and the tools pinned by a
   person at a terminal; the server's environment from a fixed source;
   pins checked per call before the send intent is recorded, as one
   binding of the tree digest, the account digest, the program and the
   tool digests, after the person confirmed the program and then the
   tools; only failures before the hand-over (a call no byte of which
   reached the server included) settle `not_sent`, and
   `tool_error` (`isError`, `input_required`, a task, a JSON-RPC error
   of any code) and lost answers settle `unknown`;
   no live MCP call outside a run, the healthcheck a pin check without a
   call; one server per call, closed after the settlement of a success
   and before the settlement of a failure;
   stdio only in this version; V113 refuses them on isolated nodes, V302
   at engine sites).
6. This public record,
   `docs/superpowers/specs/2026-11-20-mcp-tool-connectors-design.md`:
   proposal and design as one document.
7. `docs/EVALS.md` or `docs/CONNECTORS.md`: an eval case with its own
   config dir calls no MCP server live (`mcp_config_dir_redirected`) and
   gives its MCP calls a double instead.

## Release notes (for the release that ships this)

1. Connectors can now map the tools of an MCP server over stdio: `mcp:`
   in `connector.yaml`, one server process per call.
2. `apb connector approve <c> --account <a>` pins an MCP connector's
   program (its path and file digest) and tools under the connector's
   tree and account digests; it is a person's act in two confirmations
   (the program with its rendered arguments, environment and account
   fields before any secret is resolved, then the tools), at an
   interactive terminal only. Approving an MCP connector without
   `--account` is a person's act at a terminal too, and neither a script,
   a run step, MCP nor the dashboard approves an MCP connector. A call
   whose definition, program or tool changed is refused until the
   account is approved again. The approval says once that the program
   runs as you, unsandboxed, and warns when the command is an
   interpreter or a launcher, whose approval covers that program file
   only.
8. The program that runs is the file that was hashed (on Linux it starts
   from the hashed descriptor; elsewhere its path is hashed again right
   before the start). The server runs in a fresh private working
   directory, leads its own process group, and the close ends that
   group. A definition cannot set a variable that changes what code the
   server loads (`mcp_env_loads_code`).
3. A run start with unpinned MCP tools is refused `mcp_tools_unpinned`.
4. A new call outcome `tool_error`: an MCP tool reported an error, or the
   server answered with a JSON-RPC server error, and whether it acted is
   unknown.
5. Wire tests pin what `apb mcp` (rmcp 3.9.0, unchanged) serves to
   old-revision and 2026-07-28 hosts. Refs issue 57.
6. An older apb cannot load a connector with an `mcp:` block: it refuses
   to start such a run, and in a run it resumes, each call of an mcp
   function fails before anything starts.
7. The dashboard's healthcheck of an MCP connector checks the program
   and tool pins without calling a tool; its playground does not call
   MCP functions yet.

## Overlap with the in-flight changes

Read from each change's proposal, design and tasks (and eval-doubles'
code). None of them names this change beyond the sequence list, none
reserves a code at V300 or above, and none adds a run guard this change
needs.

| Change | Shared file | Who owns what there |
|---|---|---|
| eval-doubles | `connector/call/mod.rs` | eval-doubles owns `PreparedCall::Double`, `DoubleSite`, step 5b in `build_prepared`, `EventMeta.reservation`, `journal_call`, `deliver_double`, `answer_double`, the tuple `dispatch` returns and `append_event`'s `bool`. This change owns `PreparedCall::Mcp`, the live mcp order in `prepare` (after eval-doubles' `of_run` answered that the function is not doubled), branch 7a (the dry-run answer and the live refusal `mcp_live_call_unsupported`) before secret resolution, the mcp branch at the top of `healthcheck`, `EventMeta.tool_digest` and `EventMeta.mcp_session`, `PreparedCall::tool_digest` and `mcp_session`, the close in `execute_bounded`, and `mod target` made `pub(crate)` |
| eval-doubles | `connector/call/send_once.rs` | eval-doubles' changes to `gate`, if any, are its own; this change adds `preview` beside `gate` and changes nothing in it |
| eval-doubles | `connector/result.rs` | `DoubleUnmatched` is eval-doubles'; `ToolError` follows it |
| eval-doubles | `event.rs` | `RunProvenance.eval_doubles` is eval-doubles'; `ConnectorCall.tool_digest` is this change's |
| eval-doubles | `apb-core/connector/resolve.rs` | `ConnectorFacts.reconcile_functions` and `identity_function` are eval-doubles'; `mcp_transport` and the `untrusted_output_functions` line are this change's |
| eval-doubles | `call/target.rs` | eval-doubles changes `resolve` and `snapshot` signatures; this change only changes the `mod` visibility in `call/mod.rs`, not `target.rs` |
| eval-doubles | `crates/apb-engine/tests/main.rs`, `crates/apb-cli/tests/main.rs`, `docs/HOWTO-authoring.md`, `CLAUDE.md`, `AGENTS.md` | separate lines and paragraphs |
| inbox-waits | `connector/call/mod.rs` | the inbox branch of `build_prepared` and `CallOrigin::Wait`; an mcp connector has no inbox function, so no path meets |
| inbox-waits | `apb-core/connector/resolve.rs` | `names_event_type`, `inbox_ops`, `inbox_args_schemas` are inbox-waits' |
| inbox-waits | `validate/mod.rs`, `event.rs`, `docs/CONNECTORS.md`, `docs/HOWTO-authoring.md`, `CLAUDE.md`, `AGENTS.md`, `crates/apb-engine/tests/main.rs` | separate `mod` lines, calls, variants and paragraphs; inbox-waits' `mod inbox_wait;` and this change's `mod mcp;` are both new lines in `validate/mod.rs` |
| start-rules | `gate/mod.rs` | the approval-binding comparison in `Gate::admit` is start-rules'; the `mcp_tools_unpinned` return in `check_connectors` is this change's |
| start-rules | `docs/CONNECTORS.md`, `docs/MCP.md`, `CLAUDE.md`, `AGENTS.md`, test mains | separate sentences and lines (start-rules adds two MCP tools to `docs/MCP.md`; this change rewrites only the "Protocol compatibility" section) |
| incremental-lists-and-session-fork | `event.rs`, `validate/mod.rs` (a comment), `docs/HOWTO-authoring.md`, `CLAUDE.md`, `AGENTS.md`, `crates/apb-engine/tests/main.rs` | separate variants, fields and lines |

`GuardedFeature::ALL` grows to 14 with eval-doubles and to 15 with
incremental-lists; this change adds no entry.

## Unchanged from main

What this change reuses with other arguments and leaves as it is:

1. The send ledger: `send_once::gate`, `settle_unsent`, `finish`,
   `read_back`, `intent/reconcile.rs`, the admission table and every
   refusal. An mcp send is recorded, refused, reconciled and settled by
   the same code as an HTTP send; only the facts it is fed are new, and
   one read-only function is added beside `gate`: `preview`, the same
   `ledger::admit` without the lock and without recording (D4 "The order
   of one call").
2. The marker: `intent/stamp.rs` `prepare_body_args` (neutralizing,
   appending, the dry-run preview).
3. The call's bounds and journal: `max_calls`, `timeout_sec`, the
   `connector_call` event, `EventLog` locking and syncing, the attempt
   binding (`APB_ATTEMPT_TOKEN`).
4. Trust: the connector's tree digest and the account's digest
   approvals, `acknowledge_untrusted` never bypassing them, the trust scan
   collecting facts before any refusal.
5. Redaction: `result::redact_message`, `response::redact_value` (HTTP
   answers; an mcp answer goes through `mcp::redact_tree`), the account's
   resolved secrets as the redaction set.
6. The process-wide guards: `process_mode::guard_connector_call` (call
   path, unchanged) and `guard_program_start` (called by the client before
   its spawn, as by every other spawn); `fsutil::atomic_write_private` and
   its `guard_write`.
7. Stopping an attempt: the agent's process-group stop in `proc.rs`
   reaches `apb connector call`; since fix round 1 its server leads its
   own group and ends with the call process on Linux
   (`PR_SET_PDEATHSIG`), not through the agent's group.
8. Eval doubles: the whole eval-doubles mechanism, untouched.
9. Tool exposure: `scheduler/node/exposure.rs` `expected`.
10. The run outcome: `run_outcome/calls.rs` reads `classify`.
11. No new event type, no run guard, no manifest mark, no new file under
    `.apb/`, no web change, no generated TypeScript change.

## Files

Every file this change touches, by crate, checked on main at 1.0.0-beta.7. The struct
literal sites (`mcp: None`, `tool_digest: None`) are the ones `rg` finds
there; an in-flight change may add one on merged main, which work plan item 3.1
and 6.1 find with the same `rg`. The ceiling for this change is 80.

Workspace:

1. `Cargo.toml` (member)
2. `Cargo.lock`
3. `THIRD-PARTY-NOTICES.txt`
4. `crates/apb-engine/Cargo.toml`
5. `crates/apb-cli/Cargo.toml` (dev-dependency)
6. `crates/apb-server/Cargo.toml` (dev-dependency; fix round 1 moved it
   out of `[dependencies]`)
7. `scripts/third-party-notices.sh` (fix round 1: one awk pass instead
   of `cat -s`)

apb-mcp-client (new crate):

1. `crates/apb-mcp-client/Cargo.toml`
2. `crates/apb-mcp-client/src/lib.rs`
3. `crates/apb-mcp-client/src/session.rs`
4. `crates/apb-mcp-client/src/stdio.rs`
5. `crates/apb-mcp-client/src/tap.rs` (the bounded line reader and the
   raw `tools/list` capture) and `crates/apb-mcp-client/src/frame.rs`
   (the frame classification and the argument shape, fix round 3)
6. `crates/apb-mcp-client/src/testing.rs`
7. `crates/apb-mcp-client/tests/main.rs`
8. `crates/apb-mcp-client/tests/suite/stdio_test.rs`

apb-core:

1. `crates/apb-core/src/connector/mod.rs`
2. `crates/apb-core/src/connector/def.rs`
3. `crates/apb-core/src/connector/idempotency.rs`
4. `crates/apb-core/src/connector/resolve.rs`
5. `crates/apb-core/src/connector/mcp/mod.rs` (new)
6. `crates/apb-core/src/connector/mcp/digest.rs` (new)
7. `crates/apb-core/src/connector/mcp/pins.rs` (new)
8. `crates/apb-core/src/validate/mod.rs`
9. `crates/apb-core/src/validate/mcp.rs` (new)
10. `crates/apb-core/src/validate/connectors.rs` (V187 test)
11. `crates/apb-core/src/validate/isolation.rs`
12. `crates/apb-core/src/config.rs` (`account_entry`, `real_home` over it)
13. `crates/apb-core/src/connector/install.rs` (fix round 1: `uninstall`
    forgets the connector's pins)
14. `crates/apb-core/src/trust.rs` (fix round 1: `revoke` forgets the
    pins of what it revoked)

apb-engine:

1. `crates/apb-engine/src/connector/mod.rs`
2. `crates/apb-engine/src/connector/prompt.rs`
3. `crates/apb-engine/src/connector/effect.rs`
4. `crates/apb-engine/src/connector/result.rs`
5. `crates/apb-engine/src/connector/call/mod.rs`
6. `crates/apb-engine/src/connector/call/send_once.rs` (`preview`)
7. `crates/apb-engine/src/connector/mcp/mod.rs` (new)
8. `crates/apb-engine/src/connector/mcp/server.rs` (new)
9. `crates/apb-engine/src/connector/mcp/map.rs` (new) and
   `crates/apb-engine/src/connector/mcp/redact.rs` (new in fix round 1:
   the shared redaction walk and the server text bound, a leaf so
   `mod.rs` and `map.rs` do not import each other)
10. `crates/apb-engine/src/connector/mcp/pin.rs` (new)
11. `crates/apb-engine/src/event.rs`
12. `crates/apb-engine/src/review/scope.rs` (ConnectorCall literal: tool_digest: None)
13. `crates/apb-engine/src/grade/rules/consent.rs` (MAY_HAVE_ACTED)
14. `crates/apb-engine/src/gate/trust_scan.rs`
15. `crates/apb-engine/src/gate/mod.rs`
16. `crates/apb-engine/src/next_action/start.rs`
17. `crates/apb-engine/src/fit/trust.rs`
18. `crates/apb-engine/tests/main.rs`
19. `crates/apb-engine/tests/suite/connector_mcp.rs` (new)
20. `crates/apb-engine/tests/suite/connector_mcp_send.rs` (new)
21. `crates/apb-engine/tests/suite/connector_mcp_gate.rs` (new)
22. `crates/apb-engine/tests/suite/tool_exposure_test.rs`
23. `crates/apb-engine/tests/suite/event_test.rs` (tool_digest: None)
24. `crates/apb-engine/tests/suite/grade_rules_test.rs`

apb CLI:

1. `crates/apb-cli/src/main.rs` (mod connector_mcp)
2. `crates/apb-cli/src/connector.rs` (approve_cmd)
3. `crates/apb-cli/src/connector_mcp.rs` (new, print_pins)
4. `crates/apb-cli/tests/main.rs`
5. `crates/apb-cli/tests/suite/mcp_revision_test.rs` (new)
6. `crates/apb-cli/tests/fixtures/mcp/legacy_2025_06_18.jsonl` (new)
7. `crates/apb-cli/tests/suite/connector_mcp_cli_test.rs` (new, the approve tests)
8. `crates/apb-cli/tests/suite/journal_corpus_test.rs` (row M1, the older
   binaries on a run that binds an MCP function)
9. `crates/apb-cli/tests/suite/release_workflow_test.rs` (fix round 1:
   `testing_features_stay_in_test_builds`)

apb-server:

1. `crates/apb-server/src/routes/connectors/view.rs` (ConnectorDoc literal: mcp: None)
2. `crates/apb-server/src/routes/connectors/mod.rs` (the approve route
   refuses an MCP connector)
3. `crates/apb-server/tests/suite/connectors_api_test.rs` (ConnectorCall
   literal: tool_digest: None; the approve refusal and the MCP healthcheck
   and playground tests)

Docs:

1. `docs/CONNECTORS.md`
2. `docs/HOWTO-authoring.md`
3. `docs/ISOLATION.md`
4. `docs/MCP.md`
5. `docs/HOST-INTEGRATION.md`
6. `docs/EVALS.md` (fix round 1: an eval case with its own config dir
   calls no MCP server live)

Records:

1. to 9. The change's nine working files (its proposal, its design, its
   work plan and six capability specs: mcp-connector-kind, mcp-tool-pins,
   mcp-send-once, mcp-stdio-transport, mcp-server-revision and
   mcp-connector-surfaces)
10. `docs/superpowers/specs/2026-11-20-mcp-tool-connectors-design.md`
11. `CLAUDE.md`
12. `AGENTS.md`

Count: 7 + 8 + 14 + 25 + 9 + 3 + 6 + 12 = 84 files (spec files counted
one each). The ceiling for this change was 80; fix round 1 passed it by
four, with seven files its fixes needed (`crates/apb-server/Cargo.toml`,
`scripts/third-party-notices.sh`, `install.rs`, `trust.rs`,
`release_workflow_test.rs`, `connector/mcp/redact.rs` and
`docs/EVALS.md`), recorded in "Fix round 1" below. The third pass added
`crates/apb-server/src/routes/connectors/mod.rs` and
`crates/apb-cli/tests/suite/journal_corpus_test.rs`. The second pass removed
`crates/apb-mcp/Cargo.toml` and the conditional `apb-mcp` file (no rmcp
change) and added `crates/apb-mcp-client/src/tap.rs`,
`crates/apb-core/src/config.rs` and `call/send_once.rs`. The first pass
had removed four files of the draft (the claude init fixture, its test,
the `output.rs` version line, the eval doubles test file of eval-doubles)
and added three (`review/scope.rs`, `grade/rules/consent.rs`,
`grade_rules_test.rs`); `call/target.rs` stays unchanged (its `mod` line
is in `call/mod.rs`).

Read and relied on, not changed (a change to one of them is a difference
to report): `crates/apb-core/src/process_mode.rs`, `crates/apb-core/src/fsutil.rs`,
`crates/apb-core/src/fsutil/beneath.rs`,
`crates/apb-core/src/eval_doubles.rs` and
`crates/apb-engine/src/eval/doubles.rs` (eval-doubles),
`crates/apb-engine/src/connector/call/target.rs`, `call/auth.rs`,
`call/response.rs`, `intent/stamp.rs`,
`intent/reconcile.rs`, `crates/apb-engine/src/proc.rs`,
`crates/apb-engine/src/scheduler/node/exposure.rs`,
`crates/apb-engine/src/run_outcome/calls.rs`,
`crates/apb-engine/tests/fixtures/journal/known_events.jsonl`,
`crates/apb-mcp/src/server/tests.rs`,
`crates/apb-core/src/agents/claude/settings.rs`, `crates/apb-cli/src/consent.rs`,
`crates/apb-cli/src/util.rs`, `crates/apb-core/src/step_env.rs`,
`crates/apb-engine/src/gate/launch.rs`, `crates/apb-mcp/Cargo.toml`.

## Review record

### Closure pass, 2026-11-20

The draft was written on an earlier main, before #287 to #290 merged and while
the eval change was still one change with inbox waits. This pass checked
every code citation against main at 1.0.0-beta.7 (`zg --rg -F` and `rg`, then
the cited line read) and the designs of the four in-flight changes, and
applied the review lessons of the last three changes. What it changed:

1. Base and sequence: main at 1.0.0-beta.7; the in-flight changes by their
   current names (eval-doubles, inbox-waits, start-rules,
   incremental-lists-and-session-fork) instead of
   evals-and-inbox-waits, typed-inbox-and-released-waits and
   start-rules-and-schedules; the precondition is eval-doubles.
2. Line drift fixed (about 40 citations): `def.rs` (+15 lines),
   `gate/mod.rs` (+2), `call/target.rs`, `call/send_once.rs`
   (`settle_unsent` 444, `finish` 461, `read_back` 406), `call/mod.rs`
   (`build_prepared` 696, secrets 782 to 787, `append_event` 1209, OAuth
   path 871 to 876), `event.rs` (`ConnectorCall` 1104 to 1168), the
   apb-core `connector/mod.rs`, `validate/mod.rs` (421, 481), the docs
   (`CONNECTORS.md` 499, 605, 728 to 755, 875; `HOWTO-authoring.md` 512;
   `MCP.md` 1217, 1219, 1225 to 1227), `mcp_cli_test.rs` (22, 80), the
   tier-0 test (2072 to 2098). Wrong statements fixed: `docs/MCP.md`
   lines 1217 to 1219 are a heading and a stale pin, not the cache
   paragraph; `template.rs` is `connector/template.rs`; V73 is reached
   through `shipping_reason`.
3. Validator codes: the full map below V300 from main and the four
   in-flight designs (main's highest is V250, not V222). V300 stays free.
4. Lesson b (complete structures): the tool digest now takes the whole
   tool object minus `UNPINNED_MEMBERS` (`icons`, `_meta`, each with its
   reason) instead of a list of six members to keep.
5. Lesson c (one comparison in one place): stated why the gate's
   presence check and the call's digest check are two facts, not a
   check before and after; the digest is compared only in `build`.
6. Lesson e (stores): the pin store reads through `fsutil::beneath` (no
   link), takes its lock or fails, syncs the directory, and states what
   it guards against and what not; it is not a signed store, with the
   reason.
7. Lesson f (signals): the client no longer uses rmcp's
   `TokioChildProcess` (whose shutdown sends SIGTERM by pid) nor a
   `libc::kill(-pid)` group kill. apb spawns the child, hands rmcp the
   pipes (`transport-async-rw`), and ends it by closing stdin, waiting,
   and killing through the handle; `close` returns `Ended` and only
   `Reaped` counts as proof. The `libc` dependency is gone.
8. Lesson h (narrow): killing helpers a server forks, the manual host
   matrix and tier-0 re-measurement of issue 57, and the claude init
   capture of #224 moved out (proposal, out of scope 14 and 15). Issue 57
   stays open ("Refs #57").
9. Gaps the code showed: the program-start guard
   (`process_mode::guard_program_start`) before the spawn; the consent
   grader's `MAY_HAVE_ACTED` list, which would have read `tool_error` as
   a call that did not act; the sixth `ConnectorCall` literal in
   `review/scope.rs`; `mod target` being private in `call/mod.rs`; the
   `Dispatched.warnings` home for an unconfirmed end.
10. Eval doubles: rewritten from eval-doubles' actual mechanism. Its
    lookup reads no function kind and its step 5b answers before secrets
    and before branch 7a, so an mcp function is doubled with no code
    change in either doubles module; the one test moved into this
    change's own `connector_mcp.rs`. Its suite codes are V280 and V281
    (the draft said V280 to V282).
11. Closed items: the `prompts/list` answer is fixed (`-32601`); the
    isolation broker decision is taken; no question is left for the
    implementer (the owner-gated work items are gone).
12. Overlap with the in-flight changes and "Unchanged from main" are new
    sections; the file list is 74 (ceiling 80).

Not verified in this pass: the rmcp 3.11.0 line numbers (that source is
not in the local cargo registry; work plan item 0.6 checks each item by name
after the first fetch), and the claim that Claude Code negotiates
2026-07-28 by default since 2.1.292 (an outside fact this change no
longer depends on, since the host matrix moved out).

### Second pass, 2026-11-20

The design review of the closure pass (1 blocker, 4 major, 11 minor) was
checked item by item against main at 1.0.0-beta.7 (`zg --rg -F`, then the cited
line read) and against rmcp 3.9.0 in the local cargo registry, and each
disposition below was applied to the proposal, this design, the work plan and
the capability specs.

Blocker:

1. B1, the program and its environment chosen by the caller. Confirmed:
   `base_set()` (`process_env/sets.rs` lines 11 to 62) holds `PATH`,
   `HOME`, `XDG_*`, the proxies, the certificate bundles and
   `APB_CONFIG_DIR`, and the draft resolved a bare `command` in the
   caller's `PATH`. Fixed: the approval resolves `command` to an absolute
   canonical path and records it with the file's sha256 in one pin record
   (program path, program digest, tool digests, pin digest); a call
   starts exactly that path and refuses `mcp_server_changed` when the
   command or the file differs (D2 "What is trusted" item 3, "Checking
   pins" item 1). The environment comes from a fixed source (D4 "The
   server's environment": the account database's home and name, the XDG
   defaults under it, a fixed `PATH` with the program's directory, the
   declared `mcp.env`, nothing of the caller); proxy and certificate
   variables only by name in `mcp.env`. Tests: D2 wrong implementations
   14 and 15, D4 item 2.

Major:

2. M1, the digest over rmcp's typed `Tool`. Confirmed in rmcp 3.9.0
   (`src/model/tool.rs` lines 17 to 40, no catch-all), and a custom
   request does not help (`ServerResult` is untagged with `CustomResult`
   last, `src/model.rs` lines 5009 to 5034). Fixed: the line tap under
   rmcp records the raw `result` of every `tools/list` response by id,
   and the digest is over that raw object, canonicalized, minus
   `UNPINNED_MEMBERS` (D2 "The tool digest", D4 items 3 and 4). End to
   end test: `a_changed_execution_member_refuses_the_next_call`.
3. M2, JSON-RPC server errors settled `not_sent`. Confirmed against
   `effect.rs` lines 47 to 51 (a 5xx write is `WriteUnknown`). Fixed:
   only -32700, -32600, -32601 and -32602 settle `not_sent`; every other
   code is `tool_error` (unknown), as are a transport break after the
   request was written and a timeout (D3 table and its reason). Tests:
   D3 wrong implementation 10.
4. M3, pins written without a person seeing them. Confirmed:
   `approve_live` pinned everything with no confirmation. Fixed: for an
   MCP connector, approving is a person's act: at an interactive terminal
   the program path, its digest and every tool with its digest are shown
   and the typed confirmation of `--waive-probation` is asked
   (`consent::ask_typed`, the connector name typed); without a terminal
   `--expect <pin digest>` is required, and the command stops before it
   starts a server when neither is there; refused in a run step; no MCP
   tool and no HTTP route writes a pin. No refusal text and no start
   action names a command: the code contradicted the draft here, since
   the account arm of `policy_actions` it said it copied builds a
   `Remedy::Manual` with no command (`next_action/start.rs` lines 266 to
   276), and the new arm does the same. Other connector kinds keep main's
   approve; its missing run-step refusal stays a pre-existing note
   (below). Tests: D2 wrong implementations 16, 17 and 19.
5. M4, the crash and timing windows around the pre-dispatch phase and
   the close. Confirmed: the intent was recorded in `send_once::gate`
   (`send_once.rs` line 109) before the server started, and a pending
   intent of a dead process reads unknown (`intent/ledger/mod.rs` lines
   495 to 515); the draft closed the session before the journal write.
   Fixed: the admission decision first (`send_once::preview`, read-only),
   then start, list and the pin checks, then the intent, the dispatch,
   the journal and the settlement, and the close last; a failure before
   the intent journals nothing as a send. Crash table in D4 "The order of
   one call". Tests: D4 wrong implementation 7.

Minor:

6. m1, the rmcp version. The code contradicted the draft: rmcp 3.9.0 has
   `ClientLifecycleMode` (`src/service/client.rs` lines 707 to 722),
   `serve_with_lifecycle` (726 to 738), `call_tool_once` (1475, 2139),
   `CallToolResponse` (`src/model/mrtr.rs` lines 105 to 113) and
   `peer_info` (`src/service.rs` line 1024). Nothing the design needs is
   missing, so the version stays `=3.9.0` for both crates: the bump, its
   API table of a version that is not in the registry, work plan item 0.6, the
   `crates/apb-mcp/Cargo.toml` change and the claim about 3.11.0's
   `prompts/list` answer are gone; every cited line was re-read in 3.9.0
   ("The rmcp client this design uses"). D6 now pins the wire of the rmcp
   in use; on 3.9.0 `prompts/list` answers an empty list
   (`src/handler/server.rs` lines 482 to 487) and a request without
   `_meta` answers `-32602` (`src/service/server.rs` lines 519 to 527).
7. m2, the pin store read from the caller's config dir. Confirmed
   (`config.rs` lines 488 to 500, `sets.rs` line 56). Fixed with the rule
   of `gate::launch::effective_for` (lines 555 to 605): a call from a run
   step whose config dir is redirected is refused
   `mcp_config_dir_redirected`, always, since any redirect can admit a
   forged store; the eval exception of `effective_for` is not known at
   call time and is not taken (D2 "The pin store").
8. m3, a config dir that is a link. Confirmed (`fsutil/beneath.rs` lines
   359 to 364 open the anchor with `O_NOFOLLOW`). Fixed: the config dir
   is canonicalized once at the root, everything beneath stays no-follow;
   test `a_linked_config_dir_is_followed_once`.
9. m4, the discover probe. Confirmed: the probe timeout is a fixed 10 s
   (`src/service/client.rs` lines 722 and 820, no public setter). Fixed:
   the whole connect runs under the call's deadline (D4 item 2), and a
   manifest rule refuses `protocol: auto` with a `timeout_sec` under 15,
   named `mcp_timeout_too_small` (D1 rule 3.4.4; a manifest rule carries
   no V code, as every manifest rule on main).
10. m5, no bound on one message. Partly contradicted: rmcp 3.9.0 already
    caps a line at 16 MiB and drops a longer one (`src/transport/async_rw.rs`
    lines 58 and 283 to 290), so one line does not exhaust memory, but the
    dropped answer only ends as a timeout and a notification flood is not
    bounded. Fixed: the line tap is a bounded line reader at
    `MAX_LINE_BYTES` (`BODY_CAP` plus 64 KiB) and at 10 000 lines a
    session, with the named refusal `mcp_message_too_large` (`TooLarge`
    before hand-over, `Lost` after it).
11. m6, engine sites. Fixed: V302 (error) refuses an MCP connector at the
    baseline check, the follow-up check and a list item check; the notify
    target is already refused by `carries_notices`. Lifting it is future
    work.
12. m7, the function `list_live` passes. Fixed: the first mcp function in
    manifest order; `LiveTarget::load` uses the name only to pick a
    `FunctionSpec` (`call/target.rs` lines 133 to 163), so `target.rs`
    stays unchanged.
13. m8, the wire fixture. Fixed: it holds the envelopes, the sorted tool
    names and the definitions of `playbook_catalog` and `run_status` only,
    with the rule for re-recording it (D6 item 3.1).
14. m9, launchers. Fixed: D2 says plainly that for a launcher the
    approval covers the launcher only; the approve output prints a
    warning line, V301 warns at every binding, and the docs carry it.
15. m10, the scope of the new refusal. Fixed: the run-step refusal and
    the person's act apply only after the manifest is loaded and found to
    carry `mcp:`; other kinds keep main's behaviour (test
    `approve_of_an_http_connector_is_unchanged`).
16. m11, the agent MCP claim. Confirmed: `HERMETIC_FLAGS`
    (`agents/claude/settings.rs` line 61) are appended only in the minimal
    environment (`adapter.rs` line 975); the review's citation of
    `profile.rs` line 548 is a test, the enum is at lines 399 to 403.
    Fixed: the claim and the test are scoped to the minimal environment,
    and the docs say `environment: full` loads the person's own MCP
    servers, as on main.

Pre-existing on main, noted and not changed here (each line read):

17. `apb connector approve` (`crates/apb-cli/src/connector.rs` lines 345
    to 406) and `POST /api/connectors/approve`
    (`crates/apb-server/src/routes/connectors/mod.rs` line 520) are not
    refused inside a run step for connectors of other kinds.
18. `{{cmd:...}}` secret helpers start a bare program through the
    caller's `PATH` (`crates/apb-core/src/connector/secrets.rs` line 250).
    An MCP account may use them too; the account approval covers the
    command line, not the program it finds.
19. An inbox grant suppresses the untrusted-output paragraph for every
    other untrusted function of the node (`crates/apb-engine/src/connector/prompt.rs`
    lines 87 to 88); with this change that includes MCP functions granted
    beside an inbox function.
20. `TrustStore::load` follows links and reads a malformed file as an
    empty store (`crates/apb-core/src/trust.rs` lines 290 to 306).
21. A pending intent of a process that died reads `unknown` for every
    connector kind (`intent/ledger/mod.rs` lines 495 to 515); this change
    only shortens the MCP window to the time between the record and the
    dispatch.
22. A profile with `environment: full` loads the person's own claude MCP
    servers (item 16).

Recommended follow-up issues (not filed by this pass): items 17, 18 and
19, each a small change of its own.

### Third pass, 2026-11-20

The second design review (2 blockers, 4 major, 2 minor; its paths were
under a temporary snapshot of the tree) was checked item by item
against main at 1.0.0-beta.7 (`zg --rg -F`, then the cited line read) and rmcp
3.9.0 in the local cargo registry, and each disposition below was
applied to the proposal, this design, the work plan and the capability specs.

Blockers:

1. An `input_required` write settled `not_sent`. Confirmed: the D3
   table had the row `InputRequired` -> `Failed`, `not_sent`, and the
   multi round-trip pattern
   (https://modelcontextprotocol.io/specification/2026-07-28/basic/patterns/mrtr)
   gives no guarantee that the server did nothing before it asked.
   Fixed: the row is `tool_error`, `WriteUnknown`, `unknown` (every
   answer that does not prove nothing happened), with its reason under
   the table; D3 wrong implementation 8 is reversed; the spec of
   mcp-send-once and the proposal say so; the test is
   `input_required_settles_unknown` (work plan item 7.2).
2. A tree approval made an old pin usable again. Confirmed: on main the
   connector path of `approve_cmd` records the tree digest with no
   question (`crates/apb-cli/src/connector.rs` lines 353 to 366), the
   dashboard route does the same (`crates/apb-server/src/routes/connectors/mod.rs`
   lines 519 to 560), and the pin record bound only `command`, the
   program and the tools, so a changed `mcp.args` or `mcp.env` with the
   tree approved again kept the old pin usable. Fixed: one pin binding.
   `ServerPin.connector_digest` names the approved tree (which covers
   `mcp.command`, `mcp.args`, `mcp.env` and every other field), the pin
   digest covers it with the program path, the program digest and the
   tool digests, and `ServerPin::compare` is the one comparison the
   call, the healthcheck, the gate and the listing use (`command` is
   display only). Every approval path of an MCP connector is a person's
   act: the tree path takes the typed confirmation or `--expect` with the
   tree digest, refuses a run step, and never touches a pin; the
   dashboard route refuses an MCP connector. Tests: an args change with
   the tree approved again leaves the old pin refused at the call
   (`mcp_server_changed`) and at the gate (`mcp_tools_unpinned`), and
   both approval paths refuse without a person.

Major:

3. An older binary could resume an MCP run. Checked, and rejected with
   the proof. An older apb cannot execute an mcp function at all, and
   fails closed before any process starts: `ConnectorDoc` and
   `FunctionSpec` deny unknown fields on main
   (`crates/apb-core/src/connector/def.rs` lines 557 and 366) and in both
   released readers of the journal corpus (`git show
   v0.25.0:crates/apb-core/src/connector/def.rs`, lines 387 and 289;
   `v1.0.0-beta.1`, lines 436 and 291), so the `mcp:` blocks do not
   parse. A call in a run reads the run's snapshot
   `runs/<id>/connectors/<c>.yaml` through `load_snapshot`
   (`crates/apb-engine/src/connector/call/mod.rs` line 1183, the parse at
   line 1191; in v0.25.0 line 1055 and v1.0.0-beta.1 line 984, the same
   parse), which fails `config` ("snapshot is invalid (drift)") before
   the account, the secrets, any request or any program start
   (`call/target.rs` line 100). The manifest does not hold the
   definition (`ManifestConnector` is name, digest and accounts,
   `crates/apb-engine/src/manifest.rs` line 264), so an older binary
   loads the manifest and resumes the run, and every mcp call of it
   fails closed; nothing is silently dropped into a call that runs. The
   prompt's connector section skips a snapshot that does not parse
   (`connector/prompt.rs` lines 24 to 40, the same in v0.25.0), which
   omits the function's text but changes no call. A start by an older
   binary is refused `connector_unresolved` (`store::load` in
   `apb-core/src/connector/resolve.rs` line 164, the refusal in
   `gate/mod.rs` `check_connectors`, line 1303). Which binary makes the
   call does not change this: the agent runs the `apb` its `PATH`
   finds; an older one refuses the snapshot, this one runs every pin
   check itself on every call. So no run guard name is added, the task
   that keeps `guarded.rs` unchanged stays (work plan item 8.4), and the
   `GuardedFeature` counts of the overlap section stand. A journal corpus
   row records the behaviour against the real binaries (row M1,
   work plan item 8.5).
4. The live healthcheck and the playground had no MCP path. Confirmed:
   `healthcheck` and `play_call` reach `build_prepared` straight
   (`call/mod.rs` lines 1040 to 1057 and 1146 to 1181), and so does
   `play_call_pinned` (line 1089), while branch 7a was dry-run only and
   said a live mcp call never reached it. Fixed (D4 "Live calls outside
   a run"): branch 7a refuses every live mcp call outside a run by name
   (`mcp_live_call_unsupported`), and the healthcheck of an MCP
   connector is its own small path, the pin check without a tool call
   (start, list, compare the binding, close). The test that assumed a
   live tool call is removed (below). A playground call is future work.
5. A server could reflect an account secret into the pin file.
   Confirmed: `list_live` resolves the secrets before it starts the
   server, and `ToolPin.description` and `members` were stored as the
   server sent them; `resolve_secrets` answers the redactions
   (`call/auth.rs` lines 25 and 35), and `response::redact_value`
   (`call/response.rs` line 9) keeps member names as they are (line 19).
   Fixed: the digests stay over the raw tool JSON; every displayed or
   persisted text goes through `redact_display` (string leaves and
   member names) before it is printed or recorded; a tool whose name
   holds a secret refuses the listing (`mcp_tool_name_holds_a_secret`).
   Tests for the pin file and the approve output.
6. A crash could leave the server running. Confirmed: a killed process
   runs no `Drop` and no `kill_on_drop`, and a standalone call has no
   attempt stop. Fixed by stating the limit, not by a mechanism (D4 "The
   crash limit", the crash table rows 2 and 7, the stdio spec, proposal
   out of scope 15): the server ends only if it exits on end of its stdin
   or belongs to an attempt's group that an attempt stop ends
   (`proc.rs` line 133); a standalone call killed while its server
   ignores end of input leaves that server running; apb never signals by
   number; a cleanup of survivors is future work.

Minor:

7. The line cap did not cover a 1 MiB body's escaping. Confirmed: at
   `BODY_CAP` plus 64 KiB a body under the cap that the server escapes
   heavily is a lost answer. Fixed: `MAX_LINE_BYTES` is its own limit,
   seven times the body cap plus 64 KiB (7 405 568 bytes), worked out in
   D4 item 3.1 (the worst a JSON encoder writes is 6.5 bytes per body
   byte, the body once as `structuredContent` and once in a text block);
   bytes outside the body (escapes of characters that need none,
   whitespace, extra or dropped blocks, `_meta`) are not covered by any
   factor, and the spec states and a test pins that such a line can be
   refused with a body under the cap. Memory stays at about 15 MiB of
   line buffers per call process.
8. The raw `tools/list` capture had no ordering rule. Confirmed: rmcp
   writes each message through a `FramedWrite` that may hand the pipe
   part of a line (3.9.0 `src/transport/async_rw.rs` lines 234 to 242 and
   the partial-flush test at line 963; the codec ends each message with
   a newline, lines 613 to 615). Fixed: the writer holds a line until
   its newline, registers a `tools/list` id, and only then forwards the
   line; tests cover a split request and an answer written at once.

Pre-existing on main, noted and not changed here (each line read):

23. `StepCaller` reads clearable environment variables and does not
    authenticate a person (`crates/apb-core/src/step_env.rs` lines 16 to
    32); the person's acts of this change rest on a terminal or a typed
    digest besides it.
24. A live dashboard call has no run journal and no send ledger
    (`call/mod.rs` line 1005, `answer_live` at line 1009); this change
    keeps MCP functions out of it.
25. `append_event` drops a journal write failure after a call
    (`call/mod.rs` lines 1205 to 1209); an MCP call journals through it
    as an HTTP call does.
26. `response::redact_value` keeps object member names unredacted
    (`call/response.rs` line 19) for every call answer, MCP bodies
    included; only the listing's display text uses `redact_display`.
27. Item 18 above (`{{cmd:...}}` helpers through `PATH`) was raised
    again; it stands as recorded.

### Cut in the third pass

1. The `not_sent` settlement of an `input_required` write, the test
   `input_required_settles_not_sent`, and D3 wrong implementation 8 as
   it read ("Treating `InputRequired` as unknown").
2. The connector approval of an MCP connector "as today" (no question,
   no `--expect`), the `requires = "account"` of `--expect`, and the
   dashboard's approval of an MCP connector's tree and account.
3. The separate `command` comparison of the call (`the command changed`)
   and `PinCheck` with `PinStore::check_tool`: `ServerPin::compare`
   over the whole binding replaces them; `ServerPin.command` stays for
   display.
4. "Keying by names keeps the pins when a person re-approves an unchanged
   server after an unrelated account edit" as the reason for leaving the
   tree out of the record.
5. The sentence that branch 7a "is never reached by a live mcp call",
   and the test `the_healthcheck_runs_an_mcp_function` (it assumed a
   live tool call outside a run).
6. The crash table's "or is reached by the attempt's group stop" for a
   kill after the server started, and "the server reads end of input"
   as the end of a killed call's server: both promised more than a
   killed process can do.
7. `MAX_LINE_BYTES` as `BODY_CAP` plus 64 KiB "for the JSON-RPC envelope
   and escaping".
8. The writer that only "sees" each request (registration with no rule
   about when bytes are forwarded).

### Fourth pass, 2026-11-20

The third design review (2 blockers, 5 major, 2 minor; its paths were
under a temporary snapshot of the tree) was checked against main at
1.0.0-beta.7 and rmcp 3.9.0. All nine findings were confirmed and accepted
with the review's fix, applied to the proposal, this design, the work plan
and the capability specs.

Blockers:

1. The approval resolved secrets before the program was approved.
   Confirmed: the third pass's account path ran `resolve_program` and
   then `list_live`, which resolves the account's secrets and starts the
   program, before any confirmation or `--expect` check, so a replaced
   binary got the secrets even when the person declined. Fixed: two
   confirmations in order (D2 "Capturing pins" items 4 to 7). First the
   resolved path, the file digest and `program_binding_digest` are shown
   and confirmed (typed, or `--expect-program`); only then are secrets
   resolved and the program started and listed (its file checked again
   right before the start); then the tool pin is confirmed (typed again,
   or `--expect`). A wrong or missing program expectation starts nothing
   and resolves no secret; work plan item 8.1 asserts it through a `{{cmd:...}}`
   helper's marker file.
2. A post-dispatch JSON-RPC error could admit a second write.
   Confirmed: a `Failed` write settles `not_sent`
   (`crates/apb-engine/src/connector/call/send_once.rs` line 479) and a
   `not_sent` tip admits a new send (`intent/ledger/mod.rs` lines 584 to
   586), while the D3 table settled -32700 to -32602 `not_sent`. Fixed:
   for a write every error received after the `tools/call` hand-over is
   `tool_error` and `unknown`, whatever its code; `not_sent` only for
   failures known to precede the hand-over. Task 7.2's `-32602` test is
   now a server that acts and then answers `-32602`.

Major:

3. Pins crossed project accounts of the same name. Confirmed: a
   project account replaces a global one of the same name
   (`crates/apb-core/src/connector/config.rs` lines 77 to 80) and the
   key is machine-wide. Fixed: `ServerPin.account_digest`, in
   `pin_digest`, `program_binding_digest` and `ServerPin::compare`
   (`BindingDiff::Account`), read from `ManifestAccount.digest` in a run
   and from the merged account elsewhere. Tests with two project roots
   (call, gate).
4. Reflected secrets stayed guessable through hashes. Confirmed:
   `Secret` has no minimum length (`crates/apb-core/src/secret.rs` lines
   38 to 45), and the raw tool digest and member digests of a
   description holding a short secret are an offline guessing check.
   Fixed: a listing whose raw tool definitions hold any resolved secret
   value, in any member name or value, is refused
   `mcp_tools_reflect_a_secret` before anything is hashed, shown or
   stored, on the approval, the call and the healthcheck; `redact_display`
   stays as a second line. Tests of work plan items 6.3 and 8.1 revised.
5. Wire and store limits did not compose. Confirmed: `max_pages` and
   `max_tools` had no values and no aggregate bound, and `record` had no
   size check against `load`'s 4 MiB cap. Fixed: 20 pages, 500 tools and
   2 MiB of raw result bytes per listing; `record` refuses a whole store
   over `PIN_STORE_CAP` (`mcp_pin_store_too_large`) before it replaces
   the file; a test that every successful record loads again.
6. Non-object arguments had no outcome. Confirmed: the CLI accepts any
   JSON value (`crates/apb-cli/src/connector.rs` line 453) and schema
   validation runs only for a declared `args_schema` (`call/mod.rs`
   lines 735 to 737). Fixed: step 0 of "The order of one call" refuses a
   scalar, an array or `null` `invalid_args` before connection and before
   the intent; work plan item 6.2.
7. A failed handshake had no reap path. Confirmed: `connect` returned
   `Failure` with no `Session` to close. Fixed: every failure after the
   spawn runs the bounded close and wait inside the worker first, and
   `Failure.ended` says what it proved; test against a server that
   ignores end of input.

Minor:

8. The healthcheck assumed its optional function. Confirmed: rule 5
   checks `healthcheck` only when present (`def.rs` line 596) and main
   refuses an absent one (`call/mod.rs` lines 1043 to 1047). Fixed: an
   MCP connector without one keeps that refusal, a named function gives
   only its `timeout_sec`, and the approval's listing uses the fixed
   `MCP_PIN_CHECK_TIMEOUT` (30 s); test of the absent case.
9. The approve output left out title and input schema. Confirmed. Fixed:
   D5 prints each tool's title (200 characters) and input schema (2000
   characters), redacted, sanitized and cut with a marker; output
   assertion in work plan item 8.1.

Pre-existing on main, raised again and standing as recorded (items 18
to 27 above): `StepCaller` from environment variables, `{{cmd:...}}`
through `PATH`, member names kept by response redaction, the dropped
journal append failure, the inbox grant's untrusted paragraph, and a
full-environment claude profile loading the person's MCP servers.

### Cut in the fourth pass

1. One confirmation on the account path, after the listing, and
   `--expect` alone without a terminal.
2. `not_sent` for -32700, -32600, -32601 and -32602 on a write, the test
   `a_request_error_on_a_write_settles_not_sent` and the paragraph "Why
   only four JSON-RPC codes are `not_sent`".
3. "The account digest is not part of the binding".
4. Hashing a reflecting definition and then redacting what is shown,
   `mcp_tool_name_holds_a_secret` (now part of `mcp_tools_reflect_a_secret`)
   and the tests built on redacted pins.
5. Unvalued `max_pages` and `max_tools`.
6. "for an mcp connector the `healthcheck` field still names a
   function, as manifest rule 5 requires".

### Fifth pass, 2026-11-20

The fourth design review (2 blockers, 4 major, 4 minor; its paths were
under a temporary snapshot of the tree) was checked against main at
1.0.0-beta.7. All ten findings were confirmed and accepted with the
review's fix, applied to the proposal, this design, the work plan and the
capability specs.

Blockers:

1. The account could change after the program confirmation. Confirmed:
   `list_live` took only the program and reloaded the account through
   `LiveTarget::load` (`call/target.rs` line 148, `config::load_merged`).
   Fixed: `list_live` takes a `ConfirmedProgram` (tree digest, account
   digest, path, file digest) and compares the reloaded tree and account
   digests with it before any secret is resolved
   (`mcp_approve_definition_changed`, `mcp_approve_account_changed`);
   tests in work plan item 6.3 (an account edit between the confirmation and the
   listing) and 8.1.
2. The call interface had no account digest. Confirmed:
   `ManifestAccount.digest` (`manifest.rs` lines 244 to 258) covers the
   secret references and the default flag, which the fields and the
   resolved secrets cannot rebuild. Fixed: `McpConnectInput.account_digest`
   from the run manifest's `ManifestAccount.digest` (the `maccount` of
   the call's `Target`), and in the healthcheck from `LiveTarget::pins`;
   each caller is named on the field.

Major:

3. A secret helper started before a pin refusal. Confirmed: the call
   order resolved secrets first, and a `{{cmd:...}}` secret starts a
   program (`call/auth.rs` lines 55 to 70). Fixed: `mcp::check_pin` (the
   config dir rule, the store, the record, the binding of tree, account,
   program path and file digest, the tool's pin) runs before
   `resolve_secrets` on every path; the order table and its crash rows
   rewritten (steps 2 and 2a); test with a marker-writing helper and a
   changed pin.
4. The caller could return before the failed-handshake cleanup ended.
   Confirmed: the reply wait was `remaining + 1 s` while the close takes
   up to 5 s. Fixed: `REPLY_CLEANUP_ALLOWANCE` = 6 s on top of the
   deadline for every reply wait, the worker answering only after its
   close; test of a handshake failing at the deadline.
5. Definition text could control the approval terminal. Confirmed:
   `sanitize_for_terminal` (`crates/apb-cli/src/util.rs` line 85) was
   required for server text only. Fixed: every manifest-, account- or
   file-system-derived string in both confirmations is sanitized, and D1
   rule 3.4.5 refuses control characters in `mcp.command`, `mcp.args`
   and `mcp.env`; escape-sequence test for `mcp.args`.
6. The tree confirmation hid environment values. Fixed: `print_tree`
   prints every declared `env` name with its unrendered, sanitized
   template; placeholders stay unresolved.

Minor:

7. Two deadlines for the approval listing. Fixed: `list_live` always
   uses `MCP_PIN_CHECK_TIMEOUT` (30 s); a function's `timeout_sec` is for
   calls and the healthcheck only.
8. `record` could not store member digests. Fixed: `ToolListing` keeps a
   private `pins` map of prepared `ToolPin` values computed before the
   confirmation, and `record` writes exactly that map.
9. The pin spec's first requirement allowed `--expect` alone. Fixed: it
   names both digests for a non-terminal account approval, with
   scenarios.
10. Stdio reap and group assertions read `/proc` under `#[cfg(unix)]`.
    Fixed: the stub's pid file with `kill -0` on a process the test
    started and `ps -o pgid=`, both POSIX, so the tests run on macOS too.

Consistency: the approval, a call in a run and the pin-check healthcheck
now follow one order each, stated as numbered lists in D4 "The three
orders"; the CLI steps of D2, the call table of D4 and the healthcheck
paragraph were reread against them.

### Cut in the fifth pass

1. `list_live(.., program: (String, String))` and its account reload
   without a comparison.
2. `secrets` and `redactions` in `McpConnectInput`, and the one-step
   `mcp::connect` that checked pins after the secrets were resolved.
3. The reply wait of `remaining + 1 s`.
4. `env: <names>` (names only) in the tree confirmation.
5. The healthcheck function's `timeout_sec` for the approval listing.
6. `/proc` checks in tests marked `#[cfg(unix)]`.

### Sixth pass, 2026-11-20

The fifth design review (2 blockers, 1 major) was checked against main at
1.0.0-beta.7; all three findings were confirmed and accepted with the fix
the coordinator chose. Records of earlier passes keep the names they
used then (`--expect`, `--expect-program`, the program binding digest);
nothing outside the review record names them.

1. A non-terminal approval could supply its own expected digests.
   Confirmed: the tree path printed its digest, the account path the
   program binding and then the pin digest, each before checking the
   argument that names it, so a caller could read the right value from a
   refusal and retry. Fixed by removal: an MCP connector (tree and
   account paths alike) is approved only at an interactive terminal with
   typed confirmations; without a terminal, in a run step, over MCP and
   from the dashboard it is refused `mcp_approve_needs_a_person`, which
   prints no digest. `--expect`, `--expect-program`,
   `program_binding_digest` and their requirements, scenarios, tests and
   docs are gone; approval without a terminal (provisioning scripts) is
   future work (proposal, out of scope 17). No read-only "what would be
   approved" command is added. The success path is tested through
   `approve_mcp` with a scripted `Confirm`; the binary is tested
   refusing, a sequence of retries included.
2. The program confirmation did not show what controls where secrets go.
   Confirmed: `mcp.args` and `mcp.env` render account fields, and main's
   account approval prints its fields only after recording the approval
   (`crates/apb-cli/src/connector.rs` lines 380 to 404). Fixed:
   `preview_start` renders the arguments and every environment override
   from the account's non-secret fields, secret parts masked as
   `[secret <field> from <source>]`, and `print_program` shows them with
   the account's non-secret fields, all sanitized, before the first
   confirmation; the digest recheck before resolution stays. Test: an
   account field feeding `PATH` and `HTTPS_PROXY` is printed before the
   question, and the secret helper has not run.
3. A refused live playground call could start a secret helper.
   Confirmed: `prepare_play_call` reaches `build_prepared`
   (`call/mod.rs` line 1155), which resolves secrets at lines 781 to 787,
   before branch 7a, and a `{{cmd:...}}` secret starts a process
   (`call/auth.rs` lines 55 to 70). Fixed: branch 7a sits after the inbox
   branch and before secret resolution (after the dry-run and double
   decisions), the healthcheck's MCP branch is taken at the top of
   `healthcheck`; "The four orders" adds the order of a refused live
   call; playground test with a marker-writing helper.

Checked after the edits: `rg -- '--expect|expect-program|program_binding|PROGRAM_DIGEST|program binding|mcp_approve_expect_mismatch|mcp_approve_program_mismatch'`
over the proposal, the work plan, the capability specs and this design outside the
review record finds nothing.

### Cut in the sixth pass

1. `--expect` on `apb connector approve` for an MCP connector, on both
   paths, and `--expect-program`.
2. `program_binding_digest` and `PROGRAM_DIGEST_DOMAIN`.
3. The refusals `mcp_approve_expect_mismatch` and
   `mcp_approve_program_mismatch`, and every message that printed a
   digest a caller could supply.
4. The spec requirement and scenarios of a non-terminal approval naming
   both digests, the tree scenario with `--expect`, and the CLI tests
   `approve_with_the_listed_expect_pins`,
   `approve_with_a_wrong_expect_is_refused`,
   `approve_without_the_program_expectation_starts_nothing`,
   `approve_with_a_wrong_program_expectation_starts_nothing`,
   `approve_prints_the_program_binding_before_anything_starts`,
   `a_stale_program_expectation_after_an_account_edit_is_refused`,
   `approve_tree_with_a_wrong_expect_is_refused`,
   `approve_tree_with_the_tree_digest_approves_and_pins_nothing` and
   `expect_on_an_http_connector_is_refused`.
5. Branch 7a after secret resolution.

### Seventh pass, 2026-11-20

The confirmation review (1 blocker, 1 major, 2 minor) was applied as
given.

1. Blocker, a contradiction in the work plan: work plan item 6.2 placed branch 7a
   after secret resolution, against D4 (`build_prepared` resolves
   secrets at `call/mod.rs` lines 781 to 787, and a command-sourced
   secret starts a helper, `call/auth.rs` line 55). Fixed: work plan item 6.2 says
   before secret resolution, after step 5b and the mock and inbox
   branches.
2. Major: the first confirmation showed a command-sourced secret only as
   `[secret <field> from cmd]`. Fixed: `preview_start` carries
   `config::cmd_refs` (`crates/apb-core/src/connector/config.rs` line
   259, which extracts the reference text without executing it), and
   `print_program` prints `secret command: <field>: <command line>`,
   sanitized, before the question; test
   `the_program_confirmation_shows_each_secret_command` (visible, the
   helper not run), spec scenario "A command-sourced secret".
3. Minor: `mcp_server_changed` and `mcp_tool_changed` carried pinned and
   current 16-character digest prefixes. Fixed: refusals name what
   differs (the program file, the tree, the account, a tool by name) and
   carry no digest or prefix; full digests appear only in the terminal
   confirmations; spec requirement "Refusals carry no digest" and test
   `no_refusal_carries_a_digest`.
4. Minor: the pin store text still allowed approval by naming the pin
   digest. Deleted; it now says two typed confirmations at a terminal.

The work plan was reread end to end against "The four orders": work plan item 6.2 also
listed `connect` with "the program and record check" first; it now
lists `check_pin` before `resolve_secrets` and `connect` after it; task
6.1's type list gained `check_pin`, `CheckedPin`, `preview_start` and
`ConfirmedProgram`; and the unpinned healthcheck test in 6.2 had lost its
assertion to the next test, now restored with "no secret helper run".
No other step is out of order.

### Cut in the seventh pass

1. The digest prefixes `(pinned {p16}, now {d16})` in `mcp_server_changed`
   and `mcp_tool_changed`.
2. "or named the exact pin digest" in the pin store text.
3. "after secret resolution" for branch 7a in work plan item 6.2.

### Fix round 1, 2026-11-20

Two reviews of the implementation at the end of the seventh pass: a
spec conformance review (2 major, 14 minor) and a security and test
audit (8 major, 9 minor, 6 test findings). Every finding was decided by
the owner and has a disposition below: fixed, with where, or accepted,
with the reason. The fixes are numbered C1 to C9 in the round's change
list; each section of this design above states the behaviour as it is
now. The branch was rebased onto main after "Eval doubles" (#291)
with no conflicts before the round.

The changes:

1. C1, the program that runs is the file that was hashed:
   `apb_mcp_client::ProgramFile` opens the program once (`O_NOFOLLOW |
   O_NONBLOCK`, a regular file only), reads its digest from that
   descriptor and keeps it; Linux starts `/proc/self/fd/<n>` of it,
   other platforms hash the path again right before the spawn. One
   engine function, `mcp::program_file`, for the call, the healthcheck,
   the approval's listing and `resolve_program` (D2 "What is trusted",
   D4 item 3).
2. C2, interpreters and loader variables: `MCP_LAUNCHERS` names 30
   launchers and interpreters, V301 and the approval say "an interpreter
   or a launcher", and the manifest rule `mcp_env_loads_code` refuses
   `LOADER_ENV` and the `LD_` and `DYLD_` names (D1 rules 3.4.5 and
   3.4.6, V301).
3. C3, the server and what it forks: the server leads its own process
   group; the close kills the group while the server is unreaped, reaps
   it and looks for the group empty (`Ended::Reaped { killed, group }`);
   a late reap keeps waiting in the worker; `PR_SET_PDEATHSIG` on Linux;
   a failed call closes before its settlement, with the warning in front
   of the error (D4 items 3 and 6, "The order of one call").
4. C4, what the server can reach: a fresh private working directory,
   the program's directory last in `PATH`, `/opt/homebrew/bin` on macOS,
   and one line on the approval that the program runs as the person,
   unsandboxed (D4 "The server's environment", D5).
5. C5, redaction and bounds: `server_text` redacts the whole text, then
   cuts to 500 characters, for every server text; the `isError` text is
   never parsed; `redact_tree` redacts strings, member names and numbers
   for the listing check, its display and a tool's answer; the approval
   screen bounds every server field and their total and repeats the
   digests and counts above the question (D3 "Mapping", D2 "Reflected
   secrets", D5).
6. C6, the `testing` feature: `apb-server` takes the client with
   `testing` under `[dev-dependencies]` only, the client carries the
   `compile_error!` guard, and `release_workflow_test::testing_features_stay_in_test_builds`
   checks every crate (D0).
7. C7, the call order on every path: `send_once::gate(input,
   may_record)` and `SendGate::NeedsStart`, so an intent is recorded only
   after the server started and its pins were checked; `mcp_ready`
   cannot fail; `with_warning` is the one place that appends a warning
   (D4 "The order of one call").
8. C8, not sent after the record: `CallFailure::NotSent` from the line
   tap's `calls_handed`, mapped to `service` and settled `not_sent`; the
   crash tests rebuilt without `kill_child_helper`; the session byte cap
   `MAX_SESSION_BYTES` and parsing only while a listing is awaited (D3
   "When a call counts as handed over", D4 items 3 and 5).
9. C9, the remaining minors: the pin store load error in its own field
   with its own remedy and fit row; `program_status` and `compare` for
   the display comparisons; `PinStore::forget` on uninstall and revoke;
   an eval case with its own config dir calls no server live; the end
   warning of an answer from the ledger printed; the `UNPINNED_MEMBERS`
   comment; the notices script (D2, D5, below).

Spec conformance review:

1. Major 1, the test seam in the release binary. Fixed (C6): the
   dependency moved to `[dev-dependencies]` of `crates/apb-server/Cargo.toml`,
   the guard added to `crates/apb-mcp-client/src/lib.rs`, and
   `release_workflow_test::testing_features_stay_in_test_builds` added.
2. Major 2, a secret prefix in a tool error. Fixed (C5): `server_text`
   in `crates/apb-engine/src/connector/mcp/mod.rs` redacts the whole text
   before the cut; test
   `map::tests::a_secret_across_the_cut_is_redacted_whole_before_the_cut`
   (the secret at character 495).
3. Minor 1, a race path starts the server after the intent. Fixed (C7):
   the gate records nothing for a live mcp call whose server is not
   started and answers `NeedsStart` (`call/send_once.rs`,
   `call/mod.rs`); unit test
   `send_once::tests::a_gate_that_may_not_record_records_nothing`. A
   deterministic end-to-end test of the race would need a hook between
   the unlocked preview and the gate, a test-only seam in production
   code, so the rule is tested at the gate.
4. Minor 2, the pin store load error as a nonsense start action. Fixed
   (C9): `ConnectorScan.mcp_pin_store_unreadable`, the refusal's
   `tools: []` and `store_unreadable`, its own manual remedy in
   `next_action/start.rs` and one fit row in `fit/trust.rs`.
5. Minor 3, comparisons outside `ServerPin::compare`. Fixed (C9):
   `pins::program_status`, one display helper used by the listing and
   the CLI; the CLI's stale-tree check goes through `compare` with
   `ProgramFact::NotRead`.
6. Minor 4, the missing test `a_changed_command_is_refused`. Fixed (C8):
   the test exists in `connector_mcp.rs` (another program named and the
   tree approved again: `mcp_server_changed`, neither program started).
7. Minor 5, an unbounded JSON-RPC error message. Fixed (C5):
   `server_text` on it; test `server_text_has_a_bound_everywhere`.
8. Minor 6, the `isError` text lost when it is JSON. Fixed (C5): the
   text blocks are joined as they are and never parsed; test
   `an_error_text_that_reads_as_json_is_kept_as_text`.
9. Minor 7, a dead branch. Fixed (C7): the branch is gone, `send` hands
   over only an object (anything else would be `NotSent`), and
   `with_warning` serves `abandon` and `with_end`.
10. Minor 8, a dropped end warning. Fixed (C9): `Staged::Answer` carries
    the warnings of a session the call opened, printed with the answer.
11. Minor 9, manifest text unsanitized in install errors. Fixed (C2):
    the control-character check of `command` runs first and a bad tool
    name is printed escaped; test
    `install_errors_never_print_a_control_character`.
12. Minor 10, a rustfmt artifact in the `UNPINNED_MEMBERS` comment.
    Fixed (C9) in `crates/apb-core/src/connector/mcp/digest.rs`.
13. Minor 11, unrelated churn in the notices. Fixed (C9): the cause was
    `cat -s` in `scripts/third-party-notices.sh`, which some `cat`
    implementations (uutils coreutils 0.10, the default on the machine
    the file was made on) apply differently where a pipe read splits, so
    the output changed between runs and machines; main's file was made
    that way too. One awk pass (`LC_ALL=C`) strips trailing whitespace
    and squeezes blank-line runs. The regenerated file differs from main
    only by the tokio-stream row, the count and one restored blank line.
14. Minor 12, statements in the implementers' report:
    1. The `rg` check of work plan item 5.1 had an unexpected hit. Fixed: the own
       process group of C3 makes hits expected, and work plan item 5.1 now names
       them: the group calls in `session.rs` `mod group` (`killpg` of the
       server's own group) and the spawn's `process_group(0)`, the
       test-only `ps` helper `StubServer::process_group` in
       `testing.rs`, and the client tests that call it. The check now
       looks for anything else: a `setsid`, a `.signal(`, a
       `graceful_shutdown`, a `libc::kill` of a pid.
    2. The apb-server dependency was not a dev-dependency. Fixed (C6).
    3. The commits against the "do not commit" line. The commits were
       made on a local branch the owner authorized, never pushed and with
       no pull request; the work plan's working-rules line is corrected to say
       so.
    4. Gate 11.3 was not rerun whole. Accepted for the earlier round; in
       this round gate 11.3 is rerun whole.
15. Minor 13, the implementers' notes. No change needed: the review
    judged each one acceptable. Note 13.6, the manifest re-read on every
    call, stays: it is one small file read, recorded here.
16. Minor 14, a failure nothing reached filed as `Answered`. Fixed (C8):
    `CallFailure::NotSent`, mapped to a failure before the hand-over
    (`not_sent`); tests in "When a call counts as handed over".

Security and test audit:

1. M1, the check-to-exec race of the program file. Fixed (C1):
   `ProgramFile` in `crates/apb-mcp-client/src/stdio.rs`, the start in
   `session.rs` `spawn`, `mcp::program_file` in the engine; tests
   `stdio_test::a_program_replaced_after_the_hash_never_runs_from_the_descriptor`,
   `a_program_replaced_after_the_hash_is_refused_on_the_path_checked_again`,
   `an_unchanged_program_starts_on_the_path_checked_again`,
   `a_program_that_is_missing_or_a_link_is_refused_before_any_start`,
   `connector_mcp::a_program_swapped_while_the_secrets_resolve_never_runs_with_them`.
   What remains is stated: the window between the second hash and the
   exec on platforms without the descriptor start, and a rewrite in
   place of the same file on every platform.
2. M2, interpreters pin only the interpreter; loader variables. Fixed
   (C2): `MCP_LAUNCHERS` with the interpreters, V301 and the approval
   line, `mcp_env_loads_code` over `LOADER_ENV`; test
   `the_loader_list_is_refused_in_mcp_env_and_nothing_else_is`. The
   audit's optional digest of every file an argument names was not
   added; the warning says what the approval covers.
3. M3, descendants outlive the close. Fixed (C3): own process group,
   `killpg` while unreaped, `GroupEnd`, the warning in front of a failed
   call's error; tests `the_server_leads_its_own_process_group` (client
   and engine), `a_worker_the_server_forked_is_ended_with_its_group`,
   `connector_mcp_send::a_worker_the_server_forked_ends_with_the_call`.
4. M4, the server reaches beyond the approved account. Fixed in part
   (C4): a fresh private working directory, never the project root, and
   the approval's line that the program runs as the person, unsandboxed,
   and can read and change what the person can, including apb's own
   files and the secrets of other connectors (also in
   `docs/CONNECTORS.md`). `HOME` and the XDG directories stay the
   fixed-source home of D4, unchanged. Running MCP
   servers under apb's isolation sandbox is future work (proposal).
5. M5, a tool error cut before its redaction. Fixed (C5), as spec review
   Major 2.
6. M6, answer keys and numbers not redacted. Fixed (C5): `redact_tree`
   for mcp answers; tests `a_secret_as_an_answer_key_or_number_is_redacted`
   and `the_walk_redacts_strings_member_names_and_numbers`.
7. M7, the stub ships in the release binary. Fixed (C6), as spec review
   Major 1.
8. M8, the "intent recorded, then the start fails" settlement untested.
   Fixed (C7, C8): after C7 that path no longer exists (the server is
   started before any intent is recorded); the one remaining failure
   after the record that nothing reached is `NotSent`, tested end to end
   by `connector_mcp_send::a_server_gone_before_the_call_settles_not_sent`
   (one `not_sent` settlement, the repeat goes out) and the client and
   tap tests of C8.
9. m1, the pin store is an unsigned file of the user. Accepted, the
   owner's decision: a same-user process can write it, as `trust.json`;
   `docs/CONNECTORS.md` says so next to "approved at a terminal", so
   nobody reads the approval as protection against a run step of the
   same user.
10. m2, the terminal check is `isatty` plus `/dev/tty`. Accepted, the
    owner's decision: an agent with a pseudo-terminal that clears
    `APB_RUN_ID` passes it, as it passes every typed confirmation; stated
    in `docs/CONNECTORS.md`.
11. m3, approval lines a server controls are unbounded. Fixed (C5): each
    field bounded, all server text at most 20 000 characters, the digests
    and counts repeated above the question; test
    `server_text_cannot_scroll_the_digests_away`.
12. m4, the program's directory first in `PATH`; no Homebrew directory.
    Fixed (C4): `server::server_path`; test
    `the_path_puts_the_programs_own_directory_last`.
13. m5, pins are never forgotten. Fixed (C9): `forget` on uninstall and
    on revoke of a connector or a connector account; tests
    `a_revoked_approval_takes_its_pins_with_it`,
    `forgetting_nothing_writes_nothing`,
    `uninstall_forgets_the_connectors_mcp_pins`. An account has no
    removal command; its pin binds a digest nothing matches.
14. m6, server text unbounded in results and the journal. Fixed (C5):
    `server_text` on every server-provided message.
15. m7, an eval case with its own config dir refuses every live MCP
    call. Accepted as intended and documented (C9): such a case gives
    its MCP calls a double; test
    `an_eval_case_with_its_own_config_dir_calls_no_server_live`.
16. m8, the line limits allow a large amplification. Fixed (C8):
    `MAX_SESSION_BYTES` and parsing only while a listing answer is
    awaited; test `tap::tests::the_session_byte_cap_ends_the_reads`.
17. m9, an unconfirmed kill leaves a zombie. Fixed (C3): `reap_late` in
    the worker.
18. t1, `kill_child_helper` passes as a test. Fixed (C8): it is gone;
    every kill test runs itself again in a child process
    (`APB_MCP_SEND_KILL_CHILD`, `child_call`).
19. t2, a failed kill test leaves its stub running. Fixed (C8): the drop
    guards `CallChild` and `Fifo`.
20. t3, a test enshrines the shared group. Fixed (C3): replaced by
    `connector_mcp::the_server_leads_its_own_process_group`.
21. t4, refusals with no negative test. Fixed (C1, C5): a program turned
    into a link (`a_program_turned_into_a_link_is_refused`), the secret
    across the cut, the secret as a key or number, the swap during the
    secret resolution. The listing's program change after the first
    confirmation is covered by construction: `list_live` opens the file
    once, compares its digest with the confirmed one and starts that
    descriptor.
22. t5, crash cases only partly covered. Fixed (C8):
    `a_kill_once_the_intent_is_recorded_is_never_sent_again_blindly` and
    `a_kill_after_the_hand_over_is_reconciled_by_its_marker`.
23. t6, wall-clock bounds can flake. Fixed (C8): the client tests bound
    by the deadline plus `REPLY_CLEANUP_ALLOWANCE` plus 5 s.

Reversed in this round: the first design kept the server in the caller's
process group so an attempt's group stop would reach it, and listed an
own group as a wrong implementation. The owner reversed it (audit M3): a
worker the server forks must end with the close, which only a group of
its own allows, and `PR_SET_PDEATHSIG` keeps the attempt stop on Linux.
Item 6 of "Wrong implementations" below keeps the old entry, marked
reversed.

The file count passed the ceiling of 80 by four (84, "Files"): the
fixes needed `crates/apb-server/Cargo.toml`,
`scripts/third-party-notices.sh`, `crates/apb-core/src/connector/install.rs`,
`crates/apb-core/src/trust.rs`,
`crates/apb-cli/tests/suite/release_workflow_test.rs`,
`crates/apb-engine/src/connector/mcp/redact.rs` and `docs/EVALS.md`.

### Fix round 2, 2026-11-20

An independent code review of the branch after fix round 1 (one
blocker, two major, three minor findings). Every finding was verified
against the code and accepted by the owner; each is fixed below and the
sections above state the behaviour as it is now.

1. Blocker 1, a request reported `not_sent` could still be written. A
   timeout between the pin check and the confirmed write could leave the
   `tools/call` queued in rmcp or in the tap writer while the hand-over
   count was still unchanged; the session answered `NotSent`, and rmcp's
   writer task could write the request later, even during the close, so
   a repeat could act twice. Fixed: `LineTap::seal` and the `WriteGate`
   lock, under which the tap writer checks the seal, writes and counts;
   every unanswered outcome seals first and compares the count after
   (D3 "When a call counts as handed over", item 5). The writer is
   sealed rather than stdin closed, because only the seal can be one
   locked step with the count read. Tests:
   `tap::tests::a_sealed_writer_never_sends_a_queued_call`,
   `tap::tests::a_call_partly_written_before_the_seal_is_handed_over`
   and `stdio_test::a_call_queued_behind_a_held_writer_is_not_sent_and_never_written`
   (the server stops reading after the first page, a 256 KiB cursor
   fills the pipe, the call times out queued: `NotSent`, and once the
   server reads again no `tools/call` arrives; it fails when the seal is
   removed).
2. Major 2, the protocol version bypassed the redaction. Fixed: the
   version is compared whole with `SUPPORTED_VERSIONS` before anything
   keeps it; `Era` holds only the constant; a refusal is
   `mcp_protocol_unsupported` without the string, also for rmcp's
   `NoCompatibleProtocolVersion` (D3 "Mapping", with the table of every
   other server-provided string). The walk found the tool title, the
   unmapped names and the member names bounded only on the terminal;
   they are now cut to `SERVER_NAME_CHARS` after the redaction in the
   listing. Tests: `session::tests::only_a_supported_version_is_kept`,
   `stdio_test::an_unsupported_protocol_version_is_refused_without_echo`,
   `stdio_test::a_supported_older_version_is_kept_as_the_constant`,
   `pin::tests::a_server_name_is_cut_after_the_redaction` and
   `pin::tests::a_listing_bounds_every_server_name`.
3. Major 3, `NeedsStart` skipped the budget. Fixed: the locked reminder
   and budget checks run before the gate answers `NeedsStart` (D4 "The
   order of one call", step 3). Test:
   `send_once::tests::a_spent_budget_refuses_before_needs_start` (A in
   flight, B's preview goes to the gate, A settles `not_sent` and
   spends `max_calls: 1`, B's gate refuses and B's helper writes no
   marker). The interleaving is driven at the gate, the call path's own
   rule being that only `NeedsStart` starts anything; an end-to-end race
   between two processes has no deterministic hold without a seam.
4. Minor 4, the approval dropped its listing's cleanup result. Fixed:
   `ToolListing.cleanup` and the warning line of `print_pins` item 9.
   Tests: `pin::tests::a_cleanup_note_names_every_end_not_shown` and
   `connector_mcp::tests::an_unconfirmed_cleanup_is_shown_before_the_question`.
5. Minor 5, the abandoned connect skipped the late reap. Fixed:
   `Conn::finish` (D4 item on the close). Tests:
   `session::tests::an_abandoned_connect_reaps_the_server` and
   `session::tests::finish_reaps_a_late_child`. An `Unconfirmed` close
   cannot be produced without a seam (no process survives SIGKILL for
   2 s on demand), so the late reap is tested on `finish` itself.
6. Minor 6, the discover-deadline test passed without the deadline.
   Fixed: merged into
   `stdio_test::a_handshake_failing_at_the_deadline_is_reaped_before_connect_returns`
   (a silent server, so the discover probe is the one bounded), which
   asserts the confirmed cleanup and a time below the caller's reply
   timeout; with the internal deadline removed it fails on
   `Ended::Unconfirmed`.

### Fix round 3, 2026-11-20

A second independent code review after fix round 2 (one blocker, two
major findings). Every finding was verified against the code and
accepted by the owner; each fix closes the class, not the instance.

1. Blocker 1, a sent write could still settle `not_sent`. The tap
   writer forwarded a line it could not parse uncounted: arguments of
   126 nested arrays put the request past serde_json's limit, a server
   with a higher limit could act on it, and the seal then saw an
   unchanged count. The class is "a line reaches the server
   unclassified". Fixed: `frame::classify`, whose parse failure is its
   own answer; a line it cannot classify, or one over the line limit, is
   refused before any byte is forwarded, with a named reason, and seals
   the writer; and the edge (`require_call_arguments`, again in
   `Session::call_tool`) refuses arguments deeper than 64 levels or
   longer than 1 MiB, so a legal call never reaches the tap's limits
   (D4 "The client", item 3.2). Tests:
   `frame::tests::arguments_past_the_parser_limit_are_refused_on_both_sides`,
   `frame::tests::a_line_that_does_not_classify_is_never_other`,
   `frame::tests::the_edge_takes_the_deepest_and_longest_legal_arguments`,
   `tap::tests::a_line_the_tap_cannot_classify_is_never_forwarded`,
   `stdio_test::arguments_past_the_parser_limit_are_never_sent` and
   `connector_mcp::arguments_too_deep_or_too_long_are_refused_at_the_edge`.
2. Major 2, an invalid frame could stand in for the listing hashed. The
   reader took any frame with an awaited id. The class is "the raw result
   kept is not the frame rmcp accepted". Fixed: only a whole successful
   response envelope by rmcp's shape rule is captured; any other frame
   for a watched id is left to rmcp, and two frames for one id fail the
   listing closed; the raw result must read as rmcp's typed answer
   (`same_listing`), else `mcp_listing_mismatch` (D4 "The client",
   items 3.3 and 4). Tests: `frame::tests::only_a_whole_response_envelope_is_a_response`,
   `tap::tests::two_frames_for_one_listing_fail_closed`,
   `tap::tests::only_a_whole_response_is_captured` and
   `stdio_test::a_frame_with_a_method_never_stands_in_for_the_listing`
   (the review's sequence end to end: `mcp_listing_ambiguous`).
3. Major 3, a secret reflected as an error code was shown. The code was
   interpolated after only the message was redacted. The class is "a
   server value interpolated after the redaction". Fixed: every
   `CallError` the mcp kind builds goes through `map::composed`, which
   redacts the whole composed message and then bounds it; the audit
   table in D3 "Mapping" walks every server number and key. Test:
   `map::tests::a_secret_reflected_as_a_number_is_redacted_in_every_message`.

### Fix round 4, 2026-11-20

A third independent code review (one major finding). The finding: the
reader parsed an incoming line for the listing capture only, read a line
its parser refused as "nothing for the capture" and still passed it on
to rmcp. rmcp 3.9.0 strips a byte order mark before it parses and the tap
did not, and a member named twice is read differently by the two, so two
answers for the awaited listing id in one write (the first with a mark
and a changed unknown tool member such as `execution.taskSupport`, the
second without it and with the pinned value) made rmcp accept the first
and the tap capture the second; the typed cross-check drops unknown
members, so a changed definition passed its old pin. This was the third
finding in a row on one mechanism, two parsers reading one stream.

The rule that closes the class: rmcp never receives a byte the tap did
not read completely (D4 "The client", item 3.3). Every incoming line is
read by the strict reader before any byte of it is handed on, and a line
that is not exactly one JSON-RPC 2.0 frame under it is never handed on:
the reads end with `mcp_message_invalid:`, the call settles by the
existing rules and the session is closed. No branch remains that passes
on bytes the tap did not understand, so the two parsers can no longer
disagree on which frames exist; the envelope rule, the two-frames rule
and the typed cross-check of fix round 3 stay. The outgoing side was
already closed by fix round 3 (every line the client writes is
classified before any byte is forwarded, a line that does not classify is
refused and seals the writer); it now uses the same duplicate-refusing
reader at every depth.

Tests, each checked to fail with the fix reverted (the reader passing on
a line it refused, the duplicate rule replaced by a plain parse, the
carriage-return rule removed) and to pass with it restored:
`tap::tests::the_reviews_trigger_reaches_rmcp_with_no_byte` (both answers
in one read, the mark on the first, differing `execution.taskSupport`),
`connector_mcp::a_byte_order_mark_answer_never_matches_a_pin` (the same
through the whole call path: refused, no `tools/call`, no intent),
`stdio_test::a_byte_order_mark_never_reaches_rmcp`,
`stdio_test::a_duplicated_unknown_top_level_member_is_refused`,
`stdio_test::a_duplicated_member_inside_a_tool_definition_is_refused`,
`stdio_test::a_byte_order_mark_on_a_call_answer_is_a_lost_answer`,
`stdio_test::garbage_after_the_json_value_is_refused`,
`tap::tests::a_refused_line_reaches_rmcp_with_no_byte`,
`frame::tests::a_line_the_tap_cannot_read_completely_is_refused` and,
the other way, `stdio_test::a_chatty_legitimate_server_still_works`
(notifications, a server request, an error without an id and an answer
to an id nobody asked, all before the answer).

Addendum (the owner's decision on the round's open question): a line
neither parser reads is as safe as one both refuse, so apb does not end
the session on a server that logs to stdout. A whole line whose first
byte is not `{` is dropped unread and counted (at most 1000 a session,
then `mcp_message_invalid`); the note carries the count, never the
text. A line starting with the byte order mark is checked first and
still ends the session, and no line is passed on unless it passed the
strict rule. Tests, each failing with its part reverted:
`tap::tests::log_lines_between_frames_are_dropped_and_counted` (the drop
rule removed, or the line passed on instead of dropped),
`tap::tests::the_dropped_line_cap_ends_the_session` (the cap removed),
`tap::tests::a_byte_order_mark_line_is_refused_not_dropped` (the mark
dropped like log text), `connector_mcp::log_lines_on_stdout_are_dropped_and_only_counted`
and `connector_mcp::a_dropped_line_never_reaches_an_error` (the drop rule
removed, or the count left out of the note; both also check that no file
of the run, no result and no error holds a dropped line's text).

### Fix round 5, 2026-11-21

The fourth independent code review had one finding, a minor one, and no
other. `SessionHandle::close_counted` read the count of dropped lines
(`Session::dropped_lines`) before it closed the session, so a log line
the reader took in between the two was missing from the note. The count
is now read once `Session::close` returned; every caller of
`close_noted` (the call paths in `connector/call/mod.rs`, a failed
listing, an abandoned call and the healthcheck in `pin.rs`) reports
through it.

Test: `connector_mcp::a_line_written_after_the_answer_is_counted` (a log
line the server writes right after a failed call's answer, in a write of
its own, through the new stub option `set_call_suffix`): it fails when
the count is left out of the note. With only the order reverted it still
passes: the window lies between two adjacent statements while rmcp's
loop keeps reading, and no test reaches it deterministically. The test
stays as the first coverage of a line written after an answer (the
earlier ones write log lines before the answers only).

### Rebased on main after #292, 2026-11-21

The branch was rebased from eval-doubles (#291) onto "Secret params are
never rendered" (#292), without a conflict. What was reconciled:

1. V340 (no string of a playbook references a secret param) walks the
   whole definition through `validate::secret_param_refs`, so the
   arguments a node hands an mcp function are covered like every other
   connector argument. The `mcp:` block itself lives in the connector
   file, not the playbook: its `args` take `{{account.*}}` only and its
   `env` `{{account.*}}` and `{{secret.*}}`, and a `{{params.*}}`
   placeholder in either is refused when the connector loads (unknown
   placeholder namespace). New test:
   `connector::mcp::tests::a_param_placeholder_is_refused_in_args_and_env`.
   Account fields are values from the project's accounts file and are
   rendered with no param.
2. Nothing of this change uses `run_redact` (the mcp paths redact with the
   call's own `Redaction` list) or puts params on a detached driver's argv,
   so #292's changes there need nothing here.
3. V340 sits above this change's V300 to V302; no code collides.
4. `AGENTS.md` and `CLAUDE.md` keep both sides' text (V340 and the MCP
   connector kind) and stay mirrored.

### Pull request CI, 2026-11-21

The Linux and macOS test legs failed one test,
`connector_mcp::tests::an_args_change_needs_the_account_approved_again`
in apb-cli: its run gate asked for a billing confirmation
(`billing_requires_confirmation`) because the runners have no claude
login, so the agent's basis was unknown; on the development machine the
login made it known. The test is about the pins, not billing: it now
writes a `policy.yaml` in its own config dir that allows the run's agent
work. Reproduced and checked with an empty `HOME` and no agent CLI on
`PATH`; every MCP test of apb-cli and apb-engine passes that way.

CodeQL reported 16 `rust/cleartext-logging` alerts, all on assertion
messages in test modules over refusal texts and test connector
definitions; they were dismissed as false positives with that reason.

### Pull request review, 2026-11-21

An automated review of the pull request left nine threads and 27 comments
in its review body. Dispositions:

Threads:

1. Fixed: a tool's identity (function, tool, status, digest) took part in
   the server-text budget of the tools confirmation, so earlier large
   schemas could hide a later tool that the confirmation still pins. The
   identity of every row is now always shown; only its description stays
   under the budget (`connector_mcp::tests::every_tool_row_is_shown_past_the_text_budget`).
2. Fixed by making the state explicit: a pin write that fails after the
   account approval was recorded says so and says that running the same
   approval again pins the tools; that state fails closed (every call and
   run start stays refused `mcp_tools_unpinned`)
   (`connector_mcp::tests::a_failed_pin_write_says_the_account_approval_was_kept`).
3. Fixed: the account-entry test's isolation assertion is gated on debug
   assertions like the module it reads.
4. Fixed: revoking an approval forgets only the pins bound to the revoked
   digest (`PinStore::forget_bound`: the tree digest for a connector, the
   account digest for an account), so revoking an older tree leaves the
   pins of the current one (`a_revoked_approval_takes_its_pins_with_it`).
5. and 6. Fixed: an untracked live mcp call decides its attempt and its
   `max_calls` again on the journal as it is once the server runs, and
   abandons the session without `tools/call` on a refusal
   (`connector_mcp::an_allowance_spent_during_the_start_refuses_the_call`).
   Two untracked calls admitted at the same instant are not serialized,
   as for every connector kind on main.
7. Fixed: fit names an unreadable pin store behind a child playbook's
   refusal (`fit::trust::tests::an_unreadable_pin_store_of_a_child_is_a_row`).
8. Fixed: where the program starts by its path, the path is hashed again
   with nothing but the spawn left to do.
9. Kept as designed: when a line the tap refuses arrives in the same read
   as earlier accepted frames, none of them reaches rmcp and the session
   ends; a call answered in that read settles `unknown`, which never
   claims an effect and goes to a person, rather than being credited to a
   server that broke the protocol in the same read.

Review body: 15 fixed, 12 declined with a reason in the reply. Fixed: a
deadline passed before the start starts nothing; rmcp's response cache is
off, so every listing is read from the wire; `pre_exec` returns a raw OS
error; an uninstall of an absent connector still forgets its pins; the
approval and `list_live` stop on an unreadable pin store before anything
starts; a program path that is not UTF-8 is refused at approval; a
program directory holding `:` is left out of the server's `PATH`; and
test fixtures and checks (quoted program paths, exited versus reaped
processes, the raw tool list clearing paged mode, the redaction test's
cut, the healthcheck test of a changed tool, the close-order doc).
Declined: consent grading of calls not sent (changes the merged rules
grader), empty HTTP fields on mcp functions (shared deserialization),
server text in client failures (redacted and cut in the engine, D3),
member labels and redacted-key collisions (display only, server text),
the inbox-grant prompt warning (merged behaviour, recorded follow-up), the
stamp size after the start (already fails closed as `not_sent`), a close
synchronization hook (a test-only seam), and two trivial refactors.

### Wrong implementations

Each item names the test or check that catches it; the per-decision
lists above hold the rest.

1. Building branch 7a before eval-doubles' step 5b, so a doubled mcp
   call starts its server. `connector_mcp.rs::an_mcp_function_is_answered_by_its_double`.
2. Adding an `Mcp` arm, a kind check or an mcp double type to
   eval-doubles. The final diff review: `eval_doubles.rs` and
   `eval/doubles.rs` unchanged.
3. A hand-kept member list in `tool_digest`.
   `digest::tests::a_member_no_list_names_is_pinned`.
4. Sending a signal to any process but the session's own server group,
   or to that group after the server was reaped (through rmcp's
   `graceful_shutdown`, `libc::kill` of a pid or `nix`). The `rg` of
   work plan item 5.1 (its expected hits only) and
   `stdio_test.rs::a_server_that_ignores_end_of_input_is_killed_and_reaped`.
5. Reporting a server ended without a reaped `wait`, or its group empty
   without `ESRCH`.
   `stdio_test.rs::a_server_that_ignores_end_of_input_is_killed_and_reaped`
   and the unit test `connector::mcp::tests::an_unconfirmed_end_is_a_warning_never_a_stop_claim`
   (over `end_warning`, below).
6. Reversed in fix round 1, by the owner's decision. The entry read:
   "Giving the server its own process group, so stopping an attempt no
   longer reaches it", caught by
   `connector_mcp.rs::the_server_shares_the_callers_process_group`. It
   is now the required behaviour: a worker the server forks kept the
   account's secrets and could act after the call settled (audit M3),
   and only a group of its own lets the close end it. The attempt stop
   still ends the server on Linux through `PR_SET_PDEATHSIG`; elsewhere
   a killed caller leaves a server that ignores end of input running
   (D4 "The crash limit"). The wrong implementation now is the shared
   group, caught by `connector_mcp.rs::the_server_leads_its_own_process_group`
   and `stdio_test.rs::a_worker_the_server_forked_is_ended_with_its_group`.
7. Spawning without `guard_program_start`.
   `stdio_test.rs::a_read_only_process_starts_no_server`.
8. Reading the pin store through a link, or answering a refused read or
   a failed lock with an empty store. `pins::tests::a_linked_store_is_refused`,
   `pins::tests::an_unparsable_store_is_an_error`.
9. Comparing digests in the gate (starting a server at run start) or a
   second time after the call. `connector_mcp_gate.rs::the_gate_starts_no_server`
   and `connector_mcp.rs::a_description_changed_between_two_calls_is_refused`.
10. Leaving `tool_error` out of `MAY_HAVE_ACTED`.
    `grade_rules_test.rs::a_tool_error_call_may_have_acted`.
11. Writing `tool_digest` on a doubled call (no tool was verified).
    `connector_mcp.rs::an_mcp_function_is_answered_by_its_double`.
12. A new run guard, manifest mark or event type for MCP calls. The
    final diff review: `guarded.rs`, `manifest.rs` and the
    `EventPayload` variant list unchanged; `journal_compat_test` passes
    as is.
13. Starting a program a caller chose (a bare name looked up at call
    time, a caller's `HOME` or proxy). `connector_mcp.rs::a_shadow_program_on_the_callers_path_is_not_started`
    and `the_server_sees_only_the_fixed_set_and_its_declared_env`.
14. A tool digest over rmcp's typed `Tool`.
    `connector_mcp.rs::a_changed_execution_member_refuses_the_next_call`.
15. Pins written without a person, or a refusal that prints a digest:
    `connector_mcp_cli_test.rs::approve_without_a_terminal_is_refused`
    and `a_retry_without_a_terminal_learns_nothing`.
16. An intent recorded before the server started and listed, on any
    path, or a close of a successful call before its settlement.
    `connector_mcp_send.rs::a_kill_between_connect_and_call_leaves_no_intent`,
    `send_once::tests::a_gate_that_may_not_record_records_nothing` and
    `the_settlement_is_journaled_before_the_close`.
17. A pin record without the tree digest, or a tree approval that makes
    an old pin usable. `connector_mcp.rs::a_changed_definition_refuses_the_old_pin`,
    `connector_mcp_gate.rs::an_old_pin_under_a_reapproved_tree_is_unpinned`
    and `connector_mcp::tests::an_args_change_needs_the_account_approved_again`.
18. A connector approval of an MCP connector without a person.
    `connector_mcp_cli_test.rs::approve_tree_without_a_terminal_is_refused`,
    `approve_tree_in_a_run_step_is_refused`
    and `connectors_api_test.rs::the_dashboard_refuses_to_approve_an_mcp_connector`.
19. Hashing, printing or storing a tool definition that reflects a
    secret (fourth pass: refused before anything is hashed).
    `connector_mcp::tests::a_reflected_secret_refuses_the_approval`
    and `pin::tests::a_reflected_secret_refuses_the_listing_before_hashing`;
    text shown from elsewhere without the redactions:
    `pin::tests::the_display_walk_rewrites_member_names`.
20. A live tool call outside a run (healthcheck or playground).
    `connector_mcp.rs::the_healthcheck_checks_the_pins_without_a_call`
    and `a_playground_call_of_an_mcp_function_is_refused_by_name`.
21. Registering a `tools/list` id after forwarding its bytes, or
    forwarding a line in pieces. `tap::tests::the_id_is_registered_before_any_byte_is_forwarded`,
    `a_split_request_is_registered_once_whole` and
    `stdio_test.rs::a_listing_answered_at_once_is_captured_every_time`.
22. A line limit that refuses a body at the cap in the worst encoder
    escaping. `connector_mcp.rs::a_body_at_the_cap_in_the_worst_escaping_is_returned`
    and the unit test `connector::mcp::tests::max_line_bytes_covers_the_body_cap`.
23. Resolving a secret or starting a program before the program
    confirmation. `connector_mcp::tests::a_declined_program_confirmation_starts_nothing`.
24. Settling a post-hand-over JSON-RPC error of a write `not_sent`.
    `connector_mcp_send.rs::a_request_error_after_acting_is_unknown`.
25. A pin that serves another project's account of the same name.
    `connector_mcp.rs::a_pin_of_another_project_account_is_refused`.
26. A record that leaves a store `load` refuses.
    `pins::tests::every_successful_record_loads_again`.
27. A failed handshake that returns before the server is reaped.
    `stdio_test.rs::a_failed_handshake_reaps_a_server_that_ignores_end_of_input`.
28. Resolving a secret before the pin check.
    `connector_mcp.rs::a_changed_pin_runs_no_secret_helper`.
29. Listing for an account other than the one confirmed.
    `pin::tests::list_live_refuses_an_account_changed_since_confirmation`.
30. Returning from `connect` before a failed handshake's server is reaped.
    `stdio_test.rs::a_handshake_failing_at_the_deadline_is_reaped_before_connect_returns`.
31. Printing a definition string unsanitized.
    `connector_mcp_cli_test.rs::an_escape_sequence_in_mcp_args_is_refused_at_install` and `connector_mcp::tests::the_confirmations_sanitize_definition_and_account_text`.
32. A refusal or a confirmation that prints an expected digest a
    non-terminal caller could supply.
    `connector_mcp_cli_test.rs::a_retry_without_a_terminal_learns_nothing`.
33. A program confirmation that hides what an account field sets.
    `connector_mcp::tests::the_program_confirmation_shows_the_rendered_start`.
34. A refused live call that resolves secrets first.
    `connector_mcp.rs::a_playground_call_of_an_mcp_function_is_refused_by_name`.
35. Starting a program file other than the one hashed (fix round 1).
    `connector_mcp.rs::a_program_swapped_while_the_secrets_resolve_never_runs_with_them`
    and the client tests of D2 "Wrong implementations" item 20.
36. A server text cut before its redaction, or an answer redacted in its
    string values only (fix round 1).
    `map::tests::a_secret_across_the_cut_is_redacted_whole_before_the_cut`
    and `a_secret_as_an_answer_key_or_number_is_redacted`.
37. A `testing` feature on outside dev-dependencies (fix round 1).
    `release_workflow_test::testing_features_stay_in_test_builds`.
38. A recorded call that never reached the server settled `unknown` (fix
    round 1). `connector_mcp_send.rs::a_server_gone_before_the_call_settles_not_sent`.
39. A `NotSent` answered for a request that rmcp could still write (fix
    round 2). `stdio_test.rs::a_call_queued_behind_a_held_writer_is_not_sent_and_never_written`
    and `tap::tests::a_sealed_writer_never_sends_a_queued_call`.
40. A server's protocol version kept, cut or shown before it is
    validated (fix round 2). `stdio_test.rs::an_unsupported_protocol_version_is_refused_without_echo`.
41. A spent budget found only after the start (fix round 2).
    `send_once::tests::a_spent_budget_refuses_before_needs_start`.
42. An approval that hides an unconfirmed cleanup of its listing (fix
    round 2). `connector_mcp::tests::an_unconfirmed_cleanup_is_shown_before_the_question`.
43. A line forwarded to the server without being classified (fix round
    3). `tap::tests::a_line_the_tap_cannot_classify_is_never_forwarded`.
44. A listing hashed from a frame rmcp did not accept (fix round 3).
    `stdio_test.rs::a_frame_with_a_method_never_stands_in_for_the_listing`.
45. A server number interpolated after the redaction (fix round 3).
    `map::tests::a_secret_reflected_as_a_number_is_redacted_in_every_message`.
46. Forwarding to rmcp a frame the tap could not parse, or read
    differently from rmcp (a byte order mark, a member named twice,
    bytes around the object) (fix round 4).
    `tap::tests::the_reviews_trigger_reaches_rmcp_with_no_byte` and
    `connector_mcp::a_byte_order_mark_answer_never_matches_a_pin`.
