#!/usr/bin/env bash
# Report and guard Docker/build resources for this repository.
#
# Usage: scripts/disk-guard.sh [--check|--prune|--prune-cache]
#   (no args)      read-only report: host free space, Docker usage, mcp-db images,
#                  mcpdb volumes and worktree targets. Target scans run in parallel
#                  with a timeout per directory; timeout or failure is labelled.
#   --check        fail below MCPDB_MIN_FREE_GB (default 5); otherwise warn below 10 GiB
#   --prune        remove only containers in created/exited state that are named
#                  mcpdb-* and carry the project label mcpdb.project=mcp-db. The
#                  label is an ownership convention, not an authorization. Older
#                  containers without it are skipped; opt in for new ones with
#                  docker run --label mcpdb.project=mcp-db ...
#   --prune-cache  separately remove cache eligible for the default Docker builder prune
#                  from the daemon's shared build cache.
#
# Both prune modes show the scope, size, impact and recoverability, then ask for
# a new confirmation token for this run. Blank input, EOF or a mismatch aborts.
# MCPDB_PRUNE_CONFIRM=1 does not bypass confirmation.
#
# Environment: MCPDB_MIN_FREE_GB (default 5), MCPDB_DU_TIMEOUT (positive integer
# seconds per target; defaults or falls back to 30).

set -euo pipefail
export MSYS_NO_PATHCONV=1

ROOT=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
ROOT_WIN=$(cygpath -w "$ROOT" 2>/dev/null || printf '%s' "$ROOT")
MIN_FREE_GB=${MCPDB_MIN_FREE_GB:-5}
DU_TIMEOUT=${MCPDB_DU_TIMEOUT:-30}
[[ "$DU_TIMEOUT" =~ ^0*[1-9][0-9]*$ ]] || DU_TIMEOUT=30
MODE=${1:-report}

usage() {
  sed -n '4,/^$/p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//'
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

confirm_run() {
  local mode=$1 scope=$2 sum token answer
  if [ "${MCPDB_PRUNE_CONFIRM:-}" = "1" ]; then
    echo "MCPDB_PRUNE_CONFIRM=1 is ignored; this run still requires confirmation."
  fi
  # Tie the prompt to this run and its listing, so stale replies do not work.
  # A matching reply is not independent evidence of the answerer's identity.
  sum=$(printf '%s\n' "$mode" "$scope" "$$" "$RANDOM" "$RANDOM" | cksum) || {
    echo "aborted: could not generate a confirmation token; nothing removed" >&2
    return 1
  }
  token="$mode-${sum%% *}"
  printf "Type '%s' and press Enter to confirm the scope above (anything else aborts):\n" "$token"
  if ! IFS= read -r answer || [ "$answer" != "$token" ]; then
    echo "aborted: no matching confirmation for this scope; nothing removed"
    return 1
  fi
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

report_target() {
  local target=$1 out rc=0
  out=$(timeout -k 1s "$DU_TIMEOUT" du -sh -- "$target" 2>/dev/null) || rc=$?
  case "$rc" in
    0) printf '  %s\n' "$out" ;;
    124|137) printf '  %s  (scan timed out)\n' "$target" ;;
    *) printf '  %s  (du failed, exit %s)\n' "$target" "$rc" ;;
  esac
}

