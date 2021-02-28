// Smoldot
// Copyright (C) 2019-2021  Parity Technologies (UK) Ltd.
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

use crate::libp2p::{
    self, connection, discovery::kademlia, multiaddr, peer_id, PeerId, QueueNotificationError,
};
use crate::network::protocol;
use crate::util;

use alloc::{
    format,
    string::{String, ToString as _},
    vec::Vec,
};
use core::{
    fmt, iter,
    num::NonZeroUsize,
    ops::{Add, Sub},
    time::Duration,
};
use futures::{channel::mpsc, lock::Mutex, prelude::*};

/// Configuration for a [`ChainNetwork`].
pub struct Config<TPeer> {
    /// Seed for the randomness within the networking state machine.
    ///
    /// While this seed influences the general behaviour of the networking state machine, it
    /// notably isn't used when generating the ephemeral key used for the Diffie-Hellman
    /// handshake.
    /// This is a defensive measure against users passing a dummy seed instead of actual entropy.
    pub randomness_seed: [u8; 32],

    /// Addresses to listen for incoming connections.
    pub listen_addresses: Vec<multiaddr::Multiaddr>,

    /// List of blockchain peer-to-peer networks to be connected to.
    ///
    /// > **Note**: As documented in [the module-level documentation](..), the [`ChainNetwork`]
    /// >           can connect to multiple blockchain networks at the same time.
    ///
    /// The order in which the chains are list is important. The index of each entry needs to be
    /// used later in order to refer to a specific chain.
    pub chains: Vec<ChainConfig>,

    pub known_nodes: Vec<(TPeer, peer_id::PeerId, multiaddr::Multiaddr)>,

    /// Key used for the encryption layer.
    /// This is a Noise static key, according to the Noise specifications.
    /// Signed using the actual libp2p key.
    pub noise_key: connection::NoiseKey,

    /// Number of events that can be buffered internally before connections are back-pressured.
    ///
    /// A good default value is 64.
    ///
    /// # Context
    ///
    /// The [`ChainNetwork`] maintains an internal buffer of the events returned by
    /// [`ChainNetwork::next_event`]. When [`ChainNetwork::read_write`] is called, an event might
    /// get pushed to this buffer. If this buffer is full, back-pressure will be applied to the
    /// connections in order to prevent new events from being pushed.
    ///
    /// This value is important if [`ChainNetwork::next_event`] is called at a slower than the
    /// calls to [`ChainNetwork::read_write`] generate events.
    pub pending_api_events_buffer_size: NonZeroUsize,
}

/// Configuration for a specific overlay network.
///
/// See [`Config::chains`].
pub struct ChainConfig {
    /// Identifier of the protocol, used on the wire to determine which chain messages refer to.
    ///
    /// > **Note**: This value is typically found in the specifications of the chain (the
    /// >           "chain specs").
    pub protocol_id: String,

    /// List of node identities that are known to belong to this overlay network. The node
    /// identities are indices in [`Config::known_nodes`].
    pub bootstrap_nodes: Vec<usize>,

    /// If `Some`, the chain uses the GrandPa networking protocol.
    pub grandpa_protocol_config: Option<GrandpaState>,

    pub in_slots: u32,

    pub out_slots: u32,

    /// Hash of the best block according to the local node.
    pub best_hash: [u8; 32],
    /// Height of the best block according to the local node.
    pub best_number: u64,
    /// Hash of the genesis block (i.e. block number 0) according to the local node.
    pub genesis_hash: [u8; 32],
    pub role: protocol::Role,
}

#[derive(Debug, Copy, Clone)]
// TODO: link to some doc about how GrandPa works: what is a round, what is the set id, etc.
pub struct GrandpaState {
    pub round_number: u64,
    pub set_id: u64,
    pub commit_finalized_height: u32,
}

/// Identifier of a pending connection requested by the network through a [`StartConnect`].
#[derive(Debug, Copy, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct PendingId(libp2p::PendingId);

/// Identifier of a connection spawned by the [`ChainNetwork`].
#[derive(Debug, Copy, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ConnectionId(libp2p::ConnectionId);

/// Data structure containing the list of all connections, pending or not, and their latest known
/// state. See also [the module-level documentation](..).
pub struct ChainNetwork<TNow, TPeer, TConn> {
    /// Underlying data structure that manages the state of the connections and substreams.
    libp2p: libp2p::Network<TNow, TPeer, TConn>,

    /// See [`Config::chains`].
    chain_configs: Vec<ChainConfig>,

    /// For each item in [`ChainNetwork::chain_configs`].
    // TODO: merge with chain_configs?
    chain_grandpa_config: Vec<Option<Mutex<GrandpaState>>>,

    pending_in_accept: Mutex<Option<(libp2p::ConnectionId, usize, Vec<u8>)>>,

    substreams_open_tx: Mutex<mpsc::Sender<()>>,
    substreams_open_rx: Mutex<mpsc::Receiver<()>>,
}

// Update this when a new request response protocol is added.
const REQUEST_RESPONSE_PROTOCOLS_PER_CHAIN: usize = 4;
// Update this when a new notifications protocol is added.
const NOTIFICATIONS_PROTOCOLS_PER_CHAIN: usize = 3;

