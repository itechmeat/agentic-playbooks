#!/usr/bin/env bash
# Daily cleanup of what working on this repository leaves behind.
#
# Runs once a day, on the first agent session of the day (see "Daily
# cleanup" in CLAUDE.md and AGENTS.md); `--force` runs it regardless of the
# stamp. It only touches paths this project owns:
#   1. cargo artifacts older than 7 days (cargo sweep) in the checkout's
#      target dir and in every live worktree's, never a full clean;
#   2. worktrees whose branch is merged into the default branch and that
#      hold no uncommitted tracked change, with their target dirs;
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
if ! git worktree list --porcelain -z >/dev/null 2>&1; then
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
main_wt="$(git worktree list --porcelain -z 2>/dev/null | {
  IFS= read -r -d '' first || true
  printf '%s' "${first#worktree }"
})"
while IFS=$'\t' read -r ref path; do
  [ "$path" = "$repo" ] && continue
  [ "$path" = "$main_wt" ] && continue
  branch="${ref#refs/heads/}"
  [ -d "$path" ] || continue
  if [ -n "$(git -C "$path" status --porcelain 2>/dev/null)" ]; then
    echo "keep $path: uncommitted or untracked changes"
    continue
  fi
  # Ignored files also block a removal, except the build output and caches
  # this project regenerates (git collapses a fully ignored directory to
  # one "dir/" entry). Everything under .apb/ blocks: secrets.env,
  # workspace.local, runs/, trash/ and backup-*/ are state, not output.
  if [ -n "$(git -C "$path" status --porcelain --ignored=matching 2>/dev/null |
        sed -n 's/^!! //p' |
        grep -v -E '^(target|web/node_modules|web/dist|\.zvec-grep)(/|$)')" ]; then
    echo "keep $path: ignored files that are not build output"
    continue
  fi
  if merged "$ref"; then
    echo "remove merged worktree $path ($branch)"
    run git worktree remove "$path" || echo "keep $path: git refused to remove it"
  fi
done < <(worktree_branches)
run git worktree prune || echo "worktree prune failed"

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
