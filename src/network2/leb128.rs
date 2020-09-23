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

//! Little Endian Base 128
//!
//! The LEB128 encoding is used throughout the networking code.
//!
//! See https://en.wikipedia.org/wiki/LEB128

// TODO: better doc

use core::{cmp, convert::TryFrom as _, fmt, iter, mem, ops::Deref};

/// Returns an LEB128-encoded integer as a list of bytes.
// TODO: could be an ExactSizeIterator
pub fn encode(value: impl Into<u64>) -> impl Iterator<Item = u8> {
    let mut value = value.into();
    let mut finished = false;

    iter::from_fn(move || {
        if finished {
            return None;
        }

        if value < (1 << 7) {
            finished = true;
            return Some(u8::try_from(value).unwrap());
        }

        let ret = (1 << 7) | u8::try_from(value & 0b1111111).unwrap();
        value >>= 7;
        Some(ret)
    })
}

/// Returns an LEB128-encoded `usize` as a list of bytes.
pub fn encode_usize(value: usize) -> impl Iterator<Item = u8> {
    encode(u64::try_from(value).unwrap())
}

// TODO: document all this

pub struct Framed {
    max_len: usize,
    buffer: Vec<u8>,
    inner: FramedInner,
}

enum FramedInner {
    Length,
    Body { expected_len: usize },
}

impl Framed {
    pub fn new(max_len: usize) -> Self {
        Framed {
            max_len,
            buffer: Vec::with_capacity(max_len),
            inner: FramedInner::Length,
        }
    }

    pub fn take_frame(&mut self) -> Option<FrameDrain> {
        match self.inner {
            FramedInner::Body { expected_len } if expected_len == self.buffer.len() => {
                Some(FrameDrain(self))
            }
            _ => None,
        }
    }

    // TODO: corruption state after error returned?
    pub fn inject_data(&mut self, mut data: &[u8]) -> Result<usize, FramedError> {
        let mut total_read = 0;

        loop {
            match self.inner {
                FramedInner::Length => {
                    if data.is_empty() {
                        return Ok(total_read);
                    }

                    self.buffer.push(data[0]);
                    data = &data[1..];
                    total_read += 1;

                    if self.buffer.len() >= mem::size_of::<usize>() {
                        return Err(FramedError::LengthPrefixTooLarge);
                    }

                    if let Some(expected_len) = decode(&self.buffer) {
                        let expected_len = expected_len?;
                        if expected_len > self.max_len {
                            return Err(FramedError::LengthPrefixTooLarge);
                        }
                        self.buffer.clear();
                        self.inner = FramedInner::Body { expected_len };
                    }
                }
                FramedInner::Body { expected_len } => {
                    let missing = expected_len - self.buffer.len();
                    let available = cmp::min(missing, data.len());
                    self.buffer.extend_from_slice(&data[..available]);
                    debug_assert!(self.buffer.len() <= expected_len);
                    total_read += available;
                    return Ok(total_read);
                }
            }
        }
    }
}

fn decode(buffer: &[u8]) -> Option<Result<usize, FramedError>> {
    let mut out = 0usize;

    for (n, byte) in buffer.iter().enumerate() {
        out = match out.checked_mul(1 << 7) {
            Some(o) => o,
            None => return Some(Err(FramedError::LengthPrefixTooLarge)),
        };

        out |= usize::from(*byte & 0b1111111);

        if (*byte & 0x80) == 0 {
            assert_eq!(n, buffer.len() - 1);
            return Some(Ok(out));
        }
    }

    None
}

#[derive(Debug, derive_more::Display)]
pub enum FramedError {
    LengthPrefixTooLarge,
}

pub struct FrameDrain<'a>(&'a mut Framed);

impl<'a> Deref for FrameDrain<'a> {
    type Target = [u8];

    fn deref(&self) -> &[u8] {
        &self.0.buffer
    }
}

impl<'a> AsRef<[u8]> for FrameDrain<'a> {
    fn as_ref(&self) -> &[u8] {
        &self.0.buffer
    }
}

impl<'a> fmt::Debug for FrameDrain<'a> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(&self.0.buffer, f)
    }
}

impl<'a> Drop for FrameDrain<'a> {
    fn drop(&mut self) {
        debug_assert!(matches!(self.0.inner, FramedInner::Body { .. }));
        self.0.buffer.clear();
        self.0.inner = FramedInner::Length;
    }
}

// TODO: tests
