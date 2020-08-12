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

//! Parsing SCALE-encoded header.
//!
//! The standard format for block headers is the
//! [SCALE encoding](https://substrate.dev/docs/en/knowledgebase/advanced/codec).

use blake2::digest::{Input as _, VariableOutput as _};
use core::{cmp, convert::TryFrom, fmt, iter, slice};
use parity_scale_codec::{Decode as _, DecodeAll as _};

/// Returns a hash of a SCALE-encoded header.
///
/// Does not verify the validity of the header.
pub fn hash_from_scale_encoded_header(header: impl AsRef<[u8]>) -> [u8; 32] {
    hash_from_scale_encoded_header_vectored(iter::once(header))
}

/// Returns a hash of a SCALE-encoded header.
///
/// Can be passed a list of buffers, which, when concatenated, form the SCALE-encoded header.
///
/// Does not verify the validity of the header.
pub fn hash_from_scale_encoded_header_vectored(
    header: impl Iterator<Item = impl AsRef<[u8]>>,
) -> [u8; 32] {
    let mut out = [0; 32];

    let mut hasher = blake2::VarBlake2b::new_keyed(&[], 32);
    for buf in header {
        hasher.input(buf.as_ref());
    }
    hasher.variable_result(|result| {
        debug_assert_eq!(result.len(), 32);
        out.copy_from_slice(result)
    });

    out
}

/// Attempt to decode the given SCALE-encoded header.
pub fn decode<'a>(mut scale_encoded: &'a [u8]) -> Result<HeaderRef<'a>, Error> {
    if scale_encoded.len() < 32 + 1 {
        return Err(Error::TooShort);
    }

    let parent_hash: &[u8; 32] = TryFrom::try_from(&scale_encoded[0..32]).unwrap();
    scale_encoded = &scale_encoded[32..];

    let number: parity_scale_codec::Compact<u64> =
        parity_scale_codec::Decode::decode(&mut scale_encoded)
            .map_err(Error::BlockNumberDecodeError)?;

    if scale_encoded.len() < 32 + 32 + 1 {
        return Err(Error::TooShort);
    }

    let state_root: &[u8; 32] = TryFrom::try_from(&scale_encoded[0..32]).unwrap();
    scale_encoded = &scale_encoded[32..];
    let extrinsics_root: &[u8; 32] = TryFrom::try_from(&scale_encoded[0..32]).unwrap();
    scale_encoded = &scale_encoded[32..];

    let digest_logs_len: parity_scale_codec::Compact<u64> =
        parity_scale_codec::Decode::decode(&mut scale_encoded)
            .map_err(Error::DigestLenDecodeError)?;
    let digest = scale_encoded;

    // Iterate through the log items to see if anything is wrong.
    {
        let mut digest = digest;
        for _ in 0..digest_logs_len.0 {
            let (_, next) = decode_item(digest)?;
            digest = next;
        }

        if !digest.is_empty() {
            return Err(Error::TooLong);
        }
    }

    Ok(HeaderRef {
        parent_hash,
        number: number.0,
        state_root,
        extrinsics_root,
        digest: DigestRef {
            digest_logs_len: digest_logs_len.0,
            digest,
        },
    })
}

/// Potential error when decoding a header.
#[derive(Debug, derive_more::Display)]
pub enum Error {
    /// Header is not long enough.
    TooShort,
    /// Header is too long.
    TooLong,
    /// Error while decoding the block number.
    BlockNumberDecodeError(parity_scale_codec::Error),
    /// Error while decoding the digest length.
    DigestLenDecodeError(parity_scale_codec::Error),
    /// Error while decoding a digest log item length.
    DigestItemLenDecodeError(parity_scale_codec::Error),
    /// Error while decoding a digest item.
    DigestItemDecodeError(parity_scale_codec::Error),
    /// Digest log item with an unrecognized type.
    UnknownDigestLogType(u8),
    /// Bad length of a BABE seal.
    BadBabeSealLength,
    BadBabePreDigestRefType,
    BadBabeConsensusRefType,
    /// Unknown consensus engine specified in a digest log.
    #[display(fmt = "Unknown consensus engine specified in a digest log: {:?}", _0)]
    UnknownConsensusEngine([u8; 4]),
}

