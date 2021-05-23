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

//! Background syncing service.
//!
//! The role of the [`SyncService`] is to do whatever necessary to obtain and stay up-to-date
//! with the best and the finalized blocks of a chain.
//!
//! The configuration of the chain to synchronize must be passed when creating a [`SyncService`],
//! after which it will spawn background tasks and use the networking service to stay
//! synchronized.
//!
//! Use [`SyncService::subscribe_best`] and [`SyncService::subscribe_finalized`] to get notified
//! about updates of the best and finalized blocks.

use crate::{ffi, lossy_channel, network_service, runtime_service};

use blake2_rfc::blake2b::blake2b;
use futures::{
    channel::{mpsc, oneshot},
    lock::Mutex,
    prelude::*,
};
use smoldot::{
    chain, header,
    informant::HashDisplay,
    libp2p::{self, PeerId},
    network::{self, protocol, service},
    sync::{all, para},
    trie::{self, prefix_proof, proof_verify},
};
use std::{collections::HashMap, convert::TryFrom as _, fmt, num::NonZeroU32, pin::Pin, sync::Arc};

pub use crate::lossy_channel::Receiver as NotificationsReceiver;

/// Configuration for a [`SyncService`].
pub struct Config {
    /// State of the finalized chain.
    pub chain_information: chain::chain_information::ChainInformation,

    /// Closure that spawns background tasks.
    pub tasks_executor: Box<dyn FnMut(Pin<Box<dyn Future<Output = ()> + Send>>) + Send>,

    /// Access to the network, and index of the chain to sync from the point of view of the
    /// network service.
    pub network_service: (Arc<network_service::NetworkService>, usize),

    /// Receiver for events coming from the network, as returned by
    /// [`network_service::NetworkService::new`].
    pub network_events_receiver: mpsc::Receiver<network_service::Event>,

    /// Extra fields used when the chain is a parachain.
    /// If `None`, this chain is a standalone chain or a relay chain.
    pub parachain: Option<ConfigParachain>,
}

/// See [`Config::parachain`].
pub struct ConfigParachain {
    /// Runtime service that synchronizes the relay chain of this parachain.
    pub relay_chain_sync: Arc<runtime_service::RuntimeService>,

    /// Index in the network service of [`Config::network_service`] of the relay chain of this
    /// parachain.
    pub relay_network_chain_index: usize,

    /// Id of the parachain within the relay chain.
    ///
    /// This is an arbitrary number used to identify the parachain within the storage of the
    /// relay chain.
    ///
    /// > **Note**: This information is normally found in the chain specification of the
    /// >           parachain.
    pub parachain_id: u32,
}

/// Identifier for a blocks request to be performed.
#[derive(Debug, Copy, Clone, Ord, PartialOrd, Eq, PartialEq, Hash)]
pub struct BlocksRequestId(usize);

pub struct SyncService {
    /// Sender of messages towards the background task.
    to_background: Mutex<mpsc::Sender<ToBackground>>,

    /// See [`Config::network_service`].
    network_service: Arc<network_service::NetworkService>,
    /// See [`Config::network_service`].
    network_chain_index: usize,
}

impl SyncService {
    pub async fn new(mut config: Config) -> Self {
        let (to_background, from_foreground) = mpsc::channel(16);

        if let Some(config_parachain) = config.parachain {
            (config.tasks_executor)(Box::pin(start_parachain(
                config.chain_information,
                from_foreground,
                config_parachain,
            )));
        } else {
            (config.tasks_executor)(Box::pin(
                start_relay_chain(
                    config.chain_information,
                    from_foreground,
                    config.network_service.0.clone(),
                    config.network_service.1,
                    config.network_events_receiver,
                )
                .await,
            ));
        }

        SyncService {
            to_background: Mutex::new(to_background),
            network_service: config.network_service.0,
            network_chain_index: config.network_service.1,
        }
    }

    /// Returns the SCALE-encoded header of the current finalized block, alongside with a stream
    /// producing updates of the finalized block.
    ///
    /// Not all updates are necessarily reported. In particular, updates that weren't pulled from
    /// the `Stream` yet might get overwritten by newest updates.
    pub async fn subscribe_finalized(&self) -> (Vec<u8>, NotificationsReceiver<Vec<u8>>) {
        let (send_back, rx) = oneshot::channel();

        self.to_background
            .lock()
            .await
            .send(ToBackground::SubscribeFinalized { send_back })
            .await
            .unwrap();

        rx.await.unwrap()
    }

