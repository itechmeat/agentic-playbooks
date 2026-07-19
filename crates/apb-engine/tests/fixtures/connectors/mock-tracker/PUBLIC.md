---
display_name: Mock Tracker
summary: A fake task tracker for tests and connector authoring; never contacts a real service.
tags: [test, fixture]
publisher: apb
---

Mock Tracker is the reference connector shipped with apb's test suite. It exposes
mock functions for the canned outcomes a real tracker would return (a healthcheck
ping, an expired-token failure, and a throttled-client response) alongside two
HTTP functions (a read-only `list_items` and a side-effecting `create_item`) that
the end-to-end tests exercise against a local ephemeral server. It exists only to
prove the connector machinery end to end and is safe to install anywhere: it holds
no real endpoints and reaches no real network on its own.
