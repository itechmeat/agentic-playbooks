# Corrections: the ledger of what people corrected, run after run

A run ends and a person's correction is used once: a review note, a note
posted to a node, an answer to a question, a success report apb rejected.
When the same correction is made in run after run, nothing offered to put
it into the playbook, and after a fix nothing told whether the correction
stopped. The corrections ledger keeps them per playbook, groups the
recurring ones, and offers a draft for a person to apply.

Corrections are collected at every run end, next to the playbook's known
problems (see KNOWN-PROBLEMS.md). The file is
`.apb/corrections/<scope>/<id>.json`, git-ignored, signed and scrubbed
exactly like known problems are. Nothing needs a playbook block: every
playbook's corrections are listed on the dashboard and by `apb
corrections`, and a playbook that never had a correction never gets a
file.

## What a run contributes

One pass over the run's journal, four kinds:

1. A person's review decision other than `approve` with a non-empty note
   (a review `reject` with "add tests for the parser").
2. A supervisor-channel note aimed at a node (`apb note` and an agent
   supervisor post the same action, so the ledger cannot tell them apart;
   such an entry is shown to people but never offers a draft).
3. A human's answer to an interactive question (`question -> answer`).
4. An attempt whose success report apb rejected (its rejection reason).

The ledger reads a run's journal only when it reads strictly whole: a
journal with an unknown event or a broken line is not counted, and a
scenario run is never counted.

## The key

Entries are grouped without a model. A rejection that carries an engine
fingerprint (the hash of its normalized reason) is keyed by that
fingerprint (`f:`). Everything else is keyed by the digest of the kind,
the node and the sorted distinct lowercase tokens of the scrubbed text,
with tokens holding a digit or a single character dropped (`t:`), so two
wordings that share their words share an entry: "Please add the parser
tests" and "add the parser tests, please" are one entry, and "fix line
12" and "fix line 40" are one entry. Every word counts: "Please add tests
for the parser" holds `for` and is a separate entry. Two wordings that do not share a key
stay two entries; a person dismisses the duplicate.

Every text goes through the scrubber's stored-text path before it is
kept, and nothing derived from a text the scrubber flags (a secret value,
a credential-shaped token) is ever stored: such a sighting is kept only
when a valid engine fingerprint gives its key, with no text, and is
dropped whole otherwise. The file holds neither the token nor a hash of
it.

## Statuses, and the draft

An entry is `open` until it recurs in 2 runs (`proposed`), and a person
moves it between `dismissed`, `fixed` and `reopened`:

- `open`: seen once.
- `proposed`: seen in 2 or more runs. It offers a draft: one line for the
  prompt of the node it names.
- `fixed`: a person marked the version that carries the fix.
- `reopened`: the rate per run did not fall on the fix version.
- `dismissed`: a person decided it is not a correction to act on.

The draft is one line quoting the leading wording, for the prompt of the
node the entry names:

- a review note: "A person corrected this step in earlier runs: <text>"
- a human answer: "When this step asks a question, people answered in
  earlier runs: <text>"
- a rejection: "Earlier attempts of this step were rejected by apb:
  <text>. Make sure the result passes that check before you report
  success."

No draft is offered when the node is unknown or ambiguous (a review gate
fed by several branches), has no prompt, the entry came through the
supervisor channel, or its text was withheld.

apb never applies a draft and never creates a version or candidate from
it. A person adds the line to the node's prompt through the existing ways
of changing a playbook (the dashboard editor, the version tools, or a
supervisor's forward patch that a person then promotes with `apb
candidate`), and then marks the version that carries it (below).

## Marking a fix, and the reopen rule

`apb corrections <playbook> fixed <entry> --version X.Y.Z` records that
this version carries the fix. The marking snapshots the base counts (the
version of the entry's latest sighting, and how many runs and hits it had
there), so the reopen rule never needs history the file no longer holds.

Once 3 runs of the fix version were observed, the entry is `reopened`
when its rate per run there is not lower than the base rate: base 2 of 2
with the correction in 1 of 3 fix runs stays `fixed`, with it in all 3 it
is `reopened`. Below 3 fix runs there is no judgment. Only the fix
version itself is counted.

## Who may read it

The ledger holds people's words and engine fingerprints and is shown only
to people. No prompt section, no supervisor line, no template variable
and no run surface reads it: every entry point, reads included, is
refused from a run step. On MCP, `playbook_corrections` lists and
`correction_decide` dismisses or reopens (on the person's explicit word
only); marking a fix is a statement about a version and is offered on the
CLI and the dashboard only.
