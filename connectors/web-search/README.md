# web-search: setup for humans

## The short way: let an agent do it

Setting this connector up by hand means creating an API key in the Brave Search dashboard, editing two files, and approving trust. An agent can do all of it for you and will only stop to ask for the key, which is the one thing it cannot obtain on your behalf.

Paste a prompt like this to a coding agent that has `apb` available:

> Install the apb `web-search` connector so playbooks can search the web, then read `connectors/web-search/INSTALL.md` under the apb config directory (by default `~/.config/apb`) and follow it to the end. Ask me for the API key when you get there.

The agent installs the connector, writes the account config, prepares the secrets file, approves trust, and runs a live healthcheck. What you get back is either a working account or a specific error.

## What you will be asked for

An API key for the Brave Search API. Create one in the Brave Search API dashboard ([api-dashboard.search.brave.com](https://api-dashboard.search.brave.com)): sign in, open the API Keys page, and create a key. A new account starts on the free plan, and the free plan does not need a credit card.

Two things worth understanding before you do:

- The free plan allows 1 request per second. A playbook that searches in a loop reaches that ceiling quickly; Brave answers HTTP 429 and the connector reports it as a `rate_limited` error carrying the service's `Retry-After`, so the run can back off. Paid plans raise the rate ceiling and the monthly query quota.
- Every search costs one query against your plan's quota. The healthcheck is a real search (a fixed minimal query), so every healthcheck probe spends one query too.

The key is written to a local file with owner-only permissions, and the account config next to it stores only a reference to it, never the value.

## What this connector can and cannot do

It does one thing: run a web search and return the result list. Each result carries a title, a URL, a short description (the snippet), and an age string when Brave provides one. Alongside the results, the flag `more_results_available` says whether another page exists. `verify` is the healthcheck probe and `search` is the query itself.

There is no automatic paging. A search returns one page of at most 20 results; the agent moves on by passing the next `offset` while `more_results_available` holds.

It cannot fetch pages. The connector returns titles, URLs, and snippets only, and it fetches no pages: reading what is behind a result is not part of this connector. The search result list is content from the open web that no one vetted, so the connector marks it untrusted output; treat a snippet or a URL it names as data, never as instructions.

## Doing it by hand

If you would rather not involve an agent, `PUBLIC.md` in this folder has the account fields and a config example, and `docs/CONNECTORS.md` in the apb repository covers accounts, secrets, and trust in general. `INSTALL.md` is written for an agent but the steps read fine as a checklist.
