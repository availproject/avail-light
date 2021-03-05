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

//! Background network service.
//!
//! The [`NetworkService`] manages background tasks dedicated to connecting to other nodes.
//! Importantly, its design is oriented towards the particular use case of the light client.
//!
//! The [`NetworkService`] spawns one background task (using the [`Config::tasks_executor`]) for
//! each active connection.
//!
//! The objective of the [`NetworkService`] in general is to try stay connected as much as
//! possible to the nodes of the peer-to-peer network of the chain, and maintain open substreams
//! with them in order to send out requests (e.g. block requests) and notifications (e.g. block
//! announces).
//!
//! Connectivity to the network is performed in the background as an implementation detail of
//! the service. The public API only allows emitting requests and notifications towards the
//! already-connected nodes.
//!
//! An important part of the API is the list of channel receivers of [`Event`] returned by
//! [`NetworkService::new`]. These channels inform the foreground about updates to the network
//! connectivity.

use crate::ffi;

use core::{
    fmt,
    num::{NonZeroU32, NonZeroUsize},
    pin::Pin,
    time::Duration,
};
use futures::{channel::mpsc, lock::Mutex, prelude::*};
use smoldot::{
    header,
    informant::HashDisplay,
    libp2p::{
        connection,
        multiaddr::{Multiaddr, Protocol},
        peer_id::PeerId,
    },
    network::{protocol, service},
    trie::{self, prefix_proof, proof_verify},
};
use std::{collections::HashSet, sync::Arc};

/// Configuration for a [`NetworkService`].
pub struct Config {
    /// Closure that spawns background tasks.
    pub tasks_executor: Box<dyn FnMut(Pin<Box<dyn Future<Output = ()> + Send>>) + Send>,

    /// Number of event receivers returned by [`NetworkService::new`].
    pub num_events_receivers: usize,

    /// List of chains to connect to. Chains are later referred to by their index in this list.
    pub chains: Vec<ConfigChain>,
}

/// See [`Config::chains`].
pub struct ConfigChain {
    /// List of node identities and addresses that are known to belong to the chain's peer-to-pee
    /// network.
    pub bootstrap_nodes: Vec<(PeerId, Multiaddr)>,

    /// Hash of the genesis block of the chain. Sent to other nodes in order to determine whether
    /// the chains match.
    pub genesis_block_hash: [u8; 32],

    /// Number and hash of the current best block. Can later be updated with // TODO: which function?
    pub best_block: (u64, [u8; 32]),

    /// Identifier of the chain to connect to.
    ///
    /// Each blockchain has (or should have) a different "protocol id". This value identifies the
    /// chain, so as to not introduce conflicts in the networking messages.
    pub protocol_id: String,

    /// If true, the chain uses the GrandPa networking protocol.
    pub has_grandpa_protocol: bool,
}

pub struct NetworkService {
    /// Fields behind a mutex.
    guarded: Mutex<Guarded>,

    /// Data structure holding the entire state of the networking.
    network: service::ChainNetwork<ffi::Instant, (), ()>,

    /// List of nodes that are considered as important for logging purposes.
    // TODO: should also detect whenever we fail to open a block announces substream with any of these peers
    important_nodes: HashSet<PeerId, fnv::FnvBuildHasher>,
}

/// Fields of [`NetworkService`] behind a mutex.
struct Guarded {
    /// See [`Config::tasks_executor`].
    tasks_executor: Box<dyn FnMut(Pin<Box<dyn Future<Output = ()> + Send>>) + Send>,
}

