#!/usr/bin/env bash
# Daily cleanup of what working on this repository leaves behind.
#
# Runs once a day, on the first agent session of the day (see "Daily
# cleanup" in CLAUDE.md and AGENTS.md); `--force` runs it regardless of the
# stamp. It only touches paths this project owns:
#   1. cargo artifacts older than 7 days (cargo sweep) in the checkout's
#      target dir and in every live worktree's, never a full clean;
#   2. worktrees whose branch is merged into the default branch and that
#      hold no uncommitted or untracked change, no ignored file that may be
#      a person's work (see `regenerable` below) and no apb run that has
#      not ended (see `runs_ended` below), with their target dirs;
#   3. empty per-process test config dirs `apb-engine-unit-xdg-*` in the
#      temp dir that no process holds open;
#   4. `*.log` files of this project under the cache dir older than 7 days.
# Usage: scripts/daily-cleanup.sh [--force] [--dry-run]
set -euo pipefail

force=0
dry=0
for arg in "$@"; do
  case "$arg" in
    --force) force=1 ;;
    --dry-run) dry=1 ;;
    *) echo "usage: $0 [--force] [--dry-run]" >&2; exit 2 ;;
  esac
done

cache="${XDG_CACHE_HOME:-$HOME/.cache}"
stamp_dir="$cache/apb-cleanup"
stamp="$stamp_dir/last-run"
today="$(date +%F)"
if [ "$force" = 0 ] && [ -f "$stamp" ] && [ "$(cat "$stamp")" = "$today" ]; then
  echo "daily cleanup: already ran today"
  exit 0
fi

repo="$(git -C "$(dirname "$0")/.." rev-parse --show-toplevel)"
cd "$repo"

run() {
  if [ "$dry" = 1 ]; then echo "would: $*"; else "$@"; fi
}

# The default branch, as the remote names it; main when it cannot be read.
base="$(git symbolic-ref --quiet --short refs/remotes/origin/HEAD 2>/dev/null || echo origin/main)"
git fetch --quiet origin 2>/dev/null || true

# 2. Merged worktrees. A regular or fast-forward merge leaves the branch an
# ancestor of the base (a fast-forward leaves no commit of its own beyond
# the base, so ancestry alone decides), and the branch is only fresh work
# until something has been committed on it, which the branch's own reflog
# subjects tell apart. A squash merge leaves no ancestry, so a branch also
# counts as merged only when the base provably holds its work: some commit
# on the base carries its exact tree, or every non-merge commit's patch-id
# appears on the base (which survives a squash followed by a rebase once
# the base has moved on). Anything unreadable counts as unmerged.
#
# Branches are named by their full ref (refs/heads/<name>) so a tag of the
# same name can never decide, and the grep count reads its whole input, so
# pipefail cannot turn an early match into a false "keep".
merged() {
  local ref="$1"
  if git merge-base --is-ancestor "$ref" "$base" 2>/dev/null; then
    git reflog show --format=%gs "$ref" 2>/dev/null |
      grep -c '^commit' >/dev/null || return 1
    return 0
  fi
  local tip
  tip="$(git rev-parse --verify "$ref^{tree}" 2>/dev/null)" || return 1
  if git log --format=%T --max-count=500 "$base" 2>/dev/null |
    grep -cxe "$tip" >/dev/null; then
    return 0
  fi
  local ids base_ids id
  ids="$(commit_patch_ids "$ref")" || return 1
  [ -n "$ids" ] || return 1
  base_ids="$(commit_patch_ids "$base" --max-count=500)" || return 1
  [ -n "$base_ids" ] || return 1
  while IFS= read -r id; do
    [ -n "$id" ] || continue
    grep -cxe "$id" <<<"$base_ids" >/dev/null || return 1
  done <<<"$ids"
}

# The patch-id of every non-merge commit of a ref; empty commits print
# nothing and are skipped by the caller.
commit_patch_ids() {
  local c
  git log --no-merges --format=%H "$@" 2>/dev/null |
    while IFS= read -r c; do
      git diff-tree -p "$c" 2>/dev/null |
        git patch-id --stable 2>/dev/null |
        sed -n 's/^[0-9a-f]\{40\} //p'
    done
}

# `--porcelain -z` needs git 2.36 or newer; on an older git the command
# fails and every loop below would silently see no records, so say so.
listed=1
if ! git worktree list --porcelain -z >/dev/null 2>&1; then
  listed=0
  echo "warning: 'git worktree list --porcelain -z' failed (git too old?); worktree cleanup and sweep are skipped" >&2
fi

