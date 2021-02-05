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

use crate::network_service;

use futures::{
    channel::{mpsc, oneshot},
    lock::Mutex,
    prelude::*,
};
use smoldot::{chain, chain::sync::optimistic, informant::HashDisplay, libp2p, network};
use std::{collections::HashMap, num::NonZeroU32, pin::Pin, sync::Arc};

mod lossy_channel;

pub use lossy_channel::Receiver as NotificationsReceiver;

/// Configuration for a [`SyncService`].
pub struct Config {
    /// State of the finalized chain.
    pub chain_information: chain::chain_information::ChainInformation,

    /// Closure that spawns background tasks.
    pub tasks_executor: Box<dyn FnMut(Pin<Box<dyn Future<Output = ()> + Send>>) + Send>,

    /// Access to the network.
    pub network_service: Arc<network_service::NetworkService>,

    /// Receiver for events coming from the network, as returned by
    /// [`network_service::NetworkService::new`].
    pub network_events_receiver: mpsc::Receiver<network_service::Event>,
}

/// Identifier for a blocks request to be performed.
#[derive(Debug, Copy, Clone, Ord, PartialOrd, Eq, PartialEq, Hash)]
pub struct BlocksRequestId(usize);

pub struct SyncService {
    /// Sender of messages towards the background task.
    to_background: Mutex<mpsc::Sender<ToBackground>>,
}

impl SyncService {
    pub async fn new(mut config: Config) -> Self {
        let (to_background, from_foreground) = mpsc::channel(16);

        (config.tasks_executor)(Box::pin(
            start_sync(
                config.chain_information,
                from_foreground,
                config.network_service,
                config.network_events_receiver,
            )
            .await,
        ));

        SyncService {
            to_background: Mutex::new(to_background),
        }
    }

