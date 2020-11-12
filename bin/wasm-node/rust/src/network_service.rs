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

use core::{pin::Pin, time::Duration};
use futures::{
    channel::{mpsc, oneshot},
    lock::{Mutex, MutexGuard},
    prelude::*,
};
use std::sync::Arc;
use substrate_lite::network::{
    connection,
    multiaddr::{Multiaddr, Protocol},
    peer_id::PeerId,
    peerset, protocol,
};

/// Configuration for a [`NetworkService`].
pub struct Config {
    /// Closure that spawns background tasks.
    pub tasks_executor: Box<dyn FnMut(Pin<Box<dyn Future<Output = ()> + Send>>) + Send>,

    /// List of node identities and addresses that are known to belong to the chain's peer-to-pee
    /// network.
    pub bootstrap_nodes: Vec<(PeerId, Multiaddr)>,

    /// Identifier of the chain to connect to.
    ///
    /// Each blockchain has (or should have) a different "protocol id". This value identifies the
    /// chain, so as to not introduce conflicts in the networking messages.
    pub protocol_id: String,
}

pub struct NetworkService {
    /// Fields behind a mutex.
    guarded: Mutex<Guarded>,

    /// See [`Config::protocol_id`].
    protocol_id: String,

    /// Key used for the encryption layer.
    /// This is a Noise static key, according to the Noise specifications.
    /// Signed using the actual libp2p key.
    noise_key: Arc<connection::NoiseKey>,

    /// Receiver of events sent by background tasks.
    ///
    /// > **Note**: This field is not in [`Guarded`] despite being inside of a mutex. The mutex
    /// >           around this receiver is kept locked while an event is being waited for, and it
    /// >           would be undesirable to block access to the other fields of [`Guarded`] during
    /// >           that time.
    from_background: Mutex<mpsc::Receiver<FromBackground>>,

    /// Sending side of [`NetworkService::from_background`]. Clones of this field are created when
    /// a background task is spawned.
    to_foreground: mpsc::Sender<FromBackground>,
}

/// Fields of [`NetworkService`] behind a mutex.
struct Guarded {
    /// See [`Config::tasks_executor`].
    tasks_executor: Box<dyn FnMut(Pin<Box<dyn Future<Output = ()> + Send>>) + Send>,

    /// Holds the state of all the known nodes of the network, and of all the connections (pending
    /// or not).
    peerset: peerset::Peerset<(), mpsc::Sender<ToConnection>, mpsc::Sender<ToConnection>, (), ()>,
}

impl NetworkService {
    /// Initializes the network service with the given configuration.
    pub async fn new(config: Config) -> Result<Arc<Self>, InitError> {
        // Channel used for the background to communicate to the foreground.
        // Once this channel is full, background tasks that need to send a message to the network
        // service will block and wait for some space to be available.
        //
        // The ideal size of this channel depends on the volume of messages, the time it takes for
        // the network service to be polled after being waken up, and the speed of messages
        // processing. All these components are pretty hard to know in advance, and as such we go
        // for the approach of choosing an arbitrary constant value.
        let (to_foreground, from_background) = mpsc::channel(32);

        // The peerset, created below, is a data structure that helps keep track of the state of
        // the current peers and connections.
        let mut peerset = peerset::Peerset::new(peerset::Config {
            randomness_seed: rand::random(),
            peers_capacity: 50,
            num_overlay_networks: 1,
        });

        // Add to overlay #0 the nodes known to belong to the network.
        for (peer_id, address) in config.bootstrap_nodes {
            let mut node = peerset.node_mut(peer_id).or_default();
            node.add_known_address(address);
            node.add_to_overlay(0);
        }

        Ok(Arc::new(NetworkService {
            guarded: Mutex::new(Guarded {
                tasks_executor: config.tasks_executor,
                peerset,
            }),
            protocol_id: config.protocol_id,
            noise_key: Arc::new(connection::NoiseKey::new(&rand::random())),
            from_background: Mutex::new(from_background),
            to_foreground,
        }))
    }

