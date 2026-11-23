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

/// How long one step of setting up the home relay may take before it is
/// abandoned and tried again.
const STEP_LIMIT_MS: u32 = 10_000;

/// Resolves to `step`'s result, or to an error if it takes too long.
async fn limited<T>(what: &'static str, step: impl Future<Output = Result<T, Error>>) -> Result<T, Error> {
    use futures_util::future::{Either, select};
    let limit = gloo_timers::future::TimeoutFuture::new(STEP_LIMIT_MS);
    match select(std::pin::pin!(step), limit).await {
        Either::Left((result, _)) => result,
        Either::Right(_) => Err(Error::TimedOut(what)),
    }
}

/// If this code crashes, everything in the tab that waits on it waits
/// forever. Besides printing the crash to the console, tell the page, which
/// can set `globalThis.__portholeClientCrashed` to a function to hear of it.
fn report_crashes() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        std::panic::set_hook(Box::new(|crash| {
            console_error_panic_hook::hook(crash);
            let listener = js_sys::Reflect::get(&js_sys::global(), &"__portholeClientCrashed".into());
            if let Ok(listener) = listener.and_then(|l| l.dyn_into::<js_sys::Function>()) {
                let _ = listener.call1(&JsValue::NULL, &crash.to_string().into());
            }
        }));
    });
}

#[wasm_bindgen(start)]
fn start() {
    report_crashes();
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
    /// Callers of `ready()` waiting for the home relay.
    ready_waiters: RefCell<Vec<futures_channel::oneshot::Sender<()>>>,
    /// Whatever a check needs kept alive for as long as the client is.
    #[cfg(feature = "diagnostics")]
    #[allow(dead_code, reason = "held only so it is dropped with the client")]
    keep: RefCell<Option<Box<dyn std::any::Any>>>,
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
            tracing::info!(devices = peers.len(), relays = relays.len(), "first picture of the tailnet received");
            let this = self.clone();
            wasm_bindgen_futures::spawn_local(async move {
                // Until this succeeds no other device can answer us, so keep
                // trying, a little less often each time.
                let mut wait_ms = 1000;
                while let Err(error) = this.choose_home_relay(&relays).await {
                    tracing::warn!(%error, wait_ms, "could not set a home relay; other devices cannot answer yet");
                    gloo_timers::future::TimeoutFuture::new(wait_ms).await;
                    wait_ms = (wait_ms * 2).min(15_000);
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
            .ok_or_else(|| Error::Relay("the control server listed no relays".into()))?;
        tracing::info!(region, host = relays.get(&region).map(String::as_str), "nearest relay chosen");
        limited("connecting to the home relay", self.net.engine.listen_on(region)).await?;
        tracing::info!(region, "connected to the home relay");
        let connection = self.connection().await?;
        limited(
            "telling the control server our home relay",
            connection.set_home_relay(&self.config, &self.keys, region),
        )
        .await?;
        self.home_relay.set(Some(region));
        tracing::info!(region, "home relay set");
        for waiter in self.ready_waiters.borrow_mut().drain(..) {
            let _ = waiter.send(());
        }
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
                ready_waiters: RefCell::default(),
                #[cfg(feature = "diagnostics")]
                keep: RefCell::default(),
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
            // Nowhere to send the user; `waitForSignIn` waits for the admin.
            Registration::NeedsApproval => Ok(Some(String::new())),
        }
    }

    /// Resolves once the user has finished signing in at the address from
    /// [`Client::sign_in_address`]. If the tailnet makes an admin approve new
    /// devices, `on_approval_needed` is called once and this keeps waiting
    /// until that has happened.
    #[wasm_bindgen(js_name = waitForSignIn)]
    pub async fn wait_for_sign_in(&self, on_approval_needed: Option<js_sys::Function>) -> Result<(), Error> {
        let inner = self.inner.clone();
        let connection = inner.connection().await?;
        let mut told = false;
        loop {
            let waiting_on = inner.sign_in.borrow().clone();
            match connection.register(&inner.config, &inner.keys, waiting_on).await? {
                Registration::Authorized => {
                    inner.sign_in.borrow_mut().take();
                    inner.signed_in.set(true);
                    return Ok(());
                }
                Registration::NeedsSignIn(url) => *inner.sign_in.borrow_mut() = Some(url),
                Registration::NeedsApproval => {
                    inner.sign_in.borrow_mut().take();
                    if !told {
                        told = true;
                        if let Some(callback) = &on_approval_needed {
                            let _ = callback.call0(&JsValue::NULL);
                        }
                    }
                    gloo_timers::future::sleep(std::time::Duration::from_secs(3)).await;
                }
            }
        }
    }

    /// Resolves once other devices can answer this one: a home relay has
    /// been chosen and the control server told. Requests made earlier work,
    /// but their first answers may be lost and retried. Call after
    /// [`Client::watch`].
    pub async fn ready(&self) {
        loop {
            // 0 marks a choice in progress.
            if self.inner.home_relay.get().is_some_and(|region| region != 0) {
                return;
            }
            let (waiter, chosen) = futures_channel::oneshot::channel();
            self.inner.ready_waiters.borrow_mut().push(waiter);
            if chosen.await.is_err() {
                return;
            }
        }
    }

    /// Sign this device out of the tailnet. Its keys stop working, so the
    /// saved state should be discarded.
    #[wasm_bindgen(js_name = signOut)]
    pub async fn sign_out(&self) -> Result<(), Error> {
        let inner = self.inner.clone();
        let connection = inner.connection().await?;
        connection.sign_out(&inner.config, &inner.keys).await?;
        inner.signed_in.set(false);
        Ok(())
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

#[cfg(feature = "diagnostics")]
impl Client {
    /// A client that is in no tailnet: it reaches whatever `net` was set up
    /// to reach, and holds on to `keep`.
    pub(crate) fn outside_a_tailnet(net: Rc<Net>, keep: Box<dyn std::any::Any>) -> Client {
        let keys = NodeState::generate();
        Client {
            inner: Rc::new(Inner {
                site: Site::new(Tunnel(net.clone()), tls::config()),
                net,
                config: Config {
                    server_url: ts_control::DEFAULT_CONTROL_SERVER.clone(),
                    hostname: None,
                    client_name: None,
                    tags: Vec::new(),
                    ephemeral: true,
                },
                keys,
                connection: RefCell::new(None),
                sign_in: RefCell::new(None),
                signed_in: std::cell::Cell::new(false),
                home_relay: std::cell::Cell::new(None),
                ready_waiters: RefCell::default(),
                keep: RefCell::new(Some(keep)),
            }),
        }
    }
}

/// Diagnostic: resolves to a client connected to one other throwaway device,
/// called `device-b.test`, which serves a web page on port 80 and an echoing
/// WebSocket on port 8080. It lets a page try `request`, `websocket` and the
/// cookie calls with no sign-in.
#[cfg(feature = "diagnostics")]
#[wasm_bindgen(js_name = pairedClient)]
pub async fn paired_client(host: String) -> Result<Client, Error> {
    crate::selftest::paired_client(&host).await
}

/// Diagnostic: resolves to the device list, as JSON, that a made-up update
/// from the control server produces. It takes the path a real update takes.
#[cfg(feature = "diagnostics")]
#[wasm_bindgen(js_name = netmapCheck)]
pub async fn netmap_check() -> Result<String, Error> {
    crate::selftest::netmap().await
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
