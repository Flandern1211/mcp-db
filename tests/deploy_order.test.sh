#!/usr/bin/env bash
# Mock regression test for deploy/deploy.sh ordering and failure handling.
#
# Docker, curl, flock, stat and timeout are replaced by stubs in a temporary
# PATH. The test asserts:
#   - pull happens before compose up and the live container is untouched when
#     pull fails;
#   - an unhealthy up is not retried and not rolled back;
#   - success requires exact loopback port mappings and 200/200 endpoints;
#   - invalid references, missing key/state and lock contention stop before any
#     Docker call.
set -euo pipefail

ROOT=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
HEX=$(printf '%064d' 0 | tr '0' 'a')
REF="ghcr.io/flandern1211/mcp-db@sha256:$HEX"
REAL_STAT=$(command -v stat)

work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT

app="$work/app"
bin="$work/bin"
mkdir -p "$app/keys" "$app/data" "$bin"
cp "$ROOT/deploy/deploy.sh" "$app/deploy.sh"
printf 'name: test\n' > "$app/docker-compose.yml"
printf '%064d' 0 | tr '0' 'a' > "$app/keys/master.key"

cat > "$bin/stat" <<STUB
#!/usr/bin/env bash
path=\${3:-}
case \$path in
  keys/master.key)
    [ "\${STUB_KEY_MISSING:-0}" = 1 ] && exit 1
    printf '400 10001 %s\n' "\${STUB_KEY_SIZE:-64}" ;;
  keys) printf '755\n' ;;
  data) printf '700 10001\n' ;;
  *) exec "$REAL_STAT" "\$@" ;;
esac
STUB

cat > "$bin/flock" <<'STUB'
#!/usr/bin/env bash
printf 'flock %s\n' "$*" >> "$STUB_LOG"
[ "${STUB_FLOCK_FAIL:-0}" = 1 ] && exit 1
exit 0
STUB

cat > "$bin/timeout" <<'STUB'
#!/usr/bin/env bash
if [ "${1:-}" = "-k" ]; then shift 2; fi
shift
exec "$@"
STUB

cat > "$bin/docker" <<'STUB'
#!/usr/bin/env bash
printf 'docker %s\n' "$*" >> "$STUB_LOG"
case "${1:-}" in
  compose)
    case " $* " in
      *" pull "*) [ "${STUB_PULL_FAIL:-0}" = 1 ] && exit 1 ;;
      *" up "*) [ "${STUB_UP_FAIL:-0}" = 1 ] && exit 1 ;;
    esac
    ;;
  image) exit 0 ;;
  port) printf '%s\n' "${STUB_PORT_OUT:-8787/tcp -> 127.0.0.1:8787
8788/tcp -> 127.0.0.1:8788}" ;;
  logs) exit 0 ;;
esac
exit 0
STUB

cat > "$bin/curl" <<'STUB'
#!/usr/bin/env bash
printf 'curl %s\n' "$*" >> "$STUB_LOG"
case "$*" in
  *"/healthz"*)
    [ "${STUB_HEALTH_RC:-0}" = 0 ] || exit "${STUB_HEALTH_RC}"
    printf '{"service":"mcp-db","status":"ok"}' ;;
  *"/admin/login"*) printf '%s' "${STUB_ADMIN_CODE:-200}" ;;
  *) exit 1 ;;
esac
STUB

chmod +x "$bin/stat" "$bin/flock" "$bin/timeout" "$bin/docker" "$bin/curl"
export PATH="$bin:$PATH"
export STUB_LOG="$work/calls.log"
export STUB_KEY_MISSING=0 STUB_KEY_SIZE=64 STUB_FLOCK_FAIL=0
export STUB_PULL_FAIL=0 STUB_UP_FAIL=0 STUB_HEALTH_RC=0 STUB_ADMIN_CODE=200
export STUB_PORT_OUT=$'8787/tcp -> 127.0.0.1:8787\n8788/tcp -> 127.0.0.1:8788'

failures=0
pass() { printf 'PASS %s\n' "$1"; }
fail_case() { printf 'FAIL %s\n' "$1" >&2; failures=$((failures + 1)); }

run_deploy() {
  : > "$STUB_LOG"
  set +e
  bash "$app/deploy.sh" "$@" >"$work/out" 2>"$work/err"
  rc=$?
  set -e
}

docker_calls() { grep -c '^docker ' "$STUB_LOG" || true; }
line_of() { grep -n "$1" "$STUB_LOG" | cut -d: -f1 | head -1 || true; }

# 1. Success: pull before up, then ports and both endpoints.
run_deploy "$REF"
if [ "$rc" -ne 0 ]; then
  fail_case "success path exited $rc: $(cat "$work/err")"
