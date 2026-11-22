#!/usr/bin/env bash
# Deterministic PATH-stub tests: no Docker daemon, build or real cleanup.
# cksum is also stubbed to make the challenge reproducible without a coproc,
# FIFO, prompt responder or production bypass. The production script still
# includes its process ID and random values when generating the challenge.
set -euo pipefail

# Capture the real timeout before putting fixture executables on PATH.
REAL_TIMEOUT=$(type -P timeout)
if [ "${1:-}" != --bounded ]; then
  exec "$REAL_TIMEOUT" -k 2s 120s bash "${BASH_SOURCE[0]}" --bounded
fi
ROOT=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
GUARD="$ROOT/scripts/disk-guard.sh"
FIXTURE=$(mktemp -d)
trap 'rm -rf -- "$FIXTURE"' EXIT
mkdir -p "$FIXTURE/bin" "$FIXTURE/worktree space/target" \
  "$FIXTURE/timed out/target" "$FIXTURE/du failure/target" "$FIXTURE/after failure/target"
export DG_FIXTURE="$FIXTURE" DG_CALLS="$FIXTURE/calls" DG_CHALLENGE="$FIXTURE/challenge"
export DOCKER_HOST=tcp://127.0.0.1:1 DOCKER_CONTEXT= DOCKER_CONFIG="$FIXTURE/no-config"
export PATH="$FIXTURE/bin:$PATH" MCPDB_MIN_FREE_GB=5
unset MCPDB_PRUNE_CONFIRM MCPDB_DU_TIMEOUT
ID1=$(printf 'a%.0s' {1..64})
ID2=$(printf 'b%.0s' {1..64})
ID3=$(printf 'c%.0s' {1..64})

cat > "$FIXTURE/bin/docker" <<'STUB'
#!/usr/bin/env bash
set -eu
printf '%s\n' "$*" >> "$DG_CALLS"
case "$1 ${2:-}" in
  'info ') exit 0 ;;
  'ps -a') [ "${DG_QUERY_FAIL:-0}" != 1 ]; cat "$DG_FIXTURE/containers" ;;
  'system df') [ "${DG_QUERY_FAIL:-0}" != 1 ]; cat "$DG_FIXTURE/cache" ;;
  'images --format'|'volume ls') exit 0 ;;
  'rm '*) printf 'STUB rm: %s\n' "${*:2}" ;;
  'builder prune') printf 'STUB builder: %s\n' "${*:2}" ;;
  *) printf 'Unexpected Docker command: %s\n' "$*" >&2; exit 90 ;;
esac
STUB
# `docker info` has no second argument, so don't use set -u in that stub.
cat > "$FIXTURE/bin/git" <<'STUB'
#!/usr/bin/env bash
case "$*" in
  *'worktree list --porcelain -z') cat "$DG_FIXTURE/worktrees" ;;
  *) exit 90 ;;
esac
STUB
cat > "$FIXTURE/bin/du" <<'STUB'
#!/usr/bin/env bash
target=${*: -1}
printf '%s\n' "$target" >> "$DG_FIXTURE/scans"
case "$target" in
  *'/du failure/target') exit 1 ;;
  *) printf '42M\t%s\n' "$target" ;;
esac
STUB
cat > "$FIXTURE/bin/timeout" <<'STUB'
#!/usr/bin/env bash
printf '%s\n' "$*" >> "$DG_FIXTURE/limits"
[ "$1" = -k ] && [ "$2" = 1s ] || exit 90
shift 3
case "${*: -1}" in
  *'/timed out/target') printf '%s\n' "${*: -1}" >> "$DG_FIXTURE/scans"; exit 124 ;;
  *) exec "$@" ;;
esac
STUB
cat > "$FIXTURE/bin/cksum" <<'STUB'
#!/usr/bin/env bash
cat > "$DG_CHALLENGE"
printf '%s 100\n' "${DG_TEST_SUM:-424242}"
STUB
cat > "$FIXTURE/bin/df" <<'STUB'
#!/usr/bin/env bash
printf 'Filesystem 1024-blocks Used Available Capacity Mounted\n'
printf 'fixture 209715200 104857600 104857600 50%% /fixture\n'
STUB
chmod +x "$FIXTURE/bin/"*
[ "$(type -P docker)" = "$FIXTURE/bin/docker" ]
[ "$(type -P timeout)" = "$FIXTURE/bin/timeout" ]

# Do not inherit a real daemon configuration or use real Git/du during tests.
for wt in 'worktree space' 'timed out' 'du failure' 'after failure'; do
  printf 'worktree %s\0HEAD fixture\0detached\0\0' "$FIXTURE/$wt"
