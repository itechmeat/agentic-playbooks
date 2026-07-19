---
display_name: IMAP Mail
summary: Read mailboxes over IMAP with a silent-read guarantee, plus explicit read/unread marking.
tags: [email, imap, mail, notifications]
publisher: apb
---

Silent-read guarantee: reading mail never marks it seen. `search_messages`
and `get_message` open the folder read-only with `EXAMINE`, and message
content is fetched only with `BODY.PEEK[]`, so the `\Seen` flag cannot
change as a side effect of a read, even on a server that would otherwise
mark a message seen on fetch. The only functions that change anything on
the server are `mark_read` and `mark_unread`, which are explicit,
separately grantable calls.

One connector serves any IMAP provider: the protocol is identical
everywhere and only the connection settings differ, which is what an
account config is for.

## Account setup

Account fields: `host` (required), `port` (required), `use_tls` (optional,
default `true`), `auth_method` (required, `password` or `xoauth2`),
`username` (required), `password` (required, secret).

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
use `auth_method: xoauth2` with an access token from a standard OAuth
token helper, sourced with `{{cmd:...}}` the same way the GitHub connector
sources a CLI token.

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
work here: Outlook and Microsoft 365 only accept `auth_method: xoauth2`.
Source the access token from an external token helper (for example `oama`
or `mutt_oauth2`) with the `{{cmd:...}}` mechanism:

```yaml
accounts:
  - name: outlook
    host: outlook.office365.com
    port: "993"
    auth_method: xoauth2
    username: you@outlook.com
    password: "{{cmd:oama access outlook you@outlook.com}}"
```

apb does not implement an OAuth consent flow itself; the token helper owns
that.

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

No message deletion, no move between folders, and no sending: this
connector only reads and marks read/unread. Sending mail is the `smtp`
connector's job; the two are meant to be installed together for a
read-and-reply workflow.
