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

//! Collection of libp2p connections.
//!
//! The [`Network`] struct in this module is a collection of libp2p connections. It uses internal
//! buffering and interior mutability in order to provide a convenient-to-use API based around
//! notifications protocols and request-response protocols.

use crate::network::{connection, peerset, Multiaddr, PeerId};

use alloc::sync::Arc;
use core::{
    num::NonZeroUsize,
    ops::{Add, Sub},
    task::{Context, Waker},
    time::Duration,
};
use futures::{
    channel::{mpsc, oneshot},
    lock::{Mutex, MutexGuard},
    prelude::*,
}; // TODO: no_std-ize
use rand::Rng as _;
use rand_chacha::{rand_core::SeedableRng as _, ChaCha20Rng};

/// Configuration for a [`Network`].
pub struct Config<TPeer> {
    /// Seed for the randomness within the networking state machine.
    ///
    /// While this seed influences the general behaviour of the networking state machine, it
    /// notably isn't used when generating the ephemeral key used for the Diffie-Hellman
    /// handshake.
    /// This is a defensive measure against users passing a dummy seed instead of actual entropy.
    pub randomness_seed: [u8; 32],

    /// Addresses to listen for incoming connections.
    pub listen_addresses: Vec<Multiaddr>,

    pub overlay_networks: Vec<OverlayNetwork>,

    pub known_nodes: Vec<(TPeer, PeerId, Multiaddr)>,

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
    /// The [`Network`] maintains an internal buffer of the events returned by
    /// [`Network::next_event`]. When [`Network::read_write`] is called, an event might get pushed
    /// to this buffer. If this buffer is full, back-pressure will be applied to the connections
    /// in order to prevent new events from being pushed.
    ///
    /// This value is important if [`Network::next_event`] is called at a slower than the calls to
    /// [`Network::read_write`] generate events.
    pub pending_api_events_buffer_size: NonZeroUsize,
}

/// Configuration for a specific overlay network.
///
/// See [`Config::overlay_networks`].
pub struct OverlayNetwork {
    /// Main protocol. A substream using this protocol is mandatory to be considered connected.
    pub main_protocol: ProtocolConfig,

    /// Additional protocols that might or might not be supported.
    pub optional_protocols: Vec<ProtocolConfig>,

    /// List of node identities that are known to belong to this overlay network. The node
    /// identities are indices in [`Config::known_nodes`].
    pub bootstrap_nodes: Vec<usize>,

    pub in_slots: u32,

    pub out_slots: u32,
}

pub struct ProtocolConfig {
    /// Name of the protocol negotiated on the wire.
    pub name: String,

    /// Optional alternative names for this protocol. Can represent different versions.
    ///
    /// Negotiated in order in which they are passed.
    pub fallback_names: Vec<String>,
}

/// Identifier of a pending connection requested by the network through a [`Event::StartConnect`].
#[derive(Debug, Copy, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct PendingId(peerset::ConnectionId);

/// Identifier of a connection spawned by the [`Network`].
#[derive(Debug, Copy, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ConnectionId(peerset::ConnectionId);

/// Data structure containing the list of all connections, pending or not, and their latest known
/// state. See also [the module-level documentation](..).
pub struct Network<TNow, TPeer, TConn> {
    /// Fields behind a mutex.
    guarded: Mutex<Guarded<TNow, TPeer, TConn>>,

    /// See [`Config::noise_key`].
    noise_key: connection::NoiseKey,

    /// Generator for randomness seeds given to the established connections.
    // TODO: what if children use ChaCha20 as well? is that safe?
    randomness_seeds: Mutex<ChaCha20Rng>,

    /// Receiver connected to [`Guarded::events_tx`].
    events_rx: Mutex<mpsc::Receiver<Event>>,
}

/// Fields of [`Network`] behind a mutex.
struct Guarded<TNow, TPeer, TConn> {
    /// Sender connected to [`Network::events_rx`].
    events_tx: mpsc::Sender<Event>,