# One NUL-terminated record per attribute, so paths with spaces survive;
# a detached worktree has no branch record and is never touched.
worktree_branches() {
  local path rec
  { git worktree list --porcelain -z 2>/dev/null || true; } |
  while IFS= read -r -d '' rec; do
    case "$rec" in
      worktree\ *) path="${rec#worktree }" ;;
      branch\ *) printf '%s\t%s\n' "${rec#branch }" "$path" ;;
    esac
  done
}

# The main checkout (the first -z record, parsed the same way as the loop
# below) and the checkout running this script are never removed.
main_wt="$({ git worktree list --porcelain -z 2>/dev/null || true; } | {
  IFS= read -r -d '' first || true
  printf '%s' "${first#worktree }"
})"
# The ignored paths of a worktree that are output, safe to delete with
# it: build output and tool caches, and what apb itself writes under .apb/
# at run time (the list `apb_core::workspace::LOCAL_ENTRIES`): the
# workspace id (minted again on first use), run and scenario-run
# directories, the node cache, locks, the busy lock, the outside-run
# decision log, remembered run input, the known problems store, open
# launch challenges and launch handoff files. Run directories and the busy lock count only once
# `runs_ended` below found every run ended. Remembered run input and the
# known problems store are deliberately discarded with the worktree: they
# belong to that worktree's workspace, which nothing reads once it is gone
# (secret params are never stored). Anything else ignored keeps the
# worktree, in particular .apb/secrets.env, .apb/trash/ (soft-deleted
# playbooks), .apb/backup-*/ (migration backups), .ai-notes/ and any other
# notes; untracked files (a .env, .apb/policy.yaml) keep it through the
# status check before this one.
regenerable_paths=(
  'target' 'web/dist' '(.*/)?node_modules' '\.zvec-grep' '\.code-ranker'
  '(.*/)?\.DS_Store' 'docs/reviews/_date_time_review\.md'
  '\.apb/workspace\.local' '\.apb/runs' '\.apb/scenario-runs'
  '\.apb/cache' '\.apb/locks' '\.apb/workdir\.lock'
  '\.apb/decisions\.jsonl' '\.apb/memory' '\.apb/problems'
  '\.apb/launch-challenges' '\.apb/handoff'
)

# Whether every apb run of a worktree has ended, so its run directories are
# output rather than state. apb itself is not asked: every apb command but
# the read-only MCP server registers the directory as a workspace, and the
# installed apb may be older than the journals. The journals are read with
# jq instead; whatever cannot be read keeps the worktree.
#
# A run has ended when the last of its lifecycle events (started, resumed,
# queued, paused, finished, aborted) is `run_finished` or `run_aborted`, and
# every send it recorded is settled `confirmed` or `not_sent` (an `unknown`
# send waits for a person's `apb intent resolve`, which often lands after
# the run ended). A paused run (a pause waker may resume it), a run waiting
# on a gate, a queued or running one, a run directory without a journal and
# a journal that does not parse all count as not ended. The busy lock
# (`.apb/workdir.lock`) keeps the worktree while it names a live process or
# a paused run that still holds it; a pid no process holds any more is a
# crash leftover.
#
# A lock parked by a `--keep-lock` pause names `paused:<run id>`
# (`apb_engine::workdir`). It holds, as the engine decides it, only while
# that run's directory exists and the run has not ended: a run that
# finished or was aborted after its pause, or whose directory is gone, left
# a stale lock the next apb run takes, so it keeps nothing. A run id that is
# not a plain name, or a journal that does not parse, keeps the worktree.
journal_ended() {
  jq -e -s '
    ([.[] | select(.type | IN("run_started", "run_resumed", "run_queued",
        "run_paused", "run_finished", "run_aborted"))] | last | .type) as $last
    | (reduce (.[] | select(.type == "send_intent_settled")) as $e
        ({}; .[$e.intent] = $e.outcome)) as $settled
    | ($last == "run_finished" or $last == "run_aborted")
      and all(.[] | select(.type == "send_intent_recorded");
        $settled[.intent] == "confirmed" or $settled[.intent] == "not_sent")
  ' "$1" >/dev/null 2>&1
}

# Whether the last lifecycle event of a journal is `run_finished` or
# `run_aborted`, sends left aside: what ends the hold of a parked lock.
lifecycle_ended() {
  jq -e -s '
    [.[] | select(.type | IN("run_started", "run_resumed", "run_queued",
        "run_paused", "run_finished", "run_aborted"))] | last | .type
    | . == "run_finished" or . == "run_aborted"
  ' "$1" >/dev/null 2>&1
}

