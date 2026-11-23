# Rust client (work in progress)

A replacement for the Go Tailscale client in `web/node/`, built on Tailscale's
own Rust crates, [tailscale-rs](https://github.com/tailscale/tailscale-rs).
The app does not use it yet.

The reasons for it are size and reach. The Go client is a 30 MB download that
needs 18 MB of memory before it does anything, and Go's compiler sets the
oldest browser it can run in. Measured on the same Mac in Chrome (the test page also passes in Safari):

| | Go client | This client, so far |
|---|---|---|
| Download, compressed | 4.8 MB | about 0.5 MB |
| Memory after start | 18 MB | 1.5 MB |
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

Written but not yet run against a tailnet, because it needs a person to sign
in: following the device list and the relay list after sign-in. Open
`/test/client-rs-signin.html` to try it.

## What is left

In the order it needs doing:

1. Telling the control server which relay this device listens on, and
   answering the control server's pings.
2. WireGuard over the relay (`ts_tunnel`) and a TCP/IP stack on top
   (`ts_netstack_smoltcp`).
3. TLS to `https://` tailnet sites (rustls), then HTTP, cookies and
   WebSockets over that, with the same JavaScript API the Go build gives the
   app (`request`, `websocket`, `cookies`, `setCookie`).
4. Switching `web/app/` over, and building for the oldest browsers we can
   reach.

## Build and test

```sh
./build.sh                 # writes ../web/client/
RUN_TESTS=1 ./build.sh     # runs the tests first
```

Then start the site (`../serve.py`) and open
`http://localhost:8791/test/client-rs.html`. It connects to Tailscale's real
control server, asks for a sign-in address, and passes a packet between two
throwaway devices through a Tailscale relay, all without signing in.

`/test/client-rs-signin.html` goes further and needs you: it shows the
sign-in link, then lists your devices and the relays.

`build.sh` fetches tailscale-rs at a pinned commit into `../tailscale-rs-src`
and applies `tailscale-rs.patch`. After changing files there, run
`./make-patch.sh` to rewrite the patch.

## Files

| File | Purpose |
|---|---|
| `src/tab.rs` | the API the page sees: `new Client(...)`, `state()`, `signInAddress()`, `waitForSignIn()`, `watch()`, `relays()`, and the `relayCheck()` diagnostic. |
| `src/control.rs` | the control server connection over a WebSocket. |
| `src/netmap.rs` | the tab's picture of the tailnet, kept up to date from the control server's updates. |
| `src/relay.rs` | a relay (DERP) connection over a WebSocket. |
| `src/socket.rs` | a WebSocket as a byte stream. |
| `src/challenge.rs` | steps past the control server's optional opening challenge without waiting for it. |
| `src/local.rs` | lets single-threaded browser values satisfy the libraries' `Send` bounds. |
| `src/error.rs` | the errors the page can be shown. |
| `tailscale-rs.patch` | our changes to tailscale-rs. |

## What we use and what we wrote

Everything that is a protocol comes from a library:

- tailscale-rs: keys, the Noise handshake with the control server, the HTTP/2
  channel inside it, registration, the device-list stream, the relay
  protocol.
- hyper and h2 (through tailscale-rs): HTTP/2.
- ws_stream_wasm: a WebSocket as a byte stream.
- gloo-net: `fetch`.
- wasm-bindgen: the JavaScript bindings.

Our own code is the glue: opening the WebSocket the way Tailscale's browser
client does (for the control server the first handshake message goes in the
address, with the `tailscale-control-protocol` subprotocol; for a relay the
subprotocol is `derp`), keeping the device list current, and the challenge
handling in `challenge.rs`.

## The patch

tailscale-rs supports Linux, macOS and Windows. It has no browser build, and
an upstream request for one
([issue 497](https://github.com/tailscale/tailscale-rs/issues/497)) was closed
without a change. The patch is small (14 files) and does three things, all
only when building for `wasm32-unknown-unknown`:

- stops four crates asking for tokio's thread pool, which does not exist in
  a browser;
- runs HTTP connection tasks on the page's event loop instead of on tokio;
- leaves out the code that dials TCP and TLS and reads the OS hostname,
  since the page supplies the connection and the name.

Native builds of the patched crates still compile unchanged
(`cargo check -p ts_runtime` in the checkout).

tailscale-rs is before 1.0 and releases about monthly, so expect to redo
parts of the patch when moving the pinned commit.
