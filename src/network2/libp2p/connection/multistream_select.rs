//! https://github.com/multiformats/multistream-select

// TODO: docs

use crate::network2::leb128;

use alloc::borrow::Cow;
use core::{cmp, iter, mem, slice, str};

/// Handshake message sent by both sides at the beginning of each multistream-select negotiation.
const HANDSHAKE: &'static [u8] = b"/multistream/1.0.0\n";

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub enum Config<I, P> {
    Dialer { requested_protocol: P },
    Listener { supported_protocols: I },
}

pub struct Negotiation<I, P> {
    config: Config<I, P>,
    state: NegotiationState,
}

enum NegotiationState {
    SendHandshake { num_bytes_written: usize },
    SendProtocolRequest { num_bytes_written: usize },
    HandshakeExpected { num_bytes_received: usize },
    //CommandExpected,
}

impl<I, P> Negotiation<I, P>
where
    I: Iterator<Item = P> + Clone,
    P: AsRef<str>,
{
    pub fn new(config: Config<I, P>) -> Self {
        Negotiation {
            config,
            state: NegotiationState::HandshakeExpected {
                num_bytes_received: 0,
            },
        }
    }

    /// Feeds more data to the negotiation.
    pub fn inject_data(mut self, mut data: &[u8]) -> (Self, usize) {
        loop {
            match &mut self.state {
                NegotiationState::SendHandshake { .. }
                | NegotiationState::SendProtocolRequest { .. } => break (self, 0),

                NegotiationState::HandshakeExpected { num_bytes_received } => {
                    // TODO: length prefix not accounted for
                    let remain = HANDSHAKE.len() - *num_bytes_received;
                    let max_parse = cmp::min(data.len(), remain);
                    if &data[..max_parse] != &HANDSHAKE[*num_bytes_received..][..max_parse] {
                        todo!()
                    }
                    *num_bytes_received += max_parse;
                    data = &data[max_parse..];
                    if *num_bytes_received == HANDSHAKE.len() {
                        todo!();
                    }
                }
            }
        }
    }

    /// Write to the given buffer the bytes that are ready to be sent out. Returns the number of
    /// bytes written to `destination`.
    pub fn write_out(&mut self, destination: &mut [u8]) -> usize {
        let mut total_written = 0;

        loop {
            let destination = &mut destination[total_written..];

            match &mut self.state {
                NegotiationState::SendHandshake { num_bytes_written } => {
                    let message = MessageOut::Handshake::<&'static str>;
                    let (written, done) = message.write_out(*num_bytes_written, destination);
                    total_written += written;
                    *num_bytes_written += written;

                    match (done, &self.config) {
                        (false, _) => break,
                        (true, Config::Dialer { .. }) => {
                            self.state = NegotiationState::SendProtocolRequest {
                                num_bytes_written: 0,
                            }
                        }
                        (true, Config::Listener { .. }) => {
                            self.state = NegotiationState::HandshakeExpected {
                                num_bytes_received: 0,
                            }
                        }
                    };
                }

                NegotiationState::SendProtocolRequest { num_bytes_written } => {
                    let message = match &self.config {
                        Config::Dialer { requested_protocol } => {
                            MessageOut::ProtocolRequest(requested_protocol.as_ref())
                        }
                        _ => unreachable!(),
                    };
                    let (written, done) = message.write_out(*num_bytes_written, destination);
                    total_written += written;
                    *num_bytes_written += written;

                    if done {
                        todo!()
                    }
                }

                NegotiationState::HandshakeExpected { .. } => break,

                //State::NegotiatingEncryption { negotiation } => negotiation.write_out(destination),
                _ => todo!(),
            }
        }

        total_written
    }
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
    use super::MessageOut;

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
}
