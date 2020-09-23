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
//! # Usage
//!
//! To be written.
//!
//! # See also
//!
//! - [Official repository](https://github.com/multiformats/multistream-select)
//!

// TODO: write usage once it's fully fledged out

use crate::network2::leb128;

use alloc::borrow::Cow;
use core::{cmp, fmt, iter, mem, slice, str};

/// Handshake message sent by both parties at the beginning of each multistream-select negotiation.
const HANDSHAKE: &'static [u8] = b"/multistream/1.0.0\n";

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
    state: InProgressState<P>,
    /// Incoming data is buffered in this `recv_buffer` before being decoded.
    recv_buffer: leb128::Framed,
}

enum InProgressState<P> {
    SendHandshake {
        num_bytes_written: usize,
    },
    SendProtocolRequest {
        num_bytes_written: usize,
    },
    SendProtocolOk {
        num_bytes_written: usize,
        protocol: P,
    },
    SendProtocolNa {
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
        const MIN_PROTO_LEN_NO_ERR: usize = 32;
        let max_frame_len = cmp::max(
            cmp::min(max_proto_name_len, MIN_PROTO_LEN_NO_ERR),
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
            recv_buffer: leb128::Framed::new(max_frame_len),
        }
    }

    /// Feeds more data to the negotiation.
    ///
    /// Returns either an error in case of protocol error, or the new state of the negotiation
    /// and the number of bytes that have been processed in `data`.
    ///
    /// If the number of bytes read is different from 0, you should immediately call this method
    /// again with the remaining data.
    pub fn inject_data(mut self, mut data: &[u8]) -> Result<(Negotiation<I, P>, usize), Error> {
        let mut total_read = 0;

        loop {
            // If the current state involves sending out data, any additional incoming data is
            // refused in order to apply back-pressure on the remote.
            match &self.state {
                InProgressState::SendHandshake { .. }
                | InProgressState::SendProtocolRequest { .. }
                | InProgressState::SendProtocolNa { .. }
                | InProgressState::SendProtocolOk { .. } => break,
                _ => {}
            }

            // `recv_buffer` serves as a helper to delimit `data` into frames. The first step is
            // to inject the received data into `recv_buffer`.
            // `recv_buffer` doesn't necessarily consume all of `data`.
            let num_read = self.recv_buffer.inject_data(data).map_err(Error::Frame)?;
            total_read += num_read;
            data = &data[num_read..];

            let frame = match self.recv_buffer.take_frame() {
                Some(f) => f,
                None => {
                    // No frame is available.
                    debug_assert!(data.is_empty());
                    break;
                }
            };

            match (&mut self.state, &mut self.config) {
                (InProgressState::HandshakeExpected, Some(Config::Dialer { .. })) => {
                    if &*frame != HANDSHAKE {
                        return Err(Error::BadHandshake);
                    }
                    // The dialer immediately sends the request after its handshake and before
                    // waiting for the handshake from the listener. As such, after receiving the
                    // handshake, the next step is to wait for the request answer.
                    self.state = InProgressState::ProtocolRequestAnswerExpected;
                }

                (InProgressState::HandshakeExpected, Some(Config::Listener { .. })) => {
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
                    if frame.is_empty() {
                        return Err(Error::InvalidCommand);
                    } else if &*frame == b"ls\n" {
                        todo!() // TODO:
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
                    mut cfg @ Some(Config::Dialer { .. }),
                ) => {
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
                        return Ok((Negotiation::NotAvailable, total_read));
                    }
                    if &frame[..frame.len() - 1] != requested_protocol.as_ref().as_bytes() {
                        return Err(Error::UnexpectedProtocolRequestAnswer);
                    }
                    return Ok((Negotiation::Success(requested_protocol), total_read));
                }

                // Handled above.
                (InProgressState::SendHandshake { .. }, _)
                | (InProgressState::SendProtocolRequest { .. }, _)
                | (InProgressState::SendProtocolNa { .. }, _)
                | (InProgressState::SendProtocolOk { .. }, _) => unreachable!(),

                // Invalid states.
                (InProgressState::CommandExpected, Some(Config::Dialer { .. })) => unreachable!(),
                (InProgressState::ProtocolRequestAnswerExpected, Some(Config::Listener { .. })) => {
                    unreachable!()
                }
                (_, None) => unreachable!(),
            };
        }

        // This point should be reached only if data is lacking in order to proceed.
        Ok((Negotiation::InProgress(self), total_read))
    }