    /// Sends a blocks request to the given peer.
    // TODO: more docs
    // TODO: proper error type
    pub async fn blocks_request(
        self: Arc<Self>,
        target: PeerId,
        config: protocol::BlocksRequestConfig,
    ) -> Result<Vec<protocol::BlockData>, ()> {
        let mut guarded = self.guarded.lock().await;

        let connection = match guarded.peerset.node_mut(target) {
            peerset::NodeMut::Known(n) => n.connections().next().ok_or(())?,
            peerset::NodeMut::Unknown(_) => return Err(()),
        };

        let (send_back, receive_result) = oneshot::channel();

        let protocol = format!("/{}/sync/2", self.protocol_id);

        // TODO: is awaiting here a good idea? if the background task is stuck, we block the entire `Guarded`
        // It is possible for the channel to be closed, if the background task has ended but the
        // frontend hasn't processed this yet.
        guarded
            .peerset
            .connection_mut(connection)
            .unwrap()
            .into_user_data()
            .send(ToConnection::BlocksRequest {
                config,
                protocol,
                send_back,
            })
            .await
            .map_err(|_| ())?;

        // Everything must be unlocked at this point.
        drop(guarded);

        // Wait for the result of the request. Can take a long time (i.e. several seconds).
        match receive_result.await {
            Ok(r) => r,
            Err(_) => Err(()),
        }
    }

    /// Returns the next event that happens in the network service.
    ///
    /// If this method is called multiple times simultaneously, the events will be distributed
    /// amongst the different calls in an unpredictable way.
    pub async fn next_event(&self) -> Event {
        loop {
            self.fill_out_slots(&mut self.guarded.lock().await).await;

            match self.from_background.lock().await.next().await.unwrap() {
                FromBackground::HandshakeError { connection_id, .. } => {
                    let mut guarded = self.guarded.lock().await;
                    guarded.peerset.pending_mut(connection_id).unwrap().remove();
                }
                FromBackground::HandshakeSuccess {
                    connection_id,
                    peer_id,
                    accept_tx,
                } => {
                    let mut guarded = self.guarded.lock().await;
                    let id = guarded
                        .peerset
                        .pending_mut(connection_id)
                        .unwrap()
                        .into_established(|tx| tx)
                        .id();
                    accept_tx.send(id).unwrap();
                    return Event::Connected(peer_id);
                }
                FromBackground::Disconnected { connection_id } => {
                    let mut guarded = self.guarded.lock().await;
                    let connection = guarded.peerset.connection_mut(connection_id).unwrap();
                    let peer_id = connection.peer_id().clone(); // TODO: clone :(
                    connection.remove();
                    return Event::Disconnected(peer_id);
                }
                FromBackground::NotificationsOpenResult { .. } => todo!(),
                FromBackground::NotificationsCloseResult { .. } => todo!(),

                FromBackground::NotificationsOpenDesired { .. } => todo!(),

                FromBackground::NotificationsCloseDesired { .. } => todo!(),
            }
        }
    }

    /// Spawns new outgoing connections in order to fill empty outgoing slots.
    ///
    /// Must be passed as parameter an existing lock to a [`Guarded`].
    async fn fill_out_slots<'a>(&self, guarded: &mut MutexGuard<'a, Guarded>) {
        // Solves borrow checking errors regarding the borrow of multiple different fields at the
        // same time.
        let guarded = &mut **guarded;

        let mut num = 0;

        // TODO: very wip
        while let Some(mut node) = guarded.peerset.random_not_connected(0) {
            num += 1;
            if num >= 15 {
                break;
            }

            // TODO: collecting into a Vec, annoying
            for address in node.known_addresses().cloned().collect::<Vec<_>>() {
                let websocket = match multiaddr_to_url(&address) {
                    Ok(s) => ffi::WebSocket::connect(&s),
                    Err(()) => {
                        node.remove_known_address(&address).unwrap();
                        continue;
                    }
                };

                let (tx, rx) = mpsc::channel(8);
                let connection_id = node.add_outbound_attempt(address.clone(), tx);
                (guarded.tasks_executor)(Box::pin(connection_task(
                    websocket,
                    true,
                    self.noise_key.clone(),
                    connection_id,
                    self.to_foreground.clone(),
                    rx,
                )));
            }
        }
    }
}

/// Event that can happen on the network service.
pub enum Event {
    Connected(PeerId),
    Disconnected(PeerId),
}

/// Error when initializing the network service.
#[derive(Debug, derive_more::Display)]
pub enum InitError {
    // TODO: add variants or remove error altogether
}

/// Message sent to a background task dedicated to a connection.
enum ToConnection {
    /// Start a block request. See [`NetworkService::blocks_request`].
    BlocksRequest {
        config: protocol::BlocksRequestConfig,
        protocol: String,
        send_back: oneshot::Sender<Result<Vec<protocol::BlockData>, ()>>,
    },
    OpenNotifications,
    CloseNotifications,
}

/// Messsage sent from a background task and dedicated to the main [`NetworkService`]. Processed
/// in [`NetworkService::next_event`].
enum FromBackground {
    HandshakeError {
        connection_id: peerset::PendingId,
        error: HandshakeError,
    },
    HandshakeSuccess {
        connection_id: peerset::PendingId,
        peer_id: PeerId,
        accept_tx: oneshot::Sender<peerset::ConnectionId>,
    },

