#!/usr/bin/env bash
# Packages and publishes apb's seven crates on crates.io.
#
# The repository keeps the CLI package named `apb`, so the release pipeline
# (dist) derives the same file names as before. This script is the one place
# that renames it to `agentic-playbooks`, in place, right before packaging,
# and puts the CLI manifest and the lockfile back on every exit.
#
# Usage (from anywhere in the repository):
#   scripts/crates.sh check                       pack and verify all seven crates,
#                                                 check them, write SHA256SUMS
#   scripts/crates.sh preflight <version> <sums>  decide what to publish, write publish-plan
#   scripts/crates.sh repack <version> <dir>      pack the planned crates again, compare
#   scripts/crates.sh publish <version> <dir>     publish the planned crates (needs the token)
#   scripts/crates.sh installed <version>         install the published version, check it
#
# Only `publish` runs with a registry token; every other command refuses one.
# The workflow .github/workflows/publish-crates.yml runs `check` and
# `preflight` in its job without a credential, `repack` and `publish` in the
# job that holds one, and `installed` last. CI runs `check` on every pull
# request.
set -euo pipefail

# The GitHub account that owns the repository; a crate that exists on
# crates.io counts as ours only when this user is among its owners.
OWNER_LOGIN="itechmeat"
USER_AGENT="agentic-playbooks-publish"
# A dependency order, CLI last (crates/apb-cli/tests/suite/crate_manifests_test.rs).
PUBLISH_ORDER="agentic-playbooks-decide agentic-playbooks-core agentic-playbooks-mcp-client agentic-playbooks-engine agentic-playbooks-mcp agentic-playbooks-server agentic-playbooks"
MAX_CRATE_BYTES=10000000

read -r -a ORDER <<<"${PUBLISH_ORDER}"

die() {
  echo "crates.sh: $*" >&2
  exit 1
}

usage() {
  sed -n '9,15p' "$0" | sed 's/^# \{0,1\}//' >&2
  exit 2
}

cmd="${1:-}"
[ -n "${cmd}" ] || usage
shift

# 1. Credentials, before anything else.
token_in_env() {
  [ -n "${CARGO_REGISTRY_TOKEN:-}" ] && return 0
  local name
  while read -r name; do
    case "${name}" in
      CARGO_REGISTRIES_*_TOKEN) [ -n "${!name:-}" ] && return 0 ;;
    esac
  done < <(compgen -e)
  return 1
}

case "${cmd}" in
  check | preflight | repack | installed)
    if token_in_env; then
      die "refusing to run ${cmd} with a registry token in the environment"
    fi
    ;;
  publish)
    [ -n "${CARGO_REGISTRY_TOKEN:-}" ] || die "refusing to run publish without CARGO_REGISTRY_TOKEN"
    ;;
  *) usage ;;
esac

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "${root}"

work="$(mktemp -d "${TMPDIR:-/tmp}/crates-sh.XXXXXX")"
renamed=0
cleanup() {
  if [ "${renamed}" = 1 ]; then
    git checkout -- crates/apb-cli/Cargo.toml Cargo.lock
  fi
  rm -rf "${work}"
}
trap cleanup EXIT

sha256() {
  if command -v sha256sum >/dev/null 2>&1; then
    sha256sum "$1" | cut -d' ' -f1
  else
    shasum -a 256 "$1" | cut -d' ' -f1
  fi
}

workspace_version() {
  python3 -c 'import sys, tomllib; print(tomllib.load(open(sys.argv[1], "rb"))["workspace"]["package"]["version"])' Cargo.toml
}

# `http_get <url> <file>` prints the HTTP status; the body goes to <file>.
# A transport failure stops the script.
http_get() {
  local code
  code="$(curl -sS -A "${USER_AGENT}" -o "$2" -w '%{http_code}' "$1")" || die "request failed: $1"
  printf '%s' "${code}"
}