# Whether the paused run a parked lock of worktree $1 names ($2) still
# holds it; 0 holds, 1 stale. Anything unreadable holds.
paused_run_holds() {
  local wt="$1" id="$2" run
  case "$id" in
    ''|.|*/*|*\\*|*..*) return 0 ;;
  esac
  run="$wt/.apb/runs/$id"
  [ -d "$run" ] || return 1
  [ -f "$run/events.jsonl" ] || return 0
  command -v jq >/dev/null 2>&1 || return 0
  lifecycle_ended "$run/events.jsonl" && return 1
  return 0
}

runs_ended() {
  local wt="$1" d lock owner
  if [ -e "$wt/.apb/workdir.lock" ]; then
    owner="$(head -c 256 "$wt/.apb/workdir.lock" 2>/dev/null)" || return 2
    owner="$(printf '%s' "$owner" | tr -d '[:space:]')"
    case "$owner" in
      paused:*)
        paused_run_holds "$wt" "${owner#paused:}" && return 2
        ;;
      ''|*[!0-9]*) return 2 ;;
      *)
        kill -0 "$owner" 2>/dev/null && return 2
        [ -d "/proc/$owner" ] && return 2
        ;;
    esac
  fi
  [ -d "$wt/.apb/runs" ] || [ -d "$wt/.apb/scenario-runs" ] || return 0
  command -v jq >/dev/null 2>&1 || return 3
  for d in "$wt/.apb/runs"/*/ "$wt/.apb/scenario-runs"/*/; do
    [ -d "$d" ] || continue
    case "$d" in "$wt/.apb/scenario-runs/index/") continue ;; esac
    [ -f "$d/events.jsonl" ] || return 1
    journal_ended "$d/events.jsonl" || return 1
  done
  return 0
}
regenerable="^($(IFS='|'; echo "${regenerable_paths[*]}"))(/|$)"

while IFS=$'\t' read -r ref path; do
  [ "$path" = "$repo" ] && continue
  [ "$path" = "$main_wt" ] && continue
  branch="${ref#refs/heads/}"
  [ -d "$path" ] || continue
  if [ -n "$(git -C "$path" status --porcelain 2>/dev/null)" ]; then
    echo "keep $path: uncommitted or untracked changes"
    continue
  fi
  # Ignored files also block a removal, except what `regenerable` names
  # (git collapses a fully ignored directory to one "dir/" entry). Not
  # `grep -q`: an early exit can kill git with SIGPIPE, and under pipefail
  # that failure would read as "nothing to keep".
  # shellcheck disable=SC2143
  if [ -n "$(git -C "$path" status --porcelain --ignored=matching 2>/dev/null |
        sed -n 's/^!! //p' |
        grep -v -E "$regenerable")" ]; then
    echo "keep $path: ignored files that may be a person's work"
    continue
  fi
  if merged "$ref"; then
    rc=0
    runs_ended "$path" || rc=$?
    case "$rc" in
      1) echo "keep $path: a run that has not ended"; continue ;;
      2) echo "keep $path: the busy lock is held"; continue ;;
      3) echo "keep $path: holds apb runs and jq is not installed to read them"; continue ;;
    esac
    echo "remove merged worktree $path ($branch)"
    run git worktree remove "$path" || echo "keep $path: git refused to remove it"
  fi
done < <(worktree_branches)
# Only after a listing that worked: a prune would otherwise drop the
# records of a worktree whose path is only unavailable for now.
if [ "$listed" = 1 ]; then
  run git worktree prune || echo "worktree prune failed"
fi

# 1. Cargo artifacts older than a week, in every remaining target dir.
if command -v cargo-sweep >/dev/null 2>&1; then
  { git worktree list --porcelain -z 2>/dev/null || true; } |
  while IFS= read -r -d '' rec; do
    case "$rec" in worktree\ *) path="${rec#worktree }" ;; *) continue ;; esac
    [ -d "$path/target" ] || continue
    echo "sweep $path/target"
    run cargo sweep --time 7 "$path" >/dev/null || echo "sweep failed: $path"
  done
else
  echo "cargo-sweep is not installed (cargo install cargo-sweep); skipping the sweep"
fi

# 3. Leaked per-process test config dirs: empty and held by no process.
tmp="${TMPDIR:-/tmp}"
{ find "$tmp" -maxdepth 1 -type d -name 'apb-engine-unit-xdg-*' -empty -mmin +60 2>/dev/null || true; } |
while read -r d; do
  run rmdir "$d" || echo "tmp cleanup failed: $d"
done

# 4. This project's old logs in the cache dir.
{ find "$cache" -maxdepth 1 -type f -name 'apb-*.log' -mtime +7 2>/dev/null || true; } |
while read -r f; do
  run rm -f "$f" || echo "log cleanup failed: $f"
done

if [ "$dry" = 0 ]; then
  mkdir -p "$stamp_dir"
  echo "$today" > "$stamp"
fi
echo "daily cleanup: done"
