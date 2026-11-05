# Builder check

Run it once the record of a risky design is written. It finds the places
where an implementer planning from the record would have to guess, while the
issuer can still answer.

## 1. The reader

Use a fresh context that did not write the design: a subagent, another
agent, or a clean session, by the same fallback chain as the advisor. Give it
the record and this prompt, nothing else. Without any fresh context, read the
record yourself as that implementer, ignoring what you know from the pass,
and say in the record that the check was not independent.

```
You are the engineer who must plan the implementation of the design in
[RECORD]. You cannot ask its author anything.

Report only the questions you would still need answered before you could
plan this well: places where you would have to guess and a wrong guess would
change what gets built. For each, give the question and why the answer
changes the plan. Rank them by importance.

Skip what the record explicitly leaves to the implementer, what you could
reasonably decide yourself, and wording.

If nothing important is left, answer "nothing important left".
```

## 2. Sort the answers

- **Already answered** by the task, the code or the pass: write the answer
  into the record.
- **Minor**: a sensible implementer could settle it without changing what
  gets built. Settle it and mark it in the record as your call.
- **The issuer's**: only the issuer can answer it, and the answer changes
  what gets built. Collect these.

## 3. One round

Send the issuer's questions in a single message, most important first. Write
the answers into the record. Return it with the short list of your own calls
so the issuer can check them. The check produces no further round. When the
issuer is unavailable, follow section 5 of the skill.

Record in the provenance who read the record, its rung, and how many
questions fell into each group.