impl NetworkService {
    /// Initializes the network service with the given configuration.
    ///
    /// Returns the networking service, plus a list of receivers on which events are pushed.
    /// All of these receivers must be polled regularly to prevent the networking service from
    /// slowing down.
    pub async fn new(config: Config) -> (Arc<Self>, Vec<mpsc::Receiver<Event>>) {
        let (mut senders, receivers): (Vec<_>, Vec<_>) = (0..config.num_events_receivers)
            .map(|_| mpsc::channel(16))
            .unzip();

        let important_nodes = config
            .chains
            .iter()
            .flat_map(|chain| chain.bootstrap_nodes.iter())
            .map(|(peer_id, _)| peer_id.clone())
            .collect::<HashSet<_, _>>();

        let num_chains = config.chains.len();
        let mut chains = Vec::with_capacity(num_chains);
        // TODO: this `bootstrap_nodes` field is weird ; should we de-duplicate entry in known_nodes?
        let mut known_nodes = Vec::new();

        for chain in config.chains {
            chains.push(service::ChainConfig {
                bootstrap_nodes: (known_nodes.len()
                    ..(known_nodes.len() + chain.bootstrap_nodes.len()))
                    .collect(),
                in_slots: 25,
                out_slots: 25,
                grandpa_protocol_config: if chain.has_grandpa_protocol {
                    // TODO: dummy values
                    Some(service::GrandpaState {
                        commit_finalized_height: 0,
                        round_number: 1,
                        set_id: 0,
                    })
                } else {
                    None
                },
                protocol_id: chain.protocol_id.clone(),
                best_hash: chain.best_block.1,
                best_number: chain.best_block.0,
                genesis_hash: chain.genesis_block_hash,
                role: protocol::Role::Light,
            });

            known_nodes.extend(
                chain
                    .bootstrap_nodes
                    .into_iter()
                    .map(|(peer_id, addr)| ((), peer_id, addr)),
            );
        }

        let network_service = Arc::new(NetworkService {
            guarded: Mutex::new(Guarded {
                tasks_executor: config.tasks_executor,
            }),
            network: service::ChainNetwork::new(service::Config {
                chains,
                known_nodes,
                listen_addresses: Vec::new(), // TODO:
                noise_key: connection::NoiseKey::new(&rand::random()),
                // TODO: we use an abnormally large channel in order to by pass https://github.com/paritytech/smoldot/issues/615
                // once the issue is solved, this should be restored to a smaller value, such as 16
                pending_api_events_buffer_size: NonZeroUsize::new(2048).unwrap(),
                randomness_seed: rand::random(),
            }),
            important_nodes,
        });

        // Spawn a task pulling events from the network and transmitting them to the event senders.
        (network_service.guarded.try_lock().unwrap().tasks_executor)(Box::pin({
            // TODO: keeping a Weak here doesn't really work to shut down tasks
            let network_service = Arc::downgrade(&network_service);
            async move {
                loop {
                    let event = loop {
                        let network_service = match network_service.upgrade() {
                            Some(ns) => ns,
                            None => {
                                return;
                            }
                        };

                        match network_service.network.next_event().await {
                            service::Event::Connected(peer_id) => {
                                log::info!(target: "network", "Connected to {}", peer_id);
                            }
                            service::Event::Disconnected {
                                peer_id,
                                chain_indices,
                            } => {
                                log::info!(target: "network", "Disconnected from {} (chains: {:?})", peer_id, chain_indices);
                                if !chain_indices.is_empty() {
                                    // TODO: properly implement when multiple chains
                                    if chain_indices.len() == 1 {
                                        break Event::Disconnected {
                                            peer_id,
                                            chain_index: chain_indices[0],
                                        };
                                    } else {
                                        todo!()
                                    }
                                }
                            }
                            service::Event::BlockAnnounce {
                                chain_index,
                                peer_id,
                                announce,
                            } => {
                                log::debug!(
                                    target: "network",
                                    "Connection({}) => BlockAnnounce({}, {}, is_best={})",
                                    peer_id,
                                    chain_index,
                                    HashDisplay(&announce.decode().header.hash()),
                                    announce.decode().is_best
                                );
                                break Event::BlockAnnounce {
                                    chain_index,
                                    peer_id,
                                    announce,
                                };
                            }
                            service::Event::ChainConnected {
                                peer_id,
                                chain_index,
                                best_number,
                                best_hash,
                                ..
                            } => {
                                log::debug!(
                                    target: "network",
                                    "Connection({}) => ChainConnected({}, {}, {})",
                                    peer_id,
                                    chain_index,
                                    best_number,
                                    HashDisplay(&best_hash)
                                );
                                break Event::Connected {
                                    peer_id,
                                    chain_index,
                                    best_block_number: best_number,
                                    best_block_hash: best_hash,
                                };
                            }
                            service::Event::ChainDisconnected {
                                peer_id,
                                chain_index,
                            } => {
                                log::debug!(
                                    target: "network",
                                    "Connection({}) => ChainDisconnected({})",
                                    peer_id,
                                    chain_index,
                                );
                                break Event::Disconnected {
                                    peer_id,
                                    chain_index,
                                };
                            }
                            service::Event::IdentifyRequestIn { peer_id, request } => {
                                log::debug!(
                                    target: "network",
                                    "Connection({}) => IdentifyRequest",
                                    peer_id,
                                );
                                request.respond("smoldot").await;
                            }
                        }
                    };

                    // Dispatch the event to the various senders.
                    // This little `if` avoids having to do `event.clone()` if we don't have to.
                    if senders.len() == 1 {
                        let _ = senders[0].send(event).await;
                    } else {
                        for sender in &mut senders {
                            let _ = sender.send(event.clone()).await;
                        }
                    }
                }
            }
        }));

        // Spawn tasks dedicated to opening connections.
        // TODO: spawn several, or do things asynchronously, so that we try open multiple connections simultaneously
        for chain_index in 0..num_chains {
            (network_service.guarded.try_lock().unwrap().tasks_executor)(Box::pin({
                // TODO: keeping a Weak here doesn't really work to shut down tasks
                let network_service = Arc::downgrade(&network_service);
                async move {
                    loop {
                        // TODO: very crappy way of not spamming the network service ; instead we should wake this task up when a disconnect or a discovery happens
                        ffi::Delay::new(Duration::from_secs(1)).await;

                        let network_service = match network_service.upgrade() {
                            Some(ns) => ns,
                            None => {
                                return;
                            }
                        };

                        let start_connect =
                            match network_service.network.fill_out_slots(chain_index).await {
                                Some(sc) => sc,
                                None => continue,
                            };

                        let is_important_peer = network_service
                            .important_nodes
                            .contains(&start_connect.expected_peer_id);

                        // Convert the `multiaddr` (typically of the form `/ip4/a.b.c.d/tcp/d/ws`)
                        // into a `Future<dyn Output = Result<TcpStream, ...>>`.
                        let socket = {
                            log::debug!(target: "connections", "Pending({:?}) started: {}", start_connect.id, start_connect.multiaddr);
                            ffi::Connection::connect(&start_connect.multiaddr.to_string())
                        };

                        // TODO: handle dialing timeout here

                        let network_service2 = network_service.clone();
                        (network_service.guarded.lock().await.tasks_executor)(Box::pin({
                            connection_task(
                                socket,
                                network_service2,
                                start_connect.id,
                                start_connect.expected_peer_id,
                                is_important_peer,
                            )
                        }));
                    }
                }
            }));
        }

        (network_service.guarded.try_lock().unwrap().tasks_executor)(Box::pin({
            // TODO: keeping a Weak here doesn't really work to shut down tasks
            let network_service = Arc::downgrade(&network_service);
            async move {
                loop {
                    let network_service = match network_service.upgrade() {
                        Some(ns) => ns,
                        None => {
                            return;
                        }
                    };

                    network_service
                        .network
                        .next_substream()
                        .await
                        .open(ffi::Instant::now())
                        .await;
                }
            }
        }));

        (network_service, receivers)
    }