# The sparse index path of a crate name (the index's prefix rules).
index_url() {
  local name="${1,,}" prefix
  case "${#name}" in
    1) prefix="1" ;;
    2) prefix="2" ;;
    3) prefix="3/${name:0:1}" ;;
    *) prefix="${name:0:2}/${name:2:2}" ;;
  esac
  printf 'https://index.crates.io/%s/%s' "${prefix}" "${name}"
}

# `index_cksum <name> <version>` prints the index checksum of that version,
# or nothing when the crate or the version is not in the index.
index_cksum() {
  local body="${work}/index-$1" code
  code="$(http_get "$(index_url "$1")" "${body}")"
  case "${code}" in
    404) return 0 ;;
    200) ;;
    *) die "the index answered ${code} for $1" ;;
  esac
  python3 - "${body}" "$2" <<'PY'
import json, sys
path, version = sys.argv[1], sys.argv[2]
for line in open(path, encoding="utf-8"):
    line = line.strip()
    if not line:
        continue
    entry = json.loads(line)
    if entry.get("vers") == version:
        print(entry.get("cksum", ""))
        break
PY
}

# `sums_of <sums file> <version>`: checks the file holds exactly the seven
# names at <version>, each with a lowercase SHA-256, and fills SUM_<n>.
declare -A SUM=()
read_sums() {
  local file="$1" version="$2" line sum crate count=0 name
  [ -f "${file}" ] || die "no sums file ${file}"
  while IFS= read -r line; do
    [[ "${line}" =~ ^([0-9a-f]{64})\ \ (.+)$ ]] || die "not a sums line: ${line}"
    sum="${BASH_REMATCH[1]}"
    crate="${BASH_REMATCH[2]}"
    local found=""
    for name in "${ORDER[@]}"; do
      if [ "${crate}" = "${name}-${version}.crate" ]; then
        found="${name}"
      fi
    done
    [ -n "${found}" ] || die "${file} names ${crate}, not one of the seven crates at ${version}"
    [ -z "${SUM[${found}]:-}" ] || die "${file} names ${found} twice"
    SUM["${found}"]="${sum}"
    count=$((count + 1))
  done <"${file}"
  [ "${count}" = 7 ] || die "${file} holds ${count} lines, not the seven crates at ${version}"
}

# `read_plan <dir> <version>`: checks publish-plan and SHA256SUMS of <dir>
# and fills ACTION[<name>].
declare -A ACTION=()
read_plan() {
  local dir="$1" version="$2" plan="$1/publish-plan" i=0 line
  [ -f "${plan}" ] || die "no publish plan in ${dir}"
  local -a lines=()
  mapfile -t lines <"${plan}"
  [ "${#lines[@]}" = 8 ] || die "${plan} must hold the version and seven package lines"
  [ "${lines[0]}" = "${version}" ] || die "${plan} names version ${lines[0]}, not ${version}"
  for i in 0 1 2 3 4 5 6; do
    line="${lines[$((i + 1))]}"
    case "${line}" in
      "${ORDER[$i]} publish") ACTION["${ORDER[$i]}"]=publish ;;
      "${ORDER[$i]} skip") ACTION["${ORDER[$i]}"]=skip ;;
      *) die "${plan} line $((i + 2)) must be '${ORDER[$i]} publish' or '${ORDER[$i]} skip', not '${line}'" ;;
    esac
  done
  read_sums "${dir}/SHA256SUMS" "${version}"
}

all_skipped() {
  local name
  for name in "${ORDER[@]}"; do
    [ "${ACTION[${name}]}" = skip ] || return 1
  done
}

# One `--exclude <name>` per skipped crate; never all seven.
EXCLUDE=()
exclude_skipped() {
  local name count=0
  EXCLUDE=()
  for name in "${ORDER[@]}"; do
    if [ "${ACTION[${name}]}" = skip ]; then
      EXCLUDE+=(--exclude "${name}")
      count=$((count + 1))
    fi
  done
  [ "${count}" -lt 7 ] || die "internal error: empty package selection"
}