impl<TNow, TPeer, TConn> ChainNetwork<TNow, TPeer, TConn>
where
    TNow: Clone + Add<Duration, Output = TNow> + Sub<TNow, Output = Duration> + Ord,
{
    /// Initializes a new [`ChainNetwork`].
    pub fn new(config: Config<TPeer>) -> Self {
        // TODO: figure out the cloning situation here

        // The order of protocols here is important, as it defines the values of `protocol_index`
        // to pass to libp2p or that libp2p produces.
        let overlay_networks = config
            .chains
            .iter()
            .flat_map(|chain| {
                iter::once(libp2p::OverlayNetwork {
                    protocol_name: format!("/{}/block-announces/1", chain.protocol_id),
                    fallback_protocol_names: Vec::new(),
                    max_handshake_size: 256,      // TODO: arbitrary
                    max_notification_size: 32768, // TODO: arbitrary
                    bootstrap_nodes: chain.bootstrap_nodes.clone(),
                    in_slots: chain.in_slots,
                    out_slots: chain.out_slots,
                })
                .chain(iter::once(libp2p::OverlayNetwork {
                    protocol_name: format!("/{}/transactions/1", chain.protocol_id),
                    fallback_protocol_names: Vec::new(),
                    max_handshake_size: 256,      // TODO: arbitrary
                    max_notification_size: 32768, // TODO: arbitrary
                    bootstrap_nodes: chain.bootstrap_nodes.clone(),
                    in_slots: chain.in_slots,
                    out_slots: chain.out_slots,
                }))
                .chain({
                    // The `has_grandpa_protocol` flag controls whether the chain uses GrandPa.
                    // Note, however, that GrandPa is technically left enabled (but unused) on all
                    // chains, in order to make the rest of the code of this module more
                    // comprehensible.
                    iter::once(libp2p::OverlayNetwork {
                        protocol_name: "/paritytech/grandpa/1".to_string(),
                        fallback_protocol_names: Vec::new(),
                        max_handshake_size: 256,      // TODO: arbitrary
                        max_notification_size: 32768, // TODO: arbitrary
                        bootstrap_nodes: if chain.grandpa_protocol_config.is_some() {
                            chain.bootstrap_nodes.clone()
                        } else {
                            Vec::new()
                        },
                        in_slots: chain.in_slots,
                        out_slots: chain.out_slots,
                    })
                })
            })
            .collect();

        // The order of protocols here is important, as it defines the values of `protocol_index`
        // to pass to libp2p or that libp2p produces.
        let request_response_protocols = iter::once(libp2p::ConfigRequestResponse {
            name: "/ipfs/id/1.0.0".into(),
            inbound_config: libp2p::ConfigRequestResponseIn::Empty,
            max_response_size: 4096,
            inbound_allowed: true,
            timeout: Duration::from_secs(20),
        })
        .chain(config.chains.iter().flat_map(|chain| {
            // TODO: limits are arbitrary
            iter::once(libp2p::ConfigRequestResponse {
                name: format!("/{}/sync/2", chain.protocol_id),
                inbound_config: libp2p::ConfigRequestResponseIn::Payload { max_size: 1024 },
                max_response_size: 10 * 1024 * 1024,
                // TODO: make this configurable
                inbound_allowed: false,
                timeout: Duration::from_secs(20),
            })
            .chain(iter::once(libp2p::ConfigRequestResponse {
                name: format!("/{}/light/2", chain.protocol_id),
                inbound_config: libp2p::ConfigRequestResponseIn::Payload {
                    max_size: 1024 * 512,
                },
                max_response_size: 10 * 1024 * 1024,
                // TODO: make this configurable
                inbound_allowed: false,
                timeout: Duration::from_secs(20),
            }))
            .chain(iter::once(libp2p::ConfigRequestResponse {
                name: format!("/{}/kad", chain.protocol_id),
                inbound_config: libp2p::ConfigRequestResponseIn::Payload { max_size: 1024 },
                max_response_size: 1024 * 1024,
                // TODO: `false` here means we don't insert ourselves in the DHT, which is the polite thing to do for as long as Kad isn't implemented
                inbound_allowed: false,
                timeout: Duration::from_secs(20),
            }))
            .chain(iter::once(libp2p::ConfigRequestResponse {
                name: format!("/{}/sync/warp", chain.protocol_id),
                inbound_config: libp2p::ConfigRequestResponseIn::Payload { max_size: 32 },
                max_response_size: 16 * 1024 * 1024,
                // We don't support inbound warp sync requests (yet).
                inbound_allowed: false,
                timeout: Duration::from_secs(20),
            }))
        }))
        .collect();

        let (substreams_open_tx, substreams_open_rx) = mpsc::channel(0);

        let chain_grandpa_config = config
            .chains
            .iter()
            .map(|chain| chain.grandpa_protocol_config.clone().map(Mutex::new))
            .collect();

        ChainNetwork {
            libp2p: libp2p::Network::new(libp2p::Config {
                known_nodes: config.known_nodes,
                listen_addresses: config.listen_addresses,
                request_response_protocols,
                noise_key: config.noise_key,
                randomness_seed: config.randomness_seed,
                pending_api_events_buffer_size: config.pending_api_events_buffer_size,
                overlay_networks,
                ping_protocol: "/ipfs/ping/1.0.0".into(),
            }),
            chain_configs: config.chains,
            chain_grandpa_config,
            pending_in_accept: Mutex::new(None),
            substreams_open_tx: Mutex::new(substreams_open_tx),
            substreams_open_rx: Mutex::new(substreams_open_rx),
        }
    }

    fn protocol_index(&self, chain_index: usize, protocol: usize) -> usize {
        1 + chain_index * REQUEST_RESPONSE_PROTOCOLS_PER_CHAIN + protocol
    }

    /// Returns the number of established TCP connections, both incoming and outgoing.
    pub async fn num_established_connections(&self) -> usize {
        self.libp2p.num_established_connections().await
    }

    /// Returns the number of chains. Always equal to the length of [`Config::chains`].
    pub fn num_chains(&self) -> usize {
        self.chain_configs.len()
    }

    pub fn add_incoming_connection(
        &self,
        local_listen_address: &multiaddr::Multiaddr,
        remote_addr: multiaddr::Multiaddr,
        user_data: TConn,
    ) -> ConnectionId {
        ConnectionId(self.libp2p.add_incoming_connection(
            local_listen_address,
            remote_addr,
            user_data,
        ))
    }

    /// Update the state of the local node with regards to GrandPa rounds.
    ///
    /// Calling this method does two things:
    ///
    /// - Send on all the active GrandPa substreams a "neighbor packet" indicating the state of
    ///   the local node.
    /// - Update the neighbor packet that is automatically sent to peers when a GrandPa substream
    ///   gets opened.
    ///
    /// In other words, calling this function atomically informs all the present and future peers
    /// of the state of the local node regarding the GrandPa protocol.
    ///
    /// > **Note**: The information passed as parameter isn't validated in any way by this method.
    ///
    /// # Panic
    ///
    /// Panics if `chain_index` is out of range, or if the chain has GrandPa disabled.
    ///
    pub async fn set_local_grandpa_state(&self, chain_index: usize, grandpa_state: GrandpaState) {
        // TODO: futures cancellation policy?
        // TODO: right now we just try to send to everyone no matter which substream is open, which is wasteful

        let grandpa_config = self.chain_grandpa_config[chain_index].as_ref().unwrap();
        *grandpa_config.lock().await = grandpa_state;

        // Grab the list of peers to send to. This is done *after* updating
        // `chain_grandpa_config`, so that new substreams that are opened after grabbing this
        // list are guaranteed to get informed of the updated state.
        //
        // It is possible that some of the peers in this list have *just* been opened and are
        // already aware of the new state. We ignore this problem and just send another neighbor
        // packet.
        let target_peers = self.libp2p.peers_list_lock().await.collect::<Vec<_>>();

        // Bytes of the neighbor packet to send out.
        let packet = protocol::GrandpaNotificationRef::Neighbor(protocol::NeighborPacket {
            round_number: grandpa_state.round_number,
            set_id: grandpa_state.set_id,
            commit_finalized_height: grandpa_state.commit_finalized_height,
        })
        .scale_encoding()
        .fold(Vec::new(), |mut a, b| {
            a.extend_from_slice(b.as_ref());
            a
        });

        // Now sending out.
        for target in target_peers {
            // Ignore sending errors.
            let _ = self
                .libp2p
                .queue_notification(
                    &target,
                    chain_index * NOTIFICATIONS_PROTOCOLS_PER_CHAIN + 2,
                    packet.clone(),
                )
                .await;
        }
    }

    /// Sends a blocks request to the given peer.
    // TODO: more docs
    pub async fn blocks_request(
        &self,
        now: TNow,
        target: peer_id::PeerId,
        chain_index: usize,
        config: protocol::BlocksRequestConfig,
    ) -> Result<Vec<protocol::BlockData>, BlocksRequestError> {
        let request_data = protocol::build_block_request(config).fold(Vec::new(), |mut a, b| {
            a.extend_from_slice(b.as_ref());
            a
        });
        let response = self
            .libp2p
            .request(
                now,
                target,
                self.protocol_index(chain_index, 0),
                request_data,
            )
            .map_err(BlocksRequestError::Request)
            .await?;
        protocol::decode_block_response(&response).map_err(BlocksRequestError::Decode)
    }

    pub async fn grandpa_warp_sync_request(
        &self,
        now: TNow,
        target: peer_id::PeerId,
        chain_index: usize,
        begin_hash: [u8; 32],
    ) -> Result<protocol::GrandpaWarpSyncResponse, GrandpaWarpSyncRequestError> {
        let request_data = begin_hash.to_vec();

        let response = self
            .libp2p
            .request(
                now,
                target,
                self.protocol_index(chain_index, 3),
                request_data,
            )
            .map_err(GrandpaWarpSyncRequestError::Request)
            .await?;

        protocol::decode_grandpa_warp_sync_response(&response)
            .map_err(GrandpaWarpSyncRequestError::Decode)
    }

    /// Sends a storage request to the given peer.
    // TODO: more docs
    pub async fn storage_proof_request(
        &self,
        now: TNow,
        target: peer_id::PeerId,
        chain_index: usize,
        config: protocol::StorageProofRequestConfig<impl Iterator<Item = impl AsRef<[u8]>>>,
    ) -> Result<Vec<Vec<u8>>, StorageProofRequestError> {
        let request_data =
            protocol::build_storage_proof_request(config).fold(Vec::new(), |mut a, b| {
                a.extend_from_slice(b.as_ref());
                a
            });
        let response = self
            .libp2p
            .request(
                now,
                target,
                self.protocol_index(chain_index, 1),
                request_data,
            )
            .map_err(StorageProofRequestError::Request)
            .await?;
        protocol::decode_storage_proof_response(&response).map_err(StorageProofRequestError::Decode)
    }

    /// Sends a call proof request to the given peer.
    ///
    /// This request is similar to [`ChainNetwork::storage_proof_request`]. Instead of requesting
    /// specific keys, we request the list of all the keys that are accessed for a specific
    /// runtime call.
    ///
    /// There exists no guarantee that the proof is complete (i.e. that it contains all the
    /// necessary entries), as it is impossible to know this from just the proof itself. As such,
    /// this method is just an optimization. When performing the actual call, regular storage proof
    /// requests should be performed if the key is not present in the call proof response.
    pub async fn call_proof_request<'a>(
        &self,
        now: TNow,
        target: peer_id::PeerId,
        chain_index: usize,
        config: protocol::CallProofRequestConfig<'a, impl Iterator<Item = impl AsRef<[u8]>>>,
    ) -> Result<Vec<Vec<u8>>, CallProofRequestError> {
        let request_data =
            protocol::build_call_proof_request(config).fold(Vec::new(), |mut a, b| {
                a.extend_from_slice(b.as_ref());
                a
            });
        let response = self
            .libp2p
            .request(
                now,
                target,
                self.protocol_index(chain_index, 1),
                request_data,
            )
            .map_err(CallProofRequestError::Request)
            .await?;
        protocol::decode_call_proof_response(&response).map_err(CallProofRequestError::Decode)
    }

    pub async fn announce_transaction(
        &self,
        target: &peer_id::PeerId,
        chain_index: usize,
        extrinsic: &[u8],
    ) -> Result<(), QueueNotificationError> {
        let mut val = Vec::with_capacity(1 + extrinsic.len());
        val.extend_from_slice(util::encode_scale_compact_usize(1).as_ref());
        val.extend_from_slice(extrinsic);
        self.libp2p
            .queue_notification(
                &target,
                chain_index * NOTIFICATIONS_PROTOCOLS_PER_CHAIN + 1,
                val,
            )
            .await
    }

    /// After calling [`ChainNetwork::fill_out_slots`], notifies the [`ChainNetwork`] of the
    /// success of the dialing attempt.
    ///
    /// See also [`ChainNetwork::pending_outcome_err`].
    ///
    /// # Panic
    ///
    /// Panics if the [`PendingId`] is invalid.
    ///
    pub async fn pending_outcome_ok(&self, id: PendingId, user_data: TConn) -> ConnectionId {
        ConnectionId(self.libp2p.pending_outcome_ok(id.0, user_data).await)
    }

    /// After calling [`ChainNetwork::fill_out_slots`], notifies the [`ChainNetwork`] of the
    /// failure of the dialing attempt.
    ///
    /// See also [`ChainNetwork::pending_outcome_ok`].
    ///
    /// # Panic
    ///
    /// Panics if the [`PendingId`] is invalid.
    ///
    pub async fn pending_outcome_err(&self, id: PendingId) {
        self.libp2p.pending_outcome_err(id.0).await
    }

    /// Returns the next event produced by the service.
    ///
    /// This function should be called at a high enough rate that [`ChainNetwork::read_write`] can
    /// continue pushing events to the internal buffer of events. Failure to call this function
    /// often enough will lead to connections being back-pressured.
    /// See also [`Config::pending_api_events_buffer_size`].
    ///
    /// It is technically possible to call this function multiple times simultaneously, in which
    /// case the events will be distributed amongst the multiple calls in an unspecified way.
    /// Keep in mind that some [`Event`]s have logic attached to the order in which they are
    /// produced, and calling this function multiple times is therefore discouraged.
    pub async fn next_event<'a>(&'a self) -> Event<'a, TNow, TPeer, TConn> {
        let mut pending_in_accept = self.pending_in_accept.lock().await;

        loop {
            if let Some((id, overlay_network_index, handshake)) = &*pending_in_accept {
                self.libp2p
                    .accept_notifications_in(*id, *overlay_network_index, handshake.clone()) // TODO: clone :-/
                    .await;
                *pending_in_accept = None;
            }

            match self.libp2p.next_event().await {
                libp2p::Event::Connected(peer_id) => {
                    let _ = self.substreams_open_tx.lock().await.try_send(());
                    return Event::Connected(peer_id);
                }
                libp2p::Event::Disconnected {
                    peer_id,
                    user_data,
                    mut out_overlay_network_indices,
                    ..
                } => {
                    out_overlay_network_indices
                        .retain(|i| (i % NOTIFICATIONS_PROTOCOLS_PER_CHAIN) == 0);
                    for elem in &mut out_overlay_network_indices {
                        *elem /= NOTIFICATIONS_PROTOCOLS_PER_CHAIN;
                    }
                    return Event::Disconnected {
                        peer_id,
                        chain_indices: out_overlay_network_indices,
                    };
                }
                libp2p::Event::RequestIn {
                    id,
                    substream_id,
                    protocol_index,
                    peer_id,
                    ..
                } => {
                    // Only protocol 0 (identify) can receive requests at the moment.
                    debug_assert_eq!(protocol_index, 0);

                    return Event::IdentifyRequestIn {
                        peer_id,
                        request: IdentifyRequestIn {
                            service: self,
                            id,
                            substream_id,
                        },
                    };
                }
                libp2p::Event::NotificationsOutAccept {
                    id,
                    peer_id,
                    overlay_network_index,
                    remote_handshake,
                } => {
                    let chain_index = overlay_network_index / NOTIFICATIONS_PROTOCOLS_PER_CHAIN;
                    if overlay_network_index % NOTIFICATIONS_PROTOCOLS_PER_CHAIN == 0 {
                        let remote_handshake =
                            protocol::decode_block_announces_handshake(&remote_handshake).unwrap();
                        // TODO: don't unwrap
                        // TODO: compare genesis hash with ours
                        return Event::ChainConnected {
                            peer_id,
                            chain_index,
                            best_hash: *remote_handshake.best_hash,
                            best_number: remote_handshake.best_number,
                            role: remote_handshake.role,
                        };
                    } else if overlay_network_index % NOTIFICATIONS_PROTOCOLS_PER_CHAIN == 1 {
                        // Nothing to do.
                    } else if overlay_network_index % NOTIFICATIONS_PROTOCOLS_PER_CHAIN == 2 {
                        // Grandpa notification has been opened. Send neighbor packet.
                        // TODO: below is not futures-cancellation-safe!
                        let grandpa_config = self.chain_grandpa_config[chain_index]
                            .as_ref()
                            .unwrap()
                            .lock()
                            .await
                            .clone();
                        let packet =
                            protocol::GrandpaNotificationRef::Neighbor(protocol::NeighborPacket {
                                round_number: grandpa_config.round_number,
                                set_id: grandpa_config.set_id,
                                commit_finalized_height: grandpa_config.commit_finalized_height,
                            })
                            .scale_encoding()
                            .fold(Vec::new(), |mut a, b| {
                                a.extend_from_slice(b.as_ref());
                                a
                            });
                        let _ = self
                            .libp2p
                            .queue_notification(&peer_id, overlay_network_index, packet)
                            .await;
                    } else {
                        unreachable!()
                    }

                    // TODO:
                }
                libp2p::Event::NotificationsOutReject { .. } => {
                    // TODO:
                }
                libp2p::Event::NotificationsOutClose {
                    id,
                    peer_id,
                    overlay_network_index,
                } => {
                    let chain_index = overlay_network_index / NOTIFICATIONS_PROTOCOLS_PER_CHAIN;
                    if overlay_network_index % NOTIFICATIONS_PROTOCOLS_PER_CHAIN == 0 {
                        return Event::ChainDisconnected {
                            peer_id,
                            chain_index,
                        };
                    // TODO: don't unwrap
                    } else {
                    }

                    // TODO:
                }
                libp2p::Event::NotificationsInOpen {
                    id,
                    overlay_network_index,
                    remote_handshake,
                } => {
                    if (overlay_network_index % NOTIFICATIONS_PROTOCOLS_PER_CHAIN) == 0 {
                        let remote_handshake =
                            protocol::decode_block_announces_handshake(&remote_handshake).unwrap();
                        // TODO: don't unwrap

                        let chain_config = &self.chain_configs
                            [overlay_network_index / NOTIFICATIONS_PROTOCOLS_PER_CHAIN];

                        let handshake = protocol::encode_block_announces_handshake(
                            protocol::BlockAnnouncesHandshakeRef {
                                best_hash: &chain_config.best_hash,
                                best_number: chain_config.best_number,
                                genesis_hash: &chain_config.genesis_hash,
                                role: chain_config.role,
                            },
                        )
                        .fold(Vec::new(), |mut a, b| {
                            a.extend_from_slice(b.as_ref());
                            a
                        });

                        // Accepting the substream isn't done immediately because of
                        // futures-cancellation-related concerns.
                        *pending_in_accept = Some((id, overlay_network_index, handshake));
                    } else if (overlay_network_index % NOTIFICATIONS_PROTOCOLS_PER_CHAIN) == 1 {
                        // Accepting the substream isn't done immediately because of
                        // futures-cancellation-related concerns.
                        *pending_in_accept = Some((id, overlay_network_index, Vec::new()));
                    } else if (overlay_network_index % NOTIFICATIONS_PROTOCOLS_PER_CHAIN) == 2 {
                        // Grandpa substream.
                        let chain_config = &self.chain_configs
                            [overlay_network_index / NOTIFICATIONS_PROTOCOLS_PER_CHAIN];

                        let handshake = chain_config.role.scale_encoding().to_vec();

                        // Accepting the substream isn't done immediately because of
                        // futures-cancellation-related concerns.
                        *pending_in_accept = Some((id, overlay_network_index, handshake));
                    } else {
                        unreachable!()
                    }
                }
                libp2p::Event::NotificationsIn {
                    id,
                    peer_id,
                    has_symmetric_substream,
                    overlay_network_index,
                    notification,
                } => {
                    // Don't report events about nodes we don't have an outbound substream with.
                    // TODO: think about possible race conditions regarding missing block
                    // announcements, as the remote will think we know it's at a certain block
                    // while we ignored its announcement ; it isn't problematic as long as blocks
                    // are generated continuously, as announcements will be generated periodically
                    // as well and the state will no longer mismatch
                    if !has_symmetric_substream {
                        continue;
                    }

                    let chain_index = overlay_network_index / NOTIFICATIONS_PROTOCOLS_PER_CHAIN;
                    if overlay_network_index % NOTIFICATIONS_PROTOCOLS_PER_CHAIN == 0 {
                        // TODO: don't unwrap
                        let announce = protocol::decode_block_announce(&notification).unwrap();
                        return Event::BlockAnnounce {
                            chain_index,
                            peer_id,
                            announce: EncodedBlockAnnounce(notification),
                        };
                    } else if overlay_network_index % NOTIFICATIONS_PROTOCOLS_PER_CHAIN == 1 {
                        // TODO: transaction announce
                    } else if overlay_network_index % NOTIFICATIONS_PROTOCOLS_PER_CHAIN == 2 {
                        // TODO: don't unwrap
                        let _notif = protocol::decode_grandpa_notification(&notification).unwrap();
                    // TODO: do something with these notifs
                    } else {
                        unreachable!()
                    }
                }
            }
        }
    }

    /// Performs a round of Kademlia discovery.
    ///
    /// This future yields once a list of nodes on the network has been discovered, or a problem
    /// happened.
    pub async fn kademlia_discovery_round(
        &'_ self,
        now: TNow,
        chain_index: usize,
    ) -> Result<DiscoveryInsert<'_, TNow, TPeer, TConn>, DiscoveryError> {
        let random_peer_id = {
            // FIXME: don't use rand::random()! use randomness seed
            let pub_key = rand::random::<[u8; 32]>();
            peer_id::PeerId::from_public_key(&peer_id::PublicKey::Ed25519(pub_key))
        };

        let request_data = kademlia::build_find_node_request(random_peer_id.as_bytes());
        if let Some(target) = self.libp2p.peers_list_lock().await.next() {
            // TODO: better peer selection
            let response = self
                .libp2p
                .request(
                    now,
                    target,
                    self.protocol_index(chain_index, 2),
                    request_data,
                )
                .await
                .map_err(DiscoveryError::RequestFailed)?;
            let decoded = kademlia::decode_find_node_response(&response)
                .map_err(DiscoveryError::DecodeError)?;
            Ok(DiscoveryInsert {
                service: self,
                outcome: decoded,
                chain_index,
            })
        } else {
            Err(DiscoveryError::NoPeer)
        }
    }

    /// Waits until a connection is in a state in which a substream can be opened.
    pub async fn next_substream<'a>(&'a self) -> SubstreamOpen<'a, TNow, TPeer, TConn> {
        loop {
            // TODO: limit number of slots

            match self.libp2p.open_next_substream().await {
                Some(inner) => {
                    return SubstreamOpen {
                        inner,
                        chains: &self.chain_configs,
                    }
                }
                None => {
                    self.substreams_open_rx.lock().await.next().await;
                }
            };
        }
    }

    /// Spawns new outgoing connections in order to fill empty outgoing slots.
    // TODO: give more control, with number of slots and node choice
    pub async fn fill_out_slots<'a>(&self, chain_index: usize) -> Option<StartConnect> {
        let inner = self
            .libp2p
            .fill_out_slots(chain_index * NOTIFICATIONS_PROTOCOLS_PER_CHAIN)
            .await?;

        Some(StartConnect {
            id: PendingId(inner.id),
            multiaddr: inner.multiaddr,
            expected_peer_id: inner.expected_peer_id,
        })
    }

    ///
    /// # Panic
    ///
    /// Panics if `connection_id` isn't a valid connection.
    ///
    pub async fn read_write<'a>(
        &self,
        connection_id: ConnectionId,
        now: TNow,
        incoming_buffer: Option<&[u8]>,
        outgoing_buffer: (&'a mut [u8], &'a mut [u8]),
    ) -> Result<ReadWrite<TNow>, libp2p::ConnectionError> {
        let inner = self
            .libp2p
            .read_write(connection_id.0, now, incoming_buffer, outgoing_buffer)
            .await?;
        Ok(ReadWrite {
            read_bytes: inner.read_bytes,
            written_bytes: inner.written_bytes,
            wake_up_after: inner.wake_up_after,
            wake_up_future: inner.wake_up_future,
            write_close: inner.write_close,
        })
    }

    /// Returns an iterator to the list of [`PeerId`]s that we have an established connection
    /// with.
    pub async fn peers_list(&self) -> impl Iterator<Item = PeerId> {
        self.libp2p.peers_list_lock().await
    }
}