/// Header of a block, after decoding.
///
/// Note that the information in there are not guaranteed to be exact. The exactness of the
/// information depends on the context.
#[derive(Debug, Clone)]
pub struct HeaderRef<'a> {
    /// Hash of the parent block stored in the header.
    pub parent_hash: &'a [u8; 32],
    /// Block number stored in the header.
    pub number: u64,
    /// The state trie merkle root
    pub state_root: &'a [u8; 32],
    /// The merkle root of the extrinsics.
    pub extrinsics_root: &'a [u8; 32],
    /// List of auxiliary data appended to the block header.
    pub digest: DigestRef<'a>,
}

impl<'a> HeaderRef<'a> {
    /// Returns an iterator to list of buffers which, when concatenated, produces the SCALE
    /// encoding of the header.
    pub fn scale_encoding(
        &self,
    ) -> impl Iterator<Item = impl AsRef<[u8]> + Clone + 'a> + Clone + 'a {
        // TODO: don't allocate?
        let encoded_number =
            parity_scale_codec::Encode::encode(&parity_scale_codec::Compact(self.number));

        iter::once(either::Either::Left(either::Either::Left(
            &self.parent_hash[..],
        )))
        .chain(iter::once(either::Either::Left(either::Either::Right(
            encoded_number,
        ))))
        .chain(iter::once(either::Either::Left(either::Either::Left(
            &self.state_root[..],
        ))))
        .chain(iter::once(either::Either::Left(either::Either::Left(
            &self.extrinsics_root[..],
        ))))
        .chain(self.digest.scale_encoding().map(either::Either::Right))
    }

    /// Builds the hash of the header.
    pub fn hash(&self) -> [u8; 32] {
        hash_from_scale_encoded_header_vectored(self.scale_encoding())
    }
}

/// Generic header digest.
#[derive(Debug, Clone)]
pub struct DigestRef<'a> {
    /// Number of log items in the header.
    /// Must always match the actual number of items in `digest`. The validity must be verified
    /// before a [`DigestRef`] object is instantiated.
    digest_logs_len: u64,
    /// Encoded digest. Its validity must be verified before a [`DigestRef`] object is instantiated.
    digest: &'a [u8],
}

impl<'a> DigestRef<'a> {
    /// Returns a digest with empty logs.
    pub const fn empty() -> DigestRef<'a> {
        DigestRef {
            digest_logs_len: 0,
            digest: &[],
        }
    }

    /// Pops the last element of the [`DigestRef`].
    pub fn pop(&mut self) -> Option<DigestItemRef<'a>> {
        let digest_logs_len_minus_one = self.digest_logs_len.checked_sub(1)?;

        let mut iter = self.logs();
        for _ in 0..digest_logs_len_minus_one {
            let _item = iter.next();
            debug_assert!(_item.is_some());
        }

        self.digest_logs_len = digest_logs_len_minus_one;
        self.digest = &self.digest[..self.digest.len() - iter.pointer.len()];

        debug_assert_eq!(iter.remaining_len, 1);
        Some(iter.next().unwrap())
    }

    /// Returns an iterator to the log items in this digest.
    pub fn logs(&self) -> LogsIter<'a> {
        LogsIter {
            pointer: self.digest,
            remaining_len: self.digest_logs_len,
        }
    }

    /// Returns an iterator to list of buffers which, when concatenated, produces the SCALE
    /// encoding of the digest items.
    pub fn scale_encoding(
        &self,
    ) -> impl Iterator<Item = impl AsRef<[u8]> + Clone + 'a> + Clone + 'a {
        // TODO: don't allocate?
        let encoded_len =
            parity_scale_codec::Encode::encode(&parity_scale_codec::Compact(self.digest_logs_len));
        iter::once(either::Either::Left(encoded_len)).chain(
            self.logs()
                .flat_map(|v| v.scale_encoding().map(either::Either::Right)),
        )
    }
}

