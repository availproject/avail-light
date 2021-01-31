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
//! Importantly, its design is oriented towards the particular use case of the full node.
//!
//! The [`NetworkService`] spawns one background task (using the [`Config::tasks_executor`]) for
//! each active TCP socket, plus one for each TCP listening socket. Messages are exchanged between
//! the service and these background tasks.

// TODO: doc
// TODO: re-review this once finished

use crate::ffi;

use core::{num::NonZeroUsize, pin::Pin, time::Duration};
use futures::{channel::mpsc, lock::Mutex, prelude::*};
use smoldot::{
    informant::HashDisplay,
    libp2p::{
        connection,
        multiaddr::{Multiaddr, Protocol},
        peer_id::PeerId,
    },
    network::{protocol, service},
};
use std::sync::Arc;

#[derive(derive_more::Display)]
pub struct AnnounceTransactionError;

/// Configuration for a [`NetworkService`].
pub struct Config {
    /// Closure that spawns background tasks.
    pub tasks_executor: Box<dyn FnMut(Pin<Box<dyn Future<Output = ()> + Send>>) + Send>,

    /// Number of event receivers returned by [`NetworkService::new`].
    pub num_events_receivers: usize,

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
}

pub struct NetworkService {
    /// Fields behind a mutex.
    guarded: Mutex<Guarded>,

    /// Data structure holding the entire state of the networking.
    network: service::ChainNetwork<ffi::Instant, (), ()>,
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

        let network_service = Arc::new(NetworkService {
            guarded: Mutex::new(Guarded {
                tasks_executor: config.tasks_executor,
            }),
            network: service::ChainNetwork::new(service::Config {
                chains: vec![service::ChainConfig {
                    bootstrap_nodes: (0..config.bootstrap_nodes.len()).collect(),
                    in_slots: 25,
                    out_slots: 25,
                    protocol_id: config.protocol_id,
                    best_hash: config.best_block.1,
                    best_number: config.best_block.0,
                    genesis_hash: config.genesis_block_hash,
                    role: protocol::Role::Light,
                }],
                known_nodes: config
                    .bootstrap_nodes
                    .into_iter()
                    .map(|(peer_id, addr)| ((), peer_id, addr))
                    .collect(),
                listen_addresses: Vec::new(), // TODO:
                noise_key: connection::NoiseKey::new(&rand::random()),
                pending_api_events_buffer_size: NonZeroUsize::new(64).unwrap(),
                randomness_seed: rand::random(),
            }),
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
                                    break Event::Disconnected(peer_id);
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
                                debug_assert_eq!(chain_index, 0);
                                break Event::BlockAnnounce { peer_id, announce };
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
                                debug_assert_eq!(chain_index, 0);
                                break Event::Connected {
                                    peer_id,
                                    best_block_number: best_number,
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
                                debug_assert_eq!(chain_index, 0);
                                break Event::Disconnected(peer_id);
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

                    let start_connect = match network_service.network.fill_out_slots(0).await {
                        Some(sc) => sc,
                        None => continue,
                    };

                    // Convert the `multiaddr` (typically of the form `/ip4/a.b.c.d/tcp/d/ws`)
                    // into a `Future<dyn Output = Result<TcpStream, ...>>`.
                    let socket = match multiaddr_to_url(&start_connect.multiaddr) {
                        Ok(url) => {
                            log::debug!(target: "connections", "Pending({:?}) started: {}", start_connect.id, url);
                            ffi::WebSocket::connect(&url)
                        }
                        Err(()) => {
                            log::debug!(target: "connections", "Unsupported multiaddr: {}", start_connect.multiaddr);
                            network_service
                                .network
                                .pending_outcome_err(start_connect.id)
                                .await;
                            continue;
                        }
                    };

                    // TODO: handle dialing timeout here

                    let network_service2 = network_service.clone();
                    (network_service.guarded.lock().await.tasks_executor)(Box::pin({
                        connection_task(socket, network_service2, start_connect.id)
                    }));
                }
            }
        }));

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

    /// Sends a blocks request to the given peer.
    // TODO: more docs
    pub async fn blocks_request(
        self: Arc<Self>,
        target: PeerId,
        config: protocol::BlocksRequestConfig,
    ) -> Result<Vec<protocol::BlockData>, service::BlocksRequestError> {
        log::debug!(target: "network", "Connection({}) <= BlocksRequest({:?})", target, config);

        let result = self
            .network
            .blocks_request(ffi::Instant::now(), target.clone(), 0, config)
            .await; // TODO: chain_index

        log::debug!(
            target: "network",
            "Connection({}) => BlocksRequest({:?})",
            target,
            result.as_ref().map(|b| b.len())
        );

        result
    }

    /// Sends a storage proof request to the given peer.
    // TODO: more docs
    pub async fn storage_proof_request(
        self: Arc<Self>,
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
            .storage_proof_request(ffi::Instant::now(), target.clone(), 0, config)
            .await; // TODO: chain_index

        log::debug!(
            target: "network",
            "Connection({}) => StorageProofRequest({:?})",
            target,
            result.as_ref().map(|b| b.len())
        );

        result
    }

    /// Announces transaction to the peers we are connected to.
    /// Returns an error if we aren't connected to any peer, or if we fail to send the transaction to all peers.
    pub async fn announce_transaction(
        self: Arc<Self>,
        transaction: &[u8],
    ) -> Result<(), AnnounceTransactionError> {
        let mut any_propagated = false;

        for target in self.peers_list().await {
            if self
                .network
                .announce_transaction(&target, 0, &transaction)
                .await
                .is_ok()
            {
                any_propagated = true
            };
        }
        if any_propagated {
            Ok(())
        } else {
            Err(AnnounceTransactionError)
        }
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
        best_block_number: u64,
    },
    Disconnected(PeerId),
    BlockAnnounce {
        peer_id: PeerId,
        announce: service::EncodedBlockAnnounce,
    },
}

/// Asynchronous task managing a specific WebSocket connection.
async fn connection_task(
    websocket: impl Future<Output = Result<Pin<Box<ffi::WebSocket>>, ()>>,
    network_service: Arc<NetworkService>,
    pending_id: service::PendingId,
) {
    // Finishing the ongoing connection process.
    let mut websocket = match websocket.await {
        Ok(s) => s,
        Err(()) => {
            log::debug!(target: "connections", "Pending({:?}) => Failed to reach", pending_id);
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
    log::debug!(target: "connections", "Pending({:?}) => Connection({:?})", pending_id, id);

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
                log::debug!(target: "connections", "Connection({:?}) => Closed: {}", id, _err);
                return;
            }
        };

        let read_buffer_has_data = read_buffer.map_or(false, |b| !b.is_empty());
        let read_buffer_closed = read_buffer.is_none();

        if read_write.write_close && read_buffer_closed {
            log::debug!(target: "connections", "Connection({:?}) => Closed gracefully", id);
            return;
        }

        websocket.send(&write_buffer[..read_write.written_bytes]);

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
