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

//! Type definitions to help with serializing/deserializing from/to the local storage.

use crate::{chain::chain_information, header};

use alloc::vec::Vec;
use core::{fmt, num::NonZeroU64};
use hashbrown::HashMap;

/// Error that can happen when deserializing the data.
#[derive(Debug, derive_more::Display)]
pub(super) enum DeserializeError {
    Header(header::Error),
    ConsensusAlgorithmsMismatch,
    /// Some Babe-related information is missing.
    MissingBabeInformation,
}

#[derive(Debug, serde::Serialize, serde::Deserialize)]
#[serde(tag = "version")]
pub(super) enum SerializedChainInformation {
    #[serde(rename = "1")]
    V1(SerializedChainInformationV1),
}

impl SerializedChainInformation {
    pub(super) fn decode(
        self,
    ) -> Result<
        (
            chain_information::ChainInformation,
            Option<HashMap<Vec<u8>, Vec<u8>, fnv::FnvBuildHasher>>,
        ),
        DeserializeError,
    > {
        Ok(match self {
            SerializedChainInformation::V1(from) => from.decode()?,
        })
    }
}

#[derive(Debug, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct SerializedChainInformationV1 {
    #[serde(
        serialize_with = "serialize_bytes",
        deserialize_with = "deserialize_bytes"
    )]
    finalized_block_header: Vec<u8>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    aura_slot_duration: Option<NonZeroU64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    aura_finalized_authorities: Option<Vec<SerializedAuraAuthorityV1>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    babe_slots_per_epoch: Option<NonZeroU64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    babe_finalized_block_epoch_information: Option<SerializedBabeEpochInformationV1>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    babe_finalized_next_epoch_transition: Option<SerializedBabeEpochInformationV1>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    grandpa_after_finalized_block_authorities_set_id: Option<u64>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    grandpa_finalized_triggered_authorities: Vec<SerializedGrandpaAuthorityV1>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    grandpa_finalized_scheduled_change: Option<SerializedFinalizedScheduledChangeV1>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    finalized_storage: Option<Vec<SerializedFinalizedStorageEntryV1>>,
}

impl SerializedChainInformationV1 {
    pub(super) fn new<'a>(
        from: chain_information::ChainInformationRef<'a>,
        finalized_storage: Option<impl Iterator<Item = (impl AsRef<[u8]>, impl AsRef<[u8]>)>>,
    ) -> Self {
        SerializedChainInformationV1 {
            finalized_block_header: from.finalized_block_header.scale_encoding().fold(
                Vec::new(),
                |mut a, b| {
                    a.extend_from_slice(b.as_ref());
                    a
                },
            ),
            aura_slot_duration: if let chain_information::ChainInformationConsensusRef::Aura {
                slot_duration,
                ..
            } = &from.consensus
            {
                Some(*slot_duration)
            } else {
                None
            },
            aura_finalized_authorities:
                if let chain_information::ChainInformationConsensusRef::Aura {
                    finalized_authorities_list,
                    ..
                } = &from.consensus
                {
                    Some(finalized_authorities_list.clone().map(Into::into).collect())
                } else {
                    None
                },
            babe_slots_per_epoch: if let chain_information::ChainInformationConsensusRef::Babe {
                slots_per_epoch,
                ..
            } = &from.consensus
            {
                Some(*slots_per_epoch)
            } else {
                None
            },
            babe_finalized_block_epoch_information:
                if let chain_information::ChainInformationConsensusRef::Babe {
                    finalized_block_epoch_information,
                    ..
                } = &from.consensus
                {
                    finalized_block_epoch_information.clone().map(Into::into)
                } else {
                    None
                },
            babe_finalized_next_epoch_transition:
                if let chain_information::ChainInformationConsensusRef::Babe {
                    finalized_next_epoch_transition,
                    ..
                } = &from.consensus
                {
                    Some(finalized_next_epoch_transition.clone().into())
                } else {
                    None
                },
            grandpa_after_finalized_block_authorities_set_id: match from.finality {
                chain_information::ChainInformationFinalityRef::Outsourced => None,
                chain_information::ChainInformationFinalityRef::Grandpa {
                    after_finalized_block_authorities_set_id,
                    ..
                } => Some(after_finalized_block_authorities_set_id),
            },
            grandpa_finalized_triggered_authorities: match from.finality {
                chain_information::ChainInformationFinalityRef::Outsourced => Vec::new(),
                chain_information::ChainInformationFinalityRef::Grandpa {
                    finalized_triggered_authorities,
                    ..
                } => finalized_triggered_authorities
                    .iter()
                    .map(header::GrandpaAuthorityRef::from)
                    .map(Into::into)
                    .collect(),
            },
            grandpa_finalized_scheduled_change: match from.finality {
                chain_information::ChainInformationFinalityRef::Outsourced => None,
                chain_information::ChainInformationFinalityRef::Grandpa {
                    finalized_scheduled_change,
                    ..
                } => {
                    finalized_scheduled_change.map(|(n, l)| SerializedFinalizedScheduledChangeV1 {
                        trigger_block_height: n,
                        new_authorities_list: l.iter().map(Into::into).collect(),
                    })
                }
            },
            finalized_storage: finalized_storage.map(|storage| {
                storage
                    .map(|(k, v)| SerializedFinalizedStorageEntryV1 {
                        key: k.as_ref().to_vec(),
                        value: v.as_ref().to_vec(),
                    })
                    .collect()
            }),
        }
    }
}

