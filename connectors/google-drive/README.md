# google-drive: setup for humans

## The short way: let an agent do it

Setting this connector up by hand means creating a Google Cloud project, enabling the Drive API, configuring a consent screen, creating an OAuth client, editing two files, approving trust, and running a consent in your browser. An agent can do almost all of it for you. It stops twice: once for the client secret, and once for the consent, which only you can give, in your own terminal and browser.

Paste a prompt like this to a coding agent that has `apb` available:

> Install the apb `google-drive` connector so playbooks can search and read my Google Drive, then read `connectors/google-drive/INSTALL.md` under the apb config directory (by default `~/.config/apb`) and follow it to the end. Ask me for the client secret, and tell me when it is my turn to run the consent.

The agent installs the connector, writes the account config, prepares the secrets file, approves trust, hands you one command to run, and then runs a live healthcheck that asks Google which account the token reads Drive as. What you get back is either a working account or a specific error.

## What you will be asked for

A Google Cloud project with the Google Drive API enabled and an OAuth client of type Desktop app. Creating them is free and takes a few minutes in the [Google Cloud console](https://console.cloud.google.com/); the agent walks you through the screens. You copy two values from the client: its client id, which goes into the account config, and its client secret, which goes into a local secrets file with owner-only permissions.

Then you run `apb connector oauth google-drive` in your own terminal. It prints a link; you open it, sign in with the Google account whose Drive the playbooks should read, and approve. The command prints a refresh token once, and you store it in the same secrets file. The agent never runs this step and never sees the consent: apb refuses the command inside a run and without a terminal.

apb keeps no copy of any token. At call time it trades the refresh token for a short-lived access token that lives only in the memory of the process making the call.

## The scope, and the warning screen

The consent asks for one scope, `https://www.googleapis.com/auth/drive.readonly`: read every file the account can open, its own and the ones shared with it, shared drives included. Nothing in this connector can change a file, but the scope itself is wide, so the way to narrow what a playbook node may do is the function list of its grant.

Google classes `drive.readonly` as a restricted scope. An app that only you use for your own account works without Google's verification: the consent shows a warning that Google has not verified the app, and you continue past it through Advanced. That is expected for an app you created yourself.

While the consent screen's publishing status is Testing, Google lets only the test users you list sign in, and the refresh tokens it issues expire after 7 days. For a longer-lived setup, publish the app to production (it stays unverified, with the same warning screen) and run the consent once more.

## What this connector can and cannot do

It searches files and folders with Drive's query language, reads a file's metadata, exports a Google file as text, and reads a stored text file. That is the whole surface.

- `export_text` turns a Google file into text: Docs into `text/plain` or `text/markdown`, Sheets into `text/csv` or `text/tab-separated-values` (the first sheet only), Slides into `text/plain`.
- `read_text_file` returns the content of a file stored in Drive as text: plain text, Markdown, CSV, JSON, XML and YAML. A JSON file comes back as the parsed document. A Google Docs, Sheets or Slides file has no stored content, so `read_text_file` fails on it; use `export_text` for those.
- An answer that is not text (an image, a PDF, an archive) is refused before apb reads it: the call fails with a `service` error naming the type.
- apb reads at most 1 MiB of one answer. A longer file or export fails with `incomplete_response` rather than returning a cut text. Google's own limit for an export is 10 MB.
- `search_files` follows `nextPageToken` for up to five pages in one call and says `more` when the listing goes on; the trash is included unless the query says `trashed = false`.

It cannot upload, create, edit, move, share or delete anything: the connector has no function that writes, and none that sends a binary request body. It cannot read binary files, and it cannot export to formats other than the text ones above.

Everything a file holds was written by someone outside the run, so the results are marked as outside text: apb tells the agent to treat them as data, never as instructions, and suggests marking a binding that grants these functions `untrusted: true`.

## Doing it by hand

If you would rather not involve an agent, `PUBLIC.md` in this folder has the account fields, the scopes per function and a config example, and `docs/CONNECTORS.md` in the apb repository covers accounts, secrets, OAuth accounts and trust in general. `INSTALL.md` is written for an agent but the steps read fine as a checklist.
