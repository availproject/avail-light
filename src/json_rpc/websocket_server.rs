//! WebSocket server.

// TODO: docs

#![cfg(feature = "os-networking")]
#![cfg_attr(docsrs, doc(cfg(feature = "os-networking")))]

#[cfg(test)]
mod tests;

use async_std::net::{TcpListener, TcpStream};
use core::{fmt, pin::Pin, str};
use futures::{channel::mpsc, prelude::*};
use soketto::handshake::{server::Response, Server};
use std::{io, net::SocketAddr};

/// Configuration for a [`WsServer`].
pub struct Config {
    /// IP address to try to bind to.
    pub bind_address: SocketAddr,

    /// Maximum size, in bytes, of a frame sent by the remote.
    ///
    /// Since the messages are entirely buffered before being returned, a maximum value is
    /// necessary in order to prevent malicious clients from sending huge frames that would
    /// occupy a lot of memory.
    pub max_frame_size: usize,

    /// Number of pending messages to buffer up for sending before the socket is considered
    /// unresponsive.
    pub send_buffer_len: usize,

    /// When an incoming connection is rejected, it is added to a queue for clean shut down. This
    /// configures the maximum size of this queue.
    pub max_clean_rejected_sockets_shutdowns: usize,

    /// Pre-allocated capacity for the list of connections.
    pub capacity: usize,
}

/// WebSockets listening socket and list of open connections.
pub struct WsServer<T> {
    /// Value passed through [`Config::max_frame_size`].
    max_frame_size: usize,

    /// Value passed through [`Config::send_buffer_len`].
    send_buffer_len: usize,

    /// Value passed through [`Config::max_clean_rejected_sockets_shutdowns`].
    max_clean_rejected_sockets_shutdowns: usize,

    /// Endpoint for incoming TCP sockets.
    listener: TcpListener,

    /// List of TCP connections that are currently negotiating the WebSocket handshake.
    ///
    /// The output can be an error if the handshake fails.
    negotiating: stream::FuturesUnordered<
        Pin<
            Box<
                dyn Future<Output = (ConnectionId, u64, Result<Server<'static, TcpStream>, ()>)>
                    + Send,
            >,
        >,
    >,

    /// List of streams of incoming messages for all connections.
    incoming_messages: stream::SelectAll<
        Pin<Box<dyn Stream<Item = (ConnectionId, u64, Result<String, ()>)> + Send>>,
    >,

    /// Tasks dedicated to sending messages on connections. One per healthy connection.
    sending_tasks:
        stream::FuturesUnordered<Pin<Box<dyn Future<Output = (ConnectionId, u64)> + Send>>>,

    /// List of connections that are either negotiating or open.
    connections: slab::Slab<Connection<T>>,

    /// Value of [`Connection::unique_id`] for the next connection.
    next_unique_id: u64,

    /// Tasks dedicated to closing sockets that have been rejected.
    rejected_sockets: stream::FuturesUnordered<Pin<Box<dyn Future<Output = ()> + Send>>>,
}

struct Connection<T> {
    user_data: T,

    /// Sending side of [`Connection::send_rx`].
    /// Can be `None` in order to force-close a connection.
    send_tx: Option<mpsc::Sender<String>>,

    /// Receiving side of the buffer of messages pending to be sent.
    /// Once the handshake of a connection has been performed, this receiver is extracted (`None`
    /// is left) and processed in the background.
    send_rx: Option<mpsc::Receiver<String>>,

    /// Because [`ConnectionId`]s are reused, we need to make sure that received packets don't
    /// correspond to old connections with the same ID. For this reason, we additionally compare
    /// the expected unique ID with the actual one.
    unique_id: u64,
}

impl<T> WsServer<T> {
    /// Try opening a TCP listening socket.
    ///
    /// Returns an error if the listening socket fails to open.
    pub async fn new(config: Config) -> Result<Self, io::Error> {
        let listener = TcpListener::bind(config.bind_address).await?;

        Ok(WsServer {
            max_frame_size: config.max_frame_size,
            send_buffer_len: config.send_buffer_len,
            max_clean_rejected_sockets_shutdowns: config.max_clean_rejected_sockets_shutdowns,
            listener,
            negotiating: stream::FuturesUnordered::new(),
            incoming_messages: stream::SelectAll::new(),
            sending_tasks: stream::FuturesUnordered::new(),
            connections: slab::Slab::with_capacity(config.capacity),
            next_unique_id: 0,
            rejected_sockets: stream::FuturesUnordered::new(),
        })
    }

