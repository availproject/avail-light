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

//! Collection of libp2p connections.
//!
//! The [`Network`] struct in this module is a collection of libp2p connections. It uses internal
//! buffering and interior mutability in order to provide a convenient-to-use API based around
//! notifications protocols and request-response protocols.

use alloc::sync::Arc;
use core::{
    iter,
    num::NonZeroUsize,
    ops::{Add, Sub},
    task::{Context, Waker},
    time::Duration,
};
use futures::{
    channel::{mpsc, oneshot},
    lock::Mutex,
    prelude::*,
}; // TODO: no_std-ize
use rand::Rng as _;
use rand_chacha::{rand_core::SeedableRng as _, ChaCha20Rng};

pub mod connection;
pub mod discovery;
pub mod peer_id;
pub mod peerset;

pub use connection::established::ConfigRequestResponse;
pub use multiaddr::Multiaddr;
#[doc(inline)]
pub use parity_multiaddr as multiaddr;
pub use peer_id::PeerId;

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

    pub request_response_protocols: Vec<ConfigRequestResponse>,

    /// Name of the ping protocol on the network.
    pub ping_protocol: String,

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
    /// Name of the protocol negotiated on the wire.
    pub protocol_name: String,

    /// Optional alternative names for this protocol. Can represent different versions.
    ///
    /// Negotiated in order in which they are passed.
    pub fallback_protocol_names: Vec<String>,

    /// Maximum size, in bytes, of the handshake that can be received.
    pub max_handshake_size: usize,

    /// Maximum size, in bytes, of a notification that can be received.
    pub max_notification_size: usize,

    /// List of node identities that are known to belong to this overlay network. The node
    /// identities are indices in [`Config::known_nodes`].
    pub bootstrap_nodes: Vec<usize>,

    pub in_slots: u32,

    pub out_slots: u32,
}

/// Identifier of a pending connection requested by the network through a [`StartConnect`].
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

    /// See [`Config::overlay_networks`].
    overlay_networks: Vec<OverlayNetwork>,

    /// See [`Config::request_response_protocols`].
    request_response_protocols: Vec<ConfigRequestResponse>,

    /// See [`Config::ping_protocol`].
    ping_protocol: String,

    /// Generator for randomness seeds given to the established connections.
    randomness_seeds: Mutex<ChaCha20Rng>,

    /// Receiver connected to [`Guarded::events_tx`].
    events_rx: Mutex<mpsc::Receiver<Event<TConn>>>,
}

/// Fields of [`Network`] behind a mutex.
struct Guarded<TNow, TPeer, TConn> {
    /// Sender connected to [`Network::events_rx`].
    events_tx: mpsc::Sender<Event<TConn>>,

    /// Holds the state of all the known nodes of the network, and of all the connections (pending
    /// or not).
    peerset: peerset::Peerset<
        TPeer,
        Arc<Mutex<Connection<TNow, TConn>>>,
        Arc<Mutex<Option<(connection::handshake::HealthyHandshake, TConn)>>>,
        connection::established::SubstreamId,
        connection::established::SubstreamId,
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

        for (overlay_network_index, overlay_network) in config.overlay_networks.iter().enumerate() {
            for bootstrap_node in &overlay_network.bootstrap_nodes {
                // TODO: cloning :(
                peerset
                    .node_mut(ids[*bootstrap_node].clone())
                    .into_known()
                    .unwrap()
                    .add_to_overlay(overlay_network_index);
            }
        }

