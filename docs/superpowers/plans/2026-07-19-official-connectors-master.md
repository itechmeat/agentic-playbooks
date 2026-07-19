# Official Connectors Wave 1 - Master Implementation Plan

Spec: `docs/superpowers/specs/2026-07-19-official-connectors-design.md`

This master plan coordinates six slice plans. Each slice is independently
landable and carries its own detailed, task-by-task plan document in this
directory. Execute a slice with superpowers:subagent-driven-development
(recommended) or superpowers:executing-plans; delegate the coding tasks to
opus or sonnet subagents per the project working agreement.

## Slice plans

| # | Plan document | Scope (spec sections) | Depends on |
|---|---|---|---|
| 1 | `2026-07-19-official-connectors-slice-1-format-engine.md` | path auth, `{{auth}}`, headers, User-Agent, `link`, `examples`, `response_pick`, `--full` (4.3, 4.4, 4.5) | none |
| 2 | `2026-07-19-official-connectors-slice-2-cmd-secrets.md` | `{{cmd:...}}` secret source, account digest over secret refs (4.1) | none |
| 3 | `2026-07-19-official-connectors-slice-3-smtp.md` | `smtp` function kind on lettre (4.2) | none |
| 4 | `2026-07-19-official-connectors-slice-4-distribution.md` | `connectors/` embedding, `install`, trust seeding, tests.yaml runner, `connector test` (3, 4.6) | final smtp/headers arms need 1, 3 |
| 5 | `2026-07-19-official-connectors-slice-5-manifests.md` | the four manifests, PUBLIC.md, tests.yaml, demo playbooks, CI gate, live smokes, docs (5, 6, 8) | 1, 2, 3, 4 |
| 6 | `2026-07-19-official-connectors-slice-6-playground.md` | server call endpoint plus dashboard playground UI (7) | code: none; manual verification: 5 |

Recommended order: 1, 2, 3 in parallel or any order, then 4, then 5, then 6.

## Shared interface contract

The slice plans were authored in parallel against these names; a change in
one place must be propagated everywhere before execution.

- `AuthSpec::Path { value_template }`, serde tag `kind: path`; reserved URL
  placeholder literal `{{auth}}`, exactly once, only and always with path
  auth.
- `FunctionSpec` additions, all `#[serde(default)]`: `headers:
  BTreeMap<String, String>`, `examples: Vec<ExampleSpec>` with `ExampleSpec
  { args, note }`, `response_pick: Vec<String>`, `smtp: Option<SmtpSpec>`.
- `SmtpSpec { connection: SmtpConnection, message: Option<SmtpMessage>,
  verify: bool }`; connection fields host, port, use_tls, username,
  password; message fields from_email, from_name, to, cc, bcc, subject,
  body_text, body_html. `{{secret.*}}` allowed only in `auth` and the smtp
  connection password.
- A function is exactly one of HTTP, mock, smtp (three-way xor validation).
- Call result additions: `link` (raw Link header, optional), `picked`
  (true when response_pick applied). CLI flag `--full` bypasses the pick.
- Secret references: exactly one `{{env.VAR}}` or `{{cmd:<command>}}` per
  secret field; cmd runs shell-less via shell-words argv, 10 s timeout,
  never during the policy gate; reference strings join the account digest.
- tests.yaml: `cases: [{ function, account, args, expect }]`; expect arms
  `{method, url, headers?, body_contains?}` for HTTP, `{envelope}` for
  smtp, `{status, body}` for mock; secrets stub to `test-secret`; runner
  reuses the dry-run render path, fully offline.
- Embedded distribution API in apb-core: `official::list()`,
  `official::materialize(name, dest)`; `install <name>` seeds trust,
  `install --from-dir` does not.
