//! Persistent storage for light-client data based on the browser's local storage.

#![cfg(feature = "wasm-bindings")]
#![cfg_attr(docsrs, doc(cfg(feature = "wasm-bindings")))]

use crate::{chain::chain_information, header};

use core::fmt;
use wasm_bindgen::prelude::*;
use web_sys::Storage;

/// An open local storage. Corresponds to
/// [a JavaScript `Storage` object](https://developer.mozilla.org/en-US/docs/Web/API/Storage).
pub struct LocalStorage {
    inner: send_wrapper::SendWrapper<Storage>,
}

impl LocalStorage {
    /// Tries to open the storage from the browser environment.
    pub async fn open() -> Result<Self, OpenError> {
        let window = web_sys::window().ok_or(OpenError::NoWindow)?;
        let storage = window
            .local_storage()
            .map_err(OpenError::LocalStorageNotSupported)?
            .unwrap();

        Ok(LocalStorage {
            inner: send_wrapper::SendWrapper::new(storage),
        })
    }

    /// Stores the given information in the local storage.
    pub fn set_chain_information(
        &self,
        information: chain_information::ChainInformationRef<'_>,
    ) -> Result<(), AccessError> {
        let decoded = SerializedChainInformation::V1(SerializedChainInformationV1 {
            finalized_block_header: information.finalized_block_header.to_owned(),
            babe_finalized_block1_slot_number: information.babe_finalized_block1_slot_number,
            babe_finalized_block_epoch_information: information
                .babe_finalized_block_epoch_information
                .map(|epoch| SerializedBabeNextEpochV1 {
                    authorities: epoch
                        .authorities
                        .into_iter()
                        .map(|authority| SerializedBabeAuthorityV1 {
                            public_key: *authority.public_key,
                            weight: authority.weight,
                        })
                        .collect(),
                    randomness: *epoch.randomness,
                }),
            babe_finalized_next_epoch_transition: information
                .babe_finalized_next_epoch_transition
                .map(|epoch| SerializedBabeNextEpochV1 {
                    authorities: epoch
                        .authorities
                        .into_iter()
                        .map(|authority| SerializedBabeAuthorityV1 {
                            public_key: *authority.public_key,
                            weight: authority.weight,
                        })
                        .collect(),
                    randomness: *epoch.randomness,
                }),
            grandpa_after_finalized_block_authorities_set_id: information
                .grandpa_after_finalized_block_authorities_set_id,
            grandpa_finalized_triggered_authorities: information
                .grandpa_finalized_triggered_authorities
                .into_iter()
                .map(|authority| SerializedGrandpaAuthorityV1 {
                    public_key: authority.public_key,
                    weight: authority.weight,
                })
                .collect(),
            grandpa_finalized_scheduled_changes: information
                .grandpa_finalized_scheduled_changes
                .iter()
                .map(|change| SerializedFinalizedScheduledChangeV1 {
                    trigger_block_height: change.trigger_block_height,
                    new_authorities_list: change
                        .new_authorities_list
                        .iter()
                        .map(|authority| SerializedGrandpaAuthorityV1 {
                            public_key: authority.public_key,
                            weight: authority.weight,
                        })
                        .collect(),
                })
                .collect(),
        });

        let encoded = serde_json::to_string(&decoded).unwrap();
        self.inner
            .set_item("chain_information", &encoded)
            .map_err(AccessError::StorageAccess)?;
        Ok(())
    }