# The shared preparation of check, repack and publish.
META="${work}/metadata.json"
TARGET_DIR=""
VERSION=""
prepare() {
  local config
  for config in .cargo/config .cargo/config.toml crates/*/.cargo/config crates/*/.cargo/config.toml; do
    [ ! -e "${config}" ] || die "refusing a checked-in cargo configuration: ${config}"
  done

  git diff --quiet HEAD -- crates/apb-cli/Cargo.toml Cargo.lock \
    || die "crates/apb-cli/Cargo.toml or Cargo.lock has uncommitted changes; commit or discard them first"
  renamed=1
  python3 - <<'PY' || die "the rename of the CLI package failed"
import sys

def rewrite(path, pick):
    with open(path, encoding="utf-8") as f:
        lines = f.read().split("\n")
    hits = pick(lines)
    if len(hits) != 1:
        sys.exit(f"{path}: expected exactly one line to rename, found {len(hits)}")
    new = list(lines)
    new[hits[0]] = 'name = "agentic-playbooks"'
    changed = sum(1 for a, b in zip(lines, new) if a != b)
    if changed != 1:
        sys.exit(f"{path}: expected exactly one changed line, got {changed}")
    with open(path, "w", encoding="utf-8") as f:
        f.write("\n".join(new))
    return new

def tables(lines):
    table = ""
    for i, line in enumerate(lines):
        stripped = line.strip()
        if stripped.startswith("["):
            table = stripped
        yield i, table, line

def package_name(lines):
    return [i for i, t, l in tables(lines) if t == "[package]" and l == 'name = "apb"']

manifest = rewrite("crates/apb-cli/Cargo.toml", package_name)
bins = [l for i, t, l in tables(manifest) if t == "[[bin]]" and l.startswith("name = ")]
if bins != ['name = "apb"']:
    sys.exit(f"crates/apb-cli/Cargo.toml: the [[bin]] table must read name = \"apb\", found {bins}")

def lock_entry(lines):
    return [i for i, l in enumerate(lines) if l == 'name = "apb"' and i > 0 and lines[i - 1] == "[[package]]"]

rewrite("Cargo.lock", lock_entry)
PY
  cargo metadata --locked --format-version 1 >"${META}" \
    || die "cargo would change Cargo.lock after the rename; nothing was packaged"

  VERSION="$(workspace_version)"
  TARGET_DIR="$(python3 - "${META}" "${VERSION}" <<'PY'
import json, sys
meta = json.load(open(sys.argv[1], encoding="utf-8"))
version = sys.argv[2]
members = set(meta.get("workspace_members", []))
packages = [p for p in meta.get("packages", []) if p["id"] in members]
names = {p["name"] for p in packages}
problems = []
for p in packages:
    for d in p.get("dependencies", []):
        source = d.get("source") or ""
        if source.startswith("git+"):
            problems.append(f"{p['name']} depends on {d['name']} from git ({source})")
        if d["name"] in names and d.get("kind") in (None, "build") and d.get("req") != f"={version}":
            problems.append(f"{p['name']} requires {d['name']} {d.get('req')}, not ={version}")
if problems:
    sys.exit("\n".join(problems))
print(meta["target_directory"])
PY
)" || die "the workspace dependencies are not publishable"

  [ -f crates/apb-server/dashboard/index.html ] \
    || die "the dashboard is not built: run (cd web && bun install --frozen-lockfile && bun run build) from the repository root"
}

crate_file() {
  printf '%s/package/%s-%s.crate' "${TARGET_DIR}" "$1" "$2"
}

# `contains <list file> <path>` fails naming the crate when <path> is missing.
require_path() {
  grep -qxF -- "$3" "$2" || die "$1: the package does not hold $3"
}

