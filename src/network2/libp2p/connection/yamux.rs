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

use core::{
    cmp,
    convert::TryFrom as _,
    fmt, iter,
    num::{NonZeroU32, NonZeroUsize},
};

/// Name of the protocol, typically used when negotiated it using *multistream-select*.
pub const PROTOCOL_NAME: &str = "/yamux/1.0.0";

pub struct Connection<T> {
    // TODO: there's an actual chance of collision attack here if we don't use a siphasher
    substreams: hashbrown::HashMap<u32, Substream<T>, fnv::FnvBuildHasher>,
    /// Buffer for a partially read header.
    incoming_header: arrayvec::ArrayVec<[u8; 12]>,
    /// Header to be written out in priority.
    pending_out_header: arrayvec::ArrayVec<[u8; 12]>,
    pending_incoming_substream: Option<SubstreamId>,
}

struct Substream<T> {
    /// Identifier of the substream.
    id: SubstreamId,
    /// Amount of data the remote is allowed to transmit to the local node.
    remote_allowed_window: u64,
    /// Amount of data the local node is allowed to transmit to the remote.
    allowed_window: u64,
    /// True if the writing side of the local node is closed for this substream.
    local_write_closed: bool,
    /// True if the writing side of the remote node is closed for this substream.
    remote_write_closed: bool,
    /// Data chosen by the user.
    user_data: T,
}

impl<T> Connection<T> {
    /// Initializes a new yamux state machine.
    pub fn new() -> Connection<T> {
        Self::with_capacity(0)
    }

    /// Initializes a new yamux state machine with enough capacity for the given number of
    /// substreams.
    pub fn with_capacity(capacity: usize) -> Connection<T> {
        Connection {
            substreams: hashbrown::HashMap::with_capacity_and_hasher(capacity, Default::default()),
            incoming_header: arrayvec::ArrayVec::new(),
            pending_out_header: arrayvec::ArrayVec::new(),
            pending_incoming_substream: None,
        }
    }

    /// Process some incoming data.
    ///
    /// The received payload may require some data to be written back on the socket. As such,
    /// `out` is a buffer destined to hold data to be sent to the remote.
    ///
    /// # Panic
    ///
    /// Panics if pending incoming substream.
    ///
    // TODO: reword panic reason
    pub fn incoming_data(
        &mut self,
        mut data: &[u8],
        mut out: &mut [u8],
    ) -> Result<IncomingDataOutcome, Error> {
        assert!(self.pending_incoming_substream.is_none());

        let mut total_read = 0;
        let mut total_written = 0;

        loop {
            // Start by emptying `pending_out_header`. The code below might require writing
            // to it, and as such we can't proceed with any reading if it isn't empty.
            if !self.pending_out_header.is_empty() && !out.is_empty() {
                let to_write = cmp::min(self.pending_out_header.len(), out.len());
                out[..to_write].copy_from_slice(&self.pending_out_header[..to_write]);
                for _ in 0..to_write {
                    self.pending_out_header.remove(0);
                }
                out = &mut out[to_write..];
                total_written += to_write;
            }
            if !self.pending_out_header.is_empty() {
                break;
            }

            // Try to copy as much as possible from `data` to `header`.
            while !data.is_empty() && self.incoming_header.len() < 12 {
                self.incoming_header.push(data[0]);
                total_read += 1;
                data = &data[1..];
            }

            // Not enough data to finish receiving header. Nothing more can be done.
            if self.incoming_header.len() != 12 {
                debug_assert!(data.is_empty());
                break;
            }

            // Full header to decode available.

            // Byte 0 of the header is the yamux version number. Return an error if it isn't 0.
            if self.incoming_header[0] != 0 {
                return Err(Error::UnknownVersion(self.incoming_header[0]));
            }

            // Decode the three other fields: flags, substream id, and length.
            let flags_field =
                u16::from_be_bytes(<[u8; 2]>::try_from(&self.incoming_header[2..4]).unwrap());
            let substream_id_field =
                u32::from_be_bytes(<[u8; 4]>::try_from(&self.incoming_header[4..8]).unwrap());
            let length_field =
                u32::from_be_bytes(<[u8; 4]>::try_from(&self.incoming_header[8..12]).unwrap());

            // Byte 1 of the header indicates the type of message.
            match self.incoming_header[1] {
                2 => {
                    // A ping or pong has been received.
                    // TODO: check flags more strongly?
                    if (flags_field & 0x1) != 0 {
                        // Ping. Write a pong message in `self.pending_out_header`.
                        debug_assert!(self.pending_out_header.is_empty());
                        self.pending_out_header
                            .try_extend_from_slice(
                                &[
                                    0,
                                    2,
                                    0x0,
                                    0x2,
                                    0,
                                    0,
                                    0,
                                    0,
                                    self.incoming_header[8],
                                    self.incoming_header[9],
                                    self.incoming_header[10],
                                    self.incoming_header[11],
                                ][..],
                            )
                            .unwrap();
                        break;
                    }
                }
                3 => {
                    // TODO: go away
                    todo!()
                }
                // Handled below.
                0 | 1 => {}
                _ => return Err(Error::BadFrameType(self.incoming_header[1])),
            }

            // In case of data (`0`) or window size (`1`) frame, check whether the remote has the
            // the rights to send this data.
            let substream_id = match NonZeroU32::new(substream_id_field) {
                Some(i) => SubstreamId(i),
                None => return Err(Error::ZeroSubstreamId),
            };

            todo!()
        }

        todo!()
    }

