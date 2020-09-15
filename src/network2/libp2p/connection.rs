//! State machine handling a single TCP or WebSocket libp2p connection.
//!
//! This state machine tries to negotiate and apply the noise and yamux protocols on top of the
//! connection.

use core::iter;
use libp2p::PeerId;

pub use noise::{NoiseKey, UnsignedNoiseKey};

mod multistream_select;
mod noise;
mod yamux;

// TODO: needs a timeout for the handshake

#[derive(derive_more::From)]
pub enum Handshake {
    Healthy(HealthyHandshake),
    /// Connection handshake has reached the noise handshake, and it is necessary to know the
    /// noise key in order to proceed.
    NoiseKeyRequired(NoiseKeyRequired),
    Success {
        remote_peer_id: PeerId,
        // TODO: some sort of `Connection` object
        encryption: noise::Noise,
    },
}

impl Handshake {
    /// Shortcut for [`HealthyHandshake::new`] wrapped in a [`Connection`].
    pub fn new(endpoint: Endpoint) -> Self {
        HealthyHandshake::new(endpoint).into()
    }
}

pub struct HealthyHandshake {
    endpoint: Endpoint,
    state: HandshakeState,
}

enum HandshakeState {
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
}

impl HealthyHandshake {
    /// Initializes a new state machine.
    ///
    /// Must pass [`Endpoint::Dialer`] if the connection has been opened by the local machine,
    /// and [`Endpoint::Listener`] if it has been opened by the remote.
    pub fn new(endpoint: Endpoint) -> Self {
        let negotiation = multistream_select::InProgress::new(match endpoint {
            Endpoint::Dialer => multistream_select::Config::Dialer {
                requested_protocol: noise::PROTOCOL_NAME,
            },
            Endpoint::Listener => multistream_select::Config::Listener {
                supported_protocols: iter::once(noise::PROTOCOL_NAME),
            },
        });

        HealthyHandshake {
            endpoint,
            state: HandshakeState::NegotiatingEncryptionProtocol { negotiation },
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
    pub fn inject_data<'c, 'd>(mut self, mut data: &[u8]) -> Result<(Handshake, usize), Error> {
        let mut total_read = 0;

        match self.state {
            HandshakeState::NegotiatingEncryptionProtocol { negotiation } => {
                let (updated, num_read) = negotiation
                    .inject_data(data)
                    .map_err(Error::MultistreamSelect)?;
                total_read += num_read;
                data = &data[num_read..];

                match updated {
                    multistream_select::Negotiation::InProgress(updated) => {
                        self.state = HandshakeState::NegotiatingEncryptionProtocol {
                            negotiation: updated,
                        };
                    }
                    multistream_select::Negotiation::Success(_) => {
                        return Ok((
                            Handshake::NoiseKeyRequired(NoiseKeyRequired {
                                endpoint: self.endpoint,
                            }),
                            total_read,
                        ));
                    }
                    multistream_select::Negotiation::NotAvailable => {
                        return Err(Error::NoEncryptionProtocol)
                    }
                }

                Ok((Handshake::Healthy(self), total_read))
            }

            HandshakeState::NegotiatingEncryption { handshake } => {
                let (updated, num_read) =
                    handshake.inject_data(data).map_err(Error::NoiseHandshake)?;
                total_read += num_read;
                data = &data[num_read..];

                match updated {
                    noise::NoiseHandshake::Success {
                        cipher,
                        remote_peer_id,
                    } => {
                        let negotiation =
                            multistream_select::InProgress::new(match self.endpoint {
                                Endpoint::Dialer => multistream_select::Config::Dialer {
                                    requested_protocol: yamux::PROTOCOL_NAME,
                                },
                                Endpoint::Listener => multistream_select::Config::Listener {
                                    supported_protocols: iter::once(yamux::PROTOCOL_NAME),
                                },
                            });

                        self.state = HandshakeState::NegotiatingMultiplexing {
                            peer_id: remote_peer_id,
                            encryption: cipher,
                            negotiation,
                        };
                    }
                    noise::NoiseHandshake::InProgress(updated) => {
                        self.state = HandshakeState::NegotiatingEncryption { handshake: updated }
                    }
                };

                Ok((Handshake::Healthy(self), total_read))
            }

            HandshakeState::NegotiatingMultiplexing {
                negotiation,
                mut encryption,
                peer_id,
            } => {
                let num_read = encryption.inject_inbound_data(data).map_err(Error::Noise)?;
                total_read += data.len();
                data = &data[num_read..];

                let (updated, read_num) = negotiation
                    .inject_data(encryption.decoded_inbound_data())
                    .map_err(Error::MultistreamSelect)?;
                encryption.consume_inbound_data(read_num);

                match updated {
                    multistream_select::Negotiation::InProgress(updated) => {
                        self.state = HandshakeState::NegotiatingMultiplexing {
                            negotiation: updated,
                            encryption,
                            peer_id,
                        };
                    }
                    multistream_select::Negotiation::Success(_) => {
                        return Ok((
                            Handshake::Success {
                                encryption,
                                remote_peer_id: peer_id,
                            },
                            total_read,
                        ));
                    }
                    multistream_select::Negotiation::NotAvailable => {
                        return Err(Error::NoMultiplexingProtocol)
                    }
                }

                Ok((Handshake::Healthy(self), total_read))
            }
        }
    }

    /// Write to the given buffer the bytes that are ready to be sent out. Returns the number of
    /// bytes written to `destination`.
    pub fn write_out(mut self, mut destination: &mut [u8]) -> (Handshake, usize) {
        let mut total_written = 0;

        loop {
            match self.state {
                HandshakeState::NegotiatingEncryptionProtocol { negotiation } => {
                    let (updated, written) = negotiation.write_out(destination);
                    total_written += written;
                    destination = &mut destination[written..];

                    match updated {
                        multistream_select::Negotiation::InProgress(updated) => {
                            self.state = HandshakeState::NegotiatingEncryptionProtocol {
                                negotiation: updated,
                            };
                            if written == 0 {
                                break;
                            }
                        }
                        multistream_select::Negotiation::Success(_) => {
                            return (
                                Handshake::NoiseKeyRequired(NoiseKeyRequired {
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
                HandshakeState::NegotiatingEncryption { mut handshake } => {
                    let (updated, written) = handshake.write_out(destination);
                    total_written += written;
                    destination = &mut destination[written..];

                    match updated {
                        noise::NoiseHandshake::Success {
                            cipher,
                            remote_peer_id,
                        } => {
                            let negotiation =
                                multistream_select::InProgress::new(match self.endpoint {
                                    Endpoint::Dialer => multistream_select::Config::Dialer {
                                        requested_protocol: yamux::PROTOCOL_NAME,
                                    },
                                    Endpoint::Listener => multistream_select::Config::Listener {
                                        supported_protocols: iter::once(yamux::PROTOCOL_NAME),
                                    },
                                });

                            self.state = HandshakeState::NegotiatingMultiplexing {
                                peer_id: remote_peer_id,
                                encryption: cipher,
                                negotiation,
                            };
                        }
                        noise::NoiseHandshake::InProgress(updated) => {
                            self.state =
                                HandshakeState::NegotiatingEncryption { handshake: updated }
                        }
                    };

                    if written == 0 {
                        break;
                    }
                }
                HandshakeState::NegotiatingMultiplexing {
                    mut encryption,
                    negotiation,
                    peer_id,
                } => {
                    let mut buffer = encryption.prepare_buffer_encryption(destination);
                    let (updated, written_interm) = negotiation.write_out(&mut *buffer);
                    let written = buffer.finish(written_interm);
                    destination = &mut destination[written..];
                    total_written += written;

                    self.state = match updated {
                        multistream_select::Negotiation::InProgress(updated) => {
                            HandshakeState::NegotiatingMultiplexing {
                                encryption,
                                negotiation: updated,
                                peer_id,
                            }
                        }
                        multistream_select::Negotiation::Success(_) => {
                            return (
                                Handshake::Success {
                                    encryption,
                                    remote_peer_id: peer_id,
                                },
                                total_written,
                            );
                        }
                        multistream_select::Negotiation::NotAvailable => todo!(), // TODO: ?!
                    };

                    if written == 0 {
                        break;
                    }
                }
            }
        }

        (Handshake::Healthy(self), total_written)
    }
}

pub struct NoiseKeyRequired {
    endpoint: Endpoint,
}

impl NoiseKeyRequired {
    /// Turn this [`NoiseKeyRequired`] back into a [`Healthy`] by indicating the noise key.
    pub fn resume(self, noise_key: &NoiseKey) -> HealthyHandshake {
        HealthyHandshake {
            endpoint: self.endpoint,
            state: HandshakeState::NegotiatingEncryption {
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
    /// No encryption protocol in common with the remote.
    NoEncryptionProtocol,
    /// No multiplexing protocol in common with the remote.
    NoMultiplexingProtocol,
    /// Error in the noise cipher. Data has most likely been corrupted.
    Noise(noise::CipherError),
}
