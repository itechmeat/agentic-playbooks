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
as information: the work continues, nobody has to approve the list. The
check produces no further round.

- **Issuer unavailable**: no new advisor round. A reversible choice becomes
  your own call, recorded as an assumption with its consequence at the top.
  A fact only the owner has, or permission for an irreversible effect,
  stays an open gap; only the work that depends on it pauses.
- **A caller that takes one question** (a run step with a single `question`
  output): settle what you can as above. When a gap still blocks, ask its
  most important question there and list the others in the record's next
  action. When nothing blocks, the step stays ready and the question is
  empty.
- Update the next action of the record to what is true after the check.

Record the check in the provenance apart from the advisor: who read the
record, its rung, whether it was independent, and how many questions fell
into each group.
