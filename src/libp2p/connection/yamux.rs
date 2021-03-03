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
//! The [`Yamux`] object holds the state of all yamux-specific information, and the list of
//! all currently-open substreams.
//!
//! Call [`Yamux::incoming_data`] when data is available on the socket. This function parses
//! the received data, updates the internal state machine, and possibly returns an
//! [`IncomingDataDetail`].
//! Call [`Yamux::extract_out`] when the remote is ready to accept more data.
//!
//! The generic parameter of [`Yamux`] is an opaque "user data" associated to each substream.
//!
//! When [`SubstreamMut::write`] is called, the buffer of data to send out is stored within the
//! [`Yamux`] object. This data will then be progressively returned by
//! [`Yamux::extract_out`].
//!
//! It is the responsibility of the user to enforce a bound to the amount of enqueued data, as
//! the [`Yamux`] itself doesn't enforce any limit. Enforcing such a bound must be done based
//! on the logic of the higher-level protocols. Failing to do so might lead to potential DoS
//! attack vectors.

// TODO: write example

// TODO: the code of this module is rather complicated; either simplify it or write a lot of tests, including fuzzing tests

use alloc::vec::Vec;
use core::{cmp, convert::TryFrom as _, fmt, mem, num::NonZeroU32};
use hashbrown::hash_map::{Entry, OccupiedEntry};

/// Name of the protocol, typically used when negotiated it using *multistream-select*.
pub const PROTOCOL_NAME: &str = "/yamux/1.0.0";

pub struct Yamux<T> {
    /// List of substreams currently open in the yamux state machine.
    ///
    /// A SipHasher is used in order to avoid hash collision attacks on substream IDs.
    substreams: hashbrown::HashMap<NonZeroU32, Substream<T>, ahash::RandomState>,

    /// What kind of data is expected on the socket next.
    incoming: Incoming,

    /// Id of the next outgoing substream to open.
    /// This implementation allocates identifiers linearly. Every time a substream is open, its
    /// value is incremented by two.
    next_outbound_substream: NonZeroU32,

    /// Header currently being written out. Finishing to write this header is the first and
    /// foremost priority of [`Yamux::extract_out`].
    pending_out_header: arrayvec::ArrayVec<[u8; 12]>,

    /// If `Some`, contains a substream ID and a number of bytes. A data frame header has been
    /// written to the socket, and the number of bytes stored in there is the number of bytes
    /// remaining in this frame.
    ///
    /// Writing out the data of this substream is the second most highest priority after writing
    /// out [`Yamux::pending_out_header`].
    writing_out_substream: Option<(SubstreamId, usize)>,
}

struct Substream<T> {
    /// True if a message on this substream has already been sent since it has been opened. The
    /// first message on a substream must contain either a SYN or ACK flag.
    first_message_queued: bool,
    /// Amount of data the remote is allowed to transmit to the local node.
    remote_allowed_window: u64,
    /// Amount of data the local node is allowed to transmit to the remote.
    allowed_window: u64,
    /// True if the writing side of the local node is closed for this substream.
    /// Note that the data queued in [`Substream::write_buffers`] must still be sent out,
    /// alongside with a frame with a FIN flag.
    local_write_closed: bool,
    /// True if the writing side of the remote node is closed for this substream.
    remote_write_closed: bool,
    /// Buffer of buffers to be written out to the socket.
    // TODO: is it a good idea to have an unbounded Vec?
    // TODO: call shrink_to_fit from time to time?
    write_buffers: Vec<Vec<u8>>,
    /// Number of bytes in `self.write_buffers[0]` has have already been written out to the
    /// socket.
    first_write_buffer_offset: usize,
    /// Data chosen by the user.
    user_data: T,
}

enum Incoming {
    /// Expect a header. The field might contain some already-read bytes.
    Header(arrayvec::ArrayVec<[u8; 12]>),
    /// Expect the data of a previously-received data frame header.
    DataFrame {
        /// Identifier of the substream the data belongs to.
        substream_id: SubstreamId,
        /// Number of bytes of data remaining before the frame ends.
        remaining_bytes: u32,
        /// True if the remote writing side of the substream should be closed after receiving the
        /// data frame.
        fin: bool,
    },
    /// A header referring to a new substream has been received. The reception of any further data
    /// is blocked waiting for the API user to accept or reject this substream.
    PendingIncomingSubstream {
        /// Identifier of the pending substream.
        substream_id: SubstreamId,
        /// Extra local window size to give to this substream.
        extra_window: u32,
        /// If non-zero, must transition to a [`Incoming::DataFrame`].
        data_frame_size: u32,
        /// True if the remote writing side of the substream should be closed after receiving the
        /// `data_frame_size` bytes.
        fin: bool,
    },
}

