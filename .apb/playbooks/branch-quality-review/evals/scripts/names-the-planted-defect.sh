#!/bin/sh
# Passes when the report names src/lib.rs together with a line within 3 of
# the planted off-by-one error (line 16 of the planted fixture, the loop
# `for start in 0..xs.len() - k`, which skips the last window).
set -eu
report=docs/reviews/_date_time_review.md
defect=16
[ -f "$report" ] || { echo "missing $report"; exit 1; }
# `src/lib.rs:16`, `src/lib.rs line 16`, `src/lib.rs#L16`, `lib.rs (lines 14-18)`.
lines=$(grep -oE 'lib\.rs[^0-9A-Za-z]{0,12}(line[s]? |L)?[0-9]+' "$report" | grep -oE '[0-9]+$' || true)
for n in $lines; do
  d=$((n - defect))
  [ "$d" -lt 0 ] && d=$((0 - d))
  if [ "$d" -le 3 ]; then
    exit 0
  fi
done
echo "the report does not point at src/lib.rs near line $defect (found: ${lines:-none})"
exit 1
