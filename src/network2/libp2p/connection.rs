//! State machine handling a single TCP or WebSocket libp2p connection.
//!
//! This state machine tries to negotiate and apply the noise and yamux protocols on top of the
//! connection.

use core::iter;
use libp2p::PeerId;

pub use noise::{NoiseKey, UnsignedNoiseKey};

mod multiplexed_connection;
mod multistream_select;
mod noise;
mod yamux;

#[derive(derive_more::From)]
pub enum Connection {
    Healthy(Healthy),
    /// Connection handshake has reached the noise handshake, and it is necessary to know the
    /// noise key in order to proceed.
    NoiseKeyRequired(NoiseKeyRequired),
}

impl Connection {
    /// Shortcut for [`Healthy::new`] wrapped in a [`Connection`].
    pub fn new(endpoint: Endpoint) -> Self {
        Healthy::new(endpoint).into()
    }
}

pub struct Healthy {
    endpoint: Endpoint,
    state: State,
}

enum State {
    NegotiatingEncryptionProtocol {
        negotiation: multistream_select::InProgress<iter::Once<&'static str>, &'static str>,
    },
    NegotiatingEncryption {
        handshake: noise::HandshakeInProgress,
    },
    NegotiatingMultiplexing {
        peer_id: PeerId,
        encryption: noise::Noise,
        negotiation: multistream_select::InProgress<iter::Once<&'static str>, &'static str>,
    },
    Open {
        peer_id: PeerId,
        encryption: noise::Noise,
    },
}

impl Healthy {
    /// Initializes a new state machine.
    ///
    /// Must pass [`Endpoint::Dialer`] if the connection has been opened by the local machine,
    /// and [`Endpoint::Listener`] if it has been opened by the remote.
    pub fn new(endpoint: Endpoint) -> Self {
        let negotiation = multistream_select::InProgress::new(match endpoint {
            Endpoint::Dialer => multistream_select::Config::Dialer {
                requested_protocol: "/noise",
            },
            Endpoint::Listener => multistream_select::Config::Listener {
                supported_protocols: iter::once("/noise"),
            },
        });

        Healthy {
            endpoint,
            state: State::NegotiatingEncryptionProtocol { negotiation },
        }
    }

    /// Parse the content of `data`. Returns the new state of the connection and the number of
    /// bytes read from `data`.
    ///
    /// Returns an error in case of protocol error, in which case the connection should be
    /// entirely shut down.
    ///
    /// If the number of bytes read is different from 0, you should immediately call this method
    /// again with the remaining data.
    pub fn inject_data<'c, 'd>(mut self, mut data: &[u8]) -> Result<(Connection, usize), Error> {
        let mut total_read = 0;

        // Handle the case where the connection is still negotiating the encryption protocol.
        if let State::NegotiatingEncryptionProtocol { negotiation } = self.state {
            let (updated, num_read) = negotiation
                .inject_data(data)
                .map_err(Error::MultistreamSelect)?;
            total_read += num_read;
            data = &data[num_read..];

            match updated {
                multistream_select::Negotiation::InProgress(updated) => {
                    self.state = State::NegotiatingEncryptionProtocol {
                        negotiation: updated,
                    };
                }
                multistream_select::Negotiation::Success(_) => {
                    return Ok((
                        Connection::NoiseKeyRequired(NoiseKeyRequired {
                            endpoint: self.endpoint,
                        }),
                        total_read,
                    ));
                }
                multistream_select::Negotiation::NotAvailable => todo!(),
            }

            return Ok((Connection::Healthy(self), total_read));
        }

        // Handle the case where the connection is still performing the encryption handshake.
        if let State::NegotiatingEncryption { handshake } = self.state {
            let (updated, num_read) = handshake.inject_data(data).map_err(Error::NoiseHandshake)?;
            total_read += num_read;
            data = &data[num_read..];

            match updated {
                noise::NoiseHandshake::Success {
                    cipher,
                    remote_peer_id,
                } => todo!(),
                noise::NoiseHandshake::InProgress(updated) => {
                    self.state = State::NegotiatingEncryption { handshake: updated }
                }
            };

            return Ok((Connection::Healthy(self), total_read));
        }

        // Inject the received data into the cipher for decryption.
        match self.state {
            State::NegotiatingEncryptionProtocol { .. } => unreachable!(),
            State::NegotiatingEncryption { .. } => unreachable!(),
            State::NegotiatingMultiplexing { mut encryption, .. }
            | State::Open { mut encryption, .. } => {
                encryption.inject_inbound_data(data);
            }
        };

        todo!()
    }

    /// Write to the given buffer the bytes that are ready to be sent out. Returns the number of
    /// bytes written to `destination`.
    pub fn write_out(mut self, mut destination: &mut [u8]) -> (Connection, usize) {
        let mut total_written = 0;

        loop {
            match self.state {
                State::NegotiatingEncryptionProtocol { negotiation } => {
                    let (updated, written) = negotiation.write_out(destination);
                    total_written += written;
                    destination = &mut destination[written..];

                    match updated {
                        multistream_select::Negotiation::InProgress(updated) => {
                            self.state = State::NegotiatingEncryptionProtocol {
                                negotiation: updated,
                            };
                            if written == 0 {
                                break;
                            }
                        }
                        multistream_select::Negotiation::Success(_) => {
                            return (
                                Connection::NoiseKeyRequired(NoiseKeyRequired {
                                    endpoint: self.endpoint,
                                }),
                                total_written,
                            );
                        }
                        multistream_select::Negotiation::NotAvailable => todo!(), // TODO:
                    };

                    if written == 0 {
                        break;
                    }
                }
                State::NegotiatingEncryption { mut handshake } => {
                    let (updated, written) = handshake.write_out(destination);
                    total_written += written;
                    destination = &mut destination[written..];

                    match updated {
                        noise::NoiseHandshake::Success {
                            cipher,
                            remote_peer_id,
                        } => todo!(),
                        noise::NoiseHandshake::InProgress(updated) => {
                            self.state = State::NegotiatingEncryption { handshake: updated }
                        }
                    };

                    if written == 0 {
                        break;
                    }
                }
                _ => todo!(),
            }
        }

        (Connection::Healthy(self), total_written)
    }
}

pub struct NoiseKeyRequired {
    endpoint: Endpoint,
}

impl NoiseKeyRequired {
    /// Turn this [`NoiseKeyRequired`] back into a [`Healthy`] by indicating the noise key.
    pub fn resume(self, noise_key: &NoiseKey) -> Healthy {
        Healthy {
            endpoint: self.endpoint,
            state: State::NegotiatingEncryption {
                handshake: noise::HandshakeInProgress::new(
                    noise_key,
                    matches!(self.endpoint, Endpoint::Dialer),
                ),
            },
        }
    }
}

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub enum Endpoint {
    Dialer,
    Listener,
}

#[derive(Debug, PartialEq, Eq)]
pub enum InjectDataOutcome<'c, 'd> {
    ReceivedIdentity(PeerId),
    /// Received a ping request from the remote and answered it.
    Ping,
    /// Received a pong from the remote.
    Pong,
    RequestIn {
        protocol_name: &'c str,
        request: &'d [u8],
    },
}

/// Protocol error on a connection. The connection should be shut down.
#[derive(Debug, derive_more::Display)]
pub enum Error {
    MultistreamSelect(multistream_select::Error),
    NoiseHandshake(noise::HandshakeError),
}
