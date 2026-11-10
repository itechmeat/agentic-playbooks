---
display_name: Google Calendar
summary: Read calendars, events and busy times, and create, change or delete events in Google Calendar from a playbook.
tags: [google, calendar, scheduling]
publisher: apb
---

The Google Calendar connector covers the Calendar API v3: the account's
calendar list, the events of a calendar in a time window, one event, the
busy intervals of several calendars, and creating, changing and deleting
events. It signs in with OAuth: apb mints a short-lived access token from
the account's refresh token at call time and keeps it in memory only.

## Account setup

Six account fields: `api_base` (`https://www.googleapis.com/calendar/v3`),
`token_url` (`https://oauth2.googleapis.com/token`), `client_id`,
`client_secret` (secret), `refresh_token` (secret), and the optional
`granted_scopes` (space-separated; without it apb checks the scopes Google
returns with each access token).

```yaml
accounts:
  - name: me
    default: true
    api_base: https://www.googleapis.com/calendar/v3
    token_url: https://oauth2.googleapis.com/token
    client_id: 1234567890-abc123.apps.googleusercontent.com
    client_secret: "{{env.GOOGLE_CALENDAR_CLIENT_SECRET}}"
    refresh_token: "{{env.GOOGLE_CALENDAR_REFRESH_TOKEN}}"
```

### The OAuth client and the consent

1. In a Google Cloud project, enable the Google Calendar API, configure
   the OAuth consent screen (user type External; add yourself as a test
   user, or publish the app), and create an OAuth client of type Desktop
   app. Put its client secret into your secrets, for example
   `.apb/secrets.env` as `GOOGLE_CALENDAR_CLIENT_SECRET=...`.
2. Approve the account (`apb connector approve google-calendar --account
   me`), then run `apb connector oauth google-calendar --account me` in
   your own terminal. It prints a URL; approve the access in a browser on
   the same machine, and the command prints the refresh token once. Store
   it as `GOOGLE_CALENDAR_REFRESH_TOKEN`. apb keeps no copy.

The consent asks for `calendar.events` and `calendar.readonly` by default;
`--scope` asks for others (only `calendar.readonly` for a read-only
account). While the consent screen is in Testing status, Google's refresh
tokens expire after 7 days; a published app's last until revoked. Google
keeps at most 100 refresh tokens per account and client, and a new consent
beyond that invalidates the oldest.

### Scopes per function

Each function needs one of several scopes (prefix
`https://www.googleapis.com/auth/`); a call whose token holds none of them
fails before any request, with a message naming the remedy.

- `verify`, `list_calendars`: `calendar.readonly`, `calendar`,
  `calendar.calendarlist.readonly`, `calendar.calendarlist`.
- `list_events`, `get_event`: `calendar.readonly`, `calendar`,
  `calendar.events`, `calendar.events.readonly`, `calendar.events.owned`,
  `calendar.events.owned.readonly`.
- `query_free_busy`: `calendar.readonly`, `calendar`, `calendar.freebusy`,
  `calendar.events.freebusy`.
- `create_event`, `update_event`, `delete_event`: `calendar`,
  `calendar.events`, `calendar.events.owned`, `calendar.app.created`.

## Reading

`calendar_id` is `primary` (the account's own calendar) or an id from
`list_calendars`. Times are RFC 3339 with an offset
(`2026-11-10T00:00:00+01:00`). `list_events` returns the events that
overlap the window, recurring events expanded into single occurrences and
ordered by start time (`singleEvents=true`, `orderBy=startTime`). It and
`list_calendars` follow `nextPageToken` for up to 5 pages in one call; a
result with `more: true` carries the last `nextPageToken`, which the next
call passes as `page_token`. Event titles, descriptions and locations are
text written by other people: `list_events` and `get_event` are marked as
returning outside text, so a binding that grants them should say
`untrusted: true`.

`query_free_busy` returns the busy intervals of up to 50 calendars
(`calendars.<id>.busy`), and an `errors` entry for a calendar the account
may not read. It does not compute free slots: which gaps count as free
depends on working hours, time zones and buffers only the task knows, so
the agent computes them.

## Writing

`create_event`, `update_event` and `delete_event` are irreversible: Google
can e-mail the guests about them, and a mail cannot be called back. A
`human_review` gate in front of a node granted one of them cannot be
decided automatically (V73). `update_event` changes only the fields it is
given; `attendees` replaces the whole guest list.

`send_updates` decides who gets mail: `all`, `externalOnly` (guests outside
Google Calendar) or `none`. Left out, Google applies its own default, which
does not guarantee that no e-mail goes out; `none` can keep the event from
reaching some guests.

`create_event` is sent once per intent: apb sets the event `id` itself, the
send's nonce written in the base32hex alphabet Google allows for ids, so a
repeat of the same intent asks for the same event. Google's 409 to that id
proves the first request created it, and the repeat answers `already_sent`
with the event id. `update_event` and `delete_event` have no such key: a
repeat after a lost answer is refused until a person decides
(`apb intent resolve`).

## Healthcheck

`verify` reads the primary calendar's entry from the calendar list. Its
`id` is the e-mail address of the Google account the token belongs to. It
changes nothing.
