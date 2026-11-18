#!/usr/bin/env bash
# Report and guard Docker/build resources for this repository.
#
# Usage: scripts/disk-guard.sh [--check|--prune|--prune-cache]
#   (no args)      read-only report: host free space, Docker usage, mcp-db images,
#                  mcpdb volumes and cargo target directories per worktree
#   --check        exit non-zero when host free space is below MCPDB_MIN_FREE_GB
#                  (default 5); 10GB or less prints a warning
#   --prune        remove stopped containers whose names start with mcpdb- only;
#                  never images, volumes, worktrees or source. Asks for
#                  confirmation unless MCPDB_PRUNE_CONFIRM=1
#   --prune-cache  remove Docker build cache. This is shared by every project on
#                  the daemon; asks for confirmation and names the scope first.
#
# Environment: MCPDB_MIN_FREE_GB (default 5), MCPDB_PRUNE_CONFIRM=1.

set -euo pipefail
export MSYS_NO_PATHCONV=1

ROOT=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
ROOT_WIN=$(cygpath -w "$ROOT" 2>/dev/null || printf '%s' "$ROOT")
MIN_FREE_GB=${MCPDB_MIN_FREE_GB:-5}
MODE=${1:-report}

usage() {
  sed -n '3,16p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//'
}

free_gb() {
  df -Pk "$ROOT" | awk 'NR==2 {printf "%.1f", $4/1024/1024}'
}

require_docker() {
  command -v docker >/dev/null 2>&1 || {
    echo "disk-guard: docker is required" >&2
    exit 2
  }
  docker info >/dev/null 2>&1 || {
    echo "disk-guard: docker daemon is not reachable" >&2
    exit 2
  }
}

show_host() {
  local free
  free=$(free_gb)
  echo "== host =="
  echo "free: ${free} GiB (threshold ${MIN_FREE_GB} GiB)"
  df -h "$ROOT" | tail -1
}

show_docker() {
  echo
  echo "== docker =="
  if docker info >/dev/null 2>&1; then
    docker system df
    echo
    echo "mcp-db images:"
    docker images --format '{{.Repository}}:{{.Tag}}  {{.Size}}  {{.CreatedSince}}' \
      | grep '^mcp-db' | sed 's/^/  /' || true
    echo "mcpdb volumes:"
    docker volume ls --filter name=mcpdb --format '  {{.Name}}' || true
  else
    echo "docker daemon unreachable"
  fi
}

show_worktrees() {
  echo
  echo "== worktree targets =="
  git -C "$ROOT_WIN" worktree list --porcelain \
    | awk '/^worktree / {print $2}' \
    | while read -r wt; do
        if [ -d "$wt/target" ]; then
          du -sh "$wt/target" 2>/dev/null | sed 's/^/  /'
        fi
      done
}

check() {
  require_docker
  local free
  free=$(free_gb)
  if awk -v f="$free" -v m="$MIN_FREE_GB" 'BEGIN {exit !(f < m)}'; then
    echo "FAIL: host free space ${free} GiB < ${MIN_FREE_GB} GiB" >&2
    echo "hint: run scripts/disk-guard.sh to inspect; use --prune to remove stopped containers, dangling images and build cache." >&2
    exit 1
  fi
  if awk -v f="$free" 'BEGIN {exit !(f < 10)}'; then
    echo "WARN: host free space ${free} GiB is low; run only necessary builds and clean up afterwards." >&2
  else
    echo "OK: host free space ${free} GiB"
  fi
}

prune() {
  require_docker
  local names
  names=$(docker ps -a --filter status=exited --filter status=created --format '{{.Names}}' | grep -E '^mcpdb-' || true)
  if [ -z "$names" ]; then
    echo "prune: no mcpdb-* containers to remove"
    return
  fi
  echo "The following stopped mcpdb-* containers will be removed:"
  echo "$names" | sed 's/^/  /'
  echo "Not removed: images, volumes, build cache, worktrees, source, other projects' containers."
  if [ "${MCPDB_PRUNE_CONFIRM:-}" != "1" ]; then
    read -r -p "Proceed? [y/N] " answer
    case "$answer" in
      y|Y|yes|YES) ;;
      *) echo "aborted"; exit 1 ;;
    esac
  fi
  echo "$names" | xargs -r docker rm
  echo "prune: done"
  show_host
}

prune_cache() {
  require_docker
  echo "WARNING: Docker build cache is shared by every project on this daemon."
  echo "Removing it only makes future builds slower; it does not touch images, volumes or source."
  docker system df | sed 's/^/  /'
  if [ "${MCPDB_PRUNE_CONFIRM:-}" != "1" ]; then
    read -r -p "Proceed? [y/N] " answer
    case "$answer" in
      y|Y|yes|YES) ;;
      *) echo "aborted"; exit 1 ;;
    esac
  fi
  docker builder prune -f
  echo "prune-cache: done"
  show_host
}

case "$MODE" in
  report)
    show_host
    show_docker
    show_worktrees
    ;;
  --check)
    check
    ;;
  --prune)
    prune
    ;;
  --prune-cache)
    prune_cache
    ;;
  *)
    usage
    exit 2
    ;;
esac