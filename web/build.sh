#!/bin/sh
# Builds the Tailscale client for the browser: node/main.wasm and
# node/wasm_exec.js.
#
#   ./build.sh                 use ../tailscale-src, fetching it if missing
#   ./build.sh /path/to/src    use an existing patched Tailscale checkout
#   RUN_TESTS=1 ./build.sh     also run the Go tests under Node first
#
# Needs Go and git. Go downloads the toolchain version Tailscale asks for.
set -eu
HERE="$(cd "$(dirname "$0")" && pwd)"
SRC="${1:-$HERE/../tailscale-src}"

# The Tailscale commit tsconnect.patch was written against.
TAILSCALE_COMMIT=66e3d823c754fd4dd87b95785568c76869e5a786

# Fetch only if the source is not there; a folder that already holds it is
# taken to be patched.
if [ ! -d "$SRC/cmd/tsconnect" ]; then
  echo "Fetching Tailscale at $TAILSCALE_COMMIT into $SRC"
  git init -q "$SRC"
  git -C "$SRC" remote add origin https://github.com/tailscale/tailscale.git
  git -C "$SRC" fetch -q --depth 1 origin "$TAILSCALE_COMMIT"
  git -C "$SRC" checkout -q FETCH_HEAD
  git -C "$SRC" apply "$HERE/tsconnect.patch"
fi
cd "$SRC"

# The build tags are computed by Tailscale's own wasmbuild package.
TMP=cmd/tsconnect/zz_printtags
trap 'rm -rf "$TMP"' EXIT
mkdir -p "$TMP"
cat > "$TMP/main.go" <<'GO'
package main

import (
	"fmt"

	"tailscale.com/cmd/tsconnect/wasmbuild"
)

func main() { fmt.Print(wasmbuild.Tags()) }
GO
TAGS="$(go run "./$TMP")"

if [ "${RUN_TESTS:-}" != "" ]; then
  GOOS=js GOARCH=wasm go test -tags "$TAGS" \
    -exec "$(go env GOROOT)/lib/wasm/go_js_wasm_exec" \
    -run 'TestDoRequest|TestOpenWebSocket|TestSplitJar|TestCookieExportImport' -v ./cmd/tsconnect/wasm
fi

mkdir -p "$HERE/node"
GOOS=js GOARCH=wasm go build -tags "$TAGS" -trimpath -ldflags "-s -w" \
  -o "$HERE/node/main.wasm" ./cmd/tsconnect/wasm
cp -f "$(go env GOROOT)/lib/wasm/wasm_exec.js" "$HERE/node/wasm_exec.js"
ls -l "$HERE/node"