impl<T> Yamux<T> {
    /// Initializes a new yamux state machine.
    pub fn new(config: Config) -> Yamux<T> {
        Yamux {
            substreams: hashbrown::HashMap::with_capacity_and_hasher(
                config.capacity,
                ahash::RandomState::with_seeds(
                    config.randomness_seed.0,
                    config.randomness_seed.1,
                    config.randomness_seed.2,
                    config.randomness_seed.3,
                ),
            ),
            incoming: Incoming::Header(arrayvec::ArrayVec::new()),
            next_outbound_substream: if config.is_initiator {
                NonZeroU32::new(1).unwrap()
            } else {
                NonZeroU32::new(2).unwrap()
            },
            pending_out_header: arrayvec::ArrayVec::new(),
            writing_out_substream: None,
        }
    }

    /// Opens a new substream.
    ///
    /// This method only modifies the state of `self` and reserves an identifier. No message needs
    /// to be sent to the remote before data is actually being sent on the substream.
    ///
    /// > **Note**: Importantly, the remote will not be notified of the substream being open
    /// >           before the local side sends data on this substream. As such, protocols where
    /// >           the remote is expected to send data in response to a substream being open,
    /// >           without the local side first sending some data on that substream, will not
    /// >           work. In practice, while this is technically out of concern of the yamux
    /// >           protocol, all substreams in the context of libp2p start with a
    /// >           multistream-select negotiation, and this scenario can therefore never happen.
    ///
    /// # Panic
    ///
    /// Panics if all possible substream IDs are already taken. This happen if there exists more
    /// than approximately 2^31 substreams, which is very unlikely to happen unless there exists a
    /// bug in the code.
    ///
    pub fn open_substream(&mut self, user_data: T) -> SubstreamMut<T> {
        // Make sure that the `loop` below can finish.
        assert!(usize::try_from(u32::max_value() / 2 - 1)
            .map_or(true, |full_len| self.substreams.len() < full_len));

        // Grab a `VacantEntry` in `self.substreams`.
        let entry = loop {
            // Allocating a substream ID is surprisingly difficult because overflows in the
            // identifier are possible if the software runs for a very long time.
            // Rather than naively incrementing the id by two and assuming that no substream with
            // this ID exists, the code below properly handles wrapping around and ignores IDs
            // already in use .
            let id_attempt = self.next_outbound_substream;
            self.next_outbound_substream = {
                let mut id = self.next_outbound_substream.get();
                loop {
                    // Odd ids are reserved for the initiator and even ids are reserved for the
                    // listener. Assuming that the current id is valid, incrementing by 2 will
                    // lead to a valid id as well.
                    id = id.wrapping_add(2);
                    // However, the substream ID `0` is always invalid.
                    match NonZeroU32::new(id) {
                        Some(v) => break v,
                        None => continue,
                    }
                }
            };
            if let Entry::Vacant(e) = self.substreams.entry(id_attempt) {
                break e;
            }
        };

        // ID that was just allocated.
        let substream_id = SubstreamId(*entry.key());

        entry.insert(Substream {
            first_message_queued: false,
            remote_allowed_window: DEFAULT_FRAME_SIZE,
            allowed_window: DEFAULT_FRAME_SIZE,
            local_write_closed: false,
            remote_write_closed: false,
            write_buffers: Vec::with_capacity(16),
            first_write_buffer_offset: 0,
            user_data,
        });

        match self.substreams.entry(substream_id.0) {
            Entry::Occupied(e) => SubstreamMut { substream: e },
            _ => unreachable!(),
        }
    }

    /// Returns an iterator to the list of all substream user datas.
    pub fn user_datas(&self) -> impl ExactSizeIterator<Item = (SubstreamId, &T)> {
        self.substreams
            .iter()
            .map(|(id, s)| (SubstreamId(*id), &s.user_data))
    }