/// Iterator towards the digest log items.
#[derive(Debug, Clone)]
pub struct LogsIter<'a> {
    /// Encoded digest.
    pointer: &'a [u8],
    /// Number of log items remaining.
    remaining_len: u64,
}

impl<'a> Iterator for LogsIter<'a> {
    type Item = DigestItemRef<'a>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.remaining_len == 0 {
            return None;
        }

        // Validity is guaranteed when the `DigestRef` is constructed.
        let (item, new_pointer) = decode_item(self.pointer).unwrap();
        self.pointer = new_pointer;
        self.remaining_len -= 1;

        Some(item)
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        let num = usize::try_from(self.remaining_len).unwrap();
        (num, Some(num))
    }
}

impl<'a> ExactSizeIterator for LogsIter<'a> {}

// TODO: document
#[derive(Debug, PartialEq, Eq, Clone)]
pub enum DigestItemRef<'a> {
    ChangesTrieRoot(&'a [u8; 32]),
    BabePreDigest(BabePreDigestRef<'a>),
    BabeConsensus(BabeConsensusLogRef<'a>),

    /// Block signature made using the BABE consensus engine.
    ///
    /// Guaranteed to be 64 bytes long.
    // TODO: we don't use a &[u8; 64] because traits aren't defined on this type; need to fix after Rust gets proper support
    BabeSeal(&'a [u8]),
    ChangesTrieSignal(ChangesTrieSignal),
    Other(&'a [u8]),
}

impl<'a> DigestItemRef<'a> {
    /// Returns an iterator to list of buffers which, when concatenated, produces the SCALE
    /// encoding of that digest item.
    pub fn scale_encoding(
        &self,
    ) -> impl Iterator<Item = impl AsRef<[u8]> + Clone + 'a> + Clone + 'a {
        // TODO: don't use Vecs?
        match *self {
            DigestItemRef::BabePreDigest(ref babe_pre_digest) => {
                let encoded = babe_pre_digest
                    .scale_encoding()
                    .fold(Vec::new(), |mut a, b| {
                        a.extend_from_slice(b.as_ref());
                        a
                    });

                let mut ret = vec![6];
                ret.extend_from_slice(b"BABE");
                ret.extend_from_slice(&parity_scale_codec::Encode::encode(
                    &parity_scale_codec::Compact(u64::try_from(encoded.len()).unwrap()),
                ));
                ret.extend_from_slice(&encoded);
                iter::once(ret)
            }
            DigestItemRef::BabeConsensus(ref babe_consensus) => {
                let encoded = babe_consensus
                    .scale_encoding()
                    .fold(Vec::new(), |mut a, b| {
                        a.extend_from_slice(b.as_ref());
                        a
                    });

                let mut ret = vec![4];
                ret.extend_from_slice(b"BABE");
                ret.extend_from_slice(&parity_scale_codec::Encode::encode(
                    &parity_scale_codec::Compact(u64::try_from(encoded.len()).unwrap()),
                ));
                ret.extend_from_slice(&encoded);
                iter::once(ret)
            }
            DigestItemRef::BabeSeal(seal) => {
                assert_eq!(seal.len(), 64);

                let mut ret = vec![5];
                ret.extend_from_slice(b"BABE");
                ret.extend_from_slice(&parity_scale_codec::Encode::encode(
                    &parity_scale_codec::Compact(64u32),
                ));
                ret.extend_from_slice(&seal);
                iter::once(ret)
            }
            DigestItemRef::ChangesTrieSignal(ref changes) => {
                let mut ret = vec![7];
                ret.extend_from_slice(&parity_scale_codec::Encode::encode(changes));
                iter::once(ret)
            }
            DigestItemRef::ChangesTrieRoot(data) => {
                let mut ret = vec![2];
                ret.extend_from_slice(data);
                iter::once(ret)
            }
            DigestItemRef::Other(data) => {
                let mut ret = vec![0];
                ret.extend_from_slice(data);
                iter::once(ret)
            }
        }
    }
}

/// An consensus log item for BABE.
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
            authorities: BabeAuthoritiesIter(slice[0..authorities_len * 40].chunks(40)),
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

/// List of authorities in a BABE context.
#[derive(Debug, Clone)]
pub struct BabeAuthoritiesIter<'a>(slice::Chunks<'a, u8>);

impl<'a> Iterator for BabeAuthoritiesIter<'a> {
    type Item = BabeAuthorityRef<'a>;

    fn next(&mut self) -> Option<Self::Item> {
        let item = self.0.next()?;
        assert_eq!(item.len(), 40);
        Some(BabeAuthorityRef {
            public_key: <&[u8; 32]>::try_from(&item[0..32]).unwrap(),
            weight: u64::from_le_bytes(<[u8; 8]>::try_from(&item[32..40]).unwrap()),
        })
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        self.0.size_hint()
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

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub struct BabeAuthorityRef<'a> {
    /// sr25519 public key.
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

/// Decodes a single digest log item. On success, returns the item and the data that remains
/// after the item.
fn decode_item<'a>(mut slice: &'a [u8]) -> Result<(DigestItemRef<'a>, &'a [u8]), Error> {
    let index = *slice.get(0).ok_or(Error::TooShort)?;
    slice = &slice[1..];

    match index {
        0 => {
            let len: parity_scale_codec::Compact<u64> =
                parity_scale_codec::Decode::decode(&mut slice)
                    .map_err(Error::DigestItemLenDecodeError)?;

            let len = TryFrom::try_from(len.0).map_err(|_| Error::TooShort)?;

            if slice.len() < len {
                return Err(Error::TooShort);
            }

            let content = &slice[..len];
            slice = &slice[len..];
            Ok((DigestItemRef::Other(content), slice))
        }
        4 | 5 | 6 => {
            if slice.len() < 4 {
                return Err(Error::TooShort);
            }

            let engine_id: &[u8; 4] = TryFrom::try_from(&slice[..4]).unwrap();
            slice = &slice[4..];

            let len: parity_scale_codec::Compact<u64> =
                parity_scale_codec::Decode::decode(&mut slice)
                    .map_err(Error::DigestItemLenDecodeError)?;

            let len = TryFrom::try_from(len.0).map_err(|_| Error::TooShort)?;

            if slice.len() < len {
                return Err(Error::TooShort);
            }

            let content = &slice[..len];
            slice = &slice[len..];

            let item = decode_item_from_parts(index, engine_id, content)?;
            Ok((item, slice))
        }
        2 => {
            if slice.len() < 32 {
                return Err(Error::TooShort);
            }

            let hash: &[u8; 32] = TryFrom::try_from(&slice[0..32]).unwrap();
            slice = &slice[32..];
            Ok((DigestItemRef::ChangesTrieRoot(hash), slice))
        }
        7 => {
            let item = parity_scale_codec::Decode::decode(&mut slice)
                .map_err(Error::DigestItemDecodeError)?;
            Ok((DigestItemRef::ChangesTrieSignal(item), slice))
        }
        ty => Err(Error::UnknownDigestLogType(ty)),
    }
}

/// When we know the index, engine id, and content of an item, we can finish decoding.
fn decode_item_from_parts<'a>(
    index: u8,
    engine_id: &'a [u8; 4],
    content: &'a [u8],
) -> Result<DigestItemRef<'a>, Error> {
    Ok(match (index, engine_id) {
        (4, b"BABE") => DigestItemRef::BabeConsensus(BabeConsensusLogRef::from_slice(content)?),
        (4, e) => return Err(Error::UnknownConsensusEngine(*e)),
        (5, b"BABE") => DigestItemRef::BabeSeal({
            if content.len() != 64 {
                return Err(Error::BadBabeSealLength);
            }
            content
        }),
        (5, e) => return Err(Error::UnknownConsensusEngine(*e)),
        (6, b"BABE") => DigestItemRef::BabePreDigest(BabePreDigestRef::from_slice(content)?),
        (6, e) => return Err(Error::UnknownConsensusEngine(*e)),
        _ => unreachable!(),
    })
}
