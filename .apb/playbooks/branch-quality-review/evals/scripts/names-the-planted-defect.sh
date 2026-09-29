#!/bin/sh
# Passes when the report names src/lib.rs and points at a line within 3 of
# the planted off-by-one error (line 16 of the planted fixture, the loop
# `for start in 0..xs.len() - k`, which skips the last window). Accepted
# line forms: `lib.rs:16`, `lib.rs#L16`, `line 16`, `lines 14-18`,
# `**Line:** 16`.
set -eu
report=docs/reviews/_date_time_review.md
defect=16
[ -f "$report" ] || { echo "missing $report"; exit 1; }
grep -q 'lib\.rs' "$report" || { echo "the report does not name src/lib.rs"; exit 1; }
lines=$(grep -oiE '(lib\.rs:|#L|lines?[*:]* *)[0-9]+' "$report" | grep -oE '[0-9]+$' || true)
for n in $lines; do
  d=$((n - defect))
  [ "$d" -lt 0 ] && d=$((0 - d))
  if [ "$d" -le 3 ]; then
    exit 0
  fi
done
echo "the report does not point near line $defect of src/lib.rs (found: ${lines:-none})"
exit 1
