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

use super::Error;
use crate::util;

use alloc::vec::Vec;
use core::{cmp, convert::TryFrom, fmt, iter, slice};
use parity_scale_codec::DecodeAll as _;

/// A consensus log item for AURA.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AuraConsensusLogRef<'a> {
    /// List of authorities has changed.
    AuthoritiesChange(AuraAuthoritiesIter<'a>),
    /// Disable the authority with given index.
    OnDisabled(u32),
}

impl<'a> AuraConsensusLogRef<'a> {
    /// Decodes a [`AuraConsensusLogRef`] from a slice of bytes.
    pub fn from_slice(slice: &'a [u8]) -> Result<Self, Error> {
        Ok(match slice.get(0) {
            Some(1) => {
                AuraConsensusLogRef::AuthoritiesChange(AuraAuthoritiesIter::decode(&slice[1..])?)
            }
            Some(2) => AuraConsensusLogRef::OnDisabled(
                u32::decode_all(&slice[1..]).map_err(Error::DigestItemDecodeError)?,
            ),
            Some(_) => return Err(Error::BadAuraConsensusRefType),
            None => return Err(Error::TooShort),
        })
    }

    /// Returns an iterator to list of buffers which, when concatenated, produces the SCALE
    /// encoding of that object.
    pub fn scale_encoding(
        &self,
    ) -> impl Iterator<Item = impl AsRef<[u8]> + Clone + 'a> + Clone + 'a {
        #[derive(Clone)]
        struct One([u8; 1]);
        impl AsRef<[u8]> for One {
            fn as_ref(&self) -> &[u8] {
                &self.0[..]
            }
        }

        let index = iter::once(One(match self {
            AuraConsensusLogRef::AuthoritiesChange(_) => [1],
            AuraConsensusLogRef::OnDisabled(_) => [2],
        }));

        let body = match self {
            AuraConsensusLogRef::AuthoritiesChange(list) => {
                let len = util::encode_scale_compact_usize(list.len());
                either::Left(
                    iter::once(len)
                        .map(either::Left)
                        .chain(
                            list.clone()
                                .flat_map(|l| l.scale_encoding().map(either::Right)),
                        )
                        .map(either::Left),
                )
            }
            AuraConsensusLogRef::OnDisabled(digest) => either::Right(
                iter::once(parity_scale_codec::Encode::encode(digest)).map(either::Right),
            ),
        };

        index
            .map(either::Either::Left)
            .chain(body.map(either::Either::Right))
    }
}

impl<'a> From<&'a AuraConsensusLog> for AuraConsensusLogRef<'a> {
    fn from(a: &'a AuraConsensusLog) -> Self {
        match a {
            AuraConsensusLog::AuthoritiesChange(v) => AuraConsensusLogRef::AuthoritiesChange(
                AuraAuthoritiesIter(AuraAuthoritiesIterInner::List(v.iter())),
            ),
            AuraConsensusLog::OnDisabled(v) => AuraConsensusLogRef::OnDisabled(*v),
        }
    }
}

/// A consensus log item for AURA.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AuraConsensusLog {
    /// List of authorities has changed.
    AuthoritiesChange(Vec<AuraAuthority>),
    /// Disable the authority with given index.
    OnDisabled(u32),
}

impl<'a> From<AuraConsensusLogRef<'a>> for AuraConsensusLog {
    fn from(a: AuraConsensusLogRef<'a>) -> Self {
        match a {
            AuraConsensusLogRef::AuthoritiesChange(v) => {
                AuraConsensusLog::AuthoritiesChange(v.map(|a| a.into()).collect())
            }
            AuraConsensusLogRef::OnDisabled(v) => AuraConsensusLog::OnDisabled(v),
        }
    }
}

/// List of authorities in an AURA context.
#[derive(Clone)]
pub struct AuraAuthoritiesIter<'a>(AuraAuthoritiesIterInner<'a>);