/// User must start connecting to the given multiaddress.
///
/// Either [`ChainNetwork::pending_outcome_ok`] or [`ChainNetwork::pending_outcome_err`] must
/// later be called in order to inform of the outcome of the connection.
#[derive(Debug)]
#[must_use]
pub struct StartConnect {
    pub id: PendingId,
    pub multiaddr: multiaddr::Multiaddr,
    /// [`PeerId`] that is expected to be reached with this connection attempt.
    pub expected_peer_id: PeerId,
}

/// Event generated by [`ChainNetwork::next_event`].
#[derive(Debug)]
pub enum Event<'a, TNow, TPeer, TConn> {
    /// Established a transport-level connection (e.g. a TCP socket) with the given peer.
    Connected(peer_id::PeerId),

    /// A transport-level connection (e.g. a TCP socket) has been closed.
    Disconnected {
        peer_id: peer_id::PeerId,
        chain_indices: Vec<usize>,
    },

    ChainConnected {
        chain_index: usize,
        peer_id: peer_id::PeerId,
        /// Role the node reports playing on the network.
        role: protocol::Role,
        /// Height of the best block according to this node.
        best_number: u64,
        /// Hash of the best block according to this node.
        best_hash: [u8; 32],
    },
    ChainDisconnected {
        chain_index: usize,
        peer_id: peer_id::PeerId,
    },

    BlockAnnounce {
        chain_index: usize,
        peer_id: peer_id::PeerId,
        announce: EncodedBlockAnnounce,
    },

    /// A remote has sent a request for identification information.
    ///
    /// You are strongly encouraged to call [`IdentifyRequestIn::respond`].
    IdentifyRequestIn {
        /// Remote that has sent the request.
        peer_id: PeerId,
        /// Object allowing sending back the answer.
        request: IdentifyRequestIn<'a, TNow, TPeer, TConn>,
    },
    /*Transactions {
        peer_id: peer_id::PeerId,
        transactions: EncodedTransactions,
    }*/
}