    /// Returns a reference to a substream by its ID. Returns `None` if no substream with this ID
    /// is open.
    pub fn substream_by_id(&mut self, id: SubstreamId) -> Option<SubstreamMut<T>> {
        if let Entry::Occupied(e) = self.substreams.entry(id.0) {
            Some(SubstreamMut { substream: e })
        } else {
            None
        }
    }

    /// Process some incoming data.
    ///
    /// # Panic
    ///
    /// Panics if pending incoming substream.
    ///
    // TODO: explain that reading might be blocked on writing
    // TODO: reword panic reason
    pub fn incoming_data(mut self, mut data: &[u8]) -> Result<IncomingDataOutcome<T>, Error> {
        let mut total_read: usize = 0;

        while !data.is_empty() {
            match self.incoming {
                Incoming::PendingIncomingSubstream { .. } => panic!(),

                Incoming::DataFrame {
                    substream_id,
                    remaining_bytes: 0,
                    fin: true,
                } => {
                    self.incoming = Incoming::Header(arrayvec::ArrayVec::new());

                    let substream = match self.substreams.get_mut(&substream_id.0) {
                        Some(s) => s,
                        None => continue,
                    };

                    substream.remote_write_closed = true;

                    if substream.local_write_closed {
                        let user_data = self.substreams.remove(&substream_id.0).unwrap().user_data;
                        return Ok(IncomingDataOutcome {
                            yamux: self,
                            bytes_read: total_read,
                            detail: Some(IncomingDataDetail::StreamClosed {
                                substream_id,
                                user_data: Some(user_data),
                            }),
                        });
                    } else {
                        return Ok(IncomingDataOutcome {
                            yamux: self,
                            bytes_read: total_read,
                            detail: Some(IncomingDataDetail::StreamClosed {
                                substream_id,
                                user_data: None,
                            }),
                        });
                    }
                }

                Incoming::DataFrame {
                    substream_id,
                    ref mut remaining_bytes,
                    fin,
                } => {
                    let pulled_data = cmp::min(
                        *remaining_bytes,
                        u32::try_from(data.len()).unwrap_or(u32::max_value()),
                    );

                    let pulled_data_usize = usize::try_from(pulled_data).unwrap();
                    *remaining_bytes -= pulled_data;

                    let start_offset = total_read;
                    total_read += pulled_data_usize;
                    data = &data[pulled_data_usize..];

                    if let Some(substream) = self.substreams.get_mut(&substream_id.0) {
                        debug_assert!(!substream.remote_write_closed);
                        if *remaining_bytes == 0 {
                            if fin {
                                // If `fin`, leave `incoming` as `DataFrame`, so that it gets
                                // picked at the next iteration and a `StreamClosed` gets
                                // returned.
                                substream.remote_write_closed = true;
                            } else {
                                self.incoming = Incoming::Header(arrayvec::ArrayVec::new());
                            }
                        }

                        return Ok(IncomingDataOutcome {
                            yamux: self,
                            bytes_read: total_read,
                            detail: Some(IncomingDataDetail::DataFrame {
                                substream_id,
                                start_offset,
                            }),
                        });
                    } else {
                        if *remaining_bytes == 0 {
                            self.incoming = Incoming::Header(arrayvec::ArrayVec::new());
                        }
                        continue;
                    }
                }

                Incoming::Header(ref mut incoming_header) => {
                    // The code below might require writing to it, and as such we can't proceed
                    // with any reading if it isn't empty.
                    if !self.pending_out_header.is_empty() {
                        break;
                    }

                    // Try to copy as much as possible from `data` to `incoming_header`.
                    while !data.is_empty() && incoming_header.len() < 12 {
                        incoming_header.push(data[0]);
                        total_read += 1;
                        data = &data[1..];
                    }

                    // Not enough data to finish receiving header. Nothing more can be done.
                    if incoming_header.len() != 12 {
                        debug_assert!(data.is_empty());
                        break;
                    }

                    // Full header available to decode in `incoming_header`.

                    // Byte 0 of the header is the yamux version number. Return an error if it
                    // isn't 0.
                    if incoming_header[0] != 0 {
                        return Err(Error::UnknownVersion(incoming_header[0]));
                    }

                    // Decode the three other fields: flags, substream id, and length.
                    let flags_field =
                        u16::from_be_bytes(<[u8; 2]>::try_from(&incoming_header[2..4]).unwrap());
                    let substream_id_field =
                        u32::from_be_bytes(<[u8; 4]>::try_from(&incoming_header[4..8]).unwrap());
                    let length_field =
                        u32::from_be_bytes(<[u8; 4]>::try_from(&incoming_header[8..12]).unwrap());

                    if (flags_field & !0b1111) != 0 {
                        return Err(Error::UnknownFlags(flags_field));
                    }

                    // Byte 1 of the header indicates the type of message.
                    match incoming_header[1] {
                        2 => {
                            // A ping or pong has been received.
                            if (flags_field & 0b1) != 0 {
                                if (flags_field & 0b1110) != 0 {
                                    return Err(Error::BadPingFlags(flags_field));
                                }

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
                                            incoming_header[8],
                                            incoming_header[9],
                                            incoming_header[10],
                                            incoming_header[11],
                                        ][..],
                                    )
                                    .unwrap();
                                self.incoming = Incoming::Header(arrayvec::ArrayVec::new());
                                continue;
                            // TODO: pong handling
                            } else {
                                return Err(Error::BadPingFlags(flags_field));
                            }
                        }
                        3 => {
                            // TODO: go away
                            todo!()
                        }
                        // Handled below.
                        0 | 1 => {}
                        _ => return Err(Error::BadFrameType(incoming_header[1])),
                    }

                    // The frame is now either a data (`0`) or window size (`1`) frame.
                    let substream_id = match NonZeroU32::new(substream_id_field) {
                        Some(i) => SubstreamId(i),
                        None => return Err(Error::ZeroSubstreamId),
                    };

                    // Handle `RST` flag separately.
                    if (flags_field & 0x8) != 0 {
                        if incoming_header[1] == 0 && length_field != 0 {
                            return Err(Error::DataWithRst);
                        }

                        self.incoming = Incoming::Header(arrayvec::ArrayVec::new());

                        if let Some(removed) = self.substreams.remove(&substream_id.0) {
                            return Ok(IncomingDataOutcome {
                                yamux: self,
                                bytes_read: total_read,
                                detail: Some(IncomingDataDetail::StreamReset {
                                    substream_id,
                                    user_data: removed.user_data,
                                }),
                            });
                        } else {
                            // The remote might have sent a RST frame concerning a substream for
                            // which we have sent a RST frame earlier. Considering that we don't
                            // keep traces of old substreams, we have no way to know whether this
                            // is the case or not.
                            continue;
                        }
                    }

                    // Find the element in `self.substreams` corresponding to the substream
                    // requested by the remote.
                    // It is possible that the remote is referring to a substream for which a RST
                    // has been sent out. Since the local state machine doesn't keep track of
                    // RST'ted substreams, any frame concerning a substream with an unknown id is
                    // discarded and doesn't result in an error, under the presumption that we are
                    // in this situation. When that is the case, the `substream` variable below is
                    // `None`.
                    let substream: Option<_> = if (flags_field & 0x1) == 0 {
                        self.substreams.get_mut(&substream_id.0)
                    } else {
                        // Remote has sent a SYN flag.
                        if self.substreams.contains_key(&substream_id.0) {
                            return Err(Error::UnexpectedSyn(substream_id.0));
                        } else {
                            self.incoming = Incoming::PendingIncomingSubstream {
                                substream_id,
                                extra_window: if incoming_header[1] == 1 {
                                    length_field
                                } else {
                                    0
                                },
                                data_frame_size: if incoming_header[1] == 0 {
                                    length_field
                                } else {
                                    0
                                },
                                fin: (flags_field & 0x4) != 0,
                            };

                            return Ok(IncomingDataOutcome {
                                yamux: self,
                                bytes_read: total_read,
                                detail: Some(IncomingDataDetail::IncomingSubstream),
                            });
                        }
                    };

                    if incoming_header[1] == 0 {
                        // Data frame.
                        // Check whether the remote has the right to send that much data.
                        // Note that the credits aren't checked in the case of an unknown
                        // substream.
                        if let Some(substream) = substream {
                            if substream.remote_write_closed {
                                return Err(Error::WriteAfterFin);
                            }

                            substream.remote_allowed_window = substream
                                .remote_allowed_window
                                .checked_sub(u64::from(length_field))
                                .ok_or(Error::CreditsExceeded)?;

                            // TODO: temporary hack
                            let syn_ack_flag = !substream.first_message_queued;
                            substream.first_message_queued = true;
                            substream.remote_allowed_window += 256 * 1024;
                            self.queue_window_size_frame_header(
                                syn_ack_flag,
                                substream_id.0,
                                256 * 1024,
                            );
                        }

                        self.incoming = Incoming::DataFrame {
                            substream_id,
                            remaining_bytes: length_field,
                            fin: (flags_field & 0x4) != 0,
                        };
                    } else if incoming_header[1] == 1 {
                        // Window size frame.
                        self.incoming = Incoming::Header(arrayvec::ArrayVec::new());

                        if let Some(substream) = substream {
                            // Note that the specs are a unclear about whether the remote can or
                            // should continue sending FIN flags on window size frames after their
                            // side of the substream has already been closed before.
                            if (flags_field & 0x4) != 0 {
                                self.incoming = Incoming::DataFrame {
                                    substream_id,
                                    remaining_bytes: 0,
                                    fin: true,
                                };
                            }

                            substream.allowed_window = substream
                                .allowed_window
                                .checked_add(u64::from(length_field))
                                .ok_or(Error::LocalCreditsOverflow)?;
                        }
                    } else {
                        unreachable!()
                    }
                }
            }
        }

        Ok(IncomingDataOutcome {
            yamux: self,
            bytes_read: total_read,
            detail: None,
        })
    }

    /// Returns an object that provides an iterator to a list of buffers whose content must be
    /// sent out on the socket.
    ///
    /// The buffers produced by the iterator will never yield more than `size_bytes` bytes of
    /// data. The user is expected to pass an exact amount of bytes that the next layer is ready
    /// to accept.
    ///
    /// After the [`ExtractOut`] has been destroyed, the yamux state machine will automatically
    /// consider that these `size_bytes` have been sent out, even if the iterator has been
    /// destroyed before finishing. It is a logic error to `mem::forget` the [`ExtractOut`].
    ///
    /// > **Note**: Most other objects in the networking code have a "`read_write`" method that
    /// >           writes the outgoing data to a buffer. This is an idiomatic way to do things in
    /// >           situations where the data is generated on the fly. In the context of yamux,
    /// >           however, this would be rather suboptimal considering that buffers to send out
    /// >           are already stored in their final form in the state machine.
    pub fn extract_out(&mut self, size_bytes: usize) -> ExtractOut<T> {
        // TODO: this function has a zero-cost API, but its body isn't really zero-cost due to laziness

        // The implementation consists in filling a buffer of buffers, then calling `into_iter`.
        let mut buffers = Vec::with_capacity(32);

        // Copy of `size_bytes`, decremented over the iterations.
        let mut size_bytes_iter = size_bytes;

        while size_bytes_iter != 0 {
            // Finish writing `self.pending_out_header` if possible.
            if !self.pending_out_header.is_empty() {
                if size_bytes_iter >= self.pending_out_header.len() {
                    size_bytes_iter -= self.pending_out_header.len();
                    buffers.push(either::Left(mem::take(&mut self.pending_out_header)));
                } else {
                    let to_add = self.pending_out_header[..size_bytes_iter].to_vec();
                    for _ in 0..size_bytes_iter {
                        self.pending_out_header.remove(0);
                    }
                    buffers.push(either::Right(VecWithOffset(to_add, 0)));
                    break;
                }
            }

            // Now update `writing_out_substream`.
            if let Some((substream, ref mut remain)) = self.writing_out_substream {
                let mut substream = self.substreams.get_mut(&substream.0).unwrap();

                let first_buf_avail =
                    substream.write_buffers[0].len() - substream.first_write_buffer_offset;
                if first_buf_avail <= *remain && first_buf_avail <= size_bytes_iter {
                    buffers.push(either::Right(VecWithOffset(
                        substream.write_buffers.remove(0),
                        substream.first_write_buffer_offset,
                    )));
                    size_bytes_iter -= first_buf_avail;
                    substream.first_write_buffer_offset = 0;
                    *remain -= first_buf_avail;
                    if *remain == 0 {
                        self.writing_out_substream = None;
                    }
                } else if *remain <= size_bytes_iter {
                    size_bytes_iter -= *remain;
                    buffers.push(either::Right(VecWithOffset(
                        substream.write_buffers[0][substream.first_write_buffer_offset..]
                            [..*remain]
                            .to_vec(),
                        0,
                    )));
                    substream.first_write_buffer_offset += *remain;
                    self.writing_out_substream = None;
                } else {
                    buffers.push(either::Right(VecWithOffset(
                        substream.write_buffers[0][substream.first_write_buffer_offset..]
                            [..size_bytes_iter]
                            .to_vec(),
                        0,
                    )));
                    substream.first_write_buffer_offset += size_bytes_iter;
                    *remain -= size_bytes_iter;
                    size_bytes_iter = 0;
                }

                continue;
            }

            // All frames in the process of being written have been written.
            debug_assert!(self.pending_out_header.is_empty());
            debug_assert!(self.writing_out_substream.is_none());

            // Start writing more data from another substream.
            // TODO: choose substreams in some sort of round-robin way
            if let Some((id, sub)) = self
                .substreams
                .iter_mut()
                .find(|(_, s)| !s.write_buffers.is_empty())
                .map(|(id, sub)| (*id, sub))
            {
                let pending_len = sub.write_buffers.iter().fold(0, |l, b| l + b.len());
                let len_out = cmp::min(
                    u32::try_from(pending_len).unwrap_or(u32::max_value()),
                    u32::try_from(sub.allowed_window).unwrap_or(u32::max_value()),
                );
                let len_out_usize = usize::try_from(len_out).unwrap();
                sub.allowed_window -= u64::from(len_out);
                let syn_ack_flag = !sub.first_message_queued;
                sub.first_message_queued = true;
                let fin_flag = sub.local_write_closed && len_out_usize == pending_len;
                self.writing_out_substream = Some((SubstreamId(id), len_out_usize));
                self.queue_data_frame_header(syn_ack_flag, fin_flag, id, len_out);
            } else {
                break;
            }
        }

        debug_assert!(
            buffers
                .iter()
                .fold(0, |n, b| n + AsRef::<[u8]>::as_ref(b).len())
                <= size_bytes
        );

        ExtractOut {
            connection: self,
            buffers: Some(buffers),
        }
    }

    pub fn accept_pending_substream(&mut self, user_data: T) -> SubstreamMut<T> {
        match self.incoming {
            Incoming::PendingIncomingSubstream {
                substream_id,
                extra_window,
                data_frame_size,
                fin,
            } => {
                let _was_before = self.substreams.insert(
                    substream_id.0,
                    Substream {
                        first_message_queued: false,
                        remote_allowed_window: DEFAULT_FRAME_SIZE,
                        allowed_window: DEFAULT_FRAME_SIZE + u64::from(extra_window),
                        local_write_closed: false,
                        remote_write_closed: data_frame_size == 0 && fin,
                        write_buffers: Vec::new(),
                        first_write_buffer_offset: 0,
                        user_data,
                    },
                );
                debug_assert!(_was_before.is_none());

                self.incoming = if data_frame_size == 0 {
                    Incoming::Header(arrayvec::ArrayVec::new())
                } else {
                    Incoming::DataFrame {
                        substream_id,
                        remaining_bytes: data_frame_size,
                        fin,
                    }
                };

                SubstreamMut {
                    substream: match self.substreams.entry(substream_id.0) {
                        Entry::Occupied(e) => e,
                        _ => unreachable!(),
                    },
                }
            }
            _ => panic!(),
        }
    }

    pub fn reject_pending_substream(&mut self) {
        /*self.pending_out_header.push(0);
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
        to_write*/
        // TODO:
        todo!()
    }

    /// Writes a data frame header in `self.pending_out_header`.
    ///
    /// # Panic
    ///
    /// Panics if `!self.pending_out_header.is_empty()`.
    ///
    fn queue_data_frame_header(
        &mut self,
        syn_ack_flag: bool,
        fin_flag: bool,
        substream_id: NonZeroU32,
        data_length: u32,
    ) {
        assert!(self.pending_out_header.is_empty());

        let mut flags: u16 = 0;
        if syn_ack_flag {
            if (substream_id.get() % 2) == (self.next_outbound_substream.get() % 2) {
                // SYN
                flags |= 0x1;
            } else {
                // ACK
                flags |= 0x2;
            }
        }
        if fin_flag {
            flags |= 0x4;
        }

        self.pending_out_header.push(0);
        self.pending_out_header.push(0);
        self.pending_out_header
            .try_extend_from_slice(&flags.to_be_bytes())
            .unwrap();
        self.pending_out_header
            .try_extend_from_slice(&substream_id.get().to_be_bytes())
            .unwrap();
        self.pending_out_header
            .try_extend_from_slice(&data_length.to_be_bytes())
            .unwrap();

        debug_assert_eq!(self.pending_out_header.len(), 12);
    }

    /// Writes a window size update frame header in `self.pending_out_header`.
    ///
    /// # Panic
    ///
    /// Panics if `!self.pending_out_header.is_empty()`.
    ///
    fn queue_window_size_frame_header(
        &mut self,
        syn_ack_flag: bool,
        substream_id: NonZeroU32,
        window_size: u32,
    ) {
        assert!(self.pending_out_header.is_empty());

        let mut flags: u16 = 0;
        if syn_ack_flag {
            if (substream_id.get() % 2) == (self.next_outbound_substream.get() % 2) {
                // SYN
                flags |= 0x1;
            } else {
                // ACK
                flags |= 0x2;
            }
        }

        self.pending_out_header.push(0);
        self.pending_out_header.push(1);
        self.pending_out_header
            .try_extend_from_slice(&flags.to_be_bytes())
            .unwrap();
        self.pending_out_header
            .try_extend_from_slice(&substream_id.get().to_be_bytes())
            .unwrap();
        self.pending_out_header
            .try_extend_from_slice(&window_size.to_be_bytes())
            .unwrap();

        debug_assert_eq!(self.pending_out_header.len(), 12);
    }
}

