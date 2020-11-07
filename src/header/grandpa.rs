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

use super::Error;
use crate::util;

use alloc::vec::Vec;
use core::{cmp, convert::TryFrom, fmt, iter, slice};
use parity_scale_codec::{Decode as _, DecodeAll as _};

/// A consensus log item for GrandPa.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GrandpaConsensusLogRef<'a> {
    /// Schedule an authority set change.
    ///
    /// The earliest digest of this type in a single block will be respected,
    /// provided that there is no `ForcedChange` digest. If there is, then the
    /// `ForcedChange` will take precedence.
    ///
    /// No change should be scheduled if one is already and the delay has not
    /// passed completely.
    ///
    /// This should be a pure function: i.e. as long as the runtime can interpret
    /// the digest type it should return the same result regardless of the current
    /// state.
    ScheduledChange(GrandpaScheduledChangeRef<'a>),

    /// Force an authority set change.
    ///
    /// Forced changes are applied after a delay of _imported_ blocks,
    /// while pending changes are applied after a delay of _finalized_ blocks.
    ///
    /// The earliest digest of this type in a single block will be respected,
    /// with others ignored.
    ///
    /// No change should be scheduled if one is already and the delay has not
    /// passed completely.
    ///
    /// This should be a pure function: i.e. as long as the runtime can interpret
    /// the digest type it should return the same result regardless of the current
    /// state.
    ForcedChange {
        reset_block_height: u32,
        change: GrandpaScheduledChangeRef<'a>,
    },

    /// Note that the authority with given index is disabled until the next change.
    OnDisabled(u64),

    /// A signal to pause the current authority set after the given delay.
    /// After finalizing the block at _delay_ the authorities should stop voting.
    Pause(u32),

    /// A signal to resume the current authority set after the given delay.
    /// After authoring the block at _delay_ the authorities should resume voting.
    Resume(u32),
}

impl<'a> GrandpaConsensusLogRef<'a> {
    /// Decodes a [`GrandpaConsensusLogRef`] from a slice of bytes.
    pub fn from_slice(slice: &'a [u8]) -> Result<Self, Error> {
        Ok(match slice.get(0) {
            Some(1) => GrandpaConsensusLogRef::ScheduledChange(
                GrandpaScheduledChangeRef::from_slice(&slice[1..])?,
            ),
            Some(2) => {
                let reset_block_height =
                    u32::decode_all(&slice[1..9]).map_err(Error::DigestItemDecodeError)?;
                let change = GrandpaScheduledChangeRef::from_slice(&slice[9..])?;
                GrandpaConsensusLogRef::ForcedChange {
                    reset_block_height,
                    change,
                }
            }
            Some(3) => GrandpaConsensusLogRef::OnDisabled(
                u64::decode_all(&slice[1..]).map_err(Error::DigestItemDecodeError)?,
            ),
            Some(4) => GrandpaConsensusLogRef::Pause(
                u32::decode_all(&slice[1..]).map_err(Error::DigestItemDecodeError)?,
            ),
            Some(5) => GrandpaConsensusLogRef::Resume(
                u32::decode_all(&slice[1..]).map_err(Error::DigestItemDecodeError)?,
            ),
            Some(_) => return Err(Error::BadGrandpaConsensusRefType),
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
            GrandpaConsensusLogRef::ScheduledChange(_) => [1],
            GrandpaConsensusLogRef::ForcedChange { .. } => [2],
            GrandpaConsensusLogRef::OnDisabled(_) => [3],
            GrandpaConsensusLogRef::Pause(_) => [4],
            GrandpaConsensusLogRef::Resume(_) => [5],
        }));

        let body = match self {
            GrandpaConsensusLogRef::ScheduledChange(change) => {
                either::Either::Left(change.scale_encoding().map(either::Either::Left))
            }
            GrandpaConsensusLogRef::ForcedChange { .. } => todo!(), // TODO:
            GrandpaConsensusLogRef::OnDisabled(n) => either::Either::Right(iter::once(
                either::Either::Right(parity_scale_codec::Encode::encode(n)),
            )),
            GrandpaConsensusLogRef::Pause(n) => either::Either::Right(iter::once(
                either::Either::Right(parity_scale_codec::Encode::encode(n)),
            )),
            GrandpaConsensusLogRef::Resume(n) => either::Either::Right(iter::once(
                either::Either::Right(parity_scale_codec::Encode::encode(n)),
            )),
        };

        index
            .map(either::Either::Left)
            .chain(body.map(either::Either::Right))
    }
}

impl<'a> From<&'a GrandpaConsensusLog> for GrandpaConsensusLogRef<'a> {
    fn from(a: &'a GrandpaConsensusLog) -> Self {
        match a {
            GrandpaConsensusLog::ScheduledChange(v) => {
                GrandpaConsensusLogRef::ScheduledChange(v.into())
            }
            GrandpaConsensusLog::ForcedChange {
                reset_block_height,
                change,
            } => GrandpaConsensusLogRef::ForcedChange {
                reset_block_height: *reset_block_height,
                change: change.into(),
            },
            GrandpaConsensusLog::OnDisabled(v) => GrandpaConsensusLogRef::OnDisabled(*v),
            GrandpaConsensusLog::Pause(v) => GrandpaConsensusLogRef::Pause(*v),
            GrandpaConsensusLog::Resume(v) => GrandpaConsensusLogRef::Resume(*v),
        }
    }
}

