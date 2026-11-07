# untrusted-input Specification

## Purpose
TBD - created by archiving change recurring-routines. Update Purpose after archive.

## Requirements

### Requirement: Untrusted markers
A playbook SHALL be able to mark untrusted text with `untrusted: true` on a param, on a node's connector binding, and on an `agent_task` or `script` node whose own attempt reads outside text.

#### Scenario: A binding keeps its marker
- **WHEN** a binding `{ name: github, untrusted: true }` is loaded and saved
- **THEN** the saved definition still holds `untrusted: true` on it

### Requirement: Taint follows the text
A node SHALL count as rendering untrusted text when it carries a marker, reads an untrusted param, reads a tainted node's output, report, rejected output or seen keys, or reads any earlier run's node output through `run.previous_success.nodes.*`. Its own output SHALL then be tainted unless it is a judge, a review, a condition, a wait or a start.

#### Scenario: Two hops through outputs
- **WHEN** node `a` binds an untrusted connector, `b` renders `{{nodes.a.output}}` and `c` renders `{{nodes.b.output}}`
- **THEN** `c` renders untrusted text

#### Scenario: An earlier run's output is untrusted
- **WHEN** a prompt reads `{{run.previous_success.nodes.draft.output}}` and `draft` carries no marker now
- **THEN** that node renders untrusted text

#### Scenario: A judge's answer carries no taint
- **WHEN** a judge reads a tainted node and a later prompt reads the judge's answer
- **THEN** the later node does not render untrusted text through it

### Requirement: Taint through lists, sessions, evidence and context
A node SHALL also count as rendering untrusted text when its list items, its continued session or its evidence come from a tainted node, when its template-form `writes` reads an untrusted param or a tainted node's field, or when it reads `run.context` or `run.retro`, or is a finish prompt that gets the terminal context appended, while any param is untrusted or any node is tainted.

#### Scenario: List items carry taint
- **WHEN** a list node takes its items from a tainted node's field
- **THEN** the list node renders untrusted text

#### Scenario: A write scope rendered from untrusted text
- **WHEN** an `agent_task` has `writes: "{{nodes.fetch.output.paths}}"` and `fetch` is tainted
- **THEN** that node renders untrusted text

#### Scenario: The run context holds an untrusted param
- **WHEN** a param is untrusted and a prompt reads `{{run.context}}`
- **THEN** that node renders untrusted text

### Requirement: V179 for an agent that renders untrusted text and can send outward
The validator SHALL report V179 (error) for an `agent_task`, a finish with a prompt or a `playbook` node that renders untrusted text and can send outward, unless the node has `accept_untrusted`. An `agent_task` SHALL count as unable to send outward only with `isolation: { level: full, network: none }`, no connector binding and no declared network, external or irreversible effect.

#### Scenario: An unisolated agent reads an issue body
- **WHEN** an `agent_task` with no isolation renders an untrusted param
- **THEN** validation fails with V179

#### Scenario: A sandboxed agent without network
- **WHEN** the same node has `isolation: { level: full, network: none }` and no connector
- **THEN** V179 is not reported

#### Scenario: An accepted risk
- **WHEN** the unisolated node has `accept_untrusted: "labels come from params"`
- **THEN** V179 is not reported

### Requirement: V180 for an agent that also reaches secrets
The validator SHALL report V180 (warning) for a node that renders untrusted text and renders a secret param, binds a connector, or declares the `secrets` effect, whether or not it accepts the risk.

#### Scenario: An accepted node with a connector
- **WHEN** an accepted node that renders untrusted text binds a connector
- **THEN** validation warns with V180

### Requirement: V181 and V182 for misplaced markers
The validator SHALL report V181 (error) for `untrusted` on a node that is not `agent_task` or `script`, a param that is both `secret` and `untrusted`, and `accept_untrusted` on a node of another kind or with a reason that is empty, whitespace only or over 300 characters, and V182 (warning) for `accept_untrusted` on a node that renders no untrusted text or cannot send outward.

#### Scenario: An empty reason
- **WHEN** a node has `accept_untrusted: " "`
- **THEN** validation fails with V181

#### Scenario: A stale acceptance
- **WHEN** a node with `accept_untrusted` renders no untrusted text
- **THEN** validation warns with V182

### Requirement: Agents are told which text is untrusted
Every attempt of a node that renders untrusted text SHALL end its prompt with a fixed `## untrusted input` section naming the sources: after the connector block, the write scope note, the item section, the retry briefing and the evidence section, and outside the cache-keyed render.

#### Scenario: The section is last
- **WHEN** a list node that binds a connector and has `evidence` renders untrusted text
- **THEN** every attempt's prompt ends with the `## untrusted input` section

#### Scenario: The section stays out of the cache key
- **WHEN** a node with `cache: auto` renders untrusted text
- **THEN** the prompt its agent gets ends with the section
- **AND** the rendered text its cache key is derived from does not hold it

### Requirement: An older apb does not continue such a run
A run whose definition has a node that renders untrusted text SHALL carry a `run_guard` naming `untrusted_input`.

#### Scenario: The guard is journaled
- **WHEN** a run starts whose agent renders an untrusted param
- **THEN** its journal has a `run_guard` whose `requires` holds `untrusted_input`

### Requirement: The run surfaces list the untrusted agents
A start SHALL record each node that renders untrusted text, with its sources, outward reasons, secrets and accepted reason, on `run_provenance.untrusted`, and `apb runs <id>`, MCP `run_status`, the dashboard's run view and the HTML report SHALL show them.

#### Scenario: The accepted reason is shown
- **WHEN** a run's definition accepts the risk on node `classify`
- **THEN** `apb runs <id>` prints `untrusted input: node `classify` renders ...; accepted: <reason>`

### Requirement: The dashboard keeps binding keys
The dashboard's connector editor SHALL keep a binding's `on_unknown` and `untrusted` when it saves a node.

#### Scenario: Editing another binding
- **WHEN** a node binds `{ name: mail, on_unknown: resend, untrusted: true }` and a person edits another binding of the node
- **THEN** the saved definition still holds both keys on `mail`
