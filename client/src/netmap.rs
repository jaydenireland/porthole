//! The tab's picture of the tailnet: itself, the other devices, and the
//! relays. The control server sends a full picture first and changes after.

use std::collections::BTreeMap;

use serde::Serialize;
use ts_control::{DerpMap, Node, NodeId, NodeStatus, PeerUpdate, StateUpdate};

#[derive(Default)]
pub struct Netmap {
    me: Option<Node>,
    peers: BTreeMap<NodeId, Node>,
    relays: DerpMap,
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

    pub fn peers(&self) -> impl Iterator<Item = &Node> {
        self.peers.values()
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
