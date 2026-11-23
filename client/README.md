# Rust client (work in progress)

A replacement for the Go Tailscale client in `web/node/`, built on Tailscale's
own Rust crates, [tailscale-rs](https://github.com/tailscale/tailscale-rs).
The app does not use it yet.

The reasons for it are size and reach. The Go client is a 30 MB download that
needs 18 MB of memory before it does anything, and Go's compiler sets the
oldest browser it can run in. Measured on the same Mac in Chrome (the test page also passes in Safari):

| | Go client | This client, so far |
|---|---|---|
| Download, compressed | 4.8 MB | about 0.65 MB |
| Memory after start | 18 MB | 1.8 MB |
| Time to a sign-in address | about 1.2 s | 0.1 to 0.3 s |

The Rust figures cover only what is built so far, and will grow as the rest
is added.

## What works

- Creating the device's keys, and saving and restoring them as text.
- Connecting to Tailscale's control server from a browser tab.
- Registering the device and getting the address where the user signs in.
- Waiting for that sign-in to finish.
- Passing packets between devices through a Tailscale relay (DERP) over a
  WebSocket.
- WireGuard through the relay, and TCP connections on top of it. The test
  page proves this with two throwaway devices in the same tab: they meet at
  a relay, set up WireGuard, and one opens a TCP connection to the other,
  in about 1.2 s.

Written but not yet run against a real tailnet, because it needs a person to
sign in:

- Following the device list and the relay list.
- Choosing the nearest relay and telling the control server, so other
  devices can answer.
- Opening a TCP connection to a real device by name.

Open `/test/client-rs-signin.html` to try all three.

## What is left

In the order it needs doing:

1. Checking the sign-in page against a real tailnet, and fixing what it
   turns up.
2. TLS to `https://` tailnet sites (rustls), then HTTP, cookies and
   WebSockets over that, with the same JavaScript API the Go build gives the
   app (`request`, `websocket`, `cookies`, `setCookie`).
3. Answering the control server's pings, reconnecting when the control
   connection or a relay drops, and signing out.
4. Switching `web/app/` over, and building for the oldest browsers we can
   reach.

## Build and test

```sh
./build.sh                 # writes ../web/client/
RUN_TESTS=1 ./build.sh     # runs the tests first
```

Then start the site (`../serve.py`) and open
`http://localhost:8791/test/client-rs.html`. It connects to Tailscale's real
control server and asks for a sign-in address. Then, with two throwaway
devices, it passes a packet through a Tailscale relay and opens a TCP
connection over WireGuard through it. None of this signs in.

`/test/client-rs-signin.html` goes further and needs you: it shows the
sign-in link, lists your devices and the relays, and lets you open a
connection to one of your devices.

`build.sh` fetches tailscale-rs at a pinned commit into `../tailscale-rs-src`
and applies `tailscale-rs.patch`. After changing files there, run
`./make-patch.sh` to rewrite the patch.

## Files

| File | Purpose |
|---|---|
| `src/tab.rs` | the API the page sees: `new Client(...)`, `state()`, `signInAddress()`, `waitForSignIn()`, `watch()`, `relays()`, and the diagnostics `tcpProbe()`, `relayCheck()` and `pathCheck()`. |
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
without a change. The patch is small (22 files) and does five things. The
first three apply only when building for `wasm32-unknown-unknown`:

- stops four crates asking for tokio's thread pool, which does not exist in
  a browser;
- runs HTTP connection tasks on the page's event loop instead of on tokio;
- leaves out the code that dials TCP and TLS and reads the OS hostname,
  since the page supplies the connection and the name;
- reads the clock through the `web-time` crate, because the standard
  library's clock panics in a browser (natively it is the same clock);
- lets `ts_dataplane` be built without its tokio wrapper.

Native builds of the patched crates still compile unchanged
(`cargo check -p ts_runtime` in the checkout).

tailscale-rs is before 1.0 and releases about monthly, so expect to redo
parts of the patch when moving the pinned commit.