    // TODO: doc; explain the guarantees
    pub async fn block_query(
        self: Arc<Self>,
        chain_index: usize,
        hash: [u8; 32],
        fields: protocol::BlocksRequestFields,
    ) -> Result<protocol::BlockData, ()> {
        // TODO: better error?
        const NUM_ATTEMPTS: usize = 3;

        let request_config = protocol::BlocksRequestConfig {
            start: protocol::BlocksRequestConfigStart::Hash(hash),
            desired_count: NonZeroU32::new(1).unwrap(),
            direction: protocol::BlocksRequestDirection::Ascending,
            fields: fields.clone(),
        };

        // TODO: better peers selection ; don't just take the first 3
        // TODO: must only ask the peers that know about this block
        for target in self.peers_list().await.take(NUM_ATTEMPTS) {
            let mut result = match self
                .clone()
                .blocks_request(target, chain_index, request_config.clone())
                .await
            {
                Ok(b) => b,
                Err(_) => continue,
            };

            if result.len() != 1 {
                continue;
            }

            let result = result.remove(0);

            if result.header.is_none() && fields.header {
                continue;
            }
            if result
                .header
                .as_ref()
                .map_or(false, |h| header::decode(h).is_err())
            {
                continue;
            }
            if result.body.is_none() && fields.body {
                continue;
            }
            // Note: the presence of a justification isn't checked and can't be checked, as not
            // all blocks have a justification in the first place.
            if result.hash != hash {
                continue;
            }
            if result.header.as_ref().map_or(false, |h| {
                header::hash_from_scale_encoded_header(&h) != result.hash
            }) {
                continue;
            }
            match (&result.header, &result.body) {
                (Some(_), Some(_)) => {
                    // TODO: verify correctness of body
                }
                _ => {}
            }

            return Ok(result);
        }

        Err(())
    }

