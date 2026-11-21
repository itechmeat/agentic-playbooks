# mcp-stdio-transport Specification

## Purpose
An MCP server runs for one call over stdio, started from the exact program file a person approved, with an environment from a fixed source, in a fresh private working directory and as the leader of its own process group, its output read by a bounded strict line reader, and ended and reaped before the call reports its end.

## Requirements

### Requirement: One server per call
Each call SHALL start its own server process, open the conversation in the connector's `protocol` (`auto` probes with `server/discover` and falls back to `initialize`), list the tools, and make at most one `tools/call`, all within the function's `timeout_sec`, the discover probe included, then end the process after the call is settled.

#### Scenario: Two calls start two servers
- **WHEN** a node makes two calls of an mcp function
- **THEN** the server is started twice

#### Scenario: A legacy server
- **WHEN** the server answers `server/discover` with an error and supports `initialize`
- **THEN** the call works on the legacy revision

### Requirement: The server gets an environment from a fixed source
The server SHALL get `HOME`, `USER` and `LOGNAME` from the account database, the XDG base directories as their defaults under that home, `PATH` as `/usr/local/bin:/usr/bin:/bin`, then `/opt/homebrew/bin` on macOS, then the pinned program's directory last when it is not one of them, and the variables its `env` declares, and nothing of the calling process; proxy and certificate variables SHALL reach it only when its `env` declares them.

#### Scenario: Caller variables stay out
- **WHEN** the calling process has `APB_RUN_DIR`, `APB_CONFIG_DIR`, `HTTPS_PROXY`, `NODE_EXTRA_CA_CERTS`, another `HOME` and an agent token set
- **THEN** the server sees none of those values, sees the account database's home, and sees its declared `TRACKER_TOKEN`

#### Scenario: A declared proxy
- **WHEN** the connector's `env` declares `HTTPS_PROXY` from an account field
- **THEN** the server sees the account's value

#### Scenario: The program's directory comes last
- **WHEN** the pinned program lives in a directory a project can write
- **THEN** the server's `PATH` lists the system directories first and that directory last

### Requirement: The server runs in a private directory
The server SHALL get a fresh, empty, private (0700) working directory that the session creates new under the caller's temporary directory and removes with everything in it after the server was reaped, never the project root, and a null stderr. The spawn SHALL pass apb's program-start guard, so a read-only or scenario process starts no server.

#### Scenario: A read-only process
- **WHEN** a read-only process opens a session
- **THEN** the session is refused and no server starts

#### Scenario: The working directory
- **WHEN** a server starts
- **THEN** its working directory is a new empty private directory that is not the project root, and the directory is gone after the session closed

### Requirement: The server leads its own process group
The server SHALL be started as the leader of its own process group. On Linux it SHALL get `PR_SET_PDEATHSIG` set to SIGKILL, so it is killed when the thread that started it ends.

#### Scenario: The process group
- **WHEN** a server starts
- **THEN** it is the leader of its own process group, not in the caller's

### Requirement: The program that runs is the file that was hashed
A session SHALL start the program from the one open of its file: opened once without following a link and without blocking on a FIFO, a regular file only, its digest read from that descriptor. On Linux the session SHALL execute that descriptor through `/proc/self/fd/<n>`. On other platforms the session SHALL open the path again without following a link and hash it again immediately before the spawn, and SHALL refuse the start, starting nothing, when the digest differs.

#### Scenario: A program replaced after the hash on Linux
- **WHEN** the program file is replaced by another file after it was hashed and before the start
- **THEN** the hashed file runs and the replacement never runs

#### Scenario: A program replaced after the hash elsewhere
- **WHEN** the session starts by path and the file at the path no longer holds the hashed bytes
- **THEN** the start is refused and nothing starts

#### Scenario: A program that is missing or a link
- **WHEN** the program path is missing or is a symbolic link
- **THEN** it is refused before any start

### Requirement: The limits of the program check are stated
The documentation SHALL state that the window between the second hash and the exec remains on platforms that start by path, that a rewrite in place of the same file is covered on no platform, and that on Linux a script's own name (`$0`) is `/proc/self/fd/<n>`, a link that resolves to the approved file.

