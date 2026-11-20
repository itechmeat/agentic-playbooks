## ADDED Requirements

### Requirement: A list visit runs its pilot slice first
A list node with `pilot` SHALL run the first `pilot.items` items of a visit that still need to run, in source order, journal them as `list_pilot`, and SHALL NOT admit any other item of the visit before the pilot is decided or passed.

#### Scenario: Three of forty files first
- **WHEN** a list of 40 items with `pilot: { items: 3 }` starts a visit
- **THEN** exactly the first 3 items run
- **AND** the run waits on a review request of the list node with the options continue, revise and stop

#### Scenario: A rejected pilot item retries inside the slice
- **WHEN** a pilot item is rejected and has a retry left
- **THEN** it runs again before the gate opens and the slice still holds the same items

#### Scenario: The batch size does not cut into the slice
- **WHEN** a list with `pilot: { items: 3 }` and `batch: { size: 2 }` starts a visit
- **THEN** a batch of 2 items and a batch of 1 item run, and the gate is asked after exactly those 3 items

### Requirement: The pilot gate decides how the visit goes on
A pilot decision SHALL be `continue` (the other items run), `revise` (the slice items run again with the reviewer's note in every later item prompt of the visit, inside the untrusted data fence the item section uses and under a line that names the decision's channel, never claiming that a person wrote it) or `stop` (the visit ends, the node fails, the other items stay pending); the review channel SHALL refuse any other option for a pilot gate.

#### Scenario: Revise with a note
- **WHEN** a person answers revise with the note "keep the greeting short"
- **THEN** the three slice items run again and their prompts end with a reviewer note section holding that note
- **AND** the section reads "The reviewer of the first items of this list asked for this change (decided through {channel})." with the note fenced as untrusted text

#### Scenario: Stop
- **WHEN** a person answers stop
- **THEN** the node finishes failed with `stopped.reason` "pilot" in its output and the items outside the slice are pending

#### Scenario: Approve is not a pilot option
- **WHEN** a decision approve is posted for a pilot gate
- **THEN** it is refused as invalid

#### Scenario: A pilot gate takes no fields
- **WHEN** a decision continue naming the open pilot request is posted with a field value
- **THEN** it is refused `gate <node> takes no fields` and nothing is written to the review channel

### Requirement: Revise never repeats a send whose outcome is unknown
`revise` SHALL NOT interrupt a slice item whose send has an unknown outcome: the item SHALL keep its `needs_look` record, stay in the new slice as needing a look without running, and be counted by the gate as needing a look. A crash partway through the revise transition SHALL resume it so that every other slice item is interrupted once and exactly one slice of the next round is journaled.

#### Scenario: An item with an unknown send
- **WHEN** a pilot slice of three items holds one whose send is unknown and a person answers revise
- **THEN** the other two items run again, the third gets no new settlement and no attempt, the next slice names all three, and its gate counts the third as needing a look

#### Scenario: A crash inside revise
- **WHEN** the drive crashes after interrupting the first slice item for revise and before the next slice is journaled
- **THEN** the resumed drive interrupts the remaining items once, journals one slice of the next round and never interrupts the item with the unknown send

### Requirement: A pilot decision answers only the request it names
A decision on a pilot gate SHALL name the journal seq of the request it answers, and every instruction, hint and printed command for the gate SHALL carry that request (rendered by one helper); the review channel SHALL refuse one that names no request, or a request the gate replaced or withdrew, with `decide_again`, and the drive SHALL consume only a channel entry stamped with the gate's open request, never an entry by its position in the channel.

#### Scenario: A decision made before a withdrawal
- **WHEN** a person posts continue naming pilot request A, a supervisor directive then withdraws A, and the list reaches its pilot gate again with request B
- **THEN** the run keeps waiting at B, A's entry is never consumed and `apb doctor --run` lists it
- **AND** a continue that names no request is refused `decide_again`, and a continue naming B is consumed

#### Scenario: Every printed command names the request
- **WHEN** a run waits at a pilot gate, or is paused there
- **THEN** the journaled instruction, the pending block's mechanics and the hint `apb wait` prints each carry `--request review:<seq>` of the open request, and the command followed as printed is accepted

#### Scenario: Two decisions for one request
- **WHEN** two decisions naming the same open pilot request are posted
- **THEN** the second is refused and the journal holds one `review_decided` for the request

### Requirement: A pilot that passed goes on by itself only when asked
With `then: continue` a visit SHALL go on without a gate when every slice item passed, and SHALL ask a person when a slice item needs a look; with `then: review` it SHALL always ask.

#### Scenario: A clean pilot with then continue
- **WHEN** every slice item passes under `then: continue`
- **THEN** the other items run and no review request is journaled

### Requirement: An unattended run never waits at a pilot gate
In an unattended run a pilot gate SHALL fail the run (`hitl_required`) under `unattended: fail`; under `unattended: continue` it SHALL be decided `continue` by the engine, journaled with `decided_by: unattended`, only when every slice item passed, and SHALL fail the run (`hitl_required`) when a slice item needs a look.

#### Scenario: Unattended fail
- **WHEN** an unattended run reaches a pilot gate with `unattended: fail`
- **THEN** the run ends failed with `hitl_required`

#### Scenario: Unattended continue over a clean slice
- **WHEN** an unattended run reaches a pilot gate with `unattended: continue` and every slice item passed
- **THEN** the engine decides continue and the other items run

#### Scenario: Unattended continue over a bad slice
- **WHEN** an unattended run reaches a pilot gate with `unattended: continue` and a slice item needs a look
- **THEN** the run ends failed with `hitl_required`

