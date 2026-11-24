# Porthole

Browse a Tailscale network from a browser tab, on a computer where Tailscale
is not installed. Try it at [porthole.app](https://porthole.app).

The tab runs a Tailscale client compiled to WebAssembly, joins the tailnet as
a temporary device, and shows tailnet web pages in a frame. The site is static
files: tailnet traffic goes from the tab to Tailscale and never through a
server of ours.

Porthole is an independent project and is not affiliated with Tailscale Inc.

## What works and what does not

Porthole shows each tailnet site on an address of its own instead of its real
one. Most self-hosted web apps run fine that way, and a few features cannot.

| Works | Does not work |
|---|---|
| Dashboards, admin pages and internal tools | Anything that is not a web page: SSH, file shares, remote desktop |
| Username and password logins, and the sessions that follow | Sign in with Google, other third-party sign-in, and passkeys, which are tied to a site's real address |
| Pages that update over WebSockets | Apps that connect browsers directly to each other, such as video calls |
| HTTP and HTTPS on any port, by device name or address | Fast video and large downloads: traffic always goes through Tailscale's relays |

Subnet routers and tailnet lock are not supported. `client/README.md` says why.

## How it works

1. The tab downloads the client, about 1 MB. It is Rust, built on Tailscale's
   [tailscale-rs](https://github.com/tailscale/tailscale-rs) crates with a
   small patch so they compile for a browser.
2. The client connects to Tailscale's control server and gives a sign-in
   address. You approve the tab as you would any new device, and it joins as
   an ephemeral device.
3. A browser tab cannot send UDP, so packets go through Tailscale's relays
   (DERP) over a WebSocket. WireGuard, TCP and TLS run inside the tab.
4. Each tailnet site is shown on its own subdomain, such as
   `nas--h.porthole.app`. A service worker there answers the page's requests
   through the client. Because every site has its own origin, one site cannot
   read another's cookies or data.

`client/README.md` covers the client and `web/README.md` covers the site.

## Security

While the tab is open, the scripts served by the Porthole site have live
access to whatever your account can reach on your tailnet. Using
`porthole.app` means trusting the copy published there and the computer you
are on. If you would rather not, host your own copy (below).

- The tab's device keys are kept for the browser session only, unless you
  tick "Stay signed in on this browser".
- Tailscale sees what it sees for any device: the client talks to its
  coordination servers and relays. The relays cannot read the traffic.
- The published site keeps no logs of tailnet traffic, because none reaches
  it. Its host (Cloudflare) does see the names of the tailnet sites you open,
  because each is shown on a subdomain that contains the name. Its one Worker only records results the test pages send.

To report a security problem, please contact the maintainer privately before
opening a public issue.

## Layout

| Folder | What is in it |
|---|---|
| `web/` | The site: landing page, the app, the service worker, tests. See `web/README.md`. |
| `client/` | The Tailscale client, in Rust on Tailscale's tailscale-rs crates. See `client/README.md`. |
| `cloudflare/` | Worker, config and scripts that publish `web/` to Cloudflare. See `cloudflare/README.md`. |
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

## Host your own copy

The site is the `web/` folder plus the built client, and nothing in it names
a domain. Any static host works if it can do two things:

- serve the same files on the domain and on every subdomain of it, which
  needs a wildcard DNS record and a wildcard certificate;
- serve over HTTPS, which service workers require outside `localhost`.

To use the Cloudflare setup in this repository on your own domain, replace
`porthole.app` in `cloudflare/wrangler.jsonc` and `cloudflare/deploy.sh`,
follow the one-time setup in `cloudflare/README.md`, and run:

    cloudflare/deploy.sh

## Supported browsers

Chrome and Edge 84, Firefox 90, Safari 16. `web/compatibility/index.html` has
the full list and says how each entry was checked.

## Contributing

Issues and pull requests are welcome. The most useful report is an app that
breaks inside Porthole: say which app and version, and what the browser's
console shows with `?debug` added to the app's address.

Before sending a change, run `RUN_TESTS=1 client/build.sh` and open
`/test/mock.html` and `/test/client.html`.

## License

BSD 3-Clause. See `LICENSE`.

The client is built from [tailscale-rs](https://github.com/tailscale/tailscale-rs),
copyright Tailscale Inc & contributors, also under the BSD 3-Clause license.
`client/tailscale-rs.patch` is a change to that code. The other libraries the
client uses are listed in `client/README.md` and pinned in `client/Cargo.lock`.

The published site carries every library's copyright notice and licence text
in `/client/THIRD-PARTY-LICENSES.txt`, which `client/build.sh` regenerates from
`Cargo.lock` on each build (`client/make-notices.py`). If you host your own
copy, publish that file with it: the licences require it.

Tailscale is a registered trademark of Tailscale Inc.
