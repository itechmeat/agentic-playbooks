# Deploying the apb dashboard on a server

By default `apb dashboard` binds `127.0.0.1` with no authentication, which is
safe only because nothing off the machine can reach it. The API can create
playbooks, start runs, and make authenticated connector calls, so an
unprotected dashboard on a public interface is equivalent to handing out remote
code execution. This document describes the one supported way to run it on a
server: an authenticated dashboard behind a reverse proxy that terminates TLS.

## The supported topology

```text
browser -> https://apb.example.com -> reverse proxy (TLS) -> 127.0.0.1:7321 -> apb dashboard
```

The proxy owns TLS, HSTS, and certificate renewal. apb keeps serving plain HTTP
on the loopback interface behind it and never terminates TLS itself.

## 1. Issue an authorization key AS THE SERVICE USER

The key file lives in the config directory of whoever runs the command:
`APB_CONFIG_DIR`, else `$XDG_CONFIG_HOME/apb`, else `$HOME/.config/apb`. The
dashboard reads the config directory of whoever runs the dashboard. If you
issue a key from your own shell and the service runs as `apb`, the service
never sees that key and keeps running unauthenticated behind your proxy, and
the non-loopback interlock does not catch it because the bind is loopback.

Issue keys as the same user the service runs as:

```sh
sudo useradd --system --create-home --shell /usr/sbin/nologin apb   # once
sudo -u apb -H apb server key issue
```

`-H` matters: it sets `HOME` to the service user's home so the key lands in
`/home/apb/.config/apb/server-auth.yaml`. If the unit sets `XDG_CONFIG_HOME` or
`APB_CONFIG_DIR`, pass the same value here, and confirm with:

```sh
sudo -u apb -H apb server key list
```

The key itself is printed once, in the form `apb_` followed by 43 characters.
Only its SHA-256 is stored, with mode 0600, so a lost key cannot be recovered:
issue a new one and revoke the old one.

Authentication turns on the moment the first key exists and turns off again
when the last one is revoked. A running dashboard notices within a minute, and
immediately on the next request that fails to authenticate, so no restart is
needed for either.

```sh
sudo -u apb -H apb server key list             # ids and creation times, never the keys
sudo -u apb -H apb server key revoke <id>      # remove one
```

At most two keys exist at a time. That is the rotation window: issue the
second, move every client over, then revoke the first.

## 2. Bind

Keep the default `127.0.0.1` when the reverse proxy runs on the same host. That
is the recommended layout, and it means the dashboard is unreachable except
through the proxy.

Use `--bind 0.0.0.0` only when the proxy lives on another machine inside a
private network. Binding any non-loopback address with zero keys configured is
a startup error, not a warning.

A reverse proxy is mandatory for any non-loopback bind of either listener: both
set an http1 header-read timeout so a slowloris client cannot hold a socket open
indefinitely, but neither caps concurrent connections in-process. Connection
limiting, request-body timeouts, and TLS belong at the proxy, which is why the
supported topology never exposes apb directly.

```sh
apb dashboard --no-open                 # loopback, the default
apb dashboard --no-open --bind 0.0.0.0  # requires at least one key
```

The bind can also live in `<config_dir>/config.yaml`, where the flag overrides
it:

```yaml
port: 7321
server:
  bind: "127.0.0.1"
  public_base_url: "https://apb.example.com"
  trusted_proxies: ["127.0.0.1"]
```

`public_base_url` is the address the dashboard is reached at; when it is https,
the session cookie is issued with the `Secure` attribute. `trusted_proxies`
lists the exact peer addresses whose `X-Forwarded-For` and `X-Forwarded-Proto`
headers are believed. Those headers are used only for rate-limit keying,
logging, and the cookie `Secure` decision, never for an authentication
decision. Exact addresses only, no CIDR ranges.

Set `trusted_proxies` whenever `public_base_url` is set, and set it before
putting the dashboard behind the proxy. Without it every request arrives with
the proxy's own address, so all clients share a single rate-limit key and one
attacker can exhaust the failure budget for everyone. Startup prints a warning
naming this exact combination.

Only the RIGHTMOST `X-Forwarded-For` entry is believed. A proxy appends its own
view of the peer to whatever header the client sent, so the last entry is the
only one the proxy wrote itself; leftmost entries are client-supplied and
spoofable.

## 3. Reverse proxy

Caddy, which obtains and renews the certificate on its own:

```caddy
apb.example.com {
	reverse_proxy 127.0.0.1:7321
}
```

Caddy's `reverse_proxy` APPENDS the peer address to any `X-Forwarded-For`
header the client already sent, rather than replacing it. A caller can
therefore put anything it likes at the front of that list. apb reads the
RIGHTMOST entry precisely because that one is Caddy's own observation, so the
two-line config above is safe as written and needs no header scrubbing. Do not
"simplify" it by rewriting the header from the client's value.

