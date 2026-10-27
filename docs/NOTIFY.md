# Notifications ("Notify me")

A run can tell a person when it needs them or when it is done, through a
connector they already use: a Telegram chat, a Slack or Zulip channel, a
mail address. Notifications are opt-in. Nothing is sent unless the machine
default is on (and the machine config names a target), or the person asked
for it on this run, naming a target or using the machine's.

A notification carries the run's status, what the person is asked to do,
and a link to the run. It never carries the question text, the review
prompt, agent output or a secret: those stay in the run.

A review gate's title, the one string of a notice a playbook author writes,
is sent as plain text: the characters chat services read as markup become
spaces.

## What is sent

A run notifies about four transitions:

| Transition | When |
| --- | --- |
| `review` | A `human_review` gate waits for a decision |
| `question` | An interactive node asked a question nobody has answered |
| `paused` | The engine or the supervisor paused the run (a quota pause, a budget pause, a supervisor pause) |
| `finished` | The run ended, or something other than a person stopped it |

Only news is sent:

- A transition that happened before the subscription took effect is not sent.
- A gate that closed before the run got to send its notice is not sent.
- A pause or a stop a person made is not sent: they already know.
- The same wait never sends a second message, also when the driver restarts
  or a resume reads the same journal again.

A sub-playbook inherits only `review` and `question` from its parent run, so
a person hears about a gate deep in the tree but not about every child run
that ends. It follows the parent's subscription as it is at each send (and
its run views show it that way): a
parent turned off, narrowed or pointed at a new target does the same to its
children.

## Setting it up

The machine's `config.yaml` (`~/.config/apb/config.yaml`, or
`$APB_CONFIG_DIR/config.yaml`) holds the target:

```yaml
notify:
  default: off                 # on: every run notifies; off: only runs that ask. Default off.
  target:
    connector: telegram
    account: me
    function: send_message
    to: "123456789"            # the chat id, channel or address
  on: [review, question, paused, finished]   # the default
```

The target is one function of one configured connector account. The
function must be able to carry notifications: its manifest declares a
`people:` block that names the recipient argument (`to`) and every argument
its `args_schema` requires, and declares `idempotency`, so a notice goes out
at most once (see "people:" in `docs/CONNECTORS.md`). The official `telegram.send_message`,
`slack.send_message`, `smtp.send_email`, `zulip.send_stream_message` and
`zulip.send_direct_message` can. The connector and the account must be
approved (`apb connector approve <name>` and `--account <account>`).

`to` is written into the run's journal, so it must be a recipient, never a
credential: a value shaped like a token is refused, and so is one that
starts or ends with whitespace. `on` names at least one transition.

A project may narrow this in its `.apb/config.yaml`. That file is often
committed, so it names no target and no recipient:

```yaml
notify:
  enabled: false               # this project's runs ignore the machine default
  on: [review, finished]       # or notify about fewer transitions
```

An empty `on` is refused; `enabled: false` is how a project turns the
default off. A run that asks for notifications and has no transition left
once the project narrows them is refused, not started silent.

Check the setup with one test message:

```text
apb notify            # the machine's target and whether runs here notify by default
apb notify test       # sends one test message through the target
```

## Per run

A run asks for its own notifications at start, or a person changes them on a
live run. The run's own request comes first, then the project, then the
machine.

| Surface | At start | On a live run |
| --- | --- | --- |
| CLI | `apb run <id> --notify`, `--no-notify`, `--notify-on review,finished`, `--notify-via telegram/me[:send_message]`, `--notify-to <recipient>` | `apb notify <run> --on[=LIST]`, `--off`, `--via`, `--to` |
| MCP | `playbook_run` with `notify`: `true`, `false` or `{ on, target }` | `run_notify`: `true`, `false` or `{ on }`, never a target |
| HTTP | `notify` in the body of `POST /api/playbooks/{id}/run` | `POST /api/runs/{id}/notify` |
| Dashboard | "Notify me" in the Run dialog (shown when the machine has a target) | "Notify me" / "Stop notifying" on the run page |

`--notify-via` and `--notify-to` fill the parts they leave out from the
machine's target (on a live run, from the run's current target) when it
uses the same connector account. A change on a live run that leaves out the
target or the transitions keeps the run's current ones. A list after
`--on` is joined with `=` (`--on=review,finished`), so a run id after a bare
`--on` stays the run id. A recipient may start with `-` (a Telegram group's
chat id).

