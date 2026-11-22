#!/usr/bin/env bash
# Unified verification entry for this repository.
#
# Usage: scripts/verify.sh [fmt|clippy|test|build|fast|full|real|--help]
#   fmt     cargo fmt --check
#   clippy  cargo clippy --locked --all-targets --all-features -- -D warnings
#   test    cargo test --locked
#   build   cargo build --release --locked
#   fast    fmt + clippy + test (default)
#   full    fast + release build + docker build
#   real    isolated PostgreSQL integration tests (scripts/pgtest.sh)
#
# All Rust steps run in one pinned container per invocation; the host toolchain
# is never used. Each worktree gets its own target volume so parallel tasks do
# not share build locks or cross-branch artifacts.
#
# MCPDB_STEP_TIMEOUT (default 900s) applies separately to each Rust step and
# to docker build. A timeout (exit 124) prints "TIMEOUT <step>" and stops the
# run; exit 137 is reported separately as SIGKILL.
# MCPDB_GROUP_TIMEOUT (default 1800s) covers docker run, including setup, and
# can expire before a Rust step's own limit. MCPDB_TIMEOUT is the fallback
# alias when MCPDB_GROUP_TIMEOUT is unset. Defaults apply only to unset limits.
# Use a finite positive duration, e.g. 900, 0.5s or 15m (seconds if no suffix;
# suffixes s/m/h/d). Explicitly empty values and zero are rejected; GNU timeout
# treats zero as unlimited.
# Timeout sends TERM first and may escalate to SIGKILL after 10 seconds.
#
# Rust containers use --rm --init; on failure the script only reports a
# remaining container if it matches this invocation's label and name.
#
# Other overrides: MCPDB_RUST_IMAGE, MCPDB_RUST_TOOLCHAIN,
# MCPDB_CARGO_REGISTRY_VOL, MCPDB_CARGO_TARGET_VOL.

set -euo pipefail
export MSYS_NO_PATHCONV=1

MODE=${1:-fast}

usage() {
  awk '/^# Usage:/ { help = 1 }
       help && !/^#/ { exit }
       help { sub(/^# ?/, ""); print }' "${BASH_SOURCE[0]}"
}

case "$MODE" in
  -h|--help|help) usage; exit 0 ;;
esac

ROOT=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
ROOT_WIN=$(cd "$ROOT" && pwd -W 2>/dev/null || cygpath -w "$ROOT")
HASH=$(printf '%s' "$ROOT" | cksum | awk '{print $1}')
IMAGE=${MCPDB_RUST_IMAGE:-rust:1.89-slim-bookworm}
TOOLCHAIN=${MCPDB_RUST_TOOLCHAIN:-1.89.0}
REGISTRY_VOL=${MCPDB_CARGO_REGISTRY_VOL:-mcpdb-cargo-registry}
TARGET_VOL=${MCPDB_CARGO_TARGET_VOL:-mcpdb-cargo-target-$HASH}
KILL_AFTER=10

validated_duration() {
  local name=$1 value=$2
  local format='^[+]?([0-9]+(\.[0-9]*)?|\.[0-9]+)([eE][+-]?[0-9]+)?[smhd]?$'
  # GNU timeout treats zero (including numeric underflow to zero) as unlimited.
  if [[ ! $value =~ $format ]] || ! LC_ALL=C awk -v d="$value" 'BEGIN {
    n = d + 0
    unit = substr(d, length(d))
    if (unit == "m") n *= 60
    if (unit == "h") n *= 3600
    if (unit == "d") n *= 86400
    exit !(n > 0 && n - n == 0)
  }'; then
    printf 'verify: %s=%q must be a finite positive duration (e.g. 900, 0.5s or 15m); zero disables the timeout\n' "$name" "$value" >&2
    return 2
  fi
  case "$value" in
    *[smhd]) printf '%s' "$value" ;;
    *) printf '%ss' "$value" ;;
  esac
}

# --rm handles normal exit. If the client dies first, only report a container
# matching this invocation; an existing container's name alone proves nothing.
report_residue() {
  local name=$1 names candidate
  if ! names=$(timeout -k 2s 5s docker ps -a \
    --filter "label=mcpdb.verify.run=$name" \
    --format '{{.Names}}' 2>/dev/null); then
    printf 'WARN: could not check whether container %s remains.\n' "$name" >&2
    return 0
  fi
  while IFS= read -r candidate; do
    if [[ $candidate == "$name" ]]; then
      printf 'WARN: container %s for this invocation still exists; left untouched.\n' "$name" >&2
      break
    fi
  done <<< "$names"
}

require_docker() {
  command -v docker >/dev/null 2>&1 || {
    echo "verify: docker is required" >&2
    exit 2
  }
  command -v timeout >/dev/null 2>&1 || {
    echo "verify: timeout (coreutils) is required" >&2
    exit 2
  }
  docker info >/dev/null 2>&1 || {
    echo "verify: docker daemon is not reachable" >&2
    echo "hint: check Docker Desktop; on Windows also check WSL networking (networkingMode=mirrored in %USERPROFILE%\\.wslconfig), then 'wsl --shutdown' and restart Docker." >&2
    exit 2
  }
}

