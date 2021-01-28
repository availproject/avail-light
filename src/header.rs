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

//! Parsing SCALE-encoded header.
//!
//! Each block of a chain is composed of two parts: its header, and its body.
//!
//! The header of a block consists in a list of hardcoded fields such as the parent block's hash
//! or the block number, and a variable-sized list of log items.
//!
//! The standard format of a block header is the
//! [SCALE encoding](https://substrate.dev/docs/en/knowledgebase/advanced/codec). It is typically
//! under this encoding that block headers are for example transferred over the network or stored
//! in the database. Use the [`decode`] function in order to decompose a SCALE-encoded header
//! into a usable [`HeaderRef`].
//!
//! # Example
//!
//! ```
//! // Example encoded header.
//! let scale_encoded_header: &[u8] = &[
//!     246, 90, 76, 223, 195, 230, 202, 111, 120, 197, 6, 9, 90, 164, 170, 8, 194, 57, 184, 75,
//!     95, 67, 240, 169, 62, 244, 171, 95, 237, 85, 86, 1, 122, 169, 8, 0, 138, 149, 72, 185, 56,
//!     62, 30, 76, 117, 134, 123, 62, 4, 132, 23, 143, 200, 150, 171, 42, 63, 19, 173, 21, 89, 98,
//!     38, 175, 43, 132, 69, 75, 96, 168, 82, 108, 19, 182, 130, 230, 161, 43, 7, 225, 20, 229,
//!     92, 103, 57, 188, 151, 170, 16, 8, 126, 122, 98, 131, 121, 43, 181, 19, 180, 228, 8, 6, 66,
//!     65, 66, 69, 181, 1, 3, 1, 0, 0, 0, 250, 8, 207, 15, 0, 0, 0, 0, 86, 157, 105, 202, 151,
//!     254, 95, 169, 249, 150, 219, 194, 195, 143, 181, 39, 43, 87, 179, 157, 152, 191, 40, 255,
//!     23, 66, 18, 249, 93, 170, 58, 15, 178, 210, 130, 18, 66, 244, 232, 119, 74, 190, 92, 145,
//!     33, 192, 195, 176, 125, 217, 124, 33, 167, 97, 64, 63, 149, 200, 220, 191, 64, 134, 232, 9,
//!     3, 178, 186, 150, 130, 105, 25, 148, 218, 35, 208, 226, 112, 85, 184, 237, 23, 243, 86, 81,
//!     27, 127, 188, 223, 162, 244, 26, 77, 234, 116, 24, 11, 5, 66, 65, 66, 69, 1, 1, 112, 68,
//!     111, 83, 145, 78, 98, 96, 247, 64, 179, 237, 113, 175, 125, 177, 110, 39, 185, 55, 156,
//!     197, 177, 225, 226, 90, 238, 223, 115, 193, 185, 35, 67, 216, 98, 25, 55, 225, 224, 19, 43,
//!     255, 226, 125, 22, 160, 33, 182, 222, 213, 150, 40, 108, 108, 124, 254, 140, 228, 155, 29,
//!     250, 193, 65, 140,
//! ];
//!
//! // Decoding the header can panic if it is malformed. Do not unwrap if, for example, the
//! // header has been received from a remote!
//! let decoded_header = smoldot::header::decode(&scale_encoded_header).unwrap();
//!
//! println!("Block hash: {:?}", decoded_header.hash());
//! println!("Header number: {}", decoded_header.number);
//! println!("Parent block hash: {:?}", decoded_header.parent_hash);
//! for item in decoded_header.digest.logs() {
//!     println!("Digest item: {:?}", item);
//! }
//!
//! // Call `scale_encoding` to produce the header encoding.
//! let reencoded: Vec<u8> = decoded_header
//!     .scale_encoding()
//!     .fold(Vec::new(), |mut a, b| { a.extend_from_slice(b.as_ref()); a });
//! assert_eq!(reencoded, scale_encoded_header);
//! ```

// TODO: consider rewriting the encoding/decoding into a more legible style
// TODO: consider nom for decoding

use crate::util;

use alloc::{vec, vec::Vec};
use core::{convert::TryFrom, fmt, iter, slice};

mod aura;
mod babe;
mod grandpa;

pub use aura::*;
pub use babe::*;
pub use grandpa::*;

/// Returns a hash of a SCALE-encoded header.
///
/// Does not verify the validity of the header.
pub fn hash_from_scale_encoded_header(header: impl AsRef<[u8]>) -> [u8; 32] {
    hash_from_scale_encoded_header_vectored(iter::once(header))
}

/// Returns a hash of a SCALE-encoded header.
///
/// Must be passed a list of buffers, which, when concatenated, form the SCALE-encoded header.
///
/// Does not verify the validity of the header.
pub fn hash_from_scale_encoded_header_vectored(
    header: impl Iterator<Item = impl AsRef<[u8]>>,
) -> [u8; 32] {
    let mut hasher = blake2_rfc::blake2b::Blake2b::with_key(32, &[]);
    for buf in header {
        hasher.update(buf.as_ref());
    }

    let result = hasher.finalize();
    debug_assert_eq!(result.as_bytes().len(), 32);

    let mut out = [0; 32];
    out.copy_from_slice(result.as_bytes());
    out
}

