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

//! Multistream-select is a protocol whose purpose is to negotiate protocols.
//!
//! # Context
//!
//! The multistream-select protocol makes it possible for two parties to negotiate a protocol.
//!
//! When using TCP connections, it is used immediately after a connection opens in order to
//! negotiate which encryption protocol to use, then after the encryption protocol handshake to
//! negotiate which multiplexing protocol to use.
//!
//! It is also used every time a substream opens in order to negotiate which protocol to use for
//! this substream in particular.
//!
//! Once a protocol has been negotiated, the connection or substream immediately starts speaking
//! this protocol.
//!
//! The multistream-select protocol is asymmetric: one side needs to be the dialer and the other
//! side the listener. In the context of a TCP connection, the dialer and listener correspond to
//! the dialer and listener of the connection. In the context of a substream, the dialer is the
//! side that initiated the opening of the substream.
//!
//! # About protocol names
//!
//! Due to flaws in the wire protocol design, a protocol named `ls` or `na` causes an ambiguity in
//! the exchange. Because protocol names are normally decided ahead of time, this situation is
//! expected to never arise, except in the presence of a malicious remote. The decision has been
//! taken that such protocol will always fail to negotiate, but will also not produce any error
//! or panic.
//!
//! Please don't intentionally name a protocol `ls` or `na`.
//!
//! # Usage
//!
//! To be written.
//!
//! # See also
//!
//! - [Official repository](https://github.com/multiformats/multistream-select)
//!

// TODO: write usage

use crate::util::leb128;

use alloc::{vec, vec::Vec};
use core::{cmp, fmt, iter, mem, slice, str};

/// Configuration of a multistream-select protocol.
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub enum Config<I, P> {
    /// Local node is the dialing side and requests the specific protocol.
    Dialer {
        /// Name of the protocol to try negotiate. The multistream-select negotiation will
        /// ultimately succeed if and only if the remote supports this protocol.
        requested_protocol: P,
    },
    /// Local node is the listening side.
    Listener {
        /// List of protocol names that are supported. In case of success, the-negotiated protocol
        /// is one of the protocols in this list.
        supported_protocols: I,
    },
}

/// Current state of a multistream-select negotiation.
#[derive(Debug)]
pub enum Negotiation<I, P> {
    /// Negotiation is still in progress. Use the provided [`InProgress`] object to inject and
    /// extract more data from/to the remote.
    InProgress(InProgress<I, P>),
    /// Negotiation has ended successfully. A protocol has been negotiated.
    Success(P),
    /// Negotiation has ended, but there isn't any protocol in common between the two parties.
    NotAvailable,
}

impl<I, P> Negotiation<I, P>
where
    I: Iterator<Item = P> + Clone,
    P: AsRef<str>,
{
    /// Shortcut method for [`InProgress::new`] and wrapping the [`InProgress`] in a
    /// [`Negotiation`].
    pub fn new(config: Config<I, P>) -> Self {
        Negotiation::InProgress(InProgress::new(config))
    }
}

/// Negotiation in progress.
pub struct InProgress<I, P> {
    /// Configuration of the negotiation. Always `Some` except right before destruction.
    config: Option<Config<I, P>>,
    /// Current state of the negotiation.
    state: InProgressState<P>,
    /// Maximum allowed size of a frame for `recv_buffer`.
    max_frame_len: usize,
    /// Incoming data is buffered in this `recv_buffer` before being decoded.
    recv_buffer: leb128::Framed,
}

/// Current state of the negotiation.
enum InProgressState<P> {
    SendHandshake {
        /// Number of bytes of the handshake already written out.
        num_bytes_written: usize,
    },
    SendProtocolRequest {
        /// Number of bytes of the request already written out.
        num_bytes_written: usize,
    },
    SendProtocolOk {
        /// Number of bytes of the response already written out.
        num_bytes_written: usize,
        /// Which protocol to acknowledge.
        protocol: P,
    },
    SendLsResponse {
        /// Number of bytes of the response already written out.
        num_bytes_written: usize,
    },
    SendProtocolNa {
        /// Number of bytes of the response already written out.
        num_bytes_written: usize,
    },
    HandshakeExpected,
    CommandExpected,
    ProtocolRequestAnswerExpected,
}