/// Undecoded but valid block announce handshake.
pub struct EncodedBlockAnnounceHandshake(Vec<u8>);

impl EncodedBlockAnnounceHandshake {
    /// Returns the decoded version of the handshake.
    pub fn decode(&self) -> protocol::BlockAnnouncesHandshakeRef {
        protocol::decode_block_announces_handshake(&self.0).unwrap()
    }
}

impl fmt::Debug for EncodedBlockAnnounceHandshake {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        fmt::Debug::fmt(&self.decode(), f)
    }
}

/// Undecoded but valid block announce.
#[derive(Clone)]
pub struct EncodedBlockAnnounce(Vec<u8>);

impl EncodedBlockAnnounce {
    /// Returns the decoded version of the announcement.
    pub fn decode(&self) -> protocol::BlockAnnounceRef {
        protocol::decode_block_announce(&self.0).unwrap()
    }
}

impl fmt::Debug for EncodedBlockAnnounce {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        fmt::Debug::fmt(&self.decode(), f)
    }
}

/// Successfull outcome to [`ChainNetwork::kademlia_discovery_round`].
#[must_use]
pub struct DiscoveryInsert<'a, TNow, TPeer, TConn> {
    service: &'a ChainNetwork<TNow, TPeer, TConn>,
    outcome: Vec<(peer_id::PeerId, Vec<multiaddr::Multiaddr>)>,

    /// Index within [`Config::chains`] corresponding to the chain the nodes belong to.
    chain_index: usize,
}

