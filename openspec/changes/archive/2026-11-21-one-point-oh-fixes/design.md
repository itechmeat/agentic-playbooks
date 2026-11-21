# Design: one point oh fixes

Read `proposal.md` first. Every decision below names its files, its
functions and the line where the code is on main efae5822 (Wait park,
#290; apb 1.0.0-beta.7). Main holds typed-inbox-reads (#287),
people-in-the-loop (#288), before-running-alone (#289) and wait-park (#290),
all archived under `openspec/changes/archive/`.

Four changes are in flight and merge before or beside this one:
eval-doubles (implemented, in review), start-rules (being implemented),
incremental-lists-and-session-fork (spec ready) and inbox-waits (spec). The
1.0 plan also puts the drafts mcp-tool-connectors and mcp-http-and-surfaces
before it. None of them is a precondition of this change. Where they touch
the same files, the section "Shared files with the changes in flight" says
who owns what. Every line number here is to be re-verified on the main the
implementer starts from: find the site by the function or constant named
next to it, never by the number alone, and note every moved site in the
final report. A site whose function no longer exists is a stop: report
it, do not invent one.

The compatibility freeze, the template gallery, global playbooks and the
release candidate are the next change, one-point-oh-freeze. It is
generated from the main this change leaves, so what this change adds to
the schema (`project_checks.junit`) and the validator (V320) is frozen
there; nothing here writes a freeze fixture.

Two decision groups, built in the order D0, D4 (tasks.md). Nothing in this
document is left to the implementer: where a choice was open, it is
decided here with its reason; tasks.md only orders the work.

## Unchanged from main

Each fix goes through a path main already has, with other arguments. No
item adds a hand-over, a control, a recovery, a queue, a retirement, a
journal event, an event field, a manifest mark, a run guard or a signed
store. In particular these stay exactly as they are:

1. The send ledger and its flow: `connector/intent/` (the admission table,
   `reconcile::match_items`, `listing_args`), `connector/call/send_once.rs`,
   the marker stamp of `connector/intent/stamp.rs` (an absent marker arg
   becomes the marker alone, test `an_absent_marker_arg_becomes_the_marker_alone`)
   and the manifest rules of `apb_core::connector::idempotency`
   (`check_marker`, line 440). D4.1 only declares data in the gitlab
   manifest.
2. The baseline machinery: the rule `no_new_failures` and its comparison,
   `ReadCtx::tree_guarded` (`baseline/ctx.rs` line 58, the git
   fingerprint, which leaves ignored files out), `numbered_repeats`
   (`baseline/project.rs` line 150), `MAX_LIST_ITEMS`
   (`apb_core::baseline` line 35), the events `baseline_observed` and
   `baseline_compared`. D4.2 adds one more producer of the existing
   `<name>.failed_tests` observation and reads files only through
   `apb_core::fsutil::beneath`.
3. The profile write path: `ProfileWriteBody.process_env`
   (`web/src/lib/api/core.ts` line 108), `ProfileOptions::keep_stored`
   (`crates/apb-mcp/src/profile_tools.rs` line 244),
   `ProcessEnvPolicy::from_surface` and `stored`
   (`crates/apb-core/src/process_env.rs` lines 192 and 212), and the
   answer of MCP `profile_get` (line 114), which already carries the
   stored `process_env` inside `profile_yaml`. D4.3 is a web change only.
4. `scope_move::copy_playbook_to` and its claim (`scope_move.rs` lines 68
   to 120) and the doctor's store list (`doctor.rs` lines 200 to 203). D4.6
   reads; it never removes.
5. The isolation sandbox and every agent's sandbox needs: this change
   touches neither. Agents under isolation behave exactly as on main
   (claude and codex declared, every other agent refused under `full`
   with `agent_needs_unknown`, `CLAUDE_CONFIG_DIR` not followed).
6. The list runner, joins and run config: D4.11 only adds tests.
7. The connector trust store, the secret resolution and the prompt's
   connector block keep their shape: D4.13 adds a caller refusal in front
   of two approve entries and one narrower condition. A `{{cmd:...}}`
   helper is found as on main.

What an older apb does: D4.1 is bundled content an older apb never
installs. A definition that sets `project_checks.junit` is refused by an
older apb with a parse error (`ProjectChecksSpec` keeps
`deny_unknown_fields`), so it never runs such a definition with the field
dropped; a run that does not set it writes the same journal as before,
digest included. Everything else changes no stored format.

## D0. Validator codes

| Code | Severity | Decision | Rule |
|---|---|---|---|
| V320 | error | D4.2 | `baseline_check` `project_checks.junit` is not a usable report glob |

Checked on efae5822: `git grep -ohE '"V[0-9]+"' crates | tr -d '"V' | sort -n | uniq`
ends at 250 (V240 to V250 are before-running-alone and people-in-the-loop);
`git grep -nE '"V3[0-9][0-9]"' crates` finds nothing.

The blocks the other changes take, read from their documents on
2026-11-20:

| Change | Takes | Reserves |
|---|---|---|
| people-in-the-loop, before-running-alone (merged) | V240 to V247, V249, V250 | (V248 unused) |
| start-rules | V260 to V267 | V268 to V279 |
| eval-doubles | V280, V281 | V282, V287 to V299 for its family |
| incremental-lists-and-session-fork | V283 | none |
| inbox-waits | V284 to V286 | V287 to V299 (shared with eval-doubles' family) |
| mcp-tool-connectors, mcp-http-and-surfaces | V300 to V302 | V303 to V319 |
| one-point-oh-fixes (this change) | V320 | V321 to V339 for this family |
| one-point-oh-freeze | none | V321 to V339 named as this family's |
| secret-params-in-run-files | V340 | V341 to V349 |

So V320 is free, and V321 to V339 stay reserved. The secret param work
that used V321 in an earlier draft moved to secret-params-in-run-files,
which takes V340, so V321 stays in this family's reserve (as
one-point-oh-freeze's task 0.2 expects). Task 0.2 re-checks it on the
main the implementer starts from. V320 lives in the baseline family
module `crates/apb-core/src/validate/baseline.rs`, inside
`check_structure` (line 58), in its `project_checks` branch (lines 77 to
98, after the `only` loop), because it checks the same block; `validate()`
needs no new call. `check_structure` also runs for a follow-up's check,
where V185 already refuses a `project_checks` source, so V320 adds nothing
there.

## D4. Before-1.0 leftovers

Eight items, each with its own test: D4.1 to D4.3, D4.6, D4.9 to D4.11
and D4.13. The numbers up to D4.12 are the 1.0 plan's; D4.13 groups two
small fixes another review found; D4.7 and D4.8 (agent homes under
`isolation: full`) were cut in the fourth pass and are a later change of
their own (proposal "Out of scope" item 9); D4.12 (secret param values under the run
directory) is the change secret-params-in-run-files; D4.4 (`apb run
--scope global`) and D4.5 (global playbooks in the dashboard) are
one-point-oh-freeze's, because its
gallery links an install to the global playbook route D4.5 adds.

### D4.1 GitLab create_release sent once

Today (`connectors/gitlab/connector.yaml` lines 252 to 272) the function
declares no `idempotency`, so it is not tracked as a send. GitLab keys a
release by its tag and answers a second `POST /projects/:id/releases` for
the same tag with `409 Conflict` ("Release already exists"; GitLab API
behaviour, not visible in this repository), so a retry after a lost
answer fails the node although the release was created. With the marker
the retry finds the release and settles as already sent. The connector has no
`identity` block, which `check_marker`
(`crates/apb-core/src/connector/idempotency.rs` line 440) requires (line
463).

Decision: `marker` mode on `description`, looked up with a new read-only
`list_releases`, exactly as github's `create_issue` and `comment_issue`
are (`connectors/github/connector.yaml` lines 118 to 131 and 173 to 186).
No text mode: GitLab runs no quick action from release text
(`connectors/gitlab/PUBLIC.md` lines 101 and 102, `docs/CONNECTORS.md`
lines 1156 to 1158), so `markdown_no_commands` would only show
backslashes.

Changes to `connectors/gitlab/connector.yaml`:

1. `version: 0.2.0` becomes `version: 0.3.0` (line 2).
2. After `healthcheck: get_user` (line 3), in the comment style of
   `connectors/github/connector.yaml` lines 4 to 10:

```yaml
# How the connector reads its own verified account, so a repeat of a
# marker send can find the release the earlier attempt created. The token
# is a personal access token (docs/CONNECTORS.md "gitlab"): /user is its
# own user, the author of the releases it creates.
identity:
  function: get_user
  path: username
```

3. `create_release` gets, between `required: [project, tag_name]` and its
   `examples` block (the order github uses):

```yaml
    idempotency:
      mode: marker
      arg: description
      format: html_comment
      result_ref: _links.self
      reconcile:
        function: list_releases
        args_from_send: [project]
        fixed_args: { per_page: 20, order_by: created_at, sort: desc }
        items: ""
        text: description
        author: author.username
        item_ref: _links.self
```

   No `since_arg`: the releases listing has no "created after" argument;
   the newest 20 by creation time are read, and a further page with no
   match is inconclusive (`match_items`), never "absent". 20, not 100:
   the lookup runs within minutes of the send, and each listed release
   carries `commit`, `assets` and `evidences` before `response_pick`
   applies, while the response body is cut at 1 MiB
   (`crates/apb-engine/src/connector/call/wire.rs` line 11, `BODY_CAP`),
   so 100 releases with long notes would read as inconclusive and leave
   the send unknown.

4. A new function right after `get_release_by_tag` (line 274), in the
   style of `list_pipelines` (line 292):

```yaml
  - name: list_releases
    description: List a project's releases, newest first
    read_only: true
    method: GET
    url: "{{account.api_base}}/projects/{{args.project}}/releases"
    query:
      order_by: "{{args.order_by}}"
      sort: "{{args.sort}}"
      page: "{{args.page}}"
      per_page: "{{args.per_page}}"
    args_schema:
      type: object
      properties:
        project:
          type: string
          description: "numeric project id, or group/project path (pass the path with a literal slash; the engine percent-encodes it)"
        order_by: { type: string, enum: [released_at, created_at] }
        sort: { type: string, enum: [asc, desc] }
        page: { type: integer, minimum: 1 }
        per_page: { type: integer, minimum: 1, maximum: 100 }
      required: [project]
    response_pick: [tag_name, name, description, created_at, author.username, _links.self]
```

   `response_pick` is required: the official connector gate refuses a
   read-only HTTP function without a projection
   (`crates/apb-cli/tests/suite/official_connectors_gate.rs`, check 4 and
   `the_gate_refuses_a_read_only_function_without_a_projection`). Dotted
   paths are projected (`connector/call/encode.rs` `project`, line 16;
   `get_release_by_tag` already picks `_links.self`). The projection does
   not reach the reconcile: its read is `full: true` and returns the raw
   body (`connector/call/send_once.rs` `read_back`, `call/mod.rs`), so
   the reconcile sees every field of the listing. The `response_pick`
   stays because the gate requires it, and it is what an agent's own
   `list_releases` call gets.

5. `connectors/gitlab/tests.yaml`: the existing `create_release` case
   (line 220) gets `description: "/close later"` in its args and its
   `body_contains` (the text is sent as written); a test case cannot
   carry the send marker, so the case shows the body without it. Two new
   cases after the `get_release_by_tag` cases: `list_releases` with only
   `project: "42"` (url `https://gitlab.com/api/v4/projects/42/releases`)
   and `list_releases` with `order_by: created_at, sort: desc, per_page:
   20` (url with the query in key order:
   `...releases?order_by=created_at&per_page=20&sort=desc`, as the
   `list_issues` case at line 9 orders it).
6. `connectors/gitlab/PUBLIC.md`, the bullet at line 101 becomes: "`name`
   and `description` of `create_release` are sent as written; GitLab runs
   no quick action from release text. A release is sent once: apb appends
   a hidden marker to `description` and, after a lost answer, looks the
   release up with `list_releases`, so the retry is recognized as already
   sent instead of failing with GitLab's 409 for an existing tag."
   `docs/CONNECTORS.md` "### gitlab" (line 1139): one sentence before
   "Healthcheck: `get_user`.", in the words of the github paragraph (line
   1004): "`create_release` is sent once in `marker` mode: the lookup
   reads the token's account (`get_user`) and the newest releases
   (`list_releases`)." `README.md` and `INSTALL.md` need no change.

Test (engine, new), in `crates/apb-engine/tests/suite/send_intent_test.rs`:
`a_gitlab_release_with_a_lost_answer_is_found_by_its_marker_and_not_created_again`,
built like
`a_github_comment_with_a_lost_answer_is_found_by_its_marker_and_not_posted_again`
(line 422) over the checked-in `connectors/gitlab/connector.yaml`: the
first `create_release` answer is lost (the stand-in closes the connection
after reading the request); the stand-in answers `GET /user` with
`{"username": "bot"}` and `GET /projects/42/releases` with one release
whose `description` ends with the marker of the first request and whose
`author.username` is `bot`; the retry records what the github test
asserts (`already_sent`, the item reference `_links.self`) and the
stand-in saw exactly one `POST`. A second test,
`a_gitlab_release_by_another_account_is_not_taken_for_the_send`, gives
the listed release `author.username: other`: the send stays unknown and
no second `POST` leaves. The manifest literals of this file are built by
its existing helpers; eval-doubles adds a field to them (see "Shared
files"), so the new tests call the helpers and never write a
`ManifestConnector` literal.

Also: `crates/apb-core/src/connector/official.rs`
`every_official_connector_carries_the_full_file_set` (line 144) needs no
change (no file is added); no test pins the gitlab version or digest
(`rg -n 'gitlab' crates --include=*.rs` names only `connector_gitlab.rs`,
the gate and two CLI suites that do not read the version).

Wrong implementations:

1. Adding `text_args: { description: markdown_no_commands }`: escapes text
   GitLab never treats as a command. Caught by the `create_release` case
   of `tests.yaml`, whose body keeps `/close later` verbatim.
2. `identity.path: id`: `author.username` never equals a numeric id.
   Caught by the engine test (the lookup finds nothing, two `POST`s).
3. A reconcile without `order_by: created_at`: GitLab's default order is
   `released_at`, which a release with a past date sorts low. Caught by
   the second `list_releases` case and by the review of `fixed_args`.
4. Dropping `response_pick` from `list_releases`: refused by the official
   connector gate.
5. `per_page: 100` in the reconcile: a listing of long release notes over
   the 1 MiB body cap reads inconclusive. Caught by the second
   `list_releases` case (`per_page=20`).

### D4.2 JUnit XML failing tests for project checks

Today `crates/apb-engine/src/baseline/project.rs` reads failing tests only
for `cargo test` (`run`, line 233; `cmd.is_cargo_test()`, line 270), so a
failing jest, pytest or go suite compares `unknown` while it keeps
failing (the module doc says so, lines 15 to 17).

Schema (`crates/apb-core/src/project_checks.rs` `ProjectChecksSpec`, line
38):

```rust
pub struct ProjectChecksSpec {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub only: Vec<String>,
    /// A glob of JUnit XML reports, relative to the run's tree, that the
    /// project's `test` commands other than `cargo test` write. Read after
    /// each such command; only reports the command wrote (absent before
    /// it ran, or with another length, content digest or modification
    /// time than before) count. The reports
    /// must sit in a git-ignored place, since a project check must leave
    /// the tree as it found it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub junit: Option<String>,
}
```

`deny_unknown_fields` stays (line 37). The type is not mirrored in
TypeScript (`web/src/lib/api.gen.ts` does not name it), so no regeneration.

Validator V320 (error), in `check_structure`'s `project_checks` branch
after the `only` loop, one message per cause, `<at>` being the branch's
existing `at` label:

1. empty after trim: `"<at>: project_checks.junit is empty"`;
2. longer than 256 characters: `"<at>: project_checks.junit is longer than 256 characters"`;
3. absolute (`Path::is_absolute`, or starts with `/`, `\` or `~`): `"<at>: project_checks.junit must be relative to the run's tree"`;
4. a `\` anywhere: `"<at>: project_checks.junit must separate directories with /, never \"`. The engine splits components on `/` only and the matcher reads `\` as an escape, so `reports\*.xml` would pass rule 7 here with a literal first component and then walk from the tree's root;
5. a `..` component (split on `/`): `"<at>: project_checks.junit must not leave the run's tree (..)"`;
6. an empty or `.` component (`reports//x.xml`, `./reports/*.xml`; `fsutil::beneath` refuses both segments, `beneath.rs` `segments`, lines 107 to 122, so such a glob would always read unknown): `"<at>: project_checks.junit has an empty or . component"`;
7. a wildcard (`*`, `?`, `[` or `{`, `project_checks::is_wildcard`, the one set the engine's walk reads components by too) in its first component (`*.xml`, `**/junit.xml`): `"<at>: project_checks.junit must start with a literal directory, such as reports/"`. Such a glob walks from the tree's root, through `.git`, `target/` and `node_modules/`, and either exhausts the walk budget or digests every matching XML of the tree in the before listing. A glob of one literal component (`junit.xml`) passes: it names one file and is not walked (item 4.2 of "The engine");
8. not a glob the matcher of item 4 accepts (`project_checks::matcher`, `globset::GlobBuilder::new(glob).literal_separator(true).build()`, the one builder the engine matches with): `"<at>: project_checks.junit is not a valid glob"`;
9. `only` is not empty and does not contain `test`: `"<at>: project_checks.junit is read only after test commands, but only names no test"`.

The reader, a new pure module `crates/apb-core/src/project_checks/junit.rs`
(`mod junit;` in `project_checks.rs` next to `mod cargo_manifest;`, line
26; `pub use junit::{MAX_REPORT_BYTES, MAX_REPORT_FILES, failed_tests, failing_item, is_wildcard, matcher};`):

```rust
/// The largest report read; a larger one makes the list unknown.
pub const MAX_REPORT_BYTES: u64 = 16 * 1024 * 1024;
/// The most report files one command may write.
pub const MAX_REPORT_FILES: usize = 64;
/// How many bytes the scan moves between two calls of the caller's check.
pub const CHECK_EVERY: usize = 64 * 1024;
// Private: REPEAT_ROOM 8 (the characters an item leaves for " #<n>"),
// MAX_DEPTH 256 (the deepest element nesting read).

/// Whether `c` is a glob wildcard: `*`, `?`, `[` or `{`.
pub fn is_wildcard(c: char) -> bool;
/// The matcher V320 checks a glob with and the engine matches with.
pub fn matcher(glob: &str) -> Result<globset::GlobMatcher, globset::Error>;
/// The failing test cases of one JUnit XML report, each
/// `<classname>::<name>` (or `<name>` when `classname` is absent or
/// empty), sorted, when the report was understood completely; else why
/// not. `check` (the command's deadline and the run's cancel flag) runs
/// every CHECK_EVERY bytes and once before a list is returned; its error
/// ends the scan.
pub fn failed_tests(
    xml: &[u8],
    check: &mut dyn FnMut() -> Result<(), String>,
) -> Result<Vec<String>, String>;
/// One failing test as an observation list item, `<report>: <test>`, or
/// why not: longer than MAX_ITEM_CHARS - REPEAT_ROOM characters, or refused
/// by `baseline::extract::checked_text`. Never cut.
pub fn failing_item(report: &str, test: &str) -> Result<String, String>;
```

The principle of the reader, stated at the top of `junit.rs` too: it
returns a list only for a report it understood completely; anything else
is unknown with a named reason, never a partial or guessed list.

Exact semantics of `failed_tests` (a scanner, no XML crate; the workspace
has none and this change adds no dependency):

1. Encoding: a UTF-16 byte order mark is `Err("the JUnit report is UTF-16,
   not UTF-8")`, bytes that are not UTF-8 `Err("the JUnit report is not
   UTF-8")`; a UTF-8 byte order mark is skipped. An XML declaration
   (`<?xml ...?>`) is allowed only at the start, and one whose `encoding`
   is not `UTF-8` (or `utf8`, any case) is unknown.
2. Structure: before the root only whitespace, comments and processing
   instructions; one root element; every element closed in order, end tags
   `</name>` with optional XML whitespace before `>`; after the root only
   whitespace, comments and processing instructions. A report that ends
   inside an element, a tag, a comment, a CDATA section, a processing
   instruction or an attribute value is unknown naming what it ends in
   ("the JUnit report is cut short", "... inside a comment", ...); an end
   tag that does not close the open element, text outside the root,
   content after it, a DOCTYPE or any other `<!` declaration (an entity
   declaration could change what a name means), a CDATA section outside
   the root, and elements nested deeper than 256 are unknown, each with its
   own reason.
3. Attributes: `name="value"` or `name='value'`, separated by
   whitespace; a value without quotes, one holding `<`, or an attribute
   without a value is unknown. Values are decoded and their literal tab,
   newline and carriage return (a CRLF as one) become a space, as XML
   normalizes them.
4. References, in one forward pass wherever they occur (attribute values
   and text): `&amp;`, `&lt;`, `&gt;`, `&quot;`, `&apos;`, `&#<decimal>;` and
   `&#x<hex>;` with any number of leading zeros. Any other entity, an `&`
   that starts none, a reference without digits or with a digit of the
   wrong base, and one to a character XML does not allow (0, a surrogate,
   past U+10FFFF) are unknown.
5. A test case is a `<testcase>` element (one inside another is unknown);
   one without `name` is `Err("a testcase has no name")`. It failed when an
   element directly inside it is `<failure>` or `<error>`; a self-closing
   case passed; `<skipped>` is not a failure. Zero test cases is
   `Err("the JUnit report lists no test case")`.
6. The result is sorted; repeats are kept (the caller numbers them).
7. No input panics: the scan moves over the byte slice, matches only ASCII
   markers there and turns into text only values it cut at ASCII
   delimiters of input it checked is UTF-8. The engine has no guard against
   a panicking baseline read (the one `catch_unwind` is the batch worker's
   in `scheduler.rs`), so this is the guarantee.
8. Deadline and cancellation: the check runs right after the read,
   before any byte is scanned; UTF-8 is validated in chunks of
   `CHECK_EVERY` bytes with the check before each (a multi-byte sequence a
   chunk end cuts is taken whole into the next chunk); every loop of the
   scan (comments, CDATA, processing instructions, attribute values,
   references, text between elements, names) moves through one cursor
   that runs the check every `CHECK_EVERY` bytes, and the check runs once
   more before a list is returned.
9. A failing test becomes a list item through `junit_items`, and the
   item is its comparison key, unambiguous by construction and never the
   display text: the JSON array `[<report>, <classname or null>, <name>,
   <occurrence>]` (`encode_item`), the occurrence counting from 1 among
   equal report, classname and name across the report set, a component of
   the key and not text appended to the name, and an absent classname
   (`null`) apart from an empty one (`""`). JSON is injective over these
   fields, so two different tests, or two occurrences of one, never give
   equal items. The item length bound (`MAX_ITEM_CHARS`, 256) and the
   500 cap are held on the encoded form; a field with a control or
   invisible format character is refused before encoding, and the encoded
   item passes `checked_text` (`crates/apb-core/src/baseline/extract.rs`,
   now `pub`). The reason names the report, never the text. The human
   text, `<report>: <classname>::<name>` with ` #<occurrence>` from the
   second on, is `render_item`'s, used only in the comparison detail and
   never compared. `cargo test` items keep their `<binary>: <name>` form
   (a before snapshot an older apb journaled must keep comparing), with
   one change: `numbered_repeats` is injective, a first occurrence of a
   name that ends with ` #<digits>` getting ` #1`, so a test literally
   named `x #2` never reads as the second `x`.
   Every place the items are compared, deduplicated, sorted or counted,
   each on the key: `junit_items` (sort, occurrence of equal keys, the
   bound), `failed_tests_within` (the sort of `FailingTest` fields, the
   per-report cap), the engine's `Parse` allowance (one entry per test),
   `junit_failed_tests` (the 500 cap on items), `project::compare` (the
   membership `bt.contains` and the counts, called from
   `scheduler/baseline.rs`), and the observation's display
   (`baseline/mod.rs`, a count only). `baseline::rule`'s list comparison
   does not see project check items (`project::compare` is their only
   comparison).
10. The count cap where items are produced: `failed_tests_within(xml,
    max_failing, check)` returns `TOO_MANY_FAILING` the moment a report's
    failing count would pass `max_failing`, before any sort, and refuses
    an identity whose fields hold more than the 256 characters an item
    keeps while it reads. So the reader keeps at most `max_failing`
    identities of at most 256 characters each (`failed_tests` is the same with
    `MAX_LIST_ITEMS`, 500). The engine hands each next report only what is
    left of `MAX_LIST_ITEMS` and, when a report passes it, ends the walk
    unknown ("more than the 500 failing tests an observation keeps
    (stopped at <report>)"), reading no further report. What the engine
    stores is so at most 500 items of at most 256 characters, under 1 MiB
    in UTF-8; what one read holds at a time is one report, at most
    `MAX_REPORT_BYTES`.
11. A list only when it explains the outcome, as `cargo test`'s is read
    only when every binary printed its result: a `junit` command that
    did not pass while the reports it wrote name no failing test reads
    unknown, "the command failed, but the JUnit reports it wrote name no
    failing test" (`run`, `UNEXPLAINED_FAILURE`): it failed for a reason
    no test case carries (a coverage gate, a lint step, a suite that
    crashed before its report). The mirror, a command that passed while a
    report names a failing test (a flaky retry, an ignored failure), keeps
    its list, and that cannot produce a wrong "met": `compare` reads the
    lists only when both sides failed (a pass after is "met" by the rule
    whatever the list, a pass before and a failure after is "not met"
    whatever the lists), so a list beside a pass decides nothing. The
    limit, stated in HOWTO-authoring: a suite that crashes without a
    report beside other reports that name failures is not detectable.

The engine (`crates/apb-engine/src/baseline/project.rs`):

Who may read a report. A report counts only when it changed during
   the command and the run holds its working tree for the command, so
   no other run of apb writes there meanwhile. A run holds its tree
   exactly when its driver takes the tree lock: a run with a node that
   takes the lock and not started with `--allow-shared-workdir`
   (`crates/apb-engine/src/scheduler/entry.rs` lines 788 to 792: "A start
   that shares the workdir (`--allow-shared-workdir`) took no lock and
   handed none over, so its driver takes none either"; `is_write &&
   !cfg.shared_workdir`; `RunConfig.shared_workdir`,
   `crates/apb-engine/src/run_config.rs` line 247). `ReadCtx`
   (`crates/apb-engine/src/baseline/ctx.rs` line 14) gets `pub
   holds_tree: bool`, set at its three builders:
   `scheduler/baseline.rs` `BaselineRun::ctx` (line 61) with
   `entry::holds_tree(playbook, cfg)`, the one predicate
   (`!cfg.shared_workdir && playbook.nodes.iter().any(|n|
   n.kind.takes_workdir_lock())`) the drive takes the tree lock by at
   `entry.rs` line 790; `scheduler/list/check.rs` `read_ctx` (line 146)
   with that predicate over the list run's playbook and config and
   `run.workers == 1` (siblings write the tree, as its `guard_tree`
   says); `follow_up/read.rs` (line 149) with `false` (the run has
   ended, and V185 refuses a `project_checks` source there anyway). With
   `holds_tree` false, `run` does not scan and the `tests` observation
   of a `junit` command is `Observation::unknown("the run does not hold
   its working tree (it shares it, or takes no lock), so a JUnit report
   there may be another process's")`; the command still runs for its
   status. What stays open, stated in the docs: a run a person started
   with `--allow-shared-workdir` beside a holding run can still write its
   reports; that flag is the person's statement that the runs share the
   tree.
1. `run` (line 233) takes one more argument `junit: Option<&str>`. When
   `cmd.kind == "test"`, the command is not `cargo test`, `junit` is
   `Some(glob)` and `ctx.holds_tree` is true, it lists the reports the glob matches before it builds
   the `Command`: `let before = junit_reports(ctx.workdir, glob,
   &limits)` (a map relative path to its `ReportStamp`, or the error),
   where `limits` is a new private `ScanLimits { deadline, cancel:
   ctx.cancel }` holding the command's own `deadline` (the argument of
   `run`) and the run's cancel flag. Both scans are charged to that one
   deadline: no time is added for them.
2. The child's allowance is computed after the before scan, right before
   `crate::proc::run_capture` (line 255): the `left` of today's line
   241 moves there, unchanged (`deadline.saturating_duration_since(Instant::now())`),
   and a zero `left` still returns `Observation::unknown("the check ran
   out of time")` without starting the command. Then, immediately before
   `run_capture` and after every other step, `run` checks
   `ctx.cancel` once more: set, it returns `Observation::unknown("cancelled")`
   without starting the command (`run_capture` spawns first and looks at
   the flag only afterwards, `crates/apb-engine/src/proc.rs` line 297,
   and reads the flag first at line 322, so a cancel that arrived during the last scan
   operation would otherwise still start the test program). So a scan
   that used up the allowance, or one during which the run was
   cancelled, never starts a child. On that early return the `tests` observation is
   `Observation::unknown(<the before scan's reason>)` when the before
   scan failed, else `None`.
3. A failed before scan does not stop the command: it still runs for its
   status. The `tests` observation (lines 269 to 275) becomes: for
   `cargo test`, unchanged; else when step 1 ran, `Some(match &before {
   Err(why) => Observation::unknown(format!("the JUnit listing before the
   command failed: {why}")), Ok(b) => match junit_failed_tests(ctx.workdir,
   glob, b, &limits) { Ok(t) => Observation::value(ObservedValue::List(t)),
   Err(why) => Observation::unknown(why) } })`; else `None` as today. A
   command that could not start, was cancelled or ran out of time returns
   before this with the same `tests` rule as item 2.
4. Two new private functions in the same file. Every file access goes
   through `apb_core::fsutil::beneath::Beneath::open(tree)`: `list_dir`
   with one `ListBudget { remaining: 10_000 }` per walk (an exhausted
   listing is `Err("the JUnit walk examined more than 10000 entries")`),
   entries of kind `Link` and `Other` skipped, and
   `read_regular_capped(rel, MAX_REPORT_BYTES)`. Before every
   `list_dir` call and before every `read_regular_capped` call, the scan
   checks `limits`: the cancel flag set is `Err("cancelled")`, the
   deadline reached is `Err("the JUnit scan ran out of time")`. One read
   is at most `MAX_REPORT_BYTES`, so the deadline is overrun by at most
   one capped read; inside one report, `failed_tests` runs the same check
   every `CHECK_EVERY` bytes. The matcher is `project_checks::matcher`,
   `globset::GlobBuilder::new(glob).literal_separator(true).build()?.compile_matcher()`:
   `*` and `?` stay inside one path component, `**` crosses `/`, and a
   character class may match `/` (globset lets a negated class such as
   `[!x]` match it), so `reports/*.xml` never matches `reports/a/b.xml`
   (plain `Glob::new` lets `*` match `/`). V320 compiles the glob the
   same way.
   1. `fn walk_root(glob: &str) -> &str`: the longest literal DIRECTORY
      prefix of the glob, that is the `/`-separated components before the
      first component that holds `*`, `?`, `[` or `{`, and never the last
      component (the file name position). `reports/*.xml` and
      `reports/junit.xml` both walk from `reports` (the second is read
      directly, item 2, and the root only matters for a pattern);
      `junit.xml` gives the tree's root (`""`) and is read directly too;
      `out/**/TEST-*.xml` walks from `out`. A pattern can never walk from
      the root, since V320 refuses a wildcard in the first component.
      Walking the whole literal prefix would hand
      `Beneath::list_dir` a file for a literal report path
      (`crates/apb-core/src/fsutil/beneath.rs` line 449 walks every
      segment as a directory), and the list would read unknown.
   2. `junit_reports(tree, glob, limits) -> Result<BTreeMap<String,
      ReportStamp>, String>`: a glob without any wildcard names one file and is not
      walked: `Beneath::kind(glob)` decides (`Some(EntryKind::File)` is
      the one candidate, nothing there an empty map, a link or anything
      else `Err`). Otherwise a walk root that does not exist is an empty
      map; walk only into the directories a match can lie in (a glob
      without `**` or `{...}` has a fixed depth, `match_depth`) and at most
      8 directory levels below the root. The walk examines every path the
      glob can match or fails, never giving a shorter list: a directory a
      match could lie in past the 8th level, a name that is not UTF-8, and
      a link or special file where a report or a directory of reports could
      lie (links are never followed) are each `Err` with its reason, as are
      an exhausted budget and more than `MAX_REPORT_FILES` matches. Match each `File` entry's relative path from the
      tree (with `/` separators) against the matcher above, and record
      for each match a `ReportStamp { modified: Option<SystemTime>, len:
      u64, digest: Option<String> }`: `modified` and `len` from a new
      descriptor-relative `Beneath::file_stamp(rel) ->
      io::Result<Option<FileStamp>>` (`FileStamp { len: u64, modified:
      SystemTime }`), added beside `Beneath::file_size`
      (`crates/apb-core/src/fsutil/beneath.rs` line 502) with the same
      body: the parents walked without following a link, `lstat_at` on
      the leaf, a regular file only, `st_mtime` and `st_mtime_nsec` as the
      time; the portable variant beside its `file_size` (line 912) does the
      same through its checked path. Never `symlink_metadata` on a joined
      path, which would follow a directory swapped for a link after the
      listing. And
      `digest` the `sha256:<hex>` that `read_regular_capped(rel,
      MAX_REPORT_BYTES)` already returns in `CappedRead::Read { bytes,
      digest }` (`beneath.rs` lines 39 to 59; no second hash),
      `None` when it answers `CappedRead::Refused(_)`. More than
      `MAX_REPORT_FILES` matches is `Err` naming the count, so the before
      listing reads at most `MAX_REPORT_FILES * MAX_REPORT_BYTES` bytes.
   3. `junit_failed_tests(tree, glob, before, limits) ->
      Result<Vec<String>, String>`: list again (the same `junit_reports`,
      the same `limits`, handed a `Parse` with `before`), and parse every
      written report from the very read whose digest decided it was
      written: one read per report and listing, never a second read the
      decision did not see; a report path that fails `checked_text` is
      `Err` before it names anything. A report is written
      by the command when it is absent from `before`, or its `len`
      differs, or its `modified` differs, or either `digest` is `None`, or
      the two digests differ. A report counts as untouched only when its
      length and digest are equal and its modification time is equal too:
      the time is a shortcut that can say "changed", never "unchanged",
      because a file system stamps coarsely and a rewrite within one tick
      keeps the time. None kept is `Err(format!("no
      JUnit report written by the command matched `{glob}`"))`; more than
      `MAX_REPORT_FILES` is `Err` naming the count; a read answered
      `CappedRead::Refused(ReadRefusal::Link | ReadRefusal::HardLink |
      ReadRefusal::NotRegular | ReadRefusal::TooLarge { .. })` or not
      UTF-8 is `Err` naming the relative path (`ReadRefusal::Missing`, a
      report removed between the two listings, is the same `Err`);
      `apb_core::project_checks::failed_tests`
      on each; every name made one `failing_item` with the report's
      relative path in front (two suites may share a case name);
      concatenate, sort, then
      `numbered_repeats` and the `MAX_LIST_ITEMS` cap exactly as
      `cargo_failed_tests` does (lines 136 to 144).
   The comparison of before and after is made in this one place, on the
   files' own stamps and content digests, never against the wall clock (a
   file system stamps with a coarser clock than `SystemTime::now`).
5. `read` (line 169) passes `spec.junit.as_deref()` to `run` (the call at
   line 204).
6. `input_digest` (line 51) takes `junit: Option<&str>`: with `None` the
   digest is exactly today's (an existing run's before snapshot keeps
   comparing); with `Some(g)` it hashes `format!("{argv}\n{}\njunit={g}",
   ..)`. Its caller in `read` passes `spec.junit.as_deref()`.
7. The module doc (lines 1 to 21): the sentence "Runners other than `cargo
   test` therefore read unknown while they keep failing." becomes "With
   `junit`, the failing tests of the other `test` commands come from the
   JUnit XML reports the glob matches that the command wrote; without it,
   runners other than `cargo test` read unknown while they keep failing."

Tests:

1. Unit, `crates/apb-core/src/project_checks/junit.rs` `mod tests`:
   `a_failure_and_an_error_fail_and_a_skip_passes`,
   `a_self_closing_case_passes`, `classname_and_name_join_with_two_colons`,
   `a_case_without_classname_is_its_name`,
   `a_failure_inside_cdata_or_a_comment_is_not_a_failure` (the text
   `<failure` inside `<system-out><![CDATA[...]]></system-out>` and inside
   a comment), `legal_xml_reads_as_it_means` (the legal-XML table:
   nested suites, errors and failures, skipped, CDATA, entities, single
   quotes, a byte order mark, whitespace in end tags, processing
   instructions, a comment after the root, whitespace in a value),
   `what_the_reader_does_not_understand_is_unknown` (the fail-closed
   table, one row per reason, and a nesting past 256),
   `whitespace_in_an_end_tag_closes_its_case`,
   `an_incomplete_report_is_unknown`,
   `a_reference_with_leading_zeros_is_its_character`,
   `the_check_runs_inside_every_large_construct` (a large comment after
   the root, a large attribute value, CDATA, text, a processing
   instruction and many references each meet the check; it runs once
   more before a list is returned), `non_ascii_text_anywhere_reads`,
   `a_list_only_for_a_balanced_report_and_no_byte_string_panics` (200
   generated balanced reports read their failures exactly and every
   strict byte prefix of each is unknown; 20000 generated byte strings
   never panic and give a list only when they end with a closed root),
   `an_identity_is_never_cut` (the reviewer's two 301-character names
   are each unknown, never one item; an item that fits leaves room for a
   repeat's number; hidden characters refused without being repeated),
   `entity_decoding_is_linear`.
2. Unit, `crates/apb-core/src/project_checks.rs` `mod tests` (line 259):
   `junit_round_trips`, `an_absent_junit_is_not_serialized`.
3. Integration, `crates/apb-core/tests/suite/validate_baseline_test.rs`
   (where the V62 `project_checks` tests live, line 101):
   `v320_empty_glob`, `v320_absolute_glob`, `v320_backslash`
   (`reports\*.xml`, `reports\..\x.xml`, `reports/a\b.xml`),
   `v320_parent_component`,
   `v320_invalid_glob` (`"reports/[.xml"`), `v320_only_without_test`,
   `v320_wildcard_in_the_first_component` (`*.xml` and
   `**/junit.xml`), `v320_empty_or_dot_component` (`./reports/*.xml`
   and `reports//x.xml`), `a_relative_glob_with_test_is_valid` (also
   `junit.xml` alone).
4. Unit, `crates/apb-engine/src/baseline/project.rs` `mod tests` (line
   390): `a_linked_report_is_not_read` (a link `reports/x.xml` to a file
   outside the tree, under a pattern and as the literal path: `Err`
   naming that links are never followed);
   `a_walk_cut_by_its_depth_limit_is_unknown` (the reviewer's failing
   report nine directories down under `reports/**/*.xml`: `Err` naming
   the depth limit; `reports/*.xml` beside the same deep tree reads its
   one report);
   `a_name_that_is_not_utf8_where_a_report_could_lie_is_unknown`;
   `a_rewritten_report_counts_and_an_untouched_one_does_not` (pure over
   `junit_reports` and `junit_failed_tests` with a file whose modification
   time is set with `File::set_modified`);
   `an_equal_length_rewrite_within_one_timestamp_tick_counts` (a report
   listed before, rewritten with a different failing case of exactly the
   same byte length, its modification time then set back with
   `File::set_modified` to the value `before` recorded: the report counts
   as written and its failing case is read);
   `the_walk_root_is_the_literal_directory_prefix` (over `walk_root`:
   `reports/junit.xml` and `reports/*.xml` give `reports`, `junit.xml`
   gives `""`, `out/**/TEST-*.xml` gives `out`, `a/b/c.xml` gives `a/b`);
   `a_literal_report_path_is_read` (glob `reports/junit.xml`, the
   file written after the before listing: its failing case is read);
   `a_star_stays_inside_one_component` (glob `reports/*.xml` with
   `reports/a.xml` and `reports/sub/b.xml` written: only `reports/a.xml`
   is read);
   `a_scan_that_uses_up_the_allowance_does_not_start_the_command` (over
   `run`: one small report, the command `sh -c 'touch started; exit 1'`,
   a deadline already past when the scan starts, the deterministic form
   of a scan that spent the allowance: the status is `unknown` "the check
   ran out of time" (the pre-start check's words, not `run_capture`'s
   "ran out of time"), the tests observation is `unknown` naming "the
   JUnit scan ran out of time", and `started` does not exist);
   `a_cancelled_scan_stops` (over `junit_reports` with the cancel flag
   set: `Err("cancelled")`, nothing read);
   `a_cancel_during_the_last_scan_operation_starts_no_command` (over
   `run`: a one-byte report, a program that does not exist, a deadline a
   minute ahead, and the cancel flag set before the call: the status is
   `unknown` "cancelled". Without the check right before `run_capture`
   (which spawns before it reads the flag) the spawn is tried and the
   status reads "could not start", whatever the scheduling);
   `a_hidden_character_in_a_test_name_reads_unknown` (`&#x200B;` and
   `&#x202E;` in names: unknown, the reason repeats neither);
   `a_report_path_and_a_long_item_hold_to_the_bound` (a report path with
   a control character is refused without being named; a 400-character
   name seven directories down makes the list unknown naming the report);
   `without_junit_the_digest_is_unchanged` (over `input_digest`:
   without `junit` it hashes exactly `<argv>\n<defined>`, with one the
   glob appended; no fixture-bound value);
   `a_shared_tree_reads_junit_unknown` (over `run` with
   `holds_tree: false` and a command that writes `reports/a.xml` with one
   failure: the status is `failed`, the tests observation is `unknown`
   naming "does not hold its working tree", and no listing was made);
   `an_unreadable_before_scan_still_runs_the_command` (`reports` a link
   to a directory outside the tree, glob `reports/*.xml`, the command
   `sh -c 'touch started; exit 1'`: `started` exists, the status is
   `failed`, and the tests observation is `unknown` starting "the JUnit
   listing before the command failed");
   `an_exhausted_before_scan_still_runs_the_command` (`reports/` holding
   10 001 empty files named `f<n>`, the same command: `started` exists, the status is
   `failed`, and the tests observation is `unknown` naming "examined more
   than 10000 entries").
5. Engine, `crates/apb-engine/tests/suite/baseline_test.rs` (a `Makefile`
   project as at line 895):
   `junit_reports_list_the_failing_tests_of_a_make_test` (a git tree with
   `.gitignore` holding `reports/` and a `Makefile` whose `test` recipe
   writes `reports/junit.xml` with one failure and exits 1; the
   observation `make_test.failed_tests` lists
   `reports/junit.xml: suite::it_fails`),
   `a_report_the_command_did_not_write_reads_unknown` (the recipe exits 1
   without writing; a report from before is there: unknown, the reason
   names "no JUnit report written"),
   `a_report_in_a_tracked_place_reads_unknown` (no `.gitignore` entry: the
   tree guard's reason "the check changed the working tree"),
   `a_new_failing_case_is_a_regression` (before one failing case, after
   two: `not_met`, `regressed: true`, the detail names the new case), and
   `a_shared_workdir_run_reads_junit_unknown` (the first test's project,
   the run started with `allow_shared_workdir: true`: the observation
   `make_test.failed_tests` is unknown naming "does not hold its working
   tree"; the engine wiring of `holds_tree` is the contract here). The
   literal path and the digest are pinned by the unit tests of item 4
   only, where their contract lives.
6. Docs: `docs/HOWTO-authoring.md` the `project_checks` bullet (line
   5215) gets `junit` and its rule (git-ignored place, only reports the
   command wrote); the validator paragraph (line 5290) gets "**V320** (a
   `project_checks.junit` that is empty, over 256 characters, absolute,
   holds a `\`, leaves the tree, has an empty or `.` component or a wildcard in its
   first component, is not a glob, or sits beside an `only` without
   `test`)". The bullet also says that `*` stays inside one directory
   and `**` crosses directories, and: "Reports are read only in a run
   that holds its working tree; in a run started with
   `--allow-shared-workdir`, or one with no node that takes the tree, the
   failing tests read unknown. A run started with
   `--allow-shared-workdir` beside a run that holds the tree can still
   write reports there."
7. Unit, `crates/apb-core/src/fsutil/beneath.rs` `mod tests`:
   `file_stamp_reads_a_regular_file_without_following_links` (a file
   gives its length and time; a link at the leaf and a directory link on
   the way give `None` and an error as `file_size` does).

Wrong implementations:

1. Reading every report the glob matches, written or not: caught by
   `a_report_the_command_did_not_write_reads_unknown`.
2. Comparing a report's time with `SystemTime::now()` taken before the
   command: a report written in the same clock tick reads as stale on a
   file system that stamps coarsely. Caught by review against item 4.3 and
   by `a_rewritten_report_counts_and_an_untouched_one_does_not`.
3. Counting `<skipped>` as a failure: caught by
   `a_failure_and_an_error_fail_and_a_skip_passes`.
4. Matching `<failure` by substring over the whole case body: caught by
   `a_failure_inside_cdata_or_a_comment_is_not_a_failure`.
5. Changing the digest for specs without `junit`: caught by
   `without_junit_the_digest_is_unchanged`.
6. Reading with `std::fs::read` instead of `beneath`: follows a link.
   Caught by `a_linked_report_is_not_read`.
7. Walking from the whole literal prefix: a literal report path is listed
   as a directory and reads unknown. Caught by
   `the_walk_root_is_the_literal_directory_prefix`,
   `a_literal_report_path_is_read`.
8. Comparing `(modified, len)` only, or trusting an equal modification
   time as "unchanged": an equal-length rewrite within one tick is missed.
   Caught by `an_equal_length_rewrite_within_one_timestamp_tick_counts`.
9. `globset::Glob::new` as the matcher: `*` crosses `/`. Caught by
   `a_star_stays_inside_one_component`.
10. Hashing the report a second time beside `CappedRead::Read.digest`,
    or reading it a second time to parse it: the parsed bytes are not the
    ones the written decision saw. Caught by review (one read, its digest
    and its bytes).
11. Keeping a test name of any length or with any character: one report
    can put megabytes, terminal escapes or invisible text into one journal
    line and every surface that shows it. Cutting it instead makes two
    tests one item, and a new failure hides behind an old one. Caught by
    `an_identity_is_never_cut`,
    `a_hidden_character_in_a_test_name_reads_unknown` and
    `a_report_path_and_a_long_item_hold_to_the_bound`.
12. Computing the child's allowance before the scan, or giving the scans
    time of their own: the child starts past the deadline with a stale
    allowance. Caught by
    `a_scan_that_uses_up_the_allowance_does_not_start_the_command`.
13. Skipping the command when the before scan fails: its status is lost.
    Caught by `an_unreadable_before_scan_still_runs_the_command` and
    `an_exhausted_before_scan_still_runs_the_command`.
14. Reading reports in a run that does not hold its tree: another run's
    report is taken for this command's. Caught by
    `a_shared_tree_reads_junit_unknown` and
    `a_shared_workdir_run_reads_junit_unknown`.
15. Leaving the cancel check to `run_capture`: it spawns first. Caught
    by `a_cancel_during_the_last_scan_operation_starts_no_command`.
16. Stamping a report with `symlink_metadata` on a joined path: a
    directory swapped for a link after the listing is followed. Caught by
    review against item 4.2 (`Beneath::file_stamp`) and by
    `file_stamp_reads_a_regular_file_without_following_links`.
17. Slicing the report's text at a byte index the scan stepped to: a
    multi-byte character there panics the drive. Caught by
    `non_ascii_text_anywhere_reads` and
    `a_list_only_for_a_balanced_report_and_no_byte_string_panics`.
18. Looking for an entity's `;` within a fixed window, or to the value's
    end: a legal reference with many leading zeros is misread, or the
    decode is quadratic. Caught by
    `a_reference_with_leading_zeros_is_its_character` and
    `entity_decoding_is_linear`.
19. Returning the cases read so far from a report a runner stopped
    writing, or from one the reader did not understand: a partial list
    can satisfy "no new failures". Caught by
    `an_incomplete_report_is_unknown`,
    `what_the_reader_does_not_understand_is_unknown` and the prefix half
    of `a_list_only_for_a_balanced_report_and_no_byte_string_panics`.
20. Recognizing `</testcase>` only without whitespace: a failure is
    credited to the wrong case. Caught by
    `whitespace_in_an_end_tag_closes_its_case`.
21. Skipping what the walk's depth limit, a link or an odd name keeps out
    of the search: a failing report goes unread. Caught by
    `a_walk_cut_by_its_depth_limit_is_unknown`,
    `a_linked_report_is_not_read` and
    `a_name_that_is_not_utf8_where_a_report_could_lie_is_unknown`.
22. Checking the deadline only between constructs: a large comment or
    value scans unchecked. Caught by
    `the_check_runs_inside_every_large_construct`.
23. Comparing display text: a joined `<classname>::<name>` and a repeat
    numbered as text in the name make different tests equal (`A::B` +
    `C` against `A` + `B::C`, two `x` against `x` and `x #2`), and a
    swap between two failing runs reads as no new failure. Caught by
    `two_different_tests_never_share_a_key`,
    `distinct_fields_never_give_equal_keys`,
    `a_swap_between_two_failing_runs_is_a_new_failure` and
    `cargo_repeat_numbering_is_injective`.

### D4.3 process_env in the profile editor

Today the save body takes `process_env` (`web/src/lib/api/core.ts` line
108; `crates/apb-server/src/routes/profiles.rs` lines 166 and 218) and
absent keeps the stored value (`ProfileOptions::keep_stored`), but
`web/src/pages/ProfileEdit.svelte` has no field (no match for
`process_env`). The editor reads every other setting from the profile's
YAML through `parseProfileDoc` (`web/src/lib/profileedit.ts` line 223,
called in `loadProfile`, `ProfileEdit.svelte` line 225), and
`GET /api/profiles/{name}` answers `profile_get`'s `profile_yaml`
(`routes/profiles.rs` line 98), which holds the stored `process_env`. So
nothing on the server changes (the draft's `profile_get` key is dropped:
the value is already in the answer).

1. `web/src/lib/profileedit.ts`:
   1. `ParsedProfileDoc` (line 194) gains `processEnv: ProcessEnv`, and
      `parseProfileDoc` reads `process_env: { policy?, allow? }` from the
      YAML: absent, or a policy other than `allowlist`, is `{ policy:
      'inherit', allow: [] }` (the stored form drops `inherit`,
      `ProcessEnvPolicy::stored`).
   2. New exports:

```ts
export type ProcessEnvPolicy = 'inherit' | 'allowlist'
export interface ProcessEnv { policy: ProcessEnvPolicy; allow: string[] }
/** The editor's text area: one name or pattern per line, blank lines and
 * surrounding spaces dropped, a repeat kept once in first order. */
export function parseAllow(text: string): string[]
/** The text area's content for a stored list. */
export function allowText(allow: string[]): string
/** The save body's `process_env`: `undefined` when the editor's state is
 * the stored one (the save then keeps the stored value), else the policy
 * with `allow` (dropped for `inherit`). */
export function processEnvChange(
  stored: ProcessEnv,
  policy: ProcessEnvPolicy,
  text: string,
): { policy: ProcessEnvPolicy; allow?: string[] } | undefined
```

   The one comparison of the editor's state with the stored value is
   inside `processEnvChange`; the page never compares on its own.
2. `ProfileEdit.svelte`: state `storedEnv` (from `parseProfileDoc` in
   `loadProfile`, reset with the other editable fields when the route
   target changes), `envPolicy` and `envText`; a fieldset "Process
   environment" after the skills field, with a `<select>` (`inherit`:
   "Inherit every variable (default)", `allowlist`: "Allowlist: the base
   set, the agent's own variables and these") and, only for `allowlist`, a
   `<textarea>` with the hint "One variable name or pattern per line, for
   example `GITHUB_TOKEN` or `AWS_*`." `save` (line 301) adds
   `process_env: processEnvChange(storedEnv, envPolicy, envText)` to the
   `writeProfile` body; `undefined` is left out of the JSON, so an
   untouched profile keeps its stored value. A new profile starts at
   `inherit`. Plain CSS over the page's existing CSS variables; no
   utility class strings. A server refusal (`process_env: ...` from
   `from_surface`) shows in the existing error toast.

Tests, `web/src/lib/profileedit.test.ts`: `parseProfileDoc reads an
allowlist and defaults to inherit`, `parseAllow drops blanks and
repeats`, `allowText joins with newlines`, `processEnvChange is undefined
for an untouched profile` (stored allowlist, same policy, same text with
extra blank lines), `processEnvChange drops allow for inherit`,
`processEnvChange sends a changed allowlist`.

Wrong implementations:

1. Sending `process_env` on every save: an untouched profile would be
   rewritten with the editor's default and its stored allowlist lost.
   Caught by `processEnvChange is undefined for an untouched profile`.
2. Adding `process_env` to `profile_get` or to `ProfileDetail`: a second
   copy of a value the YAML already carries, and an MCP answer the freeze
   would then pin. Caught by review (no Rust file in D4.3).

### D4.6 A playbook directory a stopped copy or draft creation left, in doctor

Today a copy killed between its claim and its rename leaves an empty
`playbooks/<id>` that refuses the id (`crates/apb-core/src/scope_move.rs`
doc of `copy_playbook_to`, lines 68 to 75), and a draft creation killed
after its `create_dir` (`versioning/mod.rs` line 1282) leaves the same,
or that directory with staging and pointer files in it. `apb doctor`
reports it only as a failed load (`[fail] playbook <id>: load failed`,
the registry's "has no current pointer"), with no word on what left it or
what to do; for a project directory the guidance below joins that line,
and a global store directory, which that check does not read, gets a line
of its own. The doctor offers a removal command only for an empty directory, the
one case where removing it can lose nothing (`rmdir` refuses a directory
that is no longer empty). For any other directory it gives the path and
asks a person to inspect it: a draft creation may resume and publish
after the doctor looked (`interrupted_draft`, `versioning/mod.rs` lines
1282 to 1290, lets a retry finish it), and a name the doctor does not
know may be a person's data.

1. `crates/apb-core/src/scope_move.rs`, after `copy_playbook_to`:

```rust
/// How old a claim must be before it is named (a creation in progress is
/// younger).
pub const STALE_CLAIM_SECS: u64 = 600;

/// What a stale playbook directory holds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClaimLeft {
    /// No entry at all: `rmdir` removes it safely.
    Empty,
    /// Only names the draft creation itself writes (`is_draft_debris`).
    DraftDebris,
    /// A staged copy of this id (`playbooks-staging/<id>-<nanos>`) is
    /// still beside the store: a copy may still be running.
    Staging,
    /// Anything else beside or instead of those names.
    Other,
}

/// One playbook directory a stopped copy or draft creation may have left.
pub struct StaleClaim {
    pub path: PathBuf,
    pub left: ClaimLeft,
}

/// The playbook directories of `playbooks` (a store's `playbooks/`) that
/// hold no `current`, whose modification time is more than
/// `STALE_CLAIM_SECS` before `now`, and whose locks are not held. Never
/// removes anything; sorted by path.
pub fn stale_claims(playbooks: &Path, now: std::time::SystemTime) -> Vec<StaleClaim>;
```

   A directory counts when `symlink_metadata` says it is a directory (not
   a link), it has no entry named `current` (every playbook has one; a
   directory without it is what an interrupted claim or creation leaves),
   its `modified()` is more than `STALE_CLAIM_SECS` before `now`, its id
   lock is not held, and, when `.candidate.lock` is there, `lock_held`
   says it is not held. `left` is `Staging` when the store's staging
   area (`<parent>/playbooks-staging`, `STAGING_DIR`) holds an entry
   `<id>-<digits>`, the name `copy_playbook_to` stages under; else
   `Empty` when `read_dir` yields no entry, `DraftDebris` when every
   entry is draft debris, else `Other`. A copy that stages for longer
   than its id lock's 60 s staleness is told apart by its staged copy,
   never by the lock alone, so it is never offered `rmdir` (a copy killed
   while staging leaves the same and is asked to be inspected).
   Draft debris is exactly the names the draft code creates, never any
   other dot name (`interrupted_draft` accepts any hidden name, line
   1377; the doctor does not, since `.env` or `.git` would then read as
   debris):
   1. the staging directory of the draft version,
      `.tmp-<DRAFT_VERSION>-<nanos>` with `<nanos>` all ASCII digits
      (`temp_dir_name`, `versioning/mod.rs` lines 1665 to 1668, called at
      line 1303 with `DRAFT_VERSION`, line 1348);
   2. the pointer lock `.candidate.lock`
      (`candidate_pointer::CANDIDATE_LOCK_FILE`,
      `crates/apb-core/src/candidate_pointer.rs` line 16), taken by the
      move of `current` at line 1343;
   3. the pointer's intent file `current.intent.json`
      (`current::INTENT_FILE`, `versioning/current/intent.rs` line 13);
   4. the pointer's history file `current-history.jsonl`
      (`current::HISTORY_FILE`, `versioning/current/history.rs` line 12).
   The list is one `pub(crate) fn is_draft_debris(name: &OsStr) -> bool`
   in `versioning/mod.rs`, beside `temp_dir_name`, so the staging pattern
   is read where it is written. `interrupted_draft` stays private and
   unchanged. A copy holds `<playbooks>/<id>.lock` (`fsutil::lock_dir`,
   `scope_move.rs` line 97) for as long as its claim exists. That lock is
   a `DirLock` token file (`crates/apb-core/src/fsutil.rs` line 870), not
   an OS lock: a killed holder leaves the file behind, and `lock_dir`
   itself counts a lock file whose modification time is older than
   `LOCK_STALE_MS` (60 s, line 893) as its dead owner's. The doctor uses
   that same rule, read only: a new `pub fn lock_held(dir: &Path,
   lock_name: &str, now: SystemTime) -> bool` in `fsutil.rs`, next to
   `lock_dir`, answers true when `<dir>/<lock_name>` exists as a regular
   file (`symlink_metadata`, no link followed) whose modification time is
   less than `LOCK_STALE_MS` before `now`, or lies after `now` (clock
   skew counts as held, as `lock_is_stale`, line 1018, does).
   The doctor takes `now` so its
   tests can age files. Both read one rule, `fn stale_at(mtime, now,
   stale_ms)` beside `lock_is_stale`, so the read and the steal cannot
   drift. `lock_held` never takes, breaks or removes a
   lock. Files beside the claim (`<id>.lock`) are otherwise skipped. A
   store that does not exist is an empty list. What this protects, stated
   exactly: a claim whose id lock is held under the lock's own 60 s rule
   is never named. A `DirLock` file is written once at `create_new`
   (`fsutil.rs` line 957) and its time is never refreshed, so a copy that
   holds its lock for more than 60 s reads "not held", just as `lock_dir`
   itself would then steal that lock (pre-existing). The doctor names a
   claim only after ten minutes and only ever offers `rmdir` on an empty
   directory.
2. `crates/apb-cli/src/doctor.rs` `doctor_env` (line 136): a project
   claim the report of `apb_core::doctor::diagnose` already names as a
   failed load (`playbook <id>`, `CheckStatus::Fail`) gets its line joined
   to that check's detail (`fold_claims`), so one line names the
   directory; the claims no failed load names, and every claim of the
   global store (which `diagnose` does not read), get one `WARN` line in
   the existing loop over `stores` (lines 200 to 210) with subject
   `playbooks`. Both are built by a new pure `fn stale_claim_line(claim:
   &StaleClaim) -> String`:
   1. `Empty`: "empty directory <path>: a copy or a draft creation
      stopped before it finished; remove it with `rmdir <quoted path>`".
   2. `DraftDebris`: "directory <path> holds only what a stopped draft
      creation leaves; inspect it before removing anything (creating the
      draft again may finish it)". No command.
   3. `Staging`: "directory <path> has no `current` and a staged copy of
      it is still in the staging area; a copy may still be running,
      inspect it before removing anything". No command.
   4. `Other`: "directory <path> has no `current` and holds entries apb
      did not leave; inspect it". No command.
   The path in a command is quoted with
   `apb_engine::next_action::command::shell_quote`
   (`crates/apb-engine/src/next_action/command.rs` line 150, the one
   quoting every printed next action uses) and preserved byte for byte:
   the command is printed only when the path is UTF-8 (`Path::to_str`)
   and holds no control or hidden character (`apb_core::text::replace_hidden(s,
   ' ') == s`), and the line is not passed through
   `sanitize_for_terminal`, which collapses whitespace
   (`crates/apb-cli/src/util.rs` line 85). Otherwise the `Empty` line
   names the path through `sanitize_plain` and says "remove it by hand"
   with no command. The path outside a command goes through
   `sanitize_plain` like every other line, the `Empty` line's leading
   path included.

Tests: `crates/apb-core/tests/suite/scope_move_test.rs`:
`an_old_empty_claim_is_named`, `a_young_empty_claim_is_not_named`,
`a_directory_with_current_is_not_named`, `a_link_is_not_named`,
`an_old_empty_claim_whose_id_lock_is_held_is_not_named` (an empty
`playbooks/gone` and a `playbooks/gone.lock` held through
`fsutil::lock_dir` for the duration of the call; `now` 700 s ahead of the
directory's time and less than 60 s ahead of the lock file's, set with
`File::set_modified`) and
`an_old_empty_claim_with_a_stale_id_lock_is_named` (the lock file left
behind, its time 120 s before `now`),
`only_the_exact_draft_names_are_debris` (a `.tmp-1.0.0-123` directory,
`current.intent.json` and `current-history.jsonl`: `DraftDebris`; the
same with a `.env` beside them, or a `.tmp-x`, or a `notes.txt`:
`Other`), `a_version_without_current_is_other` (a `1.0.0` directory and
no `current`: `Other`) and
`a_claim_whose_pointer_lock_is_held_is_not_named` (a fresh
`.candidate.lock` inside) and `an_empty_claim_with_a_staged_copy_is_staging`
(`playbooks-staging/gone-123456789` beside it: `Staging`; an id
`gone-b` that shares the prefix stays `Empty`); age by passing a `now`
700 s ahead.
Unit, `crates/apb-core/src/versioning/mod.rs` `mod tests`:
`a_staging_name_is_draft_debris` (the name `temp_dir_name(DRAFT_VERSION)`
returns is debris, so the two never drift; `.tmp-2.0.0-1`,
`.tmp-1.0.0-x`, `.env`, `.git`, `1.0.0` and `current` are not).
Unit, `crates/apb-core/src/fsutil.rs` `mod tests` (line 1032):
`lock_held_follows_the_staleness_of_lock_dir` (a fresh lock file is held,
one older than `LOCK_STALE_MS` is not, one dated after `now` is, a
missing one is not, a link is not, and each answer is the negation of
`lock_is_stale` on the same file).
Unit, `crates/apb-cli/src/doctor.rs` (a new `#[cfg(test)] mod tests`):
`an_empty_claim_command_keeps_the_path_whole` (the path `/tmp/a b/$(x)/it's`:
the line ends with exactly `rmdir '/tmp/a b/$(x)/it'\''s'`, the single
quoting `shell_quote` defines, so the shell gets the path back as one
word, byte for byte), `a_path_that_cannot_be_quoted_gets_no_command` (a
non-UTF-8 path built with `OsStr::from_bytes`, and one holding a
newline: no `rmdir` in the line), and
`only_an_empty_claim_gets_a_command` (`DraftDebris`, `Staging` and
`Other` lines hold neither `rmdir` nor `rm `), and
`a_claim_joins_the_failed_load_of_its_id` (a claim whose id a failed
load names joins that detail; another comes back). No dependency is
added.
The doctor line in `crates/apb-cli/tests/suite/diagram_cli_test.rs`,
the suite that already runs `apb doctor` over the same store loop (its
`doctor` helper, line 55):
`doctor_names_an_empty_claimed_playbook_directory` (an empty
`.apb/playbooks/gone` aged with `File::set_modified` on the directory
opened read-only: exactly one line names it, the `playbook gone: load
failed` line, with `rmdir`, and no separate `playbooks:` line; the
expected path is canonicalized, since the doctor reads the physical
working directory (`/private/var` on macOS); the directory still exists
afterwards). The module doc of that file gains "and the
doctor's empty playbook directories".

Wrong implementations:

1. Removing the directory in doctor: it cannot tell a crash from a draft
   being written. Caught by the doctor test (the directory still exists).
2. No age threshold: a creation in progress is named. Caught by
   `a_young_empty_claim_is_not_named`.
3. Ignoring the id lock: a copy whose lock is held under the 60 s rule
   is named and doctor recommends `rmdir` on a live claim. Caught by
   `an_old_empty_claim_whose_id_lock_is_held_is_not_named`.
4. Counting any dot name as draft debris: `.env` or `.git` would read
   as debris. Caught by `only_the_exact_draft_names_are_debris`.
5. A removal command for a directory that is not empty: a draft may
   resume, and a name apb does not know may be data. Caught by
   `only_an_empty_claim_gets_a_command`.
6. Printing the path in a command through `sanitize_for_terminal` or
   unquoted: a space or shell syntax changes the command. Caught by
   `an_empty_claim_command_keeps_the_path_whole`.
7. Taking or breaking the lock to test it: doctor writes. Caught by
   review against item 1 (`lock_held` only reads) and by
   `an_old_empty_claim_whose_id_lock_is_held_is_not_named` (the lock
   file's token is unchanged afterwards).

### D4.9 The hermes resume form

`crates/apb-core/src/agents/hermes.rs` line 71 becomes:

```rust
resume: Some(&["--resume", "{session}", "-z", "{prompt}", "-m", "{model}"]),
```

Today it is `["-z", "--resume", "{session}", "{prompt}", ...]`, which
hermes refuses: checked on 2026-11-20 against the installed hermes 0.23.2,
`hermes -z --resume s hello -m m` exits with `argument -z/--oneshot:
expected one argument`; its help lists `-z PROMPT`, `-m MODEL` and
`--resume SESSION`. The comment above it becomes "hermes re-enters a
session via `--resume <id>`, the prompt after `-z`, still in one-shot
mode."

What this does not change: hermes's one-shot mode prints no session id
(its help: "No banner, no spinner, no tool previews, no session_id line"),
and hermes's fresh form is `FreshForm::Printed`, so apb learns a hermes
session only from a JSON `session` or `session_id` field the output does
not carry. The resume form is therefore rarely reached; this fix makes it
a valid command line when it is, and the release notes say no more than
that.

Test, a new `#[cfg(test)] mod tests` in `hermes.rs`:
`every_value_option_is_followed_by_its_placeholder`: in the launch and the
resume form, `-z` is followed by `{prompt}`, `-m` by `{model}` and
`--resume` by `{session}`. A general check in `agents/check.rs` would need
every agent's option arities; it is not added.

The session id goes back as an argument, and in hermes's case it would
come from the model's own reply text, so it is used only after the shape
check every captured session goes through,
`apb_engine::adapter::is_plausible_session_id` behind `capture_session`
(the same filter `capture_session_header` and `capture_failed_session`
apply for codex and dsh): ASCII letters, digits, `-`, `_`, `.` and `:`,
not starting with `-`, one to 128 characters (the empty id, which the
filter let through before, is refused now). Anything else is no session:
the answer round or the next `continue_session` step starts cold, and
`resume_decision` journals `interaction_downgraded` with the missing
session as its reason. Test, `crates/apb-engine/tests/suite/adapter_test.rs`
`a_hermes_session_id_of_another_shape_is_no_session` (`--yolo`, `-z`, a
space, a slash, a semicolon, 129 characters and the empty id give no
session; `20261120_101500_ab12` is taken). A shape cannot tell one real
session of this machine from another: which session a reply names stays
the agent's word, as for every agent that prints its own id.

Wrong implementation: none caught only by position; `-z {prompt} --resume
{session}` is also valid, and the test checks pairs, not positions.

### D4.10 ClaudeAdapter becomes CliAdapter

`pub struct ClaudeAdapter` (`crates/apb-engine/src/adapter.rs` line 836)
serves every agent through its `InvocationDef`. Rename the type to
`CliAdapter` in every file `rg -l ClaudeAdapter crates` lists (18 on
efae5822, the same 18 in the worktrees of the four changes in flight):
`adapter.rs`, `agent_probe.rs`, `scheduler/quota_probe.rs`,
`scheduler/judge.rs`, `scheduler/node.rs`, `scheduler/supervisor.rs`,
`apb-cli/src/run.rs` and eleven engine test files (list in "Files"), and
drop the sentence "The type name is historical: ..." of its field doc
(lines 839 to 841). No alias is kept: the crates' Rust API is not part of
the 1.x promise. Check afterwards: `rg -n ClaudeAdapter crates CLAUDE.md
AGENTS.md` finds nothing (`CLAUDE.md` and `AGENTS.md` do not name it
today). Decision records under `docs/adr/` and the dated plans and specs
under `docs/superpowers/` are records of their time and keep the old name
(`docs/adr/0005-agent-knowledge-lives-in-its-module.md` names
`ClaudeAdapter::from_env`). If a change merged before this one added a
user, rename it too and give the real count in the final report.

### D4.11 Three tests that were never written

1. CLI run, a list item cannot start its agent: in
   `crates/apb-engine/tests/suite/list_run_test.rs`, next to
   `an_unstartable_item_in_a_host_session_run_fails_without_a_host_fallback`
   (line 799), `an_unstartable_item_in_a_cli_run_fails_without_a_host_fallback`:
   the same seed and stub, `ExecutionRequest::default()` (no host
   session); item `a` fails with the CLI's error, no `execution_fallback`
   and no `host_task_requested` event, item `b` passes, the run ends.
2. List item commits: in `list_run_test.rs`,
   `items_of_one_worker_each_record_their_own_commits` (a script node with
   `batch: { size: 1, parallel: 1 }` over two items, each commits once
   with an `Apb-Run` trailer; asserted on the journal, since
   `NodeCommits` has no item or execution key field,
   `crates/apb-engine/src/run_outcome/commits.rs` lines 13 to 31, and
   `artifacts_committed` is not item-stamped, `list/stamp.rs` lines 11
   to 24: the journal holds two `artifacts_committed` events for the
   node, the first before item `a`'s `list_item_settled` and the second
   between that and item `b`'s, each listing exactly one commit whose
   `apb_run` is `Some([<the run id>])` and whose subject names its item)
   and
   `items_of_several_workers_record_no_commits` (`parallel: 2`: no commit
   record for the node, the run succeeds, and both commits are on
   `HEAD`). The item script serializes its commits with a lock directory
   made by `mkdir` (plain sh, no `flock` command, which macOS does not
   ship) and fails its item when its commit fails, so a lost commit
   cannot pass.
   Cut in implementation: the draft also asked that each event's
   `attempts[*].attempt` name an `attempt_started` of its item. A script
   execution journals no attempt lifecycle (the `NodeKind::Script` arm
   of `scheduler::node::execute_node_kind` appends no `attempt_started`;
   `provenance::attempt_started` is called from the agent arms only), and
   `run_outcome/commits.rs` documents the empty `attempts` of such a node.
   Journaling one is a journal change of its own, beyond "Unchanged from
   main"; the proposal's "Out of scope" item 11 names it as future work. These pin
   `track_provenance: run.workers == 1` (`scheduler/list/batch.rs` line
   402) and the node-keyed tracker (`scheduler/provenance.rs`
   `attempt_started`, line 46, and the parked file `parked_path`, line
   236).
3. The join collector case: in
   `crates/apb-engine/tests/suite/join_loop_test.rs`,
   `a_join_waits_when_one_branch_of_the_new_round_has_not_started`: the
   `PANEL` playbook of that file (line 23) with both reviewers `open` in
   round 1 and, in round 2, reviewer `a` clean (its gate goes straight to
   the collector) while reviewer `b`'s gate has not run yet: the
   collector's second `node_started` comes after `b`'s second
   `node_finished` (assert by journal position with the file's
   `positions` helper, line 106).
4. Secret param values on disk: the change secret-params-in-run-files
   (it was a pinning test in the draft; the review found leaks, so it
   became a decision of its own).

Items 1 to 3 may pass on the first run: they pin behaviour main already
has. A failure is a defect, fixed in this change at the site the test
names (`list/batch.rs`, `provenance.rs`, the join fold) and recorded in
the final report.

If a change merged before this one changed one of these behaviours on
purpose (its archived design says so), follow the merged behaviour and
say so in the final report.

### D4.13 Two connector fixes

Two small defects another review found; each is one site with its own
test. They add five files (the CLI and server approve entries, their two
test files, and `connector/prompt.rs`, whose test is a unit test in the
same file); `docs/CONNECTORS.md` is already in the list.

1. Approving a connector is a person's act.
   `apb connector approve <c> [--account <a>]` (`approve_cmd`,
   `crates/apb-cli/src/connector.rs` line 345, dispatched at line 159)
   and `POST /api/connectors/approve` (`approve_connector_handler`,
   `crates/apb-server/src/routes/connectors/mod.rs` line 520) approve a
   connector tree or an account digest for any caller today. Each surface
   decides who the caller is the way it already does for its other acts
   only a person takes, for every connector kind and for both the
   connector and the account form, as the first statement, before the
   connector is loaded:
   1. The CLI runs in its caller's process, so it reads the caller's
      environment: `apb_core::step_env::StepCaller::from_env().refusal("approving
      a connector")` (`crates/apb-core/src/step_env.rs` line 37), printed
      on stderr with exit 2, as `apb health clear` does
      (`crates/apb-cli/src/health.rs` lines 175 to 180).
   2. The server cannot see its caller's environment (its own
      `APB_RUN_ID` says nothing about who posts), so it decides from the
      request's credential. The auth middleware marks every request it
      lets through with a `RequestChannel`
      (`crates/apb-server/src/auth.rs` lines 688 to 714: `Keyless` at line
      993, `Session` at line 1037, `Bearer` at line 1041), and a keyless
      dashboard lets a local script's write through without an `Origin`
      header (`keyless_write_allowed`, lines 909 to 925). The handler gains
      `channel: Option<axum::Extension<crate::auth::RequestChannel>>` and
      maps it as `post_review_handler` does for a person-only gate and a
      publish consent (`crates/apb-server/src/routes/runs.rs` lines 736
      and 766): `let channel = crate::auth::RequestChannel::of(channel.map(|c|
      c.0));` then `if let Some(refusal) = channel.person_only_refusal()`
      (`crates/apb-engine/src/decision_channel.rs` line 99; only
      `DashboardSession` passes over HTTP) it answers `403` with `{
      "error": refusal.as_str(), "message": "approving a connector is a
      person's act, and over HTTP only a signed-in dashboard session takes
      it: a dashboard without keys is reachable by anything on this
      machine, and a bearer key may be a script's; approve with `apb
      connector approve <name> [--account <acct>]` at a terminal, or issue
      a key with `apb server key issue` and sign in" }`, the status every
      channel refusal of that route family answers (`gate_refusal`,
      `runs.rs` lines 725 to 727). A keyless request is
      `keyless_dashboard`, a bearer request `bearer_key`. The server does
      not read `StepCaller` here. Nothing is read or written on a
      refusal.
   The CLI refusal is a mistake guard, as every `StepCaller` refusal is:
   it reads the caller's environment, which a process can clear, so it
   stops a run step that runs the command by mistake, not one that hides
   that it is a step. A terminal-only or passkey-bound connector approval
   is future work (the proposal's "Out of scope"); MCP connectors get the
   terminal rule in their own change.
   The boundary, stated once in `docs/CONNECTORS.md` beside `apb
   connector approve` (line 217) as main states it for its other
   session-only acts (`docs/GUARDRAILS.md` "A server key is a person's
   credential", lines 116 to 123): "Over HTTP a connector is approved only
   through a signed-in dashboard session; whoever holds a server key can
   sign in, so this guards against a script or a run step posting by
   mistake, not against a key holder." The dashboard's approve button
   (`web/src/pages/ConnectorView.svelte` line 275) needs no change: on a
   dashboard without keys its existing error toast shows the refusal with
   the CLI line.
   Tests:
   1. `crates/apb-cli/tests/suite/connector_cli.rs` (next to
      `approve_marks_connector_then_account_as_trusted`, line 446)
      `approve_is_refused_inside_a_run_step` (`APB_RUN_ID` set on the
      command: `connector approve widget` and `connector approve widget
      --account default` (the account `write_widget_account` writes,
      line 52) each exit 2 with "inside run", and `connector list` still
      says `unapproved`).
   2. `crates/apb-server/tests/suite/connectors_api_test.rs`, under
      `common::env_lock()`:
      `approve_endpoint_refuses_a_keyless_request_from_a_local_process`
      (a keyless `AppState` whose own environment has no `APB_RUN_ID`,
      removed with the file's `unset_var` guard; a POST with no `Origin`
      and no CSRF marker, as a run step's script sends it, for the
      connector and for an account: `403`, `error: keyless_dashboard`,
      and the listing still reports the fixture connector unapproved; the
      same for the keyless page's own request with the CSRF marker and for
      a same-origin browser request, which pass `keyless_write_allowed`,
      the connector and the account read unapproved after each refusal),
      `approve_endpoint_refuses_a_bearer_key` (a keyed `AppState` built
      as `auth_endpoints_test.rs` `authed_with` builds it, line 21, the
      request carrying `Authorization: Bearer <key>`, for the connector
      and for an account: `403`, `error: bearer_key`, and nothing
      approved) and
      `a_signed_in_session_approves_while_the_server_has_a_run_id` (the
      server's own environment sets `APB_RUN_ID`; a signed-in session
      approves the connector and the account: the credential decides,
      never the environment).
   3. The keyless approvals the file makes today move to a signed-in
      session on a keyed server: `approve_endpoint_flips_connector_trust`
      (line 336) and `account_actions_without_workspace_use_the_listed_account`
      (lines 861 and 906, the second still answering `404` for an
      account no project configures). A keyless `AppState` cannot sign
      in (`login_handler` answers `auth_disabled`,
      `crates/apb-server/src/routes/auth.rs` lines 73 to 79), so each
      converted test builds a keyed one with a new helper
      `keyed(state: AppState) -> (AppState, String)` that issues a key
      into a temp `server-auth.yaml` and attaches `AuthState::new(None,
      keys, &ServerConfig::default())` through `with_auth`, as
      `auth_endpoints_test.rs` `authed_with` does (line 21), returning
      the key. A second helper `sign_in(state: &AppState, key: &str) ->
      String` posts `/api/auth/login` with `{ "key": key }` and the CSRF
      marker (as `login_request` does, line 49) through
      `build_router(state.clone())` and returns the `<SESSION_COOKIE>=<value>`
      pair of its `Set-Cookie`. The session lives in the auth state the
      clones share (`auth_endpoints_test.rs` module doc, lines 1 to 3), so
      every later request of the test goes through
      `build_router(state.clone())` of the same state and carries
      `Cookie: <pair>` and `CSRF_HEADER: CSRF_VALUE`: the approval and
      every other request too (the listing, the detail, the healthcheck,
      the dry run), since a keyed server refuses a request without a
      credential. `post_json` and `get_json` gain a `cookie:
      Option<&str>` argument for this; the file's other callers pass
      `None`. The bearer test of item 2 uses `keyed` and sends
      `Authorization: Bearer <key>` instead. No other test posts to the
      route (`ingest_test.rs` line 530 only lists it as absent from the
      ingest router).
2. The untrusted-output paragraph is not hidden by an inbox grant.
   `instruction_block` (`crates/apb-engine/src/connector/prompt.rs`
   lines 85 to 96) skips the untrusted-output paragraph whenever any
   granted function is an inbox function, although the inbox paragraph
   speaks only of inbox events. The condition becomes: print it when a
   granted function that is not an inbox function has
   `untrusted_output`:
   `granted_functions(grants, docs).any(|f| f.untrusted_output &&
   !f.is_inbox())`; the `inbox_covered` binding and the comment
   sentence "Inbox grants already say it in their own paragraph." go.
   An inbox grant alone still gets only the inbox paragraph.
   Test in `prompt.rs` `mod tests`, next to
   `an_inbox_grant_keeps_only_the_inbox_paragraph` (line 598):
   `an_inbox_grant_beside_an_untrusted_function_keeps_both_paragraphs`
   (the inbox grant of `sample_inbox_docs` and an untrusted grant of
   `sample_untrusted_docs` in one block: "untrusted external input" and
   "written by people outside this run" each appear exactly once). The
   existing test keeps passing.

Wrong implementations:

1. Refusing only for one connector kind, or only the account form:
   caught by both refusal tests (connector and account).
2. Loading the connector or the trust store before the refusal: caught
   by review (the refusal is the first statement).
3. Guarding the HTTP route with `StepCaller::from_env`: a step reaches a
   dashboard started outside the run, whose own environment has no run
   id. Caught by
   `approve_endpoint_refuses_a_keyless_request_from_a_local_process`.
4. Letting a bearer key approve: caught by
   `approve_endpoint_refuses_a_bearer_key`.
5. Keeping `inbox_covered` and adding the paragraph a second time:
   caught by the "exactly once" assertions.

## Shared files with the changes in flight

Read from their documents and the eval-doubles code on 2026-11-20. None
takes a code from V300 to V339 except mcp-tool-connectors (V300 to V319);
none adds a run guard name this change needs (this change adds none);
none adds a `ClaudeAdapter` user; none changes `ProjectChecksSpec`,
`baseline::project::run` or `input_digest`.

| File | Other change and what it does there | This change |
|---|---|---|
| `crates/apb-core/src/agents/hermes.rs` | incremental-lists: `fork: None` in the `Forms` literal right after `resume:` | the `resume:` line and a test module; a one-line neighbour conflict |
| `crates/apb-cli/src/connector.rs` | mcp-tool-connectors: an MCP-only step refusal and `--expect` in `approve_cmd`; mcp-http-and-surfaces: `Tools` and doctor rows | the step refusal at the top of `approve_cmd` (D4.13); with it in place, mcp-tool-connectors' own refusal is already satisfied and is dropped there or kept as a no-op, never a second message |
| `crates/apb-server/src/routes/connectors/mod.rs`, `crates/apb-server/tests/suite/connectors_api_test.rs` | mcp-tool-connectors and mcp-http-and-surfaces: other handlers and tests; mcp-tool-connectors keeps main's behaviour of this route for MCP connectors (its design, line 1138) | the session-only channel check in `approve_connector_handler`, which then also covers MCP connectors; its two refusal tests and the session helper for the file's existing approvals |
| `crates/apb-engine/src/connector/prompt.rs` | mcp-tool-connectors: note 19 of its review record names this defect; it edits lines 146 to 194 | the paragraph condition (D4.13); note 19 is settled here. Its note 18 (the `{{cmd:...}}` helper found through `PATH`) stays open there: pinning the helper's path and content at approval is the mechanism that change introduces for its server program |
| `crates/apb-engine/src/scheduler/list/check.rs` | incremental-lists: the runner it asks (line 328); secret-params-in-run-files: `render_text` (lines 371 to 400) | the one `holds_tree` field of `read_ctx` (line 146, D4.2) |
| `crates/apb-core/src/fsutil/beneath.rs` | mcp-tool-connectors: reads through `read_regular_capped` (a caller, no edit named) | the new `file_stamp` beside `file_size` (D4.2) |
| `crates/apb-engine/src/scheduler/node.rs` | incremental-lists: the session fork | the rename only (lines 2092 and 4373) |
| `crates/apb-engine/tests/suite/connector_run.rs` | eval-doubles: a `false` argument at three calls | the rename only (lines 16 and 166) |
| `crates/apb-engine/tests/suite/send_intent_test.rs` | eval-doubles: `doubled_accounts` in two `ManifestConnector` literals of the helpers | two new tests through the helpers |
| `crates/apb-cli/src/doctor.rs` | start-rules: trigger lines near the health line | the store loop at the end of `doctor_env` |
| `crates/apb-cli/src/run.rs` | start-rules: the "started by rule" line and a pass in `resume_all_cmd` | the rename (one comment, line 1525) |
| `crates/apb-engine/tests/suite/list_run_test.rs` | incremental-lists: reuses its fixtures, no edit | three new tests |
| `docs/HOWTO-authoring.md` | eval-doubles, incremental-lists, inbox-waits: other sections | the `project_checks` bullet and its validator paragraph |
| `docs/CONNECTORS.md` | start-rules and inbox-waits: line 454 area | the gitlab section |
| `CLAUDE.md`, `AGENTS.md` | every change: module map text | two module map phrases |

With secret-params-in-run-files (split from this change) it shares no
code file: that change refuses a rendered secret param in the validator
(V340) and checks it at resume, so it no longer touches the adapter,
the spawn envs, the judge, the supervisor, `crates/apb-cli/src/run.rs` or
the connector and baseline tests. Both edit only neighbour sections of
`docs/HOWTO-authoring.md`, `CLAUDE.md` and `AGENTS.md`. This change
merges first.

This change owns `ProjectChecksSpec.junit`, `project_checks::junit`,
`scope_move::stale_claims`, `fsutil::lock_held`,
`scope_move::StaleClaim`, `scope_move::ClaimLeft`,
`versioning::is_draft_debris`, `Beneath::file_stamp`,
`ReadCtx.holds_tree` and
`CliAdapter`. The
draft no longer touches `crates/apb-cli/tests/main.rs` (the doctor test
moved into `diagram_cli_test.rs`), which eval-doubles and start-rules both
edit.

## Decisions pending the owner

These follow the plan's recommendation; the owner may overrule them.

1. Process env default (plan item 10, verdict NO). Keep `inherit` as the
   1.x default: flipping it after one-point-oh-freeze would break 1.x, and
   flipping it now breaks every profile that relies on an inherited
   variable on upgrade. Isolated nodes already force the allowlist. D4.3
   only lets a person set the policy per profile.
2. Secret param values in `run.yaml` (plan item 16): decided in the
   change secret-params-in-run-files.

## Files

Sixty-two changed files, counted the way the 1.0 plan counts them
(tests, docs, records and the openspec documents included; the spec
folder is one file).

1. `connectors/gitlab/connector.yaml` (D4.1)
2. `connectors/gitlab/tests.yaml` (D4.1)
3. `connectors/gitlab/PUBLIC.md` (D4.1)
4. `crates/apb-engine/tests/suite/send_intent_test.rs` (D4.1)
5. `docs/CONNECTORS.md` (D4.1)
6. `crates/apb-core/src/project_checks.rs` (D4.2)
7. `crates/apb-core/src/project_checks/junit.rs` (new) (D4.2)
8. `crates/apb-core/src/validate/baseline.rs` (D4.2)
9. `crates/apb-core/tests/suite/validate_baseline_test.rs` (D4.2)
10. `crates/apb-engine/src/baseline/project.rs` (D4.2)
11. `crates/apb-engine/tests/suite/baseline_test.rs` (D4.2)
12. `docs/HOWTO-authoring.md` (D4.2)
13. `web/src/lib/profileedit.ts` (D4.3)
14. `web/src/lib/profileedit.test.ts` (D4.3)
15. `web/src/pages/ProfileEdit.svelte` (D4.3)
16. `crates/apb-core/src/scope_move.rs` (D4.6)
17. `crates/apb-core/tests/suite/scope_move_test.rs` (D4.6)
18. `crates/apb-cli/src/doctor.rs` (D4.6)
19. `crates/apb-cli/tests/suite/diagram_cli_test.rs` (D4.6)
20. `crates/apb-core/src/agents/hermes.rs` (D4.9)
21. `crates/apb-engine/src/adapter.rs` (D4.10)
22. `crates/apb-engine/src/agent_probe.rs` (D4.10)
23. `crates/apb-engine/src/scheduler/quota_probe.rs` (D4.10)
24. `crates/apb-engine/src/scheduler/judge.rs` (D4.10)
25. `crates/apb-engine/src/scheduler/node.rs` (D4.10)
26. `crates/apb-engine/src/scheduler/supervisor.rs` (D4.10)
27. `crates/apb-cli/src/run.rs` (D4.10)
28. `crates/apb-engine/tests/suite/acp_adapter_test.rs` (D4.10)
29. `crates/apb-engine/tests/suite/adapter_bounded_waits_test.rs` (D4.10)
30. `crates/apb-engine/tests/suite/adapter_test.rs` (D4.10)
31. `crates/apb-engine/tests/suite/agent_contract/stub.rs` (D4.10)
32. `crates/apb-engine/tests/suite/agent_env_live_test.rs` (D4.10)
33. `crates/apb-engine/tests/suite/agent_env_spawn_test.rs` (D4.10)
34. `crates/apb-engine/tests/suite/connector_run.rs` (D4.10)
35. `crates/apb-engine/tests/suite/marker_test.rs` (D4.10)
36. `crates/apb-engine/tests/suite/process_group_test.rs` (D4.10)
37. `crates/apb-engine/tests/suite/retry_test.rs` (D4.10)
38. `crates/apb-engine/tests/suite/zcode_adapter_test.rs` (D4.10)
39. `crates/apb-engine/tests/suite/list_run_test.rs` (D4.11)
40. `crates/apb-engine/tests/suite/join_loop_test.rs` (D4.11)
41. `openspec/changes/one-point-oh-fixes/proposal.md` (records)
42. `openspec/changes/one-point-oh-fixes/design.md` (records)
43. `openspec/changes/one-point-oh-fixes/tasks.md` (records)
44. `openspec/changes/one-point-oh-fixes/specs/one-point-oh-leftovers/spec.md` (records)
45. `docs/superpowers/specs/<implementation date>-one-point-oh-fixes-design.md` (new) (records)
46. `CLAUDE.md` (records)
47. `AGENTS.md` (records)
48. `crates/apb-core/src/fsutil.rs` (D4.6 `lock_held`)
49. `crates/apb-core/src/fsutil/beneath.rs` (D4.2 `file_stamp`)
50. `crates/apb-engine/src/baseline/ctx.rs` (D4.2 `holds_tree`)
51. `crates/apb-engine/src/scheduler/baseline.rs` (D4.2 `holds_tree`)
52. `crates/apb-engine/src/scheduler/list/check.rs` (D4.2 `holds_tree`)
53. `crates/apb-engine/src/follow_up/read.rs` (D4.2 `holds_tree`)
54. `crates/apb-core/src/versioning/mod.rs` (D4.6 `is_draft_debris`)
55. `crates/apb-cli/src/connector.rs` (D4.13)
56. `crates/apb-cli/tests/suite/connector_cli.rs` (D4.13)
57. `crates/apb-server/src/routes/connectors/mod.rs` (D4.13)
58. `crates/apb-server/tests/suite/connectors_api_test.rs` (D4.13)
59. `crates/apb-engine/src/connector/prompt.rs` (D4.13)
60. `crates/apb-core/src/baseline/extract.rs` (D4.2 `checked_text` made `pub`, implementation review fix round)
61. `crates/apb-engine/src/scheduler/entry.rs` (D4.2 `holds_tree`, implementation review fix round)
62. `web/src/pages/ProfileEdit.svelte.test.ts` (new) (D4.3 page test, implementation review fix round)

A caller of `ClaudeAdapter` that a change merged earlier added raises
the rename by one file each, and a fix a D4.11 pinning test forces adds
the file it fixes; each is named in the final report. The change stays
under 65 files in every case.

## Code map

| Area | File | What |
|---|---|---|
| core | `src/project_checks.rs`, `src/project_checks/junit.rs` (new) | `junit`, the reader |
| core | `src/validate/baseline.rs` | V320 |
| core | `src/scope_move.rs` | `stale_claims`, `STALE_CLAIM_SECS` |
| core | `src/agents/hermes.rs` | resume form |
| core | `src/versioning/mod.rs` | `is_draft_debris` |
| core | `src/fsutil/beneath.rs` | `file_stamp` |
| engine | `src/baseline/ctx.rs`, `src/scheduler/baseline.rs`, `src/scheduler/list/check.rs`, `src/follow_up/read.rs` | `holds_tree` |
| engine | `src/connector/prompt.rs` | the untrusted-output paragraph |
| cli, server | `src/connector.rs`, `src/routes/connectors/mod.rs` | the approve refusal: a run step on the CLI, every channel but a signed-in session over HTTP |
| engine | `src/adapter.rs` and its users | `CliAdapter` |
| engine | `src/baseline/project.rs` | the JUnit read |
| core | `src/fsutil.rs` | `lock_held` |
| cli | `src/doctor.rs`, `src/run.rs` | doctor line, rename |
| web | `src/lib/profileedit.ts`, `src/pages/ProfileEdit.svelte` | `process_env` fields |
| content | `connectors/gitlab/{connector.yaml,tests.yaml,PUBLIC.md}` | marker |

## Release notes

This change bumps no version: its lines go into the release notes of the
release candidate, which one-point-oh-freeze writes from the archived
proposals.

1. GitLab connector 0.3.0: `create_release` carries a hidden marker in
   its description. A retry after a lost answer used to fail with
   GitLab's 409 for the existing tag; it is now recognized as already
   sent (looked up with the new `list_releases`), and an outcome that
   cannot be settled is a send a person resolves (`apb intent
   resolve`). The connector reads its own account (`identity`).
2. Baseline: `project_checks.junit` reads failing tests from the JUnit
   XML reports a `test` command writes (V320), in a run that holds its
   working tree; a run that shares it reads them unknown. An older apb
   refuses a definition that sets it.
3. Fixes: hermes's resume form is a valid command line; `apb doctor`
   names a playbook directory a stopped copy or draft creation left,
   with a quoted `rmdir` command only when it is empty and a request to
   inspect it otherwise (never one whose lock is held under the lock's own
   60 s rule); the dashboard
   profile editor sets `process_env`.
4. Connectors: `apb connector approve` is refused inside a run step,
   and the dashboard approves a connector or an account only through a
   signed-in session (a dashboard without keys and a bearer key are
   refused and shown the CLI command); a node granted an inbox function
   beside other untrusted functions gets both warnings in its prompt.

## Rebased on main f0340685

The change was rebased on main f0340685 (MCP tool connectors, #293).
What both changes touch was reconciled so that both hold:

1. Connector approval. On the CLI the run step refusal at the top of
   `approve_cmd` comes first for every connector kind, before anything is
   read; the MCP connector's own refusal (`connector_mcp::refusal`) stays
   behind it and still refuses a caller without an interactive terminal.
   A run step therefore reads "approving a connector is refused inside
   run ...", and the two MCP run step tests expect that text. Main's
   test that an HTTP connector approves from a run step without a
   terminal now expects the step refusal for both forms, and proves
   outside a run step that the MCP terminal rule does not reach an HTTP
   connector. The two MCP healthcheck tests take the session argument
   the file's request helper gained. Over HTTP
   the session check is still the first statement of
   `approve_connector_handler`; a signed-in session then reaches the MCP
   check, which refuses `mcp_approve_needs_a_person` as before. The MCP
   dashboard test now signs in, so it proves that even a signed-in
   session never approves an MCP connector, and checks that a request
   without a session is refused by the session check first.
2. The untrusted paragraph of the connector prompt. Main made the
   condition `FunctionSpec::is_untrusted_output` (an MCP function is
   untrusted by default); the inbox fix keeps it and adds `!is_inbox()`,
   so an MCP grant still adds the paragraph once and an inbox grant beside
   another untrusted function still carries both paragraphs. Both sides'
   tests are kept.
3. `docs/CONNECTORS.md`: "Approving trust" names the run step refusal for
   every kind and points to "MCP servers" for the HTTP rule; "MCP servers"
   says the run step refusal comes first and that a signed-in session is
   refused too.
4. Validator codes: main took V300 to V302 for MCP connectors inside its
   V300 to V319 family; this change keeps V320. `CLAUDE.md` and
   `AGENTS.md` carry both sides' module map text and stay mirrored, and
   `docs/HOWTO-authoring.md` merged without overlap. Main did not change
   the doctor, so its stale claim line needed no reconciliation.
5. CI on macOS failed main's test
   `connector_mcp_send::a_kill_once_the_intent_is_recorded_is_never_sent_again_blindly`:
   it counted the hand-over right after killing the caller, while on
   macOS (no parent-death signal) the server outlives the caller and may
   still read the `tools/call` left in its pipe and record it, so the
   original call looked like a repeat. The test now ends the server the
   way its sibling test does on that platform and counts the hand-over
   only once the server has ended, on every platform. No product code
   changed.

## PR review

The bot review of the pull request found three defects in the JUnit
reader and its walk; each is fixed with a test that failed first:

1. A start tag that named an attribute twice (`name="new" name="old"`)
   was read with the later value, a known identity from XML that is not
   well-formed. The reader now refuses an attribute named twice in one
   start tag.
2. A literal character XML does not allow (a NUL, a C0 control other
   than tab, newline and carriage return, U+FFFE, U+FFFF) passed the
   UTF-8 pass while its reference `&#0;` was refused. The chunked UTF-8
   pass now checks every character against the same `is_xml_char`.
3. `match_depth` gave a glob with a character class a fixed depth, but
   globset lets a negated class match `/` even with a literal separator,
   so `reports/a[!x]b.xml` also matches `reports/a/b.xml`, which the
   walk skipped. A glob with a class now has no fixed depth and is walked
   like `**`; HOWTO-authoring says so.

Its one nitpick was taken as well: the cancellation test of large
constructs cancelled during the UTF-8 pass, so it could not tell whether
the scanner itself checks. It now counts the UTF-8 pass's checks, proves
a check inside the scan, and cancels at the scanner's first one.

## Implementation notes

1. The web steps need `web/node_modules` (`bun install --frozen-lockfile`
   in `web/`).

## Review record

The passes below are history. What they say was fixed in D4.7, D4.8 or
D4.13's helper search path was cut in the fourth pass and is not built
(see "Cut in the fourth pass").

### Fifth implementation review fix round (2026-11-20)

Source: an independent review; one major and two minor issues.

1. Major, a `junit` command that failed while its reports named no
   failing test gave an empty list, and the comparison read "met". Fixed:
   D4.2 "Exact semantics" item 11, a list only when it explains the
   outcome; the mirror case (a pass with a failing case named) is decided
   there too. Tests `a_failure_no_report_explains_reads_unknown` (unit:
   unknown, a before `[X]` against it compares unknown, and the mirror
   keeps its list) and `a_failure_the_reports_do_not_explain_compares_unknown`
   (engine: a `make test` writing one passing case and exiting 1 reads
   unknown before and after, and the comparison is unknown). Both fail
   with the fix removed.
2. Minor, `connectors/gitlab/PUBLIC.md` said an absent marker counts as
   not sent; the engine leaves the send unknown for a person
   (`reconcile` maps an absence to unknown). Fixed with the review's
   wording, and `docs/CONNECTORS.md` "gitlab" says the same; no other
   connector's PUBLIC.md has a marker paragraph.
3. Minor, `a_gitlab_release_beyond_the_first_page_stays_unknown` passed
   without the paging rule. Fixed: it asserts that the refusal and the
   settlement detail name "more pages exist"; it fails when the
   `rel="next"` reading in `send_once::read_back` is broken.

Found while running the gates: `items_of_several_workers_record_no_commits`
failed once in the full workspace run (an item needs a look: its commit
failed). The list test's item script now tries a commit that meets a
transient `index.lock` (git's own, or one a read of the tree took) up to
ten times before it fails the item; a commit that still fails fails the
item as before.

### Fourth implementation review fix round (2026-11-20)

A third independent pass found two major issues with one cause, a
failing test's identity being a joined display string, and one minor.

1. Major, the classname "A::B" with the name "C" equalled the classname
   "A" with the name "B::C".
2. Major, the repeat numbering "x #2" equalled a test named "x #2".
   Both fixed with one rule (D4.2 "Exact semantics" item 9): the
   comparison key is unambiguous by construction and never the display
   text. JUnit items are the injective JSON key with the occurrence as its
   own component; `cargo test` items keep their form for compatibility
   with journaled before snapshots, with an injective repeat numbering.
   Item 9 lists every place items are compared, deduplicated, sorted or
   counted.
3. Minor, a UTF-16 byte order mark returned before the cancellation
   check. Fixed: `failed_tests_within` runs the check before it looks at
   any content; the UTF-8 pass checks before each later chunk.

Tests, each failing with its fix removed:
`two_different_tests_never_share_a_key`,
`distinct_fields_never_give_equal_keys` (20000 generated fields made of
the separators), `a_swap_between_two_failing_runs_is_a_new_failure`,
`cargo_repeat_numbering_is_injective`,
`a_cancel_is_seen_before_the_byte_order_mark`; `a_key_is_never_cut`
holds the bound on the encoded form.

### Third implementation review fix round (2026-11-20)

A second independent pass found one major and one minor issue.

1. Major, a report near the 16 MiB cap with hundreds of thousands of
   short failing cases was stored and sorted whole, and the engine
   accumulated across up to 64 reports before the 500-item cap. Fixed:
   D4.2 "Exact semantics" item 10, the cap enforced where items are
   produced in the reader and carried from report to report in the
   engine, with the stored bytes bounded (at most 500 identities of at
   most 248 characters). Tests `more_failing_tests_than_the_cap_is_unknown`
   (500 read, 501 unknown with the reader's own reason, so nothing was
   sorted) and `the_failing_cap_stops_the_walk_at_the_report_that_passes_it`
   (64 reports of 250 failures each: unknown naming the third report, and
   exactly three reports handed to the reader).
2. Minor, the UTF-8 validation scanned a whole report before the first
   check. Fixed: D4.2 item 8, the check right after the read and a chunked
   validation. Tests `a_cancel_is_seen_before_the_utf8_pass` (a report not
   UTF-8 from its first byte reads "cancelled" with one call of the
   check) and `the_utf8_pass_handles_every_split_position` (every split
   position of a two-, three- and four-byte sequence over chunk sizes 4 to
   8, invalid sequences at every boundary, a sequence the input cuts).
   Each of the four tests fails with its fix removed.

### Second implementation review fix round (2026-11-20)

An independent code review found 5 major and 2 minor issues, all in the
JUnit reader and its walk. They are one class, fixed with one principle,
stated at the top of `junit.rs` and in D4.2: the reader returns a list
only for a report it understood completely; anything else is unknown with
a named reason, never a partial or guessed list.

1. Major, a cut identity hid a new failure (two names agreeing on their
   first 300 characters became one item). Fixed: `failing_item` never
   cuts; an identity too long for an item, or one the observation text
   rules refuse, makes the list unknown, the reason naming the report.
   This replaces the first round's cut (item 3 below).
2. Major, `</testcase >` was not seen as an end tag, so the next case's
   failure was credited to it. Fixed: end tags take optional XML
   whitespace before `>`.
3. Major, a report a runner stopped writing gave the cases so far. Fixed:
   a report must be complete and balanced (D4.2 "Exact semantics" items 1
   to 5); a UTF-16 byte order mark, another declared encoding, a DOCTYPE
   or entity declaration and every unterminated construct are unknown.
4. Major, the walk's depth limit dropped a deeper failing report. Fixed:
   the walk descends only where a match can lie, and a cap that stops it
   before every matching path was examined (depth, budget, report count,
   a name that is not UTF-8, a link or special file where a report could
   lie) is unknown naming the cap.
5. Major, a large comment or attribute value scanned without the
   deadline or cancel check. Fixed: every loop moves through one cursor
   that runs the check every `CHECK_EVERY` bytes, and once more before a
   list is returned.
6. Minor, `&#00000000000065;` read as written. Fixed: references are
   decoded in one forward pass with any number of leading zeros; an
   invalid or out-of-range reference is unknown.
7. Minor, the cancellation test could pass without the guard. Fixed: the
   command is a program that does not exist; without the guard the status
   reads "could not start".

### Implementation review fix round (2026-11-20)

The branch was rebased onto main 0336dada (Secret params are never
rendered, #292, after Eval doubles, #291), with no conflict; the one
semantic break, a `ManifestConnector` literal of this change's prompt test
missing main's new `doubled_accounts`, is fixed in that commit. Two
reviews of the implementation, a conformance review (1 blocker, 1 major,
8 minor) and a security and test audit (1 blocker, 8 major, 8 minor),
were each checked against the code before a fix. Every finding is fixed
or recorded with its reason; nothing the earlier passes cut came back.

1. Blocker (both reviews), the JUnit reader panicked on a non-ASCII
   character outside an attribute value: it stepped one byte at a time and
   sliced the text there. Fixed in D4.2: `failed_tests` takes the bytes,
   matches only ASCII markers and decodes only the values it keeps; tests
   with non-ASCII text in a suite name, a test name, a failure message,
   `system-out`, CDATA and a comment, bytes that are not UTF-8, every byte
   prefix of such a report, and 20000 generated byte strings. The engine
   has no guard against a panicking baseline read (its one `catch_unwind`
   is the batch worker's), so D4.2 item 7 states why no input can panic.
2. Audit major 1, quadratic entity decoding and no deadline inside a
   report. Fixed: one pass with a `;` looked for only within an entity's
   longest body, and the command's deadline and the cancel flag checked
   every `CHECK_EVERY` bytes while a report is parsed.
3. Audit major 2, JUnit names past the observation text rules. Fixed:
   every failing test becomes one `failing_item` held to
   `baseline::extract::checked_text` (now `pub`, the one rule every
   observed list item is held to), cut within 256 characters with room
   for a repeat's number; a report path is held to the same rule before
   it names anything. The 512-byte name cut and its public constants are
   gone. Baseline observations are marked untrusted nowhere on main (the
   untrusted-input analysis, `untrusted.rs`, has no baseline source, and
   MCP `run_status` serves them unmarked), so there is no marking to
   extend; the text rules are what holds them.
4. Conformance major and audit minor 3, task 6.3. Cut from this change:
   the ignored test is deleted, the spec scenario, D4.11 item 2 and the
   tasks state what is pinned, and the passing test ties each commit
   record to its item by journal order and subject. Future work in the
   proposal ("Out of scope" item 11).
5. Audit major 3, the CLI connector approval refused only by the
   environment. Accepted as recorded in D4.13: a mistake guard, like every
   `StepCaller` refusal; a terminal-only or passkey-bound approval is
   future work ("Out of scope" item 12), and MCP connectors get the
   terminal rule in their own change.
6. Audit major 4, `process_env` loosened through any channel that saves a
   profile. Pre-existing on main and not fixed here; "Out of scope" item
   13.
7. Audit major 5, a hermes session id taken from the reply. Recorded in
   D4.9: every captured id already passes `is_plausible_session_id`; the
   empty id it let through is refused now, and a test pins hermes. Any
   other value is no session, and the cold start journals
   `interaction_downgraded`.
8. Audit majors 6 to 8, portability. The list tests use a `mkdir` lock
   directory and fail an item whose commit fails; the doctor CLI test
   compares canonicalized paths; the cancellation and allowance tests set
   the flag, or pass a past deadline, before the call, with no thread,
   timing or 16 MiB file.
9. Conformance minor 1, a backslash glob V320 let through. Fixed: V320
   refuses any `\` (its rule 4 now), so V320 and the engine split
   components the same way.
10. Conformance minor 2, copies of one rule. Fixed: one `is_wildcard` and
    one `matcher` in `project_checks` for V320 and the engine, one
    `entry::holds_tree` for the drive and both read contexts.
11. Conformance minor 3, a written report read twice. Fixed: the after
    listing parses each written report from the read whose digest decided
    it.
12. Conformance minor 4, tasks 3.1 to 3.3 unticked. Ticked.
13. Conformance minor 5 and audit minor 7, the doctor's lines. Fixed: the
    `Empty` line's path goes through `sanitize_plain`; a project claim
    joins its failed load line instead of a second line; an empty
    directory with a staged copy of its id beside it is `Staging`, never
    offered `rmdir`.
14. Conformance minor 6, `flock(1)`: item 8.
15. Conformance minor 7, two premises the code proved false (the
    reconcile reads the projection; nothing reports a stopped claim).
    Corrected in D4.1 item 4 and D4.6.
16. Conformance minor 8, the public record pointed at documents not beside
    it. Fixed in the record's text.
17. Audit minor 1, the GitLab reconcile's paging edge. Fixed: a test of a
    listing with a further page (the send stays unknown), and PUBLIC.md
    and CONNECTORS.md state the limit (the 20 newest releases) and what a
    release beyond it, or a resend after an absent marker, does. The
    marker in a release description is the same design as GitHub's.
18. Audit minor 2, the approval route's negative cases. Fixed in D4.13's
    tests: the keyless page's own request and a same-origin request, the
    account form under a bearer key, the trust unchanged after each
    refusal, and a signed-in session approving while the server's
    environment names a run. A passkey-bound approval is item 5's future
    work.
19. Audit minor 4, duplicated proofs. Folded: the engine tests
    `a_literal_junit_path_lists_the_failing_tests` and the first half of
    `a_shared_workdir_run_reads_junit_unknown` are gone (their unit tests
    own the contract), and the scope move test no longer asserts
    `lock_held`, which fsutil's own test owns.
20. Audit minor 5, the digest pin. Fixed: a unit test of `input_digest`
    over fixed commands replaces the fixture-bound value.
21. Audit minor 6, exports and a staleness rule written twice. Fixed: no
    cut constant is exported, and `lock_held` and `lock_is_stale` read
    one `stale_at`, with a test of their agreement.
22. Audit minor 8, the page-level process_env save. Fixed:
    `web/src/pages/ProfileEdit.svelte.test.ts` (an untouched save sends no
    `process_env`, a changed policy and an edited allowlist send theirs).

### This pass (closure, 2026-11-20)

Every claim of the draft was checked against main efae5822; the draft was
written on 316c8180.

1. Base and sequence: main now holds #287 to #290; the old sequence names
   (typed-inbox-and-released-waits, start-rules-and-schedules,
   evals-and-inbox-waits) are replaced by the changes as they are now
   (eval-doubles, start-rules, incremental-lists-and-session-fork,
   inbox-waits, and the mcp drafts), with a table of shared files and
   symbol ownership.
2. Validator codes: verified free on efae5822 (highest code V250) and
   against every other change's block; the table in D0 replaces the
   draft's guessed ranges (start-rules takes V260 to V267, eval-doubles
   V280 and V281, incremental-lists V283, inbox-waits V284 to V286).
3. D4.1: the `list_releases` `response_pick` hedge closed (the official
   connector gate requires it, and dotted paths project); the identity
   comment corrected (the connector takes a personal access token, not a
   project or group token); `since_arg` absence explained; the tests.yaml
   case and the engine test made concrete, plus a second engine test for
   another account's release.
4. D4.2: the freshness rule changed from "modified at or after a
   `SystemTime::now()` taken before the command" (a coarse file system
   clock can stamp a fresh report earlier) to a before and after listing
   of the matched reports compared on their own times; every read and
   listing goes through `fsutil::beneath` instead of a hand walk; line
   numbers moved (HOWTO 4763 to 5215, 4840 to 5290).
5. D4.3: the code contradicted the draft. `profile_get` already returns
   the stored `process_env` inside `profile_yaml`, and the editor reads
   every setting through `parseProfileDoc`. The `profile_get` key, its MCP
   test, `ProfileDetail` and the new `processenv.ts` are dropped; the
   helpers go into `profileedit.ts`, and the "untouched save" rule is one
   pure function with its own test.
6. D4.6: `stale_claims` takes the `playbooks/` directory so the doctor
   reuses its existing store loop; the doctor test moves into
   `diagram_cli_test.rs` (no new file, no `tests/main.rs` edit that would
   collide with eval-doubles and start-rules).
7. D4.7: the code contradicted the draft's `OPENCODE_LOGIN_VARS` (the union
   of `provider_keys`): any passed name of it skips the auth file bind
   (`home::prepare`), and `OPENCODE_*` (opencode's own set) and
   `GITHUB_TOKEN` are often passed. opencode declares no login variable.
   The module unit tests move into the existing
   `agents_test.rs::sandbox_needs_are_declared_for_claude_and_codex_only`,
   which the draft missed and which would have failed. The engine tests
   name the skip helper (`common::sandbox_or_skip`) and the stub source
   (the agent contract fixtures).
8. D4.8: `CLAUDE_CONFIG_DIR` is in claude's own env set, so the plan check
   is stated precisely; `home::prepare`'s argument order and its four
   test callers named.
9. D4.9: re-checked on hermes 0.23.2. The release note no longer claims
   "hermes resumes a session again": one-shot mode prints no session id,
   so the resume form is rarely reached.
10. D4.11: the line numbers moved (`batch.rs` 280 to 402,
    `run_config.rs` 187 to 156; `provenance.rs` 236 is `parked_path`); the
    secret test is made concrete and its failure path decided (the run
    redactor at the write site, the rest named as future work).
11. Owner decision 1: the draft said start-rules warns when a profile
    inherits; its documents do not, so the sentence is removed.
12. Files: 61 to 58 (D4.3 from 6 to 3, D4.6 from 5 to 4, the sandbox and
    hermes group from 10 to 11 with `agents_test.rs`).

### Second pass (design review, 2026-11-20)

An outside design review found 1 blocker, 5 major and 2 minor issues and
noted 2 facts pre-existing on main. Each was checked against the code on
main efae5822 (this worktree's base) before it was applied; the owner
decided eight points, recorded with the findings they settle.

1. Blocker, the secret param requirement had an escape clause (the
   draft's D4.11 item 4 let a failing site be "future work").
   Confirmed. V99 and V100 refuse only a default and a non-text type
   (`crates/apb-core/src/validate/params.rs` lines 12 to 32), so a
   reference reaches prompts, script and connector arguments. The draft's
   assumption that only `run.yaml` holds the value is false on main: the
   host prompt is written verbatim to `tasks/<id>/prompt.md`
   (`crates/apb-engine/src/host_task.rs` line 358, from
   `scheduler/host.rs` line 289); the journal keeps an executor's raw
   output, and `run_secrets_test.rs` asserts it (line 496); stream logs
   replace only a cloud key (`capture/capped_file.rs` line 32); claude's
   session copy is made after the adapter's scrub (`scheduler/node.rs`
   line 2775); connector URLs, baseline observations and write evidence
   are journaled unscrubbed (`connector/call/mod.rs` line 1205,
   `baseline.rs` line 124, `scheduler/write_scope.rs` line 246); and a
   value under 6 bytes is never scrubbed (`apb_core::scrub` lines 45 and
   133; the start only warns, `gate/params.rs` lines 95 to 122). Fixed:
   new D4.12 with the table of all 25 write sites, their state today and
   their fix (scrub at the attempt, scripts, connector URL, baseline,
   write evidence; refusal by name of host mode, of the host fallback, of
   a secret in a path (V321), of a send label and of a short value), and
   eleven tests covering cli and host execution. Owner decision 1 (no
   exception) is recorded there. D4.12 has since moved to its own change
   (see "Split" below).
2. Major, a literal JUnit path cannot be walked. Confirmed:
   `Beneath::list_dir` walks every segment of its argument as a directory
   (`crates/apb-core/src/fsutil/beneath.rs` lines 449 to 453), so
   `reports/junit.xml` as the walk root fails. Fixed in D4.2 (owner
   decision 2): `walk_root` is the longest literal directory prefix and
   never the file name position; tests
   `the_walk_root_is_the_literal_directory_prefix`,
   `a_literal_report_path_is_read`,
   `a_literal_junit_path_lists_the_failing_tests`.
3. Major, a rewritten report could count as untouched. Confirmed by the
   draft's own text (`(modified, len)` only). Fixed in D4.2 (owner
   decision 3): `ReportStamp` adds a SHA-256 of at most
   `MAX_REPORT_BYTES` (`sha2` is already a dependency of both crates);
   the time can only say "changed"; test
   `an_equal_length_rewrite_within_one_timestamp_tick_counts`.
4. Major, a rejected `CLAUDE_CONFIG_DIR` stayed in the agent's
   environment. Confirmed: the variable is in claude's always-passed set
   (`crates/apb-core/src/agents/claude/mod.rs` line 209), and the draft's
   `config_dir_of` returned `None` for a bad value, so apb would bind the
   default home's login while claude read the rejected path. Fixed in
   D4.8 (owner decision 4): `ConfigDir::Rejected` refuses the isolated
   attempt by name through the existing setup refusal
   (`scheduler/node/sandbox.rs` lines 195 to 198); test
   `claude_under_full_refuses_a_config_dir_it_cannot_follow`.
5. Major, the moved directory's files were read through links.
   Confirmed: `home::prepare` uses `is_file` and `std::fs::read` for a
   copy and `std::fs::metadata` for a bind
   (`crates/apb-engine/src/sandbox/home.rs` lines 35, 39 and 48), all of
   which follow a link. Fixed in D4.8 (owner decision 5): the moved files
   are read through `fsutil::beneath` anchored at the moved directory, a
   linked file refused by name; tests
   `a_linked_credentials_file_in_a_moved_dir_is_refused`,
   `a_linked_state_file_in_a_moved_dir_is_refused`,
   `a_moved_dir_swapped_for_a_link_is_refused`. bwrap still opens a bind
   source by path at spawn: that window is main's for every default-home
   bind and is stated, not widened.
6. Major, opencode's legacy login file was not bound. Confirmed:
   `auth_paths` recognizes `.local/share/opencode/auth.json` and then
   `.config/opencode/auth.json` (`crates/apb-core/src/agents/opencode/mod.rs`
   lines 148 to 155). Fixed in D4.7 (owner decision 6): one
   `AUTH_HOME_RELS` shared by the setup and the sandbox, a bind per
   entry (`home::prepare` skips a missing source, `home.rs` lines 47 to
   50); test `an_opencode_node_runs_under_full_with_its_legacy_auth_file`
   beside the existing one, and the needs test expects two binds. (Cut
   in the third pass, owner decision A.)
7. Minor, doctor could name a claim a live copy holds. Confirmed: a copy
   holds `<id>.lock` while its empty claim exists (`scope_move.rs` line
   97). The code contradicts the review's implied fix in one point: the
   lock is a `DirLock` token file (`crates/apb-core/src/fsutil.rs` line
   870), not an OS lock, and a killed copy leaves it behind, so "the
   lock file exists" would hide exactly the claims the doctor is for.
   Fixed in D4.6 (owner decision 7): `fsutil::lock_held` applies the
   lock's own staleness rule (`LOCK_STALE_MS`, line 893) read only; tests
   `an_old_empty_claim_whose_id_lock_is_held_is_not_named` and
   `an_old_empty_claim_with_a_stale_id_lock_is_named`.
8. Minor, task 0.4's precondition was false. Confirmed:
   `web/src/lib/journal.ts` lines 63 and 116 name `process_env` in
   comments. Fixed (owner decision 8): task 0.4 searches only
   `web/src/pages/ProfileEdit.svelte` and `web/src/lib/profileedit.ts`.
9. Pre-existing, host tasks persist rendered prompts: settled for secret
   params by secret-params-in-run-files (V340: no template renders a
   secret param, so no host prompt holds one).
10. Pre-existing, the project-check tree guard leaves ignored files out
    of its fingerprint (`crates/apb-engine/src/baseline/ctx.rs` line 58):
    accepted as it is. D4.2 depends on it (reports must sit in a
    git-ignored place, so writing them does not count as changing the
    tree); "Unchanged from main" item 2 already names it.

Files: the estimate went from 58 to 75 changed files (D4.6 added
`fsutil.rs`; D4.12 added sixteen), under a cap of 85.

### Split (2026-11-20)

D4.12 is a security fix with a behaviour change (routing reads scrubbed
output, new refusals), so it moved to its own change,
secret-params-in-run-files, for its own review. Removed here: D4.12, its
V321 (the new change takes V340; V321 returns to this family's reserve,
as one-point-oh-freeze's task 0.2 expects), its tests, its spec
requirements, its tasks (old section 9), its sixteen files and
`run_secrets_test.rs`, its release note and its wrong implementations.
The owner decision on secret values in `run.yaml` moved with it. The
proposal's item 10 is a pointer. The mcp row of D0 now reads V300 to
V302 taken, V303 to V319 reserved, as mcp-tool-connectors' documents
say. What the two changes share and the merge order are in
secret-params-in-run-files' `design.md` "Shared files": this change
merges first. Files: 75 to 58, under a cap of 65.

### Third pass (design review, 2026-11-20)

A second outside design review found no blocker, 3 major and 10 minor
issues and noted 6 facts pre-existing on main. Each was checked against
the code on main efae5822 before it was applied; the owner decided five
points (A to E), recorded with the findings they settle.

1. Major 1, the engine test binary sets `CLAUDE_CONFIG_DIR` for every
   test. Confirmed: `crates/apb-engine/tests/main.rs` line 111 sets it to
   `<dir>/claude` and nothing creates it; `sandbox_run_test.rs` `env(f)`
   (lines 125 to 133) does not remove it; the variable is in claude's
   always-passed set (`crates/apb-core/src/agents/claude/mod.rs` line
   209). Wider than the review said: the `best_effort` tests of
   `profile_run_test.rs` (lines 375 and 434), `host_mode_test.rs` (line
   1232) and `operator_pause_test.rs` (line 1217) would also degrade.
   Fixed in D4.8 "The existing tests and their environment" (owner
   decision B): the constructor creates the directory (the cost basis
   still sees no login), and `env(f)` removes the variable so that file
   keeps proving the default-home path; tasks 5.3 and 5.4 name both
   files.
2. Major 2, opencode's binds miss an XDG layout. Confirmed:
   `crates/apb-core/src/process_env/sets.rs` lines 38 to 41 pass the XDG
   variables, `SandboxPlan::overlay`
   (`crates/apb-engine/src/sandbox/mod.rs` lines 88 to 105) resets only
   `HOME`, the temp directories and `XDG_RUNTIME_DIR`, and
   `crates/apb-core/src/agents/opencode/mcp.rs` lines 2 and 20 follow
   `XDG_CONFIG_HOME`. Disposed of by owner decision A: the opencode part
   is cut (see "Cut in the third pass"); opencode stays refused under
   `full` as on main, the ISOLATION bullet says why, and "opencode under
   full isolation" is future work (proposal "Out of scope" item 9).
3. Major 3, a refusal contradicted `place`'s degrade path. Confirmed:
   `crates/apb-engine/src/scheduler/node/sandbox.rs` lines 254 to 263
   send every build error through `contract::without_sandbox`
   (`crates/apb-core/src/isolation/contract.rs` line 448), which refuses
   only at `full`, while lines 195 to 198 refuse unconditionally. Fixed
   in D4.8 (owner decision C): every cause is a build error, refused at
   `full` (`isolation_unavailable`, `unavailable` at line 103) and
   degraded at `best_effort` with the reason journaled; one table; four
   engine tests at both levels.
4. Minor 1, wrong code facts. Confirmed for the variants:
   `CappedRead` is `Read { bytes, digest }` or `Refused(ReadRefusal)`
   (`crates/apb-core/src/fsutil/beneath.rs` lines 39 to 59); the design
   now names `ReadRefusal::{Missing, Link, NotRegular, HardLink,
   TooLarge}`, and `ReportStamp.digest` is `CappedRead::Read.digest`
   (no second hash). The code contradicted the third point:
   `claude_needs()` (`crates/apb-engine/src/sandbox/home.rs` lines 186
   to 188) looks the needs up through `apb_core::agents::lookup`; it is
   not a literal and needs no change. The literals are claude's (no
   `..`, `claude/mod.rs` line 172) and codex's (`..SandboxNeeds::none()`,
   `codex/mod.rs` line 108).
5. Minor 2, the GitLab premise. Accepted as GitLab API behaviour (409 on
   a second release for the same tag; not visible in this repository).
   The 1 MiB cap is confirmed (`BODY_CAP`,
   `crates/apb-engine/src/connector/call/wire.rs` line 11). Fixed in
   D4.1, the proposal and the release note: "a retry after a lost answer
   fails with 409 and is now recognized as already sent"; `per_page: 20`
   with its tests.yaml case and a wrong implementation.
6. Minor 3, glob edge cases. Confirmed: `segments` refuses an empty or
   `.` segment (`beneath.rs` lines 107 to 122). Fixed in D4.2: V320
   also refuses a wildcard in the first component and an empty or `.`
   component (two tests), and the exhausted budget reads "the JUnit walk
   examined more than 10000 entries".
7. Minor 4, uncapped test names. Fixed in D4.2: `MAX_NAME_BYTES` 512
   and `CUT_MARKER`; a longer name is cut on a character boundary; unit
   test `a_name_over_512_bytes_is_cut_with_the_marker`.
8. Minor 5, the doctor's lock claim. Confirmed: `DirLock` is written
   once at `create_new` (`crates/apb-core/src/fsutil.rs` line 957), and
   `interrupted_draft` accepts hidden names and the two pointer files
   (`crates/apb-core/src/versioning/mod.rs` lines 1361 to 1390). Fixed in
   D4.6 and the spec: the claim says "whose id lock is not held under the
   lock's own 60 s rule"; `stale_claims` accepts what `interrupted_draft`
   accepts for a creation that published nothing, through one shared
   `draft_leftover`, with `rm -r` advice for such a directory; three
   tests.
9. Minor 6, a moved directory inside the tree. Fixed in D4.8:
   `config_dir_of` rejects a directory inside the tree or a writable
   extra (the extras are resolved before `home::prepare`); test
   `a_config_dir_inside_the_tree_or_a_writable_path_is_rejected`.
10. Minor 7, the ISOLATION text. Confirmed: `SYSTEM_READ_ONLY` holds
    `/etc` and `/opt` (`crates/apb-engine/src/sandbox/view.rs` lines
    37 to 39). Fixed: "nothing else of that directory is placed in the
    sandbox home", and a directory under a path every sandbox sees stays
    visible there read-only, as before.
11. Minor 8, `OPENCODE_CONFIG_CONTENT` in a `full` sandbox. Confirmed
    (`OPENCODE_*`, `crates/apb-core/src/agents/opencode/mod.rs` line
    138). Disposed of by owner decision A with major 2: opencode is not
    sandboxed by this change; the variable is one of the future work's
    reasons.
12. Minor 9, a value naming the default directory. Fixed in D4.8: a
    value equal to `<home>/.claude`, lexically or after canonicalizing
    both, is `ConfigDir::Default`; test
    `a_config_dir_naming_the_default_dir_is_default`.
13. Minor 10, the commits assertion. Confirmed: `NodeCommits`
    (`crates/apb-engine/src/run_outcome/commits.rs` lines 13 to 31) has no
    item field and `stamp_item` does not stamp `artifacts_committed`
    (`crates/apb-engine/src/list/stamp.rs` lines 11 to 24). Fixed in
    D4.11 item 2 and task 7.1: two `artifacts_committed` events whose
    `attempts[*].attempt` resolve to `attempt_started` events with item
    `a` and `b`, one `Apb-Run` commit each.
14. Pre-existing 1 to 6 (claude's session record read from the
    operator's directory, opencode's `auth_paths` ignoring XDG, a
    `DirLock` older than 60 s stolen by `lock_dir`, a dashboard without
    keys loosening `process_env`, a configuration directory under a
    system path visible whole, `auth_hint` and `memo_files` reading
    `~/.claude.json` with the variable set): noted, not changed here.
    Item 3 is stated in D4.6 as the rule the doctor follows; item 5 is
    stated in the ISOLATION text.
15. Owner decision E, three connector fixes another review found
    (pre-existing notes 17 to 19 of mcp-tool-connectors' review record),
    each verified on main: `approve_cmd`
    (`crates/apb-cli/src/connector.rs` line 345) and
    `approve_connector_handler`
    (`crates/apb-server/src/routes/connectors/mod.rs` line 520) have no
    step refusal; `resolve_cmd` starts `Command::new(program)`
    (`crates/apb-core/src/connector/secrets.rs` line 250, so `PATH`
    decides); `inbox_covered`
    (`crates/apb-engine/src/connector/prompt.rs` lines 88 and 89)
    suppresses the untrusted-output paragraph. Together they add six
    files, inside the eight allowed, so all three are in: new D4.13. For
    the helper the fixed search path was chosen over refusing a bare
    name, which would break the shipped GitHub connector's documented
    `{{cmd:gh auth token}}`.

Files: 58 to 64 (opencode's module out; the engine test constructor and
the six files of D4.13 in), under a cap of 70.

### Cut in the third pass

1. The opencode part of D4.7 (owner decision A): opencode's sandbox
   needs, the shared `AUTH_HOME_RELS` and its use in `auth_paths`, the
   two auth-file binds, its `opencode` arm with two binds in
   `agents_test.rs`, the engine tests
   `an_opencode_node_runs_under_full_with_its_auth_file` and
   `an_opencode_node_runs_under_full_with_its_legacy_auth_file`, the
   opencode live test, the spec requirement's opencode scenarios ("legacy
   login file", "a passed key"), the release note's opencode line with
   its passed-key limit, the opencode wrong implementations, and
   `crates/apb-core/src/agents/opencode/mod.rs` from "Files". The second
   pass's item 6 (owner decision 6 then) is superseded by this cut.
2. `Placed::Refused` as the path of a rejected `CLAUDE_CONFIG_DIR`
   (second pass, item 4): replaced by the build-error path of owner
   decision C.

### Fourth pass (design review, 2026-11-20)

A third outside design review found 2 blockers, 4 major and 2 minor
issues and noted 4 facts pre-existing on main. The sandbox group had
drawn findings in all three reviews, so the owner narrowed the change
instead of repairing it (decisions A and B). Each finding was checked
against main efae5822.

1. Blocker 1, a run step can approve a connector through the dashboard.
   Confirmed: `StepCaller::from_env` reads only the server's own
   environment (`crates/apb-core/src/step_env.rs` line 17), a keyless
   dashboard lets a local script's write through without an `Origin`
   header (`crates/apb-server/src/auth.rs` line 909,
   `keyless_write_allowed`; `RequestChannel::Keyless` at line 993), and
   the handler writes the approval (`routes/connectors/mod.rs` line 520).
   Fixed in D4.13 item 1 (owner decision C): over HTTP the handler decides
   from the request's `RequestChannel`, mapped as `post_review_handler`
   maps a person-only gate (`crates/apb-server/src/routes/runs.rs` lines
   736 and 766, `DecisionChannel::person_only_refusal` at
   `crates/apb-engine/src/decision_channel.rs` line 99): only a signed-in
   session approves, keyless and bearer requests are 403. The CLI keeps
   the `StepCaller` refusal. Tests: a keyless request from a local
   process against a server whose own environment has no run id, and a
   bearer request; the file's existing keyless approvals move to a
   signed-in session. The boundary is stated in one sentence in
   `docs/CONNECTORS.md`.
2. Blocker 2, a writable symlink ancestor bypasses the moved claude
   directory check. Confirmed: `Beneath::open` applies `O_NOFOLLOW` to
   the anchor's last component only (`crates/apb-core/src/fsutil/beneath.rs`
   line 359). Disposed of by owner decision A: the moved
   `CLAUDE_CONFIG_DIR` handling is cut.
3. Major 1, zcode's custom provider config paths. Confirmed:
   `ZCODE_BUILTIN_PROVIDER_CONFIG_FILE` and
   `ZCODE_PERSONAL_PROVIDER_CONFIG_FILE` name arbitrary paths
   (`crates/apb-core/src/agents/zcode/provider.rs` lines 21, 25 and 88).
   Disposed of by decision A: zcode's sandbox declaration is cut.
4. Major 2, the JUnit scans could spend the command's deadline. Confirmed:
   `run` computes the child's allowance at
   `crates/apb-engine/src/baseline/project.rs` line 241, before the place
   the before scan was to run. Fixed in D4.2 (owner decision D): both
   scans are charged to the command's deadline through `ScanLimits`, the
   deadline and the cancel flag are checked before every listing and
   every read, and the allowance is computed right before
   `run_capture`; tests
   `a_scan_that_uses_up_the_allowance_does_not_start_the_command` and
   `a_cancelled_scan_stops`.
5. Major 3, an older binary resuming a sandbox run under other credential
   rules. Confirmed in principle (the execution-mode suffix of
   `crates/apb-engine/src/manifest.rs` line 511 is the existing way to
   keep an older apb out). Disposed of by decision A: with the sandbox
   group cut, this change writes nothing an older apb reads differently.
6. Major 4, a fixed helper search path does not bind the approval to the
   executable. Accepted: a step that can write to `/usr/local/bin` or
   `/opt/homebrew/bin` could place a `gh` there after the approval, and
   an absolute path has the same replacement problem. Disposed of by
   owner decision B: the helper fix is cut and the lookup stays as on
   main (`crates/apb-core/src/connector/secrets.rs` line 250). The right
   fix pins the helper's path and content at approval, the mechanism
   mcp-tool-connectors introduces for its server program; it is future
   work beside that mechanism (proposal "Out of scope" item 10).
7. Minor 1, the moved credentials bind did not enforce its hard-link and
   size refusals. Confirmed: `Beneath::kind` reports the type only
   (`beneath.rs` line 484). Disposed of by decision A.
8. Minor 2, the before scan's error had no defined path. Fixed in D4.2
   (owner decision E): a failed before scan does not stop the command, it
   runs for its status, and the failing-tests observation is unknown with
   the before scan's reason; tests
   `an_unreadable_before_scan_still_runs_the_command` and
   `an_exhausted_before_scan_still_runs_the_command`.
9. Pre-existing (an environment-only `StepCaller` guard, default-home
   binds reopened at spawn, the tree fingerprint leaving ignored files
   out, a `DirLock` older than 60 s stolen): noted, not changed here; the
   first no longer guards the HTTP route (item 1).
10. Found while recounting: D4.6's `draft_leftover` lives in
    `crates/apb-core/src/versioning/mod.rs`, which "Files" did not list.
    Added.

Files: 64 to 54 (eleven out: the nine sandbox files, the engine test
constructor and `connector/secrets.rs`; one in: `versioning/mod.rs`),
under a cap of 60.

### Cut in the fourth pass

1. D4.7, zcode's sandbox declaration (decision A): its four binds, the
   `agents_test.rs` rename and arm, the engine test
   `a_zcode_node_runs_under_full_with_its_settings_read_only`, the live
   test `live_zcode_answers_one_prompt_under_full`, `fixture_for`, the
   ISOLATION bullets, the implementation note on the live test. Reason:
   zcode honours provider config files at arbitrary paths (major 1), which
   home binds do not follow.
2. D4.8, the sandbox following `CLAUDE_CONFIG_DIR` (decision A):
   `ConfigDirNeed`, `SandboxNeeds.config_dir`, `ConfigDir`,
   `config_dir_of`, `Request.config_value`, `SandboxPlan.env`, the
   moved reads of `home::prepare`, the outcome table for a directory the
   sandbox cannot follow, the nine `home.rs` tests, the five claude
   engine tests, the ISOLATION caveat, and the engine test constructor
   change (`crates/apb-engine/tests/main.rs`), which only D4.8 needed.
   Reasons: a symlink ancestor in a writable place defeats the check
   (blocker 2), the bind check did not enforce its own refusals (minor
   1), and an older apb could resume such a run under the default-home
   rule (major 3).
3. D4.13 item 2, the `{{cmd:...}}` helper search path (decision B):
   `HELPER_SEARCH_PATH`, `locate_helper`, its two tests, the
   `docs/CONNECTORS.md` sentence. Reason: major 4.
4. Superseded in the record: the third pass's decision C table and the
   opencode cut of the third pass stand as history; nothing of them is
   built. Agents under isolation stay exactly as on main: claude and codex
   declared, every other agent refused under `full`, `CLAUDE_CONFIG_DIR`
   not followed.

### Fifth pass (confirmation review, 2026-11-20)

A confirmation review found no blocker, 3 major and 3 minor issues and
noted 3 facts pre-existing on main. Each was checked against main
efae5822 and applied as the owner decided.

1. Major 1, doctor could recommend deleting real data. Confirmed:
   `interrupted_draft` accepts every dot name
   (`crates/apb-core/src/versioning/mod.rs` line 1377), and a draft
   creation may resume after the doctor looked (lines 1282 to 1290).
   Fixed in D4.6: draft debris is exactly the four names the draft code
   creates (`temp_dir_name` of `DRAFT_VERSION`, lines 1665 to 1668 and
   1303; `.candidate.lock`, `candidate_pointer.rs` line 16;
   `current.intent.json`, `current/intent.rs` line 13;
   `current-history.jsonl`, `current/history.rs` line 12), in one
   `is_draft_debris` beside `temp_dir_name`; only an empty directory gets a
   command (`rmdir`); every other directory without `current` gets its
   path and "inspect it". `draft_leftover` of the third pass is replaced.
2. Major 2, another process's report taken for the command's. Confirmed:
   a shared-workdir start takes no tree lock
   (`crates/apb-engine/src/scheduler/entry.rs` lines 788 to 792), and the
   tree guard leaves ignored reports out (`baseline/ctx.rs` line 58).
   Fixed in D4.2 ("Who may read a report"): a report counts only when it
   changed during the command and the run holds its tree for it
   (`ReadCtx.holds_tree`, the predicate of `entry.rs` line 790 at its
   three builders); otherwise the failing tests are unknown with a named
   reason. Docs say so, and name the case still open (a person's
   `--allow-shared-workdir` run beside a holding one). Tests
   `a_shared_tree_reads_junit_unknown` (unit) and
   `a_shared_workdir_run_reads_junit_unknown` (engine).
3. Major 3, a cancel during the last scan operation could still start the
   command. Confirmed: `run_capture_in` spawns at
   `crates/apb-engine/src/proc.rs` line 297 and reads the flag first at
   line 322. Fixed in D4.2 item 2: one more cancel check after the before
   scan and immediately before `run_capture`; test
   `a_cancel_during_the_last_scan_operation_starts_no_command` (the flag
   flipped by a second thread while the scan's last read runs; the test
   names that no seam pins the instant and the assertion holds for every
   instant before the spawn).
4. Minor 1, a report stamp through a joined path. Confirmed: the draft
   said `symlink_metadata(tree.join(rel))`. `Beneath` has `file_size`
   (`crates/apb-core/src/fsutil/beneath.rs` line 502, and line 912 in its
   portable variant) but no time. Fixed: a new descriptor-relative
   `Beneath::file_stamp` beside it, with a unit test.
5. Minor 2, the converted approval tests had no keyed fixture. Confirmed:
   `login_handler` answers `auth_disabled` on a keyless state
   (`crates/apb-server/src/routes/auth.rs` lines 73 to 79). Fixed in D4.13
   item 1 "Tests" item 3: `keyed` builds a keyed `AppState` as
   `auth_endpoints_test.rs` `authed_with` does (line 21), `sign_in`
   returns the session cookie pair, and every request of a converted test
   goes through a router of the same state with the cookie and the CSRF
   marker.
6. Minor 3, doctor commands not shell-safe. Confirmed:
   `sanitize_for_terminal` collapses whitespace and quotes nothing
   (`crates/apb-cli/src/util.rs` line 85). Fixed in D4.6 item 2: the path
   in the one command is quoted with the existing
   `next_action::command::shell_quote` (`crates/apb-engine/src/next_action/command.rs`
   line 150) and kept byte for byte, the command is printed only for a
   UTF-8 path without control or hidden characters, and otherwise the
   path is printed with no command; three unit tests in `doctor.rs`.
7. Pre-existing (`StepCaller` is environment based, the tree fingerprint
   leaves ignored reports out, a `DirLock` older than 60 s may be taken):
   noted, not changed; the second now bounds D4.2 through `holds_tree`.

Files: 54 to 59 (`fsutil/beneath.rs`, `baseline/ctx.rs`,
`scheduler/baseline.rs`, `scheduler/list/check.rs`,
`follow_up/read.rs`), under a cap of 65. Nothing was cut in this pass.

### Wrong implementations

One list of what a reviewer rejects, from every section:

1. GitLab: a text mode on release text; `identity.path: id`; a reconcile
   without `order_by: created_at`; `list_releases` without
   `response_pick`.
2. JUnit: reading reports the command did not write; comparing report
   times with the wall clock; counting `<skipped>`; matching `<failure`
   inside CDATA or a comment; a new digest for specs without `junit`;
   reading outside `fsutil::beneath`; adding an XML dependency; walking
   from a literal file path; trusting `(modified, len)` or an equal time
   as "unchanged"; giving the scans time of their own or computing the
   child's allowance before them; skipping the command when the before
   scan fails; reading reports in a run that does not hold its tree;
   leaving the last cancel check to `run_capture`; stamping a report
   through a joined path.
3. Profile editor: sending `process_env` on every save; adding it to
   `profile_get` or `ProfileDetail`; a comparison of stored and edited
   state outside `processEnvChange`; utility class strings.
4. Doctor: removing a directory; naming a claim younger than ten minutes
   or one whose id lock is held; taking or breaking the lock to test it; a
   new CLI test file with its own `tests/main.rs` entry; any dot name as
   draft debris; a removal command for a directory that is not empty; an
   unquoted or collapsed path in a command.
5. hermes: a release note that says hermes resumes sessions.
6. Rename: keeping an alias; renaming `docs/adr/` or `docs/superpowers/`.
7. Connectors: a refusal for one kind or one form only; guarding the
   HTTP route by the server's own environment; letting a keyless or
   bearer request approve; a second untrusted paragraph.
8. Every item: a new event, event field, manifest mark, run guard, signed
   store or own recovery path; a version bump.