impl<T> fmt::Debug for Yamux<T>
where
    T: fmt::Debug,
{
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        struct List<'a, T>(&'a Yamux<T>);
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

        f.debug_struct("Yamux")
            .field("substreams", &List(self))
            .finish()
    }
}

/// Configuration for a new [`Yamux`].
#[derive(Debug)]
pub struct Config {
    /// `true` if the local machine has initiated the connection. Otherwise, `false`.
    pub is_initiator: bool,
    /// Expected number of substreams simultaneously open, both inbound and outbound substreams
    /// combined.
    pub capacity: usize,
    /// Seed used for the randomness. Used to avoid HashDos attack and determines the order in
    /// which the data on substreams is sent out.
    pub randomness_seed: (u64, u64, u64, u64),
}

/// Reference to a substream within the [`Yamux`].
// TODO: Debug
pub struct SubstreamMut<'a, T> {
    substream: OccupiedEntry<'a, NonZeroU32, Substream<T>, ahash::RandomState>,
}

impl<'a, T> SubstreamMut<'a, T> {
    /// Identifier of the substream.
    pub fn id(&self) -> SubstreamId {
        SubstreamId(*self.substream.key())
    }

    /// Returns the user data associated to this substream.
    pub fn user_data(&mut self) -> &mut T {
        &mut self.substream.get_mut().user_data
    }

