# Decision records

A decision record states one rule that holds across several files of apb, why
it holds, and the tests that fail when it is broken. A newcomer, a reviewer or
an agent reads the folder to learn the rules that no single file shows.

## Rules for the folder

1. One rule per record. The title is the rule itself, as a sentence.
2. A record is edited in place when the rule changes, and deleted when it no
   longer guards anything. History lives in git, not in superseded records.
3. Numbers are never reused, so a link to a deleted record fails loudly
   instead of pointing at another rule.
4. The folder stays small enough to read in full in one sitting. A rule that
   only one file enforces belongs in that file's comments, not here.

## Format

Every record starts with front matter:

```yaml
---
decided: YYYY-MM-DD
decision-makers: who agreed to it
---
```

followed by the rule as the title and three sections:

- **Applies to**: the code, surfaces and changes the rule covers.
- **Why**: what breaks without it.
- **Checked by**: the tests that fail when the rule is broken, named as
  `<crate>` `<test binary module>::<test>`. A record whose rule no test checks
  says so and names the review step that does.

`CLAUDE.md` and `AGENTS.md` link a record from the bullet that names its rule.

## Records

| Record | Rule |
|---|---|
| [0001](0001-new-events-are-safe-to-skip.md) | A new event type is safe to skip up to the next checkpoint; new fields are `#[serde(default)]` |
| [0002](0002-post-start-reads-come-from-the-manifest.md) | Post-start reads come from the write-once run manifest |
| [0003](0003-the-run-permit-reaches-the-engine-verbatim.md) | The run permit reaches the engine verbatim |
| [0004](0004-every-agent-spawn-gets-its-env-through-spawn-env.md) | Every agent spawn gets its environment through `adapter::spawn_env` |
| [0005](0005-agent-knowledge-lives-in-its-module.md) | Knowledge about a built-in agent lives only in its module |
| [0006](0006-a-built-in-form-without-machine-output-has-drifted.md) | A built-in form without its machine output means the format changed; a custom form read as plain text is allowed |
| [0007](0007-claude-headless-keeps-json-output.md) | Claude's headless form keeps `--output-format json`; tool exposure is checked on the stream transport only |
