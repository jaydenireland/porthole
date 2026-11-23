//! Porthole's Tailscale client for browser tabs.
//!
//! This is the start of a replacement for the Go client in `web/node/`. It is
//! built on Tailscale's own Rust crates (tailscale-rs) and exists to be much
//! smaller and to run in older browsers.
//!
//! So far it covers the control plane and the relay connection: it creates
//! or restores the device's keys, connects to the control server, reports the
//! address where the user signs in, follows the list of devices, and can pass
//! packets through a relay. Talking to other devices (WireGuard and
//! everything above it) is not implemented yet.

mod challenge;
mod cookies;
mod http;
mod site;
mod tls;

// Everything below needs a browser. The modules above do not, so their tests
// run natively with `cargo test --target <host>`.
#[cfg(target_arch = "wasm32")]
mod control;
#[cfg(target_arch = "wasm32")]
mod engine;
#[cfg(target_arch = "wasm32")]
mod error;
#[cfg(target_arch = "wasm32")]
mod local;
#[cfg(target_arch = "wasm32")]
mod netmap;
#[cfg(target_arch = "wasm32")]
mod page;
#[cfg(target_arch = "wasm32")]
mod relay;
#[cfg(all(target_arch = "wasm32", feature = "diagnostics"))]
mod selftest;
#[cfg(target_arch = "wasm32")]
mod socket;
#[cfg(target_arch = "wasm32")]
mod tab;
#[cfg(target_arch = "wasm32")]
mod timer;

#[cfg(target_arch = "wasm32")]
pub use tab::Client;
#[cfg(all(target_arch = "wasm32", feature = "diagnostics"))]
pub use tab::{abandon_check, netmap_check, paired_client, path_check, recovery_check, relay_check, site_check};