/// Attempt to decode the given SCALE-encoded header.
pub fn decode(scale_encoded: &[u8]) -> Result<HeaderRef, Error> {
    let (header, remainder) = decode_partial(scale_encoded)?;
    if !remainder.is_empty() {
        return Err(Error::TooLong);
    }

    Ok(header)
}

/// Attempt to decode the given SCALE-encoded header.
///
/// Contrary to [`decode`], doesn't return an error if the slice is too long but returns the
/// remainder.
pub fn decode_partial(mut scale_encoded: &[u8]) -> Result<(HeaderRef, &[u8]), Error> {
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

    let (digest, remainder) = DigestRef::from_scale_bytes(scale_encoded)?;

    let header = HeaderRef {
        parent_hash,
        number: number.0,
        state_root,
        extrinsics_root,
        digest,
    };

    Ok((header, remainder))
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
    /// Found a seal that isn't the last item in the list.
    SealIsntLastItem,
    /// Bad length of an AURA seal.
    BadAuraSealLength,
    BadAuraConsensusRefType,
    BadAuraAuthoritiesListLen,
    /// There are multiple Aura pre-runtime digests in the block header.
    MultipleAuraPreRuntimeDigests,
    /// Bad length of a BABE seal.
    BadBabeSealLength,
    BadBabePreDigestRefType,
    BadBabeConsensusRefType,
    /// There are multiple Babe pre-runtime digests in the block header.
    MultipleBabePreRuntimeDigests,
    /// There are multiple Babe epoch descriptor digests in the block header.
    MultipleBabeEpochDescriptors,
    /// There are multiple Babe configuration descriptor digests in the block header.
    MultipleBabeConfigDescriptors,
    /// Found a Babe configuration change digest without an epoch change digest.
    UnexpectedBabeConfigDescriptor,
    GrandpaConsensusLogDecodeError,
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

    /// Equivalent to [`HeaderRef::scale_encoding`] but returns the data in a `Vec`.
    pub fn scale_encoding_vec(&self) -> Vec<u8> {
        // TODO: Vec::with_capacity?
        self.scale_encoding().fold(Vec::new(), |mut a, b| {
            a.extend_from_slice(b.as_ref());
            a
        })
    }

    /// Builds the hash of the header.
    pub fn hash(&self) -> [u8; 32] {
        hash_from_scale_encoded_header_vectored(self.scale_encoding())
    }
}

impl<'a> From<&'a Header> for HeaderRef<'a> {
    fn from(a: &'a Header) -> HeaderRef<'a> {
        HeaderRef {
            parent_hash: &a.parent_hash,
            number: a.number,
            state_root: &a.state_root,
            extrinsics_root: &a.extrinsics_root,
            digest: (&a.digest).into(),
        }
    }
}

/// Header of a block, after decoding.
///
/// Note that the information in there are not guaranteed to be exact. The exactness of the
/// information depends on the context.
#[derive(Debug, Clone)]
pub struct Header {
    /// Hash of the parent block stored in the header.
    pub parent_hash: [u8; 32],
    /// Block number stored in the header.
    pub number: u64,
    /// The state trie merkle root
    pub state_root: [u8; 32],
    /// The merkle root of the extrinsics.
    pub extrinsics_root: [u8; 32],
    /// List of auxiliary data appended to the block header.
    pub digest: Digest,
}

impl Header {
    /// Returns an iterator to list of buffers which, when concatenated, produces the SCALE
    /// encoding of the header.
    pub fn scale_encoding<'a>(
        &'a self,
    ) -> impl Iterator<Item = impl AsRef<[u8]> + Clone + 'a> + Clone + 'a {
        HeaderRef::from(self).scale_encoding()
    }

    /// Builds the hash of the header.
    pub fn hash(&self) -> [u8; 32] {
        HeaderRef::from(self).hash()
    }
}

impl<'a> From<HeaderRef<'a>> for Header {
    fn from(a: HeaderRef<'a>) -> Header {
        Header {
            parent_hash: *a.parent_hash,
            number: a.number,
            state_root: *a.state_root,
            extrinsics_root: *a.extrinsics_root,
            digest: a.digest.into(),
        }
    }
}

/// Generic header digest.
#[derive(Clone)]
pub struct DigestRef<'a> {
    /// Actual source of digest items.
    inner: DigestRefInner<'a>,
    /// Index of the [`DigestItemRef::AuraSeal`] item, if any.
    aura_seal_index: Option<usize>,
    /// Index of the [`DigestItemRef::AuraPreDigest`] item, if any.
    aura_predigest_index: Option<usize>,
    /// Index of the [`DigestItemRef::BabeSeal`] item, if any.
    babe_seal_index: Option<usize>,
    /// Index of the [`DigestItemRef::BabePreDigest`] item, if any.
    babe_predigest_index: Option<usize>,
    /// Index of the [`DigestItemRef::BabeConsensus`] item containing a
    /// [`BabeConsensusLogRef::NextEpochData`], if any.
    babe_next_epoch_data_index: Option<usize>,
    /// Index of the [`DigestItemRef::BabeConsensus`] item containing a
    /// [`BabeConsensusLogRef::NextConfigData`], if any.
    babe_next_config_data_index: Option<usize>,
}