    /// Connection has closed.
    ///
    /// This only concerns connections onto which the handshake had succeeded. For connections on
    /// which the handshake hadn't succeeded, a [`FromBackground::HandshakeError`] is emitted
    /// instead.
    Disconnected {
        connection_id: peerset::ConnectionId,
    },

    /// Response to a [`ToConnection::OpenNotifications`].
    NotificationsOpenResult {
        connection_id: peerset::ConnectionId,
        /// Outcome of the opening. If `Ok`, the notifications protocol is now open. If `Err`, it
        /// is still closed.
        result: Result<(), ()>,
    },

    /// Response to a [`ToConnection::CloseNotifications`].
    ///
    /// Contrary to [`FromBackground::NotificationsOpenResult`], a closing request never fails.
    NotificationsCloseResult {
        connection_id: peerset::ConnectionId,
    },

    /// The remote requests that a notification substream be opened.
    ///
    /// No action has been taken. Send [`ToConnection::OpenNotifications`] to open the substream,
    /// or [`ToConnection::CloseNotifications`] to reject the request from the remote.
    NotificationsOpenDesired {
        connection_id: peerset::ConnectionId,
    },

    /// The remote requests that a notification substream be closed.
    ///
    /// No action has been taken. Send [`ToConnection::CloseNotifications`] in order to close the
    /// substream.
    ///
    /// If this follows a [`FromBackground::NotificationsOpenDesired`], it cancels it.
    NotificationsCloseDesired {
        connection_id: peerset::ConnectionId,
    },
}

