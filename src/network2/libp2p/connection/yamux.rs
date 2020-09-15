//! Yamux multiplexing protocol.
//!
//! The yamux protocol is a multiplexing protocol. As such, it allows dividing a single stream of
//! data, typically a TCP socket, into multiple individual parallel substreams. The data sent and
//! received over that single stream is divided into frames which, with the exception of `ping`
//! and `goaway` frames, belong to a specific substream. In other words, the data transmitted
//! over the substreams is interleaved.
//!
//! Specifications available at https://github.com/hashicorp/yamux/blob/master/spec.md
//!
//! # Usage
//!
//! The user of this module is responsible for managing the list of substreams in the form of a
//! collection of [`SubstreamState`]s. Each substream is identified by a [`SubstreamId`].

// TODO: finish usage

use core::{convert::TryFrom as _, fmt, iter};

/// Name of the protocol, typically used when negotiated it using *multistream-select*.
pub const PROTOCOL_NAME: &str = "/yamux/1.0.0";

/// By default, all new substreams have this implicit window size.
const DEFAULT_FRAME_SIZE: u32 = 256 * 1024;

pub fn decode<TIter, TBuf>(
    data: impl IntoIterator<Item = TBuf, IntoIter = TIter>,
) -> DecodeStep<TIter, TBuf>
where
    TIter: Iterator<Item = TBuf> + Clone,
    TBuf: AsRef<[u8]>,
{
    let mut data = data.into_iter();
    let current_buffer = match data.next() {
        Some(b) => b,
        None => return DecodeStep::Finished { num_read: 0 },
    };

    Decoder {
        current_buffer,
        current_buffer_offset: 0,
        next_buffer: data,
        num_read: 0,
    }
    .resume()
}

pub struct SubstreamState {
    /// Identifier of the substream.
    id: SubstreamId,
    /// Amount of data the remote is allowed to transmit to the local node.
    remote_allowed_window: u32,
    /// Amount of data the local node is allowed to transmit to the remote.
    allowed_window: u32,
    /// True if the writing side of the local node is closed for this substream.
    local_write_closed: bool,
    /// True if the writing side of the remote node is closed for this substream.
    remote_write_closed: bool,
}

impl fmt::Debug for SubstreamState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SubstreamState")
            .field("id", &self.id)
            .finish()
    }
}

/// Identifier of a substream in the context of a connection.
#[derive(Debug, Copy, Clone, Ord, PartialOrd, Eq, PartialEq, Hash, derive_more::From)]
pub struct SubstreamId(pub u32);

pub enum DecodeStep<TIter, TBuf> {
    NewSubstream {
        num_read: usize,
        id: SubstreamId,
        state: SubstreamState,
        resume: Decoder<TIter, TBuf>,
    },
    SubstreamStateRequest {
        num_read: usize,
        id: SubstreamId,
        request: SubstreamStateReq<TIter, TBuf>,
    },
    /// Decoding is finished, either because all the data has been processed or because more data
    /// is needed in order to be able to successfully decode something.
    Finished { num_read: usize },
    /// Error in the yamux protocol. The connection should be terminated.
    Error(Error),
}

pub struct SubstreamStateReq<TIter, TBuf> {
    /// Header of the frame currently being decoded.
    header: [u8; 12],
    decoder: Decoder<TIter, TBuf>,
}

