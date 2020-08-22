// Copyright 2017-2020 Parity Technologies (UK) Ltd.
// This file is part of Substrate.

// Substrate is free software: you can redistribute it and/or modify
// it under the terms of the GNU General Public License as published by
// the Free Software Foundation, either version 3 of the License, or
// (at your option) any later version.

// Substrate is distributed in the hope that it will be useful,
// but WITHOUT ANY WARRANTY; without even the implied warranty of
// MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
// GNU General Public License for more details.

// You should have received a copy of the GNU General Public License
// along with Substrate.  If not, see <http://www.gnu.org/licenses/>.

use super::Error;

use blake2::digest::{Input as _, VariableOutput as _};
use core::{cmp, convert::TryFrom, fmt, iter, slice};
use parity_scale_codec::{Decode as _, DecodeAll as _};

/// A consensus log item for BABE.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BabeConsensusLogRef<'a> {
    /// The epoch has changed. This provides information about the _next_
    /// epoch - information about the _current_ epoch (i.e. the one we've just
    /// entered) should already be available earlier in the chain.
    NextEpochData(BabeNextEpochRef<'a>),
    /// Disable the authority with given index.
    OnDisabled(u32),
    /// The epoch has changed, and the epoch after the current one will
    /// enact different epoch configurations.
    NextConfigData(BabeNextConfig),
}

impl<'a> BabeConsensusLogRef<'a> {
    /// Decodes a [`BabeConsensusLogRef`] from a slice of bytes.
    pub fn from_slice(slice: &'a [u8]) -> Result<Self, Error> {
        Ok(match slice.get(0) {
            Some(1) => {
                BabeConsensusLogRef::NextEpochData(BabeNextEpochRef::from_slice(&slice[1..])?)
            }
            Some(2) => BabeConsensusLogRef::OnDisabled(
                u32::decode_all(&slice[1..]).map_err(Error::DigestItemDecodeError)?,
            ),
            Some(3) => BabeConsensusLogRef::NextConfigData(
                BabeNextConfig::decode_all(&slice[1..]).map_err(Error::DigestItemDecodeError)?,
            ),
            Some(_) => return Err(Error::BadBabeConsensusRefType),
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
            BabeConsensusLogRef::NextEpochData(_) => [1],
            BabeConsensusLogRef::OnDisabled(_) => [2],
            BabeConsensusLogRef::NextConfigData(_) => [3],
        }));

        let body = match self {
            BabeConsensusLogRef::NextEpochData(digest) => {
                either::Either::Left(digest.scale_encoding().map(either::Either::Left))
            }
            BabeConsensusLogRef::OnDisabled(digest) => either::Either::Right(iter::once(
                either::Either::Right(parity_scale_codec::Encode::encode(digest)),
            )),
            BabeConsensusLogRef::NextConfigData(digest) => either::Either::Right(iter::once(
                either::Either::Right(parity_scale_codec::Encode::encode(digest)),
            )),
        };

        index
            .map(either::Either::Left)
            .chain(body.map(either::Either::Right))
    }
}

/// Information about the next epoch. This is broadcast in the first block
/// of the epoch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BabeNextEpochRef<'a> {
    /// The authorities.
    pub authorities: BabeAuthoritiesIter<'a>,

    /// The value of randomness to use for the slot-assignment.
    pub randomness: &'a [u8; 32],
}

impl<'a> BabeNextEpochRef<'a> {
    /// Decodes a [`BabePreDigestRef`] from a slice of bytes.
    pub fn from_slice(mut slice: &'a [u8]) -> Result<Self, Error> {
        // TODO: don't unwrap on decode fail
        let authorities_len = usize::try_from(
            parity_scale_codec::Compact::<u64>::decode(&mut slice)
                .unwrap()
                .0,
        )
        .unwrap();

        if slice.len() != authorities_len * 40 + 32 {
            return Err(Error::TooShort);
        }

        Ok(BabeNextEpochRef {
            authorities: BabeAuthoritiesIter(BabeAuthoritiesIterInner::Raw(
                slice[0..authorities_len * 40].chunks(40),
            )),
            randomness: <&[u8; 32]>::try_from(&slice[authorities_len * 40..]).unwrap(),
        })
    }