#[derive(Clone)]
enum DigestRefInner<'a> {
    /// Source of data is an undecoded slice of bytes.
    Undecoded {
        /// Number of log items in the header.
        /// Must always match the actual number of items in [`DigestRefInner::digest`]. The
        /// validity must be verified before a [`DigestRef`] object is instantiated.
        digest_logs_len: usize,
        /// Encoded digest. Its validity must be verified before a [`DigestRef`] object is
        /// instantiated.
        digest: &'a [u8],
    },
    Parsed(&'a [DigestItem]),
}

impl<'a> DigestRef<'a> {
    /// Returns a digest with empty logs.
    pub fn empty() -> DigestRef<'a> {
        DigestRef {
            inner: DigestRefInner::Parsed(&[]),
            aura_seal_index: None,
            aura_predigest_index: None,
            babe_seal_index: None,
            babe_predigest_index: None,
            babe_next_epoch_data_index: None,
            babe_next_config_data_index: None,
        }
    }

    /// Returns true if the list has any item that belong to the Aura consensus engine.
    pub fn has_any_aura(&self) -> bool {
        self.logs().any(|l| l.is_aura())
    }

    /// Returns true if the list has any item that belong to the Babe consensus engine.
    pub fn has_any_babe(&self) -> bool {
        self.logs().any(|l| l.is_babe())
    }

    /// Returns the Aura seal digest item, if any.
    pub fn aura_seal(&self) -> Option<&'a [u8; 64]> {
        if let Some(aura_seal_index) = self.aura_seal_index {
            if let DigestItemRef::AuraSeal(seal) = self.logs().nth(aura_seal_index).unwrap() {
                Some(seal)
            } else {
                unreachable!()
            }
        } else {
            None
        }
    }

    /// Returns the Aura pre-runtime digest item, if any.
    pub fn aura_pre_runtime(&self) -> Option<AuraPreDigest> {
        if let Some(aura_predigest_index) = self.aura_predigest_index {
            if let DigestItemRef::AuraPreDigest(item) =
                self.logs().nth(aura_predigest_index).unwrap()
            {
                Some(item)
            } else {
                unreachable!()
            }
        } else {
            None
        }
    }

    /// Returns the Babe seal digest item, if any.
    pub fn babe_seal(&self) -> Option<&'a [u8; 64]> {
        if let Some(babe_seal_index) = self.babe_seal_index {
            if let DigestItemRef::BabeSeal(seal) = self.logs().nth(babe_seal_index).unwrap() {
                Some(seal)
            } else {
                unreachable!()
            }
        } else {
            None
        }
    }

    /// Returns the Babe pre-runtime digest item, if any.
    pub fn babe_pre_runtime(&self) -> Option<BabePreDigestRef<'a>> {
        if let Some(babe_predigest_index) = self.babe_predigest_index {
            if let DigestItemRef::BabePreDigest(item) =
                self.logs().nth(babe_predigest_index).unwrap()
            {
                Some(item)
            } else {
                unreachable!()
            }
        } else {
            None
        }
    }

    /// Returns the Babe epoch information stored in the header, if any.
    ///
    /// It is guaranteed that a configuration change is present only if an epoch change is
    /// present too.
    pub fn babe_epoch_information(&self) -> Option<(BabeNextEpochRef<'a>, Option<BabeNextConfig>)> {
        if let Some(babe_next_epoch_data_index) = self.babe_next_epoch_data_index {
            if let DigestItemRef::BabeConsensus(BabeConsensusLogRef::NextEpochData(epoch)) =
                self.logs().nth(babe_next_epoch_data_index).unwrap()
            {
                if let Some(babe_next_config_data_index) = self.babe_next_config_data_index {
                    if let DigestItemRef::BabeConsensus(BabeConsensusLogRef::NextConfigData(
                        config,
                    )) = self.logs().nth(babe_next_config_data_index).unwrap()
                    {
                        Some((epoch, Some(config)))
                    } else {
                        panic!()
                    }
                } else {
                    Some((epoch, None))
                }
            } else {
                unreachable!()
            }
        } else {
            debug_assert!(self.babe_next_config_data_index.is_none());
            None
        }
    }

    /// If the last element of the list is a seal, removes it from the [`DigestRef`].
    pub fn pop_seal(&mut self) -> Option<Seal<'a>> {
        let seal_pos = self.babe_seal_index.or(self.aura_seal_index)?;

        match &mut self.inner {
            DigestRefInner::Parsed(list) => {
                debug_assert!(!list.is_empty());
                debug_assert_eq!(seal_pos, list.len() - 1);

                let item = &list[seal_pos];
                *list = &list[..seal_pos];

                match item {
                    DigestItem::AuraSeal(seal) => Some(Seal::Aura(seal)),
                    DigestItem::BabeSeal(seal) => Some(Seal::Babe(seal)),
                    _ => unreachable!(),
                }
            }

            DigestRefInner::Undecoded {
                digest,
                digest_logs_len,
            } => {
                debug_assert_eq!(seal_pos, *digest_logs_len - 1);

                let mut iter = LogsIter {
                    inner: LogsIterInner::Undecoded {
                        pointer: *digest,
                        remaining_len: *digest_logs_len,
                    },
                };
                for _ in 0..seal_pos {
                    let _item = iter.next();
                    debug_assert!(_item.is_some());
                }

                if let LogsIterInner::Undecoded {
                    pointer,
                    remaining_len,
                } = iter.inner
                {
                    *digest_logs_len -= 1;
                    *digest = &digest[..digest.len() - pointer.len()];
                    self.babe_seal_index = None;
                    debug_assert_eq!(remaining_len, 1);
                } else {
                    unreachable!()
                }

                match iter.next() {
                    Some(DigestItemRef::AuraSeal(seal)) => Some(Seal::Aura(seal)),
                    Some(DigestItemRef::BabeSeal(seal)) => Some(Seal::Babe(seal)),
                    _ => unreachable!(),
                }
            }
        }
    }

    /// Returns an iterator to the log items in this digest.
    pub fn logs(&self) -> LogsIter<'a> {
        LogsIter {
            inner: match self.inner {
                DigestRefInner::Parsed(list) => LogsIterInner::Decoded(list.iter()),
                DigestRefInner::Undecoded {
                    digest,
                    digest_logs_len,
                } => LogsIterInner::Undecoded {
                    pointer: digest,
                    remaining_len: digest_logs_len,
                },
            },
        }
    }

    /// Returns an iterator to list of buffers which, when concatenated, produces the SCALE
    /// encoding of the digest items.
    pub fn scale_encoding(
        &self,
    ) -> impl Iterator<Item = impl AsRef<[u8]> + Clone + 'a> + Clone + 'a {
        let encoded_len = util::encode_scale_compact_usize(self.logs().len());
        iter::once(either::Left(encoded_len)).chain(
            self.logs()
                .flat_map(|v| v.scale_encoding().map(either::Right)),
        )
    }

    /// Turns an already-decoded list of items into a [`DigestRef`].
    ///
    /// Error can happen if the list of items is invalid, for example if it contains a seal at the
    /// non-last position.
    pub fn from_slice(slice: &'a [DigestItem]) -> Result<Self, Error> {
        let mut aura_seal_index = None;
        let mut aura_predigest_index = None;
        let mut babe_seal_index = None;
        let mut babe_predigest_index = None;
        let mut babe_next_epoch_data_index = None;
        let mut babe_next_config_data_index = None;

        // Iterate through the log items to see if anything is wrong.
        for (item_num, item) in slice.iter().enumerate() {
            match item {
                DigestItem::AuraPreDigest(_) if aura_predigest_index.is_none() => {
                    aura_predigest_index = Some(item_num);
                }
                DigestItem::AuraPreDigest(_) => return Err(Error::MultipleAuraPreRuntimeDigests),
                DigestItem::ChangesTrieRoot(_) => {}
                DigestItem::AuraConsensus(_) => {}
                DigestItem::BabePreDigest(_) if babe_predigest_index.is_none() => {
                    babe_predigest_index = Some(item_num);
                }
                DigestItem::BabePreDigest(_) => return Err(Error::MultipleBabePreRuntimeDigests),
                DigestItem::BabeConsensus(BabeConsensusLog::NextEpochData(_))
                    if babe_next_epoch_data_index.is_none() =>
                {
                    babe_next_epoch_data_index = Some(item_num);
                }
                DigestItem::BabeConsensus(BabeConsensusLog::NextEpochData(_)) => {
                    return Err(Error::MultipleBabeEpochDescriptors);
                }
                DigestItem::BabeConsensus(BabeConsensusLog::NextConfigData(_))
                    if babe_next_config_data_index.is_none() =>
                {
                    babe_next_config_data_index = Some(item_num);
                }
                DigestItem::BabeConsensus(BabeConsensusLog::NextConfigData(_)) => {
                    return Err(Error::MultipleBabeConfigDescriptors);
                }
                DigestItem::BabeConsensus(BabeConsensusLog::OnDisabled(_)) => {}
                DigestItem::GrandpaConsensus(_) => {}
                DigestItem::AuraSeal(_) if item_num == slice.len() - 1 => {
                    debug_assert!(aura_seal_index.is_none());
                    debug_assert!(babe_seal_index.is_none());
                    aura_seal_index = Some(item_num);
                }
                DigestItem::AuraSeal(_) => return Err(Error::SealIsntLastItem),
                DigestItem::BabeSeal(_) if item_num == slice.len() - 1 => {
                    debug_assert!(aura_seal_index.is_none());
                    debug_assert!(babe_seal_index.is_none());
                    babe_seal_index = Some(item_num);
                }
                DigestItem::BabeSeal(_) => return Err(Error::SealIsntLastItem),
                DigestItem::ChangesTrieSignal(_) => {}
            }
        }

        if babe_next_config_data_index.is_some() && babe_next_epoch_data_index.is_none() {
            return Err(Error::UnexpectedBabeConfigDescriptor);
        }

        Ok(DigestRef {
            inner: DigestRefInner::Parsed(slice),
            aura_seal_index,
            aura_predigest_index,
            babe_seal_index,
            babe_predigest_index,
            babe_next_epoch_data_index,
            babe_next_config_data_index,
        })
    }

    /// Try to decode a list of digest items, from their SCALE encoding.
    fn from_scale_bytes(mut scale_encoded: &'a [u8]) -> Result<(Self, &'a [u8]), Error> {
        let digest_logs_len = {
            let len: parity_scale_codec::Compact<u64> =
                parity_scale_codec::Decode::decode(&mut scale_encoded)
                    .map_err(Error::DigestLenDecodeError)?;
            // If the number of digest items can't fit in a `usize`, we know that the buffer can't
            // be large enough to hold all these items, hence the `TooShort`.
            usize::try_from(len.0).map_err(|_| Error::TooShort)?
        };

        let mut aura_seal_index = None;
        let mut aura_predigest_index = None;
        let mut babe_seal_index = None;
        let mut babe_predigest_index = None;
        let mut babe_next_epoch_data_index = None;
        let mut babe_next_config_data_index = None;

        // Iterate through the log items to see if anything is wrong.
        let mut next_digest = scale_encoded;
        for item_num in 0..digest_logs_len {
            let (item, next) = decode_item(next_digest)?;
            next_digest = next;

            match item {
                DigestItemRef::AuraPreDigest(_) if aura_predigest_index.is_none() => {
                    aura_predigest_index = Some(item_num);
                }
                DigestItemRef::AuraPreDigest(_) => {
                    return Err(Error::MultipleAuraPreRuntimeDigests)
                }
                DigestItemRef::ChangesTrieRoot(_) => {}
                DigestItemRef::AuraConsensus(_) => {}
                DigestItemRef::BabePreDigest(_) if babe_predigest_index.is_none() => {
                    babe_predigest_index = Some(item_num);
                }
                DigestItemRef::BabePreDigest(_) => {
                    return Err(Error::MultipleBabePreRuntimeDigests)
                }
                DigestItemRef::BabeConsensus(BabeConsensusLogRef::NextEpochData(_))
                    if babe_next_epoch_data_index.is_none() =>
                {
                    babe_next_epoch_data_index = Some(item_num);
                }
                DigestItemRef::BabeConsensus(BabeConsensusLogRef::NextEpochData(_)) => {
                    return Err(Error::MultipleBabeEpochDescriptors);
                }
                DigestItemRef::BabeConsensus(BabeConsensusLogRef::NextConfigData(_))
                    if babe_next_config_data_index.is_none() =>
                {
                    babe_next_config_data_index = Some(item_num);
                }
                DigestItemRef::BabeConsensus(BabeConsensusLogRef::NextConfigData(_)) => {
                    return Err(Error::MultipleBabeConfigDescriptors);
                }
                DigestItemRef::BabeConsensus(BabeConsensusLogRef::OnDisabled(_)) => {}
                DigestItemRef::GrandpaConsensus(_) => {}
                DigestItemRef::AuraSeal(_) if item_num == digest_logs_len - 1 => {
                    debug_assert!(aura_seal_index.is_none());
                    debug_assert!(babe_seal_index.is_none());
                    aura_seal_index = Some(item_num);
                }
                DigestItemRef::AuraSeal(_) => return Err(Error::SealIsntLastItem),
                DigestItemRef::BabeSeal(_) if item_num == digest_logs_len - 1 => {
                    debug_assert!(aura_seal_index.is_none());
                    debug_assert!(babe_seal_index.is_none());
                    babe_seal_index = Some(item_num);
                }
                DigestItemRef::BabeSeal(_) => return Err(Error::SealIsntLastItem),
                DigestItemRef::ChangesTrieSignal(_) => {}
            }
        }

        if babe_next_config_data_index.is_some() && babe_next_epoch_data_index.is_none() {
            return Err(Error::UnexpectedBabeConfigDescriptor);
        }

        let out = DigestRef {
            inner: DigestRefInner::Undecoded {
                digest_logs_len,
                digest: scale_encoded,
            },
            aura_seal_index,
            aura_predigest_index,
            babe_seal_index,
            babe_predigest_index,
            babe_next_epoch_data_index,
            babe_next_config_data_index,
        };

        Ok((out, next_digest))
    }
}