impl<'a, TNow, TPeer, TConn> DiscoveryInsert<'a, TNow, TPeer, TConn>
where
    TNow: Clone + Add<Duration, Output = TNow> + Sub<TNow, Output = Duration> + Ord,
{
    /// Insert the results in the [`ChainNetwork`].
    // TODO: futures cancellation concerns T_T
    pub async fn insert(self, mut or_insert: impl FnMut(&peer_id::PeerId) -> TPeer) {
        for (peer_id, addrs) in self.outcome {
            self.service
                .libp2p
                .add_addresses(
                    || or_insert(&peer_id),
                    self.chain_index * NOTIFICATIONS_PROTOCOLS_PER_CHAIN,
                    peer_id.clone(), // TODO: clone :(
                    addrs.iter().cloned(),
                )
                .await;
            self.service
                .libp2p
                .add_addresses(
                    || or_insert(&peer_id),
                    self.chain_index * NOTIFICATIONS_PROTOCOLS_PER_CHAIN + 1,
                    peer_id.clone(), // TODO: clone :(
                    addrs.iter().cloned(),
                )
                .await;

            if self.service.chain_configs[self.chain_index]
                .grandpa_protocol_config
                .is_some()
            {
                self.service
                    .libp2p
                    .add_addresses(
                        || or_insert(&peer_id),
                        self.chain_index * NOTIFICATIONS_PROTOCOLS_PER_CHAIN + 2,
                        peer_id.clone(), // TODO: clone :(
                        addrs,
                    )
                    .await;
            }
        }
    }
}