impl SerializedChainInformationV1 {
    pub(super) fn decode(
        self,
    ) -> Result<
        (
            chain_information::ChainInformation,
            Option<HashMap<Vec<u8>, Vec<u8>, fnv::FnvBuildHasher>>,
        ),
        DeserializeError,
    > {
        let consensus = match (
            self.aura_finalized_authorities,
            self.aura_slot_duration,
            self.babe_slots_per_epoch,
            self.babe_finalized_block_epoch_information,
            self.babe_finalized_next_epoch_transition,
        ) {
            (Some(aura_authorities), Some(slot_duration), None, None, None) => {
                chain_information::ChainInformationConsensus::Aura {
                    finalized_authorities_list: aura_authorities
                        .into_iter()
                        .map(Into::into)
                        .collect(),
                    slot_duration,
                }
            }

            (
                None,
                None,
                babe_slots_per_epoch,
                babe_finalized_block_epoch_information,
                babe_finalized_next_epoch_transition,
            ) => chain_information::ChainInformationConsensus::Babe {
                slots_per_epoch: babe_slots_per_epoch
                    .ok_or(DeserializeError::MissingBabeInformation)?,
                finalized_block_epoch_information: babe_finalized_block_epoch_information
                    .map(Into::into),
                finalized_next_epoch_transition: babe_finalized_next_epoch_transition
                    .map(Into::into)
                    .ok_or(DeserializeError::MissingBabeInformation)?,
            },

            _ => return Err(DeserializeError::ConsensusAlgorithmsMismatch),
        };

        let chain_info = chain_information::ChainInformation {
            finalized_block_header: header::decode(&self.finalized_block_header)
                .map_err(DeserializeError::Header)?
                .into(),
            consensus,
            finality: if let Some(set_id) = self.grandpa_after_finalized_block_authorities_set_id {
                chain_information::ChainInformationFinality::Grandpa {
                    after_finalized_block_authorities_set_id: set_id,
                    finalized_triggered_authorities: self
                        .grandpa_finalized_triggered_authorities
                        .into_iter()
                        .map(Into::into)
                        .collect(),
                    finalized_scheduled_change: self.grandpa_finalized_scheduled_change.map(
                        |change| {
                            (
                                change.trigger_block_height,
                                change
                                    .new_authorities_list
                                    .into_iter()
                                    .map(Into::into)
                                    .collect(),
                            )
                        },
                    ),
                }
            } else {
                chain_information::ChainInformationFinality::Outsourced
            },
        };

        // TODO: consider checking integrity of the storage against the header
        let finalized_storage = if let Some(storage) = self.finalized_storage {
            Some(
                storage
                    .into_iter()
                    .map(|entry| (entry.key, entry.value))
                    .collect(),
            )
        } else {
            None
        };

        Ok((chain_info, finalized_storage))
    }
}

