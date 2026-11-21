# session-fork Specification

## Purpose
`continue_session: { from, mode: fork }` starts a node from a copy of another node's agent session without writing into it, so several nodes may start from one source at once. Claude and opencode fork through their fork form; other agents start cold with the reason recorded. A fork needs the source's profile and binding, a retry continues the fork and a re-entry forks again, and the handoff records the mode.

## Requirements

### Requirement: A node can fork a session
`continue_session` SHALL accept a bare node id (mode `continue`) or `{ from, mode: continue | fork }`, and a bare id SHALL serialize back as a bare id. With `mode: fork` the node SHALL start from a copy of the source's session through the agent's fork form and SHALL NOT write into the source session.

#### Scenario: Two forks in parallel
- **WHEN** two nodes fork one source and run at the same time on claude
- **THEN** both start warm, the source session is unchanged, and validation reports no V45

### Requirement: A retry continues the fork, a re-entry forks again
Within one attempt loop, a later attempt of a forked node SHALL continue the session id the fork reported, without the fork flag; when the forked attempt reported no session id, the next attempt SHALL fork the source again and SHALL NOT resume the source session. On every re-entry of the node (a deferred retry, a pause and its resume, a driver death, a resume) the next attempt SHALL fork the source session again, with the retry briefing in its prompt. No fork id SHALL be persisted.

#### Scenario: A retry after a fork
- **WHEN** the forked node's first attempt fails after reporting its session
- **THEN** the second attempt resumes the fork's own session without the fork flag

#### Scenario: A delayed retry
- **WHEN** a forked node with `retry_delay` fails and its retry comes due
- **THEN** the retry forks the source session again and its prompt carries the retry briefing

#### Scenario: A delayed retry on a fallback step
- **WHEN** the deferral falls on a fallback step whose executor matches the source's
- **THEN** the retry forks on that step's binary and binding

#### Scenario: A driver death
- **WHEN** the driver dies during a forked attempt and the run is resumed
- **THEN** the next attempt forks the source session again and the source is unchanged

### Requirement: Agents without a fork form start cold
A fork on an agent without a fork form, or on an installed CLI whose help does not list the fork flag, SHALL start cold, journaling `session_handoff` with the reason, and preflight SHALL report `session_handoff_cold` for an agent without a fork form. Claude and opencode SHALL have fork forms fixed in the source, and `apb doctor --agents` SHALL warn when an installed CLI's help lacks the flag.

#### Scenario: A fork on codex
- **WHEN** a node forks a source on codex
- **THEN** it starts cold with the reason that codex cannot fork a session

#### Scenario: A CLI without the flag
- **WHEN** a node forks a source on an installed claude whose help does not list `--fork-session`
- **THEN** it starts cold with the reason naming the missing flag

### Requirement: The run-time help check is cached
The run-time help check of a fork form SHALL run only after every other check of the handoff passed, never for a node that will not fork, and SHALL probe each agent binary at most once per process, keyed by its executable fingerprint (path, size, modification time), never per execution, without one lock held across probes of different binaries. A probe failure SHALL mean a cold start with the journaled reason; a help that does not read SHALL be probed again after 60 seconds.

#### Scenario: A failed probe
- **WHEN** the help of the agent binary does not read, and a fork comes 60 seconds later
- **THEN** the first fork starts cold with the reason and the later fork probes the help again

#### Scenario: A node that will not fork
- **WHEN** a fork node runs under another profile than its source
- **THEN** it starts cold naming both profiles and the help is not read

#### Scenario: A replaced binary
- **WHEN** the agent binary is replaced at the same path during the process
- **THEN** the next fork probes its help again

#### Scenario: Two forks in one process
- **WHEN** two forked nodes and a re-entry run on one claude binary in one process
- **THEN** the binary's help is read once

### Requirement: A fork needs the source's profile and a new session
A fork SHALL be warm only when the node's binding equals its source's (see "A fork compares the binding the source ran on"). A fork whose attempt reports the source's own session id SHALL be a failed fork: the node SHALL never continue it, SHALL start cold and SHALL journal `session_handoff` with the reason. A retry within the attempt loop SHALL continue the session a fork reported, also when the attempt that forked failed but printed its new session id.

#### Scenario: A fork that reports the source session
- **WHEN** a forked attempt reports the source's session id
- **THEN** the retry never resumes that id and the run journals a cold `session_handoff` with the reason

#### Scenario: A failed fork that printed its session
- **WHEN** a forked attempt fails after printing its new session id
- **THEN** the retry continues that new session

### Requirement: A fork compares the binding the source ran on
A fork SHALL compare the binding the node's execution entry will run on (a deferred retry's pinned binding included) with the binding the source's attempt recorded when it ran, whole except its routing fields; a source or a target rebound since SHALL be compared by those bindings.

#### Scenario: A source rebound after it ran
- **WHEN** the source succeeds under one profile and is rebound to the fork's profile before the fork starts
- **THEN** the fork starts cold naming both profiles

### Requirement: The handoff records the mode
`session_handoff` SHALL carry `mode: fork` for a fork and no mode for a continuation.

#### Scenario: The journal of a fork
- **WHEN** a node forks warm
- **THEN** its `session_handoff` has `warm: true` and `mode: fork`
