#!/usr/bin/env bash
# Isolated PostgreSQL integration entry.
#
# Starts a throwaway PostgreSQL 16 container on an ephemeral host port,
# creates the rw/ro/provision accounts and fixtures, runs the selected
# `--ignored` integration targets inside the pinned Rust image, then removes
# the container on exit (including Ctrl-C).
#
# Usage: scripts/pgtest.sh [test-target ...]
# Allowed targets: postgres_metadata postgres_readonly profile_limits
#                  provision_postgres http_mcp
# Default: all five.
#
# Credentials fixture tests are not included: tests/credentials.rs needs
# MCPDB_SEAL_DIR / MCPDB_OPEN_DIR and is exercised by the container smoke
# entry of the task that changes credential storage.
#
# Override with MCPDB_PGTEST_PORT (host port; 0 = ephemeral),
# MCPDB_PGTEST_IMAGE, MCPDB_RUST_IMAGE, MCPDB_RUST_TOOLCHAIN,
# MCPDB_TEST_TIMEOUT.

set -euo pipefail
export MSYS_NO_PATHCONV=1

ROOT=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
ROOT_WIN=$(cd "$ROOT" && pwd -W 2>/dev/null || cygpath -w "$ROOT")
HASH=$(printf '%s' "$ROOT" | cksum | awk '{print $1}')
RUST_IMAGE=${MCPDB_RUST_IMAGE:-rust:1.89-slim-bookworm}
PG_IMAGE=${MCPDB_PGTEST_IMAGE:-postgres:16}
TOOLCHAIN=${MCPDB_RUST_TOOLCHAIN:-1.89.0}
REGISTRY_VOL=${MCPDB_CARGO_REGISTRY_VOL:-mcpdb-cargo-registry}
TARGET_VOL=${MCPDB_CARGO_TARGET_VOL:-mcpdb-cargo-target-$HASH}
PORT_OVERRIDE=${MCPDB_PGTEST_PORT:-0}
TIMEOUT=${MCPDB_TEST_TIMEOUT:-900}
NAME="mcpdb-pgtest-$$"
CID=""
ALLOWED="postgres_metadata postgres_readonly profile_limits provision_postgres http_mcp"

cleanup() {
  if [ -n "$CID" ]; then
    docker rm -f "$CID" >/dev/null 2>&1 || true
    echo "pgtest: removed $CID"
  fi
}
on_signal() {
  cleanup
  exit 130
}
trap cleanup EXIT
trap on_signal INT TERM

command -v docker >/dev/null 2>&1 || { echo "pgtest: docker is required" >&2; exit 2; }
docker info >/dev/null 2>&1 || { echo "pgtest: docker daemon is not reachable" >&2; exit 2; }

TARGETS=("$@")
if [ ${#TARGETS[@]} -eq 0 ]; then
  TARGETS=(postgres_metadata postgres_readonly profile_limits provision_postgres http_mcp)
fi
for target in "${TARGETS[@]}"; do
  case " $ALLOWED " in
    *" $target "*) ;;
    *) echo "pgtest: unknown target '$target'" >&2; exit 2 ;;
  esac
done

echo "== pgtest: starting $PG_IMAGE =="
docker run -d --name "$NAME" \
  -e POSTGRES_PASSWORD=pgtest \
  -p "127.0.0.1:$PORT_OVERRIDE:5432" \
  "$PG_IMAGE" >/dev/null
CID=$NAME

for _ in $(seq 1 60); do
  if docker exec "$CID" psql -U postgres -tAc 'SELECT 1' >/dev/null 2>&1; then
    break
  fi
  sleep 1
done
docker exec "$CID" psql -U postgres -tAc 'SELECT 1' >/dev/null

PORT=$(docker port "$CID" 5432 | head -1 | sed 's/.*://')
[ -n "$PORT" ] || { echo "pgtest: cannot determine host port" >&2; exit 1; }

echo "== pgtest: creating accounts and fixtures on port $PORT =="
docker exec -i "$CID" psql -v ON_ERROR_STOP=1 -U postgres <<'SQL'
CREATE USER tester PASSWORD 'testerpass';
CREATE USER ro_user PASSWORD 'ropass';
CREATE USER provision PASSWORD 'provpass' CREATEDB CREATEROLE;
CREATE DATABASE app OWNER tester;
SQL

docker exec -i "$CID" psql -v ON_ERROR_STOP=1 -U postgres -d app <<'SQL'
SET ROLE tester;
CREATE TABLE users (id bigint PRIMARY KEY, name text NOT NULL);
INSERT INTO users (id, name) VALUES (1, 'alice'), (2, 'bob');
CREATE TABLE readonly_fixture (id bigint PRIMARY KEY, name text NOT NULL);
INSERT INTO readonly_fixture (id, name) VALUES (1, 'one'), (2, 'two');
RESET ROLE;
GRANT CONNECT ON DATABASE app TO ro_user;
GRANT USAGE ON SCHEMA public TO ro_user;
GRANT SELECT ON readonly_fixture TO ro_user;
GRANT SELECT ON users TO ro_user;
SQL

DSN_BASE="host=host.docker.internal port=$PORT"
PG_DSN="$DSN_BASE user=tester password=testerpass dbname=app"
RO_DSN="$DSN_BASE user=ro_user password=ropass dbname=app"
ADMIN_DSN="$DSN_BASE user=provision password=provpass dbname=app"

for target in "${TARGETS[@]}"; do
  echo
  echo "== pgtest: $target =="
  if ! timeout "$TIMEOUT" docker run --rm \
    --add-host=host.docker.internal:host-gateway \
    -e RUSTUP_TOOLCHAIN="$TOOLCHAIN" \
    -e MCPDB_TEST_PG_DSN="$PG_DSN" \
    -e MCPDB_TEST_PG_RO_DSN="$RO_DSN" \
    -e MCPDB_TEST_PG_ADMIN_DSN="$ADMIN_DSN" \
    -v "$ROOT_WIN:/work" \
    -v "$REGISTRY_VOL:/usr/local/cargo/registry" \
    -v "$TARGET_VOL:/work/target" \
    -w /work "$RUST_IMAGE" \
    cargo test --locked --test "$target" -- --ignored; then
    echo "pgtest: FAILED $target" >&2
    exit 1
  fi
done

echo
echo "pgtest: all selected targets passed"