#[derive(Debug, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct SerializedFinalizedStorageEntryV1 {
    #[serde(
        serialize_with = "serialize_bytes",
        deserialize_with = "deserialize_bytes"
    )]
    key: Vec<u8>,
    #[serde(
        serialize_with = "serialize_bytes",
        deserialize_with = "deserialize_bytes"
    )]
    value: Vec<u8>,
}

#[derive(Debug, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct SerializedAuraAuthorityV1 {
    #[serde(
        serialize_with = "serialize_bytes",
        deserialize_with = "deserialize_hash32"
    )]
    public_key: [u8; 32],
}

impl<'a> From<header::AuraAuthorityRef<'a>> for SerializedAuraAuthorityV1 {
    fn from(from: header::AuraAuthorityRef<'a>) -> Self {
        SerializedAuraAuthorityV1 {
            public_key: *from.public_key,
        }
    }
}

impl From<SerializedAuraAuthorityV1> for header::AuraAuthority {
    fn from(from: SerializedAuraAuthorityV1) -> Self {
        header::AuraAuthority {
            public_key: from.public_key,
        }
    }
}

#[derive(Debug, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct SerializedBabeEpochInformationV1 {
    epoch_index: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    start_slot_number: Option<u64>,
    authorities: Vec<SerializedBabeAuthorityV1>,
    #[serde(
        serialize_with = "serialize_bytes",
        deserialize_with = "deserialize_hash32"
    )]
    randomness: [u8; 32],
    c: SerializedBabeNextConfigConstantV1,
    allowed_slots: SerializedBabeAllowedSlotsV1,
}

impl<'a> From<chain_information::BabeEpochInformationRef<'a>> for SerializedBabeEpochInformationV1 {
    fn from(from: chain_information::BabeEpochInformationRef<'a>) -> Self {
        SerializedBabeEpochInformationV1 {
            epoch_index: from.epoch_index,
            start_slot_number: from.start_slot_number,
            authorities: from.authorities.map(Into::into).collect(),
            randomness: *from.randomness,
            c: SerializedBabeNextConfigConstantV1 {
                num: from.c.0,
                denom: from.c.1,
            },
            allowed_slots: from.allowed_slots.into(),
        }
    }
}

impl From<SerializedBabeEpochInformationV1> for chain_information::BabeEpochInformation {
    fn from(from: SerializedBabeEpochInformationV1) -> Self {
        chain_information::BabeEpochInformation {
            epoch_index: from.epoch_index,
            start_slot_number: from.start_slot_number,
            authorities: from.authorities.into_iter().map(Into::into).collect(),
            randomness: from.randomness,
            c: (from.c.num, from.c.denom),
            allowed_slots: from.allowed_slots.into(),
        }
    }
}

#[derive(Debug, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct SerializedBabeAuthorityV1 {
    #[serde(
        serialize_with = "serialize_bytes",
        deserialize_with = "deserialize_hash32"
    )]
    public_key: [u8; 32],
    weight: u64, // TODO: should be NonZeroU64; requires changing crate::header first
}

impl<'a> From<header::BabeAuthorityRef<'a>> for SerializedBabeAuthorityV1 {
    fn from(from: header::BabeAuthorityRef<'a>) -> Self {
        SerializedBabeAuthorityV1 {
            public_key: *from.public_key,
            weight: from.weight,
        }
    }
}

impl From<SerializedBabeAuthorityV1> for header::BabeAuthority {
    fn from(from: SerializedBabeAuthorityV1) -> Self {
        header::BabeAuthority {
            public_key: from.public_key,
            weight: from.weight,
        }
    }
}

#[derive(Debug, serde::Serialize, serde::Deserialize)]
struct SerializedBabeNextConfigConstantV1 {
    num: u64,
    denom: u64,
}

#[derive(Debug, serde::Serialize, serde::Deserialize)]
enum SerializedBabeAllowedSlotsV1 {
    #[serde(rename = "primary")]
    OnlyPrimary,
    #[serde(rename = "primary-and-secondary-plain")]
    PrimaryAndSecondaryPlain,
    #[serde(rename = "primary-and-secondary-vrf")]
    PrimaryAndSecondaryVrf,
}

