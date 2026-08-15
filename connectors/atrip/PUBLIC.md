---
display_name: Atrip
summary: Search, book, ticket, and manage flight orders through AtripTech's Atlas API.
tags: [atrip, flights, booking, travel]
publisher: apb
---

The Atrip connector covers AtripTech's Atlas flight-booking API end to end:
flight search, fare verification and offer pricing, order creation, payment,
ticketing status, seat and baggage ancillaries (pre- and post-ticketing),
void, refund, PNR claim and live PNR extraction, order regeneration,
account balance, route export, mail query, webhook registration, and
incident lookup. 28 functions in total, covering every operation the vendor
documents.

Most Atlas responses signal outcome through a business `status` field
inside a 200-OK JSON body, not through HTTP status: read each function's
description for what "success" means for that call, since several
operations (`verify`, `pay`, `void`, `refund`) use a status range or a
dedicated status field of their own rather than a plain 0/nonzero pair.

Eight functions are effectful (booking or money movement): `order`, `pay`,
`void`, `refund`, `stop_ticket`, `regenerate_order`, `pnr_claim`, and
`post_booking_ancillary_order`. `pay` in particular carries no documented
idempotency key: a duplicate call while a prior payment is still in flight
risks a duplicate charge, so it must never be retried blindly.

Start against the sandbox host before touching production. AtripTech issues
separate sandbox and production credentials, and production additionally
requires two per-tenant base URLs obtained from the ATRIP portal after UAT
approval; there is no shared production hostname to hardcode.

## Account setup

Four account fields: `client_id`, `client_secret` (secret), `base_url`, and
`search_base_url`.

```yaml
accounts:
  - name: sandbox
    client_id: "{{env.ATRIP_CLIENT_ID}}"
    client_secret: "{{env.ATRIP_CLIENT_SECRET}}"
    base_url: https://sandbox.atriptech.com
    search_base_url: https://sandbox.atriptech.com
```

Only `search` goes to `search_base_url`; every other function goes to
`base_url`. In sandbox both fields point at the same
host; in production AtripTech issues two distinct per-tenant URLs.

## Healthcheck

Declared as `query_void_orders`, the least-committal read-only operation: a
pure status poll with no required arguments and no booking or money side
effect. Against a tenant with no void records a live probe may come back as
a vendor business error in the body, which still proves the URL and the
credentials work; the fuller live verification is a real `balance` call.
