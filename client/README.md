# The Tailscale client

Porthole's Tailscale client for browser tabs, built on Tailscale's own Rust
crates, [tailscale-rs](https://github.com/tailscale/tailscale-rs), and
compiled to WebAssembly. The app loads it through `../web/app/client.js`.

It replaced a build of Tailscale's Go browser client (in the git history up
to November 2026). The reasons were size and reach: the Go client was a
4.8 MB compressed download that needed 18 MB of memory before it did
anything, and Go's compiler decides which WebAssembly features it uses.

| | Go client | This client |
|---|---|---|
| Download, compressed | 4.8 MB | 1.0 MB |
| Time to a sign-in address | about 1.2 s | 0.1 to 0.3 s |

## What it does

- Creates the device's keys, and saves and restores them as text.
- Connects to Tailscale's control server from a browser tab, registers the
  device, and gives the address where the user signs in.
- Follows the device list and the relay list, chooses the nearest relay and
  tells the control server, so other devices can answer.
- Passes packets through a Tailscale relay (DERP) over a WebSocket, with
  WireGuard and TCP on top.
- What pages use: HTTP and HTTPS requests with the certificate checked,
  cookies, and WebSockets.
- Reconnects on its own when the control server or a relay connection ends.
- Signs out.

All of it has been used on a real tailnet from an iPhone, and the test page
below passes in Chrome on a Mac.

## What is left

1. Building for older browsers. The client needs three WebAssembly features
   beyond the first version (bulk memory, sign extension, saturating
   conversions), and the page scripts are not transpiled. Together they put
   the floor at Chrome 84, Firefox 90 and Safari 16.
2. Desktop Safari in one visible tab, Firefox, and Android. Safari on a Mac
   passed every check up to the JavaScript calls and then became unreliable
   in hidden tabs, which Safari slows down.

Known gaps in recovery: a relay connection that dies without the browser
noticing is not detected (the library hides the relay's keep-alives), and
if the home relay stays unreachable the client keeps trying it instead of
choosing another.

## Decided, and why

- **Subnet routers are out of scope.** Only devices' own tailnet addresses
  are reachable, not addresses behind a device that routes for others.
- **Tailnet lock is out of scope.** tailscale-rs does not check tailnet lock
  signatures, so a tailnet that uses it gets no protection from it here.
- **The tailnet's access rules are not applied to arriving packets.** The
  tab never accepts connections, so a packet that is not part of one we
  opened has no socket to go to. `engine.rs` has the details.

## Build and test

```sh
./build.sh                 # writes ../web/client/ and ../web/test/client/
RUN_TESTS=1 ./build.sh     # runs the tests first
```

Then start the site (`../serve.py`) and open
`http://localhost:8791/test/client.html`. It connects to Tailscale's real
control server and asks for a sign-in address. Then, with two throwaway
devices, it passes a packet through a Tailscale relay, opens a TCP
connection over WireGuard through it, and makes HTTP, HTTPS and WebSocket
requests over that. None of this signs in.

`RUN_TESTS=1 ./build.sh` also runs 29 tests on this machine. They cover the
parts that need no browser: HTTP, TLS, cookies and WebSockets against a
small server inside the test, and the challenge handling.

There are two builds. The app's, in `../web/client/`, has no checks and no
logging. The test pages', in `../web/test/client/`, has both (the
`diagnostics` and `console-log` features), and the app loads it instead
when opened with `?debug`, which also shows what the client logs under
"Details".

The build needs a clang that can compile for WebAssembly, for the C parts of
the TLS crypto library. Apple's cannot; `brew install llvm` provides one and
`build.sh` finds it. It also needs Binaryen's `wasm-opt`
(`brew install binaryen`), which shrinks the result and refuses any
WebAssembly feature beyond the three named above, so a change that would
raise the oldest supported browser fails the build.

`/test/client-signin.html` goes further and needs you: it shows the
sign-in link, lists your devices and the relays, and lets you open a
connection to one of your devices.

`build.sh` fetches tailscale-rs at a pinned commit into `../tailscale-rs-src`
and applies `tailscale-rs.patch`. After changing files there, run
`./make-patch.sh` to rewrite the patch.

## Files