Who hears about a run, and through which account, is the person's to
decide. `apb notify <run>` with a change and `apb notify test` are refused
inside a run step (`APB_RUN_ID` or `APB_HOST_TASK` set), and so is a start
from inside a run step that names its own target (`apb run --notify-via`
or `--notify-to`, MCP `playbook_run` with `notify.target`): it may ask for
the machine's target only. MCP `run_notify` turns notifications on or off
and narrows the transitions, never past the ones the person chose (on a run
that is off, the ones it last notified about, or else the default), and
never names a target: a new target comes from a terminal outside any run or
from the dashboard. This guards against a step doing so by mistake; it
reads the environment the engine sets, so it is not a security boundary.

A start that asked for notifications that cannot be sent (no target, a
function that cannot carry them, a connector or account that is not
approved) is refused before the run exists (`notify_unavailable`). A machine
default that cannot be used, or settings that do not read, do not stop the
run: the start warns, and the run records the problem under the source
whose file did not read (`machine` or `project`). A run that asked for no
notifications checks nothing. A change on a run that has ended is refused
(`run_ended`, HTTP 409). A `notify` value of the wrong shape is refused as
`invalid_notify` (HTTP 400, with what did not parse); an MCP host that
sends `run_notify` a target gets the tool's invalid-parameters error,
since its argument has no such field.

`apb runs <id>`, MCP `run_status` and `run_report`, `GET /api/runs/{id}` and
the run page show the run's notifications: whether they are on, the target,
the transitions, where the subscription came from, each transition notified
with what became of it (`pending`, `confirmed`, `not_sent`, `unknown`), and
why a subscription could not be used or a notification was given up.

## Trust

Notifications sit outside the run permit. The run gate pins the playbook,
its profiles, its connectors and their accounts; a notification target is
none of those. It is the person's own subscription, so it is checked on its
own terms:

- When it is set (at start, or when a person changes it on a live run), the
  live connector and account must load, the function must carry
  notifications, and both must be approved. The run journals the target
  together with the connector digest and the account digest it was approved
  with (`notify_set`).
- At every send, the live connector and account are checked again against
  those pinned digests. A connector or an account that changed since, or
  lost its approval, is refused before anything leaves; the notification is
  recorded as `not_sent` (source `gate`) and is not tried again until the
  person sets the subscription up again, which pins the new version and
  sends the transitions still open.

The secrets the target's account uses are withheld from every agent process
of the run, like the secrets of any installed connector.

## Sending once

A notification is apb's own call under the engine site `@notify` (a node may
not use an id that starts with `@`, V68). It goes through the same send
ledger as a node's sends, labelled `notify:<transition>.<seq>`, so it is
recorded before the request leaves and a restart reads the record instead
of sending again. A send whose outcome is not known is not sent again
through a function with `idempotency: none` or `marker`; with
`idempotency: key` it is sent again only while the replay still lands
inside the service's deduplication window, since the service itself drops
the repeat. A send that failed, or that a passing fault kept from going out
(a target that did not load, a secret that did not resolve, a journal that
did not take the intent), is tried at most three times in all, at least 30
seconds apart. One sweep sends for at most 15 seconds before the drive
loop goes on: each send gets the time the sweep has left, at most 15
seconds, and none starts with less than a second left. A failed
notification never fails the run.

The drive loop sweeps on every turn, and an attempt that waits on a live
question (a CLI agent's `ask_user`, a host task's question) sweeps from its
own poll loop, at most once a second and for at most 5 seconds, after the
question timeout is enforced, so a question is sent while it is still
open. The driver's last sweep runs when the run pauses, ends or
fails. A notification that fails in that last sweep is tried again only by
a later driver: a resume of a paused run sends it, but a run that has ended
has none, so the run's notify block lists it as given up.

Since the Slack `send_message` and the Zulip send functions now declare
`idempotency: none`, a send through them whose outcome is not known (the
request left but no answer came back) is refused on a repeat instead of
being sent again, until a person decides (`apb intent resolve`) or the
binding opts into `on_unknown: resend`. This applies to every send through
those functions, not only to notifications.

## The run report as a page

The run report is also one self-contained HTML page a person can keep, mail
or open from disk: `apb runs <id> --html <path|->`, MCP `run_report_html`
(writes `runs/<id>/report.html` and answers with its path and size; it is
not a read-only tool, so a host asks before it writes), `GET
/api/runs/{id}/report.html`, and "Save report" on the run page. The page has
no script, no event handler, no external resource; its one stylesheet is
pinned by hash in its Content Security Policy, and every string from the
run is redacted (whole, before any cut, and each journal event field by
field before it is serialised) and escaped.