/// A consensus log item for GrandPa.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GrandpaConsensusLog {
    /// Schedule an authority set change.
    ///
    /// The earliest digest of this type in a single block will be respected,
    /// provided that there is no `ForcedChange` digest. If there is, then the
    /// `ForcedChange` will take precedence.
    ///
    /// No change should be scheduled if one is already and the delay has not
    /// passed completely.
    ///
    /// This should be a pure function: i.e. as long as the runtime can interpret
    /// the digest type it should return the same result regardless of the current
    /// state.
    ScheduledChange(GrandpaScheduledChange),

    /// Force an authority set change.
    ///
    /// Forced changes are applied after a delay of _imported_ blocks,
    /// while pending changes are applied after a delay of _finalized_ blocks.
    ///
    /// The earliest digest of this type in a single block will be respected,
    /// with others ignored.
    ///
    /// No change should be scheduled if one is already and the delay has not
    /// passed completely.
    ///
    /// This should be a pure function: i.e. as long as the runtime can interpret
    /// the digest type it should return the same result regardless of the current
    /// state.
    ForcedChange {
        reset_block_height: u32,
        change: GrandpaScheduledChange,
    },

    /// Note that the authority with given index is disabled until the next change.
    OnDisabled(u64),

    /// A signal to pause the current authority set after the given delay.
    /// After finalizing the block at _delay_ the authorities should stop voting.
    Pause(u32),

    /// A signal to resume the current authority set after the given delay.
    /// After authoring the block at _delay_ the authorities should resume voting.
    Resume(u32),
}

impl<'a> From<GrandpaConsensusLogRef<'a>> for GrandpaConsensusLog {
    fn from(a: GrandpaConsensusLogRef<'a>) -> Self {
        match a {
            GrandpaConsensusLogRef::ScheduledChange(v) => {
                GrandpaConsensusLog::ScheduledChange(v.into())
            }
            GrandpaConsensusLogRef::ForcedChange {
                reset_block_height,
                change,
            } => GrandpaConsensusLog::ForcedChange {
                reset_block_height,
                change: change.into(),
            },
            GrandpaConsensusLogRef::OnDisabled(v) => GrandpaConsensusLog::OnDisabled(v),
            GrandpaConsensusLogRef::Pause(v) => GrandpaConsensusLog::Pause(v),
            GrandpaConsensusLogRef::Resume(v) => GrandpaConsensusLog::Resume(v),
        }
    }
}

/// A scheduled change of authority set.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GrandpaScheduledChangeRef<'a> {
    /// The new authorities after the change, along with their respective weights.
    pub next_authorities: GrandpaAuthoritiesIter<'a>,
    /// The number of blocks to delay.
    pub delay: u32,
}

impl<'a> GrandpaScheduledChangeRef<'a> {
    /// Decodes a [`GrandpaScheduledChangeRef`] from a slice of bytes.
    pub fn from_slice(mut slice: &'a [u8]) -> Result<Self, Error> {
        // TODO: don't unwrap on decode fail
        let authorities_len = usize::try_from(
            parity_scale_codec::Compact::<u64>::decode(&mut slice)
                .unwrap()
                .0,
        )
        .unwrap();

        if slice.len() != authorities_len * 40 + 4 {
            return Err(Error::TooShort);
        }

        Ok(GrandpaScheduledChangeRef {
            next_authorities: GrandpaAuthoritiesIter(GrandpaAuthoritiesIterInner::Encoded(
                slice[0..authorities_len * 40].chunks(40),
            )),
            delay: u32::from_le_bytes(<[u8; 4]>::try_from(&slice[authorities_len * 40..]).unwrap()),
        })
    }

    /// Returns an iterator to list of buffers which, when concatenated, produces the SCALE
    /// encoding of that object.
    pub fn scale_encoding(
        &self,
    ) -> impl Iterator<Item = impl AsRef<[u8]> + Clone + 'a> + Clone + 'a {
        let header = util::encode_scale_compact_usize(self.next_authorities.len());
        iter::once(either::Left(either::Left(header)))
            .chain(
                self.next_authorities
                    .clone()
                    .flat_map(|a| a.scale_encoding())
                    .map(|buf| either::Right(buf)),
            )
            .chain(iter::once(either::Left(either::Right(
                self.delay.to_le_bytes().to_vec(), // TODO: don't allocate
            ))))
    }
}

impl<'a> From<&'a GrandpaScheduledChange> for GrandpaScheduledChangeRef<'a> {
    fn from(gp: &'a GrandpaScheduledChange) -> Self {
        GrandpaScheduledChangeRef {
            next_authorities: GrandpaAuthoritiesIter(GrandpaAuthoritiesIterInner::Decoded(
                gp.next_authorities.iter(),
            )),
            delay: gp.delay,
        }
    }
}

