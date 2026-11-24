#!/usr/bin/env bash
# Pre-push gate. Run this before every `git push`; it must exit 0 to proceed.
#
# Rationale (2026-11-24 review): 6 of 12 implementation commits were fixes to my
# own previous commit, and every one of them was discoverable locally before it
# reached CI. This script makes the missing gate explicit so the same classes of
# errors stop paying a full CI round-trip.
#
# Usage: scripts/preflight.sh
#   0  safe to push
#   1  blocking issue found; fix before pushing
#
# Gate logic is driven by which files are changing (committed or not):
#   *.sh                  -> bash -n, shellcheck when available
#   .github/workflows/*.yml -> actionlint (auto-provisioned into .tools/)
#   src/ tests/ Cargo*    -> requires Docker to run scripts/verify.sh fast
#   anything else         -> no gate (docs, TASK.md bookkeeping)
#
# Environment overrides (used by tests/preflight.test.sh):
#   PREFLIGHT_CHANGED  newline-separated file list; skips git detection
#   PREFLIGHT_BASE     base ref for git diff (default: origin/develop merge-base)
set -uo pipefail
export LC_ALL=C

ROOT=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
cd "$ROOT"
TOOLS="$ROOT/.tools"
mkdir -p "$TOOLS"

fails=0
warns=0
fail() { printf 'BLOCK %s\n' "$*"; fails=$((fails + 1)); }
warn() { printf 'WARN  %s\n' "$*"; warns=$((warns + 1)); }
ok()   { printf 'OK    %s\n' "$*"; }

# --- which files are in play -------------------------------------------------
if [ -n "${PREFLIGHT_CHANGED:-}" ]; then
  changed=$PREFLIGHT_CHANGED
else
  base=${PREFLIGHT_BASE:-}
  if [ -z "$base" ]; then
    base=$(git merge-base HEAD origin/develop 2>/dev/null || git rev-parse HEAD~1 2>/dev/null || echo '')
  fi
  if [ -z "$base" ]; then
    fail "cannot determine changed-file base; run from a git worktree"
  fi
  changed=$( { git diff --name-only "$base"..HEAD 2>/dev/null;
               git diff --name-only 2>/dev/null;
               git diff --cached --name-only 2>/dev/null; } | sort -u )
fi

if [ -z "$changed" ]; then
  ok "no changed files in scope"
  echo "preflight: OK ($warns warning(s))"
  exit 0
fi

printf 'changed files:\n%s\n\n' "$changed" | sed 's/^/  /'

has() { printf '%s\n' "$changed" | grep -Eq "$1"; }

# --- shell scripts: syntax always, shellcheck when a runner is available -----
if has '\.sh$'; then
  sh_files=$(printf '%s\n' "$changed" | grep '\.sh$' | grep -v '^\.tools/' || true)
  syntax_ok=1
  while IFS= read -r f; do
    [ -n "$f" ] || continue
    [ -f "$f" ] || continue
    if ! bash -n "$f" 2>/tmp/preflight-bashn.err; then
      fail "bash -n $f: $(head -1 /tmp/preflight-bashn.err)"
      syntax_ok=0
    fi
  done <<< "$sh_files"
  [ "$syntax_ok" = 1 ] && ok "bash -n ($(printf '%s\n' "$sh_files" | grep -c . ) script(s))"

  sc=''
  if command -v shellcheck >/dev/null 2>&1; then sc=shellcheck
  elif command -v timeout >/dev/null 2>&1 && command -v wsl >/dev/null 2>&1 \
    && timeout 15 wsl -e bash -lc 'command -v shellcheck' >/dev/null 2>&1; then
    sc='wsl -e shellcheck'
  elif command -v docker >/dev/null 2>&1 && timeout 20 docker info >/dev/null 2>&1; then
    sc='timeout 120 docker run --rm -i koalaman/shellcheck:stable'
  fi
  if [ -n "$sc" ]; then
    if $sc $sh_files >/tmp/preflight-sc.out 2>&1; then
      ok "shellcheck"
    else
      fail "shellcheck reported issues:"
      sed 's/^/       /' /tmp/preflight-sc.out | head -20
    fi
  else
    warn "shellcheck unavailable locally; CI still enforces it. provision: wsl -e sudo apt install shellcheck"
  fi
