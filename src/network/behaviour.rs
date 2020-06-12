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
    block_requests, debug_info,
    discovery::{DiscoveryBehaviour, DiscoveryConfig, DiscoveryOut},
    generic_proto, legacy_message,
};

use alloc::collections::VecDeque;
use core::{
    iter,
    num::NonZeroU64,
    task::{Context, Poll},
};
use hashbrown::HashMap;
use libp2p::core::{Multiaddr, PeerId, PublicKey};
use libp2p::kad::record;
use libp2p::swarm::{NetworkBehaviourAction, NetworkBehaviourEventProcess, PollParameters};
use libp2p::NetworkBehaviour;
use log::debug;
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
    /// Block request handling.
    block_requests: block_requests::BlockRequests,
    // TODO: light_client_handler: protocol::LightClientHandler,

    // TODO: this is a stupid translation layer between block_requests and a proper API
    // because block_requests is copy-pasted from Substrate, and because we want to be able to
    // easily refresh it, we need some API adjustments
    #[behaviour(ignore)]
    pending_block_requests: HashMap<(PeerId, legacy_message::BlockRequest), BlocksRequestId>,
    #[behaviour(ignore)]
    next_block_request_id: BlocksRequestId,

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
    BlockAnnounce(ScaleBlockHeader),
    BlocksResponse {
        id: BlocksRequestId,
        // TODO: proper error type
        result: Result<Vec<BlockData>, ()>,
    },
}

/// SCALE-encoded block header.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScaleBlockHeader(pub Vec<u8>);

/// Block data sent in the response.
#[derive(Debug, PartialEq, Eq, Clone)]
pub struct BlockData {
    /// Block header hash.
    pub hash: H256,
    /// SCALE-encoded block header if requested.
    pub header: Option<ScaleBlockHeader>,
    /// Block body if requested.
    pub body: Option<Vec<Extrinsic>>,
    /// Justification if requested.
    pub justification: Option<Vec<u8>>,
}

#[derive(Debug, PartialEq, Eq, Clone, Default)]
pub struct Extrinsic(pub Vec<u8>);

// TODO: the BlocksRequestConfig and all derivates should be in the block_requests module, but the
// block_requests module at the moment is more or less copy-pasted from upstream Substrate, so we
// have them here to make it easier to update the code
/// Description of a blocks request that the network must perform.
#[derive(Debug, PartialEq, Eq)]
pub struct BlocksRequestConfig {
    /// First block that the remote must return.
    pub start: BlocksRequestConfigStart,
    /// Number of blocks to request. The remote is free to return fewer blocks than requested.
    pub desired_count: u32,
    /// Whether the first block should be the one with the highest number, of the one with the
    /// lowest number.
    pub direction: BlocksRequestDirection,
    /// Which fields should be present in the response.
    pub fields: BlocksRequestFields,
}

#[derive(Debug, PartialEq, Eq)]
pub enum BlocksRequestDirection {
    Ascending,
    Descending,
}

#[derive(Debug, PartialEq, Eq)]
pub struct BlocksRequestFields {
    pub header: bool,
    pub body: bool,
    pub justification: bool,
}

/// Which block the remote must return first.
#[derive(Debug, PartialEq, Eq)]
pub enum BlocksRequestConfigStart {
    /// Hash of the block.
    ///
    /// > **Note**: As a reminder, the hash of a block is the same thing as the hash of its header.
    Hash(H256),
    /// Number of the block, where 0 would be the genesis block.
    Number(NonZeroU64),
}

