//! The tab's picture of the tailnet: itself, the other devices, and the
//! relays. The control server sends a full picture first and changes after.

use std::{
    collections::{BTreeMap, HashMap},
    net::IpAddr,
};

use serde::Serialize;
use ts_control::{DerpMap, Node, NodeId, NodeStatus, PeerUpdate, StateUpdate};

use crate::engine::Peer;

#[derive(Default)]
pub struct Netmap {
    me: Option<Node>,
    peers: BTreeMap<NodeId, Node>,
    relays: DerpMap,
    /// The engine names devices by small numbers; this keeps each device's
    /// number the same for as long as we know it.
    numbers: HashMap<NodeId, u32>,
    next_number: u32,
}

impl Netmap {
    /// Take in one update. Returns whether anything the page shows changed.
    pub fn apply(&mut self, update: StateUpdate) -> bool {
        let mut changed = false;
        if let Some(relays) = update.derp {
            self.relays = relays;
        }
        if let Some(me) = update.node {
            changed |= self.me.as_ref() != Some(&me);
            self.me = Some(me);
        }
        match update.peer_update {
            Some(PeerUpdate::Full(peers)) => {
                self.peers = peers.into_iter().map(|p| (p.id, p)).collect();
                changed = true;
            }
            Some(PeerUpdate::Delta { patch, upsert, remove }) => {
                for update in &patch {
                    if let Some(peer) = self.peers.get_mut(&update.id) {
                        peer.apply_update(update);
                        changed = true;
                    }
                }
                for peer in upsert {
                    self.peers.insert(peer.id, peer);
                    changed = true;
                }
                for id in remove {
                    changed |= self.peers.remove(&id).is_some();
                }
            }
            None => {}
        }
        changed
    }

    pub fn relays(&self) -> &DerpMap {
        &self.relays
    }

    /// This device's tailnet addresses, once the control server has said.
    pub fn addresses(&self) -> Option<Vec<IpAddr>> {
        self.me.as_ref().map(addresses)
    }

    /// Where each relay region can be reached: region number to host name.
    pub fn relay_hosts(&self) -> HashMap<u32, String> {
        self.relays
            .iter()
            .filter_map(|(id, region)| {
                let server = region.servers.iter().find(|s| !s.stun_only)?;
                let host = match server.https_port {
                    443 => server.hostname.clone(),
                    port => format!("{}:{port}", server.hostname),
                };
                Some((id.0.get(), host))
            })
            .collect()
    }

    /// The other devices, as the engine wants them.
    pub fn engine_peers(&mut self) -> Vec<Peer> {
        let known: Vec<NodeId> = self.peers.keys().copied().collect();
        self.numbers.retain(|id, _| known.contains(id));
        let mut peers = Vec::with_capacity(known.len());
        for (id, node) in &self.peers {
            let number = *self.numbers.entry(*id).or_insert_with(|| {
                self.next_number += 1;
                self.next_number
            });
            peers.push(Peer {
                id: number,
                key: node.node_key,
                addresses: addresses(node),
                relay_region: node.derp_region.map(|r| r.0.get()),
            });
        }
        peers
    }

    /// The tailnet address of the device called `name`: a short name
    /// (`nas`), a full one (`nas.tail1234.ts.net`), or an address already.
    pub fn resolve(&self, name: &str) -> Option<IpAddr> {
        if let Ok(address) = name.parse() {
            return Some(address);
        }
        let name = name.to_ascii_lowercase();
        self.peers
            .values()
            .chain(self.me.as_ref())
            .find(|node| node.matches_name(&name))
            .map(|node| node.tailnet_address.ipv4.addr().into())
    }

    /// The picture as JSON, in the shape the Go client gave the page:
    /// `{ self: { name, addresses, nodeKey }, peers: [{ ..., online }] }`.
    /// Nothing until the control server has told us who we are.
    pub fn to_json(&self) -> Option<String> {
        let me = self.me.as_ref()?;
        let view = View {
            me: Device::of(me, None),
            peers: self
                .peers
                .values()
                .map(|p| Device::of(p, Some(matches!(p.status, NodeStatus::Online))))
                .collect(),
        };
        serde_json::to_string(&view).ok()
    }
}

fn addresses(node: &Node) -> Vec<IpAddr> {
    vec![
        node.tailnet_address.ipv4.addr().into(),
        node.tailnet_address.ipv6.addr().into(),
    ]
}

#[derive(Serialize)]
struct View {
    #[serde(rename = "self")]
    me: Device,
    peers: Vec<Device>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Device {
    name: String,
    addresses: Vec<String>,
    node_key: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    online: Option<bool>,
}

impl Device {
    fn of(node: &Node, online: Option<bool>) -> Self {
        Self {
            // With the trailing dot, as the Go client writes names.
            name: node.fqdn(true),
            addresses: vec![
                node.tailnet_address.ipv4.addr().to_string(),
                node.tailnet_address.ipv6.addr().to_string(),
            ],
            node_key: node.node_key.to_string(),
            online,
        }
    }
}
