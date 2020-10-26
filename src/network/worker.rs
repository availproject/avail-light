// Substrate-lite
// Copyright (C) 2019-2020  Parity Technologies (UK) Ltd.
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

// This program is free software: you can redistribute it and/or modify
// it under the terms of the GNU General Public License as published by
// the Free Software Foundation, either version 3 of the License, or
// (at your option) any later version.

// This program is distributed in the hope that it will be useful,
// but WITHOUT ANY WARRANTY; without even the implied warranty of
// MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
// GNU General Public License for more details.

// You should have received a copy of the GNU General Public License
// along with this program.  If not, see <http://www.gnu.org/licenses/>.

use super::{behaviour, legacy_message, request_responses, schema, transport};

use alloc::boxed::Box;
use core::{
    future::Future,
    num::{NonZeroU64, NonZeroUsize},
    pin::Pin,
    str,
    time::Duration,
};
use hashbrown::HashMap;
use libp2p::{
    swarm::{SwarmBuilder, SwarmEvent},
    wasm_ext, Multiaddr, PeerId, Swarm,
};
use parity_scale_codec::{DecodeAll, Encode};
use primitive_types::H256;
use prost::Message as _;
use rand::seq::IteratorRandom as _;

pub use request_responses::RequestId;

/// State machine representing the network currently running.
pub struct Network {
    swarm: Swarm<behaviour::Behaviour>,
    chain_spec_protocol_id: String,
    // TODO: meh
    request_types: HashMap<RequestId, RequestTy, fnv::FnvBuildHasher>,
}

enum RequestTy {
    Block,
    Call,
}

/// Event that can happen on the network.
#[derive(Debug)]
pub enum Event {
    /// An announcement about a block has been gossiped to us.
    BlockAnnounce(ScaleBlockHeader),

    /// A blocks request started with [`Network::start_block_request`] has gotten a response.
    BlocksRequestFinished {
        id: RequestId,
        result: Result<Vec<BlockData>, ()>,
    },

    /// A call request started with [`Network::start_call_request`] has gotten a response.
    CallRequestFinished {
        id: RequestId,
        // TODO: better type
        result: Result<Vec<u8>, ()>,
    },

    /// Established at least one connection with the given peer.
    Connected(PeerId),
    /// No longer have any connection with the given peer.
    Disconnected(PeerId),
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
    /// Peer to ask the block from.
    pub peer_id: PeerId,
    /// Number of blocks to request. The remote is free to return fewer blocks than requested.
    pub desired_count: u32,
    /// Whether the first block should be the one with the highest number, of the one with the
    /// lowest number.
    pub direction: BlocksRequestDirection,
    /// Which fields should be present in the response.
    pub fields: BlocksRequestFields,
}

/// Description of a call request that the network must perform.
#[derive(Debug, PartialEq, Eq)]
pub struct CallRequestConfig {
    /// Peer to ask to perform the call.
    pub peer_id: PeerId,
    /// Block to perform the call on.
    pub block: [u8; 32],
    /// Name of the Wasm entry point to call.
    pub method_name: String,
    /// SCALE-encoded parameter to pass to the Wasm entry point.
    pub encoded_input_parameter: Vec<u8>,
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

/// Configuration for starting the network.
pub struct Config {
    /// List of peer ids and their addresses that we know are part of the peer-to-peer network.
    // TODO: better type
    pub known_addresses: Vec<(PeerId, Multiaddr)>,

    /// Small string identifying the name of the chain, in order to detect incompatible nodes
    /// earlier.
    // TODO: better type
    pub chain_spec_protocol_id: Vec<u8>,

    /// How to spawn libp2p background tasks.
    pub tasks_executor: Box<dyn Fn(Pin<Box<dyn Future<Output = ()> + Send>>) + Send>,

    /// Hash of the genesis block of the local node.
    pub local_genesis_hash: [u8; 32],

