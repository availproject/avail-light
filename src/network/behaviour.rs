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
    discovery::{DiscoveryBehaviour, DiscoveryConfig, DiscoveryOut},
    generic_proto, legacy_message, request_responses,
};

use alloc::{borrow::Cow, collections::VecDeque};
use core::{
    iter,
    task::{Context, Poll},
    time::Duration,
};
use libp2p::core::{Multiaddr, PeerId, PublicKey};
use libp2p::kad::record;
use libp2p::swarm::{NetworkBehaviourAction, NetworkBehaviourEventProcess, PollParameters};
use libp2p::NetworkBehaviour;
use parity_scale_codec::{DecodeAll, Encode};
use primitive_types::H256;

/// General behaviour of the network. Combines all protocols together.
#[derive(NetworkBehaviour)]
#[behaviour(out_event = "BehaviourOut", poll_method = "poll")]
pub struct Behaviour {
    /// Generic protocol. To remove at some point.
    legacy: generic_proto::GenericProto,
    /// Periodically pings and identifies the nodes we are connected to, and store information in a
    /// cache.
    debug_info: debug_info::DebugInfoBehaviour,
    /// Discovers nodes of the network.
    discovery: DiscoveryBehaviour,
    /// All request-response protocols: blocks, light client requests, and so on.
    request_responses: request_responses::RequestResponsesBehaviour,

    #[behaviour(ignore)]
    local_best_hash: H256,
    #[behaviour(ignore)]
    local_genesis_hash: H256,

    /// Queue of events to produce for the outside.
    #[behaviour(ignore)]
    events: VecDeque<BehaviourOut>,
}

#[derive(Debug)]
pub enum BehaviourOut {
    /// An announcement about a block has been gossiped to us.
    BlockAnnounce(super::ScaleBlockHeader),

    /// We have received a request from a peer and answered it.
    ///
    /// This event is generated for statistics purposes.
    InboundRequest {
        /// Peer which sent us a request.
        peer: PeerId,
        /// Protocol name of the request.
        protocol: Cow<'static, str>,
        /// If `Ok`, contains the time elapsed between when we received the request and when we
        /// sent back the response. If `Err`, the error that happened.
        outcome: Result<Duration, request_responses::InboundError>,
    },

    /// A request initiated using [`Behaviour::send_request`] has succeeded or failed.
    RequestFinished {
        /// Request that has succeeded.
        request_id: request_responses::RequestId,
        /// Response sent by the remote or reason for failure.
        outcome: Result<Vec<u8>, request_responses::OutboundFailure>,
    },
}

impl Behaviour {
    /// Builds a new `Behaviour`.
    pub async fn new(
        user_agent: String,
        chain_spec_protocol_id: Vec<u8>,
        local_public_key: PublicKey,
        known_addresses: Vec<(PeerId, Multiaddr)>,
        enable_mdns: bool,
        allow_private_ipv4: bool,
        discovery_only_if_under_num: u64,
        request_response_protocols: Vec<request_responses::ProtocolConfig>,
        local_best_hash: H256,
        local_genesis_hash: H256,
    ) -> Self {
        let peerset_config = sc_peerset::PeersetConfig {
            in_peers: 25,
            out_peers: 25,
            bootnodes: known_addresses.iter().map(|(p, _)| p.clone()).collect(),
            reserved_only: false,
            priority_groups: Default::default(),
        };

        let (peerset, _) = sc_peerset::Peerset::from_config(peerset_config);
        let legacy = generic_proto::GenericProto::new(
            local_public_key.clone().into_peer_id(),
            chain_spec_protocol_id.clone(),
            &[6, 5],
            peerset,
        );

        Behaviour {
            legacy,
            debug_info: debug_info::DebugInfoBehaviour::new(user_agent, local_public_key.clone()),
            discovery: {
                let mut cfg = DiscoveryConfig::new(local_public_key);
                cfg.with_user_defined(known_addresses);
                cfg.with_mdns(enable_mdns);
                cfg.allow_private_ipv4(allow_private_ipv4);
                cfg.discovery_limit(discovery_only_if_under_num);
                cfg.add_protocol(chain_spec_protocol_id);
                cfg.finish()
            },
            request_responses: {
                request_responses::RequestResponsesBehaviour::new(
                    request_response_protocols.into_iter(),
                )
                .unwrap()
            },
            local_best_hash,
            local_genesis_hash,
            events: VecDeque::new(),
        }
    }