        Network {
            noise_key: config.noise_key,
            overlay_networks: config.overlay_networks,
            request_response_protocols: config.request_response_protocols,
            ping_protocol: config.ping_protocol,
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

    /// Returns an iterator to the list of [`PeerId`]s that we have an established connection
    /// with.
    pub async fn peers_list_lock(&self) -> impl Iterator<Item = PeerId> {
        // TODO: actually hold the lock so that we don't allocate a Vec
        let lock = self.guarded.lock().await;
        // TODO: what about duplicate PeerIds? should they be automatically deduplicated like here? decide and document
        // TODO: wrong hashing algorithm
        lock.peerset
            .connections_peer_ids()
            .map(|(_, peer_id)| peer_id.clone())
            .collect::<hashbrown::HashSet<_, fnv::FnvBuildHasher>>()
            .into_iter()
    }

    // TODO: document and improve API
    pub async fn add_addresses(
        &self,
        or_insert: impl FnOnce() -> TPeer,
        overlay_network_index: usize,
        peer_id: PeerId,
        addrs: impl IntoIterator<Item = Multiaddr>,
    ) {
        let mut lock = self.guarded.lock().await;
        let mut node = lock.peerset.node_mut(peer_id).or_insert_with(or_insert);
        for addr in addrs {
            node.add_known_address(addr);
        }
        node.add_to_overlay(overlay_network_index);
    }

    pub fn add_incoming_connection(
        &self,
        local_listen_address: &Multiaddr,
        remote_addr: Multiaddr,
        user_data: TConn,
    ) -> ConnectionId {
        todo!()
    }

    pub async fn connection_peer_id(&self, id: ConnectionId) -> PeerId {
        // TODO: cloning :-/
        self.guarded
            .lock()
            .await
            .peerset
            .connection_mut(id.0)
            .unwrap()
            .peer_id()
            .clone()
    }

    /// Sends a request to the given peer.
    // TODO: more docs
    pub async fn request(
        &self,
        now: TNow,
        target: PeerId,
        protocol_index: usize,
        request_data: Vec<u8>,
    ) -> Result<Vec<u8>, RequestError> {
        let connection = {
            let mut guarded = self.guarded.lock().await;

            let connection = match guarded.peerset.node_mut(target) {
                peerset::NodeMut::Known(n) => {
                    n.connections().next().ok_or(RequestError::NotConnected)?
                }
                peerset::NodeMut::Unknown(_) => return Err(RequestError::NotConnected),
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
            .connection
            .as_mut()
            .ok_or(RequestError::ConnectionClosed)?
            .add_request(now, protocol_index, request_data, send_back);
        if let Some(waker) = connection_lock.waker.take() {
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
            Err(_) => Err(RequestError::ConnectionClosed),
        }
    }

    /// Adds a notification to the queue of notifications to send to the given peer.
    ///
    /// Each substream maintains a queue of notifications to be sent to the remote. This method
    /// attempts to push a notification to this queue.
    ///
    /// As the API of this module is inherently subject to race conditions, it is possible for
    /// connections and substreams to no longer exist with this peer. This is a normal situation,
    /// and this error should simply be ignored apart from diagnostic purposes.
    ///
    /// An error is also returned if the queue exceeds a certain size in bytes, for two reasons:
    ///
    /// - Since the content of the queue is transferred at a limited rate, each notification
    /// pushed at the end of the queue will take more time than the previous one to reach the
    /// destination. Once the queue reaches a certain size, the time it would take for
    /// newly-pushed notifications to reach the destination would start being unreasonably large.
    ///
    /// - If the remote deliberately applies back-pressure on the substream, it is undesirable to
    /// increase the memory usage of the local node.
    ///
    /// Similarly, the queue being full is a normal situations and notification protocols should
    /// be designed in such a way that discarding notifications shouldn't have a too negative
    /// impact.
    ///
    /// Regardless of the success of this function, no guarantee exists about the successful
    /// delivery of notifications.
    ///
    pub async fn queue_notification(
        self,
        target: &PeerId,
        protocol_index: usize,
        notification: impl Into<Vec<u8>>,
    ) -> Result<(), QueueNotificationError> {
        let (connection, substream_id) = {
            let mut guarded = self.guarded.lock().await;

            let connection = match guarded.peerset.node_mut(target.clone()) {
                peerset::NodeMut::Known(n) => n
                    .connections()
                    .next()
                    .ok_or(QueueNotificationError::NotConnected)?,
                peerset::NodeMut::Unknown(_) => return Err(QueueNotificationError::NotConnected),
            };

            let substream = *guarded
                .peerset
                .connection_mut(connection)
                .unwrap()
                .open_substreams_mut()
                .find(|(protocol, dir, _)| {
                    *protocol == protocol_index && *dir == peerset::SubstreamDirection::Out
                })
                .ok_or(QueueNotificationError::NoSubstream)?
                .2;

            let connection_arc = guarded
                .peerset
                .connection_mut(connection)
                .unwrap()
                .into_user_data()
                .clone();

            (connection_arc, substream)
        };

        let mut connection_lock = connection.lock().await;

        let waker = connection_lock.waker.take();

        // TODO: check if substream is still open to avoid a panic in `write_notification_unbounded`
        // TODO: return an error if queue is full
        connection_lock
            .connection
            .as_mut()
            .ok_or(QueueNotificationError::NotConnected)?
            .write_notification_unbounded(substream_id, notification.into());

        drop(connection_lock);

        if let Some(waker) = waker {
            waker.wake();
        }

        Ok(())
    }

    /// After calling [`Network::fill_out_slots`], notifies the [`Network`] of the success of the
    /// dialing attempt.
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

    /// After calling [`Network::fill_out_slots`], notifies the [`Network`] of the failure of the
    /// dialing attempt.
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

    // TODO: futures cancellation concerns T_T
    pub async fn accept_notifications_in(
        &self,
        id: ConnectionId,
        overlay_network_index: usize,
        handshake: Vec<u8>,
    ) {
        let mut guarded = self.guarded.lock().await;

        let mut substream_id = None;

        // TODO: we use defensive programming here because the concurrency model is still blurry
        let mut connection = match guarded.peerset.connection_mut(id.0) {
            Some(c) => c,
            None => return,
        };

        if connection
            .confirm_substream(
                overlay_network_index,
                peerset::SubstreamDirection::In,
                |id| {
                    substream_id = Some(id);
                    id
                },
            )
            .is_err()
        {
            return;
        };

        let connection_arc = connection.user_data_mut().clone();
        drop(connection);
        drop(guarded);

        let mut connection = connection_arc.lock().await;

        if let Some(waker) = connection.waker.take() {
            waker.wake();
        }

        connection
            .connection
            .as_mut()
            .unwrap()
            .accept_in_notifications_substream(
                substream_id.unwrap(),
                handshake,
                overlay_network_index,
            );
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
    pub async fn next_event(&self) -> Event<TConn> {
        let mut events_rx = self.events_rx.lock().await;
        events_rx.select_next_some().await
    }

    ///
    /// # Panic
    ///
    /// Panics if `connection_id` isn't a valid connection.
    ///
    // TODO: document the `write_close` thing
    // TODO: futures cancellation concerns T_T
    pub async fn read_write<'a>(
        &self,
        connection_id: ConnectionId,
        now: TNow,
        incoming_buffer: Option<&[u8]>,
        outgoing_buffer: (&'a mut [u8], &'a mut [u8]),
        cx: &mut Context<'_>,
    ) -> Result<ReadWrite<TNow>, ConnectionError> {
        let mut read_write = ReadWrite {
            read_bytes: 0,
            written_bytes: 0,
            wake_up_after: None,
            write_close: false,
        };

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

                        debug_assert_eq!(read_write.read_bytes, 0);
                        read_write.write_close = true;
                        return Ok(read_write);
                    }
                };

                let (handshake, user_data) = pending.take().unwrap();

                // TODO: check timeout

                let mut result = {
                    let (result, num_read, num_written) =
                        match handshake.read_write(incoming_buffer, outgoing_buffer) {
                            Ok(rw) => rw,
                            Err(err) => {
                                let mut guarded = self.guarded.lock().await;
                                guarded
                                    .peerset
                                    .pending_mut(connection_id.0)
                                    .unwrap()
                                    .remove_and_purge_address();
                                return Err(ConnectionError::Handshake(err));
                            }
                        };
                    read_write.read_bytes += num_read;
                    read_write.written_bytes += num_written;
                    if num_read != 0 || num_written != 0 {
                        cx.waker().wake_by_ref();
                    }
                    result
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
                            let mut guarded = self.guarded.lock().await;
                            let pending = guarded.peerset.pending_mut(connection_id.0).unwrap();
                            if *pending.peer_id() != remote_peer_id {
                                pending.remove_and_purge_address();
                                return Err(ConnectionError::PeerIdMismatch);
                            }

                            pending.into_established({
                                let config = self.build_connection_config().await;
                                let waker = cx.waker().clone();
                                move |_| {
                                    let established = connection.into_connection(config);
                                    Arc::new(Mutex::new(Connection {
                                        connection: Some(established),
                                        user_data: Some(user_data),
                                        waker: Some(waker),
                                    }))
                                }
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

                established
                    .lock()
                    .await
                    .read_write(
                        &self.guarded,
                        connection_id,
                        now,
                        incoming_buffer,
                        outgoing_buffer,
                        &mut read_write,
                        cx,
                    )
                    .await?;
            }
        }

        Ok(read_write)
    }

    async fn build_connection_config(&self) -> connection::established::Config {
        let randomness_seed = self.randomness_seeds.lock().await.gen();
        connection::established::Config {
            notifications_protocols: self
                .overlay_networks
                .iter()
                .flat_map(|net| {
                    let max_handshake_size = net.max_handshake_size;
                    let max_notification_size = net.max_notification_size;
                    iter::once(&net.protocol_name)
                        .chain(net.fallback_protocol_names.iter())
                        .map(move |name| {
                            connection::established::ConfigNotifications {
                                name: name.clone(), // TODO: cloning :-/
                                max_handshake_size,
                                max_notification_size,
                            }
                        })
                })
                .collect(),
            request_protocols: self.request_response_protocols.clone(),
            randomness_seed,
            ping_protocol: self.ping_protocol.clone(), // TODO: cloning :-/
        }
    }

    pub async fn open_next_substream(&'_ self) -> Option<SubstreamOpen<'_, TNow, TPeer, TConn>> {
        let mut guarded = self.guarded.lock().await;

        for overlay_network_index in 0..guarded.peerset.num_overlay_networks() {
            // Grab node for which we have an established outgoing connections but haven't yet
            // opened a substream to.
            if let Some(node) = guarded
                .peerset
                .random_connected_closed_node(overlay_network_index)
            {
                let connection_id = node.connections().next().unwrap();
                let mut peerset_entry = guarded.peerset.connection_mut(connection_id).unwrap();
                return Some(SubstreamOpen {
                    network: self,
                    connection: peerset_entry.user_data_mut().clone(),
                    connection_id,
                    overlay_network_index,
                });
            }
        }

        None
    }

    /// Spawns new outgoing connections in order to fill empty outgoing slots.
    // TODO: give more control, with number of slots and node choice
    pub async fn fill_out_slots<'a>(&self, overlay_network_index: usize) -> Option<StartConnect> {
        let mut guarded = self.guarded.lock().await;
        // Solves borrow checking errors regarding the borrow of multiple different fields at the
        // same time.
        let guarded = &mut *guarded;

        // TODO: limit number of slots

        // TODO: very wip
        while let Some(mut node) = guarded.peerset.random_not_connected(overlay_network_index) {
            let first_addr = node.known_addresses().cloned().next();
            if let Some(multiaddr) = first_addr {
                let id = node.add_outbound_attempt(multiaddr.clone(), Arc::new(Mutex::new(None)));
                return Some(StartConnect {
                    id: PendingId(id),
                    multiaddr,
                });
            }
        }

        None
    }
}

/// Data structure holding the state of a single established (i.e. post-handshake) connection.
///
/// This data structure is wrapped around `Arc<Mutex<>>`. As such, its fields do not necessarily
/// match the state in the [`Network`].
struct Connection<TNow, TConn> {
    connection: Option<
        connection::established::Established<
            TNow,
            oneshot::Sender<Result<Vec<u8>, RequestError>>,
            usize,
        >,
    >,
    user_data: Option<TConn>,
    waker: Option<Waker>,
}

impl<TNow, TConn> Connection<TNow, TConn>
where
    TNow: Clone + Add<Duration, Output = TNow> + Sub<TNow, Output = Duration> + Ord,
{
    // TODO: improve API
    // TODO: futures cancellation concerns T_T
    async fn read_write<'a, TPeer>(
        &mut self,
        guarded: &Mutex<Guarded<TNow, TPeer, TConn>>,
        connection_id: ConnectionId,
        now: TNow,
        incoming_buffer: Option<&[u8]>,
        outgoing_buffer: (&'a mut [u8], &'a mut [u8]),
        read_write: &mut ReadWrite<TNow>,
        cx: &mut Context<'_>,
    ) -> Result<(), ConnectionError> {
        // Update the `core::task::Waker` if necessary.
        match self.waker {
            Some(ref w) if w.will_wake(cx.waker()) => {}
            _ => self.waker = Some(cx.waker().clone()),
        }

        let read_write_result =
            self.connection
                .take()
                .unwrap()
                .read_write(now, incoming_buffer, outgoing_buffer);

        // TODO: should return now without doing anything async, otherwise there's cancellation problems

        let read_write_result = match read_write_result {
            Ok(rw) => rw,
            Err(err) => {
                let mut guarded = guarded.lock().await;
                let mut out_overlay_network_indices =
                    Vec::with_capacity(guarded.peerset.num_overlay_networks());
                let mut in_overlay_network_indices =
                    Vec::with_capacity(guarded.peerset.num_overlay_networks());
                let peer_id = {
                    let mut c = guarded.peerset.connection_mut(connection_id.0).unwrap();
                    for (overlay_network_index, direction, _) in c.open_substreams_mut() {
                        match direction {
                            peerset::SubstreamDirection::In => {
                                in_overlay_network_indices.push(overlay_network_index)
                            }
                            peerset::SubstreamDirection::Out => {
                                out_overlay_network_indices.push(overlay_network_index)
                            }
                        }
                    }
                    let peer_id = c.peer_id().clone();
                    c.remove();
                    peer_id
                };

                // TODO: only send if last connection
                guarded
                    .events_tx
                    .send(Event::Disconnected {
                        peer_id,
                        user_data: self.user_data.take().unwrap(),
                        in_overlay_network_indices,
                        out_overlay_network_indices,
                    })
                    .await
                    .unwrap();

                return Err(ConnectionError::Established(err));
            }
        };

        read_write.read_bytes += read_write_result.read_bytes;
        read_write.written_bytes += read_write_result.written_bytes;
        debug_assert!(read_write.wake_up_after.is_none());
        read_write.wake_up_after = read_write_result.wake_up_after;
        read_write.write_close = read_write_result.write_close;
        self.connection = Some(read_write_result.connection);

        if read_write_result.read_bytes != 0
            || read_write_result.written_bytes != 0
            || read_write_result.event.is_some()
        {
            if let Some(waker) = self.waker.take() {
                waker.wake();
            }
        }

        // TODO: finish here

        match read_write_result.event {
            None => {}
            Some(connection::established::Event::RequestIn {
                id,
                protocol_index,
                request,
            }) => todo!(),
            Some(connection::established::Event::Response {
                response,
                user_data: send_back,
                ..
            }) => {
                let _ = send_back.send(response.map_err(RequestError::Connection));
            }
            Some(connection::established::Event::NotificationsInOpen {
                id,
                protocol_index: overlay_network_index,
                handshake,
            }) => {
                let mut guarded = guarded.lock().await;

                guarded
                    .peerset
                    .connection_mut(connection_id.0)
                    .unwrap()
                    .add_pending_substream(
                        overlay_network_index,
                        peerset::SubstreamDirection::In,
                        id,
                    );

                guarded
                    .events_tx
                    .send(Event::NotificationsInOpen {
                        id: connection_id,
                        overlay_network_index,
                        remote_handshake: handshake,
                    })
                    .await
                    .unwrap();
            }
            Some(connection::established::Event::NotificationsInOpenCancel {
                protocol_index,
                ..
            }) => {
                let mut guarded = guarded.lock().await;
                guarded
                    .peerset
                    .connection_mut(connection_id.0)
                    .unwrap()
                    .remove_pending_substream(protocol_index, peerset::SubstreamDirection::In)
                    .unwrap();
            }
            Some(connection::established::Event::NotificationIn { id, notification }) => {
                let overlay_network_index = *self
                    .connection
                    .as_mut()
                    .unwrap()
                    .notifications_substream_user_data_mut(id)
                    .unwrap();

                let mut guarded = guarded.lock().await;

                let mut connection = guarded.peerset.connection_mut(connection_id.0).unwrap();
                let peer_id = connection.peer_id().clone();
                let has_symmetric_substream = connection
                    .has_open_substream(overlay_network_index, peerset::SubstreamDirection::Out);

                guarded
                    .events_tx
                    .send(Event::NotificationsIn {
                        id: connection_id,
                        peer_id,
                        has_symmetric_substream,
                        overlay_network_index,
                        notification,
                    })
                    .await
                    .unwrap();
            }
            Some(connection::established::Event::NotificationsOutAccept {
                id,
                remote_handshake,
            }) => {
                let overlay_network_index = *self
                    .connection
                    .as_mut()
                    .unwrap()
                    .notifications_substream_user_data_mut(id)
                    .unwrap();

                let mut guarded = guarded.lock().await;

                guarded
                    .peerset
                    .connection_mut(connection_id.0)
                    .unwrap()
                    .confirm_substream(
                        overlay_network_index,
                        peerset::SubstreamDirection::Out,
                        |id| id,
                    );

                guarded
                    .events_tx
                    .send(Event::NotificationsOutAccept {
                        id: connection_id,
                        overlay_network_index,
                        remote_handshake,
                    })
                    .await
                    .unwrap();
            }
            Some(connection::established::Event::NotificationsOutReject {
                id,
                user_data: overlay_network_index,
            }) => {
                let mut guarded = guarded.lock().await;

                let _expected_id = guarded
                    .peerset
                    .connection_mut(connection_id.0)
                    .unwrap()
                    .remove_pending_substream(
                        overlay_network_index,
                        peerset::SubstreamDirection::Out,
                    )
                    .unwrap();
                debug_assert_eq!(id, _expected_id);

                guarded
                    .events_tx
                    .send(Event::NotificationsOutReject {
                        id: connection_id,
                        overlay_network_index,
                    })
                    .await
                    .unwrap();
            }
            Some(connection::established::Event::NotificationsOutCloseDemanded { id }) => {
                todo!()
            }
            Some(connection::established::Event::NotificationsOutReset {
                id,
                user_data: overlay_network_index,
            }) => {
                let mut guarded = guarded.lock().await;

                let _expected_id = guarded
                    .peerset
                    .connection_mut(connection_id.0)
                    .unwrap()
                    .remove_pending_substream(
                        overlay_network_index,
                        peerset::SubstreamDirection::Out,
                    )
                    .unwrap();
                debug_assert_eq!(id, _expected_id);

                guarded
                    .events_tx
                    .send(Event::NotificationsOutClose {
                        id: connection_id,
                        overlay_network_index,
                    })
                    .await
                    .unwrap();
            }
        }

        // Connetion is considered closed if `write_close` is `true` and `incoming_buffer`
        // is `None`.
        if read_write.write_close && incoming_buffer.is_none() {
            let mut guarded = guarded.lock().await;
            let mut out_overlay_network_indices =
                Vec::with_capacity(guarded.peerset.num_overlay_networks());
            let mut in_overlay_network_indices =
                Vec::with_capacity(guarded.peerset.num_overlay_networks());
            let mut connection = guarded.peerset.connection_mut(connection_id.0).unwrap();
            let peer_id = connection.peer_id().clone(); // TODO: no
            for (overlay_network_index, direction, _) in connection.open_substreams_mut() {
                match direction {
                    peerset::SubstreamDirection::In => {
                        in_overlay_network_indices.push(overlay_network_index)
                    }
                    peerset::SubstreamDirection::Out => {
                        out_overlay_network_indices.push(overlay_network_index)
                    }
                }
            }
            connection.remove();
            guarded
                .events_tx
                .send(Event::Disconnected {
                    peer_id,
                    user_data: self.user_data.take().unwrap(),
                    out_overlay_network_indices,
                    in_overlay_network_indices,
                })
                .await
                .unwrap();
        }

        Ok(())
    }
}

/// User must start connecting to the given multiaddress.
///
/// Either [`Network::pending_outcome_ok`] or [`Network::pending_outcome_err`] must later be
/// called in order to inform of the outcome of the connection.
#[derive(Debug)]
#[must_use]
pub struct StartConnect {
    pub id: PendingId,
    pub multiaddr: Multiaddr,
}

/// Event generated by [`Network::next_event`].
#[derive(Debug)]
pub enum Event<TConn> {
    /// Established a transport-level connection (e.g. a TCP socket) with the given peer.
    Connected(PeerId),