    /// Optional external implementation of a libp2p transport. Used in WASM contexts where we
    /// need some binding between the networking provided by the operating system or environment
    /// and libp2p.
    ///
    /// This parameter exists whatever the target platform is, but it is expected to be set to
    /// `Some` only when compiling for WASM.
    pub wasm_external_transport: Option<wasm_ext::ExtTransport>,
}

impl Network {
    pub async fn start(config: Config) -> Self {
        let local_key_pair = libp2p::identity::Keypair::generate_ed25519();
        let local_public_key = local_key_pair.public();
        let local_peer_id = local_public_key.clone().into_peer_id();
        let (transport, _) =
            transport::build_transport(local_key_pair, false, config.wasm_external_transport, true);

        let chain_spec_protocol_id = str::from_utf8(&config.chain_spec_protocol_id)
            .unwrap()
            .to_owned(); // TODO: don't unwrap

        let behaviour = behaviour::Behaviour::new(
            "substrate-lite".to_string(),
            config.chain_spec_protocol_id,
            local_public_key,
            config.known_addresses,
            true,
            true,
            50,
            {
                let mut protocols = Vec::new();
                protocols.push(request_responses::ProtocolConfig {
                    name: format!("/{}/sync/2", chain_spec_protocol_id).into(),
                    max_request_size: 1024 * 1024,
                    max_response_size: 16 * 1024 * 1024,
                    request_timeout: Duration::from_secs(10),
                    requests_processing: None, // TODO:
                });
                protocols
            },
            // TODO: best hash != genesis_hash
            config.local_genesis_hash.clone().into(),
            config.local_genesis_hash.clone().into(),
        )
        .await;

        let swarm = {
            let mut builder = SwarmBuilder::new(transport, behaviour, local_peer_id)
                .peer_connection_limit(2)
                .notify_handler_buffer_size(NonZeroUsize::new(64).unwrap())
                .connection_event_buffer_size(256);

            struct SpawnImpl<F>(F);
            impl<F: Fn(Pin<Box<dyn Future<Output = ()> + Send>>)> libp2p::core::Executor for SpawnImpl<F> {
                fn exec(&self, f: Pin<Box<dyn Future<Output = ()> + Send>>) {
                    (self.0)(f)
                }
            }
            builder = builder.executor(Box::new(SpawnImpl(config.tasks_executor)));
            builder.build()
        };

        Network {
            swarm,
            chain_spec_protocol_id,
            request_types: Default::default(),
        }
    }

    /// Returns the [`PeerId`] of the local node.
    pub fn local_peer_id(&self) -> &PeerId {
        Swarm::local_peer_id(&self.swarm)
    }

    /// Sends out an announcement about the given block.
    pub async fn announce_block(&mut self) {
        unimplemented!()
    }

    /// Starts a block request on the network.
    ///
    /// Despite being asynchronous, this method only *starts* the request and does not wait for a
    /// response to come back. The method will block only in situations where the CPU is
    /// overwhelmed.
    pub async fn start_block_request(
        &mut self,
        config: BlocksRequestConfig,
    ) -> Result<RequestId, ()> {
        let request = {
            // TODO: don't use legacy stuff
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

            schema::v1::BlockRequest {
                fields: fields.to_be_u32(),
                from_block: match config.start {
                    BlocksRequestConfigStart::Hash(h) => {
                        Some(schema::v1::block_request::FromBlock::Hash(h.encode()))
                    }
                    BlocksRequestConfigStart::Number(n) => {
                        Some(schema::v1::block_request::FromBlock::Number(
                            parity_scale_codec::Encode::encode(&n.get()),
                        ))
                    }
                },
                to_block: Vec::new(),
                direction: match config.direction {
                    BlocksRequestDirection::Ascending => schema::v1::Direction::Ascending as i32,
                    BlocksRequestDirection::Descending => schema::v1::Direction::Descending as i32,
                },
                max_blocks: config.desired_count,
            }
        };

        let request_bytes = {
            let mut buf = Vec::with_capacity(request.encoded_len());
            if let Err(err) = request.encode(&mut buf) {
                return Err(());
            }
            buf
        };

        let request_id = self
            .swarm
            .send_request(
                &config.peer_id,
                &format!("/{}/sync/2", self.chain_spec_protocol_id),
                request_bytes,
            )
            .map_err(|_| ())?;

        self.request_types.insert(request_id, RequestTy::Block);
        Ok(request_id)
    }

    /// Starts a remote call request on the network.
    ///
    /// This requests a remote to perform a runtime call and return a proof of execution.
    ///
    /// Despite being asynchronous, this method only *starts* the request and does not wait for a
    /// response to come back. The method will block only in situations where the CPU is
    /// overwhelmed.
    pub async fn start_call_request(&mut self, config: CallRequestConfig) -> Result<RequestId, ()> {
        let request = schema::v1::light::RemoteCallRequest {
            block: config.block.to_vec(),
            method: config.method_name,
            data: config.encoded_input_parameter,
        };

        let request_bytes = {
            let mut buf = Vec::with_capacity(request.encoded_len());
            if let Err(err) = request.encode(&mut buf) {
                return Err(());
            }
            buf
        };

        let request_id = self
            .swarm
            .send_request(
                &config.peer_id,
                &format!("/{}/light/2", self.chain_spec_protocol_id),
                request_bytes,
            )
            .map_err(|_| ())?;

        self.request_types.insert(request_id, RequestTy::Call);
        Ok(request_id)
    }

