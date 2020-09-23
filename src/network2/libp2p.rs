// Copyright (C) 2019-2020 Parity Technologies (UK) Ltd.
// SPDX-License-Identifier: Apache-2.0

// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
// 	http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

#![allow(unused)] // TODO: remove once code is used

// TODO: this module uses the AsyncRead/AsyncWrite traits, which are unfortunately not no_std friendly

use alloc::{collections::VecDeque, sync::Arc};
use core::{fmt, future::Future, iter, marker::PhantomData, pin::Pin, task::Poll};
use futures::{
    channel::{mpsc, oneshot},
    prelude::*,
};
use hashbrown::HashMap;
use rand::{seq::IteratorRandom as _, SeedableRng as _};

pub use connection::{NoiseKey, UnsignedNoiseKey};
pub use libp2p::{multiaddr, Multiaddr, PeerId};

mod connection;

/// Configuration to provide when building a [`Network`].
pub struct Config<PIter> {
    /// Key to use during the connection handshakes. Not the same thing as the libp2p key, but
    /// instead contains a signature made using the libp2p private key.
    pub noise_key: NoiseKey,

    /// List of notification protocols.
    pub notification_protocols: PIter,

    /// List of protocol names for request-response protocols.
    pub request_protocols: Vec<String>,

    /// Executor to spawn background tasks.
    pub tasks_executor: Box<dyn Fn(Pin<Box<dyn Future<Output = ()> + Send>>) + Send>,
}

/// Collection of network connections.
pub struct Network<TUserData, TDialFut, TSocket, TRq, TProtocol> {
    nodes: HashMap<PeerId, NodeInner<TUserData, TSocket>, fnv::FnvBuildHasher>,

    /// See [`Config::noise_key`]. Wrapped in an `Arc` for sharing between all the background
    /// tasks.
    noise_key: Arc<NoiseKey>,

    /// Stream of dialing attempts.
    dials: stream::FuturesUnordered<TDialFut>,

    /// How to start a background task.
    tasks_executor: Box<dyn Fn(Pin<Box<dyn Future<Output = ()> + Send>>) + Send>,

    test_tx: mpsc::Sender<()>,
    test_rx: mpsc::Receiver<()>,

    protocols: Vec<TProtocol>,
    rq: PhantomData<TRq>,
}

struct NodeInner<TUserData, TSocket> {
    dialing: Vec<(Multiaddr, PeerId, oneshot::Receiver<TSocket>)>,

    /// User data stored for this node, or `None` if the node is reported as not-connected on
    /// the API.
    user_data: Option<TUserData>,
}

impl<TUserData, TDialFut, TDialError, TSocket, TRq, TProtocol>
    Network<TUserData, TDialFut, TSocket, TRq, TProtocol>
where
    TDialFut: Future<Output = Result<(TUserData, TSocket), TDialError>>,
    TSocket: AsyncRead + AsyncWrite + Send + 'static,
{
    /// Initializes the [`Network`].
    pub fn new(config: Config<impl Iterator<Item = TProtocol>>) -> Self {
        let (test_tx, test_rx) = mpsc::channel(512);

        Network {
            nodes: HashMap::default(),
            noise_key: Arc::new(config.noise_key),
            dials: stream::FuturesUnordered::new(),
            tasks_executor: config.tasks_executor,
            test_tx,
            test_rx,
            protocols: config.notification_protocols.collect(),
            rq: PhantomData,
        }
    }

    /// Returns the state of a certain node in the libp2p state machine.
    pub fn node_mut(
        &mut self,
        peer_id: &PeerId,
    ) -> Node<TUserData, TDialFut, TSocket, TRq, TProtocol> {
        todo!()
    }

    pub fn add_dial_future(&mut self, future: TDialFut) {
        self.dials.push(future);
    }

    pub fn inject_inbound_connection(
        &mut self,
        user_data: TUserData,
        socket: TSocket,
        target: Multiaddr,
    ) {
        self.inject_connection(user_data, socket, connection::Endpoint::Listener)
    }

    fn inject_connection(
        &mut self,
        user_data: TUserData,
        mut socket: TSocket,
        endpoint: connection::Endpoint,
    ) {
        // Create a state machine handling the traffic from/to that connection.
        let mut connection = Some(connection::HealthyHandshake::new(endpoint));

        // `noise_key` being an `Arc`, this cloning is cheap.
        let noise_key = self.noise_key.clone();

        // Spawn a background task dedicated to handling this connection.
        (self.tasks_executor)(
            async move {
                println!("connected"); // TODO: remove

                // Wrap the socket in a `BufReader`, mostly to provide a buffer rather than having
                // to do the buffering manually.
                let socket = futures::io::BufReader::new(socket); // TODO: buffer size configurable
                futures::pin_mut!(socket);

                // Buffer containing data to write to the socket.
                let mut write_buffer = vec![0; 4096].into_boxed_slice(); // TODO: 4096 configurable
                let mut write_buffer_cursor = 0;
                let mut write_buffer_filled = 0;
                let mut flush_needed = false;

                // TODO: remove explicit generic
                future::poll_fn::<(), _>(move |cx| {
                    loop {
                        let mut all_pending = true;

                        if let Poll::Ready(result) = socket.as_mut().poll_fill_buf(cx) {
                            let buffer = result.unwrap(); // TODO:
                            if buffer.is_empty() {
                                panic!(); // TODO: this indicates EOF, I believe
                            }

                            println!("recv buffer: {:?}", buffer); // TODO: remove
                            let (new_state, read_num) =
                                connection.take().unwrap().inject_data(buffer).unwrap(); // TODO: don't unwrap
                            socket.as_mut().consume(read_num);
                            connection = Some(match new_state {
                                connection::Handshake::Healthy(h) => h,
                                connection::Handshake::NoiseKeyRequired(req) => {
                                    req.resume(&noise_key)
                                }
                                connection::Handshake::Success { remote_peer_id, .. } => {
                                    todo!("{}", remote_peer_id)
                                }
                            });
                            all_pending = false;
                        }

                        debug_assert!(write_buffer_cursor <= write_buffer_filled);
                        if write_buffer_cursor < write_buffer_filled {
                            match socket.as_mut().poll_write(
                                cx,
                                &mut write_buffer[write_buffer_cursor..write_buffer_filled],
                            ) {
                                Poll::Ready(Ok(n)) => {
                                    write_buffer_cursor += n;
                                    debug_assert!(write_buffer_cursor <= write_buffer_filled);
                                    flush_needed = true;
                                    all_pending = false;
                                }
                                Poll::Ready(Err(_)) => todo!(),
                                Poll::Pending => {}
                            }
                        } else {
                            debug_assert!(write_buffer_cursor == write_buffer_filled);
                            let (new_state, num_written) =
                                connection.take().unwrap().write_out(&mut write_buffer);
                            write_buffer_cursor = 0;
                            write_buffer_filled = num_written;

                            connection = Some(match new_state {
                                connection::Handshake::Healthy(h) => h,
                                connection::Handshake::NoiseKeyRequired(req) => {
                                    all_pending = false;
                                    req.resume(&noise_key)
                                }
                                connection::Handshake::Success { remote_peer_id, .. } => {
                                    todo!("{}", remote_peer_id)
                                }
                            });

                            if num_written != 0 {
                                println!(
                                    "writing out: {:?}",
                                    &write_buffer[write_buffer_cursor..write_buffer_filled]
                                ); // TODO: remove
                                all_pending = false;
                            }
                        }

                        if write_buffer_cursor < write_buffer_filled && flush_needed {
                            if let Poll::Ready(result) = socket.as_mut().poll_flush(cx) {
                                flush_needed = false;
                                all_pending = false;
                                let () = result.unwrap(); // TODO:
                            }
                        }

                        if all_pending {
                            return Poll::Pending;
                        }
                    }
                })
                .await;
            }
            .boxed(),
        );
    }

    ///
    pub async fn write_notification(&mut self, target: &PeerId, protocol_name: &str) {
        todo!()
    }

    pub async fn next_event<'a>(&'a mut self) -> Event<'a, TUserData, TProtocol> {
        loop {
            futures::select! {
                dial_result = self.dials.select_next_some() => {
                    match dial_result {
                        Ok((user_data, socket)) => {
                            self.inject_connection(user_data, socket, connection::Endpoint::Dialer);
                        },
                        Err(_) => todo!(),
                    }
                },
                _ = self.test_rx.select_next_some() => {
                    todo!()
                },
            }
        }
    }
}

