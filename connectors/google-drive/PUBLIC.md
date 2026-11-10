---
display_name: Google Drive
summary: Search Google Drive and read files as text from a playbook, through a read-only OAuth account.
tags: [google, drive, files, documents, oauth]
publisher: apb
---

The Google Drive connector covers searching files and folders, reading a
file's metadata, exporting Google Docs, Sheets and Slides as text, and
reading stored text files (plain text, Markdown, CSV, JSON, XML, YAML)
through the Drive API v3. It is read-only: nothing in it uploads, edits,
shares or deletes a file. Every result is marked as outside text, so the
agent prompt tells the agent to treat it as data, never as instructions.

## Account setup

Six account fields: `api_base` (normally
`https://www.googleapis.com/drive/v3`), `token_url` (normally
`https://oauth2.googleapis.com/token`), `client_id`, `client_secret`
(secret), `refresh_token` (secret), and the optional `granted_scopes`.

```yaml
accounts:
  - name: default
    api_base: https://www.googleapis.com/drive/v3
    token_url: https://oauth2.googleapis.com/token
    client_id: 1234567890-abc123.apps.googleusercontent.com
    client_secret: "{{env.GOOGLE_CLIENT_SECRET}}"
    refresh_token: "{{env.GOOGLE_DRIVE_REFRESH_TOKEN}}"
```

The account uses `oauth2_refresh` auth: at call time apb trades the
refresh token for a short-lived access token, which lives only in the
memory of the calling process and is never written anywhere. A 401
answer renews it once and repeats the request once.

### The OAuth client and the consent

1. In a Google Cloud project, enable the Google Drive API.
2. Configure the OAuth consent screen (audience External for a personal
   account) and add the account as a test user.
3. Create an OAuth client of type Desktop app; its client id goes into
   the account config, its client secret into your secrets, for example
   `.apb/secrets.env` as `GOOGLE_CLIENT_SECRET=...`.
4. Approve the account (`apb connector approve google-drive --account
   default`), then run `apb connector oauth google-drive` in your own
   terminal. It prints a consent link, waits for Google to call back on
   `127.0.0.1`, and prints the refresh token once. Store it, for example
   as `GOOGLE_DRIVE_REFRESH_TOKEN=...`; apb keeps no copy.

The consent asks for `https://www.googleapis.com/auth/drive.readonly`
by default, a restricted scope: an app you create for your own account
works unverified, behind Google's warning screen. While the app's
publishing status is Testing, its refresh tokens expire after 7 days.

### Scopes per function

Any one listed scope is enough (prefix
`https://www.googleapis.com/auth/`):

- `verify`, `search_files`, `get_file_metadata`: `drive`,
  `drive.readonly`, `drive.metadata.readonly`, `drive.metadata`,
  `drive.file`.
- `export_text`, `read_text_file`: `drive`, `drive.readonly`,
  `drive.file`.

A token without a needed scope fails `auth` before any request reaches
Drive, naming the `apb connector oauth --scope` command that fixes it.
With `drive.file`, only files the app created or the person opened with
it are visible.

## Reading files as text

`export_text` exports a Google file: Docs to `text/plain` or
`text/markdown`, Sheets to `text/csv` or `text/tab-separated-values`
(the first sheet only), Slides to `text/plain`. `read_text_file` reads a
file stored in Drive, and returns a JSON file as the parsed document.
A Google Docs, Sheets or Slides file has no stored content: Google
answers `read_text_file` on one with HTTP 403 (`fileNotDownloadable`),
which apb reports as `auth`; use `export_text` for those.

Both functions declare the media types they accept (`content_types`): an
answer of any other type (an image, a PDF, an octet stream) fails
`service` naming the type, before apb reads its body. Both return the
whole document, with no projection.

Sizes: apb reads at most 1 MiB of one answer, and a longer text fails
`incomplete_response` instead of returning a cut document. Google's own
export limit is 10 MB.

## Searching and paging

`search_files` takes a Drive query in `q`, for example
`name contains 'plan' and trashed = false` (the trash is included unless
the query excludes it), and follows `nextPageToken` for up to five pages
in one call. The result says `pages` and `more`; when `more` is true,
pass the result's `nextPageToken` as `page_token` to continue. Shared
drives are included (`supportsAllDrives`, `includeItemsFromAllDrives`).

## Healthcheck

`verify` reads the `about` resource and returns the account's
`emailAddress` and `displayName`, confirming the client, the refresh
token and the scope work together. It reads no file.