nginx, with the certificate managed separately. Note `X-Forwarded-For
$remote_addr`, which sets a single-entry header from the socket peer and
discards whatever the client sent, so it is safe under either reading:

```nginx
server {
    listen 443 ssl http2;
    server_name apb.example.com;

    ssl_certificate     /etc/letsencrypt/live/apb.example.com/fullchain.pem;
    ssl_certificate_key /etc/letsencrypt/live/apb.example.com/privkey.pem;
    add_header Strict-Transport-Security "max-age=31536000" always;

    location / {
        proxy_pass http://127.0.0.1:7321;
        proxy_http_version 1.1;
        proxy_set_header Host $host;
        proxy_set_header X-Forwarded-For $remote_addr;
        proxy_set_header X-Forwarded-Proto $scheme;

        # The dashboard's live updates ride a WebSocket at /api/ws.
        proxy_set_header Upgrade $http_upgrade;
        proxy_set_header Connection "upgrade";
        proxy_read_timeout 3600s;
    }
}

server {
    listen 80;
    server_name apb.example.com;
    return 301 https://$host$request_uri;
}
```

TLS and HSTS belong to the proxy. apb does not serve https, does not manage
certificates, and does not emit HSTS headers.

## 4. Run it as a service

`/etc/systemd/system/apb-dashboard.service`:

```ini
[Unit]
Description=apb dashboard
After=network-online.target
Wants=network-online.target

[Service]
Type=simple
User=apb
Group=apb
ExecStart=/usr/local/bin/apb dashboard --no-open
Restart=on-failure
RestartSec=5
NoNewPrivileges=true
PrivateTmp=true

[Install]
WantedBy=multi-user.target
```

```sh
sudo systemctl daemon-reload
sudo systemctl enable --now apb-dashboard
journalctl -u apb-dashboard -f
```

Run apb as its own unprivileged user. That user owns the playbooks, the runs,
and every connector credential the runs use, so give it nothing else. It must
also be the user whose config directory holds the keys from step 1.

## 5. Verify that authentication is actually on

Do this before announcing the address to anyone. A key issued as the wrong user
leaves the dashboard unauthenticated behind the proxy, and nothing else in the
setup catches that.

```sh
curl -i https://apb.example.com/api/projects
```

Expected: `HTTP/2 401` with the body `{"error":"auth"}`. Any `200` here means
the running dashboard has no keys, so the key file belongs to a different user
than the service; redo step 1 with `sudo -u <service-user> -H`.

Then check the two other halves:

```sh
curl -i -H "Authorization: Bearer apb_..." https://apb.example.com/api/projects
```

Expected: `HTTP/2 200`. Finally open `https://apb.example.com` in a browser and
confirm the sign-in screen appears, that the key signs you in, and that the log
out control shows up in the top bar.

## 6. Signing in

Two credentials are accepted, and both work through any transparent proxy.

A browser: open the dashboard, paste the key once on the sign-in screen. The
server answers with an HttpOnly, SameSite=Lax session cookie valid for seven
days of activity; the key itself is never stored in the browser. Restarting the
server drops every session and returns to the sign-in screen.

A script or CI job:

```sh
curl -H "Authorization: Bearer apb_..." https://apb.example.com/api/runs
```

State-changing requests authenticated by the session cookie must also carry
`X-Requested-With: apb-dashboard`; the dashboard does this on its own. Bearer
requests do not need it.

## 7. Watching for brute force

Every failed authentication writes one line to stderr, which systemd puts in
the journal:

```text
apb auth_failed ip=203.0.113.9 path=/api/auth/login
```

More than 10 failures per minute from one address already earn HTTP 429 for the
rest of that minute. To ban repeat offenders at the firewall, add a fail2ban
filter at `/etc/fail2ban/filter.d/apb.conf`:

```ini
[Definition]
failregex = ^apb auth_failed ip=<HOST> path=\S+$
ignoreregex =
```

and a jail at `/etc/fail2ban/jail.d/apb.conf`:

```ini
[apb-dashboard]
enabled = true
backend = systemd
journalmatch = _SYSTEMD_UNIT=apb-dashboard.service
filter = apb
maxretry = 10
findtime = 600
bantime = 3600
```

## 8. Receiving webhooks

A connector that receives events needs a public HTTPS endpoint. The listener
is separate from the dashboard on purpose: it is its own socket with its own
router, and pointing a proxy or tunnel at it cannot reach `/api`.

Enable it in the global config:

```yaml
ingest:
  enabled: true
  bind: "127.0.0.1"
  port: 7322
  public_base_url: https://hooks.example.com
```