- Playground endpoint: `POST /api/connectors/:name/call` with `{ function,
  account, args, dry_run, full }` (`full` optional, default false; true
  bypasses the function's `response_pick` projection), trust-gated like the
  healthcheck probe.

## Cross-slice obligations

Findings from authoring the six plans in parallel; the slice named in each
item owns the reconciliation. Executors must read this section before
starting any slice.

1. **`CallOk` shape.** Slice 1 adds `link: Option<String>` and `picked:
   bool` to `CallOk`; slice 3 refactors `CallOk` from a struct into a
   two-variant enum (`Http { status, body, truncated }` / `Smtp { body }`).
   Whichever lands second folds the other's change in: with the recommended
   order (1 then 3), slice 3's `Http` variant carries `link` and `picked`
   and `to_success_json` emits them. Slice 6's `play_call` uses
   `to_success_json` and inherits the reconciled shape.
2. **`response_pick` on smtp functions.** The validation guard belongs to
   whichever of slices 1 and 3 lands second (slice 1 left a
   `NOTE(smtp-slice)` in the mock guard; slice 3's Task 3 is written as
   guarded on `response_pick` existing).
3. **`{{auth}}` in smtp templates.** Slice 1 adds `Namespace::Auth`; slice
   3's `validate_smtp_templates` must reject it (a `reject_auth` call in
   the connection and message walkers). Owned by whichever lands second.
4. **Lenient optional smtp message fields.** Slice 5's `send_email`
   manifest templates `cc`/`bcc`/`body_html` individually; the generic
   resolver errors on a missing arg. Slice 3 must render optional
   `SmtpMessage` fields leniently: a render failure caused by a missing
   optional arg means "field absent", not an error. Without this,
   `send_email` calls omitting `cc`/`bcc`/`body_html` fail to render.
5. **smtp envelope arm of the contract-test runner.** Slice 4's Task 9
   consumes slice 3's offline envelope render; `connector_smtp::build` with
   `dry_run: true` is the entry point (the `SmtpBuild::DryRun` JSON carries
   the envelope). Slice 4's `Envelope { from, to, subject }` must also
   accept the empty `envelope: {}` form slice 5 uses for `verify` cases
   (make the fields optional or add a dedicated verify expectation).
6. **Deferred slice-4 tasks.** Slice 4 Tasks 8 (headers expectations) and 9
   (smtp envelope expectations) start only after slices 1 and 3 merge; the
   runner fails those expectation kinds loudly until then.
7. **Seed connector handoff.** Slice 4 seeds `connectors/example/`; slice 5
   removes it and re-points the slice-4 CLI install/list/test integration
   tests at `github` (or a `--dir` fixture).
8. **Account digest already covers secret refs.** Slice 2's investigation:
   `config::account_digest` hashes secret field reference strings today, so
   no serialization change is needed and existing env accounts keep trust.
   Slice 2 adds a regression test pinning this.
9. **Doctor does not execute cmd secrets.** Slice 2 confines command
   execution to calls and live healthchecks; the passive `doctor`/status
   path neither runs commands nor reports cmd-sourced fields as missing.
   Spec 4.1's "doctor" mention resolves to this narrower behavior.
10. **Execution-time verifications flagged by planners** (check against the
    shipped code before relying on them): the exact connector/account
    approve CLI form used by slice 5's tests, the stub-agent invocation
    mechanism for live smokes, shadcn-svelte component prop names in slice
    6, and lettre 0.11 blocking-client signatures in slice 3.

## Gates for every slice

From CLAUDE.md, applying to each task in each slice plan:

- `cargo fmt --all -- --check` and
  `cargo clippy --workspace --all-targets -- -D warnings` clean.
- `code-ranker check .` clean before commit (warm the cache first:
  `cargo metadata --format-version 1 >/dev/null`).
- Web changes: `bun run test` and `bun run check` clean in `web/`.
- Commits: `git commit --signoff`, Co-Authored-By trailer for the acting
  model, no visible AI-authorship markers in public prose.
- No em-dash (U+2014), no exclamation marks in docs or user-facing
  strings, no CJK. Machine-facing text English.
- Before release readiness: `cargo clippy --release` clean.

## Definition of done for the wave

- All four connectors install from the binary, pass `apb connector test`,
  `apb connector doctor`, and their healthchecks against real services
  (manual live smoke, env-gated).
- Demo playbooks validate in CI and run end to end manually.
- Dashboard shows the four connectors with storefronts; playground calls
  work dry-run and real.
- `docs/CONNECTORS.md` documents official connectors and per-service
  setup; the spec's section 15-style implementation notes are appended to
  the spec at the end of the wave.