    /// Sends a blocks request to the given peer.
    // TODO: more docs
    pub async fn blocks_request(
        self: Arc<Self>,
        target: PeerId,
        chain_index: usize,
        config: protocol::BlocksRequestConfig,
    ) -> Result<Vec<protocol::BlockData>, service::BlocksRequestError> {
        log::debug!(target: "network", "Connection({}) <= BlocksRequest({:?})", target, config);

        let result = self
            .network
            .blocks_request(ffi::Instant::now(), target.clone(), chain_index, config)
            .await;

        log::debug!(
            target: "network",
            "Connection({}) => BlocksRequest({:?})",
            target,
            result.as_ref().map(|b| b.len())
        );

        result
    }

    /// Sends a grandpa warp sync request to the given peer.
    // TODO: more docs
    pub async fn grandpa_warp_sync_request(
        self: Arc<Self>,
        target: PeerId,
        chain_index: usize,
        begin_hash: [u8; 32],
    ) -> Result<protocol::GrandpaWarpSyncResponse, service::GrandpaWarpSyncRequestError> {
        log::debug!(target: "network", "Connection({}) <= GrandpaWarpSyncRequest({:?})", target, begin_hash);

        let result = self
            .network
            .grandpa_warp_sync_request(ffi::Instant::now(), target.clone(), chain_index, begin_hash)
            .await;

        log::debug!(
            target: "network",
            "Connection({}) => GrandpaWarpSyncRequest({:?})",
            target,
            result.as_ref().map(|response| response.fragments.len()),
        );

        result
    }