impl<I, P> InProgress<I, P>
where
    I: Iterator<Item = P> + Clone,
    P: AsRef<str>,
{
    /// Initializes a new handshake state machine.
    pub fn new(config: Config<I, P>) -> Self {
        // Length, in bytes, of the longest protocol name.
        let max_proto_name_len = match &config {
            Config::Dialer { requested_protocol } => requested_protocol.as_ref().len(),
            Config::Listener {
                supported_protocols,
            } => supported_protocols
                .clone()
                .map(|p| p.as_ref().len())
                .max()
                .unwrap_or(0),
        };

        // Any incoming frame larger than `max_frame_len` will trigger a protocol error.
        // This means that a protocol error might be reported in situations where the dialer
        // legitimately requests a protocol that the listener doesn't support. In order to prevent
        // confusion, a minimum length is applied to the protocol name length. Any protocol name
        // smaller than this will never trigger a protocol error, even if it isn't supported.
        const MIN_PROTO_LEN_NO_ERR: usize = 48;
        let max_frame_len = cmp::max(
            cmp::max(max_proto_name_len, MIN_PROTO_LEN_NO_ERR),
            HANDSHAKE.len(),
        ) + 1;

        InProgress {
            config: Some(config),
            // Note that the listener theoretically doesn't necessarily have to immediately send
            // a handshake, and could instead wait for a command from the dialer. In practice,
            // however, the specifications don't mention anything about this, and some libraries
            // such as js-libp2p wait for the listener to send a handshake before emitting a
            // command.
            state: InProgressState::SendHandshake {
                num_bytes_written: 0,
            },
            max_frame_len,
            recv_buffer: leb128::Framed::InProgress(leb128::FramedInProgress::new(max_frame_len)),
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
    pub fn read_write(
        mut self,
        mut incoming_data: &[u8],
        mut outgoing_buffer: &mut [u8],
    ) -> Result<(Negotiation<I, P>, usize, usize), Error> {
        let mut total_read = 0;
        let mut total_written = 0;

        loop {
            // `self.recv_buffer` serves as a helper to delimit `data` into frames. The first step
            // is to inject the received data into `recv_buffer`.
            if let leb128::Framed::InProgress(recv_buffer) = self.recv_buffer {
                let (num_read, framed_result) =
                    recv_buffer.update(incoming_data).map_err(Error::Frame)?;
                self.recv_buffer = framed_result;
                total_read += num_read;
                incoming_data = &incoming_data[num_read..];
            }

            match (self.state, &mut self.config) {
                (
                    InProgressState::SendHandshake {
                        mut num_bytes_written,
                    },
                    Some(config),
                ) => {
                    let message = MessageOut::Handshake::<iter::Empty<_>, &'static str>;
                    let (written, done) = message.write_out(num_bytes_written, outgoing_buffer);
                    outgoing_buffer = &mut outgoing_buffer[written..];
                    total_written += written;
                    num_bytes_written += written;

                    match (done, config) {
                        (false, _) => {
                            self.state = InProgressState::SendHandshake { num_bytes_written };
                            break;
                        }
                        (true, Config::Dialer { .. }) => {
                            self.state = InProgressState::SendProtocolRequest {
                                num_bytes_written: 0,
                            }
                        }
                        (true, Config::Listener { .. }) => {
                            self.state = InProgressState::HandshakeExpected
                        }
                    };
                }

                (
                    InProgressState::SendProtocolRequest {
                        mut num_bytes_written,
                    },
                    Some(Config::Dialer { requested_protocol }),
                ) => {
                    let message = MessageOut::ProtocolRequest::<iter::Empty<_>, _>(
                        requested_protocol.as_ref(),
                    );
                    let (written, done) = message.write_out(num_bytes_written, outgoing_buffer);
                    outgoing_buffer = &mut outgoing_buffer[written..];
                    total_written += written;
                    num_bytes_written += written;

                    if done {
                        self.state = InProgressState::HandshakeExpected;
                    } else {
                        self.state = InProgressState::SendProtocolRequest { num_bytes_written };
                        break;
                    }
                }

                (
                    InProgressState::SendProtocolNa {
                        mut num_bytes_written,
                    },
                    _,
                ) => {
                    let message = MessageOut::ProtocolNa::<iter::Empty<_>, &'static str>;
                    let (written, done) = message.write_out(num_bytes_written, outgoing_buffer);
                    outgoing_buffer = &mut outgoing_buffer[written..];
                    total_written += written;
                    num_bytes_written += written;

                    if done {
                        self.state = InProgressState::CommandExpected;
                    } else {
                        self.state = InProgressState::SendProtocolNa { num_bytes_written };
                        break;
                    }
                }

                (
                    InProgressState::SendProtocolOk {
                        mut num_bytes_written,
                        protocol,
                    },
                    _,
                ) => {
                    let message = MessageOut::ProtocolOk::<iter::Empty<_>, _>(protocol.as_ref());
                    let (written, done) = message.write_out(num_bytes_written, outgoing_buffer);
                    total_written += written;
                    num_bytes_written += written;

                    if done {
                        return Ok((Negotiation::Success(protocol), total_read, total_written));
                    } else {
                        self.state = InProgressState::SendProtocolOk {
                            num_bytes_written,
                            protocol,
                        };
                        break;
                    }
                }

                (
                    InProgressState::SendLsResponse {
                        mut num_bytes_written,
                    },
                    Some(Config::Listener {
                        supported_protocols,
                    }),
                ) => {
                    // TODO: overhead stupidity
                    let list = supported_protocols.clone().collect::<Vec<_>>();
                    let message = MessageOut::LsResponse(
                        list.iter().map(|p| AsRef::<str>::as_ref(p).as_bytes()),
                    );
                    let (written, done) = message.write_out(num_bytes_written, outgoing_buffer);
                    total_written += written;
                    num_bytes_written += written;

                    if done {
                        self.state = InProgressState::CommandExpected;
                    } else {
                        self.state = InProgressState::SendLsResponse { num_bytes_written };
                        break;
                    }
                }

                (InProgressState::HandshakeExpected, Some(Config::Dialer { .. })) => {
                    let frame = match self.recv_buffer {
                        leb128::Framed::Finished(frame) => {
                            self.recv_buffer = leb128::Framed::InProgress(
                                leb128::FramedInProgress::new(self.max_frame_len),
                            );
                            frame
                        }
                        leb128::Framed::InProgress(f) => {
                            // No frame is available.
                            debug_assert!(incoming_data.is_empty());
                            self.recv_buffer = leb128::Framed::InProgress(f);
                            self.state = InProgressState::HandshakeExpected;
                            break;
                        }
                    };

                    if &*frame != HANDSHAKE {
                        return Err(Error::BadHandshake);
                    }

                    // The dialer immediately sends the request after its handshake and before
                    // waiting for the handshake from the listener. As such, after receiving the
                    // handshake, the next step is to wait for the request answer.
                    self.state = InProgressState::ProtocolRequestAnswerExpected;
                }

                (InProgressState::HandshakeExpected, Some(Config::Listener { .. })) => {
                    let frame = match self.recv_buffer {
                        leb128::Framed::Finished(frame) => {
                            self.recv_buffer = leb128::Framed::InProgress(
                                leb128::FramedInProgress::new(self.max_frame_len),
                            );
                            frame
                        }
                        leb128::Framed::InProgress(f) => {
                            // No frame is available.
                            debug_assert!(incoming_data.is_empty());
                            self.recv_buffer = leb128::Framed::InProgress(f);
                            self.state = InProgressState::HandshakeExpected;
                            break;
                        }
                    };

                    if &*frame != HANDSHAKE {
                        return Err(Error::BadHandshake);
                    }

                    // The listener immediately sends the handshake at initialization. When this
                    // code is reached, it has therefore already been sent.
                    self.state = InProgressState::CommandExpected;
                }

                (
                    InProgressState::CommandExpected,
                    Some(Config::Listener {
                        supported_protocols,
                    }),
                ) => {
                    let frame = match self.recv_buffer {
                        leb128::Framed::Finished(frame) => {
                            self.recv_buffer = leb128::Framed::InProgress(
                                leb128::FramedInProgress::new(self.max_frame_len),
                            );
                            frame
                        }
                        leb128::Framed::InProgress(f) => {
                            // No frame is available.
                            debug_assert!(incoming_data.is_empty());
                            self.recv_buffer = leb128::Framed::InProgress(f);
                            self.state = InProgressState::CommandExpected;
                            break;
                        }
                    };

                    if frame.is_empty() {
                        return Err(Error::InvalidCommand);
                    } else if &*frame == b"ls\n" {
                        // Because of the order of checks, a protocol named `ls` will never be
                        // successfully negotiated. Debugging is expected to be less confusing if
                        // the negotiation always fails.
                        self.state = InProgressState::SendLsResponse {
                            num_bytes_written: 0,
                        };
                    } else if let Some(protocol) = supported_protocols
                        .clone()
                        .find(|p| p.as_ref().as_bytes() == &frame[..frame.len() - 1])
                    {
                        self.state = InProgressState::SendProtocolOk {
                            num_bytes_written: 0,
                            protocol,
                        };
                    } else {
                        self.state = InProgressState::SendProtocolNa {
                            num_bytes_written: 0,
                        };
                    }
                }

                (
                    InProgressState::ProtocolRequestAnswerExpected,
                    cfg @ Some(Config::Dialer { .. }),
                ) => {
                    let frame = match self.recv_buffer {
                        leb128::Framed::Finished(f) => f,
                        leb128::Framed::InProgress(f) => {
                            // No frame is available.
                            debug_assert!(incoming_data.is_empty());
                            self.recv_buffer = leb128::Framed::InProgress(f);
                            self.state = InProgressState::ProtocolRequestAnswerExpected;
                            break;
                        }
                    };

                    // Extract `config` to get the protocol name. All the paths below return,
                    // thereby `config` doesn't need to be put back in `self`.
                    let requested_protocol = match cfg.take() {
                        Some(Config::Dialer { requested_protocol }) => requested_protocol,
                        _ => unreachable!(),
                    };

                    if frame.last().map_or(true, |c| *c != b'\n') {
                        return Err(Error::UnexpectedProtocolRequestAnswer);
                    }
                    if &*frame == b"na\n" {
                        // Because of the order of checks, a protocol named `na` will never be
                        // successfully negotiated. Debugging is expected to be less confusing if
                        // the negotiation always fails.
                        return Ok((Negotiation::NotAvailable, total_read, total_written));
                    }
                    if &frame[..frame.len() - 1] != requested_protocol.as_ref().as_bytes() {
                        return Err(Error::UnexpectedProtocolRequestAnswer);
                    }
                    return Ok((
                        Negotiation::Success(requested_protocol),
                        total_read,
                        total_written,
                    ));
                }

                // Invalid states.
                (InProgressState::SendProtocolRequest { .. }, Some(Config::Listener { .. })) => {
                    unreachable!()
                }
                (InProgressState::SendLsResponse { .. }, Some(Config::Dialer { .. })) => {
                    unreachable!()
                }
                (InProgressState::CommandExpected, Some(Config::Dialer { .. })) => unreachable!(),
                (InProgressState::ProtocolRequestAnswerExpected, Some(Config::Listener { .. })) => {
                    unreachable!()
                }
                (_, None) => unreachable!(),
            };
        }

        // This point should be reached only if data is lacking in order to proceed.
        Ok((Negotiation::InProgress(self), total_read, total_written))
    }

    /// Similar to [`InProgress::read_write`], but write the outgoing data to a `Vec` rather than
    /// a borrowed buffer.
    ///
    /// Considering that the outgoing buffer is "unlimited" in size, it is guaranteed that either
    /// the negotiation ends (by a success or a failure) or the entire incoming buffer is
    /// consumed. In other words, it is not possible for [`Negotiation::InProgress`] to be
    /// returned alongside with a number or bytes that isn't equal to `incoming_data.len()`.
    pub fn read_write_vec(
        mut self,
        mut incoming_data: &[u8],
    ) -> Result<(Negotiation<I, P>, usize, Vec<u8>), Error> {
        // TODO: how to choose appropriate buffer length?
        let mut out_buffer = vec![0; 512];

        let mut total_read = 0;
        let mut total_written = 0;

        loop {
            let (new_state, read, written) = self.read_write(incoming_data, &mut out_buffer)?;
            total_read += read;
            incoming_data = &incoming_data[read..];
            total_written += written;
            assert!(total_written < out_buffer.len()); // TODO: how to choose appropriate buffer length?
            match new_state {
                Negotiation::InProgress(n) => self = n,
                other => {
                    out_buffer.truncate(total_written);
                    return Ok((other, total_read, out_buffer));
                }
            }
            if written == 0 {
                break;
            }
        }

        out_buffer.truncate(total_written);
        Ok((Negotiation::InProgress(self), total_read, out_buffer))
    }
}

impl<I, P> fmt::Debug for InProgress<I, P> {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.debug_tuple("InProgress").finish()
    }
}

/// Error that can happen during the negotiation.
#[derive(Debug, derive_more::Display)]
pub enum Error {
    /// Error while decoding a frame length, or frame size limit reached.
    Frame(leb128::FramedError),
    /// Unknown handshake or unknown multistream-select protocol version.
    BadHandshake,
    /// Received empty command.
    InvalidCommand,
    /// Received answer to protocol request that doesn't match the requested protocol.
    UnexpectedProtocolRequestAnswer,
}

/// Handshake message sent by both parties at the beginning of each multistream-select negotiation.
const HANDSHAKE: &[u8] = b"/multistream/1.0.0\n";

/// Message on the multistream-select protocol.
#[derive(Debug, Copy, Clone)]
pub enum MessageOut<I, P> {
    Handshake,
    Ls,
    LsResponse(I),
    ProtocolRequest(P),
    ProtocolOk(P),
    ProtocolNa,
}

impl<I, P> MessageOut<I, P>
where
    I: Iterator<Item = P> + Clone,
    P: AsRef<[u8]>,
{
    /// Returns the bytes representation of this message, as a list of buffers. The message
    /// consists in the concatenation of all buffers.
    pub fn into_bytes(mut self) -> impl Iterator<Item = impl AsRef<[u8]>> {
        let len = match &self {
            MessageOut::Handshake => HANDSHAKE.len(),
            MessageOut::Ls => 3,
            MessageOut::LsResponse(list) => list.clone().count(),
            MessageOut::ProtocolRequest(p) => p.as_ref().len() + 1,
            MessageOut::ProtocolOk(p) => p.as_ref().len() + 1,
            MessageOut::ProtocolNa => 3,
        };

        let length_prefix = leb128::encode_usize(len).map(|n| {
            struct One(u8);
            impl AsRef<[u8]> for One {
                fn as_ref(&self) -> &[u8] {
                    slice::from_ref(&self.0)
                }
            }
            One(n)
        });

        let mut n = 0;
        let body = iter::from_fn(move || {
            let ret = match (&mut self, n) {
                (MessageOut::Handshake, 0) => Some(either::Either::Left(HANDSHAKE)),
                (MessageOut::Handshake, _) => None,
                (MessageOut::Ls, 0) => Some(either::Either::Left(&b"ls\n"[..])),
                (MessageOut::Ls, 500) => Some(either::Either::Left(&b"\n"[..])), // TODO: hack, see below
                (MessageOut::Ls, _) => None,
                (MessageOut::LsResponse(list), n) if n % 3 == 0 => {
                    let protocol_len = list.clone().nth(n / 3)?.as_ref().len() + 1;
                    // TODO: overhead
                    let length = leb128::encode_usize(protocol_len).collect::<Vec<_>>();
                    Some(either::Either::Right(either::Right(length)))
                }
                (MessageOut::LsResponse(list), n) if n % 3 == 1 => {
                    let protocol = list.clone().nth(n / 3).unwrap();
                    Some(either::Either::Right(either::Left(protocol)))
                }
                (MessageOut::LsResponse(_), _) => Some(either::Either::Left(&b"\n"[..])),
                (MessageOut::ProtocolOk(_), 0) | (MessageOut::ProtocolRequest(_), 0) => {
                    let proto = match mem::replace(&mut self, MessageOut::Ls) {
                        MessageOut::ProtocolOk(p) | MessageOut::ProtocolRequest(p) => p,
                        _ => unreachable!(),
                    };
                    // TODO: this is completely a hack; decide whether it's acceptable
                    n = 499;
                    Some(either::Either::Right(either::Left(proto)))
                }
                (MessageOut::ProtocolOk(_), _) | (MessageOut::ProtocolRequest(_), _) => {
                    unreachable!()
                }
                (MessageOut::ProtocolNa, 0) => Some(either::Either::Left(&b"na\n"[..])),
                (MessageOut::ProtocolNa, _) => None,
            };

            if ret.is_some() {
                n += 1;
            }

            ret
        });

        length_prefix
            .map(either::Either::Left)
            .chain(body.map(either::Either::Right))
    }

    /// Write to the given buffer the bytes of the message, starting at `message_offset`. Returns
    /// the number of bytes written to `destination`, and a flag indicating whether the
    /// destination buffer was large enough to hold the entire message.
    ///
    /// # Panic
    ///
    /// Panics if `message_offset` is larger than the size of the message.
    ///
    pub fn write_out(self, mut message_offset: usize, mut destination: &mut [u8]) -> (usize, bool) {
        let mut total_written = 0;

        for buf in self.into_bytes() {
            let buf = buf.as_ref();
            if message_offset >= buf.len() {
                message_offset -= buf.len();
                continue;
            }

            let buf = &buf[message_offset..];

            let to_write = cmp::min(buf.len(), destination.len());
            destination[..to_write].copy_from_slice(&buf[..to_write]);
            destination = &mut destination[to_write..];
            total_written += to_write;
            message_offset = 0;

            if to_write < buf.len() {
                return (total_written, false);
            }
        }

        (total_written, true)
    }
}

#[cfg(test)]
mod tests {
    use super::{Config, MessageOut, Negotiation};
    use core::iter;

    #[test]
    fn encode() {
        assert_eq!(
            MessageOut::<iter::Empty<_>, &'static [u8]>::Handshake
                .into_bytes()
                .fold(Vec::new(), move |mut a, b| {
                    a.extend_from_slice(b.as_ref());
                    a
                }),
            b"\x13/multistream/1.0.0\n".to_vec()
        );

        assert_eq!(
            MessageOut::<iter::Empty<_>, &'static [u8]>::Ls
                .into_bytes()
                .fold(Vec::new(), move |mut a, b| {
                    a.extend_from_slice(b.as_ref());
                    a
                }),
            b"\x03ls\n".to_vec()
        );

        assert_eq!(
            MessageOut::ProtocolRequest::<iter::Empty<_>, _>("/hello")
                .into_bytes()
                .fold(Vec::new(), move |mut a, b| {
                    a.extend_from_slice(b.as_ref());
                    a
                }),
            b"\x07/hello\n".to_vec()
        );

        assert_eq!(
            MessageOut::<iter::Empty<_>, &'static [u8]>::ProtocolNa
                .into_bytes()
                .fold(Vec::new(), move |mut a, b| {
                    a.extend_from_slice(b.as_ref());
                    a
                }),
            b"\x03na\n".to_vec()
        );

        // TODO: all encoding testing
    }

    #[test]
    fn negotiation_basic_works() {
        fn test_with_buffer_sizes(size1: usize, size2: usize) {
            let mut negotiation1 = Negotiation::new(Config::<iter::Once<_>, _>::Dialer {
                requested_protocol: "/foo",
            });
            let mut negotiation2 = Negotiation::new(Config::Listener {
                supported_protocols: iter::once("/foo"),
            });

            let mut buf_1_to_2 = Vec::new();
            let mut buf_2_to_1 = Vec::new();

            while !matches!(
                (&negotiation1, &negotiation2),
                (Negotiation::Success(_), Negotiation::Success(_))
            ) {
                match negotiation1 {
                    Negotiation::InProgress(nego) => {
                        if buf_1_to_2.is_empty() {
                            buf_1_to_2.resize(size1, 0);
                            let (updated, num_read, written) =
                                nego.read_write(&buf_2_to_1, &mut buf_1_to_2).unwrap();
                            negotiation1 = updated;
                            for _ in 0..num_read {
                                buf_2_to_1.remove(0);
                            }
                            buf_1_to_2.truncate(written);
                        } else {
                            let (updated, num_read, _) =
                                nego.read_write(&buf_2_to_1, &mut []).unwrap();
                            negotiation1 = updated;
                            for _ in 0..num_read {
                                buf_2_to_1.remove(0);
                            }
                        }
                    }
                    Negotiation::Success(_) => {}
                    Negotiation::NotAvailable => panic!(),
                }

                match negotiation2 {
                    Negotiation::InProgress(nego) => {
                        if buf_2_to_1.is_empty() {
                            buf_2_to_1.resize(size2, 0);
                            let (updated, num_read, written) =
                                nego.read_write(&buf_1_to_2, &mut buf_2_to_1).unwrap();
                            negotiation2 = updated;
                            for _ in 0..num_read {
                                buf_1_to_2.remove(0);
                            }
                            buf_2_to_1.truncate(written);
                        } else {
                            let (updated, num_read, _) =
                                nego.read_write(&buf_1_to_2, &mut []).unwrap();
                            negotiation2 = updated;
                            for _ in 0..num_read {
                                buf_1_to_2.remove(0);
                            }
                        }
                    }
                    Negotiation::Success(_) => {}
                    Negotiation::NotAvailable => panic!(),
                }
            }
        }

        test_with_buffer_sizes(256, 256);
        test_with_buffer_sizes(1, 1);
        test_with_buffer_sizes(1, 2048);
        test_with_buffer_sizes(2048, 1);
    }
}