    /// Holds the state of all the known nodes of the network, and of all the connections (pending
    /// or not).
    peerset: peerset::Peerset<
        TPeer,
        Arc<
            Mutex<(
                Option<
                    connection::established::Established<
                        TNow,
                        oneshot::Sender<Result<Vec<u8>, ()>>,
                        (),
                    >,
                >,
                TConn,
                Option<Waker>,
            )>,
        >,
        Arc<Mutex<Option<(connection::handshake::HealthyHandshake, TConn)>>>,
        (),
        (),
    >,
}

impl<TNow, TPeer, TConn> Network<TNow, TPeer, TConn>
where
    TNow: Clone + Add<Duration, Output = TNow> + Sub<TNow, Output = Duration> + Ord,
{
    /// Initializes a new network data structure.
    pub fn new(config: Config<TPeer>) -> Self {
        let (events_tx, events_rx) = mpsc::channel(config.pending_api_events_buffer_size.get() - 1);

        let mut peerset = peerset::Peerset::new(peerset::Config {
            randomness_seed: config.randomness_seed,
            peers_capacity: 50, // TODO: ?
            num_overlay_networks: config.overlay_networks.len(),
        });

        let mut ids = Vec::with_capacity(config.known_nodes.len());
        for (user_data, peer_id, multiaddr) in config.known_nodes {
            ids.push(peer_id.clone());
            let mut node = peerset.node_mut(peer_id).or_insert_with(move || user_data);
            node.add_known_address(multiaddr);
        }

        for (overlay_network_index, overlay_network) in
            config.overlay_networks.into_iter().enumerate()
        {
            for bootstrap_node in overlay_network.bootstrap_nodes {
                // TODO: cloning :(
                peerset
                    .node_mut(ids[bootstrap_node].clone())
                    .into_known()
                    .unwrap()
                    .add_to_overlay(overlay_network_index);
            }
        }

        Network {
            noise_key: config.noise_key,
            events_rx: Mutex::new(events_rx),
            guarded: Mutex::new(Guarded { peerset, events_tx }),
            randomness_seeds: Mutex::new(ChaCha20Rng::from_seed(config.randomness_seed)),
        }
    }

    /// Returns the number of established TCP connections, both incoming and outgoing.
    pub async fn num_established_connections(&self) -> usize {
        self.guarded
            .lock()
            .await
            .peerset
            .num_established_connections()
    }

    pub fn add_incoming_connection(
        &self,
        local_listen_address: &Multiaddr,
        remote_addr: Multiaddr,
        user_data: TConn,
    ) -> ConnectionId {
        todo!()
    }

    /// Sends a request to the given peer.
    // TODO: more docs
    // TODO: proper error type
    pub async fn request(
        &self,
        now: TNow,
        target: PeerId,
        protocol: String,
        request_data: Vec<u8>,
    ) -> Result<Vec<u8>, ()> {
        let connection = {
            let mut guarded = self.guarded.lock().await;

            let connection = match guarded.peerset.node_mut(target) {
                peerset::NodeMut::Known(n) => n.connections().next().ok_or(())?,
                peerset::NodeMut::Unknown(_) => return Err(()),
            };

            guarded
                .peerset
                .connection_mut(connection)
                .unwrap()
                .into_user_data()
                .clone()
        };

        let mut connection_lock = connection.lock().await;

        let (send_back, receive_result) = oneshot::channel();
        connection_lock
            .0
            .as_mut()
            .unwrap()
            .add_request(now, protocol, request_data, send_back);
        if let Some(waker) = connection_lock.2.take() {
            waker.wake();
        }

        // Make sure to unlock the connection before waiting for the result.
        drop(connection_lock);
        // The `Arc` to the connection should also be dropped, so that the channel gets dropped
        // if the connection is removed from the peerset.
        drop(connection);

        // Wait for the result of the request. Can take a long time (i.e. several seconds).
        match receive_result.await {
            Ok(r) => r,
            Err(_) => Err(()),
        }
    }

    /// After a [`Event::StartConnect`], notifies the [`Network`] of the success of the dialing
    /// attempt.
    ///
    /// See also [`Network::pending_outcome_err`].
    ///
    /// # Panic
    ///
    /// Panics if the [`PendingId`] is invalid.
    ///
    // TODO: timeout?
    pub async fn pending_outcome_ok(&self, id: PendingId, user_data: TConn) -> ConnectionId {
        let conn = self
            .guarded
            .lock()
            .await
            .peerset
            .pending_mut(id.0)
            .unwrap()
            .user_data_mut()
            .clone();

        let mut conn = conn.try_lock().unwrap();
        assert!(conn.is_none());
        *conn = Some((
            connection::handshake::HealthyHandshake::new(true),
            user_data,
        ));
        ConnectionId(id.0)
    }

    /// After a [`Event::StartConnect`], notifies the [`Network`] of the failure of the dialing
    /// attempt.
    ///
    /// See also [`Network::pending_outcome_ok`].
    ///
    /// # Panic
    ///
    /// Panics if the [`PendingId`] is invalid.
    ///
    // TODO: timeout?
    pub async fn pending_outcome_err(&self, id: PendingId) {
        let mut guarded = self.guarded.lock().await;
        guarded
            .peerset
            .pending_mut(id.0)
            .unwrap()
            .remove_and_purge_address();
    }

    /// Returns the next event produced by the service.
    ///
    /// This function should be called at a high enough rate that [`Network::read_write`] can
    /// continue pushing events to the internal buffer of events. Failure to call this function
    /// often enough will lead to connections being back-pressured.
    /// See also [`Config::pending_api_events_buffer_size`].
    ///
    /// It is technically possible to call this function multiple times simultaneously, in which
    /// case the events will be distributed amongst the multiple calls in an unspecified way.
    /// Keep in mind that some [`Event`]s have logic attached to the order in which they are
    /// produced, and calling this function multiple times is therefore discouraged.
    pub async fn next_event(&self) -> Event {
        if let Some(event) = self.fill_out_slots(&mut self.guarded.lock().await).await {
            return event;
        }

        let mut events_rx = self.events_rx.lock().await;
        events_rx.select_next_some().await
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
    ) -> Result<ReadWrite<TNow>, ConnectionError> {
        let mut total_read = 0;
        let mut total_written = 0;
        let mut wake_up_after = None;

        let mut guarded = self.guarded.lock().await;
        match guarded
            .peerset
            .pending_or_connection_mut(connection_id.0)
            .unwrap()
        {
            peerset::PendingOrConnectionMut::Pending(mut pending) => {
                let pending = pending.user_data_mut().clone();
                drop(guarded);

                let mut pending = pending.lock().await;

                let incoming_buffer = match incoming_buffer {
                    Some(b) => b,
                    None => {
                        let mut guarded = self.guarded.lock().await;
                        guarded
                            .peerset
                            .pending_mut(connection_id.0)
                            .unwrap()
                            .remove_and_purge_address();

                        debug_assert_eq!(total_read, 0);
                        // TODO: more appropriate error?
                        return Err(ConnectionError::Established(
                            connection::established::Error::ConnectionClosed,
                        ));
                    }
                };

                let (handshake, user_data) = pending.take().unwrap();

                // TODO: check timeout

                let (mut result, is_idle) = {
                    let (result, num_read, num_written) =
                        match handshake.read_write(incoming_buffer, outgoing_buffer) {
                            Ok(rw) => rw,
                            Err(err) => {
                                let mut guarded = self.guarded.lock().await;
                                let pending = guarded.peerset.pending_mut(connection_id.0).unwrap();
                                pending.remove_and_purge_address();
                                return Err(ConnectionError::Handshake(err));
                            }
                        };
                    total_read += num_read;
                    total_written += num_written;
                    (result, num_read == 0 && num_written == 0)
                };

                loop {
                    match result {
                        connection::handshake::Handshake::Healthy(updated_handshake) => {
                            *pending = Some((updated_handshake, user_data));
                            break;
                        }
                        connection::handshake::Handshake::Success {
                            remote_peer_id,
                            connection,
                        } => {
                            let randomness_seed = self.randomness_seeds.lock().await.gen();

                            let mut guarded = self.guarded.lock().await;
                            let pending = guarded.peerset.pending_mut(connection_id.0).unwrap();
                            if *pending.peer_id() != remote_peer_id {
                                pending.remove_and_purge_address();
                                return Err(ConnectionError::PeerIdMismatch);
                            }

                            pending.into_established(|_| {
                                let established =
                                    connection.into_connection(connection::established::Config {
                                        in_notifications_protocols: vec![
                                            connection::established::ConfigNotifications {
                                                name: "/dot/block-announces/1".to_string(), // TODO: correct protocolId
                                                max_handshake_size: 1024 * 1024,
                                            },
                                        ],
                                        in_request_protocols: vec![], // TODO:
                                        randomness_seed,
                                        ping_protocol: "/ipfs/ping/1.0.0".into(), // TODO: configurable
                                    });

                                Arc::new(Mutex::new((
                                    Some(established),
                                    user_data,
                                    Some(cx.waker().clone()),
                                )))
                            });

                            guarded
                                .events_tx
                                .send(Event::Connected(remote_peer_id))
                                .await
                                .unwrap();

                            cx.waker().wake_by_ref();
                            break;
                        }
                        connection::handshake::Handshake::NoiseKeyRequired(key) => {
                            result = key.resume(&self.noise_key).into();
                        }
                    }
                }
            }
            peerset::PendingOrConnectionMut::Connection(mut established) => {
                let established = established.user_data_mut().clone();
                drop(guarded);

                let mut established = established.lock().await;
                let established = &mut *established;

                // Update the `core::task::Waker` if necessary.
                match established.2 {
                    Some(ref w) if w.will_wake(cx.waker()) => {}
                    _ => established.2 = Some(cx.waker().clone()),
                }

                let read_write_result =
                    established
                        .0
                        .take()
                        .unwrap()
                        .read_write(now, incoming_buffer, outgoing_buffer);

                let read_write = match read_write_result {
                    Ok(rw) => rw,
                    Err(err) => {
                        let mut guarded = self.guarded.lock().await;
                        let peer_id = {
                            let c = guarded.peerset.connection_mut(connection_id.0).unwrap();
                            let peer_id = c.peer_id().clone();
                            c.remove();
                            peer_id
                        };

                        // TODO: only send if last connection
                        guarded
                            .events_tx
                            .send(Event::Disconnected(peer_id))
                            .await
                            .unwrap();

                        return Err(ConnectionError::Established(err));
                    }
                };

                total_read += read_write.read_bytes;
                total_written += read_write.written_bytes;
                debug_assert!(wake_up_after.is_none());
                wake_up_after = read_write.wake_up_after;
                established.0 = Some(read_write.connection);

                // TODO: finish here

                match read_write.event {
                    None => {}
                    Some(connection::established::Event::EndOfData) => todo!(),
                    Some(connection::established::Event::RequestIn {
                        id,
                        protocol,
                        request,
                    }) => todo!(),
                    Some(connection::established::Event::Response {
                        response,
                        user_data: send_back,
                        ..
                    }) => {
                        let _ = send_back.send(response.map_err(|_| ()));
                    }
                    Some(connection::established::Event::NotificationsInOpen {
                        id,
                        protocol,
                        handshake,
                    }) => {} // TODO:
                    Some(connection::established::Event::NotificationIn { id, notification }) => {
                        todo!()
                    }
                    Some(connection::established::Event::NotificationsOutAccept {
                        id,
                        remote_handshake,
                    }) => todo!(),
                    Some(connection::established::Event::NotificationsOutReject {
                        id,
                        user_data,
                    }) => todo!(),
                    Some(connection::established::Event::NotificationsOutCloseDemanded { id }) => {
                        todo!()
                    }
                }
            }
        }

        Ok(ReadWrite {
            read_bytes: total_read,
            written_bytes: total_written,
            wake_up_after,
            write_close: false, // TODO:
        })
    }

    /// Spawns new outgoing connections in order to fill empty outgoing slots.
    ///
    /// Must be passed as parameter an existing lock to a [`Guarded`].
    async fn fill_out_slots<'a>(
        &self,
        guarded: &mut MutexGuard<'a, Guarded<TNow, TPeer, TConn>>,
    ) -> Option<Event> {
        // Solves borrow checking errors regarding the borrow of multiple different fields at the
        // same time.
        let guarded = &mut **guarded;

        // TODO: limit number of slots

        for overlay_network_index in 0..guarded.peerset.num_overlay_networks() {
            // Grab nodes for which we have an established outgoing connections but haven't yet
            // opened a substream to.
            /*while let Some(node) = guarded
                .peerset
                .random_connected_closed_node(overlay_network_index)
            {
                let connection_id = node.connections().next().unwrap();
                let mut connection = guarded.peerset.connection_mut(connection_id).unwrap();
                // It is possible for the channel to be closed if the task has shut down. This will
                // be processed by `next_event` when detected.
                let _ = connection
                    .user_data_mut()
                    .send(ToConnection::OpenOutNotifications {
                        protocol: format!("/{}/block-announces/1", self.protocol_id),
                        handshake: protocol::encode_block_announces_handshake(
                            protocol::BlockAnnouncesHandshakeRef {
                                best_hash: &self.best_block.1,
                                best_number: self.best_block.0,
                                genesis_hash: &self.genesis_block_hash,
                                role: protocol::Role::Full, // TODO:
                            },
                        )
                        .fold(Vec::new(), |mut a, b| {
                            a.extend_from_slice(b.as_ref());
                            a
                        }),
                    })
                    .await;
                connection.add_pending_substream(0, ());
            }*/

            // TODO: very wip
            while let Some(mut node) = guarded.peerset.random_not_connected(overlay_network_index) {
                let first_addr = node.known_addresses().cloned().next();
                if let Some(multiaddr) = first_addr {
                    let id =
                        node.add_outbound_attempt(multiaddr.clone(), Arc::new(Mutex::new(None)));
                    return Some(Event::StartConnect {
                        id: PendingId(id),
                        multiaddr,
                    });
                }
            }
        }

        None
    }
}