/// Asynchronous task managing a specific WebSocket connection.
async fn connection_task(
    websocket: impl Future<Output = Result<Pin<Box<ffi::WebSocket>>, ()>>,
    is_initiator: bool,
    noise_key: Arc<connection::NoiseKey>,
    connection_id: peerset::PendingId,
    mut to_foreground: mpsc::Sender<FromBackground>,
    mut to_connection: mpsc::Receiver<ToConnection>,
) {
    // Finishing any ongoing connection process.
    let mut websocket = match websocket.await {
        Ok(s) => s,
        Err(_) => {
            let _ = to_foreground
                .send(FromBackground::HandshakeError {
                    connection_id,
                    error: HandshakeError::Io,
                })
                .await;
            return;
        }
    };

    // Connections start with a handshake where the encryption and multiplexing protocols are
    // negotiated.
    let (connection_prototype, peer_id) =
        match perform_handshake(&mut websocket, &noise_key, is_initiator).await {
            Ok(v) => v,
            Err(error) => {
                let _ = to_foreground
                    .send(FromBackground::HandshakeError {
                        connection_id,
                        error,
                    })
                    .await;
                return;
            }
        };

    // Configure the `connection_prototype` to turn it into an actual connection.
    // The protocol names are hardcoded here.
    let mut connection = connection_prototype.into_connection::<_, oneshot::Sender<_>, ()>(
        connection::established::Config {
            in_request_protocols: vec![],
            in_notifications_protocols: vec![connection::established::ConfigNotifications {
                name: "/dot/block-announces/1".to_string(), // TODO: correct protocolId
                max_handshake_size: 1024 * 1024,
            }],
            ping_protocol: "/ipfs/ping/1.0.0".to_string(),
            randomness_seed: rand::random(),
        },
    );

    // Notify the outside of the transition from handshake to actual connection, and obtain an
    // updated `connection_id` in return.
    // It is possible for the outside to refuse the connection after the handshake (if e.g. the
    // `PeerId` isn't the one that is expected), in which case the task stops entirely.
    let connection_id = {
        let (accept_tx, accept_rx) = oneshot::channel();

        if to_foreground
            .send(FromBackground::HandshakeSuccess {
                connection_id,
                peer_id,
                accept_tx,
            })
            .await
            .is_err()
        {
            return;
        }

        match accept_rx.await {
            Ok(id) => id,
            Err(_) => return,
        }
    };

    // Set to a timer after which the state machine of the connection needs an update.
    let mut poll_after: ffi::Delay;

    let mut write_buffer = vec![0; 4096];

    loop {
        let read_buffer = websocket.read_buffer().now_or_never().unwrap_or(Some(&[]));

        let now = ffi::Instant::now();

        let read_write = match connection.read_write(now, read_buffer, (&mut write_buffer, &mut []))
        {
            Ok(rw) => rw,
            Err(_) => {
                let _ = to_foreground
                    .send(FromBackground::Disconnected { connection_id })
                    .await;
                return;
            }
        };
        connection = read_write.connection;

        if let Some(wake_up) = read_write.wake_up_after {
            if wake_up > now {
                let dur = wake_up - now;
                poll_after = ffi::Delay::new(dur);
            } else {
                poll_after = ffi::Delay::new(Duration::from_secs(0));
            }
        } else {
            poll_after = ffi::Delay::new(Duration::from_secs(3600));
        }

        websocket.send(&write_buffer[..read_write.written_bytes]);

        websocket.advance_read_cursor(read_write.read_bytes);

        let has_event = read_write.event.is_some();

        match read_write.event {
            Some(connection::established::Event::Response {
                response,
                user_data,
                ..
            }) => {
                if let Ok(response) = response {
                    let decoded = protocol::decode_block_response(&response).unwrap();
                    let _ = user_data.send(Ok(decoded));
                } else {
                    let _ = user_data.send(Err(()));
                }
                continue;
            }
            _ => {}
        }

        if has_event || read_write.read_bytes != 0 || read_write.written_bytes != 0 {
            continue;
        }

        // TODO: maybe optimize the code below so that multiple messages are pulled from `to_connection` at once

        futures::select! {
            _ = websocket.read_buffer().fuse() => {},
            timeout = (&mut poll_after).fuse() => { // TODO: no, ref mut + fuse() = probably panic
                // Nothing to do, but guarantees that we loop again.
            },
            message = to_connection.select_next_some().fuse() => {
                match message {
                    ToConnection::BlocksRequest { config, protocol, send_back } => {
                        let request = protocol::build_block_request(config)
                            .fold(Vec::new(), |mut a, b| {
                                a.extend_from_slice(b.as_ref());
                                a
                            });
                        connection.add_request(ffi::Instant::now(), protocol, request, send_back);
                    }
                    ToConnection::OpenNotifications => {
                        // TODO: finish
                        let id = connection.open_notifications_substream(
                            ffi::Instant::now(),
                            "/dot/block-announces/1".to_string(),  // TODO: should contain correct ProtocolId
                            Vec::new(), // TODO:
                            ()
                        );
                        todo!()
                    },
                    ToConnection::CloseNotifications => {
                        todo!()
                    },
                }
            }
        }
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

/// Drives the handshake of the given connection.
///
/// # Panic
///
/// Panics if the `websocket` is closed in the writing direction.
///
async fn perform_handshake(
    websocket: &mut Pin<Box<ffi::WebSocket>>,
    noise_key: &connection::NoiseKey,
    is_initiator: bool,
) -> Result<(connection::established::ConnectionPrototype, PeerId), HandshakeError> {
    let mut handshake = connection::handshake::Handshake::new(is_initiator);

    // Delay that triggers after we consider the remote is considered unresponsive.
    // The constant here has been chosen arbitrary.
    let timeout = ffi::Delay::new(Duration::from_secs(20));
    futures::pin_mut!(timeout);

    let mut write_buffer = vec![0; 4096];

    loop {
        match handshake {
            connection::handshake::Handshake::Success {
                remote_peer_id,
                connection,
            } => {
                break Ok((connection, remote_peer_id));
            }
            connection::handshake::Handshake::NoiseKeyRequired(key) => {
                handshake = key.resume(noise_key).into()
            }
            connection::handshake::Handshake::Healthy(healthy) => {
                // Update the handshake state machine with the received data, and writes in the
                // write buffer.
                let (new_state, num_read, num_written) = {
                    let read_buffer = websocket
                        .read_buffer()
                        .now_or_never()
                        .unwrap_or(Some(&[]))
                        .ok_or(HandshakeError::UnexpectedEof)?;
                    healthy.read_write(read_buffer, (&mut write_buffer, &mut []))?
                };
                handshake = new_state;
                websocket.send(&write_buffer[..num_written]);
                websocket.advance_read_cursor(num_read);

                if num_read != 0 || num_written != 0 {
                    continue;
                }

                // Wait either for something to happen on the socket, or for the timeout to
                // trigger.
                {
                    let process_future = websocket.read_buffer();
                    futures::pin_mut!(process_future);
                    match future::select(process_future, &mut timeout).await {
                        future::Either::Left(_) => {}
                        future::Either::Right(_) => return Err(HandshakeError::Timeout),
                    }
                }
            }
        }
    }
}

#[derive(Debug, derive_more::Display, derive_more::From)]
enum HandshakeError {
    Io,
    Timeout,
    UnexpectedEof,
    Protocol(connection::handshake::HandshakeError),
}
