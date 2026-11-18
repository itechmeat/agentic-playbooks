## ADDED Requirements

### Requirement: A gate takes typed values from the reviewer
A `human_review` with `fields` SHALL take, with a decision, a value per field of type text, number, bool or enum, defaulted by a template rendered once per request, required for every option or for the options listed; values SHALL be accepted through MCP `review_decide`, `apb review --field`, the HTTP review route and the dashboard panel.

#### Scenario: A corrected invoice amount
- **WHEN** a person approves an invoice gate with `amount` 1180.5 where the default was 1108.5
- **THEN** the next node reads 1180.5 as `{{nodes.confirm.output.amount}}`

#### Scenario: Required only for approve
- **WHEN** a field is `required: [approve]` and has no default
- **THEN** a reject without a value is accepted and an approve without one is refused

#### Scenario: A cleared required field
- **WHEN** an approve carries an empty or whitespace-only text for a field `required: [approve]`, through MCP, `apb review --field`, the HTTP route or the dashboard, whether or not the field has a default
- **THEN** the one shared check refuses it as missing on every surface

### Requirement: Values are checked twice and scrubbed
The review channel SHALL refuse a value of the wrong type, an unknown field or a missing required value; the drive SHALL check the values again when it consumes the decision and end the run `review_fields_refused` on a failure; the values SHALL be scrubbed of the run's secret values before they are journaled or output;

#### Scenario: A word for a number
- **WHEN** a decision carries `amount: "ten"`
- **THEN** it is refused as invalid

#### Scenario: A default that does not fit
- **WHEN** a field's rendered default is no value of its type and a decision the field is not required for is posted
- **THEN** the decision is accepted and the field reads null

#### Scenario: A hand-written channel line
- **WHEN** a line with invalid field values is appended to the review channel by hand
- **THEN** the run ends failed with `review_fields_refused`

### Requirement: No part of a secret leaves through a refusal or a cut
A refusal SHALL name the field and never the value, the reason a run ends on SHALL be scrubbed and bounded, a `show:` text or a default, templated or literal, SHALL be rendered whole and scrubbed before it is cut, every other text of the request's detail (a `show:` label, an enum option, a default's problem) SHALL go through the same scrub and cut, and a number SHALL read only as a whole decimal number.

#### Scenario: A secret in a refused value
- **WHEN** a hand-written channel line carries a secret param's value in a number field
- **THEN** the run ends `review_fields_refused` and the journal does not contain the value

#### Scenario: A token in a literal
- **WHEN** a field's literal default, a `show:` label or an enum option holds a token-shaped string
- **THEN** the request's detail holds the redaction mark in its place, and a literal default past 2000 bytes is cut there

#### Scenario: A secret under a small context budget
- **WHEN** a gate with `context: { output_max_bytes: 8 }` shows an earlier output that holds a secret
- **THEN** no part of the secret is shown or journaled

#### Scenario: Letters after a number
- **WHEN** a person types "12abc" into a number field on the dashboard
- **THEN** nothing is posted and the panel names the field, never posting 12

#### Scenario: A bool nobody chose on the dashboard
- **WHEN** a gate's bool field has no default and a person decides on the dashboard without choosing a value for it
- **THEN** the panel shows the field as not chosen and posts no value for it, as `apb review` and `review_decide` do, so a decision the field is required for is refused naming the field

#### Scenario: A secret typed into a field
- **WHEN** a person types a secret param's value into a text field
- **THEN** the post is refused naming the field, and neither the review channel, the journal nor the gate's output contains it

### Requirement: A value the run keeps secret is refused when posted
A posted field value that the run's secret scrub would change SHALL be refused at post time on every channel, before the channel entry is appended, with a refusal that names the field and says it holds a value this run keeps secret, never the value; while the run's secrets cannot be resolved, a post of a non-empty text value SHALL be refused the same way.

#### Scenario: A secret inside a remark
- **WHEN** a decision posts a remark that holds a secret param's value among other words
- **THEN** the refusal says the field holds a value this run keeps secret and `reviews.jsonl` does not contain the value

#### Scenario: Secrets that do not resolve
- **WHEN** the run's configuration does not read and a decision posts a text value
- **THEN** the post is refused and the value is not written; once the configuration reads, the same post is taken

### Requirement: A gate with fields outputs one JSON object
The output of a gate with fields SHALL be one JSON object with `decision` and every field, read by templates as `nodes.<gate>.output.<field>` and by `output_field` edges; `review_status` routing SHALL be unchanged.

#### Scenario: Routing on a field
- **WHEN** an `output_field` edge reads `amount` greater than 1000
- **THEN** it matches the decided value

### Requirement: Shown outputs and defaults are marked when untrusted
A `show:` entry or a field default that renders untrusted text SHALL be marked untrusted on every surface, a field default over untrusted text SHALL make the gate's output untrusted for the nodes that read it, and the validator SHALL warn about such a default (V247).

#### Scenario: A default from a fetched page
- **WHEN** a field defaults to the output of a node bound to an untrusted connector
- **THEN** its view is marked untrusted, the CLI and the dashboard mark the field so, and an agent reading the gate's output is an untrusted follower

### Requirement: A passkey covers the values
While a passkey is enforced, the assertion that decides a gate with fields SHALL be bound to the field values, so a decision with other values is not consumed; a decision without fields SHALL keep today's challenge.

#### Scenario: Values changed after the assertion
- **WHEN** a decision carries values other than the ones its assertion was minted for
- **THEN** the drive does not consume it

### Requirement: An older apb does not continue such a run
A run whose definition uses `fields`, `show` or `max_scope` SHALL carry the run guard names `review_fields` or `review_max_scope`.

#### Scenario: The guard names the feature
- **WHEN** a run of a playbook with a fielded gate starts
- **THEN** its `run_guard` names `review_fields`

### Requirement: A refusal repeats no posted value or definition text
No refusal the review channel returns or the drive journals SHALL carry a posted value, a posted key, the decision word or a field's options; it SHALL name the field and the kind of value it takes, scrubbed of the run's secret values.

#### Scenario: A token-shaped option
- **WHEN** an enum field's options hold a token-shaped string and a decision posts a value that is not one of them
- **THEN** the refusal names the field and says the value is not one of its options, and holds neither the token nor the posted value

### Requirement: A validator message repeats no definition literal
No message of V243 to V247 SHALL repeat a literal default, an option or a field name outside the field-name grammar: it SHALL name the node and the field by its name when the name fits the grammar and is not shaped like a credential, else by its position, and say what is expected.

#### Scenario: A token in the definition
- **WHEN** a gate's field name, enum options, literal default, `required` entry and `max_scope.withhold` entry hold a token
- **THEN** every V243 to V247 message names the field or the entry by its position and none holds the token