    /// Returns the SCALE-encoded header of the current best block, alongside with a stream
    /// producing updates of the best block.
    ///
    /// Not all updates are necessarily reported. In particular, updates that weren't pulled from
    /// the `Stream` yet might get overwritten by newest updates.
    pub async fn subscribe_best(&self) -> (Vec<u8>, NotificationsReceiver<Vec<u8>>) {
        let (send_back, rx) = oneshot::channel();

        self.to_background
            .lock()
            .await
            .send(ToBackground::SubscribeBest { send_back })
            .await
            .unwrap();

        rx.await.unwrap()
    }

    /// Subscribes to the state of the chain: the current state and the new blocks.
    ///
    /// Contrary to [`SyncService::subscribe_best`], *all* new blocks are reported. Only up to
    /// `buffer_size` block notifications are buffered in the channel. If the channel is full
    /// when a new notification is attempted to be pushed, the channel gets closed.
    ///
    /// See [`SubscribeAll`] for information about the return value.
    pub async fn subscribe_all(&self, buffer_size: usize) -> SubscribeAll {
        let (send_back, rx) = oneshot::channel();

        self.to_background
            .lock()
            .await
            .send(ToBackground::SubscribeAll {
                send_back,
                buffer_size,
            })
            .await
            .unwrap();

        rx.await.unwrap()
    }

    /// Returns true if it is believed that we are near the head of the chain.
    ///
    /// The way this method is implemented is opaque and cannot be relied on. The return value
    /// should only ever be shown to the user and not used for any meaningful logic.
    pub async fn is_near_head_of_chain_heuristic(&self) -> bool {
        let (send_back, rx) = oneshot::channel();

        self.to_background
            .lock()
            .await
            .send(ToBackground::IsNearHeadOfChainHeuristic { send_back })
            .await
            .unwrap();

        rx.await.unwrap()
    }

    /// Returns the list of peers from the [`network_service::NetworkService`] that are expected to
    /// be aware of the given block.
    ///
    /// A peer is returned by this method either if it has directly sent a block announce in the
    /// past, or if the requested block height is below the finalized block height and the best
    /// block of the peer is above the requested block. In other words, it is assumed that all
    /// peers are always on the same finalized chain as the local node.
    pub async fn peers_assumed_know_blocks(
        &self,
        block_number: u64,
        block_hash: &[u8; 32],
    ) -> impl Iterator<Item = PeerId> {
        let (send_back, rx) = oneshot::channel();

        self.to_background
            .lock()
            .await
            .send(ToBackground::PeersAssumedKnowBlock {
                send_back,
                block_number,
                block_hash: *block_hash,
            })
            .await
            .unwrap();

        rx.await.unwrap().into_iter()
    }

