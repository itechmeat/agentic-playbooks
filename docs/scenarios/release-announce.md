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

examples/playbooks/release-announce.yaml covers the happy path as written,
and each step names its send with `--intent`. The gaps:

1. No idempotency across runs: the send ledger belongs to one run, so an
   operator's re-run repeats every completed write or stops on the existing
   tag, and reads nothing the earlier run recorded.
2. Telegram, SMTP and GitHub's `create_release` offer no way to look a send
   up afterwards: a post, an email or a release whose confirmation was lost
   stays unknown until a person decides, so the retry refuses it rather
   than settling it.

Closed within a run: every release, email and Telegram send is recorded in
the run's send ledger before it leaves and settled afterwards, so a retry of
the timed-out Telegram node is refused with `unknown_effect` instead of
posting a second announcement (or, with `on_unknown: resend` on the binding,
sends it again as a recorded resend). The retry's prompt briefs it on what
the failed attempt sent, the run report lists the send as an unknown gap
naming `apb intent resolve`, and the run reads as needing a look. A send
that went through is answered `already_sent` on a repeat, so per-channel
completion is no longer only in the transcript (engine tests
`an_unknown_telegram_send_is_refused_unless_the_binding_allows_a_resend` in
crates/apb-engine/tests/suite/send_intent_test.rs,
`a_retry_is_briefed_on_what_the_failed_attempt_sent` in
crates/apb-engine/tests/suite/attempt_briefing_test.rs and
`an_unknown_send_is_a_gap_and_a_send_settled_later_is_not` in
crates/apb-engine/tests/suite/run_outcome_test.rs).

## 7. Target

The send ledger reaches across runs: a re-run of the same release reads what
the earlier run sent and answers those writes instead of repeating them;
before writing, a check asks the channel whether the announcement is already
there where the channel allows it. The run report lists per channel:
written, skipped (already there), or failed. A feature closing these gaps
carries an engine test that kills the run between the email and the Telegram
post and asserts the re-run sends no second email.
