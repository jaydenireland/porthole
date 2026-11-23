//! Moves packets between this tab and the other devices.
//!
//! Three layers, each from a library, joined here:
//!
//! ```text
//!   TCP connections        ts_netstack_smoltcp (smoltcp)
//!        | IP packets
//!   WireGuard              ts_dataplane, ts_tunnel
//!        | encrypted packets
//!   relay connections      ts_derp over WebSockets
//! ```
//!
//! tailscale-rs has a runtime that does this job natively, built on tokio's
//! thread pool and a set of actors. None of that runs in a browser, so this
//! is a small single-threaded replacement: a few tasks on the page's event
//! loop around the same packet-processing core (`ts_dataplane::DataPlane`).
//!
//! A tab can only reach other devices through relays, so there is no direct
//! (UDP) path, no NAT traversal, and no need to answer the messages devices
//! use to set those up.

use std::{
    cell::{Cell, RefCell},
    collections::{HashMap, HashSet},
    net::{IpAddr, SocketAddr},
    rc::Rc,
    sync::{Arc, LazyLock},
    time::Duration,
};

use futures_channel::{mpsc, oneshot};
use futures_util::{
    StreamExt,
    future::{AbortHandle, Either, abortable, select},
};
use ts_bart::RoutingTable;
use ts_dataplane::{
    DataPlane,
    overlay_router::{inbound::RouteAction as Inbound, outbound::RouteAction as Outbound},
    underlay_router,
};
use ts_keys::{NodeKeyPair, NodePublicKey};
use ts_netstack_smoltcp::{
    CreateSocket, Netstack, WakingPipe, WakingPipeDev, WakingPipeSender,
    netcore::{Channel, Config as StackConfig, HasChannel, NetstackControl},
    netsock::{TcpListener, TcpStream},
    smoltcp,
};
use ts_packet::PacketMut;
use ts_packetfilter::{Filter, PacketInfo};
use ts_transport::{DynEndpoint, OverlayTransportId, PeerId, UnderlayTransportId};

use crate::{error::Error, relay::Relay};

/// Send and receive buffer of each TCP connection, in bytes.
const TCP_BUFFER: usize = 128 * 1024;

/// The one TCP/IP stack in the tab.
const STACK: OverlayTransportId = OverlayTransportId(0);

/// Another device, as the engine needs to know it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Peer {
    /// A number that stays the same for this device across updates.
    pub id: u32,
    pub key: NodePublicKey,
    /// Its tailnet addresses.
    pub addresses: Vec<IpAddr>,
    /// The relay region it listens on, if it has told the control server.
    pub relay_region: Option<u32>,
}

/// Lets in every packet from a known device.
///
/// A tailnet's access rules normally decide which packets a device accepts.
/// tailscale-rs applies them to every arriving packet, replies included, so
/// with rules that forbid others from connecting to this device, the answers
/// to our own connections would be dropped too. This tab never listens for
/// connections, which makes the rules unnecessary here: a packet that is not
/// part of a connection we opened has no socket to go to, and the stack
/// discards it. The check that a packet's source address belongs to the
/// device that sent it still applies, in `DataPlane`.
struct RepliesOnly;

impl Filter for RepliesOnly {
    fn match_for(&self, _: &PacketInfo, _: &mut dyn Iterator<Item = &str>) -> Option<&str> {
        Some("replies")
    }
}

/// The stack's clock: time since the tab loaded this code.
fn stack_now() -> smoltcp::time::Instant {
    static START: LazyLock<web_time::Instant> = LazyLock::new(web_time::Instant::now);
    smoltcp::time::Instant::from_micros(START.elapsed().as_micros() as i64)
}

type Outgoing = mpsc::UnboundedSender<(NodePublicKey, PacketMut)>;

