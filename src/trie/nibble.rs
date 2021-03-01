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

use core::{convert::TryFrom, fmt, iter};

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

/// Returns an iterator of all possible nibble values.
pub fn all_nibbles() -> impl ExactSizeIterator<Item = Nibble> {
    (0..16).map(Nibble)
}

/// Turns an iterator of nibbles into an iterator of bytes.
///
/// If the number of nibbles is uneven, adds a `0` nibble at the end.
// TODO: ExactSizeIterator
pub fn nibbles_to_bytes_extend<I: Iterator<Item = Nibble>>(
    mut nibbles: I,
) -> impl Iterator<Item = u8> {
    iter::from_fn(move || {
        let n1 = nibbles.next()?;
        let n2 = nibbles.next().unwrap_or(Nibble(0));
        let byte = (n1.0 << 4) | n2.0;
        Some(byte)
    })
}

/// Turns an iterator of bytes into an iterator of nibbles corresponding to these bytes.
///
/// For each byte, the iterator yields a nibble containing the 4 most significant bits then a
/// nibble containing the 4 least significant bits.
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
                min.saturating_mul(2).saturating_add(1),
                max.and_then(|max| max.checked_mul(2))
                    .and_then(|max| max.checked_add(1)),
            )
        } else {
            (
                min.saturating_mul(2),
                max.and_then(|max| max.checked_mul(2)),
            )
        }
    }
}

impl<I: ExactSizeIterator<Item = u8>> ExactSizeIterator for BytesToNibbles<I> {}

#[cfg(test)]
mod tests {
    use super::{bytes_to_nibbles, Nibble, NibbleFromU8Error};
    use core::convert::TryFrom as _;

    #[test]
    fn nibble_try_from() {
        assert_eq!(u8::from(Nibble::try_from(0).unwrap()), 0);
        assert_eq!(u8::from(Nibble::try_from(1).unwrap()), 1);
        assert_eq!(u8::from(Nibble::try_from(15).unwrap()), 15);

        assert!(matches!(
            Nibble::try_from(16),
            Err(NibbleFromU8Error::TooLarge)
        ));
        assert!(matches!(
            Nibble::try_from(255),
            Err(NibbleFromU8Error::TooLarge)
        ));
    }

    #[test]
    fn bytes_to_nibbles_works() {
        assert_eq!(
            bytes_to_nibbles([].iter().cloned()).collect::<Vec<_>>(),
            &[]
        );
        assert_eq!(
            bytes_to_nibbles([1].iter().cloned()).collect::<Vec<_>>(),
            &[Nibble::try_from(0).unwrap(), Nibble::try_from(1).unwrap()]
        );
        assert_eq!(
            bytes_to_nibbles([200].iter().cloned()).collect::<Vec<_>>(),
            &[
                Nibble::try_from(0xc).unwrap(),
                Nibble::try_from(0x8).unwrap()
            ]
        );
        assert_eq!(
            bytes_to_nibbles([80, 200, 9].iter().cloned()).collect::<Vec<_>>(),
            &[
                Nibble::try_from(5).unwrap(),
                Nibble::try_from(0).unwrap(),
                Nibble::try_from(0xc).unwrap(),
                Nibble::try_from(0x8).unwrap(),
                Nibble::try_from(0).unwrap(),
                Nibble::try_from(9).unwrap()
            ]
        );
    }

    #[test]
    fn bytes_to_nibbles_len() {
        assert_eq!(bytes_to_nibbles([].iter().cloned()).len(), 0);
        assert_eq!(bytes_to_nibbles([1].iter().cloned()).len(), 2);
        assert_eq!(bytes_to_nibbles([200].iter().cloned()).len(), 2);
        assert_eq!(
            bytes_to_nibbles([1, 2, 3, 4, 5, 6].iter().cloned()).len(),
            12
        );
    }
}
