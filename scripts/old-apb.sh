#!/usr/bin/env bash
# Builds the `apb` binary of a released tag for the journal corpus
# (crates/apb-cli/tests/suite/journal_corpus_test.rs).
#
#   scripts/old-apb.sh <tag> <out_dir>
#
# The binary lands at <out_dir>/<tag>/apb; point APB_OLD_BINARIES at
# <out_dir> to run the corpus against it. The build happens in a detached
# worktree of the tag (web build first, since apb-server embeds web/dist,
# then `cargo build --release --locked -p apb`), with its own target dir
# under <out_dir>/.build. Idempotent: a binary already in place that reports
# the tag's version is kept and nothing is built. The worktree and the build
# dir are removed after a successful build, so <out_dir> holds only binaries
# and can be cached as it is.
#
# Needs git, cargo and bun on PATH, and the tag in the local repository (it
# is fetched from `origin` when missing).

set -euo pipefail

usage() {
  echo "usage: $0 <tag> <out_dir>" >&2
  exit 2
}

[ $# -eq 2 ] || usage
tag=$1
out=$2

if ! [[ $tag =~ ^v[0-9]+\.[0-9]+\.[0-9]+(-[0-9A-Za-z.]+)?$ ]]; then
  echo "old-apb: not a release tag: $tag" >&2
  exit 2
fi
version=${tag#v}

mkdir -p "$out"
out=$(cd "$out" && pwd)
dest="$out/$tag/apb"

reports_version() {
  [ -x "$1" ] && [ "$("$1" --version 2>/dev/null)" = "apb $version" ]
}

if reports_version "$dest"; then
  echo "old-apb: $dest is already apb $version"
  exit 0
fi

repo=$(git -C "$(dirname "$0")" rev-parse --show-toplevel)
if ! git -C "$repo" rev-parse -q --verify "refs/tags/$tag^{commit}" >/dev/null; then
  git -C "$repo" fetch --no-tags origin "refs/tags/$tag:refs/tags/$tag"
fi

src="$out/.build/$tag/src"
target="$out/.build/$tag/target"
# A worktree left by an interrupted build is replaced, never reused: it may
# hold a half-installed web/node_modules.
if [ -e "$src" ]; then
  git -C "$repo" worktree remove --force "$src" 2>/dev/null || rm -rf "$src"
fi
git -C "$repo" worktree prune
mkdir -p "$(dirname "$src")"
git -C "$repo" worktree add --detach "$src" "refs/tags/$tag"

(
  cd "$src/web"
  bun install --frozen-lockfile
  bun run build
)
# From inside the tag's worktree, so its own rust-toolchain.toml and
# .cargo/config.toml apply wherever the script is started.
(
  cd "$src"
  CARGO_TARGET_DIR="$target" cargo build --release --locked -p apb
)

mkdir -p "$out/$tag"
install -m 0755 "$target/release/apb" "$dest.tmp"
mv -f "$dest.tmp" "$dest"
if ! reports_version "$dest"; then
  echo "old-apb: $dest does not report apb $version" >&2
  exit 1
fi

git -C "$repo" worktree remove --force "$src"
rm -rf "$out/.build/$tag"
rmdir "$out/.build" 2>/dev/null || true
echo "old-apb: built $dest"