    /// Address of the local TCP listening socket, as provided by the operating system.
    pub fn local_addr(&self) -> Result<SocketAddr, io::Error> {
        self.listener.local_addr()
    }

    /// Destroys a connection.
    ///
    /// The connection will be cleanly shut down in the background, but for API purposes this
    /// [`ConnectionId`] is now no longer valid.
    ///
    /// # Panic
    ///
    /// Panics if the [`ConnectionId`] is invalid.
    pub fn close(&mut self, connection_id: ConnectionId) -> T {
        self.connections.remove(connection_id.0).user_data
    }

    /// Queues a text frame to be sent on the given connection.
    ///
    /// If more than [`Config::send_buffer_len`] messages are already buffered, the message is
    /// silently discarded and a [`Event::ConnectionError`] will soon be generated for this
    /// connection.
    ///
    /// # Panic
    ///
    /// Panics if the [`ConnectionId`] is invalid.
    pub fn queue_send(&mut self, connection: ConnectionId, message: String) {
        if let Some(send_tx) = self.connections[connection.0].send_tx.as_mut() {
            if send_tx.try_send(message).is_err() {
                self.connections[connection.0].send_tx = None;
            }
        }
    }

    /// Returns the next event happening on the server.
    pub async fn next_event<'a>(&'a mut self) -> Event<'a, T> {
        loop {
            futures::select! {
                socket = self.listener.accept().fuse() => {
                    let (socket, address) = match socket {
                        Ok(s) => s,
                        Err(_) => continue,
                    };
                    let open = ConnectionOpenEvent {
                        server: self,
                        socket: Some(socket),
                    };
                    return Event::ConnectionOpen { open, address };
                },

                (connection_id, unique_id, result) = self.negotiating.select_next_some() => {
                    // Make sure that what is in `self.connections` matches the outcome of the
                    // negotiation. Otherwise, it means that the connection is already closed.
                    if !self.connections.contains(connection_id.0) {
                        continue;
                    }
                    if self.connections[connection_id.0].unique_id != unique_id {
                        continue;
                    }

                    let server = match result {
                        Ok(s) => s,
                        Err(()) => return Event::ConnectionError {
                            connection_id,
                            user_data: self.connections.remove(connection_id.0).user_data,
                        },
                    };

                    let (mut sender, receiver) = {
                        let mut builder = server.into_builder();
                        builder.set_max_frame_size(self.max_frame_size);
                        builder.set_max_message_size(self.max_frame_size);
                        builder.finish()
                    };

                    // Spawn a task dedicated to receiving messages from the socket.
                    self.incoming_messages.push({
                        // Turn `receiver` into a stream of received packets.
                        let socket_packets = stream::unfold((receiver, Vec::new()), move |(mut receiver, mut buf)| async {
                            let ret = match receiver.receive_data(&mut buf).await {
                                Ok(soketto::Data::Text(len)) => Ok(str::from_utf8(&buf[..len]).unwrap().to_owned()),
                                _ => Err(())
                            };
                            Some((ret, (receiver, buf)))
                        });

                        Box::pin(socket_packets.map(move |msg| (connection_id, unique_id, msg)))
                    });

                    // Spawn a task dedicated to sending the messages buffered to be sent.
                    self.sending_tasks.push({
                        let mut send_rx = self.connections[connection_id.0].send_rx.take().unwrap();
                        Box::pin(async move {
                            while let Some(message) = send_rx.next().await {
                                match sender.send_text(&message).await {
                                    Ok(()) => {}
                                    Err(_) => break,
                                }
                            }

                            let _ = sender.close().await;
                            (connection_id, unique_id)
                        })
                    });
                },

                (connection_id, unique_id, result) = self.incoming_messages.select_next_some() => {
                    // Make sure that what is in `self.connections` matches the message. Otherwise,
                    // it means that the connection is already closed.
                    if !self.connections.contains(connection_id.0) {
                        continue;
                    }
                    if self.connections[connection_id.0].unique_id != unique_id {
                        continue;
                    }

                    let message = match result {
                        Ok(m) => m,
                        Err(()) => return Event::ConnectionError {
                            connection_id,
                            user_data: self.connections.remove(connection_id.0).user_data,
                        },
                    };

                    return Event::TextFrame {
                        connection_id,
                        user_data: &mut self.connections[connection_id.0].user_data,
                        message,
                    }
                },

                (connection_id, unique_id) = self.sending_tasks.select_next_some() => {
                    // Make sure that what is in `self.connections` matches the message. Otherwise,
                    // it means that the connection is already closed.
                    if !self.connections.contains(connection_id.0) {
                        continue;
                    }
                    if self.connections[connection_id.0].unique_id != unique_id {
                        continue;
                    }

                    return Event::ConnectionError {
                        connection_id,
                        user_data: self.connections.remove(connection_id.0).user_data,
                    }
                },

                _ = self.rejected_sockets.select_next_some() => {
                }
            }
        }
    }
}