/// Outcome of calling [`ChainNetwork::read_write`].
pub struct ReadWrite<TNow> {
    /// Number of bytes at the start of the incoming buffer that have been processed. These bytes
    /// should no longer be present the next time [`ChainNetwork::read_write`] is called.
    pub read_bytes: usize,

    /// Number of bytes written to the outgoing buffer. These bytes should be sent out to the
    /// remote. The rest of the outgoing buffer is left untouched.
    pub written_bytes: usize,

    /// If `Some`, [`ChainNetwork::read_write`] should be called again when the point in time
    /// reaches the value in the `Option`.
    pub wake_up_after: Option<TNow>,

    /// [`ChainNetwork::read_write`] should be called again when this
    /// [`libp2p::ConnectionReadyFuture`] returns `Ready`.
    pub wake_up_future: libp2p::ConnectionReadyFuture,

    /// If `true`, the writing side the connection must be closed. Will always remain to `true`
    /// after it has been set.
    ///
    /// If, after calling [`ChainNetwork::read_write`], the returned [`ReadWrite`] contains `true`
    /// here, and the inbound buffer is `None`, then the [`ConnectionId`] is now invalid.
    pub write_close: bool,
}

pub struct SubstreamOpen<'a, TNow, TPeer, TConn> {
    inner: libp2p::SubstreamOpen<'a, TNow, TPeer, TConn>,
    chains: &'a Vec<ChainConfig>,
}

