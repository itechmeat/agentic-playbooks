---
display_name: SMTP Email
summary: Send transactional email over SMTP with STARTTLS.
tags: [email, smtp, notifications]
publisher: apb
---

A single-purpose connector: one account, two functions (`verify` and
`send_email`). Any SMTP relay works, including a transactional email
provider's SMTP endpoint or a mailbox provider's app-password relay.

## Account setup

```yaml
accounts:
  - name: default
    host: smtp.example.com
    port: "587"
    username: releases@example.com
    from_email: releases@example.com
    from_name: Release Bot
    use_tls: true
    password: "{{env.SMTP_PASSWORD}}"
```

`use_tls` and `username`/`password` are schema-optional (a local
unauthenticated relay needs neither), but set `use_tls` explicitly:
the account field carries no engine-level default, so an account that
omits it fails a call cleanly rather than assuming STARTTLS. Set it to
`true` for the common case (STARTTLS on port 587) and only to `false`
for a trusted local relay with no encryption.

For Gmail, generate an app password and use `smtp.gmail.com` port 587.

## Healthcheck

`verify` connects, negotiates STARTTLS when `use_tls` is set, and
authenticates when credentials are present, without sending a message.
