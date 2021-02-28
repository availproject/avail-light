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

// # Implementation notes
//
// This module is the synchronization point between all libp2p connections. It is the boundary
// between the "single-threaded world" (each individual libp2p connection uses exterior
// mutability) and the "multithreaded world" (the API of `Network` uses interior mutability).
// In other words, it is this module that provides a consistent view of all the connections as a
// whole, while trying to allow as many concurrent accesses as possible.
//
// As such, the code in this module is rather complex.
//
// The `Network` struct mainly consists of the following mutable components:
//
// - One instance of the `Guarded` struct, guarded by a `Mutex`, containing a view of the state
// of all connections.
// - For each active connection, one `Connection` struct, guarded by a `Mutex`, containing the
// state of this connection in particular.
// TODO: update with a `Pending` struct or something
//
// In order to avoid potential bugs and deadlocks, no single thread of execution must attempt
// to lock the `Mutex` protecting the `Guarded` then a `Mutex` guarding a connection at the same
// time without unlocking the `Guarded` first. The other way around, however, is authorized: one
// can lock the `Mutex` guarding a connection, then the `Mutex` protecting the `Guarded`.
//
// The view of the network within the `Guarded` is not necessarily always up-to-date with the
// actual state of the connections. For example, the `Guarded` might think that a certain
// substream on a particular connection is open while in reality it has already been closed.
//
// Connections hold a "pending event" field containing an event that has happened on this
// connection but hasn't been delivered to the `Guarded` yet. In other words, the `Guarded`
// doesn't yet take this event into account in its view.
// This field only holds space for a single event. The connection should never be updated as long
// as an event is present in this field, in order to avoid potentially generating a second event.
// Delivering this event to the `Guarded` is expected to be extremely quick, but in case it is no,
// connections should be back-pressured.
//
// This "pending event" field solves futures-cancellation-related problems. It is legal for the
// user to interrupt any operation at any `await` point without causing a state mismatch.
//
// With all the information above in mind, the flow of a typical operation consists in the
// following steps:
//
// - Lock `Guarded` and inspect the (potentially outdated) state of the network. Do not modify
// the state within `Guarded` that would require an update of a connection. Instead, we're going
// to modify the connection first, then the `Guarded` later.
// - Unlock `Guarded` then lock the desired connection object.
// - If there exists any "pending event" on that connection, lock the `Guarded` again and apply
// the "pending event" to the `Guarded`.
// - Inspect the state of the connection. If it is found to be inconsistent with the state earlier
// found in `Guarded`, either try again from the beginning or abort the operation. An
// inconsistency can only happen if an event has *just* happened, and considering that connections
// operate in parallel, there shouldn't be any meaningful difference between this event happening
// *just before* or *just after* the attempted operation.
// - Update the state of the connection and set the "pending event" field of that connection to
// match the modification that has just been performed.
// - Lock `Guarded` again, while keeping the connection locked.
// - Remove the "pending event" and apply it to the `Guarded`.
//
// Alternatively, `Guarded` can also be locked before updating the state of the connection. This
// skips the event phase and ensures more consistency, at the cost of keeping `Guarded` locked for
// a longer period of time.
//
// Between the moment when `Guarded` is unlocked and the moment when the connection object is
// locked, *anything* can happen. The connection can close, substream can be destroyed and
// recreated, and so on. Consequently, care must be taken to not have any ambiguity in whether or
// not the state found in `Guarded` actually matches the state found in the connection.
//
// For example, it is important that `SubstreamId`s are never reused. Otherwise, the following
// sequence of events could happen: `Guarded` is locked, a `SubstreamId` is found in `Guarded` and
// copied, `Guarded` is unlocked, the substream with that ID gets destroyed, a new substream that
// reuses the same ID is recreated, the connection is locked, the copied ID is used to reference
// the substream which is thought to be the old one but is actually the new one.
//

