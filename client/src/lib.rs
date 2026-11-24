//! Porthole's Tailscale client for browser tabs.
//!
//! Built on Tailscale's own Rust crates (tailscale-rs). It creates or
//! restores the device's keys, signs in through the control server, follows
//! the list of devices, and reaches them through Tailscale's relays with
//! WireGuard, TCP, TLS, HTTP and WebSockets. README.md has the details.

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
