# Release announce

Kind: routine. Runnable example: examples/playbooks/release-announce.yaml.

## 1. Purpose

Publish once, announce everywhere: create the GitHub release, then send the
email and post to Telegram. The scenario stands for the multi-channel
routines where one write publishes and the others follow it, and where the
danger is repetition: a retry or a re-run after a partial failure must not
re-publish or double-announce.

## 2. Initial state of the world

- Tag v1.4.0 is pushed; notes for it are written.
- No GitHub release exists for the tag.
- The announce mailbox and the Telegram chat are configured and have received
  nothing for v1.4.0.

## 3. Steps

1. Create the GitHub release for the tag with the notes. (agent node,
   connector write: github create_release)
2. Send the announcement email, carrying the release link. (agent node,
   connector write: smtp send_email)
3. Post the announcement to Telegram. (agent node, connector write: telegram
   send_message)
4. The goal criteria check the release URL marker; delivery to both channels
   is a reader's criterion. (goal criteria)

The example runs the three steps sequentially; the two announce steps are
independent of each other and could be a fork today.

## 4. Expected outcome

One release exists for v1.4.0, one email with the release link was sent, one
Telegram message was posted, and each channel's completion is visible in the
run report.

## 5. Failure case

The Telegram call times out after the email went out. The node is retried,
and later the whole run is re-run by an operator who misread the state.

- What apb must show: per channel, done or not done, before anything is
  retried; and on the re-run, which writes the earlier pass already made.
- What must NOT happen: the retry of the timed-out Telegram node posting
  the announcement twice (the post may have gone through before its
  confirmation was lost); the operator's re-run creating the release a
  second time (or failing messily on the existing tag while the operator
  cannot tell what happened) or sending the email twice. The retry only
  repeats the Telegram node; the release and the email are at risk from the
  re-run.

## 6. Today

examples/playbooks/release-announce.yaml covers the happy path as written.
The gaps:

1. No idempotency: nothing marks the release or the email as done by this
   run, so a re-run repeats every completed write or stops on the existing
   tag with no honest report.
2. Per-channel completion lives only in the transcript; there is no ledger a
   retry or a re-run can read back.
3. A write that succeeded but whose confirmation was lost (the timeout case)
   is indistinguishable from a write that never happened.

## 7. Target

Each channel write carries an idempotency key (the tag plus the channel);
before writing, a check asks the channel whether the announcement is already
there; after an uncertain outcome, the check settles it. The run report
lists per channel: written, skipped (already there), or failed. A feature
closing these gaps carries an engine test that kills the run between the
email and the Telegram post and asserts the re-run sends no second email,
and one that loses the Telegram confirmation and asserts the retry posts no
second message.
