# Porthole

Browse a Tailscale network from a browser tab, on a computer where Tailscale
is not installed. The tab runs Tailscale's own client compiled to WebAssembly,
joins the tailnet as a temporary device, and shows tailnet web pages in a
frame. The site is static files: tailnet traffic goes from the tab to
Tailscale and never through a server of ours.

Porthole is an independent project and is not affiliated with Tailscale Inc.

## Layout

| Folder | What is in it |
|---|---|
| `web/` | The site: landing page, the app, the service worker, tests. See `web/README.md`. |
| `cloudflare/` | Worker, config and scripts that publish `web/` to `porthole.app`. See `cloudflare/README.md`. |
| `tailscale-src/` | Not in the repository. `web/build.sh` fetches Tailscale's source here and applies `web/tsconnect.patch`. |

## Get it running

You need Go, git, Python 3 and Node.

1. Build the Tailscale client. The first run fetches Tailscale's source.

       web/build.sh

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
| Client start | Open `/test/client.html`. It starts the client and asks Tailscale for a sign-in address, without signing in. |
| Go code added to the client | `RUN_TESTS=1 web/build.sh` |

Add `?report` to either page to have it send its result to the server log,
which helps on a device with no way to read the screen closely.

## Publish

    cloudflare/deploy.sh

`cloudflare/README.md` has the one-time setup.

## Supported browsers

Chrome and Edge 84, Firefox 90, Safari 16. `web/compatibility/index.html` has
the full list and says how each entry was checked.
