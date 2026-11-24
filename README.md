# Porthole

Browse a Tailscale network from a browser tab, on a computer where Tailscale
is not installed. The tab runs a Tailscale client compiled to WebAssembly,
joins the tailnet as a temporary device, and shows tailnet web pages in a
frame. The site is static files: tailnet traffic goes from the tab to
Tailscale and never through a server of ours.

Porthole is an independent project and is not affiliated with Tailscale Inc.

## Layout

| Folder | What is in it |
|---|---|
| `web/` | The site: landing page, the app, the service worker, tests. See `web/README.md`. |
| `cloudflare/` | Worker, config and scripts that publish `web/` to `porthole.app`. See `cloudflare/README.md`. |
| `client/` | The Tailscale client, in Rust on Tailscale's tailscale-rs crates. See `client/README.md`. |
| `tailscale-rs-src/` | Not in the repository. `client/build.sh` fetches tailscale-rs here and applies `client/tailscale-rs.patch`. |

## Get it running

You need Rust with the `wasm32-unknown-unknown` target, wasm-bindgen's
command-line tool, LLVM's clang, Binaryen, git, Python 3 and Node.
`client/build.sh` checks for each and says how to install what is missing.

1. Build the Tailscale client. The first run fetches tailscale-rs.

       client/build.sh

2. Serve the site.

       ./serve.py

   This is Python's file server with browser caching turned off, so edits
   show up on reload.

3. Open `http://localhost:8791`. The app is at `/app/`.

Each tailnet site is shown on its own subdomain, such as
`nas--h.localhost:8791`. Chrome, Firefox and Safari resolve these on their own.

## Tests

| Test | How to run it |
|---|---|
| Page loading, cookies, WebSockets, streaming | Open `/test/mock.html`. It uses stand-in sites and needs no tailnet. |
| The client: control server, relay, WireGuard, TCP, HTTP, TLS, WebSockets | Open `/test/client.html`. It asks Tailscale for a sign-in address and runs the rest between two throwaway devices, without signing in. |
| The client against your tailnet | Open `/test/client-signin.html`, sign in, and connect to a device. |
| Client code (29 tests) | `RUN_TESTS=1 client/build.sh` |

Add `?report` to the first two pages to have it send its result to the server log,
which helps on a device with no way to read the screen closely.

## Publish

    cloudflare/deploy.sh

`cloudflare/README.md` has the one-time setup.

## Supported browsers

Chrome and Edge 84, Firefox 90, Safari 16. `web/compatibility/index.html` has
the full list and says how each entry was checked.
