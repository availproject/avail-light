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

//! State machine handling the handshake with a TCP or WebSocket libp2p connection.
//!
//! A connection handshake consists of three steps:
//!
//! - A multistream-select negotiation to negotiate the encryption protocol. Only the noise
//! protocol is supported at the moment.
//! - A noise protocol handshake, where public keys are exchanged and symmetric encryption is
//! initialized.
//! - A multistream-select negotiation to negotiate the yamux protocol. Only the yamux protocol is
//! supported at the moment. This negotiation is performed on top of the noise cipher.
//!
//! This entire handshake requires in total either three or five TCP packets (not including the
//! TCP handshake), depending on the strategy used for the multistream-select protocol.

// TODO: finish commenting on the number of round trips

use super::{
    super::peer_id::PeerId,
    established::ConnectionPrototype,
    multistream_select,
    noise::{self, NoiseKey},
    yamux,
};

use alloc::{boxed::Box, vec};
use core::{fmt, iter};

mod tests;

/// Current state of a connection handshake.
#[derive(Debug, derive_more::From)]
pub enum Handshake {
    /// Connection handshake in progress.
    Healthy(HealthyHandshake),
    /// Connection handshake has reached the noise handshake, and it is necessary to know the
    /// noise key in order to proceed.
    NoiseKeyRequired(NoiseKeyRequired),
    /// Handshake has succeeded. Connection is now open.
    Success {
        /// Network identity of the remote.
        remote_peer_id: PeerId,
        /// Prototype for the connection.
        connection: ConnectionPrototype,
    },
}

impl Handshake {
    /// Shortcut for [`HealthyHandshake::new`] wrapped in a [`Handshake`].
    pub fn new(is_initiator: bool) -> Self {
        HealthyHandshake::new(is_initiator).into()
    }
}

/// Connection handshake in progress.
pub struct HealthyHandshake {
    state: HandshakeState,
}