#### Scenario: The connector documentation
- **WHEN** a person reads the MCP section of the connector documentation
- **THEN** it names the remaining window, the rewrite in place and a script's `$0` on Linux

### Requirement: Shutdown ends the server's process group
Ending a call SHALL close the server's stdin and wait up to 3 s for the server to exit without reaping it; then, with the server still unreaped, SHALL send SIGKILL to the server's whole process group; SHALL reap the server within 2 s; and SHALL look up to 1 s for the group to be empty. apb SHALL NOT signal any process but its own server's group, and that group only while the server is unreaped.

#### Scenario: A server that ignores end of input
- **WHEN** the server keeps running after its stdin closes and ignores SIGTERM
- **THEN** its group is killed and its exit is reaped

#### Scenario: A worker the server forked
- **WHEN** the server forks a worker in its group that outlives the server
- **THEN** the close ends the worker and reports the group empty

### Requirement: The end is reported as proven
apb SHALL NOT say the server ended unless its exit was reaped, nor that its group is empty unless the group was shown empty; an unconfirmed end and a group not shown empty SHALL each be a warning on the call. A reap that does not come in time SHALL leave the worker waiting on the child until it is reaped, so no zombie stays behind; this SHALL hold on every cleanup branch, including a connect whose caller stopped waiting before the handshake's answer reached it.

#### Scenario: A group not shown empty
- **WHEN** the server was reaped and its group was not seen empty within the bound
- **THEN** the call carries the warning that a process it started may still run and act

#### Scenario: An abandoned connect
- **WHEN** the caller's wait for a connect ended before the handshake's answer reached it
- **THEN** the worker closes the server and keeps waiting on it until it is reaped, and removes its working directory

#### Scenario: The handshake deadline
- **WHEN** a silent server never answers the discover probe and the deadline passes
- **THEN** the server is reaped before the connect returns, sooner than the caller's own reply timeout

### Requirement: A listing answer is captured whatever its timing
The client SHALL register the id of a `tools/list` request before any byte of that request reaches the server, holding a request written in several pieces until its line is whole, so a listing answered at once is always read off the wire.

#### Scenario: A request split across writes
- **WHEN** a `tools/list` request reaches the client's writer in three pieces
- **THEN** its id is registered once and the server receives one whole line after the last piece

#### Scenario: An immediate answer
- **WHEN** the server answers each `tools/list` as soon as it reads it
- **THEN** every listing of a hundred sessions is captured

### Requirement: Nothing reaches the server unclassified
The client SHALL classify every line it writes to the server before any byte of it is forwarded, as a `tools/call`, a `tools/list` request or another message, reading it as one JSON object whose members are each named once. A line it cannot classify, or one over the line limit, SHALL be refused before any byte of it is forwarded, with a named reason, and nothing SHALL be written to that server after it; a parse failure SHALL never be read as "not a call".

#### Scenario: A line the client cannot read
- **WHEN** the client's writer is handed a line that does not parse as one JSON object
- **THEN** no byte of it and nothing after it reaches the server, and no call counts as handed over

### Requirement: Call arguments fit the request line
The engine SHALL refuse, `invalid_args` before any secret resolves or server starts, call arguments nested deeper than 64 levels or longer than 1 MiB as JSON, and the session SHALL refuse them again before it queues anything, so a legal call always classifies.

#### Scenario: Arguments nested past the parser's limit
- **WHEN** a call's arguments are 126 nested arrays around a number
- **THEN** the call is refused `invalid_args` at the edge, no server starts, and a session asked to send them directly answers not sent without writing any byte of the request

### Requirement: Only the frame rmcp accepts is hashed
The client SHALL take the raw result of a listing answer only from a frame that is a whole successful response for that request's id (`jsonrpc` `"2.0"`, the id, a `result` object, no other member) and SHALL leave every other frame for that id to the protocol library uncaptured. Two frames for one listing id SHALL fail the listing closed, and so SHALL a raw result whose tools or cursor differ from the answer the protocol library returned.

#### Scenario: A frame with a method beside an old result
- **WHEN** the server sends, for the listing's id, a frame holding a method and an old tool list and then a whole response with the new one
- **THEN** the old list is never used and the listing fails closed

