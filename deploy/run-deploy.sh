#!/usr/bin/env bash
# SSH forced command for the GitHub Actions deploy key.
#
# The key is installed with an admin-owned entry such as:
#   restrict,command="/path/to/mcp-db/run-deploy.sh" ssh-ed25519 ...
#
# It accepts exactly one request, as one line with no extra arguments:
#   deploy ghcr.io/flandern1211/mcp-db@sha256:<64 lowercase hex>
#
# Tags, other repositories, extra arguments, whitespace variants, multiline
# input and shell syntax are rejected before anything else runs. No shell is
# interpreted, nothing is uploaded and no command from the request is executed:
# only the validated image reference is passed as a single argument to
# deploy.sh. Host configuration (compose file, key, data) stays admin-owned.
set -euo pipefail

DIR=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
readonly DIR
readonly EXPECTED='^deploy ghcr\.io/flandern1211/mcp-db@sha256:[0-9a-f]{64}$'

request=${SSH_ORIGINAL_COMMAND:-}

if [[ ! $request =~ $EXPECTED ]]; then
  echo "run-deploy: rejected request; expected 'deploy ghcr.io/flandern1211/mcp-db@sha256:<64 lowercase hex>'" >&2
  exit 2
fi

image_ref=${request#deploy }
[ -f "$DIR/deploy.sh" ] && [ ! -L "$DIR/deploy.sh" ] ||
  { echo "run-deploy: deploy.sh is missing or a symlink" >&2; exit 1; }
exec bash "$DIR/deploy.sh" "$image_ref"
