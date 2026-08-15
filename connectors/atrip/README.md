# atrip: setup for humans

## The short way: let an agent do it

Setting this connector up by hand means editing two files, generating a
credential pair in the ATRIP portal, and approving trust. An agent can do
all of it for you and will only stop to ask for the credentials, which are
the one thing it cannot obtain on your behalf.

Paste a prompt like this to a coding agent that has `apb` available:

> Install the apb `atrip` connector for my account, then read
> `connectors/atrip/INSTALL.md` under the apb config directory (by default
> `~/.config/apb`) and follow it to the end. Ask me for the client id and
> client secret when you get there.

The agent installs the connector, writes the account config, prepares the
secrets file, approves trust, and reports back with a working account or a
specific error. The declared healthcheck (`query_void_orders`) is a
reachability and credentials probe that may itself return a vendor
business error in the body, so fully verifying the credentials means
making a real call, such as `search` or `balance`, against the sandbox.

## What you will be asked for

A client id and client secret pair, plus two base URLs.

In sandbox, generate the pair yourself in the ATRIP portal (self-service,
no approval needed) and use `https://sandbox.atriptech.com` for both
`base_url` and `search_base_url`.

In production, the credential pair and both base URLs are issued together
inside the ATRIP portal, under My Profile then Company Information, once
your account has passed UAT and a customer manager has switched it to LIVE
status. Production uses two separate per-tenant base URLs, one for search
and a different one for every other call; there is no shared production
hostname, and the vendor's own docs warn against reusing the sandbox base
URL after go-live.

Keep both credential values server-side. AtripTech's own documentation
says this plainly: do not expose the client id or client secret in a
client application.

## What this connector can and cannot do

It covers the full documented Atlas API surface: flight search, fare
verification and offer-based pricing, order creation, payment, order
status queries, seat and baggage ancillaries before and after ticketing,
void, refund, PNR claim and live PNR extraction from the airline, order
regeneration, account balance, a bulk route export, mail (notification)
queries, webhook URL registration, and incident lookup for
webhook-miss reconciliation. 28 functions in total.

Most calls report success through a `status` field inside the JSON body
rather than through HTTP status, and the exact meaning of that field
differs by call: `search`, `get_offers`, `get_offer_price`, and
`query_order_details` use a plain 0-means-success convention, while
`verify`, `pay`, `void`, and `refund` use their own status ranges or
fields where several non-zero values are still non-fatal. Read the
function description before treating any non-zero value as a failure.

Eight functions move money or change a real booking and cannot be undone
by this connector: `order` (creates a booking), `pay` (charges money),
`void` (cancels a booking and starts a refund), `refund` (moves money back
to the customer), `stop_ticket` (irreversibly halts ticket issuance),
`regenerate_order` and `pnr_claim` (each creates a new order), and
`post_booking_ancillary_order` (charges money for a baggage add-on). Grant
those to the nodes that genuinely need them, and consider a `max_calls`
cap where a loop could reach them.

`pay` deserves particular caution: AtripTech documents no idempotency key
for it. A duplicate call while a prior payment may still be in flight
(status 406, "payment in progress") risks a duplicate charge, so it must
never be retried blindly; back off and check `query_order_details` before
trying again. Payment success also does not always mean ticketing is
complete, so poll `query_order_details` to confirm the final state.

A handful of endpoints (`void_quotation`, `query_void_orders`,
`refund_quotation`, `query_refund_orders`) have loosely-typed argument
schemas because the vendor's own reference does not name their exact
request body fields beyond the operation's purpose and returned
identifier; consult AtripTech's docs directly if you need to pass more
than the obvious order or offer identifier.

Rate limits: AtripTech documents only two numbers, 10 QPS on `search`
(HTTP 429 or body status 110 when exceeded) and a shared 60-calls-per-minute
pool across the seat and baggage ancillary queries; limits for the other
endpoints are unpublished, so keep `max_calls` caps conservative.

## Doing it by hand

If you would rather not involve an agent, `PUBLIC.md` in this folder has
the account fields and a sandbox config example, and `docs/CONNECTORS.md`
in the apb repository covers accounts, secrets, and trust in general.
`INSTALL.md` is written for an agent but the steps read fine as a
checklist.