    /// Returns a string representing the state of the chain using the
    /// [`smoldot::database::finalized_serialize`] module.
    pub async fn serialize_chain(&self) -> String {
        let (send_back, rx) = oneshot::channel();

        self.to_background
            .lock()
            .await
            .send(ToBackground::Serialize { send_back })
            .await
            .unwrap();

        rx.await.unwrap()
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

    /// Returns `true` if the best block is known to be above the finalized block of the network.
    ///
    /// Also returns `false` if unknown.
    pub async fn is_above_network_finalized(&self) -> bool {
        let (send_back, rx) = oneshot::channel();

        self.to_background
            .lock()
            .await
            .send(ToBackground::IsAboveNetworkFinalized { send_back })
            .await
            .unwrap();

        rx.await.unwrap()
    }
}

async fn start_sync(
    chain_information: chain::chain_information::ChainInformation,
    mut from_foreground: mpsc::Receiver<ToBackground>,
    network_service: Arc<network_service::NetworkService>,
    mut from_network_service: mpsc::Receiver<network_service::Event>,
) -> impl Future<Output = ()> {
    let mut sync = optimistic::OptimisticSync::<_, libp2p::PeerId, ()>::new(optimistic::Config {
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
        full: false,
    });

    async move {
        let mut peers_source_id_map = HashMap::new();
        let mut block_requests_finished = stream::FuturesUnordered::new();

        let mut finalized_notifications = Vec::<lossy_channel::Sender<Vec<u8>>>::new();
        let mut best_notifications = Vec::<lossy_channel::Sender<Vec<u8>>>::new();

        loop {
            while let Some(action) = sync.next_request_action() {
                match action {
                    optimistic::RequestAction::Start {
                        start,
                        block_height,
                        source,
                        num_blocks,
                        ..
                    } => {
                        let block_request = network_service.clone().blocks_request(
                            source.clone(),
                            network::protocol::BlocksRequestConfig {
                                start: network::protocol::BlocksRequestConfigStart::Number(
                                    block_height,
                                ),
                                desired_count: num_blocks,
                                direction: network::protocol::BlocksRequestDirection::Ascending,
                                fields: network::protocol::BlocksRequestFields {
                                    header: true,
                                    body: false,
                                    justification: true,
                                },
                            },
                        );

                        let (block_request, abort) = future::abortable(block_request);
                        let request_id = start.start(abort);
                        block_requests_finished
                            .push(async move { (request_id, block_request.await.map_err(|_| ())) });
                    }
                    optimistic::RequestAction::Cancel { user_data, .. } => {
                        user_data.abort();
                    }
                }
            }

            let mut verified_blocks = 0u64;

            // Verify blocks that have been fetched from queries.
            // TODO: tweak this mechanism of stopping sync from time to time
            while verified_blocks < 4096 {
                match sync.process_one(crate::ffi::unix_time()) {
                    optimistic::ProcessOne::Idle { sync: s } => {
                        sync = s;
                        break;
                    }

                    optimistic::ProcessOne::NewBest {
                        sync: s,
                        new_best_number,
                        new_best_hash,
                    } => {
                        sync = s;

                        log::debug!(
                            target: "sync-verify",
                            "New best block: #{} ({})",
                            new_best_number,
                            HashDisplay(&new_best_hash),
                        );

                        let scale_encoded_header = sync.best_block_header().scale_encoding_vec();
                        // TODO: remove expired senders
                        for notif in &mut best_notifications {
                            let _ = notif.send(scale_encoded_header.clone());
                        }
                    }

                    optimistic::ProcessOne::Reset {
                        sync: s,
                        reason,
                        previous_best_height,
                    } => {
                        sync = s;

                        log::warn!(
                            target: "sync-verify",
                            "Failed to verify block #{}: {}",
                            previous_best_height + 1,
                            reason
                        );

                        let scale_encoded_header = sync.best_block_header().scale_encoding_vec();
                        // TODO: remove expired senders
                        for notif in &mut best_notifications {
                            let _ = notif.send(scale_encoded_header.clone());
                        }
                    }

                    optimistic::ProcessOne::Finalized {
                        sync: s,
                        finalized_blocks,
                        ..
                    } => {
                        sync = s;
                        verified_blocks += 1;

                        log::debug!(
                            target: "sync-verify",
                            "Finalized {} block",
                            finalized_blocks.len()
                        );

                        let scale_encoded_header = finalized_blocks
                            .last()
                            .unwrap()
                            .header
                            .scale_encoding()
                            .fold(Vec::new(), |mut a, b| {
                                a.extend_from_slice(b.as_ref());
                                a
                            });

                        // TODO: remove expired senders
                        for notif in &mut best_notifications {
                            let _ = notif.send(scale_encoded_header.clone());
                        }

                        // TODO: remove expired senders
                        for notif in &mut finalized_notifications {
                            let _ = notif.send(scale_encoded_header.clone());
                        }
                    }

                    // Other variants can be produced if the sync state machine is configured for
                    // syncing the storage, which is not the case here.
                    _ => unreachable!(),
                }
            }

            // Since `process_one` is a CPU-heavy operation, looping until it is done can
            // take a long time. In order to avoid blocking the rest of the program in the
            // meanwhile, the `yield_once` function interrupts the current task and gives a
            // chance for other tasks to progress.
            crate::yield_once().await;

            futures::select! {
                network_event = from_network_service.next() => {
                    let network_event = match network_event {
                        Some(m) => m,
                        None => {
                            return
                        },
                    };

                    match network_event {
                        network_service::Event::Connected { peer_id, best_block_number, .. } => {
                            let id = sync.add_source(peer_id.clone(), best_block_number);
                            peers_source_id_map.insert(peer_id.clone(), id);
                        },
                        network_service::Event::Disconnected(peer_id) => {
                            let id = peers_source_id_map.remove(&peer_id).unwrap();
                            let (_, rq_list) = sync.remove_source(id);
                            for (_, rq) in rq_list {
                                rq.abort();
                            }
                        },
                        network_service::Event::BlockAnnounce { peer_id, announce } => {
                            let id = *peers_source_id_map.get(&peer_id).unwrap();
                            sync.raise_source_best_block(id, announce.decode().header.number);
                        },
                    }
                }

                message = from_foreground.next() => {
                    let message = match message {
                        Some(m) => m,
                        None => {
                            return
                        },
                    };

                    match message {
                        ToBackground::Serialize { send_back } => {
                            let chain = sync.as_chain_information();
                            let serialized = smoldot::database::finalized_serialize::encode_chain_information(chain);
                            let _ = send_back.send(serialized);
                        }
                        ToBackground::IsAboveNetworkFinalized { send_back } => {
                            // TODO: only optimistic syncing is implemented yet, hence false
                            let _ = send_back.send(false);
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
                    }
                },

                (request_id, result) = block_requests_finished.select_next_some() => {
                    // `result` is an error if the block request got cancelled by the sync state
                    // machine.
                    if let Ok(result) = result {
                        let result = result.map_err(|_| ());
                        let _ = sync.finish_request(request_id, result.map(|v| v.into_iter().map(|block| optimistic::RequestSuccessBlock {
                            scale_encoded_header: block.header.unwrap(), // TODO: don't unwrap
                            scale_encoded_justification: block.justification,
                            scale_encoded_extrinsics: Vec::new(),
                            user_data: (),
                        })).map_err(|()| optimistic::RequestFail::BlocksUnavailable));
                    }
                },

                _ = async move {
                    if verified_blocks == 0 {
                        loop {
                            futures::pending!()
                        }
                    }
                }.fuse() => {}
            }
        }
    }
}

enum ToBackground {
    /// See [`SyncService::serialize_chain`].
    Serialize { send_back: oneshot::Sender<String> },
    /// See [`SyncService::is_above_network_finalized`].
    IsAboveNetworkFinalized { send_back: oneshot::Sender<bool> },
    /// See [`SyncService::subscribe_finalized`].
    SubscribeFinalized {
        send_back: oneshot::Sender<(Vec<u8>, lossy_channel::Receiver<Vec<u8>>)>,
    },
    /// See [`SyncService::subscribe_best`].
    SubscribeBest {
        send_back: oneshot::Sender<(Vec<u8>, lossy_channel::Receiver<Vec<u8>>)>,
    },
}