impl<'a, TNow, TPeer, TConn> SubstreamOpen<'a, TNow, TPeer, TConn>
where
    TNow: Clone + Add<Duration, Output = TNow> + Sub<TNow, Output = Duration> + Ord,
{
    pub async fn open(self, now: TNow) {
        let chain_config =
            &self.chains[self.inner.overlay_network_index() / NOTIFICATIONS_PROTOCOLS_PER_CHAIN];

        let handshake =
            if self.inner.overlay_network_index() % NOTIFICATIONS_PROTOCOLS_PER_CHAIN == 0 {
                protocol::encode_block_announces_handshake(protocol::BlockAnnouncesHandshakeRef {
                    best_hash: &chain_config.best_hash,
                    best_number: chain_config.best_number,
                    genesis_hash: &chain_config.genesis_hash,
                    role: chain_config.role,
                })
                .fold(Vec::new(), |mut a, b| {
                    a.extend_from_slice(b.as_ref());
                    a
                })
            } else if self.inner.overlay_network_index() % NOTIFICATIONS_PROTOCOLS_PER_CHAIN == 1 {
                Vec::new()
            } else if self.inner.overlay_network_index() % NOTIFICATIONS_PROTOCOLS_PER_CHAIN == 2 {
                chain_config.role.scale_encoding().to_vec()
            } else {
                unreachable!()
            };

        self.inner.open(now, handshake).await;
    }
}

