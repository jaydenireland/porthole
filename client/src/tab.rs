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
    netmap::Netmap,
    relay,
};

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

struct Inner {
    config: Config,
    keys: NodeState,
    connection: RefCell<Option<Rc<Connection>>>,
    sign_in: RefCell<Option<Url>>,
    signed_in: std::cell::Cell<bool>,
    netmap: RefCell<Netmap>,
}

impl Inner {
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
    inner: Rc<Inner>,
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
        Ok(Client {
            inner: Rc::new(Inner {
                config,
                keys,
                connection: RefCell::new(None),
                sign_in: RefCell::new(None),
                signed_in: std::cell::Cell::new(false),
                netmap: RefCell::new(Netmap::default()),
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
                let changed = inner.netmap.borrow_mut().apply(update);
                if !changed {
                    continue;
                }
                let json = inner.netmap.borrow().to_json();
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
        let netmap = self.inner.netmap.borrow();
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

/// Diagnostic: resolves if this browser can pass a packet through the relay
/// at `host` (for example `derp1.tailscale.com`). Needs no sign-in.
#[wasm_bindgen(js_name = relayCheck)]
pub async fn relay_check(host: String) -> Result<(), Error> {
    relay::check(&host).await
}
