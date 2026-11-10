---
display_name: IMAP Mail
summary: Read mailboxes over IMAP with a silent-read guarantee, mark read/unread, file drafts, and save attachments into the run.
tags: [email, imap, mail, notifications]
publisher: apb
---

Silent-read guarantee: reading mail never marks it seen. `search_messages`
and `get_message` open the folder read-only with `EXAMINE`, and message
content is fetched only with `BODY.PEEK[]`, so the `\Seen` flag cannot
change as a side effect of a read, even on a server that would otherwise
mark a message seen on fetch. The only functions that change anything on
the server are `mark_read`, `mark_unread`, and `append_draft`, which adds
one composed message to the drafts folder with the `\Draft` flag and
never sends anything. Each is explicit and separately grantable.

One connector serves any IMAP provider: the protocol is identical
everywhere and only the connection settings differ, which is what an
account config is for.

## Account setup

Account fields: `host` (required), `port` (required), `use_tls` (optional,
default `true`), `auth_method` (required, `password`, `xoauth2` or
`oauth2`), `username` (required), `password` (secret, required for
`password` and `xoauth2`), `from_email` and `from_name` (optional, the
sender identity `append_draft` composes with; a draft call without
`from_email` in the account fails naming the field). An `oauth2` account
uses the `oauth_token_url`, `oauth_authorize_url`, `oauth_client_id`,
`oauth_client_secret` (secret), `oauth_refresh_token` (secret),
`oauth_scopes`, `oauth_access_type`, `oauth_prompt` and
`oauth_granted_scopes` fields instead of a password.

```yaml
accounts:
  - name: default
    host: imap.example.com
    port: "993"
    use_tls: true
    auth_method: password
    username: mailbox@example.com
    password: "{{env.IMAP_PASSWORD}}"
```

`use_tls` defaults to `true` when omitted; only set it to `false` for a
local plaintext test fixture, never for a real provider.

### Gmail

Host `imap.gmail.com`, port `993`. Gmail requires 2-Step Verification to
be enabled before you can generate an app password:

```yaml
accounts:
  - name: gmail
    host: imap.gmail.com
    port: "993"
    auth_method: password
    username: you@gmail.com
    password: "{{env.GMAIL_APP_PASSWORD}}"
```

For a Google Workspace account where app passwords are disabled by policy,
use `auth_method: oauth2`: run `apb connector oauth imap --account <name>`
once in a terminal, approve the access in the browser it opens, and store
the refresh token it prints as the account's `oauth_refresh_token` secret.
An external token helper (`auth_method: xoauth2`, sourced with
`{{cmd:...}}` the way the GitHub connector sources a CLI token) remains
the alternative.

### Yandex Mail

Host `imap.yandex.com`, port `993`. Enable IMAP access in the Yandex Mail
web settings first (Settings, then Mail clients), then generate an app
password:

```yaml
accounts:
  - name: yandex
    host: imap.yandex.com
    port: "993"
    auth_method: password
    username: you@yandex.com
    password: "{{env.YANDEX_APP_PASSWORD}}"
```

### Outlook and Microsoft 365

Host `outlook.office365.com`, port `993`. Password authentication does not
work here. The simplest path is `auth_method: oauth2` with `apb connector
oauth imap` (an Azure app registration with a loopback redirect owns the
client id and secret). The external token helper way (for example `oama`
or `mutt_oauth2`, with `auth_method: xoauth2` and the `{{cmd:...}}`
mechanism) still works:

```yaml
accounts:
  - name: outlook
    host: outlook.office365.com
    port: "993"
    auth_method: xoauth2
    username: you@outlook.com
    password: "{{cmd:oama access outlook you@outlook.com}}"
```

### iCloud

Host `imap.mail.me.com`, port `993`. Generate an app-specific password
from your Apple ID account page (App-Specific Passwords):

```yaml
accounts:
  - name: icloud
    host: imap.mail.me.com
    port: "993"
    auth_method: password
    username: you@icloud.com
    password: "{{env.ICLOUD_APP_PASSWORD}}"
```

## Healthcheck

`verify` connects, negotiates TLS when `use_tls` is set, and authenticates,
without opening or reading any mailbox.

## Excluded on purpose

No message deletion and no move between folders. No sending either:
`append_draft` only files a message in the drafts folder, so actual
delivery stays the `smtp` connector's job; the two are meant to be
installed together for a read-and-reply workflow. `save_attachments`
writes only inside the run's own directory (at most 20 files, 10 MiB per
file, 25 MiB per call, names sanitized, nothing overwritten) and reads
the mailbox read-only.