cmd_check() {
  [ "$#" = 0 ] || usage
  prepare
  cargo package --workspace --locked --allow-dirty
  local name file list base size sums="${TARGET_DIR}/package/SHA256SUMS"
  for name in "${ORDER[@]}"; do
    file="$(crate_file "${name}" "${VERSION}")"
    [ -f "${file}" ] || die "${name}: no package at ${file}"
    list="${work}/list-${name}"
    tar -tzf "${file}" >"${list}"
    base="${name}-${VERSION}"
    require_path "${name}" "${list}" "${base}/LICENSE"
    case "${name}" in
      agentic-playbooks-server)
        require_path "${name}" "${list}" "${base}/dashboard/index.html"
        grep -q "^${base}/dashboard/assets/." "${list}" || die "${name}: the package holds no file under dashboard/assets/"
        ;;
      agentic-playbooks-core)
        local tracked
        while IFS= read -r tracked; do
          case "${tracked}" in
            bundled/*) require_path "${name}" "${list}" "${base}/embedded/${tracked}" ;;
            connectors/*) require_path "${name}" "${list}" "${base}/embedded/${tracked}" ;;
            examples/playbooks/*) require_path "${name}" "${list}" "${base}/embedded/templates/${tracked#examples/playbooks/}" ;;
          esac
        done < <(git ls-files bundled connectors examples/playbooks)
        require_path "${name}" "${list}" "${base}/embedded/models.yaml"
        require_path "${name}" "${list}" "${base}/embedded/near-ai-cloud.json"
        ;;
      agentic-playbooks-mcp)
        local doc
        for doc in HOWTO-authoring.md HOWTO-interview.md AGENT-SETUP.md playbook-instructions.md; do
          require_path "${name}" "${list}" "${base}/embedded/${doc}"
        done
        ;;
      agentic-playbooks)
        require_path "${name}" "${list}" "${base}/README.md"
        ;;
    esac
    size="$(wc -c <"${file}" | tr -d ' ')"
    echo "${name}-${VERSION}.crate: ${size} bytes"
    [ "${size}" -le "${MAX_CRATE_BYTES}" ] || die "${name}: the package is ${size} bytes, over the ${MAX_CRATE_BYTES} byte limit of crates.io"
  done
  : >"${sums}"
  for name in "${ORDER[@]}"; do
    file="$(crate_file "${name}" "${VERSION}")"
    printf '%s  %s\n' "$(sha256 "${file}")" "${name}-${VERSION}.crate" >>"${sums}"
  done
  echo "wrote ${sums}"
}

cmd_preflight() {
  [ "$#" = 2 ] || usage
  local version="$1" sums="$2" ws name code body answered owners plan cksum all_skip=1
  ws="$(workspace_version)"
  [ "${version}" = "${ws}" ] || die "version ${version} is not the workspace version ${ws}"
  read_sums "${sums}" "${version}"
  plan="$(dirname "${sums}")/publish-plan"
  local text="${version}"$'\n'
  for name in "${ORDER[@]}"; do
    body="${work}/crate-${name}"
    code="$(http_get "https://crates.io/api/v1/crates/${name}" "${body}")"
    case "${code}" in
      404) ;;
      200)
        answered="$(python3 -c 'import json, sys; print(json.load(open(sys.argv[1]))["crate"]["name"])' "${body}")"
        code="$(http_get "https://crates.io/api/v1/crates/${name}/owners" "${body}.owners")"
        [ "${code}" = 200 ] || die "crates.io answered ${code} for the owners of ${name}"
        owners="$(python3 -c 'import json, sys; print(" ".join(u["login"] + ":" + u["kind"] for u in json.load(open(sys.argv[1]))["users"]))' "${body}.owners")"
        if [ "${answered}" != "${name}" ] || [[ " ${owners} " != *" ${OWNER_LOGIN}:user "* ]]; then
          die "crate ${name} exists on crates.io and is not ours (name ${answered}, owners ${owners})"
        fi
        ;;
      *) die "crates.io answered ${code} for ${name}" ;;
    esac
    cksum="$(index_cksum "${name}" "${version}")"
    if [ -z "${cksum}" ]; then
      text+="${name} publish"$'\n'
      all_skip=0
    elif [ "${cksum}" = "${SUM[${name}]}" ]; then
      text+="${name} skip"$'\n'
    else
      die "${name} ${version} is already on crates.io with checksum ${cksum}, but the verified package has ${SUM[${name}]}; nothing was published"
    fi
  done
  printf '%s' "${text}" >"${plan}"
  echo "wrote ${plan}"
  if [ -n "${GITHUB_OUTPUT:-}" ]; then
    if [ "${all_skip}" = 1 ]; then
      echo "nothing_to_publish=true" >>"${GITHUB_OUTPUT}"
    else
      echo "nothing_to_publish=false" >>"${GITHUB_OUTPUT}"
    fi
  fi
  if [ "${all_skip}" = 1 ]; then
    echo "all seven crates are on crates.io with the verified checksums"
  fi
}

cmd_repack() {
  [ "$#" = 2 ] || usage
  local version="$1" dir="$2" name got
  read_plan "${dir}" "${version}"
  if all_skipped; then
    echo "nothing to repack: all seven crates are on crates.io with the verified checksums"
    return 0
  fi
  prepare
  [ "${VERSION}" = "${version}" ] || die "version ${version} is not the workspace version ${VERSION}"
  exclude_skipped
  cargo package --workspace --no-verify --locked --allow-dirty "${EXCLUDE[@]}"
  for name in "${ORDER[@]}"; do
    [ "${ACTION[${name}]}" = publish ] || continue
    got="$(sha256 "$(crate_file "${name}" "${version}")")"
    [ "${got}" = "${SUM[${name}]}" ] \
      || die "${name}: the package built here has ${got}, the verified one ${SUM[${name}]}; nothing was published"
    echo "${name}-${version}.crate: ${got} equals the verified checksum"
  done
}

cmd_publish() {
  [ "$#" = 2 ] || usage
  local version="$1" dir="$2" name cksum tries
  read_plan "${dir}" "${version}"
  if ! all_skipped; then
    prepare
    [ "${VERSION}" = "${version}" ] || die "version ${version} is not the workspace version ${VERSION}"
    for name in "${ORDER[@]}"; do
      [ "${ACTION[${name}]}" = publish ] || continue
      cksum="$(index_cksum "${name}" "${version}")"
      [ -z "${cksum}" ] \
        || die "${name} ${version} appeared on crates.io after the plan was made; nothing was published; rerun all jobs"
    done
    exclude_skipped
    if ! cargo publish --workspace --no-verify --locked --allow-dirty "${EXCLUDE[@]}"; then
      echo "publishing ${version} stopped; rerun all jobs of the publish-crates run later: crates already published with the verified checksums are skipped" >&2
      exit 1
    fi
  fi
  for name in "${ORDER[@]}"; do
    tries=0
    while :; do
      cksum="$(index_cksum "${name}" "${version}")"
      [ -n "${cksum}" ] && break
      tries=$((tries + 1))
      [ "${tries}" -le 30 ] || die "${name} ${version} is not in the index after 5 minutes"
      sleep 10
    done
    [ "${cksum}" = "${SUM[${name}]}" ] \
      || die "${name} ${version} is on crates.io with checksum ${cksum}, but the verified package has ${SUM[${name}]}"
    echo "${name} ${version}: the index checksum equals the verified one"
  done
}

cmd_installed() {
  [ "$#" = 1 ] || usage
  local version="$1" base home dest printed
  base="${RUNNER_TEMP:-${TMPDIR:-/tmp}}"
  home="$(mktemp -d "${base}/crates-installed-home.XXXXXX")"
  dest="$(mktemp -d "${base}/crates-installed-root.XXXXXX")"
  trap 'rm -rf "${home}" "${dest}"; cleanup' EXIT
  CARGO_HOME="${home}" cargo install --locked agentic-playbooks --version "${version}" --root "${dest}" \
    || die "cargo install --locked agentic-playbooks --version ${version} failed: the published release does not install"
  printed="$("${dest}/bin/apb" --version)" || die "the installed apb did not run"
  [ "${printed}" = "apb ${version}" ] || die "the installed apb prints '${printed}', not 'apb ${version}'"
  echo "installed agentic-playbooks ${version}: ${printed}"
}

case "${cmd}" in
  check) cmd_check "$@" ;;
  preflight) cmd_preflight "$@" ;;
  repack) cmd_repack "$@" ;;
  publish) cmd_publish "$@" ;;
  installed) cmd_installed "$@" ;;
esac