else
  pull_line=$(line_of ' pull mcp-db$')
  up_line=$(line_of ' up .*mcp-db$')
  if [ -z "$pull_line" ] || [ -z "$up_line" ]; then
    fail_case "success path did not call compose pull and up"
  elif [ "$pull_line" -ge "$up_line" ]; then
    fail_case "compose pull did not run before compose up"
  elif [ "$(grep -c ' up ' "$STUB_LOG" || true)" -ne 1 ]; then
    fail_case "success path called compose up more than once"
  elif ! grep -q '^curl .*healthz' "$STUB_LOG"; then
    fail_case "success path did not check /healthz"
  elif ! grep -q '^curl .*admin/login' "$STUB_LOG"; then
    fail_case "success path did not check /admin/login"
  elif grep -q '^docker logs' "$STUB_LOG"; then
    fail_case "success path printed container logs"
  else
    pass "success: pull then up, ports and endpoints checked"
  fi
fi

# 2. Pull failure: no up, no endpoint checks.
STUB_PULL_FAIL=1
run_deploy "$REF"
if [ "$rc" -eq 0 ]; then
  fail_case "pull failure exited 0"
elif grep -q ' up ' "$STUB_LOG"; then
  fail_case "pull failure still ran compose up"
elif grep -q '^curl ' "$STUB_LOG"; then
  fail_case "pull failure still checked endpoints"
else
  pass "pull failure: no up, no endpoint checks"
fi
STUB_PULL_FAIL=0

# 3. Unhealthy up: one attempt, no retry, no rollback, no logs.
STUB_UP_FAIL=1
run_deploy "$REF"
if [ "$rc" -eq 0 ]; then
  fail_case "failed up exited 0"
elif [ "$(grep -c ' up ' "$STUB_LOG" || true)" -ne 1 ]; then
  fail_case "failed up was retried or rolled back"
elif grep -q '^curl ' "$STUB_LOG"; then
  fail_case "failed up still checked endpoints"
elif grep -q '^docker logs' "$STUB_LOG"; then
  fail_case "failed up printed container logs"
else
  pass "failed up: one attempt, no retry, no rollback, no logs"
fi
STUB_UP_FAIL=0

# 4. Wrong port mapping rejects success after up.
STUB_PORT_OUT=$'8787/tcp -> 0.0.0.0:8787\n8788/tcp -> 127.0.0.1:8788'
run_deploy "$REF"
if [ "$rc" -eq 0 ]; then
  fail_case "non-loopback port mapping exited 0"
else
  pass "non-loopback port mapping rejected"
fi
STUB_PORT_OUT=$'8787/tcp -> 127.0.0.1:8787\n8788/tcp -> 127.0.0.1:8788'

# 5. Endpoint failures reject success.
STUB_ADMIN_CODE=500
run_deploy "$REF"
if [ "$rc" -eq 0 ]; then fail_case "admin/login 500 exited 0"; else pass "admin/login 500 rejected"; fi
STUB_ADMIN_CODE=200

STUB_HEALTH_RC=22
run_deploy "$REF"
if [ "$rc" -eq 0 ]; then fail_case "healthz failure exited 0"; else pass "healthz failure rejected"; fi
STUB_HEALTH_RC=0

# 6. Missing or empty key stops before Docker.
STUB_KEY_MISSING=1
run_deploy "$REF"
if [ "$rc" -eq 0 ]; then
  fail_case "missing key exited 0"
elif [ "$(docker_calls)" -ne 0 ]; then
  fail_case "missing key reached Docker"
else
  pass "missing key stops before Docker"
fi
STUB_KEY_MISSING=0

STUB_KEY_SIZE=0
run_deploy "$REF"
if [ "$rc" -eq 0 ]; then
  fail_case "empty key exited 0"
elif [ "$(docker_calls)" -ne 0 ]; then
  fail_case "empty key reached Docker"
else
  pass "empty key stops before Docker"
fi
STUB_KEY_SIZE=64

# 7. Lock contention stops before Docker.
STUB_FLOCK_FAIL=1
run_deploy "$REF"
if [ "$rc" -eq 0 ]; then
  fail_case "lock contention exited 0"
elif [ "$(docker_calls)" -ne 0 ]; then
  fail_case "lock contention reached Docker"
else
  pass "lock contention stops before Docker"
fi
STUB_FLOCK_FAIL=0

# 8. Invalid references and extra arguments stop before Docker.
run_deploy "ghcr.io/flandern1211/mcp-db:sha-$HEX"
if [ "$rc" -eq 0 ]; then fail_case "tag reference exited 0"; elif [ "$(docker_calls)" -ne 0 ]; then fail_case "tag reference reached Docker"; else pass "tag reference rejected before Docker"; fi

run_deploy "ghcr.io/someone/mcp-db@sha256:$HEX"
if [ "$rc" -eq 0 ]; then fail_case "other owner exited 0"; elif [ "$(docker_calls)" -ne 0 ]; then fail_case "other owner reached Docker"; else pass "other owner rejected before Docker"; fi

run_deploy "$REF" extra
if [ "$rc" -eq 0 ]; then fail_case "extra argument exited 0"; elif [ "$(docker_calls)" -ne 0 ]; then fail_case "extra argument reached Docker"; else pass "extra argument rejected before Docker"; fi

if [ "$failures" -ne 0 ]; then
  printf '%d deploy case(s) failed\n' "$failures" >&2
  exit 1
fi
printf 'deploy order: all cases passed\n'
