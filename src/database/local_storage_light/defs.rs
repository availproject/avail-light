//! Type definitions to help with serializing/deserializing from/to the local storage.

use crate::{chain::chain_information, header};
use core::convert::TryFrom;

#[derive(Debug, serde::Serialize, serde::Deserialize)]
#[serde(tag = "version")]
pub(super) enum SerializedChainInformation {
    #[serde(rename = "1")]
    V1(SerializedChainInformationV1),
}

impl TryFrom<SerializedChainInformation> for chain_information::ChainInformation {
    type Error = header::Error;

    fn try_from(from: SerializedChainInformation) -> Result<Self, Self::Error> {
        Ok(match from {
            SerializedChainInformation::V1(from) => TryFrom::try_from(from)?,
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
    babe_finalized_block1_slot_number: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    babe_finalized_block_epoch_information:
        Option<(SerializedBabeNextEpochV1, SerializedBabeNextConfigV1)>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    babe_finalized_next_epoch_transition:
        Option<(SerializedBabeNextEpochV1, SerializedBabeNextConfigV1)>,
    grandpa_after_finalized_block_authorities_set_id: u64,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    grandpa_finalized_triggered_authorities: Vec<SerializedGrandpaAuthorityV1>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    grandpa_finalized_scheduled_change: Option<SerializedFinalizedScheduledChangeV1>,
}

impl<'a> From<chain_information::ChainInformationRef<'a>> for SerializedChainInformationV1 {
    fn from(from: chain_information::ChainInformationRef<'a>) -> Self {
        SerializedChainInformationV1 {
            finalized_block_header: from.finalized_block_header.scale_encoding().fold(
                Vec::new(),
                |mut a, b| {
                    a.extend_from_slice(b.as_ref());
                    a
                },
            ),
            babe_finalized_block1_slot_number: from.babe_finalized_block1_slot_number,
            babe_finalized_block_epoch_information: from
                .babe_finalized_block_epoch_information
                .map(|(e, i)| (e.into(), i.into())),
            babe_finalized_next_epoch_transition: from
                .babe_finalized_next_epoch_transition
                .map(|(e, i)| (e.into(), i.into())),
            grandpa_after_finalized_block_authorities_set_id: from
                .grandpa_after_finalized_block_authorities_set_id,
            grandpa_finalized_triggered_authorities: from
                .grandpa_finalized_triggered_authorities
                .into_iter()
                .map(header::GrandpaAuthorityRef::from)
                .map(Into::into)
                .collect(),
            grandpa_finalized_scheduled_change: from.grandpa_finalized_scheduled_change.map(
                |(n, l)| SerializedFinalizedScheduledChangeV1 {
                    trigger_block_height: n,
                    new_authorities_list: l.iter().map(Into::into).collect(),
                },
            ),
        }
    }
}

impl TryFrom<SerializedChainInformationV1> for chain_information::ChainInformation {
    type Error = header::Error;

    fn try_from(from: SerializedChainInformationV1) -> Result<Self, Self::Error> {
        Ok(chain_information::ChainInformation {
            finalized_block_header: header::decode(&from.finalized_block_header)?.into(),
            babe_finalized_block1_slot_number: from.babe_finalized_block1_slot_number,
            babe_finalized_block_epoch_information: from
                .babe_finalized_block_epoch_information
                .map(|(e, i)| (e.into(), i.into())),
            babe_finalized_next_epoch_transition: from
                .babe_finalized_next_epoch_transition
                .map(|(e, i)| (e.into(), i.into())),
            grandpa_after_finalized_block_authorities_set_id: from
                .grandpa_after_finalized_block_authorities_set_id,
            grandpa_finalized_triggered_authorities: from
                .grandpa_finalized_triggered_authorities
                .into_iter()
                .map(Into::into)
                .collect(),
            grandpa_finalized_scheduled_change: from.grandpa_finalized_scheduled_change.map(
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
        })
    }
}

#[derive(Debug, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct SerializedBabeNextEpochV1 {
    authorities: Vec<SerializedBabeAuthorityV1>,
    #[serde(
        serialize_with = "serialize_bytes",
        deserialize_with = "deserialize_hash32"
    )]
    randomness: [u8; 32],
}

impl<'a> From<header::BabeNextEpochRef<'a>> for SerializedBabeNextEpochV1 {
    fn from(from: header::BabeNextEpochRef<'a>) -> Self {
        SerializedBabeNextEpochV1 {
            authorities: from.authorities.map(Into::into).collect(),
            randomness: *from.randomness,
        }
    }
}

impl From<SerializedBabeNextEpochV1> for header::BabeNextEpoch {
    fn from(from: SerializedBabeNextEpochV1) -> Self {
        header::BabeNextEpoch {
            authorities: from.authorities.into_iter().map(Into::into).collect(),
            randomness: from.randomness,
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
struct SerializedBabeNextConfigV1 {
    c: SerializedBabeNextConfigConstantV1,
    allowed_slots: SerializedBabeAllowedSlotsV1,
}

impl From<header::BabeNextConfig> for SerializedBabeNextConfigV1 {
    fn from(from: header::BabeNextConfig) -> Self {
        SerializedBabeNextConfigV1 {
            c: SerializedBabeNextConfigConstantV1 {
                num: from.c.0,
                denom: from.c.1,
            },
            allowed_slots: from.allowed_slots.into(),
        }
    }
}

impl From<SerializedBabeNextConfigV1> for header::BabeNextConfig {
    fn from(from: SerializedBabeNextConfigV1) -> Self {
        header::BabeNextConfig {
            c: (from.c.num, from.c.denom),
            allowed_slots: from.allowed_slots.into(),
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
    PrimarySlots,
    #[serde(rename = "primary-and-secondary-plain")]
    PrimaryAndSecondaryPlainSlots,
    #[serde(rename = "primary-and-secondary-vrf")]
    PrimaryAndSecondaryVRFSlots,
}

impl From<header::BabeAllowedSlots> for SerializedBabeAllowedSlotsV1 {
    fn from(from: header::BabeAllowedSlots) -> Self {
        match from {
            header::BabeAllowedSlots::PrimarySlots => SerializedBabeAllowedSlotsV1::PrimarySlots,
            header::BabeAllowedSlots::PrimaryAndSecondaryPlainSlots => {
                SerializedBabeAllowedSlotsV1::PrimaryAndSecondaryPlainSlots
            }
            header::BabeAllowedSlots::PrimaryAndSecondaryVRFSlots => {
                SerializedBabeAllowedSlotsV1::PrimaryAndSecondaryVRFSlots
            }
        }
    }
}

impl From<SerializedBabeAllowedSlotsV1> for header::BabeAllowedSlots {
    fn from(from: SerializedBabeAllowedSlotsV1) -> Self {
        match from {
            SerializedBabeAllowedSlotsV1::PrimarySlots => header::BabeAllowedSlots::PrimarySlots,
            SerializedBabeAllowedSlotsV1::PrimaryAndSecondaryPlainSlots => {
                header::BabeAllowedSlots::PrimaryAndSecondaryPlainSlots
            }
            SerializedBabeAllowedSlotsV1::PrimaryAndSecondaryVRFSlots => {
                header::BabeAllowedSlots::PrimaryAndSecondaryVRFSlots
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
    weight: u64, // TODO: should be NonZeroU64; requires changing crate::header first
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
    // TODO: there's probably a more optimized way to do it
    serializer.serialize_str(&hex::encode(data))
}

fn deserialize_bytes<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<Vec<u8>, D::Error> {
    // TODO: not sure that's the correct way to do it
    let string = <String as serde::Deserialize>::deserialize(deserializer)?;
    Ok(hex::decode(&string).map_err(serde::de::Error::custom)?)
}

fn deserialize_hash32<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<[u8; 32], D::Error> {
    // TODO: not sure that's the correct way to do it
    let string = <String as serde::Deserialize>::deserialize(deserializer)?;
    let value = hex::decode(&string).map_err(serde::de::Error::custom)?;
    if value.len() > 32 {
        return Err(serde::de::Error::custom("invalid hash length"));
    }

    let mut out = [0u8; 32];
    out[(32 - value.len())..].copy_from_slice(&value);
    Ok(out)
}