/// Event generated by [`Network::next_event`].
#[derive(Debug)]
pub enum Event {
    Connected(PeerId),
    Disconnected(PeerId),

    /// User must start connecting to the given multiaddress.
    ///
    /// Either [`Network::pending_outcome_ok`] or [`Network::pending_outcome_err`] must later be
    /// called in order to inform of the outcome of the connection.
    StartConnect {
        id: PendingId,
        multiaddr: Multiaddr,
    },
}

/// Outcome of calling [`Network::read_write`].
pub struct ReadWrite<TNow> {
    /// Number of bytes at the start of the incoming buffer that have been processed. These bytes
    /// should no longer be present the next time [`Network::read_write`] is called.
    pub read_bytes: usize,

    /// Number of bytes written to the outgoing buffer. These bytes should be sent out to the
    /// remote. The rest of the outgoing buffer is left untouched.
    pub written_bytes: usize,

    /// If `Some`, [`Network::read_write`] should be called again when the point in time
    /// reaches the value in the `Option`.
    pub wake_up_after: Option<TNow>,

    /// If `true`, the writing side the connection must be closed. Will always remain to `true`
    /// after it has been set.
    ///
    /// If, after calling [`Network::read_write`], the returned [`ReadWrite`] contains `true` here,
    /// and the inbound buffer is `None`, then the [`ConnectionId`] is now invalid.
    pub write_close: bool,
}

pub enum ConnectionError {
    Established(connection::established::Error),
    Handshake(connection::handshake::HandshakeError),
    PeerIdMismatch,
}
