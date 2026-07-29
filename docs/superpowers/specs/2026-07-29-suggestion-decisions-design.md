# Suggestion decisions: flexible dismiss and re-offer mechanics

Status: approved design, 2026-07-29. Supersedes the dismiss portion of spec 8.2 in `2026-07-11-agent-transparent-workflows-design.md`. Implementation plan: `docs/superpowers/plans/2026-07-29-suggestion-decisions.md`.

## Problem

The offer-to-save flow (spec 8.1-8.2) ships with a dismiss store that is too rigid in four ways, confirmed by live testing of the proactive offer in July 2026:

1. Matching is byte-exact on a kebab-slug the model invents. The same action slugged `daily-note-creation` today and `create-daily-notes` next month defeats the dismissal.
2. A soft decline ("not now") is not recorded anywhere. The next session offers again immediately; only the in-session "at most one offer" prose rule limits it.
3. The user has no visibility or control: `dismissed.json` is not exposed by any CLI command or dashboard view.
4. One global hardcoded TTL (90 days) covers every case, and the store itself is global only, so a decline in one project silences the suggestion in every project.

## Decisions (from the brainstorm)

- Semantic matching is done by the model, not the server. The server's job is to store enough context (a synopsis) for the model to judge "is this the same action". No server-side language processing, no embeddings, no hardcoded word lists.
- Soft declines escalate: each soft decline on the same suggestion pushes the next offer further out on a backoff schedule. A soft decline never becomes permanent by itself; only an explicit "do not suggest this again" is a hard dismissal.
- Timing knobs have named-constant defaults in code and an optional config override, global and per-project.
- Records are scoped: project scope by default, global when the user's own wording says everywhere.
- Management surfaces: CLI commands and a dashboard section, both over the same core functions.

## Store (apb-core)

`dismiss.rs` evolves into a suggestion-decision store, schema version 2. One record per suggestion:

```json
{
  "schema": 2,
  "records": [
    {
      "pattern": "code-review-run",
      "synopsis": "Review a source file for bugs and write findings to a markdown report",
      "kind": "soft",
      "declines": 2,
      "snoozed_until_ms": 1785924000000,
      "updated_at_ms": 1785319200000
    }
  ]
}
```

On disk these two timestamps are epoch milliseconds, the apb convention for every persisted timestamp (`clock.rs`), not ISO strings; every wire surface (the `suggestion_dismiss` and `playbook_catalog` MCP responses, `apb suggestions list`, the dashboard) renders them through `iso_utc` at the point of output, never on disk.

- `pattern`: English kebab-slug chosen by the agent, as today. It is now a stable identifier for the record, not the matching key.
- `synopsis`: one English sentence describing the action that was offered. Required on new writes. This is what the model compares future candidate actions against, by meaning.
- `kind`: `soft` or `hard`.
- `declines`: soft-decline counter, drives the backoff position. Absent (0) for hard records.
- `snoozed_until_ms`: computed by the server, never by the agent, epoch milliseconds. For hard records this is the hard-TTL expiry.
- `updated_at_ms`: last write, from `clock.rs`, epoch milliseconds.

Two locations, merged like connector config: project `.apb/suggestions.json` and global `<config-dir>/suggestions.json`. A global record suppresses the suggestion in every project; a project record only in its project. When the same `pattern` exists in both scopes, the stricter record wins for that project (hard beats soft; later `snoozed_until` beats earlier).

Records are pruned on read, under a soft-retention rule rather than a plain expiry. A record stops suppressing the suggestion the moment its snooze ends, but a soft record is still KEPT on disk for `SOFT_RETAIN_DAYS` (365) counted from the later of its snooze end and its last write, so its `declines` counter and its synopsis survive the gap and the next soft decline escalates from where the last one left off instead of restarting at one day. A hard record has nothing to escalate and is pruned at its expiry. Reads never fail (a corrupt or unreadable file yields an empty list plus a diagnostic); writes go through `apb_core::fsutil` (atomic temp+rename, 0600, dir lock), and a store that is present but unreadable is moved to `suggestions.json.corrupt` before a write replaces it.

Migration: on first access, if `<config-dir>/suggestions.json` is absent and the v1 `<config-dir>/dismissed.json` exists, its records are converted (kind hard, existing expiry preserved as `snoozed_until`, synopsis empty) and written to the new file; the old file is removed only after the new one is atomically in place. An empty synopsis on a migrated record means the model falls back to slug comparison for it, which is exactly the v1 behavior.

## Backoff and configuration

Defaults as named constants in the store module: `SOFT_BACKOFF_DAYS: [u64; 4] = [1, 7, 30, 90]` and `HARD_TTL_DAYS: u64 = 90`.

On a soft dismiss the server increments `declines` and sets `snoozed_until = now + SOFT_BACKOFF_DAYS[min(declines - 1, len - 1)]`. On a hard dismiss it sets `snoozed_until = now + HARD_TTL_DAYS` and `kind = hard`.

