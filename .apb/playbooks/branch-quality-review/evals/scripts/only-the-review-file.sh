#!/bin/sh
# Passes when the run created docs/reviews/_date_time_review.md and nothing
# else outside ignored files: no other review file, no edited source.
# Runs in the eval tree after the run; exit 0 passes.
set -eu
report=docs/reviews/_date_time_review.md
[ -f "$report" ] || { echo "missing $report"; exit 1; }
changes=$(git status --porcelain --untracked-files=all | grep -v "^?? $report\$" || true)
if [ -n "$changes" ]; then
  echo "unexpected changes besides $report:"
  echo "$changes"
  exit 1
fi