impl<T: fmt::Debug> fmt::Debug for WsServer<T> {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.debug_list()
            .entries(
                self.connections
                    .iter()
                    .map(|c| (ConnectionId(c.0), &c.1.user_data)),
            )
            .finish()
    }
}

/// Event that has happened on a [`WsServer`].
#[derive(Debug)]
pub enum Event<'a, T> {
    /// A new TCP connection has arrived on the listening socket.
    ///
    /// The connection *must* be accepted using the provided [`ConnectionOpenEvent`], otherwise
    /// it will be silently dropped.
    ConnectionOpen {
        /// Object used to accept the incoming connection.
        open: ConnectionOpenEvent<'a, T>,
        /// Address of the remote, as provided by the operating system.
        address: SocketAddr,
    },

    /// An error has happened on a connection. The connection is now closed and its
    /// [`ConnectionId`] is now invalid.
    ConnectionError {
        /// Identifier of the connection. This identifier might be reused by the [`WsServer`] for
        /// another connection.
        connection_id: ConnectionId,
        /// User data associated with the connection.
        user_data: T,
    },

    /// A text frame has been received on a connection.
    TextFrame {
        /// Identifier of the connection that sent the frame.
        connection_id: ConnectionId,
        /// User data associated with the connection.
        user_data: &'a mut T,
        /// Message sent by the remote. Its content is entirely decided by the client, and
        /// nothing must be assumed about the validity of this message.
        message: String,
    },
}

/// Identifier for a connection with regard to a [`WsServer`].
///
/// After a connection has closed, its [`ConnectionId`]s might be reused.
#[derive(Debug, Copy, Clone, Ord, PartialOrd, Eq, PartialEq, Hash)]
pub struct ConnectionId(usize);

/// Object returned when a new connection arrives. The connection must be accepted by calling
/// [`ConnectionOpenEvent::accept`].
#[must_use]
pub struct ConnectionOpenEvent<'a, T> {
    server: &'a mut WsServer<T>,

    /// Socket to accept. Always `Some`, except after `accept` has been called.
    socket: Option<TcpStream>,
}

impl<'a, T> ConnectionOpenEvent<'a, T> {
    /// Accept the incoming connection. Associates the passed user data with it.
    pub fn accept(mut self, user_data: T) -> ConnectionId {
        let unique_id = {
            let id = self.server.next_unique_id;
            self.server.next_unique_id += 1;
            id
        };

        let connection_id = ConnectionId(self.server.connections.insert({
            let (send_tx, send_rx) = mpsc::channel(self.server.send_buffer_len);
            Connection {
                user_data,
                send_tx: Some(send_tx),
                send_rx: Some(send_rx),
                unique_id,
            }
        }));

        let socket = self.socket.take().unwrap();
        self.server.negotiating.push(Box::pin(async move {
            let mut server = Server::new(socket);

            let websocket_key = match server.receive_request().await {
                Ok(req) => req.into_key(),
                Err(_) => return (connection_id, unique_id, Err(())),
            };

            match server
                .send_response(&{
                    Response::Accept {
                        key: &websocket_key,
                        protocol: None,
                    }
                })
                .await
            {
                Ok(()) => {}
                Err(_) => return (connection_id, unique_id, Err(())),
            };

            (connection_id, unique_id, Ok(server))
        }));

        connection_id
    }
}

impl<'a, T> Drop for ConnectionOpenEvent<'a, T> {
    fn drop(&mut self) {
        if self.server.rejected_sockets.len() >= self.server.max_clean_rejected_sockets_shutdowns {
            return;
        }

        if let Some(mut socket) = self.socket.take() {
            self.server.rejected_sockets.push(Box::pin(async move {
                let _ = socket.close().await;
            }));
        }
    }
}

impl<'a, T> fmt::Debug for ConnectionOpenEvent<'a, T> {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.debug_tuple("ConnectionOpenEvent").finish()
    }
}
