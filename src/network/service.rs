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

use crate::network::{connection, discovery::kademlia, libp2p, multiaddr, peer_id, protocol};

use core::{
    num::NonZeroUsize,
    ops::{Add, Sub},
    task::Context,
    time::Duration,
};

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

    pub in_slots: u32,

    pub out_slots: u32,
}

/// Identifier of a pending connection requested by the network through a [`Event::StartConnect`].
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

    /// For each chain passed in [`Config::chains`], contains the [`ChainConfig::protocol_id`].
    /// Never modified.
    protocol_ids: Vec<String>,
}

impl<TNow, TPeer, TConn> ChainNetwork<TNow, TPeer, TConn>
where
    TNow: Clone + Add<Duration, Output = TNow> + Sub<TNow, Output = Duration> + Ord,
{
    /// Initializes a new [`ChainNetwork`].
    pub fn new(config: Config<TPeer>) -> Self {
        // TODO: figure out the cloning situation here

        let overlay_networks = config
            .chains
            .iter()
            .map(|chain| libp2p::OverlayNetwork {
                main_protocol: libp2p::ProtocolConfig {
                    name: format!("/{}/block-announces/1", chain.protocol_id),
                    fallback_names: Vec::new(),
                },
                optional_protocols: vec![libp2p::ProtocolConfig {
                    name: format!("/{}/transactions/1", chain.protocol_id),
                    fallback_names: Vec::new(),
                }],
                bootstrap_nodes: chain.bootstrap_nodes.clone(),
                in_slots: chain.in_slots,
                out_slots: chain.out_slots,
            })
            .collect();

        ChainNetwork {
            libp2p: libp2p::Network::new(libp2p::Config {
                known_nodes: config.known_nodes,
                listen_addresses: config.listen_addresses,
                noise_key: config.noise_key,
                randomness_seed: config.randomness_seed,
                pending_api_events_buffer_size: config.pending_api_events_buffer_size,
                overlay_networks,
            }),
            protocol_ids: config.chains.into_iter().map(|c| c.protocol_id).collect(),
        }
    }

    /// Returns the number of established TCP connections, both incoming and outgoing.
    pub async fn num_established_connections(&self) -> usize {
        self.libp2p.num_established_connections().await
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

    /// Sends a blocks request to the given peer.
    // TODO: more docs
    // TODO: proper error type
    pub async fn blocks_request(
        &self,
        now: TNow,
        target: peer_id::PeerId,
        chain_index: usize,
        config: protocol::BlocksRequestConfig,
    ) -> Result<Vec<protocol::BlockData>, ()> {
        let request_data = protocol::build_block_request(config).fold(Vec::new(), |mut a, b| {
            a.extend_from_slice(b.as_ref());
            a
        });
        let protocol = format!("/{}/sync/2", &self.protocol_ids[chain_index]);
        let response = self
            .libp2p
            .request(now, target, protocol, request_data)
            .await?;
        protocol::decode_block_response(&response).map_err(|_| ())
    }

    /// Sends a storage request to the given peer.
    // TODO: more docs
    // TODO: proper error type
    pub async fn storage_proof_request(
        &self,
        now: TNow,
        target: peer_id::PeerId,
        chain_index: usize,
        config: protocol::StorageProofRequestConfig<impl Iterator<Item = impl AsRef<[u8]>>>,
    ) -> Result<Vec<Vec<u8>>, ()> {
        let request_data =
            protocol::build_storage_proof_request(config).fold(Vec::new(), |mut a, b| {
                a.extend_from_slice(b.as_ref());
                a
            });
        let protocol = format!("/{}/light/2", &self.protocol_ids[chain_index]);
        let response = self
            .libp2p
            .request(now, target, protocol, request_data)
            .await?;
        protocol::decode_storage_proof_response(&response).map_err(|_| ())
    }

    pub async fn announce_transaction(&self, transaction: Vec<u8>) {}

    /// After a [`Event::StartConnect`], notifies the [`ChainNetwork`] of the success of the
    /// dialing attempt.
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

    /// After a [`Event::StartConnect`], notifies the [`ChainNetwork`] of the failure of the
    /// dialing attempt.
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
    pub async fn next_event(&self) -> Event {
        match self.libp2p.next_event().await {
            libp2p::Event::Connected(peer_id) => Event::Connected(peer_id),
            libp2p::Event::Disconnected(peer_id) => Event::Disconnected(peer_id),
            libp2p::Event::StartConnect { id, multiaddr } => Event::StartConnect {
                id: PendingId(id),
                multiaddr,
            },
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
            peer_id::PeerId::from_public_key(peer_id::PublicKey::Ed25519(pub_key))
        };

        let request_data = kademlia::build_find_node_request(random_peer_id.as_bytes());
        let protocol = format!("/{}/kad", &self.protocol_ids[chain_index]);
        if let Some(target) = self.libp2p.peers_list_lock().await.next() {
            // TODO: better selection
            let response = self
                .libp2p
                .request(now, target, protocol, request_data)
                .await
                .map_err(DiscoveryError::RequestFailed)?;
            let decoded = kademlia::decode_find_node_response(&response)
                .map_err(DiscoveryError::DecodeError)?;
            Ok(DiscoveryInsert {
                service: self,
                outcome: decoded,
                overlay_network_index: chain_index,
            })
        } else {
            Err(DiscoveryError::NoPeer)
        }
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
        cx: &mut Context<'_>,
    ) -> Result<ReadWrite<TNow>, libp2p::ConnectionError> {
        let inner = self
            .libp2p
            .read_write(connection_id.0, now, incoming_buffer, outgoing_buffer, cx)
            .await?;
        Ok(ReadWrite {
            read_bytes: inner.read_bytes,
            written_bytes: inner.written_bytes,
            wake_up_after: inner.wake_up_after,
            write_close: inner.write_close,
        })
    }
}

/// Event generated by [`ChainNetwork::next_event`].
#[derive(Debug)]
pub enum Event {
    Connected(peer_id::PeerId),
    Disconnected(peer_id::PeerId),

    /// User must start connecting to the given multiaddr::Multiaddress.
    ///
    /// Either [`ChainNetwork::pending_outcome_ok`] or [`ChainNetwork::pending_outcome_err`] must
    /// later be called in order to inform of the outcome of the connection.
    StartConnect {
        id: PendingId,
        multiaddr: multiaddr::Multiaddr,
    },
}

/// Successfull outcome to [`ChainNetwork::kademlia_discovery_round`].
#[must_use]
pub struct DiscoveryInsert<'a, TNow, TPeer, TConn> {
    service: &'a ChainNetwork<TNow, TPeer, TConn>,
    outcome: Vec<(peer_id::PeerId, Vec<multiaddr::Multiaddr>)>,
    overlay_network_index: usize,
}