impl<'a> fmt::Debug for DigestRef<'a> {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.debug_list().entries(self.logs()).finish()
    }
}

impl<'a> From<&'a Digest> for DigestRef<'a> {
    fn from(digest: &'a Digest) -> DigestRef<'a> {
        DigestRef {
            inner: DigestRefInner::Parsed(&digest.list),
            aura_seal_index: digest.aura_seal_index,
            aura_predigest_index: digest.aura_predigest_index,
            babe_seal_index: digest.babe_seal_index,
            babe_predigest_index: digest.babe_predigest_index,
            babe_next_epoch_data_index: digest.babe_next_epoch_data_index,
            babe_next_config_data_index: digest.babe_next_config_data_index,
        }
    }
}

/// Seal poped using [`DigestRef::pop_seal`].
pub enum Seal<'a> {
    Aura(&'a [u8; 64]),
    Babe(&'a [u8; 64]),
}

/// Generic header digest.
#[derive(Clone)]
pub struct Digest {
    /// Actual list of items.
    list: Vec<DigestItem>,
    /// Index of the [`DigestItemRef::AuraSeal`] item, if any.
    aura_seal_index: Option<usize>,
    /// Index of the [`DigestItemRef::AuraPreDigest`] item, if any.
    aura_predigest_index: Option<usize>,
    /// Index of the [`DigestItemRef::BabeSeal`] item, if any.
    babe_seal_index: Option<usize>,
    /// Index of the [`DigestItemRef::BabePreDigest`] item, if any.
    babe_predigest_index: Option<usize>,
    /// Index of the [`DigestItemRef::BabeConsensus`] item containing a
    /// [`BabeConsensusLogRef::NextEpochData`], if any.
    babe_next_epoch_data_index: Option<usize>,
    /// Index of the [`DigestItemRef::BabeConsensus`] item containing a
    /// [`BabeConsensusLogRef::NextConfigData`], if any.
    babe_next_config_data_index: Option<usize>,
}

impl Digest {
    /// Returns an iterator to the log items in this digest.
    pub fn logs(&self) -> LogsIter {
        DigestRef::from(self).logs()
    }

    /// Returns the Aura seal digest item, if any.
    pub fn aura_seal(&self) -> Option<&[u8; 64]> {
        DigestRef::from(self).aura_seal()
    }

    /// Pushes an Aura seal at the end of the list. Returns an error if there is already an Aura
    /// seal.
    pub fn push_aura_seal(&mut self, seal: [u8; 64]) -> Result<(), ()> {
        if self.aura_seal_index.is_none() {
            self.aura_seal_index = Some(self.list.len());
            self.list.push(DigestItem::AuraSeal(seal));
            Ok(())
        } else {
            Err(())
        }
    }

    /// Returns the Babe seal digest item, if any.
    pub fn babe_seal(&self) -> Option<&[u8; 64]> {
        DigestRef::from(self).babe_seal()
    }

    /// Pushes a Babe seal at the end of the list. Returns an error if there is already a Babe
    /// seal.
    pub fn push_babe_seal(&mut self, seal: [u8; 64]) -> Result<(), ()> {
        if self.babe_seal_index.is_none() {
            self.babe_seal_index = Some(self.list.len());
            self.list.push(DigestItem::BabeSeal(seal));
            Ok(())
        } else {
            Err(())
        }
    }

    /// Returns the Babe pre-runtime digest item, if any.
    pub fn babe_pre_runtime(&self) -> Option<BabePreDigestRef> {
        DigestRef::from(self).babe_pre_runtime()
    }

    /// Returns the Babe epoch information stored in the header, if any.
    ///
    /// It is guaranteed that a configuration change is present only if an epoch change is
    /// present too.
    pub fn babe_epoch_information(&self) -> Option<(BabeNextEpochRef, Option<BabeNextConfig>)> {
        DigestRef::from(self).babe_epoch_information()
    }
}

impl fmt::Debug for Digest {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.debug_list()
            .entries(self.list.iter().map(DigestItemRef::from))
            .finish()
    }
}

