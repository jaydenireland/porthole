//! The API the page sees.

use std::{cell::RefCell, rc::Rc};

use ts_control::Config;
use ts_keys::{NodeState, PersistState};
use url::Url;
use wasm_bindgen::prelude::*;

use futures_util::StreamExt;

use crate::{
    control::{Connection, Registration},
    error::Error,
    engine::Engine,
    netmap::Netmap,
    relay,
    site::{Dialer, Site, Stream},
    tls,
};
use tokio_util::compat::FuturesAsyncReadCompatExt;

#[wasm_bindgen(start)]
fn start() {
    console_error_panic_hook::set_once();
    #[cfg(feature = "console-log")]
    {
        use tracing_subscriber::prelude::*;
        let console = tracing_subscriber::fmt::layer()
            .with_ansi(false)
            .without_time()
            .with_writer(tracing_web::MakeWebConsoleWriter::new());
        // Ignored if the page already installed one (a second `init`).
        let _ = tracing_subscriber::registry()
            .with(tracing_subscriber::filter::LevelFilter::INFO)
            .with(console)
            .try_init();
    }
}

/// What a connection to a device needs: who the devices are, and the engine.
pub(crate) struct Net {
    pub(crate) netmap: RefCell<Netmap>,
    pub(crate) engine: Engine,
}

/// Opens connections to devices through the tunnel.
pub(crate) struct Tunnel(Rc<Net>);

impl Dialer for Tunnel {
    async fn dial(&self, host: &str, port: u16) -> std::io::Result<Stream> {
        let address = self.0.netmap.borrow().resolve(host).ok_or_else(|| {
            std::io::Error::new(std::io::ErrorKind::NotFound, format!("no device is called {host}"))
        })?;
        let stream = self
            .0
            .engine
            .connect(std::net::SocketAddr::new(address, port))
            .await
            .map_err(|error| std::io::Error::other(error.to_string()))?;
        // The socket speaks the `futures` flavour of async I/O; hyper,
        // rustls and tungstenite here speak tokio's.
        Ok(Box::new(stream.compat()))
    }
}

pub(crate) struct Inner {
    config: Config,
    keys: NodeState,
    connection: RefCell<Option<Rc<Connection>>>,
    sign_in: RefCell<Option<Url>>,
    signed_in: std::cell::Cell<bool>,
    /// The tailnet as we know it and the engine that reaches it.
    pub(crate) net: Rc<Net>,
    /// Requests, sockets and cookies for the sites on it.
    pub(crate) site: Site<Tunnel>,
    /// The relay region other devices reach us on, once chosen.
    home_relay: std::cell::Cell<Option<u32>>,
}

impl Inner {
    /// Bring the engine in line with the latest picture of the tailnet.
    async fn sync_engine(self: &Rc<Self>) {
        let (addresses, relays, peers) = {
            let mut netmap = self.net.netmap.borrow_mut();
            (netmap.addresses(), netmap.relay_hosts(), netmap.engine_peers())
        };
        self.net.engine.set_relays(relays.clone());
        self.net.engine.set_peers(&peers);
        if let Some(addresses) = addresses
            && let Err(error) = self.net.engine.set_addresses(addresses).await
        {
            tracing::warn!(%error, "could not set this device's addresses");
        }
        if self.home_relay.get().is_none() && !relays.is_empty() {
            // Marked first so a second update does not start a second choice.
            self.home_relay.set(Some(0));
            let this = self.clone();
            wasm_bindgen_futures::spawn_local(async move {
                if let Err(error) = this.choose_home_relay(&relays).await {
                    tracing::warn!(%error, "could not set a home relay; other devices cannot answer yet");
                    this.home_relay.set(None);
                }
            });
        }
    }

    /// Pick the nearest relay, connect to it, and tell the control server,
    /// so other devices know where to send packets for us.
    async fn choose_home_relay(
        &self,
        relays: &std::collections::HashMap<u32, String>,
    ) -> Result<(), Error> {
        let region = relay::nearest(relays)
            .await
            .ok_or_else(|| Error::Relay("no relay answered".into()))?;
        self.net.engine.listen_on(region).await?;
        let connection = self.connection().await?;
        connection.set_home_relay(&self.config, &self.keys, region).await?;
        self.home_relay.set(Some(region));
        tracing::info!(region, "home relay set");
        Ok(())
    }

