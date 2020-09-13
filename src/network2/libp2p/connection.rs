//! State machine handling a single TCP or WebSocket libp2p connection.
//!
//! This state machine tries to negotiate and apply the noise and yamux protocols on top of the
//! connection.

use core::iter;
use libp2p::PeerId;

mod multiplexed_connection;
mod multistream_select;
mod noise;
mod yamux;

pub struct Connection {
    endpoint: Endpoint,
    state: State,
}

enum State {
    NegotiatingEncryptionProtocol {
        negotiation: Option<multistream_select::InProgress<iter::Once<&'static str>, &'static str>>,
    },
    NegotiatingEncryption {
        handshake: noise::NoiseHandshake,
    },
    NegotiatingMultiplexing {
        peer_id: PeerId,
        encryption: noise::Noise,
        negotiation: Option<multistream_select::InProgress<iter::Once<&'static str>, &'static str>>,
    },
    Open {
        peer_id: PeerId,
        encryption: noise::Noise,
    },
}

impl Connection {
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

        Connection {
            endpoint,
            state: State::NegotiatingEncryptionProtocol {
                negotiation: Some(negotiation),
            },
        }
    }

    /// Parse the content of `data`. Returns the number of bytes that have been consumed from
    /// `data` and an object representing an event that happened on the connection.
    pub fn inject_data<'c, 'd>(
        &'c mut self,
        data: impl Iterator<Item = &'d [u8]>,
    ) -> (usize, Option<InjectDataOutcome<'c, 'd>>) {
        // TODO: restore
        /*// Handle the case where the connection is still negotiating the encryption protocol.
        if let State::NegotiatingEncryptionProtocol { negotiation } = &mut self.state {
            for buf in data {
                let mut buf_offset = 0;
                while buf_offset != buf.len() {
                    let (new_negotiation, processed, outcome) =
                        negotiation.inject_data(&buf[buf_offset..]);
                    *negotiation = new_negotiation;
                    buf_offset += processed;
                    debug_assert!(buf_offset <= buf.len());
                }
            }

            // TODO: return here
            todo!()
        }*/

        // Inject the received data into the cipher for decryption.
        match &mut self.state {
            State::NegotiatingEncryptionProtocol { .. } => unreachable!(),
            State::NegotiatingEncryption { .. } => todo!(),
            State::NegotiatingMultiplexing { encryption, .. } | State::Open { encryption, .. } => {
                for buf in data {
                    encryption.inject_inbound_data(buf);
                }
            }
        };

        todo!()
    }

    /// Write to the given buffer the bytes that are ready to be sent out. Returns the number of
    /// bytes written to `destination`.
    pub fn write_out(&mut self, mut destination: &mut [u8]) -> usize {
        let mut total_written = 0;

        loop {
            match &mut self.state {
                State::NegotiatingEncryptionProtocol { negotiation } => {
                    let (updated, written) = negotiation.take().unwrap().write_out(destination);
                    total_written += written;
                    destination = &mut destination[written..];

                    match updated {
                        multistream_select::Negotiation::InProgress(continuation) => {
                            *negotiation = Some(continuation);
                        }
                        multistream_select::Negotiation::Success(_) => {
                            let noise_endpoint = match self.endpoint {
                                Endpoint::Dialer => noise::Endpoint::Initiator,
                                Endpoint::Listener => noise::Endpoint::Responder,
                            };

                            self.state = State::NegotiatingEncryption {
                                handshake: noise::NoiseHandshake::new(noise_endpoint, &[0; 32]), // TODO: key
                            };
                        }
                        multistream_select::Negotiation::NotAvailable => todo!(), // TODO:
                    };
                }
                _ => todo!(),
            }
        }

        total_written
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
