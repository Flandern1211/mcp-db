#!/usr/bin/env bash
# Mock regression test for deploy/run-deploy.sh (SSH forced command).
#
# No Docker, network, SSH or real host state is used. run-deploy.sh is copied
# into a temporary directory next to a stub deploy.sh that records the single
# argument it receives; a fake `docker` in PATH fails the test if any
# deployment path is reached for a rejected request.
set -euo pipefail

ROOT=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
SRC="$ROOT/deploy/run-deploy.sh"
HEX=$(printf '%064d' 0 | tr '0' 'a')
DIGEST="sha256:$HEX"
IMAGE="ghcr.io/flandern1211/mcp-db@$DIGEST"
VALID="deploy $IMAGE"

work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT

app="$work/app"
bin="$work/bin"
mkdir -p "$app" "$bin"
cp "$SRC" "$app/run-deploy.sh"

cat > "$app/deploy.sh" <<'STUB'
#!/usr/bin/env bash
printf '%s\n' "$#" "$@" > "$STUB_OUT"
STUB

cat > "$bin/docker" <<'STUB'
#!/usr/bin/env bash
printf 'docker %s\n' "$*" >> "$STUB_DOCKER_LOG"
exit 99
STUB

chmod +x "$app/deploy.sh" "$bin/docker"
export PATH="$bin:$PATH"
export STUB_OUT="$work/stub.out"
export STUB_DOCKER_LOG="$work/docker.log"

failures=0
pass() { printf 'PASS %s\n' "$1"; }
fail_case() { printf 'FAIL %s\n' "$1" >&2; failures=$((failures + 1)); }

reset() {
  : > "$STUB_OUT"
  : > "$STUB_DOCKER_LOG"
}

invoke() {
  reset
  set +e
  SSH_ORIGINAL_COMMAND=$1 bash "$app/run-deploy.sh" >"$work/stdout" 2>"$work/stderr"
  rc=$?
  set -e
}

invoke_unset() {
  reset
  set +e
  env -u SSH_ORIGINAL_COMMAND bash "$app/run-deploy.sh" >"$work/stdout" 2>"$work/stderr"
  rc=$?
  set -e
}

reject() {
  local desc=$1 request=$2
  invoke "$request"
  if [ "$rc" -eq 0 ]; then
    fail_case "$desc: request was accepted"
  elif [ -s "$STUB_OUT" ]; then
    fail_case "$desc: deploy.sh was invoked"
  elif [ -s "$STUB_DOCKER_LOG" ]; then
    fail_case "$desc: docker was invoked"
  else
    pass "$desc"
  fi
}

accept() {
  local desc=$1 request=$2 expected=${3:-$IMAGE}
  invoke "$request"
  if [ "$rc" -ne 0 ]; then
    fail_case "$desc: rc=$rc ($(cat "$work/stderr"))"
    return
  fi
  if [ "$(cat "$STUB_OUT")" != "$(printf '1\n%s' "$expected")" ]; then
    fail_case "$desc: stub got '$(cat "$STUB_OUT")'"
  elif [ -s "$STUB_DOCKER_LOG" ]; then
    fail_case "$desc: docker was invoked"
  else
    pass "$desc"
  fi
}

accept "exact digest accepted" "$VALID"
other_image="ghcr.io/flandern1211/mcp-db@sha256:$(printf '%064d' 1 | tr '0' '1')"
accept "exact digest with different hex accepted" "deploy $other_image" "$other_image"

reject "other owner rejected" "deploy ghcr.io/someone/mcp-db@$DIGEST"
reject "other repository rejected" "deploy ghcr.io/flandern1211/other@$DIGEST"
reject "path traversal rejected" "deploy ghcr.io/flandern1211/../mcp-db@$DIGEST"
reject "tag instead of digest rejected" "deploy ghcr.io/flandern1211/mcp-db:sha-$HEX"
reject "tag plus digest rejected" "deploy ghcr.io/flandern1211/mcp-db:latest@$DIGEST"
reject "extra argument rejected" "deploy $IMAGE extra"
reject "double space rejected" "deploy  $IMAGE"
reject "leading space rejected" " $VALID"
reject "trailing space rejected" "$VALID "
reject "tab separator rejected" $'deploy\t'"$IMAGE"
reject "trailing newline rejected" "$VALID"$'\n'
reject "carriage return rejected" "$VALID"$'\r'
reject "multiline rejected" "$VALID"$'\n'"$VALID"
reject "shell semicolon rejected" "$VALID; id"
reject "command substitution rejected" "$VALID \$(id)"
reject "backtick substitution rejected" "$VALID \`id\`"
reject "pipe rejected" "$VALID | id"
reject "uppercase hex rejected" "deploy ghcr.io/flandern1211/mcp-db@sha256:$(printf '%064d' 0 | tr '0' 'A')"
reject "short digest rejected" "deploy ghcr.io/flandern1211/mcp-db@sha256:${HEX:0:63}"
reject "long digest rejected" "deploy ghcr.io/flandern1211/mcp-db@sha256:${HEX}0"
reject "wrong prefix rejected" "run $IMAGE"
reject "empty request rejected" ""

invoke_unset
if [ "$rc" -eq 0 ]; then
  fail_case "unset SSH_ORIGINAL_COMMAND rejected: request was accepted"
elif [ -s "$STUB_OUT" ] || [ -s "$STUB_DOCKER_LOG" ]; then
  fail_case "unset SSH_ORIGINAL_COMMAND rejected: deployment path reached"
else
  pass "unset SSH_ORIGINAL_COMMAND rejected"
fi

if [ "$failures" -ne 0 ]; then
  printf '%d forced-command case(s) failed\n' "$failures" >&2
  exit 1
fi
printf 'forced-command: all cases passed\n'