    /// Returns an iterator to list of buffers which, when concatenated, produces the SCALE
    /// encoding of that object.
    pub fn scale_encoding(
        &self,
    ) -> impl Iterator<Item = impl AsRef<[u8]> + Clone + 'a> + Clone + 'a {
        // TODO: don't allocate
        let header = parity_scale_codec::Encode::encode(&parity_scale_codec::Compact(
            u64::try_from(self.authorities.len()).unwrap(),
        ));

        iter::once(either::Either::Left(header))
            .chain(
                self.authorities
                    .clone()
                    .flat_map(|a| a.scale_encoding())
                    .map(|buf| either::Either::Right(either::Either::Left(buf))),
            )
            .chain(iter::once(either::Either::Right(either::Either::Right(
                &self.randomness[..],
            ))))
    }
}

impl<'a> From<&'a BabeNextEpoch> for BabeNextEpochRef<'a> {
    fn from(a: &'a BabeNextEpoch) -> Self {
        BabeNextEpochRef {
            authorities: BabeAuthoritiesIter(BabeAuthoritiesIterInner::List(a.authorities.iter())),
            randomness: &a.randomness,
        }
    }
}

/// Information about the next epoch. This is broadcast in the first block
/// of the epoch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BabeNextEpoch {
    /// The authorities.
    pub authorities: Vec<BabeAuthority>,

    /// The value of randomness to use for the slot-assignment.
    pub randomness: [u8; 32],
}

impl<'a> From<BabeNextEpochRef<'a>> for BabeNextEpoch {
    fn from(a: BabeNextEpochRef<'a>) -> Self {
        BabeNextEpoch {
            authorities: a.authorities.map(Into::into).collect(),
            randomness: *a.randomness,
        }
    }
}

/// List of authorities in a BABE context.
#[derive(Clone)]
pub struct BabeAuthoritiesIter<'a>(BabeAuthoritiesIterInner<'a>);

#[derive(Clone)]
enum BabeAuthoritiesIterInner<'a> {
    List(slice::Iter<'a, BabeAuthority>),
    Raw(slice::Chunks<'a, u8>),
}

impl<'a> Iterator for BabeAuthoritiesIter<'a> {
    type Item = BabeAuthorityRef<'a>;

    fn next(&mut self) -> Option<Self::Item> {
        match &mut self.0 {
            BabeAuthoritiesIterInner::List(l) => l.next().map(Into::into),
            BabeAuthoritiesIterInner::Raw(l) => {
                let item = l.next()?;
                assert_eq!(item.len(), 40);
                Some(BabeAuthorityRef {
                    public_key: <&[u8; 32]>::try_from(&item[0..32]).unwrap(),
                    weight: u64::from_le_bytes(<[u8; 8]>::try_from(&item[32..40]).unwrap()),
                })
            }
        }
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        match &self.0 {
            BabeAuthoritiesIterInner::List(l) => l.size_hint(),
            BabeAuthoritiesIterInner::Raw(l) => l.size_hint(),
        }
    }
}

impl<'a> ExactSizeIterator for BabeAuthoritiesIter<'a> {}

impl<'a> cmp::PartialEq<BabeAuthoritiesIter<'a>> for BabeAuthoritiesIter<'a> {
    fn eq(&self, other: &BabeAuthoritiesIter<'a>) -> bool {
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

impl<'a> cmp::Eq for BabeAuthoritiesIter<'a> {}

impl<'a> fmt::Debug for BabeAuthoritiesIter<'a> {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.debug_list().entries(self.clone()).finish()
    }
}

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub struct BabeAuthorityRef<'a> {
    /// Sr25519 public key.
    pub public_key: &'a [u8; 32],
    /// Arbitrary number indicating the weight of the authority.
    ///
    /// This value can only be compared to other weight values.
    pub weight: u64,
}

impl<'a> BabeAuthorityRef<'a> {
    /// Returns an iterator to list of buffers which, when concatenated, produces the SCALE
    /// encoding of that object.
    pub fn scale_encoding(
        &self,
    ) -> impl Iterator<Item = impl AsRef<[u8]> + Clone + 'a> + Clone + 'a {
        iter::once(either::Either::Right(self.public_key)).chain(iter::once(either::Either::Left(
            parity_scale_codec::Encode::encode(&self.weight),
        )))
    }
}

impl<'a> From<&'a BabeAuthority> for BabeAuthorityRef<'a> {
    fn from(a: &'a BabeAuthority) -> Self {
        BabeAuthorityRef {
            public_key: &a.public_key,
            weight: a.weight,
        }
    }
}

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub struct BabeAuthority {
    /// Sr25519 public key.
    pub public_key: [u8; 32],
    /// Arbitrary number indicating the weight of the authority.
    ///
    /// This value can only be compared to other weight values.
    pub weight: u64,
}

