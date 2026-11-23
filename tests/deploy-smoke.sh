#!/usr/bin/env bash
# Container smoke test for a pre-built image; used by CI and runnable locally.
#
# Usage: tests/deploy-smoke.sh <image-ref>
#
# Starts the image with a fresh ephemeral data bind mount and a synthetic
# 32-byte master key (no real credentials, no state from another run), lets
# Docker assign loopback host ports, waits for the container healthcheck with a
# bounded timeout, then asserts:
#   - GET /healthz returns 200
#   - GET /admin/login returns 200
# It never prints application logs and never touches an external service.
set -euo pipefail

if [ "$#" -ne 1 ] || [ -z "$1" ]; then
  printf 'usage: %s <image-ref>\n' "${BASH_SOURCE[0]}" >&2
  exit 2
fi
IMAGE_REF=$1

command -v docker >/dev/null 2>&1 || {
  echo "smoke: docker is required" >&2
  exit 2
}
command -v curl >/dev/null 2>&1 || {
  echo "smoke: curl is required" >&2
  exit 2
}

container="mcpdb-smoke-$$-$RANDOM"
work=$(mktemp -d)
# Git Bash needs a Windows path for Docker Desktop; Linux and macOS keep the
# POSIX path. Container paths are protected from MSYS rewriting.
if work_win=$(cd "$work" && pwd -W 2>/dev/null); then
  work=$work_win
fi
export MSYS_NO_PATHCONV=1

cleanup() {
  docker rm -f "$container" >/dev/null 2>&1 || true
  rm -rf "$work"
}
trap cleanup EXIT INT TERM

mkdir -p "$work/data" "$work/keys"
chmod 755 "$work" "$work/keys"
chmod 777 "$work/data"
head -c 32 /dev/urandom | od -An -tx1 | tr -d ' \n' > "$work/keys/master.key"
chmod 644 "$work/keys/master.key"

docker run -d --name "$container" \
  --label mcpdb.project=mcp-db \
  -v "$work/data:/data" \
  -v "$work/keys:/keys:ro" \
  -e MCPDB_CREDENTIAL_STORE=/data/credentials.enc \
  -e MCPDB_MASTER_KEY_FILE=/keys/master.key \
  -p 127.0.0.1::8787 \
  -p 127.0.0.1::8788 \
  "$IMAGE_REF" >/dev/null

status=missing
for _ in $(seq 1 60); do
  status=$(docker inspect \
    --format '{{if .State.Health}}{{.State.Health.Status}}{{else}}none{{end}}' \
    "$container" 2>/dev/null || echo missing)
  case $status in
    healthy|unhealthy) break ;;
  esac
  sleep 2
done

if [ "$status" != healthy ]; then
  printf 'smoke: container did not become healthy (status=%s)\n' "$status" >&2
  exit 1
fi

mcp_line=$(docker port "$container" 8787/tcp | head -1)
admin_line=$(docker port "$container" 8788/tcp | head -1)
case $mcp_line in
  127.0.0.1:*) ;;
  *) printf 'smoke: MCP port is not published on loopback: %s\n' "$mcp_line" >&2; exit 1 ;;
esac
case $admin_line in
  127.0.0.1:*) ;;
  *) printf 'smoke: admin port is not published on loopback: %s\n' "$admin_line" >&2; exit 1 ;;
esac
mcp_port=${mcp_line##*:}
admin_port=${admin_line##*:}

health_code=$(curl -sS --max-time 5 -o "$work/health.body" -w '%{http_code}' \
  "http://127.0.0.1:${mcp_port}/healthz")
if [ "$health_code" != 200 ]; then
  printf 'smoke: GET /healthz returned %s, expected 200\n' "$health_code" >&2
  exit 1
fi
admin_code=$(curl -sS --max-time 5 -o "$work/admin.body" -w '%{http_code}' \
  "http://127.0.0.1:${admin_port}/admin/login")
if [ "$admin_code" != 200 ]; then
  printf 'smoke: GET /admin/login returned %s, expected 200\n' "$admin_code" >&2
  exit 1
fi

printf 'smoke: ok (healthz 200, admin/login 200)\n'
