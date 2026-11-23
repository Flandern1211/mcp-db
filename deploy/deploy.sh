#!/usr/bin/env bash
# Run one deployment on the target host.
#
# CI invokes this through the forced-command wrapper:
#   bash run-deploy.sh   (SSH_ORIGINAL_COMMAND="deploy ghcr.io/...@sha256:...")
# Administrators may run it directly for a manual rollback:
#   bash deploy.sh 'ghcr.io/flandern1211/mcp-db@sha256:<known digest>'
#
# Contract:
#   - only an immutable digest for this repository is accepted;
#   - the compose file, master key and data directory are provisioned by an
#     administrator and are never uploaded or rewritten here;
#   - the new image is pulled before the live container is touched; a failed
#     pull exits without changing the running service;
#   - the service must become healthy, publish exactly both loopback ports and
#     answer /healthz and /admin/login with 200, otherwise this exits non-zero;
#   - there is no automatic retry, no automatic rollback and no log dump: an
#     uncertain result stops automation and is left for an operator.
#
# Directory layout expected next to this script (see docs/deploy.md):
#   docker-compose.yml  keys/master.key (0400, uid 10001)  data/ (0700, uid 10001)
set -euo pipefail

readonly IMAGE_RE='^ghcr\.io/flandern1211/mcp-db@sha256:[0-9a-f]{64}$'
readonly CONTAINER=mcp-db
readonly HEALTH_TIMEOUT=${MCPDB_DEPLOY_HEALTH_TIMEOUT:-180}
readonly PULL_TIMEOUT=${MCPDB_DEPLOY_PULL_TIMEOUT:-900}
readonly LOCK_TIMEOUT=${MCPDB_DEPLOY_LOCK_TIMEOUT:-60}

DIR=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
readonly DIR
cd "$DIR"

fail() { printf 'deploy: %s\n' "$*" >&2; exit 1; }
log() { printf 'deploy: %s\n' "$*"; }

# -- reference ---------------------------------------------------------------
if [ "$#" -gt 1 ]; then
  fail "expected at most one argument: <image@sha256:...>"
fi
IMAGE_REF=${1:-${MCPDB_IMAGE_REF:-}}
[ -n "$IMAGE_REF" ] || fail "an immutable image digest is required"
if [[ ! $IMAGE_REF =~ $IMAGE_RE ]]; then
  fail "refusing image reference $(printf '%q' "$IMAGE_REF"); expected ghcr.io/flandern1211/mcp-db@sha256:<64 lowercase hex>"
fi

# -- fixed paths, no symlinks ------------------------------------------------
for path in docker-compose.yml keys keys/master.key data; do
  [ -L "$path" ] && fail "refusing symlink at $path"
done
[ -f docker-compose.yml ] || fail "missing docker-compose.yml in $DIR"
[ -d keys ] || fail "missing keys directory in $DIR"
[ -f keys/master.key ] || fail "missing keys/master.key in $DIR; run bootstrap as root once"
[ -d data ] || fail "missing data directory in $DIR; run bootstrap as root once"
for state_file in data/state.db data/credentials.enc; do
  [ -e "$state_file" ] || continue
  [ -L "$state_file" ] && fail "refusing symlink at $state_file"
  [ -f "$state_file" ] || fail "$state_file is not a regular file"
done
command -v docker >/dev/null 2>&1 || fail "docker is required"
command -v curl >/dev/null 2>&1 || fail "curl is required"
command -v flock >/dev/null 2>&1 || fail "flock (util-linux) is required to serialize deployments"

# -- serialize: hold a shared read-only descriptor on the deployment dir -----
exec 9<"$DIR" || fail "cannot open $DIR for locking"
flock -w "$LOCK_TIMEOUT" 9 || fail "another deployment holds the lock; not starting"

# -- provisioned key and state (metadata only, contents never printed) -------
key_meta=$(stat -c '%a %u %s' keys/master.key) || fail "cannot stat keys/master.key"
read -r key_mode key_uid key_size <<<"$key_meta"
[ "$key_size" -gt 0 ] || fail "keys/master.key is empty; run bootstrap as root once"
[ "$key_mode" = 400 ] && [ "$key_uid" = 10001 ] ||
  fail "keys/master.key must be mode 0400 owned by uid 10001; run bootstrap as root"

keys_mode=$(stat -c '%a' keys) || fail "cannot stat keys directory"
[ "$keys_mode" = 755 ] || fail "keys/ must be mode 0755 (found $keys_mode)"

data_meta=$(stat -c '%a %u' data) || fail "cannot stat data directory"
read -r data_mode data_uid <<<"$data_meta"
[ "$data_mode" = 700 ] && [ "$data_uid" = 10001 ] ||
  fail "data/ must be mode 0700 owned by uid 10001; run bootstrap as root"

COMPOSE=(docker compose --project-name mcp-db --file "$DIR/docker-compose.yml")
export MCPDB_IMAGE="$IMAGE_REF"

# -- pull first: the live container is untouched if this fails ---------------
log "pulling $IMAGE_REF"
if ! timeout -k 10 "$PULL_TIMEOUT" "${COMPOSE[@]}" pull "$CONTAINER"; then
  fail "pull failed; the running container was not changed. Check package visibility or host network access."
fi
docker image inspect "$IMAGE_REF" >/dev/null 2>&1 ||
  fail "pulled image $IMAGE_REF is not present locally; refusing to start"

# -- start and wait; --pull never guarantees the digest we just pulled -------
log "starting $IMAGE_REF"
if ! timeout -k 10 "$((HEALTH_TIMEOUT + 60))" \
  "${COMPOSE[@]}" up --detach --wait --wait-timeout "$HEALTH_TIMEOUT" --pull never "$CONTAINER"; then
  fail "compose up failed or did not become healthy within ${HEALTH_TIMEOUT}s; not retrying and not rolling back. Inspect the host manually."
fi

# -- exact loopback port mappings, both ports only --------------------------
expected_ports=$'8787/tcp -> 127.0.0.1:8787\n8788/tcp -> 127.0.0.1:8788'
actual_ports=$(docker port "$CONTAINER" 2>/dev/null | LC_ALL=C sort) ||
  fail "cannot read port mappings for $CONTAINER"
[ "$actual_ports" = "$expected_ports" ] ||
  fail "port mappings are not exactly 127.0.0.1:8787 and 127.0.0.1:8788; refusing to report success"

# -- direct loopback checks --------------------------------------------------
curl -fsS --max-time 5 "http://127.0.0.1:8787/healthz" >/dev/null ||
  fail "GET /healthz did not return 200"
admin_code=$(curl -sS --max-time 5 -o /dev/null -w '%{http_code}' "http://127.0.0.1:8788/admin/login") ||
  fail "GET /admin/login failed at the transport level"
[ "$admin_code" = 200 ] || fail "GET /admin/login returned $admin_code, expected 200"

log "healthy: $IMAGE_REF (healthz 200, admin/login 200, ports loopback only)"
