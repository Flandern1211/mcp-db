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
# All Rust steps run in the pinned image; the host toolchain is never used.
# Each worktree gets its own target volume so parallel tasks do not share
# build locks or cross-branch artifacts.
#
# Override with MCPDB_RUST_IMAGE, MCPDB_RUST_TOOLCHAIN, MCPDB_STEP_TIMEOUT,
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
STEP_TIMEOUT=${MCPDB_STEP_TIMEOUT:-900}
MODE=${1:-fast}

usage() {
  sed -n '3,21p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//'
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

run_step() {
  local name=$1
  local cmd=$2
  local start=$SECONDS
  printf '\n== %s ==\n' "$name"
  if ! timeout "$STEP_TIMEOUT" docker run --rm \
    -e RUSTUP_TOOLCHAIN="$TOOLCHAIN" \
    -v "$ROOT_WIN:/work" \
    -v "$REGISTRY_VOL:/usr/local/cargo/registry" \
    -v "$TARGET_VOL:/work/target" \
    -w /work "$IMAGE" \
    sh -c "cargo $cmd"; then
    printf 'FAILED %s (%ss)\n' "$name" "$((SECONDS - start))"
    exit 1
  fi
  printf 'OK %s (%ss)\n' "$name" "$((SECONDS - start))"
}

run_fast() {
  run_step fmt 'fmt --check'
  run_step clippy 'clippy --locked --all-targets --all-features -- -D warnings'
  run_step test 'test --locked'
}

require_docker

case "$MODE" in
  fmt) run_step fmt 'fmt --check' ;;
  clippy) run_step clippy 'clippy --locked --all-targets --all-features -- -D warnings' ;;
  test) run_step test 'test --locked' ;;
  build) run_step build 'build --release --locked' ;;
  fast) run_fast ;;
  full)
    run_fast
    run_step build 'build --release --locked'
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