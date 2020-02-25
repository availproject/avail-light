// Copyright 2019-2020 Parity Technologies (UK) Ltd.
// This file is part of Substrate.

// Substrate is free software: you can redistribute it and/or modify
// it under the terms of the GNU General Public License as published by
// the Free Software Foundation, either version 3 of the License, or
// (at your option) any later version.

// Substrate is distributed in the hope that it will be useful,
// but WITHOUT ANY WARRANTY; without even the implied warranty of
// MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
// GNU General Public License for more details.

// You should have received a copy of the GNU General Public License
// along with Substrate.  If not, see <http://www.gnu.org/licenses/>.

use super::{
    debug_info,
    discovery::{DiscoveryBehaviour, DiscoveryOut},
    legacy_proto,
};

use core::{
    iter,
    task::{Context, Poll},
};
use libp2p::core::{Multiaddr, PeerId, PublicKey};
use libp2p::kad::record;
use libp2p::swarm::{NetworkBehaviourAction, NetworkBehaviourEventProcess, PollParameters};
use libp2p::NetworkBehaviour;
use log::debug;
use parity_scale_codec::{DecodeAll, Encode};
use primitive_types::U256;

/// General behaviour of the network. Combines all protocols together.
#[derive(NetworkBehaviour)]
#[behaviour(out_event = "BehaviourOut", poll_method = "poll")]
pub struct Behaviour {
    /// Legacy protocol. To remove at some point.
    legacy: legacy_proto::LegacyProto,
    /// Periodically pings and identifies the nodes we are connected to, and store information in a
    /// cache.
    debug_info: debug_info::DebugInfoBehaviour,
    /// Discovers nodes of the network.
    discovery: DiscoveryBehaviour,
    /*/// Block request handling.
    block_requests: protocol::BlockRequests<B>,
    /// Light client request handling.
    light_client_handler: protocol::LightClientHandler<B>,*/
    /// Queue of events to produce for the outside.
    #[behaviour(ignore)]
    events: Vec<BehaviourOut>,
}

#[derive(Debug)]
pub enum BehaviourOut {
    /// An announcement about a block has been gossiped to us.
    BlockAnnounce(BlockHeader),
}

/// Abstraction over a block header for a substrate chain.
#[derive(Debug, PartialEq, Eq, Clone)]
pub struct BlockHeader {
    /// The parent hash.
    pub parent_hash: U256,
    /// The block number.
    pub number: u32,
    /// The state trie merkle root
    pub state_root: U256,
    /// The merkle root of the extrinsics.
    pub extrinsics_root: U256,
}

impl Behaviour {
    /// Builds a new `Behaviour`.
    pub async fn new(
        user_agent: String,
        local_public_key: PublicKey,
        known_addresses: Vec<(PeerId, Multiaddr)>,
        enable_mdns: bool,
        allow_private_ipv4: bool,
        discovery_only_if_under_num: u64,
    ) -> Self {
        let peerset_config = sc_peerset::PeersetConfig {
            in_peers: 25,
            out_peers: 25,
            bootnodes: Vec::new(),
            reserved_only: false,
            reserved_nodes: Vec::new(),
        };

        let (peerset, _) = sc_peerset::Peerset::from_config(peerset_config);
        let protocol_id = {
            let mut protocol_id = smallvec::SmallVec::new();
            protocol_id.push(b'f');
            protocol_id.push(b'i');
            protocol_id.push(b'r');
            protocol_id.push(b'5');
            protocol_id
        };
        let legacy = legacy_proto::LegacyProto::new(protocol_id, &[6, 5], peerset);

        Behaviour {
            legacy,
            debug_info: debug_info::DebugInfoBehaviour::new(user_agent, local_public_key.clone()),
            discovery: DiscoveryBehaviour::new(
                local_public_key,
                known_addresses,
                enable_mdns,
                allow_private_ipv4,
                discovery_only_if_under_num,
            )
            .await,
            events: Vec::new(),
        }
    }

    pub fn send_block_request(&mut self, block_num: u32) {
        let target = match self.legacy.open_peers().next() {
            Some(p) => p.clone(),
            None => return,
        };

        let message =
            legacy_proto::message::Message::BlockRequest(legacy_proto::message::BlockRequest {
                id: 0,
                fields: legacy_proto::message::BlockAttributes::HEADER,
                from: legacy_proto::message::FromBlock::Number(block_num),
                to: None,
                direction: legacy_proto::message::Direction::Ascending,
                max: None,
            });

        self.legacy.send_packet(&target, message.encode());
    }

    /// Returns the list of nodes that we know exist in the network.
    pub fn known_peers(&mut self) -> impl Iterator<Item = &PeerId> {
        self.discovery.known_peers()
    }

    /// Adds a hard-coded address for the given peer, that never expires.
    pub fn add_known_address(&mut self, peer_id: PeerId, addr: Multiaddr) {
        self.discovery.add_known_address(peer_id, addr)
    }

    /// Borrows `self` and returns a struct giving access to the information about a node.
    ///
    /// Returns `None` if we don't know anything about this node. Always returns `Some` for nodes
    /// we're connected to, meaning that if `None` is returned then we're not connected to that
    /// node.
    pub fn node(&self, peer_id: &PeerId) -> Option<debug_info::Node> {
        self.debug_info.node(peer_id)
    }