    /// Performs one or more storage proof requests in order to find the value of the given
    /// `requested_keys`.
    ///
    /// Must be passed a block hash and the Merkle value of the root node of the storage trie of
    /// this same block. The value of `storage_trie_root` corresponds to the value in the
    /// [`smoldot::header::HeaderRef::state_root`] field.
    ///
    /// Returns the storage values of `requested_keys` in the storage of the block, or an error if
    /// it couldn't be determined. If `Ok`, the `Vec` is guaranteed to have the same number of
    /// elements as `requested_keys`.
    ///
    /// This function is equivalent to calling [`NetworkService::storage_proof_request`] and
    /// verifying the proof, potentially multiple times until it succeeds. The number of attempts
    /// and the selection of peers is done through reasonable heuristics.
    pub async fn storage_query(
        self: Arc<Self>,
        chain_index: usize,
        block_hash: &[u8; 32],
        storage_trie_root: &[u8; 32],
        requested_keys: impl Iterator<Item = impl AsRef<[u8]>> + Clone,
    ) -> Result<Vec<Option<Vec<u8>>>, StorageQueryError> {
        const NUM_ATTEMPTS: usize = 3;

        let mut outcome_errors = Vec::with_capacity(NUM_ATTEMPTS);

        // TODO: better peers selection ; don't just take the first 3
        // TODO: must only ask the peers that know about this block
        for target in self.peers_list().await.take(NUM_ATTEMPTS) {
            let result = self
                .clone()
                .storage_proof_request(
                    chain_index,
                    target,
                    protocol::StorageProofRequestConfig {
                        block_hash: *block_hash,
                        keys: requested_keys.clone(),
                    },
                )
                .await
                .map_err(StorageQueryErrorDetail::Network)
                .and_then(|outcome| {
                    let mut result = Vec::with_capacity(requested_keys.clone().count());
                    for key in requested_keys.clone() {
                        result.push(
                            proof_verify::verify_proof(proof_verify::VerifyProofConfig {
                                proof: outcome.iter().map(|nv| &nv[..]),
                                requested_key: key.as_ref(),
                                trie_root_hash: &storage_trie_root,
                            })
                            .map_err(StorageQueryErrorDetail::ProofVerification)?
                            .map(|v| v.to_owned()),
                        );
                    }
                    debug_assert_eq!(result.len(), result.capacity());
                    Ok(result)
                });

            match result {
                Ok(values) => return Ok(values),
                Err(err) => {
                    outcome_errors.push(err);
                }
            }
        }

        Err(StorageQueryError {
            errors: outcome_errors,
        })
    }