    /// Returns the next event that happened on the network.
    pub async fn next_event(&mut self) -> Event {
        loop {
            match self.swarm.next_event().await {
                SwarmEvent::Behaviour(behaviour::BehaviourOut::BlockAnnounce(header)) => {
                    return Event::BlockAnnounce(header);
                }

                SwarmEvent::Behaviour(behaviour::BehaviourOut::RequestFinished {
                    request_id,
                    outcome: Ok(response_bytes),
                }) => {
                    match self.request_types.remove(&request_id).unwrap() {
                        RequestTy::Block => {
                            // TODO: don't unwrap
                            let response =
                                match schema::v1::BlockResponse::decode(&response_bytes[..]) {
                                    Ok(r) => r,
                                    Err(_) => {
                                        // TODO: proper error
                                        return Event::BlocksRequestFinished {
                                            id: request_id,
                                            result: Err(()),
                                        };
                                    }
                                };

                            let mut blocks = Vec::with_capacity(response.blocks.len());
                            for block in response.blocks {
                                let mut body = Vec::with_capacity(block.body.len());
                                for extrinsic in block.body {
                                    let ext = match Vec::<u8>::decode_all(&mut extrinsic.as_ref()) {
                                        Ok(e) => e,
                                        Err(_) => {
                                            // TODO: proper error
                                            return Event::BlocksRequestFinished {
                                                id: request_id,
                                                result: Err(()),
                                            };
                                        }
                                    };

                                    body.push(Extrinsic(ext));
                                }

                                let hash = match H256::decode_all(&mut block.hash.as_ref()) {
                                    Ok(e) => e,
                                    Err(_) => {
                                        // TODO: proper error
                                        return Event::BlocksRequestFinished {
                                            id: request_id,
                                            result: Err(()),
                                        };
                                    }
                                };

                                blocks.push(BlockData {
                                    hash,
                                    header: if !block.header.is_empty() {
                                        Some(ScaleBlockHeader(block.header))
                                    } else {
                                        None
                                    },
                                    // TODO: no; we might not have asked for the body
                                    body: Some(body),
                                    justification: if !block.justification.is_empty() {
                                        Some(block.justification)
                                    } else if block.is_empty_justification {
                                        Some(Vec::new())
                                    } else {
                                        None
                                    },
                                });
                            }

                            return Event::BlocksRequestFinished {
                                id: request_id,
                                result: Ok(blocks),
                            };
                        }
                        RequestTy::Call => todo!(),
                    }
                }
                SwarmEvent::Behaviour(behaviour::BehaviourOut::RequestFinished {
                    request_id,
                    outcome: Err(_err),
                }) => {
                    match self.request_types.remove(&request_id).unwrap() {
                        RequestTy::Block => {
                            // TODO: proper error
                            return Event::BlocksRequestFinished {
                                id: request_id,
                                result: Err(()),
                            };
                        }
                        RequestTy::Call => todo!(),
                    }
                }
                SwarmEvent::Behaviour(behaviour::BehaviourOut::InboundRequest { .. }) => {}

                SwarmEvent::ConnectionEstablished {
                    peer_id,
                    num_established,
                    ..
                } if num_established.get() == 1 => {
                    return Event::Connected(peer_id);
                }
                SwarmEvent::ConnectionEstablished { .. } => {}
                SwarmEvent::ConnectionClosed {
                    peer_id,
                    num_established: 0,
                    ..
                } => {
                    return Event::Disconnected(peer_id);
                }
                SwarmEvent::ConnectionClosed { .. } => {}
                SwarmEvent::NewListenAddr(_) => {}
                SwarmEvent::ExpiredListenAddr(_) => {}
                SwarmEvent::UnreachableAddr { .. } => {}
                SwarmEvent::Dialing(_peer_id) => {}
                SwarmEvent::IncomingConnection { .. } => {}
                SwarmEvent::IncomingConnectionError { .. } => {}
                SwarmEvent::BannedPeer { .. } => {}
                SwarmEvent::UnknownPeerUnreachableAddr { .. } => {}
                SwarmEvent::ListenerClosed { .. } => {}
                SwarmEvent::ListenerError { .. } => {}
            }
        }
    }
}