impl<'a> From<BabeAuthorityRef<'a>> for BabeAuthority {
    fn from(a: BabeAuthorityRef<'a>) -> Self {
        BabeAuthority {
            public_key: *a.public_key,
            weight: a.weight,
        }
    }
}

/// Information about the next epoch config, if changed. This is broadcast in the first
/// block of the epoch, and applies using the same rules as `NextEpochDescriptor`.
#[derive(
    Debug, Copy, Clone, PartialEq, Eq, parity_scale_codec::Encode, parity_scale_codec::Decode,
)]
pub struct BabeNextConfig {
    /// Value of `c` in `BabeEpochConfiguration`.
    pub c: (u64, u64),
    /// Value of `allowed_slots` in `BabeEpochConfiguration`.
    pub allowed_slots: BabeAllowedSlots,
}

/// Types of allowed slots.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, parity_scale_codec::Encode, parity_scale_codec::Decode,
)]
pub enum BabeAllowedSlots {
    /// Only allow primary slot claims.
    PrimarySlots,
    /// Allow primary and secondary plain slot claims.
    PrimaryAndSecondaryPlainSlots,
    /// Allow primary and secondary VRF slot claims.
    PrimaryAndSecondaryVRFSlots,
}

/// Available changes trie signals.
// TODO: review documentation
#[derive(Debug, PartialEq, Eq, Clone, parity_scale_codec::Encode, parity_scale_codec::Decode)]
pub enum ChangesTrieSignal {
    /// New changes trie configuration is enacted, starting from **next block**.
    ///
    /// The block that emits this signal will contain changes trie (CT) that covers
    /// blocks range [BEGIN; current block], where BEGIN is (order matters):
    /// - LAST_TOP_LEVEL_DIGEST_BLOCK+1 if top level digest CT has ever been created
    ///   using current configuration AND the last top level digest CT has been created
    ///   at block LAST_TOP_LEVEL_DIGEST_BLOCK;
    /// - LAST_CONFIGURATION_CHANGE_BLOCK+1 if there has been CT configuration change
    ///   before and the last configuration change happened at block
    ///   LAST_CONFIGURATION_CHANGE_BLOCK;
    /// - 1 otherwise.
    NewConfiguration(Option<ChangesTrieConfiguration>),
}

/// Substrate changes trie configuration.
// TODO: review documentation
#[derive(
    Debug, Clone, PartialEq, Eq, Default, parity_scale_codec::Encode, parity_scale_codec::Decode,
)]
pub struct ChangesTrieConfiguration {
    /// Interval (in blocks) at which level1-digests are created. Digests are not
    /// created when this is less or equal to 1.
    pub digest_interval: u32,

    /// Maximal number of digest levels in hierarchy. 0 means that digests are not
    /// created at all (even level1 digests). 1 means only level1-digests are created.
    /// 2 means that every digest_interval^2 there will be a level2-digest, and so on.
    /// Please ensure that maximum digest interval (i.e. digest_interval^digest_levels)
    /// is within `u32` limits. Otherwise you'll never see digests covering such intervals
    /// && maximal digests interval will be truncated to the last interval that fits
    /// `u32` limits.
    pub digest_levels: u32,
}

