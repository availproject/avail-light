// Substrate-lite
// Copyright (C) 2019-2020  Parity Technologies (UK) Ltd.
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