impl<TIter, TBuf> SubstreamStateReq<TIter, TBuf>
where
    TIter: Iterator<Item = TBuf> + Clone,
    TBuf: AsRef<[u8]>,
{
    // TODO: docs
    ///
    ///
    /// # Panic
    ///
    /// Panics if the id of the provided [`SubstreamState`] doesn't match the requested one.
    ///
    pub fn resume(self, state: Option<&mut SubstreamState>) -> DecodeStep<TIter, TBuf> {
        if let Some(state) = &state {
            assert_eq!(
                state.id.0,
                u32::from_be_bytes(<[u8; 4]>::try_from(&self.header[4..8]).unwrap(),)
            );
        }

        let header_length_field =
            u32::from_be_bytes(<[u8; 4]>::try_from(&self.header[8..12]).unwrap());

        match (self.header[1], state) {
            (0, Some(existing)) => {
                if header_length_field > existing.remote_allowed_window {
                    return DecodeStep::Error(Error::CreditsExceeded);
                }
            }
            (0, None) => {
                if header_length_field > DEFAULT_FRAME_SIZE {
                    return DecodeStep::Error(Error::CreditsExceeded);
                }
            }
            (1, Some(existing)) => {}
            (1, None) => {}
            // A `SubstreamStateReq` struct is created only when the frame type is 0 or 1.
            (_, _) => unreachable!(),
        }

        self.decoder.resume()
    }
}

pub struct Decoder<TIter, TBuf> {
    current_buffer: TBuf,
    current_buffer_offset: usize,
    next_buffer: TIter,
    /// Total number of bytes read so far, to report to the user. Should only be updated after
    /// frames that have been completed processed.
    num_read: usize,
}

impl<TIter, TBuf> Decoder<TIter, TBuf>
where
    TIter: Iterator<Item = TBuf> + Clone,
    TBuf: AsRef<[u8]>,
{
    pub fn resume(self) -> DecodeStep<TIter, TBuf> {
        // Try to collect 12 bytes from the input data, to form a header.
        let header = {
            // TODO: there might be a more elegant way to write this
            let mut header = arrayvec::ArrayVec::<[u8; 12]>::new();
            let mut cur_owned = None;
            let mut cur = &self.current_buffer.as_ref()[self.current_buffer_offset..];
            let mut next = self.next_buffer.clone();
            while header.len() != 12 {
                if cur.is_empty() {
                    match next.next() {
                        Some(n) => {
                            cur_owned = Some(n);
                            cur = cur_owned.as_ref().unwrap().as_ref();
                            continue;
                        }
                        None => break,
                    }
                }

                header.push(cur[0]);
                cur = &cur[1..];
            }

            match header.into_inner() {
                Ok(h) => h,
                Err(_) => {
                    // Not enough data in `header`.
                    return DecodeStep::Finished {
                        num_read: self.num_read,
                    };
                }
            }
        };

        // Byte 0 of the header is the yamux version number. Return an error if it isn't 0.
        if header[0] != 0 {
            return DecodeStep::Error(Error::UnknownVersion(header[0]));
        }

        // Byte 1 of the header indicates the type of message.
        //
        // In case of a frame concerning a substream (data frames `0` or window updates `1`),
        // immediately ask the `SubstreamState` from the user.
        //
        // It might be tempting, for data frames, to return `DecodeStep::Finished` if not enough
        // data is available. In terms of resilience, however, it is a better idea to first check
        // whether the remote is allowed to send a frame of this length.
        if header[1] == 0 || header[1] == 1 {
            return DecodeStep::SubstreamStateRequest {
                num_read: self.num_read,
                id: SubstreamId(u32::from_be_bytes(
                    <[u8; 4]>::try_from(&header[4..8]).unwrap(),
                )),
                request: SubstreamStateReq {
                    header,
                    decoder: self,
                },
            };
        }

        // TODO: ping
        if header[1] == 2 {
            todo!()
        }

        // TODO: go away
        if header[1] == 3 {
            todo!()
        }

        DecodeStep::Error(Error::BadFrameType(header[1]))
    }
}

/// Error while decoding the yamux stream.
#[derive(Debug, derive_more::Display)]
pub enum Error {
    /// Unknown version number in a header.
    UnknownVersion(u8),
    /// Unrecognized value for the type of frame as indicated in the header.
    BadFrameType(u8),
    /// Remote tried to send more data than it was allowed to.
    CreditsExceeded,
}
