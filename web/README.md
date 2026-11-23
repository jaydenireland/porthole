# Porthole

Browse a Tailscale network from a browser tab. The tab runs Tailscale's client
compiled to WebAssembly, joins the tailnet as an ephemeral device, and shows
tailnet sites in a frame. Everything here is static files; there is no server
component.

## Files

| File | What it does |
|---|---|
| `index.html` | the landing page. |
| `app/index.html` | the browser: sign-in, device list, address bar, frame. |
| `app/bridge.js` | app side: one hidden relay frame per tailnet site; answers the service workers' requests with the Tailscale client. |
| `assets/` | shared styles and the icon. |
| `sw.js` | service worker that runs on each site's subdomain and serves its requests through the client. It must stay at the root. |
| `__tailnet/codec.js` | maps a tailnet origin to a subdomain label and back. |
| `__tailnet/relay.html` | hidden page on each site's subdomain; installs the worker there and passes its requests up to the app. |
| `__tailnet/page.js` | added to every tailnet page by the worker; routes `WebSocket` and `document.cookie` through the client. |
| `node/main.wasm`, `node/wasm_exec.js` | the built client and Go's loader. |
| `tsconnect.patch` | our changes to Tailscale's `cmd/tsconnect/wasm`. |
| `make-patch.sh` | rewrites `tsconnect.patch` from the changes in the Tailscale checkout. |
| `build.sh` | rebuilds `node/` from a patched Tailscale checkout. |
| `compatibility/index.html` | the supported browsers and devices page. |
| `test/client.html` | starts the client in the current browser and asks Tailscale for a sign-in address, without signing in. |
| `test/mock.html` | tests everything except the client itself, against fake sites; needs no tailnet. |

## How a site is shown

`http://nas.tail1a2b3c.ts.net` is shown on
`http://nas--tail1a2b3c--ts--net--h.localhost:8791`: dots become `--`,
then `h` or `s` for the scheme plus any port. Each site therefore sits at `/`
of its own origin, isolated from other sites and from the app.

## Run

Serve this folder from the root of an origin, for example:

    ../serve.py

and open `http://localhost:8791` (the app itself is at `/app/`). The same files must be served on every
subdomain of that host. Locally this relies on the browser resolving
`*.localhost` to the loopback address (Chromium and Firefox do). Hosted, it
needs a wildcard DNS record and a wildcard certificate for `*.<app host>`.

Open `/test/mock.html` to run the tests.

## Rebuild the node

`tsconnect.patch` adds to Tailscale's browser client:

- `roots_js.go`: bundles the Mozilla root CAs (Go on js/wasm has no system
  certificate store, so HTTPS verification fails without it).
- `request_js.go`: `ipn.request({url, method, headers, body})`, with no
  automatic redirects; resolves at the headers and hands back a chunked body
  reader, so streamed responses work.
- `cookies_js.go`: the in-memory cookie jar shared by requests and sockets,
  split so HttpOnly cookies stay hidden from pages; `ipn.cookies(url)` and
  `ipn.setCookie(url, line)` back `document.cookie`.
- `wasm_js.go`: one added setting, `ephemeral: false`, which registers a
  normal device for the "Stay signed in" option. The default stays ephemeral.
- `websocket_js.go`: `ipn.websocket({url, protocols, headers, on...})`,
  a WebSocket client over the tailnet that shares the cookie jar.

From a checkout of https://github.com/tailscale/tailscale (made against
commit 66e3d82):

    git apply /path/to/web/tsconnect.patch
    RUN_TESTS=1 /path/to/web/build.sh "$PWD"

`wasm_exec.js` must come from the same Go version that built `main.wasm`;
`build.sh` copies it for you.

## Checking a browser

Open `/test/mock.html?report` and `/test/client.html?report` in the browser.
With `?report`, each page sends its result to `/test/report?...`, which shows
up in the static server's request log. The minimum versions on the
compatibility page come from MDN's browser compatibility data for the features
the code uses, plus a scan of `node/main.wasm` for post-MVP instructions
(bulk memory, sign extension and saturating conversions are present).
