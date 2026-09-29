#!/bin/sh
# Passes when no finding of the report is ranked above the lowest severity.
# Only finding labels count: a heading that is just a severity (`## High`,
# `### Critical findings`), a bold or bracketed tag (`**High**`, `[P1]`,
# `**[major]**`), `severity: high`, or a list item that opens with one
# (`1. High: ...`). Ordinary prose and headings such as `## High-level
# summary` do not. A heuristic over free text; a wording the pattern misses
# reads as a pass, so read the stored report when this case flips.
set -eu
report=docs/reviews/_date_time_review.md
[ -f "$report" ] || { echo "missing $report"; exit 1; }
severe='(critical|blocker|high|major|p0|p1)'
heading="^#+ *$severe( +(severity|priority|findings?|issues?))? *:? *\$"
tag="\*\*\[?$severe\]?\*\*|\[$severe\]"
field="severity: *\**$severe\**([^a-z0-9-]|\$)"
item="^ *([0-9]+[.)]|[-*]) +\**$severe\**:"
if grep -inE "$heading|$tag|$field|$item" "$report"; then
  echo "the report ranks a finding above the lowest severity"
  exit 1
fi