/// See [`Event::IdentifyRequestIn`].
#[must_use]
pub struct IdentifyRequestIn<'a, TNow, TPeer, TConn> {
    service: &'a ChainNetwork<TNow, TPeer, TConn>,
    id: libp2p::ConnectionId,
    substream_id: libp2p::connection::established::SubstreamId,
}

impl<'a, TNow, TPeer, TConn> IdentifyRequestIn<'a, TNow, TPeer, TConn>
where
    TNow: Clone + Add<Duration, Output = TNow> + Sub<TNow, Output = Duration> + Ord,
{
    /// Queue the response to send back. The future provided by [`ChainNetwork::read_write`] will
    /// automatically be woken up.
    pub async fn respond(self, agent_version: &str) {
        let response = protocol::build_identify_response(protocol::IdentifyResponse {
            protocol_version: "/substrate/1.0", // TODO: same value as in Substrate
            agent_version,
            ed25519_public_key: self.service.libp2p.noise_key().libp2p_public_ed25519_key(),
            listen_addrs: iter::empty(),                // TODO:
            observed_addr: &libp2p::Multiaddr::empty(), // TODO:
            protocols: self
                .service
                .libp2p
                .request_response_protocols()
                .filter(|p| p.inbound_allowed)
                .map(|p| &p.name[..])
                .chain(
                    self.service
                        .libp2p
                        .overlay_networks()
                        .map(|p| &p.protocol_name[..]),
                ),
        })
        .fold(Vec::new(), |mut a, b| {
            a.extend_from_slice(b.as_ref());
            a
        });

        self.service
            .libp2p
            .respond_in_request(self.id, self.substream_id, Ok(response))
            .await;
    }
}

impl<'a, TNow, TPeer, TConn> fmt::Debug for IdentifyRequestIn<'a, TNow, TPeer, TConn> {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.debug_struct("IdentifyRequestIn").finish()
    }
}

/// Error during [`ChainNetwork::kademlia_discovery_round`].
#[derive(Debug, derive_more::Display)]
pub enum DiscoveryError {
    NoPeer,
    RequestFailed(libp2p::RequestError),
    DecodeError(kademlia::DecodeFindNodeResponseError),
}

/// Error returned by [`ChainNetwork::blocks_request`].
#[derive(Debug, derive_more::Display)]
pub enum BlocksRequestError {
    Request(libp2p::RequestError),
    Decode(protocol::DecodeBlockResponseError),
}

/// Error returned by [`ChainNetwork::storage_proof_request`].
#[derive(Debug, derive_more::Display)]
pub enum StorageProofRequestError {
    Request(libp2p::RequestError),
    Decode(protocol::DecodeStorageProofResponseError),
}

/// Error returned by [`ChainNetwork::call_proof_request`].
#[derive(Debug, derive_more::Display)]
pub enum CallProofRequestError {
    Request(libp2p::RequestError),
    Decode(protocol::DecodeCallProofResponseError),
}

/// Error returned by [`ChainNetwork::grandpa_warp_sync_request`].
#[derive(Debug, derive_more::Display)]
pub enum GrandpaWarpSyncRequestError {
    Request(libp2p::RequestError),
    Decode(protocol::DecodeGrandpaWarpSyncResponseError),
}
