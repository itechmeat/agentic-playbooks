# google-calendar: setup for humans

## The short way: let an agent do it

Setting this connector up by hand means creating a Google Cloud project, enabling the Calendar API, configuring a consent screen, creating an OAuth client, editing two files, approving trust, and granting access in your browser. An agent can do most of it for you. It stops for three things only you can do: the clicks in the Google Cloud console, the client secret, and the consent itself, which you run in your own terminal.

Paste a prompt like this to a coding agent that has `apb` available:

> Install the apb `google-calendar` connector so playbooks can read my calendar and create events, then read `connectors/google-calendar/INSTALL.md` under the apb config directory (by default `~/.config/apb`) and follow it to the end. Ask me for the OAuth client details when you get there.

The agent installs the connector, walks you through the Google Cloud console, writes the account config, prepares the secrets file, approves trust, tells you which command to run for the consent, and runs a live healthcheck that reads your primary calendar's entry. What you get back is either a working account or a specific error.

## What you will be asked for

An OAuth client of type Desktop app, in a Google Cloud project of your own with the Google Calendar API enabled. The agent gives you the clicks. The client id is not a secret; the client secret is, and it goes into a local file with owner-only permissions. The account config stores only a reference to it, never the value.

Then one consent: you run `apb connector oauth google-calendar` in your own terminal, approve the access in your browser, and the command prints a refresh token once. You paste it into the secrets file yourself. apb keeps no copy, and the agent never needs to see it. By default the consent asks for two scopes, `calendar.events` (create, change and delete events) and `calendar.readonly` (read calendars, events and busy times).

Two Google rules decide how long that token lasts:

- While the consent screen of your project is in Testing status, Google issues refresh tokens that expire after 7 days, and the account stops working a week after each consent. Publish the app (In production) for a token that lasts until you revoke it. An app that Google has not verified then shows a warning screen at consent; for an app you made for your own account you can continue past it.
- Google keeps at most 100 refresh tokens per Google account and OAuth client. A new consent beyond that silently invalidates the oldest one.

## What this connector can and cannot do

It reads the account's calendar list, lists the events of a calendar in a time window (recurring events expanded into single occurrences, up to 5 pages in one call), reads one event, returns the busy times of up to 50 calendars, and creates, changes and deletes events.

It cannot compute free slots (see below), create a recurring series, add a video meeting link, set reminders or colors, attach files, answer an invitation, manage calendars or their sharing, or watch a calendar for changes.

### Why free slots are left to the agent

`query_free_busy` returns the busy intervals of each calendar, exactly as Google reports them. Which gaps between them count as free depends on rules only the task knows: working hours, each person's time zone, buffers between meetings, a lunch break, the length the meeting needs. A connector is declarative and computes nothing, so the agent gets the busy lists and applies the task's rules itself. A calendar your account may not read comes back with an error entry instead of a busy list, which is not the same as free; the agent sees the difference.

### Writes that mail people

`create_event`, `update_event` and `delete_event` are marked irreversible. When an event has guests, Google can e-mail them about it, and a mail cannot be called back. A deleted event cannot be restored through this connector either. Because of that, a `human_review` gate in front of a node that is granted one of these functions cannot be decided automatically (`auto_decide` is refused with V73), so a person stays at that gate.

Who gets mail is the `send_updates` argument of those three functions:

- `all`: every guest is notified.
- `externalOnly`: only guests who do not use Google Calendar.
- `none`: nobody is notified. Google warns that this can keep the event from reaching some guests at all, for example guests whose calendar is not a Google calendar.
- left out: Google applies its own default for that call, and documents exceptions to it. Leaving `send_updates` out does not guarantee that no e-mail goes out.

### Sending once

A step that is retried must not create the same event twice. `create_event` picks the event's id itself, derived from apb's send ledger, so a repeat of the same send asks Google for the same id. When the first answer was lost, the repeat gets Google's answer that the id already exists, which proves the first request created the event: the repeat is answered `already_sent` with the event id, and nothing is created twice. This protection is only as good as Google's duplicate check, which Google documents as not guaranteed at the moment of creation.

`update_event` and `delete_event` cannot be made safe that way. If the answer to one of them is lost, a repeat is refused until a person decides (`apb intent resolve`), because a repeat could mail every guest again.

### Granting it to a node

Grant what the step needs and no more: `functions: read_only` for a morning briefing, a list such as `[list_events, query_free_busy, create_event]` for a scheduling step. Give the grant a `max_calls` cap when a loop could reach it. Mark the binding `untrusted: true`: event titles and descriptions are written by other people, and apb's checks for untrusted input only see a binding marked so.

A read-only setup can also ask for less at consent time: `apb connector oauth google-calendar --scope https://www.googleapis.com/auth/calendar.readonly`. A write function then fails before any request, with a message naming the scope it needs.

## Doing it by hand

If you would rather not involve an agent, `PUBLIC.md` in this folder has the account fields and a config example, and `docs/CONNECTORS.md` in the apb repository covers accounts, secrets, OAuth accounts, and trust in general. `INSTALL.md` is written for an agent but the steps read fine as a checklist.