#[derive(Debug, Copy, Clone, Hash, PartialEq, Eq)]
pub struct BlocksRequestId(u64);

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

        let block_requests = block_requests::BlockRequests::new(block_requests::Config::new(
            &chain_spec_protocol_id,
        ));

        Behaviour {
            legacy,
            debug_info: debug_info::DebugInfoBehaviour::new(user_agent, local_public_key.clone()),
            block_requests,
            discovery: {
                let mut cfg = DiscoveryConfig::new(local_public_key);
                cfg.with_user_defined(known_addresses);
                cfg.with_mdns(enable_mdns);
                cfg.allow_private_ipv4(allow_private_ipv4);
                cfg.discovery_limit(discovery_only_if_under_num);
                cfg.add_protocol(chain_spec_protocol_id);
                cfg.finish()
            },
            pending_block_requests: Default::default(),
            next_block_request_id: BlocksRequestId(0),
            local_best_hash,
            local_genesis_hash,
            events: VecDeque::new(),
        }
    }

    pub fn send_block_data_request(&mut self, config: BlocksRequestConfig) -> BlocksRequestId {
        let request_id = self.next_block_request_id;
        self.next_block_request_id.0 += 1;

        let target = match self.legacy.open_peers().next() {
            Some(p) => p.clone(),
            None => {
                self.events.push_back(BehaviourOut::BlocksResponse {
                    id: request_id,
                    result: Err(()),
                });
                return request_id;
            }
        };

        let legacy = legacy_message::BlockRequest {
            id: 0,
            fields: {
                let mut fields = legacy_message::BlockAttributes::empty();
                if config.fields.header {
                    fields |= legacy_message::BlockAttributes::HEADER;
                }
                if config.fields.body {
                    fields |= legacy_message::BlockAttributes::BODY;
                }
                if config.fields.justification {
                    fields |= legacy_message::BlockAttributes::JUSTIFICATION;
                }
                fields
            },
            from: match config.start {
                BlocksRequestConfigStart::Hash(_h) => unimplemented!(),
                BlocksRequestConfigStart::Number(n) => legacy_message::FromBlock::Number(n.get()),
            },
            to: None,
            direction: match config.direction {
                BlocksRequestDirection::Ascending => legacy_message::Direction::Ascending,
                BlocksRequestDirection::Descending => legacy_message::Direction::Descending,
            },
            max: Some(config.desired_count),
        };

        match self.block_requests.send_request(&target, legacy.clone()) {
            block_requests::SendRequestOutcome::EncodeError(_) => panic!(),
            block_requests::SendRequestOutcome::Ok => {
                self.pending_block_requests
                    .insert((target, legacy), request_id);
            }
            block_requests::SendRequestOutcome::Replaced { previous, .. } => {
                self.pending_block_requests
                    .insert((target.clone(), legacy), request_id);
                let previous = self
                    .pending_block_requests
                    .remove(&(target, previous))
                    .unwrap();
                self.events.push_back(BehaviourOut::BlocksResponse {
                    id: previous,
                    result: Err(()),
                });
            }
            block_requests::SendRequestOutcome::NotConnected => {
                self.events.push_back(BehaviourOut::BlocksResponse {
                    id: request_id,
                    result: Err(()),
                });
            }
        }

        request_id
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
                        self.events
                            .push_back(BehaviourOut::BlockAnnounce(ScaleBlockHeader(
                                announcement.header.encode(),
                            )));
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

impl NetworkBehaviourEventProcess<block_requests::Event> for Behaviour {
    fn inject_event(&mut self, out: block_requests::Event) {
        match out {
            block_requests::Event::AnsweredRequest { .. } => {}
            block_requests::Event::Response {
                peer,
                original_request,
                response,
                ..
            } => {
                let id = self
                    .pending_block_requests
                    .remove(&(peer, original_request))
                    .unwrap();
                self.events.push_back(BehaviourOut::BlocksResponse {
                    id,
                    result: Ok(response
                        .blocks
                        .into_iter()
                        .map(|data| BlockData {
                            hash: data.hash,
                            header: data.header.map(|header| ScaleBlockHeader(header.encode())),
                            body: data
                                .body
                                .map(|body| body.into_iter().map(|ext| Extrinsic(ext.0)).collect()),
                            justification: data.justification,
                        })
                        .collect()),
                });
            }
            block_requests::Event::RequestCancelled {
                peer,
                original_request,
                ..
            }
            | block_requests::Event::RequestTimeout {
                peer,
                original_request,
                ..
            } => {
                let id = self
                    .pending_block_requests
                    .remove(&(peer, original_request))
                    .unwrap();
                self.events.push_back(BehaviourOut::BlocksResponse {
                    id,
                    result: Err(()),
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