    /// Start querying a record from the DHT. Will later produce either a `ValueFound` or a `ValueNotFound` event.
    pub fn get_value(&mut self, key: &record::Key) {
        self.discovery.get_value(key);
    }

    /// Starts putting a record into DHT. Will later produce either a `ValuePut` or a `ValuePutFailed` event.
    pub fn put_value(&mut self, key: record::Key, value: Vec<u8>) {
        self.discovery.put_value(key, value);
    }
}

impl NetworkBehaviourEventProcess<void::Void> for Behaviour {
    fn inject_event(&mut self, event: void::Void) {
        void::unreachable(event)
    }
}

impl NetworkBehaviourEventProcess<debug_info::DebugInfoEvent> for Behaviour {
    fn inject_event(&mut self, event: debug_info::DebugInfoEvent) {
        let debug_info::DebugInfoEvent::Identified { peer_id, mut info } = event;
        if info.listen_addrs.len() > 30 {
            debug!(target: "sub-libp2p", "Node {:?} has reported more than 30 addresses; \
                it is identified by {:?} and {:?}", peer_id, info.protocol_version,
                info.agent_version
            );
            info.listen_addrs.truncate(30);
        }
        for addr in &info.listen_addrs {
            self.discovery
                .add_self_reported_address(&peer_id, addr.clone());
        }
    }
}

impl NetworkBehaviourEventProcess<legacy_proto::LegacyProtoOut> for Behaviour {
    fn inject_event(&mut self, out: legacy_proto::LegacyProtoOut) {
        match out {
            legacy_proto::LegacyProtoOut::CustomProtocolOpen {
                version,
                peer_id,
                endpoint,
            } => {
                let message =
                    legacy_proto::message::Message::Status(legacy_proto::message::Status {
                        version: 6,
                        min_supported_version: 6,
                        roles: legacy_proto::message::Roles::LIGHT,
                        best_number: 0,
                        best_hash:
                            "1d18dc9db0c97a2d3f1ab307ffcdea3445cbe8c54b5f2d491590db5366f84325"
                                .parse()
                                .unwrap(),
                        genesis_hash:
                            "1d18dc9db0c97a2d3f1ab307ffcdea3445cbe8c54b5f2d491590db5366f84325"
                                .parse()
                                .unwrap(),
                        chain_status: Vec::new(),
                    });

                self.legacy.send_packet(&peer_id, message.encode());
            }
            legacy_proto::LegacyProtoOut::CustomProtocolClosed { peer_id, .. } => {}
            legacy_proto::LegacyProtoOut::CustomMessage { peer_id, message } => {
                match legacy_proto::message::Message::decode_all(&message) {
                    Ok(legacy_proto::message::Message::BlockAnnounce(announcement)) => {
                        self.events.push(BehaviourOut::BlockAnnounce(BlockHeader {
                            parent_hash: announcement.header.parent_hash,
                            number: announcement.header.number,
                            state_root: announcement.header.state_root,
                            extrinsics_root: announcement.header.extrinsics_root,
                        }));
                    }
                    Ok(legacy_proto::message::Message::Status(_)) => {},
                    msg => println!("message from {:?} => {:?}", peer_id, message),
                }
                
            }
            legacy_proto::LegacyProtoOut::Clogged { .. } => {}
        }
    }
}

impl NetworkBehaviourEventProcess<DiscoveryOut> for Behaviour {
    fn inject_event(&mut self, out: DiscoveryOut) {
        match out {
            DiscoveryOut::UnroutablePeer(_peer_id) => {
                // Obtaining and reporting listen addresses for unroutable peers back
                // to Kademlia is handled by the `Identify` protocol, part of the
                // `DebugInfoBehaviour`. See the `NetworkBehaviourEventProcess`
                // implementation for `DebugInfoEvent`.
            }
            DiscoveryOut::Discovered(peer_id) => {
                self.legacy.add_discovered_nodes(iter::once(peer_id));
            }
            DiscoveryOut::ValueFound(results) => {
                //self.events.push(BehaviourOut::Event(Event::Dht(DhtEvent::ValueFound(results))));
            }
            DiscoveryOut::ValueNotFound(key) => {
                //self.events.push(BehaviourOut::Event(Event::Dht(DhtEvent::ValueNotFound(key))));
            }
            DiscoveryOut::ValuePut(key) => {
                //self.events.push(BehaviourOut::Event(Event::Dht(DhtEvent::ValuePut(key))));
            }
            DiscoveryOut::ValuePutFailed(key) => {
                //self.events.push(BehaviourOut::Event(Event::Dht(DhtEvent::ValuePutFailed(key))));
            }
        }
    }
}

impl Behaviour {
    fn poll<TEv>(
        &mut self,
        _: &mut Context,
        _: &mut impl PollParameters,
    ) -> Poll<NetworkBehaviourAction<TEv, BehaviourOut>> {
        if !self.events.is_empty() {
            return Poll::Ready(NetworkBehaviourAction::GenerateEvent(self.events.remove(0)));
        }

        Poll::Pending
    }
}
