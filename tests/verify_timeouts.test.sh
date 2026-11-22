#!/usr/bin/env bash
# Run with Bash and GNU timeout. Test the unmodified verify.sh in an isolated
# fixture, without the disk guard, Docker daemon or Rust toolchain.
set -euo pipefail

root=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
fixture=$(mktemp -d "${TMPDIR:-/tmp}/verify-timeouts.XXXXXX")
bin=$fixture/bin
tools=$fixture/tools
mkdir -p "$bin" "$tools" "$fixture/scripts"
cp -- "$root/scripts/verify.sh" "$fixture/scripts/verify.sh"

host_bash=$(type -P bash)
host_env=$(type -P env)
host_timeout=$(type -P timeout)
# Supply only the host utilities used by the entry point and stubs. Never
# fall back to a system search path for Docker, Cargo or any other command.
for tool in awk cksum date dirname sh sleep timeout; do
  tool_path=$(type -P "$tool")
  printf '#!/bin/bash\nexec %q "$@"\n' "$tool_path" > "$tools/$tool"
done
# On non-Windows hosts, the fixture does not need a real Windows mount path.
printf '#!/bin/bash\nprintf "%%s\\n" "$2"\n' > "$tools/cygpath"
chmod +x "$tools/"*

cat > "$bin/docker" <<'STUB'
#!/bin/bash
set -euo pipefail
deny() { printf 'denied %s\n' "$*" >> "$DOCKER_LOG"; exit 99; }
printf '%s\n' "$*" >> "$DOCKER_LOG"
case ${1:-} in
  info) [[ $# == 1 ]] || deny "$@" ;;
  run)
    shift
    name= label= auto_remove= init= step=
    until [[ ${1:-} == verify-timeout-fixture:fake ]]; do
      case ${1:-} in
        --rm) auto_remove=1; shift ;;
        --init) init=1; shift ;;
        --name) name=$2; shift 2 ;;
        --label) label=$2; shift 2 ;;
        -e)
          case $2 in
            MCPDB_STEP_TIMEOUT=*) step=${2#*=} ;;
            RUSTUP_TOOLCHAIN=*) : ;;
            *) deny "$@" ;;
          esac
          shift 2 ;;
        -v|-w) shift 2 ;;
        *) deny "$@" ;;
      esac
    done
    shift
    [[ $auto_remove && $init && $name == mcpdb-verify-* && $label == "mcpdb.verify.run=$name" ]] || deny 'run: wrong lifecycle flags'
    [[ $# == 3 && $1 == sh && $2 == -c && $step ]] || deny 'run: wrong entry point or missing step limit'
    printf '%s\n' "$name" > "$NAME_LOG"
    if [[ ${FAKE_RUN_RC:-0} != 0 ]]; then exit "$FAKE_RUN_RC"; fi
    if [[ ${FAKE_SLOW_RUN:-0} == 1 ]]; then exec sleep 30; fi
    export MCPDB_STEP_TIMEOUT=$step
    exec sh -c "$3"
    ;;
  build)
    [[ $# == 4 && $2 == -t && $3 == mcp-db:dev ]] || deny "$@"
    if [[ ${FAKE_BUILD_RC:-0} != 0 ]]; then exit "$FAKE_BUILD_RC"; fi
    if [[ ${FAKE_SLOW_BUILD:-0} == 1 ]]; then exec sleep 30; fi
    ;;
  ps)
    read -r name < "$NAME_LOG"
    [[ $# == 6 && $2 == -a && $3 == --filter && $4 == "label=mcpdb.verify.run=$name" && $5 == --format && $6 == '{{.Names}}' ]] || deny "$@"
    case ${FAKE_RESIDUE:-none} in
      own) printf '%s\n' "$name" ;;
      foreign) printf '%s\n' 'mcpdb-verify-foreign-fixture' ;;
      hang) exec sleep 30 ;;
    esac
    ;;
  *) deny "$@" ;;
esac
STUB

cat > "$bin/cargo" <<'STUB'
#!/bin/bash
set -eu
printf '%s\n' "$*" >> "$CARGO_LOG"
if [[ ${1:-} == "${FAKE_CARGO_STEP:-}" ]]; then
  if [[ ${FAKE_SLOW_CARGO:-0} == 1 ]]; then exec sleep 30; fi
  exit "${FAKE_CARGO_RC:-0}"
fi
STUB
printf '#!/bin/bash\nexit 0\n' > "$bin/rustup"
chmod +x "$bin/"*

passed=0
fail() {
  printf 'FAIL %s: %s\n%s\n%d passed, 1 failed\n' "$case_name" "$*" "$out" "$passed" >&2
  exit 1
}
contains() { [[ $out == *"$1"* ]] || fail "missing: $1"; }
excludes() { [[ $out != *"$1"* ]] || fail "unexpected: $1"; }
pass() { passed=$((passed + 1)); printf 'PASS %s (verify exit %s)\n' "$case_name" "$rc"; }

