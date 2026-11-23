#!/bin/sh
# Builds the Rust Tailscale client for the browser into ../web/client/:
# porthole_client_bg.wasm and porthole_client.js.
#
#   ./build.sh                 use ../tailscale-rs-src, fetching it if missing
#   ./build.sh /path/to/src    use an existing patched tailscale-rs checkout
#   RUN_TESTS=1 ./build.sh     also run the tests first
#
# Needs git, Rust with the wasm32-unknown-unknown target
# (rustup target add wasm32-unknown-unknown), and wasm-bindgen's command-line
# tool at the version in Cargo.lock; the script says how to install it.
set -eu
HERE="$(cd "$(dirname "$0")" && pwd)"
DEFAULT_SRC="$HERE/../tailscale-rs-src"
SRC="${1:-$DEFAULT_SRC}"

# The tailscale-rs commit tailscale-rs.patch was written against (v0.7.1).
TAILSCALE_RS_COMMIT=a2a56c41b1db72d50d59d71d35c6669ff9639d14

# Fetch only if the source is not there; a folder that already holds it is
# taken to be patched.
if [ ! -d "$SRC/crates" ]; then
  echo "Fetching tailscale-rs at $TAILSCALE_RS_COMMIT into $SRC"
  git init -q "$SRC"
  git -C "$SRC" remote add origin https://github.com/tailscale/tailscale-rs.git
  git -C "$SRC" fetch -q --depth 1 origin "$TAILSCALE_RS_COMMIT"
  git -C "$SRC" checkout -q FETCH_HEAD
  git -C "$SRC" apply "$HERE/tailscale-rs.patch"
fi

# Cargo.toml points at ../tailscale-rs-src, so another checkout is linked there.
if [ ! -e "$DEFAULT_SRC" ]; then
  ln -s "$(cd "$SRC" && pwd)" "$DEFAULT_SRC"
elif [ "$(cd "$SRC" && pwd -P)" != "$(cd "$DEFAULT_SRC" && pwd -P)" ]; then
  echo "$DEFAULT_SRC already exists and is a different checkout; remove it first." >&2
  exit 1
fi

cd "$HERE"

WANT="$(awk '/^name = "wasm-bindgen"$/ { getline; gsub(/[^0-9.]/, ""); print; exit }' Cargo.lock)"
HAVE="$(wasm-bindgen --version 2>/dev/null | awk '{ print $2 }' || true)"
if [ "$HAVE" != "$WANT" ]; then
  echo "wasm-bindgen $WANT is needed (found: ${HAVE:-none}). Install it with:" >&2
  echo "  cargo install wasm-bindgen-cli --version $WANT --locked" >&2
  exit 1
fi

if [ "${RUN_TESTS:-}" != "" ]; then
  # The tests cover the parts that need no browser, so they run on this machine.
  cargo test --target "$(rustc -vV | sed -n 's/^host: //p')"
fi

cargo build --release --locked
OUT="$HERE/../web/client"
rm -rf "$OUT"
wasm-bindgen --target web --no-typescript --out-dir "$OUT" --out-name porthole_client \
  target/wasm32-unknown-unknown/release/porthole_client.wasm

# Optional: Binaryen's wasm-opt makes the file smaller still.
if command -v wasm-opt >/dev/null 2>&1; then
  wasm-opt -Oz --strip-debug -o "$OUT/porthole_client_bg.wasm" "$OUT/porthole_client_bg.wasm"
fi

ls -l "$OUT"