struct Inner {
    keys: NodeKeyPair,
    dataplane: RefCell<DataPlane>,
    /// Commands to the stack: open a socket, change addresses.
    stack: Channel,
    /// Packets for the stack.
    to_stack: WakingPipeSender,
    /// Tells the timer task that the next deadline may have moved.
    reschedule: mpsc::UnboundedSender<()>,
    /// Where each relay region is, and the open connections to them.
    relay_hosts: RefCell<HashMap<u32, String>>,
    relays: RefCell<HashMap<u32, Outgoing>>,
    /// The regions whose connection is up, and who is waiting to hear that
    /// this set changed.
    relays_up: RefCell<HashSet<u32>>,
    relay_waiters: RefCell<Vec<oneshot::Sender<()>>>,
    addresses: RefCell<Vec<IpAddr>>,
    next_port: Cell<u16>,
    tasks: RefCell<Vec<AbortHandle>>,
}

/// A running engine. Dropping it stops everything it started.
pub struct Engine {
    inner: Rc<Inner>,
}

impl Drop for Engine {
    fn drop(&mut self) {
        for task in self.inner.tasks.borrow_mut().drain(..) {
            task.abort();
        }
        self.inner.relays.borrow_mut().clear();
        self.inner.relay_waiters.borrow_mut().clear();
    }
}

impl Inner {
    /// Run `task` on the page's event loop until it ends or the engine is dropped.
    fn spawn(&self, task: impl Future<Output = ()> + 'static) {
        let (task, handle) = abortable(task);
        self.tasks.borrow_mut().push(handle);
        wasm_bindgen_futures::spawn_local(async move {
            let _ = task.await;
        });
    }

    /// A relay connection came up or went away.
    fn relay_changed(&self, region: u32, up: bool) {
        if up {
            self.relays_up.borrow_mut().insert(region);
        } else {
            self.relays_up.borrow_mut().remove(&region);
            self.relays.borrow_mut().remove(&region);
        }
        for waiter in self.relay_waiters.borrow_mut().drain(..) {
            let _ = waiter.send(());
        }
    }

    fn rescheduled(&self) {
        drop(self.reschedule.unbounded_send(()));
    }

    /// Hand encrypted packets to the relays they are routed to.
    fn send_to_peers(self: &Rc<Self>, batches: underlay_router::outbound::Result) {
        for ((transport, endpoint), packets) in batches {
            let Some(key) = endpoint.as_derp() else { continue };
            let Some(relay) = self.relay(transport.0) else { continue };
            for packet in packets {
                drop(relay.unbounded_send((key, packet)));
            }
        }
    }

    /// Hand decrypted packets to the stack.
    fn deliver(&self, batches: HashMap<OverlayTransportId, Vec<PacketMut>>) {
        for packet in batches.into_values().flatten() {
            self.to_stack.send(packet.as_ref());
        }
    }

    /// The queue for a relay region, connecting to it on first use.
    fn relay(self: &Rc<Self>, region: u32) -> Option<Outgoing> {
        if let Some(open) = self.relays.borrow().get(&region) {
            return Some(open.clone());
        }
        let host = self.relay_hosts.borrow().get(&region)?.clone();
        let (queue, mut queued) = mpsc::unbounded::<(NodePublicKey, PacketMut)>();
        self.relays.borrow_mut().insert(region, queue.clone());

        let this = self.clone();
        self.spawn(async move {
            let relay = match Relay::connect(&host, &this.keys).await {
                Ok(relay) => Rc::new(relay),
                Err(error) => {
                    tracing::warn!(%error, relay = host, "could not reach relay");
                    this.relay_changed(region, false);
                    return;
                }
            };

            this.relay_changed(region, true);

            let incoming = {
                let (this, relay) = (this.clone(), relay.clone());
                async move {
                    loop {
                        match relay.recv().await {
                            Ok((_from, packet)) => this.received(packet),
                            Err(error) => {
                                tracing::warn!(%error, "relay connection ended");
                                return;
                            }
                        }
                    }
                }
            };
            let outgoing = async {
                while let Some((to, packet)) = queued.next().await {
                    if let Err(error) = relay.send(to, packet.as_ref()).await {
                        tracing::warn!(%error, "could not send to relay");
                        return;
                    }
                }
            };
            // Whichever direction fails first ends the connection; the next
            // packet for this region opens a new one.
            select(std::pin::pin!(incoming), std::pin::pin!(outgoing)).await;
            this.relay_changed(region, false);
        });
        Some(queue)
    }

