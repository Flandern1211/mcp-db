#!/usr/bin/env bash
# Idempotent, root-only preparation of the deployment directory:
#   keys/master.key  32-byte master key, mode 0400, uid 10001
#   keys/            mode 0755
#   data/            mode 0700, uid 10001, kept across upgrades
#
# Run once on the host as root:
#   sudo bash bootstrap.sh
#
# Safety rules:
#   - never generates or rewrites a key when one already exists, including an
#     empty or malformed one: an operator must restore the correct key;
#   - refuses to generate a new key when state (state.db / credentials.enc)
#     already exists, because the old encrypted data would become unreadable;
#   - refuses symlinks;
#   - changes ownership only on the named key/data paths, never recursively;
#   - prints paths and results only, never key material or file contents.
set -euo pipefail

DIR=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
readonly DIR
cd "$DIR"

fail() { printf 'bootstrap: %s\n' "$*" >&2; exit 1; }
say() { printf 'bootstrap: %s\n' "$*"; }

if [ "$(id -u)" -ne 0 ]; then
  fail "run as root (sudo bash $0)"
fi

for path in keys keys/master.key data; do
  [ -L "$path" ] && fail "refusing symlink at $path"
done

mkdir -p keys data
[ -d keys ] || fail "keys is not a directory"
[ -d data ] || fail "data is not a directory"

state_initialized=0
if [ -s data/state.db ] || [ -s data/credentials.enc ]; then
  state_initialized=1
fi

key_exists=0
if [ -e keys/master.key ]; then
  key_exists=1
  [ -f keys/master.key ] || fail "keys/master.key is not a regular file"
  [ -s keys/master.key ] || fail "existing keys/master.key is empty; refusing to overwrite it. Restore the original key from backup."
  [ "$(wc -l < keys/master.key)" -le 1 ] ||
    fail "existing keys/master.key spans multiple lines; refusing to overwrite it. Restore the original key from backup."
  key_text=$(tr -d '\r\n' < keys/master.key)
  key_ok=0
  if [[ $key_text =~ ^[0-9a-fA-F]{64}$ ]]; then
    key_ok=1
  else
    # Accept the same base64 forms as the service, including URL-safe and
    # unpadded input, without printing the key.
    normalized=$(printf '%s' "$key_text" | tr '_-' '/+')
    case $(( ${#normalized} % 4 )) in
      2) normalized+='==' ;;
      3) normalized+='=' ;;
    esac
    if [ "$(printf '%s' "$normalized" | base64 -d 2>/dev/null | wc -c)" -eq 32 ]; then
      key_ok=1
    fi
  fi
  [ "$key_ok" -eq 1 ] ||
    fail "existing keys/master.key is not a 32-byte hex or base64 key; refusing to overwrite it. Restore the original key from backup."
  say "reusing existing keys/master.key (contents not shown)"
fi

if [ "$key_exists" -eq 0 ]; then
  if [ "$state_initialized" -eq 1 ]; then
    fail "state exists in data/ but keys/master.key is missing; restore the key from backup instead of generating a new one"
  fi
  (
    umask 077
    head -c 32 /dev/urandom | od -An -tx1 | tr -d ' \n' > keys/master.key
  )
  say "created keys/master.key; back it up together with data/"
fi

chmod 755 keys
chmod 400 keys/master.key
chmod 700 data
chown 10001:10001 keys/master.key
chown 10001:10001 data

for state_file in data/state.db data/credentials.enc; do
  [ -e "$state_file" ] || continue
  [ -L "$state_file" ] && fail "refusing symlink at $state_file"
  [ -f "$state_file" ] || fail "$state_file is not a regular file"
  chown 10001:10001 "$state_file"
done

say "ok ($DIR)"
