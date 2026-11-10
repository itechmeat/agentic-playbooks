---
display_name: Gmail
summary: Search and read Gmail, prepare drafts and replies, manage labels, and send drafts through a separate, irreversible function.
tags: [gmail, email, google]
publisher: apb
---

The Gmail connector covers searching and reading mail, preparing drafts
(including threaded replies), moving labels and archiving. Sending is its
own function: `create_draft` only writes into the mailbox, and `send_draft`
is the one irreversible step that lets a message leave.

Message bodies arrive decoded: the connector names the base64url paths of
the Gmail payload shape, so `get_message` returns plain text (parts that
are not UTF-8 arrive as null with a `data_binary` flag instead of mojibake).

## Account setup

An OAuth2 refresh-token account: `api_base` (normally
`https://gmail.googleapis.com/gmail/v1`), `token_url` (normally
`https://oauth2.googleapis.com/token`), `client_id`, `client_secret`
(secret), `refresh_token` (secret), `granted_scopes` (optional), and
`email` (the mailbox address, used as the From address of drafts).

```yaml
accounts:
  - name: default
    api_base: https://gmail.googleapis.com/gmail/v1
    token_url: https://oauth2.googleapis.com/token
    client_id: "...apps.googleusercontent.com"
    client_secret: "{{env.GMAIL_CLIENT_SECRET}}"
    refresh_token: "{{env.GMAIL_REFRESH_TOKEN}}"
    granted_scopes: "https://www.googleapis.com/auth/gmail.modify https://mail.google.com/"
    email: agent@example.com
```

The access token is minted at call time from the refresh token and lives in
the calling process only; it is never stored and is redacted from every
result, journal entry and log line. Set up the Google client and run the
consent flow with `apb connector oauth` (see `INSTALL.md`).

## Grant presets

Grant functions, not the whole connector: `read` for answering questions
about the mailbox, `draft` when a node prepares replies, `triage` when it
moves labels, and `send` only where a `human_review` gate sits before the
node. See `README.md` for the exact function lists and their measured
prompt sizes.
