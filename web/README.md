# Porthole

Browse a Tailscale network from a browser tab. The tab runs a Tailscale client
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
| `__tailnet/page.js` | added to every tailnet page by the worker; routes `WebSocket` and `document.cookie` through the client, and tells the app the page's title and its address when a script or a `#` link changes it; applies the site's own rules about frames (`X-Frame-Options`, `frame-ancestors`), which the worker has to drop. |
| `app/client.js` | loads the Tailscale client and reports its progress to the app as a state. |
| `client/` | the built client. Not in the repository; `../client/build.sh` writes it. |
| `test/client/` | the same with logging and the test pages' checks. The app loads it when opened with `?debug`. |
| `compatibility/index.html` | the supported browsers and devices page. |
| `legal/index.html` | terms of use, privacy notes and licences. |
| `test/client.html` | tests the client in the current browser without signing in: a sign-in address from Tailscale, then requests between two throwaway devices. |
| `test/client-signin.html` | signs in to your tailnet and connects to a device you pick. |
| `test/sockets.html` | opens each connection the client needs and says which ones fail. |
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

## The client

The client is built from `../client`; see its README. `../client/build.sh`
writes it to `client/` and `test/client/`.

## Checking a browser

Open `/test/mock.html?report` and `/test/client.html?report` in the browser.
With `?report`, each page sends its result to `/test/report?...`, which shows
up in the static server's request log. The minimum versions on the
compatibility page come from MDN's browser compatibility data for the features
the code uses. For the client's WebAssembly those are bulk memory, sign
extension and saturating conversions; `../client/build.sh` fails if the
client needs any other.