A hard dismissal is therefore a LONG SILENCE WITH AN EXPIRY, not a permanent ban: the record is pruned once `snoozed_until` passes, and the suggestion may be offered again after that window (90 days by default, or whatever `hard_ttl_days` says). Permanence is achieved by re-declining, which renews the window from the new "now", and the window itself is configurable per project. Every user-facing surface must word it that way: the agent may treat the user's "never again" as the trigger for `kind: hard`, but no text may claim the resulting record lasts forever. The dashboard therefore labels a hard record "long snooze" next to its expiry date rather than "never again".

Optional config override, per key, project over global:

```yaml
suggestions:
  soft_backoff_days: [1, 7, 30, 90]
  hard_ttl_days: 90
```

The section lives in the existing global apb config and in the project `.apb/config.yaml`. Empty arrays are a validation error; values are days, positive integers. Users who never touch the section see the defaults.

## MCP surface (apb-mcp)

`suggestion_dismiss` args grow three fields, all backward-compatible via `#[serde(default)]`:

- `kind`: `"soft"` or `"hard"`, default `hard` (an old-style call keeps its old meaning).
- `synopsis`: string, default empty, strongly recommended in the tool description; secret-value hygiene applies as in capture.
- `scope`: `"project"` or `"global"`, default `project`.

The response reports the stored record including the computed `snoozed_until`, so the agent can tell the user how long the silence lasts.

`playbook_catalog` keeps returning `dismissed_patterns` (slug list, backward compatibility) and adds `suppressed_suggestions`: the full active records (pattern, synopsis, kind, snoozed_until) from both scopes merged for the current project. The catalog revision digest folds in the new field the same way it folds dismissed patterns today, so `unchanged` responses stay correct after any dismiss write.

## Instruction text (tier 0 and the standing block)

Two wording changes, one sentence each, applied to both `crates/apb-mcp/src/instructions.rs` (TIER0) and `crates/apb-cli/assets/playbook-instructions.md`:

- Matching: compare the candidate action against `suppressed_suggestions` by meaning of the synopsis, not by slug equality; skip the offer when a record covers it.
- Soft declines: when the user declines an offer without saying never, record it with `suggestion_dismiss` kind `soft` (project scope); reserve kind `hard` for an explicit never-again, and use global scope only when the user's wording says everywhere.

TIER0 must stay at or under 1950 bytes after the edit; the byte count is re-verified. The agent asks no extra question about scope on a decline; the model infers it from the user's wording.

## CLI (apb-cli)

New `apb suggestions` command group, thin dispatch over core:

- `apb suggestions list`: both scopes with scope label, kind, declines, snoozed-until, synopsis. Human-readable table; `--json` for scripts.
- `apb suggestions allow <pattern>`: remove the project record; `--global` removes the global one. Removing a record re-enables offers immediately.
- `apb suggestions reset <pattern>`: zero the decline counter and clear the snooze on a soft record, keeping the record itself so its synopsis stays available; `--all` resets every soft record in the project scope. Hard records are only removed via `allow`, not reset.

## Dashboard (apb-server + web)

- `GET /api/suggestions?workspace=<id>`: merged active records for the workspace, with scope labels.
- `DELETE /api/suggestions/{pattern}?workspace=<id>&scope=project|global`: same effect as `apb suggestions allow`.

Routes follow the existing one-module-per-resource pattern in `routes/`, workspace id validated with `is_safe_id`. The web UI gets a "Silenced suggestions" section on the playbooks page, built from the same shadcn-svelte Card idiom as the rest of that page rather than a table: one card per record, with badges for kind, scope and project, plus the synopsis, the until-date and a remove action. A global-scope record is shown once, not once per project, since one global record silences the suggestion everywhere; its card is labeled "all projects" instead of a single project name.

## Agent behavior summary

- Offer fires per the existing tier-0 and standing-block duties.
- User accepts: capture flow, unchanged.
- User declines softly: `suggestion_dismiss` with kind soft, project scope, synopsis filled. Server computes the escalating snooze.
- User declines hard: kind hard; global scope only on everywhere-wording.
- Before any offer: check `suppressed_suggestions` semantically; a covering record means no offer, regardless of slug.

## Out of scope

- Server-enforced "one offer per session": the server has no session identity; the rule stays as instruction prose. A future event ledger (approach C from the brainstorm) could add it; nothing in this design blocks that.
- Embeddings or any server-side semantic matching.
- Suggestion analytics or offer-history timelines.

## Testing

- Core unit tests: v1 to v2 migration (old file converted, removed only after successful write; corrupt v1 yields empty store plus diagnostic), scope merge with stricter-wins conflict rule, backoff arithmetic including the schedule tail and config overrides, prune-on-read.
- MCP tests: an old-style `suggestion_dismiss` call (no new fields) now records in project scope with kind hard and honors `ttl_days` as the hard-TTL override, matching v1 semantics except for scope, since v1 had no scope and stored globally; a record migrated from v1 stays global. New-field calls store and return the computed snooze; catalog returns both `dismissed_patterns` and `suppressed_suggestions`, and its revision changes after every dismiss write.
- CLI integration tests: list/allow/reset against a temp config dir and project, including the global flag.
- Server route tests: GET and DELETE with workspace validation.
- End-to-end sandbox scenario (manual, same harness as the July 2026 offer experiment): offer, soft decline, new session, verify silence; repeat after the snooze window using a shifted clock only if the harness allows it, otherwise verify via `apb suggestions list`.