done > "$FIXTURE/worktrees"
valid_containers() {
  printf '%s\tmcpdb-exited\tfixture:dev\texited\t12MB (virtual 99MB)\tmcp-db\n' "$ID1"
  printf '%s\tmcpdb-created\tfixture:dev\tcreated\t3MB\tmcp-db\n' "$ID2"
  printf '%s\tunrelated\text-fixture:dev\texited\t9MB\tmcp-db\n' "$ID3"
  printf '%s\tmcpdb-running\tfixture:dev\trunning\t9MB\tmcp-db\n' "$ID3"
  printf '%s\tmcpdb-unlabelled\tfixture:dev\texited\t5MB\t\n' "$ID3"
  printf '%s\tmcpdb-other-project\tfixture:dev\texited\t6MB\tother-project\n' "$ID3"
}
valid_containers > "$FIXTURE/containers"
printf 'Build Cache\t65\t0\t7.353GB\t257MB\nImages\t18\t6\t4.3GB\t3.1GB\n' > "$FIXTURE/cache"
printf '' > "$FIXTURE/eof"
printf '\n' > "$FIXTURE/blank"
printf '   \n' > "$FIXTURE/whitespace"
printf 'yes\n' > "$FIXTURE/yes"
printf 'y\n' > "$FIXTURE/y"
printf 'prune\n' > "$FIXTURE/static"
printf 'prune-424242\n' > "$FIXTURE/prune"
printf 'prune-cache-424242\n' > "$FIXTURE/prune-cache"
printf 'prune-424242' > "$FIXTURE/prune-no-newline"
printf 'prune-cache-424242' > "$FIXTURE/cache-no-newline"

PASSED=0
FAILED=0
check() {
  local description=$1
  shift
  if "$@"; then PASSED=$((PASSED + 1)); printf 'PASS %s\n' "$description"
  else FAILED=$((FAILED + 1)); printf 'FAIL %s\n' "$description"; fi
}
has() { grep -Fq -- "$1" "$FIXTURE/output"; }
absent() { ! has "$1"; }
exact_write() {
  local writes
  writes=$(grep -Ev '^(info|ps|images|system df|volume ls)( |$)' "$DG_CALLS" || [ "$?" -eq 1 ]) || return 1
  [ "$writes" = "$1" ]
}
zero_writes() { exact_write ''; }
run() {
  local mode=$1 input=$2
  shift 2
  : > "$DG_CALLS"; : > "$FIXTURE/scans"; : > "$FIXTURE/limits"
  RC=0
  env "$@" bash "$GUARD" "$mode" < "$FIXTURE/$input" > "$FIXTURE/output" 2>&1 || RC=$?
}

bash -n "$GUARD" "${BASH_SOURCE[0]}"
for mode in --prune --prune-cache; do
  for input in eof blank whitespace yes y static; do
    for legacy in 0 1; do
      run "$mode" "$input" "MCPDB_PRUNE_CONFIRM=$legacy"
      check "$mode $input env=$legacy rejects (exit $RC)" test "$RC" -eq 1
      check "$mode $input env=$legacy prints aborted" has aborted
      check "$mode $input env=$legacy makes zero deletion calls" zero_writes
    done
  done
  case "$mode" in --prune) partial=prune-no-newline ;; *) partial=cache-no-newline ;; esac
  run "$mode" "$partial" MCPDB_PRUNE_CONFIRM=1
  check "$mode exact token without newline rejects EOF" test "$RC" -eq 1
  check "$mode EOF even with matching text makes zero deletion calls" zero_writes
  check "$mode ignored variable notice is explicit" has 'MCPDB_PRUNE_CONFIRM=1 is ignored'
done

run --prune prune MCPDB_PRUNE_CONFIRM=1
check 'confirmed prune exits 0' test "$RC" -eq 0
check 'confirmed prune issues one non-force rm with full IDs only' exact_write "rm $ID1 $ID2"
check 'prune queries complete IDs, stopped states and the project label' grep -Fq -- 'ps -a --no-trunc --size --filter status=exited --filter status=created --filter label=mcpdb.project=mcp-db' "$DG_CALLS"
check 'Docker SIZE is shown verbatim, including virtual size' has 'Docker SIZE=12MB (virtual 99MB)'
check 'SIZE is not mislabeled writable' absent 'writable='
check 'scope names the label as an ownership convention' has 'label mcpdb.project=mcp-db'
check 'unlabelled mcpdb- containers are excluded' absent mcpdb-unlabelled
check 'other-project labelled containers are excluded' absent mcpdb-other-project
check 'unrelated names are not listed' absent unrelated
check 'running containers are not listed' absent mcpdb-running
check 'impact is shown' has 'Impact:'
check 'recoverability warns about lost writable changes' has 'writable-layer changes are lost unless backed up'
check 'scope was part of checksum input' grep -Fq -- "$ID1" "$DG_CHALLENGE"
check 'confirmation included nonce and process fields' test "$(wc -l < "$DG_CHALLENGE")" -eq 6

