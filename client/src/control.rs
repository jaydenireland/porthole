//! The connection to Tailscale's control server.
//!
//! A native client dials TCP, speaks TLS, and upgrades an HTTP/1.1 request to
//! the control protocol. A browser tab can do none of that, so it opens a
//! WebSocket to the same address instead, which the control server accepts
//! for browser clients. Everything after that point is shared with native
//! clients and comes from tailscale-rs: the Noise handshake, the HTTP/2
//! channel inside it, and the registration request.

use serde::Deserialize;
use ts_capabilityversion::CapabilityVersion;
use ts_control::{Config, RegistrationError, client as control};
use ts_http_util::{BytesBody, Http2};
use ts_keys::{MachineKeyPair, MachinePublicKey, NodeState};
use url::Url;
use futures_util::Stream;
use ts_control::StateUpdate;

use crate::{
    challenge::SkipChallenge,
    error::Error,
    local::Local,
    socket::{self, Socket},
};

/// Query parameter carrying the first handshake message, and the WebSocket
/// subprotocol, as Tailscale's own browser client sends them.
const HANDSHAKE_PARAM: &str = "X-Tailscale-Handshake";
const SUBPROTOCOL: &str = "tailscale-control-protocol";

/// An open channel to the control server.
pub struct Connection {
    http: Http2<BytesBody>,
    // Closing the socket ends the channel, so it lives as long as we do.
    _socket: Socket,
}

/// What the control server said about this device.
pub enum Registration {
    /// The device is part of the tailnet.
    Authorized,
    /// Someone has to sign in at this address first.
    NeedsSignIn(Url),
}

#[derive(Deserialize)]
struct ControlKeys {
    #[serde(rename = "publicKey")]
    public_key: MachinePublicKey,
}

/// Fetch the key that proves we are talking to the real control server.
///
/// This must go over HTTPS: whoever supplies this key can read the control
/// channel. The browser checks the certificate for us.
async fn fetch_control_key(control_url: &Url) -> Result<MachinePublicKey, Error> {
    let mut key_url = control_url.join("/key")?;
    if key_url.scheme() != "https" && !is_local(&key_url) {
        return Err(Error::ControlKey("the control server address must use https".into()));
    }
    key_url
        .query_pairs_mut()
        .append_pair("v", &CapabilityVersion::CURRENT.to_string());

    let response = gloo_net::http::Request::get(key_url.as_str())
        .send()
        .await
        .map_err(|e| Error::ControlKey(e.to_string()))?;
    if !response.ok() {
        return Err(Error::ControlKey(format!("status {}", response.status())));
    }
    let text = response
        .text()
        .await
        .map_err(|e| Error::ControlKey(e.to_string()))?;
    let keys: ControlKeys =
        serde_json::from_str(&text).map_err(|e| Error::ControlKey(e.to_string()))?;
    Ok(keys.public_key)
}

/// A control server on this machine, as used when developing against Headscale.
fn is_local(url: &Url) -> bool {
    matches!(url.host_str(), Some("localhost" | "127.0.0.1" | "[::1]"))
}

impl Connection {
    pub async fn open(control_url: &Url, machine_keys: &MachineKeyPair) -> Result<Self, Error> {
        let control_key = fetch_control_key(control_url).await?;

        let (handshake, first_message) = ts_control_noise::Handshake::initialize(
            &control::CONTROL_PROTOCOL_VERSION,
            machine_keys,
            &control_key,
            CapabilityVersion::CURRENT,
        );

        let mut socket_url = control_url.join("/ts2021")?;
        let scheme = if control_url.scheme() == "http" { "ws" } else { "wss" };
        socket_url
            .set_scheme(scheme)
            .map_err(|()| Error::Connection("unsupported control server address".into()))?;
        socket_url
            .query_pairs_mut()
            .append_pair(HANDSHAKE_PARAM, &first_message);

        let (socket, stream) = socket::open(socket_url.as_str(), SUBPROTOCOL).await?;

        let channel = handshake.complete(stream, machine_keys).await?;
        // The server may open with a challenge, which we do not answer.
        let http = ts_http_util::http2::connect(Local(SkipChallenge::new(channel)))
            .await
            .map_err(|e| Error::Connection(e.to_string()))?;

        tracing::info!(server = %control_url, "connected to the control server");
        Ok(Self { http, _socket: socket })
    }

    /// Ask the control server to admit this device.
    ///
    /// With `waiting_on` set to an address from an earlier
    /// [`Registration::NeedsSignIn`], the request stays open until someone
    /// finishes signing in there.
    pub async fn register(
        &self,
        config: &Config,
        keys: &NodeState,
        waiting_on: Option<Url>,
    ) -> Result<Registration, Error> {
        match control::register(config, &config.server_url, None, waiting_on, keys, &self.http).await
        {
            Ok(()) => Ok(Registration::Authorized),
            Err(RegistrationError::MachineNotAuthorized(Some(url))) => {
                Ok(Registration::NeedsSignIn(url))
            }
            Err(e) => Err(Error::Registration(e.to_string())),
        }
    }

    /// Start receiving the tailnet's state: this device, the others, the
    /// relays. The device must be registered first.
    pub async fn updates(
        &self,
        config: &Config,
        keys: &NodeState,
    ) -> Result<impl Stream<Item = StateUpdate> + use<>, Error> {
        control::start_stream(&config.server_url, keys, config, self.http.clone())
            .await
            .map_err(|e| Error::Netmap(e.to_string()))
    }
}