    async fn connection(&self) -> Result<Rc<Connection>, Error> {
        if let Some(open) = self.connection.borrow().as_ref() {
            return Ok(open.clone());
        }
        let open = Rc::new(Connection::open(&self.config.server_url, &self.keys.machine_keys).await?);
        *self.connection.borrow_mut() = Some(open.clone());
        Ok(open)
    }
}

/// One device on the tailnet, living in this tab.
#[wasm_bindgen]
pub struct Client {
    pub(crate) inner: Rc<Inner>,
}

#[wasm_bindgen]
impl Client {
    /// `saved_state` is what an earlier [`Client::state`] returned, to come
    /// back as the same device; without it the tab gets new keys.
    /// `control_url` defaults to Tailscale's control server.
    #[wasm_bindgen(constructor)]
    pub fn new(
        hostname: String,
        ephemeral: bool,
        saved_state: Option<String>,
        control_url: Option<String>,
    ) -> Result<Client, Error> {
        let keys = match saved_state.as_deref() {
            Some(json) if !json.is_empty() => {
                NodeState::from(serde_json::from_str::<PersistState>(json)?)
            }
            _ => NodeState::generate(),
        };
        let server_url = match control_url {
            Some(url) => Url::parse(&url)?,
            None => ts_control::DEFAULT_CONTROL_SERVER.clone(),
        };
        let config = Config {
            server_url,
            hostname: Some(hostname),
            client_name: Some("porthole".to_owned()),
            tags: Vec::new(),
            ephemeral,
        };
        let net = Rc::new(Net {
            netmap: RefCell::new(Netmap::default()),
            engine: Engine::new(keys.node_keys.clone()),
        });
        Ok(Client {
            inner: Rc::new(Inner {
                site: Site::new(Tunnel(net.clone()), tls::config()),
                net,
                config,
                keys,
                connection: RefCell::new(None),
                sign_in: RefCell::new(None),
                signed_in: std::cell::Cell::new(false),
                home_relay: std::cell::Cell::new(None),
            }),
        })
    }

    /// The device's private keys as text, for the page to store. Whoever
    /// holds this can act as this device.
    pub fn state(&self) -> Result<String, Error> {
        Ok(serde_json::to_string(&PersistState::from(&self.inner.keys))?)
    }

    /// Resolves to the address where the user signs in, or to nothing if
    /// this device is already part of the tailnet.
    #[wasm_bindgen(js_name = signInAddress)]
    pub async fn sign_in_address(&self) -> Result<Option<String>, Error> {
        let inner = self.inner.clone();
        let connection = inner.connection().await?;
        match connection.register(&inner.config, &inner.keys, None).await? {
            Registration::Authorized => {
                inner.signed_in.set(true);
                Ok(None)
            }
            Registration::NeedsSignIn(url) => {
                let address = url.to_string();
                *inner.sign_in.borrow_mut() = Some(url);
                Ok(Some(address))
            }
        }
    }

    /// Resolves once the user has finished signing in at the address from
    /// [`Client::sign_in_address`].
    #[wasm_bindgen(js_name = waitForSignIn)]
    pub async fn wait_for_sign_in(&self) -> Result<(), Error> {
        let inner = self.inner.clone();
        let connection = inner.connection().await?;
        loop {
            let waiting_on = inner.sign_in.borrow().clone();
            match connection.register(&inner.config, &inner.keys, waiting_on).await? {
                Registration::Authorized => {
                    inner.sign_in.borrow_mut().take();
                    inner.signed_in.set(true);
                    return Ok(());
                }
                Registration::NeedsSignIn(url) => *inner.sign_in.borrow_mut() = Some(url),
            }
        }
    }