enum HandshakeState {
    NegotiatingEncryptionProtocol {
        negotiation: multistream_select::InProgress<iter::Once<&'static str>, &'static str>,
        is_initiator: bool,
    },
    NegotiatingEncryption {
        handshake: Box<noise::HandshakeInProgress>,
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
    /// Must pass `true` if the connection has been opened by the local machine, or `false` if it
    /// has been opened by the remote.
    pub fn new(is_initiator: bool) -> Self {
        let negotiation = multistream_select::InProgress::new(if is_initiator {
            multistream_select::Config::Dialer {
                requested_protocol: noise::PROTOCOL_NAME,
            }
        } else {
            multistream_select::Config::Listener {
                supported_protocols: iter::once(noise::PROTOCOL_NAME),
            }
        });

        HealthyHandshake {
            state: HandshakeState::NegotiatingEncryptionProtocol {
                negotiation,
                is_initiator,
            },
        }
    }

    /// Feeds data coming from a socket through `incoming_data`, updates the internal state
    /// machine, and writes data destined to the socket to `outgoing_buffer`.
    ///
    /// On success, returns the new state of the negotiation, plus the number of bytes that have
    /// been read from `incoming_data` and the number of bytes that have been written to
    /// `outgoing_buffer`.
    ///
    /// An error is returned if the protocol is being violated by the remote. When that happens,
    /// the connection should be closed altogether.
    ///
    /// If the remote isn't ready to accept new data, pass an empty slice as `outgoing_buffer`.
    // TODO: should take the in and out buffers as iterators, to allow for vectored reads/writes; tricky because an impl Iterator<Item = &mut [u8]> + Clone is impossible to build
    pub fn read_write<'a>(
        mut self,
        mut incoming_buffer: &[u8],
        mut outgoing_buffer: (&'a mut [u8], &'a mut [u8]),
    ) -> Result<(Handshake, usize, usize), HandshakeError> {
        let mut total_read = 0;
        let mut total_written = 0;

        loop {
            match self.state {
                HandshakeState::NegotiatingEncryptionProtocol {
                    negotiation,
                    is_initiator,
                } => {
                    // Earliest point of the handshake. The encryption is being negotiated.
                    // Delegating read/write to the negotiation.
                    let (updated, num_read, num_written) = negotiation
                        .read_write(incoming_buffer, outgoing_buffer.0)
                        .map_err(HandshakeError::MultistreamSelect)?;
                    total_read += num_read;
                    total_written += num_written;

                    return match updated {
                        multistream_select::Negotiation::InProgress(updated) => {
                            if num_written == outgoing_buffer.0.len()
                                && !outgoing_buffer.1.is_empty()
                            {
                                outgoing_buffer = (outgoing_buffer.1, &mut []);
                                self.state = HandshakeState::NegotiatingEncryptionProtocol {
                                    negotiation: updated,
                                    is_initiator,
                                };
                                continue;
                            }

                            Ok((
                                Handshake::Healthy(HealthyHandshake {
                                    state: HandshakeState::NegotiatingEncryptionProtocol {
                                        negotiation: updated,
                                        is_initiator,
                                    },
                                }),
                                total_read,
                                total_written,
                            ))
                        }
                        multistream_select::Negotiation::Success(_) => {
                            // Reached the point where the Noise key is required in order to
                            // continue. This Noise key is requested from the user.
                            Ok((
                                Handshake::NoiseKeyRequired(NoiseKeyRequired { is_initiator }),
                                total_read,
                                total_written,
                            ))
                        }
                        multistream_select::Negotiation::NotAvailable => {
                            Err(HandshakeError::NoEncryptionProtocol)
                        }
                    };
                }

                HandshakeState::NegotiatingEncryption { handshake } => {
                    // Delegating read/write to the Noise handshake state machine.
                    let (updated, num_read, num_written) = handshake
                        .read_write(incoming_buffer, outgoing_buffer.0)
                        .map_err(HandshakeError::NoiseHandshake)?;
                    total_read += num_read;
                    total_written += num_written;
                    incoming_buffer = &incoming_buffer[num_read..];
                    outgoing_buffer.0 = &mut outgoing_buffer.0[num_written..];

                    if outgoing_buffer.0.is_empty() && !outgoing_buffer.1.is_empty() {
                        outgoing_buffer = (outgoing_buffer.1, &mut []);
                    }

                    match updated {
                        noise::NoiseHandshake::Success {
                            cipher,
                            remote_peer_id,
                        } => {
                            // Encryption layer has been successfully negotiated. Start the
                            // handshake for the multiplexing protocol negotiation.
                            let negotiation =
                                multistream_select::InProgress::new(if cipher.is_initiator() {
                                    multistream_select::Config::Dialer {
                                        requested_protocol: yamux::PROTOCOL_NAME,
                                    }
                                } else {
                                    multistream_select::Config::Listener {
                                        supported_protocols: iter::once(yamux::PROTOCOL_NAME),
                                    }
                                });

                            self.state = HandshakeState::NegotiatingMultiplexing {
                                peer_id: remote_peer_id,
                                encryption: cipher,
                                negotiation,
                            };

                            continue;
                        }
                        noise::NoiseHandshake::InProgress(updated) => {
                            return Ok((
                                Handshake::Healthy(HealthyHandshake {
                                    state: HandshakeState::NegotiatingEncryption {
                                        handshake: Box::new(updated),
                                    },
                                }),
                                total_read,
                                total_written,
                            ));
                        }
                    };
                }

                HandshakeState::NegotiatingMultiplexing {
                    negotiation,
                    mut encryption,
                    peer_id,
                } => {
                    // During the multiplexing protocol negotiation, all exchanges have to go
                    // through the Noise cipher.

                    // TODO: explain
                    let num_read = encryption
                        .inject_inbound_data(incoming_buffer)
                        .map_err(HandshakeError::Noise)?;
                    assert_eq!(num_read, incoming_buffer.len()); // TODO: not necessarily true; situation is a bit complicated; see noise module
                    total_read += num_read;

                    // Allocate a temporary buffer where to put the unencrypted data that should
                    // later be encrypted and written out.
                    // The size of this buffer is equal to the maximum possible size of
                    // unencrypted data that will lead to `outgoing_buffer.len()` encrypted bytes.
                    let mut out_intermediary = vec![
                        0;
                        encryption.encrypt_size_conv(
                            outgoing_buffer.0.len() + outgoing_buffer.1.len()
                        )
                    ];

                    // Continue the multistream-select negotiation, writing to `out_intermediary`.
                    let (updated, decrypted_read_num, written_interm) = negotiation
                        .read_write(encryption.decoded_inbound_data(), &mut out_intermediary)
                        .map_err(HandshakeError::MultistreamSelect)?;

                    // TODO: explain
                    encryption.consume_inbound_data(decrypted_read_num);

                    // Encrypt the content of `out_intermediary`, writing it to `outgoing_buffer`.
                    // It is guaranteed that `out_intermediary` will be entirely consumed and can
                    // thus be thrown away.
                    let (_unencrypted_read, encrypted_written) = encryption.encrypt(
                        iter::once(&out_intermediary[..written_interm]),
                        outgoing_buffer,
                    );
                    debug_assert_eq!(_unencrypted_read, written_interm);
                    total_written += encrypted_written;

                    return match updated {
                        multistream_select::Negotiation::InProgress(updated) => Ok((
                            Handshake::Healthy(HealthyHandshake {
                                state: HandshakeState::NegotiatingMultiplexing {
                                    negotiation: updated,
                                    encryption,
                                    peer_id,
                                },
                            }),
                            total_read,
                            total_written,
                        )),
                        multistream_select::Negotiation::Success(_) => Ok((
                            Handshake::Success {
                                connection: ConnectionPrototype::from_noise_yamux(encryption),
                                remote_peer_id: peer_id,
                            },
                            total_read,
                            total_written,
                        )),
                        multistream_select::Negotiation::NotAvailable => {
                            Err(HandshakeError::NoMultiplexingProtocol)
                        }
                    };
                }
            }
        }
    }
}