    // TODO: document
    pub fn open_peers(&self) -> impl Iterator<Item = &PeerId> {
        self.legacy.open_peers()
    }

    /// Returns the list of nodes that we know exist in the network.
    pub fn known_peers(&mut self) -> impl Iterator<Item = PeerId> {
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

    /// Initiates sending a request.
    ///
    /// An error is returned if we are not connected to the target peer of if the protocol doesn't
    /// match one that has been registered.
    pub fn send_request(
        &mut self,
        target: &PeerId,
        protocol: &str,
        request: Vec<u8>,
    ) -> Result<request_responses::RequestId, request_responses::SendRequestError> {
        self.request_responses
            .send_request(target, protocol, request)
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
            info.listen_addrs.truncate(30);
        }
        for addr in &info.listen_addrs {
            self.discovery
                .add_self_reported_address(&peer_id, addr.clone());
        }
    }
}

impl NetworkBehaviourEventProcess<generic_proto::GenericProtoOut> for Behaviour {
    fn inject_event(&mut self, out: generic_proto::GenericProtoOut) {
        match out {
            generic_proto::GenericProtoOut::CustomProtocolOpen { peer_id } => {
                let message = legacy_message::Message::Status(legacy_message::Status {
                    version: 6,
                    min_supported_version: 6,
                    roles: legacy_message::Roles::LIGHT,
                    best_number: 0,
                    best_hash: self.local_best_hash,
                    genesis_hash: self.local_genesis_hash,
                    chain_status: Vec::new(),
                });

                self.legacy.send_packet(&peer_id, message.encode());
            }
            generic_proto::GenericProtoOut::CustomProtocolClosed { peer_id: _, .. } => {}
            generic_proto::GenericProtoOut::LegacyMessage {
                peer_id: _,
                message,
            } => {
                match legacy_message::Message::decode_all(&message) {
                    Ok(legacy_message::Message::BlockAnnounce(announcement)) => {
                        self.events.push_back(BehaviourOut::BlockAnnounce(
                            super::ScaleBlockHeader(announcement.header.encode()),
                        ));
                    }
                    Ok(legacy_message::Message::Status(_)) => {}
                    _msg => {} // TODO: for debugging println!("message from {:?} => {:?}", peer_id, msg),
                }
            }
            generic_proto::GenericProtoOut::Clogged { .. } => {}
            generic_proto::GenericProtoOut::Notification { .. } => {} // TODO: !
        }
    }
}

impl NetworkBehaviourEventProcess<DiscoveryOut> for Behaviour {
    fn inject_event(&mut self, out: DiscoveryOut) {
        // TODO:
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
            DiscoveryOut::RandomKademliaStarted(_) => {}
            DiscoveryOut::ValueFound(_results) => {
                //self.events.push(BehaviourOut::Event(Event::Dht(DhtEvent::ValueFound(results))));
            }
            DiscoveryOut::ValueNotFound(_key) => {
                //self.events.push(BehaviourOut::Event(Event::Dht(DhtEvent::ValueNotFound(key))));
            }
            DiscoveryOut::ValuePut(_key) => {
                //self.events.push(BehaviourOut::Event(Event::Dht(DhtEvent::ValuePut(key))));
            }
            DiscoveryOut::ValuePutFailed(_key) => {
                //self.events.push(BehaviourOut::Event(Event::Dht(DhtEvent::ValuePutFailed(key))));
            }
        }
    }
}

impl NetworkBehaviourEventProcess<request_responses::Event> for Behaviour {
    fn inject_event(&mut self, event: request_responses::Event) {
        match event {
            request_responses::Event::InboundRequest {
                peer,
                protocol,
                outcome,
            } => {
                self.events.push_back(BehaviourOut::InboundRequest {
                    peer,
                    protocol,
                    outcome,
                });
            }

            request_responses::Event::OutboundFinished {
                request_id,
                outcome,
            } => {
                self.events.push_back(BehaviourOut::RequestFinished {
                    request_id,
                    outcome,
                });
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
        if let Some(event) = self.events.pop_front() {
            return Poll::Ready(NetworkBehaviourAction::GenerateEvent(event));
        }

        Poll::Pending
    }
}