    pub async fn storage_prefix_keys_query(
        self: Arc<Self>,
        chain_index: usize,
        block_hash: &[u8; 32],
        prefix: &[u8],
        storage_trie_root: &[u8; 32],
    ) -> Result<Vec<Vec<u8>>, StorageQueryError> {
        let mut prefix_scan = prefix_proof::prefix_scan(prefix_proof::Config {
            prefix,
            trie_root_hash: *storage_trie_root,
        });

        'main_scan: loop {
            const NUM_ATTEMPTS: usize = 3;

            let mut outcome_errors = Vec::with_capacity(NUM_ATTEMPTS);

            // TODO: better peers selection ; don't just take the first 3
            // TODO: must only ask the peers that know about this block
            for target in self.peers_list().await.take(NUM_ATTEMPTS) {
                let result = self
                    .clone()
                    .storage_proof_request(
                        chain_index,
                        target,
                        protocol::StorageProofRequestConfig {
                            block_hash: *block_hash,
                            keys: prefix_scan.requested_keys().map(|nibbles| {
                                trie::nibbles_to_bytes_extend(nibbles).collect::<Vec<_>>()
                            }),
                        },
                    )
                    .await
                    .map_err(StorageQueryErrorDetail::Network);

                match result {
                    Ok(proof) => {
                        match prefix_scan.resume(proof.iter().map(|v| &v[..])) {
                            Ok(prefix_proof::ResumeOutcome::InProgress(scan)) => {
                                // Continue next step of the proof.
                                prefix_scan = scan;
                                continue 'main_scan;
                            }
                            Ok(prefix_proof::ResumeOutcome::Success { keys }) => {
                                return Ok(keys);
                            }
                            Err((scan, err)) => {
                                prefix_scan = scan;
                                outcome_errors
                                    .push(StorageQueryErrorDetail::ProofVerification(err));
                            }
                        }
                    }
                    Err(err) => {
                        outcome_errors.push(err);
                    }
                }
            }

            return Err(StorageQueryError {
                errors: outcome_errors,
            });
        }
    }

    /// Sends a storage proof request to the given peer.
    ///
    /// See also [`NetworkService::storage_query`].
    // TODO: more docs
    pub async fn storage_proof_request(
        self: Arc<Self>,
        chain_index: usize,
        target: PeerId,
        config: protocol::StorageProofRequestConfig<impl Iterator<Item = impl AsRef<[u8]>>>,
    ) -> Result<Vec<Vec<u8>>, service::StorageProofRequestError> {
        log::debug!(
            target: "network",
            "Connection({}) <= StorageProofRequest({}, {})",
            target,
            HashDisplay(&config.block_hash),
            config.keys.size_hint().0
        );

        let result = self
            .network
            .storage_proof_request(ffi::Instant::now(), target.clone(), chain_index, config)
            .await;

        log::debug!(
            target: "network",
            "Connection({}) => StorageProofRequest({:?})",
            target,
            result.as_ref().map(|b| b.len())
        );

        result
    }

    // TODO: documentation
    pub async fn call_proof_query<'a>(
        self: Arc<Self>,
        chain_index: usize,
        config: protocol::CallProofRequestConfig<
            'a,
            impl Iterator<Item = impl AsRef<[u8]>> + Clone,
        >,
    ) -> Result<Vec<Vec<u8>>, CallProofQueryError> {
        const NUM_ATTEMPTS: usize = 3;

        let mut outcome_errors = Vec::with_capacity(NUM_ATTEMPTS);

        // TODO: better peers selection ; don't just take the first 3
        // TODO: must only ask the peers that know about this block
        for target in self.peers_list().await.take(NUM_ATTEMPTS) {
            let result = self
                .clone()
                .call_proof_request(chain_index, target, config.clone())
                .await;

            match result {
                Ok(value) => return Ok(value),
                Err(err) => {
                    outcome_errors.push(err);
                }
            }
        }

        Err(CallProofQueryError {
            errors: outcome_errors,
        })
    }

    /// Sends a call proof request to the given peer.
    ///
    /// See also [`NetworkService::call_proof_request`].
    // TODO: more docs
    pub async fn call_proof_request<'a>(
        self: Arc<Self>,
        chain_index: usize,
        target: PeerId,
        config: protocol::CallProofRequestConfig<'a, impl Iterator<Item = impl AsRef<[u8]>>>,
    ) -> Result<Vec<Vec<u8>>, service::CallProofRequestError> {
        log::debug!(
            target: "network",
            "Connection({}) <= CallProofRequest({}, {})",
            target,
            HashDisplay(&config.block_hash),
            config.method
        );

        let result = self
            .network
            .call_proof_request(ffi::Instant::now(), target.clone(), chain_index, config)
            .await;

        log::debug!(
            target: "network",
            "Connection({}) => CallProofRequest({:?})",
            target,
            result.as_ref().map(|b| b.len())
        );

        result
    }

    /// Announces transaction to the peers we are connected to.
    ///
    /// Returns a list of peers that we have sent the transaction to. Can return an empty `Vec`
    /// if we didn't send the transaction to any peer.
    ///
    /// Note that the remote doesn't confirm that it has received the transaction. Because
    /// networking is inherently unreliable, successfully sending a transaction to a peer doesn't
    /// necessarily mean that the remote has received it. In practice, however, the likelyhood of
    /// a transaction not being received are extremely low. This can be considered as known flaw.
    pub async fn announce_transaction(
        self: Arc<Self>,
        chain_index: usize,
        transaction: &[u8],
    ) -> Vec<PeerId> {
        let mut sent_peers = Vec::with_capacity(16); // TODO: capacity?

        // TODO: keep track of which peer knows about which transaction, and don't send it again

        for target in self.peers_list().await {
            if self
                .network
                .announce_transaction(&target, chain_index, &transaction)
                .await
                .is_ok()
            {
                sent_peers.push(target);
            };
        }

        sent_peers
    }

    /// Returns an iterator to the list of [`PeerId`]s that we have an established connection
    /// with.
    pub async fn peers_list(&self) -> impl Iterator<Item = PeerId> {
        self.network.peers_list().await
    }
}