/// A BABE pre-runtime digest. This contains all data required to validate a
/// block and for the BABE runtime module. Slots can be assigned to a primary
/// (VRF based) and to a secondary (slot number based).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BabePreDigestRef<'a> {
    /// A primary VRF-based slot assignment.
    Primary(BabePrimaryPreDigestRef<'a>),
    /// A secondary deterministic slot assignment.
    SecondaryPlain(BabeSecondaryPlainPreDigest),
    /// A secondary deterministic slot assignment with VRF outputs.
    SecondaryVRF(BabeSecondaryVRFPreDigestRef<'a>),
}

impl<'a> BabePreDigestRef<'a> {
    /// Decodes a [`BabePreDigestRef`] from a slice of bytes.
    pub fn from_slice(slice: &'a [u8]) -> Result<Self, Error> {
        Ok(match slice.get(0) {
            Some(1) => BabePreDigestRef::Primary(BabePrimaryPreDigestRef::from_slice(&slice[1..])?),
            Some(2) => BabePreDigestRef::SecondaryPlain(BabeSecondaryPlainPreDigest::from_slice(
                &slice[1..],
            )?),
            Some(3) => BabePreDigestRef::SecondaryVRF(BabeSecondaryVRFPreDigestRef::from_slice(
                &slice[1..],
            )?),
            Some(_) => return Err(Error::BadBabePreDigestRefType),
            None => return Err(Error::TooShort),
        })
    }

    /// Returns `true` for [`BabePreDigestRef::Primary`].
    pub fn is_primary(&self) -> bool {
        match self {
            BabePreDigestRef::Primary(_) => true,
            _ => false,
        }
    }

    /// Returns the slot number stored in the header.
    pub fn slot_number(&self) -> u64 {
        match self {
            BabePreDigestRef::Primary(digest) => digest.slot_number,
            BabePreDigestRef::SecondaryPlain(digest) => digest.slot_number,
            BabePreDigestRef::SecondaryVRF(digest) => digest.slot_number,
        }
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
            BabePreDigestRef::Primary(_) => [1],
            BabePreDigestRef::SecondaryPlain(_) => [2],
            BabePreDigestRef::SecondaryVRF(_) => [3],
        }));

        let body = match self {
            BabePreDigestRef::Primary(digest) => either::Either::Left(either::Either::Left(
                digest
                    .scale_encoding()
                    .map(|buf| either::Either::Left(either::Either::Left(buf))),
            )),
            BabePreDigestRef::SecondaryPlain(digest) => {
                either::Either::Left(either::Either::Right(
                    digest
                        .scale_encoding()
                        .map(|buf| either::Either::Left(either::Either::Right(buf))),
                ))
            }
            BabePreDigestRef::SecondaryVRF(digest) => {
                either::Either::Right(digest.scale_encoding().map(either::Either::Right))
            }
        };

        index
            .map(either::Either::Left)
            .chain(body.map(either::Either::Right))
    }
}

/// Raw BABE primary slot assignment pre-digest.
#[derive(Clone)]
pub struct BabePrimaryPreDigestRef<'a> {
    /// Authority index
    pub authority_index: u32,
    /// Slot number
    pub slot_number: u64,
    /// VRF output
    pub vrf_output: &'a [u8; 32],
    /// VRF proof
    pub vrf_proof: &'a [u8; 64],
}

impl<'a> BabePrimaryPreDigestRef<'a> {
    /// Decodes a [`BabePrimaryPreDigestRef`] from a slice of bytes.
    pub fn from_slice(slice: &'a [u8]) -> Result<Self, Error> {
        if slice.len() != 4 + 8 + 32 + 64 {
            return Err(Error::TooShort);
        }

        Ok(BabePrimaryPreDigestRef {
            authority_index: u32::from_le_bytes(<[u8; 4]>::try_from(&slice[0..4]).unwrap()),
            slot_number: u64::from_le_bytes(<[u8; 8]>::try_from(&slice[4..12]).unwrap()),
            vrf_output: TryFrom::try_from(&slice[12..44]).unwrap(),
            vrf_proof: unsafe {
                // TODO: ugh, how do you even get a &[u8; 64] from a &[u8]
                &*(slice[44..108].as_ptr() as *const [u8; 64])
            },
        })
    }

    /// Returns an iterator to list of buffers which, when concatenated, produces the SCALE
    /// encoding of that object.
    pub fn scale_encoding(
        &self,
    ) -> impl Iterator<Item = impl AsRef<[u8]> + Clone + 'a> + Clone + 'a {
        // TODO: don't allocate
        let header = iter::once(either::Either::Left(parity_scale_codec::Encode::encode(&(
            self.authority_index,
            self.slot_number,
        ))));

        header
            .chain(iter::once(either::Either::Right(&self.vrf_output[..])))
            .chain(iter::once(either::Either::Right(&self.vrf_proof[..])))
    }
}

impl<'a> cmp::PartialEq<BabePrimaryPreDigestRef<'a>> for BabePrimaryPreDigestRef<'a> {
    fn eq(&self, other: &BabePrimaryPreDigestRef<'a>) -> bool {
        self.authority_index == other.authority_index
            && self.slot_number == other.slot_number
            && &self.vrf_output[..] == &other.vrf_output[..]
            && &self.vrf_proof[..] == &other.vrf_proof[..]
    }
}

impl<'a> cmp::Eq for BabePrimaryPreDigestRef<'a> {}