    /// An encrypted packet arrived from a relay.
    fn received(self: &Rc<Self>, packet: Vec<u8>) {
        let result = self.dataplane.borrow_mut().process_inbound([PacketMut::from(packet)]);
        self.deliver(result.to_local);
        self.send_to_peers(result.to_peers);
        self.rescheduled();
    }

    /// The stack produced a packet for another device.
    fn sending(self: &Rc<Self>, packet: PacketMut) {
        let result = self.dataplane.borrow_mut().process_outbound(vec![packet]);
        self.deliver(result.loopback);
        self.send_to_peers(result.to_peers);
        self.rescheduled();
    }
}

impl Engine {
    pub fn new(keys: NodeKeyPair) -> Self {
        let config = StackConfig {
            // How much of a download can be in flight at once. The default,
            // 16 KB, means 16 KB per round trip through the relay, which is
            // what made pages slow to load.
            tcp_buffer_size: TCP_BUFFER,
            ..StackConfig::default()
        };
        let (ours, theirs) = WakingPipe::unbounded();
        let device = WakingPipeDev { pipe: ours, mtu: config.mtu, medium: smoltcp::phy::Medium::Ip };
        let mut stack = Netstack::with_clock(device, config, &stack_now);
        let WakingPipe { rx: mut from_stack, tx: to_stack } = theirs;

        let mut dataplane = DataPlane::new(keys.clone());
        dataplane.packet_filter = Arc::new(RepliesOnly);

        let (reschedule, mut rescheduled) = mpsc::unbounded();
        let inner = Rc::new(Inner {
            keys,
            dataplane: RefCell::new(dataplane),
            stack: stack.command_channel(),
            to_stack,
            reschedule,
            relay_hosts: RefCell::default(),
            relays: RefCell::default(),
            relays_up: RefCell::default(),
            relay_waiters: RefCell::default(),
            addresses: RefCell::default(),
            // The range set aside for short-lived ports.
            next_port: Cell::new(49152),
            tasks: RefCell::default(),
        });

        // The stack's own loop: timers, retransmissions, socket commands.
        inner.spawn(async move {
            stack.run_with_sleep(async |wait| gloo_timers::future::sleep(wait).await).await;
        });

        // Packets the stack wants sent.
        let this = inner.clone();
        inner.spawn(async move {
            while let Some(packet) = from_stack.recv_async().await {
                this.sending(PacketMut::from(packet.as_ref()));
            }
        });

        // WireGuard's timers: handshake retries, key rotation, keepalives.
        let this = inner.clone();
        inner.spawn(async move {
            loop {
                let next = this.dataplane.borrow().next_event();
                let wait = match next {
                    Some(at) => at.saturating_duration_since(web_time::Instant::now()),
                    None => Duration::from_secs(3600),
                };
                let sleep = std::pin::pin!(gloo_timers::future::sleep(wait));
                match select(sleep, rescheduled.next()).await {
                    Either::Left(_) => {
                        let due = this.dataplane.borrow_mut().process_events();
                        this.send_to_peers(due);
                    }
                    Either::Right((Some(()), _)) => {}
                    Either::Right((None, _)) => return,
                }
            }
        });

        Self { inner }
    }

    /// Where each relay region can be reached: region number to host name.
    pub fn set_relays(&self, hosts: HashMap<u32, String>) {
        *self.inner.relay_hosts.borrow_mut() = hosts;
    }

