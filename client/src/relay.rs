//! A connection to one of Tailscale's relay (DERP) servers.
//!
//! A browser tab cannot send UDP, so every packet to and from other devices
//! goes through a relay. The relay sees only who a packet is for; the packet
//! itself is WireGuard-encrypted between the two devices.
//!
//! Native clients reach a relay by upgrading an HTTPS request. Browser
//! clients open a WebSocket to the same address, with the `derp` subprotocol.
//! The relay protocol on top is the same and comes from tailscale-rs.

use ts_keys::{NodeKeyPair, NodePublicKey};

use crate::{
    error::Error,
    socket::{self, Socket, Stream},
};

const SUBPROTOCOL: &str = "derp";

pub struct Relay {
    client: ts_derp::Client<Stream>,
    _socket: Socket,
}

impl Relay {
    /// Connect to the relay at `host` (a name, with a port if it is not 443)
    /// as the device that owns `keys`.
    pub async fn connect(host: &str, keys: &NodeKeyPair) -> Result<Self, Error> {
        let (socket, stream) = socket::open(&format!("wss://{host}/derp"), SUBPROTOCOL).await?;
        let client = ts_derp::Client::handshake(stream, keys)
            .await
            .map_err(|e| Error::Relay(e.to_string()))?;
        tracing::info!(relay = host, "connected to relay");
        Ok(Self { client, _socket: socket })
    }

    /// Send one packet to the device with the key `to`.
    pub async fn send(&self, to: NodePublicKey, packet: &[u8]) -> Result<(), Error> {
        self.client
            .send_one(to, packet)
            .await
            .map_err(|e| Error::Relay(e.to_string()))
    }

    /// Wait for the next packet from any device.
    pub async fn recv(&self) -> Result<(NodePublicKey, Vec<u8>), Error> {
        let (from, packet) = self
            .client
            .recv_one()
            .await
            .map_err(|e| Error::Relay(e.to_string()))?;
        Ok((from, packet.as_ref().to_vec()))
    }
}

/// Checks that packets can cross a relay from this browser: two throwaway
/// devices connect to `host` and one sends the other a packet. Needs no
/// sign-in, because Tailscale's relays accept any device key.
pub async fn check(host: &str) -> Result<(), Error> {
    const NOTE: &[u8] = b"porthole relay check";
    let (a, b) = (NodeKeyPair::random(), NodeKeyPair::random());
    let to = Relay::connect(host, &b).await?;
    let from = Relay::connect(host, &a).await?;
    from.send(b.public, NOTE).await?;
    let (sender, packet) = to.recv().await?;
    if sender != a.public || packet != NOTE {
        return Err(Error::Relay("the relay delivered a different packet".into()));
    }
    Ok(())
}

/// How long to wait for the quickest relay to answer a probe.
const PROBE_LIMIT_MS: u32 = 4000;

/// Picks the relay region that answers first from this browser, which is the
/// one other devices should use to reach us. `hosts` maps region numbers to
/// host names. If no probe is answered in time (something on the device may
/// block them), falls back to the lowest-numbered region: any relay works,
/// a far one is only slower.
pub async fn nearest(hosts: &std::collections::HashMap<u32, String>) -> Option<u32> {
    use futures_util::future::{Either, select, select_ok};
    let fallback = hosts.keys().min().copied()?;
    let probes = hosts.iter().map(|(region, host)| {
        let (region, url) = (*region, format!("https://{host}/derp/latency-check"));
        Box::pin(async move {
            // The answer is not readable from another site and does not need
            // to be: only how quickly it arrives matters.
            gloo_net::http::Request::get(&url)
                .mode(web_sys::RequestMode::NoCors)
                .send()
                .await
                .map(|_| region)
        })
    });
    let limit = gloo_timers::future::TimeoutFuture::new(PROBE_LIMIT_MS);
    match select(select_ok(probes), limit).await {
        Either::Left((Ok((region, _slower)), _)) => Some(region),
        Either::Left((Err(error), _)) => {
            tracing::warn!(%error, fallback, "no relay answered a probe");
            Some(fallback)
        }
        Either::Right(_) => {
            tracing::warn!(fallback, "no relay answered a probe in time");
            Some(fallback)
        }
    }
}