impl From<header::BabeAllowedSlots> for SerializedBabeAllowedSlotsV1 {
    fn from(from: header::BabeAllowedSlots) -> Self {
        match from {
            header::BabeAllowedSlots::PrimarySlots => SerializedBabeAllowedSlotsV1::OnlyPrimary,
            header::BabeAllowedSlots::PrimaryAndSecondaryPlainSlots => {
                SerializedBabeAllowedSlotsV1::PrimaryAndSecondaryPlain
            }
            header::BabeAllowedSlots::PrimaryAndSecondaryVrfSlots => {
                SerializedBabeAllowedSlotsV1::PrimaryAndSecondaryVrf
            }
        }
    }
}

impl From<SerializedBabeAllowedSlotsV1> for header::BabeAllowedSlots {
    fn from(from: SerializedBabeAllowedSlotsV1) -> Self {
        match from {
            SerializedBabeAllowedSlotsV1::OnlyPrimary => header::BabeAllowedSlots::PrimarySlots,
            SerializedBabeAllowedSlotsV1::PrimaryAndSecondaryPlain => {
                header::BabeAllowedSlots::PrimaryAndSecondaryPlainSlots
            }
            SerializedBabeAllowedSlotsV1::PrimaryAndSecondaryVrf => {
                header::BabeAllowedSlots::PrimaryAndSecondaryVrfSlots
            }
        }
    }
}

#[derive(Debug, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct SerializedFinalizedScheduledChangeV1 {
    trigger_block_height: u64,
    new_authorities_list: Vec<SerializedGrandpaAuthorityV1>,
}

#[derive(Debug, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct SerializedGrandpaAuthorityV1 {
    #[serde(
        serialize_with = "serialize_bytes",
        deserialize_with = "deserialize_hash32"
    )]
    public_key: [u8; 32],
    weight: NonZeroU64,
}

impl<'a> From<header::GrandpaAuthorityRef<'a>> for SerializedGrandpaAuthorityV1 {
    fn from(from: header::GrandpaAuthorityRef<'a>) -> Self {
        SerializedGrandpaAuthorityV1 {
            public_key: *from.public_key,
            weight: from.weight,
        }
    }
}

impl<'a> From<&'a header::GrandpaAuthority> for SerializedGrandpaAuthorityV1 {
    fn from(from: &'a header::GrandpaAuthority) -> Self {
        SerializedGrandpaAuthorityV1 {
            public_key: from.public_key,
            weight: from.weight,
        }
    }
}

impl From<header::GrandpaAuthority> for SerializedGrandpaAuthorityV1 {
    fn from(from: header::GrandpaAuthority) -> Self {
        SerializedGrandpaAuthorityV1 {
            public_key: from.public_key,
            weight: from.weight,
        }
    }
}

impl From<SerializedGrandpaAuthorityV1> for header::GrandpaAuthority {
    fn from(from: SerializedGrandpaAuthorityV1) -> Self {
        header::GrandpaAuthority {
            public_key: from.public_key,
            weight: from.weight,
        }
    }
}

fn serialize_bytes<S: serde::Serializer>(data: &[u8], serializer: S) -> Result<S::Ok, S::Error> {
    struct Writer<'a>(&'a [u8]);
    impl<'a> fmt::Display for Writer<'a> {
        fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
            for byte in self.0 {
                write!(f, "{:02x}", byte)?
            }
            Ok(())
        }
    }

    serializer.collect_str(&Writer(data))
}

fn deserialize_bytes<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<Vec<u8>, D::Error> {
    let string = <&str as serde::Deserialize>::deserialize(deserializer)?;
    Ok(hex::decode(string).map_err(serde::de::Error::custom)?)
}

fn deserialize_hash32<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<[u8; 32], D::Error> {
    let string = <&str as serde::Deserialize>::deserialize(deserializer)?;
    if string.len() > 64 {
        return Err(serde::de::Error::custom("invalid hash length"));
    }

    let mut out = [0u8; 32];
    hex::decode_to_slice(string, &mut out[(32 - string.len() / 2)..])
        .map_err(serde::de::Error::custom)?;
    Ok(out)
}
