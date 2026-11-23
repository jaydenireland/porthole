# Porthole on Cloudflare

Publishes the site in `../web` to `porthole.app` as static files with a small
Worker. Cloudflare only serves the files. Tailnet traffic never passes through
it: the browser tab talks to Tailscale directly.

## Files

| File | What it does |
|---|---|
| `wrangler.jsonc` | Worker name, static files folder, and the two routes. |
| `src/worker.js` | Serves the Tailscale client from its compressed copy, and logs test reports. |
| `build.mjs` | Builds `dist/` from `../web` and compresses the client. |
| `deploy.sh` | Runs the build, then publishes. |

## Why there is a Worker at all

The Tailscale client (`web/node/main.wasm`) is about 29 MB, and Cloudflare
limits a single static file to 25 MiB. `build.mjs` stores it compressed with
Brotli (about 5 MB) as `node/main.wasm.br`. The Worker answers
`/node/main.wasm` with that file and `Content-Encoding: br`, so browsers
download 5 MB and unpack it themselves. Every other file is served by
Cloudflare without running the Worker.

## One-time setup

1. Sign in to Cloudflare from this folder:

       npx wrangler login

2. Add a wildcard DNS record for `porthole.app` in the Cloudflare dashboard
   (DNS, Records, Add record):

   | Field | Value |
   |---|---|
   | Type | `AAAA` |
   | Name | `*` |
   | IPv6 address | `100::` |
   | Proxy status | Proxied |

   Each tailnet site is shown on its own subdomain, such as
   `nas--h.porthole.app`. This record sends all of them to the Worker. The
   address `100::` goes nowhere; it is the usual placeholder when a Worker
   answers every request. Wrangler creates the record for `porthole.app`
   itself, so do not add one for the bare domain.

   Cloudflare's free certificate covers `porthole.app` and `*.porthole.app`,
   so no certificate setup is needed.

## Publish

    ./deploy.sh

To build and check the upload without publishing:

    ./deploy.sh --dry-run

## Run it locally

    npm run dev

This serves the built site with the Worker at `http://localhost:8787`.

## After publishing

- `https://porthole.app/test/client.html` starts the Tailscale client in the
  current browser and shows whether it could reach Tailscale.
- `https://porthole.app/test/mock.html` runs the page-loading checks against
  stand-in sites. It needs the wildcard DNS record.
- Add `?report` to either address on a device you cannot read easily, such as
  a TV, then run `npx wrangler tail` here to see the result arrive.

The test pages need no tailnet and expose nothing. To leave them out of the
published site, add `"test"` to `SKIP` in `build.mjs`.