### Requirement: The pilot survives every way a visit continues
The pilot slice, its round and its decision SHALL be read from the run journal through the list fold, so a resume, a supervisor retry and `rerun: not_passed` SHALL neither run a decided pilot again nor skip one that is not decided.

#### Scenario: Resume after the gate
- **WHEN** a run paused at the pilot gate is resumed and the gate is answered continue
- **THEN** no slice item runs a second time

#### Scenario: Supervisor retry of a failed visit
- **WHEN** a supervisor retries a list node whose visit failed after its pilot was continued
- **THEN** the reopened visit runs no new pilot

### Requirement: A resumed visit completes its opening steps first
A resumed visit with no batch and no pilot slice since its `list_started` SHALL first complete the steps that open it (a reopen's requeue, the carry of `rerun: not_passed`, the parking of items with an unknown send), decided on the journal as it stood before that `list_started` and skipping every item already settled since.

#### Scenario: A crash before the carry
- **WHEN** a drive under `rerun: not_passed` journals a new visit's `list_started` and dies before it carried the passed items, and the run is resumed
- **THEN** the resumed visit carries the passed items and parks the items with an unknown send before it selects the pilot slice, so the slice holds only the items that still need to run and no carried item runs again

#### Scenario: A crash before the requeue
- **WHEN** a supervisor retry's drive journals the reopening `list_started` and dies before it requeued the items that need a look, and the run is resumed
- **THEN** the resumed visit requeues them and they run again

### Requirement: A visit ends early when too many items need a look
A list node with `stop_when` SHALL end the visit when `needs_look_at_least` or `needs_look_share` (once `min_settled` items settled) holds over the items settled outside the latest pilot slice: it SHALL journal `list_stopped`, finish the node failed with `stopped` in its output, and leave the items it did not run pending.

#### Scenario: Ten rejected drafts
- **WHEN** 10 of the settled items of a 200-item mail list need a look under `needs_look_at_least: 10`
- **THEN** no further batch starts and the node fails with `stopped.reason` "stop_when"

### Requirement: The early stop never overrides a batch or a pilot
The `stop_when` rule SHALL be read before a batch is admitted and after a batch whose items all settled; a batch that ended in a pause, a halt, a stop or a spent budget SHALL be honoured first, and the rule SHALL NOT be read while the pilot slice is incomplete or its gate is due or open.

#### Scenario: The rule waits for the pilot gate
- **WHEN** a list of 5 items with `pilot: { items: 3 }`, `batch: { size: 2 }` and `stop_when: { needs_look_at_least: 1 }` has its first item rejected
- **THEN** all 3 slice items run, the pilot gate opens, and no `list_stopped` is journaled
- **AND** after continue the 2 other items run, and the slice's rejected item does not count toward the rule

#### Scenario: A paused batch is not overridden
- **WHEN** a pause stops a batch whose settled items already meet the rule
- **THEN** the run pauses with no `list_stopped` and the unsettled item stays pending
- **AND** on resume the visit ends `stop_when` before another batch is admitted

#### Scenario: A crash after the stop
- **WHEN** the drive crashed after `list_stopped` and before the node's `node_finished`
- **THEN** the resumed drive finishes the node failed with the same `stopped` and runs no item

### Requirement: The pilot gate shows its slice
A pilot gate SHALL be a plain review request on the list node: it SHALL journal no `review_detail`, and its pending review SHALL carry the pilot from the list fold (the round, the slice ids, how many passed, how many need a look and how many items are left).

#### Scenario: The pending pilot gate
- **WHEN** a pilot slice of 3 items settled and the gate waits
- **THEN** the pending review carries the pilot round 1 with the three ids
- **AND** the journal holds no `review_detail` of the list node

### Requirement: A scenario answers a pilot gate
A scenario SHALL be able to answer a pilot gate with a review answer on the list node, in the same visit answer as the node's item answers: `items` stays the list node's own answer kind and `review` is also taken by a node with a pilot. Its validation SHALL refuse a pilot answer that is not continue, revise or stop, and a review answer on a list node without a pilot.

#### Scenario: Items and a scripted continue in one visit
- **WHEN** a quick scenario run's answer for a pilot list node's visit carries its item answers and the review answer continue
- **THEN** the scenario validates, the other items run, and the scenario reports no missing and no extra answer

#### Scenario: Not a pilot option
- **WHEN** a scenario answers a pilot gate with approve
- **THEN** validation refuses it as V148

### Requirement: A run with a pilot is guarded against older binaries
A run whose list node uses `pilot` or `stop_when` SHALL carry the run guard `list_pilot`. Every older binary SHALL refuse to resume such a run waiting at a pilot gate and SHALL append nothing to its journal. apb 1.0.0-beta.1 and later releases older than this change cannot parse such a definition (their list block denies unknown fields), so they refuse to start such a playbook.

#### Scenario: A recent older binary starts a playbook with a pilot
- **WHEN** apb 1.0.0-beta.1 is asked to start a playbook whose list node has `pilot`
- **THEN** it exits with an error and creates no run directory

#### Scenario: An older binary at the pilot gate
- **WHEN** an apb older than this change is asked to resume a run waiting at a pilot gate
- **THEN** it refuses and the journal is byte for byte what it was before

### Requirement: apb 0.25.0 drops a pilot as it drops every new field
apb 0.25.0 accepts unknown fields in a list block: it drops `pilot` and `stop_when` and runs every item with no pilot gate and no early stop, the standing contract of an older binary meeting a new playbook field. No further guard or mode suffix SHALL be added for it, and the documentation SHALL say so.

#### Scenario: apb 0.25.0 starts a playbook with a pilot
- **WHEN** apb 0.25.0 is asked to start a playbook whose list node has `pilot`
- **THEN** it starts the run and runs every item with no pilot gate and no `list_pilot` event