run_group() {
  local label=$1
  local body=$2
  local name="mcpdb-verify-$HASH-$$-$RANDOM-$label"
  local start=$SECONDS
  local rc=0
  printf '\n== %s ==\n' "$label"
  # The attached client proxies TERM to --init. If the client dies before
  # forwarding it, --rm waits for container exit; the label scopes reporting.
  timeout -k "$KILL_AFTER" "$GROUP_TIMEOUT" docker run --rm --init --name "$name" \
    --label mcpdb.verify.run="$name" \
    -e RUSTUP_TOOLCHAIN="$TOOLCHAIN" \
    -e MCPDB_STEP_TIMEOUT="$STEP_TIMEOUT" \
    -v "$ROOT_WIN:/work" \
    -v "$REGISTRY_VOL:/usr/local/cargo/registry" \
    -v "$TARGET_VOL:/work/target" \
    -w /work "$IMAGE" \
    sh -c "set -e
step() {
  n=\$1; shift
  echo \"== \$n ==\"
  s=\$(date +%s)
  rc=0
  timeout -k $KILL_AFTER \"\$MCPDB_STEP_TIMEOUT\" \"\$@\" || rc=\$?
  el=\$(( \$(date +%s) - s ))
  if [ \"\$rc\" -eq 124 ]; then
    echo \"TIMEOUT \$n (limit \$MCPDB_STEP_TIMEOUT, \${el}s)\"
    exit 1
  fi
  if [ \"\$rc\" -eq 137 ]; then
    echo \"KILLED \$n (SIGKILL, possibly OOM or timeout escalation; \${el}s)\"
    exit 1
  fi
  if [ \"\$rc\" -ne 0 ]; then
    echo \"FAILED \$n (exit \$rc, \${el}s)\"
    exit 1
  fi
  echo \"OK \$n (\${el}s)\"
}
rustup component add rustfmt clippy >/dev/null 2>&1 || true
$body" || rc=$?
  if [ "$rc" -ne 0 ]; then
    if [ "$rc" -eq 124 ]; then
      printf 'FAILED %s (group timeout %s)\n' "$label" "$GROUP_TIMEOUT"
    elif [ "$rc" -eq 137 ]; then
      printf 'KILLED %s (SIGKILL, possibly OOM or timeout escalation; %ss)\n' "$label" "$((SECONDS - start))"
    else
      printf 'FAILED %s (%ss)\n' "$label" "$((SECONDS - start))"
    fi
    report_residue "$name"
    exit 1
  fi
  printf 'OK %s (%ss)\n' "$label" "$((SECONDS - start))"
}

FAST_BODY='step fmt cargo fmt --check
step clippy cargo clippy --locked --all-targets --all-features -- -D warnings
step test cargo test --locked'

FULL_BODY="$FAST_BODY
step build cargo build --release --locked"

# Resolve and validate before any Docker call. An explicitly empty variable is
# an error, not "unset": fail closed instead of silently falling back to a
# default, so a typo cannot weaken the gate. Only a truly unset variable uses
# the default. The delegated PostgreSQL checks do not use these limits.
if [[ $MODE != real ]]; then
  if [[ -n ${MCPDB_STEP_TIMEOUT+x} ]]; then
    STEP_TIMEOUT=$(validated_duration MCPDB_STEP_TIMEOUT "$MCPDB_STEP_TIMEOUT") || exit 2
  else
    STEP_TIMEOUT=900s
  fi
  if [[ -n ${MCPDB_GROUP_TIMEOUT+x} ]]; then
    GROUP_TIMEOUT=$(validated_duration MCPDB_GROUP_TIMEOUT "$MCPDB_GROUP_TIMEOUT") || exit 2
  elif [[ -n ${MCPDB_TIMEOUT+x} ]]; then
    GROUP_TIMEOUT=$(validated_duration MCPDB_TIMEOUT "$MCPDB_TIMEOUT") || exit 2
  else
    GROUP_TIMEOUT=1800s
  fi
fi

require_docker

# Host disk guard: fail early instead of filling the drive during a long build.
if [ -f "$ROOT/scripts/disk-guard.sh" ]; then
  bash "$ROOT/scripts/disk-guard.sh" --check
fi

case "$MODE" in
  fmt) run_group fmt 'step fmt cargo fmt --check' ;;
  clippy) run_group clippy 'step clippy cargo clippy --locked --all-targets --all-features -- -D warnings' ;;
  test) run_group test 'step test cargo test --locked' ;;
  build) run_group build 'step build cargo build --release --locked' ;;
  fast) run_group fast "$FAST_BODY" ;;
  full)
    run_group full "$FULL_BODY"
    printf '\n== docker build ==\n'
    build_start=$SECONDS
    build_rc=0
    timeout -k "$KILL_AFTER" "$STEP_TIMEOUT" docker build -t mcp-db:dev "$ROOT_WIN" || build_rc=$?
    if [ "$build_rc" -ne 0 ]; then
      if [ "$build_rc" -eq 124 ]; then
        printf 'TIMEOUT docker build (limit %s, %ss)\n' "$STEP_TIMEOUT" "$((SECONDS - build_start))"
      elif [ "$build_rc" -eq 137 ]; then
        printf 'KILLED docker build (SIGKILL, possibly OOM or timeout escalation; %ss)\n' "$((SECONDS - build_start))"
      else
        printf 'FAILED docker build (exit %s, %ss)\n' "$build_rc" "$((SECONDS - build_start))"
      fi
      exit 1
    fi
    printf 'OK docker build (%ss)\n' "$((SECONDS - build_start))"
    ;;
  real) exec "$ROOT/scripts/pgtest.sh" ;;
  *)
    echo "verify: unknown mode '$MODE'" >&2
    usage >&2
    exit 2
    ;;
esac