impl<'a> From<DigestRef<'a>> for Digest {
    fn from(digest: DigestRef<'a>) -> Digest {
        Digest {
            list: digest.logs().map(Into::into).collect(),
            aura_seal_index: digest.aura_seal_index,
            aura_predigest_index: digest.aura_predigest_index,
            babe_seal_index: digest.babe_seal_index,
            babe_predigest_index: digest.babe_predigest_index,
            babe_next_epoch_data_index: digest.babe_next_epoch_data_index,
            babe_next_config_data_index: digest.babe_next_config_data_index,
        }
    }
}

/// Iterator towards the digest log items.
#[derive(Clone)]
pub struct LogsIter<'a> {
    inner: LogsIterInner<'a>,
}

#[derive(Clone)]
enum LogsIterInner<'a> {
    Decoded(slice::Iter<'a, DigestItem>),
    Undecoded {
        /// Encoded digest.
        pointer: &'a [u8],
        /// Number of log items remaining.
        remaining_len: usize,
    },
}

impl<'a> Iterator for LogsIter<'a> {
    type Item = DigestItemRef<'a>;

    fn next(&mut self) -> Option<Self::Item> {
        match &mut self.inner {
            LogsIterInner::Decoded(iter) => iter.next().map(Into::into),
            LogsIterInner::Undecoded {
                pointer,
                remaining_len,
            } => {
                if *remaining_len == 0 {
                    return None;
                }

                // Validity is guaranteed when the `DigestRef` is constructed.
                let (item, new_pointer) = decode_item(*pointer).unwrap();
                *pointer = new_pointer;
                *remaining_len -= 1;

                Some(item)
            }
        }
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        match &self.inner {
            LogsIterInner::Decoded(iter) => iter.size_hint(),
            LogsIterInner::Undecoded { remaining_len, .. } => {
                (*remaining_len, Some(*remaining_len))
            }
        }
    }
}

