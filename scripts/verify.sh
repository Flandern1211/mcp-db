#!/usr/bin/env bash
# Unified verification entry for this repository.
#
# Usage: scripts/verify.sh [fmt|clippy|test|build|fast|full|real]
#   fmt     cargo fmt --check
#   clippy  cargo clippy -D warnings
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
# Override with MCPDB_RUST_IMAGE, MCPDB_RUST_TOOLCHAIN, MCPDB_TIMEOUT,
# MCPDB_CARGO_REGISTRY_VOL, MCPDB_CARGO_TARGET_VOL.

set -euo pipefail
export MSYS_NO_PATHCONV=1

ROOT=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
ROOT_WIN=$(cd "$ROOT" && pwd -W 2>/dev/null || cygpath -w "$ROOT")
HASH=$(printf '%s' "$ROOT" | cksum | awk '{print $1}')
IMAGE=${MCPDB_RUST_IMAGE:-rust:1.89-slim-bookworm}
TOOLCHAIN=${MCPDB_RUST_TOOLCHAIN:-1.89.0}
REGISTRY_VOL=${MCPDB_CARGO_REGISTRY_VOL:-mcpdb-cargo-registry}
TARGET_VOL=${MCPDB_CARGO_TARGET_VOL:-mcpdb-cargo-target-$HASH}
TIMEOUT=${MCPDB_TIMEOUT:-${MCPDB_STEP_TIMEOUT:-1800}}
MODE=${1:-fast}

usage() {
  sed -n '3,20p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//'
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
    exit 2
  }
}

run_group() {
  local label=$1
  local body=$2
  local start=$SECONDS
  printf '\n== %s ==\n' "$label"
  if ! timeout "$TIMEOUT" docker run --rm \
    -e RUSTUP_TOOLCHAIN="$TOOLCHAIN" \
    -v "$ROOT_WIN:/work" \
    -v "$REGISTRY_VOL:/usr/local/cargo/registry" \
    -v "$TARGET_VOL:/work/target" \
    -w /work "$IMAGE" \
    sh -c "set -e
step() { n=\$1; shift; echo \"== \$n ==\"; s=\$(date +%s); \"\$@\"; echo \"OK \$n (\$(( \$(date +%s) - s ))s)\"; }
rustup component add rustfmt clippy >/dev/null 2>&1 || true
$body"; then
    printf 'FAILED %s (%ss)\n' "$label" "$((SECONDS - start))"
    exit 1
  fi
  printf 'OK %s (%ss)\n' "$label" "$((SECONDS - start))"
}

FAST_BODY='step fmt cargo fmt --check
step clippy cargo clippy --locked --all-targets --all-features -- -D warnings
step test cargo test --locked'

FULL_BODY="$FAST_BODY
step build cargo build --release --locked"

require_docker

case "$MODE" in
  fmt) run_group fmt 'step fmt cargo fmt --check' ;;
  clippy) run_group clippy 'step clippy cargo clippy --locked --all-targets --all-features -- -D warnings' ;;
  test) run_group test 'step test cargo test --locked' ;;
  build) run_group build 'step build cargo build --release --locked' ;;
  fast) run_group fast "$FAST_BODY" ;;
  full)
    run_group full "$FULL_BODY"
    printf '\n== docker build ==\n'
    docker build -t mcp-db:dev "$ROOT_WIN"
    echo 'OK docker build'
    ;;
  real) exec "$ROOT/scripts/pgtest.sh" ;;
  -h|--help|help) usage ;;
  *)
    echo "verify: unknown mode '$MODE'" >&2
    usage >&2
    exit 2
    ;;
esac