    /// Write to the given buffer the bytes that are ready to be sent out. Returns the new state
    /// of the negotiation and the number of bytes that have been written to `destination`.
    ///
    /// If `0` is returned and `destination` wasn't empty, no more data can be written out before
    /// [`InProgress::inject_data`] is called.
    pub fn write_out(mut self, destination: &mut [u8]) -> (Negotiation<I, P>, usize) {
        let mut total_written = 0;

        loop {
            let destination = &mut destination[total_written..];

            match self.state {
                InProgressState::SendHandshake {
                    mut num_bytes_written,
                } => {
                    let message = MessageOut::Handshake::<&'static str>;
                    let (written, done) = message.write_out(num_bytes_written, destination);
                    total_written += written;
                    num_bytes_written += written;

                    match (done, self.config.as_ref().unwrap()) {
                        (false, _) => {
                            self.state = InProgressState::SendHandshake { num_bytes_written }
                        }
                        (true, Config::Dialer { .. }) => {
                            self.state = InProgressState::SendProtocolRequest {
                                num_bytes_written: 0,
                            }
                        }
                        (true, Config::Listener { .. }) => {
                            self.state = InProgressState::HandshakeExpected;
                        }
                    };
                }

                InProgressState::SendProtocolRequest {
                    mut num_bytes_written,
                } => {
                    let message = match self.config.as_ref().unwrap() {
                        Config::Dialer { requested_protocol } => {
                            MessageOut::ProtocolRequest(requested_protocol.as_ref())
                        }
                        _ => unreachable!(),
                    };
                    let (written, done) = message.write_out(num_bytes_written, destination);
                    total_written += written;
                    num_bytes_written += written;

                    if done {
                        self.state = InProgressState::HandshakeExpected;
                    } else {
                        self.state = InProgressState::SendProtocolRequest { num_bytes_written };
                    }
                }

                InProgressState::SendProtocolNa {
                    mut num_bytes_written,
                } => {
                    let message = MessageOut::ProtocolNa::<&'static str>;
                    let (written, done) = message.write_out(num_bytes_written, destination);
                    total_written += written;
                    num_bytes_written += written;

                    if done {
                        self.state = InProgressState::CommandExpected;
                    } else {
                        self.state = InProgressState::SendProtocolNa { num_bytes_written };
                    }
                }

                InProgressState::SendProtocolOk {
                    mut num_bytes_written,
                    protocol,
                } => {
                    let message = MessageOut::ProtocolOk(protocol.as_ref());
                    let (written, done) = message.write_out(num_bytes_written, destination);
                    total_written += written;
                    num_bytes_written += written;

                    if done {
                        return (Negotiation::Success(protocol), total_written);
                    } else {
                        self.state = InProgressState::SendProtocolOk {
                            num_bytes_written,
                            protocol,
                        };
                    }
                }

                // Nothing to write out when the state machine is waiting for a message to arrive.
                InProgressState::HandshakeExpected { .. }
                | InProgressState::CommandExpected { .. }
                | InProgressState::ProtocolRequestAnswerExpected => break,
            }
        }

        (Negotiation::InProgress(self), total_written)
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

/// Message on the multistream-select protocol.
#[derive(Debug, Copy, Clone)]
pub enum MessageOut<P> {
    Handshake,
    Ls,
    LsResponse, // TODO: unimplemented
    ProtocolRequest(P),
    ProtocolOk(P),
    ProtocolNa,
}

impl<P> MessageOut<P>
where
    P: AsRef<[u8]>,
{
    /// Returns the bytes representation of this message, as a list of buffers. The message
    /// consists in the concatenation of all buffers.
    pub fn to_bytes(mut self) -> impl Iterator<Item = impl AsRef<[u8]>> {
        let len = match &self {
            MessageOut::Handshake => HANDSHAKE.len(),
            MessageOut::Ls => 3,
            MessageOut::LsResponse => todo!(),
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

        // TODO: check that protocol name isn't `ls` or `na`

        let mut n = 0;
        let body = iter::from_fn(move || {
            let ret = match (&mut self, n) {
                (MessageOut::Handshake, 0) => Some(either::Either::Left(HANDSHAKE)),
                (MessageOut::Handshake, _) => None,
                (MessageOut::Ls, 0) => Some(either::Either::Left(&b"ls\n"[..])),
                (MessageOut::Ls, 500) => Some(either::Either::Left(&b"\n"[..])), // TODO: hack, see below
                (MessageOut::Ls, _) => None,
                (MessageOut::LsResponse, _) => todo!(),
                (MessageOut::ProtocolOk(_), 0) | (MessageOut::ProtocolRequest(_), 0) => {
                    let proto = match mem::replace(&mut self, MessageOut::Ls) {
                        MessageOut::ProtocolOk(p) | MessageOut::ProtocolRequest(p) => p,
                        _ => unreachable!(),
                    };
                    // TODO: this is completely a hack; decide whether it's acceptable
                    n = 499;
                    Some(either::Either::Right(proto))
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
    pub fn write_out(self, mut message_offset: usize, destination: &mut [u8]) -> (usize, bool) {
        let mut total_written = 0;

        for buf in self.to_bytes() {
            let buf = buf.as_ref();
            if message_offset >= buf.len() {
                message_offset -= buf.len();
                continue;
            }

            let buf = &buf[message_offset..];
            let destination = &mut destination[total_written..];

            let to_write = cmp::min(buf.len(), destination.len());
            destination[..to_write].copy_from_slice(&buf[..to_write]);
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
            MessageOut::<&'static [u8]>::Handshake
                .to_bytes()
                .fold(Vec::new(), move |mut a, b| {
                    a.extend_from_slice(b.as_ref());
                    a
                }),
            b"\x13/multistream/1.0.0\n".to_vec()
        );

        assert_eq!(
            MessageOut::<&'static [u8]>::Ls
                .to_bytes()
                .fold(Vec::new(), move |mut a, b| {
                    a.extend_from_slice(b.as_ref());
                    a
                }),
            b"\x03ls\n".to_vec()
        );

        assert_eq!(
            MessageOut::ProtocolRequest("/hello")
                .to_bytes()
                .fold(Vec::new(), move |mut a, b| {
                    a.extend_from_slice(b.as_ref());
                    a
                }),
            b"\x07/hello\n".to_vec()
        );

        assert_eq!(
            MessageOut::<&'static [u8]>::ProtocolNa
                .to_bytes()
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
        // TODO: test doesn't work with smaller buffer length

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
                Negotiation::InProgress(mut nego) => {
                    if buf_1_to_2.is_empty() {
                        buf_1_to_2.resize(256, 0);
                        let (updated, written) = nego.write_out(&mut buf_1_to_2);
                        negotiation1 = updated;
                        buf_1_to_2.truncate(written);
                    } else {
                        negotiation1 = Negotiation::InProgress(nego);
                    }
                }
                Negotiation::Success(_) => {}
                Negotiation::NotAvailable => panic!(),
            }

            match negotiation1 {
                Negotiation::InProgress(mut nego) => {
                    let (updated, num_read) = nego.inject_data(&buf_2_to_1).unwrap();
                    negotiation1 = updated;
                    for _ in 0..num_read {
                        buf_2_to_1.remove(0);
                    }
                }
                Negotiation::Success(_) => {}
                Negotiation::NotAvailable => panic!(),
            }

            match negotiation2 {
                Negotiation::InProgress(mut nego) => {
                    if buf_2_to_1.is_empty() {
                        buf_2_to_1.resize(256, 0);
                        let (updated, written) = nego.write_out(&mut buf_2_to_1);
                        negotiation2 = updated;
                        buf_2_to_1.truncate(written);
                    } else {
                        negotiation2 = Negotiation::InProgress(nego);
                    }
                }
                Negotiation::Success(_) => {}
                Negotiation::NotAvailable => panic!(),
            }

            match negotiation2 {
                Negotiation::InProgress(mut nego) => {
                    let (updated, num_read) = nego.inject_data(&buf_1_to_2).unwrap();
                    negotiation2 = updated;
                    for _ in 0..num_read {
                        buf_1_to_2.remove(0);
                    }
                }
                Negotiation::Success(_) => {}
                Negotiation::NotAvailable => panic!(),
            }
        }
    }
}
