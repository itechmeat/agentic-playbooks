# Proposal: Google connectors, OAuth refresh and bounded paging

## Why

People run routines over their calendar, their mail and their documents,
and apb cannot reach the services most of those live in. Reading apb
against that found these concrete gaps (line numbers from origin/main
c5bba724):

1. No OAuth. `AuthSpec` (`crates/apb-core/src/connector/def.rs` lines 24
   to 42) has `header`, `query`, `basic` and `path` only. Google APIs take a
   short-lived access token minted from a long-lived refresh token. Today
   the only way is an external helper behind `{{cmd:...}}` that prints a
   fresh access token (`docs/CONNECTORS.md` "imap": "`apb` does not
   implement an OAuth consent flow itself"), and a token that expires in
   the middle of a run fails every later call as `auth`.
2. No way to know a token lacks a permission before the service says 403.
   Google grants scopes per consent; a function that needs
   `gmail.compose` called with a read-only token fails with a bare
   `auth` error (`call/response.rs` lines 106 to 109) that names no remedy.
3. No paging. A list call returns one page: the agent must read the
   `link` field (`call/http.rs` lines 277 to 283, `result.rs` line 145) or
   a body cursor and call again by hand (`docs/CONNECTORS.md` "sentry",
   "asana"). `github.list_issues` and `list_pulls`
   (`connectors/github/connector.yaml` lines 44 and 279) return 30 items and
   agents routinely treat that as the whole list.
4. A request body cannot carry a composed e-mail. The body kinds are a
   JSON template and a form (`def.rs` lines 313 to 322); Gmail drafts take
   `message.raw`, the base64url of an RFC 5322 message, which a template
   cannot build. The composer exists only inside the SMTP path
   (`crates/apb-engine/src/connector/smtp.rs` `render_message`, line 348).
5. A response body cannot be read when the service encodes text: Gmail
   returns message parts as base64url (`payload.parts.body.data`), and a
   file download of a binary file comes back as lossy text
   (`call/response.rs` `read_body`, lines 34 to 70).
6. Calendar event ids are client-chosen but must use the base32hex
   alphabet (`a` to `v`, `0` to `9`); apb's send key nonce uses `a` to `z`
   and `2` to `7` (`crates/apb-core/src/connector/marker.rs` `new_nonce`,
   line 135), and a 409 to a keyed repeat always reads as unknown
   (`call/send_once.rs` lines 415 to 421), although for a client-chosen id
   it proves the first request went through.
7. IMAP cannot prepare a mail for a person. `ImapOp`
   (`def.rs` lines 151 to 157) has verify, list, search, fetch and set
   flags; a playbook that drafts replies must send them through SMTP,
   with no person in between. Attachments are listed (`imap.rs`
   `fetch_body_json`, line 996) but never saved.
8. Text read from mail, events, files and the web is outside text, but
   nothing tells the author to mark such a binding `untrusted: true`
   (`schema.rs` line 1301), so the V179 to V182 checks never see it.

## What changes

1. A connector auth kind `oauth2_refresh` (capability
   `oauth-refresh-auth`): the access token is minted from a refresh token,
   kept in process memory only, refreshed before expiry and once after a
   401, after which the request is retried once. Functions declare the
   `scopes` they need (any one of them); a missing scope fails before the
   request with a message naming the remedy. A person-only
   `apb connector oauth <connector>` runs a loopback consent with PKCE and
   prints the refresh token once. IMAP gets `auth_method: oauth2` over the
   same token source.
2. Declared, bounded paging for read-only HTTP functions (capability
   `bounded-pagination`): a `Link` header or a body token, a hard page
   limit (default 5, at most 20), items concatenated. Contract tests gain
   scripted responses. `github.list_issues`, `github.list_pulls` and
   `asana.list_tasks` use it.
3. Official connector `google-calendar` (capability
   `google-calendar-connector`), with two additions to the `key` send-once
   mode: `encoding: base32hex` and `conflict: confirms`.
4. Official connector `gmail` (capability `gmail-connector`), with two
   format helpers: a `mime` request body built by the composer SMTP
   already uses, and `decode_base64url` response paths.
5. Official connector `google-drive` (capability
   `google-drive-connector`), with the `content_types` response guard.
6. Official connector `web-search` over the Brave Search API (capability
   `web-search-connector`).
7. IMAP `append_draft` and `save_attachments` ops (capability
   `mail-drafts-attachments`).
8. A function flag `untrusted_output` and validator warning V187 when a
   binding grants such a function without `untrusted: true` (capability
   `untrusted-connector-output`).

Two examples of use:

1. A non-code routine: a morning playbook reads today's calendar
   (`google-calendar.list_events`) and the unread inbox
   (`gmail.search_messages`, `gmail.get_message`), drafts a reply for each
   mail that needs one (`gmail.create_draft`, in its thread) and posts a
   summary; a person reads the drafts and sends them, or a `human_review`
   gate stands before `gmail.send_draft`.
2. A code routine: a release playbook lists the open pull requests of the
   milestone (`github.list_pulls`, now every page up to the limit), creates
   a calendar event for the release window (`google-calendar.create_event`,
   sent once), and drafts the announcement mail (`gmail.create_draft` or
   `imap.append_draft` for a non-Google mailbox) for the release manager.

## Out of scope

1. Microsoft 365 connectors (Graph mail, calendar, OneDrive). The
   `oauth2_refresh` kind and the IMAP `oauth2` method already serve Outlook
   IMAP; Graph connectors are a separate change with their own API
   surface.
2. Deferred or scheduled send. Neither Gmail's API nor IMAP has a
   portable scheduled send; a playbook schedule or a `human_review` gate
   decides when a draft goes.
3. Drive upload. The format has no binary request body (the same reason
   `whatsapp` and `amocrm` leave uploads out, `docs/CONNECTORS.md`).
4. Fetching a web page and converting it to Markdown. `web-search`
   returns titles, URLs and snippets only; page fetching is arbitrary
   egress with no declared host, which the connector model (one declared
   service per connector) does not cover.
5. Storing a rotated refresh token. apb stores no secrets (`docs/CONNECTORS.md`
   "Secrets"); a provider that rotates refresh tokens is served by a
   `{{cmd:...}}` helper that keeps the newest one. Google does not rotate
   refresh tokens on refresh, so the official connectors need none.
6. A hosted token broker, or a consent button in the dashboard. The
   consent needs a browser on the same machine and a person at it; the CLI
   is that person's surface. A dashboard flow can follow later over the
   same engine functions.
7. Computing free slots inside the connector. A connector is declarative;
   `google-calendar.query_free_busy` returns the busy intervals and the
   agent computes the gaps. An engine-side slot solver would be the only
   connector-specific computation in apb.
8. Paging of write functions and of POST listings. Repeating a write per
   page would multiply its effect and the send ledger records one request;
   the lists this change needs are all GET.
9. A message delete in Gmail or IMAP. Deleting mail is irreversible and
   none of the routines above need it; `gmail.archive` covers clearing the
   inbox.

## Impact

1. Crates: `apb-core` (connector schema `def.rs`, new
   `connector/oauth.rs` and `connector/paginate.rs`, `idempotency.rs`,
   `contract.rs`, `people.rs`, `resolve.rs` facts, a new exclusive create
   in `fsutil/beneath.rs`, validator
   `validate/connectors.rs`, `token_shape.rs`, the official set test),
   `apb-engine` (`connector/call/` http, mod, send_once, new `paginate.rs`
   and `shape.rs`; new `connector/oauth/`; new `connector/compose.rs` and
   `connector/mime_read.rs`; `smtp.rs`, `imap.rs`, `prompt.rs`,
   `contract_test.rs`, `intent/stamp.rs`, `event.rs`), `apb-cli` (new
   `connector_oauth.rs`, `connector show`, the official gate and live
   smoke tests).
2. Bundled content: new `connectors/google-calendar`, `connectors/gmail`,
   `connectors/google-drive`, `connectors/web-search`; updated
   `connectors/github` (0.3.0), `connectors/asana` (0.2.0),
   `connectors/imap` (0.2.0).
3. Journal: one additive field, `connector_call.pages`. One new value of
   the existing `send_intent_settled.source` field, `key_conflict`. No new
   event type. A send whose token minting or scope check fails is settled
   `not_sent` without a `connector_call` event, like any other failure
   before the request leaves.
4. Validator: V187 (warning). V188 to V199 stay free.
5. Behaviour changes for release notes:
   1. `github.list_issues` and `github.list_pulls` return up to 5 pages
      in one call, with `pages` and `more` in the result; a call counts
      once against `max_calls`.
   2. `asana.list_tasks` follows `next_page.offset` up to 5 pages.
   3. `imap.search_messages` and `imap.get_message` are marked
      `untrusted_output`: a playbook that grants them without
      `untrusted: true` now gets warning V187 (with the connector
      installed).
   4. An older apb cannot read the new manifests of `github`, `asana` and
      `imap` (unknown keys); after going back, reinstall them with that
      binary (`apb connector install <name> --force`).
   5. Token shapes `ya29.`, `1//0` and `GOCSPX-` are scrubbed wherever the
      token-shape detector runs.
