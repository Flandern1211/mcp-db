#!/usr/bin/env bash
# Read-only closeout audit. Never writes to the repository.
#
# Checks:
#   1. the worktree being closed is clean; other dirty worktrees are WARN
#   2. no local branch has unmerged commits (WARN only)
#   3. no leftover mcpdb-* containers (WARN only)
#   4. TASK.md status and open-item count match each task file (FAIL)
#   5. HEAD commit carries an Unverified: trailer (WARN only)
#
# Usage: scripts/closeout.sh
# Exit: 0 no FAIL, 1 at least one FAIL.

set -uo pipefail
# Gap tables use emoji markers; byte-wise matching keeps ugrep/GNU grep consistent.
export LC_ALL=C

ROOT=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
INTEGRATION=${MCPDB_INTEGRATION_BRANCH:-develop}
fails=0
warns=0

fail() { printf '%-5s %s\n' "FAIL" "$*"; fails=$((fails + 1)); }
warn() { printf '%-5s %s\n' "WARN" "$*"; warns=$((warns + 1)); }
ok()   { printf '%-5s %s\n' "OK" "$*"; }

echo "== closeout: $ROOT (integration: $INTEGRATION) =="

echo
echo "-- worktrees"
while IFS= read -r wt; do
  [ -d "$wt" ] || continue
  dirty=$(git -C "$wt" status --porcelain -- . ':(exclude).plan' ':(exclude).zcode' 2>/dev/null)
  if [ -z "$dirty" ]; then
    ok "clean: $wt"
  elif [ "$(git -C "$wt" rev-parse --path-format=absolute --show-toplevel 2>/dev/null)" = \
         "$(git -C "$ROOT" rev-parse --path-format=absolute --show-toplevel 2>/dev/null)" ]; then
    fail "dirty worktree (current task): $wt"
    printf '%s\n' "$dirty" | sed 's/^/       /'
  else
    warn "dirty worktree (other task): $wt"
    printf '%s\n' "$dirty" | sed 's/^/       /'
  fi
done < <(git -C "$ROOT" worktree list --porcelain | awk '/^worktree /{print substr($0,10)}')

echo
echo "-- branches"
if ! git -C "$ROOT" rev-parse --verify --quiet "$INTEGRATION" >/dev/null; then
  fail "integration branch '$INTEGRATION' not found"
fi
while IFS= read -r branch; do
  [ "$branch" = "$INTEGRATION" ] && continue
  count=$(git -C "$ROOT" rev-list --count "$INTEGRATION..$branch" 2>/dev/null || echo 0)
  if [ "${count:-0}" -gt 0 ]; then
    warn "unmerged: $branch ($count commits)"
  fi
done < <(git -C "$ROOT" for-each-ref --format='%(refname:short)' refs/heads/)

echo
echo "-- containers"
if command -v docker >/dev/null 2>&1 && docker info >/dev/null 2>&1; then
  leftovers=$(docker ps -a --filter 'name=mcpdb-' --format '{{.Names}} ({{.Status}})' 2>/dev/null || true)
  if [ -n "$leftovers" ]; then
    while IFS= read -r line; do
      [ -n "$line" ] && warn "container: $line"
    done <<< "$leftovers"
  else
    ok "no mcpdb-* containers"
  fi
else
  warn "docker daemon not reachable; container check skipped"
fi

echo
echo "-- TASK.md consistency"
while IFS='|' read -r _ id _ _ status _ _ _ unclosed link _; do
  id=$(printf '%s' "$id" | xargs)
  status=$(printf '%s' "$status" | xargs)
  unclosed=$(printf '%s' "$unclosed" | xargs)
  file=$(printf '%s' "$link" | sed -n 's/.*(\(tasks\/[^)]*\.md\)).*/\1/p' | head -1)
  [ -n "$file" ] || continue
  if [ ! -f "$ROOT/$file" ]; then
    fail "$id: task file missing ($file)"
    continue
  fi

  task_status=$(grep -m1 '^| 状态 |' "$ROOT/$file" | awk -F'|' '{gsub(/^ +| +$/,"",$3); print $3}')
  if [ -n "$task_status" ] && [[ "$task_status" != "$status"* && "$status" != "$task_status"* ]]; then
    fail "$id: status mismatch TASK.md='$status' task='$task_status'"
  fi

  open=$(grep -c '^| 🔴 ' "$ROOT/$file" 2>/dev/null || true)
  open=${open:-0}
  if grep -q '^| 🔴 无 ' "$ROOT/$file" 2>/dev/null; then
    open=0
  fi

  if [ "$status" = "🟢 已完成" ] && [ "$open" -gt 0 ]; then
    fail "$id: marked 已完成 with $open open row(s)"
  fi

  if [ "$unclosed" = "无" ]; then
    [ "$open" -eq 0 ] || fail "$id: TASK.md says 无 but task file has $open open row(s)"
  elif [[ "$unclosed" =~ ^[0-9]+ ]]; then
    expected=${unclosed%% *}
    [ "$expected" = "$open" ] || fail "$id: open count mismatch TASK.md=$expected task=$open"
  fi
done < <(grep '^| T-' "$ROOT/TASK.md")

echo
echo "-- HEAD commit"
subject=$(git -C "$ROOT" log -1 --format=%s 2>/dev/null || true)
body=$(git -C "$ROOT" log -1 --format=%B 2>/dev/null || true)
if [[ "$subject" == merge:* ]]; then
  ok "HEAD is a merge commit; trailer check skipped"
elif printf '%s' "$body" | grep -q '^Unverified:'; then
  ok "HEAD has Unverified: trailer"
else
  warn "HEAD commit has no 'Unverified:' trailer"
fi

echo
if [ "$fails" -gt 0 ]; then
  echo "closeout: $fails blocking issue(s), $warns warning(s)"
  exit 1
fi
echo "closeout: OK ($warns warning(s))"