    /// Returns the user data associated to this substream.
    pub fn into_user_data(self) -> &'a mut T {
        &mut self.substream.into_mut().user_data
    }

    /// Appends data to the buffer of data to send out on this substream.
    ///
    /// # Panic
    ///
    /// Panics if [`SubstreamMut::close`] has already been called on this substream.
    ///
    pub fn write(&mut self, data: Vec<u8>) {
        let substream = self.substream.get_mut();
        assert!(!substream.local_write_closed);
        debug_assert!(
            !substream.write_buffers.is_empty() || substream.first_write_buffer_offset == 0
        );
        substream.write_buffers.push(data);
    }

    /// Returns the number of bytes queued for writing on this substream.
    pub fn queued_bytes(&self) -> usize {
        let substream = self.substream.get();
        substream
            .write_buffers
            .iter()
            .fold(0, |n, buf| n + buf.len())
    }

    /// Marks the substream as closed. It is no longer possible to write data on it.
    ///
    /// If the remote writing side is still open, this method returns `None` and the remote can
    /// continue to send data.
    ///
    /// If the remote writing side is already closed, this method returns `Some` with the user
    /// data, and the substream is now destroyed.
    ///
    /// # Panic
    ///
    /// Panics if [`SubstreamMut::close`] has already been called on this substream.
    ///
    pub fn close(mut self) -> Option<T> {
        let substream = self.substream.get_mut();
        assert!(!substream.local_write_closed);
        substream.local_write_closed = true;
        // TODO: what is write_buffers is empty? need to send the close frame

        if substream.remote_write_closed {
            Some(self.substream.remove().user_data)
        } else {
            None
        }
    }

    /// Abruptly shuts down the substream. Its identifier is now invalid. Sends a frame with the
    /// `RST` flag to the remote.
    ///
    /// Use this method when a protocol error happens on a substream.
    pub fn reset(self) -> T {
        let value = self.substream.remove();
        // TODO: finish
        value.user_data
    }
}