run_verify() {
  case_name=$1
  local want=$2 mode=$3 path=$bin:$tools
  shift 3
  : > "$fixture/docker.log"
  : > "$fixture/cargo.log"
  : > "$fixture/container.name"
  if [[ $mode == --help ]]; then path=$tools; fi
  rc=0
  out=$("$host_timeout" -k 1s 12s "$host_env" -i "PATH=$path" \
    "DOCKER_LOG=$fixture/docker.log" "CARGO_LOG=$fixture/cargo.log" \
    "NAME_LOG=$fixture/container.name" MCPDB_RUST_IMAGE=verify-timeout-fixture:fake \
    "$@" "$host_bash" "$fixture/scripts/verify.sh" "$mode" 2>&1) || rc=$?
  [[ $rc == "$want" ]] || fail "wanted exit $want, got $rc"
  if grep -q '^denied ' "$fixture/docker.log"; then fail 'unexpected Docker command or target'; fi
}

run_verify help 0 --help MCPDB_STEP_TIMEOUT=0
contains 'MCPDB_STEP_TIMEOUT (default 900s)'
contains 'MCPDB_GROUP_TIMEOUT (default 1800s)'
contains 'MCPDB_TIMEOUT'
contains 'Explicitly empty values and zero are rejected'
[[ ! -s $fixture/docker.log ]] || fail 'Docker was called for help'
pass

run_verify step-124 1 fast MCPDB_STEP_TIMEOUT=1 FAKE_CARGO_STEP=fmt FAKE_SLOW_CARGO=1
contains 'TIMEOUT fmt (limit 1s,'
excludes '== clippy =='
excludes '== test =='
[[ $(<"$fixture/cargo.log") == 'fmt --check' ]] || fail 'Cargo ran something other than fmt'
printf '%s\n' "$out"
pass

run_verify step-137 1 fast FAKE_CARGO_STEP=fmt FAKE_CARGO_RC=137
contains 'KILLED fmt (SIGKILL'
excludes 'TIMEOUT fmt'
excludes '== clippy =='
printf '%s\n' "$out"
pass

run_verify build-124 1 full MCPDB_STEP_TIMEOUT=0.05m FAKE_SLOW_BUILD=1
for step in fmt clippy test build; do contains "OK $step ("; done
contains 'TIMEOUT docker build (limit 0.05m,'
excludes 'OK docker build'
printf '%s\n' "$out"
pass

run_verify build-137 1 full FAKE_BUILD_RC=137
contains 'KILLED docker build (SIGKILL'
excludes 'TIMEOUT docker build'
pass

run_verify group-124 1 fast MCPDB_GROUP_TIMEOUT=1s MCPDB_TIMEOUT=0 FAKE_SLOW_RUN=1
contains 'FAILED fast (group timeout 1s)'
pass
run_verify group-alias 1 fast MCPDB_TIMEOUT=1 FAKE_SLOW_RUN=1
contains 'FAILED fast (group timeout 1s)'
pass
run_verify group-137 1 fast FAKE_RUN_RC=137
contains 'KILLED fast (SIGKILL'
excludes 'group timeout'
pass

run_verify residue 1 fast FAKE_RUN_RC=1 FAKE_RESIDUE=own
name=$(<"$fixture/container.name")
contains "WARN: container $name for this invocation still exists; left untouched."
grep -Fxq "ps -a --filter label=mcpdb.verify.run=$name --format {{.Names}}" "$fixture/docker.log" || fail 'missing scoped read-only lookup'
printf '%s\n' "$out"
pass
run_verify foreign 1 fast FAKE_RUN_RC=125 FAKE_RESIDUE=foreign
excludes 'WARN:'
excludes 'mcpdb-verify-foreign-fixture'
pass
run_verify residue-probe-timeout 1 fast FAKE_RUN_RC=1 FAKE_RESIDUE=hang
contains 'could not check whether container'
excludes 'still exists'
pass

for assignment in MCPDB_STEP_TIMEOUT={0,00,0s,0.0m,abc,1e-999s,1e999s,} \
  MCPDB_GROUP_TIMEOUT=0 MCPDB_GROUP_TIMEOUT= MCPDB_TIMEOUT=0 MCPDB_TIMEOUT=; do
  run_verify "invalid-$assignment" 2 fast "$assignment"
  contains "${assignment%%=*}="
  contains 'must be a finite positive duration'
  [[ ! -s $fixture/docker.log ]] || fail 'Docker was called before rejecting the limit'
  printf '%s\n' "$out"
  pass
done

run_verify defaults 0 full
contains 'OK docker build ('
pass
printf '\n%d passed, 0 failed\n' "$passed"
# Keep the small temporary fixture and its logs; the test does not delete files.
printf 'Fixture: %s\n' "$fixture"
