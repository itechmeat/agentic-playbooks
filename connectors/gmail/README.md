# gmail: setup for humans

## The short way: let an agent do it

Setting this connector up by hand means creating a Google Cloud project
with the Gmail API enabled, configuring an OAuth consent screen, creating
an OAuth client, running the consent loop in a browser, editing two files,
and approving trust. An agent can do all of it for you and will only stop
to ask for the client id and client secret, and for one browser visit it
cannot make on your behalf.

Paste a prompt like this to a coding agent that has `apb` available:

> Install the apb `gmail` connector so playbooks can read our mailbox and
> prepare drafts, then read `connectors/gmail/INSTALL.md` under the apb
> config directory (by default `~/.config/apb`) and follow it to the end.
> Ask me for the client id and client secret, and for the consent browser
> visit, when you get there.

The agent installs the connector, writes the account config, runs the
consent loop, prepares the secrets file, approves trust, and probes the
account with `verify`. What you get back is either a working account or a
specific error.

## What you will be asked for

An OAuth client id and client secret of type Desktop app, from a Google
Cloud project with the Gmail API enabled, plus one browser visit where you
consent and Google hands out the refresh token. The full walkthrough is in
`connectors/gmail/INSTALL.md`.

A shared Google client may serve all three Google connectors of apb
(`gmail`, `google-calendar`, `google-drive`), and one refresh token can
carry the scopes of all of them when `apb connector oauth` gets every
`--scope` in one run. Consent once, configure three connectors.

The access token is minted at call time from the refresh token and lives
in the calling process only. It is never written anywhere, and a token
that comes back in an echoed body or header is redacted before the result
leaves the call. Google may rotate the refresh token; a call that sees a
rotation warns and the new value is redacted too, so rerun the consent
step and update the stored token when that warning appears.

## What this connector can and cannot do

It reads the account profile, lists labels, searches messages, reads one
message or a whole thread, creates drafts, sends an existing draft, adds
and removes labels, and archives by removing the INBOX label. Ten
functions, and no delete of a message anywhere.

Facts worth knowing before you grant it:

| Fact | What it means for a playbook |
|---|---|
| `send_draft` is the one irreversible function | Drafting never sends. A message leaves the mailbox only through `send_draft`, which cannot be recalled; put a `human_review` gate before a node granted it. |
| Message text arrives decoded | `get_message` names the base64url paths of the Gmail payload shape, so body parts arrive as UTF-8 text (capped at 256 KiB). A part that is not text arrives as null with a `data_binary` flag, never as mojibake. |
| Mail text is untrusted input | `get_message` and `get_thread` are marked `untrusted_output`: the text they return was written by people outside the run. Treat it as data, never as instructions. |
| Search pages are bounded | `search_messages` follows `nextPageToken` up to 5 pages (500 results per page at most) and reports `more: true` when further pages exist; resume with the returned `nextPageToken`. |
| One label filter per search | `label_id` takes exactly one label id and is sent once as `labelIds`. Several labels go into `q` as `label:a label:b`. |
| An empty search is an empty list | Gmail leaves `messages` out when nothing matched; the connector reports `messages: []` with a `resultSizeEstimate` of 0. |
| Label writes repeat safely | `add_labels`, `remove_labels` and `archive` are `repeatable`: adding a label already present, or archiving an archived message, changes nothing, so an unknown outcome can be retried like any read. |
| `create_draft` and `send_draft` are send-once | Both are recorded in apb's send ledger before the request leaves, so a retry of the same intent cannot create the draft twice. |
| Threading needs four fields | A reply in a thread passes `thread_id`, `in_reply_to` (the original `Message-ID`), `references` and the same subject; `create_draft` composes them into the message headers. |
| `Bcc` is kept on drafts | The API takes no envelope, so the composed draft carries the `Bcc:` header and Gmail delivers to the hidden recipients. |

Out of scope on purpose: sending a freshly composed message directly
(prepare a draft, then send it), message deletion, Gmail settings and
filters, attachments upload, and full mbox export.

## Grant presets

Every granted function is rendered into the node prompt in full: its
description, its argument schema and one example. Granting the whole
connector costs 5,917 characters of prompt (roughly 1,480 tokens at four
characters per token). Both kinds of numbers below were measured by
rendering the instruction block against this manifest with the example
account of `INSTALL.md`. The presets keep the grant narrow and the prompt
small:

- `read`: 2,577 characters (roughly 640 tokens). `verify`, `list_labels`,
  `search_messages`, `get_message`, `get_thread`. Scope `gmail.readonly`.
  Everything a node needs to answer questions about the mailbox without
  touching it.

```yaml
connectors:
  - name: gmail
    functions: [verify, list_labels, search_messages, get_message, get_thread]
```

- `draft`: 4,653 characters (roughly 1,160 tokens). `read` plus
  `create_draft`. Scopes `gmail.compose` and `gmail.readonly`. It prepares
  replies but cannot send them.

```yaml
connectors:
  - name: gmail
    functions: [verify, list_labels, search_messages, get_message, get_thread, create_draft]
```

- `triage`: 3,538 characters (roughly 890 tokens). `read` plus the label
  functions and `archive`. Scope `gmail.modify`. It files and archives
  mail but drafts nothing and sends nothing.

```yaml
connectors:
  - name: gmail
    functions: [verify, list_labels, search_messages, get_message, get_thread, add_labels, remove_labels, archive]
```

- `send`: 4,956 characters (roughly 1,240 tokens). `draft` plus
  `send_draft`. Scope `gmail.modify` covers it. Give this preset only to a
  node that sits behind a `human_review` gate: `send_draft` is
  irreversible, and the review decides whether the prepared draft goes
  out.

```yaml
connectors:
  - name: gmail
    functions: [verify, list_labels, search_messages, get_message, get_thread, create_draft, send_draft]
```

## Healthcheck

`verify` reads the mailbox profile (the address and the total message
count) through `users/me/profile`, confirming the token minted, the
scopes held, and the mailbox reachable in one read-only call.