pub struct ExtractOut<'a, T> {
    connection: &'a mut Yamux<T>,
    buffers: Option<Vec<either::Either<arrayvec::ArrayVec<[u8; 12]>, VecWithOffset>>>,
}

impl<'a, T> ExtractOut<'a, T> {
    /// Returns the list of buffers to write.
    ///
    /// Can only be called once.
    ///
    /// # Panic
    ///
    /// Panics if called multiple times.
    ///
    pub fn buffers(&mut self) -> impl Iterator<Item = impl AsRef<[u8]> + 'a> + 'a {
        self.buffers.take().unwrap().into_iter()
    }
}

#[derive(Clone)]
struct VecWithOffset(Vec<u8>, usize);
impl AsRef<[u8]> for VecWithOffset {
    fn as_ref(&self) -> &[u8] {
        &self.0[self.1..]
    }
}

/// Identifier of a substream in the context of a connection.
#[derive(Debug, Copy, Clone, Ord, PartialOrd, Eq, PartialEq, Hash, derive_more::From)]
pub struct SubstreamId(NonZeroU32);

#[must_use]
#[derive(Debug)]
pub struct IncomingDataOutcome<T> {
    /// Yamux object on which [`Yamux::incoming_data`] has been called.
    pub yamux: Yamux<T>,
    /// Number of bytes read from the incoming buffer. These bytes should no longer be present the
    /// next time [`Yamux::incoming_data`] is called.
    pub bytes_read: usize,
    /// Detail about the incoming data. `None` if nothing of interest has happened.
    pub detail: Option<IncomingDataDetail<T>>,
}