#[derive(Clone)]
enum AuraAuthoritiesIterInner<'a> {
    List(slice::Iter<'a, AuraAuthority>),
    Raw(slice::Chunks<'a, u8>),
}

impl<'a> AuraAuthoritiesIter<'a> {
    /// Decodes a list of authorities from a SCALE-encoded blob of data.
    pub fn decode(data: &'a [u8]) -> Result<Self, Error> {
        let (data, num_items) = util::nom_scale_compact_usize::<nom::error::Error<&[u8]>>(data)
            .map_err(|_| Error::TooShort)?;

        if data.len() != num_items * 32 {
            return Err(Error::BadAuraAuthoritiesListLen);
        }

        Ok(AuraAuthoritiesIter(AuraAuthoritiesIterInner::Raw(
            data.chunks(32),
        )))
    }

    /// Builds an iterator corresponding to the given slice.
    pub fn from_slice(slice: &'a [AuraAuthority]) -> Self {
        AuraAuthoritiesIter(AuraAuthoritiesIterInner::List(slice.iter()))
    }
}

impl<'a> Iterator for AuraAuthoritiesIter<'a> {
    type Item = AuraAuthorityRef<'a>;

    fn next(&mut self) -> Option<Self::Item> {
        match &mut self.0 {
            AuraAuthoritiesIterInner::List(l) => l.next().map(Into::into),
            AuraAuthoritiesIterInner::Raw(l) => {
                let item = l.next()?;
                assert_eq!(item.len(), 32);
                Some(AuraAuthorityRef {
                    public_key: <&[u8; 32]>::try_from(item).unwrap(),
                })
            }
        }
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        match &self.0 {
            AuraAuthoritiesIterInner::List(l) => l.size_hint(),
            AuraAuthoritiesIterInner::Raw(l) => l.size_hint(),
        }
    }
}

impl<'a> ExactSizeIterator for AuraAuthoritiesIter<'a> {}

impl<'a> cmp::PartialEq<AuraAuthoritiesIter<'a>> for AuraAuthoritiesIter<'a> {
    fn eq(&self, other: &AuraAuthoritiesIter<'a>) -> bool {
        let mut a = self.clone();
        let mut b = other.clone();
        loop {
            match (a.next(), b.next()) {
                (Some(a), Some(b)) if a == b => {}
                (None, None) => return true,
                _ => return false,
            }
        }
    }
}

impl<'a> cmp::Eq for AuraAuthoritiesIter<'a> {}

impl<'a> fmt::Debug for AuraAuthoritiesIter<'a> {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.debug_list().entries(self.clone()).finish()
    }
}

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub struct AuraAuthorityRef<'a> {
    /// Sr25519 public key.
    pub public_key: &'a [u8; 32],
}

impl<'a> AuraAuthorityRef<'a> {
    /// Returns an iterator to list of buffers which, when concatenated, produces the SCALE
    /// encoding of that object.
    pub fn scale_encoding(
        &self,
    ) -> impl Iterator<Item = impl AsRef<[u8]> + Clone + 'a> + Clone + 'a {
        iter::once(self.public_key)
    }
}

impl<'a> From<&'a AuraAuthority> for AuraAuthorityRef<'a> {
    fn from(a: &'a AuraAuthority) -> Self {
        AuraAuthorityRef {
            public_key: &a.public_key,
        }
    }
}

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub struct AuraAuthority {
    /// Sr25519 public key.
    pub public_key: [u8; 32],
}

impl<'a> From<AuraAuthorityRef<'a>> for AuraAuthority {
    fn from(a: AuraAuthorityRef<'a>) -> Self {
        AuraAuthority {
            public_key: *a.public_key,
        }
    }
}

/// AURA slot number pre-digest.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuraPreDigest {
    /// Slot number when the block was produced.
    pub slot_number: u64,
}

impl AuraPreDigest {
    /// Decodes a [`AuraPreDigest`] from a slice of bytes.
    pub fn from_slice(slice: &[u8]) -> Result<Self, Error> {
        if slice.len() != 8 {
            return Err(Error::TooShort);
        }

        Ok(AuraPreDigest {
            slot_number: u64::from_le_bytes(<[u8; 8]>::try_from(slice).unwrap()),
        })
    }

    /// Returns an iterator to list of buffers which, when concatenated, produces the SCALE
    /// encoding of that object.
    pub fn scale_encoding(&self) -> impl Iterator<Item = impl AsRef<[u8]> + Clone> + Clone {
        iter::once(self.slot_number.to_le_bytes())
    }
}
