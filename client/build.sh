#!/bin/sh
# Builds the Tailscale client for the browser: porthole_client_bg.wasm and
# porthole_client.js, into ../web/client/ for the app and, with the checks
# and logging added, into ../web/test/client/ for the test pages.
#
#   ./build.sh                 use ../tailscale-rs-src, fetching it if missing
#   ./build.sh /path/to/src    use an existing patched tailscale-rs checkout
#   RUN_TESTS=1 ./build.sh     also run the tests first
#
# Needs:
# - git and Python 3;
# - Rust with the wasm32-unknown-unknown target
#   (rustup target add wasm32-unknown-unknown);
# - wasm-bindgen's command-line tool at the version in Cargo.lock; the script
#   says how to install it;
# - Binaryen's wasm-opt, which makes the result smaller and checks which
#   WebAssembly features it uses (brew install binaryen);
# - a clang that can compile for WebAssembly, for the C parts of the TLS
#   crypto library (ring). Apple's cannot; Homebrew's can (brew install llvm).
#   Set CC_wasm32_unknown_unknown and AR_wasm32_unknown_unknown to use another.
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

# Find a clang that can target WebAssembly, unless one was named.
if [ -z "${CC_wasm32_unknown_unknown:-}" ]; then
  for LLVM in /opt/homebrew/opt/llvm/bin /usr/local/opt/llvm/bin; do
    if [ -x "$LLVM/clang" ]; then
      export CC_wasm32_unknown_unknown="$LLVM/clang"
      export AR_wasm32_unknown_unknown="$LLVM/llvm-ar"
      break
    fi
  done
fi
CLANG="${CC_wasm32_unknown_unknown:-clang}"
if ! echo 'int x;' | "$CLANG" --target=wasm32-unknown-unknown -c -x c - -o /dev/null 2>/dev/null; then
  echo "$CLANG cannot compile for WebAssembly. Install one that can:" >&2
  echo "  brew install llvm" >&2
  exit 1
fi

if [ "${RUN_TESTS:-}" != "" ]; then
  # The tests cover the parts that need no browser, so they run on this machine.
  cargo test --target "$(rustc -vV | sed -n 's/^host: //p')"
fi

# The WebAssembly features the client may use, the same three the browsers
# on the compatibility page are judged by. wasm-opt refuses anything else,
# so a change that would raise the oldest supported browser fails here.
WASM_FEATURES="--mvp-features --enable-bulk-memory --enable-sign-ext --enable-nontrapping-float-to-int --enable-mutable-globals"
if ! command -v wasm-opt >/dev/null 2>&1; then
  echo "Binaryen's wasm-opt is needed. Install it with:" >&2
  echo "  brew install binaryen" >&2
  exit 1
fi

# Two builds. The app's has no checks and no logging, to keep the download
# small. The test pages' has both; the app loads it too when opened with
# ?debug.
bundle() {  # bundle <out dir> [cargo arguments]
  OUT="$1"; shift
  RAW=target/wasm32-unknown-unknown/release/porthole_client.wasm
  cargo build --release --locked "$@"
  # Rust's standard library is built with reference types and multi-value
  # switched on and says so in the file, although no code here uses them.
  # wasm-bindgen would start using them on seeing that note, so remove it.
  wasm-opt $WASM_FEATURES --enable-reference-types --enable-multivalue \
    --strip-target-features -o target/porthole_client.wasm "$RAW"
  rm -rf "$OUT"
  # (wasm-bindgen warns that the note is missing; that is intended.)
  wasm-bindgen --target web --no-typescript --out-dir "$OUT" --out-name porthole_client \
    target/porthole_client.wasm
  wasm-opt -Oz --strip-debug --strip-producers $WASM_FEATURES \
    -o "$OUT/porthole_client_bg.wasm" "$OUT/porthole_client_bg.wasm"
}

bundle "$HERE/../web/client"
bundle "$HERE/../web/test/client" --features console-log,diagnostics

# The licences require their notices to go out with the compiled client, so
# the list is rebuilt from Cargo.lock on every build and published beside it.
"$HERE/make-notices.py" "$HERE/../web/client/THIRD-PARTY-LICENSES.txt"

ls -l "$HERE/../web/client" "$HERE/../web/test/client"