/// Event that can happen on the network service.
#[derive(Debug, Clone)]
pub enum Event {
    Connected {
        peer_id: PeerId,
        chain_index: usize,
        best_block_number: u64,
        best_block_hash: [u8; 32],
    },
    Disconnected {
        peer_id: PeerId,
        chain_index: usize,
    },
    BlockAnnounce {
        peer_id: PeerId,
        chain_index: usize,
        announce: service::EncodedBlockAnnounce,
    },
}

/// Error that can happen when calling [`NetworkService::storage_query`].
#[derive(Debug)]
pub struct StorageQueryError {
    /// Contains one error per peer that has been contacted. If this list is empty, then we
    /// aren't connected to any node.
    pub errors: Vec<StorageQueryErrorDetail>,
}

impl StorageQueryError {
    /// Returns `true` if this is caused by networking issues, as opposed to a consensus-related
    /// issue.
    pub fn is_network_problem(&self) -> bool {
        self.errors.iter().all(|err| match err {
            StorageQueryErrorDetail::Network(service::StorageProofRequestError::Request(_)) => true,
            StorageQueryErrorDetail::Network(service::StorageProofRequestError::Decode(_)) => false,
            // TODO: as a temporary hack, we consider `TrieRootNotFound` as the remote not knowing about the requested block; see https://github.com/paritytech/substrate/pull/8046
            StorageQueryErrorDetail::ProofVerification(proof_verify::Error::TrieRootNotFound) => {
                true
            }
            StorageQueryErrorDetail::ProofVerification(_) => false,
        })
    }
}

impl fmt::Display for StorageQueryError {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        if self.errors.is_empty() {
            write!(f, "No node available for storage query")
        } else {
            write!(f, "Storage query errors:")?;
            for err in &self.errors {
                write!(f, "\n- {}", err)?;
            }
            Ok(())
        }
    }
}

/// See [`StorageQueryError`].
#[derive(Debug, derive_more::Display)]
pub enum StorageQueryErrorDetail {
    /// Error during the network request.
    #[display(fmt = "{}", _0)]
    Network(service::StorageProofRequestError),
    /// Error verifying the proof.
    #[display(fmt = "{}", _0)]
    ProofVerification(proof_verify::Error),
}

/// Error that can happen when calling [`NetworkService::call_proof_query`].
#[derive(Debug)]
pub struct CallProofQueryError {
    /// Contains one error per peer that has been contacted. If this list is empty, then we
    /// aren't connected to any node.
    pub errors: Vec<service::CallProofRequestError>,
}

impl fmt::Display for CallProofQueryError {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        if self.errors.is_empty() {
            write!(f, "No node available for call proof query")
        } else {
            write!(f, "Call proof query errors:")?;
            for err in &self.errors {
                write!(f, "\n- {}", err)?;
            }
            Ok(())
        }
    }
}

