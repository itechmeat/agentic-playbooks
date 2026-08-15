# Playbook interview (tier 2)

This is the on-demand guide an agent pulls via `playbook_interview` when the
user wants to automate a process by describing it in conversation. It is the
counterpart of `HOWTO-authoring.md`: that document covers the playbook YAML
format, this one covers the conversation with a person. Pull
`playbook_howto` as well before assembling the result.

## When to run an interview

Offer an interview when the user describes a recurring process they want
automated and no existing playbook covers it (check the catalog first; if a
match exists, offer the existing playbook instead of creating a duplicate).
The interview replaces nothing the user already did: if they just performed
the action by hand, `playbook_capture` is the right path, not an interview.

## Ground rules

- The conversation happens in the user's chat language. This document is
  English; your questions and summaries are not.
- Questions are for an ordinary employee: plain language, about the process
  itself. Never ask the person about nodes, profiles, models, or connectors.
  You make those technical decisions yourself from what is already configured
  in the project, and surface them only inside the final playback. When the
  person answers in technical terms, use their precision; there is no
  separate mode.
- One question per message. If a topic needs more, split it into several
  messages.
- Never invent a step the person did not name. A gap is a question, not a
  guess. Anything unclear is asked again, not filled in.
- Every point where the person chose among alternatives by feel is a choice
  with an unknown rule: clarify it, or record it as an explicit
  ask-a-person gate.

## Question flow

Work through these blocks in order.

### 1. The task and its trigger

What task to automate, how often it occurs, what starts it. Example opening:
"Tell me about the task you want to automate. How often does it come up, and
what usually kicks it off?"

### 2. Data sources

Which emails, files, sheets, pages are involved; what the person opens and
reads; where results are written.

### 3. Steps as a story

"Walk me through it as if teaching a new colleague." Listen to the whole
story, then probe the gaps. Do not interrupt with structure.

### 4. Goal and criteria (mandatory)

"How do you yourself know you did this right?" The answers become the
playbook's `goal` field: a statement in the owner's words plus verifiable
criteria (a row appears in the sheet, the email is in Sent). Each criterion
gets a check kind: `manual` when only a person can confirm it, `marker` when
the run result can carry a marker string, `script` when a check script could
confirm it. An interview without this block is not finished and must not
produce a draft.

### 5. Human gates

What the person would never trust to run without confirmation: sending,
paying, deleting, anything irreversible. These become `human_review` gates in
the playbook.

### 6. Variables vs constants

What changes run to run (an invoice number) versus what is fixed (the
recipient). Variables become playbook params.

### 7. Exceptions

"Does it ever go differently? What do you do then?" Each answer becomes a
branch, or an honest ask-a-person gate when the rule is unclear.

## Playback and confirmation

Before building anything, play the understood process back in plain language:
the steps, the goal and its criteria, the points where the playbook will ask
the person. Ask for an explicit yes. If the person corrects anything, update
and play back again. Only after the yes do you assemble the playbook.

## Assembling the draft

1. Reuse profiles: call `profile_list` and pick fitting ones. Create a new
   profile through the existing profile flow only if none fits.
2. Create the draft with `playbook_create`. Machine fields (ids, trigger,
   effects) are English; display names and descriptions follow the user's
   language. Include the `goal` field from block 4.
3. Validate. If validation fails, fix the playbook yourself and revalidate;
   validator errors are never the person's problem.
4. Offer a trial through the normal trust path. The draft is never run
   without the person going through the standard trial and approval
   mechanics.

## Edge cases

- **Interrupted interview.** The interview can stop and resume later; on
  resume, replay what was already established and continue from the first
  unanswered block.
- **Too vague to automate.** If after blocks 3 and 7 the process has no
  stable shape, say so honestly and offer to narrow the scope to the stable
  core, rather than produce a playbook of guesses.
- **Already covered.** If at any point the described process turns out to be
  covered by an existing playbook, say so and offer it.