    /// Connect to our home relay, where other devices send to us. Resolves
    /// once the relay knows we are there.
    pub async fn listen_on(&self, region: u32) -> Result<(), Error> {
        if self.inner.relay(region).is_none() {
            return Err(Error::Relay(format!("no relay is known for region {region}")));
        }
        loop {
            if self.inner.relays_up.borrow().contains(&region) {
                return Ok(());
            }
            if !self.inner.relays.borrow().contains_key(&region) {
                return Err(Error::Relay(format!("could not reach the relay for region {region}")));
            }
            let (waiter, changed) = oneshot::channel();
            self.inner.relay_waiters.borrow_mut().push(waiter);
            let _ = changed.await;
        }
    }

    /// This device's tailnet addresses.
    pub async fn set_addresses(&self, addresses: Vec<IpAddr>) -> Result<(), Error> {
        let mut routes = ts_bart::Table::default();
        for address in &addresses {
            routes.insert((*address).into(), Inbound::ToOverlay(STACK));
        }
        self.inner.dataplane.borrow_mut().or_in.swap(routes);
        self.inner
            .stack
            .set_ips(addresses.iter().copied())
            .await
            .map_err(|e| Error::Network(e.to_string()))?;
        *self.inner.addresses.borrow_mut() = addresses;
        Ok(())
    }

    /// Replace the set of devices we can talk to.
    pub fn set_peers(&self, peers: &[Peer]) {
        let mut dataplane = self.inner.dataplane.borrow_mut();
        let mut out_routes = ts_bart::Table::default();
        let mut sources = ts_bart::Table::default();
        let mut relay_routes = HashMap::new();

        for peer in peers {
            let id = PeerId(peer.id);
            dataplane.wireguard.upsert_peer(ts_tunnel::PeerConfig::new(
                ts_tunnel::PeerId(peer.id),
                peer.key,
                // Tailscale does not use WireGuard's optional extra key.
                [0u8; 32],
            ));
            for address in &peer.addresses {
                out_routes.insert((*address).into(), Outbound::Wireguard(id));
                sources.insert((*address).into(), id);
            }
            if let Some(region) = peer.relay_region {
                relay_routes.insert(id, (UnderlayTransportId(region), DynEndpoint::derp(peer.key)));
            }
        }

        let gone: Vec<PeerId> = dataplane
            .ur_out
            .table
            .keys()
            .filter(|id| !peers.iter().any(|p| p.id == id.0))
            .copied()
            .collect();
        for id in gone {
            dataplane.wireguard.remove_peer(ts_tunnel::PeerId(id.0));
        }

        dataplane.or_out.swap(out_routes);
        dataplane.src_filter_in = Arc::new(sources);
        dataplane.ur_out.table = relay_routes;
    }

    fn local(&self, remote: &SocketAddr) -> Result<SocketAddr, Error> {
        let address = self
            .inner
            .addresses
            .borrow()
            .iter()
            .copied()
            .find(|a| a.is_ipv4() == remote.is_ipv4())
            .ok_or_else(|| Error::Network("this device has no address of that kind yet".into()))?;
        let port = self.inner.next_port.get();
        self.inner.next_port.set(if port == u16::MAX { 49152 } else { port + 1 });
        Ok(SocketAddr::new(address, port))
    }

    /// Open a TCP connection to another device.
    pub async fn connect(&self, remote: SocketAddr) -> Result<TcpStream, Error> {
        let local = self.local(&remote)?;
        self.inner
            .stack
            .tcp_connect(local, remote)
            .await
            .map_err(|e| Error::Network(e.to_string()))
    }

    /// Accept TCP connections on `port`. The app never does this; it exists
    /// so two engines can be tested against each other.
    pub async fn listen(&self, port: u16) -> Result<TcpListener, Error> {
        let address = *self
            .inner
            .addresses
            .borrow()
            .first()
            .ok_or_else(|| Error::Network("this device has no address yet".into()))?;
        self.inner
            .stack
            .tcp_listen(SocketAddr::new(address, port))
            .await
            .map_err(|e| Error::Network(e.to_string()))
    }
}