impl fmt::Debug for HealthyHandshake {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.debug_struct("HealthyHandshake").finish()
    }
}

/// Connection handshake has reached the noise handshake, and it is necessary to know the noise
/// key in order to proceed.
pub struct NoiseKeyRequired {
    is_initiator: bool,
}

impl NoiseKeyRequired {
    /// Turn this [`NoiseKeyRequired`] back into a [`HealthyHandshake`] by indicating the noise key.
    pub fn resume(self, noise_key: &NoiseKey) -> HealthyHandshake {
        HealthyHandshake {
            state: HandshakeState::NegotiatingEncryption {
                handshake: Box::new(noise::HandshakeInProgress::new(
                    noise_key,
                    self.is_initiator,
                )),
            },
        }
    }
}

impl fmt::Debug for NoiseKeyRequired {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.debug_struct("NoiseKeyRequired").finish()
    }
}

/// Error during a connection handshake. The connection should be shut down.
#[derive(Debug, derive_more::Display)]
pub enum HandshakeError {
    /// Protocol error during a multistream-select negotiation.
    MultistreamSelect(multistream_select::Error),
    /// Protocol error during the noise handshake.
    NoiseHandshake(noise::HandshakeError),
    /// No encryption protocol in common with the remote.
    ///
    /// The remote is behaving correctly but isn't compatible with the local node.
    NoEncryptionProtocol,
    /// No multiplexing protocol in common with the remote.
    ///
    /// The remote is behaving correctly but isn't compatible with the local node.
    NoMultiplexingProtocol,
    /// Error in the noise cipher. Data has most likely been corrupted.
    Noise(noise::CipherError),
}