    /// A transport-level connection (e.g. a TCP socket) has been closed.
    Disconnected {
        peer_id: PeerId,
        user_data: TConn,
        out_overlay_network_indices: Vec<usize>,
        in_overlay_network_indices: Vec<usize>,
    },

    NotificationsOutAccept {
        id: ConnectionId,
        // TODO: what if fallback?
        overlay_network_index: usize,
        remote_handshake: Vec<u8>,
    },

    NotificationsOutReject {
        id: ConnectionId,
        // TODO: what if fallback?
        overlay_network_index: usize,
    },

    NotificationsOutClose {
        id: ConnectionId,
        overlay_network_index: usize,
    },

    ///
    NotificationsInOpen {
        id: ConnectionId,
        overlay_network_index: usize,
        remote_handshake: Vec<u8>,
    },

    ///
    NotificationsIn {
        id: ConnectionId,
        peer_id: PeerId, // TODO: is this field necessary? + cloning :-/
        /// `true` if there exists an open outbound substream with this peer on the same overlay
        /// network.
        has_symmetric_substream: bool,
        overlay_network_index: usize,
        notification: Vec<u8>,
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

/// Protocol error within the context of a connection. See [`Network::read_write`].
#[derive(Debug, derive_more::Display)]
pub enum ConnectionError {
    /// Protocol error after the connection has been established.
    Established(connection::established::Error),
    /// Protocol error during the handshake phase.
    Handshake(connection::handshake::HandshakeError),
    /// Mismatch between the actual [`PeerId`] and the [`PeerId`] expected by the local node.
    PeerIdMismatch,
}

pub struct SubstreamOpen<'a, TNow, TPeer, TConn> {
    network: &'a Network<TNow, TPeer, TConn>,
    connection_id: peerset::ConnectionId,
    connection: Arc<Mutex<Connection<TNow, TConn>>>,

    /// Index of the overlay network whose notifications substream to open.
    overlay_network_index: usize,
}

impl<'a, TNow, TPeer, TConn> SubstreamOpen<'a, TNow, TPeer, TConn>
where
    TNow: Clone + Add<Duration, Output = TNow> + Sub<TNow, Output = Duration> + Ord,
{
    /// Returns the index of the overlay network whose notifications substream to open.
    pub fn overlay_network_index(&self) -> usize {
        self.overlay_network_index
    }

    /// Perform the substream opening.
    ///
    /// No action is actually performed before this method is called.
    // TODO: futures cancellation concerns T_T
    pub async fn open(self, now: TNow, handshake: impl Into<Vec<u8>>) {
        let mut connection = self.connection.lock().await;

        let substream_id = if let Some(established) = connection.connection.as_mut() {
            Some(established.open_notifications_substream(
                now,
                self.overlay_network_index,
                handshake.into(),
                self.overlay_network_index,
            ))
        } else {
            None
        };

        if let Some(waker) = connection.waker.take() {
            waker.wake();
        }

        drop(connection);

        if let Some(substream_id) = substream_id {
            let mut guarded = self.network.guarded.lock().await;
            let mut peerset_entry = guarded.peerset.connection_mut(self.connection_id).unwrap();
            peerset_entry.add_pending_substream(
                self.overlay_network_index,
                peerset::SubstreamDirection::Out,
                substream_id,
            );
        }
    }
}

/// Error potentially returned by [`Network::request`].
#[derive(Debug, derive_more::Display)]
pub enum RequestError {
    /// Not connected to target.
    NotConnected,
    /// Connection has been unexpectedly closed by the remote during the request.
    ConnectionClosed,
    /// Error in the context of the connection.
    Connection(connection::established::RequestError),
}

/// Error potentially returned by [`Network::queue_notification`].
#[derive(Debug, derive_more::Display)]
pub enum QueueNotificationError {
    /// Not connected to target.
    NotConnected,
    /// No substream with the given target of the given protocol.
    NoSubstream,
    /// Queue of notifications with that peer is full.
    QueueFull,
}
