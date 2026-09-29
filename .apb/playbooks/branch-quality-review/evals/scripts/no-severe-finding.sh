#!/bin/sh
# Passes when no finding of the report is ranked above the lowest severity:
# no heading, bold label or bracketed tag reading critical, blocker, high,
# major, P0 or P1. A heuristic over free text; a wording the pattern misses
# reads as a pass, so read the stored report when this case flips.
set -eu
report=docs/reviews/_date_time_review.md
[ -f "$report" ] || { echo "missing $report"; exit 1; }
severe='(critical|blocker|high|major|p0|p1)'
if grep -inE "^#+ .*\b$severe\b|\*\*\[?$severe\]?\*\*|\[$severe\]|severity: *$severe\b" "$report"; then
  echo "the report ranks a finding above the lowest severity"
  exit 1
fi