    /// Loads information about the chain from the local storage.
    pub fn chain_information(
        &self,
    ) -> Result<Option<chain_information::ChainInformation>, AccessError> {
        let encoded = match self
            .inner
            .get_item("chain_information")
            .map_err(AccessError::StorageAccess)?
        {
            Some(v) => v,
            None => return Ok(None),
        };

        let decoded: SerializedChainInformation = serde_json::from_str(&encoded)
            .map_err(CorruptedError)
            .map_err(AccessError::Corrupted)?;

        match decoded {
            SerializedChainInformation::V1(decoded) => {
                Ok(Some(chain_information::ChainInformation {
                    finalized_block_header: decoded.finalized_block_header,
                    babe_finalized_block1_slot_number: decoded.babe_finalized_block1_slot_number,
                    babe_finalized_block_epoch_information: decoded
                        .babe_finalized_block_epoch_information
                        .map(|epoch| header::BabeNextEpoch {
                            authorities: epoch
                                .authorities
                                .into_iter()
                                .map(|authority| header::BabeAuthority {
                                    public_key: authority.public_key,
                                    weight: authority.weight,
                                })
                                .collect(),
                            randomness: epoch.randomness,
                        }),
                    babe_finalized_next_epoch_transition: decoded
                        .babe_finalized_next_epoch_transition
                        .map(|epoch| header::BabeNextEpoch {
                            authorities: epoch
                                .authorities
                                .into_iter()
                                .map(|authority| header::BabeAuthority {
                                    public_key: authority.public_key,
                                    weight: authority.weight,
                                })
                                .collect(),
                            randomness: epoch.randomness,
                        }),
                    grandpa_after_finalized_block_authorities_set_id: decoded
                        .grandpa_after_finalized_block_authorities_set_id,
                    grandpa_finalized_triggered_authorities: decoded
                        .grandpa_finalized_triggered_authorities
                        .into_iter()
                        .map(|authority| header::GrandpaAuthority {
                            public_key: authority.public_key,
                            weight: authority.weight,
                        })
                        .collect(),
                    grandpa_finalized_scheduled_changes: decoded
                        .grandpa_finalized_scheduled_changes
                        .into_iter()
                        .map(|change| chain_information::FinalizedScheduledChange {
                            trigger_block_height: change.trigger_block_height,
                            new_authorities_list: change
                                .new_authorities_list
                                .into_iter()
                                .map(|authority| header::GrandpaAuthority {
                                    public_key: authority.public_key,
                                    weight: authority.weight,
                                })
                                .collect(),
                        })
                        .collect(),
                }))
            }
        }
    }
}

/// Error when opening the database.
#[derive(Debug, derive_more::Display)]
pub enum OpenError {
    /// No `window` object available.
    ///
    /// > **Note**: This probably indicates that the environment is not a browser.
    NoWindow,
    /// Local storage is not supported by the environment.
    #[display(fmt = "Local storage is not supported by the environment: {:?}", _0)]
    LocalStorageNotSupported(JsValue),
}

/// Error accessing the database.
#[derive(Debug, derive_more::Display)]
pub enum AccessError {
    /// JavaScript error produced when accessing the storage.
    #[display(fmt = "Error when accessing local storage: {:?}", _0)]
    StorageAccess(JsValue),
    /// Corruption in the data stored in the local storage.
    Corrupted(CorruptedError),
}

/// Opaque error indicating a corruption in the data stored in the local storage.
#[derive(Debug, derive_more::Display)]
pub struct CorruptedError(serde_json::Error);

#[derive(Debug, serde::Serialize, serde::Deserialize)]
#[serde(tag = "version")]
enum SerializedChainInformation {
    #[serde(rename = "1")]
    V1(SerializedChainInformationV1),
}

#[derive(Debug, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct SerializedChainInformationV1 {
    #[serde(
        serialize_with = "serialize_bytes",
        deserialize_with = "deserialize_bytes"
    )]
    finalized_block_header: Vec<u8>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    babe_finalized_block1_slot_number: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    babe_finalized_block_epoch_information: Option<SerializedBabeNextEpochV1>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    babe_finalized_next_epoch_transition: Option<SerializedBabeNextEpochV1>,
    grandpa_after_finalized_block_authorities_set_id: u64,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    grandpa_finalized_triggered_authorities: Vec<SerializedGrandpaAuthorityV1>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    grandpa_finalized_scheduled_changes: Vec<SerializedFinalizedScheduledChangeV1>,
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

#[derive(Debug, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct SerializedBabeAuthorityV1 {
    #[serde(
        serialize_with = "serialize_bytes",
        deserialize_with = "deserialize_hash32"
    )]
    public_key: [u8; 32],
    weight: u64,
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
    weight: u64,
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