#### Scenario: Two whole answers for one listing id
- **WHEN** the server answers one listing id with a response holding an extra member and then with a whole response
- **THEN** the listing fails `mcp_listing_ambiguous`

### Requirement: The client passes on only what it read completely
The client SHALL hand the protocol library only whole lines it read completely under its strict rule (one JSON-RPC 2.0 frame, valid UTF-8, no byte order mark, no member named twice at any depth, nothing around the object but one carriage return), byte for byte. A line that starts with `{` and fails the rule, or starts with a byte order mark, SHALL NOT be passed on, nor any line after it; the session SHALL end with `mcp_message_invalid`, and a call in flight SHALL settle by the hand-over rules.

#### Scenario: A byte order mark on one of two answers in one read
- **WHEN** the server writes two answers for the awaited listing id at once, the first with a byte order mark and a changed unknown tool member, the second with the pinned value
- **THEN** no byte of either reaches the protocol library, the listing fails `mcp_message_invalid` and no pin is matched

#### Scenario: A member named twice
- **WHEN** a line from the server names a member twice, at the top level or inside a tool definition
- **THEN** nothing of it is passed on and the session ends `mcp_message_invalid`

#### Scenario: A legitimate server around the answer
- **WHEN** the server sends notifications, a request of its own, an error without an id and an answer to an id nobody asked before the listing's answer
- **THEN** the listing works

### Requirement: Lines that are not messages are dropped unread
A whole line from the server whose first byte is not `{` and that does not start with a byte order mark SHALL be dropped: not parsed, not passed on, counted against the session byte limit and against a cap of 1000 dropped lines a session, past which the session SHALL end with `mcp_message_invalid`. The call's note SHALL give their count and SHALL NOT hold their text.

#### Scenario: A server that logs to stdout
- **WHEN** the server writes log text and blank lines between its messages
- **THEN** the lines are dropped unread, the call succeeds, its note gives their count, and no result, error or run file holds their text

#### Scenario: Too many dropped lines
- **WHEN** the server writes more than 1000 lines that do not start a JSON object
- **THEN** the session ends `mcp_message_invalid`

### Requirement: The end of a killed call's server is stated as it is
When the calling process is killed, on Linux its server SHALL be killed with the thread that started it (`PR_SET_PDEATHSIG`); on other platforms it SHALL end only by exiting on end of its stdin. A call killed on such a platform while its server ignores end of input leaves that server running, and the server's own workers are not covered by the Linux signal; the documentation SHALL say so.

#### Scenario: A killed call on Linux
- **WHEN** a call process is killed while its server ignores end of input on Linux
- **THEN** the server is killed with it

#### Scenario: A killed call elsewhere
- **WHEN** a call process is killed on a platform without that signal while its server ignores end of input
- **THEN** apb sends that server no signal and the connector documentation names this limit

### Requirement: The client works under an async runtime
The MCP client SHALL run its conversation on its own thread, so a blocking caller inside an async runtime (such as a dashboard handler) works.

#### Scenario: A session inside an async runtime
- **WHEN** a caller running inside an async runtime opens a session and calls a tool
- **THEN** the call answers without a runtime panic

### Requirement: The answer is shaped
The body SHALL be `structuredContent` when present, else the joined text blocks parsed as JSON when they parse, else `{ "text": ... }`; other blocks SHALL be counted as `dropped_content`; every resolved secret SHALL be redacted in every string, every object member name and the JSON text of every number (a number whose text holds a secret becomes the redacted text); a body over 1 MiB SHALL fail a read `incomplete_response` and mark a write incomplete.

#### Scenario: A secret echoed by the tool
- **WHEN** the tool's text contains the account's token
- **THEN** the result shows `[redacted:<VAR>]` instead

#### Scenario: A secret as a key or a number
- **WHEN** the tool's structured answer holds the account's token as a member name, or a numeric secret as a JSON number
- **THEN** neither reaches the result unredacted