    pub fn accept_pending_substream(&mut self, user_data: T, mut out: &mut [u8]) -> usize {
        let pending_incoming_substream = self.pending_incoming_substream.take().unwrap();
        debug_assert!(self.pending_out_header.is_empty());

        // TODO: finish

        todo!()
    }

    pub fn reject_pending_substream(&mut self, mut out: &mut [u8]) -> usize {
        let pending_incoming_substream = self.pending_incoming_substream.take().unwrap();
        debug_assert!(self.pending_out_header.is_empty());

        self.pending_out_header.push(0);
        self.pending_out_header.push(1);
        self.pending_out_header
            .try_extend_from_slice(&0x8u16.to_be_bytes()[..])
            .unwrap();
        self.pending_out_header
            .try_extend_from_slice(&pending_incoming_substream.0.get().to_be_bytes()[..])
            .unwrap();
        self.pending_out_header
            .try_extend_from_slice(&0u32.to_be_bytes()[..])
            .unwrap();

        let to_write = cmp::min(self.pending_out_header.len(), out.len());
        out[..to_write].copy_from_slice(&self.pending_out_header[..to_write]);
        for _ in 0..to_write {
            self.pending_out_header.remove(0);
        }
        to_write
    }

    /*pub fn emit_data_frame_header(
        &mut self,
        mut payload: &[u8],
        destination: &mut [u8],
    ) -> (usize, usize) {
        // A header occupies 12 bytes. If the destination isn't capable of holding at least a header
        // and one byte of data, return immediately.
        if destination.len() <= 12 {
            return (0, 0);
        }

        // There's no point in writing empty frames.
        if payload.is_empty() {
            return (0, 0);
        }

        // If the local node isn't allowed to write any more data, return immediately as well.
        if substream.local_write_closed || substream.allowed_window == 0 {
            // TODO: error if local_write_closed instead?
            return (0, 0);
        }

        // Clamp `payload` to the length that is actually going to be emitted.
        payload = {
            let len = cmp::min(
                cmp::min(destination.len() - 12, payload.len()),
                usize::try_from(substream.allowed_window).unwrap_or(usize::max_value()),
            );
            debug_assert_ne!(len, 0);
            &payload[..len]
        };

        // Write the header.
        destination[0] = 0;
        destination[1] = 0;
        destination[2..4].copy_from_slice(&0u16.to_be_bytes());
        // Since the payload is clamped to the allowed window size, which is a u32, it is also
        // guaranteed that `payload.len()` fits in a u32.
        destination[4..8].copy_from_slice(&substream.id.0.get().to_be_bytes());
        destination[8..12].copy_from_slice(&u32::try_from(payload.len()).unwrap().to_be_bytes());

        // Copy the data.
        destination[12..][..payload.len()].copy_from_slice(&payload);

        // Success!
        substream.allowed_window -= u64::try_from(payload.len()).unwrap();
        debug_assert!(payload.len() + 12 <= destination.len());
        (payload.len(), payload.len() + 12)
    }*/
}

impl<T> fmt::Debug for Connection<T>
where
    T: fmt::Debug,
{
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        struct List<'a, T>(&'a Connection<T>);
        impl<'a, T> fmt::Debug for List<'a, T>
        where
            T: fmt::Debug,
        {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.debug_list()
                    .entries(self.0.substreams.values().map(|v| &v.user_data))
                    .finish()
            }
        }

        f.debug_struct("Connection")
            .field("substreams", &List(self))
            .finish()
    }
}

/// Identifier of a substream in the context of a connection.
#[derive(Debug, Copy, Clone, Ord, PartialOrd, Eq, PartialEq, Hash, derive_more::From)]
pub struct SubstreamId(pub NonZeroU32);

pub struct IncomingDataOutcome {
    pub bytes_read: usize,
    pub bytes_written: usize,
    pub ty: IncomingDataTy,
}

pub enum IncomingDataTy {
    IncomingSubstream(SubstreamId),
}

/// Error while decoding the yamux stream.
#[derive(Debug, derive_more::Display)]
pub enum Error {
    /// Unknown version number in a header.
    UnknownVersion(u8),
    /// Unrecognized value for the type of frame as indicated in the header.
    BadFrameType(u8),
    /// Substream ID was zero in a data of window update frame.
    ZeroSubstreamId,
    /// Received unknown substream ID without a SYN flag.
    InvalidSubstreamId(u32),
    /// Remote tried to send more data than it was allowed to.
    CreditsExceeded,
}

/// By default, all new substreams have this implicit window size.
const DEFAULT_FRAME_SIZE: u32 = 256 * 1024;