/// A scheduled change of authority set.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GrandpaScheduledChange {
    /// The new authorities after the change, along with their respective weights.
    pub next_authorities: Vec<GrandpaAuthority>,
    /// The number of blocks to delay.
    pub delay: u32,
}

impl<'a> From<GrandpaScheduledChangeRef<'a>> for GrandpaScheduledChange {
    fn from(gp: GrandpaScheduledChangeRef<'a>) -> Self {
        GrandpaScheduledChange {
            next_authorities: gp.next_authorities.map(Into::into).collect(),
            delay: gp.delay,
        }
    }
}

/// List of authorities in a GrandPa context.
#[derive(Clone)]
pub struct GrandpaAuthoritiesIter<'a>(GrandpaAuthoritiesIterInner<'a>);

#[derive(Clone)]
enum GrandpaAuthoritiesIterInner<'a> {
    Encoded(slice::Chunks<'a, u8>),
    Decoded(slice::Iter<'a, GrandpaAuthority>),
}

impl<'a> GrandpaAuthoritiesIter<'a> {
    /// Returns an iterator corresponding to the given slice.
    pub fn new(slice: &'a [GrandpaAuthority]) -> Self {
        GrandpaAuthoritiesIter(GrandpaAuthoritiesIterInner::Decoded(slice.iter()))
    }
}

impl<'a> Iterator for GrandpaAuthoritiesIter<'a> {
    type Item = GrandpaAuthorityRef<'a>;

    fn next(&mut self) -> Option<Self::Item> {
        match &mut self.0 {
            GrandpaAuthoritiesIterInner::Decoded(inner) => inner.next().map(Into::into),
            GrandpaAuthoritiesIterInner::Encoded(inner) => {
                let item = inner.next()?;
                assert_eq!(item.len(), 40);
                Some(GrandpaAuthorityRef {
                    public_key: <&[u8; 32]>::try_from(&item[0..32]).unwrap(),
                    weight: u64::from_le_bytes(<[u8; 8]>::try_from(&item[32..40]).unwrap()),
                })
            }
        }
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        match &self.0 {
            GrandpaAuthoritiesIterInner::Encoded(inner) => inner.size_hint(),
            GrandpaAuthoritiesIterInner::Decoded(inner) => inner.size_hint(),
        }
    }
}

impl<'a> ExactSizeIterator for GrandpaAuthoritiesIter<'a> {}

impl<'a> cmp::PartialEq<GrandpaAuthoritiesIter<'a>> for GrandpaAuthoritiesIter<'a> {
    fn eq(&self, other: &GrandpaAuthoritiesIter<'a>) -> bool {
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

impl<'a> cmp::Eq for GrandpaAuthoritiesIter<'a> {}

impl<'a> fmt::Debug for GrandpaAuthoritiesIter<'a> {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.debug_list().entries(self.clone()).finish()
    }
}

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub struct GrandpaAuthorityRef<'a> {
    /// Ed25519 public key.
    pub public_key: &'a [u8; 32],

    /// Arbitrary number indicating the weight of the authority.
    ///
    /// This value can only be compared to other weight values.
    // TODO: should be NonZeroU64; requires deep changes in decoding code though
    pub weight: u64,
}

impl<'a> GrandpaAuthorityRef<'a> {
    /// Returns an iterator to list of buffers which, when concatenated, produces the SCALE
    /// encoding of that object.
    pub fn scale_encoding(
        &self,
    ) -> impl Iterator<Item = impl AsRef<[u8]> + Clone + 'a> + Clone + 'a {
        iter::once(either::Either::Right(self.public_key))
            .chain(iter::once(either::Either::Left(self.weight.to_le_bytes())))
    }
}

impl<'a> From<&'a GrandpaAuthority> for GrandpaAuthorityRef<'a> {
    fn from(gp: &'a GrandpaAuthority) -> Self {
        GrandpaAuthorityRef {
            public_key: &gp.public_key,
            weight: gp.weight,
        }
    }
}

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub struct GrandpaAuthority {
    /// Ed25519 public key.
    pub public_key: [u8; 32],

    /// Arbitrary number indicating the weight of the authority.
    ///
    /// This value can only be compared to other weight values.
    // TODO: should be NonZeroU64; requires deep changes in decoding code though
    pub weight: u64,
}

impl GrandpaAuthority {
    /// Returns an iterator to list of buffers which, when concatenated, produces the SCALE
    /// encoding of that object.
    pub fn scale_encoding<'a>(
        &'a self,
    ) -> impl Iterator<Item = impl AsRef<[u8]> + Clone + 'a> + Clone + 'a {
        GrandpaAuthorityRef::from(self).scale_encoding()
    }
}

impl<'a> From<GrandpaAuthorityRef<'a>> for GrandpaAuthority {
    fn from(gp: GrandpaAuthorityRef<'a>) -> Self {
        GrandpaAuthority {
            public_key: *gp.public_key,
            weight: gp.weight,
        }
    }
}