`apb dashboard` then co-starts it. On a machine that runs no dashboard, run
`apb ingest` instead; both use the same implementation. `enabled` gates only
the co-start: `apb ingest` runs when you ask it to whatever the flag says, and
prints a line noting that the dashboard will not start the listener on its own
until the flag is set.

Proxy the hooks host to it, and nothing else. With Caddy:

```caddyfile
hooks.example.com {
	reverse_proxy 127.0.0.1:7322
}
```

Or with nginx:

```nginx
server {
	listen 443 ssl;
	server_name hooks.example.com;

	location /hooks/ {
		proxy_pass http://127.0.0.1:7322;
		proxy_set_header Host $host;
		client_max_body_size 256k;
	}

	location /healthz {
		proxy_pass http://127.0.0.1:7322;
	}

	location / {
		return 404;
	}
}
```

Use a separate hostname from the dashboard. Sharing one host and routing by
path works, but it puts the two surfaces one proxy typo apart, and the whole
point of the second listener is that a typo cannot reach the API.

Keep `ingest.bind` on the loopback interface and let the proxy reach it
there. Binding anywhere else puts the hook endpoints on the network with no
TLS of their own. apb cannot refuse that the way `apb dashboard` refuses a
non-loopback bind without a key, because on this listener the signature is
the authentication and there is no key to require, so it prints a warning to
stderr at startup and leaves the decision to you.

Add the proxy's own address to `server.trusted_proxies`, the same key the
dashboard uses. The ingest listener reads it too, and without it every
delivery arrives from the proxy's loopback address, so the per-sender failure
budget is shared by every provider. A validly signed delivery is accepted
whatever that budget says, so events are not lost either way, but a shared key
means one sender with a stale secret exhausts it for everyone: unsigned
requests and the subscription handshake (`GET /hooks/...`) are then refused
for all senders until the window rolls, and the fail2ban filter below would
ban the proxy instead of the sender. With the key set, the listener attributes
a delivery to the rightmost `X-Forwarded-For` entry, which is the one the
proxy itself appended.

```yaml
server:
  trusted_proxies: ["127.0.0.1"]
```

**Never point fail2ban, or any address-based ban, at the proxy's own
address.** The failure limiter below is keyed by client IP; without
`trusted_proxies` set as shown above, that IP is the proxy itself, and a ban
rule reading its log would ban the proxy, not the sender.

Register the callback URL with the provider. `apb connector doctor` prints
the exact one per account:

```
[ok]   connector `whatsapp` account `main`: callback: register this URL with the provider: https://hooks.example.com/hooks/whatsapp/main
```

Accounts are resolved from the global `<config-dir>/connector-config/` only.
The hook path carries no workspace, so a project-scoped account cannot be
addressed by a delivery.

Watch for rejected deliveries the same way you watch for auth failures. The
listener logs one line per rejection, keyed by a rolling 60 second, 10-failure
window per client address:

```sh
journalctl -u apb -f | grep apb ingest_rejected
```

A fail2ban filter matching `apb ingest_rejected ip=<HOST>` bans an address
that keeps sending bad signatures. As above, point it at the sender's address
only once `trusted_proxies` is configured; otherwise every ban lands on the
proxy.

**Deliveries that arrive while the listener is down are lost.** Providers
retry for a limited window and then give up. apb cannot change that: it has
no way to ask for a redelivery, and nothing buffers on its behalf while the
machine is asleep, the tunnel is down, or the service is restarting. If the
events matter, run the listener somewhere that stays up.

## Notes and limits

- `POST /api/hooks/{run_id}/{secret}` stays reachable from the internet by
  design, without a key: it is how an external service signals a `wait:
  webhook` node, and it authenticates itself with the per-run path secret in
  its own URL. If nothing in your playbooks receives external webhooks, you can
  restrict it at the proxy, for example with a Caddy matcher that answers 404
  for `/api/hooks/*` or an nginx `location /api/hooks/ { deny all; }`. Do not
  restrict it if any run waits on a webhook.
- Every response carries `X-Frame-Options: DENY`, so the dashboard cannot be
  framed. That does not depend on your proxy configuration, and a proxy should
  not strip it.
- `apb dev` is a source-tree development command, not a deployment path. It
  serves the Vite dev server next to the API on the loopback interface. If keys
  exist, the developer signs in once through the Vite proxy like any other
  browser client.
- Sessions live in memory only. A restart signs everyone out.
- There are no user accounts, roles, or per-key scopes. A key is full access to
  the API, which is why there are at most two of them and why they belong only
  to operators.
- The MCP server (`apb mcp`) speaks stdio and never traverses HTTP, so nothing
  here applies to it.
