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

//! Little Endian Base 128
//!
//! The LEB128 encoding is used throughout the networking code. This module provides utilities for
//! encoding/decoding this format.
//!
//! See [https://en.wikipedia.org/wiki/LEB128].

use alloc::vec::Vec;
use core::{cmp, convert::TryFrom as _, mem};

/// Returns an LEB128-encoded integer as a list of bytes.
///
/// This function accepts as parameter an `Into<u64>`. As such, one can also pass a `u8`, `u16`,
/// or `u32` for example. Use [`encode_usize`] for the `usize` equivalent.
pub fn encode(value: impl Into<u64>) -> impl ExactSizeIterator<Item = u8> + Clone {
    #[derive(Clone)]
    struct EncodeIter {
        value: u64,
        finished: bool,
    }

    impl Iterator for EncodeIter {
        type Item = u8;

        fn next(&mut self) -> Option<Self::Item> {
            if self.finished {
                return None;
            }

            if self.value < (1 << 7) {
                self.finished = true;
                return Some(u8::try_from(self.value).unwrap());
            }

            let ret = (1 << 7) | u8::try_from(self.value & 0b1111111).unwrap();
            self.value >>= 7;
            Some(ret)
        }

        fn size_hint(&self) -> (usize, Option<usize>) {
            let len = self.clone().count();
            (len, Some(len))
        }
    }

    impl ExactSizeIterator for EncodeIter {}

    EncodeIter {
        value: value.into(),
        finished: false,
    }
}

/// Returns an LEB128-encoded `usize` as a list of bytes.
///
/// See also [`encode`].
pub fn encode_usize(value: usize) -> impl ExactSizeIterator<Item = u8> {
    encode(u64::try_from(value).unwrap())
}

// TODO: document all this below

pub enum Framed {
    InProgress(FramedInProgress),
    Finished(Vec<u8>),
}

pub struct FramedInProgress {
    max_len: usize,
    buffer: Vec<u8>,
    inner: FramedInner,
}

enum FramedInner {
    Length,
    Body { expected_len: usize },
}

impl FramedInProgress {
    /// Initializes a new buffer for a frame.
    ///
    /// Must be passed the maximum allowed length of the frame, according to the protocol. This
    /// value is also used as the size to use to pre-allocate the buffer that is later returned
    /// in [`Framed::Finished`].
    pub fn new(max_len: usize) -> Self {
        FramedInProgress {
            max_len,
            buffer: Vec::with_capacity(max_len),
            inner: FramedInner::Length,
        }
    }

    pub fn update(mut self, mut data: &[u8]) -> Result<(usize, Framed), FramedError> {
        fn decode_leb128(buffer: &[u8]) -> Option<Result<usize, FramedError>> {
            let mut out = 0usize;

            for (n, byte) in buffer.iter().enumerate() {
                match usize::from(*byte & 0b1111111).checked_mul(1 << (7 * n)) {
                    Some(o) => out |= o,
                    None => return Some(Err(FramedError::LengthPrefixTooLarge)),
                };

                if (*byte & 0x80) == 0 {
                    assert_eq!(n, buffer.len() - 1);
                    return Some(Ok(out));
                }
            }

            None
        }

        let mut total_read = 0;

        loop {
            match self.inner {
                FramedInner::Length => {
                    if data.is_empty() {
                        return Ok((total_read, Framed::InProgress(self)));
                    }

                    self.buffer.push(data[0]);
                    data = &data[1..];
                    total_read += 1;

                    if self.buffer.len() >= 2 * mem::size_of::<usize>() {
                        return Err(FramedError::LengthPrefixTooLarge);
                    }

                    if let Some(expected_len) = decode_leb128(&self.buffer) {
                        let expected_len = expected_len?;
                        if expected_len > self.max_len {
                            return Err(FramedError::MaxLengthExceeded {
                                max_allowed: self.max_len,
                            });
                        }
                        self.buffer.clear();
                        self.inner = FramedInner::Body { expected_len };
                    }
                }
                FramedInner::Body { expected_len } => {
                    debug_assert!(self.buffer.len() <= expected_len);
                    let missing = expected_len - self.buffer.len();
                    let available = cmp::min(missing, data.len());
                    self.buffer.extend_from_slice(&data[..available]);
                    debug_assert!(self.buffer.len() <= expected_len);
                    total_read += available;

                    if expected_len == self.buffer.len() {
                        return Ok((total_read, Framed::Finished(self.buffer)));
                    } else {
                        return Ok((total_read, Framed::InProgress(self)));
                    }
                }
            }
        }
    }
}

/// Error potentially returned by [`FramedInProgress::update`].
#[derive(Debug, derive_more::Display)]
pub enum FramedError {
    /// The variable-length prefix is too large and cannot possibly represent a valid size.
    LengthPrefixTooLarge,
    /// Maximum length of the frame has been exceeded.
    MaxLengthExceeded {
        /// Maximum number of bytes allowed.
        max_allowed: usize,
    },
}

#[cfg(test)]
mod tests {
    #[test]
    fn basic_encode() {
        let obtained = super::encode(0x123456789abcdefu64).collect::<Vec<_>>();
        assert_eq!(obtained, &[239, 155, 175, 205, 248, 172, 209, 145, 1]);
    }

    #[test]
    fn encode_zero() {
        let obtained = super::encode(0u64).collect::<Vec<_>>();
        assert_eq!(obtained, &[0x0u8]);
    }

    #[test]
    fn exact_size_iterator() {
        for _ in 0..128 {
            let iter = super::encode(rand::random::<u64>());
            let expected = iter.len();
            let obtained = iter.count();
            assert_eq!(expected, obtained);
        }
    }

    // TODO: more tests
}