### Requirement: Server text is redacted whole, then bounded
Every text the server provides (a tool's `isError` text, a JSON-RPC error message, the message of a lost or not-sent call, every connect or listing failure message) SHALL pass the account's redactions over the whole text first and SHALL then be cut to 500 characters with a marker that says how many were left out. The `isError` text SHALL be the text blocks joined as they are, never parsed as JSON.

#### Scenario: A secret across the cut
- **WHEN** a tool's error text holds the account's token starting at character 495
- **THEN** no part of the token reaches the error and the error is cut with a marker

#### Scenario: An error text that reads as JSON
- **WHEN** a tool's error text is `{"code":7}`
- **THEN** the error carries that text as it is

### Requirement: A composed message is redacted whole
Every message apb composes with a server-provided value, a number such as a JSON-RPC error code included, SHALL pass the account's redactions over the whole composed message before it is bounded; a server's number SHALL be kept as a number only to classify.

#### Scenario: A secret reflected as the error code
- **WHEN** the account's secret is `123456` and the server answers a call with JSON-RPC error code 123456
- **THEN** neither the error nor the settlement's detail holds `123456`, and the code still classifies the answer

### Requirement: Every kept server name is redacted whole, then bounded
Every short string the server provides that a listing keeps or shows (a tool's title, an unmapped tool's name, a tool's member names) SHALL pass the account's redactions over the whole string and SHALL then be cut to its bound with a marker, in the listing itself.

#### Scenario: A very long title
- **WHEN** a tool's title is a thousand characters long
- **THEN** the listing keeps it cut to its bound with a marker

### Requirement: Messages from the server are bounded
A single line from the server longer than the line limit, seven times the 1 MiB body cap plus 64 KiB (7 405 568 bytes), more than 10 000 lines in one session, or more than four times the line limit summed over all lines of one session (`MAX_SESSION_BYTES`), SHALL end the conversation with a failure whose message starts `mcp_message_too_large:`: before the request is handed over a failure that sends nothing, after it a lost answer.

#### Scenario: An oversize answer
- **WHEN** the server answers a call with one line longer than the line limit
- **THEN** the call fails with a message starting `mcp_message_too_large:` before its deadline

### Requirement: The line limit fits a body at the cap
The line limit SHALL fit a body at the cap sent once as `structuredContent` and once as its serialization in a text block, in the worst escaping a JSON encoder writes. A line that is longer for bytes outside the body (escapes of characters that need none, whitespace, extra or dropped blocks, `_meta`) MAY be refused although its body is under the cap.

#### Scenario: A body at the cap in the worst escaping
- **WHEN** a tool answers a body of 1 MiB as `structuredContent` and as its text serialization, every two-byte character written as an escape
- **THEN** the call returns the body

#### Scenario: A small body with a large dropped block
- **WHEN** a tool answers a short text and an image block that makes the line longer than the line limit
- **THEN** the call fails with a message starting `mcp_message_too_large:` although the body is under the cap

### Requirement: A failed handshake reaps the server
Every failure after the spawn inside the session's connect SHALL run the bounded close (end of stdin, wait, the kill of the server's process group, the reap, the look for the group empty) before the failure is returned, and the failure SHALL say whether the exit was reaped.

#### Scenario: A handshake failing at its deadline
- **WHEN** the handshake reaches its deadline against a server that ignores end of input
- **THEN** connect returns only after the server was killed and reaped, within the deadline plus the cleanup allowance

#### Scenario: A failed handshake against a server that ignores end of input
- **WHEN** the handshake fails against a server that ignores end of input
- **THEN** the failure is returned after the server was killed and reaped

### Requirement: Only a supported protocol version is kept
The protocol version a server reports SHALL be compared whole with the fixed set of versions the client supports before anything keeps, cuts, shows or stores it; only the matching constant SHALL be exposed. Any other version SHALL end the session with a refusal named `mcp_protocol_unsupported` whose message does not contain what the server sent.

#### Scenario: A version with a secret after it
- **WHEN** a legacy server answers `initialize` with the protocol version `2025-11-25` followed by the account's token
- **THEN** the session is refused `mcp_protocol_unsupported`, no part of the token is in the message, and the server is reaped

#### Scenario: An older supported version
- **WHEN** a legacy server answers with `2025-06-18`
- **THEN** the session opens on that version as the client's own constant
