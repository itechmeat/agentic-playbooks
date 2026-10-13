#!/usr/bin/env bash
# Installs the pinned cargo-about release binary (x86_64 Linux, musl) for the
# third-party notices check in CI. The download is verified against the
# SHA-256 below before it is installed; bump both together.
#
# Usage: scripts/install-cargo-about.sh [install-dir]   (default ~/.cargo/bin)
set -euo pipefail

version="0.9.2"
sha256="9099a59e820c38a68b9d65f300662a567d56562f9a10f6aa4c7e86c17c2566af"
target="x86_64-unknown-linux-musl"
dest="${1:-$HOME/.cargo/bin}"

if command -v cargo-about >/dev/null && [ "$(cargo-about --version)" = "cargo-about $version" ]; then
  echo "cargo-about $version already installed"
  exit 0
fi

work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT
name="cargo-about-$version-$target"
curl --proto '=https' --tlsv1.2 -fsSL -o "$work/$name.tar.gz" \
  "https://github.com/EmbarkStudios/cargo-about/releases/download/$version/$name.tar.gz"
echo "$sha256  $work/$name.tar.gz" | sha256sum -c --quiet -
tar -xzf "$work/$name.tar.gz" -C "$work"
mkdir -p "$dest"
install -m 0755 "$work/$name/cargo-about" "$dest/cargo-about"
"$dest/cargo-about" --version
