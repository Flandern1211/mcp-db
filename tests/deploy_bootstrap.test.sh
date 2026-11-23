#!/usr/bin/env bash
# Mock regression test for deploy/bootstrap.sh key-handling safety.
#
# Root-only actions are isolated with stubs: `id` reports root, `chown` records
# calls and changes nothing. Key files, data and state live in a temporary
# directory. The test asserts that an existing key is never rewritten (empty or
# malformed included), that a new key is refused when state already exists, and
# that a fresh directory gets a 64-hex key.
set -euo pipefail

ROOT=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)

work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT

bin="$work/bin"
app="$work/app"
mkdir -p "$bin" "$app"
cp "$ROOT/deploy/bootstrap.sh" "$app/bootstrap.sh"

cat > "$bin/id" <<'STUB'
#!/usr/bin/env bash
printf '%s\n' "${STUB_UID:-0}"
STUB
cat > "$bin/chown" <<'STUB'
#!/usr/bin/env bash
printf '%s\n' "$*" >> "$STUB_CHOWN_LOG"
STUB
chmod +x "$bin/id" "$bin/chown"
export PATH="$bin:$PATH"
export STUB_UID=0
export STUB_CHOWN_LOG="$work/chown.log"

failures=0
pass() { printf 'PASS %s\n' "$1"; }
fail_case() { printf 'FAIL %s\n' "$1" >&2; failures=$((failures + 1)); }

reset_case() {
  rm -rf "${app:?}/keys" "${app:?}/data"
  : > "$STUB_CHOWN_LOG"
  mkdir -p "$app/keys" "$app/data"
}

run_bootstrap() {
  set +e
  bash "$app/bootstrap.sh" >"$work/out" 2>"$work/err"
  rc=$?
  set -e
}

# 1. Fresh directory: creates a 64-hex key.
reset_case
run_bootstrap
if [ "$rc" -ne 0 ]; then
  fail_case "fresh bootstrap exited $rc: $(cat "$work/err")"
elif [ ! -f "$app/keys/master.key" ]; then
  fail_case "fresh bootstrap did not create keys/master.key"
elif ! grep -qx '[0-9a-fA-F]\{64\}' <<<"$(cat "$app/keys/master.key")"; then
  fail_case "fresh bootstrap key is not 64 hex characters"
elif ! grep -q '^10001:10001 keys/master.key$' "$STUB_CHOWN_LOG"; then
  fail_case "fresh bootstrap did not set key ownership"
elif grep -q '^10001:10001 \.' "$STUB_CHOWN_LOG"; then
  fail_case "fresh bootstrap changed ownership recursively"
else
  pass "fresh: 64-hex key created, ownership set on key and data only"
fi

# 2. Second run reuses the key byte-for-byte.
before=$(cksum < "$app/keys/master.key")
run_bootstrap
after=$(cksum < "$app/keys/master.key")
if [ "$rc" -ne 0 ]; then
  fail_case "rerun exited $rc"
elif [ "$before" != "$after" ]; then
  fail_case "rerun rewrote the existing key"
else
  pass "rerun: existing key reused"
fi

# 3. Empty existing key is never overwritten.
reset_case
: > "$app/keys/master.key"
run_bootstrap
if [ "$rc" -eq 0 ]; then
  fail_case "empty existing key was accepted"
elif [ -s "$app/keys/master.key" ]; then
  fail_case "empty existing key was overwritten"
else
  pass "empty existing key: refused, not overwritten"
fi

# 4. Malformed existing key is never overwritten.
reset_case
printf 'not-a-key' > "$app/keys/master.key"
run_bootstrap
if [ "$rc" -eq 0 ]; then
  fail_case "malformed existing key was accepted"
elif [ "$(cat "$app/keys/master.key")" != 'not-a-key' ]; then
  fail_case "malformed existing key was overwritten"
else
  pass "malformed existing key: refused, not overwritten"
fi

# 5. Valid base64 key is reused, not rewritten.
reset_case
printf '01234567890123456789012345678901' | base64 | tr -d '\n' > "$app/keys/master.key"
before=$(cksum < "$app/keys/master.key")
run_bootstrap
after=$(cksum < "$app/keys/master.key")
if [ "$rc" -ne 0 ]; then
  fail_case "valid base64 key exited $rc: $(cat "$work/err")"
elif [ "$before" != "$after" ]; then
  fail_case "valid base64 key was rewritten"
else
  pass "valid base64 key: reused"
fi

# 6. Multiline key is refused.
reset_case
printf '%064d\n%064d\n' 0 0 | tr '0' 'a' > "$app/keys/master.key"
before=$(cksum < "$app/keys/master.key")
run_bootstrap
after=$(cksum < "$app/keys/master.key")
if [ "$rc" -eq 0 ]; then
  fail_case "multiline key was accepted"
elif [ "$before" != "$after" ]; then
  fail_case "multiline key was rewritten"
else
  pass "multiline key: refused, not overwritten"
fi

# 7. Missing key with initialized state is refused.
for state_file in state.db credentials.enc; do
  reset_case
  rm -f "$app/keys/master.key"
  printf 'x' > "$app/data/$state_file"
  run_bootstrap
  if [ "$rc" -eq 0 ]; then
    fail_case "missing key with $state_file was accepted"
  elif [ -e "$app/keys/master.key" ]; then
    fail_case "missing key with $state_file generated a new key"
  else
    pass "missing key with $state_file: refused, no new key"
  fi
done

# 8. Non-root invocation does nothing.
reset_case
rm -f "$app/keys/master.key"
STUB_UID=1000 run_bootstrap
if [ "$rc" -eq 0 ]; then
  fail_case "non-root bootstrap was accepted"
elif [ -e "$app/keys/master.key" ]; then
  fail_case "non-root bootstrap created a key"
else
  pass "non-root: refused, no key created"
fi
STUB_UID=0

# 9. Symlink key is refused (skipped where the filesystem cannot create one).
reset_case
rm -f "$app/keys/master.key"
if ln -s "$work/outside.key" "$app/keys/master.key" 2>/dev/null; then
  run_bootstrap
  if [ "$rc" -eq 0 ]; then
    fail_case "symlink key was accepted"
  else
    pass "symlink key: refused"
  fi
  rm -f "$app/keys/master.key"
else
  printf 'SKIP symlink key: cannot create symlink here\n'
fi

if [ "$failures" -ne 0 ]; then
  printf '%d bootstrap case(s) failed\n' "$failures" >&2
  exit 1
fi
printf 'bootstrap: all cases passed\n'