fi

# --- workflow files: actionlint ---------------------------------------------
if has '^\.github/workflows/.+\.ya?ml$'; then
  lint=''
  probe_lint() {
    # A truncated or wrong-platform binary from an interrupted download passes
    # -x but fails to run; treat that as "absent" so we re-provision instead of
    # reporting a bogus actionlint failure.
    [ -n "$1" ] || return 1
    timeout 15 "$1" -version >/dev/null 2>&1
  }
  if command -v actionlint >/dev/null 2>&1 && probe_lint actionlint; then lint=actionlint
  elif probe_lint "$TOOLS/actionlint.exe"; then lint="$TOOLS/actionlint.exe"
  elif probe_lint "$TOOLS/actionlint"; then lint="$TOOLS/actionlint"
  else
    # Drop unusable binaries, then extract from any cached archive before
    # trying the network.
    rm -f "$TOOLS/actionlint.exe" "$TOOLS/actionlint"
    ( cd "$TOOLS" && { unzip -o -q actionlint_*.zip 2>/dev/null || tar xzf actionlint_*.tar.gz 2>/dev/null; } ) || true
    probe_lint "$TOOLS/actionlint.exe" && lint="$TOOLS/actionlint.exe"
    [ -z "$lint" ] && probe_lint "$TOOLS/actionlint" && lint="$TOOLS/actionlint"
  fi
  if [ -z "$lint" ] && command -v gh >/dev/null 2>&1; then
    echo "provisioning actionlint into .tools/ ..."
    ( cd "$TOOLS" && timeout 180 gh release download --repo rhysd/actionlint \
        --pattern "*windows_amd64.zip" --pattern "*linux_x86_64.tar.gz" --clobber >/dev/null 2>&1 \
      && { unzip -o -q actionlint_*.zip 2>/dev/null || tar xzf actionlint_*.tar.gz 2>/dev/null; } ) || true
    probe_lint "$TOOLS/actionlint.exe" && lint="$TOOLS/actionlint.exe"
    [ -z "$lint" ] && probe_lint "$TOOLS/actionlint" && lint="$TOOLS/actionlint"
  fi
  if [ -n "$lint" ]; then
    if "$lint" >/tmp/preflight-al.out 2>&1; then
      ok "actionlint"
    else
      fail "actionlint reported issues:"
      sed 's/^/       /' /tmp/preflight-al.out | head -20
    fi
  else
    warn "actionlint unavailable; provision with: gh release download --repo rhysd/actionlint --pattern '*windows_amd64.zip' -D .tools"
  fi
fi

# --- Rust source: must be verified locally, CI is not a compiler ------------
# tests/*.test.sh are shell mocks and are covered by the shell gate above; only
# tests/*.rs compile with the crate.
need_rust=0
need_image=0
has '^(src/|tests/[^/]*\.rs$|Cargo\.toml|Cargo\.lock|rust-toolchain\.toml)' && need_rust=1
has '^(Dockerfile|\.dockerignore)' && need_image=1

if [ "$need_rust" = 1 ] || [ "$need_image" = 1 ]; then
  if command -v docker >/dev/null 2>&1 && timeout 20 docker info >/dev/null 2>&1; then
    mode=fast
    [ "$need_image" = 1 ] && mode=full
    if bash scripts/verify.sh "$mode" >/tmp/preflight-verify.out 2>&1; then
      ok "scripts/verify.sh $mode (fmt + clippy + locked tests$([ "$need_image" = 1 ] && echo ' + release build + docker build'))"
    else
      fail "scripts/verify.sh $mode failed:"
      tail -15 /tmp/preflight-verify.out | sed 's/^/       /'
    fi
  else
    # This is the exact gap that produced the HeaderName compile failure: local
    # verification was impossible, so the change was pushed and CI discovered
    # the error at the cost of a full round-trip.
    fail "Docker is unavailable but Rust/image files changed. Start Docker Desktop (or fix the daemon) and rerun; do not push unverified source and let CI compile it."
  fi
fi

echo
if [ "$fails" -gt 0 ]; then
  echo "preflight: $fails blocking issue(s), $warns warning(s) — do not push"
  exit 1
fi
echo "preflight: OK ($warns warning(s))"
