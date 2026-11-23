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

// Everything below needs a browser. The module above does not, so its tests
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
mod relay;
#[cfg(target_arch = "wasm32")]
mod socket;
#[cfg(target_arch = "wasm32")]
mod tab;

#[cfg(target_arch = "wasm32")]
pub use tab::{Client, path_check, relay_check};
