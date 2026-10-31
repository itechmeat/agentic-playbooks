# Release heartbeat

Kind: routine. Runnable example: examples/playbooks/release-heartbeat.yaml.

## 1. Purpose

A periodic, mostly read-only look: check the latest pipeline on a branch,
summarize it for the release channel, and file a tracking issue only when it
failed. The scenario stands for the observe-then-maybe-act routine: the same
playbook must be safe to run again and again, quiet on green, and it must
never produce two issues for one failure.

## 2. Initial state of the world

- The latest pipeline on main just finished. It failed at the test stage.
- The release channel has received no summary for it.
- No YouTrack issue exists for this pipeline.

## 3. Steps

1. Fetch the most recent pipeline for the ref and its details. (agent node,
   connector read: gitlab list_pipelines, get_pipeline)
2. Summarize the state for the channel. (agent node, judgment only)
3. If and only if the pipeline failed, create exactly one YouTrack issue
   with the failure details and the pipeline link. (agent node, connector
   write: youtrack create_issue; the conditional lives in the prompt today)
4. Post the summary, plus the issue reference when one was filed, to Slack.
   (agent node, connector write: slack send_message)
5. The goal criteria check the status marker; exactly-one-issue is a
   reader's criterion. (goal criteria)

## 4. Expected outcome

The release channel has the summary. For a failed pipeline: exactly one
YouTrack issue exists for it. For a green pipeline: none. Running the
playbook again an hour later adds no issue and posts a fresh, accurate
summary.

## 5. Failure case

Two runs overlap (an operator starts one by hand while the schedule's run is
still going) and both see the failed pipeline: two YouTrack issues exist for
one failure, and the channel gets two summaries.

- What apb must show: that two runs of the same playbook are in flight over
  the same external resource, and afterwards, that the issue count is two
  with both run ids.
- What must NOT happen: a second issue for a pipeline that already has one;
  a silent merge of the two runs' outputs into one seemingly-clean report.

## 6. Today

examples/playbooks/release-heartbeat.yaml covers the single-run happy path
as written. The gaps:

1. "Exactly one issue" is prompt discipline; no check queries YouTrack for
   an existing issue before creating one.
2. Overlapping runs cannot see each other: the engine's busy lock guards the
   working tree, not the external resource both runs act on.
3. The file-only-on-failure condition lives inside one prompt; an output
   field plus a conditional edge would make it structural, and the example
   does not use that form.
4. There is no schedule/dedup key for recurring runs of the same playbook
   over the same resource (the tag, the pipeline id).

## 7. Target

The heartbeat carries a dedup key (the pipeline id); before filing, a check
asks YouTrack whether an issue for that key exists; overlapping runs over
the same key take turns or the second reports "already handled". The
failure-conditional is an edge, not a sentence. A feature closing these gaps
carries an engine test that runs the playbook twice over the same failed
pipeline and asserts exactly one issue exists.