impl<'a, TNow, TPeer, TConn> DiscoveryInsert<'a, TNow, TPeer, TConn>
where
    TNow: Clone + Add<Duration, Output = TNow> + Sub<TNow, Output = Duration> + Ord,
{
    /// Insert the results in the [`ChainNetwork`].
    pub async fn insert(self, mut or_insert: impl FnMut(&peer_id::PeerId) -> TPeer) {
        for (peer_id, addrs) in self.outcome {
            self.service
                .libp2p
                .add_addresses(
                    || or_insert(&peer_id),
                    self.overlay_network_index,
                    peer_id.clone(), // TODO: clone :(
                    addrs,
                )
                .await;
        }
    }
}

/// Error during [`ChainNetwork::kademlia_discovery_round`].
#[derive(Debug, derive_more::Display)]
pub enum DiscoveryError {
    NoPeer,
    #[display(fmt = "Networking request failed")]
    RequestFailed(()),
    DecodeError(kademlia::DecodeFindNodeResponseError),
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

    /// If `true`, the writing side the connection must be closed. Will always remain to `true`
    /// after it has been set.
    ///
    /// If, after calling [`ChainNetwork::read_write`], the returned [`ReadWrite`] contains `true`
    /// here, and the inbound buffer is `None`, then the [`ConnectionId`] is now invalid.
    pub write_close: bool,
}
