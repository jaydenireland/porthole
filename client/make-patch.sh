#!/bin/sh
# Rewrites tailscale-rs.patch from the changes in a tailscale-rs checkout.
#
#   ./make-patch.sh [path-to-checkout]   (default: ../tailscale-rs-src)
#
# The checkout must be the git clone build.sh made, so git knows what changed.
set -eu
HERE="$(cd "$(dirname "$0")" && pwd)"
SRC="${1:-$HERE/../tailscale-rs-src}"
git -C "$SRC" add -N .
# Cargo may touch tailscale-rs's own lock file; ours (client/Cargo.lock) is
# the one that decides versions, so theirs is left out.
git -C "$SRC" diff -- . ':(exclude)Cargo.lock' > "$HERE/tailscale-rs.patch"
echo "Wrote $HERE/tailscale-rs.patch ($(wc -l < "$HERE/tailscale-rs.patch" | tr -d ' ') lines)"