    // TODO: doc; explain the guarantees
    pub async fn block_query(
        self: Arc<Self>,
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
        for target in self.network_service.peers_list().await.take(NUM_ATTEMPTS) {
            let mut result = match self
                .network_service
                .clone()
                .blocks_request(target, self.network_chain_index, request_config.clone())
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
    /// This function is equivalent to calling
    /// [`network_service::NetworkService::storage_proof_request`] and verifying the proof,
    /// potentially multiple times until it succeeds. The number of attempts and the selection of
    /// peers is done through reasonable heuristics.
    pub async fn storage_query(
        self: Arc<Self>,
        block_hash: &[u8; 32],
        storage_trie_root: &[u8; 32],
        requested_keys: impl Iterator<Item = impl AsRef<[u8]>> + Clone,
    ) -> Result<Vec<Option<Vec<u8>>>, StorageQueryError> {
        const NUM_ATTEMPTS: usize = 3;

        let mut outcome_errors = Vec::with_capacity(NUM_ATTEMPTS);

        // TODO: better peers selection ; don't just take the first 3
        // TODO: must only ask the peers that know about this block
        for target in self.network_service.peers_list().await.take(NUM_ATTEMPTS) {
            let result = self
                .network_service
                .clone()
                .storage_proof_request(
                    self.network_chain_index,
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
        block_number: u64,
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
            for target in self
                .peers_assumed_know_blocks(block_number, block_hash)
                .await
                .take(NUM_ATTEMPTS)
            {
                let result = self
                    .network_service
                    .clone()
                    .storage_proof_request(
                        self.network_chain_index,
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

    // TODO: documentation
    pub async fn call_proof_query<'a>(
        self: Arc<Self>,
        block_number: u64,
        config: protocol::CallProofRequestConfig<
            'a,
            impl Iterator<Item = impl AsRef<[u8]>> + Clone,
        >,
    ) -> Result<Vec<Vec<u8>>, CallProofQueryError> {
        const NUM_ATTEMPTS: usize = 3;

        let mut outcome_errors = Vec::with_capacity(NUM_ATTEMPTS);

        // TODO: better peers selection ; don't just take the first 3
        for target in self
            .peers_assumed_know_blocks(block_number, &config.block_hash)
            .await
            .take(NUM_ATTEMPTS)
        {
            let result = self
                .network_service
                .clone()
                .call_proof_request(self.network_chain_index, target, config.clone())
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
}

/// Error that can happen when calling [`SyncService::storage_query`].
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

/// Error that can happen when calling [`SyncService::call_proof_query`].
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

/// Return value of [`SyncService::subscribe_all`].
pub struct SubscribeAll {
    /// SCALE-encoded header of the finalized block at the time of the subscription.
    pub finalized_block_scale_encoded_header: Vec<u8>,
    /// List of all known non-finalized blocks at the time of subscription.
    ///
    /// Only one element in this list has [`BlockNotification::is_new_best`] equal to true.
    pub non_finalized_blocks: Vec<BlockNotification>,
    /// Channel onto which new blocks are sent. The channel gets closed if it is full when a new
    /// block needs to be reported.
    pub new_blocks: mpsc::Receiver<BlockNotification>,
}

/// Notification about a new block.
///
/// See [`SyncService::subscribe_all`].
#[derive(Debug)]
pub struct BlockNotification {
    /// True if this block is considered as the best block of the chain.
    pub is_new_best: bool,

    /// SCALE-encoded header of the block.
    pub scale_encoded_header: Vec<u8>,

    /// Hash of the header of the parent of this block.
    ///
    /// > **Note**: The header of a block contains the hash of its parent. When it comes to
    /// >           consensus algorithms such as Babe or Aura, the syncing code verifies that this
    /// >           hash, stored in the header, actually corresponds to a valid block. However,
    /// >           when it comes to parachain consensus, no such verification is performed.
    /// >           Contrary to the hash stored in the header, the value of this field is
    /// >           guaranteed to refer to a block that is known by the syncing service. This
    /// >           allows a subscriber of the state of the chain to precisely track the hierarchy
    /// >           of blocks, without risking to run into a problem in case of a block with an
    /// >           invalid header.
    pub parent_hash: [u8; 32],
}

async fn start_relay_chain(
    chain_information: chain::chain_information::ChainInformation,
    mut from_foreground: mpsc::Receiver<ToBackground>,
    network_service: Arc<network_service::NetworkService>,
    network_chain_index: usize,
    mut from_network_service: mpsc::Receiver<network_service::Event>,
) -> impl Future<Output = ()> {
    // TODO: implicit generics
    let mut sync = all::AllSync::<(), libp2p::PeerId, ()>::new(all::Config {
        chain_information,
        sources_capacity: 32,
        source_selection_randomness_seed: rand::random(),
        blocks_request_granularity: NonZeroU32::new(128).unwrap(),
        blocks_capacity: {
            // This is the maximum number of blocks between two consecutive justifications.
            1024
        },
        download_ahead_blocks: {
            // Verifying a block mostly consists in:
            //
            // - Verifying a sr25519 signature for each block, plus a VRF output when the
            // block is claiming a primary BABE slot.
            // - Verifying one ed25519 signature per authority for every justification.
            //
            // At the time of writing, the speed of these operations hasn't been benchmarked.
            // It is likely that it varies quite a bit between the various environments (the
            // different browser engines, and NodeJS).
            //
            // Assuming a maximum verification speed of 5k blocks/sec and a 95% latency of one
            // second, the number of blocks to download ahead of time in order to not block
            // is 5k.
            5000
        },
        full: None,
    });

    async move {
        // TODO: remove
        let mut peers_source_id_map = HashMap::new();

        // List of block requests currently in progress.
        let mut pending_block_requests = stream::FuturesUnordered::new();
        // List of grandpa warp sync requests currently in progress.
        let mut pending_grandpa_requests = stream::FuturesUnordered::new();
        // List of storage requests currently in progress.
        let mut pending_storage_requests = stream::FuturesUnordered::new();

        // TODO: remove; should store the aborthandle in the TRq user data instead
        let mut pending_requests = HashMap::new();

        let mut finalized_notifications = Vec::<lossy_channel::Sender<Vec<u8>>>::new();
        let mut best_notifications = Vec::<lossy_channel::Sender<Vec<u8>>>::new();
        let mut all_notifications = Vec::<mpsc::Sender<BlockNotification>>::new();

        // Queue of requests that the sync state machine wants to start and that haven't been
        // sent out yet.
        let mut requests_to_start = Vec::<all::Action>::with_capacity(16);

        let mut has_new_best = false;
        let mut has_new_finalized = false;

        // Main loop of the syncing logic.
        loop {
            // Drain the content of `requests_to_start` to actually start the requests that have
            // been queued by the previous iteration of the main loop.
            //
            // Note that the code below assumes that the `source_id`s found in `requests_to_start`
            // still exist. This is only guaranteed to be case because we process
            // `requests_to_start` as soon as an entry is added and before disconnect events can
            // remove sources from the state machine.
            for request in requests_to_start.drain(..) {
                match request {
                    all::Action::Start {
                        source_id,
                        request_id,
                        detail:
                            all::RequestDetail::BlocksRequest {
                                first_block,
                                ascending,
                                num_blocks,
                                request_headers,
                                request_bodies,
                                request_justification,
                            },
                    } => {
                        let peer_id = sync.source_user_data_mut(source_id).clone();

                        let block_request = network_service.clone().blocks_request(
                            peer_id.clone(),
                            network_chain_index,
                            network::protocol::BlocksRequestConfig {
                                start: match first_block {
                                    all::BlocksRequestFirstBlock::Hash(h) => {
                                        network::protocol::BlocksRequestConfigStart::Hash(h)
                                    }
                                    all::BlocksRequestFirstBlock::Number(n) => {
                                        network::protocol::BlocksRequestConfigStart::Number(n)
                                    }
                                },
                                desired_count: NonZeroU32::new(
                                    u32::try_from(num_blocks.get()).unwrap_or(u32::max_value()),
                                )
                                .unwrap(),
                                direction: if ascending {
                                    network::protocol::BlocksRequestDirection::Ascending
                                } else {
                                    network::protocol::BlocksRequestDirection::Descending
                                },
                                fields: network::protocol::BlocksRequestFields {
                                    header: request_headers,
                                    body: request_bodies,
                                    justification: request_justification,
                                },
                            },
                        );

                        let (block_request, abort) = future::abortable(block_request);
                        pending_requests.insert(request_id, abort);

                        pending_block_requests
                            .push(async move { (request_id, block_request.await) });
                    }
                    all::Action::Start {
                        source_id,
                        request_id,
                        detail:
                            all::RequestDetail::GrandpaWarpSync {
                                sync_start_block_hash,
                            },
                    } => {
                        let peer_id = sync.source_user_data_mut(source_id).clone();

                        let grandpa_request = network_service.clone().grandpa_warp_sync_request(
                            peer_id.clone(),
                            network_chain_index,
                            sync_start_block_hash,
                        );

                        let (grandpa_request, abort) = future::abortable(grandpa_request);
                        pending_requests.insert(request_id, abort);

                        pending_grandpa_requests
                            .push(async move { (request_id, grandpa_request.await) });
                    }
                    all::Action::Start {
                        source_id,
                        request_id,
                        detail:
                            all::RequestDetail::StorageGet {
                                block_hash,
                                state_trie_root,
                                keys,
                            },
                    } => {
                        let peer_id = sync.source_user_data_mut(source_id).clone();

                        let storage_request = network_service.clone().storage_proof_request(
                            network_chain_index,
                            peer_id.clone(),
                            network::protocol::StorageProofRequestConfig {
                                block_hash,
                                keys: keys.clone().into_iter(),
                            },
                        );

                        let storage_request = async move {
                            if let Ok(outcome) = storage_request.await {
                                // TODO: lots of copying around
                                // TODO: log what happens
                                keys.into_iter()
                                    .map(|key| {
                                        proof_verify::verify_proof(
                                            proof_verify::VerifyProofConfig {
                                                proof: outcome.iter().map(|nv| &nv[..]),
                                                requested_key: key.as_ref(),
                                                trie_root_hash: &state_trie_root,
                                            },
                                        )
                                        .map_err(|_| ())
                                        .map(|v| v.map(|v| v.to_vec()))
                                    })
                                    .collect::<Result<Vec<_>, ()>>()
                            } else {
                                Err(())
                            }
                        };

                        let (storage_request, abort) = future::abortable(storage_request);
                        pending_requests.insert(request_id, abort);

                        pending_storage_requests
                            .push(async move { (request_id, storage_request.await) });
                    }
                    all::Action::Cancel(request_id) => {
                        pending_requests.remove(&request_id).unwrap().abort();
                    }
                }
            }

            // The sync state machine can be in a few various states. At the time of writing:
            // idle, verifying header, verifying block, verifying grandpa warp sync proof,
            // verifying storage proof.
            // If the state is one of the "verifying" states, perform the actual verification and
            // loop again until the sync is in an idle state.
            loop {
                match sync.process_one() {
                    all::ProcessOne::AllSync(idle) => {
                        sync = idle;
                        break;
                    }
                    all::ProcessOne::VerifyWarpSyncFragment(verify) => {
                        let (sync_out, next_actions, result) = verify.perform();
                        sync = sync_out;
                        requests_to_start.extend(next_actions);

                        if let Err(err) = result {
                            // TODO: indicate peer who sent it?
                            log::warn!(
                                target: "sync-verify",
                                "Failed to verify warp sync fragment: {}", err
                            );
                        }
                    }
                    all::ProcessOne::VerifyHeader(verify) => {
                        let verified_hash = verify.hash();

                        match verify.perform(ffi::unix_time(), ()) {
                            all::HeaderVerifyOutcome::Success {
                                sync: sync_out,
                                next_actions,
                                is_new_best,
                                is_new_finalized,
                                ..
                            } => {
                                log::debug!(
                                    target: "sync-verify",
                                    "Successfully verified header {} (new best: {})",
                                    HashDisplay(&verified_hash),
                                    if is_new_best { "yes" } else { "no" }
                                );

                                requests_to_start.extend(next_actions);

                                if is_new_best {
                                    has_new_best = true;
                                }
                                if is_new_finalized {
                                    has_new_finalized = true;
                                }

                                // Elements in `all_notifications` are removed one by one and
                                // inserted back if the channel is still open.
                                for index in (0..all_notifications.len()).rev() {
                                    let mut subscription = all_notifications.swap_remove(index);
                                    // TODO: the code below is `O(n)` complexity
                                    let header = sync_out
                                        .non_finalized_blocks()
                                        .find(|h| h.hash() == verified_hash)
                                        .unwrap();
                                    let notification = BlockNotification {
                                        is_new_best,
                                        scale_encoded_header: header.scale_encoding_vec(),
                                        parent_hash: *header.parent_hash,
                                    };

                                    if subscription.try_send(notification).is_ok() {
                                        all_notifications.push(subscription);
                                    }
                                }

                                sync = sync_out;
                                continue;
                            }
                            all::HeaderVerifyOutcome::Error {
                                sync: sync_out,
                                next_actions,
                                error,
                                ..
                            } => {
                                log::warn!(
                                    target: "sync-verify",
                                    "Error while verifying header {}: {}",
                                    HashDisplay(&verified_hash),
                                    error
                                );

                                requests_to_start.extend(next_actions);
                                sync = sync_out;
                                continue;
                            }
                        }
                    }
                }
            }

            // TODO: handle this differently
            if has_new_best {
                has_new_best = false;

                let scale_encoded_header = sync.best_block_header().scale_encoding_vec();
                // TODO: remove expired senders
                for notif in &mut best_notifications {
                    let _ = notif.send(scale_encoded_header.clone());
                }

                // Since this task is verifying blocks, a heavy CPU-only operation, it is very
                // much possible for it to take a long time before having to wait for some event.
                // Since JavaScript/Wasm is single-threaded, this would prevent all the other
                // tasks in the background from running.
                // In order to provide a better granularity, we force a yield after each new serie
                // of verifications.
                crate::yield_once().await;
            }

            // TODO: handle this differently
            if has_new_finalized {
                has_new_finalized = false;

                // If the chain uses GrandPa, the networking has to be kept up-to-date with the
                // state of finalization for other peers to send back relevant gossip messages.
                // (code style) `grandpa_set_id` is extracted first in order to avoid borrowing
                // checker issues.
                let grandpa_set_id =
                    if let chain::chain_information::ChainInformationFinalityRef::Grandpa {
                        after_finalized_block_authorities_set_id,
                        ..
                    } = sync.as_chain_information().finality
                    {
                        Some(after_finalized_block_authorities_set_id)
                    } else {
                        None
                    };
                if let Some(set_id) = grandpa_set_id {
                    let commit_finalized_height =
                        u32::try_from(sync.finalized_block_header().number).unwrap(); // TODO: unwrap :-/
                    network_service
                        .set_local_grandpa_state(
                            network_chain_index,
                            network::service::GrandpaState {
                                set_id,
                                round_number: 1, // TODO:
                                commit_finalized_height,
                            },
                        )
                        .await;
                }

                let scale_encoded_header = sync.finalized_block_header().scale_encoding_vec();
                // TODO: remove expired senders
                for notif in &mut finalized_notifications {
                    let _ = notif.send(scale_encoded_header.clone());
                }

                // Since this task is verifying blocks, a heavy CPU-only operation, it is very
                // much possible for it to take a long time before having to wait for some event.
                // Since JavaScript/Wasm is single-threaded, this would prevent all the other
                // tasks in the background from running.
                // In order to provide a better granularity, we force a yield after each new serie
                // of verifications.
                crate::yield_once().await;
            }

            // All requests have been started.
            // Now waiting for some event to happen: a network event, a request from the frontend
            // of the sync service, or a request being finished.
            let response_outcome = futures::select! {
                network_event = from_network_service.next() => {
                    // Something happened on the network.

                    let network_event = match network_event {
                        Some(m) => m,
                        None => {
                            // The channel from the network service has been closed. Closing the
                            // sync background task as well.
                            return
                        },
                    };

                    match network_event {
                        network_service::Event::Connected { peer_id, chain_index, best_block_number, best_block_hash }
                            if chain_index == network_chain_index =>
                        {
                            let (id, requests) = sync.add_source(peer_id.clone(), best_block_number, best_block_hash);
                            peers_source_id_map.insert(peer_id, id);
                            requests_to_start.extend(requests);
                        },
                        network_service::Event::Disconnected { peer_id, chain_index }
                            if chain_index == network_chain_index =>
                        {
                            let id = peers_source_id_map.remove(&peer_id).unwrap();
                            let (requests, _) = sync.remove_source(id);
                            requests_to_start.extend(requests);
                        },
                        network_service::Event::BlockAnnounce { chain_index, peer_id, announce }
                            if chain_index == network_chain_index =>
                        {
                            let id = *peers_source_id_map.get(&peer_id).unwrap();
                            let decoded = announce.decode();
                            // TODO: stupid to re-encode header
                            // TODO: log the outcome
                            match sync.block_announce(id, decoded.header.scale_encoding_vec(), decoded.is_best) {
                                all::BlockAnnounceOutcome::HeaderVerify => {},
                                all::BlockAnnounceOutcome::TooOld => {},
                                all::BlockAnnounceOutcome::AlreadyInChain => {},
                                all::BlockAnnounceOutcome::NotFinalizedChain => {},
                                all::BlockAnnounceOutcome::InvalidHeader(_) => {},
                                all::BlockAnnounceOutcome::Disjoint { next_actions } => {
                                    requests_to_start.extend(next_actions);
                                },
                            }
                        },
                        network_service::Event::GrandpaCommitMessage { chain_index, message }
                            if chain_index == network_chain_index =>
                        {
                            match sync.grandpa_commit_message(&message.as_encoded()) {
                                Ok(()) => has_new_finalized = true,
                                Err(err) => {
                                    log::warn!(
                                        target: "sync-verify",
                                        "Error when verifying GrandPa commit message: {}", err
                                    );
                                }
                            }
                        },
                        _ => {
                            // Different chain index.
                        }
                    }

                    continue;
                }

                message = from_foreground.next() => {
                    // Received message from the front `SyncService`.
                    let message = match message {
                        Some(m) => m,
                        None => {
                            // The channel with the frontend sync service has been closed.
                            // Closing the sync background task as a result.
                            return
                        },
                    };

                    match message {
                        ToBackground::IsNearHeadOfChainHeuristic { send_back } => {
                            let _ = send_back.send(sync.is_near_head_of_chain_heuristic());
                        }
                        ToBackground::SubscribeFinalized { send_back } => {
                            let (tx, rx) = lossy_channel::channel();
                            finalized_notifications.push(tx);
                            let current = sync.finalized_block_header().scale_encoding_vec();
                            let _ = send_back.send((current, rx));
                        }
                        ToBackground::SubscribeBest { send_back } => {
                            let (tx, rx) = lossy_channel::channel();
                            best_notifications.push(tx);
                            let current = sync.best_block_header().scale_encoding_vec();
                            let _ = send_back.send((current, rx));
                        }
                        ToBackground::SubscribeAll { send_back, buffer_size } => {
                            let (tx, new_blocks) = mpsc::channel(buffer_size.saturating_sub(1));
                            all_notifications.push(tx);
                            let _ = send_back.send(SubscribeAll {
                                finalized_block_scale_encoded_header: sync.finalized_block_header().scale_encoding_vec(),
                                non_finalized_blocks: {
                                    let best_hash = sync.best_block_hash();
                                    sync.non_finalized_blocks().map(|h| {
                                        let scale_encoding = h.scale_encoding_vec();
                                        BlockNotification {
                                            is_new_best: header::hash_from_scale_encoded_header(&scale_encoding) == best_hash,
                                            scale_encoded_header: scale_encoding,
                                            parent_hash: *h.parent_hash,
                                        }
                                    }).collect()
                                },
                                new_blocks,
                            });
                        }
                        ToBackground::PeersAssumedKnowBlock { send_back, block_number, block_hash } => {
                            let finalized_num = sync.finalized_block_header().number;
                            let outcome = if block_number <= finalized_num {
                                sync.sources()
                                    .filter(|source_id| {
                                        let source_best = sync.source_best_block(*source_id);
                                        source_best.0 > block_number ||
                                            (source_best.0 == block_number && *source_best.1 == block_hash)
                                    })
                                    .map(|id| sync.source_user_data(id).clone())
                                    .collect()
                            } else {
                                // As documented, `knows_non_finalized_block` would panic if the
                                // block height was below the one of the known finalized block.
                                sync.knows_non_finalized_block(block_number, &block_hash)
                                    .map(|id| sync.source_user_data(id).clone())
                                    .collect()
                            };
                            let _ = send_back.send(outcome);
                        }
                    };

                    continue;
                },

                (request_id, result) = pending_block_requests.select_next_some() => {
                    pending_requests.remove(&request_id);

                    // A block(s) request has been finished.
                    // `result` is an error if the block request got cancelled by the sync state
                    // machine.
                    if let Ok(result) = result {
                        // Inject the result of the request into the sync state machine.
                        sync.blocks_request_response(
                            request_id,
                            result.map_err(|_| ()).map(|v| {
                                v.into_iter().filter_map(|block| {
                                    Some(all::BlockRequestSuccessBlock {
                                        scale_encoded_header: block.header?,
                                        scale_encoded_justification: block.justification,
                                        scale_encoded_extrinsics: Vec::new(),
                                        user_data: (),
                                    })
                                })
                            })
                        )

                    } else {
                        // The sync state machine has emitted a `Action::Cancel` earlier, and is
                        // thus no longer interested in the response.
                        continue;
                    }
                },

                (request_id, result) = pending_grandpa_requests.select_next_some() => {
                    pending_requests.remove(&request_id);

                    // A GrandPa warp sync request has been finished.
                    // `result` is an error if the block request got cancelled by the sync state
                    // machine.
                    if let Ok(result) = result {
                        // Inject the result of the request into the sync state machine.
                        sync.grandpa_warp_sync_response(
                            request_id,
                            result.ok(),
                        )

                    } else {
                        // The sync state machine has emitted a `Action::Cancel` earlier, and is
                        // thus no longer interested in the response.
                        continue;
                    }
                },

                (request_id, result) = pending_storage_requests.select_next_some() => {
                    pending_requests.remove(&request_id);

                    // A storage request has been finished.
                    // `result` is an error if the block request got cancelled by the sync state
                    // machine.
                    if let Ok(result) = result {
                        // Inject the result of the request into the sync state machine.
                        sync.storage_get_response(
                            request_id,
                            result.map(|list| list.into_iter()),
                        )

                    } else {
                        // The sync state machine has emitted a `Action::Cancel` earlier, and is
                        // thus no longer interested in the response.
                        continue;
                    }
                },
            };

            // `response_outcome` represents the way the state machine has changed as a
            // consequence of the response to a request.
            match response_outcome {
                all::ResponseOutcome::Queued { next_actions }
                | all::ResponseOutcome::NotFinalizedChain { next_actions, .. }
                | all::ResponseOutcome::AllAlreadyInChain { next_actions, .. } => {
                    requests_to_start.extend(next_actions);
                }
                all::ResponseOutcome::WarpSyncFinished { next_actions } => {
                    let finalized_num = sync.finalized_block_header().number;
                    log::info!(target: "sync-verify", "GrandPa warp sync finished to #{}", finalized_num);
                    has_new_finalized = true;
                    has_new_best = true;
                    requests_to_start.extend(next_actions);
                }
            }
        }
    }
}

async fn start_parachain(
    chain_information: chain::chain_information::ChainInformation,
    mut from_foreground: mpsc::Receiver<ToBackground>,
    parachain_config: ConfigParachain,
) {
    // TODO: handle finality as well; this is semi-complicated because the runtime service needs to provide a way to call a function on the finalized block's runtime

    let relay_best_blocks = {
        let (relay_best_block_header, relay_best_blocks_subscription) =
            parachain_config.relay_chain_sync.subscribe_best().await;
        stream::once(future::ready(relay_best_block_header)).chain(relay_best_blocks_subscription)
    };
    futures::pin_mut!(relay_best_blocks);

    let mut current_finalized_block = chain_information.finalized_block_header;
    let mut current_best_block = current_finalized_block.clone();

    // List of senders that get notified when the best block is modified.
    let mut best_subscriptions = Vec::<lossy_channel::Sender<_>>::new();

    // Hash of the head data of the best block of the parachain. `None` if no head data obtained
    // yet. Used to avoid sending out notifications if the head data hasn't changed.
    let mut previous_best_head_data_hash = None::<[u8; 32]>;

    loop {
        futures::select! {
            message = from_foreground.next().fuse() => {
                // Terminating the parachain sync task if the foreground has closed.
                let message = match message {
                    Some(m) => m,
                    None => return,
                };

                // Note that the rest of this `select!` statement can block for a long time,
                // which means that there might be a big delay for processing the messages here.
                // At the time of writing, the nature of the messages makes this a non-issue,
                // but care should be taken about this.

                match message {
                    ToBackground::IsNearHeadOfChainHeuristic { send_back } => {
                        // TODO: that doesn't seem totally correct
                        let _ = send_back.send(previous_best_head_data_hash.is_some());
                    },
                    ToBackground::SubscribeFinalized { send_back } => {
                        let (tx, rx) = lossy_channel::channel();
                        core::mem::forget(tx); // TODO:
                        let _ = send_back.send((current_finalized_block.scale_encoding_vec(), rx));
                    }
                    ToBackground::SubscribeBest { send_back } => {
                        let (tx, rx) = lossy_channel::channel();
                        best_subscriptions.push(tx);
                        let _ = send_back.send((current_best_block.scale_encoding_vec(), rx));
                    }
                    ToBackground::SubscribeAll { send_back, buffer_size } => {
                        let (tx, new_blocks) = mpsc::channel(buffer_size.saturating_sub(1));
                        let _ = send_back.send(SubscribeAll {
                            finalized_block_scale_encoded_header: current_finalized_block.scale_encoding_vec(),
                            non_finalized_blocks: Vec::new(),  // TODO: wrong /!\
                            new_blocks,
                        });

                        // TODO: `tx` is immediately discarded; the feature isn't actually fully implemented
                    }
                    ToBackground::PeersAssumedKnowBlock { send_back, block_number, block_hash } => {
                        let _ = send_back.send(Vec::new()); // TODO: implement this somehow /!\
                    }
                }
            },

            _relay_best_block = relay_best_blocks.next().fuse() => {
                // For each relay chain block, call `ParachainHost_persisted_validation_data` in
                // order to know where the parachains are.
                let pvd_result = parachain_config.relay_chain_sync.recent_best_block_runtime_call(
                    "ParachainHost_persisted_validation_data",
                    para::persisted_validation_data_parameters(
                        parachain_config.parachain_id,
                        para::OccupiedCoreAssumption::TimedOut
                    )
                ).await;

                // Even if there isn't any bug, the runtime call can likely fail because the relay
                // chain block has already been pruned from the network. This isn't a severe
                // error.
                let encoded_pvd = match pvd_result {
                    Ok(encoded_pvd) => encoded_pvd,
                    Err(err) => {
                        previous_best_head_data_hash = None;
                        if err.is_network_problem() {
                            log::debug!(target: "sync-verify", "Failed to get chain heads: {}", err);
                        } else {
                            log::warn!(target: "sync-verify", "Failed to get chain heads: {}", err);
                        }
                        continue;
                    }
                };

                // Try decode the result of the runtime call.
                // If this fails, it indicates an incompatibility between smoldot and the relay
                // chain.
                let head_data =
                    match para::decode_persisted_validation_data_return_value(&encoded_pvd) {
                        Ok(Some(pvd)) => pvd.parent_head,
                        Ok(None) => {
                            log::warn!(
                                target: "sync-verify",
                                "Couldn't find the parachain head from relay chain. \
                                The parachain likely doesn't occupy a core."
                            );
                            continue;
                        }
                        Err(error) => {
                            log::error!(
                                target: "sync-verify",
                                "Failed to fetch the parachain head from relay chain: {}",
                                error
                            );
                            continue;
                        }
                    };

                // Don't do anything more if the head data matches
                // `previous_best_head_data_hash`.
                match (&mut previous_best_head_data_hash, blake2b(32, &[], &head_data)) {
                    (&mut Some(ref mut h1), h2) if *h1 == h2.as_bytes() => continue,
                    (h1 @ _, h2) => *h1 = Some(<[u8; 32]>::try_from(h2.as_bytes()).unwrap()),
                };

                // The meaning of `head_data` depends on the parachain. It can represent
                // anything. In practice, however, it is most of the time a block header.
                match header::decode(&head_data) {
                    Ok(header) => {
                        current_best_block = header.into();
                        for sender in &mut best_subscriptions {
                            // TODO: remove senders if they're closed
                            let _ = sender.send(head_data.to_vec());
                        }
                    }
                    Err(_) => {
                        log::warn!(
                            target: "sync-verify",
                            "Head data is not a block header. This isn't supported by smoldot."
                        );
                    }
                }
            }
        }
    }
}

enum ToBackground {
    /// See [`SyncService::is_near_head_of_chain_heuristic`].
    IsNearHeadOfChainHeuristic { send_back: oneshot::Sender<bool> },
    /// See [`SyncService::subscribe_finalized`].
    SubscribeFinalized {
        send_back: oneshot::Sender<(Vec<u8>, lossy_channel::Receiver<Vec<u8>>)>,
    },
    /// See [`SyncService::subscribe_best`].
    SubscribeBest {
        send_back: oneshot::Sender<(Vec<u8>, lossy_channel::Receiver<Vec<u8>>)>,
    },
    /// See [`SyncService::subscribe_all`].
    SubscribeAll {
        send_back: oneshot::Sender<SubscribeAll>,
        buffer_size: usize,
    },
    /// See [`SyncService::peers_assumed_know_blocks`].
    PeersAssumedKnowBlock {
        send_back: oneshot::Sender<Vec<PeerId>>,
        block_number: u64,
        block_hash: [u8; 32],
    },
}
