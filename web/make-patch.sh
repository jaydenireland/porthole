#!/bin/sh
# Rewrites tsconnect.patch from the changes in a Tailscale checkout.
#
#   ./make-patch.sh [path-to-tailscale-checkout]   (default: ../tailscale-src)
#
# Fetches an untouched copy of Tailscale at the pinned commit, lays the
# checkout's cmd/tsconnect/wasm files over it, and saves the difference.
set -eu
HERE="$(cd "$(dirname "$0")" && pwd)"
SRC="${1:-$HERE/../tailscale-src}"
COMMIT="$(sed -n 's/^TAILSCALE_COMMIT=//p' "$HERE/build.sh")"

TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT
git init -q "$TMP"
git -C "$TMP" remote add origin https://github.com/tailscale/tailscale.git
git -C "$TMP" fetch -q --depth 1 origin "$COMMIT"
git -C "$TMP" checkout -q FETCH_HEAD
cp "$SRC"/cmd/tsconnect/wasm/*.go "$TMP/cmd/tsconnect/wasm/"
git -C "$TMP" add -N cmd/tsconnect/wasm
git -C "$TMP" diff > "$HERE/tsconnect.patch"
echo "Wrote $HERE/tsconnect.patch ($(wc -l < "$HERE/tsconnect.patch" | tr -d ' ') lines)"
