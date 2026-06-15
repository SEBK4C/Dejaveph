#!/usr/bin/env bash
# dejaveph-doctor — preflight checks for a Dejaveph client/server host.
#
# Verifies the things that actually break a deployment: FUSE prerequisites, the binaries, server
# reachability, and (for a tokens-mode server) that a bearer token is present. Exit code is the
# number of failed checks, so it composes in CI / systemd ExecStartPre.
#
#   scripts/dejaveph-doctor.sh --server http://dejaveph.home.arpa:9777 --volume models
set -uo pipefail

SERVER=""
VOLUME="default"
while [ $# -gt 0 ]; do
  case "$1" in
    --server) SERVER="$2"; shift 2 ;;
    --volume) VOLUME="$2"; shift 2 ;;
    -h|--help) sed -n '2,9p' "$0"; exit 0 ;;
    *) echo "unknown arg: $1" >&2; exit 2 ;;
  esac
done

fail=0
ok()   { printf '  \033[32m✓\033[0m %s\n' "$1"; }
bad()  { printf '  \033[31m✗\033[0m %s\n' "$1"; fail=$((fail+1)); }
warn() { printf '  \033[33m!\033[0m %s\n' "$1"; }

echo "FUSE prerequisites"
[ -c /dev/fuse ] && ok "/dev/fuse present" || bad "/dev/fuse missing (load the 'fuse' module / pass the device into the container)"
if command -v fusermount3 >/dev/null 2>&1 || command -v fusermount >/dev/null 2>&1; then
  ok "fusermount present ($(command -v fusermount3 || command -v fusermount))"
else
  bad "fusermount/fusermount3 not on PATH (install pkgs.fuse)"
fi

echo "Binaries"
for b in xetd xetfs; do
  if command -v "$b" >/dev/null 2>&1; then ok "$b ($(command -v $b))"; else warn "$b not on PATH (ok if this host runs only the other half)"; fi
done

if [ -n "$SERVER" ]; then
  echo "Server $SERVER"
  if ! command -v curl >/dev/null 2>&1; then
    warn "curl not found; skipping reachability check"
  else
    # The volume-entries endpoint is a cheap GET that proves the server is up and routing.
    auth=(); [ -n "${XETD_TOKEN:-}" ] && auth=(-H "Authorization: Bearer ${XETD_TOKEN}")
    code=$(curl -s -o /dev/null -w '%{http_code}' --max-time 5 "${auth[@]}" "${SERVER%/}/api/v1/volumes/${VOLUME}/entries" || echo 000)
    case "$code" in
      200) ok "reachable, volume '$VOLUME' listing OK (HTTP 200)" ;;
      401|403) bad "reachable but UNAUTHORIZED (HTTP $code) — set XETD_TOKEN for a tokens-mode server" ;;
      000) bad "unreachable (connection failed/timed out)" ;;
      *)   warn "responded HTTP $code (server up, unexpected status)" ;;
    esac
  fi
  [ -z "${XETD_TOKEN:-}" ] && warn "XETD_TOKEN not set (required for read-write or non-loopback tokens-mode servers)"
fi

echo
if [ "$fail" -eq 0 ]; then printf '\033[32mAll critical checks passed.\033[0m\n'; else printf '\033[31m%d critical check(s) failed.\033[0m\n' "$fail"; fi
exit "$fail"
