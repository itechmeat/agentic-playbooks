#!/bin/sh
# A `git` for publish_cli_test, first on PATH: it logs every call to
# $FAKE_GIT_LOG and forwards to the real git ($FAKE_GIT_REAL, an absolute
# path), never reading the system config. For a push, $FAKE_GIT_PUSH picks
# a failure the publish step must survive:
#   hang        sleep instead of pushing (the remote never changes)
#   hang_after  push for real, then sleep (the remote changed, no answer)
# The step's own timeout kills the process group either way.
printf '%s\n' "$*" >> "$FAKE_GIT_LOG"
GIT_CONFIG_NOSYSTEM=1
export GIT_CONFIG_NOSYSTEM
is_push=no
for arg in "$@"; do
  if [ "$arg" = "push" ]; then
    is_push=yes
    break
  fi
done
if [ "$is_push" = yes ]; then
  case "$FAKE_GIT_PUSH" in
    hang)
      exec sleep 60
      ;;
    hang_after)
      "$FAKE_GIT_REAL" "$@"
      exec sleep 60
      ;;
  esac
fi
exec "$FAKE_GIT_REAL" "$@"