impl<'a> ExactSizeIterator for LogsIter<'a> {}

// TODO: document
#[derive(Debug, PartialEq, Eq, Clone)]
pub enum DigestItemRef<'a> {
    AuraPreDigest(AuraPreDigest),
    /// Block signature made using the AURA consensus engine.
    AuraSeal(&'a [u8; 64]),
    AuraConsensus(AuraConsensusLogRef<'a>),

    BabePreDigest(BabePreDigestRef<'a>),
    BabeConsensus(BabeConsensusLogRef<'a>),
    /// Block signature made using the BABE consensus engine.
    BabeSeal(&'a [u8; 64]),

    GrandpaConsensus(GrandpaConsensusLogRef<'a>),

    ChangesTrieRoot(&'a [u8; 32]),
    ChangesTrieSignal(ChangesTrieSignal),
}

impl<'a> DigestItemRef<'a> {
    /// True if the item is relevant to the Aura consensus engine.
    pub fn is_aura(&self) -> bool {
        match self {
            DigestItemRef::AuraPreDigest(_) => true,
            DigestItemRef::AuraSeal(_) => true,
            DigestItemRef::AuraConsensus(_) => true,
            _ => false,
        }
    }

    /// True if the item is relevant to the Babe consensus engine.
    pub fn is_babe(&self) -> bool {
        match self {
            DigestItemRef::BabePreDigest(_) => true,
            DigestItemRef::BabeConsensus(_) => true,
            DigestItemRef::BabeSeal(_) => true,
            _ => false,
        }
    }

    /// Returns an iterator to list of buffers which, when concatenated, produces the SCALE
    /// encoding of that digest item.
    pub fn scale_encoding(
        &self,
    ) -> impl Iterator<Item = impl AsRef<[u8]> + Clone + 'a> + Clone + 'a {
        // TODO: don't use Vecs?
        match *self {
            DigestItemRef::AuraPreDigest(ref aura_pre_digest) => {
                let encoded = aura_pre_digest
                    .scale_encoding()
                    .fold(Vec::new(), |mut a, b| {
                        a.extend_from_slice(b.as_ref());
                        a
                    });

                let mut ret = vec![6];
                ret.extend_from_slice(b"aura");
                ret.extend_from_slice(util::encode_scale_compact_usize(encoded.len()).as_ref());
                ret.extend_from_slice(&encoded);
                iter::once(ret)
            }
            DigestItemRef::AuraSeal(seal) => {
                assert_eq!(seal.len(), 64);

                let mut ret = vec![5];
                ret.extend_from_slice(b"aura");
                ret.extend_from_slice(util::encode_scale_compact_usize(64).as_ref());
                ret.extend_from_slice(seal);
                iter::once(ret)
            }
            DigestItemRef::AuraConsensus(ref aura_consensus) => {
                let encoded = aura_consensus
                    .scale_encoding()
                    .fold(Vec::new(), |mut a, b| {
                        a.extend_from_slice(b.as_ref());
                        a
                    });

                let mut ret = vec![4];
                ret.extend_from_slice(b"aura");
                ret.extend_from_slice(util::encode_scale_compact_usize(encoded.len()).as_ref());
                ret.extend_from_slice(&encoded);
                iter::once(ret)
            }
            DigestItemRef::BabePreDigest(ref babe_pre_digest) => {
                let encoded = babe_pre_digest
                    .scale_encoding()
                    .fold(Vec::new(), |mut a, b| {
                        a.extend_from_slice(b.as_ref());
                        a
                    });

                let mut ret = vec![6];
                ret.extend_from_slice(b"BABE");
                ret.extend_from_slice(util::encode_scale_compact_usize(encoded.len()).as_ref());
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
                ret.extend_from_slice(util::encode_scale_compact_usize(encoded.len()).as_ref());
                ret.extend_from_slice(&encoded);
                iter::once(ret)
            }
            DigestItemRef::GrandpaConsensus(ref gp_consensus) => {
                let encoded = gp_consensus.scale_encoding().fold(Vec::new(), |mut a, b| {
                    a.extend_from_slice(b.as_ref());
                    a
                });

                let mut ret = vec![4];
                ret.extend_from_slice(b"FRNK");
                ret.extend_from_slice(util::encode_scale_compact_usize(encoded.len()).as_ref());
                ret.extend_from_slice(&encoded);
                iter::once(ret)
            }
            DigestItemRef::BabeSeal(seal) => {
                assert_eq!(seal.len(), 64);

                let mut ret = vec![5];
                ret.extend_from_slice(b"BABE");
                ret.extend_from_slice(util::encode_scale_compact_usize(64).as_ref());
                ret.extend_from_slice(seal);
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
        }
    }
}

impl<'a> From<&'a DigestItem> for DigestItemRef<'a> {
    fn from(a: &'a DigestItem) -> DigestItemRef<'a> {
        match a {
            DigestItem::AuraPreDigest(v) => DigestItemRef::AuraPreDigest(v.clone()),
            DigestItem::AuraConsensus(v) => DigestItemRef::AuraConsensus(v.into()),
            DigestItem::AuraSeal(v) => DigestItemRef::AuraSeal(v),
            DigestItem::BabePreDigest(v) => DigestItemRef::BabePreDigest(v.into()),
            DigestItem::BabeConsensus(v) => DigestItemRef::BabeConsensus(v.into()),
            DigestItem::BabeSeal(v) => DigestItemRef::BabeSeal(v),
            DigestItem::GrandpaConsensus(v) => DigestItemRef::GrandpaConsensus(v.into()),
            DigestItem::ChangesTrieRoot(v) => DigestItemRef::ChangesTrieRoot(v),
            DigestItem::ChangesTrieSignal(v) => DigestItemRef::ChangesTrieSignal(v.clone()),
        }
    }
}

// TODO: document
#[derive(Debug, Clone)]
pub enum DigestItem {
    AuraPreDigest(AuraPreDigest),
    AuraConsensus(AuraConsensusLog),
    /// Block signature made using the AURA consensus engine.
    AuraSeal([u8; 64]),

    BabePreDigest(BabePreDigest),
    BabeConsensus(BabeConsensusLog),
    /// Block signature made using the BABE consensus engine.
    BabeSeal([u8; 64]),

    GrandpaConsensus(GrandpaConsensusLog),

    ChangesTrieRoot([u8; 32]),
    ChangesTrieSignal(ChangesTrieSignal),
}

impl<'a> From<DigestItemRef<'a>> for DigestItem {
    fn from(a: DigestItemRef<'a>) -> DigestItem {
        match a {
            DigestItemRef::AuraPreDigest(v) => DigestItem::AuraPreDigest(v),
            DigestItemRef::AuraConsensus(v) => DigestItem::AuraConsensus(v.into()),
            DigestItemRef::AuraSeal(v) => {
                let mut seal = [0; 64];
                seal.copy_from_slice(v);
                DigestItem::AuraSeal(seal)
            }
            DigestItemRef::BabePreDigest(v) => DigestItem::BabePreDigest(v.into()),
            DigestItemRef::BabeConsensus(v) => DigestItem::BabeConsensus(v.into()),
            DigestItemRef::BabeSeal(v) => {
                let mut seal = [0; 64];
                seal.copy_from_slice(v);
                DigestItem::BabeSeal(seal)
            }
            DigestItemRef::GrandpaConsensus(v) => DigestItem::GrandpaConsensus(v.into()),
            DigestItemRef::ChangesTrieRoot(v) => DigestItem::ChangesTrieRoot(*v),
            DigestItemRef::ChangesTrieSignal(v) => DigestItem::ChangesTrieSignal(v),
        }
    }
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

/// Decodes a single digest log item. On success, returns the item and the data that remains
/// after the item.
fn decode_item(mut slice: &[u8]) -> Result<(DigestItemRef, &[u8]), Error> {
    let index = *slice.get(0).ok_or(Error::TooShort)?;
    slice = &slice[1..];

    match index {
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
        (4, b"aura") => DigestItemRef::AuraConsensus(AuraConsensusLogRef::from_slice(content)?),
        (4, b"BABE") => DigestItemRef::BabeConsensus(BabeConsensusLogRef::from_slice(content)?),
        (4, b"FRNK") => {
            DigestItemRef::GrandpaConsensus(GrandpaConsensusLogRef::from_slice(content)?)
        }
        (4, e) => return Err(Error::UnknownConsensusEngine(*e)),
        (5, b"aura") => DigestItemRef::AuraSeal({
            TryFrom::try_from(content).map_err(|_| Error::BadAuraSealLength)?
        }),
        (5, b"BABE") => DigestItemRef::BabeSeal({
            TryFrom::try_from(content).map_err(|_| Error::BadBabeSealLength)?
        }),
        (5, e) => return Err(Error::UnknownConsensusEngine(*e)),
        (6, b"aura") => DigestItemRef::AuraPreDigest(AuraPreDigest::from_slice(content)?),
        (6, b"BABE") => DigestItemRef::BabePreDigest(BabePreDigestRef::from_slice(content)?),
        (6, e) => return Err(Error::UnknownConsensusEngine(*e)),
        _ => unreachable!(),
    })
}
