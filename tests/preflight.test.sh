#!/usr/bin/env bash
# Mock regression test for scripts/preflight.sh gate logic.
#
# Docker, git and the linters are replaced by PATH stubs. The test asserts the
# gate blocks exactly where the 2026-11-24 review found missing coverage:
#   - Rust files changed while Docker is unavailable must BLOCK (not reach CI);
#   - a shell script with a syntax error must BLOCK;
#   - a workflow with a bad action reference must BLOCK when actionlint exists;
#   - docs-only changes pass with no toolchain at all.
set -euo pipefail

ROOT=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT

bin="$work/bin"
app="$work/app"
mkdir -p "$bin" "$app/scripts"
# preflight resolves ROOT as its parent directory, so keep the same layout as
# the real repo: scripts/preflight.sh next to scripts/verify.sh.
cp "$ROOT/scripts/preflight.sh" "$app/scripts/preflight.sh"
cat > "$app/scripts/verify.sh" <<'STUB'
#!/usr/bin/env bash
printf 'verify %s\n' "$*" >> "$STUB_VERIFY_LOG"
exit "${STUB_VERIFY_RC:-0}"
STUB
chmod +x "$app/scripts/verify.sh"

# docker stub: controllable "daemon reachable" switch.
cat > "$bin/docker" <<'STUB'
#!/usr/bin/env bash
if [ "${STUB_DOCKER_UP:-0}" != 1 ]; then
  echo "failed to connect to the docker API" >&2
  exit 1
fi
exit 0
STUB
# actionlint stub: controllable verdict.
cat > "$bin/actionlint" <<'STUB'
#!/usr/bin/env bash
printf 'actionlint %s\n' "$*" >> "$STUB_AL_LOG"
if [ "${STUB_AL_RC:-0}" = 0 ]; then exit 0; fi
echo "workflow: bad action reference"
exit 1
STUB
# shellcheck stub: controllable verdict.
cat > "$bin/shellcheck" <<'STUB'
#!/usr/bin/env bash
printf 'shellcheck %s\n' "$*" >> "$STUB_SC_LOG"
if [ "${STUB_SC_RC:-0}" = 0 ]; then exit 0; fi
echo "SC2015 example"
exit 1
STUB
# git stub: never consulted when PREFLIGHT_CHANGED is set, but must exist so a
# mis-wired script fails loudly instead of silently passing.
cat > "$bin/git" <<'STUB'
#!/usr/bin/env bash
echo "git should not be called when PREFLIGHT_CHANGED is set: $*" >&2
exit 99
STUB
chmod +x "$bin/docker" "$bin/actionlint" "$bin/shellcheck" "$bin/git"

export PATH="$bin:$PATH"
export STUB_VERIFY_LOG="$work/verify.log"
export STUB_AL_LOG="$work/al.log"
export STUB_SC_LOG="$work/sc.log"

failures=0
pass() { printf 'PASS %s\n' "$1"; }
fail_case() { printf 'FAIL %s\n' "$1" >&2; failures=$((failures + 1)); }

run_preflight() {
  : > "$STUB_VERIFY_LOG"; : > "$STUB_AL_LOG"; : > "$STUB_SC_LOG"
  set +e
  ( cd "$app" && PREFLIGHT_CHANGED="$1" bash "$app/scripts/preflight.sh" ) \
    >"$work/out" 2>"$work/err"
  rc=$?
  set -e
}

# 1. Rust changed, Docker down -> BLOCK (the HeaderName-class failure).
export STUB_DOCKER_UP=0
run_preflight 'src/transport/admin.rs'
if [ "$rc" -eq 0 ]; then
  fail_case "rust change without docker was allowed"
elif [ -s "$STUB_VERIFY_LOG" ]; then
  fail_case "verify.sh ran despite docker being down"
else
  pass "rust change + no docker: blocked before CI"
fi

# 2. Rust changed, Docker up -> verify.sh runs and gates.
export STUB_DOCKER_UP=1
export STUB_VERIFY_RC=0
run_preflight 'src/lib.rs'
if [ "$rc" -ne 0 ]; then
  fail_case "rust change with passing verify failed: $(cat "$work/err")"
elif ! grep -q '^verify fast' "$STUB_VERIFY_LOG"; then
  fail_case "verify.sh fast was not invoked"
else
  pass "rust change + docker: verify.sh fast gates"
fi

export STUB_VERIFY_RC=1
run_preflight 'Cargo.toml'
if [ "$rc" -eq 0 ]; then
  fail_case "failing verify.sh did not block"
else
  pass "failing verify.sh blocks"
fi
export STUB_VERIFY_RC=0

# 3. Shell script with a syntax error -> BLOCK.
printf '#!/usr/bin/env bash\nif [ x\n' > "$app/broken.sh"
run_preflight 'broken.sh'
if [ "$rc" -eq 0 ]; then
  fail_case "broken shell script was allowed"
else
  pass "broken shell script: blocked"
fi
rm -f "$app/broken.sh"

# 4. Shell script clean, shellcheck flagging -> BLOCK; passing -> ok.
printf '#!/usr/bin/env bash\ntrue\n' > "$app/ok.sh"
export STUB_SC_RC=1
run_preflight 'ok.sh'
if [ "$rc" -eq 0 ]; then
  fail_case "shellcheck failure did not block"
else
  pass "shellcheck failure blocks"
fi
export STUB_SC_RC=0
run_preflight 'ok.sh'
if [ "$rc" -ne 0 ]; then
  fail_case "clean shell script rejected: $(cat "$work/err")"
else
  pass "clean shell script passes"
fi

# 5. Workflow change, actionlint flagging -> BLOCK.
export STUB_AL_RC=1
run_preflight '.github/workflows/ci.yml'
if [ "$rc" -eq 0 ]; then
  fail_case "actionlint failure did not block"
else
  pass "actionlint failure blocks"
fi
export STUB_AL_RC=0
run_preflight '.github/workflows/ci.yml'
if [ "$rc" -ne 0 ]; then
  fail_case "clean workflow rejected: $(cat "$work/err")"
else
  pass "clean workflow passes"
fi

# 6. Docs-only change needs no toolchain and never blocks.
export STUB_DOCKER_UP=0
run_preflight 'docs/deploy.md
TASK.md
CHANGELOG.md'
if [ "$rc" -ne 0 ]; then
  fail_case "docs-only change blocked: $(cat "$work/err")"
elif [ -s "$STUB_VERIFY_LOG" ]; then
  fail_case "docs-only change invoked verify.sh"
else
  pass "docs-only change passes without toolchain"
fi

if [ "$failures" -ne 0 ]; then
  printf '%d preflight case(s) failed\n' "$failures" >&2
  exit 1
fi
printf 'preflight: all cases passed\n'