    /// Start following the tailnet. `on_devices` is called with the device
    /// list as JSON (`{ self: {...}, peers: [...] }`) whenever it changes,
    /// and `on_closed` once if the control server stops sending updates.
    /// Resolves when the first request has been accepted. The device must be
    /// signed in.
    pub async fn watch(
        &self,
        on_devices: js_sys::Function,
        on_closed: js_sys::Function,
    ) -> Result<(), Error> {
        let inner = self.inner.clone();
        if !inner.signed_in.get() {
            return Err(Error::NotSignedIn);
        }
        let connection = inner.connection().await?;
        let updates = connection.updates(&inner.config, &inner.keys).await?;
        wasm_bindgen_futures::spawn_local(async move {
            let mut updates = std::pin::pin!(updates);
            while let Some(update) = updates.next().await {
                let changed = inner.net.netmap.borrow_mut().apply(update);
                inner.sync_engine().await;
                if !changed {
                    continue;
                }
                let json = inner.net.netmap.borrow().to_json();
                if let Some(json) = json {
                    // A page callback that throws must not stop the updates.
                    drop(on_devices.call1(&JsValue::NULL, &json.into()));
                }
            }
            tracing::info!("the control server stopped sending updates");
            drop(on_closed.call0(&JsValue::NULL));
        });
        Ok(())
    }

    /// The relays the control server told us about, as JSON:
    /// `[{ id, code, name, hosts: ["derp1.tailscale.com", ...] }]`.
    /// Empty until [`Client::watch`] has received its first update.
    pub fn relays(&self) -> String {
        let netmap = self.inner.net.netmap.borrow();
        let list: Vec<_> = netmap
            .relays()
            .iter()
            .map(|(id, region)| {
                let hosts: Vec<String> = region
                    .servers
                    .iter()
                    .filter(|s| !s.stun_only)
                    .map(|s| match s.https_port {
                        443 => s.hostname.clone(),
                        port => format!("{}:{port}", s.hostname),
                    })
                    .collect();
                serde_json::json!({
                    "id": id.0.get(),
                    "code": region.info.code,
                    "name": region.info.name,
                    "hosts": hosts,
                })
            })
            .collect();
        serde_json::Value::Array(list).to_string()
    }
}

#[wasm_bindgen]
impl Client {
    /// Diagnostic: open a TCP connection to `host` (a device name or tailnet
    /// address) on `port`, send `request`, and resolve to the first bytes of
    /// the answer as text. For example, sending `GET / HTTP/1.0\r\n\r\n` to
    /// port 80 returns the start of a web server's reply.
    #[wasm_bindgen(js_name = tcpProbe)]
    pub async fn tcp_probe(&self, host: String, port: u16, request: String) -> Result<String, Error> {
        let inner = self.inner.clone();
        let address = inner
            .net
            .netmap
            .borrow()
            .resolve(&host)
            .ok_or_else(|| Error::Network(format!("no device is called {host}")))?;
        let stream = inner.net.engine.connect(std::net::SocketAddr::new(address, port)).await?;
        let mut left = request.as_bytes();
        while !left.is_empty() {
            let sent = stream.send(left).await.map_err(|e| Error::Network(e.to_string()))?;
            left = &left[sent..];
        }
        let mut answer = vec![0u8; 2048];
        let got = stream.recv(&mut answer).await.map_err(|e| Error::Network(e.to_string()))?;
        Ok(String::from_utf8_lossy(&answer[..got]).into_owned())
    }
}

/// Diagnostic: resolves if this browser can pass a packet through the relay
/// at `host` (for example `derp1.tailscale.com`). Needs no sign-in.
#[cfg(feature = "diagnostics")]
#[wasm_bindgen(js_name = relayCheck)]
pub async fn relay_check(host: String) -> Result<(), Error> {
    relay::check(&host).await
}

/// Diagnostic: resolves if two throwaway devices in this browser can set up
/// WireGuard through the relay at `host` and exchange data over TCP. Needs
/// no sign-in.
#[cfg(feature = "diagnostics")]
#[wasm_bindgen(js_name = pathCheck)]
pub async fn path_check(host: String) -> Result<(), Error> {
    crate::selftest::path(&host).await
}

/// Diagnostic: resolves if, between two throwaway devices in this browser,
/// an HTTP request, an HTTPS request with its certificate checked, a cookie
/// round trip and a WebSocket all work. Needs no sign-in.
#[cfg(feature = "diagnostics")]
#[wasm_bindgen(js_name = siteCheck)]
pub async fn site_check(host: String) -> Result<(), Error> {
    crate::selftest::site(&host).await
}