| File | Purpose |
|---|---|
| `src/tab.rs` | signing in and following the tailnet: `new Client(...)`, `state()`, `signInAddress()`, `waitForSignIn()`, `watch()`, `relays()`, `tcpProbe()`. |
| `src/page.rs` | what pages use: `request()`, `websocket()`, `cookies()`, `setCookie()`, `exportCookies()`, `importCookies()`. |
| `src/site.rs` | requests and WebSockets to a site: connect or reuse a connection, add TLS, send, keep cookies. |
| `src/http.rs` | HTTP requests over a connection that is kept for the next one, with the body read a piece at a time and gzip unpacked. |
| `src/tls.rs` | TLS settings and certificate checking. |
| `src/cookies.rs` | the cookie jar, with a browser's rules for scripts. |
| `src/selftest.rs` | the checks the test page runs: `relayCheck()`, `pathCheck()`, `recoveryCheck()`, `siteCheck()`, `netmapCheck()`, `pairedClient()`. |
| `src/timer.rs` | waiting, done in a worker so hidden tabs do not stall. |
| `../web/app/client.js` | loads the client for the app and reports its progress as a state. |
| `src/control.rs` | the control server connection over a WebSocket. |
| `src/engine.rs` | joins the TCP/IP stack, WireGuard and the relays, and runs them on the page's event loop. |
| `src/netmap.rs` | the tab's picture of the tailnet, kept up to date from the control server's updates. |
| `src/relay.rs` | a relay (DERP) connection over a WebSocket, and choosing the nearest relay. |
| `src/socket.rs` | a WebSocket as a byte stream. |
| `src/challenge.rs` | steps past the control server's optional opening challenge without waiting for it. |
| `src/local.rs` | lets single-threaded browser values satisfy the libraries' `Send` bounds. |
| `src/error.rs` | the errors the page can be shown. |
| `tailscale-rs.patch` | our changes to tailscale-rs. |

## What we use and what we wrote

Everything that is a protocol comes from a library:

- tailscale-rs: keys, the Noise handshake with the control server, the HTTP/2
  channel inside it, registration, the device-list stream, the relay
  protocol, WireGuard, packet routing, and the socket layer over smoltcp.
- smoltcp (through tailscale-rs): TCP/IP.
- rustls with ring: TLS and its cryptography. webpki-roots: Mozilla's list
  of certificate authorities.
- hyper: HTTP/1.1 to sites. tokio-tungstenite: WebSockets.
- cookie_store: cookie matching rules. psl: the public suffix list.
- hyper and h2 (through tailscale-rs): HTTP/2.
- ws_stream_wasm: a WebSocket as a byte stream.
- gloo-net: `fetch`.
- wasm-bindgen: the JavaScript bindings.

Our own code is the glue: opening the WebSocket the way Tailscale's browser
client does (for the control server the first handshake message goes in the
address, with the `tailscale-control-protocol` subprotocol; for a relay the
subprotocol is `derp`), keeping the device list current, the challenge
handling in `challenge.rs`, and the engine in `engine.rs`.

The engine stands in for tailscale-rs's own runtime, which is built on
tokio's thread pool and cannot run in a browser. It is much smaller because
a tab has less to do: it can only use relays, so there is no direct
connection to set up, and it never accepts connections. For that second
reason it also does not apply the tailnet's access rules to arriving packets;
`engine.rs` explains why that is safe.

## The patch

tailscale-rs supports Linux, macOS and Windows. It has no browser build, and
an upstream request for one
([issue 497](https://github.com/tailscale/tailscale-rs/issues/497)) was closed
without a change. The patch is small (24 files) and does seven things. The
first three apply only when building for `wasm32-unknown-unknown`:

- stops four crates asking for tokio's thread pool, which does not exist in
  a browser;
- runs HTTP connection tasks on the page's event loop instead of on tokio;
- leaves out the code that dials TCP and TLS and reads the OS hostname,
  since the page supplies the connection and the name;
- reads the clock through the `web-time` crate, because the standard
  library's clock panics in a browser (natively it is the same clock);
- lets `ts_dataplane` be built without its tokio wrapper;
- tells the HTTP/2 library to keep no record of abandoned responses, since
  keeping one makes it read a clock that browsers do not have;
- makes TCP sockets send and acknowledge at once instead of waiting briefly
  to batch, which suits request-and-answer traffic over a relay and needs
  fewer timers.

Native builds of the patched crates still compile unchanged
(`cargo check -p ts_runtime` in the checkout).

tailscale-rs is before 1.0 and releases about monthly, so expect to redo
parts of the patch when moving the pinned commit.