/// Details about the incoming data.
#[must_use]
#[derive(Debug)]
pub enum IncomingDataDetail<T> {
    /// Remote has requested to open a new substream.
    ///
    /// After this has been received, either [`Yamux::accept_pending_substream`] or
    /// [`Yamux::reject_pending_substream`] needs to be called in order to accept or reject
    /// this substream. Calling [`Yamux::incoming_data`] before this is done will lead to a
    /// panic.
    IncomingSubstream,
    /// Received data corresponding to a substream.
    DataFrame {
        /// Offset in the buffer passed to [`Yamux::incoming_data`] where the data frame
        /// starts. The data frame ends at the offset of [`IncomingDataOutcome::bytes_read`].
        start_offset: usize,
        /// Substream the data belongs to. Guaranteed to be valid.
        substream_id: SubstreamId,
    },
    /// Remote has closed its writing side of the substream.
    StreamClosed {
        /// Substream that got closed.
        substream_id: SubstreamId,
        /// If `None`, the local writing side is still open and needs to be closed. If `Some`, the
        /// local writing side is already closed and the substream is now considered destroyed.
        user_data: Option<T>,
    },
    /// Remote has asked to reset a substream.
    ///
    /// The substream is now considered destroyed.
    StreamReset {
        /// Substream that has been destroyed. No longer valid.
        substream_id: SubstreamId,
        /// User data that was associated to this substream.
        user_data: T,
    },
}

/// Error while decoding the yamux stream.
#[derive(Debug, derive_more::Display)]
pub enum Error {
    /// Unknown version number in a header.
    UnknownVersion(u8),
    /// Unrecognized value for the type of frame as indicated in the header.
    BadFrameType(u8),
    /// Received flags whose meaning is unknown.
    UnknownFlags(u16),
    /// Received a PING frame with invalid flags.
    BadPingFlags(u16),
    /// Substream ID was zero in a data of window update frame.
    ZeroSubstreamId,
    /// Received a SYN flag with a known substream ID.
    UnexpectedSyn(NonZeroU32),
    /// Remote tried to send more data than it was allowed to.
    CreditsExceeded,
    /// Number of credits allocated to the local node has overflowed.
    LocalCreditsOverflow,
    /// Remote sent additional data on a substream after having sent the FIN flag.
    WriteAfterFin,
    /// Remote has sent a data frame containing data at the same time as a RST flag.
    DataWithRst,
}

/// By default, all new substreams have this implicit window size.
const DEFAULT_FRAME_SIZE: u64 = 256 * 1024;