/// Event that happened on the [`Network`].
#[derive(Debug)]
pub enum Event<'a, TUserData, TProtocol> {
    RequestFinished {
        peer: &'a mut TProtocol,
    },
    Notification {
        user_data: &'a mut TUserData,
        protocol: &'a mut TProtocol,
        payload: NotificationPayload<'a>,
    },
}

/// Notification data.
pub struct NotificationPayload<'a> {
    foo: &'a mut (),
}

impl<'a> AsRef<[u8]> for NotificationPayload<'a> {
    fn as_ref(&self) -> &[u8] {
        todo!()
    }
}

impl<'a> fmt::Debug for NotificationPayload<'a> {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.debug_tuple("NotificationPayload").finish()
    }
}

#[derive(Debug, derive_more::Display)]
pub enum StartRequestError {
    NotConnected,
}

/// State of a node in the libp2p state machine.
pub enum Node<'a, TUserData, TDialFut, TSocket, TRq, TProtocol> {
    /// There isn't any healthy connect to this node.
    NotConnected(NodeNotConnected<'a, TUserData, TDialFut, TSocket, TRq, TProtocol>),
    /// There exists at least one healthy connection to this node.
    Connected(NodeConnected<'a, TUserData, TDialFut, TSocket, TRq, TProtocol>),
}

/// State of a node in the libp2p state machine.
pub struct NodeNotConnected<'a, TUserData, TDialFut, TSocket, TRq, TProtocol> {
    network: &'a mut Network<TUserData, TDialFut, TSocket, TRq, TProtocol>,
    peer_id: &'a PeerId,
}

impl<'a, T, TDialFut, TSocket, TRq, TProtocol>
    NodeNotConnected<'a, T, TDialFut, TSocket, TRq, TProtocol>
{
    pub fn is_dialing(&self) -> bool {
        todo!()
    }
}

/// State of a node in the libp2p state machine.
pub struct NodeConnected<'a, TUserData, TDialFut, TSocket, TRq, TProtocol> {
    network: &'a mut Network<TUserData, TDialFut, TSocket, TRq, TProtocol>,
    peer_id: &'a PeerId,
}

impl<'a, T, TDialFut, TSocket, TRq, TProtocol>
    NodeConnected<'a, T, TDialFut, TSocket, TRq, TProtocol>
{
    pub fn user_data(&mut self) -> &mut T {
        todo!()
    }

    pub fn into_user_data(self) -> &'a mut T {
        todo!()
    }

    /// Start disconnecting all active connections to this peer. For API-related purposes, the
    /// node is now considered disconnected.
    pub fn close(
        self,
    ) -> (
        T,
        NodeNotConnected<'a, T, TDialFut, TSocket, TRq, TProtocol>,
    ) {
        todo!()
    }
}