show_worktrees() {
  echo
  echo "== worktree targets =="
  # NUL-delimited fields preserve paths verbatim. Scan concurrently so the
  # per-target limits do not add up or cause later worktrees to be skipped.
  local field target
  git -C "$ROOT_WIN" worktree list --porcelain -z | (
    while IFS= read -r -d '' field; do
      [[ "$field" == 'worktree '* ]] || continue
      target=${field#worktree }/target
      [ -d "$target" ] || continue
      report_target "$target" &
    done
    wait
  )
}

check() {
  require_docker
  local free
  free=$(free_gb)
  if awk -v f="$free" -v m="$MIN_FREE_GB" 'BEGIN {exit !(f < m)}'; then
    echo "FAIL: host free space ${free} GiB < ${MIN_FREE_GB} GiB" >&2
    echo "hint: run 'bash scripts/disk-guard.sh' to inspect." >&2
    echo "hint: 'bash scripts/disk-guard.sh --prune' removes only stopped mcpdb-* containers labelled mcpdb.project=mcp-db." >&2
    echo "hint: 'bash scripts/disk-guard.sh --prune-cache' separately prunes the shared Docker build cache." >&2
    echo "hint: both require confirmation for this run; neither removes images, volumes, worktrees or source." >&2
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
  local rows rc=0
  # Ownership convention, not authorization: the name prefix plus the project
  # label. The label is requested back as well, so a daemon or mock that ignores
  # the filter cannot widen the set.
  rows=$(docker ps -a --no-trunc --size \
    --filter status=exited --filter status=created \
    --filter label=mcpdb.project=mcp-db \
    --format '{{.ID}}\t{{.Names}}\t{{.Image}}\t{{.State}}\t{{.Size}}\t{{.Label "mcpdb.project"}}' | awk -F '\t' '
      $2 ~ /^mcpdb-/ && $4 ~ /^(exited|created)$/ {
        if ($6 != "mcp-db") ++unlabelled
        else if (NF == 6 && length($1) == 64 && $1 ~ /^[a-f0-9]+$/ && $5 ~ /[0-9]/) {
          printf "%s\t%s\t%s\t%s\t%s\n", $1, $2, $3, $4, $5; ++ok
        } else ++bad
      }
      END {
        if (unlabelled) printf "note: %d stopped mcpdb- row(s) without the mcpdb.project=mcp-db label were skipped\n", unlabelled > "/dev/stderr"
        if (bad) printf "note: %d labelled row(s) with an incomplete ID or size were skipped\n", bad > "/dev/stderr"
        exit (bad && !ok) ? 2 : 0
      }') || rc=$?
  if [ "$rc" -ne 0 ]; then
    echo "prune: incomplete or failed container listing; nothing removed" >&2
    exit 2
  fi
  if [ -z "$rows" ]; then
    echo "prune: no stopped mcpdb-* containers with label mcpdb.project=mcp-db to remove"
    return
  fi
  echo "Scope: containers in created/exited state named mcpdb-* with label mcpdb.project=mcp-db."
  echo "The label is an ownership convention, not an authorization; unlabelled containers are skipped."
  local id name image state size ids=()
  while IFS=$'\t' read -r id name image state size; do
    ids+=("$id")
    printf '  %s  %s  image=%s  state=%s  Docker SIZE=%s\n' "$id" "$name" "$image" "$state" "$size"
  done <<< "$rows"
  echo "Impact: these containers and their writable layers are removed. Images, volumes, build cache, worktrees and source are untouched."
  echo "Recoverability: recreate the containers from their images and original options; writable-layer changes are lost unless backed up."
  confirm_run prune "$rows" || exit 1
  # Full IDs retain the displayed targets even if names are reused. No force or volume removal.
  docker rm "${ids[@]}"
  echo "prune: done"
  show_host
}

prune_cache() {
  require_docker
  local usage
  usage=$(docker system df --format '{{.Type}}\t{{.TotalCount}}\t{{.Active}}\t{{.Size}}\t{{.Reclaimable}}' \
    | awk -F '\t' '
      $1 == "Build Cache" && NF == 5 && $2 ~ /^[0-9]+$/ && $3 ~ /^[0-9]+$/ && $4 ~ /[0-9]/ && $5 ~ /[0-9]/ {
        printf "  records=%s  active=%s  size=%s  reclaimable=%s\n", $2, $3, $4, $5; ++n
      }
      END { exit n != 1 }') || {
    echo "prune-cache: could not measure build cache usage; nothing removed" >&2
    exit 2
  }
  echo "Scope: cache eligible for the default Docker builder prune, shared by projects using this daemon."
  echo "Daemon-wide build cache usage (aggregate, not a list of entries or the exact amount to be removed):"
  printf '%s\n' "$usage"
  echo "Impact: future builds may be slower. Images, volumes, containers, worktrees and source are untouched."
  echo "Recoverability: pruned cache entries are not restored; subsequent builds can regenerate them."
  confirm_run prune-cache "$usage" || exit 1
  docker builder prune -f
  echo "prune-cache: done"
  show_host
}

case "$MODE" in
  report) show_host; show_docker; show_worktrees ;;
  --check) check ;;
  --prune) prune ;;
  --prune-cache) prune_cache ;;
  *) usage; exit 2 ;;
esac
