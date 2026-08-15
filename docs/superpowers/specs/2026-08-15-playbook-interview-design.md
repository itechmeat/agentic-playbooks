# Playbook Interview: guided creation by describing, design

Status: design, approved for planning. Date: 2026-08-15.

## What this is

A skill that lets a person create an apb playbook by describing the desired
process in a short guided interview, instead of writing YAML or showing the
process by hand. The person says "I want to automate this task"; the agent
they already talk to (Claude Code, hermes in a messenger, codex, any agent
connected to the apb MCP server) pulls an interview guide from apb, asks
plain-language questions one at a time, plays the understood process back for
confirmation, and assembles a draft playbook through the existing apb tools.
The draft enters the normal registry and the existing trial and trust path
owns it from there.

This is the reverse entry point to the deferred process recorder
(`docs/superpowers/specs/2026-08-10-process-recorder-design.md`): the recorder
turns "showed it by hand" into a playbook; the interview turns "described it
in words" into a playbook. Both converge on the same artifact, a draft
playbook carrying an explicit goal with verifiable criteria. The goal schema
designed for the recorder is implemented here; when the recorder is
reactivated it reuses the field as designed.

## Scope

Three pieces, all in existing crates, no new crates:

1. A first-class `goal` field on the playbook schema plus a validator rule
   (apb-core).
2. An interview guide document, `docs/HOWTO-interview.md`, the agent-facing
   instruction for running the interview (repository docs).
3. A new MCP tool `playbook_interview` that returns that guide, plus an
   update to the MCP server instructions so agents know when to offer the
   interview (apb-mcp).

Playbook assembly itself uses only existing tools (`profile_list`,
`playbook_create`, `playbook_validate`, `playbook_trial`); no new assembly
tooling.

### Explicitly not in this project

- Any capture or observation of user actions (deferred recorder).
- Updating an existing playbook via interview (create-only in v1; lifecycle
  edits keep going through the existing update tools).
- Goal-directed automatic repair by the supervisor (the `goal` field is the
  prerequisite; the repair behavior is a separate later project).
- Machine execution of `script` goal checks by the engine (the schema names
  the check kinds; wiring them into run verdicts is later work).
- Any UI. The interview lives entirely in the chat the person already uses.

## The goal field (apb-core)

New optional field on `Playbook`, additive, no migration; playbooks without a
goal remain fully valid.

```rust
#[serde(tag = "type", rename_all = "snake_case")]
pub enum GoalCheck {
    Manual,
    Marker { marker: String },
    Script { path: String },
}
// GoalCheck defaults to Manual when omitted on a criterion.

pub struct GoalCriterion {
    pub description: String,
    pub check: GoalCheck,
}

pub struct Goal {
    pub statement: String,
    pub criteria: Vec<GoalCriterion>,
}

// on Playbook:
#[serde(default, skip_serializing_if = "Option::is_none")]
pub goal: Option<Goal>,
```

Semantics: `statement` is the goal in the owner's words ("the invoice is
recorded in the tracking sheet and sent for approval"); each criterion is one
verifiable fact with a check kind, `manual` (a person confirms), `marker` (a
marker string is expected in the run result), or `script` (a check script
path, execution wired up later).

New validator rule (next free V-code at implementation time): when `goal` is
present, `statement` must be non-empty, `criteria` must contain at least one
entry, and every criterion `description` must be non-empty. Severity: Error.
Playbooks without `goal` are unaffected.

`docs/HOWTO-authoring.md` gains a short section documenting the field.

## The interview guide (docs/HOWTO-interview.md)

Agent-facing instructions, written in English like all repository docs, and
explicitly requiring the conversation itself to happen in the user's chat
language. The guide is the counterpart of `HOWTO-authoring.md`: that one
covers the YAML format, this one covers the conversation with a person.

Audience calibration, stated as a hard rule in the guide: questions are for an
ordinary employee, in plain language, about the process itself. The agent
never asks the person about nodes, profiles, models, or connectors; it makes
those technical decisions itself from what is already configured in the
project and surfaces them only inside the final playback. When the person
answers in technical terms, the agent simply uses their precision; no separate
mode.

Question flow, one question per message, in this order:

1. **The task and its trigger.** What task to automate, how often it occurs,
   what starts it.
2. **Data sources.** Which emails, files, sheets, pages are involved; what the
   person opens and reads.
3. **Steps as a story.** "Walk me through it as if teaching a new colleague."
   The agent listens and probes gaps; it does not interrupt with structure.
4. **Goal and criteria, mandatory.** "How do you yourself know you did this
   right?" The answers become the `goal` field: a statement plus verifiable
   criteria. An interview without this block is not finished and must not
   produce a draft.
5. **Human gates.** What the person would never trust to run without
   confirmation (sending, paying, deleting); these become `human_review`
   gates.
6. **Variables vs constants.** What changes run to run (an invoice number)
   versus what is fixed (the recipient).
7. **Exceptions.** "Does it ever go differently? What do you do then?" Each
   answer becomes a branch or an honest "ask a person here" gate.

Then **playback and confirmation**: the agent returns a human-readable
summary, "here is what I understood: these steps, this goal, we ask you at
these points", and only after an explicit yes assembles the playbook: reuse
profiles from `profile_list` (create one via the existing profile flow only
if none fits), create the draft with `playbook_create`, run validation, and
offer a trial through the existing trust path. The draft is never run without
the person going through the normal trial and approval mechanics.

Honesty rules, carried over from the recorder design:

- Never invent a step the person did not name; a gap is a question, not a
  guess.
- Anything unclear is asked again, not filled in.
- Every point where the person chose among alternatives "by feel" is marked
  as a choice with an unknown rule and clarified, or recorded as an explicit
  ask-a-person gate.

## Error handling and edge cases

- **Interrupted interview.** The interview can stop and resume later; on
  resume the agent replays what was already established and continues from
  the first unanswered block. No persistence beyond the chat itself is
  required in v1.
- **Duplicate coverage.** If the described process is already covered by an
  existing playbook (the agent checks the catalog), say so and offer the
  existing one instead of creating a duplicate.
- **Too vague to automate.** If after the step and exception blocks the
  process has no stable shape, say so honestly and offer to narrow the scope
  to the stable core, rather than produce a playbook of guesses.
- **Assembly failure.** If validation of the assembled draft fails, the agent
  fixes the playbook (it is the author) and revalidates; validator errors are
  never surfaced to the person as their problem.

## Testing

- **Goal schema and validator rule (apb-core).** Plain unit tests: parse
  round-trips of playbooks with and without `goal`, each check kind, and the
  validator rule firing on empty statement, empty criteria, and empty
  criterion description, and staying silent otherwise.
- **MCP tool (apb-mcp).** A server-layer test that `playbook_interview`
  returns the embedded guide, mirroring the existing `playbook_howto` test.
- **The guide text itself.** An emulated interview: scripted "user" answers
  for a reference scenario (the synthetic invoice case) drive an agent
  session that must end in a valid draft playbook carrying a goal with at
  least one criterion. This is a manual acceptance check per release of the
  guide, not an automated CI test, since it exercises model behavior.

## Relation to the deferred recorder

The recorder spec and plan stay preserved with DEFERRED status. This project
deliberately implements the recorder plan's schema piece (its Task 10, the
`goal` field and validator rule) because it is entry-point independent. When
the recorder is reactivated, its plan should be re-baselined against the then
current schema: the goal field will already exist.