/// Asynchronous task managing a specific connection.
///
/// `is_important_peer` controls the log level used for problems that happen on this connection.
async fn connection_task(
    websocket: impl Future<Output = Result<Pin<Box<ffi::Connection>>, ()>>,
    network_service: Arc<NetworkService>,
    pending_id: service::PendingId,
    expected_peer_id: PeerId,
    is_important_peer: bool,
) {
    // Finishing the ongoing connection process.
    let mut websocket = match websocket.await {
        Ok(s) => s,
        Err(()) => {
            log::debug!(
                target: "connections",
                "Pending({:?}, {}) => Failed to reach",
                pending_id, expected_peer_id,
            );

            network_service
                .network
                .pending_outcome_err(pending_id)
                .await;

            return;
        }
    };

    let id = network_service
        .network
        .pending_outcome_ok(pending_id, ())
        .await;

    log::debug!(
        target: "connections",
        "Pending({:?}, {}) => Connection({:?})",
        pending_id,
        expected_peer_id,
        id
    );

    let mut write_buffer = vec![0; 4096];

    loop {
        let read_buffer = websocket.read_buffer().now_or_never().unwrap_or(Some(&[]));

        let now = ffi::Instant::now();

        let read_write = match network_service
            .network
            .read_write(id, now, read_buffer, (&mut write_buffer, &mut []))
            .await
        {
            Ok(rw) => rw,
            Err(_err) => {
                if is_important_peer {
                    log::warn!(target: "connections", "Error in connection with {}: {}", expected_peer_id, _err);
                } else {
                    log::debug!(target: "connections", "Connection({:?}, {}) => Closed: {}", id, expected_peer_id, _err);
                }

                return;
            }
        };

        let read_buffer_has_data = read_buffer.map_or(false, |b| !b.is_empty());
        let read_buffer_closed = read_buffer.is_none();

        if read_write.write_close && read_buffer_closed {
            log::debug!(target: "connections", "Connection({:?}, {}) => Closed gracefully", id, expected_peer_id);
            return;
        }

        if read_write.written_bytes != 0 {
            websocket.send(&write_buffer[..read_write.written_bytes]);
        }

        websocket.advance_read_cursor(read_write.read_bytes);

        // Starting from here, we block (or not) the current task until more processing needs
        // to happen.

        // Future ready when the connection state machine requests more processing.
        let poll_after = if let Some(wake_up) = read_write.wake_up_after {
            if wake_up > now {
                let dur = wake_up - now;
                future::Either::Left(ffi::Delay::new(dur))
            } else {
                continue;
            }
        } else {
            future::Either::Right(future::pending())
        }
        .fuse();

        // Future that is woken up when new data is ready on the socket.
        let read_buffer_ready =
            if !(read_buffer_has_data && read_write.read_bytes == 0) && !read_buffer_closed {
                future::Either::Left(websocket.read_buffer())
            } else {
                future::Either::Right(future::pending())
            };

        // Wait until either some data is ready on the socket, or the connection state machine
        // has been requested to be polled again.
        futures::pin_mut!(read_buffer_ready);
        future::select(
            future::select(read_buffer_ready, read_write.wake_up_future),
            poll_after,
        )
        .await;
    }
}

/// Returns the URL that corresponds to the given multiaddress. Returns an error if the
/// multiaddress protocols aren't supported.
fn multiaddr_to_url(addr: &Multiaddr) -> Result<String, ()> {
    let mut iter = addr.iter();
    let proto1 = iter.next().ok_or(())?;
    let proto2 = iter.next().ok_or(())?;
    let proto3 = iter.next().ok_or(())?;

    if iter.next().is_some() {
        return Err(());
    }

    match (proto1, proto2, proto3) {
        (Protocol::Ip4(ip), Protocol::Tcp(port), Protocol::Ws(url)) => {
            Ok(format!("ws://{}:{}{}", ip, port, url))
        }
        (Protocol::Ip6(ip), Protocol::Tcp(port), Protocol::Ws(url)) => {
            Ok(format!("ws://[{}]:{}{}", ip, port, url))
        }
        (Protocol::Ip4(ip), Protocol::Tcp(port), Protocol::Wss(url)) => {
            Ok(format!("wss://{}:{}{}", ip, port, url))
        }
        (Protocol::Ip6(ip), Protocol::Tcp(port), Protocol::Wss(url)) => {
            Ok(format!("wss://[{}]:{}{}", ip, port, url))
        }
        (Protocol::Dns(domain), Protocol::Tcp(port), Protocol::Ws(url))
        | (Protocol::Dns4(domain), Protocol::Tcp(port), Protocol::Ws(url))
        | (Protocol::Dns6(domain), Protocol::Tcp(port), Protocol::Ws(url)) => {
            Ok(format!("ws://{}:{}{}", domain, port, url))
        }
        (Protocol::Dns(domain), Protocol::Tcp(port), Protocol::Wss(url))
        | (Protocol::Dns4(domain), Protocol::Tcp(port), Protocol::Wss(url))
        | (Protocol::Dns6(domain), Protocol::Tcp(port), Protocol::Wss(url)) => {
            Ok(format!("wss://{}:{}{}", domain, port, url))
        }
        _ => Err(()),
    }
}
