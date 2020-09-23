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

use core::{convert::TryFrom, fmt};

/// A single nibble with four bits.
#[derive(Copy, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Nibble(u8);

impl TryFrom<u8> for Nibble {
    type Error = NibbleFromU8Error;

    fn try_from(val: u8) -> Result<Self, Self::Error> {
        if val < 16 {
            Ok(Nibble(val))
        } else {
            Err(NibbleFromU8Error::TooLarge)
        }
    }
}

impl From<Nibble> for u8 {
    fn from(nibble: Nibble) -> u8 {
        nibble.0
    }
}

impl fmt::Debug for Nibble {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:x}", self.0)
    }
}

/// Error when building a [`Nibble`] from a `u8`.
#[derive(Debug, derive_more::Display)]
pub enum NibbleFromU8Error {
    /// The integer value is too large.
    #[display(fmt = "Value is too large")]
    TooLarge,
}

/// Turns an iterator of bytes into an iterator of nibbles corresponding to these bytes.
pub fn bytes_to_nibbles<I>(bytes: I) -> BytesToNibbles<I> {
    BytesToNibbles {
        inner: bytes,
        next: None,
    }
}

/// Turns an iterator of bytes into an iterator of nibbles corresponding to these bytes.
#[derive(Debug, Copy, Clone)]
pub struct BytesToNibbles<I> {
    inner: I,
    next: Option<Nibble>,
}

impl<I: Iterator<Item = u8>> Iterator for BytesToNibbles<I> {
    type Item = Nibble;

    fn next(&mut self) -> Option<Nibble> {
        if let Some(next) = self.next.take() {
            return Some(next);
        }

        let byte = self.inner.next()?;
        self.next = Some(Nibble(byte & 0xf));
        Some(Nibble(byte >> 4))
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        let (min, max) = self.inner.size_hint();

        if self.next.is_some() {
            (
                min.saturating_add(1),
                max.and_then(|max| max.checked_add(1)),
            )
        } else {
            (min, max)
        }
    }
}

impl<I: ExactSizeIterator<Item = u8>> ExactSizeIterator for BytesToNibbles<I> {}
