use super::{behaviour, legacy_message, request_responses, schema, transport};

use alloc::boxed::Box;
use core::{
    future::Future,
    num::{NonZeroU64, NonZeroUsize},
    pin::Pin,
    time::Duration,
};
use libp2p::{
    swarm::{SwarmBuilder, SwarmEvent},
    Multiaddr, PeerId, Swarm,
};
use parity_scale_codec::{DecodeAll, Encode};
use primitive_types::H256;
use prost::Message as _;

pub use request_responses::RequestId;

/// State machine representing the network currently running.
pub struct Network {
    swarm: Swarm<behaviour::Behaviour>,
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

/// Configuration for starting the network.
///
/// Internal to the `network` module.
pub(super) struct Config {
    /// List of peer ids and their addresses that we know are part of the peer-to-peer network.
    pub(super) known_addresses: Vec<(PeerId, Multiaddr)>,
    /// Small string identifying the name of the chain, in order to detect incompatible nodes
    /// earlier.
    pub(super) chain_spec_protocol_id: Vec<u8>,
    /// How to spawn libp2p background tasks. If `None`, then a threads pool will be used by
    /// default.
    pub(super) executor: Option<Box<dyn Fn(Pin<Box<dyn Future<Output = ()> + Send>>) + Send>>,
    /// Hash of the genesis block of the local node.
    pub(super) local_genesis_hash: [u8; 32],
}

impl Network {
    pub(super) async fn start(config: Config) -> Self {
        let local_key_pair = libp2p::identity::Keypair::generate_ed25519();
        let local_public_key = local_key_pair.public();
        let local_peer_id = local_public_key.clone().into_peer_id();
        let (transport, _) = transport::build_transport(local_key_pair, false, None, true);
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
                    name: "/dot/sync/2".into(),
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
            if let Some(spawner) = config.executor {
                struct SpawnImpl<F>(F);
                impl<F: Fn(Pin<Box<dyn Future<Output = ()> + Send>>)> libp2p::core::Executor for SpawnImpl<F> {
                    fn exec(&self, f: Pin<Box<dyn Future<Output = ()> + Send>>) {
                        (self.0)(f)
                    }
                }
                builder = builder.executor(Box::new(SpawnImpl(spawner)));
            }
            builder.build()
        };

        Network { swarm }
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
        let target = match self.swarm.open_peers().next() {
            Some(p) => p.clone(),
            None => return Err(()),
        };

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

        self.swarm
            .send_request(&target, "/dot/sync/2", request_bytes)
            .map_err(|_| ())
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
                    // TODO: don't unwrap
                    let response = match schema::v1::BlockResponse::decode(&response_bytes[..]) {
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
                SwarmEvent::Behaviour(behaviour::BehaviourOut::RequestFinished {
                    request_id,
                    outcome: Err(_err),
                }) => {
                    // TODO: proper error
                    return Event::BlocksRequestFinished {
                        id: request_id,
                        result: Err(()),
                    };
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