use alloc::{string::String, sync::Arc, vec::Vec};
use connection::established;
use core::{
    iter, mem,
    num::NonZeroUsize,
    ops::{Add, Sub},
    pin::Pin,
    task::{Context, Poll},
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

pub use established::{ConfigRequestResponse, ConfigRequestResponseIn};
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
pub struct PendingId(peerset::ConnectionId); // TODO: must never be reused /!\

/// Identifier of a connection spawned by the [`Network`].
#[derive(Debug, Copy, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ConnectionId(peerset::ConnectionId); // TODO: must never be reused /!\

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
        established::SubstreamId,
        established::SubstreamId,
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

    /// Returns the Noise key originalled passed as [`Config::noise_key`].
    pub fn noise_key(&self) -> &connection::NoiseKey {
        &self.noise_key
    }

    /// Returns the list the overlay networks originally passed as [`Config::overlay_networks`].
    pub fn overlay_networks(&self) -> impl ExactSizeIterator<Item = &OverlayNetwork> {
        self.overlay_networks.iter()
    }

    /// Returns the list the request-response protocols originally passed as
    /// [`Config::request_response_protocols`].
    pub fn request_response_protocols(
        &self,
    ) -> impl ExactSizeIterator<Item = &ConfigRequestResponse> {
        self.request_response_protocols.iter()
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

    /// Sends a request to the given peer, and waits for a response.
    ///
    /// This consists in:
    ///
    /// - Opening a substream on an established connection with the target.
    /// - Negotiating the requested protocol (`protocol_index`) on this substream using the
    ///   *multistream-select* protocol.
    /// - Sending the request (`request_data` parameter), prefixed with its length.
    /// - Waiting for the response (prefixed with its length), which is then returned.
    ///
    /// An error happens if there is no suitable connection for that request, if the connection
    /// closes while the request is in progress, if the request or response doesn't respect
    /// the protocol limits (see [`ConfigRequestResponse`]), or if the remote takes too much time
    /// to answer.
    ///
    /// As the API of this module is inherently subject to race conditions, it is never possible
    /// to guarantee that this function will succeed. [`RequestError::ConnectionClosed`] should
    /// be handled by retrying the same request again.
    ///
    /// > **Note**: This function doesn't return before the remote has answered. It is strongly
    /// >           recommended to await the returned `Future` in the background, and not block
    /// >           any important task on this.
    ///
    /// # Panic
    ///
    /// Panics if `protocol_index` isn't a valid index in [`Config::request_response_protocols`].
    ///
    pub async fn request(
        &self,
        now: TNow,
        target: PeerId,
        protocol_index: usize,
        request_data: Vec<u8>,
    ) -> Result<Vec<u8>, RequestError> {
        // Determine which connect to use to send the request.
        let connection_arc: Arc<Mutex<Connection<_, _>>> = {
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

        // The response to the request will be sent on this channel.
        // The sending side is stored as a "user data" alongside with the request in the
        // underlying data structure specific to the connection.
        let (send_back, receive_result) = oneshot::channel();

        // Lock to the connection. This waits for any other call to `request`,
        // `queue_notification` or `read_write` to finish.
        let mut connection_lock = connection_arc.lock().await;

        // Actually start the request by updating the underlying state machine specific to that
        // connection.
        connection_lock
            .connection
            .as_alive()
            .ok_or(RequestError::ConnectionClosed)?
            .add_request(now, protocol_index, request_data, send_back);

        // Note that no update of the `Guarded` is necessary. The `Guarded` doesn't track ongoing
        // requests.

        let waker = connection_lock.waker.take();

        // Make sure to unlock the connection before waiting for the result.
        drop(connection_lock);
        // The `Arc` to the connection should also be dropped, in order for everything to be
        // properly cleaned up if the connection closes. In particular, the channel on which
        // the response is sent back should be properly destroyed if the connection closes.
        drop(connection_arc);

        // Wake up the future returned by the latest call to `read_write` on that connection.
        if let Some(waker) = waker {
            let _ = waker.send(());
        }

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
        &self,
        target: &PeerId,
        protocol_index: usize,
        notification: impl Into<Vec<u8>>,
    ) -> Result<(), QueueNotificationError> {
        // Find which connection and substream to use to send the notification.
        // Only existing, established, substreams will be used.
        let (connection_arc, substream_id) = {
            let mut guarded = self.guarded.lock().await;

            // Choose which connection to use.
            // TODO: done in a dummy way ; choose properly
            let connection = match guarded.peerset.node_mut(target.clone()) {
                peerset::NodeMut::Known(n) => n
                    .connections()
                    .next()
                    .ok_or(QueueNotificationError::NotConnected)?,
                peerset::NodeMut::Unknown(_) => return Err(QueueNotificationError::NotConnected),
            };

            // Find a substream on this connection.
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

        // Lock to the connection. This waits for any other call to `request`,
        // `queue_notification` or `read_write` to finish.
        let mut connection_lock = connection_arc.lock().await;

        let waker = connection_lock.waker.take();

        // TODO: check if substream is still open to avoid a panic in `write_notification_unbounded`
        // TODO: return an error if queue is full
        connection_lock
            .connection
            .as_alive()
            .ok_or(QueueNotificationError::NotConnected)?
            .write_notification_unbounded(substream_id, notification.into());

        // Note that no update of the `Guarded` is necessary. The `Guarded` doesn't track
        // notifications being sent.

        // Wake up the future returned by the latest call to `read_write` on that connection.
        drop(connection_lock);
        if let Some(waker) = waker {
            let _ = waker.send(());
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

    pub async fn accept_notifications_in(
        &self,
        id: ConnectionId,
        overlay_network_index: usize,
        handshake: Vec<u8>,
    ) {
        // Find the connection and substream ID corresponding to the parameters.
        let (connection_arc, substream_id): (Arc<Mutex<Connection<_, _>>>, _) = {
            let mut guarded = self.guarded.lock().await;

            // TODO: we use defensive programming here because the concurrency model is still blurry
            let mut connection = match guarded.peerset.connection_mut(id.0) {
                Some(c) => c,
                None => return,
            };

            let substream_id = match connection.pending_substream_user_data_mut(
                overlay_network_index,
                peerset::SubstreamDirection::In,
            ) {
                Some(id) => *id,
                None => return, // TODO: too defensive
            };

            (connection.user_data_mut().clone(), substream_id)
        };

        let mut connection_lock = connection_arc.lock().await;

        // Because the user can cancel the future at any `await` point, all the asynchronous
        // operations are performed ahead of any state modification.
        let mut guarded = self.guarded.lock().await;

        // In order to guarantee a proper ordering of events, any pending event must first be
        // delivered.
        if connection_lock.pending_event.is_some() {
            connection_lock.propagate_pending_event(&mut guarded).await;
            debug_assert!(connection_lock.pending_event.is_none());
        }

        if let Some(connection) = connection_lock.connection.as_alive() {
            // TODO: must check if substream_id is still valid
            connection.accept_in_notifications_substream(
                substream_id,
                handshake,
                overlay_network_index,
            );
        } else {
            // The connection no longer exists. This state mismatch is a normal situation. See
            // the implementations notes at the top of the file for more information.
            return;
        }

        // Wake up the connection in order for the substream confirmation to be sent back to the
        // remote.
        if let Some(waker) = connection_lock.waker.take() {
            let _ = waker.send(());
        }

        // Rather than putting a value into `pending_event`, this function immediately updates
        // `Guarded`. If this was instead done through events, the user could observe that the
        // substream hasn't been confirmed yet for as long as the event hasn't been delivered.
        let mut peerset_entry = guarded.peerset.connection_mut(connection_lock.id).unwrap();
        peerset_entry
            .confirm_substream(
                overlay_network_index,
                peerset::SubstreamDirection::In,
                |id| id,
            )
            .unwrap();
    }

    /// Responds to an incoming request. Must be called in response to a [`Event::RequestIn`].
    ///
    /// Passing an `Err` corresponds, on the other side, to a
    /// [`established::RequestError::SubstreamClosed`].
    ///
    /// Has no effect if the connection has been closed in the meanwhile.
    pub async fn respond_in_request(
        &self,
        id: ConnectionId,
        substream_id: established::SubstreamId,
        response: Result<Vec<u8>, ()>,
    ) {
        // Find the connection corresponding to the parameter.
        let connection_arc: Arc<Mutex<Connection<_, _>>> = {
            let mut guarded = self.guarded.lock().await;

            match guarded.peerset.connection_mut(id.0) {
                Some(mut c) => c.user_data_mut().clone(),
                None => return,
            }
        };

        let mut connection_lock = connection_arc.lock().await;

        // In order to guarantee a proper ordering of events, any pending event must first be
        // delivered.
        if connection_lock.pending_event.is_some() {
            let mut guarded = self.guarded.lock().await;
            connection_lock.propagate_pending_event(&mut guarded).await;
            debug_assert!(connection_lock.pending_event.is_none());
        }

        if let Some(connection) = connection_lock.connection.as_alive() {
            // As explained in the documentation, ignore the error where the substream has
            // already been closed. This is a normal situation caused by the racy nature of the
            // API.
            match connection.respond_in_request(substream_id, response) {
                Ok(()) => {}
                Err(established::RespondInRequestError::SubstreamClosed) => {}
            }
        } else {
            // The connection no longer exists. This state mismatch is a normal situation. See
            // the implementations notes at the top of the file for more information.
            return;
        }

        // Wake up the connection in order for the substream confirmation to be sent back to the
        // remote.
        if let Some(waker) = connection_lock.waker.take() {
            let _ = waker.send(());
        }
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
    // TODO: futures cancellation concerns
    pub async fn read_write<'a>(
        &self,
        connection_id: ConnectionId,
        now: TNow,
        incoming_buffer: Option<&[u8]>,
        outgoing_buffer: (&'a mut [u8], &'a mut [u8]),
    ) -> Result<ReadWrite<TNow>, ConnectionError> {
        let (tx, rx) = oneshot::channel();

        let mut read_write = ReadWrite {
            read_bytes: 0,
            written_bytes: 0,
            wake_up_after: None,
            wake_up_future: ConnectionReadyFuture(rx),
            write_close: false,
        };

        // TODO: ideally we wouldn't need to lock `guarded`, to reduce the possibility of lock contention

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

                let mut tx = Some(tx);

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
                        if let Some(tx) = tx.take() {
                            let _ = tx.send(());
                        }
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
                                move |_| {
                                    let established = connection.into_connection(config);
                                    Arc::new(Mutex::new(Connection {
                                        connection: ConnectionInner::Alive(established),
                                        id: connection_id.0,
                                        user_data: Some(user_data),
                                        pending_event: None,
                                        waker: None,
                                    }))
                                }
                            });

                            // Send a `Connected` event if and only if this is the first active
                            // connection to that peer.
                            if guarded
                                .peerset
                                .node_mut(remote_peer_id.clone()) // TODO: clone :-/
                                .into_known()
                                .unwrap()
                                .connections()
                                .count()
                                == 1
                            {
                                // TODO: must convert this code to thread-safe design
                                guarded
                                    .events_tx
                                    .send(Event::Connected(remote_peer_id))
                                    .await
                                    .unwrap();
                            }

                            if let Some(tx) = tx.take() {
                                let _ = tx.send(());
                            }
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

                // Update the waker.
                established.waker = Some(tx);

                if established.pending_event.is_some() {
                    let mut guarded = self.guarded.lock().await;
                    established.propagate_pending_event(&mut guarded).await;
                    debug_assert!(established.pending_event.is_none());
                }

                established.read_write(now, incoming_buffer, outgoing_buffer, &mut read_write)?;
            }
        }

        Ok(read_write)
    }

    async fn build_connection_config(&self) -> established::Config {
        let randomness_seed = self.randomness_seeds.lock().await.gen();
        established::Config {
            notifications_protocols: self
                .overlay_networks
                .iter()
                .flat_map(|net| {
                    let max_handshake_size = net.max_handshake_size;
                    let max_notification_size = net.max_notification_size;
                    iter::once(&net.protocol_name)
                        .chain(net.fallback_protocol_names.iter())
                        .map(move |name| {
                            established::ConfigNotifications {
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
                    expected_peer_id: node.peer_id().clone(),
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
    /// State machine of the underlying connection.
    connection: ConnectionInner<TNow>,

    /// Copy of the id of the connection.
    id: peerset::ConnectionId,

    /// User data decided by the user for that connection.
    user_data: Option<TConn>,

    /// Event that has just happened on the connection, but that the [`Guarded`] isn't yet aware
    /// of. See the implementations note at the top of the file for more information.
    pending_event: Option<PendingEvent>,

    /// A sender is stored here when the user call [`Network::read_write`]. The receiving part
    /// notifies the user that they must call [`Network::read_write`] again.
    /// Send a value on that channel in order to notify that data is potentially available to be
    /// sent on the socket, or that the user should call [`Network::read_write`] in general.
    waker: Option<oneshot::Sender<()>>,
}

enum ConnectionInner<TNow> {
    Alive(established::Established<TNow, oneshot::Sender<Result<Vec<u8>, RequestError>>, usize>),
    Errored(ConnectionError),
    Dead,
    Poisoned,
}

impl<TNow> ConnectionInner<TNow> {
    fn as_alive(
        &mut self,
    ) -> Option<
        &mut established::Established<TNow, oneshot::Sender<Result<Vec<u8>, RequestError>>, usize>,
    > {
        if let ConnectionInner::Alive(c) = self {
            Some(c)
        } else {
            None
        }
    }
}

enum PendingEvent {
    Inner(established::Event<oneshot::Sender<Result<Vec<u8>, RequestError>>, usize>),
    Disconnect,
}

impl<TNow, TConn> Connection<TNow, TConn>
where
    TNow: Clone + Add<Duration, Output = TNow> + Sub<TNow, Output = Duration> + Ord,
{
    fn read_write<'a>(
        &mut self,
        now: TNow,
        incoming_buffer: Option<&[u8]>,
        outgoing_buffer: (&'a mut [u8], &'a mut [u8]),
        read_write: &mut ReadWrite<TNow>,
    ) -> Result<(), ConnectionError> {
        let connection = match mem::replace(&mut self.connection, ConnectionInner::Poisoned) {
            ConnectionInner::Alive(c) => c,
            ConnectionInner::Errored(err) => return Err(err),
            ConnectionInner::Dead => panic!(),
            ConnectionInner::Poisoned => unreachable!(),
        };

        match connection.read_write(now, incoming_buffer, outgoing_buffer) {
            Ok(read_write_result) => {
                read_write.read_bytes += read_write_result.read_bytes;
                read_write.written_bytes += read_write_result.written_bytes;
                debug_assert!(read_write.wake_up_after.is_none());
                read_write.wake_up_after = read_write_result.wake_up_after;
                read_write.write_close = read_write_result.write_close;

                if read_write.write_close && incoming_buffer.is_none() {
                    self.connection = ConnectionInner::Dead;
                } else {
                    self.connection = ConnectionInner::Alive(read_write_result.connection);
                }

                if read_write_result.read_bytes != 0
                    || read_write_result.written_bytes != 0
                    || read_write_result.event.is_some()
                {
                    if let Some(waker) = self.waker.take() {
                        let _ = waker.send(());
                    }
                }

                if let Some(event) = read_write_result.event {
                    debug_assert!(self.pending_event.is_none());
                    self.pending_event = Some(PendingEvent::Inner(event));
                }
            }
            Err(err) => {
                if let Some(waker) = self.waker.take() {
                    let _ = waker.send(());
                }

                self.connection = ConnectionInner::Errored(ConnectionError::Established(err));
                self.pending_event = Some(PendingEvent::Disconnect);
            }
        };

        Ok(())
    }

    /// Removes the pending event stored within that connection and updates the [`Guarded`]
    /// accordingly.
    /// See the implementations notes at the top of the file for more information.
    async fn propagate_pending_event<TPeer>(&mut self, guarded: &mut Guarded<TNow, TPeer, TConn>) {
        if self.pending_event.is_none() {
            return;
        }

        // The body of this function consists in two operations: extracting the event from
        // `pending_event`, then sending a corresponding event on `events_tx`. Because sending an
        // item on `events_tx` might block, and because the user is free to cancel the future at
        // it heir will, it is possible for the event to have been extracted then get lost into
        // oblivion.
        //
        // To prevent this from happening, we first wait for `events_tx` to be ready to accept
        // an item, then use `try_send` to send items on it in a non-blocking way.
        future::poll_fn(|cx| guarded.events_tx.poll_ready(cx))
            .await
            .unwrap();

        // The line below does `pending_event.take()`. After this, no more `await` must be present
        // in the function's body without precautionnary measures.
        match self.pending_event.take().unwrap() {
            PendingEvent::Inner(established::Event::RequestIn {
                id: substream_id,
                protocol_index,
                request,
            }) => {
                let peer_id = guarded
                    .peerset
                    .connection_mut(self.id)
                    .unwrap()
                    .peer_id()
                    .clone();

                guarded
                    .events_tx
                    .try_send(Event::RequestIn {
                        id: ConnectionId(self.id),
                        substream_id,
                        peer_id,
                        protocol_index,
                        request_payload: request,
                    })
                    .unwrap();
            }
            PendingEvent::Inner(established::Event::Response {
                response,
                user_data: send_back,
                ..
            }) => {
                let _ = send_back.send(response.map_err(RequestError::Connection));
            }
            PendingEvent::Inner(established::Event::NotificationsInOpen {
                id,
                protocol_index: overlay_network_index,
                handshake,
            }) => {
                guarded
                    .peerset
                    .connection_mut(self.id)
                    .unwrap()
                    .add_pending_substream(
                        overlay_network_index,
                        peerset::SubstreamDirection::In,
                        id,
                    )
                    .unwrap();

                guarded
                    .events_tx
                    .try_send(Event::NotificationsInOpen {
                        id: ConnectionId(self.id),
                        overlay_network_index,
                        remote_handshake: handshake,
                    })
                    .unwrap();
            }
            PendingEvent::Inner(established::Event::NotificationsInOpenCancel {
                protocol_index,
                ..
            }) => {
                guarded
                    .peerset
                    .connection_mut(self.id)
                    .unwrap()
                    .remove_pending_substream(protocol_index, peerset::SubstreamDirection::In)
                    .unwrap();
            }
            PendingEvent::Inner(established::Event::NotificationIn { id, notification }) => {
                let overlay_network_index = *self
                    .connection
                    .as_alive()
                    .unwrap() // TODO: shouldn't unwrap here
                    .notifications_substream_user_data_mut(id)
                    .unwrap();

                let mut connection = guarded.peerset.connection_mut(self.id).unwrap();
                let peer_id = connection.peer_id().clone();
                let has_symmetric_substream = connection
                    .has_open_substream(overlay_network_index, peerset::SubstreamDirection::Out);

                guarded
                    .events_tx
                    .try_send(Event::NotificationsIn {
                        id: ConnectionId(self.id),
                        peer_id,
                        has_symmetric_substream,
                        overlay_network_index,
                        notification,
                    })
                    .unwrap();
            }
            PendingEvent::Inner(established::Event::NotificationsOutAccept {
                id,
                remote_handshake,
            }) => {
                let overlay_network_index = *self
                    .connection
                    .as_alive()
                    .unwrap() // TODO: shouldn't unwrap here
                    .notifications_substream_user_data_mut(id)
                    .unwrap();

                let mut connection = guarded.peerset.connection_mut(self.id).unwrap();
                let peer_id = connection.peer_id().clone();
                connection
                    .confirm_substream(
                        overlay_network_index,
                        peerset::SubstreamDirection::Out,
                        |id| id,
                    )
                    .unwrap();

                guarded
                    .events_tx
                    .try_send(Event::NotificationsOutAccept {
                        id: ConnectionId(self.id),
                        peer_id,
                        overlay_network_index,
                        remote_handshake,
                    })
                    .unwrap();
            }
            PendingEvent::Inner(established::Event::NotificationsOutReject {
                id,
                user_data: overlay_network_index,
            }) => {
                let mut connection = guarded.peerset.connection_mut(self.id).unwrap();
                let peer_id = connection.peer_id().clone();
                let _expected_id = connection
                    .remove_pending_substream(
                        overlay_network_index,
                        peerset::SubstreamDirection::Out,
                    )
                    .unwrap();
                debug_assert_eq!(id, _expected_id);

                guarded
                    .events_tx
                    .try_send(Event::NotificationsOutReject {
                        id: ConnectionId(self.id),
                        peer_id,
                        overlay_network_index,
                    })
                    .unwrap();
            }
            PendingEvent::Inner(established::Event::NotificationsOutCloseDemanded { id }) => {
                todo!()
            }
            PendingEvent::Inner(established::Event::NotificationsOutReset {
                id,
                user_data: overlay_network_index,
            }) => {
                let mut connection = guarded.peerset.connection_mut(self.id).unwrap();
                let peer_id = connection.peer_id().clone();
                let _expected_id = connection
                    .remove_substream(overlay_network_index, peerset::SubstreamDirection::Out)
                    .unwrap();
                debug_assert_eq!(id, _expected_id);

                guarded
                    .events_tx
                    .try_send(Event::NotificationsOutClose {
                        id: ConnectionId(self.id),
                        overlay_network_index,
                        peer_id,
                    })
                    .unwrap();
            }
            PendingEvent::Disconnect => {
                let mut out_overlay_network_indices =
                    Vec::with_capacity(guarded.peerset.num_overlay_networks());
                let mut in_overlay_network_indices =
                    Vec::with_capacity(guarded.peerset.num_overlay_networks());
                let peer_id = {
                    let mut c = guarded.peerset.connection_mut(self.id).unwrap();
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

                // Send a `Disconnected` event if and only if there is no other active connection
                // to that node.
                // TODO: clone :-/
                if !guarded
                    .peerset
                    .node_mut(peer_id.clone())
                    .into_known()
                    .map(|n| n.connections().count() >= 1)
                    .unwrap_or(false)
                {
                    guarded
                        .events_tx
                        .try_send(Event::Disconnected {
                            peer_id,
                            user_data: self.user_data.take().unwrap(),
                            in_overlay_network_indices,
                            out_overlay_network_indices,
                        })
                        .unwrap();
                }
            }
        }
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
    /// [`PeerId`] that is expected to be reached with this connection attempt.
    pub expected_peer_id: PeerId,
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

    /// Received a request from a request-response protocol.
    RequestIn {
        id: ConnectionId,
        /// Substream on which the request has been received. Must be passed back when providing
        /// the response.
        substream_id: established::SubstreamId,
        peer_id: PeerId,
        protocol_index: usize,
        request_payload: Vec<u8>,
    },

    NotificationsOutAccept {
        id: ConnectionId,
        peer_id: PeerId,
        // TODO: what if fallback?
        overlay_network_index: usize,
        remote_handshake: Vec<u8>,
    },

    NotificationsOutReject {
        id: ConnectionId,
        peer_id: PeerId,
        // TODO: what if fallback?
        overlay_network_index: usize,
    },

    NotificationsOutClose {
        id: ConnectionId,
        peer_id: PeerId,
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

    /// [`Network::read_write`] should be called again when this [`ConnectionReadyFuture`]
    /// returns `Ready`.
    pub wake_up_future: ConnectionReadyFuture,

    /// If `true`, the writing side the connection must be closed. Will always remain to `true`
    /// after it has been set.
    ///
    /// If, after calling [`Network::read_write`], the returned [`ReadWrite`] contains `true` here,
    /// and the inbound buffer is `None`, then the [`ConnectionId`] is now invalid.
    pub write_close: bool,
}

/// Future ready when a connection has data to give out via [`Network::read_write`].
#[pin_project::pin_project]
pub struct ConnectionReadyFuture(#[pin] oneshot::Receiver<()>);

impl Future for ConnectionReadyFuture {
    type Output = ();

    fn poll(self: Pin<&mut Self>, cx: &mut Context) -> Poll<()> {
        // `Err(Canceled)` is mapped to `Poll::Pending`, meaning that the `oneshot::Sender` can
        // be dropped in order to never notify this future.
        match self.project().0.poll(cx) {
            Poll::Pending => Poll::Pending,
            Poll::Ready(Ok(())) => Poll::Ready(()),
            Poll::Ready(Err(_)) => Poll::Pending,
        }
    }
}

/// Protocol error within the context of a connection. See [`Network::read_write`].
#[derive(Debug, derive_more::Display)]
pub enum ConnectionError {
    /// Protocol error after the connection has been established.
    #[display(fmt = "{}", _0)]
    Established(established::Error),
    /// Protocol error during the handshake phase.
    #[display(fmt = "{}", _0)]
    Handshake(connection::handshake::HandshakeError),
    /// Mismatch between the actual [`PeerId`] and the [`PeerId`] expected by the local node.
    #[display(fmt = "Mismatch between the actual PeerId and PeerId expected by the local node")]
    PeerIdMismatch,
}

pub struct SubstreamOpen<'a, TNow, TPeer, TConn> {
    network: &'a Network<TNow, TPeer, TConn>,
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
    pub async fn open(self, now: TNow, handshake: impl Into<Vec<u8>>) {
        let mut connection = self.connection.lock().await;

        // Because the user can cancel the future at any `await` point, all the asynchronous
        // operations are performed ahead of any state modification.
        let mut guarded = self.network.guarded.lock().await;

        // In order to guarantee a proper ordering of events, any pending event must first be
        // delivered.
        if connection.pending_event.is_some() {
            connection.propagate_pending_event(&mut guarded).await;
            debug_assert!(connection.pending_event.is_none());
        }

        // TODO: connection_id might be wrong and not correspond to the right connection
        let substream_id = if let Some(established) = connection.connection.as_alive() {
            established.open_notifications_substream(
                now,
                self.overlay_network_index,
                handshake.into(),
                self.overlay_network_index,
            )
        } else {
            // The connection no longer exists. This state mismatch is a normal situation. See
            // the implementations notes at the top of the file for more information.
            return;
        };

        // Rather than putting a value into `pending_event`, this function immediately updates
        // `Guarded`. If this was instead done through events, the user could observe that no
        // substream is being opened as long as the event hasn't been delivered.

        // While updating `Guarded`, it is assumed that there isn't any existing outgoing pending
        // substream on that overlay index. Hence the `unwrap`. The existence of an instance
        // of `SubstreamOpen` implies that there is indeed no such pending outgoing substream.
        let mut peerset_entry = guarded.peerset.connection_mut(connection.id).unwrap();
        peerset_entry
            .add_pending_substream(
                self.overlay_network_index,
                peerset::SubstreamDirection::Out,
                substream_id,
            )
            .unwrap();

        // Wake up the task dedicated to this connection in order for the substream to start
        // opening.
        if let Some(waker) = connection.waker.take() {
            let _ = waker.send(());
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
    Connection(established::RequestError),
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