run --prune-cache prune-cache
check 'confirmed cache prune exits 0' test "$RC" -eq 0
check 'cache confirmation issues only original builder prune -f' exact_write 'builder prune -f'
check 'cache aggregate includes actual sizes' has 'records=65  active=0  size=7.353GB  reclaimable=257MB'
check 'cache scope is daemon-shared' has 'shared by projects using this daemon'
check 'cache aggregate is not claimed as a list of deletion objects' has 'not a list of entries or the exact amount to be removed'
check 'cache impact is shown' has 'Impact:'
check 'cache recoverability is shown' has 'Recoverability:'
check 'cache scope was part of checksum input' grep -Fq -- 'size=7.353GB' "$DG_CHALLENGE"

for mode in --prune --prune-cache; do
  case "$mode" in --prune) input=prune ;; *) input=prune-cache ;; esac
  run "$mode" "$input" DG_TEST_SUM=989898
  check "$mode mismatching challenge exits 1" test "$RC" -eq 1
  check "$mode stale challenge makes zero deletion calls" zero_writes
  run "$mode" "$input" DG_QUERY_FAIL=1
  check "$mode query failure closes before confirmation" test "$RC" -eq 2
  check "$mode query failure makes zero deletion calls" zero_writes
done

printf 'Images\t18\t6\t4.3GB\t3.1GB\n' > "$FIXTURE/cache"
run --prune-cache prune-cache
check 'missing Build Cache row refuses' test "$RC" -eq 2
check 'missing cache metrics are explicitly reported' has 'could not measure build cache usage'
check 'missing cache row makes zero deletion calls' zero_writes
printf 'Build Cache\t65\t0\tunknown\tunknown\n' > "$FIXTURE/cache"
run --prune-cache prune-cache
check 'unknown cache sizes refuse' test "$RC" -eq 2
check 'unknown cache sizes make zero deletion calls' zero_writes
printf 'abcd\tmcpdb-incomplete\tfixture:dev\texited\t1MB\tmcp-db\n' > "$FIXTURE/containers"
run --prune prune
check 'only incomplete container data refuses' test "$RC" -eq 2
check 'incomplete container data makes zero deletion calls' zero_writes
printf '%s\tmcpdb-legacy\tfixture:dev\texited\t5MB\t\n' "$ID3" > "$FIXTURE/containers"
run --prune prune
check 'only unlabelled legacy containers are a no-op' test "$RC" -eq 0
check 'unlabelled legacy containers are reported as skipped' has 'were skipped'
check 'unlabelled legacy containers make zero deletion calls' zero_writes
: > "$FIXTURE/containers"
run --prune prune
check 'empty candidate list is a no-op' test "$RC" -eq 0
check 'empty candidate list makes zero deletion calls' zero_writes

for limit in default 7 0 00 000 abc; do
  if [ "$limit" = default ]; then run report eof
  else run report eof "MCPDB_DU_TIMEOUT=$limit"; fi
  check "report limit=$limit finishes normally" test "$RC" -eq 0
  check "report limit=$limit includes host" has '== host =='
  check "report limit=$limit includes Docker" has '== docker =='
  check "report limit=$limit includes worktrees" has '== worktree targets =='
  check "report limit=$limit preserves spaces and size" has "42M$(printf '\t')$FIXTURE/worktree space/target"
  check "report limit=$limit labels timeout accurately" has "$FIXTURE/timed out/target  (scan timed out)"
  check "report limit=$limit labels du failure accurately" has "$FIXTURE/du failure/target  (du failed, exit 1)"
  check "report limit=$limit retains the later healthy entry" has "$FIXTURE/after failure/target"
  check "report limit=$limit scans all four targets" test "$(wc -l < "$FIXTURE/scans")" -eq 4
  expected=30; [ "$limit" != 7 ] || expected=7
  check "report limit=$limit uses bounded $expected seconds" grep -Fq -- "-k 1s $expected du -sh --" "$FIXTURE/limits"
  check "report limit=$limit has zero deletion calls" zero_writes
done
run --check eof
check '--check default exits 0' test "$RC" -eq 0
check '--check reports free space from df' has 'OK: host free space 100.0 GiB'
run --check eof MCPDB_MIN_FREE_GB=1000000
check '--check impossible threshold exits 1' test "$RC" -eq 1
check '--check shows separate cache command' has 'bash scripts/disk-guard.sh --prune-cache'
check '--check protects images, volumes, worktrees and source' has 'neither removes images, volumes, worktrees or source'
printf '\n%d passed, %d failed\n' "$PASSED" "$FAILED"
[ "$FAILED" -eq 0 ]
