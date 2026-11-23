//! Porthole's Tailscale client for browser tabs.
//!
//! This is the start of a replacement for the Go client in `web/node/`. It is
//! built on Tailscale's own Rust crates (tailscale-rs) and exists to be much
//! smaller and to run in older browsers.
//!
//! So far it covers the control plane up to sign-in: it creates or restores
//! the device's keys, connects to the control server, and reports the address
//! where the user signs in. Reaching other devices is not implemented yet.

mod challenge;

// Everything below needs a browser. The module above does not, so its tests
// run natively with `cargo test --target <host>`.
#[cfg(target_arch = "wasm32")]
mod control;
#[cfg(target_arch = "wasm32")]
mod error;
#[cfg(target_arch = "wasm32")]
mod local;
#[cfg(target_arch = "wasm32")]
mod tab;

#[cfg(target_arch = "wasm32")]
pub use tab::Client;
