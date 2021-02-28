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

use crate::header::BabeNextConfig;

use alloc::{collections::BTreeMap, format, string::String, vec::Vec};
use parity_scale_codec::{Decode, DecodeAll as _, Encode};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[serde(deny_unknown_fields)]
pub(super) struct LightSyncState {
    babe_epoch_changes: HexString,
    babe_finalized_block_weight: u32,
    finalized_block_header: HexString,
    grandpa_authority_set: HexString,
}

impl LightSyncState {
    pub(super) fn decode(&self) -> DecodedLightSyncState {
        let grandpa_authority_set_slice = &self.grandpa_authority_set.0[..];
        let babe_epoch_changes_slice = &self.babe_epoch_changes.0[..];

        let decoded = DecodedLightSyncState {
            babe_finalized_block_weight: self.babe_finalized_block_weight,
            finalized_block_header: crate::header::decode(&self.finalized_block_header.0[..])
                .unwrap()
                .into(),
            grandpa_authority_set: AuthoritySet::decode_all(&grandpa_authority_set_slice).unwrap(),
            babe_epoch_changes: EpochChanges::decode_all(&babe_epoch_changes_slice).unwrap(),
        };

        decoded
    }
}

#[derive(Debug)]
pub(super) struct DecodedLightSyncState {
    pub(super) babe_epoch_changes: EpochChanges,
    babe_finalized_block_weight: u32,
    pub(super) finalized_block_header: crate::header::Header,
    pub(super) grandpa_authority_set: AuthoritySet,
}

#[derive(Debug, Decode, Encode)]
pub(super) struct EpochChanges {
    inner: ForkTree<PersistedEpochHeader>,
    pub(super) epochs: BTreeMap<([u8; 32], u32), PersistedEpoch>,
}

#[derive(Debug, Decode, Encode)]
pub(super) enum PersistedEpochHeader {
    Genesis(EpochHeader, EpochHeader),
    Regular(EpochHeader),
}

#[derive(Debug, Decode, Encode)]
pub(super) struct EpochHeader {
    start_slot: u64,
    end_slot: u64,
}

#[derive(Debug, Decode, Encode)]
pub(super) enum PersistedEpoch {
    Genesis(BabeEpoch, BabeEpoch),
    Regular(BabeEpoch),
}

#[derive(Debug, Decode, Encode)]
pub(super) struct BabeEpoch {
    pub(super) epoch_index: u64,
    pub(super) slot_number: u64,
    pub(super) duration: u64,
    pub(super) authorities: Vec<BabeAuthority>,
    pub(super) randomness: [u8; 32],
    pub(super) config: BabeNextConfig,
}

#[derive(Debug, Copy, Clone, PartialEq, Eq, Encode, Decode)]
pub struct BabeAuthority {
    /// Sr25519 public key.
    pub public_key: [u8; 32],
    /// Arbitrary number indicating the weight of the authority.
    ///
    /// This value can only be compared to other weight values.
    // TODO: should be NonZeroU64; requires deep changes in decoding code though
    pub weight: u64,
}

#[derive(Debug, Decode, Encode)]
pub(super) struct AuthoritySet {
    pub(super) current_authorities: Vec<GrandpaAuthority>,
    pub(super) set_id: u64,
    pending_standard_changes: ForkTree<PendingChange>,
    pending_forced_changes: Vec<PendingChange>,
    /// Note: this field didn't exist in Substrate before 2021-01-20. Light sync states that are
    /// older than that are missing it.
    authority_set_changes: Vec<(u64, u32)>,
}

#[derive(Debug, Decode, Encode)]
pub(super) struct PendingChange {
    next_authorities: Vec<GrandpaAuthority>,
    delay: u32,
    canon_height: u32,
    canon_hash: [u8; 32],
    delay_kind: DelayKind,
}

#[derive(Debug, Decode, Encode)]
pub(super) enum DelayKind {
    Finalized,
    Best { median_last_finalized: u32 },
}

#[derive(Debug, Copy, Clone, PartialEq, Eq, Encode, Decode)]
pub struct GrandpaAuthority {
    /// Ed25519 public key.
    pub public_key: [u8; 32],

    /// Arbitrary number indicating the weight of the authority.
    ///
    /// This value can only be compared to other weight values.
    // TODO: should be NonZeroU64; requires deep changes in decoding code though
    pub weight: u64,
}

#[derive(Debug, Decode, Encode)]
pub(super) struct ForkTree<T> {
    roots: Vec<ForkTreeNode<T>>,
    best_finalized_number: Option<u32>,
}

#[derive(Debug, Decode, Encode)]
pub(super) struct ForkTreeNode<T> {
    hash: [u8; 32],
    number: u32,
    data: T,
    children: Vec<Self>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(super) struct HexString(pub(super) Vec<u8>);

impl serde::Serialize for HexString {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        format!("0x{}", hex::encode(&self.0[..])).serialize(serializer)
    }
}

impl<'a> serde::Deserialize<'a> for HexString {
    fn deserialize<D>(deserializer: D) -> Result<HexString, D::Error>
    where
        D: serde::Deserializer<'a>,
    {
        let string = String::deserialize(deserializer)?;

        if !string.starts_with("0x") {
            return Err(serde::de::Error::custom(
                "hexadecimal string doesn't start with 0x",
            ));
        }

        let bytes = hex::decode(&string[2..]).map_err(serde::de::Error::custom)?;
        Ok(HexString(bytes))
    }
}