// This custom Debug implementation exists because `[u8; 64]` doesn't implement `Debug`
impl<'a> fmt::Debug for BabePrimaryPreDigestRef<'a> {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.debug_struct("BabePrimaryPreDigestRef").finish()
    }
}

/// BABE secondary slot assignment pre-digest.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BabeSecondaryPlainPreDigest {
    /// Authority index
    ///
    /// This is not strictly-speaking necessary, since the secondary slots
    /// are assigned based on slot number and epoch randomness. But including
    /// it makes things easier for higher-level users of the chain data to
    /// be aware of the author of a secondary-slot block.
    pub authority_index: u32,

    /// Slot number
    pub slot_number: u64,
}

impl BabeSecondaryPlainPreDigest {
    /// Decodes a [`BabeSecondaryPlainPreDigest`] from a slice of bytes.
    pub fn from_slice(slice: &[u8]) -> Result<Self, Error> {
        let (authority_index, slot_number) =
            <(u32, u64)>::decode_all(slice).map_err(|e| Error::DigestItemDecodeError(e))?;
        Ok(BabeSecondaryPlainPreDigest {
            authority_index,
            slot_number,
        })
    }

    /// Returns an iterator to list of buffers which, when concatenated, produces the SCALE
    /// encoding of that object.
    pub fn scale_encoding(&self) -> impl Iterator<Item = impl AsRef<[u8]> + Clone> + Clone {
        // TODO: don't allocate
        iter::once(parity_scale_codec::Encode::encode(&(
            self.authority_index,
            self.slot_number,
        )))
    }
}

/// BABE secondary deterministic slot assignment with VRF outputs.
#[derive(Clone)]
pub struct BabeSecondaryVRFPreDigestRef<'a> {
    /// Authority index
    pub authority_index: u32,
    /// Slot number
    pub slot_number: u64,
    /// VRF output
    pub vrf_output: &'a [u8; 32],
    /// VRF proof
    pub vrf_proof: &'a [u8; 64],
}

impl<'a> BabeSecondaryVRFPreDigestRef<'a> {
    /// Decodes a [`BabeSecondaryVRFPreDigestRef`] from a slice of bytes.
    pub fn from_slice(slice: &'a [u8]) -> Result<Self, Error> {
        if slice.len() != 4 + 8 + 32 + 64 {
            return Err(Error::TooShort);
        }

        Ok(BabeSecondaryVRFPreDigestRef {
            authority_index: u32::from_le_bytes(<[u8; 4]>::try_from(&slice[0..4]).unwrap()),
            slot_number: u64::from_le_bytes(<[u8; 8]>::try_from(&slice[4..12]).unwrap()),
            vrf_output: TryFrom::try_from(&slice[12..44]).unwrap(),
            vrf_proof: unsafe {
                // TODO: ugh, how do you even get a &[u8; 64] from a &[u8]
                &*(slice[44..108].as_ptr() as *const [u8; 64])
            },
        })
    }

    /// Returns an iterator to list of buffers which, when concatenated, produces the SCALE
    /// encoding of that object.
    pub fn scale_encoding(
        &self,
    ) -> impl Iterator<Item = impl AsRef<[u8]> + Clone + 'a> + Clone + 'a {
        // TODO: don't allocate
        let header = iter::once(either::Either::Left(parity_scale_codec::Encode::encode(&(
            self.authority_index,
            self.slot_number,
        ))));

        header
            .chain(iter::once(either::Either::Right(&self.vrf_output[..])))
            .chain(iter::once(either::Either::Right(&self.vrf_proof[..])))
    }
}

impl<'a> cmp::PartialEq<BabeSecondaryVRFPreDigestRef<'a>> for BabeSecondaryVRFPreDigestRef<'a> {
    fn eq(&self, other: &BabeSecondaryVRFPreDigestRef<'a>) -> bool {
        self.authority_index == other.authority_index
            && self.slot_number == other.slot_number
            && &self.vrf_output[..] == &other.vrf_output[..]
            && &self.vrf_proof[..] == &other.vrf_proof[..]
    }
}

impl<'a> cmp::Eq for BabeSecondaryVRFPreDigestRef<'a> {}

// This custom Debug implementation exists because `[u8; 64]` doesn't implement `Debug`
impl<'a> fmt::Debug for BabeSecondaryVRFPreDigestRef<'a> {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.debug_struct("BabeSecondaryVRFPreDigestRef").finish()
    }
}
